//! 状態を運ぶ音声の操作の部品（M17 段 1。docs/editing.md §18.1・§18.2、ADR-231 (1)）。
//!
//! IIR・包絡線のように過去の全標本に依る操作は、そのままでは「どの区間も単独で描けて、切り方によらず同じ値」（§5.3・E18）
//! の核に載らない。次の形で載せる。
//!
//! - **正は先頭から順に流した値**（状態は f64、標本ごと）。処理は [`Stateful`]：組（状態を分け合うチャンネルの並び）ごとに
//!   状態を運んで入力を食べ、出力を書く。出力 i は入力 i + after（先読み）を食べたときに出る。入力の外（終わりの後ろ）は 0。
//! - **状態の表**（[`StateTable`]）：格子 G ごとの状態 S_kG（入力 [0, kG + after) を食べたあとの状態。k = 0..⌈n/G⌉）と、
//!   有限でない標本があったか。操作が申告する内部用の解析の結果にする（`audio.normalize` が `audio.loudness_gating` を
//!   申告するのと同じ形。§11.2）。正規の形は決めたバイナリ `kura-state` 版 1（[`StateTable::to_canonical`]）。表は
//!   [`TableBuilder`] で頭から順に流して作る（`Analysis::run` も `Analysis::stream` も同じ実装）。
//! - **表から区間を描く核**（[`StatefulKernel`]）：区間 [a, b) を、格子 ⌊a/G⌋·G の表の状態から入力を食べて進め、格子を
//!   またぐたびに表の次の状態に読み替える（続けて流さない）。核の値は「表と入力」だけの関数になるので、区間の切り方・
//!   スケジュールによらず、偽の表（E26）でも同じ。組ごとに「直前に描いた終わりと状態」を覚え（値に効かない覚え）、同じ格子の
//!   区間の中なら続きから流す（E18 の 1・7 フレームずつの描画を現実的な時間にする）。覚えの錠は入力を読む間・計算の間は
//!   持たない（取り出して、成功したときだけ書き戻す）。
//! - 有限でない入力（NaN・±∞）は 0 として食べ、出力のその位置は元の値（ビット列のまま）を出す。表は有限でない標本が
//!   あったかを持ち、操作は核を作るときに警告する（核は警告を返さないので、描いた区間によらない形にする）。
//! - 状態の NaN は 1 つのビット列（[`CANONICAL_NAN`]）に揃える（表に書くとき。流す側も揃えた状態から続ける）。入力は
//!   有限に替えてから食べるので、状態が NaN になるのは不安定・桁あふれのときだけ。
//! - 格子 G は表の正規の形の一部として凍結する値（実物の操作は [`STATE_GRID`]。変えるなら解析と操作の版を上げる）。
//!   `AUDIO_CHUNK` の倍数であることに頼らない（試験の操作は格子を引数で選んで、揃わない格子を確かめる）。

use std::sync::{Arc, Mutex};

use crate::analysis::output::{AnalysisOutput, Encoding};
use crate::analysis::{AnalyzeCx, AnyOutput, AudioStream, Estimate};
use crate::audio::AudioKernel;
use crate::audio::stream::StreamEnd;
use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::shape::{Dim, Shape};
use crate::value::{AUDIO_CHUNK, Audio, Value};

/// 実物の操作の状態の表の格子（フレーム。凍結）。`AUDIO_CHUNK` と同じ値にして、チャンクに揃った範囲の描画で核の助走を
/// 0 にする（docs/editing.md §18.9 の段 1 の決定）。値が同じことには頼らない。
pub const STATE_GRID: u64 = 65_536;

/// 状態の NaN を揃えるビット列（quiet NaN、符号なし）。
pub const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

/// 組 1 つの状態の語の数の上限（表の読み手が断る大きさ。M18 の先読みの列を含めても足りる）。
pub const MAX_STATE_LEN: usize = 1 << 20;

/// 正規の形の頭の印。
const MAGIC: &[u8; 8] = b"KURASTAT";
/// 正規の形の版。
const FORMAT_VERSION: u32 = 1;
/// ヘッダの長さ（印 8・版 4・旗 4・格子 8・長さ 8・先読み 8・組の数 4・状態の語の数 4・格子の数 8）。
pub const HEADER_BYTES: usize = 56;
/// 旗：有限でない標本があった。
const FLAG_NON_FINITE: u32 = 1;
/// 入力を読んで食べる区切り（フレーム。値に効かない）。
const BLOCK: usize = 4_096;

/// 状態を運ぶ処理（組ごと）。組 k はチャンネル [k·w, (k+1)·w)（w は [`Stateful::lane_width`]）。
pub trait Stateful: Send + Sync {
    /// チャンネルの数（入力と出力で同じ）。
    fn channels(&self) -> usize;
    /// 組の幅（1 以上。チャンネルの数を割り切る）。独立のチャンネルは 1、M/S・リンクは全チャンネル。
    fn lane_width(&self) -> usize;
    /// 組 1 つの状態の f64 の数（どの組も同じ。[`MAX_STATE_LEN`] 以下）。
    fn state_len(&self) -> usize;
    /// 先読み（フレーム）。出力 i は入力 i + after を食べたときに出る。
    fn after(&self) -> u64;
    /// 組 `lane` の初めの状態（入力を何も食べていない状態）を `st` に書く。
    fn init(&self, lane: usize, st: &mut [f64]);
    /// 組 `lane` の入力（絶対位置 `pos` から、組のチャンネルの順の平面。どれも同じ長さ m。有限でない標本は 0 に替えてある）を
    /// 食べて状態 `st` を進める。`out` があれば、t 番目の入力を食べたときの出力（位置 pos + t − after）を `out[c][t]` に書く
    /// （長さ m）。区切り方によらない（[p, q) と [q, r) に分けて呼んでも、[p, r) を 1 度に呼んでも同じ状態・出力）。
    fn run(
        &self,
        lane: usize,
        st: &mut [f64],
        pos: u64,
        input: &[&[f32]],
        out: Option<&mut [&mut [f32]]>,
    );
}

/// 組の数（チャンネルの数 ÷ 幅。幅が 0・割り切れなければ内部の誤り）。
fn lane_count(m: &dyn Stateful) -> Result<usize, EngineError> {
    let (c, w) = (m.channels(), m.lane_width());
    if w == 0 || c % w != 0 || m.state_len() > MAX_STATE_LEN {
        return Err(EngineError::Internal(format!(
            "状態を運ぶ処理の組の形が正しくありません（{c} チャンネル・幅 {w}・状態 {}）",
            m.state_len()
        )));
    }
    Ok(c / w)
}

/// 状態の NaN を揃える。
fn canonicalize(st: &mut [f64]) {
    for x in st.iter_mut() {
        if x.is_nan() {
            *x = f64::from_bits(CANONICAL_NAN);
        }
    }
}

/// 格子の数（⌈n / G⌉。G の 0 は 1 とみなす）。
pub fn grid_count(frames: u64, grid: u64) -> u64 {
    frames.div_ceil(grid.max(1))
}

/// 状態の表（docs/editing.md §18.1）：格子 G ごとの状態と、有限でない標本があったか。等しさは状態のビット列で比べる
/// （NaN も同じビット列なら等しい）。
#[derive(Debug, Clone)]
pub struct StateTable {
    grid: u64,
    frames: u64,
    after: u64,
    lanes: u32,
    state_len: u32,
    non_finite: bool,
    /// 格子が外・組・状態の語が内の並び（格子 k・組 l の状態は `[(k·lanes + l)·state_len ..][..state_len]`）。
    states: Vec<f64>,
}

impl PartialEq for StateTable {
    fn eq(&self, other: &Self) -> bool {
        self.grid == other.grid
            && self.frames == other.frames
            && self.after == other.after
            && self.lanes == other.lanes
            && self.state_len == other.state_len
            && self.non_finite == other.non_finite
            && self.states.len() == other.states.len()
            && self
                .states
                .iter()
                .zip(&other.states)
                .all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

impl StateTable {
    /// 表の格子（フレーム）。
    pub fn grid(&self) -> u64 {
        self.grid
    }

    /// 入力の長さ（フレーム）。
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// 先読み（フレーム）。
    pub fn after(&self) -> u64 {
        self.after
    }

    /// 組の数。
    pub fn lanes(&self) -> usize {
        self.lanes as usize
    }

    /// 組 1 つの状態の語の数。
    pub fn state_len(&self) -> usize {
        self.state_len as usize
    }

    /// 格子の数（⌈frames / grid⌉）。
    pub fn entries(&self) -> u64 {
        grid_count(self.frames, self.grid)
    }

    /// 入力に有限でない標本（NaN・±∞）があったか。
    pub fn non_finite(&self) -> bool {
        self.non_finite
    }

    /// 格子 k・組 `lane` の状態。
    pub fn state(&self, k: u64, lane: usize) -> Result<&[f64], EngineError> {
        let len = self.state_len();
        let at = usize::try_from(k)
            .ok()
            .and_then(|k| k.checked_mul(self.lanes()))
            .and_then(|i| i.checked_add(lane))
            .and_then(|i| i.checked_mul(len))
            .filter(|_| lane < self.lanes() && k < self.entries())
            .ok_or_else(|| {
                EngineError::Internal(format!("状態の表に格子 {k}・組 {lane} はありません"))
            })?;
        self.states
            .get(at..at + len)
            .ok_or_else(|| EngineError::Internal("状態の表が短すぎます".into()))
    }

    /// 偽の値を作る（E26 の `Perturb::Typed`。テストの補助）：状態の語を `f` で書き換え、旗を `non_finite` にした表。
    pub fn perturbed(&self, f: impl Fn(f64) -> f64, non_finite: bool) -> Self {
        let mut t = self.clone();
        for x in &mut t.states {
            *x = f(*x);
        }
        canonicalize(&mut t.states);
        t.non_finite = non_finite;
        t
    }
}

/// 表の正規の形の長さ（ヘッダ ＋ 格子の数 × 組の数 × 状態の語 × 8）。あふれたら None。
pub fn table_bytes(frames: u64, grid: u64, lanes: u64, state_len: u64) -> Option<u64> {
    grid_count(frames, grid)
        .checked_mul(lanes)?
        .checked_mul(state_len)?
        .checked_mul(8)?
        .checked_add(HEADER_BYTES as u64)
}

impl AnalysisOutput for StateTable {
    const ENCODING: Encoding = Encoding::Binary;

    /// `kura-state` 版 1（リトルエンディアン）：印 `KURASTAT`・版 u32・旗 u32（1 = 有限でない標本があった）・格子 u64・
    /// 長さ u64・先読み u64・組の数 u32・状態の語の数 u32・格子の数 u64、続けて格子 k・組 l の状態の f64（格子が外）。
    /// NaN は [`CANONICAL_NAN`] に揃える。
    fn to_canonical(&self) -> Result<Vec<u8>, EngineError> {
        let total = table_bytes(
            self.frames,
            self.grid,
            u64::from(self.lanes),
            u64::from(self.state_len),
        )
        .and_then(|t| usize::try_from(t).ok())
        .ok_or_else(|| EngineError::Limit("状態の表が大きすぎます".into()))?;
        if self.grid == 0
            || total != HEADER_BYTES + self.states.len() * 8
            || self.state_len as usize > MAX_STATE_LEN
        {
            return Err(EngineError::Internal(
                "状態の表の形が正しくありません".into(),
            ));
        }
        let mut out = Vec::new();
        out.try_reserve_exact(total)
            .map_err(|_| EngineError::Limit("状態の表が大きすぎます".into()))?;
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        let flags = if self.non_finite { FLAG_NON_FINITE } else { 0 };
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&self.grid.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.after.to_le_bytes());
        out.extend_from_slice(&self.lanes.to_le_bytes());
        out.extend_from_slice(&self.state_len.to_le_bytes());
        out.extend_from_slice(&self.entries().to_le_bytes());
        for x in &self.states {
            let bits = if x.is_nan() {
                CANONICAL_NAN
            } else {
                x.to_bits()
            };
            out.extend_from_slice(&bits.to_le_bytes());
        }
        Ok(out)
    }

    /// 外から来たバイト列として読む（壊れていれば `Decode`。panic しない）。印・版・知らない旗・格子 0・格子の数・長さの
    /// 食い違い・揃っていない NaN（正規でない形）を断る。
    fn from_canonical(bytes: &[u8]) -> Result<Self, EngineError> {
        let bad = |m: &str| EngineError::Decode {
            codec: "kura-state".into(),
            message: format!("状態の表が読めません：{m}"),
        };
        let head = bytes.get(..HEADER_BYTES).ok_or_else(|| bad("短すぎます"))?;
        let u32_at =
            |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
        let u64_at = |i: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&head[i..i + 8]);
            u64::from_le_bytes(b)
        };
        if &head[..8] != MAGIC {
            return Err(bad("印が違います"));
        }
        if u32_at(8) != FORMAT_VERSION {
            return Err(bad("版が違います"));
        }
        let flags = u32_at(12);
        if flags & !FLAG_NON_FINITE != 0 {
            return Err(bad("知らない旗があります"));
        }
        let (grid, frames, after) = (u64_at(16), u64_at(24), u64_at(32));
        let (lanes, state_len, entries) = (u32_at(40), u32_at(44), u64_at(48));
        if grid == 0 || lanes == 0 || state_len as usize > MAX_STATE_LEN {
            return Err(bad("格子・組の数・状態の語の数"));
        }
        if entries != grid_count(frames, grid) {
            return Err(bad("格子の数が長さと合いません"));
        }
        let total = table_bytes(frames, grid, u64::from(lanes), u64::from(state_len))
            .ok_or_else(|| bad("大きすぎます"))?;
        if u64::try_from(bytes.len()).ok() != Some(total) {
            return Err(bad("長さが合いません"));
        }
        let body = &bytes[HEADER_BYTES..];
        let mut states = Vec::new();
        states
            .try_reserve_exact(body.len() / 8)
            .map_err(|_| EngineError::Limit("状態の表が大きすぎます".into()))?;
        for w in body.as_chunks::<8>().0 {
            let bits = u64::from_le_bytes(*w);
            let x = f64::from_bits(bits);
            if x.is_nan() && bits != CANONICAL_NAN {
                return Err(bad("NaN のビット列が揃っていません"));
            }
            states.push(x);
        }
        Ok(Self {
            grid,
            frames,
            after,
            lanes,
            state_len,
            non_finite: flags & FLAG_NON_FINITE != 0,
            states,
        })
    }
}

/// 組 1 つの作りかけ：今の状態と、書いた格子の状態。
struct LaneBuild {
    st: Vec<f64>,
    entries: Vec<f64>,
    /// 書いた格子の数。
    written: u64,
}

/// 状態の表を頭から順に流して作る（`Analysis::run` と `Analysis::stream` が使う）。組は `par_blocks` で並べる。
pub struct TableBuilder {
    machine: Arc<dyn Stateful>,
    grid: u64,
    /// 長さ（決まった形）か上限（上限の形）。
    frames: Dim,
    /// 食べた入力の数。
    consumed: u64,
    non_finite: bool,
    lanes: Vec<Mutex<LaneBuild>>,
}

impl TableBuilder {
    /// 作り手を作る（`frames` は入力の長さか上限。チャンネルの数は処理のもの）。
    pub fn new(machine: Arc<dyn Stateful>, grid: u64, frames: Dim) -> Result<Self, EngineError> {
        let lanes = lane_count(machine.as_ref())?;
        let grid = grid.max(1);
        let len = machine.state_len();
        let mut list = Vec::with_capacity(lanes);
        for lane in 0..lanes {
            let mut st = crate::codec::try_vec(len, 0f64)?;
            machine.init(lane, &mut st);
            list.push(Mutex::new(LaneBuild {
                st,
                entries: Vec::new(),
                written: 0,
            }));
        }
        Ok(Self {
            machine,
            grid,
            frames,
            consumed: 0,
            non_finite: false,
            lanes: list,
        })
    }

    /// 全体の値を窓（16 チャンク）ごとに流して表を作る（`Analysis::run`）。
    pub fn solve(
        machine: Arc<dyn Stateful>,
        grid: u64,
        v: &Value,
        cx: &AnalyzeCx<'_>,
    ) -> Result<StateTable, EngineError> {
        cx.check()?;
        let Value::Audio(a) = v else {
            return Err(EngineError::Internal(
                "状態の表の入力が音声ではありません".into(),
            ));
        };
        if a.channels() != machine.channels() {
            return Err(EngineError::Internal(
                "状態の表の入力のチャンネルの数が処理と違います".into(),
            ));
        }
        let n = a.frames() as u64;
        let mut b = Self::new(machine, grid, Dim::Exact(n))?;
        let win = 16 * AUDIO_CHUNK;
        let mut planes: Vec<Vec<f32>> = (0..a.channels())
            .map(|_| crate::codec::try_vec(win.min(a.frames()), 0f32))
            .collect::<Result<_, _>>()?;
        let mut start = 0u64;
        while start < n {
            let m = usize::try_from((n - start).min(win as u64)).unwrap_or(win);
            for (ch, p) in planes.iter_mut().enumerate() {
                a.read(ch, start, &mut p[..m])?;
            }
            let refs: Vec<&[f32]> = planes.iter().map(|p| &p[..m]).collect();
            b.feed(start, &refs, cx)?;
            start += m as u64;
        }
        b.finish_at(n, cx)
    }

    /// フレーム `start` からの全チャンネルの標本を食べる（頭から順に、重ならず）。
    pub fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        cx: &AnalyzeCx<'_>,
    ) -> Result<(), EngineError> {
        cx.check()?;
        let m = planes.first().map_or(0, |p| p.len());
        if start != self.consumed
            || planes.len() != self.machine.channels()
            || planes.iter().any(|p| p.len() != m)
        {
            return Err(EngineError::Internal(
                "状態の表の流れの窓が順でないか、形が違います".into(),
            ));
        }
        let end = start
            .checked_add(m as u64)
            .filter(|e| *e <= self.frames.max())
            .ok_or_else(|| EngineError::Internal("状態の表の流れが長さを超えました".into()))?;
        let w = self.machine.lane_width();
        let flags = par_blocks(self.lanes.len(), cx.mon, |lane| {
            let chans = &planes[lane * w..(lane + 1) * w];
            let mut clean: Vec<Vec<f32>> = Vec::with_capacity(w);
            let mut bad = false;
            for p in chans {
                let mut c = crate::codec::try_vec(p.len(), 0f32)?;
                for (o, x) in c.iter_mut().zip(p.iter()) {
                    if x.is_finite() {
                        *o = *x;
                    } else {
                        bad = true;
                    }
                }
                clean.push(c);
            }
            let refs: Vec<&[f32]> = clean.iter().map(Vec::as_slice).collect();
            let mut lb = self.lanes[lane]
                .lock()
                .map_err(|_| EngineError::Internal("状態の表の作りかけの錠が壊れました".into()))?;
            self.consume(lane, &mut lb, start, Some(&refs), m)?;
            Ok(bad)
        })?;
        self.non_finite |= flags.into_iter().any(|b| b);
        self.consumed = end;
        Ok(())
    }

    /// 組 `lane` で、絶対位置 `pos` から `m` 個の入力（None は 0）を食べ、格子の境（kG + after）に来るたびに状態を書く。
    fn consume(
        &self,
        lane: usize,
        lb: &mut LaneBuild,
        pos: u64,
        input: Option<&[&[f32]]>,
        m: usize,
    ) -> Result<(), EngineError> {
        let after = self.machine.after();
        let w = self.machine.lane_width();
        let zeros: Vec<Vec<f32>> = if input.is_none() {
            (0..w)
                .map(|_| crate::codec::try_vec(m.min(BLOCK), 0f32))
                .collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        let mut t = 0usize;
        while t < m {
            let p = pos + t as u64;
            let next = self.boundary(lb.written, after)?;
            if p == next {
                self.record(lb)?;
                continue;
            }
            // 次の境の手前まで（境は p より後ろ）
            let room = usize::try_from(next - p).unwrap_or(usize::MAX);
            let mut seg = (m - t).min(room);
            if input.is_none() {
                seg = seg.min(BLOCK);
            }
            match input {
                Some(planes) => {
                    let refs: Vec<&[f32]> = planes.iter().map(|q| &q[t..t + seg]).collect();
                    self.machine.run(lane, &mut lb.st, p, &refs, None);
                }
                None => {
                    let refs: Vec<&[f32]> = zeros.iter().map(|q| &q[..seg]).collect();
                    self.machine.run(lane, &mut lb.st, p, &refs, None);
                }
            }
            t += seg;
        }
        Ok(())
    }

    /// 格子 k の境（kG + after）。
    fn boundary(&self, k: u64, after: u64) -> Result<u64, EngineError> {
        k.checked_mul(self.grid)
            .and_then(|g| g.checked_add(after))
            .ok_or_else(|| EngineError::Limit("音声が長すぎます".into()))
    }

    /// 今の状態を（NaN を揃えて）格子の状態として書く。
    fn record(&self, lb: &mut LaneBuild) -> Result<(), EngineError> {
        canonicalize(&mut lb.st);
        lb.entries
            .try_reserve(lb.st.len())
            .map_err(|_| EngineError::Limit("状態の表が大きすぎます".into()))?;
        lb.entries.extend_from_slice(&lb.st);
        lb.written += 1;
        Ok(())
    }

    /// 長さ `n` で終える：残りの格子の境まで 0 を食べて（先読みの流し切り）、表を組む。
    pub fn finish_at(self, n: u64, cx: &AnalyzeCx<'_>) -> Result<StateTable, EngineError> {
        cx.check()?;
        let ok = match self.frames {
            Dim::Exact(f) => f == n,
            Dim::AtMost(f) => n <= f,
        };
        if !ok || self.consumed != n {
            return Err(EngineError::Internal(format!(
                "状態の表の流れの長さが違います（受けた {}・終わり {n}）",
                self.consumed
            )));
        }
        let need = grid_count(n, self.grid);
        let after = self.machine.after();
        let lanes = self.lanes.len();
        // 入力の中の境（kG + after < n）は流す間に書いてある。残りの境（先読みで入力の終わりより後ろ）まで 0 を食べて書く
        let built = par_blocks(lanes, cx.mon, |lane| {
            let mut lb = self.lanes[lane]
                .lock()
                .map_err(|_| EngineError::Internal("状態の表の作りかけの錠が壊れました".into()))?;
            let mut pos = self.consumed;
            while lb.written < need {
                let target = self.boundary(lb.written, after)?;
                if pos < target {
                    let m = usize::try_from(target - pos)
                        .map_err(|_| EngineError::Limit("先読みが長すぎます".into()))?;
                    self.consume(lane, &mut lb, pos, None, m)?;
                    pos = target;
                }
                if pos != target {
                    return Err(EngineError::Internal(
                        "状態の表の格子の境を書き落としました".into(),
                    ));
                }
                self.record(&mut lb)?;
            }
            Ok(std::mem::take(&mut lb.entries))
        })?;
        let per = self.machine.state_len();
        let need = usize::try_from(need)
            .map_err(|_| EngineError::Limit("状態の表が大きすぎます".into()))?;
        let total = need
            .checked_mul(lanes)
            .and_then(|e| e.checked_mul(per))
            .ok_or_else(|| EngineError::Limit("状態の表が大きすぎます".into()))?;
        let mut states = Vec::new();
        states
            .try_reserve_exact(total)
            .map_err(|_| EngineError::Limit("状態の表が大きすぎます".into()))?;
        for k in 0..need {
            for lane in &built {
                let row = lane
                    .get(k * per..(k + 1) * per)
                    .ok_or_else(|| EngineError::Internal("状態の表の組の長さが違います".into()))?;
                states.extend_from_slice(row);
            }
        }
        Ok(StateTable {
            grid: self.grid,
            frames: n,
            after,
            lanes: u32::try_from(lanes).map_err(|_| EngineError::Limit("組が多すぎます".into()))?,
            state_len: u32::try_from(per)
                .map_err(|_| EngineError::Limit("状態が大きすぎます".into()))?,
            non_finite: self.non_finite,
            states,
        })
    }
}

/// 流れの作り手（`Analysis::stream` の `AudioStream`）。
pub struct TableStream(pub TableBuilder);

impl AudioStream for TableStream {
    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        cx: &AnalyzeCx<'_>,
    ) -> Result<(), EngineError> {
        self.0.feed(start, planes, cx)
    }

    fn finish(
        self: Box<Self>,
        end: &StreamEnd,
        cx: &AnalyzeCx<'_>,
    ) -> Result<AnyOutput, EngineError> {
        Ok(Arc::new(self.0.finish_at(end.frames, cx)?))
    }
}

/// 入力の形から流れの作り手を作る（音声でない・チャンネルの数が違えば None）。
pub fn table_stream(
    machine: Arc<dyn Stateful>,
    grid: u64,
    input: &Shape,
) -> Option<Box<dyn AudioStream>> {
    let Shape::Audio {
        channels, frames, ..
    } = *input
    else {
        return None;
    };
    if usize::from(channels) != machine.channels() {
        return None;
    }
    TableBuilder::new(machine, grid, frames)
        .ok()
        .map(|b| Box::new(TableStream(b)) as Box<dyn AudioStream>)
}

/// 表の見積もり（大きさは正規の形の長さ、手間はフレーム × チャンネル × `per_frame`）。
pub fn table_estimate(
    grid: u64,
    input: &Shape,
    lanes: u64,
    state_len: u64,
    per_frame: u64,
) -> Estimate {
    let Shape::Audio {
        channels, frames, ..
    } = *input
    else {
        return Estimate::default();
    };
    Estimate {
        bytes: table_bytes(frames.max(), grid, lanes, state_len).unwrap_or(u64::MAX),
        cost: frames
            .max()
            .saturating_mul(u64::from(channels))
            .saturating_mul(per_frame.max(1)),
    }
}

/// 覚え：組の直前の描画が終わった位置と、そのときの状態（格子の区間 `interval` の中で流したもの）。
struct Memo {
    interval: u64,
    end: u64,
    st: Vec<f64>,
}

/// 表から区間を描く核（docs/editing.md §18.1）。出力の長さ・チャンネル・レートは入力と同じ。
pub struct StatefulKernel {
    machine: Arc<dyn Stateful>,
    table: Arc<StateTable>,
    input: Arc<Audio>,
    memo: Vec<Mutex<Option<Memo>>>,
}

impl StatefulKernel {
    /// 核を作る。表の形（格子の数・組・状態の語・先読み・長さ）が処理と入力に合わなければ `Internal`（申告と使い方の
    /// 食い違い・偽の値の誤り）。
    pub fn new(
        machine: Arc<dyn Stateful>,
        table: Arc<StateTable>,
        input: Arc<Audio>,
    ) -> Result<Self, EngineError> {
        let lanes = lane_count(machine.as_ref())?;
        if input.channels() != machine.channels()
            || table.frames() != input.frames() as u64
            || table.lanes() != lanes
            || table.state_len() != machine.state_len()
            || table.after() != machine.after()
            || table.states.len() as u64
                != table
                    .entries()
                    .saturating_mul(lanes as u64)
                    .saturating_mul(machine.state_len() as u64)
        {
            return Err(EngineError::Internal(
                "状態の表の形が入力・引数と合いません".into(),
            ));
        }
        Ok(Self {
            machine,
            table,
            input,
            memo: (0..lanes).map(|_| Mutex::new(None)).collect(),
        })
    }

    /// 組 `lane` の入力 [from, to)（絶対位置。終わりの後ろは 0）を食べて `st` を進め、`out` があれば出力を書く
    /// （`out[c][off..]` に、食べた順に）。
    fn eat(
        &self,
        lane: usize,
        st: &mut [f64],
        from: u64,
        to: u64,
        mut out: Option<(&mut [&mut [f32]], usize)>,
    ) -> Result<(), EngineError> {
        let w = self.machine.lane_width();
        let len = usize::try_from(to.saturating_sub(from)).map_or(BLOCK, |l| l.min(BLOCK));
        let mut bufs: Vec<Vec<f32>> = (0..w)
            .map(|_| crate::codec::try_vec(len, 0f32))
            .collect::<Result<_, _>>()?;
        let mut p = from;
        while p < to {
            let m = usize::try_from((to - p).min(len as u64)).unwrap_or(len);
            for (c, b) in bufs.iter_mut().enumerate() {
                self.input.read(lane * w + c, p, &mut b[..m])?;
                for x in b[..m].iter_mut() {
                    if !x.is_finite() {
                        *x = 0.0;
                    }
                }
            }
            let refs: Vec<&[f32]> = bufs.iter().map(|b| &b[..m]).collect();
            match out.as_mut() {
                Some((outs, off)) => {
                    let mut slices: Vec<&mut [f32]> =
                        outs.iter_mut().map(|o| &mut o[*off..*off + m]).collect();
                    self.machine
                        .run(lane, st, p, &refs, Some(slices.as_mut_slice()));
                    *off += m;
                }
                None => self.machine.run(lane, st, p, &refs, None),
            }
            p += m as u64;
        }
        Ok(())
    }

    /// 組 `lane` の出力 [a, a + m) を描く（`outs` は組のチャンネルの順・長さ m）。
    fn draw(&self, lane: usize, a: u64, outs: &mut [&mut [f32]]) -> Result<(), EngineError> {
        let m = outs.first().map_or(0, |o| o.len());
        if m == 0 {
            return Ok(());
        }
        let b = a
            .checked_add(m as u64)
            .filter(|b| *b <= self.table.frames())
            .ok_or_else(|| EngineError::Internal("核の区間が長さを超えています".into()))?;
        let (g, after) = (self.table.grid(), self.table.after());
        // 覚えを取り出す（錠は計算の間は持たない。壊れていれば覚えなし）
        let mut memo = match self.memo[lane].lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        let mut st: Vec<f64> = Vec::new();
        let mut s = a;
        let mut last = None;
        while s < b {
            let k = s / g;
            let e = b.min(k.saturating_add(1).saturating_mul(g));
            // 起点：同じ格子の区間の覚え（終わり ≤ s）か、表の格子 k の状態
            let from = match memo.take() {
                Some(mm)
                    if mm.interval == k && mm.end <= s && mm.st.len() == self.table.state_len() =>
                {
                    st = mm.st;
                    mm.end
                }
                _ => {
                    st.clear();
                    st.extend_from_slice(self.table.state(k, lane)?);
                    k * g
                }
            };
            let fa = from.saturating_add(after);
            let sa = s.saturating_add(after);
            self.eat(lane, &mut st, fa, sa, None)?;
            self.eat(
                lane,
                &mut st,
                sa,
                e.saturating_add(after),
                Some((&mut *outs, usize::try_from(s - a).unwrap_or(0))),
            )?;
            last = Some((k, e));
            s = e;
        }
        // 有限でない入力の位置は、元の値をそのまま出す
        if self.table.non_finite() {
            let w = self.machine.lane_width();
            let mut raw = crate::codec::try_vec(m, 0f32)?;
            for (c, o) in outs.iter_mut().enumerate() {
                self.input.read(lane * w + c, a, &mut raw)?;
                for (y, x) in o.iter_mut().zip(raw.iter()) {
                    if !x.is_finite() {
                        *y = *x;
                    }
                }
            }
        }
        if let Some((interval, end)) = last
            && let Ok(mut slot) = self.memo[lane].lock()
        {
            *slot = Some(Memo { interval, end, st });
        }
        Ok(())
    }
}

impl AudioKernel for StatefulKernel {
    fn rate(&self) -> u32 {
        self.input.rate()
    }

    fn channels(&self) -> usize {
        self.input.channels()
    }

    fn frames(&self) -> u64 {
        self.input.frames() as u64
    }

    fn lane_width(&self) -> usize {
        self.machine.lane_width()
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError> {
        let w = self.machine.lane_width();
        let (lane, at) = (ch / w, ch % w);
        if w == 1 {
            return self.draw(lane, start, &mut [out]);
        }
        let mut others: Vec<Vec<f32>> = (0..w)
            .map(|c| crate::codec::try_vec(if c == at { 0 } else { out.len() }, 0f32))
            .collect::<Result<_, _>>()?;
        let mut outs: Vec<&mut [f32]> = Vec::with_capacity(w);
        let mut out = Some(out);
        for (c, o) in others.iter_mut().enumerate() {
            if c == at {
                if let Some(x) = out.take() {
                    outs.push(x);
                }
            } else {
                outs.push(o.as_mut_slice());
            }
        }
        self.draw(lane, start, &mut outs)
    }

    fn render_lane(
        &self,
        lane: usize,
        start: u64,
        outs: &mut [&mut [f32]],
    ) -> Result<(), EngineError> {
        if outs.len() != self.machine.lane_width() {
            return Err(EngineError::Internal("組の出力の数が幅と違います".into()));
        }
        self.draw(lane, start, outs)
    }
}

/// 状態を運ぶ操作の作業領域（`Op::work_bytes`）：組ごとの覚え（`shared`）と、仕事ごとの読みの列（幅 × [`BLOCK`] × 4）・
/// 組のほかのチャンネルの出力（幅 × チャンク × 4）・核の描き手の分（`render_all_work` の幅倍）。
pub fn stateful_work(out: &Shape, width: u64, lanes: u64, state_len: u64) -> crate::op::Work {
    let base = crate::audio::render_all_work(out);
    let chunk = match out {
        Shape::Audio { frames, .. } => frames.max().min(AUDIO_CHUNK as u64),
        _ => 0,
    };
    crate::op::Work {
        shared: lanes.saturating_mul(state_len).saturating_mul(16),
        per_task: base
            .per_task
            .saturating_mul(width.max(1))
            .saturating_add(width.saturating_mul(BLOCK as u64 * 4 * 2))
            .saturating_add(width.saturating_mul(chunk).saturating_mul(4))
            .saturating_add(state_len.saturating_mul(8)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::testing::stateful::{Iir1, Iir1Params, reference};
    use rand_chacha::rand_core::Rng;

    fn noise(rate: u32, channels: u16, frames: u64, seed: u64) -> Arc<Audio> {
        match (crate::testgen::InputGen::AudioNoise {
            rate,
            channels,
            frames,
            seed,
        })
        .generate()
        .unwrap()
        {
            Value::Audio(a) => a,
            _ => panic!("音声ではありません"),
        }
    }

    fn params(lookahead: u64, link: bool, grid: u64) -> Iir1Params {
        Iir1Params {
            b0: 0.4,
            b1: -0.1,
            a1: -0.8,
            lookahead,
            link,
            grid,
            wet: 0.9,
            gain_db: 1.5,
        }
    }

    fn table(p: &Iir1Params, a: &Arc<Audio>) -> Arc<StateTable> {
        let limits = crate::Limits::default();
        let cx = AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        let m = Arc::new(Iir1::new(a.channels(), &p.state(), 1.0, 1.0));
        Arc::new(TableBuilder::solve(m, p.grid, &Value::Audio(a.clone()), &cx).unwrap())
    }

    fn kernel(p: &Iir1Params, a: &Arc<Audio>, t: Arc<StateTable>) -> StatefulKernel {
        let m = Iir1::new(
            a.channels(),
            &p.state(),
            p.wet,
            crate::math::db_to_gain(p.gain_db),
        );
        StatefulKernel::new(Arc::new(m), t, a.clone()).unwrap()
    }

    /// 区間を `spans` の順に描いて、全体の平面にする（チャンネルごとに `render` で）。
    fn draw(k: &dyn AudioKernel, spans: &[(u64, u64)]) -> Vec<Vec<f32>> {
        let n = k.frames() as usize;
        let mut out = vec![vec![f32::NAN; n]; k.channels()];
        for &(a, b) in spans {
            for (ch, plane) in out.iter_mut().enumerate() {
                k.render(ch, a, &mut plane[a as usize..b as usize]).unwrap();
            }
        }
        out
    }

    fn bits(p: &[Vec<f32>]) -> Vec<Vec<u32>> {
        p.iter()
            .map(|c| c.iter().map(|x| x.to_bits()).collect())
            .collect()
    }

    /// 区間を切り方・順によらずに描いても、先頭から順に流した参照とビット一致（格子に 1・5・3·4,096・C・2C、先読み、リンク、
    /// 1〜3 チャンネル）。順不同の区間は覚えが使えず、表から流し直す道を通る。
    #[test]
    fn regions_in_any_order_match_the_reference() {
        let c = AUDIO_CHUNK as u64;
        let mut r = crate::determinism::rng_at(31, 0, 0);
        let mut below = |n: u64| r.next_u64() % n.max(1);
        for (grid, lookahead, link, channels, frames) in [
            (1, 0, false, 1, 900),
            (5, 3, true, 2, 4_000),
            (3 * 4_096, 37, false, 2, 30_000),
            (c, 0, true, 3, c + 500),
            (2 * c, 300, false, 1, 2 * c + 77),
            (4_096, 5_000, true, 2, 3_000),
        ] {
            let a = noise(8_000, channels, frames, grid ^ lookahead);
            let p = params(lookahead, link, grid);
            let want = bits(&reference(&p, &a).unwrap());
            let k = kernel(&p, &a, table(&p, &a));
            // 全体を 1 度に、と順に切った区間
            assert_eq!(bits(&draw(&k, &[(0, frames)])), want, "全体 {grid}");
            let mut cuts = vec![0u64];
            while *cuts.last().unwrap() < frames {
                let at = *cuts.last().unwrap();
                cuts.push((at + 1 + below(frames / 5 + 1)).min(frames));
            }
            let spans: Vec<(u64, u64)> = cuts.windows(2).map(|w| (w[0], w[1])).collect();
            assert_eq!(bits(&draw(&k, &spans)), want, "順 {grid}");
            // 順不同（逆順と乱数の順）
            let mut rev = spans.clone();
            rev.reverse();
            assert_eq!(bits(&draw(&k, &rev)), want, "逆順 {grid}");
            let mut shuffled = spans.clone();
            for i in (1..shuffled.len()).rev() {
                let j = below(i as u64 + 1) as usize;
                shuffled.swap(i, j);
            }
            assert_eq!(bits(&draw(&k, &shuffled)), want, "乱数の順 {grid}");
            // 組を一度に描く口も同じ
            let w = k.lane_width();
            for lane in 0..k.channels() / w {
                let mut got = vec![vec![0f32; frames as usize]; w];
                for &(s, e) in &shuffled {
                    let mut refs: Vec<&mut [f32]> = got
                        .iter_mut()
                        .map(|g| &mut g[s as usize..e as usize])
                        .collect();
                    k.render_lane(lane, s, &mut refs).unwrap();
                }
                assert_eq!(
                    bits(&got),
                    want[lane * w..(lane + 1) * w].to_vec(),
                    "組 {lane}"
                );
            }
        }
    }

    /// 核の値は「表と入力」だけの関数：偽の表（状態をずらしたもの）でも、描き方・順・覚えの有無によらず同じ値（E26 の偽の値で
    /// 核の経路と apply の経路を照らせる）。
    #[test]
    fn the_kernel_is_a_function_of_the_table() {
        let a = noise(8_000, 2, 30_001, 9);
        let p = params(11, false, 4_096);
        let fake = Arc::new(table(&p, &a).perturbed(|x| x + 0.25, false));
        let k = kernel(&p, &a, fake.clone());
        let whole = bits(&draw(&k, &[(0, 30_001)]));
        let k = kernel(&p, &a, fake.clone());
        let ones: Vec<(u64, u64)> = (0..30_001u64)
            .step_by(7)
            .map(|s| (s, (s + 7).min(30_001)))
            .collect();
        assert_eq!(bits(&draw(&k, &ones)), whole);
        let k = kernel(&p, &a, fake);
        let mut back = ones.clone();
        back.reverse();
        assert_eq!(bits(&draw(&k, &back)), whole);
        // 偽の表は参照と違う値になる（表を使っている）
        assert_ne!(whole, bits(&reference(&p, &a).unwrap()));
    }

    /// 有限でない入力は 0 として食べ、出力のその位置は元のビット列のまま。表は旗を持つ。
    #[test]
    fn non_finite_inputs_are_fed_as_zero_and_passed_through() {
        let a = noise(8_000, 2, 9_000, 4);
        let mut planes: Vec<Vec<f32>> = (0..2)
            .map(|ch| {
                let mut p = vec![0f32; 9_000];
                a.read(ch, 0, &mut p).unwrap();
                p
            })
            .collect();
        let payload = f32::from_bits(0x7FC0_1234);
        planes[0][17] = payload;
        planes[1][4_096] = f32::INFINITY;
        let zeroed: Vec<Vec<f32>> = planes
            .iter()
            .map(|p| {
                p.iter()
                    .map(|x| if x.is_finite() { *x } else { 0.0 })
                    .collect()
            })
            .collect();
        let with = Arc::new(
            Audio::new(
                8_000,
                planes.into_iter().map(Arc::new).collect(),
                Default::default(),
            )
            .unwrap(),
        );
        let without = Arc::new(
            Audio::new(
                8_000,
                zeroed.into_iter().map(Arc::new).collect(),
                Default::default(),
            )
            .unwrap(),
        );
        let p = params(2, true, 1_000);
        let t = table(&p, &with);
        assert!(t.non_finite() && !table(&p, &without).non_finite());
        let got = draw(&kernel(&p, &with, t), &[(0, 9_000)]);
        let clean = draw(&kernel(&p, &without, table(&p, &without)), &[(0, 9_000)]);
        for ch in 0..2 {
            for i in 0..9_000 {
                let want = match (ch, i) {
                    (0, 17) => payload.to_bits(),
                    (1, 4_096) => f32::INFINITY.to_bits(),
                    _ => clean[ch][i].to_bits(),
                };
                assert_eq!(got[ch][i].to_bits(), want, "{ch} {i}");
            }
        }
    }

    /// 状態に NaN（揃っていないビット列）が出る処理でも、表の NaN は揃い、表から再開しても続けて流しても同じビット列。
    #[test]
    fn nan_states_are_canonical() {
        struct Poison;
        impl Stateful for Poison {
            fn channels(&self) -> usize {
                1
            }
            fn lane_width(&self) -> usize {
                1
            }
            fn state_len(&self) -> usize {
                1
            }
            fn after(&self) -> u64 {
                0
            }
            fn init(&self, _: usize, st: &mut [f64]) {
                st[0] = 0.0;
            }
            fn run(
                &self,
                _: usize,
                st: &mut [f64],
                pos: u64,
                input: &[&[f32]],
                mut out: Option<&mut [&mut [f32]]>,
            ) {
                for (t, x) in input[0].iter().enumerate() {
                    if pos + t as u64 == 50 {
                        st[0] = f64::from_bits(0x7FF8_0000_0000_0042);
                    } else {
                        st[0] = st[0] * 0.5 + f64::from(*x);
                    }
                    if let Some(o) = out.as_mut() {
                        o[0][t] = st[0] as f32;
                    }
                }
            }
        }
        let a = noise(8_000, 1, 300, 2);
        let limits = crate::Limits::default();
        let cx = AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        let t = TableBuilder::solve(Arc::new(Poison), 100, &Value::Audio(a.clone()), &cx).unwrap();
        assert_eq!(t.state(1, 0).unwrap()[0].to_bits(), CANONICAL_NAN);
        let bytes = t.to_canonical().unwrap();
        assert_eq!(StateTable::from_canonical(&bytes).unwrap(), t);
        // 揃っていない NaN は正規の形でないので断る
        let mut odd = bytes.clone();
        let at = HEADER_BYTES + 8;
        odd[at..at + 8].copy_from_slice(&0x7FF8_0000_0000_0042u64.to_le_bytes());
        assert!(matches!(
            StateTable::from_canonical(&odd),
            Err(EngineError::Decode { .. })
        ));
        let k = StatefulKernel::new(Arc::new(Poison), Arc::new(t), a.clone()).unwrap();
        let whole = bits(&draw(&k, &[(0, 300)]));
        let ones: Vec<(u64, u64)> = (0..300).map(|s| (s, s + 1)).collect();
        assert_eq!(bits(&draw(&k, &ones)), whole);
    }

    /// 正規の形の読み手は、壊れたバイト列（ChaCha8 で反転・切り詰め・差し込み・欄の書き換え）で panic せず、`Internal` を
    /// 返さず、読めたものは書き直すと同じバイト列。形の誤り（印・版・旗・格子 0・格子の数・長さ）はそれぞれ断る。
    #[test]
    fn the_reader_refuses_broken_tables() {
        let a = noise(8_000, 2, 10_000, 3);
        let p = params(3, true, 2_048);
        let good = table(&p, &a).to_canonical().unwrap();
        let set = |b: &mut Vec<u8>, at: usize, v: &[u8]| b[at..at + v.len()].copy_from_slice(v);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("印", {
                let mut b = good.clone();
                b[0] ^= 1;
                b
            }),
            ("版", {
                let mut b = good.clone();
                set(&mut b, 8, &2u32.to_le_bytes());
                b
            }),
            ("旗", {
                let mut b = good.clone();
                set(&mut b, 12, &2u32.to_le_bytes());
                b
            }),
            ("格子 0", {
                let mut b = good.clone();
                set(&mut b, 16, &0u64.to_le_bytes());
                b
            }),
            ("格子の数", {
                let mut b = good.clone();
                set(&mut b, 48, &99u64.to_le_bytes());
                b
            }),
            ("長さ", good[..good.len() - 8].to_vec()),
            ("短い", good[..10].to_vec()),
            ("大きい状態", {
                let mut b = good.clone();
                set(&mut b, 44, &u32::MAX.to_le_bytes());
                b
            }),
        ];
        for (name, b) in cases {
            assert!(
                matches!(
                    StateTable::from_canonical(&b),
                    Err(EngineError::Decode { .. })
                ),
                "{name}"
            );
        }
        let mut r = crate::determinism::rng_at(77, 0, 0);
        for _ in 0..2_000 {
            let mut b = good.clone();
            for _ in 0..1 + r.next_u32() % 4 {
                let at = r.next_u32() as usize % b.len().max(1);
                match r.next_u32() % 4 {
                    0 => b[at] ^= 1 << (r.next_u32() % 8),
                    1 => b.truncate(at),
                    2 => b.insert(at, r.next_u32() as u8),
                    _ => {
                        if at + 8 <= b.len() {
                            b[at..at + 8].copy_from_slice(&r.next_u64().to_le_bytes());
                        }
                    }
                }
                if b.is_empty() {
                    break;
                }
            }
            match crate::testing::catch_own_panic(|| StateTable::from_canonical(&b)) {
                Err(at) => panic!("読み手が panic した：{at}"),
                Ok(Ok(t)) => assert_eq!(t.to_canonical().unwrap(), b),
                Ok(Err(EngineError::Internal(m))) => panic!("Internal：{m}"),
                Ok(Err(_)) => {}
            }
        }
    }

    /// 流れの作り手は、順でない窓・形の違う窓・長さを超える窓・終わりの長さの違いを `Internal` で断る。核は形の合わない表を
    /// 断る。組の幅がチャンネルの数を割り切らない処理は作れない。
    #[test]
    fn shapes_are_checked() {
        let limits = crate::Limits::default();
        let cx = AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        let p = params(0, false, 100);
        let m = || Arc::new(Iir1::new(2, &p.state(), 1.0, 1.0)) as Arc<dyn Stateful>;
        let x = [0.1f32; 10];
        let mut b = TableBuilder::new(m(), 100, Dim::Exact(20)).unwrap();
        assert!(matches!(
            b.feed(5, &[&x, &x], &cx),
            Err(EngineError::Internal(_))
        ));
        assert!(matches!(
            b.feed(0, &[&x], &cx),
            Err(EngineError::Internal(_))
        ));
        b.feed(0, &[&x, &x], &cx).unwrap();
        b.feed(10, &[&x, &x], &cx).unwrap();
        assert!(matches!(
            b.feed(20, &[&x[..1], &x[..1]], &cx),
            Err(EngineError::Internal(_))
        ));
        let b2 = TableBuilder::new(m(), 100, Dim::Exact(20)).unwrap();
        assert!(matches!(
            b2.finish_at(20, &cx),
            Err(EngineError::Internal(_))
        ));
        let t = b.finish_at(20, &cx).unwrap();
        assert_eq!(t.entries(), 1);
        let a = noise(8_000, 2, 21, 1);
        assert!(matches!(
            StatefulKernel::new(m(), Arc::new(t), a),
            Err(EngineError::Internal(_))
        ));
        struct Odd;
        impl Stateful for Odd {
            fn channels(&self) -> usize {
                3
            }
            fn lane_width(&self) -> usize {
                2
            }
            fn state_len(&self) -> usize {
                1
            }
            fn after(&self) -> u64 {
                0
            }
            fn init(&self, _: usize, _: &mut [f64]) {}
            fn run(
                &self,
                _: usize,
                _: &mut [f64],
                _: u64,
                _: &[&[f32]],
                _: Option<&mut [&mut [f32]]>,
            ) {
            }
        }
        assert!(TableBuilder::new(Arc::new(Odd), 10, Dim::Exact(5)).is_err());
    }
}
