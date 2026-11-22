//! メモリの予算（docs/editing.md §12。NFR-17・E25）：エディタ（対話）と一括で 1 つの予算を分け合う、バイトの重みの
//! セマフォ。macOS では大きな確保も失敗しないので（1000 GiB の `try_reserve` が通る）、確保の失敗では予算を守れない。
//! 描く前の見積もりでここから確保し、足りなければ断る。
//!
//! - 予算の全体を超える要求は、待たずに `Limit`（描く前に断る。操作は 1 つも動かない）。
//! - 対話（[`Class::Interactive`]）は待たない：今すぐ確保できなければ [`MemoryBudget::try_acquire`] は `None`、
//!   [`MemoryBudget::acquire`] は `Limit`（呼び手がキャッシュを追い出してから確保し直す。UI を止めない）。
//! - 一括（[`Class::Bulk`]）は確保できるまで待つ（中止を見る）。先に待っている一括（下の駐車したものを除く）を追い越さない
//!   （来た順）。対話は待っている一括を追い越してよい。
//! - エディタを開いている間は [`MemoryBudget::set_reserved`] で取り置く（既定は [`default_reserved`]：min(1 GiB, 予算/2)）。
//!   一括が使えるのは「予算 − max(取り置き, 対話の使用量)」まで。
//! - 駐車：「予算 − max(取り置き, 持ち続ける対話)」を超える一括の要求は、エディタが取り置きか持ち続ける確保を下げる（閉じる）
//!   まで通りえないので、待ちの列に残したまま先頭の判定から外し、後ろの収まる一括（エディタを開いたままの保存など）を
//!   塞がない（[`Usage::parked`]）。持ち続ける対話は、手の合間も持つ対話の確保（エディタのキャッシュの確保が
//!   [`Lease::set_standing`] で申告する。入力と固定した値で取り置きを超えうる）で、描く間だけ伸ばす分は入れない。下がれば、
//!   来た順の位置のまま先頭の判定に戻る。
//! - OS を知らない（物理メモリの量は呼び手が渡す）。時刻を値に使わない（待つときに中止を見回る間隔だけ）。
//! - 予算の確保（待つことがある）はスレッドプールの外（ジョブのスレッド・プレビューの輪のスレッド）で行う。プールの
//!   ワーカーの中で待つと、ほかの描画のワーカーを塞いで遅くなる（kura-core の EngineRuntime が確かめる）。

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::shape::Shape;

const GIB: u64 = 1 << 30;

/// 一括が待っている間に中止を見回る間隔。値には効かない（Monitor には中止を知らせる口がないので、見回って気づく）。
const POLL: Duration = Duration::from_millis(2);

/// 予算の全体の既定：min(物理メモリ/4, 4 GiB)。物理メモリが分からなければ（`None`・0）2 GiB。
pub fn default_total(physical: Option<u64>) -> u64 {
    match physical {
        Some(p) if p > 0 => (p / 4).clamp(1, 4 * GIB),
        _ => 2 * GIB,
    }
}

/// エディタを開いている間の取り置きの既定：min(1 GiB, 予算/2)。
pub fn default_reserved(total: u64) -> u64 {
    (total / 2).min(GIB)
}

/// 形の値の中身のバイト数（[`Shape::value_bytes`] と同じ。`Limits::check` と同じ式で、寸法は上限で数え、あふれは
/// `u64::MAX` に飽和する）。予算の見積もりと、コーデックの作業領域の既定に使う。
pub fn value_bytes(shape: &Shape) -> u64 {
    shape.value_bytes()
}

/// 誰の確保か。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// エディタ（プレビュー・筆・スポイト）。待たない。取り置きの中なら一括に邪魔されない。
    Interactive,
    /// 一括加工・保存などのジョブ。確保できるまで待つ（中止できる）。
    Bulk,
}

/// 使用量の写し（ジョブの表示に「予算を待っている」理由を出すのに使う）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// 予算の全体。
    pub total: u64,
    /// 対話が使っている量。
    pub interactive: u64,
    /// 一括が使っている量。
    pub bulk: u64,
    /// 取り置き（一括が使えない分）。
    pub reserved: u64,
    /// 対話のうち、手の合間も持ち続ける分（エディタのキャッシュの確保。[`Lease::set_standing`]）。駐車の条件に入る。
    pub standing: u64,
    /// 待っている一括の要求の数（駐車したものを含む）。
    pub waiting: usize,
    /// 待っているうち、今の取り置きと持ち続ける対話のもとでは通りえない（予算 − max(取り置き, 持ち続ける対話) を超える）
    /// ので駐車している数。エディタが取り置きか持ち続ける確保を下げる（閉じる）まで通らない。
    pub parked: usize,
}

#[derive(Default)]
struct State {
    interactive: u64,
    bulk: u64,
    reserved: u64,
    /// 対話のうち、手の合間も持ち続ける分の合計（確保ごとの [`Lease::set_standing`] の和。対話の使用量以下）。
    standing: u64,
    /// 待っている一括の要求（番号 → バイト）。番号の順（来た順）に通す。
    waiting: BTreeMap<u64, u64>,
    next: u64,
}

impl State {
    fn used(&self) -> u64 {
        self.interactive.saturating_add(self.bulk)
    }

    /// 今の取り置きと持ち続ける対話のもとでは通りえない一括の要求か（予算 − max(取り置き, 持ち続ける対話) を超える）。
    /// 駐車した要求は先頭の判定から外し、後ろの収まる一括を塞がない。どちらかが下がれば駐車でなくなり、来た順の位置（番号）
    /// のまま先頭の判定に戻る。描く間だけ伸ばす対話（`admit_cached` のピーク・プレビューの確保）は見ない（すぐ返るので、
    /// それで駐車させると大きな一括が追い越され続ける）。
    fn parked(&self, total: u64, bytes: u64) -> bool {
        bytes > total.saturating_sub(self.reserved.max(self.standing))
    }

    /// 待ちの列の先頭：駐車していない待ちのうち、いちばん古い番号。
    fn head(&self, total: u64) -> Option<u64> {
        self.waiting
            .iter()
            .find(|(_, b)| !self.parked(total, **b))
            .map(|(t, _)| *t)
    }

    fn slot(&mut self, class: Class) -> &mut u64 {
        match class {
            Class::Interactive => &mut self.interactive,
            Class::Bulk => &mut self.bulk,
        }
    }
}

/// メモリの予算（バイト）。`Arc` で共有し、確保した分は [`Lease`] が落とすときに返す。
pub struct MemoryBudget {
    total: u64,
    state: Mutex<State>,
    cv: Condvar,
}

fn poisoned<T>(_: PoisonError<T>) -> EngineError {
    lock_broken()
}

fn lock_broken() -> EngineError {
    EngineError::Internal(
        "メモリの予算の鍵が壊れています（鍵を持ったまま止まったスレッドがあります）".into(),
    )
}

fn over_total(bytes: u64, total: u64) -> EngineError {
    EngineError::Limit(format!(
        "メモリの予算を超えます（{bytes} バイト。予算は {total} バイト）"
    ))
}

impl MemoryBudget {
    /// 全体 `total` バイトの予算を作る。
    pub fn new(total: u64) -> Arc<Self> {
        Arc::new(Self {
            total,
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
        })
    }

    /// 予算の全体。
    pub fn total(&self) -> u64 {
        self.total
    }

    /// 今の使用量の写し。
    pub fn usage(&self) -> Result<Usage, EngineError> {
        let s = self.lock()?;
        Ok(Usage {
            total: self.total,
            interactive: s.interactive,
            bulk: s.bulk,
            reserved: s.reserved,
            standing: s.standing,
            waiting: s.waiting.len(),
            parked: s
                .waiting
                .values()
                .filter(|b| s.parked(self.total, **b))
                .count(),
        })
    }

    /// 対話と一括の使用量の合計。
    pub fn used(&self) -> Result<u64, EngineError> {
        Ok(self.lock()?.used())
    }

    /// 取り置きを `bytes` にする（エディタを開く・閉じるとき。全体を超える分は全体に切り詰める）。下げると、待っている
    /// 一括が通りうる。
    pub fn set_reserved(&self, bytes: u64) -> Result<(), EngineError> {
        let mut s = self.lock()?;
        s.reserved = bytes.min(self.total);
        self.cv.notify_all();
        Ok(())
    }

    /// 待たずに確保する。今すぐ確保できなければ `None`（一括は、先に待っている一括（駐車したものを除く）がいても
    /// `None`）。予算の全体を超える量は `Limit`。
    pub fn try_acquire(
        self: &Arc<Self>,
        bytes: u64,
        class: Class,
    ) -> Result<Option<Lease>, EngineError> {
        if bytes > self.total {
            return Err(over_total(bytes, self.total));
        }
        let mut s = self.lock()?;
        if !self.admits(&s, class, bytes) {
            return Ok(None);
        }
        *s.slot(class) += bytes;
        Ok(Some(self.lease(bytes, class)))
    }

    /// 確保する。一括は確保できるまで待ち（来た順。今の取り置きと持ち続ける対話のもとで通りえない要求は駐車して、後ろを
    /// 塞がない）、
    /// 待っている間に `mon` が中止を頼めば `Canceled`（使用量は変えない）。
    /// 対話は待たない：今すぐ確保できなければ `Limit`。予算の全体を超える量は、どちらも待たずに `Limit`。
    pub fn acquire(
        self: &Arc<Self>,
        bytes: u64,
        class: Class,
        mon: &dyn Monitor,
    ) -> Result<Lease, EngineError> {
        if bytes > self.total {
            return Err(over_total(bytes, self.total));
        }
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let mut s = self.lock()?;
        if class == Class::Interactive {
            if !self.admits(&s, class, bytes) {
                return Err(EngineError::Limit(format!(
                    "メモリの予算が足りません（{bytes} バイト。使用中 {} バイト・予算 {} バイト）",
                    s.used(),
                    self.total
                )));
            }
            s.interactive += bytes;
            return Ok(self.lease(bytes, class));
        }
        let ticket = s.next;
        s.next += 1;
        s.waiting.insert(ticket, bytes);
        loop {
            let first = s.head(self.total) == Some(ticket);
            if first && self.fits(&s, Class::Bulk, bytes) {
                s.waiting.remove(&ticket);
                s.bulk += bytes;
                // 次に待っている一括も通るかもしれない
                self.cv.notify_all();
                return Ok(self.lease(bytes, class));
            }
            if mon.canceled() {
                s.waiting.remove(&ticket);
                self.cv.notify_all();
                return Err(EngineError::Canceled);
            }
            s = match self.cv.wait_timeout(s, POLL) {
                Ok((guard, _)) => guard,
                Err(e) => {
                    // 毒された鍵でも、待ちの列から外してから返す（後ろの一括を塞がない）
                    let (mut guard, _) = e.into_inner();
                    guard.waiting.remove(&ticket);
                    self.cv.notify_all();
                    return Err(lock_broken());
                }
            };
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, EngineError> {
        self.state.lock().map_err(poisoned)
    }

    fn lease(self: &Arc<Self>, bytes: u64, class: Class) -> Lease {
        Lease {
            budget: Arc::clone(self),
            bytes,
            class,
            standing: 0,
        }
    }

    /// 今すぐ通せるか（待たない確保・伸ばすときの判断）。一括は、先に待っている一括（駐車したものを除く）を追い越さない。
    fn admits(&self, s: &State, class: Class, bytes: u64) -> bool {
        (class == Class::Interactive || s.head(self.total).is_none()) && self.fits(s, class, bytes)
    }

    /// 量として収まるか：全体で予算以内。一括はさらに「予算 − max(取り置き, 対話の使用量)」以内。
    fn fits(&self, s: &State, class: Class, bytes: u64) -> bool {
        if s.used().saturating_add(bytes) > self.total {
            return false;
        }
        match class {
            Class::Interactive => true,
            Class::Bulk => {
                s.bulk.saturating_add(bytes)
                    <= self.total.saturating_sub(s.reserved.max(s.interactive))
            }
        }
    }
}

/// 確保した分。落とすと予算に返す。
pub struct Lease {
    budget: Arc<MemoryBudget>,
    bytes: u64,
    class: Class,
    /// このうち、手の合間も持ち続ける分（[`Lease::set_standing`]。`bytes` 以下）。
    standing: u64,
}

impl Lease {
    /// 確保しているバイト数。
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// 誰の確保か。
    pub fn class(&self) -> Class {
        self.class
    }

    /// 手の合間も持ち続ける分（[`Lease::set_standing`]）。
    pub fn standing(&self) -> u64 {
        self.standing
    }

    /// この確保のうち、手の合間も持ち続ける分を `bytes`（今の確保まで）にする。対話の確保だけ（エディタのキャッシュの確保
    /// [`crate::render::estimate::CacheLease`] が、描いたあとの `settle` で確保の全体を申告する）。持ち続ける対話は一括の
    /// 駐車の条件に入る（「予算 − max(取り置き, 持ち続ける対話)」を超える一括は駐車し、後ろの収まる一括を塞がない）。
    /// 描く間だけ伸ばす分は申告しない（すぐ返るので、それで駐車させると大きな一括が追い越され続ける）。一括の確保は
    /// `Internal`。
    pub fn set_standing(&mut self, bytes: u64) -> Result<(), EngineError> {
        if self.class != Class::Interactive {
            return Err(EngineError::Internal(
                "一括の確保は、持ち続ける対話として申告できません".into(),
            ));
        }
        let bytes = bytes.min(self.bytes);
        let mut s = self.budget.lock()?;
        s.standing = s
            .standing
            .saturating_sub(self.standing)
            .saturating_add(bytes);
        self.standing = bytes;
        // 駐車が変わりうる（下がれば駐車でなくなり、上がれば後ろの一括が先頭になりうる）
        self.budget.cv.notify_all();
        Ok(())
    }

    /// `bytes` まで減らして、差を返す（今より大きければ何もしない）。持ち続ける分も `bytes` までにする。
    pub fn shrink_to(&mut self, bytes: u64) -> Result<(), EngineError> {
        if bytes >= self.bytes {
            return Ok(());
        }
        let mut s = self.budget.lock()?;
        let slot = s.slot(self.class);
        *slot = slot.saturating_sub(self.bytes - bytes);
        if self.standing > bytes {
            s.standing = s.standing.saturating_sub(self.standing - bytes);
            self.standing = bytes;
        }
        self.bytes = bytes;
        self.budget.cv.notify_all();
        Ok(())
    }

    /// 待たずに `bytes` まで増やす。今すぐ増やせなければ false で、確保はそのまま（一括は、駐車していない待ちの一括がいれば
    /// 増やさない）。予算の全体を超える量は `Limit`。今以下なら何もせず true。
    pub fn try_grow_to(&mut self, bytes: u64) -> Result<bool, EngineError> {
        if bytes <= self.bytes {
            return Ok(true);
        }
        if bytes > self.budget.total {
            return Err(over_total(bytes, self.budget.total));
        }
        let mut s = self.budget.lock()?;
        let add = bytes - self.bytes;
        if !self.budget.admits(&s, self.class, add) {
            return Ok(false);
        }
        *s.slot(self.class) += add;
        self.bytes = bytes;
        Ok(true)
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // 鍵が毒されていても返す（返さないと、ほかの確保がずっと通らない）
        let mut s = self
            .budget
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let slot = s.slot(self.class);
        *slot = slot.saturating_sub(self.bytes);
        s.standing = s.standing.saturating_sub(self.standing);
        self.budget.cv.notify_all();
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("bytes", &self.bytes)
            .field("class", &self.class)
            .field("standing", &self.standing)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::monitor::{CancelAfter, CancelFlag, NoMonitor};
    use crate::shape::Dim;
    use crate::value::PixelFormat;

    /// 待っている一括の数が `n` になるまで待つ（スレッドが列に入ったのを確かめてから次へ進む）。
    fn wait_for_waiting(b: &MemoryBudget, n: usize) {
        while b.usage().unwrap().waiting != n {
            std::thread::yield_now();
        }
    }

    #[test]
    fn defaults_follow_the_physical_memory() {
        assert_eq!(default_total(Some(8 * GIB)), 2 * GIB);
        assert_eq!(default_total(Some(64 * GIB)), 4 * GIB);
        assert_eq!(default_total(Some(128 * GIB)), 4 * GIB);
        assert_eq!(default_total(Some(6 * GIB)), 6 * GIB / 4);
        assert_eq!(default_total(None), 2 * GIB);
        assert_eq!(default_total(Some(0)), 2 * GIB);
        assert_eq!(default_reserved(4 * GIB), GIB);
        assert_eq!(default_reserved(2 * GIB), GIB);
        assert_eq!(default_reserved(GIB), GIB / 2);
    }

    #[test]
    fn value_bytes_is_the_limits_formula() {
        let img = Shape::Image {
            w: Dim::Exact(300),
            h: Dim::AtMost(200),
            fmt: PixelFormat::Rgba16,
        };
        assert_eq!(value_bytes(&img), 300 * 200 * 8);
        let frames = Shape::Frames {
            count: Dim::Exact(3),
            w: Dim::Exact(10),
            h: Dim::Exact(10),
            fmt: PixelFormat::Indexed8,
            full: crate::shape::Fullness::Full,
        };
        assert_eq!(value_bytes(&frames), 300);
        let audio = Shape::Audio {
            rate: 48_000,
            channels: 2,
            frames: Dim::Exact(1_000),
        };
        assert_eq!(value_bytes(&audio), 8_000);
        let huge = Shape::Image {
            w: Dim::Exact(u64::MAX),
            h: Dim::Exact(u64::MAX),
            fmt: PixelFormat::Rgba8,
        };
        assert_eq!(value_bytes(&huge), u64::MAX);
    }

    /// 予算の全体を超える量は、どちらのクラスも待たずに Limit（待つと永久に通らない）。
    #[test]
    fn over_the_total_is_a_limit_without_waiting() {
        let b = MemoryBudget::new(100);
        for class in [Class::Interactive, Class::Bulk] {
            assert!(matches!(
                b.acquire(101, class, &NoMonitor),
                Err(EngineError::Limit(_))
            ));
            assert!(matches!(
                b.try_acquire(101, class),
                Err(EngineError::Limit(_))
            ));
        }
        assert_eq!(b.used().unwrap(), 0);
        let mut l = b.acquire(100, Class::Bulk, &NoMonitor).unwrap();
        assert_eq!(l.bytes(), 100);
        assert!(matches!(l.try_grow_to(101), Err(EngineError::Limit(_))));
    }

    #[test]
    fn dropping_a_lease_returns_it() {
        let b = MemoryBudget::new(100);
        let l = b.acquire(60, Class::Bulk, &NoMonitor).unwrap();
        let i = b.try_acquire(30, Class::Interactive).unwrap().unwrap();
        assert_eq!(b.used().unwrap(), 90);
        assert!(b.try_acquire(20, Class::Bulk).unwrap().is_none());
        assert!(b.try_acquire(20, Class::Interactive).unwrap().is_none());
        drop(l);
        assert_eq!(b.used().unwrap(), 30);
        drop(i);
        assert_eq!(b.used().unwrap(), 0);
        assert_eq!(
            b.usage().unwrap(),
            Usage {
                total: 100,
                interactive: 0,
                bulk: 0,
                reserved: 0,
                standing: 0,
                waiting: 0,
                parked: 0,
            }
        );
    }

    /// 対話は待たない：収まらなければ try は None、acquire は Limit。
    #[test]
    fn interactive_never_waits() {
        let b = MemoryBudget::new(100);
        let _bulk = b.acquire(80, Class::Bulk, &NoMonitor).unwrap();
        assert!(b.try_acquire(30, Class::Interactive).unwrap().is_none());
        assert!(matches!(
            b.acquire(30, Class::Interactive, &NoMonitor),
            Err(EngineError::Limit(_))
        ));
        let _i = b.acquire(20, Class::Interactive, &NoMonitor).unwrap();
        assert_eq!(b.used().unwrap(), 100);
    }

    /// 待っている間に中止すると Canceled で、使用量も待ちの列も変わらない。
    #[test]
    fn cancel_while_waiting() {
        let b = MemoryBudget::new(100);
        let _hold = b.acquire(100, Class::Bulk, &NoMonitor).unwrap();
        let flag = CancelFlag::default();
        #[allow(clippy::disallowed_methods)]
        let r = std::thread::scope(|sc| {
            let h = sc.spawn(|| b.acquire(10, Class::Bulk, &flag).map(|l| l.bytes()));
            wait_for_waiting(&b, 1);
            flag.cancel();
            h.join().unwrap()
        });
        assert!(matches!(r, Err(EngineError::Canceled)));
        assert_eq!(b.used().unwrap(), 100);
        assert_eq!(b.usage().unwrap().waiting, 0);
        // 先に中止を頼まれていれば、収まっても確保しない
        let b = MemoryBudget::new(100);
        let flag = CancelFlag::default();
        flag.cancel();
        assert!(matches!(
            b.acquire(1, Class::Bulk, &flag),
            Err(EngineError::Canceled)
        ));
        assert_eq!(b.used().unwrap(), 0);
    }

    /// 一括は空くまで待ち、来た順に通る。対話は待っている一括を追い越す。
    #[test]
    fn bulk_waits_in_order_and_interactive_goes_first() {
        let b = MemoryBudget::new(100);
        let hold = b.acquire(70, Class::Bulk, &NoMonitor).unwrap();
        let order = Mutex::new(Vec::new());
        let release = AtomicBool::new(false);
        #[allow(clippy::disallowed_methods)]
        std::thread::scope(|sc| {
            let first = sc.spawn(|| {
                let l = b.acquire(50, Class::Bulk, &NoMonitor).unwrap();
                order.lock().unwrap().push("bulk-50");
                while !release.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                drop(l);
            });
            wait_for_waiting(&b, 1);
            // 2 つめの一括（10）は今すぐなら収まるが、先に待っている 50 を追い越さない
            assert!(b.try_acquire(10, Class::Bulk).unwrap().is_none());
            let second = sc.spawn(|| {
                let l = b.acquire(40, Class::Bulk, &NoMonitor).unwrap();
                order.lock().unwrap().push("bulk-40");
                drop(l);
            });
            wait_for_waiting(&b, 2);
            // 対話は待っている一括を追い越す
            let i = b.try_acquire(20, Class::Interactive).unwrap().unwrap();
            order.lock().unwrap().push("interactive-20");
            // 対話が 20 を持っている間は、一括は 100 − 20 = 80 まで：70 を返すと 50 が通り、40 は 50 を返すまで待つ
            drop(hold);
            wait_for_waiting(&b, 1);
            release.store(true, Ordering::Release);
            first.join().unwrap();
            second.join().unwrap();
            drop(i);
        });
        assert_eq!(
            *order.lock().unwrap(),
            vec!["interactive-20", "bulk-50", "bulk-40"]
        );
        assert_eq!(b.used().unwrap(), 0);
    }

    /// 取り置きの間は、一括の上限が「予算 − max(取り置き, 対話の使用量)」に下がり、超える一括は取り置きが外れるまで待つ。
    #[test]
    fn reservation_lowers_the_bulk_cap() {
        let b = MemoryBudget::new(100);
        b.set_reserved(40).unwrap();
        assert!(b.try_acquire(61, Class::Bulk).unwrap().is_none());
        let bulk = b.try_acquire(60, Class::Bulk).unwrap().unwrap();
        // 取り置きの中なら、対話は一括に邪魔されない
        let i = b.try_acquire(40, Class::Interactive).unwrap().unwrap();
        drop(i);
        // 対話が取り置きより多く使っていれば、そちらが一括の上限を決める
        drop(bulk);
        let i = b.try_acquire(70, Class::Interactive).unwrap().unwrap();
        assert!(b.try_acquire(31, Class::Bulk).unwrap().is_none());
        let small = b.try_acquire(30, Class::Bulk).unwrap().unwrap();
        drop((i, small));
        // 予算 − 取り置きを超える一括は Limit ではなく待ち、取り置きを外すと通る
        #[allow(clippy::disallowed_methods)]
        std::thread::scope(|sc| {
            let h = sc.spawn(|| b.acquire(90, Class::Bulk, &NoMonitor).map(|l| l.bytes()));
            wait_for_waiting(&b, 1);
            b.set_reserved(0).unwrap();
            assert_eq!(h.join().unwrap().unwrap(), 90);
        });
        // 取り置きは全体に切り詰める
        b.set_reserved(1_000).unwrap();
        assert_eq!(b.usage().unwrap().reserved, 100);
    }

    /// 今の取り置きのもとでは通りえない一括（予算 − 取り置きを超えるもの）は先頭の判定から外れ（駐車）、後ろの収まる一括を
    /// 塞がない（エディタを開いたままの保存が、大きな書き出しの後ろで止まらない）。取り置きが下がると来た順の位置に戻り、
    /// 後から来た一括より先に通る。
    #[test]
    fn requests_over_the_reservation_park_and_do_not_block() {
        let b = MemoryBudget::new(100);
        b.set_reserved(40).unwrap();
        let order = Mutex::new(Vec::new());
        // 失敗したときに待ちっぱなしにならないよう、70 は中止できるようにしておく（中止したら Canceled が残る）
        let big = CancelFlag::default();
        #[allow(clippy::disallowed_methods)]
        let (parked, passed, r70) = std::thread::scope(|sc| {
            let h70 = sc.spawn(|| {
                let l = b.acquire(70, Class::Bulk, &big)?;
                order.lock().unwrap().push("bulk-70");
                Ok::<_, EngineError>(l.bytes())
            });
            wait_for_waiting(&b, 1);
            let parked = b.usage().unwrap().parked;
            // 収まる一括は、駐車した 70 を待たずに通る
            let passed = b.try_acquire(10, Class::Bulk).unwrap();
            if passed.is_none() {
                big.cancel();
                return (parked, false, h70.join().unwrap());
            }
            // 待つ確保も通る（塞がれていたら、約 1 秒で中止して見張る）
            let Ok(queued) = b.acquire(10, Class::Bulk, &CancelAfter::new(500)) else {
                big.cancel();
                return (parked, false, h70.join().unwrap());
            };
            assert_eq!(b.usage().unwrap().bulk, 20);
            drop((passed, queued));
            // 50 を持つ一括がいる間に取り置きを外すと、70 は先頭に戻る：後から来た 40 は、今すぐ収まっても 70 を追い越さない
            let hold = b.try_acquire(50, Class::Bulk).unwrap().unwrap();
            b.set_reserved(0).unwrap();
            assert_eq!(b.usage().unwrap().parked, 0);
            let h40 = sc.spawn(|| {
                let l = b.acquire(40, Class::Bulk, &NoMonitor).unwrap();
                order.lock().unwrap().push("bulk-40");
                drop(l);
            });
            wait_for_waiting(&b, 2);
            let overtook = b.try_acquire(10, Class::Bulk).unwrap();
            // 50 を返すと 70 が通り、40 は 70 を返すまで待つ（70 ＋ 40 は予算を超える）
            drop(hold);
            let r70 = h70.join().unwrap();
            h40.join().unwrap();
            assert!(overtook.is_none(), "先頭の 70 を追い越さない");
            (parked, true, r70)
        });
        assert!(passed, "駐車した要求（70）が、収まる一括（10）を塞いだ");
        assert_eq!(parked, 1);
        assert_eq!(r70.unwrap(), 70);
        assert_eq!(*order.lock().unwrap(), vec!["bulk-70", "bulk-40"]);
        assert_eq!(b.used().unwrap(), 0);
        // 駐車していても中止できる（待ちの列から外れる）
        b.set_reserved(40).unwrap();
        let flag = CancelFlag::default();
        #[allow(clippy::disallowed_methods)]
        let r = std::thread::scope(|sc| {
            let h = sc.spawn(|| b.acquire(90, Class::Bulk, &flag).map(|l| l.bytes()));
            wait_for_waiting(&b, 1);
            flag.cancel();
            h.join().unwrap()
        });
        assert!(matches!(r, Err(EngineError::Canceled)));
        let u = b.usage().unwrap();
        assert_eq!((u.waiting, u.parked, u.bulk), (0, 0, 0));
    }

    /// 段 10 の確認（予算の駐車）：持ち続ける対話（エディタのキャッシュの確保）が取り置きを超えていれば、「予算 − 持ち続ける
    /// 対話」を超える一括も駐車し、後ろの収まる一括を塞がない。描く間だけの対話（申告しない分）では駐車しない：その間は
    /// 来た順に待つ（すぐ返るので、大きな一括を追い越し続けない）。申告を下げれば来た順の位置に戻り、対話を返せば通る。
    #[test]
    fn standing_interactive_over_the_reservation_parks() {
        let b = MemoryBudget::new(100);
        b.set_reserved(40).unwrap();
        let mut cache = b.try_acquire(50, Class::Interactive).unwrap().unwrap();
        // 失敗したときに待ちっぱなしにならないよう、55 は中止できるようにしておく
        let big = CancelFlag::default();
        #[allow(clippy::disallowed_methods)]
        let (before, after, passed, queued, back, r55) = std::thread::scope(|sc| {
            // 55：予算 − 取り置き（60）以下、予算 − max(取り置き, 対話)（50）を超える
            let h55 = sc.spawn(|| b.acquire(55, Class::Bulk, &big).map(|l| l.bytes()));
            wait_for_waiting(&b, 1);
            let u = b.usage().unwrap();
            let before = (
                u.parked,
                u.standing,
                b.try_acquire(10, Class::Bulk).unwrap().is_some(),
            );
            // 50 を持ち続けると申告すると、55 は駐車し、10 は待たない確保でも待つ確保でも通る
            cache.set_standing(50).unwrap();
            let u = b.usage().unwrap();
            let after = (u.parked, u.standing);
            let passed = b.try_acquire(10, Class::Bulk).unwrap().map(|l| l.bytes());
            let queued = b
                .acquire(10, Class::Bulk, &CancelAfter::new(500))
                .ok()
                .map(|l| l.bytes());
            // 申告を下げると 55 は先頭に戻る：後から来た 10 は、今すぐ収まっても追い越さない
            cache.set_standing(0).unwrap();
            let back = (
                b.usage().unwrap().parked,
                b.try_acquire(10, Class::Bulk).unwrap().is_some(),
            );
            if back == (0, false) {
                // 対話を返すと 55 が通る
                drop(cache);
            } else {
                big.cancel();
            }
            (before, after, passed, queued, back, h55.join().unwrap())
        });
        assert_eq!(before, (0, 0, false), "描く間だけの対話では駐車しない");
        assert_eq!(after, (1, 50));
        assert_eq!(passed, Some(10), "待たない確保が、駐車しない 55 に塞がれた");
        assert_eq!(queued, Some(10), "待つ確保が、駐車しない 55 に塞がれた");
        assert_eq!(back, (0, false), "申告を下げたら 55 は先頭に戻る");
        assert_eq!(r55.unwrap(), 55);
        assert_eq!(b.used().unwrap(), 0);
    }

    /// 持ち続ける分は確保の中に収まり、縮めれば下がり、落とせば消える。一括の確保は申告できない。
    #[test]
    fn standing_follows_the_lease() {
        let b = MemoryBudget::new(100);
        let mut l = b.try_acquire(30, Class::Interactive).unwrap().unwrap();
        l.set_standing(80).unwrap();
        assert_eq!((l.standing(), b.usage().unwrap().standing), (30, 30));
        let mut other = b.try_acquire(20, Class::Interactive).unwrap().unwrap();
        other.set_standing(20).unwrap();
        assert_eq!(b.usage().unwrap().standing, 50);
        // 伸ばしても申告は変わらない（描く間だけの分）
        assert!(l.try_grow_to(60).unwrap());
        assert_eq!(b.usage().unwrap().standing, 50);
        l.shrink_to(10).unwrap();
        assert_eq!((l.standing(), b.usage().unwrap().standing), (10, 30));
        drop(other);
        assert_eq!(b.usage().unwrap().standing, 10);
        drop(l);
        assert_eq!(b.usage().unwrap().standing, 0);
        let mut bulk = b.try_acquire(10, Class::Bulk).unwrap().unwrap();
        assert!(matches!(
            bulk.set_standing(10),
            Err(EngineError::Internal(_))
        ));
        assert_eq!(b.usage().unwrap().standing, 0);
    }

    #[test]
    fn shrink_and_grow() {
        let b = MemoryBudget::new(100);
        let mut l = b.acquire(50, Class::Interactive, &NoMonitor).unwrap();
        l.shrink_to(20).unwrap();
        assert_eq!((l.bytes(), b.used().unwrap()), (20, 20));
        // 今より大きい shrink は何もしない
        l.shrink_to(30).unwrap();
        assert_eq!(l.bytes(), 20);
        assert!(l.try_grow_to(90).unwrap());
        assert_eq!(b.used().unwrap(), 90);
        // 今以下の grow は何もしない
        assert!(l.try_grow_to(10).unwrap());
        assert_eq!(l.bytes(), 90);
        let other = b.try_acquire(10, Class::Bulk).unwrap().unwrap();
        // 足りなければ false で、確保はそのまま
        assert!(!l.try_grow_to(91).unwrap());
        assert_eq!((l.bytes(), b.used().unwrap()), (90, 100));
        drop(other);
        assert!(l.try_grow_to(100).unwrap());
        drop(l);
        assert_eq!(b.used().unwrap(), 0);
        // 一括の grow も取り置きを守る
        b.set_reserved(40).unwrap();
        let mut bulk = b.try_acquire(50, Class::Bulk).unwrap().unwrap();
        assert!(!bulk.try_grow_to(61).unwrap());
        assert!(bulk.try_grow_to(60).unwrap());
        assert_eq!(bulk.class(), Class::Bulk);
    }

    /// 鍵が毒されたら Internal。毒されていても Lease は落とせる（返す）。
    #[test]
    fn a_poisoned_lock_is_internal() {
        let b = MemoryBudget::new(100);
        let l = b.acquire(10, Class::Bulk, &NoMonitor).unwrap();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _g = b.state.lock();
            panic!("鍵を持ったまま止まる");
        }));
        assert!(matches!(
            b.try_acquire(1, Class::Bulk),
            Err(EngineError::Internal(_))
        ));
        assert!(matches!(
            b.acquire(1, Class::Interactive, &NoMonitor),
            Err(EngineError::Internal(_))
        ));
        assert!(matches!(b.used(), Err(EngineError::Internal(_))));
        assert!(matches!(b.set_reserved(1), Err(EngineError::Internal(_))));
        drop(l);
        let s = b.state.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(s.used(), 0);
    }
}
