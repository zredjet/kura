//! 外部の道具を通るコーデック（docs/editing.md §9.1 の `External` の段）。kura-engine の `Decoder`・`Encoder` を実装し、
//! 組み立て（kura-mac・kura-devtools）が `CodecSet` に足す。kura-engine は OS・プロセスを知らないので、プロセスを動かす
//! 部分はここに置く（`cfg(target_os)` は書かない。プロセスは unix の `std::process` だけで動かす）。

pub mod ffmpeg;
