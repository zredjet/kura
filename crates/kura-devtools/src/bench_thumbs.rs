//! サムネイルのジョブの速さ（`bench thumbs-job --src <dir> [--threads N] [--engine-cap <MiB>] [--no-engine] [--out <dir>]`。
//! NFR-04：画像 50 枚/秒以上。M11 の決定 2 で、PNG の多いライブラリでの判定の範囲を書くために測る）。
//!
//! `--src` の画像などを新しいライブラリに取り込み（アプリと同じ渡しもの）、できたサムネイルを消して pending に戻してから、
//! サムネイルのジョブだけを動かして測る（形式ごとの道：エンジンは一括の予算と一括のプール、ほかは ImageIO など）。
//! 道ごとの件数と、作れなかった数も出す。プロセスのメモリのピークは `/usr/bin/time -l` で外から測る。測り終えたら
//! ライブラリを消す。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_core::edit::EngineServices;
use kura_core::import::{ImportHandler, ImportInput, ImportOptions, MediaKind, start_import};
use kura_core::jobs::{JobKind, JobManager, JobState};
use kura_core::library::Library;
use kura_core::media::{ThumbRoute, ThumbnailHandler, thumb_route};

/// `bench thumbs-job` を実行する。
pub fn run(args: &[String]) -> Result<(), String> {
    let mut src = None;
    let mut out = None;
    let mut threads = None;
    let mut cap = None;
    let mut engine = true;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--src" => src = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            "--threads" => threads = it.next().and_then(|n| n.parse::<usize>().ok()),
            "--engine-cap" => cap = it.next().and_then(|n| n.parse::<u64>().ok()),
            "--no-engine" => engine = false,
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    let src = src.ok_or("--src を指定してください")?;
    let base = out.unwrap_or_else(|| src.parent().map(PathBuf::from).unwrap_or_default());
    let path = base.join(format!("kura-bench-thumbs-{}.kuralib", std::process::id()));
    let result = measure(&src, &path, threads, cap, engine);
    let _ = std::fs::remove_dir_all(&path);
    result
}

fn measure(
    src: &Path,
    path: &Path,
    threads: Option<usize>,
    cap: Option<u64>,
    engine: bool,
) -> Result<(), String> {
    let library = Arc::new(Library::create(path).map_err(|e| e.to_string())?);
    let backend = kura_mac::media_backend();
    // 取り込みだけ（サムネイルのジョブは登録しない。積もうとしても無視される）
    {
        let jobs = JobManager::new(
            library.clone(),
            vec![(
                JobKind::Import,
                Arc::new(ImportHandler::new(backend.clone())),
            )],
            Arc::new(|_| {}),
        )
        .map_err(|e| e.to_string())?;
        start_import(
            &jobs,
            &ImportInput {
                paths: vec![src.to_path_buf()],
                options: ImportOptions::default(),
            },
        )
        .map_err(|e| e.to_string())?;
        jobs.start().map_err(|e| e.to_string())?;
        if !jobs.wait_idle(Duration::from_secs(3600)) {
            return Err("取り込みが 1 時間で終わりませんでした".into());
        }
        jobs.shutdown();
    }
    let rows: Vec<(String, String)> = library
        .db()
        .write(|tx| {
            tx.execute_batch("UPDATE items SET thumb_state = 'pending'; DELETE FROM jobs WHERE kind = 'thumbnail';")?;
            let mut stmt = tx.prepare("SELECT kind, ext FROM items")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .map_err(|e| e.to_string())?;
    let mut routes: BTreeMap<String, u64> = BTreeMap::new();
    for (kind, ext) in &rows {
        let route = MediaKind::parse(kind).map(|k| thumb_route(k, ext));
        let name = match route {
            Some(ThumbRoute::Engine) => "engine",
            Some(ThumbRoute::ImageIo) => "imageio",
            Some(ThumbRoute::Font) => "font",
            Some(ThumbRoute::Model) => "model",
            Some(ThumbRoute::Video) => "video",
            Some(ThumbRoute::Audio) => "audio",
            None => "unknown",
        };
        *routes.entry(name.to_string()).or_default() += 1;
    }
    for dir in ["thumbs", "art"] {
        let _ = std::fs::remove_dir_all(library.layout().root().join(dir));
    }

    let mut handler = ThumbnailHandler::new(backend);
    if engine {
        handler = handler.with_engine(Some(Arc::new(EngineServices::new(
            Arc::new(crate::MacEngineFactory),
            library.layout().staging_dir().join("engine"),
        ))));
    }
    if let Some(n) = threads {
        handler = handler.with_threads(n);
    }
    if let Some(mib) = cap {
        handler = handler.with_engine_cap(mib << 20);
    }
    let jobs = JobManager::new(
        library.clone(),
        vec![(JobKind::Thumbnail, Arc::new(handler))],
        Arc::new(|_| {}),
    )
    .map_err(|e| e.to_string())?;
    let id = jobs
        .enqueue(JobKind::Thumbnail, "{}".into())
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    jobs.start().map_err(|e| e.to_string())?;
    if !jobs.wait_idle(Duration::from_secs(3600)) {
        return Err("サムネイルが 1 時間で終わりませんでした".into());
    }
    let elapsed = started.elapsed();
    let job = jobs.get(id).map_err(|e| e.to_string())?;
    jobs.shutdown();
    if job.state != JobState::Done {
        return Err(job.message.unwrap_or_default());
    }
    let png: i64 = library
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM items WHERE thumb_state = 'ready'",
                [],
                |r| r.get(0),
            )?)
        })
        .map_err(|e| e.to_string())?;
    tracing::info!(
        items = rows.len(),
        ready = png,
        ?routes,
        engine,
        engine_cap_mib = cap.unwrap_or(256),
        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
        per_second = rows.len() as f64 / elapsed.as_secs_f64(),
        result = job.result_json.as_deref().unwrap_or(""),
        "サムネイルのジョブ（NFR-04：画像 50 枚/秒以上）"
    );
    Ok(())
}
