//! 戻らないことがある OS の呼び出し（ImageIO の展開）を、専用のスレッドで時間切れつきで行う。OS によらない（偽の仕事で
//! Linux でもテストする）。
//!
//! - 壊れた HEIC で、ImageIO の展開（`kura_img_begin` の `CGDataProviderCopyData`。中の VideoToolbox が XPC の返事を
//!   待つ）が戻らなくなることがある（Apple 側の不具合。同じファイルで `sips` も止まる）。同じプロセスで動かすという決定は
//!   そのままにして、呼び出しを専用のスレッドで行い、呼び出し側は [`POLL`] ごとに中止を見ながら待つ。
//! - 時間切れか中止のときは、スレッドを待たずに見捨てて戻る。仕事が持つもの（Swift のハンドル）は呼び出し側では閉じない
//!   （呼び出しの途中のハンドルを別のスレッドから閉じると未定義動作）。仕事があとで戻ったら、スレッドの中で落とす（閉じる）。
//!   戻らなければ、スレッド・ハンドル・入力の写しは漏れたまま。
//! - 見捨てて、まだ戻っていないスレッドの数に上限（`max_stalled`）を設ける。達したら新しい呼び出しを断る（呼び出し側は
//!   Platform の段を断り、次の段へ回す）。

// macOS 以外では使う側（ImageIO の backend）がない（Linux ではテストだけが使う）
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use kura_engine::Monitor;
use kura_engine::monitor::CancelFlag;

/// 待つあいだに中止を確かめる間隔。
pub(crate) const POLL: Duration = Duration::from_millis(20);

/// 見捨てた呼び出しで戻らなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// 時間切れ（上限の時間）。スレッドを見捨てた。
    TimedOut(Duration),
    /// 中止を頼まれた。スレッドを見捨てた（呼び出し前に頼まれていたら、スレッドを作っていない）。
    Canceled,
    /// 見捨てて戻っていないスレッドが上限（この数）に達しているので、呼ばなかった。
    Exhausted(usize),
    /// スレッドを作れなかった（呼ばなかった）。
    Spawn(String),
    /// 仕事が panic した（持ち物はスレッドの中で落ちた）。
    Panicked,
}

/// 見張り（見捨てたスレッドの数を数える。プロセスで 1 つ、テストでは別に作る）。
pub(crate) struct Watchdog {
    stalled: AtomicUsize,
    max_stalled: usize,
}

/// 仕事の結果の受け渡し。
enum Slot<T> {
    Waiting,
    Done(std::thread::Result<T>),
    Abandoned,
}

struct Shared<T> {
    slot: Mutex<Slot<T>>,
    ready: Condvar,
    /// 見捨てたときに立てる（仕事の中で止められる所が見る）。
    cancel: CancelFlag,
}

/// 毒の入った Mutex も中身を使う（どちらの側も、中身を壊れたまま残す途中で panic しない）。
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Watchdog {
    /// 見捨てたスレッドの数の上限を決めて作る。
    pub(crate) const fn new(max_stalled: usize) -> Self {
        Self {
            stalled: AtomicUsize::new(0),
            max_stalled,
        }
    }

    /// 見捨てて、まだ戻っていないスレッドの数。
    pub(crate) fn stalled(&self) -> usize {
        self.stalled.load(Ordering::Acquire)
    }

    /// 上限に達していればその数（呼ぶ前に確かめるだけなので、同時に呼ぶと、呼んだ数だけ上限を超えて見捨てることがある）。
    pub(crate) fn exhausted(&self) -> Option<usize> {
        let n = self.stalled();
        (n >= self.max_stalled).then_some(n)
    }

    /// `work` を専用のスレッド（名前 `name`）で呼び、戻った値を返す。`limit` までに戻らないか、`mon` が中止を返したら、
    /// スレッドを見捨てて `Stop` を返す。`work` には、見捨てたときに中止を返す Monitor を渡す。`work` が持つものは、
    /// 戻ったときにスレッドの中で落ちる（見捨てたあとでも）。上限に達しているか、呼ぶ前に中止を頼まれていれば呼ばない
    /// （`work` はここで落ちる）。
    pub(crate) fn run<T, F>(
        &'static self,
        name: &str,
        limit: Duration,
        mon: &dyn Monitor,
        work: F,
    ) -> Result<T, Stop>
    where
        T: Send + 'static,
        F: FnOnce(&dyn Monitor) -> T + Send + 'static,
    {
        if mon.canceled() {
            return Err(Stop::Canceled);
        }
        if let Some(n) = self.exhausted() {
            return Err(Stop::Exhausted(n));
        }
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot::Waiting),
            ready: Condvar::new(),
            cancel: CancelFlag::default(),
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let out = catch_unwind(AssertUnwindSafe(|| work(&worker.cancel)));
                let mut slot = lock(&worker.slot);
                if matches!(*slot, Slot::Abandoned) {
                    drop(slot);
                    // 見捨てられたあとに戻った：持ち物（ハンドル）をここで落としてから数を戻す
                    drop(out);
                    self.stalled.fetch_sub(1, Ordering::AcqRel);
                } else {
                    *slot = Slot::Done(out);
                    worker.ready.notify_all();
                }
            })
            .map_err(|e| Stop::Spawn(e.to_string()))?;
        let deadline = Instant::now().checked_add(limit);
        let mut slot = lock(&shared.slot);
        // 待つ → 戻っていれば値 → 時間切れ・中止なら見捨てる、を繰り返す（すぐ戻る仕事では中止を尋ねない）。判断と
        // 見捨てる印は、仕事が結果を置くのと同じ鍵の中で行う（数え漏れを作らない）
        loop {
            let now = Instant::now();
            let wait = deadline.map_or(POLL, |d| d.saturating_duration_since(now).min(POLL));
            if !matches!(*slot, Slot::Done(_)) && !wait.is_zero() {
                slot = match shared.ready.wait_timeout(slot, wait) {
                    Ok((g, _)) => g,
                    Err(e) => e.into_inner().0,
                };
            }
            if matches!(*slot, Slot::Done(_)) {
                return match std::mem::replace(&mut *slot, Slot::Waiting) {
                    Slot::Done(Ok(v)) => Ok(v),
                    _ => Err(Stop::Panicked),
                };
            }
            let stop = if mon.canceled() {
                Some(Stop::Canceled)
            } else if deadline.is_some_and(|d| Instant::now() >= d) {
                Some(Stop::TimedOut(limit))
            } else {
                None
            };
            if let Some(stop) = stop {
                *slot = Slot::Abandoned;
                self.stalled.fetch_add(1, Ordering::AcqRel);
                shared.cancel.cancel();
                return Err(stop);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    use kura_engine::NoMonitor;
    use kura_engine::monitor::CancelAfter;

    use super::*;

    /// テストごとの見張り（プロセスの数を汚さない）。
    fn watchdog(max: usize) -> &'static Watchdog {
        Box::leak(Box::new(Watchdog::new(max)))
    }

    /// 落ちたことを知らせる持ち物（ハンドルの代わり）。
    struct Token(Arc<AtomicBool>);
    impl Drop for Token {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < Duration::from_secs(10), "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 時間内に戻れば値が返り、数は増えない。
    #[test]
    fn returns_the_value_in_time() {
        let w = watchdog(1);
        let got = w.run("t", Duration::from_secs(10), &NoMonitor, |m| {
            (
                m.canceled(),
                std::thread::current().name().map(str::to_string),
            )
        });
        assert_eq!(got, Ok((false, Some("t".to_string()))));
        assert_eq!(w.stalled(), 0);
    }

    /// 時間切れ：待たずに戻り、スレッドを見捨てる（持ち物は落とさない）。仕事があとで戻ると、スレッドの中で持ち物を落とし、
    /// 数が戻る。見捨てた仕事には中止が見える。
    #[test]
    fn times_out_and_abandons_the_thread() {
        let w = watchdog(4);
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (seen_tx, seen_rx) = mpsc::channel::<bool>();
        let dropped = Arc::new(AtomicBool::new(false));
        let token = Token(dropped.clone());
        let start = Instant::now();
        let got = w.run("t", Duration::from_millis(100), &NoMonitor, move |m| {
            let _token = token;
            let _ = go_rx.recv();
            let _ = seen_tx.send(m.canceled());
        });
        assert_eq!(got, Err(Stop::TimedOut(Duration::from_millis(100))));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(w.stalled(), 1);
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !dropped.load(Ordering::SeqCst),
            "見捨てた持ち物を呼び出し側で落とした"
        );
        go_tx.send(()).unwrap();
        assert_eq!(seen_rx.recv(), Ok(true));
        wait_until("数が戻らない", || w.stalled() == 0);
        assert!(dropped.load(Ordering::SeqCst));
    }

    /// 中止：時間切れを待たずに見捨てて戻る。呼ぶ前に頼まれていればスレッドを作らない。
    #[test]
    fn cancel_abandons_without_waiting_for_the_limit() {
        let w = watchdog(4);
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let start = Instant::now();
        let got = w.run(
            "t",
            Duration::from_secs(600),
            &CancelAfter::new(3),
            move |_| {
                let _ = go_rx.recv();
            },
        );
        assert_eq!(got, Err(Stop::Canceled));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(w.stalled(), 1);
        go_tx.send(()).unwrap();
        wait_until("数が戻らない", || w.stalled() == 0);

        let ran = Arc::new(AtomicBool::new(false));
        let r = ran.clone();
        let got = w.run(
            "t",
            Duration::from_secs(1),
            &CancelAfter::new(1),
            move |_| r.store(true, Ordering::SeqCst),
        );
        assert_eq!(got, Err(Stop::Canceled));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!ran.load(Ordering::SeqCst));
    }

    /// 見捨てたスレッドが上限に達したら呼ばない（仕事はその場で落ちる）。1 本戻れば、また呼べる。
    #[test]
    fn refuses_when_too_many_threads_are_stalled() {
        let w = watchdog(2);
        let mut gates = Vec::new();
        for _ in 0..2 {
            let (tx, rx) = mpsc::channel::<()>();
            gates.push(tx);
            let got = w.run("t", Duration::from_millis(10), &NoMonitor, move |_| {
                let _ = rx.recv();
            });
            assert_eq!(got, Err(Stop::TimedOut(Duration::from_millis(10))));
        }
        assert_eq!(w.exhausted(), Some(2));
        let dropped = Arc::new(AtomicBool::new(false));
        let token = Token(dropped.clone());
        let got = w.run("t", Duration::from_secs(1), &NoMonitor, move |_| {
            drop(token)
        });
        assert_eq!(got, Err(Stop::Exhausted(2)));
        assert!(dropped.load(Ordering::SeqCst));
        gates[0].send(()).unwrap();
        wait_until("数が戻らない", || w.stalled() == 1);
        assert_eq!(w.exhausted(), None);
        assert_eq!(
            w.run("t", Duration::from_secs(10), &NoMonitor, |_| 7),
            Ok(7)
        );
        gates[1].send(()).unwrap();
        wait_until("数が戻らない", || w.stalled() == 0);
    }

    /// 仕事の panic は Panicked になり、数は増えない（持ち物はスレッドの中で落ちる）。
    #[test]
    fn panics_in_the_work_are_reported() {
        let w = watchdog(1);
        let dropped = Arc::new(AtomicBool::new(false));
        let token = Token(dropped.clone());
        let got: Result<(), Stop> = w.run("t", Duration::from_secs(10), &NoMonitor, move |_| {
            let _token = token;
            panic!("仕事の panic");
        });
        assert_eq!(got, Err(Stop::Panicked));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(w.stalled(), 0);
    }
}
