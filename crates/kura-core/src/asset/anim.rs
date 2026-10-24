//! アニメの帯（IPC `item_anim_get` と kura:// の `/anim/{hash}/1`。M11 の決定 8・defaults 23。docs/design.md §3.1）。
//!
//! - 見たものだけ、その場で帯の PNG を作り、`anim/ab/<hash>-1.png` に一時ファイル（ライブラリの `staging/`）から改名して
//!   書く。帯はキャンバスの大きさのセルを、行ごとに左から `cols` 個ずつ隙間なく並べたもの（`src/lib/asset/timeline.ts` の
//!   `stripCell` と同じ）。コマはセルの中にキャンバスの `offset` で置き、透明を保つ（RGBA8）。
//! - 扱うもの：コマが 2 つ以上ある画像（aseprite・GIF・APNG・アニメの WebP）と、格子を設定した画像（`asset_settings` の
//!   格子。先頭のコマをキャンバスに置き、セルを行ごとに左から切る。`sprite.split_grid` と同じ切り方）。格子があれば格子が
//!   勝つ。
//! - 帯の PNG の tEXt `kura-anim` に、帯の情報（キャンバス・列の数・コマの数・コマの長さ・タグ）と、作った元の鍵（格子の
//!   設定。ファイルのコマなら `frames`）を入れる。鍵が合えば読み直さずに帯の情報を返し（「すでにあれば作らない」）、
//!   合わなければ（格子を変えた）作り直す。格子の帯は `rev`（鍵から決める 1 以上の数）を返し、フロントは URL に
//!   `?r={rev}` を付けて WebKit のメモリのキャッシュを避ける（サムネイルの `thumbRev` と同じ）。
//! - 上限：帯は 1 辺 [`MAX_STRIP_SIDE`]、[`MAX_STRIP_PIXELS`] 画素まで（超えたら [`AnimError::TooLarge`]）。コマは
//!   [`MAX_FRAMES`] まで。
//! - ジョブにしない（利用者がその場で待つ短い表示の用意。決定 8）。呼び手が `spawn_blocking` で呼ぶ。予算は対話
//!   （待たない）で、空いていなければ [`AnimError::Busy`]、全体を超えれば [`AnimError::TooLarge`]。読む・組む・書くのは
//!   エンジンのプレビューのプールの中。
//! - コマの長さはファイルのまま（0 もそのまま返し、フロントが 100ms にする）。格子の帯は設定のコマの長さ（なければ 0）。

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use kura_engine::budget::{Class, Lease, MemoryBudget};
use kura_engine::codec::CodecSet;
use kura_engine::doc::DecodeOptions;
use kura_engine::imaging::convert::to_rgba8_rows;
use kura_engine::imaging::pixels::{down16, is_16, read_rgba};
use kura_engine::value::{Frame, Image, ImageMeta, PixelFormat, Raster, Value};
use kura_engine::{Dim, EngineError, Limits, NoMonitor, Shape};
use rusqlite::{Connection, OptionalExtension};

use super::{GridSetting, grid_from_json};
use crate::edit::EngineServices;
use crate::import::{MediaKind, format_id};
use crate::library::{ANIM_V, Layout, hex, parse_hex32};
use crate::{CoreError, Result};

/// アニメのタグとその向き（kura-engine の値の型。src-tauri は kura-engine に依らないので、ここから使う）。
pub use kura_engine::value::{AnimTag, TagDirection};

/// 帯に並べるコマの数の上限（kura-engine の `Limits::max_frames` と同じ）。
pub const MAX_FRAMES: u32 = 4_096;

/// 帯の 1 辺の上限（画素）。
pub const MAX_STRIP_SIDE: u32 = 8_192;

/// 帯の画素の数の上限（16M 画素。RGBA8 で 64 MiB）。
pub const MAX_STRIP_PIXELS: u64 = 16 * 1024 * 1024;

/// 帯の PNG の tEXt のキーワード。
pub const STRIP_META_KEY: &str = "kura-anim";

/// アニメを出せない理由。
#[derive(Debug, thiserror::Error)]
pub enum AnimError {
    /// アイテム・元のファイルがない。
    #[error("見つかりません: {0}")]
    NotFound(String),
    /// アニメではない（1 枚の画像で格子もない・画像でない）。
    #[error("アニメではありません（{0}）")]
    NotAnimated(String),
    /// 帯が大きすぎる（1 辺・画素の数・メモリの予算の全体）。
    #[error("大きすぎるためアニメを表示できません（{0}）")]
    TooLarge(String),
    /// メモリの予算が空いていない（少し待てば通りうる）。
    #[error("混んでいるためアニメを用意できません。もう一度試してください（{0}）")]
    Busy(String),
    /// エンジンを使えない。
    #[error("エンジンを使えません（{0}）")]
    Unavailable(String),
    /// 読めない・書けない。
    #[error("アニメの帯を作れませんでした（{0}）")]
    Failed(String),
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// 帯の並べ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StripLayout {
    /// コマの数。
    pub frames: u32,
    /// 1 行に並べるコマの数。
    pub cols: u32,
    pub rows: u32,
    /// セル（キャンバス）の大きさ。
    pub cell_w: u32,
    pub cell_h: u32,
}

impl StripLayout {
    /// 帯の幅。
    pub fn width(&self) -> u32 {
        self.cols.saturating_mul(self.cell_w)
    }

    /// 帯の高さ。
    pub fn height(&self) -> u32 {
        self.rows.saturating_mul(self.cell_h)
    }

    /// コマ `f` のセルの左上（`timeline.ts` の `stripCell`）。
    pub fn cell(&self, f: u32) -> (u32, u32) {
        if self.cols == 0 {
            return (0, 0);
        }
        (
            (f % self.cols).saturating_mul(self.cell_w),
            (f / self.cols).saturating_mul(self.cell_h),
        )
    }
}

/// `frames` 個の `cell_w × cell_h` のコマを並べる帯の形。列の数は 1 辺の上限に収まる最大（コマの数まで）にして行の数を
/// 決め、その行の数で空くセルが最も少なくなるように列を詰める。上限を超えたら TooLarge。
pub fn strip_layout(
    frames: u32,
    cell_w: u32,
    cell_h: u32,
) -> std::result::Result<StripLayout, AnimError> {
    if frames == 0 || cell_w == 0 || cell_h == 0 {
        return Err(AnimError::NotAnimated("コマがありません".into()));
    }
    if frames > MAX_FRAMES {
        return Err(AnimError::TooLarge(format!(
            "コマが {frames} あります（{MAX_FRAMES} まで）"
        )));
    }
    if cell_w > MAX_STRIP_SIDE || cell_h > MAX_STRIP_SIDE {
        return Err(AnimError::TooLarge(format!(
            "1 コマが {cell_w} × {cell_h} です（1 辺 {MAX_STRIP_SIDE} まで）"
        )));
    }
    let widest = (MAX_STRIP_SIDE / cell_w).min(frames);
    let rows = frames.div_ceil(widest);
    let cols = frames.div_ceil(rows);
    let layout = StripLayout {
        frames,
        cols,
        rows,
        cell_w,
        cell_h,
    };
    let (w, h) = (
        u64::from(cols) * u64::from(cell_w),
        u64::from(rows) * u64::from(cell_h),
    );
    if h > u64::from(MAX_STRIP_SIDE) || w * h > MAX_STRIP_PIXELS {
        return Err(AnimError::TooLarge(format!(
            "{frames} コマの帯が {w} × {h} になります（1 辺 {MAX_STRIP_SIDE}、{MAX_STRIP_PIXELS} 画素まで）"
        )));
    }
    Ok(layout)
}

/// 帯の PNG に入れる帯の情報（tEXt `kura-anim` の JSON）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StripMeta {
    /// この JSON の版。
    pub v: u32,
    /// 作った元の鍵（`frames` か格子の設定）。
    pub key: String,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub cols: u32,
    pub frame_count: u32,
    /// コマの長さ（ms。ファイルのまま。格子の帯は 0）。
    pub durations: Vec<u32>,
    pub tags: Vec<AnimTag>,
}

/// 帯の情報（`item_anim_get` が返すもの）。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimInfo {
    pub hash: String,
    /// 帯の並べ方の版（`/anim/{hash}/{版}`）。
    pub version: u32,
    /// 格子の帯なら鍵から決める 1 以上の数（URL の `?r=`）、ファイルのコマなら 0。
    pub rev: u32,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub cols: u32,
    pub frame_count: u32,
    pub durations: Vec<u32>,
    pub tags: Vec<AnimTag>,
}

/// 帯を作るのに要るアイテムの情報。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimRequest {
    pub hash: String,
    pub ext: String,
    pub kind: MediaKind,
    pub grid: Option<GridSetting>,
    pub frame_ms: Option<u32>,
}

/// アイテムの帯の元（なければ NotFound）。
pub fn request_of(conn: &Connection, item_id: i64) -> Result<AnimRequest> {
    let mut stmt = conn.prepare_cached(
        "SELECT i.hash, i.ext, i.kind, s.grid_json, s.frame_ms
         FROM items i LEFT JOIN asset_settings s ON s.item_id = i.id WHERE i.id = ?1",
    )?;
    let found = stmt
        .query_row([item_id], |row| {
            Ok(AnimRequest {
                hash: hex(&row.get::<_, Vec<u8>>(0)?),
                ext: row.get(1)?,
                kind: row.get(2)?,
                grid: row
                    .get::<_, Option<String>>(3)?
                    .as_deref()
                    .and_then(grid_from_json),
                frame_ms: row
                    .get::<_, Option<i64>>(4)?
                    .and_then(|v| u32::try_from(v).ok()),
            })
        })
        .optional()?;
    found.ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id}")))
}

/// ファイルのコマの帯の鍵。
const FRAMES_KEY: &str = "frames";

/// 作った元の鍵（ファイルのコマなら `frames`、格子なら設定。帯の画素を決めるものだけを入れる。コマの長さは入れない）。
pub fn strip_key(grid: Option<&GridSetting>) -> String {
    match grid {
        None => FRAMES_KEY.to_string(),
        Some(g) => format!(
            "grid:{},{},{},{},{},{},{}",
            g.cell_w,
            g.cell_h,
            g.offset_x,
            g.offset_y,
            g.gap_x,
            g.gap_y,
            g.count.map_or_else(|| "*".to_string(), |c| c.to_string())
        ),
    }
}

/// 鍵から `rev`（ファイルのコマは 0、格子は鍵の BLAKE3 の先頭 4 バイトから決める 1 以上の数）。
pub fn rev_of(key: &str) -> u32 {
    if key == FRAMES_KEY {
        return 0;
    }
    let digest = blake3::hash(key.as_bytes());
    let mut head = [0u8; 4];
    head.copy_from_slice(&digest.as_bytes()[..4]);
    u32::from_le_bytes(head).max(1)
}

/// 帯の情報の JSON の版。
const STRIP_META_V: u32 = 1;

fn failed(err: EngineError) -> AnimError {
    match err {
        EngineError::Limit(m) => AnimError::TooLarge(m),
        other => AnimError::Failed(other.to_string()),
    }
}

/// 長さ `len` の 0 の列（足りなければ TooLarge。大きさは帯の上限で決まっているが、確保の失敗で落ちない）。
fn zeroed(len: usize) -> std::result::Result<Vec<u8>, AnimError> {
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| AnimError::TooLarge(format!("{len} バイトを確保できません")))?;
    v.resize(len, 0);
    Ok(v)
}

/// 画素を RGBA8 の行にする（パレットは色に広げ、16 ビットは四捨五入で 8 ビットへ。`pixels::down16`）。
fn rgba8(px: &Raster) -> std::result::Result<Vec<u8>, AnimError> {
    if !is_16(px.format()) {
        return to_rgba8_rows(px).map_err(failed);
    }
    let rows = px.to_rows().map_err(failed)?;
    let bpp = px.format().bytes_per_pixel();
    let mut out = zeroed(rows.len() / bpp * 4)?;
    for (src, dst) in rows
        .chunks_exact(bpp)
        .zip(out.as_chunks_mut::<4>().0.iter_mut())
    {
        let c = read_rgba(px.format(), None, src).map_err(failed)?;
        for (d, v) in dst.iter_mut().zip(c) {
            *d = u8::try_from(down16(v)).unwrap_or(u8::MAX);
        }
    }
    Ok(out)
}

/// `src`（幅 `src_w` の RGBA8）の矩形 (sx, sy, w, h) を、`dst`（幅 `dst_w`）の (dx, dy) に写す。はみ出せば Failed（落ちない）。
fn blit(
    dst: &mut [u8],
    dst_w: u64,
    (dx, dy): (u64, u64),
    src: &[u8],
    src_w: u64,
    (sx, sy): (u64, u64),
    (w, h): (u64, u64),
) -> std::result::Result<(), AnimError> {
    let outside = || AnimError::Failed("コマが帯の外にはみ出します".into());
    let row = usize::try_from(w * 4).map_err(|_| outside())?;
    for y in 0..h {
        let s = usize::try_from(((sy + y) * src_w + sx) * 4).map_err(|_| outside())?;
        let d = usize::try_from(((dy + y) * dst_w + dx) * 4).map_err(|_| outside())?;
        let from = src.get(s..s + row).ok_or_else(outside)?;
        dst.get_mut(d..d + row)
            .ok_or_else(outside)?
            .copy_from_slice(from);
    }
    Ok(())
}

/// キャンバスいっぱいの RGBA8（1 枚の画像はそのまま、フレームの列は先頭のコマを offset に置いたもの）と、その大きさ。
fn canvas_rgba(value: &Value) -> std::result::Result<(u32, u32, Vec<u8>), AnimError> {
    match value {
        Value::Image(img) => Ok((img.px.width(), img.px.height(), rgba8(&img.px)?)),
        Value::Frames(fr) => {
            let (w, h) = (fr.canvas().w, fr.canvas().h);
            let first = fr.first();
            let mut out = zeroed(w as usize * h as usize * 4)?;
            let at = offset_of(first)?;
            blit(
                &mut out,
                u64::from(w),
                at,
                &rgba8(&first.px)?,
                u64::from(first.px.width()),
                (0, 0),
                (u64::from(first.px.width()), u64::from(first.px.height())),
            )?;
            Ok((w, h, out))
        }
        Value::Audio(_) => Err(AnimError::NotAnimated("音声です".into())),
    }
}

/// コマの左上（キャンバスの中。負の値は Frames が断っている）。
fn offset_of(f: &Frame) -> std::result::Result<(u64, u64), AnimError> {
    let bad = || AnimError::Failed("コマの位置が負です".into());
    Ok((
        u64::try_from(f.offset.x).map_err(|_| bad())?,
        u64::try_from(f.offset.y).map_err(|_| bad())?,
    ))
}

/// 値の形（キャンバスの幅・高さ・コマの数）と格子から、帯の形とコマの数。帯の上限・アニメでないものはここで断る。
fn plan(shape: &Shape, grid: Option<&GridSetting>) -> std::result::Result<StripLayout, AnimError> {
    let side = |d: Dim| {
        u32::try_from(d.max()).map_err(|_| AnimError::TooLarge(format!("{} 画素", d.max())))
    };
    match (shape, grid) {
        (Shape::Image { w, h, .. } | Shape::Frames { w, h, .. }, Some(g)) => {
            let n = g.frames_used(side(*w)?, side(*h)?);
            if n == 0 {
                return Err(AnimError::NotAnimated(
                    "格子にセルが 1 つもありません".into(),
                ));
            }
            strip_layout(n, g.cell_w, g.cell_h)
        }
        (Shape::Frames { count, w, h, .. }, None) => {
            let n = u32::try_from(count.max()).unwrap_or(u32::MAX);
            if n < 2 {
                return Err(AnimError::NotAnimated("コマが 1 つだけです".into()));
            }
            strip_layout(n, side(*w)?, side(*h)?)
        }
        (Shape::Image { .. }, None) => Err(AnimError::NotAnimated(
            "1 枚の画像で、格子もありません".into(),
        )),
        (Shape::Audio { .. }, _) => Err(AnimError::NotAnimated("音声です".into())),
    }
}

/// JSON を ASCII にする（文字列の中の ASCII でない文字を `\uXXXX` に。PNG の tEXt は Latin-1 なので）。
fn ascii_json(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{u:04x}");
            }
        }
    }
    out
}

/// 読んだ値から帯の PNG と帯の情報を作る（同じ値と格子なら同じバイト列）。値は組んだあとすぐ手放す。
pub fn build_strip(
    value: Value,
    grid: Option<&GridSetting>,
    codecs: &CodecSet,
) -> std::result::Result<(Vec<u8>, StripMeta), AnimError> {
    let layout = plan(&value.shape(), grid)?;
    let (sw, sh) = (u64::from(layout.width()), u64::from(layout.height()));
    let mut strip =
        zeroed(usize::try_from(sw * sh * 4).map_err(|_| AnimError::TooLarge("帯".into()))?)?;
    let (canvas_w, canvas_h, durations, tags) = match (&value, grid) {
        (_, Some(g)) => {
            let (w, _, src) = canvas_rgba(&value)?;
            let cols = g.cols(w).max(1);
            for f in 0..layout.frames {
                let (gx, gy) = (u64::from(f) % cols, u64::from(f) / cols);
                let sx = u64::from(g.offset_x) + gx * (u64::from(g.cell_w) + u64::from(g.gap_x));
                let sy = u64::from(g.offset_y) + gy * (u64::from(g.cell_h) + u64::from(g.gap_y));
                let (dx, dy) = layout.cell(f);
                blit(
                    &mut strip,
                    sw,
                    (u64::from(dx), u64::from(dy)),
                    &src,
                    u64::from(w),
                    (sx, sy),
                    (u64::from(g.cell_w), u64::from(g.cell_h)),
                )?;
            }
            (
                g.cell_w,
                g.cell_h,
                vec![0; layout.frames as usize],
                Vec::new(),
            )
        }
        (Value::Frames(fr), None) => {
            for (f, frame) in (0u32..).zip(fr.frames()) {
                let (dx, dy) = layout.cell(f);
                let (ox, oy) = offset_of(frame)?;
                blit(
                    &mut strip,
                    sw,
                    (u64::from(dx) + ox, u64::from(dy) + oy),
                    &rgba8(&frame.px)?,
                    u64::from(frame.px.width()),
                    (0, 0),
                    (u64::from(frame.px.width()), u64::from(frame.px.height())),
                )?;
            }
            (
                fr.canvas().w,
                fr.canvas().h,
                fr.frames().iter().map(|f| f.duration_ms).collect(),
                fr.meta.tags.clone(),
            )
        }
        _ => return Err(AnimError::NotAnimated("1 枚の画像です".into())),
    };
    // 組んだら元の値はもう要らない（書く間のメモリを減らす）
    drop(value);
    let meta = StripMeta {
        v: STRIP_META_V,
        key: strip_key(grid),
        canvas_w,
        canvas_h,
        cols: layout.cols,
        frame_count: layout.frames,
        durations,
        tags,
    };
    let px = Raster::from_rows(
        layout.width(),
        layout.height(),
        PixelFormat::Rgba8,
        None,
        &strip,
    )
    .map_err(failed)?;
    drop(strip);
    let mut image_meta = ImageMeta::default();
    image_meta.text.insert(
        STRIP_META_KEY.to_string(),
        ascii_json(&serde_json::to_string(&meta).map_err(|e| AnimError::Failed(e.to_string()))?),
    );
    let image = Value::Image(Arc::new(Image {
        px,
        meta: image_meta,
    }));
    let (png, _) = codecs
        .encode_bytes(
            &image,
            "png",
            &serde_json::json!({ "compression": "fast" }),
            &NoMonitor,
        )
        .map_err(failed)?;
    Ok((png, meta))
}

/// PNG の先頭。
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// 帯の情報の tEXt として読む大きさの上限（コマの長さ 4,096 個とタグで数十 KB）。
const MAX_META_BYTES: u32 = 1 << 20;
/// IDAT の前に見るチャンクの数の上限。
const MAX_HEADER_CHUNKS: usize = 64;

/// 帯の PNG から帯の情報を読む（IDAT の前の tEXt `kura-anim`。読めなければ None）。
pub fn read_strip_meta(path: &Path) -> Option<StripMeta> {
    let mut file = BufReader::new(File::open(path).ok()?);
    let mut signature = [0u8; 8];
    file.read_exact(&mut signature).ok()?;
    if signature != PNG_SIGNATURE {
        return None;
    }
    for _ in 0..MAX_HEADER_CHUNKS {
        let mut head = [0u8; 8];
        file.read_exact(&mut head).ok()?;
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        match &head[4..8] {
            b"IDAT" | b"IEND" => return None,
            b"tEXt" if len <= MAX_META_BYTES => {
                let mut data = vec![0u8; len as usize];
                file.read_exact(&mut data).ok()?;
                file.seek_relative(4).ok()?;
                let nul = data.iter().position(|b| *b == 0)?;
                if data.get(..nul)? == STRIP_META_KEY.as_bytes() {
                    let text = std::str::from_utf8(data.get(nul + 1..)?).ok()?;
                    return serde_json::from_str(text).ok();
                }
            }
            _ => file.seek_relative(i64::from(len) + 4).ok()?,
        }
    }
    None
}

/// 帯の情報から返す情報（格子の帯のコマの長さは今の設定から）。
fn info_of(req: &AnimRequest, meta: StripMeta) -> AnimInfo {
    let durations = match req.grid {
        Some(_) => vec![req.frame_ms.unwrap_or(0); meta.frame_count as usize],
        None => meta.durations,
    };
    AnimInfo {
        hash: req.hash.clone(),
        version: ANIM_V,
        rev: rev_of(&meta.key),
        canvas_w: meta.canvas_w,
        canvas_h: meta.canvas_h,
        cols: meta.cols,
        frame_count: meta.frame_count,
        durations,
        tags: meta.tags,
    }
}

/// 一時ファイル（ライブラリの staging/。開くときの掃除で消える）に書いてから改名する。
fn write_atomic(layout: &Layout, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let staging = layout.staging_dir();
    std::fs::create_dir_all(&staging)?;
    let temp = staging.join(format!(
        "anim-{}-{}.png.part",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// 予算から対話で確保する（待たない）。全体を超えれば TooLarge、空いていなければ Busy。
fn acquire(budget: &Arc<MemoryBudget>, bytes: u64) -> std::result::Result<Lease, AnimError> {
    if bytes > budget.total() {
        return Err(AnimError::TooLarge(format!(
            "メモリの予算を超えます（{bytes} バイト。予算は {} バイト）",
            budget.total()
        )));
    }
    match budget.try_acquire(bytes, Class::Interactive) {
        Ok(Some(lease)) => Ok(lease),
        Ok(None) => Err(AnimError::Busy(format!("{bytes} バイトを確保できません"))),
        Err(err) => Err(AnimError::Busy(err.to_string())),
    }
}

/// 作るのに使うメモリの見積もり：読む間（値 + 作業領域）、組む間（値 + 帯 + キャンバス）、書く間（帯の画素 + PNG の
/// 作業領域（fast は 4 倍）+ 書いたバイト列）の最大。
fn estimate(shape: &Shape, layout: &StripLayout, decode_work: u64) -> u64 {
    let value = shape.value_bytes();
    let strip = u64::from(layout.width()) * u64::from(layout.height()) * 4;
    let canvas = match shape {
        Shape::Image { w, h, fmt } | Shape::Frames { w, h, fmt, .. } => w
            .max()
            .saturating_mul(h.max())
            .saturating_mul(fmt.bytes_per_pixel() as u64 + 4),
        Shape::Audio { .. } => 0,
    };
    value
        .saturating_add(decode_work)
        .max(value.saturating_add(strip).saturating_add(canvas))
        .max(strip.saturating_mul(6))
}

/// 帯の情報を返す。帯がまだない・鍵が合わなければ作って `anim/ab/<hash>-1.png` に書く（ブロックする）。
pub fn get(
    engine: &EngineServices,
    layout: &Layout,
    req: &AnimRequest,
) -> std::result::Result<AnimInfo, AnimError> {
    if req.kind != MediaKind::Image {
        return Err(AnimError::NotAnimated(format!(
            "{} です",
            req.kind.as_str()
        )));
    }
    let key = strip_key(req.grid.as_ref());
    let path = layout.anim_path(&req.hash);
    if let Some(meta) = read_strip_meta(&path).filter(|m| m.v == STRIP_META_V && m.key == key) {
        return Ok(info_of(req, meta));
    }
    let hash = parse_hex32(&req.hash).ok_or_else(|| AnimError::NotFound(req.hash.clone()))?;
    let source = layout.content_path(&hash, &req.ext);
    let len = std::fs::metadata(&source)
        .map_err(|_| AnimError::NotFound(source.display().to_string()))?
        .len();
    let format = format_id(&req.ext)
        .ok_or_else(|| AnimError::NotAnimated(format!("{} は読めません", req.ext)))?;
    let runtime = engine
        .runtime()
        .map_err(|err| AnimError::Unavailable(err.to_string()))?;
    let codecs = engine.codecs();
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let (shape, _) = runtime
        .preview(|_| codecs.shape_path(&source, format, &opts, &limits))
        .map_err(failed)?;
    let strip = match plan(&shape, req.grid.as_ref()) {
        Ok(strip) => strip,
        Err(err) => {
            // 格子を外して 1 枚の画像に戻ったなど：前の帯は要らない
            if matches!(err, AnimError::NotAnimated(_)) && path.is_file() {
                let _ = std::fs::remove_file(&path);
            }
            return Err(err);
        }
    };
    let need = estimate(
        &shape,
        &strip,
        codecs.decode_file_work_bytes(format, &shape, len),
    );
    let _lease = acquire(runtime.budget(), need)?;
    let started = Instant::now();
    let (png, meta) = runtime.preview(|_| {
        let (value, _) = codecs
            .decode_path(&source, format, &opts, &limits, &NoMonitor)
            .map_err(failed)?;
        build_strip(value, req.grid.as_ref(), &codecs)
    })?;
    write_atomic(layout, &path, &png).map_err(|err| AnimError::Failed(err.to_string()))?;
    tracing::debug!(
        hash = req.hash,
        frames = meta.frame_count,
        cols = meta.cols,
        bytes = png.len(),
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "アニメの帯を作りました"
    );
    Ok(info_of(req, meta))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use kura_engine::doc::DecodeOptions;
    use kura_engine::geom::{IPoint, Size};
    use kura_engine::value::{
        Frame, Frames, Image, ImageMeta, Palette, PixelFormat, Raster, TagDirection,
    };
    use kura_engine::{Limits, NoMonitor};
    use rusqlite::params;
    use serde_json::json;

    use super::*;
    use crate::db::test_conn;
    use crate::edit::{EngineFactory, RuntimeConfig};
    use crate::library::parse_hex32;

    struct Factory {
        budget: u64,
    }

    impl EngineFactory for Factory {
        fn runtime_config(&self) -> RuntimeConfig {
            RuntimeConfig {
                cores: Some(2),
                budget: Some(self.budget),
                ..RuntimeConfig::default()
            }
        }

        fn codecs(&self, _work_dir: &Path) -> CodecSet {
            CodecSet::pinned()
        }
    }

    fn engine(budget: u64) -> EngineServices {
        EngineServices::new(Arc::new(Factory { budget }), PathBuf::from("/nowhere"))
    }

    fn solid(w: u32, h: u32, c: [u8; 4]) -> Raster {
        let data: Vec<u8> = (0..w * h).flat_map(|_| c).collect();
        Raster::from_rows(w, h, PixelFormat::Rgba8, None, &data).unwrap()
    }

    fn frames(
        list: Vec<(Raster, (i64, i64), u32)>,
        canvas: (u32, u32),
        tags: Vec<AnimTag>,
    ) -> Value {
        let frames = list
            .into_iter()
            .map(|(px, (x, y), duration_ms)| Frame {
                px,
                offset: IPoint { x, y },
                duration_ms,
            })
            .collect();
        let meta = ImageMeta {
            tags,
            ..ImageMeta::default()
        };
        let size = Size {
            w: canvas.0,
            h: canvas.1,
        };
        Value::Frames(Arc::new(Frames::new(frames, size, meta, 0).unwrap()))
    }

    /// 画素 (x, y) の色が [x, y, 7, 255] の画像。
    fn coords(w: u32, h: u32) -> Value {
        let data: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [x as u8, y as u8, 7, 255]))
            .collect();
        let px = Raster::from_rows(w, h, PixelFormat::Rgba8, None, &data).unwrap();
        Value::Image(Arc::new(Image {
            px,
            meta: ImageMeta::default(),
        }))
    }

    /// PNG を読んで (幅, 高さ, RGBA8 の行)。
    fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes.to_vec()),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(img) = v else {
            panic!("1 枚の画像ではありません")
        };
        assert_eq!(img.px.format(), PixelFormat::Rgba8);
        (img.px.width(), img.px.height(), img.px.to_rows().unwrap())
    }

    fn at(rows: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        rows[i..i + 4].try_into().unwrap()
    }

    fn grid(cell: u32, offset_x: u32, count: Option<u32>) -> GridSetting {
        GridSetting {
            cell_w: cell,
            cell_h: cell,
            offset_x,
            offset_y: 0,
            gap_x: 0,
            gap_y: 0,
            count,
        }
    }

    /// 帯は行ごとに左から cols 個ずつ隙間なく並べる（`stripCell` と同じ）。列は 1 辺に収まる最大から、その行の数で空く
    /// セルが最も少なくなるまで詰める。
    #[test]
    fn strip_layout_fills_rows_of_cols_without_gaps() {
        for (n, w, h, cols, rows) in [
            (1, 16, 16, 1, 1),
            (8, 16, 16, 8, 1),
            (33, 256, 256, 17, 2),
            (100, 100, 10, 50, 2),
            (4_096, 64, 64, 128, 32),
            (7, 8_192, 1, 1, 7),
        ] {
            let l = strip_layout(n, w, h).unwrap();
            assert_eq!(
                (l.frames, l.cols, l.rows, l.cell_w, l.cell_h),
                (n, cols, rows, w, h),
                "{n} × {w} × {h}"
            );
            assert!(l.width() <= MAX_STRIP_SIDE && l.height() <= MAX_STRIP_SIDE);
            assert!(u64::from(l.width()) * u64::from(l.height()) <= MAX_STRIP_PIXELS);
            assert!(
                (l.rows - 1) * l.cols < n && n <= l.rows * l.cols,
                "最後の行だけが欠ける"
            );
            for f in 0..n {
                let (x, y) = l.cell(f);
                assert_eq!(
                    (x, y),
                    ((f % l.cols) * w, (f / l.cols) * h),
                    "stripCell と同じ"
                );
                assert!(x + w <= l.width() && y + h <= l.height());
            }
        }
    }

    /// 1 辺 8,192・16M 画素・コマ 4,096 を超える帯は TooLarge。コマ 0・大きさ 0 はアニメではない。
    #[test]
    fn strip_layout_refuses_strips_over_the_limits() {
        for (n, w, h) in [
            (1, 8_193, 1),
            (1, 1, 8_193),
            (1, 8_192, 8_192),
            (4_096, 65, 64),
            (100, 8_192, 100),
            (MAX_FRAMES + 1, 1, 1),
        ] {
            assert!(
                matches!(strip_layout(n, w, h), Err(AnimError::TooLarge(_))),
                "{n} × {w} × {h}"
            );
        }
        for (n, w, h) in [(0, 8, 8), (2, 0, 8), (2, 8, 0)] {
            assert!(matches!(
                strip_layout(n, w, h),
                Err(AnimError::NotAnimated(_))
            ));
        }
    }

    /// コマはセルの中にキャンバスの offset で置き、透明を保つ（パレットの色・半透明もそのまま）。コマの長さ 0 はそのまま。
    #[test]
    fn frames_are_placed_at_their_offsets_and_keep_transparency() {
        let red = [255, 0, 0, 255];
        let glass = [0, 255, 0, 128];
        let value = frames(
            vec![
                (solid(2, 2, red), (1, 1), 0),
                (solid(4, 3, glass), (0, 0), 50),
                (solid(1, 1, [9, 8, 7, 255]), (3, 2), 100),
            ],
            (4, 3),
            vec![AnimTag {
                name: "歩く".into(),
                from: 0,
                to: 2,
                direction: TagDirection::PingPong,
                repeat: 0,
            }],
        );
        let (png, meta) = build_strip(value, None, &CodecSet::pinned()).unwrap();
        assert_eq!(
            (meta.canvas_w, meta.canvas_h, meta.cols, meta.frame_count),
            (4, 3, 3, 3)
        );
        assert_eq!(meta.durations, [0, 50, 100]);
        assert_eq!(meta.key, "frames");
        assert_eq!(meta.tags[0].name, "歩く");
        let (w, h, rows) = decode_png(&png);
        assert_eq!((w, h), (12, 3));
        assert_eq!(at(&rows, w, 0, 0), [0, 0, 0, 0], "コマの外は透明");
        assert_eq!(at(&rows, w, 1, 1), red);
        assert_eq!(at(&rows, w, 2, 2), red);
        assert_eq!(at(&rows, w, 3, 2), [0, 0, 0, 0]);
        for x in 4..8 {
            assert_eq!(at(&rows, w, x, 2), glass, "半透明はそのまま");
        }
        assert_eq!(at(&rows, w, 8 + 3, 2), [9, 8, 7, 255]);
        assert_eq!(at(&rows, w, 8, 0), [0, 0, 0, 0]);

        // パレットの画像（GIF・aseprite の Indexed）と 16 ビットの画像も RGBA8 に広げる
        let palette = Palette {
            colors: vec![[0, 0, 0, 0], [10, 20, 30, 255]],
        };
        let indexed = |i: u8| {
            Raster::from_rows(2, 1, PixelFormat::Indexed8, Some(palette.clone()), &[i, 1]).unwrap()
        };
        let value = frames(
            vec![(indexed(0), (0, 0), 10), (indexed(1), (0, 0), 10)],
            (2, 1),
            vec![],
        );
        let (png, _) = build_strip(value, None, &CodecSet::pinned()).unwrap();
        let (w, _, rows) = decode_png(&png);
        assert_eq!(at(&rows, w, 0, 0), [0, 0, 0, 0]);
        assert_eq!(at(&rows, w, 1, 0), [10, 20, 30, 255]);
        assert_eq!(at(&rows, w, 2, 0), [10, 20, 30, 255]);
        let deep = |v: u16| {
            let data: Vec<u8> = [v, 0, 65_535, 25_700]
                .iter()
                .flat_map(|c| c.to_le_bytes())
                .collect();
            Raster::from_rows(1, 1, PixelFormat::Rgba16, None, &data).unwrap()
        };
        let value = frames(
            vec![(deep(0), (0, 0), 1), (deep(65_535), (0, 0), 1)],
            (1, 1),
            vec![],
        );
        let (png, _) = build_strip(value, None, &CodecSet::pinned()).unwrap();
        let (w, _, rows) = decode_png(&png);
        assert_eq!(at(&rows, w, 0, 0), [0, 0, 255, 100]);
        assert_eq!(at(&rows, w, 1, 0), [255, 0, 255, 100]);
    }

    /// 同じ Frames からは同じバイト列。帯の情報は ASCII の JSON で PNG に入り、読み戻すと同じ（タグの名前の日本語も）。
    #[test]
    fn the_same_frames_make_the_same_bytes_and_carry_their_meta() {
        let make = || {
            frames(
                vec![
                    (solid(3, 2, [1, 2, 3, 255]), (0, 0), 30),
                    (solid(1, 1, [4, 5, 6, 255]), (2, 1), 0),
                ],
                (3, 2),
                vec![AnimTag {
                    name: "走る🏃".into(),
                    from: 0,
                    to: 1,
                    direction: TagDirection::Reverse,
                    repeat: 2,
                }],
            )
        };
        let (a, meta) = build_strip(make(), None, &CodecSet::pinned()).unwrap();
        let (b, _) = build_strip(make(), None, &CodecSet::pinned()).unwrap();
        assert_eq!(a, b);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("strip.png");
        std::fs::write(&path, &a).unwrap();
        assert_eq!(read_strip_meta(&path), Some(meta.clone()));
        assert_eq!(meta.tags[0].name, "走る🏃");
        // tEXt（Latin-1）に入るよう ASCII にしてある
        let text_at = a.windows(4).position(|w| w == b"tEXt").unwrap();
        let idat_at = a.windows(4).position(|w| w == b"IDAT").unwrap();
        assert!(text_at < idat_at, "IDAT の前");
        let len = u32::from_be_bytes(a[text_at - 4..text_at].try_into().unwrap()) as usize;
        let text = &a[text_at + 4..text_at + 4 + len];
        assert!(text.is_ascii());
        assert!(text.starts_with(b"kura-anim\0{"));
        assert!(
            text.windows(5).any(|w| w == b"u8d70"),
            "走（U+8D70）は JSON のエスケープで書く"
        );
        // 読めないもの
        std::fs::write(&path, b"not a png").unwrap();
        assert_eq!(read_strip_meta(&path), None);
        assert_eq!(read_strip_meta(&dir.path().join("none.png")), None);
    }

    /// 格子の帯：先頭のコマのセルを行ごとに左から切って並べる（`sprite.split_grid` と同じ切り方）。コマの数で絞る。
    #[test]
    fn grid_sheets_are_cut_into_cells_in_row_order() {
        let source = coords(5, 4);
        // 列は x = 1, 3、行は y = 0, 2
        let (png, meta) =
            build_strip(source.clone(), Some(&grid(2, 1, None)), &CodecSet::pinned()).unwrap();
        assert_eq!(
            (meta.canvas_w, meta.canvas_h, meta.frame_count, meta.cols),
            (2, 2, 4, 4)
        );
        assert_eq!(meta.durations, [0, 0, 0, 0]);
        assert!(meta.tags.is_empty());
        let (w, h, rows) = decode_png(&png);
        assert_eq!((w, h), (8, 2));
        let origins = [(1, 0), (3, 0), (1, 2), (3, 2)];
        for (f, (sx, sy)) in origins.iter().enumerate() {
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                assert_eq!(
                    at(&rows, w, f as u32 * 2 + dx, dy),
                    [(sx + dx) as u8, (sy + dy) as u8, 7, 255],
                    "コマ {f}"
                );
            }
        }
        let (_, three) = build_strip(
            source.clone(),
            Some(&grid(2, 1, Some(3))),
            &CodecSet::pinned(),
        )
        .unwrap();
        assert_eq!(three.frame_count, 3);
        assert_ne!(three.key, meta.key);
        assert!(rev_of(&meta.key) >= 1 && rev_of(&three.key) >= 1);
        assert_ne!(rev_of(&meta.key), rev_of(&three.key));
        assert_eq!(rev_of(&strip_key(None)), 0);
        // フレームの列にも格子を当てられる（先頭のコマをキャンバスに置いて切る）
        let animated = frames(
            vec![
                (solid(2, 2, [5, 5, 5, 255]), (2, 0), 10),
                (solid(4, 2, [6, 6, 6, 255]), (0, 0), 10),
            ],
            (4, 2),
            vec![],
        );
        let (png, meta) =
            build_strip(animated, Some(&grid(2, 0, None)), &CodecSet::pinned()).unwrap();
        assert_eq!(meta.frame_count, 2);
        let (w, _, rows) = decode_png(&png);
        assert_eq!(at(&rows, w, 0, 0), [0, 0, 0, 0]);
        assert_eq!(at(&rows, w, 2, 0), [5, 5, 5, 255]);
        // セルができない格子はアニメではない
        assert!(matches!(
            build_strip(source, Some(&grid(9, 0, None)), &CodecSet::pinned()),
            Err(AnimError::NotAnimated(_))
        ));
    }

    /// 格子のない 1 枚の画像と、コマが 1 つのフレームの列はアニメではない。
    #[test]
    fn still_images_without_a_grid_are_not_animated() {
        assert!(matches!(
            build_strip(coords(4, 4), None, &CodecSet::pinned()),
            Err(AnimError::NotAnimated(_))
        ));
        let one = frames(
            vec![(solid(2, 2, [1, 1, 1, 255]), (0, 0), 10)],
            (2, 2),
            vec![],
        );
        assert!(matches!(
            build_strip(one, None, &CodecSet::pinned()),
            Err(AnimError::NotAnimated(_))
        ));
    }

    /// ライブラリの files/ に値を PNG（フレームの列は APNG）で置く。
    fn place(layout: &Layout, b: u8, value: &Value) -> AnimRequest {
        let (bytes, _) = CodecSet::pinned()
            .encode_bytes(value, "png", &json!({}), &NoMonitor)
            .unwrap();
        let hash = format!("{b:02x}").repeat(32);
        let path = layout.content_path(&parse_hex32(&hash).unwrap(), "png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        AnimRequest {
            hash,
            ext: "png".into(),
            kind: MediaKind::Image,
            grid: None,
            frame_ms: None,
        }
    }

    fn two_frames() -> Value {
        frames(
            vec![
                (solid(8, 8, [1, 1, 1, 255]), (0, 0), 120),
                (solid(8, 8, [2, 2, 2, 255]), (0, 0), 0),
            ],
            (8, 8),
            vec![],
        )
    }

    /// 帯は 1 回だけ作り、あれば元を読まずに返す（元を壊しても返せる）。格子を変えると作り直し、格子の帯は rev と設定の
    /// コマの長さを返す。一時ファイルを残さない。
    #[test]
    fn get_writes_the_strip_once_and_rebuilds_when_the_grid_changes() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.staging_dir()).unwrap();
        let engine = engine(64 << 20);
        let req = place(&layout, 1, &two_frames());
        let info = get(&engine, &layout, &req).unwrap();
        assert_eq!(
            (
                info.version,
                info.rev,
                info.canvas_w,
                info.canvas_h,
                info.cols,
                info.frame_count
            ),
            (1, 0, 8, 8, 2, 2)
        );
        assert_eq!(info.durations, [120, 0]);
        assert_eq!(info.hash, req.hash);
        let strip = layout.anim_path(&req.hash);
        let (w, h, _) = decode_png(&std::fs::read(&strip).unwrap());
        assert_eq!((w, h), (16, 8));
        assert_eq!(
            engine.runtime().unwrap().budget().used().unwrap(),
            0,
            "予算は返した"
        );
        // 元を壊しても、帯があれば読まずに返す
        let source = layout.content_path(&parse_hex32(&req.hash).unwrap(), "png");
        let original = std::fs::read(&source).unwrap();
        std::fs::write(&source, b"broken").unwrap();
        assert_eq!(get(&engine, &layout, &req).unwrap(), info);
        // 格子を変えたら作り直す（元を読む）
        let sheet = AnimRequest {
            grid: Some(grid(4, 0, None)),
            frame_ms: Some(80),
            ..req.clone()
        };
        assert!(matches!(
            get(&engine, &layout, &sheet),
            Err(AnimError::Failed(_))
        ));
        std::fs::write(&source, original).unwrap();
        let cut = get(&engine, &layout, &sheet).unwrap();
        assert_eq!((cut.canvas_w, cut.frame_count, cut.cols), (4, 4, 4));
        assert_eq!(cut.durations, [80; 4], "格子の帯は設定のコマの長さ");
        assert!(cut.rev >= 1);
        assert_eq!(
            read_strip_meta(&strip).unwrap().key,
            strip_key(sheet.grid.as_ref())
        );
        // コマの長さだけを変えても作り直さない（帯の画素は同じ）
        std::fs::write(&source, b"broken").unwrap();
        let slower = AnimRequest {
            frame_ms: None,
            ..sheet.clone()
        };
        assert_eq!(get(&engine, &layout, &slower).unwrap().durations, [0; 4]);
        assert_eq!(
            std::fs::read_dir(layout.staging_dir()).unwrap().count(),
            0,
            "一時ファイルを残さない"
        );
    }

    /// 1 枚の画像・画像でないものはアニメではない。大きすぎる帯は読む前に TooLarge、予算が空いていなければ Busy。
    #[test]
    fn get_reports_not_animated_too_large_and_busy() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.staging_dir()).unwrap();
        let engine = engine(4 << 20);
        let still = place(&layout, 1, &coords(4, 4));
        assert!(matches!(
            get(&engine, &layout, &still),
            Err(AnimError::NotAnimated(_))
        ));
        let audio = AnimRequest {
            kind: MediaKind::Audio,
            ..still.clone()
        };
        assert!(matches!(
            get(&engine, &layout, &audio),
            Err(AnimError::NotAnimated(_))
        ));
        let missing = AnimRequest {
            hash: "ff".repeat(32),
            ..still.clone()
        };
        assert!(matches!(
            get(&engine, &layout, &missing),
            Err(AnimError::NotFound(_))
        ));
        let wide = frames(
            vec![
                (solid(8_193, 1, [1, 1, 1, 255]), (0, 0), 10),
                (solid(8_193, 1, [2, 2, 2, 255]), (0, 0), 10),
            ],
            (8_193, 1),
            vec![],
        );
        let wide = place(&layout, 2, &wide);
        assert!(matches!(
            get(&engine, &layout, &wide),
            Err(AnimError::TooLarge(_))
        ));
        assert_eq!(engine.runtime().unwrap().budget().used().unwrap(), 0);
        // 予算の全体を超える（帯 1,024 × 1,024 の 6 倍 > 4 MiB）
        let big = frames(
            vec![
                (solid(512, 1_024, [1, 1, 1, 255]), (0, 0), 10),
                (solid(512, 1_024, [2, 2, 2, 255]), (0, 0), 10),
            ],
            (512, 1_024),
            vec![],
        );
        let big = place(&layout, 3, &big);
        assert!(matches!(
            get(&engine, &layout, &big),
            Err(AnimError::TooLarge(_))
        ));
        // 空いていない
        let anim = place(&layout, 4, &two_frames());
        let budget = engine.runtime().unwrap().budget().clone();
        let held = budget
            .try_acquire(4 << 20, kura_engine::budget::Class::Interactive)
            .unwrap()
            .unwrap();
        assert!(matches!(
            get(&engine, &layout, &anim),
            Err(AnimError::Busy(_))
        ));
        drop(held);
        assert!(get(&engine, &layout, &anim).is_ok());
    }

    /// aseprite（タグとコマの長さ）とアニメの WebP（他のツールで作ったフィクスチャ）も、ファイルから帯にできる。
    #[test]
    fn get_reads_aseprite_tags_and_animated_webp() {
        use kura_engine::testfiles::{AseCel, AseFrame, AseLayer, FileGen};

        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let engine = engine(64 << 20);
        let put = |b: u8, ext: &str, bytes: &[u8]| -> AnimRequest {
            let hash = format!("{b:02x}").repeat(32);
            let path = layout.content_path(&parse_hex32(&hash).unwrap(), ext);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            AnimRequest {
                hash,
                ext: ext.into(),
                kind: MediaKind::Image,
                grid: None,
                frame_ms: None,
            }
        };
        let ase = FileGen::Aseprite {
            w: 8,
            h: 6,
            depth: 32,
            flags: 1,
            transparent: 0,
            palette_len: 4,
            layers: vec![AseLayer {
                flags: 1,
                kind: 0,
                level: 0,
                blend: 0,
                opacity: 255,
            }],
            frames: (0..3u16)
                .map(|i| AseFrame {
                    duration: 50 * (i + 1),
                    cels: vec![AseCel {
                        layer: 0,
                        x: i as i16,
                        y: 1,
                        opacity: 255,
                        z: 0,
                        kind: 0,
                        w: 3,
                        h: 2,
                        link: 0,
                        flip: false,
                    }],
                })
                .collect(),
            tileset: None,
            tags: vec![("歩く".into(), 0, 2)],
            slices: vec![],
            seed: 7,
            opaque_palette: true,
        }
        .bytes()
        .unwrap();
        let info = get(&engine, &layout, &put(1, "aseprite", &ase)).unwrap();
        assert_eq!((info.canvas_w, info.canvas_h, info.frame_count), (8, 6, 3));
        assert_eq!(info.durations, [50, 100, 150]);
        assert_eq!(info.tags.len(), 1);
        assert_eq!(
            (
                info.tags[0].name.as_str(),
                info.tags[0].from,
                info.tags[0].to
            ),
            ("歩く", 0, 2)
        );
        let (w, _, rows) = decode_png(&std::fs::read(layout.anim_path(&info.hash)).unwrap());
        assert_eq!(at(&rows, w, 0, 0)[3], 0, "セルの外は透明");

        let webp = std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../kura-engine/fixtures/webp-anim.webp"),
        )
        .unwrap();
        let info = get(&engine, &layout, &put(2, "webp", &webp)).unwrap();
        assert_eq!(info.frame_count, 3);
        assert_eq!(info.durations.len(), 3);
        assert!(layout.anim_path(&info.hash).is_file());
    }

    /// アイテムの帯の元：ハッシュ・拡張子・種類と、格子・コマの長さの設定。
    #[test]
    fn request_of_reads_the_item_and_its_grid() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at,
                                proxy_state, analysis_state)
             VALUES (1, ?1, 'aseprite', 'image', 1, 't', 't', 1, 1, 'not_needed', 'pending')",
            params![vec![0xabu8; 32]],
        )
        .unwrap();
        let req = request_of(&conn, 1).unwrap();
        assert_eq!(req.hash, "ab".repeat(32));
        assert_eq!((req.ext.as_str(), req.kind), ("aseprite", MediaKind::Image));
        assert_eq!((req.grid, req.frame_ms), (None, None));
        conn.execute(
            r#"INSERT INTO asset_settings (item_id, grid_json, frame_ms, updated_at)
               VALUES (1, '{"v":1,"cellW":4,"cellH":4,"offsetX":1,"offsetY":0,"gapX":0,"gapY":0,"count":null}', 90, 1)"#,
            [],
        )
        .unwrap();
        let req = request_of(&conn, 1).unwrap();
        assert_eq!(req.grid, Some(grid(4, 1, None)));
        assert_eq!(req.frame_ms, Some(90));
        assert!(matches!(request_of(&conn, 2), Err(CoreError::NotFound(_))));
    }

    /// 測る（`cargo test --release -p kura-core measure_anim -- --ignored --nocapture`）：GIF・APNG・アニメの WebP と
    /// 格子のシート、上限いっぱいの帯を、最初（読む・組む・書く）と 2 回目（帯の情報だけを読む）で。
    #[test]
    #[ignore = "時間を測るだけ（release で回す）"]
    fn measure_anim() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let engine = engine(2 << 30);
        let noise = |n: u32, w: u32, h: u32, seed: u64| -> Value {
            kura_engine::testgen::InputGen::NoiseFrames {
                w,
                h,
                fmt: PixelFormat::Rgba8,
                count: n,
                seed,
            }
            .generate()
            .unwrap()
        };
        // 透明の地に、コマごとに位置と色の違う四角（ドット絵のスプライトらしいもの。書き手はキャンバスいっぱいのコマだけ）
        let sprite = |n: u32, cell: u32| -> Value {
            frames(
                (0..n)
                    .map(|i| {
                        let c = [(i * 37 % 256) as u8, (i * 11 % 256) as u8, 90, 255];
                        let data: Vec<u8> = (0..cell)
                            .flat_map(|y| {
                                (0..cell).flat_map(move |x| {
                                    let inside = (i % 4..i % 4 + cell / 2).contains(&x)
                                        && (3..3 + cell / 2).contains(&y);
                                    if inside { c } else { [0, 0, 0, 0] }
                                })
                            })
                            .collect();
                        let px =
                            Raster::from_rows(cell, cell, PixelFormat::Rgba8, None, &data).unwrap();
                        (px, (0, 0), 80)
                    })
                    .collect(),
                (cell, cell),
                vec![],
            )
        };
        let cases: Vec<(&str, &str, Value, Option<GridSetting>)> = vec![
            (
                "GIF 64 コマ 128²（乱数）",
                "gif",
                noise(64, 128, 128, 1),
                None,
            ),
            (
                "APNG 64 コマ 256²（乱数）",
                "png",
                noise(64, 256, 256, 2),
                None,
            ),
            ("GIF 96 コマ 64²（スプライト）", "gif", sprite(96, 64), None),
            (
                "格子 1024² を 32² で 1024 コマ",
                "png",
                coords(1024, 1024),
                Some(grid(32, 0, None)),
            ),
            (
                "上限 4,096 コマ 64²（8192 × 2048。APNG）",
                "png",
                sprite(4_096, 64),
                None,
            ),
        ];
        // アニメの WebP はエンジンに書き手がない（読むだけ）ので測っていない
        for (b, (what, format, value, grid)) in (1u8..).zip(cases) {
            let (bytes, _) =
                match CodecSet::pinned().encode_bytes(&value, format, &json!({}), &NoMonitor) {
                    Ok(v) => v,
                    Err(err) => {
                        eprintln!("{what}: 書けません（{err}）");
                        continue;
                    }
                };
            let hash = format!("{b:02x}").repeat(32);
            let ext = if format == "png" { "png" } else { format };
            let path = layout.content_path(&parse_hex32(&hash).unwrap(), ext);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let req = AnimRequest {
                hash: hash.clone(),
                ext: ext.into(),
                kind: MediaKind::Image,
                grid,
                frame_ms: None,
            };
            let started = std::time::Instant::now();
            let info = get(&engine, &layout, &req);
            let first_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = std::time::Instant::now();
            let again = get(&engine, &layout, &req);
            let second_ms = started.elapsed().as_secs_f64() * 1000.0;
            let strip = std::fs::metadata(layout.anim_path(&hash))
                .map(|m| m.len())
                .unwrap_or(0);
            eprintln!(
                "{what}: 元 {} バイト、最初 {first_ms:.1} ms、2 回目 {second_ms:.2} ms、帯 {strip} バイト、{:?}",
                bytes.len(),
                info.as_ref()
                    .map(|i| (i.frame_count, i.cols, i.canvas_w, i.canvas_h))
                    .map_err(ToString::to_string),
            );
            assert_eq!(info.ok(), again.ok());
        }
    }
}
