//! 描画の動作の列（段 10。`paint.actions` の型付きの引数 `PaintParams::actions`）。追記だけで増える。
//!
//! - 動作は 1 つずつ `Arc` で持ち、64 個ずつの切れ目を `Arc` で持つ。追記した版は前の版の切れ目を共有するので、追記は
//!   O(足した分 ＋ 切れ目の数) で済み、前の動作（点の列）を写さない（docs/editing.md §15「点の多いペン」）。
//! - 前の版への追記かどうか（`appended_since`）は、共有している切れ目・動作を `Arc` の同一性で飛ばし、共有していない
//!   ところだけ中身を比べる。追記で作った版どうしなら O(切れ目の数 ＋ 64)。別々に作った列は全部を比べる（値は同じ）。
//! - JSON では動作の配列（`Vec<PaintAction>` と同じ書き方・読み方・誤りの文言）。

use std::fmt;
use std::sync::Arc;

use serde::de::{SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::PaintAction;

/// 切れ目 1 つの動作の数。
pub const CHUNK: usize = 64;

/// 動作の列。写すのは軽い（切れ目の `Arc` を写すだけ）。最後の切れ目のほかは、どれも `CHUNK` 個ちょうど。
#[derive(Clone, Default)]
pub struct ActionList {
    chunks: Vec<Arc<Vec<Arc<PaintAction>>>>,
    len: usize,
}

impl ActionList {
    /// 空の列。
    pub fn new() -> Self {
        Self::default()
    }

    /// 動作の数。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 動作がなければ true。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `i` 番目の動作。
    pub fn get(&self, i: usize) -> Option<&PaintAction> {
        self.chunks
            .get(i / CHUNK)
            .and_then(|c| c.get(i % CHUNK))
            .map(Arc::as_ref)
    }

    /// 先頭から順に動作を返す。
    pub fn iter(&self) -> impl Iterator<Item = &PaintAction> + '_ {
        self.iter_from(0)
    }

    /// `from` 番目から後ろの動作を返す（追記した分だけを読むとき）。
    pub fn iter_from(&self, from: usize) -> impl Iterator<Item = &PaintAction> + '_ {
        self.chunks
            .iter()
            .skip(from / CHUNK)
            .flat_map(|c| c.iter().map(Arc::as_ref))
            .skip(from % CHUNK)
    }

    /// 動作を写した `Vec`。
    pub fn to_vec(&self) -> Vec<PaintAction> {
        self.iter().cloned().collect()
    }

    /// 末尾に動作を足す（最後の切れ目を共有していれば、その切れ目だけを写す）。
    pub fn push(&mut self, action: PaintAction) {
        let item = Arc::new(action);
        match self.chunks.last_mut() {
            Some(last) if last.len() < CHUNK => Arc::make_mut(last).push(item),
            _ => {
                let mut chunk = Vec::with_capacity(CHUNK);
                chunk.push(item);
                self.chunks.push(Arc::new(chunk));
            }
        }
        self.len += 1;
    }

    /// `self` が `prev` の後ろに動作を足したもの（`prev` と同じものを含む）なら、足した分の始まり（`prev.len()`）。
    /// 共有している切れ目・動作は中身を比べない。
    pub fn appended_since(&self, prev: &ActionList) -> Option<usize> {
        if self.len < prev.len {
            return None;
        }
        // 切れ目の境はどちらも 0 から CHUNK ごとなので、同じ番号の切れ目は同じ範囲の動作を持つ
        for (a, b) in self.chunks.iter().zip(&prev.chunks) {
            if Arc::ptr_eq(a, b) {
                continue;
            }
            if a.len() < b.len() {
                return None;
            }
            let same = a
                .iter()
                .zip(b.iter())
                .all(|(x, y)| Arc::ptr_eq(x, y) || x == y);
            if !same {
                return None;
            }
        }
        Some(prev.len)
    }
}

impl From<Vec<PaintAction>> for ActionList {
    fn from(actions: Vec<PaintAction>) -> Self {
        actions.into_iter().collect()
    }
}

impl FromIterator<PaintAction> for ActionList {
    fn from_iter<I: IntoIterator<Item = PaintAction>>(iter: I) -> Self {
        let mut out = Self::new();
        for a in iter {
            out.push(a);
        }
        out
    }
}

impl PartialEq for ActionList {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.appended_since(other).is_some()
    }
}

impl fmt::Debug for ActionList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl Serialize for ActionList {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.len))?;
        for a in self.iter() {
            seq.serialize_element(a)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for ActionList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ListVisitor;

        impl<'de> Visitor<'de> for ListVisitor {
            type Value = ActionList;

            // `Vec` の読み方と同じ文言（誤りの指摘を変えない）
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ActionList, A::Error> {
                let mut out = ActionList::new();
                while let Some(a) = seq.next_element::<PaintAction>()? {
                    out.push(a);
                }
                Ok(out)
            }
        }

        d.deserialize_seq(ListVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::IPoint;
    use crate::paint::{Connectivity, PaintColor};
    use serde_json::json;

    fn fill(i: i64) -> PaintAction {
        PaintAction::Fill {
            at: IPoint { x: i, y: 0 },
            color: PaintColor::Index(1),
            tolerance: 0,
            connectivity: Connectivity::Four,
            contiguous: true,
            frame: 0,
        }
    }

    /// JSON の書き方・読み方は `Vec<PaintAction>` と同じ（読めないときの文言も）。切れ目は 64 個ずつ。
    #[test]
    fn serde_matches_a_vec() {
        for n in [0usize, 1, 63, 64, 65, 200] {
            let v: Vec<PaintAction> = (0..n as i64).map(fill).collect();
            let list = ActionList::from(v.clone());
            assert_eq!(list.len(), n);
            assert_eq!(list.chunks.len(), n.div_ceil(CHUNK));
            assert_eq!(
                serde_json::to_value(&list).unwrap(),
                serde_json::to_value(&v).unwrap()
            );
            let back: ActionList =
                serde_json::from_value(serde_json::to_value(&v).unwrap()).unwrap();
            assert_eq!(back, list);
            assert_eq!(back.to_vec(), v);
            assert_eq!(list.iter_from(n.min(70)).count(), n - n.min(70));
            assert_eq!(list.get(n.saturating_sub(1)), v.last());
        }
        for bad in [
            json!(5),
            json!("x"),
            json!([{ "type": "smudge" }]),
            json!([json!({ "type": "fill" })]),
        ] {
            let a = serde_json::from_value::<ActionList>(bad.clone()).unwrap_err();
            let b = serde_json::from_value::<Vec<PaintAction>>(bad.clone()).unwrap_err();
            assert_eq!(a.to_string(), b.to_string(), "{bad}");
        }
    }

    /// 追記した版は前の版の満ちた切れ目を共有し、前の版は変わらない。追記かどうかは共有していれば中身を比べずに分かり、
    /// 別々に作った同じ中身の列でも同じ答えになる。
    #[test]
    fn appending_shares_chunks_and_detects_prefixes() {
        let base = ActionList::from((0..130).map(fill).collect::<Vec<_>>());
        let mut more = base.clone();
        more.push(fill(500));
        more.push(fill(501));
        assert_eq!(base.len(), 130);
        assert!(Arc::ptr_eq(&base.chunks[0], &more.chunks[0]));
        assert!(Arc::ptr_eq(&base.chunks[1], &more.chunks[1]));
        assert!(!Arc::ptr_eq(&base.chunks[2], &more.chunks[2]));
        assert!(Arc::ptr_eq(&base.chunks[2][1], &more.chunks[2][1]));
        assert_eq!(more.appended_since(&base), Some(130));
        assert_eq!(base.appended_since(&more), None);
        assert_eq!(more.appended_since(&more), Some(132));
        assert_eq!(more.appended_since(&ActionList::new()), Some(0));
        // 別々に作った列（中身で比べる）
        let fresh = ActionList::from((0..130).map(fill).collect::<Vec<_>>());
        assert_eq!(more.appended_since(&fresh), Some(130));
        let mut other: Vec<PaintAction> = (0..130).map(fill).collect();
        other[70] = fill(-1);
        assert_eq!(more.appended_since(&ActionList::from(other)), None);
        assert_ne!(more, base);
        assert_eq!(more.clone(), more);
    }
}
