//! 3D モデル（glb）の読み手と描画（M11 の決定 9。docs/editing.md §9.2）。
//!
//! - **glb だけ**（glTF 2.0 のバイナリ。JSON の塊と BIN の塊）。外部のファイル・data: の URI は読まない（その部分は飛ばす）。
//! - **情報**（[`read_info`]）：JSON の塊だけから数える（BIN は読まない）。頂点・三角形（既定のシーンのノードの実体ごと）・
//!   範囲（POSITION の min・max をノードの変換で写した和）・メッシュ・材質・テクスチャ・アニメ・スキン・ノードの数、
//!   使う・必須の拡張、描けない理由（[`ModelInfo::unsupported`]）。
//! - **描画**（[`load_scene`] → [`render`]、まとめて [`thumbnail`]）：SceneKit などの OS の描画を使わない Rust のソフトウェアの
//!   描画（三角形・z バッファ・基本色と埋め込みのテクスチャ・簡単な陰影・超標本化のアンチエイリアス・透明の背景・範囲から
//!   決めた正射影のカメラ）。値は Rgba8 の [`Value::Image`]。決定性の規則に従う（超越関数は `crate::math`、並列は
//!   `par_blocks`、浮動小数の min・max は `math`）ので、どの環境でも同じ値になる（ゴールデンで凍結する）。
//! - **描かないもの**：必須の Draco・meshopt・basisu などの圧縮・知らない必須の拡張・スキン（決定 9。情報だけ出して、
//!   サムネイルは種類のアイコン）。点・線のプリミティブ・疎な accessor・外部の参照は飛ばす（[`Scene::warnings`]）。
//! - **壊れた入力**：上限（[`ModelLimits`]）を確保の前に確かめ、`try_reserve_exact` で確保する。JSON は serde_json に渡す前に
//!   生のバイト列で入れ子の深さ（64）とオブジェクトの数を数えて断る。ノードの木は再帰せずにたどり、同じノードは 1 度だけ
//!   （輪・共有の子を断つ）。自前の走査はどの枝でも進む。panic しない（ファズの対象 `model_glb`）。

mod build;
mod gltf;
mod info;
mod raster;
mod scene;

pub use build::{GlbBuilder, glb_bytes, sample_glbs};
pub use info::{Bounds, ModelInfo, read_info};
pub use raster::{RenderEstimate, RenderOptions, ViewDir, render};
pub use scene::{Scene, load_scene};

use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::value::Value;

/// glb の読み手の約束の版（読み方・数え方を変えたら上げる）。
pub const GLB_READER: &str = "glb-kura-read/1";
/// 描画の約束の版（描く値が変わる変更をしたら上げる。`goldens-model/` は追記だけ）。
pub const RENDERER: &str = "model-kura-render/1";

/// 読み手と描画の上限。既定値は、ふつうの素材が収まり、壊れた入力でもメモリと時間が抑えられる値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLimits {
    /// glb のファイルの大きさ。
    pub max_file_bytes: u64,
    /// JSON の塊の大きさ。
    pub max_json_bytes: u64,
    /// JSON のオブジェクトの数（serde で作る構造体の数を抑える）。
    pub max_json_objects: u64,
    /// ノードの数（JSON の `nodes`）。
    pub max_nodes: u64,
    /// 描くメッシュの実体（メッシュを持つノード）× プリミティブの数。
    pub max_parts: u64,
    /// 焼き込んだ頂点の数（実体ごとに数える）。
    pub max_vertices: u64,
    /// 焼き込んだ三角形の数。
    pub max_triangles: u64,
    /// 読むテクスチャの画像の数。
    pub max_textures: u64,
    /// テクスチャの画像 1 枚の画素数（読む前に形で確かめる）。
    pub max_texture_pixels: u64,
    /// 持っておくテクスチャの長辺（超えたら半分に縮め続ける）。
    pub max_texture_dim: u32,
    /// 描く画像の長辺。
    pub max_output_dim: u32,
    /// 描く手間（三角形の囲む矩形の標本の数の和）の、画像の標本の数に対する倍率。
    pub max_overdraw: u64,
}

impl Default for ModelLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 512 << 20,
            max_json_bytes: 32 << 20,
            max_json_objects: 1 << 17,
            max_nodes: 1 << 16,
            max_parts: 1 << 16,
            max_vertices: 1 << 22,
            max_triangles: 1 << 22,
            max_textures: 32,
            max_texture_pixels: 1 << 24,
            max_texture_dim: 1024,
            max_output_dim: 4096,
            max_overdraw: 64,
        }
    }
}

impl ModelLimits {
    /// ファズの上限（1 回を速くする）。
    pub fn fuzz() -> Self {
        Self {
            max_file_bytes: 4 << 20,
            max_json_bytes: 1 << 20,
            max_json_objects: 4_096,
            max_nodes: 1_024,
            max_parts: 1_024,
            max_vertices: 1 << 16,
            max_triangles: 1 << 16,
            max_textures: 4,
            max_texture_pixels: 1 << 16,
            max_texture_dim: 64,
            max_output_dim: 64,
            max_overdraw: 64,
        }
    }
}

/// 読み取りの失敗（壊れた glb）。
pub(crate) fn bad(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: GLB_READER.to_string(),
        message: message.into(),
    }
}

/// サムネイル：情報で描けるかを確かめ、読み、長辺 `size` の正方形に収めて描く（`size` が 512 以下は 3×3、それより大きければ
/// 2×2 の超標本化）。描けないもの（[`ModelInfo::unsupported`]）は `Unsupported`。
pub fn thumbnail(
    bytes: &[u8],
    size: u32,
    limits: &ModelLimits,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    let info = read_info(bytes, limits)?;
    if !info.unsupported.is_empty() {
        return Err(EngineError::Unsupported(format!(
            "描けない 3D モデルです（{}）",
            info.unsupported.join("・")
        )));
    }
    let scene = load_scene(bytes, limits, mon)?;
    let opts = RenderOptions {
        size,
        samples: if size <= 512 { 3 } else { 2 },
        view: ViewDir::ThreeQuarter,
    };
    render(&scene, &opts, limits, mon)
}
