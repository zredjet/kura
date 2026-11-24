//! スペクトルの区間の平均と最大（M17 段 4。`edit_spec_average`。docs/editing.md §18.4）。
//!
//! - 区間は出力のフレーム [start, end) で受け、中心がその中に入るフレーム（`kura-spec/1` のフレーム）の帯の値 P を使う。
//!   平均はパワーの領域で、チャンクごとの部分和（チャンクの中はフレームの順に f64 で足したもの）をチャンクの順に足して
//!   フレームの数で割る。最大は帯ごとの P の最大。どちらも dB（f32。−120 dB で止める）で返す。値は区間と出力だけで決まり、
//!   要約を覚えているか・どこで区切ったかによらない。
//! - 区間にすっぽり入るチャンクは、チャンクの要約（帯ごとの和・最大・フレームの数。タイルを作るときにも覚える。
//!   [`SUMMARY_BYTES`] の LRU）を使い、端のチャンクだけフレームを求める。選び直しても端のチャンクだけで済む。
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
use super::audio_shape;
use super::slice::{Limit, Pace, WorkProgress};
use super::spec::{SpecSide, TileKey, chan_code, key_hex};

/// チャンクの要約の置き場の上限（バイト）。48 kHz の 1 チャンネルで 1 チャンク 8 KB ほど（約 500 チャンク・12 分）。
pub const SUMMARY_BYTES: u64 = 4 << 20;
/// 覚える結果の数。
pub const AVERAGE_RESULTS: usize = 8;
/// 予約できる区間の数（前と後）。
pub const MAX_AVERAGE_RUNS: usize = 2;
/// その場で求める見積もりの上限。
pub const SYNC_BUDGET: Duration = Duration::from_millis(25);
/// 1 回のまとまりで求めるチャンクの数の上限。
const MAX_GROUP_CHUNKS: u64 = 8;

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

/// チャンクの要約の置き場（使った順の LRU）。
#[derive(Default)]
pub(crate) struct SummaryStore {
    map: BTreeMap<TileKey, (u64, Arc<Summary>)>,
    order: BTreeMap<u64, TileKey>,
    tick: u64,
    bytes: u64,
}

impl SummaryStore {
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
        while self.bytes > SUMMARY_BYTES {
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
    key: [u8; 16],
    params: Arc<SpecParams>,
    /// ポートの長さ。
    len: u64,
    /// フレームの区間。
    j0: u64,
    j1: u64,
    /// 次に求めるチャンクと、終わり（含まない）・頭。
    next: u64,
    end: u64,
    first: u64,
    acc: Acc,
}

impl AverageRun {
    fn average_key(&self) -> AverageKey {
        (self.key, chan_code(self.request.channel), self.j0, self.j1)
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
        let total = self.params.frames(self.len);
        let (ca, cb) = ((c * per).min(total), ((c + 1) * per).min(total));
        let (fa, fb) = (ca.max(self.j0), cb.min(self.j1));
        ((fa, fb), fa == ca && fb == cb)
    }

    /// 要約にないチャンク（端と、すっぽり入るが要約のないもの）の数。
    fn missing(&self, summaries: &SummaryStore) -> u64 {
        let code = chan_code(self.request.channel);
        (self.next..self.end)
            .filter(|&c| !(self.chunk_frames(c).1 && summaries.contains(&(self.key, code, c))))
            .count() as u64
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
        while run.next < run.end {
            let c = run.next;
            let ((fa, fb), whole) = run.chunk_frames(c);
            if fa >= fb {
                run.next += 1;
                continue;
            }
            if whole
                && let Some(s) = st
                    .audio
                    .as_mut()
                    .and_then(|a| a.spec.summaries.get(&(run.key, code, c)))
            {
                run.acc.add(&s.sum, &s.max, s.frames);
                run.next += 1;
                continue;
            }
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let pace = st.audio.as_ref().map(|a| a.spec.pace).unwrap_or_default();
            let n = grant(&pace, MAX_GROUP_CHUNKS);
            if n == 0 {
                return Ok(false);
            }
            let started = Instant::now();
            let done = if whole {
                // すっぽり入って要約のないチャンクを続けて n まで（タイルも作って置く）
                let mut c1 = c + 1;
                while c1 < run.end && c1 - c < n {
                    let has = st
                        .audio
                        .as_ref()
                        .is_some_and(|a| a.spec.summaries.contains(&(run.key, code, c1)));
                    if !run.chunk_frames(c1).1 || has {
                        break;
                    }
                    c1 += 1;
                }
                let (a, b) = run.params.needed_span(c, c1, run.len);
                let planes = self.output_samples(st, plan, None, a, b, mon)?;
                let input = SpecInput {
                    start: a,
                    len: run.len,
                    planes: &planes,
                };
                let params = run.params.clone();
                let chunks = self
                    .runtime
                    .preview(|_| params.chunks(&input, chan, c, c1, mon))?;
                self.keep_chunks(st, &params, chan, run.key, &chunks);
                for ch in &chunks {
                    run.acc.add(&ch.sum, &ch.max, u64::from(ch.frames));
                }
                run.next = c1;
                c1 - c
            } else {
                // 端のチャンク：区間に入るフレームだけ
                let (a, b) = run.params.frames_span(fa, fb, run.len);
                let planes = self.output_samples(st, plan, None, a, b, mon)?;
                let input = SpecInput {
                    start: a,
                    len: run.len,
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
                run.acc.add(&sum, &max, fb - fa);
                run.next = c + 1;
                1
            };
            progress.moved = progress.moved.saturating_add(done);
            if let Some(a) = st.audio.as_mut() {
                a.spec.pace.note(done, started.elapsed());
            }
        }
        Ok(true)
    }

    /// 予約した区間の平均を裏の仕事の区切りで進める（タイルのあと。操作中は呼ばない）。`key` は今の出力の鍵で、違う鍵の
    /// 予約は捨てる。
    pub(in crate::edit::editor) fn advance_spec_average(
        &self,
        st: &mut State,
        plan: &Plan,
        key: [u8; 16],
        limit: &mut Limit,
        progress: &mut WorkProgress,
        ticket: &Ticket,
    ) {
        let Some(a) = st.audio.as_mut() else {
            return;
        };
        a.spec.average.runs.retain(|r| r.key == key);
        let mut runs = std::mem::take(&mut a.spec.average.runs);
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
    if request.node.is_some() || request.side != SpecSide::Output {
        return Err(invalid("ノードの前後のスペクトルはまだ選べません"));
    }
    let plan = st.plan.clone();
    let full = plan
        .output_shapes()
        .into_iter()
        .find(|(o, _)| *o == st.output)
        .map(|(_, s)| s)
        .ok_or_else(|| EditorError::Failed("文書に出力がありません".into()))?;
    let (rate, channels, len) =
        audio_shape(&full).ok_or_else(|| invalid("長さの分からない音声は求められません"))?;
    if !request.channel.valid_for(usize::from(channels)) {
        return Err(invalid("このチャンネルの数では選べないチャンネルです"));
    }
    let fps = editor
        .runtime
        .preview(|e| e.fingerprints(&plan, &st.inputs))
        .map_err(|e| EditorError::from_engine("出力の指紋を求められません", e))?;
    let fp = fps
        .outputs
        .iter()
        .find(|(o, _)| *o == st.output)
        .map(|(_, f)| *f)
        .ok_or_else(|| EditorError::Failed("出力の指紋がありません".into()))?;
    let mut key = [0u8; 16];
    key.copy_from_slice(&fp.0.0[..16]);
    let Some(a) = st.audio.as_mut() else {
        return Err(invalid("音声ではありません"));
    };
    let params = a
        .spec
        .params(rate)
        .map_err(|e| EditorError::from_engine("スペクトルの式を作れません", e))?;
    let (j0, j1) = frame_range(&params, request.start, request.end, len);
    let code = chan_code(request.channel);
    let avg_key = (key, code, j0, j1);
    a.spec.average.track(request, avg_key);
    if let Some(state) = a.spec.average.result(&avg_key) {
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
        key,
        params: params.clone(),
        len,
        j0,
        j1,
        next: first,
        end,
        first,
        acc: Acc::new(params.bands().len()),
    };
    // 流し解きの間（M17 段 3）はその場で求めない（今の計画は解き終えるまで流せない）
    let solving = a.solve.is_some();
    let missing = run.missing(&a.spec.summaries);
    let sync = !solving && (missing == 0 || a.spec.pace.fit(SYNC_BUDGET, missing) >= missing);
    if !sync {
        let progress = run.progress();
        a.spec.average.reserve(run);
        return Ok(SpecAverageState::Measuring { progress });
    }
    let mut progress = WorkProgress::default();
    let mon = editor.cancel.as_ref();
    let r = editor.advance_average_run(st, &plan, &mut run, &mut |_, max| max, &mut progress, mon);
    let state = match r {
        Ok(_) => SpecAverageState::Ready(run.acc.values()),
        Err(e) if matches!(e.root(), EngineError::Canceled) => return Err(EditorError::Canceled),
        Err(e) => SpecAverageState::Failed {
            message: EditorError::from_engine("スペクトルの平均を求められません", e).to_string(),
        },
    };
    if let Some(a) = st.audio.as_mut() {
        a.spec.average.remember(avg_key, state.clone());
    }
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
