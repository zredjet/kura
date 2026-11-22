// Platform のコーデックの C の口の共通部品：エラーの書き込み・数の変換・ハンドルの数・ABI の照合。
//
// 書き方の規則（Swift の trap と NSException は Rust の catch_unwind で受け止められず、プロセスごと落ちる）：
// `!`・`try!`・`as!` を使わない。算術はあふれを確かめる（`*Checked`・`multipliedReportingOverflow`）。
// `Int(x)`・`UInt32(x)` は範囲の外で trap するので、`exactly:` で変換する。ポインタと長さは Rust を信じず確かめる。
// 例外を投げる Objective-C の API（NSArray の添字・JSONSerialization・AVAssetReader など）を使わない。
import Foundation
import KuraMacC
import Synchronization

let kuraOK = Int32(KURA_OK)
let kuraUnsupported = Int32(KURA_UNSUPPORTED)
let kuraFailed = Int32(KURA_FAILED)
let kuraCanceled = Int32(KURA_CANCELED)
let kuraInvalid = Int32(KURA_INVALID)
let kuraMismatch = Int32(KURA_MISMATCH)
let kuraLimit = Int32(KURA_LIMIT)

/// 失敗（状態の番号と文言）。
struct CodecFailure: Error {
    let status: Int32
    let message: String

    static func unsupported(_ m: String) -> CodecFailure { CodecFailure(status: kuraUnsupported, message: m) }
    static func failed(_ m: String) -> CodecFailure { CodecFailure(status: kuraFailed, message: m) }
    static func invalid(_ m: String) -> CodecFailure { CodecFailure(status: kuraInvalid, message: m) }
    static func mismatch(_ m: String) -> CodecFailure { CodecFailure(status: kuraMismatch, message: m) }
    static func limit(_ m: String) -> CodecFailure { CodecFailure(status: kuraLimit, message: m) }
    static let canceled = CodecFailure(status: kuraCanceled, message: "中止しました")
}

/// 文言を Rust の領域（`cap` バイト）に NUL 終端の UTF-8 で書く。文字の境目で切り詰める。
func writeError(_ message: String, _ buf: UnsafeMutablePointer<UInt8>?, _ cap: Int64) {
    guard let buf, cap > 0, let cap = Int(exactly: cap) else { return }
    let bytes = Array(message.utf8)
    var end = min(bytes.count, cap - 1)
    // 続きのバイト（10xxxxxx）の途中で切らない
    while end > 0, end < bytes.count, (bytes[end] & 0xC0) == 0x80 {
        end -= 1
    }
    var i = 0
    while i < end {
        buf[i] = bytes[i]
        i += 1
    }
    buf[end] = 0
}

/// 失敗を書いて状態の番号を返す。
func report(_ failure: CodecFailure, _ err: UnsafeMutablePointer<UInt8>?, _ cap: Int64) -> Int32 {
    writeError(failure.message, err, cap)
    return failure.status
}

/// OSStatus を読める形にする（FourCC なら '…' も）。
func describe(_ status: OSStatus) -> String {
    let raw = UInt32(bitPattern: status)
    let bytes = [UInt8(truncatingIfNeeded: raw >> 24), UInt8(truncatingIfNeeded: raw >> 16),
                 UInt8(truncatingIfNeeded: raw >> 8), UInt8(truncatingIfNeeded: raw)]
    if bytes.allSatisfy({ $0 >= 0x20 && $0 < 0x7F }), let text = String(bytes: bytes, encoding: .ascii) {
        return "\(status) '\(text)'"
    }
    return "\(status)"
}

/// FourCC を読める形にする。
func fourCC(_ v: UInt32) -> String {
    describe(OSStatus(bitPattern: v))
}

/// あふれを確かめる掛け算（あふれたら nil）。
func mulChecked(_ a: Int, _ b: Int) -> Int? {
    let (r, o) = a.multipliedReportingOverflow(by: b)
    return o ? nil : r
}

/// あふれを確かめる足し算（あふれたら nil）。
func addChecked(_ a: Int, _ b: Int) -> Int? {
    let (r, o) = a.addingReportingOverflow(b)
    return o ? nil : r
}

/// 0 以上の長さ・位置（Int64 → Int）。負は nil。
func nonNegative(_ v: Int64) -> Int? {
    guard v >= 0 else { return nil }
    return Int(exactly: v)
}

/// 中止を尋ねる（コールバックがなければ中止しない）。
func canceled(_ cancel: kura_cancel_fn?, _ ctx: UnsafeMutableRawPointer?) -> Bool {
    guard let cancel else { return false }
    return cancel(ctx)
}

/// 生きているハンドルの数（漏れの確認。テストで 0 に戻ることを確かめる）と、画像を展開した回数。
let liveHandles = Atomic<Int>(0)
let imageDecodes = Atomic<Int>(0)

func handleOpened() {
    liveHandles.wrappingAdd(1, ordering: .relaxed)
}

func handleClosed() {
    liveHandles.wrappingSubtract(1, ordering: .relaxed)
}

/// ABI の版（Rust の bridge::ABI と照らす）。
@_cdecl("kura_ffi_abi")
public func kuraFfiAbi() -> UInt32 {
    UInt32(KURA_FFI_ABI)
}

/// 構造体の大きさと各項目の位置を、Swift の見え方で `out`（`cap` 個）に書き、書いた数を返す（Rust の #[repr(C)] と照らす）。
@_cdecl("kura_ffi_layout")
public func kuraFfiLayout(_ out: UnsafeMutablePointer<Int64>?, _ cap: Int64) -> Int64 {
    var values: [Int] = []
    func add(_ v: Int?) { values.append(v ?? -1) }
    add(MemoryLayout<kura_image_info>.size)
    add(MemoryLayout<kura_image_info>.offset(of: \.width))
    add(MemoryLayout<kura_image_info>.offset(of: \.height))
    add(MemoryLayout<kura_image_info>.offset(of: \.frames))
    add(MemoryLayout<kura_image_info>.offset(of: \.orientation))
    add(MemoryLayout<kura_image_info>.offset(of: \.pixel_format))
    add(MemoryLayout<kura_image_info>.offset(of: \.alpha))
    add(MemoryLayout<kura_image_info>.offset(of: \.loop_count))
    add(MemoryLayout<kura_image_info>.offset(of: \.flags))
    add(MemoryLayout<kura_image_info>.offset(of: \.icc_len))
    add(MemoryLayout<kura_image_info>.offset(of: \.exif_len))
    add(MemoryLayout<kura_image_frame>.size)
    add(MemoryLayout<kura_image_frame>.offset(of: \.width))
    add(MemoryLayout<kura_image_frame>.offset(of: \.height))
    add(MemoryLayout<kura_image_frame>.offset(of: \.pixel_format))
    add(MemoryLayout<kura_image_frame>.offset(of: \.alpha))
    add(MemoryLayout<kura_image_frame>.offset(of: \.duration_ms))
    add(MemoryLayout<kura_image_frame>.offset(of: \.flags))
    add(MemoryLayout<kura_audio_info>.size)
    add(MemoryLayout<kura_audio_info>.offset(of: \.rate))
    add(MemoryLayout<kura_audio_info>.offset(of: \.channels))
    add(MemoryLayout<kura_audio_info>.offset(of: \.format_id))
    add(MemoryLayout<kura_audio_info>.offset(of: \.frames))
    add(MemoryLayout<kura_audio_info>.offset(of: \.priming))
    add(MemoryLayout<kura_audio_info>.offset(of: \.remainder))
    add(MemoryLayout<kura_audio_info>.offset(of: \.bits))
    add(MemoryLayout<kura_audio_info>.offset(of: \.flags))
    add(MemoryLayout<kura_aac_settings>.size)
    add(MemoryLayout<kura_aac_settings>.offset(of: \.rate))
    add(MemoryLayout<kura_aac_settings>.offset(of: \.channels))
    add(MemoryLayout<kura_aac_settings>.offset(of: \.channel_mask))
    add(MemoryLayout<kura_aac_settings>.offset(of: \.mode))
    add(MemoryLayout<kura_aac_settings>.offset(of: \.bitrate))
    add(MemoryLayout<kura_aac_settings>.offset(of: \.quality))
    add(MemoryLayout<kura_aac_settings>.offset(of: \.reserved))
    guard let out, let cap = nonNegative(cap) else { return Int64(values.count) }
    var i = 0
    while i < values.count, i < cap {
        out[i] = Int64(values[i])
        i += 1
    }
    return Int64(values.count)
}

/// 調べ物の数：[生きているハンドル, 画像を展開した回数]。書いた数を返す。
@_cdecl("kura_debug_stats")
public func kuraDebugStats(_ out: UnsafeMutablePointer<Int64>?, _ cap: Int64) -> Int64 {
    let values = [liveHandles.load(ordering: .relaxed), imageDecodes.load(ordering: .relaxed)]
    guard let out, let cap = nonNegative(cap) else { return Int64(values.count) }
    var i = 0
    while i < values.count, i < cap {
        out[i] = Int64(values[i])
        i += 1
    }
    return Int64(values.count)
}

/// 中止のコールバックを `times` 回呼び、真を返した回数を返す（Rust の中止の受け渡しの照合）。
@_cdecl("kura_ffi_call_cancel")
public func kuraFfiCallCancel(_ cancel: kura_cancel_fn?, _ ctx: UnsafeMutableRawPointer?, _ times: Int64) -> Int64 {
    var yes: Int64 = 0
    var i: Int64 = 0
    while i < times {
        if canceled(cancel, ctx) {
            yes &+= 1
        }
        i &+= 1
    }
    return yes
}
