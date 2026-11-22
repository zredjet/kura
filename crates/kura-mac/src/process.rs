//! プロセスの実行ファイルの今の場所（SVG の働き手を起動するたびに求める。M11 の 3 回目の確認 W2）。
//!
//! - `std::env::current_exe` は macOS では起動したときのパス（`_NSGetExecutablePath`）を返し、動いているアプリを移す・改名すると
//!   古い場所を指したままになる。ここは libc の `proc_pidpath`（カーネルが今の実行ファイルの vnode から作るパス）で、改名・
//!   移動を追う。実行ファイルを消された・置き換えられた（元のファイルがない）ときはエラーになりうる。
//! - unsafe は、このファイルの `proc_pidpath` の呼び出しだけ。

use std::io;
use std::path::PathBuf;

/// 自分のプロセスの今の実行ファイルのパス（改名・移動を追う）。macOS 以外は `std::env::current_exe`。
pub fn current_executable() -> io::Result<PathBuf> {
    executable_path(std::process::id())
}

/// プロセス `pid` の今の実行ファイルのパス（改名・移動を追う）。macOS 以外は自分のプロセスだけ（`std::env::current_exe`）。
#[cfg(target_os = "macos")]
pub fn executable_path(pid: u32) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let pid = libc::c_int::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid が大きすぎます"))?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let cap = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: buf は cap バイトの書き込める領域で、proc_pidpath はそこに最大 cap バイトだけ書き、書いた長さ（NUL を除く）を
    // 返す（失敗は 0 以下と errno）。呼び出しのあいだ buf は生きていて、ほかから触られない。
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), cap) };
    if len <= 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(usize::try_from(len).unwrap_or(0).min(buf.len()));
    if buf.is_empty() {
        return Err(io::Error::other("実行ファイルのパスが空です"));
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

/// プロセス `pid` の今の実行ファイルのパス（macOS 以外は自分のプロセスだけ）。
#[cfg(not(target_os = "macos"))]
pub fn executable_path(pid: u32) -> io::Result<PathBuf> {
    if pid == std::process::id() {
        std::env::current_exe()
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ほかのプロセスの実行ファイルは macOS でだけ求められます",
        ))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// 自分の実行ファイルは、起動したときの実行ファイルと同じファイル。
    #[test]
    fn current_executable_is_this_test_binary() {
        let now = current_executable().unwrap();
        let started = std::env::current_exe().unwrap();
        assert_eq!(
            std::fs::canonicalize(now).unwrap(),
            std::fs::canonicalize(started).unwrap()
        );
        assert!(executable_path(u32::MAX).is_err());
    }

    /// 長く生きるだけのテスト（`a_renamed_folder_is_followed` が、このテストの実行ファイルの写しで子として起動する）。
    #[test]
    #[ignore = "a_renamed_folder_is_followed が子として起動する"]
    fn sleeper() {
        std::thread::sleep(Duration::from_secs(30));
    }

    /// 動いているプロセスのフォルダを改名すると、新しい場所を返す（`std::env::current_exe` は起動したときのパスのまま）。
    /// 子は、このテストの実行ファイルの写しで `sleeper` だけを動かす（OS の実行ファイルの写しは署名で止められうる）。
    #[test]
    fn a_renamed_folder_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir(&a).unwrap();
        let exe = a.join("kura-mac-test");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let mut child = Command::new(&exe)
            .args([
                "--ignored",
                "--exact",
                "process::tests::sleeper",
                "--test-threads=1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let canonical = |p: PathBuf| std::fs::canonicalize(p).unwrap();
        // 起動の直後は exec が終わるまで前のパスを返しうるので、新しい実行ファイルになるまで待つ
        let started = Instant::now();
        while executable_path(pid).map(canonical).ok() != Some(canonical(exe.clone())) {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        let b = dir.path().join("b");
        std::fs::rename(&a, &b).unwrap();
        let moved = executable_path(pid);
        let alive = matches!(child.try_wait(), Ok(None));
        let _ = child.kill();
        let _ = child.wait();
        assert!(alive, "子が動いていない");
        assert_eq!(
            canonical(moved.unwrap()),
            canonical(b.join("kura-mac-test"))
        );
    }
}
