//! 値に付くメタデータ（docs/editing.md §6）。座標・時刻を持つものは、エンジンが `mapping` でまとめて写す。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::geom::{IPoint, IRect};

/// スプライトの格子。左上の `offset` から、`cell + gap` の間隔でセルが並ぶ。端の欠けたセルは数えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grid {
    pub cell_w: u32,
    pub cell_h: u32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub gap_x: u32,
    pub gap_y: u32,
}

/// 名前付きの範囲（aseprite のスライス・9 分割）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slice {
    pub name: String,
    pub rect: IRect,
    /// 9 分割の中央。
    pub center: Option<IRect>,
    pub pivot: Option<IPoint>,
}

/// 画像のメタデータ。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageMeta {
    pub grid: Option<Grid>,
    pub pivot: Option<IPoint>,
    pub slices: Vec<Slice>,
    /// 解像度（1 インチあたりの画素）。
    pub dpi: Option<(u32, u32)>,
    /// PNG のテキストなど（そのまま運ぶ）。
    pub text: BTreeMap<String, String>,
    /// ICC プロファイル（そのまま運ぶ。ダイジェストには中身のハッシュを入れる）。
    #[serde(skip)]
    pub icc: Option<Arc<Vec<u8>>>,
}

/// 音声のループ区間（フレーム。`end` は含まない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRange {
    pub start: u64,
    pub end: u64,
}

/// 音声のマーカー。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub pos: u64,
    pub name: String,
}

/// 元のファイルの音声の形式（書き出しの既定値に使う）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSource {
    pub bits: u16,
    pub float: bool,
    pub rate: u32,
}

/// 音声のメタデータ。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioMeta {
    pub loop_range: Option<LoopRange>,
    pub markers: Vec<Marker>,
    pub source: Option<AudioSource>,
    /// タグ（そのまま運ぶ）。
    pub tags: BTreeMap<String, String>,
}
