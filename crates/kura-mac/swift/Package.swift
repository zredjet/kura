// swift-tools-version:6.2
// Kura の Swift ヘルパー。Rust からは swift-rs 経由で C 互換関数（@_cdecl）として呼ぶ。
import PackageDescription

let package = Package(
    name: "KuraMac",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "KuraMac", type: .static, targets: ["KuraMac"]),
    ],
    targets: [
        .target(name: "KuraMac", path: "Sources/KuraMac"),
    ]
)
