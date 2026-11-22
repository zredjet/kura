//! 流して読む見積もりが、読み手が持つ音声でない部分（埋め込みの画像・タグ・箱・Vorbis の表）を数えること（M13 段 2 の
//! 検証の直し。docs/editing.md §12.10）。数える確保器（このテストのバイナリだけの `#[global_allocator]`）で、形を読む段
//! （`CodecSet::shape_path_staged`。頭で数える分を含む）と流して読む段（`decode_stream_with_path`）の確保の山を測り、
//! 段ごとの見積もり（`PickedStage::shape_bytes`、`stage_stream_work_bytes` ＋ 窓 × 2 ＋ ダイジェスト）を下回らないことを
//! 確かめる。ファイルはフィクスチャに箱・ページ・ilst を足して作る（フィクスチャは変えない）。
//!
//! 前の見積もりは中身によらない定数で、15 MiB の画像 3 つの FLAC で山 46.5 MiB に対して 9.4 MiB、大きな VQ の codebook を
//! 足した Vorbis で山 195.7 MiB に対して 39.9 MiB だった。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use kura_engine::audio::digest::PcmTreeDigest;
use kura_engine::codec::stream::stream_window_bytes;
use kura_engine::codec::{CodecSet, NullSink, PickedStage};
use kura_engine::doc::DecodeOptions;
use kura_engine::{Dim, Limits, NoMonitor, Shape};
use serde_json::json;
use symphonia_core::audio::{Audio as _, GenericAudioBufferRef};
use symphonia_core::codecs::audio::well_known::CODEC_ID_ALAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder as _, AudioDecoderOptions};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration, Timestamp};

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// 山は大域の数え手で測るので、このバイナリのテストは 1 本ずつ回す（並ぶと、ほかのテストの確保が山に混ざる）。
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn grow(n: usize) {
    let c = CUR.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(c, Ordering::Relaxed);
}

// SAFETY: System にそのまま渡し、成功した確保の大きさを数えるだけ。
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: 呼び手の約束をそのまま System に渡す。
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        // SAFETY: 呼び手の約束をそのまま System に渡す。
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: 呼び手の約束をそのまま System に渡す。
        unsafe { System.dealloc(p, l) };
        CUR.fetch_sub(l.size(), Ordering::Relaxed);
    }
    /// 伸ばす・縮めるときは、新しい領域に写してから古い領域を返すとみなす（その間は両方を持つ。macOS の大きな確保の
    /// realloc は写すので、peak memory footprint もそう数える）。
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        // SAFETY: 呼び手の約束をそのまま System に渡す。
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            grow(n);
            CUR.fetch_sub(l.size(), Ordering::Relaxed);
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

/// `f` の間の確保の山（呼ぶ前からあるものを除く）。
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let base = CUR.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = f();
    (r, (PEAK.load(Ordering::Relaxed) - base) as u64)
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// 形を読む段・流して読む段の山が見積もりを下回らないことを確かめ、(形の山, 形の見積もり, 流れの山, 流れの見積もり)。
fn check(name: &str, path: &Path, format: &str) -> (u64, u64, u64, u64) {
    let set = CodecSet::pinned();
    let opts = DecodeOptions::default();
    let limits = Limits::default();
    // ダイジェストの仕事の数（このテストはグローバルのプールで流す。見積もりの照らしだけで、値には効かない）
    #[allow(clippy::disallowed_methods)]
    let tasks = std::thread::available_parallelism().map_or(4, |n| n.get()) as u64;
    let mut stages: Vec<PickedStage> = Vec::new();
    let (r, shape_peak) = peak_of(|| {
        set.shape_path_staged(path, format, &opts, &limits, &NoMonitor, None, &mut |s| {
            stages.push(*s);
            Ok(())
        })
    });
    let (shape, stage) = r.unwrap();
    let shape_est = stages.iter().map(|s| s.shape_bytes).max().unwrap();
    let len = std::fs::metadata(path).unwrap().len();
    let Shape::Audio {
        channels, frames, ..
    } = shape
    else {
        panic!("{name}: 音声でない");
    };
    let stream_est = set.stage_stream_work_bytes(&stage, format, &shape, len)
        + stream_window_bytes(usize::from(channels), frames.max()) * 2
        + PcmTreeDigest::work_bytes(usize::from(channels), tasks);
    let (r, stream_peak) = peak_of(|| {
        let mut sink = NullSink;
        set.decode_stream_with_path(
            stage.id, path, format, &opts, &limits, &mut sink, &NoMonitor,
        )
    });
    let stamp = r.unwrap();
    assert_eq!(stamp.codec, stage.id, "{name}");
    eprintln!(
        "{name}: 形 {:.1} MiB（見積もり {:.1}）・流れ {:.1} MiB（見積もり {:.1}）",
        mib(shape_peak),
        mib(shape_est),
        mib(stream_peak),
        mib(stream_est)
    );
    assert!(
        shape_peak <= shape_est,
        "{name}: 形を読む山 {shape_peak} > 見積もり {shape_est}"
    );
    assert!(
        stream_peak <= stream_est,
        "{name}: 流して読む山 {stream_peak} > 見積もり {stream_est}"
    );
    (shape_peak, shape_est, stream_peak, stream_est)
}

fn mib(b: u64) -> f64 {
    b as f64 / (1u64 << 20) as f64
}

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stream-held");
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---- FLAC ----

/// 1 秒・44.1 kHz ステレオの FLAC の STREAMINFO の後ろに箱 `(種類, 中身)` を差し込む。
fn flac_with(blocks: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let v = kura_engine::testgen::InputGen::AudioNoise {
        rate: 44_100,
        channels: 2,
        frames: 44_100,
        seed: 7,
    }
    .generate()
    .unwrap();
    let bytes = CodecSet::pinned()
        .encode_bytes(&v, "flac", &json!({ "bits": 16 }), &NoMonitor)
        .unwrap()
        .0;
    let at = 4 + 4 + 34;
    let mut f = bytes[..at].to_vec();
    let was_last = f[4] & 0x80 != 0;
    f[4] &= 0x7f;
    for (i, (kind, body)) in blocks.iter().enumerate() {
        f.push(
            kind | if was_last && i + 1 == blocks.len() {
                0x80
            } else {
                0
            },
        );
        f.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes()[1..]);
        f.extend_from_slice(body);
    }
    f.extend_from_slice(&bytes[at..]);
    f
}

/// FLAC の PICTURE の中身（PNG・説明 `desc`・画像 `data_len` バイト）。
fn picture_body(data_len: usize, desc: &[u8]) -> Vec<u8> {
    let mime = b"image/png";
    let mut body = Vec::new();
    body.extend_from_slice(&3u32.to_be_bytes());
    body.extend_from_slice(&u32::try_from(mime.len()).unwrap().to_be_bytes());
    body.extend_from_slice(mime);
    body.extend_from_slice(&u32::try_from(desc.len()).unwrap().to_be_bytes());
    body.extend_from_slice(desc);
    body.extend_from_slice(&[0u8; 16]);
    body.extend_from_slice(&u32::try_from(data_len).unwrap().to_be_bytes());
    body.resize(body.len() + data_len, 0x11);
    body
}

fn base64(data: &[u8]) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        s.push(T[(n >> 18) as usize & 63]);
        s.push(T[(n >> 12) as usize & 63]);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63]
        } else {
            b'='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63]
        } else {
            b'='
        });
    }
    s
}

/// Vorbis コメント（ベンダー "kura" とコメントの列）。
fn comments(list: &[Vec<u8>]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(b"kura");
    b.extend_from_slice(&u32::try_from(list.len()).unwrap().to_le_bytes());
    for c in list {
        b.extend_from_slice(&u32::try_from(c.len()).unwrap().to_le_bytes());
        b.extend_from_slice(c);
    }
    b
}

fn picture_comment(data_len: usize) -> Vec<u8> {
    let mut c = b"METADATA_BLOCK_PICTURE=".to_vec();
    c.extend_from_slice(&base64(&picture_body(data_len, b"")));
    c
}

fn tiny_comments(n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|_| b"A=".to_vec()).collect()
}

fn text_comment(len: usize) -> Vec<u8> {
    let mut c = b"LYRICS=".to_vec();
    c.resize(c.len() + len, b'x');
    c
}

// ---- Ogg ----

struct Page {
    flags: u8,
    granule: u64,
    serial: u32,
    lacing: Vec<u8>,
    body: Vec<u8>,
}

fn crc(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c ^= u32::from(b) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04c1_1db7
            } else {
                c << 1
            };
        }
    }
    c
}

fn write_page(out: &mut Vec<u8>, p: &Page, seq: u32) {
    let mut h = b"OggS\0".to_vec();
    h.push(p.flags);
    h.extend_from_slice(&p.granule.to_le_bytes());
    h.extend_from_slice(&p.serial.to_le_bytes());
    h.extend_from_slice(&seq.to_le_bytes());
    h.extend_from_slice(&[0; 4]);
    h.push(u8::try_from(p.lacing.len()).unwrap());
    h.extend_from_slice(&p.lacing);
    h.extend_from_slice(&p.body);
    let c = crc(&h);
    h[22..26].copy_from_slice(&c.to_le_bytes());
    out.extend_from_slice(&h);
}

/// フィクスチャの Ogg Vorbis の頭の 3 つのパケットを `edit(識別, コメント, setup)` で差し替え、ページを組み直す。
fn ogg_with(edit: impl FnOnce(&mut [Vec<u8>; 3])) -> Vec<u8> {
    let b = fixture("ogg-vorbis-ffmpeg.ogg");
    let mut pages = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let n = b[at + 26] as usize;
        let lacing = b[at + 27..at + 27 + n].to_vec();
        let len: usize = lacing.iter().map(|&x| x as usize).sum();
        pages.push(Page {
            flags: b[at + 5],
            granule: u64::from_le_bytes(b[at + 6..at + 14].try_into().unwrap()),
            serial: u32::from_le_bytes(b[at + 14..at + 18].try_into().unwrap()),
            body: b[at + 27 + n..at + 27 + n + len].to_vec(),
            lacing,
        });
        at += 27 + n + len;
    }
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut cur = Vec::new();
    let mut heads_end = 0;
    'pages: for (pi, p) in pages.iter().enumerate() {
        let mut off = 0;
        for &l in &p.lacing {
            cur.extend_from_slice(&p.body[off..off + l as usize]);
            off += l as usize;
            if l < 255 {
                packets.push(std::mem::take(&mut cur));
                if packets.len() == 3 {
                    assert_eq!(off, p.body.len());
                    heads_end = pi + 1;
                    break 'pages;
                }
            }
        }
    }
    let mut heads: [Vec<u8>; 3] = packets.try_into().unwrap();
    edit(&mut heads);
    let serial = pages[0].serial;
    let mut out = Vec::new();
    let mut seq = 0u32;
    for (i, pk) in heads.iter().enumerate() {
        let mut segs = vec![255u8; pk.len() / 255];
        segs.push((pk.len() % 255) as u8);
        let mut off = 0usize;
        for (k, chunk) in segs.chunks(255).enumerate() {
            let blen: usize = chunk.iter().map(|&x| x as usize).sum();
            let ends = chunk.last().is_some_and(|&x| x < 255);
            let flags = u8::from(k > 0) | if i == 0 && k == 0 { 2 } else { 0 };
            let p = Page {
                flags,
                granule: if ends { 0 } else { u64::MAX },
                serial,
                lacing: chunk.to_vec(),
                body: pk[off..off + blen].to_vec(),
            };
            write_page(&mut out, &p, seq);
            seq += 1;
            off += blen;
        }
    }
    for p in &pages[heads_end..] {
        write_page(&mut out, p, seq);
        seq += 1;
    }
    out
}

fn vorbis_comment_packet(list: &[Vec<u8>]) -> Vec<u8> {
    let mut b = b"\x03vorbis".to_vec();
    b.extend_from_slice(&comments(list));
    b.push(1);
    b
}

/// LSB から読むビットの読み手・書き手（Vorbis の setup）。
struct Bits {
    b: Vec<u8>,
    at: usize,
}

impl Bits {
    fn take(&mut self, n: usize) -> u64 {
        let mut v = 0u64;
        for k in 0..n {
            let bit = (self.b[self.at / 8] >> (self.at % 8)) & 1;
            v |= u64::from(bit) << k;
            self.at += 1;
        }
        v
    }

    fn put(&mut self, v: u64, n: usize) {
        for k in 0..n {
            if self.at.is_multiple_of(8) {
                self.b.push(0);
            }
            if (v >> k) & 1 == 1 {
                let l = self.b.len() - 1;
                self.b[l] |= 1 << (self.at % 8);
            }
            self.at += 1;
        }
    }
}

fn lookup1(entries: u64, dims: u64) -> u64 {
    let mut v = 0u64;
    while (v + 1)
        .checked_pow(u32::try_from(dims).unwrap())
        .is_some_and(|p| p <= entries)
    {
        v += 1;
    }
    v
}

fn skip_codebook(r: &mut Bits) {
    assert_eq!(r.take(24), 0x56_4342);
    let dims = r.take(16);
    let entries = r.take(24);
    if r.take(1) == 0 {
        let sparse = r.take(1) == 1;
        for _ in 0..entries {
            if !sparse || r.take(1) == 1 {
                r.take(5);
            }
        }
    } else {
        r.take(5);
        let mut cur = 0;
        while cur < entries {
            cur += r.take((64 - (entries - cur).leading_zeros()) as usize);
        }
    }
    let lookup = r.take(4);
    if lookup == 1 || lookup == 2 {
        r.take(32);
        r.take(32);
        let bits = r.take(4) as usize + 1;
        r.take(1);
        let n = if lookup == 1 {
            lookup1(entries, dims)
        } else {
            entries * dims
        };
        for _ in 0..n {
            r.take(bits);
        }
    }
}

/// setup の codebook の列の後ろに、`book` が書く codebook を `extra` 個足す（floor・residue はもとの codebook を指したままなので、
/// 音声は前と同じに読める）。
fn add_codebooks(setup: &[u8], extra: u64, book: impl Fn(&mut Bits)) -> Vec<u8> {
    let mut r = Bits {
        b: setup.to_vec(),
        at: 56,
    };
    let books = r.take(8) + 1;
    let start = r.at;
    for _ in 0..books {
        skip_codebook(&mut r);
    }
    let end = r.at;
    let mut w = Bits {
        b: b"\x05vorbis".to_vec(),
        at: 56,
    };
    w.put(books + extra - 1, 8);
    r.at = start;
    while r.at < end {
        let v = r.take(1);
        w.put(v, 1);
    }
    for _ in 0..extra {
        book(&mut w);
    }
    let total = setup.len() * 8;
    while r.at < total {
        let v = r.take(1);
        w.put(v, 1);
    }
    w.b
}

/// 表が 16 MiB になる VQ の codebook（疎・使う項目 2・項目 131,072・次元 32・lookup 1）を `extra` 個足す。前の symphonia の
/// 読み手は項目のぶんの表を作ったが、自作の読み手（libvorbis と同じ）は使う項目のぶんだけ作るので、持つ量は長さの表
/// （項目ごとに 1 バイト）だけ。
fn add_big_codebooks(setup: &[u8], extra: u64) -> Vec<u8> {
    add_codebooks(setup, extra, |w| {
        w.put(0x56_4342, 24);
        w.put(32, 16);
        w.put(1 << 17, 24);
        w.put(0, 1);
        w.put(1, 1);
        for e in 0..(1u64 << 17) {
            if e < 2 {
                w.put(1, 1);
                w.put(0, 5);
            } else {
                w.put(0, 1);
            }
        }
        w.put(1, 4);
        w.put(0, 32);
        w.put(0, 32);
        w.put(0, 4);
        w.put(0, 1);
        w.put(0, 1);
    })
}

/// 自作の読み手が多く持つ codebook（ordered・長さ 19 が 2^19 個で全部使う・次元 2・lookup 1）を `extra` 個足す。1 冊は数百
/// バイトで、表は約 8.5 MiB（値 2^19 × 2 × 4 バイト・符号語と番号と長さ 2^19 × 9 バイト）。
fn add_ordered_codebooks(setup: &[u8], extra: u64) -> Vec<u8> {
    const ENTRIES: u64 = 1 << 19;
    add_codebooks(setup, extra, |w| {
        w.put(0x56_4342, 24);
        w.put(2, 16);
        w.put(ENTRIES, 24);
        w.put(1, 1);
        w.put(18, 5);
        w.put(ENTRIES, (64 - ENTRIES.leading_zeros()) as usize);
        w.put(1, 4);
        w.put(0, 32);
        w.put(0, 32);
        w.put(0, 4);
        w.put(0, 1);
        for _ in 0..lookup1(ENTRIES, 2) {
            w.put(0, 1);
        }
    })
}

// ---- m4a ----

fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut o = u32::try_from(8 + body.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    o.extend_from_slice(kind);
    o.extend_from_slice(body);
    o
}

/// ffmpeg の AAC の m4a（moov が最後）の moov の後ろに udta/meta/ilst（`(名前, 型, 中身)` のタグ）を足す。
fn m4a_with_tags(tags: &[([u8; 4], u32, Vec<u8>)]) -> Vec<u8> {
    let b = fixture("aac-lc-ffmpeg.m4a");
    let mut ilst = Vec::new();
    for (name, kind, data) in tags {
        let mut d = kind.to_be_bytes().to_vec();
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(data);
        ilst.extend_from_slice(&atom(name, &atom(b"data", &d)));
    }
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"mdirappl");
    hdlr.extend_from_slice(&[0u8; 9]);
    let mut meta = vec![0u8; 4];
    meta.extend_from_slice(&atom(b"hdlr", &hdlr));
    meta.extend_from_slice(&atom(b"ilst", &ilst));
    let udta = atom(b"udta", &atom(b"meta", &meta));
    let mut at = 0usize;
    let mut moov = None;
    while at + 8 <= b.len() {
        let s = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        if &b[at + 4..at + 8] == b"moov" {
            moov = Some((at, s));
        }
        at += s;
    }
    let (m, ms) = moov.unwrap();
    assert_eq!(m + ms, b.len(), "moov が最後でない");
    let mut f = b.clone();
    f[m..m + 4].copy_from_slice(&u32::try_from(ms + udta.len()).unwrap().to_be_bytes());
    f.extend_from_slice(&udta);
    f
}

/// 見積もりが、埋め込みの画像・タグ・箱・Vorbis の表で持つ量を下回らない（M13 段 2 の検証の直し）。
#[test]
fn stream_estimates_cover_what_readers_hold() {
    let _serial = serial();
    let d = dir();
    let mib8 = 8usize << 20;
    let mut cases: Vec<(&str, PathBuf, &str)> = Vec::new();
    let mut put = |name: &'static str, bytes: Vec<u8>, format: &'static str| {
        let path = d.join(format!("{name}.{format}"));
        std::fs::write(&path, bytes).unwrap();
        cases.push((name, path, format));
    };
    // FLAC：画像 3 つ（各 8 MiB）・説明の長い画像・base64 の画像のコメント・小さなコメント 20 万・長い文字・シークの表・
    // APPLICATION（画像・シークの表・APPLICATION は、M16 段 1 の自作の読み手が読み飛ばす。下の `skipped`）
    put(
        "flac-pictures",
        flac_with(&[
            (6, picture_body(mib8, b"")),
            (6, picture_body(mib8, b"")),
            (6, picture_body(mib8, b"")),
        ]),
        "flac",
    );
    put(
        "flac-long-description",
        flac_with(&[(6, picture_body(16, &vec![b'd'; 4 << 20]))]),
        "flac",
    );
    put(
        "flac-base64-picture",
        flac_with(&[(4, comments(&[picture_comment(4 << 20)]))]),
        "flac",
    );
    put(
        "flac-tiny-comments",
        flac_with(&[(4, comments(&tiny_comments(200_000)))]),
        "flac",
    );
    put(
        "flac-text",
        flac_with(&[(4, comments(&[text_comment(4 << 20)]))]),
        "flac",
    );
    let seek: Vec<u8> = (0..400_000u64)
        .flat_map(|i| {
            let mut p = (i * 4_096).to_be_bytes().to_vec();
            p.extend_from_slice(&(i * 100).to_be_bytes());
            p.extend_from_slice(&4_096u16.to_be_bytes());
            p
        })
        .collect();
    put("flac-seektable", flac_with(&[(3, seek)]), "flac");
    let mut app = b"kura".to_vec();
    app.resize(mib8, 0);
    put("flac-application", flac_with(&[(2, app)]), "flac");
    // Ogg Vorbis：base64 の画像・小さなコメント 20 万・長い文字・前の symphonia の表が 64 MiB になる codebook（自作の読み手は
    // 持たない。下の `skipped`）・自作の読み手の表が 34 MiB になる codebook（`stream_work_bytes` の見込みの 32 MiB を超える）
    put(
        "ogg-base64-picture",
        ogg_with(|h| h[1] = vorbis_comment_packet(&[picture_comment(4 << 20)])),
        "ogg",
    );
    put(
        "ogg-tiny-comments",
        ogg_with(|h| h[1] = vorbis_comment_packet(&tiny_comments(200_000))),
        "ogg",
    );
    put(
        "ogg-text",
        ogg_with(|h| h[1] = vorbis_comment_packet(&[text_comment(4 << 20)])),
        "ogg",
    );
    put(
        "ogg-big-codebooks",
        ogg_with(|h| h[2] = add_big_codebooks(&h[2], 4)),
        "ogg",
    );
    put(
        "ogg-ordered-codebooks",
        ogg_with(|h| h[2] = add_ordered_codebooks(&h[2], 4)),
        "ogg",
    );
    // m4a：covr（8 MiB）・小さなタグ 10 万・長い歌詞
    put(
        "m4a-covr",
        m4a_with_tags(&[(*b"covr", 14, vec![0x55; mib8])]),
        "m4a",
    );
    let cmt = [0xa9, b'c', b'm', b't'];
    put(
        "m4a-tiny-tags",
        m4a_with_tags(
            &(0..100_000)
                .map(|_| (cmt, 1, b"a".to_vec()))
                .collect::<Vec<_>>(),
        ),
        "m4a",
    );
    let lyr = [0xa9, b'l', b'y', b'r'];
    put(
        "m4a-lyrics",
        m4a_with_tags(&[(lyr, 1, vec![b'x'; 4 << 20])]),
        "m4a",
    );
    // 自作の FLAC の読み手が読み飛ばす箱と、自作の Ogg Vorbis の読み手が使わない項目（持たない）
    let skipped = [
        "flac-pictures",
        "flac-long-description",
        "flac-seektable",
        "flac-application",
        "ogg-big-codebooks",
    ];
    for (name, path, format) in &cases {
        let (shape_peak, _, stream_peak, _) = check(name, path, format);
        if skipped.contains(name) {
            assert!(shape_peak.max(stream_peak) < 2 << 20, "{name}");
        } else {
            // 見積もりの意味があるケース（持つ量が数 MiB ある）
            assert!(shape_peak.max(stream_peak) > 3 << 20, "{name}");
        }
        std::fs::remove_file(path).unwrap();
    }
    // 画像もタグもないフィクスチャも、見積もりの中
    for (name, format) in [
        ("flac-16.flac", "flac"),
        ("ogg-vorbis-ffmpeg.ogg", "ogg"),
        ("ogg-vorbis-5ch.ogg", "ogg"),
        ("aac-lc-ffmpeg.m4a", "m4a"),
        ("alac-24.m4a", "m4a"),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name);
        check(name, &path, format);
    }
}

/// ALAC は magic cookie の frame_length 65,536（symphonia が受け付ける上限）・8 チャンネル・32 ビットの非圧縮のフレームで、
/// デコーダ・パケット・作業列の山が `stream_work_bytes` を下回らない（前は 8,192 フレームで数え、山 6.25 MiB に対して
/// 4.5 MiB だった）。
#[test]
fn alac_largest_frames_fit_the_stream_estimate() {
    let _serial = serial();
    const FRAMES: u32 = 65_536;
    const CH: u8 = 8;
    let mut cookie = FRAMES.to_be_bytes().to_vec();
    cookie.extend_from_slice(&[0, 32, 40, 10, 14, CH]);
    cookie.extend_from_slice(&255u16.to_be_bytes());
    cookie.extend_from_slice(&[0; 8]);
    cookie.extend_from_slice(&44_100u32.to_be_bytes());
    let mut params = AudioCodecParameters::new();
    params
        .for_codec(CODEC_ID_ALAC)
        .with_sample_rate(44_100)
        .with_extra_data(cookie.into_boxed_slice());
    // 非圧縮（escape）の 1 フレーム：SCE・CPE × 3・SCE（MSB から書く）
    let mut bits: Vec<u8> = Vec::new();
    let mut at = 0usize;
    let mut put = |v: u64, n: usize| {
        for k in (0..n).rev() {
            if at.is_multiple_of(8) {
                bits.push(0);
            }
            if (v >> k) & 1 == 1 {
                let l = bits.len() - 1;
                bits[l] |= 0x80 >> (at % 8);
            }
            at += 1;
        }
    };
    for (tag, n) in [(0u64, 1u64), (1, 2), (1, 2), (1, 2), (0, 1)] {
        put(tag, 3);
        put(0, 4);
        put(0, 12);
        put(0, 1);
        put(0, 2);
        put(1, 1);
        for i in 0..u64::from(FRAMES) * n {
            put(i & 0xffff, 32);
        }
    }
    put(7, 3);
    let packet = bits.into_boxed_slice();
    let claim = CodecSet::pinned().decode_stream_work_bytes(
        "m4a",
        &Shape::Audio {
            rate: 44_100,
            channels: u16::from(CH),
            frames: Dim::Exact(u64::from(FRAMES)),
        },
        packet.len() as u64,
    );
    // 読み口が読むパケット（isomp4 は Box<[u8]> に読む）は測る前に作ったので、山に足す
    let len = packet.len() as u64;
    let (_, peak) = peak_of(move || {
        // デコーダ・decode_all の作業列
        let pkt = Packet::new(
            1,
            Timestamp::new(0),
            Duration::new(u64::from(FRAMES)),
            packet,
        );
        let mut dec =
            symphonia_codec_alac::AlacDecoder::try_new(&params, &AudioDecoderOptions::default())
                .unwrap();
        let buf = dec.decode(&pkt).unwrap();
        let mut planes: Vec<Vec<f32>> = (0..usize::from(CH)).map(|_| Vec::new()).collect();
        for (ch, plane) in planes.iter_mut().enumerate() {
            plane.try_reserve(buf.frames()).unwrap();
            let GenericAudioBufferRef::S32(b) = &buf else {
                panic!("i32 でない");
            };
            plane.extend(b.plane(ch).unwrap().iter().map(|s| *s as f32));
        }
        planes.len()
    });
    let peak = peak + len;
    eprintln!(
        "ALAC: 山 {:.2} MiB（見積もり {:.2}）",
        mib(peak),
        mib(claim)
    );
    assert!(peak <= claim, "ALAC の山 {peak} > 見積もり {claim}");
}
