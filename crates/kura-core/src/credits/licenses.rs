//! ライセンスの表（M15 段 1。docs/data-model.md §4「ライセンスとクレジット（M15）」）。既定の行（`builtin` に鍵）は名前と印を
//! 変えられず消せない。利用者が足した行は変えられ、消すと使っていたアイテムは未設定に戻る（`ON DELETE SET NULL`。ADR-202 の
//! 段 1 の決定 4）。

use rusqlite::{Connection, OptionalExtension, params};

use super::{Attribution, Permission, clean};
use crate::error::{CoreError, Result};
use crate::text::nfc;

/// 名前の長さの上限（文字）。
pub const MAX_NAME_CHARS: usize = 200;

/// ライセンス 1 行。
#[derive(Debug, Clone, PartialEq)]
pub struct License {
    pub id: i64,
    /// 既定の行の鍵（`cc-by-4.0` など）。利用者が足した行は None。
    pub builtin: Option<String>,
    pub name: String,
    pub spdx: Option<String>,
    pub url: Option<String>,
    pub attribution: Attribution,
    pub commercial: Permission,
    pub modify: Permission,
    pub share_alike: bool,
    pub note: Option<String>,
    pub pos: f64,
    /// このライセンスを付けたアイテムの数（ゴミ箱の中を含む）。
    pub items: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 足す・変えるときの中身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseInput {
    pub name: String,
    pub spdx: Option<String>,
    pub url: Option<String>,
    pub attribution: Attribution,
    pub commercial: Permission,
    pub modify: Permission,
    pub share_alike: bool,
    pub note: Option<String>,
}

/// 整えた中身（名前は NFC で前後の空白を除いたもの）。
struct Clean {
    name: String,
    spdx: Option<String>,
    url: Option<String>,
    note: Option<String>,
}

fn clean_input(input: &LicenseInput) -> Result<Clean> {
    let name = nfc(input.name.trim());
    if name.is_empty() {
        return Err(CoreError::Invalid(
            "ライセンスの名前を入力してください".into(),
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(CoreError::Invalid(format!(
            "ライセンスの名前は {MAX_NAME_CHARS} 文字までです"
        )));
    }
    // 引用符は検索の値の書き方で逃がせない（`ライセンス:"…"` に入らない）ので、名前に入れない。改行なども入れない
    if name
        .chars()
        .any(|c| matches!(c, '"' | '“' | '”' | '＂') || c.is_control())
    {
        return Err(CoreError::Invalid(
            "ライセンスの名前に引用符や改行は使えません".into(),
        ));
    }
    Ok(Clean {
        name,
        spdx: clean("SPDX の識別子", input.spdx.as_deref())?,
        url: clean("URL", input.url.as_deref())?,
        note: clean("メモ", input.note.as_deref())?,
    })
}

const COLUMNS: &str = "l.id, l.builtin, l.name, l.spdx, l.url, l.attribution, l.commercial, l.modify, l.share_alike,
     l.note, l.pos, (SELECT count(*) FROM item_credits c WHERE c.license_id = l.id), l.created_at, l.updated_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(License, String, String, String)> {
    Ok((
        License {
            id: r.get(0)?,
            builtin: r.get(1)?,
            name: r.get(2)?,
            spdx: r.get(3)?,
            url: r.get(4)?,
            attribution: Attribution::None,
            commercial: Permission::Unknown,
            modify: Permission::Unknown,
            share_alike: r.get::<_, i64>(8)? != 0,
            note: r.get(9)?,
            pos: r.get(10)?,
            items: r.get(11)?,
            created_at: r.get(12)?,
            updated_at: r.get(13)?,
        },
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
    ))
}

fn finish((mut l, a, c, m): (License, String, String, String)) -> Result<License> {
    l.attribution = Attribution::parse(&a)?;
    l.commercial = Permission::parse(&c)?;
    l.modify = Permission::parse(&m)?;
    Ok(l)
}

/// すべてのライセンス（並びの順。既定の行が先）。
pub fn list(conn: &Connection) -> Result<Vec<License>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM licenses l ORDER BY l.pos, l.id"
    ))?;
    let rows = stmt.query_map([], row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(finish(r?)?);
    }
    Ok(out)
}

/// 1 行。
pub fn get(conn: &Connection, id: i64) -> Result<License> {
    let mut stmt =
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM licenses l WHERE l.id = ?1"))?;
    let found = stmt.query_row([id], row).optional()?;
    finish(found.ok_or_else(|| CoreError::NotFound(format!("ライセンス {id}")))?)
}

/// 同じ名前（大文字小文字を問わない。ASCII 以外も Rust の小文字で比べる。索引の NOCASE は ASCII だけの守り）の行がほかに
/// あれば断る。ライセンスは少ないので全部を読んで比べる。
fn check_name(conn: &Connection, name: &str, except: Option<i64>) -> Result<()> {
    let folded = name.to_lowercase();
    let mut stmt = conn.prepare_cached("SELECT id, name FROM licenses")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (id, other) = row?;
        if Some(id) != except && other.to_lowercase() == folded {
            return Err(CoreError::Conflict(format!(
                "「{other}」というライセンスがあります"
            )));
        }
    }
    Ok(())
}

/// 利用者のライセンスを足す（並びの最後）。ID を返す。
pub fn create(conn: &Connection, input: &LicenseInput, now: i64) -> Result<i64> {
    let c = clean_input(input)?;
    check_name(conn, &c.name, None)?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO licenses (builtin, name, spdx, url, attribution, commercial, modify, share_alike, note, pos,
                               created_at, updated_at)
         VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, (SELECT ifnull(max(pos), -1) + 1 FROM licenses), ?9, ?9)",
    )?;
    stmt.execute(params![
        c.name,
        c.spdx,
        c.url,
        input.attribution.as_str(),
        input.commercial.as_str(),
        input.modify.as_str(),
        i64::from(input.share_alike),
        c.note,
        now
    ])?;
    Ok(conn.last_insert_rowid())
}

/// 既定の行なら断る。
fn editable(conn: &Connection, id: i64, verb: &str) -> Result<()> {
    let l = get(conn, id)?;
    if l.builtin.is_some() {
        return Err(CoreError::Invalid(format!(
            "既定のライセンス「{}」は{verb}ません",
            l.name
        )));
    }
    Ok(())
}

/// 利用者のライセンスを変える（既定の行は断る）。
pub fn update(conn: &Connection, id: i64, input: &LicenseInput, now: i64) -> Result<()> {
    editable(conn, id, "変えられ")?;
    let c = clean_input(input)?;
    check_name(conn, &c.name, Some(id))?;
    let mut stmt = conn.prepare_cached(
        "UPDATE licenses SET name = ?2, spdx = ?3, url = ?4, attribution = ?5, commercial = ?6, modify = ?7,
                             share_alike = ?8, note = ?9, updated_at = ?10
          WHERE id = ?1",
    )?;
    stmt.execute(params![
        id,
        c.name,
        c.spdx,
        c.url,
        input.attribution.as_str(),
        input.commercial.as_str(),
        input.modify.as_str(),
        i64::from(input.share_alike),
        c.note,
        now
    ])?;
    Ok(())
}

/// 利用者のライセンスを消す（既定の行は断る）。使っていたアイテムは未設定に戻る。未設定に戻した件数を返す。
pub fn delete(conn: &Connection, id: i64) -> Result<i64> {
    editable(conn, id, "消せ")?;
    let used = get(conn, id)?.items;
    // ライセンスのほかに何も入っていなかったアイテムは、クレジットの行ごと消す（行は何か入っているアイテムにだけある）
    conn.prepare_cached(
        "DELETE FROM item_credits WHERE license_id = ?1
            AND author IS NULL AND work IS NULL AND url IS NULL AND credit IS NULL",
    )?
    .execute([id])?;
    conn.prepare_cached("DELETE FROM licenses WHERE id = ?1")?
        .execute([id])?;
    Ok(used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    pub(crate) fn input(name: &str) -> LicenseInput {
        LicenseInput {
            name: name.into(),
            spdx: None,
            url: Some(" https://example.com/terms ".into()),
            attribution: Attribution::Optional,
            commercial: Permission::Unknown,
            modify: Permission::Yes,
            share_alike: false,
            note: Some("".into()),
        }
    }

    #[test]
    fn builtins_come_first_and_cannot_change() {
        let conn = test_conn();
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 13);
        assert_eq!(all[0].builtin.as_deref(), Some("cc0-1.0"));
        assert_eq!(all[0].attribution, Attribution::None);
        let by = all
            .iter()
            .find(|l| l.builtin.as_deref() == Some("cc-by-4.0"))
            .unwrap();
        assert_eq!(by.name, "CC BY 4.0");
        assert_eq!(by.attribution, Attribution::Required);
        assert_eq!(by.items, 0);
        let nc = all
            .iter()
            .find(|l| l.builtin.as_deref() == Some("cc-by-nc-sa-4.0"))
            .unwrap();
        assert_eq!((nc.commercial, nc.share_alike), (Permission::No, true));
        assert!(matches!(
            update(&conn, by.id, &input("x"), 5),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(delete(&conn, by.id), Err(CoreError::Invalid(_))));
        assert!(matches!(get(&conn, 999), Err(CoreError::NotFound(_))));
    }

    #[test]
    fn custom_licenses_are_cleaned_unique_and_appended() {
        let conn = test_conn();
        let id = create(&conn, &input("  魔王魂 利用規約 "), 7).unwrap();
        let l = get(&conn, id).unwrap();
        assert_eq!(l.name, "魔王魂 利用規約");
        assert_eq!(l.url.as_deref(), Some("https://example.com/terms"));
        assert_eq!(l.note, None);
        assert_eq!(l.builtin, None);
        assert_eq!(l.pos, 13.0);
        assert_eq!((l.created_at, l.updated_at), (7, 7));
        // 名前は大文字小文字を問わず一意（既定の行とも）
        for name in ["cc by 4.0", "魔王魂 利用規約"] {
            assert!(matches!(
                create(&conn, &input(name), 8),
                Err(CoreError::Conflict(_))
            ));
        }
        assert!(matches!(
            create(&conn, &input("  "), 8),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            create(&conn, &input(&"あ".repeat(MAX_NAME_CHARS + 1)), 8),
            Err(CoreError::Invalid(_))
        ));
        let other = create(&conn, &input("別の規約"), 8).unwrap();
        assert_eq!(get(&conn, other).unwrap().pos, 14.0);
        // ASCII 以外の大文字小文字も同じ名前とみなす。引用符・改行は名前に入れない
        create(&conn, &input("Ça va"), 8).unwrap();
        assert!(matches!(
            create(&conn, &input("ça VA"), 8),
            Err(CoreError::Conflict(_))
        ));
        for bad in ["\"規約\"", "“規約”", "規約\n2"] {
            assert!(
                matches!(create(&conn, &input(bad), 8), Err(CoreError::Invalid(_))),
                "{bad}"
            );
        }
        // 変える：自分の名前のままは通り、ほかの名前とぶつかれば断る
        let mut changed = input("魔王魂 利用規約");
        changed.attribution = Attribution::Required;
        update(&conn, id, &changed, 9).unwrap();
        let l = get(&conn, id).unwrap();
        assert_eq!((l.attribution, l.updated_at), (Attribution::Required, 9));
        assert!(matches!(
            update(&conn, id, &input("別の規約"), 9),
            Err(CoreError::Conflict(_))
        ));
    }

    #[test]
    fn deleting_a_used_license_unsets_the_items() {
        let conn = test_conn();
        for id in [1, 2] {
            conn.execute(
                "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                 VALUES (?1, ?2, 'png', 'image', 1, 't', 't', 0, 0)",
                params![id, vec![id as u8; 32]],
            )
            .unwrap();
        }
        let id = create(&conn, &input("規約"), 1).unwrap();
        conn.execute(
            "INSERT INTO item_credits (item_id, license_id, author, updated_at) VALUES (1, ?1, 'a', 1), (2, ?1, NULL, 1)",
            [id],
        )
        .unwrap();
        assert_eq!(get(&conn, id).unwrap().items, 2);
        assert_eq!(delete(&conn, id).unwrap(), 2);
        // 作者のあるアイテム 1 は未設定のライセンスで残り、ライセンスだけだったアイテム 2 は行ごと消える
        let rows: Vec<(i64, Option<i64>, Option<String>)> = conn
            .prepare("SELECT item_id, license_id, author FROM item_credits ORDER BY item_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec![(1, None, Some("a".into()))]);
        assert!(matches!(get(&conn, id), Err(CoreError::NotFound(_))));
    }
}
