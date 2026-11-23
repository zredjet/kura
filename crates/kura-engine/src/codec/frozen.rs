//! 試験だけ：読み取りの結果を順に畳んで凍結する（M16 段 10。ADR-227）。
//!
//! symphonia を外す前は、自作の読み手（FLAC・ALAC・AAC・Vorbis）の値を、乱数で作ったファイル・壊したファイルで前の symphonia
//! の読み手と 1 つずつ照らしていた。外したあとは、照らし終えた実装の結果（読めたものは値のダイジェスト、読めないものは印）を
//! 畳んだダイジェストを試験に書いて凍結し、読む値・読める範囲が変わればそこで気づく（読む値を変えるならデコーダの版を上げる。
//! docs/editing.md §9.1）。

use crate::error::EngineError;
use crate::value::Value;

/// 畳んだ結果。
#[derive(Default)]
pub(crate) struct Log {
    /// 読めた数。
    pub ok: usize,
    /// 読めなかった数。
    pub err: usize,
    bytes: Vec<u8>,
}

impl Log {
    /// 結果を 1 つ畳む。
    pub fn push(&mut self, r: &Result<Value, EngineError>) {
        match r {
            Ok(v) => {
                self.ok += 1;
                self.bytes.push(1);
                let d = v.digest().map(|d| d.to_hex()).unwrap_or_default();
                self.bytes.extend_from_slice(d.as_bytes());
            }
            Err(_) => {
                self.err += 1;
                self.bytes.push(0);
            }
        }
    }

    /// 畳んだダイジェスト。
    pub fn digest(&self) -> String {
        crate::digest::Hasher::new("kura-test-frozen-reads-v1")
            .bytes_with_len(&self.bytes)
            .finish()
            .to_hex()
    }
}
