#![no_main]
//! レシピのファイル（`.kurarecipe`。M14 段 1）を読み、書いて読み直し、決まった頭の文書と入力の形と素材の設定で当てる。
//! 自前のコードの panic と `Internal` を見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_recipe(data) {
        panic!("{e}");
    }
});
