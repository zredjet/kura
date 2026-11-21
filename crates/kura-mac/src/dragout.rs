//! ドラッグアウト（M15 段 5。ADR-207）：WKWebView から OS のドラッグを始め、落とす先が求めたらファイルの URL を渡す
//! （Swift の `kura_dragout_begin`。項目ごとの data provider がファイルを作り終えるまで待つ）。
//!
//! 回の受け手（[`DragHandler`]）は回の番号で引く表に置き、Swift からの呼び出し（メインスレッド）はその番号で受け手を引く
//! （ポインタを渡さないので、終わったあとに求められても受け手の寿命に当たらない）。片付けた受け手は次のドラッグで表から外す。

use std::path::PathBuf;
use std::sync::Arc;

/// 渡す 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragFile {
    /// 渡すパス（絶対）。
    pub path: PathBuf,
    /// フォルダか（ユニット）。
    pub folder: bool,
    /// ドラッグの見た目に使うサムネイル（なければ種類のアイコン）。
    pub image: Option<PathBuf>,
}

/// 1 回のドラッグの受け手（メインスレッドから呼ばれる）。
pub trait DragHandler: Send + Sync {
    /// 落とす先が項目 `index` のデータを求めた：できるまで待ち、渡してよければ true。
    fn wait(&self, index: usize) -> bool;
    /// ドラッグが終わった（`dropped` は落とす先が受けたと AppKit が言ったか。受けても false を返す先がある）。
    fn ended(&self, dropped: bool);
    /// 片付けた（もう答えない。表から外してよい）。
    fn retired(&self) -> bool;
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::BTreeMap;
    use std::ffi::{CString, c_char, c_void};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use super::{DragFile, DragHandler};

    static NEXT: AtomicU64 = AtomicU64::new(1);
    static HANDLERS: Mutex<BTreeMap<u64, Arc<dyn DragHandler>>> = Mutex::new(BTreeMap::new());

    fn handler(id: u64) -> Option<Arc<dyn DragHandler>> {
        HANDLERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
    }

    unsafe extern "C" {
        fn kura_dragout_begin(
            webview: *mut c_void,
            session: u64,
            files: *const c_char,
            wait: extern "C" fn(u64, u32) -> bool,
            ended: extern "C" fn(u64, bool),
        ) -> bool;
    }

    extern "C" fn wait(id: u64, index: u32) -> bool {
        // Swift の呼び出しへ panic を越えさせない
        std::panic::catch_unwind(|| {
            let index = usize::try_from(index).unwrap_or(usize::MAX);
            handler(id).is_some_and(|h| h.wait(index))
        })
        .unwrap_or(false)
    }

    extern "C" fn ended(id: u64, dropped: bool) {
        let _ = std::panic::catch_unwind(|| {
            if let Some(h) = handler(id) {
                h.ended(dropped);
            }
        });
    }

    #[derive(serde::Serialize)]
    struct FileJson<'a> {
        path: &'a str,
        folder: bool,
        image: Option<&'a str>,
    }

    pub(super) unsafe fn begin(
        webview: *mut c_void,
        files: &[DragFile],
        handler: Arc<dyn DragHandler>,
    ) -> bool {
        let list: Option<Vec<FileJson<'_>>> = files
            .iter()
            .map(|f| {
                Some(FileJson {
                    path: f.path.to_str()?,
                    folder: f.folder,
                    image: f.image.as_deref().and_then(|p| p.to_str()),
                })
            })
            .collect();
        let Some(json) = list
            .and_then(|l| serde_json::to_string(&l).ok())
            .and_then(|j| CString::new(j).ok())
        else {
            return false;
        };
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        {
            let mut table = HANDLERS.lock().unwrap_or_else(PoisonError::into_inner);
            table.retain(|_, h| !h.retired());
            table.insert(id, handler);
        }
        // SAFETY: webview は呼び出し側が保証する。json は呼び出しのあいだ有効（Swift はすぐ String にする）
        let started = unsafe { kura_dragout_begin(webview, id, json.as_ptr(), wait, ended) };
        if !started {
            HANDLERS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
        }
        started
    }
}

/// WKWebView（`webview` はそのポインタ）から OS のドラッグを始める。マウスのボタンが離れていれば始めずに false
/// （そのときは `handler` を呼ばない）。始めたら、落とす先が求めた項目ごとに `handler.wait`、終わると `handler.ended` を
/// 1 回呼ぶ（どれもメインスレッド）。メインスレッドから呼ぶこと（Tauri の `with_webview` の中）。
///
/// # Safety
/// `webview` は生きている WKWebView を指していること。
#[cfg(target_os = "macos")]
pub unsafe fn begin_drag(
    webview: *mut std::ffi::c_void,
    files: &[DragFile],
    handler: Arc<dyn DragHandler>,
) -> bool {
    // SAFETY: 呼び出し側が保証する
    unsafe { imp::begin(webview, files, handler) }
}

/// macOS 以外では始められない。
///
/// # Safety
/// 何もしない（macOS 版と同じ形にするため unsafe）。
#[cfg(not(target_os = "macos"))]
pub unsafe fn begin_drag(
    _webview: *mut std::ffi::c_void,
    _files: &[DragFile],
    _handler: Arc<dyn DragHandler>,
) -> bool {
    false
}
