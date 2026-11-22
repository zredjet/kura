//! 読み手が音声でない部分で持つ量を、ファイルの頭だけを見て安く数える（M13 段 2 の検証の直し。[`Held`]）。
//!
//! symphonia の読み手は、開くときに埋め込みの画像・タグ・箱を読み込み、読み終えるまで持ち続ける（FLAC の PICTURE・
//! APPLICATION・VORBIS_COMMENT、m4a の moov と ilst の covr、Ogg の頭のパケットのコメントと METADATA_BLOCK_PICTURE）。
//! 形を読む間・流して読む間の作業領域の見積もり（`CodecSet::shape_path_staged`・`stage_stream_work_bytes`）に、ここで数えた量を
//! 足す。数えるのは箱・ページの頭と、数を決める数バイトだけ（中身は読まずにシークで飛ばす。Ogg の頭のページは CRC を照らす
//! ために読むが、確保は 1 ページ分だけ）。
//!
//! 量は実測（数える確保器。realloc は写す間に前と新しい領域を両方持つと数える。tests/stream_held.rs・kura-devtools の
//! mem_smoke）に合わせた上からの見積もり：
//! - 画像（FLAC の PICTURE・m4a の covr）・APPLICATION：中身の 1 倍（そのまま持つ）。FLAC の PICTURE の MIME・説明は 2 倍。
//!   1 つの領域に読む中身は、4 MiB を超えると伸ばし直しながら読むので、読む間は最大でほぼ 2 倍（[`boxed_read_peak`]。
//!   箱を 1 つずつ読むので、その最大だけを足す）。
//! - コメント・タグの中身：[`TEXT_FACTOR`] 倍（Ogg はパケットを組み立てる写し、base64 の画像は復号した写しと画像の写し、
//!   文字はタグと読み取りの写し `raw_tags` を持つ。実測は 2.5〜5.6 倍）。m4a の ilst の covr 以外は [`MP4_TEXT_FACTOR`] 倍
//!   （実測 3 倍）。
//! - コメント・タグ 1 つあたり [`ITEM_COST`]（タグの構造と小さな確保。実測 176〜210 バイト）。数はコメントの数の欄を、
//!   残りの長さで入りうる数（1 つ 4 バイト以上）で抑える。
//! - FLAC の SEEKTABLE は 2 倍（実測 1.5 倍）、CUESHEET は 8 倍（小さい）、moov のほかの部分（標本の表）は 2 倍。
//! - 読み手そのものの緩衝（[`READER`]）。
//! - Vorbis の codebook の表（流して読む間だけ。`symph::vorbis_setup_bytes`）：setup が [`SETUP_MAX`] を超えて組み立てられない
//!   ときは、読み口が受け付ける上限（`min(max_value_bytes, 256 MiB)`）とみなす。
//!
//! 数えられない頭（壊れた箱・違う形式）は、そこまでの量を返す（読み手はそこで断るか、読み飛ばしたところで止まる）。
//! I/O の誤りだけを返す。

use std::io::SeekFrom;

use crate::codec::ReadSeek;
use crate::codec::symph::vorbis_setup_bytes;
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

/// 読み手そのものの緩衝（symphonia の `MediaSourceStream`・Ogg のページ・パケットの組み立て）。
pub(crate) const READER: u64 = 1 << 20;
/// コメント・タグ 1 つあたりの量。
pub(crate) const ITEM_COST: u64 = 256;
/// Vorbis コメント（FLAC・Ogg）の中身の倍率。
pub(crate) const TEXT_FACTOR: u64 = 6;
/// m4a の ilst の、covr 以外の中身の倍率。
pub(crate) const MP4_TEXT_FACTOR: u64 = 4;
/// Vorbis の setup を組み立てて表の大きさを見積もる上限（これを超える setup は表を上限とみなす）。正しく作られた
/// ファイルの setup は数 KB。
pub(crate) const SETUP_MAX: usize = 1 << 20;
/// Ogg のパケットの頭として残す長さ（コメントの数の欄を探す）。
const PACKET_HEAD: usize = 64 << 10;
/// 読み飛ばしをシークでなく読んで捨てる長さの上限（小さな箱が続くときに、緩衝を捨てるシークを繰り返さない）。
const SKIP_BY_READ: u64 = 64 << 10;

/// symphonia の読み口から長さ `len` を 1 つの領域に読む（`ReadBytes::read_boxed_slice_exact`）間の確保の山。4 MiB を超える
/// ものは 4・8・16 MiB… ずつ伸ばし直して読む（`safe_read_into_boxed_slice`）ので、伸ばすときに前の領域と新しい領域を両方
/// 持つ（大きな確保の realloc は写す）。
pub(crate) fn boxed_read_peak(len: u64) -> u64 {
    const FIRST: u64 = 4 << 20;
    const MAX_STEP: u64 = 1 << 30;
    let (mut have, mut step, mut peak) = (0u64, FIRST, len);
    while have < len {
        if step == MAX_STEP {
            // 残りは 1 GiB ずつ足す：山は最後に伸ばすとき（len − r から len へ）
            let r = (len - have - 1) % MAX_STEP + 1;
            let before = len - r;
            if before > 0 {
                peak = peak.max(before.saturating_add(len));
            }
            break;
        }
        let next = have + step.min(len - have);
        if have > 0 {
            peak = peak.max(have.saturating_add(next));
        }
        have = next;
        step = step.saturating_mul(2).min(MAX_STEP);
    }
    peak
}

/// 1 つの領域に読む間だけ、読んだものに足して持つ量（[`boxed_read_peak`] − `len`）。
fn boxed_extra(len: u64) -> u64 {
    boxed_read_peak(len) - len
}

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

/// FLAC（symphonia の `FlacReader` がメタデータの箱を読む順に、`last` の箱まで）。箱ごとに持ち続ける量を足し、1 つの領域に
/// 読む間だけ持つ量（[`boxed_extra`]）の最大を足す。
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
        match kind {
            // STREAMINFO
            0 => peak.add(len, 0),
            // APPLICATION（中身を 1 つの領域に読んで持つ）
            2 => peak.add(len.saturating_add(ITEM_COST), boxed_extra(len)),
            // SEEKTABLE（点を 1 つずつ伸ばす列に足す）
            3 => peak.add(len.saturating_mul(2), len.saturating_mul(2)),
            // VORBIS_COMMENT（コメントを 1 つずつ 1 つの領域に読んで、タグに写す）
            4 => {
                let n = comment_count(&mut w, end)?;
                peak.add(
                    len.saturating_mul(TEXT_FACTOR)
                        .saturating_add(n.saturating_add(1).saturating_mul(ITEM_COST)),
                    boxed_extra(len),
                );
            }
            // CUESHEET
            5 => peak.add(len.saturating_mul(8), 0),
            // PICTURE：種類・MIME の長さ・MIME・説明の長さ・説明・…・画像の長さ・画像（画像は 1 つの領域に読んで持つ。
            // MIME・説明は写しも持つ）
            6 => {
                let mut text = 0u64;
                if w.u32_be()?.is_some()
                    && let Some(mime) = w.u32_be()?
                {
                    text = u64::from(mime);
                    w.goto(w.pos.saturating_add(u64::from(mime)))?;
                    if w.pos.saturating_add(4) <= end
                        && let Some(desc) = w.u32_be()?
                    {
                        text = text.saturating_add(u64::from(desc));
                    }
                }
                let text = text.min(len);
                peak.add(
                    len.saturating_add(text.saturating_mul(2))
                        .saturating_add(ITEM_COST),
                    boxed_extra(len),
                );
            }
            // PADDING・知らない箱は読み飛ばす（持たない）
            _ => {}
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

/// m4a（symphonia の `IsoMp4Reader` が開くときに読む最上位の moov・meta）。moov の中の udta/meta/ilst のタグは、covr を
/// 1 倍、ほかを [`MP4_TEXT_FACTOR`] 倍、data（と name・mean）1 つにつき [`ITEM_COST`]。ほかの moov の中身は 2 倍。タグの
/// 中身は 1 つの領域に読むので、最も大きいものを読む間の [`boxed_extra`] を足す。
pub(crate) fn mp4(src: &mut dyn ReadSeek) -> Result<Held, EngineError> {
    let mut w = Walk::new(src)?;
    let mut peak = Peak::default();
    let len = w.len;
    let mut top = Boxes::new(0, len);
    while let Some((kind, s, e)) = top.next(&mut w)? {
        let tags = match &kind {
            b"moov" => {
                let mut tags = Tags::default();
                let mut inner = Boxes::new(s, e);
                while let Some((k, us, ue)) = inner.next(&mut w)? {
                    if &k != b"udta" {
                        continue;
                    }
                    let mut udta = Boxes::new(us, ue);
                    while let Some((k, ms, me)) = udta.next(&mut w)? {
                        if &k == b"meta" {
                            tags.add_meta(&mut w, ms, me)?;
                        }
                    }
                }
                Some(tags)
            }
            b"meta" => {
                let mut tags = Tags::default();
                tags.add_meta(&mut w, s, e)?;
                Some(tags)
            }
            _ => None,
        };
        if let Some(t) = tags {
            let rest = (e - s).saturating_sub(t.ilst);
            peak.add(
                rest.saturating_mul(2)
                    .saturating_add(t.visual)
                    .saturating_add(t.text.saturating_mul(MP4_TEXT_FACTOR))
                    .saturating_add(t.items.saturating_mul(ITEM_COST)),
                boxed_extra(t.largest),
            );
        }
    }
    Ok(Held {
        open: peak.total(),
        decode: 0,
    })
}

/// ilst の中で数えたもの。
#[derive(Default)]
struct Tags {
    /// ilst の大きさの合計（ほかの moov の中身から引く）。
    ilst: u64,
    /// covr の中身。
    visual: u64,
    /// covr 以外の中身。
    text: u64,
    /// data・name・mean の数。
    items: u64,
    /// 最も大きい中身。
    largest: u64,
}

impl Tags {
    /// meta（版と旗の 4 バイトのあとに箱が並ぶ）の中の ilst を数える。
    fn add_meta(&mut self, w: &mut Walk<'_>, s: u64, e: u64) -> Result<(), EngineError> {
        let mut meta = Boxes::new(s.saturating_add(4).min(e), e);
        while let Some((k, is, ie)) = meta.next(w)? {
            if &k != b"ilst" {
                continue;
            }
            self.ilst = self.ilst.saturating_add(ie - is);
            let mut ilst = Boxes::new(is, ie);
            while let Some((tag, ts, te)) = ilst.next(w)? {
                self.items = self.items.saturating_add(1);
                let mut parts = Boxes::new(ts, te);
                while let Some((_, ds, de)) = parts.next(w)? {
                    self.items = self.items.saturating_add(1);
                    self.largest = self.largest.max(de - ds);
                    if &tag == b"covr" {
                        self.visual = self.visual.saturating_add(de - ds);
                    } else {
                        self.text = self.text.saturating_add(de - ds);
                    }
                }
            }
        }
        Ok(())
    }
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
    /// Vorbis の識別ヘッダ（30 バイト）。
    ident: Vec<u8>,
}

/// Ogg の頭で数えたもの。
#[derive(Default)]
struct OggCount {
    comments: u64,
    /// Vorbis の codebook の表の見積もりの最大。
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
            // Vorbis の識別ヘッダはちょうど 30 バイト（symphonia もそれ以外は Vorbis とみなさない）
            self.mapping = if head.starts_with(b"\x01vorbis") && len == 30 {
                self.ident = head.clone();
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
                let tables = if (head.len() as u64) < len {
                    worst
                } else {
                    let mut extra = self.ident.clone();
                    extra.extend_from_slice(&head);
                    vorbis_setup_bytes(&extra).map_or(0, |b| b.min(worst))
                };
                count.tables = count.tables.max(tables);
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

/// Ogg（symphonia の `OggReader` が開くときに読む頭のページ：CRC の合うページを、グラニュールが 0 か −1（パケットが
/// 終わらない）のあいだと、その次の 1 ページまで。捕まえの印 `OggS` を探して、合わないページは読み飛ばす）。`decode` は
/// Vorbis の codebook の表の見積もりの全体（`SymphoniaDecoder` が `stream_work_bytes` の見込みの分を引く）。
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
            // 合わないページ：印の次から探し直す（symphonia の読み手と同じ）
            w.goto(at + 4)?;
            continue;
        }
        bytes = bytes.saturating_add(page.len() as u64);
        let granule = u64::from_le_bytes([h[6], h[7], h[8], h[9], h[10], h[11], h[12], h[13]]);
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
                    ident: Vec::new(),
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
        if granule != 0 && granule != u64::MAX {
            break;
        }
    }
    let open = READER
        .saturating_add(bytes.saturating_mul(TEXT_FACTOR))
        .saturating_add(count.comments.saturating_mul(ITEM_COST));
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
        assert!(h.open >= READER && h.open < READER + (256 << 10), "{h:?}");
        assert!(h.decode > 0 && h.decode < 8 << 20, "{h:?}");
    }

    /// symphonia の `safe_read_into_boxed_slice` の伸ばし方（4・8・16 MiB… を足す）で、最後に伸ばすときに前と新しい領域を
    /// 両方持つ。
    #[test]
    fn boxed_reads_peak_while_growing() {
        const M: u64 = 1 << 20;
        for (len, peak) in [
            (0, 0),
            (100, 100),
            (4 * M, 4 * M),
            (4 * M + 1, 8 * M + 1),
            (8 * M, 12 * M),
            (12 * M, 16 * M),
            (15 * M, 27 * M),
            (30 * M, 58 * M),
            // 1 GiB ずつ足すところ（4 + 8 + … + 512 = 1,020 MiB のあと）：1,020 → 2,044 → 3,068 MiB → 長さ
            ((3 << 30) + 5, 3_068 * M + (3 << 30) + 5),
        ] {
            assert_eq!(boxed_read_peak(len), peak, "{len}");
        }
        assert!(boxed_read_peak(u64::MAX / 2) >= u64::MAX / 2);
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
