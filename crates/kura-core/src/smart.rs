//! スマートフォルダ（保存した検索。docs/data-model.md `smart_folders`）。条件は検索の文字列のまま保存する。

use rusqlite::{Connection, OptionalExtension, params};

use crate::query::{Expr, parse};
use crate::text::nfc;
use crate::{CoreError, Result};

/// スマートフォルダ 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartFolder {
    pub id: i64,
    pub name: String,
    pub query: String,
}

/// すべてのスマートフォルダ（並び順）。
pub fn list(conn: &Connection) -> Result<Vec<SmartFolder>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, name, query FROM smart_folders ORDER BY pos, id")?;
    let rows = stmt.query_map([], |r| {
        Ok(SmartFolder {
            id: r.get(0)?,
            name: r.get(1)?,
            query: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 1 つのスマートフォルダ。
pub fn get(conn: &Connection, id: i64) -> Result<SmartFolder> {
    let mut stmt =
        conn.prepare_cached("SELECT id, name, query FROM smart_folders WHERE id = ?1")?;
    stmt.query_row([id], |r| {
        Ok(SmartFolder {
            id: r.get(0)?,
            name: r.get(1)?,
            query: r.get(2)?,
        })
    })
    .optional()?
    .ok_or_else(|| CoreError::NotFound(format!("スマートフォルダ {id}")))
}

/// 保存した条件を解析する（壊れていれば Invalid）。
pub fn expr_of(folder: &SmartFolder) -> Result<Expr> {
    parse(&folder.query).map(|p| p.expr).map_err(|e| {
        CoreError::Invalid(format!(
            "スマートフォルダ「{}」の条件：{}",
            folder.name, e.message
        ))
    })
}

fn clean_name(name: &str) -> Result<String> {
    let name = nfc(name.trim());
    if name.is_empty() {
        return Err(CoreError::Invalid(
            "スマートフォルダの名前を入力してください".into(),
        ));
    }
    Ok(name)
}

fn check_query(query: &str) -> Result<String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err(CoreError::Invalid("条件を入力してください".into()));
    }
    parse(&query).map_err(|e| CoreError::Invalid(e.message))?;
    Ok(query)
}

/// 新しく保存する。作ったスマートフォルダの ID を返す。
pub fn save(conn: &Connection, name: &str, query: &str, now: i64) -> Result<i64> {
    let name = clean_name(name)?;
    let query = check_query(query)?;
    let pos: f64 = conn.query_row(
        "SELECT coalesce(max(pos), 0) + 1 FROM smart_folders",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO smart_folders (name, query, pos, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name, query, pos, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 名前・条件を変える（None は変えない）。
pub fn update(conn: &Connection, id: i64, name: Option<&str>, query: Option<&str>) -> Result<()> {
    get(conn, id)?;
    if let Some(name) = name {
        let name = clean_name(name)?;
        conn.execute(
            "UPDATE smart_folders SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
    }
    if let Some(query) = query {
        let query = check_query(query)?;
        conn.execute(
            "UPDATE smart_folders SET query = ?2 WHERE id = ?1",
            params![id, query],
        )?;
    }
    Ok(())
}

/// 消す。
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    get(conn, id)?;
    conn.execute("DELETE FROM smart_folders WHERE id = ?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    #[test]
    fn saves_updates_and_deletes() {
        let conn = test_conn();
        let a = save(
            &conn,
            " 夜景の背景資料 ",
            "夜景 種類:画像 評価>=3 -未整理",
            0,
        )
        .unwrap();
        let b = save(
            &conn,
            "未再生の長尺動画",
            "種類:動画 長さ>20分 再生回数:0",
            0,
        )
        .unwrap();
        let names: Vec<String> = list(&conn).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["夜景の背景資料", "未再生の長尺動画"]);

        assert!(matches!(
            save(&conn, "x", "(海", 0),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            save(&conn, "x", "  ", 0),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            save(&conn, " ", "海", 0),
            Err(CoreError::Invalid(_))
        ));

        update(&conn, a, Some("夜景"), Some("夜景 -未整理")).unwrap();
        let folder = get(&conn, a).unwrap();
        assert_eq!(
            (folder.name.as_str(), folder.query.as_str()),
            ("夜景", "夜景 -未整理")
        );
        assert!(expr_of(&folder).is_ok());
        assert!(update(&conn, a, None, Some("評価>=9")).is_err());

        delete(&conn, b).unwrap();
        assert!(matches!(get(&conn, b), Err(CoreError::NotFound(_))));
    }
}
