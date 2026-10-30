//! 音声の元の開き方（M13 段 4。docs/editing.md §9.11・§12.10）。
//!
//! 1. **アンカーがある**：アンカーの f32 WAV を遅延の源で開く（`CodecSet::open_audio_source`）。アンカーの見出しの記録が DB の
//!    アンカーの記録と同じなら、その記録の値のダイジェストを入力の指紋にする。違う・開けなければ今の道（記録のデコーダで読み
//!    直して照らす。`super::super::source::read`）に落とす。値を照らすのは開くときではなく保存のとき（段 3）。
//! 2. **WAV・AIFF**（`open_audio_source` が Some を返すもの）：遅延の源で開く。DB の事実の行（`media_facts`）のデコーダが開いた
//!    デコーダと同じで、文書の入力の読み方が既定なら、その値のダイジェストで記録（`DecodeStamp`。固定の段・再現する）を作る。
//!    そうでなければ記録はなく、指紋は派生（`Fingerprint::derive("audio-source", 元の中身, デコーダと読み方)`）。
//! 3. **それ以外**（FLAC・Ogg・MP3・AAC・Platform・External、A-law の WAV など）：段 2 が入るまで、今の道で丸ごと読む。読む前に、
//!    値が [`whole_limit`]（min(取り置き/2, 512 MiB)。48 kHz ステレオで約 23 分）を超えれば TooLarge。
//!
//! 遅延の値の源の LRU の上限は [`lru_bytes`]（max(64 MiB, プレビューのスレッド × 4 チャンク × ch × 256 KiB) を、音声の全体の値と
//! 取り置きの 1/4 で頭打ち）。入力は `register_external` で数える（源 1 つにつき LRU の上限を 1 回）ので、読む段の予算
//! （`admit_read`）は通さない。

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kura_engine::audio::source::Opener;
use kura_engine::budget::default_reserved;
use kura_engine::codec::{CodecSet, DecodeStamp, ReadSeek, Tier, anchor};
use kura_engine::doc::DecodeOptions;
use kura_engine::render::cache::{Fingerprint, FingerprintedInput};
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{Digest, EngineError, Limits, Shape, Value};
use rusqlite::{Connection, OptionalExtension};

use super::super::EditorError;
use crate::edit::anchors::Anchor;

/// 丸ごと読む音声の上限の頭打ち（バイト）。
pub const WHOLE_READ_CAP: u64 = 512 << 20;
/// 源の LRU の上限の下限（バイト）。
pub const LRU_FLOOR: u64 = 64 << 20;

/// 開いた音声の入力の要約（`AudioState` が持つ）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InputInfo {
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// 値を読んだデコーダの識別子（`/edit/{s}/input/pcm` の kura チャンクに入れる）。
    pub decoder: String,
    /// 遅延の源で開いた（全体を読んでいない）。
    pub lazy: bool,
    /// 読み取りの記録（分からなければ None。段 3 の貸し出しで使う）。
    pub decoded: Option<DecodeStamp>,
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

/// 源の LRU の上限：max(64 MiB, プレビューのスレッドの数 × 4 チャンク × ch × 256 KiB)。ただし音声の全体の値（チャンクに
/// 切り上げ）より大きくせず、取り置き（`default_reserved(予算の全体)`）の 1/4 で頭打ちにする（スレッドの数 × ch × 1 チャンクは
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
    let whole = frames
        .div_ceil(AUDIO_CHUNK as u64)
        .saturating_mul(ch)
        .saturating_mul(chunk);
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

/// 遅延の源で開く（モジュールの説明の 1・2）。開けない形式・アンカーの記録が違うときは None（呼び手は丸ごと読む）。中止は
/// 見ない（見出しだけを読むので軽い）。
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
        return opened(
            o.value,
            cx.source,
            o.decoder,
            cx.opts,
            Some(a.decoded.clone()),
        )
        .map(Some);
    }
    // WAV・AIFF：見出しのチャンネル数で LRU を決める
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
        assert_eq!(lru_bytes(2, 1, 3 * 65_536 + 1, 8 * gib), 4 * chunk);
        assert_eq!(lru_bytes(2, 2, long, 64 << 20), 8 << 20);
        assert_eq!(lru_bytes(8, 2, long, 8 << 20), 16 * chunk);
    }
}
