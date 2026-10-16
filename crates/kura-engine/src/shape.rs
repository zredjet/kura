//! 形と推論（docs/editing.md §5.4）。描かずに出力の種類・大きさ・長さが分かる。

use serde::{Deserialize, Serialize};

use crate::value::PixelFormat;

/// 値の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueKind {
    Image,
    Frames,
    Audio,
}

/// 大きさ。中身で決まる操作（透明・無音のトリム）は `AtMost`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dim {
    Exact(u64),
    AtMost(u64),
}

impl Dim {
    /// 取りうる最大。
    pub fn max(self) -> u64 {
        match self {
            Self::Exact(n) | Self::AtMost(n) => n,
        }
    }

    /// 決まっていればその値。
    pub fn exact(self) -> Option<u64> {
        match self {
            Self::Exact(n) => Some(n),
            Self::AtMost(_) => None,
        }
    }

    /// 実際の値がこの推論と合うか（`Exact` は一致、`AtMost` はそれ以下）。
    pub fn admits(self, actual: u64) -> bool {
        match self {
            Self::Exact(n) => n == actual,
            Self::AtMost(n) => actual <= n,
        }
    }

    /// 同じ種類のまま値を写す（`Exact` は `Exact`、`AtMost` は `AtMost`）。
    pub fn map(self, f: impl FnOnce(u64) -> u64) -> Self {
        match self {
            Self::Exact(n) => Self::Exact(f(n)),
            Self::AtMost(n) => Self::AtMost(f(n)),
        }
    }
}

/// フレームの列が、どのフレームもキャンバスいっぱいか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Fullness {
    /// どのフレームもキャンバスいっぱい（フレームごとに持ち上げられる）。
    Full,
    /// トリムしたフレームがある。
    Trimmed,
    /// 中身で決まる（推論の段階では分からない）。
    Unknown,
}

impl Fullness {
    /// 実際の値がこの推論と合うか（Unknown はどちらも受け入れる）。
    pub fn admits(self, actual: Fullness) -> bool {
        self == Fullness::Unknown || self == actual
    }
}

/// 値の形。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Shape {
    Image {
        w: Dim,
        h: Dim,
        fmt: PixelFormat,
    },
    /// フレームの列。`w`・`h` は共通のキャンバスの大きさ。`full` はどのフレームもキャンバスいっぱいか（フレームごとに持ち上げられる）。
    Frames {
        count: Dim,
        w: Dim,
        h: Dim,
        fmt: PixelFormat,
        full: Fullness,
    },
    Audio {
        rate: u32,
        channels: u16,
        frames: Dim,
    },
}

impl Shape {
    /// 値の種類。
    pub fn kind(&self) -> ValueKind {
        match self {
            Self::Image { .. } => ValueKind::Image,
            Self::Frames { .. } => ValueKind::Frames,
            Self::Audio { .. } => ValueKind::Audio,
        }
    }

    /// 実際の形（すべて `Exact`）がこの推論と合うか（E9）。
    pub fn admits(&self, actual: &Shape) -> bool {
        match (self, actual) {
            (
                Self::Image { w, h, fmt },
                Self::Image {
                    w: aw,
                    h: ah,
                    fmt: af,
                },
            ) => fmt == af && w.admits(aw.max()) && h.admits(ah.max()),
            (
                Self::Frames {
                    count,
                    w,
                    h,
                    fmt,
                    full,
                },
                Self::Frames {
                    count: ac,
                    w: aw,
                    h: ah,
                    fmt: af,
                    full: afull,
                },
            ) => {
                fmt == af
                    && full.admits(*afull)
                    && count.admits(ac.max())
                    && w.admits(aw.max())
                    && h.admits(ah.max())
            }
            (
                Self::Audio {
                    rate,
                    channels,
                    frames,
                },
                Self::Audio {
                    rate: ar,
                    channels: ac,
                    frames: af,
                },
            ) => rate == ar && channels == ac && frames.admits(af.max()),
            _ => false,
        }
    }

    /// キャンバスいっぱい（か分からない）フレームの列なら、1 フレームの画像の形（持ち上げに使う）。
    pub fn frame_image(&self) -> Option<Shape> {
        match *self {
            Self::Frames {
                w,
                h,
                fmt,
                full: Fullness::Full | Fullness::Unknown,
                ..
            } => Some(Self::Image { w, h, fmt }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_exact_and_at_most() {
        let inferred = Shape::Image {
            w: Dim::AtMost(10),
            h: Dim::Exact(5),
            fmt: PixelFormat::Rgba8,
        };
        let ok = Shape::Image {
            w: Dim::Exact(7),
            h: Dim::Exact(5),
            fmt: PixelFormat::Rgba8,
        };
        let too_wide = Shape::Image {
            w: Dim::Exact(11),
            h: Dim::Exact(5),
            fmt: PixelFormat::Rgba8,
        };
        let other_h = Shape::Image {
            w: Dim::Exact(7),
            h: Dim::Exact(4),
            fmt: PixelFormat::Rgba8,
        };
        let other_fmt = Shape::Image {
            w: Dim::Exact(7),
            h: Dim::Exact(5),
            fmt: PixelFormat::Rgb8,
        };
        assert!(inferred.admits(&ok));
        assert!(!inferred.admits(&too_wide));
        assert!(!inferred.admits(&other_h));
        assert!(!inferred.admits(&other_fmt));
        let audio = Shape::Audio {
            rate: 1,
            channels: 1,
            frames: Dim::Exact(1),
        };
        assert!(!inferred.admits(&audio));
    }
}
