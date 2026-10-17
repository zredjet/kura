//! 段 8 の音声の解析のテスト（docs/editing.md §11）：LRA の EBU Tech 3342 の試験 1〜4、ループ点のコーデックの往復、
//! 波形の段・無音の解析をエンジンの口（`Engine::analyze`・`analyze_dyn`）から。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::sync::Arc;

use kura_engine::analysis::audio::FrameSpan;
use kura_engine::analysis::audio::loop_points::{LoopAnalysis, LoopReport};
use kura_engine::analysis::audio::loudness::{LoudnessAnalysis, LoudnessReport, Unmeasured};
use kura_engine::analysis::audio::peaks::{PEAK_BINS, Peaks, PeaksAnalysis, PeaksParams};
use kura_engine::analysis::audio::silence::{SilenceAnalysis, SilenceParams};
use kura_engine::analysis::{Analysis, AnalysisOutput, AnalysisRequest, NoParams};
use kura_engine::codec::CodecSet;
use kura_engine::value::{Audio, AudioMeta, LoopRange, Marker};
use kura_engine::{Engine, Limits, NoMonitor, Value};
use serde_json::json;

/// EBU の試験信号：1 kHz の正弦波を、区間ごとの dBFS（ピーク）で、全チャンネルに同相で入れる。
fn ebu_sine(rate: u32, channels: usize, segments: &[(f64, f64)]) -> Value {
    let mut p = Vec::new();
    let mut n = 0usize;
    for (secs, db) in segments {
        let len = (secs * f64::from(rate)).round() as usize;
        let amp = kura_engine::math::db_to_gain(*db);
        for _ in 0..len {
            let t = n as f64 / f64::from(rate);
            p.push((amp * kura_engine::math::sin(2.0 * std::f64::consts::PI * 1_000.0 * t)) as f32);
            n += 1;
        }
    }
    let planes = (0..channels).map(|_| Arc::new(p.clone())).collect();
    Value::Audio(Arc::new(
        Audio::new(rate, planes, AudioMeta::default()).unwrap(),
    ))
}

fn loudness(v: &Value) -> LoudnessReport {
    Engine::new()
        .unwrap()
        .analyze::<LoudnessAnalysis>(v, &NoParams {}, &NoMonitor)
        .unwrap()
}

/// EBU Tech 3342 の試験 1〜4（1 kHz のステレオの正弦を 20 秒ずつ）：LRA が 10・5・20・15 LU（規格の許容 ±1 LU）。
/// 48 kHz と 44.1 kHz、全体を 2 回繰り返しても同じ（試験の注記。繰り返しは 48 kHz だけ）。kura の式ではちょうどの値に
/// なる（±0.01 で確かめる）。
#[test]
fn lra_matches_ebu_tech_3342_cases_1_to_4() {
    // （名前, 区間（秒, dBFS）, 期待する LRA）
    type Case = (&'static str, Vec<(f64, f64)>, f64);
    let cases: [Case; 4] = [
        ("1", vec![(20.0, -20.0), (20.0, -30.0)], 10.0),
        ("2", vec![(20.0, -20.0), (20.0, -15.0)], 5.0),
        ("3", vec![(20.0, -40.0), (20.0, -20.0)], 20.0),
        (
            "4",
            vec![
                (20.0, -50.0),
                (20.0, -35.0),
                (20.0, -20.0),
                (20.0, -35.0),
                (20.0, -50.0),
            ],
            15.0,
        ),
    ];
    for rate in [48_000u32, 44_100] {
        for (name, segments, want) in &cases {
            let repeats: &[usize] = if rate == 48_000 { &[1, 2] } else { &[1] };
            for &repeat in repeats {
                let all: Vec<(f64, f64)> =
                    (0..repeat).flat_map(|_| segments.iter().copied()).collect();
                let r = loudness(&ebu_sine(rate, 2, &all));
                let lra = *r
                    .range
                    .value()
                    .unwrap_or_else(|| panic!("試験 {name}：{:?}", r.range));
                let ctx = format!("{rate} Hz 試験 {name}（{repeat} 回）：{lra} LU（期待 {want}）");
                assert!((lra - want).abs() <= 1.0, "規格の許容の外：{ctx}");
                assert!((lra - want).abs() <= 0.01, "{ctx}");
                // ほかの欄も計れる
                assert!(r.integrated.value().is_some(), "{ctx}");
                assert!(r.short_term_max.value().is_some(), "{ctx}");
            }
        }
    }
}

/// 400 ms・3 秒の境目：区間 ⌊(fs + 5)/10⌋ 標本の 4 個・30 個ちょうどから計れる。短い音声はピークだけ（ユーザーの決定）。
#[test]
fn short_clips_report_only_peaks() {
    for (frames, integrated, range) in [
        (17_639usize, false, false),
        (17_640, true, false),
        (132_299, true, false),
        (132_300, true, true),
    ] {
        let r = loudness(&ebu_sine(44_100, 1, &[(frames as f64 / 44_100.0, -20.0)]));
        assert_eq!(r.integrated.value().is_some(), integrated, "{frames}");
        assert_eq!(r.momentary_max.value().is_some(), integrated, "{frames}");
        assert_eq!(r.range.value().is_some(), range, "{frames}");
        assert_eq!(r.short_term_max.value().is_some(), range, "{frames}");
        if !integrated {
            assert_eq!(r.integrated.reason(), Some(&Unmeasured::TooShort));
        }
        if !range {
            assert_eq!(r.range.reason(), Some(&Unmeasured::TooShort));
        }
        assert!(r.sample_peak.value().is_some() && r.true_peak.value().is_some());
        // −20 dBFS のピーク
        assert!((r.sample_peak.value().unwrap() + 20.0).abs() < 0.01);
    }
}

fn encode(v: &Value, format: &'static str, opts: serde_json::Value) -> Vec<u8> {
    CodecSet::pinned()
        .encode_bytes(v, format, &opts, &NoMonitor)
        .unwrap_or_else(|e| panic!("{format}: {e}"))
        .0
}

fn decode(bytes: &[u8], format: &str) -> Value {
    CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes.to_vec()),
            format,
            &Default::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap_or_else(|e| panic!("{format}: {e}"))
        .0
}

fn loop_of(v: &Value) -> LoopReport {
    Engine::new()
        .unwrap()
        .analyze::<LoopAnalysis>(v, &NoParams {}, &NoMonitor)
        .unwrap()
}

/// ループ点の往復（WAV の smpl・cue、AIFF の MARK・INST、FLAC・Ogg Vorbis の LOOPSTART・LOOPLENGTH）：解析のループ点
/// ＝読んだメタデータ＝もう一度書いて読んだループ点。マーカーは WAV・AIFF だけが持つ。
#[test]
fn loop_points_round_trip_through_the_codecs() {
    let frames = 30_000usize;
    let planes = vec![Arc::new(
        (0..frames)
            .map(|i| (0.3 * kura_engine::math::sin(i as f64 * 0.05)) as f32)
            .collect::<Vec<f32>>(),
    )];
    for range in [
        LoopRange {
            start: 1_000,
            end: 25_000,
        },
        LoopRange { start: 0, end: 1 },
        LoopRange {
            start: 29_999,
            end: 30_000,
        },
    ] {
        let meta = AudioMeta {
            loop_range: Some(range),
            markers: vec![
                Marker {
                    pos: 20_000,
                    name: "B".into(),
                },
                Marker {
                    pos: 500,
                    name: "A".into(),
                },
            ],
            ..AudioMeta::default()
        };
        let v = Value::Audio(Arc::new(
            Audio::new(44_100, planes.clone(), meta.clone()).unwrap(),
        ));
        let want = Some(FrameSpan {
            start: range.start,
            end: range.end,
        });
        assert_eq!(loop_of(&v).range, want);
        for format in ["wav", "aiff", "flac", "ogg"] {
            let opts = if format == "ogg" {
                json!({})
            } else {
                json!({ "bits": 24 })
            };
            let back = decode(&encode(&v, format, opts.clone()), format);
            let Value::Audio(a) = &back else { panic!() };
            let r = loop_of(&back);
            assert_eq!(r.range, want, "{format} {range:?}");
            assert_eq!(
                a.meta.loop_range.map(|l| FrameSpan {
                    start: l.start,
                    end: l.end
                }),
                r.range,
                "{format}"
            );
            assert!(!r.dropped_loop, "{format}");
            if matches!(format, "wav" | "aiff") {
                let names: Vec<&str> = r.markers.iter().map(|m| m.name.as_str()).collect();
                assert_eq!(names, ["A", "B"], "{format}");
            }
            let again = decode(&encode(&back, format, opts), format);
            assert_eq!(loop_of(&again).range, want, "{format} の書き直し");
        }
    }
    // 他のツール（vorbis_rs）で書いた Ogg Vorbis のループ点
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("ogg-vorbis-loop.ogg");
    let v = decode(&std::fs::read(path).unwrap(), "ogg");
    let Value::Audio(a) = &v else { panic!() };
    let r = loop_of(&v);
    assert_eq!(
        r.range,
        a.meta.loop_range.map(|l| FrameSpan {
            start: l.start,
            end: l.end
        })
    );
    assert!(r.range.is_some());
}

/// 波形の段をエンジンの口から：正規の形はバイナリ（kura-peaks）で、段の表は ⌈N/B⌉、16 ビットの WAV に書いた値の段は
/// 書く前の値の段と同じ（どちらも同じ 16 ビットの格子）。
#[test]
fn peaks_through_the_engine_and_a_16_bit_wav() {
    let engine = Engine::new().unwrap();
    let frames = 100_003usize;
    let planes: Vec<Arc<Vec<f32>>> = (0..2)
        .map(|c| {
            Arc::new(
                (0..frames)
                    .map(|i| {
                        (0.9 * kura_engine::math::sin(i as f64 * (0.01 + 0.003 * c as f64))) as f32
                    })
                    .collect(),
            )
        })
        .collect();
    let v = Value::Audio(Arc::new(
        Audio::new(48_000, planes, AudioMeta::default()).unwrap(),
    ));
    let out = engine
        .analyze_dyn(&AnalysisRequest::new("audio.peaks"), &v, &NoMonitor)
        .unwrap();
    assert_eq!(&out.output[..8], b"kurapeak");
    let p = Peaks::from_canonical(&out.output).unwrap();
    assert_eq!((p.channels, p.frames, p.rate), (2, frames as u64, 48_000));
    for (l, b) in p.levels.iter().zip(PEAK_BINS) {
        assert_eq!(l.bin, b);
        assert_eq!(l.data.len(), frames.div_ceil(b as usize) * 2 * 2);
    }
    let wav = decode(&encode(&v, "wav", json!({ "bits": 16 })), "wav");
    let q = engine
        .analyze::<PeaksAnalysis>(&wav, &PeaksParams::default(), &NoMonitor)
        .unwrap();
    assert_eq!(q.levels, p.levels);
    // 範囲 [a, b) のビンは段ごとに連続したバイト列（ビンが外側・チャンネルが内側）
    let at = 32 + 24 * 5;
    let first = i16::from_le_bytes([out.output[at], out.output[at + 1]]);
    assert_eq!(Some(first), p.get(0, 0, 0).map(|(lo, _)| lo));
}

/// 無音の解析をエンジンの口から：16 ビットの WAV に書いて読んでも、前後の無音とクリップの列は同じ。
#[test]
fn silence_survives_a_16_bit_round_trip() {
    let engine = Engine::new().unwrap();
    let mut x = vec![0.0f32; 20_000];
    for (i, v) in x.iter_mut().enumerate().take(15_000).skip(3_000) {
        *v = (0.5 * kura_engine::math::sin(i as f64 * 0.02)) as f32;
    }
    x[8_000..8_010].fill(1.2);
    x[9_000..9_002].fill(-1.0);
    let v = Value::Audio(Arc::new(
        Audio::new(16_000, vec![Arc::new(x)], AudioMeta::default()).unwrap(),
    ));
    let p = SilenceParams {
        threshold_db: -60.0,
    };
    let a = engine
        .analyze::<SilenceAnalysis>(&v, &p, &NoMonitor)
        .unwrap();
    let wav = decode(&encode(&v, "wav", json!({ "bits": 16 })), "wav");
    let b = engine
        .analyze::<SilenceAnalysis>(&wav, &p, &NoMonitor)
        .unwrap();
    assert_eq!(a.content, b.content);
    assert_eq!((a.clip_runs, b.clip_runs), (1, 1));
    // 1 を超える値は 16 ビットに書くと端に止まる
    assert_eq!((a.overs, b.overs), (10, 0));
    assert_eq!(
        a.content,
        Some(FrameSpan {
            start: 3_000,
            end: 15_000
        })
    );
}

/// NaN・±∞ のある音声（解析の例）：16 ビットの WAV に書くと NaN は 0、±∞ は端になる（`pcm::quantize`）。無音の解析は
/// NaN を無音、±∞ を音ありとして読むので、書いて読んでも前後の無音とクリップの列が同じ（nonFinite だけが 0 になる）。
/// 波形の段も同じ格子なので、書く前と同じ。
#[test]
fn non_finite_examples_read_like_their_16_bit_export() {
    let engine = Engine::new().unwrap();
    let silence = SilenceAnalysis::examples()
        .into_iter()
        .filter(|e| ["non-finite-edges", "nan-is-silent"].contains(&e.name));
    let mut n = 0;
    for ex in silence {
        let v = ex.input.generate().unwrap();
        let a = engine
            .analyze::<SilenceAnalysis>(&v, &ex.params, &NoMonitor)
            .unwrap();
        let wav = decode(&encode(&v, "wav", json!({ "bits": 16 })), "wav");
        let b = engine
            .analyze::<SilenceAnalysis>(&wav, &ex.params, &NoMonitor)
            .unwrap();
        assert!(a.non_finite > 0 && b.non_finite == 0, "{}", ex.name);
        assert_eq!(
            (a.content, a.lead, a.trail, a.clip_runs, a.overs),
            (b.content, b.lead, b.trail, b.clip_runs, b.overs),
            "{}",
            ex.name
        );
        n += 1;
    }
    assert_eq!(n, 2);
    let ex = PeaksAnalysis::examples()
        .into_iter()
        .find(|e| e.name == "non-finite-bins")
        .unwrap();
    let v = ex.input.generate().unwrap();
    let p = engine
        .analyze::<PeaksAnalysis>(&v, &ex.params, &NoMonitor)
        .unwrap();
    let wav = decode(&encode(&v, "wav", json!({ "bits": 16 })), "wav");
    let q = engine
        .analyze::<PeaksAnalysis>(&wav, &ex.params, &NoMonitor)
        .unwrap();
    assert_eq!(p.levels, q.levels);
}
