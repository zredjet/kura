//! 仮想フォルダの読み取り（取り込み先の選択肢など）。作成・改名・移動は M3。

use crate::Result;
use crate::db::Db;

/// 仮想フォルダ 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct FolderNode {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub pos: f64,
}

/// すべての仮想フォルダ（親 → 並び順）。木はフロントで組み立てる。
pub fn tree(db: &Db) -> Result<Vec<FolderNode>> {
    db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT id, parent_id, name, color, pos FROM folders ORDER BY ifnull(parent_id, 0), pos, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FolderNode {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                name: row.get(2)?,
                color: row.get(3)?,
                pos: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;

    #[test]
    fn lists_folders_with_parents() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        lib.db()
            .write(|tx| {
                tx.execute_batch(
                    "INSERT INTO folders (id, parent_id, name, pos, created_at) VALUES (1, NULL, '写真', 1, 0);
                     INSERT INTO folders (id, parent_id, name, pos, created_at) VALUES (2, 1, '2026 旅行', 1, 0);
                     INSERT INTO folders (id, parent_id, name, pos, created_at) VALUES (3, NULL, 'イラスト', 2, 0);",
                )?;
                Ok(())
            })
            .unwrap();
        let nodes = tree(lib.db()).unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["写真", "イラスト", "2026 旅行"]);
        assert_eq!(nodes[2].parent_id, Some(1));
    }
}
