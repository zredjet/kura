//! 音声の計算の土台（docs/editing.md §5.3）。
//!
//! 音声の操作は、入力全体を見て出力の各フレームを決まった式で計算する「核」（`AudioKernel`）として書く。
//! 前計算（ラウドネス・フィルタの表など）は核を作るときに 1 度だけ行い、`render` は出力の区間を描く純粋な関数にする。
//! `apply` は核を `AUDIO_BLOCK` ずつ並列に描くだけなので、区間の切り方・スレッド数によらず同じ値になる（E18）。
//! 状態を運ぶフィルタ（IIR など）は核にせず、解析（`crate::analysis`）の側で全体を順に流す。

pub mod resample;

use std::sync::Arc;

use crate::determinism::{AUDIO_BLOCK, block_count, par_blocks};
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::value::{Audio, AudioMeta, Value};

/// 音声の操作の核：出力のチャンネル・長さ・レートと、出力の区間を描く関数。
pub trait AudioKernel: Send + Sync {
    /// 出力のサンプルレート。
    fn rate(&self) -> u32;
    /// 出力のチャンネル数。
    fn channels(&self) -> usize;
    /// 出力のフレーム数。
    fn frames(&self) -> u64;
    /// 出力のチャンネル `ch` の、フレーム `start` から `out.len()` フレームを書く。
    /// 同じ位置にはいつも同じ値を書く（呼び方・区間の切り方によらない）。範囲は `frames()` の中であること。
    fn render(&self, ch: usize, start: u64, out: &mut [f32]);
}

/// 核の出力の全体を描いて値にする（メタデータは `meta` を付ける。写し方は実行器が当て直す）。
pub fn render_all(
    k: &dyn AudioKernel,
    meta: AudioMeta,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    let frames =
        usize::try_from(k.frames()).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let mut planes = Vec::with_capacity(k.channels());
    for ch in 0..k.channels() {
        let blocks = par_blocks(block_count(frames, AUDIO_BLOCK), mon, |b| {
            let start = b * AUDIO_BLOCK;
            let end = start.saturating_add(AUDIO_BLOCK).min(frames);
            let mut out = crate::codec::try_vec(end - start, 0f32)?;
            k.render(ch, start as u64, &mut out);
            Ok(out)
        })?;
        let mut plane: Vec<f32> = Vec::new();
        plane
            .try_reserve_exact(frames)
            .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        for b in blocks {
            plane.extend_from_slice(&b);
        }
        planes.push(Arc::new(plane));
    }
    Ok(Value::Audio(Arc::new(Audio::new(k.rate(), planes, meta)?)))
}

/// 入力をそのまま写す核（メタデータだけを書く操作が使う）。
pub struct Identity(pub Arc<Audio>);

impl AudioKernel for Identity {
    fn rate(&self) -> u32 {
        self.0.rate()
    }

    fn channels(&self) -> usize {
        self.0.channels()
    }

    fn frames(&self) -> u64 {
        self.0.frames() as u64
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) {
        copy_from(&self.0.planes()[ch], start, out);
    }
}

/// `plane[start..]` から `out.len()` 個を写す。範囲の外は 0。
pub fn copy_from(plane: &[f32], start: u64, out: &mut [f32]) {
    let start = usize::try_from(start).unwrap_or(usize::MAX);
    for (i, o) in out.iter_mut().enumerate() {
        *o = start
            .checked_add(i)
            .and_then(|k| plane.get(k))
            .copied()
            .unwrap_or(0.0);
    }
}

/// 出力の量子化（`OutputSpec.quantize`。docs/editing.md §4.1）：標本を `bits` ビットの整数の格子に乗せる。
///
/// 式（版 1）：s = 2^(bits−1)、x = f64(標本) · s、ディザが TPDF なら x に (u1 + u2 − 1) を足す（u1・u2 は [0, 1) の一様で、
/// `determinism::rng_at(seed', チャンネル, 2·フレーム)` から順に 2 つの u32 を取って (u + 0.5) / 2^32。seed' は
/// `determinism::seed(quantize.seed, 元のハッシュ, "audio.dither")`）。q = round_ties_even(x) を [−s, s − 1] で止め
/// （NaN は 0）、値は q / s（f32 で正確）。WAV・AIFF・FLAC の書き出しの丸め（偶数への丸め）と同じ格子なので、
/// 書き出すと q がそのまま書かれる。メタデータの元の形式は、整数・`bits` ビットにする。
pub fn quantize(
    a: &Audio,
    q: &crate::doc::Quantize,
    source: &crate::digest::Digest,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    let k = Quantizer {
        audio: a,
        scale: f64::from(1u32 << (q.bits.clamp(1, 31) - 1)),
        seed: (q.dither == crate::doc::Dither::Tpdf)
            .then(|| crate::determinism::seed(q.seed, &source.0, "audio.dither")),
    };
    let mut meta = a.meta.clone();
    meta.source = Some(crate::value::AudioSource {
        bits: q.bits,
        float: false,
        rate: a.meta.source.map_or(a.rate(), |s| s.rate),
    });
    render_all(&k, meta, mon)
}

/// 量子化の核。
struct Quantizer<'a> {
    audio: &'a Audio,
    scale: f64,
    /// ディザの種（なければディザなし）。
    seed: Option<u64>,
}

impl AudioKernel for Quantizer<'_> {
    fn rate(&self) -> u32 {
        self.audio.rate()
    }

    fn channels(&self) -> usize {
        self.audio.channels()
    }

    fn frames(&self) -> u64 {
        self.audio.frames() as u64
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) {
        use rand_chacha::rand_core::Rng;
        copy_from(&self.audio.planes()[ch], start, out);
        // 区間の始まりの位置へシークして順に取る（標本ごとにシークしたものと同じ値）
        let mut rng = self
            .seed
            .map(|s| crate::determinism::rng_at(s, ch as u64, u128::from(start) * 2));
        let unit =
            |r: &mut rand_chacha::ChaCha8Rng| (f64::from(r.next_u32()) + 0.5) / 4_294_967_296.0;
        for s in out.iter_mut() {
            let mut x = f64::from(*s) * self.scale;
            if let Some(r) = rng.as_mut() {
                let (u1, u2) = (unit(r), unit(r));
                x += u1 + u2 - 1.0;
            }
            let q = if x.is_nan() {
                0.0
            } else {
                x.round_ties_even().clamp(-self.scale, self.scale - 1.0)
            };
            *s = (q / self.scale) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::Digest;
    use crate::doc::{Dither, Quantize};
    use crate::monitor::NoMonitor;
    use crate::testgen::InputGen;

    fn noise() -> Arc<Audio> {
        let v = InputGen::AudioNoise {
            rate: 48_000,
            channels: 2,
            frames: 9_000,
            seed: 3,
        }
        .generate()
        .unwrap();
        match v {
            Value::Audio(a) => a,
            _ => panic!("音声ではありません"),
        }
    }

    fn q(bits: u16, dither: Dither, seed: u64) -> Quantize {
        Quantize {
            v: 1,
            bits,
            dither,
            seed,
        }
    }

    fn planes(v: &Value) -> Vec<Vec<f32>> {
        match v {
            Value::Audio(a) => a.planes().iter().map(|p| p.to_vec()).collect(),
            _ => panic!("音声ではありません"),
        }
    }

    #[test]
    fn quantize_puts_samples_on_the_grid() {
        let a = noise();
        let src = Digest::default();
        let plain = quantize(&a, &q(16, Dither::None, 0), &src, &NoMonitor).unwrap();
        for (p, s) in planes(&plain).iter().zip(a.planes()) {
            for (x, y) in p.iter().zip(s.iter()) {
                let want = ((f64::from(*y) * 32_768.0)
                    .round_ties_even()
                    .clamp(-32_768.0, 32_767.0)
                    / 32_768.0) as f32;
                assert_eq!(*x, want);
            }
        }
        let Value::Audio(pa) = &plain else { panic!() };
        assert_eq!(pa.meta.source.map(|s| (s.bits, s.float)), Some((16, false)));
        let d1 = quantize(&a, &q(16, Dither::Tpdf, 7), &src, &NoMonitor).unwrap();
        let d2 = quantize(&a, &q(16, Dither::Tpdf, 7), &src, &NoMonitor).unwrap();
        let d3 = quantize(&a, &q(16, Dither::Tpdf, 8), &src, &NoMonitor).unwrap();
        assert_eq!(d1, d2);
        assert_ne!(d1, d3);
        assert_ne!(d1, plain);
        for p in planes(&d1) {
            for x in p {
                let k = f64::from(x) * 32_768.0;
                assert_eq!(k, k.round(), "格子に乗っていない");
            }
        }
    }

    /// 区間の始まりにシークして順に取った乱数は、標本ごとにシークしたものと同じ（区間の切り方によらない）。
    #[test]
    fn dither_is_position_addressed() {
        use rand_chacha::rand_core::Rng;
        let a = noise();
        let src = Digest::default();
        let out = planes(&quantize(&a, &q(24, Dither::Tpdf, 9), &src, &NoMonitor).unwrap());
        let seed = crate::determinism::seed(9, &src.0, "audio.dither");
        for (ch, got) in out.iter().enumerate() {
            for i in [0usize, 1, 4_095, 4_096, 8_999] {
                let mut r = crate::determinism::rng_at(seed, ch as u64, i as u128 * 2);
                let u1 = (f64::from(r.next_u32()) + 0.5) / 4_294_967_296.0;
                let u2 = (f64::from(r.next_u32()) + 0.5) / 4_294_967_296.0;
                let s = f64::from(1u32 << 23);
                let x = f64::from(a.planes()[ch][i]) * s + u1 + u2 - 1.0;
                let want = (x.round_ties_even().clamp(-s, s - 1.0) / s) as f32;
                assert_eq!(got[i], want, "ch {ch} i {i}");
            }
        }
    }
}
