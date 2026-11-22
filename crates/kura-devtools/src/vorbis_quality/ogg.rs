//! Ogg Vorbis のファイルを読む補助：ページとパケットに分け（ビットレートをヘッダと音声に分けるため）、
//! 識別・コメントのヘッダの値を取り出し、lewton（独立したデコーダ）で読む。
//!
//! lewton は低い層の口（ヘッダとパケット）だけを使い、ページはここで読む（`kura-engine/tests/ogg_vorbis.rs` と同じ。
//! lewton の `OggStreamReader` は末尾の切り取りを誤ることがあるため）。最後のページのグラニュールで長さを切る。

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Ogg の 1 ページ。
#[derive(Debug, Clone)]
struct Page {
    flags: u8,
    granule: i64,
    serial: u32,
    lacing: Vec<u8>,
    body_at: usize,
}

/// 1 つのパケット。
#[derive(Debug, Clone)]
pub struct Packet {
    pub data: Vec<u8>,
}

/// ファイルの形とバイトの内訳。
#[derive(Debug, Clone, PartialEq)]
pub struct OggStats {
    /// ファイル全体のバイト。
    pub file_bytes: u64,
    /// 最初の 3 パケット（識別・コメント・setup）のバイト。
    pub header_bytes: u64,
    /// 音声のパケット（4 つ目から）のバイトの和（ページの見出し・lacing を含まない）。
    pub audio_bytes: u64,
    /// 音声のパケットの数。
    pub audio_packets: usize,
    /// ページの数。
    pub pages: usize,
    /// 音声のパケットのバイトを含むページの数。
    pub audio_pages: usize,
    /// 最後のページのグラニュール。
    pub last_granule: i64,
    /// 最後のページに EOS が立っているか。
    pub eos: bool,
    /// 識別ヘッダのチャンネルの数・サンプルレート・公称のビットレート（bps。0 以下は書かれていない）。
    pub channels: u8,
    pub rate: u32,
    pub bitrate_nominal: i32,
    /// コメントヘッダの vendor。
    pub vendor: String,
}

/// CRC32（多項式 0x04C11DB7、反射なし、初期値 0）。
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn le_i64(b: &[u8]) -> i64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    i64::from_le_bytes(a)
}

/// ページに分ける（形と CRC を確かめる。論理ストリームは 1 つだけ）。
fn pages(bytes: &[u8]) -> Result<Vec<Page>, String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let h = &bytes[at..];
        if h.len() < 27 || &h[0..4] != b"OggS" || h[4] != 0 {
            return Err(format!("{at} バイト目が Ogg のページではありません"));
        }
        let n = usize::from(h[26]);
        if h.len() < 27 + n {
            return Err(format!("{at} バイト目のページが途中で切れています"));
        }
        let lacing = h[27..27 + n].to_vec();
        let body_len: usize = lacing.iter().map(|l| usize::from(*l)).sum();
        let total = 27 + n + body_len;
        if h.len() < total {
            return Err(format!("{at} バイト目のページが途中で切れています"));
        }
        let mut page = h[..total].to_vec();
        page[22..26].fill(0);
        if crc32(&page) != le_u32(&h[22..26]) {
            return Err(format!("{at} バイト目のページの CRC が合いません"));
        }
        out.push(Page {
            flags: h[5],
            granule: le_i64(&h[6..14]),
            serial: le_u32(&h[14..18]),
            lacing,
            body_at: at + 27 + n,
        });
        at += total;
    }
    if let Some(first) = out.first()
        && out.iter().any(|p| p.serial != first.serial)
    {
        return Err("論理ストリームが 2 つ以上あります（対象外）".into());
    }
    Ok(out)
}

/// パケットに組み立て、内訳を数える。
pub fn parse(bytes: &[u8]) -> Result<(Vec<Packet>, OggStats), String> {
    let pages = pages(bytes)?;
    let last = pages.last().ok_or("ページがありません")?;
    let mut packets: Vec<Packet> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut audio_pages = 0usize;
    for p in &pages {
        let mut off = p.body_at;
        let mut has_audio = false;
        for &l in &p.lacing {
            let l = usize::from(l);
            if l > 0 && packets.len() >= 3 {
                has_audio = true;
            }
            cur.extend_from_slice(&bytes[off..off + l]);
            off += l;
            if l < 255 {
                packets.push(Packet {
                    data: std::mem::take(&mut cur),
                });
            }
        }
        if !cur.is_empty() && packets.len() >= 3 {
            has_audio = true;
        }
        if has_audio {
            audio_pages += 1;
        }
    }
    if !cur.is_empty() {
        return Err("最後のパケットが終わっていません".into());
    }
    if packets.len() < 3 {
        return Err("Vorbis のヘッダが 3 つありません".into());
    }
    let ident = &packets[0].data;
    if ident.len() < 30 || &ident[0..7] != b"\x01vorbis" {
        return Err("識別ヘッダが Vorbis ではありません".into());
    }
    let comment = &packets[1].data;
    if comment.len() < 11 || &comment[0..7] != b"\x03vorbis" {
        return Err("コメントヘッダが Vorbis ではありません".into());
    }
    let vlen = le_u32(&comment[7..11]) as usize;
    let vendor = comment
        .get(11..11 + vlen)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    let size = |ps: &[Packet]| ps.iter().map(|p| p.data.len() as u64).sum::<u64>();
    let stats = OggStats {
        file_bytes: bytes.len() as u64,
        header_bytes: size(&packets[..3]),
        audio_bytes: size(&packets[3..]),
        audio_packets: packets.len() - 3,
        pages: pages.len(),
        audio_pages,
        last_granule: last.granule,
        eos: last.flags & 0x04 != 0,
        channels: ident[11],
        rate: le_u32(&ident[12..16]),
        bitrate_nominal: le_u32(&ident[20..24]) as i32,
        vendor,
    };
    Ok((packets, stats))
}

/// Vorbis のチャンネル `i` は kura の `planes[ORDER[n - 1][i]]`（Vorbis I §4.3.9。kura-engine の `vorbis::ORDER` と同じ）。
const ORDER: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[0, 2, 1],
    &[0, 1, 2, 3],
    &[0, 2, 1, 3, 4],
    &[0, 2, 1, 4, 5, 3],
    &[0, 2, 1, 5, 6, 4, 3],
    &[0, 2, 1, 6, 7, 4, 5, 3],
];

/// lewton の結果：kura の並びのチャンネルと、切る前の長さ。
pub struct Lewton {
    pub planes: Vec<Vec<f32>>,
    /// 最後のグラニュールで切る前の長さ。
    pub raw_frames: usize,
}

/// lewton で読む（最後のページのグラニュールで長さを切る。チャンネルは kura の並びに直す）。
pub fn decode_lewton(packets: &[Packet], stats: &OggStats) -> Result<Lewton, String> {
    let run = || -> Result<Lewton, String> {
        let e = |e: lewton::header::HeaderReadError| format!("lewton: {e:?}");
        let ident = lewton::header::read_header_ident(&packets[0].data).map_err(e)?;
        let setup = lewton::header::read_header_setup(
            &packets[2].data,
            ident.audio_channels,
            (ident.blocksize_0, ident.blocksize_1),
        )
        .map_err(e)?;
        let n = usize::from(ident.audio_channels);
        let mut planes: Vec<Vec<f32>> = vec![Vec::new(); n];
        let mut pwr = lewton::audio::PreviousWindowRight::new();
        for p in &packets[3..] {
            let out: Vec<Vec<f32>> =
                lewton::audio::read_audio_packet_generic(&ident, &setup, &p.data, &mut pwr)
                    .map_err(|e| format!("lewton: {e:?}"))?;
            for (dst, src) in planes.iter_mut().zip(out) {
                dst.extend_from_slice(&src);
            }
        }
        let raw_frames = planes.first().map_or(0, Vec::len);
        if stats.last_granule >= 0 {
            let keep = usize::try_from(stats.last_granule).unwrap_or(usize::MAX);
            for p in &mut planes {
                p.truncate(keep);
            }
        }
        let planes = match ORDER.get(n.wrapping_sub(1)) {
            Some(order) if n > 2 => {
                let mut out = vec![Vec::new(); n];
                for (i, p) in planes.into_iter().enumerate() {
                    out[order[i]] = p;
                }
                out
            }
            _ => planes,
        };
        Ok(Lewton { planes, raw_frames })
    };
    catch_unwind(AssertUnwindSafe(run))
        .unwrap_or_else(|_| Err("lewton が読み取りの途中で止まりました".into()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// テスト用の書き手：パケットをページに詰める（`per_page` 個ずつ。グラニュールはページの番号）。
    pub(crate) fn mux(packets: &[Vec<u8>], per_page: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let chunks: Vec<&[Vec<u8>]> = packets.chunks(per_page).collect();
        for (seq, chunk) in chunks.iter().enumerate() {
            let mut lacing = Vec::new();
            let mut body = Vec::new();
            for p in *chunk {
                let mut left = p.len();
                loop {
                    let l = left.min(255);
                    lacing.push(l as u8);
                    left -= l;
                    if l < 255 {
                        break;
                    }
                }
                body.extend_from_slice(p);
            }
            let mut page = b"OggS\0".to_vec();
            let mut flags = 0u8;
            if seq == 0 {
                flags |= 0x02;
            }
            if seq + 1 == chunks.len() {
                flags |= 0x04;
            }
            page.push(flags);
            page.extend_from_slice(&(seq as i64 * 100).to_le_bytes());
            page.extend_from_slice(&7u32.to_le_bytes());
            page.extend_from_slice(&(seq as u32).to_le_bytes());
            page.extend_from_slice(&[0; 4]);
            page.push(lacing.len() as u8);
            page.extend_from_slice(&lacing);
            page.extend_from_slice(&body);
            let crc = crc32(&page);
            page[22..26].copy_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&page);
        }
        out
    }

    fn headers() -> Vec<Vec<u8>> {
        let mut ident = b"\x01vorbis".to_vec();
        ident.extend_from_slice(&0u32.to_le_bytes());
        ident.push(2);
        ident.extend_from_slice(&48_000u32.to_le_bytes());
        ident.extend_from_slice(&0i32.to_le_bytes());
        ident.extend_from_slice(&112_000i32.to_le_bytes());
        ident.extend_from_slice(&0i32.to_le_bytes());
        ident.push(0xb8);
        ident.push(1);
        let mut comment = b"\x03vorbis".to_vec();
        comment.extend_from_slice(&4u32.to_le_bytes());
        comment.extend_from_slice(b"kura");
        comment.extend_from_slice(&0u32.to_le_bytes());
        comment.push(1);
        vec![ident, comment, b"\x05vorbis-setup".to_vec()]
    }

    /// CRC の検査値と、ページ・パケットの内訳（255 バイトの倍数のパケットとページをまたぐパケットを含む）。
    #[test]
    fn splits_headers_and_audio() {
        assert_eq!(crc32(b"123456789"), 0x89A1_897F);
        let mut packets = headers();
        packets.push(vec![1; 510]);
        packets.push(vec![2; 3]);
        packets.push(vec![3; 700]);
        let bytes = mux(&packets, 2);
        let (pk, st) = parse(&bytes).unwrap();
        assert_eq!(pk.len(), 6);
        assert_eq!(pk[3].data.len(), 510);
        assert_eq!(pk[5].data, vec![3; 700]);
        assert_eq!(st.header_bytes, (30 + 20 + 13) as u64);
        assert_eq!(st.audio_bytes, 510 + 3 + 700);
        assert_eq!(st.audio_packets, 3);
        assert_eq!(st.pages, 3);
        // 2 ページ目（setup と 510 バイト）と 3 ページ目
        assert_eq!(st.audio_pages, 2);
        assert_eq!(st.last_granule, 200);
        assert!(st.eos);
        assert_eq!(
            (st.channels, st.rate, st.bitrate_nominal),
            (2, 48_000, 112_000)
        );
        assert_eq!(st.vendor, "kura");
        assert_eq!(st.file_bytes, bytes.len() as u64);
        // 壊れたもの
        let mut bad = bytes.clone();
        bad[40] ^= 1;
        assert!(parse(&bad).is_err());
        assert!(parse(&bytes[..bytes.len() - 1]).is_err());
        assert!(parse(b"RIFF").is_err());
    }
}
