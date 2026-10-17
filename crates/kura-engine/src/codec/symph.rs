//! symphonia を使う読み取りの共通の部品（FLAC・ALAC・AAC・Vorbis。docs/editing.md §9.2）。
//!
//! - 部品のクレートを `=0.6.1` で直接使い、Probe は通さない（形式の表で読み口とデコーダを選ぶ）。既定の feature
//!   （opt-simd・id3v2）は入れない（Cargo.toml と `scripts/check-engine-features.sh`）。
//! - ファイルは読み込んでから渡す（大きさは `max_value_bytes` まで）。読み口は `read` のたびに中止を見て、パケットごとにも
//!   中止を確かめる。symphonia の panic は `codec::contain` が受け止める。
//! - 長さ：FLAC は STREAMINFO の総フレーム数（0 ならパケットの長さの和）、ALAC・Vorbis は、デコードせずにパケットの
//!   長さ（`dur`。遅延・詰め物を除いた数）を足した値を形にし、デコードした長さがそれと違えば壊れたファイルとして扱う
//!   （形と中身が必ず一致する。形より長く出てきたものも `Decode`）。AAC は priming を自前で切るので（`lossy.rs`）、
//!   形は上限（`AtMost`）。壊れたパケットは飛ばさずエラーにする（長さが形と食い違わないように）。
//! - 標本：FLAC・ALAC は 32 ビットに左詰めした整数を `pcm::int_to_f32(v, 32)`（元の幅の式と同じ値。WAV・AIFF と一致）、
//!   AAC・Vorbis は f32 のまま。
//! - タグ：Vorbis コメント（FLAC・Ogg）と MP4 の iTunes のタグから、title・artist・album・comment・date・genre・
//!   copyright を読み、FLAC・Ogg のほかのキーは `vorbis:<KEY>` のまま運ぶ（FLAC に書き戻せる）。
//!   `LOOPSTART`・`LOOPLENGTH`（Vorbis コメント）をループ点にする（終わり = LOOPSTART + LOOPLENGTH。含まない）。
//! - 書き出し（FLAC・Ogg の Vorbis コメント。[`vorbis_comments_for`]）は読み取りの逆：ループ点は `start < end ≤ 長さ` の
//!   ときだけ書き、読み取りが同じタグにまとめる名前（大文字と小文字だけの違い・DESCRIPTION と COMMENT・`cprt` と
//!   COPYRIGHT）が重なれば先の 1 つだけを書く（既知の名前を優先）。名前の扱いは読み取りと書き出しで 1 つの表
//!   （`KNOWN_TAGS`・[`comment_name`]）から決める。

use std::collections::BTreeSet;
use std::io::{Cursor, Read, Seek, SeekFrom};

use symphonia_core::audio::{Audio as _, GenericAudioBufferRef};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::errors::Error as SymError;
use symphonia_core::formats::{FormatOptions, FormatReader, Track, TrackType};
use symphonia_core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia_core::meta::RawValue;

use crate::codec::{ReadSeek, pcm, try_vec};
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::value::{AudioMeta, LoopRange};

/// 名前・タグとして読む 1 つの文字列の長さの上限（バイト）。
const TEXT_MAX: usize = 4_096;
/// メタデータの 1 項目ごとに予算から引く固定の重さ（バイト。WAV と同じ）。
const ITEM_COST: usize = 32;

/// 入れ物の形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Container {
    Flac,
    Ogg,
    Mp4,
    Adts,
}

/// 中止を見る読み口（読み込んだバイト列を包む）。
struct Src<'a> {
    cur: Cursor<&'a [u8]>,
    mon: &'a dyn Monitor,
}

impl Read for Src<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.mon.canceled() {
            // Interrupted は read_exact が再試行するので使わない
            return Err(std::io::Error::other("kura: canceled"));
        }
        self.cur.read(buf)
    }
}

impl Seek for Src<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.cur.seek(pos)
    }
}

impl MediaSource for Src<'_> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.cur.get_ref().len() as u64)
    }
}

/// symphonia の誤りを直す（中止を頼まれていれば `Canceled`。扱えない形式は `Unsupported` で次の段へ）。
pub(crate) fn sym_err(id: &str, e: SymError, mon: &dyn Monitor) -> EngineError {
    if mon.canceled() {
        return EngineError::Canceled;
    }
    match e {
        SymError::Unsupported(m) => EngineError::Unsupported(format!("{id}: {m}")),
        other => EngineError::Decode {
            codec: id.to_string(),
            message: other.to_string(),
        },
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

/// 開いた読み口と、音声のトラック。
pub(crate) struct Opened<'a> {
    pub reader: Box<dyn FormatReader + 'a>,
    pub track: Track,
    pub params: AudioCodecParameters,
    pub rate: u32,
    pub channels: usize,
}

/// 読み口を開き、最初の音声のトラックを選ぶ。
pub(crate) fn open<'a>(
    id: &str,
    data: &'a [u8],
    c: Container,
    mon: &'a dyn Monitor,
    limits: &Limits,
) -> Result<Opened<'a>, EngineError> {
    let mss = MediaSourceStream::new(
        Box::new(Src {
            cur: Cursor::new(data),
            mon,
        }),
        MediaSourceStreamOptions::default(),
    );
    let o = FormatOptions::default();
    let e = |e| sym_err(id, e, mon);
    let reader: Box<dyn FormatReader + 'a> = match c {
        Container::Flac => Box::new(symphonia_bundle_flac::FlacReader::try_new(mss, o).map_err(e)?),
        Container::Ogg => Box::new(symphonia_format_ogg::OggReader::try_new(mss, o).map_err(e)?),
        Container::Mp4 => {
            Box::new(symphonia_format_isomp4::IsoMp4Reader::try_new(mss, o).map_err(e)?)
        }
        Container::Adts => Box::new(symphonia_codec_aac::AdtsReader::try_new(mss, o).map_err(e)?),
    };
    let track = reader
        .default_track(TrackType::Audio)
        .cloned()
        .ok_or_else(|| EngineError::Unsupported(format!("{id}: 音声のトラックがありません")))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(CodecParameters::audio)
        .cloned()
        .ok_or_else(|| EngineError::Unsupported(format!("{id}: 音声のトラックがありません")))?;
    let rate = params
        .sample_rate
        .filter(|r| *r > 0)
        .ok_or_else(|| EngineError::Decode {
            codec: id.to_string(),
            message: "サンプルレートがありません".into(),
        })?;
    let channels = params
        .channels
        .as_ref()
        .map(|c| c.count())
        .filter(|n| *n > 0)
        .ok_or_else(|| EngineError::Decode {
            codec: id.to_string(),
            message: "チャンネルの数がありません".into(),
        })?;
    if channels as u64 > limits.max_channels {
        return Err(EngineError::Limit("チャンネルが多すぎます".into()));
    }
    if rate > limits.max_sample_rate {
        return Err(EngineError::Limit("サンプルレートが大きすぎます".into()));
    }
    Ok(Opened {
        reader,
        track,
        params,
        rate,
        channels,
    })
}

impl Opened<'_> {
    /// デコードせずに、パケットの長さ（遅延・詰め物を除く）を足す。切り取る前の長さ（デコーダが実際に作る量）の和も
    /// `max_audio_frames`・`max_value_bytes` で確かめる（ページのグラニュールで長さを 0 近くまで詰めると、形は小さい
    /// まま、大量のパケットのデコードを通せてしまうため。検証で見つけた）。
    pub fn count_frames(
        &mut self,
        id: &str,
        mon: &dyn Monitor,
        limits: &Limits,
    ) -> Result<u64, EngineError> {
        let mut total = 0u64;
        let mut decoded = 0u64;
        loop {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let Some(pkt) = self.reader.next_packet().map_err(|e| sym_err(id, e, mon))? else {
                break;
            };
            if pkt.track_id == self.track.id {
                total = total.saturating_add(pkt.dur.get());
                decoded = decoded
                    .saturating_add(pkt.dur.get())
                    .saturating_add(pkt.trim_start.get())
                    .saturating_add(pkt.trim_end.get());
            }
        }
        let channels = u16::try_from(self.channels).unwrap_or(u16::MAX);
        limits
            .check(&crate::shape::Shape::Audio {
                rate: self.rate,
                channels,
                frames: crate::shape::Dim::Exact(decoded),
            })
            .map_err(|m| EngineError::Limit(format!("デコードする量が大きすぎます（{m}）")))?;
        Ok(total)
    }

    /// 生のタグ（キー, 文字列）。全体とトラックごとのものを、出た順に。
    pub fn raw_tags(&mut self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(rev) = self.reader.metadata().skip_to_latest() {
            let all = rev
                .media
                .tags
                .iter()
                .chain(rev.per_track.iter().flat_map(|t| t.metadata.tags.iter()));
            for tag in all {
                if let RawValue::String(s) = &tag.raw.value {
                    out.push((tag.raw.key.clone(), s.as_ref().clone()));
                }
            }
        }
        out
    }

    /// すべてのパケットをデコードして、planar の f32 にする（長さは `max_frames` まで）。
    pub fn decode_all(
        &mut self,
        id: &str,
        dec: &mut dyn AudioDecoder,
        max_frames: u64,
        mon: &dyn Monitor,
    ) -> Result<Vec<Vec<f32>>, EngineError> {
        let mut planes: Vec<Vec<f32>> = (0..self.channels).map(|_| Vec::new()).collect();
        let mut total = 0u64;
        loop {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let Some(pkt) = self.reader.next_packet().map_err(|e| sym_err(id, e, mon))? else {
                break;
            };
            if pkt.track_id != self.track.id {
                continue;
            }
            let buf = dec.decode(&pkt).map_err(|e| sym_err(id, e, mon))?;
            let n = buf.frames();
            total = total.saturating_add(n as u64);
            if total > max_frames {
                // 上限は形（ヘッダ）から決めたものなので、超えるのはヘッダと中身の食い違い
                return Err(EngineError::Decode {
                    codec: id.to_string(),
                    message: "長さがヘッダより長い".into(),
                });
            }
            for (ch, out) in planes.iter_mut().enumerate() {
                out.try_reserve(n)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
                let missing = || EngineError::Decode {
                    codec: id.to_string(),
                    message: "チャンネルの数が途中で変わりました".into(),
                };
                match &buf {
                    GenericAudioBufferRef::S32(b) => {
                        let src = b.plane(ch).ok_or_else(missing)?;
                        out.extend(src.iter().map(|s| pcm::int_to_f32(*s, 32)));
                    }
                    GenericAudioBufferRef::F32(b) => {
                        out.extend_from_slice(b.plane(ch).ok_or_else(missing)?);
                    }
                    _ => {
                        return Err(EngineError::Unsupported(format!(
                            "{id}: 標本の形式を扱えません"
                        )));
                    }
                }
            }
        }
        Ok(planes)
    }
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

/// AAC のデコーダの設定（gapless：MP3・Vorbis の切り取りは読み口が付ける。AAC は自前で切る）。
pub(crate) fn decoder_options() -> AudioDecoderOptions {
    AudioDecoderOptions::default()
}

/// AudioSpecificConfig を見て、HE-AAC（SBR・PS）なら true（symphonia は半分のレートで読んでしまう。次の段へ回す）。
/// 明示の印だけを見る：オブジェクトの種類 5・29、または GASpecificConfig のあとの同期の拡張（0x2B7・種類 5・
/// sbrPresentFlag = 1）。ffmpeg は「SBR なし」を同期の拡張（sbrPresentFlag = 0）で明示するので、旗まで読む。
/// 帯域の中だけの SBR（暗黙の印）は外から見分けられない。チャンネルの構成 0（PCE）は読まない（LC とみなす）。
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
    let mvhd = find(moov, b"mvhd")?;
    let movie_scale = u64::from(match mvhd.first()? {
        1 => u32::from_be_bytes(mvhd.get(20..24)?.try_into().ok()?),
        _ => u32::from_be_bytes(mvhd.get(12..16)?.try_into().ok()?),
    });
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
        let mdhd = find(mdia, b"mdhd")?;
        let media_scale = match mdhd.first()? {
            1 => u32::from_be_bytes(mdhd.get(20..24)?.try_into().ok()?),
            _ => u32::from_be_bytes(mdhd.get(12..16)?.try_into().ok()?),
        };
        if media_scale != rate || movie_scale == 0 {
            return None;
        }
        let elst = find(find(trak, b"edts")?, b"elst")?;
        let version = *elst.first()?;
        let count = u32::from_be_bytes(elst.get(4..8)?.try_into().ok()?);
        let mut at = 8usize;
        for _ in 0..count.min(64) {
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
        return None;
    }
    None
}

/// Vorbis の setup ヘッダの codebook が symphonia（`symphonia-codec-vorbis` 0.6.1）の中で確保する大きさ（バイト）の
/// 上からの見積もり。小さなファイルが数 GB を確保させられる（VQ の表と探索表）ので、デコーダに渡す前に断るために使う。
/// `extra` は Ogg の読み口が作る「識別ヘッダ（30 バイト）＋ setup ヘッダ」。読めないもの・symphonia が確保の前に断る
/// もの（次元 0・33 以上、項目 131,073 以上、長さ 33 以上の符号語）は None。
///
/// codebook 1 つあたり（symphonia の `VorbisCodebook::read` と `CodebookBuilder::make` に合わせる）：
/// - VQ の表：lookup 1・2 は 項目 × 次元 × 4（f32）と、読んだ値 × 4（u16 の `Vec` の伸びを見込む）。
/// - 項目ごと：長さ・値の配列は使わない項目の分も先に確保するので 16 バイト、使う符号語ごとに符号と木の葉
///   （`CodebookValue`）で 32 バイトとみなす。
/// - 探索表：B = clamp(最長, 4, 8) ビットずつ読むブロックの木。ブロックは根と、深さ d = kB（d < 最長）の節のうち
///   もっと長い符号語の接頭辞になるもの。symphonia は完全な木（クラフトの等式）でなければ表を作る前に断るので、
///   その数はちょうど 2^d − Σ_{長さ ≤ d} 数 · 2^(d − 長さ)（d より長い符号語の数も超えない）。表はブロックごとに
///   2^B 個 × 8 バイトで、`Vec` の伸び（最大 2 倍）を見込んで 16 バイトとみなし、ブロックの付帯分を 128 バイトとみなす。
pub(crate) fn vorbis_setup_bytes(extra: &[u8]) -> Option<u64> {
    let setup = extra.get(30..)?;
    if setup.get(..7)? != b"\x05vorbis" {
        return None;
    }
    let mut r = Bits {
        b: setup,
        at: 7 * 8,
    };
    let books = r.take(8)? + 1;
    let mut total = 0u64;
    for _ in 0..books {
        if r.take(24)? != 0x56_4342 {
            return None;
        }
        let dims = r.take(16)?;
        let entries = r.take(24)?;
        if dims == 0 || dims > 32 || entries > 128 * 1024 {
            return None;
        }
        // 長さごとの符号語の数（長さ 1〜32）
        let mut count = [0u64; 33];
        let mut put = |len: u32, n: u64| -> Option<()> {
            if n > 0 {
                *count.get_mut(usize::try_from(len).ok()?)? += n;
            }
            Some(())
        };
        if r.take(1)? == 1 {
            // ordered：長さの順に、その長さの数を並べる
            let mut cur = 0u64;
            let mut len = r.take(5)? + 1;
            while cur < u64::from(entries) {
                let n = r.take(ilog(entries - cur as u32))?;
                put(len, u64::from(n))?;
                cur = cur.checked_add(u64::from(n))?;
                len = len.checked_add(1)?;
            }
            if cur > u64::from(entries) {
                return None;
            }
        } else {
            let sparse = r.take(1)? == 1;
            for _ in 0..entries {
                if !sparse || r.take(1)? == 1 {
                    put(r.take(5)? + 1, 1)?;
                }
            }
        }
        let codes: u64 = count.iter().sum();
        // 1 つだけの長さ 1 の符号語は、symphonia が 2 つにして完全な木にする
        if codes == 1 && count[1] == 1 {
            count[1] = 2;
        }
        let lookup = r.take(4)?;
        let vq = match lookup {
            0 => 0u64,
            1 | 2 => {
                r.take(32)?;
                r.take(32)?;
                let value_bits = r.take(4)? + 1;
                r.take(1)?;
                let n = if lookup == 1 {
                    lookup1_values(entries, dims)?
                } else {
                    u64::from(entries).checked_mul(u64::from(dims))?
                };
                // 値を読み飛ばす
                let skip = n.checked_mul(u64::from(value_bits))?;
                r.at = r.at.checked_add(usize::try_from(skip).ok()?)?;
                if r.at > r.b.len() * 8 {
                    return None;
                }
                (u64::from(entries) * u64::from(dims) * 4).saturating_add(n.saturating_mul(4))
            }
            _ => return None,
        };
        total = total
            .saturating_add(vq)
            .saturating_add(u64::from(entries) * 16)
            .saturating_add(codes.saturating_mul(32))
            .saturating_add(vorbis_lut_bytes(&count));
    }
    Some(total)
}

/// symphonia の探索表（`CodebookBuilder::make`）の大きさの上からの見積もり（`vorbis_setup_bytes` の式）。
fn vorbis_lut_bytes(count: &[u64; 33]) -> u64 {
    let Some(longest) = (1..=32u32).rev().find(|&l| count[l as usize] > 0) else {
        return 0;
    };
    let b = longest.clamp(4, 8);
    let mut blocks = 1u128;
    let mut d = b;
    while d < longest {
        // 深さ d の節のうち、もっと長い符号語の接頭辞になるもの
        let used: u128 = (1..=d)
            .map(|l| u128::from(count[l as usize]) << (d - l))
            .sum();
        let longer: u128 = (d + 1..=32).map(|l| u128::from(count[l as usize])).sum();
        let open = (1u128 << d).checked_sub(used).unwrap_or(longer);
        blocks += open.min(longer);
        d += b;
    }
    let per_block = (1u128 << b) * 16 + 128;
    u64::try_from(blocks * per_block).unwrap_or(u64::MAX)
}

/// lookup1_values：value^dims ≤ entries となる最大の value（整数だけ）。
fn lookup1_values(entries: u32, dims: u32) -> Option<u64> {
    if dims == 0 {
        return None;
    }
    let (mut lo, mut hi) = (0u64, u64::from(entries));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let fits = (0..dims)
            .try_fold(1u64, |acc, _| acc.checked_mul(mid))
            .is_some_and(|p| p <= u64::from(entries));
        if fits {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Some(lo)
}

fn ilog(v: u32) -> usize {
    (32 - v.leading_zeros()) as usize
}

/// Vorbis のビットの読み手（下位のビットから）。
struct Bits<'a> {
    b: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for k in 0..n {
            let byte = *self.b.get(self.at / 8)?;
            let bit = u32::from((byte >> (self.at % 8)) & 1);
            v |= bit << k;
            self.at += 1;
        }
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(alac_bits(&[0, 0, 16, 0, 0, 24, 0]), Some(24));
        let mut boxed = b"\0\0\0\x0cfrma\0\0\0\0\0\0\0\x24alac\0\0\0\0".to_vec();
        boxed.extend_from_slice(&[0, 0, 16, 0, 0, 20, 0]);
        assert_eq!(alac_bits(&boxed), Some(20));
        assert_eq!(alac_bits(&[0, 0, 16, 0, 0, 0]), None);
        assert_eq!(lookup1_values(100, 2), Some(10));
        assert_eq!(lookup1_values(99, 2), Some(9));
        assert_eq!(lookup1_values(u32::MAX, 32), Some(1));
        assert_eq!(lookup1_values(u32::MAX, 31), Some(2));
    }

    /// setup ヘッダを組む（下位のビットから）。
    pub(crate) struct VorbisSetup {
        b: Vec<u8>,
        at: usize,
    }

    impl VorbisSetup {
        /// codebook の数（1〜256）から始める。
        pub(crate) fn new(books: u32) -> Self {
            let mut w = Self {
                b: b"\x05vorbis".to_vec(),
                at: 7 * 8,
            };
            w.put(u64::from(books - 1), 8);
            w
        }

        pub(crate) fn put(&mut self, v: u64, n: usize) {
            for k in 0..n {
                if self.at.is_multiple_of(8) {
                    self.b.push(0);
                }
                if (v >> k) & 1 == 1 {
                    let last = self.b.len() - 1;
                    self.b[last] |= 1 << (self.at % 8);
                }
                self.at += 1;
            }
        }

        /// ordered の codebook：`runs` は（長さ, 数）を長さの順に。lookup 0。
        pub(crate) fn ordered(&mut self, dims: u64, runs: &[(u32, u64)]) {
            let entries: u64 = runs.iter().map(|r| r.1).sum();
            self.put(0x56_4342, 24);
            self.put(dims, 16);
            self.put(entries, 24);
            self.put(1, 1);
            self.put(u64::from(runs[0].0 - 1), 5);
            let mut cur = 0u64;
            for &(_, n) in runs {
                self.put(n, ilog((entries - cur) as u32));
                cur += n;
            }
            self.put(0, 4);
        }

        /// 識別ヘッダ（30 バイト）を前に付けた `extra_data` の形。
        pub(crate) fn extra(&self) -> Vec<u8> {
            let mut extra = vec![0u8; 30];
            extra.extend_from_slice(&self.b);
            extra
        }
    }

    /// 小さな setup ヘッダが巨大な表を作らせる形（検証で見つけた）を、見積もりで捉える。
    #[test]
    fn vorbis_setup_estimate_catches_huge_codebooks() {
        // 長さ 17 の符号語 131,072 個（完全な木）：探索表は 2 段目のブロックが 65,536 個で約 134 MB
        let mut w = VorbisSetup::new(1);
        w.ordered(1, &[(17, 1 << 17)]);
        let lut = vorbis_setup_bytes(&w.extra()).unwrap();
        assert!(lut >= 2 * 134_000_000, "{lut}");
        // 同じものを 8 つ並べると 8 倍（数百バイトで 2 GB を超える）
        let mut w = VorbisSetup::new(8);
        for _ in 0..8 {
            w.ordered(1, &[(17, 1 << 17)]);
        }
        assert!(w.extra().len() < 200);
        assert!(vorbis_setup_bytes(&w.extra()).unwrap() >= 8 * lut);
        // ふつうの形（長さ 1〜10 の完全な木）は小さい
        let mut w = VorbisSetup::new(1);
        w.ordered(
            2,
            &[
                (1, 1),
                (2, 1),
                (3, 1),
                (4, 1),
                (5, 1),
                (6, 1),
                (7, 1),
                (8, 1),
                (9, 1),
                (10, 2),
            ],
        );
        let small = vorbis_setup_bytes(&w.extra()).unwrap();
        assert!(small < 1 << 16, "{small}");
        // VQ の表（lookup 1：項目 × 次元 × 4）
        let mut w = VorbisSetup::new(1);
        w.put(0x56_4342, 24);
        w.put(32, 16);
        w.put(1 << 17, 24);
        w.put(1, 1);
        w.put(16, 5);
        w.put(1 << 17, 18);
        w.put(1, 4);
        w.put(0, 64);
        w.put(0, 5);
        w.put(0, 8);
        let vq = vorbis_setup_bytes(&w.extra()).unwrap();
        assert!(vq >= (1 << 17) * 32 * 4, "{vq}");
        // symphonia が確保の前に断る形と、識別の印がないものは読まない
        let mut w = VorbisSetup::new(1);
        w.ordered(33, &[(1, 2)]);
        assert_eq!(vorbis_setup_bytes(&w.extra()), None);
        assert_eq!(vorbis_setup_bytes(&[0u8; 40]), None);
        // 実際のファイル（libvorbis・ffmpeg）の setup は小さい
        for name in ["ogg-vorbis-loop.ogg", "ogg-vorbis-ffmpeg.ogg"] {
            let data = std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("fixtures")
                    .join(name),
            )
            .unwrap();
            let o = open(
                "t",
                &data,
                Container::Ogg,
                &crate::monitor::NoMonitor,
                &Limits::default(),
            )
            .unwrap();
            let bytes = vorbis_setup_bytes(o.params.extra_data.as_deref().unwrap()).unwrap();
            assert!(bytes < 8 << 20, "{name}: {bytes}");
        }
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
