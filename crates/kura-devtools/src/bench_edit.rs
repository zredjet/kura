//! 編集のベンチ（`kura-devtools bench edit`。E30・requirements.md の NFR-11〜17・docs/editing.md §16）。
//!
//! ```text
//! kura-devtools bench edit [--preview-threads 3] [--bulk-threads 3] [--threads 3,4,16] [--budget 4GiB]
//!                          [--photo <file>] [--quick] [--only 11,12] [--runs 3] [--json <file>] [--markdown <file|->]
//! ```
//!
//! - 判定の条件（ADR-154 (1)(c)）：プレビューのプールを `--preview-threads`（既定 3）、一括のプールを `--bulk-threads`
//!   （既定 3）にして判定する。`--threads` に並べた数でも回し、記録する（判定はしない）。プールと予算は kura-core の
//!   `EngineRuntime`（アプリと同じ構成）。コーデックはアプリと同じ組（`kura_mac::full_codec_set`）。
//! - 「エンジンの中」（NFR-11・12）：編集を受けてから、プレビューの値と変わったタイルの一覧ができるまで（セッションの手・
//!   差分の計画・段の計画・予算の確保・表示の経路（`Engine::render_view`：前回の表示から変わったタイルだけを描き直す）。
//!   ダイジェスト・エンコード・IPC・WebKit を含めない）。
//! - 回数：NFR-11・12 は慣らし 10 回のあと 30 回（p50・p95。最近順位法）、NFR-13 は慣らし 2 回のあと 10 回・NFR-14 は
//!   慣らし 1 回のあと通し 3 回・NFR-15 は慣らし 1 回のあと 5 回（中央値・最大）。`--quick` は極小の大きさで 2 回ずつ
//!   （煙のテスト。debug でも回る）。`--runs` で全体を繰り返し、表・Markdown は回をまたいだ幅を出す。回の前後とケース
//!   ごとにロードアベレージ（`sysctl -n vm.loadavg`）を記録する。
//! - NFR-16：NFR-11・12・13・14・15 の判定の条件の最後の文書・結果を、別のエンジン・キャッシュなしの一括の経路（1・一括の数・
//!   16 スレッド）で作り直し、値のダイジェストと書き出しのバイト列が同じかを照らす。
//! - NFR-17：自分を子プロセスとして `/usr/bin/time -l` で起こし（`--child mem:<ケース>`）、peak memory footprint から基準
//!   （実行の場を作るだけの子。保存はアプリのコーデックの組も作る子）を引いた量を予算・見積もりと比べる。小さい予算では
//!   落ちずに Limit になること。
//! - 写真のプレビュー（NFR-11 の 24MP）は縮小の段の計画（`Engine::plan_level`）で描き、段 2 で判定して段 1 も記録する
//!   （ケースの名前に `-level1`）。段の計画にできなければ等倍で描いて記録だけにする。実写真は `--photo` で渡す（なければ
//!   合成）。
//! - 出力：表（標準出力に 1 行ずつ）、`--json`（環境・回・レコード）、`--markdown`（architecture.md §13 の M9 の表の形）。

mod batch;
mod editor;
mod mem;
mod report;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use kura_core::edit::{EngineRuntime, RuntimeConfig};
use kura_engine::budget::{default_reserved, default_total};
use kura_engine::codec::{CodecSet, format_by_ext};
use kura_engine::doc::DecodeOptions;
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::cases::save_jpeg;
use kura_engine::{Limits, NoMonitor, Value};
use serde_json::{Value as J, json};

use report::{Record, Report, emit};

const USAGE: &str = "使い方: kura-devtools bench edit [--preview-threads N] [--bulk-threads N] [--threads 3,4,16] \
                     [--budget <バイト|MiB|GiB>] [--photo <file>] [--quick] [--only 11,12,…] [--runs N] \
                     [--json <file>] [--markdown <file|->]";

const MIB: u64 = 1 << 20;

/// 引数。
#[derive(Debug, Clone)]
pub(crate) struct Opts {
    pub preview_threads: usize,
    pub bulk_threads: usize,
    /// 記録のために回すスレッドの数（判定の数は別に足す）。
    pub threads: Vec<usize>,
    pub budget: Option<u64>,
    pub photo: Option<PathBuf>,
    pub quick: bool,
    /// 回す NFR（空ならすべて）。
    pub only: BTreeSet<u8>,
    pub json: Option<PathBuf>,
    pub markdown: Option<PathBuf>,
    pub runs: usize,
    /// 子プロセスのケース（`mem:<ケース>`）。
    pub child: Option<String>,
    /// 子プロセスの入力のファイルと形式。
    pub input: Option<PathBuf>,
    pub format: Option<String>,
}

fn number<T: std::str::FromStr>(name: &str, v: Option<&String>) -> Result<T, String> {
    v.and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("{name} の値が読めません。{USAGE}"))
}

/// バイト数（`4GiB`・`512MiB`・`64KiB`・数字だけ）。
fn bytes(v: Option<&String>) -> Result<u64, String> {
    let s = v.ok_or_else(|| format!("--budget の値がありません。{USAGE}"))?;
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s.as_str(), ""),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("--budget の値が読めません: {s}"))?;
    let scale = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kib" => 1 << 10,
        "m" | "mib" => MIB,
        "g" | "gib" => 1 << 30,
        _ => return Err(format!("--budget の単位が読めません: {s}")),
    };
    n.checked_mul(scale)
        .filter(|b| *b > 0)
        .ok_or_else(|| format!("--budget の値が大きすぎるか 0 です: {s}"))
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        preview_threads: 3,
        bulk_threads: 3,
        threads: Vec::new(),
        budget: None,
        photo: None,
        quick: false,
        only: BTreeSet::new(),
        json: None,
        markdown: None,
        runs: 1,
        child: None,
        input: None,
        format: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--preview-threads" => o.preview_threads = number(arg, it.next())?,
            "--bulk-threads" => o.bulk_threads = number(arg, it.next())?,
            "--threads" => {
                let list = it
                    .next()
                    .ok_or_else(|| format!("--threads の値がありません。{USAGE}"))?;
                o.threads = list
                    .split(',')
                    .map(|n| n.trim().parse::<usize>().ok().filter(|n| *n > 0))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| format!("--threads の値が読めません: {list}"))?;
            }
            "--budget" => o.budget = Some(bytes(it.next())?),
            "--photo" => o.photo = it.next().map(PathBuf::from),
            "--quick" => o.quick = true,
            "--only" => {
                let list = it
                    .next()
                    .ok_or_else(|| format!("--only の値がありません。{USAGE}"))?;
                for n in list.split(',') {
                    let nfr = n
                        .trim()
                        .trim_start_matches("NFR-")
                        .parse::<u8>()
                        .ok()
                        .filter(|n| (11..=17).contains(n))
                        .ok_or_else(|| format!("--only は 11〜17 です: {list}"))?;
                    o.only.insert(nfr);
                }
            }
            "--json" => o.json = it.next().map(PathBuf::from),
            "--markdown" => o.markdown = it.next().map(PathBuf::from),
            "--runs" => o.runs = number::<usize>(arg, it.next())?.max(1),
            "--child" => o.child = it.next().cloned(),
            "--in" => o.input = it.next().map(PathBuf::from),
            "--format" => o.format = it.next().cloned(),
            other => return Err(format!("知らない引数です: {other}。{USAGE}")),
        }
    }
    if o.preview_threads == 0 || o.bulk_threads == 0 {
        return Err(format!("スレッドの数は 1 以上です。{USAGE}"));
    }
    Ok(o)
}

/// 大きさと回数（`--quick` は極小で 2 回ずつ）。
#[derive(Debug, Clone)]
pub(crate) struct Sizes {
    pub pixel: u32,
    pub audio_rate: u32,
    pub audio_secs: usize,
    pub photo: (u32, u32),
    pub canvas: u32,
    pub priors: Vec<usize>,
    pub stroke_sizes: Vec<u8>,
    pub nearest_sides: Vec<u32>,
    pub nearest_strokes: Vec<u8>,
    pub edit_n: usize,
    pub edit_warmup: usize,
    pub save_n: usize,
    pub save_warmup: usize,
    pub bulk_items: usize,
    pub bulk_n: usize,
    pub bulk_warmup: usize,
    pub analysis_rate: u32,
    pub analysis_secs: usize,
    pub analysis_n: usize,
    pub analysis_warmup: usize,
    pub mem_canvas: u32,
    pub mem_prior: usize,
    pub mem_strokes: usize,
    pub limit_save_budget: u64,
    pub limit_editor_budget: u64,
}

impl Sizes {
    fn new(quick: bool) -> Self {
        if quick {
            Self {
                pixel: 64,
                audio_rate: 8_000,
                audio_secs: 2,
                photo: (120, 80),
                canvas: 128,
                priors: vec![0, 20],
                // 64px は範囲外の記録の道（NFR-12 は 32px まで）を通すため
                stroke_sizes: vec![1, 8, 64],
                nearest_sides: vec![32],
                nearest_strokes: vec![8],
                edit_n: 2,
                edit_warmup: 1,
                save_n: 2,
                save_warmup: 0,
                bulk_items: 20,
                bulk_n: 2,
                bulk_warmup: 0,
                analysis_rate: 8_000,
                analysis_secs: 3,
                analysis_n: 2,
                analysis_warmup: 0,
                mem_canvas: 128,
                mem_prior: 20,
                mem_strokes: 2,
                limit_save_budget: 16 << 10,
                limit_editor_budget: 8 << 10,
            }
        } else {
            Self {
                pixel: 2048,
                audio_rate: 44_100,
                audio_secs: 300,
                photo: (
                    kura_engine::testing::bench::PHOTO_W,
                    kura_engine::testing::bench::PHOTO_H,
                ),
                canvas: 4096,
                priors: vec![0, 999, 50_000],
                stroke_sizes: vec![1, 8, 32, 64],
                nearest_sides: vec![1024, 2048],
                nearest_strokes: vec![8, 32],
                edit_n: 30,
                edit_warmup: kura_engine::testing::bench::WARMUP,
                save_n: 10,
                save_warmup: 2,
                bulk_items: 1_000,
                bulk_n: 3,
                bulk_warmup: 1,
                analysis_rate: 44_100,
                analysis_secs: 300,
                analysis_n: 5,
                analysis_warmup: 1,
                mem_canvas: 4096,
                mem_prior: 999,
                mem_strokes: 20,
                limit_save_budget: 128 * MIB,
                limit_editor_budget: 64 * MIB,
            }
        }
    }
}

/// 写真の入力（合成か `--photo` の実写真）。ファイルに書いたものを保存のベンチが読む。
#[derive(Clone)]
pub(crate) struct Photo {
    /// 表の注（「合成」「実写真（名前）」）。
    pub label: String,
    /// ケースの名前の後ろに付けるもの（合成は空、実写真は `-photo`）。
    pub suffix: &'static str,
    pub path: PathBuf,
    pub format: &'static str,
    pub value: Value,
}

/// ベンチの状態（回をまたいで持つもの）。
pub(crate) struct Ctx {
    pub opts: Opts,
    pub sizes: Sizes,
    pub run: usize,
    pub records: Vec<Record>,
    /// アプリと同じコーデックの組。
    pub codecs: CodecSet,
    /// 書き出しのバイト列の照合に使う固定の組（NFR-16）。
    pub pinned: CodecSet,
    pub tmp: PathBuf,
    synthetic: Option<Photo>,
    real: Option<Photo>,
}

impl Ctx {
    /// レコードを足して、表の行を出す。
    pub fn push(&mut self, r: Record) {
        emit(&r.line());
        self.records.push(r);
    }

    /// 回す NFR か。
    pub fn wants(&self, nfr: u8) -> bool {
        self.opts.only.is_empty() || self.opts.only.contains(&nfr)
    }

    /// 予算の全体（`--budget` か、物理メモリからの既定）。
    pub fn budget_total(&self) -> u64 {
        self.opts
            .budget
            .unwrap_or_else(|| default_total(kura_mac::physical_memory()))
    }

    /// プレビュー・一括のプールをどちらも `threads` にした実行の場。
    pub fn runtime(&self, threads: usize) -> Result<EngineRuntime, String> {
        runtime(threads, threads, self.opts.budget)
    }

    /// エディタの NFR（11・12）を回すスレッドの数：判定の数と `--threads`。
    pub fn preview_set(&self) -> Vec<usize> {
        let mut s: BTreeSet<usize> = self.opts.threads.iter().copied().collect();
        s.insert(self.opts.preview_threads);
        s.into_iter().collect()
    }

    /// 一括の NFR（13〜15）を回すスレッドの数：判定の数と `--threads`。
    pub fn bulk_set(&self) -> Vec<usize> {
        let mut s: BTreeSet<usize> = self.opts.threads.iter().copied().collect();
        s.insert(self.opts.bulk_threads);
        s.into_iter().collect()
    }

    /// 一括の経路で作り直すスレッドの数（NFR-16）。
    pub fn check_threads(&self) -> Vec<usize> {
        let s: BTreeSet<usize> = [1, self.opts.bulk_threads, 16].into_iter().collect();
        s.into_iter().collect()
    }

    /// 合成の写真（24MP。`--quick` は極小）を作り、アプリの JPEG（q90）でファイルに書く（はじめの 1 回だけ）。
    pub fn synthetic(&mut self) -> Result<&Photo, String> {
        if self.synthetic.is_none() {
            let (w, h) = self.sizes.photo;
            let value = InputGen::Photo { w, h, seed: 7 }
                .generate()
                .map_err(|e| e.to_string())?;
            let (bytes, _) = self
                .codecs
                .encode_bytes(&value, "jpeg", &save_jpeg(), &NoMonitor)
                .map_err(|e| e.to_string())?;
            let path = self.tmp.join("synthetic.jpg");
            std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
            self.synthetic = Some(Photo {
                label: format!("合成の写真 {w}×{h}"),
                suffix: "",
                path,
                format: "jpeg",
                value,
            });
        }
        self.synthetic
            .as_ref()
            .ok_or_else(|| "合成の写真を作れません".to_string())
    }

    /// `--photo` の実写真（なければ None）。
    pub fn real(&self) -> Option<&Photo> {
        self.real.as_ref()
    }

    /// 写真のプレビュー・メモリのケースに使う写真（実写真があればそれ）。
    pub fn photo(&mut self) -> Result<&Photo, String> {
        if self.real.is_some() {
            return self
                .real
                .as_ref()
                .ok_or_else(|| "実写真がありません".to_string());
        }
        self.synthetic()
    }
}

/// プレビュー・一括のプールと予算の実行の場（予算は `budget`、なければ物理メモリからの既定）。
pub(crate) fn runtime(
    preview: usize,
    bulk: usize,
    budget: Option<u64>,
) -> Result<EngineRuntime, String> {
    EngineRuntime::new(&RuntimeConfig {
        physical_memory: kura_mac::physical_memory(),
        budget,
        preview_threads: Some(preview),
        bulk_threads: Some(bulk),
        ..RuntimeConfig::default()
    })
    .map_err(|e| e.to_string())
}

/// 単調に増えるナノ秒の時計（`testing::bench::measure` に渡す）。
pub(crate) fn clock() -> impl Fn() -> u64 {
    let start = Instant::now();
    move || u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// コマンドの標準出力（失敗したら None）。
fn command(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// ロードアベレージ（1・5・15 分）。`sysctl -n vm.loadavg`（`{ 4.52 5.01 5.33 }`）か `uptime` から。
pub(crate) fn loadavg() -> Option<[f64; 3]> {
    let text = command("sysctl", &["-n", "vm.loadavg"]).or_else(|| {
        command("uptime", &[]).and_then(|u| {
            u.split("averages:")
                .nth(1)
                .or_else(|| u.split("average:").nth(1))
                .map(str::to_string)
        })
    })?;
    let nums: Vec<f64> = text
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{' || c == '}')
        .filter_map(|s| s.parse().ok())
        .collect();
    match nums.as_slice() {
        [a, b, c, ..] => Some([*a, *b, *c]),
        _ => None,
    }
}

/// 1 分のロードアベレージ。
pub(crate) fn load1() -> Option<f64> {
    loadavg().map(|l| l[0])
}

fn env_json(opts: &Opts, budget: u64) -> J {
    let sysctl = |k: &str| command("sysctl", &["-n", k]);
    json!({
        "cpu": sysctl("machdep.cpu.brand_string").unwrap_or_else(|| "不明".into()),
        "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "perf_cores": sysctl("hw.perflevel0.physicalcpu").and_then(|s| s.parse::<u64>().ok()),
        "eff_cores": sysctl("hw.perflevel1.physicalcpu").and_then(|s| s.parse::<u64>().ok()),
        "memory_bytes": kura_mac::physical_memory(),
        "os": command("sw_vers", &["-productVersion"]),
        "commit": command("git", &["rev-parse", "--short", "HEAD"]),
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "preview_threads": opts.preview_threads,
        "bulk_threads": opts.bulk_threads,
        "threads": opts.threads,
        "budget_bytes": budget,
        "reserved_bytes": default_reserved(budget),
        "quick": opts.quick,
        "photo": opts.photo.as_ref().map(|p| p.display().to_string()),
        "runs": opts.runs,
        "only": opts.only,
    })
}

/// `--photo` を読む（形式は拡張子から。アプリと同じコーデックの組）。
fn load_photo(codecs: &CodecSet, path: &Path) -> Result<Photo, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let format = format_by_ext(ext)
        .map(|f| f.id)
        .ok_or_else(|| format!("{} の形式が分かりません", path.display()))?;
    let (value, _) = codecs
        .decode_path(
            path,
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .map_err(|e| format!("{} を読めません: {e}", path.display()))?;
    Ok(Photo {
        label: format!(
            "実写真 {}",
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        ),
        suffix: "-photo",
        path: path.to_path_buf(),
        format,
        value,
    })
}

/// `bench edit` の入口。
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    if cfg!(debug_assertions) && !opts.quick {
        return Err(
            "bench edit は release で回してください（cargo run --release -p kura-devtools -- bench edit）。\
             debug では --quick だけ回せます"
                .into(),
        );
    }
    if let Some(child) = &opts.child {
        return mem::child(&opts, child);
    }
    let tmp = std::env::temp_dir().join(format!("kura-bench-edit-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = run_all(opts, &tmp);
    // 一時のファイルは測り終えたら消す（失敗しても結果は返す）
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn run_all(opts: Opts, tmp: &Path) -> Result<(), String> {
    // アプリと同じ組（SVG の文字はシステムのフォントで描く）
    let codecs = kura_mac::full_codec_set(&kura_mac::pinned_options(), &tmp.join("codec"));
    let real = match &opts.photo {
        Some(p) => Some(load_photo(&codecs, p)?),
        None => None,
    };
    let mut ctx = Ctx {
        sizes: Sizes::new(opts.quick),
        opts,
        run: 0,
        records: Vec::new(),
        codecs,
        pinned: CodecSet::pinned(),
        tmp: tmp.to_path_buf(),
        synthetic: None,
        real,
    };
    let budget = ctx.budget_total();
    let env = env_json(&ctx.opts, budget);
    emit(&format!(
        "bench edit：{}・{} コア・{}・プレビュー {} スレッド・一括 {} スレッド・記録 {:?}・予算 {} MiB{}",
        env["cpu"].as_str().unwrap_or("不明"),
        env["cores"],
        env["build"].as_str().unwrap_or(""),
        ctx.opts.preview_threads,
        ctx.opts.bulk_threads,
        ctx.opts.threads,
        budget / MIB,
        if ctx.opts.quick { "・--quick" } else { "" }
    ));
    let mut runs = Vec::new();
    for run in 0..ctx.opts.runs {
        ctx.run = run;
        let before = loadavg();
        let started = Instant::now();
        emit(&format!(
            "== {} 回目（ロードアベレージ {:?}）==",
            run + 1,
            before
        ));
        run_once(&mut ctx)?;
        let after = loadavg();
        runs.push(json!({
            "run": run,
            "loadavg_before": before,
            "loadavg_after": after,
            "seconds": started.elapsed().as_secs_f64(),
        }));
    }
    let report = Report {
        env,
        runs,
        records: std::mem::take(&mut ctx.records),
    };
    let md = report.markdown();
    if let Some(path) = &ctx.opts.json {
        let text = serde_json::to_string_pretty(&report.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{} に書けません: {e}", path.display()))?;
    }
    // Markdown は表のあとに出す（`--markdown <file>` ならファイルにも書く。`-` は出すだけ）
    if let Some(p) = &ctx.opts.markdown
        && p.as_os_str() != "-"
    {
        std::fs::write(p, &md).map_err(|e| format!("{} に書けません: {e}", p.display()))?;
    }
    emit("");
    emit(&md);
    Ok(())
}

/// 全体を 1 回。NFR-16 は、照らす相手（11〜15）のケースを回しながら記録する（16 だけを選んだときは時間を記録しない）。
fn run_once(ctx: &mut Ctx) -> Result<(), String> {
    let check = ctx.wants(16);
    if ctx.wants(11) || check {
        editor::preview(ctx, ctx.wants(11), check)?;
    }
    if ctx.wants(12) || check {
        editor::paint(ctx, ctx.wants(12), check)?;
    }
    if ctx.wants(13) || check {
        batch::save(ctx, ctx.wants(13), check)?;
    }
    if ctx.wants(14) || check {
        batch::bulk(ctx, ctx.wants(14), check)?;
    }
    if ctx.wants(15) || check {
        batch::analysis(ctx, ctx.wants(15), check)?;
    }
    if ctx.wants(17) {
        mem::memory(ctx)?;
    }
    Ok(())
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
            "--preview-threads 4 --bulk-threads 12 --threads 3,4,16 --budget 2GiB --quick --only 11,NFR-17 --runs 3 --json a.json --markdown -",
        ))
        .unwrap();
        assert_eq!((o.preview_threads, o.bulk_threads), (4, 12));
        assert_eq!(o.threads, vec![3, 4, 16]);
        assert_eq!(o.budget, Some(2 << 30));
        assert!(o.quick);
        assert_eq!(o.only, BTreeSet::from([11, 17]));
        assert_eq!(o.runs, 3);
        assert_eq!(o.markdown, Some(PathBuf::from("-")));
        let d = parse(&[]).unwrap();
        assert_eq!((d.preview_threads, d.bulk_threads, d.runs), (3, 3, 1));
        assert!(d.only.is_empty() && d.threads.is_empty());
        assert_eq!(bytes(Some(&"512MiB".into())).unwrap(), 512 * MIB);
        assert_eq!(bytes(Some(&"64k".into())).unwrap(), 64 << 10);
        assert_eq!(bytes(Some(&"1000".into())).unwrap(), 1000);
        for bad in [
            "--only 99",
            "--only 10",
            "--threads 3,0",
            "--budget 4XB",
            "--budget 0",
            "--preview-threads 0",
            "--what",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
        assert!(parse(&args("--only 99")).unwrap_err().contains("--only"));
    }

    #[test]
    fn thread_sets_include_the_judged_counts() {
        let opts = parse(&args("--threads 4,16 --bulk-threads 12")).unwrap();
        let ctx = Ctx {
            sizes: Sizes::new(true),
            opts,
            run: 0,
            records: Vec::new(),
            codecs: CodecSet::pinned(),
            pinned: CodecSet::pinned(),
            tmp: PathBuf::new(),
            synthetic: None,
            real: None,
        };
        assert_eq!(ctx.preview_set(), vec![3, 4, 16]);
        assert_eq!(ctx.bulk_set(), vec![4, 12, 16]);
        assert_eq!(ctx.check_threads(), vec![1, 12, 16]);
        assert!(ctx.wants(11) && ctx.wants(17));
    }

    #[test]
    fn loadavg_reads_three_numbers() {
        // macOS では sysctl、ほかでは uptime から読める（どちらもなければ None で、表は「不明」）
        if let Some(l) = loadavg() {
            assert!(l.iter().all(|v| *v >= 0.0));
        }
    }
}
