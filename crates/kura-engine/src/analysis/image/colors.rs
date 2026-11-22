//! `image.colors` v1：色の数（257 で打ち切り）と、256 色以下ならその色の列（パレットの候補）。
//!
//! 定義（凍結する）：
//! - 数える色は **8 ビットの色**（`imaging::convert::for_each_rgba8_key`。`image.convert` が Indexed8 へ減色するときと
//!   同じ色：16 ビットは各チャンネルを四捨五入、グレーは [y, y, y, a]、アルファのない形式のアルファは 255、Indexed8 は
//!   使われている番号のパレットの色）。フレームの列は全フレームの画素の和集合（`image.convert` と同じく、フレームに
//!   覆われないキャンバスの部分は数えない）。
//! - `count`：違う色の数。アルファ 0 の色も RGB が違えば別の色。257 以上は 257（「257 色以上」）。
//! - `colors`：`count` が 256 以下のとき、色を `quantize::pack` の小さい順に並べたもの（＝ `image.convert` が作る
//!   Indexed8 のパレットそのもの。減色しない）。257 以上なら null。
//! - `visible`：アルファ 0 の色をひとつにまとめた数（257 で打ち切り。透明の下に残った色で膨らまない「見た目の色数」）。
//! - `paletteLossless`：`count` が 256 以下で、元の値が 8 ビットで正確（16 ビットは全チャンネルが 257 の倍数）。
//!   真なら Indexed8 にして元の形式に戻すと値が変わらない。
//! - `palette`：Indexed8 のとき `{len: パレットの色の数, used: 使われている番号の数}`。ほかは null。
//! - タイルごとに 257 色で打ち切る集合（並べた `Vec`）を作り、和集合（257 で頭打ち）を取る。結果は「和集合の大きさ」と
//!   「256 色以下なら並べた色の列」だけなので、タイルの順・どのタイルが先に打ち切ったかに依らない。整数だけ。

use serde::{Deserialize, Serialize};

use crate::analysis::image::{planes, shape_pixels, tile_at, tile_count};
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Estimate, NoParams};
use crate::error::EngineError;
use crate::geom::IRect;
use crate::imaging::convert::for_each_rgba8_key;
use crate::imaging::pixels::is_16;
use crate::imaging::quantize::unpack;
use crate::op::Numeric;
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Raster, Value};

/// 数えるのを打ち切る色の数（これに届いたら「257 色以上」）。凍結する。
pub const COLOR_CAP: usize = 257;

/// Indexed8 のパレットの使われ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaletteUse {
    /// パレットの色の数。
    pub len: u16,
    /// 画素に使われている番号の数（同じ色の番号が 2 つあれば `count` より多い）。
    pub used: u16,
}

/// 結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ColorsOutput {
    /// 違う色の数（1〜257。257 は 257 色以上）。
    pub count: u16,
    /// `count` が 256 以下のとき、色（ストレートの RGBA8）を pack の小さい順に。
    pub colors: Option<Vec<[u8; 4]>>,
    /// アルファ 0 の色をひとつにまとめた数（1〜257）。
    pub visible: u16,
    /// 256 色以下で、元の値が 8 ビットで正確（Indexed8 にして戻しても値が変わらない）。
    pub palette_lossless: bool,
    /// Indexed8 のときのパレットの使われ方。
    pub palette: Option<PaletteUse>,
}
crate::json_output!(ColorsOutput);

/// 257 色で打ち切る、色の集合（小さい順）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CapSet {
    keys: Vec<u32>,
    over: bool,
}

impl CapSet {
    /// 足す。257 色目で打ち切る（中身を捨てて `over`）。打ち切ったら false。
    fn insert(&mut self, k: u32) -> bool {
        if self.over {
            return false;
        }
        let i = self.keys.partition_point(|v| *v < k);
        if self.keys.get(i) != Some(&k) {
            if self.keys.len() + 1 >= COLOR_CAP {
                self.keys = Vec::new();
                self.over = true;
                return false;
            }
            self.keys.insert(i, k);
        }
        true
    }

    /// 和集合（257 で頭打ち）。交換・結合できるので、畳む順に依らない。
    fn union(self, o: &Self) -> Self {
        if self.over || o.over {
            return Self {
                keys: Vec::new(),
                over: true,
            };
        }
        let (a, b) = (&self.keys, &o.keys);
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            match (a.get(i), b.get(j)) {
                (Some(x), Some(y)) if x == y => {
                    out.push(*x);
                    i += 1;
                    j += 1;
                }
                (Some(x), Some(y)) if x < y => {
                    out.push(*x);
                    i += 1;
                }
                (_, Some(y)) => {
                    out.push(*y);
                    j += 1;
                }
                (Some(x), None) => {
                    out.push(*x);
                    i += 1;
                }
                (None, None) => break,
            }
        }
        if out.len() >= COLOR_CAP {
            return Self {
                keys: Vec::new(),
                over: true,
            };
        }
        Self {
            keys: out,
            over: false,
        }
    }
}

/// タイル（かフレーム）の数え方の途中：アルファが 0 でない色と 0 の色（交わらない）、16 ビットの値が 8 ビットで正確か。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Part {
    shown: CapSet,
    clear: CapSet,
    exact: bool,
}

impl Part {
    fn new() -> Self {
        Self {
            shown: CapSet::default(),
            clear: CapSet::default(),
            exact: true,
        }
    }

    fn union(self, o: &Self) -> Self {
        Self {
            shown: self.shown.union(&o.shown),
            clear: self.clear.union(&o.clear),
            exact: self.exact && o.exact,
        }
    }

    /// 色を 1 つ足す。打ち切って以後の画素を見なくてよくなったら（アルファが 0 でない色が 257 色に届いたら）false。
    fn insert(&mut self, k: u32) -> bool {
        if k & 0xff == 0 {
            self.clear.insert(k);
            true
        } else {
            self.shown.insert(k)
        }
    }
}

/// 16 ビットの値がすべて 257 の倍数（8 ビットの値をちょうど広げたもの）か。8 ビットの形式は常に真。
fn exact_in_8bit(fmt: PixelFormat, tile: &[u8]) -> bool {
    !is_16(fmt)
        || tile
            .as_chunks::<2>()
            .0
            .iter()
            .all(|c| u16::from_le_bytes([c[0], c[1]]) % 257 == 0)
}

/// タイルの中の色の表（開番地法の固定の表。`SLOTS` 枠・固定の乗算のハッシュ）。アルファが 0 でない色と 0 の色を
/// それぞれ 257 色で打ち切る（どちらも 256 色までしか入れないので、表は半分より埋まらない）。最後に並べ直して
/// `Part` にするので、表の作り（ハッシュ・枠の数）は結果に効かない（速さのためだけ）。
struct TileTable {
    keys: Vec<u32>,
    used: Vec<bool>,
    shown: usize,
    clear: usize,
    clear_over: bool,
}

const SLOTS: usize = 1024;

impl TileTable {
    fn new() -> Self {
        Self {
            keys: vec![0; SLOTS],
            used: vec![false; SLOTS],
            shown: 0,
            clear: 0,
            clear_over: false,
        }
    }

    /// 足す。アルファが 0 でない色が 257 色に届いたら false（そのタイルは打ち切り）。
    fn insert(&mut self, k: u32) -> bool {
        let is_clear = k & 0xff == 0;
        if is_clear && self.clear_over {
            return true;
        }
        let mut h = (k.wrapping_mul(0x9E37_79B1) >> 22) as usize & (SLOTS - 1);
        // 表は半分より埋まらないので、必ず空きか同じ色に出会う（念のため SLOTS 回で止める）
        for _ in 0..SLOTS {
            match (self.used.get(h), self.keys.get(h)) {
                (Some(false), _) => {
                    if is_clear {
                        if self.clear + 1 >= COLOR_CAP {
                            self.clear_over = true;
                            return true;
                        }
                        self.clear += 1;
                    } else {
                        if self.shown + 1 >= COLOR_CAP {
                            return false;
                        }
                        self.shown += 1;
                    }
                    if let (Some(u), Some(slot)) = (self.used.get_mut(h), self.keys.get_mut(h)) {
                        *u = true;
                        *slot = k;
                    }
                    return true;
                }
                (Some(true), Some(v)) if *v == k => return true,
                _ => h = (h + 1) & (SLOTS - 1),
            }
        }
        false
    }

    /// 並べ直して `Part` にする（`stopped` はアルファが 0 でない色で打ち切ったか）。
    fn into_part(self, stopped: bool) -> Part {
        let set = |clear: bool, over: bool| {
            if over {
                return CapSet {
                    keys: Vec::new(),
                    over: true,
                };
            }
            let mut keys: Vec<u32> = self
                .keys
                .iter()
                .zip(&self.used)
                .filter(|(k, u)| **u && ((**k & 0xff == 0) == clear))
                .map(|(k, _)| *k)
                .collect();
            keys.sort();
            CapSet { keys, over: false }
        };
        Part {
            shown: set(false, stopped),
            clear: set(true, self.clear_over),
            exact: true,
        }
    }
}

/// 1 枚の画素の色（Indexed8 以外）を数える。
fn count_raster(px: &Raster, cx: &AnalyzeCx<'_>) -> Result<Part, EngineError> {
    let fmt = px.format();
    let palette = px.palette().map(|p| p.colors.as_slice());
    let parts = cx.par_blocks(tile_count(px), |i| {
        let (_, tile) = tile_at(px, i);
        let mut table = TileTable::new();
        let mut last: Option<u32> = None;
        let mut stopped = false;
        for_each_rgba8_key(fmt, palette, tile, |k| {
            if last == Some(k) {
                return true;
            }
            last = Some(k);
            stopped = !table.insert(k);
            !stopped
        })?;
        let mut part = table.into_part(stopped);
        // 打ち切ったタイルの 16 ビットの正確さは使わない（257 色以上なら paletteLossless は偽）
        if !part.shown.over && !part.clear.over {
            part.exact = exact_in_8bit(fmt, tile);
        }
        Ok(part)
    })?;
    Ok(parts.iter().fold(Part::new(), |acc, p| acc.union(p)))
}

/// Indexed8 の使われている番号の印（256 ビット）。
fn used_indices(px: &Raster, cx: &AnalyzeCx<'_>) -> Result<[u64; 4], EngineError> {
    let parts = cx.par_blocks(tile_count(px), |i| {
        let (_, tile) = tile_at(px, i);
        let mut m = [0u64; 4];
        for b in tile {
            m[usize::from(*b >> 6)] |= 1 << (*b & 63);
        }
        Ok(m)
    })?;
    let mut m = [0u64; 4];
    for p in parts {
        for (a, b) in m.iter_mut().zip(p) {
            *a |= b;
        }
    }
    Ok(m)
}

/// 色の数とパレットの候補。
pub struct Colors;

fn small(n: usize) -> u16 {
    n.min(COLOR_CAP) as u16
}

impl Analysis for Colors {
    const ID: &'static str = "image.colors";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Image, ValueKind::Frames];
    const NUMERIC: Numeric = Numeric::Exact;
    type Params = NoParams;
    type Output = ColorsOutput;

    fn run(_p: &NoParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<ColorsOutput, EngineError> {
        cx.check()?;
        let (_, planes) = planes(v, Self::ID)?;
        let Some(first) = planes.first() else {
            return Err(EngineError::Internal(format!(
                "{} の入力に画素がありません",
                Self::ID
            )));
        };
        let mut total = Part::new();
        let mut palette_use = None;
        if first.px.format() == PixelFormat::Indexed8 {
            let pal = first
                .px
                .palette()
                .ok_or_else(|| EngineError::Internal("Indexed8 にパレットがありません".into()))?;
            let mut used = [0u64; 4];
            for plane in &planes {
                cx.check()?;
                for (a, b) in used.iter_mut().zip(used_indices(plane.px, cx)?) {
                    *a |= b;
                }
            }
            for (i, c) in pal.colors.iter().enumerate() {
                if used[i >> 6] >> (i & 63) & 1 == 1 {
                    total.insert(u32::from_be_bytes(*c));
                }
            }
            let n_used: u32 = used.iter().map(|m| m.count_ones()).sum();
            palette_use = Some(PaletteUse {
                len: pal.colors.len() as u16,
                used: n_used as u16,
            });
        } else {
            for plane in &planes {
                cx.check()?;
                total = total.union(&count_raster(plane.px, cx)?);
            }
        }
        let over = total.shown.over || total.clear.over;
        let n = total.shown.keys.len() + total.clear.keys.len();
        let count = if over { COLOR_CAP } else { n.min(COLOR_CAP) };
        let visible = if total.shown.over {
            COLOR_CAP
        } else {
            total.shown.keys.len() + usize::from(total.clear.over || !total.clear.keys.is_empty())
        };
        let colors = (count < COLOR_CAP).then(|| {
            let mut all: Vec<u32> = total
                .shown
                .keys
                .iter()
                .chain(&total.clear.keys)
                .copied()
                .collect();
            all.sort();
            all.into_iter().map(unpack).collect()
        });
        Ok(ColorsOutput {
            count: small(count),
            colors,
            visible: small(visible),
            palette_lossless: count < COLOR_CAP && total.exact,
            palette: palette_use,
        })
    }

    fn examples() -> Vec<AnalysisExample<NoParams>> {
        let ex = |name, input| AnalysisExample {
            name,
            params: NoParams {},
            input,
        };
        let swatches = |w, h, count, clear| InputGen::Swatches { w, h, count, clear };
        vec![
            ex(
                "checker-two-colors",
                InputGen::Checker {
                    w: 20,
                    h: 12,
                    fmt: PixelFormat::Rgba8,
                    cell: 3,
                },
            ),
            ex(
                "noise-over-the-cap",
                InputGen::Noise {
                    w: 64,
                    h: 64,
                    fmt: PixelFormat::Rgba8,
                    seed: 41,
                },
            ),
            ex(
                "indexed-palette-use",
                InputGen::Noise {
                    w: 20,
                    h: 20,
                    fmt: PixelFormat::Indexed8,
                    seed: 42,
                },
            ),
            ex(
                "sprite-across-tiles",
                InputGen::Sprite {
                    w: 600,
                    h: 300,
                    rect: IRect::new(250, 40, 300, 200),
                },
            ),
            ex(
                "gray16-checker-exact",
                InputGen::Checker {
                    w: 10,
                    h: 10,
                    fmt: PixelFormat::Gray16,
                    cell: 2,
                },
            ),
            ex(
                "rgba16-noise",
                InputGen::Noise {
                    w: 30,
                    h: 20,
                    fmt: PixelFormat::Rgba16,
                    seed: 43,
                },
            ),
            ex(
                "sheet-cells",
                InputGen::Sheet {
                    cols: 4,
                    rows: 2,
                    cell: 16,
                },
            ),
            ex(
                "sprite-sheet-clear-holes",
                InputGen::SpriteSheet {
                    cols: 3,
                    rows: 2,
                    cell_w: 8,
                    cell_h: 6,
                    gap_x: 1,
                    gap_y: 1,
                    margin_x: 1,
                    margin_y: 0,
                    pad_right: 0,
                    pad_bottom: 2,
                    background: None,
                    full: false,
                    empty: vec![4],
                    seed: 44,
                },
            ),
            ex(
                "trimmed-frames-indexed",
                InputGen::TrimmedFrames {
                    w: 24,
                    h: 16,
                    fmt: PixelFormat::Indexed8,
                    count: 3,
                    seed: 45,
                },
            ),
            ex(
                "upscaled-indexed",
                InputGen::Upscale {
                    base: Box::new(InputGen::Noise {
                        w: 6,
                        h: 5,
                        fmt: PixelFormat::Indexed8,
                        seed: 46,
                    }),
                    kx: 3,
                    ky: 3,
                },
            ),
            // 打ち切りの境（1 タイルに収まる）：256 色ちょうどは色の列を出し、257 色ちょうどは null。アルファが 0 でない
            // 色だけ・0 の色だけ・両方を合わせて
            ex("exactly-256-opaque", swatches(16, 16, 256, 0)),
            ex("exactly-257-opaque", swatches(16, 17, 257, 0)),
            ex("exactly-256-clear", swatches(16, 16, 256, 256)),
            ex("exactly-257-clear", swatches(16, 17, 257, 257)),
            ex("exactly-256-mixed", swatches(16, 16, 256, 128)),
            ex("exactly-257-mixed", swatches(16, 17, 257, 128)),
            // 2 タイルの和集合で 257 色に届く（どちらのタイルも 256 色以下）
            ex("exactly-257-across-tiles", swatches(300, 1, 257, 0)),
            // 透明な 8 色（RGB は違う）は visible では 1 色：count 20・visible 13
            ex("clear-colors-fold-into-one", swatches(8, 4, 20, 8)),
            // 256 色以下でも、16 ビットの値が 8 ビットで正確でなければ paletteLossless は偽
            ex(
                "rgb16-noise-not-exact",
                InputGen::Noise {
                    w: 4,
                    h: 4,
                    fmt: PixelFormat::Rgb16,
                    seed: 47,
                },
            ),
        ]
    }

    fn estimate(_p: &NoParams, input: &Shape) -> Estimate {
        let (pixels, _, _, _) = shape_pixels(input);
        Estimate {
            // 色は 1 つ "[255,255,255,255]," の 18 バイトまで × 256、ほかの項目は 256 バイトまで
            bytes: 256 * 18 + 256,
            cost: pixels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::image::test_util::run;
    use crate::value::Palette;
    use std::sync::Arc;

    fn image(w: u32, h: u32, fmt: PixelFormat, pal: Option<Palette>, data: &[u8]) -> Value {
        Value::Image(Arc::new(crate::value::Image {
            px: Raster::from_rows(w, h, fmt, pal, data).unwrap(),
            meta: Default::default(),
        }))
    }

    #[test]
    fn capped_sets_union_in_any_order() {
        let set = |ks: &[u32]| {
            let mut s = CapSet::default();
            for k in ks {
                s.insert(*k);
            }
            s
        };
        let a = set(&[5, 1, 3, 1]);
        assert_eq!(a.keys, vec![1, 3, 5]);
        let b = set(&[2, 3]);
        assert_eq!(a.clone().union(&b).keys, vec![1, 2, 3, 5]);
        assert_eq!(a.clone().union(&b), b.clone().union(&a));
        let big = set(&(0..256).collect::<Vec<u32>>());
        assert!(!big.over && big.keys.len() == 256);
        assert!(set(&(0..257).collect::<Vec<u32>>()).over);
        assert!(big.clone().union(&set(&[1000])).over);
        assert!(!big.clone().union(&set(&[3])).over);
    }

    /// 値を直接確かめる（ゴールデンに凍結する前に）。
    #[test]
    fn counts_colors_like_the_indexed_conversion() {
        // アルファ 0 の色は RGB が違えば別の色。visible はひとつにまとめる
        let v = image(
            4,
            1,
            PixelFormat::Rgba8,
            None,
            &[1, 2, 3, 0, 9, 9, 9, 0, 1, 2, 3, 0, 5, 5, 5, 255],
        );
        let out = run::<Colors>(&NoParams {}, &v);
        assert_eq!(out.count, 3);
        assert_eq!(out.visible, 2);
        assert_eq!(
            out.colors,
            Some(vec![[1, 2, 3, 0], [5, 5, 5, 255], [9, 9, 9, 0]])
        );
        assert!(out.palette_lossless);
        assert_eq!(out.palette, None);
        // 16 ビット：四捨五入した 8 ビットの色で数える。257 の倍数でない値があれば paletteLossless は偽
        let v = image(2, 1, PixelFormat::Gray16, None, &[0x01, 0x01, 0x80, 0x00]);
        let out = run::<Colors>(&NoParams {}, &v);
        assert_eq!(out.colors, Some(vec![[0, 0, 0, 255], [1, 1, 1, 255]]));
        assert!(!out.palette_lossless);
        let v = image(2, 1, PixelFormat::Gray16, None, &[0x01, 0x01, 0xff, 0xff]);
        assert!(run::<Colors>(&NoParams {}, &v).palette_lossless);
        // Indexed8：使われている番号の色だけ。同じ色の番号は 1 色
        let pal = Palette {
            colors: vec![[7, 7, 7, 255], [0, 0, 0, 0], [7, 7, 7, 255], [1, 1, 1, 1]],
        };
        let v = image(3, 1, PixelFormat::Indexed8, Some(pal), &[0, 2, 1]);
        let out = run::<Colors>(&NoParams {}, &v);
        assert_eq!(out.count, 2);
        assert_eq!(out.colors, Some(vec![[0, 0, 0, 0], [7, 7, 7, 255]]));
        assert_eq!(out.palette, Some(PaletteUse { len: 4, used: 3 }));
        // 257 色以上：打ち切り。アルファ 0 が 300 色あっても、見た目の色はアルファの色と合わせて数える
        let mut data = Vec::new();
        for i in 0..300u32 {
            data.extend_from_slice(&[i as u8, (i >> 8) as u8, 0, 0]);
        }
        data.extend_from_slice(&[1, 1, 1, 255]);
        let v = image(301, 1, PixelFormat::Rgba8, None, &data);
        let out = run::<Colors>(&NoParams {}, &v);
        assert_eq!((out.count, out.visible, out.colors), (257, 2, None));
        assert!(!out.palette_lossless);
        let mut data = Vec::new();
        for i in 0..300u32 {
            data.extend_from_slice(&[i as u8, (i >> 8) as u8, 0, 255]);
        }
        let v = image(300, 1, PixelFormat::Rgba8, None, &data);
        let out = run::<Colors>(&NoParams {}, &v);
        assert_eq!((out.count, out.visible), (257, 257));
    }

    /// 256 色ちょうどは数え、257 色ちょうどで打ち切る（アルファが 0 でない色だけ・0 の色だけ・両方を合わせて）。
    /// 1 タイルの中の表の打ち切り（`TileTable::insert`）と、タイルをまたぐ和集合（`CapSet::union`）の両方の境。
    #[test]
    fn the_cap_is_exactly_257_colors() {
        let swatches = |w, h, count, clear| {
            InputGen::Swatches { w, h, count, clear }
                .generate()
                .unwrap()
        };
        let list = |n: u32, clear: u32| -> Vec<[u8; 4]> {
            (0..n)
                .map(|k| [k as u8, (k >> 8) as u8, 0, if k < clear { 0 } else { 255 }])
                .collect()
        };
        // (w, h, count, clear) → (count, visible)。w ≤ 256 は 1 タイル、300 は 2 タイル
        for (w, h, count, clear, want, visible) in [
            (16, 16, 256, 0, 256, 256),
            (16, 17, 257, 0, 257, 257),
            (16, 16, 256, 256, 256, 1),
            (16, 17, 257, 257, 257, 1),
            (16, 16, 256, 128, 256, 129),
            (16, 17, 257, 128, 257, 130),
            (300, 1, 256, 0, 256, 256),
            (300, 1, 257, 0, 257, 257),
            (300, 1, 256, 256, 256, 1),
            (300, 1, 257, 257, 257, 1),
        ] {
            let out = run::<Colors>(&NoParams {}, &swatches(w, h, count, clear));
            let case = (w, h, count, clear);
            assert_eq!((out.count, out.visible), (want, visible), "{case:?}");
            if want == 256 {
                assert_eq!(out.colors, Some(list(256, clear)), "{case:?}");
                assert!(out.palette_lossless, "{case:?}");
            } else {
                assert_eq!(out.colors, None, "{case:?}");
                assert!(!out.palette_lossless, "{case:?}");
            }
        }
    }

    /// 例の値を直接確かめる（ゴールデンに凍結する前に）：透明な色をまとめた `visible` が `count` より少ない、
    /// 256 色以下でも 16 ビットの値が 8 ビットで正確でなければ `paletteLossless` は偽。
    #[test]
    fn visible_and_lossless_branches_of_the_examples() {
        let ex = |name: &str| {
            let e = Colors::examples()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap();
            run::<Colors>(&NoParams {}, &e.input.generate().unwrap())
        };
        let out = ex("clear-colors-fold-into-one");
        assert_eq!((out.count, out.visible), (20, 13));
        assert!(out.palette_lossless);
        let out = ex("rgb16-noise-not-exact");
        assert!(out.count <= 16 && out.colors.is_some(), "{out:?}");
        assert!(!out.palette_lossless);
        for (name, count, visible) in [
            ("exactly-256-opaque", 256, 256),
            ("exactly-257-opaque", 257, 257),
            ("exactly-256-clear", 256, 1),
            ("exactly-257-clear", 257, 1),
            ("exactly-256-mixed", 256, 129),
            ("exactly-257-mixed", 257, 130),
            ("exactly-257-across-tiles", 257, 257),
        ] {
            let out = ex(name);
            assert_eq!((out.count, out.visible), (count, visible), "{name}");
            assert_eq!(out.colors.is_some(), count == 256, "{name}");
        }
    }
}
