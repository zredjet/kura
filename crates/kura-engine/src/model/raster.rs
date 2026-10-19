//! ソフトウェアの描画：正射影のカメラ・三角形の走査（左上の規則）・z バッファ・Lambert の陰影・テクスチャ（双線形か
//! 最近傍）・超標本化のアンチエイリアス・透明の背景。
//!
//! - 画像は出力の 16 行ずつの帯に分け、帯ごとに `par_blocks` で並べて描く。帯の中は三角形の番号の順に描き（不透明と
//!   MASK）、BLEND は奥から手前へ（重心の深さ、同じなら番号の順）重ねる。どの標本の値も帯の分け方・スレッドの数によらない。
//! - 色は線形で計算し、標本の平均（乗算済み）をストレートに戻してから sRGB の 8 ビットにする（表は `math::pow` で作る）。
//! - 描く手間（三角形の囲む矩形の標本の数の和）を、描く前に数えて断る（`ModelLimits::max_overdraw`）。

use std::sync::Arc;

use super::ModelLimits;
use super::scene::{AlphaMode, Material, Scene, Texture, Wrap, srgb_to_linear};
use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::math;
use crate::monitor::Monitor;
use crate::value::{Image, ImageMeta, PixelFormat, Raster, Value};

/// 帯の行の数（出力の画素で）。
const BAND_ROWS: u32 = 16;
/// 環境の光と、向きのある光の強さ。
const AMBIENT: f32 = 0.35;
const DIFFUSE: f32 = 0.65;
/// 3/4 の眺め：右へ 30 度、上へ 20 度から見る。
const AZIMUTH_DEG: f64 = 30.0;
const ELEVATION_DEG: f64 = 20.0;
/// 光の向き（見る座標：x 右・y 上・z 手前。左上の手前から）。正規化は描くときに行う。
const LIGHT: [f64; 3] = [-0.35, 0.55, 0.75];
/// 線形から sRGB への表の段の数。
const SRGB_STEPS: usize = 4096;

/// 見る向き。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewDir {
    /// 右前の上から見る（サムネイル）。
    ThreeQuarter,
    /// 正面（+Z の側）から、上を +Y にして見る（テスト・正面の図）。
    Front,
}

/// 描き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// 長辺の画素数（もう一辺はモデルの縦横の比で決まる）。
    pub size: u32,
    /// 超標本化の 1 辺の数（1〜4。1 画素に samples² の標本）。
    pub samples: u32,
    pub view: ViewDir,
}

/// 描くのに使うメモリの見積もり（`shared` は描く全体で 1 つ、`per_task` は並べて描く帯ごと）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderEstimate {
    pub shared: u64,
    pub per_task: u64,
}

impl Scene {
    /// `render` が使うメモリの見積もり（上の側に。出力は長辺 × 長辺として数える）。
    pub fn estimate_render(&self, opts: &RenderOptions) -> RenderEstimate {
        let size = u64::from(opts.size);
        let k = u64::from(opts.samples.clamp(1, 4));
        let out = size * size * 4;
        let v = self.positions.len() as u64;
        let t = self.triangles.len() as u64;
        RenderEstimate {
            // 出力の行と画像・頂点の画面の座標と見る座標の法線（12 ＋ 12）・三角形の面の法線（12）と帯の範囲（8）と帯への
            // 振り分け（三角形あたり 2 つの帯として 8）
            shared: out * 2 + v * 24 + t * 28,
            // 帯の標本（色 16 バイト・深さ 4 バイト）と、帯の出力の行
            per_task: size * k * u64::from(BAND_ROWS) * k * 20 + size * u64::from(BAND_ROWS) * 4,
        }
    }
}

fn normalize3(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0 && len.is_finite() {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// 見る座標の基底（x 右・y 上・z 手前）。
fn basis(view: ViewDir) -> [[f64; 3]; 3] {
    match view {
        ViewDir::Front => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        ViewDir::ThreeQuarter => {
            let (az, el) = (
                AZIMUTH_DEG * std::f64::consts::PI / 180.0,
                ELEVATION_DEG * std::f64::consts::PI / 180.0,
            );
            let z = [
                math::sin(az) * math::cos(el),
                math::sin(el),
                math::cos(az) * math::cos(el),
            ];
            let x = normalize3([z[2], 0.0, -z[0]]);
            let y = cross(z, x);
            [x, y, z]
        }
    }
}

/// 標本の中心が三角形の中か（左上の規則）。辺の値が 0 のときは、上か左の辺だけが含む。
#[derive(Clone, Copy)]
struct Edge {
    a: f64,
    b: f64,
    c: f64,
    top_left: bool,
}

impl Edge {
    fn new(p0: [f64; 2], p1: [f64; 2]) -> Self {
        let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
        Self {
            a: -dy,
            b: dx,
            c: dy * p0[0] - dx * p0[1],
            top_left: (dy == 0.0 && dx > 0.0) || dy < 0.0,
        }
    }

    fn at(&self, x: f64, y: f64) -> f64 {
        self.a * x + self.b * y + self.c
    }

    fn inside(&self, w: f64) -> bool {
        w > 0.0 || (w == 0.0 && self.top_left)
    }
}

/// 描く準備：標本の座標の頂点・見る座標の法線・表。
struct Setup<'a> {
    scene: &'a Scene,
    /// 頂点の標本の座標（x・y）と深さ（大きいほど手前）。
    screen: Vec<[f32; 3]>,
    /// 頂点の見る座標の法線（法線のない場面では空）。
    normals: Vec<[f32; 3]>,
    /// 光の向き（見る座標）。
    light: [f32; 3],
    to_linear: [f32; 256],
    to_srgb: Vec<u8>,
    /// 標本の画像の幅と高さ。
    sw: u32,
    sh: u32,
    k: u32,
    /// 帯ごとの三角形の番号（番号の順）。
    bands: Vec<Vec<u32>>,
    /// 三角形ごとの見る座標の面の法線（描かないものは 0）。
    faces: Vec<[f32; 3]>,
}

/// 描く。値は Rgba8（ストレートのアルファ）の画像。描ける三角形がなければ `Unsupported`、手間・大きさが上限を超えれば
/// `Limit`。
pub fn render(
    scene: &Scene,
    opts: &RenderOptions,
    limits: &ModelLimits,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    if opts.size == 0 || opts.size > limits.max_output_dim {
        return Err(EngineError::Limit(format!(
            "描く大きさ {} は 1〜{} の外です",
            opts.size, limits.max_output_dim
        )));
    }
    if !(1..=4).contains(&opts.samples) {
        return Err(EngineError::Limit(format!(
            "超標本化の数 {} は 1〜4 の外です",
            opts.samples
        )));
    }
    let [bx, by, bz] = basis(opts.view);
    let view_of = |p: &[f32; 3]| {
        let p = [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])];
        [dot(p, bx), dot(p, by), dot(p, bz)]
    };
    // 範囲（描く三角形の、有限の頂点だけ）
    let finite = |v: [f64; 3]| v.iter().all(|c| c.is_finite());
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    let mut any = false;
    for t in &scene.triangles {
        let vs = t.v.map(|i| view_of(&scene.positions[i as usize]));
        if !vs.iter().all(|v| finite(*v)) {
            continue;
        }
        any = true;
        for v in vs {
            for k in 0..2 {
                lo[k] = math::min(lo[k], v[k]);
                hi[k] = math::max(hi[k], v[k]);
            }
        }
    }
    if !any {
        return Err(EngineError::Unsupported("描ける三角形がありません".into()));
    }
    let ext = [hi[0] - lo[0], hi[1] - lo[1]];
    let long = math::max(ext[0], ext[1]);
    let long = if long > 0.0 && long.is_finite() {
        long
    } else {
        1.0
    };
    let size = opts.size;
    let margin = if size >= 32 { size / 32 } else { 0 };
    let inner = f64::from(size - 2 * margin);
    let scale = inner / long;
    let dim = |e: f64| {
        let px = (e * scale).round();
        let px = if px.is_finite() { px as u32 } else { 0 };
        px.saturating_add(2 * margin).clamp(1, size)
    };
    let (w, h) = (dim(ext[0]), dim(ext[1]));
    let k = opts.samples;
    let (sw, sh) = (w * k, h * k);
    // 真ん中に置く
    let off = [
        (f64::from(w) - ext[0] * scale) / 2.0,
        (f64::from(h) - ext[1] * scale) / 2.0,
    ];
    let kf = f64::from(k);
    let mut screen = crate::codec::try_vec(scene.positions.len(), [f32::NAN; 3])?;
    for (s, p) in screen.iter_mut().zip(&scene.positions) {
        let v = view_of(p);
        *s = [
            (((v[0] - lo[0]) * scale + off[0]) * kf) as f32,
            (((hi[1] - v[1]) * scale + off[1]) * kf) as f32,
            v[2] as f32,
        ];
    }
    let mut normals = Vec::new();
    if !scene.normals.is_empty() {
        normals = crate::codec::try_vec(scene.normals.len(), [0.0f32; 3])?;
        for (o, n) in normals.iter_mut().zip(&scene.normals) {
            let v = view_of(n);
            *o = [v[0] as f32, v[1] as f32, v[2] as f32];
        }
    }
    // 三角形の面の法線と、帯への振り分け（裏向きの片面は描かない）
    let band_rows = BAND_ROWS * k;
    let n_bands = h.div_ceil(BAND_ROWS) as usize;
    let mut counts = crate::codec::try_vec(n_bands, 0usize)?;
    let mut faces = crate::codec::try_vec(scene.triangles.len(), [0.0f32; 3])?;
    let budget = limits
        .max_overdraw
        .saturating_mul(u64::from(sw))
        .saturating_mul(u64::from(sh));
    let mut work = 0u64;
    let mut ranges = crate::codec::try_vec(scene.triangles.len(), (u32::MAX, 0u32))?;
    for (i, t) in scene.triangles.iter().enumerate() {
        let s = t.v.map(|v| screen[v as usize]);
        if !s.iter().flatten().all(|c| c.is_finite()) {
            continue;
        }
        let p = s.map(|q| [f64::from(q[0]), f64::from(q[1])]);
        let area = Edge::new(p[0], p[1]).at(p[2][0], p[2][1]);
        if area == 0.0 || !area.is_finite() {
            continue;
        }
        let part = scene.parts[t.part as usize];
        let mat = scene.materials[part.material as usize];
        // 見る座標で反時計回り（表）なら、y が下向きの標本の座標では面積が負
        let front = (area < 0.0) != part.flip;
        if !front && !mat.double_sided {
            continue;
        }
        let vs = t.v.map(|v| view_of(&scene.positions[v as usize]));
        let mut n = normalize3(cross(
            [
                vs[1][0] - vs[0][0],
                vs[1][1] - vs[0][1],
                vs[1][2] - vs[0][2],
            ],
            [
                vs[2][0] - vs[0][0],
                vs[2][1] - vs[0][1],
                vs[2][2] - vs[0][2],
            ],
        ));
        if part.flip {
            n = [-n[0], -n[1], -n[2]];
        }
        faces[i] = [n[0] as f32, n[1] as f32, n[2] as f32];
        // 標本の中心 (i + 0.5) が入りうる範囲
        let (mut x0, mut x1, mut y0, mut y1) = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for q in &p {
            x0 = math::min(x0, q[0]);
            x1 = math::max(x1, q[0]);
            y0 = math::min(y0, q[1]);
            y1 = math::max(y1, q[1]);
        }
        let first = |v: f64| math::max((v - 0.5).ceil(), 0.0);
        let last = |v: f64, n: u32| math::min((v - 0.5).floor(), f64::from(n) - 1.0);
        let (sx0, sx1) = (first(x0), last(x1, sw));
        let (sy0, sy1) = (first(y0), last(y1, sh));
        if sx1 < sx0 || sy1 < sy0 {
            continue;
        }
        work = work.saturating_add(((sx1 - sx0 + 1.0) * (sy1 - sy0 + 1.0)) as u64);
        if work > budget {
            return Err(EngineError::Limit(
                "3D モデルを描く手間が大きすぎます".into(),
            ));
        }
        let (b0, b1) = ((sy0 as u32) / band_rows, (sy1 as u32) / band_rows);
        ranges[i] = (b0, b1);
        for b in b0..=b1 {
            counts[b as usize] += 1;
        }
    }
    let mut bands: Vec<Vec<u32>> = Vec::new();
    bands
        .try_reserve_exact(n_bands)
        .map_err(|_| EngineError::Limit("帯を確保できません".into()))?;
    for c in &counts {
        let mut v = Vec::new();
        v.try_reserve_exact(*c)
            .map_err(|_| EngineError::Limit("帯の三角形を確保できません".into()))?;
        bands.push(v);
    }
    for (i, (b0, b1)) in ranges.iter().enumerate() {
        if *b0 == u32::MAX {
            continue;
        }
        for b in *b0..=*b1 {
            bands[b as usize].push(i as u32);
        }
    }
    drop(ranges);
    let l = normalize3(LIGHT);
    let mut to_linear = [0.0f32; 256];
    for (i, v) in to_linear.iter_mut().enumerate() {
        *v = srgb_to_linear(i as f64 / 255.0) as f32;
    }
    let mut to_srgb = crate::codec::try_vec(SRGB_STEPS, 0u8)?;
    for (i, v) in to_srgb.iter_mut().enumerate() {
        let c = i as f64 / (SRGB_STEPS - 1) as f64;
        let s = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * math::pow(c, 1.0 / 2.4) - 0.055
        };
        *v = (s * 255.0 + 0.5) as u8;
    }
    let setup = Setup {
        scene,
        screen,
        normals,
        light: [l[0] as f32, l[1] as f32, l[2] as f32],
        to_linear,
        to_srgb,
        sw,
        sh,
        k,
        bands,
        faces,
    };
    let rows = par_blocks(n_bands, mon, |b| setup.band(b as u32, w, h))?;
    let mut data = Vec::new();
    data.try_reserve_exact(w as usize * h as usize * 4)
        .map_err(|_| EngineError::Limit("画像を確保できません".into()))?;
    for r in rows {
        data.extend_from_slice(&r);
    }
    let px = Raster::from_rows(w, h, PixelFormat::Rgba8, None, &data)?;
    Ok(Value::Image(Arc::new(Image {
        px,
        meta: ImageMeta::default(),
    })))
}

impl Setup<'_> {
    /// 帯 `b`（出力の行 b·16 から）を描いて、出力の行（RGBA8）を返す。
    fn band(&self, b: u32, w: u32, h: u32) -> Result<Vec<u8>, EngineError> {
        let k = self.k;
        let r0 = b * BAND_ROWS;
        let r1 = (r0 + BAND_ROWS).min(h);
        let (s0, s1) = (r0 * k, (r1 * k).min(self.sh));
        let rows = (s1 - s0) as usize;
        let sw = self.sw as usize;
        let mut color = crate::codec::try_vec(sw * rows, [0.0f32; 4])?;
        let mut depth = crate::codec::try_vec(sw * rows, f32::NEG_INFINITY)?;
        let list = &self.bands[b as usize];
        let mut blend = Vec::new();
        for &t in list {
            let tri = self.scene.triangles[t as usize];
            let mat = &self.scene.materials[self.scene.parts[tri.part as usize].material as usize];
            if mat.alpha == AlphaMode::Blend {
                blend.push(t);
            } else {
                self.raster(t, s0, s1, &mut color, &mut depth, false);
            }
        }
        // 奥から手前へ（重心の深さ、同じなら番号の順）
        let key = |t: u32| {
            let v = self.scene.triangles[t as usize].v;
            (self.screen[v[0] as usize][2]
                + self.screen[v[1] as usize][2]
                + self.screen[v[2] as usize][2])
                / 3.0
        };
        blend.sort_by(|a, b| key(*a).total_cmp(&key(*b)).then(a.cmp(b)));
        for t in blend {
            self.raster(t, s0, s1, &mut color, &mut depth, true);
        }
        // 縮める（乗算済みの平均 → ストレート → sRGB）
        let mut out = crate::codec::try_vec(w as usize * (r1 - r0) as usize * 4, 0u8)?;
        let n = (k * k) as f32;
        let ku = k as usize;
        for y in 0..(r1 - r0) as usize {
            for x in 0..w as usize {
                let mut sum = [0.0f32; 4];
                for sy in 0..ku {
                    for sx in 0..ku {
                        let c = color[(y * ku + sy) * sw + x * ku + sx];
                        for (s, v) in sum.iter_mut().zip(c) {
                            *s += v;
                        }
                    }
                }
                let o = (y * w as usize + x) * 4;
                let a = sum[3] / n;
                if a > 0.0 {
                    for c in 0..3 {
                        out[o + c] = self.srgb(sum[c] / sum[3]);
                    }
                    out[o + 3] = (math::min32(a, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
        Ok(out)
    }

    fn srgb(&self, c: f32) -> u8 {
        let c = if c.is_nan() {
            0.0
        } else {
            math::min32(math::max32(c, 0.0), 1.0)
        };
        let i = (c * (SRGB_STEPS - 1) as f32 + 0.5) as usize;
        self.to_srgb.get(i).copied().unwrap_or(255)
    }

    /// 三角形 `t` を、標本の行 [s0, s1) に描く（`blend` なら重ね、深さは書かない）。
    fn raster(
        &self,
        t: u32,
        s0: u32,
        s1: u32,
        color: &mut [[f32; 4]],
        depth: &mut [f32],
        blend: bool,
    ) {
        let tri = self.scene.triangles[t as usize];
        let part = self.scene.parts[tri.part as usize];
        let mat = &self.scene.materials[part.material as usize];
        let mut idx = tri.v;
        let mut p = idx.map(|i| {
            let s = self.screen[i as usize];
            [f64::from(s[0]), f64::from(s[1]), f64::from(s[2])]
        });
        let mut area = Edge::new([p[0][0], p[0][1]], [p[1][0], p[1][1]]).at(p[2][0], p[2][1]);
        if area < 0.0 {
            p.swap(1, 2);
            idx.swap(1, 2);
            area = -area;
        }
        if area == 0.0 || !area.is_finite() {
            return;
        }
        let xy = |q: [f64; 3]| [q[0], q[1]];
        let e0 = Edge::new(xy(p[1]), xy(p[2]));
        let e1 = Edge::new(xy(p[2]), xy(p[0]));
        let e2 = Edge::new(xy(p[0]), xy(p[1]));
        let (mut x0, mut x1, mut y0, mut y1) = (p[0][0], p[0][0], p[0][1], p[0][1]);
        for q in &p[1..] {
            x0 = math::min(x0, q[0]);
            x1 = math::max(x1, q[0]);
            y0 = math::min(y0, q[1]);
            y1 = math::max(y1, q[1]);
        }
        let sx0 = math::max((x0 - 0.5).ceil(), 0.0);
        let sx1 = math::min((x1 - 0.5).floor(), f64::from(self.sw) - 1.0);
        let sy0 = math::max((y0 - 0.5).ceil(), f64::from(s0));
        let sy1 = math::min((y1 - 0.5).floor(), f64::from(s1) - 1.0);
        if sx1 < sx0 || sy1 < sy0 {
            return;
        }
        let face = self.faces[t as usize];
        let sw = self.sw as usize;
        for sy in sy0 as u32..=sy1 as u32 {
            let py = f64::from(sy) + 0.5;
            let row = (sy - s0) as usize * sw;
            for sx in sx0 as u32..=sx1 as u32 {
                let px = f64::from(sx) + 0.5;
                let (w0, w1, w2) = (e0.at(px, py), e1.at(px, py), e2.at(px, py));
                if !(e0.inside(w0) && e1.inside(w1) && e2.inside(w2)) {
                    continue;
                }
                let l = [(w0 / area) as f32, (w1 / area) as f32, (w2 / area) as f32];
                let z = l[0] * p[0][2] as f32 + l[1] * p[1][2] as f32 + l[2] * p[2][2] as f32;
                let at = row + sx as usize;
                // 手前のものだけ描く（NaN の深さは描かない）
                if z.partial_cmp(&depth[at]) != Some(std::cmp::Ordering::Greater) {
                    continue;
                }
                let Some(c) = self.shade(&idx, l, part.normals, part.colors, mat, face) else {
                    continue;
                };
                if blend {
                    let a = c[3];
                    let dst = color[at];
                    color[at] = [
                        c[0] * a + dst[0] * (1.0 - a),
                        c[1] * a + dst[1] * (1.0 - a),
                        c[2] * a + dst[2] * (1.0 - a),
                        a + dst[3] * (1.0 - a),
                    ];
                } else {
                    color[at] = [c[0], c[1], c[2], 1.0];
                    depth[at] = z;
                }
            }
        }
    }

    /// 標本の色（線形のストレートの RGBA）。MASK で捨てる標本は None。不透明は α を 1 にする。
    fn shade(
        &self,
        idx: &[u32; 3],
        l: [f32; 3],
        has_normals: bool,
        has_colors: bool,
        mat: &Material,
        face: [f32; 3],
    ) -> Option<[f32; 4]> {
        let interp3 = |v: &[[f32; 3]]| {
            let (a, b, c) = (v[idx[0] as usize], v[idx[1] as usize], v[idx[2] as usize]);
            [
                l[0] * a[0] + l[1] * b[0] + l[2] * c[0],
                l[0] * a[1] + l[1] * b[1] + l[2] * c[1],
                l[0] * a[2] + l[1] * b[2] + l[2] * c[2],
            ]
        };
        let mut rgba = mat.base;
        if has_colors && !self.scene.colors.is_empty() {
            let c = &self.scene.colors;
            let (a, b, d) = (c[idx[0] as usize], c[idx[1] as usize], c[idx[2] as usize]);
            for (k, v) in rgba.iter_mut().enumerate() {
                *v *= l[0] * a[k] + l[1] * b[k] + l[2] * d[k];
            }
        }
        if let (Some(tex), false) = (mat.texture, self.scene.uvs.is_empty()) {
            let u = &self.scene.uvs;
            let (a, b, d) = (u[idx[0] as usize], u[idx[1] as usize], u[idx[2] as usize]);
            let mut uv = [
                l[0] * a[0] + l[1] * b[0] + l[2] * d[0],
                l[0] * a[1] + l[1] * b[1] + l[2] * d[1],
            ];
            if let Some(m) = tex.transform {
                uv = [
                    m[0] * uv[0] + m[2] * uv[1] + m[4],
                    m[1] * uv[0] + m[3] * uv[1] + m[5],
                ];
            }
            if let Some(texture) = self.scene.textures.get(tex.texture as usize) {
                let s = self.sample(texture, uv, tex.nearest, tex.wrap);
                for (v, t) in rgba.iter_mut().zip(s) {
                    *v *= t;
                }
            }
        }
        match mat.alpha {
            AlphaMode::Opaque => rgba[3] = 1.0,
            AlphaMode::Mask(cut) => {
                if rgba[3] < cut {
                    return None;
                }
                rgba[3] = 1.0;
            }
            AlphaMode::Blend => rgba[3] = math::min32(math::max32(rgba[3], 0.0), 1.0),
        }
        if !mat.unlit {
            let mut n = face;
            if has_normals && !self.normals.is_empty() {
                let v = interp3(&self.normals);
                let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                if len > 1e-6 && len.is_finite() {
                    n = [v[0] / len, v[1] / len, v[2] / len];
                }
            }
            // 裏から見える面は、見る側へ向ける
            if n[2] < 0.0 {
                n = [-n[0], -n[1], -n[2]];
            }
            let d = n[0] * self.light[0] + n[1] * self.light[1] + n[2] * self.light[2];
            let i = AMBIENT + DIFFUSE * math::max32(d, 0.0);
            for v in rgba.iter_mut().take(3) {
                *v *= i;
            }
        }
        for (v, e) in rgba.iter_mut().zip(mat.emissive) {
            *v += e;
        }
        for v in rgba.iter_mut().take(3) {
            *v = if v.is_nan() {
                0.0
            } else {
                math::min32(math::max32(*v, 0.0), 1.0)
            };
        }
        Some(rgba)
    }

    /// テクスチャの色（線形のストレートの RGBA）。双線形は乗算済みで混ぜる。
    fn sample(&self, t: &Texture, uv: [f32; 2], nearest: bool, wrap: [Wrap; 2]) -> [f32; 4] {
        let fix = |i: i64, n: u32, w: Wrap| -> usize {
            let n = i64::from(n.max(1));
            (match w {
                Wrap::Repeat => i.rem_euclid(n),
                Wrap::Clamp => i.clamp(0, n - 1),
                Wrap::Mirror => {
                    let m = i.rem_euclid(2 * n);
                    if m >= n { 2 * n - 1 - m } else { m }
                }
            }) as usize
        };
        let texel = |x: usize, y: usize| -> [f32; 4] {
            let i = (y * t.w as usize + x) * 4;
            match t.rgba.get(i..i + 4) {
                Some(p) => {
                    let a = f32::from(p[3]) / 255.0;
                    [
                        self.to_linear[p[0] as usize] * a,
                        self.to_linear[p[1] as usize] * a,
                        self.to_linear[p[2] as usize] * a,
                        a,
                    ]
                }
                None => [0.0; 4],
            }
        };
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let (u, v) = (finite(uv[0]), finite(uv[1]));
        let (fw, fh) = (t.w as f32, t.h as f32);
        let pre = if nearest {
            let (x, y) = ((u * fw).floor() as i64, (v * fh).floor() as i64);
            texel(fix(x, t.w, wrap[0]), fix(y, t.h, wrap[1]))
        } else {
            let (x, y) = (u * fw - 0.5, v * fh - 0.5);
            let (xf, yf) = (x.floor(), y.floor());
            let (fx, fy) = (x - xf, y - yf);
            let (xi, yi) = (xf as i64, yf as i64);
            let (xa, xb) = (
                fix(xi, t.w, wrap[0]),
                fix(xi.saturating_add(1), t.w, wrap[0]),
            );
            let (ya, yb) = (
                fix(yi, t.h, wrap[1]),
                fix(yi.saturating_add(1), t.h, wrap[1]),
            );
            let (c00, c10, c01, c11) = (texel(xa, ya), texel(xb, ya), texel(xa, yb), texel(xb, yb));
            let mut out = [0.0f32; 4];
            for k in 0..4 {
                let top = c00[k] * (1.0 - fx) + c10[k] * fx;
                let bottom = c01[k] * (1.0 - fx) + c11[k] * fx;
                out[k] = top * (1.0 - fy) + bottom * fy;
            }
            out
        };
        if pre[3] > 0.0 {
            [pre[0] / pre[3], pre[1] / pre[3], pre[2] / pre[3], pre[3]]
        } else {
            [0.0; 4]
        }
    }
}
