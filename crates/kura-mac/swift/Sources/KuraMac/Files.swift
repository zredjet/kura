import AppKit

/// Finder でファイル・フォルダを選んだ状態で見せる（書き出し・バックアップの「Finder で表示」）。ないものは false。
@_cdecl("kura_reveal_in_finder")
public func kuraRevealInFinder(_ path: UnsafePointer<CChar>) -> Bool {
    let url = URL(fileURLWithPath: String(cString: path))
    guard FileManager.default.fileExists(atPath: url.path) else {
        return false
    }
    DispatchQueue.main.async {
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }
    return true
}

/// `path` のあるボリュームの容量と、使える空き（大事なデータ用。Finder の「使用可能」と同じ）。読めなければ false。
@_cdecl("kura_volume_capacity")
public func kuraVolumeCapacity(
    _ path: UnsafePointer<CChar>, _ total: UnsafeMutablePointer<Int64>,
    _ available: UnsafeMutablePointer<Int64>
) -> Bool {
    let url = URL(fileURLWithPath: String(cString: path))
    guard
        let values = try? url.resourceValues(forKeys: [
            .volumeTotalCapacityKey, .volumeAvailableCapacityForImportantUsageKey,
        ]),
        let totalBytes = values.volumeTotalCapacity,
        let availableBytes = values.volumeAvailableCapacityForImportantUsage
    else {
        return false
    }
    total.pointee = Int64(totalBytes)
    available.pointee = availableBytes
    return true
}

/// 物理メモリのバイト数（`ProcessInfo.physicalMemory`。編集のメモリの予算の既定に使う。docs/editing.md §12）。
@_cdecl("kura_physical_memory")
public func kuraPhysicalMemory() -> UInt64 {
    return ProcessInfo.processInfo.physicalMemory
}
