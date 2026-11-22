#![no_main]
//! 自作の Ogg Vorbis の書き出し：先頭のバイトで設定（品質・レート・チャンネル・長さ）とメタデータ（ループ点・タグ）、
//! 残りを f32 の標本として書き、すべての ogg のデコーダで読み直して長さ・レート・チャンネルを確かめる。書いたコメント
//! （範囲の中のループ点だけ・名前の重複なし）と、読み直したメタデータも確かめる。自前のコードの panic・`Internal` を見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_encode("ogg", data) {
        panic!("{e}");
    }
});
