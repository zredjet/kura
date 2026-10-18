// AudioToolbox の AAC の書き出し（aac-audiotoolbox-enc/1。Platform）：ExtAudioFile で AAC-LC の m4a をメモリに書く。
//
// 約束（変えたら識別子の版を上げる。kura-engine の codec::platform::aac と合わせて 1 つの約束）：
// - AudioFileInitializeWithCallbacks（kAudioFileM4AType・kAudioFormatMPEG4AAC）＋ ExtAudioFileWrapAudioFileID（書き込み）。
//   一時ファイルは使わない。出力はハンドルの中の配列に溜め（上限 max_out。超えたら KURA_LIMIT）、close で消える。
// - クライアントの形式は f32・planar、ClientChannelLayout に Rust の既定の並び（ビットマップ）を必ず渡す（渡さないと
//   5.1 の Rs が LFE に入る）。
// - 変換器：kAudioCodecPropertyBitRateControlMode・品質は最大（kAudioConverterQuality_Max）・ビットレート（可変は
//   kAudioCodecPropertySoundQualityForVBR）を設定し、kExtAudioFileProperty_ConverterConfig に NULL を渡して反映する。
// - priming（2,112）・remainder は Apple が iTunSMPB に書く。m4a の時刻は Rust が 0 に書き換える。
import AudioToolbox
import Foundation
import KuraMacC

/// メモリの中の m4a（AudioFile のコールバックの相手）。ハンドルとは別のオブジェクトにして、ハンドルの deinit の中で
/// ExtAudioFile を閉じるときに呼ばれるコールバックが、解放中のハンドルに触らないようにする。
final class MemoryFile {
    var output: [UInt8] = []
    let maxOut: Int
    var overLimit = false

    init(maxOut: Int) {
        self.maxOut = maxOut
    }
}

/// 書きかけの AAC。Rust からは不透明なハンドルとして持たれ、kura_aac_close で 1 度だけ解放される。
final class AacEncodeHandle {
    let memory: MemoryFile
    var file: AudioFileID?
    var ext: ExtAudioFileRef?
    let channels: Int
    var finished = false

    init(maxOut: Int, channels: Int) {
        memory = MemoryFile(maxOut: maxOut)
        self.channels = channels
        handleOpened()
    }

    /// ExtAudioFile と AudioFile を閉じる（ExtAudioFile が先。Dispose がエンコーダの残りと m4a の頭を書く）。
    func closeFiles() -> OSStatus {
        var status = noErr
        if let ext {
            status = ExtAudioFileDispose(ext)
            self.ext = nil
        }
        if let file {
            let s = AudioFileClose(file)
            if status == noErr {
                status = s
            }
            self.file = nil
        }
        return status
    }

    deinit {
        _ = closeFiles()
        handleClosed()
    }
}

private func encoder(_ client: UnsafeMutableRawPointer) -> MemoryFile {
    Unmanaged<MemoryFile>.fromOpaque(client).takeUnretainedValue()
}

private let encodeReadProc: AudioFile_ReadProc = { client, position, request, buffer, actual in
    let h = encoder(client)
    guard let start = Int(exactly: position), start >= 0, start <= h.output.count else {
        actual.pointee = 0
        return OSStatus(kAudioFilePositionError)
    }
    let n = min(Int(request), h.output.count - start)
    h.output.withUnsafeBytes { src in
        if let base = src.baseAddress, n > 0 {
            buffer.copyMemory(from: base + start, byteCount: n)
        }
    }
    actual.pointee = UInt32(truncatingIfNeeded: n)
    return noErr
}

private let encodeWriteProc: AudioFile_WriteProc = { client, position, request, buffer, actual in
    let h = encoder(client)
    actual.pointee = 0
    guard let start = Int(exactly: position), start >= 0, let end = addChecked(start, Int(request)) else {
        return OSStatus(kAudioFilePositionError)
    }
    guard end <= h.maxOut else {
        h.overLimit = true
        return OSStatus(kAudioFileOperationNotSupportedError)
    }
    if end > h.output.count {
        h.output.append(contentsOf: repeatElement(0, count: end - h.output.count))
    }
    let n = Int(request)
    h.output.withUnsafeMutableBytes { dst in
        if let base = dst.baseAddress, n > 0 {
            (base + start).copyMemory(from: buffer, byteCount: n)
        }
    }
    actual.pointee = request
    return noErr
}

private let encodeSizeProc: AudioFile_GetSizeProc = { client in
    Int64(encoder(client).output.count)
}

private let encodeSetSizeProc: AudioFile_SetSizeProc = { client, size in
    let h = encoder(client)
    guard let s = Int(exactly: size), s >= 0 else {
        return OSStatus(kAudioFilePositionError)
    }
    guard s <= h.maxOut else {
        h.overLimit = true
        return OSStatus(kAudioFileOperationNotSupportedError)
    }
    if s < h.output.count {
        h.output.removeLast(h.output.count - s)
    } else if s > h.output.count {
        h.output.append(contentsOf: repeatElement(0, count: s - h.output.count))
    }
    return noErr
}

/// 変換器の 1 つの属性（UInt32）を設定する。
private func setConverter(_ conv: AudioConverterRef, _ id: AudioConverterPropertyID, _ value: UInt32) -> OSStatus {
    var v = value
    return AudioConverterSetProperty(conv, id, UInt32(MemoryLayout<UInt32>.size), &v)
}

private func openAac(_ s: kura_aac_settings, _ maxOut: Int64) -> Result<AacEncodeHandle, CodecFailure> {
    guard s.rate.isFinite, s.rate >= 1, s.rate <= 1_000_000 else {
        return .failure(.invalid("サンプルレートが正しくありません"))
    }
    guard (1...8).contains(s.channels), s.channel_mask.nonzeroBitCount == Int(s.channels) else {
        return .failure(.invalid("チャンネルの数か並びが正しくありません"))
    }
    guard s.mode <= 3, s.quality <= 127, s.bitrate <= 10_000_000, s.reserved == 0 else {
        return .failure(.invalid("設定が正しくありません"))
    }
    guard let limit = nonNegative(maxOut), limit > 0 else {
        return .failure(.invalid("出力の上限が正しくありません"))
    }
    var dst = AudioStreamBasicDescription(
        mSampleRate: s.rate, mFormatID: kAudioFormatMPEG4AAC, mFormatFlags: 0, mBytesPerPacket: 0,
        mFramesPerPacket: 0, mBytesPerFrame: 0, mChannelsPerFrame: s.channels, mBitsPerChannel: 0, mReserved: 0
    )
    var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    var status = AudioFormatGetProperty(kAudioFormatProperty_FormatInfo, 0, nil, &size, &dst)
    guard status == noErr else {
        return .failure(.unsupported("AAC の形式を作れません（\(describe(status))）"))
    }
    let h = AacEncodeHandle(maxOut: limit, channels: Int(s.channels))
    let client = Unmanaged.passUnretained(h.memory).toOpaque()
    var file: AudioFileID?
    status = AudioFileInitializeWithCallbacks(
        client, encodeReadProc, encodeWriteProc, encodeSizeProc, encodeSetSizeProc, kAudioFileM4AType, &dst,
        AudioFileFlags(rawValue: 0), &file
    )
    guard status == noErr, let file else {
        return .failure(.failed("m4a を作れません（\(describe(status))）"))
    }
    h.file = file
    var ext: ExtAudioFileRef?
    status = ExtAudioFileWrapAudioFileID(file, true, &ext)
    guard status == noErr, let ext else {
        return .failure(.failed("ExtAudioFile で包めません（\(describe(status))）"))
    }
    h.ext = ext
    var client32 = floatPlanar(rate: s.rate, channels: s.channels)
    status = ExtAudioFileSetProperty(
        ext, kExtAudioFileProperty_ClientDataFormat,
        UInt32(MemoryLayout<AudioStreamBasicDescription>.size), &client32
    )
    guard status == noErr else {
        return .failure(.failed("f32 を渡せません（\(describe(status))）"))
    }
    status = setClientLayout(ext, mask: s.channel_mask)
    guard status == noErr else {
        return .failure(.unsupported("チャンネルの並びを渡せません（\(describe(status))）"))
    }
    var conv: AudioConverterRef?
    size = UInt32(MemoryLayout<AudioConverterRef?>.size)
    status = ExtAudioFileGetProperty(ext, kExtAudioFileProperty_AudioConverter, &size, &conv)
    guard status == noErr, let conv else {
        return .failure(.failed("変換器を取れません（\(describe(status))）"))
    }
    status = setConverter(conv, kAudioCodecPropertyBitRateControlMode, s.mode)
    guard status == noErr else {
        return .failure(.unsupported("ビットレートの決め方を設定できません（\(describe(status))）"))
    }
    status = setConverter(conv, kAudioConverterCodecQuality, UInt32(kAudioConverterQuality_Max))
    guard status == noErr else {
        return .failure(.failed("品質を設定できません（\(describe(status))）"))
    }
    if s.mode == UInt32(kAudioCodecBitRateControlMode_Variable) {
        status = setConverter(conv, kAudioCodecPropertySoundQualityForVBR, s.quality)
    } else {
        status = setConverter(conv, kAudioConverterEncodeBitRate, s.bitrate)
    }
    guard status == noErr else {
        return .failure(.unsupported("ビットレートを設定できません（\(describe(status))）"))
    }
    var config: CFPropertyList?
    status = ExtAudioFileSetProperty(
        ext, kExtAudioFileProperty_ConverterConfig, UInt32(MemoryLayout<CFPropertyList?>.size), &config
    )
    guard status == noErr else {
        return .failure(.failed("変換器の設定を反映できません（\(describe(status))）"))
    }
    return .success(h)
}

/// 書き始める。失敗なら NULL を返し、状態の番号を `status` に、文言を `err` に書く。出力が `maxOut` バイトを超えると
/// 以後の書き込みは KURA_LIMIT になる。
@_cdecl("kura_aac_open")
public func kuraAacOpen(
    _ settings: UnsafePointer<kura_aac_settings>?, _ maxOut: Int64, _ status: UnsafeMutablePointer<Int32>?,
    _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> UnsafeMutableRawPointer? {
    guard let settings, let status else {
        writeError("引数が正しくありません", err, errCap)
        return nil
    }
    switch openAac(settings.pointee, maxOut) {
    case .success(let handle):
        status.pointee = kuraOK
        return Unmanaged.passRetained(handle).toOpaque()
    case .failure(let failure):
        status.pointee = failure.status
        writeError(failure.message, err, errCap)
        return nil
    }
}

/// 続きの PCM（planar の f32。`planes[c]` から `frames` 個）を渡す。Swift は呼び出しのあいだだけ読む。
@_cdecl("kura_aac_write")
public func kuraAacWrite(
    _ handle: UnsafeMutableRawPointer?, _ planes: UnsafePointer<UnsafePointer<Float>?>?, _ channels: Int64,
    _ frames: Int64, _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> Int32 {
    guard let handle, let planes else {
        return report(.invalid("引数が正しくありません"), err, errCap)
    }
    let h = Unmanaged<AacEncodeHandle>.fromOpaque(handle).takeUnretainedValue()
    guard let ext = h.ext, !h.finished, let ch = nonNegative(channels), ch == h.channels,
          let n = nonNegative(frames), n > 0, let bytes = mulChecked(n, 4), let byteSize = UInt32(exactly: bytes),
          let count = UInt32(exactly: n)
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
        // ExtAudioFileWrite は入力を読むだけ（AudioBuffer の型が可変のポインタなので包み直す）
        list[c] = AudioBuffer(
            mNumberChannels: 1, mDataByteSize: byteSize, mData: UnsafeMutableRawPointer(mutating: plane)
        )
        c += 1
    }
    let status = ExtAudioFileWrite(ext, count, list.unsafeMutablePointer)
    guard status == noErr else {
        if h.memory.overLimit {
            return report(.limit("書いた m4a が上限（\(h.memory.maxOut) バイト）を超えます"), err, errCap)
        }
        return report(.failed("書き込みに失敗しました（\(describe(status))）"), err, errCap)
    }
    return kuraOK
}

/// 書き終える（エンコーダの残りを出し、m4a を閉じる）。出力の長さを `outLen` に書く。
@_cdecl("kura_aac_finish")
public func kuraAacFinish(
    _ handle: UnsafeMutableRawPointer?, _ outLen: UnsafeMutablePointer<Int64>?, _ cancel: kura_cancel_fn?,
    _ ctx: UnsafeMutableRawPointer?, _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> Int32 {
    guard let handle, let outLen else {
        return report(.invalid("引数が正しくありません"), err, errCap)
    }
    outLen.pointee = 0
    let h = Unmanaged<AacEncodeHandle>.fromOpaque(handle).takeUnretainedValue()
    guard !h.finished, h.ext != nil else {
        return report(.invalid("書きかけの m4a がありません"), err, errCap)
    }
    if canceled(cancel, ctx) {
        return report(.canceled, err, errCap)
    }
    let status = h.closeFiles()
    h.finished = true
    if h.memory.overLimit {
        return report(.limit("書いた m4a が上限（\(h.memory.maxOut) バイト）を超えます"), err, errCap)
    }
    guard status == noErr else {
        return report(.failed("m4a を閉じられません（\(describe(status))）"), err, errCap)
    }
    outLen.pointee = Int64(h.memory.output.count)
    return kuraOK
}

/// 出力を `buf`（`len` = finish が返した長さちょうど）に写す。
@_cdecl("kura_aac_copy")
public func kuraAacCopy(
    _ handle: UnsafeMutableRawPointer?, _ buf: UnsafeMutablePointer<UInt8>?, _ len: Int64,
    _ err: UnsafeMutablePointer<UInt8>?, _ errCap: Int64
) -> Int32 {
    guard let handle, let buf else {
        return report(.invalid("引数が正しくありません"), err, errCap)
    }
    let h = Unmanaged<AacEncodeHandle>.fromOpaque(handle).takeUnretainedValue()
    guard h.finished, let n = nonNegative(len), n == h.memory.output.count else {
        return report(.invalid("出力の長さが合いません"), err, errCap)
    }
    h.memory.output.withUnsafeBytes { src in
        if let base = src.baseAddress, n > 0 {
            UnsafeMutableRawPointer(buf).copyMemory(from: base, byteCount: n)
        }
    }
    return kuraOK
}

/// ハンドルを解放する（Rust の Drop から 1 度だけ）。書きかけの出力も消える。
@_cdecl("kura_aac_close")
public func kuraAacClose(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    Unmanaged<AacEncodeHandle>.fromOpaque(handle).release()
}
