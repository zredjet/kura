//! 下書きを置くスレッド（docs/editing.md §12.12「下書き」。エディタごとに 1 つ）。
//!
//! - エディタは頭が変わるたび（確定・取り消し・やり直し）に、置く中身（頭の文書のスナップショット・土台・待っている保存の
//!   ジョブ）か「捨てる」を頼む。頼みは最新の 1 つだけを持つ（古いものは置かない）。
//! - 置くのは前に置いてから [`INTERVAL`] たってから（間引き）。文書を保存の形にして部品に分けて DB に書く（`drafts::save`）
//!   のはこのスレッドで、1 手の往復に載せない（5 万本の筆の文書の `to_graph` は約 24 ms）。
//! - エディタを閉じるときは、待っている頼みをすぐ置いてから止める（[`Autosave::close`]）。アプリが落ちても、最後に置いた
//!   下書きから開き直せる（S17）。

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kura_engine::session::Snapshot;

use crate::edit::drafts;
use crate::edit::save::Base;
use crate::library::Library;

/// 下書きを置く間隔の下限。
pub const INTERVAL: Duration = Duration::from_secs(3);

/// 置く頼み。
pub(super) enum DraftJob {
    /// 頭の文書（`snapshot`）と土台を置く。`after_job` はエディタが積んだ保存のジョブ（`base` はその土台）。
    Put {
        snapshot: Snapshot,
        base: Base,
        after_job: Option<i64>,
    },
    /// 下書きを捨てる（保存した頭に戻った）。
    Discard,
}

#[derive(Default)]
struct Queue {
    pending: Option<DraftJob>,
    /// 書いている途中。
    busy: bool,
    /// 待たずにすぐ書く（閉じる・捨てる前）。
    flush: bool,
    stop: bool,
    last: Option<Instant>,
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

/// 下書きを置くスレッド。
pub(super) struct Autosave {
    shared: Shared,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// スレッドを起こせなかったときに、その場で書くためのもの。
    library: Arc<Library>,
    item_id: i64,
    threaded: bool,
}

impl Autosave {
    /// アイテム `item_id` の下書きを置くスレッドを起こす（間隔は `interval`）。スレッドを起こせなければ、頼んだときにその場で
    /// 書く。
    pub(super) fn start(library: Arc<Library>, item_id: i64, interval: Duration) -> Self {
        let shared: Shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (worker, lib) = (shared.clone(), library.clone());
        let thread = std::thread::Builder::new()
            .name("kura-edit-draft".into())
            .spawn(move || run(&worker, &lib, item_id, interval))
            .map_err(
                |err| tracing::warn!(%err, "下書きのスレッドを起こせません（その場で書きます）"),
            )
            .ok();
        Self {
            shared,
            threaded: thread.is_some(),
            thread: Mutex::new(thread),
            library,
            item_id,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.shared.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 頼む（前の頼みは置き換える）。
    pub(super) fn request(&self, job: DraftJob) {
        if !self.threaded {
            write(&self.library, self.item_id, job);
            return;
        }
        let mut q = self.lock();
        q.pending = Some(job);
        self.shared.1.notify_all();
    }

    /// 待っている頼みを取り消す（下書きを捨てる前）。
    pub(super) fn cancel_pending(&self) {
        let mut q = self.lock();
        q.pending = None;
        // 書いている途中のものは書き終えるのを待つ（そのあとに捨てる）
        while q.busy {
            q = self
                .shared
                .1
                .wait(q)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// 待っている頼みをすぐ書き、書き終えるまで待つ（テスト用）。
    #[cfg(test)]
    pub(super) fn flush(&self) {
        let mut q = self.lock();
        if q.stop {
            return;
        }
        q.flush = true;
        self.shared.1.notify_all();
        while q.pending.is_some() || q.busy {
            q = self
                .shared
                .1
                .wait(q)
                .unwrap_or_else(PoisonError::into_inner);
        }
        q.flush = false;
    }

    /// 待っている頼みを書いてから止める（2 回目からは何もしない）。
    pub(super) fn close(&self) {
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(handle) = handle else {
            return;
        };
        {
            let mut q = self.lock();
            q.flush = true;
            q.stop = true;
            self.shared.1.notify_all();
        }
        if handle.join().is_err() {
            tracing::error!("下書きのスレッドが異常終了しました");
        }
    }
}

impl Drop for Autosave {
    fn drop(&mut self) {
        self.close();
    }
}

fn run(shared: &Shared, library: &Library, item_id: i64, interval: Duration) {
    let (lock, cv) = &**shared;
    loop {
        let job = {
            let mut q = lock.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if q.pending.is_some() {
                    let due = q
                        .last
                        .map_or(Duration::ZERO, |t| interval.saturating_sub(t.elapsed()));
                    if q.flush || q.stop || due.is_zero() {
                        q.busy = true;
                        break q.pending.take();
                    }
                    q = cv
                        .wait_timeout(q, due)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                } else if q.stop {
                    return;
                } else {
                    q = cv.wait(q).unwrap_or_else(PoisonError::into_inner);
                }
            }
        };
        if let Some(job) = job {
            write(library, item_id, job);
        }
        let mut q = lock.lock().unwrap_or_else(PoisonError::into_inner);
        q.busy = false;
        q.last = Some(Instant::now());
        cv.notify_all();
    }
}

fn write(library: &Library, item_id: i64, job: DraftJob) {
    let started = Instant::now();
    let r = match job {
        DraftJob::Put {
            snapshot,
            base,
            after_job,
        } => drafts::save(library.db(), item_id, &snapshot.to_graph(), base, after_job).map(|_| ()),
        DraftJob::Discard => drafts::discard(library.db(), item_id).map(|_| ()),
    };
    match r {
        Ok(()) => tracing::debug!(
            item_id,
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "下書きを置きました"
        ),
        Err(err) => tracing::warn!(%err, item_id, "下書きを置けませんでした"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::session::{EditSession, HistoryLimits};
    use kura_engine::{Digest, Engine, Graph};

    /// 頭の文書のスナップショット（反転を `n` 個）。
    fn snapshot(engine: &Engine, n: usize) -> Snapshot {
        let mut g = Graph::new_single();
        for _ in 0..n {
            g.push_linear(
                "image.flip",
                1,
                serde_json::json!({ "axis": "vertical" }),
                0,
            )
            .unwrap();
        }
        EditSession::open(engine, &g, Digest::default(), HistoryLimits::default())
            .unwrap()
            .head()
            .clone()
    }

    fn draft_root(library: &Library, item_id: i64) -> Option<Digest> {
        library
            .db()
            .read(|c| drafts::get(c, item_id))
            .unwrap()
            .map(|d| d.doc)
    }

    /// 間引き（間隔 1 時間）：1 回目の頼みはすぐ置き、間隔の中の 2 回目・3 回目は置かずに最新の 1 つだけを持つ。閉じると待っている
    /// 最新の頼みを置いてから止まり（S17 の前提：閉じても最後の下書きが残る）、閉じたあとの頼みは何もしない。
    #[test]
    fn requests_are_thinned_and_the_last_one_is_written_on_close() {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        let item = library
            .db()
            .write(|tx| Ok(crate::edit::testing::insert_item(tx, 1, [1; 32])))
            .unwrap();
        let engine = Engine::new().unwrap();
        let base = Base {
            hash: [1; 32],
            version: 0,
        };
        let put = |n: usize| DraftJob::Put {
            snapshot: snapshot(&engine, n),
            base,
            after_job: None,
        };
        let root = |n: usize| {
            kura_engine::doc_parts::split(&snapshot(&engine, n).to_graph())
                .unwrap()
                .root
        };
        let autosave = Autosave::start(library.clone(), item, Duration::from_secs(3600));
        assert!(autosave.threaded);
        // 1 回目はすぐ置く
        autosave.request(put(1));
        let deadline = Instant::now() + Duration::from_secs(30);
        while draft_root(&library, item) != Some(root(1)) {
            assert!(Instant::now() < deadline, "1 回目の頼みが置かれません");
            std::thread::sleep(Duration::from_millis(5));
        }
        // 間隔の中の頼みは置かない（最新の 1 つだけを持つ）
        autosave.request(put(2));
        autosave.request(put(3));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(draft_root(&library, item), Some(root(1)));
        // 閉じると、待っている最新の頼みを置いてから止まる
        autosave.close();
        assert_eq!(draft_root(&library, item), Some(root(3)));
        assert!(
            library
                .db()
                .read(|c| crate::edit::store::doc_id_by_digest(c, &root(2)))
                .unwrap()
                .is_none(),
            "置き換えた頼みは書かない"
        );
        // 閉じたあとの頼みは何もしない（2 回閉じても何もしない）
        autosave.request(DraftJob::Discard);
        autosave.close();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(draft_root(&library, item), Some(root(3)));
        library.close();
    }
}
