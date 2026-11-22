//! C の口の型（`abi`）とエンジンの backend の型（`kura_engine::codec::platform`）のあいだの変換。OS によらない。
//! Swift が約束を破った値（知らない番号・負の長さ）は呼び方の誤り（`Invalid`。エンジンの内部の誤り）にする。

// macOS 以外では使う側（bridge と backend）がない（Linux ではテストだけが使う）
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use kura_engine::codec::platform::{
    AacMode, AacSettings, AudioInfo, BackendError, DecodedFrame, ImageInfo, PlatformAlpha, Priming,
    rate_from_f64,
};
use kura_engine::value::{AudioSource, PixelFormat};

use crate::abi::{
    ALPHA_NONE, ALPHA_PREMULTIPLIED, ALPHA_STRAIGHT, AUD_FLOAT, AUD_PCM, AacSettingsC, AudioInfoC,
    IMG_HDR, ImageFrameC, ImageInfoC,
};

/// kura の PixelFormat::code（1〜8。Swift はパレットを広げるので 9 は来ない）。
pub(crate) fn pixel_format(code: i32) -> Option<PixelFormat> {
    PixelFormat::ALL
        .into_iter()
        .filter(|f| *f != PixelFormat::Indexed8)
        .find(|f| i32::from(f.code()) == code)
}

/// アルファの扱いの番号。
pub(crate) fn alpha(code: i32) -> Option<PlatformAlpha> {
    match code {
        ALPHA_NONE => Some(PlatformAlpha::None),
        ALPHA_STRAIGHT => Some(PlatformAlpha::Straight),
        ALPHA_PREMULTIPLIED => Some(PlatformAlpha::Premultiplied),
        _ => None,
    }
}

fn length(v: i64, what: &str) -> Result<u64, BackendError> {
    u64::try_from(v).map_err(|_| BackendError::Invalid(format!("{what}の長さが負です（{v}）")))
}

/// 開いた画像の形。HDR の旗が立っていれば `hdr`（adapter が `Unsupported` にする）。
pub(crate) fn image_info(c: &ImageInfoC) -> Result<ImageInfo, BackendError> {
    let hdr = c.flags & IMG_HDR != 0;
    let (format, alpha) = if hdr {
        (PixelFormat::Rgb8, PlatformAlpha::None)
    } else {
        (
            pixel_format(c.pixel_format).ok_or_else(|| {
                BackendError::Invalid(format!("知らない画素の形式です（{}）", c.pixel_format))
            })?,
            alpha(c.alpha).ok_or_else(|| {
                BackendError::Invalid(format!("知らないアルファの扱いです（{}）", c.alpha))
            })?,
        )
    };
    Ok(ImageInfo {
        width: c.width,
        height: c.height,
        format,
        alpha,
        orientation: u8::try_from(c.orientation).unwrap_or(1),
        frames: c.frames,
        loop_count: c.loop_count,
        icc_len: length(c.icc_len, "ICC ")?,
        exif_len: length(c.exif_len, "EXIF ")?,
        hdr,
    })
}

/// 展開したフレームの実際の形。
pub(crate) fn decoded_frame(c: &ImageFrameC) -> Result<DecodedFrame, BackendError> {
    Ok(DecodedFrame {
        width: c.width,
        height: c.height,
        format: pixel_format(c.pixel_format).ok_or_else(|| {
            BackendError::Invalid(format!("知らない画素の形式です（{}）", c.pixel_format))
        })?,
        alpha: alpha(c.alpha).ok_or_else(|| {
            BackendError::Invalid(format!("知らないアルファの扱いです（{}）", c.alpha))
        })?,
        duration_ms: c.duration_ms,
    })
}

/// 開いた音声の形。
pub(crate) fn audio_info(c: &AudioInfoC) -> Result<AudioInfo, BackendError> {
    let rate = rate_from_f64(c.rate)?;
    let channels = u16::try_from(c.channels)
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| {
            BackendError::Unsupported(format!("チャンネルの数を扱えません（{}）", c.channels))
        })?;
    let frames = u64::try_from(c.frames)
        .map_err(|_| BackendError::Failed(format!("長さが負です（{}）", c.frames)))?;
    let priming = match u64::try_from(c.priming) {
        Ok(start) => Priming::Trimmed {
            start,
            end: u64::try_from(c.remainder).unwrap_or(0),
        },
        Err(_) => Priming::None,
    };
    let source = (c.flags & AUD_PCM != 0).then(|| AudioSource {
        bits: u16::try_from(c.bits).unwrap_or(32),
        float: c.flags & AUD_FLOAT != 0,
        rate,
    });
    Ok(AudioInfo {
        rate,
        channels,
        frames,
        priming,
        source,
    })
}

/// AAC の書き出しの設定（`kAudioCodecBitRateControlMode` の値・ビット毎秒）。
pub(crate) fn aac_settings(s: &AacSettings) -> AacSettingsC {
    let mode = match s.mode {
        AacMode::Cbr => 0,
        AacMode::Abr => 1,
        AacMode::VbrConstrained => 2,
        AacMode::Vbr => 3,
    };
    AacSettingsC {
        rate: f64::from(s.rate),
        channels: u32::from(s.channels),
        channel_mask: s.channel_mask,
        mode,
        bitrate: s.bitrate_kbps.saturating_mul(1_000),
        quality: s.quality,
        reserved: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::IMG_CONVERTED;

    #[test]
    fn pixel_format_codes_round_trip() {
        for f in PixelFormat::ALL {
            let got = pixel_format(i32::from(f.code()));
            if f == PixelFormat::Indexed8 {
                assert_eq!(got, None);
            } else {
                assert_eq!(got, Some(f));
            }
        }
        assert_eq!(pixel_format(0), None);
        assert_eq!(pixel_format(-1), None);
        assert_eq!(alpha(1), Some(PlatformAlpha::Straight));
        assert_eq!(alpha(3), None);
    }

    #[test]
    fn image_info_maps_fields_and_rejects_broken_values() {
        let c = ImageInfoC {
            width: 10,
            height: 20,
            frames: 3,
            orientation: 6,
            pixel_format: 4,
            alpha: ALPHA_PREMULTIPLIED,
            loop_count: 2,
            flags: IMG_CONVERTED,
            icc_len: 536,
            exif_len: 0,
        };
        let info = image_info(&c).unwrap();
        assert_eq!(
            (
                info.width,
                info.height,
                info.format,
                info.alpha,
                info.orientation
            ),
            (10, 20, PixelFormat::Rgba8, PlatformAlpha::Premultiplied, 6)
        );
        assert_eq!(
            (info.frames, info.loop_count, info.icc_len, info.hdr),
            (3, 2, 536, false)
        );
        let hdr = image_info(&ImageInfoC {
            flags: IMG_HDR,
            pixel_format: 0,
            ..c
        })
        .unwrap();
        assert!(hdr.hdr);
        assert!(matches!(
            image_info(&ImageInfoC {
                pixel_format: 9,
                ..c
            }),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            image_info(&ImageInfoC { alpha: 7, ..c }),
            Err(BackendError::Invalid(_))
        ));
        assert!(matches!(
            image_info(&ImageInfoC { icc_len: -1, ..c }),
            Err(BackendError::Invalid(_))
        ));
        // 範囲の外の向きは adapter が 1 にする（ここでは u8 に収まらなければ 1）
        assert_eq!(
            image_info(&ImageInfoC {
                orientation: 300,
                ..c
            })
            .unwrap()
            .orientation,
            1
        );
    }

    #[test]
    fn audio_info_maps_priming_and_source() {
        let c = AudioInfoC {
            rate: 44_100.0,
            channels: 2,
            format_id: crate::codec::caps::AAC,
            frames: 44_100,
            priming: 2_112,
            remainder: 836,
            bits: 0,
            flags: 0,
        };
        let info = audio_info(&c).unwrap();
        assert_eq!((info.rate, info.channels, info.frames), (44_100, 2, 44_100));
        assert_eq!(
            info.priming,
            Priming::Trimmed {
                start: 2_112,
                end: 836
            }
        );
        assert_eq!(info.source, None);
        let none = audio_info(&AudioInfoC {
            priming: -1,
            remainder: -1,
            ..c
        })
        .unwrap();
        assert_eq!(none.priming, Priming::None);
        let pcm = audio_info(&AudioInfoC {
            bits: 24,
            flags: AUD_PCM,
            ..c
        })
        .unwrap();
        assert_eq!(
            pcm.source,
            Some(AudioSource {
                bits: 24,
                float: false,
                rate: 44_100
            })
        );
        assert!(matches!(
            audio_info(&AudioInfoC {
                rate: 44_100.5,
                ..c
            }),
            Err(BackendError::Unsupported(_))
        ));
        assert!(matches!(
            audio_info(&AudioInfoC { channels: 0, ..c }),
            Err(BackendError::Unsupported(_))
        ));
        assert!(matches!(
            audio_info(&AudioInfoC { frames: -1, ..c }),
            Err(BackendError::Failed(_))
        ));
    }

    #[test]
    fn aac_settings_use_audiotoolbox_values() {
        let s = AacSettings {
            rate: 48_000,
            channels: 6,
            channel_mask: 0x3F,
            mode: AacMode::VbrConstrained,
            bitrate_kbps: 256,
            quality: 127,
        };
        let c = aac_settings(&s);
        assert_eq!(
            (
                c.rate,
                c.channels,
                c.channel_mask,
                c.mode,
                c.bitrate,
                c.quality,
                c.reserved
            ),
            (48_000.0, 6, 0x3F, 2, 256_000, 127, 0)
        );
        for (mode, want) in [
            (AacMode::Cbr, 0),
            (AacMode::Abr, 1),
            (AacMode::VbrConstrained, 2),
            (AacMode::Vbr, 3),
        ] {
            assert_eq!(aac_settings(&AacSettings { mode, ..s }).mode, want);
        }
    }
}
