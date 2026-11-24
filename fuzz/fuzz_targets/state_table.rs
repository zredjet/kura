#![no_main]
//! 状態の表のバイナリ（kura-state。状態を運ぶ操作が申告する内部用の解析の正規の形。M17 段 1）を読む。自前のコードの
//! panic・読めたのに正規の形に戻らないものを見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_state_table(data) {
        panic!("{e}");
    }
});
