//! クレジットを集める（M15 段 1。docs/data-model.md §4「ライセンスとクレジット（M15）」、ADR-202）。
//!
//! 使ったアイテムごとに、自分と来歴の元（別名の `item_derivations`・N → 1 の `item_sources`。元の元もたどる）を見て、
//! ライセンスのあるクレジットを (ライセンス・作品・作者・出典・表記の文) の鍵でまとめる（同じ素材集の 100 件も、別名で写した
//! クレジットも 1 行。段 1 の決定 2）。来歴の元がなくライセンスも未設定のアイテムは「未設定」、元のアイテムが消えて来歴を
//! たどれないアイテムは「元が消えた」として返す（一覧の警告の節。黙って落とさない）。出典はクレジットの URL、なければ
//! アイテムの取り込み元の URL。並べ方は呼び手が決め（[`collect`] はライセンスの並び → 鍵の順）、集め方は [`assemble`]
//! （DB に触れない）。

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension};

use super::licenses::{self, License};
use crate::error::Result;

/// ライセンスのあるクレジットの鍵（同じ鍵のクレジットは 1 行にまとめる）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CreditKey {
    pub license_id: i64,
    pub work: Option<String>,
    pub author: Option<String>,
    /// 出典（クレジットの URL、なければアイテムの取り込み元の URL）。
    pub url: Option<String>,
    /// 表記の文。
    pub credit: Option<String>,
}

/// 来歴のグラフの節（1 件のアイテム）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    /// ライセンスのあるクレジット（ライセンスが未設定なら None）。
    pub credit: Option<CreditKey>,
    /// 来歴の元（別名の元・N → 1 の元の順）。None は消えた元。
    pub sources: Vec<Option<i64>>,
}

/// まとめた 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditLine {
    pub key: CreditKey,
    /// このクレジットを持つアイテム（来歴の元を含む。小さい順）。
    pub items: Vec<i64>,
    /// それにたどり着いた、使ったアイテム（小さい順）。
    pub used: Vec<i64>,
}

/// 警告の 1 件（未設定・元が消えた）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flagged {
    pub item: i64,
    /// それにたどり着いた、使ったアイテム（小さい順）。
    pub used: Vec<i64>,
}

/// 集めた結果（行は鍵の順）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assembled {
    pub lines: Vec<CreditLine>,
    /// 来歴の元がなく、ライセンスも未設定のアイテム。
    pub unset: Vec<Flagged>,
    /// 来歴の元が消えた（たどれない）アイテム。
    pub lost: Vec<Flagged>,
}

/// 使ったアイテム `used` から来歴をたどってクレジットを集める（DB に触れない）。`nodes` にないアイテムは、使ったものなら
/// 飛ばし、元なら消えた元とみなす。
pub fn assemble(nodes: &BTreeMap<i64, Node>, used: &[i64]) -> Assembled {
    let mut lines: BTreeMap<CreditKey, (BTreeSet<i64>, BTreeSet<i64>)> = BTreeMap::new();
    let mut unset: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    let mut lost: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    let used: BTreeSet<i64> = used.iter().copied().collect();
    for &u in &used {
        if !nodes.contains_key(&u) {
            continue;
        }
        let mut seen = BTreeSet::new();
        let mut stack = vec![u];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(node) = nodes.get(&id) else {
                continue;
            };
            if let Some(key) = &node.credit {
                let (items, by) = lines.entry(key.clone()).or_default();
                items.insert(id);
                by.insert(u);
            } else if node.sources.is_empty() {
                unset.entry(id).or_default().insert(u);
            }
            for source in &node.sources {
                match source {
                    Some(s) if nodes.contains_key(s) => stack.push(*s),
                    _ => {
                        lost.entry(id).or_default().insert(u);
                    }
                }
            }
        }
    }
    let flagged = |m: BTreeMap<i64, BTreeSet<i64>>| -> Vec<Flagged> {
        m.into_iter()
            .map(|(item, used)| Flagged {
                item,
                used: used.into_iter().collect(),
            })
            .collect()
    };
    Assembled {
        lines: lines
            .into_iter()
            .map(|(key, (items, used))| CreditLine {
                key,
                items: items.into_iter().collect(),
                used: used.into_iter().collect(),
            })
            .collect(),
        unset: flagged(unset),
        lost: flagged(lost),
    }
}

/// 集めたクレジット（行はライセンスの並び → 鍵の順）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credits {
    pub lines: Vec<CreditLine>,
    /// 行が使うライセンス（並びの順）。
    pub licenses: Vec<License>,
    pub unset: Vec<Flagged>,
    pub lost: Vec<Flagged>,
}

/// 来歴のグラフを DB から読む（使ったアイテムから元へ）。ないアイテムは入れない。
pub fn load(conn: &Connection, used: &[i64]) -> Result<BTreeMap<i64, Node>> {
    let mut credit = conn.prepare_cached(
        "SELECT c.license_id, c.work, c.author, coalesce(c.url, i.source_url), c.credit
           FROM items i LEFT JOIN item_credits c ON c.item_id = i.id
          WHERE i.id = ?1",
    )?;
    let mut derived =
        conn.prepare_cached("SELECT source_item_id FROM item_derivations WHERE item_id = ?1")?;
    let mut merged = conn.prepare_cached(
        "SELECT source_item_id FROM item_sources WHERE item_id = ?1 ORDER BY ord",
    )?;
    let mut nodes = BTreeMap::new();
    let mut todo: Vec<i64> = used.to_vec();
    while let Some(id) = todo.pop() {
        if nodes.contains_key(&id) {
            continue;
        }
        type Row = (
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let row: Option<Row> = credit
            .query_row([id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()?;
        let Some((license, work, author, url, text)) = row else {
            continue;
        };
        let mut sources = Vec::new();
        for s in derived.query_map([id], |r| r.get::<_, Option<i64>>(0))? {
            sources.push(s?);
        }
        for s in merged.query_map([id], |r| r.get::<_, Option<i64>>(0))? {
            sources.push(s?);
        }
        todo.extend(sources.iter().flatten().copied());
        nodes.insert(
            id,
            Node {
                credit: license.map(|license_id| CreditKey {
                    license_id,
                    work,
                    author,
                    url,
                    credit: text,
                }),
                sources,
            },
        );
    }
    Ok(nodes)
}

/// 使ったアイテム `used` のクレジットを集める（来歴の元をたどる）。
pub fn collect(conn: &Connection, used: &[i64]) -> Result<Credits> {
    let nodes = load(conn, used)?;
    let assembled = assemble(&nodes, used);
    let all = licenses::list(conn)?;
    let order: BTreeMap<i64, usize> = all.iter().enumerate().map(|(k, l)| (l.id, k)).collect();
    let mut lines = assembled.lines;
    // ライセンスの並び → 鍵の順（assemble は鍵の順なので、並べ替えは安定）
    lines.sort_by_key(|l| order.get(&l.key.license_id).copied().unwrap_or(usize::MAX));
    let used_licenses: BTreeSet<i64> = lines.iter().map(|l| l.key.license_id).collect();
    Ok(Credits {
        lines,
        licenses: all
            .into_iter()
            .filter(|l| used_licenses.contains(&l.id))
            .collect(),
        unset: assembled.unset,
        lost: assembled.lost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credits::item::{self, CreditPatch};
    use crate::db::test_conn;
    use rusqlite::params;

    fn key(license_id: i64, author: &str) -> CreditKey {
        CreditKey {
            license_id,
            work: None,
            author: Some(author.into()),
            url: None,
            credit: None,
        }
    }

    fn node(credit: Option<CreditKey>, sources: &[Option<i64>]) -> Node {
        Node {
            credit,
            sources: sources.to_vec(),
        }
    }

    #[test]
    fn same_credits_collapse_and_provenance_is_followed() {
        // 1・2・3：同じ素材集（同じ鍵）。4：1 の別名（写した同じ鍵）。5：別のライセンスに変えた 1 の別名。
        // 6：N → 1 のシート（元は 2 と 7、消えた元 1 つ）。7：未設定の根。8：使わない
        let nodes: BTreeMap<i64, Node> = [
            (1, node(Some(key(2, "もりの")), &[])),
            (2, node(Some(key(2, "もりの")), &[])),
            (3, node(Some(key(2, "もりの")), &[])),
            (4, node(Some(key(2, "もりの")), &[Some(1)])),
            (5, node(Some(key(9, "わたし")), &[Some(1)])),
            (6, node(None, &[Some(2), Some(7), None])),
            (7, node(None, &[])),
            (8, node(Some(key(3, "ほか")), &[])),
        ]
        .into_iter()
        .collect();
        let got = assemble(&nodes, &[3, 4, 5, 6, 6, 99]);
        assert_eq!(
            got.lines,
            vec![
                CreditLine {
                    key: key(2, "もりの"),
                    items: vec![1, 2, 3, 4],
                    used: vec![3, 4, 5, 6],
                },
                CreditLine {
                    key: key(9, "わたし"),
                    items: vec![5],
                    used: vec![5],
                },
            ]
        );
        assert_eq!(
            got.unset,
            vec![Flagged {
                item: 7,
                used: vec![6]
            }]
        );
        assert_eq!(
            got.lost,
            vec![Flagged {
                item: 6,
                used: vec![6]
            }]
        );
        // 何も使わなければ空
        assert_eq!(assemble(&nodes, &[]), Assembled::default());
    }

    #[test]
    fn missing_sources_count_as_lost_and_cycles_end() {
        let nodes: BTreeMap<i64, Node> = [
            (1, node(None, &[Some(2)])),
            (2, node(None, &[Some(1), Some(42)])),
        ]
        .into_iter()
        .collect();
        let got = assemble(&nodes, &[1]);
        assert!(got.lines.is_empty());
        assert!(got.unset.is_empty());
        assert_eq!(
            got.lost,
            vec![Flagged {
                item: 2,
                used: vec![1]
            }]
        );
    }

    fn insert_item(conn: &Connection, id: i64, url: Option<&str>) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, source_url, imported_at, updated_at)
             VALUES (?1, ?2, 'png', 'image', 1, 't', 't', ?3, 0, 0)",
            params![id, vec![id as u8; 32], url],
        )
        .unwrap();
    }

    fn builtin(conn: &Connection, key: &str) -> i64 {
        conn.query_row("SELECT id FROM licenses WHERE builtin = ?1", [key], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn collect_reads_provenance_from_the_db_and_orders_by_license() {
        let conn = test_conn();
        for id in 1..=6 {
            insert_item(&conn, id, Some("https://example.com/pack"));
        }
        let by = builtin(&conn, "cc-by-4.0");
        let cc0 = builtin(&conn, "cc0-1.0");
        let set = |items: &[i64], license: i64, author: &str| {
            item::set(
                &conn,
                items,
                &CreditPatch {
                    license: Some(Some(license)),
                    author: Some(Some(author.into())),
                    ..CreditPatch::default()
                },
                1,
            )
            .unwrap();
        };
        set(&[1, 2], by, "もりの");
        set(&[3], cc0, "ケニー");
        // 4 は 1 の別名（写したクレジット）、5 は 2・3 と消えた元からの N → 1 のシート、6 は未設定
        item::copy(&conn, 1, 4, 2).unwrap();
        conn.execute_batch(
            "INSERT INTO item_derivations (item_id, source_item_id, source_version, source_hash, created_at)
               VALUES (4, 1, 0, x'01', 2);
             INSERT INTO item_sources (item_id, ord, source_item_id, source_version, source_hash, frames, created_at)
               VALUES (5, 0, 3, 0, x'03', 1, 3), (5, 1, 2, 0, x'02', 1, 3), (5, 2, NULL, 0, x'09', 1, 3);",
        )
        .unwrap();
        let got = collect(&conn, &[4, 5, 6]).unwrap();
        // 並びはライセンスの並び（CC0 が先）。出典はアイテムの取り込み元の URL
        type Line<'a> = (i64, Option<&'a str>, Option<&'a str>, Vec<i64>, Vec<i64>);
        let lines: Vec<Line<'_>> = got
            .lines
            .iter()
            .map(|l| {
                (
                    l.key.license_id,
                    l.key.author.as_deref(),
                    l.key.url.as_deref(),
                    l.items.clone(),
                    l.used.clone(),
                )
            })
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    cc0,
                    Some("ケニー"),
                    Some("https://example.com/pack"),
                    vec![3],
                    vec![5]
                ),
                (
                    by,
                    Some("もりの"),
                    Some("https://example.com/pack"),
                    vec![1, 2, 4],
                    vec![4, 5]
                ),
            ]
        );
        let names: Vec<&str> = got.licenses.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["CC0 1.0", "CC BY 4.0"]);
        assert_eq!(
            got.unset,
            vec![Flagged {
                item: 6,
                used: vec![6]
            }]
        );
        assert_eq!(
            got.lost,
            vec![Flagged {
                item: 5,
                used: vec![5]
            }]
        );
        // クレジットの出典を書けば、取り込み元より先に使う
        item::set(
            &conn,
            &[3],
            &CreditPatch {
                url: Some(Some("https://kenney.nl".into())),
                ..CreditPatch::default()
            },
            4,
        )
        .unwrap();
        let got = collect(&conn, &[3]).unwrap();
        assert_eq!(got.lines[0].key.url.as_deref(), Some("https://kenney.nl"));
        // 元のアイテムを消すと、別名は自分の（写した）クレジットだけになり、元が消えたことを知らせる
        conn.execute("DELETE FROM items WHERE id = 1", []).unwrap();
        let got = collect(&conn, &[4]).unwrap();
        assert_eq!(got.lines.len(), 1);
        assert_eq!(got.lines[0].items, vec![4]);
        assert_eq!(
            got.lost,
            vec![Flagged {
                item: 4,
                used: vec![4]
            }]
        );
    }

    #[test]
    fn load_uses_the_primary_keys_and_indexes() {
        let conn = test_conn();
        let plan = crate::db::query_plan(
            &conn,
            "SELECT c.license_id, c.work, c.author, coalesce(c.url, i.source_url), c.credit
               FROM items i LEFT JOIN item_credits c ON c.item_id = i.id WHERE i.id = 1",
        );
        assert!(!plan.contains("SCAN"), "{plan}");
        let plan = crate::db::query_plan(
            &conn,
            "SELECT source_item_id FROM item_derivations WHERE item_id = 1",
        );
        assert!(!plan.contains("SCAN"), "{plan}");
        let plan = crate::db::query_plan(
            &conn,
            "SELECT source_item_id FROM item_sources WHERE item_id = 1 ORDER BY ord",
        );
        assert!(
            !plan.contains("SCAN") && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );
    }
}
