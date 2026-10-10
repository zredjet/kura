import AVFoundation
import CoreGraphics
import Foundation
import ImageIO

/// 画像のサムネイル（ImageIO の CGImageSourceCreateThumbnailAtIndex。HEIC・RAW・AVIF も扱える）。
@_cdecl("kura_image_thumbnails")
public func kuraImageThumbnails(
    _ path: UnsafePointer<CChar>, _ outLarge: UnsafePointer<CChar>, _ outSmall: UnsafePointer<CChar>,
    _ largePx: Int32, _ smallPx: Int32
) -> Int32 {
    let options = [kCGImageSourceShouldCache: false] as CFDictionary
    guard let source = CGImageSourceCreateWithURL(fileURL(path) as CFURL, options),
          CGImageSourceGetType(source) != nil,
          CGImageSourceGetCount(source) > 0
    else {
        return kUnsupported
    }
    let thumbnailOptions: [CFString: Any] = [
        kCGImageSourceCreateThumbnailFromImageAlways: true,
        kCGImageSourceCreateThumbnailWithTransform: true,
        kCGImageSourceThumbnailMaxPixelSize: Int(largePx),
        kCGImageSourceShouldCacheImmediately: true,
    ]
    guard let large = CGImageSourceCreateThumbnailAtIndex(source, 0, thumbnailOptions as CFDictionary) else {
        return kFailed
    }
    return writeThumbnails(large, outLarge, outSmall, largePx, smallPx)
}

/// 動画の `atSec` 秒の地点のサムネイル（AVAssetImageGenerator）。取れなければ先頭で作り直す。
@_cdecl("kura_video_thumbnails")
public func kuraVideoThumbnails(
    _ path: UnsafePointer<CChar>, _ atSec: Double, _ outLarge: UnsafePointer<CChar>,
    _ outSmall: UnsafePointer<CChar>, _ largePx: Int32, _ smallPx: Int32
) -> Int32 {
    let asset = AVURLAsset(url: fileURL(path))
    let tracks = waitFor { try await asset.loadTracks(withMediaType: .video) } ?? []
    if tracks.isEmpty {
        return kUnsupported
    }
    let generator = AVAssetImageGenerator(asset: asset)
    generator.appliesPreferredTrackTransform = true
    generator.maximumSize = CGSize(width: Int(largePx), height: Int(largePx))
    // 近くのキーフレームで十分（速さ優先）
    let tolerance = CMTime(seconds: 2, preferredTimescale: 600)
    generator.requestedTimeToleranceBefore = tolerance
    generator.requestedTimeToleranceAfter = tolerance

    func frame(at seconds: Double) -> CGImage? {
        let box = Box<CGImage>()
        let semaphore = DispatchSemaphore(value: 0)
        generator.generateCGImageAsynchronously(for: CMTime(seconds: seconds, preferredTimescale: 600)) {
            image, _, _ in
            box.value = image
            semaphore.signal()
        }
        semaphore.wait()
        return box.value
    }
    guard let large = frame(at: max(0, atSec)) ?? frame(at: 0) else {
        return kFailed
    }
    return writeThumbnails(large, outLarge, outSmall, largePx, smallPx)
}

/// 音楽のジャケット（メタデータの artwork）を `outArt` に書き、サムネイルも作る。
@_cdecl("kura_audio_artwork")
public func kuraAudioArtwork(
    _ path: UnsafePointer<CChar>, _ outArt: UnsafePointer<CChar>, _ outLarge: UnsafePointer<CChar>,
    _ outSmall: UnsafePointer<CChar>, _ largePx: Int32, _ smallPx: Int32
) -> Int32 {
    let asset = AVURLAsset(url: fileURL(path))
    let tracks = waitFor { try await asset.loadTracks(withMediaType: .audio) } ?? []
    if tracks.isEmpty {
        return kUnsupported
    }
    let metadata = (waitFor { try await asset.load(.commonMetadata) } ?? [])
        + (waitFor { try await asset.load(.metadata) } ?? [])
    let artworkItems = AVMetadataItem.metadataItems(from: metadata, filteredByIdentifier: .commonIdentifierArtwork)
    guard let item = artworkItems.first,
          let data = waitFor({ try await item.load(.dataValue) }) ?? nil
    else {
        return kNoArtwork
    }
    let thumbnailOptions: [CFString: Any] = [
        kCGImageSourceCreateThumbnailFromImageAlways: true,
        kCGImageSourceCreateThumbnailWithTransform: true,
        kCGImageSourceThumbnailMaxPixelSize: Int(largePx),
    ]
    guard let source = CGImageSourceCreateWithData(data as CFData, nil),
          let art = CGImageSourceCreateThumbnailAtIndex(source, 0, thumbnailOptions as CFDictionary),
          writeJPEG(art, to: fileURL(outArt))
    else {
        return kFailed
    }
    return writeThumbnails(art, outLarge, outSmall, largePx, smallPx)
}
