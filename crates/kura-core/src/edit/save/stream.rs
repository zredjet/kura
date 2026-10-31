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
//! 進捗はパスの重み（作業用のファイル 1・照らしの回 0.5・書く回 × 1・昇格の照らし 0.5・R2 の読み直し 0.25）で数える。
//! 一時停止で止めるときは作業用のファイルを残す印を立て、再開は初めから（`ensure` が残っているものを使い回す）。

use kura_engine::audio::digest::PcmTreeDigest;
use kura_engine::audio::source::Opener;
use kura_engine::audio::stream::{
    AudioProducer, AudioSink, STREAM_CHUNKS, StreamInfo, stream_value,
};
use kura_engine::budget::Class;
use kura_engine::codec::{CodecSet, Decoder, FileDigest, PickedStage, ReadSeek, StreamEncoded};
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::region::Spans;
use kura_engine::value::AUDIO_CHUNK;

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
}

impl<'a> Passes<'a> {
    /// 重みの和 `total` で始める。
    pub(super) fn new(ctx: &'a JobContext, total: f64) -> Self {
        Self {
            ctx,
            total: total.max(1e-9),
            done: 0.0,
        }
    }

    /// 今の回（重み `weight`）の進み `frac`（0〜1）を知らせる。
    fn report(&self, weight: f64, frac: f64, message: &str) {
        let p = (self.done + weight * frac.clamp(0.0, 1.0)) / self.total;
        self.ctx.progress(0.05 + 0.9 * p.clamp(0.0, 1.0), message);
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
        };
        self.inner.produce(spans, &mut wrap, mon)
    }

    fn digest(&mut self) -> std::result::Result<Option<Digest>, EngineError> {
        self.inner.digest()
    }
}

/// 流れの窓を値のダイジェストに足す受け手（照らしの回）。
struct DigestOnly {
    digest: Option<PcmTreeDigest>,
}

impl AudioSink for DigestOnly {
    fn begin(&mut self, info: &StreamInfo) -> std::result::Result<(), EngineError> {
        self.digest = Some(PcmTreeDigest::new(info.rate, info.channels, info.frames)?);
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> std::result::Result<(), EngineError> {
        match &mut self.digest {
            Some(d) => d.push_window(start, planes, mon),
            None => Err(EngineError::Internal("照らしの回が始まっていません".into())),
        }
    }

    fn finish(&mut self) -> std::result::Result<(), EngineError> {
        Ok(())
    }
}

/// 遅延の値 `value` を 1 回流して値のダイジェストを取る（照らしの回）。
fn digest_value(value: &Value, mon: &dyn Monitor) -> std::result::Result<Digest, EngineError> {
    let Value::Audio(a) = value else {
        return Err(EngineError::Internal("音声ではありません".into()));
    };
    let mut sink = DigestOnly { digest: None };
    stream_value(a, None, &mut sink, mon)?;
    sink.digest
        .ok_or_else(|| EngineError::Internal("照らしの回を流していません".into()))?
        .finish(&a.meta)
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
/// 形を読めた段で流して読む（その段が `Unsupported` なら次の段の形を読み直す。M13 段 2 の検証の直しと同じ考え方）。
fn ensure_work(cx: &StreamCx<'_>, decoder: Option<&Arc<dyn Decoder>>) -> Result<Flow<WorkHandle>> {
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
                |sink| codecs.decode_stream_with_path(id, path, format, opts, limits, sink, mon),
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
/// PCM にする。返すのは（遅延の値・読み取りの記録・作業用の PCM・源の LRU）。
fn read_source(
    cx: &StreamCx<'_>,
    decoder: Option<&Arc<dyn Decoder>>,
    passes: &mut Passes<'_>,
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
            passes.report(0.5, 0.0, "元を照らしています");
            let mon = cx.mon;
            let value = opened.value;
            let digest = go!(
                cx.ctx,
                "元を読めません",
                cx.runtime.bulk(|_| digest_value(&value, mon))
            );
            passes.finish(0.5);
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
    passes.report(1.0, 0.0, "作業用のファイルに読んでいます");
    let work = read::while_decoding(cx.ctx, platform, || ensure_work(cx, decoder))?;
    let work = match work {
        Flow::Go(w) => w,
        Flow::Stop(end) => return Ok(Flow::Stop(end)),
    };
    passes.finish(1.0);
    let resident = lru_bytes(&work.pcm().shape, tasks);
    let opened = cx
        .runtime
        .bulk(|_| anchor::open_source(&work.opener(), cx.limits, resident));
    let opened = go!(cx.ctx, "作業用のファイルを開けません", opened)
        .ok_or_else(|| CoreError::Invalid("作業用のファイルが音声ではありません".into()))?;
    let decoded = work.pcm().decoded.clone();
    Ok(Flow::Go((opened.value, decoded, Some(work), resident)))
}

/// 段 1：入力を得る。アンカー `found` があれば照らしてから源として開き、使えなければ元を記録のデコーダで読み直す（知らせを
/// `warnings` に足す）。アンカーがなければ元を段の順に読む。
pub(super) fn read_input(
    cx: &StreamCx<'_>,
    found: Option<Anchor>,
    warnings: &mut Vec<String>,
    passes: &mut Passes<'_>,
    slot: &mut Option<WorkHandle>,
) -> Result<Flow<StreamInput>> {
    let tasks = cx.runtime.bulk_threads();
    let Some(a) = found else {
        let read = match read_source(cx, None, passes)? {
            Flow::Go(r) => r,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        let (value, decoded, work, resident) = read;
        *slot = work;
        return Ok(Flow::Go(StreamInput {
            value,
            decoded,
            anchor: AnchorUse::None,
            resident,
        }));
    };
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
    passes.report(0.5, 0.0, "元の読み取りを照らしています");
    let checked = cx.runtime.bulk(|_| {
        anchor::verify_stream(&opener, &a.decoded, limits, mon)?;
        let probe = anchor::open_source(&opener, limits, LRU_MAX)?
            .ok_or_else(|| EngineError::Internal("音声のアンカーではありません".into()))?;
        let resident = lru_bytes(&probe.value.shape(), tasks);
        let opened = anchor::open_source(&opener, limits, resident)?
            .ok_or_else(|| EngineError::Internal("音声のアンカーではありません".into()))?;
        Ok::<_, EngineError>((opened.value, resident))
    });
    passes.finish(0.5);
    let why = match checked {
        Ok((value, resident)) => {
            return Ok(Flow::Go(StreamInput {
                value,
                decoded: a.decoded.clone(),
                anchor: AnchorUse::Existing(a),
                resident,
            }));
        }
        Err(e) if matches!(e.root(), EngineError::Canceled) => match stop_end(cx.ctx) {
            Some(end) => return Ok(Flow::Stop(end)),
            None => return Err(from_engine("元の読み取りを照らせません", e)),
        },
        Err(e) => e.to_string(),
    };
    warnings.push(format!(
        "アンカーが使えないので、元を読み直しました（{why}）"
    ));
    // 元を記録の識別子のデコーダで読み直し、値を記録と照らす
    let Some(decoder) = cx
        .codecs
        .decoder_by_id(&a.decoded.codec)
        .filter(|d| d.can_decode(cx.format))
        .cloned()
    else {
        return Err(CoreError::Mismatch(format!(
            "保存できません：記録のデコーダ {} がこの環境にありません（アンカーも使えません：{why}）",
            a.decoded.codec
        )));
    };
    let read = match read_source(cx, Some(&decoder), passes)? {
        Flow::Go(r) => r,
        Flow::Stop(end) => return Ok(Flow::Stop(end)),
    };
    let (value, decoded, work, resident) = read;
    *slot = work;
    if decoded.value_digest != a.decoded.value_digest {
        return Err(CoreError::Mismatch(format!(
            "保存できません：元を読み直した値が記録と違います（アンカーも使えません：{why}）"
        )));
    }
    Ok(Flow::Go(StreamInput {
        value,
        decoded: a.decoded.clone(),
        anchor: AnchorUse::Fallback(a),
        resident,
    }))
}

/// 段 3：作業用の PCM をアンカーに昇格する（staging に写して照らす）。
pub(super) fn promote(
    cx: &StreamCx<'_>,
    work: &WorkHandle,
    decoded: &DecodeStamp,
    job: i64,
    staging: &mut Staging,
) -> Result<Flow<AnchorUse>> {
    let part = cx
        .layout
        .staging_dir()
        .join(format!("edit-{job}-anchor.wav.part"));
    staging.add(part.clone());
    reflink_copy::reflink_or_copy(&work.pcm().path, &part)?;
    File::open(&part)?.sync_all()?;
    let opener = path_opener(&part);
    let (limits, mon) = (cx.limits, cx.mon);
    let verified = cx
        .runtime
        .bulk(|_| anchor::verify_stream(&opener, decoded, limits, mon));
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

/// 段 4：計画の出力を流して `part` に書き、`sync_all` する。
#[allow(clippy::too_many_arguments)]
pub(super) fn write_output(
    cx: &StreamCx<'_>,
    plan: &kura_engine::Plan,
    input: &StreamInput,
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
            codecs.encode_stream(&mut counting, out_format, enc_opts, &mut w, mon)?
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

/// 段 5（R2）：.part を 1 回読んで、生の BLAKE3（台帳）・`kura-file-v1`（書き出しの記録）・大きさを取る。
pub(super) fn hash_part(path: &Path, mon: &dyn Monitor) -> Result<([u8; 32], Digest, u64)> {
    use std::io::Read;
    let len = std::fs::metadata(path)?.len();
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
    }
    let file_digest = fd
        .finish()
        .map_err(|e| from_engine("書いたファイルを読み直せません", e))?;
    Ok((*raw.finalize().as_bytes(), file_digest, len))
}
