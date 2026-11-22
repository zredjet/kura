//! ffmpeg の段（`Tier::External`。同梱しない・任意。docs/editing.md §9.1・§9.2・§9.3・§13）。
//!
//! - 読み取り（[`FfmpegAudioDecoder`]。形式ごとに 1 つ、識別子 `<形式>-ffmpeg-dec/1`）：前の段（固定の段・Platform）が
//!   断った音声だけが来る（多チャンネルの Opus、Ogg の Opus・FLAC・Speex、圧縮の WAV・AIFF、MP3 の layer 1・2、HE-AAC）。
//!   ffprobe で中身を確かめ、1 回デコードして数えて形を `Exact` にし、もう 1 回デコードして値にする。
//! - 書き出し（[`Mp3FfmpegEncoder`]。`mp3-ffmpeg-enc/1`）：`libmp3lame` の MP3（CBR 192 kbps が既定、VBR V0〜V9。
//!   ID3 は書かない。gapless は LAME のタグで保つ）。
//! - 識別子は kura の呼び方（引数・出力の形・標本の直し方・タグの読み方・失敗の分類）の約束の版だけを持つ。引数を変えたら
//!   版を上げる（引数のゴールデンが見張る）。ffmpeg・ライブラリ・LAME の版と指紋は記録の `env` に出す。再現は約束しない
//!   （読んだ元にはアンカーを作り、作り直しはアンカーから読むので、旧版は残さない）。
//! - 検出（[`FfmpegCodecs::detect`]）は起動時に 1 回（約 0.2 秒）。ffmpeg・ffprobe が見つからない・問い合わせに答えない
//!   ときは何も足さない。実行ファイルが変わったら（大きさ・更新日時）、次の読み書きの前に検出し直す。
//! - 子プロセスは `tools::run_binary`（環境変数を消す・新しいプロセスグループ・見張りで中止・無出力・時間切れ・出力の
//!   上限でグループごと止める・子が終わってもパイプを開いたままの子孫が残れば、止めたあとならすぐ、止めていなければ
//!   子が終わってから 4 秒（2 秒で SIGKILL、さらに 2 秒）で見捨てて戻る・`Drop` で回収）。同時に動かす数は抑える（既定は
//!   コア数の半分）。一時ファイルは作業フォルダ（ライブラリの `staging/` の下を渡す。落ちたときの残りは起動時の
//!   `clean_staging` が消す）に、プロセスで 1 つの通し番号の名前で、ないことを確かめて作る（同じ作業フォルダの組が
//!   2 つあっても、ほかのプロセスの残りがあっても重ならない）。

pub mod args;
pub mod decode;
pub mod encode;
pub mod env;
pub mod probe;
pub mod tags;
pub mod wavpipe;

use std::fs::File;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use kura_engine::codec::{CodecEnv, CodecSet, Decoder, Encoder, Stage};
use kura_engine::error::EngineError;
use kura_engine::monitor::Monitor;

pub use decode::{DecodeKind, FfmpegAudioDecoder, KINDS};
pub use encode::Mp3FfmpegEncoder;

use crate::tools::{Budget, RunError, RunOutcome, Stopped};
use env::{Detected, ExeStamp};

/// 道具の名前（`EngineError::CodecMissing` の `tool`）。
pub const TOOL: &str = "ffmpeg";
/// 無出力で止めるまでの時間（読み取り・書き出し）。
pub const STALL: Duration = Duration::from_secs(60);
/// 全体の時間の上限の基本（これに音声の長さの 2 倍を足す。実時間の半分の速さを下限とみなす）。
pub const DEADLINE_BASE: Duration = Duration::from_secs(60);

/// 組み立てたコーデックが共有するもの（実行ファイルの場所・作業フォルダ・検出の結果・同時に動かす数）。
pub(crate) struct Shared {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    work_dir: PathBuf,
    state: RwLock<Arc<Detected>>,
    running: Mutex<usize>,
    freed: Condvar,
    max_running: usize,
}

/// ffmpeg がない（消えた・起動できない）。
pub(crate) fn missing() -> EngineError {
    EngineError::CodecMissing {
        tool: TOOL.to_string(),
    }
}

impl Shared {
    pub(crate) fn ffmpeg(&self) -> &Path {
        &self.ffmpeg
    }

    pub(crate) fn ffprobe(&self) -> &Path {
        &self.ffprobe
    }

    /// 今の検出の結果（プロセスを動かさない）。
    pub(crate) fn detected(&self) -> Arc<Detected> {
        match self.state.read() {
            Ok(s) => s.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// 実行ファイルが変わっていれば検出し直してから、今の結果を返す（ふだんは stat だけ）。消えていれば `CodecMissing`。
    pub(crate) fn refresh(&self) -> Result<Arc<Detected>, EngineError> {
        let cur = self.detected();
        let now = vec![ExeStamp::of(&self.ffmpeg), ExeStamp::of(&self.ffprobe)];
        if now.iter().any(Option::is_none) {
            return Err(missing());
        }
        if now == cur.stamps {
            return Ok(cur);
        }
        let dir = self.work_dir()?;
        let fresh = env::detect(&self.ffmpeg, &self.ffprobe, dir).map_err(|m| {
            tracing::warn!("ffmpeg を検出し直せません: {m}");
            missing()
        })?;
        let fresh = Arc::new(fresh);
        match self.state.write() {
            Ok(mut s) => *s = fresh.clone(),
            Err(e) => *e.into_inner() = fresh.clone(),
        }
        tracing::info!(
            "ffmpeg が変わったので検出し直しました（{}）",
            fresh.info.version
        );
        Ok(fresh)
    }

    /// 作業フォルダ（なければ作る）。
    pub(crate) fn work_dir(&self) -> Result<&Path, EngineError> {
        std::fs::create_dir_all(&self.work_dir)?;
        Ok(&self.work_dir)
    }

    /// 作業フォルダ（なければ作る）に、空の一時ファイルを作る（[`create_temp_in`]）。
    pub(crate) fn create_temp(
        &self,
        prefix: &str,
        ext: &str,
    ) -> Result<(TempFile, File), EngineError> {
        create_temp_in(self.work_dir()?, prefix, ext, &TEMP_SEQ)
    }

    /// 同時に動かす数の枠を 1 つ取る（空くまで待つ。待つあいだも中止を見る）。
    pub(crate) fn acquire(&self, mon: &dyn Monitor) -> Result<Permit<'_>, EngineError> {
        let mut n = match self.running.lock() {
            Ok(n) => n,
            Err(e) => e.into_inner(),
        };
        loop {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            if *n < self.max_running {
                *n += 1;
                return Ok(Permit(self));
            }
            n = match self.freed.wait_timeout(n, Duration::from_millis(50)) {
                Ok((n, _)) => n,
                Err(e) => e.into_inner().0,
            };
        }
    }
}

/// 同時に動かす数の枠（`Drop` で返す）。
pub(crate) struct Permit<'a>(&'a Shared);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut n = match self.0.running.lock() {
            Ok(n) => n,
            Err(e) => e.into_inner(),
        };
        *n = n.saturating_sub(1);
        self.0.freed.notify_one();
    }
}

/// 一時ファイルの通し番号（プロセスで 1 つ。組ごとに持つと、同じ作業フォルダの組が 2 つあるときに名前が重なる）。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 一時ファイルを作る名前を探す回数の上限（ほかのプロセスの残りが続いても止まらないように）。
const TEMP_TRIES: u32 = 1_000;

/// `dir` に `{prefix}-{pid}-{n}.{ext}` の空のファイルを、ないことを確かめて作る（`create_new`。すでにあれば次の番号）。
/// 作ったファイルだけを [`TempFile`] が持つので、ほかの組・ほかのプロセスのファイルを切り詰めたり消したりしない。
pub(crate) fn create_temp_in(
    dir: &Path,
    prefix: &str,
    ext: &str,
    seq: &AtomicU64,
) -> Result<(TempFile, File), EngineError> {
    for _ in 0..TEMP_TRIES {
        let n = seq.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{prefix}-{}-{n}.{ext}", std::process::id()));
        match File::options().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((TempFile::new(path), f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(EngineError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "{}：一時ファイルの名前が {TEMP_TRIES} 回とも使われています",
            dir.display()
        ),
    )))
}

/// 一時ファイル（`Drop` で消す。エラーでも panic でも残さない）。
pub(crate) struct TempFile(PathBuf);

impl TempFile {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 読み取りか書き出しかのエラー（`Decode`・`Encode`）。
pub(crate) fn io_error(id: &str, stage: Stage, message: impl Into<String>) -> EngineError {
    let (codec, message) = (id.to_string(), message.into());
    match stage {
        Stage::Decode => EngineError::Decode { codec, message },
        Stage::Encode => EngineError::Encode { codec, message },
    }
}

/// ffmpeg の行の頭の `[in#0/ogg @ 0x…] [error] ` を除き、長さを抑える。
pub(crate) fn clean_line(line: &str) -> String {
    let mut s = line.trim();
    while let Some(rest) = s.strip_prefix('[') {
        match rest.find(']') {
            Some(end) => s = rest[end + 1..].trim_start(),
            None => break,
        }
    }
    let mut end = s.len().min(300);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// 終わり方の説明。
pub(crate) fn describe(status: &ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(c), _) => format!("終了コード {c}"),
        (None, Some(s)) => format!("シグナル {s} で異常終了"),
        _ => "不明な終わり方".to_string(),
    }
}

/// 自分で終わったプロセスが失敗か（終了コードが 0 でない・`[error]` 以上の行がある）。失敗ならエラー。
pub(crate) fn failure(id: &str, stage: Stage, o: &RunOutcome) -> Option<EngineError> {
    if o.status.success() && o.error_lines.is_empty() {
        return None;
    }
    let detail = o
        .error_lines
        .first()
        .or(o.stderr_head.last())
        .map(|l| clean_line(l))
        .unwrap_or_default();
    Some(io_error(
        id,
        stage,
        format!("ffmpeg が失敗しました（{}）: {detail}", describe(&o.status)),
    ))
}

/// 起動できない・止めたときのエラー（受け手が止めた `Consumer` は、呼び手が自分のエラーを返す）。
pub(crate) fn run_error(id: &str, stage: Stage, e: RunError, budget: &Budget) -> EngineError {
    match e {
        RunError::Spawn(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            missing()
        }
        RunError::Spawn(e) => io_error(id, stage, format!("ffmpeg を起動できません: {e}")),
        RunError::Stopped(Stopped::Canceled) => EngineError::Canceled,
        RunError::Stopped(Stopped::Stalled) => io_error(
            id,
            stage,
            format!(
                "ffmpeg が {} 秒応答しなかったので止めました",
                budget.stall.as_secs()
            ),
        ),
        RunError::Stopped(Stopped::Deadline) => io_error(
            id,
            stage,
            format!(
                "ffmpeg が {} 秒で終わらなかったので止めました",
                budget.deadline.as_secs()
            ),
        ),
        RunError::Stopped(Stopped::OutputLimit) => {
            EngineError::Limit("ffmpeg の出力が上限を超えました".into())
        }
        RunError::Stopped(Stopped::Consumer) => {
            EngineError::Internal(format!("{id}: 出力の受け手が理由を残さずに止めました"))
        }
        RunError::Stopped(Stopped::Lingering) => io_error(
            id,
            stage,
            "ffmpeg が終わったあとも、入出力を開いたままのプロセスが残ったので待つのをやめました",
        ),
        RunError::Wait(e) => EngineError::Io(e),
    }
}

/// 長さ `secs` 秒の音声を扱うときの時間の上限（基本 60 秒＋長さの 2 倍）。
pub(crate) fn deadline_for(secs: f64) -> Duration {
    let extra = if secs.is_finite() && secs > 0.0 {
        Duration::try_from_secs_f64(secs * 2.0).unwrap_or(Duration::from_secs(u64::from(u32::MAX)))
    } else {
        Duration::ZERO
    };
    DEADLINE_BASE.saturating_add(extra)
}

/// 検出した ffmpeg の段（読み取りと書き出しのコーデックの組）。
#[derive(Clone)]
pub struct FfmpegCodecs {
    shared: Arc<Shared>,
}

impl FfmpegCodecs {
    /// ffmpeg・ffprobe を外部ツールの規則（設定 → Homebrew → PATH。`tools::find`）で探して検出する。見つからない・
    /// 問い合わせに答えないときは `None`（何も足さない）。`work_dir` は一時ファイルの置き場所（ライブラリの `staging/` の下）。
    pub fn detect(work_dir: &Path) -> Option<Self> {
        Self::detect_found(
            crate::tools::find("ffmpeg"),
            crate::tools::find("ffprobe"),
            work_dir,
        )
    }

    /// 探した結果から検出する（どちらかがない・問い合わせに答えないときは `None`）。
    fn detect_found(
        ffmpeg: Option<PathBuf>,
        ffprobe: Option<PathBuf>,
        work_dir: &Path,
    ) -> Option<Self> {
        let (ffmpeg, ffprobe) = (ffmpeg?, ffprobe?);
        match Self::detect_at(&ffmpeg, &ffprobe, work_dir) {
            Ok(c) => Some(c),
            Err(m) => {
                tracing::warn!("ffmpeg の段を使いません: {m}");
                None
            }
        }
    }

    /// 場所を指定して検出する。
    pub fn detect_at(ffmpeg: &Path, ffprobe: &Path, work_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(work_dir)
            .map_err(|e| format!("作業フォルダを作れません（{}）: {e}", work_dir.display()))?;
        let detected = env::detect(ffmpeg, ffprobe, work_dir)?;
        let max_running = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).max(1))
            .unwrap_or(1);
        Ok(Self {
            shared: Arc::new(Shared {
                ffmpeg: ffmpeg.to_path_buf(),
                ffprobe: ffprobe.to_path_buf(),
                work_dir: work_dir.to_path_buf(),
                state: RwLock::new(Arc::new(detected)),
                running: Mutex::new(0),
                freed: Condvar::new(),
                max_running,
            }),
        })
    }

    /// 記録の環境（コーデックごとの細目を除いたもの）。
    pub fn env(&self) -> CodecEnv {
        self.shared.detected().env()
    }

    /// 検出の結果（能力の表・調べ物のため）。
    pub fn detected(&self) -> Arc<Detected> {
        self.shared.detected()
    }

    /// 検出したあとに実行ファイルが変わった（消えた・上がった）か（stat だけ）。
    pub fn changed(&self) -> bool {
        let d = self.shared.detected();
        let now = vec![
            ExeStamp::of(&self.shared.ffmpeg),
            ExeStamp::of(&self.shared.ffprobe),
        ];
        now != d.stamps
    }

    /// 足す読み取りのコーデック（分離器と、どれかの中身のデコーダがある形式だけ）。
    pub fn decoders(&self) -> Vec<Arc<dyn Decoder>> {
        let d = self.shared.detected();
        KINDS
            .iter()
            .filter(|k| k.available(&d))
            .map(|k| Arc::new(FfmpegAudioDecoder::new(self.shared.clone(), k)) as Arc<dyn Decoder>)
            .collect()
    }

    /// 足す書き出しのコーデック（`libmp3lame` と mp3 の多重化器があるときの MP3）。
    pub fn encoders(&self) -> Vec<Arc<dyn Encoder>> {
        let d = self.shared.detected();
        let mut out: Vec<Arc<dyn Encoder>> = Vec::new();
        if Mp3FfmpegEncoder::available(&d) {
            out.push(Arc::new(Mp3FfmpegEncoder::new(self.shared.clone())));
        }
        out
    }

    /// 組みに足す（段の順は `CodecSet` が並べる）。
    pub fn add_to(&self, set: &mut CodecSet) -> Result<(), EngineError> {
        for d in self.decoders() {
            set.add_decoder(d)?;
        }
        for e in self.encoders() {
            set.add_encoder(e)?;
        }
        Ok(())
    }

    /// 同時に動かす数を変える（1 以上）。
    pub fn with_max_running(self, n: usize) -> Self {
        // 組み立ての直後だけ呼ぶ（共有する前）。共有していれば変えない
        match Arc::try_unwrap(self.shared) {
            Ok(mut s) => {
                s.max_running = n.max(1);
                Self {
                    shared: Arc::new(s),
                }
            }
            Err(shared) => Self { shared },
        }
    }
}

/// ffmpeg の段を組みに足す（見つからなければ何も足さず `None`）。
pub fn add_ffmpeg_codecs(
    set: &mut CodecSet,
    work_dir: &Path,
) -> Result<Option<FfmpegCodecs>, EngineError> {
    add_detected(set, FfmpegCodecs::detect(work_dir))
}

fn add_detected(
    set: &mut CodecSet,
    found: Option<FfmpegCodecs>,
) -> Result<Option<FfmpegCodecs>, EngineError> {
    let Some(codecs) = found else {
        return Ok(None);
    };
    codecs.add_to(set)?;
    Ok(Some(codecs))
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {

    use kura_engine::monitor::{CancelFlag, NoMonitor};

    use super::testing::{FakeTools, script};
    use super::*;

    #[test]
    fn lines_and_statuses_are_described() {
        assert_eq!(
            clean_line("[in#0/ogg @ 0x8ad024000] [error] CRC mismatch!"),
            "CRC mismatch!"
        );
        assert_eq!(
            clean_line("[fatal] Error opening input"),
            "Error opening input"
        );
        assert_eq!(clean_line("plain"), "plain");
        assert_eq!(clean_line("[unclosed"), "[unclosed");
        let long = "あ".repeat(200);
        assert!(clean_line(&long).len() <= 300);
        assert_eq!(deadline_for(0.0), DEADLINE_BASE);
        assert_eq!(deadline_for(f64::NAN), DEADLINE_BASE);
        assert_eq!(deadline_for(10.0), DEADLINE_BASE + Duration::from_secs(20));
        assert!(deadline_for(1e300) > DEADLINE_BASE);
        let ok = std::process::Command::new("sh")
            .args(["-c", "exit 3"])
            .status()
            .unwrap();
        assert_eq!(describe(&ok), "終了コード 3");
        let sig = std::process::Command::new("sh")
            .args(["-c", "kill -9 $$"])
            .status()
            .unwrap();
        assert_eq!(describe(&sig), "シグナル 9 で異常終了");
    }

    /// 外部ツールの規則（設定の場所）で見つけて足す。問い合わせに答えない ffmpeg は使わない（何も足さない）。
    #[test]
    fn detection_adds_only_working_tools() {
        // 外部ツールの場所の指定（tools::set_overrides）はアプリ全体で 1 つで、実物の ffmpeg を使うほかのテストが錠なしで
        // 読むので、ここでは変えない（探した結果を直接渡す）
        let t = FakeTools::new("exit 1", "exit 1", "exit 1");
        let found = |ffmpeg: Option<PathBuf>, ffprobe: Option<PathBuf>| {
            FfmpegCodecs::detect_found(ffmpeg, ffprobe, &t.work)
        };
        let mut set = CodecSet::pinned();
        let codecs = add_detected(
            &mut set,
            found(Some(t.ffmpeg.clone()), Some(t.ffprobe.clone())),
        )
        .unwrap();
        assert_eq!(codecs.unwrap().env().version, "9.0.2");
        assert!(set.decoder_by_id("opus-ffmpeg-dec/1").is_some());
        assert!(set.encoder_by_id("mp3-ffmpeg-enc/1").is_some());
        // 見つからない・版を答えない ffmpeg は足さない
        let broken = t.path("broken-ffmpeg");
        script(&broken, "exit 1");
        for (ffmpeg, ffprobe) in [
            (None, Some(t.ffprobe.clone())),
            (Some(t.ffmpeg.clone()), None),
            (Some(broken), Some(t.ffprobe.clone())),
            (Some(t.ffmpeg.clone()), Some(t.path("no-such-ffprobe"))),
        ] {
            let mut set = CodecSet::pinned();
            assert!(
                add_detected(&mut set, found(ffmpeg, ffprobe))
                    .unwrap()
                    .is_none()
            );
            assert!(set.decoder_by_id("opus-ffmpeg-dec/1").is_none());
        }
        // 同じものを 2 回足すことはできない（識別子が重なる）
        let codecs = t.codecs();
        let mut set = CodecSet::pinned();
        codecs.add_to(&mut set).unwrap();
        assert!(codecs.add_to(&mut set).is_err());
    }

    /// 同時に動かす数の枠：埋まっていれば待ち、待つあいだも中止が効き、返せば次が取れる。
    #[test]
    fn running_slots_are_bounded() {
        let t = FakeTools::new("exit 1", "exit 1", "exit 1");
        let codecs = t.codecs().with_max_running(1);
        let shared = &codecs.shared;
        let first = shared.acquire(&NoMonitor).unwrap();
        let flag = CancelFlag::default();
        flag.cancel();
        assert!(matches!(shared.acquire(&flag), Err(EngineError::Canceled)));
        std::thread::scope(|s| {
            let h = s.spawn(|| shared.acquire(&NoMonitor).map(|_| ()));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!h.is_finished());
            drop(first);
            assert!(h.join().unwrap().is_ok());
        });
    }

    /// 一時ファイルの名前は、同じ作業フォルダの組が 2 つあっても重ならず、すでにあるファイル（ほかのプロセスの残りなど）は
    /// 飛ばして、切り詰めも消しもしない。
    #[test]
    fn temp_files_never_collide() {
        let t = FakeTools::new("exit 1", "exit 1", "exit 1");
        let (a, b) = (t.codecs(), t.codecs());
        let (ta, _) = a.shared.create_temp("ffin", "opus").unwrap();
        let (tb, _) = b.shared.create_temp("ffin", "opus").unwrap();
        assert_ne!(ta.path(), tb.path());
        assert!(ta.path().is_file() && tb.path().is_file());

        let d = tempfile::tempdir().unwrap();
        let seq = AtomicU64::new(0);
        let taken = d.path().join(format!("ffenc-{}-0.mp3", std::process::id()));
        std::fs::write(&taken, b"other").unwrap();
        let (tmp, _) = create_temp_in(d.path(), "ffenc", "mp3", &seq).unwrap();
        assert_eq!(
            tmp.path(),
            d.path().join(format!("ffenc-{}-1.mp3", std::process::id()))
        );
        drop(tmp);
        assert_eq!(std::fs::read(&taken).unwrap(), b"other");
        assert!(
            !d.path()
                .join(format!("ffenc-{}-1.mp3", std::process::id()))
                .exists()
        );
    }
}
