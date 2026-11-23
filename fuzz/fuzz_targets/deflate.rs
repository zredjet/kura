#![no_main]
//! deflate の圧縮（M16 段 7。自作の `codec::zlib::Deflate`）を 3 つの段で回す。自前のコードの panic・元に戻らない圧縮・
//! 入力を渡す切れ目で変わるバイト列を見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_deflate(data) {
        panic!("{e}");
    }
});
