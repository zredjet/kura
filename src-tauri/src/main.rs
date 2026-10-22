/// 大きな確保を OS へすぐ返す（解析・サムネイルのあとにメモリが戻らないのを防ぐ。kura-mac の `alloc`）。
#[global_allocator]
static ALLOC: kura_mac::ReturningAllocator = kura_mac::ReturningAllocator;

fn main() -> std::process::ExitCode {
    // SVG の働き手として起動された（`<アプリ> --kura-svg-worker`）：Tauri を始めずに、標準入力の頼みを 1 つ処理して終わる
    // （決定 23。kura-core の `codec_ext::svg_worker`）
    if kura_core::codec_ext::svg_worker::is_worker_invocation() {
        return kura_core::codec_ext::svg_worker::serve(&kura_mac::pinned_options());
    }
    kura_app_lib::run()
}
