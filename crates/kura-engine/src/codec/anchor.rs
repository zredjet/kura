//! アンカー（docs/editing.md §9.4、ADR-135）：再現を約束しないデコーダ（Platform・External と、固定の段でも OS の libm を
//! 使う SVG など）で読んだ値を、固定の段のデコーダで同じ値に読み戻せる可逆の入れ物に残したもの。作り直し・エディタは
//! アンカーを元として読む（OS・ffmpeg が変わっても、版どうしの土台が変わらない）。
//!
//! - 入れ物：画像とフレームの列は標準の PNG・APNG（値の形式のまま。Indexed8 は PLTE＋tRNS、16 ビットはそのまま。ICC は
//!   iCCP。APNG は dispose NONE・blend SOURCE のキャンバスいっぱいのフレーム）に私用のチャンク `kuRa`（ヘッダ。IDAT の前）と
//!   `kuEx`（EXIF の生のバイト列。あるときだけ）を足したもの。音声は f32 の WAV（3 チャンネル以上は拡張形式）に私用の
//!   チャンク `kura`（ヘッダ。data のあと。RIFF の大きさに含める）を足したもの。外のツールでも画素・標本は開ける。
//!   eXIf・テキスト・pHYs・smpl・cue・INFO は書かない（読み手が向きを画素に当てるなど、値が変わるため）。
//! - ヘッダ：私用のチャンクの中身は「BLAKE3（`kura-anchor-header-v1`、長さ付きの JSON）32 バイト ‖ 正規の JSON」。JSON は
//!   [`AnchorHeader`]（版 `v`・書いたもの・中のデコーダの識別子・元の読み取りの記録・元のハッシュ・形式・読み方の設定・
//!   メタデータの正確な値・表示時間・繰り返し）。読み手は、ダイジェスト・正規の形（知らない項目・書き方の揺れを拒む）を
//!   確かめてから使う。
//! - 約束（`anchor-kura-enc/1`・`anchor-kura-dec/1`）：書いたものを読むと、元の読み取りの記録の値のダイジェスト
//!   （`DecodeStamp.value_digest`）と同じ値になる。書くときは値のダイジェストが記録と同じことを確かめ、読むたびにヘッダの
//!   記録と照らす（合わなければ [`Mismatch::AnchorBroken`]）。作るときは [`verify`] で読み直して照らす（[`write_verified`]
//!   はメモリで書いて読み直す）。バイト列（R2）は約束しない（圧縮・書き方は版を上げずに変えてよい）。
//! - 中の画素・標本は、ヘッダに書いた識別子の固定のデコーダ（`png-rs-dec/2`・`wav-kura-dec/1`）で読み、メタデータ（ICC は
//!   iCCP から、EXIF は `kuEx`、ほかはヘッダ）と表示時間・繰り返しをヘッダの値で置き換える。アンカーに書いたことのある
//!   識別子（[`BODY_DECODERS`]）は凍結し、そのデコーダは旧版として永久に残す（§8.4）。
//! - ヘッダの形を変えるときは `v` を上げ、読み手は古い `v` も読み続ける（アンカーは消えない）。知らない `v`・知らない中の
//!   デコーダは `Unsupported`（新しい Kura のアンカー）。
//! - 断るもの：キャンバスいっぱいでないフレームの列（`Unsupported`。どのデコーダもキャンバスいっぱいに合成する）、上限を
//!   超える値・メタデータ（`Limit`）、4 GiB を超える WAV（`Limit`）。浮動小数の画素の形式は値にない（足すときは `v` を上げて
//!   入れ物を増やす。形式の `match` が網羅でないと組み立てが通らない）。
//! - 中止：書くのは 1 枚の画像は 64 行ごと（帯ずつ写して流すので、値と同じ大きさの写しを作らない）、APNG はフレームごと
//!   （png クレートの StreamWriter は APNG の最初の IDAT に fdAT の番号を書いてしまうので、フレームを詰めて書く）、音声は
//!   16,384 フレームごと。読むのは中のデコーダ（PNG は 256 行・WAV は 16,384 フレームごと）と、
//!   音声のダイジェストの塊ごとに確かめる。
//! - 形式の表（`FORMATS`）には載せない内部の形式 [`FORMAT`]（`"anchor"`）。`CodecSet::pinned()` が [`AnchorDecoder`] を持つ
//!   （`decode_with`・ファズ・ゴールデンを同じ口で使うため）。書き出しは `Encoder` にしない（元の読み取りの記録を渡す口が
//!   ないため。[`write`] を直接呼ぶ）。

use std::borrow::Cow;
use std::io::{BufReader, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::codec::{
    DecodeStamp, Decoder, MAX_CHUNKS, ReadSeek, Stage, Tier, contain, png, seek_to, try_vec, wav,
};
use crate::digest::{Digest, Hasher, canonical};
use crate::doc::DecodeOptions;
use crate::error::{EngineError, Mismatch};
use crate::geom::IRect;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Fullness, Shape};
use crate::value::{Audio, AudioMeta, Frame, Frames, Image, ImageMeta, PixelFormat, Raster, Value};

/// 書き出しの約束の版（値だけの約束。ヘッダの形・中のデコーダの選び方・チャンクの意味を変えるときに上げる）。
pub const ENC_ID: &str = "anchor-kura-enc/1";
/// 読み取りの約束の版（ヘッダの `v` で読み分ける）。
pub const DEC_ID: &str = "anchor-kura-dec/1";
/// 形式の表に載せない内部の形式の名前（`CodecSet::decoders_for("anchor")` だけが返す）。
pub const FORMAT: &str = "anchor";
/// 今書くヘッダの版。
pub const HEADER_VERSION: u32 = 1;
/// アンカーの中の画素・標本のデコーダとして書いたことのある識別子（凍結。消さない。そのデコーダを永久に残す）。
pub const BODY_DECODERS: [&str; 2] = ["png-rs-dec/2", "wav-kura-dec/1"];

/// PNG のヘッダのチャンク（補助・私用・予約・コピーしてよい）。
const PNG_HEADER: [u8; 4] = *b"kuRa";
/// PNG の EXIF のチャンク。
const PNG_EXIF: [u8; 4] = *b"kuEx";
/// WAV のヘッダのチャンク。
const WAV_HEADER: [u8; 4] = *b"kura";
/// ヘッダのダイジェストのドメイン。
const HEADER_DOMAIN: &str = "kura-anchor-header-v1";
/// ヘッダの前に置くダイジェストの長さ。
const HEADER_DIGEST_LEN: usize = 32;
/// PNG を書くときに中止を確かめる行の数。
const BAND_ROWS: u32 = 64;
/// WAV を書くときに中止を確かめるフレームの数。
const CHUNK_FRAMES: usize = 16_384;
/// WAV を書くときの作業領域の上限（チャンネルが多いときは塊のフレームを減らす）。
const CHUNK_BYTES: usize = 1 << 20;
/// PNG のチャンクの長さの上限（PNG の仕様。2³¹ − 1）。
const PNG_CHUNK_MAX: u32 = 0x7FFF_FFFF;
/// PNG の署名。
const PNG_SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";

/// 読み取りの記録がアンカーを要するか（再現を約束しないデコーダで読んだ）。
pub fn needs_anchor(d: &DecodeStamp) -> bool {
    !d.reproducible
}

/// アンカーを書くときの文脈（kura-core が版の入力の記録から渡す。`RecordedInput::anchor_context`）。照合と調べ物のための
/// もので、値には効かない。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnchorContext {
    /// 元のファイルの中身のハッシュ（あれば、作り直しでアンカーと元の組を照らす）。
    pub source: Option<Digest>,
    /// 元の形式の ID（`heic` など）。
    pub format: Option<String>,
    /// 元を読んだときの読み方の設定。
    pub options: DecodeOptions,
}

/// アンカーのヘッダ（私用のチャンクの正規の JSON）。知らない項目は拒む。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnchorHeader {
    /// ヘッダの版（[`HEADER_VERSION`]）。
    pub v: u32,
    /// 書いたもの（[`ENC_ID`]）。
    pub anchor: String,
    /// 中の画素・標本のデコーダの識別子（[`BODY_DECODERS`] のどれか）。
    pub body: String,
    /// 元を読んだ記録（読み直した値のダイジェストはこの `value_digest` と同じ）。
    pub decoded: DecodeStamp,
    /// 元のファイルの中身のハッシュ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Digest>,
    /// 元の形式の ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// 元を読んだときの読み方の設定。
    pub options: DecodeOptions,
    /// 値の種類とメタデータの正確な値。
    pub content: AnchorContent,
}

/// 値の種類ごとの中身（メタデータの正確な値。ICC・EXIF はチャンクにあるかだけ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind",
    deny_unknown_fields
)]
pub enum AnchorContent {
    /// 1 枚の画像（PNG）。
    Image {
        meta: ImageMeta,
        /// ICC がある（iCCP）。
        icc: bool,
        /// EXIF がある（`kuEx`）。
        exif: bool,
    },
    /// キャンバスいっぱいのフレームの列（APNG）。
    Frames {
        meta: ImageMeta,
        icc: bool,
        exif: bool,
        /// 各フレームの表示時間（ms。fcTL の分数では表せない値もそのまま）。
        durations: Vec<u32>,
        /// 繰り返しの回数（0 は無限）。
        loop_count: u32,
    },
    /// 音声（f32 の WAV）。
    Audio { meta: AudioMeta },
}

/// 書いた結果。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorStamp {
    /// 書いたもの（[`ENC_ID`]）。
    pub codec: &'static str,
    /// 中のデコーダの識別子。
    pub body: &'static str,
    /// 書いた値のダイジェスト（＝元の読み取りの記録の値）。
    pub value_digest: Digest,
    /// 書いたバイト数。
    pub bytes: u64,
}

/// 読んだ結果。
#[derive(Debug, Clone)]
pub struct AnchorRead {
    /// 値（ダイジェストはヘッダの記録と同じ）。
    pub value: Value,
    /// ヘッダ（元の読み取りの記録を含む）。
    pub header: AnchorHeader,
    /// このアンカーを読んだ記録（`anchor-kura-dec/1`・固定の段・再現できる）。
    pub stamp: DecodeStamp,
}

/// アンカーが使えないことを表すエラー。
fn broken(reason: impl Into<String>) -> EngineError {
    EngineError::Mismatch(Mismatch::AnchorBroken {
        reason: reason.into(),
    })
}

/// 値のダイジェスト（音声は塊ごとに中止を確かめる）。
fn value_digest(v: &Value, mon: &dyn Monitor) -> Result<Digest, EngineError> {
    let check = || {
        if mon.canceled() {
            Err(EngineError::Canceled)
        } else {
            Ok(())
        }
    };
    match v {
        Value::Audio(a) => crate::value::audio_digest(a, &mut || check()),
        other => {
            check()?;
            other.digest()
        }
    }
}

/// ヘッダを私用のチャンクの中身（ダイジェスト ‖ 正規の JSON）にする。
fn header_payload(h: &AnchorHeader) -> Result<Vec<u8>, EngineError> {
    let json = canonical(h)?;
    let d = Hasher::new(HEADER_DOMAIN).bytes_with_len(&json).finish();
    let mut out = Vec::new();
    out.try_reserve_exact(HEADER_DIGEST_LEN + json.len())
        .map_err(|_| EngineError::Limit("アンカーのヘッダを確保できません".into()))?;
    out.extend_from_slice(&d.0);
    out.extend_from_slice(&json);
    Ok(out)
}

/// ICC・EXIF を除いたメタデータ（ヘッダに書く形。ICC・EXIF は直列化しないので、読み戻したものと同じになる）。
fn bare(meta: &ImageMeta) -> ImageMeta {
    ImageMeta {
        icc: None,
        exif: None,
        ..meta.clone()
    }
}

/// 中のデコーダの識別子（今の版）。
fn png_body() -> &'static str {
    Decoder::id(&png::PngCodec)
}

fn wav_body() -> &'static str {
    Decoder::id(&wav::WavCodec)
}

/// 識別子の中のデコーダ（凍結した一覧 [`BODY_DECODERS`] にあるものを、今の版と旧版から）。
fn body_decoder(id: &str) -> Option<Arc<dyn Decoder>> {
    if !BODY_DECODERS.contains(&id) {
        return None;
    }
    let mut all: Vec<Arc<dyn Decoder>> = vec![Arc::new(png::PngCodec), Arc::new(wav::WavCodec)];
    all.extend(png::legacy_decoders());
    all.into_iter().find(|d| d.id() == id)
}

/// 値をアンカーとして `out` に書き、書いた結果を返す。
///
/// - 値のダイジェストが `decoded.value_digest` と違えば書かない（`Internal`：呼び手が別の値を渡した）。
/// - 値の形・メタデータ（ヘッダ・EXIF・ICC の合計）が `limits` を超えれば `Limit`（読み手が同じ上限で読めるように）。
/// - 読み直しの確認は呼び手が [`verify`] で行う（ステージングのファイルから。メモリなら [`write_verified`]）。
pub fn write(
    v: &Value,
    decoded: &DecodeStamp,
    cx: &AnchorContext,
    limits: &Limits,
    out: &mut dyn Write,
    mon: &dyn Monitor,
) -> Result<AnchorStamp, EngineError> {
    limits.check(&v.shape()).map_err(EngineError::Limit)?;
    if let Value::Frames(fr) = v
        && !fr.is_full()
    {
        return Err(EngineError::Unsupported(
            "キャンバスいっぱいでないフレームの列はアンカーにできません".into(),
        ));
    }
    let digest = value_digest(v, mon)?;
    if digest != decoded.value_digest {
        return Err(EngineError::Internal(
            "値のダイジェストが元の読み取りの記録と違うので、アンカーを書けません".into(),
        ));
    }
    let (content, body, icc, exif) = match v {
        Value::Image(img) => (
            AnchorContent::Image {
                meta: bare(&img.meta),
                icc: img.meta.icc.is_some(),
                exif: img.meta.exif.is_some(),
            },
            png_body(),
            img.meta.icc.as_deref().map(Vec::as_slice),
            img.meta.exif.as_deref().map(Vec::as_slice),
        ),
        Value::Frames(fr) => (
            AnchorContent::Frames {
                meta: bare(&fr.meta),
                icc: fr.meta.icc.is_some(),
                exif: fr.meta.exif.is_some(),
                durations: fr.frames().iter().map(|f| f.duration_ms).collect(),
                loop_count: fr.loop_count,
            },
            png_body(),
            fr.meta.icc.as_deref().map(Vec::as_slice),
            fr.meta.exif.as_deref().map(Vec::as_slice),
        ),
        Value::Audio(a) => (
            AnchorContent::Audio {
                meta: a.meta.clone(),
            },
            wav_body(),
            None,
            None,
        ),
    };
    let header = AnchorHeader {
        v: HEADER_VERSION,
        anchor: ENC_ID.to_string(),
        body: body.to_string(),
        decoded: decoded.clone(),
        source: cx.source,
        format: cx.format.clone(),
        options: cx.options.clone(),
        content,
    };
    let payload = header_payload(&header)?;
    let meta_bytes = [
        payload.len(),
        icc.map_or(0, <[u8]>::len),
        exif.map_or(0, <[u8]>::len),
    ]
    .iter()
    .fold(0u64, |s, n| s.saturating_add(*n as u64));
    if meta_bytes > limits.max_meta_bytes {
        return Err(EngineError::Limit(format!(
            "アンカーのメタデータ（{meta_bytes} バイト）が上限 {} を超えます",
            limits.max_meta_bytes
        )));
    }
    let mut counter = Counting {
        inner: out,
        count: 0,
    };
    contain(ENC_ID, Stage::Encode, || match v {
        Value::Image(img) => {
            let mut chunks = vec![(PNG_HEADER, payload.as_slice())];
            if let Some(e) = exif {
                chunks.push((PNG_EXIF, e));
            }
            write_png(&[(&img.px, 0)], None, icc, &chunks, &mut counter, mon)
        }
        Value::Frames(fr) => {
            let mut chunks = vec![(PNG_HEADER, payload.as_slice())];
            if let Some(e) = exif {
                chunks.push((PNG_EXIF, e));
            }
            let frames: Vec<(&Raster, u32)> =
                fr.frames().iter().map(|f| (&f.px, f.duration_ms)).collect();
            write_png(
                &frames,
                Some(fr.loop_count),
                icc,
                &chunks,
                &mut counter,
                mon,
            )
        }
        Value::Audio(a) => write_wav(a, &payload, &mut counter, mon),
    })?;
    Ok(AnchorStamp {
        codec: ENC_ID,
        body,
        value_digest: digest,
        bytes: counter.count,
    })
}

/// メモリにアンカーを書き、読み直して元の読み取りの記録と照らしてから返す（作るときの確認込み。小さな値・テスト用）。
pub fn write_verified(
    v: &Value,
    decoded: &DecodeStamp,
    cx: &AnchorContext,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<(Vec<u8>, AnchorStamp), EngineError> {
    let mut out = Vec::new();
    let stamp = write(v, decoded, cx, limits, &mut out, mon)?;
    verify(&mut std::io::Cursor::new(&out), decoded, limits, mon)?;
    Ok((out, stamp))
}

/// アンカーを読む。ヘッダの記録の値のダイジェストと照らし、合わなければ [`Mismatch::AnchorBroken`]。
/// 壊れた・切れた・ヘッダのない・別の値のアンカーも `AnchorBroken`、新しい Kura のアンカー（知らない `v`・中のデコーダ）は
/// `Unsupported`、上限・中止はそのまま返す。
pub fn read(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<AnchorRead, EngineError> {
    let p = parse(src, limits)?;
    let rest = p.rest(limits);
    let shape = body_shape(src, &p, &rest)?;
    limits.check(&shape).map_err(EngineError::Limit)?;
    src.rewind()?;
    let body = p.body.clone();
    let raw = contain(body.id(), Stage::Decode, || {
        body.decode(
            src,
            p.container.format(),
            &DecodeOptions::default(),
            &rest,
            mon,
        )
    })
    .map_err(body_err)?;
    if !shape.admits(&raw.shape()) {
        return Err(broken("中身の形がヘッダと合いません"));
    }
    let Parsed { header, exif, .. } = p;
    let value = assemble(raw, &header, exif)?;
    let digest = value_digest(&value, mon)?;
    if digest != header.decoded.value_digest {
        return Err(broken("読み直した値が元の読み取りの記録と違います"));
    }
    Ok(AnchorRead {
        value,
        header,
        stamp: DecodeStamp {
            codec: DEC_ID.to_string(),
            tier: Tier::Pinned,
            reproducible: true,
            value_digest: digest,
            env: None,
        },
    })
}

/// ファイルのアンカーを読む（[`read`] のパスの版）。ないファイルは [`Mismatch::AnchorBroken`]。
pub fn read_path(
    path: &Path,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<AnchorRead, EngineError> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(broken("アンカーのファイルがありません"));
        }
        Err(e) => return Err(e.into()),
    };
    read(&mut BufReader::new(file), limits, mon)
}

/// 書いたアンカーを読み直し、元の読み取りの記録（`expect`。[`write`] に渡したもの）とすべて同じか照らす（作るときに必ず呼ぶ）。
pub fn verify(
    src: &mut dyn ReadSeek,
    expect: &DecodeStamp,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<AnchorHeader, EngineError> {
    let r = read(src, limits, mon)?;
    if r.header.decoded != *expect {
        return Err(broken("元の読み取りの記録が違うアンカーです"));
    }
    Ok(r.header)
}

/// アンカーの形（ヘッダと中のデコーダの形。描く前の上限の判断に使う）。
pub fn shape(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Shape, EngineError> {
    let p = parse(src, limits)?;
    let rest = p.rest(limits);
    body_shape(src, &p, &rest)
}

/// アンカーのデコーダ（`anchor-kura-dec/1`。固定の段・再現できる。形式 [`FORMAT`] だけを読む）。
pub struct AnchorDecoder;

impl Decoder for AnchorDecoder {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == FORMAT
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        shape(src, limits)
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        read(src, limits, mon).map(|r| r.value)
    }
}

// ---- 読み取り ----

/// 入れ物の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    Png,
    Wav,
}

impl Container {
    /// 中のデコーダに渡す形式。
    fn format(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Wav => "wav",
        }
    }
}

/// ヘッダまで読んだ結果。
struct Parsed {
    container: Container,
    header: AnchorHeader,
    exif: Option<Vec<u8>>,
    body: Arc<dyn Decoder>,
    /// ヘッダと EXIF に使ったメタデータの予算。
    used: u64,
}

impl Parsed {
    /// 中のデコーダに渡す上限（メタデータの予算から、ヘッダと EXIF に使った分を引く）。
    fn rest(&self, limits: &Limits) -> Limits {
        Limits {
            max_meta_bytes: limits.max_meta_bytes.saturating_sub(self.used),
            ..limits.clone()
        }
    }
}

/// 中のデコーダの誤りを、アンカーの誤りにする（壊れたファイルとしての誤りは `AnchorBroken`。上限・中止などはそのまま）。
fn body_err(e: EngineError) -> EngineError {
    match e {
        EngineError::Decode { codec, message } => broken(format!("{codec}: {message}")),
        EngineError::Unsupported(m) => broken(m),
        EngineError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::InvalidData
            ) =>
        {
            broken(format!("途中で切れています: {io}"))
        }
        other => other,
    }
}

/// 中のデコーダで形を読み、ヘッダと合うか確かめる。
fn body_shape(src: &mut dyn ReadSeek, p: &Parsed, rest: &Limits) -> Result<Shape, EngineError> {
    src.rewind()?;
    let body = p.body.clone();
    let shape = contain(body.id(), Stage::Decode, || {
        body.shape(src, p.container.format(), &DecodeOptions::default(), rest)
    })
    .map_err(body_err)?;
    let ok = match (&p.header.content, &shape) {
        (AnchorContent::Image { .. }, Shape::Image { .. })
        | (AnchorContent::Audio { .. }, Shape::Audio { .. }) => true,
        (AnchorContent::Frames { durations, .. }, Shape::Frames { count, full, .. }) => {
            *full == Fullness::Full && count.exact() == Some(durations.len() as u64)
        }
        _ => false,
    };
    if !ok {
        return Err(broken("中身の種類・フレームの数がヘッダと合いません"));
    }
    Ok(shape)
}

/// 読み口の読み取りの誤り（途中で切れた）を `AnchorBroken` にする。
fn read_exact(src: &mut dyn ReadSeek, buf: &mut [u8], what: &str) -> Result<(), EngineError> {
    src.read_exact(buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => broken(format!("{what}が途中で切れています")),
        _ => EngineError::Io(e),
    })
}

/// 私用のチャンクの中身を、予算の中で読む。
fn read_body(
    src: &mut dyn ReadSeek,
    len: u64,
    budget: &mut u64,
    what: &str,
) -> Result<Vec<u8>, EngineError> {
    if len > *budget {
        return Err(EngineError::Limit(format!(
            "アンカーの{what}がメタデータの上限を超えます（{len} バイト）"
        )));
    }
    *budget -= len;
    let n =
        usize::try_from(len).map_err(|_| EngineError::Limit(format!("{what}が大きすぎます")))?;
    let mut b = try_vec(n, 0u8)?;
    read_exact(src, &mut b, what)?;
    Ok(b)
}

/// 入れ物を見分け、ヘッダ（と EXIF）を読んで確かめる。
fn parse(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Parsed, EngineError> {
    src.rewind()?;
    let mut sig = [0u8; 12];
    let n = read_up_to(src, &mut sig)?;
    let container = if n >= 8 && sig[..8] == PNG_SIGNATURE {
        Container::Png
    } else if n == 12 && &sig[0..4] == b"RIFF" && &sig[8..12] == b"WAVE" {
        Container::Wav
    } else {
        return Err(broken(
            "アンカーではありません（PNG・WAV の見出しがありません）",
        ));
    };
    let mut budget = limits.max_meta_bytes;
    let (payload, exif) = match container {
        Container::Png => scan_png(src, &mut budget)?,
        Container::Wav => (scan_wav(src, &mut budget)?, None),
    };
    let used = limits.max_meta_bytes - budget;
    let header = parse_header(&payload)?;
    let body = body_decoder(&header.body).ok_or_else(|| {
        EngineError::Unsupported(format!(
            "アンカーの中のデコーダ {} がありません（新しい Kura のアンカー）",
            header.body
        ))
    })?;
    if !body.can_decode(container.format()) {
        return Err(broken("中のデコーダが入れ物と合いません"));
    }
    let image_flags = match &header.content {
        AnchorContent::Image { exif, .. } if container == Container::Png => Some(*exif),
        AnchorContent::Frames {
            exif, durations, ..
        } if container == Container::Png => {
            if durations.is_empty() {
                return Err(broken("フレームがありません"));
            }
            if durations.len() as u64 > limits.max_frames {
                return Err(EngineError::Limit(format!(
                    "フレームが {} を超えます",
                    limits.max_frames
                )));
            }
            Some(*exif)
        }
        AnchorContent::Audio { .. } if container == Container::Wav => None,
        _ => return Err(broken("値の種類が入れ物と合いません")),
    };
    if let Some(flag) = image_flags
        && flag != exif.is_some()
    {
        return Err(broken("EXIF のチャンクがヘッダと合いません"));
    }
    Ok(Parsed {
        container,
        header,
        exif,
        body,
        used,
    })
}

/// `buf` を埋めるまで（か終わりまで）読み、読めたバイト数を返す。
fn read_up_to(src: &mut dyn ReadSeek, buf: &mut [u8]) -> Result<usize, EngineError> {
    let mut n = 0;
    while n < buf.len() {
        match src.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(n)
}

/// ヘッダの中身（ダイジェスト ‖ 正規の JSON）を読んで確かめる。
fn parse_header(payload: &[u8]) -> Result<AnchorHeader, EngineError> {
    let Some((d, json)) = payload.split_at_checked(HEADER_DIGEST_LEN) else {
        return Err(broken("ヘッダが短すぎます"));
    };
    if Hasher::new(HEADER_DOMAIN).bytes_with_len(json).finish().0 != d {
        return Err(broken("ヘッダが壊れています（ダイジェストが合いません）"));
    }
    // 版を先に見る（新しい版のヘッダは、今の型では知らない項目を持ちうる）
    let raw: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| broken(format!("ヘッダの JSON が読めません: {e}")))?;
    match raw.get("v").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(HEADER_VERSION) => {}
        Some(v) => {
            return Err(EngineError::Unsupported(format!(
                "アンカーのヘッダの版 {v} は読めません（新しい Kura のアンカー）"
            )));
        }
        None => return Err(broken("ヘッダに版がありません")),
    }
    let header: AnchorHeader = serde_json::from_value(raw)
        .map_err(|e| broken(format!("ヘッダの形が正しくありません: {e}")))?;
    // 正規の形で書き直して同じバイト列か（知らない項目・書き方の揺れ・入れ子の深さを拒む）
    if canonical(&header).map_err(|e| broken(format!("ヘッダを正規の形にできません: {e}")))? != json
    {
        return Err(broken("ヘッダが正規の形ではありません"));
    }
    Ok(header)
}

/// PNG のチャンクの並びを IDAT（か IEND）まで読み、`kuRa`・`kuEx` の中身を返す（CRC を確かめる）。
fn scan_png(
    src: &mut dyn ReadSeek,
    budget: &mut u64,
) -> Result<(Vec<u8>, Option<Vec<u8>>), EngineError> {
    seek_to(src, PNG_SIGNATURE.len() as u64)?;
    let mut header = None;
    let mut exif = None;
    for _ in 0..MAX_CHUNKS {
        let mut head = [0u8; 8];
        read_exact(src, &mut head, "チャンクの見出し")?;
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let kind = [head[4], head[5], head[6], head[7]];
        if len > PNG_CHUNK_MAX {
            return Err(broken("チャンクの長さが正しくありません"));
        }
        match &kind {
            b"IDAT" | b"fdAT" | b"IEND" => {
                let header = header.ok_or_else(|| broken("ヘッダのチャンクがありません"))?;
                return Ok((header, exif));
            }
            k if *k == PNG_HEADER || *k == PNG_EXIF => {
                let (slot, what) = if *k == PNG_HEADER {
                    (&mut header, "ヘッダ")
                } else {
                    (&mut exif, "EXIF")
                };
                if slot.is_some() {
                    return Err(broken(format!("{what}のチャンクが 2 つあります")));
                }
                let body = read_body(src, u64::from(len), budget, what)?;
                let mut crc = [0u8; 4];
                read_exact(src, &mut crc, what)?;
                if u32::from_be_bytes(crc) != crc32(k, &body) {
                    return Err(broken(format!("{what}のチャンクが壊れています（CRC）")));
                }
                *slot = Some(body);
            }
            _ => {
                // 中身と CRC を読み飛ばす（壊れていれば、中のデコーダが CRC で断る）
                src.seek(SeekFrom::Current(i64::from(len) + 4))?;
            }
        }
    }
    Err(broken("チャンクが多すぎます"))
}

/// WAV のチャンクの並びを読み、`kura` の中身を返す。
fn scan_wav(src: &mut dyn ReadSeek, budget: &mut u64) -> Result<Vec<u8>, EngineError> {
    let file_len = src.seek(SeekFrom::End(0))?;
    let mut pos = 12u64;
    let mut header = None;
    let mut chunks = 0usize;
    while pos.saturating_add(8) <= file_len {
        chunks += 1;
        if chunks > MAX_CHUNKS {
            return Err(broken("チャンクが多すぎます"));
        }
        seek_to(src, pos)?;
        let mut head = [0u8; 8];
        read_exact(src, &mut head, "チャンクの見出し")?;
        let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        let body = pos + 8;
        if head[0..4] == WAV_HEADER {
            if header.is_some() {
                return Err(broken("ヘッダのチャンクが 2 つあります"));
            }
            if u64::from(size) > file_len - body {
                return Err(broken("ヘッダのチャンクが途中で切れています"));
            }
            header = Some(read_body(src, u64::from(size), budget, "ヘッダ")?);
        }
        pos = body
            .saturating_add(u64::from(size))
            .saturating_add(u64::from(size & 1));
    }
    header.ok_or_else(|| broken("ヘッダのチャンクがありません"))
}

/// 中のデコーダが読んだ値に、ヘッダのメタデータ・表示時間・繰り返しと EXIF を付ける。
fn assemble(
    raw: Value,
    header: &AnchorHeader,
    exif: Option<Vec<u8>>,
) -> Result<Value, EngineError> {
    let exif = exif.map(Arc::new);
    let icc_of = |got: &Option<Arc<Vec<u8>>>, want: bool| match (got, want) {
        (Some(icc), true) => Ok(Some(icc.clone())),
        (None, false) => Ok(None),
        _ => Err(broken("ICC のチャンクがヘッダと合いません")),
    };
    Ok(match (raw, &header.content) {
        (Value::Image(img), AnchorContent::Image { meta, icc, .. }) => {
            let icc = icc_of(&img.meta.icc, *icc)?;
            let px = img.px.clone();
            Value::Image(Arc::new(Image {
                px,
                meta: ImageMeta {
                    icc,
                    exif,
                    ..meta.clone()
                },
            }))
        }
        (
            Value::Frames(fr),
            AnchorContent::Frames {
                meta,
                icc,
                durations,
                loop_count,
                ..
            },
        ) => {
            let icc = icc_of(&fr.meta.icc, *icc)?;
            if durations.len() != fr.frames().len() {
                return Err(broken("フレームの数がヘッダと合いません"));
            }
            let frames: Vec<Frame> = fr
                .frames()
                .iter()
                .zip(durations)
                .map(|(f, ms)| Frame {
                    px: f.px.clone(),
                    offset: f.offset,
                    duration_ms: *ms,
                })
                .collect();
            Value::Frames(Arc::new(Frames::new(
                frames,
                fr.canvas(),
                ImageMeta {
                    icc,
                    exif,
                    ..meta.clone()
                },
                *loop_count,
            )?))
        }
        (Value::Audio(a), AnchorContent::Audio { meta }) => Value::Audio(Arc::new(Audio::new(
            a.rate(),
            a.planes().to_vec(),
            meta.clone(),
        )?)),
        _ => return Err(broken("中身の種類がヘッダと合いません")),
    })
}

// ---- 書き出し ----

/// 書いたバイト数を数える。
struct Counting<'a> {
    inner: &'a mut dyn Write,
    count: u64,
}

impl Write for Counting<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn enc_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Encode {
        codec: ENC_ID.to_string(),
        message: e.to_string(),
    }
}

/// PNG の色の種類とビット深度（値の形式のまま）。
fn png_color(fmt: PixelFormat) -> (::png::ColorType, ::png::BitDepth) {
    use ::png::{BitDepth as B, ColorType as C};
    match fmt {
        PixelFormat::Gray8 => (C::Grayscale, B::Eight),
        PixelFormat::GrayA8 => (C::GrayscaleAlpha, B::Eight),
        PixelFormat::Rgb8 => (C::Rgb, B::Eight),
        PixelFormat::Rgba8 => (C::Rgba, B::Eight),
        PixelFormat::Gray16 => (C::Grayscale, B::Sixteen),
        PixelFormat::GrayA16 => (C::GrayscaleAlpha, B::Sixteen),
        PixelFormat::Rgb16 => (C::Rgb, B::Sixteen),
        PixelFormat::Rgba16 => (C::Rgba, B::Sixteen),
        PixelFormat::Indexed8 => (C::Indexed, B::Eight),
    }
}

/// PNG（`plays` が `None` なら `frames` の最初の 1 枚の画像、`Some` なら APNG）を書く。私用のチャンクは IDAT の前に置く。
/// 1 枚の画像は `BAND_ROWS` 行ずつ写して流し、帯ごとに中止を確かめる。APNG はフレームごとに書いて確かめる。
fn write_png(
    frames: &[(&Raster, u32)],
    plays: Option<u32>,
    icc: Option<&[u8]>,
    chunks: &[([u8; 4], &[u8])],
    out: &mut dyn Write,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    let Some((first, first_ms)) = frames.first() else {
        return Err(EngineError::Internal("フレームがありません".into()));
    };
    let (w, h, fmt) = (first.width(), first.height(), first.format());
    let (color, depth) = png_color(fmt);
    let mut info = ::png::Info::with_size(w, h);
    info.color_type = color;
    info.bit_depth = depth;
    if let Some(p) = first.palette() {
        let rgb: Vec<u8> = p.colors.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
        let mut alphas: Vec<u8> = p.colors.iter().map(|c| c[3]).collect();
        while alphas.last() == Some(&255) {
            alphas.pop();
        }
        info.palette = Some(Cow::Owned(rgb));
        if !alphas.is_empty() {
            info.trns = Some(Cow::Owned(alphas));
        }
    }
    if let Some(icc) = icc {
        info.icc_profile = Some(Cow::Borrowed(icc));
    }
    let mut enc = ::png::Encoder::with_info(out, info).map_err(enc_err)?;
    enc.set_compression(::png::Compression::Fast);
    if let Some(plays) = plays {
        let n = u32::try_from(frames.len())
            .map_err(|_| EngineError::Limit("フレームが多すぎます".into()))?;
        // acTL の繰り返しは PNG の整数（2³¹ − 1 まで。超えると png クレートが acTL を断る）。正確な値はヘッダ
        enc.set_animated(n, plays.min(PNG_CHUNK_MAX))
            .map_err(enc_err)?;
    }
    let mut writer = enc.write_header().map_err(enc_err)?;
    for (kind, data) in chunks {
        writer
            .write_chunk(::png::chunk::ChunkType(*kind), data)
            .map_err(enc_err)?;
    }
    // fcTL の表示時間は外のツールのため（値の表示時間はヘッダ）。65,535 ms を超える分は詰める
    let delay = |ms: u32| (u16::try_from(ms).unwrap_or(u16::MAX), 1000u16);
    if plays.is_some() {
        let (num, den) = delay(*first_ms);
        writer.set_frame_delay(num, den).map_err(enc_err)?;
        writer
            .set_dispose_op(::png::DisposeOp::None)
            .map_err(enc_err)?;
        writer
            .set_blend_op(::png::BlendOp::Source)
            .map_err(enc_err)?;
    }
    let swap16 = |band: &mut [u8]| {
        if depth == ::png::BitDepth::Sixteen {
            // 値はリトルエンディアン、PNG はビッグエンディアン
            for pair in band.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
    };
    for (px, _) in frames {
        if px.width() != w || px.height() != h || px.format() != fmt {
            return Err(EngineError::Internal(
                "フレームの大きさ・形式がそろっていません".into(),
            ));
        }
    }
    if plays.is_none() {
        // 1 枚の画像：BAND_ROWS 行ずつ写して流す（値と同じ大きさの写しを作らない）
        let row_len = (w as usize)
            .checked_mul(fmt.bytes_per_pixel())
            .ok_or_else(|| EngineError::Limit("画像が大きすぎます".into()))?;
        let band_len = row_len
            .checked_mul(BAND_ROWS as usize)
            .ok_or_else(|| EngineError::Limit("画像が大きすぎます".into()))?;
        let mut buf = try_vec(band_len, 0u8)?;
        let mut stream = writer.stream_writer().map_err(enc_err)?;
        let mut y = 0u32;
        while y < h {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let rows = BAND_ROWS.min(h - y);
            let band = &mut buf[..row_len * rows as usize];
            first.read_rect(IRect::new(0, i64::from(y), w, rows), band)?;
            swap16(band);
            stream.write_all(band).map_err(enc_err)?;
            y += rows;
            mon.progress(u64::from(y), u64::from(h));
        }
        stream.finish().map_err(enc_err)?;
    } else {
        // APNG：フレームごとに書く（png クレートの StreamWriter は、APNG の最初の IDAT にも fdAT の番号を書いてしまうので
        // 使わない）。中止はフレームごとに確かめる
        let total = frames.len() as u64;
        for (i, (px, ms)) in frames.iter().enumerate() {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            if i > 0 {
                let (num, den) = delay(*ms);
                writer.set_frame_delay(num, den).map_err(enc_err)?;
            }
            let mut rows = px.to_rows()?;
            swap16(&mut rows);
            writer.write_image_data(&rows).map_err(enc_err)?;
            mon.progress(i as u64 + 1, total);
        }
    }
    writer.finish().map_err(enc_err)?;
    Ok(())
}

/// f32 の WAV（3 チャンネル以上は拡張形式）を書き、data のあとにヘッダのチャンクを置く。標本は `CHUNK_FRAMES` ずつ書き、
/// 塊ごとに中止を確かめる。RIFF に収まらない（4 GiB を超える）ものは `Limit`。
fn write_wav(
    a: &Audio,
    payload: &[u8],
    out: &mut dyn Write,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    let too_big = || EngineError::Limit("4 GiB を超える音声はアンカー（WAV）にできません".into());
    let channels = u16::try_from(a.channels()).map_err(|_| too_big())?;
    let block_align = channels.checked_mul(4).ok_or_else(too_big)?;
    let frames = a.frames() as u64;
    let data_len = frames
        .checked_mul(u64::from(block_align))
        .ok_or_else(too_big)?;
    let extensible = channels > 2;
    let fmt_len: u64 = if extensible { 40 } else { 16 };
    let payload_len = payload.len() as u64;
    let pad = payload_len & 1;
    let riff = [4u64, 8 + fmt_len, 8 + data_len, 8 + payload_len + pad]
        .iter()
        .try_fold(0u64, |s, n| s.checked_add(*n))
        .ok_or_else(too_big)?;
    // data の大きさ 0xFFFFFFFF は「大きさが分からない」なので書かない
    if riff > u64::from(u32::MAX) || data_len >= u64::from(u32::MAX) {
        return Err(too_big());
    }
    let byte_rate = a
        .rate()
        .checked_mul(u32::from(block_align))
        .ok_or_else(too_big)?;
    let mut head = Vec::with_capacity(12 + 8 + 40 + 8);
    head.extend_from_slice(b"RIFF");
    head.extend_from_slice(&(riff as u32).to_le_bytes());
    head.extend_from_slice(b"WAVE");
    head.extend_from_slice(b"fmt ");
    head.extend_from_slice(&(fmt_len as u32).to_le_bytes());
    head.extend_from_slice(&(if extensible { 0xFFFEu16 } else { 3 }).to_le_bytes());
    head.extend_from_slice(&channels.to_le_bytes());
    head.extend_from_slice(&a.rate().to_le_bytes());
    head.extend_from_slice(&byte_rate.to_le_bytes());
    head.extend_from_slice(&block_align.to_le_bytes());
    head.extend_from_slice(&32u16.to_le_bytes());
    if extensible {
        head.extend_from_slice(&22u16.to_le_bytes());
        head.extend_from_slice(&32u16.to_le_bytes());
        let mask = crate::codec::platform::default_channel_mask(channels).unwrap_or(0);
        head.extend_from_slice(&mask.to_le_bytes());
        // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        head.extend_from_slice(&[
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38,
            0x9B, 0x71,
        ]);
    }
    head.extend_from_slice(b"data");
    head.extend_from_slice(&(data_len as u32).to_le_bytes());
    out.write_all(&head)?;
    let ch = usize::from(channels);
    let n_frames = a.frames();
    // 塊は CHUNK_FRAMES フレームか、作業領域の上限（チャンネルが多いとき）まで
    let chunk = (CHUNK_BYTES / usize::from(block_align)).clamp(1, CHUNK_FRAMES);
    let mut buf = try_vec(chunk * ch * 4, 0u8)?;
    let mut done = 0usize;
    while done < n_frames {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let n = (n_frames - done).min(chunk);
        let bytes = &mut buf[..n * ch * 4];
        for (c, plane) in a.planes().iter().enumerate() {
            for (f, s) in plane[done..done + n].iter().enumerate() {
                let at = (f * ch + c) * 4;
                bytes[at..at + 4].copy_from_slice(&s.to_bits().to_le_bytes());
            }
        }
        out.write_all(bytes)?;
        done += n;
        mon.progress(done as u64, n_frames as u64);
    }
    let mut tail = Vec::with_capacity(8);
    tail.extend_from_slice(&WAV_HEADER);
    tail.extend_from_slice(&(payload_len as u32).to_le_bytes());
    out.write_all(&tail)?;
    out.write_all(payload)?;
    if pad == 1 {
        out.write_all(&[0])?;
    }
    out.flush()?;
    Ok(())
}

/// CRC-32（PNG・zlib の多項式）の表。
const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
};

/// PNG のチャンクの CRC（種類 ‖ 中身）。
fn crc32(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in kind.iter().chain(data) {
        c = CRC_TABLE[((c ^ u32::from(*b)) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{CodecEnv, CodecSet};
    use crate::geom::IPoint;
    use crate::monitor::{CancelAfter, NoMonitor};
    use crate::testgen::{InputGen, MetaGen};
    use crate::value::{AnimTag, AudioSource, Grid, LoopRange, Marker, Slice, TagDirection};
    use std::io::Cursor;

    fn stamp_of(v: &Value) -> DecodeStamp {
        DecodeStamp {
            codec: "heic-fake-dec/1".into(),
            tier: Tier::Platform,
            reproducible: false,
            value_digest: v.digest().unwrap(),
            env: Some(CodecEnv::new("fake", "1.0").with("build", "F1")),
        }
    }

    fn round_trip(v: &Value) -> (Vec<u8>, AnchorRead) {
        let stamp = stamp_of(v);
        let cx = AnchorContext {
            source: Some(Digest([7; 32])),
            format: Some("heic".into()),
            options: DecodeOptions::default(),
        };
        let (bytes, s) = write_verified(v, &stamp, &cx, &Limits::default(), &NoMonitor).unwrap();
        assert_eq!(s.bytes, bytes.len() as u64);
        assert_eq!(s.value_digest, stamp.value_digest);
        let r = read(&mut Cursor::new(&bytes), &Limits::default(), &NoMonitor).unwrap();
        assert_eq!(r.value.digest().unwrap(), stamp.value_digest);
        assert_eq!(r.header.decoded, stamp);
        assert_eq!(r.header.source, cx.source);
        assert_eq!(r.header.format.as_deref(), Some("heic"));
        assert_eq!(
            (r.stamp.codec.as_str(), r.stamp.tier, r.stamp.reproducible),
            (DEC_ID, Tier::Pinned, true)
        );
        (bytes, r)
    }

    fn rich_meta() -> ImageMeta {
        ImageMeta {
            grid: Some(Grid {
                cell_w: 3,
                cell_h: 2,
                offset_x: 1,
                offset_y: 0,
                gap_x: 1,
                gap_y: 2,
            }),
            pivot: Some(IPoint { x: -3, y: 7 }),
            slices: vec![Slice {
                name: "中央".into(),
                rect: IRect::new(1, 1, 3, 2),
                center: Some(IRect::new(2, 1, 1, 1)),
                pivot: Some(IPoint { x: 0, y: 1 }),
            }],
            dpi: Some((1, 72)),
            text: [
                ("".to_string(), "空のキー".to_string()),
                ("NUL\0キー".to_string(), "値\0も".to_string()),
                ("Title".to_string(), "ドット絵".to_string()),
            ]
            .into_iter()
            .collect(),
            tags: vec![AnimTag {
                name: "walk".into(),
                from: 0,
                to: 0,
                direction: TagDirection::PingPongReverse,
                repeat: 3,
            }],
            icc: Some(Arc::new((0..=255u8).cycle().take(700).collect())),
            exif: Some(Arc::new(crate::codec::exif::minimal(6, true))),
        }
    }

    /// すべての画素の形式で、メタデータ（ICC・向き 6 の EXIF・NUL や日本語のテキスト・空のキー・格子・ピボット・スライス・
    /// タグ・DPI）まで値のダイジェストが往復する。外のツール（固定の PNG の読み手）でも同じ画素に読める。
    #[test]
    fn every_pixel_format_round_trips_with_metadata() {
        for fmt in PixelFormat::ALL {
            let base = InputGen::Noise {
                w: 37,
                h: 70,
                fmt,
                seed: 3,
            }
            .generate()
            .unwrap();
            let Value::Image(img) = base else { panic!() };
            let v = Value::Image(Arc::new(Image {
                px: img.px.clone(),
                meta: rich_meta(),
            }));
            let (bytes, r) = round_trip(&v);
            assert_eq!(r.value, v, "{fmt:?}");
            // 外のツールとしての読み取り：PNG として読むと同じ画素（EXIF の向きは当たらない）
            let (png, _) = CodecSet::pinned()
                .decode(
                    &mut Cursor::new(&bytes),
                    "png",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
                .unwrap();
            let Value::Image(p) = png else { panic!() };
            assert_eq!(p.px, img.px, "{fmt:?}");
            assert_eq!(p.meta.icc, rich_meta().icc);
        }
    }

    /// フレームの列：表示時間（0・65,535・70,001・u32 の最大）と繰り返し、パレット・16 ビット。
    #[test]
    fn frames_round_trip_with_exact_durations() {
        for (fmt, plays) in [
            (PixelFormat::Indexed8, 0),
            (PixelFormat::Rgba16, 7),
            (PixelFormat::Gray8, u32::MAX),
        ] {
            let Value::Frames(fr) = (InputGen::NoiseFrames {
                w: 9,
                h: 130,
                fmt,
                count: 4,
                seed: 5,
            })
            .generate()
            .unwrap() else {
                panic!()
            };
            let durations = [0u32, 65_535, 70_001, u32::MAX];
            let frames: Vec<Frame> = fr
                .frames()
                .iter()
                .zip(durations)
                .map(|(f, d)| Frame::full(f.px.clone(), d))
                .collect();
            let v = Value::Frames(Arc::new(
                Frames::new(frames, fr.canvas(), rich_meta(), plays).unwrap(),
            ));
            let (_, r) = round_trip(&v);
            assert_eq!(r.value, v, "{fmt:?}");
        }
        // 1 フレームでもフレームの列のまま
        let one = InputGen::NoiseFrames {
            w: 3,
            h: 2,
            fmt: PixelFormat::Rgb8,
            count: 1,
            seed: 1,
        }
        .generate()
        .unwrap();
        let (_, r) = round_trip(&one);
        assert_eq!(r.value, one);
    }

    /// 音声：NaN の別のビット列・-0・±∞・1 を超える値・非正規化数・長さ 0・チャンネルの数・ループ点・マーカー・source・タグ。
    #[test]
    fn audio_round_trips_bit_exact_with_metadata() {
        let odd = vec![
            f32::NAN,
            f32::from_bits(0x7fc0_0001),
            f32::from_bits(0xffc0_1234),
            -0.0,
            0.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            1.75,
            -1.998,
            f32::from_bits(1),
            f32::MIN_POSITIVE,
        ];
        for channels in [1usize, 2, 3, 6, 32] {
            let planes: Vec<Arc<Vec<f32>>> = (0..channels)
                .map(|c| {
                    let mut p = odd.clone();
                    p.extend((0..CHUNK_FRAMES + 5).map(|i| (i * (c + 1)) as f32 * 1e-5 - 0.3));
                    Arc::new(p)
                })
                .collect();
            let meta = AudioMeta {
                loop_range: Some(LoopRange { start: 9, end: 3 }),
                markers: vec![
                    Marker {
                        pos: u64::MAX,
                        name: "範囲の外".into(),
                    },
                    Marker {
                        pos: 0,
                        name: "\0".into(),
                    },
                ],
                source: Some(AudioSource {
                    bits: 24,
                    float: false,
                    rate: 44_100,
                }),
                tags: [
                    ("title".to_string(), "効果音".to_string()),
                    ("vorbis:ENCODER".to_string(), "aoTuV".to_string()),
                ]
                .into_iter()
                .collect(),
            };
            let a = Audio::new(48_000, planes, meta).unwrap();
            let v = Value::Audio(Arc::new(a));
            let (bytes, r) = round_trip(&v);
            // 標本はビット単位で同じ（NaN のビット列も）
            let (Value::Audio(x), Value::Audio(y)) = (&r.value, &v) else {
                panic!()
            };
            for (p, q) in x.planes().iter().zip(y.planes()) {
                assert!(
                    p.iter()
                        .zip(q.iter())
                        .all(|(a, b)| a.to_bits() == b.to_bits())
                );
            }
            assert_eq!(x.meta, y.meta);
            // 外のツールとしての読み取り：WAV として読める
            let (wav, _) = CodecSet::pinned()
                .decode(
                    &mut Cursor::new(&bytes),
                    "wav",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
                .unwrap();
            assert_eq!(wav.shape(), v.shape());
        }
        // 長さ 0
        let empty = Value::Audio(Arc::new(
            Audio::new(8_000, vec![Arc::new(vec![])], AudioMeta::default()).unwrap(),
        ));
        let (_, r) = round_trip(&empty);
        assert_eq!(r.value, empty);
    }

    /// ICC・EXIF の有無と空のバイト列も往復する（空と「ない」は値のダイジェストで区別される）。
    #[test]
    fn empty_and_missing_icc_exif_round_trip() {
        let px = Raster::filled(2, 2, PixelFormat::Rgb8, None, &[1, 2, 3]).unwrap();
        for (icc, exif) in [
            (None, None),
            (Some(vec![]), None),
            (None, Some(vec![])),
            (Some(vec![1]), Some(vec![2])),
        ] {
            let v = Value::Image(Arc::new(Image {
                px: px.clone(),
                meta: ImageMeta {
                    icc: icc.clone().map(Arc::new),
                    exif: exif.clone().map(Arc::new),
                    ..ImageMeta::default()
                },
            }));
            let (_, r) = round_trip(&v);
            assert_eq!(r.value, v, "{icc:?} {exif:?}");
        }
    }

    /// 書く前の拒否：トリムしたフレームの列、記録と違う値、上限を超える形・メタデータ。
    #[test]
    fn refuses_what_it_cannot_keep() {
        let trimmed = InputGen::TrimmedFrames {
            w: 8,
            h: 6,
            fmt: PixelFormat::Rgba8,
            count: 3,
            seed: 1,
        }
        .generate()
        .unwrap();
        let cx = AnchorContext::default();
        let limits = Limits::default();
        assert!(matches!(
            write(
                &trimmed,
                &stamp_of(&trimmed),
                &cx,
                &limits,
                &mut Vec::new(),
                &NoMonitor
            ),
            Err(EngineError::Unsupported(_))
        ));
        let v = InputGen::Noise {
            w: 4,
            h: 4,
            fmt: PixelFormat::Rgb8,
            seed: 1,
        }
        .generate()
        .unwrap();
        let mut wrong = stamp_of(&v);
        wrong.value_digest = Digest([1; 32]);
        assert!(matches!(
            write(&v, &wrong, &cx, &limits, &mut Vec::new(), &NoMonitor),
            Err(EngineError::Internal(_))
        ));
        let many = InputGen::AudioNoise {
            rate: 8_000,
            channels: 40,
            frames: 4,
            seed: 1,
        }
        .generate()
        .unwrap();
        assert!(matches!(
            write(
                &many,
                &stamp_of(&many),
                &cx,
                &limits,
                &mut Vec::new(),
                &NoMonitor
            ),
            Err(EngineError::Limit(_))
        ));
        let small = Limits {
            max_meta_bytes: 100,
            ..Limits::default()
        };
        let meta = InputGen::WithMeta {
            base: Box::new(InputGen::Noise {
                w: 4,
                h: 4,
                fmt: PixelFormat::Rgb8,
                seed: 1,
            }),
            meta: MetaGen {
                icc: Some(vec![0; 200]),
                ..MetaGen::default()
            },
        }
        .generate()
        .unwrap();
        assert!(matches!(
            write(
                &meta,
                &stamp_of(&meta),
                &cx,
                &small,
                &mut Vec::new(),
                &NoMonitor
            ),
            Err(EngineError::Limit(_))
        ));
    }

    /// 書くのも読むのも、帯・塊ごとに中止できる。
    #[test]
    fn writing_and_reading_can_be_canceled() {
        let img = InputGen::Noise {
            w: 16,
            h: 300,
            fmt: PixelFormat::Rgba8,
            seed: 2,
        }
        .generate()
        .unwrap();
        let snd = InputGen::AudioNoise {
            rate: 8_000,
            channels: 2,
            frames: CHUNK_FRAMES as u64 * 3,
            seed: 2,
        }
        .generate()
        .unwrap();
        for v in [img, snd] {
            let stamp = stamp_of(&v);
            let cx = AnchorContext::default();
            let limits = Limits::default();
            let counter = CancelAfter::new(0);
            let mut bytes = Vec::new();
            write(&v, &stamp, &cx, &limits, &mut bytes, &counter).unwrap();
            let checks = counter.calls();
            assert!(checks >= 3, "{checks}");
            for after in 1..=checks {
                assert!(matches!(
                    write(
                        &v,
                        &stamp,
                        &cx,
                        &limits,
                        &mut Vec::new(),
                        &CancelAfter::new(after)
                    ),
                    Err(EngineError::Canceled)
                ));
            }
            let counter = CancelAfter::new(0);
            read(&mut Cursor::new(&bytes), &limits, &counter).unwrap();
            let checks = counter.calls();
            assert!(checks >= 2, "{checks}");
            for after in 1..=checks {
                assert!(matches!(
                    read(&mut Cursor::new(&bytes), &limits, &CancelAfter::new(after)),
                    Err(EngineError::Canceled)
                ));
            }
        }
    }

    /// 壊れたアンカーは `AnchorBroken`（ヘッダ・CRC・画素・切り詰め・別の記録・ヘッダのないファイル）。新しい版は `Unsupported`。
    #[test]
    fn broken_anchors_are_detected() {
        let v = InputGen::WithMeta {
            base: Box::new(InputGen::Noise {
                w: 20,
                h: 20,
                fmt: PixelFormat::Rgb8,
                seed: 9,
            }),
            meta: MetaGen {
                text: vec![("k".into(), "v".into())],
                exif: Some(crate::codec::exif::minimal(3, false)),
                ..MetaGen::default()
            },
        }
        .generate()
        .unwrap();
        let stamp = stamp_of(&v);
        let (bytes, _) = write_verified(
            &v,
            &stamp,
            &AnchorContext::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
        let is_broken = |b: &[u8]| {
            matches!(
                read(&mut Cursor::new(b), &Limits::default(), &NoMonitor),
                Err(EngineError::Mismatch(Mismatch::AnchorBroken { .. }))
            )
        };
        let find = |kind: &[u8]| bytes.windows(4).position(|w| w == kind).unwrap();
        // ヘッダの JSON の 1 バイト（CRC が合わない）
        let mut b = bytes.clone();
        b[find(b"kuRa") + 4 + 40] ^= 1;
        assert!(is_broken(&b));
        // EXIF の 1 バイト
        let mut b = bytes.clone();
        b[find(b"kuEx") + 6] ^= 1;
        assert!(is_broken(&b));
        // 画素（IDAT の中身。PNG の CRC で断る）
        let mut b = bytes.clone();
        b[find(b"IDAT") + 10] ^= 1;
        assert!(is_broken(&b));
        // 切り詰め（見出し・ヘッダ・画素の途中。値に効かない末尾の IEND の欠けは、値をダイジェストで照らすので問わない）
        for cut in [10, 40, find(b"kuRa") + 20, find(b"IDAT") + 20] {
            assert!(is_broken(&bytes[..cut]), "{cut}");
        }
        // ヘッダのない PNG・空・ほかのファイル
        let plain = CodecSet::pinned()
            .encode_bytes(&v, "png", &serde_json::json!({}), &NoMonitor)
            .unwrap()
            .0;
        assert!(is_broken(&plain));
        assert!(is_broken(&[]));
        assert!(is_broken(b"GIF89a"));
        // 正しいアンカーでも、別の記録とは照合で落ちる
        let mut other = stamp.clone();
        other.env = None;
        assert!(matches!(
            verify(
                &mut Cursor::new(&bytes),
                &other,
                &Limits::default(),
                &NoMonitor
            ),
            Err(EngineError::Mismatch(Mismatch::AnchorBroken { .. }))
        ));
        // ヘッダをダイジェスト・CRC ごと書き直した（記録の値のダイジェストを変えた）アンカー：読み直した値と照らして落ちる
        let mut h = parse(&mut Cursor::new(&bytes), &Limits::default())
            .unwrap()
            .header;
        h.decoded.value_digest = Digest([3; 32]);
        assert!(is_broken(&replace_png_header(
            &bytes,
            &header_payload(&h).unwrap()
        )));
        // 版 2 のヘッダは新しい Kura のアンカー
        let mut j: serde_json::Value = serde_json::to_value(&h).unwrap();
        j["v"] = serde_json::json!(2);
        j["future"] = serde_json::json!(true);
        let json = crate::digest::canonical_json(&j).unwrap();
        let mut payload = Hasher::new(HEADER_DOMAIN)
            .bytes_with_len(&json)
            .finish()
            .0
            .to_vec();
        payload.extend_from_slice(&json);
        assert!(matches!(
            read(
                &mut Cursor::new(replace_png_header(&bytes, &payload)),
                &Limits::default(),
                &NoMonitor
            ),
            Err(EngineError::Unsupported(_))
        ));
        // 版 1 に知らない項目を足したもの・正規でない書き方は壊れたもの
        j["v"] = serde_json::json!(1);
        let json = crate::digest::canonical_json(&j).unwrap();
        let mut payload = Hasher::new(HEADER_DOMAIN)
            .bytes_with_len(&json)
            .finish()
            .0
            .to_vec();
        payload.extend_from_slice(&json);
        assert!(is_broken(&replace_png_header(&bytes, &payload)));
        let pretty = serde_json::to_vec_pretty(&h).unwrap();
        let mut payload = Hasher::new(HEADER_DOMAIN)
            .bytes_with_len(&pretty)
            .finish()
            .0
            .to_vec();
        payload.extend_from_slice(&pretty);
        assert!(is_broken(&replace_png_header(&bytes, &payload)));
    }

    /// テスト用：PNG の `kuRa` の中身を差し替える（CRC も付け直す）。
    fn replace_png_header(bytes: &[u8], payload: &[u8]) -> Vec<u8> {
        let at = bytes.windows(4).position(|w| w == b"kuRa").unwrap() - 4;
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let mut out = bytes[..at].to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(b"kuRa");
        out.extend_from_slice(payload);
        out.extend_from_slice(&crc32(b"kuRa", payload).to_be_bytes());
        out.extend_from_slice(&bytes[at + 12 + len..]);
        out
    }

    /// 中のデコーダの識別子：今書くものは凍結した一覧にあり、一覧のすべてを固定の段の組（旧版を含む）が持つ。
    #[test]
    fn body_decoders_are_frozen_and_kept() {
        assert!(BODY_DECODERS.contains(&png_body()));
        assert!(BODY_DECODERS.contains(&wav_body()));
        let set = CodecSet::pinned();
        for id in BODY_DECODERS {
            assert!(set.decoder_by_id(id).is_some(), "{id}");
            assert_eq!(body_decoder(id).map(|d| d.id()), Some(id));
        }
        assert!(body_decoder("png-rs-dec/99").is_none());
        // 一覧にない識別子（今ある旧版でも）は中のデコーダにしない
        assert!(body_decoder("png-rs-dec/1").is_none());
    }

    /// CRC は zlib と同じ値（PNG の IEND の CRC）。
    #[test]
    fn crc_matches_png() {
        assert_eq!(crc32(b"IEND", &[]), 0xAE42_6082);
        assert_eq!(
            crc32(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]),
            crate::testfiles::crc32(b"IHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0")
        );
    }

    #[test]
    fn needs_anchor_follows_reproducible() {
        let v = InputGen::Noise {
            w: 1,
            h: 1,
            fmt: PixelFormat::Gray8,
            seed: 1,
        }
        .generate()
        .unwrap();
        let mut s = stamp_of(&v);
        assert!(needs_anchor(&s));
        s.reproducible = true;
        assert!(!needs_anchor(&s));
    }
}
