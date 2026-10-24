//! アプリの道のベンチ（M12 の段 6。requirements.md の NFR-11〜13・17、docs/editing.md §16、architecture.md §13）。
//!
//! M10 の `bench edit` はエンジンの中（`Engine::render_view`・一括の `render`）だけを測る。ここでは M12 で足したアプリの道を、
//! アプリと同じ部品（`LibrarySession`・`EngineServices`・アプリのコーデックの組・取り込みのジョブ・保存のジョブ）で測る：
//!
//! - `bench editor`（[`editor`]）：`edit::editor` の 1 手（`step`：手を受けて、表示の値と変わったタイルの一覧ができるまで）と、
//!   kura:// の `/edit` のタイルの書き出し（`tile::serve`。RGBA8 と PNG）。NFR-17 の取り置き・使用量の戻りと、24MP を開いた
//!   ときの最大の使用量も。
//! - `bench save`（[`save`]）：保存のジョブの全体（`start_save` → ジョブ → 描く → 書く → 置く → コミット → Done の知らせ）。
//!
//! どちらも新しいライブラリを `--work` の下に作り（利用者のライブラリには触れない）、入力は ChaCha8 と整数だけで作る
//! （`InputGen`・`testing::bench::cases`。M10 の bench edit と同じ文書・同じ手）。測り終えたらライブラリを消す（`--keep` で残す）。
//! 画面（IPC・WebKit）は含めない。

pub(crate) mod editor;
mod report;
pub(crate) mod save;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kura_core::edit::{CodecSet, EngineFactory, GcClock, RuntimeConfig};
use kura_core::import::{ImportInput, ImportOptions, start_import};
use kura_core::jobs::{EventSink, JobEvent, JobId, JobKind, JobState};
use kura_core::library::{LibrarySession, Services};
use kura_engine::NoMonitor;
use kura_engine::Value;
use serde_json::{Value as J, json};

pub(crate) use report::{Report, Row, Verdict};

const MIB: u64 = 1 << 20;

/// ジョブが動き終えるのを待つ上限。
const SETTLE: Duration = Duration::from_secs(1800);

/// プレビュー・一括のプールの大きさを決めたエンジンの部品（予算・コーデックの組はアプリと同じ：物理メモリから・
/// `crate::app_codecs`）。
pub(crate) struct BenchFactory {
    pub preview: usize,
    pub bulk: usize,
}

impl EngineFactory for BenchFactory {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            physical_memory: kura_mac::physical_memory(),
            preview_threads: Some(self.preview),
            bulk_threads: Some(self.bulk),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, work_dir: &Path) -> CodecSet {
        crate::app_codecs(work_dir)
    }
}

/// アプリと同じ渡しもの（ImageIO のメディア・アプリのコーデックの組）で、プールの大きさだけを決めたもの。
pub(crate) fn services(preview: usize, bulk: usize) -> Services {
    Services::new(kura_mac::media_backend()).with_engine(Arc::new(BenchFactory { preview, bulk }))
}

/// 保存のジョブの知らせ（動き始め・終わり）の時刻。
#[derive(Debug, Clone, Default)]
pub(crate) struct Seen {
    pub running: Option<Instant>,
    pub end: Option<(Instant, JobState, Option<String>, Option<String>)>,
}

/// 保存のジョブの知らせを受けて時刻を覚える（`EventSink`）。
#[derive(Default)]
pub(crate) struct Recorder {
    seen: Mutex<BTreeMap<JobId, Seen>>,
    cv: Condvar,
}

impl Recorder {
    /// ライブラリに渡す知らせの口（保存のジョブだけ覚える）。
    pub fn sink(self: &Arc<Self>) -> EventSink {
        let me = self.clone();
        Arc::new(move |e: JobEvent| {
            if e.kind != JobKind::Edit {
                return;
            }
            let now = Instant::now();
            let mut seen = me.seen.lock().unwrap_or_else(PoisonError::into_inner);
            let s = seen.entry(e.id).or_default();
            if e.state == JobState::Running && s.running.is_none() {
                s.running = Some(now);
            }
            if e.state.is_terminal() && s.end.is_none() {
                s.end = Some((now, e.state, e.message.clone(), e.result_json.clone()));
            }
            drop(seen);
            me.cv.notify_all();
        })
    }

    /// ジョブ `id` が終わるまで待って、知らせの時刻を返す。
    pub fn wait(&self, id: JobId, timeout: Duration) -> Result<Seen, String> {
        let deadline = Instant::now() + timeout;
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(s) = seen.get(&id)
                && s.end.is_some()
            {
                return Ok(s.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("保存のジョブ {id} が終わりません"));
            }
            seen = self
                .cv
                .wait_timeout(seen, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// ジョブが動き終えるまで待つ（取り込みのあとに積まれるサムネイル・解析、保存のあとの GC まで。2 回続けて空なら終わり）。
pub(crate) fn settle(session: &LibrarySession) -> Result<(), String> {
    for _ in 0..2 {
        if !session.jobs().wait_idle(SETTLE) {
            return Err("ジョブが動き終えません".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

/// `src` のファイルをアプリと同じ取り込みのジョブで入れ、ジョブが動き終えるまで待つ。元のファイル名 → アイテムの ID。
pub(crate) fn import_all(
    session: &LibrarySession,
    src: &Path,
) -> Result<BTreeMap<String, i64>, String> {
    let id = start_import(
        session.jobs(),
        &ImportInput {
            paths: vec![src.to_path_buf()],
            options: ImportOptions::default(),
        },
    )
    .map_err(|e| e.to_string())?;
    settle(session)?;
    let record = session.jobs().get(id).map_err(|e| e.to_string())?;
    if record.state != JobState::Done {
        return Err(format!("取り込めません: {:?}", record.message));
    }
    session
        .library()
        .db()
        .read(|c| {
            let mut stmt = c.prepare("SELECT original_name, id FROM items")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
        })
        .map_err(|e| e.to_string())
}

/// 値を `format`（`png`・`jpeg`）で書いて `path` に置く（アプリのコーデックの組。JPEG は q90）。
pub(crate) fn write_value(
    codecs: &CodecSet,
    value: &Value,
    format: &str,
    path: &Path,
) -> Result<(), String> {
    let opts = if format == "jpeg" {
        kura_engine::testing::bench::cases::save_jpeg()
    } else {
        json!({})
    };
    let (bytes, _) = codecs
        .encode_bytes(value, format, &opts, &NoMonitor)
        .map_err(|e| format!("{} を書けません: {e}", path.display()))?;
    std::fs::write(path, bytes).map_err(|e| format!("{} を書けません: {e}", path.display()))
}

/// 新しい作業の場所（`<work>/<name>-<pid>`。もうあれば断る：利用者のライブラリを指さないため）。
pub(crate) fn fresh_dir(work: &Path, name: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(work).map_err(|e| format!("{} を作れません: {e}", work.display()))?;
    let dir = work.join(format!("{name}-{}", std::process::id()));
    if dir.exists() {
        return Err(format!("{} にはもう何かあります", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 環境の記録（機械・OS・コミット・ビルド・スレッドの数・予算）。
pub(crate) fn env_json(extra: J) -> J {
    let cmd = |c: &str, a: &[&str]| {
        std::process::Command::new(c)
            .args(a)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let sysctl = |k: &str| cmd("sysctl", &["-n", k]);
    let budget = kura_engine::budget::default_total(kura_mac::physical_memory());
    let mut env = json!({
        "cpu": sysctl("machdep.cpu.brand_string").unwrap_or_else(|| "不明".into()),
        "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "perf_cores": sysctl("hw.perflevel0.physicalcpu").and_then(|s| s.parse::<u64>().ok()),
        "eff_cores": sysctl("hw.perflevel1.physicalcpu").and_then(|s| s.parse::<u64>().ok()),
        "memory_bytes": kura_mac::physical_memory(),
        "os": cmd("sw_vers", &["-productVersion"]),
        "commit": cmd("git", &["rev-parse", "--short", "HEAD"]),
        "dirty": cmd("git", &["status", "--porcelain"]).map(|s| !s.is_empty()),
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "budget_bytes": budget,
        "reserved_bytes": kura_engine::budget::default_reserved(budget),
        "loadavg": crate::bench_edit::loadavg(),
    });
    if let (Some(e), J::Object(x)) = (env.as_object_mut(), extra) {
        e.extend(x);
    }
    env
}

/// スレッドの数の並び（判定の数 `judged` と記録の数 `extra`。重なりは 1 つ）。
pub(crate) fn thread_set(judged: usize, extra: &[usize]) -> Vec<usize> {
    let mut s: Vec<usize> = extra.to_vec();
    s.push(judged);
    s.sort();
    s.dedup();
    s
}

/// `--threads 4,16` を読む。
pub(crate) fn thread_list(v: Option<&String>) -> Result<Vec<usize>, String> {
    let list = v.ok_or("--threads の値がありません")?;
    list.split(',')
        .map(|n| n.trim().parse::<usize>().ok().filter(|n| *n > 0))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| format!("--threads の値が読めません: {list}"))
}

/// 結果を書く：JSON（`--json`）と Markdown（`--markdown <file>`。`-` は標準出力だけ）。Markdown はいつも標準出力にも出す。
pub(crate) fn write_report(
    report: &Report,
    json_path: Option<&Path>,
    md_path: Option<&Path>,
) -> Result<(), String> {
    if let Some(path) = json_path {
        let text = serde_json::to_string_pretty(&report.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{} に書けません: {e}", path.display()))?;
    }
    let md = report.markdown();
    if let Some(p) = md_path
        && p.as_os_str() != "-"
    {
        std::fs::write(p, &md).map_err(|e| format!("{} に書けません: {e}", p.display()))?;
    }
    report::emit("");
    report::emit(&md);
    Ok(())
}

/// GC の時計（8 日進め、バックアップを数えない）：置き換えたばかりの版のファイルを片付けさせて、途中の版に戻すときの作り直し
/// を測るため（bench save だけ。アプリはいつも本物の時計）。
pub(crate) fn shifted_gc_clock() -> GcClock {
    const SHIFT: i64 = 8 * 24 * 60 * 60 * 1000;
    GcClock::new(
        Arc::new(|| kura_core::time::now_ms().saturating_add(SHIFT)),
        Arc::new(|_| Ok(Vec::new())),
    )
}

/// バイトを MiB に。
pub(crate) fn mib(b: u64) -> f64 {
    b as f64 / MIB as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_sets_put_the_judged_count_in_order() {
        assert_eq!(thread_set(3, &[16, 4, 3]), vec![3, 4, 16]);
        assert_eq!(thread_set(3, &[]), vec![3]);
        assert_eq!(thread_list(Some(&"4, 16".into())).unwrap(), vec![4, 16]);
        assert!(thread_list(Some(&"4,0".into())).is_err());
        assert!(thread_list(None).is_err());
    }

    #[test]
    fn fresh_dirs_refuse_existing_places() {
        let dir = tempfile::tempdir().unwrap();
        let a = fresh_dir(dir.path(), "x").unwrap();
        assert!(a.is_dir());
        assert!(fresh_dir(dir.path(), "x").is_err());
    }
}
