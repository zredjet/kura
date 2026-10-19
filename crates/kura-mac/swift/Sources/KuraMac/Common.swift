import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// C 関数の戻り値。
let kOK: Int32 = 0
let kUnsupported: Int32 = 1
let kFailed: Int32 = 2
let kNoArtwork: Int32 = 3
/// PNG を書いた（サムネイルの関数。kOK は JPEG を書いた）。
let kPNG: Int32 = 4

/// JPEG の品質（docs/architecture.md §7）。
let jpegQuality = 0.82

func fileURL(_ path: UnsafePointer<CChar>) -> URL {
    URL(fileURLWithPath: String(cString: path))
}

/// 拡張子のない書き先 `stem` に拡張子 `ext` を足したもの。
func stemURL(_ stem: UnsafePointer<CChar>, _ ext: String) -> URL {
    URL(fileURLWithPath: String(cString: stem) + "." + ext)
}

/// 非同期の処理を、呼んだスレッドで待つ（Rust の作業スレッドから呼ばれる前提）。
final class Box<T>: @unchecked Sendable {
    var value: T?
}

func waitFor<T>(_ operation: @escaping @Sendable () async throws -> T) -> T? {
    let box = Box<T>()
    let semaphore = DispatchSemaphore(value: 0)
    Task.detached {
        box.value = try? await operation()
        semaphore.signal()
    }
    semaphore.wait()
    return box.value
}

/// 長辺が `maxPx` を超えていれば縮める。
func scaled(_ image: CGImage, maxPx: Int) -> CGImage? {
    let longest = max(image.width, image.height)
    if longest <= maxPx {
        return image
    }
    let scale = Double(maxPx) / Double(longest)
    let width = max(1, Int((Double(image.width) * scale).rounded()))
    let height = max(1, Int((Double(image.height) * scale).rounded()))
    guard let space = CGColorSpace(name: CGColorSpace.sRGB),
          let context = CGContext(
              data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
              space: space, bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue
          )
    else {
        return nil
    }
    context.interpolationQuality = .high
    context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
    return context.makeImage()
}

/// 画像がアルファの列を持つか。
func hasAlphaChannel(_ image: CGImage) -> Bool {
    switch image.alphaInfo {
    case .none, .noneSkipFirst, .noneSkipLast: return false
    default: return true
    }
}

/// アルファを保って sRGB の premultipliedLast（RGBA8）に描き直す。長辺が `maxPx` を超えていれば縮める（拡大はしない）。
/// 描き直した画像と、透明な画素（α < 255）が 1 つでもあるかを返す。
func redrawKeepingAlpha(_ image: CGImage, maxPx: Int) -> (image: CGImage, transparent: Bool)? {
    let longest = max(image.width, image.height)
    let scale = longest > maxPx ? Double(maxPx) / Double(longest) : 1
    let width = max(1, Int((Double(image.width) * scale).rounded()))
    let height = max(1, Int((Double(image.height) * scale).rounded()))
    guard let space = CGColorSpace(name: CGColorSpace.sRGB),
          let context = CGContext(
              data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
              space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
          )
    else {
        return nil
    }
    let rect = CGRect(x: 0, y: 0, width: width, height: height)
    context.clear(rect)
    context.interpolationQuality = .high
    context.draw(image, in: rect)
    guard let data = context.data else {
        return nil
    }
    // 描いた領域の大きさは context の bytesPerRow × 高さ（RGBA8 なので 1 行は 4 × 幅以上）
    let stride = context.bytesPerRow
    guard stride >= width * 4 else {
        return nil
    }
    let bytes = data.bindMemory(to: UInt8.self, capacity: stride * height)
    var transparent = false
    scan: for y in 0..<height {
        let row = bytes + y * stride
        for x in 0..<width where row[x * 4 + 3] < 255 {
            transparent = true
            break scan
        }
    }
    guard let out = context.makeImage() else {
        return nil
    }
    return (out, transparent)
}

/// 書き先のフォルダを作る。
private func makeParent(_ url: URL) {
    try? FileManager.default.createDirectory(
        at: url.deletingLastPathComponent(), withIntermediateDirectories: true
    )
}

/// JPEG を書く（フォルダがなければ作る）。
func writeJPEG(_ image: CGImage, to url: URL) -> Bool {
    makeParent(url)
    guard let destination = CGImageDestinationCreateWithURL(
        url as CFURL, UTType.jpeg.identifier as CFString, 1, nil
    ) else {
        return false
    }
    let options = [kCGImageDestinationLossyCompressionQuality: jpegQuality] as CFDictionary
    CGImageDestinationAddImage(destination, image, options)
    return CGImageDestinationFinalize(destination)
}

/// PNG を書く（フォルダがなければ作る）。
func writePNG(_ image: CGImage, to url: URL) -> Bool {
    makeParent(url)
    guard let destination = CGImageDestinationCreateWithURL(
        url as CFURL, UTType.png.identifier as CFString, 1, nil
    ) else {
        return false
    }
    CGImageDestinationAddImage(destination, image, nil)
    return CGImageDestinationFinalize(destination)
}

/// 長辺 1600 の画像を書き、512 に縮めたものも書く。
func writeThumbnails(
    _ large: CGImage, _ outLarge: URL, _ outSmall: URL, _ largePx: Int32, _ smallPx: Int32
) -> Int32 {
    guard let large = scaled(large, maxPx: Int(largePx)),
          writeJPEG(large, to: outLarge),
          let small = scaled(large, maxPx: Int(smallPx)),
          writeJPEG(small, to: outSmall)
    else {
        return kFailed
    }
    return kOK
}

/// アルファを保つサムネイル：長辺 1600 に描き直して透明な画素があれば、1600 と 512 を PNG で `<stem>.png` に書いて kPNG を
/// 返す。透明な画素がなければ、今までどおり JPEG で `<stem>.jpg` に書いて kOK を返す（決定 4）。
func writeThumbnailsKeepingAlpha(
    _ large: CGImage, _ outLargeStem: UnsafePointer<CChar>, _ outSmallStem: UnsafePointer<CChar>,
    _ largePx: Int32, _ smallPx: Int32
) -> Int32 {
    guard let (redrawn, transparent) = redrawKeepingAlpha(large, maxPx: Int(largePx)) else {
        return kFailed
    }
    if !transparent {
        return writeThumbnails(
            large, stemURL(outLargeStem, "jpg"), stemURL(outSmallStem, "jpg"), largePx, smallPx
        )
    }
    guard let (small, _) = redrawKeepingAlpha(redrawn, maxPx: Int(smallPx)),
          writePNG(redrawn, to: stemURL(outLargeStem, "png")),
          writePNG(small, to: stemURL(outSmallStem, "png"))
    else {
        return kFailed
    }
    return kPNG
}
