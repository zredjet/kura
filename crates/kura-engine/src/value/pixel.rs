//! 画素の形式（docs/editing.md §5.2）。アルファは常にストレート。16 ビットはリトルエンディアンで並べる。

use serde::{Deserialize, Serialize};

/// 画素の形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PixelFormat {
    Gray8,
    GrayA8,
    Rgb8,
    Rgba8,
    Gray16,
    GrayA16,
    Rgb16,
    Rgba16,
    /// パレットの番号（パレットは `Raster` が持つ）。
    Indexed8,
}

impl PixelFormat {
    /// 1 画素のバイト数。
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Gray8 | Self::Indexed8 => 1,
            Self::GrayA8 | Self::Gray16 => 2,
            Self::Rgb8 => 3,
            Self::Rgba8 | Self::GrayA16 => 4,
            Self::Rgb16 => 6,
            Self::Rgba16 => 8,
        }
    }

    /// アルファを持つか（Indexed8 はパレットの色が持つので false）。
    pub fn has_alpha(self) -> bool {
        matches!(
            self,
            Self::GrayA8 | Self::Rgba8 | Self::GrayA16 | Self::Rgba16
        )
    }

    /// ダイジェストに書く番号。凍結する（変えるとダイジェストが変わる）。
    pub fn code(self) -> u8 {
        match self {
            Self::Gray8 => 1,
            Self::GrayA8 => 2,
            Self::Rgb8 => 3,
            Self::Rgba8 => 4,
            Self::Gray16 => 5,
            Self::GrayA16 => 6,
            Self::Rgb16 => 7,
            Self::Rgba16 => 8,
            Self::Indexed8 => 9,
        }
    }

    /// すべての形式（テスト用）。
    pub const ALL: [PixelFormat; 9] = [
        Self::Gray8,
        Self::GrayA8,
        Self::Rgb8,
        Self::Rgba8,
        Self::Gray16,
        Self::GrayA16,
        Self::Rgb16,
        Self::Rgba16,
        Self::Indexed8,
    ];
}

/// パレット（256 色まで。色はストレートの RGBA8）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Palette {
    pub colors: Vec<[u8; 4]>,
}
