// 能力と環境（kura_codec_caps）：OS の版・フレームワークの版・ImageIO の読める UTI・AudioToolbox の入れ物と中身の形式・
// AAC-LC のレートとビットレートを、事実だけ JSON で返す。kura の形式の ID への写しと、何を登録するかは Rust が決める。
// プロセスで 1 度だけ調べる（Swift の大域の let は最初に使うときに 1 度だけ作られ、スレッドの間で安全）。
import AudioToolbox
import Foundation
import ImageIO
import KuraMacC

/// AAC-LC の 1 つのチャンネルの数で選べるビットレート。
private struct AacChannels: Encodable {
    let channels: UInt32
    /// ビット毎秒（0 を除き、小さい順）。
    let bitrates: [UInt32]
}

/// AAC-LC の 1 つのレート。
private struct AacRate: Encodable {
    let rate: UInt32
    let channels: [AacChannels]
}

private struct Caps: Encodable {
    let abi: UInt32
    /// OS の版（例 "26.7"）と build（例 "25G229"）。
    let os: String
    let build: String
    /// フレームワークの CFBundleVersion（imageio・coregraphics・audiotoolbox）。
    let frameworks: [String: String]
    /// ImageIO が読める UTI。
    let imageRead: [String]
    /// AudioFile が読める入れ物の FourCC。
    let audioFileRead: [UInt32]
    /// AudioToolbox の中身のデコーダ・エンコーダの FourCC。
    let audioDecode: [UInt32]
    let audioEncode: [UInt32]
    /// AAC-LC で書けるレートとビットレート。
    let aacLc: [AacRate]
}

private func osBuild() -> String {
    var size = 0
    guard sysctlbyname("kern.osversion", nil, &size, nil, 0) == 0, size > 0, size <= 256 else { return "" }
    var buf = [CChar](repeating: 0, count: size)
    guard sysctlbyname("kern.osversion", &buf, &size, nil, 0) == 0 else { return "" }
    let bytes = buf.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
    return String(decoding: bytes, as: UTF8.self)
}

private func osVersion() -> String {
    let v = ProcessInfo.processInfo.operatingSystemVersion
    return v.patchVersion == 0 ? "\(v.majorVersion).\(v.minorVersion)" : "\(v.majorVersion).\(v.minorVersion).\(v.patchVersion)"
}

private func bundleVersion(_ id: String) -> String? {
    Bundle(identifier: id)?.infoDictionary?["CFBundleVersion"] as? String
}

/// AudioFile の大域の情報（UInt32 の並び）。
private func audioFileTypes(_ property: AudioFilePropertyID) -> [UInt32] {
    var size: UInt32 = 0
    guard AudioFileGetGlobalInfoSize(property, 0, nil, &size) == noErr, size > 0, size <= 65_536 else { return [] }
    var out = [UInt32](repeating: 0, count: Int(size) / MemoryLayout<UInt32>.size)
    var bytes = UInt32(truncatingIfNeeded: out.count * MemoryLayout<UInt32>.size)  // 65,536 以下
    let status = out.withUnsafeMutableBytes { raw -> OSStatus in
        guard let base = raw.baseAddress else { return -1 }
        return AudioFileGetGlobalInfo(property, 0, nil, &bytes, base)
    }
    guard status == noErr else { return [] }
    return Array(out.prefix(Int(bytes) / MemoryLayout<UInt32>.size))
}

/// AudioFormat の FourCC の一覧（デコーダ・エンコーダ）。
private func formatIDs(_ property: AudioFormatPropertyID) -> [UInt32] {
    var size: UInt32 = 0
    guard AudioFormatGetPropertyInfo(property, 0, nil, &size) == noErr, size > 0, size <= 65_536 else { return [] }
    var out = [UInt32](repeating: 0, count: Int(size) / MemoryLayout<UInt32>.size)
    var bytes = UInt32(truncatingIfNeeded: out.count * MemoryLayout<UInt32>.size)  // 65,536 以下
    let status = out.withUnsafeMutableBytes { raw -> OSStatus in
        guard let base = raw.baseAddress else { return -1 }
        return AudioFormatGetProperty(property, 0, nil, &bytes, base)
    }
    guard status == noErr else { return [] }
    return Array(out.prefix(Int(bytes) / MemoryLayout<UInt32>.size))
}

/// 値の範囲の並びを読む（取れなければ空）。
private func valueRanges(_ count: UInt32, _ fill: (inout UInt32, UnsafeMutableRawPointer) -> OSStatus) -> [AudioValueRange] {
    let n = Int(count) / MemoryLayout<AudioValueRange>.stride
    guard n > 0, n <= 1_024 else { return [] }
    var out = [AudioValueRange](repeating: AudioValueRange(), count: n)
    var size = UInt32(truncatingIfNeeded: n * MemoryLayout<AudioValueRange>.stride)  // 1,024 個以下
    let status = out.withUnsafeMutableBytes { raw -> OSStatus in
        guard let base = raw.baseAddress else { return -1 }
        return fill(&size, base)
    }
    guard status == noErr else { return [] }
    return Array(out.prefix(Int(size) / MemoryLayout<AudioValueRange>.stride))
}

/// 値の範囲のうち、最小と最大が同じ整数のもの（レート・ビットレートの表）。
private func exactValues(_ ranges: [AudioValueRange]) -> [UInt32] {
    var values: [UInt32] = []
    for r in ranges where r.mMinimum == r.mMaximum {
        if let v = UInt32(exactly: r.mMinimum), v > 0, !values.contains(v) {
            values.append(v)
        }
    }
    return values.sorted()
}

/// AAC-LC のレートとチャンネルの数（1〜8）ごとのビットレート。
private func aacLcTable() -> [AacRate] {
    var formatID = kAudioFormatMPEG4AAC
    var size: UInt32 = 0
    guard AudioFormatGetPropertyInfo(
        kAudioFormatProperty_AvailableEncodeSampleRates, UInt32(MemoryLayout<UInt32>.size), &formatID, &size
    ) == noErr else {
        return []
    }
    let rates = exactValues(valueRanges(size) { size, base in
        AudioFormatGetProperty(
            kAudioFormatProperty_AvailableEncodeSampleRates, UInt32(MemoryLayout<UInt32>.size), &formatID, &size, base
        )
    })
    var table: [AacRate] = []
    for rate in rates {
        var perChannels: [AacChannels] = []
        for ch in UInt32(1)...8 {
            var src = floatPlanar(rate: Double(rate), channels: ch)
            var dst = AudioStreamBasicDescription(
                mSampleRate: Double(rate), mFormatID: kAudioFormatMPEG4AAC, mFormatFlags: 0, mBytesPerPacket: 0,
                mFramesPerPacket: 0, mBytesPerFrame: 0, mChannelsPerFrame: ch, mBitsPerChannel: 0, mReserved: 0
            )
            var conv: AudioConverterRef?
            guard AudioConverterNew(&src, &dst, &conv) == noErr, let conv else { continue }
            defer { AudioConverterDispose(conv) }
            var bsize: UInt32 = 0
            guard AudioConverterGetPropertyInfo(conv, kAudioConverterApplicableEncodeBitRates, &bsize, nil) == noErr
            else { continue }
            let bitrates = exactValues(valueRanges(bsize) { size, base in
                AudioConverterGetProperty(conv, kAudioConverterApplicableEncodeBitRates, &size, base)
            })
            if !bitrates.isEmpty {
                perChannels.append(AacChannels(channels: ch, bitrates: bitrates))
            }
        }
        if !perChannels.isEmpty {
            table.append(AacRate(rate: rate, channels: perChannels))
        }
    }
    return table
}

private func collectCaps() -> [UInt8] {
    var frameworks: [String: String] = [:]
    for (key, id) in [
        ("imageio", "com.apple.ImageIO"),
        ("coregraphics", "com.apple.CoreGraphics"),
        ("audiotoolbox", "com.apple.audio.toolbox.AudioToolbox"),
    ] {
        if let v = bundleVersion(id) {
            frameworks[key] = v
        }
    }
    let caps = Caps(
        abi: UInt32(KURA_FFI_ABI),
        os: osVersion(),
        build: osBuild(),
        frameworks: frameworks,
        imageRead: ((CGImageSourceCopyTypeIdentifiers() as? [String]) ?? []).sorted(),
        audioFileRead: audioFileTypes(kAudioFileGlobalInfo_ReadableTypes),
        audioDecode: formatIDs(kAudioFormatProperty_DecodeFormatIDs),
        audioEncode: formatIDs(kAudioFormatProperty_EncodeFormatIDs),
        aacLc: aacLcTable()
    )
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    guard let data = try? encoder.encode(caps) else { return [] }
    return Array(data)
}

/// プロセスで 1 度だけ調べた能力の JSON。
private let capsJSON: [UInt8] = collectCaps()

/// 能力と環境の JSON を `buf`（`cap` バイト）に写し、JSON の長さを返す（`buf` が NULL か足りなければ写さない。
/// 長さを聞いて確保してからもう 1 度呼ぶ）。調べられなければ 0。
@_cdecl("kura_codec_caps")
public func kuraCodecCaps(_ buf: UnsafeMutablePointer<UInt8>?, _ cap: Int64) -> Int64 {
    let json = capsJSON
    guard let buf, let cap = nonNegative(cap), cap >= json.count else { return Int64(json.count) }
    var i = 0
    while i < json.count {
        buf[i] = json[i]
        i += 1
    }
    return Int64(json.count)
}
