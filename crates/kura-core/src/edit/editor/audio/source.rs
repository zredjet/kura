//! 音声の元の開き方（M13 段 4。docs/editing.md §9.11・§12.10）。
//!
//! 1. **アンカーがある**：アンカーの f32 WAV を遅延の源で開く（`CodecSet::open_audio_source`）。アンカーの見出しの記録が DB の
//!    アンカーの記録と同じなら、その記録の値のダイジェストを入力の指紋にし、元の読み取りのデコーダは記録のもの（アンカーの
//!    読み手の名前ではない）にする。違う・開けなければ今の道（記録のデコーダで読み直して照らす。`super::super::source::read`）
//!    に落とす。値を照らすのは開くときではなく保存のとき（段 3）。
//! 2. **WAV・AIFF**（`open_audio_source` が Some を返すもの）：遅延の源で開く。見出しを読む前に、この形式の最初の段が固定の段の
//!    WAV・AIFF のデコーダか確かめ（[`opens_as_source`]）、ほかの形式の形はここでは読まない（予算の外で読まない。段 4B の
//!    検証の直し）。DB の事実の行（`media_facts`）のデコーダが開いた
//!    デコーダと同じで、文書の入力の読み方が既定なら、その値のダイジェストで記録（`DecodeStamp`。固定の段・再現する）を作る。
//!    そうでなければ記録はなく、指紋は派生（`Fingerprint::derive("audio-source", 元の中身, デコーダと読み方)`）。
//! 3. **それ以外**（FLAC・Ogg・MP3・AAC・Platform・External、A-law の WAV など）：値が [`work_threshold`]（min(64 MiB,
//!    丸ごと読む上限 [`whole_limit`]）以下なら今の道で丸ごと読み、超えれば作業用のファイル（`WorkPcmStore`。封をした f32 WAV＝
//!    アンカーの形）に流して書いてから遅延の源で開く（[`make_work`]・[`open_work`]。M13 段 4B）。記録は流れのダイジェストから
//!    作ったもの（作業用のファイルの封の記録）で、丸ごと読んだときと同じ値・同じ記録・同じ指紋になる。作業用のファイルの参照
//!    （[`WorkRef`]）は `InputInfo` が持ち、閉じると落ちて、ほかに使い手（試聴の `/pcm`・保存）がなければ消える。
//!    - 作る間の予算は対話の枠で**待たずに**確保する（予算の全体を超えれば TooLarge、空いていなければ Busy）。見積もりは段ごと
//!      （`CodecSet::shape_path_staged` でその段の形を読む分を確保し直して形を読み、形を読めた段の `stage_stream_work_bytes` ＋
//!      窓 ＋ ダイジェスト ＋ 書き手の作業列で確保し直して、その段で流して読む。その段が `Unsupported` なら次の段で形から読み
//!      直す。段 2 の解析のジョブと同じ考え方）。中止は呼び手の見張り（エディタは `editor.cancel`）。
//!    - アンカーがあって遅延で使えなかったものは、設計どおり今の道（記録のデコーダで読み直す）に落ち、丸ごと読む上限を超えれば
//!      TooLarge のまま。
//!
//! 遅延の値の源の LRU の上限は [`lru_bytes`]（max(64 MiB, プレビューのスレッド × 4 チャンク × ch × 256 KiB) を、音声の全体の値と
//! 取り置きの 1/4 で頭打ち）。入力は `register_external` で数える（源 1 つにつき LRU の上限を 1 回）ので、読む段の予算
//! （`admit_read`）は通さない。

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kura_engine::audio::digest::PcmTreeDigest;
use kura_engine::audio::source::Opener;
use kura_engine::budget::{Class, Lease, MemoryBudget, default_reserved};
use kura_engine::codec::anchor::AnchorContext;
use kura_engine::codec::stream::stream_window_bytes;
use kura_engine::codec::wav::{self, F32WavWriter};
use kura_engine::codec::{CodecSet, DecodeStamp, PickedStage, ReadSeek, Tier, aiff, anchor};
use kura_engine::doc::DecodeOptions;
use kura_engine::render::cache::{Fingerprint, FingerprintedInput};
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{Digest, EngineError, Limits, Monitor, Shape, Value};
use rusqlite::{Connection, OptionalExtension};

use super::super::EditorError;
use crate::CoreError;
use crate::edit::EngineRuntime;
use crate::edit::anchors::Anchor;
use crate::edit::work_pcm::{WorkHandle, WorkKey, WorkPcmStore};

/// 丸ごと読む音声の上限の頭打ち（バイト）。
pub const WHOLE_READ_CAP: u64 = 512 << 20;
/// 源の LRU の上限の下限（バイト）。
pub const LRU_FLOOR: u64 = 64 << 20;
/// 直接読めない形式の音声を作業用のファイルにする値の大きさの頭打ち（バイト。これを超えれば作業用のファイル）。
pub const WORK_ABOVE: u64 = 64 << 20;

/// 作業用のファイルの参照（`InputInfo`・試聴の置き場が持つ。落とすと参照が減り、0 になれば消える）。
#[derive(Clone)]
pub(crate) struct WorkRef(pub(crate) Arc<WorkHandle>);

impl WorkRef {
    /// 作業用のファイルの場所。
    pub(crate) fn path(&self) -> &Path {
        &self.0.pcm().path
    }
}

impl std::fmt::Debug for WorkRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("WorkRef").field(&self.path()).finish()
    }
}

/// 開いた音声の入力の要約（`AudioState` が持つ）。
#[derive(Debug, Clone)]
pub(crate) struct InputInfo {
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// 値を読んだデコーダの識別子（`/edit/{s}/input/pcm` の kura チャンクに入れる）。
    pub decoder: String,
    /// 遅延の源で開いた（全体を読んでいない。作業用のファイルから開いたものも真）。
    pub lazy: bool,
    /// 読み取りの記録（分からなければ None。段 3 の貸し出しで使う）。
    pub decoded: Option<DecodeStamp>,
    /// 作業用のファイル（直接読めない形式の長い音声。持つことで寿命を決め、閉じると落ちる。段 4C の貸し出しで読む）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub work: Option<WorkRef>,
}

/// 開いた音声。
pub(crate) struct OpenedAudio {
    pub input: FingerprintedInput,
    pub info: InputInfo,
}

/// DB の事実の行の読み取りの記録（`media_facts` の decoder・value_digest・reproducible）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactsStamp {
    pub decoder: String,
    pub value_digest: [u8; 32],
    pub reproducible: bool,
}

/// 元の中身の事実の行の読み取りの記録（行がない・読まずに作った・ダイジェストがなければ None）。
pub(crate) fn read_facts_stamp(
    conn: &Connection,
    hash: &[u8; 32],
) -> rusqlite::Result<Option<FactsStamp>> {
    let mut stmt = conn.prepare_cached(
        "SELECT f.decoder, f.value_digest, f.reproducible FROM blobs b
           JOIN media_facts f ON f.blob_id = b.id
          WHERE b.hash = ?1 AND f.state = 'ok'",
    )?;
    let row = stmt
        .query_row([hash.as_slice()], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<Vec<u8>>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })
        .optional()?;
    Ok(row.and_then(|(decoder, digest, reproducible)| {
        let decoder = decoder?;
        let value_digest: [u8; 32] = digest?.try_into().ok()?;
        Some(FactsStamp {
            decoder,
            value_digest,
            reproducible: reproducible == Some(1),
        })
    }))
}

/// 丸ごと読む音声の値の上限：min(取り置き/2, 512 MiB)。
pub fn whole_limit(budget_total: u64) -> u64 {
    (default_reserved(budget_total) / 2).min(WHOLE_READ_CAP)
}

/// 直接読めない形式の音声を丸ごと読むか作業用のファイルにするかの境：min(64 MiB, 丸ごと読む上限)。値がこれを超えれば作業用の
/// ファイル（実機の予算では 64 MiB。小さな予算で、丸ごと読む上限と 64 MiB の間の長さを断らないよう頭打ちにする）。
pub fn work_threshold(budget_total: u64) -> u64 {
    WORK_ABOVE.min(whole_limit(budget_total))
}

/// 丸ごと読む前の確かめ：値が [`whole_limit`] を超えれば TooLarge（「約 N 分まで」はその形のレート・チャンネルで数える）。
pub(crate) fn check_whole_read(shape: &Shape, budget_total: u64) -> Result<(), EditorError> {
    let Shape::Audio { rate, channels, .. } = *shape else {
        return Ok(());
    };
    let limit = whole_limit(budget_total);
    if shape.value_bytes() <= limit {
        return Ok(());
    }
    let per_minute = u64::from(rate)
        .saturating_mul(u64::from(channels))
        .saturating_mul(4 * 60)
        .max(1);
    Err(EditorError::TooLarge(format!(
        "この形式の長い音声は、まだエディタで開けません（約 {} 分まで）",
        limit / per_minute
    )))
}

/// 源の LRU の上限：max(64 MiB, プレビューのスレッドの数 × 4 チャンク × ch × 256 KiB)。ただし音声の全体の値より大きくせず、
/// 取り置き（`default_reserved(予算の全体)`）の 1/4 で頭打ちにする（スレッドの数 × ch × 1 チャンクは
/// 下回らない。小さな予算の Mac・テストで入力が予算を埋めないように。段 1 の §4.6 からの変更）。
pub fn lru_bytes(threads: usize, channels: u16, frames: u64, budget_total: u64) -> u64 {
    let chunk = (AUDIO_CHUNK as u64) * 4;
    let ch = u64::from(channels.max(1));
    let threads = threads.max(1) as u64;
    let want = threads
        .saturating_mul(4)
        .saturating_mul(ch)
        .saturating_mul(chunk)
        .max(LRU_FLOOR);
    // LRU は読んだチャンクの中身の大きさで数える（最後の半端なチャンクは短い）ので、全体の値の大きさで足りる
    let whole = frames.saturating_mul(ch).saturating_mul(4);
    let cap =
        (default_reserved(budget_total) / 4).max(threads.saturating_mul(ch).saturating_mul(chunk));
    want.min(whole).min(cap)
}

/// ファイルを開く読み口（チャンクを読むたびに開く。`PcmFileSource` が使い回す）。
pub(crate) fn path_opener(path: &Path) -> Opener {
    let path: PathBuf = path.to_path_buf();
    Arc::new(move || -> Result<Box<dyn ReadSeek + Send>, EngineError> {
        Ok(Box::new(BufReader::new(File::open(&path)?)))
    })
}

/// 遅延で開く材料。
pub(crate) struct LazyCx<'a> {
    pub codecs: &'a CodecSet,
    pub original: &'a Path,
    pub source: Digest,
    pub format: &'a str,
    pub opts: &'a DecodeOptions,
    pub anchor: Option<&'a (Anchor, PathBuf)>,
    pub facts: Option<&'a FactsStamp>,
    pub limits: &'a Limits,
    pub threads: usize,
    /// 予算の全体（LRU の頭打ちに使う）。
    pub budget_total: u64,
}

/// 入力の指紋（記録があればその値のダイジェスト、なければ派生）。
fn fingerprint(
    source: Digest,
    decoder: &str,
    opts: &DecodeOptions,
    decoded: Option<&DecodeStamp>,
) -> Fingerprint {
    match decoded {
        Some(d) => Fingerprint::of_decoded(d),
        None => {
            let args = serde_json::json!({ "decoder": decoder, "options": opts });
            let bytes = serde_json::to_vec(&args).unwrap_or_default();
            Fingerprint::derive("audio-source", Fingerprint(source), &bytes)
        }
    }
}

/// 開いた遅延の値から、入力と要約を作る。
fn opened(
    value: Value,
    source: Digest,
    decoder: &str,
    opts: &DecodeOptions,
    decoded: Option<DecodeStamp>,
) -> Result<OpenedAudio, EditorError> {
    let Value::Audio(a) = &value else {
        return Err(EditorError::Failed("音声ではありません".into()));
    };
    let channels = u16::try_from(a.channels())
        .map_err(|_| EditorError::Failed("チャンネルが多すぎます".into()))?;
    let info = InputInfo {
        rate: a.rate(),
        channels,
        frames: a.frames() as u64,
        decoder: decoder.to_string(),
        lazy: true,
        decoded: decoded.clone(),
        work: None,
    };
    let fp = fingerprint(source, decoder, opts, decoded.as_ref());
    Ok(OpenedAudio {
        input: FingerprintedInput {
            value,
            source,
            fingerprint: fp,
        },
        info,
    })
}

/// この組が形式 `format` を遅延の源で開くか（`CodecSet::open_audio_source` の WAV・AIFF の判定と同じ：この形式の最初の段が
/// 固定の段の WAV・AIFF のデコーダ）。形を読む前に確かめる（ほかの形式の形の読みは重いことがあるので、予算の中で読む）。
pub(crate) fn opens_as_source(codecs: &CodecSet, format: &str) -> bool {
    let first = codecs.decoders_for(format).first().map(|d| d.id());
    matches!(
        (format, first),
        ("wav", Some(wav::DEC_ID)) | ("aiff", Some(aiff::DEC_ID))
    )
}

/// 遅延の源で開く（モジュールの説明の 1・2）。開けない形式・アンカーの記録が違うときは None（呼び手は作業用のファイルか
/// 丸ごと読み）。WAV・AIFF（[`opens_as_source`]）でなければ形も読まない。中止は見ない（見出しだけを読むので軽い）。
pub(crate) fn open_lazy(cx: &LazyCx<'_>) -> Result<Option<OpenedAudio>, EditorError> {
    let to_err = |e: EngineError| EditorError::from_engine("元を開けません", e);
    if let Some((a, path)) = cx.anchor {
        // アンカー：見出しの記録が DB の記録と同じなら遅延で使う。ほかは今の道で読み直して照らす
        let shape = File::open(path)
            .map_err(EngineError::from)
            .and_then(|f| anchor::shape(&mut BufReader::new(f), cx.limits));
        let Ok(Shape::Audio {
            channels,
            frames: kura_engine::Dim::Exact(frames),
            ..
        }) = shape
        else {
            return Ok(None);
        };
        let lru = lru_bytes(cx.threads, channels, frames, cx.budget_total);
        let Ok(Some(o)) = cx.codecs.open_audio_source(
            &path_opener(path),
            anchor::FORMAT,
            cx.opts,
            cx.limits,
            lru,
        ) else {
            return Ok(None);
        };
        let same = o.recorded.as_ref().is_some_and(|r| {
            r.value_digest == a.decoded.value_digest && r.codec == a.decoded.codec
        });
        if !same {
            tracing::info!("アンカーの見出しの記録が DB と違うので、元を読み直して開きます");
            return Ok(None);
        }
        // 元の読み取りのデコーダは記録のもの（アンカーの読み手の名前ではない。丸ごと読みに落ちたときと同じ値）
        return opened(
            o.value,
            cx.source,
            &a.decoded.codec,
            cx.opts,
            Some(a.decoded.clone()),
        )
        .map(Some);
    }
    // WAV・AIFF：見出しのチャンネル数で LRU を決める。ほかの形式は形を読まずに None（形を読むのにファイル全体・埋め込みの
    // 画像を読む段や、全体をデコードして数える段があるので、呼び手が段ごとに確保してから読む。段 4B の検証の直し）
    if !opens_as_source(cx.codecs, cx.format) {
        return Ok(None);
    }
    let shape = match cx
        .codecs
        .shape_path(cx.original, cx.format, cx.opts, cx.limits)
    {
        Ok((shape, _)) => shape,
        Err(e) if matches!(e.root(), EngineError::Unsupported(_)) => return Ok(None),
        Err(e) => return Err(to_err(e)),
    };
    let Shape::Audio {
        channels,
        frames: kura_engine::Dim::Exact(frames),
        ..
    } = shape
    else {
        return Ok(None);
    };
    let lru = lru_bytes(cx.threads, channels, frames, cx.budget_total);
    let o = match cx.codecs.open_audio_source(
        &path_opener(cx.original),
        cx.format,
        cx.opts,
        cx.limits,
        lru,
    ) {
        Ok(Some(o)) => o,
        Ok(None) => return Ok(None),
        Err(e) if matches!(e.root(), EngineError::Unsupported(_)) => return Ok(None),
        Err(e) => return Err(to_err(e)),
    };
    let decoded = cx
        .facts
        .filter(|f| f.decoder == o.decoder && *cx.opts == DecodeOptions::default())
        .map(|f| DecodeStamp {
            codec: f.decoder.clone(),
            tier: Tier::Pinned,
            reproducible: true,
            value_digest: Digest(f.value_digest),
            env: None,
        });
    opened(o.value, cx.source, o.decoder, cx.opts, decoded).map(Some)
}

/// 作業用のファイルを作る・開く材料（エディタと試聴の `/pcm` が使う）。
pub(crate) struct WorkCx<'a> {
    pub runtime: &'a EngineRuntime,
    pub codecs: &'a CodecSet,
    pub store: &'a WorkPcmStore,
    /// 元のファイルとその中身・形式・読み方。
    pub path: &'a Path,
    pub source: Digest,
    pub format: &'a str,
    pub opts: &'a DecodeOptions,
    pub limits: &'a Limits,
    /// 中止（エディタは閉じる・ほかを開くで止まる `editor.cancel`）。
    pub mon: &'a dyn Monitor,
}

impl WorkCx<'_> {
    /// 作業用のファイルの鍵（元の中身・形式・読み方。デコーダは段の順）。エディタと試聴の `/pcm` で同じなので、どちらかが
    /// 作ったものをもう一方が使い回す。
    pub(crate) fn key(&self) -> WorkKey {
        WorkKey {
            source: self.source.0,
            format: self.format.to_string(),
            options: self.opts.clone(),
            decoder: None,
        }
    }
}

/// 対話の枠で待たずに確保する（0 なら確保しない。予算の全体を超えれば TooLarge、今すぐ確保できなければ Busy）。
fn admit(budget: &Arc<MemoryBudget>, need: u64, what: &str) -> Result<Option<Lease>, EditorError> {
    if need == 0 {
        return Ok(None);
    }
    if need > budget.total() {
        return Err(EditorError::TooLarge(format!(
            "{what}のにメモリの予算を超えます（見積もり {need} バイト。予算は {} バイト）",
            budget.total()
        )));
    }
    match budget.try_acquire(need, Class::Interactive) {
        Ok(Some(lease)) => Ok(Some(lease)),
        Ok(None) => Err(EditorError::Busy(format!(
            "{what}メモリ（{need} バイト）が空いていません。少し待ってからやり直してください"
        ))),
        Err(e) => Err(EditorError::Busy(e.to_string())),
    }
}

/// 段の順に元の形を読む（`after` の段より後ろから。`CodecSet::shape_path_staged`）。段ごとに、その段で形を読む分を対話の枠で
/// 待たずに確保し直してから読み、読み終えたら返す。予算で断ったら外側の Err（Busy・TooLarge）、読めなければ内側の Err。
pub(crate) fn read_shape_staged(
    cx: &WorkCx<'_>,
    after: Option<&str>,
) -> Result<Result<(Shape, PickedStage), EngineError>, EditorError> {
    let budget = cx.runtime.budget();
    let mut lease: Option<Lease> = None;
    let mut refused: Option<EditorError> = None;
    let read = cx.runtime.preview(|_| {
        cx.codecs.shape_path_staged(
            cx.path,
            cx.format,
            cx.opts,
            cx.limits,
            cx.mon,
            after,
            &mut |stage| {
                lease = None;
                match admit(budget, stage.shape_bytes, "元の形を読む") {
                    Ok(l) => {
                        lease = l;
                        Ok(())
                    }
                    Err(e) => {
                        refused = Some(e);
                        Err(EngineError::Canceled)
                    }
                }
            },
        )
    });
    drop(lease);
    if let Some(e) = refused {
        return Err(e);
    }
    Ok(read)
}

/// 形 `shape` を段 `stage` で流して作業用のファイルに書く間の見積もり：その段の流れの作業領域（読み手が持つ埋め込みの画像
/// などを含む。`stage_stream_work_bytes`）＋ 窓 1 つ ＋ 値のダイジェストの作業列（プレビューのプールの仕事の数ぶん）＋ 書き手の
/// 作業列。流して読む段も形を読み直すので、形を読む分も覆う。
pub(crate) fn work_bytes(cx: &WorkCx<'_>, stage: &PickedStage, shape: &Shape) -> u64 {
    let Shape::Audio {
        channels, frames, ..
    } = *shape
    else {
        return u64::MAX;
    };
    let len = std::fs::metadata(cx.path).map(|m| m.len()).unwrap_or(0);
    let ch = usize::from(channels);
    cx.codecs
        .stage_stream_work_bytes(stage, cx.format, shape, len)
        .saturating_add(stream_window_bytes(ch, frames.max()))
        .saturating_add(PcmTreeDigest::work_bytes(
            ch,
            cx.runtime.preview_threads() as u64,
        ))
        .saturating_add(F32WavWriter::<File>::work_bytes())
        .max(stage.shape_bytes)
}

/// 置き場に渡す、同じ種類の誤り（読みの誤りそのものは呼び手が覚える。`EngineError` は `Clone` でない）。
fn same_kind(e: &EngineError) -> EngineError {
    match e.root() {
        EngineError::Canceled => EngineError::Canceled,
        EngineError::Unsupported(m) => EngineError::Unsupported(m.clone()),
        EngineError::Limit(m) => EngineError::Limit(m.clone()),
        other => EngineError::Internal(other.to_string()),
    }
}

/// 置き場の誤り（読みの誤りでないもの：空き・書き込み・封）をエディタの誤りに。
fn work_error(e: CoreError) -> EditorError {
    match e {
        CoreError::TooLarge(m) => EditorError::TooLarge(m),
        CoreError::Invalid(m) | CoreError::Unsupported(m) => {
            EditorError::Failed(format!("作業用のファイルを作れません：{m}"))
        }
        other => EditorError::Core(other),
    }
}

/// 作業用のファイルを用意する（`WorkPcmStore::ensure`。同じ鍵の封をし終えたものがあれば使い回し、読まない）。形 `shape` を読めた
/// 段 `stage` の見積もり（[`work_bytes`]）を対話の枠で待たずに確保して、その段で流して読む。その段が読めない
/// （`Unsupported`）なら次の段で形を読み直して確保し直す（段 2 の解析のジョブと同じ）。中止は `cx.mon`（Canceled）。
pub(crate) fn make_work(
    cx: &WorkCx<'_>,
    shape: Shape,
    stage: PickedStage,
) -> Result<WorkHandle, EditorError> {
    let key = cx.key();
    if let Some(h) = cx.store.find(&key) {
        return Ok(h);
    }
    let (mut shape, mut stage) = (shape, stage);
    loop {
        let need = work_bytes(cx, &stage, &shape);
        let lease = admit(cx.runtime.budget(), need, "元を作業用のファイルにする")?;
        let acx = AnchorContext {
            source: Some(cx.source),
            format: Some(cx.format.to_string()),
            options: cx.opts.clone(),
        };
        let id = stage.id;
        // 読みの誤りは、置き場の誤りに写される前の形で覚える（道具がない・中止・次の段へ回す、を見分ける）
        let mut read_err: Option<(EditorError, bool)> = None;
        let made = cx.runtime.preview(|_| {
            cx.store.ensure(
                &key,
                &shape,
                acx,
                |sink| match cx.codecs.decode_stream_with_path(
                    id, cx.path, cx.format, cx.opts, cx.limits, sink, cx.mon,
                ) {
                    Ok(stamp) => Ok(stamp),
                    Err(e) => {
                        let next = matches!(e.root(), EngineError::Unsupported(_));
                        let kind = same_kind(&e);
                        read_err = Some((EditorError::from_engine("元を読めません", e), next));
                        Err(kind)
                    }
                },
                cx.limits,
                cx.mon,
            )
        });
        drop(lease);
        let err = match made {
            Ok(h) => return Ok(h),
            Err(e) => e,
        };
        if cx.mon.canceled() {
            return Err(EditorError::Canceled);
        }
        let (read, next) = match read_err {
            Some(r) => r,
            None => return Err(work_error(err)),
        };
        if !next {
            return Err(read);
        }
        // この段では読めない：次の段（あれば）で形を読み直す。次の段がない・次の段も読めなければ、この段の誤り
        match read_shape_staged(cx, Some(id))? {
            Ok((s, st)) => {
                shape = s;
                stage = st;
            }
            Err(e) if matches!(e.root(), EngineError::Unsupported(_)) => return Err(read),
            Err(e) => return Err(EditorError::from_engine("元の大きさを読めません", e)),
        }
    }
}

/// 作業用のファイルを遅延の源で開いたもの。
pub(crate) struct OpenedWork {
    /// 遅延の値（メタデータは封の値＝流れの終わりのメタデータ）。
    pub value: Value,
    /// 元を読んだ記録（封の記録。値のダイジェストは流れのダイジェスト）。
    pub decoded: DecodeStamp,
    pub work: WorkRef,
}

/// 作業用のファイルを遅延の源で開く（`anchor::open_source`。源の LRU は [`lru_bytes`]）。
pub(crate) fn open_work(
    handle: WorkHandle,
    limits: &Limits,
    threads: usize,
    budget_total: u64,
) -> Result<OpenedWork, EditorError> {
    let (channels, frames) = match handle.pcm().shape {
        Shape::Audio {
            channels, frames, ..
        } => (channels, frames.max()),
        _ => {
            return Err(EditorError::Failed(
                "作業用のファイルが音声ではありません".into(),
            ));
        }
    };
    let lru = lru_bytes(threads, channels, frames, budget_total);
    let opened = anchor::open_source(&handle.opener(), limits, lru)
        .map_err(|e| EditorError::from_engine("作業用のファイルを開けません", e))?
        .ok_or_else(|| EditorError::Failed("作業用のファイルが音声ではありません".into()))?;
    Ok(OpenedWork {
        value: opened.value,
        decoded: handle.pcm().decoded.clone(),
        work: WorkRef(Arc::new(handle)),
    })
}

/// 作業用のファイルから開いた入力と要約（エディタの入力。指紋は封の記録の値のダイジェスト＝丸ごと読んだときと同じ）。
pub(crate) fn work_input(o: OpenedWork, source: Digest) -> Result<OpenedAudio, EditorError> {
    let Value::Audio(a) = &o.value else {
        return Err(EditorError::Failed("音声ではありません".into()));
    };
    let channels = u16::try_from(a.channels())
        .map_err(|_| EditorError::Failed("チャンネルが多すぎます".into()))?;
    let info = InputInfo {
        rate: a.rate(),
        channels,
        frames: a.frames() as u64,
        decoder: o.decoded.codec.clone(),
        lazy: true,
        decoded: Some(o.decoded.clone()),
        work: Some(o.work),
    };
    Ok(OpenedAudio {
        input: FingerprintedInput::decoded(o.value, source, &o.decoded),
        info,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::Dim;

    #[test]
    fn limits_follow_the_budget() {
        let gib = 1u64 << 30;
        assert_eq!(whole_limit(8 * gib), 512 << 20);
        assert_eq!(whole_limit(gib), 256 << 20);
        let shape = |frames| Shape::Audio {
            rate: 48_000,
            channels: 2,
            frames: Dim::Exact(frames),
        };
        // 48 kHz ステレオ・512 MiB は約 23 分
        assert!(check_whole_read(&shape(48_000 * 60 * 23), 8 * gib).is_ok());
        let err = check_whole_read(&shape(48_000 * 60 * 24), 8 * gib).unwrap_err();
        assert!(
            matches!(&err, EditorError::TooLarge(m) if m.contains("約 23 分")),
            "{err}"
        );
        let chunk = 256u64 << 10;
        let long = 48_000 * 3_600;
        assert_eq!(lru_bytes(2, 2, long, 8 * gib), LRU_FLOOR);
        assert_eq!(lru_bytes(64, 2, long, 64 * gib), 64 * 4 * 2 * chunk);
        assert_eq!(
            lru_bytes(64, 8, long, 64 * gib),
            256 << 20,
            "取り置き 1 GiB の 1/4"
        );
        // 全体の値より大きくしない・取り置きの 1/4 で頭打ち（スレッド × ch × 1 チャンクは下回らない）
        assert_eq!(
            lru_bytes(2, 1, 3 * 65_536 + 1, 8 * gib),
            (3 * 65_536 + 1) * 4
        );
        assert_eq!(lru_bytes(2, 2, long, 64 << 20), 8 << 20);
        assert_eq!(lru_bytes(8, 2, long, 8 << 20), 16 * chunk);
    }

    /// 遅延の源で開くか（形を読む前の判定）は `open_audio_source` と同じ：WAV・AIFF で、最初の段が固定の段のデコーダのとき
    /// だけ。ほかの形式・最初の段がほかのデコーダの組は開かない（`open_audio_source` も None）。
    #[test]
    fn opens_as_source_follows_the_first_stage() {
        use kura_engine::testgen::InputGen;
        use kura_engine::testing::platform::FakePlatformDecoder;

        let pinned = CodecSet::pinned();
        for (format, want) in [
            ("wav", true),
            ("aiff", true),
            ("flac", false),
            ("mp3", false),
            ("ogg", false),
            ("m4a", false),
        ] {
            assert_eq!(opens_as_source(&pinned, format), want, "{format}");
        }
        // 固定の段の WAV・AIFF がなく、Platform の段が最初
        let mut platform = CodecSet::default();
        platform
            .add_decoder(Arc::new(FakePlatformDecoder::new(
                "fake-at-dec/1",
                &["wav", "aiff"],
                InputGen::AudioNoise {
                    rate: 8_000,
                    channels: 1,
                    frames: 10,
                    seed: 1,
                },
            )))
            .unwrap();
        let wav = InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: 10,
            seed: 1,
        }
        .generate()
        .unwrap();
        let bytes = pinned
            .encode_bytes(&wav, "wav", &serde_json::json!({}), &kura_engine::NoMonitor)
            .unwrap()
            .0;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        std::fs::write(&path, &bytes).unwrap();
        for format in ["wav", "aiff"] {
            assert!(!opens_as_source(&platform, format), "{format}");
        }
        let opens = |set: &CodecSet| {
            set.open_audio_source(
                &path_opener(&path),
                "wav",
                &Default::default(),
                &Default::default(),
                1 << 20,
            )
            .unwrap()
            .is_some()
        };
        assert!(opens(&pinned));
        assert!(!opens(&platform));
    }
}
