//! 取り込みで使う DB の読み書き（アイテム・仮想フォルダ・タグ・ユニット）。

use rusqlite::{Connection, OptionalExtension, params};

use super::kind::MediaKind;
use crate::media::MediaInfo;
use crate::text::nfc;
use crate::{CoreError, Result};

/// 同じハッシュの既存アイテム。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingItem {
    pub id: i64,
    pub ext: String,
    pub trashed: bool,
}

/// 追加するアイテム。
#[derive(Debug, Clone)]
pub struct NewItem<'a> {
    pub hash: &'a [u8; 32],
    pub ext: &'a str,
    pub kind: MediaKind,
    pub mime: Option<&'a str>,
    pub size: u64,
    pub title: &'a str,
    pub original_name: &'a str,
    pub original_path: &'a str,
    /// 読めたメディア情報。
    pub info: Option<&'a MediaInfo>,
    /// 取得元のサイト（取り込みは `local`）と URL。
    pub source_site: &'a str,
    pub source_url: Option<&'a str>,
    /// 投稿日時など、メディア情報より優先する撮影・投稿日時。
    pub captured_at: Option<i64>,
    pub now: i64,
}

/// 同じ中身の持ち主のアイテムで、その中身がどれに当たるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerRole {
    /// 今（`items.hash`）。
    Current,
    /// 元（版 0）。
    Original,
    /// 過去の版（猶予中・消えたものを含む）。
    Past { version: i64 },
}

/// 同じ中身の持ち主（取り込み・ダウンロード・キャプチャの重複の判定。docs/editing.md §14）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub id: i64,
    /// アイテムの今の拡張子（置いたファイルを残すかは台帳の拡張子で決める）。
    pub ext: String,
    pub trashed: bool,
    pub role: OwnerRole,
}

/// 中身 `hash` の持ち主を、今 → 元 → 過去の版の順に探す（ゴミ箱のアイテムも）。同じ中身を元に持つアイテムが複数あれば
/// ID の小さい方、過去の版だけなら ID の小さいアイテムの新しい版。どちらの問い合わせも索引で引く（items は hash の一意の
/// 索引、版は出力のハッシュの索引）。
pub fn find_owner(conn: &Connection, hash: &[u8; 32]) -> Result<Option<Owner>> {
    if let Some(item) = find_by_hash(conn, hash)? {
        return Ok(Some(Owner {
            id: item.id,
            ext: item.ext,
            trashed: item.trashed,
            role: OwnerRole::Current,
        }));
    }
    let mut stmt = conn.prepare_cached(
        "SELECT v.item_id, i.ext, i.trashed_at IS NOT NULL, v.version
           FROM item_versions v JOIN items i ON i.id = v.item_id
          WHERE v.output_hash = ?1
          ORDER BY v.version <> 0, v.item_id, v.version DESC LIMIT 1",
    )?;
    Ok(stmt
        .query_row([&hash[..]], |row| {
            let version: i64 = row.get(3)?;
            Ok(Owner {
                id: row.get(0)?,
                ext: row.get(1)?,
                trashed: row.get(2)?,
                role: if version == 0 {
                    OwnerRole::Original
                } else {
                    OwnerRole::Past { version }
                },
            })
        })
        .optional()?)
}

/// ハッシュでアイテム（今の中身）を探す。重複の判定には、元・過去の版も見る [`find_owner`] を使う。
pub fn find_by_hash(conn: &Connection, hash: &[u8; 32]) -> Result<Option<ExistingItem>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, ext, trashed_at IS NOT NULL FROM items WHERE hash = ?1")?;
    Ok(stmt
        .query_row([&hash[..]], |row| {
            Ok(ExistingItem {
                id: row.get(0)?,
                ext: row.get(1)?,
                trashed: row.get(2)?,
            })
        })
        .optional()?)
}

/// アイテムを追加して ID を返す。プロキシを作るのは動画・音声だけなので、ほかの種類の proxy_state は not_needed。
/// 解析するのは画像・音声だけなので、ほかの種類の analysis_state は not_needed（docs/data-model.md §3）。
/// 台帳 `blobs` にはトリガが載せる。`original_path` が空なら NULL にする（ダウンロードしたもの）。
pub fn insert_item(conn: &Connection, item: &NewItem<'_>) -> Result<i64> {
    let size = i64::try_from(item.size)
        .map_err(|_| CoreError::Invalid(format!("ファイルが大きすぎます: {}", item.size)))?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO items (hash, ext, kind, mime, size_bytes, title, original_name, original_path,
                            source_site, source_url, imported_at, updated_at,
                            width, height, duration_ms, captured_at, codec_info, proxy_state, analysis_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULLIF(?8, ''), ?15, ?16, ?9, ?9, ?10, ?11, ?12, ?13, ?14,
                 CASE WHEN ?3 IN ('video', 'audio') THEN 'unknown' ELSE 'not_needed' END,
                 CASE WHEN ?3 IN ('image', 'audio') THEN 'pending' ELSE 'not_needed' END)",
    )?;
    stmt.execute(params![
        &item.hash[..],
        item.ext,
        item.kind.as_str(),
        item.mime,
        size,
        item.title,
        item.original_name,
        item.original_path,
        item.now,
        item.info.and_then(|i| i.width),
        item.info.and_then(|i| i.height),
        item.info.and_then(|i| i.duration_ms),
        item.captured_at
            .or_else(|| item.info.and_then(|i| i.captured_at)),
        item.info.and_then(MediaInfo::codec_json),
        item.source_site,
        item.source_url,
    ])?;
    Ok(conn.last_insert_rowid())
}

/// ゴミ箱から戻す。
pub fn restore_item(conn: &Connection, id: i64, now: i64) -> Result<()> {
    let mut stmt =
        conn.prepare_cached("UPDATE items SET trashed_at = NULL, updated_at = ?2 WHERE id = ?1")?;
    stmt.execute(params![id, now])?;
    Ok(())
}

/// 仮想フォルダがあるか。
pub fn folder_exists(conn: &Connection, id: i64) -> Result<bool> {
    let mut stmt = conn.prepare_cached("SELECT 1 FROM folders WHERE id = ?1")?;
    Ok(stmt.exists([id])?)
}

/// `base` の下に `path` の順でフォルダをたどり、なければ作る。最後のフォルダの ID を返す（path が空なら base）。
pub fn ensure_folder_path(
    conn: &Connection,
    base: Option<i64>,
    path: &[String],
    now: i64,
) -> Result<Option<i64>> {
    let mut parent = base;
    for name in path {
        let name = nfc(name.trim());
        if name.is_empty() {
            continue;
        }
        let parent_key = parent.unwrap_or(0);
        let existing: Option<i64> = {
            let mut stmt = conn.prepare_cached(
                "SELECT id FROM folders WHERE ifnull(parent_id, 0) = ?1 AND name = ?2",
            )?;
            stmt.query_row(params![parent_key, name], |row| row.get(0))
                .optional()?
        };
        let id = match existing {
            Some(id) => id,
            None => {
                let pos: f64 = {
                    let mut stmt = conn.prepare_cached(
                        "SELECT coalesce(max(pos), 0) + 1 FROM folders WHERE ifnull(parent_id, 0) = ?1",
                    )?;
                    stmt.query_row([parent_key], |row| row.get(0))?
                };
                let mut stmt = conn.prepare_cached(
                    "INSERT INTO folders (parent_id, name, pos, created_at) VALUES (?1, ?2, ?3, ?4)",
                )?;
                stmt.execute(params![parent, name, pos, now])?;
                conn.last_insert_rowid()
            }
        };
        parent = Some(id);
    }
    Ok(parent)
}

/// アイテムを仮想フォルダに入れる（入っていれば何もしない）。
pub fn add_to_folder(conn: &Connection, folder_id: i64, item_id: i64) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO item_folders (folder_id, item_id) VALUES (?1, ?2)",
    )?;
    stmt.execute([folder_id, item_id])?;
    Ok(())
}

pub use crate::tags::ensure_tag;

/// アイテムにタグを付ける（付いていれば何もしない）。
pub fn add_tag(conn: &Connection, tag_id: i64, item_id: i64, source: &str) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO item_tags (tag_id, item_id, source) VALUES (?1, ?2, ?3)",
    )?;
    stmt.execute(params![tag_id, item_id, source])?;
    Ok(())
}

/// ユニットがあるか。
pub fn unit_exists(conn: &Connection, id: i64) -> Result<bool> {
    let mut stmt = conn.prepare_cached("SELECT 1 FROM units WHERE id = ?1")?;
    Ok(stmt.exists([id])?)
}

/// 取り込みで作る set ユニット（origin = import、並びは manual）。
pub fn create_import_unit(conn: &Connection, name: &str, now: i64) -> Result<i64> {
    create_unit(conn, name, "import", None, now)
}

/// 取り込み・ダウンロードで作る set ユニット（並びは manual）。`origin` は `import` / `download`。
pub fn create_unit(
    conn: &Connection,
    name: &str,
    origin: &str,
    source_url: Option<&str>,
    now: i64,
) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO units (kind, name, order_mode, origin, source_url, created_at, updated_at)
         VALUES ('set', ?1, 'manual', ?2, ?3, ?4, ?4)",
    )?;
    stmt.execute(params![name, origin, source_url, now])?;
    Ok(conn.last_insert_rowid())
}

/// set ユニットにアイテムを加える。表紙が空なら最初のメンバーを表紙にする。
pub fn add_unit_member(conn: &Connection, unit_id: i64, item_id: i64, pos: f64) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO unit_items (unit_id, item_id, pos) VALUES (?1, ?2, ?3)",
    )?;
    stmt.execute(params![unit_id, item_id, pos])?;
    let mut stmt = conn.prepare_cached(
        "UPDATE units SET cover_item_id = ?2 WHERE id = ?1 AND cover_item_id IS NULL",
    )?;
    stmt.execute([unit_id, item_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};
    use crate::tags::TagName;

    fn new_item<'a>(hash: &'a [u8; 32], name: &'a str) -> NewItem<'a> {
        NewItem {
            hash,
            ext: "jpg",
            kind: MediaKind::Image,
            mime: Some("image/jpeg"),
            size: 42,
            title: name,
            original_name: name,
            original_path: "/src/x.jpg",
            info: None,
            source_site: "local",
            source_url: None,
            captured_at: None,
            now: 100,
        }
    }

    /// フォント・3D はプロキシを作らず（not_needed）、解析もしない（not_needed）。種類ごとの状態は `MediaKind` の
    /// 性質と同じ（`items_proxy_av` に当たらない）。
    #[test]
    fn fonts_and_models_are_not_proxied() {
        let conn = test_conn();
        for (i, kind) in MediaKind::ALL.into_iter().enumerate() {
            let hash = [i as u8 + 1; 32];
            let id = insert_item(
                &conn,
                &NewItem {
                    kind,
                    ..new_item(&hash, kind.as_str())
                },
            )
            .unwrap();
            let (stored, proxy, analysis): (MediaKind, String, String) = conn
                .query_row(
                    "SELECT kind, proxy_state, analysis_state FROM items WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(stored, kind);
            let expected_proxy = if kind.needs_proxy() {
                "unknown"
            } else {
                "not_needed"
            };
            assert_eq!(proxy, expected_proxy, "{kind:?}");
            let analyzed = matches!(kind, MediaKind::Image | MediaKind::Audio);
            assert_eq!(
                analysis,
                if analyzed { "pending" } else { "not_needed" },
                "{kind:?}"
            );
        }
    }

    /// 持ち主は今 → 元 → 過去の版の順。元が複数なら ID の小さいアイテム、過去の版だけなら ID の小さいアイテムの新しい版。
    #[test]
    fn find_owner_looks_at_current_then_original_then_past_versions() {
        use crate::edit::testing::{insert_item as item, insert_version};
        let conn = test_conn();
        let (a, b) = (item(&conn, 1, [1; 32]), item(&conn, 2, [2; 32]));
        // A：元 [10]・版 1 [11]（消えた）・版 2 [12]（猶予中）・版 3 は今 [1]
        insert_version(&conn, a, 0, [10; 32], "hold", None);
        insert_version(&conn, a, 1, [11; 32], "released", None);
        insert_version(&conn, a, 2, [12; 32], "waiting", None);
        insert_version(&conn, a, 3, [1; 32], "hold", None);
        // B：A から別名で保存（版 0 は A の元 [10]）・版 1 は A の版 2 と同じ中身 [12]・版 2 は今 [2]
        insert_version(&conn, b, 0, [10; 32], "hold", None);
        insert_version(&conn, b, 1, [12; 32], "waiting", None);
        insert_version(&conn, b, 2, [2; 32], "hold", None);
        conn.execute("UPDATE items SET trashed_at = 5 WHERE id = 2", [])
            .unwrap();
        let owner = |hash: [u8; 32]| {
            find_owner(&conn, &hash)
                .unwrap()
                .map(|o| (o.id, o.role, o.trashed))
        };
        assert_eq!(owner([1; 32]), Some((a, OwnerRole::Current, false)));
        assert_eq!(owner([2; 32]), Some((b, OwnerRole::Current, true)));
        assert_eq!(owner([10; 32]), Some((a, OwnerRole::Original, false)));
        assert_eq!(
            owner([11; 32]),
            Some((a, OwnerRole::Past { version: 1 }, false))
        );
        assert_eq!(
            owner([12; 32]),
            Some((a, OwnerRole::Past { version: 2 }, false))
        );
        assert_eq!(owner([13; 32]), None);
        // A の元の持ち主を消すと、同じ元を持つ B（ゴミ箱）になる
        conn.execute("DELETE FROM items WHERE id = 1", []).unwrap();
        assert_eq!(owner([10; 32]), Some((b, OwnerRole::Original, true)));
        assert_eq!(
            owner([12; 32]),
            Some((b, OwnerRole::Past { version: 1 }, true))
        );
        for (sql, want) in [
            (
                "SELECT id, ext, trashed_at IS NOT NULL FROM items WHERE hash = x'01'",
                "sqlite_autoindex_items_1",
            ),
            (
                "SELECT v.item_id, i.ext, i.trashed_at IS NOT NULL, v.version
                   FROM item_versions v JOIN items i ON i.id = v.item_id
                  WHERE v.output_hash = x'01'
                  ORDER BY v.version <> 0, v.item_id, v.version DESC LIMIT 1",
                "idx_item_versions_output",
            ),
        ] {
            let plan = query_plan(&conn, sql);
            assert!(plan.contains(want), "{sql}\n{plan}");
            assert!(!plan.contains("SCAN"), "{sql}\n{plan}");
        }
    }

    #[test]
    fn inserts_and_finds_items_by_hash() {
        let conn = test_conn();
        let hash = [7u8; 32];
        assert_eq!(find_by_hash(&conn, &hash).unwrap(), None);
        let id = insert_item(&conn, &new_item(&hash, "夕方の海")).unwrap();
        assert_eq!(
            find_by_hash(&conn, &hash).unwrap(),
            Some(ExistingItem {
                id,
                ext: "jpg".into(),
                trashed: false
            })
        );
        let (kind, source_site, imported_at, title): (String, String, i64, String) = conn
            .query_row(
                "SELECT kind, source_site, imported_at, title FROM items WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (kind.as_str(), source_site.as_str(), imported_at),
            ("image", "local", 100)
        );
        assert_eq!(title, "夕方の海");

        conn.execute("UPDATE items SET trashed_at = 5 WHERE id = ?1", [id])
            .unwrap();
        assert!(find_by_hash(&conn, &hash).unwrap().unwrap().trashed);
        restore_item(&conn, id, 200).unwrap();
        assert!(!find_by_hash(&conn, &hash).unwrap().unwrap().trashed);

        // 台帳にも載る（重複の見込みは台帳から引く。library::ledger::candidates_by_size）
        assert_eq!(
            crate::library::ledger::candidates_by_size(&conn, 42).unwrap(),
            vec![(hash, "jpg".into())]
        );
        assert!(
            crate::library::ledger::candidates_by_size(&conn, 41)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn folder_paths_are_created_once() {
        let conn = test_conn();
        let path = vec![
            "写真".to_string(),
            "2026 旅行".to_string(),
            "鎌倉".to_string(),
        ];
        let id = ensure_folder_path(&conn, None, &path, 1).unwrap().unwrap();
        assert_eq!(ensure_folder_path(&conn, None, &path, 2).unwrap(), Some(id));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM folders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);

        // 取り込み先の下に作る。並び順は末尾に足していく
        let photos = ensure_folder_path(&conn, None, &path[..1], 3)
            .unwrap()
            .unwrap();
        let a = ensure_folder_path(&conn, Some(photos), &["a".to_string()], 4)
            .unwrap()
            .unwrap();
        let b = ensure_folder_path(&conn, Some(photos), &["b".to_string()], 5)
            .unwrap()
            .unwrap();
        let pos = |id: i64| -> f64 {
            conn.query_row("SELECT pos FROM folders WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert!(pos(b) > pos(a));
        assert_eq!(
            ensure_folder_path(&conn, Some(photos), &[], 6).unwrap(),
            Some(photos)
        );
        assert_eq!(ensure_folder_path(&conn, None, &[], 6).unwrap(), None);
        assert!(folder_exists(&conn, photos).unwrap());
        assert!(!folder_exists(&conn, 999).unwrap());

        let item = insert_item(&conn, &new_item(&[1u8; 32], "x")).unwrap();
        add_to_folder(&conn, a, item).unwrap();
        add_to_folder(&conn, a, item).unwrap();
        let members: i64 = conn
            .query_row("SELECT COUNT(*) FROM item_folders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(members, 1);
    }

    #[test]
    fn tags_resolve_aliases_and_namespaces() {
        let conn = test_conn();
        let plain = ensure_tag(&conn, &TagName::plain("夕景"), 1).unwrap();
        assert_eq!(
            ensure_tag(&conn, &TagName::plain("夕景"), 2).unwrap(),
            plain
        );

        let author = TagName {
            namespace: "作者".into(),
            name: "もりの".into(),
        };
        let author_id = ensure_tag(&conn, &author, 3).unwrap();
        assert_ne!(author_id, plain);

        conn.execute(
            "INSERT INTO tag_aliases (alias, tag_id) VALUES ('夕焼け', ?1)",
            [plain],
        )
        .unwrap();
        assert_eq!(
            ensure_tag(&conn, &TagName::plain("夕焼け"), 4).unwrap(),
            plain
        );

        let item = insert_item(&conn, &new_item(&[2u8; 32], "x")).unwrap();
        add_tag(&conn, plain, item, "import").unwrap();
        add_tag(&conn, plain, item, "import").unwrap();
        let source: String = conn
            .query_row(
                "SELECT source FROM item_tags WHERE item_id = ?1",
                [item],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source, "import");
    }

    #[test]
    fn import_units_keep_member_order_and_cover() {
        let conn = test_conn();
        let unit = create_import_unit(&conn, "鎌倉", 1).unwrap();
        assert!(unit_exists(&conn, unit).unwrap());
        let a = insert_item(&conn, &new_item(&[3u8; 32], "a")).unwrap();
        let b = insert_item(&conn, &new_item(&[4u8; 32], "b")).unwrap();
        add_unit_member(&conn, unit, a, 1.0).unwrap();
        add_unit_member(&conn, unit, b, 2.0).unwrap();
        add_unit_member(&conn, unit, a, 1.0).unwrap();

        let (kind, origin, order_mode, cover): (String, String, String, i64) = conn
            .query_row(
                "SELECT kind, origin, order_mode, cover_item_id FROM units WHERE id = ?1",
                [unit],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (kind.as_str(), origin.as_str(), order_mode.as_str()),
            ("set", "import", "manual")
        );
        assert_eq!(cover, a);
        let members: Vec<i64> = conn
            .prepare("SELECT item_id FROM unit_items WHERE unit_id = ?1 ORDER BY pos")
            .unwrap()
            .query_map([unit], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(members, vec![a, b]);
    }

    #[test]
    fn lookups_use_indexes() {
        let conn = test_conn();
        let by_hash = query_plan(
            &conn,
            "SELECT id, ext, trashed_at FROM items WHERE hash = x'00'",
        );
        assert!(
            by_hash.contains("USING INDEX sqlite_autoindex_items_1"),
            "{by_hash}"
        );
        let by_size = query_plan(&conn, "SELECT hash, ext FROM items WHERE size_bytes = 1");
        assert!(by_size.contains("idx_items_size"), "{by_size}");
        let folder = query_plan(
            &conn,
            "SELECT id FROM folders WHERE ifnull(parent_id, 0) = 0 AND name = 'a'",
        );
        assert!(folder.contains("idx_folders_name"), "{folder}");
        let tag = query_plan(
            &conn,
            "SELECT id FROM tags WHERE namespace = '' AND name = 'a'",
        );
        assert!(tag.contains("sqlite_autoindex_tags_1"), "{tag}");
    }
}
