//! 解析のジョブの速さとメモリ（`bench analyze --kind image|audio [--limit N] [--src <dir>] [--work <dir>] [--json <file>]`）。
//!
//! 1. 作業フォルダに新しいライブラリを作り、見本を入れる（`--src` があればそのフォルダの同じ種類のファイルを写し、なければ
//!    種類ごとの合成の見本を件数ぶん作る。中身はどれも違う：同じ中身の結果は読まずに使うので、測りにならない）。
//! 2. 自分を子プロセスとして `/usr/bin/time -l` で起こし（`--child`）、アプリと同じエンジンの部品（MacEngineFactory：
//!    物理メモリからの予算・固定の段 → macOS → ffmpeg）で解析のジョブを 1 本動かす。見本を作るメモリをピークに入れないため。
//! 3. 子が書いたジョブの結果（読んで解いた 1 件ごとの時間の p50・p95・最大と、HEIC・HEIF の見出しの読み取りの時間（決定 15。
//!    `header_*`））と、通しの件数/秒、peak memory footprint を 1 行の JSON で出す（`--json` があれば追記する）。
//!
//! 合成の見本（`--src` なし）：画像は 128² のドット絵・1024×768 の写真らしい PNG・512² の半透明の乱数 PNG・128² で 8 コマの
//! GIF を順に、音声は 30 秒の 44.1 kHz ステレオの WAV と FLAC（正弦波。ループ点つきを混ぜる）を順に。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_core::analysis::{AnalyzeHandler, AnalyzeShared};
use kura_core::edit::{ENGINE_WORK_DIR, EngineServices};
use kura_core::import::{MediaKind, classify};
use kura_core::jobs::{EventSink, JobFilter, JobHandler, JobKind, JobManager};
use kura_core::library::Library;
use kura_engine::NoMonitor;
use kura_engine::codec::CodecSet;
use kura_engine::testgen::InputGen;
use kura_engine::value::{LoopRange, PixelFormat};
use serde_json::{Value as J, json};

const USAGE: &str = "使い方: kura-devtools bench analyze --kind image|audio [--limit N] [--src <dir>] \
                     [--work <dir>] [--json <file>]";

struct Opts {
    kind: MediaKind,
    limit: usize,
    src: Option<PathBuf>,
    work: Option<PathBuf>,
    json: Option<PathBuf>,
    child: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts {
        kind: MediaKind::Image,
        limit: 0,
        src: None,
        work: None,
        json: None,
        child: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or(USAGE.to_string());
        match arg.as_str() {
            "--kind" => {
                opts.kind = match value()?.as_str() {
                    "image" => MediaKind::Image,
                    "audio" => MediaKind::Audio,
                    other => return Err(format!("--kind は image か audio です（{other}）")),
                }
            }
            "--limit" => opts.limit = value()?.parse().map_err(|_| USAGE.to_string())?,
            "--src" => opts.src = Some(PathBuf::from(value()?)),
            "--work" => opts.work = Some(PathBuf::from(value()?)),
            "--json" => opts.json = Some(PathBuf::from(value()?)),
            "--child" => opts.child = Some(PathBuf::from(value()?)),
            other => return Err(format!("不明な引数: {other}\n{USAGE}")),
        }
    }
    if opts.limit == 0 {
        opts.limit = match opts.kind {
            MediaKind::Audio => 40,
            _ => 200,
        };
    }
    Ok(opts)
}

/// `bench analyze …` の入口。
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    if let Some(library) = &opts.child {
        return child(library);
    }
    let work = opts
        .work
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("kura-bench-analyze"));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let path = work.join(format!(
        "analyze-{}-{}.kuralib",
        opts.kind.as_str(),
        std::process::id()
    ));
    let prepared = Instant::now();
    let items = prepare(&path, &opts)?;
    tracing::info!(
        items,
        ms = prepared.elapsed().as_millis() as u64,
        path = %path.display(),
        "見本を入れました"
    );
    let measured = spawn(&path);
    let _ = std::fs::remove_dir_all(&path);
    let (mut line, peak) = measured?;
    line["kind"] = json!(opts.kind.as_str());
    line["source"] = json!(if opts.src.is_some() {
        "src"
    } else {
        "synthetic"
    });
    line["peak_bytes"] = json!(peak);
    tracing::info!(result = %line, "解析の速さ");
    emit(&line)?;
    if let Some(file) = &opts.json {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// ライブラリを作って見本を入れる。入れた件数を返す。
fn prepare(path: &Path, opts: &Opts) -> Result<usize, String> {
    let library = Library::create(path).map_err(|e| e.to_string())?;
    let files: Vec<(Vec<u8>, &'static str)> = match &opts.src {
        Some(src) => from_dir(src, opts.kind, opts.limit)?,
        None => (0..opts.limit)
            .map(|i| synthetic(opts.kind, i as u64))
            .collect::<Result<_, _>>()?,
    };
    let n = files.len();
    let rows: Vec<([u8; 32], &'static str, i64)> = files
        .into_iter()
        .map(|(bytes, ext)| {
            let hash = *blake3::hash(&bytes).as_bytes();
            let dest = library.layout().content_path(&hash, ext);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
            Ok((hash, ext, bytes.len() as i64))
        })
        .collect::<Result<_, String>>()?;
    let kind = opts.kind.as_str();
    library
        .db()
        .write(move |tx| {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at,
                                              updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, ?2, ?3, ?4, 'bench', 'bench', ?5, ?5, 'ready', 'not_needed', 'pending')",
            )?;
            for (i, (hash, ext, size)) in rows.iter().enumerate() {
                stmt.execute(rusqlite::params![&hash[..], ext, kind, size, i as i64])?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    Ok(n)
}

/// フォルダの同じ種類のファイル（`limit` 件まで。名前の順）。
fn from_dir(
    src: &Path,
    kind: MediaKind,
    limit: usize,
) -> Result<Vec<(Vec<u8>, &'static str)>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(src)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if classify(&ext) != Some(kind) {
            continue;
        }
        let Some(ext) = kura_engine::codec::formats::format_by_ext(&ext)
            .and_then(|f| f.exts.iter().find(|e| **e == ext.as_str()).copied())
        else {
            continue;
        };
        out.push((std::fs::read(&p).map_err(|e| e.to_string())?, ext));
        if out.len() >= limit {
            break;
        }
    }
    if out.is_empty() {
        return Err(format!(
            "{} に {} のファイルがありません",
            src.display(),
            kind.as_str()
        ));
    }
    Ok(out)
}

fn encode(input: InputGen, format: &str, opts: J) -> Result<Vec<u8>, String> {
    let v = input.generate().map_err(|e| e.to_string())?;
    CodecSet::pinned()
        .encode_bytes(&v, format, &opts, &NoMonitor)
        .map(|(b, _)| b)
        .map_err(|e| e.to_string())
}

/// 合成の見本 `i` 番（種類ごとに順に回す。種で中身を変える）。
fn synthetic(kind: MediaKind, i: u64) -> Result<(Vec<u8>, &'static str), String> {
    let seed = i + 1;
    match kind {
        MediaKind::Audio => {
            let rate = 44_100;
            let frames = u64::from(rate) * 30;
            let loop_range = i.is_multiple_of(3).then_some(LoopRange {
                start: u64::from(rate),
                end: frames - u64::from(rate),
            });
            let input = InputGen::Sine {
                rate,
                channels: 2,
                frames,
                freq: 220 + (i as u32 % 200),
                loop_range,
            };
            if i.is_multiple_of(2) {
                Ok((encode(input, "wav", json!({ "bits": 16 }))?, "wav"))
            } else {
                Ok((encode(input, "flac", json!({ "bits": 16 }))?, "flac"))
            }
        }
        _ => match i % 4 {
            0 => Ok((
                encode(
                    InputGen::PixelSprite {
                        w: 128,
                        h: 128,
                        fmt: PixelFormat::Rgba8,
                        seed,
                    },
                    "png",
                    json!({}),
                )?,
                "png",
            )),
            1 => Ok((
                encode(
                    InputGen::Photo {
                        w: 1024,
                        h: 768,
                        seed,
                    },
                    "png",
                    json!({}),
                )?,
                "png",
            )),
            2 => Ok((
                encode(
                    InputGen::Noise {
                        w: 512,
                        h: 512,
                        fmt: PixelFormat::Rgba8,
                        seed,
                    },
                    "png",
                    json!({}),
                )?,
                "png",
            )),
            _ => Ok((
                encode(
                    InputGen::NoiseFrames {
                        w: 128,
                        h: 128,
                        fmt: PixelFormat::Rgba8,
                        count: 8,
                        seed,
                    },
                    "gif",
                    json!({}),
                )?,
                "gif",
            )),
        },
    }
}

/// 1 行の JSON を標準出力に書く（子の結果は親が読む）。
fn emit(line: &J) -> Result<(), String> {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").map_err(|e| e.to_string())
}

/// `/usr/bin/time -l` の出力の行 `<数> <名前>` を読む。
fn read_count(stderr: &str, name: &str) -> Option<u64> {
    stderr
        .lines()
        .find(|l| l.contains(name))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

/// 子を起こして、結果の JSON と peak memory footprint を読む（`/usr/bin/time` がなければピークは None）。
fn spawn(library: &Path) -> Result<(J, Option<u64>), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let child_args = |cmd: &mut Command| {
        cmd.args(["bench", "analyze", "--child"]).arg(library);
    };
    let mut timed = Command::new("/usr/bin/time");
    timed.arg("-l").arg(&exe);
    child_args(&mut timed);
    let out = match timed.output() {
        Ok(out) => out,
        Err(err) => {
            tracing::warn!(%err, "/usr/bin/time を起こせないので、ピークは測りません");
            let mut plain = Command::new(&exe);
            child_args(&mut plain);
            plain.output().map_err(|e| e.to_string())?
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<J>(l).ok())
        .ok_or_else(|| format!("子の結果がありません：{stderr}"))?;
    if !out.status.success() {
        return Err(format!("子が失敗しました：{stderr}"));
    }
    Ok((line, read_count(&stderr, "peak memory footprint")))
}

/// 子：ライブラリを開いて解析のジョブを 1 本動かし、結果を 1 行の JSON で出す。
fn child(path: &Path) -> Result<(), String> {
    let library = Arc::new(Library::open(path).map_err(|e| e.to_string())?);
    let engine = Arc::new(EngineServices::new(
        Arc::new(crate::MacEngineFactory),
        library.layout().staging_dir().join(ENGINE_WORK_DIR),
    ));
    let sink: EventSink = Arc::new(|_| {});
    let jobs = JobManager::new(
        library.clone(),
        vec![(
            JobKind::Analyze,
            Arc::new(AnalyzeHandler::new(
                engine.clone(),
                Arc::new(AnalyzeShared::default()),
            )) as Arc<dyn JobHandler>,
        )],
        sink,
    )
    .map_err(|e| e.to_string())?;
    // コーデックの組と実行の場は先に作る（ffmpeg を探す時間を測りに入れない）
    let built = Instant::now();
    engine.runtime().map_err(|e| e.to_string())?;
    let _codecs = engine.codecs();
    let setup_ms = built.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    jobs.ensure_queued(JobKind::Analyze, "{}")
        .map_err(|e| e.to_string())?;
    jobs.start().map_err(|e| e.to_string())?;
    if !jobs.wait_idle(Duration::from_secs(3600)) {
        return Err("1 時間待っても終わりませんでした".into());
    }
    let elapsed = started.elapsed().as_secs_f64();
    let record = jobs
        .list(&JobFilter {
            kinds: vec![JobKind::Analyze],
            states: vec![],
            limit: Some(1),
        })
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or("解析のジョブがありません")?;
    let result: J = serde_json::from_str(record.result_json.as_deref().unwrap_or("{}"))
        .map_err(|e| e.to_string())?;
    let done = result["done"].as_u64().unwrap_or(0);
    let budget = engine
        .runtime_if_built()
        .map(|r| r.budget().total())
        .unwrap_or(0);
    let line = json!({
        "state": record.state.as_str(),
        "items": done,
        "failed": result["failed"],
        "elapsed_ms": elapsed * 1000.0,
        "items_per_sec": done as f64 / elapsed.max(1e-9),
        "p50_ms": result["readMs"]["p50"],
        "p95_ms": result["readMs"]["p95"],
        "max_ms": result["readMs"]["max"],
        "header_count": result["headerMs"]["count"],
        "header_p50_ms": result["headerMs"]["p50"],
        "header_p95_ms": result["headerMs"]["p95"],
        "header_max_ms": result["headerMs"]["max"],
        "setup_ms": setup_ms,
        "budget_bytes": budget,
        "bulk_threads": engine.runtime_if_built().map(|r| r.bulk_threads()),
    });
    emit(&line)?;
    jobs.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成の見本はどれも中身が違い（同じ中身は読まずに使うので）、形式の表で読める拡張子を持つ。
    #[test]
    fn synthetic_samples_differ_and_are_readable() {
        let mut hashes = std::collections::BTreeSet::new();
        for i in 0..8 {
            let (bytes, ext) = synthetic(MediaKind::Image, i).unwrap();
            assert_eq!(classify(ext), Some(MediaKind::Image));
            assert!(hashes.insert(*blake3::hash(&bytes).as_bytes()));
        }
        let (bytes, ext) = synthetic(MediaKind::Audio, 1).unwrap();
        assert_eq!((ext, classify(ext)), ("flac", Some(MediaKind::Audio)));
        assert!(!bytes.is_empty());
    }

    #[test]
    fn reads_the_peak_from_time() {
        let stderr = "  1.0 real\n  98765432  peak memory footprint\n";
        assert_eq!(
            read_count(stderr, "peak memory footprint"),
            Some(98_765_432)
        );
        assert!(parse(&["--kind".into(), "video".into()]).is_err());
        assert_eq!(parse(&["--kind".into(), "audio".into()]).unwrap().limit, 40);
    }
}
