//! 資源の上限（docs/editing.md §13）。形の推論の段階で確かめ、描く前に断る（E25）。

use crate::shape::Shape;

/// 上限。既定値は設定で変えられる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// 画像の 1 辺の画素数。
    pub max_dim: u64,
    /// 画像 1 枚の画素数。
    pub max_pixels: u64,
    /// フレームの数。
    pub max_frames: u64,
    /// 1 つの値の中身のバイト数（画素・PCM）。
    pub max_value_bytes: u64,
    /// 音声のフレーム数（1 チャンネルあたり）。
    pub max_audio_frames: u64,
    /// 音声のチャンネル数。
    pub max_channels: u64,
    /// 文書のノードの数。
    pub max_nodes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_dim: 32_768,
            max_pixels: 1 << 28,
            max_frames: 4_096,
            max_value_bytes: 4 << 30,
            // 48kHz で 3 時間
            max_audio_frames: 48_000 * 60 * 60 * 3,
            max_channels: 32,
            max_nodes: 10_000,
        }
    }
}

impl Limits {
    /// 形が上限に収まるか。収まらなければ理由を返す。
    pub fn check(&self, shape: &Shape) -> Result<(), String> {
        match shape {
            Shape::Image { w, h, fmt } | Shape::Frames { w, h, fmt, .. } => {
                let (w, h) = (w.max(), h.max());
                if w > self.max_dim || h > self.max_dim {
                    return Err(format!(
                        "画像の 1 辺が {} を超えます（{w}×{h}）",
                        self.max_dim
                    ));
                }
                let pixels = w.saturating_mul(h);
                if pixels > self.max_pixels {
                    return Err(format!("画素数が上限を超えます（{w}×{h}）"));
                }
                let frames = match shape {
                    Shape::Frames { count, .. } => count.max(),
                    _ => 1,
                };
                if frames > self.max_frames {
                    return Err(format!(
                        "フレームの数が {} を超えます（{frames}）",
                        self.max_frames
                    ));
                }
                let bytes = pixels
                    .saturating_mul(fmt.bytes_per_pixel() as u64)
                    .saturating_mul(frames);
                if bytes > self.max_value_bytes {
                    return Err("値の大きさが上限を超えます".to_string());
                }
                Ok(())
            }
            Shape::Audio {
                channels, frames, ..
            } => {
                let frames = frames.max();
                if u64::from(*channels) > self.max_channels {
                    return Err(format!(
                        "チャンネルの数が {} を超えます（{channels}）",
                        self.max_channels
                    ));
                }
                if frames > self.max_audio_frames {
                    return Err("音声が長すぎます".to_string());
                }
                let bytes = frames
                    .saturating_mul(u64::from(*channels))
                    .saturating_mul(4);
                if bytes > self.max_value_bytes {
                    return Err("値の大きさが上限を超えます".to_string());
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::Dim;
    use crate::value::PixelFormat;

    #[test]
    fn rejects_shapes_over_the_limits() {
        let limits = Limits::default();
        let ok = Shape::Image {
            w: Dim::Exact(4096),
            h: Dim::Exact(4096),
            fmt: PixelFormat::Rgba8,
        };
        assert!(limits.check(&ok).is_ok());
        let wide = Shape::Image {
            w: Dim::Exact(40_000),
            h: Dim::Exact(1),
            fmt: PixelFormat::Rgba8,
        };
        assert!(limits.check(&wide).is_err());
        let many = Shape::Image {
            w: Dim::AtMost(20_000),
            h: Dim::Exact(20_000),
            fmt: PixelFormat::Gray8,
        };
        assert!(limits.check(&many).is_err());
        let long = Shape::Audio {
            rate: 48_000,
            channels: 2,
            frames: Dim::Exact(u64::MAX),
        };
        assert!(limits.check(&long).is_err());
    }
}
