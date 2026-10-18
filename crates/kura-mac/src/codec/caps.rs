//! `kura_codec_caps`（Swift が調べた事実の JSON）を読み、何を登録するか・環境の記録・形式の ID への写しを決める。OS によらない
//! （JSON を与えて Linux でもテストする）。

// macOS 以外では使う側（bridge と backend）がない（Linux ではテストだけが使う）
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::collections::BTreeMap;

use kura_engine::codec::CodecEnv;
use serde::Deserialize;

/// FourCC を整数にする。
pub(crate) const fn fourcc(b: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*b)
}

/// AAC-LC（`aac `）。
pub(crate) const AAC: u32 = fourcc(b"aac ");
/// HE-AAC（`aach`）。
pub(crate) const AAC_HE: u32 = fourcc(b"aach");
/// HE-AAC v2（`aacp`）。
pub(crate) const AAC_HE_V2: u32 = fourcc(b"aacp");
/// Opus（`opus`）。
pub(crate) const OPUS: u32 = fourcc(b"opus");
/// 可逆の PCM（`lpcm`）。
pub(crate) const LPCM: u32 = fourcc(b"lpcm");
/// IMA 4:1 ADPCM（`ima4`）。
pub(crate) const IMA4: u32 = fourcc(b"ima4");
/// μ-law（`ulaw`）。
pub(crate) const ULAW: u32 = fourcc(b"ulaw");
/// A-law（`alaw`）。
pub(crate) const ALAW: u32 = fourcc(b"alaw");
/// WAV の Microsoft ADPCM（`ms\0\x02`）。
pub(crate) const MS_ADPCM: u32 = 0x6D73_0002;
/// WAV の IMA（DVI）ADPCM（`ms\0\x11`）。
pub(crate) const DVI_ADPCM: u32 = 0x6D73_0011;

/// AudioFile の入れ物（m4a）。
pub(crate) const FILE_M4A: u32 = fourcc(b"m4af");
/// AudioFile の入れ物（ADTS）。
pub(crate) const FILE_ADTS: u32 = fourcc(b"adts");
/// AudioFile の入れ物（Ogg。SDK のヘッダにない口）。
pub(crate) const FILE_OGG: u32 = fourcc(b"Oggf");
/// AudioFile の入れ物（WAV）。
pub(crate) const FILE_WAVE: u32 = fourcc(b"WAVE");
/// AudioFile の入れ物（AIFF・AIFC）。
pub(crate) const FILE_AIFF: u32 = fourcc(b"AIFF");
pub(crate) const FILE_AIFC: u32 = fourcc(b"AIFC");

/// ImageIO に回す形式と、その形式として受け入れる UTI（CGImageSourceGetType）。HEIC・HEIF は互いの UTI も受ける
/// （拡張子と中身の brand が食い違うファイルがあるため）。シーケンス（public.heics・public.avis）は Swift がフレームの列として
/// 読む。JPEG・PSD は固定の段が断った変種（CMYK の JPEG・ZIP の PSD など）だけが来る。
pub(crate) const IMAGE_ROUTES: &[(&str, &[&str])] = &[
    ("heic", &["public.heic", "public.heics", "public.heif"]),
    ("heif", &["public.heif", "public.heic", "public.heics"]),
    ("avif", &["public.avif", "public.avis"]),
    ("tiff", &["public.tiff"]),
    ("bmp", &["com.microsoft.bmp"]),
    ("jpeg", &["public.jpeg"]),
    ("psd", &["com.adobe.photoshop-image"]),
];

/// AudioToolbox に回す形式。
pub(crate) struct AudioRoute {
    pub format: &'static str,
    /// 読める入れ物（どれか 1 つが AudioFile の読める種類にあれば扱う）。
    pub file_types: &'static [u32],
    /// AudioFileOpenWithCallbacks の種類の手掛かり（ADTS だけ。ほかは 0）。
    pub hint: u32,
    /// 読む中身（AudioToolbox の Ogg Vorbis は先頭を 128 フレーム余分に切るので入れない。MP3・FLAC・ALAC は固定の段）。
    pub allowed: &'static [u32],
}

const AAC_FAMILY: &[u32] = &[AAC, AAC_HE, AAC_HE_V2];
const PCM_FAMILY: &[u32] = &[LPCM, IMA4, ULAW, ALAW, MS_ADPCM, DVI_ADPCM];

/// AudioToolbox に回す形式（m4a・ADTS の HE-AAC と多チャンネルの AAC、Ogg Opus、固定の段が断った WAV・AIFF の変種）。
pub(crate) const AUDIO_ROUTES: &[AudioRoute] = &[
    AudioRoute {
        format: "m4a",
        file_types: &[FILE_M4A],
        hint: 0,
        allowed: AAC_FAMILY,
    },
    AudioRoute {
        format: "aac",
        file_types: &[FILE_ADTS],
        hint: FILE_ADTS,
        allowed: AAC_FAMILY,
    },
    AudioRoute {
        format: "ogg",
        file_types: &[FILE_OGG],
        hint: 0,
        allowed: &[OPUS],
    },
    AudioRoute {
        format: "opus",
        file_types: &[FILE_OGG],
        hint: 0,
        allowed: &[OPUS],
    },
    AudioRoute {
        format: "wav",
        file_types: &[FILE_WAVE],
        hint: 0,
        allowed: PCM_FAMILY,
    },
    AudioRoute {
        format: "aiff",
        file_types: &[FILE_AIFF, FILE_AIFC],
        hint: 0,
        allowed: PCM_FAMILY,
    },
];

/// 形式の ID から AudioToolbox の経路を引く。
pub(crate) fn audio_route(format: &str) -> Option<&'static AudioRoute> {
    AUDIO_ROUTES.iter().find(|r| r.format == format)
}

/// 形式の ID から ImageIO の UTI を引く。
pub(crate) fn image_utis(format: &str) -> Option<&'static [&'static str]> {
    IMAGE_ROUTES
        .iter()
        .find(|(f, _)| *f == format)
        .map(|(_, u)| *u)
}

/// AAC-LC の 1 つのチャンネルの数で選べるビットレート。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AacChannels {
    pub channels: u32,
    /// ビット毎秒（小さい順）。
    pub bitrates: Vec<u32>,
}

/// AAC-LC の 1 つのレート。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AacRate {
    pub rate: u32,
    pub channels: Vec<AacChannels>,
}

/// macOS の能力と環境（`kura_codec_caps`。起動時に 1 度だけ調べる）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MacCaps {
    /// Swift の ABI の版。
    pub abi: u32,
    /// OS の版（例 `26.7`）。
    pub os: String,
    /// OS の build（例 `25G229`）。
    pub build: String,
    /// フレームワークの CFBundleVersion（`imageio`・`coregraphics`・`audiotoolbox`）。
    #[serde(default)]
    pub frameworks: BTreeMap<String, String>,
    /// ImageIO が読める UTI。
    #[serde(default)]
    pub image_read: Vec<String>,
    /// AudioFile が読める入れ物の FourCC。
    #[serde(default)]
    pub audio_file_read: Vec<u32>,
    /// AudioToolbox のデコーダの FourCC。
    #[serde(default)]
    pub audio_decode: Vec<u32>,
    /// AudioToolbox のエンコーダの FourCC。
    #[serde(default)]
    pub audio_encode: Vec<u32>,
    /// AAC-LC で書けるレートとビットレート。
    #[serde(default)]
    pub aac_lc: Vec<AacRate>,
}

impl MacCaps {
    /// JSON を読む。
    pub fn parse(json: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(json).map_err(|e| format!("能力の JSON を読めません: {e}"))
    }

    /// OS の版の表示（例 `26.7 (25G229)`）。
    pub fn os_label(&self) -> String {
        if self.build.is_empty() {
            self.os.clone()
        } else {
            format!("{} ({})", self.os, self.build)
        }
    }

    fn env_with(&self, keys: &[&str]) -> CodecEnv {
        let mut env = CodecEnv::new("macOS", self.os_label()).with("arch", std::env::consts::ARCH);
        for key in keys {
            if let Some(v) = self.frameworks.get(*key) {
                env = env.with(*key, v.clone());
            }
        }
        env
    }

    /// 画像の読み取りの環境の記録（OS・ImageIO・CoreGraphics の版）。
    pub fn image_env(&self) -> CodecEnv {
        self.env_with(&["imageio", "coregraphics"])
    }

    /// 音声の読み書きの環境の記録（OS・AudioToolbox の版）。
    pub fn audio_env(&self) -> CodecEnv {
        self.env_with(&["audiotoolbox"])
    }

    /// ImageIO がこの形式を読めるか（経路の UTI のどれかを読める）。
    pub fn image_readable(&self, format: &str) -> bool {
        image_utis(format)
            .is_some_and(|utis| utis.iter().any(|u| self.image_read.iter().any(|r| r == u)))
    }

    /// AudioToolbox がこの形式を読めるか（入れ物を読め、許可する中身のどれかのデコーダがある。PCM は常に読める）。
    pub fn audio_readable(&self, format: &str) -> bool {
        audio_route(format).is_some_and(|r| {
            r.file_types
                .iter()
                .any(|t| self.audio_file_read.contains(t))
                && r.allowed
                    .iter()
                    .any(|c| *c == LPCM || self.audio_decode.contains(c))
        })
    }

    /// AAC-LC をこのレート・チャンネルの数で書けるか。
    pub fn aac_supports(&self, rate: u32, channels: u16) -> bool {
        self.audio_encode.contains(&AAC) && !self.aac_bitrates_kbps(rate, channels).is_empty()
    }

    /// AAC-LC を、`max_channels` 以下のどれかのレート・チャンネルの数で書けるか（AAC のエンコーダがあり、レートの表に
    /// 選べるビットレートがある）。偽なら AAC の書き手を登録しない（能力の表が m4a を書けると偽らない）。
    pub fn aac_writable(&self, max_channels: u16) -> bool {
        self.aac_lc.iter().any(|r| {
            r.channels.iter().any(|c| {
                u16::try_from(c.channels)
                    .is_ok_and(|ch| ch <= max_channels && self.aac_supports(r.rate, ch))
            })
        })
    }

    /// AAC-LC のこのレート・チャンネルの数で選べるビットレート（kbps。1,000 で割り切れるものだけ、小さい順）。
    pub fn aac_bitrates_kbps(&self, rate: u32, channels: u16) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .aac_lc
            .iter()
            .filter(|r| r.rate == rate)
            .flat_map(|r| r.channels.iter())
            .filter(|c| c.channels == u32::from(channels))
            .flat_map(|c| c.bitrates.iter())
            .filter(|b| **b > 0 && **b % 1_000 == 0)
            .map(|b| b / 1_000)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// AAC-LC を書けるレート（小さい順）。
    pub fn aac_rates(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self.aac_lc.iter().map(|r| r.rate).collect();
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MacCaps {
        MacCaps::parse(
            br#"{"abi":1,"os":"26.7","build":"25G229",
            "frameworks":{"imageio":"2784.6.4","coregraphics":"1965.6.3","audiotoolbox":"1.14"},
            "imageRead":["public.heic","public.avif","public.tiff","public.jpeg"],
            "audioFileRead":[1832149350,1332176742,1463899717],
            "audioDecode":[1633772320,1633772392,1869641075],
            "audioEncode":[1633772320],
            "aacLc":[{"rate":48000,"channels":[{"channels":2,"bitrates":[64000,256000,320000,12345]}]}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn fourcc_constants() {
        assert_eq!(AAC, 0x6161_6320);
        assert_eq!(FILE_M4A, 1_832_149_350);
        assert_eq!(FILE_OGG, 1_332_176_742);
        assert_eq!(FILE_WAVE, 1_463_899_717);
        assert_eq!(OPUS, 1_869_641_075);
        assert_eq!(MS_ADPCM.to_be_bytes(), [b'm', b's', 0, 2]);
    }

    /// 環境の記録：OS の版と build、アーキテクチャ、部品の版（画像と音声で別）。
    #[test]
    fn envs_carry_os_and_framework_versions() {
        let caps = sample();
        let img = caps.image_env();
        assert_eq!(
            (img.tool.as_str(), img.version.as_str()),
            ("macOS", "26.7 (25G229)")
        );
        assert_eq!(
            img.components.get("imageio").map(String::as_str),
            Some("2784.6.4")
        );
        assert_eq!(
            img.components.get("arch").map(String::as_str),
            Some(std::env::consts::ARCH)
        );
        assert!(!img.components.contains_key("audiotoolbox"));
        let aud = caps.audio_env();
        assert_eq!(
            aud.components.get("audiotoolbox").map(String::as_str),
            Some("1.14")
        );
        assert!(!aud.components.contains_key("imageio"));
    }

    /// 読める形式は、経路の UTI・入れ物・中身のデコーダが能力にあるものだけ。
    #[test]
    fn readable_formats_follow_the_caps() {
        let caps = sample();
        for f in ["heic", "heif", "avif", "tiff", "jpeg"] {
            assert!(caps.image_readable(f), "{f}");
        }
        for f in ["bmp", "psd", "png", "webp", "nope"] {
            assert!(!caps.image_readable(f), "{f}");
        }
        for f in ["m4a", "ogg", "opus", "wav"] {
            assert!(caps.audio_readable(f), "{f}");
        }
        // ADTS・AIFF の入れ物がない、MP3 は経路がない
        for f in ["aac", "aiff", "mp3", "flac"] {
            assert!(!caps.audio_readable(f), "{f}");
        }
        // Vorbis はどの経路でも許可しない
        assert!(
            AUDIO_ROUTES
                .iter()
                .all(|r| !r.allowed.contains(&fourcc(b"vorb")))
        );
    }

    /// AAC のビットレートは kbps（1,000 で割り切れないものは落とす）。表にないレート・チャンネルは書けない。
    #[test]
    fn aac_table() {
        let caps = sample();
        assert_eq!(caps.aac_bitrates_kbps(48_000, 2), [64, 256, 320]);
        assert!(caps.aac_supports(48_000, 2));
        assert!(!caps.aac_supports(48_000, 1));
        assert!(!caps.aac_supports(96_000, 2));
        assert_eq!(caps.aac_rates(), [48_000]);
        let mut no_encoder = caps.clone();
        no_encoder.audio_encode.clear();
        assert!(!no_encoder.aac_supports(48_000, 2));
    }

    /// AAC の書き手を登録するのは、エンコーダがあり、上限以下のチャンネルの数で選べるビットレートがあるときだけ。
    #[test]
    fn aac_writable_needs_the_encoder_and_the_table() {
        let caps = sample();
        assert!(caps.aac_writable(6));
        assert!(caps.aac_writable(2));
        assert!(!caps.aac_writable(1));
        let mut no_encoder = caps.clone();
        no_encoder.audio_encode.clear();
        assert!(!no_encoder.aac_writable(6));
        // Swift の問い合わせが失敗すると表は空
        let mut no_table = caps.clone();
        no_table.aac_lc.clear();
        assert!(!no_table.aac_writable(6));
        // kbps で選べないビットレートだけ
        let mut odd = caps.clone();
        odd.aac_lc[0].channels[0].bitrates = vec![12_345, 0];
        assert!(!odd.aac_writable(6));
    }

    /// 経路のすべての形式は、エンジンの形式の表にあり、画像の経路は画像に、音声の経路は音声に読む形式（表にない形式の経路を
    /// 足すと、デコーダは登録されても能力の表にも拡張子の判定にも出ず、黙って使われない）。経路は重ならない。
    #[test]
    fn routes_are_in_the_format_table() {
        use kura_engine::ValueKind;
        use kura_engine::codec::format_by_id;
        for (format, utis) in IMAGE_ROUTES {
            let f =
                format_by_id(format).unwrap_or_else(|| panic!("{format} が形式の表にありません"));
            assert!(f.decodes_to.contains(&ValueKind::Image), "{format}");
            assert!(!utis.is_empty(), "{format}");
        }
        for r in AUDIO_ROUTES {
            let f = format_by_id(r.format)
                .unwrap_or_else(|| panic!("{} が形式の表にありません", r.format));
            assert_eq!(f.decodes_to, [ValueKind::Audio], "{}", r.format);
            assert!(
                !r.file_types.is_empty() && !r.allowed.is_empty(),
                "{}",
                r.format
            );
        }
        let mut ids: Vec<&str> = IMAGE_ROUTES
            .iter()
            .map(|(f, _)| *f)
            .chain(AUDIO_ROUTES.iter().map(|r| r.format))
            .collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "経路が重なっています");
    }

    /// 知らない項目は断る（Swift と Rust の食い違いに気づくため）。
    #[test]
    fn unknown_fields_are_rejected() {
        assert!(MacCaps::parse(br#"{"abi":1,"os":"26","build":"x","extra":1}"#).is_err());
        assert!(MacCaps::parse(b"not json").is_err());
    }
}
