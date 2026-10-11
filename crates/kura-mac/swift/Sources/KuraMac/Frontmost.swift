import AppKit

/// 前面のアプリのプロセス ID（なければ -1）。ログイン用の Chrome が前面かを確かめるのに使う。
@_cdecl("kura_frontmost_pid")
public func kuraFrontmostPid() -> Int32 {
    NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1
}

/// macOS のスクリーンショットと同じシャッター音（なければ「Grab」）を鳴らす。`ok` が false ならエラーの音。
@_cdecl("kura_play_capture_sound")
public func kuraPlayCaptureSound(_ ok: Bool) {
    let dir = "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/system/"
    DispatchQueue.main.async {
        if ok {
            let sound =
                NSSound(contentsOfFile: dir + "Screen Capture.aif", byReference: true)
                ?? NSSound(named: "Grab")
            sound?.play()
        } else {
            NSSound(named: "Basso")?.play()
        }
    }
}
