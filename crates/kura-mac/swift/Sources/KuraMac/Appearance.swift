import AppKit

/// 「アクセシビリティ > ディスプレイ > 透明度を下げる」がオンなら true。
@_cdecl("kura_reduce_transparency")
public func kuraReduceTransparency() -> Bool {
    NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency
}
