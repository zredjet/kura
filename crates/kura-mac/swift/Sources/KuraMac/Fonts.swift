import CoreGraphics
import CoreText
import Foundation
import ImageIO

/// フォントのファイルの形式（先頭の 4 バイト）。知らない形式は nil（CoreText に渡さない）。
private func fontFormat(_ data: Data) -> String? {
    let head = [UInt8](data.prefix(4))
    guard head.count == 4 else {
        return nil
    }
    switch String(bytes: head, encoding: .isoLatin1) {
    case "\u{0}\u{1}\u{0}\u{0}", "true": return "ttf"
    case "OTTO": return "otf"
    case "ttcf": return "ttc"
    case "wOFF": return "woff"
    case "wOF2": return "woff2"
    default: return nil
    }
}

/// フォントのファイルを読み、書体の記述子（コレクションは書体の数だけ）を作る。読めなければ nil。
private func loadFont(_ url: URL) -> (format: String, descriptors: [CTFontDescriptor])? {
    guard let data = try? Data(contentsOf: url, options: .mappedIfSafe),
          let format = fontFormat(data),
          let descriptors = CTFontManagerCreateFontDescriptorsFromData(data as CFData) as? [CTFontDescriptor],
          !descriptors.isEmpty
    else {
        return nil
    }
    return (format, descriptors)
}

/// 字の並び（左から順に送り幅で並べる）と、形の範囲の和。形がなければ nil。
private func layoutGlyphs(_ font: CTFont, _ glyphs: [CGGlyph]) -> (positions: [CGPoint], bounds: CGRect)? {
    var advances = [CGSize](repeating: .zero, count: glyphs.count)
    var rects = [CGRect](repeating: .zero, count: glyphs.count)
    CTFontGetAdvancesForGlyphs(font, .horizontal, glyphs, &advances, glyphs.count)
    CTFontGetBoundingRectsForGlyphs(font, .horizontal, glyphs, &rects, glyphs.count)
    var x: CGFloat = 0
    var positions: [CGPoint] = []
    var bounds = CGRect.null
    for i in glyphs.indices {
        positions.append(CGPoint(x: x, y: 0))
        let rect = rects[i]
        if !rect.isNull, !rect.isEmpty, rect.width.isFinite, rect.height.isFinite {
            bounds = bounds.union(rect.offsetBy(dx: x, dy: 0))
        }
        let advance = advances[i].width
        x += advance.isFinite ? advance : 0
    }
    guard !bounds.isNull, bounds.width > 0, bounds.height > 0,
          bounds.width.isFinite, bounds.height.isFinite
    else {
        return nil
    }
    return (positions, bounds)
}

/// 見本の字：候補（改行で区切る）のうち、その書体がすべての字を持つ最初のもの。どれも持たなければ、字の番号 1 から順に、
/// 形のある字を 4 つまで（番号 1,000 まで探す）。
private func sampleGlyphs(_ font: CTFont, _ candidates: [String]) -> [CGGlyph] {
    for text in candidates {
        let chars = Array(text.utf16)
        if chars.isEmpty {
            continue
        }
        var glyphs = [CGGlyph](repeating: 0, count: chars.count)
        if CTFontGetGlyphsForCharacters(font, chars, &glyphs, chars.count) {
            // サロゲートの組は 2 つめの位置が 0 になる
            return glyphs.filter { $0 != 0 }
        }
    }
    let count = min(CTFontGetGlyphCount(font), 1000)
    var picked: [CGGlyph] = []
    var index = 1
    while index < count, picked.count < 4 {
        let glyph = CGGlyph(clamping: index)
        if layoutGlyphs(font, [glyph]) != nil {
            picked.append(glyph)
        }
        index += 1
    }
    return picked
}

/// 正方形 `px` の透明の地に、字の形だけのアルファで見本を描く（グレーとアルファ。字は黒、α は覆う割合）。
/// 形の範囲が幅の 80%・高さの 70% に収まる大きさで、真ん中に置く。
private func renderSpecimen(_ base: CTFont, _ glyphs: [CGGlyph], _ px: Int) -> CGImage? {
    guard px > 0, let reference = layoutGlyphs(base, glyphs) else {
        return nil
    }
    let side = CGFloat(px)
    let scale = min(side * 0.8 / reference.bounds.width, side * 0.7 / reference.bounds.height)
    guard scale.isFinite, scale > 0 else {
        return nil
    }
    let font = CTFontCreateCopyWithAttributes(base, CTFontGetSize(base) * scale, nil, nil)
    guard let laid = layoutGlyphs(font, glyphs) else {
        return nil
    }
    let context = CGContext(
        data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) ?? CGColorSpace(name: CGColorSpace.sRGB).flatMap {
        CGContext(
            data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
            space: $0, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )
    }
    guard let context else {
        return nil
    }
    context.clear(CGRect(x: 0, y: 0, width: px, height: px))
    context.setShouldAntialias(true)
    context.setAllowsFontSmoothing(false)
    context.setFillColor(CGColor(gray: 0, alpha: 1))
    let dx = (side - laid.bounds.width) / 2 - laid.bounds.minX
    let dy = (side - laid.bounds.height) / 2 - laid.bounds.minY
    let positions = laid.positions.map { CGPoint(x: $0.x + dx, y: $0.y + dy) }
    CTFontDrawGlyphs(font, glyphs, positions, glyphs.count, context)
    return context.makeImage()
}

/// フォントのサムネイル（CoreText）。`samples` は見本の字の候補を改行で区切ったもの。長辺 `largePx`・`smallPx` の正方形の
/// PNG を `<stem>.png` に書き、kPNG を返す。フォントでない・知らない形式は kUnsupported、描けなければ kFailed。
@_cdecl("kura_font_thumbnails")
public func kuraFontThumbnails(
    _ path: UnsafePointer<CChar>, _ samples: UnsafePointer<CChar>, _ outLargeStem: UnsafePointer<CChar>,
    _ outSmallStem: UnsafePointer<CChar>, _ largePx: Int32, _ smallPx: Int32
) -> Int32 {
    guard let loaded = loadFont(fileURL(path)), let descriptor = loaded.descriptors.first else {
        return kUnsupported
    }
    let base = CTFontCreateWithFontDescriptor(descriptor, 100, nil)
    let candidates = String(cString: samples).split(separator: "\n").map(String.init)
    let glyphs = sampleGlyphs(base, candidates)
    guard !glyphs.isEmpty,
          let large = renderSpecimen(base, glyphs, Int(largePx)),
          let small = renderSpecimen(base, glyphs, Int(smallPx)),
          writePNG(large, to: stemURL(outLargeStem, "png")),
          writePNG(small, to: stemURL(outSmallStem, "png"))
    else {
        return kFailed
    }
    return kPNG
}

/// フォントの情報（`kura_probe` の種類 3）：`{"codec": 形式, "font": {"family", "style", "fullName", "postscriptName",
/// "glyphs", "faces", "variable"}}`。書体の情報はコレクションの先頭の書体のもの。
func probeFont(_ url: URL) -> [String: Any]? {
    guard let loaded = loadFont(url), let descriptor = loaded.descriptors.first else {
        return nil
    }
    let font = CTFontCreateWithFontDescriptor(descriptor, 12, nil)
    var info: [String: Any] = [
        "glyphs": max(0, CTFontGetGlyphCount(font)),
        "faces": loaded.descriptors.count,
        "family": CTFontCopyFamilyName(font) as String,
        "fullName": CTFontCopyFullName(font) as String,
        "postscriptName": CTFontCopyPostScriptName(font) as String,
    ]
    if let style = CTFontCopyName(font, kCTFontStyleNameKey) {
        info["style"] = style as String
    }
    let axes = (CTFontCopyVariationAxes(font) as? [Any]) ?? []
    info["variable"] = !axes.isEmpty
    return ["codec": loaded.format, "font": info]
}
