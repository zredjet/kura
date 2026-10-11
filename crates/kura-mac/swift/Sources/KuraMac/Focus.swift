import AppKit
import WebKit

/// フルスクリーンの出入りが終わったら、そのウィンドウの WKWebView にキーボードのフォーカスを戻す。
/// 切り替えが終わったところでフォーカスが WebView から外れ、キーが届かなくなる
/// （シアターで esc が効かない。ADR-128）。アプリ全体で 1 回だけ登録する。
@_cdecl("kura_keep_webview_focus_on_fullscreen")
public func kuraKeepWebviewFocusOnFullscreen() {
    DispatchQueue.main.async {
        for name in [NSWindow.didExitFullScreenNotification, NSWindow.didEnterFullScreenNotification] {
            NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { note in
                guard let window = note.object as? NSWindow,
                      let webview = findWebView(window.contentView)
                else {
                    return
                }
                window.makeFirstResponder(webview)
                // ウィンドウの側（tao）も同じ知らせでウィンドウの形を戻し、そのときフォーカスを置き直すので、
                // 少し待ってからもう一度渡す
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                    if window.firstResponder !== webview {
                        window.makeFirstResponder(webview)
                    }
                }
            }
        }
    }
}

/// ビューの木の中の最初の WKWebView。
private func findWebView(_ view: NSView?) -> WKWebView? {
    guard let view else {
        return nil
    }
    if let webview = view as? WKWebView {
        return webview
    }
    for child in view.subviews {
        if let found = findWebView(child) {
            return found
        }
    }
    return nil
}
