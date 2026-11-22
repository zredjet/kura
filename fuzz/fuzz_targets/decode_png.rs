#![no_main]
//! png のデコーダ（旧版を含む）を、上限を絞って読む。自前のコードの panic・形と中身の食い違いを見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_decode("png", data) {
        panic!("{e}");
    }
});
