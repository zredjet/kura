//! 中止と進捗の口（docs/editing.md §13）。ジョブ（kura-core）が実装して、既存の進捗の通知につなぐ。
//! 予約（§12）の部品 [`Latest`]・[`Ticket`] もここに置く（世代の番号で古い描画を止める。時刻を使わない）。

use std::sync::Arc;
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

/// 最新の要求だけを生かす（§12 の予約）：要求ごとに世代の番号（1 から増える）を払い出し、[`Latest::supersede`] で
/// それより古い世代の描画を止める。時刻を使わない。止めるか、描き終えるのを待って最新を描くか（合流）の方針は呼び手
/// （M12 のエディタ）が持つ。
#[derive(Debug, Default)]
pub struct Latest {
    /// 最後に払い出した世代。
    issued: AtomicU64,
    /// これより古い世代は止める（床。下がらない）。
    floor: AtomicU64,
}

impl Latest {
    /// 何も払い出していない予約を作る。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 新しい世代を払い出す（古い世代はまだ止めない）。`parent`（ジョブ・エディタを閉じるときの中止）の中止も伝え、
    /// 進捗は `parent` に流す。
    pub fn issue(self: &Arc<Self>, parent: Arc<dyn Monitor + Send>) -> Ticket {
        let generation = self.issued.fetch_add(1, Ordering::AcqRel) + 1;
        Ticket {
            latest: Arc::clone(self),
            generation,
            parent,
        }
    }

    /// `generation` より古い世代をすべて止める（`generation` とそれより新しい世代は止めない）。床は下がらない。
    pub fn supersede(&self, generation: u64) {
        self.floor.fetch_max(generation, Ordering::AcqRel);
    }

    /// 最後に払い出した世代（まだ払い出していなければ 0）。
    pub fn latest(&self) -> u64 {
        self.issued.load(Ordering::Acquire)
    }

    /// 止めている世代の床（これより古い世代は止まる）。
    pub fn floor(&self) -> u64 {
        self.floor.load(Ordering::Acquire)
    }
}

/// [`Latest`] が払い出した 1 つの要求の中止の確認（`Monitor`）：世代が床より古くなるか、親が中止を頼めば止まる。
pub struct Ticket {
    latest: Arc<Latest>,
    generation: u64,
    parent: Arc<dyn Monitor + Send>,
}

impl Ticket {
    /// 世代の番号。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 新しい要求に取って代わられたか（親の中止は含めない）。
    pub fn superseded(&self) -> bool {
        self.generation < self.latest.floor()
    }
}

impl Monitor for Ticket {
    fn canceled(&self) -> bool {
        self.superseded() || self.parent.canceled()
    }

    fn progress(&self, done: u64, total: u64) {
        self.parent.progress(done, total);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::error::EngineError;
    use crate::testgen::InputGen;
    use crate::testing::{render_with, single_node_graph};
    use crate::value::PixelFormat;

    /// 進捗を覚える親。
    #[derive(Default)]
    struct Recording(Mutex<Vec<(u64, u64)>>);

    impl Monitor for Recording {
        fn canceled(&self) -> bool {
            false
        }

        fn progress(&self, done: u64, total: u64) {
            self.0.lock().unwrap().push((done, total));
        }
    }

    #[test]
    fn supersede_stops_only_older_generations() {
        let latest = Latest::new();
        assert_eq!((latest.latest(), latest.floor()), (0, 0));
        let a = latest.issue(Arc::new(NoMonitor));
        let b = latest.issue(Arc::new(NoMonitor));
        assert_eq!((a.generation(), b.generation()), (1, 2));
        assert!(!a.canceled() && !b.canceled());
        latest.supersede(b.generation());
        assert!(a.canceled() && a.superseded());
        assert!(!b.canceled());
        // あとで払い出した世代は止まらない。床は下がらない
        let c = latest.issue(Arc::new(NoMonitor));
        latest.supersede(1);
        assert_eq!(latest.floor(), 2);
        assert!(a.canceled() && !b.canceled() && !c.canceled());
        latest.supersede(c.generation());
        assert!(b.canceled() && !c.canceled());
        assert_eq!(latest.latest(), 3);
    }

    #[test]
    fn the_parent_cancel_and_progress_pass_through() {
        let latest = Latest::new();
        let parent = Arc::new(CancelFlag::default());
        let t = latest.issue(parent.clone());
        assert!(!t.canceled());
        parent.cancel();
        assert!(t.canceled() && !t.superseded());
        let rec = Arc::new(Recording::default());
        let t = latest.issue(rec.clone());
        t.progress(3, 10);
        assert_eq!(*rec.0.lock().unwrap(), vec![(3, 10)]);
    }

    /// 取って代わられた描画は Canceled になって出力を出さない。新しい世代の描画は同じ文書で最後まで描ける（E24）。
    #[test]
    fn superseded_renders_are_canceled() {
        let engine = crate::Engine::new().unwrap();
        let graph = single_node_graph(
            "image.resize",
            1,
            serde_json::json!({ "width": { "px": 150 }, "height": "auto", "filter": "lanczos3" }),
        )
        .unwrap();
        let input = InputGen::Noise {
            w: 600,
            h: 400,
            fmt: PixelFormat::Rgba8,
            seed: 7,
        }
        .generate()
        .unwrap();
        let source = crate::Digest([7; 32]);
        let latest = Latest::new();
        let old = latest.issue(Arc::new(NoMonitor));
        let new = latest.issue(Arc::new(NoMonitor));
        latest.supersede(new.generation());
        let r = render_with(&engine, &graph, input.clone(), source, &old);
        assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
        let got = render_with(&engine, &graph, input.clone(), source, &new).unwrap();
        let want = render_with(&engine, &graph, input, source, &NoMonitor).unwrap();
        assert_eq!(got.digest().unwrap(), want.digest().unwrap());
    }
}
