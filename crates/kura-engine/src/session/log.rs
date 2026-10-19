//! 追記だけの配列（段 10。`paint.actions` の動作など、`ParamKind::Strokes` の引数）。
//!
//! - 要素は正規の形（`op::canonical_params` に往復させたもの）で、1 つずつ `Arc` で持つ。
//! - 64 個ずつの切れ目を `Arc` で持ち、追記した版どうしで前の切れ目を共有する（1 手で写すのは最後の切れ目と切れ目の列だけ）。
//! - 連鎖のダイジェスト（`op::append_chain`）を足した分だけ求め直す。追記で作っても、全体から作っても同じ値。

use std::fmt;
use std::sync::Arc;

use serde_json::Value;

use crate::digest::Digest;
use crate::error::EngineError;
use crate::op;

/// 切れ目 1 つの要素の数。
pub const CHUNK: usize = 64;

/// 要素 1 つ（正規の形の値と、その正規の JSON の長さ）。
#[derive(Clone)]
struct Item {
    value: Arc<Value>,
    len: usize,
}

/// 切れ目 1 つ（`CHUNK` 個まで）。
struct Chunk {
    items: Vec<Item>,
}

/// 追記だけの配列。中身を変えずに写すのは軽い（切れ目の `Arc` を写すだけ）。
#[derive(Clone)]
pub struct AppendLog {
    chunks: Vec<Arc<Chunk>>,
    count: usize,
    chain: Digest,
    items_len: usize,
}

impl fmt::Debug for AppendLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppendLog")
            .field("len", &self.count)
            .field("chain", &self.chain)
            .field("items_len", &self.items_len)
            .finish()
    }
}

impl Default for AppendLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AppendLog {
    /// 空の配列。
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            count: 0,
            chain: op::append_chain_start(),
            items_len: 0,
        }
    }

    /// 要素の数。
    pub fn len(&self) -> usize {
        self.count
    }

    /// 要素がなければ true。
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 連鎖のダイジェスト（`op::append_chain` を先頭から順に当てたもの）。
    pub fn chain(&self) -> Digest {
        self.chain
    }

    /// 要素の正規の JSON の長さの和。
    pub fn items_len(&self) -> usize {
        self.items_len
    }

    /// 配列としての正規の JSON の長さ（`[` と `]`、要素のあいだの `,` を含む）。
    pub fn encoded_len(&self) -> usize {
        self.items_len
            .saturating_add(2)
            .saturating_add(self.count.saturating_sub(1))
    }

    /// 切れ目の数。
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// `i` 番目の要素。
    pub fn get(&self, i: usize) -> Option<&Value> {
        let chunk = self.chunks.get(i / CHUNK)?;
        chunk.items.get(i % CHUNK).map(|it| it.value.as_ref())
    }

    /// 先頭から順に要素を返す。
    pub fn iter(&self) -> impl Iterator<Item = &Value> + '_ {
        self.chunks
            .iter()
            .flat_map(|c| c.items.iter().map(|it| it.value.as_ref()))
    }

    /// `from` 番目から後ろの要素を返す（追記した分だけを読むとき）。
    pub fn iter_from(&self, from: usize) -> impl Iterator<Item = &Value> + '_ {
        let start = from / CHUNK;
        let skip = from % CHUNK;
        self.chunks
            .iter()
            .skip(start)
            .flat_map(|c| c.items.iter().map(|it| it.value.as_ref()))
            .skip(skip)
    }

    /// 要素を写した配列（保存の形にするとき）。
    pub fn to_vec(&self) -> Vec<Value> {
        self.iter().cloned().collect()
    }

    /// 要素を足した新しい配列（`self` は変えない。前の切れ目は共有する）。`items` は正規の形の値と、その正規の JSON の
    /// 長さの組（呼び手が確かめたもの）。
    pub(crate) fn extend(
        &self,
        items: impl IntoIterator<Item = (Value, usize)>,
    ) -> Result<Self, EngineError> {
        let mut out = self.clone();
        let mut tail: Option<Vec<Item>> = None;
        for (value, len) in items {
            out.chain = op::append_chain(out.chain, &value)?;
            out.items_len = out.items_len.saturating_add(len);
            out.count = out.count.saturating_add(1);
            let item = Item {
                value: Arc::new(value),
                len,
            };
            // 最後の切れ目に空きがあれば、その写しに足す（共有している切れ目は書き換えない）
            let open = match tail.as_mut() {
                Some(t) => t,
                None => {
                    let last_has_room = out.chunks.last().is_some_and(|c| c.items.len() < CHUNK);
                    let start = if last_has_room {
                        out.chunks
                            .pop()
                            .map(|c| {
                                let mut v = Vec::with_capacity(CHUNK);
                                v.extend(c.items.iter().cloned());
                                v
                            })
                            .unwrap_or_default()
                    } else {
                        Vec::with_capacity(CHUNK)
                    };
                    tail.insert(start)
                }
            };
            open.push(item);
            if open.len() == CHUNK
                && let Some(full) = tail.take()
            {
                out.chunks.push(Arc::new(Chunk { items: full }));
            }
        }
        if let Some(rest) = tail {
            out.chunks.push(Arc::new(Chunk { items: rest }));
        }
        Ok(out)
    }

    /// `older` と共有している先頭の要素の数（切れ目と要素の `Arc` の同一性で見る。中身が同じでも共有していなければ数えない）。
    pub fn shared_prefix(&self, older: &AppendLog) -> usize {
        let mut n = 0usize;
        for (a, b) in self.chunks.iter().zip(&older.chunks) {
            if Arc::ptr_eq(a, b) {
                n = n.saturating_add(a.items.len());
                continue;
            }
            n = n.saturating_add(
                a.items
                    .iter()
                    .zip(&b.items)
                    .take_while(|(x, y)| Arc::ptr_eq(&x.value, &y.value))
                    .count(),
            );
            break;
        }
        n
    }

    /// `older` と共有していない分の見積もり（バイト）：共有していない要素の正規の JSON の長さと、写した切れ目・切れ目の列の
    /// 番地の分（要素 1 つ 16 バイト、切れ目 1 つ 8 バイト）。履歴の上限に使う（アロケータの値ではなく決定的な見積もり）。
    pub(crate) fn unshared_bytes(&self, older: &AppendLog) -> u64 {
        let mut bytes = (self.chunks.len() as u64).saturating_mul(8);
        for (i, chunk) in self.chunks.iter().enumerate() {
            let old = older.chunks.get(i);
            if old.is_some_and(|o| Arc::ptr_eq(o, chunk)) {
                continue;
            }
            bytes = bytes.saturating_add((chunk.items.len() as u64).saturating_mul(16));
            for (k, item) in chunk.items.iter().enumerate() {
                let shared = old
                    .and_then(|o| o.items.get(k))
                    .is_some_and(|o| Arc::ptr_eq(&o.value, &item.value));
                if !shared {
                    bytes = bytes.saturating_add(item.len as u64);
                }
            }
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(i: usize) -> (Value, usize) {
        let v = json!({ "n": i, "s": "x".repeat(i % 7) });
        let len = crate::digest::canonical_json(&v).unwrap().len();
        (v, len)
    }

    /// 1 つずつ足しても、まとめて足しても、同じ要素・同じ連鎖・同じ長さ。切れ目は 64 個ずつ。
    #[test]
    fn extend_one_by_one_equals_at_once() {
        let n = 3 * CHUNK + 5;
        let mut step = AppendLog::new();
        for i in 0..n {
            step = step.extend([item(i)]).unwrap();
        }
        let once = AppendLog::new().extend((0..n).map(item)).unwrap();
        assert_eq!(step.len(), n);
        assert_eq!(step.chain(), once.chain());
        assert_eq!(step.items_len(), once.items_len());
        assert_eq!(step.to_vec(), once.to_vec());
        assert_eq!(step.chunk_count(), 4);
        assert!(step.chunks.iter().take(3).all(|c| c.items.len() == CHUNK));
        let all: Vec<Value> = (0..n).map(|i| item(i).0).collect();
        assert_eq!(step.to_vec(), all);
        assert_eq!(step.get(CHUNK + 3), Some(&all[CHUNK + 3]));
        assert_eq!(step.get(n), None);
        assert_eq!(
            step.iter_from(CHUNK + 3).cloned().collect::<Vec<_>>(),
            all[CHUNK + 3..].to_vec()
        );
        assert_eq!(step.iter_from(n).count(), 0);
        let text = crate::digest::canonical_json(&Value::Array(all)).unwrap();
        assert_eq!(step.encoded_len(), text.len());
        assert_eq!(AppendLog::new().encoded_len(), 2);
        // 連鎖は op::params_digest の追記の配列の連鎖と同じ式
        let mut chain = op::append_chain_start();
        for i in 0..n {
            chain = op::append_chain(chain, &item(i).0).unwrap();
        }
        assert_eq!(step.chain(), chain);
    }

    /// 足しても前の版は変わらず、満ちた切れ目は共有する。共有している先頭の数と、共有していない分の見積もり。
    #[test]
    fn appending_shares_full_chunks() {
        let a = AppendLog::new().extend((0..CHUNK + 10).map(item)).unwrap();
        let b = a.extend([item(1000)]).unwrap();
        assert_eq!(a.len(), CHUNK + 10);
        assert!(Arc::ptr_eq(&a.chunks[0], &b.chunks[0]));
        assert!(!Arc::ptr_eq(&a.chunks[1], &b.chunks[1]));
        assert_eq!(b.shared_prefix(&a), a.len());
        assert_eq!(a.shared_prefix(&b), a.len());
        let (_, len) = item(1000);
        // 写した最後の切れ目（11 要素）の番地と、切れ目の列の番地と、新しい要素の長さ
        assert_eq!(b.unshared_bytes(&a), 2 * 8 + 11 * 16 + len as u64);
        assert_eq!(a.unshared_bytes(&a), 2 * 8);
        // 中身が同じでも、別に作った配列は共有しない
        let c = AppendLog::new().extend((0..CHUNK + 10).map(item)).unwrap();
        assert_eq!(c.chain(), a.chain());
        assert_eq!(c.shared_prefix(&a), 0);
        assert_eq!(
            c.unshared_bytes(&a) as usize,
            2 * 8 + (CHUNK + 10) * 16 + c.items_len()
        );
    }
}
