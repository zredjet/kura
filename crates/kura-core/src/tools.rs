//! 外部ツール（ffmpeg・yt-dlp・gallery-dl など）の場所と実行。GUI アプリはシェルの PATH を引き継がないので、
//! 設定での指定 → Homebrew の場所 → PATH の順に探す。

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;

/// 探す場所（Apple Silicon と Intel の Homebrew）。
pub const SEARCH_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// 状態を表示するツール（docs/architecture.md §9・§10。deno は yt-dlp が YouTube で使う。
/// chrome はキャプチャ用の Google Chrome）。
pub const KNOWN_TOOLS: [&str; 7] = [
    "yt-dlp",
    "gallery-dl",
    "deno",
    "ffmpeg",
    "ffprobe",
    "chrome",
    "mpv",
];

/// ツールの場所の指定を変えるテストを順に動かすための錠（指定はアプリ全体で 1 つ）。
#[cfg(test)]
pub(crate) static TEST_TOOLS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn defaults() -> &'static RwLock<BTreeMap<String, PathBuf>> {
    static DEFAULTS: OnceLock<RwLock<BTreeMap<String, PathBuf>>> = OnceLock::new();
    DEFAULTS.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// PATH にないツールの既定の場所（Google Chrome のアプリの中など。kura-mac が見つけて渡す）。
pub fn set_default_location(name: &str, path: PathBuf) {
    if let Ok(mut map) = defaults().write() {
        map.insert(name.to_string(), path);
    }
}

fn overrides() -> &'static RwLock<BTreeMap<String, PathBuf>> {
    static OVERRIDES: OnceLock<RwLock<BTreeMap<String, PathBuf>>> = OnceLock::new();
    OVERRIDES.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// 設定で指定した場所（ツール名 → 実行ファイル）。アプリの起動時と設定を変えたときに渡す。
pub fn set_overrides(map: BTreeMap<String, PathBuf>) {
    if let Ok(mut slot) = overrides().write() {
        *slot = map;
    }
}

/// 設定で指定した場所。
pub fn override_for(name: &str) -> Option<PathBuf> {
    overrides().read().ok()?.get(name).cloned()
}

/// ツールを探す（設定での指定 → 既定の場所 → Homebrew の場所 → PATH の順）。見つからなければ None。
pub fn find(name: &str) -> Option<PathBuf> {
    if let Some(path) = override_for(name).filter(|p| is_executable(p)) {
        return Some(path);
    }
    if let Some(path) = defaults()
        .read()
        .ok()
        .and_then(|m| m.get(name).cloned())
        .filter(|p| is_executable(p))
    {
        return Some(path);
    }
    let from_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    SEARCH_DIRS
        .iter()
        .map(PathBuf::from)
        .chain(from_path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// 子プロセスに渡す PATH（指定した場所・Homebrew・元の PATH）。yt-dlp が ffmpeg を、gallery-dl が
/// ffmpeg（うごイラの変換）を見つけられるようにする。
pub fn child_path() -> OsString {
    let mut dirs: Vec<PathBuf> = overrides()
        .read()
        .map(|m| {
            m.values()
                .filter_map(|p| p.parent().map(Path::to_path_buf))
                .collect()
        })
        .unwrap_or_default();
    dirs.extend(SEARCH_DIRS.iter().map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let new = !seen.contains(d);
        seen.push(d.clone());
        new
    });
    std::env::join_paths(dirs).unwrap_or_default()
}

/// ツールの状態（ダウンロード画面の「外部ツール」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    /// Homebrew で入れたもの（更新は `brew upgrade`）。
    pub homebrew: bool,
    /// 設定で場所を指定している。
    pub overridden: bool,
}

/// ツールを探して版を読む（5 秒で打ち切る）。
pub fn inspect(name: &str) -> ToolInfo {
    let path = find(name);
    let version = path.as_ref().and_then(|p| {
        let flag = if name.starts_with("ff") {
            "-version"
        } else {
            "--version"
        };
        let output = run_with_timeout(Command::new(p).arg(flag), Duration::from_secs(5)).ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        parse_version(name, text.lines().next()?)
    });
    let homebrew = path.as_ref().is_some_and(|p| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.clone())
            .to_string_lossy()
            .contains("/Cellar/")
    });
    ToolInfo {
        name: name.to_string(),
        overridden: override_for(name).is_some() && path == override_for(name),
        path,
        version,
        homebrew,
    }
}

/// 版の表示の 1 行目から版だけを取り出す。
fn parse_version(name: &str, line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    let version = match name {
        // 「ffmpeg version 8.0.1 Copyright …」「Google Chrome 155.0.8059.39」
        "ffmpeg" | "ffprobe" | "chrome" => words.get(2).copied(),
        // 「deno 2.9.7 (stable, …)」「mpv v0.40.0 Copyright …」
        "deno" | "mpv" => words.get(1).copied(),
        _ => words.first().copied(),
    }?;
    Some(version.trim_start_matches('v').to_string())
}

/// 子の起動の鍵（プロセスで 1 つ）。[`spawn`] を見る。
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// 子を起動する（kura の本番のコードは、子をすべてここか [`status`] で起動する。M11 の 3 回目の確認 W1）。
///
/// macOS の std はパイプを `pipe()` で作ってから別の呼び出しで FD_CLOEXEC を付けるので、その間に別のスレッドが子を起動すると、
/// その子がパイプの端を受け継ぐ（受け継いだ子が長く生きると、パイプの相手の EOF が来ない。SVG の働き手が兄弟の遅い子の
/// せいで時間切れになった）。起動（パイプを作るところから子が生まれるまで）を、プロセスで 1 つの鍵で順にする。鍵を持つのは
/// `Command::spawn` のあいだだけ（待つのは鍵の外）。`tests` の `production_code_spawns_through_the_lock` が src を見張る。
pub fn spawn(command: &mut Command) -> std::io::Result<Child> {
    let _guard = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    command.spawn()
}

/// `Command::status` の代わり：鍵の中で起動し（[`spawn`]）、鍵の外で終わるのを待つ。標準入出力は、指定がなければ親のものを
/// 受け継ぐ（`status` と同じ）。
pub fn status(command: &mut Command) -> std::io::Result<ExitStatus> {
    spawn(command)?.wait()
}

/// 外部コマンドを実行し、`timeout` を過ぎたら止める。標準出力・標準エラー出力を返す。
pub fn run_with_timeout(command: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    let mut child = spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;
    // 出力はパイプが詰まらないよう別スレッドで読む
    let stdout = child.stdout.take().map(read_in_thread);
    let stderr = child.stderr.take().map(read_in_thread);
    let status = wait_with_timeout(&mut child, timeout)?;
    let join = |h: Option<std::thread::JoinHandle<Vec<u8>>>| {
        h.map(|h| h.join().unwrap_or_default()).unwrap_or_default()
    };
    Ok(Output {
        status,
        stdout: join(stdout),
        stderr: join(stderr),
    })
}

fn read_in_thread(
    mut pipe: impl std::io::Read + Send + 'static,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::io::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{} 秒で終わらなかったので止めました", timeout.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `run_streaming` の終わり方。
#[derive(Debug)]
pub enum StreamEnd {
    /// プロセスが自分で終わった。
    Exited(ExitStatus),
    /// `stop` で止めた。
    Stopped,
    /// `stall` のあいだ何も出力しなかったので止めた。
    Stalled,
}

/// 外部コマンドを動かし、標準出力・標準エラー出力を 1 行ずつ `on_line(行, 標準エラーか)` に渡す。
/// `stop` が true を返すか、`stall` のあいだ出力がなければ、プロセスグループごと SIGTERM で止め、
/// 5 秒たっても終わらなければ SIGKILL にする（docs/architecture.md §5）。
pub fn run_streaming(
    command: &mut Command,
    on_line: &mut dyn FnMut(&str, bool),
    stop: &dyn Fn() -> bool,
    stall: Duration,
) -> std::io::Result<StreamEnd> {
    let mut child = spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 子プロセス（yt-dlp が呼ぶ ffmpeg など）もまとめて止められるように、新しいグループにする
            .process_group(0),
    )?;
    let (tx, rx) = mpsc::channel::<(String, bool)>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(read_lines(out, false, tx.clone()));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(read_lines(err, true, tx.clone()));
    }
    drop(tx);

    let mut last_output = Instant::now();
    let end = loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok((line, is_err)) => {
                last_output = Instant::now();
                on_line(&line, is_err);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // 出力を閉じたあと、終わるまでの短いあいだ
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if stop() {
            terminate_group(&mut child);
            break StreamEnd::Stopped;
        }
        if let Some(status) = child.try_wait()? {
            break StreamEnd::Exited(status);
        }
        if last_output.elapsed() > stall {
            terminate_group(&mut child);
            break StreamEnd::Stalled;
        }
    };
    // 終わったあとに残っている行を読み切る
    for reader in readers {
        let _ = reader.join();
    }
    while let Ok((line, is_err)) = rx.try_recv() {
        on_line(&line, is_err);
    }
    Ok(end)
}

fn read_lines(
    pipe: impl std::io::Read + Send + 'static,
    is_err: bool,
    tx: mpsc::Sender<(String, bool)>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    // 進捗を \r で上書きするツールもあるので、最後の部分だけ渡す
                    let line = line.trim_end_matches(['\n', '\r']);
                    let line = line.rsplit('\r').next().unwrap_or(line);
                    if tx.send((line.to_string(), is_err)).is_err() {
                        break;
                    }
                }
            }
        }
    })
}

/// プロセスグループに SIGTERM を送り、5 秒待っても終わらなければ SIGKILL にする
/// （`process_group(0)` で起動したものに使う）。
pub fn terminate_group(child: &mut Child) {
    let pid = child.id();
    let _ = signal_group(pid, "-TERM");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = signal_group(pid, "-KILL");
    let _ = child.kill();
    let _ = child.wait();
}

/// `kill` の場所（PATH に頼らない。Linux の一部は /usr/bin にだけある）。
const KILL_PATHS: [&str; 2] = ["/bin/kill", "/usr/bin/kill"];

/// プロセスグループ `pid`（`process_group(0)` で起動したものの pid）にシグナル（`-TERM`・`-KILL`）を送る。
/// `unsafe` を使わないため、`kill` のコマンドを絶対パスで呼ぶ。`kill` を起動できたかを返す（グループがもうなくて
/// `kill` が失敗で終わったときも真。偽は資源が尽きたなどで起動できず、シグナルが届いていないとき）。
pub fn signal_group(pid: u32, sig: &str) -> bool {
    let group = format!("-{pid}");
    let exe = KILL_PATHS
        .iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .unwrap_or(Path::new("kill"));
    status(
        Command::new(exe)
            .args([sig, "--", &group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok()
}

// ---- バイナリの流れ（ffmpeg のコーデックの段。標準入力に書き、標準出力をそのまま受け取る） ----

/// [`run_binary`] の予算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// 標準出力の合計の上限（バイト）。超えたら止める。
    pub max_stdout: u64,
    /// 標準出力も標準入力への書き込みも進まないまま、これだけたったら止める。
    pub stall: Duration,
    /// 全体の時間の上限。
    pub deadline: Duration,
}

/// [`run_binary`] が止めた理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// 中止を頼まれた。
    Canceled,
    /// `stall` のあいだ出力も入力も進まなかった。
    Stalled,
    /// `deadline` を過ぎた。
    Deadline,
    /// 標準出力が `max_stdout` を超えた。
    OutputLimit,
    /// 標準出力の受け手が止めた（受け手のエラー）。
    Consumer,
    /// 子が終わったあとも、標準入出力のパイプを開いたままの子孫（プロセスグループの外に出たもの）が残り、グループへの
    /// SIGKILL のあと 2 秒たっても閉じなかった（パイプを持つスレッドを見捨てて戻った）。
    Lingering,
}

/// [`run_binary`] の失敗（起動できない・止めた）。プロセスが自分で終わったときは [`RunOutcome`] を返す。
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("起動できません: {0}")]
    Spawn(std::io::Error),
    #[error("止めました（{0:?}）")]
    Stopped(Stopped),
    #[error("終わりを待てません: {0}")]
    Wait(std::io::Error),
}

/// 自分で終わったプロセスの結果。成功かどうかは呼び手が決める（終了コード・`[error]` の行・入力の書き込みの失敗）。
#[derive(Debug)]
pub struct RunOutcome {
    pub status: ExitStatus,
    /// 標準エラーの `[error]`・`[fatal]`・`[panic]` の行（ffmpeg の `-loglevel level+…`。先頭から [`KEEP_LINES`] 行まで）。
    pub error_lines: Vec<String>,
    /// 標準エラーの `[warning]` の行（同じく [`KEEP_LINES`] 行まで）。
    pub warning_lines: Vec<String>,
    /// 標準エラーの先頭の行（印によらず [`KEEP_LINES`] 行まで。印のない道具の失敗の説明に使う）。
    pub stderr_head: Vec<String>,
    pub stdout_bytes: u64,
    /// 標準入力のパイプに書けたバイト数。
    pub stdin_bytes: u64,
    /// 標準入力への書き込みの失敗（相手が先に終わったなど）。
    pub feed_error: Option<std::io::Error>,
}

/// 標準エラーから残す行の数（種類ごと）。
pub const KEEP_LINES: usize = 16;
/// 標準エラーの 1 行から残すバイト数（残りは読み捨てる）。
pub const KEEP_LINE_BYTES: usize = 512;
/// 見張りが中止・時間を確かめる間隔。
const WATCH_TICK: Duration = Duration::from_millis(20);
/// SIGTERM のあと SIGKILL にするまでの時間。子を待ち終えたのにパイプを開いたままの子孫が残っているときは、これだけ
/// たってグループに SIGKILL、さらにこれだけたっても閉じなければ、パイプを持つスレッドを見捨てて戻る。
const KILL_GRACE: Duration = Duration::from_secs(2);
/// 標準出力を読む区切り。
const READ_CHUNK: usize = 256 * 1024;
/// 標準出力の読み手が受け手に渡して待たせる区切りの数（読み手が先に進むのは `READ_CHUNK` × これまで）。
const OUT_QUEUE: usize = 4;
/// 標準入力の書き手に 1 回で渡す大きさの上限。
const IN_CHUNK: usize = 256 * 1024;
/// 標準入力の書き手に待たせる量の上限（これを超えると `feed` の書き込みが待つ）。
const IN_QUEUE_BYTES: usize = 1 << 20;

/// 標準入力に書く関数（`run_binary` を呼んだスレッドで動く）。
pub type Feed<'a> = &'a mut dyn FnMut(&mut dyn std::io::Write) -> std::io::Result<()>;

/// 標準出力の受け手（受け手のスレッドで動く）。`Err` を返すとプロセスを止める（[`Stopped::Consumer`]）。
pub type OnChunk<'a> = &'a mut (dyn FnMut(&[u8]) -> Result<(), String> + Send);

/// 標準エラーの残した行。
#[derive(Default)]
struct StderrLog {
    errors: Vec<String>,
    warnings: Vec<String>,
    head: Vec<String>,
}

impl StderrLog {
    fn push(&mut self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.head.len() < KEEP_LINES {
            self.head.push(text.to_string());
        }
        let target = if ["[error]", "[fatal]", "[panic]"]
            .iter()
            .any(|t| text.contains(t))
        {
            &mut self.errors
        } else if text.contains("[warning]") {
            &mut self.warnings
        } else {
            return;
        };
        if target.len() < KEEP_LINES {
            target.push(text.to_string());
        }
    }
}

/// 標準エラーを読み切る（行の数と長さを抑え、残りは読み捨てて詰まらせない。改行のない長い行でも確保は増えない）。
fn read_stderr(mut pipe: impl std::io::Read, log: &Mutex<StderrLog>) {
    let mut buf = [0u8; 8192];
    let mut line: Vec<u8> = Vec::with_capacity(KEEP_LINE_BYTES);
    let flush = |line: &mut Vec<u8>| {
        if let Ok(mut l) = log.lock() {
            l.push(line);
        }
        line.clear();
    };
    loop {
        let n = match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                flush(&mut line);
            } else if line.len() < KEEP_LINE_BYTES {
                line.push(b);
            }
        }
    }
    flush(&mut line);
}

/// 最後に出力か入力が進んだ時刻（見張りの無出力の判定。パイプを持つスレッドと共有する）。
struct Clock {
    started: Instant,
    /// `started` からのミリ秒。
    last: AtomicU64,
}

impl Clock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            last: AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        let ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last.store(ms, Ordering::Relaxed);
    }

    fn idle(&self) -> Duration {
        self.started
            .elapsed()
            .saturating_sub(Duration::from_millis(self.last.load(Ordering::Relaxed)))
    }
}

/// パイプを持つスレッドの数（[`OpenGuard`] で数える）。子を待ち終えたあとも 0 にならなければ、パイプを開いたままの
/// 子孫が残っている。
type OpenCount = Arc<AtomicUsize>;

/// パイプを持つスレッドの数を作るときに 1 つ増やし、`Drop` で 1 つ減らして見張りを起こす（起動に失敗したスレッドの分も
/// 戻る）。
struct OpenGuard(OpenCount, Arc<Wake>);

impl OpenGuard {
    fn new(count: &OpenCount, wake: &Arc<Wake>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(count), Arc::clone(wake))
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
        self.1.notify();
    }
}

/// 見張りを起こす合図（子を待ち終えた・パイプを持つスレッドが抜けた・受け手が終わった）。見張りは刻み（[`WATCH_TICK`]）ごとに
/// 中止・時間を確かめ、合図があれば刻みを待たずに終わりを確かめる（すぐ終わる子を刻みの分だけ待たせない）。世代の数で、
/// 見張りが待つ前に来た合図も取りこぼさない。
#[derive(Default)]
struct Wake {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl Wake {
    fn notify(&self) {
        *self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += 1;
        self.changed.notify_all();
    }

    /// 世代が `seen` から変わっていなければ、合図か `tick` まで待つ。今の世代を返す。
    fn wait(&self, seen: u64, tick: Duration) -> u64 {
        let g = self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *g != seen {
            return *g;
        }
        match self.changed.wait_timeout(g, tick) {
            Ok((g, _)) => *g,
            Err(e) => *e.into_inner().0,
        }
    }
}

/// 標準出力を `READ_CHUNK` ずつ読んで受け手へ渡す（パイプを持つ `'static` のスレッドで動く）。受け手が遅くて渡すのを
/// 待つあいだは `open` から外す（パイプを開いたままの子孫が残っているかの判定に、受け手の遅さを数えない）。
fn pump_stdout(
    mut pipe: impl std::io::Read,
    tx: &mpsc::SyncSender<Vec<u8>>,
    clock: &Clock,
    open: &AtomicUsize,
) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        let n = match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        clock.touch();
        open.fetch_sub(1, Ordering::SeqCst);
        let sent = tx.send(buf[..n].to_vec());
        open.fetch_add(1, Ordering::SeqCst);
        if sent.is_err() {
            break;
        }
    }
}

/// 標準入力の書き手のスレッドとの受け渡し。パイプに書くのは書き手（`'static` のスレッド）だけなので、標準入力を
/// 持ったまま読まない子孫が残っても、`feed` を呼ぶスレッドは書き込みで止まらずに戻れる。
#[derive(Default)]
struct InQueue {
    state: Mutex<InState>,
    changed: Condvar,
    /// パイプに書けたバイト数。
    written: AtomicU64,
}

#[derive(Default)]
struct InState {
    chunks: VecDeque<Vec<u8>>,
    /// `chunks` と、書き手が取り出してまだパイプに書き終えていない区切りの合計のバイト数。
    queued: usize,
    /// `feed` が書き終えた（書き手は残りを書いたら閉じて EOF を知らせる）。
    closed: bool,
    /// 見捨てる（書き手は残りを書かずに抜ける）。
    abandoned: bool,
    /// 書き手が抜けた。
    gone: bool,
    /// パイプへの書き込みの失敗（相手が先に終わったなど）。
    error: Option<std::io::Error>,
}

impl InQueue {
    fn lock(&self) -> MutexGuard<'_, InState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, st: MutexGuard<'a, InState>) -> MutexGuard<'a, InState> {
        match self.changed.wait_timeout(st, WATCH_TICK) {
            Ok((st, _)) => st,
            Err(e) => e.into_inner().0,
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    fn abandon(&self) {
        self.lock().abandoned = true;
        self.changed.notify_all();
    }

    /// 取り出した区切り（`n` バイト）をパイプに書き終えた。
    fn wrote(&self, n: usize) {
        let mut st = self.lock();
        st.queued = st.queued.saturating_sub(n);
        self.changed.notify_all();
    }

    /// 次に書く区切り（閉じて空になったか、見捨てたら `None`）。書き終えたら [`InQueue::wrote`] を呼ぶ。
    fn next_chunk(&self) -> Option<Vec<u8>> {
        let mut st = self.lock();
        loop {
            if st.abandoned {
                return None;
            }
            if let Some(c) = st.chunks.pop_front() {
                return Some(c);
            }
            if st.closed {
                return None;
            }
            st = self.wait(st);
        }
    }
}

/// 標準入力の書き手（パイプを持つ `'static` のスレッドで動く）。抜けるときにパイプを閉じて EOF を知らせる。
fn write_stdin(mut pipe: impl std::io::Write, queue: &InQueue, clock: &Clock) {
    let result = (|| -> std::io::Result<()> {
        while let Some(chunk) = queue.next_chunk() {
            let mut rest = chunk.as_slice();
            while !rest.is_empty() {
                match pipe.write(rest) {
                    Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                    Ok(n) => {
                        rest = &rest[n..];
                        queue.written.fetch_add(n as u64, Ordering::Relaxed);
                        clock.touch();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            queue.wrote(chunk.len());
        }
        Ok(())
    })();
    drop(pipe);
    let mut st = queue.lock();
    st.gone = true;
    st.error = result.err();
    st.chunks.clear();
    st.queued = 0;
    queue.changed.notify_all();
}

/// `feed` に渡す書き込み口：区切りを書き手に渡す（待たせる量が上限なら空くまで待つ）。止めたあと・書き手が抜けた
/// あとは書かない（`write_all` が回り続けないように）。
struct InWriter<'a> {
    queue: &'a InQueue,
    stop: &'a AtomicU8,
}

impl InWriter<'_> {
    /// 書き手が失敗した・止めた・見捨てたときのエラー。
    fn refused(&self, st: &InState) -> Option<std::io::Error> {
        if let Some(e) = &st.error {
            return Some(std::io::Error::new(e.kind(), e.to_string()));
        }
        (self.stop.load(Ordering::SeqCst) != 0 || st.abandoned || st.gone)
            .then(|| std::io::Error::other("止めました"))
    }
}

impl std::io::Write for InWriter<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if b.is_empty() {
            return Ok(0);
        }
        let mut st = self.queue.lock();
        loop {
            if let Some(e) = self.refused(&st) {
                return Err(e);
            }
            if st.queued < IN_QUEUE_BYTES {
                break;
            }
            st = self.queue.wait(st);
        }
        let n = b.len().min(IN_CHUNK);
        st.chunks.push_back(b[..n].to_vec());
        st.queued += n;
        self.queue.changed.notify_all();
        Ok(n)
    }

    /// 渡した区切りをすべてパイプに書き終えるまで待つ。
    fn flush(&mut self) -> std::io::Result<()> {
        let mut st = self.queue.lock();
        loop {
            if let Some(e) = self.refused(&st) {
                return Err(e);
            }
            if st.queued == 0 {
                return Ok(());
            }
            st = self.queue.wait(st);
        }
    }
}

/// `Drop` で書き手に「書き終えた」を知らせる（`feed` が panic しても、書き手が残りを書いて抜けられるように）。
struct CloseGuard<'a>(&'a InQueue);

impl Drop for CloseGuard<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// 落ちても・エラーで戻っても、子をグループごと止めて回収する（ゾンビ・孤児を残さない）。グループへの SIGKILL は、
/// 見張りがもう送っていれば送らない（1 回だけ）。
struct Reaper<'a> {
    child: Option<Child>,
    group_killed: &'a AtomicBool,
}

impl Drop for Reaper<'_> {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            if matches!(c.try_wait(), Ok(None)) {
                if !self.group_killed.swap(true, Ordering::SeqCst) {
                    let _ = signal_group(c.id(), "-KILL");
                }
                let _ = c.kill();
            }
            let _ = c.wait();
        }
    }
}

/// 抜けた（子を待ち終えた・受け手が終わった）ことを、`Drop` で見張りに知らせる（印を立てて起こす）。
struct DoneGuard<'a>(&'a AtomicBool, &'a Wake);

impl Drop for DoneGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        self.1.notify();
    }
}

/// 止めた理由の番号（0 は止めていない）。
fn stop_code(s: Stopped) -> u8 {
    match s {
        Stopped::Canceled => 1,
        Stopped::Stalled => 2,
        Stopped::Deadline => 3,
        Stopped::OutputLimit => 4,
        Stopped::Consumer => 5,
        Stopped::Lingering => 6,
    }
}

fn stop_from_code(c: u8) -> Option<Stopped> {
    Some(match c {
        1 => Stopped::Canceled,
        2 => Stopped::Stalled,
        3 => Stopped::Deadline,
        4 => Stopped::OutputLimit,
        5 => Stopped::Consumer,
        6 => Stopped::Lingering,
        _ => return None,
    })
}

/// 起動する（[`spawn`]。起動の鍵の中で）。書いたばかりの実行ファイルを、別のスレッドの fork が書き込みのまま持っている
/// （ETXTBSY）ときは少し待ってやり直す（偽の道具を書いてすぐ動かすテストで起きる。待つのは鍵の外）。
fn spawn_retrying(cmd: &mut Command) -> std::io::Result<Child> {
    let mut tries = 0;
    loop {
        match spawn(cmd) {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

/// パイプを持つ `'static` のスレッドを起動する（`guard` はスレッドが終わるときに数を戻す。起動に失敗しても戻る）。
fn spawn_pipe_thread(
    name: &str,
    guard: OpenGuard,
    f: impl FnOnce(&AtomicUsize) + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let guard = guard;
            f(&guard.0);
        })
        .map(drop)
}

/// 外部の道具をバイナリの流れで動かす（ffmpeg のコーデックの段。docs/editing.md §13 の安全）。
///
/// - 起動：環境変数を消し（`PATH=/usr/bin:/bin`・`LC_ALL=C` だけ）、`cwd` で、新しいプロセスグループにする。
///   `feed` がなければ標準入力は閉じる。
/// - パイプに触るのは `'static` のスレッドだけ（標準出力の読み手・標準エラーの読み手・標準入力の書き手）。`feed` は
///   このスレッドで書き、区切りを書き手に渡す（1 MB まで待たせる）。標準出力は読み手が `READ_CHUNK` ずつ受け手の
///   スレッドに渡し、受け手が `on_chunk` を呼ぶ。標準エラーは行の数と長さを抑えて残す（[`RunOutcome`]）。
/// - 見張りのスレッドが 20 ms ごとに中止（`canceled`）・無出力（`stall`）・時間切れ（`deadline`）を見て（子が終わった
///   あとも見る）、理由を 1 回だけ記録してグループに SIGTERM、2 秒で SIGKILL。標準出力の上限・受け手のエラーでも同じく
///   止める。グループへの SIGKILL は 1 回だけ送る（`kill` を起動できなかったときだけ、次の刻みで送り直す）。子を待ち
///   終えた・パイプを持つスレッドが抜けた・受け手が終わったときは見張りを起こし、刻みを待たずに終わる（すぐ終わる子を
///   20 ms 待たせない）。
/// - 子を待ち終えたのにパイプが閉じない（パイプを持つ子孫が残っている）とき：止めたあとなら、すぐグループに SIGKILL
///   して、パイプを持つスレッドを見捨てて戻る（記録した理由のエラー）。止めていなければ、2 秒でグループに SIGKILL、
///   さらに 2 秒で見捨てて [`Stopped::Lingering`] で戻る。グループの外に出た子孫（`setsid` など）は SIGKILL が届かない
///   ので、見捨てたスレッドはその子孫がパイプを閉じるまで残る。
/// - エラーで戻っても panic でも、子は `Drop` で止めて回収する。
pub fn run_binary(
    exe: &Path,
    args: &[OsString],
    cwd: &Path,
    budget: &Budget,
    canceled: &(dyn Fn() -> bool + Sync),
    feed: Option<Feed<'_>>,
    on_chunk: OnChunk<'_>,
) -> Result<RunOutcome, RunError> {
    run_binary_with(
        exe,
        args,
        cwd,
        budget,
        canceled,
        feed,
        on_chunk,
        &signal_group,
    )
}

/// [`run_binary`] と同じだが、止めるとき（中止・無出力・時間切れ・出力の上限・受け手のエラー）に SIGTERM で待たず、すぐ
/// グループに SIGKILL を送る。後始末の要らない自前の働き手（SVG を読む子。`codec_ext::svg_worker`。決定 23）に使う。
pub fn run_binary_kill(
    exe: &Path,
    args: &[OsString],
    cwd: &Path,
    budget: &Budget,
    canceled: &(dyn Fn() -> bool + Sync),
    feed: Option<Feed<'_>>,
    on_chunk: OnChunk<'_>,
) -> Result<RunOutcome, RunError> {
    run_binary_with(
        exe,
        args,
        cwd,
        budget,
        canceled,
        feed,
        on_chunk,
        &|pid, sig| signal_group(pid, if sig == "-TERM" { "-KILL" } else { sig }),
    )
}

/// [`run_binary`] の本体。`signal` はグループにシグナルを送る関数（[`signal_group`] と同じく、送れたかを返す。テストが
/// 送った数を数え、送れなかったことにする）。
#[allow(clippy::too_many_arguments)]
fn run_binary_with(
    exe: &Path,
    args: &[OsString],
    cwd: &Path,
    budget: &Budget,
    canceled: &(dyn Fn() -> bool + Sync),
    feed: Option<Feed<'_>>,
    on_chunk: OnChunk<'_>,
    signal: &(dyn Fn(u32, &str) -> bool + Sync),
) -> Result<RunOutcome, RunError> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .current_dir(cwd)
        .stdin(if feed.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let stop = AtomicU8::new(0);
    let done = AtomicBool::new(false);
    let consumer_done = AtomicBool::new(false);
    let abandoned = AtomicBool::new(false);
    let group_killed = AtomicBool::new(false);
    let open: OpenCount = Arc::new(AtomicUsize::new(0));
    let wake = Arc::new(Wake::default());
    let log = Arc::new(Mutex::new(StderrLog::default()));
    let queue = Arc::new(InQueue::default());
    let out_total = AtomicU64::new(0);

    let mut child = spawn_retrying(&mut cmd).map_err(RunError::Spawn)?;
    let clock = Arc::new(Clock::new());
    let pid = child.id();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut reaper = Reaper {
        child: Some(child),
        group_killed: &group_killed,
    };

    // パイプを持つスレッド（起動に失敗したら、子は `Reaper` が止める）
    let (out_tx, out_rx) = mpsc::sync_channel::<Vec<u8>>(OUT_QUEUE);
    match stdout {
        Some(pipe) => {
            let clock = Arc::clone(&clock);
            spawn_pipe_thread(
                "kura-tool-stdout",
                OpenGuard::new(&open, &wake),
                move |open| {
                    pump_stdout(pipe, &out_tx, &clock, open);
                },
            )
            .map_err(RunError::Spawn)?;
        }
        None => drop(out_tx),
    }
    if let Some(pipe) = stderr {
        let log = Arc::clone(&log);
        spawn_pipe_thread(
            "kura-tool-stderr",
            OpenGuard::new(&open, &wake),
            move |_| {
                read_stderr(pipe, &log);
            },
        )
        .map_err(RunError::Spawn)?;
    }
    let writer = match stdin {
        Some(pipe) => {
            let (queue, clock) = (Arc::clone(&queue), Arc::clone(&clock));
            spawn_pipe_thread("kura-tool-stdin", OpenGuard::new(&open, &wake), move |_| {
                write_stdin(pipe, &queue, &clock);
            })
            .map_err(RunError::Spawn)?;
            true
        }
        None => false,
    };

    let kill_for = |why: Stopped| {
        if stop
            .compare_exchange(0, stop_code(why), Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            // TERM で止まらなければ（届かなくても）、見張りが KILL_GRACE のあとに KILL する
            let _ = signal(pid, "-TERM");
        }
    };
    // グループへの KILL は 1 回だけ（回収したあとに pgid が使い回されても、ほかのプロセスに送らないように）。`kill` を
    // 起動できなかったときは送ったことにせず、偽を返す（見張りが次の刻みで送り直す。回収するまでは子の pid がゾンビとして
    // 残るので、pgid は使い回されない）。見張りのほかに呼ぶのは見張りを起こせなかったときだけなので、同時には呼ばれない
    let kill_group = || {
        if group_killed.load(Ordering::SeqCst) {
            return true;
        }
        let sent = signal(pid, "-KILL");
        if sent {
            group_killed.store(true, Ordering::SeqCst);
        }
        sent
    };
    let mut feed_error = None;
    let mut status: Option<std::io::Result<ExitStatus>> = None;
    let mut thread_error = None;

    std::thread::scope(|s| {
        // 抜けるときに必ず「終わった」にする（`feed` が panic しても、見張りが子孫の片付けに移って終われるように）
        let _done = DoneGuard(&done, &wake);
        let consumer = {
            let (out_total, kill_for, abandoned) = (&out_total, &kill_for, &abandoned);
            let (consumer_done, wake) = (&consumer_done, &*wake);
            let on_chunk = &mut *on_chunk;
            move || {
                let _done = DoneGuard(consumer_done, wake);
                while !abandoned.load(Ordering::SeqCst) {
                    let chunk = match out_rx.recv_timeout(WATCH_TICK) {
                        Ok(c) => c,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let n = chunk.len() as u64;
                    if out_total.fetch_add(n, Ordering::Relaxed) + n > budget.max_stdout {
                        kill_for(Stopped::OutputLimit);
                        break;
                    }
                    if on_chunk(&chunk).is_err() {
                        kill_for(Stopped::Consumer);
                        break;
                    }
                }
            }
        };
        let watcher = {
            let (stop, done, consumer_done, abandoned) = (&stop, &done, &consumer_done, &abandoned);
            let (open, clock, queue, wake) = (&*open, &*clock, &*queue, &*wake);
            let (kill_for, kill_group) = (&kill_for, &kill_group);
            move || {
                let mut stop_seen: Option<Instant> = None;
                let mut done_seen: Option<Instant> = None;
                let mut killed_at: Option<Instant> = None;
                let mut seen = 0;
                loop {
                    let finished = done.load(Ordering::SeqCst)
                        && open.load(Ordering::SeqCst) == 0
                        && consumer_done.load(Ordering::SeqCst);
                    if finished || abandoned.load(Ordering::SeqCst) {
                        break;
                    }
                    // 刻みごとに中止・時間を確かめる。終わりの合図があれば刻みを待たずに確かめ直す
                    seen = wake.wait(seen, WATCH_TICK);
                    if stop.load(Ordering::SeqCst) == 0 {
                        if canceled() {
                            kill_for(Stopped::Canceled);
                        } else if clock.idle() > budget.stall {
                            kill_for(Stopped::Stalled);
                        } else if clock.started.elapsed() > budget.deadline {
                            kill_for(Stopped::Deadline);
                        }
                    }
                    let now = Instant::now();
                    let stopped = stop.load(Ordering::SeqCst) != 0;
                    if stopped {
                        stop_seen.get_or_insert(now);
                    }
                    if done.load(Ordering::SeqCst) {
                        done_seen.get_or_insert(now);
                    }
                    let over = |t: Option<Instant>| {
                        t.is_some_and(|t| now.saturating_duration_since(t) > KILL_GRACE)
                    };
                    // 子を待ち終えたのに、パイプを開いたままの子孫が残っている
                    let lingering = done_seen.is_some() && open.load(Ordering::SeqCst) > 0;
                    // 止めたあとなら、すぐ。止めていなければ、SIGKILL のあと KILL_GRACE たっても閉じないとき。
                    let give_up = lingering && (stopped || over(killed_at));
                    let kill_due = over(stop_seen) || (lingering && over(done_seen)) || give_up;
                    if killed_at.is_none() && kill_due && kill_group() {
                        killed_at = Some(now);
                    }
                    if give_up {
                        // パイプを持つスレッドを見捨てて戻る（理由がまだなければ「残った」）
                        let _ = stop.compare_exchange(
                            0,
                            stop_code(Stopped::Lingering),
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        );
                        abandoned.store(true, Ordering::SeqCst);
                        queue.abandon();
                        break;
                    }
                }
            }
        };
        let started = std::thread::Builder::new()
            .name("kura-tool-consumer".into())
            .spawn_scoped(s, consumer)
            .and_then(|_| {
                std::thread::Builder::new()
                    .name("kura-tool-watch".into())
                    .spawn_scoped(s, watcher)
            });
        if let Err(e) = started {
            // 見張りなしでは待てない：グループを止め、ほかのスレッドを抜けさせる（子は `Reaper` が回収する）
            thread_error = Some(e);
            abandoned.store(true, Ordering::SeqCst);
            queue.abandon();
            let _ = kill_group();
            return;
        }
        if let (true, Some(feed)) = (writer, feed) {
            // 抜けるとき（panic でも）に書き終えたことにして、書き手が残りを書いたら EOF を知らせる
            let _close = CloseGuard(&queue);
            let mut w = InWriter {
                queue: &queue,
                stop: &stop,
            };
            if let Err(e) = feed(&mut w) {
                feed_error = Some(e);
            }
        }
        // 子が終わるのを待つ（見張りが止めるので必ず戻る）
        if let Some(c) = reaper.child.as_mut() {
            status = Some(c.wait());
        }
    });
    if let Some(e) = thread_error {
        return Err(RunError::Spawn(e));
    }
    if matches!(status, Some(Ok(_))) {
        reaper.child = None;
    }
    if let Some(why) = stop_from_code(stop.load(Ordering::SeqCst)) {
        return Err(RunError::Stopped(why));
    }
    let status = match status {
        Some(Ok(s)) => s,
        Some(Err(e)) => return Err(RunError::Wait(e)),
        None => {
            return Err(RunError::Wait(std::io::Error::other(
                "子プロセスがありません",
            )));
        }
    };
    let feed_error = feed_error.or_else(|| queue.lock().error.take());
    let log = std::mem::take(&mut *log.lock().unwrap_or_else(PoisonError::into_inner));
    Ok(RunOutcome {
        status,
        error_lines: log.errors,
        warning_lines: log.warnings,
        stderr_head: log.head,
        stdout_bytes: out_total.load(Ordering::Relaxed),
        stdin_bytes: queue.written.load(Ordering::Relaxed),
        feed_error,
    })
}

/// 短い問い合わせ（ffprobe・`-version` など）：標準出力を `max_stdout` バイトまで集めて返す。`timeout` を過ぎたら止める。
pub fn run_capped(
    exe: &Path,
    args: &[OsString],
    cwd: &Path,
    timeout: Duration,
    max_stdout: u64,
    canceled: &(dyn Fn() -> bool + Sync),
) -> Result<(RunOutcome, Vec<u8>), RunError> {
    let mut out = Vec::new();
    let budget = Budget {
        max_stdout,
        stall: timeout,
        deadline: timeout,
    };
    let outcome = run_binary(
        exe,
        args,
        cwd,
        &budget,
        canceled,
        None,
        &mut |chunk: &[u8]| {
            out.extend_from_slice(chunk);
            Ok(())
        },
    )?;
    Ok((outcome, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tools_on_path_and_misses_unknown_ones() {
        // sh はどの環境の PATH にもある
        assert!(find("sh").is_some());
        assert!(find("kura-no-such-tool").is_none());
    }

    #[test]
    fn stops_commands_that_take_too_long() {
        let started = Instant::now();
        let result = run_with_timeout(
            Command::new("sh").args(["-c", "sleep 5"]),
            Duration::from_millis(200),
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));

        let ok = run_with_timeout(
            Command::new("sh").args(["-c", "echo hi"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(ok.status.success());
        assert_eq!(ok.stdout, b"hi\n");
    }

    #[test]
    fn streams_lines_from_both_pipes() {
        let mut lines = Vec::new();
        let end = run_streaming(
            Command::new("sh").args(["-c", "echo one; echo two 1>&2; printf 'a\\rb\\n'; exit 3"]),
            &mut |line, is_err| lines.push((line.to_string(), is_err)),
            &|| false,
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(matches!(end, StreamEnd::Exited(s) if s.code() == Some(3)));
        lines.sort();
        assert_eq!(
            lines,
            vec![
                ("b".to_string(), false),
                ("one".to_string(), false),
                ("two".to_string(), true),
            ]
        );
    }

    #[test]
    fn stops_the_whole_process_group() {
        let started = Instant::now();
        let flag = std::sync::atomic::AtomicBool::new(false);
        let end = run_streaming(
            // 子プロセス（sleep）ごと止まらないと、パイプが閉じずに読み終わらない
            Command::new("sh").args(["-c", "echo ready; sleep 30 & sleep 30"]),
            &mut |_, _| flag.store(true, std::sync::atomic::Ordering::SeqCst),
            &|| flag.load(std::sync::atomic::Ordering::SeqCst),
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(matches!(end, StreamEnd::Stopped));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn stops_silent_processes() {
        let end = run_streaming(
            Command::new("sh").args(["-c", "sleep 30"]),
            &mut |_, _| {},
            &|| false,
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(matches!(end, StreamEnd::Stalled));
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        p
    }

    fn budget(max: u64, stall_ms: u64) -> Budget {
        Budget {
            max_stdout: max,
            stall: Duration::from_millis(stall_ms),
            deadline: Duration::from_secs(20),
        }
    }

    /// `mark` を含むコマンドのプロセスが残っていない。
    fn gone(mark: &str) -> bool {
        let out = Command::new("ps")
            .args(["-A", "-o", "command="])
            .output()
            .unwrap();
        !String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.contains(mark))
    }

    fn run_simple(
        exe: &Path,
        b: &Budget,
        canceled: &(dyn Fn() -> bool + Sync),
        on_chunk: OnChunk<'_>,
    ) -> Result<RunOutcome, RunError> {
        run_binary(exe, &[], Path::new("/"), b, canceled, None, on_chunk)
    }

    #[test]
    fn binary_output_is_passed_through_and_counted() {
        let d = tempfile::tempdir().unwrap();
        let s = script(d.path(), "ok", "head -c 800000 /dev/zero; echo note >&2");
        let mut n = 0u64;
        let r = run_simple(&s, &budget(1 << 20, 5_000), &|| false, &mut |b: &[u8]| {
            n += b.len() as u64;
            Ok(())
        })
        .unwrap();
        assert!(r.status.success());
        assert_eq!((r.stdout_bytes, n), (800_000, 800_000));
        assert_eq!(r.stderr_head, ["note"]);
        assert!(r.error_lines.is_empty() && r.feed_error.is_none());
    }

    #[test]
    fn endless_output_hits_the_limit() {
        let d = tempfile::tempdir().unwrap();
        let s = script(d.path(), "endless", "cat /dev/zero");
        let t = Instant::now();
        let r = run_simple(
            &s,
            &budget(10 << 20, 5_000),
            &|| false,
            &mut |_: &[u8]| Ok(()),
        );
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::OutputLimit))),
            "{r:?}"
        );
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    /// すぐ終わる子は、見張りの刻み（20 ms）を待たずに戻る（子を待ち終えた・パイプが閉じた・受け手が終わったときに見張りを
    /// 起こす。SVG の働き手は 1 件で子を 2〜3 回起動するので、刻みの待ちがそのまま 1 件の時間に乗っていた）。
    #[test]
    fn a_quick_child_returns_without_waiting_for_the_watch_tick() {
        let d = tempfile::tempdir().unwrap();
        let s = script(d.path(), "quick", "printf ok; echo note >&2");
        let fastest = (0..5)
            .map(|_| {
                let t = Instant::now();
                let r = run_simple(&s, &budget(1 << 20, 5_000), &|| false, &mut |_: &[u8]| {
                    Ok(())
                })
                .unwrap();
                assert!(r.status.success());
                assert_eq!(r.stdout_bytes, 2);
                t.elapsed()
            })
            .min()
            .unwrap();
        assert!(fastest < WATCH_TICK, "{fastest:?}");
    }

    /// すぐ止める形（`run_binary_kill`。SVG の働き手）：SIGTERM を無視する子でも、時間切れ・中止で待たずにグループごと
    /// SIGKILL する（`run_binary` は SIGTERM のあと 2 秒待つ）。
    #[test]
    fn the_kill_variant_stops_at_once_even_when_term_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        let s = script(d.path(), "stubborn", "trap '' TERM; sleep 34.4159");
        let b = Budget {
            max_stdout: 1 << 20,
            stall: Duration::from_secs(30),
            deadline: Duration::from_millis(200),
        };
        let t = Instant::now();
        let r = run_binary_kill(
            &s,
            &[],
            Path::new("/"),
            &b,
            &|| false,
            None,
            &mut |_: &[u8]| Ok(()),
        );
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Deadline))),
            "{r:?}"
        );
        assert!(
            t.elapsed() < Duration::from_millis(1_500),
            "{:?}",
            t.elapsed()
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(gone("sleep 34.4159"));

        // 中止も同じ（見張りの次の刻みで止める）
        let s = script(d.path(), "stubborn2", "trap '' TERM; sleep 35.4159");
        let started = Instant::now();
        let b = Budget {
            deadline: Duration::from_secs(30),
            ..b
        };
        let r = run_binary_kill(
            &s,
            &[],
            Path::new("/"),
            &b,
            &|| started.elapsed() > Duration::from_millis(150),
            None,
            &mut |_: &[u8]| Ok(()),
        );
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Canceled))),
            "{r:?}"
        );
        assert!(started.elapsed() < Duration::from_millis(1_500));
        std::thread::sleep(Duration::from_millis(200));
        assert!(gone("sleep 35.4159"));
    }

    #[test]
    fn a_silent_process_and_its_grandchild_are_stopped() {
        let d = tempfile::tempdir().unwrap();
        let s = script(d.path(), "hang", "sleep 31.4159 & sleep 32.4159");
        let t = Instant::now();
        let r = run_simple(&s, &budget(1 << 20, 300), &|| false, &mut |_: &[u8]| Ok(()));
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Stalled))),
            "{r:?}"
        );
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        std::thread::sleep(Duration::from_millis(200));
        assert!(gone("sleep 31.4159") && gone("sleep 32.4159"));
    }

    #[test]
    fn a_grandchild_holding_the_pipe_after_exit_is_killed() {
        let d = tempfile::tempdir().unwrap();
        // 親はすぐ終わるが、孫が標準出力を持ったまま眠る
        let s = script(
            d.path(),
            "orphan",
            "head -c 10 /dev/zero; (sleep 33.4159) & exit 0",
        );
        let t = Instant::now();
        let r = run_simple(
            &s,
            &budget(1 << 20, 30_000),
            &|| false,
            &mut |_: &[u8]| Ok(()),
        );
        assert!(r.is_ok(), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(8), "{:?}", t.elapsed());
        std::thread::sleep(Duration::from_millis(200));
        assert!(gone("sleep 33.4159"));
    }

    /// プロセスグループの外に出た子孫（fork して setsid した perl）が標準入出力のパイプを持ったまま 30 秒眠る偽の道具。
    /// 子孫が pid のファイルを書いた（外に出た）のを待ってから `after` を動かし、終了コード 0 で終わる（重い環境で起動が
    /// 遅れても、外に出る前に止めることがないように）。pid のファイルの場所も返す。
    fn escaping_tool(dir: &Path, name: &str, after: &str) -> (PathBuf, PathBuf) {
        let pidfile = dir.join(format!("{name}.pid"));
        let p = pidfile.display();
        let body = format!(
            "/usr/bin/perl -MPOSIX -e 'exit 0 if fork; POSIX::setsid(); open(my $f, \">\", \"$ARGV[0].tmp\") or exit 1; print $f $$; close $f; rename(\"$ARGV[0].tmp\", $ARGV[0]); sleep 30' '{p}'\n\
             i=0; while [ ! -s '{p}' ] && [ $i -lt 1000 ]; do sleep 0.01; i=$((i+1)); done\n\
             {after}\n\
             exit 0"
        );
        (script(dir, name, &body), pidfile)
    }

    /// グループの外に残った子孫を片付ける（テストのあとに残らないように）。
    fn kill_escaped(pidfile: &Path) {
        if let Ok(pid) = std::fs::read_to_string(pidfile) {
            let _ = Command::new("kill").args(["-KILL", pid.trim()]).status();
        }
    }

    /// グループに送ったシグナルを数える（送ろうとした数。`drop_kills` 回目までの KILL は送らずに失敗にする）。
    #[derive(Default)]
    struct Signals {
        term: std::sync::atomic::AtomicUsize,
        kill: std::sync::atomic::AtomicUsize,
        drop_kills: usize,
    }

    impl Signals {
        fn send(&self, pid: u32, sig: &str) -> bool {
            let n = match sig {
                "-TERM" => &self.term,
                "-KILL" => &self.kill,
                _ => return false,
            };
            let before = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if sig == "-KILL" && before < self.drop_kills {
                return false;
            }
            signal_group(pid, sig)
        }

        fn counts(&self) -> (usize, usize) {
            let o = std::sync::atomic::Ordering::SeqCst;
            (self.term.load(o), self.kill.load(o))
        }
    }

    /// グループの外に出た子孫がパイプを持ったまま残っても、時間切れの近くで戻る（子が終わったあとも見張る）。グループへの
    /// SIGTERM・SIGKILL はどちらも 1 回だけ。
    #[test]
    fn a_descendant_outside_the_group_does_not_outlive_the_deadline() {
        let d = tempfile::tempdir().unwrap();
        let (s, pidfile) = escaping_tool(d.path(), "escape", "printf 'x'");
        let sig = Signals::default();
        // 外に出るまでの時間（重い環境では 1 秒ほどかかることがある）より長くする
        let deadline = Duration::from_secs(3);
        let t = Instant::now();
        let r = run_binary_with(
            &s,
            &[],
            d.path(),
            &Budget {
                max_stdout: 1 << 20,
                stall: Duration::from_secs(30),
                deadline,
            },
            &|| false,
            None,
            &mut |_: &[u8]| Ok(()),
            &|pid, s| sig.send(pid, s),
        );
        let took = t.elapsed();
        let escaped = pidfile.is_file();
        kill_escaped(&pidfile);
        assert!(escaped, "子孫が外に出る前に時間切れになった");
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Deadline))),
            "{r:?}"
        );
        assert!(
            took >= deadline && took < deadline + Duration::from_millis(1500),
            "{took:?}"
        );
        assert_eq!(sig.counts(), (1, 1));
    }

    /// 子が終わったあとも中止が効く（グループの外の子孫がパイプを持ったまま残っていても、すぐ戻る）。
    #[test]
    fn cancel_works_after_the_child_has_exited() {
        let d = tempfile::tempdir().unwrap();
        let exited = d.path().join("exited");
        let (s, pidfile) = escaping_tool(
            d.path(),
            "escape-cancel",
            &format!("printf 'x'; : > '{}'", exited.display()),
        );
        let sig = Signals::default();
        // 子が終わる直前に書く印を見てから 300 ms で中止する
        let seen: Mutex<Option<Instant>> = Mutex::new(None);
        let canceled = || {
            let mut seen = seen.lock().unwrap();
            if seen.is_none() && exited.is_file() {
                *seen = Some(Instant::now());
            }
            seen.is_some_and(|t| t.elapsed() > Duration::from_millis(300))
        };
        let r = run_binary_with(
            &s,
            &[],
            d.path(),
            &budget(1 << 20, 30_000),
            &canceled,
            None,
            &mut |_: &[u8]| Ok(()),
            &|pid, s| sig.send(pid, s),
        );
        let since = seen.lock().unwrap().map(|t| t.elapsed());
        kill_escaped(&pidfile);
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Canceled))),
            "{r:?}"
        );
        let since = since.unwrap();
        assert!(since < Duration::from_millis(1300), "{since:?}");
        assert_eq!(sig.counts(), (1, 1));
    }

    /// 子が終わったあと、グループの外の子孫がパイプを持ったまま残る（時間切れはまだ）：KILL_GRACE でグループに SIGKILL を
    /// 1 回、さらに KILL_GRACE で見捨てて `Lingering` で戻る。
    #[test]
    fn a_lingering_descendant_is_abandoned_after_the_grace() {
        let d = tempfile::tempdir().unwrap();
        let (s, pidfile) = escaping_tool(d.path(), "linger", "printf 'x'; echo note >&2");
        let sig = Signals::default();
        let t = Instant::now();
        let r = run_binary_with(
            &s,
            &[],
            d.path(),
            &budget(1 << 20, 30_000),
            &|| false,
            None,
            &mut |_: &[u8]| Ok(()),
            &|pid, s| sig.send(pid, s),
        );
        let took = t.elapsed();
        kill_escaped(&pidfile);
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Lingering))),
            "{r:?}"
        );
        // 下限は子が終わってから 2 × KILL_GRACE。上限は外に出るまでの時間（重い環境で 1 秒ほど）を見込む
        assert!(
            took >= 2 * KILL_GRACE && took < 2 * KILL_GRACE + Duration::from_secs(3),
            "{took:?}"
        );
        assert_eq!(sig.counts(), (0, 1));
    }

    /// 標準入力を持ったまま読まない子孫がグループの外に残っても、`feed` の書き込みで止まらず、無出力で戻る。
    #[test]
    fn a_descendant_holding_stdin_does_not_block_the_feed() {
        let d = tempfile::tempdir().unwrap();
        let (s, pidfile) = escaping_tool(d.path(), "stdin-escape", "head -c 10 > /dev/null");
        let sig = Signals::default();
        let data = vec![7u8; 3_000_000];
        let mut feed = |w: &mut dyn std::io::Write| w.write_all(&data);
        // 書き始めにパイプへ入る分も「進んだ」になるので、外に出るまでの時間（重い環境では 1 秒ほど）より長くする
        let stall = Duration::from_secs(3);
        let t = Instant::now();
        let r = run_binary_with(
            &s,
            &[],
            d.path(),
            &budget(1 << 20, 3_000),
            &|| false,
            Some(&mut feed),
            &mut |_: &[u8]| Ok(()),
            &|pid, s| sig.send(pid, s),
        );
        let took = t.elapsed();
        let escaped = pidfile.is_file();
        kill_escaped(&pidfile);
        assert!(escaped, "子孫が外に出る前に止まった");
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Stalled))),
            "{r:?}"
        );
        // 最後に進んだのは head が読んだとき（外に出たあと）。そこから stall で止まり、すぐ戻る
        assert!(
            took >= stall && took < stall + Duration::from_secs(3),
            "{took:?}"
        );
        assert_eq!(sig.counts(), (1, 1));
    }

    /// グループへの KILL を送れなかったら（`kill` を起動できない）、次の刻みで送り直す。TERM を無視する子でも、時間切れから
    /// KILL_GRACE ほどで戻る。
    #[test]
    fn a_kill_that_could_not_be_sent_is_sent_again() {
        let d = tempfile::tempdir().unwrap();
        let s = script(d.path(), "ignore-term", "trap '' TERM\nsleep 30");
        let sig = Signals {
            drop_kills: 1,
            ..Signals::default()
        };
        // sh が trap を実行するまでの時間（重い環境では 1 秒ほどかかることがある）より長くする
        let deadline = Duration::from_secs(3);
        let t = Instant::now();
        let r = run_binary_with(
            &s,
            &[],
            d.path(),
            &Budget {
                max_stdout: 1 << 20,
                stall: Duration::from_secs(30),
                deadline,
            },
            &|| false,
            None,
            &mut |_: &[u8]| Ok(()),
            &|pid, s| sig.send(pid, s),
        );
        let took = t.elapsed();
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Deadline))),
            "{r:?}"
        );
        // 下限は時間切れ ＋ KILL_GRACE。上限は重い環境を見込む（送り直さなければ sleep の 30 秒まで戻らない）
        assert!(
            took >= deadline + KILL_GRACE && took < deadline + KILL_GRACE + Duration::from_secs(5),
            "{took:?}"
        );
        assert_eq!(sig.counts(), (1, 2));
    }

    /// `flush` は、渡した区切りをパイプに書き終えるまで待つ（書き手が取り出しただけでは戻らない）。
    #[test]
    fn flush_waits_until_the_pipe_has_taken_everything() {
        let d = tempfile::tempdir().unwrap();
        // パイプ（macOS で最大 64 KiB）に入りきらない量を、相手が読み始める前に書く
        let s = script(d.path(), "late-reader", "sleep 1\nwc -c");
        let flushed_after = Mutex::new(None);
        let t = Instant::now();
        let mut feed = |w: &mut dyn std::io::Write| {
            w.write_all(&vec![1u8; 200_000])?;
            w.flush()?;
            *flushed_after.lock().unwrap() = Some(t.elapsed());
            Ok(())
        };
        let mut out = Vec::new();
        let r = run_binary(
            &s,
            &[],
            d.path(),
            &budget(1 << 20, 10_000),
            &|| false,
            Some(&mut feed),
            &mut |c: &[u8]| {
                out.extend_from_slice(c);
                Ok(())
            },
        )
        .unwrap();
        assert!(r.status.success());
        assert_eq!(r.stdin_bytes, 200_000);
        assert_eq!(String::from_utf8_lossy(&out).trim(), "200000");
        let flushed_after = flushed_after.lock().unwrap().unwrap();
        assert!(
            flushed_after >= Duration::from_millis(900),
            "{flushed_after:?}"
        );
    }

    /// 受け手が遅くても（子が終わったあと、読み手が受け手に渡すのを KILL_GRACE の 2 倍より長く待っていても）、パイプを
    /// 開いたままの子孫とはみなさない。
    #[test]
    fn a_slow_consumer_after_exit_is_not_lingering() {
        let d = tempfile::tempdir().unwrap();
        // 8 回に分けて書く：受け手が最初の区切りで眠るあいだに渡す列が埋まり、読み手は渡すのを待ったまま子が終わる
        let s = script(
            d.path(),
            "bursts",
            "i=0; while [ $i -lt 8 ]; do printf 'x'; sleep 0.02; i=$((i+1)); done",
        );
        let nap = 2 * KILL_GRACE + Duration::from_secs(3);
        let mut n = 0u64;
        let t = Instant::now();
        let r = run_simple(
            &s,
            &budget(1 << 20, 30_000),
            &|| false,
            &mut |b: &[u8]| {
                if n == 0 {
                    std::thread::sleep(nap);
                }
                n += b.len() as u64;
                Ok(())
            },
        );
        let r = r.unwrap();
        assert!(r.status.success());
        assert_eq!((r.stdout_bytes, n), (8, 8));
        assert!(t.elapsed() >= nap, "{:?}", t.elapsed());
    }

    #[test]
    fn error_lines_are_reported_even_with_exit_zero() {
        let d = tempfile::tempdir().unwrap();
        let s = script(
            d.path(),
            "crc",
            "echo '[in#0/ogg @ 0x1] [error] CRC mismatch!' >&2; echo '[x @ 0x2] [warning] odd' >&2; head -c 80 /dev/zero; exit 0",
        );
        let r = run_simple(&s, &budget(1 << 20, 5_000), &|| false, &mut |_: &[u8]| {
            Ok(())
        })
        .unwrap();
        assert!(r.status.success());
        assert_eq!(r.error_lines.len(), 1);
        assert!(r.error_lines[0].contains("CRC mismatch"));
        assert_eq!(r.warning_lines.len(), 1);
    }

    #[test]
    fn stderr_lines_are_bounded() {
        let d = tempfile::tempdir().unwrap();
        // 改行のない 1 MB の行と、[error] の行を 100 行
        let s = script(
            d.path(),
            "noisy",
            "head -c 1000000 /dev/zero | tr '\\0' x >&2; echo >&2; i=0; while [ $i -lt 100 ]; do echo \"[error] line $i\" >&2; i=$((i+1)); done",
        );
        let r = run_simple(&s, &budget(1 << 20, 5_000), &|| false, &mut |_: &[u8]| {
            Ok(())
        })
        .unwrap();
        assert_eq!(r.error_lines.len(), KEEP_LINES);
        assert_eq!(r.stderr_head.len(), KEEP_LINES);
        assert_eq!(r.stderr_head[0].len(), KEEP_LINE_BYTES);
        assert!(r.stderr_head.iter().all(|l| l.len() <= KEEP_LINE_BYTES));
    }

    #[test]
    fn cancel_consumer_and_deadline_stop_quickly() {
        let d = tempfile::tempdir().unwrap();
        let s = script(
            d.path(),
            "slow",
            "while true; do head -c 4096 /dev/zero; sleep 0.05; done",
        );
        let flag = std::sync::atomic::AtomicBool::new(false);
        let t = Instant::now();
        let r = run_simple(
            &s,
            &budget(1 << 30, 5_000),
            &|| flag.load(std::sync::atomic::Ordering::SeqCst),
            &mut |_: &[u8]| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Canceled))),
            "{r:?}"
        );
        // 単独では 0.5 秒ほど。ワークスペースのテストを並べて回すと数倍に延びるので、閾値は無出力の上限（5 秒）より
        // 大きくしない範囲で広げる（止まらなければ 5 秒の無出力か、それ以上かかる）
        assert!(
            t.elapsed() < Duration::from_millis(4_500),
            "{:?}",
            t.elapsed()
        );
        let r = run_simple(&s, &budget(1 << 30, 5_000), &|| false, &mut |_: &[u8]| {
            Err("もう要らない".into())
        });
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Consumer))),
            "{r:?}"
        );
        let r = run_simple(
            &s,
            &Budget {
                max_stdout: 1 << 30,
                stall: Duration::from_secs(10),
                deadline: Duration::from_millis(300),
            },
            &|| false,
            &mut |_: &[u8]| Ok(()),
        );
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Deadline))),
            "{r:?}"
        );
    }

    #[test]
    fn stdin_is_fed_and_a_non_reader_is_stopped() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out.bin");
        let s = script(d.path(), "cat", &format!("cat > '{}'", out.display()));
        let data = vec![7u8; 3_000_000];
        let mut feed = |w: &mut dyn std::io::Write| w.write_all(&data);
        let r = run_binary(
            &s,
            &[],
            d.path(),
            &budget(1 << 20, 5_000),
            &|| false,
            Some(&mut feed),
            &mut |_: &[u8]| Ok(()),
        )
        .unwrap();
        assert_eq!(r.stdin_bytes, 3_000_000);
        assert!(r.feed_error.is_none());
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 3_000_000);
        // 標準入力を読まない相手
        let s = script(d.path(), "noread", "sleep 34.4159");
        let t = Instant::now();
        let mut feed = |w: &mut dyn std::io::Write| w.write_all(&data);
        let r = run_binary(
            &s,
            &[],
            d.path(),
            &budget(1 << 20, 300),
            &|| false,
            Some(&mut feed),
            &mut |_: &[u8]| Ok(()),
        );
        assert!(
            matches!(r, Err(RunError::Stopped(Stopped::Stalled))),
            "{r:?}"
        );
        assert!(t.elapsed() < Duration::from_secs(5));
        // 先に終わる相手（書き込みの失敗を返し、終了コードは呼び手が見る）
        let s = script(d.path(), "early", "head -c 10 > /dev/null; exit 3");
        let mut feed = |w: &mut dyn std::io::Write| w.write_all(&data);
        let r = run_binary(
            &s,
            &[],
            d.path(),
            &budget(1 << 20, 5_000),
            &|| false,
            Some(&mut feed),
            &mut |_: &[u8]| Ok(()),
        )
        .unwrap();
        assert_eq!(r.status.code(), Some(3));
        assert!(r.feed_error.is_some());
    }

    /// 入力を書く関数が panic しても止まらずに戻り（panic は呼び手に届く）、子も孫も残らない。
    #[test]
    fn a_panicking_feed_does_not_hang_or_leak() {
        let d = tempfile::tempdir().unwrap();
        let s = script(d.path(), "sleeper", "sleep 35.4159 & sleep 36.4159");
        let t = Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut feed = |_: &mut dyn std::io::Write| -> std::io::Result<()> {
                panic!("feed が落ちた");
            };
            run_binary(
                &s,
                &[],
                d.path(),
                &budget(1 << 20, 60_000),
                &|| false,
                Some(&mut feed),
                &mut |_: &[u8]| Ok(()),
            )
        }));
        assert!(r.is_err());
        assert!(t.elapsed() < Duration::from_secs(8), "{:?}", t.elapsed());
        std::thread::sleep(Duration::from_millis(200));
        assert!(gone("sleep 35.4159") && gone("sleep 36.4159"));
    }

    /// 環境変数は消し、作業フォルダで動かす。ないものは起動の失敗。短い問い合わせは出力に上限がある。
    #[test]
    fn environment_is_cleared_and_cwd_is_set() {
        let d = tempfile::tempdir().unwrap();
        let s = script(d.path(), "env", "env; pwd");
        let mut out = Vec::new();
        let r = run_binary(
            &s,
            &[],
            d.path(),
            &budget(1 << 20, 5_000),
            &|| false,
            None,
            &mut |b: &[u8]| {
                out.extend_from_slice(b);
                Ok(())
            },
        )
        .unwrap();
        assert!(r.status.success());
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("PATH=/usr/bin:/bin") && text.contains("LC_ALL=C"),
            "{text}"
        );
        assert!(!text.contains("HOME="), "{text}");
        let real = std::fs::canonicalize(d.path()).unwrap();
        assert!(
            text.lines()
                .any(|l| Path::new(l) == real || Path::new(l) == d.path()),
            "{text}"
        );
        let r = run_simple(
            Path::new("/nonexistent/kura-tool"),
            &budget(1, 1),
            &|| false,
            &mut |_: &[u8]| Ok(()),
        );
        assert!(
            matches!(r, Err(RunError::Spawn(ref e)) if e.kind() == std::io::ErrorKind::NotFound),
            "{r:?}"
        );
        let s = script(d.path(), "hello", "printf 'hello'");
        let (o, bytes) =
            run_capped(&s, &[], d.path(), Duration::from_secs(5), 100, &|| false).unwrap();
        assert!(o.status.success());
        assert_eq!(bytes, b"hello");
        let r = run_capped(&s, &[], d.path(), Duration::from_secs(5), 3, &|| false);
        assert!(matches!(r, Err(RunError::Stopped(Stopped::OutputLimit))));
    }

    /// kura の本番のコード（kura-core・kura-mac・src-tauri の src の、テストだけのモジュールより前）は、子を起動の鍵（[`spawn`]・
    /// [`status`]）を通して起動する（M11 の 3 回目の確認 W1）。`std::process::Command` を使うファイルに `.spawn()`・`.status()`・
    /// `.output()` があれば、鍵の外の起動として断る（このファイルの [`spawn`] の中の 1 か所を除く）。鍵がないと、同時の起動で
    /// パイプの端が子に漏れることは `tests/spawn_lock.rs` が確かめる。
    #[test]
    fn production_code_spawns_through_the_lock() {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for dir in [
            "crates/kura-core/src",
            "crates/kura-mac/src",
            "src-tauri/src",
        ] {
            walk(&root.join(dir), &mut files);
        }
        assert!(files.len() > 50, "{}", files.len());
        let mut found = Vec::new();
        let mut in_tools = 0;
        for path in &files {
            let name = path.file_name().unwrap().to_string_lossy();
            if name.ends_with("tests.rs") || name.ends_with("testing.rs") {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap();
            // テストだけのモジュール（`#[cfg(test)]` を付けた `mod … {`）より前（本番のコード）。注釈は見ない
            let mut code: Vec<&str> = Vec::new();
            let mut test_attr = false;
            for line in text.lines() {
                let l = line.trim();
                if l.starts_with("#[cfg(test)]") || l.starts_with("#[cfg(all(test") {
                    test_attr = true;
                } else if test_attr && l.contains("mod ") && l.ends_with('{') {
                    break;
                } else if !l.starts_with("#[") {
                    test_attr = false;
                }
                code.push(line.split("//").next().unwrap_or(""));
            }
            if !code.iter().any(|l| l.contains("Command")) {
                continue;
            }
            for (i, line) in code.iter().enumerate() {
                for call in [".spawn()", ".status()", ".output()"] {
                    if line.contains(call) {
                        if path.ends_with("kura-core/src/tools.rs") && call == ".spawn()" {
                            in_tools += 1;
                        } else {
                            found.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                        }
                    }
                }
            }
        }
        assert!(found.is_empty(), "起動の鍵の外の起動: {found:#?}");
        assert_eq!(in_tools, 1, "tools.rs の起動は spawn の中の 1 か所だけ");
    }

    #[test]
    fn overrides_come_first_and_versions_parse() {
        let _guard = TEST_TOOLS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("kura-fake-tool");
        std::fs::write(&fake, "#!/bin/sh\necho 'kura-fake-tool 1.2.3'\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        set_overrides(BTreeMap::from([(
            "kura-fake-tool".to_string(),
            fake.clone(),
        )]));
        assert_eq!(find("kura-fake-tool"), Some(fake.clone()));
        let info = inspect("kura-fake-tool");
        assert_eq!(info.version.as_deref(), Some("kura-fake-tool"));
        assert!(info.overridden && !info.homebrew);
        let path = child_path();
        assert!(std::env::split_paths(&path).any(|p| p == dir.path()));
        set_overrides(BTreeMap::new());

        assert_eq!(
            parse_version("ffmpeg", "ffmpeg version 8.0.1 Copyright (c)"),
            Some("8.0.1".into())
        );
        assert_eq!(
            parse_version("deno", "deno 2.9.7 (stable, release)"),
            Some("2.9.7".into())
        );
        assert_eq!(
            parse_version("mpv", "mpv v0.40.0 Copyright"),
            Some("0.40.0".into())
        );
        assert_eq!(
            parse_version("yt-dlp", "2026.08.19"),
            Some("2026.08.19".into())
        );
        assert_eq!(parse_version("yt-dlp", " "), None);
    }
}
