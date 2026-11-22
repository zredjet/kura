//! 流れのデコードの受け入れテスト（M13 段 2。docs/editing.md §17 の E42・E35）。
//!
//! - streamed_decode_equals_decode：すべての音声のフィクスチャと、作り方で作った WAV・AIFF で、`decode_stream(_path)` の
//!   読み取りの記録（`value_digest` を含む）が `decode(_path)` と同じ。受けた窓を連結すると値と同じで、窓はチャンクに揃って
//!   頭から順。終わりの長さ・メタデータも値と同じ。読めないものは同じ種類の誤り。
//! - unsupported_falls_through_and_begins_again：`Unsupported` で次の段へ回ると `begin` が呼び直され、受け手は後の段の値だけを持つ。
//! - mispredicted_length_reads_the_digest_again：見込みの長さが外れたデコーダ（偽の `MispredictingDecoder`）でも、2 度目の
//!   読みで正しいダイジェストになり、呼び手の受け手には 1 度だけ流れる。
//! - fan_over_decoders_equals_analyze_many（E35）：解析の扇を流れのデコードに通した結果が `analyze_many(decode)` と同じ。
//! - fan_begins_again_when_a_decoder_falls_through・canceling_stops_the_fan（E35。M13 段 2 の検証の直し）：次の段へ回ると
//!   扇は作り直して後の段の値だけで解き、中止すると全部の要求が `Canceled`。
//! - staged_shapes_follow_the_stage_order：段ごとに形を読む口（`shape_path_staged`）が段の順・`after`・`prepare` を守る。
//! - big_moov_keeps_the_edit_list：moov が 64 MiB を超える m4a でも、流して読む記録・形が `decode` と同じ。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kura_engine::audio::stream::StreamEnd;
use kura_engine::codec::{CodecSet, DecodeInfo, DecodeOut, DecodeSink, Decoder, ReadSeek, Tier};
use kura_engine::doc::DecodeOptions;
use kura_engine::render::Engine;
use kura_engine::testfiles::FileGen;
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{AnalysisRequest, Dim, EngineError, Limits, Monitor, NoMonitor, Shape, Value};
use serde_json::json;

/// 窓を集める受け手（窓の揃い・順を確かめる）。
#[derive(Default)]
struct Collect {
    begins: usize,
    info: Option<DecodeInfo>,
    windows: Vec<(u64, u64)>,
    planes: Vec<Vec<f32>>,
    end: Option<StreamEnd>,
}

impl DecodeSink for Collect {
    fn begin(&mut self, info: &DecodeInfo) -> Result<(), EngineError> {
        self.begins += 1;
        self.info = Some(*info);
        self.windows.clear();
        self.planes = vec![Vec::new(); info.channels];
        self.end = None;
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        _mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let n = planes[0].len() as u64;
        assert_eq!(start % AUDIO_CHUNK as u64, 0, "窓がチャンクに揃わない");
        assert_eq!(
            self.windows.last().map_or(0, |w| w.1),
            start,
            "窓が頭から順でない"
        );
        assert!(n > 0 && n <= 16 * AUDIO_CHUNK as u64);
        self.windows.push((start, start + n));
        for (o, p) in self.planes.iter_mut().zip(planes) {
            o.extend_from_slice(p);
        }
        Ok(())
    }

    fn end(&mut self, end: &StreamEnd) -> Result<(), EngineError> {
        self.end = Some(end.clone());
        Ok(())
    }
}

fn fixtures() -> Vec<(PathBuf, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let Some(ext) = p.extension().and_then(|x| x.to_str()) else {
            continue;
        };
        let Some(f) = kura_engine::codec::format_by_ext(ext) else {
            continue;
        };
        if f.item_kind == kura_engine::codec::ItemKind::Audio {
            out.push((p.clone(), f.id.to_string()));
        }
    }
    out.sort();
    out
}

/// 窓を連結したものが値と同じか。
fn check_windows(sink: &Collect, v: &Value, what: &str) {
    let Value::Audio(a) = v else {
        panic!("{what}: 音声ではありません")
    };
    let want = a.read_planes(0, a.frames()).unwrap();
    assert_eq!(sink.planes.len(), want.len(), "{what}");
    for (g, w) in sink.planes.iter().zip(&want) {
        assert!(
            g.len() == w.len() && g.iter().zip(w).all(|(x, y)| x.to_bits() == y.to_bits()),
            "{what}：窓を連結したものが値と違う"
        );
    }
    let end = sink.end.as_ref().unwrap();
    assert_eq!(end.frames, a.frames() as u64, "{what}");
    assert_eq!(end.meta, a.meta, "{what}");
    let info = sink.info.unwrap();
    assert_eq!(
        (info.rate, info.channels),
        (a.rate(), a.channels()),
        "{what}"
    );
}

/// E42：流して読んだ記録・値は、値に読んだものと同じ（フィクスチャと作り方の WAV・AIFF）。
#[test]
fn streamed_decode_equals_decode() {
    let set = CodecSet::pinned();
    let limits = Limits::default();
    let opts = DecodeOptions::default();
    let mut read = 0;
    for (path, format) in fixtures() {
        let what = path.display().to_string();
        let want = set.decode_path(&path, &format, &opts, &limits, &NoMonitor);
        let mut sink = Collect::default();
        let got = set.decode_stream_path(&path, &format, &opts, &limits, &mut sink, &NoMonitor);
        match (want, got) {
            (Ok((v, stamp)), Ok(got)) => {
                assert_eq!(got, stamp, "{what}");
                check_windows(&sink, &v, &what);
                read += 1;
            }
            (Err(w), Err(g)) => assert_eq!(
                std::mem::discriminant(&w),
                std::mem::discriminant(&g),
                "{what}：{w} と {g}"
            ),
            (w, g) => panic!("{what}：{:?} と {:?}", w.map(|x| x.1), g),
        }
    }
    assert!(read >= 20, "{read}");
    // 作り方の WAV・AIFF（流れの窓が 2 つ以上になる長さ・多チャンネル・浮動小数・メタデータ）
    let wav = |tag, bits, channels, rate, frames, smpl, seed| FileGen::Wav {
        tag,
        bits,
        channels,
        rate,
        frames,
        extensible: channels > 2,
        smpl,
        cues: vec![(1, 5), (2, 9)],
        labels: vec![(1, "a".into())],
        info: vec![("INAM".into(), "t".into())],
        unknown_size: false,
        seed,
    };
    let gens = vec![
        wav(1, 16, 2, 8_000, 0, None, 1),
        wav(1, 16, 1, 8_000, 1, None, 2),
        wav(
            1,
            24,
            2,
            44_100,
            2 * AUDIO_CHUNK as u32 + 37,
            Some((10, 900)),
            3,
        ),
        wav(3, 32, 3, 22_050, 70_001, None, 4),
        wav(1, 8, 1, 8_000, 16 * AUDIO_CHUNK as u32 + 5, None, 5),
        FileGen::Fixture {
            path: "aiff-24.aiff".into(),
        },
    ];
    for g in gens {
        let bytes = g.bytes().unwrap();
        let format = g.format();
        let (v, stamp) = set
            .decode(&mut Cursor::new(&bytes), format, &opts, &limits, &NoMonitor)
            .unwrap();
        let mut sink = Collect::default();
        let got = set
            .decode_stream(
                &mut Cursor::new(&bytes),
                format,
                &opts,
                &limits,
                &mut sink,
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(got, stamp, "{g:?}");
        assert_eq!(sink.begins, 1);
        check_windows(&sink, &v, &format!("{g:?}"));
    }
}

/// 偽のデコーダ：`frames` フレームのランプを読む。`fail_after` があれば、そこまで足してから `Unsupported`。`expect` は
/// 見込みの長さ（外れてよい）。形は `shape`。読んだ回数を数える。
struct FakeStream {
    id: &'static str,
    frames: u64,
    shape: Dim,
    expect: u64,
    fail_after: Option<u64>,
    reads: AtomicU64,
}

impl FakeStream {
    fn value(&self) -> Value {
        let n = self.frames as usize;
        let planes = (0..2)
            .map(|c| Arc::new((0..n).map(|i| (i * 3 + c) as f32 * 1e-6).collect()))
            .collect();
        Value::Audio(Arc::new(
            kura_engine::value::Audio::new(8_000, planes, Default::default()).unwrap(),
        ))
    }
}

impl Decoder for FakeStream {
    fn id(&self) -> &'static str {
        self.id
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "fake"
    }

    fn shape(
        &self,
        _src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        _limits: &Limits,
    ) -> Result<Shape, EngineError> {
        Ok(Shape::Audio {
            rate: 8_000,
            channels: 2,
            frames: self.shape,
        })
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let shape = self.shape(src, format, opts, limits)?;
        kura_engine::codec::stream::collect(mon, |out| self.fill(out, &shape))
    }

    fn decode_stream(
        &self,
        _src: &mut (dyn ReadSeek + Send),
        _format: &str,
        _opts: &DecodeOptions,
        _limits: &Limits,
        shape: &Shape,
        out: &mut DecodeOut<'_>,
        _mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.fill(out, shape)
    }
}

impl FakeStream {
    fn fill(&self, out: &mut DecodeOut<'_>, shape: &Shape) -> Result<(), EngineError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let Shape::Audio { frames, .. } = *shape else {
            panic!()
        };
        out.begin(DecodeInfo {
            rate: 8_000,
            channels: 2,
            frames,
            expect: self.expect,
        })?;
        let v = self.value();
        let Value::Audio(a) = &v else { panic!() };
        let n = a.frames();
        let planes = a.read_planes(0, n).unwrap();
        let mut at = 0;
        while at < n {
            if let Some(f) = self.fail_after
                && at as u64 >= f
            {
                return Err(EngineError::Unsupported(format!("{}: 途中で断る", self.id)));
            }
            let e = (at + 30_011).min(n);
            let views: Vec<&[f32]> = planes.iter().map(|p| &p[at..e]).collect();
            out.push_frames(&views)?;
            at = e;
        }
        out.end(a.meta.clone())
    }
}

fn fake(
    id: &'static str,
    frames: u64,
    shape: Dim,
    expect: u64,
    fail_after: Option<u64>,
) -> Arc<FakeStream> {
    Arc::new(FakeStream {
        id,
        frames,
        shape,
        expect,
        fail_after,
        reads: AtomicU64::new(0),
    })
}

/// E42：`Unsupported` で次の段へ回ると `begin` が呼び直され、受け手は後の段の値だけを持つ。
#[test]
fn unsupported_falls_through_and_begins_again() {
    let n = 20 * AUDIO_CHUNK as u64 + 3;
    let first = fake(
        "fake-a-dec/1",
        n,
        Dim::Exact(n),
        n,
        Some(17 * AUDIO_CHUNK as u64),
    );
    let second = fake("fake-b-dec/1", n, Dim::Exact(n), n, None);
    let mut set = CodecSet::default();
    set.add_decoder(first.clone()).unwrap();
    set.add_decoder(second.clone()).unwrap();
    let mut sink = Collect::default();
    let stamp = set
        .decode_stream(
            &mut Cursor::new(Vec::new()),
            "fake",
            &DecodeOptions::default(),
            &Limits::default(),
            &mut sink,
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(stamp.codec, "fake-b-dec/1");
    assert_eq!(sink.begins, 2);
    let v = second.value();
    check_windows(&sink, &v, "後の段");
    assert_eq!(stamp.value_digest, v.digest().unwrap());
}

/// E42：見込みの長さが外れたデコーダ（長すぎる・短すぎる見込み）でも、2 度目の読みで正しいダイジェストになる。呼び手の
/// 受け手には 1 度だけ流れる。当たれば 1 度だけ読む。形の上限を超えた中身は `Decode`。
#[test]
fn mispredicted_length_reads_the_digest_again() {
    let n = 3 * AUDIO_CHUNK as u64 + 11;
    for (expect, reads) in [
        (n, 1),
        (n + 5_000, 2),
        (n - 1, 2),
        (17 * AUDIO_CHUNK as u64, 2),
    ] {
        let d = fake("fake-m-dec/1", n, Dim::AtMost(n + 70_000), expect, None);
        let mut set = CodecSet::default();
        set.add_decoder(d.clone()).unwrap();
        let mut sink = Collect::default();
        let stamp = set
            .decode_stream(
                &mut Cursor::new(Vec::new()),
                "fake",
                &DecodeOptions::default(),
                &Limits::default(),
                &mut sink,
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(stamp.value_digest, d.value().digest().unwrap(), "{expect}");
        assert_eq!(d.reads.load(Ordering::SeqCst), reads, "{expect}");
        assert_eq!(sink.begins, 1);
        check_windows(&sink, &d.value(), "見込み");
    }
    // 形の上限より長い中身
    let d = fake("fake-l-dec/1", n, Dim::AtMost(n - 1), n, None);
    let mut set = CodecSet::default();
    set.add_decoder(d).unwrap();
    let got = set.decode_stream(
        &mut Cursor::new(Vec::new()),
        "fake",
        &DecodeOptions::default(),
        &Limits::default(),
        &mut Collect::default(),
        &NoMonitor,
    );
    assert!(matches!(got, Err(EngineError::Decode { .. })), "{got:?}");
}

/// E35：解析の扇を流れのデコードに通した結果は、値に読んで `analyze_many` したものと同じ（フィクスチャ）。
#[test]
fn fan_over_decoders_equals_analyze_many() {
    let engine = Engine::new().unwrap();
    let set = CodecSet::pinned();
    let limits = Limits::default();
    let opts = DecodeOptions::default();
    let reqs: Vec<AnalysisRequest> = ["audio.loudness", "audio.loop", "audio.silence"]
        .iter()
        .map(|id| AnalysisRequest {
            id: (*id).into(),
            v: None,
            params: json!({}),
        })
        .chain([AnalysisRequest {
            id: "audio.peaks".into(),
            v: None,
            params: json!({ "finest": 256 }),
        }])
        .collect();
    let mut checked = 0;
    for (path, format) in fixtures() {
        let Ok((v, stamp)) = set.decode_path(&path, &format, &opts, &limits, &NoMonitor) else {
            continue;
        };
        let want = engine.analyze_many(&v, &reqs, &NoMonitor);
        let (shape, _) = set.shape_path(&path, &format, &opts, &limits).unwrap();
        let mut fan = engine.analyze_stream(&reqs, &shape);
        let got_stamp = set
            .decode_stream_path(&path, &format, &opts, &limits, &mut fan, &NoMonitor)
            .unwrap();
        assert_eq!(got_stamp, stamp);
        let got = fan.finish(&NoMonitor);
        for (g, w) in got.iter().zip(&want) {
            let (g, w) = (g.as_ref().unwrap(), w.as_ref().unwrap());
            assert_eq!(g.output, w.output, "{}・{}", path.display(), g.id);
        }
        checked += 1;
    }
    assert!(checked >= 20, "{checked}");
}

/// 音声の解析の全部（既定の引数）の要求。
fn all_audio_requests() -> Vec<AnalysisRequest> {
    [
        "audio.peaks",
        "audio.loudness",
        "audio.silence",
        "audio.loop",
        "audio.loudness_gating",
    ]
    .iter()
    .map(|id| AnalysisRequest {
        id: (*id).into(),
        v: None,
        params: json!({}),
    })
    .collect()
}

/// E35（M13 段 2 の検証の直し）：前の段のデコーダが途中まで流してから `Unsupported` を返し、次の段へ回ると、扇は
/// `begin` で読み始めた形に作り直し、前の試みで受けたもの（違う長さ・違う値）を捨てる。結果は後の段の値に
/// `analyze_many` したものと出力・引数まで同じ。
#[test]
fn fan_begins_again_when_a_decoder_falls_through() {
    let n = 20 * AUDIO_CHUNK as u64 + 3;
    // 前の段は長さ（＝値）も違う
    let first = fake(
        "fake-a-dec/1",
        2 * n,
        Dim::Exact(2 * n),
        2 * n,
        Some(17 * AUDIO_CHUNK as u64),
    );
    let second = fake("fake-b-dec/1", n, Dim::Exact(n), n, None);
    let mut set = CodecSet::default();
    set.add_decoder(first.clone()).unwrap();
    set.add_decoder(second.clone()).unwrap();
    let engine = Engine::new().unwrap();
    let reqs = all_audio_requests();
    let want = engine.analyze_many(&second.value(), &reqs, &NoMonitor);
    let mut fan = engine.analyze_stream(
        &reqs,
        &Shape::Audio {
            rate: 8_000,
            channels: 2,
            frames: Dim::Exact(2 * n),
        },
    );
    let stamp = set
        .decode_stream(
            &mut Cursor::new(Vec::new()),
            "fake",
            &DecodeOptions::default(),
            &Limits::default(),
            &mut fan,
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(stamp.codec, "fake-b-dec/1");
    assert_eq!(first.reads.load(Ordering::SeqCst), 1, "前の段も流した");
    let got = fan.finish(&NoMonitor);
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        let (g, w) = (g.as_ref().unwrap(), w.as_ref().unwrap());
        assert_eq!(g.output, w.output, "{}", g.id);
        assert_eq!(g.params, w.params, "{}", g.id);
    }
}

/// `at` 回目の問い合わせから中止になる見張り。
struct CancelAfter {
    asked: AtomicU64,
    at: u64,
}

impl Monitor for CancelAfter {
    fn canceled(&self) -> bool {
        self.asked.fetch_add(1, Ordering::SeqCst) >= self.at
    }
}

/// E35（M13 段 2 の検証の直し）：流れのデコードの途中で中止すると `Canceled` で止まり、扇の `finish` もどの要求も
/// `Canceled`（ほかの要求の結果を返さない）。読み終えてから `finish` の時点で中止しても同じ。
#[test]
fn canceling_stops_the_fan() {
    let n = 20 * AUDIO_CHUNK as u64 + 3;
    let d = fake("fake-c-dec/1", n, Dim::Exact(n), n, None);
    let mut set = CodecSet::default();
    set.add_decoder(d).unwrap();
    let engine = Engine::new().unwrap();
    let reqs = all_audio_requests();
    let shape = Shape::Audio {
        rate: 8_000,
        channels: 2,
        frames: Dim::Exact(n),
    };
    for at in [0u64, 1, 3, 10, 50] {
        let mon = CancelAfter {
            asked: AtomicU64::new(0),
            at,
        };
        let mut fan = engine.analyze_stream(&reqs, &shape);
        let r = set.decode_stream(
            &mut Cursor::new(Vec::new()),
            "fake",
            &DecodeOptions::default(),
            &Limits::default(),
            &mut fan,
            &mon,
        );
        assert!(matches!(r, Err(EngineError::Canceled)), "{at}: {r:?}");
        for g in fan.finish(&mon) {
            assert!(matches!(g, Err(EngineError::Canceled)), "{at}: {g:?}");
        }
    }
    let mut fan = engine.analyze_stream(&reqs, &shape);
    set.decode_stream(
        &mut Cursor::new(Vec::new()),
        "fake",
        &DecodeOptions::default(),
        &Limits::default(),
        &mut fan,
        &NoMonitor,
    )
    .unwrap();
    let mon = CancelAfter {
        asked: AtomicU64::new(0),
        at: 0,
    };
    for g in fan.finish(&mon) {
        assert!(matches!(g, Err(EngineError::Canceled)), "{g:?}");
    }
}

/// E42（M13 段 2・D5）：ファイルから形を読む口（symphonia はファイルを開いて probe し、全体を読み込まない）は、読み口から
/// 読み込んで決めた形と同じ（読めないものは同じ種類の誤り）。
#[test]
fn shapes_from_files_equal_shapes_from_readers() {
    let set = CodecSet::pinned();
    let limits = Limits::default();
    let opts = DecodeOptions::default();
    for (path, format) in fixtures() {
        let bytes = std::fs::read(&path).unwrap();
        let a = set.shape_path(&path, &format, &opts, &limits);
        let b = set.shape(&mut Cursor::new(&bytes), &format, &opts, &limits);
        match (a, b) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{}", path.display()),
            (Err(a), Err(b)) => assert_eq!(
                std::mem::discriminant(&a),
                std::mem::discriminant(&b),
                "{}：{a} と {b}",
                path.display()
            ),
            (a, b) => panic!("{}：{a:?} と {b:?}", path.display()),
        }
    }
}

/// 段ごとの形（M13 段 2 の検証の直し）：`shape_path_staged` は `shape_path` と同じ段・同じ形を返し、試す段ごとに形を読む前に
/// `prepare` を呼ぶ（m4a の AAC は、ALAC の段が断ってから AAC の段）。`after` の段より後ろから試し、`prepare` の誤りは
/// そのまま返す。流して読む見積もりは、その段の `stream_work_bytes` ＋ 頭で数えた保持の量。
#[test]
fn staged_shapes_follow_the_stage_order() {
    let set = CodecSet::pinned();
    let limits = Limits::default();
    let opts = DecodeOptions::default();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/aac-lc-ffmpeg.m4a");
    let len = std::fs::metadata(&path).unwrap().len();
    let (want, want_id) = set.shape_path(&path, "m4a", &opts, &limits).unwrap();
    let mut seen = Vec::new();
    let (shape, stage) = set
        .shape_path_staged(&path, "m4a", &opts, &limits, &NoMonitor, None, &mut |s| {
            seen.push(*s);
            Ok(())
        })
        .unwrap();
    assert_eq!((shape, stage.id), (want, want_id));
    assert_eq!(
        seen.iter().map(|s| s.id).collect::<Vec<_>>(),
        ["m4a-alac-symphonia-dec/1", "m4a-aac-kura-dec/1"]
    );
    assert_eq!(seen[1], stage);
    assert!(stage.held.open > 0 && stage.shape_bytes == stage.held.open);
    let d = set
        .decoders_for("m4a")
        .into_iter()
        .find(|d| d.id() == stage.id)
        .unwrap();
    assert_eq!(
        set.stage_stream_work_bytes(&stage, "m4a", &shape, len),
        d.stream_work_bytes("m4a", &shape, len) + stage.held.open + stage.held.decode
    );
    // 前の段のあとから：ALAC のあとは AAC、AAC のあとはない
    let after = |id| {
        set.shape_path_staged(
            &path,
            "m4a",
            &opts,
            &limits,
            &NoMonitor,
            Some(id),
            &mut |_| Ok(()),
        )
    };
    assert_eq!(after("m4a-alac-symphonia-dec/1").unwrap().1.id, stage.id);
    assert!(matches!(
        after("m4a-aac-kura-dec/1"),
        Err(EngineError::Unsupported(_))
    ));
    // prepare の誤りはそのまま
    let refused =
        set.shape_path_staged(&path, "m4a", &opts, &limits, &NoMonitor, None, &mut |_| {
            Err(EngineError::Limit("予算".into()))
        });
    assert!(matches!(refused, Err(EngineError::Limit(m)) if m == "予算"));
}

/// E42（M13 段 2 の検証の直し）：moov が 64 MiB を超える m4a（ffmpeg の AAC。編集リストあり・iTunSMPB なし）でも、流して
/// 読んだ記録（`value_digest`）が `decode_path` と同じで、ファイルから読む形が読み口から読む形と同じ（前の版はファイルから
/// 読むときだけ大きな moov の編集リストを黙って捨て、priming を切らなかった）。フィクスチャの moov の中身の後ろに
/// 65 MiB の free の箱を足して作る（moov が最後なので、チャンクの位置は変わらない）。
#[test]
fn big_moov_keeps_the_edit_list() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/aac-lc-ffmpeg.m4a");
    let b = std::fs::read(&src).unwrap();
    let mut at = 0usize;
    let mut moov = None;
    while at + 8 <= b.len() {
        let s = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        if &b[at + 4..at + 8] == b"moov" {
            moov = Some((at, s));
        }
        at += s;
    }
    let (m, ms) = moov.unwrap();
    assert_eq!(m + ms, b.len(), "moov が最後でない");
    let pad = 65usize << 20;
    let mut f = b.clone();
    f[m..m + 4].copy_from_slice(&u32::try_from(ms + 8 + pad).unwrap().to_be_bytes());
    f.extend_from_slice(&u32::try_from(8 + pad).unwrap().to_be_bytes());
    f.extend_from_slice(b"free");
    f.resize(f.len() + pad, 0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e42-big-moov");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big-moov.m4a");
    std::fs::write(&path, &f).unwrap();
    let set = CodecSet::pinned();
    let limits = Limits::default();
    let opts = DecodeOptions::default();
    let (orig, orig_stamp) = set
        .decode_path(&src, "m4a", &opts, &limits, &NoMonitor)
        .unwrap();
    let (v, stamp) = set
        .decode_path(&path, "m4a", &opts, &limits, &NoMonitor)
        .unwrap();
    assert_eq!(stamp, orig_stamp, "free の箱で値は変わらない");
    assert_eq!(v.shape(), orig.shape());
    let mut sink = Collect::default();
    let streamed = set
        .decode_stream_path(&path, "m4a", &opts, &limits, &mut sink, &NoMonitor)
        .unwrap();
    assert_eq!(streamed, stamp, "流して読んだ記録が decode と違う");
    check_windows(&sink, &v, "moov の大きな m4a");
    let mut sink = Collect::default();
    let from_reader = set
        .decode_stream(
            &mut Cursor::new(&f),
            "m4a",
            &opts,
            &limits,
            &mut sink,
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(from_reader, stamp);
    let by_path = set.shape_path(&path, "m4a", &opts, &limits).unwrap();
    let by_reader = set
        .shape(&mut Cursor::new(&f), "m4a", &opts, &limits)
        .unwrap();
    assert_eq!(by_path, by_reader);
    std::fs::remove_file(&path).unwrap();
}

/// E42（D5）：流れの窓が 2 つ以上になる FLAC を読み口から流して読むと、記録・値が `decode` と同じ。STREAMINFO の MD5 が
/// 違えば、流して読んでも `Decode`（MD5 は最後に照らすので、受け手は窓を受けても終わりを受けない）。
#[test]
fn long_flac_streams_and_md5_is_checked_last() {
    let set = CodecSet::pinned();
    let limits = Limits::default();
    let opts = DecodeOptions::default();
    let v = kura_engine::testgen::InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 17 * AUDIO_CHUNK as u64 + 5,
        seed: 42,
    }
    .generate()
    .unwrap();
    let (bytes, _) = set
        .encode_bytes(&v, "flac", &json!({ "bits": 16 }), &NoMonitor)
        .unwrap();
    let (want, stamp) = set
        .decode(&mut Cursor::new(&bytes), "flac", &opts, &limits, &NoMonitor)
        .unwrap();
    let mut sink = Collect::default();
    let got = set
        .decode_stream(
            &mut Cursor::new(bytes.clone()),
            "flac",
            &opts,
            &limits,
            &mut sink,
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(got, stamp);
    assert_eq!(sink.windows.len(), 2);
    check_windows(&sink, &want, "長い FLAC");
    // MD5（STREAMINFO の最後の 16 バイト：fLaC 4 ＋ 箱の頭 4 ＋ 18）を壊す
    assert_eq!(&bytes[..4], b"fLaC");
    let mut broken = bytes;
    broken[30] ^= 0x5a;
    let mut sink = Collect::default();
    let got = set.decode_stream(
        &mut Cursor::new(broken.clone()),
        "flac",
        &opts,
        &limits,
        &mut sink,
        &NoMonitor,
    );
    assert!(matches!(got, Err(EngineError::Decode { .. })), "{got:?}");
    assert!(!sink.windows.is_empty() && sink.end.is_none());
    assert!(matches!(
        set.decode(
            &mut Cursor::new(&broken),
            "flac",
            &opts,
            &limits,
            &NoMonitor
        ),
        Err(EngineError::Decode { .. })
    ));
}

/// E42（D6）：Platform の音声（偽の AudioToolbox。入力はメモリ、塊ごとの読みをそのまま流す）と MP3 も流して読める。記録・
/// 窓・値は `decode` と同じ。形を読むのにファイル全体を読むデコーダは、その分を `shape_work_bytes` に申告する。
#[test]
fn platform_and_mp3_stream() {
    use kura_engine::codec::platform::PlatformAudioDecoder;
    use kura_engine::testing::platform::{FakeAudio, FakeAudioBackend};
    let n = 17 * AUDIO_CHUNK + 9;
    let planes: Vec<Vec<f32>> = (0..2)
        .map(|c| (0..n).map(|i| ((i * 5 + c) % 997) as f32 * 1e-3).collect())
        .collect();
    let mut fake = FakeAudio::new(44_100, planes);
    fake.chunk = 10_007;
    let mut set = CodecSet::default();
    set.add_decoder(Arc::new(PlatformAudioDecoder::new(FakeAudioBackend::new(
        fake,
    ))))
    .unwrap();
    let limits = Limits::default();
    let opts = DecodeOptions::default();
    let bytes = vec![7u8; 1_000];
    let (v, stamp) = set
        .decode(&mut Cursor::new(&bytes), "m4a", &opts, &limits, &NoMonitor)
        .unwrap();
    let mut sink = Collect::default();
    let got = set
        .decode_stream(
            &mut Cursor::new(bytes.clone()),
            "m4a",
            &opts,
            &limits,
            &mut sink,
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(got, stamp);
    assert_eq!(sink.windows.len(), 2);
    check_windows(&sink, &v, "Platform");
    assert_eq!(set.shape_work_bytes("m4a", 1_000), 1_000);
    // MP3（フィクスチャ。形は上限で、見込みは上限なので外れうる）
    let pinned = CodecSet::pinned();
    assert_eq!(pinned.shape_work_bytes("mp3", 5_000), 5_000);
    assert_eq!(pinned.shape_work_bytes("flac", 5_000), 0);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    for name in ["mp3-lame.mp3", "mp3-notag.mp3", "mp3-free.mp3"] {
        let path = dir.join(name);
        let (v, stamp) = pinned
            .decode_path(&path, "mp3", &opts, &limits, &NoMonitor)
            .unwrap();
        let mut sink = Collect::default();
        let got = pinned
            .decode_stream_path(&path, "mp3", &opts, &limits, &mut sink, &NoMonitor)
            .unwrap();
        assert_eq!(got, stamp, "{name}");
        check_windows(&sink, &v, name);
    }
}
