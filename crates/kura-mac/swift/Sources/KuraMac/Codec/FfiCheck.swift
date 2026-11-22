// kura_mac_ffi.h の関数のプロトタイプ（C から取り込んだ宣言）と、@_cdecl で定義した関数の型が同じことを、コンパイル時に
// 照らす（食い違えばこのファイルがコンパイルできない）。Rust の宣言（bridge.rs）とヘッダは、abi.rs のテストが照らす。
//
// 型はヘッダの宣言で決め（1 つ目の文）、定義をその型として受ける（2 つ目の文）。1 つの式にすると、関数の型の変換
// （引数の Optional を外すなど）で両方に合う型が選ばれて、食い違いを見逃す。
import KuraMacC

/// ヘッダの宣言の型。
private struct HeaderSignature<T> {
    init(_: T) {}
    /// 定義がヘッダの宣言と同じ型か（違えばコンパイルできない）。
    func check(_: T) {}
}

/// 型を照らすだけで、呼ばない。
private func checkFfiSignatures() {
    let ffiAbi = HeaderSignature(kura_ffi_abi)
    ffiAbi.check(kuraFfiAbi)
    let ffiLayout = HeaderSignature(kura_ffi_layout)
    ffiLayout.check(kuraFfiLayout)
    let debugStats = HeaderSignature(kura_debug_stats)
    debugStats.check(kuraDebugStats)
    let callCancel = HeaderSignature(kura_ffi_call_cancel)
    callCancel.check(kuraFfiCallCancel)

    let imgOpen = HeaderSignature(kura_img_open)
    imgOpen.check(kuraImgOpen)
    let imgBegin = HeaderSignature(kura_img_begin)
    imgBegin.check(kuraImgBegin)
    let imgReadRows = HeaderSignature(kura_img_read_rows)
    imgReadRows.check(kuraImgReadRows)
    let imgCopyIcc = HeaderSignature(kura_img_copy_icc)
    imgCopyIcc.check(kuraImgCopyIcc)
    let imgClose = HeaderSignature(kura_img_close)
    imgClose.check(kuraImgClose)

    let audOpen = HeaderSignature(kura_aud_open)
    audOpen.check(kuraAudOpen)
    let audRead = HeaderSignature(kura_aud_read)
    audRead.check(kuraAudRead)
    let audClose = HeaderSignature(kura_aud_close)
    audClose.check(kuraAudClose)

    let aacOpen = HeaderSignature(kura_aac_open)
    aacOpen.check(kuraAacOpen)
    let aacWrite = HeaderSignature(kura_aac_write)
    aacWrite.check(kuraAacWrite)
    let aacFinish = HeaderSignature(kura_aac_finish)
    aacFinish.check(kuraAacFinish)
    let aacCopy = HeaderSignature(kura_aac_copy)
    aacCopy.check(kuraAacCopy)
    let aacClose = HeaderSignature(kura_aac_close)
    aacClose.check(kuraAacClose)

    let codecCaps = HeaderSignature(kura_codec_caps)
    codecCaps.check(kuraCodecCaps)
}
