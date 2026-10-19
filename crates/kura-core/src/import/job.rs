//! 取り込みジョブ（docs/architecture.md §6）。
//!
//! 500 件（または 1GB）ずつ「コピー・ハッシュ・配置」を並列で行い、その結果を 1 つのトランザクションで DB に入れる。
//! 同じトランザクションで「どこまで終わったか」を jobs.result_json に保存するので、
//! 強制終了・一時停止・中止のどの場合も、続きはそこから再開できる。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::kind::mime_for;
use super::place::{place, stage};
use super::plan::{
    Assignment, DuplicatePolicy, FolderTarget, ImportOptions, TagName, assign, title_from_name,
};
use super::scan::{ScannedFile, scan};
use super::store::{
    NewItem, add_tag, add_to_folder, add_unit_member, create_import_unit, ensure_folder_path,
    ensure_tag, find_by_hash, folder_exists, insert_item, restore_item, unit_exists,
};
use crate::jobs::{
    Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager, save_partial,
};
use crate::library::Layout;
use crate::media::{MediaBackend, MediaInfo, NoBackend};
use crate::time::now_ms;
use crate::{CoreError, Result};

/// 1 回のトランザクションで入れるファイル数。
pub const BATCH_FILES: usize = 500;
/// 1 回のトランザクションで扱う合計サイズ（大きな動画が多いときに区切りを短くする）。
pub const BATCH_BYTES: u64 = 1 << 30;
/// 結果に残す失敗の明細の上限（全件はジョブのログにある）。
pub const MAX_FAILURE_DETAILS: usize = 1000;

/// コピー・ハッシュ・配置を並列に行う数の上限。APFS のクローンと rename は並列にしすぎると
/// かえって遅くなる（2 万件・16 コアで、6 並列 3.6 秒、16 並列 11 秒。docs/decisions.md）。
pub const MAX_IMPORT_THREADS: usize = 6;

/// 取り込みジョブの入力（jobs.input_json）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportInput {
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub options: ImportOptions,
}

/// 取り込みに失敗したファイル。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFailure {
    pub path: String,
    pub reason: String,
}

/// 取り込みの結果・途中経過（jobs.result_json）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportResult {
    /// 処理し終えた候補の数（再開位置）。
    pub cursor: u64,
    /// 候補の数。
    pub total: u64,
    /// 新しく追加した数。
    pub added: u64,
    /// 内容が同一のものがすでにあった数。
    pub duplicates: u64,
    /// 対応していない形式・空のファイルの数。
    pub skipped: u64,
    /// 失敗した数。
    pub failed: u64,
    /// 失敗の明細（上限あり）。
    pub failures: Vec<ImportFailure>,
    /// 拡張子は対応しているが中身が違うので取り込まなかったもの（`.ase` の色見本など。`skipped` に含む）と理由（上限あり）。
    pub rejected: Vec<ImportFailure>,
    /// 作ったユニット（末端フォルダ → unit_id）。再開のときに同じユニットへ足すため。
    pub units: BTreeMap<String, i64>,
}

/// 取り込みジョブの処理。
pub struct ImportHandler {
    batch_files: usize,
    batch_bytes: u64,
    /// コピー・ハッシュ・配置の並列数。
    threads: usize,
    /// メディア情報（幅・高さ・長さ・撮影日時など）を読む実装。
    backend: Arc<dyn MediaBackend>,
    /// テスト用：この番号のバッチを配置したところで、DB に入れずに失敗する（強制終了の再現）。
    #[cfg(test)]
    abort_before_commit: Option<usize>,
}

impl ImportHandler {
    /// メディア情報を `backend` で読む取り込み。
    pub fn new(backend: Arc<dyn MediaBackend>) -> Self {
        Self {
            batch_files: BATCH_FILES,
            batch_bytes: BATCH_BYTES,
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(MAX_IMPORT_THREADS),
            backend,
            #[cfg(test)]
            abort_before_commit: None,
        }
    }

    /// 並列数を変える（計測用）。
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }
}

impl Default for ImportHandler {
    /// メディア情報を読まない取り込み（テスト用。読めなかった分はサムネイルのジョブが埋める）。
    fn default() -> Self {
        Self::new(Arc::new(NoBackend))
    }
}

impl JobHandler for ImportHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let input: ImportInput = serde_json::from_str(ctx.input())?;
        let mut state: ImportResult = match ctx.partial() {
            Some(json) => serde_json::from_str(json)?,
            None => ImportResult::default(),
        };
        let library = ctx.library();
        let layout = library.layout().clone();

        let scan = scan(&input.paths);
        for (path, reason) in &scan.errors {
            ctx.log(&format!("読めませんでした: {} ({reason})", path.display()));
        }
        let mut options = input.options;
        if let Some(dest) = options.dest_folder_id
            && !library.db().read(|conn| folder_exists(conn, dest))?
        {
            ctx.log("取り込み先のフォルダが見つからないため、指定なしとして続けます");
            options.dest_folder_id = None;
        }
        let plan = assign(&scan, &options);
        let files = scan.files;
        let total = files.len();
        state.total = total as u64;
        state.skipped = scan.skipped;
        state.rejected.clear();
        for (path, reason) in scan.rejected.iter().take(MAX_FAILURE_DETAILS) {
            ctx.log(&format!("取り込みません: {} ({reason})", path.display()));
            state.rejected.push(ImportFailure {
                path: path.to_string_lossy().into_owned(),
                reason: reason.clone(),
            });
        }

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.threads)
            .thread_name(|i| format!("kura-import-{i}"))
            .build()
            .map_err(|err| CoreError::Invalid(format!("取り込みのスレッドを作れません: {err}")))?;

        let mut cursor = usize::try_from(state.cursor).unwrap_or(0).min(total);
        if cursor > 0 {
            ctx.log(&format!("{cursor} 件目から再開します"));
            ctx.progress(fraction(cursor, total), &progress_message(cursor, total));
        }

        #[cfg(test)]
        let mut batch_index = 0usize;
        while cursor < total {
            match ctx.control() {
                Control::Continue => {}
                Control::Pause => return Ok(JobEnd::Paused),
                Control::Cancel => {
                    return Ok(JobEnd::Canceled {
                        result_json: Some(serde_json::to_string(&state)?),
                    });
                }
            }
            let end = batch_end(&files, cursor, self.batch_files, self.batch_bytes);
            let job_id = ctx.id();
            // files/ に置いてから DB に書き終えるまで、ゴミ箱を空にする処理を待たせる（同じ中身を消されないように）
            let ingest = library.ingest_guard();
            // コピー・ハッシュ・配置・メディア情報の読み取りは並列に（DB には触らない）
            let backend = self.backend.as_ref();
            let outcomes: Vec<(usize, PlaceOutcome)> = pool.install(|| {
                (cursor..end)
                    .into_par_iter()
                    .map(|i| {
                        let file = &files[i];
                        let staging = layout
                            .staging_dir()
                            .join(format!("import-{job_id}-{i}.{}", file.ext));
                        let outcome = stage_and_place(&layout, backend, file, &staging)
                            .map_err(|err| err.to_string());
                        (i, outcome)
                    })
                    .collect()
            });

            #[cfg(test)]
            {
                if self.abort_before_commit == Some(batch_index) {
                    return Err(CoreError::Invalid("テスト用の中断".into()));
                }
                batch_index += 1;
            }

            let mut entries = Vec::with_capacity(outcomes.len());
            for (index, outcome) in outcomes {
                if let Err(reason) = &outcome {
                    ctx.log(&format!("失敗: {} ({reason})", files[index].path.display()));
                }
                entries.push(BatchEntry {
                    index,
                    file: files[index].clone(),
                    assignment: plan[index].clone(),
                    outcome,
                });
            }
            let commit = BatchCommit {
                job_id,
                entries,
                state: state.clone(),
                end,
                total,
                policy: options.duplicates,
                file_info_tags: options.file_info_tags,
                layout: layout.clone(),
            };
            let added_before = state.added;
            let (next, extra_files) = library.db().write(move |tx| commit.apply(tx))?;
            // 新しく入ったアイテムのサムネイルを作る（取り込みと並行して進める）
            if next.added > added_before {
                ctx.ensure_queued(JobKind::Thumbnail, "{}")?;
                // 動画・音楽は再生できるかを確かめ、要ればプロキシを作る
                ctx.ensure_queued(JobKind::Proxy, "{}")?;
            }
            // 既存と同じ中身で拡張子だけ違ったコピーは、どのアイテムも使わないので消す
            for path in extra_files {
                if let Err(err) = std::fs::remove_file(&path) {
                    tracing::warn!(%err, path = %path.display(), "不要なコピーを消せませんでした");
                }
            }
            drop(ingest);
            state = next;
            cursor = end;
            ctx.progress(fraction(cursor, total), &progress_message(cursor, total));
        }

        let message = format!(
            "{} 項目を追加（重複 {}・失敗 {}）",
            state.added, state.duplicates, state.failed
        );
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: serde_json::to_string(&state)?,
            message: Some(message),
        })
    }
}

/// 1 ファイルを配置した結果。
struct Placed {
    hash: [u8; 32],
    size: u64,
    /// 読めたメディア情報（読めなければサムネイルのジョブがあとで読む）。
    info: Option<MediaInfo>,
}

/// 1 ファイルを配置した結果、または失敗の理由。
type PlaceOutcome = std::result::Result<Placed, String>;

/// 1 ファイル分の配置結果と所属。
struct BatchEntry {
    index: usize,
    file: ScannedFile,
    assignment: Assignment,
    outcome: PlaceOutcome,
}

/// 1 バッチ分の DB への書き込み。
struct BatchCommit {
    job_id: JobId,
    entries: Vec<BatchEntry>,
    state: ImportResult,
    end: usize,
    total: usize,
    policy: DuplicatePolicy,
    file_info_tags: bool,
    layout: Layout,
}

impl BatchCommit {
    /// アイテムと所属を入れ、途中経過を保存する。消してよい余分なファイルも返す。
    fn apply(self, tx: &Connection) -> Result<(ImportResult, Vec<PathBuf>)> {
        let now = now_ms();
        let mut state = self.state;
        let mut extra_files = Vec::new();
        let mut folders: HashMap<FolderTarget, Option<i64>> = HashMap::new();
        let mut tags: HashMap<TagName, i64> = HashMap::new();

        for entry in self.entries {
            let file = &entry.file;
            let placed = match entry.outcome {
                Ok(placed) => placed,
                Err(reason) => {
                    state.failed += 1;
                    if state.failures.len() < MAX_FAILURE_DETAILS {
                        state.failures.push(ImportFailure {
                            path: file.path.to_string_lossy().into_owned(),
                            reason,
                        });
                    }
                    continue;
                }
            };
            let hash = placed.hash;
            let item_id = match find_by_hash(tx, &hash)? {
                Some(existing) => {
                    state.duplicates += 1;
                    if existing.ext != file.ext {
                        extra_files.push(self.layout.content_path(&hash, &file.ext));
                    }
                    if self.policy == DuplicatePolicy::Ignore {
                        continue;
                    }
                    if existing.trashed {
                        restore_item(tx, existing.id, now)?;
                    }
                    existing.id
                }
                None => {
                    let id = insert_item(
                        tx,
                        &NewItem {
                            hash: &hash,
                            ext: &file.ext,
                            kind: file.kind,
                            mime: mime_for(&file.ext),
                            size: placed.size,
                            title: &title_from_name(&file.name),
                            original_name: &file.name,
                            original_path: &file.path.to_string_lossy(),
                            info: placed.info.as_ref(),
                            source_site: "local",
                            source_url: None,
                            captured_at: None,
                            now,
                        },
                    )?;
                    state.added += 1;
                    id
                }
            };

            if let Some(target) = &entry.assignment.folder {
                let folder = match folders.get(target) {
                    Some(folder) => *folder,
                    None => {
                        let folder = ensure_folder_path(tx, target.base, &target.path, now)?;
                        folders.insert(target.clone(), folder);
                        folder
                    }
                };
                if let Some(folder) = folder {
                    add_to_folder(tx, folder, item_id)?;
                }
            }
            let info_tags = if self.file_info_tags {
                placed.info.as_ref().map(info_tags).unwrap_or_default()
            } else {
                Vec::new()
            };
            for tag in entry.assignment.tags.iter().chain(&info_tags) {
                let tag_id = match tags.get(tag) {
                    Some(id) => *id,
                    None => {
                        let id = ensure_tag(tx, tag, now)?;
                        tags.insert(tag.clone(), id);
                        id
                    }
                };
                add_tag(tx, tag_id, item_id, "import")?;
            }
            if let Some(key) = &entry.assignment.unit {
                let storage_key = key.storage_key();
                let unit_id = match state.units.get(&storage_key).copied() {
                    Some(id) if unit_exists(tx, id)? => id,
                    _ => {
                        let id = create_import_unit(tx, key.name(), now)?;
                        state.units.insert(storage_key, id);
                        id
                    }
                };
                add_unit_member(tx, unit_id, item_id, entry.index as f64)?;
            }
        }

        state.cursor = self.end as u64;
        save_partial(
            tx,
            self.job_id,
            &serde_json::to_string(&state)?,
            fraction(self.end, self.total),
            &progress_message(self.end, self.total),
        )?;
        Ok((state, extra_files))
    }
}

/// staging にコピーしてハッシュを取り、files/ に置いてからメディア情報を読む。
fn stage_and_place(
    layout: &Layout,
    backend: &dyn MediaBackend,
    file: &ScannedFile,
    staging: &Path,
) -> Result<Placed> {
    let staged = stage(&file.path, staging)?;
    let final_path = layout.content_path(&staged.hash, &file.ext);
    place(&staged, &final_path)?;
    let info = backend.probe(&final_path, file.kind).ok();
    Ok(Placed {
        hash: staged.hash,
        size: staged.size,
        info,
    })
}

/// 「ファイルの情報からタグ」：音楽のアーティストとアルバム（docs/decisions.md ADR-30）。
fn info_tags(info: &MediaInfo) -> Vec<TagName> {
    [("アーティスト", &info.artist), ("アルバム", &info.album)]
        .into_iter()
        .filter_map(|(namespace, value)| {
            let name = crate::text::nfc(value.as_deref()?.trim());
            (!name.is_empty()).then(|| TagName {
                namespace: namespace.to_string(),
                name,
            })
        })
        .collect()
}

/// `start` から、ファイル数か合計サイズの上限に達するまでを 1 バッチにする（最低 1 件）。
fn batch_end(files: &[ScannedFile], start: usize, max_files: usize, max_bytes: u64) -> usize {
    let mut end = start;
    let mut bytes = 0u64;
    while end < files.len() && end - start < max_files.max(1) {
        bytes += files[end].size;
        end += 1;
        if bytes >= max_bytes {
            break;
        }
    }
    end
}

fn fraction(done: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        done as f64 / total as f64
    }
}

fn progress_message(done: usize, total: usize) -> String {
    format!("{done} / {total} 項目")
}

/// 取り込みジョブを積む。
pub fn start_import(jobs: &JobManager, input: &ImportInput) -> Result<JobId> {
    if input.paths.is_empty() {
        return Err(CoreError::Invalid("取り込むファイルがありません".into()));
    }
    jobs.enqueue(JobKind::Import, serde_json::to_string(input)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::place::hash_file;
    use crate::jobs::{EventSink, JobEvent, JobRecord, JobState};
    use crate::library::{Library, check_files, clean_staging};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    const WAIT: Duration = Duration::from_secs(30);

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// 取り込み元の全ファイルの (内容のハッシュ, サイズ, 更新日時)。
    fn snapshot(root: &Path) -> HashMap<PathBuf, ([u8; 32], u64, SystemTime)> {
        walkdir::WalkDir::new(root)
            .into_iter()
            .map(Result::unwrap)
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                let meta = e.metadata().unwrap();
                (
                    e.path().to_path_buf(),
                    (
                        hash_file(e.path()).unwrap(),
                        meta.len(),
                        meta.modified().unwrap(),
                    ),
                )
            })
            .collect()
    }

    struct Env {
        dir: tempfile::TempDir,
        library: Arc<Library>,
        events: Arc<Mutex<Vec<JobEvent>>>,
    }

    impl Env {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let library = Arc::new(Library::create(&dir.path().join("L")).unwrap());
            Self {
                dir,
                library,
                events: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn src(&self) -> PathBuf {
            self.dir.path().join("src")
        }

        fn manager(&self, handler: ImportHandler) -> JobManager {
            let events = self.events.clone();
            let sink: EventSink = Arc::new(move |e| events.lock().unwrap().push(e));
            JobManager::new(
                self.library.clone(),
                vec![(JobKind::Import, Arc::new(handler))],
                sink,
            )
            .unwrap()
        }

        /// 取り込みを最後まで動かして結果を返す。
        fn import(&self, paths: Vec<PathBuf>, options: ImportOptions) -> (JobRecord, ImportResult) {
            let jobs = self.manager(ImportHandler::default());
            let id = start_import(&jobs, &ImportInput { paths, options }).unwrap();
            jobs.start().unwrap();
            assert!(jobs.wait_idle(WAIT));
            let record = jobs.get(id).unwrap();
            let result = serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap();
            (record, result)
        }

        fn count(&self, sql: &'static str) -> i64 {
            self.library
                .db()
                .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
                .unwrap()
        }

        /// DB のすべてのアイテムの実ファイルがあり、中身がハッシュと一致すること。
        fn assert_files_match_db(&self) {
            let rows: Vec<(Vec<u8>, String)> = self
                .library
                .db()
                .read(|c| {
                    let mut stmt = c.prepare("SELECT hash, ext FROM items")?;
                    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    Ok(rows.collect::<std::result::Result<_, _>>()?)
                })
                .unwrap();
            for (hash, ext) in rows {
                let hash: [u8; 32] = hash.try_into().unwrap();
                let path = self.library.layout().content_path(&hash, &ext);
                assert_eq!(hash_file(&path).unwrap(), hash, "{}", path.display());
            }
            assert_eq!(
                check_files(self.library.db(), self.library.layout()).unwrap(),
                (0, 0)
            );
            let staging = std::fs::read_dir(self.library.layout().staging_dir())
                .unwrap()
                .count();
            assert_eq!(staging, 0, "staging に残骸がある");
        }
    }

    /// 完了条件：1,000 ファイル（重複 50 件）を取り込むと、重複はスキップ、残りはコピーされ、進捗が出る。
    /// 取り込み元のファイルは変化しない。
    #[test]
    fn imports_1000_files_with_50_duplicates() {
        let env = Env::new();
        // 先にライブラリへ入れておく 25 件
        let pre = env.dir.path().join("pre");
        for i in 0..25 {
            write(
                &pre.join(format!("p{i}.jpg")),
                format!("shared-{i}").as_bytes(),
            );
        }
        let (_, first) = env.import(vec![pre], ImportOptions::default());
        assert_eq!(first.added, 25);

        // 1,000 ファイル = 新規 950 + ライブラリと同じ 25 + 取り込みの中で重なる 25
        let src = env.src();
        for i in 0..950 {
            let sub = format!("sub{}", i % 7);
            write(
                &src.join(sub).join(format!("f{i}.png")),
                format!("unique-{i}").as_bytes(),
            );
        }
        for i in 0..25 {
            write(
                &src.join(format!("again{i}.jpg")),
                format!("shared-{i}").as_bytes(),
            );
            write(
                &src.join(format!("copy{i}.png")),
                format!("unique-{i}").as_bytes(),
            );
        }
        let before = snapshot(&src);
        assert_eq!(before.len(), 1000);

        let (record, result) = env.import(vec![src.clone()], ImportOptions::default());
        assert_eq!(record.state, JobState::Done);
        assert_eq!(result.total, 1000);
        assert_eq!(result.cursor, 1000);
        assert_eq!(result.added, 950);
        assert_eq!(result.duplicates, 50);
        assert_eq!(result.failed, 0);
        assert_eq!(env.count("SELECT COUNT(*) FROM items"), 975);

        // 取り込み元は変化していない
        assert_eq!(snapshot(&src), before);
        env.assert_files_match_db();

        // 進捗：単調に増えて、最後は完了
        let events = env.events.lock().unwrap();
        let ours: Vec<&JobEvent> = events.iter().filter(|e| e.id == record.id).collect();
        let progress: Vec<f64> = ours
            .iter()
            .filter(|e| e.state == JobState::Running)
            .filter_map(|e| e.progress)
            .collect();
        assert!(!progress.is_empty());
        assert!(progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
        let last = ours.last().unwrap();
        assert_eq!(last.state, JobState::Done);
        assert!(last.result_json.is_some());
    }

    #[test]
    fn options_create_folders_tags_and_units() {
        let env = Env::new();
        let dest = env
            .library
            .db()
            .write(|tx| {
                crate::import::store::ensure_folder_path(
                    tx,
                    None,
                    &["写真".to_string(), "2026 旅行".to_string()],
                    0,
                )
            })
            .unwrap()
            .unwrap();
        let trip = env.src().join("旅行2026");
        write(&trip.join("top.jpg"), b"top");
        write(&trip.join("鎌倉/img10.jpg"), b"k10");
        write(&trip.join("鎌倉/img2.jpg"), b"k2");
        write(&trip.join("江ノ島/e.jpg"), b"e");

        let options = ImportOptions {
            dest_folder_id: Some(dest),
            reproduce_folders: true,
            folder_names_as_tags: true,
            units_per_folder: true,
            file_info_tags: false,
            tags: vec!["旅行".into(), "撮影者:わたし".into()],
            duplicates: DuplicatePolicy::AddMemberships,
        };
        let (_, result) = env.import(vec![trip], options);
        assert_eq!(result.added, 4);

        let folder_of = |title: &'static str| -> String {
            env.library
                .db()
                .read(move |c| {
                    Ok(c.query_row(
                        "SELECT group_concat(f.name, '/') FROM items i
                         JOIN item_folders m ON m.item_id = i.id JOIN folders f ON f.id = m.folder_id
                         WHERE i.title = ?1",
                        [title],
                        |r| r.get(0),
                    )?)
                })
                .unwrap()
        };
        // 旅行2026 → 写真/2026 旅行、旅行2026/鎌倉 → 写真/2026 旅行/鎌倉
        assert_eq!(folder_of("top"), "2026 旅行");
        assert_eq!(folder_of("img2"), "鎌倉");
        let kamakura_parent: String = env
            .library
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT p.name FROM folders f JOIN folders p ON p.id = f.parent_id WHERE f.name = '鎌倉'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(kamakura_parent, "2026 旅行");

        let tags: String = env
            .library
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT group_concat(CASE t.namespace WHEN '' THEN t.name ELSE t.namespace || ':' || t.name END, ',')
                     FROM (SELECT t.* FROM items i JOIN item_tags m ON m.item_id = i.id
                           JOIN tags t ON t.id = m.tag_id WHERE i.title = 'img2' ORDER BY t.namespace, t.name) t",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(tags, "旅行,旅行2026,鎌倉,撮影者:わたし");

        // 末端フォルダ（鎌倉・江ノ島）ごとにユニット。並びは自然順（img2 → img10）
        assert_eq!(
            env.count("SELECT COUNT(*) FROM units WHERE origin = 'import'"),
            2
        );
        let members: String = env
            .library
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT group_concat(title, ',') FROM (
                       SELECT i.title FROM units u JOIN unit_items m ON m.unit_id = u.id
                       JOIN items i ON i.id = m.item_id WHERE u.name = '鎌倉' ORDER BY m.pos)",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(members, "img2,img10");
        assert_eq!(result.units.len(), 2);
    }

    #[test]
    fn duplicate_policy_controls_memberships_and_trash() {
        let env = Env::new();
        let first = env.dir.path().join("first");
        write(&first.join("a.jpg"), b"same");
        write(&first.join("b.jpg"), b"other");
        env.import(vec![first], ImportOptions::default());
        // a をゴミ箱へ
        env.library
            .db()
            .write(|tx| {
                tx.execute("UPDATE items SET trashed_at = 1 WHERE title = 'a'", [])?;
                Ok(())
            })
            .unwrap();

        let again = env.dir.path().join("again");
        write(&again.join("a2.jpg"), b"same");
        write(&again.join("b2.jpg"), b"other");

        // 何もしない：タグも付かず、ゴミ箱のまま
        let ignore = ImportOptions {
            tags: vec!["二回目".into()],
            duplicates: DuplicatePolicy::Ignore,
            ..Default::default()
        };
        let (_, result) = env.import(vec![again.clone()], ignore);
        assert_eq!((result.added, result.duplicates), (0, 2));
        assert_eq!(env.count("SELECT COUNT(*) FROM item_tags"), 0);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items WHERE trashed_at IS NOT NULL"),
            1
        );

        // フォルダ・タグは追加：タグが付き、ゴミ箱から戻る
        let add = ImportOptions {
            tags: vec!["二回目".into()],
            ..Default::default()
        };
        let (_, result) = env.import(vec![again], add);
        assert_eq!((result.added, result.duplicates), (0, 2));
        assert_eq!(env.count("SELECT COUNT(*) FROM item_tags"), 2);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items WHERE trashed_at IS NOT NULL"),
            0
        );
        // 元の名前は最初のまま
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items WHERE original_name = 'a.jpg'"),
            1
        );
    }

    #[test]
    fn same_content_with_another_extension_leaves_no_orphan() {
        let env = Env::new();
        let src = env.src();
        write(&src.join("a.jpg"), b"same bytes");
        write(&src.join("b.jpeg"), b"same bytes");
        let (_, result) = env.import(vec![src], ImportOptions::default());
        assert_eq!((result.added, result.duplicates), (1, 1));
        env.assert_files_match_db();
    }

    #[test]
    fn pause_and_resume_continue_from_cursor() {
        let env = Env::new();
        let src = env.src();
        for i in 0..60 {
            write(
                &src.join(format!("f{i}.jpg")),
                format!("content-{i}").as_bytes(),
            );
        }
        let before = snapshot(&src);
        let handler = ImportHandler {
            batch_files: 10,
            ..ImportHandler::default()
        };
        let events = env.events.clone();
        let slot: Arc<std::sync::OnceLock<Arc<JobManager>>> = Arc::new(std::sync::OnceLock::new());
        let slot2 = slot.clone();
        // 最初の進捗が出たら一時停止する
        let sink: EventSink = Arc::new(move |e: JobEvent| {
            if e.state == JobState::Running
                && e.progress.is_some_and(|p| p > 0.0)
                && let Some(jobs) = slot2.get()
            {
                jobs.pause(e.id).unwrap();
            }
            events.lock().unwrap().push(e);
        });
        let jobs = Arc::new(
            JobManager::new(
                env.library.clone(),
                vec![(JobKind::Import, Arc::new(handler))],
                sink,
            )
            .unwrap(),
        );
        let _ = slot.set(jobs.clone());
        let id = start_import(
            &jobs,
            &ImportInput {
                paths: vec![src.clone()],
                options: ImportOptions::default(),
            },
        )
        .unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(WAIT));

        let paused = jobs.get(id).unwrap();
        assert_eq!(paused.state, JobState::Paused);
        let partial: ImportResult =
            serde_json::from_str(paused.result_json.as_deref().unwrap()).unwrap();
        assert!(
            partial.cursor > 0 && partial.cursor < 60,
            "cursor = {}",
            partial.cursor
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items") as u64,
            partial.added
        );

        // 再開すると残りだけを処理する（二重に数えない）
        let jobs2 = env.manager(ImportHandler {
            batch_files: 10,
            ..ImportHandler::default()
        });
        jobs2.resume(id).unwrap();
        jobs2.start().unwrap();
        assert!(jobs2.wait_idle(WAIT));
        let done = jobs2.get(id).unwrap();
        assert_eq!(done.state, JobState::Done);
        let result: ImportResult =
            serde_json::from_str(done.result_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            (result.added, result.duplicates, result.cursor),
            (60, 0, 60)
        );
        assert_eq!(snapshot(&src), before);
        env.assert_files_match_db();
    }

    /// 完了条件：途中で強制終了しても、次に開いたときに壊れていない（続きから取り込まれる）。
    #[test]
    fn crash_before_commit_recovers_on_reopen() {
        let env = Env::new();
        let src = env.src();
        for i in 0..50 {
            write(
                &src.join(format!("f{i}.jpg")),
                format!("content-{i}").as_bytes(),
            );
        }
        let before = snapshot(&src);

        // 3 つめのバッチを配置したところで DB に入れずに止まる
        let crashing = ImportHandler {
            batch_files: 10,
            abort_before_commit: Some(2),
            ..ImportHandler::default()
        };
        let jobs = env.manager(crashing);
        let id = start_import(
            &jobs,
            &ImportInput {
                paths: vec![src.clone()],
                options: ImportOptions::default(),
            },
        )
        .unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(WAIT));
        drop(jobs);
        // 強制終了の状態にする：ジョブは running のまま、staging に書きかけ、files に DB にないファイル
        env.library
            .db()
            .write(move |tx| {
                tx.execute("UPDATE jobs SET state = 'running' WHERE id = ?1", [id])?;
                Ok(())
            })
            .unwrap();
        write(
            &env.library.layout().staging_dir().join("import-1-999.jpg"),
            b"half",
        );
        assert_eq!(env.count("SELECT COUNT(*) FROM items"), 20);
        let (_, orphans) = check_files(env.library.db(), env.library.layout()).unwrap();
        assert_eq!(orphans, 10);

        // 開き直す：staging を掃除し、running を queued に戻して再開する
        assert_eq!(clean_staging(env.library.layout(), &[]).unwrap(), 1);
        let jobs = env.manager(ImportHandler {
            batch_files: 10,
            ..ImportHandler::default()
        });
        assert_eq!(jobs.get(id).unwrap().state, JobState::Queued);
        jobs.start().unwrap();
        assert!(jobs.wait_idle(WAIT));
        let record = jobs.get(id).unwrap();
        assert_eq!(record.state, JobState::Done);
        let result: ImportResult =
            serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap();
        assert_eq!((result.added, result.duplicates, result.failed), (50, 0, 0));
        assert_eq!(env.count("SELECT COUNT(*) FROM items"), 50);
        assert_eq!(snapshot(&src), before);
        env.assert_files_match_db();
    }

    #[test]
    fn cancel_keeps_committed_batches() {
        let env = Env::new();
        let src = env.src();
        for i in 0..40 {
            write(
                &src.join(format!("f{i}.jpg")),
                format!("content-{i}").as_bytes(),
            );
        }
        let jobs = env.manager(ImportHandler {
            batch_files: 10,
            ..ImportHandler::default()
        });
        let id = start_import(
            &jobs,
            &ImportInput {
                paths: vec![src],
                options: ImportOptions::default(),
            },
        )
        .unwrap();
        jobs.cancel(id).unwrap();
        assert_eq!(jobs.get(id).unwrap().state, JobState::Canceled);
        assert_eq!(env.count("SELECT COUNT(*) FROM items"), 0);
        env.assert_files_match_db();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_are_reported_and_others_imported() {
        use std::os::unix::fs::PermissionsExt;
        let env = Env::new();
        let src = env.src();
        write(&src.join("ok.jpg"), b"ok");
        let locked = src.join("locked.jpg");
        write(&locked, b"locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let (record, result) = env.import(vec![src], ImportOptions::default());
        assert_eq!(record.state, JobState::Done);
        assert_eq!((result.added, result.failed), (1, 1));
        assert_eq!(result.failures[0].path, locked.to_string_lossy());
        let log = std::fs::read_to_string(record.log_path.unwrap()).unwrap();
        assert!(log.contains("locked.jpg"), "{log}");

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        env.assert_files_match_db();
    }

    #[cfg(unix)]
    #[test]
    fn read_only_sources_import_and_stay_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let env = Env::new();
        let src = env.src();
        write(&src.join("sub/a.jpg"), b"a");
        write(&src.join("b.jpg"), b"b");
        for path in [src.join("sub/a.jpg"), src.join("b.jpg")] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
        for dir in [src.join("sub"), src.clone()] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        let before = snapshot(&src);
        let (_, result) = env.import(vec![src.clone()], ImportOptions::default());
        assert_eq!(result.added, 2);
        assert_eq!(snapshot(&src), before);
        // 後片付けのために書き込みを戻す
        for dir in [src.clone(), src.join("sub")] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn probes_media_and_tags_from_file_info() {
        let env = Env::new();
        let src = env.src();
        write(&src.join("photo.jpg"), b"photo");
        write(&src.join("song.flac"), b"song");
        let handler = ImportHandler::new(Arc::new(crate::media::fake::FakeBackend));
        let jobs = env.manager(handler);
        let options = ImportOptions {
            file_info_tags: true,
            ..ImportOptions::default()
        };
        let id = start_import(
            &jobs,
            &ImportInput {
                paths: vec![src],
                options,
            },
        )
        .unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(WAIT));
        assert_eq!(jobs.get(id).unwrap().state, JobState::Done);

        let (w, h, codec): (i64, i64, String) = env
            .library
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT width, height, codec_info FROM items WHERE ext = 'jpg'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .unwrap();
        assert_eq!((w, h, codec.as_str()), (1600, 900, r#"{"codec":"jpeg"}"#));
        let duration: i64 = env
            .library
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT duration_ms FROM items WHERE ext = 'flac'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(duration, 180_000);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM tags WHERE (namespace, name) IN (VALUES ('アーティスト', '灯野'), ('アルバム', '夜'))"),
            2
        );
        // 取り込んだらサムネイルのジョブが積まれる
        assert_eq!(
            env.count("SELECT COUNT(*) FROM jobs WHERE kind = 'thumbnail' AND state = 'queued'"),
            1
        );
    }

    /// フォント・3D・Aseprite を取り込み、プロキシ・解析の対象にしない。Aseprite でない `.ase` は取り込まず、
    /// 理由を取り込みの結果に出す。
    #[test]
    fn imports_fonts_and_models_and_reports_rejected_ase() {
        let env = Env::new();
        let src = env.src();
        write(&src.join("a.otf"), b"OTTO font");
        write(&src.join("b.glb"), b"glTF model");
        write(&src.join("c.ase"), &[0x80, 0, 0, 0, 0xE0, 0xA5, 1, 0]);
        write(&src.join("colors.ase"), b"ASEF\x00\x01\x00\x00");
        let jobs = env.manager(ImportHandler::new(Arc::new(
            crate::media::fake::FakeBackend,
        )));
        let id = start_import(
            &jobs,
            &ImportInput {
                paths: vec![src.clone()],
                options: ImportOptions::default(),
            },
        )
        .unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(WAIT));
        let record = jobs.get(id).unwrap();
        assert_eq!(record.state, JobState::Done);
        let result: ImportResult =
            serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap();
        assert_eq!((result.added, result.skipped, result.failed), (3, 1, 0));
        assert_eq!(result.rejected.len(), 1);
        assert_eq!(
            result.rejected[0].path,
            src.join("colors.ase").to_string_lossy()
        );
        assert!(result.rejected[0].reason.contains("Aseprite"));

        let rows: Vec<(String, String, String, String)> = env
            .library
            .db()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT ext, kind, proxy_state, analysis_state FROM items ORDER BY ext",
                )?;
                let rows =
                    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                Ok(rows.collect::<std::result::Result<_, _>>()?)
            })
            .unwrap();
        let rows: Vec<(&str, &str, &str, &str)> = rows
            .iter()
            .map(|(a, b, c, d)| (a.as_str(), b.as_str(), c.as_str(), d.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("ase", "image", "not_needed", "pending"),
                ("glb", "model", "not_needed", "not_needed"),
                ("otf", "font", "not_needed", "not_needed"),
            ]
        );
    }

    #[test]
    fn rejects_empty_input() {
        let env = Env::new();
        let jobs = env.manager(ImportHandler::default());
        let err = start_import(
            &jobs,
            &ImportInput {
                paths: vec![],
                options: ImportOptions::default(),
            },
        );
        assert!(matches!(err, Err(crate::CoreError::Invalid(_))));
    }
}
