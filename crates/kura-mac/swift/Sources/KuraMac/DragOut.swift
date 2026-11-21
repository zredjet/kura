import AppKit
import UniformTypeIdentifiers
import WebKit

/// 落とす先が項目 `index` のデータを求めたときに呼ぶ関数（回の番号・項目の番号）。できるまで待ち、渡してよければ true。
/// メインスレッドで呼ぶ。
public typealias KuraDragWait = @convention(c) (UInt64, UInt32) -> Bool

/// ドラッグが終わったときに呼ぶ関数（回の番号・落ちたか）。メインスレッドで、回ごとにちょうど 1 回呼ぶ。
public typealias KuraDragEnded = @convention(c) (UInt64, Bool) -> Void

/// 渡す 1 項目（Rust から JSON で受け取る）。
private struct KuraDragFile: Decodable {
    /// 渡すパス（絶対）。
    let path: String
    /// フォルダか（ユニット）。
    let folder: Bool
    /// ドラッグの見た目に使うサムネイル（なければ種類のアイコン）。
    let image: String?
}

/// 1 項目のファイルの URL を、求められたときに渡す（作り終えるまで待つ）。
private final class KuraDragProvider: NSObject, NSPasteboardItemDataProvider {
    let session: UInt64
    let index: UInt32
    let url: URL
    let wait: KuraDragWait

    init(session: UInt64, index: UInt32, url: URL, wait: @escaping KuraDragWait) {
        self.session = session
        self.index = index
        self.url = url
        self.wait = wait
    }

    func pasteboard(
        _ pasteboard: NSPasteboard?, item: NSPasteboardItem,
        provideDataForType type: NSPasteboard.PasteboardType
    ) {
        guard type == .fileURL else {
            return
        }
        // Kura 自身の窓の上で求められたら（Tauri のファイルドロップの受け手）渡さず、待たない（取り込まない。
        // 作り終えるのを待ってメインスレッドを止めない）
        let under = NSWindow.windowNumber(at: NSEvent.mouseLocation, belowWindowWithWindowNumber: 0)
        if NSApp.windows.contains(where: { $0.windowNumber == under }) {
            return
        }
        if wait(session, index) {
            item.setString(url.absoluteString, forType: .fileURL)
        }
    }
}

/// OS のドラッグの元。アプリの外へだけコピーで渡す（Kura 自身には落とせない）。
private final class KuraDragSource: NSObject, NSDraggingSource {
    let session: UInt64
    let ended: KuraDragEnded
    /// 求められるまで項目の data provider を持っておく（落としたあとに求める先があっても答えられるように）。
    let providers: [KuraDragProvider]
    /// ドラッグを始めた WebView（終わったらボタンを離したことを渡す）。
    weak var view: NSView?
    var finishedAt: Date?

    init(
        session: UInt64, ended: @escaping KuraDragEnded, providers: [KuraDragProvider], view: NSView
    ) {
        self.session = session
        self.ended = ended
        self.providers = providers
        self.view = view
    }

    func draggingSession(
        _ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext
    ) -> NSDragOperation {
        context == .outsideApplication ? .copy : []
    }

    // ⌥・⌘ で移動やリンクにしない（元はライブラリの一時置き場の写し）
    func ignoreModifierKeys(for session: NSDraggingSession) -> Bool {
        true
    }

    func draggingSession(
        _ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation
    ) {
        guard finishedAt == nil else {
            return
        }
        finishedAt = Date()
        // WebView はページの中のドラッグの途中で OS のドラッグに引き継がれたので、ボタンを離したのを知らない（押したままと
        // 思って次のクリックを取り違える）。離したところで離したことを渡して、押したままの状態を終える
        if let view, let window = view.window {
            let point = window.convertPoint(fromScreen: screenPoint)
            if let up = NSEvent.mouseEvent(
                with: .leftMouseUp,
                location: point,
                modifierFlags: [],
                timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: window.windowNumber,
                context: nil,
                eventNumber: 0,
                clickCount: 1,
                pressure: 0
            ) {
                view.mouseUp(with: up)
            }
        }
        ended(self.session, !operation.isEmpty)
    }
}

/// 動いている・終わったばかりのドラッグの元（終わってから 15 分は持つ。一時置き場を消す 10 分より長く）。
private var kuraDragSources: [KuraDragSource] = []

/// ドラッグの見た目の 1 辺（pt）。
private let kuraDragImageSide: CGFloat = 64

/// 見た目に使う画像：サムネイルか、なければ種類のアイコン（フォルダはフォルダのアイコン）。
private func kuraDragImage(_ file: KuraDragFile) -> NSImage {
    if let path = file.image, let image = NSImage(contentsOfFile: path), image.size.width > 0,
        image.size.height > 0
    {
        return image
    }
    if file.folder {
        return NSWorkspace.shared.icon(for: .folder)
    }
    let ext = (file.path as NSString).pathExtension
    return NSWorkspace.shared.icon(for: UTType(filenameExtension: ext) ?? .data)
}

/// 画像を 1 辺の箱に収める枠（縦横比を保つ）。
private func kuraFit(_ size: NSSize, at center: NSPoint) -> NSRect {
    let scale = min(kuraDragImageSide / max(size.width, 1), kuraDragImageSide / max(size.height, 1))
    let w = max(size.width * scale, 1)
    let h = max(size.height * scale, 1)
    return NSRect(x: center.x - w / 2, y: center.y - h / 2, width: w, height: h)
}

/// WKWebView（`webview`）から OS のドラッグを始める（M15 段 5。ADR-207）。`files` は渡す項目の JSON の配列
/// （`[{path, folder, image}]`）。マウスのボタンが離れていれば始めずに false（そのときは `wait`・`ended` を呼ばない）。
/// 始めたら、落とす先が求めた項目ごとに `wait(session, index)` を呼んでファイルの URL を渡し、終わると `ended` を 1 回呼ぶ。
/// メインスレッドから呼ぶ。
@_cdecl("kura_dragout_begin")
public func kuraDragoutBegin(
    _ webview: UnsafeMutableRawPointer,
    _ session: UInt64,
    _ files: UnsafePointer<CChar>,
    _ wait: KuraDragWait,
    _ ended: KuraDragEnded
) -> Bool {
    let view = Unmanaged<WKWebView>.fromOpaque(webview).takeUnretainedValue()
    guard let window = view.window,
        NSEvent.pressedMouseButtons & 1 != 0,
        let data = String(cString: files).data(using: .utf8),
        let list = try? JSONDecoder().decode([KuraDragFile].self, from: data),
        !list.isEmpty,
        list.count <= Int(UInt32.max)
    else {
        return false
    }
    // 終わってから時間のたった元を手放す
    let now = Date()
    kuraDragSources.removeAll { source in
        source.finishedAt.map { now.timeIntervalSince($0) > 15 * 60 } ?? false
    }

    let mouse = view.convert(window.mouseLocationOutsideOfEventStream, from: nil)
    var providers: [KuraDragProvider] = []
    var items: [NSDraggingItem] = []
    for (k, file) in list.enumerated() {
        let url = URL(fileURLWithPath: file.path, isDirectory: file.folder)
        let provider = KuraDragProvider(
            session: session, index: UInt32(k), url: url, wait: wait)
        let item = NSPasteboardItem()
        guard item.setDataProvider(provider, forTypes: [.fileURL]) else {
            return false
        }
        providers.append(provider)
        let dragging = NSDraggingItem(pasteboardWriter: item)
        // 重ねて少しずつずらす（多いときは AppKit が数を添える）
        let offset = CGFloat(min(k, 4)) * 6
        let image = k < 20 ? kuraDragImage(file) : NSWorkspace.shared.icon(for: .data)
        let center = NSPoint(x: mouse.x + offset, y: mouse.y - offset)
        dragging.setDraggingFrame(kuraFit(image.size, at: center), contents: image)
        items.append(dragging)
    }
    // ボタンを押したまま動いている今の位置で、ドラッグの出来事を作る（ページの中のドラッグから引き継ぐ）
    guard
        let event = NSEvent.mouseEvent(
            with: .leftMouseDragged,
            location: window.mouseLocationOutsideOfEventStream,
            modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber,
            context: nil,
            eventNumber: 0,
            clickCount: 1,
            pressure: 1
        )
    else {
        return false
    }
    let source = KuraDragSource(session: session, ended: ended, providers: providers, view: view)
    kuraDragSources.append(source)
    let dragging = view.beginDraggingSession(with: items, event: event, source: source)
    dragging.animatesToStartingPositionsOnCancelOrFail = false
    dragging.draggingFormation = .pile
    return true
}
