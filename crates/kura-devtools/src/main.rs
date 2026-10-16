//! 開発用ツール。
//!
//! - `import --library <path> [--create] [<src>...]`：取り込む（<src> を省くと、中断していた取り込みの再開だけ待つ）
//! - `download --library <path> <url>...`：アプリと同じジョブで取得してライブラリに入れる（M7 の確認用）
//! - `check --library <path>`：件数と整合性（DB とファイルの食い違い）を表示する
//! - `seed --items <件数> --out <path>`：ダミーライブラリを作る（グリッドと起動時間の計測用）
//! - `bench query --library <path>`：一覧の取得の速さを測る
//! - `bench thumbs --src <画像のフォルダ> [--threads N]`：画像サムネイルの速さを測る（NFR-04）
//! - `edit shape --in <file>` / `edit render --doc <文書.json> --in <file> --out <file>`：編集のエンジンを手で確かめる（docs/editing.md）

mod bench;
mod edit;
mod seed;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kura_core::download::{CookieSource, DownloadInput, DownloadOptions, detect, start_download};
use kura_core::import::{ImportInput, ImportOptions, start_import};
use kura_core::jobs::{EventSink, JobFilter, JobKind, JobState};
use kura_core::library::{LibrarySession, check_files};

const USAGE: &str = "使い方: kura-devtools import --library <path> [--create] [<src>...] | \
                     download --library <path> <url>... | check --library <path>";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("import") => import(&args[1..]),
        Some("download") => download(&args[1..]),
        Some("check") => check(&args[1..]),
        Some("seed") => seed::run(&args[1..]),
        Some("bench") => bench::run(&args[1..]),
        Some("edit") => edit::run(&args[1..]),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            tracing::error!("{message}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    library: PathBuf,
    create: bool,
    sources: Vec<PathBuf>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut library = None;
    let mut create = false;
    let mut sources = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--library" => library = it.next().map(PathBuf::from),
            "--create" => create = true,
            other => sources.push(PathBuf::from(other)),
        }
    }
    Ok(Args {
        library: library.ok_or(USAGE)?,
        create,
        sources,
    })
}

fn progress_sink() -> EventSink {
    Arc::new(|event| {
        tracing::info!(
            id = event.id,
            state = event.state.as_str(),
            progress = event.progress.unwrap_or(0.0),
            message = event.message.as_deref().unwrap_or(""),
        );
    })
}

/// URL を取得してライブラリに入れる（アプリと同じダウンロードのジョブ。既定の選択で）。
fn download(args: &[String]) -> Result<(), String> {
    let args = parse(args)?;
    let session = LibrarySession::open(&args.library, progress_sink(), kura_mac::media_backend())
        .map_err(|e| e.to_string())?;
    session.wait_ready();
    for url in &args.sources {
        let detection = detect(&url.to_string_lossy(), &[]).map_err(|e| e.to_string())?;
        let input = DownloadInput {
            detection,
            options: DownloadOptions::default(),
            cookies: CookieSource::None,
            redownload: false,
        };
        let id = start_download(session.jobs(), &input).map_err(|e| e.to_string())?;
        tracing::info!(id, url = %url.display(), "ダウンロードを積みました");
    }
    if !session.jobs().wait_idle(Duration::from_secs(3600)) {
        return Err("1 時間待っても終わりませんでした".into());
    }
    let jobs = session
        .jobs()
        .list(&JobFilter {
            kinds: vec![JobKind::Download],
            states: vec![],
            limit: Some(args.sources.len().max(1) as u32),
        })
        .map_err(|e| e.to_string())?;
    let mut failed = None;
    for job in jobs {
        tracing::info!(
            id = job.id,
            state = job.state.as_str(),
            message = job.message.as_deref().unwrap_or(""),
            result = job.result_json.as_deref().unwrap_or(""),
            "ダウンロード"
        );
        if job.state == JobState::Failed {
            failed = job.message;
        }
    }
    match failed {
        Some(message) => Err(message),
        None => report(&session),
    }
}

fn import(args: &[String]) -> Result<(), String> {
    let args = parse(args)?;
    let session = if args.create {
        LibrarySession::create(&args.library, progress_sink(), kura_mac::media_backend())
    } else {
        LibrarySession::open(&args.library, progress_sink(), kura_mac::media_backend())
    }
    .map_err(|e| e.to_string())?;

    if !args.sources.is_empty() {
        let input = ImportInput {
            paths: args.sources,
            options: ImportOptions::default(),
        };
        let id = start_import(session.jobs(), &input).map_err(|e| e.to_string())?;
        tracing::info!(id, "取り込みを積みました");
    }
    session.wait_ready();
    if !session.jobs().wait_idle(Duration::from_secs(3600)) {
        return Err("1 時間待っても終わりませんでした".into());
    }
    let jobs = session
        .jobs()
        .list(&JobFilter {
            kinds: vec![JobKind::Import, JobKind::Thumbnail],
            states: vec![],
            limit: Some(5),
        })
        .map_err(|e| e.to_string())?;
    for job in jobs {
        tracing::info!(
            id = job.id,
            state = job.state.as_str(),
            result = job.result_json.as_deref().unwrap_or(""),
            "ジョブ"
        );
        if job.state == JobState::Failed {
            return Err(job.message.unwrap_or_default());
        }
    }
    report(&session)
}

fn check(args: &[String]) -> Result<(), String> {
    let args = parse(args)?;
    let session = LibrarySession::open(&args.library, progress_sink(), kura_mac::media_backend())
        .map_err(|e| e.to_string())?;
    session.wait_ready();
    report(&session)
}

fn report(session: &LibrarySession) -> Result<(), String> {
    let info = session.info().map_err(|e| e.to_string())?;
    let (missing, orphans) = check_files(session.library().db(), session.library().layout())
        .map_err(|e| e.to_string())?;
    let staging = std::fs::read_dir(session.library().layout().staging_dir())
        .map(|d| d.count())
        .unwrap_or(0);
    tracing::info!(
        items = info.item_count,
        missing_files = missing,
        orphan_files = orphans,
        staging_files = staging,
        integrity_at_open = ?info.integrity,
        "ライブラリの状態"
    );
    Ok(())
}
