//! ダミーライブラリの生成（`seed --items <件数> --out <path>`）。グリッドと起動時間の計測に使う。
//!
//! - 種類は画像 70%・動画 20%・音楽 10%。縦横比は 8 種類、追加日は約 2 年に散らす。
//! - 実ファイルとサムネイルは、少数のひな形 JPEG を APFS クローンで複製する（容量はほとんど増えない）。
//! - タグ（2,000 個、1 件あたり 0〜5 個）と仮想フォルダ（60 個）も付ける（M4 の検索の計測用）。
//! - 200 件ごとに 10 件の set ユニットを作る（M3 の「ユニットをまとめて表示」の計測用）。
//! - 乱数はシード付きの ChaCha8 なので、同じ件数なら毎回同じライブラリになる。

use std::path::{Path, PathBuf};
use std::time::Instant;

use kura_core::library::{Library, ThumbSize, hex};
use kura_core::time::now_ms;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use rayon::prelude::*;

/// ひな形の縦横（長辺 1600 のときの大きさ）。
const SHAPES: [(u32, u32); 8] = [
    (1600, 1067),
    (1067, 1600),
    (1600, 900),
    (900, 1600),
    (1600, 1600),
    (1600, 1200),
    (1200, 1600),
    (1131, 1600),
];

/// ひな形の色（地・丸・下の帯）。Main.dc.html のタイルと同じ配色。
const PALETTES: [([u8; 3], [u8; 3], [u8; 3]); 6] = [
    ([0x2C, 0x4A, 0x63], [0xF2, 0xA6, 0x5A], [0x1B, 0x32, 0x47]),
    ([0x24, 0x1E, 0x33], [0x8E, 0x6B, 0xFF], [0x17, 0x12, 0x1F]),
    ([0x3A, 0x31, 0x50], [0xFF, 0xC7, 0x7A], [0x27, 0x20, 0x38]),
    ([0x1E, 0x3A, 0x36], [0x7E, 0xD8, 0xC3], [0x1E, 0x3A, 0x36]),
    ([0x3B, 0x46, 0x52], [0xE8, 0xD9, 0xA8], [0x2A, 0x32, 0x3B]),
    ([0x4A, 0x3A, 0x2E], [0xF5, 0xC0, 0x8A], [0x33, 0x27, 0x1F]),
];

const TAGS: usize = 2000;
const NAMESPACES: [&str; 6] = ["", "作者", "場所", "イベント", "投稿者", "アーティスト"];
const DAY_MS: i64 = 86_400_000;
/// ユニットを作る間隔と大きさ（200 件ごとに 10 件のユニット）。
const UNIT_EVERY: usize = 200;
const UNIT_SIZE: usize = 10;
/// 1 トランザクションで入れる件数（.claude/rules/database.md：大量挿入は 500 件程度ずつ）。
const CHUNK: usize = 500;

/// ひな形 1 つ（s と l の JPEG）。
struct Template {
    width: u32,
    height: u32,
    small: PathBuf,
    large: PathBuf,
}

/// 1 件のダミーアイテム。
struct Seeded {
    hash: [u8; 32],
    kind: &'static str,
    ext: &'static str,
    template: usize,
    title: String,
    size: i64,
    duration_ms: Option<i64>,
    imported_at: i64,
    captured_at: i64,
    rating: i64,
    source_site: &'static str,
    folder: Option<i64>,
    tags: Vec<i64>,
}

/// `--items` 件のダミーライブラリを `--out` に作る。
pub fn run(args: &[String]) -> Result<(), String> {
    let mut items = 100_000usize;
    let mut out = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--items" => {
                items = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--items には件数を指定してください")?;
            }
            "--out" => out = it.next().map(PathBuf::from),
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    let out = out.ok_or("--out にライブラリの場所を指定してください")?;
    let started = Instant::now();

    let library = Library::create(&out).map_err(|e| e.to_string())?;
    let layout = library.layout().clone();
    let template_dir = layout.staging_dir().join("seed-templates");
    let templates = make_templates(&template_dir)?;
    tracing::info!(templates = templates.len(), "ひな形を作りました");

    let mut rng = ChaCha8Rng::seed_from_u64(0x6b75_7261);
    let now = now_ms();
    let folders = seed_folders(&library, now)?;
    seed_tags(&library, now)?;
    let seeded = plan_items(&mut rng, items, &templates, &folders, now);
    tracing::info!(items = seeded.len(), "アイテムを決めました");

    // 実ファイル・サムネイル・ジャケットをクローンで置く（APFS のクローンは並列にしすぎると遅いので 6 並列）
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(6)
        .build()
        .map_err(|e| e.to_string())?;
    pool.install(|| {
        seeded.par_iter().try_for_each(|item| {
            let t = &templates[item.template];
            let hash_hex = hex(&item.hash);
            clone(&t.large, &layout.content_path(&item.hash, item.ext))?;
            clone(&t.small, &layout.thumb_path(&hash_hex, ThumbSize::Small))?;
            clone(&t.large, &layout.thumb_path(&hash_hex, ThumbSize::Large))?;
            if item.kind == "audio" {
                clone(&t.large, &layout.art_path(&hash_hex))?;
            }
            Ok::<(), String>(())
        })
    })?;
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "ファイルを置きました"
    );

    insert_items(&library, &templates, seeded)?;
    seed_units(&library, items, now)?;
    std::fs::remove_dir_all(&template_dir).map_err(|e| e.to_string())?;
    tracing::info!(
        items,
        elapsed_ms = started.elapsed().as_millis() as u64,
        path = %layout.root().display(),
        "ダミーライブラリを作りました"
    );
    Ok(())
}

fn clone(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    reflink_copy::reflink_or_copy(src, dst)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", dst.display()))
}

/// 地・丸・下の帯の JPEG を書く。
fn write_jpeg(path: &Path, w: u32, h: u32, palette: usize) -> Result<(), String> {
    let (bg, fg, band) = PALETTES[palette % PALETTES.len()];
    let (cx, cy) = (w as f32 * 0.62, h as f32 * 0.38);
    let r = w.min(h) as f32 * 0.18;
    let band_top = h as f32 * 0.68;
    let mut buf = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            let color = if dx * dx + dy * dy <= r * r {
                fg
            } else if (y as f32) >= band_top {
                band
            } else {
                bg
            };
            buf.extend_from_slice(&color);
        }
    }
    let w16 = u16::try_from(w).map_err(|e| e.to_string())?;
    let h16 = u16::try_from(h).map_err(|e| e.to_string())?;
    jpeg_encoder::Encoder::new_file(path, 82)
        .map_err(|e| e.to_string())?
        .encode(&buf, w16, h16, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| e.to_string())
}

fn make_templates(dir: &Path) -> Result<Vec<Template>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut specs = Vec::new();
    for (si, &(w, h)) in SHAPES.iter().enumerate() {
        for palette in 0..PALETTES.len() {
            specs.push((si, w, h, palette));
        }
    }
    specs
        .into_par_iter()
        .map(|(si, w, h, palette)| {
            let scale = 512.0 / w.max(h) as f32;
            let (sw, sh) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
            let small = dir.join(format!("t{si}-{palette}_s.jpg"));
            let large = dir.join(format!("t{si}-{palette}_l.jpg"));
            write_jpeg(&small, sw, sh, palette)?;
            write_jpeg(&large, w, h, palette)?;
            Ok(Template {
                width: w,
                height: h,
                small,
                large,
            })
        })
        .collect()
}

fn seed_folders(library: &Library, now: i64) -> Result<Vec<i64>, String> {
    library
        .db()
        .write(move |tx| {
            let mut ids = Vec::new();
            let mut stmt = tx.prepare_cached(
                "INSERT INTO folders (parent_id, name, pos, created_at) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for top in 0..10i64 {
                stmt.execute(rusqlite_params(
                    None,
                    &format!("フォルダ{top}"),
                    top as f64,
                    now,
                ))?;
                let parent = tx.last_insert_rowid();
                ids.push(parent);
                for child in 0..5i64 {
                    stmt.execute(rusqlite_params(
                        Some(parent),
                        &format!("サブ{top}-{child}"),
                        child as f64,
                        now,
                    ))?;
                    ids.push(tx.last_insert_rowid());
                }
            }
            Ok(ids)
        })
        .map_err(|e| e.to_string())
}

fn rusqlite_params(
    parent: Option<i64>,
    name: &str,
    pos: f64,
    now: i64,
) -> (Option<i64>, String, f64, i64) {
    (parent, name.to_string(), pos, now)
}

fn seed_tags(library: &Library, now: i64) -> Result<(), String> {
    library
        .db()
        .write(move |tx| {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO tags (id, namespace, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for i in 0..TAGS {
                let ns = NAMESPACES[i % NAMESPACES.len()];
                stmt.execute((i as i64 + 1, ns, format!("タグ{i}"), now))?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
}

/// 200 件ごとに、続く 10 件を set ユニットにまとめる（「ユニットをまとめて表示」の計測用）。
fn seed_units(library: &Library, items: usize, now: i64) -> Result<(), String> {
    let units = items / UNIT_EVERY;
    library
        .db()
        .write(move |tx| {
            let mut unit_stmt = tx.prepare_cached(
                "INSERT INTO units (kind, name, cover_item_id, order_mode, origin, created_at, updated_at)
                 VALUES ('set', ?1, ?2, 'manual', 'user', ?3, ?3)",
            )?;
            let mut member_stmt = tx
                .prepare_cached("INSERT INTO unit_items (unit_id, item_id, pos) VALUES (?1, ?2, ?3)")?;
            for u in 0..units {
                let first = (u * UNIT_EVERY + 1) as i64;
                unit_stmt.execute((format!("ユニット{u}"), first, now))?;
                let unit = tx.last_insert_rowid();
                for k in 0..UNIT_SIZE as i64 {
                    member_stmt.execute((unit, first + k, (k + 1) as f64))?;
                }
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
}

/// 0..n を偏りのある分布で選ぶ（小さい番号ほど選ばれやすい。人気のタグを再現する）。
fn skewed(rng: &mut ChaCha8Rng, n: usize) -> usize {
    let u = (rng.next_u32() as f64) / (u32::MAX as f64);
    ((u * u * u) * n as f64) as usize % n
}

fn plan_items(
    rng: &mut ChaCha8Rng,
    count: usize,
    templates: &[Template],
    folders: &[i64],
    now: i64,
) -> Vec<Seeded> {
    let span = 730 * DAY_MS;
    let mut times: Vec<i64> = (0..count)
        .map(|_| now - (rng.next_u64() % span as u64) as i64)
        .collect();
    times.sort_unstable();
    let per_shape = PALETTES.len();
    times
        .into_iter()
        .enumerate()
        .map(|(i, imported_at)| {
            let roll = rng.next_u32() % 10;
            let palette = (rng.next_u32() as usize) % per_shape;
            let (kind, ext, shape, size, duration_ms) = match roll {
                0..=6 => {
                    let ext = ["jpg", "heic", "png"][(rng.next_u32() % 3) as usize];
                    let shape = (rng.next_u32() as usize) % SHAPES.len();
                    (
                        "image",
                        ext,
                        shape,
                        1_000_000 + (rng.next_u32() % 14_000_000) as i64,
                        None,
                    )
                }
                7 | 8 => {
                    let ext = ["mp4", "mov", "mkv"][(rng.next_u32() % 3) as usize];
                    let shape = if rng.next_u32().is_multiple_of(5) {
                        3
                    } else {
                        2
                    };
                    let duration = 10_000 + (rng.next_u64() % 7_200_000) as i64;
                    (
                        "video",
                        ext,
                        shape,
                        50_000_000 + (rng.next_u64() % 5_000_000_000) as i64,
                        Some(duration),
                    )
                }
                _ => {
                    let ext = ["flac", "mp3", "m4a"][(rng.next_u32() % 3) as usize];
                    let duration = 120_000 + (rng.next_u64() % 480_000) as i64;
                    (
                        "audio",
                        ext,
                        4,
                        3_000_000 + (rng.next_u32() % 97_000_000) as i64,
                        Some(duration),
                    )
                }
            };
            let template = shape * per_shape + palette;
            debug_assert!(template < templates.len());
            let prefix = match kind {
                "image" => "IMG",
                "video" => "VID",
                _ => "track",
            };
            let source_site = match rng.next_u32() % 100 {
                0..=4 => "pixiv",
                5..=9 => "youtube",
                10..=11 => "capture",
                _ => "local",
            };
            let tag_count = (rng.next_u32() % 6) as usize;
            let mut tags: Vec<i64> = (0..tag_count)
                .map(|_| skewed(rng, TAGS) as i64 + 1)
                .collect();
            tags.sort_unstable();
            tags.dedup();
            let folder = rng
                .next_u32()
                .is_multiple_of(2)
                .then(|| folders[(rng.next_u32() as usize) % folders.len()]);
            Seeded {
                hash: *blake3::hash(format!("kura-seed-{i}").as_bytes()).as_bytes(),
                kind,
                ext,
                template,
                title: format!("{prefix}_{i:06}"),
                size,
                duration_ms,
                imported_at,
                captured_at: imported_at - (rng.next_u64() % (365 * DAY_MS) as u64) as i64,
                rating: [0, 0, 0, 1, 2, 3, 3, 4, 4, 5][(rng.next_u32() % 10) as usize],
                source_site,
                folder,
                tags,
            }
        })
        .collect()
}

fn insert_items(
    library: &Library,
    templates: &[Template],
    seeded: Vec<Seeded>,
) -> Result<(), String> {
    let dims: Vec<(u32, u32)> = templates.iter().map(|t| (t.width, t.height)).collect();
    let mut seeded = seeded;
    while !seeded.is_empty() {
        let rest = seeded.split_off(seeded.len().min(CHUNK));
        let chunk = std::mem::replace(&mut seeded, rest);
        let dims = dims.clone();
        library
            .db()
            .write(move |tx| {
                let mut item_stmt = tx.prepare_cached(
                    "INSERT INTO items (hash, ext, kind, mime, size_bytes, width, height, duration_ms,
                                        title, original_name, original_path, source_site, rating,
                                        captured_at, imported_at, updated_at, thumb_state, proxy_state)
                     VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, 'ready', 'not_needed')",
                )?;
                let mut tag_stmt =
                    tx.prepare_cached("INSERT OR IGNORE INTO item_tags (tag_id, item_id, source) VALUES (?1, ?2, 'user')")?;
                let mut folder_stmt =
                    tx.prepare_cached("INSERT OR IGNORE INTO item_folders (folder_id, item_id) VALUES (?1, ?2)")?;
                for item in chunk {
                    let (w, h) = dims[item.template];
                    let (width, height) = if item.kind == "audio" {
                        (None, None)
                    } else {
                        (Some(i64::from(w)), Some(i64::from(h)))
                    };
                    let name = format!("{}.{}", item.title, item.ext);
                    item_stmt.execute(rusqlite::params![
                        &item.hash[..],
                        item.ext,
                        item.kind,
                        item.size,
                        width,
                        height,
                        item.duration_ms,
                        item.title,
                        name,
                        format!("/Users/seed/{name}"),
                        item.source_site,
                        item.rating,
                        item.captured_at,
                        item.imported_at,
                    ])?;
                    let id = tx.last_insert_rowid();
                    for tag in item.tags {
                        tag_stmt.execute((tag, id))?;
                    }
                    if let Some(folder) = item.folder {
                        folder_stmt.execute((folder, id))?;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: usize, out: &Path) -> Vec<String> {
        vec![
            "--items".into(),
            items.to_string(),
            "--out".into(),
            out.to_string_lossy().into_owned(),
        ]
    }

    #[test]
    fn seeds_items_with_files_thumbnails_tags_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("bench.kuralib");
        run(&args(1200, &out)).unwrap();

        let library = Library::open(&out).unwrap();
        let (items, pending, tagged, filed, tags, folders) = library
            .db()
            .read(|c| {
                let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
                Ok((
                    count("SELECT count(*) FROM items")?,
                    count("SELECT count(*) FROM items WHERE thumb_state <> 'ready'")?,
                    count("SELECT count(DISTINCT item_id) FROM item_tags")?,
                    count("SELECT count(*) FROM item_folders")?,
                    count("SELECT count(*) FROM tags")?,
                    count("SELECT count(*) FROM folders")?,
                ))
            })
            .unwrap();
        assert_eq!(items, 1200);
        assert_eq!(pending, 0);
        assert!(tagged > 600, "tagged {tagged}");
        assert!(filed > 400, "filed {filed}");
        assert_eq!(tags, TAGS as i64);
        assert!(folders > 0);
        let (units, members) = library
            .db()
            .read(|c| {
                Ok((
                    c.query_row("SELECT count(*) FROM units", [], |r| r.get::<_, i64>(0))?,
                    c.query_row("SELECT count(*) FROM unit_items", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                ))
            })
            .unwrap();
        assert_eq!((units, members), (6, 60));

        // 実ファイル・サムネイル・ジャケットが置かれ、ひな形は消えている
        let layout = library.layout();
        let rows: Vec<(Vec<u8>, String, String)> = library
            .db()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hash, ext, kind FROM items")?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok(rows)
            })
            .unwrap();
        for (hash, ext, kind) in &rows {
            let hash: [u8; 32] = hash.as_slice().try_into().unwrap();
            let hash_hex = hex(&hash);
            assert!(layout.content_path(&hash, ext).exists());
            assert!(layout.thumb_path(&hash_hex, ThumbSize::Small).exists());
            assert!(layout.thumb_path(&hash_hex, ThumbSize::Large).exists());
            assert_eq!(layout.art_path(&hash_hex).exists(), kind == "audio");
        }
        assert!(!layout.staging_dir().join("seed-templates").exists());
    }

    #[test]
    fn same_seed_plans_the_same_library() {
        let templates: Vec<Template> = (0..SHAPES.len() * PALETTES.len())
            .map(|_| Template {
                width: 1,
                height: 1,
                small: PathBuf::new(),
                large: PathBuf::new(),
            })
            .collect();
        let plan = || {
            let mut rng = ChaCha8Rng::seed_from_u64(7);
            plan_items(&mut rng, 3000, &templates, &[1, 2, 3], 1_800_000_000_000)
        };
        let key = |s: &Seeded| {
            (
                s.title.clone(),
                s.kind,
                s.ext,
                s.template,
                s.imported_at,
                s.tags.clone(),
                s.folder,
            )
        };
        let (a, b) = (plan(), plan());
        assert!(a.iter().map(key).eq(b.iter().map(key)));

        // 追加日は古い順に並び、種類はおよそ画像 7・動画 2・音楽 1
        assert!(a.windows(2).all(|w| w[0].imported_at <= w[1].imported_at));
        let share = |kind: &str| a.iter().filter(|s| s.kind == kind).count() as f64 / 3000.0;
        assert!((share("image") - 0.7).abs() < 0.05);
        assert!((share("video") - 0.2).abs() < 0.05);
        assert!((share("audio") - 0.1).abs() < 0.05);
        assert!(a.iter().all(|s| s.template < templates.len()));
    }
}
