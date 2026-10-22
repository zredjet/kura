//! SVG の働き手とのやり取りの形（決定 23）。子の標準入力に頼みを 1 つ、子の標準出力に返事を 1 つ。どちらも版付き・長さ付き・
//! 上限付きで、数はリトルエンディアン。
//!
//! - 先頭（頼みも返事も同じ）：印（頼み `KURASVG>`・返事 `KURASVG<`）・やり取りの版 u32・ビルドの印の長さ u32・ビルドの印
//!   （[`super::build_stamp`]。SVG のデコーダの識別子・kura-core と kura-engine の版・ビルドの種類・CPU・フォント。M11 の 3 回目の
//!   確認 W2）。版と印は、それより後ろの形を読む前に照らす（別のビルドなら後ろの形が違いうる）。
//! - 頼み（親 → 子）：先頭・何を u8（1 形・2 値）・見出しの長さ u32・見出し（JSON：形式・読み方・上限・時間の上限（子は親が
//!   いなくなっても、これに猶予を足したら自分で終わる））・入力の種類 u8（1 パス・2 バイト列）・長さ u64・中身（パスのバイト列
//!   か、ファイルの中身）。子は長さどおりに読んだら答える（標準入力の EOF を待たない。M11 の 3 回目の確認 W1：同時に起動した
//!   兄弟の子が書き口を受け継いでいると、EOF が来ない）。
//! - 返事（子 → 親）：先頭・種類 u8（1 形・2 値・3 エラー）・見出しの長さ u32・見出し（JSON）・中身の長さ u64・中身。値の中身は
//!   ICC・EXIF・フレームごとの画素の行（上から、行を詰めて）の順。形とエラーの中身は空。
//! - 版か印が違う（別のビルド）：子は頼みの後ろを読まずに、専用のエラー（`foreign`）を自分の印で返す。親は返事の先頭で版・印を
//!   照らし、違えば（印 `KURASVG<` でないものも）「別のビルド」として受け取りをやめる（[`ReplyReader::foreign`]。親は一時的な
//!   誤りにする。`super::is_unavailable`）。
//! - 親は子の返事を信用しない（[`ReplyReader`]）：版・印・種類・見出しの大きさ（[`MAX_HEAD`]）・JSON の形（知らない項目は
//!   拒む）を確かめ、値は大きさ・フレームの数・画素のバイト数・メタデータのバイト数を頼んだときの上限（`Limits`）で確かめ、
//!   中身の長さが見出しから決まる長さと等しいことを確かめてから受け取る。画素は届いた分だけタイルに組む（`RasterBuilder`。
//!   値の写しをもう 1 つ持たない）。余分なバイト・足りない中身・パレットにない番号は壊れた返事にする。

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use kura_engine::doc::DecodeOptions;
use kura_engine::geom::{IPoint, IRect, Size};
use kura_engine::value::{
    Frame, Frames, Image, ImageMeta, Palette, PixelFormat, Raster, RasterBuilder,
};
use kura_engine::{EngineError, Limits, Shape, Value};

/// やり取りの形の版（形を変えたら上げる。親と子が違う版なら、どちらも別のビルドとして断る）。2：先頭にビルドの印を入れ、
/// 子が頼みの後ろの EOF を待たない（M11 の 3 回目の確認 W1・W2）。
pub const VERSION: u32 = 2;
/// 頼みの印。
pub const REQUEST_MAGIC: [u8; 8] = *b"KURASVG>";
/// 返事の印。
pub const REPLY_MAGIC: [u8; 8] = *b"KURASVG<";
/// 見出し（JSON）の上限。値の見出しはフレームの数（`max_frames` は既定で 4,096）とメタデータで伸びる。
pub const MAX_HEAD: u32 = 1 << 20;
/// パスの長さの上限（macOS の PATH_MAX は 1,024）。
pub const MAX_PATH: u64 = 16 << 10;
/// ビルドの印の長さの上限。
pub const MAX_STAMP: u32 = 4 << 10;
/// 先頭の固定部：印・版・ビルドの印の長さ。
const LEAD: usize = 8 + 4 + 4;
/// 印のあとの固定部：何を（種類）・見出しの長さ。
const FIXED: usize = 1 + 4;
/// 子が画素を書く 1 回の区切り（行をこの大きさにまとめて書く。子のメモリを値の写しで増やさない）。
const WRITE_BAND: usize = 1 << 20;

const KIND_SHAPE: u8 = 1;
const KIND_VALUE: u8 = 2;
const KIND_ERROR: u8 = 3;
const INPUT_PATH: u8 = 1;
const INPUT_BYTES: u8 = 2;

/// 何を頼むか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// 形（`Decoder::shape`）。
    Shape,
    /// 値（`Decoder::decode`）。
    Decode,
}

impl Op {
    fn code(self) -> u8 {
        match self {
            Op::Shape => 1,
            Op::Decode => 2,
        }
    }

    fn from_code(c: u8) -> Option<Self> {
        match c {
            1 => Some(Op::Shape),
            2 => Some(Op::Decode),
            _ => None,
        }
    }
}

/// 頼むときの入力。
#[derive(Debug, Clone, Copy)]
pub enum Input<'a> {
    /// ファイルのパス（子が開く。絶対パスで渡す）。
    Path(&'a Path),
    /// ファイルの中身。
    Bytes(&'a [u8]),
}

/// 子が受け取った入力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

/// 子が受け取った頼み。
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub op: Op,
    pub format: String,
    pub opts: DecodeOptions,
    pub limits: Limits,
    /// 親がこの頼みに使う時間の上限（子は親がいなくなっても、これに猶予を足したら自分で終わる）。
    pub deadline: Duration,
    pub source: Source,
}

/// 子が頼みを受け取れなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// 壊れた頼み（内部の誤りの返事にする）。
    Broken(String),
    /// 別のビルドの親からの頼み（やり取りの版か、ビルドの印が違う。専用のエラーの返事にする）。
    Foreign(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Broken(m) | Self::Foreign(m) => f.write_str(m),
        }
    }
}

/// 子が返すもの。
#[derive(Debug)]
pub enum Answer {
    Shape(Shape),
    Value(Value),
}

/// 親が受け取った返事。
#[derive(Debug)]
pub enum Reply {
    Shape(Shape),
    Value(Value),
    Error(EngineError),
}

/// 上限（`Limits`）の写し（`Limits` は serde を持たないので、やり取りのためにここで持つ。項目を足したらここにも足す）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireLimits {
    max_dim: u64,
    max_pixels: u64,
    max_frames: u64,
    max_value_bytes: u64,
    max_audio_frames: u64,
    max_channels: u64,
    max_nodes: u64,
    max_sample_rate: u32,
    max_markers: u64,
    max_meta_bytes: u64,
}

impl From<&Limits> for WireLimits {
    fn from(l: &Limits) -> Self {
        let Limits {
            max_dim,
            max_pixels,
            max_frames,
            max_value_bytes,
            max_audio_frames,
            max_channels,
            max_nodes,
            max_sample_rate,
            max_markers,
            max_meta_bytes,
        } = l.clone();
        Self {
            max_dim,
            max_pixels,
            max_frames,
            max_value_bytes,
            max_audio_frames,
            max_channels,
            max_nodes,
            max_sample_rate,
            max_markers,
            max_meta_bytes,
        }
    }
}

impl From<WireLimits> for Limits {
    fn from(l: WireLimits) -> Self {
        Self {
            max_dim: l.max_dim,
            max_pixels: l.max_pixels,
            max_frames: l.max_frames,
            max_value_bytes: l.max_value_bytes,
            max_audio_frames: l.max_audio_frames,
            max_channels: l.max_channels,
            max_nodes: l.max_nodes,
            max_sample_rate: l.max_sample_rate,
            max_markers: l.max_markers,
            max_meta_bytes: l.max_meta_bytes,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestHead {
    format: String,
    opts: DecodeOptions,
    limits: WireLimits,
    deadline_ms: u64,
}

/// 値の見出し。`frames` がなければ 1 枚の画像（`w`・`h` はその大きさ）、あればフレームの列（`w`・`h` はキャンバス）。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ValueHead {
    fmt: PixelFormat,
    palette: Option<Palette>,
    meta: ImageMeta,
    /// ICC の長さ（中身の先頭）。
    icc: Option<u64>,
    /// EXIF の長さ（ICC の後ろ）。
    exif: Option<u64>,
    w: u32,
    h: u32,
    frames: Option<FramesHead>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FramesHead {
    loop_count: u32,
    list: Vec<FrameHead>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrameHead {
    w: u32,
    h: u32,
    x: i64,
    y: i64,
    duration_ms: u32,
}

/// エラーの写し（読み取りで起きるものだけ。ほかは内部の誤りにする）。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum WireError {
    Unsupported {
        message: String,
    },
    Limit {
        message: String,
    },
    Canceled,
    Decode {
        codec: String,
        message: String,
    },
    Io {
        io: String,
        message: String,
    },
    Internal {
        message: String,
    },
    /// 別のビルドの親からの頼みを断った（親は一時的な誤りにする）。
    Foreign {
        message: String,
    },
}

/// 写す入出力のエラーの種類（ほかは `Other`）。
const IO_KINDS: [(std::io::ErrorKind, &str); 8] = [
    (std::io::ErrorKind::NotFound, "notFound"),
    (std::io::ErrorKind::PermissionDenied, "permissionDenied"),
    (std::io::ErrorKind::UnexpectedEof, "unexpectedEof"),
    (std::io::ErrorKind::InvalidData, "invalidData"),
    (std::io::ErrorKind::InvalidInput, "invalidInput"),
    (std::io::ErrorKind::IsADirectory, "isADirectory"),
    (std::io::ErrorKind::OutOfMemory, "outOfMemory"),
    (std::io::ErrorKind::Interrupted, "interrupted"),
];

impl From<&EngineError> for WireError {
    fn from(e: &EngineError) -> Self {
        match e {
            EngineError::Unsupported(m) => Self::Unsupported { message: m.clone() },
            EngineError::Limit(m) => Self::Limit { message: m.clone() },
            EngineError::Canceled => Self::Canceled,
            EngineError::Decode { codec, message } => Self::Decode {
                codec: codec.clone(),
                message: message.clone(),
            },
            EngineError::Io(err) => Self::Io {
                io: IO_KINDS
                    .iter()
                    .find(|(k, _)| *k == err.kind())
                    .map_or("other", |(_, n)| n)
                    .to_string(),
                message: err.to_string(),
            },
            EngineError::Internal(m) => Self::Internal { message: m.clone() },
            other => Self::Internal {
                message: other.to_string(),
            },
        }
    }
}

impl From<WireError> for EngineError {
    fn from(e: WireError) -> Self {
        match e {
            WireError::Unsupported { message } => Self::Unsupported(message),
            WireError::Limit { message } => Self::Limit(message),
            WireError::Canceled => Self::Canceled,
            WireError::Decode { codec, message } => Self::Decode { codec, message },
            WireError::Io { io, message } => {
                let kind = IO_KINDS
                    .iter()
                    .find(|(_, n)| *n == io)
                    .map_or(std::io::ErrorKind::Other, |(k, _)| *k);
                Self::Io(std::io::Error::new(kind, message))
            }
            WireError::Internal { message } => Self::Internal(message),
            WireError::Foreign { message } => super::unavailable(message),
        }
    }
}

fn json<T: Serialize>(v: &T) -> std::io::Result<Vec<u8>> {
    let b = serde_json::to_vec(v).map_err(std::io::Error::other)?;
    if b.len() > MAX_HEAD as usize {
        return Err(std::io::Error::other("見出しが大きすぎます"));
    }
    Ok(b)
}

/// 先頭（印・版・ビルドの印）を書く。
fn write_lead(out: &mut dyn Write, magic: &[u8; 8], stamp: &str) -> std::io::Result<()> {
    let len = u32::try_from(stamp.len())
        .ok()
        .filter(|n| *n <= MAX_STAMP)
        .ok_or_else(|| std::io::Error::other("ビルドの印が長すぎます"))?;
    out.write_all(magic)?;
    out.write_all(&VERSION.to_le_bytes())?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(stamp.as_bytes())
}

/// 先頭のあとの固定部（種類・見出しの長さ）と見出しを書く。
fn write_fixed(out: &mut dyn Write, kind: u8, head: &[u8]) -> std::io::Result<()> {
    out.write_all(&[kind])?;
    let len = u32::try_from(head.len()).map_err(std::io::Error::other)?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(head)
}

/// ビルドの印の違いの説明（違う項目と両方の値。JSON の対応表として読めなければ先頭だけ）。`ours`・`theirs` は呼び名。
fn stamp_mismatch(mine: &str, other: &[u8], ours: &str, theirs: &str) -> String {
    type Fields = std::collections::BTreeMap<String, serde_json::Value>;
    let parsed = (
        serde_json::from_str::<Fields>(mine),
        serde_json::from_slice::<Fields>(other),
    );
    let detail = match parsed {
        (Ok(a), Ok(b)) => {
            let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            keys.into_iter()
                .filter(|k| a.get(*k) != b.get(*k))
                .map(|k| {
                    let show = |v: Option<&serde_json::Value>| {
                        v.map_or_else(|| "なし".to_string(), serde_json::Value::to_string)
                    };
                    format!(
                        "{k}: {ours} {}・{theirs} {}",
                        show(a.get(k)),
                        show(b.get(k))
                    )
                })
                .collect::<Vec<_>>()
                .join("、")
        }
        _ => {
            let other = String::from_utf8_lossy(other);
            let head = |t: &str| t.chars().take(120).collect::<String>();
            format!("{ours} {}・{theirs} {}", head(mine), head(&other))
        }
    };
    format!("別のビルドです（{detail}）")
}

/// 頼みを書く（親）。`stamp` は親のビルドの印、`deadline` は親がこの頼みに使う時間の上限。
#[allow(clippy::too_many_arguments)]
pub fn write_request(
    out: &mut dyn Write,
    stamp: &str,
    op: Op,
    input: Input<'_>,
    format: &str,
    opts: &DecodeOptions,
    limits: &Limits,
    deadline: Duration,
) -> std::io::Result<()> {
    let head = json(&RequestHead {
        format: format.to_string(),
        opts: opts.clone(),
        limits: WireLimits::from(limits),
        deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
    })?;
    write_lead(out, &REQUEST_MAGIC, stamp)?;
    write_fixed(out, op.code(), &head)?;
    let (kind, data) = match input {
        Input::Path(p) => (INPUT_PATH, p.as_os_str().as_bytes()),
        Input::Bytes(b) => (INPUT_BYTES, b),
    };
    out.write_all(&[kind])?;
    out.write_all(&(data.len() as u64).to_le_bytes())?;
    out.write_all(data)?;
    out.flush()
}

fn read_exact(src: &mut dyn Read, buf: &mut [u8]) -> Result<(), String> {
    src.read_exact(buf)
        .map_err(|e| format!("頼みが途中で切れています（{e}）"))
}

fn read_bytes(src: &mut dyn Read, len: u64, max: u64, what: &str) -> Result<Vec<u8>, String> {
    if len > max {
        return Err(format!("{what}が大きすぎます（{len} バイト）"));
    }
    let len = usize::try_from(len).map_err(|e| e.to_string())?;
    let mut b = Vec::new();
    b.try_reserve_exact(len)
        .map_err(|_| format!("{what}の領域を確保できません"))?;
    b.resize(len, 0);
    read_exact(src, &mut b)?;
    Ok(b)
}

/// 頼みを読む（子）。`stamp` は子のビルドの印、バイト列の入力は `max_bytes` まで。やり取りの版か印が違えば、後ろを読まずに
/// [`RequestError::Foreign`]。長さどおりに読んだら返す（後ろの EOF を待たない。同時に起動した兄弟の子が標準入力の書き口を
/// 受け継いでいると EOF が来ないため。M11 の 3 回目の確認 W1）。
pub fn read_request(
    src: &mut dyn Read,
    max_bytes: u64,
    stamp: &str,
) -> Result<Request, RequestError> {
    let broken = RequestError::Broken;
    let mut lead = [0u8; LEAD];
    read_exact(src, &mut lead).map_err(broken)?;
    if lead[..8] != REQUEST_MAGIC {
        return Err(broken("頼みの印が違います".into()));
    }
    let version = u32::from_le_bytes([lead[8], lead[9], lead[10], lead[11]]);
    if version != VERSION {
        return Err(RequestError::Foreign(format!(
            "やり取りの版が違います（頼み {version}・働き手 {VERSION}）"
        )));
    }
    let stamp_len = u32::from_le_bytes([lead[12], lead[13], lead[14], lead[15]]);
    let theirs = read_bytes(
        src,
        u64::from(stamp_len),
        u64::from(MAX_STAMP),
        "ビルドの印",
    )
    .map_err(broken)?;
    if theirs != stamp.as_bytes() {
        return Err(RequestError::Foreign(stamp_mismatch(
            stamp,
            &theirs,
            "働き手",
            "頼み",
        )));
    }
    let mut fixed = [0u8; FIXED];
    read_exact(src, &mut fixed).map_err(broken)?;
    let op = Op::from_code(fixed[0]).ok_or_else(|| broken("知らない頼みです".into()))?;
    let head_len = u32::from_le_bytes([fixed[1], fixed[2], fixed[3], fixed[4]]);
    let head =
        read_bytes(src, u64::from(head_len), u64::from(MAX_HEAD), "見出し").map_err(broken)?;
    let head: RequestHead =
        serde_json::from_slice(&head).map_err(|e| broken(format!("見出しが読めません: {e}")))?;
    let mut kind = [0u8; 9];
    read_exact(src, &mut kind).map_err(broken)?;
    let len = u64::from_le_bytes([
        kind[1], kind[2], kind[3], kind[4], kind[5], kind[6], kind[7], kind[8],
    ]);
    let source = match kind[0] {
        INPUT_PATH => Source::Path(PathBuf::from(OsString::from_vec(
            read_bytes(src, len, MAX_PATH, "パス").map_err(broken)?,
        ))),
        INPUT_BYTES => Source::Bytes(read_bytes(src, len, max_bytes, "入力").map_err(broken)?),
        _ => return Err(broken("知らない入力の種類です".into())),
    };
    Ok(Request {
        op,
        format: head.format,
        opts: head.opts,
        limits: head.limits.into(),
        deadline: Duration::from_millis(head.deadline_ms),
        source,
    })
}

/// エラーの返事を中身なしで書く。
fn write_wire_error(out: &mut dyn Write, stamp: &str, err: &WireError) -> std::io::Result<()> {
    let head = json(err)?;
    write_lead(out, &REPLY_MAGIC, stamp)?;
    write_fixed(out, KIND_ERROR, &head)?;
    out.write_all(&0u64.to_le_bytes())?;
    out.flush()
}

/// エラーの返事を書く（子。`stamp` は子のビルドの印）。
pub fn write_error(out: &mut dyn Write, stamp: &str, err: &EngineError) -> std::io::Result<()> {
    write_wire_error(out, stamp, &WireError::from(err))
}

/// 別のビルドの親からの頼みを断る返事を書く（子。専用のエラー。親は一時的な誤りにする）。
pub fn write_foreign(out: &mut dyn Write, stamp: &str, message: &str) -> std::io::Result<()> {
    write_wire_error(
        out,
        stamp,
        &WireError::Foreign {
            message: message.to_string(),
        },
    )
}

/// 返事を書く（子。`stamp` は子のビルドの印）。音声の値は返せない（内部の誤りの返事にする）。
pub fn write_reply(
    out: &mut dyn Write,
    stamp: &str,
    result: &Result<Answer, EngineError>,
) -> std::io::Result<()> {
    match result {
        Ok(Answer::Shape(shape)) => {
            let head = json(shape)?;
            write_lead(out, &REPLY_MAGIC, stamp)?;
            write_fixed(out, KIND_SHAPE, &head)?;
            out.write_all(&0u64.to_le_bytes())?;
            out.flush()
        }
        Ok(Answer::Value(value)) => match value_parts(value) {
            Some(parts) => write_value(out, stamp, &parts),
            None => write_error(
                out,
                stamp,
                &EngineError::Internal("この働き手は画像の値だけを返せます".into()),
            ),
        },
        Err(err) => write_error(out, stamp, err),
    }
}

/// 値を書くための分解（画像・フレームの列）。
struct ValueParts<'a> {
    meta: &'a ImageMeta,
    w: u32,
    h: u32,
    loop_count: Option<u32>,
    rasters: Vec<(&'a Raster, IPoint, u32)>,
}

fn value_parts(value: &Value) -> Option<ValueParts<'_>> {
    match value {
        Value::Image(img) => Some(ValueParts {
            meta: &img.meta,
            w: img.px.width(),
            h: img.px.height(),
            loop_count: None,
            rasters: vec![(&img.px, IPoint { x: 0, y: 0 }, 0)],
        }),
        Value::Frames(f) => Some(ValueParts {
            meta: &f.meta,
            w: f.canvas().w,
            h: f.canvas().h,
            loop_count: Some(f.loop_count),
            rasters: f
                .frames()
                .iter()
                .map(|fr| (&fr.px, fr.offset, fr.duration_ms))
                .collect(),
        }),
        _ => None,
    }
}

fn write_value(out: &mut dyn Write, stamp: &str, parts: &ValueParts<'_>) -> std::io::Result<()> {
    let Some((first, _, _)) = parts.rasters.first() else {
        return Err(std::io::Error::other("フレームがありません"));
    };
    let icc: Option<&[u8]> = parts.meta.icc.as_deref().map(Vec::as_slice);
    let exif: Option<&[u8]> = parts.meta.exif.as_deref().map(Vec::as_slice);
    let head = ValueHead {
        fmt: first.format(),
        palette: first.palette().map(|p| (**p).clone()),
        meta: parts.meta.clone(),
        icc: icc.map(|b| b.len() as u64),
        exif: exif.map(|b| b.len() as u64),
        w: parts.w,
        h: parts.h,
        frames: parts.loop_count.map(|loop_count| FramesHead {
            loop_count,
            list: parts
                .rasters
                .iter()
                .map(|(px, at, ms)| FrameHead {
                    w: px.width(),
                    h: px.height(),
                    x: at.x,
                    y: at.y,
                    duration_ms: *ms,
                })
                .collect(),
        }),
    };
    let head = json(&head)?;
    let mut total = icc.map_or(0, <[u8]>::len) as u64 + exif.map_or(0, <[u8]>::len) as u64;
    for (px, _, _) in &parts.rasters {
        total = total.saturating_add(
            u64::from(px.width())
                .saturating_mul(u64::from(px.height()))
                .saturating_mul(px.bpp() as u64),
        );
    }
    write_lead(out, &REPLY_MAGIC, stamp)?;
    write_fixed(out, KIND_VALUE, &head)?;
    out.write_all(&total.to_le_bytes())?;
    out.write_all(icc.unwrap_or_default())?;
    out.write_all(exif.unwrap_or_default())?;
    let mut band = Vec::new();
    for (px, _, _) in &parts.rasters {
        let row_len = px.width() as usize * px.bpp();
        let rows = (WRITE_BAND / row_len.max(1)).max(1);
        let mut y = 0u32;
        while y < px.height() {
            let n = (rows as u32).min(px.height() - y);
            band.resize(row_len * n as usize, 0);
            px.read_rect(
                IRect::new(0, i64::from(y), px.width(), n),
                band.as_mut_slice(),
            )
            .map_err(std::io::Error::other)?;
            out.write_all(&band)?;
            y += n;
        }
    }
    out.flush()
}

/// 返事の読み手（親）。子の標準出力の区切りを順に [`ReplyReader::push`] に渡し、終わったら [`ReplyReader::finish`]。
pub struct ReplyReader {
    op: Op,
    limits: Limits,
    /// 親のビルドの印（返事の印と照らす）。
    stamp: Vec<u8>,
    state: State,
    /// 別のビルドの返事として断った理由（[`ReplyReader::foreign`]）。
    foreign: Option<String>,
}

enum State {
    Lead(Vec<u8>),
    Stamp { len: usize, buf: Vec<u8> },
    Fixed(Vec<u8>),
    Head { kind: u8, len: usize, buf: Vec<u8> },
    Len { head: Parsed, buf: Vec<u8> },
    Body { head: Parsed, left: u64 },
    Failed,
}

/// 返事を断った理由。
enum Refusal {
    /// 壊れた返事。
    Broken(String),
    /// 別のビルドの返事（印・やり取りの版・ビルドの印が違う）。
    Foreign(String),
}

impl From<String> for Refusal {
    fn from(m: String) -> Self {
        Self::Broken(m)
    }
}

impl From<&str> for Refusal {
    fn from(m: &str) -> Self {
        Self::Broken(m.to_string())
    }
}

enum Parsed {
    Shape(Shape),
    Error(WireError),
    Value(Box<ValueSink>),
}

impl Parsed {
    fn payload_len(&self) -> u64 {
        match self {
            Parsed::Value(v) => v.expected,
            _ => 0,
        }
    }
}

/// 決まった長さ `len` までためる（ためた数を返す）。
fn fill(buf: &mut Vec<u8>, len: usize, chunk: &[u8]) -> usize {
    let n = len.saturating_sub(buf.len()).min(chunk.len());
    buf.extend_from_slice(&chunk[..n]);
    n
}

impl ReplyReader {
    /// `op` を頼み、上限 `limits` で確かめる読み手。`stamp` は親のビルドの印（返事の印が違えば別のビルドとして断る）。
    pub fn new(op: Op, limits: &Limits, stamp: &str) -> Self {
        Self {
            op,
            limits: limits.clone(),
            stamp: stamp.as_bytes().to_vec(),
            state: State::Lead(Vec::with_capacity(LEAD)),
            foreign: None,
        }
    }

    /// 返事の長さの上限（子の標準出力の上限に使う）。
    pub fn max_len(&self) -> u64 {
        (LEAD as u64 + u64::from(MAX_STAMP) + FIXED as u64 + u64::from(MAX_HEAD) + 8)
            .saturating_add(self.limits.max_value_bytes)
            .saturating_add(self.limits.max_meta_bytes)
    }

    /// 別のビルドの返事として断ったなら、その理由（返事の印が `KURASVG<` でない・やり取りの版が違う・ビルドの印が違う）。
    pub fn foreign(&self) -> Option<&str> {
        self.foreign.as_deref()
    }

    /// 子の標準出力の区切りを受け取る。壊れていたら理由（以後は受け取らない）。別のビルドの返事なら [`Self::foreign`] も立つ。
    pub fn push(&mut self, mut chunk: &[u8]) -> Result<(), String> {
        while !chunk.is_empty() {
            let state = std::mem::replace(&mut self.state, State::Failed);
            let (next, used) = match self.step(state, chunk) {
                Ok(stepped) => stepped,
                Err(Refusal::Broken(m)) => return Err(m),
                Err(Refusal::Foreign(m)) => {
                    self.foreign = Some(m.clone());
                    return Err(m);
                }
            };
            self.state = next;
            chunk = &chunk[used..];
        }
        Ok(())
    }

    /// 1 歩進める（使ったバイト数を返す。どの枝も 1 バイト以上使うか、状態を進める）。
    fn step(&self, state: State, chunk: &[u8]) -> Result<(State, usize), Refusal> {
        match state {
            State::Lead(mut buf) => {
                let n = fill(&mut buf, LEAD, chunk);
                if buf.len() < LEAD {
                    return Ok((State::Lead(buf), n));
                }
                let len = self.lead(&buf)?;
                Ok((
                    State::Stamp {
                        len,
                        buf: Vec::with_capacity(len),
                    },
                    n,
                ))
            }
            State::Stamp { len, mut buf } => {
                let n = fill(&mut buf, len, chunk);
                if buf.len() < len {
                    return Ok((State::Stamp { len, buf }, n));
                }
                if buf != self.stamp {
                    let mine = String::from_utf8_lossy(&self.stamp);
                    return Err(Refusal::Foreign(stamp_mismatch(
                        &mine,
                        &buf,
                        "親",
                        "働き手",
                    )));
                }
                Ok((State::Fixed(Vec::with_capacity(FIXED)), n))
            }
            State::Fixed(mut buf) => {
                let n = fill(&mut buf, FIXED, chunk);
                if buf.len() < FIXED {
                    return Ok((State::Fixed(buf), n));
                }
                let (kind, len) = self.fixed(&buf)?;
                let mut head = Vec::new();
                head.try_reserve_exact(len)
                    .map_err(|_| "見出しの領域を確保できません")?;
                Ok((
                    State::Head {
                        kind,
                        len,
                        buf: head,
                    },
                    n,
                ))
            }
            State::Head { kind, len, mut buf } => {
                let n = fill(&mut buf, len, chunk);
                if buf.len() < len {
                    return Ok((State::Head { kind, len, buf }, n));
                }
                let head = self.head(kind, &buf)?;
                Ok((
                    State::Len {
                        head,
                        buf: Vec::with_capacity(8),
                    },
                    n,
                ))
            }
            State::Len { head, mut buf } => {
                let n = fill(&mut buf, 8, chunk);
                if buf.len() < 8 {
                    return Ok((State::Len { head, buf }, n));
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&buf);
                let len = u64::from_le_bytes(b);
                if len != head.payload_len() {
                    return Err(Refusal::Broken(format!(
                        "中身の長さが見出しと合いません（{len}・見出しから {}）",
                        head.payload_len()
                    )));
                }
                Ok((State::Body { head, left: len }, n))
            }
            State::Body { mut head, left } => {
                if left == 0 {
                    return Err("返事の後ろに余分なバイトがあります".into());
                }
                let n = usize::try_from(left).unwrap_or(usize::MAX).min(chunk.len());
                if let Parsed::Value(sink) = &mut head {
                    sink.feed(&chunk[..n])?;
                }
                Ok((
                    State::Body {
                        head,
                        left: left - n as u64,
                    },
                    n,
                ))
            }
            State::Failed => Err("壊れた返事です".into()),
        }
    }

    /// 印・版・ビルドの印の長さを確かめる。印か版が違えば別のビルド（働き手でないものを含む）。
    fn lead(&self, buf: &[u8]) -> Result<usize, Refusal> {
        if buf[..8] != REPLY_MAGIC {
            return Err(Refusal::Foreign(
                "働き手の返事ではありません（返事の印が違います）".into(),
            ));
        }
        let version = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        if version != VERSION {
            return Err(Refusal::Foreign(format!(
                "やり取りの版が違います（返事 {version}・親 {VERSION}）"
            )));
        }
        let len = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        if len > MAX_STAMP {
            return Err(Refusal::Broken(format!(
                "ビルドの印が長すぎます（{len} バイト）"
            )));
        }
        Ok(len as usize)
    }

    /// 種類・見出しの長さを確かめる。
    fn fixed(&self, buf: &[u8]) -> Result<(u8, usize), Refusal> {
        let kind = buf[0];
        let want = match self.op {
            Op::Shape => KIND_SHAPE,
            Op::Decode => KIND_VALUE,
        };
        if kind != want && kind != KIND_ERROR {
            return Err(Refusal::Broken(format!(
                "頼みと違う種類の返事です（{kind}）"
            )));
        }
        let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
        if len > MAX_HEAD {
            return Err(Refusal::Broken(format!(
                "見出しが大きすぎます（{len} バイト）"
            )));
        }
        Ok((kind, len as usize))
    }

    /// 見出しを読んで確かめる。
    fn head(&self, kind: u8, buf: &[u8]) -> Result<Parsed, String> {
        let bad = |e: serde_json::Error| format!("見出しが読めません: {e}");
        Ok(match kind {
            KIND_SHAPE => Parsed::Shape(serde_json::from_slice(buf).map_err(bad)?),
            KIND_ERROR => Parsed::Error(serde_json::from_slice(buf).map_err(bad)?),
            _ => {
                let head: ValueHead = serde_json::from_slice(buf).map_err(bad)?;
                Parsed::Value(Box::new(ValueSink::new(head, &self.limits)?))
            }
        })
    }

    /// 返事を受け取り終えた。足りなければ理由。
    pub fn finish(self) -> Result<Reply, String> {
        match self.state {
            State::Body { head, left: 0 } => Ok(match head {
                Parsed::Shape(s) => Reply::Shape(s),
                Parsed::Error(e) => Reply::Error(e.into()),
                Parsed::Value(sink) => Reply::Value(sink.finish()?),
            }),
            State::Failed => Err("壊れた返事です".into()),
            _ => Err("返事が途中で終わりました".into()),
        }
    }
}

/// 値の中身の受け手：ICC・EXIF をためてから、フレームごとに行をタイルに組む。
struct ValueSink {
    head: ValueHead,
    /// 中身の長さ（見出しから決まる）。
    expected: u64,
    /// ICC と EXIF の長さの合計（ためる分）。
    meta_len: usize,
    meta: Vec<u8>,
    /// フレームの大きさ（画像なら 1 つ）。
    sizes: Vec<(u32, u32)>,
    done: Vec<Raster>,
    builder: Option<RasterBuilder>,
    rows: u32,
    row: Vec<u8>,
}

impl ValueSink {
    /// 見出しを上限で確かめ、中身の長さを決める（まだ画素の領域は確保しない）。
    fn new(head: ValueHead, limits: &Limits) -> Result<Self, String> {
        let bpp = head.fmt.bytes_per_pixel() as u64;
        let dims_ok = |w: u32, h: u32| {
            w > 0
                && h > 0
                && u64::from(w) <= limits.max_dim
                && u64::from(h) <= limits.max_dim
                && u64::from(w) * u64::from(h) <= limits.max_pixels
        };
        if !dims_ok(head.w, head.h) {
            return Err(format!("大きさが上限の外です（{}×{}）", head.w, head.h));
        }
        let sizes: Vec<(u32, u32)> = match &head.frames {
            None => vec![(head.w, head.h)],
            Some(f) => {
                if f.list.is_empty() || f.list.len() as u64 > limits.max_frames {
                    return Err(format!("フレームの数が上限の外です（{}）", f.list.len()));
                }
                // 位置は子の見出しから来る i64 なので、足し算のあふれも外とする（kura-engine の `Frames::new` と同じ判定を、
                // 画素を受け取る前にする）
                let within = |at: i64, len: u32, canvas: u32| {
                    at >= 0
                        && at
                            .checked_add(i64::from(len))
                            .is_some_and(|end| end <= i64::from(canvas))
                };
                for fr in &f.list {
                    let inside = within(fr.x, fr.w, head.w) && within(fr.y, fr.h, head.h);
                    if !dims_ok(fr.w, fr.h) || !inside {
                        return Err("フレームがキャンバスの外です".into());
                    }
                }
                f.list.iter().map(|fr| (fr.w, fr.h)).collect()
            }
        };
        let mut pixels = 0u64;
        for (w, h) in &sizes {
            pixels = pixels.saturating_add(u64::from(*w) * u64::from(*h) * bpp);
        }
        if pixels > limits.max_value_bytes {
            return Err(format!("画素のバイト数が上限を超えます（{pixels}）"));
        }
        let meta = head.icc.unwrap_or(0).saturating_add(head.exif.unwrap_or(0));
        if meta > limits.max_meta_bytes {
            return Err(format!("メタデータが上限を超えます（{meta} バイト）"));
        }
        let meta_len = usize::try_from(meta).map_err(|e| e.to_string())?;
        Ok(Self {
            head,
            expected: pixels + meta,
            meta_len,
            meta: Vec::new(),
            sizes,
            done: Vec::new(),
            builder: None,
            rows: 0,
            row: Vec::new(),
        })
    }

    fn feed(&mut self, mut data: &[u8]) -> Result<(), String> {
        let internal = |e: EngineError| format!("画素を組めません: {e}");
        while !data.is_empty() {
            if self.meta.len() < self.meta_len {
                if self.meta.capacity() == 0 {
                    self.meta
                        .try_reserve_exact(self.meta_len)
                        .map_err(|_| "メタデータの領域を確保できません".to_string())?;
                }
                let n = (self.meta_len - self.meta.len()).min(data.len());
                self.meta.extend_from_slice(&data[..n]);
                data = &data[n..];
                continue;
            }
            let builder = match &mut self.builder {
                Some(b) => b,
                None => {
                    let Some(&(w, h)) = self.sizes.get(self.done.len()) else {
                        return Err("画素が多すぎます".into());
                    };
                    let b = RasterBuilder::new(w, h, self.head.fmt, self.head.palette.clone())
                        .map_err(internal)?;
                    self.row.clear();
                    self.row
                        .try_reserve_exact(b.row_len())
                        .map_err(|_| "行の領域を確保できません".to_string())?;
                    self.rows = 0;
                    self.builder.insert(b)
                }
            };
            let row_len = builder.row_len();
            if self.row.is_empty() && data.len() >= row_len {
                builder.push_row(&data[..row_len]).map_err(internal)?;
                data = &data[row_len..];
            } else {
                let n = (row_len - self.row.len()).min(data.len());
                self.row.extend_from_slice(&data[..n]);
                data = &data[n..];
                if self.row.len() < row_len {
                    continue;
                }
                builder.push_row(&self.row).map_err(internal)?;
                self.row.clear();
            }
            self.rows += 1;
            let (_, h) = self.sizes[self.done.len()];
            if self.rows == h
                && let Some(b) = self.builder.take()
            {
                self.done.push(b.finish().map_err(internal)?);
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Value, String> {
        if self.done.len() != self.sizes.len() || self.builder.is_some() {
            return Err("画素が足りません".into());
        }
        let internal = |e: EngineError| format!("値を組めません: {e}");
        let mut meta = self.head.meta;
        let icc_len = usize::try_from(self.head.icc.unwrap_or(0)).map_err(|e| e.to_string())?;
        let (icc, exif) = self.meta.split_at(icc_len.min(self.meta.len()));
        meta.icc = self.head.icc.map(|_| Arc::new(icc.to_vec()));
        meta.exif = self.head.exif.map(|_| Arc::new(exif.to_vec()));
        match self.head.frames {
            None => {
                let mut done = self.done;
                let px = done.pop().ok_or("画素がありません")?;
                Ok(Value::Image(Arc::new(Image { px, meta })))
            }
            Some(f) => {
                let frames = self
                    .done
                    .into_iter()
                    .zip(&f.list)
                    .map(|(px, fr)| Frame {
                        px,
                        offset: IPoint { x: fr.x, y: fr.y },
                        duration_ms: fr.duration_ms,
                    })
                    .collect();
                let canvas = Size {
                    w: self.head.w,
                    h: self.head.h,
                };
                Ok(Value::Frames(Arc::new(
                    Frames::new(frames, canvas, meta, f.loop_count).map_err(internal)?,
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::shape::Dim;
    use kura_engine::testgen::InputGen;
    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::{Rng, SeedableRng};

    /// 親と子のビルドの印（テスト用）と、別のビルドの印。
    const STAMP: &str = r#"{"decoder":"svg-resvg-dec/1","fonts":null}"#;
    const OTHER: &str = r#"{"decoder":"svg-resvg-dec/1","fonts":{"serif":"Times"}}"#;

    fn reply_bytes(result: &Result<Answer, EngineError>) -> Vec<u8> {
        reply_bytes_as(STAMP, result)
    }

    fn reply_bytes_as(stamp: &str, result: &Result<Answer, EngineError>) -> Vec<u8> {
        let mut out = Vec::new();
        write_reply(&mut out, stamp, result).unwrap();
        out
    }

    /// 区切りを乱数の大きさにして読み手に渡す（1 バイトずつも含む）。
    fn read_back(op: Op, limits: &Limits, bytes: &[u8], seed: u64) -> Result<Reply, String> {
        let (r, pushed) = feed(ReplyReader::new(op, limits, STAMP), bytes, seed);
        pushed?;
        r.finish()
    }

    /// 区切りを乱数の大きさにして `r` に渡し、読み手（断った理由が別のビルドかを見る）と渡した結果を返す。
    fn feed(mut r: ReplyReader, bytes: &[u8], seed: u64) -> (ReplyReader, Result<(), String>) {
        let pushed = push_all(&mut r, bytes, seed);
        (r, pushed)
    }

    fn push_all(r: &mut ReplyReader, bytes: &[u8], seed: u64) -> Result<(), String> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = if seed == 0 {
                1
            } else {
                1 + (rng.next_u32() as usize % 70_000)
            }
            .min(rest.len());
            r.push(&rest[..n])?;
            rest = &rest[n..];
        }
        Ok(())
    }

    /// 決まった中身だけを返す読み口。中身のあとに読まれたら印を立てて失敗にする（子が頼みの後ろの EOF を待たないことを
    /// 見る。待てば、兄弟の子が書き口を持ち続けるあいだ止まる）。
    struct NoMore<'a> {
        data: &'a [u8],
        overread: bool,
    }

    impl Read for NoMore<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.data.is_empty() {
                self.overread = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "頼みの後ろを読もうとした",
                ));
            }
            let n = buf.len().min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    fn request(stamp: &str, op: Op, input: Input<'_>, limits: &Limits) -> Vec<u8> {
        let mut b = Vec::new();
        write_request(
            &mut b,
            stamp,
            op,
            input,
            "svg",
            &DecodeOptions::default(),
            limits,
            Duration::from_millis(1_234),
        )
        .unwrap();
        b
    }

    fn image(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Value {
        InputGen::Noise { w, h, fmt, seed }.generate().unwrap()
    }

    /// 頼み：パスもバイト列も、形式・読み方・上限がそのまま届く。子は長さどおりに読んだら返し、後ろを読まない（EOF を
    /// 待たない。M11 の 3 回目の確認 W1）。切れた・大きすぎる入力は壊れた頼み、違う版・違うビルドの印は別のビルド
    /// （後ろを読まずに断る。W2）。
    #[test]
    fn requests_round_trip_without_waiting_for_eof_and_reject_other_builds() {
        let limits = Limits {
            max_dim: 123,
            max_meta_bytes: 7,
            ..Limits::default()
        };
        for (op, input) in [
            (Op::Shape, Input::Path(Path::new("/tmp/絵 1.svg"))),
            (Op::Decode, Input::Bytes(b"<svg/>")),
        ] {
            let b = request(STAMP, op, input, &limits);
            let mut src = NoMore {
                data: &b,
                overread: false,
            };
            let req = read_request(&mut src, 1 << 20, STAMP).unwrap();
            assert!(!src.overread, "頼みの後ろを読んだ（EOF を待った）");
            assert_eq!(req.op, op);
            assert_eq!(req.deadline, Duration::from_millis(1_234));
            assert_eq!(req.format, "svg");
            assert_eq!(req.limits, limits);
            match (input, &req.source) {
                (Input::Path(p), Source::Path(q)) => assert_eq!(p, q.as_path()),
                (Input::Bytes(a), Source::Bytes(b)) => assert_eq!(a, b.as_slice()),
                other => panic!("{other:?}"),
            }
            // 後ろに何があっても読まない（標準入力の書き口が兄弟の子に残っていても、待たずに答える）
            let mut extra = b.clone();
            extra.extend_from_slice(b"rest");
            let mut rest = extra.as_slice();
            assert!(read_request(&mut rest, 1 << 20, STAMP).is_ok());
            assert_eq!(rest, b"rest");
            assert!(matches!(
                read_request(&mut &b[..b.len() - 1], 1 << 20, STAMP),
                Err(RequestError::Broken(_))
            ));
        }
        let big = vec![b'x'; 100];
        let mut b = request(STAMP, Op::Decode, Input::Bytes(&big), &limits);
        assert!(matches!(
            read_request(&mut b.as_slice(), 99, STAMP),
            Err(RequestError::Broken(_))
        ));
        assert!(read_request(&mut b.as_slice(), 100, STAMP).is_ok());
        // 別のビルド：印の違いを項目で示し、後ろ（見出し・入力）を読まずに断る
        let lead = LEAD + STAMP.len();
        let mut src = NoMore {
            data: &b[..lead],
            overread: false,
        };
        match read_request(&mut src, 100, OTHER) {
            Err(RequestError::Foreign(m)) => {
                assert!(m.contains("別のビルド") && m.contains("fonts"), "{m}");
            }
            other => panic!("{other:?}"),
        }
        assert!(!src.overread);
        // 違う版（前の版の親など）：印より後ろを読まずに断る
        b[8] = 1;
        let mut src = NoMore {
            data: &b[..LEAD],
            overread: false,
        };
        match read_request(&mut src, 100, STAMP) {
            Err(RequestError::Foreign(m)) => assert!(m.contains("版"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(!src.overread);
        // 頼みの印が違う・ビルドの印の長さが上限を超えるのは壊れた頼み
        let mut c = request(STAMP, Op::Shape, Input::Bytes(b"<svg/>"), &limits);
        c[0] = b'X';
        assert!(matches!(
            read_request(&mut c.as_slice(), 100, STAMP),
            Err(RequestError::Broken(_))
        ));
        let mut c = request(STAMP, Op::Shape, Input::Bytes(b"<svg/>"), &limits);
        c[12..16].copy_from_slice(&(MAX_STAMP + 1).to_le_bytes());
        assert!(matches!(
            read_request(&mut c.as_slice(), 100, STAMP),
            Err(RequestError::Broken(_))
        ));
    }

    /// 返事：形・画像（メタデータ・ICC・EXIF・パレット）・フレームの列・エラーが、どんな区切りで届いても同じに戻る。
    #[test]
    fn replies_round_trip_in_any_chunking() {
        let limits = Limits::default();
        let shape = Shape::Image {
            w: Dim::Exact(300),
            h: Dim::Exact(2),
            fmt: PixelFormat::Rgba8,
        };
        let mut with_meta = image(300, 270, PixelFormat::Rgba8, 1);
        if let Value::Image(img) = &mut with_meta {
            let img = Arc::make_mut(img);
            img.meta.text.insert("Title".into(), "見本".into());
            img.meta.dpi = Some((144, 144));
            img.meta.icc = Some(Arc::new(vec![1, 2, 3]));
            img.meta.exif = Some(Arc::new(vec![9; 40]));
        }
        let values = [
            with_meta,
            image(5, 3, PixelFormat::Indexed8, 2),
            image(1, 1, PixelFormat::Gray8, 3),
            InputGen::TrimmedFrames {
                w: 40,
                h: 30,
                fmt: PixelFormat::Rgba8,
                count: 3,
                seed: 4,
            }
            .generate()
            .unwrap(),
        ];
        for seed in [0, 1, 2] {
            let b = reply_bytes(&Ok(Answer::Shape(shape)));
            match read_back(Op::Shape, &limits, &b, seed).unwrap() {
                Reply::Shape(s) => assert_eq!(s, shape),
                other => panic!("{other:?}"),
            }
            for v in &values {
                let b = reply_bytes(&Ok(Answer::Value(v.clone())));
                match read_back(Op::Decode, &limits, &b, seed).unwrap() {
                    Reply::Value(got) => {
                        assert_eq!(&got, v);
                        assert_eq!(got.digest().unwrap(), v.digest().unwrap());
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
        let errors = [
            EngineError::Unsupported("画像を含む".into()),
            EngineError::Limit("SVG が大きすぎます".into()),
            EngineError::Canceled,
            EngineError::Decode {
                codec: "svg-resvg-dec/1".into(),
                message: "UTF-8 ではありません".into(),
            },
            EngineError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No such file or directory (os error 2)",
            )),
            EngineError::Internal("内部".into()),
        ];
        for e in &errors {
            for op in [Op::Shape, Op::Decode] {
                let b = reply_bytes(&Err(clone_error(e)));
                match read_back(op, &limits, &b, 1).unwrap() {
                    Reply::Error(got) => {
                        assert_eq!(got.to_string(), e.to_string());
                        assert_eq!(std::mem::discriminant(&got), std::mem::discriminant(e));
                        if let (EngineError::Io(a), EngineError::Io(b)) = (&got, e) {
                            assert_eq!(a.kind(), b.kind());
                        }
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
        // 音声の値は返さない（内部の誤りの返事）
        let audio = InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: 4,
            seed: 1,
        }
        .generate()
        .unwrap();
        let b = reply_bytes(&Ok(Answer::Value(audio)));
        assert!(matches!(
            read_back(Op::Decode, &limits, &b, 1).unwrap(),
            Reply::Error(EngineError::Internal(_))
        ));
    }

    fn clone_error(e: &EngineError) -> EngineError {
        match e {
            EngineError::Io(io) => EngineError::Io(std::io::Error::new(io.kind(), io.to_string())),
            other => EngineError::from(WireError::from(other)),
        }
    }

    fn value_reply(v: &Value) -> Vec<u8> {
        reply_bytes(&Ok(Answer::Value(v.clone())))
    }

    /// 先頭（印・版・ビルドの印）のあとの固定部（種類・見出しの長さ）の位置。
    fn fixed_at(b: &[u8]) -> usize {
        LEAD + u32::from_le_bytes([b[12], b[13], b[14], b[15]]) as usize
    }

    /// 見出しの JSON の範囲（返事の固定部の後ろ）。
    fn head_range(b: &[u8]) -> std::ops::Range<usize> {
        let at = fixed_at(b);
        let len = u32::from_le_bytes([b[at + 1], b[at + 2], b[at + 3], b[at + 4]]) as usize;
        at + FIXED..at + FIXED + len
    }

    /// 見出しを書き換えた返事（中身の長さの欄は元のまま）。
    fn with_head(b: &[u8], f: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let r = head_range(b);
        let mut head: serde_json::Value = serde_json::from_slice(&b[r.clone()]).unwrap();
        f(&mut head);
        let head = serde_json::to_vec(&head).unwrap();
        let mut out = b[..fixed_at(b) + 1].to_vec();
        out.extend_from_slice(&(head.len() as u32).to_le_bytes());
        out.extend_from_slice(&head);
        out.extend_from_slice(&b[r.end..]);
        out
    }

    /// 親は子の返事を信用しない：印・版・ビルドの印・種類・見出しの大きさと形・上限・中身の長さを確かめ、確保の前に断る。
    /// 印・版・ビルドの印が違うものは別のビルド（`foreign`。親は一時的な誤りにする）、ほかは壊れた返事。
    #[test]
    fn broken_or_oversized_replies_are_refused_before_allocating() {
        let limits = Limits {
            max_dim: 64,
            max_pixels: 64 * 64,
            max_value_bytes: 64 * 64 * 4,
            max_frames: 4,
            max_meta_bytes: 16,
            ..Limits::default()
        };
        let v = image(8, 4, PixelFormat::Rgba8, 7);
        let good = value_reply(&v);
        assert!(read_back(Op::Decode, &limits, &good, 1).is_ok());
        // 壊れた返事（別のビルドとはしない）
        let refused = |b: &[u8], op: Op| {
            let (r, pushed) = feed(ReplyReader::new(op, &limits, STAMP), b, 1);
            match pushed {
                Err(e) => {
                    assert_eq!(r.foreign(), None, "{e}");
                    e
                }
                Ok(()) => r.finish().map(drop).unwrap_err(),
            }
        };
        // 別のビルドの返事
        let foreign = |b: &[u8]| {
            let (r, pushed) = feed(ReplyReader::new(Op::Decode, &limits, STAMP), b, 1);
            let err = pushed.unwrap_err();
            assert_eq!(r.foreign(), Some(err.as_str()));
            err
        };

        // 印・版・ビルドの印は別のビルド（働き手でないものを含む）
        let mut b = good.clone();
        b[0] = b'X';
        assert!(foreign(&b).contains("印"));
        let mut b = good.clone();
        b[8] = 9;
        assert!(foreign(&b).contains("版"));
        let other = reply_bytes_as(OTHER, &Ok(Answer::Value(v.clone())));
        let why = foreign(&other);
        assert!(why.contains("別のビルド") && why.contains("fonts"), "{why}");
        let err = reply_bytes_as(OTHER, &Err(EngineError::Internal("x".into())));
        assert!(foreign(&err).contains("別のビルド"));
        // 子が断った（別のビルドの頼み）専用のエラーも、受け取れば働き手を使えない一時的な誤りにする
        let mut b = Vec::new();
        write_foreign(&mut b, STAMP, "別のビルドです").unwrap();
        match read_back(Op::Decode, &limits, &b, 1).unwrap() {
            Reply::Error(e) => assert!(super::super::is_unavailable(&e), "{e}"),
            other => panic!("{other:?}"),
        }
        // ビルドの印の長さの欄が大きすぎる・種類
        let mut b = good.clone();
        b[12..16].copy_from_slice(&(MAX_STAMP + 1).to_le_bytes());
        assert!(refused(&b, Op::Decode).contains("ビルドの印"));
        assert!(refused(&good, Op::Shape).contains("種類"));
        // 見出しの長さの欄が大きすぎる（確保しない）
        let mut b = good.clone();
        let at = fixed_at(&b);
        b[at + 1..at + 5].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(refused(&b, Op::Decode).contains("見出し"));
        // 見出しの JSON が壊れている・知らない項目
        let mut b = good.clone();
        b[head_range(&good).start] = b'!';
        assert!(refused(&b, Op::Decode).contains("見出し"));
        let b = with_head(&good, |h| h["extra"] = 1.into());
        assert!(refused(&b, Op::Decode).contains("見出し"));
        // 上限の外の大きさ・メタデータ（中身の長さの欄より先に断る）
        for (key, val) in [("w", 65u64), ("h", 0), ("icc", 17)] {
            let b = with_head(&good, |h| h[key] = val.into());
            assert!(refused(&b, Op::Decode).contains("上限"), "{key}");
        }
        let huge = with_head(&good, |h| {
            h["w"] = 1_000_000.into();
            h["h"] = 1_000_000.into();
        });
        assert!(read_back(Op::Decode, &Limits::default(), &huge, 1).is_err());
        // フレームの数・キャンバスの外
        let frames = InputGen::NoiseFrames {
            w: 4,
            h: 4,
            fmt: PixelFormat::Rgba8,
            count: 2,
            seed: 1,
        }
        .generate()
        .unwrap();
        let fb = value_reply(&frames);
        assert!(read_back(Op::Decode, &limits, &fb, 2).is_ok());
        let b = with_head(&fb, |h| h["frames"]["list"][1]["x"] = 1.into());
        assert!(refused(&b, Op::Decode).contains("キャンバス"));
        // 位置が i64 の端（幅・高さを足すとあふれる）でも、画素を受け取る前にキャンバスの外として断る（M11 の 3 回目の確認 W4）
        for key in ["x", "y"] {
            for at in [i64::MAX, i64::MAX - 3] {
                let b = with_head(&fb, |h| h["frames"]["list"][0][key] = at.into());
                let mut r = ReplyReader::new(Op::Decode, &limits, STAMP);
                let head_end = head_range(&b).end + 8;
                let err = r.push(&b[..head_end]).unwrap_err();
                assert!(err.contains("キャンバス"), "{key}={at}: {err}");
            }
        }
        let b = with_head(&fb, |h| {
            let one = h["frames"]["list"][0].clone();
            h["frames"]["list"] = serde_json::Value::Array(vec![one; 5]);
        });
        assert!(refused(&b, Op::Decode).contains("フレームの数"));
        // 中身の長さの欄が見出しと合わない
        let r = head_range(&good);
        let mut b = good.clone();
        b[r.end..r.end + 8].copy_from_slice(&(u64::MAX).to_le_bytes());
        assert!(refused(&b, Op::Decode).contains("長さ"));
        // 足りない・余分
        let short = &good[..good.len() - 1];
        assert!(
            read_back(Op::Decode, &limits, short, 1)
                .unwrap_err()
                .contains("途中")
        );
        let mut long = good.clone();
        long.push(0);
        assert!(refused(&long, Op::Decode).contains("余分"));
        // パレットにない番号
        let idx = image(4, 2, PixelFormat::Indexed8, 5);
        let mut b = value_reply(&idx);
        let last = b.len() - 1;
        b[last] = 255;
        assert!(refused(&b, Op::Decode).contains("組めません"));
        // 形の返事の後ろの余分
        let mut b = reply_bytes(&Ok(Answer::Shape(v.shape())));
        b.push(1);
        assert!(refused(&b, Op::Shape).contains("余分"));
        // 空
        assert!(
            ReplyReader::new(Op::Shape, &limits, STAMP)
                .finish()
                .is_err()
        );
        assert!(ReplyReader::new(Op::Decode, &limits, STAMP).max_len() > limits.max_value_bytes);
    }
}
