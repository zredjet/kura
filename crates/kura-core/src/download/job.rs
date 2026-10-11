//! ダウンロードのジョブ（docs/architecture.md §9）。
//!
//! 1. 作業フォルダ `staging/dl-<job>/` にツールで取得する（進捗は `KURA` の行から）。
//!    一時停止・中止はプロセスグループごと止める。作業フォルダは待機中・一時停止中は残るので、
//!    再開すると yt-dlp は書きかけの続きから、gallery-dl は取得済みのファイルを飛ばして続ける。
//! 2. 終わったら、作業フォルダにできたファイルを取り込みと同じ方法で配置し、1 つのトランザクションで
//!    アイテム・タグ・ユニット・`remote_sources` と途中経過を書く。
//! 3. ツールが失敗で終わっても、できたファイルは入れてから失敗にする（再試行で残りを取る）。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::args::{
    CookieSource, DownloadOptions, RunPaths, archive_file, gallerydl_args, ytdlp_args,
};
use super::detect::{Detection, Tool};
use super::meta::TagPolicy;
use super::progress::{Progress, parse_line};
use super::register::{Collect, Origin, collect, place_all, write_entries};
use crate::jobs::{
    Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager, save_partial,
};
use crate::media::MediaBackend;
use crate::tools::{StreamEnd, child_path, find, run_streaming};
use crate::{CoreError, Result};

/// この時間なにも出力がなければ止める（ログインの待ちなどで固まったとき）。
const STALL: Duration = Duration::from_secs(10 * 60);
/// 結果に残すアイテムの ID の上限（「ライブラリで表示」に使う）。
const MAX_ITEM_IDS: usize = 1000;

/// ダウンロードの入力（jobs.input_json）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadInput {
    pub detection: Detection,
    #[serde(default)]
    pub options: DownloadOptions,
    #[serde(default)]
    pub cookies: CookieSource,
}

/// ダウンロードの結果・途中経過（jobs.result_json）。再試行すると数は足していく。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DownloadResult {
    /// 取得したものの題名（最初の 1 本・1 投稿）。
    pub title: Option<String>,
    /// ライブラリに入れたファイルの数（新規＋既存）。
    pub files: u64,
    pub added: u64,
    pub duplicates: u64,
    /// 取得済みとして飛ばした数（gallery-dl）。
    pub skipped: u64,
    /// 対応していない形式で入れなかった数。
    pub unsupported: u64,
    /// 付けたタグの数。
    pub tags: u64,
    pub item_ids: Vec<i64>,
    pub unit_ids: Vec<i64>,
}

/// 作業フォルダの名前（`staging/` の中）。開くときの掃除で残すかどうかの判断にも使う。
pub fn work_dir_name(job_id: JobId) -> String {
    format!("dl-{job_id}")
}

/// ダウンロードのジョブの処理。
pub struct DownloadHandler {
    backend: Arc<dyn MediaBackend>,
}

impl DownloadHandler {
    /// 取得したファイルのメディア情報を `backend` で読む。
    pub fn new(backend: Arc<dyn MediaBackend>) -> Self {
        Self { backend }
    }
}

impl JobHandler for DownloadHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let input: DownloadInput = serde_json::from_str(ctx.input())?;
        let mut state: DownloadResult = match ctx.partial() {
            Some(json) => serde_json::from_str(json).unwrap_or_default(),
            None => DownloadResult::default(),
        };
        let layout = ctx.library().layout().clone();
        let work = layout.staging_dir().join(work_dir_name(ctx.id()));
        let archives = layout.archives_dir();
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&archives)?;

        let tool = input.detection.tool;
        let program = find(tool.command())
            .ok_or_else(|| CoreError::ToolMissing(tool.command().to_string()))?;
        let paths = RunPaths {
            work_dir: work.clone(),
            archive: archive_file(&archives, tool),
            ffmpeg: find("ffmpeg"),
            deno: find("deno"),
        };
        let args = match tool {
            Tool::YtDlp => ytdlp_args(&input.detection, &input.options, input.cookies, &paths),
            Tool::GalleryDl => gallerydl_args(&input.detection, input.cookies, &paths),
        };
        ctx.log(&format!(
            "実行: {} {}",
            program.display(),
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        ));

        let mut progress = Progress::default();
        let mut errors: Vec<String> = Vec::new();
        let mut title_saved = state.title.is_some();
        let mut command = Command::new(&program);
        command
            .args(&args)
            .env("PATH", child_path())
            .current_dir(&work);
        let end = run_streaming(
            &mut command,
            &mut |line, is_err| match parse_line(line) {
                Some(parsed) => {
                    progress.apply(&parsed);
                    let fraction = progress.fraction().unwrap_or(0.0);
                    let message = progress.message();
                    if !title_saved && let Some(title) = &progress.title {
                        // 題名がわかったら一覧に出す
                        title_saved = true;
                        state.title = Some(title.clone());
                        if let Ok(json) = serde_json::to_string(&state)
                            && let Err(err) = ctx.checkpoint(&json, fraction, &message)
                        {
                            tracing::warn!(%err, "ダウンロードの途中経過を保存できませんでした");
                        }
                    } else {
                        ctx.progress(fraction, &message);
                    }
                }
                None => {
                    let line = line.trim();
                    if line.is_empty() {
                        return;
                    }
                    ctx.log(line);
                    if is_err && is_error_line(line) {
                        errors.push(line.to_string());
                    }
                }
            },
            &|| ctx.control() != Control::Continue,
            STALL,
        )?;
        state.skipped += progress.skipped;
        if state.title.is_none() {
            state.title = progress.title.clone();
        }

        let status = match end {
            StreamEnd::Stopped => {
                return Ok(match ctx.control() {
                    Control::Cancel => {
                        remove_dir(&work);
                        JobEnd::Canceled {
                            result_json: Some(serde_json::to_string(&state)?),
                        }
                    }
                    _ => JobEnd::Paused,
                });
            }
            StreamEnd::Stalled => {
                return Err(CoreError::Tool(format!(
                    "{} が {} 分間なにも出力しなかったので止めました",
                    tool.command(),
                    STALL.as_secs() / 60
                )));
            }
            StreamEnd::Exited(status) => status,
        };

        // できたファイルを入れる（失敗で終わったときも、できた分は入れる）
        let policy = TagPolicy {
            uploader: input.options.tag_uploader,
            site_tags: input.options.tag_site_tags,
        };
        let how = Collect {
            tool,
            site: &input.detection.site,
            policy,
            playlist_unit: input.options.playlist_unit,
            job_url: &input.detection.url,
        };
        let (files, unsupported) = collect(&work, &how)?;
        state.unsupported += unsupported;
        if !files.is_empty() {
            ctx.progress(
                1.0,
                &format!("{} 件をライブラリに入れています", files.len()),
            );
            let placed = place_all(&layout, self.backend.as_ref(), ctx.id(), &files);
            for (file, outcome) in files.iter().zip(&placed) {
                if let Err(reason) = outcome {
                    ctx.log(&format!("入れられませんでした: {} ({reason})", file.name));
                }
            }
            let entries = files.into_iter().zip(placed).collect();
            let dest = input.options.dest_folder_id;
            let (id, before) = (ctx.id(), state.clone());
            let (state_after, registered) = ctx.library().db().write(move |tx| {
                let registered = write_entries(tx, entries, dest, Origin::Download)?;
                let mut state = before;
                state.files += registered.added + registered.duplicates;
                state.added += registered.added;
                state.duplicates += registered.duplicates;
                state.tags += registered.tags;
                for id in &registered.item_ids {
                    if state.item_ids.len() < MAX_ITEM_IDS && !state.item_ids.contains(id) {
                        state.item_ids.push(*id);
                    }
                }
                for id in &registered.unit_ids {
                    if !state.unit_ids.contains(id) {
                        state.unit_ids.push(*id);
                    }
                }
                save_partial(
                    tx,
                    id,
                    &serde_json::to_string(&state)?,
                    1.0,
                    "ライブラリに入れました",
                )?;
                Ok((state, registered))
            })?;
            state = state_after;
            if registered.added > 0 {
                ctx.ensure_queued(JobKind::Thumbnail, "{}")?;
                ctx.ensure_queued(JobKind::Proxy, "{}")?;
            }
            // 入れたファイルは作業フォルダから消す（再試行で同じものを入れ直さないように）
            for path in &registered.placed_paths {
                let _ = std::fs::remove_file(path);
            }
        }

        if !status.success() {
            let reason = errors
                .last()
                .map(|e| explain_error(e))
                .unwrap_or_else(|| format!("{} が失敗しました（{status}）", tool.command()));
            ctx.log(&format!("失敗: {reason}"));
            return Err(CoreError::Tool(reason));
        }
        remove_dir(&work);
        let message = done_message(&state);
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: serde_json::to_string(&state)?,
            message: Some(message),
        })
    }
}

/// 完了の一行（「2 項目を追加 · タグ 7 件」「取得済みのため新しいものはありません」）。
fn done_message(state: &DownloadResult) -> String {
    if state.files == 0 {
        return if state.skipped > 0 {
            format!("取得済みの {} 件はスキップしました", state.skipped)
        } else {
            "新しく取得したものはありません（取得済みか、対応していない形式です）".to_string()
        };
    }
    let mut parts = vec![format!("{} 項目を追加", state.added)];
    if state.duplicates > 0 {
        parts.push(format!("既存 {} 件", state.duplicates));
    }
    if state.tags > 0 {
        parts.push(format!("タグ {} 件", state.tags));
    }
    if state.skipped > 0 {
        parts.push(format!("取得済みの {} 件はスキップ", state.skipped));
    }
    parts.join(" · ")
}

/// エラーの行か（yt-dlp は `ERROR:`、gallery-dl は `[error]`）。
fn is_error_line(line: &str) -> bool {
    line.starts_with("ERROR:") || line.contains("[error]")
}

/// ツールのエラーを画面向けの説明にする。ログインが要りそうなら Cookie の案内を足す。
fn explain_error(line: &str) -> String {
    let text = line
        .trim_start_matches("ERROR:")
        .trim()
        .split("[error]")
        .last()
        .unwrap_or(line)
        .trim()
        .to_string();
    let lower = text.to_ascii_lowercase();
    let login = [
        "login",
        "log in",
        "sign in",
        "authrequired",
        "cookies",
        "private",
        "403",
    ]
    .iter()
    .any(|w| lower.contains(w));
    if login {
        format!("ログインが必要です。Cookie の読み込み元を確認してください（{text}）")
    } else {
        text
    }
}

fn remove_dir(dir: &Path) {
    if let Err(err) = std::fs::remove_dir_all(dir)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%err, path = %dir.display(), "作業フォルダを消せませんでした");
    }
}

/// ダウンロードを積む。
pub fn start_download(jobs: &JobManager, input: &DownloadInput) -> Result<JobId> {
    jobs.enqueue(JobKind::Download, serde_json::to_string(input)?)
}

/// 作業フォルダ（掃除で残すかどうかの判断用）。
pub fn work_dir(staging: &Path, job_id: JobId) -> PathBuf {
    staging.join(work_dir_name(job_id))
}

#[cfg(test)]
#[path = "job_tests.rs"]
mod tests;
