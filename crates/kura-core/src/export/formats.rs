//! 書き出しの形式と設定（M15 段 3。ADR-205）：画像・音声ごとの書く形式（そのまま・変えない・選んだ形式）と、プロジェクトの
//! 持ち出しの既定（`projects.settings`）。

use serde::{Deserialize, Serialize};

use super::names::NameRule;
use crate::error::{CoreError, Result};
use crate::import::{MediaKind, format_id};
use crate::project::Engine;

/// Kura が書けない形式を変えるときの既定（画像）。
pub const PLAYABLE_IMAGE: &str = "png";
/// Kura が書けない形式を変えるときの既定（音声。書けなければ WAV）。
pub const PLAYABLE_AUDIO: [&str; 2] = ["ogg", "wav"];

/// 画像・音声の書く形式。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FormatTarget {
    /// どの形式もそのまま写す（M9 の書き出し）。
    #[default]
    Original,
    /// 変えない。ただし Kura が書けない形式（PSD・aseprite・SVG・HEIC・AVIF・TIFF・BMP、m4a・Opus など）は、画像は PNG・音声は
    /// Ogg Vorbis に変えて書く（段 3 の決定 2）。
    Playable,
    /// この形式で書く（形式の表の ID。`png`・`webp`・`ogg` など）。
    To { format: String },
}

/// 1 件の書き方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// 今のファイルをそのまま写す。
    Copy,
    /// 元＋頭の文書から、この形式でエンジンで描いて書く。
    Convert { format: String },
}

/// 形式を書けるか（形式の表の ID）。
pub type Writable<'a> = &'a dyn Fn(&str) -> bool;

/// 1 件の書き方を決める。`writable` が None（エンジンの部品がない）なら、書けるかを判断できないので「変えない」は写す。
pub fn plan(
    kind: MediaKind,
    current_ext: &str,
    target: &FormatTarget,
    writable: Option<Writable<'_>>,
) -> Plan {
    if !matches!(kind, MediaKind::Image | MediaKind::Audio) {
        return Plan::Copy;
    }
    let current = format_id(current_ext);
    match target {
        FormatTarget::Original => Plan::Copy,
        FormatTarget::Playable => {
            let Some(writable) = writable else {
                return Plan::Copy;
            };
            if current.is_some_and(writable) {
                return Plan::Copy;
            }
            let to = match kind {
                MediaKind::Audio => PLAYABLE_AUDIO
                    .into_iter()
                    .find(|f| writable(f))
                    .unwrap_or(PLAYABLE_AUDIO[0]),
                _ => PLAYABLE_IMAGE,
            };
            Plan::Convert {
                format: to.to_string(),
            }
        }
        FormatTarget::To { format } if current == Some(format.as_str()) => Plan::Copy,
        FormatTarget::To { format } => Plan::Convert {
            format: format.clone(),
        },
    }
}

/// 画像・音声の形式の指定を確かめる（`To` の形式が形式の表にあり、その種類のもの）。
pub fn check_target(target: &FormatTarget, kind: MediaKind) -> Result<()> {
    if let FormatTarget::To { format } = target {
        let desc = kura_engine::codec::formats::format_by_id(format)
            .ok_or_else(|| CoreError::Invalid(format!("知らない形式です：{format}")))?;
        if MediaKind::from(desc.item_kind) != kind {
            return Err(CoreError::Invalid(format!(
                "{format} は{}の形式ではありません",
                match kind {
                    MediaKind::Audio => "音声",
                    _ => "画像",
                }
            )));
        }
    }
    Ok(())
}

fn yes() -> bool {
    true
}

/// 書き出しの設定（ジョブの入力の `options`。省けば M9 と同じ：元の名前・形式はそのまま・JSON を添える）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExportOptions {
    pub name: NameRule,
    pub image: FormatTarget,
    pub audio: FormatTarget,
    /// `kura-export.json` を添える（登録したプロジェクトの中へは既定で添えない。段 3 の決定 4）。
    #[serde(default = "yes")]
    pub manifest: bool,
    /// 登録したプロジェクトの中へ書き出す（書き出し先がその中であることを確かめ、終わったら照らし直しを積む）。
    pub project: Option<i64>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            name: NameRule::default(),
            image: FormatTarget::Original,
            audio: FormatTarget::Original,
            manifest: true,
            project: None,
        }
    }
}

impl ExportOptions {
    /// 名前のテンプレートと形式を確かめる。
    pub fn check(&self) -> Result<()> {
        if let Some(t) = &self.name.template {
            super::names::check_template(t)?;
        }
        check_target(&self.image, MediaKind::Image)?;
        check_target(&self.audio, MediaKind::Audio)
    }

    /// 種類の形式の指定。
    pub fn target(&self, kind: MediaKind) -> &FormatTarget {
        match kind {
            MediaKind::Audio => &self.audio,
            _ => &self.image,
        }
    }
}

/// プロジェクトの持ち出しの既定（`projects.settings`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExportPreset {
    pub name: NameRule,
    pub image: FormatTarget,
    pub audio: FormatTarget,
    pub manifest: bool,
}

impl Default for ExportPreset {
    fn default() -> Self {
        Self::for_engine(Engine::Folder)
    }
}

impl ExportPreset {
    /// エンジンごとの既定：Godot は snake_case（Godot の書き方の決まり）、Unity は PascalCase、ほかはそのまま。形式はどれも
    /// 「変えない（書けない形式は変える）」、JSON は添えない、ASCII にはしない。
    pub fn for_engine(engine: Engine) -> Self {
        let style = match engine {
            Engine::Godot => super::names::NameStyle::Snake,
            Engine::Unity => super::names::NameStyle::Pascal,
            Engine::Folder => super::names::NameStyle::Keep,
        };
        Self {
            name: NameRule {
                template: None,
                style,
                ascii: false,
            },
            image: FormatTarget::Playable,
            audio: FormatTarget::Playable,
            manifest: false,
        }
    }

    /// 保存した JSON から読む（`{}` や読めないものはエンジンの既定。足りない項目は既定で埋めない：項目ごとの `default` は
    /// フォルダの既定になるので、空の JSON だけをエンジンの既定にする）。
    pub fn from_json(json: &str, engine: Engine) -> Self {
        match serde_json::from_str::<serde_json::Value>(json) {
            Ok(serde_json::Value::Object(map)) if !map.is_empty() => {
                serde_json::from_value(serde_json::Value::Object(map))
                    .unwrap_or_else(|_| Self::for_engine(engine))
            }
            _ => Self::for_engine(engine),
        }
    }

    /// 書き出しの設定にする（プロジェクトの中へ）。
    pub fn options(&self, project: i64) -> ExportOptions {
        ExportOptions {
            name: self.name.clone(),
            image: self.image.clone(),
            audio: self.audio.clone(),
            manifest: self.manifest,
            project: Some(project),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writable(f: &str) -> bool {
        matches!(
            f,
            "png" | "jpeg" | "gif" | "webp" | "wav" | "aiff" | "flac" | "ogg"
        )
    }

    #[test]
    fn plans_copy_or_convert_by_kind_and_target() {
        let w: Writable<'_> = &writable;
        let to = |f: &str| FormatTarget::To { format: f.into() };
        let convert = |f: &str| Plan::Convert { format: f.into() };
        // そのまま（M9）はいつも写す
        assert_eq!(
            plan(MediaKind::Image, "psd", &FormatTarget::Original, Some(w)),
            Plan::Copy
        );
        // 変えない：書ける形式は写し、書けない形式は PNG・Ogg に
        assert_eq!(
            plan(MediaKind::Image, "png", &FormatTarget::Playable, Some(w)),
            Plan::Copy
        );
        assert_eq!(
            plan(MediaKind::Image, "jpg", &FormatTarget::Playable, Some(w)),
            Plan::Copy
        );
        for ext in ["psd", "ase", "svg", "heic", "avif", "tif", "bmp"] {
            assert_eq!(
                plan(MediaKind::Image, ext, &FormatTarget::Playable, Some(w)),
                convert("png"),
                "{ext}"
            );
        }
        for ext in ["m4a", "opus", "mp3"] {
            assert_eq!(
                plan(MediaKind::Audio, ext, &FormatTarget::Playable, Some(w)),
                convert("ogg"),
                "{ext}"
            );
        }
        // Ogg が書けなければ WAV
        let no_ogg = |f: &str| f == "wav";
        assert_eq!(
            plan(
                MediaKind::Audio,
                "m4a",
                &FormatTarget::Playable,
                Some(&no_ogg)
            ),
            convert("wav")
        );
        // 選んだ形式：同じ形式なら写す（jpg と jpeg は同じ）
        assert_eq!(
            plan(MediaKind::Image, "jpg", &to("jpeg"), Some(w)),
            Plan::Copy
        );
        assert_eq!(
            plan(MediaKind::Image, "png", &to("webp"), Some(w)),
            convert("webp")
        );
        assert_eq!(
            plan(MediaKind::Audio, "wav", &to("ogg"), Some(w)),
            convert("ogg")
        );
        // 動画・フォント・3D はいつも写す。エンジンの部品がなければ「変えない」は写す
        for kind in [MediaKind::Video, MediaKind::Font, MediaKind::Model] {
            assert_eq!(plan(kind, "mp4", &to("png"), Some(w)), Plan::Copy);
        }
        assert_eq!(
            plan(MediaKind::Image, "psd", &FormatTarget::Playable, None),
            Plan::Copy
        );
    }

    #[test]
    fn options_are_checked_and_default_to_m9() {
        let o: ExportOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(o, ExportOptions::default());
        assert!(o.manifest);
        assert_eq!(o.image, FormatTarget::Original);
        let o: ExportOptions = serde_json::from_str(
            r#"{"name":{"template":"{名前}","style":"snake","ascii":true},"image":{"type":"to","format":"webp"},"audio":{"type":"playable"},"manifest":false,"project":3}"#,
        )
        .unwrap();
        assert!(o.check().is_ok());
        assert_eq!(o.project, Some(3));
        let bad = |json: &str| serde_json::from_str::<ExportOptions>(json).unwrap().check();
        assert!(bad(r#"{"image":{"type":"to","format":"ogg"}}"#).is_err());
        assert!(bad(r#"{"audio":{"type":"to","format":"png"}}"#).is_err());
        assert!(bad(r#"{"image":{"type":"to","format":"nope"}}"#).is_err());
        assert!(bad(r#"{"name":{"template":"{色}"}}"#).is_err());
    }

    #[test]
    fn presets_follow_the_engine_and_round_trip() {
        let g = ExportPreset::from_json("{}", Engine::Godot);
        assert_eq!(g.name.style, super::super::names::NameStyle::Snake);
        assert_eq!(
            (g.image.clone(), g.manifest),
            (FormatTarget::Playable, false)
        );
        assert_eq!(
            ExportPreset::from_json("not json", Engine::Unity)
                .name
                .style,
            super::super::names::NameStyle::Pascal
        );
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(ExportPreset::from_json(&json, Engine::Folder), g);
        let o = g.options(9);
        assert_eq!((o.project, o.manifest), (Some(9), false));
    }
}
