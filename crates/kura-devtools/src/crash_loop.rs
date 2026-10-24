//! 本当に落とす試験（M12 の段 2。docs/architecture.md §13、docs/editing.md §14.1）：
//! `edit crash-loop --library <path> [--runs 50] [--seed 1] [--min-ms 10] [--max-ms 500]`。
//!
//! 1. `--library` の場所に新しいライブラリを作る（もう何かあれば断る。本物のライブラリを指さないため。テンポラリや作業用の
//!    フォルダを渡す）。ドット絵の PNG を 1 つ、アプリと同じ取り込みのジョブで入れる。
//! 2. 回ごとに自分を子として起こす（`edit crash-loop-child`）。子はアプリと同じ渡しもの（MacEngineFactory：物理メモリの予算・
//!    アプリと同じコーデックの組）でライブラリを開き、保存を繰り返す（ふだんは上書き。版が 2 つ以上あれば 4 回に 1 回は版に
//!    戻す）。子だけ GC の時計を 8 日進めるので（`Services::with_gc_clock`。M12 の段 3）、開いたあと・保存のコミットのあとに
//!    GC が積まれ、置き換えたばかりの版を照合して消す（保存は 2 回に 1 回、GC が動き終えるのを待ってから次を積む。GC は保存に
//!    譲るので）。親は ChaCha8 で決めた 10〜500 ms 待ってから SIGKILL する。
//! 3. 親はライブラリを開き直し（ロック → staging の掃除 → 予告の照合 → 整合性チェック → ジョブの再開。親の時計は本物なので
//!    GC は版を消さない）、ジョブが動き終えるのを待って食い違いを数える：開いたときと終わったあとの整合性（DB にないファイル・
//!    ファイルのない台帳の行・参照されない台帳の行）、台帳のファイルの中身と hash、版の不変条件（`edit::versions::check_invariants`）、
//!    予告の行（`ledger_pending`）、staging のファイル、動いている・失敗した保存と GC のジョブ、照合が合わずに残した版（固定の
//!    段のコーデックなので 0 のはず）、結果が saved・unchanged なのに版のないジョブ、アイテムの数。
//! 4. 結果を 1 つの JSON で標準出力に書く（回ごとに、落とした直後の様子 `atCrash`：実行中の保存・GC のジョブ・台帳の予告
//!    （うち 'delete'）・staging の書きかけの数も。合計はその状態だった回の数）。食い違いがあれば失敗で終わる。子のログは
//!    `<ライブラリ>.crash-loop.log` に足す。

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_core::edit::{Base, GcClock, SaveAction, SaveRequest, start_save, versions};
use kura_core::import::{ImportInput, ImportOptions, hash_file, start_import};
use kura_core::jobs::JobState;
use kura_core::library::{IntegrityDepth, Library, LibrarySession, check_integrity};
use kura_engine::codec::CodecSet;
use kura_engine::testgen::InputGen;
use kura_engine::value::PixelFormat;
use kura_engine::{Graph, NoMonitor};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use serde_json::{Value as Json, json};

const USAGE: &str = "使い方: kura-devtools edit crash-loop --library <新しいライブラリの場所> [--runs 50] [--seed 1] [--min-ms 10] [--max-ms 500]";

/// 開き直してジョブが動き終えるのを待つ上限。
const SETTLE: Duration = Duration::from_secs(600);

/// 子の GC の時計を進める長さ（7 日の猶予を過ぎたものとして、置き換えたばかりの版を片付けさせる）。
const GC_SHIFT_MS: i64 = 8 * 24 * 60 * 60 * 1000;

/// ドット絵の幅・高さ（取り込む PNG と、塗りつぶしの位置の範囲）。
const W: u32 = 24;
const H: u32 = 20;

struct Opts {
    library: PathBuf,
    runs: u32,
    seed: u64,
    min_ms: u64,
    max_ms: u64,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts {
        library: PathBuf::new(),
        runs: 50,
        seed: 1,
        min_ms: 10,
        max_ms: 500,
    };
    let mut library = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{arg} の値がありません"))
        };
        let num = |v: String| v.parse::<u64>().map_err(|e| format!("{arg}: {e}"));
        match arg.as_str() {
            "--library" => library = Some(PathBuf::from(value()?)),
            "--runs" => {
                opts.runs = u32::try_from(num(value()?)?).map_err(|e| format!("--runs: {e}"))?;
            }
            "--seed" => opts.seed = num(value()?)?,
            "--min-ms" => opts.min_ms = num(value()?)?,
            "--max-ms" => opts.max_ms = num(value()?)?,
            other => return Err(format!("知らない引数 {other}。{USAGE}")),
        }
    }
    opts.library = library.ok_or(USAGE)?;
    if opts.min_ms > opts.max_ms {
        return Err("--min-ms は --max-ms 以下にしてください".into());
    }
    Ok(opts)
}

fn quiet() -> kura_core::library::EventSink {
    Arc::new(|_| {})
}

/// `edit crash-loop` の入口（親）。
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    let started = Instant::now();
    let root = create_library(&opts.library)?;
    let log = log_path(&root);
    let mut rng = ChaCha8Rng::seed_from_u64(opts.seed);
    let span = opts.max_ms - opts.min_ms + 1;
    let mut runs = Vec::new();
    let mut mismatches = 0usize;
    let mut failed_before = (0i64, 0i64);
    let mut early = 0u32;
    let mut at_crash = AtCrash::default();
    for run in 0..opts.runs {
        let child_seed = rng.next_u64();
        let delay = opts.min_ms + rng.next_u64() % span;
        let exited_early = crash_once(&root, child_seed, Duration::from_millis(delay), &log)?;
        if exited_early.is_some() {
            early += 1;
        }
        let at = probe(&root)?;
        at_crash.add(&at);
        let check = verify(&root)?;
        let mut issues = check.issues(failed_before);
        if let Some(status) = &exited_early {
            issues.push(format!("子が落とす前に終わりました（{status}）"));
        }
        failed_before = (check.failed_edit_jobs, check.failed_gc_jobs);
        mismatches += issues.len();
        tracing::info!(
            run,
            delay_ms = delay,
            versions = check.versions,
            issues = issues.len(),
            "落として開き直しました"
        );
        runs.push(json!({
            "run": run,
            "delayMs": delay,
            "childSeed": child_seed,
            "atCrash": at.json(),
            "versions": check.versions,
            "releasedVersions": check.released_versions,
            "editJobsDone": check.done_edit_jobs,
            "gcJobsDone": check.done_gc_jobs,
            "issues": issues,
        }));
    }
    let last = verify(&root)?;
    let report = json!({
        "library": root.display().to_string(),
        "runs": opts.runs,
        "seed": opts.seed,
        "delayMs": { "min": opts.min_ms, "max": opts.max_ms },
        "mismatches": mismatches,
        "runsWithMismatch": runs.iter().filter(|r| r["issues"].as_array().is_some_and(|i| !i.is_empty())).map(|r| r["run"].clone()).collect::<Vec<Json>>(),
        "childExitedEarly": early,
        "atCrash": at_crash.json(),
        "totals": {
            "versions": last.versions,
            "revertVersions": last.revert_versions,
            "editJobsDone": last.done_edit_jobs,
            "editJobsFailed": last.failed_edit_jobs,
            "gcJobsDone": last.done_gc_jobs,
            "gcJobsFailed": last.failed_gc_jobs,
            "releasedVersions": last.released_versions,
            "waitingVersions": last.waiting_versions,
            "keptVersions": last.kept_versions,
            "items": last.items,
        },
        "elapsedMs": started.elapsed().as_millis() as u64,
        "perRun": runs,
    });
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.write_all(b"\n"))
        .map_err(|e| e.to_string())?;
    if mismatches > 0 {
        return Err(format!("食い違いが {mismatches} 件ありました"));
    }
    Ok(())
}

/// 落とした直後（開き直す前）の様子：保存の途中だったか、台帳の予告・staging の書きかけが残っているか。回ごとの値と、
/// その状態だった回の数（どの段で落ちたかの散らばり）。
#[derive(Default)]
struct AtCrash {
    /// 実行中のまま残った保存のジョブ（保存の途中で落ちた）。
    running_edit: i64,
    /// 実行中のまま残った GC のジョブ（GC の途中で落ちた）。
    running_gc: i64,
    /// 台帳の予告の行（予告のあと・コミットの前で落ちた）。
    pending: i64,
    /// うち 'delete' の予告（GC が台帳から外したあと・ファイルを消して予告を消す前で落ちた）。
    pending_delete: i64,
    /// staging のファイル（書きかけ。描いて書いたあと・予告・配置の前など）。
    staging: i64,
}

impl AtCrash {
    fn add(&mut self, other: &AtCrash) {
        self.running_edit += i64::from(other.running_edit > 0);
        self.running_gc += i64::from(other.running_gc > 0);
        self.pending += i64::from(other.pending > 0);
        self.pending_delete += i64::from(other.pending_delete > 0);
        self.staging += i64::from(other.staging > 0);
    }

    fn json(&self) -> Json {
        json!({
            "runningEditJobs": self.running_edit,
            "runningGcJobs": self.running_gc,
            "ledgerPending": self.pending,
            "ledgerPendingDelete": self.pending_delete,
            "stagingFiles": self.staging,
        })
    }
}

/// 落とした直後の DB と staging を、ライブラリを開かずに見る（DB は WAL を戻すだけの接続で読む。整合性の確かめは
/// [`verify`] が開き直して行う）。
fn probe(root: &Path) -> Result<AtCrash, String> {
    let conn = rusqlite::Connection::open(root.join(kura_core::library::DB_FILE))
        .map_err(|e| e.to_string())?;
    let count = |sql: &str| -> Result<i64, String> {
        conn.query_row(sql, [], |r| r.get(0))
            .map_err(|e| e.to_string())
    };
    let mut staging = Vec::new();
    files_under(&root.join("staging"), &mut staging);
    Ok(AtCrash {
        running_edit: count("SELECT count(*) FROM jobs WHERE kind = 'edit' AND state = 'running'")?,
        running_gc: count("SELECT count(*) FROM jobs WHERE kind = 'gc' AND state = 'running'")?,
        pending: count("SELECT count(*) FROM ledger_pending")?,
        pending_delete: count("SELECT count(*) FROM ledger_pending WHERE kind = 'delete'")?,
        staging: staging.len() as i64,
    })
}

/// 子のログの置き場（ライブラリの隣）。
fn log_path(root: &Path) -> PathBuf {
    let mut name = root.as_os_str().to_owned();
    name.push(".crash-loop.log");
    PathBuf::from(name)
}

/// 新しいライブラリを作り、ドット絵の PNG を 1 つ取り込む。ライブラリの根の場所を返す。
fn create_library(path: &Path) -> Result<PathBuf, String> {
    let root = kura_core::library::normalize_library_path(path);
    if path.exists() || root.exists() {
        return Err(format!(
            "{} にはもう何かあります。crash-loop は新しい場所にライブラリを作ります（本物のライブラリを指さないでください）",
            root.display()
        ));
    }
    let session = LibrarySession::create(path, quiet(), crate::services())
        .map_err(|e| format!("ライブラリを作れません: {e}"))?;
    session.wait_ready();
    let root = session.library().layout().root().to_path_buf();
    let src = root.with_extension("crash-loop-src");
    std::fs::create_dir_all(&src).map_err(|e| e.to_string())?;
    let value = InputGen::PixelSprite {
        w: W,
        h: H,
        fmt: PixelFormat::Rgba8,
        seed: 12,
    }
    .generate()
    .map_err(|e| e.to_string())?;
    let (png, _) = CodecSet::pinned()
        .encode_bytes(&value, "png", &json!({}), &NoMonitor)
        .map_err(|e| e.to_string())?;
    std::fs::write(src.join("sprite.png"), png).map_err(|e| e.to_string())?;
    let id = start_import(
        session.jobs(),
        &ImportInput {
            paths: vec![src.clone()],
            options: ImportOptions::default(),
        },
    )
    .map_err(|e| e.to_string())?;
    if !session.jobs().wait_idle(SETTLE) {
        return Err("取り込みが終わりません".into());
    }
    let record = session.jobs().get(id).map_err(|e| e.to_string())?;
    if record.state != JobState::Done {
        return Err(format!("取り込めません: {:?}", record.message));
    }
    drop(session);
    std::fs::remove_dir_all(&src).map_err(|e| e.to_string())?;
    Ok(root)
}

/// 子を起こし、`delay` 待って SIGKILL する。子が先に終わっていれば、その終わり方を返す。
fn crash_once(
    root: &Path,
    seed: u64,
    delay: Duration,
    log: &Path,
) -> Result<Option<String>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args(["edit", "crash-loop-child", "--library"])
        .arg(root)
        .args(["--seed", &seed.to_string()])
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("子を起こせません: {e}"))?;
    std::thread::sleep(delay);
    if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
        return Ok(Some(status.to_string()));
    }
    // Unix では SIGKILL（後始末をさせない）
    child.kill().map_err(|e| format!("子を落とせません: {e}"))?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(None)
}

/// 開き直して確かめた結果。
struct Check {
    open: [u64; 3],
    after: [u64; 3],
    bad_blobs: Vec<String>,
    invariants: Vec<String>,
    pending: i64,
    staging: Vec<String>,
    live_edit_jobs: i64,
    failed_edit_jobs: i64,
    done_edit_jobs: i64,
    live_gc_jobs: i64,
    failed_gc_jobs: i64,
    done_gc_jobs: i64,
    uncommitted: i64,
    versions: i64,
    revert_versions: i64,
    released_versions: i64,
    waiting_versions: i64,
    kept_versions: i64,
    items: i64,
}

impl Check {
    /// 食い違いの文（`failed_before` は前の回までに失敗した保存・GC のジョブの数）。
    fn issues(&self, failed_before: (i64, i64)) -> Vec<String> {
        let mut out = Vec::new();
        for (when, [missing, orphan, unreferenced]) in
            [("開いたとき", self.open), ("終わったあと", self.after)]
        {
            if missing + orphan + unreferenced > 0 {
                out.push(format!(
                    "{when}の整合性：ファイルのない台帳の行 {missing}・DB にないファイル {orphan}・参照されない台帳の行 {unreferenced}"
                ));
            }
        }
        out.extend(self.bad_blobs.iter().cloned());
        out.extend(self.invariants.iter().map(|m| format!("不変条件：{m}")));
        if self.pending != 0 {
            out.push(format!("台帳の予告が {} 行残っています", self.pending));
        }
        out.extend(self.staging.iter().map(|p| format!("staging に残骸：{p}")));
        if self.live_edit_jobs != 0 {
            out.push(format!(
                "動き終えていない保存のジョブが {} 本",
                self.live_edit_jobs
            ));
        }
        if self.failed_edit_jobs > failed_before.0 {
            out.push(format!(
                "失敗した保存のジョブが {} 本増えました",
                self.failed_edit_jobs - failed_before.0
            ));
        }
        if self.live_gc_jobs != 0 {
            out.push(format!(
                "動き終えていない GC のジョブが {} 本",
                self.live_gc_jobs
            ));
        }
        if self.failed_gc_jobs > failed_before.1 {
            out.push(format!(
                "失敗した GC のジョブが {} 本増えました",
                self.failed_gc_jobs - failed_before.1
            ));
        }
        if self.kept_versions != 0 {
            out.push(format!(
                "照合が合わずに残した版が {} 個（固定の段のコーデックなので 0 のはず）",
                self.kept_versions
            ));
        }
        if self.uncommitted != 0 {
            out.push(format!(
                "保存したと答えたのに版のないジョブが {} 本",
                self.uncommitted
            ));
        }
        if self.items != 1 {
            out.push(format!("アイテムが {} 個（1 個のはず）", self.items));
        }
        out
    }
}

fn count(lib: &Library, sql: &str) -> Result<i64, String> {
    lib.db()
        .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .map_err(|e| e.to_string())
}

/// staging の中のファイル（入れ子を含む）。
fn files_under(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path.display().to_string());
        }
    }
}

/// ライブラリを開き直し、ジョブが動き終えるのを待って確かめる。
fn verify(root: &Path) -> Result<Check, String> {
    let session = LibrarySession::open(root, quiet(), crate::services())
        .map_err(|e| format!("開き直せません: {e}"))?;
    session.wait_ready();
    let open = session
        .integrity()
        .map(|r| [r.missing_files, r.orphan_files, r.unreferenced_blobs])
        .unwrap_or([u64::MAX; 3]);
    if !session.jobs().wait_idle(SETTLE) {
        return Err("開き直したあと、ジョブが動き終えません".into());
    }
    let lib = session.library();
    let report = check_integrity(lib.db(), lib.layout(), 0, IntegrityDepth::Startup)
        .map_err(|e| e.to_string())?;
    let blobs: Vec<(Vec<u8>, String)> = lib
        .db()
        .read(|c| {
            let mut stmt = c.prepare("SELECT hash, ext FROM blobs")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .map_err(|e| e.to_string())?;
    let mut bad_blobs = Vec::new();
    for (hash, ext) in blobs {
        let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) else {
            bad_blobs.push("台帳の hash が 32 バイトではありません".into());
            continue;
        };
        let path = lib.layout().content_path(&hash, &ext);
        match hash_file(&path) {
            Ok(h) if h == hash => {}
            Ok(_) => bad_blobs.push(format!("中身が hash と違います：{}", path.display())),
            Err(e) => bad_blobs.push(format!("読めません：{}（{e}）", path.display())),
        }
    }
    let invariants = lib
        .db()
        .read(versions::check_invariants)
        .map_err(|e| e.to_string())?;
    let mut staging = Vec::new();
    files_under(&lib.layout().staging_dir(), &mut staging);
    let check = Check {
        open,
        after: [
            report.missing_files,
            report.orphan_files,
            report.unreferenced_blobs,
        ],
        bad_blobs,
        invariants,
        pending: count(lib, "SELECT count(*) FROM ledger_pending")?,
        staging,
        live_edit_jobs: count(
            lib,
            "SELECT count(*) FROM jobs WHERE kind = 'edit' AND state NOT IN ('done', 'failed', 'canceled')",
        )?,
        failed_edit_jobs: count(
            lib,
            "SELECT count(*) FROM jobs WHERE kind = 'edit' AND state = 'failed'",
        )?,
        done_edit_jobs: count(
            lib,
            "SELECT count(*) FROM jobs WHERE kind = 'edit' AND state = 'done'",
        )?,
        live_gc_jobs: count(
            lib,
            "SELECT count(*) FROM jobs WHERE kind = 'gc' AND state NOT IN ('done', 'failed', 'canceled')",
        )?,
        failed_gc_jobs: count(
            lib,
            "SELECT count(*) FROM jobs WHERE kind = 'gc' AND state = 'failed'",
        )?,
        done_gc_jobs: count(
            lib,
            "SELECT count(*) FROM jobs WHERE kind = 'gc' AND state = 'done'",
        )?,
        uncommitted: count(
            lib,
            "SELECT count(*) FROM jobs j WHERE j.kind = 'edit' AND j.state = 'done'
                AND json_extract(j.result_json, '$.outcome') IN ('saved', 'unchanged')
                AND NOT EXISTS (SELECT 1 FROM item_versions v WHERE v.job_id = j.id)",
        )?,
        versions: count(lib, "SELECT count(*) FROM item_versions")?,
        revert_versions: count(
            lib,
            "SELECT count(*) FROM item_versions WHERE kind = 'revert'",
        )?,
        released_versions: count(
            lib,
            "SELECT count(*) FROM item_versions WHERE file_state = 'released'",
        )?,
        waiting_versions: count(
            lib,
            "SELECT count(*) FROM item_versions WHERE file_state = 'waiting'",
        )?,
        kept_versions: count(
            lib,
            "SELECT count(*) FROM item_versions WHERE file_state = 'kept'",
        )?,
        items: count(lib, "SELECT count(*) FROM items")?,
    };
    drop(session);
    Ok(check)
}

/// 塗りつぶし（と、ときどき反転）の文書（元に対する累積。色・位置は `rng` で決める）。
fn random_doc(rng: &mut ChaCha8Rng) -> Result<Graph, String> {
    let mut g = Graph::new_single();
    if rng.next_u32().is_multiple_of(2) {
        let axis = if rng.next_u32().is_multiple_of(2) {
            "vertical"
        } else {
            "horizontal"
        };
        g.push_linear("image.flip", 1, json!({ "axis": axis }), 0)
            .map_err(|e| e.to_string())?;
    }
    let actions: Vec<Json> = (0..1 + rng.next_u32() % 3)
        .map(|_| {
            let rgba = [
                rng.next_u32() % 256,
                rng.next_u32() % 256,
                rng.next_u32() % 256,
                255,
            ];
            json!({
                "type": "fill", "at": { "x": rng.next_u32() % W, "y": rng.next_u32() % H },
                "color": { "rgba": rgba }, "tolerance": 30, "connectivity": "eight",
                "contiguous": true, "frame": 0,
            })
        })
        .collect();
    g.push_linear("paint.actions", 1, json!({ "actions": actions }), 0)
        .map_err(|e| e.to_string())?;
    Ok(g)
}

/// `edit crash-loop-child`（子）：保存を繰り返す（親が SIGKILL するまで）。GC の時計だけ 8 日進めて開く。
pub fn child(args: &[String]) -> Result<(), String> {
    let mut library = None;
    let mut seed = 0u64;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--library" => library = it.next().map(PathBuf::from),
            "--seed" => {
                seed = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--seed の値がありません")?;
            }
            other => return Err(format!("知らない引数 {other}")),
        }
    }
    let library = library.ok_or("--library がありません")?;
    let services = crate::services().with_gc_clock(GcClock::shifted(GC_SHIFT_MS));
    let session = LibrarySession::open(&library, quiet(), services)
        .map_err(|e| format!("開けません: {e}"))?;
    session.wait_ready();
    let lib = session.library().clone();
    let item = count(&lib, "SELECT min(id) FROM items")?;
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    loop {
        let head = lib
            .db()
            .read(|c| versions::head(c, item))
            .map_err(|e| e.to_string())?
            .ok_or("アイテムがありません")?;
        let base = Base {
            hash: head.hash,
            version: head.version,
        };
        let action = if head.version >= 2 && rng.next_u32().is_multiple_of(4) {
            SaveAction::Revert {
                to: i64::from(rng.next_u32()) % head.version,
            }
        } else {
            SaveAction::Overwrite {
                graph: random_doc(&mut rng)?,
            }
        };
        let id = start_save(
            lib.db(),
            session.jobs(),
            &SaveRequest {
                item_id: item,
                base,
                action,
            },
        )
        .map_err(|e| e.to_string())?;
        loop {
            let record = session.jobs().get(id).map_err(|e| e.to_string())?;
            if record.state.is_terminal() {
                tracing::info!(
                    id,
                    state = record.state.as_str(),
                    result = record.result_json.as_deref().unwrap_or(""),
                    "保存"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // 2 回に 1 回は、保存のコミットのあとに積まれた GC が動き終えるのを待つ（GC は保存に譲るので、続けて保存を積むと
        // 動けない）
        if rng.next_u32().is_multiple_of(2) {
            let until = Instant::now() + Duration::from_secs(5);
            while count(
                &lib,
                "SELECT count(*) FROM jobs WHERE kind = 'gc' AND state IN ('queued', 'running')",
            )? > 0
                && Instant::now() < until
            {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}
