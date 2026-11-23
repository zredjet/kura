//! GIF の書き手の試験。照合の相手は前の書き手（試験だけの `codec::gif_old`。gif クレート 0.14.2）。

use std::io::Cursor;
use std::sync::Arc;

use rand_chacha::rand_core::Rng;
use serde_json::json;

use crate::codec::gif::GifCodec;
use crate::codec::gif_old::OldGif;
use crate::codec::{Decoder, Encoder};
use crate::doc::DecodeOptions;
use crate::limits::Limits;
use crate::monitor::NoMonitor;
use crate::testgen::InputGen;
use crate::value::{Frames, PixelFormat, Value};

fn decode(d: &dyn Decoder, b: &[u8]) -> Value {
    d.decode(
        &mut Cursor::new(b.to_vec()),
        "gif",
        &DecodeOptions::default(),
        &Limits::default(),
        &NoMonitor,
    )
    .unwrap()
}

fn encode(e: &dyn Encoder, v: &Value, o: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    let n = e.encode(v, "gif", o, &mut out, &NoMonitor).unwrap();
    assert_eq!(n as usize, out.len());
    out
}

/// いろいろな値（画像・フレームの列・透明・色の数）を、前の書き手と今の書き手で書くと、どちらも同じ値に読める（今の
/// 書き手のものは前の読み手（gif クレート）でも同じ値）。
#[test]
fn writes_the_same_value_as_the_gif_crate() {
    let mut rng = crate::determinism::rng(3);
    for i in 0..120 {
        let w = 1 + rng.next_u32() % 90;
        let h = 1 + rng.next_u32() % 60;
        let fmt = [
            PixelFormat::Rgba8,
            PixelFormat::Rgb8,
            PixelFormat::Indexed8,
            PixelFormat::Gray8,
            PixelFormat::GrayA8,
        ][rng.next_u32() as usize % 5];
        let seed = u64::from(rng.next_u32());
        let v = if i % 3 == 0 {
            let Value::Frames(fr) = (InputGen::NoiseFrames {
                w,
                h,
                fmt,
                count: 1 + rng.next_u32() % 4,
                seed,
            })
            .generate()
            .unwrap() else {
                panic!()
            };
            let mut fr = Arc::unwrap_or_clone(fr);
            fr.loop_count = [0u32, 2, 3, 70_000][rng.next_u32() as usize % 4];
            if fr.frames().len() == 1 && fr.loop_count == 1 {
                fr.loop_count = 0;
            }
            Value::Frames(Arc::new(
                Frames::full(
                    fr.frames()
                        .iter()
                        .map(|f| (f.px.clone(), rng.next_u32() % 3000))
                        .collect(),
                    fr.meta.clone(),
                    fr.loop_count,
                )
                .unwrap(),
            ))
        } else if i % 3 == 1 {
            (InputGen::Sprite {
                w,
                h,
                rect: crate::geom::IRect::new(1, 1, w / 2, h / 2),
            })
            .generate()
            .unwrap()
        } else {
            (InputGen::Noise { w, h, fmt, seed }).generate().unwrap()
        };
        for o in [
            json!({}),
            json!({ "alphaThreshold": 1 }),
            json!({ "alphaThreshold": 255 }),
        ] {
            let old = encode(&OldGif, &v, &o);
            let new = encode(&GifCodec, &v, &o);
            let want = decode(&OldGif, &old);
            assert!(decode(&GifCodec, &new) == want, "{i} {o}");
            assert!(decode(&OldGif, &new) == want, "{i} {o}：前の読み手");
        }
    }
}

/// 大きさと速さ（手で回す。`KURA_GIF_CORPUS=<フォルダ> cargo test --release -p kura-engine --lib gif_write_corpus --
/// --ignored --nocapture`）：コーパスの GIF を読んで値にし、前と今の書き手で書いた大きさの合計と時間を比べる。
#[test]
#[ignore]
// 手で回す確かめ：フォルダの場所（環境変数）と時間を読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn gif_write_corpus() {
    let Ok(root) = std::env::var("KURA_GIF_CORPUS") else {
        return;
    };
    let mut values = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(root)];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("gif")
                && let Ok(d) = std::fs::read(&p)
                && let Ok(v) = GifCodec.decode(
                    &mut Cursor::new(d),
                    "gif",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
            {
                values.push(v);
            }
        }
    }
    let o = json!({});
    for (who, e) in [("前", &OldGif as &dyn Encoder), ("今", &GifCodec)] {
        let s = std::time::Instant::now();
        let mut size = 0usize;
        for v in &values {
            let mut out = Vec::new();
            if e.encode(v, "gif", &o, &mut out, &NoMonitor).is_ok() {
                size += out.len();
            }
        }
        eprintln!(
            "ZZGIFW {who} {} 件 {size} B {:.1} ms",
            values.len(),
            s.elapsed().as_secs_f64() * 1e3
        );
    }
}
