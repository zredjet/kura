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
        .target(
            name: "KuraMac",
            path: "Sources/KuraMac",
            // AVFoundation の非同期 API を同期の C 関数から待つため、Swift 5 の言語モードにする
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
