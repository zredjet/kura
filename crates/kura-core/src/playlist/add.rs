//! プレイリストへの追加（docs/playlists.md §3.5）。どれを展開し、どれをそのまま入れるかは `plan_add`（純粋関数）で決める。

use rusqlite::Connection;

use super::model::{EntryTarget, Granularity};
use super::resolve::expand;
use super::store::{self, NewEntry};
use crate::Result;
use crate::unit;

/// 追加するもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddSource {
    Item(i64),
    Unit(i64),
    /// 仮想フォルダ（子孫フォルダも含む）。
    Folder(i64),
}

/// 追加のしかた。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddOptions {
    /// ⌥：展開する／しないを逆にする。
    pub invert: bool,
    /// ユニット単位に複数のアイテムを入れるとき、この名前の set ユニットにまとめて入れる（None は 1 件ずつ）。
    pub group_name: Option<String>,
}

/// 追加の 1 手。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddStep {
    Entry(EntryTarget),
    /// ユニットのその時点のメンバーを item エントリにする。
    ExpandUnit(i64),
    /// フォルダの中身（子孫も）を item エントリにする。
    ExpandFolder(i64),
    /// アイテムを 1 つの set ユニットにまとめて unit エントリにする。
    Group(Vec<i64>),
}

/// §3.5 の表：
///
/// | 追加するもの | ファイル単位 | ユニット単位 |
/// |---|---|---|
/// | アイテム | item エントリ | item エントリ（複数で `group_name` があれば 1 つのユニットにまとめる） |
/// | ユニット | 展開して item エントリ | unit エントリ |
/// | フォルダ | 展開して item エントリ | folder エントリ（動的） |
///
/// ⌥（`invert`）ならユニット・フォルダの「展開する／しない」を逆にする。
pub fn plan_add(
    granularity: Granularity,
    sources: &[AddSource],
    options: &AddOptions,
) -> Vec<AddStep> {
    let expand = (granularity == Granularity::File) != options.invert;
    let items: Vec<i64> = sources
        .iter()
        .filter_map(|s| match s {
            AddSource::Item(id) => Some(*id),
            _ => None,
        })
        .collect();
    let group = granularity == Granularity::Unit && items.len() > 1 && options.group_name.is_some();
    let mut steps = Vec::new();
    let mut grouped = false;
    for source in sources {
        match *source {
            AddSource::Item(_) if group => {
                // まとめたユニットは、最初のアイテムの位置に入れる
                if !grouped {
                    steps.push(AddStep::Group(items.clone()));
                    grouped = true;
                }
            }
            AddSource::Item(id) => steps.push(AddStep::Entry(EntryTarget::Item(id))),
            AddSource::Unit(id) if expand => steps.push(AddStep::ExpandUnit(id)),
            AddSource::Unit(id) => steps.push(AddStep::Entry(EntryTarget::Unit(id))),
            AddSource::Folder(id) if expand => steps.push(AddStep::ExpandFolder(id)),
            AddSource::Folder(id) => steps.push(AddStep::Entry(EntryTarget::Folder {
                folder_id: id,
                recursive: true,
            })),
        }
    }
    steps
}

/// 追加した結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    pub entry_ids: Vec<i64>,
    /// まとめるために作ったユニット。
    pub unit_id: Option<i64>,
}

/// プレイリストに追加する（`before` のエントリの前、None なら末尾）。
pub fn add(
    conn: &Connection,
    id: i64,
    sources: &[AddSource],
    options: &AddOptions,
    before: Option<i64>,
    now: i64,
) -> Result<Added> {
    let playlist = store::get(conn, id)?;
    let mut targets: Vec<NewEntry> = Vec::new();
    let mut unit_id = None;
    for step in plan_add(playlist.granularity, sources, options) {
        match step {
            AddStep::Entry(target) => targets.push(target.into()),
            AddStep::ExpandUnit(unit) => targets.extend(
                unit::members(conn, unit)?
                    .into_iter()
                    .map(|item| NewEntry::from(EntryTarget::Item(item))),
            ),
            AddStep::ExpandFolder(folder) => {
                let target = EntryTarget::Folder {
                    folder_id: folder,
                    recursive: true,
                };
                let snap =
                    store::snapshot_of(conn, vec![super::model::Entry::new(0, 0.0, target)])?;
                let (_, items) = expand(&snap, target);
                targets.extend(
                    items
                        .into_iter()
                        .map(|item| NewEntry::from(EntryTarget::Item(item))),
                );
            }
            AddStep::Group(items) => {
                let name = options.group_name.as_deref().unwrap_or_default();
                let created = unit::create_set(conn, name, &items, now)?;
                unit_id = Some(created);
                targets.push(EntryTarget::Unit(created).into());
            }
        }
    }
    let entry_ids = store::add_entries(conn, id, &targets, before, now)?;
    Ok(Added { entry_ids, unit_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(invert: bool, group: Option<&str>) -> AddOptions {
        AddOptions {
            invert,
            group_name: group.map(str::to_string),
        }
    }

    #[test]
    fn follows_the_table_of_section_3_5() {
        let all = [AddSource::Item(1), AddSource::Unit(7), AddSource::Folder(9)];
        let folder = EntryTarget::Folder {
            folder_id: 9,
            recursive: true,
        };
        assert_eq!(
            plan_add(Granularity::File, &all, &opts(false, None)),
            vec![
                AddStep::Entry(EntryTarget::Item(1)),
                AddStep::ExpandUnit(7),
                AddStep::ExpandFolder(9)
            ]
        );
        assert_eq!(
            plan_add(Granularity::Unit, &all, &opts(false, None)),
            vec![
                AddStep::Entry(EntryTarget::Item(1)),
                AddStep::Entry(EntryTarget::Unit(7)),
                AddStep::Entry(folder)
            ]
        );
        // ⌥ で逆にする
        assert_eq!(
            plan_add(Granularity::File, &all, &opts(true, None)),
            vec![
                AddStep::Entry(EntryTarget::Item(1)),
                AddStep::Entry(EntryTarget::Unit(7)),
                AddStep::Entry(folder)
            ]
        );
        assert_eq!(
            plan_add(Granularity::Unit, &all, &opts(true, None)),
            vec![
                AddStep::Entry(EntryTarget::Item(1)),
                AddStep::ExpandUnit(7),
                AddStep::ExpandFolder(9)
            ]
        );
    }

    #[test]
    fn groups_several_items_only_by_unit_and_when_named() {
        let items = [AddSource::Item(1), AddSource::Unit(7), AddSource::Item(2)];
        assert_eq!(
            plan_add(Granularity::Unit, &items, &opts(false, Some("鎌倉"))),
            vec![
                AddStep::Group(vec![1, 2]),
                AddStep::Entry(EntryTarget::Unit(7))
            ]
        );
        // 1 件だけならまとめない
        assert_eq!(
            plan_add(
                Granularity::Unit,
                &[AddSource::Item(1)],
                &opts(false, Some("x"))
            ),
            vec![AddStep::Entry(EntryTarget::Item(1))]
        );
        // ファイル単位ではまとめない
        assert_eq!(
            plan_add(
                Granularity::File,
                &[AddSource::Item(1), AddSource::Item(2)],
                &opts(false, Some("x"))
            ),
            vec![
                AddStep::Entry(EntryTarget::Item(1)),
                AddStep::Entry(EntryTarget::Item(2))
            ]
        );
    }
}
