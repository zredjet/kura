fn main() {
    // kura-mac の Swift ヘルパーが使う Swift の並行処理ランタイムは OS の /usr/lib/swift にある
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        #[allow(clippy::print_stdout)]
        {
            println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        }
    }
    tauri_build::build();
}
