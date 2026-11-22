#![no_main]
//! 文書の部品（M12 の段 2。`kura_engine::doc_parts`。DB から読み直す外の本文）をつなぎ、文書なら分けてつなぎ直す。自前のコードの
//! panic・`Internal`・往復しない（分けてつなぐと戻らない・つないだ文書を分け直すと別の根になる）を見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_doc_parts(data) {
        panic!("{e}");
    }
});
