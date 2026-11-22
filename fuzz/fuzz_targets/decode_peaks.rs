#![no_main]
//! 波形の段のバイナリ（kura-peaks。解析 audio.peaks の正規の形）を読む。自前のコードの panic・読めたのに正規の形に
//! 戻らないものを見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_peaks(data) {
        panic!("{e}");
    }
});
