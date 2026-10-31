//! 長い音声の 1 手（M13 段 1。docs/editing.md §17 の E41）：60 分・48 kHz ステレオの合成の源（LRU 64 MiB）で、
//! trim → fade → normalize → gain → set_loop の文書に 1 手（ゲインの引数）を当て、見えている 10 秒 ＋ 試聴の 6 秒の区間を
//! `render_view` で描く。
//!
//! 1. `estimate(spec.area)` のピーク ≤ N（LRU の上限 ＋ 64 MiB）。
//! 2. `AreaStats.drawn_chunks` が、区間のチャンクの数 × ノードの数を超えない。
//! 3. 源が作ったチャンクの数（normalize の最初の流し解きを除く）が区間の分だけ。
//! 4. 出力の常駐のバイト（`resident_bytes`）が区間の分以下。
//!
//! 60 分は重い（最初の表示で normalize の計測を全体に流すので、デバッグのビルドで数十秒）ので通常のテストでは回さない：
//! `cargo test --release -p kura-engine --test long_audio -- --ignored`。通常のテストでは同じ確かめを 6 分の音声で回す。
//!
//! 長い音声の保存（M13 段 3。E44）：60 分・48 kHz ステレオの合成の源（LRU 64 MiB）の trim → fade → normalize → gain を、
//! `Engine::output_stream` から WAV・FLAC・ogg q5 に流して書く（書き先は数えるだけ）。(1) `estimate(spec.stream)` のピーク ≤
//! N_save（LRU ＋ 128 MiB）、(2) 源が作ったチャンク ≤ (パスの数 ＋ 流し解き) × 全体、(3) 受け手に渡る窓は 16 チャンクまで
//! （出力を組まない）。通常のテストは 2 分（Ogg Vorbis は 30 秒）、60 分・3 時間は `#[ignore]`。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::audio::source::AudioSource;
use kura_engine::audio::stream::{AudioProducer, AudioSink, StreamInfo};
use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::render::Engine;
use kura_engine::render::audio::chunks_covering;
use kura_engine::render::cache::{Fingerprint, FingerprintedInput, NodeCache};
use kura_engine::render::estimate::{CacheProbe, EstimateSpec, Source};
use kura_engine::render::region::Area;
use kura_engine::render::view::ViewRequest;
use kura_engine::testing::source::SyntheticSource;
use kura_engine::value::{AUDIO_CHUNK, Audio, AudioMeta};
use kura_engine::{Digest, NoMonitor, Value};
use serde_json::json;

const RATE: u64 = 48_000;
const MIB: u64 = 1 << 20;
const LRU: u64 = 64 * MIB;

/// trim → fade → normalize → gain → set_loop（長さ `frames` の入力に）。
fn document(frames: u64, db: f64) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear(
        "audio.trim",
        1,
        json!({ "start": RATE + 123, "end": frames - RATE }),
        1,
    )
    .unwrap();
    g.push_linear(
        "audio.fade",
        1,
        json!({ "fadeIn": 2 * RATE, "fadeOut": 3 * RATE, "curve": "equalPower" }),
        2,
    )
    .unwrap();
    g.push_linear(
        "audio.normalize",
        1,
        json!({ "target": -16.0, "peakLimit": -1.0 }),
        3,
    )
    .unwrap();
    g.push_linear("audio.gain", 1, json!({ "db": db }), 4)
        .unwrap();
    g.push_linear(
        "audio.set_loop",
        1,
        json!({ "loop": { "start": 10 * RATE, "end": 20 * RATE } }),
        5,
    )
    .unwrap();
    g
}

/// 測ったもの（報告用）。
#[derive(Debug)]
pub struct StepReport {
    pub estimate_peak: u64,
    pub drawn_chunks: u64,
    pub area_chunks: u64,
    pub source_reads_first: u64,
    pub source_reads_step: u64,
    pub resident: u64,
}

/// 長さ `seconds` 秒の音声で 1 手を描いて、E41 の 4 つを確かめる。
fn one_step(seconds: u64) -> StepReport {
    let frames = seconds * RATE;
    let engine = Engine::new().unwrap();
    let src = Arc::new(SyntheticSource::new(RATE as u32, 2, frames, 7, LRU));
    let value = Value::Audio(Arc::new(
        Audio::from_source(src.clone(), AudioMeta::default()).unwrap(),
    ));
    let shape = value.shape();
    // 遅延の入力の指紋は記録か派生の指紋（開くときに全体のダイジェストを取らない）
    let inputs = BTreeMap::from([(
        InputId(0),
        FingerprintedInput {
            value,
            source: Digest([1; 32]),
            fingerprint: Fingerprint(Digest([2; 32])),
        },
    )]);
    let shapes = BTreeMap::from([(InputId(0), shape)]);
    let plan = engine.compile(&document(frames, -2.0), &shapes).unwrap();
    // 見えている 10 秒（全体の 1/3 あたり）＋ 試聴の 6 秒（半分あたり）
    let view_at = frames / 3;
    let audition = frames / 2;
    let area =
        Area::span(view_at, view_at + 10 * RATE).union(&Area::span(audition, audition + 6 * RATE));
    let req = ViewRequest {
        output: OutputId(0),
        area: area.clone(),
        focus: None,
    };
    let mut cache = NodeCache::new(512 * MIB);
    cache.register_external(&inputs[&InputId(0)].value);
    let first = engine
        .render_view(&plan, &inputs, &mut cache, &req, None, &NoMonitor)
        .unwrap();
    assert_eq!(first.area.presolved, 1, "最初の表示で計測を流して解く");
    let source_reads_first = src.stats().reads;
    // 1 手：ゲインの引数
    let next = engine.compile(&document(frames, -5.0), &shapes).unwrap();
    let spec = EstimateSpec::new(16)
        .all_inputs(&next, Source::Held)
        .area(OutputId(0), area.clone());
    let est = engine
        .estimate(
            &next,
            &spec,
            Some(CacheProbe {
                cache: &cache,
                inputs: &inputs,
            }),
        )
        .unwrap();
    let before = src.stats().reads;
    let got = engine
        .render_view(
            &next,
            &inputs,
            &mut cache,
            &req,
            Some(&first.view),
            &NoMonitor,
        )
        .unwrap();
    let source_reads_step = src.stats().reads - before;
    let out_frames = frames - 2 * RATE - 123;
    let area_chunks = chunks_covering(&area, out_frames as usize).count() as u64;
    let report = StepReport {
        estimate_peak: est.peak,
        drawn_chunks: got.area.drawn_chunks,
        area_chunks,
        source_reads_first,
        source_reads_step,
        resident: got.view.value().resident_bytes(),
    };
    let n = LRU + 64 * MIB;
    // (1) 見積もりのピーク ≤ N
    assert!(report.estimate_peak <= n, "{report:?}");
    // (2) 描いたチャンク ≤ 区間のチャンク × ノードの数（流し解きは起きない）
    assert_eq!(got.area.presolved, 0, "{report:?}");
    assert!(report.drawn_chunks <= area_chunks * 5, "{report:?}");
    // (3) 源が作ったチャンク ≤ 区間の分（trim のずれで、区間ごとに 1 チャンク多く掛かりうる。同じ番号を同時に読もうと
    // したら、先に読み始めたものを待つので二度作らない）
    assert!(report.source_reads_step <= area_chunks + 2, "{report:?}");
    // (4) 出力の常駐のバイト ≤ 区間のチャンクの分
    assert!(
        report.resident <= area_chunks * 2 * AUDIO_CHUNK as u64 * 4,
        "{report:?}"
    );
    // 全体を描いていない（源のチャンクの総数より、手で作った数がずっと少ない）
    assert!(
        report.source_reads_step * 4 < frames.div_ceil(AUDIO_CHUNK as u64),
        "{report:?}"
    );
    assert_eq!(src.frames(), frames);
    report
}

/// E41 の確かめを、通常のテストで回せる長さ（6 分）で。
#[test]
fn one_step_on_six_minutes_stays_small() {
    one_step(6 * 60);
}

/// E41：60 分（デバッグのビルドでは重いので、CI で `--release -- --ignored`）。
#[test]
#[ignore = "重い（--release -- --ignored で回す）"]
fn one_step_on_sixty_minutes_stays_small() {
    let r = one_step(60 * 60);
    eprintln!("60 分：{r:?}");
}

/// E41：3 時間でも同じ（`#[ignore]`）。
#[test]
#[ignore = "重い（--release -- --ignored で回す）"]
fn three_hours_step() {
    let r = one_step(3 * 60 * 60);
    eprintln!("3 時間：{r:?}");
}

// ---- M13 段 3（E44）：長い音声の保存を流して書く ----

/// 書いたバイト数だけを数える書き先（シークを受ける。FLAC は最後に STREAMINFO へ戻る）。
#[derive(Default)]
struct NullWriteSeek {
    pos: u64,
    len: u64,
}

impl std::io::Write for NullWriteSeek {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pos += buf.len() as u64;
        self.len = self.len.max(self.pos);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for NullWriteSeek {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let to = match pos {
            std::io::SeekFrom::Start(p) => Some(p),
            std::io::SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            std::io::SeekFrom::End(d) => self.len.checked_add_signed(d),
        };
        self.pos = to.ok_or_else(|| std::io::Error::other("範囲の外へのシーク"))?;
        Ok(self.pos)
    }
}

/// 生産者を包み、受け手に渡った窓の最大の長さと回数を数える。
struct Spy<'a> {
    inner: &'a mut dyn AudioProducer,
    max_window: u64,
    windows: u64,
}

struct SpySink<'s> {
    sink: &'s mut dyn AudioSink,
    max_window: &'s mut u64,
    windows: &'s mut u64,
}

impl AudioSink for SpySink<'_> {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), kura_engine::EngineError> {
        self.sink.begin(info)
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn kura_engine::Monitor,
    ) -> Result<(), kura_engine::EngineError> {
        let n = planes.first().map_or(0, |p| p.len()) as u64;
        *self.max_window = (*self.max_window).max(n);
        *self.windows += 1;
        self.sink.feed(start, planes, mon)
    }

    fn finish(&mut self) -> Result<(), kura_engine::EngineError> {
        self.sink.finish()
    }
}

impl AudioProducer for Spy<'_> {
    fn info(&self) -> &StreamInfo {
        self.inner.info()
    }

    fn produce(
        &mut self,
        spans: Option<&kura_engine::render::region::Spans>,
        sink: &mut dyn AudioSink,
        mon: &dyn kura_engine::Monitor,
    ) -> Result<(), kura_engine::EngineError> {
        let mut wrap = SpySink {
            sink,
            max_window: &mut self.max_window,
            windows: &mut self.windows,
        };
        self.inner.produce(spans, &mut wrap, mon)
    }

    fn digest(&mut self) -> Result<Option<Digest>, kura_engine::EngineError> {
        self.inner.digest()
    }
}

/// 長い音声の保存の測ったもの（報告用）。
#[derive(Debug)]
pub struct SaveReport {
    pub format: &'static str,
    pub estimate_peak: u64,
    pub passes: u32,
    pub source_reads: u64,
    pub input_chunks: u64,
    pub max_window: u64,
    pub windows: u64,
    pub bytes: u64,
    pub secs: f64,
}

/// trim → fade → normalize → gain（E44 の文書）。
fn save_document(frames: u64) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear(
        "audio.trim",
        1,
        json!({ "start": RATE + 123, "end": frames - RATE }),
        1,
    )
    .unwrap();
    g.push_linear(
        "audio.fade",
        1,
        json!({ "fadeIn": 2 * RATE, "fadeOut": 3 * RATE, "curve": "equalPower" }),
        2,
    )
    .unwrap();
    g.push_linear(
        "audio.normalize",
        1,
        json!({ "target": -16.0, "peakLimit": -1.0 }),
        3,
    )
    .unwrap();
    g.push_linear("audio.gain", 1, json!({ "db": -1.5 }), 4)
        .unwrap();
    g
}

/// 長さ `seconds` 秒の合成の源（LRU 64 MiB）の文書を `formats` に流して書き、E44 の 3 つを確かめる。
fn save_streams(seconds: u64, formats: &[(&'static str, serde_json::Value)]) -> Vec<SaveReport> {
    // 報告用に時間を測る（描画の外。値に効かない）
    #![allow(clippy::disallowed_methods)]
    let frames = seconds * RATE;
    let engine = Engine::new().unwrap();
    let codecs = kura_engine::codec::CodecSet::pinned();
    let plan = {
        let shape = kura_engine::Shape::Audio {
            rate: RATE as u32,
            channels: 2,
            frames: kura_engine::Dim::Exact(frames),
        };
        engine
            .compile(
                &save_document(frames),
                &BTreeMap::from([(InputId(0), shape)]),
            )
            .unwrap()
    };
    let mut out = Vec::new();
    for (format, opts) in formats {
        let src = Arc::new(SyntheticSource::new(RATE as u32, 2, frames, 11, LRU));
        let value = Value::Audio(Arc::new(
            Audio::from_source(src.clone(), AudioMeta::default()).unwrap(),
        ));
        let inputs = BTreeMap::from([(
            InputId(0),
            FingerprintedInput {
                value,
                source: Digest([1; 32]),
                fingerprint: Fingerprint(Digest([3; 32])),
            },
        )]);
        let spec = EstimateSpec::new(16)
            .stream(OutputId(0))
            .digests(true)
            .input(InputId(0), Source::Lazy { resident: LRU })
            .output(
                OutputId(0),
                kura_engine::render::estimate::Sink::Encode {
                    codecs: &codecs,
                    format,
                    opts,
                },
            );
        let est = engine.estimate(&plan, &spec, None).unwrap();
        let started = std::time::Instant::now();
        let mut cache = NodeCache::new(0);
        let mut stream = engine
            .output_stream(&plan, &inputs, &mut cache, OutputId(0), &NoMonitor)
            .unwrap();
        let mut spy = Spy {
            inner: &mut stream,
            max_window: 0,
            windows: 0,
        };
        let mut sink = NullWriteSeek::default();
        let se = codecs
            .encode_stream(&mut spy, format, opts, &mut sink, &NoMonitor)
            .unwrap();
        let input_chunks = frames.div_ceil(AUDIO_CHUNK as u64);
        let report = SaveReport {
            format,
            estimate_peak: est.peak,
            passes: est.stream_passes,
            source_reads: src.stats().reads,
            input_chunks,
            max_window: spy.max_window,
            windows: spy.windows,
            bytes: se.bytes,
            secs: started.elapsed().as_secs_f64(),
        };
        assert_eq!(sink.len, se.bytes, "{report:?}");
        // (1) 見積もりのピーク ≤ N_save（源の LRU ＋ 128 MiB）
        assert!(report.estimate_peak <= LRU + 128 * MIB, "{report:?}");
        // (2) 源が作ったチャンク ≤ パスの数 × 全体 ＋ 流し解き（全体を 1 回）。窓ごとに trim のずれで 1 チャンク多く掛かりうる
        let windows_per_pass = frames.div_ceil(16 * AUDIO_CHUNK as u64);
        let bound = (u64::from(report.passes) + 1) * (input_chunks + windows_per_pass + 1);
        assert!(report.source_reads <= bound, "{report:?}・上限 {bound}");
        // (3) 出力を組まない：受け手に渡る窓は 16 チャンクまで
        assert!(report.max_window <= 16 * AUDIO_CHUNK as u64, "{report:?}");
        out.push(report);
    }
    out
}

/// E44 の確かめを、通常のテストで回せる長さ（2 分。WAV・FLAC。Ogg Vorbis はデバッグのビルドで重いので 30 秒）で。
#[test]
fn two_minute_save_streams() {
    save_streams(2 * 60, &[("wav", json!({})), ("flac", json!({}))]);
    save_streams(30, &[("ogg", json!({ "quality": 5 }))]);
}

/// E44：60 分・48 kHz ステレオを WAV・FLAC・ogg q5 に流して書く（`--release -- --ignored`）。
#[test]
#[ignore = "重い（--release -- --ignored で回す）"]
fn sixty_minute_save_streams() {
    for r in save_streams(
        60 * 60,
        &[
            ("wav", json!({})),
            ("flac", json!({})),
            ("ogg", json!({ "quality": 5 })),
        ],
    ) {
        eprintln!("60 分：{r:?}");
    }
}

/// E44：3 時間でも同じ（`#[ignore]`）。
#[test]
#[ignore = "重い（--release -- --ignored で回す）"]
fn three_hour_save_streams() {
    for r in save_streams(
        3 * 60 * 60,
        &[
            ("wav", json!({})),
            ("flac", json!({})),
            ("ogg", json!({ "quality": 5 })),
        ],
    ) {
        eprintln!("3 時間：{r:?}");
    }
}
