//! 音声の元の開き方（M13 段 4。docs/editing.md §9.11・§12.10）。
//!
//! 1. **アンカーがある**：アンカーの f32 WAV を遅延の源で開く（`CodecSet::open_audio_source`）。アンカーの見出しの記録が DB の
//!    アンカーの記録と同じなら、その記録の値のダイジェストを入力の指紋にし、元の読み取りのデコーダは記録のもの（アンカーの
//!    読み手の名前ではない）にする。違う・開けなければ今の道（記録のデコーダで読み直して照らす。`super::super::source::read`）
//!    に落とす。値を照らすのは開くときではなく保存のとき（段 3）。
//! 2. **WAV・AIFF**（`open_audio_source` が Some を返すもの）：遅延の源で開く。見出しを読む前に、この形式の最初の段が固定の段の
//!    WAV・AIFF のデコーダか確かめ（[`opens_as_source`]）、ほかの形式の形はここでは読まない（予算の外で読まない。段 4B の
//!    検証の直し）。読むのはその段の見出しだけ（`CodecSet::shape_path_first`）で、その段が読めない変種（A-law など）は
//!    後ろの段へ回さず 3 に任せる（段 7 の検証の直し）。DB の事実の行（`media_facts`）のデコーダが開いた
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
//! 遅延の値の源の LRU の上限は [`lru_bytes`]（max(64 MiB, max(プレビューのスレッド × 4, 64) チャンク × ch × 256 KiB) を、音声の
//! 全体の値と取り置きの 1/4 で頭打ち）。入力は `register_external` で数える（源 1 つにつき LRU の上限を 1 回）ので、読む段の予算
//! （`admit_read`）は通さない。

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kura_engine::audio::digest::PcmTreeDigest;
use kura_engine::audio::source::{KEPT_VIEW_CHUNKS, Opener};
use kura_engine::audio::stream::{STREAM_CHUNKS, StreamEnd};
use kura_engine::budget::{Class, Lease, MemoryBudget, default_reserved};
use kura_engine::codec::anchor::AnchorContext;
use kura_engine::codec::stream::stream_window_bytes;
use kura_engine::codec::wav::{self, F32WavWriter};
use kura_engine::codec::{CodecSet, DecodeStamp, PickedStage, ReadSeek, Tier, aiff, anchor};
use kura_engine::codec::{DecodeInfo, DecodeSink};
use kura_engine::doc::DecodeOptions;
use kura_engine::render::cache::{Fingerprint, FingerprintedInput};
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{Digest, EngineError, Limits, Monitor, Shape, Value};
use rusqlite::{Connection, OptionalExtension};

use super::super::EditorError;
use crate::CoreError;
use crate::edit::EngineRuntime;
use crate::edit::anchors::Anchor;
use crate::edit::services::{LentAnchor, LentInput};
use crate::edit::work_pcm::{WorkHandle, WorkKey, WorkPcmStore};

/// 丸ごと読む音声の上限の頭打ち（バイト）。
pub const WHOLE_READ_CAP: u64 = 512 << 20;
/// 源の LRU の上限の下限（バイト）。
pub const LRU_FLOOR: u64 = 64 << 20;
/// 源の LRU に置きたいチャンクの数（全チャンネル）：主の段に区間で頼める上限（`KEPT_VIEW_CHUNKS`）、流しの段に流れの窓
/// （`STREAM_CHUNKS`）。`stream_tier_bytes` の 1/4 の分け方でちょうど分かれる。
pub const LRU_CHUNKS: u64 = KEPT_VIEW_CHUNKS + STREAM_CHUNKS as u64;
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

    /// 鍵 `key` の作業用のファイルか（置き場の名前は鍵から決まる）。
    pub(crate) fn is_for(&self, key: &WorkKey) -> bool {
        key.file_name()
            .is_ok_and(|name| self.path().file_name() == Some(std::ffi::OsStr::new(&name)))
    }
}

/// 作業用のファイルの鍵（元の中身・形式・読み方。デコーダは段の順）。エディタと試聴の `/pcm` で同じなので、どちらかが
/// 作ったものをもう一方が使い回す。
pub(crate) fn work_key(source: Digest, format: &str, opts: &DecodeOptions) -> WorkKey {
    WorkKey {
        source: source.0,
        format: format.to_string(),
        options: opts.clone(),
        decoder: None,
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
    /// 作業用のファイル（直接読めない形式の長い音声。持つことで寿命を決め、閉じると落ちる。保存に貸すときは参照を写す）。
    pub work: Option<WorkRef>,
}

/// 保存に貸す入力の材料（[`lent_input`]）。
pub(crate) struct LendCx<'a> {
    pub item_id: i64,
    /// 元の中身・形式・読み方の設定のダイジェスト（`anchors::options_digest`）。
    pub source: Digest,
    pub format: &'a str,
    pub options_digest: [u8; 32],
    /// 開くときに見つけたアンカー（あれば、遅延の入力はこのアンカーから開いたもの）。
    pub anchor: Option<&'a Anchor>,
}

/// 開いた入力を保存に貸す形（M13 段 4C。`EngineServices::lend_input`。docs/editing.md §14）。貸すのは遅延で開いた入力のうち、
/// 読み取りの記録が分かるものだけ：
///
/// - 作業用のファイルから開いた：参照ごと貸す（保存が借りている間は消えない）。封の記録は流れのダイジェストなので照らし済み。
/// - アンカーから遅延で開いた：アンカーの記録で、照らしていない印（保存が照らしの回で値を記録と照らす）。
/// - WAV・AIFF の直接読み：記録（中身のハッシュの名前のファイルを同じデコーダで読んだ事実の行）があれば照らし済み。記録の分から
///   ないものは貸さない（保存は自分で遅延で開くので軽い）。
///
/// 丸ごと読んだ入力（短い FLAC など・アンカーが遅延で使えずに読み直したもの）は貸さない：値の中身はエディタの予算で数えて
/// いて、保存は借りた入力の源の LRU しか数えないので、エディタを閉じたあとも保存が持つと予算の外になる（短いものは読み直しても
/// 軽い）。
pub(crate) fn lent_input(
    cx: &LendCx<'_>,
    input: &FingerprintedInput,
    info: &InputInfo,
) -> Option<LentInput> {
    if !info.lazy {
        return None;
    }
    let decoded = info.decoded.clone()?;
    let (anchor, work, verified) = match (&info.work, cx.anchor) {
        (Some(w), _) => (LentAnchor::None, Some(WorkHandle::clone(&w.0)), true),
        (None, Some(a)) => (LentAnchor::Existing(a.clone()), None, false),
        (None, None) => (LentAnchor::None, None, true),
    };
    Some(LentInput {
        item_id: cx.item_id,
        source: cx.source.0,
        options_digest: Digest(cx.options_digest),
        format: cx.format.to_string(),
        value: input.value.clone(),
        decoded,
        anchor,
        work,
        verified,
    })
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

/// 源の LRU の上限：max(64 MiB, max(プレビューのスレッドの数 × 4, [`LRU_CHUNKS`]) チャンク × ch × 256 KiB)（[`LRU_CHUNKS`] が効くのは
/// 5 チャンネルから：区間と流れの窓が主の段と流しの段にそれぞれ収まる）。ただし音声の全体の値より大きくせず、
/// 取り置き（`default_reserved(予算の全体)`）の 1/4 で頭打ちにする（スレッドの数 × ch × 1 チャンクは
/// 下回らない。小さな予算の Mac・テストで入力が予算を埋めないように。段 1 の §4.6 からの変更）。
pub fn lru_bytes(threads: usize, channels: u16, frames: u64, budget_total: u64) -> u64 {
    let chunk = (AUDIO_CHUNK as u64) * 4;
    let ch = u64::from(channels.max(1));
    let threads = threads.max(1) as u64;
    let want = threads
        .saturating_mul(4)
        .max(LRU_CHUNKS)
        .saturating_mul(ch)
        .saturating_mul(chunk)
        .max(LRU_FLOOR);
    // LRU は読んだチャンクの中身の大きさで数える（最後の半端なチャンクは短い）ので、全体の値の大きさで足りる。全体が収まる
    // 源は段を分けない（`ChunkLru::for_source`。全チャンクが主の段に入る）
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
    /// 中止（エディタは閉じる・ほかを開くで止まる `editor.cancel`）。
    pub mon: &'a dyn Monitor,
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
/// 丸ごと読み）。WAV・AIFF（[`opens_as_source`]）でなければ形も読まない。読むのは固定の段の見出しだけ（確保しない）で、その段が
/// 読めない変種も None（後ろの段の形は、呼び手が段ごとに確保して読む）。
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
        .shape_path_first(cx.original, cx.format, cx.opts, cx.limits, cx.mon)
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
    /// 作業用のファイルの鍵（[`work_key`]）。
    pub(crate) fn key(&self) -> WorkKey {
        work_key(self.source, self.format, self.opts)
    }
}

/// 対話の枠で待たずに確保する（0 なら確保しない。予算の全体を超えれば TooLarge、今すぐ確保できなければ Busy）。
pub(crate) fn admit(
    budget: &Arc<MemoryBudget>,
    need: u64,
    what: &str,
) -> Result<Option<Lease>, EditorError> {
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
    shape_staged(
        &ShapeCx {
            runtime: cx.runtime,
            codecs: cx.codecs,
            path: cx.path,
            format: cx.format,
            opts: cx.opts,
            limits: cx.limits,
            mon: cx.mon,
        },
        after,
    )
}

/// 段ごとに確保して形を読む材料（[`shape_staged`]）。
pub(crate) struct ShapeCx<'a> {
    pub runtime: &'a EngineRuntime,
    pub codecs: &'a CodecSet,
    /// 元のファイルとその形式・読み方。
    pub path: &'a Path,
    pub format: &'a str,
    pub opts: &'a DecodeOptions,
    pub limits: &'a Limits,
    pub mon: &'a dyn Monitor,
}

/// [`read_shape_staged`] の本体（作業用のファイルの置き場を使わない読み手も使う。エディタの今の道の形の読み。段 7）。
pub(crate) fn shape_staged(
    cx: &ShapeCx<'_>,
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

/// 作業用のファイルの書き手（`WorkPcmStore::ensure` が渡す受け手）を包み、書き手が返した Io の誤り（ENOSPC など）を、元を
/// 読む誤りと分けて覚える（段 4B の検証の直し）。読み手には同じ種類の写し（`EngineError::Io`）を返す。書けた値のバイト数も
/// 数える（空きの不足の目安）。
struct WriteGuard<'a> {
    inner: &'a mut dyn DecodeSink,
    /// 書き手が返した最初の Io の誤り。
    failed: Option<std::io::Error>,
    /// 今の試み（`begin` から）で書けた値のバイト数。
    written: u64,
}

impl<'a> WriteGuard<'a> {
    fn new(inner: &'a mut dyn DecodeSink) -> Self {
        Self {
            inner,
            failed: None,
            written: 0,
        }
    }

    /// 書き手の誤りを覚える（Io だけ。ほかの誤りはそのまま返す）。
    fn check(&mut self, r: Result<(), EngineError>) -> Result<(), EngineError> {
        match r {
            Err(EngineError::Io(e)) => {
                let copy = match e.raw_os_error() {
                    Some(code) => std::io::Error::from_raw_os_error(code),
                    None => std::io::Error::new(e.kind(), e.to_string()),
                };
                if self.failed.is_none() {
                    self.failed = Some(e);
                }
                Err(EngineError::Io(copy))
            }
            other => other,
        }
    }

    /// 書き手の誤り（あれば）と、それまでに書けた値のバイト数。
    fn into_failure(self) -> Option<(std::io::Error, u64)> {
        let written = self.written;
        self.failed.map(|e| (e, written))
    }
}

impl DecodeSink for WriteGuard<'_> {
    fn begin(&mut self, info: &DecodeInfo) -> Result<(), EngineError> {
        self.written = 0;
        let r = self.inner.begin(info);
        self.check(r)
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let r = self.inner.feed(start, planes, mon);
        if r.is_ok() {
            let n = planes
                .iter()
                .map(|p| p.len() as u64)
                .fold(0u64, u64::saturating_add);
            self.written = self.written.saturating_add(n.saturating_mul(4));
        }
        self.check(r)
    }

    fn end(&mut self, end: &StreamEnd) -> Result<(), EngineError> {
        let r = self.inner.end(end);
        self.check(r)
    }
}

/// 作業用のファイルに書く途中の誤り（書き手の Io）を置き場の誤りに：空きの不足（ENOSPC。`CoreError::is_no_space`）は
/// `NoSpace`（要る空きは書く値の大きさ `need`、いまの空きは書けた分 `written` を目安にする）、ほかは `Io`。どちらも
/// `EditorError::Core`（src-tauri の写しは NoSpace → `AppErrorCode::NoSpace`、Io → `AppErrorCode::Io`）。
fn write_error(e: std::io::Error, need: u64, written: u64) -> EditorError {
    let core = CoreError::Io(e);
    if core.is_no_space() {
        return EditorError::Core(CoreError::NoSpace {
            needed: need.max(written.saturating_add(1)),
            available: written,
        });
    }
    EditorError::Core(core)
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
/// 作業用のファイルに書く途中の書き手の Io は、読みの誤りより先に置き場の誤りにする（[`write_error`]。空きの不足は
/// `Core(NoSpace)`、ほかは `Core(Io)`）。
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
        // 読みの誤りは、置き場の誤りに写される前の形で覚える（道具がない・中止・次の段へ回す、を見分ける）。書き手（作業用の
        // ファイル）の Io は読みの誤りと分けて覚える
        let mut read_err: Option<(EditorError, bool)> = None;
        let mut write_err: Option<(std::io::Error, u64)> = None;
        let made = cx.runtime.preview(|_| {
            cx.store.ensure(
                &key,
                &shape,
                acx,
                |sink| {
                    let mut guard = WriteGuard::new(sink);
                    let r = cx.codecs.decode_stream_with_path(
                        id, cx.path, cx.format, cx.opts, cx.limits, &mut guard, cx.mon,
                    );
                    write_err = guard.into_failure();
                    match r {
                        Ok(stamp) => Ok(stamp),
                        Err(e) if write_err.is_some() => Err(same_kind(&e)),
                        Err(e) => {
                            let next = matches!(e.root(), EngineError::Unsupported(_));
                            let kind = same_kind(&e);
                            read_err = Some((EditorError::from_engine("元を読めません", e), next));
                            Err(kind)
                        }
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
        // 書き手の誤り（置き場そのもの）は読みの誤りより先に
        if let Some((e, written)) = write_err {
            return Err(write_error(e, shape.value_bytes(), written));
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
        assert_eq!(
            lru_bytes(4, 4, long, 8 * gib),
            LRU_FLOOR,
            "4 チャンネルまで"
        );
        assert_eq!(lru_bytes(4, 5, long, 8 * gib), 64 * 5 * chunk);
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

    /// 区間で頼める上限（48 チャンク）は、実機の予算の LRU の上限なら主の段に収まる（段 7 より前に 64 MiB の LRU に
    /// 収まっていた 5 チャンネルも）。2 回読んでも源から読むのは 1 回だけ。
    #[test]
    fn view_chunks_fit_the_source_lru() {
        use kura_engine::audio::source::AudioSource;
        use kura_engine::testing::source::SyntheticSource;

        let gib = 1u64 << 30;
        let frames = 48_000 * 3_600;
        for channels in [1u16, 2, 4, 5] {
            let lru = lru_bytes(4, channels, frames, 8 * gib);
            let src = SyntheticSource::new(48_000, usize::from(channels), frames, 1, lru);
            let n = crate::edit::editor::audio::MAX_VIEW_CHUNKS as u32;
            for _ in 0..2 {
                for k in 100..100 + n {
                    src.read_chunk(k).unwrap();
                }
            }
            assert_eq!(src.stats().reads, u64::from(n), "{channels} チャンネル");
        }
    }

    /// `fail_after` 回の窓のあと、書くと `error` の Io を返す受け手（書き手の誤りの代わり）。
    struct FailingSink {
        fail_after: usize,
        feeds: usize,
        error: fn() -> std::io::Error,
        internal: bool,
    }

    impl DecodeSink for FailingSink {
        fn begin(&mut self, _info: &DecodeInfo) -> Result<(), EngineError> {
            self.feeds = 0;
            Ok(())
        }

        fn feed(
            &mut self,
            _start: u64,
            _planes: &[&[f32]],
            _mon: &dyn Monitor,
        ) -> Result<(), EngineError> {
            self.feeds += 1;
            if self.feeds <= self.fail_after {
                Ok(())
            } else if self.internal {
                Err(EngineError::Internal("窓が順でない".into()))
            } else {
                Err(EngineError::Io((self.error)()))
            }
        }

        fn end(&mut self, _end: &StreamEnd) -> Result<(), EngineError> {
            Err(EngineError::Io((self.error)()))
        }
    }

    /// 段 4B の検証の直し 4：書き手が返した Io は覚えて（読み手には同じ種類の写しを返す）、空きの不足（ENOSPC）は
    /// `Core(NoSpace)`（いまの空きは書けた分）、ほかの Io は `Core(Io)` に写す。書き手の Io でない誤りは覚えない。
    #[test]
    fn write_errors_are_kept_apart_from_read_errors() {
        let info = DecodeInfo {
            rate: 8_000,
            channels: 2,
            frames: Dim::Exact(100),
            expect: 100,
        };
        let planes: [&[f32]; 2] = [&[0.0; 10], &[0.0; 10]];
        let enospc = || std::io::Error::from_raw_os_error(28);
        let mut sink = FailingSink {
            fail_after: 2,
            feeds: 0,
            error: enospc,
            internal: false,
        };
        let mut guard = WriteGuard::new(&mut sink);
        guard.begin(&info).unwrap();
        guard.feed(0, &planes, &kura_engine::NoMonitor).unwrap();
        guard.feed(10, &planes, &kura_engine::NoMonitor).unwrap();
        let err = guard
            .feed(20, &planes, &kura_engine::NoMonitor)
            .unwrap_err();
        assert!(
            matches!(&err, EngineError::Io(e) if e.raw_os_error() == Some(28)),
            "読み手にも Io の写しを返す：{err}"
        );
        let (e, written) = guard.into_failure().unwrap();
        assert_eq!(e.raw_os_error(), Some(28));
        assert_eq!(written, 2 * 2 * 10 * 4, "書けた 2 窓の分");
        let mapped = write_error(e, 1 << 20, written);
        assert!(
            matches!(
                &mapped,
                EditorError::Core(CoreError::NoSpace { needed, available })
                    if *needed == 1 << 20 && *available == written
            ),
            "{mapped}"
        );
        assert!(
            mapped
                .to_string()
                .starts_with("ディスクの空き容量が足りません")
        );
        // ほかの Io（EFBIG）は Io のまま。end の誤りも覚える
        let mut sink = FailingSink {
            fail_after: usize::MAX,
            feeds: 0,
            error: || std::io::Error::from_raw_os_error(27),
            internal: false,
        };
        let mut guard = WriteGuard::new(&mut sink);
        guard.begin(&info).unwrap();
        guard.feed(0, &planes, &kura_engine::NoMonitor).unwrap();
        assert!(
            guard
                .end(&StreamEnd {
                    frames: 10,
                    meta: Default::default(),
                })
                .is_err()
        );
        let (e, written) = guard.into_failure().unwrap();
        let mapped = write_error(e, 1 << 20, written);
        assert!(
            matches!(&mapped, EditorError::Core(CoreError::Io(io)) if io.raw_os_error() == Some(27)),
            "{mapped}"
        );
        // 書き手の Io でない誤りは覚えない（読みの誤りとして扱う）
        let mut sink = FailingSink {
            fail_after: 0,
            feeds: 0,
            error: enospc,
            internal: true,
        };
        let mut guard = WriteGuard::new(&mut sink);
        guard.begin(&info).unwrap();
        assert!(matches!(
            guard.feed(0, &planes, &kura_engine::NoMonitor),
            Err(EngineError::Internal(_))
        ));
        assert!(guard.into_failure().is_none());
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
