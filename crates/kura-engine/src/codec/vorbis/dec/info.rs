//! 3 つのヘッダの読み取り（デコーダ）。
//!
//! | 関数 | C |
//! |---|---|
//! | [`unpack_info`] | info.c:237–270 `_vorbis_unpack_info` |
//! | [`unpack_comment`] | info.c:273–319 `_vorbis_unpack_comment`（SSE でない経路。`_v_readstring` は 54–58） |
//! | [`unpack_books`] | info.c:323–397 `_vorbis_unpack_books`（codebook の表の量を予算で確かめる。`codebook` の「表の量」） |
//! | [`floor0_unpack`] | floor0.c:69–97 |
//! | [`floor1_unpack`] | floor1.c:119–180（`icomp` の `qsort` は重なりを見るだけなので並べ替えの種類によらない） |
//! | [`res0_unpack`] | res0.c:191–253（`icount` は 151–158） |
//! | [`mapping0_unpack`] | mapping0.c:95–149 |
//! | [`Headers::headerin`] | info.c:430–498 `vorbis_synthesis_headerin` |
//!
//! 定数は codec_internal.h・backends.h のとおり：`VI_TIMEB` 1、`VI_FLOORB` 2、`VI_RESB` 3、`VI_MAPB` 1、`VI_WINDOWB` 1。
//! 誤り（C の `OV_EBADHEADER`・`OV_ENOTVORBIS`）は `Decode`（壊れたファイル。ADR-217 (2)）。

use vorbis_tables::{
    StaticCodebook, VIF_CLASS, VIF_PARTS, VIF_POSIT, VorbisInfoFloor1, VorbisInfoMapping0,
    VorbisInfoMode, VorbisInfoResidue0,
};

use super::bad_header;
use super::bitwise::OggpackReader;
use super::codebook::{TableBytes, book_bytes, ov_ilog, staticbook_unpack, too_big};
use crate::error::EngineError;

const VI_TIMEB: i64 = 1;
const VI_FLOORB: i64 = 2;
const VI_RESB: i64 = 3;
const VI_MAPB: i64 = 1;
const VI_WINDOWB: i32 = 1;

/// `vorbis_info_floor0`（backends.h）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VorbisInfoFloor0 {
    pub order: i32,
    pub rate: i64,
    pub barkmap: i64,
    pub ampbits: i32,
    pub ampdb: i32,
    /// <= 16
    pub numbooks: i32,
    pub books: [i32; 16],
}

/// floor の設定（`floor_type` と `floor_param`）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Floor {
    Zero(VorbisInfoFloor0),
    One(Box<VorbisInfoFloor1>),
}

/// 識別ヘッダ（`vorbis_info` の復号に要る欄）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdHeader {
    pub channels: i32,
    pub rate: i64,
    pub blocksizes: [i64; 2],
}

/// setup ヘッダ（`codec_setup_info` の復号に要る欄。codebook は静的な形のまま。復号の表は合成の初期化で作る）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Setup {
    pub books: Vec<StaticCodebook>,
    pub floors: Vec<Floor>,
    pub residue_types: Vec<i32>,
    pub residues: Vec<VorbisInfoResidue0>,
    pub maps: Vec<VorbisInfoMapping0>,
    pub modes: Vec<VorbisInfoMode>,
    /// codebook の表の量（`codebook::book_bytes` の和。全体が予算の内）。
    pub table_bytes: TableBytes,
}

/// `_v_readstring`（info.c:54–58）：`bytes` バイトを 8 ビットずつ読む（終わりを越えた分は C と同じく −1 の下位 8 ビット）。
fn readstring(opb: &mut OggpackReader<'_>, bytes: i32) -> Vec<u8> {
    (0..bytes.max(0)).map(|_| opb.read(8) as u8).collect()
}

/// `_vorbis_unpack_info`（info.c:237–270）。
pub(crate) fn unpack_info(opb: &mut OggpackReader<'_>) -> Result<IdHeader, EngineError> {
    let version = opb.read(32) as i32;
    if version != 0 {
        return Err(bad_header("識別ヘッダの版が 0 ではありません"));
    }
    let channels = opb.read(8) as i32;
    let rate = opb.read(32);
    let _bitrate_upper = opb.read(32) as i32;
    let _bitrate_nominal = opb.read(32) as i32;
    let _bitrate_lower = opb.read(32) as i32;
    let bs0 = opb.read(4) as i32;
    if bs0 < 0 {
        return Err(bad_header("識別ヘッダが切れています"));
    }
    let bs1 = opb.read(4) as i32;
    if bs1 < 0 {
        return Err(bad_header("識別ヘッダが切れています"));
    }
    let blocksizes = [1i64 << bs0, 1i64 << bs1];
    if rate < 1
        || channels < 1
        || blocksizes[0] < 64
        || blocksizes[1] < blocksizes[0]
        || blocksizes[1] > 8192
    {
        return Err(bad_header("識別ヘッダの値が範囲の外です"));
    }
    // EOP check
    if opb.read(1) != 1 {
        return Err(bad_header("識別ヘッダの終わりの印がありません"));
    }
    Ok(IdHeader {
        channels,
        rate,
        blocksizes,
    })
}

/// `_vorbis_unpack_comment`（info.c:273–319）。コメント（バイト列）の並びを返す（vendor は捨てる）。
pub(crate) fn unpack_comment(opb: &mut OggpackReader<'_>) -> Result<Vec<Vec<u8>>, EngineError> {
    let broken = || bad_header("コメントヘッダが正しくありません");
    let vendorlen = opb.read(32) as i32;
    if vendorlen < 0 || i64::from(vendorlen) > opb.storage - 8 {
        return Err(broken());
    }
    let _vendor = readstring(opb, vendorlen);
    let i = opb.read(32) as i32;
    if i < 0 || i64::from(i) > ((opb.storage - opb.bytes()) >> 2) {
        return Err(broken());
    }
    let mut comments = Vec::new();
    comments
        .try_reserve_exact(i as usize)
        .map_err(|_| EngineError::Limit("コメントが多すぎます".into()))?;
    for _ in 0..i {
        let len = opb.read(32) as i32;
        if len < 0 || i64::from(len) > opb.storage - opb.bytes() {
            return Err(broken());
        }
        comments.push(readstring(opb, len));
    }
    // EOP check
    if opb.read(1) != 1 {
        return Err(broken());
    }
    Ok(comments)
}

/// `icount`（res0.c:151–158）。
fn icount(mut v: u32) -> i32 {
    let mut ret = 0;
    while v != 0 {
        ret += (v & 1) as i32;
        v >>= 1;
    }
    ret
}

/// `floor0_unpack`（floor0.c:69–97）。
pub(crate) fn floor0_unpack(
    books: &[StaticCodebook],
    opb: &mut OggpackReader<'_>,
) -> Option<VorbisInfoFloor0> {
    let mut info = VorbisInfoFloor0 {
        order: opb.read(8) as i32,
        rate: opb.read(16),
        barkmap: opb.read(16),
        ampbits: opb.read(6) as i32,
        ampdb: opb.read(8) as i32,
        numbooks: (opb.read(4) + 1) as i32,
        books: [0; 16],
    };
    if info.order < 1 || info.rate < 1 || info.barkmap < 1 || info.numbooks < 1 {
        return None;
    }
    for j in 0..info.numbooks as usize {
        let b = opb.read(8) as i32;
        info.books[j] = b;
        let book = usize::try_from(b).ok().and_then(|b| books.get(b))?;
        if book.maptype == 0 || book.dim < 1 {
            return None;
        }
    }
    Some(info)
}

/// `floor1_unpack`（floor1.c:119–180）。
pub(crate) fn floor1_unpack(books: i32, opb: &mut OggpackReader<'_>) -> Option<VorbisInfoFloor1> {
    let mut info = VorbisInfoFloor1 {
        partitions: 0,
        partitionclass: [0; VIF_PARTS],
        class_dim: [0; VIF_CLASS],
        class_subs: [0; VIF_CLASS],
        class_book: [0; VIF_CLASS],
        class_subbook: [[0; 8]; VIF_CLASS],
        mult: 0,
        postlist: [0; VIF_POSIT + 2],
        maxover: 0.0,
        maxunder: 0.0,
        maxerr: 0.0,
        twofitweight: 0.0,
        twofitatten: 0.0,
        n: 0,
    };
    let mut maxclass = -1i32;
    // read partitions（only 0 to 31 legal）
    info.partitions = opb.read(5) as i32;
    for j in 0..info.partitions.max(0) as usize {
        let c = opb.read(4) as i32;
        info.partitionclass[j] = c;
        if c < 0 {
            return None;
        }
        if maxclass < c {
            maxclass = c;
        }
    }
    // read partition classes
    for j in 0..(maxclass + 1) as usize {
        info.class_dim[j] = (opb.read(3) + 1) as i32;
        info.class_subs[j] = opb.read(2) as i32;
        if info.class_subs[j] < 0 {
            return None;
        }
        if info.class_subs[j] != 0 {
            info.class_book[j] = opb.read(8) as i32;
        }
        if info.class_book[j] < 0 || info.class_book[j] >= books {
            return None;
        }
        for k in 0..(1usize << info.class_subs[j]) {
            info.class_subbook[j][k] = (opb.read(8) - 1) as i32;
            if info.class_subbook[j][k] < -1 || info.class_subbook[j][k] >= books {
                return None;
            }
        }
    }
    // read the post list（only 1,2,3,4 legal now）
    info.mult = (opb.read(2) + 1) as i32;
    let rangebits = opb.read(4) as i32;
    if rangebits < 0 {
        return None;
    }
    let mut count = 0usize;
    let mut k = 0usize;
    for j in 0..info.partitions.max(0) as usize {
        count += info.class_dim[info.partitionclass[j] as usize] as usize;
        if count > VIF_POSIT {
            return None;
        }
        while k < count {
            let t = opb.read(rangebits) as i32;
            info.postlist[k + 2] = t;
            if t < 0 || t >= (1 << rangebits) {
                return None;
            }
            k += 1;
        }
    }
    info.postlist[0] = 0;
    info.postlist[1] = 1 << rangebits;
    // don't allow repeated values in post list as they'd result in zero-length segments
    let mut sorted: Vec<i32> = info.postlist[..count + 2].to_vec();
    sorted.sort();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return None;
    }
    Some(info)
}

/// `res0_unpack`（res0.c:191–253）。
pub(crate) fn res0_unpack(
    books: &[StaticCodebook],
    opb: &mut OggpackReader<'_>,
) -> Option<VorbisInfoResidue0> {
    let mut info = VorbisInfoResidue0 {
        begin: opb.read(24),
        end: opb.read(24),
        grouping: (opb.read(24) + 1) as i32,
        partitions: (opb.read(6) + 1) as i32,
        partvals: 0,
        groupbook: opb.read(8) as i32,
        secondstages: [0; 64],
        booklist: [0; 512],
        classmetric1: [0; 64],
        classmetric2: [0; 64],
    };
    // check for premature EOP
    if info.groupbook < 0 {
        return None;
    }
    let mut acc = 0usize;
    for j in 0..info.partitions.max(0) as usize {
        let mut cascade = opb.read(3) as i32;
        let cflag = opb.read(1) as i32;
        if cflag < 0 {
            return None;
        }
        if cflag != 0 {
            let c = opb.read(5) as i32;
            if c < 0 {
                return None;
            }
            cascade |= c << 3;
        }
        info.secondstages[j] = cascade;
        acc += icount(cascade as u32) as usize;
    }
    for j in 0..acc {
        let book = opb.read(8) as i32;
        if book < 0 {
            return None;
        }
        info.booklist[j] = book;
    }
    if info.groupbook as usize >= books.len() {
        return None;
    }
    for j in 0..acc {
        let b = books.get(info.booklist[j] as usize)?;
        if b.maptype == 0 {
            return None;
        }
    }
    // verify the phrasebook is not specifying an impossible or inconsistent partitioning scheme.
    let group = &books[info.groupbook as usize];
    let entries = group.entries as i32;
    let mut dim = group.dim as i32;
    let mut partvals: i32 = 1;
    if dim < 1 {
        return None;
    }
    while dim > 0 {
        partvals = partvals.wrapping_mul(info.partitions);
        if partvals > entries {
            return None;
        }
        dim -= 1;
    }
    info.partvals = partvals;
    Some(info)
}

/// `mapping0_unpack`（mapping0.c:95–149）。
pub(crate) fn mapping0_unpack(
    channels: i32,
    floors: usize,
    residues: usize,
    opb: &mut OggpackReader<'_>,
) -> Option<VorbisInfoMapping0> {
    let mut info = VorbisInfoMapping0 {
        submaps: 0,
        chmuxlist: [0; 256],
        floorsubmap: [0; 16],
        residuesubmap: [0; 16],
        coupling_steps: 0,
        coupling_mag: [0; 256],
        coupling_ang: [0; 256],
    };
    if channels <= 0 {
        return None;
    }
    let b = opb.read(1);
    if b < 0 {
        return None;
    }
    if b != 0 {
        info.submaps = (opb.read(4) + 1) as i32;
        if info.submaps <= 0 {
            return None;
        }
    } else {
        info.submaps = 1;
    }
    let b = opb.read(1);
    if b < 0 {
        return None;
    }
    if b != 0 {
        info.coupling_steps = (opb.read(8) + 1) as i32;
        if info.coupling_steps <= 0 {
            return None;
        }
        let bits = ov_ilog((channels - 1) as u32);
        for i in 0..info.coupling_steps as usize {
            let test_m = opb.read(bits) as i32;
            let test_a = opb.read(bits) as i32;
            info.coupling_mag[i] = test_m;
            info.coupling_ang[i] = test_a;
            if test_m < 0
                || test_a < 0
                || test_m == test_a
                || test_m >= channels
                || test_a >= channels
            {
                return None;
            }
        }
    }
    // 2,3:reserved
    if opb.read(2) != 0 {
        return None;
    }
    if info.submaps > 1 {
        for i in 0..channels as usize {
            let c = opb.read(4) as i32;
            info.chmuxlist[i] = c;
            if c >= info.submaps || c < 0 {
                return None;
            }
        }
    }
    for i in 0..info.submaps as usize {
        // time submap unused
        opb.read(8);
        let f = opb.read(8) as i32;
        info.floorsubmap[i] = f;
        if f < 0 || f as usize >= floors {
            return None;
        }
        let r = opb.read(8) as i32;
        info.residuesubmap[i] = r;
        if r < 0 || r as usize >= residues {
            return None;
        }
    }
    Some(info)
}

/// `_vorbis_unpack_books`（info.c:323–397）。codebook ごとに表の量（`book_bytes`）を足し、`table_budget` を超えれば
/// `Unsupported`（確保の前に確かめる）。
pub(crate) fn unpack_books(
    channels: i32,
    opb: &mut OggpackReader<'_>,
    table_budget: u64,
) -> Result<Setup, EngineError> {
    let broken = || bad_header("setup ヘッダが正しくありません");
    // codebooks
    let nbooks = (opb.read(8) + 1) as i32;
    if nbooks <= 0 {
        return Err(broken());
    }
    let mut books = Vec::with_capacity(nbooks as usize);
    let mut table_bytes = TableBytes::default();
    for _ in 0..nbooks {
        let b = staticbook_unpack(opb, table_budget.saturating_sub(table_bytes.total()))?
            .ok_or_else(broken)?;
        let used = b.lengthlist.iter().filter(|l| **l > 0).count() as i64;
        table_bytes = table_bytes.add(book_bytes(
            b.entries,
            used,
            b.dim,
            b.maptype,
            b.quantlist.len() as i64,
        ));
        if table_bytes.total() > table_budget {
            return Err(too_big());
        }
        books.push(b);
    }
    // time backend settings; hooks are unused
    let times = (opb.read(6) + 1) as i32;
    if times <= 0 {
        return Err(broken());
    }
    for _ in 0..times {
        let test = opb.read(16);
        if !(0..VI_TIMEB).contains(&test) {
            return Err(broken());
        }
    }
    // floor backend settings
    let nfloors = (opb.read(6) + 1) as i32;
    if nfloors <= 0 {
        return Err(broken());
    }
    let mut floors = Vec::with_capacity(nfloors as usize);
    for _ in 0..nfloors {
        let t = opb.read(16);
        if !(0..VI_FLOORB).contains(&t) {
            return Err(broken());
        }
        floors.push(if t == 0 {
            Floor::Zero(floor0_unpack(&books, opb).ok_or_else(broken)?)
        } else {
            Floor::One(Box::new(floor1_unpack(nbooks, opb).ok_or_else(broken)?))
        });
    }
    // residue backend settings
    let nres = (opb.read(6) + 1) as i32;
    if nres <= 0 {
        return Err(broken());
    }
    let mut residue_types = Vec::with_capacity(nres as usize);
    let mut residues = Vec::with_capacity(nres as usize);
    for _ in 0..nres {
        let t = opb.read(16);
        if !(0..VI_RESB).contains(&t) {
            return Err(broken());
        }
        residue_types.push(t as i32);
        residues.push(res0_unpack(&books, opb).ok_or_else(broken)?);
    }
    // map backend settings
    let nmaps = (opb.read(6) + 1) as i32;
    if nmaps <= 0 {
        return Err(broken());
    }
    let mut maps = Vec::with_capacity(nmaps as usize);
    for _ in 0..nmaps {
        let t = opb.read(16);
        if !(0..VI_MAPB).contains(&t) {
            return Err(broken());
        }
        maps.push(mapping0_unpack(channels, floors.len(), residues.len(), opb).ok_or_else(broken)?);
    }
    // mode settings
    let nmodes = (opb.read(6) + 1) as i32;
    if nmodes <= 0 {
        return Err(broken());
    }
    let mut modes = Vec::with_capacity(nmodes as usize);
    for _ in 0..nmodes {
        let m = VorbisInfoMode {
            blockflag: opb.read(1) as i32,
            windowtype: opb.read(16) as i32,
            transformtype: opb.read(16) as i32,
            mapping: opb.read(8) as i32,
        };
        if m.windowtype >= VI_WINDOWB
            || m.transformtype >= VI_WINDOWB
            || m.mapping >= nmaps
            || m.mapping < 0
        {
            return Err(broken());
        }
        modes.push(m);
    }
    // top level EOP check
    if opb.read(1) != 1 {
        return Err(broken());
    }
    Ok(Setup {
        books,
        floors,
        residue_types,
        residues,
        maps,
        modes,
        table_bytes,
    })
}

/// 3 つのヘッダを順に受け取る（`vorbis_synthesis_headerin`。info.c:430–498）。
#[derive(Debug)]
pub(crate) struct Headers {
    pub id: Option<IdHeader>,
    pub comments: Option<Vec<Vec<u8>>>,
    pub setup: Option<Setup>,
    /// codebook の表の予算（[`unpack_books`]）。
    pub table_budget: u64,
}

impl Headers {
    /// codebook の表の予算 `table_budget` で受け取り始める。
    pub fn new(table_budget: u64) -> Self {
        Self {
            id: None,
            comments: None,
            setup: None,
            table_budget,
        }
    }

    /// ヘッダのパケットを 1 つ受け取る。
    pub fn headerin(&mut self, packet: &[u8], b_o_s: bool) -> Result<(), EngineError> {
        let mut opb = OggpackReader::new(packet);
        // Which of the three types of header is this? Also verify header-ness, vorbis
        let packtype = opb.read(8) as i32;
        if readstring(&mut opb, 6) != b"vorbis" {
            return Err(bad_header("Vorbis のヘッダではありません"));
        }
        match packtype {
            0x01 => {
                if !b_o_s || self.id.is_some() {
                    return Err(bad_header("識別ヘッダの位置が正しくありません"));
                }
                self.id = Some(unpack_info(&mut opb)?);
                Ok(())
            }
            0x03 => {
                if self.id.is_none() || self.comments.is_some() {
                    return Err(bad_header("コメントヘッダの位置が正しくありません"));
                }
                self.comments = Some(unpack_comment(&mut opb)?);
                Ok(())
            }
            0x05 => {
                let Some(id) = self.id else {
                    return Err(bad_header("setup ヘッダの位置が正しくありません"));
                };
                if self.comments.is_none() || self.setup.is_some() {
                    return Err(bad_header("setup ヘッダの位置が正しくありません"));
                }
                self.setup = Some(unpack_books(id.channels, &mut opb, self.table_budget)?);
                Ok(())
            }
            _ => Err(bad_header("ヘッダの種類が正しくありません")),
        }
    }
}
