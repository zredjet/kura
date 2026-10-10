import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// C 関数の戻り値。
let kOK: Int32 = 0
let kUnsupported: Int32 = 1
let kFailed: Int32 = 2
let kNoArtwork: Int32 = 3

/// JPEG の品質（docs/architecture.md §7）。
let jpegQuality = 0.82

func fileURL(_ path: UnsafePointer<CChar>) -> URL {
    URL(fileURLWithPath: String(cString: path))
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

/// JPEG を書く（フォルダがなければ作る）。
func writeJPEG(_ image: CGImage, to url: URL) -> Bool {
    try? FileManager.default.createDirectory(
        at: url.deletingLastPathComponent(), withIntermediateDirectories: true
    )
    guard let destination = CGImageDestinationCreateWithURL(
        url as CFURL, UTType.jpeg.identifier as CFString, 1, nil
    ) else {
        return false
    }
    let options = [kCGImageDestinationLossyCompressionQuality: jpegQuality] as CFDictionary
    CGImageDestinationAddImage(destination, image, options)
    return CGImageDestinationFinalize(destination)
}

/// 長辺 1600 の画像を書き、512 に縮めたものも書く。
func writeThumbnails(
    _ large: CGImage, _ outLarge: UnsafePointer<CChar>, _ outSmall: UnsafePointer<CChar>,
    _ largePx: Int32, _ smallPx: Int32
) -> Int32 {
    guard let large = scaled(large, maxPx: Int(largePx)),
          writeJPEG(large, to: fileURL(outLarge)),
          let small = scaled(large, maxPx: Int(smallPx)),
          writeJPEG(small, to: fileURL(outSmall))
    else {
        return kFailed
    }
    return kOK
}
