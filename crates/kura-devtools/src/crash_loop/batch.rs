//! 一括加工を本当に落とす試験（M14 段 4。`edit crash-loop --batch [N] [--save-as]`。docs/editing.md §14「一括加工」、
//! ADR-190・192・193）：一括のジョブ（`JobKind::Batch`。`kura_core::edit::batch`）が、何度 SIGKILL されても、どのアイテムも
//! ちょうど 1 回だけ処理することを確かめる。向きは上書き（既定）と別名（`--save-as`。名前の型は既定の
//! `DEFAULT_SAVE_AS_NAME`「{名前} のコピー」）。
//!
//! 1. `--library` の場所に新しいライブラリを作る（もう何かあれば断る）。ドット絵の PNG を N 個（種 1, 2, …。元の中身・期待する
//!    結果の中身がほかと重なる種は飛ばす）アプリと同じ取り込みのジョブで入れ、レシピ「トリム・4 倍」（`image.trim_alpha` →
//!    `image.resize` 4 倍・nearest。NFR-14 と同じ手）を 1 つ DB に足す（`edit::recipes::insert`。`recipes::get` で当てられる
//!    ことも確かめる）。アイテムごとに、固定の段のコーデックとエンジンで 1 回当てた結果の中身（期待する頭）を先に求めておく。
//! 2. 回ごとに子（`edit crash-loop-child --batch [--save-as]`）を起こす。子はアプリと同じ渡しもの（MacEngineFactory：アプリと
//!    同じコーデックの組・`MacDurability`。GC の時計は保存の子と同じく 8 日進める）で開き、一括のジョブがなければ全件に上書き
//!    （`--save-as` なら別名）の一括を積み（`start_batch`。下書きは含めない）、あれば開いたときに再開したジョブが動くに任せる。
//!    ジョブが動き終えたら子は終わる。親は ChaCha8 で決めた `--min-ms`〜`--max-ms` 待って SIGKILL する。一括は速い（debug の
//!    組み立てでも、子を起こしてから最初のコミットまで約 200〜250 ms、そのあと 1,000 件を約 1.6〜1.8 秒。別名も上書きと
//!    ほぼ同じ速さ）。コミットは 32 件のまとまりごとなので、1 回に進む行は落とす時刻に対して段になる（約 300 ms で 64 行、
//!    約 360 ms で 96 行）。1,000 件・30 回なら `--min-ms 200 --max-ms 330` くらいで落とす時刻が一括の初め・中・終わりに
//!    散らばる（`--max-ms` を広げると早い回で終わり、残りの回は終わったあとに落とす。390 では 15〜20 回目で終わった）。
//! 3. 親は落とした直後の様子（`atCrash`：一括のジョブの状態・済んだ行と pending の行・台帳の予告・staging の書きかけ）を
//!    ライブラリを開かずに見てから、エンジンの部品を渡さずに開き直す（ロック → staging の掃除 → 予告の照合 → 整合性チェック →
//!    ジョブの再開。一括の処理を登録しないので、一括のジョブは待ちに戻るだけで親では進まない。続けるのは次の子）。動くジョブ
//!    （サムネイル）が終わるのを待って食い違いを数える。どちらの向きでも：開いたときと終わったあとの整合性、台帳のファイルの
//!    中身と hash、版の不変条件（`edit::versions::check_invariants`）、予告の行、staging のファイル、失敗した一括・保存・GC の
//!    ジョブ、一括のジョブが 2 本以上・行の数が N でない、failed・skipped・existing・canceled の行（別名では unchanged も）、
//!    一括の版（`item_versions` の version > 0 で job_id が空）が 2 つ以上のアイテム、前の回までに済んだ行が済んでいない・
//!    別の版（別名では別のコピー）を指す。
//!    - 上書き：アイテムの数が N でない、saved・unchanged なのに version_id がそのアイテムの一括の版（記録の `batch.job` が
//!      このジョブで、土台の次の版）を指さない行、pending なのに一括の版があるアイテム、ほかの状態なのに一括の版がある
//!      アイテム、頭の中身が期待と違うアイテム（済んだ行は 1 回当てた結果、pending と積む前は元）。
//!    - 別名：元のアイテムに一括の版がある・頭の中身が元と違う。saved の行の out_item_id が空・ないアイテム・元のアイテムを
//!      指す、2 行以上が同じコピーを指す、指すコピーの頭が 1 回当てた結果と違う・版 0 が行の元と違う（コピーの版 0 は元の
//!      アイテムの元を共有する）、version_id がそのコピーの一括の版（版 1・記録の `batch.job` がこのジョブ）を指さない。
//!      コピー（元でないアイテム）の数が saved の行の数と違う、どの saved の行も指さないコピー（孤立したコピー）、版 0 が
//!      どの元とも違うコピー、コピーが 2 つ以上ある元、pending なのにコピーがある元、アイテムの数が N ＋ saved の行でない。
//!
//!    食い違いは種類ごとに 1 件と数える（文に数と先頭の例を書く）。
//! 4. 最後の回のあと、アプリと同じ渡しもので開き直して一括を終わらせ（ジョブが動き終えるまで待つ）、同じ確かめに加えて、
//!    一括のジョブが done・行がすべて saved・一括の版がちょうど 1 つのアイテムが N 個（別名ではコピーがちょうど N 個で、
//!    どれも一括の版がちょうど 1 つ）・動いているジョブがない、を求める。
//! 5. 結果を 1 つの JSON で標準出力に書く（向き `batchMode`、落とした段の散らばり `atCrash`、回ごとの `perRun`、合計
//!    `totals`）。食い違いがあれば失敗で終わる。子のログは `<ライブラリ>.crash-loop.log` に足す。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kura_core::edit::batch::{
    BatchMode, BatchRequest, DEFAULT_SAVE_AS_NAME, RecipeSource, start_batch,
};
use kura_core::edit::{GcClock, recipes, versions};
use kura_core::import::{ImportInput, ImportOptions, start_import};
use kura_core::jobs::JobState;
use kura_core::library::{IntegrityDepth, Library, LibrarySession, Services, check_integrity};
use kura_engine::codec::CodecSet;
use kura_engine::doc::InputId;
use kura_engine::recipe::{Recipe, RecipeKind};
use kura_engine::render::{Engine, InputValue};
use kura_engine::testgen::InputGen;
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, Graph, NoMonitor};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use rusqlite::OptionalExtension as _;
use serde_json::{Value as Json, json};

use super::{
    GC_SHIFT_MS, H, Opts, SETTLE, W, blob_issues, count, crash_once, files_under, log_path, quiet,
};

/// レシピの名前（NFR-14 と同じ手）。
const RECIPE_NAME: &str = "トリム・4 倍";

/// 食い違いの文に載せる例の数。
const EXAMPLES: usize = 10;

type Hash = [u8; 32];

/// 作ったライブラリと、アイテムごとの元・期待する頭の中身。
struct Fixture {
    root: PathBuf,
    /// アイテム（元）→ (元の中身, レシピを 1 回当てた中身)。
    items: BTreeMap<i64, (Hash, Hash)>,
    /// 別名の一括（`--save-as`）。偽なら上書き。
    save_as: bool,
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `edit crash-loop --batch` の入口（親）。
pub(super) fn run(opts: &Opts, n: usize) -> Result<(), String> {
    let started = Instant::now();
    let fixture = create_library(&opts.library, n, opts.save_as)?;
    let create_ms = ms(started.elapsed());
    let log = log_path(&fixture.root);
    let mut rng = ChaCha8Rng::seed_from_u64(opts.seed);
    let span = opts.max_ms - opts.min_ms + 1;
    let mut extra = vec!["--batch".to_string()];
    if opts.save_as {
        extra.push("--save-as".into());
    }
    let mut saved_before = BTreeMap::new();
    let mut tally = Tally::default();
    let mut runs = Vec::new();
    let mut mismatches = 0usize;
    let crash_started = Instant::now();
    for run in 0..opts.runs {
        let child_seed = rng.next_u64();
        let delay = opts.min_ms + rng.next_u64() % span;
        let ended = crash_once(
            &fixture.root,
            child_seed,
            &extra,
            Duration::from_millis(delay),
            &log,
        )?;
        let at = probe(&fixture.root)?;
        let check = verify(&fixture, Reopen::Hold)?;
        let mut issues = check.issues(&fixture, &saved_before);
        if let Some(status) = ended.filter(|s| !s.success()) {
            issues.push(format!("子が失敗で終わりました（{status}）"));
        }
        let phase = at.phase(n);
        tally.add(&at, phase, ended.is_none());
        mismatches += issues.len();
        tracing::info!(
            run,
            delay_ms = delay,
            phase = phase.name(),
            finished = at.finished,
            issues = issues.len(),
            "落として開き直しました"
        );
        runs.push(json!({
            "run": run,
            "delayMs": delay,
            "childSeed": child_seed,
            "killed": ended.is_none(),
            "childExit": ended.map(|s| s.to_string()),
            "atCrash": at.json(phase),
            "savedRows": check.state_count("saved"),
            "copies": check.copies.len(),
            "batchVersions": check.batch_versions,
            "issues": issues,
        }));
        saved_before = check.saved;
    }
    let crash_ms = ms(crash_started.elapsed());
    // 最後：アプリと同じ渡しもので開き直し、一括を終わらせる
    let settle_started = Instant::now();
    let last = verify(&fixture, Reopen::Finish)?;
    let settle_ms = ms(settle_started.elapsed());
    let mut final_issues = last.issues(&fixture, &saved_before);
    final_issues.extend(last.final_issues(&fixture));
    mismatches += final_issues.len();
    let report = json!({
        "library": fixture.root.display().to_string(),
        "mode": "batch",
        "batchMode": if opts.save_as { "saveAs" } else { "overwrite" },
        "recipe": RECIPE_NAME,
        "items": n,
        "runs": opts.runs,
        "seed": opts.seed,
        "delayMs": { "min": opts.min_ms, "max": opts.max_ms },
        "mismatches": mismatches,
        "runsWithMismatch": runs
            .iter()
            .filter(|r| r["issues"].as_array().is_some_and(|i| !i.is_empty()))
            .map(|r| r["run"].clone())
            .collect::<Vec<Json>>(),
        "finalIssues": final_issues,
        "atCrash": tally.json(),
        "totals": {
            "batchJobs": last.batch_jobs.len(),
            "batchJobState": last.batch_jobs.first().map(|j| j.1.clone()),
            "batchResult": last
                .batch_jobs
                .first()
                .and_then(|j| j.2.as_deref())
                .and_then(|r| serde_json::from_str::<Json>(r).ok()),
            "rows": last.states,
            "batchVersions": last.batch_versions,
            "itemsWithOneBatchVersion": last.one_version,
            "versions": last.versions,
            "items": last.items,
            "copies": last.copies.len(),
            "failedJobs": last.failed_jobs,
        },
        "elapsedMs": {
            "total": ms(started.elapsed()),
            "create": create_ms,
            "crashRuns": crash_ms,
            "finalSettle": settle_ms,
        },
        "perRun": runs,
    });
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    {
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        out.write_all(text.as_bytes())
            .and_then(|()| out.write_all(b"\n"))
            .map_err(|e| e.to_string())?;
    }
    if mismatches > 0 {
        return Err(format!("食い違いが {mismatches} 件ありました"));
    }
    Ok(())
}

/// レシピの文書：透明な余白のトリム → 4 倍（nearest）。
fn recipe_graph() -> Result<Graph, String> {
    let mut g = Graph::new_single();
    g.push_linear(
        "image.trim_alpha",
        1,
        json!({ "threshold": 0, "mode": "union" }),
        0,
    )
    .map_err(|e| e.to_string())?;
    g.push_linear(
        "image.resize",
        1,
        json!({ "width": { "scale": { "num": 4, "den": 1 } }, "height": "auto", "filter": "nearest" }),
        0,
    )
    .map_err(|e| e.to_string())?;
    Ok(g)
}

/// ドット絵の PNG（固定の段で書いたもの。種 `seed`）。
fn sprite_png(codecs: &CodecSet, seed: u64) -> Result<Vec<u8>, String> {
    let value = InputGen::PixelSprite {
        w: W,
        h: H,
        fmt: PixelFormat::Rgba8,
        seed,
    }
    .generate()
    .map_err(|e| e.to_string())?;
    codecs
        .encode_bytes(&value, "png", &json!({}), &NoMonitor)
        .map(|(png, _)| png)
        .map_err(|e| e.to_string())
}

/// 一括の外で、PNG に文書を 1 回当てて PNG に書いたバイト列（一括の上書き・別名の出力と同じになるはずのもの）。
fn render_once(
    engine: &Engine,
    codecs: &CodecSet,
    graph: &Graph,
    png: &[u8],
) -> Result<Vec<u8>, String> {
    let (value, _) = codecs
        .decode(
            &mut std::io::Cursor::new(png),
            "png",
            &Default::default(),
            engine.limits(),
            &NoMonitor,
        )
        .map_err(|e| e.to_string())?;
    let plan = engine
        .compile(graph, &BTreeMap::from([(InputId(0), value.shape())]))
        .map_err(|e| e.to_string())?;
    let source = Digest(*blake3::hash(png).as_bytes());
    let rendered = engine
        .render(
            &plan,
            &BTreeMap::from([(InputId(0), InputValue { value, source })]),
            &NoMonitor,
        )
        .map_err(|e| e.to_string())?;
    let out = rendered.outputs.first().ok_or("出力がありません")?;
    codecs
        .encode_bytes(&out.value, "png", &json!({}), &NoMonitor)
        .map(|(bytes, _)| bytes)
        .map_err(|e| e.to_string())
}

/// 新しいライブラリを作り、ドット絵の PNG を `n` 個取り込んで、レシピを 1 つ足す（`save_as` は確かめ方の向き）。
fn create_library(path: &Path, n: usize, save_as: bool) -> Result<Fixture, String> {
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
    let graph = recipe_graph()?;
    let engine = Engine::new().map_err(|e| e.to_string())?;
    let pinned = CodecSet::pinned();
    // 元の中身 → 1 回当てた中身（元・結果のどちらもほかと重ならない種だけ使う。同じ中身は取り込みでまとまり、結果が重なると
    // 一括が existing にする）
    let mut expected: BTreeMap<Hash, Hash> = BTreeMap::new();
    let mut seen: BTreeSet<Hash> = BTreeSet::new();
    let mut seed = 0u64;
    let mut skipped = 0u64;
    while expected.len() < n {
        seed += 1;
        let png = sprite_png(&pinned, seed)?;
        let original = *blake3::hash(&png).as_bytes();
        let once = *blake3::hash(&render_once(&engine, &pinned, &graph, &png)?).as_bytes();
        if original == once || seen.contains(&original) || seen.contains(&once) {
            skipped += 1;
            continue;
        }
        seen.insert(original);
        seen.insert(once);
        expected.insert(original, once);
        std::fs::write(src.join(format!("sprite-{seed:05}.png")), png)
            .map_err(|e| e.to_string())?;
    }
    if skipped > 0 {
        tracing::info!(skipped, "中身が重なる種を飛ばしました");
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
    let lib = session.library().clone();
    // レシピ（アプリの「レシピとして保存」と同じ表に、同じ形で入れる）
    let recipe = Recipe::new(
        RECIPE_NAME.into(),
        String::new(),
        RecipeKind::Each,
        graph,
        Vec::new(),
    )
    .map_err(|e| format!("レシピを作れません: {e}"))?;
    let recipe_id = lib
        .db()
        .write(move |tx| recipes::insert(tx, &recipe, kura_core::time::now_ms()))
        .map_err(|e| format!("レシピを入れられません: {e}"))?;
    let runtime = session
        .engine()
        .ok_or("エンジンの部品がありません")?
        .runtime()
        .map_err(|e| e.to_string())?;
    let stored = recipes::get(lib.db(), runtime, recipe_id).map_err(|e| e.to_string())?;
    if let Some(why) = stored.problem {
        return Err(format!("レシピを当てられません: {why}"));
    }
    let rows: Vec<(i64, Vec<u8>)> = lib
        .db()
        .read(|c| {
            let mut stmt = c.prepare("SELECT id, hash FROM items ORDER BY id")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .map_err(|e| e.to_string())?;
    let mut items = BTreeMap::new();
    for (id, hash) in rows {
        let hash =
            Hash::try_from(hash.as_slice()).map_err(|_| "hash が 32 バイトではありません")?;
        let once = expected
            .get(&hash)
            .ok_or_else(|| format!("取り込んでいないアイテム {id} があります"))?;
        items.insert(id, (hash, *once));
    }
    if items.len() != n {
        return Err(format!("アイテムが {} 個（{n} 個のはず）", items.len()));
    }
    drop(session);
    std::fs::remove_dir_all(&src).map_err(|e| e.to_string())?;
    Ok(Fixture {
        root,
        items,
        save_as,
    })
}

/// 落とした段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 一括のジョブを積む前（積むトランザクションのコミットの前）。
    BeforeStart,
    /// 積んだが、まだ 1 件も済んでいない。
    Start,
    /// 途中（済んだ行が 1/3 未満・2/3 未満・それ以上）。
    Early,
    Middle,
    Late,
    /// 行はすべて済んだが、ジョブはまだ終わっていない（最後のコミットとジョブの終わりの間）。
    Tail,
    /// ジョブが終わったあと（後のジョブの途中・子が先に終わった）。
    Done,
    /// ジョブが done 以外で終わっている・止まっている（失敗・中止・一時停止。起きないはず）。
    Other,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::BeforeStart => "beforeStart",
            Self::Start => "start",
            Self::Early => "early",
            Self::Middle => "middle",
            Self::Late => "late",
            Self::Tail => "tail",
            Self::Done => "done",
            Self::Other => "other",
        }
    }

    /// 一括の途中（積んだあと・ジョブが終わる前）か。
    fn mid_batch(self) -> bool {
        matches!(
            self,
            Self::Start | Self::Early | Self::Middle | Self::Late | Self::Tail
        )
    }
}

/// 落とした直後（開き直す前）の様子。
struct AtCrash {
    /// 一括のジョブの状態（なければ積む前）。
    job: Option<String>,
    /// pending の行。
    pending: i64,
    /// 済んだ（pending でない）行。
    finished: i64,
    /// 台帳の予告の行（予告のあと・コミットの前で落ちた）。
    ledger_pending: i64,
    /// staging のファイル（書きかけ）。
    staging: i64,
}

impl AtCrash {
    fn phase(&self, n: usize) -> Phase {
        let n = i64::try_from(n).unwrap_or(i64::MAX);
        match self.job.as_deref() {
            None => Phase::BeforeStart,
            Some("done") => Phase::Done,
            Some("queued" | "running") if self.finished == 0 => Phase::Start,
            Some("queued" | "running") if self.pending == 0 => Phase::Tail,
            Some("queued" | "running") if self.finished * 3 < n => Phase::Early,
            Some("queued" | "running") if self.finished * 3 < 2 * n => Phase::Middle,
            Some("queued" | "running") => Phase::Late,
            Some(_) => Phase::Other,
        }
    }

    fn json(&self, phase: Phase) -> Json {
        json!({
            "phase": phase.name(),
            "jobState": self.job,
            "finishedRows": self.finished,
            "pendingRows": self.pending,
            "ledgerPending": self.ledger_pending,
            "stagingFiles": self.staging,
        })
    }
}

/// 落とした段の散らばり（その状態だった回の数）。
#[derive(Default)]
struct Tally {
    /// 子が落とす前に終わらず、SIGKILL した回。
    killed: u32,
    /// 一括の途中で落とした回。
    mid_batch: u32,
    phases: BTreeMap<&'static str, u32>,
    /// 一括のジョブが実行中のまま残った回。
    running_batch: u32,
    ledger_pending: u32,
    staging: u32,
}

impl Tally {
    fn add(&mut self, at: &AtCrash, phase: Phase, killed: bool) {
        self.killed += u32::from(killed);
        self.mid_batch += u32::from(killed && phase.mid_batch());
        *self.phases.entry(phase.name()).or_insert(0) += 1;
        self.running_batch += u32::from(at.job.as_deref() == Some("running"));
        self.ledger_pending += u32::from(at.ledger_pending > 0);
        self.staging += u32::from(at.staging > 0);
    }

    fn json(&self) -> Json {
        json!({
            "killed": self.killed,
            "midBatch": self.mid_batch,
            "phases": self.phases,
            "runningBatchJob": self.running_batch,
            "ledgerPending": self.ledger_pending,
            "stagingFiles": self.staging,
        })
    }
}

/// 落とした直後の DB と staging を、ライブラリを開かずに見る（DB は WAL を戻すだけの接続で読む）。
fn probe(root: &Path) -> Result<AtCrash, String> {
    let conn = rusqlite::Connection::open(root.join(kura_core::library::DB_FILE))
        .map_err(|e| e.to_string())?;
    let count = |sql: &str| -> Result<i64, String> {
        conn.query_row(sql, [], |r| r.get(0))
            .map_err(|e| e.to_string())
    };
    let job: Option<String> = conn
        .query_row(
            "SELECT state FROM jobs WHERE kind = 'batch' ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let mut staging = Vec::new();
    files_under(&root.join("staging"), &mut staging);
    Ok(AtCrash {
        job,
        pending: count("SELECT count(*) FROM batch_items WHERE state = 'pending'")?,
        finished: count("SELECT count(*) FROM batch_items WHERE state <> 'pending'")?,
        ledger_pending: count("SELECT count(*) FROM ledger_pending")?,
        staging: staging.len() as i64,
    })
}

/// 開き直し方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reopen {
    /// エンジンの部品を渡さずに開く（一括・保存・GC の処理を登録しないので、一括のジョブは待ちに戻るだけで進まない）。
    Hold,
    /// アプリと同じ渡しもので開き、一括を終わらせる。
    Finish,
}

/// 一括の行（順、アイテム、状態、版、別名で作ったアイテム、理由）。
struct Row {
    ord: i64,
    item: i64,
    state: String,
    version_id: Option<i64>,
    out_item: Option<i64>,
    code: Option<String>,
}

/// 開き直して確かめた結果。
struct Check {
    open: [u64; 3],
    after: [u64; 3],
    bad_blobs: Vec<String>,
    invariants: Vec<String>,
    ledger_pending: i64,
    staging: Vec<String>,
    /// 一括のジョブ（ID・状態・結果）。
    batch_jobs: Vec<(i64, String, Option<String>)>,
    rows: Vec<Row>,
    /// 行の状態ごとの数。
    states: BTreeMap<String, i64>,
    /// アイテム → 一括の版の数（1 つ以上のものだけ）。
    batch_versions_of: BTreeMap<i64, i64>,
    /// saved・unchanged なのに、version_id がそのアイテム（別名では out_item_id のコピー）の一括の版を指さない行のアイテム。
    dangling: Vec<i64>,
    /// アイテム → 今の中身。
    heads: BTreeMap<i64, Hash>,
    /// 済んだ（saved・unchanged）行：アイテム → (版, 別名で作ったアイテム)（次の回に同じであること）。
    saved: BTreeMap<i64, (Option<i64>, Option<i64>)>,
    /// 元でないアイテム（別名で作ったコピー）→ その版 0 の中身（コピーの版 0 は元のアイテムの元を共有する）。
    copies: BTreeMap<i64, Option<Hash>>,
    /// 失敗した一括・保存・GC のジョブ（種類 → 数）。
    failed_jobs: BTreeMap<String, i64>,
    /// 動き終えていない一括・保存・GC のジョブ。
    live_jobs: i64,
    batch_versions: i64,
    /// 一括の版がちょうど 1 つのアイテム。
    one_version: i64,
    versions: i64,
    items: i64,
}

/// 例を先頭から `EXAMPLES` 個だけ並べた文。
fn examples<T: std::fmt::Display>(list: &[T]) -> String {
    let mut text = list
        .iter()
        .take(EXAMPLES)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("、");
    if list.len() > EXAMPLES {
        text.push_str("、…");
    }
    text
}

impl Check {
    fn state_count(&self, state: &str) -> i64 {
        self.states.get(state).copied().unwrap_or(0)
    }

    /// どの回にも求める食い違いの文（`saved_before` は前の回までに済んだ行）。向きで分かれる確かめは
    /// [`Self::overwrite_issues`]・[`Self::save_as_issues`]。
    fn issues(
        &self,
        fx: &Fixture,
        saved_before: &BTreeMap<i64, (Option<i64>, Option<i64>)>,
    ) -> Vec<String> {
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
        if self.ledger_pending != 0 {
            out.push(format!(
                "台帳の予告が {} 行残っています",
                self.ledger_pending
            ));
        }
        out.extend(self.staging.iter().map(|p| format!("staging に残骸：{p}")));
        for (kind, n) in &self.failed_jobs {
            out.push(format!("失敗した {kind} のジョブが {n} 本"));
        }
        if self.batch_jobs.len() > 1 {
            out.push(format!(
                "一括のジョブが {} 本（1 本のはず）",
                self.batch_jobs.len()
            ));
        }
        let n = fx.items.len();
        if !self.batch_jobs.is_empty() && self.rows.len() != n {
            out.push(format!("一括の行が {} 行（{n} 行のはず）", self.rows.len()));
        }
        // 来ないはずの終わりの状態（元・結果が重なる種は除いたので existing も来ない。別名は unchanged にならない）
        let bad_states: &[&str] = if fx.save_as {
            &["failed", "skipped", "existing", "canceled", "unchanged"]
        } else {
            &["failed", "skipped", "existing", "canceled"]
        };
        for state in bad_states {
            let bad: Vec<String> = self
                .rows
                .iter()
                .filter(|r| r.state == *state)
                .map(|r| {
                    format!(
                        "順 {} のアイテム {}（{}）",
                        r.ord,
                        r.item,
                        r.code.as_deref().unwrap_or("-")
                    )
                })
                .collect();
            if !bad.is_empty() {
                out.push(format!(
                    "{state} の行が {} 行：{}",
                    bad.len(),
                    examples(&bad)
                ));
            }
        }
        let doubled: Vec<String> = self
            .batch_versions_of
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(item, n)| format!("{item}（{n} 個）"))
            .collect();
        if !doubled.is_empty() {
            out.push(format!(
                "一括の版が 2 つ以上のアイテムが {} 個：{}",
                doubled.len(),
                examples(&doubled)
            ));
        }
        if fx.save_as {
            self.save_as_issues(fx, &mut out);
        } else {
            self.overwrite_issues(fx, &mut out);
        }
        // 前の回までに済んだ行は、済んだまま同じ版（別名では同じコピー）を指す
        let changed: Vec<i64> = saved_before
            .iter()
            .filter(|(item, was)| self.saved.get(item) != Some(was))
            .map(|(item, _)| *item)
            .collect();
        if !changed.is_empty() {
            let what = if fx.save_as {
                "別の版・コピー"
            } else {
                "別の版"
            };
            out.push(format!(
                "前の回に済んだ行が、済んでいないか{what}を指しています：{} 行（アイテム {}）",
                changed.len(),
                examples(&changed)
            ));
        }
        out
    }

    /// 上書きの確かめ：アイテムの数は N のまま、済んだ行はそのアイテムの一括の版を指し、一括の版は済んだ行のアイテムにだけ
    /// あり、頭の中身は済んだ行なら 1 回当てた結果・pending と積む前なら元。
    fn overwrite_issues(&self, fx: &Fixture, out: &mut Vec<String>) {
        let n = fx.items.len();
        if usize::try_from(self.items).ok() != Some(n) {
            out.push(format!("アイテムが {} 個（{n} 個のはず）", self.items));
        }
        if !self.dangling.is_empty() {
            out.push(format!(
                "済んだ行なのに、version_id がそのアイテムの一括の版を指しません：{} 行（アイテム {}）",
                self.dangling.len(),
                examples(&self.dangling)
            ));
        }
        let state_of: BTreeMap<i64, &str> = self
            .rows
            .iter()
            .map(|r| (r.item, r.state.as_str()))
            .collect();
        let versioned = |want: &dyn Fn(&str) -> bool| -> Vec<i64> {
            self.batch_versions_of
                .keys()
                .filter(|item| want(state_of.get(item).copied().unwrap_or("")))
                .copied()
                .collect()
        };
        let pending = versioned(&|s| s == "pending");
        if !pending.is_empty() {
            out.push(format!(
                "行が pending なのに一括の版があるアイテムが {} 個：{}",
                pending.len(),
                examples(&pending)
            ));
        }
        let other = versioned(&|s| !matches!(s, "pending" | "saved" | "unchanged"));
        if !other.is_empty() {
            out.push(format!(
                "行が済んでいない（saved・unchanged・pending でない、か行がない）のに一括の版があるアイテムが {} 個：{}",
                other.len(),
                examples(&other)
            ));
        }
        // 頭の中身：済んだ行は 1 回当てた結果、pending と積む前は元
        let mut wrong = Vec::new();
        for (item, (original, once)) in &fx.items {
            let want = match state_of.get(item).copied() {
                Some("saved" | "unchanged") => once,
                Some("pending") | None => original,
                Some(_) => continue,
            };
            match self.heads.get(item) {
                Some(head) if head == want => {}
                Some(head) if head == original => wrong.push(format!("{item}（元のまま）")),
                Some(head) if head == once => wrong.push(format!("{item}（当てた結果）")),
                Some(_) => wrong.push(format!("{item}（どちらでもない）")),
                None => wrong.push(format!("{item}（ない）")),
            }
        }
        if !wrong.is_empty() {
            out.push(format!(
                "頭の中身が期待と違うアイテムが {} 個：{}",
                wrong.len(),
                examples(&wrong)
            ));
        }
    }

    /// 別名の確かめ：元のアイテムは変わらない（一括の版がない・頭は元のまま）。saved の行の out_item_id は、それぞれ別の、
    /// 存在するコピーを指し、その頭は 1 回当てた結果・版 0 は行の元・version_id はその版 1。コピー（元でないアイテム）は
    /// saved の行と同じ数で、どれも saved の行が指し、どの元にも 1 つまで、pending の元にはない。アイテムの数は N ＋ saved の行。
    fn save_as_issues(&self, fx: &Fixture, out: &mut Vec<String>) {
        let n = fx.items.len();
        let saved = self.state_count("saved");
        let want = i64::try_from(n).unwrap_or(i64::MAX).saturating_add(saved);
        if self.items != want {
            out.push(format!(
                "アイテムが {} 個（元 {n} 個 ＋ saved の行 {saved} 行 ＝ {want} 個のはず）",
                self.items
            ));
        }
        // 元のアイテムは変わらない
        let touched: Vec<i64> = self
            .batch_versions_of
            .keys()
            .filter(|item| fx.items.contains_key(item))
            .copied()
            .collect();
        if !touched.is_empty() {
            out.push(format!(
                "元のアイテムに一括の版があります：{} 個：{}",
                touched.len(),
                examples(&touched)
            ));
        }
        let mut changed = Vec::new();
        for (item, (original, once)) in &fx.items {
            match self.heads.get(item) {
                Some(head) if head == original => {}
                Some(head) if head == once => changed.push(format!("{item}（当てた結果）")),
                Some(_) => changed.push(format!("{item}（どちらでもない）")),
                None => changed.push(format!("{item}（ない）")),
            }
        }
        if !changed.is_empty() {
            out.push(format!(
                "元のアイテムの頭の中身が元と違います：{} 個：{}",
                changed.len(),
                examples(&changed)
            ));
        }
        if !self.dangling.is_empty() {
            out.push(format!(
                "saved の行なのに、version_id が out_item_id のコピーの一括の版（版 1）を指しません：{} 行（アイテム {}）",
                self.dangling.len(),
                examples(&self.dangling)
            ));
        }
        // saved の行が指すコピー
        let mut pointed: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        let mut missing = Vec::new();
        let mut originals = Vec::new();
        let mut wrong_head = Vec::new();
        let mut wrong_source = Vec::new();
        for row in self.rows.iter().filter(|r| r.state == "saved") {
            let Some(copy) = row.out_item else {
                missing.push(format!("{}（空）", row.item));
                continue;
            };
            pointed.entry(copy).or_default().push(row.item);
            if fx.items.contains_key(&copy) {
                originals.push(format!("{}（→ {copy}）", row.item));
                continue;
            }
            let Some(head) = self.heads.get(&copy) else {
                missing.push(format!("{}（→ {copy}。ない）", row.item));
                continue;
            };
            let Some((original, once)) = fx.items.get(&row.item) else {
                continue;
            };
            if head != once {
                let what = if head == original {
                    "元のまま"
                } else {
                    "どちらでもない"
                };
                wrong_head.push(format!("{}（→ {copy}。{what}）", row.item));
            }
            if self.copies.get(&copy).copied().flatten() != Some(*original) {
                wrong_source.push(format!("{}（→ {copy}）", row.item));
            }
        }
        for (list, what) in [
            (
                &missing,
                "saved の行の out_item_id が空か、ないアイテムを指します",
            ),
            (
                &originals,
                "saved の行の out_item_id が元のアイテムを指します",
            ),
            (&wrong_head, "コピーの頭の中身が 1 回当てた結果と違います"),
            (&wrong_source, "コピーの版 0 が行の元と違います"),
        ] {
            if !list.is_empty() {
                out.push(format!(
                    "{what}：{} 行（アイテム {}）",
                    list.len(),
                    examples(list)
                ));
            }
        }
        let shared: Vec<String> = pointed
            .iter()
            .filter(|(_, items)| items.len() > 1)
            .map(|(copy, items)| format!("{copy}（← {}）", examples(items)))
            .collect();
        if !shared.is_empty() {
            out.push(format!(
                "2 行以上の saved の行が指すコピーが {} 個：{}",
                shared.len(),
                examples(&shared)
            ));
        }
        // コピーの側から：数・孤立・元ごとの数・pending の元
        let copies = i64::try_from(self.copies.len()).unwrap_or(i64::MAX);
        if copies != saved {
            out.push(format!(
                "別名で作ったアイテム（コピー）が {copies} 個（saved の行の数 {saved} と同じはず）"
            ));
        }
        let orphans: Vec<i64> = self
            .copies
            .keys()
            .filter(|copy| !pointed.contains_key(copy))
            .copied()
            .collect();
        if !orphans.is_empty() {
            out.push(format!(
                "どの saved の行も指さないコピー（孤立したコピー）が {} 個：{}",
                orphans.len(),
                examples(&orphans)
            ));
        }
        let source_of: BTreeMap<&Hash, i64> = fx
            .items
            .iter()
            .map(|(item, (original, _))| (original, *item))
            .collect();
        let mut per_source: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        let mut unknown = Vec::new();
        for (copy, original) in &self.copies {
            match original.as_ref().and_then(|h| source_of.get(h)) {
                Some(item) => per_source.entry(*item).or_default().push(*copy),
                None => unknown.push(*copy),
            }
        }
        if !unknown.is_empty() {
            out.push(format!(
                "版 0 がどの元とも違うコピーが {} 個：{}",
                unknown.len(),
                examples(&unknown)
            ));
        }
        let doubled: Vec<String> = per_source
            .iter()
            .filter(|(_, copies)| copies.len() > 1)
            .map(|(item, copies)| format!("{item}（→ {}）", examples(copies)))
            .collect();
        if !doubled.is_empty() {
            out.push(format!(
                "コピーが 2 つ以上ある元が {} 個：{}",
                doubled.len(),
                examples(&doubled)
            ));
        }
        let pending: Vec<i64> = self
            .rows
            .iter()
            .filter(|r| r.state == "pending" && per_source.contains_key(&r.item))
            .map(|r| r.item)
            .collect();
        if !pending.is_empty() {
            out.push(format!(
                "行が pending なのにコピーがある元が {} 個：{}",
                pending.len(),
                examples(&pending)
            ));
        }
    }

    /// 最後（一括を終わらせたあと）にだけ求める食い違いの文。
    fn final_issues(&self, fx: &Fixture) -> Vec<String> {
        let mut out = Vec::new();
        match self.batch_jobs.first() {
            Some((_, state, _)) if state == "done" => {}
            Some((id, state, _)) => {
                out.push(format!("一括のジョブ {id} が {state}（done のはず）"))
            }
            None => out.push("一括のジョブがありません".into()),
        }
        let n = i64::try_from(fx.items.len()).unwrap_or(i64::MAX);
        let saved = self.state_count("saved");
        if saved != n {
            out.push(format!("saved の行が {saved} 行（{n} 行のはず）"));
        }
        // 別名：コピーがちょうど N 個（一括の版を持つのはコピーだけ）
        let holder = if fx.save_as {
            let copies = i64::try_from(self.copies.len()).unwrap_or(i64::MAX);
            if copies != n {
                out.push(format!("コピーが {copies} 個（{n} 個のはず）"));
            }
            "コピー"
        } else {
            "アイテム"
        };
        if self.one_version != n {
            out.push(format!(
                "一括の版がちょうど 1 つの{holder}が {} 個（{n} 個のはず）",
                self.one_version
            ));
        }
        if self.live_jobs != 0 {
            out.push(format!(
                "動き終えていない一括・保存・GC のジョブが {} 本",
                self.live_jobs
            ));
        }
        out
    }
}

/// 開き直し（`how`）、動くジョブが終わるのを待って確かめる。
fn verify(fx: &Fixture, how: Reopen) -> Result<Check, String> {
    let services = match how {
        Reopen::Hold => Services::new(kura_mac::media_backend()),
        Reopen::Finish => crate::services(),
    };
    let session = LibrarySession::open(&fx.root, quiet(), services)
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
    let bad_blobs = blob_issues(lib)?;
    let invariants = lib
        .db()
        .read(versions::check_invariants)
        .map_err(|e| e.to_string())?;
    let mut staging = Vec::new();
    files_under(&lib.layout().staging_dir(), &mut staging);
    let batch_jobs = read_rows(
        lib,
        "SELECT id, state, result_json FROM jobs WHERE kind = 'batch' ORDER BY id",
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let rows = read_rows(
        lib,
        "SELECT ord, item_id, state, version_id, out_item_id, code FROM batch_items ORDER BY job_id, ord",
        |r| {
            Ok(Row {
                ord: r.get(0)?,
                item: r.get(1)?,
                state: r.get(2)?,
                version_id: r.get(3)?,
                out_item: r.get(4)?,
                code: r.get(5)?,
            })
        },
    )?;
    let mut states = BTreeMap::new();
    for row in &rows {
        *states.entry(row.state.clone()).or_insert(0) += 1;
    }
    let saved = rows
        .iter()
        .filter(|r| matches!(r.state.as_str(), "saved" | "unchanged"))
        .map(|r| (r.item, (r.version_id, r.out_item)))
        .collect();
    let batch_versions_of: BTreeMap<i64, i64> = read_rows(
        lib,
        "SELECT item_id, count(*) FROM item_versions WHERE version > 0 AND job_id IS NULL GROUP BY item_id",
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?
    .into_iter()
    .collect();
    // 上書き：そのアイテムの土台の次の版。別名：out_item_id のコピーの版 1（版 0 は元を共有する）
    let dangling_sql = if fx.save_as {
        "SELECT b.item_id FROM batch_items b
          WHERE b.state IN ('saved', 'unchanged') AND NOT EXISTS (
                SELECT 1 FROM item_versions v
                 WHERE v.id = b.version_id AND v.item_id = b.out_item_id AND v.version = 1 AND v.job_id IS NULL
                   AND json_extract(v.record, '$.batch.job') = b.job_id)
          ORDER BY b.item_id"
    } else {
        "SELECT b.item_id FROM batch_items b
          WHERE b.state IN ('saved', 'unchanged') AND NOT EXISTS (
                SELECT 1 FROM item_versions v
                 WHERE v.id = b.version_id AND v.item_id = b.item_id AND v.version > 0 AND v.job_id IS NULL
                   AND v.version = b.base_version + 1
                   AND json_extract(v.record, '$.batch.job') = b.job_id)
          ORDER BY b.item_id"
    };
    let dangling = read_rows(lib, dangling_sql, |r| r.get(0))?;
    let mut copies = BTreeMap::new();
    for (id, original) in read_rows(
        lib,
        "SELECT i.id, v.output_hash FROM items i
           LEFT JOIN item_versions v ON v.item_id = i.id AND v.version = 0
          ORDER BY i.id",
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)),
    )? {
        if !fx.items.contains_key(&id) {
            copies.insert(id, original.and_then(|h| Hash::try_from(h.as_slice()).ok()));
        }
    }
    let mut heads = BTreeMap::new();
    for (id, hash) in read_rows(lib, "SELECT id, hash FROM items", |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })? {
        if let Ok(hash) = Hash::try_from(hash.as_slice()) {
            heads.insert(id, hash);
        }
    }
    let failed_jobs = read_rows(
        lib,
        "SELECT kind, count(*) FROM jobs WHERE kind IN ('batch', 'edit', 'gc') AND state = 'failed' GROUP BY kind",
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?
    .into_iter()
    .collect();
    let check = Check {
        open,
        after: [
            report.missing_files,
            report.orphan_files,
            report.unreferenced_blobs,
        ],
        bad_blobs,
        invariants,
        ledger_pending: count(lib, "SELECT count(*) FROM ledger_pending")?,
        staging,
        batch_jobs,
        rows,
        states,
        batch_versions: batch_versions_of.values().sum(),
        one_version: batch_versions_of.values().filter(|n| **n == 1).count() as i64,
        batch_versions_of,
        dangling,
        heads,
        saved,
        copies,
        failed_jobs,
        live_jobs: count(
            lib,
            "SELECT count(*) FROM jobs WHERE kind IN ('batch', 'edit', 'gc') AND state NOT IN ('done', 'failed', 'canceled')",
        )?,
        versions: count(lib, "SELECT count(*) FROM item_versions")?,
        items: count(lib, "SELECT count(*) FROM items")?,
    };
    drop(session);
    Ok(check)
}

/// 問い合わせの行をすべて読む。
fn read_rows<T>(
    lib: &Library,
    sql: &str,
    map: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, String> {
    lib.db()
        .read(|c| {
            let mut stmt = c.prepare(sql)?;
            let rows = stmt.query_map([], &map)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .map_err(|e| e.to_string())
}

/// `edit crash-loop-child --batch [--save-as]`（子）：一括のジョブがなければ全件に上書き（`save_as` なら既定の名前の型で
/// 別名）の一括を積み、あれば開いたときに再開したものが動くに任せる。ジョブが動き終えたら終わる（たいていはその前に親が
/// SIGKILL する）。GC の時計だけ 8 日進めて開く。
pub(super) fn child(library: &Path, seed: u64, save_as: bool) -> Result<(), String> {
    let services = crate::services().with_gc_clock(GcClock::shifted(GC_SHIFT_MS));
    let session =
        LibrarySession::open(library, quiet(), services).map_err(|e| format!("開けません: {e}"))?;
    session.wait_ready();
    let lib = session.library().clone();
    if count(&lib, "SELECT count(*) FROM jobs WHERE kind = 'batch'")? == 0 {
        let engine = session.engine().ok_or("エンジンの部品がありません")?;
        let recipe_id = count(&lib, "SELECT min(id) FROM edit_recipes")?;
        let items = read_rows(&lib, "SELECT id FROM items ORDER BY id", |r| r.get(0))?;
        let id = start_batch(
            lib.db(),
            session.jobs(),
            engine,
            &BatchRequest {
                source: RecipeSource::Recipe(recipe_id),
                items,
                mode: if save_as {
                    BatchMode::SaveAs {
                        name: DEFAULT_SAVE_AS_NAME.into(),
                    }
                } else {
                    BatchMode::Overwrite
                },
                include_drafts: false,
            },
        )
        .map_err(|e| format!("一括を積めません: {e}"))?;
        tracing::info!(id, seed, save_as, "一括を積みました");
    } else {
        tracing::info!(seed, "積んであった一括を再開します");
    }
    if !session.jobs().wait_idle(SETTLE) {
        return Err("一括が動き終えません".into());
    }
    let jobs = read_rows(
        &lib,
        "SELECT id, state, result_json FROM jobs WHERE kind = 'batch' ORDER BY id",
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        },
    )?;
    for (id, state, result) in jobs {
        tracing::info!(id, state, result = result.as_deref().unwrap_or(""), "一括");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(job: Option<&str>, finished: i64, pending: i64) -> AtCrash {
        AtCrash {
            job: job.map(str::to_string),
            pending,
            finished,
            ledger_pending: 0,
            staging: 0,
        }
    }

    /// 落とした段：積む前・1 件も済んでいない・済んだ行の割合で前／中／後・行は済んだがジョブが終わる前・終わったあと。
    #[test]
    fn phases_split_the_batch_into_thirds() {
        let n = 1_000;
        let cases = [
            (at(None, 0, 0), Phase::BeforeStart),
            (at(Some("queued"), 0, 1_000), Phase::Start),
            (at(Some("running"), 0, 1_000), Phase::Start),
            (at(Some("running"), 333, 667), Phase::Early),
            (at(Some("running"), 334, 666), Phase::Middle),
            (at(Some("running"), 666, 334), Phase::Middle),
            (at(Some("running"), 667, 333), Phase::Late),
            (at(Some("running"), 1_000, 0), Phase::Tail),
            (at(Some("done"), 1_000, 0), Phase::Done),
            (at(Some("failed"), 10, 990), Phase::Other),
        ];
        for (at, want) in cases {
            assert_eq!(at.phase(n), want, "{:?} {}", at.job, at.finished);
        }
        assert!(Phase::Tail.mid_batch() && Phase::Start.mid_batch());
        assert!(!Phase::BeforeStart.mid_batch() && !Phase::Done.mid_batch());
    }

    fn hash(k: u8) -> Hash {
        [k; 32]
    }

    /// アイテム 1..=3（元は 10 + k、1 回当てた結果は 20 + k）と、`states` の行・一括の版・頭のある確かめの結果（上書き）。
    fn sample(states: [&str; 3]) -> (Fixture, Check) {
        sample_in(states, false)
    }

    /// [`sample`] の向きを選べるもの。別名（`save_as`）では、saved の行の元 k にコピー 10 + k（頭は 1 回当てた結果・版 0 は
    /// 元・一括の版 1 つ）があり、行の out_item_id が指す。元の頭は元のまま。
    fn sample_in(states: [&str; 3], save_as: bool) -> (Fixture, Check) {
        let fx = Fixture {
            root: PathBuf::from("/nonexistent"),
            items: (1..=3)
                .map(|k| (i64::from(k), (hash(10 + k), hash(20 + k))))
                .collect(),
            save_as,
        };
        let copy_of = |item: i64| 10 + item;
        let rows: Vec<Row> = states
            .iter()
            .zip(1..=3i64)
            .map(|(state, item)| Row {
                ord: item - 1,
                item,
                state: (*state).to_string(),
                version_id: (*state == "saved").then_some(100 + item),
                out_item: (save_as && *state == "saved").then_some(copy_of(item)),
                code: None,
            })
            .collect();
        let mut by_state = BTreeMap::new();
        for r in &rows {
            *by_state.entry(r.state.clone()).or_insert(0) += 1;
        }
        let done: Vec<i64> = rows
            .iter()
            .filter(|r| r.state == "saved")
            .map(|r| r.item)
            .collect();
        let mut heads: BTreeMap<i64, Hash> = fx
            .items
            .iter()
            .map(|(item, (original, once))| {
                (
                    *item,
                    if done.contains(item) && !save_as {
                        *once
                    } else {
                        *original
                    },
                )
            })
            .collect();
        let mut copies = BTreeMap::new();
        if save_as {
            for item in &done {
                let (original, once) = fx.items[item];
                heads.insert(copy_of(*item), once);
                copies.insert(copy_of(*item), Some(original));
            }
        }
        let versioned = |item: &i64| if save_as { copy_of(*item) } else { *item };
        let check = Check {
            open: [0; 3],
            after: [0; 3],
            bad_blobs: Vec::new(),
            invariants: Vec::new(),
            ledger_pending: 0,
            staging: Vec::new(),
            batch_jobs: vec![(7, "running".into(), None)],
            saved: rows
                .iter()
                .filter(|r| r.state == "saved")
                .map(|r| (r.item, (r.version_id, r.out_item)))
                .collect(),
            rows,
            states: by_state,
            batch_versions_of: done.iter().map(|item| (versioned(item), 1)).collect(),
            dangling: Vec::new(),
            heads,
            copies,
            failed_jobs: BTreeMap::new(),
            live_jobs: 0,
            batch_versions: done.len() as i64,
            one_version: done.len() as i64,
            versions: 2 * done.len() as i64,
            items: if save_as { 3 + done.len() as i64 } else { 3 },
        };
        (fx, check)
    }

    fn has(issues: &[String], needle: &str) -> bool {
        issues.iter().any(|i| i.contains(needle))
    }

    /// 途中の様子として正しいもの（済んだ行は版を 1 つ指し、頭は 1 回当てた結果。pending の頭は元）は食い違い 0。
    #[test]
    fn a_consistent_midway_state_has_no_issues() {
        let (fx, check) = sample(["saved", "pending", "pending"]);
        assert_eq!(check.issues(&fx, &BTreeMap::new()), Vec::<String>::new());
        let before = BTreeMap::from([(1, (Some(101), None))]);
        assert_eq!(check.issues(&fx, &before), Vec::<String>::new());
        // 最後に求めるものは満たさない（まだ pending がある・ジョブが動いている）
        let last = check.final_issues(&fx);
        assert!(has(&last, "running（done のはず）"), "{last:?}");
        assert!(has(&last, "saved の行が 1 行"), "{last:?}");
        assert!(has(&last, "ちょうど 1 つのアイテムが 1 個"), "{last:?}");
    }

    /// 2 回当てた（版が 2 つ・頭がどちらでもない）、版があるのに pending、済んだ行が消えた・版を指し替えた、版を指さない行、
    /// failed の行を数える。
    #[test]
    fn issues_catch_double_application_and_lost_rows() {
        let (fx, mut check) = sample(["saved", "pending", "pending"]);
        // アイテム 1 に 2 回当てた
        check.batch_versions_of.insert(1, 2);
        check.heads.insert(1, hash(99));
        // アイテム 2 は版も頭も当てたのに、行は pending のまま
        check.batch_versions_of.insert(2, 1);
        check.heads.insert(2, hash(22));
        let issues = check.issues(&fx, &BTreeMap::new());
        assert!(
            has(&issues, "一括の版が 2 つ以上のアイテムが 1 個：1（2 個）"),
            "{issues:?}"
        );
        assert!(
            has(&issues, "pending なのに一括の版があるアイテムが 1 個：2"),
            "{issues:?}"
        );
        assert!(
            has(&issues, "頭の中身が期待と違うアイテムが 2 個"),
            "{issues:?}"
        );
        assert!(has(&issues, "1（どちらでもない）"), "{issues:?}");
        assert!(has(&issues, "2（当てた結果）"), "{issues:?}");

        // 前の回に済んだ行が pending に戻った・別の版を指す
        let (fx, check) = sample(["saved", "pending", "pending"]);
        let before = BTreeMap::from([(1, (Some(555), None)), (3, (Some(103), None))]);
        let issues = check.issues(&fx, &before);
        assert!(
            has(
                &issues,
                "前の回に済んだ行が、済んでいないか別の版を指しています：2 行（アイテム 1、3）"
            ),
            "{issues:?}"
        );

        // 版を指さない saved の行・failed の行・失敗したジョブ・2 本目の一括
        let (fx, mut check) = sample(["saved", "failed", "pending"]);
        check.dangling.push(1);
        check.rows[1].code = Some("Io".into());
        check.failed_jobs.insert("batch".into(), 1);
        check.batch_jobs.push((8, "queued".into(), None));
        let issues = check.issues(&fx, &BTreeMap::new());
        assert!(
            has(
                &issues,
                "version_id がそのアイテムの一括の版を指しません：1 行"
            ),
            "{issues:?}"
        );
        assert!(
            has(&issues, "failed の行が 1 行：順 1 のアイテム 2（Io）"),
            "{issues:?}"
        );
        assert!(has(&issues, "失敗した batch のジョブが 1 本"), "{issues:?}");
        assert!(has(&issues, "一括のジョブが 2 本"), "{issues:?}");
    }

    /// 最後：ジョブが done・行がすべて saved・どのアイテムも一括の版がちょうど 1 つなら食い違い 0。
    #[test]
    fn the_final_state_needs_every_row_saved_once() {
        let (fx, mut check) = sample(["saved", "saved", "saved"]);
        check.batch_jobs[0].1 = "done".into();
        assert_eq!(check.issues(&fx, &BTreeMap::new()), Vec::<String>::new());
        assert_eq!(check.final_issues(&fx), Vec::<String>::new());
        check.live_jobs = 1;
        assert!(has(&check.final_issues(&fx), "動き終えていない"));
    }

    /// 別名：途中の様子として正しいもの（元は変わらず、saved の行はコピーを指し、pending の元にコピーがない）は食い違い 0。
    /// 前の回に済んだ行が同じコピーを指していても 0。最後に求めるものは満たさない。
    #[test]
    fn a_consistent_save_as_midway_state_has_no_issues() {
        let (fx, check) = sample_in(["saved", "pending", "pending"], true);
        assert_eq!(check.issues(&fx, &BTreeMap::new()), Vec::<String>::new());
        let before = BTreeMap::from([(1, (Some(101), Some(11)))]);
        assert_eq!(check.issues(&fx, &before), Vec::<String>::new());
        let last = check.final_issues(&fx);
        assert!(has(&last, "running（done のはず）"), "{last:?}");
        assert!(has(&last, "saved の行が 1 行"), "{last:?}");
        assert!(has(&last, "コピーが 1 個（3 個のはず）"), "{last:?}");
        assert!(has(&last, "ちょうど 1 つのコピーが 1 個"), "{last:?}");
        // 上書きの確かめを当てると、元の頭が元のまま・アイテムが多い、になる（向きで確かめが分かれている）
        let overwrite = Fixture {
            save_as: false,
            ..fx
        };
        let issues = check.issues(&overwrite, &BTreeMap::new());
        assert!(has(&issues, "アイテムが 4 個（3 個のはず）"), "{issues:?}");
        assert!(has(&issues, "1（元のまま）"), "{issues:?}");
    }

    /// 別名：元が変わった（頭・一括の版）、pending の元のコピー・孤立したコピー・2 つ目のコピー、行の指すコピーがない・
    /// 元を指す・2 行が同じコピーを指す・頭や版 0 が違う、unchanged の行、版を指さない行、前の回と違うコピーを数える。
    #[test]
    fn save_as_issues_catch_changed_originals_and_stray_copies() {
        // 元のアイテム 1 に当ててしまった（頭が当てた結果・一括の版がある）
        let (fx, mut check) = sample_in(["saved", "pending", "pending"], true);
        check.heads.insert(1, hash(21));
        check.batch_versions_of.insert(1, 1);
        let issues = check.issues(&fx, &BTreeMap::new());
        assert!(
            has(
                &issues,
                "元のアイテムの頭の中身が元と違います：1 個：1（当てた結果）"
            ),
            "{issues:?}"
        );
        assert!(
            has(&issues, "元のアイテムに一括の版があります：1 個：1"),
            "{issues:?}"
        );

        // pending の元 2 にコピー 99、済んだ元 1 に 2 つ目のコピー 98（どちらも行が指さない）
        let (fx, mut check) = sample_in(["saved", "pending", "pending"], true);
        for (copy, item) in [(99, 2i64), (98, 1)] {
            let (original, once) = fx.items[&item];
            check.copies.insert(copy, Some(original));
            check.heads.insert(copy, once);
            check.batch_versions_of.insert(copy, 1);
        }
        check.items += 2;
        let issues = check.issues(&fx, &BTreeMap::new());
        assert!(
            has(&issues, "行が pending なのにコピーがある元が 1 個：2"),
            "{issues:?}"
        );
        assert!(
            has(&issues, "孤立したコピー）が 2 個：98、99"),
            "{issues:?}"
        );
        assert!(
            has(&issues, "コピーが 2 つ以上ある元が 1 個：1（→ 11、98）"),
            "{issues:?}"
        );
        assert!(
            has(&issues, "コピー）が 3 個（saved の行の数 1 と同じはず）"),
            "{issues:?}"
        );
        assert!(
            has(
                &issues,
                "アイテムが 6 個（元 3 個 ＋ saved の行 1 行 ＝ 4 個のはず）"
            ),
            "{issues:?}"
        );

        // 行の指すコピーがない（消えて SET NULL・ない ID）、元を指す
        let (fx, mut check) = sample_in(["saved", "saved", "saved"], true);
        check.rows[0].out_item = None;
        check.rows[1].out_item = Some(77);
        check.rows[2].out_item = Some(1);
        let issues = check.issues(&fx, &BTreeMap::new());
        assert!(
            has(
                &issues,
                "out_item_id が空か、ないアイテムを指します：2 行（アイテム 1（空）、2（→ 77。ない））"
            ),
            "{issues:?}"
        );
        assert!(
            has(
                &issues,
                "out_item_id が元のアイテムを指します：1 行（アイテム 3（→ 1））"
            ),
            "{issues:?}"
        );
        assert!(has(&issues, "孤立したコピー）が 3 個"), "{issues:?}");

        // 2 行が同じコピーを指す・コピーの頭がどちらでもない・版 0 が別の元・unchanged の行・版を指さない行
        let (fx, mut check) = sample_in(["saved", "saved", "unchanged"], true);
        check.rows[1].out_item = Some(11);
        check.heads.insert(11, hash(99));
        check.copies.insert(12, Some(hash(13)));
        check.dangling.push(2);
        let issues = check.issues(&fx, &BTreeMap::new());
        assert!(
            has(
                &issues,
                "2 行以上の saved の行が指すコピーが 1 個：11（← 1、2）"
            ),
            "{issues:?}"
        );
        assert!(
            has(
                &issues,
                "コピーの頭の中身が 1 回当てた結果と違います：2 行（アイテム 1（→ 11。どちらでもない）、2（→ 11。どちらでもない））"
            ),
            "{issues:?}"
        );
        assert!(
            has(
                &issues,
                "コピーの版 0 が行の元と違います：1 行（アイテム 2（→ 11））"
            ),
            "{issues:?}"
        );
        assert!(has(&issues, "孤立したコピー）が 1 個：12"), "{issues:?}");
        assert!(
            has(&issues, "unchanged の行が 1 行：順 2 のアイテム 3"),
            "{issues:?}"
        );
        assert!(
            has(
                &issues,
                "out_item_id のコピーの一括の版（版 1）を指しません：1 行（アイテム 2）"
            ),
            "{issues:?}"
        );

        // 前の回に済んだ行が、別のコピーを指す・pending に戻った
        let (fx, check) = sample_in(["saved", "pending", "pending"], true);
        let before = BTreeMap::from([(1, (Some(101), Some(55))), (2, (Some(102), Some(12)))]);
        let issues = check.issues(&fx, &before);
        assert!(
            has(
                &issues,
                "前の回に済んだ行が、済んでいないか別の版・コピーを指しています：2 行（アイテム 1、2）"
            ),
            "{issues:?}"
        );
    }

    /// 別名の最後：ジョブが done・行がすべて saved・コピーがちょうど N 個で、どれも一括の版がちょうど 1 つなら食い違い 0。
    #[test]
    fn the_final_save_as_state_needs_one_copy_per_original() {
        let (fx, mut check) = sample_in(["saved", "saved", "saved"], true);
        check.batch_jobs[0].1 = "done".into();
        assert_eq!(check.issues(&fx, &BTreeMap::new()), Vec::<String>::new());
        assert_eq!(check.final_issues(&fx), Vec::<String>::new());
        check.copies.remove(&13);
        check.one_version = 2;
        let last = check.final_issues(&fx);
        assert!(has(&last, "コピーが 2 個（3 個のはず）"), "{last:?}");
        assert!(has(&last, "ちょうど 1 つのコピーが 2 個"), "{last:?}");
    }

    /// 例は先頭の 10 個までで、残りは「…」。
    #[test]
    fn examples_are_capped() {
        let list: Vec<i64> = (1..=12).collect();
        assert_eq!(examples(&list), "1、2、3、4、5、6、7、8、9、10、…");
        assert_eq!(examples(&[1, 2]), "1、2");
    }

    /// レシピの文書は、トリム → 4 倍で、ドット絵を当てると幅・高さが 4 の倍数で元より大きくなる（もう 1 回当てると変わる）。
    #[test]
    fn the_recipe_trims_and_scales_by_four() {
        let graph = recipe_graph().unwrap();
        let ops: Vec<&str> = graph.nodes.iter().map(|n| n.op.as_str()).collect();
        assert_eq!(ops, ["image.trim_alpha", "image.resize"]);
        let engine = Engine::new().unwrap();
        let pinned = CodecSet::pinned();
        let png = sprite_png(&pinned, 1).unwrap();
        let once = render_once(&engine, &pinned, &graph, &png).unwrap();
        let twice = render_once(&engine, &pinned, &graph, &once).unwrap();
        let shape = |bytes: &[u8]| {
            pinned
                .shape(
                    &mut std::io::Cursor::new(bytes),
                    "png",
                    &Default::default(),
                    engine.limits(),
                )
                .unwrap()
                .0
        };
        let (s1, s2) = (shape(&once), shape(&twice));
        assert_ne!(
            s1, s2,
            "2 回当てると形が変わる（二重に当てたことが中身で分かる）"
        );
        assert_ne!(once, png);
    }
}
