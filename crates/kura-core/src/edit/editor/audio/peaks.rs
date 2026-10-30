//! エディタの波形の段（M13 段 4。docs/editing.md §11.4・§12.8・§12.13）。
//!
//! - **持ち方**：組み終えた段（`done`。rev と `Arc<Peaks>`）と、そこから今の出力までに変わりうる区間（`dirty`。今の出力の座標）。
//!   確定した段だけを rev ごとに公開し（今と直前）、`/edit/{s}/{rev}/peaks/{bin}/{tile}`（immutable）で配る。
//! - **土台**（開くとき）：頭が元（版 0）・入力の記録の値のダイジェストが事実の行と同じ・出力に量子化がない、をすべて満たせば、
//!   ライブラリの解析の段のファイル（`peaks/ab/<hash>.peaks`）をそのまま土台にし、空の文書から今の文書までに変わった区間
//!   （`Engine::delta`）だけを `dirty` にする（手を当てていなければ空で、開いた瞬間に概観が出る）。ほかは全体を作る。
//! - **手のあと**：表示の変わった区間を `dirty` に足す（レート・チャンネルが変われば全体。長さの変化は `PeaksPatch` が尾を足す）。
//! - **直す**（[`Editor::advance_peaks`]。操作中は何もしない）：`PeaksPatch` を作り、R のうち表示の値で有効なチャンクは写さずに
//!   渡し、残りが [`PEAKS_SYNC_FRAMES`] 以下なら流して直す。それを超える分は、`edit_view { work: true }` のたびに
//!   [`WORK_SLICE`] まで、見えている区間に重なる窓から流す（裏の仕事。別のスレッドは使わない）。直し終えたら差し替え、それまでは
//!   前の段を出す（`pending`）。
//! - **予算**：公開した段 2 つと組んでいる段の分を、表示の値と一緒に `CacheLease::extra` で数える。

use std::sync::Arc;
use std::time::{Duration, Instant};

use kura_engine::analysis::audio::peaks::{PEAK_BINS, Peaks, PeaksParams, PeaksPatch};
use kura_engine::analysis::output::AnalysisOutput;
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

/// 波形の段の仕事の状態（`AudioState` が持つ）。
pub(crate) struct PeaksJob {
    params: PeaksParams,
    /// 最後に組み終えた段（公開済み）。
    done: Option<(u64, Arc<Peaks>)>,
    /// `done` から今の出力までに変わりうる区間。
    dirty: Dirty,
    /// 今の出力の (rate, channels, frames)。
    shape: Option<(u32, u16, u64)>,
    run: Option<Run>,
    /// 前に返した段から、公開した段で変わった区間（返したら空にする）。
    changed_since_reported: Dirty,
    /// 前に返した段の形（変わったら全体を取り直させる）。
    reported_shape: Option<(u32, u16, u64)>,
    /// 次の表示の変わりを足さない（開いたときの最初の表示。土台からの差分は開くときに入れた）。
    skip_next: bool,
}

/// 表示の情報に載せる波形の段の状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeaksInfo {
    /// 公開した今の段の rev（なければ None）。今の文書の rev より古いことがある（そのときは `pending`）。
    pub rev: Option<u64>,
    /// その段のレート・チャンネル数・長さ。
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// 段のビンのフレーム数（細かい順）。
    pub bins: Vec<u32>,
    /// タイル 1 枚のビンの数。
    pub tile_bins: u32,
    /// 前に返した段から変わった区間（フレーム。この区間に掛かるタイルを取り直す）。
    pub changed: ChangedSpans,
    /// 直している途中（古い段を出している）。
    pub pending: bool,
}

impl PeaksJob {
    /// 土台（ライブラリの段。変わりうる区間つき）から始める。土台がなければ全体を作る。
    pub(crate) fn new(base: Option<(Arc<Peaks>, Dirty)>, rev: u64) -> Self {
        let params = PeaksParams::default();
        let (done, dirty) = match base {
            Some((peaks, dirty)) => (Some((rev, peaks)), dirty),
            None => (None, Dirty::All),
        };
        Self {
            params,
            done,
            dirty,
            shape: None,
            run: None,
            changed_since_reported: Dirty::All,
            reported_shape: None,
            skip_next: true,
        }
    }

    /// 土台（開いたときに組み終えている段）。
    pub(crate) fn base(&self) -> Option<Arc<Peaks>> {
        self.done.as_ref().map(|(_, p)| p.clone())
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
            .is_none_or(|(_, p)| p.rate == shape.0 && p.channels == shape.1);
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
        let bins = self.params.bins();
        let Some((rev, p)) = &self.done else {
            return PeaksInfo {
                rev: None,
                rate: 0,
                channels: 0,
                frames: 0,
                bins,
                tile_bins: TILE_BINS,
                changed: ChangedSpans::All,
                pending,
            };
        };
        let shape = (p.rate, p.channels, p.frames);
        let changed = if self.reported_shape != Some(shape) {
            ChangedSpans::All
        } else {
            self.changed_since_reported.report()
        };
        self.changed_since_reported = Dirty::none();
        self.reported_shape = Some(shape);
        PeaksInfo {
            rev: Some(*rev),
            rate: p.rate,
            channels: p.channels,
            frames: p.frames,
            bins,
            tile_bins: TILE_BINS,
            changed,
            pending,
        }
    }

    /// 組み終えた段を受ける（公開は呼び手）。
    fn finish(&mut self, rev: u64, peaks: Arc<Peaks>, target: &kura_engine::value::ChunkSet) {
        let frames = peaks.frames;
        let shape_changed = self.done.as_ref().is_none_or(|(_, p)| {
            p.rate != peaks.rate || p.channels != peaks.channels || p.frames != peaks.frames
        });
        if shape_changed {
            self.changed_since_reported = Dirty::All;
        } else {
            let mut s = Spans::new();
            for (a, b) in chunk_runs(target, frames) {
                s.push(a, b);
            }
            self.changed_since_reported.add(&Dirty::Spans(s));
        }
        self.done = Some((rev, peaks));
        self.dirty = Dirty::none();
        self.run = None;
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
    /// チャンクだけを値から渡す。遅延の入力のチャンクを読み回さないため）。中止（新しい要求）は途中で止め、組んだ分は残す。
    pub(super) fn advance_peaks(
        &self,
        st: &mut State,
        rev: u64,
        value: &PartialValue,
        drawn: &Area,
        vp: &super::AudioViewport,
        ticket: &Ticket,
    ) {
        let Some(shape) = st.audio.as_ref().and_then(|a| a.peaks.shape) else {
            return;
        };
        if vp.interacting {
            return;
        }
        let deadline = vp.work.then(|| Instant::now() + WORK_SLICE);
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
                (Some((_, old)), Dirty::Spans(spans))
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
            let r = self.runtime.preview(|e| {
                e.stream_audio(
                    &plan,
                    inputs,
                    cache,
                    output,
                    Some(&spans),
                    &mut run.patch.partial(),
                    ticket,
                )
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
        let job = &mut audio.peaks;
        let Some(run) = job.run.as_ref() else {
            return;
        };
        if !run.patch.missing().is_empty() {
            return;
        }
        match run.patch.assemble(ticket) {
            Ok(peaks) => {
                let target = run.patch.target().clone();
                let peaks = Arc::new(peaks);
                job.finish(rev, peaks.clone(), &target);
                self.publish_peaks(rev, peaks);
            }
            Err(err) => {
                tracing::warn!(%err, "波形の段を組めません");
                job.run = None;
            }
        }
    }
}

/// ライブラリの解析の段のファイルを土台として読む（引数の段・入力の形と合わなければ None）。
pub(crate) fn read_base(
    path: &std::path::Path,
    rate: u32,
    channels: u16,
    frames: u64,
) -> Option<Arc<Peaks>> {
    let bytes = std::fs::read(path).ok()?;
    let p = Peaks::from_canonical(&bytes).ok()?;
    let bins: Vec<u32> = p.levels.iter().map(|l| l.bin).collect();
    (bins == PeaksParams::default().bins()
        && p.rate == rate
        && p.channels == channels
        && p.frames == frames)
        .then(|| Arc::new(p))
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

/// `/edit/{s}/{rev}/peaks/{bin}/{tile}` の中身（モジュールの説明。公開した段だけ。錠を待たない）。
pub fn serve_peaks(
    engine: &EngineServices,
    session: &str,
    rev: u64,
    bin: u32,
    tile: u32,
) -> Result<Vec<u8>, TileError> {
    let editor = engine
        .editor_for(session)
        .ok_or_else(|| TileError::NotFound(format!("エディタ {session}")))?;
    let peaks = editor
        .published_peaks(rev)
        .ok_or_else(|| TileError::NotFound(format!("rev {rev} の波形の段")))?;
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
