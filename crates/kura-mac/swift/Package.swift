// swift-tools-version:6.2
// Kura の Swift ヘルパー。Rust からは C 互換関数（@_cdecl）として呼ぶ。
import PackageDescription

let package = Package(
    name: "KuraMac",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "KuraMac", type: .static, targets: ["KuraMac"]),
    ],
    targets: [
        // Platform のコーデックの C の口の型と関数のプロトタイプ（include/kura_mac_ffi.h。Swift と Rust の正本）
        .target(name: "KuraMacC", path: "Sources/KuraMacC"),
        .target(
            name: "KuraMac",
            dependencies: ["KuraMacC"],
            path: "Sources/KuraMac",
            // AVFoundation の非同期 API を同期の C 関数から待つため、Swift 5 の言語モードにする
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
