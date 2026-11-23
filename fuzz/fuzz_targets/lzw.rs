#![no_main]
//! GIF の LZW（M16 段 8。自作の `codec::lzw`）の展開と往復を回す。自前のコードの panic・進まない読み・元に戻らない圧縮を
//! 見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_lzw(data) {
        panic!("{e}");
    }
});
