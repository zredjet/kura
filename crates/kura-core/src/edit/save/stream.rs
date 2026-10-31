//! 保存の流れ（M13 段 3。docs/editing.md §14）：音声を値に組まずに、遅延の入力（アンカー・WAV/AIFF の源・作業用の PCM）から
//! 計画の出力を窓ごとに描き、エンコーダが流して書く（`Engine::output_stream` → `CodecSet::encode_stream`）。
//!
//! 1. 入力（[`read_input`]）：アンカーがあれば `anchor::verify_stream` で照らしてから源として開く（使えなければ元を記録の
//!    デコーダで読み直す）。元から読むときは、WAV・AIFF で `open_audio_source` が開ければ源にして照らしの回でダイジェストを
//!    取り、ほかは作業用の PCM（`WorkPcmStore::ensure`。段ごとに形を読んで見積もり、形を読めた段で流して読む）にして、封の
//!    記録を元の読み取りの記録にする。OS のデコーダを通りうるなら `decoding` の印を立てて読む。
//! 2. 計画：遅延の値の形で compile し、流して書く見積もり（`EstimateSpec::stream`・`Source::Lazy`）で確保する。
//! 3. アンカーの昇格：再現しない読み（Platform・External）なら、作業用のファイルを staging に写し（`reflink_or_copy`）、
//!    `verify_stream` で照らしてアンカーにする（値を書き直さない。封をしたファイルは `anchor::write` と同じバイト列）。
//! 4. 描いて書く：一括のプールで `output_stream` を `encode_stream` に流し、.part を `sync_all`。
//! 5. R2：.part を 1 回読んで、生の BLAKE3（台帳）と `kura-file-v1`（書き出しの記録）を取る。
//!
//! 進捗はパスの重み（入力 1（作業用のファイル・源の照らしの回、アンカーなら照らしの回 0.5）・書く回 × 1・昇格の照らし 0.5・R2 の
//! 読み直し 0.25）で数え、どの回も受け手を包んでフレーム（R2 はバイト）を数えて今の回の割合として知らせる。割合は 0.05〜0.9
//! （コミットの「保存しています」0.9 まで）で、送った割合より下げない（M13 段 3 の検証の直し）。
//! 一時停止で止めるときは作業用のファイルを残す印を立て、再開は初めから（`ensure` が残っているものを使い回す）。

use std::collections::BTreeSet;

use kura_engine::audio::digest::PcmTreeDigest;
use kura_engine::audio::source::Opener;
use kura_engine::audio::stream::{
    AudioProducer, AudioSink, STREAM_CHUNKS, StreamEnd, StreamInfo, stream_value,
};
use kura_engine::budget::Class;
use kura_engine::codec::stream::{DecodeInfo, DecodeSink};
use kura_engine::codec::{CodecSet, Decoder, FileDigest, PickedStage, ReadSeek, StreamEncoded};
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::region::Spans;
use kura_engine::value::{AUDIO_CHUNK, Chunk};

use super::*;
use crate::edit::EngineRuntime;
use crate::edit::work_pcm::{WorkHandle, WorkKey, ensure_estimate};

/// 保存で源の LRU に許す量の上限（エディタと同じ 64 MiB。音声の全体がそれより小さければ全体）。
const LRU_MAX: u64 = 64 << 20;

/// 流して書くか：音声で、出力の形式の書き手が流して書ける（代表の形 48 kHz ステレオで選んだ書き手）。
pub(super) fn applies(codecs: &CodecSet, kind: MediaKind, out_format: &str) -> bool {
    kind == MediaKind::Audio
        && codecs.encode_streams(
            out_format,
            &Shape::Audio {
                rate: 48_000,
                channels: 2,
                frames: Dim::Exact(48_000),
            },
        )
}

/// 源の LRU の上限：min(64 MiB, 音声の全体（チャンクに切り上げ）)。並列の仕事 `tasks` 個が 1 チャンクずつ持てる量は下回らない。
pub(super) fn lru_bytes(shape: &Shape, tasks: usize) -> u64 {
    let Shape::Audio {
        channels, frames, ..
    } = shape
    else {
        return LRU_MAX;
    };
    let chunk = AUDIO_CHUNK as u64 * u64::from(*channels) * 4;
    let whole = frames
        .max()
        .div_ceil(AUDIO_CHUNK as u64)
        .saturating_mul(chunk);
    LRU_MAX
        .min(whole)
        .max((tasks.max(1) as u64).saturating_mul(chunk))
}

/// ファイル `path` を開く閉包（源の読み口）。
pub(super) fn path_opener(path: &Path) -> Opener {
    let path = path.to_path_buf();
    Arc::new(move || {
        let f = File::open(&path)?;
        Ok(Box::new(BufReader::new(f)) as Box<dyn ReadSeek + Send>)
    })
}

/// 流して読む入力（遅延の値と、その読み取りの記録）。
pub(super) struct StreamInput {
    pub value: Value,
    pub decoded: DecodeStamp,
    pub anchor: AnchorUse,
    /// 源の LRU の上限（見積もりの `Source::Lazy`）。
    pub resident: u64,
}

/// パスの重みで進み具合を数えるもの（段の間の文言と割合）。
pub(super) struct Passes<'a> {
    ctx: &'a JobContext,
    total: f64,
    done: f64,
    /// 送るか（保存・版に戻すは真。GC の照合は偽：GC は自分の進捗を送る。M13 段 3 の検証の直し）。
    send: bool,
    /// これまでに出した割合（f64 のビット。負でない f64 はビットの順が値の順。下げない）。
    last: std::sync::atomic::AtomicU64,
    /// テストの割り込み（流れの途中の段）。
    #[cfg(test)]
    engine: &'a EngineServices,
}

impl<'a> Passes<'a> {
    /// 重みの和 `total` で始める（`send` が真なら `cx` のジョブに知らせる）。
    pub(super) fn new(cx: &StreamCx<'a>, total: f64, send: bool) -> Self {
        Self {
            ctx: cx.ctx,
            total: total.max(1e-9),
            done: 0.0,
            send,
            last: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            engine: cx.engine,
        }
    }

    /// テストの割り込み（流れの途中の段 `stage`）。
    #[cfg(test)]
    fn at(&self, stage: super::Stage) {
        self.engine.at(stage);
    }

    /// 今の回（重み `weight`）の進み `frac`（0〜1）の割合（0.05〜0.9。コミットの 0.9 を超えない）。これまでに出した割合より
    /// 下げない（覚える）。
    pub(super) fn value(&self, weight: f64, frac: f64) -> f64 {
        let p = (self.done + weight * frac.clamp(0.0, 1.0)) / self.total;
        let v = 0.05 + 0.85 * p.clamp(0.0, 1.0);
        let prev = self
            .last
            .fetch_max(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
        v.max(f64::from_bits(prev))
    }

    /// 今の回（重み `weight`）の進み `frac`（0〜1）を知らせる。
    pub(super) fn report(&self, weight: f64, frac: f64, message: &str) {
        if !self.send {
            return;
        }
        self.ctx.progress(self.value(weight, frac), message);
    }

    /// 回（重み `weight`）を終えた。
    pub(super) fn finish(&mut self, weight: f64) {
        self.done += weight;
    }
}

/// 生産者を包み、流れたフレームを数えて進み具合を知らせる（`passes` 回の書く回をまとめて 1 つの重み `weight` で）。
struct Counting<'p, 'a> {
    inner: &'p mut dyn AudioProducer,
    passes: &'p Passes<'a>,
    weight: f64,
    rounds: u32,
    done: u64,
}

struct CountingSink<'s, 'p, 'a> {
    sink: &'s mut dyn AudioSink,
    passes: &'p Passes<'a>,
    weight: f64,
    total: f64,
    done: &'s mut u64,
    /// 1 回の長さ（テストの割り込みに渡す）。
    #[cfg(test)]
    frames: u64,
}

impl AudioSink for CountingSink<'_, '_, '_> {
    fn begin(&mut self, info: &StreamInfo) -> std::result::Result<(), EngineError> {
        self.sink.begin(info)
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> std::result::Result<(), EngineError> {
        self.sink.feed(start, planes, mon)?;
        *self.done += planes.first().map_or(0, |p| p.len()) as u64;
        self.passes
            .report(self.weight, *self.done as f64 / self.total, "書いています");
        #[cfg(test)]
        self.passes.at(super::Stage::Window {
            fed: *self.done,
            frames: self.frames,
        });
        Ok(())
    }

    fn finish(&mut self) -> std::result::Result<(), EngineError> {
        self.sink.finish()
    }
}

impl AudioProducer for Counting<'_, '_> {
    fn info(&self) -> &StreamInfo {
        self.inner.info()
    }

    fn produce(
        &mut self,
        spans: Option<&Spans>,
        sink: &mut dyn AudioSink,
        mon: &dyn Monitor,
    ) -> std::result::Result<(), EngineError> {
        let total = (self.inner.info().frames as f64 * f64::from(self.rounds.max(1))).max(1.0);
        let mut wrap = CountingSink {
            sink,
            passes: self.passes,
            weight: self.weight,
            total,
            done: &mut self.done,
            #[cfg(test)]
            frames: self.inner.info().frames,
        };
        self.inner.produce(spans, &mut wrap, mon)
    }

    fn digest(&mut self) -> std::result::Result<Option<Digest>, EngineError> {
        self.inner.digest()
    }
}

/// 今の回の進み具合の知らせ先（`passes` の重み `weight` の回、文言 `message`）。
#[derive(Clone, Copy)]
pub(super) struct Pass<'p, 'a> {
    pub passes: &'p Passes<'a>,
    pub weight: f64,
    pub message: &'p str,
}

impl Pass<'_, '_> {
    /// `done` / `total` まで進んだ。
    fn report(&self, done: u64, total: u64) {
        self.passes
            .report(self.weight, done as f64 / total.max(1) as f64, self.message);
    }
}

/// 流れの窓を値のダイジェストに足す受け手（照らしの回。流れたフレームを数えて今の回の割合として知らせる）。
struct DigestOnly<'p, 'a> {
    digest: Option<PcmTreeDigest>,
    frames: u64,
    pass: Option<Pass<'p, 'a>>,
}

impl AudioSink for DigestOnly<'_, '_> {
    fn begin(&mut self, info: &StreamInfo) -> std::result::Result<(), EngineError> {
        self.digest = Some(PcmTreeDigest::new(info.rate, info.channels, info.frames)?);
        self.frames = info.frames;
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> std::result::Result<(), EngineError> {
        match &mut self.digest {
            Some(d) => d.push_window(start, planes, mon)?,
            None => return Err(EngineError::Internal("照らしの回が始まっていません".into())),
        }
        let n = planes.first().map_or(0, |p| p.len()) as u64;
        if let Some(pass) = &self.pass {
            pass.report(start.saturating_add(n), self.frames);
        }
        Ok(())
    }

    fn finish(&mut self) -> std::result::Result<(), EngineError> {
        Ok(())
    }
}

/// 遅延の値 `value` を 1 回流して値のダイジェストを取る（照らしの回。進み具合を `pass` に知らせる）。
fn digest_value(
    value: &Value,
    mon: &dyn Monitor,
    pass: Option<Pass<'_, '_>>,
) -> std::result::Result<Digest, EngineError> {
    let Value::Audio(a) = value else {
        return Err(EngineError::Internal("音声ではありません".into()));
    };
    let mut sink = DigestOnly {
        digest: None,
        frames: 0,
        pass,
    };
    stream_value(a, None, &mut sink, mon)?;
    sink.digest
        .ok_or_else(|| EngineError::Internal("照らしの回を流していません".into()))?
        .finish(&a.meta)
}

/// 照らしの回（`anchor::verify_stream`。窓ごとに `progress` を呼ぶ）の進み具合を今の回の割合として知らせる中止の口。
struct PassMonitor<'p, 'a> {
    inner: &'p dyn Monitor,
    pass: Pass<'p, 'a>,
}

impl Monitor for PassMonitor<'_, '_> {
    fn canceled(&self) -> bool {
        self.inner.canceled()
    }

    fn progress(&self, done: u64, total: u64) {
        self.pass.report(done, total);
    }
}

/// 作業用の PCM を作る回の受け手を包み、読んだフレームを数えて今の回の割合として知らせる（長さが上限しか分からない形は
/// 見込みの長さで）。
struct CountingDecode<'s, 'p, 'a> {
    inner: &'s mut dyn DecodeSink,
    pass: Pass<'p, 'a>,
    total: u64,
}

impl DecodeSink for CountingDecode<'_, '_, '_> {
    fn begin(&mut self, info: &DecodeInfo) -> std::result::Result<(), EngineError> {
        self.total = info.expect;
        self.inner.begin(info)
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> std::result::Result<(), EngineError> {
        self.inner.feed(start, planes, mon)?;
        let n = planes.first().map_or(0, |p| p.len()) as u64;
        self.pass.report(start.saturating_add(n), self.total);
        Ok(())
    }

    fn end(&mut self, end: &StreamEnd) -> std::result::Result<(), EngineError> {
        self.inner.end(end)
    }
}

/// 源として開いた WAV・AIFF の値 `value` を照らしの回で流してダイジェストを取り、決めた LRU `resident` で源を開き直した値
/// （LRU が空）と返す（開き直しは見出しを読むだけ）。照らしの回で埋めた LRU（最大 `resident`）を、計画の確保を待つ間に予算の
/// 外で持たない（M13 段 3 の検証の直し）。
#[allow(clippy::too_many_arguments)]
fn verify_source(
    codecs: &CodecSet,
    opener: &Opener,
    format: &str,
    opts: &DecodeOptions,
    limits: &Limits,
    resident: u64,
    value: Value,
    mon: &dyn Monitor,
    pass: Option<Pass<'_, '_>>,
) -> std::result::Result<(Value, Digest), EngineError> {
    let digest = digest_value(&value, mon, pass)?;
    drop(value);
    let reopened = codecs
        .open_audio_source(opener, format, opts, limits, resident)?
        .ok_or_else(|| EngineError::Internal("元を開き直せません".into()))?;
    Ok((reopened.value, digest))
}

/// 照らしの回の作業領域（窓 1 つ ＋ ダイジェスト ＋ 源の LRU）。
pub(super) fn verify_bytes(shape: &Shape, tasks: u64, resident: u64) -> u64 {
    let Shape::Audio {
        channels, frames, ..
    } = shape
    else {
        return resident;
    };
    let window = frames
        .max()
        .min((STREAM_CHUNKS * AUDIO_CHUNK) as u64)
        .saturating_mul(u64::from(*channels))
        .saturating_mul(4);
    window
        .saturating_add(PcmTreeDigest::work_bytes(usize::from(*channels), tasks))
        .saturating_add(resident)
}

/// 流して読むのに使うもの（1 つのジョブの間は同じ）。
pub(super) struct StreamCx<'a> {
    pub ctx: &'a JobContext,
    pub runtime: &'a EngineRuntime,
    pub codecs: &'a CodecSet,
    pub engine: &'a EngineServices,
    pub layout: &'a Layout,
    pub original: &'a Path,
    pub original_hash: [u8; 32],
    pub format: &'a str,
    pub opts: &'a DecodeOptions,
    pub limits: &'a Limits,
    pub mon: &'a JobMonitor<'a>,
}

impl StreamCx<'_> {
    /// 一括の予算を確保する（待つことがある。中止で止める）。
    fn acquire(&self, bytes: u64) -> Result<Flow<kura_engine::budget::Lease>> {
        let r = self.runtime.budget().acquire(bytes, Class::Bulk, self.mon);
        engine_flow(self.ctx, "保存のメモリを確保できません", r)
    }

    /// 作業用の PCM の結果：中止（止める指示があるとき）は止め方に。
    fn work_flow<T>(&self, r: Result<T>) -> Result<Flow<T>> {
        match r {
            Ok(v) => Ok(Flow::Go(v)),
            Err(CoreError::Canceled) => match stop_end(self.ctx) {
                Some(end) => Ok(Flow::Stop(end)),
                None => Err(CoreError::Canceled),
            },
            Err(e) => Err(e),
        }
    }

    fn acx(&self) -> AnchorContext {
        AnchorContext {
            source: Some(Digest(self.original_hash)),
            format: Some(self.format.to_string()),
            options: self.opts.clone(),
        }
    }
}

/// 元をデコーダ `decoder`（None は段の順）で作業用の PCM にする（あれば使い回す）。段ごとに形を読んで見積もりを確保し直し、
/// 形を読めた段で流して読む（その段が `Unsupported` なら次の段の形を読み直す。M13 段 2 の検証の直しと同じ考え方）。読む
/// 進み具合を `pass` に知らせる。
fn ensure_work(
    cx: &StreamCx<'_>,
    decoder: Option<&Arc<dyn Decoder>>,
    pass: Pass<'_, '_>,
) -> Result<Flow<WorkHandle>> {
    let store = cx.engine.work_pcm().clone();
    let key = WorkKey {
        source: cx.original_hash,
        format: cx.format.to_string(),
        options: cx.opts.clone(),
        decoder: decoder.map(|d| d.id().to_string()),
    };
    if let Some(h) = store.find(&key) {
        return Ok(Flow::Go(h));
    }
    let len = file_len(cx.original);
    let tasks = cx.runtime.bulk_threads() as u64;
    let (codecs, path, format, opts, limits, mon) = (
        cx.codecs,
        cx.original,
        cx.format,
        cx.opts,
        cx.limits,
        cx.mon,
    );
    let mut after: Option<&'static str> = None;
    loop {
        // 形を読む（段ごとに、その段で形を読む分を確保し直す）
        let mut lease = None;
        let mut stopped = None;
        let read = match decoder {
            Some(d) => {
                let held =
                    kura_engine::codec::contain(d.id(), kura_engine::codec::Stage::Decode, || {
                        d.held_bytes_file(path, format, limits)
                    });
                match held {
                    Ok(held) => {
                        let stage = PickedStage {
                            id: d.id(),
                            held,
                            shape_bytes: d.shape_work_bytes(format, len).saturating_add(held.open),
                        };
                        match cx.acquire(stage.shape_bytes)? {
                            Flow::Go(l) => lease = Some(l),
                            Flow::Stop(end) => return Ok(Flow::Stop(end)),
                        }
                        kura_engine::codec::contain(
                            d.id(),
                            kura_engine::codec::Stage::Decode,
                            || d.shape_file_monitored(path, format, opts, limits, mon),
                        )
                        .and_then(|s| limits.check(&s).map_err(EngineError::Limit).map(|()| s))
                        .map(|s| (s, stage))
                    }
                    Err(e) => Err(e),
                }
            }
            None => {
                codecs.shape_path_staged(path, format, opts, limits, mon, after, &mut |stage| {
                    lease = None;
                    match cx.acquire(stage.shape_bytes) {
                        Ok(Flow::Go(l)) => {
                            lease = Some(l);
                            Ok(())
                        }
                        Ok(Flow::Stop(end)) => {
                            stopped = Some(Ok(end));
                            Err(EngineError::Canceled)
                        }
                        Err(e) => {
                            stopped = Some(Err(e));
                            Err(EngineError::Canceled)
                        }
                    }
                })
            }
        };
        match stopped {
            Some(Ok(end)) => return Ok(Flow::Stop(end)),
            Some(Err(e)) => return Err(e),
            None => {}
        }
        let (shape, stage) = go!(cx.ctx, "元の大きさを読めません", read);
        // 流して読む段の見積もり（形を読む分も覆う）で確保し直す
        drop(lease);
        let need = ensure_estimate(
            codecs.stage_stream_work_bytes(&stage, format, &shape, len),
            &shape,
            tasks,
        )
        .max(stage.shape_bytes);
        let _lease = match cx.acquire(need)? {
            Flow::Go(l) => l,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        let id = stage.id;
        let acx = cx.acx();
        let made = cx.runtime.bulk(|_| {
            store.ensure(
                &key,
                &shape,
                acx,
                |sink| {
                    let mut counting = CountingDecode {
                        inner: sink,
                        pass,
                        total: 0,
                    };
                    codecs.decode_stream_with_path(
                        id,
                        path,
                        format,
                        opts,
                        limits,
                        &mut counting,
                        mon,
                    )
                },
                limits,
                mon,
            )
        });
        match made {
            // この段では読めない：次の段（段の順のとき）
            Err(CoreError::Unsupported(_)) if decoder.is_none() => {
                after = Some(id);
                continue;
            }
            other => return cx.work_flow(other),
        }
    }
}

/// 元を読んだもの（遅延の値・読み取りの記録・作業用の PCM・源の LRU）。
type SourceRead = (Value, DecodeStamp, Option<WorkHandle>, u64);

/// 元を読む（デコーダ `decoder`。None は段の順）：WAV・AIFF で源として開ければ源にして照らしの回で記録を作り、ほかは作業用の
/// PCM にする。返すのは（遅延の値・読み取りの記録・作業用の PCM・源の LRU）。進捗はこの読みを重み `weight` の 1 回で数える。
fn read_source(
    cx: &StreamCx<'_>,
    decoder: Option<&Arc<dyn Decoder>>,
    passes: &mut Passes<'_>,
    weight: f64,
) -> Result<Flow<SourceRead>> {
    let tasks = cx.runtime.bulk_threads();
    let opener = path_opener(cx.original);
    // WAV・AIFF：段の最初が自前の読み手なら源として開ける（記録の識別子が同じときだけ）
    let first = cx
        .codecs
        .decoders_for(cx.format)
        .first()
        .map(|d| d.id())
        .filter(|id| decoder.is_none_or(|d| d.id() == *id));
    if first.is_some() {
        let probe = cx.runtime.bulk(|_| {
            cx.codecs
                .open_audio_source(&opener, cx.format, cx.opts, cx.limits, LRU_MAX)
        });
        let opened = match probe {
            Ok(o) => o,
            // 開けない WAV（読み手が扱わない変種）は作業用の PCM へ回す
            Err(EngineError::Unsupported(_)) => None,
            Err(e) => return Err(from_engine("元を開けません", e)),
        };
        if let Some(opened) = opened {
            let shape = opened.value.shape();
            let resident = lru_bytes(&shape, tasks);
            // 決まった LRU で開き直す（全体が小さければ全体）
            let reopened = cx.runtime.bulk(|_| {
                cx.codecs
                    .open_audio_source(&opener, cx.format, cx.opts, cx.limits, resident)
            });
            let opened = go!(cx.ctx, "元を開けません", reopened)
                .ok_or_else(|| CoreError::Invalid("元を開き直せません".into()))?;
            let d = cx
                .codecs
                .decoder_by_id(opened.decoder)
                .cloned()
                .ok_or_else(|| CoreError::Invalid("源のデコーダがありません".into()))?;
            let _lease = match cx.acquire(verify_bytes(&shape, tasks as u64, resident))? {
                Flow::Go(l) => l,
                Flow::Stop(end) => return Ok(Flow::Stop(end)),
            };
            let pass = Pass {
                passes,
                weight,
                message: "元を照らしています",
            };
            passes.report(weight, 0.0, pass.message);
            let mon = cx.mon;
            let value = opened.value;
            let (value, digest) = go!(
                cx.ctx,
                "元を読めません",
                cx.runtime.bulk(|_| verify_source(
                    cx.codecs,
                    &opener,
                    cx.format,
                    cx.opts,
                    cx.limits,
                    resident,
                    value,
                    mon,
                    Some(pass)
                ))
            );
            passes.finish(weight);
            let decoded = DecodeStamp {
                codec: d.id().to_string(),
                tier: d.tier(),
                reproducible: d.reproducible(),
                value_digest: digest,
                env: d.env(),
            };
            return Ok(Flow::Go((value, decoded, None, resident)));
        }
    }
    // それ以外：作業用の PCM（OS のデコーダを通りうるなら、読み終えるまで印を保つ）
    let platform = match decoder {
        Some(d) => d.tier() == Tier::Platform,
        None => cx
            .codecs
            .decoders_for(cx.format)
            .iter()
            .any(|d| d.tier() == Tier::Platform),
    };
    let pass = Pass {
        passes,
        weight,
        message: "作業用のファイルに読んでいます",
    };
    passes.report(weight, 0.0, pass.message);
    // OS のデコーダの印の割合も今の回のもの（進捗を戻さない）
    let work = read::while_decoding_at(
        cx.ctx,
        platform,
        passes.value(weight, 0.0),
        || passes.value(weight, 1.0),
        || ensure_work(cx, decoder, pass),
    )?;
    let work = match work {
        Flow::Go(w) => w,
        Flow::Stop(end) => return Ok(Flow::Stop(end)),
    };
    passes.finish(weight);
    let resident = lru_bytes(&work.pcm().shape, tasks);
    let opened = cx
        .runtime
        .bulk(|_| anchor::open_source(&work.opener(), cx.limits, resident));
    let opened = go!(cx.ctx, "作業用のファイルを開けません", opened)
        .ok_or_else(|| CoreError::Invalid("作業用のファイルが音声ではありません".into()))?;
    let decoded = work.pcm().decoded.clone();
    Ok(Flow::Go((opened.value, decoded, Some(work), resident)))
}

/// 何を読むか：段の順（アンカーがまだない保存）か、記録の読み取り `decoded` に合う入力（アンカーがある保存・版に戻す作り直し・
/// GC の照合。`platform` が偽なら、記録のデコーダが OS のものなら読まない）。
#[derive(Clone, Copy)]
pub(super) enum Want<'a> {
    Any,
    Recorded {
        decoded: &'a DecodeStamp,
        platform: bool,
    },
}

/// 段 1 の結果：読めた・今は読めない（記録のデコーダがない・OS のデコーダでしか読めない）・記録と違う。
#[allow(clippy::large_enum_variant)] // 1 回の読みで 1 度だけ返して、すぐ取り出す（`read::RecordedRead` と同じ）
pub(super) enum StreamRead {
    Read(StreamInput),
    Unavailable(String),
    Mismatch(String),
}

impl StreamRead {
    /// 保存・版に戻すの扱い：読めない・違うはどちらも Mismatch（docs/editing.md §14「作り直しの照合の決まり」）。
    pub(super) fn or_mismatch(self) -> Result<StreamInput> {
        match self {
            Self::Read(input) => Ok(input),
            Self::Unavailable(why) | Self::Mismatch(why) => Err(CoreError::Mismatch(why)),
        }
    }
}

/// 段 1：入力を得る。アンカー `found` があれば記録と照らしてから源として開き、使えなければ元を記録の識別子のデコーダで読み
/// 直して値を記録と照らす（知らせを `warnings` に足す。`read::read_recorded_input` と同じ分かれ方）。段の順なら元を段の順に
/// 読む。作業用の PCM を作ったら `slot` に入れる（呼び手が一時停止のときに残す印を立てる）。`cant` は何ができないかの言葉。
#[allow(clippy::too_many_arguments)]
pub(super) fn read_input(
    cx: &StreamCx<'_>,
    found: Option<Anchor>,
    want: Want<'_>,
    cant: &str,
    warnings: &mut Vec<String>,
    passes: &mut Passes<'_>,
    slot: &mut Option<WorkHandle>,
) -> Result<Flow<StreamRead>> {
    let tasks = cx.runtime.bulk_threads();
    let Want::Recorded { decoded, platform } = want else {
        let read = match read_source(cx, None, passes, 1.0)? {
            Flow::Go(r) => r,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        let (value, decoded, work, resident) = read;
        *slot = work;
        return Ok(Flow::Go(StreamRead::Read(StreamInput {
            value,
            decoded,
            anchor: AnchorUse::None,
            resident,
        })));
    };
    let mut unusable: Option<(Anchor, String)> = None;
    if let Some(a) = found {
        // アンカーから：記録と照らしてから源として開く
        let path = cx.layout.content_path(&a.hash, &a.ext);
        let opener = path_opener(&path);
        let (limits, mon) = (cx.limits, cx.mon);
        // 照らしの回の量（`verify_stream` の源の LRU 32 MiB ＋ 窓 ＋ ダイジェスト）。見出しを読めなければ照らしで断る
        let shape = File::open(&path)
            .map_err(EngineError::from)
            .and_then(|f| anchor::shape(&mut BufReader::new(f), limits))
            .ok();
        let need = shape.map_or(64 << 20, |s| {
            verify_bytes(&s, tasks as u64, 32 << 20).max(lru_bytes(&s, tasks))
        });
        let _lease = match cx.acquire(need)? {
            Flow::Go(l) => l,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        let pass = Pass {
            passes,
            weight: 0.5,
            message: "元の読み取りを照らしています",
        };
        passes.report(0.5, 0.0, pass.message);
        let checked = cx.runtime.bulk(|_| {
            anchor::verify_stream(&opener, decoded, limits, &PassMonitor { inner: mon, pass })?;
            let probe = anchor::open_source(&opener, limits, LRU_MAX)?
                .ok_or_else(|| EngineError::Internal("音声のアンカーではありません".into()))?;
            let resident = lru_bytes(&probe.value.shape(), tasks);
            let opened = anchor::open_source(&opener, limits, resident)?
                .ok_or_else(|| EngineError::Internal("音声のアンカーではありません".into()))?;
            Ok::<_, EngineError>((opened.value, resident))
        });
        passes.finish(0.5);
        match checked {
            Ok((value, resident)) => {
                // 入力の重み 1 の残り（読み直さなかった分）
                passes.finish(0.5);
                return Ok(Flow::Go(StreamRead::Read(StreamInput {
                    value,
                    decoded: decoded.clone(),
                    anchor: AnchorUse::Existing(a),
                    resident,
                })));
            }
            Err(e) if matches!(e.root(), EngineError::Canceled) => match stop_end(cx.ctx) {
                Some(end) => return Ok(Flow::Stop(end)),
                None => return Err(from_engine("元の読み取りを照らせません", e)),
            },
            Err(e) => {
                warnings.push(format!("アンカーが使えないので、元を読み直しました（{e}）"));
                unusable = Some((a, e.to_string()));
            }
        }
    }
    let also = match &unusable {
        Some((_, why)) => format!("（アンカーも使えません：{why}）"),
        None => String::new(),
    };
    // 元を記録の識別子のデコーダで読み直し、値を記録と照らす
    let Some(decoder) = cx
        .codecs
        .decoder_by_id(&decoded.codec)
        .filter(|d| d.can_decode(cx.format))
        .cloned()
    else {
        return Ok(Flow::Go(StreamRead::Unavailable(format!(
            "{cant}：記録のデコーダ {} がこの環境にありません{also}",
            decoded.codec
        ))));
    };
    if !platform && decoder.tier() == Tier::Platform {
        return Ok(Flow::Go(StreamRead::Unavailable(format!(
            "{cant}：記録のデコーダ {} は OS のデコーダなので、ここでは読みません{also}",
            decoded.codec
        ))));
    }
    // 入力の重み 1 のうち、アンカーを照らした回の残り
    let weight = if unusable.is_some() { 0.5 } else { 1.0 };
    let read = match read_source(cx, Some(&decoder), passes, weight)? {
        Flow::Go(r) => r,
        Flow::Stop(end) => return Ok(Flow::Stop(end)),
    };
    let (value, got, work, resident) = read;
    *slot = work;
    if got.value_digest != decoded.value_digest {
        return Ok(Flow::Go(StreamRead::Mismatch(format!(
            "{cant}：元を読み直した値が記録と違います（{}）{also}",
            decoded.codec
        ))));
    }
    let anchor = match unusable {
        Some((a, _)) => AnchorUse::Fallback(a),
        None => AnchorUse::None,
    };
    Ok(Flow::Go(StreamRead::Read(StreamInput {
        value,
        decoded: decoded.clone(),
        anchor,
        resident,
    })))
}

/// 値 `value` が参照する源の LRU が持ちうる量の和（源 1 つにつき、LRU の上限と源の全体の中身の小さいほうを 1 回。LRU は読んだ
/// チャンクの中身の大きさで数え、上限を超えない。源を参照しない値は 0）。
fn source_resident(value: &Value) -> u64 {
    let Value::Audio(a) = value else {
        return 0;
    };
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    let mut total = 0u64;
    for ch in 0..a.channels() {
        for k in 0..a.chunk_count() {
            if let Ok(Chunk::Source(r)) = a.chunk(ch, k)
                && seen.insert(Arc::as_ptr(&r.src).cast::<()>().addr())
            {
                let whole = r
                    .src
                    .frames()
                    .saturating_mul(r.src.channels() as u64)
                    .saturating_mul(4);
                total = total.saturating_add(r.src.resident_bytes().min(whole));
            }
        }
    }
    total
}

/// 借りた入力（入力・作業用の PCM）。
type Borrowed = (StreamInput, Option<WorkHandle>);

/// エディタが貸している入力を借りる（M13 段 3。`EngineServices::borrow_input`）：同じアイテム・元・読み方のものがあれば、
/// 照らしていなければ照らしの回で値を記録と照らし（違えば使わない）、アンカーに昇格する読み（再現しない読み）なのに作業用の
/// PCM がなければ使わない。使えれば（入力・作業用の PCM）。
pub(super) fn borrowed(
    cx: &StreamCx<'_>,
    item_id: i64,
    options_digest: &[u8; 32],
    passes: &mut Passes<'_>,
) -> Result<Flow<Option<Borrowed>>> {
    let Some(lent) = cx
        .engine
        .borrow_input(item_id, &cx.original_hash, &Digest(*options_digest))
        .filter(|l| l.format == cx.format)
    else {
        return Ok(Flow::Go(None));
    };
    let anchor = match &lent.anchor {
        crate::edit::services::LentAnchor::None => AnchorUse::None,
        crate::edit::services::LentAnchor::Existing(a) => AnchorUse::Existing(a.clone()),
        crate::edit::services::LentAnchor::Fallback(a) => AnchorUse::Fallback(a.clone()),
    };
    if matches!(anchor, AnchorUse::None)
        && anchor::needs_anchor(&lent.decoded)
        && lent.work.is_none()
    {
        return Ok(Flow::Go(None));
    }
    let tasks = cx.runtime.bulk_threads();
    let shape = lent.value.shape();
    // 貸した値の源の LRU は貸し手（エディタ）の式の上限で開いている（多チャンネル・多コアでは保存の式より大きい）。保存が流して
    // 読む間に源の LRU はその上限まで埋まり、エディタが先に閉じれば二重に数えた分も外れるので、上限どおりに数える（M13 段 4C の
    // 検証の直し）
    let resident = lru_bytes(&shape, tasks).max(source_resident(&lent.value));
    if !lent.verified {
        let _lease = match cx.acquire(verify_bytes(&shape, tasks as u64, resident))? {
            Flow::Go(l) => l,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        let pass = Pass {
            passes,
            weight: 1.0,
            message: "元の読み取りを照らしています",
        };
        passes.report(1.0, 0.0, pass.message);
        let mon = cx.mon;
        let value = lent.value.clone();
        let got = cx.runtime.bulk(|_| digest_value(&value, mon, Some(pass)));
        let got = match got {
            Ok(d) => d,
            Err(e) if matches!(e.root(), EngineError::Canceled) => match stop_end(cx.ctx) {
                Some(end) => return Ok(Flow::Stop(end)),
                None => return Err(from_engine("元の読み取りを照らせません", e)),
            },
            // 照らせない貸しものは使わない（ふつうに読む）
            Err(_) => return Ok(Flow::Go(None)),
        };
        if got != lent.decoded.value_digest {
            return Ok(Flow::Go(None));
        }
    }
    passes.finish(1.0);
    Ok(Flow::Go(Some((
        StreamInput {
            value: lent.value,
            decoded: lent.decoded,
            anchor,
            resident,
        },
        lent.work,
    ))))
}

/// 段 3：作業用の PCM をアンカーに昇格する（staging に写して照らす。照らしの進み具合を `pass` に知らせる）。
pub(super) fn promote(
    cx: &StreamCx<'_>,
    work: &WorkHandle,
    decoded: &DecodeStamp,
    job: i64,
    staging: &mut Staging,
    pass: Pass<'_, '_>,
) -> Result<Flow<AnchorUse>> {
    pass.passes.report(pass.weight, 0.0, pass.message);
    let part = cx
        .layout
        .staging_dir()
        .join(format!("edit-{job}-anchor.wav.part"));
    staging.add(part.clone());
    reflink_copy::reflink_or_copy(&work.pcm().path, &part)?;
    File::open(&part)?.sync_all()?;
    let opener = path_opener(&part);
    let (limits, mon) = (cx.limits, cx.mon);
    let verified = cx.runtime.bulk(|_| {
        anchor::verify_stream(&opener, decoded, limits, &PassMonitor { inner: mon, pass })
    });
    go!(cx.ctx, "元の読み取りを残せません", verified);
    let hash = hash_file(&part)?;
    let size = std::fs::metadata(&part)?.len();
    Ok(Flow::Go(AnchorUse::New(NewAnchorFile {
        part,
        hash,
        size,
        ext: "wav",
    })))
}

/// 段 4 の結果。
pub(super) struct Written {
    pub encoded: StreamEncoded,
    pub warnings: Vec<String>,
}

/// 段 4：計画の出力を流して `part` に書き、`sync_all` する（`encoder` があればその識別子の書き手で）。
#[allow(clippy::too_many_arguments)]
pub(super) fn write_output(
    cx: &StreamCx<'_>,
    plan: &kura_engine::Plan,
    input: &StreamInput,
    encoder: Option<&str>,
    out_format: &str,
    enc_opts: &serde_json::Value,
    part: &Path,
    passes: &Passes<'_>,
    weight: f64,
    rounds: u32,
) -> Result<Flow<Written>> {
    let inputs = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::decoded(
            input.value.clone(),
            Digest(cx.original_hash),
            &input.decoded,
        ),
    )]);
    let (codecs, mon) = (cx.codecs, cx.mon);
    let written = cx.runtime.bulk(|e| {
        let mut cache = NodeCache::new(0);
        let mut src = e.output_stream(plan, &inputs, &mut cache, OutputId(0), mon)?;
        let file = File::create(part)?;
        let mut w = BufWriter::new(file);
        let encoded = {
            let mut counting = Counting {
                inner: &mut src,
                passes,
                weight,
                rounds,
                done: 0,
            };
            match encoder {
                Some(id) => codecs.encode_stream_with(
                    id,
                    &mut counting,
                    out_format,
                    enc_opts,
                    &mut w,
                    mon,
                )?,
                None => codecs.encode_stream(&mut counting, out_format, enc_opts, &mut w, mon)?,
            }
        };
        let file = w
            .into_inner()
            .map_err(|e| EngineError::Io(e.into_error()))?;
        file.sync_all()?;
        let warnings = src.warnings().iter().map(|w| w.message.clone()).collect();
        Ok::<_, EngineError>(Written { encoded, warnings })
    });
    Ok(Flow::Go(go!(cx.ctx, "書けません", written)))
}

/// 段 5（R2）：.part を 1 回読んで、生の BLAKE3（台帳）・`kura-file-v1`（書き出しの記録）・大きさを取る。進捗は重み 0.25 の
/// 回で、読んだバイトを数える。
pub(super) fn hash_part(
    path: &Path,
    passes: &Passes<'_>,
    mon: &dyn Monitor,
) -> Result<([u8; 32], Digest, u64)> {
    use std::io::Read;
    const MESSAGE: &str = "書いたものを確かめています";
    let len = std::fs::metadata(path)?.len();
    passes.report(0.25, 0.0, MESSAGE);
    let mut read = 0u64;
    let mut f = File::open(path)?;
    let mut raw = blake3::Hasher::new();
    let mut fd = FileDigest::new(len);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if mon.canceled() {
            return Err(CoreError::Canceled);
        }
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        raw.update(&buf[..n]);
        fd.update(&buf[..n]);
        read = read.saturating_add(n as u64);
        passes.report(0.25, read as f64 / len.max(1) as f64, MESSAGE);
        #[cfg(test)]
        passes.at(super::Stage::Rehash);
    }
    let file_digest = fd
        .finish()
        .map_err(|e| from_engine("書いたファイルを読み直せません", e))?;
    Ok((*raw.finalize().as_bytes(), file_digest, len))
}

#[cfg(test)]
mod tests {
    use kura_engine::testgen::InputGen;
    use kura_engine::value::Chunk;
    use kura_engine::{Engine, NoMonitor};
    use serde_json::json;

    use super::*;

    /// 源の読みの数（チャンク 0 の源の統計）。
    fn source_reads(value: &Value) -> u64 {
        let Value::Audio(a) = value else {
            panic!("音声ではない");
        };
        match a.chunk(0, 0).unwrap() {
            Chunk::Source(r) => r.src.stats().reads + r.src.stats().hits,
            _ => panic!("源ではない"),
        }
    }

    /// 照らしの回のあと、決めた LRU で源を開き直して返す（照らしの回で埋めた LRU を持ったまま計画の確保を待たない。M13 段 3
    /// の検証の直し #10）。ダイジェストは丸ごと読んだ値と同じ。
    #[test]
    fn verified_source_comes_back_with_an_empty_lru() {
        let codecs = CodecSet::pinned();
        let v = InputGen::AudioNoise {
            rate: 8_000,
            channels: 2,
            frames: 3 * AUDIO_CHUNK as u64 + 5,
            seed: 10,
        }
        .generate()
        .unwrap();
        let bytes = codecs
            .encode_bytes(&v, "wav", &json!({ "bits": 16 }), &NoMonitor)
            .unwrap()
            .0;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("a.wav");
        std::fs::write(&path, &bytes).unwrap();
        let opener = path_opener(&path);
        let opts = DecodeOptions::default();
        let limits = Engine::new().unwrap().limits().clone();
        let opened = codecs
            .open_audio_source(&opener, "wav", &opts, &limits, 1 << 20)
            .unwrap()
            .unwrap();
        assert_eq!(source_reads(&opened.value), 0);
        let (value, digest) = verify_source(
            &codecs,
            &opener,
            "wav",
            &opts,
            &limits,
            1 << 20,
            opened.value,
            &NoMonitor,
            None,
        )
        .unwrap();
        assert_eq!(
            source_reads(&value),
            0,
            "照らしの回の LRU を持ったまま返した"
        );
        let (whole, _) = codecs
            .decode(
                &mut std::io::Cursor::new(&bytes),
                "wav",
                &opts,
                &limits,
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(digest, whole.digest().unwrap());
        assert_eq!(value.digest().unwrap(), digest);
    }

    /// 値の源の LRU は源 1 つにつき 1 回数える（全チャンネル・全チャンクが同じ源を指しても 1 回）。数えるのは LRU の上限と
    /// 源の全体の中身の小さいほう。中身を持つ値は 0。
    #[test]
    fn source_resident_counts_each_source_once() {
        let codecs = CodecSet::pinned();
        let frames = 4 * AUDIO_CHUNK as u64 + 1;
        let v = InputGen::AudioNoise {
            rate: 8_000,
            channels: 3,
            frames,
            seed: 11,
        }
        .generate()
        .unwrap();
        let bytes = codecs
            .encode_bytes(&v, "wav", &json!({ "bits": 16 }), &NoMonitor)
            .unwrap()
            .0;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("a.wav");
        std::fs::write(&path, &bytes).unwrap();
        let limits = Engine::new().unwrap().limits().clone();
        let open = |lru: u64| {
            codecs
                .open_audio_source(
                    &path_opener(&path),
                    "wav",
                    &DecodeOptions::default(),
                    &limits,
                    lru,
                )
                .unwrap()
                .unwrap()
                .value
        };
        let whole = 3 * frames * 4;
        assert_eq!(
            source_resident(&open(1 << 20)),
            1 << 20,
            "上限が全体より小さい"
        );
        assert_eq!(
            source_resident(&open(64 << 20)),
            whole,
            "全体が上限より小さい"
        );
        assert_eq!(source_resident(&v), 0, "中身を持つ値は源の LRU を持たない");
    }
}
