//! Finder で見せる・ボリュームの容量（書き出し・バックアップ・サイドバーの容量）と、
//! フルスクリーンの出入りのあとの WebView のフォーカス。

use std::path::Path;

/// Finder で選んだ状態で見せる。見せられなければ false（ないパス・macOS 以外）。
pub fn reveal_in_finder(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::{CString, c_char};
        use std::os::unix::ffi::OsStrExt;

        unsafe extern "C" {
            fn kura_reveal_in_finder(path: *const c_char) -> bool;
        }
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: NUL 終端の文字列を渡すだけ（Swift 側でコピーする）
        unsafe { kura_reveal_in_finder(path.as_ptr()) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

/// フルスクリーンの出入りが終わったら、WebView にキーボードのフォーカスを戻すようにする（アプリの起動時に 1 回。ADR-128）。
pub fn keep_webview_focus_on_fullscreen() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn kura_keep_webview_focus_on_fullscreen();
        }
        // SAFETY: 引数のない C 関数。通知の受け取りをメインスレッドで登録するだけ
        unsafe { kura_keep_webview_focus_on_fullscreen() };
    }
}

/// ボリュームの容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeCapacity {
    pub total_bytes: u64,
    /// 使える空き（Finder の「使用可能」）。
    pub available_bytes: u64,
}

/// `path` のあるボリュームの容量（読めなければ None）。
pub fn volume_capacity(path: &Path) -> Option<VolumeCapacity> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::{CString, c_char};
        use std::os::unix::ffi::OsStrExt;

        unsafe extern "C" {
            fn kura_volume_capacity(
                path: *const c_char,
                total: *mut i64,
                available: *mut i64,
            ) -> bool;
        }
        let path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let (mut total, mut available) = (0i64, 0i64);
        // SAFETY: NUL 終端の文字列と、書き込み先の i64 を 2 つ渡す
        let ok = unsafe { kura_volume_capacity(path.as_ptr(), &mut total, &mut available) };
        ok.then(|| VolumeCapacity {
            total_bytes: u64::try_from(total).unwrap_or(0),
            available_bytes: u64::try_from(available).unwrap_or(0),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_capacity_of_the_temp_volume() {
        let dir = std::env::temp_dir();
        if cfg!(target_os = "macos") {
            let cap = volume_capacity(&dir).unwrap();
            assert!(cap.total_bytes > 0 && cap.available_bytes <= cap.total_bytes);
        }
        assert!(volume_capacity(Path::new("/ない/パス")).is_none());
    }

    #[test]
    fn reveal_refuses_missing_paths() {
        assert!(!reveal_in_finder(Path::new("/ない/パス")));
    }
}
