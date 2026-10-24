//! 形と推論（docs/editing.md §5.4）。描かずに出力の種類・大きさ・長さが分かる。

use serde::{Deserialize, Serialize};

use crate::value::PixelFormat;

/// 値の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
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

    /// 値の中身（画素・PCM）のバイト数の上の見積もり（`AtMost` は最大で数える。あふれたら `u64::MAX`）。
    /// `Limits::check` の `max_value_bytes` の照らしと、キャッシュ・予算の見積もりが同じ式で数える。
    pub fn value_bytes(&self) -> u64 {
        match self {
            Self::Image { w, h, fmt } => w
                .max()
                .saturating_mul(h.max())
                .saturating_mul(fmt.bytes_per_pixel() as u64),
            Self::Frames {
                count, w, h, fmt, ..
            } => w
                .max()
                .saturating_mul(h.max())
                .saturating_mul(fmt.bytes_per_pixel() as u64)
                .saturating_mul(count.max()),
            Self::Audio {
                channels, frames, ..
            } => frames
                .max()
                .saturating_mul(u64::from(*channels))
                .saturating_mul(4),
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

    /// 値のバイト数：画像は幅×高さ×画素、フレームの列はその数倍、音声は標本×チャンネル×4。AtMost は最大で数え、
    /// あふれは u64::MAX に止める。
    #[test]
    fn value_bytes_counts_the_largest_value() {
        let img = Shape::Image {
            w: Dim::AtMost(300),
            h: Dim::Exact(20),
            fmt: PixelFormat::Rgba16,
        };
        assert_eq!(img.value_bytes(), 300 * 20 * 8);
        let frames = Shape::Frames {
            count: Dim::Exact(3),
            w: Dim::Exact(5),
            h: Dim::Exact(4),
            fmt: PixelFormat::Indexed8,
            full: Fullness::Full,
        };
        assert_eq!(frames.value_bytes(), 3 * 5 * 4);
        let audio = Shape::Audio {
            rate: 48_000,
            channels: 2,
            frames: Dim::AtMost(1_000),
        };
        assert_eq!(audio.value_bytes(), 1_000 * 2 * 4);
        let huge = Shape::Image {
            w: Dim::Exact(u64::MAX),
            h: Dim::Exact(2),
            fmt: PixelFormat::Rgb8,
        };
        assert_eq!(huge.value_bytes(), u64::MAX);
    }

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
