//! 音声の計算の土台（docs/editing.md §5.3）。
//!
//! 音声の操作は、入力全体を見て出力の各フレームを決まった式で計算する「核」（`AudioKernel`）として書く。
//! 前計算（ラウドネス・フィルタの表など）は核を作るときに 1 度だけ行い、`render` は出力の区間を描く関数にする（同じ位置には
//! いつも同じ値。値に効かない覚え（続きの流し）は持ってよく、並列の仕事の間で共有しないか錠で守る。M17 段 1）。
//! `apply` は核を（組, チャンク）ずつ並列に描くだけなので、区間の切り方・スレッド数によらず同じ値になる（E18）。
//! 状態を運ぶ操作（IIR・包絡線）は、操作が申告した「状態の表」の解析から区間を描く核にする（[`state`]。M17 段 1。
//! docs/editing.md §18.1）。ラウドネスの計測の K 特性は解析（`crate::analysis`）の側で全体を順に流す。

pub mod biquad;
pub mod digest;
pub mod fft;
pub mod fir;
pub mod resample;
pub mod source;
pub mod spec;
pub mod state;
pub mod stream;

use std::sync::Arc;

use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::value::{AUDIO_CHUNK, Audio, AudioMeta, Chunk, ChunkSet, Value};

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
    /// チャンネルの組の幅（M17 段 1。docs/editing.md §18.1）：組 k はチャンネル [k·w, (k+1)·w) で、同じ計算を分け合う
    /// （M/S のバンド・ステレオのリンクでは、出力の各チャンネルが組のすべての入力に依る）。描き手（`render_all`・
    /// `render_chunks`）は（組, チャンク）ごとに [`AudioKernel::render_lane`] で描く。1 以上で、チャンネルの数を割り切ること。
    /// 既定は 1（各チャンネルが 1 組）。
    fn lane_width(&self) -> usize {
        1
    }
    /// 組 `lane` のチャンネルを、フレーム `start` から `outs[c].len()` フレームずつ一度に描く（`outs` は組のチャンネルの順で、
    /// 長さは幅、どれも同じ長さ）。値は `render` をチャンネルごとに呼んだものと同じであること（E18）。既定は `render` を
    /// チャンネルごとに呼ぶ。
    fn render_lane(
        &self,
        lane: usize,
        start: u64,
        outs: &mut [&mut [f32]],
    ) -> Result<(), EngineError> {
        let w = self.lane_width();
        for (c, out) in outs.iter_mut().enumerate() {
            self.render(lane * w + c, start, out)?;
        }
        Ok(())
    }
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

/// 核の組の幅と組の数（幅が 0・チャンネルの数を割り切らなければ内部の誤り）。
fn lanes_of(k: &dyn AudioKernel) -> Result<(usize, usize), EngineError> {
    let (w, c) = (k.lane_width(), k.channels());
    if w == 0 || c % w != 0 {
        return Err(EngineError::Internal(format!(
            "核の組の幅 {w} が {c} チャンネルを割り切りません"
        )));
    }
    Ok((w, c / w))
}

/// 組 `lane` のチャンネルを [start, end) だけ `render_lane` で描いて、チャンネルの順の中身にする。
fn draw_lane(
    k: &dyn AudioKernel,
    w: usize,
    lane: usize,
    start: usize,
    end: usize,
) -> Result<Vec<Chunk>, EngineError> {
    let mut outs: Vec<Vec<f32>> = (0..w)
        .map(|_| crate::codec::try_vec(end - start, 0f32))
        .collect::<Result<_, _>>()?;
    let mut refs: Vec<&mut [f32]> = outs.iter_mut().map(Vec::as_mut_slice).collect();
    k.render_lane(lane, start as u64, &mut refs)?;
    Ok(outs.into_iter().map(|o| Chunk::Data(Arc::new(o))).collect())
}

/// 核の出力の全体を描いて値にする（メタデータは `meta` を付ける。写し方は実行器が当て直す）。
/// （組, チャンク）の対ごとに、チャンク（`AUDIO_CHUNK` フレーム）を `par_blocks` で描いてそのまま表に入れる
/// （平面へ写し直さない。持つ量は出力の値と、並列の仕事ごとの核の作業列だけ）。組は [`AudioKernel::lane_width`]。
pub fn render_all(
    k: &dyn AudioKernel,
    meta: AudioMeta,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    let frames =
        usize::try_from(k.frames()).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let channels = k.channels();
    let (w, lanes) = lanes_of(k)?;
    let count = frames.div_ceil(AUDIO_CHUNK);
    let tasks = lanes
        .checked_mul(count)
        .ok_or_else(|| EngineError::Limit("音声が長すぎます".into()))?;
    let drawn = par_blocks(tasks, mon, |t| {
        let (lane, j) = (t / count, t % count);
        let start = j * AUDIO_CHUNK;
        let end = start.saturating_add(AUDIO_CHUNK).min(frames);
        draw_lane(k, w, lane, start, end)
    })?;
    let mut table: Vec<Vec<Chunk>> = (0..channels).map(|_| Vec::with_capacity(count)).collect();
    for (t, group) in drawn.into_iter().enumerate() {
        let lane = t / count;
        for (c, chunk) in group.into_iter().enumerate() {
            table[lane * w + c].push(chunk);
        }
    }
    Ok(Value::Audio(Arc::new(Audio::from_chunks(
        k.rate(),
        frames,
        table,
        meta,
    )?)))
}

/// 範囲の描画の部品（M13 段 1。docs/editing.md §12.5）：核の出力のうち `picks` のチャンクだけを描いた値と、そのうち
/// 入力 0 の実体を共有したチャンクの数（チャンクの番号の数。チャンネルで数えない）。
///
/// - `picks` の各（組, チャンク）を `par_blocks` で描く（組は [`AudioKernel::lane_width`]。M17 段 1）。核が入力 0 をずらして写したもの（`passthrough` が Some(off)）で、
///   off がチャンクの倍数・入力 0 のチャンク k + off/C の長さが出力のチャンク k と同じなら、その実体（中身・源の参照）を共有する
///   （無効なら読んだ印を立てて誤り）。そうでなければ核で描く。
/// - `picks` の外は、`base`（前の値。レート・チャンネルが同じで、その番号のチャンクの長さが同じとき。長さの違う前の値も
///   受ける）の有効なチャンクを共有し、なければ無効にする。
/// - 値は分け方によらない（核の約束）。メタデータは `meta`（写し方は実行器が当て直す）。
pub fn render_chunks(
    k: &dyn AudioKernel,
    input0: Option<&Audio>,
    picks: &ChunkSet,
    base: Option<&Audio>,
    meta: AudioMeta,
    mon: &dyn Monitor,
) -> Result<(Audio, u64), EngineError> {
    let frames =
        usize::try_from(k.frames()).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let channels = k.channels();
    let count = frames.div_ceil(AUDIO_CHUNK);
    let list: Vec<usize> = picks.iter().filter(|j| *j < count).collect();
    let span = |j: usize| {
        let start = j.saturating_mul(AUDIO_CHUNK).min(frames);
        start..start.saturating_add(AUDIO_CHUNK).min(frames)
    };
    // ずらした写しの共有：出力のチャンク j ＝ 入力 0 のチャンク j + q（長さが同じとき）
    let shift = k
        .passthrough()
        .filter(|off| off % AUDIO_CHUNK as u64 == 0)
        .and_then(|off| usize::try_from(off / AUDIO_CHUNK as u64).ok());
    let source_of = |j: usize| -> Option<(&Audio, usize)> {
        let (q, inp) = (shift?, input0?);
        let jj = j.checked_add(q)?;
        (inp.rate() == k.rate()
            && inp.channels() == channels
            && jj < inp.chunk_count()
            && inp.chunk_range(jj).len() == span(j).len())
        .then_some((inp, jj))
    };
    let (w, lanes) = lanes_of(k)?;
    let tasks = lanes
        .checked_mul(list.len())
        .ok_or_else(|| EngineError::Limit("音声が長すぎます".into()))?;
    let per = list.len().max(1);
    let drawn = par_blocks(tasks, mon, |t| {
        let (lane, j) = (t / per, list[t % per]);
        if let Some((inp, jj)) = source_of(j) {
            let group = (0..w)
                .map(|c| inp.share_chunk(lane * w + c, jj))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok((group, true));
        }
        let r = span(j);
        Ok((draw_lane(k, w, lane, r.start, r.end)?, false))
    })?;
    let from_base = |ch: usize, j: usize| -> Chunk {
        match base {
            Some(b)
                if b.rate() == k.rate()
                    && b.channels() == channels
                    && j < b.chunk_count()
                    && b.chunk_range(j).len() == span(j).len() =>
            {
                b.chunk(ch, j).cloned().unwrap_or(Chunk::Hole)
            }
            _ => Chunk::Hole,
        }
    };
    let mut table: Vec<Vec<Chunk>> = (0..channels)
        .map(|ch| (0..count).map(|j| from_base(ch, j)).collect())
        .collect();
    // 前の値の無効なチャンクは全チャンネルでそろう（チャンネルごとに有効・無効が違えば無効にそろえる）
    for j in 0..count {
        if table.iter().any(|row| matches!(row[j], Chunk::Hole)) {
            for row in &mut table {
                row[j] = Chunk::Hole;
            }
        }
    }
    let mut shared = 0u64;
    for (t, (group, from_input)) in drawn.into_iter().enumerate() {
        let (lane, j) = (t / per, list[t % per]);
        if from_input && lane == 0 {
            shared += 1;
        }
        for (c, chunk) in group.into_iter().enumerate() {
            table[lane * w + c][j] = chunk;
        }
    }
    Ok((Audio::from_chunks(k.rate(), frames, table, meta)?, shared))
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
///
/// `only` があればそのチャンク（有効なものだけ）を量子化し、ほかは `a` のチャンクのまま（無効なチャンクは無効のまま）。None は
/// 有効なチャンクすべて。TPDF のディザは冪等でない（格子の上の値に乱れを足すので 2 度当てると変わる）ので、継ぎはぎは前の
/// 表示の量子化済みのチャンクに当て直さず、描いたチャンクだけを渡す（M13 段 1。docs/editing.md §12.5）。
pub fn quantize(
    a: &Audio,
    q: &crate::doc::Quantize,
    source: &crate::digest::Digest,
    only: Option<&ChunkSet>,
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
    if only.is_none() && a.is_complete() {
        return render_all(&k, meta, mon);
    }
    let mut picks = a.valid_chunks();
    if let Some(o) = only {
        picks.intersect_with(o);
    }
    let (v, _) = render_chunks(&k, None, &picks, Some(a), meta, mon)?;
    Ok(Value::Audio(Arc::new(v)))
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
        let plain = quantize(&a, &q(16, Dither::None, 0), &src, None, &NoMonitor).unwrap();
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
        let d1 = quantize(&a, &q(16, Dither::Tpdf, 7), &src, None, &NoMonitor).unwrap();
        let d2 = quantize(&a, &q(16, Dither::Tpdf, 7), &src, None, &NoMonitor).unwrap();
        let d3 = quantize(&a, &q(16, Dither::Tpdf, 8), &src, None, &NoMonitor).unwrap();
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

    /// 組を一度に描く核（M17 段 1）：幅 2 の組で、L・R から和と差を出す。`render_lane` を呼んだ数を数える。
    struct SumDiff {
        audio: Arc<Audio>,
        lanes: std::sync::atomic::AtomicUsize,
    }

    impl AudioKernel for SumDiff {
        fn rate(&self) -> u32 {
            self.audio.rate()
        }
        fn channels(&self) -> usize {
            self.audio.channels()
        }
        fn frames(&self) -> u64 {
            self.audio.frames() as u64
        }
        fn lane_width(&self) -> usize {
            2
        }
        fn render(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError> {
            let lane = ch / 2;
            let mut l = vec![0f32; out.len()];
            let mut r = vec![0f32; out.len()];
            self.audio.read(lane * 2, start, &mut l)?;
            self.audio.read(lane * 2 + 1, start, &mut r)?;
            for (i, o) in out.iter_mut().enumerate() {
                *o = if ch.is_multiple_of(2) {
                    l[i] + r[i]
                } else {
                    l[i] - r[i]
                };
            }
            Ok(())
        }
        fn render_lane(
            &self,
            lane: usize,
            start: u64,
            outs: &mut [&mut [f32]],
        ) -> Result<(), EngineError> {
            self.lanes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for (c, out) in outs.iter_mut().enumerate() {
                self.render(lane * 2 + c, start, out)?;
            }
            Ok(())
        }
    }

    /// 描き手（`render_all`・`render_chunks`）は（組, チャンク）ごとに `render_lane` で描き、値はチャンネルごとの `render` と
    /// 同じ。前の値・選んだチャンクの扱いも幅 1 と同じ。幅がチャンネルの数を割り切らなければ内部の誤り。
    #[test]
    fn lanes_are_drawn_at_once() {
        use std::sync::atomic::Ordering;
        let v = InputGen::AudioNoise {
            rate: 8_000,
            channels: 4,
            frames: 2 * AUDIO_CHUNK as u64 + 77,
            seed: 5,
        }
        .generate()
        .unwrap();
        let Value::Audio(a) = v else { panic!() };
        let k = SumDiff {
            audio: a.clone(),
            lanes: Default::default(),
        };
        let all = render_all(&k, AudioMeta::default(), &NoMonitor).unwrap();
        // 2 組 × 3 チャンク
        assert_eq!(k.lanes.load(Ordering::Relaxed), 6);
        let Value::Audio(all) = all else { panic!() };
        for ch in 0..4 {
            let mut want = vec![0f32; a.frames()];
            k.render(ch, 0, &mut want).unwrap();
            let mut got = vec![0f32; a.frames()];
            all.read(ch, 0, &mut got).unwrap();
            assert!(
                got.iter()
                    .zip(&want)
                    .all(|(x, y)| x.to_bits() == y.to_bits())
            );
        }
        // 選んだチャンクだけ（ほかは前の値か無効）
        k.lanes.store(0, Ordering::Relaxed);
        let mut picks = ChunkSet::empty(3);
        picks.insert(1);
        let (part, shared) = render_chunks(
            &k,
            Some(&a),
            &picks,
            Some(&all),
            AudioMeta::default(),
            &NoMonitor,
        )
        .unwrap();
        assert_eq!((k.lanes.load(Ordering::Relaxed), shared), (2, 0));
        assert!(part.is_complete());
        assert_eq!(Value::Audio(Arc::new(part)), Value::Audio(all.clone()));
        let (part, _) =
            render_chunks(&k, Some(&a), &picks, None, AudioMeta::default(), &NoMonitor).unwrap();
        assert!(!part.is_complete());
        let mut got = vec![0f32; AUDIO_CHUNK];
        part.read(3, AUDIO_CHUNK as u64, &mut got).unwrap();
        let mut want = vec![0f32; AUDIO_CHUNK];
        all.read(3, AUDIO_CHUNK as u64, &mut want).unwrap();
        assert_eq!(got, want);
        // 幅が割り切らない
        let odd = InputGen::AudioNoise {
            rate: 8_000,
            channels: 3,
            frames: 100,
            seed: 6,
        }
        .generate()
        .unwrap();
        let Value::Audio(odd) = odd else { panic!() };
        let k = SumDiff {
            audio: odd,
            lanes: Default::default(),
        };
        assert!(matches!(
            render_all(&k, AudioMeta::default(), &NoMonitor),
            Err(EngineError::Internal(_))
        ));
    }

    /// 区間の始まりにシークして順に取った乱数は、標本ごとにシークしたものと同じ（区間の切り方によらない）。
    #[test]
    fn dither_is_position_addressed() {
        use rand_chacha::rand_core::Rng;
        let a = noise();
        let src = Digest::default();
        let out = planes(&quantize(&a, &q(24, Dither::Tpdf, 9), &src, None, &NoMonitor).unwrap());
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
