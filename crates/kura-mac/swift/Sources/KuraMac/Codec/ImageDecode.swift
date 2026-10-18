// ImageIO の読み取り（imageio-dec/1。Platform）：開く（ヘッダだけ）→ フレームを 1 度だけ展開 → 帯ずつ kura の並びに変換 → ICC。
//
// 約束（変えたら識別子の版を上げる。kura-engine の codec::platform::image と合わせて 1 つの約束）：
// - 入力のバイト列は Swift の CFData に写す（ImageIO に Rust の領域を貸さない）。CGImageSource はキャッシュなし。
// - 形式は CGImageSourceGetType を Rust が渡した UTI の集合と照らす（違えば扱えない）。フレームの列として読むのは
//   public.heics・public.avis だけで、ほかは主画像（CGImageSourceGetPrimaryImageIndex）を 1 枚。
// - CGImageSourceCreateImageAtIndex（キャッシュなし・浮動小数を許さない。切り抜き・縮小・サムネイル・向きの変形は使わない）
//   のあと、CGDataProviderCopyData で全体を 1 度だけ展開する。展開できたかは取り出したデータが NULL でないことで確かめる
//   （vImageBuffer_InitWithCGImage は展開に失敗しても 0 で埋めて成功を返す）。
// - 帯ごとに vImageConvert_AnyToAny で kura の並び（グレー／RGB、＋α、8・16 ビット LE）に写す。行き先の色空間は元の色空間
//   そのもの（パレットは基の色空間）なので色は変換しない。CMYK・Lab だけは sRGB に変換する。アルファはストレートか
//   プレマルチプライドのまま（戻しは Rust の整数の式）。向きは当てずに報告する。
// - HDR・浮動小数（浮動小数の成分・ITU-R 2100 の伝達関数・拡張範囲の色空間）は扱えない（KURA_UNSUPPORTED。旗 KURA_IMG_HDR）。
// - ICC は行き先の色空間のもの（ImageIO が推定した sRGB なども）を常に返す。生の EXIF は返さない（ImageIO に口がない）。
import Accelerate
import CoreGraphics
import Foundation
import ImageIO
import KuraMacC

/// フレームの列として読む UTI。
private let sequenceTypes: Set<String> = ["public.heics", "public.avis"]

/// kura の画素の並び（書く側）。
struct PixelLayout: Equatable {
    /// kura の PixelFormat::code（1〜8）。
    let pixelFormat: Int32
    /// KURA_ALPHA_*。
    let alpha: Int32
    let gray: Bool
    let bytesPerComponent: Int

    var channels: Int { (gray ? 1 : 3) + (alpha == Int32(KURA_ALPHA_NONE) ? 0 : 1) }
    var bytesPerPixel: Int { channels * bytesPerComponent }
}

/// CGImage を kura の並びに写す決まり。
struct ConvertPlan {
    let layout: PixelLayout
    /// 行き先の色空間（元のまま。パレットは基の色空間、CMYK・Lab は sRGB）。
    let space: CGColorSpace
    let converted: Bool
}

/// 写し方を決めた結果。
enum PlanResult {
    case plan(ConvertPlan)
    case hdr
    case unsupported(String)
}

/// 色空間の種類の名前（文言用）。
private func modelName(_ m: CGColorSpaceModel) -> String {
    switch m {
    case .monochrome: return "gray"
    case .rgb: return "rgb"
    case .cmyk: return "cmyk"
    case .lab: return "lab"
    case .indexed: return "indexed"
    case .deviceN: return "deviceN"
    case .XYZ: return "xyz"
    case .pattern: return "pattern"
    case .unknown: return "unknown"
    @unknown default: return "?"
    }
}

/// CGImage（展開しない属性だけ）から写し方を決める。
func convertPlan(_ image: CGImage, isFloat: Bool) -> PlanResult {
    guard let space = image.colorSpace else {
        return .unsupported("色空間がありません")
    }
    if isFloat || image.bitmapInfo.contains(.floatComponents) || image.bitsPerComponent > 16
        || CGColorSpaceUsesITUR_2100TF(space) || CGColorSpaceUsesExtendedRange(space)
    {
        return .hdr
    }
    guard [1, 2, 4, 5, 8, 16].contains(image.bitsPerComponent) else {
        return .unsupported("画素の形を扱えません（bpc=\(image.bitsPerComponent)・bpp=\(image.bitsPerPixel)）")
    }
    var dest = space
    var converted = false
    if space.model == .indexed {
        guard let base = space.baseColorSpace else {
            return .unsupported("パレットの基の色空間がありません")
        }
        dest = base
    }
    let gray: Bool
    switch dest.model {
    case .monochrome:
        gray = true
    case .rgb:
        gray = false
    case .cmyk, .lab:
        guard let srgb = CGColorSpace(name: CGColorSpace.sRGB) else {
            return .unsupported("sRGB の色空間を作れません")
        }
        dest = srgb
        gray = false
        converted = true
    default:
        return .unsupported("色空間（\(modelName(dest.model))）を扱えません")
    }
    let alpha: Int32
    switch image.alphaInfo {
    case .none, .noneSkipLast, .noneSkipFirst:
        alpha = Int32(KURA_ALPHA_NONE)
    case .last, .first:
        alpha = Int32(KURA_ALPHA_STRAIGHT)
    case .premultipliedLast, .premultipliedFirst:
        alpha = Int32(KURA_ALPHA_PREMULTIPLIED)
    case .alphaOnly:
        return .unsupported("アルファだけの画像は扱えません")
    @unknown default:
        return .unsupported("アルファの扱い（\(image.alphaInfo.rawValue)）を扱えません")
    }
    let sixteen = image.bitsPerComponent > 8
    let hasAlpha = alpha != Int32(KURA_ALPHA_NONE)
    let code: Int32
    switch (gray, sixteen, hasAlpha) {
    case (true, false, false): code = 1
    case (true, false, true): code = 2
    case (false, false, false): code = 3
    case (false, false, true): code = 4
    case (true, true, false): code = 5
    case (true, true, true): code = 6
    case (false, true, false): code = 7
    case (false, true, true): code = 8
    }
    let layout = PixelLayout(pixelFormat: code, alpha: alpha, gray: gray, bytesPerComponent: sixteen ? 2 : 1)
    return .plan(ConvertPlan(layout: layout, space: dest, converted: converted))
}

/// 行き先の vImage の形式。
private func destinationFormat(_ plan: ConvertPlan) -> vImage_CGImageFormat? {
    let layout = plan.layout
    var info: UInt32
    switch layout.alpha {
    case Int32(KURA_ALPHA_STRAIGHT): info = CGImageAlphaInfo.last.rawValue
    case Int32(KURA_ALPHA_PREMULTIPLIED): info = CGImageAlphaInfo.premultipliedLast.rawValue
    default: info = CGImageAlphaInfo.none.rawValue
    }
    if layout.bytesPerComponent == 2 {
        info |= CGBitmapInfo.byteOrder16Little.rawValue
    }
    return vImage_CGImageFormat(
        bitsPerComponent: layout.bytesPerComponent * 8,
        bitsPerPixel: layout.bytesPerPixel * 8,
        colorSpace: plan.space,
        bitmapInfo: CGBitmapInfo(rawValue: info),
        renderingIntent: .defaultIntent
    )
}

/// 秒を表示時間（ミリ秒）にする（範囲の外・有限でないものは 0）。
private func milliseconds(_ seconds: Double?) -> UInt32 {
    guard let seconds, seconds.isFinite, seconds >= 0 else { return 0 }
    let ms = (seconds * 1000).rounded()
    return UInt32(exactly: ms) ?? (ms > 0 ? UInt32.max : 0)
}

/// 展開したフレーム。
private final class DecodedFrame {
    let image: CGImage
    /// 展開した画素（ImageIO の形のまま）。
    let bitmap: CFData
    let base: UnsafePointer<UInt8>
    let length: Int
    let bytesPerRow: Int
    let width: Int
    let height: Int
    let converter: vImageConverter
    let layout: PixelLayout

    init(image: CGImage, bitmap: CFData, base: UnsafePointer<UInt8>, length: Int, converter: vImageConverter,
         layout: PixelLayout)
    {
        self.image = image
        self.bitmap = bitmap
        self.base = base
        self.length = length
        bytesPerRow = image.bytesPerRow
        width = image.width
        height = image.height
        self.converter = converter
        self.layout = layout
    }
}

/// 開いた画像。Rust からは不透明なハンドルとして持たれ、kura_img_close で 1 度だけ解放される。
final class ImageDecodeHandle {
    let data: CFData
    let source: CGImageSource
    let sequence: Bool
    let primary: Int
    let frames: Int
    let plan: ConvertPlan
    let width: Int
    let height: Int
    let icc: [UInt8]
    fileprivate var current: DecodedFrame?

    init(data: CFData, source: CGImageSource, sequence: Bool, primary: Int, frames: Int, plan: ConvertPlan,
         width: Int, height: Int, icc: [UInt8])
    {
        self.data = data
        self.source = source
        self.sequence = sequence
        self.primary = primary
        self.frames = frames
        self.plan = plan
        self.width = width
        self.height = height
        self.icc = icc
        handleOpened()
    }

    deinit {
        handleClosed()
    }

    /// ImageIO に渡す設定（キャッシュなし・浮動小数を許さない）。
    static let imageOptions = [
        kCGImageSourceShouldCache: false,
        kCGImageSourceShouldAllowFloat: false,
    ] as CFDictionary

    func imageIndex(_ frame: Int) -> Int {
        sequence ? frame : primary
    }
}

/// 属性の辞書（なければ空）。
private func properties(_ source: CGImageSource, _ index: Int) -> [CFString: Any] {
    (CGImageSourceCopyPropertiesAtIndex(source, index, nil) as? [CFString: Any]) ?? [:]
}

/// シーケンスの辞書（HEICS・AVIS）。
private func sequenceDictionary(_ props: [CFString: Any]) -> [CFString: Any]? {
    (props[kCGImagePropertyHEICSDictionary] as? [CFString: Any])
        ?? (props[kCGImagePropertyAVISDictionary] as? [CFString: Any])
}

private func openImage(
    _ bytes: UnsafePointer<UInt8>?, _ len: Int64, _ utis: UnsafePointer<CChar>?,
    _ out: UnsafeMutablePointer<kura_image_info>
) -> Result<ImageDecodeHandle, CodecFailure> {
    guard let bytes, let length = nonNegative(len), length > 0 else {
        return .failure(.invalid("入力のバイト列がありません"))
    }
    guard let utis, let list = String(validatingCString: utis) else {
        return .failure(.invalid("UTI の一覧が読めません"))
    }
    let allowed = Set(list.split(separator: ",").map(String.init))
    guard let data = CFDataCreate(kCFAllocatorDefault, bytes, length) else {
        return .failure(.limit("入力のバイト列を写せません"))
    }
    guard let source = CGImageSourceCreateWithData(data, [kCGImageSourceShouldCache: false] as CFDictionary) else {
        return .failure(.failed("ImageIO で開けません"))
    }
    guard let type = CGImageSourceGetType(source) as String? else {
        return .failure(.failed("ImageIO で形式を判別できません"))
    }
    guard allowed.contains(type) else {
        return .failure(.unsupported("ImageIO はこのファイルを \(type) と判別しました"))
    }
    let count = CGImageSourceGetCount(source)
    guard count > 0 else {
        return .failure(.failed("画像がありません"))
    }
    let sequence = sequenceTypes.contains(type) && count > 1
    let primary = sequence ? 0 : CGImageSourceGetPrimaryImageIndex(source)
    guard primary >= 0, primary < count else {
        return .failure(.failed("主画像の番号が範囲の外です"))
    }
    let props = properties(source, primary)
    var orientation = UInt32(1)
    if let o = props[kCGImagePropertyOrientation] as? Int, (1...8).contains(o), let v = UInt32(exactly: o) {
        orientation = v
    }
    let isFloat = (props[kCGImagePropertyIsFloat] as? Bool) ?? false
    guard let image = CGImageSourceCreateImageAtIndex(source, primary, ImageDecodeHandle.imageOptions) else {
        return .failure(.failed("ImageIO で画像を作れません"))
    }
    guard image.width > 0, image.height > 0, let w = UInt32(exactly: image.width),
          let h = UInt32(exactly: image.height)
    else {
        return .failure(.failed("画像の大きさが範囲の外です（\(image.width)×\(image.height)）"))
    }
    var flags: UInt32 = 0
    let plan: ConvertPlan
    switch convertPlan(image, isFloat: isFloat) {
    case .plan(let p):
        plan = p
    case .hdr:
        // 形と旗だけ書いて断る（扱えない変種。Rust は次の段へ回す）
        out.pointee = kura_image_info(
            width: w, height: h, frames: 1, orientation: orientation, pixel_format: 0,
            alpha: Int32(KURA_ALPHA_NONE), loop_count: 0, flags: UInt32(KURA_IMG_HDR), icc_len: 0, exif_len: 0
        )
        return .failure(.unsupported("HDR・浮動小数の画像は編集できません"))
    case .unsupported(let m):
        return .failure(.unsupported(m))
    }
    if plan.converted {
        flags |= UInt32(KURA_IMG_CONVERTED)
    }
    var loopCount: UInt32 = 0
    if sequence {
        flags |= UInt32(KURA_IMG_SEQUENCE)
        let top = (CGImageSourceCopyProperties(source, nil) as? [CFString: Any]) ?? [:]
        if let dict = sequenceDictionary(top), let n = dict[kCGImagePropertyHEICSLoopCount] as? Int, n > 0 {
            loopCount = UInt32(exactly: n) ?? 0
        }
    }
    let icc: [UInt8] = (plan.space.copyICCData() as Data?).map { Array($0) } ?? []
    guard let frames = UInt32(exactly: sequence ? count : 1) else {
        return .failure(.limit("フレームが多すぎます"))
    }
    out.pointee = kura_image_info(
        width: w, height: h, frames: frames, orientation: orientation, pixel_format: plan.layout.pixelFormat,
        alpha: plan.layout.alpha, loop_count: loopCount, flags: flags, icc_len: Int64(icc.count), exif_len: 0
    )
    return .success(ImageDecodeHandle(
        data: data, source: source, sequence: sequence, primary: primary, frames: Int(frames), plan: plan,
        width: image.width, height: image.height, icc: icc
    ))
}

/// 開く（展開しない）。`utis` は扱う UTI をカンマで区切った NUL 終端の文字列。成功なら形を `out` に書いてハンドルを返す。
/// 失敗なら NULL を返し、状態の番号を `status` に、文言を `err` に書く。HDR・浮動小数は KURA_UNSUPPORTED で、`out` の
/// flags に KURA_IMG_HDR を立てる。
@_cdecl("kura_img_open")
public func kuraImgOpen(
    _ bytes: UnsafePointer<UInt8>?, _ len: Int64, _ utis: UnsafePointer<CChar>?,
    _ out: UnsafeMutablePointer<kura_image_info>?, _ status: UnsafeMutablePointer<Int32>?,
    _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> UnsafeMutableRawPointer? {
    guard let out, let status else {
        writeError("引数が正しくありません", err, errCap)
        return nil
    }
    out.pointee = kura_image_info()
    switch openImage(bytes, len, utis, out) {
    case .success(let handle):
        status.pointee = kuraOK
        return Unmanaged.passRetained(handle).toOpaque()
    case .failure(let failure):
        status.pointee = failure.status
        writeError(failure.message, err, errCap)
        return nil
    }
}

private func beginFrame(
    _ h: ImageDecodeHandle, _ index: UInt32, _ out: UnsafeMutablePointer<kura_image_frame>,
    _ cancel: kura_cancel_fn?, _ ctx: UnsafeMutableRawPointer?
) -> CodecFailure? {
    let frame = Int(index)
    guard frame < h.frames else {
        return .invalid("フレームの番号が範囲の外です（\(frame) / \(h.frames)）")
    }
    if canceled(cancel, ctx) {
        return .canceled
    }
    // 前のフレームの展開を先に捨てる
    h.current = nil
    let imageIndex = h.imageIndex(frame)
    let props = properties(h.source, imageIndex)
    let isFloat = (props[kCGImagePropertyIsFloat] as? Bool) ?? false
    guard let image = CGImageSourceCreateImageAtIndex(h.source, imageIndex, ImageDecodeHandle.imageOptions) else {
        return .failed("ImageIO で画像を作れません")
    }
    let plan: ConvertPlan
    switch convertPlan(image, isFloat: isFloat) {
    case .plan(let p): plan = p
    case .hdr: return .unsupported("HDR・浮動小数のフレームは編集できません")
    case .unsupported(let m): return .unsupported(m)
    }
    guard let w = UInt32(exactly: image.width), let ht = UInt32(exactly: image.height) else {
        return .mismatch("フレームの大きさが範囲の外です")
    }
    var duration: UInt32 = 0
    if h.sequence, let dict = sequenceDictionary(props) {
        duration = milliseconds(
            (dict[kCGImagePropertyHEICSUnclampedDelayTime] as? Double)
                ?? (dict[kCGImagePropertyHEICSDelayTime] as? Double)
        )
    }
    out.pointee = kura_image_frame(
        width: w, height: ht, pixel_format: plan.layout.pixelFormat, alpha: plan.layout.alpha,
        duration_ms: duration, flags: plan.converted ? UInt32(KURA_IMG_CONVERTED) : 0
    )
    // 形が開いたときと違えば、Rust が壊れたファイルにする（展開しない）
    guard image.width == h.width, image.height == h.height, plan.layout == h.plan.layout else {
        return nil
    }
    // 全体を 1 度だけ展開する（途中では止められない）。NULL なら展開に失敗した
    imageDecodes.wrappingAdd(1, ordering: .relaxed)
    guard let bitmap = image.dataProvider?.data, let base = CFDataGetBytePtr(bitmap) else {
        return .failed("画像を展開できません（壊れたファイル）")
    }
    let length = CFDataGetLength(bitmap)
    // 展開した画素の長さを確かめる：(高さ − 1) × 行の長さ + 1 行の中身
    guard let rowBits = mulChecked(image.width, image.bitsPerPixel),
          let rowMin = addChecked(rowBits, 7).map({ $0 / 8 }),
          image.bytesPerRow >= rowMin,
          let body = mulChecked(image.bytesPerRow, image.height - 1),
          let need = addChecked(body, rowMin),
          length >= need
    else {
        return .mismatch("展開した画素の長さが合いません（\(length) バイト）")
    }
    guard var src = vImage_CGImageFormat(cgImage: image) else {
        return .unsupported("元の画素の形（bpc=\(image.bitsPerComponent)・bpp=\(image.bitsPerPixel)）を変換できません")
    }
    guard var dst = destinationFormat(plan) else {
        return .unsupported("kura の画素の形を作れません")
    }
    var error = vImage_Error(kvImageNoError)
    guard let converter = vImageConverter_CreateWithCGImageFormat(&src, &dst, nil, vImage_Flags(kvImageNoFlags), &error)?
        .takeRetainedValue()
    else {
        return .unsupported("変換器を作れません（vImage \(error)）")
    }
    if canceled(cancel, ctx) {
        return .canceled
    }
    h.current = DecodedFrame(
        image: image, bitmap: bitmap, base: base, length: length, converter: converter, layout: plan.layout
    )
    return nil
}

/// フレーム `index` を全体で 1 度だけ展開し、実際の形を `out` に書く（前のフレームの展開は捨てる）。形が開いたときと
/// 違えば展開せずに KURA_OK を返す（Rust が照らして壊れたファイルにする）。
@_cdecl("kura_img_begin")
public func kuraImgBegin(
    _ handle: UnsafeMutableRawPointer?, _ index: UInt32, _ out: UnsafeMutablePointer<kura_image_frame>?,
    _ cancel: kura_cancel_fn?, _ ctx: UnsafeMutableRawPointer?,
    _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> Int32 {
    guard let handle, let out else {
        return report(.invalid("引数が正しくありません"), err, errCap)
    }
    let h = Unmanaged<ImageDecodeHandle>.fromOpaque(handle).takeUnretainedValue()
    out.pointee = kura_image_frame()
    if let failure = beginFrame(h, index, out, cancel, ctx) {
        return report(failure, err, errCap)
    }
    return kuraOK
}

/// 展開したフレームの行 `y0..y0 + rows` を kura の並びで `buf`（`len` = `rows × rowBytes` バイトちょうど）に書く。
@_cdecl("kura_img_read_rows")
public func kuraImgReadRows(
    _ handle: UnsafeMutableRawPointer?, _ y0: Int64, _ rows: Int64, _ buf: UnsafeMutablePointer<UInt8>?,
    _ len: Int64, _ rowBytes: Int64, _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> Int32 {
    guard let handle, let buf else {
        return report(.invalid("引数が正しくありません"), err, errCap)
    }
    let h = Unmanaged<ImageDecodeHandle>.fromOpaque(handle).takeUnretainedValue()
    guard let frame = h.current else {
        return report(.invalid("展開したフレームがありません"), err, errCap)
    }
    guard let y = nonNegative(y0), let n = nonNegative(rows), n > 0, let end = addChecked(y, n), end <= frame.height,
          let stride = nonNegative(rowBytes), let want = mulChecked(frame.width, frame.layout.bytesPerPixel),
          stride == want, let total = nonNegative(len), let need = mulChecked(n, stride), total == need,
          let offset = mulChecked(y, frame.bytesPerRow)
    else {
        return report(.invalid("帯の範囲か領域の長さが正しくありません"), err, errCap)
    }
    var s = vImage_Buffer(
        data: UnsafeMutableRawPointer(mutating: frame.base + offset), height: vImagePixelCount(n),
        width: vImagePixelCount(frame.width), rowBytes: frame.bytesPerRow
    )
    var d = vImage_Buffer(
        data: UnsafeMutableRawPointer(buf), height: vImagePixelCount(n), width: vImagePixelCount(frame.width),
        rowBytes: stride
    )
    let e = vImageConvert_AnyToAny(frame.converter, &s, &d, nil, vImage_Flags(kvImageNoFlags))
    guard e == kvImageNoError else {
        return report(.failed("画素を変換できません（vImage \(e)）"), err, errCap)
    }
    return kuraOK
}

/// ICC プロファイルを `buf`（`len` = icc_len バイトちょうど）に写す。
@_cdecl("kura_img_copy_icc")
public func kuraImgCopyIcc(
    _ handle: UnsafeMutableRawPointer?, _ buf: UnsafeMutablePointer<UInt8>?, _ len: Int64,
    _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> Int32 {
    guard let handle, let buf else {
        return report(.invalid("引数が正しくありません"), err, errCap)
    }
    let h = Unmanaged<ImageDecodeHandle>.fromOpaque(handle).takeUnretainedValue()
    guard let n = nonNegative(len), n == h.icc.count else {
        return report(.invalid("ICC の長さが合いません"), err, errCap)
    }
    var i = 0
    while i < n {
        buf[i] = h.icc[i]
        i += 1
    }
    return kuraOK
}

/// ハンドルを解放する（Rust の Drop から 1 度だけ）。展開した画素もここで消える。
@_cdecl("kura_img_close")
public func kuraImgClose(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    Unmanaged<ImageDecodeHandle>.fromOpaque(handle).release()
}
