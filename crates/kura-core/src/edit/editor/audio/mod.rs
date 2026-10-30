//! 音声のエディタ（M13 段 4。docs/editing.md §12.8・§12.10・§12.12）：開く・表示の範囲・表示の情報・予算。
//!
//! - **開く**（[`source`]）：WAV・AIFF・アンカーは遅延の源で開き、全体を読まない（`CodecSet::open_audio_source`）。ほかの形式は
//!   段 2 が入るまで丸ごと読む（min(取り置き/2, 512 MiB) まで）。開くときに M11 の試聴の置き場を空にする。
//! - **表示の範囲**（[`view_area`]）：出力の値が [`WHOLE_VIEW_BYTES`] 以下なら全体（中間のノードも `Exact` でキャッシュに
//!   入り、PCM・波形・計りが手の中で済む）。それより長ければ、フロントが頼む区間（`spans`）だけをチャンクに揃えて、合わせて
//!   [`MAX_VIEW_CHUNKS`] まで。`visible` は描く範囲に入れない。
//! - **表示の情報**（[`AudioViewInfo`]）：rate・channels・frames・ループ・マーカー・`samplesId`（量子化を含む出力の指紋）・
//!   変わった区間（出力のフレーム）・公開した表示の値の有効な区間。
//! - **予算**：公開した今と直前の表示の値（`resident_bytes`）を、エディタの確保の `extra` で数える（画像は数えない。
//!   docs/editing.md §12.10）。

pub mod handles;
#[cfg(test)]
mod handles_tests;
pub mod measure;
pub mod peaks;
#[cfg(test)]
mod peaks_tests;
#[cfg(test)]
mod query_tests;
pub mod serve;
#[cfg(test)]
mod serve_tests;
#[cfg(test)]
mod session_tests;
pub mod snap;
pub(super) mod source;
pub mod timeline;

use std::time::Instant;

use kura_engine::monitor::Ticket;
use kura_engine::render::delta::OutputChange;
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::region::{Area, PartialValue, Spans};
use kura_engine::render::view::ViewRequest;
use kura_engine::value::{AUDIO_CHUNK, ChunkSet};
use kura_engine::{Dim, EngineError, Issue, Monitor, Shape};

use super::view::AudioViewport;
use super::{
    Editor, EditorError, Problem, ProblemCode, State, ViewInfo, ViewOutcome, Viewport, failed,
    settle,
};

/// 出力の値の大きさ（frames × ch × 4）がこれ以下なら、表示の範囲を全体にする（48 kHz ステレオで約 87 秒）。
pub const WHOLE_VIEW_BYTES: u64 = 32 << 20;
/// 区間で頼むときのチャンクの数の上限（ステレオで 24 MiB）。超えたら頼んだ順に切る。
pub const MAX_VIEW_CHUNKS: usize = 48;
/// 頼める区間の数の上限。
pub const MAX_VIEW_SPANS: usize = 4;

/// 出力のフレームの区間の変わり。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangedSpans {
    /// すべて（最初の表示・形が変わった）。
    All,
    /// この区間だけ（空なら変わっていない）。
    Spans(Vec<(u64, u64)>),
}

impl ChangedSpans {
    /// エンジンの変わりの報告から（画像のタイルは念のため全体）。
    pub fn of(c: &OutputChange) -> Self {
        match c {
            OutputChange::Samples(s) => Self::Spans(s.spans().to_vec()),
            OutputChange::All | OutputChange::Tiles(_) => Self::All,
        }
    }
}

/// 音声の表示した結果（kura:// の `/edit/{session}/{rev}/pcm/{a}-{b}` で、`valid` の区間の PCM を取る）。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioViewInfo {
    pub rev: u64,
    /// 今の出力のレート・チャンネル数・長さ（フレーム）。
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// 今の出力のループ点（終わりは含まない）とマーカー。
    pub loop_range: Option<(u64, u64)>,
    pub markers: Vec<(u64, String)>,
    /// 出力のポートの指紋（量子化を含む）の先頭 8 バイトの 16 進。同じ標本なら同じ（取り消しで戻る）。
    pub samples_id: String,
    /// 前回の表示から変わった区間（出力のフレーム）。フロントはこの区間の PCM・波形を捨てる。
    pub changed: ChangedSpans,
    /// ループ点・マーカーなどのメタデータが変わった。
    pub meta_changed: bool,
    /// 公開した表示の値の有効な区間（`/edit/{s}/{rev}/pcm` で取れる所。チャンクに揃う）。
    pub valid: Vec<(u64, u64)>,
    /// 波形の段の状態（`/edit/{s}/{peaks.rev}/peaks/{bin}/{tile}` で取る）。
    pub peaks: peaks::PeaksInfo,
    /// 出力のフレームを元（文書の入力）のフレームへ写す区分（写せなければ None：聞き比べを出さない）。
    pub source_map: Option<Vec<timeline::SourcePiece>>,
    /// 出力につながるノード（入力 → 出力の順）の入力のレート・長さとハンドルの位置。
    pub nodes: Vec<handles::AudioNodeView>,
    /// ラウドネスの状態（今の `samplesId` で覚えているもの。なければ `Idle`）。
    pub loudness: measure::LoudnessState,
    /// 裏の仕事が残っている（フロントは手が止まったら `work: true` で頼み直す）。
    pub work: bool,
    /// 描いたノード・当たったノードの警告。
    pub warnings: Vec<Issue>,
    pub drawn_nodes: u64,
    /// 描くのにかかった時間（ミリ秒）。
    pub ms: f64,
}

/// 開いている音声のエディタの状態（`State::audio`）。
pub(crate) struct AudioState {
    /// 元（文書の入力）の読み方。
    pub(crate) input: source::InputInfo,
    /// 波形の段の仕事。
    pub(crate) peaks: peaks::PeaksJob,
    /// ラウドネスの結果。
    pub(crate) measure: measure::MeasureJob,
}

/// 音声の出力の形の (rate, channels, frames)。長さの分からないものは None。
pub(crate) fn audio_shape(s: &Shape) -> Option<(u32, u16, u64)> {
    match *s {
        Shape::Audio {
            rate,
            channels,
            frames: Dim::Exact(frames),
        } => Some((rate, channels, frames)),
        _ => None,
    }
}

/// 出力の値の大きさ（frames × ch × 4）が [`WHOLE_VIEW_BYTES`] 以下か。
pub fn is_short(full: &Shape) -> bool {
    audio_shape(full).is_some_and(|(_, ch, frames)| {
        frames.saturating_mul(u64::from(ch)).saturating_mul(4) <= WHOLE_VIEW_BYTES
    })
}

/// 表示する範囲（モジュールの説明）：短い音声は全体、長い音声は頼んだ区間をチャンクに揃えて [`MAX_VIEW_CHUNKS`] まで。
pub fn view_area(full: &Shape, vp: &AudioViewport) -> Area {
    if is_short(full) {
        return Area::All;
    }
    let frames = audio_shape(full).map_or(0, |(_, _, n)| n);
    let c = AUDIO_CHUNK as u64;
    let mut spans = Spans::new();
    let mut chunks = 0usize;
    'outer: for (i, &(a, b)) in vp.spans.iter().enumerate() {
        if i >= MAX_VIEW_SPANS {
            tracing::debug!(
                asked = vp.spans.len(),
                "音声の表示の区間を {MAX_VIEW_SPANS} つで切りました"
            );
            break;
        }
        let (a, b) = (a.min(frames), b.min(frames));
        if a >= b {
            continue;
        }
        let first = a / c;
        let last = b.div_ceil(c);
        for k in first..last {
            if spans
                .spans()
                .iter()
                .any(|(x, y)| *x <= k * c && (k * c).min(frames) < *y)
            {
                continue;
            }
            if chunks >= MAX_VIEW_CHUNKS {
                tracing::debug!("音声の表示の区間を {MAX_VIEW_CHUNKS} チャンクで切りました");
                break 'outer;
            }
            spans.push(k * c, ((k + 1) * c).min(frames));
            chunks += 1;
        }
    }
    if spans.is_empty() {
        Area::Empty
    } else {
        Area::samples(spans)
    }
}

/// チャンクの集合を、続いたチャンクの区間の列にする（畳まない。長さで切る）。
pub fn chunk_runs(set: &ChunkSet, frames: u64) -> Vec<(u64, u64)> {
    let c = AUDIO_CHUNK as u64;
    let mut out: Vec<(u64, u64)> = Vec::new();
    for k in set.iter() {
        let a = (k as u64).saturating_mul(c).min(frames);
        let b = a.saturating_add(c).min(frames);
        if a >= b {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.1 == a => last.1 = b,
            _ => out.push((a, b)),
        }
    }
    out
}

/// 出力の指紋の先頭 8 バイトの 16 進（`samplesId`）。
pub(crate) fn samples_id(fp: &kura_engine::render::cache::Fingerprint) -> String {
    crate::library::hex(&fp.0.0[..8])
}

/// 公開した表示の値の有効な区間。
fn valid_runs(value: &PartialValue, frames: u64) -> Vec<(u64, u64)> {
    value
        .audio_valid()
        .map(|set| chunk_runs(&set, frames))
        .unwrap_or_default()
}

/// 音声のビューポート（画像の要求なら、同じ操作中・編集中のノードで既定の要求）。
pub(crate) fn audio_viewport(vp: &Viewport) -> AudioViewport {
    match vp {
        Viewport::Audio(a) => a.clone(),
        Viewport::Image(i) => AudioViewport {
            interacting: i.interacting,
            focus: i.focus,
            ..AudioViewport::idle()
        },
    }
}

impl Editor {
    /// 今の文書の音声の出力を、表示の範囲（[`view_area`]）だけ描いて公開する（`Editor::render` の音声の枝）。
    pub(super) fn render_audio(
        &self,
        st: &mut State,
        vp: &Viewport,
        ticket: &Ticket,
        full: Shape,
        started: Instant,
    ) -> Result<ViewOutcome, EditorError> {
        let Some((rate, channels, frames)) = audio_shape(&full) else {
            return Ok(ViewOutcome::Failed(Problem {
                code: ProblemCode::Invalid,
                message: "長さの分からない音声は表示できません".into(),
                issues: Vec::new(),
            }));
        };
        let avp = audio_viewport(vp);
        let area = view_area(&full, &avp);
        let plan = st.plan.clone();
        let spec = EstimateSpec::new(0)
            .all_inputs(&plan, Source::Held)
            .area(st.output, area.clone());
        if let Err(e) = self.runtime.admit_preview(
            &plan,
            spec.clone(),
            &st.inputs,
            &mut st.cache,
            &mut st.lease,
        ) {
            let problem = self.admit_problem(&plan, spec, st, e);
            settle(st);
            return Ok(ViewOutcome::Failed(problem));
        }
        let req = ViewRequest {
            output: st.output,
            area: area.clone(),
            focus: avp.focus,
        };
        let (inputs, cache, prev) = (&st.inputs, &mut st.cache, st.view.as_ref());
        let r = self
            .runtime
            .preview(|e| e.render_view(&plan, inputs, cache, &req, prev, ticket));
        settle(st);
        let vr = match r {
            Ok(vr) => vr,
            Err(e) if matches!(e.root(), EngineError::Canceled) => {
                return if self.cancel.canceled() {
                    Err(EditorError::Canceled)
                } else {
                    Ok(ViewOutcome::Superseded)
                };
            }
            Err(e) => return failed(EditorError::from_engine("表示を描けません", e)),
        };
        if vr.area.fallbacks > 0 {
            tracing::warn!(
                fallbacks = vr.area.fallbacks,
                "音声の範囲の描画が全体に落ちました（申告の誤り）"
            );
        }
        let id = match self
            .runtime
            .preview(|e| e.fingerprints(&plan, &st.inputs))
            .map_err(|e| EditorError::from_engine("出力の指紋を求められません", e))
            .and_then(|fps| {
                fps.outputs
                    .iter()
                    .find(|(o, _)| *o == st.output)
                    .map(|(_, fp)| samples_id(fp))
                    .ok_or_else(|| EditorError::Failed("出力の指紋がありません".into()))
            }) {
            Ok(id) => id,
            Err(e) => return failed(e),
        };
        let rev = st.session.rev().0;
        let value = vr.view.value().clone();
        let meta = value.audio_meta().cloned().unwrap_or_default();
        let changed = ChangedSpans::of(&vr.changed);
        self.publish_audio(rev, vr.view.clone(), id.clone());
        st.view = Some(vr.view.clone());
        if let Some(a) = st.audio.as_mut() {
            a.peaks.note(rev, (rate, channels, frames), &changed);
        }
        self.advance_peaks(st, rev, &value, &area, &avp, ticket);
        self.count_resident(st);
        let (peaks, work, loudness) = match st.audio.as_mut() {
            Some(a) => {
                let info = a.peaks.report();
                let work = info.pending;
                (info, work, a.measure.state(&id))
            }
            None => return failed(EditorError::Failed("音声の状態がありません".into())),
        };
        let chain = plan.time_chain(st.output);
        let input_shape = st.shapes.get(&kura_engine::doc::InputId(0)).copied();
        let source_map = chain
            .as_ref()
            .zip(input_shape)
            .and_then(|(c, s)| timeline::source_map(c, &s));
        let nodes = chain
            .as_ref()
            .map(|c| handles::node_views(c, st.session.current()))
            .unwrap_or_default();
        let info = AudioViewInfo {
            rev,
            rate,
            channels,
            frames,
            loop_range: meta.loop_range.map(|l| (l.start, l.end)),
            markers: meta
                .markers
                .iter()
                .map(|m| (m.pos, m.name.clone()))
                .collect(),
            samples_id: id.clone(),
            changed,
            meta_changed: vr.meta_changed,
            valid: valid_runs(&value, frames),
            peaks,
            source_map,
            nodes,
            loudness,
            work,
            warnings: vr.warnings.clone(),
            drawn_nodes: vr.drawn_nodes(),
            ms: started.elapsed().as_secs_f64() * 1000.0,
        };
        Ok(ViewOutcome::Shown(ViewInfo::Audio(Box::new(info))))
    }

    /// 持ち続ける表示の値（公開した今と直前）・波形の段（公開した 2 つと組んでいる途中のもの）を、エディタの確保の `extra`
    /// に入れる（音声だけ）。
    pub(super) fn count_resident(&self, st: &mut State) {
        let peak_bytes = |p: &kura_engine::analysis::audio::peaks::Peaks| {
            p.levels
                .iter()
                .map(|l| (l.data.len() as u64).saturating_mul(2))
                .fold(0u64, u64::saturating_add)
        };
        let bytes = {
            let p = self.lock_published();
            let views = [p.now.as_ref(), p.before.as_ref()]
                .into_iter()
                .flatten()
                .map(|s| s.view.value().resident_bytes())
                .fold(0u64, u64::saturating_add);
            let peaks = [p.peaks_now.as_ref(), p.peaks_before.as_ref()]
                .into_iter()
                .flatten()
                .map(|(_, p)| peak_bytes(p))
                .fold(0u64, u64::saturating_add);
            views.saturating_add(peaks)
        };
        let run = st.audio.as_ref().map_or(0, |a| a.peaks.run_bytes());
        st.lease.set_extra(bytes.saturating_add(run));
        settle(st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(rate: u32, channels: u16, frames: u64) -> Shape {
        Shape::Audio {
            rate,
            channels,
            frames: Dim::Exact(frames),
        }
    }

    fn vp(spans: &[(u64, u64)]) -> AudioViewport {
        AudioViewport {
            spans: spans.to_vec(),
            ..AudioViewport::idle()
        }
    }

    /// 短い音声は全体、長い音声は頼んだ区間だけをチャンクに揃えて 48 チャンクまで。空なら Empty。
    #[test]
    fn view_area_is_whole_for_short_and_spans_for_long_audio() {
        let c = AUDIO_CHUNK as u64;
        let short = audio(48_000, 2, 48_000 * 87);
        assert!(is_short(&short));
        assert_eq!(view_area(&short, &vp(&[(5, 9)])), Area::All);
        let long = audio(48_000, 2, 48_000 * 600);
        assert!(!is_short(&long));
        assert_eq!(view_area(&long, &vp(&[])), Area::Empty);
        assert_eq!(view_area(&long, &vp(&[(5, 5)])), Area::Empty);
        assert_eq!(
            view_area(&long, &vp(&[(c + 3, 2 * c + 1)])),
            Area::samples(Spans::one(c, 3 * c))
        );
        // 48 チャンクで切る（先の区間ほど優先）
        let Area::Samples(s) = view_area(&long, &vp(&[(0, 40 * c), (100 * c, 120 * c)])) else {
            panic!()
        };
        assert_eq!(s.spans(), &[(0, 40 * c), (100 * c, 108 * c)]);
        // 重なる区間は 1 度だけ数える・長さで切る
        let n = 48_000 * 600;
        let Area::Samples(s) = view_area(&long, &vp(&[(0, c), (0, 2 * c), (n - 5, n + 99)])) else {
            panic!()
        };
        assert_eq!(s.spans(), &[(0, 2 * c), ((n - 5) / c * c, n)]);
        // 5 つ目からは見ない
        let many: Vec<(u64, u64)> = (0..6).map(|i| (i * 10 * c, i * 10 * c + 1)).collect();
        let Area::Samples(s) = view_area(&long, &vp(&many)) else {
            panic!()
        };
        assert_eq!(s.spans().len(), MAX_VIEW_SPANS);
    }

    #[test]
    fn chunk_runs_merge_neighbours_without_folding() {
        let c = AUDIO_CHUNK as u64;
        let mut set = ChunkSet::empty(200);
        for k in (0..200).step_by(2) {
            set.insert(k);
        }
        set.insert(1);
        let runs = chunk_runs(&set, 199 * c + 7);
        assert_eq!(runs.len(), 99, "畳まない");
        assert_eq!(runs[0], (0, 3 * c));
        assert_eq!(*runs.last().unwrap(), (198 * c, 199 * c));
        assert!(chunk_runs(&ChunkSet::empty(3), 10).is_empty());
    }
}
