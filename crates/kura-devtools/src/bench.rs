//! ベンチマーク（`bench query --library <path>`）。NFR の目標と比べるための数値を出す。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use kura_core::items::{
    ItemQuery, ItemSort, ItemView, MAX_SUMMARIES, counts, detail, query_ids, recent_since,
    summaries,
};
use kura_core::library::Library;
use kura_core::time::now_ms;

/// `bench query --library <path>` / `bench thumbs --src <dir> [--threads N]`。
pub fn run(args: &[String]) -> Result<(), String> {
    let mut library = None;
    let mut src = None;
    let mut threads = None;
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--library" => library = it.next().map(PathBuf::from),
            "--src" => src = it.next().map(PathBuf::from),
            "--threads" => threads = it.next().and_then(|n| n.parse().ok()),
            _ => {}
        }
    }
    match args.first().map(String::as_str) {
        Some("query") => query(&library.ok_or("--library を指定してください")?),
        Some("thumbs") => thumbs(&src.ok_or("--src を指定してください")?, threads),
        _ => Err(
            "使い方: kura-devtools bench query --library <path> | thumbs --src <dir> [--threads N]"
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
                let targets = ThumbTargets {
                    large: out.join(format!("{i}_l.jpg")),
                    small: out.join(format!("{i}_s.jpg")),
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

fn query(path: &std::path::Path) -> Result<(), String> {
    let opened = Instant::now();
    let library = Library::open(path).map_err(|e| e.to_string())?;
    let open_time = opened.elapsed();
    let db = library.db();

    let mut list_runs = Vec::new();
    let mut bytes = 0;
    let mut ids = Vec::new();
    for _ in 0..7 {
        let started = Instant::now();
        let list = db
            .read(|conn| query_ids(conn, &ItemQuery::default(), ItemSort::ImportedDesc, 540))
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
            };
            let list = db
                .read(|conn| query_ids(conn, &query, ItemSort::ImportedDesc, 540))
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
