//! スペクトルの区間の平均と最大（M17 段 4。`edit_spec_average`。docs/editing.md §18.4）。
//!
//! - 区間は出力のフレーム [start, end) で受け（ノードのポートなら時間の写しでポートのフレームに写した区間の包み）、中心が
//!   その中に入るフレーム（`kura-spec/1` のフレーム）の帯の値 P を使う。平均はパワーの領域で、和は 3 段で決まった順に f64 で
//!   足す：チャンクの中はフレームの順、まとまり（[`SUMMARY_GROUP`] チャンクずつ、チャンクの番号で揃える）の中はチャンクの順、
//!   全体はまとまりの順（どれも 0 から）。これをフレームの数で割る。最大は帯ごとの P の最大。どちらも dB（f32。−120 dB で
//!   止める）で返す。値は区間と出力だけで決まり、要約を覚えているか・どこで区切ったかによらない。
//! - 区間にすっぽり入るまとまりはまとまりの要約（[`GROUP_BYTES`] の LRU）、すっぽり入るチャンクはチャンクの要約（帯ごとの
//!   和・最大・フレームの数。タイルを作るときにも覚える。[`SUMMARY_BYTES`] の LRU）を使い、端のチャンクだけフレームを
//!   求める。60 分を求めたあとに選び直しても、端のまとまりの中の要約のないチャンクだけで済む。
//! - その場で求めるのは、求めるチャンクの数がタイルの作りの速さの見積もりで [`SYNC_BUDGET`] に収まるときだけ（流し解きの間は
//!   その場で求めない）。収まらなければ予約して（[`MAX_AVERAGE_RUNS`] つまで。前と後）`measuring` を返し、裏の仕事の区切りで
//!   タイルのあとに進める（操作中は進めない）。出力が変わった（鍵が違う）予約は捨てる。結果は [`AVERAGE_RESULTS`] 件の LRU。
//! - 表示の情報（`specAverage`）には、最後に頼んだ区間（向きごとに 1 つ）の状態（値は載せない。`ready` なら
//!   `edit_spec_average` を頼み直すと LRU から返る）を毎回載せる。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_engine::audio::spec::{DB_FLOOR, SpecChannel, SpecChunk, SpecInput, SpecParams};
use kura_engine::doc::NodeId;
use kura_engine::monitor::Ticket;
use kura_engine::render::Plan;
use kura_engine::{EngineError, Monitor};

use super::super::{Editor, EditorEnv, EditorError, State, editor_of};
use super::slice::{Limit, Pace, WorkProgress};
use super::spec::{Port, SpecSide, TileKey, chan_code, group_chunks, key_hex};

/// チャンクの要約の置き場の上限（バイト）。48 kHz の 1 チャンネルで 1 チャンク 8 KB ほど（約 500 チャンク・12 分）。
pub const SUMMARY_BYTES: u64 = 4 << 20;
/// まとまりの要約の置き場の上限（バイト）。48 kHz で約 260 まとまり（約 95 分）。
pub const GROUP_BYTES: u64 = 2 << 20;
/// まとまりのチャンクの数（チャンクの番号 16g〜16g + 15。和の順を決める。変えると値が変わる）。
pub const SUMMARY_GROUP: u64 = 16;
/// 覚える結果の数。
pub const AVERAGE_RESULTS: usize = 8;
/// 予約できる区間の数（前と後）。
pub const MAX_AVERAGE_RUNS: usize = 2;
/// その場で求める見積もりの上限。
pub const SYNC_BUDGET: Duration = Duration::from_millis(25);

/// チャンク 1 つの要約（帯ごとの P の和（フレームの順）・最大と、フレームの数）。
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub sum: Vec<f64>,
    pub max: Vec<f64>,
    pub frames: u64,
}

impl Summary {
    pub(crate) fn of(c: &SpecChunk) -> Self {
        Self {
            sum: c.sum.clone(),
            max: c.max.clone(),
            frames: u64::from(c.frames),
        }
    }

    fn bytes(&self) -> u64 {
        ((self.sum.len() + self.max.len()) * 8) as u64
    }
}

/// 要約の置き場（使った順の LRU。鍵の最後はチャンクかまとまりの番号）。
pub(crate) struct SummaryStore {
    map: BTreeMap<TileKey, (u64, Arc<Summary>)>,
    order: BTreeMap<u64, TileKey>,
    tick: u64,
    bytes: u64,
    cap: u64,
}

impl Default for SummaryStore {
    /// チャンクの要約の置き場。
    fn default() -> Self {
        Self::with_cap(SUMMARY_BYTES)
    }
}

/// まとまりの要約の置き場。
pub(crate) struct GroupStore(pub(crate) SummaryStore);

impl Default for GroupStore {
    fn default() -> Self {
        Self(SummaryStore::with_cap(GROUP_BYTES))
    }
}

impl SummaryStore {
    fn with_cap(cap: u64) -> Self {
        Self {
            map: BTreeMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            bytes: 0,
            cap,
        }
    }

    pub(crate) fn get(&mut self, key: &TileKey) -> Option<Arc<Summary>> {
        let (old, s) = self.map.get(key).map(|(t, s)| (*t, s.clone()))?;
        self.order.remove(&old);
        self.tick += 1;
        self.order.insert(self.tick, *key);
        if let Some(e) = self.map.get_mut(key) {
            e.0 = self.tick;
        }
        Some(s)
    }

    pub(crate) fn contains(&self, key: &TileKey) -> bool {
        self.map.contains_key(key)
    }

    pub(crate) fn put(&mut self, key: TileKey, s: Arc<Summary>) {
        if let Some((t, old)) = self.map.remove(&key) {
            self.order.remove(&t);
            self.bytes = self.bytes.saturating_sub(old.bytes());
        }
        self.tick += 1;
        self.bytes = self.bytes.saturating_add(s.bytes());
        self.order.insert(self.tick, key);
        self.map.insert(key, (self.tick, s));
        while self.bytes > self.cap {
            let Some((_, k)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, s)) = self.map.remove(&k) {
                self.bytes = self.bytes.saturating_sub(s.bytes());
            }
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// 区間の平均の頼み。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecAverageRequest {
    /// ノード（None は文書の出力）。
    pub node: Option<NodeId>,
    pub side: SpecSide,
    pub channel: SpecChannel,
    /// 出力のフレームの区間。
    pub start: u64,
    pub end: u64,
}

/// 区間の平均の値。
#[derive(Debug, Clone, PartialEq)]
pub struct SpecAverageValues {
    /// 数えたフレームの数（0 なら mean・max は空）。
    pub frames: u64,
    /// 帯ごとの平均（dB）。
    pub mean: Vec<f32>,
    /// 帯ごとの最大（dB）。
    pub max: Vec<f32>,
}

/// 区間の平均の状態。
#[derive(Debug, Clone, PartialEq)]
pub enum SpecAverageState {
    Measuring { progress: f64 },
    Ready(SpecAverageValues),
    Failed { message: String },
}

/// 表示の情報に載せる状態（値は載せない）。
#[derive(Debug, Clone, PartialEq)]
pub enum SpecAveragePhase {
    /// 予約が捨てられた（出力が変わった）。頼み直す。
    Idle,
    Measuring {
        progress: f64,
    },
    Ready,
    Failed {
        message: String,
    },
}

/// 最後に頼んだ区間の状態。
#[derive(Debug, Clone, PartialEq)]
pub struct SpecAverageStatus {
    pub request: SpecAverageRequest,
    /// 求めているポートの鍵（タイルの `key` と同じ）。
    pub key: String,
    pub phase: SpecAveragePhase,
}

/// 結果の鍵：（ポートの鍵, チャンネルの符号, フレーム j0, j1）。
type AverageKey = ([u8; 16], u16, u64, u64);

/// 足し合わせ。
#[derive(Debug, Clone)]
struct Acc {
    sum: Vec<f64>,
    max: Vec<f64>,
    frames: u64,
}

impl Acc {
    fn new(bands: usize) -> Self {
        Self {
            sum: vec![0.0; bands],
            max: vec![0.0; bands],
            frames: 0,
        }
    }

    fn add(&mut self, sum: &[f64], max: &[f64], frames: u64) {
        for (a, s) in self.sum.iter_mut().zip(sum) {
            *a += *s;
        }
        for (a, m) in self.max.iter_mut().zip(max) {
            *a = kura_engine::math::max(*a, *m);
        }
        self.frames += frames;
    }

    fn values(&self) -> SpecAverageValues {
        if self.frames == 0 {
            return SpecAverageValues {
                frames: 0,
                mean: Vec::new(),
                max: Vec::new(),
            };
        }
        let db = |p: f64| -> f32 {
            if p.is_nan() || p <= 0.0 {
                return DB_FLOOR as f32;
            }
            let d = 10.0 * kura_engine::math::log10(p);
            (if d < DB_FLOOR { DB_FLOOR } else { d }) as f32
        };
        let n = self.frames as f64;
        SpecAverageValues {
            frames: self.frames,
            mean: self.sum.iter().map(|s| db(*s / n)).collect(),
            max: self.max.iter().map(|m| db(*m)).collect(),
        }
    }
}

/// 予約した区間の求めかけ。
pub(crate) struct AverageRun {
    request: SpecAverageRequest,
    /// 求めるポート（頼んだときの計画で解いたもの）。
    port: Port,
    params: Arc<SpecParams>,
    /// フレームの区間。
    j0: u64,
    j1: u64,
    /// 次に求めるチャンクと、終わり（含まない）・頭。
    next: u64,
    end: u64,
    first: u64,
    /// 全体の和（まとまりの順）。
    acc: Acc,
    /// 足しているまとまりと、その中の和（チャンクの順）。
    group: Option<(u64, Acc)>,
}

impl AverageRun {
    fn average_key(&self) -> AverageKey {
        (
            self.port.key,
            chan_code(self.request.channel),
            self.j0,
            self.j1,
        )
    }

    fn progress(&self) -> f64 {
        if self.end <= self.first {
            1.0
        } else {
            (self.next - self.first) as f64 / (self.end - self.first) as f64
        }
    }

    /// 求めるチャンク c のフレームの区間（ポートのフレームの番号）と、チャンクがすっぽり入るか。
    fn chunk_frames(&self, c: u64) -> ((u64, u64), bool) {
        let per = self.params.frames_per_chunk();
        let total = self.params.frames(self.port.frames);
        let (ca, cb) = ((c * per).min(total), ((c + 1) * per).min(total));
        let (fa, fb) = (ca.max(self.j0), cb.min(self.j1));
        ((fa, fb), fa == ca && fb == cb)
    }

    /// まとまり g が区間にすっぽり入るか。
    fn group_whole(&self, g: u64) -> bool {
        let per = self.params.frames_per_chunk();
        let total = self.params.frames(self.port.frames);
        let ga = g
            .saturating_mul(SUMMARY_GROUP)
            .saturating_mul(per)
            .min(total);
        let gb = (g + 1)
            .saturating_mul(SUMMARY_GROUP)
            .saturating_mul(per)
            .min(total);
        ga < gb && self.j0 <= ga && gb <= self.j1
    }

    /// 要約で済まないチャンク（端と、すっぽり入るが要約のないもの。要約のあるまとまりの中は数えない）の数。
    fn missing(&self, chunks: &SummaryStore, groups: &SummaryStore) -> u64 {
        let code = chan_code(self.request.channel);
        let (mut n, mut c) = (0u64, self.next);
        while c < self.end {
            let g = c / SUMMARY_GROUP;
            if c == g * SUMMARY_GROUP
                && self.group_whole(g)
                && groups.contains(&(self.port.key, code, g))
            {
                c = (g + 1) * SUMMARY_GROUP;
                continue;
            }
            if !(self.chunk_frames(c).1 && chunks.contains(&(self.port.key, code, c))) {
                n += 1;
            }
            c += 1;
        }
        n
    }

    /// 足しているまとまりを全体の和に足して閉じる（すっぽり入るまとまりなら要約として返す）。
    fn close_group(&mut self) -> Option<(u64, Summary)> {
        let (g, acc) = self.group.take()?;
        self.acc.add(&acc.sum, &acc.max, acc.frames);
        if !self.group_whole(g) {
            return None;
        }
        Some((
            g,
            Summary {
                sum: acc.sum,
                max: acc.max,
                frames: acc.frames,
            },
        ))
    }

    /// まとまり g の和に足す（まとまりが替われば前のまとまりを閉じ、その要約を返す）。
    fn add_to_group(
        &mut self,
        g: u64,
        sum: &[f64],
        max: &[f64],
        frames: u64,
    ) -> Option<(u64, Summary)> {
        let closed = if self.group.as_ref().is_some_and(|(cur, _)| *cur != g) {
            self.close_group()
        } else {
            None
        };
        let bands = self.params.bands().len();
        let (_, acc) = self.group.get_or_insert_with(|| (g, Acc::new(bands)));
        acc.add(sum, max, frames);
        closed
    }
}

/// セッションの区間の平均の仕事（`SpecJob` の中）。
#[derive(Default)]
pub(crate) struct AverageJob {
    runs: Vec<AverageRun>,
    results: Vec<(AverageKey, SpecAverageState)>,
    /// 最後に頼んだ区間（向き（ノード・前後）ごとに 1 つ。2 つまで）と、その結果の鍵。
    tracked: Vec<(SpecAverageRequest, AverageKey)>,
}

impl AverageJob {
    fn result(&mut self, key: &AverageKey) -> Option<SpecAverageState> {
        let i = self.results.iter().position(|(k, _)| k == key)?;
        let e = self.results.remove(i);
        let state = e.1.clone();
        self.results.push(e);
        Some(state)
    }

    fn remember(&mut self, key: AverageKey, state: SpecAverageState) {
        self.results.retain(|(k, _)| *k != key);
        self.results.push((key, state));
        while self.results.len() > AVERAGE_RESULTS {
            self.results.remove(0);
        }
    }

    fn track(&mut self, request: &SpecAverageRequest, key: AverageKey) {
        let same = |r: &SpecAverageRequest| r.node == request.node && r.side == request.side;
        self.tracked.retain(|(r, _)| !same(r));
        self.tracked.push((request.clone(), key));
        while self.tracked.len() > MAX_AVERAGE_RUNS {
            self.tracked.remove(0);
        }
    }

    /// 予約する（同じ向きの予約と、上限を超えたら古いものを捨てる）。
    fn reserve(&mut self, run: AverageRun) {
        let same =
            |r: &SpecAverageRequest| r.node == run.request.node && r.side == run.request.side;
        self.runs.retain(|r| !same(&r.request));
        self.runs.push(run);
        while self.runs.len() > MAX_AVERAGE_RUNS {
            self.runs.remove(0);
        }
    }

    /// 求めかけがあるか（表示の情報の `work`）。
    pub(crate) fn running(&self) -> bool {
        !self.runs.is_empty()
    }

    /// 最後に頼んだ区間の状態。
    pub(crate) fn statuses(&self) -> Vec<SpecAverageStatus> {
        self.tracked
            .iter()
            .map(|(request, key)| {
                let phase = match self.results.iter().find(|(k, _)| k == key) {
                    Some((_, SpecAverageState::Ready(_))) => SpecAveragePhase::Ready,
                    Some((_, SpecAverageState::Failed { message })) => SpecAveragePhase::Failed {
                        message: message.clone(),
                    },
                    Some((_, SpecAverageState::Measuring { .. })) | None => {
                        match self.runs.iter().find(|r| r.average_key() == *key) {
                            Some(r) => SpecAveragePhase::Measuring {
                                progress: r.progress(),
                            },
                            None => SpecAveragePhase::Idle,
                        }
                    }
                };
                SpecAverageStatus {
                    request: request.clone(),
                    key: key_hex(&key.0),
                    phase,
                }
            })
            .collect()
    }
}

/// 出力のフレームの区間 [start, end) の、中心が入るフレームの区間（ポートの長さ `len` の内）。
fn frame_range(p: &SpecParams, start: u64, end: u64, len: u64) -> (u64, u64) {
    let (hop, total) = (p.hop() as u64, p.frames(len));
    if start >= end {
        return (0, 0);
    }
    let (j0, j1) = (start.div_ceil(hop).min(total), end.div_ceil(hop).min(total));
    if j0 >= j1 { (0, 0) } else { (j0, j1) }
}

impl Editor {
    /// 予約の求めかけを区切り `grant` の中で進める。終えたら真。
    fn advance_average_run(
        &self,
        st: &mut State,
        plan: &Plan,
        run: &mut AverageRun,
        grant: &mut dyn FnMut(&Pace, u64) -> u64,
        progress: &mut WorkProgress,
        mon: &dyn Monitor,
    ) -> Result<bool, EngineError> {
        let code = chan_code(run.request.channel);
        let chan = run.request.channel;
        let key = run.port.key;
        // 閉じたまとまりの要約を覚える
        let keep_group = |st: &mut State, closed: Option<(u64, Summary)>| {
            if let (Some((g, sm)), Some(a)) = (closed, st.audio.as_mut()) {
                a.spec.groups.0.put((key, code, g), Arc::new(sm));
            }
        };
        while run.next < run.end {
            let c = run.next;
            let g = c / SUMMARY_GROUP;
            // すっぽり入って要約のあるまとまり
            if c == g * SUMMARY_GROUP && run.group_whole(g) {
                let s = st
                    .audio
                    .as_mut()
                    .and_then(|a| a.spec.groups.0.get(&(key, code, g)));
                if let Some(s) = s {
                    let closed = run.close_group();
                    keep_group(st, closed);
                    run.acc.add(&s.sum, &s.max, s.frames);
                    run.next = ((g + 1) * SUMMARY_GROUP).min(run.end);
                    continue;
                }
            }
            let ((fa, fb), whole) = run.chunk_frames(c);
            if fa >= fb {
                run.next += 1;
                continue;
            }
            if whole
                && let Some(s) = st
                    .audio
                    .as_mut()
                    .and_then(|a| a.spec.summaries.get(&(key, code, c)))
            {
                let closed = run.add_to_group(g, &s.sum, &s.max, s.frames);
                keep_group(st, closed);
                run.next += 1;
                continue;
            }
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let pace = st.audio.as_ref().map(|a| a.spec.pace).unwrap_or_default();
            let n = grant(&pace, group_chunks(&run.params, run.port.channels));
            if n == 0 {
                return Ok(false);
            }
            let started = Instant::now();
            let done = if whole {
                // すっぽり入って要約のないチャンクを、まとまりをまたがずに続けて n まで（タイルも作って置く）
                let stop = ((g + 1) * SUMMARY_GROUP).min(run.end);
                let mut c1 = c + 1;
                while c1 < stop && c1 - c < n {
                    let has = st
                        .audio
                        .as_ref()
                        .is_some_and(|a| a.spec.summaries.contains(&(key, code, c1)));
                    if !run.chunk_frames(c1).1 || has {
                        break;
                    }
                    c1 += 1;
                }
                let (a, b) = run.params.needed_span(c, c1, run.port.frames);
                let planes = self.port_samples(st, plan, None, &run.port, a, b, mon)?;
                let input = SpecInput {
                    start: a,
                    len: run.port.frames,
                    planes: &planes,
                };
                let params = run.params.clone();
                let chunks = self
                    .runtime
                    .preview(|_| params.chunks(&input, chan, c, c1, mon))?;
                // 要約だけを覚える（タイルを置くと、見えている所のタイルを追い出す）
                self.keep_chunks(st, &params, chan, key, &chunks, false);
                for ch in &chunks {
                    let closed = run.add_to_group(g, &ch.sum, &ch.max, u64::from(ch.frames));
                    keep_group(st, closed);
                }
                run.next = c1;
                c1 - c
            } else {
                // 端のチャンク：区間に入るフレームだけ
                let (a, b) = run.params.frames_span(fa, fb, run.port.frames);
                let planes = self.port_samples(st, plan, None, &run.port, a, b, mon)?;
                let input = SpecInput {
                    start: a,
                    len: run.port.frames,
                    planes: &planes,
                };
                let params = run.params.clone();
                let rows = self
                    .runtime
                    .preview(|_| params.frame_bands(&input, chan, fa, fb, mon))?;
                let bands = params.bands().len();
                let (mut sum, mut max) = (vec![0.0f64; bands], vec![0.0f64; bands]);
                for row in &rows {
                    for (k, &p) in row.iter().enumerate() {
                        sum[k] += p;
                        max[k] = kura_engine::math::max(max[k], p);
                    }
                }
                let closed = run.add_to_group(g, &sum, &max, fb - fa);
                keep_group(st, closed);
                run.next = c + 1;
                1
            };
            progress.moved = progress.moved.saturating_add(done);
            if let Some(a) = st.audio.as_mut() {
                a.spec.pace.note(done, started.elapsed());
            }
        }
        let closed = run.close_group();
        keep_group(st, closed);
        Ok(true)
    }

    /// 予約した区間の平均を裏の仕事の区切りで進める（タイルのあと。操作中は呼ばない）。ポートの鍵が今の計画で解いた鍵と
    /// 違う予約は捨てる。
    pub(in crate::edit::editor) fn advance_spec_average(
        &self,
        st: &mut State,
        plan: &Plan,
        limit: &mut Limit,
        progress: &mut WorkProgress,
        ticket: &Ticket,
    ) {
        let Some(a) = st.audio.as_mut() else {
            return;
        };
        let mut runs = std::mem::take(&mut a.spec.average.runs);
        runs.retain(|r| {
            self.resolve_port(st, plan, r.request.node, r.request.side)
                .is_some_and(|p| p.key == r.port.key)
        });
        let mut kept = Vec::new();
        let mut stop = false;
        for mut run in runs.drain(..) {
            if stop {
                kept.push(run);
                continue;
            }
            let r = self.advance_average_run(
                st,
                plan,
                &mut run,
                &mut |pace, max| limit.grant(pace, max),
                progress,
                ticket,
            );
            match r {
                Ok(true) => {
                    let state = SpecAverageState::Ready(run.acc.values());
                    if let Some(a) = st.audio.as_mut() {
                        a.spec.average.remember(run.average_key(), state);
                    }
                }
                Ok(false) => {
                    kept.push(run);
                    stop = true;
                }
                Err(e) if matches!(e.root(), EngineError::Canceled) => {
                    kept.push(run);
                    stop = true;
                }
                // ポートに届く申告した解析がまだ手元にない：解けるまで待つ
                Err(e) if matches!(e.root(), EngineError::Unsupported(_)) => {
                    kept.push(run);
                    stop = true;
                    progress.blocked = true;
                }
                Err(e) => {
                    let state = SpecAverageState::Failed {
                        message: EditorError::from_engine("スペクトルの平均を求められません", e)
                            .to_string(),
                    };
                    progress.blocked = true;
                    if let Some(a) = st.audio.as_mut() {
                        a.spec.average.remember(run.average_key(), state);
                    }
                }
            }
        }
        if let Some(a) = st.audio.as_mut() {
            // 進めている間に足された予約（ない）より前に、残った予約を戻す
            kept.append(&mut a.spec.average.runs);
            a.spec.average.runs = kept;
        }
    }
}

/// 区間の平均（モジュールの説明）：今の rev の出力の、区間 [start, end) の帯ごとの平均と最大。
pub fn spec_average(
    env: &EditorEnv<'_>,
    session: &str,
    request: &SpecAverageRequest,
) -> Result<SpecAverageState, EditorError> {
    let invalid = |message: &str| EditorError::Invalid {
        message: message.into(),
        issues: Vec::new(),
    };
    let editor = editor_of(env, session)?;
    let mut guard = editor.lock_state();
    let st = guard.as_mut().ok_or(EditorError::NoSession)?;
    if st.plan_rev != st.session.rev() {
        return Err(invalid("今の文書の計画を作れません"));
    }
    if st.audio.is_none() {
        return Err(invalid("音声ではありません"));
    }
    let plan = st.plan.clone();
    let port = editor
        .resolve_port(st, &plan, request.node, request.side)
        .ok_or_else(|| invalid("このポートのスペクトルは求められません"))?;
    if !request.channel.valid_for(usize::from(port.channels)) {
        return Err(invalid("このチャンネルの数では選べないチャンネルです"));
    }
    let Some(a) = st.audio.as_mut() else {
        return Err(invalid("音声ではありません"));
    };
    let params = a
        .spec
        .params(port.rate)
        .map_err(|e| EditorError::from_engine("スペクトルの式を作れません", e))?;
    // 出力の区間をポートに写した区間の包み（下流の写しが飛び飛びになる操作（中を切る・つなぐ）が入ると、出力にない区間も
    // 入る。今の時間の写しの操作（trim・resample・ループ）では 1 区分）
    let mapped = port.to_port(&[(request.start, request.end)]);
    let (ps, pe) = mapped
        .iter()
        .fold((u64::MAX, 0u64), |(a, b), (x, y)| (a.min(*x), b.max(*y)));
    let (j0, j1) = frame_range(&params, ps, pe, port.frames);
    let code = chan_code(request.channel);
    let avg_key = (port.key, code, j0, j1);
    a.spec.average.track(request, avg_key);
    // 覚えた値（失敗は覚えていても求め直す：予算や源の読みの誤りは一時のことがある）
    if let Some(state) = a.spec.average.result(&avg_key)
        && matches!(state, SpecAverageState::Ready(_))
    {
        return Ok(state);
    }
    if let Some(r) = a
        .spec
        .average
        .runs
        .iter()
        .find(|r| r.average_key() == avg_key)
    {
        return Ok(SpecAverageState::Measuring {
            progress: r.progress(),
        });
    }
    let per = params.frames_per_chunk();
    let (first, end) = if j0 < j1 {
        (j0 / per, j1.div_ceil(per))
    } else {
        (0, 0)
    };
    let mut run = AverageRun {
        request: request.clone(),
        port,
        params: params.clone(),
        j0,
        j1,
        next: first,
        end,
        first,
        acc: Acc::new(params.bands().len()),
        group: None,
    };
    // 流し解きの間（M17 段 3）と、ポートに届く申告した解析が手元にないときはその場で求めない（錠の中で入力の全体を
    // 流し解かない）
    let solving = a.solve.is_some();
    let missing = run.missing(&a.spec.summaries, &a.spec.groups.0);
    let fits = missing == 0 || a.spec.pace.fit(SYNC_BUDGET, missing) >= missing;
    let sync = !solving && fits && editor.port_solved(st, &plan, &run.port);
    let reserve = |st: &mut State, run: AverageRun| -> SpecAverageState {
        let progress = run.progress();
        if let Some(a) = st.audio.as_mut() {
            a.spec.average.reserve(run);
        }
        SpecAverageState::Measuring { progress }
    };
    if !sync {
        return Ok(reserve(st, run));
    }
    let mut progress = WorkProgress::default();
    let mon = editor.cancel.as_ref();
    let r = editor.advance_average_run(st, &plan, &mut run, &mut |_, max| max, &mut progress, mon);
    let state = match r {
        Ok(_) => SpecAverageState::Ready(run.acc.values()),
        Err(e) if matches!(e.root(), EngineError::Canceled) => return Err(EditorError::Canceled),
        Err(e) if matches!(e.root(), EngineError::Unsupported(_)) => {
            // 求めかけの続きを裏の仕事で
            let state = reserve(st, run);
            editor.count_resident(st);
            return Ok(state);
        }
        Err(e) => SpecAverageState::Failed {
            message: EditorError::from_engine("スペクトルの平均を求められません", e).to_string(),
        },
    };
    if let Some(a) = st.audio.as_mut() {
        a.spec.average.remember(avg_key, state.clone());
    }
    // その場で覚えた要約を取り置きに数える
    editor.count_resident(st);
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中心が区間に入るフレーム（長さの内）。空の区間は (0, 0)。
    #[test]
    fn frames_in_the_span() {
        let p = SpecParams::for_rate(48_000).unwrap();
        let hop = p.hop() as u64;
        let len = 10 * hop + 3;
        assert_eq!(frame_range(&p, 0, 1, len), (0, 1));
        assert_eq!(frame_range(&p, 1, hop, len), (0, 0));
        assert_eq!(frame_range(&p, 1, hop + 1, len), (1, 2));
        assert_eq!(frame_range(&p, 3 * hop, 5 * hop, len), (3, 5));
        assert_eq!(frame_range(&p, 3 * hop, len + 100, len), (3, 11));
        assert_eq!(frame_range(&p, 5, 5, len), (0, 0));
        assert_eq!(frame_range(&p, len + 1, len + 9, len), (0, 0));
    }

    /// 平均はパワーの領域、最大は帯ごと、どちらも −120 dB で止める。フレームがなければ空。
    #[test]
    fn values_are_power_means() {
        let mut acc = Acc::new(2);
        assert_eq!(acc.values().mean, Vec::<f32>::new());
        acc.add(&[2.0, 0.0], &[1.0, 0.0], 1);
        acc.add(&[0.0, 0.0], &[0.5, 0.0], 1);
        let v = acc.values();
        assert_eq!(v.frames, 2);
        assert_eq!(v.mean, vec![0.0, -120.0]);
        assert_eq!(v.max, vec![0.0, -120.0]);
    }

    /// 結果の LRU と、向きごとに 1 つの頼み。
    #[test]
    fn results_and_tracking() {
        let mut job = AverageJob::default();
        let ready = || SpecAverageState::Ready(Acc::new(0).values());
        for i in 0..(AVERAGE_RESULTS as u64 + 2) {
            job.remember(([0; 16], 0, i, i + 1), ready());
        }
        assert!(job.result(&([0; 16], 0, 0, 1)).is_none());
        assert!(job.result(&([0; 16], 0, 2, 3)).is_some());
        let req = |side, start| SpecAverageRequest {
            node: None,
            side,
            channel: SpecChannel::Sum,
            start,
            end: start + 1,
        };
        job.track(&req(SpecSide::Output, 0), ([0; 16], 0, 2, 3));
        job.track(&req(SpecSide::Output, 5), ([0; 16], 0, 2, 3));
        job.track(&req(SpecSide::Input, 5), ([1; 16], 0, 9, 9));
        let st = job.statuses();
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].request.start, 5);
        assert_eq!(st[0].phase, SpecAveragePhase::Ready);
        assert_eq!(st[1].phase, SpecAveragePhase::Idle);
    }
}
