//! `kura-devtools bench batch`：一括加工のジョブの通し（NFR-14 のアプリの道。M14 段 8）と、エディタで同じ手を保存した結果との
//! バイト列の照らし（E4 のアプリの道。M14 の完了条件）。
//!
//! ```text
//! kura-devtools bench batch --work <dir> [--items 1000] [--bulk-threads 3] [--threads 4,16] [--runs 3] [--e4 20] [--quick]
//!                           [--json <file>] [--markdown <file|->] [--keep]
//! ```
//!
//! - 入力：ドット絵の PNG（`sprite_batch(N, 1)`。M10 の bench edit の NFR-14 と同じ絵・同じ PNG の設定 `bulk_png`）を `--work`
//!   の下に書き、回 × スレッドの数ごとに新しいライブラリ（`BenchFactory`：アプリのコーデックの組・アプリと同じ耐久
//!   `MacDurability`・一括のプールをその数）にアプリと同じ取り込みのジョブで入れ、後のジョブ（サムネイル・解析）が動き
//!   終えるまで待つ。レシピ「トリム・4 倍」（`bulk_doc`：`image.trim_alpha` → `image.resize` 最近傍 4 倍。形式は元と同じ）を
//!   アプリの「レシピとして保存」と同じ表に入れる（`recipes::insert`）。
//! - 1 つのライブラリで：まず別のレシピ（反転）の別名の一括（慣らし。元を変えず、出力が判定の上書きの出力と重ならない。
//!   測らない）、後のジョブが終わってから上書きの一括（判定）を回す。同じレシピの別名は、上書きの出力と同じ中身のコピーを
//!   作って上書きを「同じ中身」（existing）にしてしまうので、もう 1 つの新しいライブラリで測る（記録）。
//!   測るのは `start_batch` の直前から一括のジョブの終わりの知らせ（`JobEvent` の Done）まで（積む・待ち行列・行を読む・
//!   元を読む・描く・書く・1 件ずつの fsync・区切りの F_FULLFSYNC・まとまりのコミット・後のジョブを積む。利用者の決定 1）。
//!   後のジョブが動き終えるまでの時間は別の行（記録）。どの行も saved になったことを確かめる（違えば失敗で終わる）。
//! - 判定（NFR-14）：判定のスレッドの数の上書きの通しの、回をまたいだ最大 ≤ 5 秒（M10 と同じく慣らし 1 回のあと 3 回の最大）。
//! - E4（利用者の決定 2）：判定のスレッドの数の最後の回の上書きのあと、`--e4` 件（等間隔）の元だけを新しいライブラリに入れ、
//!   エディタ（`editor::open` → 同じ文書を `Edit::Replace` → `editor::save` の上書き）で保存して、頭のファイルのバイト列
//!   （中身のハッシュ）が一括の結果と同じことを確かめる。違えば失敗で終わる。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_core::edit::batch::{
    BatchMode, BatchRequest, DEFAULT_SAVE_AS_NAME, RecipeSource, start_batch,
};
use kura_core::edit::editor::{self, EditorEnv, OpenRequest, SaveMode, Step, Viewport};
use kura_core::edit::{recipes, versions};
use kura_core::jobs::JobState;
use kura_core::library::LibrarySession;
use kura_engine::recipe::{Recipe, RecipeKind};
use kura_engine::session::{CommitMode, Edit};
use kura_engine::testing::bench::cases::{bulk_doc, bulk_png};
use kura_engine::testing::bench::{Stats, sprite_batch};
use kura_engine::{Graph, NoMonitor};
use serde_json::{Value as J, json};

use super::report::emit;
use super::{Recorder, Report, Row, Verdict};
use crate::bench_edit::{load1, loadavg};

const USAGE: &str = "使い方: kura-devtools bench batch --work <dir> [--items 1000] [--bulk-threads 3] [--threads 4,16] \
                     [--runs 3] [--e4 20] [--quick] [--json <file>] [--markdown <file|->] [--keep]";

/// NFR-14 の目標（ミリ秒）。
const NFR14_MS: f64 = 5000.0;

/// 一括・保存のジョブ 1 つを待つ上限。
const JOB_WAIT: Duration = Duration::from_secs(900);

/// レシピの名前（NFR-14 と同じ手）。
pub(crate) const RECIPE_NAME: &str = "トリム・4 倍";

/// 慣らしのレシピの名前（判定の上書きと出力が重ならない手）。
const WARMUP_NAME: &str = "反転（慣らし）";

/// 慣らしの文書：左右の反転だけ。
fn warmup_doc() -> Result<Graph, String> {
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .map_err(|e| e.to_string())?;
    Ok(g)
}

#[derive(Debug, Clone)]
struct Opts {
    work: Option<PathBuf>,
    items: usize,
    bulk_threads: usize,
    threads: Vec<usize>,
    runs: usize,
    e4: usize,
    quick: bool,
    json: Option<PathBuf>,
    markdown: Option<PathBuf>,
    keep: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        work: None,
        items: 1000,
        bulk_threads: 3,
        threads: Vec::new(),
        runs: 3,
        e4: 20,
        quick: false,
        json: None,
        markdown: None,
        keep: false,
    };
    let number = |v: Option<&String>, name: &str| -> Result<usize, String> {
        v.and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .ok_or(format!("{name} には 1 以上の数を指定してください"))
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--work" => o.work = it.next().map(PathBuf::from),
            "--items" => o.items = number(it.next(), "--items")?,
            "--bulk-threads" => o.bulk_threads = number(it.next(), "--bulk-threads")?,
            "--threads" => o.threads = super::thread_list(it.next())?,
            "--runs" => o.runs = number(it.next(), "--runs")?,
            "--e4" => o.e4 = number(it.next(), "--e4")?,
            "--quick" => o.quick = true,
            "--json" => o.json = it.next().map(PathBuf::from),
            "--markdown" => o.markdown = it.next().map(PathBuf::from),
            "--keep" => o.keep = true,
            other => return Err(format!("知らない引数 {other}\n{USAGE}")),
        }
    }
    if o.work.is_none() {
        return Err(USAGE.into());
    }
    if o.quick {
        o.items = o.items.min(24);
        o.runs = o.runs.min(1);
        o.e4 = o.e4.min(3);
    }
    o.e4 = o.e4.min(o.items);
    Ok(o)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    if cfg!(debug_assertions) && !opts.quick {
        return Err(
            "bench batch は release で回してください（cargo run --release -p kura-devtools -- bench batch）。\
             debug では --quick だけ回せます"
                .into(),
        );
    }
    let work = opts.work.clone().ok_or(USAGE)?;
    let root = super::fresh_dir(&work, "batch")?;
    let result = run_all(&opts, &root);
    if !opts.keep {
        let _ = std::fs::remove_dir_all(&root);
    }
    result
}

/// 入力の名前（取り込みのファイル名）。
pub(crate) fn sprite_name(i: usize) -> String {
    format!("sprite-{i:04}.png")
}

/// 入力の PNG を書く（M10 の NFR-14 と同じ絵と PNG の設定）。
pub(crate) fn write_inputs(src: &Path, n: usize) -> Result<(), String> {
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    let codecs = crate::app_codecs(src);
    for (i, g) in sprite_batch(n, 1).iter().enumerate() {
        let v = g.generate().map_err(|e| e.to_string())?;
        let (bytes, _) = codecs
            .encode_bytes(&v, "png", &bulk_png(), &NoMonitor)
            .map_err(|e| e.to_string())?;
        std::fs::write(src.join(sprite_name(i)), bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// レシピ「トリム・4 倍」をライブラリの表に入れる（アプリのレシピとして保存と同じ表・同じ形）。ID を返す。
pub(crate) fn insert_recipe(
    session: &LibrarySession,
    name: &str,
    graph: &Graph,
) -> Result<i64, String> {
    let recipe = Recipe::new(
        name.into(),
        String::new(),
        RecipeKind::Each,
        graph.clone(),
        Vec::new(),
    )
    .map_err(|e| format!("レシピを作れません: {e}"))?;
    let id = session
        .library()
        .db()
        .write(move |tx| recipes::insert(tx, &recipe, kura_core::time::now_ms()))
        .map_err(|e| format!("レシピを入れられません: {e}"))?;
    let runtime = session
        .engine()
        .ok_or("エンジンの部品がありません")?
        .runtime()
        .map_err(|e| e.to_string())?;
    let stored = recipes::get(session.library().db(), runtime, id).map_err(|e| e.to_string())?;
    if let Some(why) = stored.problem {
        return Err(format!("レシピを当てられません: {why}"));
    }
    Ok(id)
}

/// 一括 1 本の結果：積んでから終わりの知らせまで（ナノ秒）、そのうち待ち行列の分、後のジョブが動き終えるまで。
struct Timed {
    job_ns: u64,
    queue_ns: u64,
    follow_ns: u64,
}

/// 一括を 1 本積んで終わるまで待つ。どの行も saved でなければ失敗。
fn one_batch(
    session: &LibrarySession,
    rec: &Recorder,
    recipe: i64,
    items: &[i64],
    mode: BatchMode,
) -> Result<Timed, String> {
    let engine = session.engine().ok_or("エンジンの部品がありません")?;
    let t0 = Instant::now();
    let id = start_batch(
        session.library().db(),
        session.jobs(),
        engine,
        &BatchRequest {
            source: RecipeSource::Recipe(recipe),
            items: items.to_vec(),
            mode,
            include_drafts: false,
        },
    )
    .map_err(|e| format!("一括を積めません: {e}"))?;
    let seen = rec.wait(id, JOB_WAIT)?;
    let (end, state, message, result) = seen.end.clone().ok_or("一括が終わりません")?;
    if state != JobState::Done {
        return Err(format!("一括が終わりません（{state:?}）: {message:?}"));
    }
    let saved = result
        .as_deref()
        .and_then(|r| serde_json::from_str::<J>(r).ok())
        .and_then(|j| j["counts"]["saved"].as_u64())
        .unwrap_or(0);
    if saved != items.len() as u64 {
        return Err(format!(
            "当てた行が {saved} / {} です: {result:?}",
            items.len()
        ));
    }
    let job_ns = end.duration_since(t0).as_nanos() as u64;
    let queue_ns = seen
        .running
        .map_or(0, |r| r.duration_since(t0).as_nanos() as u64);
    let t1 = Instant::now();
    super::settle(session)?;
    Ok(Timed {
        job_ns,
        queue_ns,
        follow_ns: t1.elapsed().as_nanos() as u64,
    })
}

/// 頭の中身（ファイルのハッシュ）。
fn head_hash(session: &LibrarySession, item: i64) -> Result<[u8; 32], String> {
    session
        .library()
        .db()
        .read(|c| versions::head(c, item))
        .map_err(|e| e.to_string())?
        .map(|h| h.hash)
        .ok_or(format!("アイテム {item} がありません"))
}

/// E4 の照らし：`names` の元を新しいライブラリに入れ、エディタで同じ文書を保存して、頭の中身が `expected`（一括の結果）と
/// 同じか。違う元の名前を返す。
fn editor_matches(
    root: &Path,
    src: &Path,
    graph: &Graph,
    threads: usize,
    names: &[String],
    expected: &BTreeMap<String, [u8; 32]>,
) -> Result<Vec<String>, String> {
    let picked = root.join("e4-src");
    std::fs::create_dir_all(&picked).map_err(|e| e.to_string())?;
    for name in names {
        std::fs::copy(src.join(name), picked.join(name)).map_err(|e| e.to_string())?;
    }
    let lib = root.join("e4.kuralib");
    let rec = Arc::new(Recorder::default());
    let session = LibrarySession::create(&lib, rec.sink(), super::services(3, threads))
        .map_err(|e| format!("ライブラリを作れません: {e}"))?;
    session.wait_ready();
    let items = super::import_all(&session, &picked)?;
    let env = EditorEnv::of(&session).map_err(|e| e.to_string())?;
    let mut mismatched = Vec::new();
    for name in names {
        let item = *items
            .get(name)
            .ok_or(format!("{name} が取り込まれていません"))?;
        let opened = editor::open(
            &env,
            &OpenRequest {
                item_id: item,
                viewport: Viewport::whole(),
                from_draft: false,
            },
        )
        .map_err(|e| format!("{name} を開けません: {e}"))?;
        let out = editor::step(
            &env,
            &opened.session,
            vec![Step::Commit {
                edit: Edit::Replace {
                    graph: graph.clone(),
                },
                mode: CommitMode::Step,
            }],
            &Viewport::whole(),
        )
        .map_err(|e| format!("{name} に手を入れられません: {e}"))?;
        if let Some(r) = out.rejected {
            return Err(format!("{name} に手を入れられません: {:?}", r.problem));
        }
        let started = editor::save(&env, &opened.session, SaveMode::Overwrite, None)
            .map_err(|e| format!("{name} を保存できません: {e}"))?;
        let seen = rec.wait(started.job, JOB_WAIT)?;
        if seen.end.as_ref().map(|e| e.1) != Some(JobState::Done) {
            return Err(format!("{name} の保存が終わりません: {:?}", seen.end));
        }
        editor::close(env.engine, &opened.session);
        if Some(&head_hash(&session, item)?) != expected.get(name) {
            mismatched.push(name.clone());
        }
    }
    super::settle(&session)?;
    session.close();
    drop(session);
    let _ = std::fs::remove_dir_all(&lib);
    let _ = std::fs::remove_dir_all(&picked);
    Ok(mismatched)
}

fn stats(ns: u64) -> Result<Stats, String> {
    Stats::from_samples(vec![ns]).ok_or("測った値がありません".into())
}

fn run_all(opts: &Opts, root: &Path) -> Result<(), String> {
    let src = root.join("src");
    emit(&format!(
        "bench batch：入力を作っています（{} 件、{}）",
        opts.items,
        src.display()
    ));
    write_inputs(&src, opts.items)?;
    let graph = bulk_doc().map_err(|e| e.to_string())?;
    let env = super::env_json(json!({
        "bench": "batch",
        "items": opts.items,
        "bulk_threads": opts.bulk_threads,
        "preview_threads": 3,
        "threads": opts.threads,
        "runs": opts.runs,
        "e4": opts.e4,
        "quick": opts.quick,
        "durability": "MacDurability（1 件ずつ fsync・32 件か 1 秒ごとに F_FULLFSYNC）",
        "recipe": RECIPE_NAME,
    }));
    emit(&format!("bench batch：{env}"));
    let set = super::thread_set(opts.bulk_threads, &opts.threads);
    let over_label = format!("上書き png×{}", opts.items);
    let save_as_label = format!("別名 png×{}", opts.items);
    let mut rows = Vec::new();
    let mut runs = Vec::new();
    let mut e4 = None;
    for run in 0..opts.runs {
        let before = loadavg();
        let started = Instant::now();
        emit(&format!(
            "== {} 回目（ロードアベレージ {before:?}）==",
            run + 1
        ));
        for &threads in &set {
            let judged = threads == opts.bulk_threads;
            let lib = root.join(format!("r{run}-t{threads}.kuralib"));
            let rec = Arc::new(Recorder::default());
            let session = LibrarySession::create(&lib, rec.sink(), super::services(3, threads))
                .map_err(|e| format!("ライブラリを作れません: {e}"))?;
            session.wait_ready();
            let result = (|| -> Result<(), String> {
                let items = super::import_all(&session, &src)?;
                let ids: Vec<i64> = (0..opts.items)
                    .map(|i| items.get(&sprite_name(i)).copied())
                    .collect::<Option<_>>()
                    .ok_or("取り込まれていない入力があります")?;
                let recipe = insert_recipe(&session, RECIPE_NAME, &graph)?;
                let warm = insert_recipe(&session, WARMUP_NAME, &warmup_doc()?)?;
                // 慣らし：反転の別名（元を変えない。出力は判定の上書きと重ならない。測らない）
                one_batch(
                    &session,
                    &rec,
                    warm,
                    &ids,
                    BatchMode::SaveAs {
                        name: DEFAULT_SAVE_AS_NAME.into(),
                    },
                )?;
                // 判定：上書き
                let load = load1();
                let over = one_batch(&session, &rec, recipe, &ids, BatchMode::Overwrite)?;
                emit(&format!(
                    "  {threads} スレッド：上書き {:.0} ms（待ち {:.0} ms・後のジョブ {:.0} ms）",
                    over.job_ns as f64 / 1e6,
                    over.queue_ns as f64 / 1e6,
                    over.follow_ns as f64 / 1e6,
                ));
                let mut row = Row::timed(
                    run,
                    14,
                    "batch-overwrite",
                    &over_label,
                    "job",
                    "積んでから一括の終わり（Done）",
                    threads,
                    stats(over.job_ns)?,
                    Some(NFR14_MS),
                    judged,
                    load,
                );
                row.extra = json!({ "queue_ms": over.queue_ns as f64 / 1e6 });
                rows.push(row);
                rows.push(Row::timed(
                    run,
                    14,
                    "batch-overwrite",
                    &over_label,
                    "follow",
                    "後のジョブ（サムネイル・解析）が動き終えるまで（参考）",
                    threads,
                    stats(over.follow_ns)?,
                    None,
                    false,
                    load,
                ));
                // E4：最後の回の判定のスレッドの数で
                if judged && run + 1 == opts.runs {
                    let step = (opts.items / opts.e4).max(1);
                    let names: Vec<String> = (0..opts.e4).map(|k| sprite_name(k * step)).collect();
                    let expected = names
                        .iter()
                        .map(|n| {
                            let id = *items.get(n).ok_or("入力がありません")?;
                            Ok((n.clone(), head_hash(&session, id)?))
                        })
                        .collect::<Result<BTreeMap<_, _>, String>>()?;
                    let bad = editor_matches(root, &src, &graph, threads, &names, &expected)?;
                    emit(&format!(
                        "  E4：エディタで保存した {} 件のうち、一括の結果とバイト列が違うもの {} 件",
                        names.len(),
                        bad.len()
                    ));
                    rows.push(Row::value(
                        run,
                        14,
                        "e4-editor",
                        &format!("E4 png×{}", names.len()),
                        "mismatch",
                        "エディタで同じ手を保存した結果とバイト列が違う件数（E4）",
                        threads,
                        bad.len() as f64,
                        "件",
                        if bad.is_empty() {
                            Verdict::Pass
                        } else {
                            Verdict::Fail
                        },
                        load1(),
                    ));
                    e4 = Some(bad);
                }
                Ok(())
            })();
            session.close();
            drop(session);
            if !opts.keep {
                let _ = std::fs::remove_dir_all(&lib);
            }
            result?;
            // 別名（記録）：同じレシピの別名は上書きの出力と同じ中身のコピーを作るので、新しいライブラリで
            let lib = root.join(format!("r{run}-t{threads}-saveas.kuralib"));
            let rec = Arc::new(Recorder::default());
            let session = LibrarySession::create(&lib, rec.sink(), super::services(3, threads))
                .map_err(|e| format!("ライブラリを作れません: {e}"))?;
            session.wait_ready();
            let result = (|| -> Result<(), String> {
                let items = super::import_all(&session, &src)?;
                let ids: Vec<i64> = (0..opts.items)
                    .map(|i| items.get(&sprite_name(i)).copied())
                    .collect::<Option<_>>()
                    .ok_or("取り込まれていない入力があります")?;
                let recipe = insert_recipe(&session, RECIPE_NAME, &graph)?;
                let warm = insert_recipe(&session, WARMUP_NAME, &warmup_doc()?)?;
                // 慣らし：反転の別名（元を変えず、コピーの中身は測る別名の出力と重ならない）
                one_batch(
                    &session,
                    &rec,
                    warm,
                    &ids,
                    BatchMode::SaveAs {
                        name: DEFAULT_SAVE_AS_NAME.into(),
                    },
                )?;
                let load = load1();
                let sa = one_batch(
                    &session,
                    &rec,
                    recipe,
                    &ids,
                    BatchMode::SaveAs {
                        name: DEFAULT_SAVE_AS_NAME.into(),
                    },
                )?;
                emit(&format!(
                    "  {threads} スレッド：別名 {:.0} ms（待ち {:.0} ms）",
                    sa.job_ns as f64 / 1e6,
                    sa.queue_ns as f64 / 1e6,
                ));
                rows.push(Row::timed(
                    run,
                    14,
                    "batch-save-as",
                    &save_as_label,
                    "job",
                    "積んでから一括の終わり（Done）",
                    threads,
                    stats(sa.job_ns)?,
                    Some(NFR14_MS),
                    false,
                    load,
                ));
                Ok(())
            })();
            session.close();
            drop(session);
            if !opts.keep {
                let _ = std::fs::remove_dir_all(&lib);
            }
            result?;
        }
        runs.push(json!({
            "run": run,
            "loadavg_before": before,
            "loadavg_after": loadavg(),
            "seconds": started.elapsed().as_secs_f64(),
        }));
    }
    let report = Report {
        title: "bench batch（一括加工のジョブの通し：start_batch → Done の知らせ。E4 のアプリの道）",
        env,
        runs,
        rows,
        judged_threads: opts.bulk_threads,
        thread_metrics: &["job"],
    };
    super::write_report(&report, opts.json.as_deref(), opts.markdown.as_deref())?;
    match e4 {
        Some(bad) if !bad.is_empty() => Err(format!(
            "E4：エディタで保存した結果と一括の結果のバイト列が違います: {bad:?}"
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn arguments_are_parsed() {
        let o = parse(&args(
            "--work w --items 50 --bulk-threads 4 --threads 3,16 --runs 2 --e4 5 --keep",
        ))
        .unwrap();
        assert_eq!((o.items, o.bulk_threads, o.runs, o.e4), (50, 4, 2, 5));
        assert_eq!(o.threads, vec![3, 16]);
        assert!(o.keep && !o.quick);
        let q = parse(&args("--work w --quick")).unwrap();
        assert_eq!((q.items, q.runs, q.e4), (24, 1, 3));
        assert!(parse(&args("--items 3")).is_err());
        assert!(parse(&args("--work w --items 0")).is_err());
        assert!(parse(&args("--work w --bogus")).is_err());
    }

    /// 極小の大きさで通しで回す（debug。取り込み・別名と上書きの一括・後のジョブ・エディタで保存した結果との照らし）。
    #[test]
    fn quick_run_goes_through_the_batch_job_and_matches_the_editor() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("r.json");
        let a = vec![
            "--work".to_string(),
            dir.path().join("w").display().to_string(),
            "--quick".into(),
            "--items".into(),
            "8".into(),
            "--json".into(),
            out.display().to_string(),
        ];
        run(&a).unwrap();
        let j: J = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let rows = j["rows"].as_array().unwrap();
        for case in ["batch-save-as", "batch-overwrite", "e4-editor"] {
            assert!(rows.iter().any(|r| r["case"] == case), "{case}");
        }
        let e4 = rows.iter().find(|r| r["case"] == "e4-editor").unwrap();
        assert_eq!(e4["verdict"], "pass", "{e4}");
        assert_eq!(std::fs::read_dir(dir.path().join("w")).unwrap().count(), 0);
    }
}
