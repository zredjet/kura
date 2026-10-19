//! 書き出しのジョブ（`kind = 'export'`）。

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::names::{numbered_dir, numbered_file, safe_name};
use crate::import::MediaKind;
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

/// 書き出しのジョブを積む。
pub fn start_export(jobs: &JobManager, input: &ExportInput) -> Result<JobId> {
    if input.entries.is_empty() {
        return Err(CoreError::Invalid("書き出す項目を選んでください".into()));
    }
    jobs.enqueue(JobKind::Export, serde_json::to_string(input)?)
}

/// 書き出しのジョブの処理。
#[derive(Debug, Default)]
pub struct ExportHandler;

/// 書き出しのまとまり（ばらのアイテムは `folder` なし、ユニットはユニット名のフォルダ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub folder: Option<String>,
    pub items: Vec<i64>,
}

/// 選んだものをまとまりにする：ばらのアイテム（重ねて選んだものは 1 回）、続いてユニットごと。
/// 見つからないユニットは飛ばす。
pub(super) fn resolve(conn: &Connection, entries: &[i64]) -> Result<Vec<Group>> {
    let mut loose = Group {
        folder: None,
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
        let groups = library.db().read(|conn| resolve(conn, &input.entries))?;
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
        let mut saved = Instant::now();
        while cursor < total {
            match ctx.control() {
                Control::Continue => {}
                Control::Pause => {
                    save(ctx, library, &state)?;
                    return Ok(JobEnd::Paused);
                }
                Control::Cancel => {
                    // 書いたファイルはそのまま残す（書き出し先は消さない）
                    state.files.clear();
                    state.folders.clear();
                    return Ok(JobEnd::Canceled {
                        result_json: Some(serde_json::to_string(&state)?),
                    });
                }
            }
            let (g, item_id) = flat[cursor];
            let outcome = folder_for(&input.dest, &groups[g], &mut state.folders[g]).and_then(
                |(dir, rel)| {
                    export_one(library, item_id, &dir, ctx.id(), cursor).map(|name| match rel {
                        Some(folder) => format!("{folder}/{name}"),
                        None => name,
                    })
                },
            );
            match outcome {
                Ok(path) => {
                    state.exported += 1;
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

        let records = library
            .db()
            .read(|conn| manifest_records(conn, &state.files))?;
        let manifest = write_manifest(&input.dest, &records)?;
        state.manifest = Some(manifest);
        state.files.clear();
        state.folders.clear();
        let message = format!("{} 件を書き出しました", state.exported);
        Ok(JobEnd::Done {
            result_json: serde_json::to_string(&state)?,
            message: Some(message),
        })
    }
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

/// まとまりの書き出し先のフォルダ（ユニットはフォルダを作る。同じ名前のものがあれば番号を付ける）。
/// 書き出し先からの相対の名前も返す。
fn folder_for(
    dest: &Path,
    group: &Group,
    chosen: &mut Option<String>,
) -> Result<(PathBuf, Option<String>)> {
    let Some(name) = &group.folder else {
        return Ok((dest.to_path_buf(), None));
    };
    if let Some(existing) = chosen {
        return Ok((dest.join(&*existing), Some(existing.clone())));
    }
    for n in 1..=MAX_TRIES {
        let candidate = numbered_dir(name, n);
        // 作れたら自分のフォルダ（あれば次の番号。既存のフォルダには書かない）
        match std::fs::create_dir(dest.join(&candidate)) {
            Ok(()) => {
                *chosen = Some(candidate.clone());
                return Ok((dest.join(&candidate), Some(candidate)));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(CoreError::Invalid(format!(
        "同じ名前のフォルダが多すぎます: {name}"
    )))
}

/// 1 件を書き出す（書き出した名前）。
fn export_one(
    library: &Library,
    item_id: i64,
    dir: &Path,
    job: JobId,
    index: usize,
) -> Result<String> {
    let row = library.db().read(|conn| {
        let mut stmt =
            conn.prepare_cached("SELECT hash, ext, original_name, title FROM items WHERE id = ?1")?;
        Ok(stmt
            .query_row([item_id], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .optional()?)
    })?;
    let Some((hash, ext, original_name, title)) = row else {
        return Err(CoreError::NotFound(format!("アイテム {item_id}")));
    };
    let hash: [u8; 32] = hash
        .try_into()
        .map_err(|_| CoreError::Invalid(format!("アイテム {item_id} のハッシュが壊れています")))?;
    let src = library.layout().content_path(&hash, &ext);
    if !src.is_file() {
        return Err(CoreError::NotFound(format!(
            "ライブラリのファイルがありません: {}",
            src.display()
        )));
    }
    let fallback = format!("{}.{ext}", safe_name(&title, &format!("item-{item_id}")));
    let name = safe_name(&original_name, &fallback);
    place_new(
        &src,
        dir,
        &name,
        &format!(".kura-export-{job}-{index}.part"),
    )
}

/// `src` を `dir` に `name` で置く（同じ名前があれば番号を付ける）。上書きはしない：
/// 一時ファイルにコピーしてから、すでにあれば失敗するハードリンクで名前を付ける。
pub(super) fn place_new(src: &Path, dir: &Path, name: &str, temp_name: &str) -> Result<String> {
    let temp = dir.join(temp_name);
    // 前に中断したときの一時ファイル（自分の名前のもの）だけ消す
    let _ = std::fs::remove_file(&temp);
    reflink_copy::reflink_or_copy(src, &temp)?;
    let placed = (|| {
        for n in 1..=MAX_TRIES {
            let candidate = numbered_file(name, n);
            let dst = dir.join(&candidate);
            match std::fs::hard_link(&temp, &dst) {
                Ok(()) => return Ok(candidate),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    // ハードリンクが使えないボリューム（exFAT など）：なければ名前を変える
                    if std::fs::symlink_metadata(&dst).is_ok() {
                        continue;
                    }
                    std::fs::rename(&temp, &dst)?;
                    return Ok(candidate);
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
