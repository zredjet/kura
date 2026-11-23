#![no_main]
//! PNG・APNG の書き出し（M16 段 7。自作の `codec::png_write`）を、入力から作った値で 3 つの段に回す。自前のコードの panic・
//! 書けない値・読み直して変わる値を見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_png_write(data) {
        panic!("{e}");
    }
});
