//! クレジットの一覧を文にする（M15 段 4。ADR-202 (2)・ADR-206）。[`super::collect::Credits`] を、ライセンスごとの見出しと
//! 表記の行、要確認の節（ライセンスが未設定・元のアイテムが消えた・記入が足りない・プロジェクトを照らし終えていない）の、
//! テキストか Markdown にする。言葉は日本語か英語（段 4 の決定 1）。表記が必要なものと要確認の節はいつも載り、任意・不要は
//! 選ぶ（段 4 の決定 4）。同じライセンスの中で文が同じ行は 1 つにまとめる。DB に触れない（[`list`] と [`write`] を除く）。

use std::collections::BTreeMap;

use super::Attribution;
use super::collect::{CreditKey, Credits};
use super::licenses::License;
use crate::text::natural_cmp;

/// 一覧の言葉。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    Ja,
    En,
}

/// 一覧の書式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextFormat {
    #[default]
    Markdown,
    Text,
}

/// プロジェクトの照らし直しの様子（一覧が欠けているかもしれないとき。段 4 の検証の直し・利用者の決定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectNote {
    /// まだ一度も照らし終えていない。
    NotScanned,
    /// 最後の照らしでフォルダが見つからなかった（前の記録で作った）。
    Missing,
}

/// 一覧の作り方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOptions {
    pub lang: Lang,
    pub format: TextFormat,
    /// 表記が任意のライセンスの素材も載せる（既定で載せる）。
    pub include_optional: bool,
    /// 表記が不要のライセンスの素材も載せる（既定で載せない）。
    pub include_none: bool,
    /// 行の下に、その表記を使っている素材の名前を並べる（既定で並べない）。
    pub list_items: bool,
    /// 見出しに添える名前（プロジェクトの名前など）。
    pub title: Option<String>,
    /// プロジェクトの照らし直しの様子（[`list`] が入れる）。
    pub project: Option<ProjectNote>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            lang: Lang::Ja,
            format: TextFormat::Markdown,
            include_optional: true,
            include_none: false,
            list_items: false,
            title: None,
            project: None,
        }
    }
}

/// 作った一覧。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    /// 載せた行の数（同じ文の行をまとめたあと）。
    pub lines: usize,
    /// 載せなかった行の数（任意・不要）。
    pub omitted_optional: usize,
    pub omitted_none: usize,
    /// 要確認の件数（未設定・元が消えた・記入が足りないの素材の数と、プロジェクトの照らしの 1 件）。
    pub warnings: usize,
}

/// 言葉ごとの文。
struct Words {
    title: &'static str,
    review: &'static str,
    unset: &'static str,
    lost: &'static str,
    incomplete: &'static str,
    not_scanned: &'static str,
    missing: &'static str,
    empty: &'static str,
}

fn words(lang: Lang) -> Words {
    match lang {
        Lang::Ja => Words {
            title: "クレジット",
            review: "要確認",
            unset: "ライセンスが未設定",
            lost: "元のアイテムが消えて、来歴をたどれない",
            incomplete: "表記が必要なのに、作者・作品・表記の文がない",
            not_scanned: "プロジェクトをまだ照らし終えていないので、使っている素材が欠けていることがあります",
            missing: "プロジェクトのフォルダが見つからないので、前の記録で作りました",
            empty: "載せるクレジットはありません。",
        },
        Lang::En => Words {
            title: "Credits",
            review: "Needs review",
            unset: "License not set",
            lost: "Source item was deleted; provenance cannot be followed",
            incomplete: "Attribution required but author, work and credit text are missing",
            not_scanned: "The project has not been scanned yet; some assets in use may be missing",
            missing: "The project folder was not found; this list uses the last scan",
            empty: "No credits to list.",
        },
    }
}

/// ライセンスの見出しの名前（既定の行のうち日本語の名前のものは、英語の一覧では英語の名前。段 4 の検証の直し・利用者の決定）。
fn license_name(license: &License, lang: Lang) -> &str {
    match (lang, license.builtin.as_deref()) {
        (Lang::En, Some("public-domain")) => "Public domain",
        (Lang::En, Some("own")) => "Original work",
        _ => &license.name,
    }
}

/// Markdown の記号を逃がす（強調・リンク・表・HTML の記号と、行の頭の `-`・`+`・`数字.`・`数字)`。改行・制御文字は空白）。
fn md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.push(' ');
            continue;
        }
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~' | '!'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    // 行の頭の `#`（見出し）・`-`・`+` と `数字.`・`数字)` を見出し・箇条書きにしない
    if out.starts_with('-') || out.starts_with('+') || out.starts_with('#') {
        out.insert(0, '\\');
    }
    let digits = out.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) && matches!(out[digits..].chars().next(), Some('.' | ')')) {
        out.insert(digits, '\\');
    }
    out
}

fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// 書式に合わせて利用者の文を整える。
fn esc(format: TextFormat, text: &str) -> String {
    match format {
        TextFormat::Markdown => md(text),
        TextFormat::Text => plain(text),
    }
}

/// CommonMark の自動リンクになる絶対 URI か（スキーム `[A-Za-z][A-Za-z0-9+.-]{1,31}:` で始まり、空白・制御文字・`<`・`>` がない）。
fn is_absolute_uri(u: &str) -> bool {
    let Some((scheme, _)) = u.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    let starts = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    starts
        && (2..=32).contains(&scheme.len())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        && !u
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>')
}

/// URL（Markdown は絶対 URI のときだけ <…>。ほかは逃がした文）。
fn url(format: TextFormat, u: &str) -> String {
    match format {
        TextFormat::Markdown if is_absolute_uri(u) => format!("<{u}>"),
        _ => esc(format, u),
    }
}

/// 作者・作品・表記の文のどれもない（表記が必要なら記入が足りない）。
fn incomplete(key: &CreditKey) -> bool {
    key.credit.is_none() && key.author.is_none() && key.work.is_none()
}

/// 1 行の表記（表記の文があればそのまま、なければ作品・作者・出典から組む。ライセンスは見出しにある）。
pub fn credit_line(key: &CreditKey, lang: Lang, format: TextFormat) -> String {
    if let Some(text) = &key.credit {
        return esc(format, text);
    }
    let work = key.work.as_deref().map(|w| esc(format, w));
    let author = key.author.as_deref().map(|a| esc(format, a));
    let link = key.url.as_deref().map(|u| url(format, u));
    let mut out = String::new();
    match lang {
        Lang::Ja => {
            if let Some(w) = &work {
                out.push_str(&format!("「{w}」"));
            }
            if let Some(a) = &author {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(a);
            }
            if let Some(l) = &link {
                out.push_str(&format!("（{l}）"));
            }
        }
        Lang::En => {
            if let Some(w) = &work {
                out.push_str(&format!("\"{w}\""));
            }
            if let Some(a) = &author {
                if out.is_empty() {
                    out.push_str(a);
                } else {
                    out.push_str(&format!(" by {a}"));
                }
            }
            if let Some(l) = &link {
                if out.is_empty() {
                    out.push_str(l);
                } else {
                    out.push_str(&format!(" ({l})"));
                }
            }
        }
    }
    if out.is_empty() {
        // 何も記入がない（要確認にも出す）
        out.push_str(match lang {
            Lang::Ja => "（作者・作品の記入なし）",
            Lang::En => "(no author or work given)",
        });
    }
    out
}

/// 素材の名前を、名前の自然な順で並べる（ID ごとに 1 行。名前が同じ別のアイテムは `（#ID）` を添えて見分ける。ないものは `#ID`）。
fn sorted_names(ids: &[i64], names: &BTreeMap<i64, String>, lang: Lang) -> Vec<String> {
    let mut ids: Vec<i64> = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut list: Vec<(String, i64)> = ids
        .iter()
        .map(|id| {
            (
                names.get(id).cloned().unwrap_or_else(|| format!("#{id}")),
                *id,
            )
        })
        .collect();
    list.sort_by(|a, b| natural_cmp(&a.0, &b.0).then(a.1.cmp(&b.1)));
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (name, _) in &list {
        *counts.entry(name.clone()).or_default() += 1;
    }
    list.into_iter()
        .map(|(name, id)| {
            if counts.get(&name).copied().unwrap_or(0) > 1 {
                match lang {
                    Lang::Ja => format!("{name}（#{id}）"),
                    Lang::En => format!("{name} (#{id})"),
                }
            } else {
                name
            }
        })
        .collect()
}

/// まとめた 1 行（同じライセンスの中で文が同じもの）。
struct Merged {
    text: String,
    used: Vec<i64>,
}

/// 一覧を作る。`names` は素材の表示名（使った素材・要確認の素材）。
pub fn render(credits: &Credits, names: &BTreeMap<i64, String>, opts: &RenderOptions) -> Rendered {
    let w = words(opts.lang);
    let f = opts.format;
    let mut out = Rendered::default();
    let mut text = String::new();
    // 見出し
    let title = match &opts.title {
        Some(t) => format!("{} — {}", w.title, esc(f, t)),
        None => w.title.to_string(),
    };
    match f {
        TextFormat::Markdown => text.push_str(&format!("# {title}\n")),
        TextFormat::Text => text.push_str(&format!("{title}\n")),
    }
    let licenses: BTreeMap<i64, &License> = credits.licenses.iter().map(|l| (l.id, l)).collect();
    // ライセンスごとに（credits.lines はライセンスの並び → 鍵の順）、同じ文の行は 1 つにまとめる
    let mut groups: Vec<(&License, Vec<Merged>)> = Vec::new();
    let mut incomplete_items: Vec<i64> = Vec::new();
    for line in &credits.lines {
        let Some(license) = licenses.get(&line.key.license_id) else {
            continue;
        };
        let include = match license.attribution {
            Attribution::Required => true,
            Attribution::Optional => opts.include_optional,
            Attribution::None => opts.include_none,
        };
        // 記入が足りないのは、そのクレジットを持つアイテム（来歴の元を含む。直す相手）
        if license.attribution == Attribution::Required && incomplete(&line.key) {
            incomplete_items.extend(line.items.iter().copied());
        }
        if !include {
            match license.attribution {
                Attribution::Optional => out.omitted_optional += 1,
                _ => out.omitted_none += 1,
            }
            continue;
        }
        let rendered = credit_line(&line.key, opts.lang, f);
        if groups.last().is_none_or(|(l, _)| l.id != license.id) {
            groups.push((license, Vec::new()));
        }
        if let Some((_, merged)) = groups.last_mut() {
            match merged.iter_mut().find(|m| m.text == rendered) {
                Some(m) => m.used.extend(line.used.iter().copied()),
                None => merged.push(Merged {
                    text: rendered,
                    used: line.used.clone(),
                }),
            }
        }
    }
    let bullet = match f {
        TextFormat::Markdown => "- ",
        TextFormat::Text => "・",
    };
    let sub = match f {
        TextFormat::Markdown => "  - ",
        TextFormat::Text => "    ",
    };
    if groups.is_empty() {
        text.push('\n');
        text.push_str(w.empty);
        text.push('\n');
    }
    for (license, lines) in &groups {
        text.push('\n');
        let name = license_name(license, opts.lang);
        match f {
            TextFormat::Markdown => text.push_str(&format!("## {}\n", md(name))),
            TextFormat::Text => text.push_str(&format!("■ {}\n", plain(name))),
        }
        if let Some(u) = &license.url {
            text.push_str(&url(f, u));
            text.push('\n');
        }
        text.push('\n');
        for line in lines {
            text.push_str(bullet);
            text.push_str(&line.text);
            text.push('\n');
            out.lines += 1;
            if opts.list_items {
                for name in sorted_names(&line.used, names, opts.lang) {
                    text.push_str(sub);
                    text.push_str(&esc(f, &name));
                    text.push('\n');
                }
            }
        }
    }
    // 要確認の節（いつも載せる。ADR-202 (2)）
    let unset: Vec<i64> = credits.unset.iter().map(|u| u.item).collect();
    let lost: Vec<i64> = credits.lost.iter().map(|l| l.item).collect();
    let sections = [
        (w.unset, unset),
        (w.lost, lost),
        (w.incomplete, incomplete_items),
    ];
    let note = opts.project.map(|n| match n {
        ProjectNote::NotScanned => w.not_scanned,
        ProjectNote::Missing => w.missing,
    });
    if note.is_some() || sections.iter().any(|(_, ids)| !ids.is_empty()) {
        text.push('\n');
        match f {
            TextFormat::Markdown => text.push_str(&format!("## {}\n", w.review)),
            TextFormat::Text => text.push_str(&format!("■ {}\n", w.review)),
        }
        if let Some(note) = note {
            out.warnings += 1;
            text.push('\n');
            text.push_str(note);
            text.push('\n');
        }
        for (label, ids) in sections {
            let list = sorted_names(&ids, names, opts.lang);
            if list.is_empty() {
                continue;
            }
            out.warnings += list.len();
            text.push('\n');
            match f {
                TextFormat::Markdown => text.push_str(&format!("### {label}\n\n")),
                TextFormat::Text => text.push_str(&format!("{label}:\n")),
            }
            for name in list {
                text.push_str(bullet);
                text.push_str(&esc(f, &name));
                text.push('\n');
            }
        }
    }
    out.text = text;
    out
}

/// 一覧の範囲。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// 登録したプロジェクトで今使っている素材（見出しにプロジェクトの名前）。
    Project(i64),
    /// 選んだアイテム（ユニットはメンバーに広げて渡す）。
    Items(Vec<i64>),
}

/// 素材の表示名を読む（ないアイテムは入れない）。
fn load_names(conn: &rusqlite::Connection, ids: &[i64]) -> crate::Result<BTreeMap<i64, String>> {
    let mut stmt = conn.prepare_cached("SELECT title FROM items WHERE id = ?1")?;
    let mut out = BTreeMap::new();
    for &id in ids {
        use rusqlite::OptionalExtension;
        if let Some(title) = stmt.query_row([id], |r| r.get::<_, String>(0)).optional()? {
            out.insert(id, title);
        }
    }
    Ok(out)
}

/// 1 件のアイテムの表記の行（来歴の元をたどる。テキストで、インスペクタの見本。未設定・元が消えたものは載らない。M15 段 6）。
pub fn preview(conn: &rusqlite::Connection, item: i64, lang: Lang) -> crate::Result<Vec<String>> {
    let credits = super::collect::collect(conn, &[item])?;
    Ok(credits
        .lines
        .iter()
        .map(|l| credit_line(&l.key, lang, TextFormat::Text))
        .collect())
}

/// 範囲の素材のクレジットを集めて一覧にする（プロジェクトなら見出しに名前を添え、照らし終えていない・フォルダが見つからない
/// ときは要確認に出す。`opts.title` があればそれ）。
pub fn list(
    conn: &rusqlite::Connection,
    scope: &Scope,
    opts: &RenderOptions,
) -> crate::Result<Rendered> {
    let (items, title, note) = match scope {
        Scope::Project(id) => {
            let p = crate::project::get(conn, *id)?;
            let note = if p.missing {
                Some(ProjectNote::Missing)
            } else if p.scanned_at.is_none() {
                Some(ProjectNote::NotScanned)
            } else {
                None
            };
            (crate::project::used_items(conn, *id)?, Some(p.name), note)
        }
        Scope::Items(ids) => (ids.clone(), None, None),
    };
    let credits = super::collect::collect(conn, &items)?;
    let mut ids: Vec<i64> = credits
        .lines
        .iter()
        .flat_map(|l| l.used.iter().chain(l.items.iter()).copied())
        .collect();
    ids.extend(credits.unset.iter().map(|u| u.item));
    ids.extend(credits.lost.iter().map(|l| l.item));
    ids.sort_unstable();
    ids.dedup();
    let names = load_names(conn, &ids)?;
    let opts = RenderOptions {
        title: opts.title.clone().or(title),
        project: note,
        ..opts.clone()
    };
    Ok(render(&credits, &names, &opts))
}

/// 一覧をファイルに書く（同じフォルダの毎回違う名前の一時ファイルを新しく作って書いてから名前を替える。保存のダイアログで
/// 選んだ所なので、あれば置き換える）。ライブラリの中は断る。
pub fn write(
    path: &std::path::Path,
    text: &str,
    library_root: &std::path::Path,
) -> crate::Result<()> {
    use crate::error::CoreError;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().filter(|d| d.is_dir()).ok_or_else(|| {
        CoreError::Invalid(format!(
            "書き出し先のフォルダがありません: {}",
            path.display()
        ))
    })?;
    let dir_c = std::fs::canonicalize(dir)?;
    let lib = std::fs::canonicalize(library_root).unwrap_or_else(|_| library_root.to_path_buf());
    if crate::project::within(&lib, &dir_c) {
        return Err(CoreError::Invalid(
            "ライブラリの中には書き出せません。ほかのフォルダを選んでください".into(),
        ));
    }
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        CoreError::Invalid(format!("ファイルの名前を読めません: {}", path.display()))
    })?;
    // 一時ファイルは短い決まった形の名前（長い名前でも上限を超えない）で、すでにあるものには書かない（create_new）
    let (file, temp) = (|| -> crate::Result<(std::fs::File, std::path::PathBuf)> {
        for _ in 0..64 {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let temp = dir_c.join(format!(".kura-credits-{}-{n}.part", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
            {
                Ok(f) => return Ok((f, temp)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(CoreError::Invalid("一時ファイルを作れません".into()))
    })()?;
    let written = (|| -> crate::Result<()> {
        let mut file = file;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, dir_c.join(name))?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credits::Permission;
    use crate::credits::collect::{CreditLine, Flagged};

    fn license(id: i64, name: &str, url: Option<&str>, attribution: Attribution) -> License {
        License {
            id,
            builtin: None,
            name: name.into(),
            spdx: None,
            url: url.map(str::to_string),
            attribution,
            commercial: Permission::Yes,
            modify: Permission::Yes,
            share_alike: false,
            note: None,
            pos: id as f64,
            items: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn key(license: i64, work: Option<&str>, author: Option<&str>, url: Option<&str>) -> CreditKey {
        CreditKey {
            license_id: license,
            work: work.map(str::to_string),
            author: author.map(str::to_string),
            url: url.map(str::to_string),
            credit: None,
        }
    }

    fn line(key: CreditKey, used: &[i64]) -> CreditLine {
        CreditLine {
            key,
            items: used.to_vec(),
            used: used.to_vec(),
        }
    }

    fn sample() -> (Credits, BTreeMap<i64, String>) {
        let credits = Credits {
            lines: vec![
                line(
                    key(
                        1,
                        Some("森の素材集"),
                        Some("もりの"),
                        Some("https://example.com/pack"),
                    ),
                    &[2, 1],
                ),
                line(
                    CreditKey {
                        credit: Some("BGM: 魔王魂".into()),
                        ..key(1, None, None, None)
                    },
                    &[3],
                ),
                line(key(1, None, None, None), &[4]),
                line(
                    key(2, None, Some("Kenney"), Some("https://kenney.nl")),
                    &[5],
                ),
                line(key(3, Some("Free SFX"), None, None), &[6]),
            ],
            licenses: vec![
                license(
                    1,
                    "CC BY 4.0",
                    Some("https://creativecommons.org/licenses/by/4.0/"),
                    Attribution::Required,
                ),
                license(2, "CC0 1.0", None, Attribution::None),
                license(3, "サイトの規約", None, Attribution::Optional),
            ],
            unset: vec![Flagged {
                item: 7,
                used: vec![7],
            }],
            lost: vec![Flagged {
                item: 8,
                used: vec![8],
            }],
        };
        let names = [
            (1, "hero.png"),
            (2, "enemy.png"),
            (3, "theme.ogg"),
            (4, "tile.png"),
            (5, "coin.png"),
            (6, "jump.wav"),
            (7, "unknown.png"),
            (8, "sheet.png"),
        ]
        .into_iter()
        .map(|(id, n)| (id, n.to_string()))
        .collect();
        (credits, names)
    }

    #[test]
    fn japanese_markdown_groups_by_license_and_lists_warnings() {
        let (credits, names) = sample();
        let opts = RenderOptions {
            title: Some("My Game".into()),
            ..RenderOptions::default()
        };
        let got = render(&credits, &names, &opts);
        assert_eq!(
            got.text,
            "# クレジット — My Game

## CC BY 4.0
<https://creativecommons.org/licenses/by/4.0/>

- 「森の素材集」 もりの（<https://example.com/pack>）
- BGM: 魔王魂
- （作者・作品の記入なし）

## サイトの規約

- 「Free SFX」

## 要確認

### ライセンスが未設定

- unknown.png

### 元のアイテムが消えて、来歴をたどれない

- sheet.png

### 表記が必要なのに、作者・作品・表記の文がない

- tile.png
"
        );
        assert_eq!(
            (
                got.lines,
                got.omitted_optional,
                got.omitted_none,
                got.warnings
            ),
            (4, 0, 1, 3)
        );
    }

    #[test]
    fn english_text_with_items_and_filters() {
        let (credits, names) = sample();
        let opts = RenderOptions {
            lang: Lang::En,
            format: TextFormat::Text,
            include_optional: false,
            include_none: true,
            list_items: true,
            title: None,
            project: None,
        };
        let got = render(&credits, &names, &opts);
        assert_eq!(
            got.text,
            "Credits

■ CC BY 4.0
https://creativecommons.org/licenses/by/4.0/

・\"森の素材集\" by もりの (https://example.com/pack)
    enemy.png
    hero.png
・BGM: 魔王魂
    theme.ogg
・(no author or work given)
    tile.png

■ CC0 1.0

・Kenney (https://kenney.nl)
    coin.png

■ Needs review

License not set:
・unknown.png

Source item was deleted; provenance cannot be followed:
・sheet.png

Attribution required but author, work and credit text are missing:
・tile.png
"
        );
        assert_eq!(
            (got.lines, got.omitted_optional, got.omitted_none),
            (4, 1, 0)
        );
    }

    #[test]
    fn markdown_escapes_user_text_and_empty_lists_say_so() {
        let k = CreditKey {
            license_id: 1,
            work: Some("*Bold* [link](x) #1".into()),
            author: Some("- dash\nnext".into()),
            url: Some("https://e.com/a b".into()),
            credit: None,
        };
        assert_eq!(
            credit_line(&k, Lang::Ja, TextFormat::Markdown),
            "「\\*Bold\\* \\[link\\](x) #1」 \\- dash next（https://e.com/a b）"
        );
        let empty = render(
            &Credits::default(),
            &BTreeMap::new(),
            &RenderOptions::default(),
        );
        assert_eq!(
            empty.text,
            "# クレジット\n\n載せるクレジットはありません。\n"
        );
        assert_eq!(empty.lines, 0);
        // 行の頭の数字と点・かっこは番号付きの箇条書きにしない。制御文字は空白
        assert_eq!(md("01. Opening"), "01\\. Opening");
        assert_eq!(md("1) BGM"), "1\\) BGM");
        assert_eq!(md("2024年"), "2024年");
        assert_eq!(md("a\rb\tc"), "a b c");
        // URL の形でない出典は <…> で囲まない
        let k = CreditKey {
            license_id: 1,
            work: None,
            author: Some("もりの".into()),
            url: Some("OpenGameArt".into()),
            credit: None,
        };
        assert_eq!(
            credit_line(&k, Lang::Ja, TextFormat::Markdown),
            "もりの（OpenGameArt）"
        );
        let k = CreditKey {
            url: Some("itch.io/foo".into()),
            ..k
        };
        assert_eq!(
            credit_line(&k, Lang::Ja, TextFormat::Markdown),
            "もりの（itch.io/foo）"
        );
        assert!(is_absolute_uri("https://example.com/a?b=c"));
        assert!(is_absolute_uri("mailto:a@example.com"));
        assert!(!is_absolute_uri("example.com"));
        assert!(!is_absolute_uri("a:b"));
    }

    #[test]
    fn same_text_lines_merge_and_items_with_the_same_name_are_told_apart() {
        // 表記の文が同じで出典だけが違う行は 1 行。記入の足りないクレジットは持ち主（来歴の元）を名指す
        let credits = Credits {
            lines: vec![
                line(
                    CreditKey {
                        credit: Some("BGM: 魔王魂".into()),
                        ..key(1, None, None, Some("https://a"))
                    },
                    &[1],
                ),
                line(
                    CreditKey {
                        credit: Some("BGM: 魔王魂".into()),
                        ..key(1, None, None, Some("https://b"))
                    },
                    &[2],
                ),
                CreditLine {
                    key: key(1, None, None, None),
                    items: vec![10],
                    used: vec![11],
                },
            ],
            licenses: vec![license(1, "CC BY 4.0", None, Attribution::Required)],
            unset: vec![
                Flagged {
                    item: 3,
                    used: vec![3],
                },
                Flagged {
                    item: 4,
                    used: vec![4],
                },
            ],
            lost: vec![],
        };
        let names: BTreeMap<i64, String> = [
            (1, "a"),
            (2, "b"),
            (3, "icon"),
            (4, "icon"),
            (10, "元"),
            (11, "派生"),
        ]
        .into_iter()
        .map(|(id, n)| (id, n.to_string()))
        .collect();
        let opts = RenderOptions {
            list_items: true,
            ..RenderOptions::default()
        };
        let got = render(&credits, &names, &opts);
        assert_eq!(
            got.text.matches("- BGM: 魔王魂\n").count(),
            1,
            "{}",
            got.text
        );
        assert!(
            got.text.contains("- BGM: 魔王魂\n  - a\n  - b\n"),
            "{}",
            got.text
        );
        assert_eq!(got.lines, 2);
        assert!(
            got.text.contains("- icon（#3）\n- icon（#4）\n"),
            "{}",
            got.text
        );
        assert!(
            got.text
                .contains("### 表記が必要なのに、作者・作品・表記の文がない\n\n- 元\n"),
            "{}",
            got.text
        );
        assert_eq!(got.warnings, 3);
    }

    #[test]
    fn english_lists_use_english_builtin_names_and_note_the_project_scan() {
        let mut own = license(1, "自作", None, Attribution::None);
        own.builtin = Some("own".into());
        let mut pd = license(2, "パブリックドメイン", None, Attribution::None);
        pd.builtin = Some("public-domain".into());
        let credits = Credits {
            lines: vec![
                line(key(1, None, Some("me"), None), &[1]),
                line(key(2, Some("Old"), None, None), &[2]),
            ],
            licenses: vec![own, pd],
            unset: vec![],
            lost: vec![],
        };
        let opts = RenderOptions {
            lang: Lang::En,
            include_none: true,
            project: Some(ProjectNote::NotScanned),
            ..RenderOptions::default()
        };
        let got = render(&credits, &BTreeMap::new(), &opts);
        assert!(got.text.contains("## Original work\n"), "{}", got.text);
        assert!(got.text.contains("## Public domain\n"), "{}", got.text);
        assert!(
            got.text
                .contains("## Needs review\n\nThe project has not been scanned yet"),
            "{}",
            got.text
        );
        assert_eq!(got.warnings, 1);
        let ja = render(
            &credits,
            &BTreeMap::new(),
            &RenderOptions {
                include_none: true,
                project: Some(ProjectNote::Missing),
                ..RenderOptions::default()
            },
        );
        assert!(ja.text.contains("## 自作\n"), "{}", ja.text);
        assert!(
            ja.text.contains("プロジェクトのフォルダが見つからない"),
            "{}",
            ja.text
        );
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::credits::item::{self, CreditPatch};
    use crate::db::test_conn;

    #[test]
    fn list_reads_the_scope_and_names() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
               VALUES (1, x'01', 'png', 'image', 1, 'hero', 'hero.png', 0, 0),
                      (2, x'02', 'png', 'image', 1, 'tile', 'tile.png', 0, 0);
             INSERT INTO projects (id, name, root, engine, pos, scanned_at, created_at, updated_at)
               VALUES (5, 'My Game', '/g', 'godot', 1, 1, 0, 0);
             INSERT INTO project_files (project_id, path, size_bytes, mtime_ns, hash, item_id, matched, seen_at)
               VALUES (5, 'a.png', 1, 0, x'01', 1, 'current', 1);",
        )
        .unwrap();
        let by: i64 = conn
            .query_row(
                "SELECT id FROM licenses WHERE builtin = 'cc-by-4.0'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        item::set(
            &conn,
            &[1],
            &CreditPatch {
                license: Some(Some(by)),
                author: Some(Some("もりの".into())),
                ..CreditPatch::default()
            },
            1,
        )
        .unwrap();
        let opts = RenderOptions {
            list_items: true,
            ..RenderOptions::default()
        };
        let got = list(&conn, &Scope::Project(5), &opts).unwrap();
        assert!(
            got.text.starts_with("# クレジット — My Game\n"),
            "{}",
            got.text
        );
        assert!(got.text.contains("- もりの\n  - hero\n"), "{}", got.text);
        assert_eq!((got.lines, got.warnings), (1, 0));
        // 選んだアイテム：未設定の素材は要確認に出る
        let got = list(&conn, &Scope::Items(vec![1, 2]), &RenderOptions::default()).unwrap();
        assert!(
            got.text.contains("### ライセンスが未設定\n\n- tile\n"),
            "{}",
            got.text
        );
        assert_eq!(got.warnings, 1);
        assert!(list(&conn, &Scope::Project(99), &opts).is_err());
    }

    /// 完了条件：プロジェクトが使う派生のアイテム（別名で写したクレジットと、N → 1 のシート）から、来歴の元の表記が必要な
    /// クレジットまで一覧に載る。照らし終えていないプロジェクトは要確認に出る。
    #[test]
    fn project_lists_reach_required_credits_through_provenance() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
               VALUES (1, x'01', 'png', 'image', 1, 'A', 'a.png', 0, 0),
                      (2, x'02', 'png', 'image', 1, 'B', 'b.png', 0, 0),
                      (3, x'03', 'png', 'image', 1, 'C', 'c.png', 0, 0),
                      (4, x'04', 'png', 'image', 1, 'sheet', 'sheet.png', 0, 0);
             INSERT INTO item_derivations (item_id, source_item_id, source_version, source_hash, created_at)
               VALUES (2, 1, 0, x'01', 1);
             INSERT INTO item_sources (item_id, ord, source_item_id, source_version, source_hash, frames, created_at)
               VALUES (4, 0, 3, 0, x'03', 1, 1);
             INSERT INTO projects (id, name, root, engine, pos, created_at, updated_at)
               VALUES (5, 'G', '/g', 'godot', 1, 0, 0);
             INSERT INTO project_files (project_id, path, size_bytes, mtime_ns, hash, item_id, matched, seen_at)
               VALUES (5, 'b.png', 1, 0, x'02', 2, 'current', 1), (5, 'sheet.png', 1, 0, x'04', 4, 'current', 1);",
        )
        .unwrap();
        let by: i64 = conn
            .query_row(
                "SELECT id FROM licenses WHERE builtin = 'cc-by-4.0'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        for (id, author) in [(1, "もりの"), (3, "ケニー")] {
            item::set(
                &conn,
                &[id],
                &CreditPatch {
                    license: Some(Some(by)),
                    author: Some(Some(author.into())),
                    ..CreditPatch::default()
                },
                1,
            )
            .unwrap();
        }
        item::copy(&conn, 1, 2, 2).unwrap();
        let got = list(&conn, &Scope::Project(5), &RenderOptions::default()).unwrap();
        assert!(got.text.contains("- もりの\n"), "{}", got.text);
        assert!(got.text.contains("- ケニー\n"), "{}", got.text);
        assert_eq!(got.lines, 2);
        // まだ照らし終えていない
        assert!(
            got.text.contains("プロジェクトをまだ照らし終えていない"),
            "{}",
            got.text
        );
        assert_eq!(got.warnings, 1);
        conn.execute("UPDATE projects SET scanned_at = 9 WHERE id = 5", [])
            .unwrap();
        let got = list(&conn, &Scope::Project(5), &RenderOptions::default()).unwrap();
        assert_eq!(got.warnings, 0, "{}", got.text);
    }

    #[test]
    fn write_replaces_the_file_and_refuses_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("L.kuralib");
        std::fs::create_dir_all(lib.join("files")).unwrap();
        let out = dir.path().join("CREDITS.md");
        write(&out, "one", &lib).unwrap();
        write(&out, "two", &lib).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "two");
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "L.kuralib")
            .collect();
        assert_eq!(names, vec!["CREDITS.md"]);
        assert!(write(&lib.join("files/CREDITS.md"), "x", &lib).is_err());
        assert!(write(&dir.path().join("none/CREDITS.md"), "x", &lib).is_err());
        // 長い名前（255 バイト近く）も一時ファイルの名前が上限を超えない
        let long = dir.path().join(format!("{}.md", "あ".repeat(82)));
        write(&long, "long", &lib).unwrap();
        assert_eq!(std::fs::read_to_string(&long).unwrap(), "long");
        // 並んで書いても混ざらない（どちらかの中身そのまま）
        let both = dir.path().join("BOTH.md");
        let (a, b) = ("a".repeat(100_000), "b".repeat(100_000));
        std::thread::scope(|s| {
            s.spawn(|| write(&both, &a, &lib).unwrap());
            s.spawn(|| write(&both, &b, &lib).unwrap());
        });
        let got = std::fs::read_to_string(&both).unwrap();
        assert!(got == a || got == b);
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::credits::item::{self, CreditPatch};
    use crate::db::test_conn;

    /// インスペクタの見本：表記の行をテキストで（未設定は空）。
    #[test]
    fn preview_composes_the_line_or_nothing_when_unset() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, source_url)
               VALUES (1, x'01', 'png', 'image', 1, 'hero', 'hero.png', 0, 0, 'https://example.com/hero'),
                      (2, x'02', 'png', 'image', 1, 'tile', 'tile.png', 0, 0, NULL);",
        )
        .unwrap();
        assert!(preview(&conn, 2, Lang::Ja).unwrap().is_empty());
        let by: i64 = conn
            .query_row(
                "SELECT id FROM licenses WHERE builtin = 'cc-by-4.0'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        item::set(
            &conn,
            &[1],
            &CreditPatch {
                license: Some(Some(by)),
                author: Some(Some("もりの".into())),
                work: Some(Some("森の素材集".into())),
                ..CreditPatch::default()
            },
            1,
        )
        .unwrap();
        assert_eq!(
            preview(&conn, 1, Lang::Ja).unwrap(),
            ["「森の素材集」 もりの（https://example.com/hero）"]
        );
        assert_eq!(
            preview(&conn, 1, Lang::En).unwrap(),
            ["\"森の素材集\" by もりの (https://example.com/hero)"]
        );
    }
}
