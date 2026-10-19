//! サムネイルのジョブ。`thumb_state = 'pending'` のアイテムを新しい順に少しずつ取り出し、
//! メディア情報が足りなければ読み、サムネイル（s / l）と音楽のジャケットを作る。
//!
//! 進み具合はアイテムごとの thumb_state そのもの（強制終了しても、続きは pending のものから）。

use std::sync::Arc;

use rayon::prelude::*;
use rusqlite::{Connection, params};

use super::{MediaBackend, MediaError, MediaInfo, ThumbTargets};
use crate::import::MediaKind;
use crate::jobs::{Control, JobContext, JobEnd, JobHandler};
use crate::library::{Layout, ThumbSize, hex};
use crate::time::now_ms;
use crate::{CoreError, Result};

/// 1 回に取り出す件数（見えている新しいものから早く出すため小さめ）。
pub const THUMB_BATCH: usize = 64;

/// サムネイルを作るアイテム。
#[derive(Debug, Clone)]
struct Pending {
    id: i64,
    hash: [u8; 32],
    ext: String,
    kind: MediaKind,
    width: Option<i64>,
    height: Option<i64>,
    duration_ms: Option<i64>,
}

impl Pending {
    /// メディア情報を読み直す必要があるか（取り込みのときに読めなかった、M1 で取り込んだ、など）。
    /// フォント・3D の情報は段 3 で専用の道（CoreText・glb の読み手）で読むので、ここでは読まない。
    fn needs_probe(&self) -> bool {
        match self.kind {
            MediaKind::Image => self.width.is_none() || self.height.is_none(),
            MediaKind::Video => self.width.is_none() || self.duration_ms.is_none(),
            MediaKind::Audio => self.duration_ms.is_none(),
            MediaKind::Font | MediaKind::Model => false,
        }
    }
}

/// 1 件の結果。
#[derive(Debug)]
struct Outcome {
    id: i64,
    info: Option<MediaInfo>,
    ready: bool,
    error: Option<String>,
}

/// サムネイルのジョブの処理。
pub struct ThumbnailHandler {
    backend: Arc<dyn MediaBackend>,
    threads: usize,
}

impl ThumbnailHandler {
    /// `backend` でサムネイルを作る。並列数は CPU コア数。
    pub fn new(backend: Arc<dyn MediaBackend>) -> Self {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self { backend, threads }
    }

    /// 並列数を変える（計測用）。
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }
}

impl JobHandler for ThumbnailHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let library = ctx.library();
        let layout = library.layout().clone();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.threads)
            .thread_name(|i| format!("kura-thumb-{i}"))
            .build()
            .map_err(|err| {
                CoreError::Invalid(format!("サムネイルのスレッドを作れません: {err}"))
            })?;

        let (mut done, mut failed) = (0u64, 0u64);
        loop {
            match ctx.control() {
                Control::Continue => {}
                Control::Pause => return Ok(JobEnd::Paused),
                Control::Cancel => {
                    return Ok(JobEnd::Canceled {
                        result_json: Some(summary(done, failed)),
                    });
                }
            }
            let (batch, unknown) = library.db().read(|conn| pending_batch(conn, THUMB_BATCH))?;
            if batch.is_empty() && unknown.is_empty() {
                break;
            }
            let backend = self.backend.as_ref();
            let mut outcomes: Vec<Outcome> = pool.install(|| {
                batch
                    .par_iter()
                    .map(|item| process(&layout, backend, item))
                    .collect()
            });
            outcomes.extend(unknown);
            for outcome in &outcomes {
                if let Some(error) = &outcome.error {
                    ctx.log(&format!("アイテム {}: {error}", outcome.id));
                }
            }
            done += outcomes.len() as u64;
            failed += outcomes.iter().filter(|o| !o.ready).count() as u64;
            library.db().write(move |tx| apply(tx, &outcomes))?;

            let remaining = library.db().read(count_pending)?;
            let total = done + remaining;
            ctx.progress(
                done as f64 / total.max(1) as f64,
                &format!("{done} / {total} 枚"),
            );
        }
        let message = format!("{done} 件のサムネイルを作成（作れなかったもの {failed}）");
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: summary(done, failed),
            message: Some(message),
        })
    }
}

fn summary(done: u64, failed: u64) -> String {
    serde_json::json!({ "done": done, "failed": failed }).to_string()
}

/// サムネイル未作成のものを新しい順に取り出す（idx_items_thumb_pending を使う）。
/// 知らない種類（DB の CHECK が外れていたなど）は画像として描かず、失敗の結果にして返す（同じ行を拾い直さない）。
fn pending_batch(conn: &Connection, limit: usize) -> Result<(Vec<Pending>, Vec<Outcome>)> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, hash, ext, kind, width, height, duration_ms FROM items
         WHERE thumb_state = 'pending' ORDER BY imported_at DESC, id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        let hash: Vec<u8> = row.get(1)?;
        let kind: String = row.get(3)?;
        Ok((
            row.get::<_, i64>(0)?,
            hash,
            row.get::<_, String>(2)?,
            kind,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    })?;
    let (mut out, mut unknown) = (Vec::new(), Vec::new());
    for row in rows {
        let (id, hash, ext, kind, width, height, duration_ms) = row?;
        let Some(kind) = MediaKind::parse(&kind) else {
            unknown.push(Outcome {
                id,
                info: None,
                ready: false,
                error: Some(format!(
                    "知らない種類「{kind}」なので、サムネイルを作りません"
                )),
            });
            continue;
        };
        let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) else {
            continue;
        };
        out.push(Pending {
            id,
            hash,
            ext,
            kind,
            width,
            height,
            duration_ms,
        });
    }
    Ok((out, unknown))
}

/// サムネイル未作成の数。
fn count_pending(conn: &Connection) -> Result<u64> {
    let mut stmt =
        conn.prepare_cached("SELECT COUNT(*) FROM items WHERE thumb_state = 'pending'")?;
    let count: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// 1 件分を処理する（DB には触らない）。
fn process(layout: &Layout, backend: &dyn MediaBackend, item: &Pending) -> Outcome {
    let src = layout.content_path(&item.hash, &item.ext);
    let hash_hex = hex(&item.hash);
    let info = if item.needs_probe() {
        backend.probe(&src, item.kind).ok()
    } else {
        None
    };
    let targets = ThumbTargets {
        large: layout.thumb_path(&hash_hex, ThumbSize::Large),
        small: layout.thumb_path(&hash_hex, ThumbSize::Small),
    };
    let result: std::result::Result<bool, MediaError> = match item.kind {
        MediaKind::Image => backend.image_thumbnails(&src, &targets).map(|()| true),
        MediaKind::Video => {
            // 長さの 10% の地点（わからなければ 1 秒）
            let duration = info
                .as_ref()
                .and_then(|i| i.duration_ms)
                .or(item.duration_ms);
            let at = duration.map(|ms| ms as f64 / 1000.0 * 0.1).unwrap_or(1.0);
            backend.video_thumbnails(&src, at, &targets).map(|()| true)
        }
        // ジャケットがなければサムネイルなし（グリッドは音楽の表示にする）
        MediaKind::Audio => backend.audio_artwork(&src, &layout.art_path(&hash_hex), &targets),
        // 画像として描かない。段 3 で専用の道（フォントは CoreText、3D は glb の読み手と描画）を作り、
        // 'thumbs.plan' の指紋で failed を pending に戻す
        MediaKind::Font => Err(MediaError::Failed(
            "フォントのサムネイルはまだ作れません".into(),
        )),
        MediaKind::Model => Err(MediaError::Failed(
            "3D モデルのサムネイルはまだ作れません".into(),
        )),
    };
    match result {
        Ok(ready) => Outcome {
            id: item.id,
            info,
            ready,
            error: None,
        },
        Err(err) => Outcome {
            id: item.id,
            info,
            ready: false,
            error: Some(err.to_string()),
        },
    }
}

/// 結果を書く。読めたメディア情報は空の列だけ埋める。
fn apply(tx: &Connection, outcomes: &[Outcome]) -> Result<()> {
    let now = now_ms();
    let mut stmt = tx.prepare_cached(
        "UPDATE items SET
           width = coalesce(width, ?2), height = coalesce(height, ?3),
           duration_ms = coalesce(duration_ms, ?4), captured_at = coalesce(captured_at, ?5),
           codec_info = coalesce(codec_info, ?6), thumb_state = ?7, updated_at = ?8
         WHERE id = ?1",
    )?;
    for outcome in outcomes {
        let info = outcome.info.as_ref();
        stmt.execute(params![
            outcome.id,
            info.and_then(|i| i.width),
            info.and_then(|i| i.height),
            info.and_then(|i| i.duration_ms),
            info.and_then(|i| i.captured_at),
            info.and_then(MediaInfo::codec_json),
            if outcome.ready { "ready" } else { "failed" },
            now,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{EventSink, JobKind, JobManager, JobState};
    use crate::library::Library;
    use crate::media::fake::FakeBackend;
    use std::time::Duration;

    /// 種類・拡張子・既知のメタデータを指定してアイテムと実ファイルを入れる。
    fn add(
        lib: &Library,
        id: i64,
        kind: &str,
        ext: &str,
        width: Option<i64>,
        duration: Option<i64>,
    ) {
        let hash = [id as u8; 32];
        let path = lib.layout().content_path(&hash, ext);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
        let (kind, ext) = (kind.to_string(), ext.to_string());
        lib.db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, duration_ms, title, original_name, imported_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5, ?6, 't', 't', ?1, ?1)",
                    params![id, hash.to_vec(), ext, kind, width, duration],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn run(lib: &Arc<Library>) -> JobManager {
        let sink: EventSink = Arc::new(|_| {});
        let jobs = JobManager::new(
            lib.clone(),
            vec![(
                JobKind::Thumbnail,
                Arc::new(ThumbnailHandler::new(Arc::new(FakeBackend)).with_threads(2)),
            )],
            sink,
        )
        .unwrap();
        jobs.ensure_queued(JobKind::Thumbnail, "{}").unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(Duration::from_secs(10)));
        jobs
    }

    fn row(lib: &Library, id: i64) -> (String, Option<i64>, Option<i64>, Option<String>) {
        lib.db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT thumb_state, width, duration_ms, codec_info FROM items WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?)
            })
            .unwrap()
    }

    #[test]
    fn makes_thumbnails_and_fills_missing_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        add(&lib, 1, "image", "jpg", None, None); // M1 で取り込んだもの（幅が空）
        add(&lib, 2, "video", "mov", Some(640), Some(3000)); // 取り込みで読めたもの
        add(&lib, 3, "audio", "flac", None, None);

        let jobs = run(&lib);
        let last = jobs
            .list(&crate::jobs::JobFilter {
                kinds: vec![JobKind::Thumbnail],
                states: vec![],
                limit: Some(1),
            })
            .unwrap();
        assert_eq!(last[0].state, JobState::Done);

        assert_eq!(
            row(&lib, 1),
            (
                "ready".into(),
                Some(1600),
                None,
                Some(r#"{"codec":"jpeg"}"#.into())
            )
        );
        // すでにある値は上書きしない
        assert_eq!(row(&lib, 2).1, Some(640));
        assert_eq!(row(&lib, 3).0, "ready");
        assert_eq!(row(&lib, 3).2, Some(180_000));

        let hex1 = hex(&[1u8; 32]);
        assert!(lib.layout().thumb_path(&hex1, ThumbSize::Small).is_file());
        assert!(lib.layout().thumb_path(&hex1, ThumbSize::Large).is_file());
        assert!(lib.layout().art_path(&hex(&[3u8; 32])).is_file());
        assert_eq!(lib.db().read(count_pending).unwrap(), 0);
    }

    #[test]
    fn missing_artwork_and_unsupported_files_are_marked_failed() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let layout = lib.layout().clone();
        let pending = |ext: &str, kind| Pending {
            id: 9,
            hash: [9u8; 32],
            ext: ext.into(),
            kind,
            width: None,
            height: None,
            duration_ms: None,
        };
        // 偽物はパスに「noart」を含む音楽をジャケットなしとする。失敗ではないので理由は残さない
        let noart = process(&layout, &FakeBackend, &pending("noart", MediaKind::Audio));
        assert!(!noart.ready);
        assert!(noart.error.is_none());
        // どの実装でも扱えない形式は失敗として理由を残す
        let unsupported = process(
            &layout,
            &crate::media::NoBackend,
            &pending("jpg", MediaKind::Image),
        );
        assert!(!unsupported.ready);
        assert!(unsupported.error.is_some());
    }

    /// 最後のサムネイルのジョブの記録。
    fn last_log(jobs: &JobManager) -> String {
        let last = jobs
            .list(&crate::jobs::JobFilter {
                kinds: vec![JobKind::Thumbnail],
                states: vec![],
                limit: Some(1),
            })
            .unwrap();
        jobs.log_text(last[0].id, 1 << 16).unwrap()
    }

    /// 知らない種類（DB の CHECK を外して入れたもの）を画像として描かない。失敗にして理由を残し、同じ行を拾い直さない。
    #[test]
    fn unknown_kind_is_not_treated_as_image() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        lib.db()
            .write(|tx| {
                tx.execute_batch("ALTER TABLE items DROP CONSTRAINT items_kind")?;
                Ok(())
            })
            .unwrap();
        add(&lib, 1, "sticker", "png", None, None);
        add(&lib, 2, "image", "jpg", None, None);

        let jobs = run(&lib);
        assert_eq!(row(&lib, 1).0, "failed");
        assert_eq!(row(&lib, 1).1, None, "画像として読んだ幅を書かない");
        let hex1 = hex(&[1u8; 32]);
        assert!(!lib.layout().thumb_path(&hex1, ThumbSize::Large).exists());
        assert!(!lib.layout().thumb_path(&hex1, ThumbSize::Small).exists());
        // ほかの行は止めずに作る
        assert_eq!(row(&lib, 2).0, "ready");
        assert_eq!(lib.db().read(count_pending).unwrap(), 0);
        assert!(last_log(&jobs).contains("sticker"), "{}", last_log(&jobs));
    }

    /// フォント・3D は画像として描かない（段 3 で専用の道を作る）。失敗にして理由を残し、同じ行を拾い直さない。
    /// プロキシも作らない（proxy_state は not_needed のまま）。
    #[test]
    fn fonts_and_models_are_not_drawn_as_images() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        add(&lib, 1, "font", "ttf", None, None);
        add(&lib, 2, "model", "glb", None, None);
        lib.db()
            .write(|tx| {
                tx.execute("UPDATE items SET proxy_state = 'not_needed'", [])?;
                Ok(())
            })
            .unwrap();

        let jobs = run(&lib);
        for id in [1u8, 2] {
            let (state, width, _, codec) = row(&lib, i64::from(id));
            assert_eq!(
                (state.as_str(), width, codec),
                ("failed", None, None),
                "{id}"
            );
            let h = hex(&[id; 32]);
            assert!(!lib.layout().thumb_path(&h, ThumbSize::Large).exists());
            assert!(!lib.layout().art_path(&h).exists());
        }
        assert_eq!(lib.db().read(count_pending).unwrap(), 0);
        let proxy: Vec<String> = lib
            .db()
            .read(|c| {
                let mut stmt = c.prepare("SELECT proxy_state FROM items ORDER BY id")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<std::result::Result<_, _>>()?)
            })
            .unwrap();
        assert_eq!(proxy, vec!["not_needed", "not_needed"]);
        let log = last_log(&jobs);
        assert!(
            log.contains("アイテム 1") && log.contains("アイテム 2"),
            "{log}"
        );
    }

    #[test]
    fn pending_lookup_uses_index() {
        let conn = crate::db::test_conn();
        let plan = crate::db::query_plan(
            &conn,
            "SELECT id, hash, ext, kind, width, height, duration_ms FROM items
             WHERE thumb_state = 'pending' ORDER BY imported_at DESC, id DESC LIMIT 64",
        );
        assert!(plan.contains("idx_items_thumb_pending"), "{plan}");
        let count = crate::db::query_plan(
            &conn,
            "SELECT COUNT(*) FROM items WHERE thumb_state = 'pending'",
        );
        assert!(count.contains("idx_items_thumb_pending"), "{count}");
    }
}
