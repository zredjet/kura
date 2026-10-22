//! SVG の働き手の手間（`bench svg-worker --src <dir> [--limit N] [--parallel N] [--spawns N] [--each] [--deadline-ms N]`。
//! 決定 23）。
//!
//! - 子を起動する手間：小さな SVG の形を、働き手（この実行ファイルを `--kura-svg-worker` 付きで起動する。アプリと同じ）で
//!   `--spawns` 回（既定 50）読み、1 回あたりの時間を出す（プロセスの中で読む時間と比べる）。
//! - ふつうの SVG：`--src` の下の .svg（入れ子も。`--limit` 件まで）を 1 件ずつ、プロセスの中の固定の段と働き手で読み
//!   （`decode_path`：形と値で子を 2 回起動する）、1 件の時間（p50・p95・最大・合計）と、結果（値と読み取りの記録・
//!   エラーの言葉）が同じかを数える。
//! - `--parallel N`（既定 0 は測らない）：働き手で N 件ずつ同時に読んだ全体の時間（一括のプールの並びと同じ）。
//! - `--each`：1 件ずつ、形（`shape_path`）と値（`decode_path`）の時間をプロセスの中と働き手で出す（時間の上限を決める実測）。
//!   `--deadline-ms N` は働き手の時間の上限（形・値とも）を変える（既定は `svg_worker` の上限）。

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kura_core::codec_ext::svg_worker::{self, WorkerCommand};
use kura_core::edit::CodecSet;
use kura_engine::codec::{DecodeStamp, svg};
use kura_engine::doc::DecodeOptions;
use kura_engine::{EngineError, Limits, NoMonitor, Value};

/// `bench svg-worker` を実行する。
pub fn run(args: &[String]) -> Result<(), String> {
    let mut src = None;
    let mut limit = usize::MAX;
    let mut parallel = 0usize;
    let mut spawns = 50usize;
    let mut each = false;
    let mut deadline = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut num = |name: &str| {
            it.next()
                .and_then(|n| n.parse::<usize>().ok())
                .ok_or(format!("{name} の数が読めません"))
        };
        match arg.as_str() {
            "--src" => src = it.next().map(PathBuf::from),
            "--limit" => limit = num("--limit")?,
            "--parallel" => parallel = num("--parallel")?,
            "--spawns" => spawns = num("--spawns")?,
            "--deadline-ms" => deadline = Some(num("--deadline-ms")? as u64),
            "--each" => each = true,
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    let local = CodecSet::pinned_with(&kura_mac::pinned_options());
    let mut remote = CodecSet::pinned_with(&kura_mac::pinned_options());
    let deadlines =
        deadline.map_or_else(svg_worker::Deadlines::default, |ms| svg_worker::Deadlines {
            shape: Duration::from_millis(ms),
            decode: Duration::from_millis(ms),
        });
    svg_worker::install_with(&mut remote, WorkerCommand::current_exe(), deadlines)
        .map_err(|e| e.to_string())?;
    spawn_cost(&local, &remote, spawns)?;
    if let Some(src) = src {
        let mut files = Vec::new();
        collect(&src, &mut files);
        files.sort();
        files.truncate(limit);
        if each {
            for f in &files {
                one_by_one(&local, &remote, f);
            }
        }
        compare(&local, &remote, &files);
        if parallel > 0 {
            in_parallel(&remote, &files, parallel);
        }
    }
    Ok(())
}

/// `dir` の下の .svg（入れ子も。シンボリックリンクはたどらない）。
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(t) = e.file_type() else { continue };
        let p = e.path();
        if t.is_dir() {
            collect(&p, out);
        } else if t.is_file()
            && p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("svg"))
        {
            out.push(p);
        }
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// 並べた時間の p（0〜100）の近い順位の値。
fn pct(sorted: &[f64], p: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (sorted.len() * p).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn summary(label: &str, mut v: Vec<f64>) {
    v.sort_by(f64::total_cmp);
    let total: f64 = v.iter().sum();
    tracing::info!(
        n = v.len(),
        p50_ms = pct(&v, 50),
        p95_ms = pct(&v, 95),
        max_ms = v.last().copied().unwrap_or(0.0),
        total_ms = total,
        "{label}"
    );
}

/// 子を起動する手間（小さな SVG の形）。
fn spawn_cost(local: &CodecSet, remote: &CodecSet, n: usize) -> Result<(), String> {
    let tiny = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="4" height="4"/></svg>"#;
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let time = |set: &CodecSet| -> Result<Vec<f64>, String> {
        (0..n)
            .map(|_| {
                let t = Instant::now();
                set.shape(&mut Cursor::new(&tiny[..]), "svg", &opts, &limits)
                    .map_err(|e| e.to_string())?;
                Ok(ms(t.elapsed()))
            })
            .collect()
    };
    summary("小さな SVG の形（プロセスの中）", time(local)?);
    summary("小さな SVG の形（働き手。子の起動 1 回）", time(remote)?);
    Ok(())
}

type Read = Result<(Value, DecodeStamp), EngineError>;

fn read(set: &CodecSet, path: &Path) -> (Read, f64) {
    let t = Instant::now();
    let r = set.decode_path(
        path,
        "svg",
        &DecodeOptions::default(),
        &Limits::default(),
        &NoMonitor,
    );
    (r, ms(t.elapsed()))
}

/// 1 件の形と値の時間（プロセスの中・働き手）。
fn one_by_one(local: &CodecSet, remote: &CodecSet, path: &Path) {
    let (opts, limits) = (DecodeOptions::default(), Limits::default());
    let shape = |set: &CodecSet| {
        let t = Instant::now();
        let r = set.shape_path(path, "svg", &opts, &limits);
        (
            r.map(|(s, _)| s).map_err(|e| e.to_string()),
            ms(t.elapsed()),
        )
    };
    let (ls, ls_ms) = shape(local);
    let (rs, rs_ms) = shape(remote);
    let (ld, ld_ms) = read(local, path);
    let (rd, rd_ms) = read(remote, path);
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tracing::info!(
        file = %path.display(),
        bytes,
        local_shape_ms = ls_ms,
        worker_shape_ms = rs_ms,
        local_decode_ms = ld_ms,
        worker_decode_ms = rd_ms,
        shape = ?ls,
        same_shape = ls == rs,
        local = ?ld.as_ref().map(|r| r.1.value_digest).map_err(ToString::to_string),
        worker = ?rd.as_ref().map(|r| r.1.value_digest).map_err(ToString::to_string),
        "1 件の形と値"
    );
}

/// 1 件ずつ、プロセスの中と働き手で読んで比べる。
fn compare(local: &CodecSet, remote: &CodecSet, files: &[PathBuf]) {
    let (mut a, mut b, mut over) = (Vec::new(), Vec::new(), Vec::new());
    let (mut same, mut ok) = (0usize, 0usize);
    let mut differ = Vec::new();
    for f in files {
        let (ra, ta) = read(local, f);
        let (rb, tb) = read(remote, f);
        a.push(ta);
        b.push(tb);
        over.push(tb - ta);
        let equal = match (&ra, &rb) {
            (Ok(x), Ok(y)) => {
                ok += 1;
                x == y
            }
            (Err(x), Err(y)) => x.to_string() == y.to_string(),
            _ => false,
        };
        if equal {
            same += 1;
        } else if differ.len() < 10 {
            differ.push(format!(
                "{}: {:?} / {:?}",
                f.display(),
                ra.as_ref()
                    .map(|r| r.1.value_digest)
                    .map_err(ToString::to_string),
                rb.as_ref()
                    .map(|r| r.1.value_digest)
                    .map_err(ToString::to_string)
            ));
        }
    }
    tracing::info!(
        files = files.len(),
        read_ok = ok,
        same,
        ?differ,
        source_max = svg::MAX_SOURCE,
        "ふつうの SVG（形と値。値と記録・エラーが同じ数）"
    );
    summary("1 件（プロセスの中）", a);
    summary("1 件（働き手。子の起動 2 回）", b);
    summary("1 件の差（働き手 − プロセスの中）", over);
}

/// 働き手で `n` 件ずつ同時に読む。
fn in_parallel(remote: &CodecSet, files: &[PathBuf], n: usize) {
    let t = Instant::now();
    let mut times = Vec::new();
    for chunk in files.chunks(n) {
        std::thread::scope(|s| {
            let hs: Vec<_> = chunk
                .iter()
                .map(|f| s.spawn(|| read(remote, f).1))
                .collect();
            for h in hs {
                if let Ok(t) = h.join() {
                    times.push(t);
                }
            }
        });
    }
    tracing::info!(
        parallel = n,
        files = files.len(),
        total_ms = ms(t.elapsed()),
        per_second = files.len() as f64 / t.elapsed().as_secs_f64(),
        "働き手で同時に読んだ全体"
    );
    summary("1 件（働き手・同時）", times);
}
