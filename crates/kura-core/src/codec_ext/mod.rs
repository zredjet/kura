//! 外部の道具・別のプロセスを通るコーデック（docs/editing.md §9.1 の `External` の段と、固定の段の SVG を別のプロセスで
//! 読む働き手）。kura-engine の `Decoder`・`Encoder` を実装し、組み立て（kura-mac・kura-devtools・src-tauri）が `CodecSet` に
//! 足す・差し替える。kura-engine は OS・プロセスを知らないので、プロセスを動かす部分はここに置く（`cfg(target_os)` は書かない。
//! プロセスは unix の `std::process` だけで動かす）。

pub mod ffmpeg;
pub mod svg_worker;
