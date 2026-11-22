#![no_main]
//! 文書の JSON を読み、形を確かめ、計画を作る。自前のコードの panic を見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_document(data) {
        panic!("{e}");
    }
});
