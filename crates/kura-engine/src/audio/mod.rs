//! 音声の計算の土台（docs/editing.md §5.3）。
//!
//! 音声の操作は、入力全体を見て出力の各フレームを決まった式で計算する「核」（`AudioKernel`）として書く。
//! 前計算（ラウドネス・フィルタの表など）は核を作るときに 1 度だけ行い、`render` は出力の区間を描く純粋な関数にする。
//! `apply` は核をチャンク（`AUDIO_CHUNK`）ずつ並列に描くだけなので、区間の切り方・スレッド数によらず同じ値になる（E18）。
//! 状態を運ぶフィルタ（IIR など）は核にせず、解析（`crate::analysis`）の側で全体を順に流す。

pub mod resample;

use std::sync::Arc;

use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::value::{AUDIO_CHUNK, Audio, AudioMeta, Chunk, Value};

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
    /// 入力は `Audio::read` で読み、その誤り（範囲の描画で描かなかったチャンクを読んだ `Internal`、源の読みの `Io`）を返す。
    fn render(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError>;
    /// 出力が入力 0 をずらして写したもの（出力のフレーム i ＝ 入力 0 の同じチャンネルのフレーム i + off）なら Some(off)。
    /// 実行器はずれと長さが合うチャンクを入力の実体で共有してよい。値は `render` と同じであること（E18）。既定は None。
    fn passthrough(&self) -> Option<u64> {
        None
    }
    /// メタデータを書く操作（`MetaAccess::Write`）が書く出力のメタデータ（`apply` の出力と同じもの）。写す操作
    /// （`MetaAccess::Map`）は None（実行器が入力 0 のメタデータを写す）。既定は None。
    fn meta(&self) -> Option<AudioMeta> {
        None
    }
}

/// 核の出力の全体を描いて値にする（メタデータは `meta` を付ける。写し方は実行器が当て直す）。
/// （チャンネル, チャンク）の対ごとに、チャンク（`AUDIO_CHUNK` フレーム）を `par_blocks` で描いてそのまま表に入れる
/// （平面へ写し直さない。持つ量は出力の値と、並列の仕事ごとの核の作業列だけ）。
pub fn render_all(
    k: &dyn AudioKernel,
    meta: AudioMeta,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    let frames =
        usize::try_from(k.frames()).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let channels = k.channels();
    let count = frames.div_ceil(AUDIO_CHUNK);
    let tasks = channels
        .checked_mul(count)
        .ok_or_else(|| EngineError::Limit("音声が長すぎます".into()))?;
    let chunks = par_blocks(tasks, mon, |t| {
        let (ch, j) = (t / count, t % count);
        let start = j * AUDIO_CHUNK;
        let end = start.saturating_add(AUDIO_CHUNK).min(frames);
        let mut out = crate::codec::try_vec(end - start, 0f32)?;
        k.render(ch, start as u64, &mut out)?;
        Ok(Chunk::Data(Arc::new(out)))
    })?;
    let mut table = Vec::with_capacity(channels);
    let mut it = chunks.into_iter();
    for _ in 0..channels {
        table.push(it.by_ref().take(count).collect());
    }
    Ok(Value::Audio(Arc::new(Audio::from_chunks(
        k.rate(),
        frames,
        table,
        meta,
    )?)))
}

/// [`render_all`] が出力の値のほかに使う作業領域（`Op::work_bytes`）：並列の仕事（チャンク 1 つ）ごとの核の作業列
/// （チャンクの長さ × 16 バイト。mono の f64 の和と f32 の読みは 12 バイト）。チャンクを直に描いて表に入れるので、平面へ写し直す分
/// （M13 の前は出力の 1 チャンネル分）は持たない。窓の大きい核（resample の大きな縮小）は自分の分を足す。
pub fn render_all_work(out: &crate::shape::Shape) -> crate::op::Work {
    match out {
        crate::shape::Shape::Audio { frames, .. } => crate::op::Work {
            shared: 0,
            per_task: frames.max().min(AUDIO_CHUNK as u64).saturating_mul(16),
        },
        _ => crate::op::Work::ZERO,
    }
}

/// 入力をそのまま写す核（何も変えない引数の操作と、メタデータだけを書く操作が使う）。
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

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError> {
        self.0.read(ch, start, out)
    }

    fn passthrough(&self) -> Option<u64> {
        Some(0)
    }
}

/// メタデータを書く操作の核：標本は入力のまま、メタデータは `meta`（`audio.set_loop` が使う）。
pub struct WithMeta {
    pub audio: Arc<Audio>,
    pub meta: AudioMeta,
}

impl AudioKernel for WithMeta {
    fn rate(&self) -> u32 {
        self.audio.rate()
    }

    fn channels(&self) -> usize {
        self.audio.channels()
    }

    fn frames(&self) -> u64 {
        self.audio.frames() as u64
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError> {
        self.audio.read(ch, start, out)
    }

    fn passthrough(&self) -> Option<u64> {
        Some(0)
    }

    fn meta(&self) -> Option<AudioMeta> {
        Some(self.meta.clone())
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

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError> {
        use rand_chacha::rand_core::Rng;
        self.audio.read(ch, start, out)?;
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
        Ok(())
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
            Value::Audio(a) => a.to_planes().unwrap().iter().map(|p| p.to_vec()).collect(),
            _ => panic!("音声ではありません"),
        }
    }

    #[test]
    fn quantize_puts_samples_on_the_grid() {
        let a = noise();
        let src = Digest::default();
        let plain = quantize(&a, &q(16, Dither::None, 0), &src, &NoMonitor).unwrap();
        for (p, s) in planes(&plain).iter().zip(a.to_planes().unwrap()) {
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
                let x = f64::from(a.to_planes().unwrap()[ch][i]) * s + u1 + u2 - 1.0;
                let want = (x.round_ties_even().clamp(-s, s - 1.0) / s) as f32;
                assert_eq!(got[i], want, "ch {ch} i {i}");
            }
        }
    }
}
