//! キャプチャで使う macOS の機能：Google Chrome の場所と、WKWebView の表示範囲の撮影（見たままキャプチャ）。

use std::path::{Path, PathBuf};

/// Google Chrome の実行ファイル（/Applications か ~/Applications）。
pub fn find_chrome() -> Option<PathBuf> {
    const APP: &str = "Google Chrome.app/Contents/MacOS/Google Chrome";
    let mut candidates = vec![PathBuf::from("/Applications").join(APP)];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications").join(APP));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// 撮った画像の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub title: String,
    pub url: String,
}

/// 撮り終えたときに呼ぶもの。
pub type SnapshotDone = Box<dyn FnOnce(Result<SnapshotInfo, String>) + Send>;

/// WKWebView（`webview` はそのポインタ）の表示範囲を撮って PNG で `out` に書く。
/// メインスレッドから呼ぶこと（Tauri の `with_webview` の中）。撮影は非同期で、終わると `done` を呼ぶ。
///
/// # Safety
/// `webview` は生きている WKWebView を指していること。
#[cfg(target_os = "macos")]
pub unsafe fn snapshot_webview(webview: *mut std::ffi::c_void, out: &Path, done: SnapshotDone) {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn kura_snapshot_webview(
            webview: *mut c_void,
            path: *const c_char,
            done: extern "C" fn(*mut c_void, bool, *const c_char, *const c_char),
            context: *mut c_void,
        );
    }

    extern "C" fn finished(
        context: *mut c_void,
        ok: bool,
        title: *const c_char,
        url: *const c_char,
    ) {
        if context.is_null() {
            return;
        }
        // SAFETY: `context` は下で Box::into_raw した `SnapshotDone` で、ここで一度だけ戻す
        let done: Box<SnapshotDone> = unsafe { Box::from_raw(context.cast::<SnapshotDone>()) };
        let text = |p: *const c_char| {
            if p.is_null() {
                String::new()
            } else {
                // SAFETY: Swift が呼び出しのあいだ有効な C 文字列を渡す
                unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
            }
        };
        if ok {
            done(Ok(SnapshotInfo {
                title: text(title),
                url: text(url),
            }));
        } else {
            done(Err("ページを撮れませんでした".into()));
        }
    }

    let Ok(path) = CString::new(out.as_os_str().as_bytes()) else {
        done(Err("保存先のパスが正しくありません".into()));
        return;
    };
    let context = Box::into_raw(Box::new(done)).cast::<c_void>();
    // SAFETY: webview は呼び出し側が保証する。path は呼び出しのあいだ有効（Swift はすぐ String にする）
    unsafe { kura_snapshot_webview(webview, path.as_ptr(), finished, context) };
}

/// macOS 以外では撮れない。
///
/// # Safety
/// 何もしない（macOS 版と同じ形にするため unsafe）。
#[cfg(not(target_os = "macos"))]
pub unsafe fn snapshot_webview(_webview: *mut std::ffi::c_void, _out: &Path, done: SnapshotDone) {
    done(Err("この OS では撮れません".into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_chrome_when_installed() {
        if let Some(path) = find_chrome() {
            assert!(path.ends_with("Google Chrome"));
        }
    }
}
