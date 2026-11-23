//! 自作の読み取り・書き出しの共通の部品（音声のタグ・ループ点・MP4 の編集リスト・NaN・読む元。docs/editing.md §9.2）。M16 段 10 で
//! symphonia を外したとき、symphonia の読み口を包んでいた `codec::symph` から、symphonia によらない部品だけを残した。
//!
//! - 読む元は 2 つ（[`Media`]）：読み込んだバイト列（読み口の口 `shape`・`decode`。大きさは `max_value_bytes` まで。
//!   [`read_all`]）と、シークできる読み口（ファイル。流して読む口 `decode_stream`・形 `shape_file`。M13 段 2。全体を読み込まない）。
//!   自作の読み口（`byte_in`・`ogg_read`・`mp4_read`・`flac_read`・`aac::adts`）がどちらも同じ道で読むので、値は同じ。
//! - 標本：AAC・Vorbis の NaN は `0x7fc00000` にそろえる（[`canonical_nan`]）。
//! - タグ：Vorbis コメント（FLAC・Ogg）と MP4 の iTunes のタグから、title・artist・album・comment・date・genre・
//!   copyright を読み、FLAC・Ogg のほかのキーは `vorbis:<KEY>` のまま運ぶ（FLAC に書き戻せる。[`meta_from_tags`]）。
//!   `LOOPSTART`・`LOOPLENGTH`（Vorbis コメント）をループ点にする（終わり = LOOPSTART + LOOPLENGTH。含まない）。
//! - 書き出し（FLAC・Ogg の Vorbis コメント。[`vorbis_comments_for`]）は読み取りの逆：ループ点は `start < end ≤ 長さ` の
//!   ときだけ書き、読み取りが同じタグにまとめる名前（大文字と小文字だけの違い・DESCRIPTION と COMMENT・`cprt` と
//!   COPYRIGHT）が重なれば先の 1 つだけを書く（既知の名前を優先）。名前の扱いは読み取りと書き出しで 1 つの表
//!   （`KNOWN_TAGS`・[`comment_name`]）から決める。
//! - AAC の切り取り（[`mp4_edit`]・[`parse_itunsmpb`]）・HE-AAC の明示の印（[`is_he_aac`]）・ALAC の cookie のビット数
//!   （[`alac_bits`]）。

use std::collections::BTreeSet;
use std::io::SeekFrom;

use crate::codec::{ReadSeek, try_vec};
use crate::error::EngineError;
use crate::limits::Limits;
use crate::value::{AudioMeta, LoopRange};

/// 名前・タグとして読む 1 つの文字列の長さの上限（バイト）。
const TEXT_MAX: usize = 4_096;
/// メタデータの 1 項目ごとに予算から引く固定の重さ（バイト。WAV と同じ）。
const ITEM_COST: usize = 32;

/// 読む元（モジュールの説明）。
pub(crate) enum Media<'a> {
    /// 読み込んだバイト列。
    Bytes(&'a [u8]),
    /// シークできる読み口（ファイル。開くたびに頭へ戻す）。
    Reader(&'a mut (dyn ReadSeek + Send)),
}

impl Media<'_> {
    /// MP4 の編集リストの切り取り（[`mp4_edit`]。読み口は箱の頭をたどり、要る箱の頭だけを読む：[`mp4_edit_reader`]）。
    pub fn mp4_edit(&mut self, rate: u32) -> Result<Option<(u64, u64)>, EngineError> {
        match self {
            Media::Bytes(data) => Ok(mp4_edit(data, rate)),
            Media::Reader(r) => mp4_edit_reader(&mut **r, rate),
        }
    }
}

/// ファイルを読み込む（`max_value_bytes` まで）。
pub(crate) fn read_all(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Vec<u8>, EngineError> {
    let len = src.seek(SeekFrom::End(0))?;
    if len > limits.max_value_bytes {
        return Err(EngineError::Limit("ファイルが大きすぎます".into()));
    }
    src.seek(SeekFrom::Start(0))?;
    let mut buf = try_vec(len as usize, 0u8)?;
    src.read_exact(&mut buf)?;
    Ok(buf)
}

/// NaN を 1 つのビット列（`0x7fc00000`。値のダイジェスト `kura-pcm-v1` がそろえる形と同じ）にする。ほかの値はそのまま。
///
/// 壊れた Vorbis・AAC では、デコードの途中で inf − inf・0 · inf などが起きて NaN が出る。そのビット列（符号・ペイロード）は
/// CPU の既定の NaN（x86 の SSE は `0xffc00000`、ARM は `0x7fc00000`）と演算の順で決まり、arm64 と x86_64 で違う
/// （値のダイジェストは同じでも、f32 の WAV に書いたバイト列が違った。段 9 の検証）。値に入れる前にそろえて、読んだ値を
/// ビット単位でどの環境でも同じにする。ダイジェストはもともと NaN をそろえるので、デコーダの識別子は変えない。
pub(crate) fn canonical_nan(s: f32) -> f32 {
    if s.is_nan() {
        f32::from_bits(0x7fc0_0000)
    } else {
        s
    }
}

/// 読み口の `[at, end)` の中の箱を、頭だけ読んで順にたどる（[`mp4_edit`] の `boxes` と同じ規則：壊れた箱で止まる・
/// 大きさ 0 は範囲の終わりまで・大きさ 1 は 64 ビット）。中身は読まない（シークで飛ばす）。
struct BoxWalk {
    at: u64,
    end: u64,
}

impl BoxWalk {
    fn new(at: u64, end: u64) -> Self {
        Self { at, end }
    }

    /// 次の箱（種類, 中身の始まり, 中身の終わり）。範囲の終わり・壊れた箱なら None（それより後はたどらない）。
    fn next(&mut self, src: &mut dyn ReadSeek) -> Result<Option<([u8; 4], u64, u64)>, EngineError> {
        let at = self.at;
        if at.saturating_add(8) > self.end {
            return Ok(None);
        }
        src.seek(SeekFrom::Start(at))?;
        let mut h = [0u8; 16];
        let want = if at + 16 <= self.end { 16 } else { 8 };
        src.read_exact(&mut h[..want])?;
        let size = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let kind = [h[4], h[5], h[6], h[7]];
        let (head, size) = if size == 1 && at + 16 <= self.end {
            let mut v = [0u8; 8];
            v.copy_from_slice(&h[8..16]);
            (16u64, u64::from_be_bytes(v))
        } else if size == 0 {
            (8, self.end - at)
        } else {
            (8, size)
        };
        if size < head || at.saturating_add(size) > self.end {
            self.at = self.end;
            return Ok(None);
        }
        self.at = at + size;
        Ok(Some((kind, at + head, at + size)))
    }
}

/// `[at, end)` の中で最初の `kind` の箱の中身の範囲（壊れた箱より前だけを見る。[`mp4_edit`] の `find` と同じ）。
fn find_box(
    src: &mut dyn ReadSeek,
    at: u64,
    end: u64,
    kind: &[u8; 4],
) -> Result<Option<(u64, u64)>, EngineError> {
    let mut walk = BoxWalk::new(at, end);
    while let Some((k, s, e)) = walk.next(src)? {
        if &k == kind {
            return Ok(Some((s, e)));
        }
    }
    Ok(None)
}

/// 箱の中身の頭 `n` バイトまで（中身が短ければ中身の全部）。
fn read_head(
    src: &mut dyn ReadSeek,
    (start, end): (u64, u64),
    n: u64,
) -> Result<Vec<u8>, EngineError> {
    let len = (end - start).min(n);
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
    src.seek(SeekFrom::Start(start))?;
    src.read_exact(&mut buf)?;
    Ok(buf)
}

/// mvhd・mdhd の中身の時間の単位（版 1 は 20 バイト目、版 0 は 12 バイト目から）。中身の頭 24 バイトだけを見る。
fn time_scale(b: &[u8]) -> Option<u32> {
    Some(match b.first()? {
        1 => u32::from_be_bytes(b.get(20..24)?.try_into().ok()?),
        _ => u32::from_be_bytes(b.get(12..16)?.try_into().ok()?),
    })
}

/// elst の項目の数の上限（[`mp4_edit`] が見る数）。
const ELST_MAX: u32 = 64;

/// elst の中身から、最初の空でない編集を (始まり, 長さ)（メディアの時間の単位）で返す（中身の頭 8 ＋ 64 × 20 バイトだけを
/// 見る）。
fn elst_first(elst: &[u8], media_scale: u32, movie_scale: u64) -> Option<(u64, u64)> {
    let version = *elst.first()?;
    let count = u32::from_be_bytes(elst.get(4..8)?.try_into().ok()?);
    let mut at = 8usize;
    for _ in 0..count.min(ELST_MAX) {
        let (dur, time, step) = if version == 1 {
            (
                u64::from_be_bytes(elst.get(at..at + 8)?.try_into().ok()?),
                i64::from_be_bytes(elst.get(at + 8..at + 16)?.try_into().ok()?),
                20,
            )
        } else {
            (
                u64::from(u32::from_be_bytes(elst.get(at..at + 4)?.try_into().ok()?)),
                i64::from(i32::from_be_bytes(
                    elst.get(at + 4..at + 8)?.try_into().ok()?,
                )),
                12,
            )
        };
        at += step;
        if time < 0 {
            continue;
        }
        // 長さは映像全体の時間の単位 → メディアの時間の単位
        let len = u128::from(dur) * u128::from(media_scale) / u128::from(movie_scale);
        return Some((time as u64, u64::try_from(len).ok()?));
    }
    None
}

/// [`mp4_edit`] の読み口の版（M13 段 2 の検証の直し）：moov を写さずに、箱の頭をシークでたどって要る箱（mvhd・trak の
/// mdia の hdlr・mdhd、edts の elst）の頭だけを読む。たどり方・見る箱・読む値は `mp4_edit` と同じなので、moov の大きさに
/// よらず同じ切り取りを返す（上限を置かない。上限で黙って切り取りを捨てると、値が `decode` と食い違うため）。
pub(crate) fn mp4_edit_reader(
    src: &mut dyn ReadSeek,
    rate: u32,
) -> Result<Option<(u64, u64)>, EngineError> {
    let len = src.seek(SeekFrom::End(0))?;
    let Some(moov) = find_box(src, 0, len, b"moov")? else {
        return Ok(None);
    };
    let Some(mvhd) = find_box(src, moov.0, moov.1, b"mvhd")? else {
        return Ok(None);
    };
    let Some(movie_scale) = time_scale(&read_head(src, mvhd, 24)?) else {
        return Ok(None);
    };
    let movie_scale = u64::from(movie_scale);
    let mut traks = BoxWalk::new(moov.0, moov.1);
    while let Some((kind, ts, te)) = traks.next(src)? {
        if &kind != b"trak" {
            continue;
        }
        let Some(mdia) = find_box(src, ts, te, b"mdia")? else {
            continue;
        };
        let Some(hdlr) = find_box(src, mdia.0, mdia.1, b"hdlr")? else {
            continue;
        };
        if read_head(src, hdlr, 12)?.get(8..12) != Some(b"soun") {
            continue;
        }
        let Some(mdhd) = find_box(src, mdia.0, mdia.1, b"mdhd")? else {
            return Ok(None);
        };
        let Some(media_scale) = time_scale(&read_head(src, mdhd, 24)?) else {
            return Ok(None);
        };
        if media_scale != rate || movie_scale == 0 {
            return Ok(None);
        }
        let Some(edts) = find_box(src, ts, te, b"edts")? else {
            return Ok(None);
        };
        let Some(elst) = find_box(src, edts.0, edts.1, b"elst")? else {
            return Ok(None);
        };
        let head = read_head(src, elst, 8 + u64::from(ELST_MAX) * 20)?;
        return Ok(elst_first(&head, media_scale, movie_scale));
    }
    Ok(None)
}

/// 読み取りの予算（マーカーの上限とバイト数）。
pub(crate) struct Budget {
    pub bytes: u64,
}

impl Budget {
    pub fn take(&mut self, len: usize) -> bool {
        let len = len as u64;
        if len > self.bytes {
            self.bytes = 0;
            false
        } else {
            self.bytes -= len;
            true
        }
    }
}

/// 既知のタグの名前の表の 1 行。読み取り（[`comment_name`]）と書き出し（[`vorbis_comments_for`]）が同じ表を使う
/// （読み取りが同じ名前にまとめるコメントを、書き出しが 2 つ書かないように）。
struct KnownTag {
    /// kura の名前（`AudioMeta::tags` のキー）。
    name: &'static str,
    /// Vorbis コメントに書く名前。読み取りは、大文字にした名前がこれと同じコメントをこの名前にする。
    vorbis: &'static str,
    /// 読み取りが同じ名前にする別の Vorbis コメントの名前（大文字にした名前と比べる）。
    aliases: &'static [&'static str],
    /// 読み取りが同じ名前にするキー（MP4 の iTunes のタグの箱の名前。大文字と小文字を区別して比べる。Vorbis コメントにも
    /// 当てるので、小文字の `cprt` という名前のコメントも copyright になる）。
    exact: &'static str,
}

/// 既知のタグの名前（読み取りは上から順に照らし、最初に合った行の名前にする）。
const KNOWN_TAGS: [KnownTag; 7] = [
    KnownTag {
        name: "title",
        vorbis: "TITLE",
        aliases: &[],
        exact: "\u{a9}nam",
    },
    KnownTag {
        name: "artist",
        vorbis: "ARTIST",
        aliases: &[],
        exact: "\u{a9}ART",
    },
    KnownTag {
        name: "album",
        vorbis: "ALBUM",
        aliases: &[],
        exact: "\u{a9}alb",
    },
    KnownTag {
        name: "comment",
        vorbis: "COMMENT",
        aliases: &["DESCRIPTION"],
        exact: "\u{a9}cmt",
    },
    KnownTag {
        name: "date",
        vorbis: "DATE",
        aliases: &[],
        exact: "\u{a9}day",
    },
    KnownTag {
        name: "genre",
        vorbis: "GENRE",
        aliases: &[],
        exact: "\u{a9}gen",
    },
    KnownTag {
        name: "copyright",
        vorbis: "COPYRIGHT",
        aliases: &[],
        exact: "cprt",
    },
];

/// 読み取りがコメントの名前に付ける意味（[`comment_name`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommentName {
    /// ループ点の始まり（LOOPSTART。大文字と小文字を区別しない）。
    LoopStart,
    /// ループ点の長さ（LOOPLENGTH。大文字と小文字を区別しない）。
    LoopLength,
    /// タグ：kura の名前（既知の名前か `vorbis:<大文字の名前>`）。
    Tag(String),
}

/// Vorbis コメントの名前として書ける形（空でなく、0x20〜0x7D の ASCII で `=` を含まない。Vorbis I の規則）。
fn vorbis_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| (0x20..=0x7D).contains(&b) && b != b'=')
}

/// 読み取り（[`meta_from_tags`]）がコメントの名前 `key` をどう扱うか。運ばない名前は None。`vorbis` なら、既知の名前と
/// ループ点の名前のほかも `vorbis:<大文字の名前>` で運ぶ（Vorbis コメントとして書ける形のときだけ）。
pub(crate) fn comment_name(key: &str, vorbis: bool) -> Option<CommentName> {
    let upper = key.to_ascii_uppercase();
    match upper.as_str() {
        "LOOPSTART" => return Some(CommentName::LoopStart),
        "LOOPLENGTH" => return Some(CommentName::LoopLength),
        _ => {}
    }
    if let Some(t) = KNOWN_TAGS
        .iter()
        .find(|t| upper == t.vorbis || t.aliases.contains(&upper.as_str()) || key == t.exact)
    {
        return Some(CommentName::Tag(t.name.to_string()));
    }
    (vorbis && vorbis_name_ok(&upper)).then(|| CommentName::Tag(format!("vorbis:{upper}")))
}

/// 生のタグを kura のメタデータにする（タグとループ点）。`frames` は音声の長さ（ループ点の確かめ）。
/// `vorbis` なら知らないキーも `vorbis:<KEY>` で運ぶ。名前の扱いは [`comment_name`]。
pub(crate) fn meta_from_tags(
    raw: &[(String, String)],
    frames: u64,
    vorbis: bool,
    limits: &Limits,
) -> AudioMeta {
    let mut budget = Budget {
        bytes: limits.max_meta_bytes,
    };
    let mut meta = AudioMeta::default();
    let (mut loop_start, mut loop_len) = (None, None);
    for (key, value) in raw {
        let name = match comment_name(key, vorbis) {
            Some(CommentName::LoopStart) => {
                loop_start = loop_start.or_else(|| value.trim().parse::<u64>().ok());
                continue;
            }
            Some(CommentName::LoopLength) => {
                loop_len = loop_len.or_else(|| value.trim().parse::<u64>().ok());
                continue;
            }
            Some(CommentName::Tag(name)) => name,
            None => continue,
        };
        if meta.tags.contains_key(&name) || value.len() > TEXT_MAX {
            continue;
        }
        if budget.take(name.len() + value.len() + ITEM_COST) {
            meta.tags.insert(name, value.clone());
        }
    }
    if let (Some(s), Some(n)) = (loop_start, loop_len)
        && n > 0
        && let Some(e) = s.checked_add(n)
        && e <= frames
    {
        meta.loop_range = Some(LoopRange { start: s, end: e });
    }
    meta
}

/// 書き出しで運ぶループ点：`start < end ≤ frames` のときだけ（WAV・AIFF の書き出しと同じ。`meta_from_tags` が読み直せる
/// 範囲）。範囲の外・長さ 0・逆向きのループ点は書かない。
pub(crate) fn writable_loop(l: Option<LoopRange>, frames: u64) -> Option<LoopRange> {
    l.filter(|l| l.start < l.end && l.end <= frames)
}

/// FLAC・Ogg に書く Vorbis コメント（タグとループ点）の (キー, 値)。`meta_from_tags` の逆。`frames` は音声の長さ。
///
/// - ループ点は [`writable_loop`] の範囲のときだけ LOOPSTART・LOOPLENGTH（終わり − 始まり）を書く。
/// - タグは既知の名前（title など）を先に、次に `vorbis:<KEY>` を、それぞれ `tags` の順に書く。書くのは、読み取りが
///   タグとして読む名前（[`comment_name`] が `Tag` を返す、Vorbis コメントとして書ける名前）だけ。読み取りが先に書いた
///   項目と同じタグにまとめる名前（大文字と小文字だけが違う名前、DESCRIPTION と COMMENT、`cprt` と COPYRIGHT）は書かない。
///   読み取りは同じ名前の 2 つめ以降を捨てるので、重複を書くと、書いて・読んで・書き直したときにコメントが変わる。
pub(crate) fn vorbis_comments_for(meta: &AudioMeta, frames: u64) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(l) = writable_loop(meta.loop_range, frames) {
        out.push(("LOOPSTART".to_string(), l.start.to_string()));
        out.push(("LOOPLENGTH".to_string(), (l.end - l.start).to_string()));
    }
    // 書いた名前を読み取りが付ける kura の名前（重複を除く）。既知の名前の段を先に回す（tags の順では既知の名前が
    // `vorbis:` より前に来るので、重複のない入力の並びは 1 段で回したときと同じ）
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for known_pass in [true, false] {
        for (key, value) in &meta.tags {
            let name = if known_pass {
                KNOWN_TAGS
                    .iter()
                    .find(|t| t.name == key)
                    .map(|t| t.vorbis.to_string())
            } else {
                key.strip_prefix("vorbis:").map(str::to_string)
            };
            // 書けない名前とループ点の名前は書かない
            let Some(name) = name.filter(|k| vorbis_name_ok(k)) else {
                continue;
            };
            let Some(CommentName::Tag(same)) = comment_name(&name, true) else {
                continue;
            };
            if seen.insert(same) {
                out.push((name, crate::codec::clip_text(value, TEXT_MAX).to_string()));
            }
        }
    }
    out
}

/// AudioSpecificConfig を見て、HE-AAC（SBR・PS）なら true（AAC-LC の読み手は SBR を持たないので、次の段へ回す）。
/// 明示の印だけを見る：オブジェクトの種類 5・29、または GASpecificConfig のあとの同期の拡張（0x2B7・種類 5・
/// sbrPresentFlag = 1）。ffmpeg は「SBR なし」を同期の拡張（sbrPresentFlag = 0）で明示するので、旗まで読む。
/// 帯域の中だけの SBR（暗黙の印。ADTS はいつもこれ）は ASC では見分けられないので、AAC の読み手（`aac::read`）が先頭の
/// パケットを読んで fill 要素の SBR の拡張を見る。チャンネルの構成 0（PCE）は読まない（LC とみなす）。
pub(crate) fn is_he_aac(asc: &[u8]) -> bool {
    let bit = |i: usize| -> Option<u32> {
        let b = asc.get(i / 8)?;
        Some(u32::from((b >> (7 - (i % 8))) & 1))
    };
    let bits = |at: usize, n: usize| -> Option<u32> {
        (0..n).try_fold(0u32, |acc, k| Some((acc << 1) | bit(at + k)?))
    };
    let Some(aot) = bits(0, 5) else {
        return false;
    };
    let (aot, mut at) = if aot == 31 {
        (bits(5, 6).map_or(0, |e| 32 + e), 11)
    } else {
        (aot, 5)
    };
    if matches!(aot, 5 | 29) {
        return true;
    }
    // 周波数の番号（15 なら 24 ビットの値が続く）とチャンネルの構成
    let Some(freq) = bits(at, 4) else {
        return false;
    };
    at += if freq == 15 { 28 } else { 4 };
    let Some(channels) = bits(at, 4) else {
        return false;
    };
    at += 4;
    if aot != 2 || channels == 0 {
        return false;
    }
    // GASpecificConfig：frameLengthFlag・dependsOnCoreCoder（1 なら 14 ビットの遅延）・extensionFlag
    let Some(core) = bits(at + 1, 1) else {
        return false;
    };
    at += 3 + if core == 1 { 14 } else { 0 };
    bits(at, 11) == Some(0x2B7) && bits(at + 11, 5) == Some(5) && bits(at + 16, 1) == Some(1)
}

/// ALAC の magic cookie（`frma`・`alac` の箱の頭が付くこともある）から、元のビットの深さを読む。
pub(crate) fn alac_bits(cookie: &[u8]) -> Option<u8> {
    let mut b = cookie;
    for head in [b"frma", b"alac"] {
        if b.get(4..8) == Some(head.as_slice()) {
            b = b.get(12..)?;
        }
    }
    b.get(5).copied().filter(|d| (1..=32).contains(d))
}

/// iTunSMPB（" 00000000 PPPPPPPP QQQQQQQQ LLLLLLLLLLLLLLLL ..."）→ (priming, 長さ)。
pub(crate) fn parse_itunsmpb(s: &str) -> Option<(u64, u64)> {
    let mut it = s.split_ascii_whitespace();
    let _ = it.next()?;
    let priming = u64::from_str_radix(it.next()?, 16).ok()?;
    let _padding = u64::from_str_radix(it.next()?, 16).ok()?;
    let len = u64::from_str_radix(it.next()?, 16).ok()?;
    Some((priming, len))
}

/// MP4 の箱をたどって、音声のトラックの最初の空でない編集（elst）を (始まり, 長さ)（メディアの時間の単位）で返す。
/// メディアの時間の単位がサンプルレートと違えば None。箱の入れ子は 8 段まで。
pub(crate) fn mp4_edit(data: &[u8], rate: u32) -> Option<(u64, u64)> {
    fn boxes(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + 8 <= b.len() {
            let size = u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
            let kind = [b[at + 4], b[at + 5], b[at + 6], b[at + 7]];
            let (head, size) = if size == 1 && at + 16 <= b.len() {
                let mut v = [0u8; 8];
                v.copy_from_slice(&b[at + 8..at + 16]);
                (
                    16usize,
                    usize::try_from(u64::from_be_bytes(v)).unwrap_or(usize::MAX),
                )
            } else if size == 0 {
                (8, b.len() - at)
            } else {
                (8, size)
            };
            if size < head || at.saturating_add(size) > b.len() {
                break;
            }
            out.push((kind, &b[at + head..at + size]));
            at += size;
        }
        out
    }
    fn find<'b>(b: &'b [u8], k: &[u8; 4]) -> Option<&'b [u8]> {
        boxes(b).into_iter().find(|(t, _)| t == k).map(|(_, v)| v)
    }
    let moov = find(data, b"moov")?;
    let movie_scale = u64::from(time_scale(find(moov, b"mvhd")?)?);
    for (kind, trak) in boxes(moov) {
        if &kind != b"trak" {
            continue;
        }
        let Some(mdia) = find(trak, b"mdia") else {
            continue;
        };
        let Some(hdlr) = find(mdia, b"hdlr") else {
            continue;
        };
        if hdlr.get(8..12) != Some(b"soun") {
            continue;
        }
        let media_scale = time_scale(find(mdia, b"mdhd")?)?;
        if media_scale != rate || movie_scale == 0 {
            return None;
        }
        let elst = find(find(trak, b"edts")?, b"elst")?;
        return elst_first(elst, media_scale, movie_scale);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// M13 段 2：読み口から moov だけを読む編集リストの解析は、全体を読み込んだものと同じ（フィクスチャの m4a と、moov を
    /// mdat の後ろに置いたもの・壊れた箱で止まるもの・大きさ 0 の箱）。
    #[test]
    fn mp4_edit_reader_equals_mp4_edit() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut seen_edit = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|x| x.to_str()) != Some("m4a") {
                continue;
            }
            let data = std::fs::read(&p).unwrap();
            for rate in [44_100, 48_000, 8_000] {
                let want = mp4_edit(&data, rate);
                let got = mp4_edit_reader(&mut Cursor::new(&data), rate).unwrap();
                assert_eq!(got, want, "{}・{rate}", p.display());
                seen_edit += usize::from(want.is_some());
            }
            // moov を後ろへ回す（最上位の箱の並びを入れ替える）・後ろに壊れた箱を足す・大きさ 0 の箱を足す
            let mut boxes = Vec::new();
            let mut at = 0usize;
            while at + 8 <= data.len() {
                let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
                if size < 8 || at + size > data.len() {
                    break;
                }
                boxes.push(data[at..at + size].to_vec());
                at += size;
            }
            boxes.reverse();
            let moved: Vec<u8> = boxes.concat();
            let mut tails = vec![moved.clone()];
            let mut bad = moved.clone();
            bad.extend_from_slice(&[0, 0, 0, 3, b'f', b'r', b'e', b'e']);
            tails.push(bad);
            let mut zero = vec![0, 0, 0, 0, b'f', b'r', b'e', b'e', 1, 2, 3];
            zero.extend_from_slice(&moved);
            tails.push(zero);
            // moov の中身の後ろに 65 MiB の free を足す（前の版は 64 MiB を超える moov を黙って読まなかった。M13 段 2 の検証）
            if mp4_edit(&data, 44_100).is_some() {
                tails.push(with_big_moov(&moved, 65 << 20));
            }
            for d in tails {
                for rate in [44_100, 48_000] {
                    assert_eq!(
                        mp4_edit_reader(&mut Cursor::new(&d), rate).unwrap(),
                        mp4_edit(&d, rate),
                        "{}",
                        p.display()
                    );
                }
            }
        }
        assert!(seen_edit >= 1, "{seen_edit}");
    }

    /// 最上位の箱を並べた `data` の moov の中身の後ろに、`pad` バイトの free の箱を足す（moov の大きさを直し、ほかの箱は
    /// そのまま）。
    fn with_big_moov(data: &[u8], pad: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + 8 <= data.len() {
            let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            let b = &data[at..at + size];
            if &b[4..8] == b"moov" {
                let grown = u32::try_from(size + 8 + pad).unwrap();
                out.extend_from_slice(&grown.to_be_bytes());
                out.extend_from_slice(&b[4..]);
                out.extend_from_slice(&u32::try_from(8 + pad).unwrap().to_be_bytes());
                out.extend_from_slice(b"free");
                out.resize(out.len() + pad, 0);
            } else {
                out.extend_from_slice(b);
            }
            at += size;
        }
        out
    }

    /// NaN はどのビット列でも `0x7fc00000` にそろえ（符号・ペイロードを捨てる）、ほかの値（-0・±∞・非正規化数）は
    /// ビット単位でそのまま。
    #[test]
    fn canonical_nan_keeps_everything_but_nan() {
        for bits in [
            0x7fc0_0000u32,
            0xffc0_0000,
            0x7f80_0001,
            0xff80_0001,
            0x7fff_ffff,
            0xffff_ffff,
        ] {
            assert_eq!(
                canonical_nan(f32::from_bits(bits)).to_bits(),
                0x7fc0_0000,
                "{bits:#x}"
            );
        }
        for bits in [
            0u32,
            0x8000_0000,
            0x7f80_0000,
            0xff80_0000,
            1,
            0x8000_0001,
            0x3f80_0000,
            0xbf7f_ffff,
        ] {
            assert_eq!(
                canonical_nan(f32::from_bits(bits)).to_bits(),
                bits,
                "{bits:#x}"
            );
        }
    }

    #[test]
    fn itunsmpb_and_asc() {
        assert_eq!(
            parse_itunsmpb(" 00000000 00000840 000000DD 00000000000166E3 00000000"),
            Some((2_112, 91_875))
        );
        assert_eq!(parse_itunsmpb("x"), None);
        // AAC-LC 44.1k ステレオ（0x12 0x10）は HE ではない。オブジェクトの種類 5 は HE
        assert!(!is_he_aac(&[0x12, 0x10]));
        assert!(is_he_aac(&[0x2B, 0x92, 0x08, 0x00]));
        // 同期の拡張：afconvert の HE-AAC（sbrPresentFlag = 1）と、ffmpeg の LC（SBR なしの明示）
        assert!(is_he_aac(&[0x13, 0x90, 0x56, 0xE5, 0xA0]));
        assert!(!is_he_aac(&[0x12, 0x10, 0x56, 0xE5, 0x00]));
        assert!(!is_he_aac(&[]));
        // 暗黙の SBR（ADTS から写した m4a の ASC：LC・22,050 Hz・2 チャンネルだけ）は ASC では見分けられない
        // （自作の AAC の読み手が帯域の中の fill 要素を見る。`aac::decoder`）
        assert!(!is_he_aac(&[0x13, 0x90]));
        assert_eq!(alac_bits(&[0, 0, 16, 0, 0, 24, 0]), Some(24));
        let mut boxed = b"\0\0\0\x0cfrma\0\0\0\0\0\0\0\x24alac\0\0\0\0".to_vec();
        boxed.extend_from_slice(&[0, 0, 16, 0, 0, 20, 0]);
        assert_eq!(alac_bits(&boxed), Some(20));
        assert_eq!(alac_bits(&[0, 0, 16, 0, 0, 0]), None);
    }

    #[test]
    fn tags_and_loops_round_trip_through_vorbis_comments() {
        let raw = vec![
            ("LOOPSTART".to_string(), "100".to_string()),
            ("looplength".to_string(), "50".to_string()),
            ("TITLE".to_string(), "曲".to_string()),
            ("ENCODER".to_string(), "x".to_string()),
            ("bad=key".to_string(), "x".to_string()),
        ];
        let meta = meta_from_tags(&raw, 1_000, true, &Limits::default());
        assert_eq!(
            meta.loop_range,
            Some(LoopRange {
                start: 100,
                end: 150
            })
        );
        assert_eq!(meta.tags.get("title").map(String::as_str), Some("曲"));
        assert_eq!(
            meta.tags.get("vorbis:ENCODER").map(String::as_str),
            Some("x")
        );
        assert_eq!(meta.tags.len(), 2);
        let back = meta_from_tags(
            &vorbis_comments_for(&meta, 1_000),
            1_000,
            true,
            &Limits::default(),
        );
        assert_eq!(back, meta);
        // 長さを超えるループは読まない
        assert_eq!(
            meta_from_tags(&raw, 120, true, &Limits::default()).loop_range,
            None
        );
    }

    /// 読み取りの名前の扱い（`comment_name`）：既知の名前と別名（DESCRIPTION）は大文字と小文字を区別しない。MP4 の鍵
    /// （`©nam` など・`cprt`）は大文字と小文字を区別して、ちょうど同じときだけ（Vorbis コメントにも当てる）。ほかは
    /// `vorbis` のときだけ `vorbis:<大文字>`（書ける形のときだけ）。書き出しは既知の名前を表の名前で書き、読み取りはそれを
    /// 同じ名前に読む。
    #[test]
    fn comment_names_follow_one_table() {
        use CommentName::{LoopLength, LoopStart, Tag};
        let tag = |s: &str| Some(Tag(s.to_string()));
        for (key, vorbis, want) in [
            ("TITLE", true, tag("title")),
            ("title", true, tag("title")),
            ("\u{a9}nam", false, tag("title")),
            ("\u{a9}NAM", true, None),
            ("\u{a9}ART", false, tag("artist")),
            ("Album", true, tag("album")),
            ("description", true, tag("comment")),
            ("DESCRIPTION", false, tag("comment")),
            ("\u{a9}cmt", false, tag("comment")),
            ("date", true, tag("date")),
            ("\u{a9}gen", true, tag("genre")),
            ("copyright", true, tag("copyright")),
            ("cprt", true, tag("copyright")),
            ("cprt", false, tag("copyright")),
            ("CPRT", true, tag("vorbis:CPRT")),
            ("Cprt", true, tag("vorbis:CPRT")),
            ("CPRT", false, None),
            ("LoopStart", true, Some(LoopStart)),
            ("LOOPLENGTH", false, Some(LoopLength)),
            ("foo", true, tag("vorbis:FOO")),
            ("A B", true, tag("vorbis:A B")),
            ("foo", false, None),
            ("a=b", true, None),
            ("", true, None),
            ("~", true, None),
        ] {
            assert_eq!(comment_name(key, vorbis), want, "{key:?} {vorbis}");
        }
        for t in &KNOWN_TAGS {
            let meta = AudioMeta {
                tags: [(t.name.to_string(), "v".to_string())]
                    .into_iter()
                    .collect(),
                ..AudioMeta::default()
            };
            let written = vorbis_comments_for(&meta, 0);
            assert_eq!(written, vec![(t.vorbis.to_string(), "v".to_string())]);
            assert_eq!(meta_from_tags(&written, 0, true, &Limits::default()), meta);
        }
    }

    fn with_loop(start: u64, end: u64) -> AudioMeta {
        AudioMeta {
            loop_range: Some(LoopRange { start, end }),
            ..AudioMeta::default()
        }
    }

    /// ループ点は `start < end ≤ frames` のときだけ書く（WAV・AIFF と同じ）。逆向きでも引き算があふれない。
    #[test]
    fn loop_points_are_written_only_in_range() {
        let frames = 30_000;
        let pair = |s: &str, n: &str| {
            vec![
                ("LOOPSTART".to_string(), s.to_string()),
                ("LOOPLENGTH".to_string(), n.to_string()),
            ]
        };
        for (s, e) in [(0, 30_000), (0, 1), (29_999, 30_000), (100, 200)] {
            let got = vorbis_comments_for(&with_loop(s, e), frames);
            assert_eq!(got, pair(&s.to_string(), &(e - s).to_string()), "{s}..{e}");
            // 読み直すと同じループ点
            let back = meta_from_tags(&got, frames, true, &Limits::default());
            assert_eq!(back.loop_range, Some(LoopRange { start: s, end: e }));
        }
        for (s, e) in [
            (30_000, 30_005),
            (100, 30_100),
            (0, u64::MAX),
            (u64::MAX, u64::MAX),
        ] {
            assert_eq!(
                vorbis_comments_for(&with_loop(s, e), frames),
                vec![],
                "{s}..{e}"
            );
            assert_eq!(
                writable_loop(Some(LoopRange { start: s, end: e }), frames),
                None
            );
        }
        for (s, e) in [(5, 5), (10, 5), (200, 100), (u64::MAX, 0), (0, 0)] {
            assert_eq!(
                vorbis_comments_for(&with_loop(s, e), frames),
                vec![],
                "{s}..{e}"
            );
        }
        assert_eq!(writable_loop(None, frames), None);
    }

    /// 読み取りが同じタグにまとめる名前の重複は先の 1 つだけ（既知の名前を優先し、大文字と小文字だけの違い・DESCRIPTION と
    /// COMMENT・小文字ちょうどの `cprt` と COPYRIGHT を同じとみなす）。読み取りが別の名前にするもの（`CPRT` と `cprt`）は
    /// 両方書く。読み取りが作る形（既知の名前は小文字、ほかは大文字の `vorbis:KEY`）は、重複を除く前と同じ並び。
    #[test]
    fn duplicate_tag_names_are_written_once() {
        let tags = |kv: &[(&str, &str)]| AudioMeta {
            tags: kv
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            ..AudioMeta::default()
        };
        let kv = |list: Vec<(String, String)>| -> Vec<String> {
            list.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
        };
        for (input, want) in [
            (
                vec![("artist", "A"), ("vorbis:ARTIST", "B")],
                vec!["ARTIST=A"],
            ),
            (
                vec![("vorbis:foo", "x"), ("vorbis:FOO", "y")],
                vec!["FOO=y"],
            ),
            (
                vec![("title", "T"), ("vorbis:title", "t"), ("vorbis:Title", "u")],
                vec!["TITLE=T"],
            ),
            // vorbis:TITLE だけなら TITLE として書く（読み直すと title）
            (vec![("vorbis:TITLE", "t")], vec!["TITLE=t"]),
            (
                vec![("comment", "c"), ("vorbis:DESCRIPTION", "d")],
                vec!["COMMENT=c"],
            ),
            (
                vec![("vorbis:COMMENT", "c"), ("vorbis:description", "d")],
                vec!["COMMENT=c"],
            ),
            // 小文字ちょうどの cprt は、読み取りが copyright にする（MP4 の鍵と同じ照らし方）
            (
                vec![("copyright", "X"), ("vorbis:cprt", "Y")],
                vec!["COPYRIGHT=X"],
            ),
            (
                vec![("vorbis:COPYRIGHT", "X"), ("vorbis:cprt", "Y")],
                vec!["COPYRIGHT=X"],
            ),
            // 大文字の CPRT は読み取りが vorbis:CPRT にするので、cprt とは別の名前（両方書く）
            (
                vec![("vorbis:CPRT", "Z"), ("vorbis:cprt", "Y")],
                vec!["CPRT=Z", "cprt=Y"],
            ),
            (
                vec![
                    ("copyright", "X"),
                    ("vorbis:CPRT", "Z"),
                    ("vorbis:cprt", "Y"),
                ],
                vec!["COPYRIGHT=X", "CPRT=Z"],
            ),
            // 読み取りが作る形はそのまま（既知の名前のあとに vorbis:KEY、tags の順）
            (
                vec![
                    ("title", "曲"),
                    ("artist", "誰か"),
                    ("vorbis:ZZZ", "z"),
                    ("vorbis:ENCODER", "e"),
                    ("vorbis:A B", "sp"),
                    ("vorbis:K", "a=b"),
                ],
                vec![
                    "ARTIST=誰か",
                    "TITLE=曲",
                    "A B=sp",
                    "ENCODER=e",
                    "K=a=b",
                    "ZZZ=z",
                ],
            ),
        ] {
            assert_eq!(kv(vorbis_comments_for(&tags(&input), 0)), want, "{input:?}");
        }
    }

    /// 書いて・読んで・書き直すと同じコメント（ChaCha8 で選んだ名前の組。重複・別名・範囲の外を含むループ点）。読み取りが
    /// 作らない形のうち、名前が変わるものは組に入れない：小文字の `vorbis:foo` は大文字の `vorbis:FOO` がある組だけ（`foo`
    /// だけなら読み直すと `FOO`）、既知の名前の別名（`vorbis:TITLE`・`vorbis:DESCRIPTION`・小文字ちょうどの `vorbis:cprt`
    /// など）は既知の名前がある組だけ（別名だけなら読み直すと既知の名前になり、並びと DESCRIPTION・cprt の名前が変わる）。
    /// どれも値は保たれる。
    #[test]
    fn comments_are_stable_through_a_round_trip() {
        use rand_chacha::rand_core::Rng;
        let names = [
            "title",
            "artist",
            "comment",
            "date",
            "copyright",
            "vorbis:TITLE",
            "vorbis:ARTIST",
            "vorbis:DESCRIPTION",
            "vorbis:COMMENT",
            "vorbis:COPYRIGHT",
            "vorbis:CPRT",
            "vorbis:cprt",
            "vorbis:FOO",
            "vorbis:BAR",
            "vorbis:ENCODER",
            "vorbis:LOOPSTART",
            "vorbis:A=B",
            "vorbis:",
            "riff:ISBJ",
        ];
        let values = ["", "x", "a=b=c", "効果音", " sp "];
        let mut r = crate::determinism::rng(0x76);
        for case in 0..500 {
            let mut meta = AudioMeta::default();
            for _ in 0..(r.next_u32() % 6) {
                let name = names[r.next_u32() as usize % names.len()];
                let value = values[r.next_u32() as usize % values.len()];
                meta.tags.insert(name.to_string(), value.to_string());
                // 大文字の項目があるときだけ、小文字の別名を足す
                if r.next_u32().is_multiple_of(3)
                    && let Some(key) = name.strip_prefix("vorbis:")
                    && !key.is_empty()
                    && !matches!(key, "DESCRIPTION")
                {
                    meta.tags
                        .insert(format!("vorbis:{}", key.to_ascii_lowercase()), "low".into());
                }
            }
            // 既知の名前の別名（vorbis:TITLE・vorbis:DESCRIPTION・vorbis:cprt など）は、既知の名前があるときだけ残す（別名
            // だけだと、読み直すと既知の名前になり、書き直すと並び（と DESCRIPTION・cprt の名前）が変わる。値は保たれる）
            let has = meta.tags.clone();
            meta.tags.retain(|k, _| {
                let Some(key) = k.strip_prefix("vorbis:") else {
                    return true;
                };
                let known = match (key.to_ascii_uppercase().as_str(), key) {
                    ("TITLE", _) => "title",
                    ("ARTIST", _) => "artist",
                    ("COMMENT" | "DESCRIPTION", _) => "comment",
                    ("DATE", _) => "date",
                    ("COPYRIGHT", _) | (_, "cprt") => "copyright",
                    _ => return true,
                };
                has.contains_key(known)
            });
            let frames = u64::from(r.next_u32() % 100);
            let (s, e) = (u64::from(r.next_u32() % 120), u64::from(r.next_u32() % 120));
            meta.loop_range = r
                .next_u32()
                .is_multiple_of(2)
                .then_some(LoopRange { start: s, end: e });
            let first = vorbis_comments_for(&meta, frames);
            let back = meta_from_tags(&first, frames, true, &Limits::default());
            let again = vorbis_comments_for(&back, frames);
            assert_eq!(again, first, "{case}: {meta:?}");
            assert_eq!(
                back.loop_range,
                writable_loop(meta.loop_range, frames),
                "{case}"
            );
        }
    }
}
