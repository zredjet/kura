import AVFoundation
import CoreGraphics
import Foundation
import ImageIO

/// メディア情報を JSON（`{"width":…,"height":…,"durationMs":…,"capturedAt":…,"codec":…,"artist":…,"album":…,"title":…}`）で返す。
/// `kind` は 0 = 画像、1 = 動画、2 = 音楽。扱えない形式なら NULL。返した文字列は `kura_free_string` で解放する。
@_cdecl("kura_probe")
public func kuraProbe(_ path: UnsafePointer<CChar>, _ kind: Int32) -> UnsafeMutablePointer<CChar>? {
    let url = fileURL(path)
    let info: [String: Any]?
    switch kind {
    case 0: info = probeImage(url)
    case 1: info = probeVideo(url)
    default: info = probeAudio(url)
    }
    guard let info,
          let data = try? JSONSerialization.data(withJSONObject: info),
          let json = String(data: data, encoding: .utf8)
    else {
        return nil
    }
    return strdup(json)
}

/// `kura_probe` が返した文字列を解放する。
@_cdecl("kura_free_string")
public func kuraFreeString(_ pointer: UnsafeMutablePointer<CChar>?) {
    free(pointer)
}

private func probeImage(_ url: URL) -> [String: Any]? {
    guard let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary),
          let type = CGImageSourceGetType(source),
          let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
    else {
        return nil
    }
    var info: [String: Any] = ["codec": shortType(type as String)]
    if let width = props[kCGImagePropertyPixelWidth] as? Int,
       let height = props[kCGImagePropertyPixelHeight] as? Int {
        // 向き 5〜8 は 90 度回っているので縦横を入れ替える
        let orientation = props[kCGImagePropertyOrientation] as? Int ?? 1
        let rotated = (5...8).contains(orientation)
        info["width"] = rotated ? height : width
        info["height"] = rotated ? width : height
    }
    if let exif = props[kCGImagePropertyExifDictionary] as? [CFString: Any],
       let original = exif[kCGImagePropertyExifDateTimeOriginal] as? String,
       let date = exifDate(original, offset: exif[kCGImagePropertyExifOffsetTimeOriginal] as? String) {
        info["capturedAt"] = Int64(date.timeIntervalSince1970 * 1000)
    }
    return info
}

private func probeVideo(_ url: URL) -> [String: Any]? {
    let asset = AVURLAsset(url: url)
    guard let track = (waitFor { try await asset.loadTracks(withMediaType: .video) } ?? []).first else {
        return nil
    }
    var info: [String: Any] = [:]
    if let (size, transform) = waitFor({ try await track.load(.naturalSize, .preferredTransform) }) {
        let shown = size.applying(transform)
        info["width"] = Int(abs(shown.width).rounded())
        info["height"] = Int(abs(shown.height).rounded())
    }
    if let duration = waitFor({ try await asset.load(.duration) }), duration.isNumeric {
        info["durationMs"] = Int64((duration.seconds * 1000).rounded())
    }
    if let formats = waitFor({ try await track.load(.formatDescriptions) }), let format = formats.first {
        info["codec"] = codecName(CMFormatDescriptionGetMediaSubType(format))
    }
    if let item = waitFor({ try await asset.load(.creationDate) }) ?? nil,
       let date = waitFor({ try await item.load(.dateValue) }) ?? nil {
        info["capturedAt"] = Int64(date.timeIntervalSince1970 * 1000)
    }
    return info
}

private func probeAudio(_ url: URL) -> [String: Any]? {
    let asset = AVURLAsset(url: url)
    guard let track = (waitFor { try await asset.loadTracks(withMediaType: .audio) } ?? []).first else {
        return nil
    }
    var info: [String: Any] = [:]
    if let duration = waitFor({ try await asset.load(.duration) }), duration.isNumeric {
        info["durationMs"] = Int64((duration.seconds * 1000).rounded())
    }
    if let formats = waitFor({ try await track.load(.formatDescriptions) }), let format = formats.first {
        info["codec"] = codecName(CMFormatDescriptionGetMediaSubType(format))
    }
    let metadata = waitFor { try await asset.load(.commonMetadata) } ?? []
    let text: (AVMetadataIdentifier) -> String? = { identifier in
        guard let item = AVMetadataItem.metadataItems(from: metadata, filteredByIdentifier: identifier).first,
              let value = waitFor({ try await item.load(.stringValue) }) ?? nil
        else {
            return nil
        }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
    info["artist"] = text(.commonIdentifierArtist)
    info["album"] = text(.commonIdentifierAlbumName)
    info["title"] = text(.commonIdentifierTitle)
    return info
}

/// 「public.jpeg」→「jpeg」のように短くする。
private func shortType(_ identifier: String) -> String {
    identifier.split(separator: ".").last.map(String.init) ?? identifier
}

/// FourCC を「h264」などの名前にする。
private func codecName(_ code: FourCharCode) -> String {
    let bytes = [24, 16, 8, 0].map { UInt8((code >> $0) & 0xFF) }
    let fourcc = String(bytes: bytes, encoding: .ascii)?.trimmingCharacters(in: .whitespaces) ?? ""
    switch fourcc {
    case "avc1", "avc3": return "h264"
    case "hvc1", "hev1": return "hevc"
    case "fLaC": return "flac"
    case ".mp3": return "mp3"
    case "aac": return "aac"
    default: return fourcc.lowercased()
    }
}

/// EXIF の「2026:08:30 18:22:01」を日時にする（時差がなければこの Mac の時間帯とみなす）。
private func exifDate(_ text: String, offset: String?) -> Date? {
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    if let offset {
        formatter.dateFormat = "yyyy:MM:dd HH:mm:ssxxx"
        return formatter.date(from: text + offset)
    }
    formatter.dateFormat = "yyyy:MM:dd HH:mm:ss"
    formatter.timeZone = TimeZone.current
    return formatter.date(from: text)
}
