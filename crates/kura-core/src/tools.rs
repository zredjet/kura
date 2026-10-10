//! 外部ツール（ffmpeg など）の場所。GUI アプリはシェルの PATH を引き継がないので、Homebrew の場所も探す。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// 探す場所（Apple Silicon と Intel の Homebrew）。設定での上書きは M7。
pub const SEARCH_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// ツールを探す（Homebrew の場所 → PATH の順）。見つからなければ None。
pub fn find(name: &str) -> Option<PathBuf> {
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
}
