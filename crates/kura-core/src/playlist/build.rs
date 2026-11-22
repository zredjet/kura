//! キューを作る（docs/playlists.md §5.2〜§5.5）。DB に触れない純粋関数。
//!
//! 乱数はすべてシードから作る ChaCha8 で、呼ぶ順番も固定する（同じ入力＋同じシード＝同じ結果）。
//! ユニット内シャッフルだけは、ユニットごとに派生させたシードを使う（ユニットの並び順に左右されない）。

use std::collections::{HashMap, HashSet};

use rand_chacha::ChaCha8Rng;

use super::model::{Entry, EntryTarget, Granularity, QueueOptions};
use super::resolve::{ResolvedEntry, Snapshot};
use crate::import::MediaKind;
use crate::queue::{Slot, UnitKey};
use crate::random::{fnv1a64, open_unit, rng, shuffle, splitmix64};

/// 「最近再生した」とみなす期間（重みを 1/4 にする。§5.4）。
const RECENT_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 評価から重みを決める（§5.4 の表）。
pub fn rating_weight(rating: i64) -> f64 {
    match rating {
        1 => 0.6,
        2 => 0.8,
        4 => 1.5,
        5 => 2.0,
        _ => 1.0,
    }
}

/// 重み（評価と最終再生から）。
pub fn weight(rating: i64, last_played_at: Option<i64>, now_ms: i64) -> f64 {
    let w = rating_weight(rating);
    match last_played_at {
        Some(at) if now_ms - at <= RECENT_MS => w * 0.25,
        _ => w,
    }
}

/// 種類の番号（連続制限の比較に使う）。フォント・3D はキューに入らない（`resolve` が除く）が、番号は分けておく。
pub(crate) fn kind_code(kind: MediaKind) -> u8 {
    match kind {
        MediaKind::Image => 0,
        MediaKind::Audio => 1,
        MediaKind::Video => 2,
        MediaKind::Font => 3,
        MediaKind::Model => 4,
    }
}

/// ユニットの代表種類：メンバーで最も多い種類。同数なら video > audio > image（§5.4）。
/// フォント・3D は数えない（再生しないので、連続制限の判定に効かせない）。
pub fn representative_kind(kinds: impl Iterator<Item = MediaKind>) -> u8 {
    // [image, audio, video] の数（kind_code の順）
    let mut counts = [0usize; 3];
    for kind in kinds {
        let slot = match kind {
            MediaKind::Image => &mut counts[0],
            MediaKind::Audio => &mut counts[1],
            MediaKind::Video => &mut counts[2],
            MediaKind::Font | MediaKind::Model => continue,
        };
        *slot += 1;
    }
    let mut best = 2u8;
    for code in [1u8, 0] {
        if counts[code as usize] > counts[best as usize] {
            best = code;
        }
    }
    best
}

/// ユニット内シャッフルのシード（§5.5）：`splitmix64(seed ^ fnv1a64(正規化した鍵))`。
pub fn unit_seed(seed: u64, key: UnitKey) -> u64 {
    let normalized = match key {
        UnitKey::Unit(id) => format!("u:{id}"),
        UnitKey::Folder {
            folder_id,
            recursive,
        } => format!("f:{folder_id}:{}", u8::from(recursive)),
    };
    splitmix64(seed ^ fnv1a64(normalized.as_bytes()))
}

/// 並べ替えの対象 1 つ（ファイル単位ならアイテム、ユニット単位ならグループ）。
pub(crate) struct Element<T> {
    pub value: T,
    pub weight: f64,
    pub kind: u8,
}

/// シャッフルして（`weighted` なら Efraimidis–Spirakis 法）、同じ種類の連続を制限する。
pub(crate) fn order<T>(
    elements: Vec<Element<T>>,
    weighted: bool,
    max_run: u32,
    rng: &mut ChaCha8Rng,
) -> Vec<T> {
    let mut elements = elements;
    if weighted {
        // キー u^(1/w) の降順。比べやすいよう ln(u)/w（単調）で持つ。同じなら元の順
        let mut keyed: Vec<(f64, usize, Element<T>)> = elements
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                let u = open_unit(rng);
                let w = if e.weight > 0.0 {
                    e.weight
                } else {
                    f64::MIN_POSITIVE
                };
                (u.ln() / w, i, e)
            })
            .collect();
        keyed.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        elements = keyed.into_iter().map(|(_, _, e)| e).collect();
    } else {
        shuffle(&mut elements, rng);
    }
    limit_runs(&mut elements, |e| e.kind, max_run as usize);
    elements.into_iter().map(|e| e.value).collect()
}

/// 同じ種類が `k` を超えて続かないようにする（§5.4。k = 0 は制限なし。乱数は使わない）。
///
/// 先頭から走査し、同じ種類が k+1 個続いた位置 i を見つけたら、i より後ろで種類の違う最初の要素 j と入れ替える。
/// 後ろに違う種類がもうなければ、i の要素を前のほうの「入れても k を超えない」いちばん前の位置へ移す
/// （末尾に同じ種類が固まったときのため）。それもなければ、満たせるところまでで終える。
pub fn limit_runs<T>(xs: &mut Vec<T>, kind: impl Fn(&T) -> u8, k: usize) {
    if k == 0 || xs.len() <= k {
        return;
    }
    let mut i = 1;
    let mut run = 1;
    while i < xs.len() {
        run = if kind(&xs[i]) == kind(&xs[i - 1]) {
            run + 1
        } else {
            1
        };
        if run > k {
            let current = kind(&xs[i]);
            if let Some(j) = (i + 1..xs.len()).find(|&j| kind(&xs[j]) != current) {
                xs.swap(i, j);
                run = 1;
            } else if let Some(p) = insertion_point(xs, i, current, &kind, k) {
                let moved = xs.remove(i);
                xs.insert(p, moved);
                // 動かした位置から数え直す
                i = 1;
                run = 1;
                continue;
            } else {
                return;
            }
        }
        i += 1;
    }
}

/// `xs[skip]`（種類 `current`）を入れても前後の同じ種類が k 個以内で収まる、いちばん前の位置。
fn insertion_point<T>(
    xs: &[T],
    skip: usize,
    current: u8,
    kind: &impl Fn(&T) -> u8,
    k: usize,
) -> Option<usize> {
    (0..skip).find(|&p| {
        let left = xs[..p]
            .iter()
            .rev()
            .take_while(|x| kind(x) == current)
            .count();
        let right = xs[p..]
            .iter()
            .enumerate()
            .filter(|(q, _)| p + q != skip)
            .take_while(|(_, x)| kind(x) == current)
            .count();
        left + 1 + right <= k
    })
}

/// 展開したエントリ 1 つ（ユニット内シャッフル済み）。
struct Group<'a> {
    resolved: &'a ResolvedEntry,
    entry: Option<&'a Entry>,
    items: Vec<i64>,
}

/// 展開したエントリからキューを作る。同じ入力・同じシードなら同じ結果。
///
/// - file：全エントリのアイテムをつなぎ、`dedup_items` なら最初の出現だけ残し、shuffle なら全体をシャッフル。
/// - unit：エントリ（グループ）の列をシャッフル。グループの中は順番どおり。
/// - どちらでも、`inner_shuffle` のユニットは中だけシャッフルする（ユニットごとのシード）。
/// - shuffle のときだけ、同じ種類の連続を `max_same_kind_run` までにする。
pub fn build_queue(
    resolved: &[ResolvedEntry],
    snap: &Snapshot,
    options: &QueueOptions,
    seed: u64,
) -> Vec<Slot> {
    let entries: HashMap<i64, &Entry> = snap.entries.iter().map(|e| (e.id, e)).collect();
    let groups: Vec<Group<'_>> = resolved
        .iter()
        .map(|r| {
            let mut items = r.items.clone();
            if let Some(key @ UnitKey::Unit(unit_id)) = r.unit_key
                && snap.units.get(&unit_id).is_some_and(|u| u.inner_shuffle)
            {
                shuffle(&mut items, &mut rng(unit_seed(seed, key)));
            }
            Group {
                resolved: r,
                entry: entries.get(&r.entry_id).copied(),
                items,
            }
        })
        .collect();

    let info = |id: i64| snap.members.items.get(&id);
    let item_kind = |id: i64| info(id).map_or(0, |i| kind_code(i.kind));
    let mut rng = rng(seed);

    // (グループの番号, グループの中の位置) の列
    let order_pairs: Vec<(usize, usize)> = match options.granularity {
        Granularity::File => {
            let mut seen: HashSet<i64> = HashSet::new();
            let pairs: Vec<(usize, usize)> = groups
                .iter()
                .enumerate()
                .flat_map(|(g, group)| (0..group.items.len()).map(move |i| (g, i)))
                .filter(|&(g, i)| !options.dedup_items || seen.insert(groups[g].items[i]))
                .collect();
            if options.shuffle {
                let elements = pairs
                    .into_iter()
                    .map(|(g, i)| {
                        let id = groups[g].items[i];
                        let (rating, last) =
                            info(id).map_or((0, None), |m| (m.rating, m.last_played_at));
                        Element {
                            value: (g, i),
                            weight: weight(rating, last, options.now_ms),
                            kind: item_kind(id),
                        }
                    })
                    .collect();
                order(
                    elements,
                    options.weight_by_rating,
                    options.max_same_kind_run,
                    &mut rng,
                )
            } else {
                pairs
            }
        }
        Granularity::Unit => {
            let indices: Vec<usize> = if options.shuffle {
                let elements = groups
                    .iter()
                    .enumerate()
                    .map(|(g, group)| {
                        let (rating, last) = group_rating(group, snap);
                        Element {
                            value: g,
                            weight: weight(rating, last, options.now_ms),
                            kind: representative_kind(
                                group
                                    .items
                                    .iter()
                                    .filter_map(|id| info(*id))
                                    .map(|m| m.kind),
                            ),
                        }
                    })
                    .collect();
                order(
                    elements,
                    options.weight_by_rating,
                    options.max_same_kind_run,
                    &mut rng,
                )
            } else {
                (0..groups.len()).collect()
            };
            indices
                .into_iter()
                .flat_map(|g| (0..groups[g].items.len()).map(move |i| (g, i)))
                .collect()
        }
    };

    order_pairs
        .into_iter()
        .map(|(g, i)| slot(&groups[g], i, snap, options))
        .collect()
}

/// グループ（ユニット単位のシャッフルの対象）の評価と最終再生。
/// unit エントリはユニットの集計値、folder エントリはメンバーから、item エントリはそのアイテム。
fn group_rating(group: &Group<'_>, snap: &Snapshot) -> (i64, Option<i64>) {
    let members = || {
        group
            .items
            .iter()
            .filter_map(|id| snap.members.items.get(id))
    };
    let member_rating = || {
        let rated: Vec<i64> = members()
            .map(|m| m.rating)
            .filter(|r| (1..=5).contains(r))
            .collect();
        if rated.is_empty() {
            0
        } else {
            (rated.iter().sum::<i64>() as f64 / rated.len() as f64).round() as i64
        }
    };
    let member_last = || members().filter_map(|m| m.last_played_at).max();
    match group.resolved.unit_key {
        Some(UnitKey::Unit(unit_id)) => match snap.units.get(&unit_id) {
            Some(unit) => (
                if unit.rating >= 1 {
                    unit.rating
                } else {
                    member_rating()
                },
                unit.last_played_at,
            ),
            None => (member_rating(), member_last()),
        },
        _ => (member_rating(), member_last()),
    }
}

/// スロット 1 つ。画像の表示時間はエントリ → ユニット → プレイリストの順（§5.3）。
fn slot(group: &Group<'_>, index: usize, snap: &Snapshot, options: &QueueOptions) -> Slot {
    let item_id = group.items[index];
    let is_image = snap
        .members
        .items
        .get(&item_id)
        .is_some_and(|m| m.kind == MediaKind::Image);
    let unit_ms = match group.resolved.unit_key {
        Some(UnitKey::Unit(unit_id)) => snap.units.get(&unit_id).and_then(|u| u.image_duration_ms),
        _ => None,
    };
    let entry_ms = group.entry.and_then(|e| e.image_duration_ms);
    // トリムは item エントリのときだけ意味を持つ
    let trims = group
        .entry
        .filter(|e| matches!(e.target, EntryTarget::Item(_)))
        .map_or((None, None), |e| (e.start_ms, e.end_ms));
    Slot {
        item_id,
        entry_id: Some(group.resolved.entry_id),
        unit_key: group.resolved.unit_key,
        index_in_unit: u32::try_from(index).unwrap_or(u32::MAX),
        unit_len: u32::try_from(group.items.len()).unwrap_or(u32::MAX),
        image_duration_ms: is_image
            .then(|| entry_ms.or(unit_ms).unwrap_or(options.image_duration_ms)),
        start_ms: trims.0,
        end_ms: trims.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_follow_the_table_and_recent_plays() {
        let now = 100 * RECENT_MS;
        assert_eq!(
            (0..=5).map(rating_weight).collect::<Vec<_>>(),
            vec![1.0, 0.6, 0.8, 1.0, 1.5, 2.0]
        );
        assert_eq!(weight(5, Some(now - RECENT_MS), now), 0.5);
        assert_eq!(weight(5, Some(now - RECENT_MS - 1), now), 2.0);
        assert_eq!(weight(4, None, now), 1.5);
    }

    #[test]
    fn representative_kind_prefers_video_then_audio_on_ties() {
        use MediaKind::{Audio, Font, Image, Model, Video};
        assert_eq!(representative_kind([Image, Image, Audio].into_iter()), 0);
        assert_eq!(representative_kind([Image, Audio].into_iter()), 1);
        assert_eq!(representative_kind([Image, Video, Audio].into_iter()), 2);
        assert_eq!(representative_kind(std::iter::empty()), 2);
        // フォント・3D は数えない（多くても代表にならない）
        assert_eq!(
            representative_kind([Font, Font, Model, Image, Audio, Audio].into_iter()),
            1
        );
        assert_eq!(representative_kind([Font, Model, Image].into_iter()), 0);
    }

    #[test]
    fn limit_runs_swaps_forward_and_falls_back_to_earlier_slots() {
        // 前に入れ替えられるとき
        let mut xs = vec![0u8, 0, 0, 1, 1];
        limit_runs(&mut xs, |x| *x, 2);
        assert_eq!(xs, vec![0, 0, 1, 0, 1]);
        // 末尾に固まったとき（後ろに違う種類がない）
        let mut xs = vec![1u8, 1, 2, 2, 1, 2, 0, 0, 0, 0, 0, 0];
        limit_runs(&mut xs, |x| *x, 2);
        for w in xs.windows(3) {
            assert!(!(w[0] == w[1] && w[1] == w[2]), "{xs:?}");
        }
        assert_eq!(xs.len(), 12);
        // 満たせないときは満たせるところまで
        let mut xs = vec![0u8, 0, 0, 0, 1];
        limit_runs(&mut xs, |x| *x, 1);
        assert_eq!(xs.len(), 5);
        // 0 は制限なし
        let mut xs = vec![0u8, 0, 0];
        limit_runs(&mut xs, |x| *x, 0);
        assert_eq!(xs, vec![0, 0, 0]);
    }

    #[test]
    fn unit_seeds_depend_only_on_seed_and_key() {
        let a = unit_seed(7, UnitKey::Unit(3));
        assert_eq!(a, unit_seed(7, UnitKey::Unit(3)));
        assert_ne!(a, unit_seed(8, UnitKey::Unit(3)));
        assert_ne!(a, unit_seed(7, UnitKey::Unit(4)));
        assert_ne!(
            unit_seed(
                7,
                UnitKey::Folder {
                    folder_id: 3,
                    recursive: true
                }
            ),
            unit_seed(
                7,
                UnitKey::Folder {
                    folder_id: 3,
                    recursive: false
                }
            )
        );
    }
}
