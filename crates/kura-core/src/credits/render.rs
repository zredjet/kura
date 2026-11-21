//! クレジットの一覧を文にする（M15 段 4。ADR-202 (2)・ADR-206）。[`super::collect::Credits`] を、ライセンスごとの見出しと
//! 表記の行、要確認の節（ライセンスが未設定・元のアイテムが消えた・記入が足りない）の、テキストか Markdown にする。言葉は
//! 日本語か英語（段 4 の決定 1）。表記が必要なものと要確認の節はいつも載り、任意・不要は選ぶ（段 4 の決定 4）。DB に触れない。

use std::collections::BTreeMap;

use super::Attribution;
use super::collect::{CreditKey, CreditLine, Credits};
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
        }
    }
}

/// 作った一覧。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    /// 載せた行の数。
    pub lines: usize,
    /// 載せなかった行の数（任意・不要）。
    pub omitted_optional: usize,
    pub omitted_none: usize,
    /// 要確認の件数（未設定・元が消えた・記入が足りない）。
    pub warnings: usize,
}

/// 言葉ごとの文。
struct Words {
    title: &'static str,
    review: &'static str,
    unset: &'static str,
    lost: &'static str,
    incomplete: &'static str,
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
            empty: "載せるクレジットはありません。",
        },
        Lang::En => Words {
            title: "Credits",
            review: "Needs review",
            unset: "License not set",
            lost: "Source item was deleted; provenance cannot be followed",
            incomplete: "Attribution required but author, work and credit text are missing",
            empty: "No credits to list.",
        },
    }
}

/// Markdown の記号を逃がす（行の頭の `#`・`-`・`>`・数字の後の `.` も含め、強調・リンク・表の記号）。
fn md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~' | '!'
        ) {
            out.push('\\');
        }
        // 改行は空白にする（1 行に収める）
        if c == '\n' || c == '\r' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    // 行の頭の `-`・`+`・`数字.` を箇条書きにしない
    if out.starts_with('-') || out.starts_with('+') {
        out.insert(0, '\\');
    }
    out
}

fn plain(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// 書式に合わせて利用者の文を整える。
fn esc(format: TextFormat, text: &str) -> String {
    match format {
        TextFormat::Markdown => md(text),
        TextFormat::Text => plain(text),
    }
}

/// URL（Markdown は <…>。`<`・`>`・空白を含む URL はそのまま逃がす）。
fn url(format: TextFormat, u: &str) -> String {
    match format {
        TextFormat::Markdown if !u.contains(['<', '>', ' ', '\n']) => format!("<{u}>"),
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
        // 何も記入がない（要確認にも出す）：見分けがつくよう出典もないことを書く
        out.push_str(match lang {
            Lang::Ja => "（作者・作品の記入なし）",
            Lang::En => "(no author or work given)",
        });
    }
    out
}

/// 素材の名前を、名前の自然な順で並べる（ないものは `#ID`）。
fn sorted_names(ids: &[i64], names: &BTreeMap<i64, String>) -> Vec<String> {
    let mut list: Vec<String> = ids
        .iter()
        .map(|id| names.get(id).cloned().unwrap_or_else(|| format!("#{id}")))
        .collect();
    list.sort_by(|a, b| natural_cmp(a, b));
    list.dedup();
    list
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
    // ライセンスごとに（credits.lines はライセンスの並び → 鍵の順）
    let mut groups: Vec<(&License, Vec<&CreditLine>)> = Vec::new();
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
        if license.attribution == Attribution::Required && incomplete(&line.key) {
            incomplete_items.extend(line.used.iter().copied());
        }
        if !include {
            match license.attribution {
                Attribution::Optional => out.omitted_optional += 1,
                _ => out.omitted_none += 1,
            }
            continue;
        }
        match groups.last_mut() {
            Some((l, lines)) if l.id == license.id => lines.push(line),
            _ => groups.push((license, vec![line])),
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
        match f {
            TextFormat::Markdown => text.push_str(&format!("## {}\n", md(&license.name))),
            TextFormat::Text => text.push_str(&format!("■ {}\n", plain(&license.name))),
        }
        if let Some(u) = &license.url {
            text.push_str(&url(f, u));
            text.push('\n');
        }
        text.push('\n');
        for line in lines {
            text.push_str(bullet);
            text.push_str(&credit_line(&line.key, opts.lang, f));
            text.push('\n');
            out.lines += 1;
            if opts.list_items {
                for name in sorted_names(&line.used, names) {
                    text.push_str(sub);
                    text.push_str(&esc(f, &name));
                    text.push('\n');
                }
            }
        }
    }
    // 要確認の節（いつも載せる。ADR-202 (2)）
    incomplete_items.sort_unstable();
    incomplete_items.dedup();
    let unset: Vec<i64> = credits.unset.iter().map(|u| u.item).collect();
    let lost: Vec<i64> = credits.lost.iter().map(|l| l.item).collect();
    let sections = [
        (w.unset, unset),
        (w.lost, lost),
        (w.incomplete, incomplete_items),
    ];
    if sections.iter().any(|(_, ids)| !ids.is_empty()) {
        text.push('\n');
        match f {
            TextFormat::Markdown => text.push_str(&format!("## {}\n", w.review)),
            TextFormat::Text => text.push_str(&format!("■ {}\n", w.review)),
        }
        for (label, ids) in sections {
            if ids.is_empty() {
                continue;
            }
            out.warnings += ids.len();
            text.push('\n');
            match f {
                TextFormat::Markdown => text.push_str(&format!("### {label}\n\n")),
                TextFormat::Text => text.push_str(&format!("{label}:\n")),
            }
            for name in sorted_names(&ids, names) {
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

/// 範囲の素材のクレジットを集めて一覧にする（プロジェクトなら見出しに名前を添える。`opts.title` があればそれ）。
pub fn list(
    conn: &rusqlite::Connection,
    scope: &Scope,
    opts: &RenderOptions,
) -> crate::Result<Rendered> {
    let (items, title) = match scope {
        Scope::Project(id) => {
            let p = crate::project::get(conn, *id)?;
            (crate::project::used_items(conn, *id)?, Some(p.name))
        }
        Scope::Items(ids) => (ids.clone(), None),
    };
    let credits = super::collect::collect(conn, &items)?;
    let mut ids: Vec<i64> = credits
        .lines
        .iter()
        .flat_map(|l| l.used.iter().copied())
        .collect();
    ids.extend(credits.unset.iter().map(|u| u.item));
    ids.extend(credits.lost.iter().map(|l| l.item));
    ids.sort_unstable();
    ids.dedup();
    let names = load_names(conn, &ids)?;
    let opts = RenderOptions {
        title: opts.title.clone().or(title),
        ..opts.clone()
    };
    Ok(render(&credits, &names, &opts))
}

/// 一覧をファイルに書く（同じフォルダの一時ファイルに書いてから名前を替える。保存のダイアログで選んだ所なので、あれば置き換える）。
/// ライブラリの中は断る。
pub fn write(
    path: &std::path::Path,
    text: &str,
    library_root: &std::path::Path,
) -> crate::Result<()> {
    use crate::error::CoreError;
    use std::io::Write;
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
    let temp = dir_c.join(format!(".{name}.kura-part"));
    let written = (|| -> crate::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
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
    use crate::credits::collect::Flagged;

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
            "「\\*Bold\\* \\[link\\](x) \\#1」 \\- dash next（https://e.com/a b）"
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
             INSERT INTO projects (id, name, root, engine, pos, created_at, updated_at)
               VALUES (5, 'My Game', '/g', 'godot', 1, 0, 0);
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
    }
}
