#![no_main]
//! 音声の源（M13 段 1）：WAV・AIFF・アンカーを、チャンクごとにシークして読む遅延の値として開き、全チャンクを読む。
//! 自前のコードの panic・`decode` と違う値・`decode` との食い違いを見つける。

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = kura_engine::testing::fuzz_audio_source(data) {
        panic!("{e}");
    }
});
