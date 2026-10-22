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
use super::{DECODE_DEADLINE, MEMORY_LIMIT, SHAPE_DEADLINE, TIME_LIMIT, WORKER_ARG, unavailable};
use crate::tools::{self, Budget, RunError, RunOutcome, Stopped};

/// 働き手の実行ファイルを求める口（起動のたびに呼ぶ）。
pub type ExeResolver = Arc<dyn Fn() -> std::io::Result<PathBuf> + Send + Sync>;

/// 働き手を起動するコマンド（実行ファイルを求める口と引数）。実行ファイルは起動のたびに求め直す（動いているアプリを移す・
/// 改名しても、今の場所を起動する。M11 の 3 回目の確認 W2）。
#[derive(Clone)]
pub struct WorkerCommand {
    exe: ExeResolver,
    args: Vec<OsString>,
}

impl std::fmt::Debug for WorkerCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerCommand")
            .field("exe", &"（起動のたびに求める）")
            .field("args", &self.args)
            .finish()
    }
}

impl WorkerCommand {
    /// 決まった実行ファイルと引数から作る（テスト・偽の働き手）。
    pub fn new(
        exe: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        let exe: PathBuf = exe.into();
        Self::resolved_by(move || Ok(exe.clone()), args)
    }

    /// 実行ファイルを起動のたびに `resolve` で求める形で作る。
    pub fn resolved_by(
        resolve: impl Fn() -> std::io::Result<PathBuf> + Send + Sync + 'static,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            exe: Arc::new(resolve),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// 自分の実行ファイル（起動のたびに `resolve` で求める）を [`WORKER_ARG`] 付きで起動する。アプリと kura-devtools は
    /// kura-mac の `current_executable`（自分のプロセスの今の実行ファイル。改名・移動を追う）を渡す。
    pub fn own_exe(resolve: impl Fn() -> std::io::Result<PathBuf> + Send + Sync + 'static) -> Self {
        Self::resolved_by(resolve, [WORKER_ARG])
    }

    /// [`Self::own_exe`] の OS によらない既定（`std::env::current_exe`。macOS では起動したときのパスで、改名を追わない）。
    pub fn current_exe() -> Self {
        Self::own_exe(std::env::current_exe)
    }

    /// 今の実行ファイル（求める口を呼ぶ）。
    pub fn exe(&self) -> std::io::Result<PathBuf> {
        (self.exe)()
    }

    /// 引数。
    pub fn args(&self) -> &[OsString] {
        &self.args
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
    cmd: WorkerCommand,
    /// 親のビルドの印（`super::build_stamp`。頼みに入れ、返事の印と照らす）。
    stamp: String,
    deadlines: Deadlines,
}

impl SvgWorkerDecoder {
    /// プロセスの中のデコーダ `inner` の代わりに、`cmd` の働き手に頼むものを作る。`stamp` は親のビルドの印。
    pub fn new(inner: Arc<dyn Decoder>, cmd: WorkerCommand, stamp: String) -> Self {
        Self {
            inner,
            cmd,
            stamp,
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

    /// 働き手を使えない一時的な誤りにする（ログにも出す）。
    fn unavailable(&self, why: impl std::fmt::Display) -> EngineError {
        let err = unavailable(why);
        tracing::warn!("{err}");
        err
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
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        // 実行ファイルは起動のたびに求める（動いているアプリを移す・改名しても、今の場所を起動する）
        let exe = self
            .cmd
            .exe()
            .map_err(|e| self.unavailable(format!("自分の実行ファイルが分かりません: {e}")))?;
        let deadline = match op {
            Op::Shape => self.deadlines.shape,
            Op::Decode => self.deadlines.decode,
        };
        let mut request = Vec::new();
        wire::write_request(
            &mut request,
            &self.stamp,
            op,
            input,
            format,
            opts,
            limits,
            deadline,
        )?;
        let mut reader = ReplyReader::new(op, limits, &self.stamp);
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
                &exe,
                self.cmd.args(),
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
                // 返事の印・版・ビルドの印が違う：別のビルドの働き手（一時的な誤り）
                if let Some(why) = reader.foreign() {
                    return Err(self.unavailable(format!("{} は{why}", exe.display())));
                }
                return Err(self.decode_error(format!(
                    "SVG を読む働き手の返事が壊れています（{}）",
                    broken.unwrap_or_default()
                )));
            }
            Err(RunError::Stopped(Stopped::Lingering)) => {
                return Err(self.decode_error("SVG を読む働き手が終わりません"));
            }
            Err(RunError::Spawn(e)) => {
                return Err(self.unavailable(format!("{} を起動できません: {e}", exe.display())));
            }
            Err(RunError::Wait(e)) => {
                return Err(
                    self.decode_error(format!("SVG を読む働き手の終わりを待てません（{e}）"))
                );
            }
        };
        if !outcome.status.success() {
            let why = crashed(&outcome);
            if ran_out_of_memory(&outcome) {
                // 確保の上限（か OS）に確保を断られて落ちた（ADR-164 (4)）
                tracing::warn!(exe = %exe.display(), "SVG を読む働き手が確保の上限を超えました（{why}）");
                return Err(self.decode_error(MEMORY_LIMIT));
            }
            tracing::warn!(exe = %exe.display(), "SVG を読む働き手が異常終了しました（{why}）");
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

/// 子が確保を断られて落ちたか（Rust が確保の失敗の言葉「memory allocation of N bytes failed」を標準エラーに書いて abort
/// した）。
fn ran_out_of_memory(outcome: &RunOutcome) -> bool {
    outcome
        .stderr_head
        .iter()
        .any(|l| l.starts_with("memory allocation of ") && l.ends_with(" bytes failed"))
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
