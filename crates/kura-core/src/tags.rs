//! タグ（docs/data-model.md「タグ」）：名前の読み取り、候補、付け外し、編集（名前空間・色・別名・親子）、統合、削除。

use std::cmp::Ordering;
use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::folders::check_color;
use crate::text::{natural_cmp, nfc};
use crate::{CoreError, Result};

/// 候補を数える件数がこれより多ければ、タグごとではなくまとめて数える。
const COUNT_ONE_BY_ONE: usize = 300;

/// タグの名前（名前空間なしは空文字）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TagName {
    pub namespace: String,
    pub name: String,
}

impl TagName {
    /// 名前空間なしのタグ。
    pub fn plain(name: impl Into<String>) -> Self {
        Self {
            namespace: String::new(),
            name: name.into(),
        }
    }

    /// 入力どおりの表記（「名前」または「名前空間:名前」。別名の照合に使う）。
    pub fn display(&self) -> String {
        display(&self.namespace, &self.name)
    }
}

fn display(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}:{name}")
    }
}

fn is_colon(c: char) -> bool {
    c == ':' || c == '：'
}

/// 「名前」または「名前空間:名前」を読む（全角の「：」も区切りとして扱う）。空なら None。
pub fn parse_tag(input: &str) -> Option<TagName> {
    let text = nfc(input.trim());
    let (namespace, name) = match text.char_indices().find(|(_, c)| is_colon(*c)) {
        Some((i, c)) => (
            text.get(..i).unwrap_or_default().trim(),
            text.get(i + c.len_utf8()..).unwrap_or_default().trim(),
        ),
        None => ("", text.as_str()),
    };
    if name.is_empty() {
        return None;
    }
    Some(TagName {
        namespace: namespace.to_string(),
        name: name.to_string(),
    })
}

/// タグの ID を返す（別名なら正式タグ。なければ作る）。
pub fn ensure_tag(conn: &Connection, tag: &TagName, now: i64) -> Result<i64> {
    if let Some(id) = alias_target(conn, &tag.display())? {
        return Ok(id);
    }
    if let Some(id) = find(conn, &tag.namespace, &tag.name)? {
        return Ok(id);
    }
    let mut stmt =
        conn.prepare_cached("INSERT INTO tags (namespace, name, created_at) VALUES (?1, ?2, ?3)")?;
    stmt.execute(params![tag.namespace, tag.name, now])?;
    Ok(conn.last_insert_rowid())
}

fn alias_target(conn: &Connection, alias: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached("SELECT tag_id FROM tag_aliases WHERE alias = ?1")?;
    Ok(stmt.query_row([alias], |row| row.get(0)).optional()?)
}

fn find(conn: &Connection, namespace: &str, name: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached("SELECT id FROM tags WHERE namespace = ?1 AND name = ?2")?;
    Ok(stmt
        .query_row(params![namespace, name], |row| row.get(0))
        .optional()?)
}

fn ensure_exists(conn: &Connection, id: i64) -> Result<()> {
    let mut stmt = conn.prepare_cached("SELECT 1 FROM tags WHERE id = ?1")?;
    if stmt.exists([id])? {
        Ok(())
    } else {
        Err(CoreError::NotFound(format!("タグ {id}")))
    }
}

/// タグ（名前空間付き）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRef {
    pub id: i64,
    pub namespace: String,
    pub name: String,
    pub color: Option<String>,
}

/// 候補 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSuggestion {
    pub tag: TagRef,
    /// 使われている数（ゴミ箱のアイテムも含む）。
    pub count: i64,
    /// 別名で一致したときの別名。
    pub alias: Option<String>,
}

/// 入力に合うタグの候補（前方一致 → 部分一致、それぞれ使われている数の多い順）。空の入力ならよく使うタグ。
/// 「名前空間:名前」の形なら、名前空間が一致するものの中から名前で探す。
pub fn suggest(conn: &Connection, input: &str, limit: usize) -> Result<Vec<TagSuggestion>> {
    let text = nfc(input.trim()).to_lowercase();
    let (ns_query, name_query) = match text.char_indices().find(|(_, c)| is_colon(*c)) {
        Some((i, c)) => (
            Some(text.get(..i).unwrap_or_default().trim().to_string()),
            text.get(i + c.len_utf8()..)
                .unwrap_or_default()
                .trim()
                .to_string(),
        ),
        None => (None, text.clone()),
    };

    let tags: Vec<TagRef> = {
        let mut stmt = conn.prepare_cached("SELECT id, namespace, name, color FROM tags")?;
        let rows = stmt.query_map([], tag_ref_from_row)?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let aliases: Vec<(String, i64)> = {
        let mut stmt = conn.prepare_cached("SELECT alias, tag_id FROM tag_aliases")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    // 一致の度合い（0 = 前方一致、1 = 部分一致）
    let rank_of = |namespace: &str, name: &str| -> Option<u8> {
        let ns = namespace.to_lowercase();
        let name = name.to_lowercase();
        match &ns_query {
            Some(q) => {
                if ns != *q {
                    None
                } else if name.starts_with(name_query.as_str()) {
                    Some(0)
                } else if name.contains(name_query.as_str()) {
                    Some(1)
                } else {
                    None
                }
            }
            None => {
                let full = display(&ns, &name);
                if name.starts_with(name_query.as_str()) || full.starts_with(name_query.as_str()) {
                    Some(0)
                } else if full.contains(name_query.as_str()) {
                    Some(1)
                } else {
                    None
                }
            }
        }
    };

    let by_id: HashMap<i64, &TagRef> = tags.iter().map(|t| (t.id, t)).collect();
    let mut matched: HashMap<i64, (u8, Option<String>)> = HashMap::new();
    for tag in &tags {
        if let Some(rank) = rank_of(&tag.namespace, &tag.name) {
            matched.insert(tag.id, (rank, None));
        }
    }
    for (alias, tag_id) in &aliases {
        let Some(parsed) = parse_tag(alias) else {
            continue;
        };
        if let Some(rank) = rank_of(&parsed.namespace, &parsed.name) {
            let entry = matched
                .entry(*tag_id)
                .or_insert((rank, Some(alias.clone())));
            if rank < entry.0 {
                *entry = (rank, Some(alias.clone()));
            }
        }
    }

    let counts = count_uses(conn, matched.keys().copied().collect())?;
    let mut out: Vec<(u8, TagSuggestion)> = matched
        .into_iter()
        .filter_map(|(id, (rank, alias))| {
            let tag = (*by_id.get(&id)?).clone();
            Some((
                rank,
                TagSuggestion {
                    tag,
                    count: counts.get(&id).copied().unwrap_or(0),
                    alias,
                },
            ))
        })
        .collect();
    out.sort_by(|(ra, a), (rb, b)| {
        ra.cmp(rb)
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| natural_cmp(&a.tag.namespace, &b.tag.namespace))
            .then_with(|| natural_cmp(&a.tag.name, &b.tag.name))
            .then_with(|| a.tag.id.cmp(&b.tag.id))
    });
    out.truncate(limit);
    Ok(out.into_iter().map(|(_, s)| s).collect())
}

fn tag_ref_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TagRef> {
    Ok(TagRef {
        id: row.get(0)?,
        namespace: row.get(1)?,
        name: row.get(2)?,
        color: row.get(3)?,
    })
}

/// タグごとの使われている数。
fn count_uses(conn: &Connection, ids: Vec<i64>) -> Result<HashMap<i64, i64>> {
    let mut counts = HashMap::with_capacity(ids.len());
    if ids.len() <= COUNT_ONE_BY_ONE {
        let mut stmt = conn.prepare_cached("SELECT count(*) FROM item_tags WHERE tag_id = ?1")?;
        for id in ids {
            counts.insert(id, stmt.query_row([id], |row| row.get(0))?);
        }
    } else {
        let mut stmt =
            conn.prepare_cached("SELECT tag_id, count(*) FROM item_tags GROUP BY tag_id")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        for row in rows {
            let (id, n) = row?;
            counts.insert(id, n);
        }
    }
    Ok(counts)
}

/// 入力した名前のタグを付け（なければ作る）、`remove` のタグを外す。付けたタグの ID を返す。
pub fn apply(
    conn: &Connection,
    item_ids: &[i64],
    add: &[String],
    remove: &[i64],
    now: i64,
) -> Result<Vec<i64>> {
    let added = ensure_all(conn, add, now)?;
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO item_tags (tag_id, item_id, source) SELECT ?1, id, 'user' FROM items WHERE id = ?2",
    )?;
    let mut delete =
        conn.prepare_cached("DELETE FROM item_tags WHERE tag_id = ?1 AND item_id = ?2")?;
    for item in item_ids {
        for tag in &added {
            insert.execute([tag, item])?;
        }
        for tag in remove {
            delete.execute([tag, item])?;
        }
    }
    Ok(added)
}

/// ユニット自体にタグを付け外しする。付けたタグの ID を返す。
pub fn apply_to_units(
    conn: &Connection,
    unit_ids: &[i64],
    add: &[String],
    remove: &[i64],
    now: i64,
) -> Result<Vec<i64>> {
    let added = ensure_all(conn, add, now)?;
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO unit_tags (tag_id, unit_id) SELECT ?1, id FROM units WHERE id = ?2",
    )?;
    let mut delete =
        conn.prepare_cached("DELETE FROM unit_tags WHERE tag_id = ?1 AND unit_id = ?2")?;
    for unit in unit_ids {
        for tag in &added {
            insert.execute([tag, unit])?;
        }
        for tag in remove {
            delete.execute([tag, unit])?;
        }
    }
    Ok(added)
}

fn ensure_all(conn: &Connection, names: &[String], now: i64) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    for name in names {
        let tag = parse_tag(name)
            .ok_or_else(|| CoreError::Invalid("タグの名前を入力してください".into()))?;
        let id = ensure_tag(conn, &tag, now)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// タグの編集画面に出す内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagDetail {
    pub tag: TagRef,
    pub count: i64,
    pub aliases: Vec<String>,
    pub parents: Vec<TagRef>,
    pub children: Vec<TagRef>,
}

/// 1 つのタグの詳細。
pub fn detail(conn: &Connection, id: i64) -> Result<TagDetail> {
    let tag = {
        let mut stmt =
            conn.prepare_cached("SELECT id, namespace, name, color FROM tags WHERE id = ?1")?;
        stmt.query_row([id], tag_ref_from_row).optional()?
    };
    let Some(tag) = tag else {
        return Err(CoreError::NotFound(format!("タグ {id}")));
    };
    let count = count_uses(conn, vec![id])?.get(&id).copied().unwrap_or(0);
    let aliases = {
        let mut stmt =
            conn.prepare_cached("SELECT alias FROM tag_aliases WHERE tag_id = ?1 ORDER BY alias")?;
        let rows = stmt.query_map([id], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<String>, _>>()?
    };
    let related = |sql: &str| -> Result<Vec<TagRef>> {
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt.query_map([id], tag_ref_from_row)?;
        let mut tags = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        tags.sort_by(|a, b| {
            natural_cmp(&a.namespace, &b.namespace).then_with(|| natural_cmp(&a.name, &b.name))
        });
        Ok(tags)
    };
    let parents = related(
        "SELECT t.id, t.namespace, t.name, t.color FROM tag_parents p JOIN tags t ON t.id = p.parent_id WHERE p.child_id = ?1",
    )?;
    let children = related(
        "SELECT t.id, t.namespace, t.name, t.color FROM tag_parents p JOIN tags t ON t.id = p.child_id WHERE p.parent_id = ?1",
    )?;
    Ok(TagDetail {
        tag,
        count,
        aliases,
        parents,
        children,
    })
}

/// 変更する項目（None は変えない）。`aliases`・`parents` は渡したもので置き換える。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagPatch {
    pub namespace: Option<String>,
    pub name: Option<String>,
    pub color: Option<Option<String>>,
    pub aliases: Option<Vec<String>>,
    pub parents: Option<Vec<i64>>,
}

/// 名前空間・名前・色・別名・親タグを変える。
pub fn update(conn: &Connection, id: i64, patch: &TagPatch) -> Result<()> {
    let current = detail(conn, id)?.tag;

    if patch.namespace.is_some() || patch.name.is_some() {
        let namespace = nfc(patch
            .namespace
            .as_deref()
            .unwrap_or(&current.namespace)
            .trim());
        let name = nfc(patch.name.as_deref().unwrap_or(&current.name).trim());
        if name.is_empty() {
            return Err(CoreError::Invalid("タグの名前を入力してください".into()));
        }
        if namespace.chars().any(is_colon) {
            return Err(CoreError::Invalid("名前空間に「:」は使えません".into()));
        }
        if let Some(other) = find(conn, &namespace, &name)?
            && other != id
        {
            return Err(CoreError::Conflict(format!(
                "「{}」というタグがあります。まとめるときは「ほかのタグに統合」を使ってください",
                display(&namespace, &name)
            )));
        }
        let new_display = display(&namespace, &name);
        match alias_target(conn, &new_display)? {
            Some(owner) if owner != id => {
                return Err(CoreError::Conflict(format!(
                    "「{new_display}」はほかのタグの別名です"
                )));
            }
            // 自分の別名だった表記を正式名にしたら、別名からは外す
            Some(_) => {
                let mut stmt = conn.prepare_cached("DELETE FROM tag_aliases WHERE alias = ?1")?;
                stmt.execute([&new_display])?;
            }
            None => {}
        }
        let mut stmt =
            conn.prepare_cached("UPDATE tags SET namespace = ?2, name = ?3 WHERE id = ?1")?;
        stmt.execute(params![id, namespace, name])?;
    }

    if let Some(color) = &patch.color {
        let color = check_color(color.as_deref())?;
        let mut stmt = conn.prepare_cached("UPDATE tags SET color = ?2 WHERE id = ?1")?;
        stmt.execute(params![id, color])?;
    }

    if let Some(aliases) = &patch.aliases {
        let mut stmt = conn.prepare_cached("DELETE FROM tag_aliases WHERE tag_id = ?1")?;
        stmt.execute([id])?;
        for alias in aliases {
            let Some(parsed) = parse_tag(alias) else {
                continue;
            };
            let alias = parsed.display();
            if find(conn, &parsed.namespace, &parsed.name)?.is_some() {
                return Err(CoreError::Conflict(format!(
                    "「{alias}」というタグがあるので別名にできません"
                )));
            }
            if let Some(owner) = alias_target(conn, &alias)? {
                if owner == id {
                    continue;
                }
                return Err(CoreError::Conflict(format!(
                    "「{alias}」はほかのタグの別名です"
                )));
            }
            let mut stmt =
                conn.prepare_cached("INSERT INTO tag_aliases (alias, tag_id) VALUES (?1, ?2)")?;
            stmt.execute(params![alias, id])?;
        }
    }

    if let Some(parents) = &patch.parents {
        let below = descendants(conn, id)?;
        for parent in parents {
            ensure_exists(conn, *parent)?;
            if *parent == id || below.contains(parent) {
                return Err(CoreError::Invalid(
                    "自分自身や子孫のタグは親にできません".into(),
                ));
            }
        }
        let mut stmt = conn.prepare_cached("DELETE FROM tag_parents WHERE child_id = ?1")?;
        stmt.execute([id])?;
        let mut stmt = conn.prepare_cached(
            "INSERT OR IGNORE INTO tag_parents (child_id, parent_id) VALUES (?1, ?2)",
        )?;
        for parent in parents {
            stmt.execute([id, *parent])?;
        }
    }
    Ok(())
}

/// 子孫のタグ（子・孫…）の ID。
pub fn descendants(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE d(id) AS (
           SELECT child_id FROM tag_parents WHERE parent_id = ?1
           UNION
           SELECT p.child_id FROM tag_parents p JOIN d ON p.parent_id = d.id
         )
         SELECT id FROM d",
    )?;
    let rows = stmt.query_map([id], |row| row.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// `from` を `into` に統合する。付いていたアイテム・ユニット・別名・親子を移し、`from` の名前は別名として残す。
pub fn merge(conn: &Connection, from: i64, into: i64) -> Result<()> {
    if from == into {
        return Err(CoreError::Invalid("同じタグには統合できません".into()));
    }
    let source = detail(conn, from)?.tag;
    ensure_exists(conn, into)?;
    conn.execute(
        "INSERT OR IGNORE INTO item_tags (tag_id, item_id, source)
         SELECT ?2, item_id, source FROM item_tags WHERE tag_id = ?1",
        [from, into],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO unit_tags (tag_id, unit_id) SELECT ?2, unit_id FROM unit_tags WHERE tag_id = ?1",
        [from, into],
    )?;
    conn.execute(
        "UPDATE tag_aliases SET tag_id = ?2 WHERE tag_id = ?1",
        [from, into],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO tag_parents (child_id, parent_id)
         SELECT ?2, parent_id FROM tag_parents WHERE child_id = ?1 AND parent_id <> ?2",
        [from, into],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO tag_parents (child_id, parent_id)
         SELECT child_id, ?2 FROM tag_parents WHERE parent_id = ?1 AND child_id <> ?2",
        [from, into],
    )?;
    conn.execute("DELETE FROM tags WHERE id = ?1", [from])?;
    conn.execute(
        "INSERT OR IGNORE INTO tag_aliases (alias, tag_id) VALUES (?1, ?2)",
        params![source_display(&source), into],
    )?;
    if descendants(conn, into)?.contains(&into) {
        return Err(CoreError::Invalid(
            "統合すると親子関係が循環するため統合できません".into(),
        ));
    }
    Ok(())
}

fn source_display(tag: &TagRef) -> String {
    display(&tag.namespace, &tag.name)
}

/// タグを消す（付いていたアイテムからも外れる）。
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    ensure_exists(conn, id)?;
    conn.execute("DELETE FROM tags WHERE id = ?1", [id])?;
    Ok(())
}

/// タグの一覧の 1 行（タグの一覧の画面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagListEntry {
    pub tag: TagRef,
    /// 付いているアイテムの数（ゴミ箱を除く。押したときの絞り込みの件数と合う）。
    pub items: i64,
    /// そのうち、ダウンロード・キャプチャで自動で付いたもの（`item_tags.source`）。
    pub auto: i64,
    /// 付いているユニットの数。
    pub units: i64,
}

/// すべてのタグと、付いている数（名前空間なしを後ろに、名前空間ごとに付いている数の多い順）。
pub fn list_all(conn: &Connection) -> Result<Vec<TagListEntry>> {
    // すべてのタグを数えるので、item_tags を 1 回まとめて読み（意図した全件の集計）、ゴミ箱の分だけ引く。
    // ゴミ箱の分は、ゴミ箱のアイテムから先に引く（CROSS JOIN で順を決める。そうしないと item_tags を全部読む）。
    // 10 万件・タグ 2,000 個で、items と突き合わせて数えると 158ms（ADR-126）
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.namespace, t.name, t.color,
                coalesce(c.items, 0) - coalesce(x.items, 0),
                coalesce(c.auto, 0) - coalesce(x.auto, 0),
                coalesce(u.units, 0)
         FROM tags t
         LEFT JOIN (
           SELECT tag_id, count(*) AS items, sum(source IN ('download', 'capture')) AS auto
           FROM item_tags GROUP BY tag_id
         ) c ON c.tag_id = t.id
         LEFT JOIN (
           SELECT m.tag_id, count(*) AS items, sum(m.source IN ('download', 'capture')) AS auto
           FROM items i CROSS JOIN item_tags m ON m.item_id = i.id
           WHERE i.trashed_at IS NOT NULL
           GROUP BY m.tag_id
         ) x ON x.tag_id = t.id
         LEFT JOIN (SELECT tag_id, count(*) AS units FROM unit_tags GROUP BY tag_id) u
           ON u.tag_id = t.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(TagListEntry {
            tag: TagRef {
                id: row.get(0)?,
                namespace: row.get(1)?,
                name: row.get(2)?,
                color: row.get(3)?,
            },
            items: row.get(4)?,
            auto: row.get(5)?,
            units: row.get(6)?,
        })
    })?;
    let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    out.sort_by(|a, b| {
        a.tag
            .namespace
            .is_empty()
            .cmp(&b.tag.namespace.is_empty())
            .then_with(|| natural_cmp(&a.tag.namespace, &b.tag.namespace))
            .then_with(|| b.items.cmp(&a.items))
            .then_with(|| natural_cmp(&a.tag.name, &b.tag.name))
    });
    Ok(out)
}

/// 名前空間なしを後ろに、名前空間 → 名前の自然順で並べる（インスペクタの表示順）。
pub fn display_order(a: &TagRef, b: &TagRef) -> Ordering {
    a.namespace
        .is_empty()
        .cmp(&b.namespace.is_empty())
        .then_with(|| natural_cmp(&a.namespace, &b.namespace))
        .then_with(|| natural_cmp(&a.name, &b.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    fn item(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (?1, ?2, 'jpg', 'image', 1, 't', 't', 0, 0)",
            params![id, vec![id as u8; 32]],
        )
        .unwrap();
    }

    fn tags_of(conn: &Connection, item: i64) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT t.namespace, t.name FROM item_tags m JOIN tags t ON t.id = m.tag_id
                 WHERE m.item_id = ?1 ORDER BY t.namespace, t.name",
            )
            .unwrap();
        stmt.query_map([item], |r| {
            Ok(display(&r.get::<_, String>(0)?, &r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    #[test]
    fn lists_every_tag_with_counts_and_auto_tags() {
        let conn = test_conn();
        for id in 1..=4 {
            item(&conn, id);
        }
        conn.execute("UPDATE items SET trashed_at = 9 WHERE id = 4", [])
            .unwrap();
        conn.execute_batch(
            "INSERT INTO tags (id, namespace, name, created_at) VALUES
               (1, '', '夕景', 0), (2, '作者', 'もりの', 0), (3, '作者', 'KR', 0),
               (4, '', '使われていない', 0), (5, '場所', '鎌倉', 0);
             INSERT INTO item_tags (tag_id, item_id, source) VALUES
               (1, 1, 'download'), (1, 2, 'user'), (1, 4, 'download'),
               (2, 1, 'download'), (3, 1, 'download'), (3, 2, 'capture'), (3, 3, 'user'),
               (5, 2, 'import');
             INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (1, 'set', 'u', 'manual', 0, 0);
             INSERT INTO unit_tags (tag_id, unit_id) VALUES (5, 1);",
        )
        .unwrap();

        let all = list_all(&conn).unwrap();
        let rows: Vec<(String, i64, i64, i64)> = all
            .iter()
            .map(|e| {
                (
                    display(&e.tag.namespace, &e.tag.name),
                    e.items,
                    e.auto,
                    e.units,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                // 名前空間ごとに、付いている数の多い順。名前空間なしは後ろ。ゴミ箱は数えない
                ("作者:KR".to_string(), 3, 2, 0),
                ("作者:もりの".to_string(), 1, 1, 0),
                ("場所:鎌倉".to_string(), 1, 0, 1),
                ("夕景".to_string(), 2, 1, 0),
                ("使われていない".to_string(), 0, 0, 0),
            ]
        );
    }

    #[test]
    fn parses_names_with_namespaces() {
        assert_eq!(parse_tag(" 夕景 "), Some(TagName::plain("夕景")));
        assert_eq!(
            parse_tag("作者：もりの"),
            Some(TagName {
                namespace: "作者".into(),
                name: "もりの".into()
            })
        );
        assert_eq!(parse_tag("作者:"), None);
        assert_eq!(parse_tag("  "), None);
    }

    #[test]
    fn applies_and_removes_tags_creating_new_ones() {
        let conn = test_conn();
        item(&conn, 1);
        item(&conn, 2);
        let added = apply(
            &conn,
            &[1, 2],
            &["夕景".into(), "作者:もりの".into(), "夕景".into()],
            &[],
            0,
        )
        .unwrap();
        assert_eq!(added.len(), 2);
        assert_eq!(tags_of(&conn, 2), vec!["夕景", "作者:もりの"]);

        // 別名で付けると正式タグになる
        conn.execute(
            "INSERT INTO tag_aliases (alias, tag_id) VALUES ('夕焼け', ?1)",
            [added[0]],
        )
        .unwrap();
        let again = apply(&conn, &[1], &["夕焼け".into()], &[added[1]], 0).unwrap();
        assert_eq!(again, vec![added[0]]);
        assert_eq!(tags_of(&conn, 1), vec!["夕景"]);
        assert!(apply(&conn, &[1], &["作者:".into()], &[], 0).is_err());
    }

    #[test]
    fn suggests_prefix_first_then_by_use() {
        let conn = test_conn();
        for id in 1..=3 {
            item(&conn, id);
        }
        apply(&conn, &[1, 2, 3], &["夜景".into()], &[], 0).unwrap();
        apply(
            &conn,
            &[1],
            &["夜".into(), "作者:夜野".into(), "深夜".into()],
            &[],
            0,
        )
        .unwrap();
        let names = |q: &str| -> Vec<String> {
            suggest(&conn, q, 10)
                .unwrap()
                .into_iter()
                .map(|s| display(&s.tag.namespace, &s.tag.name))
                .collect()
        };
        // 前方一致（夜景 3 件、夜 1 件）→ 部分一致（作者:夜野、深夜）
        assert_eq!(names("夜"), vec!["夜景", "夜", "作者:夜野", "深夜"]);
        assert_eq!(names("作者:"), vec!["作者:夜野"]);
        assert_eq!(names("作者：夜"), vec!["作者:夜野"]);
        assert_eq!(names("")[0], "夜景");
        let top = suggest(&conn, "", 1).unwrap();
        assert_eq!(top[0].count, 3);

        // 別名でも見つかり、どの別名で一致したかを返す
        let night = find(&conn, "", "夜景").unwrap().unwrap();
        conn.execute(
            "INSERT INTO tag_aliases (alias, tag_id) VALUES ('ナイト', ?1)",
            [night],
        )
        .unwrap();
        let hit = suggest(&conn, "ナイ", 5).unwrap();
        assert_eq!(hit[0].tag.id, night);
        assert_eq!(hit[0].alias.as_deref(), Some("ナイト"));
    }

    #[test]
    fn updates_names_aliases_and_parents() {
        let conn = test_conn();
        let a = ensure_tag(&conn, &TagName::plain("海"), 0).unwrap();
        let b = ensure_tag(&conn, &TagName::plain("港"), 0).unwrap();
        let c = ensure_tag(&conn, &TagName::plain("風景"), 0).unwrap();

        update(
            &conn,
            a,
            &TagPatch {
                namespace: Some("場所".into()),
                color: Some(Some("blue".into())),
                aliases: Some(vec!["うみ".into(), "sea".into()]),
                parents: Some(vec![c]),
                ..TagPatch::default()
            },
        )
        .unwrap();
        let d = detail(&conn, a).unwrap();
        assert_eq!(
            (d.tag.namespace.as_str(), d.tag.color.as_deref()),
            ("場所", Some("blue"))
        );
        assert_eq!(d.aliases, vec!["sea", "うみ"]);
        assert_eq!(d.parents.iter().map(|t| t.id).collect::<Vec<_>>(), vec![c]);
        assert_eq!(detail(&conn, c).unwrap().children[0].id, a);

        // 同じ名前・ほかのタグの別名・既存のタグ名の別名は競合
        let conflict = |patch: TagPatch| update(&conn, b, &patch);
        assert!(matches!(
            conflict(TagPatch {
                namespace: Some("場所".into()),
                name: Some("海".into()),
                ..TagPatch::default()
            }),
            Err(CoreError::Conflict(_))
        ));
        assert!(matches!(
            conflict(TagPatch {
                name: Some("うみ".into()),
                ..TagPatch::default()
            }),
            Err(CoreError::Conflict(_))
        ));
        assert!(matches!(
            conflict(TagPatch {
                aliases: Some(vec!["風景".into()]),
                ..TagPatch::default()
            }),
            Err(CoreError::Conflict(_))
        ));
        assert!(matches!(
            conflict(TagPatch {
                namespace: Some("a:b".into()),
                ..TagPatch::default()
            }),
            Err(CoreError::Invalid(_))
        ));

        // 子孫を親にはできない（風景 → 海 の下に 風景 は置けない）
        assert!(matches!(
            update(
                &conn,
                c,
                &TagPatch {
                    parents: Some(vec![a]),
                    ..TagPatch::default()
                }
            ),
            Err(CoreError::Invalid(_))
        ));
        // 自分の別名を正式名にすると別名から外れる
        update(
            &conn,
            a,
            &TagPatch {
                namespace: Some(String::new()),
                name: Some("うみ".into()),
                ..TagPatch::default()
            },
        )
        .unwrap();
        assert_eq!(detail(&conn, a).unwrap().aliases, vec!["sea"]);
    }

    #[test]
    fn merges_items_aliases_and_parents() {
        let conn = test_conn();
        item(&conn, 1);
        item(&conn, 2);
        apply(&conn, &[1], &["夕焼け".into()], &[], 0).unwrap();
        apply(&conn, &[2], &["夕景".into()], &[], 0).unwrap();
        let from = find(&conn, "", "夕焼け").unwrap().unwrap();
        let into = find(&conn, "", "夕景").unwrap().unwrap();
        let parent = ensure_tag(&conn, &TagName::plain("空"), 0).unwrap();
        update(
            &conn,
            from,
            &TagPatch {
                aliases: Some(vec!["sunset".into()]),
                parents: Some(vec![parent]),
                ..TagPatch::default()
            },
        )
        .unwrap();

        merge(&conn, from, into).unwrap();
        assert_eq!(tags_of(&conn, 1), vec!["夕景"]);
        let d = detail(&conn, into).unwrap();
        assert_eq!(d.count, 2);
        assert_eq!(d.aliases, vec!["sunset", "夕焼け"]);
        assert_eq!(d.parents[0].id, parent);
        assert!(detail(&conn, from).is_err());
        assert!(merge(&conn, into, into).is_err());

        delete(&conn, parent).unwrap();
        assert!(detail(&conn, into).unwrap().parents.is_empty());
    }

    #[test]
    fn merge_refuses_cycles() {
        let conn = test_conn();
        let x = ensure_tag(&conn, &TagName::plain("x"), 0).unwrap();
        let from = ensure_tag(&conn, &TagName::plain("from"), 0).unwrap();
        let into = ensure_tag(&conn, &TagName::plain("into"), 0).unwrap();
        // from は x の子、into は x の親 → 統合すると into が x の親かつ子になる
        update(
            &conn,
            from,
            &TagPatch {
                parents: Some(vec![x]),
                ..TagPatch::default()
            },
        )
        .unwrap();
        update(
            &conn,
            x,
            &TagPatch {
                parents: Some(vec![into]),
                ..TagPatch::default()
            },
        )
        .unwrap();
        assert!(matches!(
            merge(&conn, from, into),
            Err(CoreError::Invalid(_))
        ));
    }
}
