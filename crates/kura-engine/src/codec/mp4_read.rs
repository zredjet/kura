//! MP4 の読み口（自作。M16 段 3・ADR-219）：m4a の中の ALAC のトラックの、パケットの位置・長さ・コーデックの引数・タグ。
//!
//! 識別子 `m4a-alac-symphonia-dec/1` を変えない（ADR-216 (2)）ので、symphonia 0.6.1 の `IsoMp4Reader` が読めた正しい
//! ファイルでは、同じトラック・同じパケット（バイトと長さ）・同じ cookie・同じ文字列のタグを出す。照合は dev 依存に残した
//! symphonia と、フィクスチャ・乱数で作った正しいファイル・壊したファイルで行う（`tests` と `tests/m4a_read.rs`）。
//!
//! # symphonia と同じにしていること
//!
//! - 箱の読み方：残りが 8 バイト未満なら黙って終わり、親を越える子は親の残りごと捨てる。大きさ 1〜7・64 ビットで 16
//!   未満は誤り。入れ子の大きさ 0 は誤り（最上段の大きさ 0 は「ファイルの終わりまで」。symphonia は mdat のときだけ開けて
//!   最後に誤りにしたが、正しい形なので読む）。
//! - 最上段の走査：ftyp と moov を読んだあとの最初の mdat・moof で止まり、それより後ろの最上段の meta は見ない。ftyp・moov
//!   がなければ `Unsupported`（次の段へ）。moov は止まる前の最後のもの。
//! - トラック：moov の trak の順に、コーデックが NULL でない音声の最初のもの（handler・enabled は見ない）。それが ALAC で
//!   なければ `Unsupported`（m4a の AAC は次のデコーダが読む）。ALAC になるのは、音声の見本（`mp4a`・`alac` など）の直下に
//!   読める `alac` の子があるとき（`wave` の中の `alac`（MOV の形）は見ない）。見本が 2 つ以上・`alac` の子の版が 0 で
//!   ない・cookie が読めない（チャンネルが決まらない）ものは `Unsupported`。
//! - レートとチャンネル：cookie の値（mdhd の尺・見本の値は使わない）。
//! - パケット：トラックの終わりは stts の数、長さ（`dur`）は stts の delta。編集リスト（elst）・iTunSMPB は当てない。
//!   位置は stsc・stco（co64。両方あれば stco）・stsz から。
//! - 断片（moof）：trak と trex は添字で対応させ、その trex の track_id の traf を読む。base は tfhd の base_data_offset、
//!   なければ moof の頭。trun の data_offset、なければ前の trun の終わり。長さ・大きさはサンプルごとの値、なければ tfhd、
//!   なければ trex の既定。tfdt は見ない。
//! - タグ：最後の版だけ（moov/udta の最後の meta の最後の ilst。なければ走査の止まる前の最上段の meta のうち ilst のある
//!   最後のもの）。kura が使う 7 つのキー（`©nam`・`©ART`・`©alb`・`©cmt`・`©day`・`©gen`・`cprt`）の項目の、型 1・4
//!   （UTF-8。不正な列は U+FFFD、NUL も残す）・2・5（UTF-16。BOM で向きを決め、なければ BE。奇数長の最後のバイトは捨てる）の
//!   `data` を出た順に。ほかの項目・型は読まない（`symph::meta_from_tags` は MP4 ではこの 7 つのキーしか使わない）。
//!   [`TEXT_MAX`] より長くなる値は読まずに捨てる（`meta_from_tags` も捨てる）。
//!
//! # symphonia と違うところ（壊れたファイルと、symphonia が読み違える形。ADR-219 (3)。docs/editing.md §9.6）
//!
//! - 壊れた表は `Decode`：stsz の数（可変）・定数の stsz の sample_count が stts の数と違う（symphonia は stts の数で黙って
//!   終わる）、大きさ 0 のサンプル（trun の既定の大きさ 0 を含む。ALAC は空のパケットを読めない）、サンプルがファイルの外
//!   （symphonia も誤り）、サンプルのある moof のあとに mdat がないまま終わる・途中で切れた最後の mdat（symphonia はその断片を
//!   黙って捨てる）。断片を読み進めるときの終わりの切れた頭は、symphonia と同じく終わりとみなす。
//! - symphonia が正しいファイルを読み違える形は `Unsupported`（次の段の AudioToolbox・ffmpeg が正しく読む）：chunk・trun の
//!   位置が前のサンプルの終わりより前（symphonia は前の終わりから続けて読む。chunk の並びが前後しているファイル）、1 つの
//!   moof に同じトラックの traf が 2 つ以上（symphonia は最後のものだけを読む）、trex の track_id が trak と違う（symphonia
//!   はほかのトラックのサンプルを混ぜる）。
//! - ほかのトラック（チャプター・カバー）の表の誤り、使わないタグの項目の誤りでは止めない（symphonia は開けなかった）。
//!   stz2 も読む（stsz がないときだけ。symphonia は stsz がないとして開けなかった）。
//!
//! 箱の中身は必要なものだけを読み込む（covr・mdat・使わないトラックは読まない）。1 つの箱を読み込む大きさは
//! [`MAX_READ`] まで（それを超えるものは `Limit`）。

use crate::codec::ReadSeek;
use crate::codec::symph::Media;
use crate::error::EngineError;
use crate::monitor::Monitor;

/// 1 つの箱（表・タグの項目）・1 つのサンプルとして読み込む大きさの上限（バイト）。
pub(crate) const MAX_READ: u64 = 256 << 20;
/// 文字列のタグとして運ぶ値の長さの上限（`symph::meta_from_tags` と同じ。これより長いものは捨てる）。
const TEXT_MAX: usize = 4_096;
/// 箱の入れ子の深さの上限（symphonia と同じ 32。ALAC の m4a は 8 段）。
const MAX_DEPTH: u32 = 32;

/// kura が使う MP4 のタグのキー（箱の種類と、symphonia の生のキー）。
pub(crate) const TAG_KEYS: [([u8; 4], &str); 7] = [
    ([0xa9, b'n', b'a', b'm'], "\u{a9}nam"),
    ([0xa9, b'A', b'R', b'T'], "\u{a9}ART"),
    ([0xa9, b'a', b'l', b'b'], "\u{a9}alb"),
    ([0xa9, b'c', b'm', b't'], "\u{a9}cmt"),
    ([0xa9, b'd', b'a', b'y'], "\u{a9}day"),
    ([0xa9, b'g', b'e', b'n'], "\u{a9}gen"),
    (*b"cprt", "cprt"),
];

/// 音声として読む見本の種類（symphonia の `stsd.rs:66-86`）。
const AUDIO_ENTRIES: [&[u8; 4]; 18] = [
    b"mp4a", b"alac", b"ac-3", b"ec-3", b"fLaC", b"Opus", b".mp3", b"lpcm", b"wave", b"alaw",
    b"ulaw", b"raw ", b"sowt", b"twos", b"in24", b"in32", b"fl32", b"fl64",
];

/// PCM の見本の種類（コーデックが NULL でない）。
const PCM_ENTRIES: [&[u8; 4]; 7] = [
    b"raw ", b"sowt", b"twos", b"in24", b"in32", b"fl32", b"fl64",
];

enum Backing<'a> {
    Bytes(&'a [u8]),
    Reader(&'a mut (dyn ReadSeek + Send)),
}

/// 読む元（位置を指して読む。読むたびに中止を見る）。
pub(crate) struct Source<'a> {
    backing: Backing<'a>,
    /// ファイルの長さ。
    pub len: u64,
    buf: Vec<u8>,
    mon: &'a dyn Monitor,
    codec: &'static str,
}

impl<'a> Source<'a> {
    fn new(
        media: &'a mut Media<'_>,
        mon: &'a dyn Monitor,
        codec: &'static str,
    ) -> Result<Self, EngineError> {
        let (backing, len) = match media {
            Media::Bytes(data) => (Backing::Bytes(data), data.len() as u64),
            Media::Reader(r) => {
                let len = r.seek(std::io::SeekFrom::End(0))?;
                r.seek(std::io::SeekFrom::Start(0))?;
                (Backing::Reader(&mut **r), len)
            }
        };
        Ok(Self {
            backing,
            len,
            buf: Vec::new(),
            mon,
            codec,
        })
    }

    fn decode(&self, message: impl Into<String>) -> EngineError {
        EngineError::Decode {
            codec: self.codec.to_string(),
            message: message.into(),
        }
    }

    fn unsupported(&self, message: &str) -> EngineError {
        EngineError::Unsupported(format!("{}: {message}", self.codec))
    }

    /// `at` から `n` バイト（ファイルの外にかかれば壊れたファイル、[`MAX_READ`] を超えれば `Limit`）。
    pub fn read(&mut self, at: u64, n: u64) -> Result<&[u8], EngineError> {
        if self.mon.canceled() {
            return Err(EngineError::Canceled);
        }
        if n > MAX_READ {
            return Err(EngineError::Limit(format!(
                "MP4 の箱かサンプルが大きすぎます（{n} バイト）"
            )));
        }
        let end = at.checked_add(n).filter(|e| *e <= self.len);
        let Some(end) = end else {
            return Err(self.decode("MP4：ファイルの外を指しています（途中で切れています）"));
        };
        match &mut self.backing {
            Backing::Bytes(d) => Ok(&d[at as usize..end as usize]),
            Backing::Reader(r) => {
                let n = n as usize;
                self.buf.clear();
                self.buf
                    .try_reserve_exact(n)
                    .map_err(|_| EngineError::Limit("MP4 の箱を確保できません".into()))?;
                self.buf.resize(n, 0);
                r.seek(std::io::SeekFrom::Start(at))?;
                r.read_exact(&mut self.buf)?;
                Ok(&self.buf)
            }
        }
    }
}

/// 箱の頭。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Head {
    kind: [u8; 4],
    /// 箱の頭の位置。
    at: u64,
    /// 中身の頭。
    body: u64,
    /// 箱の終わり。
    end: u64,
}

impl Head {
    fn len(&self) -> u64 {
        self.end - self.body
    }
}

/// 親の中の箱の並び（symphonia の `AtomIterator::next_header` の規則。モジュールの説明）。
struct Boxes {
    pos: u64,
    end: u64,
    top: bool,
    depth: u32,
    /// 終わりの切れた頭を誤りにせず終わりとする（断片を読み進めるとき。symphonia の `try_read_more_segments` が
    /// UnexpectedEof を見逃すのと同じ）。
    lenient_end: bool,
}

impl Boxes {
    fn top(len: u64) -> Self {
        Self {
            pos: 0,
            end: len,
            top: true,
            depth: 0,
            lenient_end: false,
        }
    }

    fn inside(h: &Head, depth: u32) -> Self {
        Self {
            pos: h.body,
            end: h.end,
            top: false,
            depth,
            lenient_end: false,
        }
    }

    fn inside_from(h: &Head, skip: u64, depth: u32) -> Self {
        Self {
            pos: h.body.saturating_add(skip).min(h.end),
            end: h.end,
            top: false,
            depth,
            lenient_end: false,
        }
    }

    fn next(&mut self, src: &mut Source<'_>) -> Result<Option<Head>, EngineError> {
        if self.depth >= MAX_DEPTH {
            return Err(src.decode("MP4：箱の入れ子が深すぎます"));
        }
        if self.end.saturating_sub(self.pos) < 8 {
            // 親の残り 0〜7 バイトは黙って捨てる
            self.pos = self.end;
            return Ok(None);
        }
        let at = self.pos;
        let h = src.read(at, 8)?;
        let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
        let kind = [h[4], h[5], h[6], h[7]];
        let (body, end) = match size {
            0 => {
                if !self.top {
                    return Err(src.decode("MP4：大きさの分からない箱が入れ子になっています"));
                }
                (at + 8, src.len)
            }
            1 => {
                if self.end - at < 16 {
                    if self.top && !self.lenient_end {
                        return Err(src.decode("MP4：箱の頭が途中で切れています"));
                    }
                    self.pos = self.end;
                    return Ok(None);
                }
                let b = src.read(at + 8, 8)?;
                let large = u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                if large < 16 {
                    return Err(src.decode("MP4：箱の大きさが正しくありません"));
                }
                let end = at
                    .checked_add(large)
                    .ok_or_else(|| src.decode("MP4：箱の大きさが正しくありません"))?;
                (at + 16, end)
            }
            2..=7 => return Err(src.decode("MP4：箱の大きさが正しくありません")),
            n => (at + 8, at + u64::from(n)),
        };
        if end > self.end {
            // 親（最上段ならファイル）を越える箱：その箱から後ろは見ない
            self.pos = self.end;
            return Ok(None);
        }
        self.pos = end;
        Ok(Some(Head {
            kind,
            at,
            body,
            end,
        }))
    }
}

/// 中身の列を頭から読む小さな読み手（足りなければ None）。
struct Bytes<'b> {
    b: &'b [u8],
    at: usize,
}

impl<'b> Bytes<'b> {
    fn new(b: &'b [u8]) -> Self {
        Self { b, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'b [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    fn left(&self) -> usize {
        self.b.len().saturating_sub(self.at)
    }
}

/// stsc の項目（0 始まりの chunk）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StscEntry {
    first_chunk: u32,
    spc: u32,
}

/// stsz（stz2 も）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sizes {
    Constant { size: u32, count: u32 },
    Variable(Vec<u32>),
}

/// 断片でないサンプルの表。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tables {
    /// (数, delta)。
    stts: Vec<(u32, u32)>,
    stsc: Vec<StscEntry>,
    sizes: Sizes,
    /// chunk の位置（stco か co64。stco が優先）。
    chunks: Option<Vec<u64>>,
}

/// 読むトラックの中身。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    Alac,
    /// MP4 の中の AAC（M16 段 4。esds の AudioSpecificConfig）。
    Aac,
}

/// 開いたトラック。
pub(crate) struct Mp4Track<'a> {
    pub src: Source<'a>,
    /// レート（ALAC は cookie、AAC は AudioSpecificConfig の値）。
    pub rate: u32,
    /// チャンネルの数（同上）。
    pub channels: usize,
    /// コーデックの設定（ALAC は `alac` の子の 24 か 48 バイト、AAC は esds の DecoderSpecificInfo。ファイルのバイトのまま）。
    pub cookie: Vec<u8>,
    /// 文字列のタグ（キー, 値）。出た順。
    pub tags: Vec<(String, String)>,
    tables: Tables,
    /// 断片の読み（mvex があるとき）。
    frag: Option<Fragments>,
    // 読む位置
    stts_i: usize,
    stts_left: u32,
    stsc_i: usize,
    chunk: u32,
    in_chunk: u32,
    sample: u32,
    next_pos: u64,
    moov_done: bool,
}

/// 1 つのサンプル（位置・大きさ・長さ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sample {
    pub at: u64,
    pub size: u32,
    pub dur: u32,
}

/// 断片の読み（symphonia の `MoofSegment` と `try_read_more_segments`）。
struct Fragments {
    /// 最上段のどこから読み進めるか。
    walk: Boxes,
    /// このトラックの trex（track_id・既定の長さ・既定の大きさ）。
    track_id: u32,
    default_dur: u32,
    default_size: u32,
    /// 読んだ moof の trun（mdat に着いたら出す）。
    pending: Vec<Run>,
    /// 出す trun。
    ready: std::collections::VecDeque<Run>,
    /// 出している trun（と、その中の次のサンプル）。
    current: Option<(Run, u32, u64)>,
    done: bool,
}

/// 1 つの trun（サンプルを並べて持たず、サンプルごとの値の生のバイトを持つ。宣言の数だけ確保させない）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    /// 最初のサンプルの位置。
    at: u64,
    count: u32,
    flags: u32,
    dur: u32,
    size: u32,
    /// サンプルごとの値（flags の 0x100・0x200・0x400・0x800 の順に 4 バイトずつ）。
    per: Vec<u8>,
}

impl Run {
    /// サンプルごとの値の幅（バイト）。
    fn stride(&self) -> usize {
        [0x100u32, 0x200, 0x400, 0x800]
            .iter()
            .filter(|b| self.flags & **b != 0)
            .count()
            * 4
    }

    /// `i` 番目のサンプルの（長さ, 大きさ）。
    fn sample(&self, i: u32) -> (u32, u32) {
        let at = i as usize * self.stride();
        let word = |k: usize| {
            self.per
                .get(at + 4 * k..at + 4 * k + 4)
                .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let mut k = 0;
        let dur = if self.flags & 0x100 != 0 {
            k += 1;
            word(0)
        } else {
            self.dur
        };
        let size = if self.flags & 0x200 != 0 {
            word(k)
        } else {
            self.size
        };
        (dur, size)
    }
}

/// トラックのコーデックの分け方（symphonia の codec が NULL か、ALAC か、ほかの音声か）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Codec {
    Null,
    /// 見本が 2 つ以上（symphonia は開けずに `Unsupported`）。
    Multi,
    Alac {
        cookie: Vec<u8>,
        version_ok: bool,
        /// 見本のレート（cookie が読めないときに symphonia が使う）。
        entry_rate: u32,
    },
    /// AAC（esds の objectTypeIndication が 0x40・0x66・0x67）。DecoderSpecificInfo（AudioSpecificConfig）。
    Aac {
        asc: Vec<u8>,
    },
    Other,
}

/// 開いて `want` の中身のトラックを選び、表とタグを読む。
pub(crate) fn open<'a>(
    media: &'a mut Media<'_>,
    mon: &'a dyn Monitor,
    codec: &'static str,
    want: Want,
) -> Result<Mp4Track<'a>, EngineError> {
    let mut src = Source::new(media, mon, codec)?;
    // 最上段の走査
    let mut top = Boxes::top(src.len);
    let (mut ftyp, mut moov) = (false, None::<Head>);
    // 最上段の meta のうち ilst のある最後のもの（の最後の ilst）
    let mut top_ilst = None::<Head>;
    let mut stop = None::<Head>;
    while let Some(h) = top.next(&mut src)? {
        match &h.kind {
            b"ftyp" => ftyp = true,
            b"moov" => moov = Some(h),
            b"meta" => {
                if let Some(i) = ilst_of(&mut src, &h)? {
                    top_ilst = Some(i);
                }
            }
            b"mdat" | b"moof" if ftyp && moov.is_some() => {
                stop = Some(h);
                break;
            }
            _ => {}
        }
    }
    if !ftyp {
        return Err(src.unsupported("MP4 の ftyp がありません"));
    }
    let Some(moov) = moov else {
        return Err(src.unsupported("MP4 の moov がありません"));
    };
    if stop.is_none() {
        // 2 度目の走査：最初の mdat か moof
        let mut again = Boxes::top(src.len);
        while let Some(h) = again.next(&mut src)? {
            if &h.kind == b"mdat" || &h.kind == b"moof" {
                stop = Some(h);
                break;
            }
        }
    }
    // moov の中：trak（数えながら、コーデックが NULL でない音声の最初のものを選ぶ）・mvex・udta
    let (mut mvex, mut udta, mut mvhd) = (None::<Head>, None::<Head>, false);
    let mut traks = 0usize;
    let mut chosen = None;
    let mut inner = Boxes::inside(&moov, 1);
    while let Some(h) = inner.next(&mut src)? {
        match &h.kind {
            b"trak" => {
                if chosen.is_none() {
                    match trak_codec(&mut src, &h)? {
                        Codec::Null => {}
                        // symphonia は見本が 2 つ以上の trak があると開けない
                        Codec::Multi => {
                            return Err(src.unsupported("MP4 の見本が 2 つ以上あります"));
                        }
                        c => chosen = Some((traks, h, c)),
                    }
                }
                traks += 1;
            }
            b"mvex" => mvex = Some(h),
            b"udta" => udta = Some(h),
            b"mvhd" => mvhd = true,
            _ => {}
        }
    }
    let Some((index, trak, codec_kind)) = chosen else {
        return Err(src.unsupported("音声のトラックがありません"));
    };
    let (cookie, rate, channels) = match (want, codec_kind) {
        (
            Want::Alac,
            Codec::Alac {
                cookie,
                version_ok,
                entry_rate,
            },
        ) => {
            if !version_ok {
                return Err(src.unsupported("alac の箱の版を扱えません"));
            }
            // cookie が読めなければ、symphonia はレートを見本の値（0 なら壊れたファイル）にし、チャンネルは決まらない
            // （`Unsupported`）
            let Some(cfg) = crate::codec::alac_read::Cookie::parse(&cookie).ok() else {
                if entry_rate == 0 {
                    return Err(src.decode("サンプルレートがありません"));
                }
                return Err(src.unsupported("チャンネルの数がありません"));
            };
            (cookie, cfg.sample_rate, usize::from(cfg.channels))
        }
        (Want::Aac, Codec::Aac { asc }) => {
            let cfg = crate::codec::aac::config::parse_asc(&asc)?;
            (asc, cfg.rate, cfg.channels)
        }
        _ => return Err(src.unsupported("このデコーダの扱う中身ではありません")),
    };
    if !mvhd {
        return Err(src.decode("MP4：moov に mvhd がありません"));
    }
    required_boxes(&mut src, &trak)?;
    let tables = trak_tables(&mut src, &trak)?;
    let frag = match mvex {
        Some(mvex) => Some(fragments(&mut src, &mvex, index, traks, &trak, stop)?),
        None => None,
    };
    let tags = read_tags(&mut src, udta.as_ref(), top_ilst)?;
    let mut r = Mp4Track {
        src,
        rate,
        channels,
        cookie,
        tags,
        tables,
        frag,
        stts_i: 0,
        stts_left: 0,
        stsc_i: 0,
        chunk: 0,
        in_chunk: 0,
        sample: 0,
        next_pos: 0,
        moov_done: false,
    };
    r.check_tables()?;
    r.start();
    Ok(r)
}

/// 選んだ trak に、symphonia が求める箱があるか（tkhd・mdia、mdia の mdhd・hdlr・minf、minf の stbl。stbl の表は
/// [`trak_tables`]）。なければ壊れたファイル（ほかの trak は見ない。モジュールの説明）。
fn required_boxes(src: &mut Source<'_>, trak: &Head) -> Result<(), EngineError> {
    for (path, what) in [
        (&[b"tkhd"][..], "tkhd"),
        (&[b"mdia"][..], "mdia"),
        (&[b"mdia", b"mdhd"][..], "mdhd"),
        (&[b"mdia", b"hdlr"][..], "hdlr"),
        (&[b"mdia", b"minf"][..], "minf"),
        (&[b"mdia", b"minf", b"stbl"][..], "stbl"),
    ] {
        if find(src, trak, path, 2)?.is_none() {
            return Err(src.decode(format!("MP4：トラックに {what} がありません")));
        }
    }
    Ok(())
}

/// trak の tkhd の id。
fn tkhd_id(src: &mut Source<'_>, trak: &Head) -> Result<Option<u32>, EngineError> {
    // symphonia は後の tkhd が上書き
    let Some(h) = find(src, trak, &[b"tkhd"], 2)? else {
        return Ok(None);
    };
    let body = src.read(h.body, h.len().min(32))?;
    let mut r = Bytes::new(body);
    let version = r.u8();
    r.take(3);
    Ok(match version {
        Some(0) => r.take(8).and_then(|_| r.u32()),
        Some(1) => r.take(16).and_then(|_| r.u32()),
        _ => None,
    })
}

/// trak の中の箱を探す（`path` の順にたどる）。
fn find(
    src: &mut Source<'_>,
    from: &Head,
    path: &[&[u8; 4]],
    depth: u32,
) -> Result<Option<Head>, EngineError> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(Some(*from));
    };
    let mut found = None;
    let mut b = Boxes::inside(from, depth);
    while let Some(h) = b.next(src)? {
        // symphonia は後のものが上書き
        if &h.kind == *first {
            found = Some(h);
        }
    }
    match found {
        Some(h) => find(src, &h, rest, depth + 1),
        None => Ok(None),
    }
}

/// trak のコーデックの分け方（symphonia の `stsd.rs` の `make_codec_params` と同じ判定。読めない形は NULL とみなす）。
fn trak_codec(src: &mut Source<'_>, trak: &Head) -> Result<Codec, EngineError> {
    let Some(stsd) = find(src, trak, &[b"mdia", b"minf", b"stbl", b"stsd"], 2)? else {
        return Ok(Codec::Null);
    };
    if stsd.len() < 8 {
        return Ok(Codec::Null);
    }
    let head = src.read(stsd.body, 8)?;
    let entries = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
    if entries == 0 {
        return Ok(Codec::Null);
    }
    if entries >= 2 {
        return Ok(Codec::Multi);
    }
    let mut b = Boxes::inside_from(&stsd, 8, 6);
    let Some(entry) = b.next(src)? else {
        return Ok(Codec::Null);
    };
    if !AUDIO_ENTRIES.iter().any(|k| **k == entry.kind) {
        return Ok(Codec::Null);
    }
    // 音声の見本：版ごとの頭の長さ
    if entry.len() < 28 {
        return Ok(Codec::Null);
    }
    let fixed = src.read(entry.body, 28)?;
    let version = u16::from_be_bytes([fixed[8], fixed[9]]);
    let skip = match version {
        0 => 28,
        1 => 44,
        2 => 64,
        _ => return Ok(Codec::Null),
    };
    // 見本のレート（16.16。v2 は f64。cookie が読めないときに symphonia が使う）
    let mut entry_rate = u32::from_be_bytes([fixed[24], fixed[25], fixed[26], fixed[27]]) >> 16;
    // v0・v1 の PCM は見本の種類で、v2 はどの種類でも lpcm の値で決まる（ALAC の子があれば上書き）
    let mut codec = if version == 2 {
        let v2 = src.read(
            entry.body.saturating_add(28),
            36.min(entry.len().saturating_sub(28)),
        )?;
        let mut r = Bytes::new(v2);
        let lpcm = (|| {
            r.u32()?;
            let rate = f64::from_bits(r.u64()?);
            r.u32()?;
            r.u32()?;
            let bits = r.u32()?;
            let flags = r.u32()?;
            Some((rate, lpcm_known(bits, flags)))
        })();
        if let Some((rate, _)) = lpcm {
            // f64 → u32（0 方向へ切り捨て・飽和、NaN は 0。Rust の `as` と同じ）
            entry_rate = rate as u32;
        }
        if lpcm.is_some_and(|(_, known)| known) {
            Codec::Other
        } else {
            Codec::Null
        }
    } else if PCM_ENTRIES.iter().any(|k| **k == entry.kind) {
        Codec::Other
    } else {
        Codec::Null
    };
    let mut children = Boxes::inside_from(&entry, skip, 7);
    while let Some(h) = children.next(src)? {
        match &h.kind {
            b"alac" => {
                let body = src.read(h.body, h.len().min(4 + 48))?;
                let version = body.first().copied();
                let flags_zero = body.get(1..4) == Some(&[0, 0, 0][..]);
                let rest = h.len().saturating_sub(4);
                if version == Some(0) && !flags_zero {
                    return Err(src.decode("alac の箱の旗が 0 ではありません"));
                }
                if version == Some(0) && rest != 24 && rest != 48 {
                    return Err(src.decode("alac の magic cookie の長さが正しくありません"));
                }
                let cookie = body.get(4..).unwrap_or(&[]).to_vec();
                codec = Codec::Alac {
                    cookie,
                    version_ok: version == Some(0),
                    entry_rate,
                };
            }
            b"esds" => {
                if let Some(c) = esds_codec(src, &h)? {
                    codec = c;
                }
            }
            b"dac3" | b"dec3" | b"dfLa" | b"dOps" => codec = Codec::Other,
            b"wave" => {
                let mut w = Boxes::inside(&h, 8);
                while let Some(e) = w.next(src)? {
                    if &e.kind == b"esds"
                        && let Some(c) = esds_codec(src, &e)?
                    {
                        codec = c;
                    }
                }
            }
            _ => {}
        }
    }
    if &entry.kind == b".mp3" {
        codec = Codec::Other;
    }
    Ok(codec)
}

/// v2 の見本の lpcm の値が NULL でないか（symphonia の `lpcm_codec_id`：浮動小数は 32・64、整数は 8・16・24・32）。
fn lpcm_known(bits: u32, flags: u32) -> bool {
    if flags & 0x1 != 0 {
        bits == 32 || bits == 64
    } else {
        matches!(bits, 8 | 16 | 24 | 32)
    }
}

/// esds の中身（symphonia と同じく、ES_Descriptor の頭のあとの記述子を順に見て、DecoderConfigDescriptor（tag 4）の
/// 最後のものの objectTypeIndication と、その中の DecoderSpecificInfo（tag 5）の最後のもの）。AAC（0x40・0x66・0x67）なら
/// [`Codec::Aac`]、ほかの音声（MP3・AC-3・E-AC-3・DTS）なら [`Codec::Other`]、ほか・読めない形は None（前の子のまま）。
fn esds_codec(src: &mut Source<'_>, h: &Head) -> Result<Option<Codec>, EngineError> {
    let body = src.read(h.body, h.len().min(64 << 10))?;
    let mut r = Bytes::new(body);
    if r.take(4).is_none() {
        return Ok(None);
    }
    let header = |r: &mut Bytes<'_>| -> Option<(u8, usize)> {
        let tag = r.u8()?;
        let mut len = 0usize;
        for _ in 0..4 {
            let b = r.u8()?;
            len = (len << 7) | usize::from(b & 0x7f);
            if b & 0x80 == 0 {
                break;
            }
        }
        Some((tag, len))
    };
    let found = (|| {
        let (tag, len) = header(&mut r)?;
        if tag != 3 {
            return None;
        }
        let es = r.take(len)?;
        let mut e = Bytes::new(es);
        e.u16()?;
        let flags = e.u8()?;
        if flags & 0x80 != 0 {
            e.u16()?;
        }
        if flags & 0x40 != 0 {
            let n = e.u8()?;
            e.take(usize::from(n))?;
        }
        if flags & 0x20 != 0 {
            e.u16()?;
        }
        let mut found = None;
        while e.left() > 2 {
            let (tag, len) = header(&mut e)?;
            let d = e.take(len)?;
            if tag == 4 {
                let mut c = Bytes::new(d);
                let oti = c.u8()?;
                c.take(12)?;
                let mut dsi = Vec::new();
                while c.left() > 2 {
                    let (t, l) = header(&mut c)?;
                    let v = c.take(l)?;
                    if t == 5 {
                        dsi = v.to_vec();
                    }
                }
                found = Some((oti, dsi));
            }
        }
        found
    })();
    Ok(match found {
        Some((0x40 | 0x66 | 0x67, asc)) => Some(Codec::Aac { asc }),
        Some((0x69 | 0x6b | 0xa5 | 0xa6 | 0xa9, _)) => Some(Codec::Other),
        _ => None,
    })
}

/// 選んだトラックのサンプルの表。
fn trak_tables(src: &mut Source<'_>, trak: &Head) -> Result<Tables, EngineError> {
    let Some(stbl) = find(src, trak, &[b"mdia", b"minf", b"stbl"], 2)? else {
        return Err(src.decode("MP4：stbl がありません"));
    };
    let (mut stts, mut stsc, mut stsz, mut stz2, mut stco, mut co64) =
        (None, None, None, None, None, None);
    let mut b = Boxes::inside(&stbl, 5);
    while let Some(h) = b.next(src)? {
        match &h.kind {
            b"stts" => stts = Some(h),
            b"stsc" => stsc = Some(h),
            b"stsz" => stsz = Some(h),
            b"stz2" => stz2 = Some(h),
            b"stco" => stco = Some(h),
            b"co64" => co64 = Some(h),
            _ => {}
        }
    }
    // symphonia は stz2 を見ない（stsz があればそれ。stz2 は stsz がないときだけ読む）
    let sizes = stsz.or(stz2);
    let codec = src.codec;
    let broken = |_: (), what: &str| EngineError::Decode {
        codec: codec.to_string(),
        message: format!("MP4：{what}が正しくありません"),
    };
    let (Some(stts), Some(stsc), Some(sizes)) = (stts, stsc, sizes) else {
        return Err(src.decode("MP4：サンプルの表（stts・stsc・stsz）がありません"));
    };
    // stts
    let body = src.read(stts.body, stts.len())?;
    let mut r = Bytes::new(body);
    r.take(4).ok_or_else(|| broken((), "stts "))?;
    let n = r.u32().ok_or_else(|| broken((), "stts "))? as usize;
    if n > r.left() / 8 {
        return Err(broken((), "stts "));
    }
    let mut runs = Vec::new();
    runs.try_reserve_exact(n)
        .map_err(|_| EngineError::Limit("MP4 の表を確保できません".into()))?;
    let mut total = 0u64;
    for _ in 0..n {
        let count = r.u32().ok_or_else(|| broken((), "stts "))?;
        let delta = r.u32().ok_or_else(|| broken((), "stts "))?;
        total = total
            .checked_add(u64::from(count) * u64::from(delta))
            .ok_or_else(|| broken((), "stts の長さ"))?;
        runs.push((count, delta));
    }
    // stsc
    let body = src.read(stsc.body, stsc.len())?;
    let mut r = Bytes::new(body);
    r.take(4).ok_or_else(|| broken((), "stsc "))?;
    let n = r.u32().ok_or_else(|| broken((), "stsc "))? as usize;
    if n > r.left() / 12 {
        return Err(broken((), "stsc "));
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(n)
        .map_err(|_| EngineError::Limit("MP4 の表を確保できません".into()))?;
    for _ in 0..n {
        let first = r.u32().ok_or_else(|| broken((), "stsc "))?;
        let spc = r.u32().ok_or_else(|| broken((), "stsc "))?;
        r.u32().ok_or_else(|| broken((), "stsc "))?;
        if first == 0 {
            return Err(broken((), "stsc の first_chunk "));
        }
        entries.push(StscEntry {
            first_chunk: first - 1,
            spc,
        });
    }
    let mut first_sample = 0u32;
    for i in 0..entries.len() {
        if i + 1 < entries.len() && entries[i + 1].first_chunk < entries[i].first_chunk {
            return Err(broken((), "stsc の順"));
        }
        if entries[i].spc == 0 {
            return Err(broken((), "stsc のサンプルの数"));
        }
        if i + 1 < entries.len() {
            let chunks = entries[i + 1].first_chunk - entries[i].first_chunk;
            first_sample = chunks
                .checked_mul(entries[i].spc)
                .and_then(|d| first_sample.checked_add(d))
                .ok_or_else(|| broken((), "stsc のサンプルの数"))?;
        }
    }
    // stsz・stz2
    let body = src.read(sizes.body, sizes.len())?;
    let mut r = Bytes::new(body);
    r.take(4).ok_or_else(|| broken((), "stsz "))?;
    let sizes = if &sizes.kind == b"stsz" {
        let size = r.u32().ok_or_else(|| broken((), "stsz "))?;
        let count = r.u32().ok_or_else(|| broken((), "stsz "))?;
        if size != 0 {
            Sizes::Constant { size, count }
        } else {
            let count = count as usize;
            if count > r.left() / 4 {
                return Err(broken((), "stsz "));
            }
            let mut v = Vec::new();
            v.try_reserve_exact(count)
                .map_err(|_| EngineError::Limit("MP4 の表を確保できません".into()))?;
            for _ in 0..count {
                v.push(r.u32().ok_or_else(|| broken((), "stsz "))?);
            }
            Sizes::Variable(v)
        }
    } else {
        // stz2：予約 3 バイト、field_size 1 バイト（4・8・16）、sample_count
        r.take(3).ok_or_else(|| broken((), "stz2 "))?;
        let field = r.u8().ok_or_else(|| broken((), "stz2 "))?;
        let count = r.u32().ok_or_else(|| broken((), "stz2 "))? as usize;
        let need = match field {
            4 => count.div_ceil(2),
            8 => count,
            16 => count.checked_mul(2).ok_or_else(|| broken((), "stz2 "))?,
            _ => return Err(broken((), "stz2 の field_size ")),
        };
        if need > r.left() {
            return Err(broken((), "stz2 "));
        }
        let mut v = Vec::new();
        v.try_reserve_exact(count)
            .map_err(|_| EngineError::Limit("MP4 の表を確保できません".into()))?;
        let data = r.take(need).ok_or_else(|| broken((), "stz2 "))?;
        for i in 0..count {
            v.push(match field {
                4 => u32::from(if i % 2 == 0 {
                    data[i / 2] >> 4
                } else {
                    data[i / 2] & 0x0f
                }),
                8 => u32::from(data[i]),
                _ => u32::from(u16::from_be_bytes([data[2 * i], data[2 * i + 1]])),
            });
        }
        Sizes::Variable(v)
    };
    // stco（優先）・co64
    let chunks = match (stco, co64) {
        (Some(h), _) | (None, Some(h)) => {
            let wide = &h.kind == b"co64";
            let body = src.read(h.body, h.len())?;
            let mut r = Bytes::new(body);
            r.take(4).ok_or_else(|| broken((), "stco "))?;
            let n = r.u32().ok_or_else(|| broken((), "stco "))? as usize;
            if n > r.left() / if wide { 8 } else { 4 } {
                return Err(broken((), "stco "));
            }
            let mut v = Vec::new();
            v.try_reserve_exact(n)
                .map_err(|_| EngineError::Limit("MP4 の表を確保できません".into()))?;
            for _ in 0..n {
                v.push(if wide {
                    r.u64().ok_or_else(|| broken((), "co64 "))?
                } else {
                    u64::from(r.u32().ok_or_else(|| broken((), "stco "))?)
                });
            }
            Some(v)
        }
        (None, None) => None,
    };
    Ok(Tables {
        stts: runs,
        stsc: entries,
        sizes,
        chunks,
    })
}

/// mvex の trex と、断片の読みの始まり。
fn fragments(
    src: &mut Source<'_>,
    mvex: &Head,
    index: usize,
    traks: usize,
    trak: &Head,
    stop: Option<Head>,
) -> Result<Fragments, EngineError> {
    let mut trexs = Vec::new();
    let mut b = Boxes::inside(mvex, 2);
    while let Some(h) = b.next(src)? {
        if &h.kind == b"trex" {
            let body = src.read(h.body, h.len().min(24))?;
            let mut r = Bytes::new(body);
            let t = (|| {
                r.take(4)?;
                let id = r.u32()?;
                r.u32()?;
                let dur = r.u32()?;
                let size = r.u32()?;
                Some((id, dur, size))
            })()
            .ok_or_else(|| src.decode("MP4：trex が正しくありません"))?;
            trexs.push(t);
        }
    }
    if trexs.len() != traks {
        return Err(src.decode("MP4：mvex の trex の数が trak の数と合いません"));
    }
    let (track_id, default_dur, default_size) = trexs[index];
    if tkhd_id(src, trak)? != Some(track_id) {
        return Err(src.unsupported("MP4 の trex の並びが trak と違います"));
    }
    let walk = match stop {
        Some(h) => Boxes {
            pos: h.at,
            end: src.len,
            top: true,
            depth: 0,
            lenient_end: true,
        },
        None => Boxes {
            pos: src.len,
            end: src.len,
            top: true,
            depth: 0,
            lenient_end: true,
        },
    };
    Ok(Fragments {
        walk,
        track_id,
        default_dur,
        default_size,
        pending: Vec::new(),
        ready: std::collections::VecDeque::new(),
        current: None,
        done: false,
    })
}

/// moof の中のこのトラックの trun（symphonia の `MoofSegment`）。traf・trun は並べて持たずに、traf ごとに中を 2 度たどる
/// （tfhd を見つけてから trun を順に）。サンプルの数 0 の trun は持たない。
fn moof_runs(
    src: &mut Source<'_>,
    moof: &Head,
    f: &Fragments,
    out: &mut Vec<Run>,
) -> Result<(), EngineError> {
    let codec = src.codec;
    let file_len = src.len;
    let broken = || EngineError::Decode {
        codec: codec.to_string(),
        message: "MP4：trun が正しくありません".into(),
    };
    let mut has_mfhd = false;
    let mut mine = false;
    let mut b = Boxes::inside(moof, 1);
    while let Some(traf) = b.next(src)? {
        match &traf.kind {
            b"mfhd" => {
                has_mfhd = true;
                continue;
            }
            b"traf" => {}
            _ => continue,
        }
        // tfhd（後のものが上書き）
        let Some(tfhd) = find(src, &traf, &[b"tfhd"], 2)? else {
            return Err(src.decode("MP4：traf に tfhd がありません"));
        };
        let body = src.read(tfhd.body, tfhd.len().min(40))?;
        let mut r = Bytes::new(body);
        let (id, base, tdur, tsize) = (|| {
            r.u8()?;
            let fl = r.take(3)?;
            let flags = u32::from_be_bytes([0, fl[0], fl[1], fl[2]]);
            let id = r.u32()?;
            let base = if flags & 0x1 != 0 {
                Some(r.u64()?)
            } else {
                None
            };
            if flags & 0x2 != 0 {
                r.u32()?;
            }
            let dur = if flags & 0x8 != 0 {
                Some(r.u32()?)
            } else {
                None
            };
            let size = if flags & 0x10 != 0 {
                Some(r.u32()?)
            } else {
                None
            };
            Some((id, base, dur, size))
        })()
        .ok_or_else(|| src.decode("MP4：tfhd が正しくありません"))?;
        if id != f.track_id {
            continue;
        }
        if mine {
            return Err(src.unsupported("MP4 の 1 つの moof に同じトラックの traf が 2 つあります"));
        }
        mine = true;
        let base = base.unwrap_or(moof.at);
        let dur = tdur.unwrap_or(f.default_dur);
        let size = tsize.unwrap_or(f.default_size);
        let mut prev_end = base;
        let mut truns = Boxes::inside(&traf, 3);
        while let Some(trun) = truns.next(src)? {
            if &trun.kind != b"trun" {
                continue;
            }
            let body = src.read(trun.body, trun.len())?;
            let mut r = Bytes::new(body);
            let parsed = (|| {
                r.u8()?;
                let fl = r.take(3)?;
                let flags = u32::from_be_bytes([0, fl[0], fl[1], fl[2]]);
                let count = r.u32()?;
                let off = if flags & 0x1 != 0 {
                    Some(r.u32()? as i32)
                } else {
                    None
                };
                let first = if flags & 0x4 != 0 {
                    Some(r.u32()?)
                } else {
                    None
                };
                Some((flags, count, off, first, r.at))
            })();
            let Some((flags, count, off, first, at)) = parsed else {
                return Err(broken());
            };
            if flags & 0x4 != 0 && flags & 0x400 != 0 {
                return Err(broken());
            }
            let start = match off {
                Some(o) => base.checked_add_signed(i64::from(o)).ok_or_else(broken)?,
                None => prev_end,
            };
            if count == 0 {
                prev_end = start;
                continue;
            }
            // symphonia は「最初のサンプルだけ値がある」を first_sample_flags の値のビットで見て、値の列がないと panic する
            if let Some(ff) = first
                && ((flags & 0x100 == 0 && ff & 0x4 != 0)
                    || (flags & 0x200 == 0 && ff & 0x200 != 0))
            {
                return Err(broken());
            }
            let mut run = Run {
                at: start,
                count,
                flags,
                dur,
                size,
                per: Vec::new(),
            };
            let need = (count as usize)
                .checked_mul(run.stride())
                .ok_or_else(broken)?;
            let per = body
                .get(at..at.checked_add(need).ok_or_else(broken)?)
                .ok_or_else(broken)?;
            // trun の中のサンプルは続けて並ぶ。終わりはファイルの中（大きさ 0 のサンプルは読めない。`next_sample`）
            let end = if flags & 0x200 == 0 {
                if size == 0 {
                    return Err(src.decode("MP4：大きさ 0 のサンプルがあります"));
                }
                u64::from(count)
                    .checked_mul(u64::from(size))
                    .and_then(|n| start.checked_add(n))
                    .ok_or_else(broken)?
            } else {
                run.per = per.to_vec();
                let mut end = start;
                for i in 0..count {
                    end = end
                        .checked_add(u64::from(run.sample(i).1))
                        .ok_or_else(broken)?;
                }
                end
            };
            if end > file_len {
                return Err(EngineError::Decode {
                    codec: codec.to_string(),
                    message: "MP4：ファイルの外を指しています（途中で切れています）".into(),
                });
            }
            if flags & 0x200 == 0 && run.stride() > 0 {
                run.per
                    .try_reserve_exact(per.len())
                    .map_err(|_| EngineError::Limit("MP4 の断片を確保できません".into()))?;
                run.per.extend_from_slice(per);
            }
            prev_end = end;
            out.try_reserve(1)
                .map_err(|_| EngineError::Limit("MP4 の断片を確保できません".into()))?;
            out.push(run);
        }
    }
    if !has_mfhd {
        return Err(src.decode("MP4：moof に mfhd がありません"));
    }
    Ok(())
}

/// meta（版と旗の 4 バイトのあとに箱が並ぶ）の最後の ilst。
fn ilst_of(src: &mut Source<'_>, meta: &Head) -> Result<Option<Head>, EngineError> {
    let mut last = None;
    let mut b = Boxes::inside_from(meta, 4, 1);
    while let Some(h) = b.next(src)? {
        if &h.kind == b"ilst" {
            last = Some(h);
        }
    }
    Ok(last)
}

/// タグ（最後の版。モジュールの説明）。
fn read_tags(
    src: &mut Source<'_>,
    udta: Option<&Head>,
    top_ilst: Option<Head>,
) -> Result<Vec<(String, String)>, EngineError> {
    let mut ilst = None;
    if let Some(udta) = udta {
        let mut last_meta = None;
        let mut b = Boxes::inside(udta, 2);
        while let Some(h) = b.next(src)? {
            if &h.kind == b"meta" {
                last_meta = Some(h);
            }
        }
        if let Some(m) = last_meta {
            ilst = ilst_of(src, &m)?;
        }
    }
    let ilst = ilst.or(top_ilst);
    let Some(ilst) = ilst else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut items = Boxes::inside(&ilst, 4);
    while let Some(item) = items.next(src)? {
        if &item.kind == b"----" {
            freeform(src, &item, &mut out)?;
            continue;
        }
        let Some((_, key)) = TAG_KEYS.iter().find(|(k, _)| *k == item.kind) else {
            continue;
        };
        let mut b = Boxes::inside(&item, 5);
        while let Some(d) = b.next(src)? {
            if &d.kind != b"data" || d.len() < 8 {
                continue;
            }
            let head = src.read(d.body, 8)?;
            if head[0] != 0 {
                continue;
            }
            let kind = u32::from_be_bytes([0, head[1], head[2], head[3]]);
            let n = d.len() - 8;
            let value = match kind {
                // 不正な列の置き換えは長くなるだけなので、生の長さで先に捨てられる
                1 | 4 if n <= TEXT_MAX as u64 => {
                    String::from_utf8_lossy(src.read(d.body + 8, n)?).into_owned()
                }
                // UTF-16 は 2 バイトが 1 バイト以上になる（BOM 2 バイトと奇数長の最後の 1 バイトは数えない）
                2 | 5 if n <= 2 * TEXT_MAX as u64 + 3 => utf16(src.read(d.body + 8, n)?),
                _ => continue,
            };
            if value.len() <= TEXT_MAX {
                out.push((key.to_string(), value));
            }
        }
    }
    Ok(out)
}

/// 自由形式の項目（`----`）のうち、名前が `iTunSMPB` で終わるもの（AAC の priming。symphonia のキーは `mean:name`）の
/// UTF-8 の値。読み手（前の symphonia も自作も）は最初の iTunSMPB だけを使うので、項目ごとに最初の値だけを持つ。名前は
/// 値より先に確かめ、`mean`・`name`・値は置き換え（不正な UTF-8）のあとの長さが [`TEXT_MAX`] を超えれば捨てる（持つ量を
/// `held::mp4` の数え方の内に収める）。
fn freeform(
    src: &mut Source<'_>,
    item: &Head,
    out: &mut Vec<(String, String)>,
) -> Result<(), EngineError> {
    let text = |src: &mut Source<'_>, at: u64, n: u64| -> Result<Option<String>, EngineError> {
        let v = String::from_utf8_lossy(src.read(at, n)?).into_owned();
        Ok((v.len() <= TEXT_MAX).then_some(v))
    };
    let (mut mean, mut name) = (None::<String>, None::<String>);
    let mut b = Boxes::inside(item, 5);
    while let Some(d) = b.next(src)? {
        if matches!(&d.kind, b"mean" | b"name") && d.len() >= 4 && d.len() <= TEXT_MAX as u64 {
            let v = text(src, d.body + 4, d.len() - 4)?;
            if &d.kind == b"mean" {
                mean = v.or(mean);
            } else {
                name = v.or(name);
            }
        }
    }
    if mean.is_none() && name.is_none() {
        return Ok(());
    }
    let key = format!("{}:{}", mean.unwrap_or_default(), name.unwrap_or_default());
    if !key.ends_with("iTunSMPB") {
        return Ok(());
    }
    let mut b = Boxes::inside(item, 5);
    while let Some(d) = b.next(src)? {
        if &d.kind == b"data" && d.len() >= 8 && d.len() <= TEXT_MAX as u64 {
            let head = src.read(d.body, 8)?;
            if head[0] == 0 && u32::from_be_bytes([0, head[1], head[2], head[3]]) == 1 {
                if let Some(v) = text(src, d.body + 8, d.len() - 8)? {
                    out.push((key, v));
                }
                return Ok(());
            }
        }
    }
    Ok(())
}

/// MP4 のタグの UTF-16（symphonia と同じ：`FE FF` なら BE、`FF FE` なら LE、ほかは全体を BE。奇数長の最後のバイトは
/// 捨て、対にならないサロゲートは U+FFFD）。
fn utf16(b: &[u8]) -> String {
    let (le, b) = match b {
        [0xfe, 0xff, rest @ ..] => (false, rest),
        [0xff, 0xfe, rest @ ..] => (true, rest),
        _ => (false, b),
    };
    let units = b.as_chunks::<2>().0.iter().map(|c| {
        if le {
            u16::from_le_bytes([c[0], c[1]])
        } else {
            u16::from_be_bytes([c[0], c[1]])
        }
    });
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

impl Mp4Track<'_> {
    fn decode(&self, message: impl Into<String>) -> EngineError {
        self.src.decode(message)
    }

    /// 表どうしの数を確かめる（壊れた表は `Decode`。モジュールの説明）。
    fn check_tables(&self) -> Result<(), EngineError> {
        let count: u64 = self.tables.stts.iter().map(|(c, _)| u64::from(*c)).sum();
        let sizes = match &self.tables.sizes {
            Sizes::Constant { count, .. } => u64::from(*count),
            Sizes::Variable(v) => v.len() as u64,
        };
        if count != sizes {
            return Err(self.decode("MP4：stts と stsz のサンプルの数が合いません"));
        }
        if count > 0 && self.tables.stsc.is_empty() {
            return Err(self.decode("MP4：stsc が空です"));
        }
        if count > 0 && self.tables.chunks.is_none() {
            return Err(self.decode("MP4：stco も co64 もありません"));
        }
        Ok(())
    }

    fn start(&mut self) {
        self.stts_i = 0;
        self.stts_left = self.tables.stts.first().map_or(0, |r| r.0);
        self.stsc_i = 0;
        self.chunk = self.tables.stsc.first().map_or(0, |e| e.first_chunk);
        self.in_chunk = 0;
        self.sample = 0;
        self.next_pos = 0;
        self.moov_done = false;
    }

    /// 次のサンプル（位置・大きさ・長さ）。終わりなら None。サンプルはファイルの中にあり、大きさが 0 でない（ALAC は空の
    /// パケットを読めない。symphonia も読むときに誤り。サンプルの数がファイルの長さで抑えられる）。
    pub fn next_sample(&mut self) -> Result<Option<Sample>, EngineError> {
        let s = if self.moov_done {
            None
        } else {
            self.next_moov_sample()?
        };
        let s = match s {
            Some(s) => Some(s),
            None => {
                self.moov_done = true;
                self.next_fragment_sample()?
            }
        };
        if let Some(s) = &s {
            if s.size == 0 {
                return Err(self.decode("MP4：大きさ 0 のサンプルがあります"));
            }
            if s.at
                .checked_add(u64::from(s.size))
                .is_none_or(|e| e > self.src.len)
            {
                return Err(self.decode("MP4：ファイルの外を指しています（途中で切れています）"));
            }
        }
        Ok(s)
    }

    fn next_moov_sample(&mut self) -> Result<Option<Sample>, EngineError> {
        // stts の数で終わる
        while self.stts_left == 0 {
            self.stts_i += 1;
            match self.tables.stts.get(self.stts_i) {
                Some(r) => self.stts_left = r.0,
                None => return Ok(None),
            }
        }
        let dur = self.tables.stts[self.stts_i].1;
        self.stts_left -= 1;
        // chunk の項目（first_chunk が今の chunk 以下の最後のもの）
        while self.stsc_i + 1 < self.tables.stsc.len()
            && self.tables.stsc[self.stsc_i + 1].first_chunk <= self.chunk
        {
            self.stsc_i += 1;
        }
        let chunks = self.tables.chunks.as_deref().unwrap_or(&[]);
        let Some(base) = chunks.get(self.chunk as usize).copied() else {
            return Err(self.decode("MP4：stco の chunk が足りません"));
        };
        let size = match &self.tables.sizes {
            Sizes::Constant { size, .. } => *size,
            Sizes::Variable(v) => v[self.sample as usize],
        };
        let at = if self.in_chunk == 0 {
            if base < self.next_pos {
                return Err(self
                    .src
                    .unsupported("MP4 の chunk の位置が前のサンプルより前にあります"));
            }
            base
        } else {
            self.next_pos
        };
        self.next_pos = at
            .checked_add(u64::from(size))
            .ok_or_else(|| self.decode("MP4：サンプルの位置があふれます"))?;
        self.sample += 1;
        self.in_chunk += 1;
        if self.in_chunk == self.tables.stsc[self.stsc_i].spc {
            self.in_chunk = 0;
            self.chunk = self
                .chunk
                .checked_add(1)
                .ok_or_else(|| self.decode("MP4：chunk の番号があふれます"))?;
        }
        Ok(Some(Sample { at, size, dur }))
    }

    fn next_fragment_sample(&mut self) -> Result<Option<Sample>, EngineError> {
        let Some(f) = self.frag.as_mut() else {
            return Ok(None);
        };
        loop {
            if let Some((run, i, at)) = f.current.as_mut() {
                if *i < run.count {
                    let (dur, size) = run.sample(*i);
                    let s = Sample { at: *at, size, dur };
                    *i += 1;
                    *at += u64::from(size);
                    if s.at < self.next_pos {
                        return Err(self
                            .src
                            .unsupported("MP4 の trun の位置が前のサンプルより前にあります"));
                    }
                    self.next_pos = s.at + u64::from(s.size);
                    return Ok(Some(s));
                }
                f.current = None;
            }
            if let Some(run) = f.ready.pop_front() {
                let at = run.at;
                f.current = Some((run, 0, at));
                continue;
            }
            if f.done {
                return Ok(None);
            }
            match f.walk.next(&mut self.src)? {
                Some(h) if &h.kind == b"moof" => {
                    let mut out = std::mem::take(&mut f.pending);
                    moof_runs(&mut self.src, &h, f, &mut out)?;
                    f.pending = out;
                }
                Some(h) if &h.kind == b"mdat" => {
                    // 出す列は空（空になってから読み進める）なので、読んだ trun の列をそのまま渡す
                    f.ready = std::mem::take(&mut f.pending).into();
                }
                Some(_) => {}
                None => {
                    f.done = true;
                    if !f.pending.is_empty() {
                        return Err(self
                            .src
                            .decode("MP4：moof のあとに mdat がないまま終わります"));
                    }
                }
            }
        }
    }

    /// サンプルのバイト。
    pub fn read_sample(&mut self, s: &Sample) -> Result<&[u8], EngineError> {
        self.src.read(s.at, u64::from(s.size))
    }
}

/// 試験の m4a を作る（ALAC のトラック 1 つと、前に置くほかのトラック、タグ、断片）。
#[cfg(test)]
pub(crate) mod m4a_gen {
    /// 箱。
    pub fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut o = ((8 + body.len()) as u32).to_be_bytes().to_vec();
        o.extend_from_slice(kind);
        o.extend_from_slice(body);
        o
    }

    /// 版と旗のある箱。
    pub fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![version];
        b.extend_from_slice(&flags.to_be_bytes()[1..]);
        b.extend_from_slice(body);
        bx(kind, &b)
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// タグの 1 つの項目（`data` の型と中身の列）。
    #[derive(Debug, Clone)]
    pub struct Tag {
        pub key: [u8; 4],
        pub data: Vec<(u32, Vec<u8>)>,
    }

    /// 前に置くトラックの種類。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Extra {
        /// チャプターの文字（`text` の見本。コーデックなし）。
        Text,
        /// AAC（`mp4a` と esds。読み手は `Unsupported`）。
        Aac,
        /// AAC の esds で、DecoderConfigDescriptor の前にほかの記述子があるもの（symphonia は飛ばして AAC とみなす）。
        AacDesc,
        /// v2 の `sowt` の見本で、lpcm の値が NULL になるもの（12 ビット。symphonia は NULL として次のトラックへ）。
        PcmV2,
    }

    /// 作る m4a の形。
    #[derive(Debug, Clone)]
    pub struct M4a {
        pub cookie: Vec<u8>,
        /// （パケット, stts の delta）。
        pub packets: Vec<(Vec<u8>, u32)>,
        pub moov_first: bool,
        pub co64: bool,
        pub stz2: bool,
        /// chunk ごとのサンプルの数の並び（足りなければ最後の値を繰り返す）。
        pub chunking: Vec<u32>,
        pub tags: Vec<Tag>,
        /// 最上段の meta のタグ（moov の udta の前に置く）。
        pub top_tags: Option<Vec<Tag>>,
        pub extra_before: Vec<Extra>,
        /// 断片にする（1 つの moof に入れるサンプルの数）。
        pub fragment: Option<u32>,
        /// 見本の版（0・1）。
        pub entry_version: u16,
        /// chunk の間に空ける詰め物（バイト）。
        pub gap: u32,
        /// stsz のあとに、中身の違う stz2 を足す（symphonia は stz2 を見ない）。
        pub extra_stz2: bool,
        /// 断片の付け足し：traf にサンプル 0 の trun（first_sample_flags に 0x4・0x200）、最後に mdat のない空の moof、
        /// 終わりに 16 バイトに満たない 64 ビットの大きさの頭。
        pub frag_extras: bool,
    }

    impl M4a {
        pub fn new(cookie: Vec<u8>, packets: Vec<(Vec<u8>, u32)>) -> Self {
            Self {
                cookie,
                packets,
                moov_first: true,
                co64: false,
                stz2: false,
                chunking: vec![4],
                tags: Vec::new(),
                top_tags: None,
                extra_before: Vec::new(),
                fragment: None,
                entry_version: 0,
                gap: 0,
                extra_stz2: false,
                frag_extras: false,
            }
        }

        fn ftyp() -> Vec<u8> {
            bx(b"ftyp", b"M4A \x00\x00\x02\x00M4A mp42isom")
        }

        fn mvhd() -> Vec<u8> {
            let mut b = vec![0u8; 8];
            b.extend_from_slice(&1000u32.to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&0x0001_0000u32.to_be_bytes());
            b.extend_from_slice(&0x0100u16.to_be_bytes());
            b.extend_from_slice(&[0; 10]);
            b.extend_from_slice(&[0; 36]);
            b.extend_from_slice(&[0; 24]);
            b.extend_from_slice(&3u32.to_be_bytes());
            full(b"mvhd", 0, 0, &b)
        }

        fn tkhd(id: u32) -> Vec<u8> {
            let mut b = vec![0u8; 8];
            b.extend_from_slice(&id.to_be_bytes());
            b.extend_from_slice(&[0; 4]);
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&[0; 8]);
            b.extend_from_slice(&[0; 4]);
            b.extend_from_slice(&0x0100u16.to_be_bytes());
            b.extend_from_slice(&[0; 2]);
            b.extend_from_slice(&[0; 36]);
            b.extend_from_slice(&[0; 8]);
            full(b"tkhd", 0, 7, &b)
        }

        fn mdhd(scale: u32, dur: u32) -> Vec<u8> {
            let mut b = vec![0u8; 8];
            b.extend_from_slice(&scale.to_be_bytes());
            b.extend_from_slice(&dur.to_be_bytes());
            b.extend_from_slice(&[0x55, 0xc4, 0, 0]);
            full(b"mdhd", 0, 0, &b)
        }

        fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
            let mut b = vec![0u8; 4];
            b.extend_from_slice(kind);
            b.extend_from_slice(&[0; 12]);
            b.push(0);
            full(b"hdlr", 0, 0, &b)
        }

        fn audio_entry(&self, kind: &[u8; 4], children: &[u8]) -> Vec<u8> {
            let mut b = vec![0u8; 6];
            b.extend_from_slice(&1u16.to_be_bytes());
            b.extend_from_slice(&self.entry_version.to_be_bytes());
            b.extend_from_slice(&[0; 6]);
            b.extend_from_slice(&2u16.to_be_bytes());
            b.extend_from_slice(&16u16.to_be_bytes());
            b.extend_from_slice(&[0; 4]);
            b.extend_from_slice(&(44_100u32 << 16).to_be_bytes());
            if self.entry_version == 1 {
                b.extend_from_slice(&[0; 16]);
            }
            b.extend_from_slice(children);
            bx(kind, &b)
        }

        fn stbl(
            &self,
            entry: Vec<u8>,
            samples: &[(u32, u32)],
            offsets: &[u64],
            per_chunk: &[u32],
        ) -> Vec<u8> {
            let mut stsd = 1u32.to_be_bytes().to_vec();
            stsd.extend_from_slice(&entry);
            // stts（同じ delta は 1 つの項目に）
            let mut runs: Vec<(u32, u32)> = Vec::new();
            for (_, d) in samples {
                match runs.last_mut() {
                    Some((c, dd)) if dd == d => *c += 1,
                    _ => runs.push((1, *d)),
                }
            }
            let mut stts = (runs.len() as u32).to_be_bytes().to_vec();
            for (c, d) in &runs {
                stts.extend_from_slice(&c.to_be_bytes());
                stts.extend_from_slice(&d.to_be_bytes());
            }
            // stsc（同じ数が続く chunk は 1 つの項目に）
            let mut entries: Vec<(u32, u32)> = Vec::new();
            for (i, n) in per_chunk.iter().enumerate() {
                if entries.last().is_none_or(|(_, m)| m != n) {
                    entries.push((i as u32 + 1, *n));
                }
            }
            let mut stsc = (entries.len() as u32).to_be_bytes().to_vec();
            for (f, n) in &entries {
                stsc.extend_from_slice(&f.to_be_bytes());
                stsc.extend_from_slice(&n.to_be_bytes());
                stsc.extend_from_slice(&1u32.to_be_bytes());
            }
            let sizes = if self.stz2 {
                let mut b = vec![0, 0, 0, 16];
                b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
                for (s, _) in samples {
                    b.extend_from_slice(&(*s as u16).to_be_bytes());
                }
                full(b"stz2", 0, 0, &b)
            } else {
                let constant = samples.windows(2).all(|w| w[0].0 == w[1].0) && !samples.is_empty();
                let mut b = Vec::new();
                if constant {
                    b.extend_from_slice(&samples[0].0.to_be_bytes());
                    b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
                } else {
                    b.extend_from_slice(&0u32.to_be_bytes());
                    b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
                    for (s, _) in samples {
                        b.extend_from_slice(&s.to_be_bytes());
                    }
                }
                full(b"stsz", 0, 0, &b)
            };
            let mut co = (offsets.len() as u32).to_be_bytes().to_vec();
            for o in offsets {
                if self.co64 {
                    co.extend_from_slice(&o.to_be_bytes());
                } else {
                    co.extend_from_slice(&(*o as u32).to_be_bytes());
                }
            }
            let co = if self.co64 {
                full(b"co64", 0, 0, &co)
            } else {
                full(b"stco", 0, 0, &co)
            };
            let extra = if self.extra_stz2 && !self.stz2 {
                let mut b = vec![0, 0, 0, 8];
                b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
                b.extend(std::iter::repeat_n(1u8, samples.len()));
                full(b"stz2", 0, 0, &b)
            } else {
                Vec::new()
            };
            bx(
                b"stbl",
                &cat(&[
                    full(b"stsd", 0, 0, &stsd),
                    full(b"stts", 0, 0, &stts),
                    full(b"stsc", 0, 0, &stsc),
                    sizes,
                    extra,
                    co,
                ]),
            )
        }

        fn trak(&self, id: u32, handler: &[u8; 4], stbl: Vec<u8>, dur: u32) -> Vec<u8> {
            let minf = bx(
                b"minf",
                &cat(&[
                    full(b"smhd", 0, 0, &[0; 4]),
                    bx(b"dinf", &full(b"dref", 0, 0, &[0, 0, 0, 0])),
                    stbl,
                ]),
            );
            let mdia = bx(
                b"mdia",
                &cat(&[Self::mdhd(44_100, dur), Self::hdlr(handler), minf]),
            );
            bx(b"trak", &cat(&[Self::tkhd(id), mdia]))
        }

        fn ilst(tags: &[Tag]) -> Vec<u8> {
            let mut items = Vec::new();
            for t in tags {
                let mut body = Vec::new();
                for (kind, v) in &t.data {
                    let mut d = kind.to_be_bytes().to_vec();
                    d[0] = 0;
                    d.extend_from_slice(&[0; 4]);
                    d.extend_from_slice(v);
                    body.extend_from_slice(&bx(b"data", &d));
                }
                items.extend_from_slice(&bx(&t.key, &body));
            }
            let mut meta = vec![0u8; 4];
            meta.extend_from_slice(&Self::hdlr(b"mdir"));
            meta.extend_from_slice(&bx(b"ilst", &items));
            bx(b"meta", &meta)
        }

        /// ファイルを作る。
        pub fn build(&self) -> Vec<u8> {
            let alac_child = full(b"alac", 0, 0, &self.cookie);
            let entry = self.audio_entry(b"alac", &alac_child);
            let total: u32 = self.packets.iter().map(|(_, d)| *d).sum();
            let n_extra = self.extra_before.len() as u32;
            let extra_traks = |s: &Self| -> Vec<u8> {
                let mut v = Vec::new();
                for (i, e) in s.extra_before.iter().enumerate() {
                    let stbl = match e {
                        Extra::Text => s.stbl(bx(b"text", &[0; 8]), &[], &[], &[]),
                        Extra::Aac => {
                            // ES_Descriptor（tag 3）→ DecoderConfigDescriptor（tag 4、object type 0x40）
                            let esds = full(
                                b"esds",
                                0,
                                0,
                                &[
                                    3, 0x19, 0, 1, 0, 4, 0x11, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0,
                                    0, 0, 0, 5, 2, 0x12, 0x10, 6, 1, 2,
                                ],
                            );
                            s.stbl(s.audio_entry(b"mp4a", &esds), &[], &[], &[])
                        }
                        Extra::AacDesc => {
                            // 知らない記述子（tag 0x0e）が DecoderConfigDescriptor の前にある
                            let esds = full(
                                b"esds",
                                0,
                                0,
                                &[
                                    3, 0x1d, 0, 1, 0, 0x0e, 2, 0, 0, 4, 0x11, 0x40, 0x15, 0, 0, 0,
                                    0, 0, 0, 0, 0, 0, 0, 0, 5, 2, 0x12, 0x10, 6, 1, 2,
                                ],
                            );
                            s.stbl(s.audio_entry(b"mp4a", &esds), &[], &[], &[])
                        }
                        Extra::PcmV2 => {
                            let mut b = vec![0u8; 6];
                            b.extend_from_slice(&1u16.to_be_bytes());
                            b.extend_from_slice(&2u16.to_be_bytes());
                            b.extend_from_slice(&[0; 6]);
                            b.extend_from_slice(&3u16.to_be_bytes());
                            b.extend_from_slice(&16u16.to_be_bytes());
                            b.extend_from_slice(&[0xff, 0xfe, 0, 0]);
                            b.extend_from_slice(&(1u32 << 16).to_be_bytes());
                            b.extend_from_slice(&72u32.to_be_bytes());
                            b.extend_from_slice(&48_000f64.to_bits().to_be_bytes());
                            b.extend_from_slice(&2u32.to_be_bytes());
                            b.extend_from_slice(&0x7f00_0000u32.to_be_bytes());
                            b.extend_from_slice(&12u32.to_be_bytes());
                            b.extend_from_slice(&0x6u32.to_be_bytes());
                            b.extend_from_slice(&4u32.to_be_bytes());
                            b.extend_from_slice(&1u32.to_be_bytes());
                            s.stbl(bx(b"sowt", &b), &[], &[], &[])
                        }
                    };
                    v.extend_from_slice(&s.trak(i as u32 + 1, b"soun", stbl, 0));
                }
                v
            };
            let udta = bx(b"udta", &Self::ilst(&self.tags));
            let ftyp = Self::ftyp();
            let top_meta = self
                .top_tags
                .as_ref()
                .map(|t| Self::ilst(t))
                .unwrap_or_default();
            let id = n_extra + 1;
            if let Some(per) = self.fragment {
                // 断片：moov の表は空、mvex に trex
                let stbl = self.stbl(entry.clone(), &[], &[], &[]);
                let mut trexs = Vec::new();
                for i in 0..=n_extra {
                    let mut b = (i + 1).to_be_bytes().to_vec();
                    b.extend_from_slice(&1u32.to_be_bytes());
                    b.extend_from_slice(&0u32.to_be_bytes());
                    b.extend_from_slice(&0u32.to_be_bytes());
                    b.extend_from_slice(&0u32.to_be_bytes());
                    trexs.extend_from_slice(&full(b"trex", 0, 0, &b));
                }
                let moov = bx(
                    b"moov",
                    &cat(&[
                        Self::mvhd(),
                        extra_traks(self),
                        self.trak(id, b"soun", stbl, 0),
                        bx(b"mvex", &trexs),
                        udta,
                    ]),
                );
                let mut out = cat(&[ftyp, top_meta, moov]);
                for (seq, group) in self.packets.chunks(per.max(1) as usize).enumerate() {
                    let build_moof = |data_offset: u32| {
                        let mut tfhd = id.to_be_bytes().to_vec();
                        let tfhd = {
                            let _ = &mut tfhd;
                            full(b"tfhd", 0, 0x02_0000, &tfhd)
                        };
                        let mut trun = (group.len() as u32).to_be_bytes().to_vec();
                        trun.extend_from_slice(&data_offset.to_be_bytes());
                        for (p, d) in group {
                            trun.extend_from_slice(&d.to_be_bytes());
                            trun.extend_from_slice(&(p.len() as u32).to_be_bytes());
                        }
                        let trun = full(b"trun", 0, 0x0301, &trun);
                        let mfhd = full(b"mfhd", 0, 0, &(seq as u32 + 1).to_be_bytes());
                        // サンプル 0 の trun（first_sample_flags の値に 0x4・0x200）
                        let empty = if self.frag_extras {
                            let mut b = 0u32.to_be_bytes().to_vec();
                            b.extend_from_slice(&0x204u32.to_be_bytes());
                            full(b"trun", 0, 0x4, &b)
                        } else {
                            Vec::new()
                        };
                        bx(
                            b"moof",
                            &cat(&[mfhd, bx(b"traf", &cat(&[tfhd, empty, trun]))]),
                        )
                    };
                    let probe = build_moof(0);
                    let moof = build_moof(probe.len() as u32 + 8);
                    let data: Vec<u8> = group.iter().flat_map(|(p, _)| p.clone()).collect();
                    out.extend_from_slice(&moof);
                    out.extend_from_slice(&bx(b"mdat", &data));
                }
                if self.frag_extras {
                    // mdat のない空の moof と、終わりの切れた 64 ビットの大きさの頭
                    let tfhd = full(b"tfhd", 0, 0x02_0000, &id.to_be_bytes());
                    let trun = full(b"trun", 0, 0, &0u32.to_be_bytes());
                    let mfhd = full(b"mfhd", 0, 0, &999u32.to_be_bytes());
                    out.extend_from_slice(&bx(
                        b"moof",
                        &cat(&[mfhd, bx(b"traf", &cat(&[tfhd, trun]))]),
                    ));
                    out.extend_from_slice(&[0, 0, 0, 1, b'j', b'u', b'n', b'k', 0, 0, 0, 0]);
                }
                return out;
            }
            // chunk の分け方
            let mut per_chunk = Vec::new();
            let mut left = self.packets.len() as u32;
            let mut k = 0usize;
            while left > 0 {
                let n = self
                    .chunking
                    .get(k)
                    .or(self.chunking.last())
                    .copied()
                    .unwrap_or(1)
                    .max(1)
                    .min(left);
                per_chunk.push(n);
                left -= n;
                k += 1;
            }
            let samples: Vec<(u32, u32)> = self
                .packets
                .iter()
                .map(|(p, d)| (p.len() as u32, *d))
                .collect();
            // mdat の中身（chunk の間に詰め物）
            let mut data = Vec::new();
            let mut rel = Vec::new();
            let mut i = 0usize;
            for n in &per_chunk {
                data.extend(std::iter::repeat_n(0xa5u8, self.gap as usize));
                rel.push(data.len() as u64);
                for _ in 0..*n {
                    data.extend_from_slice(&self.packets[i].0);
                    i += 1;
                }
            }
            let moov_of = |base: u64| {
                let offsets: Vec<u64> = rel.iter().map(|r| base + r).collect();
                let stbl = self.stbl(entry.clone(), &samples, &offsets, &per_chunk);
                bx(
                    b"moov",
                    &cat(&[
                        Self::mvhd(),
                        extra_traks(self),
                        self.trak(id, b"soun", stbl, total),
                        udta.clone(),
                    ]),
                )
            };
            let mdat_head = 8u64;
            if self.moov_first {
                let len = moov_of(0).len() as u64;
                let base = (ftyp.len() + top_meta.len()) as u64 + len + mdat_head;
                cat(&[ftyp, top_meta, moov_of(base), bx(b"mdat", &data)])
            } else {
                let base = (ftyp.len() + top_meta.len()) as u64 + mdat_head;
                cat(&[ftyp, top_meta, bx(b"mdat", &data), moov_of(base)])
            }
        }
    }
}
