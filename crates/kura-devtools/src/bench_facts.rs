//! `bench query --library <path> --fake-facts`：素材の事実のフィールド（透過・ループ・ラウドネスなど。M11）で
//! 検索する速さを測る（NFR-03 の 50ms。docs/search-query.md §3）。
//!
//! - 画像・音声のアイテムに、擬似の事実（`media_facts`）と素材の設定（`asset_settings`）を入れてから測り、終わったら消す。
//!   擬似の行は `media_facts.decoder = 'kura-devtools-fake'`・`asset_settings.updated_at = 1` で見分ける。
//! - 本物の解析の結果・素材の設定があるライブラリでは断る（seed の写しに使う）。
//! - 値はシード付きの ChaCha8 で決める（同じライブラリなら毎回同じ事実）。解析の進み（`analysis_state`）は変えないので、
//!   「未解析 N 件」は seed のまま（画像・音声がすべて pending）の、いちばん重い場合を測る。

use std::time::{Duration, Instant};

use kura_core::db::Db;
use kura_core::items::{ItemQuery, ItemView, count_unanalyzed, query_ids};
use kura_core::query::{QueryContext, check, parse};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};

/// 擬似の事実の印（`media_facts.decoder`）。
const FAKE_DECODER: &str = "kura-devtools-fake";
/// 擬似の素材の設定の印（`asset_settings.updated_at`）。
const FAKE_AT: i64 = 1;
/// 1 トランザクションで入れる件数（.claude/rules/database.md）。
const CHUNK: usize = 500;

/// 擬似の事実を入れる。入れた (画像, 音声, 設定) の件数を返す。
pub fn insert(db: &Db) -> Result<(usize, usize, usize), String> {
    let real = db
        .read(|conn| {
            let facts: i64 = conn.query_row(
                "SELECT count(*) FROM media_facts WHERE decoder IS NOT ?1",
                [FAKE_DECODER],
                |r| r.get(0),
            )?;
            let settings: i64 = conn.query_row(
                "SELECT count(*) FROM asset_settings WHERE updated_at <> ?1",
                [FAKE_AT],
                |r| r.get(0),
            )?;
            Ok(facts + settings)
        })
        .map_err(|e| e.to_string())?;
    if real > 0 {
        return Err(format!(
            "本物の解析の結果か素材の設定が {real} 件あるので --fake-facts は使えません（seed の写しに使ってください）"
        ));
    }
    remove(db)?;
    let rows: Vec<(i64, String, String)> = db
        .read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT b.id, i.kind, i.ext FROM items i JOIN blobs b ON b.hash = i.hash
                 WHERE i.kind IN ('image', 'audio') ORDER BY i.id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
        .map_err(|e| e.to_string())?;
    let mut rng = ChaCha8Rng::seed_from_u64(11);
    let mut facts = Vec::with_capacity(rows.len());
    let (mut images, mut audios) = (0, 0);
    for (blob, kind, ext) in rows {
        let roll = |rng: &mut ChaCha8Rng, n: u32| rng.next_u32() % n;
        if kind == "image" {
            images += 1;
            // 透過：なし 70%・二値 20%・半透明 10%。JPEG・HEIC は形式から（透過なし・色数なし・ドット絵でない）
            let photo = matches!(ext.as_str(), "jpg" | "jpeg" | "heic");
            let alpha = if photo {
                0
            } else {
                match roll(&mut rng, 10) {
                    0..=6 => 0,
                    7..=8 => 1,
                    _ => 2,
                }
            };
            let colors = (!photo).then(|| match roll(&mut rng, 5) {
                0 => 1 + i64::from(roll(&mut rng, 16)),
                _ => 17 + i64::from(roll(&mut rng, 241)),
            });
            let pixel = i64::from(!photo && roll(&mut rng, 10) == 0);
            let frames = if roll(&mut rng, 20) == 0 {
                2 + i64::from(roll(&mut rng, 30))
            } else {
                1
            };
            facts.push(Fact {
                blob,
                alpha: Some(alpha),
                colors,
                pixel: Some(pixel),
                frames: Some(frames),
                ..Fact::default()
            });
        } else {
            audios += 1;
            let lossy = roll(&mut rng, 2) == 0;
            facts.push(Fact {
                blob,
                rate: Some([44_100, 48_000, 96_000][roll(&mut rng, 3) as usize]),
                bits: (!lossy).then(|| [16, 24][roll(&mut rng, 2) as usize]),
                channels: Some(1 + i64::from(roll(&mut rng, 2))),
                // 5% は短くて計れない
                loudness: (roll(&mut rng, 20) != 0)
                    .then(|| -30.0 + f64::from(roll(&mut rng, 24_000)) / 1000.0),
                peak: Some(-12.0 + f64::from(roll(&mut rng, 12_000)) / 1000.0),
                has_loop: Some(i64::from(roll(&mut rng, 20) == 0)),
                ..Fact::default()
            });
        }
    }
    for chunk in facts.chunks(CHUNK) {
        let chunk = chunk.to_vec();
        db.write(move |tx| {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO media_facts (blob_id, state, decoder, facts_v, alpha, colors, pixel_art, pixel_rule,
                                          frame_count, sample_rate, bits, channels, loudness, true_peak, has_loop,
                                          analyzed_at)
                 VALUES (?1, 'ok', ?2, 1, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0)",
            )?;
            for f in &chunk {
                stmt.execute(rusqlite::params![
                    f.blob,
                    FAKE_DECODER,
                    f.alpha,
                    f.colors,
                    f.pixel,
                    f.frames,
                    f.rate,
                    f.bits,
                    f.channels,
                    f.loudness,
                    f.peak,
                    f.has_loop
                ])?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    }
    // 素材の設定：画像の 1% に「ドット絵：はい／いいえ」
    let settings = db
        .write(|tx| {
            Ok(tx.execute(
                "INSERT INTO asset_settings (item_id, pixel_art, updated_at)
                 SELECT id, id % 2, ?1 FROM items WHERE kind = 'image' AND id % 100 = 0",
                [FAKE_AT],
            )?)
        })
        .map_err(|e| e.to_string())?;
    Ok((images, audios, settings))
}

/// 擬似の事実・設定を消す。
pub fn remove(db: &Db) -> Result<(), String> {
    db.write(|tx| {
        tx.execute("DELETE FROM media_facts WHERE decoder = ?1", [FAKE_DECODER])?;
        tx.execute(
            "DELETE FROM asset_settings WHERE updated_at = ?1",
            [FAKE_AT],
        )?;
        Ok(())
    })
    .map_err(|e| e.to_string())
}

/// 1 件分の擬似の事実。
#[derive(Debug, Clone, Default)]
struct Fact {
    blob: i64,
    alpha: Option<i64>,
    colors: Option<i64>,
    pixel: Option<i64>,
    frames: Option<i64>,
    rate: Option<i64>,
    bits: Option<i64>,
    channels: Option<i64>,
    loudness: Option<f64>,
    peak: Option<f64>,
    has_loop: Option<i64>,
}

fn median(mut runs: Vec<Duration>) -> f64 {
    runs.sort();
    runs[runs.len() / 2].as_secs_f64() * 1000.0
}

/// 事実のフィールドで検索する速さ（一覧の ID・パレットの件数と未解析の件数）。どれかが 50ms を超えたら知らせる。
pub fn search(db: &Db, ctx: &QueryContext) -> Result<(), String> {
    let cases = [
        "透過:あり",
        "透過!=あり",
        "-透過:あり",
        "透過:半透明",
        "ドット絵:あり",
        "ドット絵:なし",
        "色数<=16",
        "2のべき乗:あり",
        "アニメ:あり",
        "レート:48k",
        "ビット:24",
        "チャンネル:ステレオ",
        "ラウドネス>-14",
        "ピーク>-1",
        "ループ:あり",
        "種類:画像 透過:あり 色数<=16",
        "種類:音楽 -ループ:あり",
        "(透過:あり | ループ:あり) 評価>=3",
    ];
    let mut slowest = 0f64;
    for text in cases {
        let filter = parse(text).map_err(|e| e.message)?.expr;
        let query = ItemQuery {
            filter: Some(filter),
            ..ItemQuery::default()
        };
        let mut list_runs = Vec::new();
        let mut unanalyzed_runs = Vec::new();
        let mut check_runs = Vec::new();
        let mut matched = 0;
        let mut unanalyzed = None;
        for _ in 0..7 {
            let started = Instant::now();
            matched = db
                .read(|conn| query_ids(conn, &query, ctx))
                .map_err(|e| e.to_string())?
                .ids
                .len();
            list_runs.push(started.elapsed());
            let started = Instant::now();
            unanalyzed = db
                .read(|conn| count_unanalyzed(conn, &query, ctx))
                .map_err(|e| e.to_string())?;
            unanalyzed_runs.push(started.elapsed());
            // パレットの 1 回分（解析・種類ごとの件数・未解析の件数）
            let started = Instant::now();
            db.read(|conn| check(conn, text, ItemView::All, ctx))
                .map_err(|e| e.to_string())?;
            check_runs.push(started.elapsed());
        }
        let (list_ms, unanalyzed_ms, check_ms) = (
            median(list_runs),
            median(unanalyzed_runs),
            median(check_runs),
        );
        slowest = slowest.max(list_ms).max(check_ms);
        tracing::info!(
            query = text,
            matched,
            unanalyzed,
            list_ms,
            unanalyzed_ms,
            palette_ms = check_ms,
            "素材の事実で検索（中央値）"
        );
    }
    if slowest > 50.0 {
        tracing::warn!(slowest_ms = slowest, "50ms（NFR-03）を超えた検索があります");
    } else {
        tracing::info!(slowest_ms = slowest, "素材の事実の検索はすべて 50ms 以内");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_core::library::Library;

    fn count(db: &Db, sql: &str) -> i64 {
        db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }

    #[test]
    fn fake_facts_are_searchable_and_removed_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("bench.kuralib");
        crate::seed::run(&[
            "--items".into(),
            "400".into(),
            "--out".into(),
            out.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let library = Library::open(&out).unwrap();
        let db = library.db();
        let (images, audios, _) = insert(db).unwrap();
        assert!(images > 0 && audios > 0);
        assert_eq!(
            count(db, "SELECT count(*) FROM media_facts") as usize,
            images + audios
        );
        // 同じライブラリなら同じ事実（ChaCha8）
        let first = count(
            db,
            "SELECT sum(coalesce(alpha, 9) * blob_id) FROM media_facts",
        );
        insert(db).unwrap();
        assert_eq!(
            count(
                db,
                "SELECT sum(coalesce(alpha, 9) * blob_id) FROM media_facts"
            ),
            first
        );
        let ctx = QueryContext {
            now_ms: kura_core::time::now_ms(),
            utc_offset_minutes: 540,
        };
        search(db, &ctx).unwrap();
        // 透過ありの画像が見つかり、未解析（seed は pending のまま）も数える
        let query = ItemQuery {
            filter: Some(parse("透過:あり").unwrap().expr),
            ..ItemQuery::default()
        };
        let found = db.read(|c| query_ids(c, &query, &ctx)).unwrap().ids.len();
        assert!(found > 0);
        assert!(
            db.read(|c| count_unanalyzed(c, &query, &ctx))
                .unwrap()
                .is_some_and(|n| n > 0)
        );
        remove(db).unwrap();
        assert_eq!(count(db, "SELECT count(*) FROM media_facts"), 0);
        assert_eq!(count(db, "SELECT count(*) FROM asset_settings"), 0);
    }

    #[test]
    fn refuses_a_library_with_real_facts() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("bench.kuralib");
        crate::seed::run(&[
            "--items".into(),
            "50".into(),
            "--out".into(),
            out.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let library = Library::open(&out).unwrap();
        let db = library.db();
        db.write(|tx| {
            tx.execute(
                "INSERT INTO media_facts (blob_id, state, decoder, facts_v, alpha, analyzed_at)
                 SELECT min(id), 'ok', 'png', 1, 0, 0 FROM blobs",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let err = insert(db).unwrap_err();
        assert!(err.contains("seed"), "{err}");
        // 本物の行は消さない
        assert_eq!(count(db, "SELECT count(*) FROM media_facts"), 1);
    }
}
