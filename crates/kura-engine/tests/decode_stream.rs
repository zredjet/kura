//! 流れのデコードの受け入れテスト（M13 段 2。docs/editing.md §17 の E42・E35）。
//!
//! - streamed_decode_equals_decode：すべての音声のフィクスチャと、作り方で作った WAV・AIFF で、`decode_stream(_path)` の
//!   読み取りの記録（`value_digest` を含む）が `decode(_path)` と同じ。受けた窓を連結すると値と同じで、窓はチャンクに揃って
//!   頭から順。終わりの長さ・メタデータも値と同じ。読めないものは同じ種類の誤り。
//! - unsupported_falls_through_and_begins_again：`Unsupported` で次の段へ回ると `begin` が呼び直され、受け手は後の段の値だけを持つ。
//! - mispredicted_length_reads_the_digest_again：見込みの長さが外れたデコーダ（偽の `MispredictingDecoder`）でも、2 度目の
//!   読みで正しいダイジェストになり、呼び手の受け手には 1 度だけ流れる。
//! - fan_over_decoders_equals_analyze_many（E35）：解析の扇を流れのデコードに通した結果が `analyze_many(decode)` と同じ。

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
