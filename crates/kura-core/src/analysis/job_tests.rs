//! 解析のジョブのテスト（plan.md の tests 11）。固定の段のコーデックと小さな実行の場で、Linux でも回る。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use kura_engine::analysis::AnalysisOutput;
use kura_engine::analysis::audio::peaks::Peaks;
use kura_engine::budget::Class;
use kura_engine::codec::{CodecSet, Decoder, ReadSeek, Tier};
use kura_engine::doc::DecodeOptions;
use kura_engine::testgen::InputGen;
use kura_engine::testing::platform::FakePlatformDecoder;
use kura_engine::value::{LoopRange, PixelFormat};
use kura_engine::{EngineError, Limits, Monitor, NoMonitor, Shape, Value};
use rusqlite::params;
use serde_json::json;

use super::facts::FACTS_V;
use super::plan::{PLAN_KEY, PlanFingerprint, READ_ID};
use super::store::{self, INFLIGHT_KEY, Inflight};
use super::{AnalyzeHandler, AnalyzeShared};
use crate::edit::{EngineFactory, EngineServices, RuntimeConfig};
use crate::jobs::{EventSink, JobFilter, JobHandler, JobKind, JobManager, JobRecord, JobState};
use crate::library::{Library, hex};

const WAIT: Duration = Duration::from_secs(30);

type Hook = Arc<dyn Fn(&str) + Send + Sync>;

/// 読んだファイル（中身のハッシュの 16 進）を順に記録し、読む前にフックを呼ぶデコーダ（中身は包んだデコーダで読む）。
struct Watching {
    inner: Arc<dyn Decoder>,
    log: Arc<Mutex<Vec<String>>>,
    hook: Option<Hook>,
}

impl Decoder for Watching {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn tier(&self) -> Tier {
        self.inner.tier()
    }
    fn reproducible(&self) -> bool {
        self.inner.reproducible()
    }
    fn env(&self) -> Option<kura_engine::codec::CodecEnv> {
        self.inner.env()
    }
    fn can_decode(&self, format: &str) -> bool {
        self.inner.can_decode(format)
    }
    fn work_bytes(&self, format: &str, shape: &Shape) -> u64 {
        self.inner.work_bytes(format, shape)
    }
    fn file_work_bytes(&self, format: &str, shape: &Shape, len: u64) -> u64 {
        self.inner.file_work_bytes(format, shape, len)
    }
    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.inner.shape(src, format, opts, limits)
    }
    fn shape_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.log.lock().unwrap().push(format!("shape:{name}"));
        self.inner.shape_file(path, format, opts, limits)
    }
    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        self.inner.decode(src, format, opts, limits, mon)
    }
    fn decode_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.log.lock().unwrap().push(name.clone());
        if let Some(hook) = &self.hook {
            hook(&name);
        }
        self.inner.decode_file(path, format, opts, limits, mon)
    }
}

/// 決まったコーデックの組と予算を返すファクトリ。
struct TestFactory {
    codecs: CodecSet,
    budget: u64,
}

impl EngineFactory for TestFactory {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            cores: Some(2),
            budget: Some(self.budget),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, _work_dir: &Path) -> CodecSet {
        self.codecs.clone()
    }
}

struct Env {
    _dir: tempfile::TempDir,
    library: Arc<Library>,
    engine: Arc<EngineServices>,
    shared: Arc<AnalyzeShared>,
    jobs: Arc<JobManager>,
    log: Arc<Mutex<Vec<String>>>,
}

/// 固定の段の png・wav のデコーダを、読んだものを記録する形で包んだ組（`extra` を足す）。
fn watched_set(
    log: &Arc<Mutex<Vec<String>>>,
    hook: Option<Hook>,
    extra: Vec<Arc<dyn Decoder>>,
) -> CodecSet {
    let pinned = CodecSet::pinned();
    let mut set = CodecSet::default();
    for format in ["png", "wav"] {
        for d in pinned.decoders_for(format) {
            set.add_decoder(Arc::new(Watching {
                inner: d.clone(),
                log: log.clone(),
                hook: hook.clone(),
            }))
            .unwrap();
        }
    }
    for d in extra {
        set.add_decoder(Arc::new(Watching {
            inner: d,
            log: log.clone(),
            hook: hook.clone(),
        }))
        .unwrap();
    }
    set
}

impl Env {
    fn new() -> Self {
        Self::with(|log, _| watched_set(log, None, Vec::new()), 64 << 20)
    }

    /// コーデックの組を `make(記録, ライブラリ)` で作る。
    fn with(
        make: impl FnOnce(&Arc<Mutex<Vec<String>>>, &Arc<Library>) -> CodecSet,
        budget: u64,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::create(&dir.path().join("L.kuralib")).unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let codecs = make(&log, &library);
        let engine = Arc::new(EngineServices::new(
            Arc::new(TestFactory { codecs, budget }),
            library.layout().staging_dir().join("engine"),
        ));
        let shared = Arc::new(AnalyzeShared::default());
        let sink: EventSink = Arc::new(|_| {});
        let jobs = Arc::new(
            JobManager::new(
                library.clone(),
                vec![(
                    JobKind::Analyze,
                    Arc::new(AnalyzeHandler::new(engine.clone(), shared.clone()))
                        as Arc<dyn JobHandler>,
                )],
                sink,
            )
            .unwrap(),
        );
        jobs.start().unwrap();
        Self {
            _dir: dir,
            library,
            engine,
            shared,
            jobs,
            log,
        }
    }

    /// 中身 `body` のアイテムを入れる（ハッシュは中身から。新しい順は ID の大きい順）。中身のハッシュの 16 進を返す。
    fn add(&self, id: i64, kind: &str, ext: &str, body: &[u8]) -> String {
        let hash = *blake3::hash(body).as_bytes();
        let path = self.library.layout().content_path(&hash, ext);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        let (kind, ext, size) = (kind.to_string(), ext.to_string(), body.len() as i64);
        self.library
            .db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at,
                                        updated_at, analysis_state, proxy_state)
                     VALUES (?1, ?2, ?3, ?4, ?5, 't', 't', ?1, ?1, 'pending', 'not_needed')",
                    params![id, hash.to_vec(), ext, kind, size],
                )?;
                Ok(())
            })
            .unwrap();
        hex(&hash)
    }

    /// 解析のジョブを積んで、終わるまで待つ。最後の解析のジョブを返す。
    fn run(&self) -> JobRecord {
        self.jobs.ensure_queued(JobKind::Analyze, "{}").unwrap();
        assert!(self.jobs.wait_idle(WAIT));
        self.last()
    }

    fn last(&self) -> JobRecord {
        self.jobs
            .list(&JobFilter {
                kinds: vec![JobKind::Analyze],
                states: vec![],
                limit: Some(1),
            })
            .unwrap()
            .remove(0)
    }

    fn state(&self, id: i64) -> String {
        self.query(&format!("SELECT analysis_state FROM items WHERE id = {id}"))
    }

    fn query<T: rusqlite::types::FromSql + Send + 'static>(&self, sql: &str) -> T {
        let sql = sql.to_string();
        self.library
            .db()
            .read(move |c| Ok(c.query_row(&sql, [], |r| r.get(0))?))
            .unwrap()
    }

    /// 中身 `hash_hex` の事実の列 `col`。
    fn fact<T: rusqlite::types::FromSql + Send + 'static>(&self, hash_hex: &str, col: &str) -> T {
        self.query(&format!(
            "SELECT f.{col} FROM media_facts f JOIN blobs b ON b.id = f.blob_id WHERE hex(b.hash) = upper('{hash_hex}')"
        ))
    }

    /// 中身を読んだもの（順に）。
    fn reads(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|n| !n.starts_with("shape:"))
            .cloned()
            .collect()
    }

    /// ヘッダを読もうとした回数（中身を読む前に失敗したものも数える）。
    fn tries(&self) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.starts_with("shape:"))
            .count()
    }

    fn exec(&self, sql: &str) {
        let sql = sql.to_string();
        self.library
            .db()
            .write(move |tx| {
                tx.execute_batch(&sql)?;
                Ok(())
            })
            .unwrap();
    }
}

fn png(input: InputGen) -> Vec<u8> {
    let v = input.generate().unwrap();
    CodecSet::pinned()
        .encode_bytes(&v, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0
}

fn wav(frames: u64, loop_range: Option<LoopRange>) -> Vec<u8> {
    let v = InputGen::Sine {
        rate: 8_000,
        channels: 1,
        frames,
        freq: 440,
        loop_range,
    }
    .generate()
    .unwrap();
    CodecSet::pinned()
        .encode_bytes(&v, "wav", &json!({}), &NoMonitor)
        .unwrap()
        .0
}

fn sprite(seed: u64) -> Vec<u8> {
    png(InputGen::PixelSprite {
        w: 64,
        h: 64,
        fmt: PixelFormat::Rgba8,
        seed,
    })
}

fn result_json(record: &JobRecord) -> serde_json::Value {
    serde_json::from_str(record.result_json.as_deref().unwrap_or("{}")).unwrap()
}

/// 新しい順に読み、画像・音声の事実と結果を書いて ready にする。JPEG は読まずに形式から、動画は not_needed。
#[test]
fn analyzes_newest_first_and_marks_ready() {
    let env = Env::new();
    let old = env.add(1, "image", "png", &sprite(7));
    let song = env.add(
        2,
        "audio",
        "wav",
        &wav(
            8_000,
            Some(LoopRange {
                start: 100,
                end: 7_000,
            }),
        ),
    );
    let noise = env.add(
        3,
        "image",
        "png",
        &png(InputGen::Noise {
            w: 32,
            h: 24,
            fmt: PixelFormat::Rgba8,
            seed: 3,
        }),
    );
    let photo = env.add(4, "image", "jpg", b"not really a jpeg");
    env.add(5, "video", "mp4", b"movie");
    let record = env.run();
    assert_eq!(record.state, JobState::Done);
    assert_eq!(
        env.reads(),
        [noise.clone(), song.clone(), old.clone()],
        "新しい順"
    );
    for id in 1..=4 {
        assert_eq!(env.state(id), "ready", "{id}");
    }
    assert_eq!(env.state(5), "not_needed");

    // ドット絵のスプライト：二値の透過・16 色以下・等倍でドット絵
    assert_eq!(env.fact::<i64>(&old, "alpha"), 1);
    assert_eq!(env.fact::<i64>(&old, "pixel_art"), 1);
    assert_eq!(env.fact::<i64>(&old, "frame_count"), 1);
    assert_eq!(env.fact::<i64>(&old, "facts_v"), FACTS_V);
    assert!(
        env.fact::<String>(&old, "decoder")
            .starts_with("png-rs-dec/")
    );
    // 乱数の画素：半透明あり・257 色以上でドット絵でない
    assert_eq!(env.fact::<i64>(&noise, "alpha"), 2);
    assert_eq!(env.fact::<i64>(&noise, "colors"), 257);
    assert_eq!(env.fact::<i64>(&noise, "pixel_art"), 0);
    // 音声：ループ点・レート・深さ・波形の段
    assert_eq!(env.fact::<i64>(&song, "has_loop"), 1);
    assert_eq!(
        (
            env.fact::<i64>(&song, "loop_start"),
            env.fact::<i64>(&song, "loop_end")
        ),
        (100, 7_000)
    );
    assert_eq!(env.fact::<i64>(&song, "sample_rate"), 8_000);
    assert_eq!(env.fact::<i64>(&song, "channels"), 1);
    assert!(env.fact::<Option<f64>>(&song, "loudness").is_some());
    assert!(env.library.layout().peaks_path(&song).is_file());
    // JPEG：読まずに形式から（透過なし・色数は計らない・ドット絵でない・デコーダなし）
    assert_eq!(env.fact::<i64>(&photo, "alpha"), 0);
    assert_eq!(env.fact::<Option<i64>>(&photo, "colors"), None);
    assert_eq!(env.fact::<i64>(&photo, "pixel_art"), 0);
    assert_eq!(env.fact::<Option<String>>(&photo, "decoder"), None);
    // 結果の表：読み取りの記録・段 1・段 2（スプライトは透過があるので格子の推定も）
    let analyses: String = env.query(&format!(
        "SELECT group_concat(r.analysis, ',') FROM (SELECT r.analysis FROM analysis_results r JOIN blobs b
           ON b.id = r.blob_id WHERE hex(b.hash) = upper('{old}') ORDER BY r.analysis) r"
    ));
    assert_eq!(
        analyses,
        "image.alpha,image.colors,image.pixel_scale,image.sprite_grid,kura.read"
    );
    assert_eq!(
        env.query::<i64>("SELECT count(*) FROM media_facts"),
        4,
        "動画には事実を書かない"
    );
    let summary = result_json(&record);
    assert_eq!(
        (summary["done"].as_u64(), summary["reused"].as_u64()),
        (Some(5), Some(0))
    );
    assert_eq!(
        env.query::<i64>("SELECT count(*) FROM settings WHERE key = 'analysis.inflight'"),
        0
    );
}

/// 優先の指定（インスペクタで選んだもの）を先に解く。
#[test]
fn prioritized_items_go_first() {
    let env = Env::new();
    let a = env.add(1, "image", "png", &sprite(1));
    let b = env.add(2, "image", "png", &sprite(2));
    let c = env.add(3, "image", "png", &sprite(3));
    env.shared.prioritize(vec![1, 99]);
    env.run();
    assert_eq!(env.reads(), [a, c, b]);
}

/// 中止したら、解いていた 1 件と残りは pending のまま（書きためた分は書く）。書く前に落ちても（書き込みの失敗）pending のまま。
#[test]
fn cancel_or_crash_leaves_items_pending() {
    let manager: Arc<OnceLock<Arc<JobManager>>> = Arc::new(OnceLock::new());
    let slot = manager.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let r = reads.clone();
    let hook: Hook = Arc::new(move |_| {
        // 2 件目を読んでいる途中で中止を頼む
        if r.fetch_add(1, Ordering::SeqCst) == 1
            && let Some(jobs) = slot.get()
        {
            let running = jobs
                .list(&JobFilter {
                    kinds: vec![JobKind::Analyze],
                    states: vec![JobState::Running],
                    limit: Some(1),
                })
                .unwrap();
            jobs.cancel(running[0].id).unwrap();
        }
    });
    let env = Env::with(|log, _| watched_set(log, Some(hook), Vec::new()), 64 << 20);
    let _ = manager.set(env.jobs.clone());
    for id in 1..=3 {
        env.add(id, "image", "png", &sprite(id as u64));
    }
    let record = env.run();
    assert_eq!(record.state, JobState::Canceled);
    assert_eq!(env.state(3), "ready", "中止の前に解いたものは書く");
    assert_eq!(env.state(2), "pending", "解いていたものは捨てる");
    assert_eq!(env.state(1), "pending");
    assert_eq!(env.query::<i64>("SELECT count(*) FROM media_facts"), 1);

    // 書き込みが失敗する（書く前に落ちたのと同じく、何も書かれない）
    env.exec(
        "CREATE TRIGGER fail_facts BEFORE INSERT ON media_facts BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    );
    let record = env.run();
    assert_eq!(record.state, JobState::Failed);
    assert_eq!(
        (env.state(2), env.state(1)),
        ("pending".into(), "pending".into())
    );
    env.exec("DROP TRIGGER fail_facts");
    let record = env.run();
    assert_eq!(record.state, JobState::Done);
    assert_eq!(
        (env.state(2), env.state(1)),
        ("ready".into(), "ready".into())
    );
}

/// 同じ中身の結果がそろっていれば、読まずに使う（読み取りの記録の列も変えない）。
#[test]
fn same_content_results_are_reused_without_decoding() {
    let env = Env::new();
    let img = env.add(1, "image", "png", &sprite(5));
    let song = env.add(2, "audio", "wav", &wav(4_000, None));
    env.run();
    assert_eq!(env.reads().len(), 2);
    let before: (String, i64) = (env.fact(&img, "decoder"), env.fact(&img, "pixel_art"));
    env.exec("UPDATE items SET analysis_state = 'pending'");
    let record = env.run();
    assert_eq!(env.reads().len(), 2, "読み直さない");
    assert_eq!(
        (env.state(1), env.state(2)),
        ("ready".into(), "ready".into())
    );
    assert_eq!(
        (
            env.fact::<String>(&img, "decoder"),
            env.fact::<i64>(&img, "pixel_art")
        ),
        before
    );
    assert_eq!(result_json(&record)["reused"].as_u64(), Some(2));

    // 波形の段のファイルがなくなっていたら、読み直して作る
    std::fs::remove_file(env.library.layout().peaks_path(&song)).unwrap();
    env.exec("UPDATE items SET analysis_state = 'pending'");
    env.run();
    assert_eq!(env.reads().len(), 3);
    assert!(env.library.layout().peaks_path(&song).is_file());
}

/// 投影の版が上がったら（古い行）、読まずに結果の表から作り直す。
#[test]
fn facts_version_bump_reprojects_without_decoding() {
    let env = Env::new();
    let img = env.add(1, "image", "png", &sprite(6));
    let photo = env.add(2, "image", "jpg", b"jpeg");
    env.run();
    assert_eq!(env.reads().len(), 1);
    env.exec("UPDATE media_facts SET facts_v = 0, pixel_art = NULL, alpha = NULL, colors = NULL, frame_count = NULL");
    let record = env.run();
    assert_eq!(env.reads().len(), 1, "読み直さない");
    assert_eq!(result_json(&record)["reprojected"].as_u64(), Some(2));
    assert_eq!(env.fact::<i64>(&img, "facts_v"), FACTS_V);
    assert_eq!(
        (
            env.fact::<i64>(&img, "alpha"),
            env.fact::<i64>(&img, "pixel_art"),
            env.fact::<i64>(&img, "frame_count")
        ),
        (1, 1, 1)
    );
    assert!(env.fact::<i64>(&img, "colors") <= 16);
    assert_eq!(
        (
            env.fact::<i64>(&photo, "alpha"),
            env.fact::<i64>(&photo, "pixel_art")
        ),
        (0, 0)
    );
    assert_eq!(env.state(1), "ready");
}

/// 読めないものは失敗を記録し、指紋が変わるか頼まれるまで読み直さない。
#[test]
fn failure_is_recorded_and_not_retried_until_fingerprint_changes() {
    let env = Env::new();
    let broken = env.add(1, "image", "png", b"\x89PNG broken");
    env.run();
    assert_eq!(env.state(1), "failed");
    assert_eq!(env.fact::<String>(&broken, "state"), "failed");
    assert_eq!(env.fact::<String>(&broken, "error_code"), "decode");
    let tried = env.tries();
    env.run();
    assert_eq!(env.tries(), tried, "失敗したものは読み直さない");
    assert_eq!(env.state(1), "failed");

    // 画像の解析の版が変わった（起動のスレッドの sync_plan）
    let registry = kura_engine::analysis::registry().unwrap();
    let current = PlanFingerprint::current(&registry).unwrap();
    let mut older = current.clone();
    older
        .kinds
        .get_mut("image")
        .unwrap()
        .get_mut("image.alpha")
        .unwrap()
        .0 = 0;
    let old_json = older.to_canonical().unwrap();
    env.library
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![PLAN_KEY, old_json],
            )?;
            store::sync_plan(tx, &current)
        })
        .unwrap();
    assert_eq!(env.state(1), "pending");
    env.run();
    assert_eq!(env.tries(), tried + 1);
    assert_eq!(env.state(1), "failed");

    // 利用者が頼んだ（retry_analysis）
    env.library
        .db()
        .write(|tx| store::retry_failed(tx, &[1]))
        .unwrap();
    env.run();
    assert_eq!(env.tries(), tried + 2);
}

/// 見積もりが min(予算/2, 1 GiB) を超えるものは、予算を確保せずに failed（too_large）にする（予算が埋まっていても待たない）。
#[test]
fn too_large_fails_without_acquiring() {
    let env = Env::with(|log, _| watched_set(log, None, Vec::new()), 1 << 20);
    let big = env.add(
        1,
        "image",
        "png",
        &png(InputGen::Noise {
            w: 256,
            h: 256,
            fmt: PixelFormat::Rgba8,
            seed: 1,
        }),
    );
    // 予算をすべて持っておく：確保しようとすれば待ち続けて終わらない
    let runtime = env.engine.runtime().unwrap().clone();
    let held = runtime
        .budget()
        .try_acquire(runtime.budget().total(), Class::Interactive)
        .unwrap()
        .unwrap();
    let record = env.run();
    assert_eq!(record.state, JobState::Done);
    assert_eq!(env.state(1), "failed");
    assert_eq!(env.fact::<String>(&big, "error_code"), "too_large");
    assert!(env.reads().is_empty(), "読まない");
    assert_eq!(env.tries(), 1, "ヘッダだけ読んで見積もる");
    drop(held);
    assert_eq!(runtime.budget().used().unwrap(), 0);
}

/// 波形の段のファイルは DB に書く前に置く（書き込みが失敗してもファイルはあり、アイテムは pending のまま）。
#[test]
fn peaks_file_written_before_commit() {
    let env = Env::new();
    let song = env.add(1, "audio", "wav", &wav(20_000, None));
    let peaks = env.library.layout().peaks_path(&song);
    env.exec(
        "CREATE TRIGGER fail_facts BEFORE INSERT ON media_facts BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    );
    let record = env.run();
    assert_eq!(record.state, JobState::Failed);
    assert!(peaks.is_file(), "コミットの前に置いている");
    assert_eq!(env.state(1), "pending");
    assert_eq!(env.query::<i64>("SELECT count(*) FROM analysis_results"), 0);
    env.exec("DROP TRIGGER fail_facts");
    env.run();
    assert_eq!(env.state(1), "ready");
    let bytes = std::fs::read(&peaks).unwrap();
    let parsed = Peaks::from_canonical(&bytes).unwrap();
    assert_eq!(parsed.frames, 20_000);
    let (encoding, output_len, has_output): (String, i64, bool) = env
        .library
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT encoding, output_len, output IS NOT NULL FROM analysis_results WHERE analysis = 'audio.peaks'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (encoding.as_str(), output_len, has_output),
        ("binary", bytes.len() as i64, false)
    );
    // 置き場の一時ファイルは残さない
    let leftovers: Vec<_> = std::fs::read_dir(env.library.layout().staging_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("peaks-"))
        .collect();
    assert!(leftovers.is_empty());
}

/// OS のデコーダ（Platform の段）で読むときは、読む前に処理中の印を付けてコミットし、書いたら消す。前の実行が印を残して
/// 終わっていたら（読んでいる途中で落ちた）、そのアイテムを failed（crashed）にして先へ進む。
#[test]
fn os_decoder_marks_the_item_before_reading_and_a_crash_fails_it() {
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let s = seen.clone();
    let env = Env::with(
        |log, library| {
            let library = library.clone();
            let hook: Hook = Arc::new(move |_| {
                let mark: Option<String> = library
                    .db()
                    .read(|c| {
                        Ok(c.query_row(
                            "SELECT value FROM settings WHERE key = ?1",
                            [INFLIGHT_KEY],
                            |r| r.get(0),
                        )
                        .ok())
                    })
                    .unwrap();
                s.lock().unwrap().push(mark);
            });
            let fake = FakePlatformDecoder::new(
                "fake-imageio-dec/1",
                &["bmp"],
                InputGen::Noise {
                    w: 8,
                    h: 8,
                    fmt: PixelFormat::Rgba8,
                    seed: 2,
                },
            );
            watched_set(log, Some(hook), vec![Arc::new(fake)])
        },
        64 << 20,
    );
    let bmp = env.add(1, "image", "bmp", b"BM fake");
    let pngh = env.add(2, "image", "png", &sprite(2));
    env.run();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    let marks: Vec<Option<Inflight>> = seen
        .iter()
        .map(|m| m.as_deref().and_then(|j| serde_json::from_str(j).ok()))
        .collect();
    assert_eq!(marks[0], None, "固定の段では印を付けない");
    assert_eq!(
        marks[1],
        Some(Inflight {
            id: 1,
            hash: bmp.clone()
        }),
        "OS のデコーダで読む前に印をコミットしている"
    );
    assert_eq!(
        env.query::<i64>("SELECT count(*) FROM settings WHERE key = 'analysis.inflight'"),
        0
    );
    assert_eq!(
        (env.state(1), env.state(2)),
        ("ready".into(), "ready".into())
    );
    assert_eq!(env.fact::<String>(&bmp, "decoder"), "fake-imageio-dec/1");
    assert_eq!(env.fact::<i64>(&bmp, "reproducible"), 0);
    assert!(env.fact::<String>(&bmp, "env").contains("fake"));
    assert!(!pngh.is_empty());

    // 前回、3 を OS のデコーダで読んでいる途中で落ちた
    let crashed = env.add(3, "image", "bmp", b"BM crash");
    env.add(4, "image", "png", &sprite(4));
    let mark = serde_json::to_string(&Inflight {
        id: 3,
        hash: crashed.clone(),
    })
    .unwrap();
    env.library
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)",
                params![INFLIGHT_KEY, mark],
            )?;
            Ok(())
        })
        .unwrap();
    let reads = env.reads().len();
    env.run();
    assert_eq!(env.state(3), "failed");
    assert_eq!(env.fact::<String>(&crashed, "error_code"), "crashed");
    assert_eq!(env.state(4), "ready");
    assert_eq!(env.reads().len(), reads + 1, "落ちたものは読み直さない");
    assert_eq!(
        env.query::<i64>("SELECT count(*) FROM settings WHERE key = 'analysis.inflight'"),
        0
    );
}

/// 読み取りの記録は結果の表に kura.read として残り、読まずに計画を立てるのに使う。
#[test]
fn read_record_is_kept_with_the_results() {
    let env = Env::new();
    let song = env.add(1, "audio", "wav", &wav(1_000, None));
    env.run();
    let read: Vec<u8> = env.query(&format!(
        "SELECT r.output FROM analysis_results r JOIN blobs b ON b.id = r.blob_id
          WHERE r.analysis = '{READ_ID}' AND hex(b.hash) = upper('{song}')"
    ));
    let read = super::facts::ReadRecord::from_canonical(&read).unwrap();
    assert!(matches!(
        read,
        super::facts::ReadRecord::Audio {
            rate: 8_000,
            channels: 1,
            frames: 1_000,
            ..
        }
    ));
}
