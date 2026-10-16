//! 中止と進捗の口（docs/editing.md §13）。ジョブ（kura-core）が実装して、既存の進捗の通知につなぐ。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// 中止の確認と進捗の通知。
pub trait Monitor: Sync {
    /// 中止を頼まれていれば true。操作はブロック・タイルの合間で確かめる。
    fn canceled(&self) -> bool;
    /// 進捗（`done / total`。単位は操作が決める：画素・フレームなど）。
    fn progress(&self, _done: u64, _total: u64) {}
}

/// 何もしない（中止しない・進捗を捨てる）。
pub struct NoMonitor;

impl Monitor for NoMonitor {
    fn canceled(&self) -> bool {
        false
    }
}

/// 中止の旗だけを持つ（テストや、プレビューの古い描画を止めるとき）。
#[derive(Default)]
pub struct CancelFlag(AtomicBool);

impl CancelFlag {
    /// 中止を頼む。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Monitor for CancelFlag {
    fn canceled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// `canceled` を呼ばれた回数を数え、`after` 回目から中止を返す（テストで、操作の途中の中止を確かめる）。
/// `after` が 0 なら中止しない（数えるだけ）。
pub struct CancelAfter {
    after: u64,
    calls: AtomicU64,
}

impl CancelAfter {
    /// `after` 回目の呼び出しから中止を返す。
    pub fn new(after: u64) -> Self {
        Self {
            after,
            calls: AtomicU64::new(0),
        }
    }

    /// これまでに `canceled` が呼ばれた回数。
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Monitor for CancelAfter {
    fn canceled(&self) -> bool {
        let n = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        self.after != 0 && n >= self.after
    }
}
