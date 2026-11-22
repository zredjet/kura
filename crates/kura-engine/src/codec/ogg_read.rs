//! Ogg の読み口（RFC 3533。M16 段 2・ADR-217）：最初の Vorbis の論理ストリームのパケットを順に返す。
//!
//! libogg（framing.c）の移植ではなく、正しいファイルで libogg と同じパケット・グラニュール・旗を返す自前の実装。違うのは
//! 壊れたファイルの扱いだけ（libogg は飛ばして読み、ここは断る。ADR-217 (2)）。
//!
//! - 頭が `OggS` でなければ `Unsupported`（Ogg ではない）。最初の BOS のページの組に Vorbis の論理ストリーム（BOS のページの
//!   パケットが識別ヘッダ `\x01vorbis` で始まるもの）がなければ `Unsupported`（Opus・Ogg の FLAC などは次の段へ）。Vorbis が
//!   2 つ以上あれば最初のもの。
//! - 壊れたページは壊れたファイル（`Decode`）：CRC-32 が合わない、版が 0 でない、Vorbis のページの番号が 1 ずつ増えない、
//!   続きの旗と前のページの終わりが合わない、ファイルがページの途中で終わる、パケットが途中で切れたまま終わる、Vorbis の論理
//!   ストリームが終わる前にページの頭（`OggS`）でないものが来る。
//! - パケットの旗とグラニュールは libogg と同じ：BOS のページの最初のパケットに b_o_s、ページで終わる最後のパケットにページの
//!   グラニュール（ほかは −1）、EOS のページで終わる最後のパケットに e_o_s。
//! - 多重化（Theora などと）：Vorbis でない論理ストリームのページは CRC を照らして読み飛ばす。
//! - Vorbis の論理ストリームの終わり（EOS）のあと：残りのページを見て、新しい BOS があれば連結として `Unsupported`（次の段へ。
//!   ADR-217 (3)）。`OggS` でないもの（ID3v1 など）・壊れたページが来たらそこで見るのをやめる。EOS のないまま終わる
//!   ファイル（流しの録音など）は、最後のページまでを読む（libvorbis と同じく終わりの切り取りはしない）。
//!   BOS の組のあとに新しい BOS が来る（EOS の前でも）のも連結。
//! - パケットは [`MAX_PACKET`] まで（それを超えるものは `Limit`）。中止はページごとと、読み口を補うたびに見る。

use std::collections::VecDeque;

use crate::codec::byte_in::Input;
use crate::codec::ogg::crc32;
use crate::codec::symph::Media;
use crate::error::EngineError;
use crate::monitor::Monitor;

/// 1 つのパケットの大きさの上限（バイト。コメントヘッダの埋め込みの画像を見込む）。
pub(crate) const MAX_PACKET: usize = 64 << 20;
/// ページの見出しの大きさ（`OggS` から lacing の数まで）。
const HEADER_LEN: usize = 27;

/// パケット。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OggPacket {
    pub data: Vec<u8>,
    /// そのページで終わる最後のパケットならページのグラニュール、ほかは −1。
    pub granulepos: i64,
    pub bos: bool,
    pub eos: bool,
}

/// 読んだページ。
struct Page {
    flags: u8,
    granule: i64,
    serial: u32,
    seq: u32,
    lacing: Vec<u8>,
    body: Vec<u8>,
}

impl Page {
    fn bos(&self) -> bool {
        self.flags & 0x02 != 0
    }
    fn eos(&self) -> bool {
        self.flags & 0x04 != 0
    }
    fn continued(&self) -> bool {
        self.flags & 0x01 != 0
    }
    /// BOS のページの最初のパケットが Vorbis の識別ヘッダか。
    fn is_vorbis_bos(&self) -> bool {
        self.bos() && self.body.get(..7) == Some(b"\x01vorbis".as_slice())
    }
}

/// 次に来るもの。
enum Next {
    Page(Page),
    /// ページの頭（`OggS`）でないもの。
    NotOgg,
    End,
}

/// 最初の Vorbis の論理ストリームのパケットを読む。
pub(crate) struct OggReader<'a> {
    input: Input<'a>,
    codec: &'static str,
    serial: u32,
    next_seq: u32,
    /// ページをまたいで続いているパケット（`open` のとき）。
    partial: Vec<u8>,
    open: bool,
    queue: VecDeque<OggPacket>,
    /// Vorbis の論理ストリームが終わった（EOS か、ファイルの終わり）。
    done: bool,
    /// 終わりのあとのページを見た。
    tail_checked: bool,
    /// BOS の組のあと（新しい BOS は連結）。
    past_bos_group: bool,
}

impl<'a> OggReader<'a> {
    /// 開いて、最初の BOS の組から Vorbis の論理ストリームを選ぶ（その最初のページのパケットを並べたところまで読む）。
    pub fn open(
        media: &'a mut Media<'_>,
        mon: &'a dyn Monitor,
        codec: &'static str,
    ) -> Result<Self, EngineError> {
        let input = Input::new(media, mon, codec)?;
        let mut r = Self {
            input,
            codec,
            serial: 0,
            next_seq: 0,
            partial: Vec::new(),
            open: false,
            queue: VecDeque::new(),
            done: false,
            tail_checked: false,
            past_bos_group: false,
        };
        let mut chosen: Option<Page> = None;
        loop {
            let page = match r.next_page()? {
                Next::Page(p) => p,
                Next::NotOgg if chosen.is_none() && r.input.pos == 0 => {
                    return Err(r.unsupported("Ogg ではありません"));
                }
                Next::NotOgg => return Err(r.broken("ページの頭がありません")),
                Next::End => break,
            };
            if !page.bos() {
                // BOS の組の終わり。選んだ論理ストリームのページなら並べる
                r.past_bos_group = true;
                let Some(first) = chosen.take() else {
                    return Err(r.unsupported("Ogg の中身が Vorbis ではありません"));
                };
                r.start(first)?;
                if page.serial == r.serial {
                    r.take(page)?;
                }
                return Ok(r);
            }
            if chosen.is_none() && page.is_vorbis_bos() {
                chosen = Some(page);
            }
        }
        // BOS のページだけで終わる（音声のパケットのない Vorbis など）
        let Some(first) = chosen else {
            return Err(r.unsupported("Ogg の中身が Vorbis ではありません"));
        };
        r.start(first)?;
        r.past_bos_group = true;
        r.done = true;
        Ok(r)
    }

    fn broken(&self, why: &str) -> EngineError {
        EngineError::Decode {
            codec: self.codec.to_string(),
            message: format!("壊れた Ogg：{why}"),
        }
    }

    fn unsupported(&self, why: &str) -> EngineError {
        EngineError::Unsupported(format!("{}: {why}", self.codec))
    }

    /// 選んだ論理ストリームの最初のページ。
    fn start(&mut self, first: Page) -> Result<(), EngineError> {
        self.serial = first.serial;
        self.next_seq = first.seq;
        self.take(first)
    }

    /// 次のページを読む（ページの頭でなければ `NotOgg`、ファイルの終わりなら `End`）。CRC が合わない・途中で切れたページは誤り。
    fn next_page(&mut self) -> Result<Next, EngineError> {
        if self.input.mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let head = self.input.peek(4)?;
        if head.is_empty() {
            return Ok(Next::End);
        }
        if head != b"OggS" {
            return Ok(Next::NotOgg);
        }
        let mut h = [0u8; HEADER_LEN];
        self.input.read_exact(&mut h)?;
        if h[4] != 0 {
            return Err(self.broken("版が 0 ではありません"));
        }
        let flags = h[5];
        let granule = i64::from_le_bytes([h[6], h[7], h[8], h[9], h[10], h[11], h[12], h[13]]);
        let serial = u32::from_le_bytes([h[14], h[15], h[16], h[17]]);
        let seq = u32::from_le_bytes([h[18], h[19], h[20], h[21]]);
        let crc = u32::from_le_bytes([h[22], h[23], h[24], h[25]]);
        let mut lacing = vec![0u8; usize::from(h[26])];
        self.input.read_exact(&mut lacing)?;
        let body_len: usize = lacing.iter().map(|l| usize::from(*l)).sum();
        let mut body = vec![0u8; body_len];
        self.input.read_exact(&mut body)?;
        h[22..26].fill(0);
        let mut page = Vec::with_capacity(HEADER_LEN + lacing.len() + body.len());
        page.extend_from_slice(&h);
        page.extend_from_slice(&lacing);
        page.extend_from_slice(&body);
        if crc32(&page) != crc {
            return Err(self.broken("ページの CRC-32 が合いません"));
        }
        Ok(Next::Page(Page {
            flags,
            granule,
            serial,
            seq,
            lacing,
            body,
        }))
    }

    /// 選んだ論理ストリームのページからパケットを組み立てて並べる。
    fn take(&mut self, page: Page) -> Result<(), EngineError> {
        if page.seq != self.next_seq {
            return Err(self.broken("ページの番号が飛んでいます"));
        }
        self.next_seq = page.seq.wrapping_add(1);
        if page.continued() != self.open {
            return Err(self.broken("続きの旗と前のページの終わりが合いません"));
        }
        let first_new = self.queue.len();
        let mut off = 0usize;
        let mut first_packet = true;
        for &l in &page.lacing {
            let l = usize::from(l);
            if self.partial.len() + l > MAX_PACKET {
                return Err(EngineError::Limit("Ogg のパケットが大きすぎます".into()));
            }
            self.partial
                .try_reserve(l)
                .map_err(|_| EngineError::Limit("Ogg のパケットの領域を確保できません".into()))?;
            self.partial.extend_from_slice(&page.body[off..off + l]);
            off += l;
            self.open = true;
            if l < 255 {
                let bos = page.bos() && first_packet && !page.continued();
                first_packet = false;
                self.open = false;
                self.queue.push_back(OggPacket {
                    data: std::mem::take(&mut self.partial),
                    granulepos: -1,
                    bos,
                    eos: false,
                });
            }
        }
        // ページで終わる最後のパケットに、ページのグラニュールと EOS
        if self.queue.len() > first_new
            && let Some(last) = self.queue.back_mut()
        {
            last.granulepos = page.granule;
            last.eos = page.eos();
        }
        if page.eos() {
            if self.open {
                return Err(self.broken("最後のパケットが途中で切れています"));
            }
            self.done = true;
        }
        Ok(())
    }

    /// 次のパケット（なければ None）。
    pub fn next_packet(&mut self) -> Result<Option<OggPacket>, EngineError> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(Some(p));
            }
            if self.done {
                self.check_tail()?;
                return Ok(None);
            }
            match self.next_page()? {
                Next::Page(p) if p.serial == self.serial => {
                    if p.bos() {
                        return Err(
                            self.unsupported("連結した Ogg（同じシリアルの新しい論理ストリーム）")
                        );
                    }
                    self.take(p)?;
                }
                Next::Page(p) => {
                    if p.bos() && self.past_bos_group {
                        return Err(
                            self.unsupported("連結した Ogg（あとから始まる論理ストリーム）")
                        );
                    }
                }
                Next::NotOgg => return Err(self.broken("ページの頭がありません")),
                Next::End => {
                    if self.open {
                        return Err(self.broken("最後のパケットが途中で切れています"));
                    }
                    self.done = true;
                    self.tail_checked = true;
                }
            }
        }
    }

    /// Vorbis の論理ストリームが終わったあとのページに、新しい論理ストリーム（連結）がないか。
    fn check_tail(&mut self) -> Result<(), EngineError> {
        if self.tail_checked {
            return Ok(());
        }
        self.tail_checked = true;
        loop {
            match self.next_page() {
                Ok(Next::Page(p)) => {
                    if p.bos() {
                        return Err(
                            self.unsupported("連結した Ogg（あとから始まる論理ストリーム）")
                        );
                    }
                }
                Ok(Next::NotOgg | Next::End) => return Ok(()),
                Err(EngineError::Canceled) => return Err(EngineError::Canceled),
                // 終わりのあとの壊れたページは値に効かないので、見るのをやめる
                Err(_) => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::codec::ogg::tests::{packets, pages};
    use crate::codec::ogg::{Flush, OggWriter, page_checksum_set};
    use crate::monitor::NoMonitor;

    const ID: &str = "ogg-vorbis-kura-dec/1";

    fn read_all(data: &[u8]) -> Result<Vec<OggPacket>, EngineError> {
        let mut media = Media::Bytes(data);
        let mut r = OggReader::open(&mut media, &NoMonitor, ID)?;
        let mut out = Vec::new();
        while let Some(p) = r.next_packet()? {
            out.push(p);
        }
        Ok(out)
    }

    fn read_all_reader(data: &[u8]) -> Result<Vec<OggPacket>, EngineError> {
        let mut cur = Cursor::new(data.to_vec());
        let mut media = Media::Reader(&mut cur);
        let mut r = OggReader::open(&mut media, &NoMonitor, ID)?;
        let mut out = Vec::new();
        while let Some(p) = r.next_packet()? {
            out.push(p);
        }
        Ok(out)
    }

    /// Vorbis の BOS に見える頭のパケットと、いくつかのパケットを書いた論理ストリーム。
    fn stream(serial: u32, packets: &[Vec<u8>], granules: &[i64]) -> Vec<u8> {
        let mut w = OggWriter::with_serial(serial);
        for (i, p) in packets.iter().enumerate() {
            let flush = if i == 0 { Flush::After } else { Flush::Auto };
            w.packet(p, granules[i], i + 1 == packets.len(), flush)
                .unwrap();
        }
        w.finish()
    }

    fn vorbis_packets(n: usize, seed: u8) -> Vec<Vec<u8>> {
        let mut out = vec![b"\x01vorbis-ident".to_vec()];
        for i in 0..n {
            let len = (i * 997 + usize::from(seed) * 13) % 9_000;
            out.push((0..len).map(|k| (k as u8).wrapping_mul(seed)).collect());
        }
        out
    }

    fn granules(n: usize) -> Vec<i64> {
        (0..n).map(|i| i as i64 * 100).collect()
    }

    /// libogg の規則でページからパケットの旗とグラニュールを付ける（照合の相手。ogg.rs の試験の切り出しを使う）。
    fn expected(bytes: &[u8]) -> Vec<OggPacket> {
        let pg = pages(bytes);
        let pk = packets(&pg);
        let mut out: Vec<OggPacket> = Vec::new();
        for (k, (data, page)) in pk.iter().enumerate() {
            let last_on_page = pk.get(k + 1).is_none_or(|(_, p)| p != page);
            let starts_page = k == 0 || pk[k - 1].1 != *page;
            out.push(OggPacket {
                data: data.clone(),
                granulepos: if last_on_page { pg[*page].granule } else { -1 },
                bos: pg[*page].flags & 0x02 != 0 && starts_page && pg[*page].flags & 0x01 == 0,
                eos: last_on_page && pg[*page].flags & 0x04 != 0,
            });
        }
        out
    }

    #[test]
    fn packets_granules_and_flags_follow_libogg() {
        for (n, seed) in [(1, 3), (5, 7), (40, 11), (200, 29)] {
            let pk = vorbis_packets(n, seed);
            let data = stream(0x1234, &pk, &granules(pk.len()));
            let got = read_all(&data).unwrap();
            assert_eq!(got, expected(&data), "{n}");
            assert_eq!(got.iter().map(|p| p.data.clone()).collect::<Vec<_>>(), pk);
            assert_eq!(read_all_reader(&data).unwrap(), got);
        }
    }

    #[test]
    fn real_encoder_output_and_fixtures() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "ogg") {
                let data = std::fs::read(&p).unwrap();
                match read_all(&data) {
                    Ok(got) => {
                        assert_eq!(got, expected(&data), "{}", p.display());
                        n += 1;
                    }
                    Err(EngineError::Unsupported(_)) => {}
                    Err(e) => panic!("{}: {e}", p.display()),
                }
            }
        }
        assert!(n >= 3, "Ogg Vorbis のフィクスチャが少ない（{n}）");
    }

    #[test]
    fn broken_pages_are_errors() {
        let pk = vorbis_packets(60, 5);
        let data = stream(7, &pk, &granules(pk.len()));
        let pg = pages(&data);
        let page_starts: Vec<usize> = {
            let mut at = 0;
            pg.iter()
                .map(|p| {
                    let s = at;
                    at += 27 + p.lacing.len() + p.body.len();
                    s
                })
                .collect()
        };
        // CRC の合わないページ
        let mut b = data.clone();
        b[page_starts[2] + 40] ^= 1;
        assert!(matches!(read_all(&b), Err(EngineError::Decode { .. })));
        // ページを 1 つ抜く
        let mut b = data[..page_starts[2]].to_vec();
        b.extend_from_slice(&data[page_starts[3]..]);
        assert!(matches!(read_all(&b), Err(EngineError::Decode { .. })));
        // ページの途中で切れる
        assert!(matches!(
            read_all(&data[..page_starts[3] + 30]),
            Err(EngineError::Decode { .. })
        ));
        // ページのあいだの余分なバイト
        let mut b = data[..page_starts[3]].to_vec();
        b.push(0);
        b.extend_from_slice(&data[page_starts[3]..]);
        assert!(matches!(read_all(&b), Err(EngineError::Decode { .. })));
        // 頭が Ogg でない・Vorbis でない
        assert!(matches!(
            read_all(b"RIFF...."),
            Err(EngineError::Unsupported(_))
        ));
        let opus = stream(9, &[b"OpusHead....".to_vec(), vec![1, 2, 3]], &[0, 960]);
        assert!(matches!(read_all(&opus), Err(EngineError::Unsupported(_))));
    }

    #[test]
    fn trailing_data_chains_and_multiplexing() {
        let pk = vorbis_packets(30, 9);
        let data = stream(7, &pk, &granules(pk.len()));
        let want = read_all(&data).unwrap();
        // 終わりのあとの ID3v1 は読み飛ばす
        let mut b = data.clone();
        b.extend_from_slice(b"TAG");
        b.extend_from_slice(&[0u8; 125]);
        assert_eq!(read_all(&b).unwrap(), want);
        // 連結（終わりのあとに新しい論理ストリーム）は次の段へ
        let mut b = data.clone();
        b.extend_from_slice(&stream(8, &vorbis_packets(3, 2), &granules(4)));
        assert!(matches!(read_all(&b), Err(EngineError::Unsupported(_))));
        // 多重化：ほかの論理ストリームのページを挟んでも同じパケット
        let other = stream(
            99,
            &[b"\x80theora".to_vec(), vec![5; 300], vec![6; 70_000]],
            &[0, 1, 2],
        );
        let (vp, op) = (pages(&data), pages(&other));
        let raw = |bytes: &[u8], pages: &[crate::codec::ogg::tests::Page]| -> Vec<Vec<u8>> {
            let mut at = 0;
            pages
                .iter()
                .map(|p| {
                    let len = 27 + p.lacing.len() + p.body.len();
                    let v = bytes[at..at + len].to_vec();
                    at += len;
                    v
                })
                .collect()
        };
        let (vr, or) = (raw(&data, &vp), raw(&other, &op));
        // BOS の組（両方の BOS）→ 交互
        let mut mux = Vec::new();
        mux.extend_from_slice(&or[0]);
        mux.extend_from_slice(&vr[0]);
        let (mut i, mut j) = (1, 1);
        while i < vr.len() || j < or.len() {
            if i < vr.len() {
                mux.extend_from_slice(&vr[i]);
                i += 1;
            }
            if j < or.len() {
                mux.extend_from_slice(&or[j]);
                j += 1;
            }
        }
        assert_eq!(read_all(&mux).unwrap(), want);
        // 版の違うページ・壊した CRC を付け直したページ（形は読めるが番号が飛ぶ）
        let mut b = data.clone();
        b[4] = 1;
        page_checksum_set(&mut b[..27 + vp[0].lacing.len() + vp[0].body.len()]);
        assert!(matches!(read_all(&b), Err(EngineError::Decode { .. })));
    }
}
