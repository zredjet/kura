//! ユニットのメンバー解決と集計値（docs/playlists.md §2.2・§2.4）。DB に触れない純粋関数。

use std::collections::{HashMap, HashSet};

use crate::text::natural_cmp;

/// ユニットの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitKind {
    /// ファイル群（選んだアイテムの順序付きリスト）。
    Set,
    /// フォルダ群（参照するフォルダの中身に追従する）。
    Folder,
}

impl UnitKind {
    /// DB に保存する名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Folder => "folder",
        }
    }

    /// DB の名前から読む。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "set" => Some(Self::Set),
            "folder" => Some(Self::Folder),
            _ => None,
        }
    }
}

/// メンバーの並び順。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderMode {
    /// 手動（set だけ。`unit_items.pos` の順）。
    Manual,
    /// 名前の自然順（「2」<「10」）。
    Name,
    /// 追加日の古い順。
    Imported,
    /// 撮影日の古い順（不明は後ろ）。
    Captured,
    /// 長さの短い順（不明は後ろ）。
    Duration,
}

impl OrderMode {
    /// DB に保存する名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Name => "name",
            Self::Imported => "imported",
            Self::Captured => "captured",
            Self::Duration => "duration",
        }
    }

    /// DB の名前から読む。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(Self::Manual),
            "name" => Some(Self::Name),
            "imported" => Some(Self::Imported),
            "captured" => Some(Self::Captured),
            "duration" => Some(Self::Duration),
            _ => None,
        }
    }
}

/// folder ユニットが参照するフォルダ。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitFolder {
    pub folder_id: i64,
    /// 子孫フォルダも含めるか。
    pub recursive: bool,
    pub pos: f64,
}

/// メンバー解決に必要なユニットの定義。
#[derive(Debug, Clone, PartialEq)]
pub struct UnitDef {
    pub kind: UnitKind,
    pub order_mode: OrderMode,
    /// set のメンバー（アイテム ID と pos）。
    pub items: Vec<(i64, f64)>,
    /// folder の参照フォルダ。
    pub folders: Vec<UnitFolder>,
}

/// メンバー候補のアイテムの情報。
#[derive(Debug, Clone, PartialEq)]
pub struct MemberInfo {
    pub title: String,
    pub kind: String,
    pub imported_at: i64,
    pub captured_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub rating: i64,
    /// 最終再生（シャッフルの重み付けに使う。docs/playlists.md §5.4）。
    pub last_played_at: Option<i64>,
    pub trashed: bool,
}

/// DB から読んだ、解決に必要なデータ。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemberSnapshot {
    pub items: HashMap<i64, MemberInfo>,
    /// フォルダ → 子フォルダ。
    pub children: HashMap<i64, Vec<i64>>,
    /// フォルダ → 直接入っているアイテム。
    pub folder_items: HashMap<i64, Vec<i64>>,
}

impl MemberSnapshot {
    fn alive(&self, id: i64) -> bool {
        self.items.get(&id).is_some_and(|i| !i.trashed)
    }

    /// そのフォルダ（と `recursive` なら子孫フォルダ）に入っているアイテム（重複なし、順不同）。
    fn items_under(&self, folder: i64, recursive: bool) -> Vec<i64> {
        let mut folders = vec![folder];
        if recursive {
            let mut visited: HashSet<i64> = HashSet::from([folder]);
            let mut i = 0;
            while let Some(&f) = folders.get(i) {
                for child in self.children.get(&f).into_iter().flatten() {
                    if visited.insert(*child) {
                        folders.push(*child);
                    }
                }
                i += 1;
            }
        }
        let mut seen = HashSet::new();
        folders
            .iter()
            .flat_map(|f| self.folder_items.get(f).into_iter().flatten())
            .copied()
            .filter(|id| seen.insert(*id))
            .collect()
    }
}

/// ユニットのメンバーを並び順で返す（§2.2）。ゴミ箱と存在しないアイテムは除く。
///
/// - set：`manual` なら pos の順、それ以外は `order_mode` の順。
/// - folder：参照フォルダを pos の順に、フォルダごとに集めて `order_mode` で並べてからつなげる（重複は最初の 1 回だけ）。
pub fn resolve_members(unit: &UnitDef, snap: &MemberSnapshot) -> Vec<i64> {
    match unit.kind {
        UnitKind::Set => {
            let mut members: Vec<(i64, f64)> = unit
                .items
                .iter()
                .copied()
                .filter(|(id, _)| snap.alive(*id))
                .collect();
            if unit.order_mode == OrderMode::Manual {
                members.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                members.into_iter().map(|(id, _)| id).collect()
            } else {
                let mut ids: Vec<i64> = members.into_iter().map(|(id, _)| id).collect();
                sort_by_mode(&mut ids, unit.order_mode, snap);
                ids
            }
        }
        UnitKind::Folder => {
            let mut refs = unit.folders.clone();
            refs.sort_by(|a, b| {
                a.pos
                    .total_cmp(&b.pos)
                    .then_with(|| a.folder_id.cmp(&b.folder_id))
            });
            let mut seen: HashSet<i64> = HashSet::new();
            let mut out = Vec::new();
            for r in refs {
                let mut ids: Vec<i64> = snap
                    .items_under(r.folder_id, r.recursive)
                    .into_iter()
                    .filter(|id| snap.alive(*id))
                    .collect();
                // folder ユニットに手動の並びはないので、名前順として扱う
                let mode = match unit.order_mode {
                    OrderMode::Manual => OrderMode::Name,
                    mode => mode,
                };
                sort_by_mode(&mut ids, mode, snap);
                out.extend(ids.into_iter().filter(|id| seen.insert(*id)));
            }
            out
        }
    }
}

fn sort_by_mode(ids: &mut [i64], mode: OrderMode, snap: &MemberSnapshot) {
    let info = |id: &i64| snap.items.get(id);
    ids.sort_by(|a, b| {
        let (Some(x), Some(y)) = (info(a), info(b)) else {
            return a.cmp(b);
        };
        let primary = match mode {
            OrderMode::Manual | OrderMode::Name => natural_cmp(&x.title, &y.title),
            OrderMode::Imported => x.imported_at.cmp(&y.imported_at),
            OrderMode::Captured => cmp_nulls_last(x.captured_at, y.captured_at),
            OrderMode::Duration => cmp_nulls_last(x.duration_ms, y.duration_ms),
        };
        primary.then_with(|| a.cmp(b))
    });
}

fn cmp_nulls_last(a: Option<i64>, b: Option<i64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// 集計値（§2.4。最終再生・追加日は units の列をそのまま使う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberStats {
    pub count: usize,
    /// 音楽・動画は長さ、画像は表示時間の合計（ミリ秒）。
    pub duration_ms: i64,
    /// メンバーの種類（image → video → audio の順）。
    pub kinds: Vec<&'static str>,
    /// ユニットの評価が 1 以上ならそれ。0 なら評価済みメンバーの平均（四捨五入）。なければ 0。
    pub rating: i64,
}

/// メンバーの集計値。`image_duration_ms` は画像 1 枚の表示時間（ユニットの設定か既定値）。
pub fn aggregate(
    members: &[i64],
    snap: &MemberSnapshot,
    unit_rating: i64,
    image_duration_ms: i64,
) -> MemberStats {
    let mut duration_ms = 0i64;
    let mut has = [false; 3];
    let (mut rated, mut rating_sum) = (0i64, 0i64);
    for info in members.iter().filter_map(|id| snap.items.get(id)) {
        match info.kind.as_str() {
            "image" => {
                has[0] = true;
                duration_ms += image_duration_ms;
            }
            "video" => {
                has[1] = true;
                duration_ms += info.duration_ms.unwrap_or(0);
            }
            _ => {
                has[2] = true;
                duration_ms += info.duration_ms.unwrap_or(0);
            }
        }
        if (1..=5).contains(&info.rating) {
            rated += 1;
            rating_sum += info.rating;
        }
    }
    let kinds = ["image", "video", "audio"]
        .into_iter()
        .zip(has)
        .filter_map(|(k, h)| h.then_some(k))
        .collect();
    let rating = if unit_rating >= 1 {
        unit_rating
    } else if rated > 0 {
        (rating_sum as f64 / rated as f64).round() as i64
    } else {
        0
    };
    MemberStats {
        count: members.len(),
        duration_ms,
        kinds,
        rating,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(title: &str, kind: &str, imported: i64) -> MemberInfo {
        MemberInfo {
            title: title.into(),
            kind: kind.into(),
            imported_at: imported,
            captured_at: None,
            duration_ms: None,
            rating: 0,
            last_played_at: None,
            trashed: false,
        }
    }

    fn snap(items: &[(i64, MemberInfo)]) -> MemberSnapshot {
        MemberSnapshot {
            items: items.iter().cloned().collect(),
            ..MemberSnapshot::default()
        }
    }

    fn set(order_mode: OrderMode, items: &[(i64, f64)]) -> UnitDef {
        UnitDef {
            kind: UnitKind::Set,
            order_mode,
            items: items.to_vec(),
            folders: Vec::new(),
        }
    }

    #[test]
    fn set_manual_follows_pos_and_skips_trash_and_missing() {
        let mut s = snap(&[
            (1, info("c", "image", 3)),
            (2, info("a", "image", 1)),
            (3, info("b", "image", 2)),
        ]);
        s.items.get_mut(&3).unwrap().trashed = true;
        let unit = set(
            OrderMode::Manual,
            &[(1, 2.0), (2, 1.0), (3, 0.5), (99, 0.1)],
        );
        assert_eq!(resolve_members(&unit, &s), vec![2, 1]);
    }

    #[test]
    fn set_orders_by_name_naturally_and_by_other_modes() {
        let mut s = snap(&[
            (1, info("p10", "image", 1)),
            (2, info("p2", "image", 3)),
            (3, info("p1", "video", 2)),
        ]);
        s.items.get_mut(&1).unwrap().captured_at = Some(50);
        s.items.get_mut(&2).unwrap().captured_at = Some(10);
        s.items.get_mut(&3).unwrap().duration_ms = Some(5);
        let items = [(1, 1.0), (2, 2.0), (3, 3.0)];
        let order = |mode| resolve_members(&set(mode, &items), &s);
        assert_eq!(order(OrderMode::Name), vec![3, 2, 1]);
        assert_eq!(order(OrderMode::Imported), vec![1, 3, 2]);
        // 不明は後ろ
        assert_eq!(order(OrderMode::Captured), vec![2, 1, 3]);
        assert_eq!(order(OrderMode::Duration), vec![3, 1, 2]);
    }

    /// 写真(10) ─ 旅行(11) ─ 鎌倉(12)、別に 音楽(20)
    fn folder_snapshot() -> MemberSnapshot {
        let mut s = snap(&[
            (1, info("photo-2", "image", 1)),
            (2, info("photo-10", "image", 2)),
            (3, info("trip-1", "image", 3)),
            (4, info("kamakura", "image", 4)),
            (5, info("song", "audio", 5)),
            (6, info("old", "image", 6)),
        ]);
        s.items.get_mut(&6).unwrap().trashed = true;
        s.children = HashMap::from([(10, vec![11]), (11, vec![12])]);
        s.folder_items = HashMap::from([
            (10, vec![2, 1, 6]),
            (11, vec![3, 1]),
            (12, vec![4]),
            (20, vec![5, 4]),
        ]);
        s
    }

    fn folder_unit(order_mode: OrderMode, refs: &[(i64, bool, f64)]) -> UnitDef {
        UnitDef {
            kind: UnitKind::Folder,
            order_mode,
            items: Vec::new(),
            folders: refs
                .iter()
                .map(|&(folder_id, recursive, pos)| UnitFolder {
                    folder_id,
                    recursive,
                    pos,
                })
                .collect(),
        }
    }

    #[test]
    fn folder_recursive_includes_descendants_without_duplicates() {
        let s = folder_snapshot();
        let unit = folder_unit(OrderMode::Name, &[(10, true, 1.0)]);
        // 1 は 写真 と 旅行 の両方にあるが 1 回だけ。6 はゴミ箱
        assert_eq!(resolve_members(&unit, &s), vec![4, 1, 2, 3]);
    }

    #[test]
    fn folder_non_recursive_excludes_children() {
        let s = folder_snapshot();
        let unit = folder_unit(OrderMode::Name, &[(10, false, 1.0)]);
        assert_eq!(resolve_members(&unit, &s), vec![1, 2]);
    }

    #[test]
    fn multiple_folders_are_concatenated_in_pos_order() {
        let s = folder_snapshot();
        // 音楽（pos 1）→ 鎌倉（pos 2）。4 は両方にあるので音楽の方で 1 回だけ
        let unit = folder_unit(OrderMode::Imported, &[(12, true, 2.0), (20, false, 1.0)]);
        assert_eq!(resolve_members(&unit, &s), vec![4, 5]);
        // フォルダごとに並べてからつなぐ（全体をまとめて並べ直さない）
        let unit = folder_unit(OrderMode::Name, &[(20, false, 1.0), (11, false, 2.0)]);
        assert_eq!(resolve_members(&unit, &s), vec![4, 5, 1, 3]);
        let unit = folder_unit(OrderMode::Name, &[(20, false, 2.0), (11, false, 1.0)]);
        assert_eq!(resolve_members(&unit, &s), vec![1, 3, 4, 5]);
    }

    #[test]
    fn folder_reflects_current_contents_and_survives_cycles() {
        let mut s = folder_snapshot();
        let unit = folder_unit(OrderMode::Name, &[(12, true, 1.0)]);
        assert_eq!(resolve_members(&unit, &s), vec![4]);
        s.items.insert(7, info("added", "image", 7));
        s.folder_items.entry(12).or_default().push(7);
        assert_eq!(resolve_members(&unit, &s), vec![7, 4]);
        // 壊れた循環があっても止まる
        s.children.insert(12, vec![10]);
        assert_eq!(resolve_members(&unit, &s).len(), 5);
    }

    #[test]
    fn one_item_can_belong_to_many_units() {
        let s = folder_snapshot();
        let a = set(OrderMode::Manual, &[(1, 1.0), (2, 2.0)]);
        let b = set(OrderMode::Manual, &[(2, 1.0), (5, 2.0)]);
        let c = folder_unit(OrderMode::Name, &[(10, false, 1.0)]);
        for unit in [&a, &b, &c] {
            assert!(resolve_members(unit, &s).contains(&2));
        }
    }

    #[test]
    fn aggregates_duration_kinds_and_rating() {
        let mut s = snap(&[
            (1, info("a", "image", 1)),
            (2, info("b", "video", 1)),
            (3, info("c", "audio", 1)),
        ]);
        s.items.get_mut(&2).unwrap().duration_ms = Some(60_000);
        s.items.get_mut(&3).unwrap().duration_ms = Some(30_000);
        s.items.get_mut(&1).unwrap().rating = 4;
        s.items.get_mut(&2).unwrap().rating = 5;
        let stats = aggregate(&[1, 2, 3], &s, 0, 8000);
        assert_eq!(stats.count, 3);
        assert_eq!(stats.duration_ms, 98_000);
        assert_eq!(stats.kinds, vec!["image", "video", "audio"]);
        assert_eq!(stats.rating, 5); // (4 + 5) / 2 = 4.5 → 5
        assert_eq!(aggregate(&[1, 2, 3], &s, 2, 8000).rating, 2);
        assert_eq!(aggregate(&[3], &s, 0, 8000).rating, 0);
        assert_eq!(aggregate(&[], &s, 0, 8000).kinds, Vec::<&str>::new());
    }
}
