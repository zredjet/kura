//! `vorbis_info`・`codec_setup_info` と 3 つのヘッダの書き出し。
//!
//! # 移す C
//!
//! | 関数 | ファイル:行 | 備考 |
//! |---|---|---|
//! | `vorbis_info_init` | info.c:183–186 | 0 で埋めた `vorbis_info` と `codec_setup_info` |
//! | `vorbis_info_blocksize` | info.c:177–180 | |
//! | `_v_writestring` | info.c:47–52 | 1 バイトずつ 8 ビットで |
//! | `_vorbis_pack_info` | info.c:500–526 | 識別ヘッダ（30 バイト）：種類 1・`vorbis`・版 0（32）・チャンネル（8）・レート（32）・bitrate の上・名目・下（各 32。名目は `setting_to_approx_bitrate`、上下は 0）・`ov_ilog(blocksizes[0]-1)`（4）・`ov_ilog(blocksizes[1]-1)`（4）・1（1） |
//! | `_vorbis_pack_comment` | info.c:528–556 | **kura の版**：vendor は `ENCODE_VENDOR_STRING` でなく呼ぶ側が渡す（kura の書き出しは [`super::VENDOR`]＝`"kura"`。照合用のエンコーダの既定は空）。項目も呼ぶ側が渡し、並びのまま `KEY=value`（`vorbis_comment_add_tag` と同じ形）で書く（E28 の並び）。形は C と同じ（種類 3・`vorbis`・vendor の長さ（32）と本体・項目の数（32）・各項目の長さ（32）と本体・1（1））。長さが 32 ビットに収まらなければ `Unsupported`（16 MiB の上限は encoder.rs が見る） |
//! | `_vorbis_pack_books` | info.c:558–612 | setup ヘッダ：種類 5・`vorbis`・`books-1`（8）・各 codebook・time（6 ビットの 0 と 16 ビットの 0）・floor・residue・mapping・mode・1（1） |
//! | `vorbis_analysis_headerout` | info.c:638–717 | 3 つのパケット（packetno 0・1・2、granulepos 0、最初だけ b_o_s） |
//! | `vorbis_staticbook_pack` | codebook.c:158–273 | books.bin の詰めた形（`vorbis_tables::packed`）を、記録したビット数だけビットのまま写す（C が詰めたものそのもの。vorbis-tables のテストで、読んで詰め直すと同じになることを全冊確かめた） |
//! | `floor1_pack` | floor1.c:77–113 | partitions（5）・class（4）・class ごとに dim−1（3）・subs（2）・book（8）・subbook+1（8 × 2^subs）・mult−1（2）・rangebits = `ov_ilog(postlist[1]-1)`（4）・ポスト（rangebits） |
//! | `icount` | res0.c:151–158 | |
//! | `res0_pack` | res0.c:161–188 | begin・end・grouping−1（各 24）・partitions−1（6）・groupbook（8）・secondstages（3 ビット + 1 ビットの旗 + 5 ビット、または 4 ビット）・booklist（8 × 立っているビットの数の和） |
//! | `mapping0_pack` | mapping0.c:51–92 | submaps・coupling（mag・ang は `ov_ilog(channels-1)` ビット）・予約 2 ビット・chmux・submap ごとの time（8）・floor（8）・residue（8） |
//!
//! ヘッダの識別・setup は照合用のエンコーダのものとビット単位で同じになる。コメントは vendor とタグが違うだけで、同じ vendor
//! （照合用の既定は空、または `oracle_enc --vendor`）・タグなしなら同じになる（テストで確かめた）。
//!
//! C の `oggpack_write(opb, value, bits)` の `value` は `unsigned long`（負の `int`・`long` は 2 の補数のビット列の下位
//! `bits` ビット）。移植は [`write`] で同じにする。
//!
//! # 移さないもの
//!
//! 読み取り（`_vorbis_unpack_*`・`vorbis_synthesis_headerin`）、`vorbis_comment` の API（kura は `(String, String)` の並びで
//! 持つ）、`vorbis_info_clear`、`vorbis_granule_time`、`vorbis_version_string`。
//!
//! # 型の対応
//!
//! - `vorbis_info`（codec.h:27–53）→ [`VorbisInfo`]。`codec_setup` は持ち物の [`CodecSetupInfo`]。
//! - `codec_setup_info`（codec_internal.h:102–140）→ [`CodecSetupInfo`]。`mode_param[64]` などのポインタの配列は
//!   `Vec`（数は `modes` などと同じ）。`book_param[256]`（static へのポインタ）は `Vec<BookId>`、`fullbooks` は
//!   `Vec<Codebook>`。`psy_param[4]` は `Vec<VorbisInfoPsy>`（`psys` 個）。`bi`（ビットレートの管理）・`halfrate_flag`
//!   （デコーダ）は持たない。
//! - floor・residue・mapping の型番号は Vorbis I の番号（floor 1・residue 1 か 2・mapping 0・mode の window 0・transform 0）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use vorbis_tables::{
    BookId, VorbisInfoFloor1, VorbisInfoMapping0, VorbisInfoMode, VorbisInfoPsy,
    VorbisInfoPsyGlobal, VorbisInfoResidue0, ov_ilog, packed,
};

use super::bitpack::OggpackBuffer;
use super::codebook::Codebook;
use super::setup::HighlevelEncodeSetup;
use crate::codec::ogg::OggPacket;
use crate::error::EngineError;

/// `vorbis_info`（codec.h:27–53）。
#[derive(Debug, Clone, Default)]
pub(crate) struct VorbisInfo {
    pub version: i32,
    pub channels: i32,
    pub rate: i64,
    pub bitrate_upper: i64,
    pub bitrate_nominal: i64,
    pub bitrate_lower: i64,
    pub bitrate_window: i64,
    pub codec_setup: CodecSetupInfo,
}

/// `codec_setup_info`（codec_internal.h:102–140）のエンコードに要る欄。
#[derive(Debug, Clone, Default)]
pub(crate) struct CodecSetupInfo {
    pub blocksizes: [i64; 2],
    pub modes: i32,
    pub maps: i32,
    pub floors: i32,
    pub residues: i32,
    pub books: i32,
    pub psys: i32,
    pub mode_param: Vec<VorbisInfoMode>,
    pub map_type: Vec<i32>,
    pub map_param: Vec<VorbisInfoMapping0>,
    pub floor_type: Vec<i32>,
    /// テンプレートの写し（`class_book`・`class_subbook` は全体の book の番号に足し直したもの。vorbisenc.c:209–216）。
    pub floor_param: Vec<VorbisInfoFloor1>,
    pub residue_type: Vec<i32>,
    /// テンプレートの写し（grouping・secondstages・groupbook・booklist・end を埋めたもの。vorbisenc.c:493–627）。
    pub residue_param: Vec<VorbisInfoResidue0>,
    /// C の `static_codebook *book_param[256]`（`books` 個）。
    pub book_param: Vec<BookId>,
    /// C の `codebook *fullbooks`（`vorbis_book_init_encode` で作る。block.c:228–232）。
    pub fullbooks: Vec<Codebook>,
    /// C の `vorbis_info_psy *psy_param[4]`（0 impulse・1 padding・2 transition・3 long）。
    pub psy_param: Vec<VorbisInfoPsy>,
    pub psy_g_param: VorbisInfoPsyGlobal,
    /// vorbisenc.c だけが使う高い層の設定。
    pub hi: HighlevelEncodeSetup,
    /// residue の lowpass（vorbisenc.c:610・618）。
    pub block_lowpassr: [i32; 2],
}

/// `vorbis_info_init`（info.c:183–186）。
pub(crate) fn vorbis_info_init() -> VorbisInfo {
    VorbisInfo::default()
}

/// `vorbis_info_blocksize`（info.c:177–180）：`zo` が 0 なら短い窓、1 なら長い窓の大きさ（範囲の外は −1）。
#[cfg(test)]
pub(crate) fn vorbis_info_blocksize(vi: &VorbisInfo, zo: i32) -> i64 {
    usize::try_from(zo)
        .ok()
        .and_then(|i| vi.codec_setup.blocksizes.get(i))
        .copied()
        .unwrap_or(-1)
}

/// C の `oggpack_write(opb, value, bits)`：`value` は C の `long`・`int`（負の値は 2 の補数のビット列。下位 `bits` ビット
/// だけ書く）。
fn write(opb: &mut OggpackBuffer, value: i64, bits: i32) -> Result<(), EngineError> {
    let bits = u32::try_from(bits)
        .map_err(|_| EngineError::Internal(format!("oggpack_write：{bits} ビットは書けません")))?;
    opb.oggpack_write(value as u64, bits)
}

/// 書けない設定（C の `goto err_out`・`OV_EFAULT`・範囲の外）。
fn bad_setup(what: &str) -> EngineError {
    EngineError::Internal(format!("Vorbis のヘッダ：{what}"))
}

/// `s[i]`（範囲の外は書けない設定）。
fn at<T: Copy>(s: &[T], i: i32, what: &str) -> Result<T, EngineError> {
    usize::try_from(i)
        .ok()
        .and_then(|i| s.get(i))
        .copied()
        .ok_or_else(|| bad_setup(&format!("{what} の添字が範囲の外です")))
}

/// 長さ・数を 32 ビットで書く（収まらなければ `Unsupported`）。
fn write_len32(opb: &mut OggpackBuffer, n: Option<usize>, what: &str) -> Result<(), EngineError> {
    let n = n.and_then(|n| u32::try_from(n).ok()).ok_or_else(|| {
        EngineError::Unsupported(format!("Ogg Vorbis のコメント：{what} が大きすぎます"))
    })?;
    opb.oggpack_write(u64::from(n), 32)
}

/// `_v_writestring`（info.c:47–52）。
fn v_writestring(o: &mut OggpackBuffer, s: &[u8]) -> Result<(), EngineError> {
    for &b in s {
        o.oggpack_write(u64::from(b), 8)?;
    }
    Ok(())
}

/// `_vorbis_pack_info`（info.c:500–526）。
fn vorbis_pack_info(opb: &mut OggpackBuffer, vi: &VorbisInfo) -> Result<(), EngineError> {
    let ci = &vi.codec_setup;
    if ci.blocksizes[0] < 64 || ci.blocksizes[1] < ci.blocksizes[0] {
        return Err(bad_setup("ブロックの大きさが正しくありません"));
    }

    // preamble
    write(opb, 0x01, 8)?;
    v_writestring(opb, b"vorbis")?;

    // basic information about the stream
    write(opb, 0x00, 32)?;
    write(opb, i64::from(vi.channels), 8)?;
    write(opb, vi.rate, 32)?;

    write(opb, vi.bitrate_upper, 32)?;
    write(opb, vi.bitrate_nominal, 32)?;
    write(opb, vi.bitrate_lower, 32)?;

    write(opb, i64::from(ov_ilog((ci.blocksizes[0] - 1) as u32)), 4)?;
    write(opb, i64::from(ov_ilog((ci.blocksizes[1] - 1) as u32)), 4)?;
    write(opb, 1, 1)?;
    Ok(())
}

/// `_vorbis_pack_comment`（info.c:528–556）の kura の版：vendor と項目（`KEY=value`。並びのまま）を書く。
fn vorbis_pack_comment(
    opb: &mut OggpackBuffer,
    vendor: &str,
    comments: &[(String, String)],
) -> Result<(), EngineError> {
    // preamble
    write(opb, 0x03, 8)?;
    v_writestring(opb, b"vorbis")?;

    // vendor
    write_len32(opb, Some(vendor.len()), "vendor")?;
    v_writestring(opb, vendor.as_bytes())?;

    // comments（vorbis_comment_add_tag と同じ `tag=contents`）
    write_len32(opb, Some(comments.len()), "項目の数")?;
    for (key, value) in comments {
        let len = key
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_add(value.len()));
        write_len32(opb, len, "項目")?;
        v_writestring(opb, key.as_bytes())?;
        v_writestring(opb, b"=")?;
        v_writestring(opb, value.as_bytes())?;
    }
    write(opb, 1, 1)?;
    Ok(())
}

/// `_vorbis_pack_books`（info.c:558–612）。
fn vorbis_pack_books(opb: &mut OggpackBuffer, vi: &VorbisInfo) -> Result<(), EngineError> {
    let ci = &vi.codec_setup;
    // 数（`ci->books` など）と、その数だけ並んでいること
    let count = |n: i32, len: usize, what: &str| -> Result<usize, EngineError> {
        usize::try_from(n)
            .ok()
            .filter(|&n| n <= len)
            .ok_or_else(|| bad_setup(&format!("{what} の数が正しくありません")))
    };

    write(opb, 0x05, 8)?;
    v_writestring(opb, b"vorbis")?;

    // books
    let books = count(ci.books, ci.book_param.len(), "book")?;
    write(opb, i64::from(ci.books) - 1, 8)?;
    for &book in ci.book_param.iter().take(books) {
        vorbis_staticbook_pack(book, opb)?;
    }

    // times; hook placeholders
    write(opb, 0, 6)?;
    write(opb, 0, 16)?;

    // floors
    let floors = count(
        ci.floors,
        ci.floor_param.len().min(ci.floor_type.len()),
        "floor",
    )?;
    write(opb, i64::from(ci.floors) - 1, 6)?;
    for (&ty, f) in ci.floor_type.iter().zip(&ci.floor_param).take(floors) {
        write(opb, i64::from(ty), 16)?;
        // _floor_P[1] だけが pack を持つ（floor 0 の pack は NULL → err_out）
        if ty != 1 {
            return Err(bad_setup("floor 1 だけを書けます"));
        }
        floor1_pack(f, opb)?;
    }

    // residues
    let residues = count(
        ci.residues,
        ci.residue_param.len().min(ci.residue_type.len()),
        "residue",
    )?;
    write(opb, i64::from(ci.residues) - 1, 6)?;
    for (&ty, r) in ci.residue_type.iter().zip(&ci.residue_param).take(residues) {
        write(opb, i64::from(ty), 16)?;
        // _residue_P[0〜2] の pack はどれも res0_pack
        if !(0..=2).contains(&ty) {
            return Err(bad_setup("residue の種類が正しくありません"));
        }
        res0_pack(r, opb)?;
    }

    // maps
    let maps = count(
        ci.maps,
        ci.map_param.len().min(ci.map_type.len()),
        "mapping",
    )?;
    write(opb, i64::from(ci.maps) - 1, 6)?;
    for (&ty, m) in ci.map_type.iter().zip(&ci.map_param).take(maps) {
        write(opb, i64::from(ty), 16)?;
        if ty != 0 {
            return Err(bad_setup("mapping 0 だけを書けます"));
        }
        mapping0_pack(vi, m, opb)?;
    }

    // modes
    let modes = count(ci.modes, ci.mode_param.len(), "mode")?;
    write(opb, i64::from(ci.modes) - 1, 6)?;
    for m in ci.mode_param.iter().take(modes) {
        write(opb, i64::from(m.blockflag), 1)?;
        write(opb, i64::from(m.windowtype), 16)?;
        write(opb, i64::from(m.transformtype), 16)?;
        write(opb, i64::from(m.mapping), 8)?;
    }
    write(opb, 1, 1)?;
    Ok(())
}

/// 書いたパケット（C の `ogg_packet`。b_o_s は Ogg の書き手が最初のパケットに付ける）。
fn packet(opb: &OggpackBuffer, packetno: i64) -> OggPacket {
    OggPacket {
        packet: opb.oggpack_get_buffer().to_vec(),
        e_o_s: false,
        granulepos: 0,
        packetno,
    }
}

/// `vorbis_analysis_headerout`（info.c:638–717）：識別・コメント・setup の 3 つのパケット。コメントの vendor と項目
/// （`(KEY, value)`。並びのまま書く）は呼ぶ側が渡す（kura の書き出しの vendor は [`super::VENDOR`]）。
pub(crate) fn vorbis_analysis_headerout(
    vi: &VorbisInfo,
    vendor: &str,
    comments: &[(String, String)],
) -> Result<[OggPacket; 3], EngineError> {
    if vi.channels <= 0 || vi.channels > 256 {
        return Err(bad_setup("チャンネルの数が正しくありません"));
    }

    // first header packet
    let mut opb = OggpackBuffer::oggpack_writeinit();
    vorbis_pack_info(&mut opb, vi)?;
    let op = packet(&opb, 0);

    // second header packet (comments)
    opb.oggpack_reset();
    vorbis_pack_comment(&mut opb, vendor, comments)?;
    let op_comm = packet(&opb, 1);

    // third header packet (modes/codebooks)
    opb.oggpack_reset();
    vorbis_pack_books(&mut opb, vi)?;
    let op_code = packet(&opb, 2);

    Ok([op, op_comm, op_code])
}

/// `vorbis_staticbook_pack`（codebook.c:158–273）：books.bin の詰めた形をビットのまま写す。
fn vorbis_staticbook_pack(c: BookId, opb: &mut OggpackBuffer) -> Result<(), EngineError> {
    let (bytes, bits) =
        packed(c).ok_or_else(|| bad_setup(&format!("codebook {} がありません", c.0)))?;
    let full = (bits / 8) as usize;
    let rest = bits % 8;
    let (body, tail) = bytes
        .split_at_checked(full)
        .ok_or_else(|| bad_setup("codebook の長さが合いません"))?;
    for &b in body {
        opb.oggpack_write(u64::from(b), 8)?;
    }
    if rest > 0 {
        let last = tail
            .first()
            .ok_or_else(|| bad_setup("codebook の長さが合いません"))?;
        opb.oggpack_write(u64::from(*last), rest)?;
    }
    Ok(())
}

/// `floor1_pack`（floor1.c:77–113）。
fn floor1_pack(info: &VorbisInfoFloor1, opb: &mut OggpackBuffer) -> Result<(), EngineError> {
    let mut count = 0i32;
    let maxposit = info.postlist[1];
    let mut maxclass = -1;

    // save out partitions
    write(opb, i64::from(info.partitions), 5)?; // only 0 to 31 legal
    for j in 0..info.partitions {
        let pc = at(&info.partitionclass, j, "partitionclass")?;
        write(opb, i64::from(pc), 4)?; // only 0 to 15 legal
        if maxclass < pc {
            maxclass = pc;
        }
    }

    // save out partition classes
    for j in 0..maxclass + 1 {
        let subs = at(&info.class_subs, j, "class_subs")?;
        write(opb, i64::from(at(&info.class_dim, j, "class_dim")?) - 1, 3)?; // 1 to 8
        write(opb, i64::from(subs), 2)?; // 0 to 3
        if subs != 0 {
            write(opb, i64::from(at(&info.class_book, j, "class_book")?), 8)?;
        }
        let subbooks = at(&info.class_subbook, j, "class_subbook")?;
        let n = u32::try_from(subs)
            .ok()
            .and_then(|s| 1i32.checked_shl(s))
            .ok_or_else(|| bad_setup("class_subs が範囲の外です"))?;
        for k in 0..n {
            write(opb, i64::from(at(&subbooks, k, "class_subbook")?) + 1, 8)?;
        }
    }

    // save out the post list
    write(opb, i64::from(info.mult) - 1, 2)?; // only 1,2,3,4 legal now
    // maxposit cannot legally be less than 1; this is encode-side, we can assume our setup is OK
    write(opb, i64::from(ov_ilog((maxposit - 1) as u32)), 4)?;
    let rangebits = ov_ilog((maxposit - 1) as u32);

    let mut k = 0i32;
    for j in 0..info.partitions {
        count += at(
            &info.class_dim,
            at(&info.partitionclass, j, "partitionclass")?,
            "class_dim",
        )?;
        while k < count {
            write(
                opb,
                i64::from(at(&info.postlist, k + 2, "postlist")?),
                rangebits,
            )?;
            k += 1;
        }
    }
    Ok(())
}

/// `icount`（res0.c:151–158）：立っているビットの数。
fn icount(v: u32) -> i32 {
    v.count_ones() as i32
}

/// `res0_pack`（res0.c:161–188）。
fn res0_pack(info: &VorbisInfoResidue0, opb: &mut OggpackBuffer) -> Result<(), EngineError> {
    let mut acc = 0i32;
    write(opb, info.begin, 24)?;
    write(opb, info.end, 24)?;

    // residue vectors to group and code with a partitioned book
    write(opb, i64::from(info.grouping) - 1, 24)?;
    write(opb, i64::from(info.partitions) - 1, 6)?; // possible partition choices
    write(opb, i64::from(info.groupbook), 8)?; // group huffman book

    // secondstages is a bitmask; as encoding progresses pass by pass, a bitmask of one indicates this partition class
    // has bits to write this pass
    for j in 0..info.partitions {
        let s = at(&info.secondstages, j, "secondstages")?;
        if ov_ilog(s as u32) > 3 {
            // yes, this is a minor hack due to not thinking ahead
            write(opb, i64::from(s), 3)?;
            write(opb, 1, 1)?;
            write(opb, i64::from(s >> 3), 5)?;
        } else {
            write(opb, i64::from(s), 4)?; // trailing zero
        }
        acc += icount(s as u32);
    }
    for j in 0..acc {
        write(opb, i64::from(at(&info.booklist, j, "booklist")?), 8)?;
    }
    Ok(())
}

/// `mapping0_pack`（mapping0.c:51–92）。
fn mapping0_pack(
    vi: &VorbisInfo,
    info: &VorbisInfoMapping0,
    opb: &mut OggpackBuffer,
) -> Result<(), EngineError> {
    // another 'we meant to do it this way' hack... up to beta 4, we packed 4 binary zeros here to signify one
    // submapping in use. We now redefine that to mean four bitflags that indicate use of deeper features; bit0:
    // submappings, bit1:coupling, bit2,3:reserved. This is backward compatable with all actual uses of the beta code.
    if info.submaps > 1 {
        write(opb, 1, 1)?;
        write(opb, i64::from(info.submaps) - 1, 4)?;
    } else {
        write(opb, 0, 1)?;
    }

    if info.coupling_steps > 0 {
        write(opb, 1, 1)?;
        write(opb, i64::from(info.coupling_steps) - 1, 8)?;

        let bits = ov_ilog((vi.channels - 1) as u32);
        for i in 0..info.coupling_steps {
            write(
                opb,
                i64::from(at(&info.coupling_mag, i, "coupling_mag")?),
                bits,
            )?;
            write(
                opb,
                i64::from(at(&info.coupling_ang, i, "coupling_ang")?),
                bits,
            )?;
        }
    } else {
        write(opb, 0, 1)?;
    }

    write(opb, 0, 2)?; // 2,3:reserved

    // we don't write the channel submappings if we only have one...
    if info.submaps > 1 {
        for i in 0..vi.channels {
            write(opb, i64::from(at(&info.chmuxlist, i, "chmuxlist")?), 4)?;
        }
    }
    for i in 0..info.submaps {
        write(opb, 0, 8)?; // time submap unused
        write(opb, i64::from(at(&info.floorsubmap, i, "floorsubmap")?), 8)?;
        write(
            opb,
            i64::from(at(&info.residuesubmap, i, "residuesubmap")?),
            8,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use vorbis_tables::BOOKS;

    use super::*;
    use crate::codec::vorbis::oracle;
    use crate::codec::vorbis::setup::tests::{CHANNELS, RATES, setup_of};

    fn headers(vi: &VorbisInfo, vendor: &str, comments: &[(String, String)]) -> [OggPacket; 3] {
        vorbis_analysis_headerout(vi, vendor, comments).unwrap()
    }

    /// Vorbis I の読み方（LSb から）で読む小さな読み手（ヘッダの形を仕様どおりに読み戻して確かめる）。
    struct Reader<'a> {
        data: &'a [u8],
        pos: usize,
    }

    impl Reader<'_> {
        fn read(&mut self, n: u32) -> u64 {
            let mut v = 0u64;
            for i in 0..n {
                let byte = self.data[self.pos / 8];
                if (byte >> (self.pos % 8)) & 1 == 1 {
                    v |= 1 << i;
                }
                self.pos += 1;
            }
            v
        }

        fn int(&mut self, n: u32) -> i32 {
            self.read(n) as i32
        }

        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.read(8) as u8).collect()
        }
    }

    /// 識別ヘッダを仕様（Vorbis I §4.2.2）のとおりに読み、`vi` の値と照らす。
    fn check_ident(p: &[u8], vi: &VorbisInfo) {
        assert_eq!(p.len(), 30);
        let mut r = Reader { data: p, pos: 0 };
        assert_eq!(r.read(8), 1);
        assert_eq!(r.bytes(6), b"vorbis");
        assert_eq!(r.read(32), 0);
        assert_eq!(r.read(8), vi.channels as u64);
        assert_eq!(r.read(32), vi.rate as u64);
        assert_eq!(r.read(32) as u32 as i32, vi.bitrate_upper as i32);
        assert_eq!(r.read(32) as u32 as i32, vi.bitrate_nominal as i32);
        assert_eq!(r.read(32) as u32 as i32, vi.bitrate_lower as i32);
        assert_eq!(1i64 << r.read(4), vi.codec_setup.blocksizes[0]);
        assert_eq!(1i64 << r.read(4), vi.codec_setup.blocksizes[1]);
        assert_eq!(r.read(1), 1);
        assert_eq!(r.pos, 30 * 8 - 7);
    }

    /// setup ヘッダを仕様（Vorbis I §4.2.4）のとおりに読み、`vi` の codebook・floor・residue・mapping・mode と照らす。
    fn check_setup(p: &[u8], vi: &VorbisInfo) {
        let ci = &vi.codec_setup;
        let mut r = Reader { data: p, pos: 0 };
        assert_eq!(r.read(8), 5);
        assert_eq!(r.bytes(6), b"vorbis");
        // codebook：同期の印と、books.bin の詰めた形そのもの
        assert_eq!(r.int(8) + 1, ci.books);
        for id in &ci.book_param {
            let b = BOOKS[usize::from(id.0)];
            let start = r.pos;
            assert_eq!(r.read(24), 0x56_4342);
            r.pos = start;
            let (bytes, bits) = vorbis_tables::packed(*id).unwrap();
            for k in 0..bits as usize {
                assert_eq!(r.read(1) as u8, (bytes[k / 8] >> (k % 8)) & 1, "{}", b.name);
            }
        }
        // time
        let times = r.int(6) + 1;
        for _ in 0..times {
            assert_eq!(r.read(16), 0);
        }
        // floor 1
        assert_eq!(r.int(6) + 1, ci.floors);
        for (k, f) in ci.floor_param.iter().enumerate() {
            assert_eq!(r.int(16), ci.floor_type[k]);
            let parts = r.int(5);
            assert_eq!(parts, f.partitions);
            let classes: Vec<i32> = (0..parts).map(|_| r.int(4)).collect();
            assert_eq!(classes, f.partitionclass[..parts as usize]);
            let maxclass = classes.iter().copied().max().unwrap_or(-1);
            for c in 0..=maxclass as usize {
                assert_eq!(r.int(3) + 1, f.class_dim[c]);
                let subs = r.int(2);
                assert_eq!(subs, f.class_subs[c]);
                if subs != 0 {
                    assert_eq!(r.int(8), f.class_book[c]);
                }
                for s in 0..1usize << subs {
                    assert_eq!(r.int(8) - 1, f.class_subbook[c][s]);
                }
            }
            assert_eq!(r.int(2) + 1, f.mult);
            let rangebits = r.read(4) as u32;
            assert_eq!(rangebits as i32, ov_ilog((f.postlist[1] - 1) as u32));
            let mut post = 2;
            for &c in &classes {
                for _ in 0..f.class_dim[c as usize] {
                    assert_eq!(r.int(rangebits), f.postlist[post]);
                    post += 1;
                }
            }
        }
        // residue
        assert_eq!(r.int(6) + 1, ci.residues);
        for (k, res) in ci.residue_param.iter().enumerate() {
            assert_eq!(r.int(16), ci.residue_type[k]);
            assert_eq!(r.read(24) as i64, res.begin);
            assert_eq!(r.read(24) as i64, res.end);
            assert_eq!(r.int(24) + 1, res.grouping);
            let parts = r.int(6) + 1;
            assert_eq!(parts, res.partitions);
            assert_eq!(r.int(8), res.groupbook);
            let mut cascade = Vec::new();
            for j in 0..parts as usize {
                let mut c = r.int(3);
                if r.read(1) == 1 {
                    c += r.int(5) * 8;
                }
                assert_eq!(c, res.secondstages[j]);
                cascade.push(c);
            }
            let mut n = 0;
            for c in cascade {
                for bit in 0..8 {
                    if c & (1 << bit) != 0 {
                        assert_eq!(r.int(8), res.booklist[n]);
                        n += 1;
                    }
                }
            }
        }
        // mapping 0
        assert_eq!(r.int(6) + 1, ci.maps);
        let chbits = ov_ilog((vi.channels - 1) as u32) as u32;
        for (k, m) in ci.map_param.iter().enumerate() {
            assert_eq!(r.int(16), ci.map_type[k]);
            let submaps = if r.read(1) == 1 { r.int(4) + 1 } else { 1 };
            assert_eq!(submaps, m.submaps);
            let steps = if r.read(1) == 1 { r.int(8) + 1 } else { 0 };
            assert_eq!(steps, m.coupling_steps);
            for s in 0..steps as usize {
                assert_eq!(r.int(chbits), m.coupling_mag[s]);
                assert_eq!(r.int(chbits), m.coupling_ang[s]);
            }
            assert_eq!(r.read(2), 0);
            if submaps > 1 {
                for c in 0..vi.channels as usize {
                    assert_eq!(r.int(4), m.chmuxlist[c]);
                }
            }
            for s in 0..submaps as usize {
                assert_eq!(r.read(8), 0);
                assert_eq!(r.int(8), m.floorsubmap[s]);
                assert_eq!(r.int(8), m.residuesubmap[s]);
            }
        }
        // mode
        assert_eq!(r.int(6) + 1, ci.modes);
        for m in &ci.mode_param {
            assert_eq!(r.int(1), m.blockflag);
            assert_eq!(r.int(16), m.windowtype);
            assert_eq!(r.int(16), m.transformtype);
            assert_eq!(r.int(8), m.mapping);
        }
        assert_eq!(r.read(1), 1);
        // 最後のバイトの余りは 0
        assert_eq!(p.len(), r.pos.div_ceil(8));
        while r.pos < p.len() * 8 {
            assert_eq!(r.read(1), 0);
        }
    }

    /// kura の範囲のすべての設定で 3 つのヘッダが書け、仕様どおりに読み戻せる（照合用の C がなくても回る）。
    #[test]
    fn headers_follow_the_spec() {
        for rate in RATES {
            for ch in 1..=8usize {
                for q in -1..=10 {
                    let vi = setup_of(rate, ch, q);
                    let [ident, comment, setup] = headers(&vi, "", &[]);
                    assert_eq!(
                        (ident.packetno, comment.packetno, setup.packetno),
                        (0, 1, 2)
                    );
                    for p in [&ident, &comment, &setup] {
                        assert_eq!((p.granulepos, p.e_o_s), (0, false));
                    }
                    check_ident(&ident.packet, &vi);
                    check_setup(&setup.packet, &vi);
                    // vendor が空・項目なしのコメント
                    assert_eq!(
                        comment.packet,
                        [&[3u8][..], b"vorbis", &[0; 4], &[0; 4], &[1]].concat()
                    );
                }
            }
        }
    }

    /// 既知の値：44.1 kHz ステレオ q5 の識別ヘッダ（160 kbps、256/2048）と、kura の vendor・項目のコメント。
    #[test]
    fn known_header_bytes() {
        let vi = setup_of(44_100, 2, 5);
        let comments = vec![
            ("LOOPSTART".to_string(), "0".to_string()),
            ("ENCODER".to_string(), "ogg-vorbis-kura-enc/1".to_string()),
            ("TITLE".to_string(), "曲".to_string()),
        ];
        let [ident, comment, setup] = headers(&vi, super::super::VENDOR, &comments);
        assert_eq!(
            ident.packet,
            [
                &[1u8][..],
                b"vorbis",
                &[0, 0, 0, 0],
                &[2],
                &44_100u32.to_le_bytes(),
                &[0; 4],
                &160_000u32.to_le_bytes(),
                &[0; 4],
                &[8 | (11 << 4)],
                &[1],
            ]
            .concat()
        );
        let mut want = vec![3u8];
        want.extend_from_slice(b"vorbis");
        want.extend_from_slice(&4u32.to_le_bytes());
        want.extend_from_slice(b"kura");
        want.extend_from_slice(&3u32.to_le_bytes());
        for s in ["LOOPSTART=0", "ENCODER=ogg-vorbis-kura-enc/1", "TITLE=曲"] {
            want.extend_from_slice(&(s.len() as u32).to_le_bytes());
            want.extend_from_slice(s.as_bytes());
        }
        want.push(1);
        assert_eq!(comment.packet, want);
        // setup ヘッダの頭（種類 5・`vorbis`・book の数 − 1）
        assert_eq!(&setup.packet[..7], &[5, b'v', b'o', b'r', b'b', b'i', b's']);
        assert_eq!(setup.packet[7], (vi.codec_setup.books - 1) as u8);
        // 同じ引数なら同じバイト列
        assert_eq!(
            headers(&vi, super::super::VENDOR, &comments)[2].packet,
            setup.packet
        );
        // 負の名目のビットレート（X のテンプレート）は 2 の補数の 32 ビット
        let x = setup_of(96_000, 6, -1);
        let [ident, ..] = headers(&x, "", &[]);
        assert_eq!(&ident.packet[20..24], &(-6i32).to_le_bytes());
        // 書けない設定は誤り
        let mut bad = vi.clone();
        bad.codec_setup.blocksizes = [32, 32];
        assert!(vorbis_analysis_headerout(&bad, "", &[]).is_err());
        let mut bad = vi.clone();
        bad.channels = 0;
        assert!(vorbis_analysis_headerout(&bad, "", &[]).is_err());
        let mut bad = vi.clone();
        bad.codec_setup.books += 1;
        assert!(vorbis_analysis_headerout(&bad, "", &[]).is_err());
        assert_eq!(vorbis_info_blocksize(&vi, 0), 256);
        assert_eq!(vorbis_info_blocksize(&vi, 1), 2048);
        assert_eq!(vorbis_info_blocksize(&vi, 2), -1);
        assert_eq!(icount(0b1011_0001), 4);
    }

    /// 照合 (b)：9 レート × 1・2・3・6・8 ch × q −1〜10 のすべてで、識別・コメント（vendor が空・タグなし）・setup の
    /// ヘッダが照合用のエンコーダの 0・1・2 番のパケットとバイト単位で一致する。
    #[test]
    fn oracle_headers_match_byte_for_byte() {
        if oracle::root().is_none() {
            return;
        }
        let kinds = [
            oracle::Signal::Music,
            oracle::Signal::Noise,
            oracle::Signal::Sine,
            oracle::Signal::Silence,
            oracle::Signal::Impulses,
        ];
        let mut cases = 0;
        let mut bytes = 0;
        for rate in RATES {
            for (ci, ch) in CHANNELS.into_iter().enumerate() {
                for q in -1..=10 {
                    let kind = kinds[(ci + (q + 1) as usize) % kinds.len()];
                    let input = oracle::signal(kind, rate, ch, 300, 7);
                    let (packets, _) = oracle::encode(&input, rate, q, None).unwrap();
                    let vi = setup_of(rate, ch, q);
                    let got = headers(&vi, "", &[]);
                    for k in 0..3 {
                        let want = &packets[k];
                        assert_eq!(
                            got[k].packet,
                            want.body,
                            "{rate} Hz・{ch} ch・q{q}：ヘッダ {k} が違う（{} と {} バイト）",
                            got[k].packet.len(),
                            want.body.len()
                        );
                        assert_eq!(want.granule, got[k].granulepos);
                        assert_eq!(want.flags & 1, u8::from(k == 0));
                        bytes += want.body.len();
                    }
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 9 * 5 * 12);
        assert!(bytes > 1_000_000, "{bytes}");
    }

    /// 照合用のエンコーダを `--vendor` 付きで回し、最初の 3 つのパケット（ヘッダ）を読む。
    fn oracle_headers_with_vendor(
        root: &Path,
        rate: u32,
        ch: usize,
        q: i32,
        vendor: &str,
    ) -> Vec<Vec<u8>> {
        let dir = std::env::temp_dir();
        let tag = format!("kura-vorbis-info-{}-{rate}-{ch}-{q}", std::process::id());
        let input = dir.join(format!("{tag}.f32"));
        let output = dir.join(format!("{tag}.pkt"));
        std::fs::write(&input, vec![0u8; 4 * ch * 100]).unwrap();
        let out = Command::new(root.join("bin/oracle_enc"))
            .arg(&input)
            .arg(&output)
            .arg(rate.to_string())
            .arg(ch.to_string())
            .arg(q.to_string())
            .arg("--vendor")
            .arg(vendor)
            .env_remove("KURA_ABL")
            .env_remove("KURA_DUMP")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = std::fs::read(&output).unwrap();
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
        let mut packets = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() && packets.len() < 3 {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            packets.push(bytes[at + 13..at + 13 + len].to_vec());
            at += 13 + len;
        }
        packets
    }

    /// コメントのヘッダは `oracle_enc --vendor` と同じ形（kura の vendor でも、ほかの vendor でも）。
    #[test]
    fn oracle_comment_header_with_vendor() {
        let Some(root) = oracle::root() else {
            return;
        };
        for (rate, ch, q, vendor) in [
            (44_100, 2, 5, super::super::VENDOR),
            (8_000, 1, -1, "kura"),
            (192_000, 8, 10, "a vendor with spaces and UTF-8 ✓"),
        ] {
            let want = oracle_headers_with_vendor(&root, rate, ch, q, vendor);
            let vi = setup_of(rate, ch, q);
            let got = headers(&vi, vendor, &[]);
            for k in 0..3 {
                assert_eq!(
                    got[k].packet, want[k],
                    "{rate} Hz・{ch} ch・q{q}・{vendor}：{k}"
                );
            }
        }
    }
}
