//! aseprite（.aseprite・.ase）の読み取り（自作。`aseprite-kura-dec/1`。Pinned）。書き出しはしない。
//!
//! 仕様は Aseprite の docs/ase-file-specs.md。合成した各フレームを値にする（層の値 `Layered` は将来）。
//! - 層は下から順に描く。見えない層（親のグループが見えないものを含む）と参照層は描かない。グループ自身は描かない。
//! - ヘッダの印 2（グループを別に合成する）があれば、グループの子を透明な領域に描いてから、グループのブレンドと不透明度で重ねる。
//!   なければ、子をそのまま重ねる（グループのブレンド・不透明度は使わない）。
//! - 層の不透明度は、ヘッダの印 1 があるときだけ使う（なければ 255）。重ねる不透明度は `mul_un8(層, セル)`。
//! - ブレンドは `imaging::blend`（Aseprite の式）。セルの z-index は、同じ親の中の並び（位置 + z、同じなら z の小さい順）に当てる。
//! - リンクしたセルは、指す先のフレームのセルを描く。タイルマップの層はタイルセットのタイルを並べて描く
//!   （タイル 0 は空。反転の印があるタイルは段 4 では読めない：`Unsupported`）。外部のタイルセットは読めない。
//! - 色の形式：RGBA は Rgba8。グレーは RGBA に広げて合成して Rgba8（色相などのモードでは灰色でなくなることがある。
//!   Aseprite 本体のグレーの合成とは丸めが違いうる。asefile と同じ）。パレットは、描く層がどれも通常・不透明度 255 で、
//!   別に合成するグループもなければ、番号のまま重ねて Indexed8（上の層の透明の番号でない画素が勝つ。背景の層では透明の番号も色）、
//!   ほかは色に広げて合成して Rgba8。パレットの透明の番号の色は、背景の層がなければアルファ 0 にする（RGB はパレットのまま）。
//!   この判断と形は、画素を展開しない読み取りで先に決める（形と中身を一致させる）。
//! - フレームが 1 つなら 1 枚の画像、2 つ以上ならフレームの列（どれもキャンバスいっぱい、繰り返しは無限）。表示時間はフレームの ms。
//! - スライスは、フレーム 0 の鍵（なければ最初の鍵）を、キャンバスの座標の矩形・9 分割の中央・ピボットにする。
//!   タグはアニメのタグとして運ぶ。ICC（色のプロファイルのチャンク）を運ぶ。
//! - 上限：ファイル全体を読む前に大きさを `max_value_bytes` で、セル・タイルセットの展開を、それぞれの大きさちょうどで抑える。

use std::io::SeekFrom;
use std::sync::Arc;

use crate::codec::{Decoder, ReadSeek, Tier, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::geom::{IPoint, IRect};
use crate::imaging::blend::{self, BlendMode};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Fullness, Shape};
use crate::value::{
    AnimTag, Frames, Image, ImageMeta, Palette, PixelFormat, Raster, Slice, TagDirection, Value,
};

/// aseprite の読み取り。
pub struct AsepriteCodec;

const DEC_ID: &str = "aseprite-kura-dec/1";

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: e.to_string(),
    }
}

/// 読み進める位置付きのバイト列（範囲の外は落ちずにエラー）。
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], EngineError> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| err("長さがあふれます"))?;
        let s = self
            .b
            .get(self.at..end)
            .ok_or_else(|| err("データが途中で切れています"))?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, EngineError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, EngineError> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn i16(&mut self) -> Result<i16, EngineError> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, EngineError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self) -> Result<i32, EngineError> {
        Ok(self.u32()? as i32)
    }
    fn skip(&mut self, n: usize) -> Result<(), EngineError> {
        self.take(n).map(|_| ())
    }
    fn string(&mut self) -> Result<String, EngineError> {
        let n = usize::from(self.u16()?);
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    Rgba,
    Gray,
    Indexed,
}

impl Depth {
    fn bpp(self) -> usize {
        match self {
            Depth::Rgba => 4,
            Depth::Gray => 2,
            Depth::Indexed => 1,
        }
    }
}

#[derive(Debug, Clone)]
struct Layer {
    visible: bool,
    background: bool,
    reference: bool,
    /// 0 画像・1 グループ・2 タイルマップ。
    kind: u16,
    level: u16,
    blend: BlendMode,
    opacity: u8,
    tileset: u32,
}

#[derive(Debug, Clone)]
enum Content {
    /// 画素（`compressed` なら zlib）。
    Image {
        w: u32,
        h: u32,
        data: (usize, usize),
        compressed: bool,
    },
    Linked(u16),
    Tilemap {
        w: u32,
        h: u32,
        id_mask: u32,
        flip_mask: u32,
        data: (usize, usize),
    },
}

#[derive(Debug, Clone)]
struct Cel {
    layer: usize,
    x: i64,
    y: i64,
    opacity: u8,
    z: i16,
    content: Content,
}

#[derive(Debug, Clone)]
struct Tileset {
    id: u32,
    tw: u32,
    th: u32,
    count: u32,
    zero_is_empty: bool,
    data: Option<(usize, usize)>,
}

/// 画素を展開せずに読んだファイルの構造。
struct Sprite {
    w: u32,
    h: u32,
    depth: Depth,
    flags: u32,
    transparent: u8,
    layers: Vec<Layer>,
    /// フレームごとの（表示時間、セル）。
    frames: Vec<(u32, Vec<Cel>)>,
    palette: Vec<[u8; 4]>,
    tilesets: Vec<Tileset>,
    meta: ImageMeta,
}

fn read_all(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Vec<u8>, EngineError> {
    let len = src.seek(SeekFrom::End(0))?;
    if len > limits.max_value_bytes {
        return Err(EngineError::Limit("ファイルが大きすぎます".into()));
    }
    src.seek(SeekFrom::Start(0))?;
    let mut buf = try_vec(len as usize, 0u8)?;
    src.read_exact(&mut buf)
        .map_err(|_| err("ファイルが読めません"))?;
    Ok(buf)
}

fn parse(file: &[u8], limits: &Limits) -> Result<Sprite, EngineError> {
    let mut h = Cur::new(file);
    let _file_size = h.u32()?;
    if h.u16()? != 0xA5E0 {
        return Err(err("aseprite のファイルではありません"));
    }
    let frame_count = h.u16()?;
    let (w, ht) = (u32::from(h.u16()?), u32::from(h.u16()?));
    let depth = match h.u16()? {
        32 => Depth::Rgba,
        16 => Depth::Gray,
        8 => Depth::Indexed,
        d => return Err(err(format!("色の深さ {d} は読めません"))),
    };
    let flags = h.u32()?;
    h.skip(2 + 4 + 4)?;
    let transparent = h.u8()?;
    h.skip(3)?;
    let ncolors = h.u16()?;
    h.skip(128 - h.at)?;
    if w == 0 || ht == 0 {
        return Err(err("キャンバスの大きさが 0 です"));
    }
    if u64::from(w) > limits.max_dim || u64::from(ht) > limits.max_dim {
        return Err(EngineError::Limit("キャンバスが大きすぎます".into()));
    }
    if u64::from(frame_count) > limits.max_frames || frame_count == 0 {
        return Err(EngineError::Limit(format!(
            "フレームの数が上限の外です（{frame_count}）"
        )));
    }
    let mut layers: Vec<Layer> = Vec::new();
    let mut frames = Vec::with_capacity(usize::from(frame_count));
    let mut palette: Vec<[u8; 4]> = vec![
        [0, 0, 0, 255];
        if ncolors == 0 {
            256
        } else {
            usize::from(ncolors).min(256)
        }
    ];
    let mut new_palette = false;
    let mut old_palette: Option<Vec<[u8; 4]>> = None;
    let mut tilesets = Vec::new();
    let mut meta = ImageMeta::default();
    let mut slices: Vec<Slice> = Vec::new();
    let mut meta_bytes = 0u64;
    let mut budget = |n: usize| -> Result<(), EngineError> {
        meta_bytes = meta_bytes.saturating_add(n as u64 + 32);
        if meta_bytes > limits.max_meta_bytes {
            Err(EngineError::Limit("メタデータが大きすぎます".into()))
        } else {
            Ok(())
        }
    };
    let mut pos = 128usize;
    for _ in 0..frame_count {
        let mut fh = Cur::new(
            file.get(pos..)
                .ok_or_else(|| err("フレームが途中で切れています"))?,
        );
        let bytes = fh.u32()? as usize;
        if fh.u16()? != 0xF1FA {
            return Err(err("フレームの印が正しくありません"));
        }
        let old_chunks = fh.u16()?;
        let duration = u32::from(fh.u16()?);
        fh.skip(2)?;
        let new_chunks = fh.u32()?;
        let chunks = if new_chunks == 0 {
            u32::from(old_chunks)
        } else {
            new_chunks
        };
        let frame_end = pos
            .checked_add(bytes)
            .filter(|e| *e <= file.len())
            .ok_or_else(|| err("フレームの長さが正しくありません"))?;
        let mut cpos = pos + 16;
        let mut cels = Vec::new();
        for _ in 0..chunks {
            let mut ch = Cur::new(
                file.get(cpos..frame_end)
                    .ok_or_else(|| err("チャンクが途中で切れています"))?,
            );
            let size = ch.u32()? as usize;
            let kind = ch.u16()?;
            if size < 6 || cpos.checked_add(size).is_none_or(|e| e > frame_end) {
                return Err(err("チャンクの長さが正しくありません"));
            }
            let body_start = cpos + 6;
            let body = &file[body_start..cpos + size];
            let mut c = Cur::new(body);
            match kind {
                0x2004 => {
                    let lf = c.u16()?;
                    let lk = c.u16()?;
                    let level = c.u16()?;
                    c.skip(4)?;
                    let blend = c.u16()?;
                    let opacity = c.u8()?;
                    c.skip(3)?;
                    let _name = c.string()?;
                    let tileset = if lk == 2 { c.u32()? } else { 0 };
                    if layers.len() >= 65_535 {
                        return Err(EngineError::Limit("層が多すぎます".into()));
                    }
                    layers.push(Layer {
                        visible: lf & 1 != 0,
                        background: lf & 8 != 0,
                        reference: lf & 64 != 0,
                        kind: lk,
                        level,
                        blend: BlendMode::from_code(blend).unwrap_or(BlendMode::Normal),
                        opacity,
                        tileset,
                    });
                }
                0x2005 => {
                    let layer = usize::from(c.u16()?);
                    let x = i64::from(c.i16()?);
                    let y = i64::from(c.i16()?);
                    let opacity = c.u8()?;
                    let ct = c.u16()?;
                    let z = c.i16()?;
                    c.skip(5)?;
                    let content = match ct {
                        0 | 2 => {
                            let (cw, chh) = (u32::from(c.u16()?), u32::from(c.u16()?));
                            let start = body_start + c.at;
                            let len = body.len() - c.at;
                            Content::Image {
                                w: cw,
                                h: chh,
                                data: (start, len),
                                compressed: ct == 2,
                            }
                        }
                        1 => Content::Linked(c.u16()?),
                        3 => {
                            let (tw, th) = (u32::from(c.u16()?), u32::from(c.u16()?));
                            let bits = c.u16()?;
                            if bits != 32 {
                                return Err(EngineError::Unsupported(format!(
                                    "タイルの {bits} ビットは読めません"
                                )));
                            }
                            let id_mask = c.u32()?;
                            let xf = c.u32()?;
                            let yf = c.u32()?;
                            let df = c.u32()?;
                            c.skip(10)?;
                            let start = body_start + c.at;
                            let len = body.len() - c.at;
                            Content::Tilemap {
                                w: tw,
                                h: th,
                                id_mask,
                                flip_mask: xf | yf | df,
                                data: (start, len),
                            }
                        }
                        _ => return Err(err(format!("セルの種類 {ct} は読めません"))),
                    };
                    cels.push(Cel {
                        layer,
                        x,
                        y,
                        opacity,
                        z,
                        content,
                    });
                }
                0x2007 => {
                    let pt = c.u16()?;
                    c.skip(2 + 4 + 8)?;
                    if pt == 2 {
                        let n = c.u32()? as usize;
                        budget(n)?;
                        meta.icc = Some(Arc::new(c.take(n)?.to_vec()));
                    }
                }
                0x2018 => {
                    let n = usize::from(c.u16()?);
                    c.skip(8)?;
                    for _ in 0..n {
                        let from = u32::from(c.u16()?);
                        let to = u32::from(c.u16()?);
                        let dir = c.u8()?;
                        let repeat = u32::from(c.u16()?);
                        c.skip(6 + 3 + 1)?;
                        let name = c.string()?;
                        budget(name.len())?;
                        meta.tags.push(AnimTag {
                            name,
                            from,
                            to,
                            direction: match dir {
                                1 => TagDirection::Reverse,
                                2 => TagDirection::PingPong,
                                3 => TagDirection::PingPongReverse,
                                _ => TagDirection::Forward,
                            },
                            repeat,
                        });
                    }
                }
                0x2019 => {
                    let size = c.u32()? as usize;
                    let first = c.u32()? as usize;
                    let last = c.u32()? as usize;
                    c.skip(8)?;
                    if size > 256 || last >= 256 || first > last {
                        return Err(err("パレットの範囲が正しくありません"));
                    }
                    if palette.len() < size {
                        palette.resize(size, [0, 0, 0, 255]);
                    }
                    for i in first..=last {
                        let f = c.u16()?;
                        let rgba = [c.u8()?, c.u8()?, c.u8()?, c.u8()?];
                        if f & 1 != 0 {
                            c.string()?;
                        }
                        if i >= palette.len() {
                            palette.resize(i + 1, [0, 0, 0, 255]);
                        }
                        palette[i] = rgba;
                    }
                    new_palette = true;
                }
                0x0004 | 0x0011 => {
                    let packets = c.u16()?;
                    let mut p = vec![[0, 0, 0, 255]; 256];
                    let mut i = 0usize;
                    for _ in 0..packets {
                        i += usize::from(c.u8()?);
                        let n = match c.u8()? {
                            0 => 256,
                            n => usize::from(n),
                        };
                        for _ in 0..n {
                            let mut rgb = [c.u8()?, c.u8()?, c.u8()?];
                            if kind == 0x0011 {
                                // 0〜63 を 0〜255 に
                                for v in &mut rgb {
                                    *v = ((u16::from(*v) * 255 + 31) / 63) as u8;
                                }
                            }
                            if i < 256 {
                                p[i] = [rgb[0], rgb[1], rgb[2], 255];
                            }
                            i += 1;
                        }
                    }
                    old_palette.get_or_insert(p);
                }
                0x2022 => {
                    let keys = c.u32()?;
                    let sflags = c.u32()?;
                    c.skip(4)?;
                    let name = c.string()?;
                    budget(name.len())?;
                    let mut chosen: Option<Slice> = None;
                    for k in 0..keys {
                        let frame = c.u32()?;
                        let (sx, sy) = (i64::from(c.i32()?), i64::from(c.i32()?));
                        let (sw, sh) = (c.u32()?, c.u32()?);
                        let center = if sflags & 1 != 0 {
                            let (cx, cy) = (i64::from(c.i32()?), i64::from(c.i32()?));
                            let (cw, chh) = (c.u32()?, c.u32()?);
                            Some(IRect::new(sx + cx, sy + cy, cw, chh))
                        } else {
                            None
                        };
                        let pivot = if sflags & 2 != 0 {
                            let (px, py) = (i64::from(c.i32()?), i64::from(c.i32()?));
                            Some(IPoint {
                                x: sx + px,
                                y: sy + py,
                            })
                        } else {
                            None
                        };
                        if k == 0 || frame == 0 {
                            chosen = Some(Slice {
                                name: name.clone(),
                                rect: IRect::new(sx, sy, sw, sh),
                                center,
                                pivot,
                            });
                        }
                    }
                    if let Some(s) = chosen {
                        slices.push(s);
                    }
                }
                0x2023 => {
                    let id = c.u32()?;
                    let tf = c.u32()?;
                    let count = c.u32()?;
                    let (tw, th) = (u32::from(c.u16()?), u32::from(c.u16()?));
                    c.skip(2 + 14)?;
                    let _name = c.string()?;
                    if tf & 1 != 0 {
                        c.skip(8)?;
                    }
                    let data = if tf & 2 != 0 {
                        let n = c.u32()? as usize;
                        let start = body_start + c.at;
                        c.skip(n)?;
                        Some((start, n))
                    } else {
                        None
                    };
                    tilesets.push(Tileset {
                        id,
                        tw,
                        th,
                        count,
                        zero_is_empty: tf & 4 != 0,
                        data,
                    });
                }
                _ => {}
            }
            cpos += size;
        }
        frames.push((duration, cels));
        pos = frame_end;
    }
    if !new_palette && let Some(p) = old_palette {
        palette = p;
    }
    meta.slices = slices;
    Ok(Sprite {
        w,
        h: ht,
        depth,
        flags,
        transparent,
        layers,
        frames,
        palette,
        tilesets,
        meta,
    })
}

/// グループの入れ子の深さの上限（Aseprite で作れる範囲より十分大きい）。
const MAX_GROUP_DEPTH: usize = 64;

/// 層の木（親と子の表）。
struct Tree {
    parents: Vec<Option<usize>>,
    /// 親ごとの子（層の並びの順）。最後の要素が根の子。
    kids: Vec<Vec<usize>>,
}

impl Sprite {
    /// 層の木を作る。親は、深さが 1 浅い、まだ閉じていない直前のグループ（グループの子はグループの直後に並ぶ）。
    /// 親の見つからない深い層は根の子にする。入れ子が深すぎれば断る。
    fn tree(&self) -> Result<Tree, EngineError> {
        let n = self.layers.len();
        let mut parents = Vec::with_capacity(n);
        let mut kids = vec![Vec::new(); n + 1];
        let mut open: Vec<usize> = Vec::new();
        for (i, l) in self.layers.iter().enumerate() {
            let level = usize::from(l.level);
            if level > MAX_GROUP_DEPTH {
                return Err(EngineError::Limit(format!(
                    "グループの入れ子が {MAX_GROUP_DEPTH} を超えます"
                )));
            }
            open.truncate(level);
            let parent = if level > 0 && open.len() == level {
                open.last().copied()
            } else {
                None
            };
            parents.push(parent);
            kids[parent.unwrap_or(n)].push(i);
            if l.kind == 1 && open.len() == level {
                open.push(i);
            }
        }
        Ok(Tree { parents, kids })
    }

    fn layer_opacity(&self, l: &Layer) -> u8 {
        if self.flags & 1 != 0 { l.opacity } else { 255 }
    }

    fn groups_separately(&self) -> bool {
        self.flags & 2 != 0
    }

    /// 描く層か（自分と親のグループがどれも見え、参照層でない）。親をたどる回数は入れ子の深さの上限まで。
    fn drawn(&self, i: usize, tree: &Tree) -> bool {
        let mut cur = Some(i);
        while let Some(j) = cur {
            let l = &self.layers[j];
            if !l.visible || l.reference {
                return false;
            }
            cur = tree.parents[j];
        }
        true
    }

    fn has_background(&self, tree: &Tree) -> bool {
        self.layers
            .iter()
            .enumerate()
            .any(|(i, l)| l.background && self.drawn(i, tree))
    }

    /// パレットの番号のまま重ねられるか：描く層がどれも通常・不透明度 255、セルも 255、別に合成するグループもない、
    /// かつ描きうる番号の色がどれも不透明（透明の番号は、背景の層がなければ透明として扱うので除く）。
    /// このときだけ、番号のまま重ねた結果を色に広げると、色で合成した結果と同じになる。
    fn index_mode(&self, tree: &Tree) -> bool {
        if self.depth != Depth::Indexed {
            return false;
        }
        let background = self.has_background(tree);
        let opaque = self
            .palette
            .iter()
            .enumerate()
            .all(|(i, c)| c[3] == 255 || (i == usize::from(self.transparent) && !background));
        if !opaque {
            return false;
        }
        for (i, l) in self.layers.iter().enumerate() {
            if !self.drawn(i, tree) {
                continue;
            }
            let groupish = l.kind == 1 && self.groups_separately();
            if (l.kind != 1 || groupish)
                && (l.blend != BlendMode::Normal || self.layer_opacity(l) != 255)
            {
                return false;
            }
        }
        self.frames.iter().all(|(_, cels)| {
            cels.iter().all(|c| {
                c.opacity == 255 || !(c.layer < self.layers.len() && self.drawn(c.layer, tree))
            })
        })
    }

    fn out_format(&self, index_mode: bool) -> PixelFormat {
        match self.depth {
            Depth::Rgba => PixelFormat::Rgba8,
            Depth::Gray => PixelFormat::Rgba8,
            Depth::Indexed if index_mode => PixelFormat::Indexed8,
            Depth::Indexed => PixelFormat::Rgba8,
        }
    }
}

/// 読み取りの準備（形と中身で共通）。
fn prepare(
    src: &mut dyn ReadSeek,
    limits: &Limits,
) -> Result<(Vec<u8>, Sprite, Tree), EngineError> {
    let file = read_all(src, limits)?;
    let s = parse(&file, limits)?;
    let tree = s.tree()?;
    Ok((file, s, tree))
}

impl Decoder for AsepriteCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "aseprite"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let (_, s, tree) = prepare(src, limits)?;
        let fmt = s.out_format(s.index_mode(&tree));
        let (w, h) = (Dim::Exact(u64::from(s.w)), Dim::Exact(u64::from(s.h)));
        Ok(if s.frames.len() == 1 {
            Shape::Image { w, h, fmt }
        } else {
            Shape::Frames {
                count: Dim::Exact(s.frames.len() as u64),
                w,
                h,
                fmt,
                full: Fullness::Full,
            }
        })
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let (file, s, tree) = prepare(src, limits)?;
        let index_mode = s.index_mode(&tree);
        let fmt = s.out_format(index_mode);
        let background = s.has_background(&tree);
        let mut cx = RenderCx {
            s: &s,
            file: &file,
            tree: &tree,
            limits,
            mon,
            caches: Caches::default(),
            frame_bytes: 0,
        };
        let mut out = Vec::with_capacity(s.frames.len());
        for f in 0..s.frames.len() {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            cx.frame_bytes = 0;
            let px = if index_mode {
                let mut canvas = try_vec(s.w as usize * s.h as usize, s.transparent)?;
                cx.render_indexed(f, None, &mut canvas)?;
                let mut colors = s.palette.clone();
                if !background && let Some(c) = colors.get_mut(usize::from(s.transparent)) {
                    c[3] = 0;
                }
                if canvas.iter().any(|i| usize::from(*i) >= colors.len()) {
                    return Err(err("パレットにない番号の画素があります"));
                }
                Raster::from_rows(s.w, s.h, fmt, Some(Palette { colors }), &canvas)?
            } else {
                let mut canvas = try_vec(s.w as usize * s.h as usize * 4, 0u8)?;
                cx.render_group(f, None, &mut canvas)?;
                Raster::from_rows(s.w, s.h, fmt, None, &canvas)?
            };
            out.push((px, s.frames[f].0));
        }
        let meta = s.meta.clone();
        if out.len() == 1 {
            let (px, _) = out.remove(0);
            return Ok(Value::Image(Arc::new(Image { px, meta })));
        }
        Ok(Value::Frames(Arc::new(Frames::full(out, meta, 0)?)))
    }
}

/// 展開したもの（タイルセットは ID ごと、セルは元のフレームと層ごと）。合計は `max_value_bytes` まで持ち、超えたら持たない。
#[derive(Default)]
struct Caches {
    tiles: std::collections::BTreeMap<u32, Arc<Vec<u8>>>,
    cels: std::collections::BTreeMap<(usize, usize), Arc<Vec<u8>>>,
    bytes: u64,
}

/// zlib を、上限で確保した `expect` バイトの領域にちょうど展開する（多くても少なくても壊れたファイル）。
fn inflate(
    file: &[u8],
    (start, len): (usize, usize),
    expect: usize,
) -> Result<Vec<u8>, EngineError> {
    let src = file
        .get(start..start.saturating_add(len))
        .ok_or_else(|| err("圧縮したデータが途中で切れています"))?;
    let mut out = try_vec(expect, 0u8)?;
    let n = miniz_oxide::inflate::decompress_slice_iter_to_slice(
        &mut out,
        std::iter::once(src),
        true,
        false,
    )
    .map_err(|_| err("圧縮したデータが壊れているか、大きすぎます"))?;
    if n != expect {
        return Err(err("展開したデータの大きさが合いません"));
    }
    Ok(out)
}

/// 1 フレームを描く間の状態。
struct RenderCx<'a> {
    s: &'a Sprite,
    file: &'a [u8],
    tree: &'a Tree,
    limits: &'a Limits,
    mon: &'a dyn Monitor,
    caches: Caches,
    /// このフレームで展開したバイト（`max_value_bytes` まで）。
    frame_bytes: u64,
}

impl<'a> RenderCx<'a> {
    /// 展開したバイトを予算から引く。
    fn spend(&mut self, n: usize) -> Result<(), EngineError> {
        self.frame_bytes = self.frame_bytes.saturating_add(n as u64);
        if self.frame_bytes > self.limits.max_value_bytes {
            return Err(EngineError::Limit(
                "1 フレームで展開するデータが大きすぎます".into(),
            ));
        }
        Ok(())
    }

    fn keep(&mut self, n: usize) -> bool {
        let next = self.caches.bytes.saturating_add(n as u64);
        if next > self.limits.max_value_bytes {
            return false;
        }
        self.caches.bytes = next;
        true
    }

    /// フレーム `f` の層 `layer` のセル（リンクをたどる）。元のフレームの番号も返す。
    fn cel_of(&self, f: usize, layer: usize) -> Result<Option<(usize, &'a Cel)>, EngineError> {
        let s: &'a Sprite = self.s;
        let Some(c) = s.frames[f].1.iter().find(|c| c.layer == layer) else {
            return Ok(None);
        };
        if let Content::Linked(to) = c.content {
            let to = usize::from(to);
            let target = s
                .frames
                .get(to)
                .and_then(|(_, cels)| cels.iter().find(|c| c.layer == layer));
            return match target {
                Some(t) if !matches!(t.content, Content::Linked(_)) => Ok(Some((to, t))),
                Some(_) => Err(err("リンクしたセルがリンクを指しています")),
                None => Ok(None),
            };
        }
        Ok(Some((f, c)))
    }

    /// セルの画素（行を詰めたもの）。リンク先などで同じセルを何度も展開しないよう持っておく。
    fn cel_pixels(&mut self, key: (usize, usize), cel: &Cel) -> Result<Arc<Vec<u8>>, EngineError> {
        if let Some(v) = self.caches.cels.get(&key) {
            return Ok(v.clone());
        }
        let Content::Image {
            w,
            h,
            data,
            compressed,
        } = cel.content
        else {
            return Err(err("画像のセルではありません"));
        };
        let limits = self.limits;
        let n = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(self.s.depth.bpp()))
            .filter(|n| {
                (*n as u64) <= limits.max_value_bytes
                    && u64::from(w) <= limits.max_dim
                    && u64::from(h) <= limits.max_dim
            })
            .ok_or_else(|| EngineError::Limit("セルが大きすぎます".into()))?;
        self.spend(n)?;
        let px = Arc::new(if compressed {
            inflate(self.file, data, n)?
        } else {
            self.file
                .get(data.0..data.0 + n)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| err("セルの画素が足りません"))?
        });
        if self.keep(n) {
            self.caches.cels.insert(key, px.clone());
        }
        Ok(px)
    }

    /// セルの、キャンバスに入る画素だけを `visit(x, y, 画素)` に渡す。行ごとに中止を見る。
    fn for_each_cel_pixel(
        &mut self,
        key: (usize, usize),
        cel: &Cel,
        mut visit: impl FnMut(usize, usize, &[u8]),
    ) -> Result<(), EngineError> {
        let s = self.s;
        let bpp = s.depth.bpp();
        let (cw, ch) = (i64::from(s.w), i64::from(s.h));
        match &cel.content {
            Content::Image { w, h, .. } => {
                let (w, h) = (i64::from(*w), i64::from(*h));
                // キャンバスと交わる範囲（セルの座標）
                let (x0, x1) = ((-cel.x).clamp(0, w), (cw - cel.x).clamp(0, w));
                let (y0, y1) = ((-cel.y).clamp(0, h), (ch - cel.y).clamp(0, h));
                if x0 >= x1 || y0 >= y1 {
                    return Ok(());
                }
                let px = self.cel_pixels(key, cel)?;
                for y in y0..y1 {
                    if (y - y0) % 64 == 63 && self.mon.canceled() {
                        return Err(EngineError::Canceled);
                    }
                    let row = (y * w) as usize * bpp;
                    for x in x0..x1 {
                        let p = &px[row + x as usize * bpp..row + (x as usize + 1) * bpp];
                        visit((cel.x + x) as usize, (cel.y + y) as usize, p);
                    }
                }
            }
            Content::Tilemap {
                w,
                h,
                id_mask,
                flip_mask,
                data,
            } => {
                let limits = self.limits;
                let layer = &s.layers[cel.layer];
                let ts = s
                    .tilesets
                    .iter()
                    .find(|t| t.id == layer.tileset)
                    .ok_or_else(|| err("タイルセットがありません"))?;
                let Some(tdata) = ts.data else {
                    return Err(EngineError::Unsupported(
                        "外部のタイルセットは読めません".into(),
                    ));
                };
                let tile_bytes = (ts.tw as usize)
                    .checked_mul(ts.th as usize)
                    .and_then(|n| n.checked_mul(bpp))
                    .ok_or_else(|| EngineError::Limit("タイルが大きすぎます".into()))?;
                let set = match self.caches.tiles.get(&ts.id) {
                    Some(v) => v.clone(),
                    None => {
                        let set_bytes = tile_bytes
                            .checked_mul(ts.count as usize)
                            .filter(|n| (*n as u64) <= limits.max_value_bytes)
                            .ok_or_else(|| {
                                EngineError::Limit("タイルセットが大きすぎます".into())
                            })?;
                        self.spend(set_bytes)?;
                        let v = Arc::new(inflate(self.file, tdata, set_bytes)?);
                        if self.keep(set_bytes) {
                            self.caches.tiles.insert(ts.id, v.clone());
                        }
                        v
                    }
                };
                let map_bytes = (*w as usize)
                    .checked_mul(*h as usize)
                    .and_then(|n| n.checked_mul(4))
                    .filter(|n| {
                        (*n as u64) <= limits.max_value_bytes
                            && u64::from(*w) <= limits.max_dim
                            && u64::from(*h) <= limits.max_dim
                    })
                    .ok_or_else(|| EngineError::Limit("タイルマップが大きすぎます".into()))?;
                self.spend(map_bytes)?;
                let map = inflate(self.file, *data, map_bytes)?;
                let tile = |i: usize| {
                    u32::from_le_bytes([map[i * 4], map[i * 4 + 1], map[i * 4 + 2], map[i * 4 + 3]])
                };
                if (0..map_bytes / 4).any(|i| tile(i) & flip_mask != 0) {
                    return Err(EngineError::Unsupported("タイルの反転は読めません".into()));
                }
                let (tw, th) = (i64::from(ts.tw), i64::from(ts.th));
                if tw == 0 || th == 0 {
                    return Ok(());
                }
                // キャンバスと交わるタイルの範囲
                let tx0 = (-cel.x).div_euclid(tw).clamp(0, i64::from(*w));
                let tx1 = ((cw - cel.x + tw - 1).div_euclid(tw)).clamp(0, i64::from(*w));
                let ty0 = (-cel.y).div_euclid(th).clamp(0, i64::from(*h));
                let ty1 = ((ch - cel.y + th - 1).div_euclid(th)).clamp(0, i64::from(*h));
                for ty in ty0..ty1 {
                    if self.mon.canceled() {
                        return Err(EngineError::Canceled);
                    }
                    for tx in tx0..tx1 {
                        let v = tile((ty * i64::from(*w) + tx) as usize);
                        let id = v & id_mask;
                        let empty = if ts.zero_is_empty {
                            id == 0
                        } else {
                            v == u32::MAX
                        };
                        if empty {
                            continue;
                        }
                        if id >= ts.count {
                            return Err(err("タイルセットにないタイルです"));
                        }
                        let t = &set[id as usize * tile_bytes..(id as usize + 1) * tile_bytes];
                        let (ox, oy) = (cel.x + tx * tw, cel.y + ty * th);
                        let (px0, px1) = ((-ox).clamp(0, tw), (cw - ox).clamp(0, tw));
                        let (py0, py1) = ((-oy).clamp(0, th), (ch - oy).clamp(0, th));
                        for py in py0..py1 {
                            for pxi in px0..px1 {
                                let at = (py * tw + pxi) as usize * bpp;
                                visit((ox + pxi) as usize, (oy + py) as usize, &t[at..at + bpp]);
                            }
                        }
                    }
                }
            }
            Content::Linked(_) => return Err(err("リンクしたセルが残っています")),
        }
        Ok(())
    }

    /// 子の描く順（位置 + z、同じなら z の小さい順）。グループの子は、グループの位置にまとめて描く。
    fn children(&self, f: usize, parent: Option<usize>) -> Vec<usize> {
        let s = self.s;
        let list = &self.tree.kids[parent.unwrap_or(s.layers.len())];
        let mut kids: Vec<(i64, i64, usize)> = list
            .iter()
            .enumerate()
            .map(|(pos, &i)| {
                let z = s.frames[f]
                    .1
                    .iter()
                    .find(|c| c.layer == i)
                    .map_or(0, |c| i64::from(c.z));
                (pos as i64 + z, z, i)
            })
            .collect();
        kids.sort();
        kids.into_iter().map(|k| k.2).collect()
    }

    /// RGBA8 で、`parent` の子を `canvas` に描く。
    fn render_group(
        &mut self,
        f: usize,
        parent: Option<usize>,
        canvas: &mut [u8],
    ) -> Result<(), EngineError> {
        let s = self.s;
        let cw = s.w as usize;
        for i in self.children(f, parent) {
            let l = &s.layers[i];
            if !l.visible || l.reference {
                continue;
            }
            if l.kind == 1 {
                if s.groups_separately() {
                    let mut sub = try_vec(canvas.len(), 0u8)?;
                    self.render_group(f, Some(i), &mut sub)?;
                    let op = s.layer_opacity(l);
                    for (d, src) in canvas.chunks_exact_mut(4).zip(sub.chunks_exact(4)) {
                        let back = [d[0], d[1], d[2], d[3]];
                        d.copy_from_slice(&blend::blend(
                            l.blend,
                            back,
                            [src[0], src[1], src[2], src[3]],
                            op,
                        ));
                    }
                } else {
                    self.render_group(f, Some(i), canvas)?;
                }
                continue;
            }
            let Some((src_frame, cel)) = self.cel_of(f, i)? else {
                continue;
            };
            let op = blend::mul_un8(i32::from(s.layer_opacity(l)), i32::from(cel.opacity)) as u8;
            let (transparent, background, depth) = (s.transparent, l.background, s.depth);
            let pal = &s.palette;
            self.for_each_cel_pixel((src_frame, i), cel, |x, y, p| {
                let src = match depth {
                    Depth::Rgba => [p[0], p[1], p[2], p[3]],
                    Depth::Gray => [p[0], p[0], p[0], p[1]],
                    Depth::Indexed => {
                        // 透明の番号は、パレットの色のアルファを 0 にしたもの（番号のまま読んで広げたときと同じ）
                        let c = pal.get(usize::from(p[0])).copied().unwrap_or([0, 0, 0, 0]);
                        if p[0] == transparent && !background {
                            [c[0], c[1], c[2], 0]
                        } else {
                            c
                        }
                    }
                };
                let at = (y * cw + x) * 4;
                let back = [canvas[at], canvas[at + 1], canvas[at + 2], canvas[at + 3]];
                canvas[at..at + 4].copy_from_slice(&blend::blend(l.blend, back, src, op));
            })?;
        }
        Ok(())
    }

    /// パレットの番号のまま描く（index_mode のときだけ）。グループは子をそのまま重ねる。
    fn render_indexed(
        &mut self,
        f: usize,
        parent: Option<usize>,
        canvas: &mut [u8],
    ) -> Result<(), EngineError> {
        let s = self.s;
        let cw = s.w as usize;
        for i in self.children(f, parent) {
            let l = &s.layers[i];
            if !l.visible || l.reference {
                continue;
            }
            if l.kind == 1 {
                self.render_indexed(f, Some(i), canvas)?;
                continue;
            }
            let Some((src_frame, cel)) = self.cel_of(f, i)? else {
                continue;
            };
            let (transparent, background) = (s.transparent, l.background);
            self.for_each_cel_pixel((src_frame, i), cel, |x, y, p| {
                if p[0] != transparent || background {
                    canvas[y * cw + x] = p[0];
                }
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::testfiles::{AseCel, AseFrame, AseLayer, AseSlice, FileGen};
    use std::io::Cursor;

    fn layer(flags: u16, kind: u16, level: u16, blend: u16, opacity: u8) -> AseLayer {
        AseLayer {
            flags,
            kind,
            level,
            blend,
            opacity,
        }
    }

    fn cel(layer: u16, x: i16, y: i16, w: u16, h: u16) -> AseCel {
        AseCel {
            layer,
            x,
            y,
            opacity: 255,
            z: 0,
            kind: 2,
            w,
            h,
            link: 0,
            flip: false,
        }
    }

    fn sprite(depth: u16, flags: u32, layers: Vec<AseLayer>, frames: Vec<AseFrame>) -> FileGen {
        FileGen::Aseprite {
            w: 8,
            h: 6,
            depth,
            flags,
            transparent: 0,
            palette_len: 8,
            layers,
            frames,
            tileset: None,
            tags: vec![],
            slices: vec![],
            seed: 3,
            opaque_palette: true,
        }
    }

    fn decode(g: &FileGen) -> Result<Value, EngineError> {
        let bytes = g.bytes().unwrap();
        let shape = AsepriteCodec.shape(
            &mut Cursor::new(bytes.clone()),
            "aseprite",
            &DecodeOptions::default(),
            &Limits::default(),
        )?;
        let v = AsepriteCodec.decode(
            &mut Cursor::new(bytes),
            "aseprite",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )?;
        assert!(shape.admits(&v.shape()), "{shape:?} {:?}", v.shape());
        Ok(v)
    }

    /// RGBA に広げる。完全に透明な画素の RGB は見えず合成にも効かないので 0 にそろえる。
    fn rgba(v: &Value) -> Vec<u8> {
        let mut out = match v {
            Value::Image(i) => crate::imaging::convert::to_rgba8_rows(&i.px).unwrap(),
            Value::Frames(f) => crate::imaging::convert::to_rgba8_rows(&f.frames()[0].px).unwrap(),
            Value::Audio(_) => panic!(),
        };
        for p in out.chunks_exact_mut(4) {
            if p[3] == 0 {
                p.fill(0);
            }
        }
        out
    }

    /// 見えないグループの子・参照層は描かない。グループを別に合成しない（印 2 がない）ときは、グループの不透明度を使わない。
    #[test]
    fn hidden_groups_reference_layers_and_flattened_groups() {
        let frames = vec![AseFrame {
            duration: 50,
            cels: vec![cel(0, 0, 0, 8, 6), cel(2, 1, 1, 4, 4)],
        }];
        let only_bottom = decode(&sprite(
            32,
            1,
            vec![layer(1, 0, 0, 0, 255)],
            vec![AseFrame {
                duration: 50,
                cels: vec![cel(0, 0, 0, 8, 6)],
            }],
        ))
        .unwrap();
        // 見えないグループの子
        let hidden = decode(&sprite(
            32,
            1,
            vec![
                layer(1, 0, 0, 0, 255),
                layer(0, 1, 0, 0, 255),
                layer(1, 0, 1, 0, 255),
            ],
            frames.clone(),
        ))
        .unwrap();
        assert_eq!(rgba(&hidden), rgba(&only_bottom));
        // 参照層
        let reference = decode(&sprite(
            32,
            1,
            vec![
                layer(1, 0, 0, 0, 255),
                layer(1, 1, 0, 0, 255),
                layer(1 | 64, 0, 1, 0, 255),
            ],
            frames.clone(),
        ))
        .unwrap();
        assert_eq!(rgba(&reference), rgba(&only_bottom));
        // グループの不透明度 0 は、印 2 がなければ使わない（子はそのまま描かれる）
        let flat = decode(&sprite(
            32,
            1,
            vec![
                layer(1, 0, 0, 0, 255),
                layer(1, 1, 0, 0, 0),
                layer(1, 0, 1, 0, 255),
            ],
            frames.clone(),
        ))
        .unwrap();
        let no_group = decode(&sprite(
            32,
            1,
            vec![
                layer(1, 0, 0, 0, 255),
                layer(1, 0, 0, 0, 255),
                layer(1, 0, 0, 0, 255),
            ],
            frames.clone(),
        ))
        .unwrap();
        assert_eq!(rgba(&flat), rgba(&no_group));
        // 印 2 があれば、不透明度 0 のグループは見えない
        let separate = decode(&sprite(
            32,
            1 | 2,
            vec![
                layer(1, 0, 0, 0, 255),
                layer(1, 1, 0, 0, 0),
                layer(1, 0, 1, 0, 255),
            ],
            frames,
        ))
        .unwrap();
        assert_eq!(rgba(&separate), rgba(&only_bottom));
    }

    /// z-index：同じ親の中で、位置 + z の順に描く（下の層を上に出す）。
    #[test]
    fn z_index_reorders_within_the_parent() {
        // 層 0 のセルに z = 2 を当てると、層 1 の上に描かれる：層ごとに読んだ画素を手で重ねた結果と同じ
        let make = |z: i16, visible: [u16; 2]| {
            let mut bottom = cel(0, 0, 0, 8, 6);
            bottom.z = z;
            bottom.opacity = 255;
            let mut top = cel(1, 0, 0, 8, 6);
            top.opacity = 255;
            sprite(
                32,
                1,
                vec![
                    layer(visible[0], 0, 0, 0, 255),
                    layer(visible[1], 0, 0, 0, 255),
                ],
                vec![AseFrame {
                    duration: 1,
                    cels: vec![bottom, top],
                }],
            )
        };
        let i0 = rgba(&decode(&make(0, [1, 0])).unwrap());
        let i1 = rgba(&decode(&make(0, [0, 1])).unwrap());
        let over = |under: &[u8], top: &[u8]| -> Vec<u8> {
            let v: Vec<u8> = under
                .chunks_exact(4)
                .zip(top.chunks_exact(4))
                .flat_map(|(u, t)| {
                    blend::normal([u[0], u[1], u[2], u[3]], [t[0], t[1], t[2], t[3]], 255)
                })
                .collect();
            visible_only(v)
        };
        assert_eq!(rgba(&decode(&make(0, [1, 1])).unwrap()), over(&i0, &i1));
        assert_eq!(rgba(&decode(&make(2, [1, 1])).unwrap()), over(&i1, &i0));
        // z が負なら下へ：層 1 に z = -2 を当てると層 0 の下
        let mut g = make(0, [1, 1]);
        if let FileGen::Aseprite { frames, .. } = &mut g {
            frames[0].cels[1].z = -2;
        }
        assert_eq!(rgba(&decode(&g).unwrap()), over(&i1, &i0));
    }

    fn visible_only(mut v: Vec<u8>) -> Vec<u8> {
        for p in v.chunks_exact_mut(4) {
            if p[3] == 0 {
                p.fill(0);
            }
        }
        v
    }

    /// グループを別に合成する：子を透明な領域に重ねてから、グループのブレンドと不透明度で下に重ねた手計算と同じ。
    /// 印 1 がなければ層の不透明度は使わない（255 の層と同じ）。
    #[test]
    fn separate_groups_and_layer_opacity_flag() {
        let cels = |opacity: u8| AseFrame {
            duration: 1,
            cels: vec![
                AseCel {
                    opacity,
                    ..cel(0, 0, 0, 8, 6)
                },
                AseCel {
                    opacity,
                    ..cel(2, 1, 1, 6, 4)
                },
                AseCel {
                    opacity,
                    ..cel(3, -1, 2, 7, 4)
                },
            ],
        };
        // 層：0 下地・1 グループ（スクリーン、150）・2 子（通常）・3 子（乗算）
        let layers = |v: [u16; 4], g_blend: u16, g_op: u8| {
            vec![
                layer(v[0], 0, 0, 0, 255),
                layer(v[1], 1, 0, g_blend, g_op),
                layer(v[2], 0, 1, 0, 255),
                layer(v[3], 0, 1, 1, 255),
            ]
        };
        let alone = |v: [u16; 4]| {
            rgba(&decode(&sprite(32, 1, layers(v, 0, 255), vec![cels(255)])).unwrap())
        };
        let (base, c1, c2) = (
            alone([1, 1, 0, 0]),
            alone([0, 1, 1, 0]),
            alone([0, 1, 0, 1]),
        );
        let px = |v: &[u8], i: usize| [v[i * 4], v[i * 4 + 1], v[i * 4 + 2], v[i * 4 + 3]];
        let want: Vec<u8> = (0..48)
            .flat_map(|i| {
                let sub = blend::blend(
                    BlendMode::Multiply,
                    blend::normal([0; 4], px(&c1, i), 255),
                    px(&c2, i),
                    255,
                );
                blend::blend(BlendMode::Screen, px(&base, i), sub, 150)
            })
            .collect();
        let got =
            rgba(&decode(&sprite(32, 1 | 2, layers([1; 4], 2, 150), vec![cels(255)])).unwrap());
        assert_eq!(got, visible_only(want));
        // 印 1 なし：層 0 の不透明度 37 は使わない
        let with_flag = |flags: u32, op: u8| {
            let mut ls = layers([1, 0, 0, 0], 0, 255);
            ls[0].opacity = op;
            rgba(&decode(&sprite(32, flags, ls, vec![cels(255)])).unwrap())
        };
        assert_eq!(with_flag(0, 37), with_flag(1, 255));
        assert_ne!(with_flag(1, 37), with_flag(1, 255));
    }

    /// パレットに不透明でない色があれば（透明の番号のほかに）、番号のまま重ねない。
    #[test]
    fn palette_alpha_forces_rgba() {
        let make = |opaque: bool| FileGen::Aseprite {
            w: 8,
            h: 6,
            depth: 8,
            flags: 1,
            transparent: 0,
            palette_len: 64,
            layers: vec![layer(1, 0, 0, 0, 255), layer(1, 0, 0, 0, 255)],
            frames: vec![AseFrame {
                duration: 1,
                cels: vec![
                    AseCel {
                        opacity: 255,
                        ..cel(0, 0, 0, 8, 6)
                    },
                    AseCel {
                        opacity: 255,
                        ..cel(1, 1, 1, 6, 4)
                    },
                ],
            }],
            tileset: None,
            tags: vec![],
            slices: vec![],
            seed: 3,
            opaque_palette: opaque,
        };
        let fmt = |g: FileGen| match decode(&g).unwrap() {
            Value::Image(i) => i.px.format(),
            _ => panic!(),
        };
        assert_eq!(fmt(make(true)), PixelFormat::Indexed8);
        assert_eq!(fmt(make(false)), PixelFormat::Rgba8);
    }

    /// パレットの絵：通常・不透明度 255 だけなら番号のまま（Indexed8）。背景の層では透明の番号も色。ほかは Rgba8。
    #[test]
    fn indexed_mode_and_background_layers() {
        let frames = vec![AseFrame {
            duration: 1,
            cels: vec![cel(0, 0, 0, 8, 6), cel(1, 2, 1, 3, 3)],
        }];
        let fmt = |v: &Value| match v {
            Value::Image(i) => i.px.format(),
            _ => panic!(),
        };
        let plain = decode(&sprite(
            8,
            1,
            vec![layer(1, 0, 0, 0, 255), layer(1, 0, 0, 0, 255)],
            frames.clone(),
        ))
        .unwrap();
        assert_eq!(fmt(&plain), PixelFormat::Indexed8);
        let Value::Image(img) = &plain else { panic!() };
        assert_eq!(img.px.palette().unwrap().colors[0][3], 0);
        let multiply = decode(&sprite(
            8,
            1,
            vec![layer(1, 0, 0, 0, 255), layer(1, 0, 0, 1, 255)],
            frames.clone(),
        ))
        .unwrap();
        assert_eq!(fmt(&multiply), PixelFormat::Rgba8);
        let faded = decode(&sprite(
            8,
            1,
            vec![layer(1, 0, 0, 0, 255), layer(1, 0, 0, 0, 100)],
            frames.clone(),
        ))
        .unwrap();
        assert_eq!(fmt(&faded), PixelFormat::Rgba8);
        // 印 1 がなければ層の不透明度は使わない → 番号のまま
        let ignored = decode(&sprite(
            8,
            0,
            vec![layer(1, 0, 0, 0, 255), layer(1, 0, 0, 0, 100)],
            frames.clone(),
        ))
        .unwrap();
        assert_eq!(fmt(&ignored), PixelFormat::Indexed8);
        // 背景の層：透明の番号もパレットの色（アルファはそのまま）
        let bg = decode(&sprite(
            8,
            1,
            vec![layer(1 | 8, 0, 0, 0, 255), layer(1, 0, 0, 0, 255)],
            frames,
        ))
        .unwrap();
        let Value::Image(img) = &bg else { panic!() };
        assert_eq!(img.px.palette().unwrap().colors[0][3], 255);
    }

    /// スライス・タグ・リンク・表示時間を読む。
    #[test]
    fn slices_tags_links_and_durations() {
        let mut g = sprite(
            32,
            1,
            vec![layer(1, 0, 0, 0, 255)],
            vec![
                AseFrame {
                    duration: 40,
                    cels: vec![cel(0, 1, 1, 3, 3)],
                },
                AseFrame {
                    duration: 70,
                    cels: vec![AseCel {
                        kind: 1,
                        link: 0,
                        ..cel(0, 5, 5, 1, 1)
                    }],
                },
            ],
        );
        if let FileGen::Aseprite { tags, slices, .. } = &mut g {
            *tags = vec![("walk".into(), 0, 1)];
            *slices = vec![AseSlice {
                name: "hit".into(),
                rect: (1, 2, 4, 3),
                center: Some((1, 1, 2, 1)),
                pivot: Some((2, 3)),
            }];
        }
        let Value::Frames(f) = decode(&g).unwrap() else {
            panic!()
        };
        assert_eq!(
            f.frames().iter().map(|x| x.duration_ms).collect::<Vec<_>>(),
            vec![40, 70]
        );
        // リンクしたセルは同じ絵
        assert_eq!(f.frames()[0].px, f.frames()[1].px);
        assert_eq!(f.meta.tags.len(), 1);
        assert_eq!((f.meta.tags[0].from, f.meta.tags[0].to), (0, 1));
        let s = &f.meta.slices[0];
        assert_eq!(s.rect, IRect::new(1, 2, 4, 3));
        assert_eq!(s.center, Some(IRect::new(2, 3, 2, 1)));
        assert_eq!(s.pivot, Some(IPoint { x: 3, y: 5 }));
    }

    /// タイルの反転は読めない（Unsupported）。展開爆弾・壊れたファイルは落ちずにエラー。
    #[test]
    fn unsupported_bombs_and_broken_files() {
        use rand_chacha::rand_core::Rng;
        let mut flipped = sprite(
            32,
            1,
            vec![layer(1, 2, 0, 0, 255)],
            vec![AseFrame {
                duration: 1,
                cels: vec![AseCel {
                    kind: 3,
                    flip: true,
                    ..cel(0, 0, 0, 2, 2)
                }],
            }],
        );
        if let FileGen::Aseprite { tileset, .. } = &mut flipped {
            *tileset = Some((2, 2, 3));
        }
        assert!(matches!(decode(&flipped), Err(EngineError::Unsupported(_))));
        // 64×64 と言いながら、展開すると大きくなるデータ → 大きさちょうどで止める
        let good = sprite(
            32,
            1,
            vec![layer(1, 0, 0, 0, 255)],
            vec![AseFrame {
                duration: 1,
                cels: vec![cel(0, 0, 0, 8, 6)],
            }],
        )
        .bytes()
        .unwrap();
        let mut r = crate::determinism::rng(1);
        for _ in 0..500 {
            let mut b = good.clone();
            for _ in 0..1 + r.next_u32() % 4 {
                let i = r.next_u32() as usize % b.len();
                b[i] = r.next_u32() as u8;
            }
            let tight = Limits {
                max_value_bytes: 1 << 20,
                max_dim: 4_096,
                ..Limits::default()
            };
            let _ = AsepriteCodec.shape(
                &mut Cursor::new(b.clone()),
                "aseprite",
                &DecodeOptions::default(),
                &tight,
            );
            let _ = AsepriteCodec.decode(
                &mut Cursor::new(b),
                "aseprite",
                &DecodeOptions::default(),
                &tight,
                &NoMonitor,
            );
        }
        let bomb = {
            let px = vec![0u8; 4 * 64 * 64 * 64];
            let z = miniz_oxide::deflate::compress_to_vec_zlib(&px, 9);
            let mut b = good.clone();
            // セルのチャンクを、64×64 と書いた大きな展開のデータに差し替える
            let at = b.windows(2).rposition(|w| w == [0x05, 0x20]).unwrap() - 4;
            b.truncate(at);
            let mut body = Vec::new();
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&[0, 0, 0, 0, 255]);
            body.extend_from_slice(&2u16.to_le_bytes());
            body.extend_from_slice(&[0u8; 7]);
            body.extend_from_slice(&64u16.to_le_bytes());
            body.extend_from_slice(&64u16.to_le_bytes());
            body.extend_from_slice(&z);
            b.extend_from_slice(&(body.len() as u32 + 6).to_le_bytes());
            b.extend_from_slice(&0x2005u16.to_le_bytes());
            b.extend_from_slice(&body);
            // フレームの長さを直す
            let frame_len = (b.len() - 128) as u32;
            b[128..132].copy_from_slice(&frame_len.to_le_bytes());
            b
        };
        assert!(decode_bytes(bomb).is_err());
    }

    fn decode_bytes(b: Vec<u8>) -> Result<Value, EngineError> {
        AsepriteCodec.decode(
            &mut Cursor::new(b),
            "aseprite",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
    }

    /// グループの入れ子が深すぎるファイル・層の深さの値があふれるファイルは、落ちずに断る（スタックをあふれさせない）。
    #[test]
    fn deep_groups_are_refused() {
        let deep: Vec<AseLayer> = (0..200u16).map(|lv| layer(1, 1, lv, 0, 255)).collect();
        let g = sprite(
            32,
            1,
            deep,
            vec![AseFrame {
                duration: 1,
                cels: vec![],
            }],
        );
        assert!(matches!(decode(&g), Err(EngineError::Limit(_))));
        let odd = sprite(
            32,
            1,
            vec![layer(1, 1, 65_535, 0, 255), layer(1, 0, 1, 0, 255)],
            vec![AseFrame {
                duration: 1,
                cels: vec![],
            }],
        );
        assert!(matches!(decode(&odd), Err(EngineError::Limit(_))));
        // 64 段までは読める
        let ok: Vec<AseLayer> = (0..=64u16).map(|lv| layer(1, 1, lv, 0, 255)).collect();
        assert!(
            decode(&sprite(
                32,
                1,
                ok,
                vec![AseFrame {
                    duration: 1,
                    cels: vec![]
                }]
            ))
            .is_ok()
        );
    }

    /// タイルマップの展開は上限で断る。キャンバスの外のタイルは回さない。1 フレームの中でも中止できる。
    #[test]
    fn tilemap_bombs_and_cancel_inside_a_frame() {
        let mut g = sprite(
            32,
            1,
            vec![layer(1, 2, 0, 0, 255)],
            vec![AseFrame {
                duration: 1,
                cels: vec![AseCel {
                    kind: 3,
                    ..cel(0, 0, 0, 520, 520)
                }],
            }],
        );
        if let FileGen::Aseprite { tileset, .. } = &mut g {
            *tileset = Some((1, 1, 2));
        }
        let bytes = g.bytes().unwrap();
        let small = Limits {
            max_value_bytes: 1 << 20,
            ..Limits::default()
        };
        assert!(matches!(
            AsepriteCodec.decode(
                &mut Cursor::new(bytes.clone()),
                "aseprite",
                &DecodeOptions::default(),
                &small,
                &NoMonitor
            ),
            Err(EngineError::Limit(_))
        ));
        // 既定の上限なら読める（キャンバスの 8×6 だけ描く）
        assert!(decode_bytes(bytes).is_ok());
        // 大きなセルを描く途中で中止できる（中止の確認がフレームの切れ目だけでない）
        let big = FileGen::Aseprite {
            w: 128,
            h: 128,
            depth: 32,
            flags: 1,
            transparent: 0,
            palette_len: 8,
            layers: vec![layer(1, 0, 0, 0, 255)],
            frames: vec![AseFrame {
                duration: 1,
                cels: vec![cel(0, 0, 0, 128, 128)],
            }],
            tileset: None,
            tags: vec![],
            slices: vec![],
            seed: 3,
            opaque_palette: true,
        };
        let mon = crate::monitor::CancelAfter::new(2);
        let r = AsepriteCodec.decode(
            &mut Cursor::new(big.bytes().unwrap()),
            "aseprite",
            &DecodeOptions::default(),
            &Limits::default(),
            &mon,
        );
        assert!(matches!(r, Err(EngineError::Canceled)));
    }
}
