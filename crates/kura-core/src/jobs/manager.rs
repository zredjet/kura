//! ジョブの待ち行列と実行。スケジューラのスレッドが種類ごとの空きに合わせてジョブを取り出し、ジョブごとにスレッドで動かす。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::store::{self, JobFilter, JobRecord};
use super::{JobEvent, JobId, JobKind, JobState, Throttle};
use crate::library::Library;
use crate::time::{format_utc, now_ms};
use crate::{CoreError, Result};

/// 進捗イベントの送り先。
pub type EventSink = Arc<dyn Fn(JobEvent) + Send + Sync>;

/// 進捗イベントの最小間隔。
const PROGRESS_INTERVAL_MS: i64 = 200;

/// スケジューラが待ち行列を見直す間隔（通知がなくても見直す）。
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

const RUN: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;
const SHUTDOWN: u8 = 3;

/// ジョブの種類ごとの処理。
pub trait JobHandler: Send + Sync {
    /// ジョブを実行する。`ctx.control()` を区切りごとに確かめ、一時停止・中止に応じる。
    fn run(&self, ctx: &JobContext) -> Result<JobEnd>;
}

/// ジョブの終わり方。
#[derive(Debug, Clone, PartialEq)]
pub enum JobEnd {
    /// 完了。
    Done {
        result_json: String,
        message: Option<String>,
    },
    /// 一時停止（途中経過はジョブ側が保存済み）。
    Paused,
    /// 中止。
    Canceled { result_json: Option<String> },
}

/// 実行中のジョブへの指示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Continue,
    /// 区切りのよいところで止めて `JobEnd::Paused` を返す。
    Pause,
    /// 区切りのよいところで止めて `JobEnd::Canceled` を返す。
    Cancel,
}

/// ジョブの実行に必要なもの。
pub struct JobContext {
    record: JobRecord,
    library: Arc<Library>,
    control: Arc<AtomicU8>,
    sink: EventSink,
    throttle: Mutex<Throttle>,
    log: Mutex<Option<File>>,
}

impl JobContext {
    /// ジョブの ID。
    pub fn id(&self) -> JobId {
        self.record.id
    }

    /// 入力（jobs.input_json）。
    pub fn input(&self) -> &str {
        &self.record.input_json
    }

    /// 前回までの途中経過（jobs.result_json）。再開に使う。
    pub fn partial(&self) -> Option<&str> {
        self.record.result_json.as_deref()
    }

    /// ライブラリ。
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// 一時停止・中止の指示を確かめる。
    pub fn control(&self) -> Control {
        match self.control.load(Ordering::Acquire) {
            CANCEL => Control::Cancel,
            PAUSE | SHUTDOWN => Control::Pause,
            _ => Control::Continue,
        }
    }

    /// 進捗を送る（200ms より短い間隔の呼び出しは間引く）。
    pub fn progress(&self, progress: f64, message: &str) {
        let ready = self
            .throttle
            .lock()
            .map(|mut t| t.ready(now_ms()))
            .unwrap_or(true);
        if ready {
            (self.sink)(JobEvent {
                id: self.record.id,
                kind: self.record.kind,
                state: JobState::Running,
                progress: Some(progress.clamp(0.0, 1.0)),
                message: Some(message.to_string()),
                result_json: None,
            });
        }
    }

    /// ジョブのログ（`logs/job-<id>.log`）に 1 行書く。
    pub fn log(&self, line: &str) {
        let Ok(mut file) = self.log.lock() else {
            return;
        };
        if file.is_none() {
            let path = self.library.layout().job_log_path(self.record.id);
            *file = OpenOptions::new().create(true).append(true).open(path).ok();
        }
        if let Some(file) = file.as_mut() {
            let _ = writeln!(file, "{} {line}", format_utc(now_ms()));
        }
    }
}

struct Sched {
    started: bool,
    shutdown: bool,
    running: HashMap<JobKind, usize>,
    controls: HashMap<JobId, Arc<AtomicU8>>,
    workers: Vec<JoinHandle<()>>,
}

struct Inner {
    library: Arc<Library>,
    handlers: HashMap<JobKind, Arc<dyn JobHandler>>,
    sink: EventSink,
    sched: Mutex<Sched>,
    wake: Condvar,
}

/// ジョブの待ち行列。
pub struct JobManager {
    inner: Arc<Inner>,
    scheduler: Mutex<Option<JoinHandle<()>>>,
}

impl JobManager {
    /// 作る（`start` までジョブは動かさない）。前回 `running` のまま終わったジョブは `queued` に戻す。
    pub fn new(
        library: Arc<Library>,
        handlers: Vec<(JobKind, Arc<dyn JobHandler>)>,
        sink: EventSink,
    ) -> Result<Self> {
        let recovered = library.db().write(|tx| store::recover_running(tx))?;
        if recovered > 0 {
            tracing::info!(recovered, "中断していたジョブを待ち行列に戻しました");
        }
        Ok(Self {
            inner: Arc::new(Inner {
                library,
                handlers: handlers.into_iter().collect(),
                sink,
                sched: Mutex::new(Sched {
                    started: false,
                    shutdown: false,
                    running: HashMap::new(),
                    controls: HashMap::new(),
                    workers: Vec::new(),
                }),
                wake: Condvar::new(),
            }),
            scheduler: Mutex::new(None),
        })
    }

    /// 待ち行列の処理を始める。
    pub fn start(&self) -> Result<()> {
        {
            let mut sched = self.lock()?;
            if sched.started || sched.shutdown {
                return Ok(());
            }
            sched.started = true;
        }
        let inner = self.inner.clone();
        let handle = std::thread::Builder::new()
            .name("kura-jobs".into())
            .spawn(move || scheduler_loop(&inner))?;
        if let Ok(mut slot) = self.scheduler.lock() {
            *slot = Some(handle);
        }
        Ok(())
    }

    /// ジョブを追加する。
    pub fn enqueue(&self, kind: JobKind, input_json: String) -> Result<JobId> {
        let id = self
            .inner
            .library
            .db()
            .write(move |tx| store::insert(tx, kind, &input_json, now_ms()))?;
        self.emit(JobEvent {
            id,
            kind,
            state: JobState::Queued,
            progress: None,
            message: None,
            result_json: None,
        });
        self.inner.wake.notify_all();
        Ok(id)
    }

    /// 1 件読む。
    pub fn get(&self, id: JobId) -> Result<JobRecord> {
        self.inner
            .library
            .db()
            .read(|conn| store::get(conn, id))?
            .ok_or_else(|| CoreError::NotFound(format!("ジョブ {id}")))
    }

    /// 新しい順に一覧する。
    pub fn list(&self, filter: &JobFilter) -> Result<Vec<JobRecord>> {
        self.inner
            .library
            .db()
            .read(|conn| store::list(conn, filter))
    }

    /// 中止する。実行中なら区切りで止まる。
    pub fn cancel(&self, id: JobId) -> Result<()> {
        self.signal_or_transition(
            id,
            CANCEL,
            &[JobState::Queued, JobState::Paused],
            JobState::Canceled,
        )
    }

    /// 一時停止する。実行中なら区切りで止まる。
    pub fn pause(&self, id: JobId) -> Result<()> {
        self.signal_or_transition(id, PAUSE, &[JobState::Queued], JobState::Paused)
    }

    /// 一時停止したジョブを待ち行列に戻す。
    pub fn resume(&self, id: JobId) -> Result<()> {
        let event = {
            let _sched = self.lock()?;
            let changed = self.inner.library.db().write(move |tx| {
                store::transition(tx, id, &[JobState::Paused], JobState::Queued)
            })?;
            if changed { Some(self.get(id)?) } else { None }
        };
        if let Some(record) = event {
            self.emit(event_from(&record));
            self.inner.wake.notify_all();
        }
        Ok(())
    }

    /// 止める。実行中のジョブには一時停止を指示し（次回 `queued` から再開）、終わるのを待つ。
    pub fn shutdown(&self) {
        let workers = match self.inner.sched.lock() {
            Ok(mut sched) => {
                sched.shutdown = true;
                for control in sched.controls.values() {
                    control.store(SHUTDOWN, Ordering::Release);
                }
                std::mem::take(&mut sched.workers)
            }
            Err(_) => Vec::new(),
        };
        self.inner.wake.notify_all();
        if let Ok(mut slot) = self.scheduler.lock()
            && let Some(handle) = slot.take()
        {
            let _ = handle.join();
        }
        for worker in workers {
            let _ = worker.join();
        }
    }

    /// 待ち行列と実行中のジョブがなくなるまで待つ（テスト・開発ツール用）。時間切れなら false。
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let running = self
                .inner
                .sched
                .lock()
                .map(|s| s.running.values().sum::<usize>())
                .unwrap_or(0);
            let queued = self
                .list(&JobFilter {
                    kinds: self.inner.handlers.keys().copied().collect(),
                    states: vec![JobState::Queued, JobState::Running],
                    limit: Some(1),
                })
                .map(|rows| rows.len())
                .unwrap_or(0);
            if running == 0 && queued == 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Sched>> {
        self.inner
            .sched
            .lock()
            .map_err(|_| CoreError::Invalid("ジョブの管理が止まっています".into()))
    }

    fn emit(&self, event: JobEvent) {
        (self.inner.sink)(event);
    }

    /// 実行中なら指示を出し、そうでなければ DB の状態を `from` → `to` に変える。
    fn signal_or_transition(
        &self,
        id: JobId,
        signal: u8,
        from: &'static [JobState],
        to: JobState,
    ) -> Result<()> {
        let event = {
            let sched = self.lock()?;
            if let Some(control) = sched.controls.get(&id) {
                control.store(signal, Ordering::Release);
                None
            } else {
                let changed = self.inner.library.db().write(move |tx| {
                    let changed = store::transition(tx, id, from, to)?;
                    if changed && to.is_terminal() {
                        store::finish(tx, id, to, None, None, None, None, now_ms())?;
                    }
                    Ok(changed)
                })?;
                if changed { Some(self.get(id)?) } else { None }
            }
        };
        if let Some(record) = event {
            self.emit(event_from(&record));
        }
        Ok(())
    }
}

impl Drop for JobManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn event_from(record: &JobRecord) -> JobEvent {
    JobEvent {
        id: record.id,
        kind: record.kind,
        state: record.state,
        progress: record.progress,
        message: record.message.clone(),
        result_json: record.result_json.clone(),
    }
}

fn scheduler_loop(inner: &Arc<Inner>) {
    let Ok(mut sched) = inner.sched.lock() else {
        return;
    };
    loop {
        if sched.shutdown {
            return;
        }
        sched.workers.retain(|w| !w.is_finished());
        let mut kinds: Vec<JobKind> = inner.handlers.keys().copied().collect();
        kinds.sort_by_key(|k| k.as_str());
        for kind in kinds {
            while sched.running.get(&kind).copied().unwrap_or(0) < kind.concurrency() {
                let layout = inner.library.layout().clone();
                let claimed = inner.library.db().write(move |tx| {
                    store::claim_next(tx, kind, now_ms(), |id| {
                        layout.job_log_path(id).to_string_lossy().into_owned()
                    })
                });
                match claimed {
                    Ok(Some(record)) => spawn_worker(inner, &mut sched, record),
                    Ok(None) => break,
                    Err(err) => {
                        tracing::error!(%err, "ジョブを取り出せませんでした");
                        break;
                    }
                }
            }
        }
        sched = match inner.wake.wait_timeout(sched, RESCAN_INTERVAL) {
            Ok((guard, _)) => guard,
            Err(_) => return,
        };
    }
}

fn spawn_worker(inner: &Arc<Inner>, sched: &mut Sched, record: JobRecord) {
    let Some(handler) = inner.handlers.get(&record.kind).cloned() else {
        return;
    };
    let id = record.id;
    let kind = record.kind;
    let control = Arc::new(AtomicU8::new(RUN));
    let ctx = JobContext {
        record,
        library: inner.library.clone(),
        control: control.clone(),
        sink: inner.sink.clone(),
        throttle: Mutex::new(Throttle::new(PROGRESS_INTERVAL_MS)),
        log: Mutex::new(None),
    };
    let worker_inner = inner.clone();
    match std::thread::Builder::new()
        .name(format!("kura-job-{id}"))
        .spawn(move || run_worker(&worker_inner, handler.as_ref(), ctx))
    {
        Ok(handle) => {
            sched.controls.insert(id, control);
            *sched.running.entry(kind).or_insert(0) += 1;
            sched.workers.push(handle);
        }
        Err(err) => {
            tracing::error!(%err, id, "ジョブのスレッドを作れませんでした");
            let _ = inner
                .library
                .db()
                .write(move |tx| store::transition(tx, id, &[JobState::Running], JobState::Queued));
        }
    }
}

fn run_worker(inner: &Arc<Inner>, handler: &dyn JobHandler, ctx: JobContext) {
    let id = ctx.id();
    let kind = ctx.record.kind;
    (inner.sink)(JobEvent {
        id,
        kind,
        state: JobState::Running,
        progress: ctx.record.progress,
        message: ctx.record.message.clone(),
        result_json: None,
    });
    ctx.log("開始");

    let outcome = handler.run(&ctx);
    let shutting_down = ctx.control.load(Ordering::Acquire) == SHUTDOWN;
    let db = inner.library.db();
    let saved = match outcome {
        Ok(JobEnd::Done {
            result_json,
            message,
        }) => {
            ctx.log("完了");
            db.write(move |tx| {
                store::finish(
                    tx,
                    id,
                    JobState::Done,
                    Some(1.0),
                    message.as_deref(),
                    None,
                    Some(&result_json),
                    now_ms(),
                )
            })
        }
        Ok(JobEnd::Paused) => {
            // アプリの終了で止めたものは次回自動で再開する
            let to = if shutting_down {
                JobState::Queued
            } else {
                JobState::Paused
            };
            ctx.log(if shutting_down {
                "終了のため中断"
            } else {
                "一時停止"
            });
            db.write(move |tx| store::transition(tx, id, &[JobState::Running], to).map(|_| ()))
        }
        Ok(JobEnd::Canceled { result_json }) => {
            ctx.log("中止");
            db.write(move |tx| {
                store::finish(
                    tx,
                    id,
                    JobState::Canceled,
                    None,
                    None,
                    None,
                    result_json.as_deref(),
                    now_ms(),
                )
            })
        }
        Err(err) => {
            ctx.log(&format!("失敗: {err}"));
            tracing::warn!(id, %err, "ジョブが失敗しました");
            let code = err.code();
            let message = err.to_string();
            db.write(move |tx| {
                store::finish(
                    tx,
                    id,
                    JobState::Failed,
                    None,
                    Some(&message),
                    Some(code),
                    None,
                    now_ms(),
                )
            })
        }
    };
    if let Err(err) = saved {
        tracing::error!(id, %err, "ジョブの状態を保存できませんでした");
    }
    if let Ok(Some(record)) = db.read(|conn| store::get(conn, id)) {
        (inner.sink)(event_from(&record));
    }

    if let Ok(mut sched) = inner.sched.lock() {
        if let Some(n) = sched.running.get_mut(&kind) {
            *n = n.saturating_sub(1);
        }
        sched.controls.remove(&id);
    }
    inner.wake.notify_all();
}

#[cfg(test)]
mod tests {
    use super::store::save_partial;
    use super::*;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicUsize;

    type RunFn = dyn Fn(&JobContext) -> Result<JobEnd> + Send + Sync;
    struct FnHandler(Box<RunFn>);
    impl JobHandler for FnHandler {
        fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
            (self.0)(ctx)
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        library: Arc<Library>,
        events: Arc<Mutex<Vec<JobEvent>>>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        Fixture {
            _dir: dir,
            library,
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn sink(events: &Arc<Mutex<Vec<JobEvent>>>) -> EventSink {
        let events = events.clone();
        Arc::new(move |e| events.lock().unwrap().push(e))
    }

    fn manager(
        f: &Fixture,
        handler: impl Fn(&JobContext) -> Result<JobEnd> + Send + Sync + 'static,
    ) -> JobManager {
        JobManager::new(
            f.library.clone(),
            vec![(JobKind::Import, Arc::new(FnHandler(Box::new(handler))))],
            sink(&f.events),
        )
        .unwrap()
    }

    fn done(json: &str) -> Result<JobEnd> {
        Ok(JobEnd::Done {
            result_json: json.into(),
            message: None,
        })
    }

    #[test]
    fn runs_queued_job_to_done_with_events() {
        let f = fixture();
        let jobs = manager(&f, |ctx| {
            ctx.progress(0.5, "半分");
            ctx.log("処理中");
            done("{\"ok\":true}")
        });
        let id = jobs.enqueue(JobKind::Import, "{}".into()).unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(Duration::from_secs(5)));

        let record = jobs.get(id).unwrap();
        assert_eq!(record.state, JobState::Done);
        assert_eq!(record.progress, Some(1.0));
        assert_eq!(record.result_json.as_deref(), Some("{\"ok\":true}"));
        let log = std::fs::read_to_string(f.library.layout().job_log_path(id)).unwrap();
        assert!(log.contains("処理中"));

        let states: Vec<JobState> = f.events.lock().unwrap().iter().map(|e| e.state).collect();
        assert_eq!(states.first(), Some(&JobState::Queued));
        assert!(states.contains(&JobState::Running));
        assert_eq!(states.last(), Some(&JobState::Done));
    }

    #[test]
    fn import_jobs_run_one_at_a_time() {
        let f = fixture();
        let active = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let (a, m) = (active.clone(), max.clone());
        let jobs = manager(&f, move |_| {
            let now = a.fetch_add(1, Ordering::SeqCst) + 1;
            m.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(30));
            a.fetch_sub(1, Ordering::SeqCst);
            done("{}")
        });
        for _ in 0..3 {
            jobs.enqueue(JobKind::Import, "{}".into()).unwrap();
        }
        jobs.start().unwrap();
        assert!(jobs.wait_idle(Duration::from_secs(5)));
        assert_eq!(max.load(Ordering::SeqCst), 1);
    }

    /// 0.1 ずつ進めて途中経過を保存し、一時停止・中止に応じるハンドラ。
    fn stepping_handler(ctx: &JobContext) -> Result<JobEnd> {
        let mut step: u32 = ctx.partial().and_then(|s| s.parse().ok()).unwrap_or(0);
        while step < 10 {
            match ctx.control() {
                Control::Continue => {}
                Control::Pause => return Ok(JobEnd::Paused),
                Control::Cancel => {
                    return Ok(JobEnd::Canceled {
                        result_json: Some(step.to_string()),
                    });
                }
            }
            step += 1;
            let id = ctx.id();
            let json = step.to_string();
            ctx.library()
                .db()
                .write(move |tx| save_partial(tx, id, &json, f64::from(step) / 10.0, ""))?;
            ctx.progress(f64::from(step) / 10.0, "");
            std::thread::sleep(Duration::from_millis(15));
        }
        done("10")
    }

    #[test]
    fn pause_then_resume_continues_from_saved_point() {
        let f = fixture();
        let jobs = Arc::new(manager(&f, stepping_handler));
        let id = jobs.enqueue(JobKind::Import, "{}".into()).unwrap();
        jobs.start().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        jobs.pause(id).unwrap();
        assert!(jobs.wait_idle(Duration::from_secs(5)));

        let paused = jobs.get(id).unwrap();
        assert_eq!(paused.state, JobState::Paused);
        let step: u32 = paused.result_json.unwrap().parse().unwrap();
        assert!((1..10).contains(&step), "step = {step}");

        jobs.resume(id).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(jobs.wait_idle(Duration::from_secs(5)));
        let finished = jobs.get(id).unwrap();
        assert_eq!(finished.state, JobState::Done);
        assert_eq!(finished.result_json.as_deref(), Some("10"));
    }

    #[test]
    fn cancel_queued_and_running_jobs() {
        let f = fixture();
        let jobs = manager(&f, stepping_handler);
        let queued = jobs.enqueue(JobKind::Import, "{}".into()).unwrap();
        jobs.cancel(queued).unwrap();
        assert_eq!(jobs.get(queued).unwrap().state, JobState::Canceled);

        let running = jobs.enqueue(JobKind::Import, "{}".into()).unwrap();
        jobs.start().unwrap();
        std::thread::sleep(Duration::from_millis(40));
        jobs.cancel(running).unwrap();
        assert!(jobs.wait_idle(Duration::from_secs(5)));
        let record = jobs.get(running).unwrap();
        assert_eq!(record.state, JobState::Canceled);
        assert!(record.finished_at.is_some());
    }

    #[test]
    fn failure_is_recorded_with_code() {
        let f = fixture();
        let jobs = manager(&f, |_| Err(CoreError::Invalid("壊れた入力".into())));
        let id = jobs.enqueue(JobKind::Import, "{}".into()).unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(Duration::from_secs(5)));
        let record = jobs.get(id).unwrap();
        assert_eq!(record.state, JobState::Failed);
        assert_eq!(record.error_code.as_deref(), Some("Invalid"));
        assert!(record.message.unwrap().contains("壊れた入力"));
    }

    #[test]
    fn interrupted_running_job_is_requeued_and_shutdown_requeues() {
        let f = fixture();
        // 前回のアプリが running のまま落ちた状態を作る
        let id = f
            .library
            .db()
            .write(|tx| {
                let id = store::insert(tx, JobKind::Import, "{}", 0)?;
                store::claim_next(tx, JobKind::Import, 1, |_| String::new())?;
                Ok(id)
            })
            .unwrap();

        let jobs = manager(&f, stepping_handler);
        assert_eq!(jobs.get(id).unwrap().state, JobState::Queued);
        jobs.start().unwrap();
        std::thread::sleep(Duration::from_millis(40));
        jobs.shutdown();
        // 終了で止めたものは paused ではなく queued に戻る
        let record = jobs.get(id).unwrap();
        assert_eq!(record.state, JobState::Queued);
        assert!(record.result_json.is_some());
    }

    #[test]
    fn sink_may_call_back_into_manager() {
        let f = fixture();
        let slot: Arc<OnceLock<Arc<JobManager>>> = Arc::new(OnceLock::new());
        let events = f.events.clone();
        let slot2 = slot.clone();
        let sink: EventSink = Arc::new(move |e: JobEvent| {
            if e.state == JobState::Running
                && e.progress.is_some_and(|p| p > 0.0)
                && let Some(jobs) = slot2.get()
            {
                jobs.pause(e.id).unwrap();
            }
            events.lock().unwrap().push(e);
        });
        let jobs = Arc::new(
            JobManager::new(
                f.library.clone(),
                vec![(
                    JobKind::Import,
                    Arc::new(FnHandler(Box::new(stepping_handler))),
                )],
                sink,
            )
            .unwrap(),
        );
        let _ = slot.set(jobs.clone());
        let id = jobs.enqueue(JobKind::Import, "{}".into()).unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(Duration::from_secs(5)));
        assert_eq!(jobs.get(id).unwrap().state, JobState::Paused);
    }
}
