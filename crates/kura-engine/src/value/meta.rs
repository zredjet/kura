//! 値に付くメタデータ（docs/editing.md §6）。座標・時刻を持つものは、エンジンが `mapping` でまとめて写す。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::geom::{IPoint, IRect};

/// スプライトの格子。左上の `offset` から、`cell + gap` の間隔でセルが並ぶ。端の欠けたセルは数えない。
/// 値のメタデータの型。操作の引数には `ops::util::GridParam` を使う（メタデータに項目を足しても引数の形が変わらないように）。
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

/// アニメのタグの再生の向き（aseprite）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TagDirection {
    Forward,
    Reverse,
    PingPong,
    PingPongReverse,
}

/// アニメのタグ（aseprite）。フレームの番号の範囲（両端を含む）。`repeat` は 0 が無限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimTag {
    pub name: String,
    pub from: u32,
    pub to: u32,
    pub direction: TagDirection,
    pub repeat: u32,
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
    /// アニメのタグ（aseprite）。空なら正規の JSON に書かない（足す前の値のダイジェストを変えない）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<AnimTag>,
    /// ICC プロファイル（そのまま運ぶ。ダイジェストには中身のハッシュを入れる）。
    #[serde(skip)]
    pub icc: Option<Arc<Vec<u8>>>,
    /// EXIF（TIFF の形のバイト列。先頭の `Exif\0\0` は付けない）。読むときに向き（Orientation）を画素に当て、ここでは 1（そのまま）にしてある。
    #[serde(skip)]
    pub exif: Option<Arc<Vec<u8>>>,
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
