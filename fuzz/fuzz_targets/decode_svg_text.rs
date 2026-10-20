#![no_main]
//! 文字を含む SVG を、小さなフォント 2 つを渡した svg のデコーダで読む（文字のレイアウト・代わりのフォント・レイアウトの
//! 手間の見積もり。M11 の検証 S1）。自前のコードの panic・形と中身の食い違いを見つける。遅い入力は libFuzzer の時間切れで見る。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_svg_text(data) {
        panic!("{e}");
    }
});
