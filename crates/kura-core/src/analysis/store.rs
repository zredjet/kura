//! 解析の読み書き（docs/data-model.md §3・§4）。
//!
//! - 取り出し：`analysis_state = 'pending'` を新しい順に（`idx_items_analysis_pending`）。優先のものは ID で引く。
//! - 書き込み（[`write`]）：1 つのトランザクションで、`UPDATE items … WHERE id = ? AND hash = ? AND analysis_state = 'pending'`
//!   が 1 行変えたときだけ結果と事実を書く（読んだあとに中身が変わった・利用者が戻したものに古い結果を書かない）。台帳の行
//!   （`blob_id`）は書くトランザクションの中でハッシュから引く（前に引いた値を使い回さない）。結果の鍵は台帳の行 × 解析 ID
//!   （版と正規の引数は列。defaults 9）。
//! - 計画の指紋（[`sync_plan`]。起動のスレッド）とデコーダの指紋（[`sync_decoders`]。ジョブのスレッド）が変わったら、
//!   影響のあるものだけ pending に戻す。どちらも一度きりの全件の走査（EXPLAIN のテストに例外として書く）。
//! - 処理中の印（[`set_inflight`]・[`take_inflight`]。settings の `analysis.inflight`）：OS のデコーダ（Platform の段）で読む前に
//!   付け、書いたら消す。次に動いたときに残っていれば、そのアイテムで落ちたとみなして failed（`crashed`）にする（critic 8）。

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::facts::{FACTS_V, MediaFacts, PIXEL_RULE};
use super::plan::{
    DECODERS_KEY, DecoderFingerprint, FROM_FORMAT, PLAN_KEY, PlanFingerprint, canonical_string,
};
use crate::Result;
use crate::import::MediaKind;
use crate::settings;

/// 処理中の印を置く settings のキー。
pub const INFLIGHT_KEY: &str = "analysis.inflight";

/// 解析を待つアイテム。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: i64,
    pub hash: [u8; 32],
    pub ext: String,
    /// 種類（DB の CHECK が外れていて知らない値なら None。解析せずに failed にする）。
    pub kind: Option<MediaKind>,
}

fn pending_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Pending>> {
    let hash: Vec<u8> = row.get(1)?;
    let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) else {
        return Ok(None);
    };
    Ok(Some(Pending {
        id: row.get(0)?,
        hash,
        ext: row.get(2)?,
        kind: MediaKind::parse(&row.get::<_, String>(3)?),
    }))
}

/// 解析を待つものを新しい順に `limit` 件（`idx_items_analysis_pending`）。
pub fn pending_batch(conn: &Connection, limit: usize) -> Result<Vec<Pending>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, hash, ext, kind FROM items
         WHERE analysis_state = 'pending' ORDER BY imported_at DESC, id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], pending_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.extend(row?);
    }
    Ok(out)
}

/// `ids` のうち、まだ解析を待っているもの（`ids` の順）。
pub fn pending_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<Pending>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, hash, ext, kind FROM items WHERE id = ?1 AND analysis_state = 'pending'",
    )?;
    let mut out = Vec::new();
    for &id in ids {
        if let Some(p) = stmt.query_row([id], pending_from_row).optional()?.flatten() {
            out.push(p);
        }
    }
    Ok(out)
}

/// 解析を待つものの数。
pub fn count_pending(conn: &Connection) -> Result<u64> {
    let mut stmt =
        conn.prepare_cached("SELECT count(*) FROM items WHERE analysis_state = 'pending'")?;
    let n: i64 = stmt.query_row([], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// 結果の表の 1 行（正本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResult {
    pub analysis: String,
    pub version: u16,
    /// 正規の引数（正規の JSON の文字列）。
    pub params: String,
    /// `json` か `binary`。
    pub encoding: String,
    /// 正規の形。ファイルに置くもの（波形の段）は None。
    pub output: Option<Vec<u8>>,
    pub output_len: i64,
}

/// 中身（ハッシュ）の結果の行（解析 ID の順）。
pub fn stored_results(conn: &Connection, hash: &[u8; 32]) -> Result<Vec<StoredResult>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.analysis, r.version, r.params, r.encoding, r.output, r.output_len
           FROM blobs b JOIN analysis_results r ON r.blob_id = b.id
          WHERE b.hash = ?1 ORDER BY r.analysis",
    )?;
    let rows = stmt.query_map([hash.as_slice()], |r| {
        Ok(StoredResult {
            analysis: r.get(0)?,
            version: r.get(1)?,
            params: r.get(2)?,
            encoding: r.get(3)?,
            output: r.get(4)?,
            output_len: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 書く結果 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewResult {
    pub analysis: String,
    pub version: u16,
    /// 正規の引数（正規の JSON の文字列）。
    pub params: String,
    /// `json` か `binary`。
    pub encoding: &'static str,
    /// 正規の形。ファイルに置くもの（波形の段）は None。
    pub output: Option<Vec<u8>>,
    pub output_len: u64,
    /// `kura-analysis-v1`。
    pub digest: [u8; 32],
}

/// 読み取りの記録（`media_facts` の decoder・reproducible・value_digest・env）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadStamp {
    pub decoder: Option<String>,
    pub reproducible: Option<bool>,
    pub value_digest: Option<[u8; 32]>,
    /// 環境（正規の JSON）。
    pub env: Option<String>,
}

/// 1 件の書き込みの中身。
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    /// 解析した（か、結果の表から読まずに作った）。`results` を書き、`keep` があれば、そこにない解析の行を消す。
    /// `stamp` が None なら読み取りの記録の列は前のまま（読まずに作ったとき）。
    Ready {
        results: Vec<NewResult>,
        keep: Option<Vec<String>>,
        facts: Box<MediaFacts>,
        stamp: Option<ReadStamp>,
    },
    /// 解析できなかった（`unsupported`・`too_large`・`decode`・`crashed`）。指紋が変わるか利用者が頼むまで解き直さない。
    Failed {
        code: &'static str,
        stamp: Option<ReadStamp>,
    },
    /// 解析しない種類だった（not_needed にする）。
    NotNeeded,
}

/// アイテム 1 件の書き込み（読んだときのハッシュを条件にする）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemWrite {
    pub id: i64,
    pub hash: [u8; 32],
    pub write: Write,
}

/// 書いた数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Written {
    /// 状態を変えて書いたもの。
    pub written: u64,
    /// 中身が変わった・待っていなかったので書かなかったもの。
    pub skipped: u64,
}

fn bool_int(b: Option<bool>) -> Option<i64> {
    b.map(i64::from)
}

/// 結果と事実を 1 つのトランザクションの中で書く（呼び手が `Db::write` の中で呼ぶ）。
pub fn write(tx: &Connection, items: &[ItemWrite], now: i64) -> Result<Written> {
    let mut state = tx.prepare_cached(
        "UPDATE items SET analysis_state = ?3
          WHERE id = ?1 AND hash = ?2 AND analysis_state = 'pending'",
    )?;
    let mut blob = tx.prepare_cached("SELECT id FROM blobs WHERE hash = ?1")?;
    let mut out = Written::default();
    for item in items {
        let to = match &item.write {
            Write::Ready { .. } => "ready",
            Write::Failed { .. } => "failed",
            Write::NotNeeded => "not_needed",
        };
        if state.execute(params![item.id, item.hash.as_slice(), to])? == 0 {
            out.skipped += 1;
            continue;
        }
        out.written += 1;
        let blob_id: Option<i64> = blob
            .query_row([item.hash.as_slice()], |r| r.get(0))
            .optional()?;
        let Some(blob_id) = blob_id else {
            continue;
        };
        match &item.write {
            Write::Ready {
                results,
                keep,
                facts,
                stamp,
            } => {
                for r in results {
                    upsert_result(tx, blob_id, r, now)?;
                }
                if let Some(keep) = keep {
                    let mut stmt = tx.prepare_cached(
                        "DELETE FROM analysis_results
                          WHERE blob_id = ?1 AND analysis NOT IN (SELECT value FROM json_each(?2))",
                    )?;
                    stmt.execute(params![blob_id, serde_json::to_string(keep)?])?;
                }
                upsert_facts(tx, blob_id, "ok", None, facts, stamp.as_ref(), now)?;
            }
            Write::Failed { code, stamp } => {
                let empty = MediaFacts {
                    facts_v: FACTS_V,
                    ..MediaFacts::default()
                };
                upsert_facts(
                    tx,
                    blob_id,
                    "failed",
                    Some(code),
                    &empty,
                    stamp.as_ref(),
                    now,
                )?;
            }
            Write::NotNeeded => {}
        }
    }
    Ok(out)
}

fn upsert_result(tx: &Connection, blob_id: i64, r: &NewResult, now: i64) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO analysis_results
           (blob_id, analysis, version, params, encoding, output, output_len, digest, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (blob_id, analysis) DO UPDATE SET
           version = excluded.version, params = excluded.params, encoding = excluded.encoding,
           output = excluded.output, output_len = excluded.output_len, digest = excluded.digest,
           created_at = excluded.created_at",
    )?;
    stmt.execute(params![
        blob_id,
        r.analysis,
        r.version,
        r.params,
        r.encoding,
        r.output,
        i64::try_from(r.output_len).unwrap_or(i64::MAX),
        r.digest.as_slice(),
        now
    ])?;
    Ok(())
}

/// 事実の行を書く（`stamp` が None なら読み取りの記録の列は前のまま）。
fn upsert_facts(
    tx: &Connection,
    blob_id: i64,
    state: &str,
    error_code: Option<&str>,
    f: &MediaFacts,
    stamp: Option<&ReadStamp>,
    now: i64,
) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO media_facts
           (blob_id, state, error_code, decoder, reproducible, value_digest, env, facts_v,
            alpha, colors, visible_colors, pixel_kx, pixel_ky, pixel_lossless, pixel_art, pixel_rule,
            frame_count, loudness, loudness_reason, loudness_range, true_peak, sample_peak,
            has_loop, loop_start, loop_end, sample_rate, channels, bits, clip_runs, analyzed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30)
         ON CONFLICT (blob_id) DO UPDATE SET
           state = excluded.state, error_code = excluded.error_code,
           decoder = CASE WHEN ?31 THEN excluded.decoder ELSE decoder END,
           reproducible = CASE WHEN ?31 THEN excluded.reproducible ELSE reproducible END,
           value_digest = CASE WHEN ?31 THEN excluded.value_digest ELSE value_digest END,
           env = CASE WHEN ?31 THEN excluded.env ELSE env END,
           facts_v = excluded.facts_v, alpha = excluded.alpha, colors = excluded.colors,
           visible_colors = excluded.visible_colors, pixel_kx = excluded.pixel_kx,
           pixel_ky = excluded.pixel_ky, pixel_lossless = excluded.pixel_lossless,
           pixel_art = excluded.pixel_art, pixel_rule = excluded.pixel_rule,
           frame_count = excluded.frame_count, loudness = excluded.loudness,
           loudness_reason = excluded.loudness_reason, loudness_range = excluded.loudness_range,
           true_peak = excluded.true_peak, sample_peak = excluded.sample_peak,
           has_loop = excluded.has_loop, loop_start = excluded.loop_start,
           loop_end = excluded.loop_end, sample_rate = excluded.sample_rate,
           channels = excluded.channels, bits = excluded.bits, clip_runs = excluded.clip_runs,
           analyzed_at = excluded.analyzed_at",
    )?;
    let none = ReadStamp::default();
    let s = stamp.unwrap_or(&none);
    stmt.execute(params![
        blob_id,
        state,
        error_code,
        s.decoder,
        bool_int(s.reproducible),
        s.value_digest.as_ref().map(|d| d.as_slice()),
        s.env,
        f.facts_v,
        f.alpha,
        f.colors,
        f.visible_colors,
        f.pixel_kx,
        f.pixel_ky,
        bool_int(f.pixel_lossless),
        bool_int(f.pixel_art),
        f.pixel_rule,
        f.frame_count,
        f.loudness,
        f.loudness_reason,
        f.loudness_range,
        f.true_peak,
        f.sample_peak,
        bool_int(f.has_loop),
        f.loop_start,
        f.loop_end,
        f.sample_rate,
        f.channels,
        f.bits,
        f.clip_runs,
        now,
        stamp.is_some(),
    ])?;
    Ok(())
}

/// 投影の古い事実の行（読み直さずに作り直すもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleFacts {
    pub blob_id: i64,
    pub ext: String,
    /// JSON の結果（ID → 正規の形。読み取りの記録を含む）。
    pub results: BTreeMap<String, Vec<u8>>,
}

/// 投影の版かドット絵の規則が古い事実の行を、台帳の行の順に `after` の次から `limit` 件。
pub fn stale_facts(conn: &Connection, after: i64, limit: usize) -> Result<Vec<StaleFacts>> {
    let mut stmt = conn.prepare_cached(
        "SELECT f.blob_id, b.ext FROM media_facts f JOIN blobs b ON b.id = f.blob_id
          WHERE f.blob_id > ?1 AND f.state = 'ok' AND (f.facts_v <> ?2 OR f.pixel_rule <> ?3)
          ORDER BY f.blob_id LIMIT ?4",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map(
            params![
                after,
                FACTS_V,
                PIXEL_RULE,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
        .collect::<std::result::Result<_, _>>()?;
    let mut outputs = conn.prepare_cached(
        "SELECT analysis, output FROM analysis_results
          WHERE blob_id = ?1 AND encoding = 'json' AND output IS NOT NULL",
    )?;
    let mut out = Vec::with_capacity(rows.len());
    for (blob_id, ext) in rows {
        let results = outputs
            .query_map([blob_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        out.push(StaleFacts {
            blob_id,
            ext,
            results,
        });
    }
    Ok(out)
}

/// 投影の古い事実の行があるか。
pub fn any_stale_facts(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM media_facts
                         WHERE state = 'ok' AND (facts_v <> ?1 OR pixel_rule <> ?2))",
    )?;
    Ok(stmt.query_row(params![FACTS_V, PIXEL_RULE], |r| r.get(0))?)
}

/// 作り直した事実を書く（読み取りの記録の列と状態は前のまま）。
pub fn write_projection(tx: &Connection, blob_id: i64, f: &MediaFacts, now: i64) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "UPDATE media_facts SET
           facts_v = ?2, alpha = ?3, colors = ?4, visible_colors = ?5, pixel_kx = ?6, pixel_ky = ?7,
           pixel_lossless = ?8, pixel_art = ?9, pixel_rule = ?10, frame_count = ?11, loudness = ?12,
           loudness_reason = ?13, loudness_range = ?14, true_peak = ?15, sample_peak = ?16,
           has_loop = ?17, loop_start = ?18, loop_end = ?19, sample_rate = ?20, channels = ?21,
           bits = ?22, clip_runs = ?23, analyzed_at = ?24
         WHERE blob_id = ?1",
    )?;
    stmt.execute(params![
        blob_id,
        f.facts_v,
        f.alpha,
        f.colors,
        f.visible_colors,
        f.pixel_kx,
        f.pixel_ky,
        bool_int(f.pixel_lossless),
        bool_int(f.pixel_art),
        f.pixel_rule,
        f.frame_count,
        f.loudness,
        f.loudness_reason,
        f.loudness_range,
        f.true_peak,
        f.sample_peak,
        bool_int(f.has_loop),
        f.loop_start,
        f.loop_end,
        f.sample_rate,
        f.channels,
        f.bits,
        f.clip_runs,
        now,
    ])?;
    Ok(())
}

/// 計画の指紋を比べた結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanSync {
    /// pending に戻した数。
    pub reset: usize,
    /// 解析のジョブを積むか（待っているもの・投影の古い行がある）。
    pub todo: bool,
}

/// 起動のスレッドで、整合性チェックのあとに呼ぶ：前に置いた計画の指紋と比べ、解析の版・計画が変わった種類の ready・failed を
/// pending に戻し、今の指紋を置く。投影だけの変更は戻さない（ジョブが結果の表から作り直す）。
///
/// 種類で戻す UPDATE は items の全件の走査になる（指紋が変わったときだけの一度きり。10 万件で約 70 ms。
/// テスト `sync_plan_reset_is_a_one_time_full_scan` に例外として書く）。
pub fn sync_plan(tx: &Connection, current: &PlanFingerprint) -> Result<PlanSync> {
    let stored: Option<PlanFingerprint> = settings::get(tx, PLAN_KEY)?;
    let mut reset = 0;
    for kind in current.kinds_to_reset(stored.as_ref()) {
        let mut stmt = tx.prepare_cached(
            "UPDATE items SET analysis_state = 'pending'
              WHERE kind = ?1 AND analysis_state IN ('ready', 'failed')",
        )?;
        reset += stmt.execute([kind.as_str()])?;
    }
    if stored.as_ref() != Some(current) {
        put_canonical(tx, PLAN_KEY, &current.to_canonical()?)?;
        tracing::info!(
            reset,
            "解析の計画が変わったので、影響のあるものを解き直します"
        );
    }
    let mut pending =
        tx.prepare_cached("SELECT EXISTS (SELECT 1 FROM items WHERE analysis_state = 'pending')")?;
    let any_pending: bool = pending.query_row([], |r| r.get(0))?;
    Ok(PlanSync {
        reset,
        todo: any_pending || any_stale_facts(tx)?,
    })
}

/// 解析のジョブがコーデックの組を作ったときに呼ぶ：前に置いたデコーダの指紋と比べ、今の組にないデコーダで読んだものと、
/// 並びが変わった形式で失敗していたものを pending に戻し、今の指紋を置く。最初（前の指紋がない）は戻さない。
///
/// 戻す UPDATE は事実の表の全件の走査になる（デコーダが変わったときだけの一度きり。`sync_plan` と同じ例外）。
pub fn sync_decoders(tx: &Connection, current: &DecoderFingerprint) -> Result<usize> {
    let stored: Option<DecoderFingerprint> = settings::get(tx, DECODERS_KEY)?;
    let mut reset = 0;
    if let Some(stored) = &stored {
        let changed = current.changed_formats(stored);
        if !changed.is_empty() {
            let exts: Vec<&str> = changed
                .iter()
                .filter_map(|f| kura_engine::codec::formats::format_by_id(f))
                .flat_map(|f| f.exts.iter().copied())
                .collect();
            let mut stmt = tx.prepare_cached(
                "UPDATE items SET analysis_state = 'pending'
                  WHERE analysis_state IN ('ready', 'failed') AND hash IN (
                    SELECT b.hash FROM media_facts f JOIN blobs b ON b.id = f.blob_id
                     WHERE (f.decoder IS NOT NULL
                            AND f.decoder NOT IN (SELECT value FROM json_each(?1)))
                        OR (f.state = 'failed' AND b.ext IN (SELECT value FROM json_each(?2))))",
            )?;
            reset = stmt.execute(params![
                serde_json::to_string(&current.all_ids())?,
                serde_json::to_string(&exts)?
            ])?;
            tracing::info!(
                reset,
                ?changed,
                "デコーダが変わったので、影響のあるものを解き直します"
            );
        }
    }
    if stored.as_ref() != Some(current) {
        put_canonical(
            tx,
            DECODERS_KEY,
            &canonical_string(&serde_json::to_value(current)?)?,
        )?;
    }
    Ok(reset)
}

fn put_canonical(tx: &Connection, key: &str, json: &str) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?;
    stmt.execute(params![key, json])?;
    Ok(())
}

/// 処理中の印（OS のデコーダで読んでいるアイテム）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inflight {
    pub id: i64,
    /// 中身のハッシュ（16 進）。
    pub hash: String,
}

/// 処理中の印を付ける（読む前。この書き込みをコミットしてから読む）。
pub fn set_inflight(tx: &Connection, mark: &Inflight) -> Result<()> {
    settings::set(tx, INFLIGHT_KEY, mark)
}

/// 処理中の印を消す（書いたあと）。
pub fn clear_inflight(tx: &Connection) -> Result<()> {
    let mut stmt = tx.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
    stmt.execute([INFLIGHT_KEY])?;
    Ok(())
}

/// 残っていた処理中の印を取り出して消す（前の実行がその読み取りの途中で落ちた）。
pub fn take_inflight(tx: &Connection) -> Result<Option<Inflight>> {
    let mark: Option<Inflight> = settings::get(tx, INFLIGHT_KEY)?;
    clear_inflight(tx)?;
    Ok(mark)
}

/// 失敗したものを解析待ちに戻す（利用者の「もう一度解析」）。戻した数。
pub fn retry_failed(tx: &Connection, ids: &[i64]) -> Result<usize> {
    let mut stmt = tx.prepare_cached(
        "UPDATE items SET analysis_state = 'pending' WHERE id = ?1 AND analysis_state = 'failed'",
    )?;
    let mut n = 0;
    for &id in ids {
        n += stmt.execute([id])?;
    }
    Ok(n)
}

/// `ids` のどれかが解析を待っているか。
pub fn any_pending(conn: &Connection, ids: &[i64]) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM items WHERE id = ?1 AND analysis_state = 'pending')",
    )?;
    for &id in ids {
        if stmt.query_row([id], |r| r.get::<_, bool>(0))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 形式から事実を書く形式か（JPEG・HEIC・HEIF）。
pub fn is_from_format(format: &str) -> bool {
    FROM_FORMAT.contains(&format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};
    use kura_engine::analysis::registry;

    fn add(conn: &Connection, id: i64, kind: &str, ext: &str, hash: u8) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at,
                                analysis_state)
             VALUES (?1, ?2, ?3, ?4, 10, 't', 't', ?1, ?1,
                     CASE WHEN ?4 IN ('image', 'audio') THEN 'pending' ELSE 'not_needed' END)",
            params![id, vec![hash; 32], ext, kind],
        )
        .unwrap();
    }

    fn state(conn: &Connection, id: i64) -> String {
        conn.query_row(
            "SELECT analysis_state FROM items WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn ready(id: i64, hash: u8, results: Vec<NewResult>) -> ItemWrite {
        ItemWrite {
            id,
            hash: [hash; 32],
            write: Write::Ready {
                results,
                keep: None,
                facts: Box::new(MediaFacts {
                    facts_v: FACTS_V,
                    alpha: Some(1),
                    ..MediaFacts::default()
                }),
                stamp: Some(ReadStamp {
                    decoder: Some("png-rs-dec/2".into()),
                    reproducible: Some(true),
                    value_digest: Some([7; 32]),
                    env: None,
                }),
            },
        }
    }

    fn result(id: &str, version: u16) -> NewResult {
        NewResult {
            analysis: id.into(),
            version,
            params: "{}".into(),
            encoding: "json",
            output: Some(b"{}".to_vec()),
            output_len: 2,
            digest: [1; 32],
        }
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// 書き込みはハッシュと pending を条件にする：読んだあとに中身が変わったら（M12 の保存の形）書かない。
    #[test]
    fn analysis_write_is_guarded_by_hash_and_state() {
        let conn = test_conn();
        add(&conn, 1, "image", "png", 1);
        add(&conn, 2, "image", "png", 2);
        // 読んだあとに中身が変わった（ハッシュ・拡張子・大きさを 1 つの UPDATE で変え、pending に戻す）
        conn.execute(
            "UPDATE items SET hash = ?1, size_bytes = 11, analysis_state = 'pending' WHERE id = 1",
            [vec![9u8; 32]],
        )
        .unwrap();
        let w = write(
            &conn,
            &[
                ready(1, 1, vec![result("image.alpha", 1)]),
                ready(2, 2, vec![result("image.alpha", 1)]),
            ],
            5,
        )
        .unwrap();
        assert_eq!(
            w,
            Written {
                written: 1,
                skipped: 1
            }
        );
        assert_eq!(
            state(&conn, 1),
            "pending",
            "古い中身の結果を新しい中身に書かない"
        );
        assert_eq!(state(&conn, 2), "ready");
        assert_eq!(count(&conn, "SELECT count(*) FROM analysis_results"), 1);
        assert_eq!(count(&conn, "SELECT count(*) FROM media_facts"), 1);
        let owner: Vec<u8> = conn
            .query_row(
                "SELECT b.hash FROM media_facts f JOIN blobs b ON b.id = f.blob_id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            owner,
            vec![2u8; 32],
            "台帳の行は書くトランザクションの中でハッシュから引く"
        );

        // 待っていないもの（利用者が戻した・ほかの書き手が書いた）にも書かない
        let again = write(&conn, &[ready(2, 2, vec![result("image.colors", 1)])], 6).unwrap();
        assert_eq!(
            again,
            Written {
                written: 0,
                skipped: 1
            }
        );
        assert_eq!(count(&conn, "SELECT count(*) FROM analysis_results"), 1);
    }

    /// 結果は台帳の行 × 解析 ID で上書きし、計画にない行は消す。失敗は事実の行に理由を書く。読まずに作ったときは
    /// 読み取りの記録の列を変えない。
    #[test]
    fn results_and_facts_are_upserted_per_blob() {
        let conn = test_conn();
        add(&conn, 1, "audio", "wav", 1);
        write(
            &conn,
            &[ready(
                1,
                1,
                vec![result("audio.loudness", 1), result("audio.peaks", 1)],
            )],
            5,
        )
        .unwrap();
        conn.execute(
            "UPDATE items SET analysis_state = 'pending' WHERE id = 1",
            [],
        )
        .unwrap();
        let mut w = ready(1, 1, vec![result("audio.loudness", 2)]);
        if let Write::Ready { keep, stamp, .. } = &mut w.write {
            *keep = Some(vec!["audio.loudness".into()]);
            *stamp = None;
        }
        write(&conn, &[w], 6).unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM analysis_results"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT version FROM analysis_results WHERE analysis = 'audio.loudness'"
            ),
            2
        );
        let (decoder, at): (Option<String>, i64) = conn
            .query_row("SELECT decoder, analyzed_at FROM media_facts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((decoder.as_deref(), at), (Some("png-rs-dec/2"), 6));

        add(&conn, 2, "image", "png", 2);
        write(
            &conn,
            &[ItemWrite {
                id: 2,
                hash: [2; 32],
                write: Write::Failed {
                    code: "too_large",
                    stamp: None,
                },
            }],
            7,
        )
        .unwrap();
        assert_eq!(state(&conn, 2), "failed");
        let (st, code, alpha): (String, String, Option<i64>) = conn
            .query_row(
                "SELECT f.state, f.error_code, f.alpha FROM media_facts f JOIN blobs b ON b.id = f.blob_id
                  WHERE b.hash = ?1",
                [vec![2u8; 32]],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (st.as_str(), code.as_str(), alpha),
            ("failed", "too_large", None)
        );

        // 解析しない種類が pending で入っていれば not_needed にする（事実は書かない）
        add(&conn, 3, "video", "mp4", 3);
        conn.execute(
            "UPDATE items SET analysis_state = 'pending' WHERE id = 3",
            [],
        )
        .unwrap();
        write(
            &conn,
            &[ItemWrite {
                id: 3,
                hash: [3; 32],
                write: Write::NotNeeded,
            }],
            8,
        )
        .unwrap();
        assert_eq!(state(&conn, 3), "not_needed");
        assert_eq!(count(&conn, "SELECT count(*) FROM media_facts"), 2);
    }

    /// 計画の版が上がった種類だけ戻し、デコーダが変わったらそのデコーダで読んだものと、その形式で失敗していたものだけ戻す。
    #[test]
    fn plan_changes_reset_only_what_they_affect() {
        let conn = test_conn();
        add(&conn, 1, "image", "png", 1);
        add(&conn, 2, "audio", "ogg", 2);
        add(&conn, 3, "audio", "wav", 3);
        add(&conn, 4, "image", "bmp", 4);
        add(&conn, 5, "video", "mp4", 5);
        add(&conn, 6, "image", "png", 6);
        let registry = registry().unwrap();
        let current = PlanFingerprint::current(&registry).unwrap();
        // 最初の起動：指紋を置くだけ（0007 で pending にしたものを解く）
        let first = sync_plan(&conn, &current).unwrap();
        assert_eq!(
            first,
            PlanSync {
                reset: 0,
                todo: true
            }
        );
        let stamp = |decoder: &str| ReadStamp {
            decoder: Some(decoder.into()),
            ..ReadStamp::default()
        };
        let mut done = vec![
            ready(1, 1, vec![]),
            ready(2, 2, vec![]),
            ready(3, 3, vec![]),
            ready(6, 6, vec![]),
        ];
        for (w, d) in done.iter_mut().zip([
            "png-rs-dec/2",
            "ogg-vorbis-symphonia-dec/3",
            "wav-rs-dec/1",
            "png-rs-dec/2",
        ]) {
            if let Write::Ready { stamp: s, .. } = &mut w.write {
                *s = Some(stamp(d));
            }
        }
        done.push(ItemWrite {
            id: 4,
            hash: [4; 32],
            write: Write::Failed {
                code: "unsupported",
                stamp: None,
            },
        });
        write(&conn, &done, 1).unwrap();
        conn.execute(
            "UPDATE items SET analysis_state = 'failed' WHERE id = 6",
            [],
        )
        .unwrap();
        let states = |conn: &Connection| -> String {
            (1..=6)
                .map(|id| format!("{id}:{}", state(conn, id)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            states(&conn),
            "1:ready 2:ready 3:ready 4:failed 5:not_needed 6:failed"
        );
        assert_eq!(
            sync_plan(&conn, &current).unwrap(),
            PlanSync {
                reset: 0,
                todo: false
            }
        );

        // (b) 音声の解析の版が上がった：音声だけ pending
        let mut older = current.clone();
        older
            .kinds
            .get_mut("audio")
            .unwrap()
            .get_mut("audio.loudness")
            .unwrap()
            .0 = 0;
        put_canonical(&conn, PLAN_KEY, &older.to_canonical().unwrap()).unwrap();
        let synced = sync_plan(&conn, &current).unwrap();
        assert_eq!(
            synced,
            PlanSync {
                reset: 2,
                todo: true
            }
        );
        assert_eq!(
            states(&conn),
            "1:ready 2:pending 3:pending 4:failed 5:not_needed 6:failed"
        );
        let stored: PlanFingerprint = settings::get(&conn, PLAN_KEY).unwrap().unwrap();
        assert_eq!(stored, current);
        conn.execute(
            "UPDATE items SET analysis_state = 'ready' WHERE id IN (2, 3)",
            [],
        )
        .unwrap();

        // (c) デコーダ：最初は置くだけ。ogg のデコーダが入れ替わり、bmp に読めるものが入った
        let mut decoders = DecoderFingerprint::default();
        decoders.0.insert("png".into(), vec!["png-rs-dec/2".into()]);
        decoders
            .0
            .insert("ogg".into(), vec!["ogg-vorbis-symphonia-dec/3".into()]);
        decoders.0.insert("wav".into(), vec!["wav-rs-dec/1".into()]);
        decoders.0.insert("bmp".into(), vec![]);
        assert_eq!(sync_decoders(&conn, &decoders).unwrap(), 0);
        let mut now = decoders.clone();
        now.0
            .insert("ogg".into(), vec!["ogg-vorbis-symphonia-dec/4".into()]);
        now.0.insert("bmp".into(), vec!["imageio-dec/1".into()]);
        assert_eq!(sync_decoders(&conn, &now).unwrap(), 2);
        assert_eq!(
            states(&conn),
            "1:ready 2:pending 3:ready 4:pending 5:not_needed 6:failed",
            "png で失敗していた 6 は、png のデコーダが同じなので戻さない"
        );
        let stored: DecoderFingerprint = settings::get(&conn, DECODERS_KEY).unwrap().unwrap();
        assert_eq!(stored, now);
        assert_eq!(sync_decoders(&conn, &now).unwrap(), 0);
    }

    /// 取り出しと数えるのは部分索引を使う。結果は台帳の UNIQUE と結果の UNIQUE で引く。
    #[test]
    fn analysis_queries_use_their_indexes() {
        let conn = test_conn();
        let plan = query_plan(
            &conn,
            "SELECT id, hash, ext, kind FROM items
              WHERE analysis_state = 'pending' ORDER BY imported_at DESC, id DESC LIMIT 16",
        );
        assert!(
            plan.contains("idx_items_analysis_pending") && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );
        let plan = query_plan(
            &conn,
            "SELECT count(*) FROM items WHERE analysis_state = 'pending'",
        );
        assert!(plan.contains("idx_items_analysis_pending"), "{plan}");
        let plan = query_plan(
            &conn,
            "SELECT r.analysis FROM blobs b JOIN analysis_results r ON r.blob_id = b.id
              WHERE b.hash = x'00' ORDER BY r.analysis",
        );
        assert!(!plan.contains("SCAN"), "{plan}");
        let plan = query_plan(
            &conn,
            "SELECT f.blob_id, b.ext FROM media_facts f JOIN blobs b ON b.id = f.blob_id
              WHERE f.blob_id > 0 AND f.state = 'ok' AND (f.facts_v <> 1 OR f.pixel_rule <> 1)
              ORDER BY f.blob_id LIMIT 500",
        );
        assert!(
            plan.contains("USING INTEGER PRIMARY KEY") && !plan.contains("SCAN b"),
            "{plan}"
        );
        let plan = query_plan(
            &conn,
            "SELECT id, hash, ext, kind FROM items WHERE id = 1 AND analysis_state = 'pending'",
        );
        assert!(plan.contains("USING INTEGER PRIMARY KEY"), "{plan}");
    }

    /// 例外：指紋が変わったときに種類やデコーダで pending に戻す UPDATE は、items（と事実の表）の全件の走査になる。
    /// 指紋が変わったときだけの一度きり（起動のスレッド・ジョブのスレッド。10 万件で約 70 ms）なので許す
    /// （.claude/rules/database.md）。
    #[test]
    fn sync_plan_reset_is_a_one_time_full_scan() {
        let conn = test_conn();
        let plan = query_plan(
            &conn,
            "UPDATE items SET analysis_state = 'pending'
              WHERE kind = 'audio' AND analysis_state IN ('ready', 'failed')",
        );
        assert!(plan.contains("SCAN items"), "{plan}");
        let plan = query_plan(
            &conn,
            "UPDATE items SET analysis_state = 'pending'
              WHERE analysis_state IN ('ready', 'failed') AND hash IN (
                SELECT b.hash FROM media_facts f JOIN blobs b ON b.id = f.blob_id
                 WHERE (f.decoder IS NOT NULL AND f.decoder NOT IN (SELECT value FROM json_each('[]')))
                    OR (f.state = 'failed' AND b.ext IN (SELECT value FROM json_each('[]'))))",
        );
        assert!(plan.contains("SCAN"), "{plan}");
    }

    /// 処理中の印は置いて取り出すと消える。失敗を戻すのは failed だけ。
    #[test]
    fn inflight_marks_and_retry() {
        let conn = test_conn();
        assert_eq!(take_inflight(&conn).unwrap(), None);
        let mark = Inflight {
            id: 3,
            hash: "ab".into(),
        };
        set_inflight(&conn, &mark).unwrap();
        assert_eq!(take_inflight(&conn).unwrap(), Some(mark));
        assert_eq!(take_inflight(&conn).unwrap(), None);

        add(&conn, 1, "image", "png", 1);
        add(&conn, 2, "image", "png", 2);
        conn.execute(
            "UPDATE items SET analysis_state = 'failed' WHERE id = 1",
            [],
        )
        .unwrap();
        conn.execute("UPDATE items SET analysis_state = 'ready' WHERE id = 2", [])
            .unwrap();
        assert!(!any_pending(&conn, &[1, 2]).unwrap());
        assert_eq!(retry_failed(&conn, &[1, 2, 99]).unwrap(), 1);
        assert_eq!(state(&conn, 1), "pending");
        assert_eq!(state(&conn, 2), "ready");
        assert!(any_pending(&conn, &[2, 1]).unwrap());
        assert_eq!(
            pending_by_ids(&conn, &[2, 1])
                .unwrap()
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            [1]
        );
    }
}
