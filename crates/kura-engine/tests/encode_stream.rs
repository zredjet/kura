//! 流れの書き出しの受け入れテスト（M13 段 3。docs/editing.md §17 の E43）。
//!
//! - streamed_encode_equals_encode_bytes：ランダムな音声の文書（量子化・normalize の流し解きを含む）と決まった長さ（0・1・31・
//!   33・ブロック・チャンク・窓の境の前後）で、`CodecSet::encode_stream(Engine::output_stream)` のバイト列・記録が
//!   `encode_bytes(render の値)` と同じ。値のダイジェストが `render` のダイジェストと同じ。警告が同じ。
//! - value_producer_streams_like_encode：値の生産者（`ValueProducer`）を窓 1 チャンクずつに縛っても同じバイト列。
//! - streaming_needs_a_full_pass：全体を 1 度も流さずに書き終えたエンコーダは `Internal`。流して書けないエンコーダは `Unsupported`。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use kura_engine::audio::stream::{AudioProducer, AudioSink, StreamInfo, ValueProducer};
use kura_engine::codec::{CodecSet, EncodedStamp, FileDigest};
use kura_engine::doc::{Dither, Graph, InputId, OutputId, QUANTIZE_VERSION, Quantize};
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::region::Spans;
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::testgen::InputGen;
use kura_engine::testing::region::{
    below, random_audio_chain, random_audio_input, random_stateful_chain,
};
use kura_engine::value::{AUDIO_CHUNK, Audio, AudioMeta, LoopRange, Marker};
use kura_engine::{Digest, EngineError, Issue, Monitor, NoMonitor, Value, determinism};
use serde_json::json;

const C: u64 = AUDIO_CHUNK as u64;

/// 書き出しの組（形式・設定）。Ogg Vorbis の流して書く形は後ろ向き・前向きの 2 回流す（一度に渡す形が /1 の正。品質の
/// 範囲と細かい窓・区間は `codec::vorbis::stream_tests` が見る）。
fn targets() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("ogg", json!({})),
        ("ogg", json!({ "quality": -1 })),
        ("ogg", json!({ "quality": 10 })),
        ("flac", json!({})),
        ("flac", json!({ "bits": 8 })),
        ("flac", json!({ "bits": 24 })),
        ("wav", json!({})),
        ("wav", json!({ "bits": 8 })),
        ("wav", json!({ "bits": 16 })),
        ("wav", json!({ "bits": 24 })),
        ("wav", json!({ "bits": 32 })),
        ("wav", json!({ "float": true })),
        ("aiff", json!({})),
        ("aiff", json!({ "bits": 8 })),
        ("aiff", json!({ "bits": 24 })),
        ("aiff", json!({ "float": true })),
    ]
}

fn file_digest(bytes: &[u8]) -> Digest {
    let mut d = FileDigest::new(bytes.len() as u64);
    d.update(bytes);
    d.finish().unwrap()
}

struct Rendered {
    value: Value,
    digest: Digest,
    warnings: Vec<Issue>,
}

fn render(engine: &Engine, plan: &Plan, input: &Value, source: Digest) -> Option<Rendered> {
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source,
        },
    )]);
    let mut r = engine.render(plan, &plain, &NoMonitor).ok()?;
    let out = r.outputs.swap_remove(0);
    Some(Rendered {
        value: out.value,
        digest: out.digest,
        warnings: r.warnings,
    })
}

/// 1 つの文書・入力を、組ごとに流して書いたものと値を書いたもので照らす。照らした組の数を返す。
fn check_doc(
    engine: &Engine,
    codecs: &CodecSet,
    g: &Graph,
    input: &Value,
    source: Digest,
    targets: &[(&'static str, serde_json::Value)],
    what: &str,
) -> usize {
    let Ok(plan) = engine.compile(g, &BTreeMap::from([(InputId(0), input.shape())])) else {
        return 0;
    };
    let Some(want) = render(engine, &plan, input, source) else {
        return 0;
    };
    let fin = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), source).unwrap(),
    )]);
    let doc = String::from_utf8_lossy(&g.to_canonical_json().unwrap()).into_owned();
    let mut n = 0;
    for (format, opts) in targets {
        let whole = codecs.encode_bytes(&want.value, format, opts, &NoMonitor);
        let mut cache = NodeCache::new(0);
        let mut src = engine
            .output_stream(&plan, &fin, &mut cache, OutputId(0), &NoMonitor)
            .unwrap_or_else(|e| panic!("{what} {format}: {e}\n{doc}"));
        let mut out = Cursor::new(Vec::new());
        let streamed = codecs.encode_stream(&mut src, format, opts, &mut out, &NoMonitor);
        match (whole, streamed) {
            (Ok((bytes, stamp)), Ok(se)) => {
                let got = out.into_inner();
                assert!(
                    got == bytes,
                    "{what} {format} {opts}：バイト列が違う（{} と {}）\n{doc}",
                    got.len(),
                    bytes.len()
                );
                let streamed_stamp: EncodedStamp = se.stamp(file_digest(&got));
                assert_eq!(streamed_stamp, stamp, "{what} {format} {opts}\n{doc}");
                assert_eq!(
                    se.value_digest, want.digest,
                    "{what} {format} {opts}\n{doc}"
                );
                assert_eq!(src.warnings(), want.warnings.as_slice(), "{what}\n{doc}");
                n += 1;
            }
            (Err(a), Err(b)) => {
                assert_eq!(
                    std::mem::discriminant(&a),
                    std::mem::discriminant(&b),
                    "{what} {format} {opts}：誤りの種類が違う（{a} と {b}）"
                );
            }
            (a, b) => panic!(
                "{what} {format} {opts}：片方だけ書けた（{:?} と {:?}）\n{doc}",
                a.map(|x| x.0.len()),
                b.map(|x| x.bytes)
            ),
        }
    }
    n
}

/// E43：ランダムな音声の文書と決まった長さで、流して書いたものが値を書いたものと同じ。
#[test]
fn streamed_encode_equals_encode_bytes() {
    let engine = Engine::new().unwrap();
    let codecs = CodecSet::pinned();
    let targets = targets();
    let mut r = determinism::rng(4301);
    let mut checked = 0;
    for case in 0..24 {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = below(&mut r, 4) as usize;
        let g = random_audio_chain(&engine, &mut r, &input.shape(), len);
        checked += check_doc(
            &engine,
            &codecs,
            &g,
            &input,
            Digest([case as u8; 32]),
            &targets,
            &format!("ランダム {case}"),
        );
    }
    // 決まった長さ（0・1・31・33・ブロック・チャンク・窓の境の前後）：ゲイン → フェード → 正規化（流し解き）、量子化つき
    for (k, frames) in [0u64, 1, 31, 33, 4_095, 4_097, C - 1, C + 1, 16 * C + 5]
        .into_iter()
        .enumerate()
    {
        let input = InputGen::AudioNoise {
            rate: 8_000,
            channels: 1 + (k % 2) as u16,
            frames,
            seed: k as u64,
        }
        .generate()
        .unwrap();
        let mut g = Graph::new_single();
        g.push_linear("audio.gain", 1, json!({ "db": -2.0 }), 1)
            .unwrap();
        g.push_linear(
            "audio.fade",
            1,
            json!({ "fadeIn": frames / 3, "fadeOut": frames / 4, "curve": "linear" }),
            2,
        )
        .unwrap();
        if frames > 0 {
            g.push_linear(
                "audio.normalize",
                1,
                json!({ "target": -18.0, "peakLimit": -1.0 }),
                3,
            )
            .unwrap();
        }
        if k % 3 == 0 {
            g.outputs[0].quantize = Some(Quantize {
                v: QUANTIZE_VERSION,
                bits: 16,
                dither: Dither::Tpdf,
                seed: k as u64,
            });
        }
        // 窓が 2 つになる長さの Ogg Vorbis は既定の品質だけ（デバッグのビルドで重い）
        let some: Vec<_> = targets
            .iter()
            .filter(|(f, o)| frames < 16 * C || *f != "ogg" || o.get("quality").is_none())
            .cloned()
            .collect();
        checked += check_doc(
            &engine,
            &codecs,
            &g,
            &input,
            Digest([7; 32]),
            &some,
            &format!("長さ {frames}"),
        );
    }
    assert!(checked > 150, "{checked}");
}

/// E43（M17 段 1）：試験の状態を運ぶ操作（状態の表を流して解き、表から区間を描く核）を混ぜたランダムな文書と、決まった
/// 長さの鎖（状態を運ぶ操作 2 つ。格子 C と 3·4,096・先読み・リンク）で、流して書いたもの（WAV・FLAC・Ogg Vorbis の 2 回の
/// 流し）が値を書いたものと同じ。
#[test]
fn stateful_streamed_encode_equals_encode_bytes() {
    let engine = kura_engine::testing::stateful::engine().unwrap();
    let codecs = CodecSet::pinned();
    let targets = targets();
    let mut r = determinism::rng(4302);
    let (mut checked, mut with_state) = (0, 0);
    for case in 0..16 {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = 1 + below(&mut r, 3) as usize;
        let g = random_stateful_chain(&engine, &mut r, &input.shape(), len);
        with_state += usize::from(
            g.nodes
                .iter()
                .any(|n| n.op == kura_engine::testing::stateful::OP_ID),
        );
        checked += check_doc(
            &engine,
            &codecs,
            &g,
            &input,
            Digest([case as u8; 32]),
            &targets,
            &format!("状態のランダム {case}"),
        );
    }
    for (k, frames) in [0u64, 1, 33, 4_097, C + 1, 16 * C + 5]
        .into_iter()
        .enumerate()
    {
        let input = InputGen::AudioNoise {
            rate: 8_000,
            channels: 2,
            frames,
            seed: 40 + k as u64,
        }
        .generate()
        .unwrap();
        let mut g = Graph::new_single();
        g.push_linear("audio.gain", 1, json!({ "db": -2.0 }), 1)
            .unwrap();
        for (i, (grid, lookahead, link)) in [(C, 0u64, false), (3 * 4_096, 37, true)]
            .into_iter()
            .enumerate()
        {
            g.push_linear(
                kura_engine::testing::stateful::OP_ID,
                1,
                json!({ "b0": 0.4, "b1": 0.2, "a1": -0.7, "lookahead": lookahead, "link": link,
                        "grid": grid, "wet": 0.9, "gainDb": -1.0 }),
                2 + i as u64,
            )
            .unwrap();
        }
        let some: Vec<_> = targets
            .iter()
            .filter(|(f, o)| frames < 16 * C || *f != "ogg" || o.get("quality").is_none())
            .cloned()
            .collect();
        checked += check_doc(
            &engine,
            &codecs,
            &g,
            &input,
            Digest([8; 32]),
            &some,
            &format!("状態の長さ {frames}"),
        );
        with_state += 1;
    }
    assert!(checked > 60 && with_state > 10, "{checked} {with_state}");
}

/// 窓を 1 チャンクずつに縛る生産者（値の生産者を区間ごとに呼ぶ）。
struct OneChunk<'a>(ValueProducer<'a>);

impl AudioProducer for OneChunk<'_> {
    fn info(&self) -> &StreamInfo {
        self.0.info()
    }

    fn produce(
        &mut self,
        spans: Option<&Spans>,
        sink: &mut dyn AudioSink,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let frames = self.0.info().frames;
        let want = |k: u64| {
            let (a, b) = (k * C, ((k + 1) * C).min(frames));
            spans.is_none_or(|s| s.spans().iter().any(|(x, y)| *x < b && a < *y))
        };
        let mut inner = Forward {
            sink,
            begun: false,
            info: self.0.info().clone(),
        };
        inner.sink.begin(&inner.info)?;
        inner.begun = true;
        for k in 0..frames.div_ceil(C) {
            if want(k) {
                self.0
                    .produce(Some(&Spans::one(k * C, k * C + 1)), &mut inner, mon)?;
            }
        }
        inner.sink.finish()
    }
}

/// 内側の流れの begin・finish を呼び手へ渡さず、窓だけを渡す包み。
struct Forward<'s> {
    sink: &'s mut dyn AudioSink,
    begun: bool,
    info: StreamInfo,
}

impl AudioSink for Forward<'_> {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        assert!(self.begun && *info == self.info);
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.sink.feed(start, planes, mon)
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

fn meta_rich(frames: u64) -> AudioMeta {
    let mut meta = AudioMeta::default();
    if frames > 10 {
        meta.loop_range = Some(LoopRange {
            start: 3,
            end: frames - 2,
        });
        meta.markers = vec![Marker {
            pos: frames / 2,
            name: "なか".into(),
        }];
    }
    meta.tags.insert("title".into(), "流れ".into());
    meta.tags.insert("artist".into(), "kura".into());
    meta
}

/// E43：値の生産者を 1 チャンクずつに縛っても、`encode_bytes` と同じバイト列・ダイジェスト（メタデータつき）。
#[test]
fn value_producer_streams_like_encode() {
    let codecs = CodecSet::pinned();
    let mut r = determinism::rng(4302);
    for (rate, channels, frames) in [
        (8_000u32, 1u16, 0u64),
        (8_000, 2, 1),
        (16_000, 3, 2 * C + 37),
        (48_000, 6, C + 4_097),
    ] {
        let Value::Audio(a) = (InputGen::AudioNoise {
            rate,
            channels,
            frames,
            seed: r.next_u64_for_test(),
        })
        .generate()
        .unwrap() else {
            panic!()
        };
        let a = Arc::new(Audio::clone(&a).with_meta(meta_rich(frames)));
        let value = Value::Audio(a.clone());
        for (format, opts) in targets() {
            let whole = codecs.encode_bytes(&value, format, &opts, &NoMonitor);
            let mut src = OneChunk(ValueProducer::new(&a));
            let mut out = Cursor::new(Vec::new());
            let streamed = codecs.encode_stream(&mut src, format, &opts, &mut out, &NoMonitor);
            match (whole, streamed) {
                (Ok((bytes, stamp)), Ok(se)) => {
                    let got = out.into_inner();
                    assert!(got == bytes, "{format} {opts} {channels} {frames}");
                    assert_eq!(se.stamp(file_digest(&got)), stamp);
                    assert_eq!(se.value_digest, value.digest().unwrap());
                }
                (Err(_), Err(_)) => {}
                (a, b) => panic!(
                    "{format} {opts} {frames}：{:?} {:?}",
                    a.map(|x| x.0.len()),
                    b.map(|x| x.bytes)
                ),
            }
        }
    }
}

/// 全体を 1 度も流さずに書き終えたら `Internal`（偽のエンコーダ）。流して書けない書き手は `Unsupported`。
#[test]
fn streaming_needs_a_full_pass() {
    use kura_engine::codec::{Encoder, MetaCaps, Tier, WriteSeek};
    use kura_engine::op::ParamSchema;
    use kura_engine::shape::Shape;

    struct Lazy;
    impl Encoder for Lazy {
        fn id(&self) -> &'static str {
            "lazy-test-enc/1"
        }
        fn tier(&self) -> Tier {
            Tier::External
        }
        fn can_encode(&self, format: &str, shape: &Shape) -> bool {
            format == "wav" && matches!(shape, Shape::Audio { .. })
        }
        fn options(&self, _format: &str) -> ParamSchema {
            ParamSchema { params: vec![] }
        }
        fn caps(&self, format: &str, _opts: &serde_json::Value) -> MetaCaps {
            kura_engine::codec::caps_of(format)
        }
        fn encode(
            &self,
            _value: &Value,
            _format: &str,
            _opts: &serde_json::Value,
            _out: &mut dyn std::io::Write,
            _mon: &dyn Monitor,
        ) -> Result<u64, EngineError> {
            Ok(0)
        }
        fn streams(&self, _format: &str) -> bool {
            true
        }
        fn encode_stream(
            &self,
            src: &mut dyn AudioProducer,
            _format: &str,
            _opts: &serde_json::Value,
            _out: &mut dyn WriteSeek,
            mon: &dyn Monitor,
        ) -> Result<u64, EngineError> {
            // 頭のチャンクだけ流す
            struct Drop0;
            impl AudioSink for Drop0 {
                fn begin(&mut self, _: &StreamInfo) -> Result<(), EngineError> {
                    Ok(())
                }
                fn feed(
                    &mut self,
                    _: u64,
                    _: &[&[f32]],
                    _: &dyn Monitor,
                ) -> Result<(), EngineError> {
                    Ok(())
                }
                fn finish(&mut self) -> Result<(), EngineError> {
                    Ok(())
                }
            }
            src.produce(Some(&Spans::one(0, 1)), &mut Drop0, mon)?;
            Ok(0)
        }
    }

    let Value::Audio(a) = (InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 2 * C,
        seed: 1,
    })
    .generate()
    .unwrap() else {
        panic!()
    };
    let mut set = CodecSet::default();
    set.add_encoder(Arc::new(Lazy)).unwrap();
    let mut out = Cursor::new(Vec::new());
    let err = set
        .encode_stream(
            &mut ValueProducer::new(&a),
            "wav",
            &json!({}),
            &mut out,
            &NoMonitor,
        )
        .unwrap_err();
    assert!(matches!(err, EngineError::Internal(_)), "{err}");
    // 流して書けない書き手（PNG の形式には音声を書けない）
    let err = CodecSet::pinned()
        .encode_stream(
            &mut ValueProducer::new(&a),
            "png",
            &json!({}),
            &mut out,
            &NoMonitor,
        )
        .unwrap_err();
    assert!(matches!(err, EngineError::Unsupported(_)), "{err}");
}

/// テストの乱数（u64）。
trait NextU64 {
    fn next_u64_for_test(&mut self) -> u64;
}

impl NextU64 for rand_chacha::ChaCha8Rng {
    fn next_u64_for_test(&mut self) -> u64 {
        use rand_chacha::rand_core::Rng;
        self.next_u64()
    }
}
