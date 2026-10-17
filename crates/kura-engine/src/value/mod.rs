//! 値（docs/editing.md §5）。操作の入出力。中身は Arc で共有し、書き換えるときだけ複製する。

pub mod meta;
pub mod pixel;
pub mod raster;

use std::sync::Arc;

pub use meta::{
    AnimTag, AudioMeta, AudioSource, Grid, ImageMeta, LoopRange, Marker, Slice, TagDirection,
};
pub use pixel::{Palette, PixelFormat};
pub use raster::{Raster, RasterBuilder, TILE};

use crate::digest::{Digest, Hasher, canonical};
use crate::error::EngineError;
use crate::geom::{IPoint, Size};
use crate::shape::{Dim, Fullness, Shape, ValueKind};

/// 1 枚の画像。
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub px: Raster,
    pub meta: ImageMeta,
}

/// アニメーション・スプライトの 1 フレーム。共通のキャンバスの中の `offset` の位置に置く。
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub px: Raster,
    /// キャンバスの中の左上（0 以上）。トリムしたスプライトでは 0 でない。
    pub offset: IPoint,
    /// 表示時間（ミリ秒）。
    pub duration_ms: u32,
}

impl Frame {
    /// キャンバスいっぱいのフレーム（offset 0）。
    pub fn full(px: Raster, duration_ms: u32) -> Self {
        Self {
            px,
            offset: IPoint { x: 0, y: 0 },
            duration_ms,
        }
    }
}

/// フレームの列（docs/editing.md §5.1）。すべてのフレームは共通のキャンバスの中に置き、形式・パレットがそろっている。
/// フレームごとに大きさ・位置が違ってよい（トリムしたスプライト）。メタデータ（格子・ピボット）はキャンバスの座標。
#[derive(Debug, Clone, PartialEq)]
pub struct Frames {
    frames: Vec<Frame>,
    canvas: Size,
    pub meta: ImageMeta,
    /// 繰り返しの回数（0 は無限）。
    pub loop_count: u32,
}

impl Frames {
    /// フレームの列を作る。空、形式・パレットがそろっていない、キャンバスからはみ出すフレームがあればエラー。
    pub fn new(
        frames: Vec<Frame>,
        canvas: Size,
        meta: ImageMeta,
        loop_count: u32,
    ) -> Result<Self, EngineError> {
        let Some(first) = frames.first() else {
            return Err(EngineError::Internal("フレームが 1 つもありません".into()));
        };
        if canvas.w == 0 || canvas.h == 0 {
            return Err(EngineError::Internal("キャンバスの大きさが 0 です".into()));
        }
        let (fmt, palette) = (first.px.format(), first.px.palette().cloned());
        for f in &frames {
            if f.px.format() != fmt || f.px.palette().cloned() != palette {
                return Err(EngineError::Internal(
                    "フレームの形式・パレットがそろっていません".into(),
                ));
            }
            let inside = f.offset.x >= 0
                && f.offset.y >= 0
                && f.offset.x.saturating_add(i64::from(f.px.width())) <= i64::from(canvas.w)
                && f.offset.y.saturating_add(i64::from(f.px.height())) <= i64::from(canvas.h);
            if !inside {
                return Err(EngineError::Internal(
                    "フレームがキャンバスからはみ出しています".into(),
                ));
            }
        }
        Ok(Self {
            frames,
            canvas,
            meta,
            loop_count,
        })
    }

    /// キャンバスいっぱいのフレームの列（すべて同じ大きさ。キャンバスはその大きさ）。
    pub fn full(
        frames: Vec<(Raster, u32)>,
        meta: ImageMeta,
        loop_count: u32,
    ) -> Result<Self, EngineError> {
        let Some((first, _)) = frames.first() else {
            return Err(EngineError::Internal("フレームが 1 つもありません".into()));
        };
        let canvas = Size {
            w: first.width(),
            h: first.height(),
        };
        if frames
            .iter()
            .any(|(px, _)| px.width() != canvas.w || px.height() != canvas.h)
        {
            return Err(EngineError::Internal(
                "フレームの大きさがそろっていません".into(),
            ));
        }
        Self::new(
            frames
                .into_iter()
                .map(|(px, ms)| Frame::full(px, ms))
                .collect(),
            canvas,
            meta,
            loop_count,
        )
    }

    /// フレーム。
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// 共通のキャンバスの大きさ。
    pub fn canvas(&self) -> Size {
        self.canvas
    }

    /// どのフレームもキャンバスいっぱいなら true（フレームごとに持ち上げられる）。
    pub fn is_full(&self) -> bool {
        self.frames.iter().all(|f| {
            f.offset == IPoint { x: 0, y: 0 }
                && f.px.width() == self.canvas.w
                && f.px.height() == self.canvas.h
        })
    }

    /// 最初のフレーム（形式の代表）。
    pub fn first(&self) -> &Frame {
        // new で空でないことを確かめている
        &self.frames[0]
    }
}

/// 音声。planar f32（チャンネルごとの配列）。すべてのチャンネルは同じ長さ。
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    rate: u32,
    planes: Vec<Arc<Vec<f32>>>,
    pub meta: AudioMeta,
}

impl Audio {
    /// 音声を作る。チャンネルが 0 か 65,535 を超える、長さがそろっていない、レートが 0 ならエラー。
    pub fn new(
        rate: u32,
        planes: Vec<Arc<Vec<f32>>>,
        meta: AudioMeta,
    ) -> Result<Self, EngineError> {
        if rate == 0 || planes.is_empty() || planes.len() > usize::from(u16::MAX) {
            return Err(EngineError::Internal(
                "音声のレート・チャンネルの数が正しくありません".into(),
            ));
        }
        let len = planes[0].len();
        if planes.iter().any(|p| p.len() != len) {
            return Err(EngineError::Internal(
                "チャンネルの長さがそろっていません".into(),
            ));
        }
        Ok(Self { rate, planes, meta })
    }

    /// サンプルレート。
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// チャンネル数（1〜65,535）。
    pub fn channels(&self) -> usize {
        self.planes.len()
    }

    /// 1 チャンネルあたりのフレーム数。
    pub fn frames(&self) -> usize {
        self.planes[0].len()
    }

    /// チャンネルの中身。
    pub fn planes(&self) -> &[Arc<Vec<f32>>] {
        &self.planes
    }
}

/// 操作の入出力の値。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Image(Arc<Image>),
    Frames(Arc<Frames>),
    Audio(Arc<Audio>),
}

impl Value {
    /// 値の種類。
    pub fn kind(&self) -> ValueKind {
        match self {
            Self::Image(_) => ValueKind::Image,
            Self::Frames(_) => ValueKind::Frames,
            Self::Audio(_) => ValueKind::Audio,
        }
    }

    /// 実際の形。
    pub fn shape(&self) -> Shape {
        match self {
            Self::Image(img) => Shape::Image {
                w: Dim::Exact(u64::from(img.px.width())),
                h: Dim::Exact(u64::from(img.px.height())),
                fmt: img.px.format(),
            },
            Self::Frames(fr) => Shape::Frames {
                count: Dim::Exact(fr.frames().len() as u64),
                w: Dim::Exact(u64::from(fr.canvas().w)),
                h: Dim::Exact(u64::from(fr.canvas().h)),
                fmt: fr.first().px.format(),
                full: if fr.is_full() {
                    Fullness::Full
                } else {
                    Fullness::Trimmed
                },
            },
            Self::Audio(a) => Shape::Audio {
                rate: a.rate(),
                channels: u16::try_from(a.channels()).unwrap_or(u16::MAX),
                frames: Dim::Exact(a.frames() as u64),
            },
        }
    }

    /// エンコードする前の中身のダイジェスト（docs/editing.md §8.6 の式。凍結する）。
    pub fn digest(&self) -> Result<Digest, EngineError> {
        match self {
            Self::Image(img) => {
                let mut h = Hasher::new("kura-img-v1");
                img.px.digest_into(&mut h);
                image_meta_into(&img.meta, &mut h)?;
                Ok(h.finish())
            }
            Self::Frames(fr) => {
                let mut h = Hasher::new("kura-frames-v1");
                h.u32(fr.frames().len() as u32)
                    .u32(fr.loop_count)
                    .u32(fr.canvas().w)
                    .u32(fr.canvas().h);
                for f in fr.frames() {
                    h.u32(f.duration_ms)
                        .u64(f.offset.x as u64)
                        .u64(f.offset.y as u64);
                    f.px.digest_into(&mut h);
                }
                image_meta_into(&fr.meta, &mut h)?;
                Ok(h.finish())
            }
            Self::Audio(a) => audio_digest(a, &mut || Ok(())),
        }
    }
}

/// 音声の値のダイジェスト（[`Value::digest`] の `kura-pcm-v1` の式そのもの。`Value::digest` は止めない確かめでこれを
/// 呼ぶ）。標本を [`DIGEST_CHUNK`] 個ずつ足し、各チャンネルの各塊を足す前に `check` を呼ぶ。`check` が `Err` を返したら
/// そこで止めてそのエラーを返す。長い音声のダイジェスト（ステレオ 3 時間で 2 秒ほど）の途中でも呼び手が中止を確かめる
/// ための口で、`check` は値に効かない。`check` を呼ぶ数は Σ_チャンネル ceil(フレーム数 / [`DIGEST_CHUNK`])。
pub(crate) fn audio_digest(
    a: &Audio,
    check: &mut dyn FnMut() -> Result<(), EngineError>,
) -> Result<Digest, EngineError> {
    let mut h = Hasher::new("kura-pcm-v1");
    h.u32(a.rate())
        .u32(a.channels() as u32)
        .u64(a.frames() as u64);
    // 標本のビット列を DIGEST_CHUNK 個ずつまとめて足す（BLAKE3 は流れのハッシュなので、まとめ方で値は
    // 変わらない。4 バイトずつ足すと 3 分のステレオで 0.15 秒かかっていた）
    let mut buf = Vec::with_capacity(DIGEST_CHUNK * 4);
    for plane in a.planes() {
        for chunk in plane.chunks(DIGEST_CHUNK) {
            check()?;
            buf.clear();
            for s in chunk {
                // NaN は 1 つの値にそろえる
                let bits = if s.is_nan() { 0x7fc0_0000 } else { s.to_bits() };
                buf.extend_from_slice(&bits.to_le_bytes());
            }
            h.raw(&buf);
        }
    }
    h.bytes_with_len(&canonical(&a.meta)?);
    Ok(h.finish())
}

/// 音声のダイジェストで標本をまとめて足す数（速さと、[`audio_digest`] が確かめを呼ぶ間隔のためだけ。値に効かない）。
pub(crate) const DIGEST_CHUNK: usize = 16 * 1024;

/// 画像のメタデータをダイジェストに足す：正規の JSON ‖ ICC ‖ 拡張。
/// 拡張は、あるものだけを番号の順に（番号 u8 ‖ 長さ付きのバイト列）書く。ないときは何も書かないので、
/// 拡張を足す前の値のダイジェストは変わらない（ICC までは長さで区切れるので、後ろの有無で区別できる）。
/// - 1：EXIF
fn image_meta_into(meta: &ImageMeta, h: &mut Hasher) -> Result<(), EngineError> {
    h.bytes_with_len(&canonical(meta)?);
    match &meta.icc {
        Some(icc) => {
            h.u8(1).bytes_with_len(icc);
        }
        None => {
            h.u8(0);
        }
    }
    if let Some(exif) = &meta.exif {
        h.u8(1).bytes_with_len(exif);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_sees_pixels_meta_and_kind() {
        let px = Raster::filled(3, 2, PixelFormat::Rgba8, None, &[1, 2, 3, 4]).unwrap();
        let a = Value::Image(Arc::new(Image {
            px: px.clone(),
            meta: ImageMeta::default(),
        }));
        let meta = ImageMeta {
            pivot: Some(IPoint { x: 1, y: 1 }),
            ..ImageMeta::default()
        };
        let b = Value::Image(Arc::new(Image {
            px: px.clone(),
            meta,
        }));
        let c = Value::Frames(Arc::new(
            Frames::full(vec![(px.clone(), 100)], ImageMeta::default(), 0).unwrap(),
        ));
        let d = Value::Frames(Arc::new(
            Frames::new(
                vec![Frame {
                    px,
                    offset: IPoint { x: 1, y: 0 },
                    duration_ms: 100,
                }],
                Size { w: 4, h: 2 },
                ImageMeta::default(),
                0,
            )
            .unwrap(),
        ));
        let (da, db, dc, dd) = (
            a.digest().unwrap(),
            b.digest().unwrap(),
            c.digest().unwrap(),
            d.digest().unwrap(),
        );
        assert_ne!(da, db);
        assert_ne!(da, dc);
        assert_ne!(dc, dd);
        assert_eq!(da, a.clone().digest().unwrap());
    }

    /// ダイジェストの式を凍結する（docs/editing.md §8.6。変わったらゴールデンもすべて変わる）。
    #[test]
    fn digest_formula_is_frozen() {
        let px =
            Raster::from_rows(2, 1, PixelFormat::Rgba8, None, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let img = Value::Image(Arc::new(Image {
            px,
            meta: ImageMeta::default(),
        }));
        let mut h = Hasher::new("kura-img-v1");
        h.u32(2)
            .u32(1)
            .u8(PixelFormat::Rgba8.code())
            .u8(0)
            .raw(&[1, 2, 3, 4, 5, 6, 7, 8]);
        h.bytes_with_len(br#"{"dpi":null,"grid":null,"pivot":null,"slices":[],"text":{}}"#)
            .u8(0);
        assert_eq!(img.digest().unwrap(), h.finish());
        // 拡張（EXIF）は、あるときだけ後ろに足す
        let Value::Image(base) = &img else { panic!() };
        let mut with_exif = (**base).clone();
        with_exif.meta.exif = Some(Arc::new(vec![9, 8, 7]));
        let mut h = Hasher::new("kura-img-v1");
        h.u32(2)
            .u32(1)
            .u8(PixelFormat::Rgba8.code())
            .u8(0)
            .raw(&[1, 2, 3, 4, 5, 6, 7, 8]);
        h.bytes_with_len(br#"{"dpi":null,"grid":null,"pivot":null,"slices":[],"text":{}}"#)
            .u8(0)
            .u8(1)
            .bytes_with_len(&[9, 8, 7]);
        assert_eq!(
            Value::Image(Arc::new(with_exif)).digest().unwrap(),
            h.finish()
        );
    }

    /// 音声のダイジェストの式（docs/editing.md §8.6）：標本を 1 つずつ足したものと同じ（まとめる区切りをまたぐ長さで）。
    #[test]
    fn audio_digest_formula_is_frozen() {
        let planes: Vec<Vec<f32>> = (0..2)
            .map(|c| {
                (0..DIGEST_CHUNK * 2 + 37)
                    .map(|i| {
                        if i == 5 {
                            f32::NAN
                        } else {
                            (i * (c + 1)) as f32 * 1e-3 - 0.5
                        }
                    })
                    .collect()
            })
            .collect();
        let a = Audio::new(
            44_100,
            planes.iter().cloned().map(Arc::new).collect(),
            AudioMeta::default(),
        )
        .unwrap();
        let mut h = Hasher::new("kura-pcm-v1");
        h.u32(44_100).u32(2).u64(planes[0].len() as u64);
        for p in &planes {
            for s in p {
                let bits = if s.is_nan() {
                    0x7fc0_0000_u32
                } else {
                    s.to_bits()
                };
                h.raw(&bits.to_le_bytes());
            }
        }
        h.bytes_with_len(&canonical(&a.meta).unwrap());
        assert_eq!(Value::Audio(Arc::new(a)).digest().unwrap(), h.finish());
    }

    /// 確かめを呼ぶ口（[`audio_digest`]）：値は [`Value::digest`] と同じ（確かめは値に効かない）。確かめはチャンネルごとに
    /// 塊（[`DIGEST_CHUNK`] 個）の数だけ呼ばれ、`Err` を返すとそこで止まってそのエラーを返す（残りの塊は読まない）。
    #[test]
    fn audio_digest_checks_between_chunks_without_changing_the_value() {
        for (channels, frames) in [
            (1usize, 0usize),
            (2, 1),
            (3, DIGEST_CHUNK),
            (2, DIGEST_CHUNK * 2 + 37),
        ] {
            let planes: Vec<Arc<Vec<f32>>> = (0..channels)
                .map(|c| Arc::new((0..frames).map(|i| (i + c) as f32 * 1e-4).collect()))
                .collect();
            let a = Audio::new(48_000, planes, AudioMeta::default()).unwrap();
            let mut calls = 0usize;
            let got = audio_digest(&a, &mut || {
                calls += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!(got, Value::Audio(Arc::new(a.clone())).digest().unwrap());
            assert_eq!(
                calls,
                channels * frames.div_ceil(DIGEST_CHUNK),
                "{channels} ch・{frames}"
            );
            // 途中（最後の塊の前）で Err を返すと、そこで止まる
            if calls >= 2 {
                let mut n = 0usize;
                let r = audio_digest(&a, &mut || {
                    n += 1;
                    if n == calls {
                        Err(EngineError::Canceled)
                    } else {
                        Ok(())
                    }
                });
                assert!(matches!(r, Err(EngineError::Canceled)));
                assert_eq!(n, calls);
            }
        }
    }

    #[test]
    fn audio_digest_folds_nan() {
        let a = Audio::new(
            48_000,
            vec![Arc::new(vec![f32::NAN, 0.5])],
            AudioMeta::default(),
        )
        .unwrap();
        let b = Audio::new(
            48_000,
            vec![Arc::new(vec![f32::from_bits(0x7fc0_0001), 0.5])],
            AudioMeta::default(),
        )
        .unwrap();
        assert_eq!(
            Value::Audio(Arc::new(a)).digest().unwrap(),
            Value::Audio(Arc::new(b)).digest().unwrap()
        );
    }

    #[test]
    fn invariants_are_checked() {
        let a = Raster::filled(2, 2, PixelFormat::Gray8, None, &[0]).unwrap();
        let b = Raster::filled(3, 2, PixelFormat::Gray8, None, &[0]).unwrap();
        let c = Raster::filled(3, 2, PixelFormat::Rgb8, None, &[0, 0, 0]).unwrap();
        assert!(Frames::full(vec![], ImageMeta::default(), 0).is_err());
        assert!(
            Frames::full(
                vec![(a.clone(), 1), (b.clone(), 1)],
                ImageMeta::default(),
                0
            )
            .is_err()
        );
        let canvas = Size { w: 3, h: 2 };
        // 大きさの違うフレームはキャンバスの中なら置ける
        let ok = Frames::new(
            vec![Frame::full(a.clone(), 1), Frame::full(b.clone(), 1)],
            canvas,
            ImageMeta::default(),
            0,
        )
        .unwrap();
        assert!(!ok.is_full());
        assert!(
            Frames::new(
                vec![Frame {
                    px: a.clone(),
                    offset: IPoint { x: 2, y: 0 },
                    duration_ms: 1
                }],
                canvas,
                ImageMeta::default(),
                0
            )
            .is_err()
        );
        assert!(
            Frames::new(
                vec![Frame {
                    px: a,
                    offset: IPoint { x: -1, y: 0 },
                    duration_ms: 1
                }],
                canvas,
                ImageMeta::default(),
                0
            )
            .is_err()
        );
        assert!(
            Frames::new(
                vec![Frame::full(b, 1), Frame::full(c, 1)],
                canvas,
                ImageMeta::default(),
                0
            )
            .is_err()
        );
        assert!(Audio::new(0, vec![Arc::new(vec![])], AudioMeta::default()).is_err());
        assert!(
            Audio::new(
                1,
                vec![Arc::new(vec![0.0]), Arc::new(vec![])],
                AudioMeta::default()
            )
            .is_err()
        );
        let many: Vec<Arc<Vec<f32>>> = (0..=usize::from(u16::MAX))
            .map(|_| Arc::new(Vec::new()))
            .collect();
        assert!(Audio::new(1, many, AudioMeta::default()).is_err());
    }
}
