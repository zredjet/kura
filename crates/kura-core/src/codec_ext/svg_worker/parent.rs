//! 親の側：固定の段の SVG のデコーダの代わりに、呼ぶたびに働き手（子）を起動して頼むデコーダ。

use std::ffi::OsString;
use std::io::SeekFrom;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use kura_engine::codec::{CodecEnv, Decoder, ReadSeek, Tier, svg, try_vec};
use kura_engine::doc::DecodeOptions;
use kura_engine::{EngineError, Limits, Monitor, NoMonitor, Shape, Value};

use super::wire::{self, Op, Reply, ReplyReader};
use super::{DECODE_DEADLINE, SHAPE_DEADLINE, TIME_LIMIT, WORKER_ARG};
use crate::tools::{self, Budget, RunError, RunOutcome, Stopped};

/// 働き手を起動するコマンド（実行ファイルと引数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCommand {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
}

impl WorkerCommand {
    /// 実行ファイルと引数から作る。
    pub fn new(
        exe: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            exe: exe.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// 自分の実行ファイルを [`WORKER_ARG`] 付きで起動する（アプリ・kura-devtools）。分からなければ理由。
    pub fn current_exe() -> Result<Self, String> {
        std::env::current_exe()
            .map(|exe| Self::new(exe, [WORKER_ARG]))
            .map_err(|e| format!("自分の実行ファイルが分かりません: {e}"))
    }
}

/// 時間の上限（形・値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    pub shape: Duration,
    pub decode: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            shape: SHAPE_DEADLINE,
            decode: DECODE_DEADLINE,
        }
    }
}

/// 働き手に頼む SVG のデコーダ。識別子・段・再現の約束・環境・読める形式・作業領域の見積もりは、差し替える前の
/// プロセスの中のデコーダ（`inner`）のものを返す（`inner` で読むことはしない）。
pub struct SvgWorkerDecoder {
    inner: Arc<dyn Decoder>,
    cmd: Result<WorkerCommand, String>,
    deadlines: Deadlines,
}

impl SvgWorkerDecoder {
    /// プロセスの中のデコーダ `inner` の代わりに、`cmd` の働き手に頼むものを作る。
    pub fn new(inner: Arc<dyn Decoder>, cmd: Result<WorkerCommand, String>) -> Self {
        Self {
            inner,
            cmd,
            deadlines: Deadlines::default(),
        }
    }

    /// 時間の上限を変える。
    pub fn with_deadlines(mut self, deadlines: Deadlines) -> Self {
        self.deadlines = deadlines;
        self
    }

    fn decode_error(&self, message: impl Into<String>) -> EngineError {
        EngineError::Decode {
            codec: self.inner.id().to_string(),
            message: message.into(),
        }
    }

    /// 読み口の中身を読む（大きさの上限は子を起こす前に、プロセスの中のデコーダと同じ言葉で断る）。
    fn read_all(&self, src: &mut dyn ReadSeek) -> Result<Vec<u8>, EngineError> {
        let len = src.seek(SeekFrom::End(0))?;
        svg::check_source_len(len)?;
        src.seek(SeekFrom::Start(0))?;
        let len = usize::try_from(len).map_err(|e| EngineError::Limit(e.to_string()))?;
        let mut b = try_vec(len, 0u8)?;
        src.read_exact(&mut b)
            .map_err(|_| self.decode_error("ファイルが読めません"))?;
        Ok(b)
    }

    /// パスを確かめる（ない・開けないは入出力のエラー、大きすぎるは `Limit`。どちらも子を起こさない）。子には絶対パスで渡す。
    fn checked_path(path: &Path) -> Result<PathBuf, EngineError> {
        let len = std::fs::metadata(path)?.len();
        svg::check_source_len(len)?;
        Ok(std::path::absolute(path)?)
    }

    /// 子を 1 つ起動して頼み、返事を受け取る。
    fn call(
        &self,
        op: Op,
        input: wire::Input<'_>,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Reply, EngineError> {
        let cmd = self
            .cmd
            .as_ref()
            .map_err(|m| self.decode_error(format!("SVG を読む働き手を起動できません（{m}）")))?;
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let deadline = match op {
            Op::Shape => self.deadlines.shape,
            Op::Decode => self.deadlines.decode,
        };
        let mut request = Vec::new();
        wire::write_request(&mut request, op, input, format, opts, limits, deadline)?;
        let mut reader = ReplyReader::new(op, limits);
        let budget = Budget {
            max_stdout: reader.max_len(),
            stall: deadline,
            deadline,
        };
        let mut broken: Option<String> = None;
        let result = {
            let mut feed = |w: &mut dyn std::io::Write| w.write_all(&request);
            let mut on_chunk = |chunk: &[u8]| -> Result<(), String> {
                reader.push(chunk).inspect_err(|e| broken = Some(e.clone()))
            };
            tools::run_binary_kill(
                &cmd.exe,
                &cmd.args,
                Path::new("/"),
                &budget,
                &|| mon.canceled(),
                Some(&mut feed),
                &mut on_chunk,
            )
        };
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(RunError::Stopped(Stopped::Canceled)) => return Err(EngineError::Canceled),
            Err(RunError::Stopped(Stopped::Deadline | Stopped::Stalled)) => {
                return Err(EngineError::Limit(TIME_LIMIT.into()));
            }
            Err(RunError::Stopped(Stopped::OutputLimit)) => {
                return Err(self.decode_error("SVG を読む働き手の返事が大きすぎます"));
            }
            Err(RunError::Stopped(Stopped::Consumer)) => {
                return Err(self.decode_error(format!(
                    "SVG を読む働き手の返事が壊れています（{}）",
                    broken.unwrap_or_default()
                )));
            }
            Err(RunError::Stopped(Stopped::Lingering)) => {
                return Err(self.decode_error("SVG を読む働き手が終わりません"));
            }
            Err(RunError::Spawn(e)) => {
                return Err(self.decode_error(format!("SVG を読む働き手を起動できません（{e}）")));
            }
            Err(RunError::Wait(e)) => {
                return Err(
                    self.decode_error(format!("SVG を読む働き手の終わりを待てません（{e}）"))
                );
            }
        };
        if !outcome.status.success() {
            let why = crashed(&outcome);
            tracing::warn!(exe = %cmd.exe.display(), "SVG を読む働き手が異常終了しました（{why}）");
            return Err(self.decode_error(format!("SVG を読む働き手が異常終了しました（{why}）")));
        }
        reader
            .finish()
            .map_err(|e| self.decode_error(format!("SVG を読む働き手の返事が壊れています（{e}）")))
    }

    fn shape_reply(&self, reply: Reply) -> Result<Shape, EngineError> {
        match reply {
            Reply::Shape(s) => Ok(s),
            Reply::Error(e) => Err(e),
            Reply::Value(_) => Err(self.decode_error("頼みと違う返事です")),
        }
    }

    fn value_reply(&self, reply: Reply) -> Result<Value, EngineError> {
        match reply {
            Reply::Value(v) => Ok(v),
            Reply::Error(e) => Err(e),
            Reply::Shape(_) => Err(self.decode_error("頼みと違う返事です")),
        }
    }
}

/// 異常終了の説明（シグナルか終了コードと、標準エラーの最初の行）。
fn crashed(outcome: &RunOutcome) -> String {
    let status: ExitStatus = outcome.status;
    let mut why = match (status.signal(), status.code()) {
        (Some(sig), _) => format!("シグナル {sig}"),
        (None, Some(code)) => format!("終了コード {code}"),
        (None, None) => status.to_string(),
    };
    if let Some(line) = outcome.stderr_head.first() {
        why.push_str(": ");
        why.push_str(line);
    }
    why
}

impl Decoder for SvgWorkerDecoder {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn tier(&self) -> Tier {
        self.inner.tier()
    }

    fn reproducible(&self) -> bool {
        self.inner.reproducible()
    }

    fn env(&self) -> Option<CodecEnv> {
        self.inner.env()
    }

    fn can_decode(&self, format: &str) -> bool {
        self.inner.can_decode(format)
    }

    fn work_bytes(&self, format: &str, shape: &Shape) -> u64 {
        self.inner.work_bytes(format, shape)
    }

    fn file_work_bytes(&self, format: &str, shape: &Shape, len: u64) -> u64 {
        self.inner.file_work_bytes(format, shape, len)
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.shape_monitored(src, format, opts, limits, &NoMonitor)
    }

    fn shape_monitored(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        let bytes = self.read_all(src)?;
        let reply = self.call(
            Op::Shape,
            wire::Input::Bytes(&bytes),
            format,
            opts,
            limits,
            mon,
        )?;
        self.shape_reply(reply)
    }

    fn shape_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.shape_file_monitored(path, format, opts, limits, &NoMonitor)
    }

    fn shape_file_monitored(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        let path = Self::checked_path(path)?;
        let reply = self.call(
            Op::Shape,
            wire::Input::Path(&path),
            format,
            opts,
            limits,
            mon,
        )?;
        self.shape_reply(reply)
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let bytes = self.read_all(src)?;
        let reply = self.call(
            Op::Decode,
            wire::Input::Bytes(&bytes),
            format,
            opts,
            limits,
            mon,
        )?;
        self.value_reply(reply)
    }

    fn decode_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let path = Self::checked_path(path)?;
        let reply = self.call(
            Op::Decode,
            wire::Input::Path(&path),
            format,
            opts,
            limits,
            mon,
        )?;
        self.value_reply(reply)
    }
}
