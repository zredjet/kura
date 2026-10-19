//! 事実の投影とドット絵の判定 v1（純粋な関数。DB に触らない）。
//!
//! - 事実（`media_facts` の列）は、解析の結果（`analysis_results` の正規の形）と読み取りの記録（[`ReadRecord`]）から
//!   [`project`] で作る。投影の版 [`FACTS_V`] を上げても、読み直さずに結果の表から作り直せる。
//! - ドット絵の自動の判定 v1（決定 7。[`pixel_art_v1`]、版 [`PIXEL_RULE`]）：aseprite は常にはい、svg・jpeg は常にいいえ。
//!   ほかは「等倍：見える色が 64 色以下・長辺 512 以下・半透明なし」か「拡大：可逆で 2≤k≤64・元の大きさ 4px 以上」ならはい。
//!   表示と検索の実効値は「設定 ?? 自動」（`asset_settings.pixel_art` ?? `media_facts.pixel_art`）。
//! - JPEG・HEIC・HEIF は読まずに形式から書く（[`from_format`]。透過なし・色数は計らない・ドット絵でない。決定 5）。

use std::collections::BTreeMap;

use kura_engine::Value;
use kura_engine::analysis::audio::loop_points::{LoopAnalysis, LoopReport};
use kura_engine::analysis::audio::loudness::{LoudnessAnalysis, LoudnessReport, Unmeasured};
use kura_engine::analysis::audio::silence::{SilenceAnalysis, SilenceReport};
use kura_engine::analysis::image::alpha::{Alpha, AlphaKind, AlphaOutput};
use kura_engine::analysis::image::colors::{Colors, ColorsOutput};
use kura_engine::analysis::image::pixel_scale::{PixelScale, PixelScaleOutput};
use kura_engine::analysis::output::json_from_canonical;
use kura_engine::analysis::{Analysis, Reasoned};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// 投影の版（列の作り方を変えたら上げる。上がった行は、読み直さずに結果の表から作り直す）。
pub const FACTS_V: i64 = 1;
/// ドット絵の判定の版（[`pixel_art_v1`] の規則と定数）。
pub const PIXEL_RULE: i64 = 1;
/// 等倍のドット絵の見える色の数の上限。
pub const NATIVE_MAX_COLORS: u16 = 64;
/// 等倍のドット絵の長辺の上限（画素）。
pub const NATIVE_MAX_SIDE: u64 = 512;
/// 拡大したドット絵の倍率の範囲。
pub const SCALE_MIN: u32 = 2;
pub const SCALE_MAX: u32 = 64;
/// 拡大したドット絵の元の大きさ（各辺 / 倍率）の下限（画素）。
pub const SCALED_MIN_SOURCE: u64 = 4;
/// 原の深さを数える（非可逆でない）音声の形式。ほかは `bits` を書かない。
const LOSSLESS_AUDIO: [&str; 3] = ["wav", "aiff", "flac"];

/// 読み取りの記録（kura-core の結果 `kura.read` の正規の形）：値の種類と大きさ・長さ。デコードした値から作る。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "value", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReadRecord {
    /// 1 枚の画像。
    Image { w: u64, h: u64 },
    /// フレームの列（キャンバスの大きさとフレームの数）。
    Frames { w: u64, h: u64, count: u64 },
    /// 音声（`bits` は元の形式の標本の深さ。非可逆の形式・分からなければ null）。
    Audio {
        rate: u32,
        channels: u16,
        frames: u64,
        bits: Option<u16>,
    },
}

impl ReadRecord {
    /// 読んだ値から作る（`format` は形式の表の ID。深さを書くかどうかに使う）。
    pub fn from_value(v: &Value, format: &str) -> Self {
        match v {
            Value::Image(img) => Self::Image {
                w: u64::from(img.px.width()),
                h: u64::from(img.px.height()),
            },
            Value::Frames(fr) => {
                let canvas = fr.canvas();
                Self::Frames {
                    w: u64::from(canvas.w),
                    h: u64::from(canvas.h),
                    count: fr.frames().len() as u64,
                }
            }
            Value::Audio(a) => Self::Audio {
                rate: a.rate(),
                channels: u16::try_from(a.channels()).unwrap_or(u16::MAX),
                frames: a.frames() as u64,
                bits: a
                    .meta
                    .source
                    .filter(|_| LOSSLESS_AUDIO.contains(&format))
                    .map(|s| s.bits),
            },
        }
    }

    /// 画像の大きさ（キャンバス）。音声は None。
    fn size(&self) -> Option<(u64, u64)> {
        match *self {
            Self::Image { w, h } | Self::Frames { w, h, .. } => Some((w, h)),
            Self::Audio { .. } => None,
        }
    }

    /// 正規の形（結果の表に置くバイト列）。
    pub fn to_canonical(&self) -> crate::Result<Vec<u8>> {
        kura_engine::analysis::output::json_to_canonical(self)
            .map_err(|e| crate::CoreError::Invalid(format!("読み取りの記録を書けません: {e}")))
    }

    /// 正規の形から読む（読めなければ None）。
    pub fn from_canonical(bytes: &[u8]) -> Option<Self> {
        json_from_canonical(bytes).ok()
    }
}

/// 事実（`media_facts` の、解析から決まる列。読み取りの記録の列（decoder など）は別に持つ）。None は NULL（計っていない・
/// 計れない）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaFacts {
    pub facts_v: i64,
    /// 0 なし・1 二値・2 半透明あり。
    pub alpha: Option<i64>,
    /// 1〜257（257 は 256 を超える）。
    pub colors: Option<i64>,
    pub visible_colors: Option<i64>,
    pub pixel_kx: Option<i64>,
    pub pixel_ky: Option<i64>,
    pub pixel_lossless: Option<bool>,
    /// 自動の判定（[`pixel_art_v1`]）。材料が足りなければ None。
    pub pixel_art: Option<bool>,
    pub pixel_rule: Option<i64>,
    pub frame_count: Option<i64>,
    /// 積分ラウドネス（LUFS）。計れなければ None で `loudness_reason`。
    pub loudness: Option<f64>,
    pub loudness_reason: Option<String>,
    pub loudness_range: Option<f64>,
    pub true_peak: Option<f64>,
    pub sample_peak: Option<f64>,
    pub has_loop: Option<bool>,
    pub loop_start: Option<i64>,
    pub loop_end: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub bits: Option<i64>,
    pub clip_runs: Option<i64>,
}

/// ドット絵の判定の材料。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelArtInput<'a> {
    /// 形式の表の ID。
    pub format: &'a str,
    /// キャンバスの大きさ（画素）。
    pub size: Option<(u64, u64)>,
    pub alpha: Option<&'a AlphaOutput>,
    pub colors: Option<&'a ColorsOutput>,
    pub scale: Option<&'a PixelScaleOutput>,
}

fn in_scale(k: u32) -> bool {
    (SCALE_MIN..=SCALE_MAX).contains(&k)
}

/// ドット絵の自動の判定 v1（決定 7。版 [`PIXEL_RULE`]）。材料が足りず決まらなければ None。
///
/// - svg・jpeg はいいえ。aseprite ははい。
/// - 拡大：倍率が可逆で、両方の軸が 2≤k≤64、元の大きさ（各辺 / k）が 4px 以上ならはい。
/// - 等倍：見える色が 64 色以下・半透明の画素なし・長辺 512 以下ならはい。
/// - それ以外はいいえ。
pub fn pixel_art_v1(input: &PixelArtInput<'_>) -> Option<bool> {
    match input.format {
        "svg" | "jpeg" => return Some(false),
        "aseprite" => return Some(true),
        _ => {}
    }
    let (w, h) = input.size?;
    let scaled = input.scale.map(|s| match (s.x, s.y) {
        (Some(x), Some(y)) => {
            s.lossless
                && in_scale(x.k)
                && in_scale(y.k)
                && w / u64::from(x.k) >= SCALED_MIN_SOURCE
                && h / u64::from(y.k) >= SCALED_MIN_SOURCE
        }
        _ => false,
    });
    let native = match (input.colors, input.alpha) {
        (Some(c), Some(a)) => {
            Some(c.visible <= NATIVE_MAX_COLORS && a.partial == 0 && w.max(h) <= NATIVE_MAX_SIDE)
        }
        _ => None,
    };
    match (scaled, native) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// 結果の正規の形（JSON）を型に読む（ない・読めなければ None）。
fn output<T: DeserializeOwned>(results: &BTreeMap<String, Vec<u8>>, id: &str) -> Option<T> {
    results.get(id).and_then(|b| json_from_canonical(b).ok())
}

fn alpha_code(kind: AlphaKind) -> i64 {
    match kind {
        AlphaKind::None => 0,
        AlphaKind::Binary => 1,
        AlphaKind::Partial => 2,
    }
}

fn reason_name(r: Unmeasured) -> &'static str {
    match r {
        Unmeasured::TooShort => "tooShort",
        Unmeasured::Silent => "silent",
        Unmeasured::BelowGate => "belowGate",
        Unmeasured::LowRate => "lowRate",
        Unmeasured::NonFinite => "nonFinite",
    }
}

fn level(l: &Reasoned<f64, Unmeasured>) -> Option<f64> {
    l.value().copied().filter(|v| v.is_finite())
}

fn as_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// 事実を作る：読み取りの記録と解析の結果（ID → 正規の形。JSON のもの）から。結果のない列は None。
pub fn project(
    format: &str,
    read: Option<&ReadRecord>,
    results: &BTreeMap<String, Vec<u8>>,
) -> MediaFacts {
    let mut f = MediaFacts {
        facts_v: FACTS_V,
        ..MediaFacts::default()
    };
    let alpha: Option<AlphaOutput> = output(results, Alpha::ID);
    let colors: Option<ColorsOutput> = output(results, Colors::ID);
    let scale: Option<PixelScaleOutput> = output(results, PixelScale::ID);
    match read {
        Some(ReadRecord::Image { .. }) => f.frame_count = Some(1),
        Some(ReadRecord::Frames { count, .. }) => f.frame_count = Some(as_i64(*count)),
        Some(ReadRecord::Audio {
            rate,
            channels,
            bits,
            ..
        }) => {
            f.sample_rate = Some(i64::from(*rate));
            f.channels = Some(i64::from(*channels));
            f.bits = bits.map(i64::from);
        }
        None => {}
    }
    let is_image = matches!(
        read,
        Some(ReadRecord::Image { .. } | ReadRecord::Frames { .. })
    );
    if is_image {
        f.alpha = alpha.as_ref().map(|a| alpha_code(a.kind));
        f.colors = colors.as_ref().map(|c| i64::from(c.count));
        f.visible_colors = colors.as_ref().map(|c| i64::from(c.visible));
        if let Some(s) = &scale {
            f.pixel_kx = s.x.map(|a| i64::from(a.k));
            f.pixel_ky = s.y.map(|a| i64::from(a.k));
            f.pixel_lossless = Some(s.lossless);
        }
        f.pixel_art = pixel_art_v1(&PixelArtInput {
            format,
            size: read.and_then(ReadRecord::size),
            alpha: alpha.as_ref(),
            colors: colors.as_ref(),
            scale: scale.as_ref(),
        });
        f.pixel_rule = Some(PIXEL_RULE);
    }
    if let Some(l) = output::<LoudnessReport>(results, LoudnessAnalysis::ID) {
        f.loudness = level(&l.integrated);
        f.loudness_reason = l.integrated.reason().map(|r| reason_name(*r).to_string());
        f.loudness_range = level(&l.range);
        f.true_peak = level(&l.true_peak);
        f.sample_peak = level(&l.sample_peak);
    }
    if let Some(lp) = output::<LoopReport>(results, LoopAnalysis::ID) {
        f.has_loop = Some(lp.range.is_some());
        f.loop_start = lp.range.map(|r| as_i64(r.start));
        f.loop_end = lp.range.map(|r| as_i64(r.end));
    }
    if let Some(s) = output::<SilenceReport>(results, SilenceAnalysis::ID) {
        f.clip_runs = Some(as_i64(s.clip_runs));
    }
    f
}

/// 読まずに形式から書く事実（JPEG・HEIC・HEIF。決定 5）：透過なし・色数は計らない・ドット絵でない。
pub fn from_format(format: &str) -> MediaFacts {
    MediaFacts {
        facts_v: FACTS_V,
        alpha: Some(0),
        pixel_art: pixel_art_v1(&PixelArtInput {
            format,
            size: None,
            alpha: None,
            colors: None,
            scale: None,
        })
        .or(Some(false)),
        pixel_rule: Some(PIXEL_RULE),
        ..MediaFacts::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::analysis::audio::FrameSpan;
    use kura_engine::analysis::image::pixel_scale::AxisScale;
    use kura_engine::analysis::output::json_to_canonical;

    fn alpha(kind: AlphaKind, partial: u64) -> AlphaOutput {
        AlphaOutput {
            kind,
            transparent: 1,
            partial,
            opaque: 1,
            uncovered: 0,
        }
    }

    fn colors(count: u16, visible: u16) -> ColorsOutput {
        ColorsOutput {
            count,
            colors: None,
            visible,
            palette_lossless: count <= 256,
            palette: None,
        }
    }

    fn scale(kx: u32, ky: u32, lossless: bool) -> PixelScaleOutput {
        let axis = |k| {
            Some(AxisScale {
                k,
                phase: 0,
                lines: 8,
            })
        };
        PixelScaleOutput {
            x: axis(kx),
            y: axis(ky),
            lossless,
        }
    }

    /// 16 の見本（解析の担当が M11 の設計で作った見本の事実。decisions の決定 7）の判定。誤り 2 つ（31 色に減らした写真の
    /// GIF を「はい」、2048² の 16 色のスプライトを「いいえ」）も v1 の振る舞いとして凍結する。
    #[test]
    fn pixel_art_v1_on_sixteen_samples() {
        use AlphaKind::{Binary, None as Opaque, Partial};
        struct Sample {
            name: &'static str,
            format: &'static str,
            size: (u64, u64),
            alpha: AlphaOutput,
            colors: ColorsOutput,
            scale: PixelScaleOutput,
            want: bool,
        }
        let s = |name, format, size, alpha, colors, scale, want| Sample {
            name,
            format,
            size,
            alpha,
            colors,
            scale,
            want,
        };
        let samples = [
            s(
                "pixel64（等倍・16 色・二値の透過）",
                "png",
                (64, 64),
                alpha(Binary, 0),
                colors(16, 16),
                scale(1, 1, true),
                true,
            ),
            s(
                "pixel64x4（最近傍で 4 倍）",
                "png",
                (256, 256),
                alpha(Binary, 0),
                colors(16, 16),
                scale(4, 4, true),
                true,
            ),
            s(
                "pixel64x8（最近傍で 8 倍）",
                "png",
                (512, 512),
                alpha(Binary, 0),
                colors(16, 16),
                scale(8, 8, true),
                true,
            ),
            s(
                "pixel64_3x2（横 3・縦 2 倍）",
                "png",
                (192, 128),
                alpha(Binary, 0),
                colors(16, 16),
                scale(3, 2, true),
                true,
            ),
            s(
                "pixel64x4_q92（JPEG にした 4 倍）",
                "jpeg",
                (256, 256),
                alpha(Opaque, 0),
                colors(257, 257),
                scale(1, 1, false),
                false,
            ),
            s(
                "pixel64_smooth256（平滑に 4 倍）",
                "png",
                (256, 256),
                alpha(Partial, 900),
                colors(257, 257),
                scale(1, 1, false),
                false,
            ),
            s(
                "photo_dither（ディザした 255 色の GIF）",
                "gif",
                (480, 320),
                alpha(Opaque, 0),
                colors(255, 255),
                scale(1, 1, true),
                false,
            ),
            s(
                "photo_32c（31 色に減らした写真の GIF。誤検出）",
                "gif",
                (480, 320),
                alpha(Opaque, 0),
                colors(31, 31),
                scale(1, 1, true),
                true,
            ),
            s(
                "photo24（写真）",
                "jpeg",
                (6000, 4000),
                alpha(Opaque, 0),
                colors(257, 257),
                scale(1, 1, false),
                false,
            ),
            s(
                "photo_small（写真の PNG）",
                "png",
                (640, 480),
                alpha(Opaque, 0),
                colors(257, 257),
                scale(1, 1, false),
                false,
            ),
            s(
                "icon_smooth（64² の写真）",
                "png",
                (64, 64),
                alpha(Opaque, 0),
                colors(257, 257),
                scale(1, 1, false),
                false,
            ),
            s(
                "icon.svg（半透明の円）",
                "svg",
                (512, 512),
                alpha(Partial, 5000),
                colors(257, 257),
                scale(1, 1, false),
                false,
            ),
            s(
                "art2048（2048² の壁紙）",
                "png",
                (2048, 2048),
                alpha(Opaque, 0),
                colors(257, 257),
                scale(1, 1, false),
                false,
            ),
            s(
                "art2048.psd",
                "psd",
                (2048, 2048),
                alpha(Opaque, 0),
                colors(257, 257),
                scale(1, 1, false),
                false,
            ),
            s(
                "pixel2048_idx（2048² の 16 色。取りこぼし）",
                "png",
                (2048, 2048),
                alpha(Binary, 0),
                colors(16, 16),
                scale(1, 1, true),
                false,
            ),
            s(
                "anim128.aseprite",
                "aseprite",
                (128, 128),
                alpha(Binary, 0),
                colors(300, 257),
                scale(1, 1, false),
                true,
            ),
        ];
        assert_eq!(samples.len(), 16);
        for x in &samples {
            let got = pixel_art_v1(&PixelArtInput {
                format: x.format,
                size: Some(x.size),
                alpha: Some(&x.alpha),
                colors: Some(&x.colors),
                scale: Some(&x.scale),
            });
            assert_eq!(got, Some(x.want), "{}", x.name);
        }
    }

    /// 境目：倍率 2 と 64、元の大きさ 4px、64 色・長辺 512 はちょうどで「はい」。材料が足りなければ決めない。
    #[test]
    fn pixel_art_v1_edges() {
        let judge = |size, a: &AlphaOutput, c: &ColorsOutput, sc: &PixelScaleOutput| {
            pixel_art_v1(&PixelArtInput {
                format: "png",
                size: Some(size),
                alpha: Some(a),
                colors: Some(c),
                scale: Some(sc),
            })
        };
        let opaque = alpha(AlphaKind::None, 0);
        let photo = colors(257, 257);
        assert_eq!(
            judge((8, 8), &opaque, &photo, &scale(2, 2, true)),
            Some(true)
        );
        assert_eq!(
            judge((7, 8), &opaque, &photo, &scale(2, 2, true)),
            Some(false),
            "元が 3px"
        );
        assert_eq!(
            judge((4096, 4096), &opaque, &photo, &scale(64, 64, true)),
            Some(true)
        );
        assert_eq!(
            judge((4160, 4160), &opaque, &photo, &scale(65, 65, true)),
            Some(false)
        );
        assert_eq!(
            judge((400, 400), &opaque, &photo, &scale(4, 4, false)),
            Some(false),
            "可逆でない"
        );
        assert_eq!(
            judge((400, 400), &opaque, &photo, &scale(4, 1, true)),
            Some(false)
        );
        let few = colors(70, 64);
        assert_eq!(
            judge((512, 100), &opaque, &few, &scale(1, 1, false)),
            Some(true)
        );
        assert_eq!(
            judge((513, 100), &opaque, &few, &scale(1, 1, false)),
            Some(false)
        );
        assert_eq!(
            judge((64, 64), &opaque, &colors(65, 65), &scale(1, 1, true)),
            Some(false)
        );
        assert_eq!(
            judge(
                (64, 64),
                &alpha(AlphaKind::Partial, 1),
                &few,
                &scale(1, 1, true)
            ),
            Some(false)
        );
        let null_axes = PixelScaleOutput {
            x: None,
            y: None,
            lossless: true,
        };
        assert_eq!(
            judge((16, 16), &opaque, &colors(1, 1), &null_axes),
            Some(true),
            "1 色の小さな絵"
        );
        assert_eq!(
            pixel_art_v1(&PixelArtInput {
                format: "png",
                size: Some((64, 64)),
                alpha: None,
                colors: None,
                scale: Some(&scale(1, 1, true)),
            }),
            None,
            "色数・透過がなければ決めない"
        );
        assert_eq!(
            pixel_art_v1(&PixelArtInput {
                format: "png",
                size: Some((64, 64)),
                alpha: None,
                colors: None,
                scale: Some(&scale(4, 4, true)),
            }),
            Some(true),
            "拡大で決まれば材料が欠けていても決める"
        );
        for (format, want) in [("svg", false), ("jpeg", false), ("aseprite", true)] {
            let got = pixel_art_v1(&PixelArtInput {
                format,
                size: None,
                alpha: None,
                colors: None,
                scale: None,
            });
            assert_eq!(got, Some(want), "{format}");
        }
    }

    fn results(pairs: &[(&str, Vec<u8>)]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// 投影は facts_v を記録し、結果の正規の形から列を作る（画像）。
    #[test]
    fn project_records_facts_v_for_images() {
        let read = ReadRecord::Frames {
            w: 64,
            h: 64,
            count: 8,
        };
        let r = results(&[
            (
                Alpha::ID,
                json_to_canonical(&alpha(AlphaKind::Binary, 0)).unwrap(),
            ),
            (Colors::ID, json_to_canonical(&colors(17, 16)).unwrap()),
            (
                PixelScale::ID,
                json_to_canonical(&scale(1, 1, true)).unwrap(),
            ),
        ]);
        let f = project("gif", Some(&read), &r);
        assert_eq!(f.facts_v, FACTS_V);
        assert_eq!(
            (f.alpha, f.colors, f.visible_colors, f.frame_count),
            (Some(1), Some(17), Some(16), Some(8))
        );
        assert_eq!(
            (f.pixel_kx, f.pixel_ky, f.pixel_lossless),
            (Some(1), Some(1), Some(true))
        );
        assert_eq!((f.pixel_art, f.pixel_rule), (Some(true), Some(PIXEL_RULE)));
        assert_eq!((f.loudness, f.has_loop, f.sample_rate), (None, None, None));

        // 結果がなければ計っていない（NULL）。ドット絵は決めない
        let f = project(
            "png",
            Some(&ReadRecord::Image { w: 4, h: 4 }),
            &BTreeMap::new(),
        );
        assert_eq!((f.facts_v, f.alpha, f.pixel_art), (FACTS_V, None, None));
        // 読めない結果の行は無視する
        let f = project("png", Some(&read), &results(&[(Alpha::ID, b"{".to_vec())]));
        assert_eq!(f.alpha, None);
    }

    /// 音声：ラウドネスは計れなければ NULL と理由、ループ点と深さ（可逆の形式だけ）。
    #[test]
    fn project_records_audio_facts() {
        let read = ReadRecord::Audio {
            rate: 48_000,
            channels: 2,
            frames: 48_000,
            bits: Some(24),
        };
        let loudness = LoudnessReport {
            integrated: Reasoned::Value(-14.25),
            range: Reasoned::Reason(Unmeasured::TooShort),
            sample_peak: Reasoned::Value(-1.0),
            true_peak: Reasoned::Value(-0.5),
            momentary_max: Reasoned::Value(-10.0),
            short_term_max: Reasoned::Reason(Unmeasured::TooShort),
        };
        let lp = LoopReport {
            frames: 48_000,
            rate: 48_000,
            range: Some(FrameSpan {
                start: 100,
                end: 47_000,
            }),
            dropped_loop: false,
            markers: vec![],
            marker_count: 0,
            dropped_markers: 0,
        };
        let silence = SilenceReport {
            frames: 48_000,
            content: None,
            lead: 0,
            trail: 0,
            clip_runs: 3,
            overs: 0,
            non_finite: 0,
        };
        let r = results(&[
            (LoudnessAnalysis::ID, json_to_canonical(&loudness).unwrap()),
            (LoopAnalysis::ID, json_to_canonical(&lp).unwrap()),
            (SilenceAnalysis::ID, json_to_canonical(&silence).unwrap()),
        ]);
        let f = project("wav", Some(&read), &r);
        assert_eq!(f.facts_v, FACTS_V);
        assert_eq!(
            (f.loudness, f.loudness_reason.as_deref()),
            (Some(-14.25), None)
        );
        assert_eq!(
            (f.loudness_range, f.true_peak, f.sample_peak),
            (None, Some(-0.5), Some(-1.0))
        );
        assert_eq!(
            (f.has_loop, f.loop_start, f.loop_end),
            (Some(true), Some(100), Some(47_000))
        );
        assert_eq!(
            (f.sample_rate, f.channels, f.bits, f.clip_runs),
            (Some(48_000), Some(2), Some(24), Some(3))
        );
        assert_eq!((f.alpha, f.pixel_art, f.pixel_rule), (None, None, None));

        let silent = LoudnessReport {
            integrated: Reasoned::Reason(Unmeasured::Silent),
            ..loudness
        };
        let r = results(&[(LoudnessAnalysis::ID, json_to_canonical(&silent).unwrap())]);
        let f = project("wav", Some(&read), &r);
        assert_eq!(
            (f.loudness, f.loudness_reason.as_deref()),
            (None, Some("silent"))
        );
        assert_eq!(f.has_loop, None, "ループの結果がなければ計っていない");
    }

    /// JPEG・HEIC は読まずに形式から：透過なし・色数は計らない・ドット絵でない。
    #[test]
    fn facts_from_format() {
        for format in ["jpeg", "heic", "heif"] {
            let f = from_format(format);
            assert_eq!(f.facts_v, FACTS_V);
            assert_eq!(
                (f.alpha, f.colors, f.visible_colors),
                (Some(0), None, None),
                "{format}"
            );
            assert_eq!((f.pixel_art, f.pixel_rule), (Some(false), Some(PIXEL_RULE)));
        }
    }

    /// 読み取りの記録は正規の形で往復し、深さは可逆の形式だけ書く。
    #[test]
    fn read_record_round_trips() {
        use kura_engine::testgen::InputGen;
        let v = InputGen::Sine {
            rate: 22_050,
            channels: 1,
            frames: 100,
            freq: 440,
            loop_range: None,
        }
        .generate()
        .unwrap();
        let r = ReadRecord::from_value(&v, "wav");
        assert!(matches!(
            r,
            ReadRecord::Audio {
                rate: 22_050,
                channels: 1,
                frames: 100,
                ..
            }
        ));
        let bytes = r.to_canonical().unwrap();
        assert_eq!(ReadRecord::from_canonical(&bytes), Some(r));
        let img = ReadRecord::Image { w: 3, h: 2 };
        assert_eq!(
            String::from_utf8(img.to_canonical().unwrap()).unwrap(),
            r#"{"h":2,"value":"image","w":3}"#
        );
        assert_eq!(ReadRecord::from_canonical(b"{\"value\":\"image\"}"), None);
    }
}
