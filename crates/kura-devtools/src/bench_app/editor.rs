//! `kura-devtools bench editor`：エディタのアプリの道（`edit::editor`）の 1 手と、kura:// の `/edit` のタイルの書き出し
//! （NFR-11・12 のアプリの道の分）、NFR-17 の取り置き・使用量の戻りと 24MP を開いたときの最大の使用量。
//!
//! ```text
//! kura-devtools bench editor --work <dir> [--preview-threads 3] [--threads 4,16] [--runs 3] [--quick]
//!                            [--only pixel,photo,stroke,nearest,fill,mem] [--json <file>] [--markdown <file|->] [--keep]
//!                            [--tile-format rgba|png|both] [--viewport 1600x1000] [--no-tiles]
//! ```
//!
//! - 切り分けの口：`--tile-format` は 1 手のあとに書き出すタイルの形（既定は両方。フロントの今の既定は RGBA8）、`--viewport`
//!   は等倍で見るケースの表示する範囲（左上からの幅×高さ。なければ出力の全体）、`--no-tiles` はタイルを書き出さない。
//!
//! - `--work` の下に新しいライブラリを作り、入力（ドット絵 2048² の Rgba8・Indexed8 の PNG、24MP の JPEG、単色の 4096²・
//!   1024²・2048² の PNG。どれも ChaCha8 と整数で作る。M10 の bench edit と同じ生成器）をアプリと同じ取り込みのジョブで入れる
//!   （サムネイル・解析のジョブが動き終えるまで待つ）。
//! - スレッドの数ごとに、そのプレビューのプールの大きさのエンジンの部品でライブラリを開き直し（`BenchFactory`）、ケースごとに
//!   エディタを開いて（`editor::open`）、文書を 1 手で入れる（`Edit::Replace`。M10 と同じ文書・同じノードの ID）。測る手は
//!   M10 の bench edit と同じ（スライダーは `Step::Preview`、筆・塗りつぶしは描画のノードへの `AppendItems` の確定）。
//! - 1 手ごとに測るもの：`editor::step` の時間（手を受けてから、表示の値と変わったタイルの一覧ができるまで。下書きの依頼・
//!   文書の差分・表示の情報を含む）、その中の表示の時間（`ViewInfo.ms`）、変わったタイルを `tile::serve` で書き出す時間
//!   （RGBA8・PNG。フロントと同じく同時に 6 本：`canvasController.ts` の `MAX_LOADS`）。ケースごとに、最初の表示のタイルを
//!   1 枚ずつ順に書き出した時間（1 タイルあたり）と、開く時間も測る。表示する範囲は出力の全体（M10 と同じ）。
//! - 判定：1 手の p95 を NFR-11（25 ms）・NFR-12（8 ms・30 ms）の「エンジンの中」の目標と比べる（アプリの道で、手を受けて
//!   から変わったタイルの一覧ができるまで）。1 手 ＋ タイルは NFR-11 の 50 ms のうち Rust の中の分として記録する（IPC・WebKit
//!   を含まないので判定しない）。
//! - NFR-17：ケースごとに、開く前と閉じたあとの予算の使用量（取り置き・対話・持ち続ける分・一括）が同じかを数える。各回の
//!   最後に、自分を子として `/usr/bin/time -l` で起こし（`--child base|photo`）、24MP を開いて段 2 と等倍で手を当てたときの
//!   予算で数えた使用量の最大（200 µs ごとに見る）と peak memory footprint（基準の子との差）を記録する。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use kura_core::edit::EngineServices;
use kura_core::edit::editor::{
    self, ChangedTiles, EditorEnv, OpenRequest, Step, TileFormat, TileRequest, ViewInfo,
    ViewOutcome, Viewport, tile,
};
use kura_core::library::LibrarySession;
use kura_engine::budget::{Usage, default_reserved};
use kura_engine::doc::{Graph, NodeId};
use kura_engine::session::{CommitMode, Edit};
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::cases::{
    PHOTO_LEVEL, Sweep, fill_item, flat_canvas, nearest4, paint_doc, photo_sweeps, pixel_sweeps,
    prior_strokes, stroke_item,
};
use kura_engine::testing::bench::{
    OUT_OF_SCOPE_STROKE, PHOTO_H, PHOTO_W, Stats, WARMUP, stroke_in_scope,
};
use kura_engine::value::PixelFormat;
use serde_json::{Value as J, json};

use super::report::{emit, ms};
use super::{Report, Row, Verdict, mib};
use crate::bench_edit::{load1, loadavg};

const USAGE: &str = "使い方: kura-devtools bench editor --work <dir> [--preview-threads 3] [--threads 4,16] [--runs 3] \
                     [--quick] [--only pixel,photo,stroke,nearest,fill,mem] [--json <file>] [--markdown <file|->] [--keep] \
                     [--tile-format rgba|png|both] [--viewport <幅>x<高さ>] [--no-tiles]";

/// 同時に取りに行くタイルの数（フロントの `canvasController.ts` の `MAX_LOADS` と同じ）。
const TILE_LOADS: usize = 6;

/// 1 タイルあたりの時間を順に測るタイルの数の上限（ケースごと）。
const SEQ_TILES: usize = 256;

/// 1 手 ＋ タイルの比べる先（NFR-11 の 50 ms。IPC・WebKit を含まないので判定しない）。
const NFR11_TOTAL_MS: f64 = 50.0;

const GROUPS: [&str; 6] = ["pixel", "photo", "stroke", "nearest", "fill", "mem"];

#[derive(Debug, Clone)]
struct Opts {
    work: Option<PathBuf>,
    preview_threads: usize,
    threads: Vec<usize>,
    runs: usize,
    quick: bool,
    only: BTreeSet<String>,
    json: Option<PathBuf>,
    markdown: Option<PathBuf>,
    keep: bool,
    /// 1 手のあとにタイルを書き出さない（タイルの書き出しが次の手に効くかを比べるため）。
    no_tiles: bool,
    /// 等倍で見るケース（ドット絵・筆・塗りつぶし）の表示する範囲（左上からの幅×高さ。なければ出力の全体）。
    viewport: Option<(u32, u32)>,
    /// 1 手のあとに書き出すタイルの形（RGBA8, PNG）。`--tile-format rgba|png|both`（既定は両方）。
    formats: (bool, bool),
    child: Option<String>,
    library: Option<PathBuf>,
    item: Option<i64>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        work: None,
        preview_threads: 3,
        threads: Vec::new(),
        runs: 1,
        quick: false,
        only: BTreeSet::new(),
        json: None,
        markdown: None,
        keep: false,
        no_tiles: false,
        viewport: None,
        formats: (true, true),
        child: None,
        library: None,
        item: None,
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
            "--preview-threads" => o.preview_threads = num(it.next())?,
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
                    o.only.insert(g.to_string());
                }
            }
            "--json" => o.json = it.next().map(PathBuf::from),
            "--markdown" => o.markdown = it.next().map(PathBuf::from),
            "--keep" => o.keep = true,
            "--no-tiles" => o.no_tiles = true,
            "--tile-format" => {
                o.formats = match it.next().map(String::as_str) {
                    Some("rgba") => (true, false),
                    Some("png") => (false, true),
                    Some("both") => (true, true),
                    _ => return Err(format!("--tile-format は rgba・png・both です。{USAGE}")),
                };
            }
            "--viewport" => {
                let v = it
                    .next()
                    .ok_or_else(|| format!("--viewport の値がありません。{USAGE}"))?;
                let (w, h) = v
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
                    .filter(|(w, h)| *w > 0 && *h > 0)
                    .ok_or_else(|| format!("--viewport は 幅x高さ です: {v}"))?;
                o.viewport = Some((w, h));
            }
            "--child" => o.child = it.next().cloned(),
            "--library" => o.library = it.next().map(PathBuf::from),
            "--item" => o.item = it.next().and_then(|s| s.parse().ok()),
            other => return Err(format!("知らない引数です: {other}。{USAGE}")),
        }
    }
    if o.child.is_none() && o.work.is_none() {
        return Err(format!("--work を指定してください。{USAGE}"));
    }
    Ok(o)
}

/// 大きさと回数（`--quick` は極小で 2 回ずつ）。
#[derive(Debug, Clone)]
struct Sizes {
    pixel: u32,
    photo: (u32, u32),
    canvas: u32,
    priors: Vec<usize>,
    stroke_sizes: Vec<u8>,
    nearest_sides: Vec<u32>,
    nearest_prior: usize,
    nearest_strokes: Vec<u8>,
    warmup: usize,
    n: usize,
    mem_steps: usize,
}

impl Sizes {
    fn new(quick: bool) -> Self {
        if quick {
            Self {
                pixel: 64,
                photo: (120, 80),
                canvas: 128,
                priors: vec![0, 20],
                stroke_sizes: vec![1, 8, 64],
                nearest_sides: vec![32],
                nearest_prior: 20,
                nearest_strokes: vec![8],
                warmup: 1,
                n: 2,
                mem_steps: 2,
            }
        } else {
            Self {
                pixel: 2048,
                photo: (PHOTO_W, PHOTO_H),
                canvas: 4096,
                priors: vec![0, 999, 50_000],
                stroke_sizes: vec![1, 8, 32, 64],
                nearest_sides: vec![1024, 2048],
                nearest_prior: 999,
                nearest_strokes: vec![8, 32],
                warmup: WARMUP,
                n: 30,
                mem_steps: 10,
            }
        }
    }

    fn canvases(&self) -> Vec<u32> {
        let mut c = self.nearest_sides.clone();
        c.push(self.canvas);
        c.sort();
        c.dedup();
        c
    }
}

fn canvas_name(side: u32) -> String {
    format!("canvas-{side}.png")
}

const SPRITE_RGBA: &str = "sprite-rgba.png";
const SPRITE_INDEXED: &str = "sprite-indexed.png";
const PHOTO: &str = "photo.jpg";

/// 入力のファイルを `src` に書く（M10 の bench edit と同じ生成器・種）。写真の値を返す（写真のケースの文書が大きさを使う）。
fn write_inputs(src: &Path, z: &Sizes) -> Result<kura_engine::Value, String> {
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    let codecs = crate::app_codecs(&src.join(".codec"));
    let sprite = |fmt, seed| InputGen::PixelSprite {
        w: z.pixel.max(16),
        h: z.pixel.max(16),
        fmt,
        seed,
    };
    let make = |g: InputGen| g.generate().map_err(|e| e.to_string());
    super::write_value(
        &codecs,
        &make(sprite(PixelFormat::Rgba8, 11))?,
        "png",
        &src.join(SPRITE_RGBA),
    )?;
    super::write_value(
        &codecs,
        &make(sprite(PixelFormat::Indexed8, 12))?,
        "png",
        &src.join(SPRITE_INDEXED),
    )?;
    let photo = make(InputGen::Photo {
        w: z.photo.0,
        h: z.photo.1,
        seed: 7,
    })?;
    super::write_value(&codecs, &photo, "jpeg", &src.join(PHOTO))?;
    for side in z.canvases() {
        let v = flat_canvas(side).map_err(|e| e.to_string())?;
        super::write_value(&codecs, &v, "png", &src.join(canvas_name(side)))?;
    }
    let _ = std::fs::remove_dir_all(src.join(".codec"));
    Ok(photo)
}

fn quiet() -> kura_core::jobs::EventSink {
    Arc::new(|_| {})
}

/// 表示した結果（表示できなければエラー）。
fn shown(view: ViewOutcome) -> Result<ViewInfo, String> {
    match view {
        ViewOutcome::Shown(info) => Ok(info),
        other => Err(format!("表示できませんでした: {other:?}")),
    }
}

/// 取り直すタイル（フレーム, tx, ty）：変わったタイルの一覧、すべてなら見ている範囲の全体（`vp.rect` は等倍の座標。
/// なければフレームの全体）。どちらも見ている範囲のタイルだけ（フロントが取りに行くのは見えているタイル）。
fn tile_list(info: &ViewInfo, vp: &Viewport) -> Vec<(u32, u32, u32)> {
    let seen = frame_tiles(info, vp);
    match &info.changed {
        ChangedTiles::All => seen,
        ChangedTiles::Tiles(list) => list
            .iter()
            .flat_map(|(f, ts)| ts.iter().map(move |[x, y]| (*f, *x, *y)))
            .filter(|t| t.0 != vp.frame || seen.contains(t))
            .collect(),
    }
}

/// 見ているフレームの、見ている範囲のタイル（フレーム, tx, ty）。
fn frame_tiles(info: &ViewInfo, vp: &Viewport) -> Vec<(u32, u32, u32)> {
    let frame = vp.frame;
    let (w, h) = info
        .frames
        .iter()
        .find(|f| f.index == frame)
        .map(|f| (u64::from(f.rect.w), u64::from(f.rect.h)))
        .unwrap_or(info.canvas);
    let t = u64::from(info.tile.max(1));
    let k = u32::from(info.level.min(62));
    // 見ている範囲を段の座標に（始まりは床、終わりは天井）し、フレームの中に収める
    let (x0, y0, x1, y1) = match vp.rect {
        Some(r) => {
            let lo = |v: i64| u64::try_from(v.max(0)).unwrap_or(0) >> k;
            let hi = |v: i64| u64::try_from(v.max(0)).unwrap_or(0).div_ceil(1 << k);
            (
                lo(r.x),
                lo(r.y),
                hi(r.x.saturating_add(i64::from(r.w))).min(w),
                hi(r.y.saturating_add(i64::from(r.h))).min(h),
            )
        }
        None => (0, 0, w, h),
    };
    let (tx0, ty0, tx1, ty1) = (x0 / t, y0 / t, x1.div_ceil(t), y1.div_ceil(t));
    (ty0..ty1)
        .flat_map(|y| (tx0..tx1).map(move |x| (frame, x as u32, y as u32)))
        .collect()
}

/// タイルを `workers` 本の同時の要求で書き出す（kura:// の要求は spawn_blocking の別々のスレッドで答えるので、std のスレッドで
/// 写す）。全体の時間と 1 枚ごとの時間（ナノ秒）、バイト数。
fn serve_tiles(
    engine: &EngineServices,
    sid: &str,
    info: &ViewInfo,
    tiles: &[(u32, u32, u32)],
    format: TileFormat,
    workers: usize,
) -> Result<(u64, Vec<u64>, u64), String> {
    let next = AtomicUsize::new(0);
    let started = Instant::now();
    let results: Vec<Result<(Vec<u64>, u64), String>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers.min(tiles.len()).max(1))
            .map(|_| {
                s.spawn(|| -> Result<(Vec<u64>, u64), String> {
                    let mut each = Vec::new();
                    let mut bytes = 0u64;
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(frame, tx, ty)) = tiles.get(i) else {
                            return Ok((each, bytes));
                        };
                        let t0 = Instant::now();
                        let out = tile::serve(
                            engine,
                            &TileRequest {
                                session: sid,
                                rev: info.rev,
                                level: info.level,
                                frame,
                                tx,
                                ty,
                                format,
                            },
                        )
                        .map_err(|e| format!("タイル {frame}/{tx}/{ty}: {e}"))?;
                        each.push(nanos(t0));
                        bytes += out.len() as u64;
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("タイルのスレッドが落ちました".into()))
            })
            .collect()
    });
    let wall = nanos(started);
    let mut each = Vec::new();
    let mut bytes = 0;
    for r in results {
        let (e, b) = r?;
        each.extend(e);
        bytes += b;
    }
    Ok((wall, each, bytes))
}

fn nanos(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn stats(v: Vec<u64>) -> Result<Stats, String> {
    Stats::from_samples(v).ok_or_else(|| "測った値がありません".to_string())
}

/// 開いたエディタ 1 つ。
struct Open<'s> {
    env: EditorEnv<'s>,
    engine: &'s Arc<EngineServices>,
    sid: String,
    vp: Viewport,
    open_ns: u64,
    /// 文書を入れたあとの表示。
    first: ViewInfo,
    /// その表示のタイルを 1 枚ずつ順に書き出した時間（RGBA8・PNG。表示を差し替えると古い rev のタイルは返らないので、
    /// 開いてすぐに測る）。
    seq: [(Vec<u64>, u64); 2],
    seq_tiles: usize,
    before: Usage,
    /// 開いている間の取り置きと、手のあとに持ち続けた確保の最大。
    reserved: u64,
    max_interactive: u64,
}

impl<'s> Open<'s> {
    /// アイテム `item` をエディタで開き、文書 `graph` を 1 手で入れる（`Edit::Replace`）。
    fn new(
        session: &'s LibrarySession,
        item: i64,
        vp: Viewport,
        graph: &Graph,
    ) -> Result<Self, String> {
        let env = EditorEnv::of(session).map_err(|e| e.to_string())?;
        let engine = session.engine().ok_or("エンジンの部品がありません")?;
        let rt = engine.runtime().map_err(|e| e.to_string())?;
        let before = rt.budget().usage().map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        let opened = editor::open(
            &env,
            &OpenRequest {
                item_id: item,
                viewport: vp.clone(),
                from_draft: false,
            },
        )
        .map_err(|e| format!("開けません: {e}"))?;
        let open_ns = nanos(t0);
        shown(opened.outcome.view)?;
        let sid = opened.session;
        let out = editor::step(
            &env,
            &sid,
            vec![Step::Commit {
                edit: Edit::Replace {
                    graph: graph.clone(),
                },
                mode: CommitMode::Step,
            }],
            &vp,
        )
        .map_err(|e| format!("文書を入れられません: {e}"))?;
        if let Some(r) = out.rejected {
            return Err(format!("文書を入れられません: {:?}", r.problem));
        }
        let first = shown(out.view)?;
        let usage = rt.budget().usage().map_err(|e| e.to_string())?;
        let mut tiles = frame_tiles(&first, &vp);
        tiles.truncate(SEQ_TILES);
        let (_, rgba, rgba_bytes) = serve_tiles(engine, &sid, &first, &tiles, TileFormat::Rgba, 1)?;
        let (_, png, png_bytes) = serve_tiles(engine, &sid, &first, &tiles, TileFormat::Png, 1)?;
        Ok(Self {
            env,
            engine,
            sid,
            vp,
            open_ns,
            first,
            seq: [(rgba, rgba_bytes), (png, png_bytes)],
            seq_tiles: tiles.len(),
            before,
            reserved: usage.reserved,
            max_interactive: usage.interactive,
        })
    }

    /// 手を 1 つ当てて表示する（時間と表示）。
    fn step(&mut self, step: Step) -> Result<(u64, ViewInfo), String> {
        let t0 = Instant::now();
        let out = editor::step(&self.env, &self.sid, vec![step], &self.vp)
            .map_err(|e| format!("手を当てられません: {e}"))?;
        let ns = nanos(t0);
        if let Some(r) = out.rejected {
            return Err(format!("手を断られました: {:?}", r.problem));
        }
        let info = shown(out.view)?;
        if let Ok(u) = self
            .engine
            .runtime()
            .map_err(|e| e.to_string())
            .and_then(|rt| rt.budget().usage().map_err(|e| e.to_string()))
        {
            self.max_interactive = self.max_interactive.max(u.interactive);
        }
        Ok((ns, info))
    }

    /// 閉じて、閉じたあとの使用量が開く前と同じか。
    fn close(self) -> Result<(bool, Usage, Usage), String> {
        editor::close(self.engine, &self.sid);
        let rt = self.engine.runtime().map_err(|e| e.to_string())?;
        let after = rt.budget().usage().map_err(|e| e.to_string())?;
        let b = &self.before;
        let same = after.reserved == 0
            && after.reserved == b.reserved
            && after.interactive == b.interactive
            && after.standing == b.standing
            && after.bulk == b.bulk;
        Ok((same, self.before, after))
    }
}

/// 1 手ごとの値。
#[derive(Default)]
struct Samples {
    step: Vec<u64>,
    render: Vec<u64>,
    rgba: Vec<u64>,
    png: Vec<u64>,
    total_rgba: Vec<u64>,
    total_png: Vec<u64>,
    tiles: Vec<usize>,
    levels: BTreeSet<u8>,
}

/// ケースの名前と判定。
struct Case<'a> {
    nfr: u8,
    case: &'a str,
    item: &'a str,
    target_ms: f64,
    /// 判定する（範囲の中で、判定のスレッドの数）。
    judge: bool,
    out_of_scope: Option<&'a str>,
    note: &'a str,
}

/// 回 × スレッドの数の状態。
struct Ctx<'a> {
    run: usize,
    threads: usize,
    judged: bool,
    z: &'a Sizes,
    session: &'a LibrarySession,
    items: &'a BTreeMap<String, i64>,
    rows: Vec<Row>,
    /// 閉じたあと予算が開く前に戻ったケースの数・すべてのケースの数。
    restored: (usize, usize),
    max_interactive: u64,
    reserved: u64,
    /// 1 手のあとにタイルを書き出さない（`--no-tiles`）。
    no_tiles: bool,
    /// 等倍で見るケースの表示する範囲（`--viewport`）。
    viewport: Option<(u32, u32)>,
    /// 1 手のあとに書き出すタイルの形（RGBA8, PNG。`--tile-format`）。
    formats: (bool, bool),
}

impl Ctx<'_> {
    /// 等倍で見るケースの表示する範囲（`--viewport` があれば左上からその大きさ、なければ出力の全体）。
    fn rect(&self) -> Option<kura_engine::geom::IRect> {
        self.viewport
            .map(|(w, h)| kura_engine::geom::IRect::new(0, 0, w, h))
    }

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

    /// 開いたエディタで手を測る：`prepare`（測らない。取り消しなど）→ 手（測る）→ 変わったタイルの書き出し（RGBA8・PNG）。
    fn measure(
        &mut self,
        open: &mut Open<'_>,
        mut prepare: impl FnMut(&mut Open<'_>, usize) -> Result<(), String>,
        mut next: impl FnMut(usize) -> Result<Step, String>,
    ) -> Result<Samples, String> {
        let mut s = Samples::default();
        let (warmup, n) = (self.z.warmup, self.z.n);
        for i in 0..warmup + n {
            prepare(open, i)?;
            let step = next(i)?;
            let (step_ns, info) = open.step(step)?;
            let tiles = tile_list(&info, &open.vp);
            let serve = |on: bool, format| -> Result<u64, String> {
                if self.no_tiles || !on {
                    return Ok(0);
                }
                Ok(serve_tiles(open.engine, &open.sid, &info, &tiles, format, TILE_LOADS)?.0)
            };
            let rgba = serve(self.formats.0, TileFormat::Rgba)?;
            let png = serve(self.formats.1, TileFormat::Png)?;
            if i >= warmup {
                s.step.push(step_ns);
                s.render.push((info.ms * 1e6) as u64);
                s.rgba.push(rgba);
                s.png.push(png);
                s.total_rgba.push(step_ns + rgba);
                s.total_png.push(step_ns + png);
                s.tiles.push(tiles.len());
                s.levels.insert(info.level);
            }
        }
        Ok(s)
    }

    /// エディタを開いたときの行（ケースの最初だけ）：開く時間と、1 タイルあたりの時間（文書を入れたあとの表示のタイルを、
    /// 1 枚ずつ順に。RGBA8・PNG）。
    fn per_tile(&mut self, open: &Open<'_>, c: &Case<'_>, load: Option<f64>) -> Result<(), String> {
        for ((each, bytes), (metric, label)) in open.seq.iter().zip([
            ("tile-rgba", "タイル 1 枚の書き出し RGBA8（順に）"),
            ("tile-png", "タイル 1 枚の書き出し PNG（順に）"),
        ]) {
            let n = each.len().max(1) as u64;
            let r = Row::timed(
                self.run,
                c.nfr,
                c.case,
                c.item,
                metric,
                label,
                self.threads,
                stats(each.clone())?,
                None,
                false,
                load,
            )
            .with_extra(json!({ "tiles": open.seq_tiles, "bytes_per_tile": bytes / n }));
            self.push(r);
        }
        let r = Row::timed(
            self.run,
            c.nfr,
            c.case,
            c.item,
            "open",
            "開く（元の読み ＋ 最初の表示）",
            self.threads,
            stats(vec![open.open_ns])?,
            None,
            false,
            load,
        );
        self.push(r);
        Ok(())
    }

    /// ケースの行を足す。
    fn rows_for(
        &mut self,
        c: &Case<'_>,
        s: Samples,
        load: Option<f64>,
        extra: J,
    ) -> Result<(), String> {
        let judge = c.judge && self.judged && c.out_of_scope.is_none();
        let tiles_mean = s.tiles.iter().sum::<usize>() as f64 / s.tiles.len().max(1) as f64;
        let tiles_max = s.tiles.iter().copied().max().unwrap_or(0);
        let levels: Vec<u8> = s.levels.iter().copied().collect();
        let mut step = Row::timed(
            self.run, c.nfr, c.case, c.item, "step", "1 手（editor::step）", self.threads,
            stats(s.step)?, Some(c.target_ms), judge, load,
        )
        .with_note(c.note)
        .with_extra(json!({ "levels": levels, "tiles_mean": tiles_mean, "tiles_max": tiles_max, "case": extra }));
        if let Some(why) = c.out_of_scope {
            step = step.with_note(why).out_of_scope();
        }
        self.push(step);
        let rec = |metric: &'static str,
                   label: &'static str,
                   v: Vec<u64>,
                   target: Option<f64>|
         -> Result<Row, String> {
            Ok(Row::timed(
                self.run,
                c.nfr,
                c.case,
                c.item,
                metric,
                label,
                self.threads,
                stats(v)?,
                target,
                false,
                load,
            ))
        };
        let tiles_note = format!("変わったタイル 平均 {tiles_mean:.1}・最大 {tiles_max} 枚");
        let rows = [
            rec(
                "render",
                "うち表示（ViewInfo.ms：段の計画・確保・表示の経路）",
                s.render,
                None,
            )?,
            rec(
                "tiles-rgba",
                "変わったタイルの書き出し RGBA8（6 本同時）",
                s.rgba,
                None,
            )?
            .with_note(&tiles_note),
            rec(
                "tiles-png",
                "変わったタイルの書き出し PNG（6 本同時）",
                s.png,
                None,
            )?
            .with_note(&tiles_note),
            rec(
                "total-rgba",
                "1 手 ＋ タイル RGBA8（Rust の中の分）",
                s.total_rgba,
                (c.nfr == 11).then_some(NFR11_TOTAL_MS),
            )?
            .with_note(if c.nfr == 11 {
                "IPC・WebKit を含まない（判定は画面で）"
            } else {
                ""
            }),
            rec(
                "total-png",
                "1 手 ＋ タイル PNG（Rust の中の分）",
                s.total_png,
                (c.nfr == 11).then_some(NFR11_TOTAL_MS),
            )?,
        ];
        for r in rows {
            self.push(r);
        }
        Ok(())
    }

    /// 閉じて、予算が戻ったかを数える。
    fn finish(&mut self, open: Open<'_>) -> Result<(), String> {
        self.max_interactive = self.max_interactive.max(open.max_interactive);
        self.reserved = self.reserved.max(open.reserved);
        let (same, before, after) = open.close()?;
        self.restored.1 += 1;
        if same {
            self.restored.0 += 1;
        } else {
            emit(&format!(
                "閉じたあと予算が戻りません: 前 {before:?}・後 {after:?}"
            ));
        }
        Ok(())
    }

    /// NFR-11：スライダー（`Step::Preview`）。写真は段 2 で判定し、段 1 も記録する。
    fn sweep(&mut self, s: &Sweep, item: i64, scale: f64, level: u8) -> Result<(), String> {
        let load = load1();
        let case = if level == s.level {
            s.case.to_string()
        } else {
            format!("{}-level{level}", s.case)
        };
        let label = if s.level > 0 {
            format!("{}（段 {level}）", s.item)
        } else {
            s.item.to_string()
        };
        let vp = Viewport {
            frame: 0,
            rect: if scale >= 1.0 { self.rect() } else { None },
            scale,
            interacting: false,
            focus: Some(s.node),
        };
        let mut open = Open::new(self.session, item, vp, &s.graph)?;
        let edit = s.edit.clone();
        let samples = self.measure(
            &mut open,
            |_, _| Ok(()),
            |i| Ok(Step::Preview { edit: edit(i) }),
        )?;
        let levels = samples.levels.clone();
        let fallback = levels.iter().any(|l| *l != level);
        let c = Case {
            nfr: 11,
            case: &case,
            item: &label,
            target_ms: 25.0,
            judge: s.judged && level == s.level && !fallback,
            out_of_scope: (!s.judged).then_some(s.note),
            note: if fallback {
                "求めた段で描けなかった"
            } else {
                ""
            },
        };
        self.per_tile(&open, &c, load)?;
        self.rows_for(&c, samples, load, json!({ "scale": scale }))?;
        self.finish(open)
    }

    fn pixel(&mut self) -> Result<(), String> {
        let rgba = self.item(SPRITE_RGBA)?;
        let indexed = self.item(SPRITE_INDEXED)?;
        for s in pixel_sweeps(self.z.pixel).map_err(|e| e.to_string())? {
            let item = if s.case == "pixel-palette-replace" {
                indexed
            } else {
                rgba
            };
            self.sweep(&s, item, 1.0, s.level)?;
        }
        Ok(())
    }

    fn photo(&mut self, photo: &kura_engine::Value) -> Result<(), String> {
        let item = self.item(PHOTO)?;
        for s in photo_sweeps(photo.clone()).map_err(|e| e.to_string())? {
            // 段 2（判定）は 1/4 倍、段 1（記録）は 1/2 倍で見ている
            self.sweep(&s, item, 0.25, PHOTO_LEVEL)?;
            self.sweep(&s, item, 0.5, 1)?;
        }
        Ok(())
    }

    /// NFR-12：筆を `sizes` の太さで足す（1 本ごとに確定。描画のノードへの `AppendItems`）。
    #[allow(clippy::too_many_arguments)]
    fn strokes(
        &mut self,
        item: i64,
        side: u32,
        prior: usize,
        tail: Option<(&str, J)>,
        sizes: &[u8],
        case_of: impl Fn(u8) -> String,
        item_of: impl Fn(u8) -> String,
        judge: bool,
        seed: u64,
    ) -> Result<(), String> {
        let (g, paint) = paint_doc(prior_strokes(prior, side).map_err(|e| e.to_string())?, tail)
            .map_err(|e| e.to_string())?;
        let vp = Viewport {
            frame: 0,
            rect: self.rect(),
            scale: 1.0,
            interacting: false,
            focus: Some(paint),
        };
        let open_load = load1();
        let mut open = Open::new(self.session, item, vp, &g)?;
        let mut first = true;
        for &size in sizes {
            let load = load1();
            let (case, label) = (case_of(size), item_of(size));
            let samples = self.measure(
                &mut open,
                |_, _| Ok(()),
                |i| {
                    let item = stroke_item(seed + i as u64 * 31 + u64::from(size), size, side)
                        .map_err(|e| e.to_string())?;
                    Ok(append(paint, item))
                },
            )?;
            let c = Case {
                nfr: 12,
                case: &case,
                item: &label,
                target_ms: 8.0,
                judge,
                out_of_scope: (!stroke_in_scope(size)).then_some(OUT_OF_SCOPE_STROKE),
                note: "",
            };
            if first {
                self.per_tile(&open, &c, open_load)?;
                first = false;
            }
            self.rows_for(
                &c,
                samples,
                load,
                json!({ "prior": prior, "size": size, "side": side }),
            )?;
        }
        self.finish(open)
    }

    fn stroke(&mut self) -> Result<(), String> {
        let side = self.z.canvas;
        let item = self.item(&canvas_name(side))?;
        let sizes = self.z.stroke_sizes.clone();
        for prior in self.z.priors.clone() {
            self.strokes(
                item,
                side,
                prior,
                None,
                &sizes,
                |size| format!("stroke-{prior}-{size}px"),
                |size| format!("筆 1 本（500 点）の確定（{side}²・前の筆 {prior} 本・{size}px）"),
                true,
                7_000,
            )?;
        }
        Ok(())
    }

    fn nearest(&mut self) -> Result<(), String> {
        let sizes = self.z.nearest_strokes.clone();
        let prior = self.z.nearest_prior;
        for side in self.z.nearest_sides.clone() {
            let item = self.item(&canvas_name(side))?;
            self.strokes(
                item,
                side,
                prior,
                Some(nearest4()),
                &sizes,
                |size| format!("stroke-nearest4-{side}-{size}px"),
                |size| {
                    format!(
                        "筆のあとに最近傍 ×4（{side}² → {}²・前の筆 {prior} 本・{size}px）",
                        side * 4
                    )
                },
                side * 4 <= self.z.canvas,
                9_000,
            )?;
        }
        Ok(())
    }

    /// NFR-12：全面の塗りつぶし（1 手ごとに測らずに取り消して、同じ単色の画像に塗る）。
    fn fill(&mut self) -> Result<(), String> {
        let side = self.z.canvas;
        let item = self.item(&canvas_name(side))?;
        let (g, paint) = paint_doc(Vec::new(), None).map_err(|e| e.to_string())?;
        let vp = Viewport {
            frame: 0,
            rect: self.rect(),
            scale: 1.0,
            interacting: false,
            focus: Some(paint),
        };
        let open_load = load1();
        let mut open = Open::new(self.session, item, vp, &g)?;
        let mut first = true;
        for contiguous in [true, false] {
            let load = load1();
            let case = if contiguous {
                "fill-contiguous"
            } else {
                "fill-global"
            };
            let label = format!(
                "全面の塗りつぶし（{side}²・{}）",
                if contiguous {
                    "4 連結のつながった全面"
                } else {
                    "画像全体の同じ色"
                }
            );
            let mut appended = false;
            let samples = self.measure(
                &mut open,
                |o, _| {
                    if appended {
                        o.step(Step::Undo)?;
                    }
                    appended = true;
                    Ok(())
                },
                |i| {
                    let item = fill_item(i, 0, contiguous).map_err(|e| e.to_string())?;
                    Ok(append(paint, item))
                },
            )?;
            // 最後の塗りつぶしを取り消して、次の塗り方を同じ単色の画像から始める
            open.step(Step::Undo)?;
            let c = Case {
                nfr: 12,
                case,
                item: &label,
                target_ms: 30.0,
                judge: true,
                out_of_scope: None,
                note: "",
            };
            if first {
                self.per_tile(&open, &c, open_load)?;
                first = false;
            }
            self.rows_for(&c, samples, load, json!({ "contiguous": contiguous }))?;
        }
        self.finish(open)
    }

    /// NFR-17：閉じたあと予算が戻ったケースの数と、開いている間の取り置き・持ち続けた確保の最大。
    fn budget_rows(&mut self) {
        let load = load1();
        let (ok, all) = self.restored;
        let r = Row::value(
            self.run,
            17,
            "editor-restored",
            "エディタを閉じたあと予算の使用量（取り置き・対話・持ち続ける分・一括）が開く前と同じ",
            "restored",
            "戻ったケース",
            self.threads,
            ok as f64,
            "件",
            if ok == all {
                Verdict::Pass
            } else {
                Verdict::Fail
            },
            load,
        )
        .with_note(&format!("{ok}/{all} ケース"));
        self.push(r);
        let reserved = self.reserved;
        let r = Row::value(
            self.run,
            17,
            "editor-reserved",
            "開いている間の取り置き（min(1 GiB, 予算/2)）",
            "reserved",
            "取り置き",
            self.threads,
            mib(reserved),
            "MiB",
            Verdict::Record,
            load,
        );
        self.push(r);
        let r = Row::value(
            self.run,
            17,
            "editor-standing",
            "手のあとに持ち続けた対話の確保の最大（キャッシュ）",
            "interactive",
            "対話の確保",
            self.threads,
            mib(self.max_interactive),
            "MiB",
            Verdict::Record,
            load,
        );
        self.push(r);
    }
}

/// 描画のノードに 1 つ足して確定する手（M10 の bench edit と同じ）。
fn append(paint: NodeId, item: J) -> Step {
    Step::Commit {
        edit: Edit::AppendItems {
            node: paint,
            field: "actions".into(),
            items: vec![item],
        },
        mode: CommitMode::Step,
    }
}

/// `bench editor` の入口。
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    if cfg!(debug_assertions) && !opts.quick && opts.child.is_none() {
        return Err(
            "bench editor は release で回してください（cargo run --release -p kura-devtools -- bench editor）。\
             debug では --quick だけ回せます"
                .into(),
        );
    }
    if let Some(child) = &opts.child {
        return child_main(&opts, child);
    }
    let work = opts.work.clone().ok_or(USAGE)?;
    let root = super::fresh_dir(&work, "editor")?;
    let result = run_all(&opts, &root);
    if !opts.keep {
        let _ = std::fs::remove_dir_all(&root);
    }
    result
}

fn wants(opts: &Opts, group: &str) -> bool {
    opts.only.is_empty() || opts.only.contains(group)
}

fn run_all(opts: &Opts, root: &Path) -> Result<(), String> {
    let z = Sizes::new(opts.quick);
    let src = root.join("src");
    let lib = root.join("bench.kuralib");
    emit(&format!(
        "bench editor：入力を作っています（{}）",
        src.display()
    ));
    let photo = write_inputs(&src, &z)?;
    let items = {
        let session =
            LibrarySession::create(&lib, quiet(), super::services(opts.preview_threads, 3))
                .map_err(|e| format!("ライブラリを作れません: {e}"))?;
        session.wait_ready();
        let items = super::import_all(&session, &src)?;
        session.close();
        items
    };
    let env = super::env_json(json!({
        "bench": "editor",
        "preview_threads": opts.preview_threads,
        "threads": opts.threads,
        "quick": opts.quick,
        "runs": opts.runs,
        "tile_loads": TILE_LOADS,
        "no_tiles": opts.no_tiles,
        "viewport": opts.viewport,
        "tile_formats": [opts.formats.0, opts.formats.1],
        "library": lib.display().to_string(),
    }));
    emit(&format!("bench editor：{env}"));
    let set = super::thread_set(opts.preview_threads, &opts.threads);
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
            let session = LibrarySession::open(&lib, quiet(), super::services(threads, 3))
                .map_err(|e| format!("ライブラリを開けません: {e}"))?;
            session.wait_ready();
            super::settle(&session)?;
            let mut ctx = Ctx {
                run,
                threads,
                judged: threads == opts.preview_threads,
                z: &z,
                session: &session,
                items: &items,
                rows: Vec::new(),
                restored: (0, 0),
                max_interactive: 0,
                reserved: 0,
                no_tiles: opts.no_tiles,
                viewport: opts.viewport,
                formats: opts.formats,
            };
            if wants(opts, "pixel") {
                ctx.pixel()?;
            }
            if wants(opts, "photo") {
                ctx.photo(&photo)?;
            }
            if wants(opts, "stroke") {
                ctx.stroke()?;
            }
            if wants(opts, "nearest") {
                ctx.nearest()?;
            }
            if wants(opts, "fill") {
                ctx.fill()?;
            }
            if ctx.restored.1 > 0 {
                ctx.budget_rows();
            }
            rows.append(&mut ctx.rows);
            session.close();
        }
        if wants(opts, "mem") {
            let photo_id = items
                .get(PHOTO)
                .copied()
                .ok_or("写真を取り込めていません")?;
            rows.extend(memory(opts, &lib, photo_id, run)?);
        }
        runs.push(json!({
            "run": run,
            "loadavg_before": before,
            "loadavg_after": loadavg(),
            "seconds": started.elapsed().as_secs_f64(),
        }));
    }
    let report = Report {
        title: "bench editor（エディタのアプリの道：edit::editor の 1 手と /edit のタイル）",
        env,
        runs,
        rows,
        judged_threads: opts.preview_threads,
        thread_metrics: &["step", "tiles-rgba", "tiles-png", "total-rgba"],
    };
    super::write_report(&report, opts.json.as_deref(), opts.markdown.as_deref())
}

/// `/usr/bin/time -l` の出力の `<数> <名前>` を読む。
fn read_count(stderr: &str, name: &str) -> Option<u64> {
    stderr
        .lines()
        .find(|l| l.contains(name))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

/// 子を `/usr/bin/time -l` で起こし、最後の JSON の行と peak memory footprint を読む。
fn spawn_child(opts: &Opts, case: &str, lib: &Path, item: i64) -> Result<(J, Option<u64>), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new("/usr/bin/time");
    cmd.arg("-l")
        .arg(exe)
        .args(["bench", "editor", "--child", case, "--library"])
        .arg(lib)
        .args([
            "--item",
            &item.to_string(),
            "--preview-threads",
            &opts.preview_threads.to_string(),
        ]);
    if opts.quick {
        cmd.arg("--quick");
    }
    let out = cmd
        .output()
        .map_err(|e| format!("/usr/bin/time を起こせません: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let result = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<J>(l).ok())
        .ok_or_else(|| {
            format!(
                "子（{case}）の結果がありません: {}",
                stderr.lines().rev().take(5).collect::<Vec<_>>().join(" / ")
            )
        })?;
    Ok((result, read_count(&stderr, "peak memory footprint")))
}

/// NFR-17 の親：基準の子と 24MP を開く子を起こして記録する。
fn memory(opts: &Opts, lib: &Path, photo: i64, run: usize) -> Result<Vec<Row>, String> {
    let load = load1();
    let (base, base_peak) = spawn_child(opts, "base", lib, photo)?;
    let (got, peak) = spawn_child(opts, "photo", lib, photo)?;
    if got["outcome"] != "ok" {
        return Err(format!("子（photo）が失敗しました: {got}"));
    }
    let t = opts.preview_threads;
    let total = got["total"].as_u64().unwrap_or(0);
    let mut rows = Vec::new();
    let field = |k: &str| got[k].as_u64().unwrap_or(0);
    let item = "24MP の JPEG をエディタで開く（段 2 で鎖の切り抜きを動かし、等倍に切り替えて手を当て、閉じる）";
    let restored = got["restored"].as_bool().unwrap_or(false);
    rows.push(
        Row::value(
            run,
            17,
            "mem-photo-restored",
            item,
            "restored",
            "閉じたあと予算が戻った",
            t,
            f64::from(u8::from(restored)),
            "",
            if restored {
                Verdict::Pass
            } else {
                Verdict::Fail
            },
            load,
        )
        .with_extra(json!({ "before": got["before"], "after": got["after"] })),
    );
    rows.push(Row::value(
        run,
        17,
        "mem-photo-reserved",
        item,
        "reserved",
        "開いている間の取り置き",
        t,
        mib(field("reserved_open")),
        "MiB",
        Verdict::Record,
        load,
    ));
    rows.push(Row::value(
        run,
        17,
        "mem-photo-level2",
        item,
        "max-level2",
        "予算で数えた使用量の最大（段 2）",
        t,
        mib(field("max_used_level2")),
        "MiB",
        Verdict::Record,
        load,
    ));
    rows.push(
        Row::value(
            run,
            17,
            "mem-photo-level0",
            item,
            "max-level0",
            "予算で数えた使用量の最大（等倍まで）",
            t,
            mib(field("max_used")),
            "MiB",
            if field("max_used") <= total {
                Verdict::Pass
            } else {
                Verdict::Fail
            },
            load,
        )
        .with_note(&format!("予算 {:.0} MiB", mib(total))),
    );
    if let (Some(p), Some(b)) = (peak, base_peak) {
        let delta = p.saturating_sub(b);
        rows.push(
            Row::value(
                run,
                17,
                "mem-photo-footprint",
                item,
                "footprint",
                "peak memory footprint − 基準の子",
                t,
                mib(delta),
                "MiB",
                if delta <= total {
                    Verdict::Pass
                } else {
                    Verdict::Fail
                },
                load,
            )
            .with_note(&format!(
                "子 {:.0} MiB・基準 {:.0} MiB・予算 {:.0} MiB",
                mib(p),
                mib(b),
                mib(total)
            ))
            .with_extra(json!({ "peak": p, "base": b, "base_child": base })),
        );
    }
    for r in &rows {
        emit(&r.line());
    }
    Ok(rows)
}

fn usage_json(u: &Usage) -> J {
    json!({ "total": u.total, "interactive": u.interactive, "bulk": u.bulk, "reserved": u.reserved, "standing": u.standing })
}

/// 子：`base` はライブラリを開いて実行の場とコーデックの組を作るだけ。`photo` は写真をエディタで開き、段 2 で手を当て、等倍に
/// 切り替えて手を当てて閉じる。予算の使用量を 200 µs ごとに見て最大を出す。結果を 1 行の JSON で標準出力に書く。
fn child_main(opts: &Opts, case: &str) -> Result<(), String> {
    let lib = opts.library.clone().ok_or("--library がありません")?;
    let item = opts.item.ok_or("--item がありません")?;
    let session = LibrarySession::open(&lib, quiet(), super::services(opts.preview_threads, 3))
        .map_err(|e| format!("ライブラリを開けません: {e}"))?;
    session.wait_ready();
    super::settle(&session)?;
    let engine = session.engine().ok_or("エンジンの部品がありません")?;
    let rt = engine.runtime().map_err(|e| e.to_string())?.clone();
    let _codecs = engine.codecs();
    let budget = rt.budget().clone();
    let before = budget.usage().map_err(|e| e.to_string())?;
    let out = match case {
        "base" => json!({ "outcome": "base", "before": usage_json(&before) }),
        "photo" => {
            let z = Sizes::new(opts.quick);
            let photo = InputGen::Photo {
                w: z.photo.0,
                h: z.photo.1,
                seed: 7,
            }
            .generate()
            .map_err(|e| e.to_string())?;
            let sweeps = photo_sweeps(photo).map_err(|e| e.to_string())?;
            let chain = sweeps
                .iter()
                .find(|s| s.case == "photo-chain")
                .ok_or("鎖のケースがありません")?;
            let stop = Arc::new(AtomicBool::new(false));
            let peaks = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
            let level0 = Arc::new(AtomicBool::new(false));
            let sampler = {
                let (stop, peaks, level0, budget) =
                    (stop.clone(), peaks.clone(), level0.clone(), budget.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if let Ok(u) = budget.usage() {
                            let used = (u.interactive + u.bulk) as usize;
                            peaks[0].fetch_max(used, Ordering::Relaxed);
                            if !level0.load(Ordering::Relaxed) {
                                peaks[1].fetch_max(used, Ordering::Relaxed);
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_micros(200));
                    }
                })
            };
            let vp = Viewport {
                frame: 0,
                rect: None,
                scale: 0.25,
                interacting: false,
                focus: Some(chain.node),
            };
            let mut open = Open::new(&session, item, vp, &chain.graph)?;
            for i in 0..z.mem_steps {
                open.step(Step::Preview {
                    edit: (chain.edit)(i),
                })?;
            }
            let reserved_open = budget.usage().map_err(|e| e.to_string())?.reserved;
            let level2 = open.first.level;
            level0.store(true, Ordering::Relaxed);
            open.vp.scale = 1.0;
            let (_, full) = open.step(Step::Preview {
                edit: (chain.edit)(z.mem_steps),
            })?;
            for i in 0..z.mem_steps {
                open.step(Step::Preview {
                    edit: (chain.edit)(z.mem_steps + 1 + i),
                })?;
            }
            let (restored, _, after) = open.close()?;
            stop.store(true, Ordering::Relaxed);
            let _ = sampler.join();
            json!({
                "outcome": "ok",
                "total": budget.total(),
                "reserved_open": reserved_open,
                "expected_reserved": default_reserved(budget.total()),
                "level2": level2,
                "level_full": full.level,
                "max_used_level2": peaks[1].load(Ordering::Relaxed),
                "max_used": peaks[0].load(Ordering::Relaxed),
                "restored": restored,
                "before": usage_json(&before),
                "after": usage_json(&after),
                "full_canvas": full.canvas,
                "ms_full": ms((full.ms * 1e6) as u64),
            })
        }
        other => return Err(format!("知らない子のケースです: {other}")),
    };
    session.close();
    println_json(&out)
}

/// 標準出力に 1 行の JSON（親が読む）。
fn println_json(v: &J) -> Result<(), String> {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{v}").map_err(|e| e.to_string())
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
            "--work /tmp/x --preview-threads 4 --threads 3,16 --runs 3 --quick --only pixel,mem --json a.json --markdown - --keep",
        ))
        .unwrap();
        assert_eq!(o.work, Some(PathBuf::from("/tmp/x")));
        assert_eq!((o.preview_threads, o.runs), (4, 3));
        assert_eq!(o.threads, vec![3, 16]);
        assert!(o.quick && o.keep);
        assert_eq!(
            o.only,
            BTreeSet::from(["pixel".to_string(), "mem".to_string()])
        );
        assert!(parse(&args("--threads 3")).is_err(), "--work がない");
        assert!(parse(&args("--work x --only audio")).is_err());
        assert!(parse(&args("--work x --runs 0")).is_err());
        assert!(parse(&args("--child base --library l --item 1")).is_ok());
        let d = parse(&args(
            "--work x --tile-format rgba --viewport 1600x1000 --no-tiles",
        ))
        .unwrap();
        assert_eq!(d.formats, (true, false));
        assert_eq!(d.viewport, Some((1600, 1000)));
        assert!(d.no_tiles);
        assert_eq!(parse(&args("--work x")).unwrap().formats, (true, true));
        assert!(parse(&args("--work x --tile-format gif")).is_err());
        assert!(parse(&args("--work x --viewport 0x10")).is_err());
        assert!(parse(&args("--work x --viewport 100")).is_err());
    }

    #[test]
    fn tile_lists_cover_the_frame_or_the_changes() {
        let info = |changed| ViewInfo {
            rev: 1,
            level: 0,
            fallback: None,
            full: (600, 300),
            canvas: (600, 300),
            frames: vec![kura_core::edit::editor::FrameInfo {
                index: 0,
                rect: kura_engine::geom::IRect::new(0, 0, 600, 300),
                duration_ms: None,
            }],
            format: None,
            tile: 256,
            changed,
            meta_changed: false,
            warnings: Vec::new(),
            drawn_nodes: 0,
            ms: 0.0,
        };
        let whole = Viewport::whole();
        let all = tile_list(&info(ChangedTiles::All), &whole);
        assert_eq!(all.len(), 3 * 2);
        assert_eq!(all[5], (0, 2, 1));
        let some = tile_list(
            &info(ChangedTiles::Tiles(vec![(0, vec![[1, 0], [2, 1]])])),
            &whole,
        );
        assert_eq!(some, vec![(0, 1, 0), (0, 2, 1)]);
        // 見ている範囲（等倍の 300×200）の外のタイルは取りに行かない
        let part = Viewport {
            rect: Some(kura_engine::geom::IRect::new(0, 0, 300, 200)),
            ..Viewport::whole()
        };
        assert_eq!(
            tile_list(&info(ChangedTiles::All), &part),
            vec![(0, 0, 0), (0, 1, 0)]
        );
        let some = tile_list(
            &info(ChangedTiles::Tiles(vec![(0, vec![[1, 0], [2, 1]])])),
            &part,
        );
        assert_eq!(some, vec![(0, 1, 0)]);
    }

    /// 極小の大きさで通しで回す（debug。取り込み・エディタ・タイル・子の NFR-17 まで）。
    #[test]
    fn quick_run_goes_through_the_app_path() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("r.json");
        let a = vec![
            "--work".to_string(),
            dir.path().join("w").display().to_string(),
            "--quick".into(),
            "--only".into(),
            "pixel,photo,stroke,nearest,fill".into(),
            "--json".into(),
            out.display().to_string(),
        ];
        run(&a).unwrap();
        let j: J = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let rows = j["rows"].as_array().unwrap();
        assert!(
            rows.iter()
                .any(|r| r["case"] == "photo-crop" && r["metric"] == "step")
        );
        assert!(
            rows.iter()
                .any(|r| r["case"] == "fill-global" && r["metric"] == "tiles-png")
        );
        let restored = rows.iter().find(|r| r["metric"] == "restored").unwrap();
        assert_eq!(restored["verdict"], "pass", "{restored}");
        // 作業の場所は消える
        assert_eq!(std::fs::read_dir(dir.path().join("w")).unwrap().count(), 0);
    }
}
