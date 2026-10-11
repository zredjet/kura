//! Chrome の起動（キャプチャ用のヘッドレスと、ログイン用の画面あり）。
//!
//! どちらもアプリ専用のプロファイル（`capture-profile`）を使い、ふだんの Chrome のプロファイルには触れない。
//! サンドボックスは切らない。Cookie の暗号化（キーチェーン）も Chrome の既定のまま。
//! DevTools は `--remote-debugging-port=0`（127.0.0.1 の空いているポート）で、撮影のあいだだけ開く。

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::tools::terminate_group;
use crate::{CoreError, Result};

/// 外部ツールの欄・場所の指定に使う名前。
pub const CHROME_TOOL: &str = "chrome";

/// 両方に付ける引数（初回の案内・同期・拡張機能の更新など、撮影に要らない通信を止める）。
fn common_args(profile: &Path) -> Vec<OsString> {
    let mut dir = OsString::from("--user-data-dir=");
    dir.push(profile.as_os_str());
    vec![
        dir,
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-sync".into(),
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-default-apps".into(),
    ]
}

/// キャプチャ用（ヘッドレス・DevTools は 127.0.0.1 の空いているポート）の引数。
pub fn headless_args(profile: &Path) -> Vec<OsString> {
    let mut args = common_args(profile);
    args.extend(
        [
            "--headless",
            "--remote-debugging-port=0",
            "--remote-debugging-address=127.0.0.1",
            "--hide-scrollbars",
            "--mute-audio",
            "about:blank",
        ]
        .map(OsString::from),
    );
    args
}

/// ログイン用（画面あり。DevTools は開かない）の引数。
pub fn login_args(profile: &Path, url: Option<&str>) -> Vec<OsString> {
    let mut args = common_args(profile);
    if let Some(url) = url {
        args.push("--".into());
        args.push(url.into());
    }
    args
}

/// プロファイルをほかの Chrome（ログイン用に開いたものなど）が使っているか。
/// Chrome はプロファイルに `SingletonLock`（`<ホスト名>-<pid>` へのシンボリックリンク）を置く。
pub fn profile_in_use(profile: &Path) -> bool {
    let Ok(target) = std::fs::read_link(profile.join("SingletonLock")) else {
        return false;
    };
    let target = target.to_string_lossy();
    let Some(pid) = target
        .rsplit('-')
        .next()
        .and_then(|p| p.parse::<u32>().ok())
    else {
        return false;
    };
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// 動いている Chrome。手放すとプロセスグループごと止める。
pub struct ChromeProcess {
    child: Child,
    /// 標準エラー出力の最後の数行（起動に失敗したときの説明に使う）。
    stderr: Arc<Mutex<Vec<String>>>,
}

impl ChromeProcess {
    /// キャプチャ用に起動する（古い `DevToolsActivePort` は先に消す）。
    pub fn launch_headless(chrome: &Path, profile: &Path) -> Result<Self> {
        std::fs::create_dir_all(profile)?;
        let _ = std::fs::remove_file(profile.join("DevToolsActivePort"));
        let mut child = Command::new(chrome)
            .args(headless_args(profile))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| CoreError::Tool(format!("Chrome を起動できませんでした: {e}")))?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        if let Some(pipe) = child.stderr.take() {
            let lines = stderr.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    if let Ok(mut lines) = lines.lock() {
                        lines.push(line);
                        let excess = lines.len().saturating_sub(20);
                        lines.drain(..excess);
                    }
                }
            });
        }
        Ok(Self { child, stderr })
    }

    /// まだ動いているか。
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 標準エラー出力の最後の数行。
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Drop for ChromeProcess {
    fn drop(&mut self) {
        terminate_group(&mut self.child);
    }
}

/// ログイン用に画面ありで開く（終わるのは待たない）。
pub fn open_for_login(chrome: &Path, profile: &Path, url: Option<&str>) -> Result<()> {
    std::fs::create_dir_all(profile)?;
    Command::new(chrome)
        .args(login_args(profile, url))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| CoreError::Tool(format!("Chrome を起動できませんでした: {e}")))?;
    Ok(())
}

/// アプリ専用のプロファイルの場所（アプリの設定のフォルダの中）。
pub fn profile_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("capture-profile")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn headless_uses_the_app_profile_and_localhost() {
        let args = strings(&headless_args(Path::new("/app/capture-profile")));
        assert!(args.contains(&"--user-data-dir=/app/capture-profile".to_string()));
        assert!(args.contains(&"--headless".to_string()));
        assert!(args.contains(&"--remote-debugging-port=0".to_string()));
        assert!(args.contains(&"--remote-debugging-address=127.0.0.1".to_string()));
        // 安全のための既定は変えない
        for weak in [
            "--no-sandbox",
            "--disable-web-security",
            "--use-mock-keychain",
            "--remote-allow-origins",
        ] {
            assert!(!args.iter().any(|a| a.starts_with(weak)), "{weak}");
        }
        let login = strings(&login_args(Path::new("/p"), Some("https://x.test/login")));
        assert!(!login.iter().any(|a| a.contains("remote-debugging")));
        assert_eq!(&login[login.len() - 2..], ["--", "https://x.test/login"]);
    }

    #[test]
    fn reads_the_profile_lock() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!profile_in_use(dir.path()));
        // 動いているプロセス（このテスト）を指すロック
        std::os::unix::fs::symlink(
            format!("host-{}", std::process::id()),
            dir.path().join("SingletonLock"),
        )
        .unwrap();
        assert!(profile_in_use(dir.path()));
        std::fs::remove_file(dir.path().join("SingletonLock")).unwrap();
        // 終わったプロセスを指すロックは使っていない扱い
        std::os::unix::fs::symlink("host-999999999", dir.path().join("SingletonLock")).unwrap();
        assert!(!profile_in_use(dir.path()));
    }
}
