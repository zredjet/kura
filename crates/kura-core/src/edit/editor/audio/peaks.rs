//! エディタの波形の段（M13 段 4。docs/editing.md §11.4・§12.8・§12.13）。
//!
//! - **持ち方**：部分更新の元にできる段（`done`）と、そこから今の出力までに変わりうる区間（`dirty`。今の出力の座標）。
//! - **公開と世代**：段は公開のたびにセッションの中で増やす**段の世代**の番号を付けて公開し（今と直前）、
//!   `/edit/{s}/{世代}/peaks/{bin}/{tile}`（immutable）で配る。番号は文書の rev ではなく、同じ番号で違う中身を二度と公開しない
//!   （取り消しで同じ標本に戻っても新しい世代）。表示の情報の `peaks.rev` がこの番号。
//! - **土台**（開くとき）：頭が元（版 0）・入力の記録の値のダイジェストが事実の行と同じ・出力に量子化がない、をすべて満たせば、
//!   ライブラリの解析の段のファイル（`peaks/ab/<hash>.peaks`）を最初の世代として公開し、空の文書から今の文書までに変わった区間
//!   （`Engine::delta`）を `dirty` にする（手を当てていなければ空で、開いた瞬間に概観が出る）。下書きから開いて `dirty` が
//!   空でなければ、土台は手の前の波形を出す仮の段（`pending`）で、直し終えたら新しい世代で公開する。ライブラリの段は解析の
//!   ジョブの引数（`analysis::plan::peaks_params(frames)`）なので、約 22 秒を超える音声では粗い段だけ：それを仮の段として
//!   出し（`peaks.bins` は持っている段だけ）、部分更新の元にはせず全体を作る。ほかは全体を作る。
//! - **手のあと**：表示の変わった区間を `dirty` に足す（レート・チャンネルが変われば全体。長さの変化は `PeaksPatch` が尾を足す）。
//! - **直す**（[`Editor::advance_peaks`]。操作中は何もしない）：`PeaksPatch` を作り、R のうち表示の値で有効なチャンクは写さずに
//!   渡し、残りが [`PEAKS_SYNC_FRAMES`] 以下なら流して直す。それを超える分は、`edit_view { work: true }` のたびに
//!   [`WORK_SLICE`] まで、見えている区間に重なる窓から流す（裏の仕事。別のスレッドは使わない）。直し終えたら新しい世代で
//!   公開し、それまでは前の段を出す（`pending`）。
//! - **予算**：公開した段 2 つと組んでいる段の分を、表示の値と一緒に `CacheLease::extra` で数える。

use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_engine::analysis::audio::peaks::{PEAK_BINS, Peaks, PeaksParams, PeaksPatch};
use kura_engine::analysis::output::AnalysisOutput;
use kura_engine::audio::source::read_through;
use kura_engine::audio::stream::stream_windows;
use kura_engine::monitor::Ticket;
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::region::{Area, PartialValue, Spans};
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{EngineError, Monitor};

use super::super::{Editor, State, TileError, settle};
use super::{ChangedSpans, chunk_runs};
use crate::edit::EngineServices;

/// 手の中で流して直す、表示の外の汚れの上限（フレーム）。
pub const PEAKS_SYNC_FRAMES: u64 = 1 << 21;
/// 裏の仕事を 1 回の `edit_view` で進める時間。
pub const WORK_SLICE: Duration = Duration::from_millis(40);
/// 波形の段のタイル 1 枚のビンの数。
pub const TILE_BINS: u32 = 1_024;
/// タイルの並びの magic と版。
const TILE_MAGIC: &[u8; 4] = b"kpt1";

/// 変わりうる区間（今の出力の座標）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dirty {
    All,
    Spans(Spans),
}

impl Dirty {
    fn none() -> Self {
        Self::Spans(Spans::new())
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Spans(s) if s.is_empty())
    }

    fn add(&mut self, other: &Dirty) {
        match (&mut *self, other) {
            (Self::All, _) => {}
            (_, Self::All) => *self = Self::All,
            (Self::Spans(a), Self::Spans(b)) => a.union_with(b),
        }
    }

    /// エンジンの範囲から（画像の範囲は全体）。
    pub(crate) fn of_area(a: &Area) -> Self {
        match a {
            Area::Empty => Self::none(),
            Area::Samples(s) => Self::Spans(s.clone()),
            Area::All | Area::Pixels(_) => Self::All,
        }
    }

    fn of(c: &ChangedSpans) -> Self {
        match c {
            ChangedSpans::All => Self::All,
            ChangedSpans::Spans(list) => {
                let mut s = Spans::new();
                for (a, b) in list {
                    s.push(*a, *b);
                }
                Self::Spans(s)
            }
        }
    }

    fn report(&self) -> ChangedSpans {
        match self {
            Self::All => ChangedSpans::All,
            Self::Spans(s) => ChangedSpans::Spans(s.spans().to_vec()),
        }
    }
}

/// 組んでいる途中の段（`target_rev` の出力のもの）。
struct Run {
    target_rev: u64,
    patch: PeaksPatch,
}

/// 段の形（レート・チャンネル数・長さ・段のビン）。変わったらフロントに全体を取り直させる。
type Form = (u32, u16, u64, Vec<u32>);

fn form_of(p: &Peaks) -> Form {
    (
        p.rate,
        p.channels,
        p.frames,
        p.levels.iter().map(|l| l.bin).collect(),
    )
}

/// 波形の段の仕事の状態（`AudioState` が持つ）。
pub(crate) struct PeaksJob {
    params: PeaksParams,
    /// 部分更新の元にできる段（`params` の段がすべてそろい、`dirty` の外は今の出力の段と同じ）。
    done: Option<Arc<Peaks>>,
    /// `done` から今の出力までに変わりうる区間。
    dirty: Dirty,
    /// 今の出力の (rate, channels, frames)。
    shape: Option<(u32, u16, u64)>,
    run: Option<Run>,
    /// 公開した今の段（段の世代と段）。直している途中は古い段・仮の段（土台）のことがある。
    shown: Option<(u64, Arc<Peaks>)>,
    /// 次に公開する段の世代（セッションの中で公開のたびに増やす）。
    next_gen: u64,
    /// 前に返した世代から、公開した段で変わった区間（返したら空にする）。
    changed_since_reported: Dirty,
    /// 前に返した段の形（変わったら全体を取り直させる）。
    reported: Option<Form>,
    /// 次の表示の変わりを足さない（開いたときの最初の表示。土台からの差分は開くときに入れた）。
    skip_next: bool,
}

/// 表示の情報に載せる波形の段の状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeaksInfo {
    /// 公開した今の段の世代（`/edit/{s}/{rev}/peaks/{bin}/{tile}` の番号。なければ None）。文書の rev とは別の数で、
    /// セッションの中で公開のたびに増え、同じ番号の段の中身は変わらない。直している途中（`pending`）は、前の文書の段か、
    /// 開いたときの仮の段（ライブラリの解析の段）のことがある。
    pub rev: Option<u64>,
    /// その段のレート・チャンネル数・長さ。
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// その段が持つ段のビンのフレーム数（細かい順）。
    pub bins: Vec<u32>,
    /// タイル 1 枚のビンの数。
    pub tile_bins: u32,
    /// 前に返した世代の段から変わった区間（フレーム。この区間に掛かるタイルを取り直す）。
    pub changed: ChangedSpans,
    /// 直している途中（古い段・仮の段を出している）。
    pub pending: bool,
}

impl PeaksJob {
    /// 土台（ライブラリの段。変わりうる区間つき）から始める。土台は最初の世代として公開する（`dirty` が空でなければ
    /// 直し終えるまでの仮の段）。段がすべてそろっていない土台（長い音声のライブラリの粗い段だけ）は仮の段として出すだけで、
    /// 部分更新の元にはしない（全体を作る）。土台がなければ全体を作る。
    pub(crate) fn new(base: Option<(Arc<Peaks>, Dirty)>) -> Self {
        let params = PeaksParams::default();
        let mut job = Self {
            params,
            done: None,
            dirty: Dirty::All,
            shape: None,
            run: None,
            shown: None,
            next_gen: 0,
            changed_since_reported: Dirty::All,
            reported: None,
            skip_next: true,
        };
        if let Some((peaks, dirty)) = base {
            if form_of(&peaks).3 == params.bins() {
                job.done = Some(peaks.clone());
                job.dirty = dirty;
            }
            job.publish(peaks);
        }
        job
    }

    /// 段を新しい世代で今の段にし、その世代を返す。
    fn publish(&mut self, peaks: Arc<Peaks>) -> u64 {
        let generation = self.next_gen;
        self.next_gen = self.next_gen.saturating_add(1);
        self.shown = Some((generation, peaks));
        generation
    }

    /// 公開した今の段（世代と段。開いたときの土台を配るのに使う）。
    pub(crate) fn shown(&self) -> Option<(u64, Arc<Peaks>)> {
        self.shown.clone()
    }

    /// 直している途中か（`dirty` が残る・組んでいる途中）。
    pub(crate) fn pending(&self) -> bool {
        !self.dirty.is_empty() || self.run.is_some()
    }

    /// 組んでいる段の持つバイト数。
    pub(crate) fn run_bytes(&self) -> u64 {
        self.run.as_ref().map_or(0, |r| r.patch.fresh_bytes())
    }

    /// 表示を描いた：今の出力の形と変わった区間を受ける。
    pub(crate) fn note(&mut self, rev: u64, shape: (u32, u16, u64), changed: &ChangedSpans) {
        let same_kind = self
            .done
            .as_ref()
            .is_none_or(|p| p.rate == shape.0 && p.channels == shape.1);
        if self.skip_next {
            self.skip_next = false;
        } else {
            self.dirty.add(&Dirty::of(changed));
        }
        if !same_kind {
            self.dirty = Dirty::All;
        }
        self.shape = Some(shape);
        if self.run.as_ref().is_some_and(|r| r.target_rev != rev) {
            self.run = None;
        }
    }

    /// 今の状態を返す形にし、返したことにする。
    pub(crate) fn report(&mut self) -> PeaksInfo {
        let pending = self.pending();
        let Some((generation, p)) = &self.shown else {
            return PeaksInfo {
                rev: None,
                rate: 0,
                channels: 0,
                frames: 0,
                bins: self.params.bins(),
                tile_bins: TILE_BINS,
                changed: ChangedSpans::All,
                pending,
            };
        };
        let form = form_of(p);
        let changed = if self.reported.as_ref() != Some(&form) {
            ChangedSpans::All
        } else {
            self.changed_since_reported.report()
        };
        self.changed_since_reported = Dirty::none();
        let bins = form.3.clone();
        self.reported = Some(form);
        PeaksInfo {
            rev: Some(*generation),
            rate: p.rate,
            channels: p.channels,
            frames: p.frames,
            bins,
            tile_bins: TILE_BINS,
            changed,
            pending,
        }
    }

    /// 組んでいる段が R をすべて受けていれば組み、新しい世代にして返す（配る置き場への公開は呼び手）。まだ受けていない・
    /// 組めなければ None。中止（新しい要求・閉じる）は組みかけを残し（次の呼び出しで組み直す）、ほかの誤りは捨てる（次の
    /// 呼び出しで最初から作り直す）。
    fn complete(&mut self, mon: &dyn Monitor) -> Option<(u64, Arc<Peaks>)> {
        let run = self.run.as_ref()?;
        if !run.patch.missing().is_empty() {
            return None;
        }
        match run.patch.assemble(mon) {
            Ok(peaks) => {
                let target = run.patch.target().clone();
                let peaks = Arc::new(peaks);
                let generation = self.finish(peaks.clone(), &target);
                Some((generation, peaks))
            }
            Err(err) if matches!(err.root(), EngineError::Canceled) => None,
            Err(err) => {
                tracing::warn!(%err, "波形の段を組めません");
                self.run = None;
                None
            }
        }
    }

    /// 組み終えた段を受けて新しい世代にし、その世代を返す（配る置き場への公開は呼び手）。変わった区間は、前に公開した段と
    /// 形が同じなら R、違えば全体。
    fn finish(&mut self, peaks: Arc<Peaks>, target: &kura_engine::value::ChunkSet) -> u64 {
        let frames = peaks.frames;
        let same_form = self
            .shown
            .as_ref()
            .is_some_and(|(_, p)| form_of(p) == form_of(&peaks));
        if same_form {
            let mut s = Spans::new();
            for (a, b) in chunk_runs(target, frames) {
                s.push(a, b);
            }
            self.changed_since_reported.add(&Dirty::Spans(s));
        } else {
            self.changed_since_reported = Dirty::All;
        }
        self.done = Some(peaks.clone());
        self.dirty = Dirty::none();
        self.run = None;
        self.publish(peaks)
    }
}

/// 窓を、見えている区間に重なるものから順に並べる（重ならないものは頭から）。
fn order_windows(windows: Vec<(u64, u64)>, visible: Option<(u64, u64)>) -> Vec<(u64, u64)> {
    let Some((va, vb)) = visible else {
        return windows;
    };
    let (mut near, mut far): (Vec<_>, Vec<_>) =
        windows.into_iter().partition(|(a, b)| *a < vb && va < *b);
    near.append(&mut far);
    near
}

impl Editor {
    /// 波形の段を直す（モジュールの説明）。`value` は公開した今の表示の値、`drawn` はその表示で描いた範囲（ここに掛かる有効な
    /// チャンクだけを値から渡す。遅延の入力のチャンクを読み回さないため）。`deadline` は裏の仕事の区切りの終わり（`work` の
    /// 要求だけ。呼び手が決め、残った時間で長い音声の計りを進める）。中止（新しい要求）は途中で止め、組んだ分は残す。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn advance_peaks(
        &self,
        st: &mut State,
        rev: u64,
        value: &PartialValue,
        drawn: &Area,
        vp: &super::AudioViewport,
        deadline: Option<Instant>,
        ticket: &Ticket,
    ) {
        let Some(shape) = st.audio.as_ref().and_then(|a| a.peaks.shape) else {
            return;
        };
        if vp.interacting {
            return;
        }
        let Some(audio) = st.audio.as_mut() else {
            return;
        };
        let job = &mut audio.peaks;
        if job.run.is_none() {
            if job.dirty.is_empty() {
                return;
            }
            let (rate, channels, frames) = shape;
            let patch = match (&job.done, &job.dirty) {
                (Some(old), Dirty::Spans(spans))
                    if old.rate == rate && old.channels == channels =>
                {
                    PeaksPatch::new(old.clone(), job.params, frames, spans)
                }
                _ => PeaksPatch::fresh(job.params, rate, channels, frames),
            };
            job.run = Some(Run {
                target_rev: rev,
                patch,
            });
        }
        let Some(run) = job.run.as_mut() else {
            return;
        };
        // 表示で描いた範囲の有効なチャンクは写さずに渡す
        if let Some(valid) = value.audio_valid() {
            let channels = usize::from(shape.1);
            let frames = usize::try_from(shape.2).unwrap_or(usize::MAX);
            let drawn = kura_engine::render::audio::chunks_covering(drawn, frames);
            for k in run
                .patch
                .missing()
                .iter()
                .filter(|k| valid.contains(*k) && drawn.contains(*k))
            {
                let data: Result<Vec<_>, EngineError> =
                    (0..channels).map(|ch| value.audio_chunk(ch, k)).collect();
                let fed = data.and_then(|d| {
                    let views: Vec<&[f32]> = d.iter().map(|c| &**c).collect();
                    run.patch.feed_chunk(k, &views)
                });
                if let Err(err) = fed {
                    tracing::warn!(%err, "波形の段に表示の値を渡せません");
                    job.run = None;
                    return;
                }
            }
        }
        let remaining = run.patch.missing().count() as u64 * AUDIO_CHUNK as u64;
        if remaining > PEAKS_SYNC_FRAMES && deadline.is_none() {
            return;
        }
        let windows = order_windows(
            stream_windows(run.patch.frames(), Some(&run.patch.missing_spans())),
            vp.visible,
        );
        let plan = st.plan.clone();
        let output = st.output;
        for (a, b) in windows {
            if remaining > PEAKS_SYNC_FRAMES && deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            let spans = Spans::one(a, b);
            let spec = EstimateSpec::new(0)
                .all_inputs(&plan, Source::Held)
                .area(output, Area::samples(spans.clone()));
            if let Err(err) =
                self.runtime
                    .admit_preview(&plan, spec, &st.inputs, &mut st.cache, &mut st.lease)
            {
                tracing::debug!(%err, "波形の段を直す予算が空いていないので、あとにします");
                settle(st);
                return;
            }
            let Some(run) = st.audio.as_mut().and_then(|a| a.peaks.run.as_mut()) else {
                settle(st);
                return;
            };
            let (inputs, cache) = (&st.inputs, &mut st.cache);
            // 流して読む読みは源の LRU に入れない（見えている区間のチャンクを追い出さない。段 7）
            let r = self.runtime.preview(|e| {
                read_through(|| {
                    e.stream_audio(
                        &plan,
                        inputs,
                        cache,
                        output,
                        Some(&spans),
                        &mut run.patch.partial(),
                        ticket,
                    )
                })
            });
            settle(st);
            if let Err(err) = r {
                if !matches!(err.root(), EngineError::Canceled) {
                    tracing::warn!(%err, "波形の段を直せません（あとでやり直します）");
                    if let Some(a) = st.audio.as_mut() {
                        a.peaks.run = None;
                    }
                }
                return;
            }
            if ticket.canceled() {
                return;
            }
        }
        let Some(audio) = st.audio.as_mut() else {
            return;
        };
        if let Some((generation, peaks)) = audio.peaks.complete(ticket) {
            self.publish_peaks(generation, peaks);
        }
    }
}

/// ライブラリの解析のジョブが長さ `frames` の音声に書く段の引数（`analysis::plan::peaks_params`。約 22 秒（256 × 4,096
/// フレーム）を超えると、細かい段を書かない）。
fn library_params(frames: u64) -> Option<PeaksParams> {
    serde_json::from_value(crate::analysis::plan::peaks_params(frames)).ok()
}

/// ライブラリの解析の段のファイルを土台として読む。段の並びがエディタの段（`PeaksParams::default()`）か、解析のジョブが
/// その長さに書く段（[`library_params`]。既定の段の列の後ろ寄りの一部）で、入力の形と同じときだけ。ほかは None。
pub(crate) fn read_base(
    path: &std::path::Path,
    rate: u32,
    channels: u16,
    frames: u64,
) -> Option<Arc<Peaks>> {
    let bytes = std::fs::read(path).ok()?;
    let p = Peaks::from_canonical(&bytes).ok()?;
    let bins: Vec<u32> = p.levels.iter().map(|l| l.bin).collect();
    let known = bins == PeaksParams::default().bins()
        || library_params(frames).is_some_and(|l| bins == l.bins());
    (known && p.rate == rate && p.channels == channels && p.frames == frames).then(|| Arc::new(p))
}

/// 波形の段のタイルの並び（リトルエンディアン。`crates/kura-core/tests/vectors/edit/peaks_tile.json` が正）：
/// `"kpt1"`・u16 チャンネル数・u16 0・u32 bin・u32 tile・u32 count・u32 0・u64 frames・count × C × [min i16, max i16]。
/// 段 `bin` がない・タイルが範囲の外なら None。
pub fn tile_bytes(p: &Peaks, bin: u32, tile: u32) -> Option<Vec<u8>> {
    let level = p.levels.iter().find(|l| l.bin == bin)?;
    let c = usize::from(p.channels);
    let n = level.data.len() / (2 * c.max(1));
    let first = (tile as usize).checked_mul(TILE_BINS as usize)?;
    if first >= n {
        return None;
    }
    let count = (n - first).min(TILE_BINS as usize);
    let mut out = Vec::with_capacity(32 + count * c * 4);
    out.extend_from_slice(TILE_MAGIC);
    out.extend_from_slice(&p.channels.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&bin.to_le_bytes());
    out.extend_from_slice(&tile.to_le_bytes());
    out.extend_from_slice(&(count as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&p.frames.to_le_bytes());
    for v in level.data.get(first * c * 2..(first + count) * c * 2)? {
        out.extend_from_slice(&v.to_le_bytes());
    }
    Some(out)
}

/// `/edit/{s}/{世代}/peaks/{bin}/{tile}` の中身（モジュールの説明。公開した今か直前の世代の段だけ。錠を待たない）。
pub fn serve_peaks(
    engine: &EngineServices,
    session: &str,
    generation: u64,
    bin: u32,
    tile: u32,
) -> Result<Vec<u8>, TileError> {
    let editor = engine
        .editor_for(session)
        .ok_or_else(|| TileError::NotFound(format!("エディタ {session}")))?;
    let peaks = editor
        .published_peaks(generation)
        .ok_or_else(|| TileError::NotFound(format!("世代 {generation} の波形の段")))?;
    if !PEAK_BINS.contains(&bin) {
        return Err(TileError::NotFound(format!("段 {bin}")));
    }
    tile_bytes(&peaks, bin, tile)
        .ok_or_else(|| TileError::NotFound(format!("段 {bin} のタイル {tile}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::analysis::audio::peaks::PeakLevel;

    fn small() -> Peaks {
        Peaks {
            rate: 8_000,
            channels: 2,
            frames: 600,
            levels: vec![
                PeakLevel {
                    bin: 256,
                    data: vec![-3, 4, -1, 2, -5, 6, 0, 0, -7, 8, -9, 10],
                },
                PeakLevel {
                    bin: 1_024,
                    data: vec![-7, 8, -9, 10],
                },
            ],
        }
    }

    /// タイルの並びは共有のベクタ（`tests/vectors/edit/peaks_tile.json`）と同じ。範囲の外・ない段は None。
    #[test]
    fn tiles_match_the_shared_vector() {
        let raw = include_str!("../../../../tests/vectors/edit/peaks_tile.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let p = &v["peaks"];
        let peaks = Peaks {
            rate: p["rate"].as_u64().unwrap() as u32,
            channels: p["channels"].as_u64().unwrap() as u16,
            frames: p["frames"].as_u64().unwrap(),
            levels: p["levels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| PeakLevel {
                    bin: l["bin"].as_u64().unwrap() as u32,
                    data: l["data"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_i64().unwrap() as i16)
                        .collect(),
                })
                .collect(),
        };
        assert_eq!(peaks, small(), "ベクタの段");
        for t in v["tiles"].as_array().unwrap() {
            let bin = t["bin"].as_u64().unwrap() as u32;
            let tile = t["tile"].as_u64().unwrap() as u32;
            let got = tile_bytes(&peaks, bin, tile).map(|b| crate::library::hex(&b));
            assert_eq!(got.as_deref(), t["hex"].as_str(), "{bin}/{tile}");
        }
    }

    #[test]
    fn tiles_split_the_level_by_1024_bins() {
        let n = 2_500usize;
        let p = Peaks {
            rate: 8_000,
            channels: 1,
            frames: (n * 256) as u64,
            levels: vec![PeakLevel {
                bin: 256,
                data: (0..n)
                    .flat_map(|i| [-(i as i16 % 100), i as i16 % 100])
                    .collect(),
            }],
        };
        let counts: Vec<u32> = (0..3)
            .map(|t| {
                let b = tile_bytes(&p, 256, t).unwrap();
                u32::from_le_bytes(b[16..20].try_into().unwrap())
            })
            .collect();
        assert_eq!(counts, vec![1_024, 1_024, 452]);
        assert!(tile_bytes(&p, 256, 3).is_none());
        assert!(tile_bytes(&p, 1_024, 0).is_none());
        let b = tile_bytes(&p, 256, 2).unwrap();
        assert_eq!(b.len(), 32 + 452 * 4);
        assert_eq!(
            &b[32..36],
            &[(-48i16).to_le_bytes(), 48i16.to_le_bytes()].concat()[..]
        );
    }

    #[test]
    fn windows_near_the_visible_span_come_first() {
        let w = vec![(0, 10), (10, 20), (20, 30)];
        assert_eq!(order_windows(w.clone(), None), w);
        assert_eq!(
            order_windows(w, Some((15, 25))),
            vec![(10, 20), (20, 30), (0, 10)]
        );
    }

    /// 組み立て（assemble）が中止（新しい要求に取って代わられた）で止まっても、受けたチャンク（組みかけ）は残し、次の
    /// 呼び出しで組み直して新しい世代で出す。
    #[test]
    fn canceled_assembly_keeps_the_run() {
        let c = AUDIO_CHUNK as u64;
        let frames = 2 * c + 5;
        let data: Vec<f32> = (0..frames)
            .map(|i| (i % 200) as f32 / 100.0 - 1.0)
            .collect();
        let mut job = PeaksJob::new(None);
        let mut patch = PeaksPatch::fresh(PeaksParams::default(), 8_000, 1, frames);
        for k in patch.missing().iter() {
            let a = k * AUDIO_CHUNK;
            let b = (a + AUDIO_CHUNK).min(data.len());
            patch.feed_chunk(k, &[&data[a..b]]).unwrap();
        }
        job.run = Some(Run {
            target_rev: 0,
            patch,
        });
        let canceled = kura_engine::monitor::CancelFlag::default();
        canceled.cancel();
        assert!(job.complete(&canceled).is_none());
        assert!(job.run.is_some(), "中止では組みかけを残す");
        assert!(job.pending());
        let (generation, peaks) = job.complete(&kura_engine::NoMonitor).unwrap();
        assert!(job.run.is_none());
        assert!(!job.pending());
        assert_eq!(job.shown().map(|(g, _)| g), Some(generation));
        assert_eq!(peaks.frames, frames);
        assert_eq!(peaks.levels.len(), PEAK_BINS.len());
    }

    #[test]
    fn dirty_accumulates() {
        let mut d = Dirty::none();
        assert!(d.is_empty());
        d.add(&Dirty::of(&ChangedSpans::Spans(vec![(0, 5)])));
        d.add(&Dirty::of(&ChangedSpans::Spans(vec![(3, 9)])));
        assert_eq!(d.report(), ChangedSpans::Spans(vec![(0, 9)]));
        d.add(&Dirty::All);
        assert_eq!(d.report(), ChangedSpans::All);
        d.add(&Dirty::none());
        assert_eq!(d, Dirty::All);
    }
}
