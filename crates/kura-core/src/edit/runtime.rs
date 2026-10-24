//! 編集のエンジンの実行の場（docs/editing.md §12）：エンジン 1 つ、プレビューと一括の 2 つのスレッドプール、メモリの予算。
//!
//! - エンジンはスレッドプールを作らない（§8.2。並列は `par_blocks` だけで、呼ばれたスレッドのプールで回る）。ここで作った
//!   プールの `install` の中で描く。`Engine` にはこの閉包（[`EngineRuntime::preview`]・[`EngineRuntime::bulk`]）の中から
//!   だけ触れるので、グローバルのプールに漏れない。値はプール・スレッドの数によらない（E1・NFR-16）。
//! - プレビュー（エディタ）と一括（保存・一括加工のジョブ）でプールを分ける。一括を流している間も、プレビューのワーカーは
//!   空いている（段 10 の調査：分けると p95 27.6 ms、相乗りすると 59 ms）。大きさはコアの数からの式（[`pool_sizes`]）。
//! - メモリの予算（`kura_engine::budget::MemoryBudget`）の確保は、閉包に入る前（プールの外。ジョブのスレッド・プレビューの
//!   輪のスレッド）で行う。ワーカーの中で待つと、ほかの描画のワーカーを塞ぐ（試作で 2.3〜3 倍遅い）。閉包はプールの中から
//!   入れない（debug のビルドで止める）。
//! - 物理メモリは設定の値で受け取る（kura-mac の `physical_memory`。kura-core は OS を知らない）。
//! - 確保の口：一括は [`EngineRuntime::admit_bulk`]（見積もって、全体を超えれば Limit、収まれば待つ）、エディタは
//!   [`EngineRuntime::open_cache`] でキャッシュの分を確保し、描くたびに [`EngineRuntime::admit_preview`]（待たない。足りなければ
//!   キャッシュを追い出す）。見積もりの並列の数は、描くプールのスレッドの数。エディタを開いている間の取り置き
//!   （`MemoryBudget::set_reserved`。既定は `budget::default_reserved`）は呼び手（M12 のエディタ）が決める。

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::budget::{Class, Lease, MemoryBudget, default_total};
use kura_engine::doc::InputId;
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::estimate::{Admission, CacheLease, EstimateSpec, MemoryEstimate};
use kura_engine::{Engine, EngineError, Limits, Monitor, Plan};
use rayon::ThreadPool;

use crate::{CoreError, Result};

/// コアの数が分からないときに使う数。
const FALLBACK_CORES: usize = 4;

/// プールの大きさの既定：(プレビュー, 一括) = (max(1, ⌊コア/4⌋), max(1, コア − プレビュー))。16 コアで (4, 12)、
/// 8 コアで (2, 6)、4 コアで (1, 3)。合計はコアの数を超えない（1 コアだけは (1, 1)）。
pub fn pool_sizes(cores: usize) -> (usize, usize) {
    let cores = cores.max(1);
    let preview = (cores / 4).max(1);
    (preview, (cores - preview).max(1))
}

/// [`EngineRuntime`] の設定。`None` は既定。
#[derive(Debug, Clone, Default)]
pub struct RuntimeConfig {
    /// 物理メモリのバイト数（kura-mac の `physical_memory`）。分からなければ `None`（予算は 2 GiB）。
    pub physical_memory: Option<u64>,
    /// 予算の全体のバイト数。既定は `default_total(physical_memory)`（min(物理/4, 4 GiB)）。
    pub budget: Option<u64>,
    /// コアの数。既定は `available_parallelism`（分からなければ 4）。
    pub cores: Option<usize>,
    /// プレビューのプールのスレッドの数。既定は [`pool_sizes`] の式。
    pub preview_threads: Option<usize>,
    /// 一括のプールのスレッドの数。既定は [`pool_sizes`] の式。
    pub bulk_threads: Option<usize>,
    /// エンジンの上限。既定は `Limits::default()`。
    pub limits: Option<Limits>,
}

/// エンジン 1 つと、プレビュー・一括の 2 つのプール、メモリの予算。開いているライブラリに 1 つ持つ（[`super::EngineServices`]。
/// 最初に使うときに作る）。ノードのキャッシュは作ったエンジンに縛られるので、ライブラリを開いている間は作り直さない。
pub struct EngineRuntime {
    engine: Engine,
    preview: ThreadPool,
    bulk: ThreadPool,
    budget: Arc<MemoryBudget>,
}

fn pool(threads: usize, name: &'static str) -> Result<ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .thread_name(move |i| format!("kura-edit-{name}-{i}"))
        .build()
        .map_err(|err| CoreError::Invalid(format!("編集の{name}のスレッドを作れません: {err}")))
}

impl EngineRuntime {
    /// 設定からエンジン・プール・予算を作る。
    pub fn new(cfg: &RuntimeConfig) -> Result<Self> {
        let cores = cfg.cores.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(FALLBACK_CORES)
        });
        let (preview, bulk) = pool_sizes(cores);
        let engine = Engine::with_limits(cfg.limits.clone().unwrap_or_default())
            .map_err(|err| CoreError::Invalid(format!("編集のエンジンを作れません: {err}")))?;
        Ok(Self {
            engine,
            preview: pool(cfg.preview_threads.unwrap_or(preview), "preview")?,
            bulk: pool(cfg.bulk_threads.unwrap_or(bulk), "bulk")?,
            budget: MemoryBudget::new(
                cfg.budget
                    .unwrap_or_else(|| default_total(cfg.physical_memory)),
            ),
        })
    }

    /// プレビュー（エディタ）のプールで `f` を回す。予算はこの前に確保しておく。プールの中から呼ばない。
    pub fn preview<R: Send>(&self, f: impl FnOnce(&Engine) -> R + Send) -> R {
        enter(&self.preview, &self.engine, f)
    }

    /// 一括（保存・一括加工）のプールで `f` を回す。予算はこの前に確保しておく。プールの中から呼ばない。
    pub fn bulk<R: Send>(&self, f: impl FnOnce(&Engine) -> R + Send) -> R {
        enter(&self.bulk, &self.engine, f)
    }

    /// メモリの予算（エディタの取り置き・一括の確保に使う）。
    pub fn budget(&self) -> &Arc<MemoryBudget> {
        &self.budget
    }

    /// プレビューのプールのスレッドの数。
    pub fn preview_threads(&self) -> usize {
        self.preview.current_num_threads()
    }

    /// 一括のプールのスレッドの数。
    pub fn bulk_threads(&self) -> usize {
        self.bulk.current_num_threads()
    }

    /// 一括の確保（§12）：計画を見積もり（並列の数は一括のプールのスレッドの数）、予算の全体を超えれば何も確保せずに
    /// Limit、収まれば確保できるまで待つ（`mon` の中止で Canceled）。プールの外で呼び、返したものを [`EngineRuntime::bulk`]
    /// で描き終える（書き終える）まで持つ。
    pub fn admit_bulk(
        &self,
        plan: &Plan,
        spec: EstimateSpec<'_>,
        mon: &dyn Monitor,
    ) -> std::result::Result<Admission, EngineError> {
        outside_the_pools("予算は、編集のプールの外で確保します");
        let spec = spec.tasks(self.bulk_threads() as u64);
        self.engine
            .admit(plan, &spec, &self.budget, Class::Bulk, mon)
    }

    /// 一括の確保に、呼び手の分を足す口（保存のジョブがアンカーを作るとき。docs/editing.md §9.4）：計画を見積もり（並列の数は
    /// 一括のプールのスレッドの数）、ピークと `extra(&見積もり)` の大きいほうを確保する。それが予算の全体を超えれば何も確保
    /// せずに Limit、収まれば確保できるまで待つ（`mon` の中止で Canceled）。プールの外で呼び、返した確保を描き終える（書き
    /// 終える）まで持つ。アンカーは読んだ直後・描く前に書いて読み直すので、描く段のピークとは重ならない（大きいほうでよい）。
    pub fn admit_bulk_with(
        &self,
        plan: &Plan,
        spec: EstimateSpec<'_>,
        extra: impl FnOnce(&MemoryEstimate) -> u64,
        mon: &dyn Monitor,
    ) -> std::result::Result<(MemoryEstimate, Lease), EngineError> {
        outside_the_pools("予算は、編集のプールの外で確保します");
        let spec = spec.tasks(self.bulk_threads() as u64);
        let estimate = self.engine.estimate(plan, &spec, None)?;
        let need = estimate.peak.max(extra(&estimate));
        let total = self.budget.total();
        if need > total {
            return Err(EngineError::Limit(format!(
                "メモリの予算を超える見込みです（見積もり {need} バイト。予算は {total} バイト）"
            )));
        }
        let lease = self.budget.acquire(need, Class::Bulk, mon)?;
        Ok((estimate, lease))
    }

    /// エディタを開くとき：キャッシュの上限を `want` にして、その分（とキャッシュが数える入力）を予算から確保する
    /// （待たない。空いていなければ空いている分まで上限を下げる。入力だけで収まらなければ Limit）。
    pub fn open_cache(
        &self,
        cache: &mut NodeCache,
        want: u64,
    ) -> std::result::Result<CacheLease, EngineError> {
        CacheLease::open(&self.budget, cache, want)
    }

    /// エディタの確保：キャッシュの当たりを引いて見積もり（並列の数はプレビューのプールのスレッドの数）、`held` を待たずに
    /// 伸ばす。足りなければキャッシュの固定していないものを追い出して確かめ直し、それでも足りなければ Limit。プールの外で
    /// 呼び、[`EngineRuntime::preview`] で描いたあとに `held.settle(cache)` で戻す。
    pub fn admit_preview(
        &self,
        plan: &Plan,
        spec: EstimateSpec<'_>,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        held: &mut CacheLease,
    ) -> std::result::Result<MemoryEstimate, EngineError> {
        outside_the_pools("予算は、編集のプールの外で確保します");
        let spec = spec.tasks(self.preview_threads() as u64);
        self.engine.admit_cached(plan, &spec, inputs, cache, held)
    }
}

/// プールのワーカーの中ではないこと（debug のビルドで止める）。ワーカーの中から別のプールに入ると、外のプールのワーカーを
/// 塞いだまま待つ。予算の確保（待つことがある）も、ほかの描画のワーカーを塞ぐ。
fn outside_the_pools(why: &str) {
    debug_assert!(rayon::current_thread_index().is_none(), "{why}");
}

fn enter<R: Send>(pool: &ThreadPool, engine: &Engine, f: impl FnOnce(&Engine) -> R + Send) -> R {
    outside_the_pools("編集のプールには、プールの外から入ります");
    pool.install(|| f(engine))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Mutex;

    use kura_engine::determinism::par_blocks;
    use kura_engine::monitor::Monitor;
    use kura_engine::testgen::InputGen;
    use kura_engine::testing::{render_with, single_node_graph};
    use kura_engine::value::PixelFormat;
    use kura_engine::{Digest, Graph, NoMonitor, Value};

    use super::*;

    const GIB: u64 = 1 << 30;

    /// 中止の確認を呼んだスレッドの名前を覚える（par_blocks はブロックの前に呼ぶ）。
    #[derive(Default)]
    struct Threads(Mutex<BTreeSet<String>>);

    impl Monitor for Threads {
        fn canceled(&self) -> bool {
            let name = std::thread::current()
                .name()
                .unwrap_or("名前なし")
                .to_string();
            self.0.lock().unwrap().insert(name);
            false
        }
    }

    impl Threads {
        fn all_start_with(&self, prefix: &str) -> bool {
            let names = self.0.lock().unwrap();
            !names.is_empty() && names.iter().all(|n| n.starts_with(prefix))
        }
    }

    fn runtime(preview: usize, bulk: usize) -> EngineRuntime {
        EngineRuntime::new(&RuntimeConfig {
            preview_threads: Some(preview),
            bulk_threads: Some(bulk),
            ..RuntimeConfig::default()
        })
        .unwrap()
    }

    fn doc() -> (Graph, Value) {
        let graph = single_node_graph(
            "image.resize",
            1,
            serde_json::json!({ "width": { "px": 333 }, "height": "auto", "filter": "lanczos3" }),
        )
        .unwrap();
        let input = InputGen::Noise {
            w: 700,
            h: 520,
            fmt: PixelFormat::Rgba8,
            seed: 11,
        }
        .generate()
        .unwrap();
        (graph, input)
    }

    fn render(e: &Engine, graph: &Graph, input: &Value, mon: &dyn Monitor) -> Digest {
        render_with(e, graph, input.clone(), Digest([3; 32]), mon)
            .unwrap()
            .digest()
            .unwrap()
    }

    #[test]
    fn pool_sizes_follow_the_cores() {
        assert_eq!(pool_sizes(16), (4, 12));
        assert_eq!(pool_sizes(12), (3, 9));
        assert_eq!(pool_sizes(8), (2, 6));
        assert_eq!(pool_sizes(4), (1, 3));
        assert_eq!(pool_sizes(2), (1, 1));
        assert_eq!(pool_sizes(1), (1, 1));
        assert_eq!(pool_sizes(0), (1, 1));
        for cores in 2..=64 {
            let (p, b) = pool_sizes(cores);
            assert!(p >= 1 && b >= 1 && p + b <= cores, "{cores}");
        }
        let rt = EngineRuntime::new(&RuntimeConfig {
            cores: Some(8),
            ..RuntimeConfig::default()
        })
        .unwrap();
        assert_eq!((rt.preview_threads(), rt.bulk_threads()), (2, 6));
    }

    #[test]
    fn the_budget_follows_the_config() {
        let rt = EngineRuntime::new(&RuntimeConfig {
            physical_memory: Some(8 * GIB),
            cores: Some(2),
            ..RuntimeConfig::default()
        })
        .unwrap();
        assert_eq!(rt.budget().total(), 2 * GIB);
        let rt = EngineRuntime::new(&RuntimeConfig {
            physical_memory: Some(128 * GIB),
            cores: Some(2),
            ..RuntimeConfig::default()
        })
        .unwrap();
        assert_eq!(rt.budget().total(), 4 * GIB);
        let rt = EngineRuntime::new(&RuntimeConfig {
            budget: Some(123),
            cores: Some(2),
            ..RuntimeConfig::default()
        })
        .unwrap();
        assert_eq!(rt.budget().total(), 123);
        assert_eq!(runtime(1, 1).budget().total(), 2 * GIB);
    }

    /// 閉包の中の par_blocks と描画は、そのプールのスレッドで回る（グローバルのプールに漏れない）。
    #[test]
    fn closures_run_on_their_own_pool() {
        let rt = runtime(3, 5);
        let (graph, input) = doc();
        for (name, threads) in [("preview", 3), ("bulk", 5)] {
            let run = |f: &(dyn Fn(&Engine) -> (usize, Digest) + Sync)| match name {
                "preview" => rt.preview(f),
                _ => rt.bulk(f),
            };
            let blocks = Threads::default();
            let drawn = Threads::default();
            let (n, _) = run(&|e| {
                let n = par_blocks(256, &blocks, Ok).unwrap().len();
                assert_eq!(rayon::current_num_threads(), threads);
                (n, render(e, &graph, &input, &drawn))
            });
            assert_eq!(n, 256);
            let prefix = format!("kura-edit-{name}-");
            assert!(blocks.all_start_with(&prefix), "{:?}", blocks.0);
            assert!(drawn.all_start_with(&prefix), "{:?}", drawn.0);
        }
    }

    /// 2 つのプールで同時に描いても、1 スレッドで描いたのと同じ値（NFR-16）。
    #[test]
    fn two_pools_render_the_same_value_at_once() {
        let rt = runtime(4, 12);
        let (graph, input) = doc();
        let one = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let want = one.install(|| render(&Engine::new().unwrap(), &graph, &input, &NoMonitor));
        let (a, b) = std::thread::scope(|sc| {
            let a = sc.spawn(|| {
                (0..3)
                    .map(|_| rt.preview(|e| render(e, &graph, &input, &NoMonitor)))
                    .collect::<Vec<_>>()
            });
            let b = sc.spawn(|| {
                (0..3)
                    .map(|_| rt.bulk(|e| render(e, &graph, &input, &NoMonitor)))
                    .collect::<Vec<_>>()
            });
            (a.join().unwrap(), b.join().unwrap())
        });
        assert!(a.iter().chain(&b).all(|d| *d == want));
    }

    /// プールの中から入ると debug のビルドで止まる（予算もプールの外で確保する決まり）。
    #[test]
    fn entering_from_inside_a_pool_is_caught() {
        let rt = runtime(1, 1);
        let r = catch_unwind(AssertUnwindSafe(|| rt.bulk(|_| rt.preview(|_| 1))));
        if cfg!(debug_assertions) {
            assert!(r.is_err());
        } else {
            assert_eq!(r.ok(), Some(1));
        }
        // 外からなら入れる
        assert_eq!(rt.preview(|_| 2), 2);
    }

    const MIB: u64 = 1 << 20;

    fn compiled(rt: &EngineRuntime, graph: &Graph, input: &Value) -> Plan {
        let shapes = BTreeMap::from([(InputId(0), input.shape())]);
        rt.bulk(|e| e.compile(graph, &shapes)).unwrap()
    }

    /// 予算はプールの外で確保して、閉包の中で描く（§12）。一括は見積もり（一括のプールのスレッドの数で）の分を確保してから
    /// 一括のプールで描き、落とすと返る。エディタはキャッシュの分の確保を見積もりの分だけ伸ばしてプレビューのプールで描き、
    /// 描いたあとに戻す。値はどれも 1 スレッドで描いたものと同じ。
    #[test]
    fn budgets_are_acquired_outside_the_pools() {
        use kura_engine::render::InputValue;
        use kura_engine::render::cache::RenderOptions;
        use kura_engine::render::estimate::Source;

        let rt = EngineRuntime::new(&RuntimeConfig {
            preview_threads: Some(2),
            bulk_threads: Some(3),
            budget: Some(64 * MIB),
            ..RuntimeConfig::default()
        })
        .unwrap();
        let (graph, input) = doc();
        let plan = compiled(&rt, &graph, &input);
        let want = render(&Engine::new().unwrap(), &graph, &input, &NoMonitor);
        let source = Digest([3; 32]);
        // 一括
        let admission = rt
            .admit_bulk(&plan, EstimateSpec::new(0).digests(true), &NoMonitor)
            .unwrap();
        let in_pool = rt
            .bulk(|e| e.estimate(&plan, &EstimateSpec::new(3).digests(true), None))
            .unwrap();
        assert_eq!(admission.estimate, in_pool, "並列の数は一括のプールの数");
        assert!(admission.estimate.nodes[0].work > 0, "平滑なリサイズ");
        assert_eq!(rt.budget().used().unwrap(), admission.estimate.peak);
        let inputs = BTreeMap::from([(
            InputId(0),
            InputValue {
                value: input.clone(),
                source,
            },
        )]);
        let got = rt.bulk(|e| e.render(&plan, &inputs, &NoMonitor)).unwrap();
        assert_eq!(got.outputs[0].digest, want);
        drop(admission);
        assert_eq!(rt.budget().used().unwrap(), 0);
        // エディタ：入力はキャッシュの外の値として数え、キャッシュの分を確保して開く
        let fins = BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(input.clone(), source).unwrap(),
        )]);
        let mut cache = NodeCache::new(0);
        cache.register_external(&input);
        let mut held = rt.open_cache(&mut cache, 16 * MIB).unwrap();
        assert_eq!(held.bytes(), 16 * MIB);
        let spec = || EstimateSpec::new(0).all_inputs(&plan, Source::Held);
        let est = rt
            .admit_preview(&plan, spec(), &fins, &mut cache, &mut held)
            .unwrap();
        assert!(est.peak > 0);
        assert_eq!(held.bytes(), 16 * MIB + est.peak);
        let opts = RenderOptions {
            digests: true,
            ..RenderOptions::default()
        };
        let r = rt
            .preview(|e| e.render_cached(&plan, &fins, &mut cache, &opts, &NoMonitor))
            .unwrap();
        assert_eq!(r.outputs[0].digest, Some(want));
        held.settle(&mut cache).unwrap();
        assert_eq!(held.bytes(), 16 * MIB);
        assert_eq!(rt.budget().used().unwrap(), 16 * MIB);
        // 温まったキャッシュでは描くものがないので、伸ばさない
        let est = rt
            .admit_preview(&plan, spec(), &fins, &mut cache, &mut held)
            .unwrap();
        assert_eq!(est.peak, 0);
        drop(held);
        assert_eq!(rt.budget().used().unwrap(), 0);
    }

    /// 予算の全体を超える計画は、閉包に入る前（見積もりの段階）に Limit で、描くプールに入らない。中止の確かめも呼ばない。
    #[test]
    fn over_budget_plans_never_enter_the_pool() {
        use kura_engine::monitor::CancelAfter;
        use std::sync::atomic::{AtomicBool, Ordering};

        let rt = EngineRuntime::new(&RuntimeConfig {
            budget: Some(MIB),
            cores: Some(2),
            ..RuntimeConfig::default()
        })
        .unwrap();
        let (graph, input) = doc();
        let plan = compiled(&rt, &graph, &input);
        let entered = AtomicBool::new(false);
        let mon = CancelAfter::new(0);
        let r = rt
            .admit_bulk(&plan, EstimateSpec::new(0), &mon)
            .map(|admission| {
                rt.bulk(|_| entered.store(true, Ordering::Relaxed));
                admission.estimate.peak
            });
        assert!(matches!(r, Err(EngineError::Limit(_))), "{r:?}");
        assert!(!entered.load(Ordering::Relaxed));
        assert_eq!(mon.calls(), 0);
        assert_eq!(rt.budget().used().unwrap(), 0);
    }

    /// 呼び手の分を足す確保：ピークと呼び手の分の大きいほうを確保し、全体を超えれば何も確保せずに Limit、中止は Canceled。
    #[test]
    fn admit_bulk_with_takes_the_larger_of_peak_and_extra() {
        use kura_engine::monitor::CancelFlag;

        let rt = EngineRuntime::new(&RuntimeConfig {
            budget: Some(64 * MIB),
            bulk_threads: Some(2),
            preview_threads: Some(1),
            ..RuntimeConfig::default()
        })
        .unwrap();
        let (graph, input) = doc();
        let plan = compiled(&rt, &graph, &input);
        let (est, lease) = rt
            .admit_bulk_with(&plan, EstimateSpec::new(0), |_| 0, &NoMonitor)
            .unwrap();
        assert_eq!(lease.bytes(), est.peak);
        assert_eq!(rt.budget().used().unwrap(), est.peak);
        drop(lease);
        let (est, lease) = rt
            .admit_bulk_with(&plan, EstimateSpec::new(0), |e| e.peak + MIB, &NoMonitor)
            .unwrap();
        assert_eq!(lease.bytes(), est.peak + MIB);
        drop(lease);
        let r = rt.admit_bulk_with(&plan, EstimateSpec::new(0), |_| 65 * MIB, &NoMonitor);
        assert!(matches!(r, Err(EngineError::Limit(_))), "{:?}", r.err());
        assert_eq!(rt.budget().used().unwrap(), 0);
        // 埋まっていれば待ち、中止で Canceled
        let _hold = rt
            .budget()
            .acquire(60 * MIB, Class::Bulk, &NoMonitor)
            .unwrap();
        let flag = CancelFlag::default();
        flag.cancel();
        let r = rt.admit_bulk_with(&plan, EstimateSpec::new(0), |_| 10 * MIB, &flag);
        assert!(matches!(r, Err(EngineError::Canceled)), "{:?}", r.err());
    }

    /// 予算の確保をプールの中からすると、debug のビルドで止まる（ほかの描画のワーカーを塞いで待たない）。
    #[test]
    fn admitting_from_inside_a_pool_is_caught() {
        let rt = runtime(1, 1);
        let (graph, input) = doc();
        let plan = compiled(&rt, &graph, &input);
        let r = catch_unwind(AssertUnwindSafe(|| {
            rt.bulk(|_| {
                rt.admit_bulk(&plan, EstimateSpec::new(0), &NoMonitor)
                    .map(|a| a.lease.bytes())
            })
        }));
        if cfg!(debug_assertions) {
            assert!(r.is_err());
        } else {
            assert!(r.unwrap().is_ok());
        }
        assert_eq!(rt.budget().used().unwrap(), 0);
        // 外からなら確保できる
        assert!(
            rt.admit_bulk(&plan, EstimateSpec::new(0), &NoMonitor)
                .is_ok()
        );
    }
}
