//! GIF の入れ物の書き手（M16 段 8。ADR-225）。`gif-kura-enc/1`（`codec::gif` の書き出し）と、試験のファイルの作り手
//! （`testfiles` の `FileGen::Gif`）が使う。
//!
//! 書き方は前の gif クレート 0.14.2 の書き出しと同じ意味：GIF89a、論理画面（背景色の番号と縦横比は 0）と大域パレット
//! （色の数を 2 の冪に 0 の色で詰める。色がなくても 2 色の黒を書く）、NETSCAPE2.0 の繰り返し（与えたときだけ）、フレームごとに
//! GCE（dispose・遅延・透明の番号）と画像の記述子（局所パレットも同じく詰める）と LZW（最小の符号の長さは最大の番号から。
//! 255 バイトずつのサブブロック）、終わり。LZW の符号は自作（`lzw::encode`）。

use std::io::Write;

use super::lzw;
use crate::error::EngineError;

/// 書くフレーム。
pub(crate) struct FrameOut<'a> {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    /// 局所パレット（RGB の並び。詰める前）。
    pub palette: Option<&'a [u8]>,
    pub transparent: Option<u8>,
    /// 0 Any・1 Keep・2 Background・3 Previous。
    pub dispose: u8,
    /// 1/100 秒。
    pub delay: u16,
    /// インターレースの印（`indices` はその順に並べてあること）。
    pub interlaced: bool,
    /// 番号（幅 × 高さ。インターレースなら行をその順に）。
    pub indices: &'a [u8],
}

/// 色の数の印（2^(印+1) 色の表）。
fn table_flag(colors: usize) -> u8 {
    (colors.clamp(2, 255).next_power_of_two().trailing_zeros() - 1) as u8
}

/// 色の表（RGB）と、2 の冪に詰める 0 の色を書く。印を返す。
fn color_table(out: &mut Vec<u8>, rgb: &[u8]) -> Result<u8, EngineError> {
    let colors = rgb.len() / 3;
    if colors > 256 {
        return Err(EngineError::Internal("GIF の色は 256 までです".into()));
    }
    let flag = table_flag(colors);
    out.extend_from_slice(&rgb[..colors * 3]);
    out.resize(out.len() + ((2usize << flag) - colors) * 3, 0);
    Ok(flag)
}

/// GIF を組む。`global` は大域パレット（RGB）、`repeat` は NETSCAPE の繰り返し（`Some(0)` は無限、`Some(n)` は n 回の
/// 繰り返し。`None` は書かない）。
pub(crate) struct GifOut {
    bytes: Vec<u8>,
}

impl GifOut {
    /// 頭・論理画面・大域パレット・繰り返しを書く。
    pub(crate) fn new(
        width: u16,
        height: u16,
        global: &[u8],
        repeat: Option<u16>,
    ) -> Result<Self, EngineError> {
        let mut b = Vec::new();
        b.extend_from_slice(b"GIF89a");
        b.extend_from_slice(&width.to_le_bytes());
        b.extend_from_slice(&height.to_le_bytes());
        let at = b.len();
        b.extend_from_slice(&[0, 0, 0]);
        let flag = color_table(&mut b, global)?;
        b[at] = 0x80 | flag | (flag << 4);
        if let Some(n) = repeat {
            b.extend_from_slice(&[0x21, 0xFF, 11]);
            b.extend_from_slice(b"NETSCAPE2.0");
            b.extend_from_slice(&[3, 1]);
            b.extend_from_slice(&n.to_le_bytes());
            b.push(0);
        }
        Ok(GifOut { bytes: b })
    }

    /// フレームを足す。
    pub(crate) fn frame(&mut self, f: &FrameOut<'_>) -> Result<(), EngineError> {
        let n = usize::from(f.width) * usize::from(f.height);
        if f.indices.len() < n {
            return Err(EngineError::Internal(
                "GIF のフレームの番号が足りません".into(),
            ));
        }
        let b = &mut self.bytes;
        // GCE
        let flags = u8::from(f.transparent.is_some()) | ((f.dispose & 7) << 2);
        b.extend_from_slice(&[0x21, 0xF9, 4, flags]);
        b.extend_from_slice(&f.delay.to_le_bytes());
        b.extend_from_slice(&[f.transparent.unwrap_or(0), 0]);
        // 画像の記述子
        b.push(0x2C);
        for v in [f.left, f.top, f.width, f.height] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let at = b.len();
        b.push(if f.interlaced { 0x40 } else { 0 });
        if let Some(p) = f.palette {
            let flag = color_table(b, p)?;
            b[at] |= 0x80 | flag;
        }
        // LZW
        let data = &f.indices[..n];
        let min = lzw::min_size_for(data);
        b.push(min as u8);
        let mut z = Vec::with_capacity(n / 2 + 16);
        if !lzw::encode(data, min, &mut z) {
            return Err(EngineError::Internal("LZW に書けない番号です".into()));
        }
        for block in z.chunks(255) {
            b.push(block.len() as u8);
            b.extend_from_slice(block);
        }
        b.push(0);
        Ok(())
    }

    /// 終わりを書いてバイト列を返す。
    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.bytes.push(0x3B);
        self.bytes
    }

    /// 終わりを書いて `w` へ書く。書いたバイト数を返す。
    pub(crate) fn write_to(self, w: &mut dyn Write) -> Result<u64, EngineError> {
        let bytes = self.finish();
        w.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }
}

#[cfg(test)]
mod tests;
