//! 働き手の確保の上限（子のメモリは NFR-17 の予算の外なので、確保の総量で抑える。ADR-164 (4)・docs/editing.md §9.5）。
//!
//! - 数える目印（[`AllocMeter`]）は、子の確保器が持つ（アプリと kura-devtools は kura-mac の `ReturningAllocator`、テストの
//!   働き手 `kura-svg-worker` は `System` を包んだもの）。確保器は確保の前に [`AllocMeter::charge`]、解放したとき・確保に
//!   失敗したときに [`AllocMeter::refund`] を呼ぶ。取り直しは、新しい大きさを数えてから古い大きさを返す（大きな確保の道は
//!   新しく取って写すので、一瞬は両方を持つ）。
//! - 数えるのは [`AllocMeter::start`] を呼んだプロセス（働き手）だけ。数えないプロセス（アプリの本体）では、確保ごとに旗を
//!   1 度読むだけ。数え始める前に確保したものの解放は数えた量を減らす（上限が少し緩む側。働き手は始めにすぐ数え始める）。
//! - 上限を超える確保は断る（確保器がヌルを返す）。落ちてよい確保（`Vec` の伸長など）なら Rust が「memory allocation of N
//!   bytes failed」と書いて abort し（親は `Decode`）、`try_reserve` なら呼び手のエラーになる（子は [`MEMORY_LIMIT`] の
//!   `Decode` を返す）。
//! - 上限：働き手は、頼みを読んだら前段の見積もりの分（[`precheck_allowance`]。入力の大きさに比例）を今の量に足した上限で
//!   読み始め、エンジンの見積もりの口（`svg::PlanHook`）で、木を作る前に木の分（[`tree_allowance`]。実体を展開した文字の数と、
//!   use・marker を展開した要素の数に比例）、値を読むときは描く前に描く分（[`render_allowance`]。キャンバスの 3 枚とエンジンの
//!   見積もりの一時の画像と、図形ごとに作って捨てるもの）を、そのときの量に足した上限にする。係数は、エンジンの見積もりを通る
//!   正しい重い形（地図風・16000² の値・要素の多い木・use の展開・点線）の実測（`kura-devtools bench svg-worker --memory`）に
//!   余裕を見た値。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use kura_engine::codec::svg::{RenderPlan, TreePlan};

/// 上限で確保を断ったときの `Decode` の言葉。
pub const MEMORY_LIMIT: &str = "SVG を読むのにメモリを使いすぎます";

/// 数える単位（libmalloc の最小の単位）。これに切り上げて数える（小さな確保をたくさんしても、数えた量が実際より大きく
/// 下回らないように）。
const QUANTUM: u64 = 16;

/// 確保の量を数え、上限を超える確保を断る目印（働き手の確保器が持つ）。
#[derive(Debug)]
pub struct AllocMeter {
    counting: AtomicBool,
    /// 数え始めてからの確保の総量（解放した分を引く）の上限。
    limit: AtomicU64,
    /// 数え始めてからの確保の総量。数え始める前の確保を解放すると負になりうる。
    used: AtomicI64,
    peak: AtomicI64,
    refused: AtomicU64,
}

impl Default for AllocMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// 数える量（[`QUANTUM`] に切り上げる）。
#[inline]
fn charged(size: usize) -> i64 {
    let size = u64::try_from(size).unwrap_or(u64::MAX);
    let q = size.saturating_add(QUANTUM - 1) & !(QUANTUM - 1);
    i64::try_from(q).unwrap_or(i64::MAX)
}

impl AllocMeter {
    /// 数えていない目印（確保器の `static` に置ける）。
    pub const fn new() -> Self {
        Self {
            counting: AtomicBool::new(false),
            limit: AtomicU64::new(u64::MAX),
            used: AtomicI64::new(0),
            peak: AtomicI64::new(0),
            refused: AtomicU64::new(0),
        }
    }

    /// 数え始め、上限を `limit`（数え始めてからの確保の総量）にする。一度始めたら止めない（数えずに確保したものの解放を
    /// 数えると、量がずれる）。
    pub fn start(&self, limit: u64) {
        self.limit.store(limit, Ordering::SeqCst);
        self.counting.store(true, Ordering::SeqCst);
    }

    /// 上限を変える（数え始めてからの確保の総量）。
    pub fn set_limit(&self, limit: u64) {
        self.limit.store(limit, Ordering::SeqCst);
    }

    /// 今の上限。
    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::SeqCst)
    }

    /// 数えているか。
    #[inline]
    pub fn is_counting(&self) -> bool {
        self.counting.load(Ordering::Relaxed)
    }

    /// 数え始めてからの確保の総量（負なら 0）。
    pub fn used(&self) -> u64 {
        u64::try_from(self.used.load(Ordering::SeqCst)).unwrap_or(0)
    }

    /// 数え始めてから（[`Self::reset_peak`] から）の総量のいちばん大きいもの。
    pub fn peak(&self) -> u64 {
        u64::try_from(self.peak.load(Ordering::SeqCst)).unwrap_or(0)
    }

    /// いちばん大きい量を今の量にする（測るとき）。
    pub fn reset_peak(&self) {
        self.peak
            .store(self.used.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// 上限で断った確保の数。
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::SeqCst)
    }

    /// 確保の前に呼ぶ（`size` バイトを数える）。数えていなければ何もせずに true。上限を超えるなら数えずに false（確保器は
    /// 確保を断ってヌルを返す）。確保ごとに呼ぶので、量は順序を付けずに足し引きする（どの量もそれぞれ原子的に変わる）。
    #[inline]
    pub fn charge(&self, size: usize) -> bool {
        if !self.is_counting() {
            return true;
        }
        let n = charged(size);
        let used = self.used.fetch_add(n, Ordering::Relaxed).saturating_add(n);
        let limit = i64::try_from(self.limit.load(Ordering::Relaxed)).unwrap_or(i64::MAX);
        if used > limit {
            self.used.fetch_sub(n, Ordering::Relaxed);
            self.refused.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.peak.fetch_max(used, Ordering::Relaxed);
        true
    }

    /// 解放したとき・確保に失敗したときに呼ぶ（`size` バイトを返す）。数えていなければ何もしない。
    #[inline]
    pub fn refund(&self, size: usize) {
        if self.is_counting() {
            self.used.fetch_sub(charged(size), Ordering::Relaxed);
        }
    }
}

/// 前段の見積もり・木を作る分の下駄（返事を書く区切り・フォントの集まり・文字のレイアウト・usvg の決まった分）。
const BASE: u64 = 64 << 20;

/// 文字 1 バイトあたりの確保（XML の木・文字列の写し・path の点・線の外形の外接矩形を求める一時の外形）。
const PER_BYTE: u64 = 32;

/// 要素 1 個あたりの確保（usvg の木の節・塗り・変形。use・marker の展開は参照ごとに作り直す）。
const PER_ELEMENT: u64 = 2 << 10;

/// 描く分の下駄（tiny-skia の辺の表・線の外形・破線・グラデーションの写しなど、図形ごとに作って捨てるもの）。
const RENDER_BASE: u64 = 128 << 20;

/// 描く分の、SVG 1 バイトあたりの確保（いちばん大きな図形の線の外形・辺の表は、図形の点の数に比例する）。
const RENDER_PER_BYTE: u64 = 64;

/// 頼みを読んだあと、前段の見積もり（実体・入れ子・要素の数）を通るまでに足してよい確保（`len` は SVG のバイト数）。
pub fn precheck_allowance(len: u64) -> u64 {
    BASE.saturating_add(len.saturating_mul(PER_BYTE))
}

/// 木を作る前（見積もりの口の `Plan::Tree`）に、木を作って描く前の見積もりを通るまでに足してよい確保。
pub fn tree_allowance(plan: &TreePlan) -> u64 {
    BASE.saturating_add(plan.text_bytes.saturating_mul(PER_BYTE))
        .saturating_add(plan.elements.saturating_mul(PER_ELEMENT))
}

/// 描く前（見積もりの口の `Plan::Render`）に、描いて値にするまでに足してよい確保（`len` は SVG のバイト数）：
/// キャンバス（tiny-skia の画像）・乗算を戻した行・値のタイルの 3 枚と、エンジンの見積もりの一時の画像と、図形ごとに作って
/// 捨てるもの。
pub fn render_allowance(plan: &RenderPlan, len: u64) -> u64 {
    let canvas = u64::from(plan.width)
        .saturating_mul(u64::from(plan.height))
        .saturating_mul(4);
    canvas
        .saturating_mul(3)
        .saturating_add(plan.scratch_bytes)
        .saturating_add(RENDER_BASE)
        .saturating_add(len.saturating_mul(RENDER_PER_BYTE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 数えていない目印は、何も数えず断らない（アプリの本体）。
    #[test]
    fn an_idle_meter_counts_nothing() {
        let m = AllocMeter::new();
        m.set_limit(0);
        assert!(m.charge(1 << 30));
        m.refund(1 << 30);
        assert!(!m.is_counting());
        assert_eq!((m.used(), m.peak(), m.refused()), (0, 0, 0));
    }

    /// 数え始めたら、確保の総量を 16 バイトに切り上げて数え、上限を超える確保は数えずに断る。返した分は減る。
    #[test]
    fn a_started_meter_refuses_what_exceeds_the_limit() {
        let m = AllocMeter::new();
        m.start(1_008);
        assert!(m.charge(1));
        assert_eq!(m.used(), 16);
        assert!(m.charge(984));
        assert_eq!(m.used(), 1_008);
        assert!(!m.charge(1));
        assert_eq!((m.used(), m.refused()), (1_008, 1));
        m.refund(984);
        assert_eq!(m.used(), 16);
        assert!(m.charge(500));
        assert_eq!((m.used(), m.peak()), (528, 1_008));
        // 上限を広げる・狭める
        m.set_limit(10_000);
        assert!(m.charge(9_000));
        m.set_limit(100);
        assert!(!m.charge(16));
        assert_eq!(m.refused(), 2);
        m.reset_peak();
        assert_eq!(m.peak(), m.used());
    }

    /// 数え始める前の確保の解放は量を負にしうる（上限が緩む側）。量は 0 より下に見せない。
    #[test]
    fn frees_from_before_the_start_only_loosen_the_limit() {
        let m = AllocMeter::new();
        m.start(32);
        m.refund(64);
        assert_eq!(m.used(), 0);
        assert!(m.charge(80));
        assert!(!m.charge(32));
    }

    /// 同時に数えても、量は合い、通した確保の総量は上限を超えない（競ると、ほかの確保が戻す前の量を見て断ることがある。
    /// 断る側に倒れるだけ）。
    #[test]
    fn concurrent_charges_never_exceed_the_limit() {
        let m = AllocMeter::new();
        m.start(16 * 1_000);
        let ok = AtomicU64::new(0);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..1_000 {
                        if m.charge(16) {
                            ok.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
            }
        });
        let ok = ok.load(Ordering::SeqCst);
        assert!((1..=1_000).contains(&ok), "{ok}");
        assert_eq!(m.used(), 16 * ok);
        assert_eq!(m.refused(), 8_000 - ok);
        assert!(m.peak() <= 16 * 1_000);
    }

    /// 上限の式：前段の分は入力に比例し、木の分は展開した文字と要素の数に比例し、描く分はキャンバスの 3 枚と一時の画像を足す。
    #[test]
    fn allowances_grow_with_the_input_the_elements_and_the_canvas() {
        assert_eq!(precheck_allowance(0), BASE);
        assert_eq!(precheck_allowance(1 << 20), BASE + (PER_BYTE << 20));
        assert_eq!(precheck_allowance(u64::MAX), u64::MAX);
        let tree = |text_bytes, elements| TreePlan {
            text_bytes,
            elements,
        };
        assert_eq!(tree_allowance(&tree(0, 0)), BASE);
        assert_eq!(
            tree_allowance(&tree(1_000, 300)),
            BASE + 1_000 * PER_BYTE + 300 * PER_ELEMENT
        );
        assert_eq!(tree_allowance(&tree(1, u64::MAX)), u64::MAX);
        let plan = RenderPlan {
            width: 16_000,
            height: 16_000,
            scratch_bytes: 7,
        };
        assert_eq!(
            render_allowance(&plan, 1_000),
            3 * 16_000 * 16_000 * 4 + 7 + RENDER_BASE + 1_000 * RENDER_PER_BYTE
        );
        let huge = RenderPlan {
            scratch_bytes: u64::MAX,
            ..plan
        };
        assert_eq!(render_allowance(&huge, 0), u64::MAX);
    }
}
