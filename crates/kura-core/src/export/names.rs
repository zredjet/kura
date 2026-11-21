//! 書き出しのファイル名（純粋関数）。M15 段 3 で名前の規則（テンプレート・書き方・ASCII だけにする。ADR-205）を足した。

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::import::format_id;
use crate::text::nfc;

/// テンプレートの語（M15 段 3）。
pub const TOKEN_TITLE: &str = "{名前}";
pub const TOKEN_ORIGINAL: &str = "{元の名前}";
pub const TOKEN_INDEX: &str = "{番号}";
pub const TOKEN_UNIT: &str = "{ユニット}";
pub const TOKEN_WIDTH: &str = "{幅}";
pub const TOKEN_HEIGHT: &str = "{高さ}";
/// テンプレートの語の一覧（画面の候補の順）。
pub const TOKENS: [&str; 6] = [
    TOKEN_TITLE,
    TOKEN_ORIGINAL,
    TOKEN_INDEX,
    TOKEN_UNIT,
    TOKEN_WIDTH,
    TOKEN_HEIGHT,
];
/// テンプレートの長さの上限（文字）。
pub const MAX_TEMPLATE_CHARS: usize = 200;

/// 名前の書き方。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NameStyle {
    /// そのまま。
    #[default]
    Keep,
    /// `hero_idle`
    Snake,
    /// `hero-idle`
    Kebab,
    /// `HeroIdle`
    Pascal,
    /// `heroIdle`
    Camel,
}

/// 名前の規則（M15 段 3。ADR-205 (1)）。既定は M9 と同じ（元のファイル名のまま）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NameRule {
    /// テンプレート（[`TOKENS`] の語を置き換える）。None なら元のファイル名（拡張子は今の中身・書く形式に合わせる）。
    pub template: Option<String>,
    pub style: NameStyle,
    /// ASCII の文字だけにする（ほかを除き、空になれば `item-<ID>`）。
    pub ascii: bool,
}

/// 名前を組むのに使うアイテムの値。
#[derive(Debug, Clone, Copy)]
pub struct NameParts<'a> {
    pub item_id: i64,
    pub title: &'a str,
    /// 元のファイル名（拡張子つき）。
    pub original_name: &'a str,
    /// 書き出す順（1 から）。
    pub index: usize,
    /// ユニットの中のアイテムなら、そのユニットの名前。
    pub unit: Option<&'a str>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

/// テンプレートを確かめる：空でなく、200 文字までで、`{…}` は知っている語だけ。
pub fn check_template(template: &str) -> Result<()> {
    if template.trim().is_empty() {
        return Err(CoreError::Invalid(
            "名前のテンプレートを入力してください".into(),
        ));
    }
    if template.chars().count() > MAX_TEMPLATE_CHARS {
        return Err(CoreError::Invalid(format!(
            "名前のテンプレートは {MAX_TEMPLATE_CHARS} 文字までです"
        )));
    }
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open..];
        let Some(close) = after.find('}') else {
            return Err(CoreError::Invalid(format!(
                "「{{」が閉じていません：{template}"
            )));
        };
        let token = &after[..=close];
        if !TOKENS.contains(&token) {
            return Err(CoreError::Invalid(format!(
                "知らない語です：{token}（使えるのは {}）",
                TOKENS.join("・")
            )));
        }
        rest = &after[close + 1..];
    }
    if rest.contains('}') {
        return Err(CoreError::Invalid(format!(
            "「}}」が開いていません：{template}"
        )));
    }
    Ok(())
}

/// テンプレートの語を置き換える（確かめたテンプレートを渡す）。
fn expand(template: &str, parts: &NameParts<'_>) -> String {
    let num = |v: Option<i64>| v.map_or(String::new(), |v| v.to_string());
    template
        .replace(TOKEN_TITLE, parts.title)
        .replace(TOKEN_ORIGINAL, split_ext(parts.original_name).0)
        .replace(TOKEN_INDEX, &format!("{:03}", parts.index))
        .replace(TOKEN_UNIT, parts.unit.unwrap_or(""))
        .replace(TOKEN_WIDTH, &num(parts.width))
        .replace(TOKEN_HEIGHT, &num(parts.height))
}

/// 語の区切りか（空白・`_`・`-`・`.`・全角の空白）。
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '_' | '-' | '.' | '\u{3000}')
}

/// 語に分ける（区切りの文字と、ASCII の大文字小文字の境目。`HTTPServer2` は `HTTP`・`Server2`）。
fn words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if is_separator(c) {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_ascii_uppercase() && !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 1 語の頭を大文字・残りを小文字にする。
fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
}

/// 書き方を当てる。
pub fn apply_style(text: &str, style: NameStyle) -> String {
    if style == NameStyle::Keep {
        return text.to_string();
    }
    let ws = words(text);
    match style {
        NameStyle::Keep => text.to_string(),
        NameStyle::Snake => ws
            .iter()
            .map(|w| w.to_lowercase())
            .collect::<Vec<_>>()
            .join("_"),
        NameStyle::Kebab => ws
            .iter()
            .map(|w| w.to_lowercase())
            .collect::<Vec<_>>()
            .join("-"),
        NameStyle::Pascal => ws.iter().map(|w| capitalize(w)).collect(),
        NameStyle::Camel => ws
            .iter()
            .enumerate()
            .map(|(i, w)| {
                if i == 0 {
                    w.to_lowercase()
                } else {
                    capitalize(w)
                }
            })
            .collect(),
    }
}

/// ASCII の文字だけにする（英数字と区切り・ASCII の記号を残す）。続いた区切りは 1 つにし、両端の区切りを除く。
fn ascii_only(text: &str) -> String {
    let kept: String = text
        .chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .collect();
    let mut out = String::new();
    let mut last_sep = false;
    for c in kept.chars() {
        let sep = is_separator(c);
        if sep && (out.is_empty() || last_sep) {
            continue;
        }
        out.push(c);
        last_sep = sep;
    }
    while out.ends_with(is_separator) {
        out.pop();
    }
    out
}

/// 名前の規則で語幹（拡張子なし）を作る。空になれば `item-<ID>` に書き方を当てたもの。
pub fn rule_stem(rule: &NameRule, parts: &NameParts<'_>) -> String {
    let raw = match &rule.template {
        Some(t) => expand(t, parts),
        None => split_ext(parts.original_name).0.to_string(),
    };
    let mut stem = apply_style(&nfc(raw.trim()), rule.style);
    if rule.ascii {
        stem = ascii_only(&stem);
    }
    let stem = stem.trim().to_string();
    if stem.is_empty() || stem.chars().all(is_separator) {
        apply_style(&format!("item-{}", parts.item_id), rule.style)
    } else {
        stem
    }
}

/// フォルダの名前（ユニット）に書き方と ASCII を当てる（テンプレートは当てない）。空になれば `fallback`。
pub fn rule_folder(rule: &NameRule, name: &str, fallback: &str) -> String {
    let mut out = apply_style(name, rule.style);
    if rule.ascii {
        out = ascii_only(&out);
    }
    if out.trim().is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

/// 名前の規則で書き出すファイル名を作る（`ext` は書く形式の拡張子）。規則がないとき（テンプレートなし・そのまま・ASCII でない）は
/// M9 の [`export_name`]（元の名前の拡張子が同じ形式ならそのまま）。
pub fn rule_name(rule: &NameRule, parts: &NameParts<'_>, ext: &str) -> String {
    let fallback = format!(
        "{}.{ext}",
        safe_name(parts.title, &format!("item-{}", parts.item_id))
    );
    if rule == &NameRule::default() {
        return safe_name(&export_name(parts.original_name, ext), &fallback);
    }
    safe_name(&format!("{}.{ext}", rule_stem(rule, parts)), &fallback)
}

/// APFS・HFS+ のファイル名の上限（バイト）。
const MAX_NAME_BYTES: usize = 255;

/// ファイル名に使えるようにする：`/`・`:`・制御文字を `_` にし、前後の空白を除き、
/// 先頭の `.`（隠しファイルになる）を `_` にする。空になったら `fallback`。
pub fn safe_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == ':' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    let out = match trimmed.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None if trimmed.is_empty() => fallback.to_string(),
        None => trimmed.to_string(),
    };
    truncate_bytes(&out, MAX_NAME_BYTES)
}

/// `n` 番目の候補：1 ならそのまま、2 以降は拡張子の前に「 2」（Finder と同じ）。
/// 名前が長すぎるときは、番号と拡張子が収まるよう名前の側を詰める。
pub fn numbered_file(name: &str, n: u32) -> String {
    let (stem, ext) = split_ext(name);
    let suffix = if n <= 1 {
        String::new()
    } else {
        format!(" {n}")
    };
    let tail = match ext {
        Some(ext) => format!("{suffix}.{ext}"),
        None => suffix,
    };
    let room = MAX_NAME_BYTES.saturating_sub(tail.len());
    format!("{}{tail}", truncate_bytes(stem, room))
}

/// 書き出すときの名前：元の名前の拡張子が今の中身（`items.ext`）と同じ形式なら元の名前のまま（`jpg` と `jpeg` は同じ。
/// 形式の表の ID で比べる）、違えば元の名前の語幹に今の拡張子を付ける（形式を変えて上書きしたあとも、元の名前は
/// 変えずに書き出しで直す。M12 の段 2 の決定 10）。拡張子のない名前・知らない拡張子も「違う」とみなす。
pub fn export_name(original_name: &str, ext: &str) -> String {
    let (stem, old) = split_ext(original_name);
    let ext = ext.trim();
    if ext.is_empty() || stem.trim().is_empty() {
        return original_name.to_string();
    }
    let same = old.is_some_and(|old| match (format_id(old), format_id(ext)) {
        (Some(a), Some(b)) => a == b,
        _ => old.eq_ignore_ascii_case(ext),
    });
    if same {
        original_name.to_string()
    } else {
        format!("{stem}.{ext}")
    }
}

/// フォルダの `n` 番目の候補（拡張子を区別しない。「鎌倉 2」）。
pub fn numbered_dir(name: &str, n: u32) -> String {
    if n <= 1 {
        return truncate_bytes(name, MAX_NAME_BYTES);
    }
    let suffix = format!(" {n}");
    let room = MAX_NAME_BYTES.saturating_sub(suffix.len());
    format!("{}{suffix}", truncate_bytes(name, room))
}

/// 名前と拡張子に分ける（先頭の `.` と、最後の `.` のあとが空のものは拡張子とみなさない）。
fn split_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], Some(&name[i + 1..])),
        _ => (name, None),
    }
}

/// 文字の途中で切らないよう、`max` バイト以内に詰める。
fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(title: &'a str, original: &'a str) -> NameParts<'a> {
        NameParts {
            item_id: 42,
            title,
            original_name: original,
            index: 7,
            unit: Some("鎌倉"),
            width: Some(64),
            height: Some(32),
        }
    }

    #[test]
    fn styles_split_words_by_separators_and_case() {
        let cases = [
            (
                "Hero Idle",
                "hero_idle",
                "hero-idle",
                "HeroIdle",
                "heroIdle",
            ),
            (
                "hero_idle-02",
                "hero_idle_02",
                "hero-idle-02",
                "HeroIdle02",
                "heroIdle02",
            ),
            (
                "HTTPServer2 run",
                "http_server2_run",
                "http-server2-run",
                "HttpServer2Run",
                "httpServer2Run",
            ),
            (
                "heroIdle.final",
                "hero_idle_final",
                "hero-idle-final",
                "HeroIdleFinal",
                "heroIdleFinal",
            ),
            (
                "夕方 の 空",
                "夕方_の_空",
                "夕方-の-空",
                "夕方の空",
                "夕方の空",
            ),
            ("Ａｂ　ｃ", "ａｂ_ｃ", "ａｂ-ｃ", "ＡｂＣ", "ａｂＣ"),
        ];
        for (src, snake, kebab, pascal, camel) in cases {
            assert_eq!(apply_style(src, NameStyle::Snake), snake, "{src}");
            assert_eq!(apply_style(src, NameStyle::Kebab), kebab, "{src}");
            assert_eq!(apply_style(src, NameStyle::Pascal), pascal, "{src}");
            assert_eq!(apply_style(src, NameStyle::Camel), camel, "{src}");
            assert_eq!(apply_style(src, NameStyle::Keep), src, "{src}");
        }
    }

    #[test]
    fn templates_expand_known_tokens_and_reject_others() {
        let rule = NameRule {
            template: Some("{ユニット}_{番号}_{名前}_{幅}x{高さ}".into()),
            style: NameStyle::Keep,
            ascii: false,
        };
        assert_eq!(
            rule_name(&rule, &parts("夕方", "IMG_1.HEIC"), "png"),
            "鎌倉_007_夕方_64x32.png"
        );
        let rule = NameRule {
            template: Some("{元の名前}".into()),
            style: NameStyle::Snake,
            ascii: false,
        };
        assert_eq!(
            rule_name(&rule, &parts("x", "HeroIdle.psd"), "png"),
            "hero_idle.png"
        );
        for bad in ["", "  ", "{名前", "名前}", "{色}", "{名前}{}"] {
            assert!(check_template(bad).is_err(), "{bad}");
        }
        for good in ["{名前}", "sprite_{番号}", "{ユニット}/{名前}"] {
            assert!(check_template(good).is_ok(), "{good}");
        }
        assert!(check_template(&"あ".repeat(MAX_TEMPLATE_CHARS + 1)).is_err());
    }

    #[test]
    fn ascii_drops_other_letters_and_falls_back_to_the_id() {
        let rule = NameRule {
            template: Some("{名前}".into()),
            style: NameStyle::Snake,
            ascii: true,
        };
        assert_eq!(
            rule_name(&rule, &parts("夕方 Sky 2", "a.png"), "png"),
            "sky_2.png"
        );
        assert_eq!(
            rule_name(&rule, &parts("夕方の空", "a.png"), "png"),
            "item_42.png"
        );
        let pascal = NameRule {
            style: NameStyle::Pascal,
            ..rule.clone()
        };
        assert_eq!(
            rule_name(&pascal, &parts("夕方", "a.png"), "png"),
            "Item42.png"
        );
        let keep = NameRule {
            template: None,
            style: NameStyle::Keep,
            ascii: true,
        };
        assert_eq!(
            rule_name(&keep, &parts("t", "夕方 hero.png"), "webp"),
            "hero.webp"
        );
        // 規則がないときは M9 と同じ（元の名前の拡張子が同じ形式ならそのまま）
        let m9 = NameRule::default();
        assert_eq!(
            rule_name(&m9, &parts("t", "IMG_0001.JPG"), "jpg"),
            "IMG_0001.JPG"
        );
        assert_eq!(rule_name(&m9, &parts("t", "hero.psd"), "png"), "hero.png");
        // 区切りの文字・隠しファイルは safe_name が直す
        let slash = NameRule {
            template: Some(".{名前}".into()),
            style: NameStyle::Keep,
            ascii: false,
        };
        assert_eq!(rule_name(&slash, &parts("a/b", "a.png"), "png"), "_a_b.png");
        assert_eq!(rule_folder(&rule, "鎌倉 Trip", "unit-1"), "trip");
        assert_eq!(rule_folder(&rule, "鎌倉", "unit-1"), "unit-1");
    }

    #[test]
    fn safe_name_replaces_separators_and_hidden_prefix() {
        assert_eq!(safe_name("旅行/鎌倉:夕方.jpg", "x"), "旅行_鎌倉_夕方.jpg");
        assert_eq!(safe_name("  .secret.png ", "x"), "_secret.png");
        assert_eq!(safe_name("a\u{0}b\nc.jpg", "x"), "a_b_c.jpg");
        assert_eq!(safe_name("   ", "item-3.jpg"), "item-3.jpg");
        assert_eq!(safe_name("", "item-3.jpg"), "item-3.jpg");
    }

    #[test]
    fn numbered_file_puts_the_number_before_the_extension() {
        assert_eq!(numbered_file("IMG_0001.JPG", 1), "IMG_0001.JPG");
        assert_eq!(numbered_file("IMG_0001.JPG", 2), "IMG_0001 2.JPG");
        assert_eq!(numbered_file("archive.tar.gz", 3), "archive.tar 3.gz");
        assert_eq!(numbered_file("README", 2), "README 2");
        assert_eq!(numbered_file("_secret", 2), "_secret 2");
    }

    #[test]
    fn export_name_keeps_the_name_when_the_format_is_the_same() {
        assert_eq!(export_name("IMG_0001.JPG", "jpg"), "IMG_0001.JPG");
        assert_eq!(export_name("photo.jpeg", "jpg"), "photo.jpeg");
        assert_eq!(export_name("anim.apng", "png"), "anim.apng");
        assert_eq!(export_name("scan.TIF", "tiff"), "scan.TIF");
    }

    #[test]
    fn export_name_uses_the_current_extension_when_the_format_changed() {
        // PSD を PNG で上書きしたあと（original_name は変えない）
        assert_eq!(export_name("hero.psd", "png"), "hero.png");
        assert_eq!(export_name("hero.png", "webp"), "hero.webp");
        assert_eq!(export_name("archive.v2.gif", "png"), "archive.v2.png");
        // 拡張子のない名前・知らない拡張子は今の拡張子を付ける
        assert_eq!(export_name("README", "png"), "README.png");
        assert_eq!(export_name("dump.dat", "png"), "dump.png");
        // 知らない拡張子どうしは文字で比べる
        assert_eq!(export_name("a.xyz", "XYZ"), "a.xyz");
    }

    #[test]
    fn export_name_leaves_empty_names_to_the_fallback() {
        assert_eq!(export_name("", "png"), "");
        assert_eq!(export_name("  ", "png"), "  ");
        assert_eq!(export_name("hero.png", ""), "hero.png");
    }

    #[test]
    fn long_names_stay_within_the_limit_without_breaking_characters() {
        let long = format!("{}.jpg", "あ".repeat(100));
        let first = numbered_file(&long, 1);
        assert!(
            first.len() <= MAX_NAME_BYTES && first.ends_with(".jpg"),
            "{first}"
        );
        let tenth = numbered_file(&long, 10);
        assert!(
            tenth.len() <= MAX_NAME_BYTES && tenth.ends_with(" 10.jpg"),
            "{tenth}"
        );
        let dir = numbered_dir(&"い".repeat(100), 2);
        assert!(dir.len() <= MAX_NAME_BYTES && dir.ends_with(" 2"), "{dir}");
        assert_eq!(numbered_dir("鎌倉", 1), "鎌倉");
        assert_eq!(numbered_dir("v1.2", 2), "v1.2 2");
    }
}
