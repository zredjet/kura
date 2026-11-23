//! 読み手が音声でない部分で持つ量を、ファイルの頭だけを見て安く数える（M13 段 2 の検証の直し。[`Held`]）。
//!
//! 読み手は、開くときにタグ・箱・頭のパケットを読み込み、読み終えるまで持ち続ける（Ogg の頭のパケットのコメントと
//! METADATA_BLOCK_PICTURE、m4a のサンプルの表と 7 つのキーのタグ。M16 段 1 の自作の FLAC の読み手は VORBIS_COMMENT のタグだけで、
//! PICTURE・APPLICATION・SEEKTABLE・CUESHEET は読み飛ばす。前の symphonia の読み手（段 10 で外した）は m4a の moov と ilst の
//! covr も持った）。
//! 形を読む間・流して読む間の作業領域の見積もり（`CodecSet::shape_path_staged`・`stage_stream_work_bytes`）に、ここで数えた量を
//! 足す。数えるのは箱・ページの頭と、数を決める数バイトだけ（中身は読まずにシークで飛ばす。Ogg の頭のページは CRC を照らす
//! ために読むが、確保は 1 ページ分だけ）。
//!
//! 量は実測（数える確保器。realloc は写す間に前と新しい領域を両方持つと数える。tests/stream_held.rs・kura-devtools の
//! mem_smoke）に合わせた上からの見積もり：
//! - コメント・タグの中身：[`TEXT_FACTOR`] 倍（Ogg はパケットを組み立てる写し、文字はタグとその写しを持つ）。
//! - コメント・タグ 1 つあたり [`ITEM_COST`]（タグの構造と小さな確保）。数はコメントの数の欄を、残りの長さで入りうる数
//!   （1 つ 4 バイト以上）で抑える。
//! - m4a（[`mp4`]）：サンプルの表・moof の中身とタグの値（[`mp4`] の説明）。FLAC の VORBIS_COMMENT は箱の 1 倍（読む間だけ）と
//!   タグ（読めない UTF-8 を置き換えると最大 3 倍）で、コメントの倍率の内。
//! - 読み手そのものの緩衝（[`READER`]）。
//! - Vorbis の codebook の表：自作の読み手の `vorbis::dec::codebook::setup_table_bytes` で、setup の静的な表は開いている間
//!   （形を読む間も）、復号の表は流して読む間だけ。setup が [`SETUP_MAX`] を超えて組み立てられないときは、読み口が受け付ける
//!   上限（`min(max_value_bytes, 256 MiB)`）とみなす。
//!
//! 数えられない頭（壊れた箱・違う形式）は、そこまでの量を返す（読み手はそこで断るか、読み飛ばしたところで止まる）。
//! I/O の誤りだけを返す。

use std::io::SeekFrom;

use crate::codec::ReadSeek;
use crate::error::EngineError;
use crate::limits::Limits;

/// 読み手が音声でない部分で持つ量（バイト。[`crate::codec::Decoder::held_bytes`]）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    /// 開いた読み手が持ち続ける量（形を読む間も、流して読む間も）。
    pub open: u64,
    /// 流して読む間だけ、それに足して持つ量（デコーダの表。Vorbis の codebook のうち、`stream_work_bytes` が見込む分を
    /// 超える分）。
    pub decode: u64,
}

/// 自作の Vorbis の読み手（`ogg-vorbis-kura-dec/1`。`vorbis::dec::codebook::setup_table_bytes`）の codebook の表の量
/// （開いている間、流して読む間）。setup の静的な表は開いている間（[`Held::open`]）、復号の表は流して読む間だけ
/// （[`Held::decode`]）。setup（組み立てられなければ None）を数えられなければ、上限 `worst` の全部を開いている間に持つと
/// みなす（読み手は表の量の和を上限で断るので、持つのは上限まで）。
fn vorbis_tables(setup: Option<&[u8]>, worst: u64) -> (u64, u64) {
    match setup {
        Some(setup) => crate::codec::vorbis::dec::codebook::setup_table_bytes(setup)
            .map_or((0, 0), |t| (t.setup.min(worst), t.decode.min(worst))),
        None => (worst, 0),
    }
}

/// 読み手そのものの緩衝（読み口の緩衝・Ogg のページ・パケットの組み立て）。
pub(crate) const READER: u64 = 1 << 20;
/// コメント・タグ 1 つあたりの量。
pub(crate) const ITEM_COST: u64 = 256;
/// Vorbis コメント（FLAC・Ogg）の中身の倍率。
pub(crate) const TEXT_FACTOR: u64 = 6;
/// Vorbis の setup を組み立てて表の大きさを見積もる上限（これを超える setup は表を上限とみなす）。正しく作られた
/// ファイルの setup は数 KB。
pub(crate) const SETUP_MAX: usize = 1 << 20;
/// Ogg のパケットの頭として残す長さ（コメントの数の欄を探す）。
const PACKET_HEAD: usize = 64 << 10;
/// 読み飛ばしをシークでなく読んで捨てる長さの上限（小さな箱が続くときに、緩衝を捨てるシークを繰り返さない）。
const SKIP_BY_READ: u64 = 64 << 10;

/// 持ち続ける量と、読む間だけ持つ量（箱を 1 つずつ読むので、その最大）。
#[derive(Default)]
struct Peak {
    keep: u64,
    extra: u64,
}

impl Peak {
    fn add(&mut self, keep: u64, extra: u64) {
        self.keep = self.keep.saturating_add(keep);
        self.extra = self.extra.max(extra);
    }

    fn total(&self) -> u64 {
        READER.saturating_add(self.keep).saturating_add(self.extra)
    }
}

/// 位置を数えながら読む口（小さな読み飛ばしは読んで捨てる）。
struct Walk<'a> {
    src: &'a mut dyn ReadSeek,
    pos: u64,
    len: u64,
}

impl<'a> Walk<'a> {
    fn new(src: &'a mut dyn ReadSeek) -> Result<Self, EngineError> {
        let len = src.seek(SeekFrom::End(0))?;
        src.seek(SeekFrom::Start(0))?;
        Ok(Self { src, pos: 0, len })
    }

    fn left(&self) -> u64 {
        self.len.saturating_sub(self.pos)
    }

    /// `buf` を埋める。足りなければ偽（位置は終わり）。
    fn fill(&mut self, buf: &mut [u8]) -> Result<bool, EngineError> {
        if (buf.len() as u64) > self.left() {
            self.goto(self.len)?;
            return Ok(false);
        }
        self.src.read_exact(buf)?;
        self.pos += buf.len() as u64;
        Ok(true)
    }

    fn u32_be(&mut self) -> Result<Option<u32>, EngineError> {
        let mut b = [0u8; 4];
        Ok(self.fill(&mut b)?.then(|| u32::from_be_bytes(b)))
    }

    fn u32_le(&mut self) -> Result<Option<u32>, EngineError> {
        let mut b = [0u8; 4];
        Ok(self.fill(&mut b)?.then(|| u32::from_le_bytes(b)))
    }

    /// `to` へ進む（後ろへは戻らない。終わりを超えるなら終わりへ）。
    fn goto(&mut self, to: u64) -> Result<(), EngineError> {
        let to = to.min(self.len);
        if to <= self.pos {
            if to < self.pos {
                self.src.seek(SeekFrom::Start(to))?;
                self.pos = to;
            }
            return Ok(());
        }
        let n = to - self.pos;
        if n <= SKIP_BY_READ {
            let mut scratch = [0u8; 4096];
            let mut rest = n;
            while rest > 0 {
                let k = rest.min(scratch.len() as u64) as usize;
                self.src.read_exact(&mut scratch[..k])?;
                rest -= k as u64;
            }
        } else {
            self.src.seek(SeekFrom::Start(to))?;
        }
        self.pos = to;
        Ok(())
    }
}

/// Vorbis コメント（ベンダーの長さ・ベンダー・コメントの数・コメントの列。リトルエンディアン）の数を、`len` バイトの
/// 中身に入りうる数で抑えて返す。`w` はコメントの頭（ベンダーの長さの前）にあり、`end` は中身の終わり。
fn comment_count(w: &mut Walk<'_>, end: u64) -> Result<u64, EngineError> {
    let Some(vendor) = w.u32_le()? else {
        return Ok(0);
    };
    let at = w.pos.saturating_add(u64::from(vendor));
    if at.saturating_add(4) > end {
        return Ok(0);
    }
    w.goto(at)?;
    let Some(count) = w.u32_le()? else {
        return Ok(0);
    };
    Ok(u64::from(count).min(end.saturating_sub(w.pos) / 4))
}

/// FLAC（自作の読み手 `codec::flac_read` がメタデータの箱を読む順に、`last` の箱まで）。持つのは VORBIS_COMMENT（箱を
/// ちょうどの大きさの 1 つの領域に読み、タグに写す）だけで、ほかの箱は読み飛ばす（M16 段 1。前の symphonia の読み手は
/// PICTURE・APPLICATION・SEEKTABLE・CUESHEET も持っていた）。
pub(crate) fn flac(src: &mut dyn ReadSeek) -> Result<Held, EngineError> {
    let mut w = Walk::new(src)?;
    let mut peak = Peak::default();
    let mut marker = [0u8; 4];
    if !w.fill(&mut marker)? || &marker != b"fLaC" {
        return Ok(Held {
            open: peak.total(),
            decode: 0,
        });
    }
    while let Some(h) = w.u32_be()? {
        let last = h & 0x8000_0000 != 0;
        let kind = (h >> 24) & 0x7f;
        let len = u64::from(h & 0x00ff_ffff);
        let body = w.pos;
        let end = body.saturating_add(len);
        if end > w.len {
            break;
        }
        // VORBIS_COMMENT（箱を 1 つの領域に読んでタグに写す）。ほかの箱は読み飛ばす（持たない）
        if kind == 4 {
            let n = comment_count(&mut w, end)?;
            peak.add(
                len.saturating_mul(TEXT_FACTOR)
                    .saturating_add(n.saturating_add(1).saturating_mul(ITEM_COST)),
                0,
            );
        }
        w.goto(end)?;
        if last {
            break;
        }
    }
    Ok(Held {
        open: peak.total(),
        decode: 0,
    })
}

/// 自作の読み手が読むタグの 1 つの値の上限（UTF-16 の 2 倍と頭）。
const TAG_VALUE: u64 = 2 * 4_096 + 16;
/// moof の大きさに対する、自作の読み手が持つ trun の量の倍率（trun 1 つ 16 バイト以上に `Run` 48 バイト、列の伸び 2 倍、
/// サンプルごとの値の写し、読み込む領域）。
const MOOF_FACTOR: u64 = 10;

/// m4a（自作の読み手 `codec::mp4_read`。ALAC・AAC）：サンプルの表（stts・stsc・stsz・stco・co64）を 3 倍（読み込む領域と、
/// 位置を u64 にした表）、stz2 を 9 倍（4 ビットの項目を u32 にすると 8 倍）。どのトラックを選ぶかは数えないので、すべての
/// trak の分。7 つのキーのタグの値は 1 つ `min(3 × 長さ, 4,096)`（不正な UTF-8 の置き換えで 3 倍まで伸び、4,096 を
/// 超えるものは持たない）と [`ITEM_COST`]、読む間の写しに [`TAG_VALUE`] の 2 つ分。mdat の前に続く moof の大きさの和の
/// 最大を [`MOOF_FACTOR`] 倍。covr・ほかのタグ・mdat は読まない。
pub(crate) fn mp4(src: &mut dyn ReadSeek) -> Result<Held, EngineError> {
    let mut w = Walk::new(src)?;
    let len = w.len;
    let (mut tables, mut tags) = (0u64, 0u64);
    let (mut run, mut run_peak) = (0u64, 0u64);
    let mut top = Boxes::new(0, len);
    while let Some((kind, s, e)) = top.next(&mut w)? {
        match &kind {
            b"moov" => {
                let mut moov = Boxes::new(s, e);
                while let Some((k, ms, me)) = moov.next(&mut w)? {
                    match &k {
                        b"trak" => tables = tables.saturating_add(trak_tables(&mut w, ms, me)?),
                        b"udta" => {
                            let mut udta = Boxes::new(ms, me);
                            while let Some((k, us, ue)) = udta.next(&mut w)? {
                                if &k == b"meta" {
                                    tags = tags.saturating_add(kura_tags(&mut w, us, ue)?);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"meta" => tags = tags.saturating_add(kura_tags(&mut w, s, e)?),
            b"moof" => {
                run = run.saturating_add(e - s);
                run_peak = run_peak.max(run);
            }
            b"mdat" => run = 0,
            _ => {}
        }
    }
    Ok(Held {
        open: READER
            .saturating_add(tables)
            .saturating_add(tags)
            // タグを読む間の写し（生の値と置き換えた文字列。タグがあるときだけ）
            .saturating_add(if tags > 0 {
                TAG_VALUE.saturating_mul(2)
            } else {
                0
            })
            .saturating_add(run_peak.saturating_mul(MOOF_FACTOR)),
        decode: 0,
    })
}

/// trak の中のサンプルの表を持つ量（stz2 は 9 倍、ほかは 3 倍）。
fn trak_tables(w: &mut Walk<'_>, s: u64, e: u64) -> Result<u64, EngineError> {
    let mut total = 0u64;
    let mut stack = vec![(s, e)];
    while let Some((s, e)) = stack.pop() {
        let mut b = Boxes::new(s, e);
        while let Some((k, cs, ce)) = b.next(w)? {
            match &k {
                b"mdia" | b"minf" | b"stbl" => stack.push((cs, ce)),
                b"stts" | b"stsc" | b"stsz" | b"stco" | b"co64" => {
                    total = total.saturating_add((ce - cs).saturating_mul(3));
                }
                b"stz2" => total = total.saturating_add((ce - cs).saturating_mul(9)),
                _ => {}
            }
        }
    }
    Ok(total)
}

/// meta の中の ilst の、自作の読み手が読む 7 つのキーのタグの量。
fn kura_tags(w: &mut Walk<'_>, s: u64, e: u64) -> Result<u64, EngineError> {
    let mut total = 0u64;
    let mut meta = Boxes::new(s.saturating_add(4).min(e), e);
    while let Some((k, is, ie)) = meta.next(w)? {
        if &k != b"ilst" {
            continue;
        }
        let mut ilst = Boxes::new(is, ie);
        while let Some((tag, ts, te)) = ilst.next(w)? {
            // 7 つのキーと、自由形式（iTunSMPB。AAC の priming）
            if !crate::codec::mp4_read::TAG_KEYS
                .iter()
                .any(|(k, _)| *k == tag)
                && &tag != b"----"
            {
                continue;
            }
            let mut parts = Boxes::new(ts, te);
            while let Some((_, ds, de)) = parts.next(w)? {
                total = total
                    .saturating_add((de - ds).saturating_mul(3).min(4_096))
                    .saturating_add(ITEM_COST);
            }
        }
    }
    Ok(total)
}

/// `[at, end)` の箱を頭だけ読んでたどる（symphonia の `AtomIterator` と同じ形：大きさ 0 は範囲の終わりまで、1 は 64 ビット。
/// 壊れた箱で止まる）。
struct Boxes {
    at: u64,
    end: u64,
}

impl Boxes {
    fn new(at: u64, end: u64) -> Self {
        Self { at, end }
    }

    fn next(&mut self, w: &mut Walk<'_>) -> Result<Option<([u8; 4], u64, u64)>, EngineError> {
        let at = self.at;
        if at.saturating_add(8) > self.end {
            return Ok(None);
        }
        w.goto(at)?;
        let mut h = [0u8; 8];
        if !w.fill(&mut h)? {
            self.at = self.end;
            return Ok(None);
        }
        let size = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let kind = [h[4], h[5], h[6], h[7]];
        let (head, size) = match size {
            1 => {
                let mut v = [0u8; 8];
                if at.saturating_add(16) > self.end || !w.fill(&mut v)? {
                    self.at = self.end;
                    return Ok(None);
                }
                (16, u64::from_be_bytes(v))
            }
            0 => (8, self.end - at),
            n => (8, n),
        };
        if size < head || at.saturating_add(size) > self.end {
            self.at = self.end;
            return Ok(None);
        }
        self.at = at + size;
        Ok(Some((kind, at + head, at + size)))
    }
}

/// Ogg の論理ストリームの対応（0 番のパケットの頭で決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mapping {
    Vorbis,
    Opus,
    Flac,
    Other,
}

/// 論理ストリームごとのパケットの組み立て（頭だけを残す）。
struct Stream {
    serial: u32,
    mapping: Mapping,
    /// 今のパケットの番号（0 から）。
    index: u32,
    /// 今のパケットのここまでの長さ。
    len: u64,
    /// 今のパケットの頭（[`PACKET_HEAD`] まで。Vorbis の setup は [`SETUP_MAX`] まで）。
    head: Vec<u8>,
}

/// Ogg の頭で数えたもの。
#[derive(Default)]
struct OggCount {
    /// 最初の Vorbis の論理ストリーム（自作の読み手が読むもの）。
    vorbis_serial: Option<u32>,
    /// その setup を数え終えた（自作の読み手は、ここまでの頭を読んで開く）。
    vorbis_setup_done: bool,
    comments: u64,
    /// Vorbis の codebook の表のうち、開いている間に持つ量の見積もりの最大。
    open_tables: u64,
    /// Vorbis の codebook の表のうち、流して読む間だけ持つ量の見積もりの最大。
    tables: u64,
}

impl Stream {
    fn cap(&self) -> usize {
        if self.mapping == Mapping::Vorbis && self.index == 2 {
            SETUP_MAX
        } else {
            PACKET_HEAD
        }
    }

    /// パケットの断片を足す。
    fn push(&mut self, part: &[u8]) {
        let room = self.cap().saturating_sub(self.head.len());
        self.head.extend_from_slice(&part[..part.len().min(room)]);
        self.len = self.len.saturating_add(part.len() as u64);
    }

    /// パケットが終わった：種類を決め、コメントの数・Vorbis の表を数える。
    fn finish(&mut self, count: &mut OggCount, limits: &Limits) {
        let head = std::mem::take(&mut self.head);
        let len = self.len;
        if self.index == 0 {
            // 自作の読み手（`codec::ogg_read`・`vorbis::dec::info::unpack_info`）は識別ヘッダの長さを見ない
            self.mapping = if head.starts_with(b"\x01vorbis") {
                count.vorbis_serial.get_or_insert(self.serial);
                Mapping::Vorbis
            } else if head.starts_with(b"OpusHead") {
                Mapping::Opus
            } else if head.starts_with(b"\x7fFLAC") {
                Mapping::Flac
            } else {
                Mapping::Other
            };
        } else {
            let comment_at = match self.mapping {
                Mapping::Vorbis if head.starts_with(b"\x03vorbis") => Some(7),
                Mapping::Opus if head.starts_with(b"OpusTags") => Some(8),
                Mapping::Flac if head.first().is_some_and(|b| b & 0x7f == 4) => Some(4),
                _ => None,
            };
            if let Some(at) = comment_at {
                count.comments = count
                    .comments
                    .saturating_add(head_comment_count(&head, at, len));
            }
            if self.mapping == Mapping::Vorbis && head.starts_with(b"\x05vorbis") {
                let worst = limits.max_value_bytes.min(256 << 20);
                let (open, decode) = if (head.len() as u64) < len {
                    vorbis_tables(None, worst)
                } else {
                    vorbis_tables(Some(&head), worst)
                };
                count.open_tables = count.open_tables.max(open);
                count.tables = count.tables.max(decode);
                if count.vorbis_serial == Some(self.serial) {
                    count.vorbis_setup_done = true;
                }
            }
        }
        self.index = self.index.saturating_add(1);
        self.len = 0;
    }
}

/// パケットの頭 `head`（全体の長さ `len`）の `at` から始まる Vorbis コメントの数（入りうる数で抑える）。頭に数の欄が
/// なければ、入りうる数の上限。
fn head_comment_count(head: &[u8], at: usize, len: u64) -> u64 {
    let read = |i: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            head.get(i..i.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let fit = |from: u64| len.saturating_sub(from) / 4;
    let Some(vendor) = read(at) else {
        return 0;
    };
    let count_at = (at as u64)
        .saturating_add(4)
        .saturating_add(u64::from(vendor));
    match usize::try_from(count_at).ok().and_then(read) {
        Some(n) => u64::from(n).min(fit(count_at.saturating_add(4))),
        None => fit(count_at),
    }
}

/// Ogg（自作の読み手 `codec::ogg_read` が頭のパケットを組み立てるまでに読むページ：最初の Vorbis の setup を読み終える
/// まで。BOS の組が終わっても Vorbis がなければそこまで。捕まえの印 `OggS` を探して、合わないページは読み飛ばす）。Vorbis の
/// codebook の表は、開いている間の分を `open` に、流して読む間だけの分を `decode` に足す（デコーダが `stream_work_bytes` の
/// 見込みの分を `decode` から引く）。
pub(crate) fn ogg(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Held, EngineError> {
    let mut w = Walk::new(src)?;
    let mut bytes = 0u64;
    let mut count = OggCount::default();
    let mut streams: Vec<Stream> = Vec::new();
    let mut page = Vec::new();
    loop {
        // 捕まえの印を探す
        if !sync(&mut w)? {
            break;
        }
        let at = w.pos;
        let mut h = [0u8; 27];
        if !w.fill(&mut h)? {
            break;
        }
        let nseg = usize::from(h[26]);
        let mut lacing = [0u8; 255];
        if !w.fill(&mut lacing[..nseg])? {
            break;
        }
        let body: usize = lacing[..nseg].iter().map(|&l| usize::from(l)).sum();
        page.clear();
        page.extend_from_slice(&h);
        page.extend_from_slice(&lacing[..nseg]);
        let head_len = page.len();
        page.resize(head_len + body, 0);
        if !w.fill(&mut page[head_len..])? {
            break;
        }
        let crc = u32::from_le_bytes([h[22], h[23], h[24], h[25]]);
        page[22..26].fill(0);
        if h[4] != 0 || crate::codec::ogg::crc32(&page) != crc {
            // 合わないページ：印の次から探し直す
            w.goto(at + 4)?;
            continue;
        }
        bytes = bytes.saturating_add(page.len() as u64);
        let serial = u32::from_le_bytes([h[14], h[15], h[16], h[17]]);
        let first = h[5] & 0x02 != 0;
        let i = match streams.iter().position(|s| s.serial == serial) {
            Some(i) => i,
            None if first => {
                streams.push(Stream {
                    serial,
                    mapping: Mapping::Other,
                    index: 0,
                    len: 0,
                    head: Vec::new(),
                });
                streams.len() - 1
            }
            None => usize::MAX,
        };
        if let Some(s) = streams.get_mut(i) {
            let mut off = head_len;
            for &l in &lacing[..nseg] {
                let l = usize::from(l);
                s.push(&page[off..off + l]);
                off += l;
                if l < 255 {
                    s.finish(&mut count, limits);
                }
            }
        }
        // 最初の Vorbis の setup を読み終えるまで（グラニュールは見ない）。BOS の組が終わっても Vorbis がなければ、読み手は
        // そこで断る
        if count.vorbis_setup_done || (!first && count.vorbis_serial.is_none()) {
            break;
        }
    }
    let open = READER
        .saturating_add(bytes.saturating_mul(TEXT_FACTOR))
        .saturating_add(count.comments.saturating_mul(ITEM_COST))
        .saturating_add(count.open_tables);
    Ok(Held {
        open,
        decode: count.tables,
    })
}

/// 次の捕まえの印 `OggS` の頭へ進む（なければ偽。終わりまで読む）。64 KiB ずつ読んで探す。
fn sync(w: &mut Walk<'_>) -> Result<bool, EngineError> {
    let mut buf = vec![0u8; 64 << 10];
    let mut kept = 0usize;
    let mut base = w.pos;
    loop {
        let n = usize::try_from(w.left().min((buf.len() - kept) as u64)).unwrap_or(0);
        if n == 0 {
            return Ok(false);
        }
        w.src.read_exact(&mut buf[kept..kept + n])?;
        w.pos += n as u64;
        let filled = kept + n;
        if let Some(i) = buf[..filled].windows(4).position(|x| x == b"OggS") {
            w.goto(base + i as u64)?;
            return Ok(true);
        }
        // 印が読んだ境をまたぐことがあるので、末尾の 3 バイトを残す
        let k = filled.min(3);
        buf.copy_within(filled - k..filled, 0);
        base += (filled - k) as u64;
        kept = k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use rand_chacha::rand_core::Rng;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(name),
        )
        .unwrap()
    }

    /// 画像やタグのない小さなファイルは、読み手の緩衝とわずかな箱だけ。
    #[test]
    fn plain_files_hold_little() {
        let limits = Limits::default();
        for (name, f) in [
            (
                "flac-16.flac",
                flac as fn(&mut dyn ReadSeek) -> Result<Held, EngineError>,
            ),
            ("alac-16.m4a", mp4),
            ("aac-lc-ffmpeg.m4a", mp4),
        ] {
            let h = f(&mut Cursor::new(fixture(name))).unwrap();
            assert!(
                h.open >= READER && h.open < READER + (256 << 10),
                "{name}: {h:?}"
            );
            assert_eq!(h.decode, 0, "{name}");
        }
        let h = ogg(&mut Cursor::new(fixture("ogg-vorbis-ffmpeg.ogg")), &limits).unwrap();
        assert!(h.open >= READER && h.open < READER + (512 << 10), "{h:?}");
        assert!(h.decode > 0 && h.decode < 8 << 20, "{h:?}");
    }

    /// 自作の読み手が開くときに読む頭（識別ヘッダが 30 バイトでない・頭のページのグラニュールが 0 でない）でも、setup の
    /// codebook の表を数える（ordered の 2^22 項目の codebook。setup は数十バイト）。前の symphonia の数え方はどちらも数えな
    /// かった。
    #[test]
    fn kura_counts_the_headers_its_reader_reads() {
        use crate::codec::ogg::tests::page;
        use crate::codec::vorbis::bitpack::OggpackBuffer;
        let mut ident = b"\x01vorbis".to_vec();
        ident.extend_from_slice(&0u32.to_le_bytes());
        ident.push(2);
        ident.extend_from_slice(&44_100u32.to_le_bytes());
        ident.extend_from_slice(&[0; 12]);
        ident.push(0xb8);
        ident.push(1);
        assert_eq!(ident.len(), 30);
        let mut comment = b"\x03vorbis".to_vec();
        comment.extend_from_slice(&[0; 8]);
        comment.push(1);
        let mut w = OggpackBuffer::oggpack_writeinit();
        let entries = 1u64 << 22;
        for (v, bits) in [
            (5u64, 8u32),
            (u64::from(b'v'), 8),
            (u64::from(b'o'), 8),
            (u64::from(b'r'), 8),
            (u64::from(b'b'), 8),
            (u64::from(b'i'), 8),
            (u64::from(b's'), 8),
            (0, 8),
            (0x56_4342, 24),
            (1, 16),
            (entries, 24),
            (1, 1),
            (21, 5),
            (entries, 23),
            (0, 4),
        ] {
            w.oggpack_write(v, bits).unwrap();
        }
        let setup = w.oggpack_get_buffer().to_vec();
        let tables = crate::codec::vorbis::dec::codebook::setup_table_bytes(&setup).unwrap();
        assert!(
            tables.setup >= 4 << 20 && tables.decode >= 100 << 20,
            "{tables:?}"
        );
        let file = |ident: &[u8], granule: u64| {
            let mut f = page(7, 0, 0x02, granule, &[ident]);
            f.extend_from_slice(&page(7, 1, 0, granule, &[&comment]));
            f.extend_from_slice(&page(7, 2, 0, 0, &[&setup]));
            f.extend_from_slice(&page(7, 3, 0x04, 4_096, &[&[0u8]]));
            f
        };
        let mut long = ident.clone();
        long.push(0);
        let limits = Limits::default();
        for (name, bytes) in [
            ("30 バイト", file(&ident, 0)),
            ("31 バイト", file(&long, 0)),
            ("頭のグラニュール 1", file(&ident, 1)),
        ] {
            let h = ogg(&mut Cursor::new(&bytes), &limits).unwrap();
            assert!(h.open >= READER + tables.setup, "{name}: {h:?}");
            assert_eq!(h.decode, tables.decode, "{name}: {h:?}");
        }
        // Vorbis のない BOS の組が終わったら（BOS でないページが来たら）、そのあとは見ない（自作の読み手はそこで断る。
        // あとの Vorbis は連結）。同じ BOS の組にある Vorbis は読む（多重化）
        let opus_head = page(9, 0, 0x02, 0, &[b"OpusHead\x01\x02\0\0\x80\xbb\0\0\0\0\0"]);
        let mut muxed = opus_head.clone();
        muxed.extend_from_slice(&file(&ident, 0));
        let h = ogg(&mut Cursor::new(&muxed), &limits).unwrap();
        assert_eq!(h.decode, tables.decode, "{h:?}");
        let mut opus = opus_head;
        opus.extend_from_slice(&page(9, 1, 0, 0, &[b"OpusTags\0\0\0\0\0\0\0\0"]));
        opus.extend_from_slice(&file(&ident, 0));
        let h = ogg(&mut Cursor::new(&opus), &limits).unwrap();
        assert_eq!(h.decode, 0, "{h:?}");
    }

    /// 違う形式・空・壊れた頭でも、落ちずに読み手の緩衝だけを返す。
    #[test]
    fn foreign_bytes_count_nothing() {
        let limits = Limits::default();
        for bytes in [Vec::new(), b"RIFF0000WAVE".to_vec(), vec![0xff; 100]] {
            assert_eq!(flac(&mut Cursor::new(&bytes)).unwrap().open, READER);
            assert_eq!(ogg(&mut Cursor::new(&bytes), &limits).unwrap().open, READER);
            assert_eq!(mp4(&mut Cursor::new(&bytes)).unwrap().open, READER);
        }
    }

    /// 壊れた入力（ChaCha8 で変えたフィクスチャと雑音）でも panic せず、必ず終わる。
    #[test]
    fn broken_inputs_do_not_panic() {
        let limits = Limits::default();
        let mut rng = crate::determinism::rng(13);
        let seeds = [
            fixture("flac-16.flac"),
            fixture("ogg-vorbis-ffmpeg.ogg"),
            fixture("aac-lc-ffmpeg.m4a"),
            fixture("alac-24.m4a"),
        ];
        for _ in 0..300 {
            let mut b = seeds[(rng.next_u32() as usize) % seeds.len()].clone();
            for _ in 0..1 + rng.next_u32() % 16 {
                let i = (rng.next_u32() as usize) % b.len();
                b[i] = rng.next_u32() as u8;
            }
            if rng.next_u32().is_multiple_of(4) {
                let cut = (rng.next_u32() as usize) % b.len();
                b.truncate(cut);
            }
            let _ = flac(&mut Cursor::new(&b)).unwrap();
            let _ = ogg(&mut Cursor::new(&b), &limits).unwrap();
            let _ = mp4(&mut Cursor::new(&b)).unwrap();
        }
    }
}
