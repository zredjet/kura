//! 版の行（`item_versions`・`item_version_inputs`。docs/data-model.md §3・§4「保存と版（M12）」、docs/editing.md §14）。
//!
//! - 版 0（元）は最初の保存のときに足す（10 万件のライブラリに行を足さない）。版の行がないアイテムは「版 0 は暗に今、頭の版は
//!   0」と読む（[`head`]・[`original`]）。
//! - 頭（いちばん新しい版）の出力はアイテムの今（`items.hash`・`ext`）に等しい。版 0 と頭は `hold`、頭でなくなった版は
//!   `waiting`（段 3 の GC が消す）か、出力が新しい頭か版 0 と同じなら、すぐ `released`（ファイルを持たない）。
//! - 書く口（[`insert_original`]・[`supersede_head`]・[`insert_version`]・[`insert_inputs`]）は保存のジョブ（`edit::save`）の
//!   トランザクションの中からだけ呼ぶ。不変条件は [`check_invariants`] で照らす（テストと整合性の確かめ）。

use kura_engine::codec::{DecodeStamp, EncodedStamp};
use kura_engine::{Digest, Engine, Graph};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::store;
use crate::import::{MediaKind, format_id};
use crate::{CoreError, Result};

/// 版の種類（`item_versions.kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionKind {
    /// 版 0（元）。
    Original,
    /// 文書を描いて保存した版。
    Edit,
    /// 前の版に戻した版（段 2 の C）。
    Revert,
}

impl VersionKind {
    /// DB に書く名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Edit => "edit",
            Self::Revert => "revert",
        }
    }

    /// DB の名前から読む（知らない名前は None）。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "original" => Self::Original,
            "edit" => Self::Edit,
            "revert" => Self::Revert,
            _ => return None,
        })
    }
}

/// 版のファイルの状態（`item_versions.file_state`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// 今か元（消さない）。
    Hold,
    /// 頭でなくなった（7 日と、置き換えより前のバックアップの条件で段 3 の GC が消す）。
    Waiting,
    /// GC の照合が合わず残す（段 3）。
    Kept,
    /// この行はもうファイルを持たない。
    Released,
}

impl FileState {
    /// DB に書く名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::Waiting => "waiting",
            Self::Kept => "kept",
            Self::Released => "released",
        }
    }

    /// DB の名前から読む（知らない名前は None）。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "hold" => Self::Hold,
            "waiting" => Self::Waiting,
            "kept" => Self::Kept,
            "released" => Self::Released,
            _ => return None,
        })
    }
}

/// 頭でなくなる版のファイルの状態：出力が新しい頭（`new`）か版 0（`original`）と同じなら、その行がファイルを持つので
/// すぐ released、ほかは waiting（docs/editing.md §14）。
pub fn supersede_state(old: &[u8; 32], new: &[u8; 32], original: &[u8; 32]) -> FileState {
    if old == new || old == original {
        FileState::Released
    } else {
        FileState::Waiting
    }
}

/// 版のメディア情報の記録（版 0 はアイテムの mime・codec_info を写し、元に戻すときにここから items を戻す）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_info: Option<String>,
}

/// 元に戻した版の記録（段 2 の C）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevertRecord {
    /// 戻した先の版。
    pub to: i64,
    /// 猶予中のファイルを使い直した。
    pub reused: bool,
    /// 文書から作り直した。
    pub rebuilt: bool,
}

/// 版の記録（`item_versions.record` の JSON）。知らない項目は読み捨てる（新しい Kura が足した項目）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionRecord {
    /// 記録の形の版（1）。
    pub v: u32,
    /// 保存したエンジンの版（`kura_engine::VERSION`）。
    pub engine: String,
    /// 書き出しの記録（描いて書いた版）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoded: Option<EncodedStamp>,
    /// 元を読んだ記録（版 0。描いた版の入力ごとの記録は `item_version_inputs.recorded`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoded: Option<DecodeStamp>,
    /// メディア情報（mime・codec_info）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaRecord>,
    /// 描いたときの警告の文。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// 元に戻した版の記録。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert: Option<RevertRecord>,
}

impl VersionRecord {
    /// 今のエンジンの版で、中身の空の記録。
    pub fn new() -> Self {
        Self {
            v: 1,
            engine: kura_engine::VERSION.to_string(),
            encoded: None,
            decoded: None,
            media: None,
            warnings: Vec::new(),
            revert: None,
        }
    }
}

impl Default for VersionRecord {
    fn default() -> Self {
        Self::new()
    }
}

/// アイテムの頭（今と、頭の版）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// 今の中身（`items.hash`）。
    pub hash: [u8; 32],
    /// 今の拡張子。
    pub ext: String,
    /// アイテムの種類（保存で変わらない）。
    pub kind: MediaKind,
    /// 頭の版（版の行がなければ 0）。
    pub version: i64,
    /// ゴミ箱にあるか。
    pub trashed: bool,
}

/// アイテムの元（版 0。版の行がなければアイテムの今）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginalFile {
    pub hash: [u8; 32],
    pub ext: String,
    pub size_bytes: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    /// mime・codec_info（版 0 の記録か、アイテムの列）。
    pub media: MediaRecord,
    /// 版 0 の行があるか（なければ最初の保存で足す）。
    pub recorded: bool,
}

/// 版の行。
#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub id: i64,
    pub item_id: i64,
    pub version: i64,
    pub kind: VersionKind,
    /// 文書の根の部品の行（版 0 と、版 0 に戻した版は None）。
    pub doc_id: Option<i64>,
    pub base_version: Option<i64>,
    pub revert_to: Option<i64>,
    pub output_hash: [u8; 32],
    pub ext: String,
    pub size_bytes: i64,
    pub format: Option<String>,
    pub value_digest: Option<Digest>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    /// 版の記録の JSON（[`Version::record`] で読む）。
    pub record: String,
    pub file_blob_id: Option<i64>,
    pub file_state: FileState,
    pub replaced_at: Option<i64>,
    pub job_id: Option<i64>,
    pub created_at: i64,
}

impl Version {
    /// 版の記録を読む（壊れていれば Invalid。知らない項目は読み捨てる）。
    pub fn record(&self) -> Result<VersionRecord> {
        serde_json::from_str(&self.record).map_err(|e| {
            CoreError::Invalid(format!(
                "版 {}（アイテム {}）の記録を読めません: {e}",
                self.version, self.item_id
            ))
        })
    }
}

/// 版の入力の役割（`item_version_inputs.role`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRole {
    Primary,
    Fixed,
    Selection,
}

impl InputRole {
    /// DB に書く名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Fixed => "fixed",
            Self::Selection => "selection",
        }
    }

    /// DB の名前から読む（知らない名前は None）。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "primary" => Self::Primary,
            "fixed" => Self::Fixed,
            "selection" => Self::Selection,
            _ => return None,
        })
    }
}

/// 版の入力の行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInput {
    pub input: i64,
    pub role: InputRole,
    pub source_hash: [u8; 32],
    pub value_digest: Digest,
    /// `RecordedInput` の正規の JSON。
    pub recorded: String,
    pub anchor_id: Option<i64>,
}

fn hash32(v: Vec<u8>, what: &str) -> rusqlite::Result<[u8; 32]> {
    <[u8; 32]>::try_from(v.as_slice()).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Blob,
            format!("{what} が 32 バイトではありません").into(),
        )
    })
}

fn bad_enum(what: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("{what} の値 {value} を知りません").into(),
    )
}

/// アイテムの頭を読む（アイテムがなければ None）。頭の版は版の行の最大（`UNIQUE(item_id, version)` の索引で引く）。
pub fn head(conn: &Connection, item_id: i64) -> Result<Option<Head>> {
    let mut stmt = conn.prepare_cached(
        "SELECT i.hash, i.ext, i.kind, i.trashed_at IS NOT NULL,
                (SELECT coalesce(max(v.version), 0) FROM item_versions v WHERE v.item_id = i.id)
           FROM items i WHERE i.id = ?1",
    )?;
    Ok(stmt
        .query_row([item_id], |row| {
            Ok(Head {
                hash: hash32(row.get(0)?, "items.hash")?,
                ext: row.get(1)?,
                kind: row.get(2)?,
                trashed: row.get(3)?,
                version: row.get(4)?,
            })
        })
        .optional()?)
}

/// アイテムの元を読む（版 0 の行があればそれ、なければアイテムの今。アイテムがなければ None）。
pub fn original(conn: &Connection, item_id: i64) -> Result<Option<OriginalFile>> {
    if let Some(v0) = get(conn, item_id, 0)? {
        let media = v0.record()?.media.unwrap_or_default();
        return Ok(Some(OriginalFile {
            hash: v0.output_hash,
            ext: v0.ext,
            size_bytes: v0.size_bytes,
            width: v0.width,
            height: v0.height,
            duration_ms: v0.duration_ms,
            media,
            recorded: true,
        }));
    }
    let mut stmt = conn.prepare_cached(
        "SELECT hash, ext, size_bytes, width, height, duration_ms, mime, codec_info FROM items WHERE id = ?1",
    )?;
    Ok(stmt
        .query_row([item_id], |row| {
            Ok(OriginalFile {
                hash: hash32(row.get(0)?, "items.hash")?,
                ext: row.get(1)?,
                size_bytes: row.get(2)?,
                width: row.get(3)?,
                height: row.get(4)?,
                duration_ms: row.get(5)?,
                media: MediaRecord {
                    mime: row.get(6)?,
                    codec_info: row.get(7)?,
                },
                recorded: false,
            })
        })
        .optional()?)
}

const VERSION_COLUMNS: &str = "id, item_id, version, kind, doc_id, base_version, revert_to, output_hash, ext, \
     size_bytes, format, value_digest, width, height, duration_ms, record, file_blob_id, file_state, \
     replaced_at, job_id, created_at";

fn version_from_row(row: &Row<'_>) -> rusqlite::Result<Version> {
    let kind: String = row.get(3)?;
    let state: String = row.get(17)?;
    let digest: Option<Vec<u8>> = row.get(11)?;
    Ok(Version {
        id: row.get(0)?,
        item_id: row.get(1)?,
        version: row.get(2)?,
        kind: VersionKind::parse(&kind).ok_or_else(|| bad_enum("item_versions.kind", &kind))?,
        doc_id: row.get(4)?,
        base_version: row.get(5)?,
        revert_to: row.get(6)?,
        output_hash: hash32(row.get(7)?, "item_versions.output_hash")?,
        ext: row.get(8)?,
        size_bytes: row.get(9)?,
        format: row.get(10)?,
        value_digest: digest
            .map(|d| hash32(d, "item_versions.value_digest").map(Digest))
            .transpose()?,
        width: row.get(12)?,
        height: row.get(13)?,
        duration_ms: row.get(14)?,
        record: row.get(15)?,
        file_blob_id: row.get(16)?,
        file_state: FileState::parse(&state)
            .ok_or_else(|| bad_enum("item_versions.file_state", &state))?,
        replaced_at: row.get(18)?,
        job_id: row.get(19)?,
        created_at: row.get(20)?,
    })
}

/// アイテムの版の行を版の順に読む（版の行がなければ空）。
pub fn list(conn: &Connection, item_id: i64) -> Result<Vec<Version>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {VERSION_COLUMNS} FROM item_versions WHERE item_id = ?1 ORDER BY version"
    ))?;
    let rows = stmt.query_map([item_id], version_from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// アイテムの版 `version` の行（なければ None）。
pub fn get(conn: &Connection, item_id: i64, version: i64) -> Result<Option<Version>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {VERSION_COLUMNS} FROM item_versions WHERE item_id = ?1 AND version = ?2"
    ))?;
    Ok(stmt
        .query_row(params![item_id, version], version_from_row)
        .optional()?)
}

/// 保存のジョブ `job_id` が頭にした版の行（なければ None。再開で版を 2 つ足さないために、ジョブの初めに引く）。
pub fn by_job(conn: &Connection, job_id: i64) -> Result<Option<Version>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {VERSION_COLUMNS} FROM item_versions WHERE job_id = ?1"
    ))?;
    Ok(stmt.query_row([job_id], version_from_row).optional()?)
}

/// 版の入力の行（入力の順）。
pub fn inputs(conn: &Connection, version_id: i64) -> Result<Vec<VersionInput>> {
    let mut stmt = conn.prepare_cached(
        "SELECT input, role, source_hash, value_digest, recorded, anchor_id
           FROM item_version_inputs WHERE version_id = ?1 ORDER BY input",
    )?;
    let rows = stmt.query_map([version_id], |row| {
        let role: String = row.get(1)?;
        Ok(VersionInput {
            input: row.get(0)?,
            role: InputRole::parse(&role)
                .ok_or_else(|| bad_enum("item_version_inputs.role", &role))?,
            source_hash: hash32(row.get(2)?, "item_version_inputs.source_hash")?,
            value_digest: Digest(hash32(row.get(3)?, "item_version_inputs.value_digest")?),
            recorded: row.get(4)?,
            anchor_id: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 版 `version` の文書（版 0 と、版 0 に戻した版は空の文書）。版の行がなく `version` が 0 なら空の文書、ほかは NotFound。
pub fn doc_of(conn: &Connection, engine: &Engine, item_id: i64, version: i64) -> Result<Graph> {
    match get(conn, item_id, version)? {
        Some(Version {
            doc_id: Some(doc_id),
            ..
        }) => store::load_doc(conn, engine, doc_id),
        Some(_) => Ok(Graph::new_single()),
        None if version == 0 => Ok(Graph::new_single()),
        None => Err(CoreError::NotFound(format!(
            "アイテム {item_id} の版 {version}"
        ))),
    }
}

/// 版・入力・アンカー・下書きの不変条件を照らし、破れているものを文で返す（空なら正しい。docs/data-model.md §4
/// 「保存と版（M12）」の不変条件）：
/// - 版の行は 0 個か 2 個以上で 0..n に欠けなく並ぶ。版 0 と頭は hold、ほかの版は hold でない。頭の output_hash・ext は
///   items.hash・ext に等しい。released でない行の file_blob_id が指す台帳の行の hash は output_hash と同じ。
/// - 版 0 でない版の土台は 1 つ前の版（`base_version = version - 1`）。文書を持つ版の文書は根（doc）の部品。
/// - 描いた版（edit）は入力 0（Primary）の行を持つ。
/// - 戻した版（revert）は戻した先の版がそのアイテムにあり（`revert_to < version`）、文書・値のダイジェスト・入力の数が
///   戻した先と同じ。
/// - 入力の `anchor_id` はアンカーの行を指し、アンカーは台帳の行を持ち、元が入力の元と同じ。どの入力からも参照されない
///   アンカーはない（ゴミ箱を空にする処理が片付ける）。
/// - 下書きの文書は根の部品。版・下書きの文書は部品をつなぎ直せる（ダイジェストを照らす）。
/// - 終わっていない（待ち・実行中・一時停止の）保存のジョブの入力の文書は、根の部品があってつなぎ直せる（GC の部品の
///   片付けが消さない。M12 の段 3）。
///
/// 表を走査する（テストと整合性の確かめ・crash-loop で呼ぶ。起動の道では呼ばない）。
pub fn check_invariants(conn: &Connection) -> Result<Vec<String>> {
    let mut out = Vec::new();
    // 版の並び（全件の走査。意図したもの）
    let mut stmt = conn.prepare_cached(
        "SELECT item_id, count(*), min(version), max(version) FROM item_versions GROUP BY item_id
          HAVING count(*) = 1 OR min(version) <> 0 OR max(version) <> count(*) - 1",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (item, n, min, max) = row?;
        out.push(format!(
            "アイテム {item}：版の行が {n} 個（{min}〜{max}）で、0..n に欠けなく並んでいません"
        ));
    }
    // hold は版 0 と頭だけ、頭は items の今と同じ
    let mut stmt = conn.prepare_cached(
        "SELECT v.item_id, v.version, v.file_state, v.output_hash = i.hash AND v.ext = i.ext,
                v.version = (SELECT max(w.version) FROM item_versions w WHERE w.item_id = v.item_id)
           FROM item_versions v JOIN items i ON i.id = v.item_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, bool>(3)?,
            r.get::<_, bool>(4)?,
        ))
    })?;
    for row in rows {
        let (item, version, state, same_as_items, is_head) = row?;
        let hold = state == FileState::Hold.as_str();
        if (version == 0 || is_head) && !hold {
            out.push(format!(
                "アイテム {item}：版 {version}（{}）が hold ではありません（{state}）",
                if version == 0 { "元" } else { "頭" }
            ));
        }
        if version != 0 && !is_head && hold {
            out.push(format!(
                "アイテム {item}：頭でも元でもない版 {version} が hold です"
            ));
        }
        if is_head && !same_as_items {
            out.push(format!(
                "アイテム {item}：頭の版 {version} の出力・拡張子がアイテムの今と違います"
            ));
        }
    }
    // ファイルを持つ行の台帳の行
    let mut stmt = conn.prepare_cached(
        "SELECT v.item_id, v.version FROM item_versions v LEFT JOIN blobs b ON b.id = v.file_blob_id
          WHERE v.file_state <> 'released' AND (b.hash IS NULL OR b.hash <> v.output_hash)",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (item, version) = row?;
        out.push(format!(
            "アイテム {item}：版 {version} の file_blob_id が出力の台帳の行を指していません"
        ));
    }
    check_lineage(conn, &mut out)?;
    check_inputs_and_anchors(conn, &mut out)?;
    check_docs(conn, &mut out)?;
    Ok(out)
}

/// (アイテム, 版) の組を返す問い合わせを回し、組ごとに `say` の文を足す。
fn report_pairs(
    conn: &Connection,
    sql: &str,
    out: &mut Vec<String>,
    say: impl Fn(i64, i64) -> String,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (a, b) = row?;
        out.push(say(a, b));
    }
    Ok(())
}

/// 版の系譜：土台は 1 つ前の版、戻した版は戻した先と文書・値・入力の数が同じ、描いた版は入力 0 を持つ。
fn check_lineage(conn: &Connection, out: &mut Vec<String>) -> Result<()> {
    report_pairs(
        conn,
        "SELECT item_id, version FROM item_versions
          WHERE kind <> 'original' AND base_version IS NOT version - 1",
        out,
        |item, v| format!("アイテム {item}：版 {v} の土台が 1 つ前の版ではありません"),
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT v.item_id, v.version, v.revert_to, t.id IS NULL,
                t.doc_id IS NOT v.doc_id, t.value_digest IS NOT v.value_digest,
                (SELECT count(*) FROM item_version_inputs n WHERE n.version_id = v.id)
                  <> (SELECT count(*) FROM item_version_inputs n WHERE n.version_id = t.id)
           FROM item_versions v
           LEFT JOIN item_versions t ON t.item_id = v.item_id AND t.version = v.revert_to
          WHERE v.kind = 'revert'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, bool>(3)?,
            r.get::<_, bool>(4)?,
            r.get::<_, bool>(5)?,
            r.get::<_, Option<bool>>(6)?,
        ))
    })?;
    for row in rows {
        let (item, v, to, missing, doc, digest, inputs) = row?;
        let to = to.map_or_else(|| "?".to_string(), |t| t.to_string());
        if missing {
            out.push(format!(
                "アイテム {item}：版 {v} が戻した先の版 {to} がありません"
            ));
            continue;
        }
        for (broken, what) in [
            (doc, "文書"),
            (digest, "値のダイジェスト"),
            (inputs.unwrap_or(true), "入力の数"),
        ] {
            if broken {
                out.push(format!(
                    "アイテム {item}：版 {v} の{what}が、戻した先の版 {to} と違います"
                ));
            }
        }
    }
    report_pairs(
        conn,
        "SELECT v.item_id, v.version FROM item_versions v
          WHERE v.kind = 'edit' AND NOT EXISTS (
                SELECT 1 FROM item_version_inputs n
                 WHERE n.version_id = v.id AND n.input = 0 AND n.role = 'primary')",
        out,
        |item, v| format!("アイテム {item}：描いた版 {v} に入力 0（元）の記録がありません"),
    )
}

/// 入力とアンカー：入力のアンカーは台帳の行を持ち元が同じ、どの入力からも参照されないアンカーはない。
fn check_inputs_and_anchors(conn: &Connection, out: &mut Vec<String>) -> Result<()> {
    report_pairs(
        conn,
        "SELECT v.item_id, v.version FROM item_version_inputs n
           JOIN item_versions v ON v.id = n.version_id
           LEFT JOIN edit_anchors a ON a.id = n.anchor_id
           LEFT JOIN blobs b ON b.id = a.blob_id
          WHERE n.anchor_id IS NOT NULL
            AND (a.id IS NULL OR b.id IS NULL OR a.source_hash <> n.source_hash)",
        out,
        |item, v| {
            format!(
                "アイテム {item}：版 {v} の入力のアンカーが、台帳の行か同じ元のアンカーを指していません"
            )
        },
    )?;
    report_pairs(
        conn,
        "SELECT a.id, a.blob_id FROM edit_anchors a
          WHERE NOT EXISTS (SELECT 1 FROM item_version_inputs n WHERE n.anchor_id = a.id)",
        out,
        |id, _| format!("アンカー {id} がどの版の入力からも参照されていません"),
    )
}

/// 版・下書きの文書：根（doc）の部品を指し、部品をつなぎ直せる（本文からダイジェストを求め直して照らす）。
fn check_docs(conn: &Connection, out: &mut Vec<String>) -> Result<()> {
    report_pairs(
        conn,
        "SELECT v.item_id, v.version FROM item_versions v LEFT JOIN edit_objects o ON o.id = v.doc_id
          WHERE v.doc_id IS NOT NULL AND (o.id IS NULL OR o.kind <> 'doc')",
        out,
        |item, v| format!("アイテム {item}：版 {v} の文書が根の部品ではありません"),
    )?;
    report_pairs(
        conn,
        "SELECT d.item_id, d.doc_id FROM edit_drafts d LEFT JOIN edit_objects o ON o.id = d.doc_id
          WHERE o.id IS NULL OR o.kind <> 'doc'",
        out,
        |item, _| format!("アイテム {item}：下書きの文書が根の部品ではありません"),
    )?;
    let docs: Vec<i64> = {
        let mut stmt = conn.prepare_cached(
            "SELECT v.doc_id FROM item_versions v JOIN edit_objects o ON o.id = v.doc_id AND o.kind = 'doc'
             UNION
             SELECT d.doc_id FROM edit_drafts d JOIN edit_objects o ON o.id = d.doc_id AND o.kind = 'doc'",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    for doc in docs {
        if let Err(e) = store::load_doc_bytes(conn, doc) {
            out.push(format!("文書（部品 {doc}）をつなぎ直せません：{e}"));
        }
    }
    // 終わっていない保存のジョブの文書（GC の部品の片付けが残す根。M12 の段 3）
    for root in super::save::live_roots(conn)? {
        match store::doc_id_by_digest(conn, &root)? {
            None => out.push(format!(
                "終わっていない保存のジョブの文書（{root}）の根の部品がありません"
            )),
            Some(doc) => {
                if let Err(e) = store::load_doc_bytes(conn, doc) {
                    out.push(format!(
                        "終わっていない保存のジョブの文書（{root}）をつなぎ直せません：{e}"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 版 0（元）を足す（呼び手のトランザクションの中で。最初の保存と、別名で保存した新しいアイテム）。ファイルは `original` の
/// 中身の台帳の行を指し、hold。`value_digest`・`decoded` は今回元を読んだ記録。足した行の ID を返す。
pub(crate) fn insert_original(
    tx: &Connection,
    item_id: i64,
    original: &OriginalFile,
    decoded: &DecodeStamp,
    now: i64,
) -> Result<i64> {
    let blob = crate::library::ledger::id_of(tx, &original.hash)?.ok_or_else(|| {
        CoreError::NotFound(format!("アイテム {item_id} の元の中身が台帳にありません"))
    })?;
    let record = VersionRecord {
        decoded: Some(decoded.clone()),
        media: Some(original.media.clone()),
        ..VersionRecord::new()
    };
    insert_version(
        tx,
        &NewVersion {
            item_id,
            version: 0,
            kind: VersionKind::Original,
            doc_id: None,
            base_version: None,
            revert_to: None,
            output_hash: original.hash,
            ext: &original.ext,
            size_bytes: original.size_bytes,
            format: format_id(&original.ext),
            value_digest: Some(decoded.value_digest),
            width: original.width,
            height: original.height,
            duration_ms: original.duration_ms,
            record: &record,
            file_blob_id: Some(blob),
            file_state: FileState::Hold,
            replaced_at: None,
            job_id: None,
            now,
        },
    )
}

/// 頭でなくなる版 `version`（1 以上）の状態を変える（呼び手のトランザクションの中で）：出力が新しい頭の `new` か版 0 の
/// `original` と同じなら released（ファイルを持たない）、ほかは waiting。どちらも置き換えの時刻を持つ。変えた状態を返す。
pub(crate) fn supersede_head(
    tx: &Connection,
    item_id: i64,
    version: i64,
    new: &[u8; 32],
    original: &[u8; 32],
    now: i64,
) -> Result<FileState> {
    let old = get(tx, item_id, version)?
        .ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id} の版 {version}")))?;
    let state = supersede_state(&old.output_hash, new, original);
    let mut stmt = tx.prepare_cached(
        "UPDATE item_versions
            SET file_state = ?3,
                file_blob_id = CASE WHEN ?3 = 'released' THEN NULL ELSE file_blob_id END,
                replaced_at = ?4
          WHERE item_id = ?1 AND version = ?2",
    )?;
    stmt.execute(params![item_id, version, state.as_str(), now])?;
    Ok(state)
}

/// 足す版の行。
pub(crate) struct NewVersion<'a> {
    pub item_id: i64,
    pub version: i64,
    pub kind: VersionKind,
    pub doc_id: Option<i64>,
    pub base_version: Option<i64>,
    pub revert_to: Option<i64>,
    pub output_hash: [u8; 32],
    pub ext: &'a str,
    pub size_bytes: i64,
    pub format: Option<&'a str>,
    pub value_digest: Option<Digest>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub record: &'a VersionRecord,
    pub file_blob_id: Option<i64>,
    pub file_state: FileState,
    pub replaced_at: Option<i64>,
    pub job_id: Option<i64>,
    pub now: i64,
}

/// 版の行を足し、行の ID を返す（呼び手のトランザクションの中で。形の決まりは表の CHECK が確かめる）。
pub(crate) fn insert_version(tx: &Connection, v: &NewVersion<'_>) -> Result<i64> {
    let record = serde_json::to_string(v.record)?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO item_versions (item_id, version, kind, doc_id, base_version, revert_to, output_hash, ext,
                                    size_bytes, format, value_digest, width, height, duration_ms, record,
                                    file_blob_id, file_state, replaced_at, job_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
    )?;
    stmt.execute(params![
        v.item_id,
        v.version,
        v.kind.as_str(),
        v.doc_id,
        v.base_version,
        v.revert_to,
        &v.output_hash[..],
        v.ext,
        v.size_bytes,
        v.format,
        v.value_digest.as_ref().map(|d| d.0.to_vec()),
        v.width,
        v.height,
        v.duration_ms,
        record,
        v.file_blob_id,
        v.file_state.as_str(),
        v.replaced_at,
        v.job_id,
        v.now,
    ])?;
    Ok(tx.last_insert_rowid())
}

/// 足す版の入力の行。
pub(crate) struct NewInput<'a> {
    pub input: i64,
    pub role: InputRole,
    pub source_hash: [u8; 32],
    pub value_digest: Digest,
    pub recorded: &'a str,
    pub anchor_id: Option<i64>,
}

/// 版の入力の行を足す（呼び手のトランザクションの中で）。
pub(crate) fn insert_inputs(
    tx: &Connection,
    version_id: i64,
    inputs: &[NewInput<'_>],
) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO item_version_inputs (version_id, input, role, source_hash, value_digest, recorded, anchor_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for i in inputs {
        stmt.execute(params![
            version_id,
            i.input,
            i.role.as_str(),
            &i.source_hash[..],
            &i.value_digest.0[..],
            i.recorded,
            i.anchor_id,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};
    use crate::edit::testing::{insert_item, insert_version};

    #[test]
    fn supersede_state_releases_only_files_someone_else_holds() {
        let (a, b, o) = ([1; 32], [2; 32], [3; 32]);
        assert_eq!(supersede_state(&a, &b, &o), FileState::Waiting);
        assert_eq!(
            supersede_state(&a, &a, &o),
            FileState::Released,
            "版だけ足す"
        );
        assert_eq!(supersede_state(&o, &b, &o), FileState::Released, "元と同じ");
        for s in [
            FileState::Hold,
            FileState::Waiting,
            FileState::Kept,
            FileState::Released,
        ] {
            assert_eq!(FileState::parse(s.as_str()), Some(s));
        }
        for k in [
            VersionKind::Original,
            VersionKind::Edit,
            VersionKind::Revert,
        ] {
            assert_eq!(VersionKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(FileState::parse("gone"), None);
    }

    /// 版の行がなければ、頭は版 0 で元はアイテムの今。行があれば最大の版と版 0 の行。
    #[test]
    fn head_and_original_read_the_implicit_version_zero() {
        let conn = test_conn();
        let id = insert_item(&conn, 1, [1; 32]);
        let h = head(&conn, id).unwrap().unwrap();
        assert_eq!((h.hash, h.version, h.trashed), ([1; 32], 0, false));
        assert_eq!(h.kind, MediaKind::Image);
        let o = original(&conn, id).unwrap().unwrap();
        assert_eq!((o.hash, o.recorded), ([1; 32], false));
        assert!(head(&conn, 99).unwrap().is_none());
        assert!(original(&conn, 99).unwrap().is_none());
        assert!(list(&conn, id).unwrap().is_empty());
        assert!(check_invariants(&conn).unwrap().is_empty());

        insert_version(&conn, id, 0, [5; 32], "hold", None);
        insert_version(&conn, id, 1, [1; 32], "hold", None);
        let h = head(&conn, id).unwrap().unwrap();
        assert_eq!(h.version, 1);
        let o = original(&conn, id).unwrap().unwrap();
        assert_eq!((o.hash, o.recorded), ([5; 32], true));
        let all = list(&conn, id).unwrap();
        assert_eq!(
            all.iter().map(|v| v.version).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(all[1].kind, VersionKind::Edit);
        assert_eq!(
            get(&conn, id, 1).unwrap().unwrap().file_state,
            FileState::Hold
        );
        assert!(check_invariants(&conn).unwrap().is_empty());
        let engine = Engine::new().unwrap();
        assert_eq!(doc_of(&conn, &engine, id, 0).unwrap(), Graph::new_single());
        assert_eq!(doc_of(&conn, &engine, id, 1).unwrap(), Graph::new_single());
        assert!(matches!(
            doc_of(&conn, &engine, id, 7),
            Err(CoreError::NotFound(_))
        ));
    }

    /// 不変条件の破れを文で返す：版 1 つだけ、hold でない頭、頭がアイテムの今と違う、頭でも元でもない hold。
    #[test]
    fn check_invariants_reports_each_break() {
        let conn = test_conn();
        let a = insert_item(&conn, 1, [1; 32]);
        insert_version(&conn, a, 0, [1; 32], "hold", None);
        let found = check_invariants(&conn).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("0..n"), "{found:?}");

        let conn = test_conn();
        let a = insert_item(&conn, 1, [1; 32]);
        insert_version(&conn, a, 0, [5; 32], "hold", None);
        insert_version(&conn, a, 1, [6; 32], "hold", None);
        insert_version(&conn, a, 2, [7; 32], "waiting", None);
        let found = check_invariants(&conn).unwrap();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().any(|m| m.contains("頭でも元でもない版 1")));
        assert!(
            found
                .iter()
                .any(|m| m.contains("版 2（頭）が hold ではありません"))
        );
        assert!(found.iter().any(|m| m.contains("アイテムの今と違います")));

        // 台帳の行が別の中身を指す
        let conn = test_conn();
        let a = insert_item(&conn, 1, [1; 32]);
        insert_version(&conn, a, 0, [5; 32], "hold", None);
        insert_version(&conn, a, 1, [1; 32], "hold", None);
        conn.execute(
            "UPDATE item_versions SET file_blob_id = (SELECT id FROM blobs WHERE hash = ?1) WHERE version = 0",
            [&[1u8; 32][..]],
        )
        .unwrap();
        let found = check_invariants(&conn).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("file_blob_id"), "{found:?}");
    }

    /// 系譜・入力・アンカー・文書の破れ：土台が 1 つ前でない、描いた版に入力がない、戻した版の先がない・文書が違う、入力の
    /// アンカーの元が違う、参照されないアンカー、下書き・版の文書が根でない・つなぎ直せない。
    #[test]
    fn check_invariants_reports_lineage_input_anchor_and_doc_breaks() {
        use crate::edit::testing::insert_anchor;
        let found_one = |conn: &Connection, needle: &str| {
            let found = check_invariants(conn).unwrap();
            assert_eq!(found.len(), 1, "{found:?}");
            assert!(found[0].contains(needle), "{found:?}");
        };
        let setup = || {
            let conn = test_conn();
            let a = insert_item(&conn, 1, [1; 32]);
            insert_version(&conn, a, 0, [5; 32], "hold", None);
            insert_version(&conn, a, 1, [6; 32], "waiting", None);
            insert_version(&conn, a, 2, [1; 32], "hold", None);
            assert!(check_invariants(&conn).unwrap().is_empty());
            (conn, a)
        };
        let (conn, _) = setup();
        conn.execute(
            "UPDATE item_versions SET base_version = 0 WHERE version = 2",
            [],
        )
        .unwrap();
        found_one(&conn, "土台が 1 つ前の版ではありません");

        let (conn, _) = setup();
        conn.execute(
            "DELETE FROM item_version_inputs WHERE version_id = (SELECT id FROM item_versions WHERE version = 1)",
            [],
        )
        .unwrap();
        found_one(&conn, "入力 0（元）の記録がありません");

        // 版 2 を「版 1 に戻した版」にする：文書・値・入力の数が版 1 と同じなら破れなし、文書が違えば破れ
        let (conn, _) = setup();
        conn.execute(
            "UPDATE item_versions SET kind = 'revert', revert_to = 1,
                    doc_id = (SELECT doc_id FROM item_versions WHERE version = 1) WHERE version = 2",
            [],
        )
        .unwrap();
        assert!(check_invariants(&conn).unwrap().is_empty());
        conn.execute(
            "UPDATE item_versions SET doc_id = NULL WHERE version = 2",
            [],
        )
        .unwrap();
        found_one(&conn, "文書が、戻した先の版 1 と違います");
        conn.execute(
            "UPDATE item_versions SET doc_id = (SELECT doc_id FROM item_versions WHERE version = 1),
                    value_digest = x'0101010101010101010101010101010101010101010101010101010101010101'
              WHERE version = 2",
            [],
        )
        .unwrap();
        found_one(&conn, "値のダイジェストが、戻した先の版 1 と違います");

        // アンカー：入力が使うアンカーの元が違う・どの入力も使わないアンカー
        let (conn, a) = setup();
        let anchor = insert_anchor(&conn, [5; 32], [11; 32]);
        insert_version(&conn, a, 3, [1; 32], "hold", Some(anchor));
        conn.execute(
            "UPDATE item_versions SET file_state = 'released', file_blob_id = NULL, replaced_at = 5 WHERE version = 2",
            [],
        )
        .unwrap();
        assert!(check_invariants(&conn).unwrap().is_empty());
        conn.execute("UPDATE edit_anchors SET source_hash = x'09'", [])
            .unwrap();
        found_one(&conn, "同じ元のアンカーを指していません");
        let (conn, _) = setup();
        insert_anchor(&conn, [5; 32], [12; 32]);
        found_one(&conn, "どの版の入力からも参照されていません");

        // 下書き：根でない部品・つなぎ直せない文書
        let draft = |conn: &Connection, item: i64, doc: i64| {
            conn.execute(
                "INSERT OR REPLACE INTO edit_drafts (item_id, doc_id, base_hash, base_version, updated_at)
                 VALUES (?1, ?2, x'00', 2, 1)",
                params![item, doc],
            )
            .unwrap();
        };
        let (conn, a) = setup();
        let mut g = Graph::new_single();
        g.push_linear(
            "image.flip",
            1,
            serde_json::json!({ "axis": "vertical" }),
            0,
        )
        .unwrap();
        let root = store::put_doc(&conn, &kura_engine::doc_parts::split(&g).unwrap(), 1).unwrap();
        draft(&conn, a, root);
        assert!(check_invariants(&conn).unwrap().is_empty());
        let node: i64 = conn
            .query_row("SELECT id FROM edit_objects WHERE kind = 'node'", [], |r| {
                r.get(0)
            })
            .unwrap();
        draft(&conn, a, node);
        found_one(&conn, "下書きの文書が根の部品ではありません");
        draft(&conn, a, root);
        conn.execute("DELETE FROM edit_edges WHERE parent_id = ?1", [root])
            .unwrap();
        found_one(&conn, "つなぎ直せません");

        // 終わっていない保存のジョブの文書の根がない（GC が消してはいけないもの）。終われば問わない
        let (conn, _) = setup();
        let job = |state: &str| {
            conn.execute(
                "INSERT INTO jobs (kind, state, input_json, created_at) VALUES ('edit', ?1, ?2, 1)",
                params![
                    state,
                    format!(
                        r#"{{"v":1,"itemId":1,"base":{{"hash":"{}","version":2}},"action":{{"type":"overwrite","doc":"{}"}}}}"#,
                        "00".repeat(32),
                        "ab".repeat(32)
                    )
                ],
            )
            .unwrap();
        };
        job("failed");
        assert!(check_invariants(&conn).unwrap().is_empty());
        job("paused");
        found_one(&conn, "終わっていない保存のジョブの文書");
    }

    /// 頭でなくなる版は、出力が新しい頭か元と同じならすぐ released（台帳の行を指さない）、ほかは waiting。
    #[test]
    fn supersede_head_sets_state_and_time() {
        let conn = test_conn();
        let a = insert_item(&conn, 1, [1; 32]);
        insert_version(&conn, a, 0, [5; 32], "hold", None);
        insert_version(&conn, a, 1, [1; 32], "hold", None);
        assert_eq!(
            supersede_head(&conn, a, 1, &[9; 32], &[5; 32], 77).unwrap(),
            FileState::Waiting
        );
        let v1 = get(&conn, a, 1).unwrap().unwrap();
        assert_eq!(
            (v1.file_state, v1.replaced_at, v1.file_blob_id.is_some()),
            (FileState::Waiting, Some(77), true)
        );
        conn.execute(
            "UPDATE item_versions SET file_state = 'hold', replaced_at = NULL WHERE version = 1",
            [],
        )
        .unwrap();
        assert_eq!(
            supersede_head(&conn, a, 1, &[1; 32], &[5; 32], 78).unwrap(),
            FileState::Released
        );
        let v1 = get(&conn, a, 1).unwrap().unwrap();
        assert_eq!(
            (v1.file_state, v1.file_blob_id),
            (FileState::Released, None)
        );
    }

    #[test]
    fn queries_use_their_indexes() {
        let conn = test_conn();
        let plan = query_plan(
            &conn,
            "SELECT i.hash, i.ext, i.kind, i.trashed_at IS NOT NULL,
                    (SELECT coalesce(max(v.version), 0) FROM item_versions v WHERE v.item_id = i.id)
               FROM items i WHERE i.id = 1",
        );
        assert!(
            plan.contains("SEARCH i USING INTEGER PRIMARY KEY"),
            "{plan}"
        );
        assert!(plan.contains("sqlite_autoindex_item_versions_1"), "{plan}");
        assert!(!plan.contains("SCAN"), "{plan}");
        let plan = query_plan(
            &conn,
            &format!("SELECT {VERSION_COLUMNS} FROM item_versions WHERE job_id = 5"),
        );
        assert!(plan.contains("idx_item_versions_job"), "{plan}");
        let plan = query_plan(
            &conn,
            &format!(
                "SELECT {VERSION_COLUMNS} FROM item_versions WHERE item_id = 1 ORDER BY version"
            ),
        );
        assert!(plan.contains("sqlite_autoindex_item_versions_1"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        let plan = query_plan(
            &conn,
            "SELECT input, role, source_hash, value_digest, recorded, anchor_id
               FROM item_version_inputs WHERE version_id = 1 ORDER BY input",
        );
        assert!(plan.contains("PRIMARY KEY (version_id=?)"), "{plan}");
    }
}
