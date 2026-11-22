//! プロジェクトの照らし直しの時間（`bench project --library <kuralib> --out <dir> [--files N] [--bytes B] [--matched N]
//! [--runs N] [--json <file>]`。M15 段 8）。
//!
//! 1. `--out` に合成の Godot のプロジェクトを作る（もうあって、同じ大きさで作ったものなら使い直す）：照らす素材のファイルを
//!    `--files` 個（既定 2 万）、合わせて約 `--bytes`（既定 4 GB）。中身はどれも ChaCha8 の乱数（どれも違う）で、大きさは
//!    16 KiB〜2 MiB の対数一様を合わせて `--bytes` になるように揃える。うち `--matched` 個（既定は 4 分の 1。
//!    `library_assets/` の下）は、同じハッシュのアイテムをライブラリに足して（なければ）、照らすと結び付くようにする
//!    （`seed` の実ファイルは共通のひな形で、アイテムのハッシュと中身が合わないので写しでは結び付かない）。Godot と同じく
//!    素材ごとに `.import` の添えファイル（照らさない形式）を置き、照らさないキャッシュ（`.godot/imported/`）も置く。
//! 2. ライブラリにプロジェクトを登録し（登録済みなら使い直す）、前の行を消して、照らし直しのジョブ（アプリと同じ
//!    `ProjectHandler`）を次の順に回す：初めての照らし（全部ハッシュ）、変わっていない照らし直しを `--runs` 回、素材の 1% を
//!    同じ大きさの別の中身に書き換えたあとの照らし直し。時間はジョブを積んでから終わる（Done）まで。
//! 3. 結果を 1 行の JSON で出す（`--json` があれば追記する）。
//!
//! ファイルは作った直後なので、初めての照らしもページキャッシュから読む（冷えた読み取りは測らない。`purge` は管理者が要る）。
//! ライブラリは自分で用意する（`seed --items 100000 --out …`）。利用者の本物のライブラリは渡さない（登録と照らしの行を書く）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_core::import::classify;
use kura_core::jobs::{EventSink, JobHandler, JobKind, JobManager, JobState};
use kura_core::library::Library;
use kura_core::project::scan::{ProjectHandler, ScanResult};
use kura_core::project::{self, Engine};
use kura_core::time::now_ms;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use serde_json::{Value as J, json};

const USAGE: &str = "使い方: kura-devtools bench project --library <kuralib> --out <dir> [--files N] [--bytes B] \
                     [--matched N] [--runs N] [--json <file>]";

/// 作ったプロジェクトの印（作ったときの大きさ。同じなら使い直す）。
const MARKER: &str = ".kura-bench-project.json";

/// 乱数の中身の素材（置くフォルダ・拡張子・重み）。どれも取り込める形式（`classify` で確かめる）。
const KINDS: [(&str, &str, u32); 9] = [
    ("sprites", "png", 50),
    ("sprites", "webp", 5),
    ("art", "psd", 5),
    ("art", "aseprite", 5),
    ("audio/sfx", "ogg", 15),
    ("audio/sfx", "wav", 10),
    ("audio/bgm", "mp3", 5),
    ("fonts", "ttf", 2),
    ("models", "glb", 3),
];

/// 乱数の中身の大きさの範囲（対数一様。あとで合計を揃える）。
const MIN_SIZE: f64 = 16.0 * 1024.0;
const MAX_SIZE: f64 = 2.0 * 1024.0 * 1024.0;

/// 照らさないキャッシュ（`.godot/imported/`）に置くファイルの数。
const CACHE_FILES: usize = 1_000;

struct Opts {
    library: PathBuf,
    out: PathBuf,
    files: usize,
    bytes: u64,
    matched: Option<usize>,
    runs: usize,
    json: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut library = None;
    let mut out = None;
    let mut opts = Opts {
        library: PathBuf::new(),
        out: PathBuf::new(),
        files: 20_000,
        bytes: 4_000_000_000,
        matched: None,
        runs: 3,
        json: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or(USAGE.to_string());
        let number = |s: String| s.parse::<u64>().map_err(|_| USAGE.to_string());
        match arg.as_str() {
            "--library" => library = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--files" => opts.files = number(value()?)? as usize,
            "--bytes" => opts.bytes = number(value()?)?,
            "--matched" => opts.matched = Some(number(value()?)? as usize),
            "--runs" => opts.runs = number(value()?)?.max(1) as usize,
            "--json" => opts.json = Some(PathBuf::from(value()?)),
            other => return Err(format!("不明な引数: {other}\n{USAGE}")),
        }
    }
    opts.library = library.ok_or(USAGE.to_string())?;
    opts.out = out.ok_or(USAGE.to_string())?;
    if opts.files == 0 {
        return Err("--files は 1 以上です".into());
    }
    Ok(opts)
}

/// `bench project …` の入口。
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse(args)?;
    for (_, ext, _) in KINDS {
        if classify(ext).is_none() {
            return Err(format!("取り込めない形式を置こうとしました: {ext}"));
        }
    }
    let library = Arc::new(Library::open(&opts.library).map_err(|e| e.to_string())?);
    let matched_want = opts.matched.unwrap_or(opts.files / 4).min(opts.files);
    let built = Instant::now();
    let made = make_project(&opts, &library, matched_want)?;
    let make_s = built.elapsed().as_secs_f64();
    tracing::info!(
        files = made.files.len(),
        matched = made.matched,
        bytes = made.bytes,
        reused = made.reused,
        seconds = make_s,
        "プロジェクトを用意しました"
    );

    let id = register(&library, &opts.out)?;
    let sink: EventSink = Arc::new(|_| {});
    let jobs = JobManager::new(
        library.clone(),
        vec![(
            JobKind::Project,
            Arc::new(ProjectHandler) as Arc<dyn JobHandler>,
        )],
        sink,
    )
    .map_err(|e| e.to_string())?;
    jobs.start().map_err(|e| e.to_string())?;

    let (first_ms, first) = scan(&jobs)?;
    tracing::info!(
        ms = first_ms,
        files = first.files,
        hashed = first.hashed,
        matched = first.matched,
        "初めての照らし"
    );
    let mut unchanged = Vec::new();
    let mut unchanged_result = ScanResult::default();
    for n in 0..opts.runs {
        let (ms, r) = scan(&jobs)?;
        tracing::info!(
            run = n + 1,
            ms,
            hashed = r.hashed,
            "変わっていない照らし直し"
        );
        unchanged.push(ms);
        unchanged_result = r;
    }
    let changed = rewrite_some(&made, opts.files.div_ceil(100))?;
    let (changed_ms, after) = scan(&jobs)?;
    tracing::info!(
        changed,
        ms = changed_ms,
        hashed = after.hashed,
        "書き換えたあとの照らし直し"
    );

    let rows = library
        .db()
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*), count(item_id) FROM project_files WHERE project_id = ?1",
                [id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )?)
        })
        .map_err(|e| e.to_string())?;
    let mut sorted = unchanged.clone();
    sorted.sort_by(f64::total_cmp);
    let line = json!({
        "bench": "project",
        "files": made.files.len(),
        "matchedFiles": made.matched,
        "bytes": made.bytes,
        "libraryItems": item_count(&library)?,
        "firstMs": round(first_ms),
        "first": first,
        "unchangedMs": unchanged.iter().map(|ms| round(*ms)).collect::<Vec<_>>(),
        "unchangedMedianMs": round(sorted[sorted.len() / 2]),
        "unchanged": unchanged_result,
        "changedFiles": changed,
        "changedMs": round(changed_ms),
        "changed": after,
        "rows": rows.0,
        "rowsWithItem": rows.1,
        "threads": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
    });
    writeln!(std::io::stdout().lock(), "{line}").map_err(|e| e.to_string())?;
    if let Some(path) = &opts.json {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| e.to_string())?;
    }
    check(&made, &first, &unchanged_result, changed, &after)
}

fn round(ms: f64) -> f64 {
    (ms * 10.0).round() / 10.0
}

/// 数え方が合っているか（合わなければ測りとして使えないので失敗にする）。
fn check(
    made: &Made,
    first: &ScanResult,
    unchanged: &ScanResult,
    changed: usize,
    after: &ScanResult,
) -> Result<(), String> {
    let files = made.files.len() as u64;
    let mut wrong = Vec::new();
    if (first.files, first.hashed) != (files, files) {
        wrong.push(format!(
            "初めての照らしは {files} 個をハッシュするはず（{} 個を照らし {} 個をハッシュ）",
            first.files, first.hashed
        ));
    }
    if first.matched < made.matched as u64 {
        wrong.push(format!(
            "ライブラリの写し {} 個が結び付くはず（一致 {}）",
            made.matched, first.matched
        ));
    }
    if (unchanged.files, unchanged.hashed) != (files, 0) {
        wrong.push(format!(
            "変わっていない照らし直しはハッシュしないはず（{} 個を照らし {} 個をハッシュ）",
            unchanged.files, unchanged.hashed
        ));
    }
    if after.hashed != changed as u64 {
        wrong.push(format!(
            "書き換えた {changed} 個だけをハッシュするはず（{} 個）",
            after.hashed
        ));
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(wrong.join("\n"))
    }
}

fn item_count(library: &Library) -> Result<i64, String> {
    library
        .db()
        .read(|c| Ok(c.query_row("SELECT count(*) FROM items", [], |r| r.get(0))?))
        .map_err(|e| e.to_string())
}

/// 照らし直しのジョブを 1 本積み、終わるまで待つ（ジョブを積んでから Done まで。ほかの種類のジョブは見ない）。
fn scan(jobs: &JobManager) -> Result<(f64, ScanResult), String> {
    let started = Instant::now();
    let id = project::queue_scan(jobs)
        .map_err(|e| e.to_string())?
        .ok_or("照らし直しのジョブがもう積まれています")?;
    loop {
        let record = jobs.get(id).map_err(|e| e.to_string())?;
        match record.state {
            JobState::Done => {
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                let result: ScanResult =
                    serde_json::from_str(record.result_json.as_deref().unwrap_or("{}"))
                        .map_err(|e| e.to_string())?;
                return Ok((ms, result));
            }
            JobState::Failed | JobState::Canceled => {
                return Err(format!(
                    "照らし直しが終わりませんでした：{:?} {:?}",
                    record.error_code, record.message
                ));
            }
            _ => {}
        }
        if started.elapsed() > Duration::from_secs(3600) {
            return Err("1 時間待っても終わりませんでした".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// プロジェクトを登録する（同じ場所が登録済みなら使い直す）。前の回の行は消す（初めての照らしで全部ハッシュする）。
fn register(library: &Library, out: &Path) -> Result<i64, String> {
    let existing = library
        .db()
        .read(project::list)
        .map_err(|e| e.to_string())?;
    let canonical = out.canonicalize().map_err(|e| e.to_string())?;
    let id = match existing
        .iter()
        .find(|p| Path::new(&p.root) == canonical.as_path())
    {
        Some(p) => p.id,
        None => {
            let (root, engine) =
                project::prepare_root(out, library.layout().root(), &existing, None)
                    .map_err(|e| e.to_string())?;
            if engine != Engine::Godot {
                return Err(format!(
                    "Godot のプロジェクトと見分けられませんでした（{engine:?}）"
                ));
            }
            library
                .db()
                .write(move |tx| project::add(tx, &root, engine, Some("bench"), now_ms()))
                .map_err(|e| e.to_string())?
        }
    };
    library
        .db()
        .write(move |tx| {
            tx.execute("DELETE FROM project_files WHERE project_id = ?1", [id])?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// 作ったプロジェクト。
struct Made {
    /// 素材のファイル（ルートからの相対。ライブラリの写しは前、乱数の中身は後ろ）。
    files: Vec<String>,
    root: PathBuf,
    /// うちライブラリの写しの数。
    matched: usize,
    bytes: u64,
    reused: bool,
}

/// 合成の Godot のプロジェクトを作る（印が同じ大きさなら、置いたファイルの一覧を作り直して使い直す）。
fn make_project(opts: &Opts, library: &Library, matched_want: usize) -> Result<Made, String> {
    let want = json!({"files": opts.files, "bytes": opts.bytes, "matched": matched_want});
    let marker = opts.out.join(MARKER);
    if let Ok(text) = std::fs::read_to_string(&marker)
        && let Ok(saved) = serde_json::from_str::<J>(&text)
        && saved.get("want") == Some(&want)
    {
        let files: Vec<String> =
            serde_json::from_value(saved["files"].clone()).map_err(|e| e.to_string())?;
        let items: Vec<(String, String, u64)> =
            serde_json::from_value(saved["items"].clone()).map_err(|e| e.to_string())?;
        ensure_items(library, &items)?;
        return Ok(Made {
            files,
            root: opts.out.clone(),
            matched: items.len(),
            bytes: saved["bytes"].as_u64().unwrap_or(0),
            reused: true,
        });
    }
    if opts.out.exists()
        && std::fs::read_dir(&opts.out)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(format!(
            "{} は空ではなく、同じ大きさで作ったプロジェクトでもありません（空のフォルダを渡してください）",
            opts.out.display()
        ));
    }
    let root = opts.out.clone();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let put = |rel: &str, body: &[u8]| -> Result<(), String> {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, body).map_err(|e| e.to_string())
    };
    put(
        "project.godot",
        b"config_version=5\n\n[application]\n\nconfig/name=\"kura-bench\"\n",
    )?;
    let mut files = Vec::with_capacity(opts.files);
    let mut bytes = 0u64;

    // 乱数の中身（大きさは対数一様の重みを、合計に揃える）。前の `matched_want` 個はアイテムを足して結び付ける
    let mut rng = ChaCha8Rng::seed_from_u64(0x6b75_7261_5f70_726a);
    let mut items: Vec<(String, String, u64)> = Vec::with_capacity(matched_want);
    let rest = opts.files;
    let total_weight: u32 = KINDS.iter().map(|k| k.2).sum();
    let weights: Vec<f64> = (0..rest)
        .map(|_| {
            let u = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
            (MIN_SIZE.ln() + u * (MAX_SIZE.ln() - MIN_SIZE.ln())).exp()
        })
        .collect();
    let budget = opts.bytes as f64;
    let scale = budget / weights.iter().sum::<f64>().max(1.0);
    let mut buf = Vec::new();
    for (i, w) in weights.iter().enumerate() {
        let mut pick = (rng.next_u64() % u64::from(total_weight)) as u32;
        let (dir, ext, _) = KINDS
            .iter()
            .find(|k| {
                if pick < k.2 {
                    true
                } else {
                    pick -= k.2;
                    false
                }
            })
            .copied()
            .unwrap_or(KINDS[0]);
        let size = ((w * scale) as usize).max(64);
        buf.resize(size, 0);
        rng.fill_bytes(&mut buf);
        let rel = if i < matched_want {
            format!("library_assets/{dir}/{:03}/item_{i}.{ext}", i / 500)
        } else {
            format!("{dir}/{:03}/asset_{i}.{ext}", i / 500)
        };
        put(&rel, &buf)?;
        if i < matched_want {
            let hash = blake3::hash(&buf).to_hex().to_string();
            items.push((hash, ext.to_string(), size as u64));
        }
        put(&format!("{rel}.import"), import_sidecar(&rel).as_bytes())?;
        bytes += size as u64;
        files.push(rel);
    }

    // 照らさないキャッシュ
    let mut small = [0u8; 4096];
    for i in 0..CACHE_FILES {
        rng.fill_bytes(&mut small);
        put(&format!(".godot/imported/cache_{i}.ctex"), &small)?;
    }

    ensure_items(library, &items)?;
    let saved = json!({"want": want, "files": files, "items": items, "bytes": bytes});
    std::fs::write(&marker, saved.to_string()).map_err(|e| e.to_string())?;
    Ok(Made {
        files,
        root,
        matched: items.len(),
        bytes,
        reused: false,
    })
}

/// 結び付ける素材と同じハッシュのアイテムを足す（同じハッシュのアイテムがあれば足さない）。実ファイルは置かない
/// （照らしは DB のハッシュだけを見る）。1 つの書き込みでまとめて足す。
fn ensure_items(library: &Library, items: &[(String, String, u64)]) -> Result<(), String> {
    let rows: Vec<(Vec<u8>, String, &'static str, i64)> = items
        .iter()
        .map(|(hex, ext, size)| {
            let hash = blake3::Hash::from_hex(hex).map_err(|e| e.to_string())?;
            let kind = classify(ext).ok_or(format!("取り込めない形式: {ext}"))?;
            Ok((
                hash.as_bytes().to_vec(),
                ext.clone(),
                kind.as_str(),
                i64::try_from(*size).unwrap_or(i64::MAX),
            ))
        })
        .collect::<Result<_, String>>()?;
    let now = now_ms();
    library
        .db()
        .write(move |tx| {
            let mut stmt = tx.prepare(
                "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                 SELECT ?1, ?2, ?3, ?4, 'bench-project', 'bench-project.' || ?2, ?5, ?5
                 WHERE NOT EXISTS (SELECT 1 FROM items WHERE hash = ?1)",
            )?;
            for (hash, ext, kind, size) in &rows {
                stmt.execute(rusqlite::params![hash, ext, kind, size, now])?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
}

/// Godot の `.import` の添えファイル（照らさない形式。歩くときに見て飛ばす分）。
fn import_sidecar(rel: &str) -> String {
    format!(
        "[remap]\n\nimporter=\"texture\"\ntype=\"CompressedTexture2D\"\n\n[deps]\n\nsource_file=\"res://{rel}\"\n"
    )
}

/// 乱数の中身の素材のうち `n` 個を、同じ大きさの別の中身に書き換える（更新時刻も変わる）。書き換えた数を返す。
fn rewrite_some(made: &Made, n: usize) -> Result<usize, String> {
    let random = &made.files[made.matched..];
    if random.is_empty() {
        return Ok(0);
    }
    let step = (random.len() / n.max(1)).max(1);
    let mut rng = ChaCha8Rng::seed_from_u64(now_ms() as u64);
    let mut done = 0;
    for rel in random.iter().step_by(step).take(n) {
        let path = made.root.join(rel);
        let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len() as usize;
        let mut buf = vec![0u8; len];
        rng.fill_bytes(&mut buf);
        std::fs::write(&path, &buf).map_err(|e| e.to_string())?;
        done += 1;
    }
    Ok(done)
}
