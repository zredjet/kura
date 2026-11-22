//! codebook の読み取りと復号（デコーダ）。
//!
//! | 関数 | C |
//! |---|---|
//! | [`staticbook_unpack`] | codebook.c:277–399 `vorbis_staticbook_unpack`（表の量を確かめて確保する。下の「表の量」） |
//! | [`maptype1_quantvals`] | sharedbook.c:174–211 `_book_maptype1_quantvals` |
//! | [`book_unquantize`] | sharedbook.c:216–279 `_book_unquantize`（復号の init の、`sparsemap` のある形だけ） |
//! | [`DecodeBook::init`] | sharedbook.c:333–458 `vorbis_book_init_decode`・319–325 `bitreverse` |
//! | `decode_packed_entry_number` | codebook.c:452–505（aoTuV の `vorbis_oggpack_look`・`vorbis_oggpack_adv`） |
//! | [`DecodeBook::decode`] | codebook.c:518–527 `vorbis_book_decode` |
//! | [`DecodeBook::decodevs_add`] | codebook.c:531–548 |
//! | [`DecodeBook::decodev_add`] | codebook.c:551–566 |
//! | [`DecodeBook::decodev_set`] | codebook.c:570–590 |
//! | [`DecodeBook::decodevv_add`] | codebook.c:593–934（SSE でない経路 915–928） |
//!
//! 型：`long` は i64、`int` は i32、`ogg_uint32_t` は u32、`char *lengthlist` は u8。`qsort`（sharedbook.c:370 の
//! `sort32a`）は符号語の値で並べる。符号語は prefix-free なので値は重ならず、どの正しい並べ替えでも同じ（規則 8）→ 安定な
//! `sort_by_key`。
//!
//! C と違うところ（どれも C が壊れた setup で止まる・範囲の外を読む形。正しいファイルでは同じ）：
//! - `_book_maptype1_quantvals` は `dim == 0` で終わらない（`vals` を 1 ずつ増やし続ける）。移植は 0 を返す（`dim == 0` の
//!   maptype 1 は `_book_unquantize` の中のループを回らないので、値は変わらない）。
//! - 値のない（maptype 0）codebook で値を足す復号（`decodev*`）を頼まれると、C は NULL の `valuelist` を読む。移植は誤り
//!   （`Err`）を返す。
//! - 速さのため、復号の最初の表（`dec_firsttable`）の幅は C の `clamp(ilog(used) − 4, 5, 8)` と、最長の符号語の長さ
//!   （[`FIRSTTABLE_MAX`] まで）の大きいほう。表は「直に当たる符号語」と「二分探索の範囲」を与えるだけで、その範囲はいつも
//!   探索の答え（`codelist` のうち読んだビット以下の最後のもの）を含むので、どの幅でも読める番号・進むビット・パケットの
//!   終わりの −1 は同じ（残りが表の幅より短いときは、どちらも範囲を狭めずに探す）。
//! - 最初の表の「直に当たる符号語」の値は、C の `番号 + 1` でなく `長さ << 24 | 番号`（[`direct_entry`]）にして、長さの表を
//!   引き直さない（同じ長さ。番号は 2^24 より小さく、長さは表の幅以下なので、C の探索の範囲の印 `0x80000000` とは重ならない）。
//! - パケットの終わりまで 8 バイト以上あるときは、8 バイトをまとめて読んだ語（[`OggpackReader::peek_word`]）から表を引く。
//!   aoTuV の覗きの範囲の確かめはそのとき必ず通る（−1 にならない）ので、表の幅でも最長の符号語の長さでも、覗いた値は語の
//!   下位のビットと同じ。それより終わりに近いときは C の形のまま（`decode_near_end`）。値を足す復号のループ
//!   （`decodev_add`・`decodevv_add`）は、覗きの窓（[`Window`]。読んだビットを語に持ち、32 ビット以上あれば覗きは −1 に
//!   ならない）から読み、窓に読み足せなくなったら読み手に戻して C の形で読む（[`Entries`]）。試験
//!   `decode_reads_like_the_c_decoder` が、C の表と C の流れをそのまま組んだものと全冊で照らす。
//!
//! # 表の量（kura の側で決めること）
//!
//! ordered の codebook は数十ビットで 2^24 − 1 項目まで宣言できる（C は長さの表 `entries` バイトをそのまま確保する）。
//! 小さなファイルで大量に確保させないよう、確保の前に [`book_bytes`] の和を予算（`min(max_value_bytes, 256 MiB)`）で
//! 確かめ、超えれば `Unsupported`（前の symphonia と同じく次の段へ）。同じ式を、値を持たずに setup を数える
//! [`setup_table_bytes`] が使う（読み手が持つ量の見積もり `codec::held::ogg`）。量は setup の静的な表（開く間に持つ。形を
//! 読む間も）と復号の表（値を作るときだけ。形を読む間は作らない）に分ける（[`TableBytes`]）。

use vorbis_tables::StaticCodebook;

use super::bitwise::{OggpackReader, Window};
use crate::codec::vorbis::codebook::{float32_unpack, make_words};
use crate::error::EngineError;

/// `ov_ilog`（sharedbook.c:35–39）。
pub(crate) fn ov_ilog(mut v: u32) -> i32 {
    let mut ret = 0;
    while v != 0 {
        ret += 1;
        v >>= 1;
    }
    ret
}

/// 復号の最初の表の幅の上限（ビット。表は 2^10 × 4 バイト）。
const FIRSTTABLE_MAX: i32 = 10;

/// 表の量が予算を超えたときの誤り。
pub(crate) fn too_big() -> EngineError {
    EngineError::Unsupported(format!("{}: codebook の表が大きすぎます", super::DEC_ID))
}

/// codebook の表の量（バイト。[`book_bytes`] とその和）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TableBytes {
    /// setup の静的な表（長さ・量子化の値）。setup を読んでから開き終えるまで持つ（形を読む間も）。
    pub setup: u64,
    /// 復号の表と、それを作る間の一時の列。値を作るときだけ（流して読む間）。
    pub decode: u64,
}

impl TableBytes {
    /// 全体（予算で確かめる量）。
    pub fn total(self) -> u64 {
        self.setup.saturating_add(self.decode)
    }

    /// 和。
    pub fn add(self, o: Self) -> Self {
        Self {
            setup: self.setup.saturating_add(o.setup),
            decode: self.decode.saturating_add(o.decode),
        }
    }
}

/// codebook 1 つの表の量の上からの見積もり。setup の静的な表（長さ 1 バイト × `entries`・量子化の値 8 バイト ×
/// `quantvals`）と、復号の表（[`DecodeBook::init`]：使う項目ごとに符号語・番号 4 バイトずつと長さ 1 バイト、値 4 バイト ×
/// `dim`（maptype 1・2）、最初の表（2^[`FIRSTTABLE_MAX`] × 4 バイトまで）と付帯分で 5 KiB）と、作る間の一時の列（使う項目
/// ごとに 16 バイト：符号語 4・並べ替えの番号 8・逆の番号 4。並べ替えの作業領域 8 は逆の番号を作る前に返る）。
pub(crate) fn book_bytes(
    entries: i64,
    used: i64,
    dim: i64,
    maptype: i32,
    quantvals: i64,
) -> TableBytes {
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    let values = if maptype == 1 || maptype == 2 {
        n(used).saturating_mul(n(dim)).saturating_mul(4)
    } else {
        0
    };
    TableBytes {
        setup: n(entries).saturating_add(n(quantvals).saturating_mul(8)),
        decode: n(used)
            .saturating_mul(25)
            .saturating_add(values)
            .saturating_add((4 << FIRSTTABLE_MAX) + (1 << 10)),
    }
}

/// maptype の量子化の値の数（`vorbis_staticbook_unpack` の `quantvals`）。
fn quantvals_of(maptype: i32, dim: i64, entries: i64) -> i32 {
    match maptype {
        1 => {
            if dim == 0 {
                0
            } else {
                maptype1_quantvals(&StaticCodebook {
                    dim,
                    entries,
                    lengthlist: Vec::new(),
                    maptype,
                    q_min: 0,
                    q_delta: 0,
                    q_quant: 0,
                    q_sequencep: 0,
                    quantlist: Vec::new(),
                }) as i32
            }
        }
        _ => (entries * dim) as i32,
    }
}

/// 0 で埋めた長さ `n` の列を、予算の内で確保する（確かめは呼び手。ここは確保の失敗だけを断る）。
fn zeroed<T: Copy + Default>(n: usize) -> Result<Vec<T>, EngineError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| too_big())?;
    v.resize(n, T::default());
    Ok(v)
}

/// `vorbis_staticbook_unpack`（codebook.c:277–399）。読めなければ `Ok(None)`（C の NULL）。長さの表・量子化の値を
/// 確保する前に、それまでの量と合わせて `room`（予算の残り）を超えないかを確かめる（超えれば `Err`：[`too_big`]）。
pub(crate) fn staticbook_unpack(
    opb: &mut OggpackReader<'_>,
    room: u64,
) -> Result<Option<StaticCodebook>, EngineError> {
    // make sure alignment is correct
    if opb.read(24) != 0x564342 {
        return Ok(None);
    }
    // first the basic parameters
    let dim = opb.read(16);
    let entries = opb.read(24);
    if entries == -1 {
        return Ok(None);
    }
    if ov_ilog(dim as u32) + ov_ilog(entries as u32) > 24 {
        return Ok(None);
    }
    let mut lengthlist: Vec<u8>;
    // codeword ordering.... length ordered or unordered?
    match opb.read(1) as i32 {
        0 => {
            // allocated but unused entries?
            let unused = opb.read(1);
            if (entries * if unused != 0 { 1 } else { 5 } + 7) >> 3 > opb.storage - opb.bytes() {
                return Ok(None);
            }
            if entries as u64 > room {
                return Err(too_big());
            }
            lengthlist = zeroed(entries as usize)?;
            if unused != 0 {
                for l in lengthlist.iter_mut() {
                    if opb.read(1) != 0 {
                        let num = opb.read(5);
                        if num == -1 {
                            return Ok(None);
                        }
                        *l = (num + 1) as u8;
                    } else {
                        *l = 0;
                    }
                }
            } else {
                // all entries used; no tagging
                for l in lengthlist.iter_mut() {
                    let num = opb.read(5);
                    if num == -1 {
                        return Ok(None);
                    }
                    *l = (num + 1) as u8;
                }
            }
        }
        1 => {
            // ordered
            let mut length = opb.read(5) + 1;
            if length == 0 {
                return Ok(None);
            }
            if entries as u64 > room {
                return Err(too_big());
            }
            lengthlist = zeroed(entries as usize)?;
            let mut i = 0i64;
            while i < entries {
                let num = opb.read(ov_ilog((entries - i) as u32));
                if num == -1 {
                    return Ok(None);
                }
                if length > 32 || num > entries - i || (num > 0 && (num - 1) >> (length - 1) > 1) {
                    return Ok(None);
                }
                for _ in 0..num {
                    lengthlist[i as usize] = length as u8;
                    i += 1;
                }
                length += 1;
            }
        }
        // EOF
        _ => return Ok(None),
    }
    // Do we have a mapping to unpack?
    let maptype = opb.read(4) as i32;
    let mut s = StaticCodebook {
        dim,
        entries,
        lengthlist,
        maptype,
        q_min: 0,
        q_delta: 0,
        q_quant: 0,
        q_sequencep: 0,
        quantlist: Vec::new(),
    };
    match maptype {
        // no mapping
        0 => {}
        1 | 2 => {
            s.q_min = opb.read(32);
            s.q_delta = opb.read(32);
            s.q_quant = (opb.read(4) + 1) as i32;
            s.q_sequencep = opb.read(1) as i32;
            if s.q_sequencep == -1 {
                return Ok(None);
            }
            let quantvals = quantvals_of(maptype, s.dim, s.entries);
            // quantized values
            if i64::from((quantvals.wrapping_mul(s.q_quant).wrapping_add(7)) >> 3)
                > opb.storage - opb.bytes()
            {
                return Ok(None);
            }
            let Ok(n) = usize::try_from(quantvals) else {
                return Ok(None);
            };
            if (s.entries as u64).saturating_add((n as u64).saturating_mul(8)) > room {
                return Err(too_big());
            }
            let mut quantlist = Vec::new();
            quantlist.try_reserve_exact(n).map_err(|_| too_big())?;
            for _ in 0..n {
                quantlist.push(opb.read(s.q_quant));
            }
            if quantvals != 0 && quantlist[n - 1] == -1 {
                return Ok(None);
            }
            s.quantlist = quantlist;
        }
        _ => return Ok(None),
    }
    Ok(Some(s))
}

/// 1 冊の [`book_bytes`] を、値を持たずに数える（[`staticbook_unpack`] と同じ読み方・同じ確かめ。ordered は長さの区切りごとに
/// 数え、項目ごとに回らない）。読めなければ None。
fn scan_book(opb: &mut OggpackReader<'_>) -> Option<TableBytes> {
    if opb.read(24) != 0x564342 {
        return None;
    }
    let dim = opb.read(16);
    let entries = opb.read(24);
    if entries == -1 || ov_ilog(dim as u32) + ov_ilog(entries as u32) > 24 {
        return None;
    }
    let mut used = 0i64;
    match opb.read(1) as i32 {
        0 => {
            let unused = opb.read(1);
            if (entries * if unused != 0 { 1 } else { 5 } + 7) >> 3 > opb.storage - opb.bytes() {
                return None;
            }
            for _ in 0..entries {
                if unused == 0 || opb.read(1) != 0 {
                    if opb.read(5) == -1 {
                        return None;
                    }
                    used += 1;
                }
            }
        }
        1 => {
            let mut length = opb.read(5) + 1;
            if length == 0 {
                return None;
            }
            let mut i = 0i64;
            while i < entries {
                let num = opb.read(ov_ilog((entries - i) as u32));
                if num == -1 {
                    return None;
                }
                if length > 32 || num > entries - i || (num > 0 && (num - 1) >> (length - 1) > 1) {
                    return None;
                }
                i += num;
                length += 1;
            }
            used = entries;
        }
        _ => return None,
    }
    let maptype = opb.read(4) as i32;
    let quantvals = match maptype {
        0 => 0,
        1 | 2 => {
            opb.read(32);
            opb.read(32);
            let q_quant = (opb.read(4) + 1) as i32;
            if opb.read(1) == -1 {
                return None;
            }
            let quantvals = quantvals_of(maptype, dim, entries);
            if i64::from((quantvals.wrapping_mul(q_quant).wrapping_add(7)) >> 3)
                > opb.storage - opb.bytes()
            {
                return None;
            }
            let n = usize::try_from(quantvals).ok()?;
            let mut last = 0;
            for _ in 0..n {
                last = opb.read(q_quant);
            }
            if quantvals != 0 && last == -1 {
                return None;
            }
            i64::from(quantvals)
        }
        _ => return None,
    };
    Some(book_bytes(entries, used, dim, maptype, quantvals))
}

/// setup ヘッダ（`packet` はパケットの全体）の codebook の表の量（[`book_bytes`] の和。予算では切らない）。値を持たずに
/// 数える（`codec::held::ogg` の見積もり）。setup の印でない・codebook が読めなければ None。
pub(crate) fn setup_table_bytes(packet: &[u8]) -> Option<TableBytes> {
    let mut opb = OggpackReader::new(packet);
    if opb.read(8) != 5 {
        return None;
    }
    for &c in b"vorbis" {
        if opb.read(8) != i64::from(c) {
            return None;
        }
    }
    let books = opb.read(8) + 1;
    let mut total = TableBytes::default();
    for _ in 0..books {
        total = total.add(scan_book(&mut opb)?);
    }
    Some(total)
}

/// `_book_maptype1_quantvals`（sharedbook.c:174–211）。`dim == 0` は 0（C は終わらない。モジュールの説明）。
pub(crate) fn maptype1_quantvals(b: &StaticCodebook) -> i64 {
    if b.entries < 1 || b.dim == 0 {
        return 0;
    }
    // C：vals=floor(pow((float)b->entries,1.f/b->dim));（double の pow に float を渡す。1.f/dim は float の割り算）
    let guess = crate::math::pow(b.entries as f32 as f64, (1.0f32 / b.dim as f32) as f64).floor();
    let mut vals = guess as i64;
    // treat the above as an initial guess
    if vals < 1 {
        vals = 1;
    }
    loop {
        let mut acc: i64 = 1;
        let mut acc1: i64 = 1;
        let mut i = 0i64;
        while i < b.dim {
            if b.entries / vals < acc {
                break;
            }
            acc = acc.wrapping_mul(vals);
            if i64::MAX / (vals + 1) < acc1 {
                acc1 = i64::MAX;
            } else {
                acc1 *= vals + 1;
            }
            i += 1;
        }
        if i >= b.dim && acc <= b.entries && acc1 > b.entries {
            return vals;
        } else if i < b.dim || acc > b.entries {
            vals -= 1;
        } else {
            vals += 1;
        }
    }
}

/// `_book_unquantize`（sharedbook.c:216–279）の、復号の init が使う形（`n` は使う項目の数、`sparsemap` は並べ替えの番号）。
/// 値のない codebook は空。
pub(crate) fn book_unquantize(
    b: &StaticCodebook,
    n: i32,
    sparsemap: &[i32],
) -> Result<Vec<f32>, EngineError> {
    if b.maptype != 1 && b.maptype != 2 {
        return Ok(Vec::new());
    }
    let dim = b.dim as usize;
    let len = (n as usize)
        .checked_mul(dim)
        .ok_or_else(|| EngineError::Limit("codebook が大きすぎます".into()))?;
    let mut r = Vec::new();
    r.try_reserve_exact(len)
        .map_err(|_| EngineError::Limit("codebook の表を確保できません".into()))?;
    r.resize(len, 0f32);
    let mindel = float32_unpack(b.q_min);
    let delta = float32_unpack(b.q_delta);
    let mut count = 0usize;
    let quantvals = if b.maptype == 1 {
        maptype1_quantvals(b) as i32
    } else {
        0
    };
    for j in 0..b.entries {
        if b.lengthlist[j as usize] == 0 {
            continue;
        }
        let mut last = 0f32;
        let mut indexdiv: i32 = 1;
        let base = sparsemap[count] as usize * dim;
        for k in 0..dim {
            let q = if b.maptype == 1 {
                let index = ((j / i64::from(indexdiv)) % i64::from(quantvals)) as i32;
                b.quantlist[index as usize]
            } else {
                b.quantlist[j as usize * dim + k]
            };
            // C：float val=b->quantlist[index]; val=fabs(val)*delta+mindel+last;（fabs は double。和は double で、代入で 1 回丸める）
            let val = q as f32;
            let val = ((val as f64).abs() * delta as f64 + mindel as f64 + last as f64) as f32;
            if b.q_sequencep != 0 {
                last = val;
            }
            r[base + k] = val;
            indexdiv = indexdiv.wrapping_mul(quantvals);
        }
        count += 1;
    }
    Ok(r)
}

/// 最初の表の「直に当たる符号語」の値（並べた番号 `index` と長さ `len`。モジュールの説明）。
fn direct_entry(index: usize, len: u8) -> u32 {
    (u32::from(len) << 24) | (index as u32 & 0x00ff_ffff)
}

/// 下位 `bits` ビット（0〜32）の印（`mask[bits]`）。
#[inline]
fn low_mask(bits: i32) -> u64 {
    (1u64 << bits.clamp(0, 32)) - 1
}

/// 値のない codebook で値を読もうとしたときの誤り。
#[cold]
fn no_values() -> EngineError {
    EngineError::Decode {
        codec: super::DEC_ID.to_string(),
        message: "値のない codebook で値を読もうとしました".into(),
    }
}

/// 復号のループの読み手：パケットの終わりから遠い間は窓（[`Window`]）から読み、近づいたら読み手に戻して C の形
/// （[`DecodeBook::decode_packed_entry_number`]）で読む。落とすときに読み手を窓の位置に戻す。
struct Entries<'r, 'a> {
    b: &'r mut OggpackReader<'a>,
    win: Option<Window<'a>>,
}

impl<'r, 'a> Entries<'r, 'a> {
    fn new(b: &'r mut OggpackReader<'a>) -> Self {
        let win = Window::open(b);
        Self { b, win }
    }

    /// 次の並べた番号か −1（`decode_packed_entry_number` と同じ）。
    #[inline]
    fn next(&mut self, book: &DecodeBook) -> i64 {
        if let Some(w) = self.win.as_mut() {
            if w.fill() {
                return book.decode_window(w);
            }
            w.close(self.b);
            self.win = None;
        }
        book.decode_packed_entry_number(self.b)
    }
}

impl Drop for Entries<'_, '_> {
    fn drop(&mut self) {
        if let Some(w) = self.win.take() {
            w.close(self.b);
        }
    }
}

/// `bitreverse`（sharedbook.c:319–325）。
fn bitreverse(x: u32) -> u32 {
    x.reverse_bits()
}

/// 復号用の codebook（`codebook` の復号の欄）。
#[derive(Debug, Clone, Default)]
pub(crate) struct DecodeBook {
    pub dim: i64,
    pub used_entries: i64,
    /// 値（使う項目 × dim。値のない codebook は空）。
    pub valuelist: Vec<f32>,
    codelist: Vec<u32>,
    dec_index: Vec<i32>,
    dec_codelengths: Vec<u8>,
    dec_firsttable: Vec<u32>,
    dec_firsttablen: i32,
    dec_maxlength: i32,
}

impl DecodeBook {
    /// `vorbis_book_init_decode`（sharedbook.c:333–458）。符号語の木が正しくなければ None（C の −1）。
    pub fn init(s: &StaticCodebook) -> Result<Option<Self>, EngineError> {
        Self::init_with(s, FIRSTTABLE_MAX)
    }

    /// [`DecodeBook::init`] の、最初の表を最長の符号語の長さの `widen` ビットまで広げるもの（0 なら C と同じ幅）。
    fn init_with(s: &StaticCodebook, widen: i32) -> Result<Option<Self>, EngineError> {
        let mut c = DecodeBook {
            dim: s.dim,
            ..DecodeBook::default()
        };
        // count actually used entries and find max length
        let n = s.lengthlist.iter().filter(|l| **l > 0).count();
        c.used_entries = n as i64;
        if n == 0 {
            return Ok(Some(c));
        }
        // perform sort（確保の失敗は `Limit`、木が正しくなければ None）
        let mut codes = match make_words(&s.lengthlist, s.entries, n as i64) {
            Ok(codes) => codes,
            Err(e @ EngineError::Limit(_)) => return Err(e),
            Err(_) => return Ok(None),
        };
        for code in codes.iter_mut() {
            *code = bitreverse(*code);
        }
        let mut codep: Vec<usize> = zeroed(n)?;
        for (i, p) in codep.iter_mut().enumerate() {
            *p = i;
        }
        codep.sort_by_key(|&i| codes[i]);
        // the index is a reverse index
        let mut sortindex: Vec<i32> = zeroed(n)?;
        for (i, &position) in codep.iter().enumerate() {
            sortindex[position] = i as i32;
        }
        c.codelist = zeroed(n)?;
        for i in 0..n {
            c.codelist[sortindex[i] as usize] = codes[i];
        }
        c.valuelist = book_unquantize(s, n as i32, &sortindex)?;
        c.dec_index = zeroed(n)?;
        c.dec_codelengths = zeroed(n)?;
        let mut k = 0usize;
        for (i, &l) in s.lengthlist.iter().enumerate() {
            if l > 0 {
                let at = sortindex[k] as usize;
                c.dec_index[at] = i as i32;
                c.dec_codelengths[at] = l;
                c.dec_maxlength = c.dec_maxlength.max(i32::from(l));
                k += 1;
            }
        }
        if n == 1 && c.dec_maxlength == 1 {
            // special case the 'single entry codebook' with a single bit fastpath table (that always returns entry 0)
            c.dec_firsttablen = 1;
            c.dec_firsttable = zeroed(2)?;
            c.dec_firsttable.fill(direct_entry(0, 1));
        } else {
            // C は clamp(ilog(used) − 4, 5, 8)。最長の符号語まで（FIRSTTABLE_MAX まで）広げる（モジュールの説明）
            c.dec_firsttablen = (ov_ilog(c.used_entries as u32) - 4)
                .clamp(5, 8)
                .max(c.dec_maxlength.min(widen));
            let tabn = 1usize << c.dec_firsttablen;
            c.dec_firsttable = zeroed(tabn)?;
            for i in 0..n {
                let len = i32::from(c.dec_codelengths[i]);
                if len <= c.dec_firsttablen {
                    let orig = bitreverse(c.codelist[i]);
                    for j in 0..(1u32 << (c.dec_firsttablen - len)) {
                        c.dec_firsttable[(orig | (j << len)) as usize] =
                            direct_entry(i, c.dec_codelengths[i]);
                    }
                }
            }
            // now fill in 'unused' entries in the firsttable with hi/lo search hints for the non-direct-hits
            // C：ogg_uint32_t mask=0xfffffffeUL<<(31-c->dec_firsttablen);（unsigned long で ずらして 32 ビットに切る）
            let mask = ((0xffff_fffe_u64 << (31 - c.dec_firsttablen)) & 0xffff_ffff) as u32;
            let (mut lo, mut hi) = (0usize, 0usize);
            for i in 0..tabn {
                let word = (i as u32) << (32 - c.dec_firsttablen);
                let slot = bitreverse(word) as usize;
                if c.dec_firsttable[slot] == 0 {
                    while lo + 1 < n && c.codelist[lo + 1] <= word {
                        lo += 1;
                    }
                    while hi < n && word >= (c.codelist[hi] & mask) {
                        hi += 1;
                    }
                    // we only actually have 15 bits per hint to play with here.
                    let loval = (lo as u32).min(0x7fff);
                    let hival = ((n - hi) as u32).min(0x7fff);
                    c.dec_firsttable[slot] = 0x8000_0000 | (loval << 15) | hival;
                }
            }
        }
        Ok(Some(c))
    }

    /// `decode_packed_entry_number`（codebook.c:452–505）。並べ替えた番号か −1。パケットの終わりまで 8 バイト以上あれば、
    /// まとめて読んだ語で C と同じ流れをたどる（覗きは −1 にならない。モジュールの説明）。
    #[inline]
    fn decode_packed_entry_number(&self, b: &mut OggpackReader<'_>) -> i64 {
        let Some(word) = b.peek_word() else {
            return self.decode_near_end(b);
        };
        let (entry, adv) = self.decode_word(word);
        b.vadv(adv as i32);
        entry
    }

    /// 覗いた語（下位の 32 ビット以上が有効で、aoTuV の覗きが −1 にならないもの）から、並べた番号か −1 と進むビット数。
    #[inline]
    fn decode_word(&self, word: u64) -> (i64, u32) {
        let Some(&entry) = self
            .dec_firsttable
            .get((word & low_mask(self.dec_firsttablen)) as usize)
        else {
            return (-1, 0);
        };
        if entry & 0x8000_0000 == 0 {
            return (i64::from(entry & 0x00ff_ffff), entry >> 24);
        }
        let lo = i64::from((entry >> 15) & 0x7fff);
        let hi = self.used_entries - i64::from(entry & 0x7fff);
        let read = self.dec_maxlength;
        self.search((word & low_mask(read)) as u32, read, lo, hi)
    }

    /// [`DecodeBook::decode_packed_entry_number`] の、窓（[`Window`]。32 ビット以上あるもの）から読む形。
    #[inline]
    fn decode_window(&self, w: &mut Window<'_>) -> i64 {
        let (entry, adv) = self.decode_word(w.bits());
        w.consume(adv);
        entry
    }

    /// [`DecodeBook::decode_packed_entry_number`] の、パケットの終わりまで 8 バイトないとき（C の流れのまま）。
    #[inline(never)]
    fn decode_near_end(&self, b: &mut OggpackReader<'_>) -> i64 {
        let mut read = self.dec_maxlength;
        let (lo, hi): (i64, i64);
        let lok = b.vlook(self.dec_firsttablen);
        if lok >= 0 {
            let Some(&entry) = self.dec_firsttable.get(lok as usize) else {
                return -1;
            };
            if entry & 0x8000_0000 != 0 {
                lo = i64::from((entry >> 15) & 0x7fff);
                hi = self.used_entries - i64::from(entry & 0x7fff);
            } else {
                b.vadv((entry >> 24) as i32);
                return i64::from(entry & 0x00ff_ffff);
            }
        } else {
            lo = 0;
            hi = self.used_entries;
        }
        let mut lok = b.vlook(read);
        while lok < 0 && read > 1 {
            read -= 1;
            lok = b.vlook(read);
        }
        if lok < 0 {
            return -1;
        }
        let (entry, adv) = self.search(lok as u32, read, lo, hi);
        b.vadv(adv as i32);
        entry
    }

    /// 覗いた `read` ビット（`lok`）の符号語を、並べた符号語の `lo..hi` から二分探索で探す（codebook.c:489–504）。並べた番号か
    /// −1 と、進むビット数（見つかれば符号語の長さ、なければ `read`）。
    #[inline]
    fn search(&self, lok: u32, read: i32, mut lo: i64, mut hi: i64) -> (i64, u32) {
        // bisect search for the codeword in the ordered list
        let testword = bitreverse(lok);
        while hi - lo > 1 {
            let p = (hi - lo) >> 1;
            let test = i64::from(self.codelist[(lo + p) as usize] > testword);
            lo += p & (test - 1);
            hi -= p & (-test);
        }
        let len = i32::from(self.dec_codelengths[lo as usize]);
        if len <= read {
            return (lo, len as u32);
        }
        (-1, read as u32)
    }

    #[inline]
    fn values(&self, entry: i64) -> Result<&[f32], EngineError> {
        let dim = self.dim as usize;
        let start = entry as usize * dim;
        match self.valuelist.get(start..start + dim) {
            Some(t) => Ok(t),
            None => Err(no_values()),
        }
    }

    /// `vorbis_book_decode`（codebook.c:518–527）：元の項目の番号か −1。
    pub fn decode(&self, b: &mut OggpackReader<'_>) -> i64 {
        if self.used_entries > 0 {
            let packed = self.decode_packed_entry_number(b);
            if packed >= 0 {
                return i64::from(self.dec_index[packed as usize]);
            }
        }
        -1
    }

    /// `vorbis_book_decodevs_add`（codebook.c:531–548）。0 か −1（終わり）。C は `step` 個の番号を先に全部読み、どれかが
    /// 読めなければ何も足さずに −1 を返す。移植は番号を持たずに、写した読み手で先に読めるかを確かめてから、もう一度読んで
    /// 足す（番号は同じ。`a` の各要素には 1 回ずつ足すので、足す順は値に効かない）。
    pub fn decodevs_add(
        &self,
        a: &mut [f32],
        b: &mut OggpackReader<'_>,
        n: i32,
    ) -> Result<i64, EngineError> {
        if self.used_entries > 0 {
            // C：int step=n/book->dim;（dim 0 は 0 で割る。照合用の C（arm64 の sdiv）は 0 になり、何も読まずに 0 を返す）
            let step = i64::from(n).checked_div(self.dim).unwrap_or(0).max(0) as usize;
            let mut probe = *b;
            for _ in 0..step {
                if self.decode_packed_entry_number(&mut probe) == -1 {
                    *b = probe;
                    return Ok(-1);
                }
            }
            let n = n.max(0) as usize;
            let dim = self.dim as usize;
            for j in 0..step {
                let e = self.decode_packed_entry_number(b);
                let t = self.values(e)?;
                // C：for(i=0,o=0;i<dim;i++,o+=step) for(j=0;o+j<n && j<step;j++) a[o+j]+=t[j][i];
                for (i, v) in t.iter().enumerate().take(dim) {
                    let at = i * step + j;
                    if at >= n {
                        break;
                    }
                    a[at] += *v;
                }
            }
        }
        Ok(0)
    }

    /// `vorbis_book_decodev_add`（codebook.c:551–566）。
    pub fn decodev_add(
        &self,
        a: &mut [f32],
        b: &mut OggpackReader<'_>,
        n: i32,
    ) -> Result<i64, EngineError> {
        if self.used_entries > 0 {
            let n = n as usize;
            let mut i = 0usize;
            let mut src = Entries::new(b);
            while i < n {
                let entry = src.next(self);
                if entry == -1 {
                    return Ok(-1);
                }
                let t = self.values(entry)?;
                let mut j = 0usize;
                while i < n && j < t.len() {
                    a[i] += t[j];
                    i += 1;
                    j += 1;
                }
            }
        }
        Ok(0)
    }

    /// `vorbis_book_decodev_set`（codebook.c:570–590）。
    pub fn decodev_set(
        &self,
        a: &mut [f32],
        b: &mut OggpackReader<'_>,
        n: i32,
    ) -> Result<i64, EngineError> {
        let n = n as usize;
        if self.used_entries > 0 {
            let mut i = 0usize;
            while i < n {
                let entry = self.decode_packed_entry_number(b);
                if entry == -1 {
                    return Ok(-1);
                }
                let t = self.values(entry)?;
                let mut j = 0usize;
                while i < n && j < t.len() {
                    a[i] = t[j];
                    i += 1;
                    j += 1;
                }
            }
        } else {
            a[..n].fill(0.0);
        }
        Ok(0)
    }

    /// `vorbis_book_decodevv_add`（codebook.c:593–934 の SSE でない経路）：`ch` チャンネルを交互に足す。
    pub fn decodevv_add(
        &self,
        a: &mut [&mut [f32]],
        offset: i64,
        ch: i32,
        b: &mut OggpackReader<'_>,
        n: i32,
    ) -> Result<i64, EngineError> {
        if self.used_entries > 0 && ch == 2 && a.len() == 2 {
            // 2 チャンネル（いちばん多い形）：列を先に分けて借りる（足す先と順は下の一般の形と同じ）
            let (l, r) = a.split_at_mut(1);
            let (a0, a1) = (&mut *l[0], &mut *r[0]);
            let m = ((offset + i64::from(n)) / 2) as usize;
            let mut i = (offset / 2) as usize;
            let mut second = false;
            let pairs = self.dim as usize / 2;
            let even = self.dim % 2 == 0;
            let mut src = Entries::new(b);
            while i < m {
                let entry = src.next(self);
                if entry == -1 {
                    return Ok(-1);
                }
                let t = self.values(entry)?;
                // 左から始まり、dim が偶数で、この項目の値がすべて m の内に入るとき：値は左・右・左・右…の順に
                // `i..i + dim/2` へ 1 つずつ足す（下の 1 つずつの形と同じ足し算）
                if !second
                    && even
                    && i + pairs <= m
                    && let (Some(d0), Some(d1)) =
                        (a0.get_mut(i..i + pairs), a1.get_mut(i..i + pairs))
                {
                    for ((x0, x1), v) in d0.iter_mut().zip(d1).zip(t.as_chunks::<2>().0) {
                        *x0 += v[0];
                        *x1 += v[1];
                    }
                    i += pairs;
                    continue;
                }
                for &v in t {
                    if i >= m {
                        break;
                    }
                    if second {
                        a1[i] += v;
                        i += 1;
                    } else {
                        a0[i] += v;
                    }
                    second = !second;
                }
            }
            return Ok(0);
        }
        if self.used_entries > 0 {
            let ch = i64::from(ch);
            let m = (offset + i64::from(n)) / ch;
            let mut i = offset / ch;
            let mut chptr = 0usize;
            let mut src = Entries::new(b);
            while i < m {
                let entry = src.next(self);
                if entry == -1 {
                    return Ok(-1);
                }
                let t = self.values(entry)?;
                let mut j = 0usize;
                while i < m && j < t.len() {
                    a[chptr][i as usize] += t[j];
                    chptr += 1;
                    if chptr as i64 == ch {
                        chptr = 0;
                        i += 1;
                    }
                    j += 1;
                }
            }
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vorbis::bitpack::OggpackBuffer;

    #[test]
    fn quantvals_match_the_exact_definition() {
        for entries in [
            1i64,
            2,
            3,
            8,
            9,
            10,
            15,
            16,
            17,
            80,
            81,
            255,
            256,
            625,
            1000,
            6561,
            1 << 20,
        ] {
            for dim in 1i64..=8 {
                let b = StaticCodebook {
                    dim,
                    entries,
                    lengthlist: vec![],
                    maptype: 1,
                    q_min: 0,
                    q_delta: 0,
                    q_quant: 1,
                    q_sequencep: 0,
                    quantlist: vec![],
                };
                let v = maptype1_quantvals(&b);
                assert!(
                    v.checked_pow(dim as u32).is_some_and(|p| p <= entries),
                    "{entries} {dim}"
                );
                assert!(
                    (v + 1).checked_pow(dim as u32).is_none_or(|p| p > entries),
                    "{entries} {dim}"
                );
            }
        }
    }

    /// 数えるだけの読み（`scan_book`）が、確保する読み（`staticbook_unpack`）と同じ所で断り、同じ量・同じ位置で終わる
    /// （books.bin の全冊と、ChaCha8 で変え・切ったもの）。
    #[test]
    fn scan_agrees_with_unpack() {
        use rand_chacha::rand_core::Rng;
        let mut rng = crate::determinism::rng(29);
        let (mut some, mut none) = (0, 0);
        for k in 0..vorbis_tables::BOOKS.len() {
            let (bytes, _) = vorbis_tables::packed(vorbis_tables::BookId(k as u16)).unwrap();
            for round in 0..6 {
                let mut b = bytes.to_vec();
                b.extend_from_slice(&[0u8; 8]);
                if round > 0 {
                    for _ in 0..1 + rng.next_u32() % 4 {
                        let i = (rng.next_u32() as usize) % b.len();
                        b[i] ^= 1 << (rng.next_u32() % 8);
                    }
                    if rng.next_u32().is_multiple_of(3) {
                        let cut = (rng.next_u32() as usize) % b.len();
                        b.truncate(cut);
                    }
                }
                let mut r1 = OggpackReader::new(&b);
                let mut r2 = OggpackReader::new(&b);
                let got = staticbook_unpack(&mut r1, u64::MAX).unwrap();
                let scanned = scan_book(&mut r2);
                match got {
                    Some(c) => {
                        let used = c.lengthlist.iter().filter(|l| **l > 0).count() as i64;
                        let want =
                            book_bytes(c.entries, used, c.dim, c.maptype, c.quantlist.len() as i64);
                        assert_eq!(scanned, Some(want), "本 {k} 回 {round}");
                        assert_eq!(
                            (r1.endbyte, r1.endbit),
                            (r2.endbyte, r2.endbit),
                            "本 {k} 回 {round}"
                        );
                        some += 1;
                    }
                    None => {
                        assert_eq!(scanned, None, "本 {k} 回 {round}");
                        none += 1;
                    }
                }
            }
        }
        // どちらの枝も十分に通る
        assert!(some > 100 && none > 100, "{some} {none}");
    }

    /// ordered の codebook は数十ビットで 2^22 項目を宣言できる（長さ 22 が 2^22 個）。256 冊並べた setup（数 KB）でも、
    /// 予算を超える分は確保の前に断る（`Unsupported`）。数えるだけの読みは全体の量を返す。
    #[test]
    fn huge_ordered_books_are_refused_before_allocating() {
        let entries = 1i64 << 22;
        let mut w = OggpackBuffer::oggpack_writeinit();
        let put = |w: &mut OggpackBuffer, v: u64, bits: u32| w.oggpack_write(v, bits).unwrap();
        put(&mut w, 5, 8);
        for &c in b"vorbis" {
            put(&mut w, u64::from(c), 8);
        }
        put(&mut w, 255, 8);
        for _ in 0..256 {
            put(&mut w, 0x56_4342, 24);
            put(&mut w, 1, 16);
            put(&mut w, entries as u64, 24);
            put(&mut w, 1, 1);
            put(&mut w, 21, 5);
            put(&mut w, entries as u64, ov_ilog(entries as u32) as u32);
            put(&mut w, 0, 4);
        }
        let bytes = w.oggpack_get_buffer().to_vec();
        assert!(bytes.len() < 4 << 10, "{}", bytes.len());
        let one = book_bytes(entries, entries, 1, 0, 0);
        let all = (0..256).fold(TableBytes::default(), |a, _| a.add(one));
        assert_eq!(setup_table_bytes(&bytes), Some(all));
        let one = one.total();
        let mut r = OggpackReader::new(&bytes);
        r.read(8);
        for _ in 0..6 {
            r.read(8);
        }
        let err = super::super::info::unpack_books(2, &mut r, 256 << 20).unwrap_err();
        assert!(matches!(err, EngineError::Unsupported(_)), "{err:?}");
        // 1 冊は約 104 MiB（長さ 4 MiB と使う項目ごとの 25 バイト）。3 冊目を足したところで断る
        assert!(2 * one < 256 << 20 && 3 * one > 256 << 20);
    }

    /// 最初の表を広げても（[`FIRSTTABLE_MAX`]）、C と同じ幅の表と同じ番号を読み、同じだけ進み、同じ所で −1 になる
    /// （books.bin の全冊 × ChaCha8 の短いビット列。パケットの終わりの近くを多く通す）。
    #[test]
    fn wide_first_table_reads_like_the_c_width() {
        use rand_chacha::rand_core::Rng;
        let mut rng = crate::determinism::rng(31);
        let mut widened = 0;
        for k in 0..vorbis_tables::BOOKS.len() {
            let s = vorbis_tables::unpack(vorbis_tables::BookId(k as u16)).unwrap();
            let c = DecodeBook::init_with(&s, 0).unwrap().unwrap();
            let w = DecodeBook::init(&s).unwrap().unwrap();
            if w.dec_firsttablen > c.dec_firsttablen {
                widened += 1;
            }
            for _ in 0..40 {
                let len = 1 + (rng.next_u32() % 12) as usize;
                let bytes: Vec<u8> = (0..len).map(|_| rng.next_u32() as u8).collect();
                let (mut rc, mut rw) = (OggpackReader::new(&bytes), OggpackReader::new(&bytes));
                loop {
                    let (a, b) = (
                        c.decode_packed_entry_number(&mut rc),
                        w.decode_packed_entry_number(&mut rw),
                    );
                    assert_eq!(a, b, "本 {k}");
                    assert_eq!((rc.endbyte, rc.endbit), (rw.endbyte, rw.endbit), "本 {k}");
                    if a == -1 || rc.endbyte >= rc.storage {
                        break;
                    }
                }
            }
        }
        assert!(widened > 50, "{widened}");
    }

    /// C の最初の表（sharedbook.c:401–455：C の幅 `clamp(ilog(used) − 4, 5, 8)`・直に当たる値は `番号 + 1`）。符号語の列と
    /// 長さは [`DecodeBook::init`] が C と同じ形に作ったもの（`codelist`・`dec_codelengths`）を使う。
    fn c_firsttable(c: &DecodeBook) -> (Vec<u32>, i32) {
        let n = c.used_entries as usize;
        if n == 1 && c.dec_maxlength == 1 {
            return (vec![1, 1], 1);
        }
        let tablen = (ov_ilog(c.used_entries as u32) - 4).clamp(5, 8);
        let tabn = 1usize << tablen;
        let mut table = vec![0u32; tabn];
        for i in 0..n {
            let len = i32::from(c.dec_codelengths[i]);
            if len <= tablen {
                let orig = bitreverse(c.codelist[i]);
                for j in 0..(1u32 << (tablen - len)) {
                    table[(orig | (j << len)) as usize] = (i + 1) as u32;
                }
            }
        }
        let mask = ((0xffff_fffe_u64 << (31 - tablen)) & 0xffff_ffff) as u32;
        let (mut lo, mut hi) = (0usize, 0usize);
        for i in 0..tabn {
            let word = (i as u32) << (32 - tablen);
            let slot = bitreverse(word) as usize;
            if table[slot] == 0 {
                while lo + 1 < n && c.codelist[lo + 1] <= word {
                    lo += 1;
                }
                while hi < n && word >= (c.codelist[hi] & mask) {
                    hi += 1;
                }
                let loval = (lo as u32).min(0x7fff);
                let hival = ((n - hi) as u32).min(0x7fff);
                table[slot] = 0x8000_0000 | (loval << 15) | hival;
            }
        }
        (table, tablen)
    }

    /// C の `decode_packed_entry_number`（codebook.c:452–505）を、C の表（[`c_firsttable`]）と aoTuV の覗き（`vlook`・`vadv`）で
    /// そのまま組んだもの。
    fn c_decode(c: &DecodeBook, table: &[u32], tablen: i32, b: &mut OggpackReader<'_>) -> i64 {
        let mut read = c.dec_maxlength;
        let (mut lo, mut hi): (i64, i64);
        let lok = b.vlook(tablen);
        if lok >= 0 {
            let entry = i64::from(table[lok as usize]);
            if entry & 0x8000_0000 != 0 {
                lo = (entry >> 15) & 0x7fff;
                hi = c.used_entries - (entry & 0x7fff);
            } else {
                b.vadv(i32::from(c.dec_codelengths[(entry - 1) as usize]));
                return entry - 1;
            }
        } else {
            lo = 0;
            hi = c.used_entries;
        }
        let mut lok = b.vlook(read);
        while lok < 0 && read > 1 {
            read -= 1;
            lok = b.vlook(read);
        }
        if lok < 0 {
            return -1;
        }
        let testword = bitreverse(lok as u32);
        while hi - lo > 1 {
            let p = (hi - lo) >> 1;
            let test = i64::from(c.codelist[(lo + p) as usize] > testword);
            lo += p & (test - 1);
            hi -= p & (-test);
        }
        let len = i32::from(c.dec_codelengths[lo as usize]);
        if len <= read {
            b.vadv(len);
            return lo;
        }
        b.vadv(read);
        -1
    }

    /// 復号（広げた表・表の値の形・8 バイトの覗きの速い道・窓）が、C の表と C の流れ（[`c_decode`]）と同じ番号を読み、同じだけ
    /// 進み、同じ所で −1 になる（books.bin の全冊 × ChaCha8 のビット列。1〜48 バイトで、速い道とパケットの終わりの近くの
    /// 両方を通す。1 つの項目だけの codebook も）。
    #[test]
    fn decode_reads_like_the_c_decoder() {
        use rand_chacha::rand_core::Rng;
        let mut rng = crate::determinism::rng(37);
        let mut books: Vec<StaticCodebook> = (0..vorbis_tables::BOOKS.len())
            .map(|k| vorbis_tables::unpack(vorbis_tables::BookId(k as u16)).unwrap())
            .collect();
        // 1 つの項目だけ（長さ 1。sharedbook.c が受け付ける唯一の足りない木）
        books.push(StaticCodebook {
            dim: 1,
            entries: 4,
            lengthlist: vec![0, 1, 0, 0],
            maptype: 0,
            q_min: 0,
            q_delta: 0,
            q_quant: 0,
            q_sequencep: 0,
            quantlist: vec![],
        });
        let (mut fast, mut near) = (0u64, 0u64);
        for (k, s) in books.iter().enumerate() {
            let w = DecodeBook::init(s).unwrap().unwrap();
            let (table, tablen) = c_firsttable(&w);
            for _ in 0..60 {
                let len = 1 + (rng.next_u32() % 48) as usize;
                let bytes: Vec<u8> = (0..len).map(|_| rng.next_u32() as u8).collect();
                let (mut rc, mut rw) = (OggpackReader::new(&bytes), OggpackReader::new(&bytes));
                // C の流れの番号と、各回のあとの位置
                let mut seq = Vec::new();
                loop {
                    if rw.peek_word().is_some() {
                        fast += 1;
                    } else {
                        near += 1;
                    }
                    let (a, b) = (
                        c_decode(&w, &table, tablen, &mut rc),
                        w.decode_packed_entry_number(&mut rw),
                    );
                    assert_eq!(a, b, "本 {k}");
                    assert_eq!((rc.endbyte, rc.endbit), (rw.endbyte, rw.endbit), "本 {k}");
                    seq.push((a, rc.endbyte, rc.endbit));
                    if a == -1 || rc.endbyte >= rc.storage {
                        break;
                    }
                }
                // 窓で読む復号のループ（`Entries`）：途中の回数で止めて、落としたあとの位置も同じ（途中から読み直しても同じ）
                for _ in 0..3 {
                    let stop = 1 + (rng.next_u32() as usize) % seq.len();
                    let mut r = OggpackReader::new(&bytes);
                    let mut got = Vec::new();
                    {
                        let mut src = Entries::new(&mut r);
                        for _ in 0..stop {
                            got.push(src.next(&w));
                        }
                    }
                    let want: Vec<i64> = seq[..stop].iter().map(|s| s.0).collect();
                    assert_eq!(got, want, "本 {k}");
                    assert_eq!(
                        (r.endbyte, r.endbit),
                        (seq[stop - 1].1, seq[stop - 1].2),
                        "本 {k}"
                    );
                    let mut src = Entries::new(&mut r);
                    for s in &seq[stop..] {
                        assert_eq!(src.next(&w), s.0, "本 {k}");
                    }
                }
            }
        }
        assert!(fast > 100_000 && near > 10_000, "{fast} {near}");
    }

    /// C の `vorbis_book_decodevv_add`（codebook.c:915–928 の SSE でない経路）をそのまま組んだもの（C の流れの読み
    /// [`c_decode`] と値の表で）。
    fn c_decodevv_add(
        c: &DecodeBook,
        table: &[u32],
        tablen: i32,
        a: &mut [Vec<f32>],
        offset: i64,
        ch: i64,
        b: &mut OggpackReader<'_>,
        n: i64,
    ) -> i64 {
        if c.used_entries > 0 {
            let mut chptr = 0usize;
            let mut i = offset / ch;
            while i < (offset + n) / ch {
                let entry = c_decode(c, table, tablen, b);
                if entry == -1 {
                    return -1;
                }
                let dim = c.dim as usize;
                let t = &c.valuelist[entry as usize * dim..entry as usize * dim + dim];
                let mut j = 0usize;
                while i < (offset + n) / ch && j < dim {
                    a[chptr][i as usize] += t[j];
                    chptr += 1;
                    if chptr as i64 == ch {
                        chptr = 0;
                        i += 1;
                    }
                    j += 1;
                }
            }
        }
        0
    }

    /// `decodevv_add`（2 チャンネルの速い道と一般の形・窓）が、C の流れと同じ値を同じ順に足し、同じ所まで読む（books.bin の
    /// 値のある冊 × ChaCha8：チャンネル 1〜4、奇数を含む offset、dim の倍数でない n、短いビット列でパケットの終わりも通す）。
    #[test]
    fn decodevv_add_matches_the_c_loop() {
        use rand_chacha::rand_core::Rng;
        let mut rng = crate::determinism::rng(61);
        let mut dims = std::collections::BTreeSet::new();
        let mut cases = 0;
        for k in 0..vorbis_tables::BOOKS.len() {
            let s = vorbis_tables::unpack(vorbis_tables::BookId(k as u16)).unwrap();
            if s.maptype == 0 {
                continue;
            }
            let w = DecodeBook::init(&s).unwrap().unwrap();
            let (table, tablen) = c_firsttable(&w);
            dims.insert(w.dim);
            for _ in 0..6 {
                let ch = 1 + i64::from(rng.next_u32() % 4);
                let offset = i64::from(rng.next_u32() % 40);
                let n = i64::from(rng.next_u32() % 90);
                let len = 1 + (rng.next_u32() % 64) as usize;
                let bytes: Vec<u8> = (0..len).map(|_| rng.next_u32() as u8).collect();
                let size = ((offset + n) / ch + 1) as usize;
                let init: Vec<Vec<f32>> = (0..ch)
                    .map(|_| {
                        (0..size)
                            .map(|_| (rng.next_u32() % 7) as f32 - 3.0)
                            .collect()
                    })
                    .collect();
                let mut want = init.clone();
                let mut rc = OggpackReader::new(&bytes);
                let rw_ = c_decodevv_add(&w, &table, tablen, &mut want, offset, ch, &mut rc, n);
                let mut got = init;
                let mut rg = OggpackReader::new(&bytes);
                let mut views: Vec<&mut [f32]> = got.iter_mut().map(Vec::as_mut_slice).collect();
                let rg_ = w
                    .decodevv_add(&mut views, offset, ch as i32, &mut rg, n as i32)
                    .unwrap();
                assert_eq!(rg_, rw_, "本 {k}");
                assert_eq!((rg.endbyte, rg.endbit), (rc.endbyte, rc.endbit), "本 {k}");
                let bits = |v: &[Vec<f32>]| -> Vec<u32> {
                    v.iter().flatten().map(|f| f.to_bits()).collect()
                };
                assert_eq!(
                    bits(&got),
                    bits(&want),
                    "本 {k} ch {ch} offset {offset} n {n}"
                );
                cases += 1;
            }
        }
        assert!(cases > 1_000 && dims.len() >= 4, "{cases} {dims:?}");
    }

    /// dim 0 の codebook（maptype 1・値 0 個）を residue 0 が使っても 0 で割らない。照合用の C（arm64）と同じく何も読まずに
    /// 0 を返す。
    #[test]
    fn residue0_with_a_dim0_book_reads_nothing() {
        let s = StaticCodebook {
            dim: 0,
            entries: 2,
            lengthlist: vec![1, 1],
            maptype: 1,
            q_min: 0,
            q_delta: 0,
            q_quant: 1,
            q_sequencep: 0,
            quantlist: vec![],
        };
        let d = DecodeBook::init(&s).unwrap().unwrap();
        let bytes = [0xffu8; 8];
        let mut r = OggpackReader::new(&bytes);
        let mut a = [1.0f32; 8];
        assert_eq!(d.decodevs_add(&mut a, &mut r, 8).unwrap(), 0);
        assert_eq!(a, [1.0; 8]);
        assert_eq!((r.endbyte, r.endbit), (0, 0));
    }

    /// 書き出しの移植が詰めた codebook（books.bin の全冊）を読み戻し、復号の表で全部の項目を読める。
    #[test]
    fn unpacks_and_decodes_every_shipped_book() {
        for k in 0..vorbis_tables::BOOKS.len() {
            let id = vorbis_tables::BookId(k as u16);
            let s = vorbis_tables::unpack(id).unwrap();
            // C の vorbis_staticbook_pack が詰めたバイト列（books.bin）。setup の中では後ろに続きがあるので、残りのバイト数の
            // 確かめ（codebook.c:299・374）が 1 冊だけで切れないよう、後ろに詰め物を足す
            let (bytes, _) = vorbis_tables::packed(id).unwrap();
            let mut bytes = bytes.to_vec();
            bytes.extend_from_slice(&[0u8; 8]);
            let mut r = OggpackReader::new(&bytes);
            let back = staticbook_unpack(&mut r, u64::MAX)
                .unwrap()
                .unwrap_or_else(|| panic!("本 {k}"));
            assert_eq!(back, s, "本 {k}");
            let d = DecodeBook::init(&back).unwrap().unwrap();
            // 項目ごとに符号語を書いて読む
            let words = make_words(&s.lengthlist, s.entries, 0).unwrap();
            let mut w = OggpackBuffer::oggpack_writeinit();
            let mut want = Vec::new();
            for (e, &l) in s.lengthlist.iter().enumerate() {
                if l > 0 {
                    w.oggpack_write(u64::from(words[e]), u32::from(l)).unwrap();
                    want.push(e as i64);
                }
            }
            let bytes = w.oggpack_get_buffer().to_vec();
            let mut r = OggpackReader::new(&bytes);
            for e in want {
                assert_eq!(d.decode(&mut r), e, "本 {k}");
            }
        }
    }
}
