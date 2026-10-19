//! キャプチャのジョブ（docs/architecture.md §10）。
//!
//! 1. 専用プロファイルの Chrome をヘッドレスで起動し、DevTools（127.0.0.1）につなぐ。
//! 2. ルールのとおりに撮る（`shoot`）。終わったら Chrome はプロセスグループごと止める。
//! 3. PNG を取り込みと同じ方法で配置し、タグ `キャプチャ`・`サイト:<ホスト>`・URL を付けて登録する。
//!    ルールの最終実行と結果も同じトランザクションで記録する。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::cdp::Cdp;
use super::chrome::{CHROME_TOOL, ChromeProcess, profile_in_use};
use super::rules::{CaptureRule, record_run};
use super::shoot::{Pace, shoot};
use crate::download::meta::FileMeta;
use crate::download::register::{Downloaded, Origin, place_all, write_entries};
use crate::import::MediaKind;
use crate::jobs::{
    Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager, save_partial,
};
use crate::media::MediaBackend;
use crate::tags::TagName;
use crate::time::now_ms;
use crate::tools::find;
use crate::{CoreError, Result};

/// Chrome の DevTools の準備を待つ時間。
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// キャプチャの入力（jobs.input_json）。ルールは積んだときの内容を写して持つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureInput {
    pub url: String,
    pub rule: CaptureRule,
    /// 専用プロファイルの場所（アプリが決める）。
    pub profile_dir: PathBuf,
}

/// キャプチャの結果（jobs.result_json）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureResult {
    pub title: Option<String>,
    pub added: u64,
    pub duplicates: u64,
    pub tags: u64,
    pub item_ids: Vec<i64>,
    /// 長すぎて途中までになった。
    pub truncated: bool,
}

/// URL のホスト（`サイト:` のタグ）。読めなければ None。
pub fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then(|| host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// キャプチャで付けるタグ（`キャプチャ` と `サイト:<ホスト>`）。
pub fn capture_tags(url: &str) -> Vec<TagName> {
    let mut tags = vec![TagName::plain("キャプチャ")];
    if let Some(host) = host_of(url) {
        tags.push(TagName {
            namespace: "サイト".into(),
            name: host,
        });
    }
    tags
}

/// 撮った・保存した画像の登録内容。
pub fn capture_meta(url: &str, title: &str, at: i64) -> FileMeta {
    let title = title.trim();
    FileMeta {
        title: if title.is_empty() {
            host_of(url).unwrap_or_else(|| url.to_string())
        } else {
            title.to_string()
        },
        source_url: Some(url.to_string()),
        site: "capture".into(),
        remote_id: None,
        page_index: None,
        captured_at: Some(at),
        tags: capture_tags(url),
        group: None,
    }
}

/// `png` のファイルを、キャプチャとしてライブラリに入れる（見たままキャプチャも使う）。
/// ルールがあれば、その最終実行を同じトランザクションで記録する。`job` があれば途中経過も保存する。
pub fn register_png(
    library: &crate::library::Library,
    backend: &dyn MediaBackend,
    png: &Path,
    meta: FileMeta,
    dest_folder_id: Option<i64>,
    rule_id: Option<i64>,
    job: Option<(JobId, CaptureResult)>,
) -> Result<CaptureResult> {
    let name = png
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "capture.png".into());
    let file = Downloaded {
        path: png.to_path_buf(),
        sidecar: None,
        name,
        ext: "png".into(),
        kind: MediaKind::Image,
        meta: meta.clone(),
    };
    let job_id = job.as_ref().map_or(0, |(id, _)| *id);
    // files/ に置いてから DB に書き終えるまで、ゴミ箱を空にする処理を待たせる（同じ中身を消されないように）
    let _ingest = library.ingest_guard();
    let placed = place_all(
        library.layout(),
        backend,
        job_id,
        std::slice::from_ref(&file),
    );
    let entries: Vec<_> = vec![file].into_iter().zip(placed).collect();
    let title = meta.title.clone();
    let layout = library.layout().clone();
    let (result, _) = library.db().write(move |tx| {
        let registered = write_entries(tx, &layout, entries, dest_folder_id, Origin::Capture)?;
        if registered.failed > 0 {
            return Err(CoreError::Tool(
                "撮った画像をライブラリに入れられませんでした".into(),
            ));
        }
        if let Some(rule) = rule_id {
            record_run(tx, rule, true, now_ms())?;
        }
        let mut result = job.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
        result.title = Some(title);
        result.added += registered.added;
        result.duplicates += registered.duplicates;
        result.tags = registered.tags;
        result.item_ids.extend(&registered.item_ids);
        if let Some((id, _)) = &job {
            save_partial(
                tx,
                *id,
                &serde_json::to_string(&result)?,
                1.0,
                "ライブラリに入れました",
            )?;
        }
        Ok((result, registered))
    })?;
    Ok(result)
}

/// キャプチャのジョブの処理。
pub struct CaptureHandler {
    backend: std::sync::Arc<dyn MediaBackend>,
}

impl CaptureHandler {
    /// 撮った画像のメディア情報を `backend` で読む。
    pub fn new(backend: std::sync::Arc<dyn MediaBackend>) -> Self {
        Self { backend }
    }
}

struct JobPace<'a>(&'a JobContext);

impl Pace for JobPace<'_> {
    fn stop(&self) -> bool {
        self.0.control() != Control::Continue
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

impl JobHandler for CaptureHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let input: CaptureInput = serde_json::from_str(ctx.input())?;
        let library = ctx.library();
        let outcome = self.capture(ctx, &input);
        match outcome {
            Ok(Some(result)) => {
                let message = if result.truncated {
                    "撮影しました（長いページのため途中まで）".to_string()
                } else {
                    "撮影しました".to_string()
                };
                ctx.log(&message);
                Ok(JobEnd::Done {
                    result_json: serde_json::to_string(&result)?,
                    message: Some(message),
                })
            }
            // 止める指示（一時停止・中止）。撮影はやり直すので途中経過はない
            Ok(None) => Ok(match ctx.control() {
                Control::Cancel => JobEnd::Canceled { result_json: None },
                _ => JobEnd::Paused,
            }),
            Err(err) => {
                let id = input.rule.id;
                if let Err(e) = library
                    .db()
                    .write(move |tx| record_run(tx, id, false, now_ms()))
                {
                    tracing::warn!(%e, "ルールの実行結果を記録できませんでした");
                }
                Err(err)
            }
        }
    }
}

impl CaptureHandler {
    /// 撮って入れる。止める指示が出たら None。
    fn capture(&self, ctx: &JobContext, input: &CaptureInput) -> Result<Option<CaptureResult>> {
        if host_of(&input.url).is_none() {
            return Err(CoreError::Invalid(
                "http:// か https:// の URL だけ撮れます".into(),
            ));
        }
        // ルールを直してから再試行したときのために、今のルールを使う（消されていれば積んだときの写し）
        let rule = ctx
            .library()
            .db()
            .read(|conn| super::rules::get(conn, input.rule.id))
            .unwrap_or_else(|_| input.rule.clone());
        if profile_in_use(&input.profile_dir) {
            return Err(CoreError::Tool(
                "ログイン用の Chrome が開いているあいだは、裏での撮影ができません。キャプチャの欄の「このページを撮る」を使うか、閉じてから再試行してください".into(),
            ));
        }
        let chrome =
            find(CHROME_TOOL).ok_or_else(|| CoreError::ToolMissing("Google Chrome".into()))?;
        ctx.progress(0.1, "Chrome を起動しています");
        ctx.log(&format!(
            "撮影: {} （ルール「{}」）",
            input.url, input.rule.name
        ));
        let mut process = ChromeProcess::launch_headless(&chrome, &input.profile_dir)?;
        let connected =
            Cdp::connect_from_profile(&input.profile_dir, START_TIMEOUT, &mut || process.alive());
        let mut cdp = match connected {
            Ok(cdp) => cdp,
            Err(err) => {
                for line in process.stderr_tail() {
                    ctx.log(&line);
                }
                return Err(err);
            }
        };
        cdp.open_page()?;
        ctx.progress(0.3, "ページを読み込んでいます");
        let pace = JobPace(ctx);
        let shot = match shoot(&mut cdp, &input.url, &rule, &pace) {
            Ok(shot) => shot,
            Err(_) if pace.stop() => return Ok(None),
            Err(err) => return Err(err),
        };
        // 撮り終えたら Chrome を止める（プロファイルのロックを外す）
        drop(cdp);
        drop(process);

        ctx.progress(0.9, "ライブラリに入れています");
        let staging = ctx.library().layout().staging_dir();
        std::fs::create_dir_all(&staging)?;
        let host = host_of(&input.url).unwrap_or_else(|| "page".into());
        let png = staging.join(format!("capture-{}-{host}.png", ctx.id()));
        std::fs::write(&png, &shot.png)?;
        let meta = capture_meta(&input.url, &shot.title, now_ms());
        let start = CaptureResult {
            truncated: shot.truncated,
            ..CaptureResult::default()
        };
        let result = register_png(
            ctx.library(),
            self.backend.as_ref(),
            &png,
            meta,
            rule.dest_folder_id,
            Some(rule.id),
            Some((ctx.id(), start)),
        );
        let _ = std::fs::remove_file(&png);
        let result = result?;
        if result.added > 0 {
            ctx.ensure_queued(JobKind::Thumbnail, "{}")?;
        }
        Ok(Some(result))
    }
}

/// キャプチャを積む。
pub fn start_capture(jobs: &JobManager, input: &CaptureInput) -> Result<JobId> {
    jobs.enqueue(JobKind::Capture, serde_json::to_string(input)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;
    use crate::media::NoBackend;

    #[test]
    fn hosts_and_tags() {
        assert_eq!(
            host_of("https://www.Example.com:8443/a?b").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            host_of("http://user@news.example.jp/x").as_deref(),
            Some("news.example.jp")
        );
        assert_eq!(host_of("file:///etc/passwd"), None);
        assert_eq!(host_of("javascript:alert(1)"), None);
        assert_eq!(
            capture_tags("https://example.com/a"),
            vec![
                TagName::plain("キャプチャ"),
                TagName {
                    namespace: "サイト".into(),
                    name: "example.com".into()
                }
            ]
        );
        let meta = capture_meta("https://example.com/a", "  ", 5);
        assert_eq!(
            (meta.title.as_str(), meta.site.as_str(), meta.captured_at),
            ("example.com", "capture", Some(5))
        );
    }

    #[test]
    fn registers_a_png_with_capture_tags_and_records_the_rule() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::create(&dir.path().join("L.kuralib")).unwrap();
        let rule = library
            .db()
            .write(|tx| {
                crate::capture::rules::create(
                    tx,
                    crate::capture::rules::RuleInput {
                        name: "記事".into(),
                        url_pattern: "https://example.com/*".into(),
                        mode: crate::capture::CaptureMode::FullPage,
                        target_selector: None,
                        hide_selectors: vec![],
                        viewport_width: 1200,
                        scroll_times: 0,
                        wait_ms: 0,
                        dest_folder_id: None,
                        enabled: true,
                    },
                )
            })
            .unwrap();
        let png = dir.path().join("shot.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nfake").unwrap();
        let result = register_png(
            &library,
            &NoBackend,
            &png,
            capture_meta("https://example.com/a", "記事の題名", 7),
            None,
            Some(rule.id),
            None,
        )
        .unwrap();
        assert_eq!(
            (result.added, result.tags, result.title.as_deref()),
            (1, 2, Some("記事の題名"))
        );
        let row: (String, String, String) = library
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT title, source_site, source_url FROM items",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .unwrap();
        assert_eq!(
            row,
            (
                "記事の題名".into(),
                "capture".into(),
                "https://example.com/a".into()
            )
        );
        let tags: Vec<String> = library
            .db()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT t.namespace || ':' || t.name || '/' || it.source FROM item_tags it
                     JOIN tags t ON t.id = it.tag_id ORDER BY t.namespace",
                )?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<std::result::Result<_, _>>()?)
            })
            .unwrap();
        assert_eq!(
            tags,
            vec![":キャプチャ/capture", "サイト:example.com/capture"]
        );
        let rule = library
            .db()
            .read(|c| crate::capture::rules::get(c, rule.id))
            .unwrap();
        assert_eq!(rule.last_status.as_deref(), Some("ok"));
    }
}
