//! ライセンスとクレジット（M15 段 1。docs/data-model.md §4「ライセンスとクレジット（M15）」、ADR-202・203）。
//!
//! - [`licenses`]：ライセンスの表（既定の行と利用者が足した行。表記義務・商用・改変・継承の印）。
//! - [`item`]：アイテムごとのクレジット（ライセンス・作者・作品・出典・表記の文）。まとめて付ける・別名の保存で写す。
//! - [`collect`]：使ったアイテムから来歴（別名の `item_derivations`・N → 1 の `item_sources`）をたどってクレジットを集め、
//!   同じ表記の行を 1 つにまとめる。

pub mod collect;
pub mod item;
pub mod licenses;
pub mod render;

use crate::error::{CoreError, Result};
use crate::text::nfc;

/// 文の項目の長さの上限（文字）。
pub const MAX_TEXT_CHARS: usize = 4_000;

/// 表記義務。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attribution {
    /// 必要（クレジットの一覧に必ず載る）。
    Required,
    /// 任意（推奨。載せるか選べる）。
    Optional,
    /// 不要。
    None,
}

impl Attribution {
    /// DB の値。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::None => "none",
        }
    }

    /// DB の値から。
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "required" => Ok(Self::Required),
            "optional" => Ok(Self::Optional),
            "none" => Ok(Self::None),
            other => Err(CoreError::Invalid(format!(
                "表記義務の値が違います: {other}"
            ))),
        }
    }
}

/// 商用・改変の可否。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permission {
    Yes,
    No,
    /// わからない（規約に書いていない・確かめていない）。
    Unknown,
}

impl Permission {
    /// DB の値。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unknown => "unknown",
        }
    }

    /// DB の値から。
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "yes" => Ok(Self::Yes),
            "no" => Ok(Self::No),
            "unknown" => Ok(Self::Unknown),
            other => Err(CoreError::Invalid(format!("可否の値が違います: {other}"))),
        }
    }
}

/// 文の項目を整える：前後の空白を除き NFC にし、空なら None。長すぎれば断る。
pub(crate) fn clean(label: &str, value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = nfc(value.trim());
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(CoreError::Invalid(format!(
            "{label}は {MAX_TEXT_CHARS} 文字までです"
        )));
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip() {
        for a in [
            Attribution::Required,
            Attribution::Optional,
            Attribution::None,
        ] {
            assert_eq!(Attribution::parse(a.as_str()).unwrap(), a);
        }
        for p in [Permission::Yes, Permission::No, Permission::Unknown] {
            assert_eq!(Permission::parse(p.as_str()).unwrap(), p);
        }
        assert!(Attribution::parse("maybe").is_err());
        assert!(Permission::parse("").is_err());
    }

    #[test]
    fn clean_trims_and_drops_empty_text() {
        assert_eq!(clean("作者", None).unwrap(), None);
        assert_eq!(clean("作者", Some("  ")).unwrap(), None);
        assert_eq!(
            clean("作者", Some(" もりの ")).unwrap().as_deref(),
            Some("もりの")
        );
        // NFC にする（濁点の合成）
        assert_eq!(
            clean("作者", Some("か\u{3099}")).unwrap().as_deref(),
            Some("が")
        );
        let long = "あ".repeat(MAX_TEXT_CHARS + 1);
        assert!(matches!(
            clean("作者", Some(&long)),
            Err(CoreError::Invalid(_))
        ));
        let fits = "あ".repeat(MAX_TEXT_CHARS);
        assert!(clean("作者", Some(&fits)).is_ok());
    }
}
