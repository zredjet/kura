//! アイテムごとのクレジット（M15 段 1。docs/data-model.md §4「ライセンスとクレジット（M15）」）：ライセンス・作者・作品・出典・
//! 表記の文。行は何か 1 つでも入っているアイテムにだけあり、全部を空にすると消す。まとめて付ける（選んだもの・ユニットの
//! アイテム）は [`set`]、別名で保存した新しいアイテムへの写しは [`copy`]（ADR-202 の段 1 の決定 2）。

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params};

use super::clean;
use crate::error::{CoreError, Result};

/// 1 件のクレジット（行がなければ全部 None）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemCredit {
    /// ライセンス（None は未設定）。
    pub license_id: Option<i64>,
    pub author: Option<String>,
    /// 作品・素材集の名前。
    pub work: Option<String>,
    /// 出典（None なら取り込み元の URL を使う）。
    pub url: Option<String>,
    /// 表記の文（None なら作者・作品・出典・ライセンスから組む）。
    pub credit: Option<String>,
}

impl ItemCredit {
    /// どの項目も空か。
    pub fn is_empty(&self) -> bool {
        self.license_id.is_none()
            && self.author.is_none()
            && self.work.is_none()
            && self.url.is_none()
            && self.credit.is_none()
    }
}

/// 変える項目（外側の None は変えない、`Some(None)` は消す）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreditPatch {
    pub license: Option<Option<i64>>,
    pub author: Option<Option<String>>,
    pub work: Option<Option<String>>,
    pub url: Option<Option<String>>,
    pub credit: Option<Option<String>>,
}

/// 項目ごとに、全件が同じならその値（`Some`）、食い違えば None。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreditSummary {
    /// 見つかったアイテムの数。
    pub count: usize,
    pub license: Option<Option<i64>>,
    pub author: Option<Option<String>>,
    pub work: Option<Option<String>>,
    pub url: Option<Option<String>>,
    pub credit: Option<Option<String>>,
}

fn exists(conn: &Connection, item: i64) -> Result<bool> {
    let mut stmt = conn.prepare_cached("SELECT 1 FROM items WHERE id = ?1")?;
    Ok(stmt.query_row([item], |_| Ok(())).optional()?.is_some())
}

fn read(conn: &Connection, item: i64) -> Result<ItemCredit> {
    let mut stmt = conn.prepare_cached(
        "SELECT license_id, author, work, url, credit FROM item_credits WHERE item_id = ?1",
    )?;
    Ok(stmt
        .query_row([item], |r| {
            Ok(ItemCredit {
                license_id: r.get(0)?,
                author: r.get(1)?,
                work: r.get(2)?,
                url: r.get(3)?,
                credit: r.get(4)?,
            })
        })
        .optional()?
        .unwrap_or_default())
}

/// 1 件のクレジット（行がなければ空）。アイテムがなければ NotFound。
pub fn get(conn: &Connection, item: i64) -> Result<ItemCredit> {
    if !exists(conn, item)? {
        return Err(CoreError::NotFound(format!("アイテム {item}")));
    }
    read(conn, item)
}

/// 整えたパッチ（文の項目を整え、ライセンスがあることを確かめる）。
fn clean_patch(conn: &Connection, patch: &CreditPatch) -> Result<CreditPatch> {
    if let Some(Some(license)) = patch.license {
        let mut stmt = conn.prepare_cached("SELECT 1 FROM licenses WHERE id = ?1")?;
        if stmt.query_row([license], |_| Ok(())).optional()?.is_none() {
            return Err(CoreError::NotFound(format!("ライセンス {license}")));
        }
    }
    let text = |label: &str, v: &Option<Option<String>>| -> Result<Option<Option<String>>> {
        v.as_ref().map(|v| clean(label, v.as_deref())).transpose()
    };
    Ok(CreditPatch {
        license: patch.license,
        author: text("作者", &patch.author)?,
        work: text("作品", &patch.work)?,
        url: text("出典", &patch.url)?,
        credit: text("表記の文", &patch.credit)?,
    })
}

/// 1 件に書く（全部空なら行を消す）。
fn write(conn: &Connection, item: i64, c: &ItemCredit, now: i64) -> Result<()> {
    if c.is_empty() {
        let mut stmt = conn.prepare_cached("DELETE FROM item_credits WHERE item_id = ?1")?;
        stmt.execute([item])?;
        return Ok(());
    }
    let mut stmt = conn.prepare_cached(
        "INSERT INTO item_credits (item_id, license_id, author, work, url, credit, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(item_id) DO UPDATE SET license_id = excluded.license_id, author = excluded.author,
           work = excluded.work, url = excluded.url, credit = excluded.credit, updated_at = excluded.updated_at",
    )?;
    stmt.execute(params![
        item,
        c.license_id,
        c.author,
        c.work,
        c.url,
        c.credit,
        now
    ])?;
    Ok(())
}

/// 選んだアイテムに同じ項目を付ける（パッチにない項目はアイテムごとのまま）。ないアイテムは飛ばす。変えた件数を返す
/// （中身が同じものは書かず、数えない）。呼び手が 1 つのトランザクションで呼ぶ。
pub fn set(conn: &Connection, items: &[i64], patch: &CreditPatch, now: i64) -> Result<usize> {
    let patch = clean_patch(conn, patch)?;
    let mut changed = 0;
    let mut seen = BTreeSet::new();
    for &item in items.iter().filter(|id| seen.insert(**id)) {
        if !exists(conn, item)? {
            continue;
        }
        let before = read(conn, item)?;
        let mut after = before.clone();
        if let Some(v) = patch.license {
            after.license_id = v;
        }
        if let Some(v) = &patch.author {
            after.author = v.clone();
        }
        if let Some(v) = &patch.work {
            after.work = v.clone();
        }
        if let Some(v) = &patch.url {
            after.url = v.clone();
        }
        if let Some(v) = &patch.credit {
            after.credit = v.clone();
        }
        if after != before {
            write(conn, item, &after, now)?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// アイテムとユニット（メンバー全員）を、重なりを除いたアイテムの列にする（選んだ順。ユニットのメンバーはその後ろ）。
pub fn expand(conn: &Connection, items: &[i64], units: &[i64]) -> Result<Vec<i64>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for &id in items {
        if seen.insert(id) {
            out.push(id);
        }
    }
    for &unit in units {
        for id in crate::unit::members(conn, unit)? {
            if seen.insert(id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// 選んだアイテムのクレジットをまとめる（複数選択のインスペクタ）。ないアイテムは数えない。
pub fn summary(conn: &Connection, items: &[i64]) -> Result<CreditSummary> {
    fn merge<T: PartialEq + Clone>(slot: &mut Option<T>, value: &T, first: bool) {
        if first {
            *slot = Some(value.clone());
        } else if slot.as_ref().is_some_and(|s| s != value) {
            *slot = None;
        }
    }
    let mut out = CreditSummary::default();
    let mut seen = BTreeSet::new();
    for &item in items.iter().filter(|id| seen.insert(**id)) {
        if !exists(conn, item)? {
            continue;
        }
        let c = read(conn, item)?;
        let first = out.count == 0;
        merge(&mut out.license, &c.license_id, first);
        merge(&mut out.author, &c.author, first);
        merge(&mut out.work, &c.work, first);
        merge(&mut out.url, &c.url, first);
        merge(&mut out.credit, &c.credit, first);
        out.count += 1;
    }
    Ok(out)
}

/// `from` のクレジットを `to`（別名で保存した新しいアイテム）に写す。`from` に行がなければ何もしない。別名の保存の
/// トランザクション 2 の中で呼ぶ（docs/data-model.md §4「保存と版（M12）」の「別名の写し方」）。
pub fn copy(conn: &Connection, from: i64, to: i64, now: i64) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO item_credits (item_id, license_id, author, work, url, credit, updated_at)
         SELECT ?2, license_id, author, work, url, credit, ?3 FROM item_credits WHERE item_id = ?1
         ON CONFLICT(item_id) DO NOTHING",
    )?;
    stmt.execute(params![from, to, now])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    fn item(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (?1, ?2, 'png', 'image', 1, 't', 't', 0, 0)",
            params![id, vec![id as u8; 32]],
        )
        .unwrap();
    }

    fn license(conn: &Connection, key: &str) -> i64 {
        conn.query_row("SELECT id FROM licenses WHERE builtin = ?1", [key], |r| {
            r.get(0)
        })
        .unwrap()
    }

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM item_credits", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn set_patches_only_the_given_fields_and_drops_empty_rows() {
        let conn = test_conn();
        for id in [1, 2, 3] {
            item(&conn, id);
        }
        let by = license(&conn, "cc-by-4.0");
        // まとめて付ける：ライセンスと作者。ない ID と重なった ID は数えない
        let patch = CreditPatch {
            license: Some(Some(by)),
            author: Some(Some("  もりの ".into())),
            ..CreditPatch::default()
        };
        assert_eq!(set(&conn, &[1, 2, 2, 99], &patch, 5).unwrap(), 2);
        assert_eq!(
            get(&conn, 1).unwrap(),
            ItemCredit {
                license_id: Some(by),
                author: Some("もりの".into()),
                ..ItemCredit::default()
            }
        );
        // 同じ中身はもう一度書かない
        assert_eq!(set(&conn, &[1, 2], &patch, 6).unwrap(), 0);
        // 1 件だけ作品を足す：ほかの項目はそのまま
        let work = CreditPatch {
            work: Some(Some("森の素材集".into())),
            ..CreditPatch::default()
        };
        assert_eq!(set(&conn, &[2], &work, 7).unwrap(), 1);
        assert_eq!(get(&conn, 2).unwrap().author.as_deref(), Some("もりの"));
        assert_eq!(get(&conn, 2).unwrap().work.as_deref(), Some("森の素材集"));
        assert_eq!(get(&conn, 1).unwrap().work, None);
        // 空の文は消すのと同じ。全部空になれば行を消す
        let clear = CreditPatch {
            license: Some(None),
            author: Some(Some("  ".into())),
            ..CreditPatch::default()
        };
        assert_eq!(set(&conn, &[1, 2], &clear, 8).unwrap(), 2);
        assert_eq!(get(&conn, 1).unwrap(), ItemCredit::default());
        assert_eq!(get(&conn, 2).unwrap().work.as_deref(), Some("森の素材集"));
        assert_eq!(rows(&conn), 1);
        // 行のないアイテムは空、ないアイテムは NotFound、ないライセンスは断る
        assert_eq!(get(&conn, 3).unwrap(), ItemCredit::default());
        assert!(matches!(get(&conn, 99), Err(CoreError::NotFound(_))));
        let bad = CreditPatch {
            license: Some(Some(999)),
            ..CreditPatch::default()
        };
        assert!(matches!(
            set(&conn, &[3], &bad, 9),
            Err(CoreError::NotFound(_))
        ));
        let long = CreditPatch {
            credit: Some(Some("あ".repeat(super::super::MAX_TEXT_CHARS + 1))),
            ..CreditPatch::default()
        };
        assert!(matches!(
            set(&conn, &[3], &long, 9),
            Err(CoreError::Invalid(_))
        ));
        assert_eq!(get(&conn, 3).unwrap(), ItemCredit::default());
    }

    #[test]
    fn summary_reports_shared_and_mixed_fields() {
        let conn = test_conn();
        for id in [1, 2, 3] {
            item(&conn, id);
        }
        let by = license(&conn, "cc-by-4.0");
        let patch = CreditPatch {
            license: Some(Some(by)),
            author: Some(Some("もりの".into())),
            ..CreditPatch::default()
        };
        set(&conn, &[1, 2], &patch, 1).unwrap();
        set(
            &conn,
            &[2],
            &CreditPatch {
                work: Some(Some("w".into())),
                ..CreditPatch::default()
            },
            1,
        )
        .unwrap();
        let s = summary(&conn, &[1, 2, 99]).unwrap();
        assert_eq!(s.count, 2);
        assert_eq!(s.license, Some(Some(by)));
        assert_eq!(s.author, Some(Some("もりの".into())));
        assert_eq!(s.work, None);
        assert_eq!(s.url, Some(None));
        // 行のないアイテムが混じると、ライセンスは食い違い
        let s = summary(&conn, &[1, 3]).unwrap();
        assert_eq!((s.count, s.license, s.url), (2, None, Some(None)));
        assert_eq!(summary(&conn, &[]).unwrap(), CreditSummary::default());
    }

    #[test]
    fn expand_adds_unit_members_once() {
        let conn = test_conn();
        for id in [1, 2, 3, 4] {
            item(&conn, id);
        }
        conn.execute_batch(
            "INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (1, 'set', 'u', 'manual', 0, 0);
             INSERT INTO unit_items (unit_id, item_id, pos) VALUES (1, 3, 1), (1, 2, 2), (1, 4, 3);",
        )
        .unwrap();
        assert_eq!(expand(&conn, &[2, 1, 2], &[1]).unwrap(), vec![2, 1, 3, 4]);
        assert_eq!(expand(&conn, &[], &[]).unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn copy_writes_the_credit_to_the_new_item_once() {
        let conn = test_conn();
        for id in [1, 2, 3] {
            item(&conn, id);
        }
        let by = license(&conn, "cc-by-4.0");
        set(
            &conn,
            &[1],
            &CreditPatch {
                license: Some(Some(by)),
                url: Some(Some("https://example.com/pack".into())),
                ..CreditPatch::default()
            },
            1,
        )
        .unwrap();
        copy(&conn, 1, 2, 5).unwrap();
        assert_eq!(get(&conn, 2).unwrap(), get(&conn, 1).unwrap());
        // 行のない元からは何も写さない
        copy(&conn, 3, 2, 6).unwrap();
        copy(&conn, 3, 3, 6).unwrap();
        assert_eq!(get(&conn, 3).unwrap(), ItemCredit::default());
        assert_eq!(rows(&conn), 2);
    }
}
