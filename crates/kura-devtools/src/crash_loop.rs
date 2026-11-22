//! 本当に落とす試験（M12 の段 2。docs/architecture.md §13、docs/editing.md §14.1）：
//! `edit crash-loop --library <path> [--runs 50] [--seed 1] [--min-ms 10] [--max-ms 500] [--audio-minutes 0] [--batch [N] [--save-as]]`。
//!
//! `--batch [N]`（M14 段 4。N の既定は 1,000）を付けると、保存の代わりに一括加工のジョブ（`JobKind::Batch`）を落とす：N 個の
//! ドット絵の PNG と、レシピ「トリム・4 倍」を 1 つ入れたライブラリで、子は全件に上書きの一括を積む（あれば再開したものを
//! 動かす）。落としては開き直し、どのアイテムも一括の版が 1 つまで・済んだ行は版を指す・pending の行に版がない・頭の中身が
//! 期待どおり、などを数え、最後に一括を終わらせて全件ちょうど 1 回を確かめる。`--audio-minutes` とは併せられない。
//! `--save-as`（`--batch` と一緒のときだけ）を足すと、子は上書きの代わりに別名の一括（名前の型は既定の「{名前} のコピー」）を
//! 積み、確かめを別名に合わせる（元のアイテムは変わらない・どの元にもコピーがちょうど 1 つ・saved の行がそのコピーを指す、など）。
//! 流れと確かめの一覧は [`batch`] の冒頭。
//!
//! `--audio-minutes n`（M13 段 3。既定 0。確かめるときは 6）を付けると、親は n 分の合成の FLAC（44.1 kHz ステレオ）も
//! 取り込み、子は画像と音声を交互に保存する（音声はランダムな gain・fade・normalize・trim を、flac・ogg・wav に流して書く。
//! 作業用の PCM を通る）。音声の保存は長いので、待つ上限は max(`--max-ms`, n × 1,000 ms) に広げる。確かめに「開き直したあと
//! `staging/pcm` が空」と「アイテムが 2 個」を足す。子には音声の長さ（n × 60 × 44,100 フレーム）を `--audio-frames` で渡す
//! （ランダムな trim の位置を決める。音声のアイテムがあるのに渡されなければ子は失敗する。M13 段 3 の検証の直し）。
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
use std::process::{Command, ExitStatus, Stdio};
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

mod batch;

const USAGE: &str = "使い方: kura-devtools edit crash-loop --library <新しいライブラリの場所> [--runs 50] [--seed 1] [--min-ms 10] [--max-ms 500] [--audio-minutes 0] [--batch [N] [--save-as]]";

/// `--batch` の件数を省いたときの件数（NFR-14 と同じ 1,000 件）。
const DEFAULT_BATCH_ITEMS: usize = 1_000;

/// 取り込む合成の FLAC のレート。
const AUDIO_RATE: u32 = 44_100;

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
    /// 取り込む合成の音声の長さ（分。0 なら音声を入れない）。
    audio_minutes: u64,
    /// 一括加工を落とすときのアイテムの数（`--batch`。None なら保存を落とす）。
    batch: Option<usize>,
    /// 一括を別名で積む（`--save-as`。`--batch` のときだけ。偽なら上書き）。
    save_as: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts {
        library: PathBuf::new(),
        runs: 50,
        seed: 1,
        min_ms: 10,
        max_ms: 500,
        audio_minutes: 0,
        batch: None,
        save_as: false,
    };
    let mut library = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        // `--batch` の件数は省ける（次が数でなければ既定の件数）
        if arg == "--batch" {
            let n = match args.get(i).and_then(|v| v.parse::<usize>().ok()) {
                Some(n) => {
                    i += 1;
                    n
                }
                None => DEFAULT_BATCH_ITEMS,
            };
            if n == 0 {
                return Err("--batch は 1 件以上にしてください".into());
            }
            opts.batch = Some(n);
            continue;
        }
        if arg == "--save-as" {
            opts.save_as = true;
            continue;
        }
        let mut value = || {
            let v = args
                .get(i)
                .cloned()
                .ok_or_else(|| format!("{arg} の値がありません"));
            i += 1;
            v
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
            "--audio-minutes" => opts.audio_minutes = num(value()?)?,
            other => return Err(format!("知らない引数 {other}。{USAGE}")),
        }
    }
    opts.library = library.ok_or(USAGE)?;
    if opts.min_ms > opts.max_ms {
        return Err("--min-ms は --max-ms 以下にしてください".into());
    }
    if opts.batch.is_some() && opts.audio_minutes > 0 {
        return Err("--batch と --audio-minutes は併せられません".into());
    }
    if opts.save_as && opts.batch.is_none() {
        return Err("--save-as は --batch と一緒に使ってください".into());
    }
    Ok(opts)
}

fn quiet() -> kura_core::library::EventSink {
    Arc::new(|_| {})
}

/// `edit crash-loop` の入口（親）。
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    if let Some(n) = opts.batch {
        return batch::run(&opts, n);
    }
    let started = Instant::now();
    let root = create_library(&opts.library, opts.audio_minutes)?;
    let log = log_path(&root);
    let mut rng = ChaCha8Rng::seed_from_u64(opts.seed);
    // 音声の保存は長い（作業用の PCM・流して書く）ので、落とす時刻の上限を広げる
    let max_ms = if opts.audio_minutes > 0 {
        opts.max_ms.max(opts.audio_minutes.saturating_mul(1_000))
    } else {
        opts.max_ms
    };
    let span = max_ms - opts.min_ms + 1;
    let want_items = if opts.audio_minutes > 0 { 2 } else { 1 };
    let frames = audio_frames(opts.audio_minutes);
    let child_extra: Vec<String> = if frames > 0 {
        vec!["--audio-frames".into(), frames.to_string()]
    } else {
        Vec::new()
    };
    let mut runs = Vec::new();
    let mut mismatches = 0usize;
    let mut failed_before = (0i64, 0i64);
    let mut early = 0u32;
    let mut at_crash = AtCrash::default();
    for run in 0..opts.runs {
        let child_seed = rng.next_u64();
        let delay = opts.min_ms + rng.next_u64() % span;
        let exited_early = crash_once(
            &root,
            child_seed,
            &child_extra,
            Duration::from_millis(delay),
            &log,
        )?;
        if exited_early.is_some() {
            early += 1;
        }
        let at = probe(&root)?;
        at_crash.add(&at);
        let check = verify(&root)?;
        let mut issues = check.issues(failed_before, want_items);
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
        "delayMs": { "min": opts.min_ms, "max": max_ms },
        "audioMinutes": opts.audio_minutes,
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

/// 新しいライブラリを作り、ドット絵の PNG を 1 つ（`audio_minutes` が 0 でなければ、その長さの合成の FLAC も）取り込む。
/// ライブラリの根の場所を返す。
fn create_library(path: &Path, audio_minutes: u64) -> Result<PathBuf, String> {
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
    if audio_minutes > 0 {
        let audio = InputGen::AudioNoise {
            rate: AUDIO_RATE,
            channels: 2,
            frames: audio_minutes * 60 * u64::from(AUDIO_RATE),
            seed: 13,
        }
        .generate()
        .map_err(|e| e.to_string())?;
        let (flac, _) = CodecSet::pinned()
            .encode_bytes(&audio, "flac", &json!({ "bits": 16 }), &NoMonitor)
            .map_err(|e| e.to_string())?;
        std::fs::write(src.join("noise.flac"), flac).map_err(|e| e.to_string())?;
    }
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

/// 取り込む合成の音声の長さ（フレーム。`minutes` 分の 44.1 kHz）。
fn audio_frames(minutes: u64) -> u64 {
    minutes
        .saturating_mul(60)
        .saturating_mul(u64::from(AUDIO_RATE))
}

/// 子を起こし（`extra` は子に足す引数：音声の長さ `--audio-frames n`、一括の `--batch`・`--save-as`）、`delay` 待って SIGKILL する。子が
/// 先に終わっていれば、その終わり方を返す。
fn crash_once(
    root: &Path,
    seed: u64,
    extra: &[String],
    delay: Duration,
    log: &Path,
) -> Result<Option<ExitStatus>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    command
        .args(["edit", "crash-loop-child", "--library"])
        .arg(root)
        .args(["--seed", &seed.to_string()])
        .args(extra);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("子を起こせません: {e}"))?;
    std::thread::sleep(delay);
    if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
        return Ok(Some(status));
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
    /// `staging/pcm`（作業用の PCM）に残ったファイル。
    work_pcm: Vec<String>,
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
    /// 食い違いの文（`failed_before` は前の回までに失敗した保存・GC のジョブの数、`want_items` はアイテムの数）。
    fn issues(&self, failed_before: (i64, i64), want_items: i64) -> Vec<String> {
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
        out.extend(
            self.work_pcm
                .iter()
                .map(|p| format!("開き直したあとの staging/pcm に残骸：{p}")),
        );
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
        if self.items != want_items {
            out.push(format!(
                "アイテムが {} 個（{want_items} 個のはず）",
                self.items
            ));
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

/// 台帳のどの行も、ファイルの中身が hash と同じか（違う・読めない行の文を返す）。
fn blob_issues(lib: &Library) -> Result<Vec<String>, String> {
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
    Ok(bad_blobs)
}

/// ライブラリを開き直し、ジョブが動き終えるのを待って確かめる。
fn verify(root: &Path) -> Result<Check, String> {
    let session = LibrarySession::open(root, quiet(), crate::services())
        .map_err(|e| format!("開き直せません: {e}"))?;
    session.wait_ready();
    // 開いたときの staging の掃除で、前の子の作業用の PCM も消えている（ジョブを動かす前に見る）
    let mut work_pcm = Vec::new();
    files_under(
        &session.library().layout().staging_dir().join("pcm"),
        &mut work_pcm,
    );
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
    let bad_blobs = blob_issues(lib)?;
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
        work_pcm,
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

/// 音声の文書（元に対する累積：ランダムな trim・gain・fade・normalize と、出力の形式 flac・ogg・wav。長さ `frames`）。
fn random_audio_doc(rng: &mut ChaCha8Rng, frames: u64) -> Result<Graph, String> {
    let mut g = Graph::new_single();
    let pick = |rng: &mut ChaCha8Rng, n: u64| u64::from(rng.next_u32()) % n.max(1);
    if rng.next_u32().is_multiple_of(2) {
        let start = pick(rng, frames / 4);
        g.push_linear("audio.trim", 1, json!({ "start": start, "end": null }), 0)
            .map_err(|e| e.to_string())?;
    }
    g.push_linear(
        "audio.gain",
        1,
        json!({ "db": (pick(rng, 120) as f64 - 60.0) / 10.0 }),
        0,
    )
    .map_err(|e| e.to_string())?;
    if rng.next_u32().is_multiple_of(2) {
        let r = u64::from(AUDIO_RATE);
        g.push_linear(
            "audio.fade",
            1,
            json!({ "fadeIn": pick(rng, 5 * r), "fadeOut": pick(rng, 5 * r), "curve": "linear" }),
            0,
        )
        .map_err(|e| e.to_string())?;
    }
    if rng.next_u32().is_multiple_of(3) {
        g.push_linear(
            "audio.normalize",
            1,
            json!({ "target": -(14.0 + pick(rng, 100) as f64 / 10.0), "peakLimit": -1.0 }),
            0,
        )
        .map_err(|e| e.to_string())?;
    }
    let format = ["flac", "ogg", "wav"][pick(rng, 3) as usize];
    g.outputs[0].format = kura_engine::doc::FormatChoice::To {
        format: format.into(),
        options: json!({}),
    };
    Ok(g)
}

/// 子の引数。
#[derive(Debug, PartialEq)]
struct ChildArgs {
    library: PathBuf,
    seed: u64,
    /// 音声のアイテムの長さ（フレーム。親が渡す。0 は渡されていない）。
    audio_frames: u64,
    /// 保存の代わりに一括加工を動かす（`--batch`。親の `--batch` のとき）。
    batch: bool,
    /// 一括を別名で積む（`--save-as`。親の `--save-as` のとき。`--batch` と一緒のときだけ）。
    save_as: bool,
}

fn child_args(args: &[String]) -> Result<ChildArgs, String> {
    let mut library = None;
    let mut seed = 0u64;
    let mut audio_frames = 0u64;
    let mut batch = false;
    let mut save_as = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--library" => library = it.next().map(PathBuf::from),
            "--batch" => batch = true,
            "--save-as" => save_as = true,
            "--seed" => {
                seed = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--seed の値がありません")?;
            }
            "--audio-frames" => {
                audio_frames = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|n| *n > 0)
                    .ok_or("--audio-frames の値がありません（1 以上）")?;
            }
            other => return Err(format!("知らない引数 {other}")),
        }
    }
    if batch && audio_frames > 0 {
        return Err("--batch と --audio-frames は併せられません".into());
    }
    if save_as && !batch {
        return Err("--save-as は --batch と一緒に渡してください".into());
    }
    Ok(ChildArgs {
        library: library.ok_or("--library がありません")?,
        seed,
        audio_frames,
        batch,
        save_as,
    })
}

/// `edit crash-loop-child`（子）：保存を繰り返す（親が SIGKILL するまで）。GC の時計だけ 8 日進めて開く。音声のアイテムが
/// あれば、画像と音声を交互に保存する（音声の長さは親が `--audio-frames` で渡す）。`--batch` なら一括加工を動かす
/// （[`batch::child`]。`--save-as` なら別名の一括）。
pub fn child(args: &[String]) -> Result<(), String> {
    let ChildArgs {
        library,
        seed,
        audio_frames,
        batch,
        save_as,
    } = child_args(args)?;
    if batch {
        return batch::child(&library, seed, save_as);
    }
    let services = crate::services().with_gc_clock(GcClock::shifted(GC_SHIFT_MS));
    let session = LibrarySession::open(&library, quiet(), services)
        .map_err(|e| format!("開けません: {e}"))?;
    session.wait_ready();
    let lib = session.library().clone();
    let image = count(&lib, "SELECT min(id) FROM items WHERE kind = 'image'")?;
    let audio = count(
        &lib,
        "SELECT coalesce(min(id), 0) FROM items WHERE kind = 'audio'",
    )?;
    // 音声の長さは親が渡す（読めないのに 1 秒とみなすと、trim がいつも頭の 0.25 秒になる）
    if audio > 0 && audio_frames == 0 {
        return Err("音声のアイテムがあるのに、長さ（--audio-frames）が渡されていません".into());
    }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut turn = 0u64;
    loop {
        turn += 1;
        let on_audio = audio > 0 && turn.is_multiple_of(2);
        let item = if on_audio { audio } else { image };
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
                graph: if on_audio {
                    random_audio_doc(&mut rng, audio_frames)?
                } else {
                    random_doc(&mut rng)?
                },
            }
        };
        let id = match start_save(
            lib.db(),
            session.jobs(),
            &SaveRequest {
                item_id: item,
                base,
                action,
            },
        ) {
            Ok(id) => id,
            // 前回落ちたときの保存が、開き直して再開されて待っている（同じアイテムの保存は 1 つずつ）：終わるのを待って積み直す
            Err(kura_core::CoreError::Busy(_)) => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// 子の引数：音声の長さは `--audio-frames`（1 以上）。渡されなければ 0（音声のアイテムがあれば子が失敗する）。
    #[test]
    fn child_takes_the_audio_length_from_the_parent() {
        let got = child_args(&args(&[
            "--library",
            "/tmp/x.kuralib",
            "--seed",
            "7",
            "--audio-frames",
            "15876000",
        ]))
        .unwrap();
        assert_eq!(
            got,
            ChildArgs {
                library: PathBuf::from("/tmp/x.kuralib"),
                seed: 7,
                audio_frames: 15_876_000,
                batch: false,
                save_as: false,
            }
        );
        assert_eq!(audio_frames(6), 15_876_000);
        assert_eq!(audio_frames(0), 0);
        let none = child_args(&args(&["--library", "/tmp/x.kuralib"])).unwrap();
        assert_eq!(none.audio_frames, 0);
        for bad in [
            &["--audio-frames"][..],
            &["--audio-frames", "0"],
            &["--audio-frames", "x"],
        ] {
            assert!(child_args(&args(bad)).is_err(), "{bad:?}");
        }
    }

    /// `--batch` の件数は省ける（既定 1,000。次の引数が数でなければ件数として読まない）。0 件と、音声との併用は断る。子には
    /// `--batch` だけを渡す。
    #[test]
    fn batch_takes_an_optional_count() {
        let base = ["--library", "/tmp/x.kuralib"];
        let with = |more: &[&str]| {
            let mut list = base.to_vec();
            list.extend_from_slice(more);
            parse(&args(&list))
        };
        assert_eq!(with(&[]).unwrap().batch, None);
        assert_eq!(with(&["--batch"]).unwrap().batch, Some(DEFAULT_BATCH_ITEMS));
        let opts = with(&["--batch", "--runs", "3"]).unwrap();
        assert_eq!((opts.batch, opts.runs), (Some(DEFAULT_BATCH_ITEMS), 3));
        let opts = with(&["--batch", "100", "--max-ms", "900"]).unwrap();
        assert_eq!((opts.batch, opts.max_ms), (Some(100), 900));
        assert!(with(&["--batch", "0"]).is_err());
        assert!(with(&["--batch", "5", "--audio-minutes", "1"]).is_err());
        assert!(with(&["--runs"]).is_err());
        let child = child_args(&args(&["--library", "/tmp/x.kuralib", "--batch"])).unwrap();
        assert!(child.batch && !child.save_as);
        assert!(!with(&["--batch"]).unwrap().save_as);
        assert!(
            child_args(&args(&[
                "--library",
                "/tmp/x.kuralib",
                "--batch",
                "--audio-frames",
                "9"
            ]))
            .is_err()
        );
    }

    /// `--save-as` は `--batch` と一緒のときだけ（件数の前後どちらでも。件数を省いても）。子にも `--batch` と一緒に渡す。
    #[test]
    fn save_as_goes_with_batch() {
        let base = ["--library", "/tmp/x.kuralib"];
        let with = |more: &[&str]| {
            let mut list = base.to_vec();
            list.extend_from_slice(more);
            parse(&args(&list))
        };
        for more in [
            &["--batch", "--save-as"][..],
            &["--save-as", "--batch"],
            &["--batch", "50", "--save-as"],
        ] {
            let opts = with(more).unwrap();
            assert!(opts.save_as && opts.batch.is_some(), "{more:?}");
        }
        assert_eq!(with(&["--save-as", "--batch", "7"]).unwrap().batch, Some(7));
        assert!(with(&["--save-as"]).is_err());
        let child = child_args(&args(&[
            "--library",
            "/tmp/x.kuralib",
            "--batch",
            "--save-as",
        ]))
        .unwrap();
        assert!(child.batch && child.save_as);
        assert!(child_args(&args(&["--library", "/tmp/x.kuralib", "--save-as"])).is_err());
    }

    /// 音声の文書の trim は、渡した長さの 1/4 までの位置から始まる（6 分なら 0.25 秒を大きく超える位置も出る）。
    #[test]
    fn audio_trim_spans_the_given_length() {
        let frames = audio_frames(6);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let mut starts = Vec::new();
        for _ in 0..64 {
            let g = random_audio_doc(&mut rng, frames).unwrap();
            for n in &g.nodes {
                if n.op == "audio.trim" {
                    starts.push(n.params["start"].as_u64().unwrap());
                }
            }
        }
        assert!(!starts.is_empty());
        assert!(starts.iter().all(|s| *s < frames / 4), "{starts:?}");
        assert!(
            starts.iter().any(|s| *s > 60 * u64::from(AUDIO_RATE)),
            "{starts:?}"
        );
    }
}
