//! サムネイルのジョブ。`thumb_state = 'pending'` のアイテムを新しい順に少しずつ取り出し、なくなったら `'stale'`（作り方が
//! 変わったので、今の絵を出したまま作り直すもの）を取り出す。メディア情報が足りなければ読み、形式ごとの道（[`thumb_route`]）で
//! サムネイル（s / l。透明があれば PNG、なければ JPEG）と音楽のジャケットを作る。
//!
//! - エンジンの道（png・gif・webp・aseprite・psd・svg・glb）は [`EngineThumbs`]（一括の予算と一括のプール）で描き、読めないもの・
//!   大きすぎるものは ImageIO の道へ回す。OS の道（ImageIO・CoreText・AVFoundation・ffmpeg）はサムネイルのプール
//!   （`kura-thumb-*`）で並べて作る。サムネイルのプールの中ではエンジンを呼ばない。
//! - 書くときは `thumb_rev` を +1 し（URL の `?r=`）、UPDATE は `AND hash = ?`（読んだあとに中身が変わっていたら書かない）。
//!   書いたら、ほかの形式の古いファイルを消す。stale のものを作り直せなかったときは、前の絵のまま ready に戻す。
//! - 作る前に作っているアイテムの印を付け、結果と同じトランザクションで消す（[`inflight`]。OS のデコーダで落ちたときに、
//!   同じファイルで落ち続けないように）。前回落ちたときにまとめて作っていたものは、先に 1 件ずつ作る。
//!
//! 進み具合はアイテムごとの thumb_state そのもの（強制終了しても、続きは pending・stale のものから）。

use std::sync::Arc;

use kura_engine::Monitor;
use kura_engine::model::ModelLimits;
use rayon::prelude::*;
use rusqlite::{Connection, params};

use super::engine_thumbs::{
    EngineDone, EngineJob, EngineThumbs, EngineWhat, INFLIGHT_CAP, glb_info,
};
use super::route::{ThumbRoute, sync_plan, thumb_route};
use super::{MediaBackend, MediaError, MediaInfo, ModelInfo, ThumbFormat, ThumbTargets, inflight};
use crate::db::Db;
use crate::edit::EngineServices;
use crate::import::{MediaKind, format_id};
use crate::jobs::{Control, JobContext, JobEnd, JobHandler};
use crate::library::{Layout, hex};
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
    /// items.codec_info があるか（フォント・3D の情報を読んだか）。
    has_codec_info: bool,
    /// 作り方が変わったので作り直すもの（今の絵がある）。
    stale: bool,
}

impl Pending {
    /// メディア情報を読み直す必要があるか（取り込みのときに読めなかった、M1 で取り込んだ、フォント・3D の情報がない、など）。
    fn needs_probe(&self) -> bool {
        match self.kind {
            MediaKind::Image => unknown(self.width) || unknown(self.height),
            MediaKind::Video => unknown(self.width) || self.duration_ms.is_none(),
            MediaKind::Audio => self.duration_ms.is_none(),
            MediaKind::Font | MediaKind::Model => !self.has_codec_info,
        }
    }

    fn route(&self) -> ThumbRoute {
        thumb_route(self.kind, &self.ext)
    }
}

/// 幅・高さが分からないか（空。ImageIO は SVG を 0×0 と読むので 0 も分からないとする）。
fn unknown(v: Option<i64>) -> bool {
    v.is_none_or(|v| v <= 0)
}

/// 1 件の終わり方。
#[derive(Debug, Clone, PartialEq)]
enum Done {
    /// 書いた（形式）。
    Ready(ThumbFormat),
    /// ジャケットのない音楽（失敗ではないので理由は残さない）。
    NoArt,
    /// 作れなかった（理由）。
    Failed(String),
    /// 中止・一時停止で作らなかった（何も書かない）。
    Skipped,
}

/// 1 件の結果。
#[derive(Debug)]
struct Outcome {
    id: i64,
    hash: Vec<u8>,
    stale: bool,
    info: Option<MediaInfo>,
    done: Done,
    /// ログに残すこと（エンジンから ImageIO へ回した理由など）。
    note: Option<String>,
}

impl Outcome {
    fn new(item: &Pending, info: Option<MediaInfo>, done: Done) -> Self {
        Self {
            id: item.id,
            hash: item.hash.to_vec(),
            stale: item.stale,
            info,
            done,
            note: None,
        }
    }

    fn error(&self) -> Option<&str> {
        match &self.done {
            Done::Failed(e) => Some(e),
            _ => None,
        }
    }
}

/// サムネイルのジョブの処理。
pub struct ThumbnailHandler {
    backend: Arc<dyn MediaBackend>,
    engine: Option<Arc<EngineServices>>,
    threads: usize,
    engine_cap: u64,
}

impl ThumbnailHandler {
    /// `backend` でサムネイルを作る。並列数は CPU コア数。エンジンなし（画像はすべて ImageIO の道、3D は情報だけ）。
    pub fn new(backend: Arc<dyn MediaBackend>) -> Self {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            backend,
            engine: None,
            threads,
            engine_cap: INFLIGHT_CAP,
        }
    }

    /// エンジンの道に使う、ライブラリのエンジンの部品（実行の場とコーデックの組は、エンジンの道のアイテムに最初に出会ったときに
    /// 作る）。None ならエンジンを使わない。
    pub fn with_engine(mut self, engine: Option<Arc<EngineServices>>) -> Self {
        self.engine = engine;
        self
    }

    /// 並列数を変える（計測用）。
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// エンジンの道の同時の確保の上限を変える（テスト・計測用。既定は 256 MiB）。
    pub fn with_engine_cap(mut self, cap: u64) -> Self {
        self.engine_cap = cap.max(1);
        self
    }
}

/// ジョブの一時停止・中止をエンジンの中止として見せる。
struct JobMonitor<'a>(&'a JobContext);

impl Monitor for JobMonitor<'_> {
    fn canceled(&self) -> bool {
        !matches!(self.0.control(), Control::Continue)
    }
}

/// エンジンの道を、最初に使うときに作る（ライブラリを開いただけでは作らない）。
struct EngineSlot<'a> {
    services: Option<&'a Arc<EngineServices>>,
    cap: u64,
    built: Option<Option<EngineThumbs>>,
}

impl EngineSlot<'_> {
    fn get(&mut self) -> Option<&EngineThumbs> {
        if self.built.is_none() {
            let thumbs = self
                .services
                .and_then(|services| match EngineThumbs::new(services) {
                    Ok(thumbs) => Some(thumbs.with_cap(self.cap)),
                    Err(err) => {
                        tracing::warn!(%err, "エンジンの道を使えないので、画像は ImageIO の道で作ります");
                        None
                    }
                });
            self.built = Some(thumbs);
        }
        self.built.as_ref().and_then(Option::as_ref)
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
        let mon = JobMonitor(ctx);
        let mut engine = EngineSlot {
            services: self.engine.as_ref(),
            cap: self.engine_cap,
            built: None,
        };

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
            // 前回落ちたときにまとめて作っていたもの（疑わしいもの）を、先に 1 件ずつ作る（1 件だけの印を付けて）
            let suspect = library
                .db()
                .read(|conn| Ok(inflight::suspects(conn)?.first().copied()))?;
            let (batch, unknown, solo) = if let Some(id) = suspect {
                match library.db().read(|conn| pending_by_id(conn, id))? {
                    Some(item) => (vec![item], Vec::new(), Some(id)),
                    None => {
                        library
                            .db()
                            .write(move |tx| inflight::drop_suspect(tx, id))?;
                        continue;
                    }
                }
            } else {
                let (batch, unknown) =
                    library.db().read(|conn| pending_batch(conn, THUMB_BATCH))?;
                (batch, unknown, None)
            };
            if batch.is_empty() && unknown.is_empty() {
                break;
            }
            let ids: Vec<i64> = batch.iter().map(|p| p.id).collect();
            if !ids.is_empty() {
                library
                    .db()
                    .write(move |tx| inflight::mark(tx, &ids, solo.is_some()))?;
            }
            let mut outcomes = self.process(&layout, &pool, &mut engine, batch, &mon);
            outcomes.extend(unknown);
            for outcome in &outcomes {
                if let Some(note) = &outcome.note {
                    ctx.log(&format!("アイテム {}: {note}", outcome.id));
                }
                if let Some(error) = outcome.error() {
                    ctx.log(&format!("アイテム {}: {error}", outcome.id));
                }
            }
            let finished: Vec<&Outcome> = outcomes
                .iter()
                .filter(|o| o.done != Done::Skipped)
                .collect();
            done += finished.len() as u64;
            failed += finished
                .iter()
                .filter(|o| !matches!(o.done, Done::Ready(_)))
                .count() as u64;
            library.db().write(move |tx| {
                apply(tx, &outcomes)?;
                if let Some(id) = solo {
                    inflight::drop_suspect(tx, id)?;
                }
                inflight::clear(tx)
            })?;

            let remaining = library.db().read(count_remaining)?;
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

impl ThumbnailHandler {
    /// 取り出したものを道ごとに作る（DB には触らない）：エンジンの道を先に（ジョブのスレッドで見積もり、一括のプールで描く）、
    /// 残りと ImageIO へ回したものをサムネイルのプールで並べて。
    fn process(
        &self,
        layout: &Layout,
        pool: &rayon::ThreadPool,
        engine: &mut EngineSlot<'_>,
        items: Vec<Pending>,
        mon: &dyn Monitor,
    ) -> Vec<Outcome> {
        let mut outcomes = Vec::with_capacity(items.len());
        let mut os: Vec<(Pending, ThumbRoute, Option<String>)> = Vec::new();
        let mut on_engine: Vec<Pending> = Vec::new();
        for item in items {
            match item.route() {
                ThumbRoute::Engine | ThumbRoute::Model => on_engine.push(item),
                route => os.push((item, route, None)),
            }
        }
        if !on_engine.is_empty() {
            match engine.get() {
                Some(thumbs) => {
                    let jobs: Vec<EngineJob> = on_engine
                        .iter()
                        .map(|item| engine_job(layout, item))
                        .collect();
                    let results = thumbs.render(&jobs, mon);
                    for (item, result) in on_engine.into_iter().zip(results) {
                        match engine_outcome(&item, result) {
                            Ok(outcome) => outcomes.push(outcome),
                            Err(reason) => os.push((
                                item,
                                ThumbRoute::ImageIo,
                                Some(format!(
                                    "エンジンで作れないので ImageIO で作ります（{reason}）"
                                )),
                            )),
                        }
                    }
                }
                None => {
                    for item in on_engine {
                        match item.route() {
                            ThumbRoute::Model => outcomes.push(model_without_engine(layout, &item)),
                            _ => os.push((item, ThumbRoute::ImageIo, None)),
                        }
                    }
                }
            }
        }
        let backend = self.backend.as_ref();
        let os_outcomes: Vec<Outcome> = pool.install(|| {
            os.par_iter()
                .map(|(item, route, note)| {
                    let mut outcome = process_os(layout, backend, item, *route);
                    if outcome.note.is_none() {
                        outcome.note = note.clone();
                    }
                    outcome
                })
                .collect()
        });
        outcomes.extend(os_outcomes);
        outcomes
    }
}

/// エンジンの道の 1 件。
fn engine_job(layout: &Layout, item: &Pending) -> EngineJob {
    let what = match item.route() {
        ThumbRoute::Model => EngineWhat::Model,
        _ => EngineWhat::Image {
            format: format_id(&item.ext.to_ascii_lowercase()).unwrap_or("png"),
        },
    };
    EngineJob {
        path: layout.content_path(&item.hash, &item.ext),
        what,
        targets: ThumbTargets::for_hash(layout, &hex(&item.hash)),
    }
}

/// エンジンの道の結果を、アイテムの結果にする。ImageIO の道へ回すものは Err（理由）。
fn engine_outcome(item: &Pending, result: EngineDone) -> std::result::Result<Outcome, String> {
    let format = format_id(&item.ext.to_ascii_lowercase()).map(str::to_string);
    let model_info = |model: Option<Box<ModelInfo>>| {
        model.map(|m| MediaInfo {
            codec: Some("glb".into()),
            model: Some(*m),
            ..Default::default()
        })
    };
    Ok(match result {
        EngineDone::Written {
            format: written,
            size,
            model,
        } => {
            let info = match model {
                Some(_) => model_info(model),
                None => size.map(|(w, h)| MediaInfo {
                    width: Some(i64::from(w)),
                    height: Some(i64::from(h)),
                    codec: format,
                    ..Default::default()
                }),
            };
            Outcome::new(item, info, Done::Ready(written))
        }
        EngineDone::Fallback(reason) => return Err(reason),
        EngineDone::Failed { error, model } => {
            Outcome::new(item, model_info(model), Done::Failed(error))
        }
        EngineDone::Canceled => Outcome::new(item, None, Done::Skipped),
    })
}

/// エンジンがないときの 3D：情報だけ読み（kura-engine の読み手。プールは使わない）、サムネイルは作らない。
fn model_without_engine(layout: &Layout, item: &Pending) -> Outcome {
    let path = layout.content_path(&item.hash, &item.ext);
    let info = glb_info(&path, &ModelLimits::default())
        .ok()
        .map(|(info, _)| MediaInfo {
            codec: Some("glb".into()),
            model: Some(ModelInfo::from(&info)),
            ..Default::default()
        });
    Outcome::new(
        item,
        info,
        Done::Failed("エンジンの部品がないので、3D のサムネイルは作りません".into()),
    )
}

fn summary(done: u64, failed: u64) -> String {
    serde_json::json!({ "done": done, "failed": failed }).to_string()
}

const COLUMNS: &str =
    "id, hash, ext, kind, width, height, duration_ms, codec_info IS NOT NULL, thumb_state";

/// 行を読む。知らない種類（DB の CHECK が外れていたなど）は画像として描かず、失敗の結果にして返す（同じ行を拾い直さない）。
fn read_rows(
    stmt: &mut rusqlite::CachedStatement<'_>,
    params: impl rusqlite::Params,
) -> Result<(Vec<Pending>, Vec<Outcome>)> {
    let rows = stmt.query_map(params, |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, bool>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    let (mut out, mut unknown) = (Vec::new(), Vec::new());
    for row in rows {
        let (id, hash, ext, kind, width, height, duration_ms, has_codec_info, state) = row?;
        let stale = state == "stale";
        let Some(kind) = MediaKind::parse(&kind) else {
            unknown.push(Outcome {
                id,
                hash,
                stale,
                info: None,
                done: Done::Failed(format!(
                    "知らない種類「{kind}」なので、サムネイルを作りません"
                )),
                note: None,
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
            has_codec_info,
            stale,
        });
    }
    Ok((out, unknown))
}

/// 作るものを新しい順に取り出す：pending（idx_items_thumb_pending）を先に、なくなったら stale（idx_items_thumb_stale）。
fn pending_batch(conn: &Connection, limit: usize) -> Result<(Vec<Pending>, Vec<Outcome>)> {
    let mut pending = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM items
         WHERE thumb_state = 'pending' ORDER BY imported_at DESC, id DESC LIMIT ?1"
    ))?;
    let found = read_rows(&mut pending, [limit as i64])?;
    if !found.0.is_empty() || !found.1.is_empty() {
        return Ok(found);
    }
    let mut stale = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM items
         WHERE thumb_state = 'stale' ORDER BY imported_at DESC, id DESC LIMIT ?1"
    ))?;
    read_rows(&mut stale, [limit as i64])
}

/// 疑わしいもの 1 件（まだ作っていなければ）。
fn pending_by_id(conn: &Connection, id: i64) -> Result<Option<Pending>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM items WHERE id = ?1 AND thumb_state IN ('pending', 'stale')"
    ))?;
    let (mut found, _) = read_rows(&mut stmt, [id])?;
    Ok(found.pop())
}

/// 作るもの（pending と stale）の数。
fn count_remaining(conn: &Connection) -> Result<u64> {
    let mut stmt = conn.prepare_cached(
        "SELECT (SELECT COUNT(*) FROM items WHERE thumb_state = 'pending')
              + (SELECT COUNT(*) FROM items WHERE thumb_state = 'stale')",
    )?;
    let count: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// 作るもの（pending・stale・疑わしいもの）があるか。
fn has_work(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM items WHERE thumb_state = 'pending')
             OR EXISTS (SELECT 1 FROM items WHERE thumb_state = 'stale')",
    )?;
    let any: bool = stmt.query_row([], |row| row.get(0))?;
    Ok(any || !inflight::suspects(conn)?.is_empty())
}

/// ライブラリを開くとき（ジョブを動かす前に。library/session.rs から呼ぶ）：前回落ちたときの印を片付け（[`inflight::recover`]）、
/// サムネイルの作り方の指紋を照らして、変わった形式を作り直すものにする（[`sync_plan`]）。作るものがあれば true
/// （サムネイルのジョブを積む）。失敗はログに出して先へ進む。
pub fn prepare_on_open(db: &Db) -> bool {
    if let Err(err) = db.write(|tx| inflight::recover(tx)) {
        tracing::warn!(%err, "サムネイルの途中の印を片付けられませんでした");
    }
    if let Err(err) = db.write(|tx| sync_plan(tx)) {
        tracing::warn!(%err, "サムネイルの作り方の指紋を照らせませんでした");
    }
    db.read(has_work).unwrap_or_else(|err| {
        tracing::warn!(%err, "サムネイルを作るものがあるか確かめられませんでした");
        true
    })
}

/// OS の道の 1 件を作る（DB には触らない）。
fn process_os(
    layout: &Layout,
    backend: &dyn MediaBackend,
    item: &Pending,
    route: ThumbRoute,
) -> Outcome {
    let src = layout.content_path(&item.hash, &item.ext);
    let hash_hex = hex(&item.hash);
    let info = if item.needs_probe() {
        backend.probe(&src, item.kind).ok()
    } else {
        None
    };
    let targets = ThumbTargets::for_hash(layout, &hash_hex);
    let result: std::result::Result<Option<ThumbFormat>, MediaError> = match route {
        ThumbRoute::ImageIo | ThumbRoute::Engine => {
            backend.image_thumbnails(&src, &targets).map(Some)
        }
        ThumbRoute::Video => {
            // 長さの 10% の地点（わからなければ 1 秒）
            let duration = info
                .as_ref()
                .and_then(|i| i.duration_ms)
                .or(item.duration_ms);
            let at = duration.map(|ms| ms as f64 / 1000.0 * 0.1).unwrap_or(1.0);
            backend.video_thumbnails(&src, at, &targets).map(Some)
        }
        // ジャケットがなければサムネイルなし（グリッドは音楽の表示にする）
        ThumbRoute::Audio => backend.audio_artwork(&src, &layout.art_path(&hash_hex), &targets),
        ThumbRoute::Font => backend.font_thumbnails(&src, &targets).map(Some),
        ThumbRoute::Model => Err(MediaError::Failed(
            "3D はエンジンの道で作ります（内部の誤り）".into(),
        )),
    };
    match result {
        Ok(Some(format)) => {
            let mut outcome = Outcome::new(item, info, Done::Ready(format));
            if let Err(err) = targets.remove_others(format) {
                outcome.note = Some(format!("前の形式のサムネイルを消せませんでした: {err}"));
            }
            outcome
        }
        Ok(None) => Outcome::new(item, info, Done::NoArt),
        Err(err) => Outcome::new(item, info, Done::Failed(err.to_string())),
    }
}

/// 結果を書く。読めたメディア情報は空の列だけ埋める。書いたら thumb_rev を +1。作り直し（stale）を作れなかったときは、前の絵の
/// まま ready に戻す。どれも、読んだときと中身（ハッシュ）が同じときだけ書く。
fn apply(tx: &Connection, outcomes: &[Outcome]) -> Result<()> {
    let now = now_ms();
    let mut stmt = tx.prepare_cached(
        "UPDATE items SET
           width = coalesce(nullif(width, 0), ?3), height = coalesce(nullif(height, 0), ?4),
           duration_ms = coalesce(duration_ms, ?5), captured_at = coalesce(captured_at, ?6),
           codec_info = coalesce(codec_info, ?7), thumb_state = ?8, thumb_rev = thumb_rev + ?9,
           updated_at = ?10
         WHERE id = ?1 AND hash = ?2",
    )?;
    for outcome in outcomes {
        let (state, rev) = match (&outcome.done, outcome.stale) {
            (Done::Skipped, _) => continue,
            (Done::Ready(_), _) => ("ready", 1),
            (Done::NoArt | Done::Failed(_), true) => ("ready", 0),
            (Done::NoArt | Done::Failed(_), false) => ("failed", 0),
        };
        let info = outcome.info.as_ref();
        stmt.execute(params![
            outcome.id,
            outcome.hash,
            info.and_then(|i| i.width),
            info.and_then(|i| i.height),
            info.and_then(|i| i.duration_ms),
            info.and_then(|i| i.captured_at),
            info.and_then(MediaInfo::codec_json),
            state,
            rev,
            now,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::edit::services::fake::FakeEngineFactory;
    use crate::jobs::{EventSink, JobKind, JobManager, JobState};
    use crate::library::{Library, LibrarySession, Services, ThumbSize};
    use crate::media::engine_thumbs::test_png;
    use crate::media::fake::{self, FakeBackend};
    use crate::media::inflight::{INFLIGHT_KEY, Inflight, SUSPECTS_KEY};
    use crate::media::{FontInfo, NoBackend};
    use crate::settings;

    /// 種類・拡張子・既知のメタデータを指定してアイテムと実ファイル（中身 `bytes`）を入れる。
    fn add_bytes(
        lib: &Library,
        id: i64,
        kind: &str,
        ext: &str,
        width: Option<i64>,
        duration: Option<i64>,
        bytes: &[u8],
    ) {
        let hash = [id as u8; 32];
        let path = lib.layout().content_path(&hash, ext);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
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

    fn add(
        lib: &Library,
        id: i64,
        kind: &str,
        ext: &str,
        width: Option<i64>,
        duration: Option<i64>,
    ) {
        add_bytes(lib, id, kind, ext, width, duration, b"x");
    }

    fn sql(lib: &Library, sql: &'static str) {
        lib.db()
            .write(move |tx| {
                tx.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
    }

    fn run_with(lib: &Arc<Library>, handler: ThumbnailHandler) -> JobManager {
        let sink: EventSink = Arc::new(|_| {});
        let jobs = JobManager::new(
            lib.clone(),
            vec![(JobKind::Thumbnail, Arc::new(handler.with_threads(2)))],
            sink,
        )
        .unwrap();
        jobs.ensure_queued(JobKind::Thumbnail, "{}").unwrap();
        jobs.start().unwrap();
        assert!(jobs.wait_idle(Duration::from_secs(20)));
        jobs
    }

    fn run(lib: &Arc<Library>) -> JobManager {
        run_with(lib, ThumbnailHandler::new(Arc::new(FakeBackend)))
    }

    /// 固定の段のコーデックの組と、予算 1 GiB の小さな実行の場（偽の工場の 64 MiB では 3D の見積もりが収まらない）。
    struct Factory;

    impl crate::edit::EngineFactory for Factory {
        fn runtime_config(&self) -> crate::edit::RuntimeConfig {
            crate::edit::RuntimeConfig {
                cores: Some(2),
                budget: Some(1 << 30),
                ..Default::default()
            }
        }
        fn codecs(&self, _: &Path) -> kura_engine::codec::CodecSet {
            kura_engine::codec::CodecSet::pinned()
        }
    }

    fn engine(dir: &Path) -> Option<Arc<EngineServices>> {
        Some(Arc::new(EngineServices::new(
            Arc::new(Factory),
            dir.join("engine"),
        )))
    }

    type Row = (String, Option<i64>, Option<i64>, Option<String>);

    fn row(lib: &Library, id: i64) -> Row {
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

    fn rev(lib: &Library, id: i64) -> i64 {
        lib.db()
            .read(|c| {
                Ok(
                    c.query_row("SELECT thumb_rev FROM items WHERE id = ?1", [id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap()
    }

    fn targets(lib: &Library, id: u8) -> ThumbTargets {
        ThumbTargets::for_hash(lib.layout(), &hex(&[id; 32]))
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

    fn inflight_now(lib: &Library) -> Option<Inflight> {
        lib.db()
            .read(|c| settings::get::<Inflight>(c, INFLIGHT_KEY))
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
        // 作ったら thumb_rev が 1 になる（URL の ?r=1）
        assert_eq!((rev(&lib, 1), rev(&lib, 2), rev(&lib, 3)), (1, 1, 1));

        let hex1 = hex(&[1u8; 32]);
        assert!(lib.layout().thumb_path(&hex1, ThumbSize::Small).is_file());
        assert!(lib.layout().thumb_path(&hex1, ThumbSize::Large).is_file());
        assert!(lib.layout().art_path(&hex(&[3u8; 32])).is_file());
        assert_eq!(lib.db().read(count_remaining).unwrap(), 0);
        assert_eq!(inflight_now(&lib), None, "途中の印を消していない");
    }

    fn pending(ext: &str, kind: MediaKind) -> Pending {
        Pending {
            id: 9,
            hash: [9u8; 32],
            ext: ext.into(),
            kind,
            width: None,
            height: None,
            duration_ms: None,
            has_codec_info: false,
            stale: false,
        }
    }

    #[test]
    fn missing_artwork_and_unsupported_files_are_marked_failed() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let layout = lib.layout().clone();
        // 偽物はパスに「noart」を含む音楽をジャケットなしとする。失敗ではないので理由は残さない
        let noart = process_os(
            &layout,
            &FakeBackend,
            &pending("noart", MediaKind::Audio),
            ThumbRoute::Audio,
        );
        assert_eq!(noart.done, Done::NoArt);
        assert!(noart.error().is_none());
        // どの実装でも扱えない形式は失敗として理由を残す
        let unsupported = process_os(
            &layout,
            &NoBackend,
            &pending("jpg", MediaKind::Image),
            ThumbRoute::ImageIo,
        );
        assert!(matches!(unsupported.done, Done::Failed(_)));
        assert!(unsupported.error().is_some());
    }

    /// 知らない種類（DB の CHECK を外して入れたもの）を画像として描かない。失敗にして理由を残し、同じ行を拾い直さない。
    #[test]
    fn unknown_kind_is_not_treated_as_image() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        sql(&lib, "ALTER TABLE items DROP CONSTRAINT items_kind");
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
        assert_eq!(lib.db().read(count_remaining).unwrap(), 0);
        assert!(last_log(&jobs).contains("sticker"), "{}", last_log(&jobs));
    }

    /// 決定 9・plan defaults 4：フォントは CoreText の道（偽物）で PNG の見本、情報（書体名・字の数）は codec_info の `font`。
    /// 3D はエンジンの描画で PNG、情報は codec_info の `model`。どちらも幅・高さは空のまま、プロキシは作らない。エンジンが
    /// なければ 3D は情報だけ（作れなかったもの）。
    #[test]
    fn fonts_and_models_get_their_own_thumbnails_and_info() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        let glbs = kura_engine::model::sample_glbs().unwrap();
        let cube = &glbs.iter().find(|(n, _)| *n == "cube").unwrap().1;
        add(&lib, 1, "font", "ttf", None, None);
        add_bytes(&lib, 2, "model", "glb", None, None, cube);
        add_bytes(&lib, 3, "model", "glb", None, None, cube);
        sql(&lib, "UPDATE items SET proxy_state = 'not_needed'");
        // 3 はあとでエンジンなしで作る
        sql(&lib, "UPDATE items SET thumb_state = 'ready' WHERE id = 3");

        run_with(
            &lib,
            ThumbnailHandler::new(Arc::new(FakeBackend)).with_engine(engine(dir.path())),
        );
        let (state, width, _, codec) = row(&lib, 1);
        assert_eq!((state.as_str(), width), ("ready", None));
        let json: serde_json::Value = serde_json::from_str(&codec.unwrap()).unwrap();
        assert_eq!(json["codec"], "ttf");
        assert_eq!(json["font"]["family"], "偽の書体");
        assert_eq!(json["font"]["glyphs"], 42);
        assert!(targets(&lib, 1).files(ThumbFormat::Png).1.is_file());

        let (state, width, _, codec) = row(&lib, 2);
        assert_eq!((state.as_str(), width), ("ready", None));
        let json: serde_json::Value = serde_json::from_str(&codec.unwrap()).unwrap();
        assert_eq!(json["codec"], "glb");
        assert_eq!(json["model"]["triangles"], 12);
        assert_eq!(json["model"]["materials"], 6);
        let (l, s) = targets(&lib, 2).files(ThumbFormat::Png);
        assert!(l.is_file() && s.is_file());
        assert_eq!(&std::fs::read(&s).unwrap()[1..4], b"PNG");

        // エンジンなし：3D は情報だけ読み、サムネイルは作らない
        sql(
            &lib,
            "UPDATE items SET thumb_state = 'pending' WHERE id = 3",
        );
        let jobs = run(&lib);
        let (state, width, _, codec) = row(&lib, 3);
        assert_eq!((state.as_str(), width), ("failed", None));
        assert!(codec.unwrap().contains("\"triangles\":12"));
        assert!(!targets(&lib, 3).files(ThumbFormat::Png).0.exists());
        assert!(last_log(&jobs).contains("エンジンの部品がない"));

        let proxy: Vec<String> = lib
            .db()
            .read(|c| {
                let mut stmt = c.prepare("SELECT proxy_state FROM items ORDER BY id")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<std::result::Result<_, _>>()?)
            })
            .unwrap();
        assert_eq!(proxy, vec!["not_needed"; 3]);
    }

    /// エンジンの道：png は kura-engine で描き（透明があれば PNG）、読めた幅・高さを空欄（と 0。ImageIO は SVG を 0×0 と読む）
    /// に入れる。エンジンで読めないものは ImageIO の道（偽物は JPEG）へ回し、理由をログに残す。
    #[test]
    fn engine_route_fills_the_size_and_falls_back_to_imageio() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        add_bytes(
            &lib,
            1,
            "image",
            "png",
            Some(0),
            None,
            &test_png(300, 200, true),
        );
        add_bytes(
            &lib,
            2,
            "image",
            "png",
            Some(0),
            None,
            b"\x89PNG\r\n\x1a\n broken",
        );
        add_bytes(
            &lib,
            3,
            "image",
            "gif",
            None,
            None,
            &test_png(20, 10, false),
        );
        let jobs = run_with(
            &lib,
            ThumbnailHandler::new(Arc::new(FakeBackend)).with_engine(engine(dir.path())),
        );
        let (state, width, _, codec) = row(&lib, 1);
        assert_eq!((state.as_str(), width), ("ready", Some(300)));
        assert_eq!(codec.as_deref(), Some(r#"{"codec":"png"}"#));
        let (l, s) = targets(&lib, 1).files(ThumbFormat::Png);
        assert!(l.is_file() && s.is_file());
        // 読めない png は ImageIO の道（偽物）で作り、偽物の情報が入る
        assert_eq!(row(&lib, 2).0, "ready");
        assert_eq!(row(&lib, 2).1, Some(1600));
        assert!(targets(&lib, 2).files(ThumbFormat::Jpeg).0.is_file());
        assert!(!targets(&lib, 2).files(ThumbFormat::Png).0.exists());
        assert!(last_log(&jobs).contains("ImageIO"), "{}", last_log(&jobs));
        // 拡張子が gif でも中身が png なら、エンジンは読めずに ImageIO へ回す
        assert_eq!(row(&lib, 3).0, "ready");
    }

    /// 偽物を包んで、呼ばれた順・そのときの途中の印・失敗させるアイテムを覚える。
    struct Recording {
        lib: Arc<Library>,
        calls: Mutex<Vec<(i64, Option<Inflight>)>>,
        fail: Vec<i64>,
    }

    impl Recording {
        fn new(lib: &Arc<Library>) -> Arc<Self> {
            Self::failing(lib, Vec::new())
        }
        fn failing(lib: &Arc<Library>, fail: Vec<i64>) -> Arc<Self> {
            Arc::new(Self {
                lib: lib.clone(),
                calls: Mutex::new(Vec::new()),
                fail,
            })
        }
        fn id_of(path: &Path) -> i64 {
            let name = path.file_stem().unwrap().to_string_lossy();
            i64::from(u8::from_str_radix(&name[..2], 16).unwrap())
        }
        fn calls(&self) -> Vec<(i64, Option<Inflight>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MediaBackend for Recording {
        fn probe(
            &self,
            path: &Path,
            kind: MediaKind,
        ) -> std::result::Result<MediaInfo, MediaError> {
            FakeBackend.probe(path, kind)
        }
        fn image_thumbnails(
            &self,
            path: &Path,
            out: &ThumbTargets,
        ) -> std::result::Result<ThumbFormat, MediaError> {
            let id = Self::id_of(path);
            let mark = self
                .lib
                .db()
                .read(|c| settings::get::<Inflight>(c, INFLIGHT_KEY))
                .unwrap();
            self.calls.lock().unwrap().push((id, mark));
            if self.fail.contains(&id) {
                return Err(MediaError::Failed("作れませんでした".into()));
            }
            FakeBackend.image_thumbnails(path, out)
        }
        fn video_thumbnails(
            &self,
            path: &Path,
            at: f64,
            out: &ThumbTargets,
        ) -> std::result::Result<ThumbFormat, MediaError> {
            FakeBackend.video_thumbnails(path, at, out)
        }
        fn audio_artwork(
            &self,
            path: &Path,
            art: &Path,
            out: &ThumbTargets,
        ) -> std::result::Result<Option<ThumbFormat>, MediaError> {
            FakeBackend.audio_artwork(path, art, out)
        }
        fn font_thumbnails(
            &self,
            path: &Path,
            out: &ThumbTargets,
        ) -> std::result::Result<ThumbFormat, MediaError> {
            FakeBackend.font_thumbnails(path, out)
        }
    }

    /// plan tests 12：stale は pending のあとに作り直し、ready・thumb_rev+1 になる。別の形式の古いファイルは消える。作り直せ
    /// なかった stale は、前の絵のまま ready に戻す（thumb_rev は変えない）。
    #[test]
    fn stale_thumbs_stay_visible_and_are_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        add(&lib, 1, "image", "heic", Some(10), None);
        add(&lib, 2, "image", "jpg", Some(10), None);
        add(&lib, 3, "image", "tif", Some(10), None);
        for id in [1u8, 3] {
            fake::write(&targets(&lib, id), ThumbFormat::Jpeg, b"OLD", b"OLD").unwrap();
        }
        sql(
            &lib,
            "UPDATE items SET thumb_state = 'stale', thumb_rev = 3 WHERE id IN (1, 3);
             UPDATE items SET imported_at = 99 WHERE id = 1",
        );
        let backend = Recording::failing(&lib, vec![3]);
        run_with(&lib, ThumbnailHandler::new(backend.clone()));

        // pending（2）を先に、stale（1・3。同じ取り出しの中は並べて作るので順は決まらない）をあとに
        let mut order: Vec<i64> = backend.calls().into_iter().map(|(id, _)| id).collect();
        assert_eq!(order.first(), Some(&2), "{order:?}");
        order[1..].sort();
        assert_eq!(order, vec![2, 1, 3]);
        assert_eq!((row(&lib, 1).0.as_str(), rev(&lib, 1)), ("ready", 4));
        let (pl, ps) = targets(&lib, 1).files(ThumbFormat::Png);
        assert!(pl.is_file() && ps.is_file());
        let (jl, js) = targets(&lib, 1).files(ThumbFormat::Jpeg);
        assert!(
            !jl.exists() && !js.exists(),
            "古い形式のファイルが残っている"
        );
        assert_eq!((row(&lib, 2).0.as_str(), rev(&lib, 2)), ("ready", 1));
        // 作り直せなかった stale は前の絵のまま
        assert_eq!((row(&lib, 3).0.as_str(), rev(&lib, 3)), ("ready", 3));
        assert_eq!(
            std::fs::read(targets(&lib, 3).files(ThumbFormat::Jpeg).0).unwrap(),
            b"OLD"
        );
        assert_eq!(lib.db().read(count_remaining).unwrap(), 0);
    }

    /// 書き込みは、読んだときとハッシュが同じときだけ（読んだあとに中身が変わっていたら書かない）。
    #[test]
    fn writes_are_guarded_by_the_hash() {
        let conn = crate::db::test_conn();
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (1, ?1, 'jpg', 'image', 1, 't', 't', 1, 1)",
            [vec![1u8; 32]],
        )
        .unwrap();
        let outcome = |hash: [u8; 32]| Outcome {
            id: 1,
            hash: hash.to_vec(),
            stale: false,
            info: Some(MediaInfo {
                width: Some(5),
                ..Default::default()
            }),
            done: Done::Ready(ThumbFormat::Png),
            note: None,
        };
        apply(&conn, &[outcome([2u8; 32])]).unwrap();
        let state = |c: &Connection| -> (String, i64, Option<i64>) {
            c.query_row(
                "SELECT thumb_state, thumb_rev, width FROM items WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        };
        assert_eq!(state(&conn), ("pending".into(), 0, None));
        apply(&conn, &[outcome([1u8; 32])]).unwrap();
        assert_eq!(state(&conn), ("ready".into(), 1, Some(5)));
        // 中止したもの（Skipped）は書かない
        let mut skipped = outcome([1u8; 32]);
        skipped.done = Done::Skipped;
        apply(&conn, &[skipped]).unwrap();
        assert_eq!(state(&conn).1, 1);
    }

    /// 取り出しと数え方は pending・stale の部分索引を使う。
    #[test]
    fn pending_and_stale_lookups_use_their_indexes() {
        let conn = crate::db::test_conn();
        for (state, index) in [
            ("pending", "idx_items_thumb_pending"),
            ("stale", "idx_items_thumb_stale"),
        ] {
            let plan = crate::db::query_plan(
                &conn,
                &format!(
                    "SELECT {COLUMNS} FROM items
                     WHERE thumb_state = '{state}' ORDER BY imported_at DESC, id DESC LIMIT 64"
                ),
            );
            assert!(plan.contains(index), "{plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        }
        for sql in [
            "SELECT (SELECT COUNT(*) FROM items WHERE thumb_state = 'pending')
                  + (SELECT COUNT(*) FROM items WHERE thumb_state = 'stale')",
            "SELECT EXISTS (SELECT 1 FROM items WHERE thumb_state = 'pending')
                 OR EXISTS (SELECT 1 FROM items WHERE thumb_state = 'stale')",
        ] {
            let plan = crate::db::query_plan(&conn, sql);
            assert!(
                plan.contains("idx_items_thumb_pending") && plan.contains("idx_items_thumb_stale"),
                "{plan}"
            );
            // 部分索引の中だけを数える（表そのものは走査しない）
            assert!(!plan.split(" / ").any(|s| s == "SCAN items"), "{plan}");
        }
    }

    /// critic 8：OS のデコーダを呼ぶ前に、作っているアイテム（まとめての印）を付け、書いたら消す。
    #[test]
    fn items_are_marked_before_calling_the_decoders() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        for id in 1..=3 {
            add(&lib, id, "image", "jpg", None, None);
        }
        let backend = Recording::new(&lib);
        run_with(&lib, ThumbnailHandler::new(backend.clone()));
        let calls = backend.calls();
        assert_eq!(calls.len(), 3);
        for (id, mark) in calls {
            let mark = mark.unwrap_or_else(|| panic!("{id} の印がない"));
            assert!(!mark.solo);
            assert!(mark.ids.contains(&id), "{mark:?}");
        }
        assert_eq!(inflight_now(&lib), None);
    }

    /// critic 8：前回落ちたときにまとめて作っていたもの（疑わしいもの）は、先に 1 件ずつ、1 件だけの印で作る。作り終えた・
    /// もう作らないものは疑わしいものから外す。
    #[test]
    fn suspects_are_retried_one_by_one() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        for id in 1..=4 {
            add(&lib, id, "image", "jpg", None, None);
        }
        sql(&lib, "UPDATE items SET thumb_state = 'ready' WHERE id = 4");
        lib.db()
            .write(|tx| settings::set(tx, SUSPECTS_KEY, &vec![3i64, 4, 2]))
            .unwrap();
        let backend = Recording::new(&lib);
        run_with(&lib, ThumbnailHandler::new(backend.clone()));
        let calls = backend.calls();
        let order: Vec<i64> = calls.iter().map(|(id, _)| *id).collect();
        assert_eq!(order, vec![3, 2, 1]);
        for (id, mark) in &calls[..2] {
            assert_eq!(
                mark.as_ref(),
                Some(&Inflight {
                    solo: true,
                    ids: vec![*id]
                })
            );
        }
        assert!(!calls[2].1.as_ref().unwrap().solo);
        for id in 1..=4 {
            assert_eq!(row(&lib, id).0, "ready");
        }
        assert!(
            lib.db()
                .read(|c| settings::get::<Vec<i64>>(c, SUSPECTS_KEY))
                .unwrap()
                .is_none()
        );
        assert_eq!(inflight_now(&lib), None);
    }

    /// ライブラリを作り、`setup` でアイテムを入れて閉じる（root を返す）。
    fn library_with(dir: &Path, setup: impl FnOnce(&Arc<Library>)) -> PathBuf {
        let lib = Arc::new(Library::create(&dir.join("L")).unwrap());
        setup(&lib);
        let root = lib.layout().root().to_path_buf();
        drop(lib);
        root
    }

    fn open(root: &Path, services: Services) -> LibrarySession {
        let session = LibrarySession::open(root, Arc::new(|_| {}), services).unwrap();
        session.wait_ready();
        assert!(session.jobs().wait_idle(Duration::from_secs(20)));
        session
    }

    /// critic 8（開く道）：1 件だけの印が残っていれば、そのアイテムは作らずに failed にして先へ進む。まとめての印なら、1 件ずつ
    /// 作り直す（失敗にしない）。
    #[test]
    fn opening_after_a_crash_fails_only_the_item_that_crashed() {
        let dir = tempfile::tempdir().unwrap();
        let root = library_with(dir.path(), |lib| {
            for id in 1..=3 {
                add(lib, id, "image", "jpg", None, None);
            }
            lib.db().write(|tx| inflight::mark(tx, &[1], true)).unwrap();
        });
        let session = open(&root, Services::new(Arc::new(FakeBackend)));
        let lib = session.library().clone();
        assert_eq!(row(&lib, 1).0, "failed");
        assert!(!targets(&lib, 1).files(ThumbFormat::Jpeg).0.exists());
        assert_eq!(
            (row(&lib, 2).0, row(&lib, 3).0),
            ("ready".into(), "ready".into())
        );
        drop(session);

        let root = library_with(&dir.path().join("b"), |lib| {
            for id in 1..=2 {
                add(lib, id, "image", "jpg", None, None);
            }
            lib.db()
                .write(|tx| inflight::mark(tx, &[1, 2], false))
                .unwrap();
        });
        let session = open(&root, Services::new(Arc::new(FakeBackend)));
        let lib = session.library().clone();
        assert_eq!(
            (row(&lib, 1).0, row(&lib, 2).0),
            ("ready".into(), "ready".into())
        );
        assert!(lib.db().read(inflight::suspects).unwrap().is_empty());
    }

    /// plan defaults 20（開く道）：作り方の指紋のないライブラリ（M10 まで）を開くと、作り方が変わった形式（png）の ready を
    /// stale にして作り直す（thumb_rev+1）。jpeg はそのまま。指紋を記録するので、次に開いても作り直さない。
    #[test]
    fn opening_rebuilds_formats_whose_way_changed() {
        let dir = tempfile::tempdir().unwrap();
        let root = library_with(dir.path(), |lib| {
            add_bytes(
                lib,
                1,
                "image",
                "png",
                Some(300),
                None,
                &test_png(300, 200, true),
            );
            add(lib, 2, "image", "jpg", Some(10), None);
            fake::write(&targets(lib, 1), ThumbFormat::Jpeg, b"OLD", b"OLD").unwrap();
            sql(lib, "UPDATE items SET thumb_state = 'ready'");
        });
        let services = || {
            Services::new(Arc::new(FakeBackend)).with_engine(Arc::new(FakeEngineFactory::default()))
        };
        let session = open(&root, services());
        let lib = session.library().clone();
        assert_eq!((row(&lib, 1).0.as_str(), rev(&lib, 1)), ("ready", 1));
        assert!(targets(&lib, 1).files(ThumbFormat::Png).0.is_file());
        assert!(!targets(&lib, 1).files(ThumbFormat::Jpeg).0.exists());
        assert_eq!((row(&lib, 2).0.as_str(), rev(&lib, 2)), ("ready", 0));
        let stored: BTreeMap<String, String> = lib
            .db()
            .read(|c| settings::get::<super::super::route::Plan>(c, super::super::route::PLAN_KEY))
            .unwrap()
            .unwrap()
            .formats;
        assert_eq!(stored["png"], super::super::route::ENGINE_THUMBS);
        drop(session);
        let session = open(&root, services());
        assert_eq!(rev(session.library(), 1), 1, "もう一度作り直した");
    }

    #[test]
    fn font_info_round_trips_through_codec_info() {
        let info = MediaInfo {
            codec: Some("otf".into()),
            font: Some(FontInfo {
                family: Some("Geneva".into()),
                glyphs: 3,
                faces: 1,
                ..Default::default()
            }),
            ..Default::default()
        };
        let back: MediaInfo = serde_json::from_str(&info.codec_json().unwrap()).unwrap();
        assert_eq!(back, info);
    }
}
