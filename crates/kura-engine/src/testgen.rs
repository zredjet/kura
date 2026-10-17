//! 例とテストの入力を、記述から決定的に作る（docs/editing.md §8.5）。バイナリのフィクスチャを置かずに済ませる。

use std::sync::Arc;

use rand_chacha::rand_core::Rng;
use serde::{Deserialize, Serialize};

use crate::determinism;
use crate::error::EngineError;
use crate::geom::{IPoint, IRect};
use crate::math;
use crate::value::{
    Audio, AudioMeta, Frames, Grid, Image, ImageMeta, LoopRange, Palette, PixelFormat, Raster,
    Value,
};

/// 入力の作り方。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type",
    deny_unknown_fields
)]
pub enum InputGen {
    /// 2 色の市松模様。
    Checker {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        cell: u32,
    },
    /// ChaCha8 の乱数の画素（Indexed8 は 16 色のパレット）。
    Noise {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        seed: u64,
    },
    /// 透明な余白に囲まれた不透明な矩形（Rgba8）。
    Sprite { w: u32, h: u32, rect: IRect },
    /// セルごとに色の違うスプライトシート（Rgba8）。格子とピボットのメタデータ付き。
    Sheet { cols: u32, rows: u32, cell: u32 },
    /// 乱数のフレームの列。
    NoiseFrames {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        count: u32,
        seed: u64,
    },
    /// トリムしたフレームの列：`w × h` のキャンバスに、大きさと位置の違う乱数のフレームを置く
    /// （フレーム i は 幅 `w/2 + i % (w/2)`・高さ `h/2` を、左上 `(i % (w/2), i % (h/2))` に置く）。
    TrimmedFrames {
        w: u32,
        h: u32,
        fmt: PixelFormat,
        count: u32,
        seed: u64,
    },
    /// 正弦波（振幅 0.5）。チャンネルごとに周波数を 1.5 倍ずつ上げる。
    Sine {
        rate: u32,
        channels: u16,
        frames: u64,
        freq: u32,
        loop_range: Option<LoopRange>,
    },
    /// 区間ごとに大きさの違う 1 チャンネルの正弦波：区間は（フレームの数, dBFS の振幅）。正規化の絶対ゲートをまたぐ形を作る。
    SineLevels {
        rate: u32,
        freq: u32,
        segments: Vec<(u64, i32)>,
    },
    /// -1〜1 の乱数の音声。
    AudioNoise {
        rate: u32,
        channels: u16,
        frames: u64,
        seed: u64,
    },
    /// 元の作り方の値にメタデータを付ける（コーデックのゴールデンでメタデータのチャンクを凍結する）。
    WithMeta { base: Box<InputGen>, meta: MetaGen },
    /// 衝撃の繰り返し（Ogg Vorbis の短い窓と aoTuV の M3 に届く入力。44.1 kHz・品質 0 では M2 にも届く。M9 は `Swell`）：`period` フレームごとに、16 フレームの
    /// 雑音（振幅 0.95。チャンネルごとの ChaCha8 の流れから順に取る）と、そのあと `rate × 200 / 44,100` フレーム（約 4.5 ms）
    /// だけ続いて急に止まる 700 Hz の正弦波（振幅 0.3。位相はフレームの位置から）。残りは無音。
    Bursts {
        rate: u32,
        channels: u16,
        frames: u64,
        period: u64,
        seed: u64,
    },
    /// だんだん大きくなる音（Ogg Vorbis の長い窓で、前のブロックより大きくなるトーン。aoTuV の M9 に届く入力）：周波数
    /// `freqs`（Hz）の正弦波の平均を、振幅 `from_db` から `to_db`（dBFS）まで、フレームの位置に比例して dB で大きくする。
    /// 位相はフレームの位置から、チャンネル c・k 番めの周波数は `0.3 c + k` ラジアンずらす。
    Swell {
        rate: u32,
        channels: u16,
        frames: u64,
        freqs: Vec<u32>,
        from_db: i32,
        to_db: i32,
    },
}

/// 付けるメタデータ。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct MetaGen {
    pub dpi: Option<(u32, u32)>,
    pub text: Vec<(String, String)>,
    pub icc: Option<Vec<u8>>,
    pub loop_range: Option<LoopRange>,
    pub markers: Vec<(u64, String)>,
    pub tags: Vec<(String, String)>,
    /// EXIF（TIFF の形）。ないときは書かない（足す前に凍結した入力の書き方を変えない）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exif: Option<Vec<u8>>,
    /// スライス（画像だけ）。ないときは書かない（同上）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slices: Vec<crate::value::Slice>,
}

impl MetaGen {
    fn apply_image(&self, meta: &mut ImageMeta) {
        meta.dpi = self.dpi.or(meta.dpi);
        meta.text.extend(self.text.iter().cloned());
        if let Some(icc) = &self.icc {
            meta.icc = Some(Arc::new(icc.clone()));
        }
        if let Some(exif) = &self.exif {
            meta.exif = Some(Arc::new(exif.clone()));
        }
        meta.slices.extend(self.slices.iter().cloned());
    }

    fn apply_audio(&self, meta: &mut AudioMeta) {
        meta.loop_range = self.loop_range.or(meta.loop_range);
        meta.markers
            .extend(self.markers.iter().map(|(pos, name)| crate::value::Marker {
                pos: *pos,
                name: name.clone(),
            }));
        meta.tags.extend(self.tags.iter().cloned());
    }
}

fn palette16() -> Palette {
    Palette {
        colors: (0..16u8)
            .map(|i| [i * 16, 255 - i * 16, i * 7, if i == 0 { 0 } else { 255 }])
            .collect(),
    }
}

fn noise_raster(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Result<Raster, EngineError> {
    let palette = (fmt == PixelFormat::Indexed8).then(palette16);
    let bpp = fmt.bytes_per_pixel();
    Raster::from_tile_fn(
        w,
        h,
        fmt,
        palette,
        &crate::monitor::NoMonitor,
        |rect, out| {
            // 行ごとの流れの中で x の位置へシークするので、タイルの分け方・並列によらず同じ中身になる
            let row_len = rect.w as usize * bpp;
            for (r, row) in out.chunks_exact_mut(row_len).enumerate() {
                let mut rng = determinism::rng_stream(seed, rect.y as u64 + r as u64);
                rng.set_word_pos(rect.x as u128 * bpp as u128);
                for b in row.iter_mut() {
                    let v = rng.next_u32() as u8;
                    *b = if fmt == PixelFormat::Indexed8 {
                        v & 0x0f
                    } else {
                        v
                    };
                }
            }
            Ok(())
        },
    )
}

impl InputGen {
    /// 値を作る。
    pub fn generate(&self) -> Result<Value, EngineError> {
        // メタデータを付ける作り方は、元の作り方の値に付けて返す
        if let Self::WithMeta { base, meta } = self {
            {
                return Ok(match base.generate()? {
                    Value::Image(img) => {
                        let mut img = Arc::unwrap_or_clone(img);
                        meta.apply_image(&mut img.meta);
                        Value::Image(Arc::new(img))
                    }
                    Value::Frames(fr) => {
                        let mut fr = Arc::unwrap_or_clone(fr);
                        meta.apply_image(&mut fr.meta);
                        Value::Frames(Arc::new(fr))
                    }
                    Value::Audio(a) => {
                        let mut a = Arc::unwrap_or_clone(a);
                        meta.apply_audio(&mut a.meta);
                        Value::Audio(Arc::new(a))
                    }
                });
            }
        }
        match *self {
            Self::Checker { w, h, fmt, cell } => {
                let bpp = fmt.bytes_per_pixel();
                let palette = (fmt == PixelFormat::Indexed8).then(palette16);
                let cell = cell.max(1);
                let px = Raster::from_tile_fn(
                    w,
                    h,
                    fmt,
                    palette,
                    &crate::monitor::NoMonitor,
                    |rect, out| {
                        // Indexed8 はパレットの 1 番と 0 番、ほかは 0xff と 0x01
                        let (on_v, off_v) = if fmt == PixelFormat::Indexed8 {
                            (1, 0)
                        } else {
                            (0xff, 0x01)
                        };
                        for (i, chunk) in out.chunks_exact_mut(bpp).enumerate() {
                            let x = rect.x as u32 + i as u32 % rect.w;
                            let y = rect.y as u32 + i as u32 / rect.w;
                            let on = ((x / cell) + (y / cell)).is_multiple_of(2);
                            chunk.fill(if on { on_v } else { off_v });
                        }
                        Ok(())
                    },
                )?;
                Ok(Value::Image(Arc::new(Image {
                    px,
                    meta: ImageMeta::default(),
                })))
            }
            Self::Noise { w, h, fmt, seed } => Ok(Value::Image(Arc::new(Image {
                px: noise_raster(w, h, fmt, seed)?,
                meta: ImageMeta::default(),
            }))),
            Self::Sprite { w, h, rect } => {
                let px = Raster::from_tile_fn(
                    w,
                    h,
                    PixelFormat::Rgba8,
                    None,
                    &crate::monitor::NoMonitor,
                    |t, out| {
                        for (i, chunk) in out.chunks_exact_mut(4).enumerate() {
                            let x = t.x + i64::from(i as u32 % t.w);
                            let y = t.y + i64::from(i as u32 / t.w);
                            let inside =
                                x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom();
                            chunk.copy_from_slice(if inside {
                                &[200, 80, 40, 255]
                            } else {
                                &[0, 0, 0, 0]
                            });
                        }
                        Ok(())
                    },
                )?;
                Ok(Value::Image(Arc::new(Image {
                    px,
                    meta: ImageMeta::default(),
                })))
            }
            Self::Sheet { cols, rows, cell } => {
                let (w, h) = (cols * cell, rows * cell);
                let px = Raster::from_tile_fn(
                    w,
                    h,
                    PixelFormat::Rgba8,
                    None,
                    &crate::monitor::NoMonitor,
                    |t, out| {
                        for (i, chunk) in out.chunks_exact_mut(4).enumerate() {
                            let x = t.x as u32 + i as u32 % t.w;
                            let y = t.y as u32 + i as u32 / t.w;
                            let n = (y / cell) * cols + x / cell;
                            let edge = x.is_multiple_of(cell) || y.is_multiple_of(cell);
                            chunk.copy_from_slice(&[
                                (n * 37) as u8,
                                (n * 91) as u8,
                                if edge { 0 } else { 255 },
                                255,
                            ]);
                        }
                        Ok(())
                    },
                )?;
                let meta = ImageMeta {
                    grid: Some(Grid {
                        cell_w: cell,
                        cell_h: cell,
                        offset_x: 0,
                        offset_y: 0,
                        gap_x: 0,
                        gap_y: 0,
                    }),
                    pivot: Some(IPoint {
                        x: i64::from(cell / 2),
                        y: i64::from(cell),
                    }),
                    ..ImageMeta::default()
                };
                Ok(Value::Image(Arc::new(Image { px, meta })))
            }
            Self::NoiseFrames {
                w,
                h,
                fmt,
                count,
                seed,
            } => {
                let frames = (0..count.max(1))
                    .map(|i| {
                        Ok((
                            noise_raster(w, h, fmt, determinism::splitmix64(seed ^ u64::from(i)))?,
                            100,
                        ))
                    })
                    .collect::<Result<Vec<_>, EngineError>>()?;
                Ok(Value::Frames(Arc::new(Frames::full(
                    frames,
                    ImageMeta::default(),
                    0,
                )?)))
            }
            Self::TrimmedFrames {
                w,
                h,
                fmt,
                count,
                seed,
            } => {
                let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));
                let frames = (0..count.max(1))
                    .map(|i| {
                        let (fw, fh) = ((hw + i % hw).min(w), hh);
                        let (ox, oy) = (i % hw, i % hh);
                        let (ox, oy) = (ox.min(w - fw), oy.min(h - fh));
                        Ok(crate::value::Frame {
                            px: noise_raster(
                                fw,
                                fh,
                                fmt,
                                determinism::splitmix64(seed ^ u64::from(i)),
                            )?,
                            offset: IPoint {
                                x: i64::from(ox),
                                y: i64::from(oy),
                            },
                            duration_ms: 50 + 10 * i,
                        })
                    })
                    .collect::<Result<Vec<_>, EngineError>>()?;
                Ok(Value::Frames(Arc::new(Frames::new(
                    frames,
                    crate::geom::Size { w, h },
                    ImageMeta::default(),
                    0,
                )?)))
            }
            Self::Sine {
                rate,
                channels,
                frames,
                freq,
                loop_range,
            } => {
                let planes = (0..channels.max(1))
                    .map(|c| {
                        let f = f64::from(freq) * math::pow(1.5, f64::from(c));
                        let step = 2.0 * std::f64::consts::PI * f / f64::from(rate);
                        Arc::new(
                            (0..frames)
                                .map(|i| (0.5 * math::sin(step * i as f64)) as f32)
                                .collect::<Vec<f32>>(),
                        )
                    })
                    .collect();
                let meta = AudioMeta {
                    loop_range,
                    ..AudioMeta::default()
                };
                Ok(Value::Audio(Arc::new(Audio::new(rate, planes, meta)?)))
            }
            Self::SineLevels {
                rate,
                freq,
                ref segments,
            } => {
                let step = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(rate);
                let total = segments.iter().map(|s| s.0).sum::<u64>();
                let mut plane = Vec::new();
                plane
                    .try_reserve_exact(usize::try_from(total).unwrap_or(usize::MAX))
                    .map_err(|_| EngineError::Limit("長すぎます".into()))?;
                let mut i = 0u64;
                for &(n, db) in segments {
                    let amp = math::db_to_gain(f64::from(db));
                    for _ in 0..n {
                        plane.push((amp * math::sin(step * i as f64)) as f32);
                        i += 1;
                    }
                }
                Ok(Value::Audio(Arc::new(Audio::new(
                    rate,
                    vec![Arc::new(plane)],
                    AudioMeta::default(),
                )?)))
            }
            Self::AudioNoise {
                rate,
                channels,
                frames,
                seed,
            } => {
                let planes = (0..channels.max(1))
                    .map(|c| {
                        let mut r = determinism::rng_stream(seed, u64::from(c));
                        // 上位 24 ビットで [-1, 1)
                        Arc::new(
                            (0..frames)
                                .map(|_| ((r.next_u32() >> 8) as f32 / 8_388_608.0) - 1.0)
                                .collect::<Vec<f32>>(),
                        )
                    })
                    .collect();
                Ok(Value::Audio(Arc::new(Audio::new(
                    rate,
                    planes,
                    AudioMeta::default(),
                )?)))
            }
            Self::Bursts {
                rate,
                channels,
                frames,
                period,
                seed,
            } => {
                let period = period.max(1);
                let tail = (u64::from(rate) * 200 / 44_100).max(1);
                let step = 2.0 * std::f64::consts::PI * 700.0 / f64::from(rate);
                let planes = (0..channels.max(1))
                    .map(|c| {
                        let mut r = determinism::rng_stream(seed, u64::from(c));
                        Arc::new(
                            (0..frames)
                                .map(|i| {
                                    let k = i % period;
                                    if k < 16 {
                                        // 上位 24 ビットで [-1, 1)
                                        0.95 * (((r.next_u32() >> 8) as f32 / 8_388_608.0) - 1.0)
                                    } else if k < 16 + tail {
                                        (0.3 * math::sin(step * i as f64)) as f32
                                    } else {
                                        0.0
                                    }
                                })
                                .collect::<Vec<f32>>(),
                        )
                    })
                    .collect();
                Ok(Value::Audio(Arc::new(Audio::new(
                    rate,
                    planes,
                    AudioMeta::default(),
                )?)))
            }
            Self::Swell {
                rate,
                channels,
                frames,
                ref freqs,
                from_db,
                to_db,
            } => {
                let two_pi = 2.0 * std::f64::consts::PI;
                let count = freqs.len().max(1) as f64;
                let len = frames.max(1) as f64;
                let (db0, db1) = (f64::from(from_db), f64::from(to_db));
                let planes = (0..channels.max(1))
                    .map(|c| {
                        Arc::new(
                            (0..frames)
                                .map(|i| {
                                    let t = i as f64 / f64::from(rate);
                                    let gain =
                                        math::db_to_gain(db0 + (db1 - db0) * (i as f64 / len));
                                    let sum: f64 = freqs
                                        .iter()
                                        .enumerate()
                                        .map(|(k, f)| {
                                            math::sin(
                                                two_pi * f64::from(*f) * t
                                                    + 0.3 * f64::from(c)
                                                    + k as f64,
                                            )
                                        })
                                        .sum();
                                    (gain * sum / count) as f32
                                })
                                .collect::<Vec<f32>>(),
                        )
                    })
                    .collect();
                Ok(Value::Audio(Arc::new(Audio::new(
                    rate,
                    planes,
                    AudioMeta::default(),
                )?)))
            }
            // 上で扱った
            Self::WithMeta { .. } => Err(EngineError::Internal("WithMeta は上で扱う".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_repeatable_and_thread_independent() {
        let gens = [
            InputGen::Noise {
                w: 600,
                h: 300,
                fmt: PixelFormat::Rgba8,
                seed: 1,
            },
            InputGen::Noise {
                w: 40,
                h: 30,
                fmt: PixelFormat::Indexed8,
                seed: 2,
            },
            InputGen::Checker {
                w: 300,
                h: 300,
                fmt: PixelFormat::Rgb16,
                cell: 8,
            },
            InputGen::Sheet {
                cols: 4,
                rows: 2,
                cell: 16,
            },
            InputGen::AudioNoise {
                rate: 48_000,
                channels: 2,
                frames: 10_000,
                seed: 3,
            },
        ];
        for g in gens {
            let mut digests = Vec::new();
            for threads in [1, 3, 16] {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                digests.push(pool.install(|| g.generate().unwrap().digest().unwrap()));
            }
            assert!(digests.windows(2).all(|w| w[0] == w[1]), "{g:?}");
        }
    }

    /// 衝撃の繰り返し：周期ごとに 16 フレームの雑音（振幅 0.95 まで）、続く 700 Hz の音（振幅 0.3 まで）、残りは無音。
    /// チャンネルごとに雑音が違い、同じ記述なら同じ値。JSON の形は `{"type":"bursts",…}`。
    #[test]
    fn bursts_have_the_described_shape() {
        let g = InputGen::Bursts {
            rate: 44_100,
            channels: 2,
            frames: 3_000,
            period: 1_000,
            seed: 7,
        };
        let v = g.generate().unwrap();
        assert_eq!(v, g.generate().unwrap());
        let Value::Audio(a) = &v else {
            panic!("音声ではありません");
        };
        assert_eq!((a.rate(), a.channels(), a.frames()), (44_100, 2, 3_000));
        let tail = 200;
        for p in a.planes() {
            for (i, s) in p.iter().enumerate() {
                let k = i % 1_000;
                if k < 16 {
                    assert!(s.abs() <= 0.95, "{i}: {s}");
                } else if k < 16 + tail {
                    assert!(s.abs() <= 0.3, "{i}: {s}");
                } else {
                    assert_eq!(*s, 0.0, "{i}");
                }
            }
            assert!(p[..16].iter().any(|s| s.abs() > 0.1));
            assert!(p[1_000..1_016].iter().any(|s| s.abs() > 0.1));
        }
        assert_ne!(a.planes()[0][..16], a.planes()[1][..16]);
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::json!({
                "type": "bursts",
                "rate": 44_100,
                "channels": 2,
                "frames": 3_000,
                "period": 1_000,
                "seed": 7
            })
        );
    }

    /// だんだん大きくなる音：振幅は `from_db` から `to_db` まで増え（区間ごとの最大が増える）、最大は `to_db` を超えない。
    /// チャンネルごとに位相が違う。JSON の形は `{"type":"swell",…}`。
    #[test]
    fn swell_grows() {
        let g = InputGen::Swell {
            rate: 48_000,
            channels: 2,
            frames: 4_800,
            freqs: vec![1_000, 3_000],
            from_db: -40,
            to_db: -6,
        };
        let v = g.generate().unwrap();
        assert_eq!(v, g.generate().unwrap());
        let Value::Audio(a) = &v else {
            panic!("音声ではありません");
        };
        assert_eq!((a.rate(), a.channels(), a.frames()), (48_000, 2, 4_800));
        let limit = math::db_to_gain(-6.0) as f32;
        for p in a.planes() {
            let peaks: Vec<f32> = p
                .chunks(480)
                .map(|c| c.iter().fold(0.0_f32, |m, s| math::max32(m, s.abs())))
                .collect();
            assert!(peaks.windows(2).all(|w| w[0] < w[1]), "{peaks:?}");
            assert!(peaks[0] < math::db_to_gain(-36.0) as f32, "{peaks:?}");
            assert!(peaks.iter().all(|&m| m <= limit), "{peaks:?}");
        }
        assert_ne!(a.planes()[0], a.planes()[1]);
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::json!({
                "type": "swell",
                "rate": 48_000,
                "channels": 2,
                "frames": 4_800,
                "freqs": [1_000, 3_000],
                "fromDb": -40,
                "toDb": -6
            })
        );
    }
}
