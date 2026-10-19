//! 試聴の PCM（kura:// の `/pcm/{hash}/{start}-{end}`。M11 の決定 8・critic 18。docs/architecture.md §4）。
//!
//! - 元をエンジンのコーデックの組（ライブラリの [`EngineServices`]。解析と同じ組）で読み、フレームの半開区間
//!   `[start, end)` を WAV にして返す。並び（RIFF・fmt・kura・data）と kura チャンク（版 1）は
//!   `tests/vectors/pcm/wav.json`（TS の `src/lib/asset/wav.ts` と共有）が正。
//! - kura チャンクには、この PCM を読んだのと同じ読み取りのループ点・フレーム数・デコーダの識別子を入れる。試聴は DB の値
//!   ではなくこれを使う（解析のときとデコーダが変わっていても、継ぎ目が PCM とずれない）。
//! - 元が 16 ビット以下の整数で、区間の標本がすべて 16 ビットの格子の上にあれば 16 ビット（値は同じ）、ほかは 32 ビット
//!   浮動小数で書く（非可逆の形式は 16 ビットと名乗っても格子の上にないので浮動小数になる）。
//! - 読んだ結果は (ライブラリ, ハッシュ, デコーダの識別子) で直近の 1 件だけ覚える（[`PcmCache`]。試聴は前奏と本体・継ぎ目の
//!   前後を続けて取るので、2 回目からは読まない）。覚えている間は予算（対話）を持ち、使わずに 60 秒たつと捨てる。
//! - ジョブにしない（利用者がその場で待つ短い表示の用意。決定 8）。呼び手が `spawn_blocking` で呼ぶ。予算は対話
//!   （`Class::Interactive`。待たない）で、足りなければ 503。範囲が音声の外か、浮動小数で 64 MiB を超える長さは 416。
//!   読むのはエンジンのプレビューのプールの中（`EngineRuntime::preview`）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use kura_engine::budget::{Class, Lease, MemoryBudget};
use kura_engine::codec::CodecSet;
use kura_engine::codec::pcm::quantize;
use kura_engine::doc::DecodeOptions;
use kura_engine::shape::Shape;
use kura_engine::value::{Audio, AudioSource};
use kura_engine::{EngineError, Limits, NoMonitor, Value};

use crate::edit::EngineServices;
use crate::serve::PcmSource;

/// kura チャンクの並びの版。
pub const KURA_CHUNK_VERSION: u16 = 1;

/// 1 回の応答の上限（浮動小数にしたときのバイト数。フロントの `PCM_MAX_BYTES` と同じ）。
pub const MAX_PCM_BYTES: u64 = 64 << 20;

/// デコーダの識別子のバイト数の上限（フロントの読み手と同じ）。
pub const MAX_DECODER_ID_BYTES: usize = 255;

/// 読んだ結果を使わずに覚えておく時間。
pub const CACHE_TTL: Duration = Duration::from_secs(60);

/// WAV の標本の形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 16 ビットの整数（形式 1）。
    I16,
    /// 32 ビットの浮動小数（形式 3）。
    F32,
}

/// kura チャンクの中身。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KuraChunk<'a> {
    /// 応答の先頭が、元の音声の何フレーム目か。
    pub start: u64,
    /// 元の音声のフレーム数（同じ読み取りで数えたもの）。
    pub total: u64,
    /// ループ点（同じ読み取りのもの。終わりは含まない）。
    pub loop_range: Option<(u64, u64)>,
    /// デコーダの識別子（`DecodeStamp::codec`）。
    pub decoder: &'a str,
}

/// 試聴の PCM を返せない理由。
#[derive(Debug, thiserror::Error)]
pub enum PcmError {
    /// 試聴できる音声がない（ファイルがない・音声でない）。
    #[error("試聴できる音声がありません")]
    NotFound,
    /// 範囲が音声の外。
    #[error("範囲が音声の外です（{start}〜{end}。音声は {total} フレーム）")]
    OutOfRange { start: u64, end: u64, total: u64 },
    /// 一度に返せる長さを超える。
    #[error("一度に返せる長さを超えます（{bytes} バイト。上限は {MAX_PCM_BYTES} バイト）")]
    TooLong { bytes: u64 },
    /// メモリの予算が空いていない（少し待てば通りうる）。
    #[error("メモリの予算が空いていません（{0}）")]
    Busy(String),
    /// エンジンを使えない（ライブラリを開いていない・実行の場を作れない）。
    #[error("エンジンを使えません（{0}）")]
    Unavailable(String),
    /// 読めない・書けない。
    #[error("試聴の PCM を作れませんでした（{0}）")]
    Failed(String),
}

impl PcmError {
    /// HTTP の状態（404・416・503・500）。
    pub fn status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::OutOfRange { .. } | Self::TooLong { .. } => 416,
            Self::Busy(_) | Self::Unavailable(_) => 503,
            Self::Failed(_) => 500,
        }
    }
}

/// 書く形式を決める：元が 16 ビット以下の整数で、すべての標本が 16 ビットの格子の上にある（16 ビットで書いて読むと同じ
/// 値に戻る）なら 16 ビット、ほかは浮動小数。
pub fn sample_format(source: Option<&AudioSource>, planes: &[&[f32]]) -> SampleFormat {
    let small_int = source.is_some_and(|s| !s.float && s.bits <= 16);
    let on_grid = |x: &f32| (quantize(*x, 16) as f32 / 32_768.0).to_bits() == x.to_bits();
    if small_int && planes.iter().all(|p| p.iter().all(on_grid)) {
        SampleFormat::I16
    } else {
        SampleFormat::F32
    }
}

/// kura チャンクの固定の部分のバイト数（識別子の前まで）。
const KURA_FIXED: usize = 40;

fn malformed(why: &str) -> PcmError {
    PcmError::Failed(why.to_string())
}

/// WAV のバイト数（書く前に予算を確保するのに使う）。
pub fn wav_len(frames: u64, channels: u64, format: SampleFormat, decoder_len: usize) -> u64 {
    let size = match format {
        SampleFormat::I16 => 2,
        SampleFormat::F32 => 4,
    };
    let kura = (KURA_FIXED + decoder_len) as u64;
    let data = frames.saturating_mul(channels).saturating_mul(size);
    // RIFF・WAVE（12）、fmt（8 + 16）、kura（8 + 中身 + 詰め物）、data（8 + 中身）
    12u64
        .saturating_add(24)
        .saturating_add(8 + kura + (kura & 1))
        .saturating_add(8)
        .saturating_add(data)
}

/// WAV を書く（並びはモジュールの説明と `tests/vectors/pcm/wav.json`）。`planes` はチャンネルごとの同じ長さの標本。
pub fn write_wav(
    planes: &[&[f32]],
    rate: u32,
    format: SampleFormat,
    info: &KuraChunk<'_>,
) -> Result<Vec<u8>, PcmError> {
    let Some(first) = planes.first() else {
        return Err(malformed("チャンネルがありません"));
    };
    let frames = first.len();
    if planes.iter().any(|p| p.len() != frames) {
        return Err(malformed("チャンネルの長さがそろっていません"));
    }
    let decoder = info.decoder.as_bytes();
    if decoder.len() > MAX_DECODER_ID_BYTES {
        return Err(malformed("デコーダの識別子が長すぎます"));
    }
    let (tag, bits): (u16, u16) = match format {
        SampleFormat::I16 => (1, 16),
        SampleFormat::F32 => (3, 32),
    };
    let channels = u16::try_from(planes.len()).map_err(|_| malformed("チャンネルが多すぎます"))?;
    let block = channels
        .checked_mul(bits / 8)
        .ok_or_else(|| malformed("チャンネルが多すぎます"))?;
    let byte_rate = rate
        .checked_mul(u32::from(block))
        .ok_or_else(|| malformed("レートが大きすぎます"))?;
    let total = wav_len(frames as u64, u64::from(channels), format, decoder.len());
    let riff = u32::try_from(total - 8).map_err(|_| PcmError::TooLong { bytes: total })?;
    let data_len = frames * usize::from(block);
    let mut out = Vec::new();
    out.try_reserve_exact(usize::try_from(total).map_err(|_| PcmError::TooLong { bytes: total })?)
        .map_err(|_| PcmError::TooLong { bytes: total })?;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    let kura_len = KURA_FIXED + decoder.len();
    out.extend_from_slice(b"kura");
    out.extend_from_slice(&(kura_len as u32).to_le_bytes());
    out.extend_from_slice(&KURA_CHUNK_VERSION.to_le_bytes());
    out.extend_from_slice(&(decoder.len() as u16).to_le_bytes());
    out.extend_from_slice(&u32::from(info.loop_range.is_some()).to_le_bytes());
    out.extend_from_slice(&info.start.to_le_bytes());
    out.extend_from_slice(&info.total.to_le_bytes());
    let (loop_start, loop_end) = info.loop_range.unwrap_or((0, 0));
    out.extend_from_slice(&loop_start.to_le_bytes());
    out.extend_from_slice(&loop_end.to_le_bytes());
    out.extend_from_slice(decoder);
    if kura_len & 1 == 1 {
        out.push(0);
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for f in 0..frames {
        for plane in planes {
            let x = plane.get(f).copied().unwrap_or(0.0);
            match format {
                SampleFormat::I16 => out.extend_from_slice(&(quantize(x, 16) as i16).to_le_bytes()),
                SampleFormat::F32 => out.extend_from_slice(&x.to_le_bytes()),
            }
        }
    }
    Ok(out)
}

/// 覚えている読み取りの結果（1 件）。
struct Entry {
    root: PathBuf,
    hash: String,
    /// 読んだデコーダの識別子（`DecodeStamp::codec`）。
    decoder: String,
    /// 読んだときのコーデックの組（ツールの場所を変えて組が作り直されたら、デコーダの識別子で照らし直す）。
    codecs: Weak<CodecSet>,
    audio: Arc<Audio>,
    last_used: Instant,
    /// 覚えている値の分の予算（対話）。捨てると返る。
    _lease: Lease,
}

#[derive(Default)]
struct CacheState {
    entry: Option<Entry>,
    /// TTL を見張るスレッドが動いているか。
    reaper: bool,
}

/// 読んだ結果を 1 件だけ覚える置き場（アプリに 1 つ。ライブラリごとに分けて照らす）。
pub struct PcmCache {
    ttl: Duration,
    state: Mutex<CacheState>,
    /// 見張りのスレッドを起こす（覚えたもの・捨てたものが変わったとき）。
    wake: Condvar,
    /// 読むのを 1 つずつにする（前奏と本体を並んで要求されても、2 つ目は 1 つ目が読んだものを使う）。
    reading: Mutex<()>,
    decodes: AtomicU64,
}

impl PcmCache {
    /// `ttl` だけ使われなければ捨てる置き場を作る。
    pub fn new(ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            ttl,
            state: Mutex::new(CacheState::default()),
            wake: Condvar::new(),
            reading: Mutex::new(()),
            decodes: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 覚えているもの（ハッシュ, デコーダの識別子）。診断とテスト用。
    pub fn cached(&self) -> Option<(String, String)> {
        self.lock()
            .entry
            .as_ref()
            .map(|e| (e.hash.clone(), e.decoder.clone()))
    }

    /// 元を読んだ回数（覚えていたものを使った回数は数えない）。診断とテスト用。
    pub fn decodes(&self) -> u64 {
        self.decodes.load(Ordering::Relaxed)
    }

    /// 覚えているものを手放す（ライブラリを閉じるときなど）。
    pub fn clear(&self) {
        self.lock().entry = None;
        self.wake.notify_all();
    }

    /// 覚えているものを使う。`decoder` が None なら、同じコーデックの組で読んだものだけ（同じ組なら同じデコーダを選ぶ）。
    /// Some なら、そのデコーダで読んだものを（組が作り直されていても）使い、組を今のものにする。
    fn lookup(
        &self,
        root: &Path,
        hash: &str,
        codecs: &Arc<CodecSet>,
        decoder: Option<&str>,
    ) -> Option<(Arc<Audio>, String)> {
        let mut state = self.lock();
        let entry = state.entry.as_mut()?;
        if entry.root != root || entry.hash != hash {
            return None;
        }
        match decoder {
            None if !Weak::ptr_eq(&entry.codecs, &Arc::downgrade(codecs)) => return None,
            Some(id) if entry.decoder != id => return None,
            _ => {}
        }
        entry.codecs = Arc::downgrade(codecs);
        entry.last_used = Instant::now();
        Some((entry.audio.clone(), entry.decoder.clone()))
    }

    /// 覚える（前のものは捨てる）。TTL の見張りがいなければ起こす。
    fn store(self: &Arc<Self>, entry: Entry) {
        let mut state = self.lock();
        state.entry = Some(entry);
        if !state.reaper {
            let weak = Arc::downgrade(self);
            match std::thread::Builder::new()
                .name("kura-pcm-cache".into())
                .spawn(move || reap(&weak))
            {
                Ok(_) => state.reaper = true,
                Err(err) => {
                    tracing::warn!(%err, "試聴の PCM の見張りを起こせないので、覚えずにおきます");
                    state.entry = None;
                }
            }
        }
        self.wake.notify_all();
    }
}

/// TTL の見張り：覚えたものが `ttl` のあいだ使われなければ捨てる。覚えたものがなくなれば終わる（次に覚えたときにまた起こす）。
fn reap(weak: &Weak<PcmCache>) {
    loop {
        let Some(cache) = weak.upgrade() else {
            return;
        };
        let mut state = cache.lock();
        let wait = match &state.entry {
            Some(entry) => {
                let idle = entry.last_used.elapsed();
                if idle >= cache.ttl {
                    tracing::debug!(hash = %entry.hash, "使われないので試聴の PCM を手放しました");
                    state.entry = None;
                    continue;
                }
                cache.ttl - idle
            }
            None => {
                state.reaper = false;
                return;
            }
        };
        let _ = cache
            .wake
            .wait_timeout(state, wait)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// 予算から対話で確保する（待たない）。全体を超える・今空いていないは Busy（503）。
fn acquire(budget: &Arc<MemoryBudget>, bytes: u64) -> Result<Lease, PcmError> {
    match budget.try_acquire(bytes, Class::Interactive) {
        Ok(Some(lease)) => Ok(lease),
        Ok(None) => Err(PcmError::Busy(format!(
            "{bytes} バイトを確保できません。少し待ってからやり直してください"
        ))),
        Err(err) => Err(PcmError::Busy(err.to_string())),
    }
}

fn engine_failed(err: EngineError) -> PcmError {
    PcmError::Failed(err.to_string())
}

/// 元の音声（覚えていればそれ、なければ読んで覚える）と、読んだデコーダの識別子。
fn audio_of(
    cache: &Arc<PcmCache>,
    engine: &EngineServices,
    root: &Path,
    hash: &str,
    source: &PcmSource,
) -> Result<(Arc<Audio>, String), PcmError> {
    let codecs = engine.codecs();
    if let Some(hit) = cache.lookup(root, hash, &codecs, None) {
        return Ok(hit);
    }
    let _reading = cache.reading.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = cache.lookup(root, hash, &codecs, None) {
        return Ok(hit);
    }
    let runtime = engine
        .runtime()
        .map_err(|err| PcmError::Unavailable(err.to_string()))?;
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let len = std::fs::metadata(&source.path)
        .map_err(|_| PcmError::NotFound)?
        .len();
    let (shape, decoder) = runtime
        .preview(|_| codecs.shape_path(&source.path, source.format, &opts, &limits))
        .map_err(engine_failed)?;
    if let Some(hit) = cache.lookup(root, hash, &codecs, Some(decoder)) {
        return Ok(hit);
    }
    if !matches!(shape, Shape::Audio { .. }) {
        return Err(PcmError::NotFound);
    }
    // 覚えるのは 1 件だけ：読む前に前のものを手放す（メモリと予算を返す）
    cache.clear();
    let need = shape
        .value_bytes()
        .saturating_add(codecs.decode_file_work_bytes(source.format, &shape, len));
    let mut lease = acquire(runtime.budget(), need)?;
    let started = Instant::now();
    let (value, stamp) = runtime
        .preview(|_| codecs.decode_path(&source.path, source.format, &opts, &limits, &NoMonitor))
        .map_err(engine_failed)?;
    cache.decodes.fetch_add(1, Ordering::Relaxed);
    let Value::Audio(audio) = value else {
        return Err(PcmError::NotFound);
    };
    // 読み終えたら、覚えている値の分だけを持ち続ける
    let held = Value::Audio(audio.clone()).shape().value_bytes();
    if let Err(err) = lease.shrink_to(held.min(lease.bytes())) {
        tracing::warn!(%err, "試聴の PCM の予算を縮められませんでした");
    }
    tracing::debug!(
        hash,
        decoder = stamp.codec,
        frames = audio.frames(),
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "試聴の PCM の元を読みました"
    );
    cache.store(Entry {
        root: root.to_path_buf(),
        hash: hash.to_string(),
        decoder: stamp.codec.clone(),
        codecs: Arc::downgrade(&codecs),
        audio: audio.clone(),
        last_used: Instant::now(),
        _lease: lease,
    });
    Ok((audio, stamp.codec))
}

/// `/pcm/{hash}/{start}-{end}` の WAV を作る（ブロックする。呼び手が `spawn_blocking` で呼ぶ）。`root` はライブラリのフォルダ
/// （覚えた結果をライブラリごとに分ける）。
pub fn serve(
    cache: &Arc<PcmCache>,
    engine: &EngineServices,
    root: &Path,
    hash: &str,
    source: &PcmSource,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, PcmError> {
    let (audio, decoder) = audio_of(cache, engine, root, hash, source)?;
    let total = audio.frames() as u64;
    if start >= end || end > total {
        return Err(PcmError::OutOfRange { start, end, total });
    }
    let channels = audio.channels() as u64;
    let floats = (end - start).saturating_mul(channels).saturating_mul(4);
    if floats > MAX_PCM_BYTES {
        return Err(PcmError::TooLong { bytes: floats });
    }
    let (from, to) = (
        usize::try_from(start).map_err(|_| PcmError::TooLong { bytes: floats })?,
        usize::try_from(end).map_err(|_| PcmError::TooLong { bytes: floats })?,
    );
    let planes: Vec<&[f32]> = audio
        .planes()
        .iter()
        .map(|p| p.get(from..to).unwrap_or(&[]))
        .collect();
    let format = sample_format(audio.meta.source.as_ref(), &planes);
    // 書く間の分も予算で数える（応答を渡したら返す）
    let runtime = engine
        .runtime()
        .map_err(|err| PcmError::Unavailable(err.to_string()))?;
    let _writing = acquire(
        runtime.budget(),
        wav_len(end - start, channels, format, decoder.len()),
    )?;
    // ループ点は書き出しと同じ判定（start < end ≤ 長さ）を通ったものだけ（`audio.loop` の解析と同じ）
    let loop_range = audio
        .meta
        .loop_range
        .filter(|l| l.start < l.end && l.end <= total)
        .map(|l| (l.start, l.end));
    write_wav(
        &planes,
        audio.rate(),
        format,
        &KuraChunk {
            start,
            total,
            loop_range,
            decoder: &decoder,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use kura_engine::codec::CodecSet;
    use kura_engine::value::{Audio, AudioMeta, LoopRange};
    use kura_engine::{NoMonitor, Value};
    use serde_json::{Value as Json, json};

    use super::*;
    use crate::edit::{EngineFactory, RuntimeConfig};
    use crate::library::{Layout, parse_hex32};

    /// 予算を決められる偽のファクトリ（固定の段だけの組。Linux でも回る）。
    struct Factory {
        budget: u64,
    }

    impl EngineFactory for Factory {
        fn runtime_config(&self) -> RuntimeConfig {
            RuntimeConfig {
                cores: Some(2),
                budget: Some(self.budget),
                ..RuntimeConfig::default()
            }
        }

        fn codecs(&self, _work_dir: &Path) -> CodecSet {
            CodecSet::pinned()
        }
    }

    fn engine(budget: u64) -> EngineServices {
        EngineServices::new(Arc::new(Factory { budget }), PathBuf::from("/nowhere"))
    }

    fn hash(b: u8) -> String {
        format!("{b:02x}").repeat(32)
    }

    /// ライブラリの files/ に音声を WAV で置く（`bits` 16 の整数か、0 なら浮動小数）。
    fn place(layout: &Layout, b: u8, audio: Audio, bits: u32) -> (String, PcmSource) {
        let opts = if bits == 0 {
            json!({ "float": true, "bits": 32 })
        } else {
            json!({ "bits": bits })
        };
        let (bytes, _) = CodecSet::pinned()
            .encode_bytes(&Value::Audio(Arc::new(audio)), "wav", &opts, &NoMonitor)
            .unwrap();
        let h = hash(b);
        let path = layout.content_path(&parse_hex32(&h).unwrap(), "wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        (
            h,
            PcmSource {
                path,
                format: "wav",
            },
        )
    }

    /// 16 ビットの格子の上の 2 チャンネルの音声（フレーム i の値は ±(i % 1000) / 32768）。
    fn ramp(frames: usize, rate: u32, looped: Option<(u64, u64)>) -> Audio {
        let plane = |sign: f32| -> Arc<Vec<f32>> {
            Arc::new(
                (0..frames)
                    .map(|i| sign * (i % 1000) as f32 / 32_768.0)
                    .collect(),
            )
        };
        Audio::new(
            rate,
            vec![plane(1.0), plane(-1.0)],
            AudioMeta {
                loop_range: looped.map(|(start, end)| LoopRange { start, end }),
                ..AudioMeta::default()
            },
        )
        .unwrap()
    }

    /// 応答の WAV を読む（テスト用。形式・チャンネル・レート・kura チャンク・data）。
    struct Parsed {
        tag: u16,
        channels: u16,
        rate: u32,
        start: u64,
        total: u64,
        looped: Option<(u64, u64)>,
        decoder: String,
        data: Vec<u8>,
    }

    fn parse(b: &[u8]) -> Parsed {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32_at(4) as usize, b.len() - 8);
        assert_eq!(&b[12..16], b"fmt ");
        assert_eq!(&b[36..40], b"kura");
        let k = 44;
        assert_eq!(u16_at(k), KURA_CHUNK_VERSION);
        let len = usize::from(u16_at(k + 2));
        let flags = u32_at(k + 4);
        let kura_len = u32_at(40) as usize;
        let data_at = 44 + kura_len + (kura_len & 1);
        assert_eq!(&b[data_at..data_at + 4], b"data");
        Parsed {
            tag: u16_at(20),
            channels: u16_at(22),
            rate: u32_at(24),
            start: u64_at(k + 8),
            total: u64_at(k + 16),
            looped: (flags & 1 == 1).then(|| (u64_at(k + 24), u64_at(k + 32))),
            decoder: String::from_utf8(b[k + 40..k + 40 + len].to_vec()).unwrap(),
            data: b[data_at + 8..].to_vec(),
        }
    }

    /// 書き手は TS と共有のベクタ（tests/vectors/pcm/wav.json）とバイト列まで一致する。
    #[test]
    fn wav_writer_matches_the_shared_vectors() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/pcm/wav.json");
        let frozen: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cases = frozen["cases"].as_array().unwrap();
        assert!(cases.len() >= 6);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let planes: Vec<Vec<f32>> = case["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    p.as_array()
                        .unwrap()
                        .iter()
                        .map(|x| f32::from_bits(u32::try_from(x.as_u64().unwrap()).unwrap()))
                        .collect()
                })
                .collect();
            let slices: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
            let format = match case["format"].as_str().unwrap() {
                "i16" => SampleFormat::I16,
                _ => SampleFormat::F32,
            };
            let looped = case["loop"]
                .as_array()
                .map(|l| (l[0].as_u64().unwrap(), l[1].as_u64().unwrap()));
            let bytes = write_wav(
                &slices,
                u32::try_from(case["rate"].as_u64().unwrap()).unwrap(),
                format,
                &KuraChunk {
                    start: case["start"].as_u64().unwrap(),
                    total: case["total"].as_u64().unwrap(),
                    loop_range: looped,
                    decoder: case["decoder"].as_str().unwrap(),
                },
            )
            .unwrap();
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(hex, case["hex"].as_str().unwrap(), "{name}");
        }
    }

    /// 書く前に断るもの：チャンネル 0・長さがそろわない・長すぎるデコーダの識別子。
    #[test]
    fn wav_writer_refuses_malformed_input() {
        let a = [0.0f32; 4];
        let b = [0.0f32; 3];
        let info = KuraChunk {
            start: 0,
            total: 4,
            loop_range: None,
            decoder: "wav-kura-dec/1",
        };
        assert!(write_wav(&[], 44_100, SampleFormat::F32, &info).is_err());
        assert!(write_wav(&[&a, &b], 44_100, SampleFormat::F32, &info).is_err());
        let long = "x".repeat(MAX_DECODER_ID_BYTES + 1);
        let info = KuraChunk {
            decoder: &long,
            ..info
        };
        assert!(write_wav(&[&a], 44_100, SampleFormat::F32, &info).is_err());
    }

    /// 16 ビットは、元が 16 ビット以下の整数で、どの標本も 16 ビットで書いて読むと同じ値に戻るときだけ。
    #[test]
    fn sample_format_is_i16_only_when_the_values_stay_the_same() {
        let int16 = AudioSource {
            bits: 16,
            float: false,
            rate: 44_100,
        };
        let on_grid = [0.0f32, 0.5, -1.0, 32_767.0 / 32_768.0, 3.0 / 32_768.0];
        assert_eq!(sample_format(Some(&int16), &[&on_grid]), SampleFormat::I16);
        let int8 = AudioSource { bits: 8, ..int16 };
        assert_eq!(sample_format(Some(&int8), &[&on_grid]), SampleFormat::I16);
        // 非可逆の形式（16 ビットと名乗るが格子の上にない）・−0・NaN・1 は浮動小数
        for odd in [0.1f32, -0.0, f32::NAN, 1.0, 1.5] {
            let plane = [0.0f32, odd];
            assert_eq!(
                sample_format(Some(&int16), &[&on_grid, &plane]),
                SampleFormat::F32,
                "{odd}"
            );
        }
        let int24 = AudioSource { bits: 24, ..int16 };
        let float = AudioSource {
            float: true,
            bits: 32,
            ..int16
        };
        for source in [Some(&int24), Some(&float), None] {
            assert_eq!(sample_format(source, &[&on_grid]), SampleFormat::F32);
        }
    }

    /// 区間を WAV にし、同じ読み取りのループ点・フレーム数・デコーダの識別子を kura チャンクに入れる。値は元のまま。
    #[test]
    fn serves_a_range_with_the_loop_and_decoder_of_the_same_read() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let cache = PcmCache::new(CACHE_TTL);
        let engine = engine(64 << 20);
        let (h, source) = place(&layout, 1, ramp(48_000, 48_000, Some((1_000, 40_000))), 16);
        let wav = serve(&cache, &engine, dir.path(), &h, &source, 100, 1_100).unwrap();
        let p = parse(&wav);
        assert_eq!(
            (p.tag, p.channels, p.rate),
            (1, 2, 48_000),
            "16 ビットの元は 16 ビット"
        );
        assert_eq!((p.start, p.total), (100, 48_000));
        assert_eq!(p.looped, Some((1_000, 40_000)));
        assert_eq!(p.decoder, "wav-kura-dec/1");
        assert_eq!(p.data.len(), 1_000 * 2 * 2);
        // フレーム 100 の左は 100、右は −100
        assert_eq!(i16::from_le_bytes([p.data[0], p.data[1]]), 100);
        assert_eq!(i16::from_le_bytes([p.data[2], p.data[3]]), -100);
        // 浮動小数の元は浮動小数（ビットそのまま）。ループ点がなければ印なし
        let (h2, source2) = place(&layout, 2, ramp(1_000, 22_050, None), 0);
        let p = parse(&serve(&cache, &engine, dir.path(), &h2, &source2, 0, 1_000).unwrap());
        assert_eq!((p.tag, p.rate, p.looped), (3, 22_050, None));
        assert_eq!(
            f32::from_le_bytes(p.data[8 * 3..8 * 3 + 4].try_into().unwrap()),
            3.0 / 32_768.0
        );
    }

    /// 範囲が音声の外・浮動小数で 64 MiB を超える長さは 416（読んだものは覚えたまま）。
    #[test]
    fn out_of_range_and_too_long_are_416() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let cache = PcmCache::new(CACHE_TTL);
        let small = engine(64 << 20);
        let (h, source) = place(&layout, 1, ramp(1_000, 8_000, None), 16);
        for (start, end) in [(0, 1_001), (1_000, 1_001), (5_000, 6_000)] {
            let err = serve(&cache, &small, dir.path(), &h, &source, start, end).unwrap_err();
            assert_eq!(err.status(), 416, "{start}-{end}: {err}");
            assert!(
                matches!(err, PcmError::OutOfRange { total: 1_000, .. }),
                "{err}"
            );
        }
        assert!(serve(&cache, &small, dir.path(), &h, &source, 999, 1_000).is_ok());
        assert_eq!(cache.decodes(), 1);
        // 浮動小数で 64 MiB を超える長さ（2 チャンネル × 4 バイト × 8 Mi フレーム + 1）
        let frames = 8 * 1024 * 1024;
        let (h2, source2) = place(&layout, 2, ramp(frames + 1, 8_000, None), 16);
        let big = engine(512 << 20);
        let err = serve(
            &cache,
            &big,
            dir.path(),
            &h2,
            &source2,
            0,
            frames as u64 + 1,
        )
        .unwrap_err();
        assert_eq!(err.status(), 416, "{err}");
        assert!(matches!(err, PcmError::TooLong { .. }));
        assert!(serve(&cache, &big, dir.path(), &h2, &source2, 0, frames as u64).is_ok());
    }

    /// 同じ元は 1 回だけ読む（前奏と本体を続けて取っても）。別の元を読むと前のものを捨てる（覚えるのは 1 件だけ）。
    #[test]
    fn the_same_source_is_read_once_and_a_new_one_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let cache = PcmCache::new(CACHE_TTL);
        let engine = engine(64 << 20);
        let (h1, s1) = place(&layout, 1, ramp(4_000, 8_000, Some((10, 20))), 16);
        let (h2, s2) = place(&layout, 2, ramp(4_000, 8_000, None), 16);
        assert_eq!(cache.cached(), None);
        serve(&cache, &engine, dir.path(), &h1, &s1, 0, 10).unwrap();
        serve(&cache, &engine, dir.path(), &h1, &s1, 10, 20).unwrap();
        assert_eq!(cache.decodes(), 1);
        assert_eq!(cache.cached(), Some((h1.clone(), "wav-kura-dec/1".into())));
        let used = engine.runtime().unwrap().budget().used().unwrap();
        assert_eq!(used, 4_000 * 2 * 4, "覚えている間は値の分の予算を持つ");
        serve(&cache, &engine, dir.path(), &h2, &s2, 0, 10).unwrap();
        assert_eq!(cache.decodes(), 2);
        assert_eq!(cache.cached(), Some((h2, "wav-kura-dec/1".into())));
        assert_eq!(
            engine.runtime().unwrap().budget().used().unwrap(),
            used,
            "前のものは返した"
        );
        serve(&cache, &engine, dir.path(), &h1, &s1, 0, 10).unwrap();
        assert_eq!(cache.decodes(), 3);
        // 別のライブラリの同じハッシュは別物
        let other = tempfile::tempdir().unwrap();
        let other_layout = Layout::new(other.path());
        let (_, s3) = place(&other_layout, 1, ramp(2_000, 8_000, None), 16);
        let p = parse(&serve(&cache, &engine, other.path(), &h1, &s3, 0, 10).unwrap());
        assert_eq!(p.total, 2_000);
        assert_eq!(cache.decodes(), 4);
    }

    /// 予算（対話。待たない）が足りなければ 503。
    #[test]
    fn a_short_budget_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let cache = PcmCache::new(CACHE_TTL);
        let (h, source) = place(&layout, 1, ramp(48_000, 48_000, None), 16);
        // 値だけで 384,000 バイト要る
        let err = serve(&cache, &engine(100_000), dir.path(), &h, &source, 0, 10).unwrap_err();
        assert_eq!(err.status(), 503, "{err}");
        assert!(matches!(err, PcmError::Busy(_)));
        assert_eq!(cache.decodes(), 0);
        // ほかが使っていて空いていない
        let engine = engine(1 << 20);
        let budget = engine.runtime().unwrap().budget().clone();
        let held = budget
            .try_acquire(900_000, kura_engine::budget::Class::Interactive)
            .unwrap()
            .unwrap();
        let err = serve(&cache, &engine, dir.path(), &h, &source, 0, 10).unwrap_err();
        assert_eq!(err.status(), 503);
        drop(held);
        assert!(serve(&cache, &engine, dir.path(), &h, &source, 0, 10).is_ok());
    }

    /// 使わずに TTL がたつと、覚えていたものと予算を手放す。
    #[test]
    fn the_cache_forgets_after_its_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let cache = PcmCache::new(Duration::from_millis(50));
        let engine = engine(64 << 20);
        let (h, source) = place(&layout, 1, ramp(1_000, 8_000, None), 16);
        serve(&cache, &engine, dir.path(), &h, &source, 0, 10).unwrap();
        assert!(cache.cached().is_some());
        let started = std::time::Instant::now();
        while cache.cached().is_some() {
            assert!(started.elapsed() < Duration::from_secs(5), "捨てられません");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(engine.runtime().unwrap().budget().used().unwrap(), 0);
        serve(&cache, &engine, dir.path(), &h, &source, 0, 10).unwrap();
        assert_eq!(cache.decodes(), 2);
    }

    /// 測る（`cargo test --release -p kura-core measure_pcm -- --ignored --nocapture`）：3 分の 44.1kHz ステレオを、
    /// WAV（16 ビット）・FLAC・Ogg Vorbis で置き、最初の要求（読む＋書く）と 2 回目（覚えたもの）の時間。
    #[test]
    #[ignore = "時間を測るだけ（release で回す）"]
    fn measure_pcm() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let engine = engine(2 << 30);
        let rate = 44_100u32;
        let frames = 180 * rate as usize;
        let tone = |k: f32| -> Arc<Vec<f32>> {
            Arc::new(
                (0..frames)
                    .map(|i| {
                        let x = (i as f32 * k * 0.01).sin() * 0.5;
                        (x * 32_768.0).round() / 32_768.0
                    })
                    .collect(),
            )
        };
        let audio = Audio::new(
            rate,
            vec![tone(1.0), tone(1.5)],
            AudioMeta {
                loop_range: Some(LoopRange {
                    start: 44_100,
                    end: frames as u64,
                }),
                ..AudioMeta::default()
            },
        )
        .unwrap();
        let value = Value::Audio(Arc::new(audio));
        for (b, format, opts) in [
            (1u8, "wav", json!({ "bits": 16 })),
            (2, "flac", json!({})),
            (3, "ogg", json!({})),
        ] {
            let (bytes, _) = CodecSet::pinned()
                .encode_bytes(&value, format, &opts, &NoMonitor)
                .unwrap();
            let h = hash(b);
            let path = layout.content_path(&parse_hex32(&h).unwrap(), format);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let source = PcmSource {
                path,
                format: match format {
                    "ogg" => "ogg",
                    "flac" => "flac",
                    _ => "wav",
                },
            };
            let cache = PcmCache::new(CACHE_TTL);
            let total = frames as u64;
            // 試聴の本体（ループの区間）：前奏のあとの全体（f32 で 2 × 4 × 7,938,000 = 63.5 MB ≤ 64 MiB）
            let started = std::time::Instant::now();
            let first = serve(&cache, &engine, dir.path(), &h, &source, 44_100, total);
            let first_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = std::time::Instant::now();
            let intro = serve(&cache, &engine, dir.path(), &h, &source, 0, 44_100).unwrap();
            let second_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = std::time::Instant::now();
            let seam = serve(&cache, &engine, dir.path(), &h, &source, total - 176_400, total).unwrap();
            let seam_ms = started.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "{format}: ファイル {} バイト、最初（読む＋本体 {} バイト）{first_ms:.1} ms、2 回目（前奏 {} バイト）{second_ms:.1} ms、継ぎ目 4 秒 {} バイト {seam_ms:.1} ms、デコーダ {:?}",
                bytes.len(),
                first.as_ref().map(Vec::len).unwrap_or(0),
                intro.len(),
                seam.len(),
                cache.cached().map(|c| c.1),
            );
        }
    }
}
