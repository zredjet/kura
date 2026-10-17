//! 段 6 の音声の受け入れテスト（docs/editing.md §17 の E16〜E20）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::sync::Arc;

use kura_engine::analysis::loudness;
use kura_engine::codec::CodecSet;
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::testing::render_value;
use kura_engine::value::{Audio, AudioMeta, LoopRange, Marker};
use kura_engine::{Engine, Limits, NoMonitor, Value};
use proptest::prelude::*;
use serde_json::json;

fn op(id: &str, params: serde_json::Value, input: Value) -> Value {
    render_value(id, 1, params, input).unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn audio(v: &Value) -> &Arc<Audio> {
    match v {
        Value::Audio(a) => a,
        _ => panic!("音声ではありません"),
    }
}

fn noise(rate: u32, channels: u16, frames: u64, seed: u64) -> Value {
    InputGen::AudioNoise {
        rate,
        channels,
        frames,
        seed,
    }
    .generate()
    .unwrap()
}

/// ループ点とマーカー付きの雑音。
fn noise_with_meta(frames: u64) -> Value {
    InputGen::WithMeta {
        base: Box::new(InputGen::AudioNoise {
            rate: 44_100,
            channels: 2,
            frames,
            seed: 5,
        }),
        meta: MetaGen {
            loop_range: Some(LoopRange {
                start: frames / 4,
                end: frames * 3 / 4,
            }),
            markers: vec![(frames / 3, "a".into()), (frames / 2, "b".into())],
            ..MetaGen::default()
        },
    }
    .generate()
    .unwrap()
}

fn trim(v: Value, start: u64, end: u64) -> Value {
    op("audio.trim", json!({ "start": start, "end": end }), v)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// E16：音声のトリムを 2 回するのは、合成した 1 回のトリムと同じ（ループ点・マーカーの写し方も含めて）。
    #[test]
    fn e16_trim_composes(n in 8u64..3_000, a in 0u64..1_000, b in 0u64..1_000, c in 0u64..1_000, d in 0u64..1_000) {
        let (a1, b1) = (a % n, b % n);
        prop_assume!(a1 < b1);
        let len = b1 - a1;
        let (a2, b2) = (c % len, d % (len + 1));
        prop_assume!(a2 < b2);
        let input = noise_with_meta(n);
        let twice = trim(trim(input.clone(), a1, b1), a2, b2);
        let once = trim(input, a1 + a2, a1 + b2);
        prop_assert_eq!(audio(&twice).frames() as u64, b2 - a2);
        prop_assert_eq!(&twice, &once);
    }
}

/// 整数の格子に乗った音声（`bits` ビット）。
fn on_grid(bits: u32, channels: usize, frames: usize, seed: u64) -> Value {
    let scale = f64::from(1u32 << (bits - 1));
    let mut planes = Vec::new();
    let mut state = seed;
    for _ in 0..channels {
        let mut p = Vec::with_capacity(frames);
        for _ in 0..frames {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let range = 1u64 << bits;
            let q = (state >> 11) % range;
            let v = q as f64 - scale;
            p.push((v / scale) as f32);
        }
        planes.push(Arc::new(p));
    }
    Value::Audio(Arc::new(
        Audio::new(48_000, planes, AudioMeta::default()).unwrap(),
    ))
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

/// E17：ゲイン 0dB と長さ 0 のフェードは何も変えない。
#[test]
fn e17_identity_ops() {
    let input = noise_with_meta(5_000);
    assert_eq!(op("audio.gain", json!({ "db": 0.0 }), input.clone()), input);
    assert_eq!(
        op(
            "audio.fade",
            json!({ "fadeIn": 0, "fadeOut": 0, "curve": "linear" }),
            input.clone()
        ),
        input
    );
    assert_eq!(
        op(
            "audio.fade",
            json!({ "fadeIn": 0, "fadeOut": 0, "curve": "equalPower" }),
            input.clone()
        ),
        input
    );
}

/// E17：int16・int24 → f32 → 元のビット数の往復はビット一致（WAV・AIFF・FLAC）。
#[test]
fn e17_integer_round_trips() {
    for format in ["wav", "aiff", "flac"] {
        for bits in [16u32, 24] {
            for channels in [1usize, 2, 6] {
                let v = on_grid(
                    bits,
                    channels,
                    10_007,
                    u64::from(bits) * 7 + channels as u64,
                );
                let first = encode(&v, format, json!({ "bits": bits }));
                let back = decode(&first, format);
                assert_eq!(
                    audio(&back).planes(),
                    audio(&v).planes(),
                    "{format} {bits} ビット {channels}ch"
                );
                let again = encode(&back, format, json!({ "bits": bits }));
                assert_eq!(
                    first, again,
                    "{format} {bits} ビット {channels}ch の書き直し"
                );
            }
        }
    }
}

/// E18：区間ごとに描いても（チャンク 1・7・4,096・全体）値が同じ。音声を出すすべての操作の例で確かめる。
/// 音声を出すのに核（`Op::audio_kernel`）を返さない操作は失敗にする（核を書き忘れた操作が網から抜けないように）。
#[test]
fn e18_chunked_rendering_matches() {
    let engine = Engine::new().unwrap();
    let (mut audio_examples, mut checked) = (0, 0);
    for op in engine.registry().all() {
        for ex in op.examples().unwrap() {
            let inputs: Vec<Value> = ex.inputs.iter().map(|g| g.generate().unwrap()).collect();
            // 持ち上げ（フレームの列の各フレームに当てる）は実行器が行うので、その例だけは apply を直に呼べない。
            // ほかは入力の種類によらず apply し、出力が音声なら核を求める（音声以外から音声を出す操作も網に掛ける）
            let lifted = op.signature().lift != kura_engine::op::Lift::None
                && inputs.iter().any(|v| matches!(v, Value::Frames(_)));
            if lifted {
                continue;
            }
            let refs: Vec<&Value> = inputs.iter().collect();
            let params = op.parse(&ex.params).unwrap();
            let limits = Limits::default();
            let mut cx = kura_engine::op::ApplyCx {
                mon: &NoMonitor,
                limits: &limits,
                seed: 0,
                rules: kura_engine::doc::RULES,
                warnings: Vec::new(),
            };
            let whole = op.apply(params.as_ref(), &refs, &mut cx).unwrap().value;
            let Value::Audio(whole) = &whole else {
                continue;
            };
            audio_examples += 1;
            let Some(kernel) = op.audio_kernel(params.as_ref(), &refs, &mut cx) else {
                panic!("{} {}：音声を出すのに核がありません", op.id(), ex.name);
            };
            let kernel = kernel.unwrap_or_else(|e| panic!("{} {}: {e}", op.id(), ex.name));
            let shape = (kernel.rate(), kernel.channels(), kernel.frames());
            let want_shape = (whole.rate(), whole.channels(), whole.frames() as u64);
            assert_eq!(shape, want_shape, "{} {}", op.id(), ex.name);
            for chunk in [1usize, 7, 4_096, usize::MAX] {
                for ch in 0..kernel.channels() {
                    let len = kernel.frames() as usize;
                    let mut got = vec![0f32; len];
                    let mut start = 0usize;
                    while start < len {
                        let end = start.saturating_add(chunk).min(len);
                        kernel.render(ch, start as u64, &mut got[start..end]);
                        start = end;
                    }
                    let want = &whole.planes()[ch][..];
                    assert!(
                        got.iter()
                            .zip(want)
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                        "{} {}：チャンク {chunk}・チャンネル {ch} で値が違う",
                        op.id(),
                        ex.name
                    );
                }
            }
            checked += 1;
        }
    }
    assert_eq!(checked, audio_examples);
    assert!(checked > 0);
}

/// EBU Tech 3341 の試験信号：1 kHz の正弦波を、区間ごとの dBFS（ピーク）で、全チャンネルに同相で入れる。
fn ebu_sine(rate: u32, channels: &[f64], segments: &[(f64, f64)]) -> Value {
    let frames: Vec<usize> = segments
        .iter()
        .map(|(secs, _)| (secs * f64::from(rate)).round() as usize)
        .collect();
    let total: usize = frames.iter().sum();
    let mut planes = Vec::new();
    for ch_db in channels {
        let mut p = Vec::with_capacity(total);
        let mut n = 0usize;
        for ((_, db), len) in segments.iter().zip(&frames) {
            let amp = kura_engine::math::db_to_gain(*db + ch_db);
            for _ in 0..*len {
                let t = n as f64 / f64::from(rate);
                p.push(
                    (amp * kura_engine::math::sin(2.0 * std::f64::consts::PI * 1000.0 * t)) as f32,
                );
                n += 1;
            }
        }
        planes.push(Arc::new(p));
    }
    Value::Audio(Arc::new(
        Audio::new(rate, planes, AudioMeta::default()).unwrap(),
    ))
}

/// E19：ラウドネスの計測が EBU Tech 3341 の試験信号で ±0.1 LU。正規化したあとの積分ラウドネスが目標の ±0.1 LU。
#[test]
fn e19_loudness_matches_ebu_3341() {
    let stereo = [0.0, 0.0];
    let cases: Vec<(&str, Value, f64)> = vec![
        ("1", ebu_sine(48_000, &stereo, &[(20.0, -23.0)]), -23.0),
        ("2", ebu_sine(48_000, &stereo, &[(20.0, -33.0)]), -33.0),
        (
            "3",
            ebu_sine(
                48_000,
                &stereo,
                &[(10.0, -36.0), (60.0, -23.0), (10.0, -36.0)],
            ),
            -23.0,
        ),
        (
            "4",
            ebu_sine(
                48_000,
                &stereo,
                &[
                    (10.0, -72.0),
                    (10.0, -36.0),
                    (60.0, -23.0),
                    (10.0, -36.0),
                    (10.0, -72.0),
                ],
            ),
            -23.0,
        ),
        (
            "5",
            ebu_sine(
                48_000,
                &stereo,
                &[(20.0, -26.0), (20.1, -20.0), (20.0, -26.0)],
            ),
            -23.0,
        ),
        // 5.0：L・R は −28、C は −24、Ls・Rs は −30 dBFS
        (
            "6",
            ebu_sine(48_000, &[-5.0, -5.0, -1.0, -7.0, -7.0], &[(20.0, -23.0)]),
            -23.0,
        ),
        (
            "1 at 44.1k",
            ebu_sine(44_100, &stereo, &[(20.0, -23.0)]),
            -23.0,
        ),
    ];
    for (name, v, want) in &cases {
        let m = loudness::measure(audio(v), &NoMonitor).unwrap();
        assert!(
            (m.integrated - want).abs() <= 0.1,
            "試験 {name}：{} LUFS（期待 {want}）",
            m.integrated
        );
    }
    // 正規化：目標 -23 と -16 に合わせる。利得をかけると絶対ゲート（−70 LKFS）をまたぐブロックが入れ替わる入力
    // （検証で見つけた形。1 回の計測の差では 1.6〜3.3 LU ずれ、静かな入力は計れないとされた）も含める
    let mut cases = cases;
    for (name, segments) in [
        ("gate-10s-69-60s-71", vec![(10.0, -69.0), (60.0, -71.0)]),
        ("gate-20s-64-40s-71", vec![(20.0, -64.0), (40.0, -71.0)]),
        // 全体が −70 LKFS より静かな入力（利得をかければゲートを通る）
        ("gate-quiet-only", vec![(10.0, -75.0), (10.0, -82.0)]),
    ] {
        cases.push((name, ebu_sine(8_000, &stereo, &segments), f64::NAN));
    }
    for target in [-23.0, -16.0] {
        for (name, v, _) in &cases {
            let out = op(
                "audio.normalize",
                json!({ "target": target, "peakLimit": null }),
                v.clone(),
            );
            let m = loudness::measure(audio(&out), &NoMonitor).unwrap();
            assert!(
                (m.integrated - target).abs() <= 0.1,
                "試験 {name} を {target} に正規化：{} LUFS",
                m.integrated
            );
        }
    }
}

/// EBU Tech 3341 の 15〜19：トゥルーピークが +0.2/−0.4 dB に入る。
#[test]
fn e19_true_peak_matches_ebu_3341() {
    let rate = 48_000u32;
    let make = |div: f64, amp: f64, phase_deg: f64| {
        let frames = rate as usize;
        let fade = (0.01 * f64::from(rate)) as usize;
        let p: Vec<f32> = (0..frames)
            .map(|n| {
                let env = if n < fade {
                    n as f64 / fade as f64
                } else if n >= frames - fade {
                    (frames - 1 - n) as f64 / fade as f64
                } else {
                    1.0
                };
                let ph = phase_deg.to_radians();
                (env * amp
                    * kura_engine::math::sin(2.0 * std::f64::consts::PI * n as f64 / div + ph))
                    as f32
            })
            .collect();
        let planes = vec![Arc::new(p.clone()), Arc::new(p)];
        Audio::new(rate, planes, AudioMeta::default()).unwrap()
    };
    let cases = [
        ("15", make(4.0, 0.5, 0.0), -6.0),
        ("16", make(4.0, 0.5, 45.0), -6.0),
        ("17", make(6.0, 0.5, 60.0), -6.0),
        ("18", make(8.0, 0.5, 67.5), -6.0),
        ("19", make(4.0, 1.41, 45.0), 3.0),
    ];
    for (name, a, want) in &cases {
        let m = loudness::measure(a, &NoMonitor).unwrap();
        assert!(
            m.true_peak_db <= want + 0.2 && m.true_peak_db >= want - 0.4,
            "試験 {name}：{} dBTP（期待 {want}）",
            m.true_peak_db
        );
    }
}

/// 音声の書き出しの設定（可逆の形式は 24 ビット、Ogg Vorbis は既定の品質）。
fn audio_opts(format: &str) -> serde_json::Value {
    if format == "ogg" {
        json!({})
    } else {
        json!({ "bits": 24 })
    }
}

/// Ogg のページのシリアル番号と CRC を 0 にしたもの（シリアル番号は値のダイジェストから決まるので、メタデータが変わると
/// 変わる。それ以外のバイト列を比べる）。
fn without_serial(ogg: &[u8]) -> Vec<u8> {
    let mut b = ogg.to_vec();
    let mut at = 0usize;
    while at < b.len() {
        assert_eq!(&b[at..at + 4], b"OggS", "ページの印（{at}）");
        let n = usize::from(b[at + 26]);
        let body: usize = b[at + 27..at + 27 + n]
            .iter()
            .map(|l| usize::from(*l))
            .sum();
        b[at + 14..at + 18].fill(0);
        b[at + 22..at + 26].fill(0);
        at += 27 + n + body;
    }
    b
}

/// 値の標本はそのままに、メタデータだけを `meta` に替える。
fn with_meta_of(v: &Value, meta: &AudioMeta) -> Value {
    let mut a = (**audio(v)).clone();
    a.meta = meta.clone();
    Value::Audio(Arc::new(a))
}

/// E20：WAV の smpl・cue、AIFF の MARK・INST、FLAC・Ogg Vorbis の LOOPSTART・LOOPLENGTH を読んで書いても保たれる
/// （マーカーを持たないのは FLAC と Ogg Vorbis）。
#[test]
fn e20_loop_points_round_trip() {
    let input = noise_with_meta(30_000);
    for format in ["wav", "aiff", "flac", "ogg"] {
        let bytes = encode(&input, format, audio_opts(format));
        let back = decode(&bytes, format);
        let (src, got) = (&audio(&input).meta, &audio(&back).meta);
        assert_eq!(got.loop_range, src.loop_range, "{format} のループ点");
        if !matches!(format, "flac" | "ogg") {
            assert_eq!(got.markers, src.markers, "{format} のマーカー");
        }
        if format == "ogg" {
            // 非可逆なので標本は変わる：読んだメタデータを元の標本に付けて書くと、シリアル番号（値のダイジェストから
            // 決まる）のほかは同じバイト列。読んだ値を書き直して読んでもループ点は同じ
            let again = encode(&with_meta_of(&input, got), format, audio_opts(format));
            assert_eq!(
                without_serial(&bytes),
                without_serial(&again),
                "{format} の書き直し（メタデータ）"
            );
            let twice = decode(&encode(&back, format, audio_opts(format)), format);
            assert_eq!(audio(&twice).meta.loop_range, src.loop_range, "{format}");
            assert_eq!(audio(&twice).frames(), audio(&input).frames(), "{format}");
        } else {
            // 書き直しても同じ
            let again = encode(&back, format, audio_opts(format));
            assert_eq!(bytes, again, "{format} の書き直し");
        }
    }
    // ループの端：先頭から、終わりまで、1 フレームだけ
    for (start, end) in [
        (0u64, 30_000u64),
        (0, 1),
        (29_999, 30_000),
        (12_345, 12_346),
    ] {
        let mut a = (**audio(&noise(48_000, 1, 30_000, 9))).clone();
        a.meta.loop_range = Some(LoopRange { start, end });
        a.meta.markers = vec![Marker {
            pos: 0,
            name: "zero".into(),
        }];
        let v = Value::Audio(Arc::new(a));
        for format in ["wav", "aiff", "flac", "ogg"] {
            let back = decode(&encode(&v, format, audio_opts(format)), format);
            assert_eq!(
                audio(&back).meta.loop_range,
                Some(LoopRange { start, end }),
                "{format} {start}..{end}"
            );
        }
    }
}

/// E20：他のツール（vorbis_rs）で書いた Ogg Vorbis の LOOPSTART・LOOPLENGTH を読む。
#[test]
fn e20_ogg_loop_comments_are_read() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("ogg-vorbis-loop.ogg");
    let bytes = std::fs::read(path).unwrap();
    let v = decode(&bytes, "ogg");
    assert_eq!(
        audio(&v).meta.loop_range,
        Some(LoopRange {
            start: 4_410,
            end: 4_410 + 22_050
        })
    );
}

/// E20：範囲の外・長さ 0・逆向きのループ点は、どの形式も書かない（WAV・AIFF はループのチャンクを、FLAC・Ogg Vorbis は
/// LOOPSTART・LOOPLENGTH を。`start < end ≤ 長さ` だけを書く）。書き出しは落ちず（逆向きでも引き算があふれない）、
/// ループ点のない入力と同じバイト列になる（Ogg Vorbis はシリアル番号と CRC を除く。シリアル番号は値のダイジェストから決まる）。
#[test]
fn e20_invalid_loop_points_are_not_written() {
    let frames = 12_000u64;
    let base = noise(44_100, 2, frames, 9);
    let plain: Vec<(&str, Vec<u8>)> = ["wav", "aiff", "flac", "ogg"]
        .into_iter()
        .map(|f| (f, encode(&base, f, audio_opts(f))))
        .collect();
    for (start, end) in [
        // 始まりが長さ以上・終わりが長さを超える
        (frames, frames + 5),
        (100, frames + 100),
        (0, frames + 1),
        (0, u64::MAX),
        // 長さ 0
        (5, 5),
        (0, 0),
        (frames, frames),
        // 逆向き
        (10, 5),
        (frames, 0),
        (u64::MAX, 0),
    ] {
        let mut a = (**audio(&base)).clone();
        a.meta.loop_range = Some(LoopRange { start, end });
        let v = Value::Audio(Arc::new(a));
        for (format, want) in &plain {
            let bytes = CodecSet::pinned()
                .encode_bytes(&v, format, &audio_opts(format), &NoMonitor)
                .unwrap_or_else(|e| panic!("{format} {start}..{end}: {e}"))
                .0;
            assert!(
                !bytes.windows(9).any(|w| w == b"LOOPSTART")
                    && !bytes.windows(10).any(|w| w == b"LOOPLENGTH"),
                "{format} {start}..{end}：LOOPSTART・LOOPLENGTH を書いた"
            );
            if *format == "ogg" {
                assert_eq!(
                    without_serial(&bytes),
                    without_serial(want),
                    "{format} {start}..{end}"
                );
            } else {
                assert_eq!(&bytes, want, "{format} {start}..{end}");
            }
            assert_eq!(
                audio(&decode(&bytes, format)).meta.loop_range,
                None,
                "{format} {start}..{end}"
            );
        }
    }
}

/// タグの別名（既知の名前と同じ名前の `vorbis:KEY`、大文字と小文字だけが違う `vorbis:KEY`、COMMENT と DESCRIPTION、
/// COPYRIGHT と小文字ちょうどの `cprt`）は、FLAC・Ogg Vorbis に先の 1 つだけを書く（既知の名前を優先。読み取りは同じ名前の
/// 2 つめ以降を捨てる）。読み取りが別の名前にする `CPRT`（`vorbis:CPRT` と読む）は消さない。書いて読んだメタデータを
/// 書き直すと同じバイト列（Ogg Vorbis は元の標本に付けて、シリアル番号と CRC を除いて比べる）。
#[test]
fn duplicate_tag_names_are_written_once() {
    for (tags, want) in [
        (
            vec![("artist", "A"), ("vorbis:ARTIST", "B")],
            vec![("artist", "A")],
        ),
        (
            vec![("vorbis:foo", "x"), ("vorbis:FOO", "y")],
            vec![("vorbis:FOO", "y")],
        ),
        (
            vec![
                ("comment", "c"),
                ("vorbis:DESCRIPTION", "d"),
                ("vorbis:description", "e"),
            ],
            vec![("comment", "c")],
        ),
        (
            vec![
                ("title", "T"),
                ("vorbis:title", "t"),
                ("vorbis:BAR", "b"),
                ("vorbis:bar", "c"),
                ("vorbis:Bar", "d"),
            ],
            vec![("title", "T"), ("vorbis:BAR", "b")],
        ),
        (
            vec![("copyright", "X"), ("vorbis:cprt", "Y")],
            vec![("copyright", "X")],
        ),
        (
            vec![
                ("copyright", "X"),
                ("vorbis:CPRT", "Z"),
                ("vorbis:cprt", "Y"),
            ],
            vec![("copyright", "X"), ("vorbis:CPRT", "Z")],
        ),
    ] {
        let mut a = (**audio(&noise(8_000, 1, 2_000, 3))).clone();
        for (k, val) in &tags {
            a.meta.tags.insert((*k).into(), (*val).into());
        }
        let v = Value::Audio(Arc::new(a));
        for format in ["flac", "ogg"] {
            let opts = if format == "ogg" {
                json!({})
            } else {
                json!({ "bits": 16 })
            };
            let bytes = encode(&v, format, opts.clone());
            let back = decode(&bytes, format);
            let mut got: Vec<(String, String)> = audio(&back)
                .meta
                .tags
                .iter()
                .filter(|(k, _)| k.as_str() != "vorbis:ENCODER")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            got.sort();
            let want: Vec<(String, String)> = want
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            assert_eq!(got, want, "{format} {tags:?}");
            let again = encode(&with_meta_of(&v, &audio(&back).meta), format, opts);
            if format == "ogg" {
                assert_eq!(without_serial(&again), without_serial(&bytes), "{tags:?}");
            } else {
                assert_eq!(again, bytes, "{format} {tags:?}");
            }
        }
    }
}

/// 読み取りの上限（4,096 バイト）より長いタグは、AIFF・FLAC・Ogg Vorbis の書き出しが文字の境目で切り詰める。書いて読み直した
/// 値を書き直すと同じバイト列になる（検証で見つけた：切り詰めずに書くと、自分の読み取りがタグを丸ごと捨てていた）。
/// Ogg Vorbis は非可逆なので、読み直したメタデータを元の標本に付けて書き直し、シリアル番号のほかを比べる。
#[test]
fn long_tags_are_clipped_when_written() {
    let mut a = (**audio(&noise(8_000, 1, 300, 5))).clone();
    // 3 バイトの文字を並べて、4,096 バイトの境目が文字の途中に来るようにする
    let long = "あ".repeat(1_400);
    a.meta.tags.insert("title".into(), long.clone());
    a.meta.tags.insert("artist".into(), "a".into());
    // 切り詰めると空白で終わる値（AIFF の読み取りは前後の空白を取るので、書くときにもそろえる。検証で見つけた）
    a.meta.tags.insert("comment".into(), "é ".repeat(2_048));
    let v = Value::Audio(Arc::new(a));
    for format in ["aiff", "flac", "ogg"] {
        let opts = if format == "ogg" {
            json!({})
        } else {
            json!({ "bits": 16 })
        };
        let bytes = encode(&v, format, opts.clone());
        let back = decode(&bytes, format);
        let title = audio(&back).meta.tags.get("title").cloned().unwrap();
        assert_eq!(title, "あ".repeat(4_096 / 3), "{format}");
        assert_eq!(
            audio(&back).meta.tags.get("artist").map(String::as_str),
            Some("a")
        );
        let comment = audio(&back).meta.tags.get("comment").cloned().unwrap();
        assert!(
            comment.len() <= 4_096 && comment.starts_with("é é"),
            "{format}"
        );
        if format == "ogg" {
            let again = encode(&with_meta_of(&v, &audio(&back).meta), format, opts);
            assert_eq!(without_serial(&again), without_serial(&bytes), "{format}");
        } else {
            assert_eq!(encode(&back, format, opts), bytes, "{format}");
        }
    }
}

/// 空の音声は FLAC に書かない（総フレーム数 0 は「不明」の意味で、読み直せない）。WAV・AIFF・Ogg Vorbis は書いて読める
/// （Ogg Vorbis は libvorbis と同じく短いブロックの音声パケット 1 つで、最後のグラニュールが 0）。
#[test]
fn empty_audio_is_refused_by_flac() {
    let empty = Value::Audio(Arc::new(
        Audio::new(8_000, vec![Arc::new(Vec::new())], AudioMeta::default()).unwrap(),
    ));
    let r = CodecSet::pinned().encode_bytes(&empty, "flac", &json!({}), &NoMonitor);
    assert!(
        matches!(r, Err(kura_engine::EngineError::Unsupported(_))),
        "{r:?}"
    );
    for format in ["wav", "aiff", "ogg"] {
        let opts = if format == "ogg" {
            json!({})
        } else {
            json!({ "bits": 16 })
        };
        let back = decode(&encode(&empty, format, opts), format);
        assert_eq!(audio(&back).frames(), 0, "{format}");
        assert_eq!(
            (audio(&back).rate(), audio(&back).channels()),
            (8_000, 1),
            "{format}"
        );
    }
}

/// 正規化は、区間ごとに大きさの違う入力（絶対ゲートをまたぐ区間や、全体が −70 LKFS より静かな入力を含む）でも、
/// 目標（−69〜0 LUFS）の ±0.1 LU に届く（ChaCha8 で作った入力）。
#[test]
fn normalize_reaches_any_target() {
    use rand_chacha::rand_core::Rng;
    let mut r = kura_engine::determinism::rng(0x4E);
    let mut checked = 0;
    for case in 0..100 {
        let n = 2 + r.next_u32() as usize % 3;
        let segments: Vec<(f64, f64)> = (0..n)
            .map(|_| {
                let secs = 0.5 + f64::from(r.next_u32() % 80) / 10.0;
                let db = -100.0 + f64::from(r.next_u32() % 95);
                (secs, db)
            })
            .collect();
        let v = ebu_sine(8_000, &[0.0], &segments);
        let target = -69.0 + f64::from(r.next_u32() % 70);
        let out = op(
            "audio.normalize",
            json!({ "target": target, "peakLimit": null }),
            v.clone(),
        );
        let m = loudness::measure(audio(&out), &NoMonitor).unwrap();
        assert!(
            (m.integrated - target).abs() <= 0.1,
            "{case}: {segments:?} を {target} に：{} LUFS",
            m.integrated
        );
        checked += 1;
    }
    assert_eq!(checked, 100);
}
