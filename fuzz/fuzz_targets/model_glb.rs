#![no_main]
//! glb（3D モデル。M11）の情報を読み、焼き込み、2 つの向きで小さく描く。自前のコードの panic・`Internal`・情報と焼き込みの
//! 食い違い・描いた画像の形の食い違いを見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_model(data) {
        panic!("{e}");
    }
});
