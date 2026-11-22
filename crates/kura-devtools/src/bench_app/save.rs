//! `kura-devtools bench save`：保存のジョブの全体（NFR-13 のアプリの道の分）。
//!
//! ```text
//! kura-devtools bench save --work <dir> [--bulk-threads 3] [--threads 4,16] [--runs 3] [--quick]
//!                          [--only engine,overwrite,second,rebuild,revert0,saveas,png] [--json <file>] [--markdown <file|->] [--keep]
//! ```
//!
//! - 回 × スレッドの数ごとに、新しいライブラリを `--work` の下に作り（一括のプールをその数にした `BenchFactory`、プレビューは
//!   3）、入力（24MP の JPEG を中身の違うもの 12 枚と別名用 6 枚、2048² のドット絵の PNG 6 枚。ChaCha8 の `InputGen`）を
//!   アプリと同じ取り込みのジョブで入れる。
//! - 測るのは `start_save` の直前から、保存のジョブの終わりの知らせ（`JobEvent` の Done。フロントが保存の終わりを知る時）まで
//!   （積む・待ち行列・文書を読む・元を読む・描く・書く・fsync・ハッシュ・予告・置く・コミット・後のジョブを積む・ジョブの
//!   終わりの書き込み）。動き始めの知らせで、待ち行列の分と本体の分にも分ける。保存のあとのジョブ（サムネイル・解析・GC）は
//!   次の保存の前に動き終えるまで待つ（測らない）。
//! - 判定（NFR-13）：24MP の JPEG を線形光で 50%（Lanczos3）にして JPEG q90 で書く上書き。どれもそのアイテムの最初の保存
//!   （同じ中身を 2 回保存すると「版だけ足す」になるので、中身の違う写真を 1 枚ずつ使う）。慣らし 2 回のあと 10 回の中央値で
//!   判定し（M10 の bench edit と同じ）、最大も出す。同じ写真・同じスレッドの数で、M10 の道（`bench_edit::batch::save_once`：
//!   エンジンの中の読み・描き・書き）も測って並べる。
//! - 参考：2 回目の上書き（bilinear 50%）、途中の版に戻す（作り直し：GC の時計を 8 日進めて版 1 のファイルを片付けさせてから）、
//!   元に戻す（使い直し）、別名で保存、ドット絵の PNG の上書き（筆 20 本）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_core::edit::versions::{self, FileState};
use kura_core::edit::{Base, SaveAction, SaveRequest, start_save};
use kura_core::jobs::JobState;
use kura_core::library::LibrarySession;
use kura_engine::doc::{FormatChoice, Graph};
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::cases::{paint_doc, prior_strokes, save_doc, save_jpeg};
use kura_engine::testing::bench::{PHOTO_H, PHOTO_W, Stats};
use kura_engine::value::PixelFormat;
use serde_json::{Value as J, json};

use super::report::emit;
use super::{Recorder, Report, Row};
use crate::bench_edit::{load1, loadavg};

const USAGE: &str = "使い方: kura-devtools bench save --work <dir> [--bulk-threads 3] [--threads 4,16] [--runs 3] [--quick] \
                     [--only engine,overwrite,second,rebuild,revert0,saveas,png] [--json <file>] [--markdown <file|->] [--keep]";

const GROUPS: [&str; 7] = [
    "engine",
    "overwrite",
    "second",
    "rebuild",
    "revert0",
    "saveas",
    "png",
];

/// NFR-13 の目標（ミリ秒）。
const NFR13_MS: f64 = 500.0;

/// 保存のジョブ 1 つを待つ上限。
const JOB_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
struct Opts {
    work: Option<PathBuf>,
    bulk_threads: usize,
    threads: Vec<usize>,
    runs: usize,
    quick: bool,
    only: Vec<String>,
    json: Option<PathBuf>,
    markdown: Option<PathBuf>,
    keep: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        work: None,
        bulk_threads: 3,
        threads: Vec::new(),
        runs: 1,
        quick: false,
        only: Vec::new(),
        json: None,
        markdown: None,
        keep: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let num = |v: Option<&String>| {
            v.and_then(|s| s.parse::<usize>().ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{arg} の値が読めません。{USAGE}"))
        };
        match arg.as_str() {
            "--work" => o.work = it.next().map(PathBuf::from),
            "--bulk-threads" => o.bulk_threads = num(it.next())?,
            "--threads" => o.threads = super::thread_list(it.next())?,
            "--runs" => o.runs = num(it.next())?,
            "--quick" => o.quick = true,
            "--only" => {
                let list = it
                    .next()
                    .ok_or_else(|| format!("--only の値がありません。{USAGE}"))?;
                for g in list.split(',').map(str::trim) {
                    if !GROUPS.contains(&g) {
                        return Err(format!("--only は {} です: {g}", GROUPS.join("・")));
                    }
                    o.only.push(g.to_string());
                }
            }
            "--json" => o.json = it.next().map(PathBuf::from),
            "--markdown" => o.markdown = it.next().map(PathBuf::from),
            "--keep" => o.keep = true,
            other => return Err(format!("知らない引数です: {other}。{USAGE}")),
        }
    }
    if o.work.is_none() {
        return Err(format!("--work を指定してください。{USAGE}"));
    }
    Ok(o)
}

/// 大きさと回数。
#[derive(Debug, Clone)]
struct Sizes {
    photo: (u32, u32),
    sprite: u32,
    /// 判定の上書きの慣らしと回数。
    warmup: usize,
    n: usize,
    /// 参考のケースの慣らしと回数。
    ref_warmup: usize,
    ref_n: usize,
}

impl Sizes {
    fn new(quick: bool) -> Self {
        if quick {
            Self {
                photo: (120, 80),
                sprite: 64,
                warmup: 0,
                n: 4,
                ref_warmup: 0,
                ref_n: 2,
            }
        } else {
            Self {
                photo: (PHOTO_W, PHOTO_H),
                sprite: 2048,
                warmup: 2,
                n: 10,
                ref_warmup: 1,
                ref_n: 5,
            }
        }
    }

    fn photos(&self) -> usize {
        self.warmup + self.n
    }

    fn refs(&self) -> usize {
        self.ref_warmup + self.ref_n
    }
}

fn photo_name(i: usize) -> String {
    format!("photo-{i:02}.jpg")
}

fn as_name(i: usize) -> String {
    format!("asphoto-{i:02}.jpg")
}

fn sprite_name(i: usize) -> String {
    format!("sprite-{i:02}.png")
}

/// 入力を `src` に書く（中身の違う写真・ドット絵。種は通し番号から）。
fn write_inputs(src: &Path, z: &Sizes) -> Result<(), String> {
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    let codecs = crate::app_codecs(&src.join(".codec"));
    let photo = |seed| {
        InputGen::Photo {
            w: z.photo.0,
            h: z.photo.1,
            seed,
        }
        .generate()
        .map_err(|e| e.to_string())
    };
    for i in 0..z.photos() {
        super::write_value(
            &codecs,
            &photo(100 + i as u64)?,
            "jpeg",
            &src.join(photo_name(i)),
        )?;
    }
    for i in 0..z.refs() {
        super::write_value(
            &codecs,
            &photo(200 + i as u64)?,
            "jpeg",
            &src.join(as_name(i)),
        )?;
        let sprite = InputGen::PixelSprite {
            w: z.sprite,
            h: z.sprite,
            fmt: PixelFormat::Rgba8,
            seed: 300 + i as u64,
        }
        .generate()
        .map_err(|e| e.to_string())?;
        super::write_value(&codecs, &sprite, "png", &src.join(sprite_name(i)))?;
    }
    let _ = std::fs::remove_dir_all(src.join(".codec"));
    Ok(())
}

/// 線形光で 50% に縮めて JPEG q90 で書く文書（NFR-13。`filter` は lanczos3・bilinear）。
fn jpeg_doc(filter: &str) -> Result<Graph, String> {
    let mut g = save_doc(filter).map_err(|e| e.to_string())?;
    let out = g.outputs.first_mut().ok_or("文書に出力がありません")?;
    out.format = FormatChoice::To {
        format: "jpeg".into(),
        options: save_jpeg(),
    };
    Ok(g)
}

/// ドット絵に筆 20 本を描く文書（PNG のまま）。
fn png_doc(side: u32) -> Result<Graph, String> {
    Ok(
        paint_doc(prior_strokes(20, side).map_err(|e| e.to_string())?, None)
            .map_err(|e| e.to_string())?
            .0,
    )
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// 保存のジョブ 1 つの時間。
struct JobTimes {
    /// `start_save` の呼び出し（文書の部品を置いてジョブを積むトランザクション）。
    call: u64,
    /// `start_save` の前から動き始めの知らせまで。
    queue: u64,
    /// 動き始めの知らせから終わりの知らせまで。
    body: u64,
    /// `start_save` の前から終わりの知らせまで。
    total: u64,
    result: J,
}

/// 1 つのケースの値。
#[derive(Default)]
struct Samples {
    call: Vec<u64>,
    queue: Vec<u64>,
    body: Vec<u64>,
    total: Vec<u64>,
}

impl Samples {
    fn push(&mut self, t: &JobTimes) {
        self.call.push(t.call);
        self.queue.push(t.queue);
        self.body.push(t.body);
        self.total.push(t.total);
    }
}

struct Ctx<'a> {
    run: usize,
    threads: usize,
    judged: bool,
    z: &'a Sizes,
    src: &'a Path,
    tmp: &'a Path,
    session: &'a LibrarySession,
    rec: &'a Recorder,
    items: &'a BTreeMap<String, i64>,
    rows: Vec<Row>,
}

impl Ctx<'_> {
    fn item(&self, name: &str) -> Result<i64, String> {
        self.items
            .get(name)
            .copied()
            .ok_or_else(|| format!("{name} を取り込めていません"))
    }

    fn push(&mut self, r: Row) {
        emit(&r.line());
        self.rows.push(r);
    }

    /// 保存を積み、終わりの知らせまで待つ。そのあと、後のジョブが動き終えるまで待つ（測らない）。
    fn job(&self, item: i64, action: SaveAction) -> Result<JobTimes, String> {
        let lib = self.session.library();
        let head = lib
            .db()
            .read(|c| versions::head(c, item))
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("アイテム {item} がありません"))?;
        let base = Base {
            hash: head.hash,
            version: head.version,
        };
        let t0 = Instant::now();
        let id = start_save(
            lib.db(),
            self.session.jobs(),
            &SaveRequest {
                item_id: item,
                base,
                action,
            },
        )
        .map_err(|e| format!("保存を積めません: {e}"))?;
        let call = nanos(t0.elapsed());
        let seen = self.rec.wait(id, JOB_WAIT)?;
        let (end, state, message, result) = seen.end.ok_or("終わりの知らせがありません")?;
        if state != JobState::Done {
            return Err(format!(
                "保存のジョブ {id} が {} で終わりました: {message:?}",
                state.as_str()
            ));
        }
        let running = seen.running.unwrap_or(end);
        let times = JobTimes {
            call,
            queue: nanos(running.saturating_duration_since(t0)),
            body: nanos(end.saturating_duration_since(running)),
            total: nanos(end.saturating_duration_since(t0)),
            result: result
                .as_deref()
                .and_then(|r| serde_json::from_str(r).ok())
                .unwrap_or(J::Null),
        };
        super::settle(self.session)?;
        Ok(times)
    }

    /// ケースの行（全体・待ち行列・本体・積む呼び出し）。`judge` は判定する（判定のスレッドの数で、中央値で）。
    fn rows_for(
        &mut self,
        case: &str,
        item: &str,
        s: Samples,
        judge: bool,
        load: Option<f64>,
        note: &str,
    ) -> Result<(), String> {
        let st = |v: Vec<u64>| Stats::from_samples(v).ok_or("測った値がありません".to_string());
        let mut total = Row::timed(
            self.run,
            13,
            case,
            item,
            "job",
            "保存のジョブ全体（start_save → Done の知らせ）",
            self.threads,
            st(s.total)?,
            Some(NFR13_MS),
            false,
            load,
        )
        .with_note(note);
        if judge && self.judged {
            total = total.judge_median();
        }
        self.push(total);
        for (metric, label, v) in [
            ("queue", "うち積んでから動き始めの知らせまで", s.queue),
            ("body", "うち動き始めから Done の知らせまで", s.body),
            (
                "call",
                "start_save の呼び出し（文書の部品を置いて積む）",
                s.call,
            ),
        ] {
            let r = Row::timed(
                self.run,
                13,
                case,
                item,
                metric,
                label,
                self.threads,
                st(v)?,
                None,
                false,
                load,
            );
            self.push(r);
        }
        Ok(())
    }

    /// M10 の道（エンジンの中の読み・描き・書き。`bench edit` の NFR-13 と同じ）を、同じ写真・同じ実行の場で。
    fn engine(&mut self) -> Result<(), String> {
        let load = load1();
        let engine = self.session.engine().ok_or("エンジンの部品がありません")?;
        let rt = engine.runtime().map_err(|e| e.to_string())?.clone();
        let codecs = engine.codecs();
        let graph = save_doc("lanczos3").map_err(|e| e.to_string())?;
        let out = self.tmp.join("engine.jpg");
        let mut samples = Vec::new();
        for i in 0..self.z.photos() {
            let path = self.src.join(photo_name(i));
            let t0 = Instant::now();
            crate::bench_edit::batch::save_once(&rt, &codecs, &graph, &path, "jpeg", &out)
                .map_err(|e| format!("エンジンの中の保存: {e}"))?;
            if i >= self.z.warmup {
                samples.push(nanos(t0.elapsed()));
            }
        }
        let _ = std::fs::remove_file(&out);
        let r = Row::timed(
            self.run,
            13,
            "engine-lanczos3",
            "24MP の JPEG → Lanczos3 50% → JPEG q90（M10 の道：エンジンの中の読み・描き・書き）",
            "engine",
            "エンジンの中（bench edit の NFR-13 と同じ道）",
            self.threads,
            Stats::from_samples(samples).ok_or("測った値がありません")?,
            Some(NFR13_MS),
            false,
            load,
        );
        self.push(r);
        Ok(())
    }

    /// 判定：中身の違う写真をそれぞれ初めて上書き保存する。
    fn overwrite(&mut self) -> Result<(), String> {
        let load = load1();
        let doc = jpeg_doc("lanczos3")?;
        let mut s = Samples::default();
        for i in 0..self.z.photos() {
            let item = self.item(&photo_name(i))?;
            let t = self.job(item, SaveAction::Overwrite { graph: doc.clone() })?;
            if t.result["outcome"] != "saved" {
                return Err(format!("上書きの結果が saved ではありません: {}", t.result));
            }
            if i >= self.z.warmup {
                s.push(&t);
            }
        }
        self.rows_for(
            "overwrite-lanczos3",
            "24MP の JPEG → 線形光で Lanczos3 50% → JPEG q90 の上書き（そのアイテムの最初の保存）",
            s,
            true,
            load,
            "",
        )
    }

    /// 参考：2 回目の上書き（bilinear 50%）。作り直しの準備（版 1 が置き換えられ、GC が片付ける）も兼ねる。
    fn second(&mut self) -> Result<(), String> {
        let load = load1();
        let doc = jpeg_doc("bilinear")?;
        let mut s = Samples::default();
        for i in 0..self.z.refs() {
            let item = self.item(&photo_name(i))?;
            let t = self.job(item, SaveAction::Overwrite { graph: doc.clone() })?;
            if i >= self.z.ref_warmup {
                s.push(&t);
            }
        }
        self.rows_for(
            "overwrite-second",
            "2 回目の上書き（同じアイテムを bilinear 50% → JPEG q90 で）",
            s,
            false,
            load,
            "",
        )
    }

    /// 参考：途中の版（版 1）に戻す。版 1 のファイルは GC が片付けたので作り直す（読む・描く・照らす・書く）。
    fn rebuild(&mut self) -> Result<(), String> {
        let load = load1();
        let mut s = Samples::default();
        for i in 0..self.z.refs() {
            let item = self.item(&photo_name(i))?;
            let v1 = self
                .session
                .library()
                .db()
                .read(|c| versions::get(c, item, 1))
                .map_err(|e| e.to_string())?
                .ok_or("版 1 がありません")?;
            if v1.file_state != FileState::Released {
                return Err(format!(
                    "版 1 のファイルが片付いていません（{:?}）。2 回目の上書き（--only second）を先に回します",
                    v1.file_state
                ));
            }
            let t = self.job(item, SaveAction::Revert { to: 1 })?;
            if t.result["revert"]["rebuilt"] != true {
                return Err(format!("作り直していません: {}", t.result));
            }
            if i >= self.z.ref_warmup {
                s.push(&t);
            }
        }
        self.rows_for(
            "revert-rebuild",
            "途中の版に戻す（作り直し：24MP を読んで Lanczos3 50% を描き、値を照らして JPEG で書く）",
            s, false, load, "版 1 のファイルは GC（時計を 8 日進めた）が片付けたあと",
        )
    }

    /// 参考：元に戻す（使い直し。版 0 のファイルがあるので描かない）。
    fn revert0(&mut self) -> Result<(), String> {
        let load = load1();
        let mut s = Samples::default();
        let from = self.z.refs();
        for (k, i) in (from..from + self.z.refs()).enumerate() {
            let item = self.item(&photo_name(i))?;
            let t = self.job(item, SaveAction::Revert { to: 0 })?;
            if t.result["revert"]["reused"] != true {
                return Err(format!("使い直していません: {}", t.result));
            }
            if k >= self.z.ref_warmup {
                s.push(&t);
            }
        }
        self.rows_for(
            "revert-original",
            "元に戻す（使い直し：版 0 のファイルを使い、描かない）",
            s,
            false,
            load,
            "",
        )
    }

    /// 参考：別名で保存（新しいアイテムを作る）。
    fn saveas(&mut self) -> Result<(), String> {
        let load = load1();
        let doc = jpeg_doc("lanczos3")?;
        let mut s = Samples::default();
        for i in 0..self.z.refs() {
            let item = self.item(&as_name(i))?;
            let t = self.job(
                item,
                SaveAction::SaveAs {
                    graph: doc.clone(),
                    title: format!("別名 {i}"),
                    stem: format!("photo-as-{i}"),
                },
            )?;
            if t.result["outcome"] != "saved" {
                return Err(format!("別名の結果が saved ではありません: {}", t.result));
            }
            if i >= self.z.ref_warmup {
                s.push(&t);
            }
        }
        self.rows_for(
            "save-as",
            "別名で保存（24MP → Lanczos3 50% → JPEG q90 の新しいアイテム）",
            s,
            false,
            load,
            "",
        )
    }

    /// 参考：ドット絵の PNG の上書き（筆 20 本）。
    fn png(&mut self) -> Result<(), String> {
        let load = load1();
        let side = self.z.sprite;
        let doc = png_doc(side)?;
        let mut s = Samples::default();
        for i in 0..self.z.refs() {
            let item = self.item(&sprite_name(i))?;
            let t = self.job(item, SaveAction::Overwrite { graph: doc.clone() })?;
            if i >= self.z.ref_warmup {
                s.push(&t);
            }
        }
        let item = format!("ドット絵の PNG の上書き（{side}² の Rgba8 に筆 20 本 → PNG）");
        self.rows_for("overwrite-png", &item, s, false, load, "")
    }
}

/// `bench save` の入口。
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    if cfg!(debug_assertions) && !opts.quick {
        return Err(
            "bench save は release で回してください（cargo run --release -p kura-devtools -- bench save）。\
             debug では --quick だけ回せます"
                .into(),
        );
    }
    let work = opts.work.clone().ok_or(USAGE)?;
    let root = super::fresh_dir(&work, "save")?;
    let result = run_all(&opts, &root);
    if !opts.keep {
        let _ = std::fs::remove_dir_all(&root);
    }
    result
}

fn wants(opts: &Opts, group: &str) -> bool {
    opts.only.is_empty() || opts.only.iter().any(|g| g == group)
}

fn run_all(opts: &Opts, root: &Path) -> Result<(), String> {
    let z = Sizes::new(opts.quick);
    let src = root.join("src");
    emit(&format!(
        "bench save：入力を作っています（{}）",
        src.display()
    ));
    write_inputs(&src, &z)?;
    let env = super::env_json(json!({
        "bench": "save",
        "bulk_threads": opts.bulk_threads,
        "preview_threads": 3,
        "threads": opts.threads,
        "quick": opts.quick,
        "runs": opts.runs,
        "gc_clock": "8 日進めた時計・バックアップを数えない（途中の版に戻すの作り直しのため）",
    }));
    emit(&format!("bench save：{env}"));
    let set = super::thread_set(opts.bulk_threads, &opts.threads);
    let mut rows = Vec::new();
    let mut runs = Vec::new();
    for run in 0..opts.runs {
        let before = loadavg();
        let started = Instant::now();
        emit(&format!(
            "== {} 回目（ロードアベレージ {before:?}）==",
            run + 1
        ));
        for &threads in &set {
            let lib = root.join(format!("r{run}-t{threads}.kuralib"));
            let rec = Arc::new(Recorder::default());
            let services = super::services(3, threads).with_gc_clock(super::shifted_gc_clock());
            let session = LibrarySession::create(&lib, rec.sink(), services)
                .map_err(|e| format!("ライブラリを作れません: {e}"))?;
            session.wait_ready();
            let items = super::import_all(&session, &src)?;
            let mut ctx = Ctx {
                run,
                threads,
                judged: threads == opts.bulk_threads,
                z: &z,
                src: &src,
                tmp: root,
                session: &session,
                rec: &rec,
                items: &items,
                rows: Vec::new(),
            };
            let result = (|| -> Result<(), String> {
                if wants(opts, "engine") {
                    ctx.engine()?;
                }
                if wants(opts, "overwrite") {
                    ctx.overwrite()?;
                }
                if wants(opts, "second") {
                    ctx.second()?;
                }
                if wants(opts, "rebuild") {
                    ctx.rebuild()?;
                }
                if wants(opts, "revert0") {
                    if !wants(opts, "overwrite") {
                        return Err(
                            "元に戻す（revert0）は上書き（overwrite）のあとに回します".into()
                        );
                    }
                    ctx.revert0()?;
                }
                if wants(opts, "saveas") {
                    ctx.saveas()?;
                }
                if wants(opts, "png") {
                    ctx.png()?;
                }
                Ok(())
            })();
            rows.append(&mut ctx.rows);
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
        title: "bench save（保存のジョブの全体：start_save → Done の知らせ）",
        env,
        runs,
        rows,
        judged_threads: opts.bulk_threads,
        thread_metrics: &["job", "body", "engine"],
    };
    super::write_report(&report, opts.json.as_deref(), opts.markdown.as_deref())
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
            "--work w --bulk-threads 4 --threads 3,16 --runs 2 --quick --only engine,png --keep",
        ))
        .unwrap();
        assert_eq!((o.bulk_threads, o.runs), (4, 2));
        assert_eq!(o.threads, vec![3, 16]);
        assert_eq!(o.only, vec!["engine".to_string(), "png".to_string()]);
        assert!(o.quick && o.keep);
        assert!(parse(&args("--runs 2")).is_err());
        assert!(parse(&args("--work w --only gc")).is_err());
    }

    #[test]
    fn the_jpeg_document_writes_jpeg_q90() {
        let g = jpeg_doc("lanczos3").unwrap();
        assert_eq!(
            g.outputs[0].format,
            FormatChoice::To {
                format: "jpeg".into(),
                options: json!({ "quality": 90 })
            }
        );
        assert_eq!(g.nodes.len(), 1);
    }

    /// 極小の大きさで通しで回す（debug。取り込み・保存のジョブ・作り直し・使い直し・別名・PNG）。
    #[test]
    fn quick_run_goes_through_the_save_job() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("r.json");
        let a = vec![
            "--work".to_string(),
            dir.path().join("w").display().to_string(),
            "--quick".into(),
            "--json".into(),
            out.display().to_string(),
        ];
        run(&a).unwrap();
        let j: J = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let rows = j["rows"].as_array().unwrap();
        for case in [
            "engine-lanczos3",
            "overwrite-lanczos3",
            "overwrite-second",
            "revert-rebuild",
            "revert-original",
            "save-as",
            "overwrite-png",
        ] {
            assert!(rows.iter().any(|r| r["case"] == case), "{case}");
        }
        assert_eq!(std::fs::read_dir(dir.path().join("w")).unwrap().count(), 0);
    }
}
