//! 外部ツール（ffmpeg・yt-dlp・gallery-dl など）の場所と実行。GUI アプリはシェルの PATH を引き継がないので、
//! 設定での指定 → Homebrew の場所 → PATH の順に探す。

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;

/// 探す場所（Apple Silicon と Intel の Homebrew）。
pub const SEARCH_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// 状態を表示するツール（docs/architecture.md §9。deno は yt-dlp が YouTube で使う）。
pub const KNOWN_TOOLS: [&str; 6] = ["yt-dlp", "gallery-dl", "deno", "ffmpeg", "ffprobe", "mpv"];

/// ツールの場所の指定を変えるテストを順に動かすための錠（指定はアプリ全体で 1 つ）。
#[cfg(test)]
pub(crate) static TEST_TOOLS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// ツールを探す（設定での指定 → Homebrew の場所 → PATH の順）。見つからなければ None。
pub fn find(name: &str) -> Option<PathBuf> {
    if let Some(path) = override_for(name).filter(|p| is_executable(p)) {
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
        // 「ffmpeg version 8.0.1 Copyright …」
        "ffmpeg" | "ffprobe" => words.get(2).copied(),
        // 「deno 2.9.7 (stable, …)」「mpv v0.40.0 Copyright …」
        "deno" | "mpv" => words.get(1).copied(),
        _ => words.first().copied(),
    }?;
    Some(version.trim_start_matches('v').to_string())
}

/// 外部コマンドを実行し、`timeout` を過ぎたら止める。標準出力・標準エラー出力を返す。
pub fn run_with_timeout(command: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
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
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 子プロセス（yt-dlp が呼ぶ ffmpeg など）もまとめて止められるように、新しいグループにする
        .process_group(0)
        .spawn()?;
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

/// プロセスグループに SIGTERM を送り、5 秒待っても終わらなければ SIGKILL にする。
fn terminate_group(child: &mut Child) {
    let group = format!("-{}", child.id());
    let signal = |sig: &str| {
        let _ = Command::new("kill")
            .args([sig, "--", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    signal("-TERM");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    signal("-KILL");
    let _ = child.kill();
    let _ = child.wait();
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
