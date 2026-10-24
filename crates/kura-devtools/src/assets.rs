//! 形式の見本（`assets --out <dir>`。M11 の決定の「形式の網羅」・critic 10）。
//!
//! 形式の表（kura-engine の `FORMATS`）の画像・フォント・3D の行ごとに 1 つ以上と、動画・音声の見本を書く。M11 の macOS の
//! テスト（下の `every_image_font_and_model_format_gets_a_thumbnail_and_probe`）が、これを新しいライブラリに取り込んで、
//! 行ごとにサムネイルと情報ができることを確かめる。UI の確かめ（`pnpm tauri dev` で取り込む）にも使う。
//!
//! - 画像：アルファ付きの PNG、ドット絵（16×16 と 4 倍）、aseprite（タグ付き）、アニメの WebP・GIF・APNG、PSD・PSB、文字のある
//!   SVG、JPEG は kura-engine の書き手と `testfiles::FileGen` で作る。アルファ付きの HEIC・AVIF・TIFF・BMP は `sips` で PNG から
//!   作る（HEIF は `sips` が書けないので、HEIC を `.heif` の名前で置く。中身は HEIF の一種）。
//! - フォント：システムのフォントの写し（ttf・otf・ttc）と、それを包み直した WOFF（表を圧縮しない 1.0）・WOFF2（表を変換せず、
//!   Brotli の「圧縮しない塊」で包む）。リポジトリには入れない。
//! - 3D：kura-engine の見本の glb（立方体・テクスチャの四角）。
//! - 動画・音声：ffmpeg（mp4・mp3・aac・flac・opus・aiff）と kura-engine の書き手（10 秒以下の効果音の WAV、ループ点付きの
//!   Ogg Vorbis、周期の倍数でループするサイン波の WAV（smpl））。ffmpeg がなければ ffmpeg の分は作らない。
//!
//! 作れなかったものは理由を返す（`sips` が HEIC を書けない CI の仮想マシンなど）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_engine::NoMonitor;
use kura_engine::codec::CodecSet;
use kura_engine::geom::Size;
use kura_engine::testfiles::{AseCel, AseFrame, AseLayer, FileGen, PsdGen};
use kura_engine::value::{
    Audio, AudioMeta, Frame, Frames, Image, ImageMeta, LoopRange, PixelFormat, Raster,
};
use kura_engine::{Value, math};

/// 書いた見本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// ファイルの名前（拡張子は形式の表のもの）。
    pub name: String,
    /// サムネイルが PNG になるはずか（透明のある画像・フォント・3D）。
    pub png: bool,
}

/// 作れなかった見本と理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub name: String,
    pub reason: String,
}

/// `assets --out <dir> [--bench <N>]` を実行する。`--bench` は、形式の見本の代わりに、サムネイルのジョブを測る PNG の多い
/// 組（`bench_set`）を書く。
pub fn run(args: &[String]) -> Result<(), String> {
    let mut out = None;
    let mut bench = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--bench" => bench = it.next().and_then(|n| n.parse::<u32>().ok()),
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    let out = out.ok_or("--out に見本を書くフォルダを指定してください")?;
    if let Some(n) = bench {
        let count = bench_set(&out, n)?;
        tracing::info!(path = %out.display(), count, "サムネイルのジョブを測る組を書きました");
        return Ok(());
    }
    let (samples, skipped) = build(&out)?;
    for s in &samples {
        tracing::info!(name = %s.name, png = s.png, "見本");
    }
    for s in &skipped {
        tracing::warn!(name = %s.name, reason = %s.reason, "作れなかった見本");
    }
    tracing::info!(
        path = %out.display(),
        count = samples.len(),
        skipped = skipped.len(),
        "形式の見本を書きました"
    );
    Ok(())
}

/// サムネイルのジョブを測る PNG の多い組（NFR-04 の判定の範囲。M11 の決定 2）：大きなアルファ付きの PNG（2048²）と大きな
/// 不透明な PNG（3000×2000）を `n` 枚ずつ、小さなスプライト（64²）を `4n` 枚、JPEG（1200×800）を `n` 枚。取り込みは中身で
/// 重複を除くので、1 枚ずつ色を変える。書いた数を返す。
pub fn bench_set(out: &Path, n: u32) -> Result<usize, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let codecs = CodecSet::pinned();
    let encode = |v: &Value, format: &str| {
        codecs
            .encode_bytes(v, format, &serde_json::json!({}), &NoMonitor)
            .map(|(b, _)| b)
            .map_err(|e| format!("{format}: {e}"))
    };
    let write = |name: String, bytes: Vec<u8>| {
        std::fs::write(out.join(&name), bytes).map_err(|e| format!("{name}: {e}"))
    };
    let tint = |rows: &mut [u8], bpp: usize, i: u32| {
        for p in rows.chunks_exact_mut(bpp) {
            p[2] = p[2].wrapping_add(i as u8);
        }
    };
    let mut count = 0;
    for i in 0..n {
        let mut alpha = soft_circle(2048, 2048);
        tint(&mut alpha, 4, i);
        write(
            format!("big-alpha-{i:03}.png"),
            encode(&image(raster(2048, 2048, &alpha)?), "png")?,
        )?;
        let mut opaque = landscape(3000, 2000);
        tint(&mut opaque, 3, i);
        let rgb = |rows: &[u8], w, h| {
            Raster::from_rows(w, h, PixelFormat::Rgb8, None, rows).map_err(|e| e.to_string())
        };
        write(
            format!("big-opaque-{i:03}.png"),
            encode(&image(rgb(&opaque, 3000, 2000)?), "png")?,
        )?;
        let mut photo = landscape(1200, 800);
        tint(&mut photo, 3, i);
        write(
            format!("photo-{i:03}.jpg"),
            encode(&image(rgb(&photo, 1200, 800)?), "jpeg")?,
        )?;
        for k in 0..4 {
            let mut sprite = pixel_art(4);
            tint(&mut sprite, 4, i * 4 + k);
            write(
                format!("sprite-{i:03}-{k}.png"),
                encode(&image(raster(64, 64, &sprite)?), "png")?,
            )?;
        }
        count += 7;
    }
    Ok(count)
}

/// 見本を `out` に書く（なければ作る）。
pub fn build(out: &Path) -> Result<(Vec<Sample>, Vec<Skipped>), String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut b = Builder {
        out: out.to_path_buf(),
        codecs: CodecSet::pinned(),
        samples: Vec::new(),
        skipped: Vec::new(),
    };
    b.images()?;
    b.fonts();
    b.models()?;
    b.sounds()?;
    b.ffmpeg();
    Ok((b.samples, b.skipped))
}

struct Builder {
    out: PathBuf,
    codecs: CodecSet,
    samples: Vec<Sample>,
    skipped: Vec<Skipped>,
}

/// RGBA8 の画像。
fn raster(w: u32, h: u32, rows: &[u8]) -> Result<Raster, String> {
    Raster::from_rows(w, h, PixelFormat::Rgba8, None, rows).map_err(|e| e.to_string())
}

fn image(px: Raster) -> Value {
    Value::Image(Arc::new(Image {
        px,
        meta: ImageMeta::default(),
    }))
}

/// 透明の地に、ふちのやわらかい円（半透明の画素がある）。
fn soft_circle(w: u32, h: u32) -> Vec<u8> {
    let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let r = f64::from(w.min(h)) * 0.42;
    let mut rows = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let d = math::hypot(f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
            let a = ((r - d) / 6.0).clamp(0.0, 1.0);
            rows.extend_from_slice(&[
                (60.0 + 180.0 * f64::from(x) / f64::from(w)) as u8,
                (90.0 + 120.0 * f64::from(y) / f64::from(h)) as u8,
                220,
                (a * 255.0).round() as u8,
            ]);
        }
    }
    rows
}

/// 16×16 のドット絵（透明の地に、8 色以下）。
const PIXEL_ART: [&str; 16] = [
    "................",
    ".....######.....",
    "...##oooooo##...",
    "..#oooooooooo#..",
    ".#oooooooooooo#.",
    ".#ooWWooooWWoo#.",
    "#oooWKooooWKooo#",
    "#oooooooooooooo#",
    "#ooooooRRoooooo#",
    "#oooooRRRRooooo#",
    ".#oooooooooooo#.",
    ".#oooooooooooo#.",
    "..##oooooooo##..",
    "....########....",
    "................",
    "................",
];

fn pixel_rgba(c: u8) -> [u8; 4] {
    match c {
        b'#' => [24, 32, 64, 255],
        b'o' => [96, 200, 120, 255],
        b'W' => [250, 250, 250, 255],
        b'K' => [10, 10, 10, 255],
        b'R' => [220, 70, 80, 255],
        _ => [0, 0, 0, 0],
    }
}

/// ドット絵を `k` 倍（最近傍）にした RGBA8 の行。
fn pixel_art(k: usize) -> Vec<u8> {
    let mut rows = Vec::new();
    for line in PIXEL_ART {
        for _ in 0..k {
            for &c in line.as_bytes() {
                for _ in 0..k {
                    rows.extend_from_slice(&pixel_rgba(c));
                }
            }
        }
    }
    rows
}

/// 弾む玉のアニメ（透明の地。`n` コマ、64×64）。
fn bouncing(n: u32) -> Result<Vec<Raster>, String> {
    (0..n)
        .map(|i| {
            let (w, h) = (64u32, 64u32);
            let t = f64::from(i) / f64::from(n);
            let cy = 16.0 + 32.0 * (1.0 - (2.0 * t - 1.0).abs());
            let mut rows = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    let d = math::hypot(f64::from(x) + 0.5 - 32.0, f64::from(y) + 0.5 - cy);
                    let inside = d <= 12.0;
                    rows.extend_from_slice(&if inside {
                        [240, (120 + i * 12) as u8, 60, 255]
                    } else {
                        [0, 0, 0, 0]
                    });
                }
            }
            raster(w, h, &rows)
        })
        .collect()
}

/// 不透明な写真の代わり（空と地面のグラデーション）。
fn landscape(w: u32, h: u32) -> Vec<u8> {
    let mut rows = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let sky = y < h * 3 / 5;
            let fx = f64::from(x) / f64::from(w);
            let fy = f64::from(y) / f64::from(h);
            rows.extend_from_slice(&if sky {
                [(80.0 + 80.0 * fy) as u8, (140.0 + 60.0 * fy) as u8, 230]
            } else {
                [(60.0 + 40.0 * fx) as u8, (120.0 + 50.0 * fy) as u8, 70]
            });
        }
    }
    rows
}

impl Builder {
    fn path(&self, name: &str) -> PathBuf {
        self.out.join(name)
    }

    fn write(&mut self, name: &str, bytes: &[u8], png: bool) -> Result<(), String> {
        std::fs::write(self.path(name), bytes).map_err(|e| format!("{name}: {e}"))?;
        self.samples.push(Sample {
            name: name.to_string(),
            png,
        });
        Ok(())
    }

    fn encode(&self, v: &Value, format: &str) -> Result<Vec<u8>, String> {
        self.codecs
            .encode_bytes(v, format, &serde_json::json!({}), &NoMonitor)
            .map(|(bytes, _)| bytes)
            .map_err(|e| format!("{format}: {e}"))
    }

    fn skip(&mut self, name: &str, reason: impl Into<String>) {
        self.skipped.push(Skipped {
            name: name.to_string(),
            reason: reason.into(),
        });
    }

    fn images(&mut self) -> Result<(), String> {
        // アルファ付きの PNG（半透明のふち）
        let alpha = image(raster(480, 320, &soft_circle(480, 320))?);
        let png = self.encode(&alpha, "png")?;
        self.write("alpha.png", &png, true)?;
        // ドット絵（等倍と 4 倍）
        let small = image(raster(16, 16, &pixel_art(1))?);
        self.write("pixel-16.png", &self.encode(&small, "png")?, true)?;
        let big = image(raster(64, 64, &pixel_art(4))?);
        self.write("pixel-16x4.png", &self.encode(&big, "png")?, true)?;
        // JPEG（不透明）
        let photo = Value::Image(Arc::new(Image {
            px: Raster::from_rows(1200, 800, PixelFormat::Rgb8, None, &landscape(1200, 800))
                .map_err(|e| e.to_string())?,
            meta: ImageMeta::default(),
        }));
        self.write("photo.jpg", &self.encode(&photo, "jpeg")?, false)?;
        // アニメ：GIF・APNG（kura の書き手）と WebP（静止画の VP8L を ANMF に包む）
        let frames = bouncing(8)?;
        let anim = Value::Frames(Arc::new(
            Frames::new(
                frames
                    .iter()
                    .map(|px| Frame::full(px.clone(), 80))
                    .collect(),
                Size { w: 64, h: 64 },
                ImageMeta::default(),
                0,
            )
            .map_err(|e| e.to_string())?,
        ));
        self.write("anim.gif", &self.encode(&anim, "gif")?, true)?;
        self.write("anim.apng", &self.encode(&anim, "png")?, true)?;
        let stills = frames
            .iter()
            .map(|px| self.encode(&image(px.clone()), "webp"))
            .collect::<Result<Vec<_>, _>>()?;
        self.write("anim.webp", &animated_webp(&stills, 64, 64, 80)?, true)?;
        // aseprite（タグ付き。画素は位置とフレームから決めたもの）
        let ase = FileGen::Aseprite {
            w: 32,
            h: 32,
            depth: 32,
            flags: 1,
            transparent: 0,
            palette_len: 8,
            layers: vec![AseLayer {
                flags: 1,
                kind: 0,
                level: 0,
                blend: 0,
                opacity: 255,
            }],
            frames: (0..4)
                .map(|i| AseFrame {
                    duration: 100,
                    cels: vec![AseCel {
                        layer: 0,
                        x: 4 + i * 4,
                        y: 8,
                        opacity: 255,
                        z: 0,
                        kind: 2,
                        w: 12,
                        h: 16,
                        link: 0,
                        flip: false,
                    }],
                })
                .collect(),
            tileset: None,
            tags: vec![("idle".into(), 0, 1), ("walk".into(), 2, 3)],
            slices: vec![],
            seed: 7,
            opaque_palette: false,
        };
        self.write(
            "sprite.aseprite",
            &ase.bytes().map_err(|e| e.to_string())?,
            true,
        )?;
        // PSD・PSB（アルファのチャンネル付き。Photoshop のように白で下塗り）
        // 合成の透明は、PSD は全体の追加の情報の Mtrn、PSB は層の数が負で示す
        for (name, psb, w, h, layers, mtrn) in [
            ("layers.psd", false, 96, 64, 0, true),
            ("big.psb", true, 160, 120, -1, false),
        ] {
            let g = FileGen::Psd(PsdGen {
                w,
                h,
                mode: 3,
                depth: 8,
                extra_channels: 1,
                psb,
                compression: 1,
                layer_count: layers,
                merged_key: mtrn,
                matted: true,
                real_merged: None,
                transparent_index: None,
                resolution: None,
                exif: None,
                seed: 11,
            });
            self.write(name, &g.bytes().map_err(|e| e.to_string())?, true)?;
        }
        // 文字のある SVG（システムのフォントで描く）
        self.write("text.svg", SVG.as_bytes(), true)?;
        // ImageIO の道の形式（sips で PNG から。アルファを保つ）。取り込みは中身で重複を除くので、HEIF は HEIC と別の絵から作る
        for (name, src, format) in [
            ("alpha.heic", "alpha.png", "heic"),
            ("alpha.heif", "pixel-16x4.png", "heic"),
            ("alpha.avif", "alpha.png", "avif"),
            ("alpha.tiff", "alpha.png", "tiff"),
            ("alpha.bmp", "alpha.png", "bmp"),
        ] {
            match sips(&self.path(src), format, &self.path(name)) {
                Ok(()) => self.samples.push(Sample {
                    name: name.to_string(),
                    png: true,
                }),
                Err(reason) => self.skip(name, reason),
            }
        }
        Ok(())
    }

    fn fonts(&mut self) {
        let ttf = system_font(&["Geneva.ttf", "Supplemental/Arial.ttf"], "ttf");
        let otf = system_font(&["Supplemental/STIXGeneral.otf"], "otf");
        let ttc = system_font(&["Courier.ttc", "Helvetica.ttc"], "ttc");
        for (name, src) in [("font.ttf", &ttf), ("font.otf", &otf), ("font.ttc", &ttc)] {
            match src.as_ref().map(std::fs::read) {
                Some(Ok(bytes)) => {
                    if let Err(e) = self.write(name, &bytes, true) {
                        self.skip(name, e);
                    }
                }
                _ => self.skip(name, "システムのフォントが見つかりません"),
            }
        }
        // WOFF は ttf を、WOFF2 は CFF の otf（glyf を変換しなくてよい）を包み直す
        let wrapped = [
            (
                "font.woff",
                &ttf,
                woff1 as fn(&[u8]) -> Result<Vec<u8>, String>,
            ),
            ("font.woff2", &otf, woff2),
        ];
        for (name, src, wrap) in wrapped {
            let bytes = src
                .as_ref()
                .ok_or_else(|| "システムのフォントが見つかりません".to_string())
                .and_then(|p| std::fs::read(p).map_err(|e| e.to_string()))
                .and_then(|b| wrap(&b));
            match bytes {
                Ok(bytes) => {
                    if let Err(e) = self.write(name, &bytes, true) {
                        self.skip(name, e);
                    }
                }
                Err(e) => self.skip(name, e),
            }
        }
    }

    fn models(&mut self) -> Result<(), String> {
        let samples = kura_engine::model::sample_glbs().map_err(|e| e.to_string())?;
        for (name, file) in [("cube", "cube.glb"), ("textured_quad", "textured.glb")] {
            let bytes = samples
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| format!("glb の見本 {name} がありません"))?;
            self.write(file, &bytes, true)?;
        }
        Ok(())
    }

    fn sounds(&mut self) -> Result<(), String> {
        let rate = 44_100u32;
        // 10 秒以下の効果音（0.25 秒の減衰する音）
        let n = rate as usize / 4;
        let blip: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                (math::sin(2.0 * std::f64::consts::PI * 880.0 * t) * math::exp(-t * 14.0) * 0.6)
                    as f32
            })
            .collect();
        let blip = audio(rate, vec![blip.clone(), blip], None)?;
        self.write("blip.wav", &self.encode(&blip, "wav")?, false)?;
        // 周期の倍数でループするサイン波（441 Hz は 100 フレームで 1 周期。ループは 1 万〜5 万フレーム）
        let sine: Vec<f32> = (0..2 * rate as usize)
            .map(|i| (math::sin(2.0 * std::f64::consts::PI * i as f64 / 100.0) * 0.5) as f32)
            .collect();
        let looped = audio(
            rate,
            vec![sine.clone(), sine.clone()],
            Some(LoopRange {
                start: 10_000,
                end: 50_000,
            }),
        )?;
        self.write("sine-loop.wav", &self.encode(&looped, "wav")?, false)?;
        // ループ点付きの Ogg Vorbis（LOOPSTART・LOOPLENGTH）
        self.write("loop.ogg", &self.encode(&looped, "ogg")?, false)?;
        Ok(())
    }

    fn ffmpeg(&mut self) {
        let Some(ffmpeg) = kura_core::tools::find("ffmpeg") else {
            for name in [
                "clip.mp4",
                "tone.mp3",
                "tone.aac",
                "tone.flac",
                "tone.opus",
                "tone.aiff",
            ] {
                self.skip(name, "ffmpeg がありません");
            }
            return;
        };
        let video = [
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=24:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ];
        let tone = |codec: &'static str| -> Vec<&'static str> {
            vec![
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:a",
                codec,
            ]
        };
        let jobs: [(&str, Vec<&str>); 6] = [
            ("clip.mp4", video.to_vec()),
            ("tone.mp3", tone("libmp3lame")),
            ("tone.aac", tone("aac")),
            ("tone.flac", tone("flac")),
            ("tone.opus", tone("libopus")),
            ("tone.aiff", tone("pcm_s16be")),
        ];
        for (name, args) in jobs {
            let out = self.path(name);
            let status = Command::new(&ffmpeg)
                .args(["-v", "error", "-y"])
                .args(&args)
                .arg(&out)
                .status();
            match status {
                Ok(s) if s.success() && out.is_file() => self.samples.push(Sample {
                    name: name.to_string(),
                    png: false,
                }),
                Ok(s) => self.skip(name, format!("ffmpeg が失敗しました（{s}）")),
                Err(e) => self.skip(name, e.to_string()),
            }
        }
    }
}

fn audio(rate: u32, planes: Vec<Vec<f32>>, loop_range: Option<LoopRange>) -> Result<Value, String> {
    Ok(Value::Audio(Arc::new(
        Audio::new(
            rate,
            planes.into_iter().map(Arc::new).collect(),
            AudioMeta {
                loop_range,
                ..AudioMeta::default()
            },
        )
        .map_err(|e| e.to_string())?,
    )))
}

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="320" height="200" viewBox="0 0 320 200">
  <rect x="10" y="10" width="300" height="180" rx="24" fill="#2b6cb0" fill-opacity="0.85"/>
  <circle cx="78" cy="100" r="48" fill="#f6ad55"/>
  <text x="140" y="116" font-family="Helvetica" font-size="44" fill="#ffffff">Kura</text>
</svg>
"##;

/// `sips` を待つ上限。CI の仮想マシンでは HEIC・AVIF を書くときに失敗せず終わらないことがある（2026-10 の macOS の
/// CI で 3 時間止まった）。超えたら止めて、作れなかった見本として飛ばす。
const SIPS_TIMEOUT: Duration = Duration::from_secs(20);

/// `sips` で `src` を `format` にする（[`SIPS_TIMEOUT`] を超えたら止める）。
fn sips(src: &Path, format: &str, out: &Path) -> Result<(), String> {
    let mut child = Command::new("sips")
        .args(["-s", "format", format])
        .arg(src)
        .arg("--out")
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("sips を起動できません: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < SIPS_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(out);
                return Err(format!(
                    "sips が {} 秒で {format} を書き終えませんでした",
                    SIPS_TIMEOUT.as_secs()
                ));
            }
            Err(e) => return Err(format!("sips を待てません: {e}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("sips を待てません: {e}"))?;
    if output.status.success() && out.is_file() {
        Ok(())
    } else {
        Err(format!(
            "sips が {format} を書けませんでした: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// システムのフォント（候補の順に。なければフォルダから拡張子で探す）。
fn system_font(candidates: &[&str], ext: &str) -> Option<PathBuf> {
    let root = Path::new("/System/Library/Fonts");
    candidates
        .iter()
        .map(|c| root.join(c))
        .find(|p| p.is_file())
        .or_else(|| {
            [root.to_path_buf(), root.join("Supplemental")]
                .iter()
                .filter_map(|d| std::fs::read_dir(d).ok())
                .flatten()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == ext))
                .min()
        })
}

fn be16(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| "フォントが短すぎます".to_string())
}

fn be32(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "フォントが短すぎます".to_string())
}

/// sfnt の表（タグ・チェックサム・中身）。
struct Table<'a> {
    tag: [u8; 4],
    checksum: u32,
    data: &'a [u8],
}

fn sfnt_tables(sfnt: &[u8]) -> Result<(u32, Vec<Table<'_>>), String> {
    let flavor = be32(sfnt, 0)?;
    let n = usize::from(be16(sfnt, 4)?);
    let mut tables = Vec::with_capacity(n);
    for i in 0..n {
        let r = 12 + i * 16;
        let tag: [u8; 4] = sfnt
            .get(r..r + 4)
            .and_then(|t| t.try_into().ok())
            .ok_or("表の見出しが短すぎます")?;
        let (checksum, at, len) = (be32(sfnt, r + 4)?, be32(sfnt, r + 8)?, be32(sfnt, r + 12)?);
        let data = sfnt
            .get(at as usize..(at as usize).saturating_add(len as usize))
            .ok_or("表がファイルを超えます")?;
        tables.push(Table {
            tag,
            checksum,
            data,
        });
    }
    Ok((flavor, tables))
}

fn pad4(len: usize) -> usize {
    (len + 3) & !3
}

/// sfnt を、表を圧縮しない WOFF 1.0 に包み直す（表の中身はそのまま）。
fn woff1(sfnt: &[u8]) -> Result<Vec<u8>, String> {
    let (flavor, tables) = sfnt_tables(sfnt)?;
    let n = tables.len();
    let sfnt_size = 12 + 16 * n + tables.iter().map(|t| pad4(t.data.len())).sum::<usize>();
    let mut offset = 44 + 20 * n;
    let mut dir = Vec::new();
    let mut data = Vec::new();
    for t in &tables {
        let len = t.data.len() as u32;
        dir.extend_from_slice(&t.tag);
        dir.extend_from_slice(&(offset as u32).to_be_bytes());
        dir.extend_from_slice(&len.to_be_bytes());
        dir.extend_from_slice(&len.to_be_bytes());
        dir.extend_from_slice(&t.checksum.to_be_bytes());
        data.extend_from_slice(t.data);
        data.resize(data.len() + pad4(t.data.len()) - t.data.len(), 0);
        offset += pad4(t.data.len());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"wOFF");
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&(offset as u32).to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(sfnt_size as u32).to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, 0]);
    out.extend_from_slice(&[0; 20]);
    out.extend_from_slice(&dir);
    out.extend_from_slice(&data);
    Ok(out)
}

/// WOFF2 の既知の表のタグ（番号が表の中の位置）。
const WOFF2_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm",
    b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern",
    b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC",
    b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar",
    b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty",
    b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat",
    b"Gloc", b"Feat", b"Sill",
];

/// UIntBase128（WOFF2 の可変長の数）。
fn base128(mut v: u32, out: &mut Vec<u8>) {
    let mut bytes = vec![(v & 0x7F) as u8];
    v >>= 7;
    while v > 0 {
        bytes.push((v & 0x7F) as u8 | 0x80);
        v >>= 7;
    }
    bytes.reverse();
    out.extend_from_slice(&bytes);
}

/// Brotli の「圧縮しない塊」だけの流れ（WBITS 24。塊は 2^16 バイトずつ）。
fn brotli_stored(data: &[u8]) -> Vec<u8> {
    struct Bits {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }
    impl Bits {
        fn put(&mut self, v: u64, n: u32) {
            self.acc |= v << self.n;
            self.n += n;
            while self.n >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }
        fn align(&mut self) {
            if self.n > 0 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    let mut b = Bits {
        out: Vec::new(),
        acc: 0,
        n: 0,
    };
    // WBITS：1 と 3 ビットの 7（17 + 7 = 24）
    b.put(1, 1);
    b.put(7, 3);
    for chunk in data.chunks(1 << 16) {
        b.put(0, 1); // ISLAST = 0
        b.put(0, 2); // MNIBBLES = 4
        b.put((chunk.len() - 1) as u64, 16); // MLEN - 1
        b.put(1, 1); // ISUNCOMPRESSED
        b.align();
        b.out.extend_from_slice(chunk);
    }
    b.put(1, 1); // ISLAST
    b.put(1, 1); // ISLASTEMPTY
    b.align();
    b.out
}

/// sfnt を WOFF2 に包み直す（どの表も変換しない。glyf・loca は「変換なし」の版 3。Brotli は圧縮しない塊）。
fn woff2(sfnt: &[u8]) -> Result<Vec<u8>, String> {
    let (flavor, tables) = sfnt_tables(sfnt)?;
    let n = tables.len();
    let mut dir = Vec::new();
    let mut stream = Vec::new();
    for t in &tables {
        let known = WOFF2_TAGS.iter().position(|k| **k == t.tag);
        let transform: u8 = if &t.tag == b"glyf" || &t.tag == b"loca" {
            3 << 6
        } else {
            0
        };
        match known {
            Some(i) => dir.push(i as u8 | transform),
            None => {
                dir.push(63 | transform);
                dir.extend_from_slice(&t.tag);
            }
        }
        base128(t.data.len() as u32, &mut dir);
        stream.extend_from_slice(t.data);
    }
    let compressed = brotli_stored(&stream);
    let sfnt_size = 12 + 16 * n + tables.iter().map(|t| pad4(t.data.len())).sum::<usize>();
    let header = 48;
    let total = pad4(header + dir.len() + compressed.len());
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"wOF2");
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(sfnt_size as u32).to_be_bytes());
    out.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, 0]); // 版 1.0
    out.extend_from_slice(&[0; 20]); // メタデータ・私的な塊なし
    out.extend_from_slice(&dir);
    out.extend_from_slice(&compressed);
    out.resize(total, 0);
    Ok(out)
}

/// 静止画の WebP（kura の書き手の VP8L）を、アニメの WebP（VP8X・ANIM・ANMF）に包む。
fn animated_webp(stills: &[Vec<u8>], w: u32, h: u32, ms: u32) -> Result<Vec<u8>, String> {
    let chunk = |tag: &[u8; 4], payload: &[u8]| {
        let mut c = Vec::with_capacity(payload.len() + 9);
        c.extend_from_slice(tag);
        c.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        c.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            c.push(0);
        }
        c
    };
    let u24 = |v: u32| {
        [
            (v & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            ((v >> 16) & 0xFF) as u8,
        ]
    };
    let mut body = Vec::new();
    // VP8X：アニメとアルファ
    let mut vp8x = vec![0x02 | 0x10, 0, 0, 0];
    vp8x.extend_from_slice(&u24(w - 1));
    vp8x.extend_from_slice(&u24(h - 1));
    body.extend(chunk(b"VP8X", &vp8x));
    // ANIM：背景は透明、くり返しは無限
    body.extend(chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]));
    for still in stills {
        // RIFF の中の、画像の塊（ALPH・VP8・VP8L）を取り出す
        let mut frame = Vec::new();
        let mut at = 12usize;
        while at + 8 <= still.len() {
            let tag = &still[at..at + 4];
            let len =
                u32::from_le_bytes([still[at + 4], still[at + 5], still[at + 6], still[at + 7]])
                    as usize;
            let end = (at + 8 + len).min(still.len());
            if matches!(tag, b"ALPH" | b"VP8 " | b"VP8L") {
                frame.extend_from_slice(&still[at..end]);
                if len % 2 == 1 {
                    frame.push(0);
                }
            }
            at = at + 8 + len + (len % 2);
        }
        if frame.is_empty() {
            return Err("WebP の画像の塊がありません".into());
        }
        let mut anmf = Vec::new();
        anmf.extend_from_slice(&u24(0));
        anmf.extend_from_slice(&u24(0));
        anmf.extend_from_slice(&u24(w - 1));
        anmf.extend_from_slice(&u24(h - 1));
        anmf.extend_from_slice(&u24(ms));
        anmf.push(0x02); // 重ねない（blending なし）・下地に戻さない
        anmf.extend_from_slice(&frame);
        body.extend(chunk(b"ANMF", &anmf));
    }
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::Limits;
    use kura_engine::doc::DecodeOptions;

    /// 自前で包んだ形（アニメの WebP・WOFF2 の Brotli）が読める。アニメの WebP は 8 コアのフレームの列になる。
    #[test]
    fn wrapped_formats_are_readable() {
        let dir = tempfile::tempdir().unwrap();
        let (samples, _) = build(dir.path()).unwrap();
        let names: Vec<&str> = samples.iter().map(|s| s.name.as_str()).collect();
        for name in [
            "alpha.png",
            "pixel-16.png",
            "pixel-16x4.png",
            "photo.jpg",
            "anim.gif",
            "anim.apng",
            "anim.webp",
            "sprite.aseprite",
            "layers.psd",
            "big.psb",
            "text.svg",
            "cube.glb",
            "textured.glb",
            "blip.wav",
            "sine-loop.wav",
            "loop.ogg",
        ] {
            assert!(names.contains(&name), "{name}");
        }
        let codecs = CodecSet::pinned();
        let (v, _) = codecs
            .decode_path(
                &dir.path().join("anim.webp"),
                "webp",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Frames(f) = v else {
            panic!("フレームの列でない")
        };
        assert_eq!((f.frames().len(), f.canvas()), (8, Size { w: 64, h: 64 }));
        let (v, _) = codecs
            .decode_path(
                &dir.path().join("loop.ogg"),
                "ogg",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Audio(a) = v else { panic!() };
        assert_eq!(
            a.meta.loop_range,
            Some(LoopRange {
                start: 10_000,
                end: 50_000
            })
        );
        // Brotli の圧縮しない塊は、長さを 2^16 ごとに区切る
        // 見出し（WBITS 4 ビット＋塊の見出し 20 ビット）3 バイト、2 つめの塊の見出し 3 バイト、終わりの 1 バイト
        let stored = brotli_stored(&vec![7u8; 70_000]);
        assert_eq!(stored.len(), 3 + 65_536 + 3 + 4_464 + 1);
        let mut v = Vec::new();
        base128(63, &mut v);
        base128(300, &mut v);
        assert_eq!(v, vec![63, 0x82, 0x2C]);
    }

    /// 形式の網羅（M11 の既定・critic 10）：形式の表の画像・フォント・3D の行ごとに見本があり、アプリと同じ渡しもの（kura-mac の
    /// MediaBackend とエンジンの部品）のライブラリに取り込むと、どの見本もサムネイルができ（透明のある画像・フォント・3D は
    /// PNG、ほかは JPEG）、情報が読める（画像は幅・高さ、フォントは書体名と字の数、3D は三角形の数。フォント・3D の幅・高さは空）。
    /// 表に行を足したら、見本を足さないと落ちる。
    #[test]
    #[cfg(target_os = "macos")]
    fn every_image_font_and_model_format_gets_a_thumbnail_and_probe() {
        use std::time::Duration;

        use kura_core::import::{ImportInput, ImportOptions, MediaKind, start_import};
        use kura_core::library::{LibrarySession, ThumbFormat, ThumbSize, hex};
        use kura_engine::codec::formats::FORMATS;

        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("assets");
        let (samples, skipped) = build(&assets).unwrap();
        // CI の仮想マシンでは sips が HEIC・AVIF を書けないことがある（kura-mac の fixtures/README.md）。ffmpeg がなければ
        // 動画・音声の一部も作らない。それ以外を作れなければ落とす
        let may_skip = [
            "alpha.heic",
            "alpha.heif",
            "alpha.avif",
            "clip.mp4",
            "tone.mp3",
            "tone.aac",
            "tone.flac",
            "tone.opus",
            "tone.aiff",
        ];
        for s in &skipped {
            assert!(may_skip.contains(&s.name.as_str()), "{s:?}");
            eprintln!("作れなかった見本：{}（{}）", s.name, s.reason);
        }

        let session = LibrarySession::create(
            &dir.path().join("L"),
            std::sync::Arc::new(|_| {}),
            crate::services(),
        )
        .unwrap();
        start_import(
            session.jobs(),
            &ImportInput {
                paths: vec![assets.clone()],
                options: ImportOptions::default(),
            },
        )
        .unwrap();
        session.wait_ready();
        assert!(session.jobs().wait_idle(Duration::from_secs(300)));
        let library = session.library();
        type Row = (
            String,
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Vec<u8>,
        );
        let rows: Vec<Row> = library
            .db()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT original_name, kind, thumb_state, width, height, codec_info, hash FROM items",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })?;
                Ok(rows.collect::<std::result::Result<_, _>>()?)
            })
            .unwrap();
        let mut checked = 0;
        for f in FORMATS.iter().filter(|f| {
            matches!(
                MediaKind::from(f.item_kind),
                MediaKind::Image | MediaKind::Font | MediaKind::Model
            )
        }) {
            let ext_of = |name: &str| name.rsplit('.').next().unwrap_or("").to_string();
            let mine: Vec<&Row> = rows
                .iter()
                .filter(|r| f.exts.contains(&ext_of(&r.0).as_str()))
                .collect();
            if mine.is_empty() {
                assert!(
                    skipped
                        .iter()
                        .any(|s| f.exts.contains(&ext_of(&s.name).as_str())),
                    "形式 {} の見本がありません",
                    f.id
                );
                continue;
            }
            for (name, kind, state, width, height, codec, hash) in mine {
                assert_eq!(
                    state, "ready",
                    "{name}（{}）のサムネイルができていない",
                    f.id
                );
                let sample = samples.iter().find(|s| &s.name == name).unwrap();
                let format = if sample.png {
                    ThumbFormat::Png
                } else {
                    ThumbFormat::Jpeg
                };
                let h = hex(hash);
                for size in [ThumbSize::Small, ThumbSize::Large] {
                    let path = library.layout().thumb_path_as(&h, size, format);
                    assert!(path.is_file(), "{name}：{} がない", path.display());
                }
                let codec: serde_json::Value =
                    serde_json::from_str(codec.as_deref().unwrap_or("null")).unwrap();
                match MediaKind::parse(kind).unwrap() {
                    MediaKind::Image => {
                        assert!(
                            width.is_some_and(|w| w > 0) && height.is_some_and(|h| h > 0),
                            "{name}：幅・高さがない（{width:?}×{height:?}）"
                        );
                    }
                    MediaKind::Font => {
                        assert_eq!((*width, *height), (None, None), "{name}");
                        assert!(
                            codec["font"]["family"]
                                .as_str()
                                .is_some_and(|s| !s.is_empty()),
                            "{name}：書体名がない {codec}"
                        );
                        assert!(codec["font"]["glyphs"].as_u64() > Some(0), "{name}");
                    }
                    MediaKind::Model => {
                        assert_eq!((*width, *height), (None, None), "{name}");
                        assert!(
                            codec["model"]["triangles"].as_u64() > Some(0),
                            "{name}：三角形の数がない {codec}"
                        );
                    }
                    MediaKind::Video | MediaKind::Audio => unreachable!(),
                }
                checked += 1;
            }
        }
        assert!(checked >= 20, "{checked}");
    }
}
