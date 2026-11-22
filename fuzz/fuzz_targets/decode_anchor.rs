#![no_main]
//! アンカー（PNG・APNG・f32 の WAV ＋私用のチャンク。段 9）のデコーダを、上限を絞って読む。自前のコードの panic・
//! 形と中身の食い違いを見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_decode("anchor", data) {
        panic!("{e}");
    }
});
