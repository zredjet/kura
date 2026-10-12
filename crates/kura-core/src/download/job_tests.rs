//! ダウンロードのジョブのテスト。yt-dlp・gallery-dl の代わりに、同じ形の行とファイルを出すスクリプトを使う。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::*;
use crate::download::detect::detect;
use crate::jobs::{EventSink, JobEvent, JobRecord, JobState};
use crate::library::Library;
use crate::media::NoBackend;
use crate::tools::set_overrides;

const WAIT: Duration = Duration::from_secs(30);

/// ツールの場所の指定はアプリ全体で 1 つなので、指定を使うテストは順に動かす。
fn lock_tools() -> MutexGuard<'static, ()> {
    crate::tools::TEST_TOOLS_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

struct Env {
    /// 一時フォルダ（テストの終わりまで残す）。
    _dir: tempfile::TempDir,
    library: Arc<Library>,
    events: Arc<Mutex<Vec<JobEvent>>>,
    jobs: JobManager,
}

impl Env {
    /// `name` のツールを `script`（sh）にして、ジョブを動かす準備をする。
    fn new(name: &str, script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join(name);
        std::fs::write(&fake, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        set_overrides(BTreeMap::from([(name.to_string(), fake)]));
        let library = Arc::new(Library::create(&dir.path().join("L.kuralib")).unwrap());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        let sink: EventSink = Arc::new(move |e| sink_events.lock().unwrap().push(e));
        let jobs = JobManager::new(
            library.clone(),
            vec![(
                JobKind::Download,
                Arc::new(DownloadHandler::new(Arc::new(NoBackend))) as Arc<dyn JobHandler>,
            )],
            sink,
        )
        .unwrap();
        jobs.start().unwrap();
        Self {
            _dir: dir,
            library,
            events,
            jobs,
        }
    }

    fn enqueue(&self, url: &str) -> JobId {
        self.enqueue_with(url, false)
    }

    fn enqueue_with(&self, url: &str, redownload: bool) -> JobId {
        let input = DownloadInput {
            detection: detect(url, &[]).unwrap(),
            options: DownloadOptions::default(),
            cookies: CookieSource::None,
            redownload,
        };
        start_download(&self.jobs, &input).unwrap()
    }

    fn finish(&self, url: &str) -> (JobRecord, DownloadResult) {
        self.finish_with(url, false)
    }

    fn finish_with(&self, url: &str, redownload: bool) -> (JobRecord, DownloadResult) {
        let id = self.enqueue_with(url, redownload);
        assert!(self.jobs.wait_idle(WAIT));
        let record = self.jobs.get(id).unwrap();
        let result = record
            .result_json
            .as_deref()
            .map(|j| serde_json::from_str(j).unwrap())
            .unwrap_or_default();
        (record, result)
    }

    fn work(&self, id: JobId) -> PathBuf {
        self.library.layout().staging_dir().join(work_dir_name(id))
    }

    fn query<T: rusqlite::types::FromSql>(&self, sql: &'static str) -> Vec<T> {
        self.library
            .db()
            .read(|c| {
                let mut stmt = c.prepare(sql)?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<std::result::Result<_, _>>()?)
            })
            .unwrap()
    }

    fn wait_for(&self, id: JobId, state: JobState) {
        let started = Instant::now();
        while self.jobs.get(id).unwrap().state != state {
            assert!(started.elapsed() < WAIT, "{state:?} になりません");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.jobs.shutdown();
        set_overrides(BTreeMap::new());
    }
}

/// `-P` の場所に動画と info.json を書く yt-dlp の代わり。
const FAKE_YTDLP: &str = r##"
dir=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -P) dir="$2"; shift 2;;
    --) url="$2"; shift 2;;
    *) shift;;
  esac
done
printf 'KURA\tplan\tvid1\tNA\tNA\t[100, 50]\tNA\tNA\t"テスト動画"\n'
printf 'KURA\tdl\t100\t100\tNA\t1000\t0\t137\n'
printf 'KURA\tdl\t50\t50\tNA\t1000\t0\t140\n'
printf 'video-%s' "$url" > "$dir/vid1.mp4"
printf '{"id":"vid1","title":"テスト動画","uploader":"ちゃんねる","webpage_url":"%s","description":"#ねこ","upload_date":"20260421"}' "$url" > "$dir/vid1.info.json"
printf 'KURA\tfile\t%s/vid1.mp4\n' "$dir"
echo "[info] done"
"##;

#[test]
fn ytdlp_download_is_registered_with_tags() {
    let _guard = lock_tools();
    let env = Env::new("yt-dlp", FAKE_YTDLP);
    let url = "https://www.youtube.com/watch?v=vid1";
    let (record, result) = env.finish(url);
    assert_eq!(record.state, JobState::Done, "{:?}", record.message);
    assert_eq!(result.title.as_deref(), Some("テスト動画"));
    assert_eq!((result.files, result.added, result.tags), (1, 1, 2));
    assert_eq!(record.message.as_deref(), Some("1 項目を追加 · タグ 2 件"));
    assert_eq!(
        env.query::<String>("SELECT title || '|' || source_site || '|' || source_url FROM items"),
        vec![format!("テスト動画|youtube|{url}")]
    );
    let mut tags = env.query::<String>(
        "SELECT t.namespace || ':' || t.name FROM item_tags it JOIN tags t ON t.id = it.tag_id",
    );
    tags.sort();
    assert_eq!(tags, vec![":ねこ", "投稿者:ちゃんねる"]);
    assert_eq!(
        env.query::<String>("SELECT site || '/' || remote_id FROM remote_sources"),
        vec!["youtube/vid1"]
    );
    // 作業フォルダは消える
    assert!(!env.work(record.id).exists());
    // 題名がわかった時点で一覧に出せるよう、結果つきの進捗が届く
    let events = env.events.lock().unwrap();
    assert!(events.iter().any(|e| {
        e.state == JobState::Running
            && e.result_json
                .as_deref()
                .is_some_and(|j| j.contains("テスト動画"))
    }));
}

/// 2 ページの作品を取る gallery-dl の代わり。2 回目は取得済みとして飛ばす。
const FAKE_GALLERYDL: &str = r##"
dir=""; archive=""
while [ $# -gt 0 ]; do
  case "$1" in
    -D) dir="$2"; shift 2;;
    --download-archive) archive="$2"; shift 2;;
    *) shift;;
  esac
done
for num in 0 1; do
  printf 'KURA\tprepare\tpixiv\t100\t%s\t2\t"作品"\n' "$num"
  if [ -f "$archive" ] && grep -q "p$num" "$archive"; then
    printf 'KURA\tskip\t%s/100_p%s.jpg\n' "$dir" "$num"
    continue
  fi
  printf 'image-%s' "$num" > "$dir/100_p$num.jpg"
  printf '{"category":"pixiv","id":100,"num":%s,"count":2,"title":"作品","date":"2026-07-22 08:29:06","user":{"name":"KR"},"tags":["オリジナル"]}' "$num" > "$dir/100_p$num.jpg.json"
  echo "p$num" >> "$archive"
  printf 'KURA\tfile\t%s/100_p%s.jpg\n' "$dir" "$num"
done
"##;

#[test]
fn gallerydl_multi_page_work_becomes_a_unit() {
    let _guard = lock_tools();
    let env = Env::new("gallery-dl", FAKE_GALLERYDL);
    let url = "https://www.pixiv.net/artworks/100";
    let (record, result) = env.finish(url);
    assert_eq!(record.state, JobState::Done, "{:?}", record.message);
    assert_eq!((result.added, result.unit_ids.len()), (2, 1));
    assert_eq!(
        env.query::<String>("SELECT name || '|' || origin FROM units"),
        vec!["作品|download"]
    );
    assert_eq!(
        env.query::<String>("SELECT title FROM items ORDER BY id"),
        vec!["作品（1/2）", "作品（2/2）"]
    );
    let members = env
        .library
        .db()
        .read(|c| crate::unit::members(c, result.unit_ids[0]))
        .unwrap();
    assert_eq!(members, result.item_ids);

    // 2 回目は取得済み
    let (record, result) = env.finish(url);
    assert_eq!(record.state, JobState::Done);
    assert_eq!((result.files, result.skipped), (0, 2));
    assert_eq!(
        record.message.as_deref(),
        Some("取得済みの 2 件はスキップしました")
    );
    assert_eq!(env.query::<i64>("SELECT count(*) FROM items"), vec![2]);
}

/// 取得済みの記録（テキスト）を見て、記録にあれば飛ばす yt-dlp の代わり。
const FAKE_YTDLP_ARCHIVE: &str = r##"
dir=""; url=""; archive=""
while [ $# -gt 0 ]; do
  case "$1" in
    -P) dir="$2"; shift 2;;
    --download-archive) archive="$2"; shift 2;;
    --) url="$2"; shift 2;;
    *) shift;;
  esac
done
if [ -f "$archive" ] && grep -qx "youtube vid1" "$archive"; then
  echo "[download] vid1: has already been recorded in the archive"
  exit 0
fi
printf 'KURA\tplan\tvid1\tNA\tNA\t[100]\tNA\tNA\t"テスト動画"\n'
printf 'video' > "$dir/vid1.mp4"
printf '{"id":"vid1","title":"テスト動画","webpage_url":"%s"}' "$url" > "$dir/vid1.info.json"
echo "youtube vid1" >> "$archive"
printf 'KURA\tfile\t%s/vid1.mp4\n' "$dir"
"##;

#[test]
fn redownload_ignores_the_archive_and_records_again() {
    let _guard = lock_tools();
    let env = Env::new("yt-dlp", FAKE_YTDLP_ARCHIVE);
    let url = "https://www.youtube.com/watch?v=vid1";
    let archive = env.library.layout().archives_dir().join("yt-dlp.txt");
    let (_, result) = env.finish(url);
    assert_eq!(result.added, 1);
    assert_eq!(std::fs::read_to_string(&archive).unwrap(), "youtube vid1\n");

    // ゴミ箱に入れて空にすると、ふつうのダウンロードでは記録にあるので取り直さない
    env.library
        .db()
        .write(|tx| {
            tx.execute("UPDATE items SET trashed_at = 1", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(env.library.empty_trash().unwrap().items, 1);
    let (_, result) = env.finish(url);
    assert_eq!((result.files, result.skipped), (0, 1));

    // 再ダウンロードは記録にあっても取り直す。記録は増やさない
    let (record, result) = env.finish_with(url, true);
    assert_eq!(record.state, JobState::Done, "{:?}", record.message);
    assert_eq!((result.files, result.added, result.skipped), (1, 1, 0));
    assert_eq!(env.query::<i64>("SELECT count(*) FROM items"), vec![1]);
    assert_eq!(std::fs::read_to_string(&archive).unwrap(), "youtube vid1\n");
    assert!(!env.work(record.id).exists());
    let log = env.jobs.log_text(record.id, 64 * 1024).unwrap();
    assert!(log.contains("再ダウンロード"), "{log}");

    // 記録を消したあとの再ダウンロードは、取ったものを記録に戻す
    std::fs::remove_file(&archive).unwrap();
    let (_, result) = env.finish_with(url, true);
    assert_eq!((result.files, result.duplicates), (1, 1));
    assert_eq!(std::fs::read_to_string(&archive).unwrap(), "youtube vid1\n");
}

#[test]
fn failures_keep_what_was_downloaded_and_explain_logins() {
    let _guard = lock_tools();
    let script = r##"
dir=""
while [ $# -gt 0 ]; do
  case "$1" in -D) dir="$2"; shift 2;; *) shift;; esac
done
printf 'KURA\tprepare\tpixiv\t5\t0\t3\t"途中まで"\n'
printf 'one' > "$dir/5_p0.jpg"
printf 'KURA\tfile\t%s/5_p0.jpg\n' "$dir"
echo "[pixiv][error] AuthRequired: 'refresh-token' needed to access this work" 1>&2
exit 4
"##;
    let env = Env::new("gallery-dl", script);
    let (record, result) = env.finish("https://www.pixiv.net/artworks/5");
    assert_eq!(record.state, JobState::Failed);
    assert_eq!(record.error_code.as_deref(), Some("ToolFailed"));
    let message = record.message.unwrap_or_default();
    assert!(message.starts_with("ログインが必要です"), "{message}");
    // できた分は入っていて、作業フォルダからは消えている
    assert_eq!(result.added, 1);
    assert_eq!(env.query::<i64>("SELECT count(*) FROM items"), vec![1]);
    assert!(!env.work(record.id).join("5_p0.jpg").exists());
    // 再試行すると待ち行列に戻る（同じ失敗で終わる）
    env.jobs.retry(record.id).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    let again = env.jobs.get(record.id).unwrap();
    assert_eq!(again.state, JobState::Failed);
    let log = env.jobs.log_text(record.id, 64 * 1024).unwrap();
    assert!(log.contains("AuthRequired"), "{log}");
}

#[test]
fn pause_keeps_the_work_dir_and_cancel_removes_it() {
    let _guard = lock_tools();
    let script = r##"
dir=""
while [ $# -gt 0 ]; do
  case "$1" in -P) dir="$2"; shift 2;; *) shift;; esac
done
printf 'partial' > "$dir/slow.mp4.part"
printf 'KURA\tplan\tslow\tNA\tNA\t[1000]\tNA\tNA\t"遅い動画"\n'
printf 'KURA\tdl\t10\t1000\tNA\t1\t999\t18\n'
sleep 30
"##;
    let env = Env::new("yt-dlp", script);
    let id = env.enqueue("https://www.youtube.com/watch?v=slow");
    env.wait_for(id, JobState::Running);
    // 進捗が届くまで待ってから止める
    let started = Instant::now();
    while !env.work(id).join("slow.mp4.part").exists() {
        assert!(started.elapsed() < WAIT);
        std::thread::sleep(Duration::from_millis(20));
    }
    env.jobs.pause(id).unwrap();
    env.wait_for(id, JobState::Paused);
    assert!(env.work(id).join("slow.mp4.part").exists());
    let record = env.jobs.get(id).unwrap();
    let saved: DownloadResult =
        serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(saved.title.as_deref(), Some("遅い動画"));

    env.jobs.resume(id).unwrap();
    env.wait_for(id, JobState::Running);
    env.jobs.cancel(id).unwrap();
    env.wait_for(id, JobState::Canceled);
    assert!(!env.work(id).exists());
}

#[test]
fn done_messages() {
    let mut state = DownloadResult::default();
    assert!(done_message(&state).starts_with("新しく取得したものはありません"));
    state.skipped = 3;
    assert_eq!(done_message(&state), "取得済みの 3 件はスキップしました");
    state.files = 2;
    state.added = 1;
    state.duplicates = 1;
    assert_eq!(
        done_message(&state),
        "1 項目を追加 · 既存 1 件 · 取得済みの 3 件はスキップ"
    );
    assert_eq!(
        explain_error("ERROR: [youtube] x: Sign in to confirm your age"),
        "ログインが必要です。Cookie の読み込み元を確認してください（[youtube] x: Sign in to confirm your age）"
    );
    assert_eq!(
        explain_error("ERROR: Unsupported URL: x"),
        "Unsupported URL: x"
    );
}
