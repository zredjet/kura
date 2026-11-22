//! ADTS の読み口（ISO/IEC 13818-7 6.2・ISO/IEC 14496-3 1.A.3）：フレームの頭を読み、raw_data_block を順に返す。
//!
//! 頭の見つけ方は symphonia の `AdtsReader`（前の読み手）と同じ：頭ごとに同期の語（`0xFFF` と layer 0）まで 1 バイトずつ
//! 探し、終わりまで（か、頭の途中で終わるまで）に見つからなければ流れの終わり（末尾の ID3v1・APEv2・Lyrics3・0 の詰め物を
//! 読まずに終わる）。ファイルの頭の ID3v2 は、大きさを読んで飛ばす（symphonia は中を探したので、タグの中の同期の語に
//! 似たバイトを頭と取り違えうった）。最初の頭が見つからなければ `Decode`（symphonia も開けない）。1 つのフレームに
//! raw_data_block が 2 つ以上のもの（number_of_raw_data_blocks_in_frame > 0）は `Unsupported`。頭のあとのフレームが途中で
//! 切れていれば `Decode`（symphonia も同じ）。違うのは、形（profile・レート・配置）が途中で変わるものを `Decode` にする所
//! （symphonia は最初の形のまま読み進めた。ADR-221 (6)）。

use crate::codec::byte_in::Input;
use crate::codec::symph::Media;
use crate::error::EngineError;
use crate::monitor::Monitor;

use super::config::{self, Config};
use super::{ADTS_ID, unsupported};

/// フレームの頭。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    profile: u32,
    index: u32,
    layout: u32,
    /// フレームの長さ（頭を含む）。
    len: usize,
    /// 頭の長さ（CRC があれば 9）。
    head: usize,
    blocks: u32,
}

fn header(b: &[u8]) -> Option<Header> {
    if b.len() < 7 {
        return None;
    }
    let sync = (u16::from(b[0]) << 8) | u16::from(b[1]);
    // 同期の語 0xFFF と layer 0（symphonia の `is_sync_word` と同じ印）
    if sync & 0xfff6 != 0xfff0 {
        return None;
    }
    let crc = sync & 1 == 0;
    let profile = u32::from(b[2] >> 6);
    let index = u32::from((b[2] >> 2) & 0xf);
    let layout = (u32::from(b[2] & 1) << 2) | u32::from(b[3] >> 6);
    let len = (usize::from(b[3] & 0x3) << 11) | (usize::from(b[4]) << 3) | usize::from(b[5] >> 5);
    let blocks = u32::from(b[6] & 0x3);
    Some(Header {
        profile,
        index,
        layout,
        len,
        head: if crc { 9 } else { 7 },
        blocks,
    })
}

fn broken(message: &str) -> EngineError {
    EngineError::Decode {
        codec: ADTS_ID.to_string(),
        message: format!("ADTS：{message}"),
    }
}

/// 開いた ADTS。
pub(crate) struct Adts<'a> {
    input: Input<'a>,
    pub cfg: Config,
    first: Header,
    frame: Vec<u8>,
}

/// 同期の語（`0xFFF` と layer 0。symphonia の `is_sync_word` と同じ印）か。
fn is_sync(b0: u8, b1: u8) -> bool {
    ((u16::from(b0) << 8) | u16::from(b1)) & 0xfff6 == 0xfff0
}

/// 次の同期の語の頭まで読み飛ばす（symphonia と同じく 1 バイトずつ）。見つからずに終われば false。
fn sync(input: &mut Input<'_>) -> Result<bool, EngineError> {
    loop {
        if input.mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let w = input.peek(4_096)?;
        if w.len() < 2 {
            return Ok(false);
        }
        let (found, len) = (w.windows(2).position(|p| is_sync(p[0], p[1])), w.len());
        match found {
            Some(i) => {
                input.skip(i as u64)?;
                return Ok(true);
            }
            // 最後の 1 バイトは次の組の頭になりうるので残す（1 バイト以上は必ず進む）
            None => input.skip((len - 1) as u64)?,
        }
    }
}

/// ファイルの頭の ID3v2 のタグ（続けていくつでも）を、大きさ（syncsafe の 28 ビットと、あればフッタの 10 バイト）で飛ばす。
fn skip_id3v2(input: &mut Input<'_>) -> Result<(), EngineError> {
    loop {
        let h = input.peek(10)?;
        let tag = h.len() == 10
            && h.starts_with(b"ID3")
            && h[3] != 0xff
            && h[4] != 0xff
            && h[6..10].iter().all(|b| *b < 0x80);
        if !tag {
            return Ok(());
        }
        let size = h[6..10].iter().fold(0u64, |a, b| (a << 7) | u64::from(*b));
        let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
        input.skip(10 + size + footer)?;
    }
}

/// 同期の語のあとの頭を読む。頭が途中で終われば None（流れの終わり。symphonia と同じ）。
fn read_header(input: &mut Input<'_>) -> Result<Option<Header>, EngineError> {
    if !sync(input)? {
        return Ok(None);
    }
    let head = input.peek(9)?;
    let Some(h) = header(head) else {
        return Ok(None);
    };
    if head.len() < h.head {
        return Ok(None);
    }
    Ok(Some(h))
}

/// 開いて最初の頭から読む形を決める。
pub(crate) fn open<'a>(
    media: &'a mut Media<'_>,
    mon: &'a dyn Monitor,
) -> Result<Adts<'a>, EngineError> {
    let mut input = Input::new(media, mon, ADTS_ID)?;
    skip_id3v2(&mut input)?;
    let Some(first) = read_header(&mut input)? else {
        return Err(broken("ADTS の頭が見つかりません"));
    };
    let cfg = config::from_adts(first.profile, first.index, first.layout)?;
    Ok(Adts {
        input,
        cfg,
        first,
        frame: Vec::new(),
    })
}

impl Adts<'_> {
    /// 次のフレームの raw_data_block（終わりなら None）。
    pub fn next(&mut self) -> Result<Option<&[u8]>, EngineError> {
        if self.input.mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let Some(h) = read_header(&mut self.input)? else {
            return Ok(None);
        };
        if (h.profile, h.index, h.layout)
            != (self.first.profile, self.first.index, self.first.layout)
        {
            return Err(broken(
                "フレームの形（profile・レート・配置）が途中で変わります",
            ));
        }
        if h.blocks != 0 {
            return Err(unsupported(
                "1 つの ADTS のフレームに 2 つ以上の AAC のフレームがあるものは次の段で読みます",
            ));
        }
        if h.len < h.head {
            return Err(broken("フレームの長さが正しくありません"));
        }
        self.frame.clear();
        self.frame.resize(h.len, 0);
        self.input.read_exact(&mut self.frame)?;
        Ok(Some(&self.frame[h.head..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// フレームの数（読めなければ誤り）。
    fn frames(data: &[u8]) -> Result<usize, EngineError> {
        let mut media = Media::Bytes(data);
        let mut a = open(&mut media, &NoMonitor)?;
        let mut n = 0;
        while a.next()?.is_some() {
            n += 1;
        }
        Ok(n)
    }

    /// 前の読み手（symphonia）と同じく、頭の ID3v2・末尾の ID3v1・APEv2・0 の詰め物・フレームのあいだのゴミがあっても
    /// 読み、頭のない入力・最後のフレームが切れたものは `Decode`（見直しの指摘）。
    #[test]
    fn tags_and_padding_around_frames_are_skipped() {
        let base = fixture("aac-lc.aac");
        let n = frames(&base).unwrap();
        assert_eq!(n, 45);
        // ID3v2（syncsafe の大きさ。中に同期の語に似たバイト）を 2 つ
        let mut id3 = b"ID3\x04\x00\x00\x00\x00\x00\x0a".to_vec();
        id3.extend_from_slice(&[0xff, 0xf1, 0x50, 0x80, 0x00, 0x1f, 0xfc, 0, 0, 0]);
        let with_id3 = [id3.clone(), id3, base.clone()].concat();
        assert_eq!(frames(&with_id3).unwrap(), n);
        // 末尾の ID3v1・APEv2 のフッタ・0 の詰め物
        let mut tail = b"TAG".to_vec();
        tail.resize(128, b' ');
        let mut ape = b"APETAGEX".to_vec();
        ape.resize(32, 0);
        for t in [tail, ape, vec![0u8; 3], vec![0u8; 1]] {
            assert_eq!(frames(&[base.clone(), t].concat()).unwrap(), n);
        }
        // フレームのあいだのゴミ（同期の語を含まない）
        let first = (usize::from(base[3] & 0x3) << 11)
            | (usize::from(base[4]) << 3)
            | usize::from(base[5] >> 5);
        let mid = [&base[..first], &[0x12, 0x34, 0x56][..], &base[first..]].concat();
        assert_eq!(frames(&mid).unwrap(), n);
        // 頭のない入力・最後のフレームが切れたもの
        assert!(matches!(
            frames(&[0u8; 64]),
            Err(EngineError::Decode { .. })
        ));
        assert!(matches!(
            frames(&base[..base.len() - 5]),
            Err(EngineError::Decode { .. })
        ));
    }
}
