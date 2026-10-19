//! ⌘K パレットの候補（docs/search-query.md §6）：入力中の語に一致するタグ → フィールド名 → 保存済みの検索。
//! `フィールド:` まで入力していれば、そのフィールドの値（種類・取得元・フォルダ・ユニットなど）。

use rusqlite::Connection;

use super::fields::{FIELDS, FieldDef, FieldKind, KIND_WORDS, find_field};
use super::parser::split_op;
use crate::{Result, folders, smart, tags, unit};

/// 候補の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Tag,
    Field,
    Value,
    /// 保存済みの検索（スマートフォルダ）。
    Saved,
}

/// 候補 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    /// 入力中の語と置き換える文字列（保存済みの検索は、その条件）。
    pub insert: String,
    /// 前に薄く出す部分（名前空間・フィールド名と記号）。
    pub prefix: String,
    pub label: String,
    /// 補足（保存済みの検索の条件など）。
    pub detail: Option<String>,
    /// 件数（タグの使われている数など）。
    pub count: Option<i64>,
    /// 保存済みの検索の ID。
    pub id: Option<i64>,
}

impl Suggestion {
    fn new(kind: SuggestionKind, insert: String, prefix: &str, label: String) -> Self {
        Self {
            kind,
            insert,
            prefix: prefix.to_string(),
            label,
            detail: None,
            count: None,
            id: None,
        }
    }
}

/// 空白や記号を含む値は引用符で囲む。
/// タグ 1 つを検索の条件にする（「作者:KR」「"夕方の 海"」。タグの一覧から絞り込むとき）。
pub fn tag_clause(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        quote_if_needed(name)
    } else {
        format!("{namespace}:{}", quote_if_needed(name))
    }
}

fn quote_if_needed(value: &str) -> String {
    if value.starts_with('-')
        || value.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '(' | ')' | '|' | '"' | ':' | '：' | '=' | '<' | '>' | '!'
                )
        })
    {
        format!("\"{}\"", value.replace('"', ""))
    } else {
        value.to_string()
    }
}

fn normalize(word: &str) -> String {
    word.chars()
        .map(|c| match c {
            '：' => ':',
            '＝' => '=',
            '＞' => '>',
            '＜' => '<',
            '！' => '!',
            '－' => '-',
            other => other,
        })
        .collect()
}

/// 入力中の語（`word`）の候補。空なら保存済みの検索だけ。
pub fn suggest(conn: &Connection, word: &str, limit: usize) -> Result<Vec<Suggestion>> {
    let word = normalize(word.trim());
    let (minus, body) = match word.strip_prefix('-') {
        Some(rest) => ("-", rest.to_string()),
        None => ("", word.clone()),
    };
    let mut out = Vec::new();
    if body.is_empty() {
        out.extend(saved(conn, "", limit)?);
        return Ok(out);
    }

    if let Some((left, op, right)) = split_op(&body)
        && let Some(def) = find_field(left)
    {
        let prefix = format!("{minus}{left}{op}");
        out.extend(values(conn, def, &prefix, right, limit)?);
        return Ok(out);
    }

    // タグ（名前空間付きの入力も tags::suggest が扱う）
    for s in tags::suggest(conn, &body, limit)? {
        let (prefix, insert) = if s.tag.namespace.is_empty() {
            (
                String::new(),
                format!("{minus}{}", quote_if_needed(&s.tag.name)),
            )
        } else {
            (
                format!("{}:", s.tag.namespace),
                format!(
                    "{minus}{}:{}",
                    s.tag.namespace,
                    quote_if_needed(&s.tag.name)
                ),
            )
        };
        let mut suggestion = Suggestion::new(SuggestionKind::Tag, insert, &prefix, s.tag.name);
        suggestion.count = Some(s.count);
        suggestion.detail = s.alias;
        out.push(suggestion);
    }

    // フィールド名（日本語・英語の前方一致）
    if !body.contains(':') {
        let lower = body.to_lowercase();
        for def in FIELDS {
            if let Some(name) = def
                .names
                .iter()
                .find(|n| n.starts_with(lower.as_str()) || n.starts_with(body.as_str()))
            {
                out.push(Suggestion::new(
                    SuggestionKind::Field,
                    format!("{minus}{name}:"),
                    "",
                    format!("{name}:"),
                ));
            }
        }
    }

    out.extend(saved(conn, &body, limit)?);
    Ok(out)
}

/// 保存済みの検索（名前か条件に入力を含むもの）。
fn saved(conn: &Connection, text: &str, limit: usize) -> Result<Vec<Suggestion>> {
    let lower = text.to_lowercase();
    Ok(smart::list(conn)?
        .into_iter()
        .filter(|s| {
            lower.is_empty()
                || s.name.to_lowercase().contains(&lower)
                || s.query.to_lowercase().contains(&lower)
        })
        .take(limit)
        .map(|s| {
            let mut suggestion =
                Suggestion::new(SuggestionKind::Saved, s.query.clone(), "", s.name);
            suggestion.detail = Some(s.query);
            suggestion.id = Some(s.id);
            suggestion
        })
        .collect())
}

/// フィールドの値の候補。`typed` は記号の後ろに入力済みの部分。
fn values(
    conn: &Connection,
    def: &FieldDef,
    prefix: &str,
    typed: &str,
    limit: usize,
) -> Result<Vec<Suggestion>> {
    // 種類は「,」で複数。最後の 1 つを候補で補う
    let (head, last) = match (def.kind, typed.rfind(',')) {
        (FieldKind::Type, Some(i)) => (&typed[..=i], &typed[i + 1..]),
        _ => ("", typed),
    };
    let lower = last.trim().to_lowercase();
    let fixed = |words: &[&str]| -> Vec<(String, Option<i64>)> {
        words.iter().map(|w| ((*w).to_string(), None)).collect()
    };
    let candidates: Vec<(String, Option<i64>)> = match def.kind {
        FieldKind::Type => fixed(&KIND_WORDS.map(|(word, _)| word)),
        FieldKind::Played => fixed(&["未", "7日以内", "30日以上前"]),
        FieldKind::Added | FieldKind::Taken => {
            fixed(&["今日", "昨日", "7日以内", "30日以内", "30日以上前"])
        }
        FieldKind::Orient => fixed(&["縦", "横", "正方形"]),
        FieldKind::Rating => fixed(&["5", "4", "3", "2", "1", "0"]),
        FieldKind::Source => {
            let mut stmt = conn.prepare_cached(
                "SELECT source_site, count(*) FROM items WHERE trashed_at IS NULL AND source_site IS NOT NULL
                 GROUP BY source_site ORDER BY count(*) DESC, source_site",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, Some(r.get::<_, i64>(1)?)))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        }
        FieldKind::Folder => {
            let tree = folders::tree(conn)?;
            let mut paths: Vec<String> = tree
                .iter()
                .map(|n| folders::path_of(&tree, n.id).join("/"))
                .collect();
            paths.sort();
            paths.into_iter().map(|p| (p, None)).collect()
        }
        FieldKind::Unit => {
            let mut list = fixed(&["あり", "なし"]);
            list.extend(
                unit::list(conn, last.trim(), None, limit)?
                    .into_iter()
                    .map(|u| (u.name, None)),
            );
            list
        }
        _ => Vec::new(),
    };
    let mut seen = Vec::new();
    Ok(candidates
        .into_iter()
        .filter(|(value, _)| lower.is_empty() || value.to_lowercase().contains(&lower))
        .filter(|(value, _)| {
            let new = !seen.contains(value);
            if new {
                seen.push(value.clone());
            }
            new
        })
        .take(limit)
        .map(|(value, count)| {
            let mut s = Suggestion::new(
                SuggestionKind::Value,
                format!("{prefix}{head}{}", quote_if_needed(&value)),
                prefix,
                value,
            );
            s.count = count;
            s
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    fn inserts(conn: &Connection, word: &str) -> Vec<(SuggestionKind, String)> {
        suggest(conn, word, 8)
            .unwrap()
            .into_iter()
            .map(|s| (s.kind, s.insert))
            .collect()
    }

    fn library() -> Connection {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, source_site, imported_at, updated_at)
               VALUES (1, x'01', 'jpg', 'image', 1, 'a', 'a', 'pixiv', 0, 0),
                      (2, x'02', 'jpg', 'image', 1, 'b', 'b', 'pixiv', 0, 0),
                      (3, x'03', 'jpg', 'image', 1, 'c', 'c', 'youtube', 0, 0);
             INSERT INTO tags (id, namespace, name, created_at) VALUES (1, '作者', 'もりの', 0), (2, '', '森', 0), (3, '', '夜 景', 0);
             INSERT INTO item_tags (tag_id, item_id) VALUES (1, 1), (1, 2), (2, 3);
             INSERT INTO folders (id, parent_id, name, pos, created_at) VALUES (1, NULL, '写真', 1, 0), (2, 1, '2026 旅行', 1, 0);
             INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (1, 'set', '鎌倉', 'manual', 0, 0);",
        )
        .unwrap();
        crate::smart::save(&conn, "夜景の背景資料", "夜景 種類:画像", 0).unwrap();
        conn
    }

    #[test]
    fn suggests_tags_then_fields_then_saved_searches() {
        let conn = library();
        assert_eq!(
            inserts(&conn, "も"),
            vec![(SuggestionKind::Tag, "作者:もりの".to_string())]
        );
        assert_eq!(
            inserts(&conn, "作者:も"),
            vec![(SuggestionKind::Tag, "作者:もりの".to_string())]
        );
        // 除外の「-」は付けたまま、空白を含むタグは引用符で囲む
        assert_eq!(
            inserts(&conn, "-夜"),
            vec![
                (SuggestionKind::Tag, "-\"夜 景\"".to_string()),
                (SuggestionKind::Saved, "夜景 種類:画像".to_string())
            ]
        );
        // フィールド名
        let fields = inserts(&conn, "種");
        assert!(
            fields.contains(&(SuggestionKind::Field, "種類:".to_string())),
            "{fields:?}"
        );
        let fields = inserts(&conn, "ty");
        assert!(
            fields.contains(&(SuggestionKind::Field, "type:".to_string())),
            "{fields:?}"
        );
        // 空なら保存済みの検索
        let saved = suggest(&conn, "", 8).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, Some(1));
    }

    #[test]
    fn suggests_field_values() {
        let conn = library();
        assert_eq!(
            inserts(&conn, "種類:"),
            vec![
                (SuggestionKind::Value, "種類:画像".to_string()),
                (SuggestionKind::Value, "種類:動画".to_string()),
                (SuggestionKind::Value, "種類:音楽".to_string()),
                (SuggestionKind::Value, "種類:フォント".to_string()),
                (SuggestionKind::Value, "種類:3D".to_string())
            ]
        );
        assert_eq!(
            inserts(&conn, "種類:画像,動"),
            vec![(SuggestionKind::Value, "種類:画像,動画".to_string())]
        );
        assert_eq!(
            inserts(&conn, "種類:フォ"),
            vec![(SuggestionKind::Value, "種類:フォント".to_string())]
        );
        assert_eq!(
            inserts(&conn, "種類:画像,3d"),
            vec![(SuggestionKind::Value, "種類:画像,3D".to_string())]
        );
        let sources = suggest(&conn, "取得元：", 8).unwrap();
        assert_eq!(
            sources
                .iter()
                .map(|s| (s.insert.as_str(), s.count))
                .collect::<Vec<_>>(),
            vec![("取得元:pixiv", Some(2)), ("取得元:youtube", Some(1))]
        );
        assert_eq!(
            inserts(&conn, "フォルダ:旅"),
            vec![(
                SuggestionKind::Value,
                "フォルダ:\"写真/2026 旅行\"".to_string()
            )]
        );
        assert_eq!(
            inserts(&conn, "-ユニット:鎌"),
            vec![(SuggestionKind::Value, "-ユニット:鎌倉".to_string())]
        );
        assert_eq!(inserts(&conn, "名前:"), vec![]);
    }

    #[test]
    fn tag_clause_quotes_names_the_parser_would_misread() {
        assert_eq!(tag_clause("", "夕景"), "夕景");
        assert_eq!(tag_clause("作者", "KR"), "作者:KR");
        assert_eq!(tag_clause("", "夕方の 海"), "\"夕方の 海\"");
        assert_eq!(tag_clause("", "-逆"), "\"-逆\"");
        assert_eq!(tag_clause("", "評価:5"), "\"評価:5\"");
        // 名前空間付きなら、記号や空白を含む名前でもそのタグ 1 つの条件として読める
        for name in ["a b", "Re:ゼロ", "-逆", "x=y"] {
            let clause = tag_clause("作者", name);
            match crate::query::parse(&clause).unwrap().expr {
                crate::query::Expr::Tag(term) => {
                    assert_eq!(
                        (term.ns.as_str(), term.name.as_str()),
                        ("作者", name),
                        "{clause}"
                    );
                }
                other => panic!("{clause} はタグの条件ではない: {other:?}"),
            }
        }
        // 名前空間なしの普通の名前はそのまま語になる。記号や空白を含む名前は引用符で囲み、
        // 解析は通る（引用符の語はいまは全文検索として読む）
        assert!(matches!(
            crate::query::parse(&tag_clause("", "夕景")).unwrap().expr,
            crate::query::Expr::Tag(_)
        ));
        for name in ["夕方の 海", "Re:ゼロ", "-逆", "x=y"] {
            assert!(crate::query::parse(&tag_clause("", name)).is_ok(), "{name}");
        }
    }
}
