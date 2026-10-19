// kura-mac のフィクスチャを作る（ImageIO の CGImageDestination で書く）。
// 使い方: mkfix <出力のフォルダ>
import CoreGraphics
import Foundation
import ImageIO

let out = URL(fileURLWithPath: CommandLine.arguments[1])

/// 決まった画素（位置から作る）。
func pattern(_ w: Int, _ h: Int, _ seed: Int) -> [UInt8] {
    var px = [UInt8](repeating: 0, count: w * h * 4)
    for y in 0..<h {
        for x in 0..<w {
            let i = (y * w + x) * 4
            px[i] = UInt8((x * 37 + seed * 50) & 0xFF)
            px[i + 1] = UInt8((y * 59 + seed * 30) & 0xFF)
            px[i + 2] = UInt8(((x + y) * 23 + seed * 70) & 0xFF)
            px[i + 3] = UInt8((x * 13 + y * 29 + 40) & 0xFF)
        }
    }
    return px
}

func image(_ px: [UInt8], _ w: Int, _ h: Int, alpha: CGImageAlphaInfo) -> CGImage {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let provider = CGDataProvider(data: Data(px) as CFData)!
    return CGImage(width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4, space: space,
                   bitmapInfo: CGBitmapInfo(rawValue: alpha.rawValue), provider: provider, decode: nil,
                   shouldInterpolate: false, intent: .defaultIntent)!
}

// 1. HEICS（3 フレーム・24×16・不透明。LoopCount 2・DelayTime 0.25/0.5/0.75 を渡したが、ImageIO は LoopCount 1・
//    DelayTime 0.25/0.5/0.5 で書いた。README の表は読んだ値）
do {
    let url = out.appendingPathComponent("heics-3x24x16.heic")
    let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.heics" as CFString, 3, nil)!
    CGImageDestinationSetProperties(dest, [kCGImagePropertyHEICSDictionary: [kCGImagePropertyHEICSLoopCount: 2]] as CFDictionary)
    for i in 0..<3 {
        var px = pattern(24, 16, i)
        for k in stride(from: 3, to: px.count, by: 4) { px[k] = 255 }
        let props = [kCGImagePropertyHEICSDictionary: [kCGImagePropertyHEICSDelayTime: 0.25 + Double(i) * 0.25],
                     kCGImageDestinationLossyCompressionQuality: 1.0] as CFDictionary
        CGImageDestinationAddImage(dest, image(px, 24, 16, alpha: .noneSkipLast), props)
    }
    print("heics", CGImageDestinationFinalize(dest))
}

// 2. 関連付けたアルファ（プレマルチプライド）の TIFF（RGBA8・7×5）
do {
    let w = 7, h = 5
    var px = pattern(w, h, 4)
    // プレマルチプライドにする（c ≤ a）
    for i in stride(from: 0, to: px.count, by: 4) {
        let a = Int(px[i + 3])
        for c in 0..<3 { px[i + c] = UInt8((Int(px[i + c]) * a + 127) / 255) }
    }
    let url = out.appendingPathComponent("tiff-premultiplied.tif")
    let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.tiff" as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, image(px, w, h, alpha: .premultipliedLast), nil)
    print("premul tiff", CGImageDestinationFinalize(dest))
    try? Data(px).write(to: out.appendingPathComponent("tiff-premultiplied.rgba"))
}

// 3. 浮動小数の TIFF（RGBA の f32・4×4）
do {
    let w = 4, h = 4
    var fl = [Float](repeating: 0, count: w * h * 4)
    for i in 0..<fl.count { fl[i] = Float(i % 17) / 8.0 }
    let space = CGColorSpace(name: CGColorSpace.extendedLinearSRGB)!
    let data = fl.withUnsafeBytes { Data($0) }
    let provider = CGDataProvider(data: data as CFData)!
    let info = CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue | CGBitmapInfo.floatComponents.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
    let img = CGImage(width: w, height: h, bitsPerComponent: 32, bitsPerPixel: 128, bytesPerRow: w * 16, space: space,
                      bitmapInfo: info, provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
    let url = out.appendingPathComponent("tiff-float.tif")
    let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.tiff" as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, img, nil)
    print("float tiff", CGImageDestinationFinalize(dest))
}

// 4. なめらかな HEIC（300×200・不透明・品質 0.9）。画素はテストの gradient と同じ式：r = x·255/w、g = y·255/h、b = 128
//    （CI の仮想マシンでは HEIC を書けないことがあるので、テストの中では作らない）
do {
    let w = 300, h = 200
    var px = [UInt8](repeating: 255, count: w * h * 4)
    for y in 0..<h {
        for x in 0..<w {
            let i = (y * w + x) * 4
            px[i] = UInt8(x * 255 / w)
            px[i + 1] = UInt8(y * 255 / h)
            px[i + 2] = 128
        }
    }
    let url = out.appendingPathComponent("heic-gradient-300x200.heic")
    let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.heic" as CFString, 1, nil)!
    let props = [kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary
    CGImageDestinationAddImage(dest, image(px, w, h, alpha: .noneSkipLast), props)
    print("gradient heic", CGImageDestinationFinalize(dest))
}

// 5. 16 ビットの関連付けたアルファの TIFF（RGBA16・8×8・sRGB）と、書いたプレマルチプライドの画素（RGBA16 の LE を行の順に
//    512 バイト）。先頭の 37 画素の R は、vImage の戻し（vImageConvert_AnyToAny で premultipliedLast → last）が kura の整数の
//    式 ⌊(2·c·65535 + a)/(2a)⌋ と 1 違う (c, a) の組（macOS 26.7 の vImage を乱数と総当たりで探した。8 ビットでは全組が同じ
//    なので、16 ビットでだけ戻しを vImage に任せたことに気づける）。残りは端の値と決まった模様。
do {
    let pairs: [(UInt16, UInt16)] = [
        (28879, 38837), (20376, 39877), (11266, 65387), (44421, 49391), (40681, 51067), (28325, 38689),
        (30670, 38467), (30674, 48329), (23696, 40399), (25366, 34943), (3918, 55511), (34625, 40097),
        (22956, 51607), (2495, 37541), (38899, 39013), (29031, 58927), (26976, 47011), (20517, 49699),
        (5688, 44237), (13810, 42527), (16566, 53437), (18970, 56459), (3921, 59489), (37383, 40649),
        (40976, 53497), (24873, 34109), (13674, 63961), (51656, 59633), (34792, 45389), (37363, 63541),
        (33482, 61547), (30046, 57559), (26437, 51577), (25337, 49583), (41968, 43601), (19046, 37619),
        (185, 33631),
    ]
    let edges: [(UInt16, UInt16)] = [
        (0, 0), (0, 1), (1, 1), (1, 2), (1, 3), (2, 3), (0, 65535), (1, 65535), (32767, 65535), (32768, 65535),
        (65534, 65535), (65535, 65535), (16384, 32768), (128, 257), (100, 200), (12345, 54321),
    ]
    let w = 8, h = 8
    var px = [UInt16](repeating: 0, count: w * h * 4)
    for i in 0..<(w * h) {
        var (c, a): (UInt16, UInt16)
        if i < pairs.count {
            (c, a) = pairs[i]
        } else if i - pairs.count < edges.count {
            (c, a) = edges[i - pairs.count]
        } else {
            a = UInt16((i * 4099 + 1234) % 65536)
            c = UInt16(Int(a) * ((i * 37) % 101) / 100)
        }
        px[i * 4] = c
        px[i * 4 + 1] = c / 2
        px[i * 4 + 2] = a
        px[i * 4 + 3] = a
    }
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let data = px.withUnsafeBytes { Data($0) }
    let info = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder16Little.rawValue)
    let img = CGImage(width: w, height: h, bitsPerComponent: 16, bitsPerPixel: 64, bytesPerRow: w * 8, space: space,
                      bitmapInfo: info, provider: CGDataProvider(data: data as CFData)!, decode: nil,
                      shouldInterpolate: false, intent: .defaultIntent)!
    let url = out.appendingPathComponent("tiff-premultiplied16.tif")
    let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.tiff" as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, img, nil)
    print("premul16 tiff", CGImageDestinationFinalize(dest))
    try? data.write(to: out.appendingPathComponent("tiff-premultiplied16.rgba"))
}

// 6. アルファのある HEIC と TIFF（48×32・sRGB。M11：ImageIO の道のサムネイルがアルファを保つ）。外周 8 画素は透明（α = 0）、
//    内側は不透明（r = x·255/w、g = y·255/h、b = 200）。HEIC は品質 0.9（CI の仮想マシンでは HEIC を書けないことがあるので、
//    テストの中では作らない）。TIFF はストレートのアルファ（last）
do {
    let w = 48, h = 32, border = 8
    var px = [UInt8](repeating: 0, count: w * h * 4)
    for y in 0..<h {
        for x in 0..<w {
            let i = (y * w + x) * 4
            let inside = x >= border && x < w - border && y >= border && y < h - border
            px[i] = inside ? UInt8(x * 255 / w) : 0
            px[i + 1] = inside ? UInt8(y * 255 / h) : 0
            px[i + 2] = inside ? 200 : 0
            px[i + 3] = inside ? 255 : 0
        }
    }
    let heic = out.appendingPathComponent("heic-alpha-48x32.heic")
    let hdest = CGImageDestinationCreateWithURL(heic as CFURL, "public.heic" as CFString, 1, nil)!
    let props = [kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary
    CGImageDestinationAddImage(hdest, image(px, w, h, alpha: .last), props)
    print("alpha heic", CGImageDestinationFinalize(hdest))
    let tiff = out.appendingPathComponent("tiff-alpha-48x32.tif")
    let tdest = CGImageDestinationCreateWithURL(tiff as CFURL, "public.tiff" as CFString, 1, nil)!
    CGImageDestinationAddImage(tdest, image(px, w, h, alpha: .last), nil)
    print("alpha tiff", CGImageDestinationFinalize(tdest))
}
