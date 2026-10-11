//! スマートプレイリストの並びを作る（docs/playlists.md §4）。DB から候補を読み、`smart` の判定と
//! `build` の並べ替えを使って、エントリにする並びを返す（並びはエントリとして保存する。ADR）。

use std::collections::HashSet;

use rusqlite::{Connection, params_from_iter};

use super::build::{Element, kind_code, order, representative_kind, weight};
use super::model::{EntryTarget, Granularity, LimitKind};
use super::smart::{UnitCond, UnitFacts, unit_matches};
use super::store::{NewEntry, Playlist};
use crate::Result;
use crate::items::{ItemQuery, ItemSort, ItemView, SortKey, query_ids};
use crate::query::ast::{Expr, FieldCond, Op};
use crate::query::{DbResolver, QueryContext, Resolver, compile};
use crate::random::rng;
use crate::text::natural_cmp;
use crate::unit::{
    aggregate, all_ids, load_member_snapshot, own_tag_ids, play_info, resolve_members,
};

/// 候補 1 つ（ユニットか、単独のアイテム）。
struct Candidate {
    target: EntryTarget,
    name: String,
    kind: u8,
    rating: i64,
    last_played_at: Option<i64>,
    duration_ms: i64,
    created_at: i64,
    count: usize,
}

/// スマートプレイリストの並びを作る。シャッフルなら `seed` で並べ、上限まで取る。
pub fn generate(
    conn: &Connection,
    playlist: &Playlist,
    seed: u64,
    ctx: &QueryContext,
) -> Result<Vec<NewEntry>> {
    let rule = playlist.rule.clone().unwrap_or_default();
    let expr = rule.expr()?;
    let candidates = match playlist.granularity {
        Granularity::File => item_candidates(conn, &expr, playlist, ctx, None)?,
        Granularity::Unit => unit_candidates(conn, &expr, playlist, ctx)?,
    };
    let ordered = if playlist.shuffle {
        let elements = candidates
            .into_iter()
            .map(|c| Element {
                weight: weight(c.rating, c.last_played_at, ctx.now_ms),
                kind: c.kind,
                value: c,
            })
            .collect();
        order(
            elements,
            playlist.weight_by_rating,
            playlist.max_same_kind_run,
            &mut rng(seed),
        )
    } else {
        candidates
    };
    Ok(take_limit(ordered, playlist.limit)
        .into_iter()
        .map(|c| NewEntry::from(c.target))
        .collect())
}

/// 上限まで取る：件数ならその数、合計時間なら超えない範囲で先頭から（§4.1）。
fn take_limit(ordered: Vec<Candidate>, limit: Option<(LimitKind, i64)>) -> Vec<Candidate> {
    match limit {
        None => ordered,
        Some((LimitKind::Count, n)) => ordered
            .into_iter()
            .take(usize::try_from(n).unwrap_or(0))
            .collect(),
        Some((LimitKind::Duration, max)) => {
            let mut total = 0i64;
            ordered
                .into_iter()
                .take_while(|c| {
                    total += c.duration_ms;
                    total <= max
                })
                .collect()
        }
    }
}

/// 条件に合うアイテム（ゴミ箱以外）を、プレイリストの並び順で。`exclude` に入っているものは除く。
fn item_candidates(
    conn: &Connection,
    expr: &Expr,
    playlist: &Playlist,
    ctx: &QueryContext,
    exclude: Option<&HashSet<i64>>,
) -> Result<Vec<Candidate>> {
    let list = query_ids(
        conn,
        &ItemQuery {
            view: ItemView::All,
            collapse_units: false,
            filter: Some(expr.clone()),
            sort: playlist.sort,
        },
        ctx,
    )?;
    let ids: Vec<i64> = list
        .ids
        .iter()
        .map(|id| i64::from(*id))
        .filter(|id| exclude.is_none_or(|e| !e.contains(id)))
        .collect();
    let snap = load_member_snapshot(conn, ids.iter().copied().collect(), &[])?;
    Ok(ids
        .into_iter()
        .filter_map(|id| {
            let info = snap.items.get(&id)?;
            let duration_ms = if info.kind == "image" {
                playlist.image_duration_ms
            } else {
                info.duration_ms.unwrap_or(0)
            };
            Some(Candidate {
                target: EntryTarget::Item(id),
                name: info.title.clone(),
                kind: kind_code(&info.kind),
                rating: info.rating,
                last_played_at: info.last_played_at,
                duration_ms,
                created_at: info.imported_at,
                count: 1,
            })
        })
        .collect())
}

/// 条件に合うユニット（と、`include_loose_items` ならどのユニットにも属さないアイテム）。
fn unit_candidates(
    conn: &Connection,
    expr: &Expr,
    playlist: &Playlist,
    ctx: &QueryContext,
) -> Result<Vec<Candidate>> {
    let cond = to_unit_cond(conn, expr, ctx)?;
    let mut infos = Vec::new();
    let mut item_ids: HashSet<i64> = HashSet::new();
    let mut folders: Vec<(i64, bool)> = Vec::new();
    for id in all_ids(conn)? {
        if let Some(info) = play_info(conn, id)? {
            item_ids.extend(info.def.items.iter().map(|(i, _)| *i));
            folders.extend(info.def.folders.iter().map(|f| (f.folder_id, f.recursive)));
            infos.push((id, info));
        }
    }
    let snap = load_member_snapshot(conn, item_ids, &folders)?;
    let mut tags_of = conn.prepare_cached("SELECT tag_id FROM item_tags WHERE item_id = ?1")?;
    let mut in_units: HashSet<i64> = HashSet::new();
    let mut out = Vec::new();
    for (id, info) in infos {
        let members = resolve_members(&info.def, &snap);
        in_units.extend(members.iter().copied());
        if members.is_empty() {
            continue;
        }
        let image_ms = info.image_duration_ms.unwrap_or(playlist.image_duration_ms);
        let stats = aggregate(&members, &snap, info.rating, image_ms);
        let mut member_tags = Vec::with_capacity(members.len());
        for m in &members {
            let rows = tags_of.query_map([m], |r| r.get::<_, i64>(0))?;
            member_tags.push(rows.collect::<std::result::Result<HashSet<_>, _>>()?);
        }
        let facts = UnitFacts {
            own_tags: own_tag_ids(conn, id)?.into_iter().collect(),
            member_tags,
            kinds: stats.kinds.iter().map(|k| (*k).to_string()).collect(),
            rating: stats.rating,
            duration_ms: stats.duration_ms,
            play_count: info.play_count,
            last_played_at: info.last_played_at,
            created_at: info.created_at,
            name: info.name.clone(),
            source_url: info.source_url.clone(),
            note: info.note.clone(),
            folders: info.def.folders.iter().map(|f| f.folder_id).collect(),
            members: members.clone(),
        };
        if !unit_matches(&cond, &facts, playlist.unit_match, ctx) {
            continue;
        }
        out.push(Candidate {
            target: EntryTarget::Unit(id),
            name: info.name,
            kind: representative_kind(
                members
                    .iter()
                    .filter_map(|m| snap.items.get(m))
                    .map(|m| m.kind.as_str()),
            ),
            rating: stats.rating,
            last_played_at: info.last_played_at,
            duration_ms: stats.duration_ms,
            created_at: info.created_at,
            count: stats.count,
        });
    }
    if playlist.include_loose_items {
        out.extend(item_candidates(conn, expr, playlist, ctx, Some(&in_units))?);
    }
    sort_candidates(&mut out, playlist.sort);
    Ok(out)
}

/// ユニットの並び順（シャッフルしないとき）：追加日・撮影日は作成日、名前は自然順、評価・長さは集計値、
/// サイズはメンバー数。`desc` なら逆順。同じなら名前、さらに同じなら元の順。
fn sort_candidates(list: &mut [Candidate], sort: ItemSort) {
    list.sort_by(|a, b| {
        let primary = match sort.key {
            SortKey::Imported | SortKey::Captured => a.created_at.cmp(&b.created_at),
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Rating => a.rating.cmp(&b.rating),
            SortKey::Duration => a.duration_ms.cmp(&b.duration_ms),
            SortKey::Size => a.count.cmp(&b.count),
        };
        let primary = if sort.desc {
            primary.reverse()
        } else {
            primary
        };
        primary.then_with(|| natural_cmp(&a.name, &b.name))
    });
}

/// 検索の AST を、ユニットで判定できる形にする（タグ・フォルダを ID に、メンバーで判定する項目をアイテムの集合に）。
pub fn to_unit_cond(conn: &Connection, expr: &Expr, ctx: &QueryContext) -> Result<UnitCond> {
    let resolver = DbResolver { conn };
    Ok(match expr {
        Expr::And(parts) => UnitCond::And(
            parts
                .iter()
                .map(|p| to_unit_cond(conn, p, ctx))
                .collect::<Result<_>>()?,
        ),
        Expr::Or(parts) => UnitCond::Or(
            parts
                .iter()
                .map(|p| to_unit_cond(conn, p, ctx))
                .collect::<Result<_>>()?,
        ),
        Expr::Not(inner) => UnitCond::Not(Box::new(to_unit_cond(conn, inner, ctx)?)),
        // 語（引用符で囲んだ語も。search-query.md §2）
        Expr::Tag(_) | Expr::Text(_) => {
            let Some(term) = expr.tag_term() else {
                return Ok(UnitCond::Tags(HashSet::new()));
            };
            let ids = if term.ns.is_empty() {
                resolver.word_tags(&term.name)?
            } else {
                resolver.ns_tags(&term.ns, &term.name)?
            };
            match ids {
                Some(ids) => UnitCond::Tags(ids.into_iter().collect()),
                // 名前空間なしでタグがなければ語として（ユニットの名前・URL・メモ）
                None if term.ns.is_empty() => UnitCond::Text(term.name.clone()),
                None => UnitCond::Tags(HashSet::new()),
            }
        }
        Expr::Field(field) => match field {
            FieldCond::Type { .. }
            | FieldCond::Rating { .. }
            | FieldCond::Dur { .. }
            | FieldCond::Plays { .. }
            | FieldCond::Added { .. }
            | FieldCond::Played { .. }
            | FieldCond::Name { .. }
            | FieldCond::Url { .. }
            | FieldCond::Note { .. }
            | FieldCond::Unit { .. } => UnitCond::Field(field.clone()),
            FieldCond::Folder { op, value } => {
                let folders: HashSet<i64> = resolver
                    .folders(value, *op != Op::Eq)?
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                let items = items_in_folders(conn, &folders)?;
                let cond = UnitCond::Folder { folders, items };
                if *op == Op::Ne {
                    UnitCond::Not(Box::new(cond))
                } else {
                    cond
                }
            }
            // §4.2 にない項目（撮影日・向き・取得元・幅・高さ・タグの数）はメンバーのどれかで判定する
            _ => UnitCond::AnyMember(matching_items(conn, expr, ctx)?),
        },
    })
}

/// フォルダに直接入っているアイテム。
fn items_in_folders(conn: &Connection, folders: &HashSet<i64>) -> Result<HashSet<i64>> {
    let mut stmt = conn.prepare_cached("SELECT item_id FROM item_folders WHERE folder_id = ?1")?;
    let mut out = HashSet::new();
    for folder in folders {
        let rows = stmt.query_map([folder], |r| r.get::<_, i64>(0))?;
        for row in rows {
            out.insert(row?);
        }
    }
    Ok(out)
}

/// 条件に合うアイテム（ゴミ箱以外）の集合。
fn matching_items(conn: &Connection, expr: &Expr, ctx: &QueryContext) -> Result<HashSet<i64>> {
    let cond = compile(expr, &DbResolver { conn }, ctx)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT i.id FROM items i WHERE i.trashed_at IS NULL AND ({})",
        cond.sql
    ))?;
    let rows = stmt.query_map(params_from_iter(cond.params.iter()), |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
