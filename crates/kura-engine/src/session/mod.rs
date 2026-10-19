//! エディタの履歴（段 10。docs/editing.md §12 のセッション、ADR-141）。
//!
//! - 純粋な構造：OS・時刻・スレッドを使わず、描画・計画・キャッシュも持たない。状態を変える関数は `&Engine` を受け取り、
//!   登録表（操作の版と、追記だけの引数）と上限だけを読む。
//! - 履歴は文書のスナップショット（`Snapshot`）の列。逆操作は持たない。ノードは手のあいだで共有する。
//! - まとめ方に時刻を使わない：`CommitMode`（Step・Merge）と `seal`、履歴に入らない仮の値（`preview`）で表す。
//!   ドラッグは preview を繰り返して離したときに Step、キーボードの矢印・数値の入力は Merge、blur・Enter で seal。
//! - 番号（`Rev`）は新しい中身の状態ごとに振り、undo・redo は戻った先の手の番号を返す（同じ手には同じ番号。同じ中身でも、
//!   新しく積んだ手・出し直した仮の値には新しい番号を振る。中身の違う状態に同じ番号は振らない）。
//! - 履歴は 1,000 手と、共有を除いた文書の 64 MiB の早いほうで、古い手から捨てる。
//! - 下書きの形式は作らない（M12）。頭の文書と土台の版から作り直す口（`open`・`open_json`）だけを持つ。

mod edit;
pub mod log;
pub mod snapshot;

use std::collections::VecDeque;

use crate::digest::Digest;
use crate::doc::{Expand, FormatChoice, Graph, NodeId, OutputId, PortRef, Quantize};
use crate::error::EngineError;
use crate::render::Engine;

pub use log::AppendLog;
pub use snapshot::{NodeChange, NodeView, ParamsView, Snapshot, SnapshotDiff};

/// 今の文書（確定した手か仮の値）の番号。単調に増やし、中身の違う状態に同じ番号を振らない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rev(pub u64);

/// 確定のしかた。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMode {
    /// 1 手にする。
    Step,
    /// 同じ引数を続けて変える手（キーボードの矢印・数値の入力）を 1 手にまとめる。まとめられない手は `Step` と同じ。
    Merge,
}

/// 文書への変更 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// 線形の列の `at` 番目（0 が先頭、ノードの数なら末尾）にノードを足す。ID は `next_id` から払い出し、種は `seed`
    /// （なければ ID の値）。`extra_inputs` は 2 つ目からの入力（文書の入力か、`at` より前のノード）。
    AddNode {
        at: usize,
        op: String,
        v: u16,
        params: serde_json::Value,
        seed: Option<u64>,
        extra_inputs: Vec<PortRef>,
    },
    /// 線形の列からノードを消し、前後をつなぎ直す。
    RemoveNode { node: NodeId },
    /// 線形の列の中でノードを `to` 番目へ動かし、つなぎ直す。
    MoveNode { node: NodeId, to: usize },
    /// 有効か無効かを替える。
    SetEnabled { node: NodeId, enabled: bool },
    /// 引数をまとめて替える。
    SetParams {
        node: NodeId,
        params: serde_json::Value,
    },
    /// 引数の項目 1 つを替える。
    SetParam {
        node: NodeId,
        key: String,
        value: serde_json::Value,
    },
    /// 追記だけの配列の引数（`field`）に要素を足す。
    AppendItems {
        node: NodeId,
        field: String,
        items: Vec<serde_json::Value>,
    },
    /// 出力の形式・量子化・展開を替える（つなぐ先は変えない）。
    SetOutput {
        output: OutputId,
        format: FormatChoice,
        quantize: Option<Quantize>,
        expand: Expand,
    },
    /// 文書を丸ごと替える（すべての操作を外す・レシピの読み込みなど。今のスキーマの文書）。
    Replace { graph: Graph },
}

/// まとめる手のキー（同じキーの続く Merge を 1 手にする）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum MergeKey {
    /// 引数の項目 1 つ（ノード, 名前）。
    Param(NodeId, String),
    /// 引数のまとめての変更（ノード）。
    Params(NodeId),
}

impl MergeKey {
    /// 手のキー。まとめられない手は None。
    fn of(edit: &Edit) -> Option<Self> {
        match edit {
            Edit::SetParam { node, key, .. } => Some(Self::Param(*node, key.clone())),
            Edit::SetParams { node, .. } => Some(Self::Params(*node)),
            _ => None,
        }
    }
}

/// 確定の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Committed {
    /// 確定したあとの文書の番号。
    pub rev: Rev,
    /// 足したノードの ID。
    pub created: Option<NodeId>,
    /// いちばん上の手にまとめた（手の数を増やしていない）。
    pub merged: bool,
    /// 何も変えなかった（手を積まない）。
    pub noop: bool,
}

/// 履歴の上限。先に届いたほうで、古い手から捨てる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    /// 取り消せる手の数。
    pub max_steps: usize,
    /// 履歴が持つ文書のバイト数（共有を除いた正規の JSON の長さの見積もり）。
    pub max_bytes: u64,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_steps: 1_000,
            max_bytes: 64 << 20,
        }
    }
}

/// 直前に捨てた仮の値をいくつ引けるようにしておくか（描いている途中の要求のため）。
const RECENT: usize = 4;

/// 履歴の手 1 つ。
struct Entry {
    rev: Rev,
    snap: Snapshot,
    /// この手が持つバイトの見積もり：土台は文書の長さ、ほかは 1 つ前の手と共有していない分。
    bytes: u64,
}

/// 仮の値。
struct Provisional {
    rev: Rev,
    snap: Snapshot,
    merge: Option<MergeKey>,
    created: Option<NodeId>,
}

/// エディタの履歴。描くときは `snapshot(rev)` を写して（`Arc` なので軽い）描くスレッドへ渡す。
pub struct EditSession {
    entries: VecDeque<Entry>,
    cursor: usize,
    provisional: Option<Provisional>,
    recent: VecDeque<(Rev, Snapshot)>,
    open_merge: Option<MergeKey>,
    next_rev: u64,
    saved: Option<Rev>,
    base: Digest,
    limits: HistoryLimits,
    bytes: u64,
}

impl EditSession {
    /// 頭の文書（今のスキーマ）と土台の版から作る。知らない操作・新しすぎる版（E8）・形の誤りは `Invalid`、
    /// 大きすぎる・深すぎる文書は `Limit`。開いた文書は保存したものとみなす（`is_dirty` は偽）。
    pub fn open(
        engine: &Engine,
        graph: &Graph,
        base: Digest,
        limits: HistoryLimits,
    ) -> Result<Self, EngineError> {
        let snap = edit::from_graph(engine, graph, None)?;
        let rev = Rev(0);
        let bytes = snap.encoded_len() as u64;
        Ok(Self {
            entries: VecDeque::from([Entry { rev, snap, bytes }]),
            cursor: 0,
            provisional: None,
            recent: VecDeque::new(),
            open_merge: None,
            next_rev: 1,
            saved: Some(rev),
            base,
            limits,
            bytes,
        })
    }

    /// 頭の文書の JSON（保存の形）と土台の版から作る（`Engine::load` を通す。古いスキーマは移す）。
    pub fn open_json(
        engine: &Engine,
        raw: &[u8],
        base: Digest,
        limits: HistoryLimits,
    ) -> Result<Self, EngineError> {
        let g = engine.load(raw)?;
        Self::open(engine, &g, base, limits)
    }

    /// 土台の版。
    pub fn base(&self) -> Digest {
        self.base
    }

    /// 履歴の上限。
    pub fn limits(&self) -> HistoryLimits {
        self.limits
    }

    fn entry(&self) -> &Entry {
        // カーソルはいつも履歴の中を指す（entries は空にならない）
        &self.entries[self.cursor]
    }

    /// 今の文書の番号（仮の値があればその番号）。
    pub fn rev(&self) -> Rev {
        self.provisional
            .as_ref()
            .map_or(self.entry().rev, |p| p.rev)
    }

    /// 確定した頭の番号。
    pub fn head_rev(&self) -> Rev {
        self.entry().rev
    }

    /// 今の文書（仮の値があればそれ）。
    pub fn current(&self) -> &Snapshot {
        self.provisional
            .as_ref()
            .map_or(&self.entry().snap, |p| &p.snap)
    }

    /// 確定した頭。
    pub fn head(&self) -> &Snapshot {
        &self.entry().snap
    }

    /// 仮の値を出しているか。
    pub fn is_provisional(&self) -> bool {
        self.provisional.is_some()
    }

    /// 番号の文書（履歴の手、今の仮の値、直前に捨てた仮の値 4 つまで）。それより古いものは None。
    pub fn snapshot(&self, rev: Rev) -> Option<&Snapshot> {
        self.entries
            .iter()
            .find(|e| e.rev == rev)
            .map(|e| &e.snap)
            .or_else(|| {
                self.provisional
                    .as_ref()
                    .filter(|p| p.rev == rev)
                    .map(|p| &p.snap)
            })
            .or_else(|| self.recent.iter().find(|(r, _)| *r == rev).map(|(_, s)| s))
    }

    fn fresh_rev(&mut self) -> Rev {
        let rev = Rev(self.next_rev);
        self.next_rev = self.next_rev.saturating_add(1);
        rev
    }

    /// 仮の値を捨てる（直前に捨てたものとして 4 つまで引ける）。
    fn discard_provisional(&mut self) {
        if let Some(p) = self.provisional.take() {
            self.recent.push_back((p.rev, p.snap));
            while self.recent.len() > RECENT {
                self.recent.pop_front();
            }
        }
    }

    /// 確定した頭に `edit` を当てた仮の値を出す（履歴に入れない。前の仮の値は捨てる）。何も変えなければ仮の値を消して
    /// 頭の番号を返す。断ったら状態は変えない。
    pub fn preview(&mut self, engine: &Engine, edit: Edit) -> Result<Rev, EngineError> {
        let out = edit::apply(engine, self.head(), &edit)?;
        self.discard_provisional();
        if out.snap.same_content(self.head()) {
            return Ok(self.head_rev());
        }
        let rev = self.fresh_rev();
        self.provisional = Some(Provisional {
            rev,
            snap: out.snap,
            merge: MergeKey::of(&edit),
            created: out.created,
        });
        Ok(rev)
    }

    /// 仮の値を捨てる。捨てたら頭の番号、なければ None。
    pub fn cancel_preview(&mut self) -> Option<Rev> {
        self.provisional.as_ref()?;
        self.discard_provisional();
        Some(self.head_rev())
    }

    /// 仮の値を確定する（番号は仮の値のもの。まとめて前の手と同じ中身に戻ったときは、その手の番号）。
    /// 仮の値がなければ何もしない（`noop`）。
    pub fn commit_preview(&mut self, mode: CommitMode) -> Result<Committed, EngineError> {
        let Some(p) = self.provisional.take() else {
            return Ok(self.noop());
        };
        let merge = match mode {
            CommitMode::Merge => p.merge,
            CommitMode::Step => None,
        };
        Ok(self.push(p.snap, Some(p.rev), merge, p.created))
    }

    /// 確定した頭に `edit` を当てて確定する（仮の値は捨てる）。断ったら状態は変えない（仮の値も残す）。
    pub fn commit(
        &mut self,
        engine: &Engine,
        edit: Edit,
        mode: CommitMode,
    ) -> Result<Committed, EngineError> {
        let out = edit::apply(engine, self.head(), &edit)?;
        self.discard_provisional();
        let merge = match mode {
            CommitMode::Merge => MergeKey::of(&edit),
            CommitMode::Step => None,
        };
        Ok(self.push(out.snap, None, merge, out.created))
    }

    fn noop(&self) -> Committed {
        Committed {
            rev: self.head_rev(),
            created: None,
            merged: false,
            noop: true,
        }
    }

    /// 確定した頭の上に `snap` を積む（まとめる・値を戻した手を消す・redo の枝を切る・上限で古い手を捨てる）。
    fn push(
        &mut self,
        snap: Snapshot,
        rev: Option<Rev>,
        merge: Option<MergeKey>,
        created: Option<NodeId>,
    ) -> Committed {
        if snap.same_content(self.head()) {
            return self.noop();
        }
        // 新しい手を積むと、redo の枝を切る
        while self.entries.len() > self.cursor + 1 {
            if let Some(cut) = self.entries.pop_back() {
                self.bytes = self.bytes.saturating_sub(cut.bytes);
            }
        }
        if let Some(key) = &merge
            && self.open_merge.as_ref() == Some(key)
            && self.cursor >= 1
        {
            // いちばん上の手を置き換える。1 つ前の手と同じ中身に戻ったら、その手を消す
            let below = &self.entries[self.cursor - 1];
            if snap.same_content(&below.snap) {
                let below_rev = below.rev;
                if let Some(top) = self.entries.pop_back() {
                    self.bytes = self.bytes.saturating_sub(top.bytes);
                }
                self.cursor -= 1;
                self.open_merge = None;
                return Committed {
                    rev: below_rev,
                    created: None,
                    merged: true,
                    noop: false,
                };
            }
            let added = snap.unshared_bytes(&below.snap);
            let rev = rev.unwrap_or_else(|| self.fresh_rev());
            let top = &mut self.entries[self.cursor];
            self.bytes = self.bytes.saturating_sub(top.bytes).saturating_add(added);
            *top = Entry {
                rev,
                snap,
                bytes: added,
            };
            self.trim();
            return Committed {
                rev,
                created,
                merged: true,
                noop: false,
            };
        }
        let added = snap.unshared_bytes(self.head());
        let rev = rev.unwrap_or_else(|| self.fresh_rev());
        self.entries.push_back(Entry {
            rev,
            snap,
            bytes: added,
        });
        self.cursor += 1;
        self.bytes = self.bytes.saturating_add(added);
        self.open_merge = merge;
        self.trim();
        Committed {
            rev,
            created,
            merged: false,
            noop: false,
        }
    }

    /// 上限を超えていれば、古い手から捨てる（次の手を新しい土台にする。カーソルは土台より前にならない）。
    fn trim(&mut self) {
        while self.entries.len() > 1
            && self.cursor > 0
            && (self.entries.len() - 1 > self.limits.max_steps
                || self.bytes > self.limits.max_bytes)
        {
            if let Some(old) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.bytes);
            }
            if let Some(base) = self.entries.front_mut() {
                self.bytes = self.bytes.saturating_sub(base.bytes);
                base.bytes = base.snap.encoded_len() as u64;
                self.bytes = self.bytes.saturating_add(base.bytes);
            }
            self.cursor -= 1;
        }
        // 土台になった手はまとめない
        if self.cursor == 0 {
            self.open_merge = None;
        }
    }

    /// まとめている途中の手を閉じる（次の `Merge` は新しい手になる）。
    pub fn seal(&mut self) {
        self.open_merge = None;
    }

    /// 1 手戻す（仮の値を捨て、区切ってから）。戻った先の番号、戻れなければ None。
    pub fn undo(&mut self) -> Option<Rev> {
        self.discard_provisional();
        self.open_merge = None;
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        Some(self.head_rev())
    }

    /// 1 手進める（仮の値を捨て、区切ってから）。進んだ先の番号、進めなければ None。
    pub fn redo(&mut self) -> Option<Rev> {
        self.discard_provisional();
        self.open_merge = None;
        if self.cursor + 1 >= self.entries.len() {
            return None;
        }
        self.cursor += 1;
        Some(self.head_rev())
    }

    /// 戻せるか。
    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    /// 進められるか。
    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// 履歴の手の数（土台を含む）。
    pub fn history_len(&self) -> usize {
        self.entries.len()
    }

    /// 履歴が持つ文書のバイト数の見積もり（共有を除いた正規の JSON の長さ。決定的にするため、アロケータの値ではなく
    /// 見積もりで数える）。仮の値と、捨てた仮の値は数えない。
    pub fn history_bytes(&self) -> u64 {
        self.bytes
    }

    /// 今の確定した頭を保存したことにする。
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.head_rev());
    }

    /// 確定した頭が、保存したものと違うか。
    pub fn is_dirty(&self) -> bool {
        self.saved != Some(self.head_rev())
    }

    /// 今の文書の正規の JSON の長さ。
    pub fn encoded_len(&self) -> usize {
        self.current().encoded_len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_chacha::rand_core::Rng;
    use serde_json::json;

    impl EditSession {
        /// 履歴のバイトの数え方を、土台から数え直したものと照らす（土台は文書の長さ、ほかは 1 つ前の手と共有していない分）。
        fn check_bytes(&self) {
            let mut total = 0u64;
            for (i, e) in self.entries.iter().enumerate() {
                let want = match i.checked_sub(1).and_then(|k| self.entries.get(k)) {
                    Some(prev) => e.snap.unshared_bytes(&prev.snap),
                    None => e.snap.encoded_len() as u64,
                };
                assert_eq!(e.bytes, want, "{i}");
                total += want;
            }
            assert_eq!(self.bytes, total);
        }
    }

    /// 積む・まとめる・値を戻して消す・redo の枝を切る・上限で捨てる、のどの手のあとも、履歴のバイトは数え直したものと同じで、
    /// 上限（手の数・バイト）を超えない（1 手で超えるときは今の手だけ）。
    #[test]
    fn history_bytes_equal_a_recount() {
        let engine = Engine::new().unwrap();
        let mut g = Graph::new_single();
        g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
            .unwrap();
        g.push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
            .unwrap();
        let limits = HistoryLimits {
            max_steps: 5,
            max_bytes: 6_000,
        };
        let mut s = EditSession::open(&engine, &g, Digest::default(), limits).unwrap();
        let mut r = crate::determinism::rng(0xb17e);
        let x = |v: u32| Edit::SetParam {
            node: NodeId(1),
            key: "x".into(),
            value: json!(v),
        };
        let (mut full_steps, mut near_bytes) = (0usize, 0usize);
        for step in 0..600u32 {
            match r.next_u32() % 9 {
                0 | 1 => {
                    let items = (0..r.next_u32() % 40)
                        .map(|i| json!({ "k": i, "s": step }))
                        .collect();
                    let e = Edit::AppendItems {
                        node: NodeId(2),
                        field: "actions".into(),
                        items,
                    };
                    s.commit(&engine, e, CommitMode::Step).unwrap();
                }
                2 | 3 => {
                    s.commit(&engine, x(r.next_u32() % 3), CommitMode::Merge)
                        .unwrap();
                }
                4 => {
                    s.undo();
                }
                5 => {
                    s.redo();
                }
                6 => {
                    s.preview(&engine, x(r.next_u32() % 3)).unwrap();
                    s.commit_preview(CommitMode::Merge).unwrap();
                }
                7 => {
                    let e = Edit::SetParams {
                        node: NodeId(2),
                        params: json!({ "actions": [] }),
                    };
                    s.commit(&engine, e, CommitMode::Step).unwrap();
                }
                _ => s.seal(),
            }
            s.check_bytes();
            assert!(s.history_len() <= limits.max_steps + 1);
            assert!(s.history_bytes() <= limits.max_bytes || s.history_len() == 1);
            assert!(s.cursor < s.entries.len());
            full_steps += usize::from(s.history_len() == limits.max_steps + 1);
            near_bytes += usize::from(
                s.history_bytes() * 4 > limits.max_bytes * 3 && s.history_len() <= limits.max_steps,
            );
        }
        // 手の数の上限とバイトの上限の両方に届いている
        assert!(
            full_steps > 20 && near_bytes > 20,
            "{full_steps} {near_bytes}"
        );
    }
}
