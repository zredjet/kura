//! macOS 向けのときだけ Swift パッケージ（swift/）をビルドしてリンクする。
//! それ以外の OS では何もしない（`kura-mac` は空の実装になる）。

/// Swift 側の最小 OS（ADR-01：macOS 26 以降）。
const MACOS_MIN_VERSION: &str = "26.0";

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        swift_rs::SwiftLinker::new(MACOS_MIN_VERSION)
            .with_package("KuraMac", "swift")
            .link();
        // Swift の並行処理ランタイムは OS の /usr/lib/swift にある（このクレートのテスト用。
        // 実行ファイル側の build.rs にも同じ指定がいる）
        #[allow(clippy::print_stdout)]
        {
            println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        }
    }
}
