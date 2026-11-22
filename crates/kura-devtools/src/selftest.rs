//! kura:// と CSP を実機で確かめるライブラリ（`selftest-library --out <path> [--web <dir>] [--font <ttf>]`）。
//! docs/architecture.md §4.2 の P1〜P8・F1〜F4 を、`VITE_KURA_SELFTEST=1` のフロント（`src/dev/selftest.ts`）が試す。
//!
//! - 今のスキーマのライブラリを作り、種類が今の 3 種のもの（アルファ付き PNG・JPEG・WAV）は DB に入れる。
//!   JPEG のサムネイル（s・l）も置く。
//! - DB の種類にまだないもの（`<script>` 入りの SVG・ttf・glb）は `files/ab/cd/<hash>.<ext>` に直に置く
//!   （`/file` は DB を引かない）。WAV の波形の段（`kura-peaks`）も `peaks/ab/<hash>.peaks` に置く。
//! - フロントが中身のハッシュを知るための目録（JSON）を、決まった名前 [`MANIFEST_HASH`] で `files/` に置く
//!   （`/file/<MANIFEST_HASH>` で読める。整合性の確かめでは孤立したファイルに数えられる）。
//! - `--web <dir>` には、アプリ内ブラウザ（`capture-browser`）で開く F2 のページ `f2.html` を書く
//!   （`python3 -m http.server -d <dir>` などで配る）。
//! - ttf はシステムのフォントの写し（リポジトリには入れない）。`--font` で指定できる。
//!
//! 本物のライブラリには使わない（新しい場所にだけ作る）。

use std::path::{Path, PathBuf};

use kura_core::library::{Library, ThumbSize, hex};
use kura_core::time::now_ms;
use kura_engine::analysis::AnalysisOutput;
use kura_engine::analysis::audio::peaks::{PeakLevel, Peaks};

/// 目録を置く決まった名前（"selftest" に似せた 16 進。BLAKE3 の値として現れることはまずない）。
pub const MANIFEST_HASH: &str = "5e1f7e575e1f7e575e1f7e575e1f7e575e1f7e575e1f7e575e1f7e575e1f7e57";

/// アルファ付き PNG の左上の画素（P6 で読む）。
pub const PNG_TOP_LEFT: [u8; 4] = [255, 0, 0, 128];
/// WAV のレートと長さ（P5 で照らす）。
pub const WAV_RATE: u32 = 44_100;
pub const WAV_FRAMES: u32 = 22_050;

/// 探すシステムのフォント（最初に見つかったものを写す）。
const SYSTEM_FONTS: [&str; 3] = [
    "/System/Library/Fonts/Supplemental/Andale Mono.ttf",
    "/System/Library/Fonts/Supplemental/Courier New.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
];

/// 置いた 1 件。
struct Placed {
    hash: [u8; 32],
    ext: &'static str,
    size: i64,
}

/// `selftest-library` を実行する。
pub fn run(args: &[String]) -> Result<(), String> {
    let mut out = None;
    let mut web = None;
    let mut font = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--web" => web = it.next().map(PathBuf::from),
            "--font" => font = it.next().map(PathBuf::from),
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    let out = out.ok_or("--out にライブラリの場所を指定してください")?;
    let font = font.or_else(|| SYSTEM_FONTS.iter().map(PathBuf::from).find(|p| p.is_file()));
    let manifest = build(&out, web.as_deref(), font.as_deref())?;
    tracing::info!(path = %out.display(), "selftest のライブラリを作りました");
    tracing::info!("{manifest}");
    Ok(())
}

/// ライブラリを作り、目録（JSON）を返す。
fn build(out: &Path, web: Option<&Path>, font: Option<&Path>) -> Result<String, String> {
    let library = Library::create(out).map_err(|e| e.to_string())?;
    let layout = library.layout().clone();
    let place = |bytes: &[u8], ext: &'static str| -> Result<Placed, String> {
        let hash = *blake3::hash(bytes).as_bytes();
        write(&layout.content_path(&hash, ext), bytes)?;
        let size = i64::try_from(bytes.len()).map_err(|e| e.to_string())?;
        Ok(Placed { hash, ext, size })
    };

    let png = place(&alpha_png(), "png")?;
    let jpeg_bytes = jpeg(64, 48)?;
    let jpg = place(&jpeg_bytes, "jpg")?;
    let (wav_bytes, samples) = wav();
    let wav = place(&wav_bytes, "wav")?;
    let svg = place(SCRIPT_SVG.as_bytes(), "svg")?;
    let glb = place(&triangle_glb(), "glb")?;
    let ttf = match font {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Some(place(&bytes, "ttf")?)
        }
        None => {
            tracing::warn!("フォントが見つからないので ttf は置きません（P4 は見送り）");
            None
        }
    };

    // 画像の 2 件には JPEG のサムネイル（s・l）を置く
    for item in [&png, &jpg] {
        let h = hex(&item.hash);
        write(&layout.thumb_path(&h, ThumbSize::Small), &jpeg_bytes)?;
        write(&layout.thumb_path(&h, ThumbSize::Large), &jpeg_bytes)?;
    }
    // WAV の波形の段（/peaks の中身は M11 の段 3。置き場所は peaks/ab/<hash>.peaks）
    let wav_hex = hex(&wav.hash);
    let peaks_path = layout
        .root()
        .join("peaks")
        .join(&wav_hex[..2])
        .join(format!("{wav_hex}.peaks"));
    write(&peaks_path, &peaks(&samples)?)?;

    insert_items(
        &library,
        &[
            (&png, "image", Some((4, 4)), None, "ready", "alpha"),
            (&jpg, "image", Some((64, 48)), None, "ready", "photo"),
            (&wav, "audio", None, Some(500), "failed", "tone"),
        ],
    )?;

    let entry = |p: &Placed| serde_json::json!({ "hash": hex(&p.hash), "ext": p.ext });
    let manifest = serde_json::json!({
        "version": 1,
        "png": { "hash": hex(&png.hash), "width": 4, "topLeft": PNG_TOP_LEFT },
        "jpg": { "hash": hex(&jpg.hash), "width": 64 },
        "wav": { "hash": wav_hex, "rate": WAV_RATE, "frames": WAV_FRAMES },
        "svg": entry(&svg),
        "glb": entry(&glb),
        "ttf": ttf.as_ref().map(entry),
        "missing": "0".repeat(64),
    })
    .to_string();
    let manifest_path = layout
        .content_dir(MANIFEST_HASH)
        .ok_or("目録の置き場所を作れません")?
        .join(format!("{MANIFEST_HASH}.json"));
    write(&manifest_path, manifest.as_bytes())?;

    if let Some(web) = web {
        write(&web.join("f2.html"), f2_page(&hex(&png.hash)).as_bytes())?;
    }
    Ok(manifest)
}

/// (置いたもの, 種類, 幅と高さ, 長さ ms, サムネイルの状態, 題名)。
type Row<'a> = (
    &'a Placed,
    &'static str,
    Option<(i64, i64)>,
    Option<i64>,
    &'static str,
    &'static str,
);

fn insert_items(library: &Library, rows: &[Row<'_>]) -> Result<(), String> {
    let now = now_ms();
    let rows: Vec<_> = rows
        .iter()
        .map(|(p, kind, size, duration, thumb, title)| {
            (
                p.hash.to_vec(),
                p.ext,
                p.size,
                *kind,
                *size,
                *duration,
                *thumb,
                *title,
            )
        })
        .collect();
    library
        .db()
        .write(move |tx| {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO items (hash, ext, kind, size_bytes, width, height, duration_ms, title,
                                    original_name, source_site, imported_at, updated_at, thumb_state, proxy_state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'local', ?10, ?10, ?11, 'not_needed')",
            )?;
            for (hash, ext, bytes, kind, size, duration, thumb, title) in rows {
                stmt.execute(rusqlite::params![
                    hash,
                    ext,
                    kind,
                    bytes,
                    size.map(|s| s.0),
                    size.map(|s| s.1),
                    duration,
                    title,
                    format!("{title}.{ext}"),
                    now,
                    thumb,
                ])?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// 4×4 の RGBA の PNG。左上は半透明の赤（[`PNG_TOP_LEFT`]）、右下の 2×2 は透明、ほかは不透明の青。
fn alpha_png() -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..4u8 {
        raw.push(0); // フィルタなし
        for x in 0..4u8 {
            let px = match (x, y) {
                (0, 0) => PNG_TOP_LEFT,
                (2.., 2..) => [0, 0, 0, 0],
                _ => [0, 96, 255, 255],
            };
            raw.extend_from_slice(&px);
        }
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&4u32.to_be_bytes());
    ihdr.extend_from_slice(&4u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 ビット・RGBA
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// 圧縮しない zlib（無圧縮のブロック 1 つ。65,535 バイトまで）。
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let len = data.len() as u16;
    let mut out = vec![0x78, 0x01, 0x01];
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(data);
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// 横に色の変わる JPEG。
fn jpeg(w: u16, h: u16) -> Result<Vec<u8>, String> {
    let mut px = Vec::with_capacity(usize::from(w) * usize::from(h) * 3);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8, 160]);
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 85)
        .encode(&px, w, h, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// 0.5 秒・44.1 kHz・モノラル・16 ビットの 440 Hz（WAV のバイト列と標本）。
fn wav() -> (Vec<u8>, Vec<i16>) {
    let samples: Vec<i16> = (0..WAV_FRAMES)
        .map(|i| {
            let t = f64::from(i) / f64::from(WAV_RATE);
            ((t * 440.0 * std::f64::consts::TAU).sin() * 12_000.0).round() as i16
        })
        .collect();
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // モノラル
    out.extend_from_slice(&WAV_RATE.to_le_bytes());
    out.extend_from_slice(&(WAV_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    (out, samples)
}

/// 標本から `kura-peaks` 版 1（256・1,024 の 2 段）を作る。
fn peaks(samples: &[i16]) -> Result<Vec<u8>, String> {
    let level = |bin: usize| PeakLevel {
        bin: bin as u32,
        data: samples
            .chunks(bin)
            .flat_map(|c| {
                let min = c.iter().copied().min().unwrap_or(0);
                let max = c.iter().copied().max().unwrap_or(0);
                [min, max]
            })
            .collect(),
    };
    Peaks {
        rate: WAV_RATE,
        channels: 1,
        frames: samples.len() as u64,
        levels: vec![level(256), level(1024)],
    }
    .to_canonical()
    .map_err(|e| e.to_string())
}

/// 三角形 1 つの glb（glTF 2.0 のバイナリ）。
fn triangle_glb() -> Vec<u8> {
    let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let mut bin: Vec<u8> = positions.iter().flat_map(|v| v.to_le_bytes()).collect();
    let json = serde_json::json!({
        "asset": { "version": "2.0", "generator": "kura-devtools selftest-library" },
        "buffers": [{ "byteLength": bin.len() }],
        "bufferViews": [{ "buffer": 0, "byteLength": bin.len() }],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0, 0, 0], "max": [1, 1, 0]
        }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "nodes": [{ "mesh": 0 }],
        "scenes": [{ "nodes": [0] }],
        "scene": 0
    })
    .to_string();
    let mut json = json.into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

/// 文書として開かれてスクリプトが動けば、親へ知らせ、印の付いた要求を出す SVG（F4 で動かないことを見る）。
/// 目録の名前に `?p=svg-script-ran` を付けて読むので、動けばログに残る。
const SCRIPT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32">
<rect width="32" height="32" fill="#2c4a63"/>
<circle cx="16" cy="16" r="9" fill="#f2a65a"/>
<script>
try { parent.postMessage('kura-selftest-svg-script-ran', '*'); } catch (e) {}
try { new Image().src = 'kura://localhost/file/5e1f7e575e1f7e575e1f7e575e1f7e575e1f7e575e1f7e575e1f7e575e1f7e57?p=svg-script-ran'; } catch (e) {}
</script>
</svg>
"##;

/// アプリ内ブラウザで開く F2 のページ。kura:// を読もうとして、結果を同じ http サーバーへ返す（アクセスログで見る）。
fn f2_page(png_hash: &str) -> String {
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>kura selftest F2</title>
<p>kura selftest F2（ほかの WebView からの kura://）</p>
<script>
const K = 'kura://localhost/file/{png_hash}';
const report = (k, v) => fetch('/report?' + k + '=' + encodeURIComponent(v)).catch(() => {{}});
const img = new Image();
img.onload = () => report('F2-img', 'load');
img.onerror = () => report('F2-img', 'error');
img.src = K + '?p=F2-img';
fetch(K + '?p=F2-fetch')
  .then((r) => report('F2-fetch', 'status-' + r.status))
  .catch((e) => report('F2-fetch', 'error-' + e.name));
</script>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::codec::CodecSet;
    use kura_engine::doc::DecodeOptions;
    use kura_engine::value::Value;
    use kura_engine::{Limits, NoMonitor};

    fn content(layout: &kura_core::library::Layout, hash: &str, ext: &str) -> Vec<u8> {
        let hash = kura_core::library::parse_hex32(hash).unwrap();
        std::fs::read(layout.content_path(&hash, ext)).unwrap()
    }

    #[test]
    fn selftest_library_places_every_fixture_and_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("selftest.kuralib");
        let web = dir.path().join("web");
        let font = dir.path().join("font.ttf");
        std::fs::write(&font, b"\x00\x01\x00\x00not-a-real-font").unwrap();
        let manifest: serde_json::Value =
            serde_json::from_str(&build(&out, Some(&web), Some(&font)).unwrap()).unwrap();

        let library = Library::open(&out).unwrap();
        let layout = library.layout().clone();
        // 目録は決まった名前で /file から読める
        let listed: serde_json::Value =
            serde_json::from_slice(&content(&layout, MANIFEST_HASH, "json")).unwrap();
        assert_eq!(listed, manifest);
        assert!(kura_core::serve::parse_route(&format!("/file/{MANIFEST_HASH}")).is_some());

        // 置いたものは中身のハッシュの名前
        for (key, ext) in [
            ("png", "png"),
            ("jpg", "jpg"),
            ("wav", "wav"),
            ("svg", "svg"),
            ("glb", "glb"),
            ("ttf", "ttf"),
        ] {
            let hash = manifest[key]["hash"].as_str().unwrap();
            let bytes = content(&layout, hash, ext);
            assert_eq!(hex(blake3::hash(&bytes).as_bytes()), hash, "{key}");
        }

        // PNG はエンジンで読めて、左上が半透明の赤
        let codecs = CodecSet::pinned();
        let png = content(&layout, manifest["png"]["hash"].as_str().unwrap(), "png");
        let (value, _) = codecs
            .decode(
                &mut std::io::Cursor::new(png),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(image) = value else {
            panic!("画像として読めません");
        };
        let rgba = kura_engine::imaging::convert::to_rgba8_rows(&image.px).unwrap();
        assert_eq!(rgba[..4], PNG_TOP_LEFT);
        assert_eq!(rgba[rgba.len() - 4..], [0, 0, 0, 0]);

        // WAV は長さが合う
        let wav = content(&layout, manifest["wav"]["hash"].as_str().unwrap(), "wav");
        let (value, _) = codecs
            .decode(
                &mut std::io::Cursor::new(wav),
                "wav",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Audio(audio) = value else {
            panic!("音声として読めません");
        };
        assert_eq!(
            (audio.rate(), audio.frames()),
            (WAV_RATE, WAV_FRAMES as usize)
        );

        // 波形の段は kura-peaks として読み戻せる
        let wav_hex = manifest["wav"]["hash"].as_str().unwrap();
        let peaks_bytes = std::fs::read(
            layout
                .root()
                .join("peaks")
                .join(&wav_hex[..2])
                .join(format!("{wav_hex}.peaks")),
        )
        .unwrap();
        let peaks = Peaks::from_canonical(&peaks_bytes).unwrap();
        assert_eq!(peaks.frames, u64::from(WAV_FRAMES));
        assert_eq!(peaks.levels.len(), 2);

        // glb のヘッダ
        let glb = content(&layout, manifest["glb"]["hash"].as_str().unwrap(), "glb");
        assert_eq!(&glb[..4], b"glTF");
        assert_eq!(
            u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize,
            glb.len()
        );

        // SVG はスクリプトが動けば印を付けて読む
        let svg = content(&layout, manifest["svg"]["hash"].as_str().unwrap(), "svg");
        assert!(String::from_utf8(svg).unwrap().contains("svg-script-ran"));

        // DB には今の 3 種だけ（サムネイルは画像の 2 件）
        let rows: Vec<(String, String)> = library
            .db()
            .read(|c| {
                let mut stmt = c.prepare("SELECT kind, thumb_state FROM items ORDER BY id")?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap();
        assert_eq!(
            rows,
            [
                ("image".into(), "ready".into()),
                ("image".into(), "ready".into()),
                ("audio".into(), "failed".into())
            ]
        );
        let png_hex = manifest["png"]["hash"].as_str().unwrap();
        assert!(layout.thumb_path(png_hex, ThumbSize::Small).is_file());

        // F2 のページ
        let page = std::fs::read_to_string(web.join("f2.html")).unwrap();
        assert!(page.contains(&format!("kura://localhost/file/{png_hex}")));

        // 同じ場所には作り直さない（本物のライブラリを上書きしない）
        assert!(build(&out, None, None).is_err());
    }
}
