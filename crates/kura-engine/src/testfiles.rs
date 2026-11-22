//! テストとゴールデンのためのファイルを、下の層で組む（docs/editing.md §8.5）。
//!
//! 自前のエンコーダが書かない形（1/2/4 ビット、tRNS、インターレース、64 ビットの浮動小数、拡張形式、他のツールのメタデータ、
//! 大きさの分からない data など）を、記述から決定的に作り、デコーダだけの経路をゴールデンで凍結できるようにする。
//! PNG は無圧縮の deflate（stored ブロック）とフィルタなしの行で組むので、圧縮器に頼らない。

use rand_chacha::rand_core::Rng;
use serde::{Deserialize, Serialize};

use crate::determinism;
use crate::error::EngineError;

/// ファイルの作り方。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type",
    deny_unknown_fields
)]
pub enum FileGen {
    /// PNG。`color` は PNG の色の種類（0 グレー・2 RGB・3 パレット・4 グレー＋アルファ・6 RGBA）、`depth` はビット深度。
    Png {
        w: u32,
        h: u32,
        color: u8,
        depth: u8,
        interlace: bool,
        /// パレットの色の数（color 3 のとき）。
        palette_len: u16,
        trns: Option<Vec<u8>>,
        text: Vec<(String, String)>,
        icc: Option<Vec<u8>>,
        /// pHYs（x・y・単位）。
        phys: Option<(u32, u32, u8)>,
        seed: u64,
        /// eXIf（IHDR の直後に置く）。ないときは書かない。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exif: Option<Vec<u8>>,
    },
    /// APNG（8 ビット。`color` は 2 RGB・3 パレット・6 RGBA）。フレームの中身は位置とフレームの番号から決める。
    Apng {
        w: u32,
        h: u32,
        color: u8,
        palette_len: u16,
        trns: Option<Vec<u8>>,
        /// 既定の画像をアニメの外に置く（IDAT の前に fcTL を書かない）。
        separate_default: bool,
        plays: u32,
        frames: Vec<ApngFrame>,
        exif: Option<Vec<u8>>,
        seed: u64,
        /// インターレース（Adam7）で書く。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        interlace: bool,
    },
    /// GIF（gif クレートで書く。行の並べ替えはここで行う）。
    Gif {
        w: u16,
        h: u16,
        /// 大域パレットの色の数（0 はなし）。
        global_len: u16,
        /// NETSCAPE の繰り返し（なしは書かない、0 は無限、n は n 回）。
        repeat: Option<u16>,
        frames: Vec<GifFrame>,
        seed: u64,
    },
    /// aseprite（仕様どおりの自前の書き手）。画素は位置・フレーム・層から決める。
    Aseprite {
        w: u16,
        h: u16,
        /// 32 RGBA・16 グレー・8 パレット。
        depth: u16,
        flags: u32,
        transparent: u8,
        palette_len: u16,
        layers: Vec<AseLayer>,
        frames: Vec<AseFrame>,
        /// タイルセット（タイルの幅・高さ・数）。タイルマップの層はこれを使う。
        tileset: Option<(u16, u16, u32)>,
        tags: Vec<(String, u16, u16)>,
        /// スライス（名前・x・y・w・h、9 分割の中央があれば x・y・w・h、ピボットがあれば x・y）。
        slices: Vec<AseSlice>,
        seed: u64,
        /// パレットの色をすべて不透明にする（ないときは、約 1/7 の色をアルファ 0 にする）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        opaque_palette: bool,
    },
    /// PSD・PSB の合成済みの画像（仕様どおりの自前の書き手）。画素は位置とチャンネルから決める。
    Psd(PsdGen),
    /// 他のツールで作って置いたファイル（`crates/kura-engine/fixtures/<path>`。作り方は fixtures/README.md）。
    /// 形式は拡張子から決める。ファイルは消さない・変えない（追記だけの検査が見る）。
    Fixture { path: String },
    /// WAV。`tag` は形式（1 PCM・3 浮動小数）。
    Wav {
        tag: u16,
        bits: u16,
        channels: u16,
        rate: u32,
        frames: u32,
        extensible: bool,
        /// smpl のループ（始め・終わり。終わりは含む）。
        smpl: Option<(u32, u32)>,
        /// cue（ID・位置）。
        cues: Vec<(u32, u32)>,
        /// adtl の labl（ID・名前）。
        labels: Vec<(u32, String)>,
        /// INFO（4 文字の ID・文字列）。
        info: Vec<(String, String)>,
        /// data の大きさを 0xFFFFFFFF（分からない）と書く（data を最後に置く）。
        unknown_size: bool,
        seed: u64,
    },
    /// アンカー（`codec::anchor`。段 9）：`input` の値を、偽の Platform のデコーダ（`fake-platform-dec/1`・既定の環境）で
    /// 読んだ記録として書いたもの。形式は `anchor`。その場の書き手で書いて読み直すので、読んだ値は作り方の値と必ず同じ
    /// （デコーダのゴールデンは値の作り方を守るだけ。決まったバイト列からの読み取りは凍結したアンカーのフィクスチャで守る）。
    Anchor { input: crate::testgen::InputGen },
}

/// aseprite の層。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AseLayer {
    pub flags: u16,
    /// 0 画像・1 グループ・2 タイルマップ。
    pub kind: u16,
    pub level: u16,
    pub blend: u16,
    pub opacity: u8,
}

/// aseprite のフレーム。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AseFrame {
    pub duration: u16,
    pub cels: Vec<AseCel>,
}

/// aseprite のセル。`kind` は 0 生・1 リンク（`link` のフレーム）・2 圧縮・3 タイルマップ（w・h はタイルの数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AseCel {
    pub layer: u16,
    pub x: i16,
    pub y: i16,
    pub opacity: u8,
    pub z: i16,
    pub kind: u16,
    pub w: u16,
    pub h: u16,
    pub link: u16,
    /// タイルマップで反転の印を立てるか。
    #[serde(default)]
    pub flip: bool,
}

/// aseprite のスライス。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AseSlice {
    pub name: String,
    pub rect: (i32, i32, u32, u32),
    pub center: Option<(i32, i32, u32, u32)>,
    pub pivot: Option<(i32, i32)>,
}

/// PSD の作り方。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PsdGen {
    pub w: u32,
    pub h: u32,
    /// 0 ビットマップ・1 グレー・2 インデックス・3 RGB。
    pub mode: u16,
    pub depth: u16,
    /// 色の後ろに足すチャンネルの数（透明があるときは、その最初がアルファ）。
    pub extra_channels: u16,
    pub psb: bool,
    /// 0 なし・1 RLE・2 ZIP。
    pub compression: u16,
    /// 層の情報に書く層の数（0 なら層とマスクの情報を空にする）。
    pub layer_count: i16,
    /// 全体の追加の情報に Mtrn を書く。
    pub merged_key: bool,
    /// アルファがあるとき、色を白で下塗りして書く（Photoshop のように）。
    pub matted: bool,
    /// 1057（hasRealMergedData）。
    pub real_merged: Option<bool>,
    /// 1047（透明の番号）。
    pub transparent_index: Option<u16>,
    /// 1005（解像度。16.16 の固定小数点と単位 1 インチ・2 センチ）。
    pub resolution: Option<(u32, u16)>,
    /// 1058（EXIF）。
    pub exif: Option<Vec<u8>>,
    pub seed: u64,
}

/// GIF の 1 フレーム。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GifFrame {
    pub left: u16,
    pub top: u16,
    pub w: u16,
    pub h: u16,
    /// 局所パレットの色の数（0 はなし）。
    pub local_len: u16,
    pub transparent: Option<u8>,
    /// 0 Any・1 Keep・2 Background・3 Previous。
    pub dispose: u8,
    pub delay: u16,
    pub interlaced: bool,
}

/// APNG の 1 フレーム（fcTL）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApngFrame {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub delay: (u16, u16),
    /// 0 NONE・1 BACKGROUND・2 PREVIOUS。
    pub dispose: u8,
    /// 0 SOURCE・1 OVER。
    pub blend: u8,
}

impl FileGen {
    /// 形式の ID。
    pub fn format(&self) -> &'static str {
        match self {
            Self::Png { .. } | Self::Apng { .. } => "png",
            Self::Gif { .. } => "gif",
            Self::Aseprite { .. } => "aseprite",
            Self::Psd(_) => "psd",
            Self::Fixture { path } => path
                .rsplit('.')
                .next()
                .and_then(crate::codec::format_by_ext)
                .map_or("", |f| f.id),
            Self::Wav { .. } => "wav",
            Self::Anchor { .. } => crate::codec::anchor::FORMAT,
        }
    }

    /// バイト列を作る。
    pub fn bytes(&self) -> Result<Vec<u8>, EngineError> {
        match self {
            Self::Png {
                w,
                h,
                color,
                depth,
                interlace,
                palette_len,
                trns,
                text,
                icc,
                phys,
                seed,
                exif,
            } => png_bytes(
                *w,
                *h,
                *color,
                *depth,
                *interlace,
                *palette_len,
                trns.as_deref(),
                text,
                icc.as_deref(),
                *phys,
                *seed,
                exif.as_deref(),
            ),
            Self::Apng {
                w,
                h,
                color,
                palette_len,
                trns,
                separate_default,
                plays,
                frames,
                exif,
                seed,
                interlace,
            } => apng_bytes(
                *w,
                *h,
                *color,
                *palette_len,
                trns.as_deref(),
                *separate_default,
                *plays,
                frames,
                exif.as_deref(),
                *seed,
                *interlace,
            ),
            Self::Gif {
                w,
                h,
                global_len,
                repeat,
                frames,
                seed,
            } => gif_bytes(*w, *h, *global_len, *repeat, frames, *seed),
            Self::Aseprite {
                w,
                h,
                depth,
                flags,
                transparent,
                palette_len,
                layers,
                frames,
                tileset,
                tags,
                slices,
                seed,
                opaque_palette,
            } => aseprite_bytes(
                (
                    *w,
                    *h,
                    *depth,
                    *flags,
                    *transparent,
                    *palette_len,
                    *opaque_palette,
                ),
                layers,
                frames,
                *tileset,
                tags,
                slices,
                *seed,
            ),
            Self::Psd(g) => psd_bytes(g),
            Self::Fixture { path } => {
                if path.contains("..") || path.starts_with('/') {
                    return Err(EngineError::Internal(
                        "フィクスチャのパスが正しくありません".into(),
                    ));
                }
                let full = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("fixtures")
                    .join(path);
                std::fs::read(&full)
                    .map_err(|e| EngineError::Internal(format!("{}: {e}", full.display())))
            }
            Self::Wav {
                tag,
                bits,
                channels,
                rate,
                frames,
                extensible,
                smpl,
                cues,
                labels,
                info,
                unknown_size,
                seed,
            } => Ok(wav_bytes(
                *tag,
                *bits,
                *channels,
                *rate,
                *frames,
                *extensible,
                *smpl,
                cues,
                labels,
                info,
                *unknown_size,
                *seed,
            )),
            Self::Anchor { input } => anchor_bytes(input),
        }
    }
}

/// アンカーのバイト列（`FileGen::Anchor`）。書いて読み直して照らしたもの。
fn anchor_bytes(input: &crate::testgen::InputGen) -> Result<Vec<u8>, EngineError> {
    use crate::codec::anchor::{AnchorContext, write_verified};
    let v = input.generate()?;
    let stamp = crate::codec::DecodeStamp {
        codec: "fake-platform-dec/1".into(),
        tier: crate::codec::Tier::Platform,
        reproducible: false,
        value_digest: v.digest()?,
        env: Some(crate::testing::FakePlatformDecoder::default_env()),
    };
    write_verified(
        &v,
        &stamp,
        &AnchorContext::default(),
        &crate::limits::Limits::default(),
        &crate::monitor::NoMonitor,
    )
    .map(|(b, _)| b)
}

/// CRC-32（PNG のチャンク）。
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32（zlib）。
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for x in bytes {
        a = (a + u32::from(*x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// 小さな TrueType のフォント（SVG の文字のテスト用。M11）。`chars`（BMP の字）のそれぞれに、送り幅 1,000・
/// 塗りつぶした正方形（x 100〜700・y 0〜700。1 em = 1,000）の字を当てる。表は cmap（形式 4）・glyf・head・hhea・hmtx・
/// loca・maxp・name（書体名 `family`・スタイル Regular）・post。`chars` は重ならないこと（並べ替えて使う）。
pub fn tiny_ttf(family: &str, chars: &[char]) -> Vec<u8> {
    let be16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_be_bytes());
    let be32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_be_bytes());
    let mut codes: Vec<u16> = chars
        .iter()
        .filter_map(|c| u16::try_from(u32::from(*c)).ok())
        .collect();
    codes.sort();
    codes.dedup();
    let glyphs = codes.len() as u16 + 1;
    // glyf：0 番（.notdef）は空、ほかは正方形 1 つ（4 点・座標は int16 の差分）
    let mut glyf = Vec::new();
    // 0 番は空（始まりと終わりがどちらも 0）
    let mut loca = vec![0u16, 0];
    let square = {
        let mut g = Vec::new();
        for x in [1i16, 100, 0, 700, 700] {
            be16(&mut g, x as u16); // 輪郭の数・xMin・yMin・xMax・yMax
        }
        be16(&mut g, 3); // 輪郭の終わりの点
        be16(&mut g, 0); // 命令の長さ
        g.extend_from_slice(&[1, 1, 1, 1]); // 曲線の上の点・座標は int16
        for dx in [100i16, 0, 600, 0] {
            be16(&mut g, dx as u16);
        }
        for dy in [0i16, 700, 0, -700] {
            be16(&mut g, dy as u16);
        }
        g
    };
    for _ in &codes {
        glyf.extend_from_slice(&square);
        loca.push((glyf.len() / 2) as u16);
    }
    let mut loca_b = Vec::new();
    for o in &loca {
        be16(&mut loca_b, *o);
    }
    let mut head = Vec::new();
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0);
    be32(&mut head, 0x5F0F_3CF5);
    be16(&mut head, 0x000B);
    be16(&mut head, 1000);
    head.extend_from_slice(&[0; 16]);
    for v in [0i16, 0, 1000, 1000] {
        be16(&mut head, v as u16);
    }
    for v in [0u16, 8, 2, 0, 0] {
        be16(&mut head, v); // macStyle・lowestRecPPEM・fontDirectionHint・indexToLocFormat（短い）・glyphDataFormat
    }
    let mut hhea = Vec::new();
    be32(&mut hhea, 0x0001_0000);
    for v in [800i16, -200, 0] {
        be16(&mut hhea, v as u16);
    }
    be16(&mut hhea, 1000);
    for v in [0i16, 0, 1000, 1, 0, 0, 0, 0, 0, 0, 0] {
        be16(&mut hhea, v as u16);
    }
    be16(&mut hhea, glyphs);
    let mut hmtx = Vec::new();
    for _ in 0..glyphs {
        be16(&mut hmtx, 1000);
        be16(&mut hmtx, 100);
    }
    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0001_0000);
    be16(&mut maxp, glyphs);
    for v in [4u16, 1, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0] {
        be16(&mut maxp, v);
    }
    // cmap：Windows の Unicode BMP（3, 1）の形式 4。字ごとに 1 つの区間と、終わりの 0xFFFF
    let seg = codes.len() as u16 + 1;
    let mut sub = Vec::new();
    let mut ends: Vec<u16> = codes.clone();
    ends.push(0xFFFF);
    let mut starts = codes.clone();
    starts.push(0xFFFF);
    let deltas: Vec<u16> = codes
        .iter()
        .enumerate()
        .map(|(i, c)| (i as u16 + 1).wrapping_sub(*c))
        .chain(std::iter::once(1))
        .collect();
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow * 2 <= seg {
        pow *= 2;
        log += 1;
    }
    be16(&mut sub, 4);
    be16(&mut sub, 16 + 8 * seg);
    be16(&mut sub, 0);
    be16(&mut sub, seg * 2);
    be16(&mut sub, pow * 2);
    be16(&mut sub, log);
    be16(&mut sub, seg * 2 - pow * 2);
    for e in &ends {
        be16(&mut sub, *e);
    }
    be16(&mut sub, 0);
    for s in &starts {
        be16(&mut sub, *s);
    }
    for d in &deltas {
        be16(&mut sub, *d);
    }
    for _ in 0..seg {
        be16(&mut sub, 0);
    }
    let mut cmap = Vec::new();
    be16(&mut cmap, 0);
    be16(&mut cmap, 1);
    be16(&mut cmap, 3);
    be16(&mut cmap, 1);
    be32(&mut cmap, 12);
    cmap.extend_from_slice(&sub);
    // name：書体名（1）・スタイル（2）・フルネーム（4）・PostScript 名（6）を UTF-16BE で
    let names = [
        (1u16, family.to_string()),
        (2, "Regular".to_string()),
        (4, format!("{family} Regular")),
        (6, format!("{family}-Regular")),
    ];
    let mut strings = Vec::new();
    let mut name = Vec::new();
    be16(&mut name, 0);
    be16(&mut name, names.len() as u16);
    be16(&mut name, 6 + 12 * names.len() as u16);
    for (id, text) in &names {
        let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        for v in [
            3u16,
            1,
            0x409,
            *id,
            utf16.len() as u16,
            strings.len() as u16,
        ] {
            be16(&mut name, v);
        }
        strings.extend_from_slice(&utf16);
    }
    name.extend_from_slice(&strings);
    let mut post = Vec::new();
    be32(&mut post, 0x0003_0000);
    be32(&mut post, 0);
    be16(&mut post, (-100i16) as u16);
    be16(&mut post, 50);
    post.extend_from_slice(&[0; 20]);
    // 表の並びは名前の順
    let tables: [(&[u8; 4], Vec<u8>); 9] = [
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca_b),
        (b"maxp", maxp),
        (b"name", name),
        (b"post", post),
    ];
    let mut out = Vec::new();
    be32(&mut out, 0x0001_0000);
    be16(&mut out, tables.len() as u16);
    for v in [128u16, 3, 16] {
        be16(&mut out, v); // searchRange・entrySelector・rangeShift（9 表）
    }
    let mut offset = 12 + 16 * tables.len() as u32;
    let mut body = Vec::new();
    for (tag, data) in &tables {
        let checksum = data
            .chunks(4)
            .map(|c| {
                let mut w = [0u8; 4];
                w[..c.len()].copy_from_slice(c);
                u32::from_be_bytes(w)
            })
            .fold(0u32, u32::wrapping_add);
        out.extend_from_slice(*tag);
        be32(&mut out, checksum);
        be32(&mut out, offset);
        be32(&mut out, data.len() as u32);
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * tables.len() as u32 + body.len() as u32;
    }
    out.extend_from_slice(&body);
    out
}

/// 無圧縮の deflate（stored ブロック）で包んだ zlib のバイト列。
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        let last = chunks.peek().is_none();
        out.push(u8::from(last));
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Adam7 の各段の（x の始め・y の始め・x の間隔・y の間隔）。
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

#[allow(clippy::too_many_arguments)]
fn png_bytes(
    w: u32,
    h: u32,
    color: u8,
    depth: u8,
    interlace: bool,
    palette_len: u16,
    trns: Option<&[u8]>,
    text: &[(String, String)],
    icc: Option<&[u8]>,
    phys: Option<(u32, u32, u8)>,
    seed: u64,
    exif: Option<&[u8]>,
) -> Result<Vec<u8>, EngineError> {
    let channels: u32 = match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => {
            return Err(EngineError::Internal(format!(
                "PNG の色の種類 {color} は作れません"
            )));
        }
    };
    if w == 0
        || h == 0
        || !matches!(depth, 1 | 2 | 4 | 8 | 16)
        || (color == 3 && (palette_len == 0 || depth == 16))
    {
        return Err(EngineError::Internal(
            "PNG の大きさ・深度が正しくありません".into(),
        ));
    }
    let mask: u32 = if depth == 16 {
        0xFFFF
    } else {
        (1 << depth) - 1
    };
    // 画素 (x, y) のチャンネル c の値（位置から決まるので、インターレースでも同じ値になる）
    let sample = |x: u32, y: u32, c: u32| -> u32 {
        let mut r = determinism::rng_at(seed, u64::from(y), u128::from(x * channels + c));
        let v = r.next_u32();
        if color == 3 {
            v % u32::from(palette_len)
        } else {
            v & mask
        }
    };
    let pack_row = |xs: &[u32], y: u32| -> Vec<u8> {
        let mut row = vec![0u8]; // フィルタなし
        let mut acc = 0u32;
        let mut nbits = 0u32;
        for &x in xs {
            for c in 0..channels {
                let v = sample(x, y, c);
                if depth == 16 {
                    row.extend_from_slice(&(v as u16).to_be_bytes());
                } else if depth == 8 {
                    row.push(v as u8);
                } else {
                    acc = (acc << depth) | v;
                    nbits += u32::from(depth);
                    if nbits == 8 {
                        row.push(acc as u8);
                        acc = 0;
                        nbits = 0;
                    }
                }
            }
        }
        if nbits > 0 {
            row.push((acc << (8 - nbits)) as u8);
        }
        row
    };
    let mut raw = Vec::new();
    if interlace {
        for (x0, y0, dx, dy) in ADAM7 {
            let xs: Vec<u32> = (x0..w).step_by(dx as usize).collect();
            if xs.is_empty() {
                continue;
            }
            for y in (y0..h).step_by(dy as usize) {
                raw.extend(pack_row(&xs, y));
            }
        }
    } else {
        let xs: Vec<u32> = (0..w).collect();
        for y in 0..h {
            raw.extend(pack_row(&xs, y));
        }
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[depth, color, 0, 0, u8::from(interlace)]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    if let Some(e) = exif {
        png_chunk(&mut out, b"eXIf", e);
    }
    if let Some(icc) = icc {
        let mut d = b"icc\0\0".to_vec();
        d.extend(zlib_stored(icc));
        png_chunk(&mut out, b"iCCP", &d);
    }
    if let Some((x, y, unit)) = phys {
        let mut d = x.to_be_bytes().to_vec();
        d.extend_from_slice(&y.to_be_bytes());
        d.push(unit);
        png_chunk(&mut out, b"pHYs", &d);
    }
    if color == 3 {
        let mut r = determinism::rng_stream(seed, u64::MAX);
        let pal: Vec<u8> = (0..u32::from(palette_len) * 3)
            .map(|_| r.next_u32() as u8)
            .collect();
        png_chunk(&mut out, b"PLTE", &pal);
    }
    if let Some(t) = trns {
        png_chunk(&mut out, b"tRNS", t);
    }
    for (k, v) in text {
        let mut d = k.as_bytes().to_vec();
        d.push(0);
        d.extend_from_slice(v.as_bytes());
        png_chunk(&mut out, b"tEXt", &d);
    }
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// PackBits（同じ値が 3 つ以上続けば繰り返し、ほかはそのまま）。
fn pack_bits(row: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < row.len() {
        let mut run = 1;
        while i + run < row.len() && run < 128 && row[i + run] == row[i] {
            run += 1;
        }
        if run >= 3 {
            out.push((257 - run) as u8);
            out.push(row[i]);
            i += run;
        } else {
            let start = i;
            while i < row.len() && i - start < 128 {
                if i + 2 < row.len() && row[i] == row[i + 1] && row[i] == row[i + 2] {
                    break;
                }
                i += 1;
            }
            out.push((i - start - 1) as u8);
            out.extend_from_slice(&row[start..i]);
        }
    }
    out
}

fn psd_bytes(g: &PsdGen) -> Result<Vec<u8>, EngineError> {
    let color: u16 = if g.mode == 3 { 3 } else { 1 };
    let channels = color + g.extra_channels;
    let alpha = g.extra_channels > 0;
    let row = (g.w as usize * g.depth as usize).div_ceil(8);
    let max: u32 = if g.depth == 16 { 65_535 } else { 255 };
    // チャンネル c の (x, y) の値（ビットマップは 0/1）
    let value = |c: u16, x: u32, y: u32| -> u32 {
        let mut r = determinism::rng_at(g.seed ^ u64::from(c), u64::from(y), u128::from(x));
        let v = r.next_u32();
        match (g.mode, g.depth) {
            (0, _) => v & 1,
            (2, _) => v % 16,
            (_, 16) => v & 0xFFFF,
            _ => v & 0xFF,
        }
    };
    let mut planes: Vec<Vec<u8>> = Vec::new();
    for c in 0..channels {
        let mut plane = vec![0u8; row * g.h as usize];
        for y in 0..g.h {
            for x in 0..g.w {
                let mut v = value(c, x, y);
                // 白の下塗り：c = s·a/M + (M − a)
                if g.matted && alpha && c < color {
                    let a = value(color, x, y);
                    v = (v * a + max / 2) / max + (max - a);
                }
                let at = y as usize * row;
                match g.depth {
                    1 => plane[at + x as usize / 8] |= (v as u8) << (7 - x % 8),
                    16 => plane[at + x as usize * 2..at + x as usize * 2 + 2]
                        .copy_from_slice(&(v as u16).to_be_bytes()),
                    _ => plane[at + x as usize] = v as u8,
                }
            }
        }
        planes.push(plane);
    }
    let len_bytes = |out: &mut Vec<u8>, v: u64| {
        if g.psb {
            out.extend_from_slice(&v.to_be_bytes());
        } else {
            out.extend_from_slice(&(v as u32).to_be_bytes());
        }
    };
    let mut out = b"8BPS".to_vec();
    out.extend_from_slice(&(if g.psb { 2u16 } else { 1 }).to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&channels.to_be_bytes());
    out.extend_from_slice(&g.h.to_be_bytes());
    out.extend_from_slice(&g.w.to_be_bytes());
    out.extend_from_slice(&g.depth.to_be_bytes());
    out.extend_from_slice(&g.mode.to_be_bytes());
    // 色のモードのデータ（インデックスはパレット）
    if g.mode == 2 {
        out.extend_from_slice(&768u32.to_be_bytes());
        let mut r = determinism::rng_stream(g.seed, u64::MAX);
        out.extend((0..768).map(|_| r.next_u32() as u8));
    } else {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    // 画像のリソース
    let mut res = Vec::new();
    let mut resource = |id: u16, data: &[u8]| {
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&id.to_be_bytes());
        res.extend_from_slice(&[0, 0]);
        res.extend_from_slice(&(data.len() as u32).to_be_bytes());
        res.extend_from_slice(data);
        if data.len() % 2 == 1 {
            res.push(0);
        }
    };
    if let Some((fixed, unit)) = g.resolution {
        let mut d = fixed.to_be_bytes().to_vec();
        d.extend_from_slice(&unit.to_be_bytes());
        d.extend_from_slice(&1u16.to_be_bytes());
        d.extend_from_slice(&fixed.to_be_bytes());
        d.extend_from_slice(&unit.to_be_bytes());
        d.extend_from_slice(&1u16.to_be_bytes());
        resource(1005, &d);
    }
    if let Some(t) = g.transparent_index {
        resource(1047, &t.to_be_bytes());
    }
    if let Some(real) = g.real_merged {
        resource(1057, &[0, 0, 0, 1, u8::from(real), 0, 0, 0, 0]);
    }
    if let Some(e) = &g.exif {
        resource(1058, e);
    }
    out.extend_from_slice(&(res.len() as u32).to_be_bytes());
    out.extend(res);
    // 層とマスクの情報
    if g.layer_count == 0 && !g.merged_key {
        len_bytes(&mut out, 0);
    } else {
        let mut lm = Vec::new();
        len_bytes(&mut lm, 2);
        lm.extend_from_slice(&g.layer_count.to_be_bytes());
        lm.extend_from_slice(&0u32.to_be_bytes());
        if g.merged_key {
            lm.extend_from_slice(b"8BIMMtrn");
            len_bytes(&mut lm, 0);
        }
        len_bytes(&mut out, lm.len() as u64);
        out.extend(lm);
    }
    // 画像のデータ
    out.extend_from_slice(&g.compression.to_be_bytes());
    match g.compression {
        1 => {
            let packed: Vec<Vec<u8>> = planes
                .iter()
                .flat_map(|p| p.chunks(row.max(1)).map(pack_bits).collect::<Vec<_>>())
                .collect();
            for r in &packed {
                if g.psb {
                    out.extend_from_slice(&(r.len() as u32).to_be_bytes());
                } else {
                    out.extend_from_slice(&(r.len() as u16).to_be_bytes());
                }
            }
            for r in packed {
                out.extend(r);
            }
        }
        2 => out.extend(miniz_oxide::deflate::compress_to_vec_zlib(
            &planes.concat(),
            6,
        )),
        _ => {
            for p in planes {
                out.extend(p);
            }
        }
    }
    Ok(out)
}

fn ase_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn ase_chunk(out: &mut Vec<u8>, kind: u16, body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32 + 6).to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(body);
}

#[allow(clippy::type_complexity)]
fn aseprite_bytes(
    (w, h, depth, flags, transparent, palette_len, opaque_palette): (
        u16,
        u16,
        u16,
        u32,
        u8,
        u16,
        bool,
    ),
    layers: &[AseLayer],
    frames: &[AseFrame],
    tileset: Option<(u16, u16, u32)>,
    tags: &[(String, u16, u16)],
    slices: &[AseSlice],
    seed: u64,
) -> Result<Vec<u8>, EngineError> {
    let bpp: usize = match depth {
        32 => 4,
        16 => 2,
        8 => 1,
        _ => {
            return Err(EngineError::Internal(
                "aseprite の深さが正しくありません".into(),
            ));
        }
    };
    let zlib = |d: &[u8]| miniz_oxide::deflate::compress_to_vec_zlib(d, 6);
    // 画素の値（流れはフレームと層、位置は画素の番号）
    let pixel = |stream: u64, i: u64| -> Vec<u8> {
        let mut r =
            determinism::rng_at(determinism::splitmix64(seed ^ stream), 0, u128::from(i) * 4);
        let v = [r.next_u32(), r.next_u32(), r.next_u32(), r.next_u32()];
        let alpha = [0u8, 255, 255, 128, 37, 200][(v[3] % 6) as usize];
        match bpp {
            4 => vec![v[0] as u8, v[1] as u8, v[2] as u8, alpha],
            2 => vec![v[0] as u8, alpha],
            _ => vec![(v[0] % u32::from(palette_len.max(1))) as u8],
        }
    };
    let mut out = vec![0u8; 128];
    // ヘッダ
    let mut hdr = Vec::new();
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&0xA5E0u16.to_le_bytes());
    hdr.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    hdr.extend_from_slice(&w.to_le_bytes());
    hdr.extend_from_slice(&h.to_le_bytes());
    hdr.extend_from_slice(&depth.to_le_bytes());
    hdr.extend_from_slice(&flags.to_le_bytes());
    hdr.extend_from_slice(&100u16.to_le_bytes());
    hdr.extend_from_slice(&[0u8; 8]);
    hdr.push(transparent);
    hdr.extend_from_slice(&[0u8; 3]);
    hdr.extend_from_slice(&palette_len.to_le_bytes());
    hdr.extend_from_slice(&[1, 1]);
    hdr.extend_from_slice(&[0u8; 8]);
    out[..hdr.len()].copy_from_slice(&hdr);
    for (k, f) in frames.iter().enumerate() {
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        if k == 0 {
            // パレット（新しい形）
            let mut b = Vec::new();
            let n = u32::from(palette_len.max(1));
            b.extend_from_slice(&n.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&(n - 1).to_le_bytes());
            b.extend_from_slice(&[0u8; 8]);
            let mut r = determinism::rng_stream(seed, u64::MAX);
            for _ in 0..n {
                let v = r.next_u32().to_le_bytes();
                b.extend_from_slice(&0u16.to_le_bytes());
                b.extend_from_slice(&[
                    v[0],
                    v[1],
                    v[2],
                    if v[3].is_multiple_of(7) && !opaque_palette {
                        0
                    } else {
                        255
                    },
                ]);
            }
            let mut c = Vec::new();
            ase_chunk(&mut c, 0x2019, &b);
            chunks.push(c);
            for (i, l) in layers.iter().enumerate() {
                let mut b = Vec::new();
                b.extend_from_slice(&l.flags.to_le_bytes());
                b.extend_from_slice(&l.kind.to_le_bytes());
                b.extend_from_slice(&l.level.to_le_bytes());
                b.extend_from_slice(&[0u8; 4]);
                b.extend_from_slice(&l.blend.to_le_bytes());
                b.push(l.opacity);
                b.extend_from_slice(&[0u8; 3]);
                ase_str(&mut b, &format!("layer{i}"));
                if l.kind == 2 {
                    b.extend_from_slice(&0u32.to_le_bytes());
                }
                let mut c = Vec::new();
                ase_chunk(&mut c, 0x2004, &b);
                chunks.push(c);
            }
            if let Some((tw, th, count)) = tileset {
                let mut px = Vec::new();
                for t in 0..u64::from(count) {
                    for i in 0..u64::from(tw) * u64::from(th) {
                        // タイル 0 は空（透明）
                        if t == 0 {
                            px.extend(std::iter::repeat_n(
                                if bpp == 1 { transparent } else { 0 },
                                bpp,
                            ));
                        } else {
                            px.extend(pixel(0x7000_0000 + t, i));
                        }
                    }
                }
                let data = zlib(&px);
                let mut b = Vec::new();
                b.extend_from_slice(&0u32.to_le_bytes());
                b.extend_from_slice(&(2u32 | 4).to_le_bytes());
                b.extend_from_slice(&count.to_le_bytes());
                b.extend_from_slice(&tw.to_le_bytes());
                b.extend_from_slice(&th.to_le_bytes());
                b.extend_from_slice(&1i16.to_le_bytes());
                b.extend_from_slice(&[0u8; 14]);
                ase_str(&mut b, "tiles");
                b.extend_from_slice(&(data.len() as u32).to_le_bytes());
                b.extend_from_slice(&data);
                let mut c = Vec::new();
                ase_chunk(&mut c, 0x2023, &b);
                chunks.push(c);
            }
            if !tags.is_empty() {
                let mut b = Vec::new();
                b.extend_from_slice(&(tags.len() as u16).to_le_bytes());
                b.extend_from_slice(&[0u8; 8]);
                for (i, (name, from, to)) in tags.iter().enumerate() {
                    b.extend_from_slice(&from.to_le_bytes());
                    b.extend_from_slice(&to.to_le_bytes());
                    b.push((i % 4) as u8);
                    b.extend_from_slice(&(i as u16).to_le_bytes());
                    b.extend_from_slice(&[0u8; 10]);
                    ase_str(&mut b, name);
                }
                let mut c = Vec::new();
                ase_chunk(&mut c, 0x2018, &b);
                chunks.push(c);
            }
            for sl in slices {
                let mut b = Vec::new();
                b.extend_from_slice(&1u32.to_le_bytes());
                let sf = u32::from(sl.center.is_some()) | (u32::from(sl.pivot.is_some()) << 1);
                b.extend_from_slice(&sf.to_le_bytes());
                b.extend_from_slice(&0u32.to_le_bytes());
                ase_str(&mut b, &sl.name);
                b.extend_from_slice(&0u32.to_le_bytes());
                b.extend_from_slice(&sl.rect.0.to_le_bytes());
                b.extend_from_slice(&sl.rect.1.to_le_bytes());
                b.extend_from_slice(&sl.rect.2.to_le_bytes());
                b.extend_from_slice(&sl.rect.3.to_le_bytes());
                if let Some(c) = sl.center {
                    b.extend_from_slice(&c.0.to_le_bytes());
                    b.extend_from_slice(&c.1.to_le_bytes());
                    b.extend_from_slice(&c.2.to_le_bytes());
                    b.extend_from_slice(&c.3.to_le_bytes());
                }
                if let Some(pv) = sl.pivot {
                    b.extend_from_slice(&pv.0.to_le_bytes());
                    b.extend_from_slice(&pv.1.to_le_bytes());
                }
                let mut c = Vec::new();
                ase_chunk(&mut c, 0x2022, &b);
                chunks.push(c);
            }
        }
        for c in &f.cels {
            let mut b = Vec::new();
            b.extend_from_slice(&c.layer.to_le_bytes());
            b.extend_from_slice(&c.x.to_le_bytes());
            b.extend_from_slice(&c.y.to_le_bytes());
            b.push(c.opacity);
            b.extend_from_slice(&c.kind.to_le_bytes());
            b.extend_from_slice(&c.z.to_le_bytes());
            b.extend_from_slice(&[0u8; 5]);
            let stream = ((k as u64) << 16) | u64::from(c.layer);
            match c.kind {
                1 => b.extend_from_slice(&c.link.to_le_bytes()),
                3 => {
                    let (_, _, count) = tileset.unwrap_or((1, 1, 1));
                    b.extend_from_slice(&c.w.to_le_bytes());
                    b.extend_from_slice(&c.h.to_le_bytes());
                    b.extend_from_slice(&32u16.to_le_bytes());
                    b.extend_from_slice(&0x1fff_ffffu32.to_le_bytes());
                    b.extend_from_slice(&0x2000_0000u32.to_le_bytes());
                    b.extend_from_slice(&0x4000_0000u32.to_le_bytes());
                    b.extend_from_slice(&0x8000_0000u32.to_le_bytes());
                    b.extend_from_slice(&[0u8; 10]);
                    let mut map = Vec::new();
                    for i in 0..u64::from(c.w) * u64::from(c.h) {
                        let mut r = determinism::rng_at(stream, 1, u128::from(i));
                        let mut v = r.next_u32() % count.max(1);
                        if c.flip && i == 0 {
                            v |= 0x2000_0000;
                        }
                        map.extend_from_slice(&v.to_le_bytes());
                    }
                    b.extend(zlib(&map));
                }
                kind => {
                    b.extend_from_slice(&c.w.to_le_bytes());
                    b.extend_from_slice(&c.h.to_le_bytes());
                    let px: Vec<u8> = (0..u64::from(c.w) * u64::from(c.h))
                        .flat_map(|i| pixel(stream, i))
                        .collect();
                    if kind == 2 {
                        b.extend(zlib(&px));
                    } else {
                        b.extend(px);
                    }
                }
            }
            let mut ch = Vec::new();
            ase_chunk(&mut ch, 0x2005, &b);
            chunks.push(ch);
        }
        let body: Vec<u8> = chunks.concat();
        let mut fh = Vec::new();
        fh.extend_from_slice(&(body.len() as u32 + 16).to_le_bytes());
        fh.extend_from_slice(&0xF1FAu16.to_le_bytes());
        fh.extend_from_slice(&(chunks.len().min(0xFFFF) as u16).to_le_bytes());
        fh.extend_from_slice(&f.duration.to_le_bytes());
        fh.extend_from_slice(&[0u8; 2]);
        fh.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
        out.extend(fh);
        out.extend(body);
    }
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&size.to_le_bytes());
    Ok(out)
}

fn gif_bytes(
    w: u16,
    h: u16,
    global_len: u16,
    repeat: Option<u16>,
    frames: &[GifFrame],
    seed: u64,
) -> Result<Vec<u8>, EngineError> {
    let bad = |e: gif::EncodingError| EngineError::Internal(format!("GIF を作れません: {e}"));
    let palette = |len: u16, stream: u64| -> Vec<u8> {
        let mut r = determinism::rng_stream(seed, stream);
        (0..u32::from(len) * 3)
            .map(|_| r.next_u32() as u8)
            .collect()
    };
    let mut out = Vec::new();
    {
        let mut enc =
            gif::Encoder::new(&mut out, w, h, &palette(global_len, u64::MAX)).map_err(bad)?;
        match repeat {
            Some(0) => enc.set_repeat(gif::Repeat::Infinite).map_err(bad)?,
            Some(n) => enc.set_repeat(gif::Repeat::Finite(n)).map_err(bad)?,
            None => {}
        }
        for (k, f) in frames.iter().enumerate() {
            let len = if f.local_len > 0 {
                f.local_len
            } else {
                global_len
            };
            if len == 0 {
                return Err(EngineError::Internal(
                    "パレットのない GIF のフレーム".into(),
                ));
            }
            // 行 y の画素（番号）。透明の番号もときどき混ぜる
            let row = |y: u16| -> Vec<u8> {
                (0..f.w)
                    .map(|x| {
                        let mut r = determinism::rng_at(
                            determinism::splitmix64(seed ^ k as u64),
                            u64::from(y),
                            u128::from(x),
                        );
                        let v = r.next_u32();
                        match f.transparent {
                            Some(t) if v.is_multiple_of(4) => t,
                            _ => (v % u32::from(len)) as u8,
                        }
                    })
                    .collect()
            };
            let order: Vec<u16> = if f.interlaced {
                [(0u16, 8u16), (4, 8), (2, 4), (1, 2)]
                    .iter()
                    .flat_map(|(start, step)| (*start..f.h).step_by(usize::from(*step)))
                    .collect()
            } else {
                (0..f.h).collect()
            };
            let buffer: Vec<u8> = order.into_iter().flat_map(row).collect();
            let dispose = match f.dispose {
                1 => gif::DisposalMethod::Keep,
                2 => gif::DisposalMethod::Background,
                3 => gif::DisposalMethod::Previous,
                _ => gif::DisposalMethod::Any,
            };
            let frame = gif::Frame {
                left: f.left,
                top: f.top,
                width: f.w,
                height: f.h,
                delay: f.delay,
                dispose,
                transparent: f.transparent,
                interlaced: f.interlaced,
                palette: (f.local_len > 0).then(|| palette(f.local_len, k as u64)),
                buffer: std::borrow::Cow::Owned(buffer),
                ..gif::Frame::default()
            };
            enc.write_frame(&frame).map_err(bad)?;
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn apng_bytes(
    w: u32,
    h: u32,
    color: u8,
    palette_len: u16,
    trns: Option<&[u8]>,
    separate_default: bool,
    plays: u32,
    frames: &[ApngFrame],
    exif: Option<&[u8]>,
    seed: u64,
    interlace: bool,
) -> Result<Vec<u8>, EngineError> {
    let channels: u32 = match color {
        2 => 3,
        3 => 1,
        6 => 4,
        _ => {
            return Err(EngineError::Internal(format!(
                "APNG の色の種類 {color} は作れません"
            )));
        }
    };
    if w == 0 || h == 0 || frames.is_empty() || (color == 3 && palette_len == 0) {
        return Err(EngineError::Internal(
            "APNG の作り方が正しくありません".into(),
        ));
    }
    // フレーム k の (x, y) の画素（行を詰めたもの。フィルタなし）。インターレースなら Adam7 の段の順に並べる
    let rows = |k: u64, fw: u32, fh: u32| -> Vec<u8> {
        let mut order: Vec<(u32, Vec<u32>)> = Vec::new();
        if interlace {
            for (x0, y0, dx, dy) in ADAM7 {
                let xs: Vec<u32> = (x0..fw).step_by(dx as usize).collect();
                if xs.is_empty() {
                    continue;
                }
                for y in (y0..fh).step_by(dy as usize) {
                    order.push((y, xs.clone()));
                }
            }
        } else {
            for y in 0..fh {
                order.push((y, (0..fw).collect()));
            }
        }
        let mut raw = Vec::new();
        for (y, xs) in order {
            raw.push(0);
            for x in xs {
                for c in 0..channels {
                    let mut r = determinism::rng_at(
                        determinism::splitmix64(seed ^ k),
                        u64::from(y),
                        u128::from(x * channels + c),
                    );
                    let v = r.next_u32();
                    raw.push(if color == 3 {
                        (v % u32::from(palette_len)) as u8
                    } else if channels == 4 && c == 3 {
                        // アルファは 0・255・半透明が混ざるように
                        [0u8, 255, 255, 128, 37][(v % 5) as usize]
                    } else {
                        v as u8
                    });
                }
            }
        }
        raw
    };
    let fctl = |seq: u32, f: &ApngFrame| -> Vec<u8> {
        let mut d = seq.to_be_bytes().to_vec();
        for v in [f.w, f.h, f.x, f.y] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        d.extend_from_slice(&f.delay.0.to_be_bytes());
        d.extend_from_slice(&f.delay.1.to_be_bytes());
        d.push(f.dispose);
        d.push(f.blend);
        d
    };
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, color, 0, 0, u8::from(interlace)]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    let mut actl = (frames.len() as u32).to_be_bytes().to_vec();
    actl.extend_from_slice(&plays.to_be_bytes());
    png_chunk(&mut out, b"acTL", &actl);
    if let Some(e) = exif {
        png_chunk(&mut out, b"eXIf", e);
    }
    if color == 3 {
        let mut r = determinism::rng_stream(seed, u64::MAX);
        let pal: Vec<u8> = (0..u32::from(palette_len) * 3)
            .map(|_| r.next_u32() as u8)
            .collect();
        png_chunk(&mut out, b"PLTE", &pal);
    }
    if let Some(t) = trns {
        png_chunk(&mut out, b"tRNS", t);
    }
    let mut seq = 0u32;
    let mut rest = frames;
    if separate_default {
        png_chunk(&mut out, b"IDAT", &zlib_stored(&rows(u64::MAX, w, h)));
    } else {
        let f = &frames[0];
        if (f.x, f.y, f.w, f.h) != (0, 0, w, h) {
            return Err(EngineError::Internal(
                "既定の画像のフレームはキャンバスいっぱい".into(),
            ));
        }
        png_chunk(&mut out, b"fcTL", &fctl(seq, f));
        seq += 1;
        png_chunk(&mut out, b"IDAT", &zlib_stored(&rows(0, w, h)));
        rest = &frames[1..];
    }
    for (i, f) in rest.iter().enumerate() {
        let k = (frames.len() - rest.len() + i) as u64;
        png_chunk(&mut out, b"fcTL", &fctl(seq, f));
        seq += 1;
        let mut d = seq.to_be_bytes().to_vec();
        seq += 1;
        d.extend(zlib_stored(&rows(k, f.w, f.h)));
        png_chunk(&mut out, b"fdAT", &d);
    }
    png_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn wav_bytes(
    tag: u16,
    bits: u16,
    channels: u16,
    rate: u32,
    frames: u32,
    extensible: bool,
    smpl: Option<(u32, u32)>,
    cues: &[(u32, u32)],
    labels: &[(u32, String)],
    info: &[(String, String)],
    unknown_size: bool,
    seed: u64,
) -> Vec<u8> {
    let sample_bytes = u32::from(bits / 8);
    let block = u32::from(channels) * sample_bytes;
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&(if extensible { 0xFFFEu16 } else { tag }).to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&rate.wrapping_mul(block).to_le_bytes());
    fmt.extend_from_slice(&(block as u16).to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if extensible {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&0u32.to_le_bytes());
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    let mut data = Vec::with_capacity((frames * block) as usize);
    for f in 0..frames {
        for c in 0..channels {
            let mut r = determinism::rng_at(seed, u64::from(c), u128::from(f) * 2);
            let raw = r.next_u64();
            match (tag, bits) {
                (3, 32) => data
                    .extend_from_slice(&(((raw >> 40) as f32 / 8_388_608.0) - 1.0).to_le_bytes()),
                (3, 64) => data.extend_from_slice(
                    &(((raw >> 11) as f64 / 4_503_599_627_370_496.0) - 1.0).to_le_bytes(),
                ),
                _ => data.extend_from_slice(&raw.to_le_bytes()[..sample_bytes as usize]),
            }
        }
    }
    let mut chunks: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"fmt ", fmt)];
    if let Some((start, end)) = smpl {
        let mut b = Vec::new();
        for v in [0u32, 0, 22_675, 60, 0, 0, 0, 1, 0, 0, 0, start, end, 0, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        chunks.push((b"smpl", b));
    }
    if !cues.is_empty() {
        let mut b = (cues.len() as u32).to_le_bytes().to_vec();
        for (id, pos) in cues {
            b.extend_from_slice(&id.to_le_bytes());
            b.extend_from_slice(&pos.to_le_bytes());
            b.extend_from_slice(b"data");
            for v in [0u32, 0, *pos] {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        chunks.push((b"cue ", b));
    }
    let sub = |list: &mut Vec<u8>, id: &[u8], body: &[u8]| {
        list.extend_from_slice(id);
        list.extend_from_slice(&(body.len() as u32).to_le_bytes());
        list.extend_from_slice(body);
        if body.len() % 2 == 1 {
            list.push(0);
        }
    };
    if !labels.is_empty() {
        let mut b = b"adtl".to_vec();
        for (id, name) in labels {
            let mut body = id.to_le_bytes().to_vec();
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            sub(&mut b, b"labl", &body);
        }
        chunks.push((b"LIST", b));
    }
    if !info.is_empty() {
        let mut b = b"INFO".to_vec();
        for (id, text) in info {
            let mut body = text.as_bytes().to_vec();
            body.push(0);
            let id4: Vec<u8> = id.bytes().chain(std::iter::repeat(b' ')).take(4).collect();
            sub(&mut b, &id4, &body);
        }
        chunks.push((b"LIST", b));
    }
    let mut body = b"WAVE".to_vec();
    for (id, b) in &chunks {
        sub(&mut body, *id, b);
    }
    body.extend_from_slice(b"data");
    body.extend_from_slice(
        &(if unknown_size {
            u32::MAX
        } else {
            data.len() as u32
        })
        .to_le_bytes(),
    );
    body.extend_from_slice(&data);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let z = zlib_stored(&[1, 2, 3]);
        assert_eq!(&z[..2], &[0x78, 0x01]);
    }
}
