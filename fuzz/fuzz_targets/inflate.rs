#![no_main]
//! zlib・deflate の展開（M16 段 6。自作の `codec::zlib`）を 3 つの受け付けで回す。自前のコードの panic・上限を超えた出力・
//! 進まない読み・受け付けの違いでの値の食い違いを見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_inflate(data) {
        panic!("{e}");
    }
});
