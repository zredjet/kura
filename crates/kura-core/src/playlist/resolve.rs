//! エントリをアイテム列に展開する（docs/playlists.md §5.1・§5.2 の 1）。DB に触れない純粋関数。

use std::collections::HashMap;

use super::model::{Entry, EntryTarget};
use crate::queue::UnitKey;
use crate::unit::{MemberSnapshot, OrderMode, UnitDef, UnitFolder, UnitKind, resolve_members};

/// キューを作るのに要るユニットの情報。
#[derive(Debug, Clone, PartialEq)]
pub struct UnitData {
    pub def: UnitDef,
    pub name: String,
    pub image_duration_ms: Option<i64>,
    pub inner_shuffle: bool,
    /// ユニット自体の評価（0 は未評価）。
    pub rating: i64,
    pub last_played_at: Option<i64>,
}

/// DB から読んだ、解決とキューの生成に要るデータ。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// エントリ（並び順は `pos`）。
    pub entries: Vec<Entry>,
    pub units: HashMap<i64, UnitData>,
    /// アイテムの情報とフォルダの木（unit::resolve_members と同じもの）。
    pub members: MemberSnapshot,
}

/// 展開したエントリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEntry {
    pub entry_id: i64,
    /// unit / folder エントリならその鍵。item エントリは None。
    pub unit_key: Option<UnitKey>,
    pub items: Vec<i64>,
}

/// エントリを `pos` の順に展開する。ゴミ箱・存在しないアイテムは除き、空になったエントリは捨てる。
pub fn resolve(snap: &Snapshot) -> Vec<ResolvedEntry> {
    let mut entries: Vec<&Entry> = snap.entries.iter().collect();
    entries.sort_by(|a, b| a.pos.total_cmp(&b.pos).then_with(|| a.id.cmp(&b.id)));
    entries
        .into_iter()
        .filter_map(|entry| {
            let (unit_key, items) = expand(snap, entry.target);
            (!items.is_empty()).then_some(ResolvedEntry {
                entry_id: entry.id,
                unit_key,
                items,
            })
        })
        .collect()
}

/// 1 つのエントリの中身（§2.2・§3.2）。
pub fn expand(snap: &Snapshot, target: EntryTarget) -> (Option<UnitKey>, Vec<i64>) {
    match target {
        EntryTarget::Item(id) => {
            let alive = snap
                .members
                .items
                .get(&id)
                .is_some_and(|info| !info.trashed);
            (None, if alive { vec![id] } else { Vec::new() })
        }
        EntryTarget::Unit(unit_id) => (
            Some(UnitKey::Unit(unit_id)),
            snap.units
                .get(&unit_id)
                .map(|unit| resolve_members(&unit.def, &snap.members))
                .unwrap_or_default(),
        ),
        EntryTarget::Folder {
            folder_id,
            recursive,
        } => {
            // 名前のない folder ユニット（名前順）として扱う
            let def = UnitDef {
                kind: UnitKind::Folder,
                order_mode: OrderMode::Name,
                items: Vec::new(),
                folders: vec![UnitFolder {
                    folder_id,
                    recursive,
                    pos: 0.0,
                }],
            };
            (
                Some(UnitKey::Folder {
                    folder_id,
                    recursive,
                }),
                resolve_members(&def, &snap.members),
            )
        }
    }
}
