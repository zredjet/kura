//! ベンチマーク（`bench query --library <path>` など）。NFR の目標と比べるための数値を出す。編集のベンチ（`bench edit`）は
//! `bench_edit` に振り分ける。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use kura_core::items::{
    ItemQuery, ItemSort, ItemView, MAX_SUMMARIES, SortKey, counts, detail, query_ids, recent_since,
    summaries,
};
use kura_core::library::Library;
use kura_core::query::{QueryContext, parse};
use kura_core::time::now_ms;

/// `bench query --library <path>` / `bench thumbs --src <dir> [--threads N]` /
/// `bench import --src <dir> [--threads N] [--out <dir>]` / `bench backup --library <path>` /
/// `bench edit …`（編集のベンチ。`bench_edit`）/ `bench migrate …`（マイグレーション。`bench_migrate`）/
/// `bench analyze …`（解析のジョブ。`bench_analyze`）/ `bench thumbs-job --src <dir> …`（サムネイルのジョブ。`bench_thumbs`）/
/// `bench svg-worker …`（SVG の働き手の手間。`bench_svg`）/ `bench editor …`・`bench save …`（M12 のアプリの道：エディタの
/// 1 手とタイル、保存のジョブの全体。`bench_app`）。
pub fn run(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("edit") {
        return crate::bench_edit::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("editor") {
        return crate::bench_app::editor::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("save") {
        return crate::bench_app::save::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("thumbs-job") {
        return crate::bench_thumbs::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("migrate") {
        return crate::bench_migrate::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("analyze") {
        return crate::bench_analyze::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("svg-worker") {
        return crate::bench_svg::run(&args[1..]);
    }
    let mut library = None;
    let mut src = None;
    let mut threads = None;
    let mut out = None;
    let mut fake_facts = false;
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--library" => library = it.next().map(PathBuf::from),
            "--src" => src = it.next().map(PathBuf::from),
            "--threads" => threads = it.next().and_then(|n| n.parse().ok()),
            "--out" => out = it.next().map(PathBuf::from),
            "--fake-facts" => fake_facts = true,
            _ => {}
        }
    }
    match args.first().map(String::as_str) {
        Some("query") => query(&library.ok_or("--library を指定してください")?, fake_facts),
        Some("thumbs") => thumbs(&src.ok_or("--src を指定してください")?, threads),
        Some("import") => import(&src.ok_or("--src を指定してください")?, threads, out),
        Some("backup") => backup(&library.ok_or("--library を指定してください")?),
        _ => Err(
            "使い方: kura-devtools bench query --library <path> [--fake-facts] | thumbs --src <dir> [--threads N] \
             | import --src <dir> [--threads N] [--out <dir>] | backup --library <path> | edit [--quick] … \
             | migrate (--library <path> | --items <件数> [--history]) [--runs 3] [--work <dir>] [--json <file>] \
             | analyze --kind image|audio [--limit N] [--src <dir>] [--work <dir>] [--json <file>] \
             | editor --work <dir> [--threads 4,16] [--runs 3] [--quick] … | save --work <dir> [--threads 4,16] [--runs 3] [--quick] …"
                .into(),
        ),
    }
}

/// 画像サムネイル（長辺 1600 と 512）を作る速さ。NFR-04 は 50 枚/秒以上。
fn thumbs(src: &std::path::Path, threads: Option<usize>) -> Result<(), String> {
    use kura_core::media::ThumbTargets;
    use rayon::prelude::*;

    let images: Vec<PathBuf> = std::fs::read_dir(src)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            kura_core::import::classify(&p.extension().unwrap_or_default().to_string_lossy())
                == Some(kura_core::import::MediaKind::Image)
        })
        .collect();
    if images.is_empty() {
        return Err("画像がありません".into());
    }
    let out = tempfile_dir()?;
    let backend = kura_mac::media_backend();
    let threads = threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let failed = pool.install(|| {
        images
            .par_iter()
            .enumerate()
            .filter(|(i, path)| {
                // 書き先は拡張子のない stem（書いた形式の拡張子が付く）
                let targets = ThumbTargets {
                    large: out.join(format!("{i}_l")),
                    small: out.join(format!("{i}_s")),
                };
                backend.image_thumbnails(path, &targets).is_err()
            })
            .count()
    });
    let elapsed = started.elapsed();
    std::fs::remove_dir_all(&out).map_err(|e| e.to_string())?;
    tracing::info!(
        images = images.len(),
        failed,
        threads,
        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
        per_second = images.len() as f64 / elapsed.as_secs_f64(),
        "画像サムネイル（目標 50 枚/秒以上）"
    );
    Ok(())
}

/// 取り込みの速さ（NFR-05：ディスクの速度が上限になり、ハッシュが詰まらないこと）。
///
/// 新しいライブラリを `--out`（既定は `--src` と同じ場所。同じボリュームならクローン）に作り、`--src` を
/// アプリと同じ取り込みのジョブで入れる。同じファイルをハッシュだけした速さも出す。測り終えたらライブラリを消す。
fn import(
    src: &std::path::Path,
    threads: Option<usize>,
    out: Option<PathBuf>,
) -> Result<(), String> {
    use kura_core::import::{
        ImportHandler, ImportInput, ImportOptions, classify, hash_file, start_import,
    };
    use kura_core::jobs::{JobKind, JobManager, JobState};
    use rayon::prelude::*;
    use std::sync::Arc;

    let files: Vec<(PathBuf, u64)> = std::fs::read_dir(src)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| classify(&p.extension().unwrap_or_default().to_string_lossy()).is_some())
        .filter_map(|p| std::fs::metadata(&p).ok().map(|m| (p, m.len())))
        .collect();
    if files.is_empty() {
        return Err("取り込めるファイルがありません".into());
    }
    let bytes: u64 = files.iter().map(|(_, len)| len).sum();
    let mb = bytes as f64 / 1_000_000.0;
    let base = out.unwrap_or_else(|| src.parent().map(PathBuf::from).unwrap_or_default());
    let path = base.join(format!("kura-bench-import-{}.kuralib", std::process::id()));

    let library = Arc::new(kura_core::library::Library::create(&path).map_err(|e| e.to_string())?);
    let mut handler = ImportHandler::new(kura_mac::media_backend());
    if let Some(n) = threads {
        handler = handler.with_threads(n);
    }
    let jobs = JobManager::new(
        library,
        vec![(JobKind::Import, Arc::new(handler))],
        Arc::new(|_| {}),
    )
    .map_err(|e| e.to_string())?;
    let input = ImportInput {
        paths: vec![src.to_path_buf()],
        options: ImportOptions::default(),
    };
    let started = Instant::now();
    let id = start_import(&jobs, &input).map_err(|e| e.to_string())?;
    jobs.start().map_err(|e| e.to_string())?;
    if !jobs.wait_idle(Duration::from_secs(3600)) {
        return Err("1 時間待っても終わりませんでした".into());
    }
    let elapsed = started.elapsed();
    let job = jobs.get(id).map_err(|e| e.to_string())?;
    jobs.shutdown();
    drop(jobs);
    std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
    if job.state != JobState::Done {
        return Err(job.message.unwrap_or_default());
    }

    // 同じファイルをハッシュだけする（読み込みはキャッシュに載っている前提で、CPU の上限を見る）
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.unwrap_or(kura_core::import::MAX_IMPORT_THREADS))
        .build()
        .map_err(|e| e.to_string())?;
    let hashed = Instant::now();
    pool.install(|| {
        files
            .par_iter()
            .try_for_each(|(p, _)| hash_file(p).map(|_| ()))
    })
    .map_err(|e| e.to_string())?;
    let hash_elapsed = hashed.elapsed();

    tracing::info!(
        files = files.len(),
        mb,
        threads = threads.unwrap_or(0),
        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
        import_mb_per_s = mb / elapsed.as_secs_f64(),
        files_per_s = files.len() as f64 / elapsed.as_secs_f64(),
        hash_only_mb_per_s = mb / hash_elapsed.as_secs_f64(),
        result = job.result_json.as_deref().unwrap_or(""),
        "取り込み（NFR-05：ディスクの速度が上限）"
    );
    Ok(())
}

/// DB のバックアップを作る速さ（`backups/` に 1 つ作る。新しい 7 個を残す）。
fn backup(path: &std::path::Path) -> Result<(), String> {
    let library = Library::open(path).map_err(|e| e.to_string())?;
    let db_bytes = std::fs::metadata(library.layout().db_path())
        .map_err(|e| e.to_string())?
        .len();
    let started = Instant::now();
    let info = kura_core::library::backup::create(library.layout(), now_ms())
        .map_err(|e| e.to_string())?;
    tracing::info!(
        db_mb = db_bytes as f64 / 1_000_000.0,
        backup_mb = info.size_bytes as f64 / 1_000_000.0,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        path = %info.path.display(),
        "DB のバックアップ"
    );
    Ok(())
}

fn tempfile_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("kura-bench-thumbs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 何回か測って中央値を返す。
fn median(mut runs: Vec<Duration>) -> Duration {
    runs.sort();
    runs[runs.len() / 2]
}

/// `fake_facts`：素材の事実の擬似の行を入れて、事実のフィールドの検索も測る（終わったら消す。`bench_facts`）。
fn query(path: &std::path::Path, fake_facts: bool) -> Result<(), String> {
    let opened = Instant::now();
    let library = Library::open(path).map_err(|e| e.to_string())?;
    let open_time = opened.elapsed();
    let db = library.db();
    let ctx = QueryContext {
        now_ms: now_ms(),
        utc_offset_minutes: 540,
    };
    if fake_facts {
        let started = Instant::now();
        let (images, audios, settings) = crate::bench_facts::insert(db)?;
        tracing::info!(
            images,
            audios,
            settings,
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "擬似の事実を入れました（測り終えたら消します）"
        );
        let measured = crate::bench_facts::search(db, &ctx);
        crate::bench_facts::remove(db)?;
        measured?;
    }

    let mut list_runs = Vec::new();
    let mut bytes = 0;
    let mut ids = Vec::new();
    for _ in 0..7 {
        let started = Instant::now();
        let list = db
            .read(|conn| query_ids(conn, &ItemQuery::default(), &ctx))
            .map_err(|e| e.to_string())?;
        bytes = list.encode().len();
        list_runs.push(started.elapsed());
        ids = list.ids;
    }

    let first: Vec<i64> = ids
        .iter()
        .take(MAX_SUMMARIES)
        .map(|&id| i64::from(id))
        .collect();
    let mut summary_runs = Vec::new();
    for _ in 0..7 {
        let started = Instant::now();
        let items = db
            .read(|conn| summaries(conn, &first))
            .map_err(|e| e.to_string())?;
        summary_runs.push(started.elapsed());
        if items.len() != first.len() {
            return Err("詳細の件数が合いません".into());
        }
    }

    let mut detail_runs = Vec::new();
    for &id in first.iter().take(7) {
        let started = Instant::now();
        db.read(|conn| detail(conn, id))
            .map_err(|e| e.to_string())?;
        detail_runs.push(started.elapsed());
    }

    // M3：サイドバーの一覧の種類と件数
    let folder = db
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT min(id) FROM folders WHERE parent_id IS NULL",
                [],
                |r| r.get::<_, Option<i64>>(0),
            )?)
        })
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    let view_ms = |view: ItemView, collapse: bool| -> Result<(f64, usize), String> {
        let mut runs = Vec::new();
        let mut n = 0;
        for _ in 0..5 {
            let started = Instant::now();
            let query = ItemQuery {
                view,
                collapse_units: collapse,
                ..ItemQuery::default()
            };
            let list = db
                .read(|conn| query_ids(conn, &query, &ctx))
                .map_err(|e| e.to_string())?;
            runs.push(started.elapsed());
            n = list.ids.len();
        }
        Ok((median(runs).as_secs_f64() * 1000.0, n))
    };
    let since = recent_since(now_ms());
    let (recent_ms, _) = view_ms(ItemView::Recent { since }, false)?;
    let (unsorted_ms, unsorted) = view_ms(ItemView::Unsorted, false)?;
    let (untagged_ms, untagged) = view_ms(ItemView::Untagged, false)?;
    let (folder_ms, in_folder) = view_ms(ItemView::Folder { id: folder }, false)?;
    let (random_ms, _) = view_ms(ItemView::Random { seed: 1 }, false)?;
    let (collapse_ms, collapsed) = view_ms(ItemView::All, true)?;
    let mut count_runs = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        db.read(|conn| counts(conn, since))
            .map_err(|e| e.to_string())?;
        db.read(kura_core::folders::item_counts)
            .map_err(|e| e.to_string())?;
        count_runs.push(started.elapsed());
    }
    let mut suggest_runs = Vec::new();
    for prefix in ["タ", "タグ1", "作者:", "", "タグ19"] {
        let started = Instant::now();
        db.read(|conn| kura_core::tags::suggest(conn, prefix, 8))
            .map_err(|e| e.to_string())?;
        suggest_runs.push(started.elapsed());
    }
    let mut tag_runs = Vec::new();
    let mut tag_count = 0;
    for _ in 0..5 {
        let started = Instant::now();
        tag_count = db
            .read(kura_core::tags::list_all)
            .map_err(|e| e.to_string())?
            .len();
        tag_runs.push(started.elapsed());
    }
    tracing::info!(
        tags = tag_count,
        tags_list_ms = median(tag_runs).as_secs_f64() * 1000.0,
        "タグの一覧（中央値）"
    );
    tracing::info!(
        recent_ms,
        unsorted_ms,
        unsorted,
        untagged_ms,
        untagged,
        folder_ms,
        in_folder,
        random_ms,
        collapse_ms,
        collapsed,
        sidebar_counts_ms = median(count_runs).as_secs_f64() * 1000.0,
        tags_suggest_ms = median(suggest_runs).as_secs_f64() * 1000.0,
        "サイドバーの一覧と件数（中央値）"
    );

    search(db, &ctx)?;

    tracing::info!(
        items = ids.len(),
        open_ms = open_time.as_secs_f64() * 1000.0,
        query_ids_ms = median(list_runs).as_secs_f64() * 1000.0,
        query_ids_bytes = bytes,
        items_get_200_ms = median(summary_runs).as_secs_f64() * 1000.0,
        item_detail_ms = median(detail_runs).as_secs_f64() * 1000.0,
        "一覧の取得（中央値）"
    );
    Ok(())
}

/// M4：検索（NFR-03：タグ 3 つの AND が 50ms 以内、全文検索が 100ms 以内）。
fn search(db: &kura_core::db::Db, ctx: &QueryContext) -> Result<(), String> {
    let cases = [
        (
            "tags3_popular",
            "タグ0 作者:タグ1 場所:タグ2",
            SortKey::Imported,
        ),
        ("tags3_plain", "タグ0 タグ6 タグ12", SortKey::Imported),
        ("tags3_rare", "タグ1800 タグ0 タグ6", SortKey::Imported),
        ("fulltext_3", "IMG_00012", SortKey::Imported),
        ("fulltext_2", "99", SortKey::Imported),
        (
            "fields",
            "種類:画像 評価>=4 追加:365日以内",
            SortKey::Imported,
        ),
        (
            "or_not",
            "(タグ0 | タグ6) -タグ12 種類:動画,音楽",
            SortKey::Imported,
        ),
        ("sort_name", "", SortKey::Name),
        ("sort_captured", "", SortKey::Captured),
    ];
    for (name, text, key) in cases {
        let filter = parse(text).map_err(|e| e.message)?.expr;
        let query = ItemQuery {
            filter: Some(filter),
            sort: ItemSort { key, desc: true },
            ..ItemQuery::default()
        };
        let mut runs = Vec::new();
        let mut n = 0;
        for _ in 0..7 {
            let started = Instant::now();
            let list = db
                .read(|conn| query_ids(conn, &query, ctx))
                .map_err(|e| e.to_string())?;
            runs.push(started.elapsed());
            n = list.ids.len();
        }
        tracing::info!(
            case = name,
            query = text,
            matched = n,
            ms = median(runs).as_secs_f64() * 1000.0,
            "検索（中央値）"
        );
    }
    Ok(())
}
