//! 開発用ツール。
//!
//! - `import --library <path> [--create] [--close-after-ms N] [<src>...]`：取り込む（<src> を省くと、中断していた取り込みの
//!   再開だけ待つ。`--close-after-ms` は N ms 待ってジョブの途中でライブラリを閉じ、閉じるのにかかった時間を出す）
//! - `download --library <path> <url>...`：アプリと同じジョブで取得してライブラリに入れる（M7 の確認用）
//! - `check --library <path>`：件数と整合性（DB とファイルの食い違い）を表示する
//! - `seed --items <件数> --out <path> [--analysis pending|ready]`：ダミーライブラリを作る（グリッドと起動時間の計測用。
//!   `--analysis ready` は画像・音声に偽の事実を入れて解析済みにする）
//! - `assets --out <dir>`：形式の表の画像・フォント・3D の行ごとの見本と、動画・音声の見本を書く（M11 の形式の網羅。
//!   `assets` の冒頭）
//! - `selftest-library --out <path> [--web <dir>] [--font <ttf>]`：kura:// と CSP を実機で確かめるライブラリ
//!   （`VITE_KURA_SELFTEST=1` のフロントが試す。`selftest` の冒頭）
//! - `bench query --library <path> [--fake-facts]`：一覧の取得の速さを測る（`--fake-facts` は素材の事実の擬似の行を
//!   入れて、透過・ループ・ラウドネスなどの検索も測り、終わったら消す。`bench_facts` の冒頭）
//! - `bench thumbs --src <画像のフォルダ> [--threads N]`：画像サムネイルの速さを測る（NFR-04）
//! - `bench thumbs-job --src <dir> [--threads N] [--engine-cap MiB] [--no-engine] [--out <dir>]`：サムネイルのジョブ
//!   （道の割り振り・エンジンの道の予算込み）の速さを測る（NFR-04。M11。`bench_thumbs` の冒頭）
//! - `bench migrate (--library <path> | --items <件数> [--history]) [--runs 3]`：マイグレーションの時間と照合
//!   （docs/data-model.md §5。`bench_migrate` の冒頭）
//! - `bench svg-worker [--src <dir>] [--limit N] [--parallel N] [--spawns N]`：SVG の働き手（別のプロセスで読む。決定 23）の
//!   子の起動の手間と、ふつうの SVG をプロセスの中と働き手で読んだ時間・結果の一致（`bench_svg` の冒頭）
//! - `bench analyze --kind image|audio [--limit N] [--src <dir>] [--json <file>]`：解析のジョブの件数/秒・1 件の
//!   p50・p95・ピークのメモリ（`bench_analyze` の冒頭）
//! - `bench edit [--quick] [--threads 3,4,16] [--photo <file>] [--json <file>] [--markdown <file>] …`：編集の性能
//!   （NFR-11〜17。`bench_edit` の冒頭）
//! - `edit shape --in <file>` / `edit render --doc <文書.json> --in <file> --out <file>`：編集のエンジンを手で確かめる（docs/editing.md）
//! - `vorbis-quality --out <dir> [--quick] [--input <file>]... [--reference <音>/<ラベル>=<ogg>]...`：
//!   自作の Ogg Vorbis のエンコーダの品質を記録する（M10 段 7。`vorbis_quality` の冒頭）

/// アプリと同じ確保器（大きな確保を OS へすぐ返す。計測をアプリに合わせる。SVG の働き手のときだけ確保の上限を付ける）。
#[global_allocator]
static ALLOC: kura_mac::ReturningAllocator = kura_mac::ReturningAllocator::new();

mod assets;
mod bench;
mod bench_analyze;
mod bench_edit;
mod bench_facts;
mod bench_migrate;
mod bench_svg;
mod bench_thumbs;
mod edit;
mod mem_smoke;
mod seed;
mod selftest;
mod vorbis_quality;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kura_core::codec_ext::svg_worker::{self, WorkerCommand};
use kura_core::download::{CookieSource, DownloadInput, DownloadOptions, detect, start_download};
use kura_core::edit::{CodecSet, EngineFactory, RuntimeConfig};
use kura_core::import::{ImportInput, ImportOptions, start_import};
use kura_core::jobs::{EventSink, JobFilter, JobKind, JobState};
use kura_core::library::{LibrarySession, Services, UpgradeEvent, UpgradeHooks, check_files};

const USAGE: &str = "使い方: kura-devtools import --library <path> [--create] [<src>...] | \
                     download --library <path> <url>... | check --library <path> | \
                     vorbis-quality --out <dir> [--quick] [--input <file>]... [--reference <音>/<ラベル>=<ogg>]...";

fn main() -> ExitCode {
    // SVG の働き手として起動された（アプリと同じ組で SVG を読むとき、自分を `--kura-svg-worker` 付きで起動する。決定 23）
    if svg_worker::is_worker_invocation() {
        return svg_worker::serve(&kura_mac::pinned_options(), ALLOC.meter());
    }
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
        Some("assets") => assets::run(&args[1..]),
        Some("selftest-library") => selftest::run(&args[1..]),
        Some("bench") => bench::run(&args[1..]),
        Some("edit") => edit::run(&args[1..]),
        Some("vorbis-quality") => vorbis_quality::run(&args[1..]),
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
    /// 取り込みを積んでからこれだけ待ってライブラリを閉じ、閉じるのにかかった時間を出す（終わるのを待たない）。
    close_after: Option<Duration>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut library = None;
    let mut create = false;
    let mut sources = Vec::new();
    let mut close_after = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--library" => library = it.next().map(PathBuf::from),
            "--create" => create = true,
            "--close-after-ms" => {
                close_after = it
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .map(Duration::from_millis);
            }
            other => sources.push(PathBuf::from(other)),
        }
    }
    Ok(Args {
        library: library.ok_or(USAGE)?,
        create,
        sources,
        close_after,
    })
}

/// アプリ（src-tauri の `MacEngineFactory`）と同じエンジンの部品の作り方：物理メモリは kura-mac、コーデックの組は
/// [`app_codecs`]。
struct MacEngineFactory;

impl EngineFactory for MacEngineFactory {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            physical_memory: kura_mac::physical_memory(),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, work_dir: &std::path::Path) -> CodecSet {
        app_codecs(work_dir)
    }
}

/// アプリと同じコーデックの組：固定の段（SVG の文字はシステムのフォント）→ macOS → ffmpeg。SVG は自分を働き手として起動した
/// 別のプロセスで読む（決定 23。`svg_worker`）。`work_dir` は ffmpeg の一時ファイルの置き場。
pub(crate) fn app_codecs(work_dir: &std::path::Path) -> CodecSet {
    let opts = kura_mac::pinned_options();
    let mut set = kura_mac::full_codec_set(&opts, work_dir);
    if let Some(cmd) = svg_worker_command()
        && let Err(e) = svg_worker::install(&mut set, &opts, cmd)
    {
        tracing::error!("SVG の働き手を組めません: {e}");
    }
    set
}

/// SVG の働き手を起動するコマンド：自分の実行ファイル（`--kura-svg-worker` 付き。起動のたびに kura-mac の
/// `current_executable` で今の場所を求める）。
#[cfg(not(test))]
fn svg_worker_command() -> Option<WorkerCommand> {
    Some(WorkerCommand::own_exe(kura_mac::current_executable))
}

/// テストのハーネス（`target/<profile>/deps/kura_devtools-…`）は働き手になれないので、cargo が隣に作る実行ファイル
/// （`target/<profile>/kura-devtools`。結合テスト `tests/svg_worker.rs` が使うので `cargo test -p kura-devtools` で作られる）を
/// 働き手にする。なければ（単体テストだけを組んだとき）SVG はプロセスの中で読む。
#[cfg(test)]
fn svg_worker_command() -> Option<WorkerCommand> {
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?.parent()?.join("kura-devtools");
    bin.is_file()
        .then(|| WorkerCommand::new(bin, [svg_worker::WORKER_ARG]))
}

/// アプリと同じ渡しもの（メディアの実装・エンジンの部品・DB を新しい版にするときの空きの確かめ）。知らせはログに出す。
fn services() -> Services {
    Services::new(kura_mac::media_backend())
        .with_engine(Arc::new(MacEngineFactory))
        .with_upgrade(UpgradeHooks {
            free_space: Some(Arc::new(|path: &std::path::Path| {
                kura_mac::volume_capacity(path).map(|v| v.available_bytes)
            })),
            notify: Some(Arc::new(|event: &UpgradeEvent| {
                tracing::info!(?event, "ライブラリの DB の更新");
            })),
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
    let session = LibrarySession::open(&args.library, progress_sink(), services())
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
        LibrarySession::create(&args.library, progress_sink(), services())
    } else {
        LibrarySession::open(&args.library, progress_sink(), services())
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
    if let Some(after) = args.close_after {
        // 動いているジョブ（サムネイル・解析が SVG の働き手を待っているところなど）の途中で閉じる
        std::thread::sleep(after);
        let started = std::time::Instant::now();
        drop(session);
        tracing::info!(
            close_ms = started.elapsed().as_secs_f64() * 1000.0,
            "ライブラリを閉じました"
        );
        return Ok(());
    }
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
    let session = LibrarySession::open(&args.library, progress_sink(), services())
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kura_engine::doc::DecodeOptions;
    use kura_engine::{Limits, NoMonitor, Value};

    use super::*;

    /// 開く道はアプリと同じ部品を渡す：エンジンはライブラリごとに、使うまで作らない。作ると、予算は物理メモリから決まり、
    /// コーデックの組は macOS の段と、システムのフォント（文字を含む SVG を読める）を持つ。
    #[test]
    fn sessions_get_the_same_engine_as_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let session = LibrarySession::create(&dir.path().join("L"), progress_sink(), services())
            .expect("ライブラリを作る");
        session.wait_ready();
        let engine = session.engine().expect("エンジンの部品");
        assert_eq!(
            engine.work_dir(),
            dir.path().join("L.kuralib/staging/engine")
        );
        assert!(engine.runtime_if_built().is_none());
        assert!(engine.codecs_if_built().is_none());

        let runtime = engine.runtime().unwrap();
        assert_eq!(
            runtime.budget().total(),
            kura_engine::budget::default_total(kura_mac::physical_memory())
        );
        let codecs = engine.codecs();
        if cfg!(target_os = "macos") {
            let heic = codecs
                .capabilities()
                .into_iter()
                .find(|c| c.format == "heic")
                .unwrap();
            assert!(
                heic.read
                    .iter()
                    .any(|r| r.codec == kura_mac::codec::IMAGEIO_DEC),
                "{:?}",
                heic.read
            );
            let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="40"><text x="4" y="30" font-family="sans-serif" font-size="24">Kq</text></svg>"##;
            let (value, _) = codecs
                .decode(
                    &mut Cursor::new(svg.as_bytes().to_vec()),
                    "svg",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
                .expect("文字を含む SVG を読める");
            assert!(matches!(value, Value::Image(_)));
        }
    }
}
