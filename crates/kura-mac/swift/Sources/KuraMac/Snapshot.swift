import AppKit
import WebKit

/// 撮り終えたときに呼ぶ関数（成功したか・題名・URL。文字列は呼び出しのあいだだけ有効）。
public typealias KuraSnapshotDone = @convention(c) (
    UnsafeMutableRawPointer?, Bool, UnsafePointer<CChar>?, UnsafePointer<CChar>?
) -> Void

/// WKWebView の表示範囲を撮って PNG で `path` に書く（見たままキャプチャ。docs/architecture.md §10）。
/// メインスレッドから呼ぶ。撮影は非同期で、終わると `done(context, ok, title, url)` を呼ぶ。
@_cdecl("kura_snapshot_webview")
public func kuraSnapshotWebview(
    _ webview: UnsafeMutableRawPointer,
    _ path: UnsafePointer<CChar>,
    _ done: KuraSnapshotDone,
    _ context: UnsafeMutableRawPointer?
) {
    let view = Unmanaged<WKWebView>.fromOpaque(webview).takeUnretainedValue()
    let out = URL(fileURLWithPath: String(cString: path))
    let title = view.title ?? ""
    let url = view.url?.absoluteString ?? ""
    let config = WKSnapshotConfiguration()
    // 表示範囲を画面の解像度のまま
    config.afterScreenUpdates = true
    view.takeSnapshot(with: config) { image, _ in
        var ok = false
        if let image,
            let tiff = image.tiffRepresentation,
            let rep = NSBitmapImageRep(data: tiff),
            let png = rep.representation(using: .png, properties: [:])
        {
            ok = (try? png.write(to: out, options: .atomic)) != nil
        }
        title.withCString { t in
            url.withCString { u in
                done(context, ok, t, u)
            }
        }
    }
}
