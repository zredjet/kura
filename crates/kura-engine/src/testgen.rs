//! 例とテストの入力を、記述から決定的に作る（docs/editing.md §8.5）。バイナリのフィクスチャを置かずに済ませる。

use std::sync::Arc;

use rand_chacha::rand_core::Rng;
use serde::{Deserialize, Serialize};

use crate::determinism;
use crate::error::EngineError;
use crate::geom::{IPoint, IRect};
use crate::math;
use crate::value::{
    Audio, AudioMeta, Frames, Grid, Image, ImageMeta, LoopRange, Palette, PixelFormat, Raster,
    Value,
};

/// 入力の作り方。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type",
    deny_unknown_fields
)]
pub enum InputGen {
    /// 2 色の市松模様。
    Checker {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        cell: u32,
    },
    /// ChaCha8 の乱数の画素（Indexed8 は 16 色のパレット）。
    Noise {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        seed: u64,
    },
    /// 透明な余白に囲まれた不透明な矩形（Rgba8）。
    Sprite { w: u32, h: u32, rect: IRect },
    /// セルごとに色の違うスプライトシート（Rgba8）。格子とピボットのメタデータ付き。
    Sheet { cols: u32, rows: u32, cell: u32 },
    /// 乱数のフレームの列。
    NoiseFrames {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        count: u32,
        seed: u64,
    },
    /// トリムしたフレームの列：`w × h` のキャンバスに、大きさと位置の違う乱数のフレームを置く
    /// （フレーム i は 幅 `w/2 + i % (w/2)`・高さ `h/2` を、左上 `(i % (w/2), i % (h/2))` に置く）。
    TrimmedFrames {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        count: u32,
        seed: u64,
    },
    /// 元の作り方の画像（フレームの列なら各フレームと位置・キャンバス）を、最近傍で横 `kx`・縦 `ky` 倍にしたもの
    /// （ドット絵の整数倍の拡大）。倍率は 1 以上（0 は 1 とみなす）。メタデータは持たない。
    Upscale {
        base: Box<InputGen>,
        kx: u32,
        ky: u32,
    },
    /// 元の作り方の画像（1 枚）から矩形 `rect`（画像の中に収まるもの）を切り抜いたもの。メタデータは持たない。
    Crop { base: Box<InputGen>, rect: IRect },
    /// 格子に並べたスプライトシート（Rgba8）。大きさは 横 `marginX + cols·cellW + (cols − 1)·gapX + padRight`・
    /// 縦 `marginY + rows·cellH + (rows − 1)·gapY + padBottom`。地は `background`（null は透明 [0,0,0,0]）。
    /// セル（行ごとに左から番号を振る。`empty` の番号は描かない）の中身は、`full` ならセルいっぱい、そうでなければ
    /// セルの縁の 1 画素を空けた矩形（幅 `max(⌊cellW/2⌋, 1)〜cellW − 2`・左端 1〜。セルの番号を流れにした ChaCha8 で
    /// 決める。3 画素より小さいセルはいっぱい）。中身の色は画素の位置から決め（不透明。地の色と同じなら赤の 2 の位を
    /// 反転）、`full` でなければ矩形の縁を除く内側の 11 画素に 1 つほどを地のまま残す（穴）。
    SpriteSheet {
        cols: u32,
        rows: u32,
        cell_w: u32,
        cell_h: u32,
        gap_x: u32,
        gap_y: u32,
        margin_x: u32,
        margin_y: u32,
        pad_right: u32,
        pad_bottom: u32,
        background: Option<[u8; 4]>,
        full: bool,
        empty: Vec<u32>,
        seed: u64,
    },
    /// 正弦波（振幅 0.5）。チャンネルごとに周波数を 1.5 倍ずつ上げる。
    Sine {
        rate: u32,
        channels: u16,
        frames: u64,
        freq: u32,
        loop_range: Option<LoopRange>,
    },
    /// 区間ごとに大きさの違う 1 チャンネルの正弦波：区間は（フレームの数, dBFS の振幅）。正規化の絶対ゲートをまたぐ形を作る。
    SineLevels {
        rate: u32,
        freq: u32,
        segments: Vec<(u64, i32)>,
    },
    /// -1〜1 の乱数の音声。
    AudioNoise {
        rate: u32,
        channels: u16,
        frames: u64,
        seed: u64,
    },
    /// 元の作り方の値にメタデータを付ける（コーデックのゴールデンでメタデータのチャンクを凍結する）。
    WithMeta { base: Box<InputGen>, meta: MetaGen },
    /// 衝撃の繰り返し（Ogg Vorbis の短い窓と aoTuV の M3 に届く入力。44.1 kHz・品質 0 では M2 にも届く。M9 は `Swell`）：`period` フレームごとに、16 フレームの
    /// 雑音（振幅 0.95。チャンネルごとの ChaCha8 の流れから順に取る）と、そのあと `rate × 200 / 44,100` フレーム（約 4.5 ms）
    /// だけ続いて急に止まる 700 Hz の正弦波（振幅 0.3。位相はフレームの位置から）。残りは無音。
    Bursts {
        rate: u32,
        channels: u16,
        frames: u64,
        period: u64,
        seed: u64,
    },
    /// だんだん大きくなる音（Ogg Vorbis の長い窓で、前のブロックより大きくなるトーン。aoTuV の M9 に届く入力）：周波数
    /// `freqs`（Hz）の正弦波の平均を、振幅 `from_db` から `to_db`（dBFS）まで、フレームの位置に比例して dB で大きくする。
    /// 位相はフレームの位置から、チャンネル c・k 番めの周波数は `0.3 c + k` ラジアンずらす。
    Swell {
        rate: u32,
        channels: u16,
        frames: u64,
        freqs: Vec<u32>,
        from_db: i32,
        to_db: i32,
    },
    /// 色の見本（Rgba8）：画素の番号 i（行の順。i = y·w + x）を `count` で割った余り k の色
    /// [k の下位 8 ビット, 次の 8 ビット, その次の 8 ビット, a] で塗る。a は k < `clear` なら 0（RGB の違う透明な色）、
    /// ほかは 255。`count` は 1 以上（0 は 1 とみなす）。`count` ≤ w·h かつ `count` ≤ 2²⁴ なら、違う色がちょうど
    /// `count` 個（そのうち透明な色が min(`clear`, `count`) 個）。色の数の境（256・257 色）を作る。
    Swatches {
        w: u32,
        h: u32,
        count: u32,
        clear: u32,
    },
    /// 画素を 1 つずつ書いた小さな画像（Rgba8）：`index`（行の順に w·h 個）は `colors`（ストレートの RGBA8）の番号。
    /// 数が合わない・番号が範囲の外なら誤り。透明で RGB の違う画素など、ほかの作り方で作れない並びを書く。
    Pixels {
        w: u32,
        h: u32,
        colors: Vec<[u8; 4]>,
        index: Vec<u8>,
    },
    /// 元の作り方の音声の、指定の位置の標本を NaN・±∞ にしたもの：`at` の（チャンネル, フレーム, 値）ごとに置き換える
    /// （同じ位置は後のものが勝つ）。範囲の外の位置・音声でない元は誤り。レート・メタデータはそのまま。
    NonFinite {
        base: Box<InputGen>,
        at: Vec<(u16, u64, NonFiniteSample)>,
    },
    /// 写真らしい合成の画像（Rgb8。ベンチの入力。整数だけで作る）：粗い格子（間隔 `max(16, ⌊長辺/6⌋)`）の色を双一次で
    /// 補間した大きな濃淡に、細かい格子（その 1/8。2 以上）の ±32 の揺らぎ、画素ごとの −6〜6 の雑音、縁のはっきりした
    /// 12 個の円（色を −64〜64 ずらす）を足して 0〜255 に切る。格子の色と雑音は `splitmix64`、円は ChaCha8 で種から決める。
    Photo { w: u32, h: u32, seed: u64 },
    /// ドット絵のスプライト（`fmt` は Rgba8 か Indexed8。色は 16 色のパレットで、0 番は透明）：各辺に透明な余白
    /// （1〜max(1, ⌊辺/4⌋) 画素）を残した内側の楕円を、2〜4 画素の塊ごとに 1〜15 番の色で塗る（余白と塊の大きさは
    /// ChaCha8、塊の色は `splitmix64` で種から決める）。Rgba8 はパレットの色（透明は [0,0,0,0]）。
    PixelSprite {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        seed: u64,
    },
}

/// `InputGen::NonFinite` で置く値。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NonFiniteSample {
    /// NaN（`f32::NAN`）。
    Nan,
    /// +∞。
    Inf,
    /// −∞。
    NegInf,
}

impl NonFiniteSample {
    fn value(self) -> f32 {
        match self {
            Self::Nan => f32::NAN,
            Self::Inf => f32::INFINITY,
            Self::NegInf => f32::NEG_INFINITY,
        }
    }
}

/// 付けるメタデータ。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct MetaGen {
    pub dpi: Option<(u32, u32)>,
    pub text: Vec<(String, String)>,
    pub icc: Option<Vec<u8>>,
    pub loop_range: Option<LoopRange>,
    pub markers: Vec<(u64, String)>,
    pub tags: Vec<(String, String)>,
    /// EXIF（TIFF の形）。ないときは書かない（足す前に凍結した入力の書き方を変えない）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exif: Option<Vec<u8>>,
    /// スライス（画像だけ）。ないときは書かない（同上）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slices: Vec<crate::value::Slice>,
}

impl MetaGen {
    fn apply_image(&self, meta: &mut ImageMeta) {
        meta.dpi = self.dpi.or(meta.dpi);
        meta.text.extend(self.text.iter().cloned());
        if let Some(icc) = &self.icc {
            meta.icc = Some(Arc::new(icc.clone()));
        }
        if let Some(exif) = &self.exif {
            meta.exif = Some(Arc::new(exif.clone()));
        }
        meta.slices.extend(self.slices.iter().cloned());
    }

    fn apply_audio(&self, meta: &mut AudioMeta) {
        meta.loop_range = self.loop_range.or(meta.loop_range);
        meta.markers
            .extend(self.markers.iter().map(|(pos, name)| crate::value::Marker {
                pos: *pos,
                name: name.clone(),
            }));
        meta.tags.extend(self.tags.iter().cloned());
    }
}

fn palette16() -> Palette {
    Palette {
        colors: (0..16u8)
            .map(|i| [i * 16, 255 - i * 16, i * 7, if i == 0 { 0 } else { 255 }])
            .collect(),
    }
}

fn noise_raster(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Result<Raster, EngineError> {
    let palette = (fmt == PixelFormat::Indexed8).then(palette16);
    let bpp = fmt.bytes_per_pixel();
    Raster::from_tile_fn(
        w,
        h,
        fmt,
        palette,
        &crate::monitor::NoMonitor,
        |rect, out| {
            // 行ごとの流れの中で x の位置へシークするので、タイルの分け方・並列によらず同じ中身になる
            let row_len = rect.w as usize * bpp;
            for (r, row) in out.chunks_exact_mut(row_len).enumerate() {
                let mut rng = determinism::rng_stream(seed, rect.y as u64 + r as u64);
                rng.set_word_pos(rect.x as u128 * bpp as u128);
                for b in row.iter_mut() {
                    let v = rng.next_u32() as u8;
                    *b = if fmt == PixelFormat::Indexed8 {
                        v & 0x0f
                    } else {
                        v
                    };
                }
            }
            Ok(())
        },
    )
}

/// 最近傍の整数倍の拡大（出力の (x, y) は入力の (⌊x/kx⌋, ⌊y/ky⌋)）。形式・パレットはそのまま。
fn upscale(px: &Raster, kx: u32, ky: u32) -> Result<Raster, EngineError> {
    let too_large = || EngineError::Limit("大きすぎます".into());
    let (w, h) = (
        px.width().checked_mul(kx).ok_or_else(too_large)?,
        px.height().checked_mul(ky).ok_or_else(too_large)?,
    );
    let bpp = px.bpp();
    px.derive(w, h, &crate::monitor::NoMonitor, |t, out| {
        let (x0, y0) = (t.x as u32, t.y as u32);
        let (sx0, sy0) = (x0 / kx, y0 / ky);
        let (sx1, sy1) = ((x0 + t.w - 1) / kx + 1, (y0 + t.h - 1) / ky + 1);
        let sw = (sx1 - sx0) as usize;
        let mut src = vec![0u8; sw * (sy1 - sy0) as usize * bpp];
        px.read_rect(
            IRect::new(i64::from(sx0), i64::from(sy0), sx1 - sx0, sy1 - sy0),
            &mut src,
        )?;
        for (i, dst) in out.chunks_exact_mut(bpp).enumerate() {
            let (x, y) = (x0 + i as u32 % t.w, y0 + i as u32 / t.w);
            let at = (((y / ky - sy0) as usize) * sw + (x / kx - sx0) as usize) * bpp;
            let s = src
                .get(at..at + bpp)
                .ok_or_else(|| EngineError::Internal("拡大の読み元が範囲の外です".into()))?;
            dst.copy_from_slice(s);
        }
        Ok(())
    })
}

/// `InputGen::Photo` を作る。
fn photo(w: u32, h: u32, seed: u64) -> Result<Raster, EngineError> {
    let coarse = (w.max(h) / 6).max(16);
    let fine = (coarse / 8).max(2);
    // 格子の点の色（層ごとに別の流れ。画素の位置から決まるので、タイルの分け方によらない）
    let lattice = |layer: u64, gx: u32, gy: u32| -> [i64; 3] {
        let v = determinism::splitmix64(
            determinism::splitmix64(seed ^ layer) ^ (u64::from(gy) << 32 | u64::from(gx)),
        );
        [
            (v & 0xff) as i64,
            ((v >> 8) & 0xff) as i64,
            ((v >> 16) & 0xff) as i64,
        ]
    };
    // 間隔 g の格子を双一次で補間する（整数。四捨五入）
    let bilinear = |layer: u64, g: u32, x: u32, y: u32| -> [i64; 3] {
        let (gx, gy) = (x / g, y / g);
        let (fx, fy) = (i64::from(x % g), i64::from(y % g));
        let g = i64::from(g);
        let (c00, c10) = (lattice(layer, gx, gy), lattice(layer, gx + 1, gy));
        let (c01, c11) = (lattice(layer, gx, gy + 1), lattice(layer, gx + 1, gy + 1));
        let mut out = [0i64; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let s = c00[k] * (g - fx) * (g - fy)
                + c10[k] * fx * (g - fy)
                + c01[k] * (g - fx) * fy
                + c11[k] * fx * fy;
            *o = (s + g * g / 2) / (g * g);
        }
        out
    };
    // 縁のはっきりした円：(中心 x, 中心 y, 半径, 色のずれ)
    let short = i64::from(w.min(h).max(1));
    let mut r = determinism::rng_stream(seed, 1);
    let discs: Vec<(i64, i64, i64, [i64; 3])> = (0..12)
        .map(|_| {
            let cx = i64::from(r.next_u32() % w.max(1));
            let cy = i64::from(r.next_u32() % h.max(1));
            let rad = short / 20 + 1 + i64::from(r.next_u32()) % (short / 5 + 1);
            let mut shift = [0i64; 3];
            for s in &mut shift {
                *s = i64::from(r.next_u32() % 129) - 64;
            }
            (cx, cy, rad, shift)
        })
        .collect();
    Raster::from_tile_fn(
        w,
        h,
        PixelFormat::Rgb8,
        None,
        &crate::monitor::NoMonitor,
        |t, out| {
            for (i, px) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let x = t.x as u32 + i as u32 % t.w;
                let y = t.y as u32 + i as u32 / t.w;
                let base = bilinear(0, coarse, x, y);
                let detail = bilinear(1, fine, x, y);
                let n = determinism::splitmix64(
                    seed ^ 0x9e37_79b9 ^ (u64::from(y) << 32 | u64::from(x)),
                );
                let mut c = [0i64; 3];
                for k in 0..3 {
                    c[k] = base[k] + (detail[k] - 128) / 4 + ((n >> (8 * k)) % 13) as i64 - 6;
                }
                for (cx, cy, rad, shift) in &discs {
                    let (dx, dy) = (i64::from(x) - cx, i64::from(y) - cy);
                    if dx * dx + dy * dy <= rad * rad {
                        for k in 0..3 {
                            c[k] += shift[k];
                        }
                    }
                }
                for (o, v) in px.iter_mut().zip(c) {
                    *o = v.clamp(0, 255) as u8;
                }
            }
            Ok(())
        },
    )
}

/// `InputGen::PixelSprite` を作る。
fn pixel_sprite(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Result<Raster, EngineError> {
    if !matches!(fmt, PixelFormat::Rgba8 | PixelFormat::Indexed8) {
        return Err(EngineError::Internal(
            "ドット絵のスプライトは Rgba8 か Indexed8 です".into(),
        ));
    }
    let palette = palette16();
    let mut r = determinism::rng(seed);
    let mut margin = |side: u32| 1 + r.next_u32() % (side / 4).max(1);
    let (ml, mr, mt, mb) = (margin(w), margin(w), margin(h), margin(h));
    let block = 2 + r.next_u32() % 3;
    let (x0, x1) = (i64::from(ml), i64::from(w) - i64::from(mr));
    let (y0, y1) = (i64::from(mt), i64::from(h) - i64::from(mb));
    let (cw, ch) = ((x1 - x0).max(0) as i128, (y1 - y0).max(0) as i128);
    // 内側の矩形に内接する楕円の中か（画素の中心で、座標を 2 倍した整数で比べる）
    let inside = |x: i64, y: i64| -> bool {
        if cw == 0 || ch == 0 {
            return false;
        }
        let dx = i128::from(2 * x + 1) - i128::from(x0 + x1);
        let dy = i128::from(2 * y + 1) - i128::from(y0 + y1);
        dx * dx * ch * ch + dy * dy * cw * cw <= cw * cw * ch * ch
    };
    let bpp = fmt.bytes_per_pixel();
    let colors = palette.colors.clone();
    Raster::from_tile_fn(
        w,
        h,
        fmt,
        (fmt == PixelFormat::Indexed8).then_some(palette),
        &crate::monitor::NoMonitor,
        |t, out| {
            for (i, px) in out.chunks_exact_mut(bpp).enumerate() {
                let x = t.x + i64::from(i as u32 % t.w);
                let y = t.y + i64::from(i as u32 / t.w);
                let idx = if inside(x, y) {
                    let (bx, by) = (
                        (x - x0) as u64 / u64::from(block),
                        (y - y0) as u64 / u64::from(block),
                    );
                    1 + (determinism::splitmix64(seed ^ (by << 32 | bx)) % 15) as u8
                } else {
                    0
                };
                if fmt == PixelFormat::Indexed8 {
                    px[0] = idx;
                } else if idx == 0 {
                    px.copy_from_slice(&[0, 0, 0, 0]);
                } else {
                    px.copy_from_slice(&colors[usize::from(idx)]);
                }
            }
            Ok(())
        },
    )
}

/// `InputGen::SpriteSheet` を作る（`cells` は (cols, rows, cellW, cellH)、`space` は (gapX, gapY, marginX, marginY,
/// padRight, padBottom)）。
fn sprite_sheet(
    cells: (u32, u32, u32, u32),
    space: (u32, u32, u32, u32, u32, u32),
    background: Option<[u8; 4]>,
    full: bool,
    empty: &[u32],
    seed: u64,
) -> Result<Value, EngineError> {
    let (cols, rows, cw, ch) = cells;
    let (gx, gy, mx, my, pr, pb) = space;
    if cols == 0 || rows == 0 || cw == 0 || ch == 0 {
        return Err(EngineError::Internal(
            "スプライトシートのセルの数・大きさは 1 以上".into(),
        ));
    }
    let len = |m: u32, n: u32, c: u32, g: u32, p: u32| {
        u64::from(m) + u64::from(n) * u64::from(c) + u64::from(n - 1) * u64::from(g) + u64::from(p)
    };
    let too_large = |_| EngineError::Limit("大きすぎます".into());
    let w = u32::try_from(len(mx, cols, cw, gx, pr)).map_err(too_large)?;
    let h = u32::try_from(len(my, rows, ch, gy, pb)).map_err(too_large)?;
    let bg = background.unwrap_or([0, 0, 0, 0]);
    // セルごとの中身の矩形（セルの中の座標）。描かないセルは None
    let rects: Vec<Option<(u32, u32, u32, u32)>> = (0..u64::from(cols) * u64::from(rows))
        .map(|n| {
            if empty.iter().any(|e| u64::from(*e) == n) {
                return None;
            }
            if full {
                return Some((0, 0, cw, ch));
            }
            let mut r = determinism::rng_stream(seed, n);
            // セルの縁の 1 画素は空けておく（3 画素より小さいセルはいっぱい）
            let mut pick = |c: u32| {
                if c < 3 {
                    return (c, 0);
                }
                let lo = (c / 2).max(1);
                let s = lo + r.next_u32() % (c - 2 - lo + 1);
                let o = 1 + r.next_u32() % (c - 2 - s + 1);
                (s, o)
            };
            let (sw, ox) = pick(cw);
            let (sh, oy) = pick(ch);
            Some((ox, oy, sw, sh))
        })
        .collect();
    let (period_x, period_y) = (u64::from(cw) + u64::from(gx), u64::from(ch) + u64::from(gy));
    // 軸の位置 v から (セルの番号, セルの中の位置)。セルの外（余白・間隔・余り）は None
    let cell = |v: u64, m: u32, period: u64, c: u32, n: u32| {
        let v = v.checked_sub(u64::from(m))?;
        let (k, at) = (v / period, v % period);
        (k < u64::from(n) && at < u64::from(c)).then_some((k, at as u32))
    };
    let px = Raster::from_tile_fn(
        w,
        h,
        PixelFormat::Rgba8,
        None,
        &crate::monitor::NoMonitor,
        |t, out| {
            for (i, chunk) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = t.x as u64 + u64::from(i as u32 % t.w);
                let y = t.y as u64 + u64::from(i as u32 / t.w);
                let mut color = bg;
                if let (Some((ci, lx)), Some((cj, ly))) = (
                    cell(x, mx, period_x, cw, cols),
                    cell(y, my, period_y, ch, rows),
                ) && let Some(Some((ox, oy, sw, sh))) =
                    rects.get((cj * u64::from(cols) + ci) as usize)
                    && (*ox..ox + sw).contains(&lx)
                    && (*oy..oy + sh).contains(&ly)
                {
                    let v = determinism::splitmix64(seed ^ (y << 32 | x));
                    let edge = lx == *ox || ly == *oy || lx + 1 == ox + sw || ly + 1 == oy + sh;
                    if full || edge || !v.is_multiple_of(11) {
                        color = [(v >> 8) as u8 | 1, (v >> 16) as u8, (v >> 24) as u8, 255];
                        if color == bg {
                            color[0] ^= 2;
                        }
                    }
                }
                chunk.copy_from_slice(&color);
            }
            Ok(())
        },
    )?;
    Ok(Value::Image(Arc::new(Image {
        px,
        meta: ImageMeta::default(),
    })))
}

impl InputGen {
    /// 値を作る。
    pub fn generate(&self) -> Result<Value, EngineError> {
        // メタデータを付ける作り方は、元の作り方の値に付けて返す
        if let Self::WithMeta { base, meta } = self {
            {
                return Ok(match base.generate()? {
                    Value::Image(img) => {
                        let mut img = Arc::unwrap_or_clone(img);
                        meta.apply_image(&mut img.meta);
                        Value::Image(Arc::new(img))
                    }
                    Value::Frames(fr) => {
                        let mut fr = Arc::unwrap_or_clone(fr);
                        meta.apply_image(&mut fr.meta);
                        Value::Frames(Arc::new(fr))
                    }
                    Value::Audio(a) => {
                        let mut a = Arc::unwrap_or_clone(a);
                        meta.apply_audio(&mut a.meta);
                        Value::Audio(Arc::new(a))
                    }
                });
            }
        }
        match *self {
            Self::Checker { w, h, fmt, cell } => {
                let bpp = fmt.bytes_per_pixel();
                let palette = (fmt == PixelFormat::Indexed8).then(palette16);
                let cell = cell.max(1);
                let px = Raster::from_tile_fn(
                    w,
                    h,
                    fmt,
                    palette,
                    &crate::monitor::NoMonitor,
                    |rect, out| {
                        // Indexed8 はパレットの 1 番と 0 番、ほかは 0xff と 0x01
                        let (on_v, off_v) = if fmt == PixelFormat::Indexed8 {
                            (1, 0)
                        } else {
                            (0xff, 0x01)
                        };
                        for (i, chunk) in out.chunks_exact_mut(bpp).enumerate() {
                            let x = rect.x as u32 + i as u32 % rect.w;
                            let y = rect.y as u32 + i as u32 / rect.w;
                            let on = ((x / cell) + (y / cell)).is_multiple_of(2);
                            chunk.fill(if on { on_v } else { off_v });
                        }
                        Ok(())
                    },
                )?;
                Ok(Value::Image(Arc::new(Image {
                    px,
                    meta: ImageMeta::default(),
                })))
            }
            Self::Noise { w, h, fmt, seed } => Ok(Value::Image(Arc::new(Image {
                px: noise_raster(w, h, fmt, seed)?,
                meta: ImageMeta::default(),
            }))),
            Self::Sprite { w, h, rect } => {
                let px = Raster::from_tile_fn(
                    w,
                    h,
                    PixelFormat::Rgba8,
                    None,
                    &crate::monitor::NoMonitor,
                    |t, out| {
                        for (i, chunk) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                            let x = t.x + i64::from(i as u32 % t.w);
                            let y = t.y + i64::from(i as u32 / t.w);
                            let inside =
                                x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom();
                            chunk.copy_from_slice(if inside {
                                &[200, 80, 40, 255]
                            } else {
                                &[0, 0, 0, 0]
                            });
                        }
                        Ok(())
                    },
                )?;
                Ok(Value::Image(Arc::new(Image {
                    px,
                    meta: ImageMeta::default(),
                })))
            }
            Self::Sheet { cols, rows, cell } => {
                let (w, h) = (cols * cell, rows * cell);
                let px = Raster::from_tile_fn(
                    w,
                    h,
                    PixelFormat::Rgba8,
                    None,
                    &crate::monitor::NoMonitor,
                    |t, out| {
                        for (i, chunk) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                            let x = t.x as u32 + i as u32 % t.w;
                            let y = t.y as u32 + i as u32 / t.w;
                            let n = (y / cell) * cols + x / cell;
                            let edge = x.is_multiple_of(cell) || y.is_multiple_of(cell);
                            chunk.copy_from_slice(&[
                                (n * 37) as u8,
                                (n * 91) as u8,
                                if edge { 0 } else { 255 },
                                255,
                            ]);
                        }
                        Ok(())
                    },
                )?;
                let meta = ImageMeta {
                    grid: Some(Grid {
                        cell_w: cell,
                        cell_h: cell,
                        offset_x: 0,
                        offset_y: 0,
                        gap_x: 0,
                        gap_y: 0,
                    }),
                    pivot: Some(IPoint {
                        x: i64::from(cell / 2),
                        y: i64::from(cell),
                    }),
                    ..ImageMeta::default()
                };
                Ok(Value::Image(Arc::new(Image { px, meta })))
            }
            Self::NoiseFrames {
                w,
                h,
                fmt,
                count,
                seed,
            } => {
                let frames = (0..count.max(1))
                    .map(|i| {
                        Ok((
                            noise_raster(w, h, fmt, determinism::splitmix64(seed ^ u64::from(i)))?,
                            100,
                        ))
                    })
                    .collect::<Result<Vec<_>, EngineError>>()?;
                Ok(Value::Frames(Arc::new(Frames::full(
                    frames,
                    ImageMeta::default(),
                    0,
                )?)))
            }
            Self::TrimmedFrames {
                w,
                h,
                fmt,
                count,
                seed,
            } => {
                let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));
                let frames = (0..count.max(1))
                    .map(|i| {
                        let (fw, fh) = ((hw + i % hw).min(w), hh);
                        let (ox, oy) = (i % hw, i % hh);
                        let (ox, oy) = (ox.min(w - fw), oy.min(h - fh));
                        Ok(crate::value::Frame {
                            px: noise_raster(
                                fw,
                                fh,
                                fmt,
                                determinism::splitmix64(seed ^ u64::from(i)),
                            )?,
                            offset: IPoint {
                                x: i64::from(ox),
                                y: i64::from(oy),
                            },
                            duration_ms: 50 + 10 * i,
                        })
                    })
                    .collect::<Result<Vec<_>, EngineError>>()?;
                Ok(Value::Frames(Arc::new(Frames::new(
                    frames,
                    crate::geom::Size { w, h },
                    ImageMeta::default(),
                    0,
                )?)))
            }
            Self::Upscale { ref base, kx, ky } => {
                let (kx, ky) = (kx.max(1), ky.max(1));
                match base.generate()? {
                    Value::Image(img) => Ok(Value::Image(Arc::new(Image {
                        px: upscale(&img.px, kx, ky)?,
                        meta: ImageMeta::default(),
                    }))),
                    Value::Frames(fr) => {
                        let frames = fr
                            .frames()
                            .iter()
                            .map(|f| {
                                Ok(crate::value::Frame {
                                    px: upscale(&f.px, kx, ky)?,
                                    offset: IPoint {
                                        x: f.offset.x.saturating_mul(i64::from(kx)),
                                        y: f.offset.y.saturating_mul(i64::from(ky)),
                                    },
                                    duration_ms: f.duration_ms,
                                })
                            })
                            .collect::<Result<Vec<_>, EngineError>>()?;
                        let canvas = fr.canvas();
                        let size = |v: u32, k: u32| {
                            v.checked_mul(k)
                                .ok_or_else(|| EngineError::Limit("大きすぎます".into()))
                        };
                        Ok(Value::Frames(Arc::new(Frames::new(
                            frames,
                            crate::geom::Size {
                                w: size(canvas.w, kx)?,
                                h: size(canvas.h, ky)?,
                            },
                            ImageMeta::default(),
                            fr.loop_count,
                        )?)))
                    }
                    Value::Audio(_) => Err(EngineError::Internal("音声は拡大できません".into())),
                }
            }
            Self::Crop { ref base, rect } => {
                let Value::Image(img) = base.generate()? else {
                    return Err(EngineError::Internal(
                        "切り抜けるのは 1 枚の画像だけです".into(),
                    ));
                };
                if !rect.inside(img.px.width(), img.px.height()) {
                    return Err(EngineError::Internal(format!(
                        "切り抜く範囲が画像の外です（{rect:?}）"
                    )));
                }
                let px = img
                    .px
                    .derive(rect.w, rect.h, &crate::monitor::NoMonitor, |t, out| {
                        img.px
                            .read_rect(IRect::new(rect.x + t.x, rect.y + t.y, t.w, t.h), out)
                    })?;
                Ok(Value::Image(Arc::new(Image {
                    px,
                    meta: ImageMeta::default(),
                })))
            }
            Self::SpriteSheet {
                cols,
                rows,
                cell_w,
                cell_h,
                gap_x,
                gap_y,
                margin_x,
                margin_y,
                pad_right,
                pad_bottom,
                background,
                full,
                ref empty,
                seed,
            } => sprite_sheet(
                (cols, rows, cell_w, cell_h),
                (gap_x, gap_y, margin_x, margin_y, pad_right, pad_bottom),
                background,
                full,
                empty,
                seed,
            ),
            Self::Sine {
                rate,
                channels,
                frames,
                freq,
                loop_range,
            } => {
                let planes = (0..channels.max(1))
                    .map(|c| {
                        let f = f64::from(freq) * math::pow(1.5, f64::from(c));
                        let step = 2.0 * std::f64::consts::PI * f / f64::from(rate);
                        Arc::new(
                            (0..frames)
                                .map(|i| (0.5 * math::sin(step * i as f64)) as f32)
                                .collect::<Vec<f32>>(),
                        )
                    })
                    .collect();
                let meta = AudioMeta {
                    loop_range,
                    ..AudioMeta::default()
                };
                Ok(Value::Audio(Arc::new(Audio::new(rate, planes, meta)?)))
            }
            Self::SineLevels {
                rate,
                freq,
                ref segments,
            } => {
                let step = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(rate);
                let total = segments.iter().map(|s| s.0).sum::<u64>();
                let mut plane = Vec::new();
                plane
                    .try_reserve_exact(usize::try_from(total).unwrap_or(usize::MAX))
                    .map_err(|_| EngineError::Limit("長すぎます".into()))?;
                let mut i = 0u64;
                for &(n, db) in segments {
                    let amp = math::db_to_gain(f64::from(db));
                    for _ in 0..n {
                        plane.push((amp * math::sin(step * i as f64)) as f32);
                        i += 1;
                    }
                }
                Ok(Value::Audio(Arc::new(Audio::new(
                    rate,
                    vec![Arc::new(plane)],
                    AudioMeta::default(),
                )?)))
            }
            Self::AudioNoise {
                rate,
                channels,
                frames,
                seed,
            } => {
                let planes = (0..channels.max(1))
                    .map(|c| {
                        let mut r = determinism::rng_stream(seed, u64::from(c));
                        // 上位 24 ビットで [-1, 1)
                        Arc::new(
                            (0..frames)
                                .map(|_| ((r.next_u32() >> 8) as f32 / 8_388_608.0) - 1.0)
                                .collect::<Vec<f32>>(),
                        )
                    })
                    .collect();
                Ok(Value::Audio(Arc::new(Audio::new(
                    rate,
                    planes,
                    AudioMeta::default(),
                )?)))
            }
            Self::Bursts {
                rate,
                channels,
                frames,
                period,
                seed,
            } => {
                let period = period.max(1);
                let tail = (u64::from(rate) * 200 / 44_100).max(1);
                let step = 2.0 * std::f64::consts::PI * 700.0 / f64::from(rate);
                let planes = (0..channels.max(1))
                    .map(|c| {
                        let mut r = determinism::rng_stream(seed, u64::from(c));
                        Arc::new(
                            (0..frames)
                                .map(|i| {
                                    let k = i % period;
                                    if k < 16 {
                                        // 上位 24 ビットで [-1, 1)
                                        0.95 * (((r.next_u32() >> 8) as f32 / 8_388_608.0) - 1.0)
                                    } else if k < 16 + tail {
                                        (0.3 * math::sin(step * i as f64)) as f32
                                    } else {
                                        0.0
                                    }
                                })
                                .collect::<Vec<f32>>(),
                        )
                    })
                    .collect();
                Ok(Value::Audio(Arc::new(Audio::new(
                    rate,
                    planes,
                    AudioMeta::default(),
                )?)))
            }
            Self::Swell {
                rate,
                channels,
                frames,
                ref freqs,
                from_db,
                to_db,
            } => {
                let two_pi = 2.0 * std::f64::consts::PI;
                let count = freqs.len().max(1) as f64;
                let len = frames.max(1) as f64;
                let (db0, db1) = (f64::from(from_db), f64::from(to_db));
                let planes = (0..channels.max(1))
                    .map(|c| {
                        Arc::new(
                            (0..frames)
                                .map(|i| {
                                    let t = i as f64 / f64::from(rate);
                                    let gain =
                                        math::db_to_gain(db0 + (db1 - db0) * (i as f64 / len));
                                    let sum: f64 = freqs
                                        .iter()
                                        .enumerate()
                                        .map(|(k, f)| {
                                            math::sin(
                                                two_pi * f64::from(*f) * t
                                                    + 0.3 * f64::from(c)
                                                    + k as f64,
                                            )
                                        })
                                        .sum();
                                    (gain * sum / count) as f32
                                })
                                .collect::<Vec<f32>>(),
                        )
                    })
                    .collect();
                Ok(Value::Audio(Arc::new(Audio::new(
                    rate,
                    planes,
                    AudioMeta::default(),
                )?)))
            }
            Self::Swatches { w, h, count, clear } => {
                let count = u64::from(count.max(1));
                let px = Raster::from_tile_fn(
                    w,
                    h,
                    PixelFormat::Rgba8,
                    None,
                    &crate::monitor::NoMonitor,
                    |t, out| {
                        for (i, chunk) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                            let x = t.x as u64 + u64::from(i as u32 % t.w);
                            let y = t.y as u64 + u64::from(i as u32 / t.w);
                            let k = (y * u64::from(w) + x) % count;
                            let a = if k < u64::from(clear) { 0 } else { 255 };
                            chunk.copy_from_slice(&[k as u8, (k >> 8) as u8, (k >> 16) as u8, a]);
                        }
                        Ok(())
                    },
                )?;
                Ok(Value::Image(Arc::new(Image {
                    px,
                    meta: ImageMeta::default(),
                })))
            }
            Self::Pixels {
                w,
                h,
                ref colors,
                ref index,
            } => {
                if u64::from(w) * u64::from(h) != index.len() as u64 {
                    return Err(EngineError::Internal(
                        "画素の番号の数が大きさと合いません".into(),
                    ));
                }
                let mut data = Vec::new();
                data.try_reserve_exact(index.len().saturating_mul(4))
                    .map_err(|_| EngineError::Limit("大きすぎます".into()))?;
                for i in index {
                    let c = colors.get(usize::from(*i)).ok_or_else(|| {
                        EngineError::Internal(format!("色の番号 {i} が範囲の外です"))
                    })?;
                    data.extend_from_slice(c);
                }
                Ok(Value::Image(Arc::new(Image {
                    px: Raster::from_rows(w, h, PixelFormat::Rgba8, None, &data)?,
                    meta: ImageMeta::default(),
                })))
            }
            Self::NonFinite { ref base, ref at } => {
                let Value::Audio(a) = base.generate()? else {
                    return Err(EngineError::Internal(
                        "NaN・∞ を置けるのは音声だけです".into(),
                    ));
                };
                let mut planes = a.planes().to_vec();
                for &(ch, frame, v) in at {
                    let slot = planes
                        .get_mut(usize::from(ch))
                        .map(Arc::make_mut)
                        .and_then(|p| usize::try_from(frame).ok().and_then(|f| p.get_mut(f)))
                        .ok_or_else(|| {
                            EngineError::Internal(format!(
                                "NaN・∞ を置く位置（チャンネル {ch}・フレーム {frame}）が範囲の外です"
                            ))
                        })?;
                    *slot = v.value();
                }
                Ok(Value::Audio(Arc::new(Audio::new(
                    a.rate(),
                    planes,
                    a.meta.clone(),
                )?)))
            }
            Self::Photo { w, h, seed } => Ok(Value::Image(Arc::new(Image {
                px: photo(w, h, seed)?,
                meta: ImageMeta::default(),
            }))),
            Self::PixelSprite { w, h, fmt, seed } => Ok(Value::Image(Arc::new(Image {
                px: pixel_sprite(w, h, fmt, seed)?,
                meta: ImageMeta::default(),
            }))),
            // 上で扱った
            Self::WithMeta { .. } => Err(EngineError::Internal("WithMeta は上で扱う".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_repeatable_and_thread_independent() {
        let gens = [
            InputGen::Noise {
                w: 600,
                h: 300,
                fmt: PixelFormat::Rgba8,
                seed: 1,
            },
            InputGen::Noise {
                w: 40,
                h: 30,
                fmt: PixelFormat::Indexed8,
                seed: 2,
            },
            InputGen::Checker {
                w: 300,
                h: 300,
                fmt: PixelFormat::Rgb16,
                cell: 8,
            },
            InputGen::Sheet {
                cols: 4,
                rows: 2,
                cell: 16,
            },
            InputGen::AudioNoise {
                rate: 48_000,
                channels: 2,
                frames: 10_000,
                seed: 3,
            },
        ];
        for g in gens {
            let mut digests = Vec::new();
            for threads in [1, 3, 16] {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                digests.push(pool.install(|| g.generate().unwrap().digest().unwrap()));
            }
            assert!(digests.windows(2).all(|w| w[0] == w[1]), "{g:?}");
        }
    }

    /// ベンチの入力（写真・ドット絵）は凍結する：値のダイジェストが決めた値（どの機械・ubuntu の CI でも同じ）で、
    /// スレッド数によらない。タイルの境（256）をまたぐ大きさで確かめる。
    #[test]
    fn bench_inputs_are_frozen() {
        let cases = [
            (
                InputGen::Photo {
                    w: 300,
                    h: 260,
                    seed: 1,
                },
                "e65c59ad9e75a6689215252c1feacb00686926d1d219598284d9c94b56e7a709",
            ),
            (
                InputGen::PixelSprite {
                    w: 270,
                    h: 33,
                    fmt: PixelFormat::Rgba8,
                    seed: 2,
                },
                "db7d147cf39d1fab7869e1705b9168627ad7c8ca18b2f2419308b85686f4705c",
            ),
            (
                InputGen::PixelSprite {
                    w: 270,
                    h: 33,
                    fmt: PixelFormat::Indexed8,
                    seed: 2,
                },
                "90d1ded7c9c2a0dd1f062dbff96213d587be468459e6ae357c96cbbb814c1c90",
            ),
        ];
        for (g, want) in cases {
            let mut digests = Vec::new();
            for threads in [1, 3] {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                digests.push(pool.install(|| g.generate().unwrap().digest().unwrap()));
            }
            assert_eq!(digests[0], digests[1], "{g:?}");
            assert_eq!(digests[0].to_hex(), want, "{g:?}");
        }
    }

    /// 写真は Rgb8 で、濃淡・円の縁・雑音がある（どの行も同じ色ではない）。ドット絵は縁が透明で、中に不透明な塊がある。
    /// Rgba8 と Indexed8 は同じ絵（Indexed8 のパレットの色を引くと Rgba8 になる）。ほかの形式は断る。
    #[test]
    fn bench_inputs_have_the_described_shape() {
        let Value::Image(p) = (InputGen::Photo {
            w: 120,
            h: 80,
            seed: 3,
        })
        .generate()
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            (p.px.width(), p.px.height(), p.px.format()),
            (120, 80, PixelFormat::Rgb8)
        );
        let rows = p.px.to_rows().unwrap();
        assert!(
            rows.as_chunks::<{ 120 * 3 }>().0.iter().all(|r| r
                .as_chunks::<3>()
                .0
                .iter()
                .any(|c| c != &r[..3]))
        );
        let sprite = |fmt| {
            let Value::Image(img) = (InputGen::PixelSprite {
                w: 64,
                h: 48,
                fmt,
                seed: 4,
            })
            .generate()
            .unwrap() else {
                panic!()
            };
            img
        };
        let (rgba, idx) = (sprite(PixelFormat::Rgba8), sprite(PixelFormat::Indexed8));
        let rgba_rows = rgba.px.to_rows().unwrap();
        let pal = idx.px.palette().unwrap().colors.clone();
        let from_idx: Vec<u8> = idx
            .px
            .to_rows()
            .unwrap()
            .iter()
            .flat_map(|i| {
                if *i == 0 {
                    [0, 0, 0, 0]
                } else {
                    pal[usize::from(*i)]
                }
            })
            .collect();
        assert_eq!(rgba_rows, from_idx);
        let alpha = |x: usize, y: usize| rgba_rows[(y * 64 + x) * 4 + 3];
        assert!((0..64).all(|x| alpha(x, 0) == 0 && alpha(x, 47) == 0));
        assert!((0..48).all(|y| alpha(0, y) == 0 && alpha(63, y) == 0));
        assert_eq!(alpha(32, 24), 255);
        assert!(
            InputGen::PixelSprite {
                w: 8,
                h: 8,
                fmt: PixelFormat::Gray8,
                seed: 0
            }
            .generate()
            .is_err()
        );
        // 余白しかない小さなスプライトも作れる
        assert!(
            InputGen::PixelSprite {
                w: 1,
                h: 1,
                fmt: PixelFormat::Rgba8,
                seed: 0
            }
            .generate()
            .is_ok()
        );
    }

    /// 衝撃の繰り返し：周期ごとに 16 フレームの雑音（振幅 0.95 まで）、続く 700 Hz の音（振幅 0.3 まで）、残りは無音。
    /// チャンネルごとに雑音が違い、同じ記述なら同じ値。JSON の形は `{"type":"bursts",…}`。
    #[test]
    fn bursts_have_the_described_shape() {
        let g = InputGen::Bursts {
            rate: 44_100,
            channels: 2,
            frames: 3_000,
            period: 1_000,
            seed: 7,
        };
        let v = g.generate().unwrap();
        assert_eq!(v, g.generate().unwrap());
        let Value::Audio(a) = &v else {
            panic!("音声ではありません");
        };
        assert_eq!((a.rate(), a.channels(), a.frames()), (44_100, 2, 3_000));
        let tail = 200;
        for p in a.planes() {
            for (i, s) in p.iter().enumerate() {
                let k = i % 1_000;
                if k < 16 {
                    assert!(s.abs() <= 0.95, "{i}: {s}");
                } else if k < 16 + tail {
                    assert!(s.abs() <= 0.3, "{i}: {s}");
                } else {
                    assert_eq!(*s, 0.0, "{i}");
                }
            }
            assert!(p[..16].iter().any(|s| s.abs() > 0.1));
            assert!(p[1_000..1_016].iter().any(|s| s.abs() > 0.1));
        }
        assert_ne!(a.planes()[0][..16], a.planes()[1][..16]);
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::json!({
                "type": "bursts",
                "rate": 44_100,
                "channels": 2,
                "frames": 3_000,
                "period": 1_000,
                "seed": 7
            })
        );
    }

    /// 色の見本：画素の番号を `count` で割った余りの色。`clear` より小さい番号は透明（RGB は違う）。タイルをまたいでも
    /// 番号は行の順。JSON の形は `{"type":"swatches",…}`。
    #[test]
    fn swatches_number_the_pixels() {
        let g = InputGen::Swatches {
            w: 300,
            h: 2,
            count: 257,
            clear: 3,
        };
        let Value::Image(img) = g.generate().unwrap() else {
            panic!("画像ではありません");
        };
        let rows = img.px.to_rows().unwrap();
        let px = |i: usize| <[u8; 4]>::try_from(&rows[i * 4..i * 4 + 4]).unwrap();
        assert_eq!(px(0), [0, 0, 0, 0]);
        assert_eq!(px(2), [2, 0, 0, 0]);
        assert_eq!(px(3), [3, 0, 0, 255]);
        assert_eq!(px(256), [0, 1, 0, 255]);
        assert_eq!(px(257), [0, 0, 0, 0]);
        // 2 行目の左端（タイルの外の列から続く番号）：300 mod 257 = 43
        assert_eq!(px(300), [43, 0, 0, 255]);
        let mut all: Vec<[u8; 4]> = (0..600).map(px).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 257);
        assert_eq!(all.iter().filter(|c| c[3] == 0).count(), 3);
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::json!({"type": "swatches", "w": 300, "h": 2, "count": 257, "clear": 3})
        );
    }

    /// 画素を書いた画像：番号の色が行の順に並ぶ。数が合わない・番号が範囲の外は誤り。
    #[test]
    fn pixels_are_written_as_given() {
        let g = InputGen::Pixels {
            w: 2,
            h: 2,
            colors: vec![[1, 2, 3, 0], [9, 9, 9, 255]],
            index: vec![1, 0, 0, 1],
        };
        let Value::Image(img) = g.generate().unwrap() else {
            panic!("画像ではありません");
        };
        assert_eq!(
            img.px.to_rows().unwrap(),
            [9, 9, 9, 255, 1, 2, 3, 0, 1, 2, 3, 0, 9, 9, 9, 255]
        );
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::json!({
                "type": "pixels",
                "w": 2,
                "h": 2,
                "colors": [[1, 2, 3, 0], [9, 9, 9, 255]],
                "index": [1, 0, 0, 1]
            })
        );
        for (w, index) in [(3, vec![0, 0, 0, 0]), (2, vec![0, 2, 0, 0])] {
            let bad = InputGen::Pixels {
                w,
                h: 2,
                colors: vec![[0; 4], [1; 4]],
                index,
            };
            assert!(bad.generate().is_err());
        }
    }

    /// NaN・±∞ を置く：指定の位置だけが変わり（後のものが勝つ）、ほかの標本・レート・メタデータは元のまま。同じ記述なら
    /// 同じ値（NaN は == で比べられないのでダイジェストで）。範囲の外・音声でない元は誤り。
    #[test]
    fn non_finite_samples_are_placed() {
        let base = InputGen::WithMeta {
            base: Box::new(InputGen::AudioNoise {
                rate: 8_000,
                channels: 2,
                frames: 100,
                seed: 4,
            }),
            meta: MetaGen {
                loop_range: Some(LoopRange { start: 1, end: 50 }),
                ..MetaGen::default()
            },
        };
        let g = InputGen::NonFinite {
            base: Box::new(base.clone()),
            at: vec![
                (0, 5, NonFiniteSample::Inf),
                (1, 99, NonFiniteSample::NegInf),
                (0, 5, NonFiniteSample::Nan),
            ],
        };
        let (Value::Audio(a), Value::Audio(b)) = (g.generate().unwrap(), base.generate().unwrap())
        else {
            panic!("音声ではありません");
        };
        assert_eq!(
            g.generate().unwrap().digest().unwrap(),
            Value::Audio(a.clone()).digest().unwrap()
        );
        assert_eq!((a.rate(), a.meta.clone()), (b.rate(), b.meta.clone()));
        assert!(a.planes()[0][5].is_nan());
        assert_eq!(a.planes()[1][99], f32::NEG_INFINITY);
        for (c, (p, q)) in a.planes().iter().zip(b.planes()).enumerate() {
            for (i, (x, y)) in p.iter().zip(q.iter()).enumerate() {
                if (c, i) != (0, 5) && (c, i) != (1, 99) {
                    assert_eq!(x.to_bits(), y.to_bits(), "{c} {i}");
                }
            }
        }
        assert_eq!(
            serde_json::to_value(&g).unwrap()["at"],
            serde_json::json!([[0, 5, "inf"], [1, 99, "negInf"], [0, 5, "nan"]])
        );
        for at in [(2, 0), (0, 100)] {
            let bad = InputGen::NonFinite {
                base: Box::new(base.clone()),
                at: vec![(at.0, at.1, NonFiniteSample::Nan)],
            };
            assert!(bad.generate().is_err());
        }
        let image = InputGen::NonFinite {
            base: Box::new(InputGen::Sheet {
                cols: 1,
                rows: 1,
                cell: 2,
            }),
            at: vec![],
        };
        assert!(image.generate().is_err());
    }

    /// だんだん大きくなる音：振幅は `from_db` から `to_db` まで増え（区間ごとの最大が増える）、最大は `to_db` を超えない。
    /// チャンネルごとに位相が違う。JSON の形は `{"type":"swell",…}`。
    #[test]
    fn swell_grows() {
        let g = InputGen::Swell {
            rate: 48_000,
            channels: 2,
            frames: 4_800,
            freqs: vec![1_000, 3_000],
            from_db: -40,
            to_db: -6,
        };
        let v = g.generate().unwrap();
        assert_eq!(v, g.generate().unwrap());
        let Value::Audio(a) = &v else {
            panic!("音声ではありません");
        };
        assert_eq!((a.rate(), a.channels(), a.frames()), (48_000, 2, 4_800));
        let limit = math::db_to_gain(-6.0) as f32;
        for p in a.planes() {
            let peaks: Vec<f32> = p
                .chunks(480)
                .map(|c| c.iter().fold(0.0_f32, |m, s| math::max32(m, s.abs())))
                .collect();
            assert!(peaks.windows(2).all(|w| w[0] < w[1]), "{peaks:?}");
            assert!(peaks[0] < math::db_to_gain(-36.0) as f32, "{peaks:?}");
            assert!(peaks.iter().all(|&m| m <= limit), "{peaks:?}");
        }
        assert_ne!(a.planes()[0], a.planes()[1]);
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::json!({
                "type": "swell",
                "rate": 48_000,
                "channels": 2,
                "frames": 4_800,
                "freqs": [1_000, 3_000],
                "fromDb": -40,
                "toDb": -6
            })
        );
    }
}
