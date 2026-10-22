//! プロキシのジョブ（docs/architecture.md §8）。
//!
//! 1. 判定：`proxy_state = 'unknown'` の動画・音楽を ffprobe で読み、再生可否の表と照らして
//!    `not_needed`（元のまま再生できる）・`pending`（プロキシが要る）・`failed`（読めない）にする。
//! 2. 作成：`pending` のものを 1 件ずつ `proxies/ab/<hash>.mp4` に作る。入れ替えで済むなら `-c copy`、
//!    そうでなければ映像は VideoToolbox、音声は AAC 256kbps。一時ファイルに書いてから置き換える。
//!
//! 再生キューに入ったもの（`ProxyShared::prioritize`）を先に処理する。進み具合はアイテムごとの
//! proxy_state そのもの（強制終了しても、続きは unknown / pending のものから）。

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};

use super::support::{
    Container, PlaybackSupport, ProxyPlan, StreamAction, Streams, forced_plan, parse_streams, plan,
};
use crate::import::MediaKind;
use crate::jobs::{Control, JobContext, JobEnd, JobHandler};
use crate::library::{Layout, hex};
use crate::time::now_ms;
use crate::tools::{find, run_with_timeout};
use crate::{CoreError, Result};

/// 映像を変換するときのエンコーダ（ffmpeg の名前。macOS の VideoToolbox）。
pub const VIDEO_ENCODER: &str = "h264_videotoolbox";
/// 1 回に判定する件数。
const DECIDE_BATCH: usize = 16;
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// ffmpeg の進み具合がこれだけ止まったら諦める。
const STALL_TIMEOUT: Duration = Duration::from_secs(120);
/// 再生可否の表の指紋を入れておく settings のキー。
const SUPPORT_KEY: &str = "playback.support";

/// ジョブとライブラリで共有するもの：再生可否の表と、先に処理するアイテム。
#[derive(Debug, Default)]
pub struct ProxyShared {
    support: RwLock<Option<PlaybackSupport>>,
    priority: Mutex<Vec<i64>>,
}

impl ProxyShared {
    /// 再生可否の表を入れる。
    pub fn set_support(&self, support: PlaybackSupport) {
        if let Ok(mut slot) = self.support.write() {
            *slot = Some(support);
        }
    }

    /// 再生可否の表（まだなければ None）。
    pub fn support(&self) -> Option<PlaybackSupport> {
        self.support.read().ok().and_then(|s| s.clone())
    }

    /// 先に処理するアイテムを入れ替える（再生キューの今と次のもの）。
    pub fn prioritize(&self, ids: Vec<i64>) {
        if let Ok(mut slot) = self.priority.lock() {
            *slot = ids;
        }
    }

    fn priority(&self) -> Vec<i64> {
        self.priority.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

/// ffmpeg と ffprobe の場所と、映像のエンコーダ。
#[derive(Debug, Clone)]
pub struct ProxyTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub video_encoder: String,
}

impl ProxyTools {
    /// ffmpeg と ffprobe が見つかれば作る。
    pub fn detect() -> Option<Self> {
        Some(Self {
            ffmpeg: find("ffmpeg")?,
            ffprobe: find("ffprobe")?,
            video_encoder: VIDEO_ENCODER.to_string(),
        })
    }

    /// 映像・音声のコーデックを読む。
    fn probe(&self, src: &Path) -> std::result::Result<Streams, String> {
        let mut cmd = Command::new(&self.ffprobe);
        cmd.args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(src);
        let output = run_with_timeout(&mut cmd, PROBE_TIMEOUT).map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(last_line(&output.stderr, "ffprobe が失敗しました"));
        }
        parse_streams(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| "ffprobe の結果を読めません".to_string())
    }
}

fn last_line(bytes: &[u8], fallback: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// 映像の目安のビットレート（変換するとき）。
fn video_bitrate(streams: &Streams) -> &'static str {
    match streams.width.zip(streams.height).map(|(w, h)| w * h) {
        Some(px) if px <= 1280 * 720 => "5M",
        Some(px) if px <= 1920 * 1080 => "10M",
        Some(_) => "30M",
        None => "10M",
    }
}

/// ffmpeg の引数（純粋関数）。`out` には一時ファイルを渡すので入れ物は `-f mp4` で決める。
pub fn proxy_args(
    src: &Path,
    out: &Path,
    plan: ProxyPlan,
    streams: &Streams,
    encoder: &str,
) -> Vec<OsString> {
    let ProxyPlan::Make { video, audio } = plan else {
        return Vec::new();
    };
    let mut args: Vec<OsString> = ["-hide_banner", "-v", "error", "-nostdin", "-y", "-i"]
        .iter()
        .map(OsString::from)
        .collect();
    args.push(src.into());
    let mut push = |items: &[&str]| args.extend(items.iter().map(OsString::from));
    if video != StreamAction::None
        && let Some(index) = streams.video_index
    {
        push(&["-map", &format!("0:{index}")]);
    }
    if audio != StreamAction::None
        && let Some(index) = streams.audio_index
    {
        push(&["-map", &format!("0:{index}")]);
    }
    match video {
        StreamAction::None => push(&["-vn"]),
        StreamAction::Copy => {
            push(&["-c:v", "copy"]);
            // Safari は hev1 の HEVC を再生しない
            if streams.video.as_deref() == Some("hevc") {
                push(&["-tag:v", "hvc1"]);
            }
        }
        StreamAction::Encode => {
            push(&[
                "-c:v",
                encoder,
                "-b:v",
                video_bitrate(streams),
                "-pix_fmt",
                "yuv420p",
            ]);
            if encoder.ends_with("_videotoolbox") {
                // ハードウェアが使えない環境ではソフトウェアで
                push(&["-allow_sw", "1"]);
            }
        }
    }
    match audio {
        StreamAction::None => push(&["-an"]),
        StreamAction::Copy => push(&["-c:a", "copy"]),
        StreamAction::Encode => push(&["-c:a", "aac", "-b:a", "256k"]),
    }
    push(&[
        "-sn",
        "-dn",
        "-strict",
        "experimental",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
        "-progress",
        "pipe:1",
        "-nostats",
    ]);
    args.push(out.into());
    args
}

/// 判定・作成を待っているアイテム。
#[derive(Debug, Clone)]
struct Todo {
    id: i64,
    hash: [u8; 32],
    ext: String,
    kind: MediaKind,
}

/// 次にすること。
#[derive(Debug)]
enum Work {
    Decide(Vec<Todo>),
    Make(Todo),
    Nothing,
}

fn todo_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Vec<u8>, String, MediaKind)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn to_todo((id, hash, ext, kind): (i64, Vec<u8>, String, MediaKind)) -> Option<Todo> {
    let hash = <[u8; 32]>::try_from(hash.as_slice()).ok()?;
    Some(Todo {
        id,
        hash,
        ext,
        kind,
    })
}

/// 次にすることを選ぶ：優先のものの判定 → 優先のものの作成 → 新しい順に判定 → 新しい順に作成。
fn next_work(conn: &Connection, priority: &[i64]) -> Result<Work> {
    let mut by_id = conn.prepare_cached(
        "SELECT id, hash, ext, kind, proxy_state FROM items
         WHERE id = ?1 AND +kind IN ('video', 'audio') AND trashed_at IS NULL",
    )?;
    let mut decide = Vec::new();
    let mut make = None;
    for &id in priority {
        let row = by_id
            .query_row([id], |r| Ok((todo_from_row(r)?, r.get::<_, String>(4)?)))
            .optional()?;
        match row {
            Some((todo, state)) if state == "unknown" => decide.extend(to_todo(todo)),
            Some((todo, state)) if state == "pending" && make.is_none() => {
                make = to_todo(todo);
            }
            _ => {}
        }
    }
    if !decide.is_empty() {
        return Ok(Work::Decide(decide));
    }
    if let Some(todo) = make {
        return Ok(Work::Make(todo));
    }

    let mut unknown = conn.prepare_cached(
        "SELECT id, hash, ext, kind FROM items
         WHERE proxy_state = 'unknown' AND +kind IN ('video', 'audio') AND trashed_at IS NULL
         ORDER BY imported_at DESC, id DESC LIMIT ?1",
    )?;
    let batch: Vec<Todo> = unknown
        .query_map([DECIDE_BATCH as i64], todo_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(to_todo)
        .collect();
    if !batch.is_empty() {
        return Ok(Work::Decide(batch));
    }
    let mut pending = conn.prepare_cached(
        "SELECT id, hash, ext, kind FROM items
         WHERE proxy_state = 'pending' AND +kind IN ('video', 'audio') AND trashed_at IS NULL
         ORDER BY imported_at DESC, id DESC LIMIT 1",
    )?;
    let next = pending.query_row([], todo_from_row).optional()?;
    Ok(next.and_then(to_todo).map_or(Work::Nothing, Work::Make))
}

/// 判定・作成を待っている数。
fn count_todo(conn: &Connection) -> Result<u64> {
    let mut stmt = conn.prepare_cached(
        "SELECT (SELECT COUNT(*) FROM items WHERE proxy_state = 'unknown' AND +kind IN ('video', 'audio') AND trashed_at IS NULL)
              + (SELECT COUNT(*) FROM items WHERE proxy_state = 'pending' AND +kind IN ('video', 'audio') AND trashed_at IS NULL)",
    )?;
    let count: i64 = stmt.query_row([], |r| r.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// 判定・作成を待っているものがあるか。
pub fn has_todo(conn: &Connection) -> Result<bool> {
    Ok(count_todo(conn)? > 0)
}

/// 再生可否の表が前回と変わっていたら、作り済み以外の判定をやり直す（settings に指紋を記録）。
/// 判定・作成を待っているものがあれば true。
pub fn sync_support(tx: &Connection, fingerprint: &str) -> Result<bool> {
    let stored: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SUPPORT_KEY],
            |r| r.get(0),
        )
        .optional()?;
    let current = serde_json::to_string(fingerprint)?;
    if stored.as_deref() != Some(current.as_str()) {
        let reset = tx.execute(
            "UPDATE items SET proxy_state = 'unknown'
             WHERE +kind IN ('video', 'audio') AND proxy_state IN ('not_needed', 'pending', 'failed')",
            [],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SUPPORT_KEY, current],
        )?;
        tracing::info!(
            reset,
            "再生可否の表が変わったので、プロキシの判定をやり直します"
        );
    }
    has_todo(tx)
}

/// 再生できなかったものをプロキシの作成待ちに戻す（元のまま再生できるはずだった・失敗していたもの）。
/// 判定・作成を待っているものが `ids` にあれば true。
pub fn force_pending(tx: &Connection, ids: &[i64]) -> Result<bool> {
    let mut update = tx.prepare_cached(
        "UPDATE items SET proxy_state = 'pending'
         WHERE id = ?1 AND +kind IN ('video', 'audio') AND proxy_state IN ('not_needed', 'unknown', 'failed')",
    )?;
    for &id in ids {
        update.execute([id])?;
    }
    any_todo(tx, ids)
}

/// `ids` に判定・作成を待っているものがあるか。
pub fn any_todo(conn: &Connection, ids: &[i64]) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM items WHERE id = ?1 AND +kind IN ('video', 'audio')
                        AND proxy_state IN ('unknown', 'pending') AND trashed_at IS NULL)",
    )?;
    for &id in ids {
        if stmt.query_row([id], |r| r.get::<_, bool>(0))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 判定の結果。
#[derive(Debug)]
struct Decision {
    id: i64,
    state: &'static str,
    codec_json: Option<String>,
    error: Option<String>,
}

fn decide(tools: &ProxyTools, layout: &Layout, support: &PlaybackSupport, todo: &Todo) -> Decision {
    let src = layout.content_path(&todo.hash, &todo.ext);
    match tools.probe(&src) {
        Ok(streams) => {
            let state = match plan(Container::from_ext(&todo.ext), &streams, support) {
                ProxyPlan::NotNeeded => "not_needed",
                ProxyPlan::Make { .. } => "pending",
                ProxyPlan::Unplayable => "failed",
            };
            let main = if todo.kind == MediaKind::Video {
                streams.video.clone()
            } else {
                streams.audio.clone()
            };
            Decision {
                id: todo.id,
                state,
                codec_json: Some(
                    serde_json::json!({ "codec": main, "video": streams.video, "audio": streams.audio })
                        .to_string(),
                ),
                error: (state == "failed").then(|| "映像も音声もありません".to_string()),
            }
        }
        Err(err) => Decision {
            id: todo.id,
            state: "failed",
            codec_json: None,
            error: Some(err),
        },
    }
}

fn apply_decisions(tx: &Connection, decisions: &[Decision]) -> Result<()> {
    let now = now_ms();
    let mut stmt = tx.prepare_cached(
        "UPDATE items SET proxy_state = ?2, codec_info = coalesce(?3, codec_info), updated_at = ?4
         WHERE id = ?1 AND proxy_state = 'unknown'",
    )?;
    for d in decisions {
        stmt.execute(params![d.id, d.state, d.codec_json, now])?;
    }
    Ok(())
}

fn set_state(tx: &Connection, id: i64, state: &str) -> Result<()> {
    tx.execute(
        "UPDATE items SET proxy_state = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, state, now_ms()],
    )?;
    Ok(())
}

/// 作成の結果。
#[derive(Debug)]
enum Made {
    Ready,
    Failed(String),
    /// 一時停止・中止で止めた（proxy_state は pending のまま）。
    Interrupted(Control),
}

/// ffmpeg を動かし、進み具合を `on_progress`（0〜1）に送る。一時停止・中止・止まったままなら止める。
fn run_ffmpeg(
    tools: &ProxyTools,
    args: &[OsString],
    duration_ms: Option<i64>,
    control: &dyn Fn() -> Control,
    on_progress: &dyn Fn(f64),
) -> std::result::Result<Option<Control>, String> {
    let mut child: Child = crate::tools::spawn(
        Command::new(&tools.ffmpeg)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .map_err(|e| format!("ffmpeg を起動できません: {e}"))?;
    let (sender, receiver) = mpsc::channel::<f64>();
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let Some(stdout) = stdout else { return };
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            // -progress の out_time_us（マイクロ秒）
            if let Some(us) = line
                .strip_prefix("out_time_us=")
                .and_then(|v| v.trim().parse::<f64>().ok())
            {
                let fraction = duration_ms
                    .filter(|d| *d > 0)
                    .map(|d| (us / 1000.0 / d as f64).clamp(0.0, 1.0))
                    .unwrap_or(0.0);
                if sender.send(fraction).is_err() {
                    return;
                }
            } else if line.starts_with("progress=") && sender.send(-1.0).is_err() {
                return;
            }
        }
    });
    let stderr = child.stderr.take();
    let errors = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = std::io::Read::read_to_end(&mut stderr, &mut buf);
        }
        buf
    });

    let mut last_seen = Instant::now();
    let stop = |child: &mut Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let status = loop {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(fraction) => {
                last_seen = Instant::now();
                if fraction >= 0.0 {
                    on_progress(fraction);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
        match control() {
            Control::Continue => {}
            other => {
                stop(&mut child);
                let _ = reader.join();
                let _ = errors.join();
                return Ok(Some(other));
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                stop(&mut child);
                return Err(err.to_string());
            }
        }
        if last_seen.elapsed() > STALL_TIMEOUT {
            stop(&mut child);
            return Err(format!(
                "ffmpeg が {} 秒進まなかったので止めました",
                STALL_TIMEOUT.as_secs()
            ));
        }
    };
    let _ = reader.join();
    let stderr = errors.join().unwrap_or_default();
    if status.success() {
        Ok(None)
    } else {
        Err(last_line(&stderr, "ffmpeg が失敗しました"))
    }
}

fn make(
    ctx: &JobContext,
    tools: &ProxyTools,
    support: &PlaybackSupport,
    todo: &Todo,
    on_progress: &dyn Fn(f64),
) -> Made {
    let layout = ctx.library().layout();
    let src = layout.content_path(&todo.hash, &todo.ext);
    let out = layout.proxy_path(&hex(&todo.hash));
    if out.is_file() {
        return Made::Ready;
    }
    let streams = match tools.probe(&src) {
        Ok(streams) => streams,
        Err(err) => return Made::Failed(err),
    };
    // 判定では「元のまま再生できる」はずだったのに作成待ちにされたものは、すべて変換する
    let plan = match plan(Container::from_ext(&todo.ext), &streams, support) {
        ProxyPlan::NotNeeded => forced_plan(&streams),
        other => other,
    };
    if plan == ProxyPlan::Unplayable {
        return Made::Failed("映像も音声もありません".into());
    }
    if let Some(parent) = out.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return Made::Failed(err.to_string());
    }
    let part = out.with_extension("mp4.part");
    let args = proxy_args(&src, &part, plan, &streams, &tools.video_encoder);
    let result = run_ffmpeg(
        tools,
        &args,
        streams.duration_ms,
        &|| ctx.control(),
        on_progress,
    );
    let finished = match result {
        Ok(None) => std::fs::rename(&part, &out)
            .map(|()| Made::Ready)
            .unwrap_or_else(|e| Made::Failed(e.to_string())),
        Ok(Some(control)) => Made::Interrupted(control),
        Err(err) => Made::Failed(err),
    };
    if !matches!(finished, Made::Ready) {
        let _ = std::fs::remove_file(&part);
    }
    finished
}

/// プロキシのジョブの処理。
pub struct ProxyHandler {
    tools: Option<ProxyTools>,
    shared: Arc<ProxyShared>,
    threads: usize,
}

impl ProxyHandler {
    /// `tools` が None（ffmpeg がない）なら何もしない。
    pub fn new(tools: Option<ProxyTools>, shared: Arc<ProxyShared>) -> Self {
        Self {
            tools,
            shared,
            threads: 4,
        }
    }
}

fn summary(decided: u64, made: u64, failed: u64) -> String {
    serde_json::json!({ "decided": decided, "made": made, "failed": failed }).to_string()
}

impl JobHandler for ProxyHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let done = |message: &str| {
            ctx.log(message);
            Ok(JobEnd::Done {
                result_json: summary(0, 0, 0),
                message: Some(message.to_string()),
            })
        };
        let Some(tools) = &self.tools else {
            return done("ffmpeg が見つからないので、プロキシは作りません");
        };
        let Some(support) = self.shared.support() else {
            return done("再生可否の表がまだないので、あとで判定します");
        };
        let library = ctx.library();
        let layout = library.layout().clone();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.threads)
            .thread_name(|i| format!("kura-proxy-{i}"))
            .build()
            .map_err(|err| CoreError::Invalid(format!("プロキシのスレッドを作れません: {err}")))?;

        let (mut decided, mut made, mut failed) = (0u64, 0u64, 0u64);
        loop {
            match ctx.control() {
                Control::Continue => {}
                Control::Pause => return Ok(JobEnd::Paused),
                Control::Cancel => {
                    return Ok(JobEnd::Canceled {
                        result_json: Some(summary(decided, made, failed)),
                    });
                }
            }
            let priority = self.shared.priority();
            let work = library.db().read(|conn| next_work(conn, &priority))?;
            let remaining = library.db().read(count_todo)?;
            let finished = decided + made + failed;
            let overall = finished as f64 / (finished + remaining).max(1) as f64;
            match work {
                Work::Nothing => break,
                Work::Decide(batch) => {
                    let decisions: Vec<Decision> = pool.install(|| {
                        batch
                            .par_iter()
                            .map(|todo| decide(tools, &layout, &support, todo))
                            .collect()
                    });
                    for d in &decisions {
                        if let Some(error) = &d.error {
                            ctx.log(&format!("アイテム {}: {error}", d.id));
                        }
                    }
                    decided += decisions.iter().filter(|d| d.state != "failed").count() as u64;
                    failed += decisions.iter().filter(|d| d.state == "failed").count() as u64;
                    library
                        .db()
                        .write(move |tx| apply_decisions(tx, &decisions))?;
                    ctx.progress(
                        overall,
                        &format!("再生できるか確かめています（残り {remaining} 件）"),
                    );
                }
                Work::Make(todo) => {
                    ctx.progress(overall, &format!("プロキシを作成中（残り {remaining} 件）"));
                    let on_progress = |fraction: f64| {
                        ctx.progress(
                            overall,
                            &format!(
                                "プロキシを作成中 {}%（残り {remaining} 件）",
                                (fraction * 100.0).round()
                            ),
                        );
                    };
                    match make(ctx, tools, &support, &todo, &on_progress) {
                        Made::Ready => {
                            made += 1;
                            let id = todo.id;
                            library.db().write(move |tx| set_state(tx, id, "ready"))?;
                        }
                        Made::Failed(err) => {
                            failed += 1;
                            ctx.log(&format!("アイテム {}: {err}", todo.id));
                            let id = todo.id;
                            library.db().write(move |tx| set_state(tx, id, "failed"))?;
                        }
                        Made::Interrupted(Control::Cancel) => {
                            return Ok(JobEnd::Canceled {
                                result_json: Some(summary(decided, made, failed)),
                            });
                        }
                        Made::Interrupted(_) => return Ok(JobEnd::Paused),
                    }
                }
            }
        }
        let message = format!("{decided} 件を判定し、{made} 件のプロキシを作成（失敗 {failed}）");
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: summary(decided, made, failed),
            message: Some(message),
        })
    }
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
