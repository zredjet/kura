//! マイグレーションの時間と照合（`bench migrate`。docs/data-model.md §5）。
//!
//! - `--library <path>`：既存のライブラリの DB（`library.sqlite` と `-wal`）を写して開く（seed で作った前の版のライブラリなど）。
//!   元のライブラリには触らない（読むのも写しから）。
//! - `--items <件数> [--history]`：スキーマ 6 の DB を合成する（画像 7 割・動画 2 割・音声 1 割、ゴミ箱 1%、タグ 1 件あたり約 2.5、
//!   フォルダ・ユニット）。`--history` で再生の記録（1 件あたり 2）・プレイリストのエントリ 1 万・取得元 1 万も入れる。
//! - `--runs <回数>`（既定 3）・`--work <dir>`（既定は一時フォルダ。終わったら消す）・`--json <file>`（1 回ごとの結果を JSON の行で足す）。
//!
//! 毎回、写しを `Library::open` で開き（当てる前のバックアップ → マイグレーション → `wal_checkpoint(TRUNCATE)`）、
//! MigrationReport の時間と、DB・WAL の大きさを出す。続けて同じ写しを開き直す時間（更新後の 2 回目）も出す。
//! 最後の写しで、表ごとの行数（写す前と同じ）・`foreign_key_check` が空・FTS の照合（rank=1）・`quick_check`・
//! 台帳と items の 1 対 1 を確かめ、どれかが外れたら失敗にする。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use kura_core::library::Library;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use rusqlite::{Connection, params};

/// 合成する DB のスキーマの版（この版の列で行を入れる）。
const SYNTH_VERSION: usize = 6;
/// 1 トランザクションで入れる件数。
const CHUNK: usize = 5_000;
const TAGS: i64 = 2_000;
const FOLDERS: i64 = 60;
const DAY_MS: i64 = 86_400_000;

/// `bench migrate …`。
pub fn run(args: &[String]) -> Result<(), String> {
    let mut library = None;
    let mut items = None;
    let mut history = false;
    let mut runs = 3usize;
    let mut work = None;
    let mut json = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--library" => library = it.next().map(PathBuf::from),
            "--items" => {
                items = Some(
                    it.next()
                        .and_then(|v| v.parse::<usize>().ok())
                        .ok_or("--items には件数を指定してください")?,
                );
            }
            "--history" => history = true,
            "--runs" => {
                runs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|n| *n > 0)
                    .ok_or("--runs には 1 以上の回数を指定してください")?;
            }
            "--work" => work = it.next().map(PathBuf::from),
            "--json" => json = it.next().map(PathBuf::from),
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    // 既定の作業フォルダは一時フォルダ（終わったら消す）。--work は残す
    let (work, remove_after) = match work {
        Some(dir) => (dir, false),
        None => (
            std::env::temp_dir().join(format!("kura-bench-migrate-{}", std::process::id())),
            true,
        ),
    };
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;

    let source = work.join("source");
    std::fs::create_dir_all(&source).map_err(|e| e.to_string())?;
    let label = match (&library, items) {
        (Some(lib), None) => {
            copy_db(lib, &source)?;
            format!("library {}", lib.display())
        }
        (None, Some(n)) => {
            let started = Instant::now();
            synthesize(&source.join(kura_core::library::DB_FILE), n, history)?;
            tracing::info!(
                items = n,
                history,
                ms = ms(started),
                "スキーマ 6 の DB を合成しました"
            );
            format!("synthetic items={n} history={history}")
        }
        _ => {
            return Err(
                "使い方: kura-devtools bench migrate (--library <path> | --items <件数> [--history]) \
                 [--runs 3] [--work <dir>] [--json <file>]"
                    .into(),
            );
        }
    };

    // 写す前の行数（写しを数える。元は WAL ごと残す）
    let counted = work.join("counted");
    std::fs::create_dir_all(&counted).map_err(|e| e.to_string())?;
    copy_db(&source, &counted)?;
    let before = table_counts(&counted.join(kura_core::library::DB_FILE))?;
    let from = before.version;

    let mut last = None;
    for run in 0..runs {
        let dir = work.join(format!("run-{run}.kuralib"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        copy_db(&source, &dir)?;
        // ページキャッシュを温める（起動の 2 回目以降と同じ条件）
        let _ = std::fs::read(dir.join(kura_core::library::DB_FILE));

        let started = Instant::now();
        let lib = Library::open(&dir).map_err(|e| e.to_string())?;
        let open_ms = ms(started);
        let report = lib.migration().clone();
        let db_mb = mb(&dir.join(kura_core::library::DB_FILE));
        let wal_mb = mb(&dir.join("library.sqlite-wal"));
        let backup_mb = kura_core::library::backup::list(lib.layout())
            .ok()
            .and_then(|l| l.first().map(|b| b.size_bytes as f64 / 1e6))
            .unwrap_or(0.0);
        drop(lib);
        let reopened = Instant::now();
        drop(Library::open(&dir).map_err(|e| e.to_string())?);
        let reopen_ms = ms(reopened);

        let steps: BTreeMap<&str, f64> = report.applied.iter().map(|a| (a.name, a.ms)).collect();
        tracing::info!(
            run,
            from = report.from,
            to = report.to,
            open_ms,
            backup_ms = report.before_ms,
            steps = ?steps,
            checkpoint_ms = report.checkpoint_ms,
            tolerated_violations = report.tolerated_violations,
            db_mb,
            wal_mb,
            backup_mb,
            reopen_ms,
            "マイグレーション（開くところから。バックアップ・各マイグレーション・TRUNCATE）"
        );
        if let Some(path) = &json {
            let line = serde_json::json!({
                "bench": "migrate",
                "source": label,
                "run": run,
                "from": report.from,
                "to": report.to,
                "openMs": open_ms,
                "backupMs": report.before_ms,
                "steps": steps,
                "checkpointMs": report.checkpoint_ms,
                "toleratedViolations": report.tolerated_violations,
                "dbMb": db_mb,
                "walMb": wal_mb,
                "backupMb": backup_mb,
                "reopenMs": reopen_ms,
            });
            append_line(path, &line.to_string())?;
        }
        last = Some(dir);
    }

    let Some(dir) = last else {
        return Err("1 回も開けませんでした".into());
    };
    let verified = verify(&dir.join(kura_core::library::DB_FILE), &before, from);
    if remove_after {
        let _ = std::fs::remove_dir_all(&work);
    }
    verified
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn mb(path: &Path) -> f64 {
    std::fs::metadata(path)
        .map(|m| m.len() as f64 / 1e6)
        .unwrap_or(0.0)
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{line}").map_err(|e| e.to_string())
}

/// ライブラリのフォルダ `from` の DB（`library.sqlite` と、あれば `-wal`）を `to` に写す（-shm は開くときに作り直される）。
fn copy_db(from: &Path, to: &Path) -> Result<(), String> {
    let db = kura_core::library::DB_FILE;
    let main = from.join(db);
    if !main.is_file() {
        return Err(format!("DB がありません: {}", main.display()));
    }
    std::fs::copy(&main, to.join(db)).map_err(|e| e.to_string())?;
    let wal = from.join(format!("{db}-wal"));
    let target_wal = to.join(format!("{db}-wal"));
    if wal.is_file() {
        std::fs::copy(&wal, &target_wal).map_err(|e| e.to_string())?;
    } else {
        let _ = std::fs::remove_file(&target_wal);
    }
    let _ = std::fs::remove_file(to.join(format!("{db}-shm")));
    Ok(())
}

/// 写す前の表ごとの行数と版。
struct Counts {
    version: usize,
    tables: BTreeMap<String, i64>,
}

fn table_counts(path: &Path) -> Result<Counts, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    let version = kura_core::db::applied(&conn).map_err(|e| e.to_string())?;
    let tables = counts_of(&conn)?;
    Ok(Counts { version, tables })
}

fn counts_of(conn: &Connection) -> Result<BTreeMap<String, i64>, String> {
    let names: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                   AND name NOT LIKE 'items_fts_%' ORDER BY name",
            )
            .map_err(|e| e.to_string())?;
        stmt.query_map([], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| e.to_string())?
    };
    let mut out = BTreeMap::new();
    for name in names {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        out.insert(name, n);
    }
    Ok(out)
}

/// 当てたあとの照合。どれかが外れたら失敗にする。
fn verify(path: &Path, before: &Counts, from: usize) -> Result<(), String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| e.to_string())?;
    let version = kura_core::db::applied(&conn).map_err(|e| e.to_string())?;
    let after = counts_of(&conn)?;
    let mut problems = Vec::new();
    for (table, n) in &before.tables {
        match after.get(table) {
            Some(m) if m == n => {}
            other => problems.push(format!("{table}: {n} 行 → {other:?}")),
        }
    }
    let violations = kura_core::db::violations(&conn).map_err(|e| e.to_string())?;
    if !violations.is_empty() {
        problems.push(format!(
            "foreign_key_check: {} 件（例 {:?}）",
            violations.len(),
            violations[0]
        ));
    }
    let fts = conn
        .execute(
            "INSERT INTO items_fts(items_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .map(|_| "ok".to_string())
        .unwrap_or_else(|e| e.to_string());
    if fts != "ok" {
        problems.push(format!("FTS の照合: {fts}"));
    }
    let quick: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if quick != "ok" {
        problems.push(format!("quick_check: {quick}"));
    }
    let ledger: (i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM items), (SELECT count(*) FROM blobs),
                    (SELECT count(*) FROM items i JOIN blobs b
                        ON b.hash = i.hash AND b.ext = i.ext AND b.size_bytes = i.size_bytes)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    if ledger.0 != ledger.1 || ledger.0 != ledger.2 {
        problems.push(format!(
            "台帳と items が 1 対 1 でない（items {}・blobs {}・一致 {}）",
            ledger.0, ledger.1, ledger.2
        ));
    }
    let kinds: String = conn
        .query_row(
            "SELECT group_concat(kind || ':' || analysis_state || '=' || n, ' ')
               FROM (SELECT kind, analysis_state, count(*) n FROM items GROUP BY kind, analysis_state)",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let new_tables: Vec<String> = after
        .iter()
        .filter(|(t, _)| !before.tables.contains_key(*t))
        .map(|(t, n)| format!("{t}={n}"))
        .collect();
    tracing::info!(
        from,
        to = version,
        tables = before.tables.len(),
        rows = before.tables.values().sum::<i64>(),
        new_tables = ?new_tables,
        fts,
        quick_check = quick,
        foreign_key_violations = violations.len(),
        ledger = ?ledger,
        kinds,
        "照合"
    );
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("照合に失敗しました: {}", problems.join(" / ")))
    }
}

/// スキーマ 6 の DB を合成する（アプリの書き込みと同じ PRAGMA。乱数はシード付きの ChaCha8）。
fn synthesize(path: &Path, items: usize, history: bool) -> Result<(), String> {
    let mut conn = Connection::open(path).map_err(|e| e.to_string())?;
    kura_core::db::apply_pragmas(&conn).map_err(|e| e.to_string())?;
    kura_core::db::migrate_until(&mut conn, SYNTH_VERSION).map_err(|e| e.to_string())?;
    let mut rng = ChaCha8Rng::seed_from_u64(0x6d69_6772);
    let now = 1_790_000_000_000i64;
    let e = |e: rusqlite::Error| e.to_string();

    let tx = conn.transaction().map_err(e)?;
    {
        let mut tag = tx
            .prepare("INSERT INTO tags (id, namespace, name, created_at) VALUES (?1, ?2, ?3, ?4)")
            .map_err(e)?;
        for i in 1..=TAGS {
            let ns = ["", "作者", "場所", "イベント"][(i % 4) as usize];
            tag.execute(params![i, ns, format!("タグ{i}"), now])
                .map_err(e)?;
        }
        let mut folder = tx
            .prepare(
                "INSERT INTO folders (id, parent_id, name, pos, created_at) VALUES (?1, ?2, ?3, ?1, ?4)",
            )
            .map_err(e)?;
        for i in 1..=FOLDERS {
            let parent = (i > 10).then_some((i - 1) % 10 + 1);
            folder
                .execute(params![i, parent, format!("フォルダ{i}"), now])
                .map_err(e)?;
        }
    }
    tx.commit().map_err(e)?;

    let mut id = 0i64;
    while (id as usize) < items {
        let tx = conn.transaction().map_err(e)?;
        {
            let mut item = tx
                .prepare_cached(
                    "INSERT INTO items (id, hash, ext, kind, mime, size_bytes, width, height, duration_ms, codec_info,
                                        title, original_name, original_path, source_url, source_site, rating, note,
                                        captured_at, imported_at, updated_at, trashed_at, play_count, last_played_at,
                                        thumb_state, proxy_state)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, 'local', ?13, NULL,
                             ?14, ?15, ?15, ?16, 0, NULL, 'ready', ?17)",
                )
                .map_err(e)?;
            let mut item_tag = tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO item_tags (tag_id, item_id, source) VALUES (?1, ?2, 'user')",
                )
                .map_err(e)?;
            let mut item_folder = tx
                .prepare_cached("INSERT INTO item_folders (folder_id, item_id) VALUES (?1, ?2)")
                .map_err(e)?;
            for _ in 0..CHUNK.min(items - id as usize) {
                id += 1;
                let roll = rng.next_u32() % 10;
                let (kind, ext, size, wh, duration, proxy) = match roll {
                    0..=6 => (
                        "image",
                        ["jpg", "png", "heic"][(rng.next_u32() % 3) as usize],
                        1_000_000 + i64::from(rng.next_u32() % 14_000_000),
                        Some((1600, 1067)),
                        None,
                        "not_needed",
                    ),
                    7 | 8 => (
                        "video",
                        ["mp4", "mkv", "mov"][(rng.next_u32() % 3) as usize],
                        50_000_000 + (rng.next_u64() % 5_000_000_000) as i64,
                        Some((1920, 1080)),
                        Some(10_000 + (rng.next_u64() % 7_200_000) as i64),
                        ["unknown", "not_needed", "ready"][(rng.next_u32() % 3) as usize],
                    ),
                    _ => (
                        "audio",
                        ["flac", "mp3", "ogg"][(rng.next_u32() % 3) as usize],
                        3_000_000 + i64::from(rng.next_u32() % 97_000_000),
                        None,
                        Some(120_000 + (rng.next_u64() % 480_000) as i64),
                        ["unknown", "not_needed"][(rng.next_u32() % 2) as usize],
                    ),
                };
                let imported = now - (rng.next_u64() % (730 * DAY_MS as u64)) as i64;
                let trashed = (rng.next_u32() % 100 == 0).then_some(imported + 1);
                let title = format!("{kind}_{id:06}");
                let hash = blake3::hash(format!("kura-bench-migrate-{id}").as_bytes());
                item.execute(params![
                    id,
                    &hash.as_bytes()[..],
                    ext,
                    kind,
                    size,
                    wh.map(|(w, _)| w),
                    wh.map(|(_, h)| h),
                    duration,
                    (kind == "video").then_some("{\"codec\":\"h264\"}"),
                    title,
                    format!("{title}.{ext}"),
                    format!("/Users/bench/{title}.{ext}"),
                    i64::from(rng.next_u32() % 6),
                    imported - DAY_MS,
                    imported,
                    trashed,
                    proxy,
                ])
                .map_err(e)?;
                // タグは 1 件あたり 0〜5（平均 2.5）
                for _ in 0..(rng.next_u32() % 6) {
                    let t = i64::from(rng.next_u32()) % TAGS + 1;
                    item_tag.execute(params![t, id]).map_err(e)?;
                }
                if rng.next_u32() % 2 == 0 {
                    let f = i64::from(rng.next_u32()) % FOLDERS + 1;
                    item_folder.execute(params![f, id]).map_err(e)?;
                }
            }
        }
        tx.commit().map_err(e)?;
    }

    let tx = conn.transaction().map_err(e)?;
    {
        // 200 件ごとに 10 件の set ユニット（表紙あり）
        let mut unit = tx
            .prepare(
                "INSERT INTO units (id, kind, name, cover_item_id, order_mode, origin, created_at, updated_at)
                 VALUES (?1, 'set', ?2, ?3, 'manual', 'user', ?4, ?4)",
            )
            .map_err(e)?;
        let mut member = tx
            .prepare("INSERT INTO unit_items (unit_id, item_id, pos) VALUES (?1, ?2, ?3)")
            .map_err(e)?;
        for u in 0..(id / 200) {
            let first = u * 200 + 1;
            unit.execute(params![u + 1, format!("ユニット{u}"), first, now])
                .map_err(e)?;
            for k in 0..10 {
                member
                    .execute(params![u + 1, first + k, k + 1])
                    .map_err(e)?;
            }
        }
        tx.execute_batch(
            "INSERT INTO settings (key, value) VALUES ('playback.support', '\"bench\"');
             INSERT INTO tag_aliases (alias, tag_id) VALUES ('べつめい', 1);
             INSERT INTO tag_parents (child_id, parent_id) VALUES (2, 1);
             INSERT INTO unit_tags (tag_id, unit_id) SELECT 1, id FROM units WHERE id % 10 = 1;",
        )
        .map_err(e)?;
        // フォルダのユニット（参照するフォルダ）
        tx.execute(
            "INSERT INTO units (kind, name, order_mode, origin, created_at, updated_at)
             VALUES ('folder', 'フォルダのユニット', 'name', 'user', ?1, ?1)",
            [now],
        )
        .map_err(e)?;
        tx.execute(
            "INSERT INTO unit_folders (unit_id, folder_id, recursive, pos) VALUES (?1, 1, 1, 1)",
            [tx.last_insert_rowid()],
        )
        .map_err(e)?;
        tx.execute(
            "INSERT INTO jobs (kind, state, input_json, created_at) VALUES ('import', 'done', '{}', ?1)",
            [now],
        )
        .map_err(e)?;
    }
    tx.commit().map_err(e)?;

    if history && id > 0 {
        let tx = conn.transaction().map_err(e)?;
        {
            let mut playlist = tx
                .prepare(
                    "INSERT INTO playlists (id, name, kind, pos, created_at, updated_at) VALUES (?1, ?2, 'manual', ?1, ?3, ?3)",
                )
                .map_err(e)?;
            for p in 1..=20i64 {
                playlist
                    .execute(params![p, format!("プレイリスト{p}"), now])
                    .map_err(e)?;
            }
            let mut entry = tx
                .prepare(
                    "INSERT INTO playlist_entries (playlist_id, pos, entry_type, item_id) VALUES (?1, ?2, 'item', ?3)",
                )
                .map_err(e)?;
            for k in 0..10_000i64 {
                let item = i64::from(rng.next_u32()) % id + 1;
                entry.execute(params![k % 20 + 1, k, item]).map_err(e)?;
            }
            tx.execute(
                "INSERT INTO playlist_state (playlist_id, seed, slot_index, position_ms, updated_at, item_id)
                 VALUES (1, 7, 3, 1500, ?1, 1)",
                [now],
            )
            .map_err(e)?;
            let mut remote = tx
                .prepare(
                    "INSERT OR IGNORE INTO remote_sources (site, remote_id, item_id, page_index, fetched_at)
                     VALUES ('pixiv', ?1, ?2, 0, ?3)",
                )
                .map_err(e)?;
            for k in 0..10_000i64 {
                let item = i64::from(rng.next_u32()) % id + 1;
                remote
                    .execute(params![format!("r{k}"), item, now])
                    .map_err(e)?;
            }
            tx.execute(
                "INSERT INTO capture_rules (name, url_pattern, mode, pos) VALUES ('r', 'https://example.com/*', 'viewport', 1)",
                [],
            )
            .map_err(e)?;
            tx.execute(
                "INSERT INTO smart_folders (name, query, pos, created_at) VALUES ('海', '海', 1, ?1)",
                [now],
            )
            .map_err(e)?;
        }
        tx.commit().map_err(e)?;
        // 再生の記録は 1 件あたり 2（50,000 件ずつ）
        let mut written = 0i64;
        while written < id * 2 {
            let tx = conn.transaction().map_err(e)?;
            {
                let mut play = tx
                    .prepare_cached(
                        "INSERT INTO play_events (item_id, playlist_id, played_at, played_ms, completed)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                    )
                    .map_err(e)?;
                for _ in 0..50_000.min(id * 2 - written) {
                    written += 1;
                    let item = (written - 1) / 2 + 1;
                    let playlist = (written % 7 == 0).then_some(written % 20 + 1);
                    play.execute(params![item, playlist, now + written, 8000, written % 2])
                        .map_err(e)?;
                }
            }
            tx.commit().map_err(e)?;
        }
    }
    drop(conn);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_a_small_synthetic_library_and_verifies_it() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let json = dir.path().join("out.jsonl");
        run(&[
            "--items".into(),
            "1200".into(),
            "--history".into(),
            "--runs".into(),
            "2".into(),
            "--work".into(),
            work.to_string_lossy().into_owned(),
            "--json".into(),
            json.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&json)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["from"], 6);
        assert_eq!(lines[0]["to"], kura_core::db::MIGRATIONS.len());
        assert!(
            lines[0]["steps"]
                .as_object()
                .unwrap()
                .contains_key("0007_blobs_and_kinds")
        );
        assert_eq!(lines[0]["walMb"], 0.0);
        // 合成した DB にはすべての表に行がある（作り直しで子が残ることを確かめられる）
        let counts = table_counts(&work.join("counted").join("library.sqlite")).unwrap();
        for (table, n) in &counts.tables {
            assert!(*n > 0, "{table} が空");
        }
    }

    #[test]
    fn refuses_without_a_source() {
        assert!(run(&[]).is_err());
        assert!(run(&["--runs".into(), "0".into(), "--items".into(), "1".into()]).is_err());
    }
}
