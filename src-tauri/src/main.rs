/// 大きな確保を OS へすぐ返す（解析・サムネイルのあとにメモリが戻らないのを防ぐ。kura-mac の `alloc`）。SVG の働き手の
/// ときだけ、確保の量を数えて上限を付ける（ADR-164 (4)）。
#[global_allocator]
static ALLOC: kura_mac::ReturningAllocator = kura_mac::ReturningAllocator::new();

fn main() -> std::process::ExitCode {
    // SVG の働き手として起動された（`<アプリ> --kura-svg-worker`）：Tauri を始めずに、標準入力の頼みを 1 つ処理して終わる
    // （決定 23。kura-core の `codec_ext::svg_worker`）。確保器の目印で確保の上限を付ける
    if kura_core::codec_ext::svg_worker::is_worker_invocation() {
        return kura_core::codec_ext::svg_worker::serve(&kura_mac::pinned_options(), ALLOC.meter());
    }
    kura_app_lib::run()
}
