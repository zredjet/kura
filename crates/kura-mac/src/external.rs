//! 外部プレイヤーで開く（docs/architecture.md §8：プロキシ作成中・失敗時の逃げ道）。
//! mpv があれば mpv、なければ QuickTime Player。ライブラリのファイルを読むだけで、変更はしない。

use std::path::Path;
use std::process::{Command, Stdio};

use kura_core::tools::find;

/// 開いたプレイヤー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalPlayer {
    Mpv,
    QuickTime,
}

/// `path` を外部プレイヤーで開く（待たずに戻る）。
pub fn open_in_player(path: &Path) -> Result<ExternalPlayer, String> {
    if let Some(mpv) = find("mpv") {
        let mut child = Command::new(mpv)
            .arg("--force-window=immediate")
            .arg("--")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("mpv を起動できません: {e}"))?;
        // 終わったプロセスを片付ける
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Ok(ExternalPlayer::Mpv);
    }
    open_with_quicktime(path)
}

#[cfg(target_os = "macos")]
fn open_with_quicktime(path: &Path) -> Result<ExternalPlayer, String> {
    let status = Command::new("/usr/bin/open")
        .args(["-a", "QuickTime Player", "--"])
        .arg(path)
        .status()
        .map_err(|e| format!("QuickTime Player を開けません: {e}"))?;
    if status.success() {
        Ok(ExternalPlayer::QuickTime)
    } else {
        Err("QuickTime Player で開けませんでした".into())
    }
}

#[cfg(not(target_os = "macos"))]
fn open_with_quicktime(_path: &Path) -> Result<ExternalPlayer, String> {
    Err("mpv が見つかりません".into())
}
