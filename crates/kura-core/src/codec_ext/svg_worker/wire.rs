//! SVG の働き手とのやり取りの形（決定 23）。子の標準入力に頼みを 1 つ、子の標準出力に返事を 1 つ。どちらも版付き・長さ付き・
//! 上限付きで、数はリトルエンディアン。
//!
//! - 頼み（親 → 子）：`KURASVG>`・版 u32・何を u8（1 形・2 値）・見出しの長さ u32・見出し（JSON：形式・読み方・上限・
//!   時間の上限（子は親がいなくなっても、これに猶予を足したら自分で終わる））・
//!   入力の種類 u8（1 パス・2 バイト列）・長さ u64・中身（パスのバイト列か、ファイルの中身）。
//! - 返事（子 → 親）：`KURASVG<`・版 u32・種類 u8（1 形・2 値・3 エラー）・見出しの長さ u32・見出し（JSON）・中身の長さ u64・
//!   中身。値の中身は ICC・EXIF・フレームごとの画素の行（上から、行を詰めて）の順。形とエラーの中身は空。
//! - 親は子の返事を信用しない（[`ReplyReader`]）：版・種類・見出しの大きさ（[`MAX_HEAD`]）・JSON の形（知らない項目は拒む）を
//!   確かめ、値は大きさ・フレームの数・画素のバイト数・メタデータのバイト数を頼んだときの上限（`Limits`）で確かめ、中身の長さが
//!   見出しから決まる長さと等しいことを確かめてから受け取る。画素は届いた分だけタイルに組む（`RasterBuilder`。値の写しを
//!   もう 1 つ持たない）。余分なバイト・足りない中身・パレットにない番号は壊れた返事にする。

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

/// やり取りの形の版（形を変えたら上げる。親と子が違う版なら、どちらも断る）。
pub const VERSION: u32 = 1;
/// 頼みの印。
pub const REQUEST_MAGIC: [u8; 8] = *b"KURASVG>";
/// 返事の印。
pub const REPLY_MAGIC: [u8; 8] = *b"KURASVG<";
/// 見出し（JSON）の上限。値の見出しはフレームの数（`max_frames` は既定で 4,096）とメタデータで伸びる。
pub const MAX_HEAD: u32 = 1 << 20;
/// パスの長さの上限（macOS の PATH_MAX は 1,024）。
pub const MAX_PATH: u64 = 16 << 10;
/// 印・版・種類・見出しの長さ。
const FIXED: usize = 8 + 4 + 1 + 4;
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
    Unsupported { message: String },
    Limit { message: String },
    Canceled,
    Decode { codec: String, message: String },
    Io { io: String, message: String },
    Internal { message: String },
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

fn write_fixed(out: &mut dyn Write, magic: &[u8; 8], kind: u8, head: &[u8]) -> std::io::Result<()> {
    out.write_all(magic)?;
    out.write_all(&VERSION.to_le_bytes())?;
    out.write_all(&[kind])?;
    let len = u32::try_from(head.len()).map_err(std::io::Error::other)?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(head)
}

/// 頼みを書く（親）。`deadline` は親がこの頼みに使う時間の上限。
pub fn write_request(
    out: &mut dyn Write,
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
    write_fixed(out, &REQUEST_MAGIC, op.code(), &head)?;
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

/// 頼みを読む（子）。バイト列の入力は `max_bytes` まで。後ろに余分なバイトがあれば断る。
pub fn read_request(src: &mut dyn Read, max_bytes: u64) -> Result<Request, String> {
    let mut fixed = [0u8; FIXED];
    read_exact(src, &mut fixed)?;
    let (magic, rest) = fixed.split_at(8);
    if magic != REQUEST_MAGIC {
        return Err("頼みの印が違います".into());
    }
    let version = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    if version != VERSION {
        return Err(format!(
            "やり取りの版が違います（頼み {version}・働き手 {VERSION}）"
        ));
    }
    let op = Op::from_code(rest[4]).ok_or("知らない頼みです")?;
    let head_len = u32::from_le_bytes([rest[5], rest[6], rest[7], rest[8]]);
    let head = read_bytes(src, u64::from(head_len), u64::from(MAX_HEAD), "見出し")?;
    let head: RequestHead =
        serde_json::from_slice(&head).map_err(|e| format!("見出しが読めません: {e}"))?;
    let mut kind = [0u8; 9];
    read_exact(src, &mut kind)?;
    let len = u64::from_le_bytes([
        kind[1], kind[2], kind[3], kind[4], kind[5], kind[6], kind[7], kind[8],
    ]);
    let source = match kind[0] {
        INPUT_PATH => Source::Path(PathBuf::from(OsString::from_vec(read_bytes(
            src, len, MAX_PATH, "パス",
        )?))),
        INPUT_BYTES => Source::Bytes(read_bytes(src, len, max_bytes, "入力")?),
        _ => return Err("知らない入力の種類です".into()),
    };
    let mut extra = [0u8; 1];
    match src.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => return Err("頼みの後ろに余分なバイトがあります".into()),
        Err(e) => return Err(format!("頼みの終わりを読めません（{e}）")),
    }
    Ok(Request {
        op,
        format: head.format,
        opts: head.opts,
        limits: head.limits.into(),
        deadline: Duration::from_millis(head.deadline_ms),
        source,
    })
}

/// エラーの返事を書く（子）。
pub fn write_error(out: &mut dyn Write, err: &EngineError) -> std::io::Result<()> {
    let head = json(&WireError::from(err))?;
    write_fixed(out, &REPLY_MAGIC, KIND_ERROR, &head)?;
    out.write_all(&0u64.to_le_bytes())?;
    out.flush()
}

/// 返事を書く（子）。音声の値は返せない（内部の誤りの返事にする）。
pub fn write_reply(
    out: &mut dyn Write,
    result: &Result<Answer, EngineError>,
) -> std::io::Result<()> {
    match result {
        Ok(Answer::Shape(shape)) => {
            let head = json(shape)?;
            write_fixed(out, &REPLY_MAGIC, KIND_SHAPE, &head)?;
            out.write_all(&0u64.to_le_bytes())?;
            out.flush()
        }
        Ok(Answer::Value(value)) => match value_parts(value) {
            Some(parts) => write_value(out, &parts),
            None => write_error(
                out,
                &EngineError::Internal("この働き手は画像の値だけを返せます".into()),
            ),
        },
        Err(err) => write_error(out, err),
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

fn write_value(out: &mut dyn Write, parts: &ValueParts<'_>) -> std::io::Result<()> {
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
    write_fixed(out, &REPLY_MAGIC, KIND_VALUE, &head)?;
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
    state: State,
}

enum State {
    Fixed(Vec<u8>),
    Head { kind: u8, len: usize, buf: Vec<u8> },
    Len { head: Parsed, buf: Vec<u8> },
    Body { head: Parsed, left: u64 },
    Failed,
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

impl ReplyReader {
    /// `op` を頼み、上限 `limits` で確かめる読み手。
    pub fn new(op: Op, limits: &Limits) -> Self {
        Self {
            op,
            limits: limits.clone(),
            state: State::Fixed(Vec::with_capacity(FIXED)),
        }
    }

    /// 返事の長さの上限（子の標準出力の上限に使う）。
    pub fn max_len(&self) -> u64 {
        (FIXED as u64 + u64::from(MAX_HEAD) + 8)
            .saturating_add(self.limits.max_value_bytes)
            .saturating_add(self.limits.max_meta_bytes)
    }

    /// 子の標準出力の区切りを受け取る。壊れていたら理由（以後は受け取らない）。
    pub fn push(&mut self, mut chunk: &[u8]) -> Result<(), String> {
        while !chunk.is_empty() {
            let state = std::mem::replace(&mut self.state, State::Failed);
            let (next, used) = self.step(state, chunk)?;
            self.state = next;
            chunk = &chunk[used..];
        }
        Ok(())
    }

    /// 1 歩進める（使ったバイト数を返す。どの枝も 1 バイト以上使うか、状態を進める）。
    fn step(&self, state: State, chunk: &[u8]) -> Result<(State, usize), String> {
        match state {
            State::Fixed(mut buf) => {
                let n = (FIXED - buf.len()).min(chunk.len());
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() < FIXED {
                    return Ok((State::Fixed(buf), n));
                }
                let (kind, len) = self.fixed(&buf)?;
                let mut head = Vec::new();
                head.try_reserve_exact(len)
                    .map_err(|_| "見出しの領域を確保できません".to_string())?;
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
                let n = (len - buf.len()).min(chunk.len());
                buf.extend_from_slice(&chunk[..n]);
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
                let n = (8 - buf.len()).min(chunk.len());
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() < 8 {
                    return Ok((State::Len { head, buf }, n));
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&buf);
                let len = u64::from_le_bytes(b);
                if len != head.payload_len() {
                    return Err(format!(
                        "中身の長さが見出しと合いません（{len}・見出しから {}）",
                        head.payload_len()
                    ));
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

    /// 印・版・種類・見出しの長さを確かめる。
    fn fixed(&self, buf: &[u8]) -> Result<(u8, usize), String> {
        if buf[..8] != REPLY_MAGIC {
            return Err("返事の印が違います".into());
        }
        let version = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        if version != VERSION {
            return Err(format!(
                "やり取りの版が違います（返事 {version}・親 {VERSION}）"
            ));
        }
        let kind = buf[12];
        let want = match self.op {
            Op::Shape => KIND_SHAPE,
            Op::Decode => KIND_VALUE,
        };
        if kind != want && kind != KIND_ERROR {
            return Err(format!("頼みと違う種類の返事です（{kind}）"));
        }
        let len = u32::from_le_bytes([buf[13], buf[14], buf[15], buf[16]]);
        if len > MAX_HEAD {
            return Err(format!("見出しが大きすぎます（{len} バイト）"));
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
                for fr in &f.list {
                    let inside = fr.x >= 0
                        && fr.y >= 0
                        && fr.x + i64::from(fr.w) <= i64::from(head.w)
                        && fr.y + i64::from(fr.h) <= i64::from(head.h);
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

    fn reply_bytes(result: &Result<Answer, EngineError>) -> Vec<u8> {
        let mut out = Vec::new();
        write_reply(&mut out, result).unwrap();
        out
    }

    /// 区切りを乱数の大きさにして読み手に渡す（1 バイトずつも含む）。
    fn read_back(op: Op, limits: &Limits, bytes: &[u8], seed: u64) -> Result<Reply, String> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut r = ReplyReader::new(op, limits);
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
        r.finish()
    }

    fn image(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Value {
        InputGen::Noise { w, h, fmt, seed }.generate().unwrap()
    }

    /// 頼み：パスもバイト列も、形式・読み方・上限がそのまま届く。後ろの余分・大きすぎる入力・違う版は断る。
    #[test]
    fn requests_round_trip_and_reject_extras() {
        let limits = Limits {
            max_dim: 123,
            max_meta_bytes: 7,
            ..Limits::default()
        };
        for (op, input) in [
            (Op::Shape, Input::Path(Path::new("/tmp/絵 1.svg"))),
            (Op::Decode, Input::Bytes(b"<svg/>")),
        ] {
            let mut b = Vec::new();
            write_request(
                &mut b,
                op,
                input,
                "svg",
                &DecodeOptions::default(),
                &limits,
                Duration::from_millis(1_234),
            )
            .unwrap();
            let req = read_request(&mut b.as_slice(), 1 << 20).unwrap();
            assert_eq!(req.op, op);
            assert_eq!(req.deadline, Duration::from_millis(1_234));
            assert_eq!(req.format, "svg");
            assert_eq!(req.limits, limits);
            match (input, &req.source) {
                (Input::Path(p), Source::Path(q)) => assert_eq!(p, q.as_path()),
                (Input::Bytes(a), Source::Bytes(b)) => assert_eq!(a, b.as_slice()),
                other => panic!("{other:?}"),
            }
            let mut extra = b.clone();
            extra.push(0);
            assert!(read_request(&mut extra.as_slice(), 1 << 20).is_err());
            assert!(read_request(&mut &b[..b.len() - 1], 1 << 20).is_err());
        }
        let mut b = Vec::new();
        let big = vec![b'x'; 100];
        write_request(
            &mut b,
            Op::Decode,
            Input::Bytes(&big),
            "svg",
            &DecodeOptions::default(),
            &limits,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(read_request(&mut b.as_slice(), 99).is_err());
        assert!(read_request(&mut b.as_slice(), 100).is_ok());
        b[8] = 2;
        assert!(
            read_request(&mut b.as_slice(), 100)
                .unwrap_err()
                .contains("版")
        );
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

    /// 見出しの JSON の範囲（返事の固定部の後ろ）。
    fn head_range(b: &[u8]) -> std::ops::Range<usize> {
        let len = u32::from_le_bytes([b[13], b[14], b[15], b[16]]) as usize;
        FIXED..FIXED + len
    }

    /// 見出しを書き換えた返事（中身の長さの欄は元のまま）。
    fn with_head(b: &[u8], f: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let r = head_range(b);
        let mut head: serde_json::Value = serde_json::from_slice(&b[r.clone()]).unwrap();
        f(&mut head);
        let head = serde_json::to_vec(&head).unwrap();
        let mut out = b[..13].to_vec();
        out.extend_from_slice(&(head.len() as u32).to_le_bytes());
        out.extend_from_slice(&head);
        out.extend_from_slice(&b[r.end..]);
        out
    }

    /// 親は子の返事を信用しない：印・版・種類・見出しの大きさと形・上限・中身の長さを確かめ、確保の前に断る。
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
        let refused = |b: &[u8], op: Op| read_back(op, &limits, b, 1).unwrap_err();

        // 印・版・種類
        let mut b = good.clone();
        b[0] = b'X';
        assert!(refused(&b, Op::Decode).contains("印"));
        let mut b = good.clone();
        b[8] = 9;
        assert!(refused(&b, Op::Decode).contains("版"));
        assert!(refused(&good, Op::Shape).contains("種類"));
        // 見出しの長さの欄が大きすぎる（確保しない）
        let mut b = good.clone();
        b[13..17].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(refused(&b, Op::Decode).contains("見出し"));
        // 見出しの JSON が壊れている・知らない項目
        let mut b = good.clone();
        b[FIXED] = b'!';
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
        assert!(ReplyReader::new(Op::Shape, &limits).finish().is_err());
        assert!(ReplyReader::new(Op::Decode, &limits).max_len() > limits.max_value_bytes);
    }
}
