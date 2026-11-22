//! 書き出しの形式と設定（M15 段 3。ADR-205）：画像・音声ごとの書く形式（そのまま・変えない・選んだ形式）と、プロジェクトの
//! 持ち出しの既定（`projects.settings`）。

use serde::{Deserialize, Serialize};

use super::names::NameRule;
use crate::error::{CoreError, Result};
use crate::import::{MediaKind, format_id};
use crate::project::Engine;

/// 「変えない」でそのまま写す形式（ゲームエンジン（Godot・Unity）が読める固定の表。段 3 の検証の直し・利用者の決定）。
pub const PLAYABLE_KEEP_IMAGE: [&str; 3] = ["png", "jpeg", "webp"];
pub const PLAYABLE_KEEP_AUDIO: [&str; 3] = ["wav", "ogg", "mp3"];
/// 表にない画像を変える先。
pub const PLAYABLE_IMAGE: &str = "png";
/// 表にない音声を変える先：可逆（FLAC・AIFF・ALAC）は WAV、非可逆（AAC・Opus など）は Ogg Vorbis（書けなければもう一方）。
pub const PLAYABLE_LOSSLESS_AUDIO: [&str; 2] = ["wav", "ogg"];
pub const PLAYABLE_LOSSY_AUDIO: [&str; 2] = ["ogg", "wav"];

/// 画像・音声の書く形式。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FormatTarget {
    /// どの形式もそのまま写す（M9 の書き出し）。
    #[default]
    Original,
    /// 変えない。ただしゲームエンジンが読める表（画像 PNG・JPEG・WebP、音声 WAV・Ogg Vorbis・MP3）にない形式は、画像は PNG、
    /// 音声は可逆なら WAV・非可逆なら Ogg Vorbis に変えて書く（段 3 の決定 2 と検証の直し）。
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

/// 音声の形式が可逆か（FLAC・AIFF・WAV と、コーデックが ALAC の m4a）。
fn lossless_audio(format: Option<&str>, codec: Option<&str>) -> bool {
    matches!(format, Some("flac" | "aiff" | "wav"))
        || codec.is_some_and(|c| c.eq_ignore_ascii_case("alac"))
}

/// 1 件の書き方を決める（`codec` は items.codec_info の主なコーデック。m4a の可逆を見分ける）。`writable` は変える先を選ぶのに
/// 使う（None ならエンジンの部品がないので、変える 1 件は書くときに失敗する）。
pub fn plan(
    kind: MediaKind,
    current_ext: &str,
    codec: Option<&str>,
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
            let keep: &[&str] = match kind {
                MediaKind::Audio => &PLAYABLE_KEEP_AUDIO,
                _ => &PLAYABLE_KEEP_IMAGE,
            };
            if current.is_some_and(|c| keep.contains(&c)) {
                return Plan::Copy;
            }
            let to = match kind {
                MediaKind::Audio => {
                    let order = if lossless_audio(current, codec) {
                        PLAYABLE_LOSSLESS_AUDIO
                    } else {
                        PLAYABLE_LOSSY_AUDIO
                    };
                    order
                        .into_iter()
                        .find(|f| writable.is_some_and(|w| w(f)))
                        .unwrap_or(order[0])
                }
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

    #[test]
    fn plans_copy_or_convert_by_kind_and_target() {
        // macOS の組に近い：m4a（AAC）・MP3 も書ける
        let w: Writable<'_> = &|f: &str| {
            matches!(
                f,
                "png" | "jpeg" | "gif" | "webp" | "wav" | "aiff" | "flac" | "ogg" | "mp3" | "m4a"
            )
        };
        let to = |f: &str| FormatTarget::To { format: f.into() };
        let convert = |f: &str| Plan::Convert { format: f.into() };
        let pl = FormatTarget::Playable;
        // そのまま（M9）はいつも写す
        assert_eq!(
            plan(
                MediaKind::Image,
                "psd",
                None,
                &FormatTarget::Original,
                Some(w)
            ),
            Plan::Copy
        );
        // 変えない：エンジンが読める表の形式は写し、ほかは変える（Kura が書けても GIF・m4a・FLAC・AIFF は変える）
        for ext in ["png", "jpg", "jpeg", "webp"] {
            assert_eq!(
                plan(MediaKind::Image, ext, None, &pl, Some(w)),
                Plan::Copy,
                "{ext}"
            );
        }
        for ext in ["psd", "ase", "svg", "heic", "avif", "tif", "bmp", "gif"] {
            assert_eq!(
                plan(MediaKind::Image, ext, None, &pl, Some(w)),
                convert("png"),
                "{ext}"
            );
        }
        for ext in ["wav", "ogg", "mp3"] {
            assert_eq!(
                plan(MediaKind::Audio, ext, None, &pl, Some(w)),
                Plan::Copy,
                "{ext}"
            );
        }
        // 可逆は WAV、非可逆は Ogg（m4a はコーデックで見分ける）
        for ext in ["flac", "aiff"] {
            assert_eq!(
                plan(MediaKind::Audio, ext, None, &pl, Some(w)),
                convert("wav"),
                "{ext}"
            );
        }
        assert_eq!(
            plan(MediaKind::Audio, "m4a", Some("alac"), &pl, Some(w)),
            convert("wav")
        );
        assert_eq!(
            plan(MediaKind::Audio, "m4a", Some("aac"), &pl, Some(w)),
            convert("ogg")
        );
        assert_eq!(
            plan(MediaKind::Audio, "opus", None, &pl, Some(w)),
            convert("ogg")
        );
        // 変える先が書けなければもう一方
        let only_wav: Writable<'_> = &|f: &str| f == "wav";
        assert_eq!(
            plan(MediaKind::Audio, "opus", None, &pl, Some(only_wav)),
            convert("wav")
        );
        // 選んだ形式：同じ形式なら写す（jpg と jpeg は同じ）
        assert_eq!(
            plan(MediaKind::Image, "jpg", None, &to("jpeg"), Some(w)),
            Plan::Copy
        );
        assert_eq!(
            plan(MediaKind::Image, "png", None, &to("webp"), Some(w)),
            convert("webp")
        );
        assert_eq!(
            plan(MediaKind::Audio, "wav", None, &to("ogg"), Some(w)),
            convert("ogg")
        );
        // 動画・フォント・3D はいつも写す。エンジンの部品がなくても表で決める（変える 1 件は書くときに失敗する）
        for kind in [MediaKind::Video, MediaKind::Font, MediaKind::Model] {
            assert_eq!(plan(kind, "mp4", None, &to("png"), Some(w)), Plan::Copy);
        }
        assert_eq!(
            plan(MediaKind::Image, "psd", None, &pl, None),
            convert("png")
        );
        assert_eq!(plan(MediaKind::Image, "png", None, &pl, None), Plan::Copy);
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
