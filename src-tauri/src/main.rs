/// 大きな確保を OS へすぐ返す（解析・サムネイルのあとにメモリが戻らないのを防ぐ。kura-mac の `alloc`）。
#[global_allocator]
static ALLOC: kura_mac::ReturningAllocator = kura_mac::ReturningAllocator;

fn main() -> std::process::ExitCode {
    kura_app_lib::run()
}
