// AudioToolbox の読み取り（audiotoolbox-dec/1。Platform）：メモリのバイト列を AudioFileOpenWithCallbacks で開き、ExtAudioFile で
// planar の f32 にして、Rust の plane にチャンクずつ書く。
//
// 約束（変えたら識別子の版を上げる。kura-engine の codec::platform::audio と合わせて 1 つの約束）：
// - 入力のバイト列は Rust が持つ（ハンドルより長く生きる）。AudioFile の読みのコールバックは呼び出しの中だけで動く。
// - 中身の FourCC は Rust が渡した許可の一覧と照らす（AudioToolbox の Ogg Vorbis は先頭を 128 フレーム余分に切るので、
//   Rust は 'vorb' を許可しない）。
// - 長さは ExtAudioFile の FileLengthFrames（m4a の iTunSMPB・Ogg Opus の pre-skip から priming・remainder を切った長さ）。
// - 3 チャンネル以上は、ファイルのチャンネルの配置をビットマップにしたものが Rust の既定の並び（WAVE_FORMAT_EXTENSIBLE の
//   マスク）と同じときだけ読み、ClientChannelLayout にそのマスクを渡して kura の並びにする（並びを推測しない）。
import AudioToolbox
import Foundation
import KuraMacC

/// Rust のバイト列（AudioFile の読みのコールバックの相手。ハンドルより長く Rust が持つ）。ハンドルとは別のオブジェクトに
/// して、ハンドルの deinit の中で閉じるときのコールバックが解放中のハンドルに触らないようにする。
final class MemorySource {
    let bytes: UnsafePointer<UInt8>
    let length: Int

    init(bytes: UnsafePointer<UInt8>, length: Int) {
        self.bytes = bytes
        self.length = length
    }
}

/// 開いた音声。Rust からは不透明なハンドルとして持たれ、kura_aud_close で 1 度だけ解放される。
final class AudioDecodeHandle {
    let source: MemorySource
    var file: AudioFileID?
    var ext: ExtAudioFileRef?
    var channels = 0

    init(bytes: UnsafePointer<UInt8>, length: Int) {
        source = MemorySource(bytes: bytes, length: length)
        handleOpened()
    }

    deinit {
        // ExtAudioFileWrapAudioFileID は AudioFile を閉じないので、両方とも閉じる（ExtAudioFile が先）
        if let ext {
            ExtAudioFileDispose(ext)
        }
        if let file {
            AudioFileClose(file)
        }
        handleClosed()
    }
}

private let decodeReadProc: AudioFile_ReadProc = { client, position, request, buffer, actual in
    let h = Unmanaged<MemorySource>.fromOpaque(client).takeUnretainedValue()
    guard let start = Int(exactly: position), start >= 0, start <= h.length else {
        actual.pointee = 0
        return OSStatus(kAudioFilePositionError)
    }
    let n = min(Int(request), h.length - start)
    buffer.copyMemory(from: UnsafeRawPointer(h.bytes + start), byteCount: n)
    actual.pointee = UInt32(truncatingIfNeeded: n)
    return noErr
}

private let decodeSizeProc: AudioFile_GetSizeProc = { client in
    Int64(Unmanaged<MemorySource>.fromOpaque(client).takeUnretainedValue().length)
}

/// ExtAudioFile の属性を読む。
private func extProperty<T>(_ ext: ExtAudioFileRef, _ id: ExtAudioFilePropertyID, _ value: inout T) -> OSStatus {
    var size = UInt32(MemoryLayout<T>.size)
    return ExtAudioFileGetProperty(ext, id, &size, &value)
}

/// AudioChannelLayout の生のバイト列のチャンネルの記述（ラベル）をビットマップにする。表せなければ nil。
private func descriptionsBitmap(_ raw: UnsafeRawBufferPointer) -> UInt32? {
    guard raw.count >= 12 else { return nil }
    let n = Int(raw.loadUnaligned(fromByteOffset: 8, as: UInt32.self))
    guard n > 0, n <= (raw.count - 12) / 20 else { return nil }
    var bitmap: UInt32 = 0
    var i = 0
    while i < n {
        // AudioChannelDescription：label（u32）・flags（u32）・座標（f32 × 3）の 20 バイト
        let label = raw.loadUnaligned(fromByteOffset: 12 + i * 20, as: UInt32.self)
        // kAudioChannelLabel_Left（1）〜 TopBackRight（18）は、ビット label − 1 と同じ並び
        guard label >= 1, label <= 18 else { return nil }
        let bit = UInt32(1) << (label - 1)
        guard bitmap & bit == 0 else { return nil }
        bitmap |= bit
        i += 1
    }
    return bitmap
}

/// 配置のタグ（AAC_5_1 など）をビットマップにする：kAudioFormatProperty_BitmapForLayoutTag、だめならタグを記述に展開して
/// ラベルから（AAC_5_1 は BitmapForLayoutTag が 'fmt?' を返す）。
private func tagBitmap(_ tag: AudioChannelLayoutTag) -> UInt32? {
    var t = tag
    let tagSize = UInt32(MemoryLayout<AudioChannelLayoutTag>.size)
    var bitmap: UInt32 = 0
    var size = UInt32(MemoryLayout<UInt32>.size)
    if AudioFormatGetProperty(kAudioFormatProperty_BitmapForLayoutTag, tagSize, &t, &size, &bitmap) == noErr {
        return bitmap
    }
    var full: UInt32 = 0
    guard AudioFormatGetPropertyInfo(kAudioFormatProperty_ChannelLayoutForTag, tagSize, &t, &full) == noErr,
          full >= 12, full <= 65_536
    else {
        return nil
    }
    let count = Int(full)
    let raw = UnsafeMutableRawPointer.allocate(byteCount: count, alignment: 8)
    defer { raw.deallocate() }
    guard AudioFormatGetProperty(kAudioFormatProperty_ChannelLayoutForTag, tagSize, &t, &full, raw) == noErr,
          let got = Int(exactly: full), got >= 12, got <= count
    else {
        return nil
    }
    return descriptionsBitmap(UnsafeRawBufferPointer(start: raw, count: got))
}

/// チャンネルの配置（AudioChannelLayout の生のバイト列）をビットマップにする。表せなければ nil。
func channelBitmap(_ raw: UnsafeRawBufferPointer) -> UInt32? {
    guard raw.count >= 12 else { return nil }
    let tag = raw.loadUnaligned(fromByteOffset: 0, as: UInt32.self)
    switch tag {
    case kAudioChannelLayoutTag_UseChannelBitmap:
        return raw.loadUnaligned(fromByteOffset: 4, as: UInt32.self)
    case kAudioChannelLayoutTag_UseChannelDescriptions:
        return descriptionsBitmap(raw)
    default:
        return tagBitmap(tag)
    }
}

/// ファイルのチャンネルの配置のビットマップ（なければ nil）。
private func fileChannelBitmap(_ ext: ExtAudioFileRef) -> UInt32? {
    var size: UInt32 = 0
    guard ExtAudioFileGetPropertyInfo(ext, kExtAudioFileProperty_FileChannelLayout, &size, nil) == noErr,
          size >= 12, size <= 65_536
    else {
        return nil
    }
    let count = Int(size)
    let raw = UnsafeMutableRawPointer.allocate(byteCount: count, alignment: 8)
    defer { raw.deallocate() }
    guard ExtAudioFileGetProperty(ext, kExtAudioFileProperty_FileChannelLayout, &size, raw) == noErr,
          let got = Int(exactly: size), got >= 12, got <= count
    else {
        return nil
    }
    return channelBitmap(UnsafeRawBufferPointer(start: raw, count: got))
}

/// f32・planar のクライアントの形式。
func floatPlanar(rate: Double, channels: UInt32) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription(
        mSampleRate: rate, mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked | kAudioFormatFlagIsNonInterleaved,
        mBytesPerPacket: 4, mFramesPerPacket: 1, mBytesPerFrame: 4, mChannelsPerFrame: channels,
        mBitsPerChannel: 32, mReserved: 0
    )
}

/// ビットマップの ClientChannelLayout を渡す。
func setClientLayout(_ ext: ExtAudioFileRef, mask: UInt32) -> OSStatus {
    var layout = AudioChannelLayout(
        mChannelLayoutTag: kAudioChannelLayoutTag_UseChannelBitmap,
        mChannelBitmap: AudioChannelBitmap(rawValue: mask),
        mNumberChannelDescriptions: 0,
        mChannelDescriptions: AudioChannelDescription()
    )
    return ExtAudioFileSetProperty(
        ext, kExtAudioFileProperty_ClientChannelLayout, UInt32(MemoryLayout<AudioChannelLayout>.size), &layout
    )
}

private func openAudio(
    _ bytes: UnsafePointer<UInt8>?, _ len: Int64, _ hint: UInt32, _ allowed: UnsafePointer<UInt32>?,
    _ nAllowed: Int64, _ masks: UnsafePointer<UInt32>?, _ nMasks: Int64, _ out: UnsafeMutablePointer<kura_audio_info>
) -> Result<AudioDecodeHandle, CodecFailure> {
    guard let bytes, let length = nonNegative(len), length > 0 else {
        return .failure(.invalid("入力のバイト列がありません"))
    }
    guard let n = nonNegative(nAllowed), n <= 64, n == 0 || allowed != nil else {
        return .failure(.invalid("許可する形式の一覧が正しくありません"))
    }
    guard let m = nonNegative(nMasks), m <= 64, m == 0 || masks != nil else {
        return .failure(.invalid("チャンネルの並びの表が正しくありません"))
    }
    var allow: [UInt32] = []
    if let allowed {
        var i = 0
        while i < n {
            allow.append(allowed[i])
            i += 1
        }
    }
    var maskTable: [UInt32] = []
    if let masks {
        var i = 0
        while i < m {
            maskTable.append(masks[i])
            i += 1
        }
    }
    let h = AudioDecodeHandle(bytes: bytes, length: length)
    let client = Unmanaged.passUnretained(h.source).toOpaque()
    var file: AudioFileID?
    var status = AudioFileOpenWithCallbacks(client, decodeReadProc, nil, decodeSizeProc, nil, hint, &file)
    guard status == noErr, let file else {
        if status == kAudioFileUnsupportedFileTypeError || status == kAudioFileUnsupportedDataFormatError {
            return .failure(.unsupported("AudioToolbox はこの形式を読めません（\(describe(status))）"))
        }
        return .failure(.failed("AudioToolbox で開けません（\(describe(status))）"))
    }
    h.file = file
    var ext: ExtAudioFileRef?
    status = ExtAudioFileWrapAudioFileID(file, false, &ext)
    guard status == noErr, let ext else {
        return .failure(.failed("ExtAudioFile で包めません（\(describe(status))）"))
    }
    h.ext = ext
    var asbd = AudioStreamBasicDescription()
    status = extProperty(ext, kExtAudioFileProperty_FileDataFormat, &asbd)
    guard status == noErr else {
        return .failure(.failed("形式を読めません（\(describe(status))）"))
    }
    guard allow.contains(asbd.mFormatID) else {
        return .failure(.unsupported("中身の形式 \(fourCC(asbd.mFormatID)) は AudioToolbox では読みません"))
    }
    guard asbd.mChannelsPerFrame > 0, asbd.mSampleRate.isFinite, asbd.mSampleRate > 0 else {
        return .failure(.failed("サンプルレート・チャンネルの数が正しくありません"))
    }
    var frames: Int64 = 0
    status = extProperty(ext, kExtAudioFileProperty_FileLengthFrames, &frames)
    guard status == noErr, frames >= 0 else {
        return .failure(.failed("長さを読めません（\(describe(status))）"))
    }
    var priming: Int64 = -1
    var remainder: Int64 = -1
    var table = AudioFilePacketTableInfo()
    var size = UInt32(MemoryLayout<AudioFilePacketTableInfo>.size)
    if AudioFileGetProperty(file, kAudioFilePropertyPacketTableInfo, &size, &table) == noErr {
        priming = Int64(table.mPrimingFrames)
        remainder = Int64(table.mRemainderFrames)
    }
    var client32 = floatPlanar(rate: asbd.mSampleRate, channels: asbd.mChannelsPerFrame)
    status = ExtAudioFileSetProperty(
        ext, kExtAudioFileProperty_ClientDataFormat,
        UInt32(MemoryLayout<AudioStreamBasicDescription>.size), &client32
    )
    guard status == noErr else {
        return .failure(.failed("f32 に直せません（\(describe(status))）"))
    }
    if asbd.mChannelsPerFrame >= 3 {
        // 並びを推測しない：ファイルの配置が既定の並びと同じときだけ読み、kura の並びに並べ替えさせる
        let index = Int(asbd.mChannelsPerFrame) - 1
        guard index < maskTable.count, case let mask = maskTable[index], let bitmap = fileChannelBitmap(ext),
              bitmap == mask, mask.nonzeroBitCount == Int(asbd.mChannelsPerFrame)
        else {
            return .failure(.unsupported("\(asbd.mChannelsPerFrame) チャンネルの並びを決められません"))
        }
        status = setClientLayout(ext, mask: mask)
        guard status == noErr else {
            return .failure(.unsupported("チャンネルの並びを渡せません（\(describe(status))）"))
        }
    }
    h.channels = Int(asbd.mChannelsPerFrame)
    var bits: UInt32 = 0
    var flags: UInt32 = 0
    if asbd.mFormatID == kAudioFormatLinearPCM {
        bits = asbd.mBitsPerChannel
        flags |= UInt32(KURA_AUD_PCM)
        if asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0 {
            flags |= UInt32(KURA_AUD_FLOAT)
        }
    }
    out.pointee = kura_audio_info(
        rate: asbd.mSampleRate, channels: asbd.mChannelsPerFrame, format_id: asbd.mFormatID, frames: frames,
        priming: priming, remainder: remainder, bits: bits, flags: flags
    )
    return .success(h)
}

/// 開いて形を `out` に書く。`hint` は AudioFile の種類の手掛かり（ADTS のときだけ要る。ほかは 0）、`allowed` は読む中身の
/// FourCC の一覧、`masks` はチャンネルの数（1〜）ごとの既定の並び（WAVE_FORMAT_EXTENSIBLE のマスク）。失敗なら NULL を返し、
/// 状態の番号を `status` に、文言を `err` に書く。`bytes` は close まで Rust が持つ。
@_cdecl("kura_aud_open")
public func kuraAudOpen(
    _ bytes: UnsafePointer<UInt8>?, _ len: Int64, _ hint: UInt32, _ allowed: UnsafePointer<UInt32>?,
    _ nAllowed: Int64, _ masks: UnsafePointer<UInt32>?, _ nMasks: Int64,
    _ out: UnsafeMutablePointer<kura_audio_info>?, _ status: UnsafeMutablePointer<Int32>?,
    _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> UnsafeMutableRawPointer? {
    guard let out, let status else {
        writeError("引数が正しくありません", err, errCap)
        return nil
    }
    out.pointee = kura_audio_info()
    switch openAudio(bytes, len, hint, allowed, nAllowed, masks, nMasks, out) {
    case .success(let handle):
        status.pointee = kuraOK
        return Unmanaged.passRetained(handle).toOpaque()
    case .failure(let failure):
        status.pointee = failure.status
        writeError(failure.message, err, errCap)
        return nil
    }
}

/// 続きを最大 `frames` フレーム読み、各チャンネルの `planes[c]`（`frames` 個ぶん書ける）の先頭から書く。読んだ数を
/// `outFrames` に書く（0 なら終わり）。
@_cdecl("kura_aud_read")
public func kuraAudRead(
    _ handle: UnsafeMutableRawPointer?, _ planes: UnsafePointer<UnsafeMutablePointer<Float>?>?, _ channels: Int64,
    _ frames: Int64, _ outFrames: UnsafeMutablePointer<Int64>?, _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> Int32 {
    guard let handle, let planes, let outFrames else {
        return report(.invalid("引数が正しくありません"), err, errCap)
    }
    outFrames.pointee = 0
    let h = Unmanaged<AudioDecodeHandle>.fromOpaque(handle).takeUnretainedValue()
    guard let ext = h.ext, let ch = nonNegative(channels), ch == h.channels, ch > 0,
          let n = nonNegative(frames), n > 0, let bytes = mulChecked(n, 4), let byteSize = UInt32(exactly: bytes),
          let request = UInt32(exactly: n)
    else {
        return report(.invalid("チャンネルの数か長さが正しくありません"), err, errCap)
    }
    let list = AudioBufferList.allocate(maximumBuffers: ch)
    defer { free(list.unsafeMutablePointer) }
    var c = 0
    while c < ch {
        guard let plane = planes[c] else {
            return report(.invalid("plane がありません"), err, errCap)
        }
        list[c] = AudioBuffer(mNumberChannels: 1, mDataByteSize: byteSize, mData: UnsafeMutableRawPointer(plane))
        c += 1
    }
    var io = request
    let status = ExtAudioFileRead(ext, &io, list.unsafeMutablePointer)
    guard status == noErr else {
        return report(.failed("読み取りに失敗しました（\(describe(status))）"), err, errCap)
    }
    guard io <= request else {
        return report(.invalid("求めた数より多く読みました"), err, errCap)
    }
    outFrames.pointee = Int64(io)
    return kuraOK
}

/// ハンドルを解放する（Rust の Drop から 1 度だけ）。
@_cdecl("kura_aud_close")
public func kuraAudClose(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    Unmanaged<AudioDecodeHandle>.fromOpaque(handle).release()
}
