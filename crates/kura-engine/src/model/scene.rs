//! glb を読んで、描くための三角形の集まり（[`Scene`]）に焼き込む：ノードの変換を頂点に掛け、三角形にし、材質と埋め込みの
//! テクスチャを用意する。

use std::io::Cursor;

use super::gltf::{self, PrimitiveDef, Root};
use super::info::info_of;
use super::{ModelLimits, bad};
use crate::codec::CodecSet;
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::value::{PixelFormat, Raster, Value};

/// 三角形（頂点の番号と、どのプリミティブの実体か）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tri {
    pub v: [u32; 3],
    pub part: u32,
}

/// プリミティブの実体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Part {
    pub material: u32,
    /// 頂点の法線を持つ。
    pub normals: bool,
    /// 頂点の色を持つ。
    pub colors: bool,
    /// 変換が裏返す（行列式が負。表の向きが逆になる）。
    pub flip: bool,
}

/// テクスチャの繰り返し方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wrap {
    Repeat,
    Clamp,
    Mirror,
}

/// 材質の透明の扱い。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AlphaMode {
    Opaque,
    /// この値より小さいアルファは描かない。
    Mask(f32),
    Blend,
}

/// 基本色のテクスチャの参照。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TexRef {
    /// `Scene::textures` の番号。
    pub texture: u32,
    /// UV の変換（KHR_texture_transform。列優先の 2×3：u' = m0·u + m2·v + m4、v' = m1·u + m3·v + m5）。
    pub transform: Option<[f32; 6]>,
    pub nearest: bool,
    pub wrap: [Wrap; 2],
}

/// 材質（基本色は線形のストレートの RGBA）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Material {
    pub base: [f32; 4],
    pub texture: Option<TexRef>,
    /// 読む UV の組（TEXCOORD_n）。
    pub uv_set: u32,
    pub alpha: AlphaMode,
    pub double_sided: bool,
    pub unlit: bool,
    pub emissive: [f32; 3],
}

impl Default for Material {
    /// 材質のないプリミティブの既定（白・不透明・片面）。
    fn default() -> Self {
        Self {
            base: [1.0; 4],
            texture: None,
            uv_set: 0,
            alpha: AlphaMode::Opaque,
            double_sided: false,
            unlit: false,
            emissive: [0.0; 3],
        }
    }
}

/// テクスチャ（sRGB のストレートの RGBA8。長辺は `max_texture_dim` まで縮めたもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Texture {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// 描くための三角形の集まり（世界の座標に焼き込んだもの）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scene {
    pub(crate) positions: Vec<[f32; 3]>,
    /// 空か、頂点と同じ数（法線のないプリミティブの頂点は 0）。
    pub(crate) normals: Vec<[f32; 3]>,
    /// 空か、頂点と同じ数。
    pub(crate) uvs: Vec<[f32; 2]>,
    /// 空か、頂点と同じ数（線形の RGBA。色のないプリミティブの頂点は白）。
    pub(crate) colors: Vec<[f32; 4]>,
    pub(crate) triangles: Vec<Tri>,
    pub(crate) parts: Vec<Part>,
    pub(crate) materials: Vec<Material>,
    pub(crate) textures: Vec<Texture>,
    /// 飛ばしたもの（点・線のプリミティブ、外部の画像、読めないテクスチャなど）。
    pub warnings: Vec<String>,
}

impl Scene {
    /// 頂点の数。
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// 三角形の数。
    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    /// 持っているメモリのおおよそのバイト数。
    pub fn bytes(&self) -> u64 {
        let v = self.positions.len() as u64;
        v * 12
            + self.normals.len() as u64 * 12
            + self.uvs.len() as u64 * 8
            + self.colors.len() as u64 * 16
            + self.triangles.len() as u64 * 16
            + self
                .textures
                .iter()
                .map(|t| t.rgba.len() as u64)
                .sum::<u64>()
    }
}

/// sRGB の 8 ビットを線形に。
pub(crate) fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        crate::math::pow((c + 0.055) / 1.055, 2.4)
    }
}

fn clamp01(x: f64) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        crate::math::min(crate::math::max(x, 0.0), 1.0) as f32
    }
}

/// 材質の定義を読む（テクスチャの番号は `texture_slot` で `Scene::textures` の番号にする）。
fn material(
    def: &gltf::MaterialDef,
    root: &Root,
    texture_slot: &mut dyn FnMut(u32) -> Option<u32>,
) -> Material {
    let pbr = def.pbr_metallic_roughness.as_ref();
    let base = pbr
        .and_then(|p| p.base_color_factor)
        .unwrap_or([1.0; 4])
        .map(clamp01);
    let alpha = match def.alpha_mode.as_deref() {
        Some("MASK") => AlphaMode::Mask(clamp01(def.alpha_cutoff.unwrap_or(0.5))),
        Some("BLEND") => AlphaMode::Blend,
        _ => AlphaMode::Opaque,
    };
    let mut uv_set = 0;
    let texture = pbr
        .and_then(|p| p.base_color_texture.as_ref())
        .and_then(|ti| {
            let tex = root.textures.get(ti.index as usize)?;
            let slot = texture_slot(ti.index)?;
            let sampler = tex.sampler.and_then(|s| root.samplers.get(s as usize));
            let wrap = |w: Option<u32>| match w {
                Some(33071) => Wrap::Clamp,
                Some(33648) => Wrap::Mirror,
                _ => Wrap::Repeat,
            };
            uv_set = ti.tex_coord;
            let transform = ti.extensions.transform.as_ref().map(|t| {
                if let Some(c) = t.tex_coord {
                    uv_set = c;
                }
                let [ox, oy] = t.offset.unwrap_or([0.0, 0.0]);
                let [sx, sy] = t.scale.unwrap_or([1.0, 1.0]);
                let r = t.rotation.unwrap_or(0.0);
                let (s, c) = (crate::math::sin(r), crate::math::cos(r));
                // T · R · S（KHR_texture_transform の式）
                [
                    (c * sx) as f32,
                    (-s * sx) as f32,
                    (s * sy) as f32,
                    (c * sy) as f32,
                    ox as f32,
                    oy as f32,
                ]
            });
            Some(TexRef {
                texture: slot,
                transform,
                nearest: sampler.and_then(|s| s.mag_filter) == Some(9728),
                wrap: [
                    wrap(sampler.and_then(|s| s.wrap_s)),
                    wrap(sampler.and_then(|s| s.wrap_t)),
                ],
            })
        });
    let emissive = def.emissive_factor.unwrap_or([0.0; 3]).map(clamp01);
    Material {
        base,
        texture,
        uv_set,
        alpha,
        double_sided: def.double_sided,
        unlit: def.extensions.contains_key("KHR_materials_unlit"),
        emissive,
    }
}

/// 画像の形式を先頭のバイトで見分ける。
fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// 画素を sRGB のストレートの RGBA8 にする（16 ビットは丸めて 8 ビットに）。
fn texels(px: &Raster) -> Result<Vec<u8>, EngineError> {
    match px.format() {
        PixelFormat::Gray16 | PixelFormat::GrayA16 | PixelFormat::Rgb16 | PixelFormat::Rgba16 => {
            let rows = px.to_rows()?;
            let n = px.width() as usize * px.height() as usize;
            let mut out = crate::value::raster::alloc_zeroed(
                n.checked_mul(4)
                    .ok_or_else(|| EngineError::Limit("テクスチャが大きすぎます".into()))?,
            )?;
            let c8 = |s: &[u8]| {
                ((u32::from(u16::from_le_bytes([s[0], s[1]])) * 255 + 32_767) / 65_535) as u8
            };
            let bpp = px.format().bytes_per_pixel();
            for (s, d) in rows
                .chunks_exact(bpp)
                .zip(out.as_chunks_mut::<4>().0.iter_mut())
            {
                d.copy_from_slice(&match px.format() {
                    PixelFormat::Gray16 => [c8(&s[0..]), c8(&s[0..]), c8(&s[0..]), 255],
                    PixelFormat::GrayA16 => [c8(&s[0..]), c8(&s[0..]), c8(&s[0..]), c8(&s[2..])],
                    PixelFormat::Rgb16 => [c8(&s[0..]), c8(&s[2..]), c8(&s[4..]), 255],
                    _ => [c8(&s[0..]), c8(&s[2..]), c8(&s[4..]), c8(&s[6..])],
                });
            }
            Ok(out)
        }
        _ => crate::imaging::convert::to_rgba8_rows(px),
    }
}

/// 長辺が `max` を超えるあいだ半分に縮める（2×2 の平均。色はアルファで重みを付ける）。
fn shrink(mut t: Texture, max: u32) -> Result<Texture, EngineError> {
    while t.w.max(t.h) > max.max(1) {
        let (w2, h2) = (t.w.div_ceil(2), t.h.div_ceil(2));
        let mut out = crate::value::raster::alloc_zeroed(w2 as usize * h2 as usize * 4)?;
        for y in 0..h2 {
            for x in 0..w2 {
                let (mut sum, mut a_sum, mut n) = ([0u32; 3], 0u32, 0u32);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = (x * 2 + dx, y * 2 + dy);
                    if sx >= t.w || sy >= t.h {
                        continue;
                    }
                    let i = (sy as usize * t.w as usize + sx as usize) * 4;
                    let a = u32::from(t.rgba[i + 3]);
                    for (k, s) in sum.iter_mut().enumerate() {
                        *s += u32::from(t.rgba[i + k]) * a;
                    }
                    a_sum += a;
                    n += 1;
                }
                let o = (y as usize * w2 as usize + x as usize) * 4;
                for (k, s) in sum.iter().enumerate() {
                    if let Some(v) = (s + a_sum / 2).checked_div(a_sum) {
                        out[o + k] = v as u8;
                    }
                }
                out[o + 3] = ((a_sum + n / 2) / n.max(1)) as u8;
            }
        }
        t = Texture {
            w: w2,
            h: h2,
            rgba: out,
        };
    }
    Ok(t)
}

/// 埋め込みの画像を読んでテクスチャにする。
fn decode_texture(
    root: &Root,
    bin: Option<&[u8]>,
    image: u32,
    codecs: &CodecSet,
    limits: &ModelLimits,
    mon: &dyn Monitor,
) -> Result<Texture, EngineError> {
    let img = root
        .images
        .get(image as usize)
        .ok_or_else(|| bad("image の番号が範囲の外です"))?;
    let view = img
        .buffer_view
        .ok_or_else(|| EngineError::Unsupported("外部・data: の画像は読みません".into()))?;
    let bytes = gltf::view_bytes(root, bin, view)?;
    let format =
        sniff(bytes).ok_or_else(|| EngineError::Unsupported("知らない画像の形式です".into()))?;
    let lim = Limits {
        max_pixels: limits.max_texture_pixels,
        max_frames: 1,
        max_value_bytes: limits.max_texture_pixels.saturating_mul(8),
        ..Limits::default()
    };
    let (v, _) = codecs.decode(
        &mut Cursor::new(bytes),
        format,
        &DecodeOptions::default(),
        &lim,
        mon,
    )?;
    let px = match &v {
        Value::Image(i) => &i.px,
        Value::Frames(f) => &f.first().px,
        Value::Audio(_) => return Err(bad("画像でない")),
    };
    let t = Texture {
        w: px.width(),
        h: px.height(),
        rgba: texels(px)?,
    };
    drop(v);
    shrink(t, limits.max_texture_dim)
}

/// 属性の列を、頂点の数に合わせて伸ばしてから足す（まだない属性は `fill` で埋める）。
fn extend_attr<T: Copy>(
    v: &mut Vec<T>,
    base: usize,
    data: Option<Vec<T>>,
    count: usize,
    fill: T,
) -> Result<(), EngineError> {
    match data {
        Some(d) => {
            if v.len() < base {
                v.try_reserve(base - v.len() + count)
                    .map_err(|_| EngineError::Limit("頂点の属性を確保できません".into()))?;
                v.resize(base, fill);
            }
            v.try_reserve(count)
                .map_err(|_| EngineError::Limit("頂点の属性を確保できません".into()))?;
            v.extend(d);
        }
        None if !v.is_empty() => {
            v.try_reserve(base + count - v.len())
                .map_err(|_| EngineError::Limit("頂点の属性を確保できません".into()))?;
            v.resize(base + count, fill);
        }
        None => {}
    }
    Ok(())
}

/// glb を読んで焼き込む。描けないもの（必須の Draco・スキンなど。[`super::ModelInfo::unsupported`]）は `Unsupported`、
/// 上限を超えたら `Limit`。読めないプリミティブ・テクスチャは飛ばして `warnings` に書く。
pub fn load_scene(
    bytes: &[u8],
    limits: &ModelLimits,
    mon: &dyn Monitor,
) -> Result<Scene, EngineError> {
    let glb = gltf::parse(bytes, limits)?;
    let root = &glb.root;
    let info = info_of(root, limits)?;
    if !info.unsupported.is_empty() {
        return Err(EngineError::Unsupported(format!(
            "描けない 3D モデルです（{}）",
            info.unsupported.join("・")
        )));
    }
    if info.vertices > limits.max_vertices {
        return Err(EngineError::Limit(format!(
            "頂点が多すぎます（{}）",
            info.vertices
        )));
    }
    if info.triangles > limits.max_triangles {
        return Err(EngineError::Limit(format!(
            "三角形が多すぎます（{}）",
            info.triangles
        )));
    }
    let mut scene = Scene::default();
    // テクスチャ：材質が使うものだけを、画像ごとに 1 度読む
    let codecs = CodecSet::pinned();
    let mut slots: Vec<Option<Option<u32>>> = crate::codec::try_vec(root.images.len(), None)?;
    let mut warnings = Vec::new();
    let mut textures: Vec<Texture> = Vec::new();
    let mut slot_of = |texture: u32| -> Option<u32> {
        let tex = root.textures.get(texture as usize)?;
        let image = tex
            .extensions
            .webp
            .as_ref()
            .and_then(|w| w.source)
            .or(tex.source)?;
        let entry = slots.get_mut(image as usize)?;
        if let Some(done) = entry {
            return *done;
        }
        let made = if textures.len() as u64 >= limits.max_textures {
            Err(EngineError::Limit("テクスチャが多すぎます".into()))
        } else {
            decode_texture(root, glb.bin, image, &codecs, limits, mon)
        };
        let slot = match made {
            Ok(t) => {
                textures.push(t);
                Some(textures.len() as u32 - 1)
            }
            Err(e) => {
                warnings.push(format!("テクスチャ（画像 {image}）を読めません: {e}"));
                None
            }
        };
        *entry = Some(slot);
        slot
    };
    let mut materials: Vec<Material> = Vec::new();
    materials
        .try_reserve_exact(root.materials.len() + 1)
        .map_err(|_| EngineError::Limit("材質を確保できません".into()))?;
    for m in &root.materials {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        materials.push(material(m, root, &mut slot_of));
    }
    // 材質のないプリミティブの既定の材質は末尾に置く
    materials.push(Material::default());
    scene.materials = materials;
    scene.textures = textures;
    scene.warnings = warnings;

    for inst in gltf::walk(root)? {
        let Some(mesh) = root
            .nodes
            .get(inst.node as usize)
            .and_then(|n| n.mesh)
            .and_then(|m| root.meshes.get(m as usize))
        else {
            continue;
        };
        for (k, p) in mesh.primitives.iter().enumerate() {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            match add_primitive(&mut scene, root, glb.bin, p, &inst, limits) {
                Ok(()) => {}
                Err(e @ (EngineError::Limit(_) | EngineError::Canceled)) => return Err(e),
                Err(e) => scene.warnings.push(format!(
                    "ノード {} のプリミティブ {k} を飛ばしました: {e}",
                    inst.node
                )),
            }
        }
    }
    let n = scene.positions.len();
    if !scene.normals.is_empty() {
        scene.normals.resize(n, [0.0; 3]);
    }
    if !scene.uvs.is_empty() {
        scene.uvs.resize(n, [0.0; 2]);
    }
    if !scene.colors.is_empty() {
        scene.colors.resize(n, [1.0; 4]);
    }
    Ok(scene)
}

/// プリミティブ 1 つを焼き込む。
fn add_primitive(
    scene: &mut Scene,
    root: &Root,
    bin: Option<&[u8]>,
    p: &PrimitiveDef,
    inst: &gltf::Instance,
    limits: &ModelLimits,
) -> Result<(), EngineError> {
    let mode = p.mode.unwrap_or(4);
    if !matches!(mode, 4..=6) {
        return Err(EngineError::Unsupported(format!(
            "三角形でないプリミティブ（モード {mode}）は描きません"
        )));
    }
    let pos_index = p
        .attributes
        .get("POSITION")
        .ok_or_else(|| bad("POSITION がありません"))?;
    for ext in ["KHR_draco_mesh_compression", "EXT_meshopt_compression"] {
        let raw = root
            .accessors
            .get(pos_index as usize)
            .is_some_and(|a| a.buffer_view.is_none());
        if p.extensions.contains_key(ext) && raw {
            return Err(EngineError::Unsupported(format!(
                "{ext} の圧縮は読めません"
            )));
        }
    }
    let room = limits
        .max_vertices
        .saturating_sub(scene.positions.len() as u64);
    let positions = gltf::read_floats::<3>(root, bin, pos_index, [0.0; 3], room)?;
    let count = positions.len();
    // 属性は頂点と同じ数だけ（多いものは確保の前に断って、属性なしとして扱う）
    let same = count as u64;
    let read_opt = |name: &str| p.attributes.get(name);
    let normals = match read_opt("NORMAL") {
        Some(i) => gltf::read_floats::<3>(root, bin, i, [0.0; 3], same)
            .ok()
            .filter(|n| n.len() == count),
        None => None,
    };
    let colors = match read_opt("COLOR_0") {
        Some(i) => gltf::read_floats::<4>(root, bin, i, [1.0; 4], same)
            .ok()
            .filter(|c| c.len() == count),
        None => None,
    };
    let material_index = p
        .material
        .filter(|m| (*m as usize) < root.materials.len())
        .unwrap_or(root.materials.len() as u32);
    let mat = scene
        .materials
        .get(material_index as usize)
        .copied()
        .unwrap_or_default();
    let uvs = match (mat.texture, read_opt(&format!("TEXCOORD_{}", mat.uv_set))) {
        (Some(_), Some(i)) => gltf::read_floats::<2>(root, bin, i, [0.0; 2], same)
            .ok()
            .filter(|u| u.len() == count),
        _ => None,
    };
    let base = scene.positions.len();
    let too_many = || EngineError::Limit("頂点が多すぎます".into());
    let base32 = u32::try_from(base).map_err(|_| too_many())?;
    let count32 = u32::try_from(count).map_err(|_| too_many())?;
    base32.checked_add(count32).ok_or_else(too_many)?;
    let indices = match p.indices {
        Some(i) => gltf::read_indices(root, bin, i, limits.max_triangles.saturating_mul(3))?,
        None => {
            let mut v = Vec::new();
            v.try_reserve_exact(count)
                .map_err(|_| EngineError::Limit("索引を確保できません".into()))?;
            v.extend(0..count32);
            v
        }
    };
    let n_tris = super::info::triangle_count(mode, indices.len() as u64);
    if (scene.triangles.len() as u64).saturating_add(n_tris) > limits.max_triangles {
        return Err(EngineError::Limit("三角形が多すぎます".into()));
    }
    // 三角形にする（帯は奇数番めの向きを戻し、扇は先頭の頂点を共有する）
    let mut tris: Vec<[u32; 3]> = Vec::new();
    tris.try_reserve_exact(n_tris as usize)
        .map_err(|_| EngineError::Limit("三角形を確保できません".into()))?;
    match mode {
        4 => tris.extend(
            indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| [c[0], c[1], c[2]]),
        ),
        5 => tris.extend(indices.windows(3).enumerate().map(|(i, w)| {
            if i % 2 == 0 {
                [w[0], w[1], w[2]]
            } else {
                [w[1], w[0], w[2]]
            }
        })),
        _ => {
            if let Some((first, rest)) = indices.split_first() {
                tris.extend(rest.windows(2).map(|w| [*first, w[0], w[1]]));
            }
        }
    }
    drop(indices);
    let world = &inst.world;
    let nm = gltf::normal_matrix(world);
    let part = scene.parts.len() as u32;
    scene.parts.push(Part {
        material: material_index,
        normals: normals.is_some(),
        colors: colors.is_some(),
        flip: gltf::det3(world) < 0.0,
    });
    scene
        .positions
        .try_reserve(count)
        .map_err(|_| EngineError::Limit("頂点を確保できません".into()))?;
    for q in &positions {
        let w = gltf::transform_point(world, [f64::from(q[0]), f64::from(q[1]), f64::from(q[2])]);
        scene
            .positions
            .push([w[0] as f32, w[1] as f32, w[2] as f32]);
    }
    let normals = normals.map(|ns| {
        ns.into_iter()
            .map(|n| {
                let (x, y, z) = (f64::from(n[0]), f64::from(n[1]), f64::from(n[2]));
                let t = [
                    nm[0] * x + nm[3] * y + nm[6] * z,
                    nm[1] * x + nm[4] * y + nm[7] * z,
                    nm[2] * x + nm[5] * y + nm[8] * z,
                ];
                let len = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
                if len > 0.0 && len.is_finite() {
                    [
                        (t[0] / len) as f32,
                        (t[1] / len) as f32,
                        (t[2] / len) as f32,
                    ]
                } else {
                    [0.0; 3]
                }
            })
            .collect()
    });
    extend_attr(&mut scene.normals, base, normals, count, [0.0; 3])?;
    extend_attr(&mut scene.colors, base, colors, count, [1.0; 4])?;
    extend_attr(&mut scene.uvs, base, uvs, count, [0.0; 2])?;
    scene
        .triangles
        .try_reserve(tris.len())
        .map_err(|_| EngineError::Limit("三角形を確保できません".into()))?;
    // 範囲の外の番号を含むものは捨てる（base + count は u32 に収まることを確かめてある）
    for t in tris {
        if t.iter().all(|i| *i < count32) {
            scene.triangles.push(Tri {
                v: t.map(|i| i + base32),
                part,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_by_three() -> Texture {
        Texture {
            w: 3,
            h: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 0, 10, 20, 30, 255, //
                255, 0, 0, 255, 0, 255, 0, 0, 10, 20, 30, 255,
            ],
        }
    }

    /// 縮めるときは色をアルファで重み付けする（透明な画素の色が混ざらない）。奇数の辺は端の画素だけで平均する。
    #[test]
    fn shrink_weights_colors_by_alpha() {
        let s = shrink(two_by_three(), 1).unwrap();
        assert_eq!((s.w, s.h), (1, 1));
        let s = shrink(two_by_three(), 2).unwrap();
        assert_eq!((s.w, s.h), (2, 1));
        // 左：赤（α 255）2 つと緑（α 0）2 つ → 赤・α は半分
        assert_eq!(&s.rgba[..4], &[255, 0, 0, 128]);
        // 右：端の列だけ（2 つ）
        assert_eq!(&s.rgba[4..], &[10, 20, 30, 255]);
        // 収まっていれば縮めない
        assert_eq!(shrink(two_by_three(), 3).unwrap(), two_by_three());
    }

    /// 16 ビットのテクスチャは丸めて 8 ビットにする。
    #[test]
    fn sixteen_bit_texels_are_rounded() {
        let mut row = Vec::new();
        for v in [0u16, 128, 65_535, 32_768] {
            row.extend_from_slice(&v.to_le_bytes());
        }
        let px = Raster::from_rows(1, 1, PixelFormat::Rgba16, None, &row).unwrap();
        assert_eq!(texels(&px).unwrap(), [0, 0, 255, 128]);
        let gray =
            Raster::from_rows(1, 1, PixelFormat::GrayA16, None, &[0xFF, 0xFF, 0, 0]).unwrap();
        assert_eq!(texels(&gray).unwrap(), [255, 255, 255, 0]);
    }

    #[test]
    fn image_formats_are_sniffed() {
        assert_eq!(sniff(&[0x89, b'P', b'N', b'G', 13, 10]), Some("png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(&[]), None);
    }
}
