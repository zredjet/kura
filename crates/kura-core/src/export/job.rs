//! 書き出しのジョブ（`kind = 'export'`）。M15 段 3 で名前の規則・形式の変換（エンジンで描く）・Kura が書いたファイルの置き換え・
//! プロジェクトの中への書き出しを足した（ADR-205）。

use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::formats::{ExportOptions, Plan, plan};
use super::names::{NameParts, numbered_dir, numbered_file, rule_folder, rule_name, safe_name};
use crate::edit::save::{Base, Flow, GraphExtra, Mode, Saver, UnitCx};
use crate::edit::services::EngineServices;
use crate::import::{MediaKind, hash_file};
use crate::items::detail;
use crate::jobs::{
    Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager, save_partial,
};
use crate::library::Library;
use crate::tags::TagName;
use crate::time::{format_iso8601, now_ms};
use crate::unit::{self, LIST_FLAG};
use crate::{CoreError, Result};

/// 書き出しに添える JSON の名前（同じ名前があれば「kura-export 2.json」）。
const MANIFEST_NAME: &str = "kura-export.json";

/// 同じ名前を避けて番号を付けるのを諦めるまでの回数。
const MAX_TRIES: u32 = 10_000;

/// 途中経過を保存する間隔。
const SAVE_EVERY: Duration = Duration::from_secs(2);

/// 書き出すもの（ジョブの入力）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportInput {
    /// 書き出し先のフォルダ（あるもの）。
    pub dest: PathBuf,
    /// 一覧の ID（アイテム、またはユニットは `LIST_FLAG | unit_id`）。選んだ順。
    pub entries: Vec<i64>,
    /// 名前の規則・形式・JSON・プロジェクト（M15 段 3。省けば M9 と同じ）。
    #[serde(default)]
    pub options: ExportOptions,
}

/// 書き出したファイル（書き出し先からの相対パス）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedFile {
    pub item_id: i64,
    pub path: String,
}

/// 書き出しの結果（途中経過も同じ形で保存し、再開に使う）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub dest: String,
    pub total: u64,
    pub exported: u64,
    pub failed: u64,
    /// 形式を変えて書いた数（M15 段 3）。
    #[serde(default)]
    pub converted: u64,
    /// Kura が書いた記録どおりのファイルを置き換えた数（中身が同じで書かなかったものを含む）。
    #[serde(default)]
    pub replaced: u64,
    /// 飛ばした数（ゴミ箱の中のアイテム。M15 段 3 の検証の直し）。
    #[serde(default)]
    pub skipped: u64,
    /// OS のデコーダで読んでいる 1 件の位置（読む前に書き、読み終えたら消す。残っていれば次の回にその 1 件を失敗にする）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoding: Option<u64>,
    /// 書いた JSON の名前（終わったとき）。
    pub manifest: Option<String>,
    /// 次に書き出す位置（再開用）。
    #[serde(default)]
    pub cursor: u64,
    /// 書き出したファイル（JSON を書くのに使う。終わったら空にする）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ExportedFile>,
    /// ユニットごとに作ったフォルダの名前（再開しても同じフォルダに書く）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<Option<String>>,
}

/// 書き出しのジョブを積む（名前のテンプレートと形式を確かめる）。
pub fn start_export(jobs: &JobManager, input: &ExportInput) -> Result<JobId> {
    if input.entries.is_empty() {
        return Err(CoreError::Invalid("書き出す項目を選んでください".into()));
    }
    input.options.check()?;
    jobs.enqueue(JobKind::Export, serde_json::to_string(input)?)
}

/// 書き出しのジョブの処理。形式を変えるのにエンジンの部品を使う（ないライブラリでは、形式を変える 1 件は Unsupported）。
#[derive(Default)]
pub struct ExportHandler {
    engine: Option<Arc<EngineServices>>,
}

impl std::fmt::Debug for ExportHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportHandler")
            .field("engine", &self.engine.is_some())
            .finish()
    }
}

impl ExportHandler {
    /// エンジンの部品（あれば）で作る。
    pub fn new(engine: Option<Arc<EngineServices>>) -> Self {
        Self { engine }
    }
}

/// 書き出しのまとまり（ばらのアイテムは `folder` なし、ユニットはユニット名のフォルダ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub folder: Option<String>,
    /// ユニットの名前（テンプレートの {ユニット}）。
    pub unit: Option<String>,
    /// ユニットの ID（フォルダの名前が空になったときの `unit-<ID>`）。
    pub unit_id: Option<i64>,
    pub items: Vec<i64>,
}

/// 選んだものをまとまりにする：ばらのアイテム（重ねて選んだものは 1 回）、続いてユニットごと。
/// 見つからないユニットは飛ばす。
pub(super) fn resolve(conn: &Connection, entries: &[i64]) -> Result<Vec<Group>> {
    let mut loose = Group {
        folder: None,
        unit: None,
        unit_id: None,
        items: Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut units = Vec::new();
    for &entry in entries {
        let raw = u32::try_from(entry).unwrap_or(0);
        if raw & LIST_FLAG != 0 {
            let unit_id = i64::from(raw & !LIST_FLAG);
            let Some(summary) = unit::summaries(conn, &[unit_id])?.into_iter().next() else {
                continue;
            };
            let mut members = unit::members(conn, unit_id)?;
            let mut in_unit = HashSet::new();
            members.retain(|id| in_unit.insert(*id));
            let fallback = format!("unit-{unit_id}");
            units.push(Group {
                folder: Some(safe_name(&summary.name, &fallback)),
                unit: Some(summary.name.clone()),
                unit_id: Some(unit_id),
                items: members,
            });
        } else if entry > 0 && seen.insert(entry) {
            loose.items.push(entry);
        }
    }
    let mut groups = vec![loose];
    groups.extend(units);
    Ok(groups)
}

impl JobHandler for ExportHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let input: ExportInput = serde_json::from_str(ctx.input())?;
        let options = input.options.clone();
        let mut state: ExportResult = match ctx.partial() {
            Some(json) => serde_json::from_str(json)?,
            None => ExportResult::default(),
        };
        let library = ctx.library();
        if !input.dest.is_dir() {
            return Err(CoreError::Invalid(format!(
                "書き出し先のフォルダが見つかりません: {}",
                input.dest.display()
            )));
        }
        // 書いたファイルの記録の鍵にする、書き出し先の正規化したパス
        let dest_c = std::fs::canonicalize(&input.dest)?;
        let mut groups = library.db().read(|conn| resolve(conn, &input.entries))?;
        // ユニットのフォルダにも書き方と ASCII を当てる（テンプレートは当てない。空になれば unit-<ID>）
        for group in &mut groups {
            if let (Some(unit), Some(id)) = (&group.unit, group.unit_id) {
                let fallback = format!("unit-{id}");
                let named = rule_folder(&options.name, unit, &fallback);
                group.folder = Some(safe_name(&named, &fallback));
            }
        }
        let flat: Vec<(usize, i64)> = groups
            .iter()
            .enumerate()
            .flat_map(|(g, group)| group.items.iter().map(move |id| (g, *id)))
            .collect();
        state.dest = input.dest.display().to_string();
        state.total = flat.len() as u64;
        if state.folders.len() != groups.len() {
            state.folders = vec![None; groups.len()];
        }

        let total = flat.len();
        let mut cursor = usize::try_from(state.cursor).unwrap_or(0).min(total);
        // 前の回に OS のデコーダで読んでいる途中で落ちた 1 件は、失敗にして先へ（同じファイルで落ち続けない）
        if let Some(at) = state.decoding.take()
            && usize::try_from(at).ok() == Some(cursor)
            && cursor < total
        {
            state.failed += 1;
            ctx.log(&format!(
                "書き出せませんでした: アイテム {}（読んでいる途中で終了しました）",
                flat[cursor].1
            ));
            cursor += 1;
            state.cursor = cursor as u64;
        }
        let writable_formats = writable_formats(self.engine.as_ref());
        let writable = |f: &str| writable_formats.as_ref().is_some_and(|w| w.contains(f));
        let writable_ref: Option<&dyn Fn(&str) -> bool> = writable_formats
            .as_ref()
            .map(|_| &writable as &dyn Fn(&str) -> bool);
        let mut saved = Instant::now();
        while cursor < total {
            match ctx.control() {
                Control::Continue => {}
                Control::Pause => {
                    save(ctx, library, &state)?;
                    return Ok(JobEnd::Paused);
                }
                Control::Cancel => return canceled(&mut state),
            }
            let (g, item_id) = flat[cursor];
            let outcome = match folder_for(
                library,
                &input.dest,
                &dest_c,
                &groups[g],
                &mut state.folders[g],
            ) {
                Err(err) => Err(err),
                Ok((dir, rel)) => {
                    let one = One {
                        item_id,
                        index: cursor + 1,
                        unit: groups[g].unit.clone(),
                        options: &options,
                        writable: writable_ref,
                        state: &state,
                        cursor,
                    };
                    let dir_c = match &rel {
                        Some(folder) => dest_c.join(folder),
                        None => dest_c.clone(),
                    };
                    self.export_one(ctx, &one, &dir, &dir_c).map(|done| {
                        let path = match rel {
                            Some(folder) => format!("{folder}/{}", done.name),
                            None => done.name.clone(),
                        };
                        (path, done)
                    })
                }
            };
            match outcome {
                Ok((_, done)) if done.stopped.is_some() => match done.stopped {
                    Some(JobEnd::Canceled { .. }) => return canceled(&mut state),
                    Some(end) => {
                        save(ctx, library, &state)?;
                        return Ok(end);
                    }
                    None => {}
                },
                Ok((_, done)) if done.skipped => {
                    state.skipped += 1;
                    ctx.log(&format!(
                        "飛ばしました: アイテム {item_id}（ゴミ箱の中のアイテムは書き出せません）"
                    ));
                }
                Ok((path, done)) => {
                    state.exported += 1;
                    if done.converted {
                        state.converted += 1;
                    }
                    if done.replaced {
                        state.replaced += 1;
                    }
                    state.files.push(ExportedFile { item_id, path });
                }
                Err(err) => {
                    state.failed += 1;
                    ctx.log(&format!("書き出せませんでした: アイテム {item_id} ({err})"));
                }
            }
            cursor += 1;
            state.cursor = cursor as u64;
            ctx.progress(
                cursor as f64 / total as f64,
                &progress_message(cursor, total),
            );
            if saved.elapsed() >= SAVE_EVERY {
                save(ctx, library, &state)?;
                saved = Instant::now();
            }
        }

        if options.manifest {
            let records = library
                .db()
                .read(|conn| manifest_records(conn, &state.files))?;
            let manifest = write_manifest(&input.dest, &records)?;
            state.manifest = Some(manifest);
        }
        state.files.clear();
        state.folders.clear();
        // プロジェクトの中へ書き出したら照らし直す（使った記録を付ける。M15 段 2）
        if options.project.is_some()
            && let Err(err) = ctx.ensure_queued(JobKind::Project, "{}")
        {
            ctx.log(&format!(
                "プロジェクトの照らし直しを積めませんでした ({err})"
            ));
        }
        let message = if state.converted > 0 {
            format!(
                "{} 件を書き出しました（形式を変えたもの {}）",
                state.exported, state.converted
            )
        } else {
            format!("{} 件を書き出しました", state.exported)
        };
        Ok(JobEnd::Done {
            result_json: serde_json::to_string(&state)?,
            message: Some(message),
        })
    }
}

/// エンジンのコーデックの組が書ける形式（エンジンがなければ None：形式を変える 1 件は Unsupported）。
pub(super) fn writable_formats(engine: Option<&Arc<EngineServices>>) -> Option<BTreeSet<String>> {
    let codecs = engine.map(|e| e.codecs_if_built().unwrap_or_else(|| e.codecs()));
    codecs.map(|c| {
        c.capabilities()
            .into_iter()
            .filter(|f| !f.write.is_empty())
            .map(|f| f.format.to_string())
            .collect()
    })
}

/// 中止：書いたファイルはそのまま残し（書き出し先は消さない）、ファイルとフォルダの一覧は結果に残さない。
fn canceled(state: &mut ExportResult) -> Result<JobEnd> {
    state.files.clear();
    state.folders.clear();
    state.decoding = None;
    Ok(JobEnd::Canceled {
        result_json: Some(serde_json::to_string(state)?),
    })
}

fn progress_message(done: usize, total: usize) -> String {
    format!("{done} / {total} 件")
}

/// 途中経過を保存する（画面へは送らない。ファイルの一覧は大きくなるので）。
fn save(ctx: &JobContext, library: &Library, state: &ExportResult) -> Result<()> {
    let json = serde_json::to_string(state)?;
    let (id, total) = (ctx.id(), state.total.max(1));
    let progress = state.cursor as f64 / total as f64;
    let message = progress_message(
        usize::try_from(state.cursor).unwrap_or(0),
        usize::try_from(state.total).unwrap_or(0),
    );
    library
        .db()
        .write(move |tx| save_partial(tx, id, &json, progress, &message))
}

/// まとまりの書き出し先のフォルダ（ユニットはフォルダを作る）。同じ名前のフォルダがあれば、その中にこのユニットのアイテムを
/// Kura が書いた記録があるならそのフォルダを使い直し（書き出し直し。段 3 の検証の直し・利用者の決定）、ほかは番号を付ける。
/// 書き出し先からの相対の名前も返す。
fn folder_for(
    library: &Library,
    dest: &Path,
    dest_c: &Path,
    group: &Group,
    chosen: &mut Option<String>,
) -> Result<(PathBuf, Option<String>)> {
    let Some(name) = &group.folder else {
        return Ok((dest.to_path_buf(), None));
    };
    if let Some(existing) = chosen {
        return Ok((dest.join(&*existing), Some(existing.clone())));
    }
    let members: BTreeSet<i64> = group.items.iter().copied().collect();
    for n in 1..=MAX_TRIES {
        let candidate = numbered_dir(name, n);
        match std::fs::create_dir(dest.join(&candidate)) {
            Ok(()) => {
                *chosen = Some(candidate.clone());
                return Ok((dest.join(&candidate), Some(candidate)));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let is_dir =
                    std::fs::symlink_metadata(dest.join(&candidate)).is_ok_and(|m| m.is_dir());
                if is_dir && recorded_under(library, &dest_c.join(&candidate), &members)? {
                    *chosen = Some(candidate.clone());
                    return Ok((dest.join(&candidate), Some(candidate)));
                }
                continue;
            }
            Err(err) => return Err(err.into()),
        }
    }
    Err(CoreError::Invalid(format!(
        "同じ名前のフォルダが多すぎます: {name}"
    )))
}

/// フォルダ `dir`（正規化したパス）の中に、`members` のどれかのアイテムを Kura が書いた記録があるか（主キーの範囲で引く）。
fn recorded_under(library: &Library, dir: &Path, members: &BTreeSet<i64>) -> Result<bool> {
    let Some(prefix) = dir.to_str().map(|p| format!("{p}/")) else {
        return Ok(false);
    };
    // '/' の次の文字 '0' までの範囲がフォルダの中
    let upper = format!("{}0", &prefix[..prefix.len() - 1]);
    let items: Vec<i64> = library.db().read(move |c| {
        let mut stmt = c.prepare_cached(
            "SELECT item_id FROM export_files WHERE path > ?1 AND path < ?2 LIMIT 10000",
        )?;
        let rows = stmt.query_map(rusqlite::params![prefix, upper], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    })?;
    Ok(items.iter().any(|i| members.contains(i)))
}

/// 1 件を書き出すのに渡すもの。
struct One<'a> {
    item_id: i64,
    /// 書き出す順（1 から。テンプレートの {番号}）。
    index: usize,
    unit: Option<String>,
    options: &'a ExportOptions,
    writable: Option<&'a dyn Fn(&str) -> bool>,
    /// 読む前の印に書く今の途中経過。
    state: &'a ExportResult,
    cursor: usize,
}

/// 1 件を書き出した結果。
#[derive(Default)]
struct Done {
    /// 書き出した名前（そのフォルダの中）。
    name: String,
    converted: bool,
    replaced: bool,
    /// 飛ばした（ゴミ箱の中のアイテム）。
    skipped: bool,
    /// 形式を変えて描く途中で止めた（一時停止・中止・譲る）。
    stopped: Option<JobEnd>,
}

/// 書き出しの 1 件の場（形式を変えて描くとき、保存の準備の段に渡す）。読む前の印は、書き出しの途中経過の `decoding` に
/// DB にだけ書く（画面へは送らない。ファイルの一覧は大きくなるので）。
struct ExportUnit<'a> {
    ctx: &'a JobContext,
    state: &'a ExportResult,
    cursor: usize,
}

impl UnitCx for ExportUnit<'_> {
    fn library(&self) -> &Library {
        self.ctx.library()
    }

    fn control(&self) -> Control {
        self.ctx.control()
    }

    fn progress(&self, _progress: f64, _message: &str) {}

    fn log(&self, line: &str) {
        self.ctx.log(line);
    }

    fn mark(&self, result_json: &str, _progress: f64, _message: &str) -> Result<()> {
        let decoding = serde_json::from_str::<serde_json::Value>(result_json)
            .ok()
            .and_then(|v| v.get("decoding").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        let mut state = self.state.clone();
        state.decoding = decoding.then_some(self.cursor as u64);
        save(self.ctx, self.ctx.library(), &state)
    }

    fn job(&self) -> i64 {
        self.ctx.id()
    }

    fn version_job(&self) -> Option<i64> {
        None
    }

    // 書き出しの staging は置いたあとで捨てる一時のファイルなので、同期しない
    fn sync_file(&self, _file: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }

    fn tag(&self) -> String {
        format!("export-{}", self.ctx.id())
    }
}

/// アイテムの行（書き出しに使う列）。
pub(super) struct ItemRow {
    pub(super) hash: [u8; 32],
    pub(super) ext: String,
    pub(super) kind: MediaKind,
    pub(super) original_name: String,
    pub(super) title: String,
    pub(super) width: Option<i64>,
    pub(super) height: Option<i64>,
    /// 主なコーデック（codec_info。m4a の可逆を見分ける）。
    pub(super) codec: Option<String>,
    pub(super) trashed: bool,
}

pub(super) fn item_row(library: &Library, item_id: i64) -> Result<ItemRow> {
    type Row = (
        Vec<u8>,
        String,
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
        Option<String>,
        bool,
    );
    let row: Option<Row> = library.db().read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT hash, ext, kind, original_name, title, width, height, json_extract(codec_info, '$.codec'),
                    trashed_at IS NOT NULL
               FROM items WHERE id = ?1",
        )?;
        Ok(stmt
            .query_row([item_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })
            .optional()?)
    })?;
    let Some((hash, ext, kind, original_name, title, width, height, codec, trashed)) = row else {
        return Err(CoreError::NotFound(format!("アイテム {item_id}")));
    };
    let hash: [u8; 32] = hash
        .try_into()
        .map_err(|_| CoreError::Invalid(format!("アイテム {item_id} のハッシュが壊れています")))?;
    let kind = MediaKind::parse(&kind)
        .ok_or_else(|| CoreError::Invalid(format!("アイテム {item_id} の種類が違います")))?;
    Ok(ItemRow {
        hash,
        ext,
        kind,
        original_name,
        title,
        width,
        height,
        codec,
        trashed,
    })
}

/// 置き方に渡すもの。
struct PlaceCx<'a> {
    library: &'a Library,
    job: JobId,
    item_id: i64,
    /// 書く中身とその大きさ。
    hash: [u8; 32],
    size: u64,
    /// 置くフォルダとその正規化したパス（記録の鍵）。
    dir: &'a Path,
    dir_c: &'a Path,
    temp_name: &'a str,
}

/// Kura が書いたファイルの記録。
struct Record {
    item_id: i64,
    hash: Vec<u8>,
    size: i64,
    mtime_ns: i64,
    job_id: i64,
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

fn key_of(dir_c: &Path, name: &str) -> Option<String> {
    dir_c.join(name).to_str().map(str::to_string)
}

fn record_of(library: &Library, key: &str) -> Result<Option<Record>> {
    let key = key.to_string();
    library.db().read(move |c| {
        Ok(c.prepare_cached(
            "SELECT item_id, hash, size_bytes, mtime_ns, job_id FROM export_files WHERE path = ?1",
        )?
        .query_row([key], |r| {
            Ok(Record {
                item_id: r.get(0)?,
                hash: r.get(1)?,
                size: r.get(2)?,
                mtime_ns: r.get(3)?,
                job_id: r.get(4)?,
            })
        })
        .optional()?)
    })
}

/// 書いたファイルを記録する（同じパスの前の記録は置き換える）。古い名前の記録（大文字小文字を直したとき）は消す。
fn write_record(cx: &PlaceCx<'_>, name: &str, old: Option<&str>) -> Result<()> {
    let Some(key) = key_of(cx.dir_c, name) else {
        return Ok(());
    };
    let meta = std::fs::symlink_metadata(cx.dir.join(name))?;
    let (item, hash, size, mtime, job) = (
        cx.item_id,
        cx.hash,
        i64::try_from(meta.len()).unwrap_or(i64::MAX),
        mtime_ns(&meta),
        cx.job,
    );
    let old = old.and_then(|o| key_of(cx.dir_c, o)).filter(|o| *o != key);
    cx.library.db().write(move |tx| {
        if let Some(old) = old {
            tx.prepare_cached("DELETE FROM export_files WHERE path = ?1")?
                .execute([old])?;
        }
        tx.prepare_cached(
            "INSERT INTO export_files (path, item_id, hash, size_bytes, mtime_ns, job_id, written_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET item_id = excluded.item_id, hash = excluded.hash,
               size_bytes = excluded.size_bytes, mtime_ns = excluded.mtime_ns, job_id = excluded.job_id,
               written_at = excluded.written_at",
        )?
        .execute(rusqlite::params![key, item, &hash[..], size, mtime, job, now_ms()])?;
        Ok(())
    })
}

/// フォルダの中の、大文字小文字を問わず `name` と同じ名前の実際の名前（APFS は大文字小文字を区別しないことが多い）。
fn actual_name(dir: &Path, name: &str) -> String {
    if dir.join(name).exists()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        let lower = name.to_lowercase();
        let mut found = None;
        for e in entries.flatten() {
            if let Some(n) = e.file_name().to_str() {
                if n == name {
                    return name.to_string();
                }
                if found.is_none() && n.to_lowercase() == lower {
                    found = Some(n.to_string());
                }
            }
        }
        if let Some(n) = found {
            return n;
        }
    }
    name.to_string()
}

/// `src` を置く。同じ名前のファイルがあれば：
/// - 中身が書く中身と同じなら書かない（時刻を変えず、エンジンが読み直さない。大文字小文字だけ違えば名前を直す）。
/// - Kura がこのアイテムとして書いた記録どおりのまま（大きさ・更新時刻・中身が記録と同じで、この回に書いたものでない）なら置き換える。
/// - ほか（取り込み元のファイル・利用者が直したファイル・ほかのアイテムとして書いたファイル・読めないファイル）は壊さず番号を付ける。
///
/// 書いたファイル（新しく置いた・置き換えた）は記録する。一時ファイルは書くと決めてから作る。(名前, 置き換えたか)。
fn place_recorded(cx: &PlaceCx<'_>, src: &Path, name: &str) -> Result<(String, bool)> {
    let temp = cx.dir.join(cx.temp_name);
    let mut temp_ready = false;
    let make_temp = |ready: &mut bool| -> Result<()> {
        if !*ready {
            // 前に中断したときの一時ファイル（自分の名前のもの）だけ消す
            let _ = std::fs::remove_file(&temp);
            reflink_copy::reflink_or_copy(src, &temp)?;
            *ready = true;
        }
        Ok(())
    };
    let placed = (|| {
        for n in 1..=MAX_TRIES {
            let candidate = numbered_file(name, n);
            let dst = cx.dir.join(&candidate);
            match std::fs::symlink_metadata(&dst) {
                Ok(meta) if meta.is_file() => {
                    let actual = actual_name(cx.dir, &candidate);
                    let real = cx.dir.join(&actual);
                    let record = match key_of(cx.dir_c, &actual) {
                        Some(key) => record_of(cx.library, &key)?,
                        None => None,
                    };
                    // この回にほかのアイテムとして書いたファイルには触れない
                    if record
                        .as_ref()
                        .is_some_and(|r| r.job_id == cx.job && r.item_id != cx.item_id)
                    {
                        continue;
                    }
                    let same =
                        meta.len() == cx.size && hash_file(&real).is_ok_and(|h| h == cx.hash);
                    if same {
                        if actual != candidate {
                            std::fs::rename(&real, &dst)?;
                            if record.is_some() {
                                write_record(cx, &candidate, Some(&actual))?;
                            }
                        }
                        return Ok((candidate, true));
                    }
                    let ours = record.as_ref().is_some_and(|r| {
                        r.item_id == cx.item_id
                            && r.job_id != cx.job
                            && u64::try_from(r.size).ok() == Some(meta.len())
                            && r.mtime_ns == mtime_ns(&meta)
                            && hash_file(&real).is_ok_and(|h| h.as_slice() == r.hash.as_slice())
                    });
                    if ours {
                        make_temp(&mut temp_ready)?;
                        std::fs::rename(&temp, &real)?;
                        temp_ready = false;
                        if actual != candidate {
                            std::fs::rename(&real, &dst)?;
                        }
                        write_record(cx, &candidate, Some(&actual))?;
                        return Ok((candidate, true));
                    }
                    continue;
                }
                Ok(_) => continue,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                // 読めない（許されていない）名前は、持ち主が分からないので番号を付ける
                Err(_) => continue,
            }
            make_temp(&mut temp_ready)?;
            match std::fs::hard_link(&temp, &dst) {
                Ok(()) => {
                    write_record(cx, &candidate, None)?;
                    return Ok((candidate, false));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    // ハードリンクが使えないボリューム（exFAT など）：なければ名前を変える
                    if std::fs::symlink_metadata(&dst).is_ok() {
                        continue;
                    }
                    std::fs::rename(&temp, &dst)?;
                    temp_ready = false;
                    write_record(cx, &candidate, None)?;
                    return Ok((candidate, false));
                }
            }
        }
        Err(CoreError::Invalid(format!(
            "同じ名前のファイルが多すぎます: {name}"
        )))
    })();
    let _ = std::fs::remove_file(&temp);
    placed
}

impl ExportHandler {
    /// 1 件を書き出す（名前の規則で名前を決め、形式を変えるならエンジンで描き、Kura が書いた記録どおりのファイルなら置き換える）。
    fn export_one(
        &self,
        ctx: &JobContext,
        one: &One<'_>,
        dir: &Path,
        dir_c: &Path,
    ) -> Result<Done> {
        let library = ctx.library();
        let row = item_row(library, one.item_id)?;
        if row.trashed {
            return Ok(Done {
                skipped: true,
                ..Done::default()
            });
        }
        let parts = NameParts {
            item_id: one.item_id,
            title: &row.title,
            original_name: &row.original_name,
            index: one.index,
            unit: one.unit.as_deref(),
            width: row.width,
            height: row.height,
        };
        let temp = format!(".kura-export-{}-{}.part", ctx.id(), one.cursor);
        let place = |hash: [u8; 32], size: u64| PlaceCx {
            library,
            job: ctx.id(),
            item_id: one.item_id,
            hash,
            size,
            dir,
            dir_c,
            temp_name: &temp,
        };
        match plan(
            row.kind,
            &row.ext,
            row.codec.as_deref(),
            one.options.target(row.kind),
            one.writable,
        ) {
            Plan::Copy => {
                let src = library.layout().content_path(&row.hash, &row.ext);
                let size = match std::fs::metadata(&src) {
                    Ok(m) if m.is_file() => m.len(),
                    _ => {
                        return Err(CoreError::NotFound(format!(
                            "ライブラリのファイルがありません: {}",
                            src.display()
                        )));
                    }
                };
                let name = rule_name(&one.options.name, &parts, &row.ext);
                let (name, replaced) = place_recorded(&place(row.hash, size), &src, &name)?;
                Ok(Done {
                    name,
                    replaced,
                    ..Done::default()
                })
            }
            Plan::Convert { format } => {
                let unit = ExportUnit {
                    ctx,
                    state: one.state,
                    cursor: one.cursor,
                };
                let written = match render_converted(
                    self.engine.as_ref(),
                    &unit,
                    one.item_id,
                    &row,
                    &format,
                    false,
                )? {
                    Flow::Go(w) => w,
                    Flow::Stop(end) => {
                        return Ok(Done {
                            stopped: Some(end),
                            ..Done::default()
                        });
                    }
                };
                let (hash, size, ext) = (written.out_hash(), written.out_size(), written.out_ext());
                let name = rule_name(&one.options.name, &parts, &ext);
                let placed = place_recorded(&place(hash, size), written.part(), &name);
                // staging は落として消す（置いたものは書き出し先の写し）
                drop(written);
                let (name, replaced) = placed?;
                // 形式を変えた出力の中身を記録する（照らし直しが一致させる。今と同じ中身なら要らない）
                record_output(library, one.item_id, &row, hash, &ext, size)?;
                Ok(Done {
                    name,
                    converted: true,
                    replaced,
                    ..Done::default()
                })
            }
        }
    }
}

/// 形式を変えて描く（保存の準備の段に、元＋頭の文書を出力の形式だけ変えて渡す。版を足さず、アンカーも作らない。
/// staging に書いた出力を返し、落とすと消える）。エンジンがなければ Unsupported。`interactive` なら予算を対話の枠で
/// 待たずに確保し、足りなければ断る（ドラッグアウト）。書き出しのジョブは一括の枠で待つ。
pub(super) fn render_converted(
    engine: Option<&Arc<EngineServices>>,
    unit: &dyn UnitCx,
    item_id: i64,
    row: &ItemRow,
    format: &str,
    interactive: bool,
) -> Result<Flow<crate::edit::save::Written>> {
    let engine = engine.cloned().ok_or_else(|| {
        CoreError::Unsupported(format!(
            "このライブラリでは形式を変えられません（{} → {format}）",
            row.ext
        ))
    })?;
    let library = unit.library();
    let runtime = engine.runtime()?.clone();
    let head = library
        .db()
        .read(|c| crate::edit::versions::head(c, item_id))?
        .ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id}")))?;
    let mut graph = crate::edit::batch::head_doc(library.db(), &runtime, item_id, head.version)?;
    if let Some(out) = graph.outputs.first_mut() {
        out.format = kura_engine::doc::FormatChoice::To {
            format: format.to_string(),
            options: serde_json::json!({}),
        };
    }
    Saver::new(engine).prepare_graph(
        unit,
        Mode::SaveAs,
        item_id,
        Base {
            hash: head.hash,
            version: head.version,
        },
        &graph,
        GraphExtra {
            transient: true,
            interactive,
            ..GraphExtra::default()
        },
    )
}

/// 形式を変えた出力の中身を `item_outputs` に記録する（照らし直しが一致させる。今と同じ中身なら書かない）。
pub(super) fn record_output(
    library: &Library,
    item_id: i64,
    row: &ItemRow,
    hash: [u8; 32],
    ext: &str,
    size: u64,
) -> Result<()> {
    if hash == row.hash {
        return Ok(());
    }
    let (source, ext) = (row.hash, ext.to_string());
    library.db().write(move |tx| {
        tx.prepare_cached(
            "INSERT INTO item_outputs (hash, item_id, source_hash, ext, size_bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(hash, item_id) DO NOTHING",
        )?
        .execute(rusqlite::params![
            &hash[..],
            item_id,
            &source[..],
            ext,
            i64::try_from(size).unwrap_or(i64::MAX),
            now_ms()
        ])?;
        Ok(())
    })
}

/// `src` を `dir` に `name` で置く（同じ名前があれば番号を付ける。上書きはしない。試験用：記録を持たない）。
#[cfg(test)]
pub(super) fn place_new(src: &Path, dir: &Path, name: &str, temp_name: &str) -> Result<String> {
    let temp = dir.join(temp_name);
    let _ = std::fs::remove_file(&temp);
    reflink_copy::reflink_or_copy(src, &temp)?;
    let placed = (|| {
        for n in 1..=MAX_TRIES {
            let candidate = numbered_file(name, n);
            match std::fs::hard_link(&temp, dir.join(&candidate)) {
                Ok(()) => return Ok(candidate),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(CoreError::from(err)),
            }
        }
        Err(CoreError::Invalid(format!(
            "同じ名前のファイルが多すぎます: {name}"
        )))
    })();
    let _ = std::fs::remove_file(&temp);
    placed
}

/// JSON の 1 件。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManifestRecord {
    pub file: String,
    pub original_name: String,
    pub title: String,
    pub kind: MediaKind,
    pub hash: String,
    pub size_bytes: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub rating: i64,
    pub note: Option<String>,
    /// 「名前空間:名前」または「名前」。
    pub tags: Vec<String>,
    /// フォルダの道筋（いちばん上から）。
    pub folders: Vec<Vec<String>>,
    pub units: Vec<String>,
    pub source_site: Option<String>,
    pub source_url: Option<String>,
    pub captured_at: Option<String>,
    pub imported_at: String,
}

/// JSON 全体。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest<'a> {
    app: &'static str,
    version: u32,
    exported_at: String,
    items: &'a [ManifestRecord],
}

/// 書き出したファイルの情報を読む（消えていたアイテムは飛ばす）。
pub(super) fn manifest_records(
    conn: &Connection,
    files: &[ExportedFile],
) -> Result<Vec<ManifestRecord>> {
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let found = match detail(conn, file.item_id) {
            Ok(found) => found,
            Err(CoreError::NotFound(_)) => continue,
            Err(err) => return Err(err),
        };
        let s = found.summary;
        out.push(ManifestRecord {
            file: file.path.clone(),
            original_name: found.original_name,
            title: s.title,
            kind: s.kind,
            hash: s.hash,
            size_bytes: s.size_bytes,
            width: s.width,
            height: s.height,
            duration_ms: s.duration_ms,
            rating: s.rating,
            note: found.note,
            tags: found
                .tags
                .into_iter()
                .map(|t| {
                    TagName {
                        namespace: t.namespace,
                        name: t.name,
                    }
                    .display()
                })
                .collect(),
            folders: found.folders.into_iter().map(|f| f.path).collect(),
            units: found.units.into_iter().map(|u| u.name).collect(),
            source_site: s.source_site,
            source_url: found.source_url,
            captured_at: s.captured_at.map(format_iso8601),
            imported_at: format_iso8601(s.imported_at),
        });
    }
    Ok(out)
}

/// JSON を書く（同じ名前があれば番号を付ける。上書きしない）。書いた名前を返す。
pub(super) fn write_manifest(dest: &Path, records: &[ManifestRecord]) -> Result<String> {
    let body = serde_json::to_vec_pretty(&Manifest {
        app: "Kura",
        version: 1,
        exported_at: format_iso8601(now_ms()),
        items: records,
    })?;
    for n in 1..=MAX_TRIES {
        let name = numbered_file(MANIFEST_NAME, n);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest.join(&name))
        {
            Ok(mut file) => {
                file.write_all(&body)?;
                file.sync_all()?;
                return Ok(name);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(CoreError::Invalid(
        "同じ名前の JSON が多すぎて書けませんでした".into(),
    ))
}
