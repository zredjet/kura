//! エンジンの部品（実行の場とコーデックの組）を、開いているライブラリごとに 1 つ持つ（M11。docs/editing.md §12）。
//!
//! - 解析・エンジンのサムネイル（ジョブ）・試聴の PCM・アニメの帯（その場の読み取り）が、同じ実行の場（プレビューと一括の
//!   2 つのプールとメモリの予算。[`EngineRuntime`]）と、同じコーデックの組（[`CodecSet`]）を使う。
//! - どちらも最初に使うときに、使うスレッド（ジョブ・IPC の `spawn_blocking`）で作る。ライブラリを開くだけでは作らない
//!   （NFR-01 の起動の道に載せない。プールのスレッドと ffmpeg の問い合わせは、使うまで起こさない）。
//! - 作る関数（[`EngineFactory`]）はアプリが外から渡す。kura-core は OS を知らない（物理メモリ・ImageIO・AudioToolbox・
//!   システムのフォントは kura-mac）。テストは偽のファクトリで Linux でも回る。
//! - ライブラリごとに作る（ライブラリを切り替えたら別物）。コーデックの組の ffmpeg の一時ファイルの置き場は、そのライブラリの
//!   `staging/engine`（開くときの掃除で消える）。
//! - 外部ツールの場所を変えたら（`tools_set_path`）コーデックの組を作り直す（[`EngineServices::reload_codecs`]。ffmpeg を
//!   探し直す）。使っている途中の組（`Arc`）は使い終わるまで生きる。実行の場は作り直さない（予算を 2 つにしない。プールも
//!   ツールに依らない）。そのため、実行の場は `OnceLock`、コーデックの組は作り直せる置き場に持つ。
//! - 試聴の PCM の読んだ結果の置き場（[`PcmCache`]。覚えている間はこの実行の場の予算を持つ）もライブラリごとに持ち、閉じる
//!   （部品を捨てる）ときにすぐ手放す（M11 の検証 E2：アプリに 1 つだと、前のライブラリの音声と予算の確保を TTL まで持ち続け、
//!   新しいライブラリの予算はそれを数えなかった）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

pub use kura_engine::codec::CodecSet;

use super::editor::{Editor, Reservation};
use super::runtime::{EngineRuntime, RuntimeConfig};
use crate::Result;
use crate::asset::pcm::{CACHE_TTL, PcmCache};

/// ffmpeg の段の一時ファイルの置き場（ライブラリの `staging/` の下の名前）。
pub const ENGINE_WORK_DIR: &str = "engine";

/// エンジンの部品を作る口。アプリ（src-tauri・kura-devtools）が kura-mac で実装して渡す。どちらも最初に使うときに 1 回だけ
/// 呼ばれる（コーデックの組は、ツールの場所を変えたあとにもう一度）。
pub trait EngineFactory: Send + Sync {
    /// 実行の場の設定（物理メモリなど）。
    fn runtime_config(&self) -> RuntimeConfig;
    /// コーデックの組（固定の段 → Platform → ffmpeg）。`work_dir` は ffmpeg の一時ファイルの置き場。呼ぶたびに外部ツールを
    /// 探し直す（設定で指定した場所を含む）。
    fn codecs(&self, work_dir: &Path) -> CodecSet;
}

/// 開いているライブラリのエンジンの部品。[`crate::library::LibrarySession`] が開くときに作り、閉じると捨てる。
pub struct EngineServices {
    factory: Arc<dyn EngineFactory>,
    work_dir: PathBuf,
    runtime: OnceLock<Arc<EngineRuntime>>,
    /// 実行の場を作るのを 1 つにする（同時に使い始めても、プールを 2 回作らない）。
    runtime_init: Mutex<()>,
    /// コーデックの組（作り直せるように `OnceLock` ではなく置き場にする。作る間は錠を持ち、2 回作らない）。
    codecs: Mutex<Option<Arc<CodecSet>>>,
    /// 試聴の PCM の読んだ結果の置き場（このライブラリの分。見張りのスレッドは覚えたときだけ起きる）。
    pcm: Arc<PcmCache>,
    /// 開いているエディタ（ライブラリに 1 つ。M12。kura:// の `/edit` もここから引く）。
    editor: Mutex<Option<Arc<Editor>>>,
    /// エディタの取り置きの持ち主（エディタどうしで共有し、外すのは持ち主だけ）。
    reservation: Arc<Reservation>,
}

impl EngineServices {
    /// ファクトリと一時ファイルの置き場から作る（まだ何も作らない）。
    pub fn new(factory: Arc<dyn EngineFactory>, work_dir: PathBuf) -> Self {
        Self {
            factory,
            work_dir,
            runtime: OnceLock::new(),
            runtime_init: Mutex::new(()),
            codecs: Mutex::new(None),
            pcm: PcmCache::new(CACHE_TTL),
            editor: Mutex::new(None),
            reservation: Arc::new(Reservation::default()),
        }
    }

    /// エディタの取り置きの持ち主（エディタを作るときに渡す）。
    pub(crate) fn reservation(&self) -> Arc<Reservation> {
        self.reservation.clone()
    }

    fn editor_slot(&self) -> std::sync::MutexGuard<'_, Option<Arc<Editor>>> {
        self.editor.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 開いているエディタ（なければ None）。
    pub fn current_editor(&self) -> Option<Arc<Editor>> {
        self.editor_slot().clone()
    }

    /// セッション `session` のエディタ（今のエディタのセッションでなければ None）。
    pub fn editor_for(&self, session: &str) -> Option<Arc<Editor>> {
        self.editor_slot()
            .as_ref()
            .filter(|e| e.id() == session)
            .cloned()
    }

    /// エディタを置く（前のエディタがあれば閉じる。開く途中から置き、閉じる・ほかを開くと読みを止められるようにする）。
    pub(crate) fn install_editor(&self, editor: Arc<Editor>) {
        let old = self.editor_slot().replace(editor);
        if let Some(old) = old {
            old.shutdown();
        }
    }

    /// エディタを置き場から外す（`session` を渡せば、そのセッションのときだけ）。閉じるのは呼び手（`Editor::shutdown`）。
    pub(crate) fn take_editor(&self, session: Option<&str>) -> Option<Arc<Editor>> {
        let mut slot = self.editor_slot();
        match session {
            Some(id) if slot.as_ref().is_some_and(|e| e.id() != id) => None,
            _ => slot.take(),
        }
    }

    /// 開いているエディタを閉じる（エディタを閉じる・ライブラリを閉じる・切り替える・終了のどれでも同じ：中止 → キャッシュ
    /// を空に → 確保を落とす → 取り置きを 0。docs/editing.md §12.10）。
    pub fn close_editor(&self) {
        if let Some(editor) = self.take_editor(None) {
            editor.shutdown();
        }
    }

    /// 試聴の PCM の読んだ結果の置き場（このライブラリの分）。
    pub fn pcm_cache(&self) -> &Arc<PcmCache> {
        &self.pcm
    }

    /// ffmpeg の段の一時ファイルの置き場（そのライブラリの `staging/engine`）。
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// 実行の場（2 つのプールと予算）。最初に呼んだときに作る。編集のプールの中から呼ばない。
    pub fn runtime(&self) -> Result<&Arc<EngineRuntime>> {
        if let Some(runtime) = self.runtime.get() {
            return Ok(runtime);
        }
        let _init = self
            .runtime_init
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(runtime) = self.runtime.get() {
            return Ok(runtime);
        }
        let started = Instant::now();
        let runtime = Arc::new(EngineRuntime::new(&self.factory.runtime_config())?);
        tracing::info!(
            preview_threads = runtime.preview_threads(),
            bulk_threads = runtime.bulk_threads(),
            budget_bytes = runtime.budget().total(),
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "エンジンの実行の場を作りました"
        );
        Ok(self.runtime.get_or_init(|| runtime))
    }

    /// コーデックの組。最初に呼んだとき（とツールの場所を変えたあと）に作る。ffmpeg を探して問い合わせるので、ジョブか
    /// `spawn_blocking` のスレッドで呼ぶ。
    pub fn codecs(&self) -> Arc<CodecSet> {
        let mut slot = self.codecs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(set) = slot.as_ref() {
            return set.clone();
        }
        let started = Instant::now();
        let set = Arc::new(self.factory.codecs(&self.work_dir));
        tracing::info!(
            work_dir = %self.work_dir.display(),
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "エンジンのコーデックの組を作りました"
        );
        *slot = Some(set.clone());
        set
    }

    /// 外部ツールの場所が変わった：コーデックの組を捨て、次に使うときに作り直す（使っている途中の組はそのまま使える）。
    pub fn reload_codecs(&self) {
        let old = self
            .codecs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if old.is_some() {
            tracing::info!(
                "外部ツールの場所が変わったので、エンジンのコーデックの組を作り直します"
            );
        }
    }

    /// 実行の場を作ってあるか（作らずに見る。診断とテスト用）。
    pub fn runtime_if_built(&self) -> Option<&Arc<EngineRuntime>> {
        self.runtime.get()
    }

    /// コーデックの組を作ってあるか（作らずに見る。診断とテスト用）。
    pub fn codecs_if_built(&self) -> Option<Arc<CodecSet>> {
        self.codecs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for EngineServices {
    /// 閉じるとき：試聴の PCM で覚えていた音声と予算の確保をすぐ手放す（TTL の見張りは置き場への強い参照を持って待つので、
    /// 置き場が捨てられるのを待たずに空にする）。
    fn drop(&mut self) {
        self.close_editor();
        self.pcm.clear();
    }
}

/// テスト用の偽のファクトリ（Linux でも回る：固定の段だけの組と、小さな実行の場）。
#[cfg(test)]
pub(crate) mod fake {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// 呼ばれた回数と、渡された一時ファイルの置き場を覚える。コーデックの組は既定で固定の段だけ、[`FakeEngineFactory::with_codecs`]
    /// で作ったものは置き場の組の写し（テストが中身を入れ替え、`EngineServices::reload_codecs` で作り直させる）。
    #[derive(Default)]
    pub struct FakeEngineFactory {
        runtimes: AtomicUsize,
        work_dirs: Mutex<Vec<PathBuf>>,
        codecs: Option<Arc<Mutex<CodecSet>>>,
    }

    impl FakeEngineFactory {
        /// コーデックの組を `codecs` の写しにする（偽の Platform のデコーダを足した組など）。
        pub fn with_codecs(codecs: Arc<Mutex<CodecSet>>) -> Self {
            Self {
                codecs: Some(codecs),
                ..Self::default()
            }
        }

        /// 実行の場の設定を作った回数。
        pub fn runtimes(&self) -> usize {
            self.runtimes.load(Ordering::SeqCst)
        }

        /// コーデックの組を作った回数。
        pub fn codec_builds(&self) -> usize {
            self.work_dirs().len()
        }

        /// コーデックの組を作るときに渡された置き場（順に）。
        pub fn work_dirs(&self) -> Vec<PathBuf> {
            self.work_dirs.lock().unwrap().clone()
        }
    }

    impl EngineFactory for FakeEngineFactory {
        fn runtime_config(&self) -> RuntimeConfig {
            self.runtimes.fetch_add(1, Ordering::SeqCst);
            RuntimeConfig {
                cores: Some(2),
                budget: Some(64 << 20),
                ..RuntimeConfig::default()
            }
        }

        fn codecs(&self, work_dir: &Path) -> CodecSet {
            self.work_dirs.lock().unwrap().push(work_dir.to_path_buf());
            match &self.codecs {
                Some(set) => set.lock().unwrap().clone(),
                None => CodecSet::pinned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeEngineFactory;
    use super::*;

    fn services() -> (Arc<FakeEngineFactory>, EngineServices) {
        let factory = Arc::new(FakeEngineFactory::default());
        let services = EngineServices::new(factory.clone(), PathBuf::from("/lib/staging/engine"));
        (factory, services)
    }

    /// 作っただけでは何も作らない。最初に使うときに 1 回だけ作り、あとは同じものを返す。
    #[test]
    fn nothing_is_built_until_first_use() {
        let (factory, services) = services();
        assert_eq!((factory.runtimes(), factory.codec_builds()), (0, 0));
        assert!(services.runtime_if_built().is_none());
        assert!(services.codecs_if_built().is_none());

        let runtime = services.runtime().unwrap().clone();
        assert_eq!((factory.runtimes(), factory.codec_builds()), (1, 0));
        assert_eq!(runtime.bulk_threads() + runtime.preview_threads(), 2);
        assert_eq!(runtime.budget().total(), 64 << 20);
        let codecs = services.codecs();
        assert_eq!(factory.work_dirs(), [PathBuf::from("/lib/staging/engine")]);

        assert!(Arc::ptr_eq(services.runtime().unwrap(), &runtime));
        assert!(Arc::ptr_eq(&services.codecs(), &codecs));
        assert_eq!((factory.runtimes(), factory.codec_builds()), (1, 1));
    }

    /// ツールの場所を変えたら、コーデックの組だけを作り直す（実行の場はそのまま。使っていた組もそのまま使える）。
    #[test]
    fn reloading_rebuilds_codecs_and_keeps_the_runtime() {
        let (factory, services) = services();
        services.reload_codecs();
        assert_eq!(factory.codec_builds(), 0, "作っていなければ何もしない");
        let runtime = services.runtime().unwrap().clone();
        let before = services.codecs();

        services.reload_codecs();
        assert!(services.codecs_if_built().is_none());
        let after = services.codecs();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(factory.codec_builds(), 2);
        assert!(Arc::ptr_eq(services.runtime().unwrap(), &runtime));
        assert_eq!(factory.runtimes(), 1);
        assert!(!before.capabilities().is_empty(), "前の組も使える");
    }

    /// 同時に使い始めても、実行の場とコーデックの組は 1 回だけ作る。
    #[test]
    fn concurrent_first_use_builds_once() {
        let (factory, services) = services();
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    let _ = services.codecs();
                    services.runtime().unwrap();
                });
            }
        });
        assert_eq!((factory.runtimes(), factory.codec_builds()), (1, 1));
    }
}
