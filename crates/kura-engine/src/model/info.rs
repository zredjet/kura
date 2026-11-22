//! glb の情報（JSON の塊だけから数える。BIN は読まない）。

use serde::Serialize;

use super::ModelLimits;
use super::gltf::{self, Root};
use crate::error::EngineError;

/// 描くのに必須でも、無くて困らない（または自前で扱う）拡張。これ以外の必須の拡張があれば描かない。
const RENDERABLE_REQUIRED: [&str; 6] = [
    "KHR_mesh_quantization",
    "KHR_texture_transform",
    "KHR_materials_unlit",
    "EXT_texture_webp",
    "KHR_lights_punctual",
    "KHR_materials_variants",
];

/// 必須でも見た目だけにかかわる材質の拡張の頭（clearcoat・transmission など。基本色で描く）。
const MATERIAL_PREFIX: &str = "KHR_materials_";

/// 範囲（世界の座標）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Bounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Bounds {
    /// 大きさ（max − min）。
    pub fn size(&self) -> [f64; 3] {
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }
}

/// 3D モデルの情報（インスペクタの素材の欄に出す。決定の「既定」：頂点・三角形・大きさ・材質の数・未対応の拡張の印）。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// 頂点の数（既定のシーンのノードの実体ごとに、プリミティブの POSITION の数を足したもの）。
    pub vertices: u64,
    /// 三角形の数（同じく実体ごと。点・線のプリミティブは数えない）。
    pub triangles: u64,
    pub meshes: u32,
    /// 描くプリミティブの実体の数。
    pub parts: u64,
    pub materials: u32,
    pub textures: u32,
    pub images: u32,
    pub animations: u32,
    pub skins: u32,
    pub nodes: u32,
    /// 範囲（POSITION の min・max の角をノードの変換で写した和）。数えられなければ None。
    pub bounds: Option<Bounds>,
    /// 作った道具（asset.generator）。
    pub generator: Option<String>,
    pub extensions_used: Vec<String>,
    pub extensions_required: Vec<String>,
    /// 描けない理由（必須の Draco・meshopt・知らない拡張の名前、`skin`）。空なら描ける。
    pub unsupported: Vec<String>,
    /// JSON を読んで組み立てる確保の見積もり（バイト。オブジェクトと値の数から。`load_scene` も同じ JSON を読み直すので
    /// [`Self::estimate_load_bytes`] に入る）。ライブラリに残す情報ではないので、書き出さない。
    #[serde(skip)]
    pub json_heap: u64,
}

impl ModelInfo {
    /// `load_scene` が使うメモリの見積もり（glb のバイト列そのものは含めない。上の側に）。焼き込んだ頂点（位置・法線・UV・
    /// 色で 48 バイト）と三角形（16 バイト）、読む途中の accessor の写しと索引（頂点 48・三角形 24 バイト）、テクスチャ
    /// （持つもの `max_textures` 枚 × `max_texture_dim`² × 4 と、読む途中の 1 枚 `max_texture_pixels` × 16）、JSON を
    /// 組み立てる分（[`Self::json_heap`]）。
    pub fn estimate_load_bytes(&self, limits: &super::ModelLimits) -> u64 {
        let v = self.vertices.saturating_mul(96);
        let t = self.triangles.saturating_mul(40);
        let textures = if self.images == 0 {
            0
        } else {
            let kept = u64::from(self.images)
                .min(limits.max_textures)
                .saturating_mul(
                    u64::from(limits.max_texture_dim)
                        .saturating_mul(u64::from(limits.max_texture_dim))
                        * 4,
                );
            kept.saturating_add(limits.max_texture_pixels.saturating_mul(16))
        };
        v.saturating_add(t)
            .saturating_add(textures)
            .saturating_add(self.json_heap)
    }
}

/// u64 を u32 に（超えたら上限で止める。数は上限で確かめてある）。
fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// プリミティブの三角形の数（モードと、索引か頂点の数から）。
pub(crate) fn triangle_count(mode: u32, n: u64) -> u64 {
    match mode {
        4 => n / 3,
        5 | 6 => n.saturating_sub(2),
        _ => 0,
    }
}

/// glb の情報を読む（JSON の塊だけ）。
pub fn read_info(bytes: &[u8], limits: &ModelLimits) -> Result<ModelInfo, EngineError> {
    let glb = gltf::parse(bytes, limits)?;
    let mut info = info_of(&glb.root, limits)?;
    info.json_heap = glb.json_heap;
    Ok(info)
}

/// 情報を数える。描くプリミティブの実体（ノードの実体 × プリミティブ）が `max_parts` を超えたら、数える手間が大きすぎる
/// ので `Limit`（同じメッシュを指すノードが多いと、ノードとプリミティブの数の積になる）。
pub(crate) fn info_of(root: &Root, limits: &ModelLimits) -> Result<ModelInfo, EngineError> {
    let mut info = ModelInfo {
        meshes: count(root.meshes.len()),
        materials: count(root.materials.len()),
        textures: count(root.textures.len()),
        images: count(root.images.len()),
        animations: count(root.animations.len()),
        skins: count(root.skins.len()),
        nodes: count(root.nodes.len()),
        generator: root.asset.generator.as_deref().map(str::to_string),
        extensions_used: root.extensions_used.0.clone(),
        extensions_required: root.extensions_required.0.clone(),
        ..ModelInfo::default()
    };
    for ext in &root.extensions_required.0 {
        if !RENDERABLE_REQUIRED.contains(&ext.as_str()) && !ext.starts_with(MATERIAL_PREFIX) {
            info.unsupported.push(ext.clone());
        }
    }
    if !root.skins.is_empty() || root.nodes.iter().any(|n| n.skin.is_some()) {
        info.unsupported.push("skin".into());
    }
    let mut bounds: Option<Bounds> = None;
    for inst in gltf::walk(root)? {
        let Some(mesh) = root
            .nodes
            .get(inst.node as usize)
            .and_then(|n| n.mesh)
            .and_then(|m| root.meshes.get(m as usize))
        else {
            continue;
        };
        for p in &mesh.primitives {
            let mode = p.mode.unwrap_or(4);
            let Some(pos) = p
                .attributes
                .get("POSITION")
                .and_then(|a| root.accessors.get(a as usize))
            else {
                continue;
            };
            info.parts = info.parts.saturating_add(1);
            if info.parts > limits.max_parts {
                return Err(EngineError::Limit(format!(
                    "描くプリミティブの実体が多すぎます（{} を超える）",
                    limits.max_parts
                )));
            }
            info.vertices = info.vertices.saturating_add(pos.count);
            let n = p
                .indices
                .and_then(|i| root.accessors.get(i as usize))
                .map_or(pos.count, |a| a.count);
            info.triangles = info.triangles.saturating_add(triangle_count(mode, n));
            if let (Some(lo), Some(hi)) = (&pos.min, &pos.max)
                && lo.len() >= 3
                && hi.len() >= 3
            {
                for corner in 0..8 {
                    let c = [
                        if corner & 1 == 0 { lo[0] } else { hi[0] },
                        if corner & 2 == 0 { lo[1] } else { hi[1] },
                        if corner & 4 == 0 { lo[2] } else { hi[2] },
                    ];
                    let w = gltf::transform_point(&inst.world, c);
                    if !w.iter().all(|v| v.is_finite()) {
                        continue;
                    }
                    let b = bounds.get_or_insert(Bounds { min: w, max: w });
                    for (k, v) in w.iter().enumerate() {
                        b.min[k] = crate::math::min(b.min[k], *v);
                        b.max[k] = crate::math::max(b.max[k], *v);
                    }
                }
            }
        }
    }
    info.bounds = bounds;
    Ok(info)
}
