//! 解析の計画（純粋な関数。DB に触らない。docs/data-model.md §3・decisions の決定 5・7）。
//!
//! - 種類と形式から、読んで解析するか（[`Plan::Read`]）、読まずに形式から事実を書くか（[`Plan::FromFormat`]。JPEG）、
//!   見出し（画素を展開しない読み取り）でアルファを持つものだけを読むか（[`Plan::ByHeader`]。HEIC・HEIF。決定 15）、
//!   解析しないか（[`Plan::NotNeeded`]。動画・フォント・3D）を決める。
//! - 読むときの要求は 2 段：段 1 は値の種類だけで決まり（画像は透過・色数・倍率、音声はラウドネス・ループ・無音・波形の段）、
//!   段 2 は段 1 の結果で決める（スプライトの格子の推定は、1 枚の画像で透過があるか 256 色以下のときだけ）。波形の段は長さから
//!   決める（最も細かい段はビンが [`PEAKS_MAX_BINS`] 以下になる段。粗い方は 65,536 まで）。
//! - 計画の指紋（[`PlanFingerprint`]。settings の `analysis.plan`）は起動のスレッドで比べ、変わった種類だけ pending に戻す。
//!   形式ごとの扱い（読む以外のもの）が変わったら、その形式のものだけを戻す（決定 15 で HEIC・HEIF の見出しを見直す）。
//!   デコーダの指紋（[`DecoderFingerprint`]。`analysis.decoders`）はコーデックの組が要るので、解析のジョブが組を作ったときに
//!   比べる（起動の道でコーデックの組を作らない）。

use std::collections::BTreeMap;

use kura_engine::AnalysisRequest;
use kura_engine::Shape;
use kura_engine::analysis::audio::loop_points::LoopAnalysis;
use kura_engine::analysis::audio::loudness::LoudnessAnalysis;
use kura_engine::analysis::audio::peaks::{PEAK_BINS, PeaksAnalysis};
use kura_engine::analysis::audio::silence::SilenceAnalysis;
use kura_engine::analysis::image::alpha::{Alpha, AlphaKind, AlphaOutput};
use kura_engine::analysis::image::colors::{Colors, ColorsOutput};
use kura_engine::analysis::image::pixel_scale::PixelScale;
use kura_engine::analysis::image::sprite_grid::SpriteGrid;
use kura_engine::analysis::{Analysis, AnalysisRegistry};
use kura_engine::codec::CodecSet;
use kura_engine::codec::formats::FORMATS;
use kura_engine::digest::canonical_json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::facts::{FACTS_V, PIXEL_RULE, ReadRecord};
use crate::import::MediaKind;
use crate::{CoreError, Result};

/// 計画の決め方の版（要求の選び方・段 2 の条件・波形の段の決め方を変えたら上げる。上げると画像・音声をすべて解き直す）。
pub const PLAN_V: u32 = 1;
/// 計画の指紋を置く settings のキー。
pub const PLAN_KEY: &str = "analysis.plan";
/// 解析した形式ごとのデコーダの指紋を置く settings のキー（解析のジョブが比べる）。
pub const DECODERS_KEY: &str = "analysis.decoders";
/// 読み取りの記録（kura-core の ID。値の種類・大きさ・長さ。投影と、読まずに計画を立てるのに使う）を置く結果の ID。
pub const READ_ID: &str = "kura.read";
/// 読み取りの記録の版。
pub const READ_V: u16 = 1;
/// 波形の最も細かい段のビンの数の上限（5 分の 44.1 kHz で 4,096 フレームの段＝約 34 KB）。
pub const PEAKS_MAX_BINS: u64 = 4_096;
/// 読まずに形式から事実を書く形式（決定 5：透過なし・色数は計らない（NULL）・ドット絵でない・1 コマ。読むと 24MP で 1 件
/// 100〜166 ms かかるため。`facts::from_format`）。
pub const FROM_FORMAT: [&str; 1] = ["jpeg"];
/// 見出しで決める形式（決定 15）：画素を展開しない読み取り（Platform の段の形。ImageIO の `CGImage` の属性）でアルファを
/// 持つと分かったものだけを読んで解析し、ほかは読まずに形式から書く（決定 5 のまま。見出しを読めないものも形式から）。
pub const BY_HEADER: [&str; 2] = ["heic", "heif"];
/// スプライトの格子を推定する色の数の上限（これ以下か、透過があるとき）。
pub const GRID_MAX_COLORS: u16 = 256;

/// アイテムの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// 解析しない（動画・フォント・3D。analysis_state は not_needed）。
    NotNeeded,
    /// 読まずに形式から事実を書く（JPEG：透過なし・色数は計らない・ドット絵でない）。
    FromFormat,
    /// 見出しを読み、アルファを持つ形（[`read_after_header`]）なら読んで解析し、ほかは読まずに形式から書く（HEIC・HEIF。
    /// 決定 15）。
    ByHeader,
    /// 読んで解析する。
    Read,
}

impl Plan {
    /// 計画の指紋に書く名前。
    pub fn name(self) -> &'static str {
        match self {
            Self::NotNeeded => "notNeeded",
            Self::FromFormat => "fromFormat",
            Self::ByHeader => "byHeader",
            Self::Read => "read",
        }
    }
}

/// 種類と形式（形式の表の ID）からの扱い。
pub fn plan_for(kind: MediaKind, format: &str) -> Plan {
    match kind {
        MediaKind::Video | MediaKind::Font | MediaKind::Model => Plan::NotNeeded,
        MediaKind::Image if FROM_FORMAT.contains(&format) => Plan::FromFormat,
        MediaKind::Image if BY_HEADER.contains(&format) => Plan::ByHeader,
        MediaKind::Image | MediaKind::Audio => Plan::Read,
    }
}

/// 見出しで決める形式（[`Plan::ByHeader`]）で、見出しの形がアルファを持つ（1 枚の画像かフレームの列で、画素の形式に
/// アルファがある）なら読む。Platform の段は画素の形式を、展開しない `CGImage` のアルファの扱いから決める。
pub fn read_after_header(shape: &Shape) -> bool {
    match shape {
        Shape::Image { fmt, .. } | Shape::Frames { fmt, .. } => fmt.has_alpha(),
        Shape::Audio { .. } => false,
    }
}

/// 読み取りの記録（`kura.read`）がなければ形式から書く形式か（JPEG と、見出しで決める HEIC・HEIF。投影の作り直しで、
/// 読まなかったものを見分ける）。
pub fn from_format_unless_read(format: &str) -> bool {
    FROM_FORMAT.contains(&format) || BY_HEADER.contains(&format)
}

fn request<A: Analysis>(params: serde_json::Value) -> AnalysisRequest {
    AnalysisRequest {
        id: A::ID.to_string(),
        v: Some(A::VERSION),
        params,
    }
}

/// 段 1：読んだ値に当てる解析（値の種類と、音声は長さで決まる）。
pub fn stage1(read: &ReadRecord) -> Vec<AnalysisRequest> {
    match read {
        ReadRecord::Image { .. } | ReadRecord::Frames { .. } => vec![
            request::<Alpha>(json!({})),
            request::<Colors>(json!({})),
            request::<PixelScale>(json!({})),
        ],
        ReadRecord::Audio { frames, .. } => vec![
            request::<LoudnessAnalysis>(json!({})),
            request::<LoopAnalysis>(json!({})),
            request::<SilenceAnalysis>(json!({})),
            request::<PeaksAnalysis>(peaks_params(*frames)),
        ],
    }
}

/// 段 2：段 1 の結果で決める解析。スプライトの格子の推定は 1 枚の画像（フレームの列は受けない）で、透過があるか
/// 256 色以下のときだけ（写真では意味がない）。段 1 の結果がなければ（失敗）解かない。
pub fn stage2(
    read: &ReadRecord,
    alpha: Option<&AlphaOutput>,
    colors: Option<&ColorsOutput>,
) -> Vec<AnalysisRequest> {
    let ReadRecord::Image { .. } = read else {
        return Vec::new();
    };
    let transparent = alpha.is_some_and(|a| a.kind != AlphaKind::None);
    let few_colors = colors.is_some_and(|c| c.count <= GRID_MAX_COLORS);
    if transparent || few_colors {
        vec![request::<SpriteGrid>(json!({}))]
    } else {
        Vec::new()
    }
}

/// 波形の段の引数：最も細かい段は、ビンの数が [`PEAKS_MAX_BINS`] 以下になる最も細かい段（どれも超えれば最も粗い段）、
/// 最も粗い段は 65,536 フレーム。
pub fn peaks_params(frames: u64) -> serde_json::Value {
    let coarsest = PEAK_BINS[PEAK_BINS.len() - 1];
    let finest = PEAK_BINS
        .iter()
        .copied()
        .find(|b| frames.div_ceil(u64::from(*b)) <= PEAKS_MAX_BINS)
        .unwrap_or(coarsest);
    json!({ "finest": finest, "coarsest": coarsest })
}

/// 解析した要求の鍵：ID・版・正規の引数（既定値を補って正規の JSON にしたもの）。結果の行と照らすのに使う。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RequestKey {
    pub id: String,
    pub v: u16,
    /// 正規の引数（正規の JSON の文字列）。
    pub params: String,
}

/// 要求を鍵にする（引数を登録表で正規の形にする）。知らない ID・版、読めない引数は誤り。
pub fn request_key(registry: &AnalysisRegistry, req: &AnalysisRequest) -> Result<RequestKey> {
    let a = registry
        .resolve(&req.id, req.v)
        .map_err(|i| CoreError::Invalid(format!("解析 {} を引けません: {}", req.id, i.message)))?;
    let params = a
        .check_params(&req.params)
        .map_err(|_| CoreError::Invalid(format!("解析 {} の引数が正しくありません", req.id)))?;
    Ok(RequestKey {
        id: a.id().to_string(),
        v: a.version(),
        params: canonical_string(&params)?,
    })
}

/// 正規の JSON の文字列（キーの順・数の書き方が決まった形）。
pub fn canonical_string(value: &serde_json::Value) -> Result<String> {
    let bytes = canonical_json(value)
        .map_err(|e| CoreError::Invalid(format!("正規の JSON にできません: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|e| CoreError::Invalid(format!("正規の JSON が UTF-8 でありません: {e}")))
}

/// 計画の指紋（settings の `analysis.plan`）。起動のスレッドで、前に置いたものと比べる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFingerprint {
    pub plan_v: u32,
    pub facts_v: i64,
    pub pixel_rule: i64,
    pub read_v: u16,
    /// 種類（image・audio）ごとの解析：ID → (版, 既定の引数の正規の形)。音声の波形の段の引数は長さで決まるので、ここは
    /// 既定の引数（決め方は `plan_v`）。
    pub kinds: BTreeMap<String, BTreeMap<String, (u16, String)>>,
    /// 画像・音声の形式のうち、読む（`read`）以外の扱いのもの：形式 → 扱いの名前（[`Plan::name`]）。決定 15 より前の指紋には
    /// この欄がなく、JPEG・HEIC・HEIF をすべて形式から書いていた（[`formats_before_header`]）。
    #[serde(default = "formats_before_header")]
    pub formats: BTreeMap<String, String>,
}

/// 決定 15 より前の指紋の形式ごとの扱い（欄がないときの値）：JPEG・HEIC・HEIF はすべて形式から。
fn formats_before_header() -> BTreeMap<String, String> {
    ["heic", "heif", "jpeg"]
        .into_iter()
        .map(|f| (f.to_string(), Plan::FromFormat.name().to_string()))
        .collect()
}

/// 今の形式ごとの扱い（形式の表の画像・音声の行のうち、読む以外のもの）。
fn current_formats() -> BTreeMap<String, String> {
    FORMATS
        .iter()
        .filter_map(|f| {
            let kind = MediaKind::from(f.item_kind);
            if !ANALYZED_KINDS.contains(&kind) {
                return None;
            }
            match plan_for(kind, f.id) {
                Plan::Read | Plan::NotNeeded => None,
                plan @ (Plan::FromFormat | Plan::ByHeader) => {
                    Some((f.id.to_string(), plan.name().to_string()))
                }
            }
        })
        .collect()
}

/// 種類ごとの、計画に入りうるすべての解析の型の要求（段 2 を含む。引数は既定）。
fn template(kind: MediaKind) -> Vec<AnalysisRequest> {
    match kind {
        MediaKind::Image => vec![
            request::<Alpha>(json!({})),
            request::<Colors>(json!({})),
            request::<PixelScale>(json!({})),
            request::<SpriteGrid>(json!({})),
        ],
        MediaKind::Audio => vec![
            request::<LoudnessAnalysis>(json!({})),
            request::<LoopAnalysis>(json!({})),
            request::<SilenceAnalysis>(json!({})),
            request::<PeaksAnalysis>(json!({})),
        ],
        MediaKind::Video | MediaKind::Font | MediaKind::Model => Vec::new(),
    }
}

/// 解析する種類（画像・音声）。
pub const ANALYZED_KINDS: [MediaKind; 2] = [MediaKind::Image, MediaKind::Audio];

impl PlanFingerprint {
    /// 今の計画の指紋（エンジンの登録表で、各解析の今の版と既定の引数の正規の形を引く）。
    pub fn current(registry: &AnalysisRegistry) -> Result<Self> {
        let mut kinds = BTreeMap::new();
        for kind in ANALYZED_KINDS {
            let mut analyses = BTreeMap::new();
            for req in template(kind) {
                let key = request_key(registry, &req)?;
                analyses.insert(key.id, (key.v, key.params));
            }
            kinds.insert(kind.as_str().to_string(), analyses);
        }
        Ok(Self {
            plan_v: PLAN_V,
            facts_v: FACTS_V,
            pixel_rule: PIXEL_RULE,
            read_v: READ_V,
            kinds,
            formats: current_formats(),
        })
    }

    /// 正規の JSON（settings に置く形。同じ中身なら同じバイト列）。
    pub fn to_canonical(&self) -> Result<String> {
        canonical_string(&serde_json::to_value(self)?)
    }

    /// 前に置いた指紋 `old` から変わったとき、解き直す（pending に戻す）種類。`old` が読めない・ないときは、0007 で
    /// pending にしたものだけが対象なので何も戻さない（最初の起動）。
    pub fn kinds_to_reset(&self, old: Option<&PlanFingerprint>) -> Vec<MediaKind> {
        let Some(old) = old else {
            return Vec::new();
        };
        let all = old.plan_v != self.plan_v || old.read_v != self.read_v;
        ANALYZED_KINDS
            .into_iter()
            .filter(|k| all || old.kinds.get(k.as_str()) != self.kinds.get(k.as_str()))
            .collect()
    }

    /// 前に置いた指紋 `old` から扱いが変わった形式（足された・なくなった・変わった。並べたもの）。その形式のものを解き直す
    /// （決定 15：形式から書いていた HEIC・HEIF の見出しを見直す）。`old` がなければ何も戻さない（最初の起動）。
    pub fn formats_to_recheck(&self, old: Option<&PlanFingerprint>) -> Vec<String> {
        let Some(old) = old else {
            return Vec::new();
        };
        let mut out: Vec<String> = self
            .formats
            .iter()
            .filter(|(f, plan)| old.formats.get(*f) != Some(*plan))
            .map(|(f, _)| f.clone())
            .collect();
        out.extend(
            old.formats
                .keys()
                .filter(|f| !self.formats.contains_key(*f))
                .cloned(),
        );
        out.sort();
        out.dedup();
        out
    }

    /// 投影（事実の列）だけが変わったか（読み直さずに結果の表から作り直す）。
    pub fn projection_changed(&self, old: Option<&PlanFingerprint>) -> bool {
        old.is_some_and(|o| o.facts_v != self.facts_v || o.pixel_rule != self.pixel_rule)
    }
}

/// 形式（解析で読む形式だけ）ごとの、段の順のデコーダの識別子（settings の `analysis.decoders`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecoderFingerprint(pub BTreeMap<String, Vec<String>>);

impl DecoderFingerprint {
    /// コーデックの組から作る（形式の表の画像・音声のうち、読んで解析する形式と、見出しで決める形式）。
    pub fn of(codecs: &CodecSet) -> Self {
        let mut map = BTreeMap::new();
        for f in FORMATS {
            let kind = MediaKind::from(f.item_kind);
            if !matches!(plan_for(kind, f.id), Plan::Read | Plan::ByHeader) {
                continue;
            }
            let ids = codecs
                .decoders_for(f.id)
                .iter()
                .map(|d| d.id().to_string())
                .collect();
            map.insert(f.id.to_string(), ids);
        }
        Self(map)
    }

    /// 前の指紋から、デコーダの並びが変わった形式（足された・なくなった・入れ替わった）。
    pub fn changed_formats(&self, old: &DecoderFingerprint) -> Vec<String> {
        let mut out: Vec<String> = self
            .0
            .iter()
            .filter(|(f, ids)| old.0.get(*f) != Some(*ids))
            .map(|(f, _)| f.clone())
            .collect();
        out.extend(old.0.keys().filter(|f| !self.0.contains_key(*f)).cloned());
        out
    }

    /// どれかの形式で今使えるデコーダの識別子（重複なし・並べたもの）。
    pub fn all_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.0.values().flatten().cloned().collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::analysis::registry;
    use kura_engine::shape::{Dim, Fullness};
    use kura_engine::value::PixelFormat;

    fn image() -> ReadRecord {
        ReadRecord::Image { w: 64, h: 64 }
    }

    fn ids(reqs: &[AnalysisRequest]) -> Vec<&str> {
        reqs.iter().map(|r| r.id.as_str()).collect()
    }

    /// 種類と形式ごとの扱い：JPEG は読まない（決定 5）、HEIC・HEIF は見出しで決める（決定 15）、動画・フォント・3D は解析しない。
    #[test]
    fn plans_by_kind_and_format() {
        use MediaKind::*;
        let cases = [
            (Image, "png", Plan::Read),
            (Image, "gif", Plan::Read),
            (Image, "webp", Plan::Read),
            (Image, "aseprite", Plan::Read),
            (Image, "psd", Plan::Read),
            (Image, "svg", Plan::Read),
            (Image, "avif", Plan::Read),
            (Image, "tiff", Plan::Read),
            (Image, "bmp", Plan::Read),
            (Image, "jpeg", Plan::FromFormat),
            (Image, "heic", Plan::ByHeader),
            (Image, "heif", Plan::ByHeader),
            (Audio, "wav", Plan::Read),
            (Audio, "flac", Plan::Read),
            (Audio, "mp3", Plan::Read),
            (Audio, "ogg", Plan::Read),
            (Video, "mp4", Plan::NotNeeded),
            (Video, "mov", Plan::NotNeeded),
            (Font, "ttf", Plan::NotNeeded),
            (Font, "woff2", Plan::NotNeeded),
            (Model, "glb", Plan::NotNeeded),
        ];
        for (kind, format, plan) in cases {
            assert_eq!(plan_for(kind, format), plan, "{kind:?} {format}");
        }
        // 形式の表のすべての行で決まる（画像・音声は読むか形式からか、ほかは解析しない）
        for f in FORMATS {
            let kind = MediaKind::from(f.item_kind);
            let plan = plan_for(kind, f.id);
            match kind {
                MediaKind::Image => assert_ne!(plan, Plan::NotNeeded, "{}", f.id),
                MediaKind::Audio => assert_eq!(plan, Plan::Read, "{}", f.id),
                MediaKind::Video | MediaKind::Font | MediaKind::Model => {
                    assert_eq!(plan, Plan::NotNeeded, "{}", f.id)
                }
            }
        }
    }

    /// 段 1 は値の種類で決まり、段 2（格子の推定）は 1 枚の画像で透過があるか 256 色以下のときだけ。
    #[test]
    fn stages_follow_the_value_and_the_first_results() {
        assert_eq!(
            ids(&stage1(&image())),
            ["image.alpha", "image.colors", "image.pixel_scale"]
        );
        let frames = ReadRecord::Frames {
            w: 64,
            h: 64,
            count: 8,
        };
        assert_eq!(ids(&stage1(&frames)), ids(&stage1(&image())));
        let audio = ReadRecord::Audio {
            rate: 44_100,
            channels: 2,
            frames: 44_100,
            bits: Some(16),
        };
        assert_eq!(
            ids(&stage1(&audio)),
            [
                "audio.loudness",
                "audio.loop",
                "audio.silence",
                "audio.peaks"
            ]
        );
        for r in stage1(&audio).iter().chain(stage1(&image()).iter()) {
            assert!(r.v.is_some(), "版を決めて頼む: {}", r.id);
        }

        let alpha = |kind| AlphaOutput {
            kind,
            transparent: 0,
            partial: 0,
            opaque: 1,
            uncovered: 0,
        };
        let colors = |count| ColorsOutput {
            count,
            colors: None,
            visible: count,
            palette_lossless: false,
            palette: None,
        };
        let opaque = alpha(AlphaKind::None);
        let binary = alpha(AlphaKind::Binary);
        let photo = colors(257);
        let few = colors(256);
        assert!(stage2(&image(), Some(&opaque), Some(&photo)).is_empty());
        assert_eq!(
            ids(&stage2(&image(), Some(&binary), Some(&photo))),
            ["image.sprite_grid"]
        );
        assert_eq!(
            ids(&stage2(&image(), Some(&opaque), Some(&few))),
            ["image.sprite_grid"]
        );
        assert!(
            stage2(&frames, Some(&binary), Some(&few)).is_empty(),
            "フレームの列は受けない"
        );
        assert!(stage2(&audio, None, None).is_empty());
        assert!(
            stage2(&image(), None, None).is_empty(),
            "段 1 が失敗したら解かない"
        );
    }

    /// 波形の段：最も細かい段はビンが 4,096 以下になる段（5 分の 44.1 kHz で 4,096 フレームの段）、粗い方は 65,536。
    #[test]
    fn peaks_levels_follow_the_length() {
        let p = |frames: u64| {
            let v = peaks_params(frames);
            (
                v["finest"].as_u64().unwrap(),
                v["coarsest"].as_u64().unwrap(),
            )
        };
        assert_eq!(p(0), (256, 65_536));
        assert_eq!(p(256 * 4_096), (256, 65_536));
        assert_eq!(p(256 * 4_096 + 1), (1_024, 65_536));
        assert_eq!(p(44_100 * 300), (4_096, 65_536));
        assert_eq!(p(65_536 * 4_096 + 1), (65_536, 65_536));
        assert_eq!(p(u64::MAX), (65_536, 65_536));
        let registry = registry().unwrap();
        for frames in [0, 1, 44_100 * 300, u64::MAX] {
            let req = request::<PeaksAnalysis>(peaks_params(frames));
            request_key(&registry, &req).unwrap();
        }
    }

    /// 指紋は正規の JSON で、同じ中身なら同じバイト列（凍結した形と照らす）。版を上げた種類だけ解き直す。
    #[test]
    fn fingerprint_is_canonical_and_stable() {
        let registry = registry().unwrap();
        let a = PlanFingerprint::current(&registry).unwrap();
        let b = PlanFingerprint::current(&registry).unwrap();
        assert_eq!(a.to_canonical().unwrap(), b.to_canonical().unwrap());
        assert_eq!(
            a.to_canonical().unwrap(),
            concat!(
                r#"{"factsV":1,"formats":{"heic":"byHeader","heif":"byHeader","jpeg":"fromFormat"},"#,
                r#""kinds":{"audio":{"audio.loop":[1,"{}"],"audio.loudness":[1,"{}"],"#,
                r#""audio.peaks":[1,"{\"coarsest\":65536,\"finest\":256}"],"#,
                r#""audio.silence":[1,"{\"thresholdDb\":-60}"]},"#,
                r#""image":{"image.alpha":[1,"{}"],"image.colors":[1,"{}"],"image.pixel_scale":[1,"{}"],"#,
                r#""image.sprite_grid":[1,"{}"]}},"pixelRule":1,"planV":1,"readV":1}"#
            )
        );
        let back: PlanFingerprint = serde_json::from_str(&a.to_canonical().unwrap()).unwrap();
        assert_eq!(back, a);

        assert!(a.kinds_to_reset(None).is_empty(), "最初の起動では戻さない");
        assert!(a.kinds_to_reset(Some(&a)).is_empty());
        let mut old = a.clone();
        old.kinds
            .get_mut("audio")
            .unwrap()
            .get_mut("audio.loudness")
            .unwrap()
            .0 = 0;
        assert_eq!(a.kinds_to_reset(Some(&old)), vec![MediaKind::Audio]);
        let mut old = a.clone();
        old.plan_v = 0;
        assert_eq!(
            a.kinds_to_reset(Some(&old)),
            vec![MediaKind::Image, MediaKind::Audio]
        );
        let mut old = a.clone();
        old.facts_v = 0;
        assert!(a.kinds_to_reset(Some(&old)).is_empty());
        assert!(a.projection_changed(Some(&old)));
        assert!(!a.projection_changed(Some(&a)));
        assert!(
            a.formats_to_recheck(None).is_empty(),
            "最初の起動では戻さない"
        );
        assert!(a.formats_to_recheck(Some(&a)).is_empty());
    }

    /// 見出しで決める（決定 15）：アルファを持つ形（1 枚の画像・フレームの列）なら読む。持たなければ読まずに形式から書く。
    #[test]
    fn the_header_decides_whether_to_read() {
        let image = |fmt| Shape::Image {
            w: Dim::Exact(48),
            h: Dim::Exact(32),
            fmt,
        };
        for fmt in [
            PixelFormat::Rgba8,
            PixelFormat::GrayA8,
            PixelFormat::Rgba16,
            PixelFormat::GrayA16,
        ] {
            assert!(read_after_header(&image(fmt)), "{fmt:?}");
        }
        for fmt in [
            PixelFormat::Rgb8,
            PixelFormat::Gray8,
            PixelFormat::Rgb16,
            PixelFormat::Gray16,
        ] {
            assert!(!read_after_header(&image(fmt)), "{fmt:?}");
        }
        let frames = |fmt| Shape::Frames {
            count: Dim::Exact(3),
            w: Dim::Exact(24),
            h: Dim::Exact(16),
            fmt,
            full: Fullness::Full,
        };
        assert!(read_after_header(&frames(PixelFormat::Rgba8)));
        assert!(!read_after_header(&frames(PixelFormat::Rgb8)));
        let audio = Shape::Audio {
            rate: 44_100,
            channels: 2,
            frames: Dim::Exact(1),
        };
        assert!(!read_after_header(&audio));
    }

    /// 決定 15 の前の指紋（形式ごとの扱いの欄がない：JPEG・HEIC・HEIF をすべて形式から書いていた）から上げると、扱いが
    /// 変わった HEIC・HEIF だけを見直す（種類ごとには戻さない。投影も作り直さない）。
    #[test]
    fn a_fingerprint_from_before_the_header_rechecks_heic_and_heif() {
        let registry = registry().unwrap();
        let current = PlanFingerprint::current(&registry).unwrap();
        let before: PlanFingerprint = serde_json::from_str(concat!(
            r#"{"factsV":1,"kinds":{"audio":{"audio.loop":[1,"{}"],"audio.loudness":[1,"{}"],"#,
            r#""audio.peaks":[1,"{\"coarsest\":65536,\"finest\":256}"],"#,
            r#""audio.silence":[1,"{\"thresholdDb\":-60}"]},"#,
            r#""image":{"image.alpha":[1,"{}"],"image.colors":[1,"{}"],"image.pixel_scale":[1,"{}"],"#,
            r#""image.sprite_grid":[1,"{}"]}},"pixelRule":1,"planV":1,"readV":1}"#
        ))
        .unwrap();
        assert_eq!(
            before.formats.get("heic").map(String::as_str),
            Some("fromFormat")
        );
        assert_ne!(before, current);
        assert_eq!(current.formats_to_recheck(Some(&before)), ["heic", "heif"]);
        assert!(current.kinds_to_reset(Some(&before)).is_empty());
        assert!(!current.projection_changed(Some(&before)));
        // 扱いをなくした形式も見直す（今は読む形式に戻したとき）
        let mut later = current.clone();
        later.formats.remove("heif");
        assert_eq!(later.formats_to_recheck(Some(&current)), ["heif"]);
    }

    /// デコーダの指紋：解析で読む形式だけを持ち、並びが変わった形式を挙げる。
    #[test]
    fn decoder_fingerprint_lists_changed_formats() {
        let pinned = DecoderFingerprint::of(&CodecSet::pinned());
        let png = &pinned.0["png"];
        assert_eq!(png.len(), 1);
        assert!(png[0].starts_with("png-rs-dec/"), "{png:?}");
        assert!(!pinned.0.contains_key("jpeg"), "JPEG は読まない");
        assert!(
            pinned.0.contains_key("heic") && pinned.0.contains_key("heif"),
            "見出しで決める形式は、アルファがあれば読む"
        );
        assert!(!pinned.0.contains_key("mp4"));
        assert!(
            pinned.0.contains_key("bmp"),
            "固定の段で読めなくても形式は載る"
        );
        assert!(pinned.changed_formats(&pinned).is_empty());
        let mut old = pinned.clone();
        old.0.insert("bmp".into(), vec!["imageio-dec/1".into()]);
        old.0.remove("wav");
        let mut changed = pinned.changed_formats(&old);
        changed.sort();
        assert_eq!(changed, ["bmp", "wav"]);
        assert!(pinned.all_ids().contains(&png[0]));
    }
}
