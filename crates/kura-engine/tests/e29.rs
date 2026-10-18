//! E29（docs/editing.md §17）：Platform・External のコーデックを通った版は、デコードのダイジェストとコーデックの識別子が
//! 記録され、変わったら検出される。アンカーがあればアンカーから作り直す。
//!
//! Linux でも回るように、土台の偽の Platform・External のデコーダ（`testing::FakePlatformDecoder`。読むたびに値が変わる＝
//! OS の更新を写す、識別子を変える＝包みの約束の版を上げた環境、失敗する）で、記録・検出・識別子の消失・アンカーからの
//! 作り直し（描いた出力のダイジェストが記録と一致）・壊れたアンカーの検出を確かめる。同じ形式を読むデコーダが 2 つ
//! （Platform と External。値は別）ある組で、記録の識別子のデコーダで読み直すこと（段の順の先頭ではない）、照合が値の
//! ダイジェストだけで env を見ないことも確かめる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use kura_engine::codec::anchor::{self, AnchorRead};
use kura_engine::codec::recorded::{InputOrigin, Recorded, RecordedInput, Source, read_recorded};
use kura_engine::codec::{CodecEnv, CodecSet, DecodeStamp, Tier};
use kura_engine::digest::{Digest, Hasher};
use kura_engine::doc::{DecodeOptions, Dither, Graph, QUANTIZE_VERSION, Quantize};
use kura_engine::error::Mismatch;
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::testing::platform::FakeFailure;
use kura_engine::testing::{FakePlatformDecoder, render_full};
use kura_engine::value::{LoopRange, PixelFormat, Slice};
use kura_engine::{Engine, EngineError, Limits, NoMonitor, Value};
use serde_json::json;

/// 1 つの場合：形式・段・値の作り方・作り直す文書。
struct Case {
    name: &'static str,
    format: &'static str,
    tier: Tier,
    input: InputGen,
    doc: Graph,
}

fn cases() -> Vec<Case> {
    let mut flip = Graph::new_single();
    flip.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    // 音声は出力に TPDF のディザ（乱数の種は元のハッシュから）を付ける：元のハッシュの約束も見る
    let mut gain = Graph::new_single();
    gain.push_linear("audio.gain", 1, json!({ "db": -3.5 }), 0)
        .unwrap();
    gain.outputs[0].quantize = Some(Quantize {
        v: QUANTIZE_VERSION,
        bits: 16,
        dither: Dither::Tpdf,
        seed: 7,
    });
    let image = InputGen::WithMeta {
        base: Box::new(InputGen::Sheet {
            cols: 3,
            rows: 2,
            cell: 8,
        }),
        meta: MetaGen {
            dpi: Some((300, 300)),
            text: vec![
                ("Title".into(), "ドット絵".into()),
                ("".into(), "\0".into()),
            ],
            icc: Some((0..=255u8).collect()),
            exif: Some(kura_engine::codec::exif::minimal(6, false)),
            slices: vec![Slice {
                name: "顔".into(),
                rect: kura_engine::geom::IRect::new(1, 1, 4, 4),
                center: None,
                pivot: None,
            }],
            ..MetaGen::default()
        },
    };
    let frames = InputGen::NoiseFrames {
        w: 12,
        h: 9,
        fmt: PixelFormat::Indexed8,
        count: 3,
        seed: 4,
    };
    let audio = InputGen::WithMeta {
        base: Box::new(InputGen::AudioNoise {
            rate: 48_000,
            channels: 2,
            frames: 20_000,
            seed: 6,
        }),
        meta: MetaGen {
            loop_range: Some(LoopRange {
                start: 100,
                end: 19_000,
            }),
            markers: vec![(5, "attack".into()), (7_000, "".into())],
            tags: vec![
                ("title".into(), "効果音".into()),
                ("vorbis:ENCODER".into(), "Lavf".into()),
            ],
            ..MetaGen::default()
        },
    };
    vec![
        Case {
            name: "heic（ImageIO）",
            format: "heic",
            tier: Tier::Platform,
            input: image,
            doc: flip.clone(),
        },
        Case {
            name: "avif のシーケンス（ImageIO）",
            format: "avif",
            tier: Tier::Platform,
            input: frames,
            doc: flip,
        },
        Case {
            name: "opus（ffmpeg）",
            format: "opus",
            tier: Tier::External,
            input: audio,
            doc: gain,
        },
    ]
}

/// 偽の Platform・External のデコーダ。
fn fake(c: &Case, id: &'static str) -> FakePlatformDecoder {
    let formats: &'static [&'static str] = match c.format {
        "heic" => &["heic"],
        "avif" => &["avif"],
        _ => &["opus"],
    };
    FakePlatformDecoder::new(id, formats, c.input.clone()).with_tier(c.tier)
}

fn codecs_with(d: FakePlatformDecoder) -> (CodecSet, Arc<FakePlatformDecoder>) {
    let d = Arc::new(d);
    let mut set = CodecSet::pinned();
    set.add_decoder(d.clone()).unwrap();
    (set, d)
}

/// 固定の段に、偽のデコーダをいくつも足した組（段の順に並ぶ）。
fn codecs_with_all<const N: usize>(
    ds: [FakePlatformDecoder; N],
) -> (CodecSet, [Arc<FakePlatformDecoder>; N]) {
    let ds = ds.map(Arc::new);
    let mut set = CodecSet::pinned();
    for d in &ds {
        set.add_decoder(d.clone()).unwrap();
    }
    (set, ds)
}

/// 同じ形式を読む 2 つめのデコーダ（External の偽物）が読む、`c.input` とは別の値（種類は同じ）。
fn other_input(c: &Case) -> InputGen {
    match c.format {
        "heic" => InputGen::Noise {
            w: 6,
            h: 4,
            fmt: PixelFormat::Rgba8,
            seed: 99,
        },
        "avif" => InputGen::NoiseFrames {
            w: 12,
            h: 9,
            fmt: PixelFormat::Indexed8,
            count: 3,
            seed: 99,
        },
        _ => InputGen::AudioNoise {
            rate: 48_000,
            channels: 2,
            frames: 20_000,
            seed: 99,
        },
    }
}

/// External の偽物（`fake-ext/1`）：`c.format` を `other_input(c)` の値に読む。
fn fake_ext(c: &Case) -> FakePlatformDecoder {
    let formats: &'static [&'static str] = match c.format {
        "heic" => &["heic"],
        "avif" => &["avif"],
        _ => &["opus"],
    };
    FakePlatformDecoder::new("fake-ext/1", formats, other_input(c)).with_tier(Tier::External)
}

/// 元のファイル（偽のデコーダは中身を値に使わない）と、そのハッシュ。
fn original(c: &Case) -> (Vec<u8>, Digest) {
    let bytes = format!("fake {} original", c.format).into_bytes();
    let hash = Hasher::new("kura-file-v1").bytes_with_len(&bytes).finish();
    (bytes, hash)
}

fn render(engine: &Engine, c: &Case, value: Value, source: Digest) -> Digest {
    let r = render_full(engine, &c.doc, value, source, &NoMonitor).unwrap();
    r.outputs[0].digest
}

/// 最初の読み取り（版を作ったとき）：記録・描いた出力・アンカー。
struct Saved {
    rec: RecordedInput,
    out: Digest,
    anchor: Vec<u8>,
    original: Vec<u8>,
}

fn save(engine: &Engine, c: &Case, codecs: &CodecSet) -> Saved {
    let (bytes, source) = original(c);
    let (v, stamp) = codecs
        .decode(
            &mut Cursor::new(&bytes),
            c.format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
    let mut rec = RecordedInput::new(source, c.format, DecodeOptions::default(), stamp);
    let out = render(engine, c, v.clone(), source);
    let (anchor, astamp) = anchor::write_verified(
        &v,
        &rec.decoded,
        &rec.anchor_context(),
        &Limits::default(),
        &NoMonitor,
    )
    .unwrap();
    assert_eq!(astamp.value_digest, rec.decoded.value_digest);
    rec.anchor = Some(Hasher::new("kura-file-v1").bytes_with_len(&anchor).finish());
    Saved {
        rec,
        out,
        anchor,
        original: bytes,
    }
}

fn read(
    codecs: &CodecSet,
    s: &Saved,
    anchor: Option<&[u8]>,
    original: Option<&[u8]>,
) -> Result<Recorded, EngineError> {
    let mut a = anchor.map(|b| Cursor::new(b.to_vec()));
    let mut o = original.map(|b| Cursor::new(b.to_vec()));
    read_recorded(
        codecs,
        &s.rec,
        a.as_mut().map(|c| Source::Reader(c)),
        o.as_mut().map(|c| Source::Reader(c)),
        &Limits::default(),
        &NoMonitor,
    )
}

fn matched(r: Recorded) -> (kura_engine::render::InputValue, InputOrigin) {
    match r {
        Recorded::Matched { input, origin } => (input, origin),
        Recorded::Mismatched { mismatch, .. } => panic!("食い違い: {mismatch:?}"),
    }
}

/// 記録：Platform・External のデコーダで読んだ版は、識別子・段・再現しないこと・値のダイジェスト・環境を記録し、
/// アンカーが要ると分かる。記録は JSON で読み戻せる。
#[test]
fn e29_platform_reads_are_recorded() {
    for c in cases() {
        let (codecs, _) = codecs_with(fake(&c, "fake-dec/1"));
        let engine = Engine::new().unwrap();
        let s = save(&engine, &c, &codecs);
        let d = &s.rec.decoded;
        assert_eq!(
            (d.codec.as_str(), d.tier, d.reproducible),
            ("fake-dec/1", c.tier, false),
            "{}",
            c.name
        );
        assert_eq!(d.env, Some(FakePlatformDecoder::default_env()));
        assert_eq!(
            d.value_digest,
            c.input.generate().unwrap().digest().unwrap()
        );
        assert!(s.rec.needs_anchor() && anchor::needs_anchor(d));
        let json = serde_json::to_string(&s.rec).unwrap();
        assert_eq!(
            serde_json::from_str::<RecordedInput>(&json).unwrap(),
            s.rec,
            "{json}"
        );
    }
}

/// 変わったら検出：同じ識別子のデコーダが別の値を読む（OS の更新）と、失敗ではなく「値が変わった」の食い違いを、今の値を
/// 添えて返す。同じ値を読む環境では元から作り直せ、描いた出力は記録と同じ。
#[test]
fn e29_changed_reads_are_detected() {
    for c in cases() {
        let engine = Engine::new().unwrap();
        let (codecs, d) = codecs_with(fake(&c, "fake-dec/1").drifting());
        let s = save(&engine, &c, &codecs);
        assert_eq!(d.reads(), 1);
        match read(&codecs, &s, None, Some(&s.original)).unwrap() {
            Recorded::Mismatched {
                mismatch:
                    Mismatch::SourceChanged {
                        codec,
                        expected,
                        got,
                    },
                current: Some((v, stamp)),
            } => {
                assert_eq!(codec, "fake-dec/1");
                assert_eq!(expected, s.rec.decoded.value_digest);
                assert_ne!(got, expected, "{}", c.name);
                assert_eq!((v.digest().unwrap(), stamp.value_digest), (got, got));
            }
            other => panic!("{}: {other:?}", c.name),
        }
        // 食い違いは `?` でつなぐと EngineError::Mismatch（code は SourceChanged）
        let e = read(&codecs, &s, None, Some(&s.original))
            .unwrap()
            .into_matched()
            .unwrap_err();
        assert_eq!(e.code(), "SourceChanged");
        // 同じ値を読む環境（同じ OS）では元から作り直せる
        let (stable, _) = codecs_with(fake(&c, "fake-dec/1"));
        let (input, origin) = matched(read(&stable, &s, None, Some(&s.original)).unwrap());
        assert_eq!(origin, InputOrigin::Original);
        assert_eq!(input.source, s.rec.source);
        assert_eq!(render(&engine, &c, input.value, input.source), s.out);
    }
}

/// 識別子の消失：記録の識別子のデコーダがない環境（包みの約束の版を上げた・ffmpeg を消した）では「記録のデコーダがない」。
/// アンカーがあれば、デコーダがなくても作り直せる。
#[test]
fn e29_missing_decoder_is_detected_and_anchor_still_works() {
    for c in cases() {
        let engine = Engine::new().unwrap();
        let (codecs, _) = codecs_with(fake(&c, "fake-dec/1"));
        let s = save(&engine, &c, &codecs);
        let (bumped, _) = codecs_with(fake(&c, "fake-dec/2"));
        for env in [bumped, CodecSet::pinned()] {
            match read(&env, &s, None, Some(&s.original)).unwrap() {
                Recorded::Mismatched {
                    mismatch: Mismatch::DecoderGone { codec },
                    current: None,
                } => assert_eq!(codec, "fake-dec/1"),
                other => panic!("{}: {other:?}", c.name),
            }
            let (input, origin) =
                matched(read(&env, &s, Some(&s.anchor), Some(&s.original)).unwrap());
            assert_eq!(origin, InputOrigin::Anchor);
            assert_eq!(render(&engine, &c, input.value, input.source), s.out);
        }
    }
}

/// アンカーからの作り直し：読むたびに値が変わる環境でも、アンカーから読んだ値は記録と同じで、描いた出力のダイジェストも
/// 記録と同じ。元は読まない。元のハッシュ（乱数の種）は元のファイルのもので、アンカーのファイルのハッシュにすると
/// ディザの出力が変わる（だから `Recorded::Matched` の `input.source` は元のハッシュにしてある）。
#[test]
fn e29_rebuilds_from_the_anchor() {
    for c in cases() {
        let engine = Engine::new().unwrap();
        let (codecs, d) = codecs_with(fake(&c, "fake-dec/1").drifting());
        let s = save(&engine, &c, &codecs);
        let reads = d.reads();
        let (input, origin) =
            matched(read(&codecs, &s, Some(&s.anchor), Some(&s.original)).unwrap());
        assert_eq!(origin, InputOrigin::Anchor);
        assert_eq!(d.reads(), reads, "アンカーがあれば元を読まない");
        assert_eq!(input.source, s.rec.source);
        assert_eq!(input.value.digest().unwrap(), s.rec.decoded.value_digest);
        assert_eq!(
            render(&engine, &c, input.value.clone(), input.source),
            s.out,
            "{}",
            c.name
        );
        if c.format == "opus" {
            let anchor_hash = s.rec.anchor.unwrap();
            assert_ne!(
                render(&engine, &c, input.value, anchor_hash),
                s.out,
                "元のハッシュの代わりにアンカーのハッシュを種にすると、ディザの出力が変わる"
            );
        }
        // アンカーそのものも固定の段のデコーダとして読める（記録は anchor-kura-dec/1・固定・再現できる）
        let (v, stamp) = codecs
            .decode(
                &mut Cursor::new(&s.anchor),
                anchor::FORMAT,
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
            (anchor::DEC_ID, Tier::Pinned, true)
        );
        assert_eq!(stamp.value_digest, s.rec.decoded.value_digest);
        assert!(!anchor::needs_anchor(&stamp));
        assert_eq!(v.digest().unwrap(), s.rec.decoded.value_digest);
    }
}

/// 壊れたアンカーの検出：画素・ヘッダ・切り詰め・別の値・別の元のアンカーは「アンカーが使えない」。呼び手はアンカーを渡さずに
/// 元から読み直せる（同じ値を読む環境なら作り直せ、変わった環境なら「値が変わった」）。
#[test]
fn e29_broken_anchors_are_detected() {
    for c in cases() {
        let engine = Engine::new().unwrap();
        let (codecs, _) = codecs_with(fake(&c, "fake-dec/1").drifting());
        let s = save(&engine, &c, &codecs);
        let mut bad: Vec<(String, Vec<u8>)> = Vec::new();
        // 画素・標本の 1 バイト（PNG は最初の IDAT の中身、WAV は先頭から 100 バイト目＝data の中）
        let body_at = if c.format == "opus" {
            100
        } else {
            let p = s.anchor.windows(4).position(|w| w == b"IDAT").unwrap();
            p + 8
        };
        let mut b = s.anchor.clone();
        b[body_at] ^= 0x10;
        bad.push(("中身".into(), b));
        // ヘッダの 1 バイト（ダイジェストの後ろの JSON）
        let kind: &[u8] = if c.format == "opus" { b"kura" } else { b"kuRa" };
        let h = s.anchor.windows(4).position(|w| w == kind).unwrap();
        let mut b = s.anchor.clone();
        b[h + 4 + 32 + 10] ^= 1;
        bad.push(("ヘッダ".into(), b));
        // 切り詰め
        bad.push(("切り詰め".into(), s.anchor[..s.anchor.len() / 2].to_vec()));
        bad.push(("空".into(), Vec::new()));
        // 別の値のアンカー（同じ元の 2 回目の読み取り＝変わった値から作ったもの）
        let (codecs2, _) = codecs_with(fake(&c, "fake-dec/1").drifting());
        let v1 = {
            let d = fake(&c, "fake-dec/1").drifting();
            d.value_for(1).unwrap()
        };
        let other_stamp = DecodeStamp {
            value_digest: v1.digest().unwrap(),
            ..s.rec.decoded.clone()
        };
        let (other, _) = anchor::write_verified(
            &v1,
            &other_stamp,
            &s.rec.anchor_context(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
        bad.push(("別の値".into(), other));
        // 別の元のアンカー（値は同じでも、ヘッダの元のハッシュが違う）
        let v0 = c.input.generate().unwrap();
        let mut cx = s.rec.anchor_context();
        cx.source = Some(Digest([9; 32]));
        let (foreign, _) =
            anchor::write_verified(&v0, &s.rec.decoded, &cx, &Limits::default(), &NoMonitor)
                .unwrap();
        bad.push(("別の元".into(), foreign));
        for (what, b) in &bad {
            match read(&codecs2, &s, Some(b), Some(&s.original)).unwrap() {
                Recorded::Mismatched {
                    mismatch: Mismatch::AnchorBroken { .. },
                    current: None,
                } => {}
                other => panic!("{} {what}: {other:?}", c.name),
            }
        }
        // アンカーを渡さずに読み直す：変わった環境では「値が変わった」、同じ環境では作り直せる
        let changed = read(&codecs2, &s, None, Some(&s.original)).unwrap();
        // 1 回目の読み取りは安定して元の値（偽のデコーダの 0 回目）なので、新しい組では一致する
        assert!(changed.mismatch().is_none(), "{}", c.name);
        let again = read(&codecs2, &s, None, Some(&s.original)).unwrap();
        assert!(matches!(
            again.mismatch(),
            Some(Mismatch::SourceChanged { .. })
        ));
    }
}

/// パスで読む口：アンカー・元をファイルのパスで渡しても同じ。ないアンカーのファイルは「アンカーが使えない」。
#[test]
fn e29_paths_work_like_readers() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e29-paths");
    std::fs::create_dir_all(&dir).unwrap();
    for c in cases() {
        let engine = Engine::new().unwrap();
        let (codecs, _) = codecs_with(fake(&c, "fake-dec/1"));
        let s = save(&engine, &c, &codecs);
        let a = dir.join(format!("{}.anchor", c.format));
        let o = dir.join(format!("{}.orig", c.format));
        std::fs::write(&a, &s.anchor).unwrap();
        std::fs::write(&o, &s.original).unwrap();
        let limits = Limits::default();
        let r = read_recorded(
            &codecs,
            &s.rec,
            Some(Source::Path(&a)),
            None,
            &limits,
            &NoMonitor,
        )
        .unwrap();
        let (input, origin) = matched(r);
        assert_eq!(origin, InputOrigin::Anchor);
        assert_eq!(render(&engine, &c, input.value, input.source), s.out);
        let r = read_recorded(
            &codecs,
            &s.rec,
            None,
            Some(Source::Path(&o)),
            &limits,
            &NoMonitor,
        )
        .unwrap();
        assert_eq!(matched(r).1, InputOrigin::Original);
        let missing = dir.join("missing.anchor");
        let r = read_recorded(
            &codecs,
            &s.rec,
            Some(Source::Path(&missing)),
            Some(Source::Path(&o)),
            &limits,
            &NoMonitor,
        )
        .unwrap();
        assert!(matches!(r.mismatch(), Some(Mismatch::AnchorBroken { .. })));
        let AnchorRead { header, .. } = anchor::read_path(&a, &limits, &NoMonitor).unwrap();
        assert_eq!(header.decoded, s.rec.decoded);
        assert_eq!(header.format.as_deref(), Some(c.format));
    }
}

/// 同じ形式を読むデコーダが 2 つある（Platform と External。読む値は別）：元から作り直すときは、段の順の先頭ではなく
/// 記録の識別子のデコーダで読む。記録の識別子のデコーダがあってもその形式を読めなければ（同じ識別子で別の形式だけを読む
/// 環境）、「記録のデコーダがない」の食い違い（読めない `Err` ではない）。
#[test]
fn e29_rereads_with_the_recorded_decoder_not_the_first_in_tier_order() {
    for c in cases() {
        let engine = Engine::new().unwrap();
        assert_ne!(
            other_input(&c).generate().unwrap().digest().unwrap(),
            c.input.generate().unwrap().digest().unwrap()
        );
        // 版を作ったとき：Platform がこの変種を断ったので External が読んだ
        let (saving, [plat, ext]) = codecs_with_all([
            fake(&c, "fake-dec/1")
                .with_tier(Tier::Platform)
                .failing(FakeFailure::Unsupported),
            fake_ext(&c),
        ]);
        let s = save(&engine, &c, &saving);
        assert_eq!((plat.reads(), ext.reads()), (0, 1), "{}", c.name);
        assert_eq!(
            (s.rec.decoded.codec.as_str(), s.rec.decoded.tier),
            ("fake-ext/1", Tier::External),
            "{}",
            c.name
        );
        assert_eq!(
            s.rec.decoded.value_digest,
            other_input(&c).generate().unwrap().digest().unwrap()
        );
        // 作り直すとき：Platform も読める（段の順の先頭）が、記録の識別子の External で読み直す
        let (both, [plat, ext]) = codecs_with_all([
            fake(&c, "fake-dec/1").with_tier(Tier::Platform),
            fake_ext(&c),
        ]);
        assert_eq!(
            both.decoders_for(c.format)
                .iter()
                .map(|d| d.id())
                .collect::<Vec<_>>(),
            ["fake-dec/1", "fake-ext/1"]
        );
        let (input, origin) = matched(read(&both, &s, None, Some(&s.original)).unwrap());
        assert_eq!(origin, InputOrigin::Original, "{}", c.name);
        assert_eq!((plat.reads(), ext.reads()), (0, 1), "{}", c.name);
        assert_eq!(input.value.digest().unwrap(), s.rec.decoded.value_digest);
        assert_eq!(render(&engine, &c, input.value, input.source), s.out);
        // パスで読む口も同じ
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e29-recorded-id");
        std::fs::create_dir_all(&dir).unwrap();
        let o = dir.join(format!("{}.orig", c.format));
        std::fs::write(&o, &s.original).unwrap();
        let r = read_recorded(
            &both,
            &s.rec,
            None,
            Some(Source::Path(&o)),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
        assert_eq!(matched(r).1, InputOrigin::Original);
        assert_eq!((plat.reads(), ext.reads()), (0, 2), "{}", c.name);
        // 記録の識別子のデコーダが、この形式を読めない環境：段の順の先頭（Platform）で読まずに「記録のデコーダがない」
        let (moved, [plat, ext]) = codecs_with_all([
            fake(&c, "fake-dec/1").with_tier(Tier::Platform),
            FakePlatformDecoder::new("fake-ext/1", &["tiff"], other_input(&c))
                .with_tier(Tier::External),
        ]);
        assert!(moved.decoder_by_id("fake-ext/1").is_some());
        match read(&moved, &s, None, Some(&s.original)).unwrap() {
            Recorded::Mismatched {
                mismatch: Mismatch::DecoderGone { codec },
                current: None,
            } => assert_eq!(codec, "fake-ext/1"),
            other => panic!("{}: {other:?}", c.name),
        }
        assert_eq!((plat.reads(), ext.reads()), (0, 0), "{}", c.name);
        // アンカーがあれば、どちらの環境でもアンカーから作り直せる
        for env in [&both, &moved] {
            let (input, origin) =
                matched(read(env, &s, Some(&s.anchor), Some(&s.original)).unwrap());
            assert_eq!(origin, InputOrigin::Anchor);
            assert_eq!(render(&engine, &c, input.value, input.source), s.out);
        }
    }
}

/// 照合は値のダイジェストだけで、環境（env）は見ない（§9.4）：作り直す側のデコーダの env が版の記録と違っても（OS の
/// 細目の版が上がった・env を出さない）、同じ値を読めば元から作り直せる。アンカーのヘッダの env が版の記録と違っても、
/// 値が同じなら作り直せる。
#[test]
fn e29_env_differences_alone_still_match() {
    for c in cases() {
        let engine = Engine::new().unwrap();
        let (codecs, _) = codecs_with(fake(&c, "fake-dec/1"));
        let s = save(&engine, &c, &codecs);
        assert_eq!(s.rec.decoded.env, Some(FakePlatformDecoder::default_env()));
        let upgraded = CodecEnv::new("fake", "2.0").with("build", "F2");
        for env in [Some(upgraded.clone()), None] {
            let (other, d) = codecs_with(fake(&c, "fake-dec/1").with_env(env.clone()));
            match read(&other, &s, None, Some(&s.original)).unwrap() {
                Recorded::Matched { input, origin } => {
                    assert_eq!(origin, InputOrigin::Original);
                    assert_eq!(render(&engine, &c, input.value, input.source), s.out);
                }
                Recorded::Mismatched { mismatch, .. } => {
                    panic!("{} {env:?}: {mismatch:?}", c.name)
                }
            }
            assert_eq!(d.reads(), 1);
            // 値が変わったときの「今の値」の記録には、今の環境が入る
            let (drift, _) = codecs_with(fake(&c, "fake-dec/1").drifting().with_env(env.clone()));
            let _ = read(&drift, &s, None, Some(&s.original)).unwrap();
            match read(&drift, &s, None, Some(&s.original)).unwrap() {
                Recorded::Mismatched {
                    mismatch: Mismatch::SourceChanged { .. },
                    current: Some((_, stamp)),
                } => assert_eq!(stamp.env, env),
                other => panic!("{}: {other:?}", c.name),
            }
        }
        // アンカーのヘッダの env が版の記録と違う（同じ値を別の環境で読んだ記録でアンカーを作った）
        let v = c.input.generate().unwrap();
        for env in [Some(upgraded.clone()), None] {
            let stamp = DecodeStamp {
                env: env.clone(),
                ..s.rec.decoded.clone()
            };
            assert_ne!(stamp, s.rec.decoded);
            let (anchor_bytes, _) = anchor::write_verified(
                &v,
                &stamp,
                &s.rec.anchor_context(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
            let header = anchor::read(
                &mut Cursor::new(&anchor_bytes),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap()
            .header;
            assert_eq!(header.decoded.env, env);
            let (input, origin) = matched(read(&codecs, &s, Some(&anchor_bytes), None).unwrap());
            assert_eq!(origin, InputOrigin::Anchor, "{} {env:?}", c.name);
            assert_eq!(render(&engine, &c, input.value, input.source), s.out);
        }
    }
}

/// 読めないのは失敗（食い違いではない）：記録の識別子のデコーダが中身で失敗した・扱えないと答えたときは `Err`。
#[test]
fn e29_read_failures_are_errors_not_mismatches() {
    let c = cases().remove(0);
    let engine = Engine::new().unwrap();
    let (codecs, _) = codecs_with(fake(&c, "fake-dec/1"));
    let s = save(&engine, &c, &codecs);
    for f in [FakeFailure::Decode, FakeFailure::Unsupported] {
        let (failing, _) = codecs_with(fake(&c, "fake-dec/1").failing(f));
        let r = read(&failing, &s, None, Some(&s.original));
        assert!(
            matches!(
                r,
                Err(EngineError::Decode { .. } | EngineError::Unsupported(_))
            ),
            "{f:?}: {r:?}"
        );
    }
}
