//! 音声のエディタ（M13 段 4。docs/editing.md §12.8・§12.10・§12.12）：開く・表示の範囲・表示の情報・予算。
//!
//! - **開く**（[`source`]）：WAV・AIFF・アンカーは遅延の源で開き、全体を読まない（`CodecSet::open_audio_source`）。ほかの形式は
//!   値が min(64 MiB, 丸ごと読む上限 min(取り置き/2, 512 MiB)) 以下なら丸ごと読み、超えれば作業用のファイル（`WorkPcmStore`）に
//!   流して書いてから遅延の源で開く（段 4B）。開くときに M11 の試聴の置き場を空にする（試聴が持つ作業用のファイルは、同じ元
//!   のものだけ開き終えるまで残してエディタが使い回し、違う元のものはその場で手放す）。
//! - **表示の範囲**（[`view_area`]）：出力の値が [`WHOLE_VIEW_BYTES`] 以下なら全体（中間のノードも `Exact` でキャッシュに
//!   入り、PCM・波形・計りが手の中で済む）。それより長ければ、フロントが頼む区間（`spans`）だけをチャンクに揃えて、合わせて
//!   [`MAX_VIEW_CHUNKS`] まで。`visible` は描く範囲に入れない。
//! - **表示の情報**（[`AudioViewInfo`]）：rate・channels・frames・ループ・マーカー・`samplesId`（量子化を含む出力の指紋）・
//!   変わった区間（出力のフレーム）・公開した表示の値の有効な区間。
//! - **予算**：公開した今と直前の表示の値（`resident_bytes`）を、エディタの確保の `extra` で数える（画像は数えない。
//!   docs/editing.md §12.10）。

pub mod audition;
#[cfg(test)]
mod audition_tests;
pub mod eq_curve;
pub mod handles;
#[cfg(test)]
mod handles_tests;
#[cfg(test)]
mod lend_tests;
#[cfg(test)]
mod long_tests;
pub mod measure;
#[cfg(test)]
mod measure_tests;
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
pub(crate) mod slice;
pub mod snap;
pub(crate) mod solve;
#[cfg(test)]
mod solve_tests;
pub(crate) mod source;
pub mod spec;
pub mod spec_average;
#[cfg(test)]
mod spec_tests;
pub mod timeline;
#[cfg(test)]
mod work_tests;

use std::time::Instant;

use kura_engine::audio::source::SourceStats;
use kura_engine::doc::InputId;
use kura_engine::monitor::Ticket;
use kura_engine::render::area::AreaStats;
use kura_engine::render::delta::OutputChange;
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::region::{Area, PartialValue, Spans};
use kura_engine::render::view::ViewRequest;
use kura_engine::value::{AUDIO_CHUNK, Chunk, ChunkSet};
use kura_engine::{Dim, EngineError, Issue, Monitor, Shape, Value};

use super::view::AudioViewport;
use super::{
    Editor, EditorError, Problem, ProblemCode, State, ViewInfo, ViewOutcome, Viewport, failed,
    settle,
};

/// 出力の値の大きさ（frames × ch × 4）がこれ以下なら、表示の範囲を全体にする（48 kHz ステレオで約 87 秒）。
pub const WHOLE_VIEW_BYTES: u64 = 32 << 20;
/// 区間で頼むときのチャンクの数の上限（ステレオで 24 MiB）。超えたら頼んだ順に切る。源の LRU の主の段に収める分と同じ。
pub const MAX_VIEW_CHUNKS: usize = kura_engine::audio::source::KEPT_VIEW_CHUNKS as usize;
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
    /// この `work: true` の頼みでは、誤り・予算の断りのために裏の仕事が 1 チャンクも進まなかった（中止は含めない。`work` が
    /// 真のときだけ立つ）。フロントは連鎖をやめて待つ。
    pub work_stalled: bool,
    /// 描いたノード・当たったノードの警告。
    pub warnings: Vec<Issue>,
    pub drawn_nodes: u64,
    /// 描くのにかかった時間（ミリ秒）。
    pub ms: f64,
    /// 流し解きの進み（0〜1）。頭の文書の流し解きを裏で進めていて、この表示は最後に公開した rev（`rev`）の計画で描いた
    /// もの（頭より古い）。解き終えると新しい rev の表示になる（M17 段 3。docs/editing.md §18.3）。
    pub solving: Option<f64>,
    /// 頼んだスペクトルのポートの状態（M17 段 4。`/edit/{s}/spec/{key}/{ch}/{chunk}` で取る）。
    pub spec: Vec<spec::SpecPortInfo>,
    /// 最後に頼んだスペクトルの区間の平均の状態（`edit_spec_average`。値は載せない）。
    pub spec_average: Vec<spec_average::SpecAverageStatus>,
    /// 鳴らす音の指紋（M17 段 6。試聴の重ねがなければ `samplesId`。外して聞いている間は試聴の計画の出力の指紋で、
    /// `/edit/{s}/audition/{auditionId}/pcm/{a}-{b}` で取る。デコーダは `kura-edit/1:{auditionId}`）。
    pub audition_id: String,
    /// 試聴の情報（頼みに試聴があるときだけ）。
    pub audition: Option<audition::AuditionInfo>,
}

/// 開いている音声のエディタの状態（`State::audio`）。
pub(crate) struct AudioState {
    /// 元（文書の入力）の読み方。
    pub(crate) input: source::InputInfo,
    /// 波形の段の仕事。
    pub(crate) peaks: peaks::PeaksJob,
    /// ラウドネスの結果。
    pub(crate) measure: measure::MeasureJob,
    /// 最後の表示の範囲の描画の数（測るための数。[`Editor::audio_counts`]）。
    pub(crate) last_area: AreaStats,
    /// 最後に公開した rev の計画と、その申告した解析の結果（古い計画の道。M17 段 3）。
    pub(crate) held: Option<solve::Held>,
    /// 裏で進める流し解き（M17 段 3）。
    pub(crate) solve: Option<solve::SolveRun>,
    /// 最後の表示は握る計画で描いた（頭より古い）。
    pub(crate) stale: bool,
    /// 誤りが続いて捨てた流し解きの要る鍵の列（その間は錠の中で解く道で描いて誤りを知らせる）。
    pub(crate) solve_failed: Option<Vec<kura_engine::render::cache::Fingerprint>>,
    /// スペクトルのタイルの仕事（M17 段 4）。
    pub(crate) spec: spec::SpecJob,
    /// 試聴の計画（一時的に外して聞く。M17 段 6）。
    pub(crate) audition: audition::AuditionState,
    /// テスト：次の裏の仕事の区切りを、時間ではなくチャンクの数で区切る（`slice::Limit::Chunks`）。
    #[cfg(test)]
    pub(crate) script: Option<u64>,
}

/// 音声のエディタの測るための数（A18・devtools の `mem_smoke` と `bench edit`。振る舞いに効かない）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioCounts {
    /// 最後の表示の範囲の描画の数（描いたチャンク・ノード・全体に落ちた回数など）。
    pub area: AreaStats,
    /// 元（文書の入力）の源の読みの数（遅延の入力だけ。丸ごと読んだ値は None）。
    pub source: Option<SourceStats>,
    /// キャッシュが持つ量（入力の源の LRU の上限を含む）と、その上限。
    pub cache_bytes: u64,
    pub cache_capacity: u64,
    /// 持ち続ける表示の値・波形の段（`CacheLease::extra`）。
    pub extra: u64,
    /// キャッシュの分の確保（描いていない間。上限と今の大きさの大きいほう ＋ extra）。
    pub lease_bytes: u64,
    /// 裏の仕事で 1 チャンクを流す時間の見積もり（波形の段・計り。区切りの中で流す数を決めるもの。段 7）。
    pub peaks_chunk: Option<std::time::Duration>,
    pub measure_chunk: Option<std::time::Duration>,
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
    /// 今の文書の音声の出力を、表示の範囲（[`view_area`]）だけ描いて公開する（`Editor::render` の音声の枝）。試聴の頼みが
    /// あれば、その流し解きを表示より先に進め、表示のあとに試聴の値を描いて添える（M17 段 6。[`audition`]）。
    pub(super) fn render_audio(
        &self,
        st: &mut State,
        vp: &Viewport,
        ticket: &Ticket,
        full: Shape,
        started: Instant,
    ) -> Result<ViewOutcome, EditorError> {
        if audio_shape(&full).is_none() {
            return Ok(ViewOutcome::Failed(Problem {
                code: ProblemCode::Invalid,
                message: "長さの分からない音声は表示できません".into(),
                issues: Vec::new(),
            }));
        }
        let avp = audio_viewport(vp);
        // 裏の仕事の区切り（`work` の頼みだけ）：試聴の流し解き → 表示の流し解き（操作中でも）→ 波形の段 → 長い音声の計り
        // （操作中は進めない）の順に 1 つの締め切りを分け合う
        let mut limit = avp
            .work
            .then(|| slice::Limit::Deadline(Instant::now() + peaks::WORK_SLICE));
        #[cfg(test)]
        if let (Some(l), Some(n)) = (
            limit.as_mut(),
            st.audio.as_mut().and_then(|a| a.script.take()),
        ) {
            *l = slice::Limit::Chunks(n);
        }
        let mut progress = slice::WorkProgress::default();
        // 試聴の計画（聞こえる音を先に替える）
        let mut prep = self.prepare_audition(st, &avp, &full);
        if self
            .advance_audition(st, &mut prep, limit.as_mut(), &mut progress, ticket)
            .is_err()
        {
            return self.interrupted();
        }
        let outcome =
            self.render_display(st, &avp, ticket, &full, started, &mut limit, &mut progress)?;
        let ViewOutcome::Shown(ViewInfo::Audio(mut info)) = outcome else {
            return Ok(outcome);
        };
        if self
            .attach_audition(st, &mut info, prep, &avp, &full, ticket)
            .is_err()
        {
            return self.interrupted();
        }
        Ok(ViewOutcome::Shown(ViewInfo::Audio(info)))
    }

    /// 描いている途中で中止された：閉じたなら Canceled、新しい頼みに取って代わられたなら Superseded。
    fn interrupted(&self) -> Result<ViewOutcome, EditorError> {
        if self.cancel.canceled() {
            Err(EditorError::Canceled)
        } else {
            Ok(ViewOutcome::Superseded)
        }
    }

    /// 表示を描いて公開する（[`Self::render_audio`] の表示の分）。
    #[allow(clippy::too_many_arguments)]
    fn render_display(
        &self,
        st: &mut State,
        avp: &AudioViewport,
        ticket: &Ticket,
        full: &Shape,
        started: Instant,
        limit: &mut Option<slice::Limit>,
        progress: &mut slice::WorkProgress,
    ) -> Result<ViewOutcome, EditorError> {
        let Some((rate, channels, frames)) = audio_shape(full) else {
            return failed(EditorError::Failed("長さの分からない音声です".into()));
        };
        let avp = avp.clone();
        // M17 段 3：流し解きが重い手は裏の仕事にし、解き終えるまで握る計画で描く
        if let solve::Gate::Background = self.solve_gate(st, full) {
            let done = match limit.as_mut() {
                Some(l) => match self.advance_solve(st, l, progress, ticket) {
                    Ok(solve::Advanced::Done) => {
                        // 解いた結果がキャッシュから消えていれば入れ直す（錠の中で解き直さない）
                        self.restore_solved(st);
                        true
                    }
                    Ok(solve::Advanced::Failed) => {
                        self.fail_solve(st);
                        true
                    }
                    Ok(solve::Advanced::Pending) => false,
                    Err(_) => {
                        return if self.cancel.canceled() {
                            Err(EditorError::Canceled)
                        } else {
                            Ok(ViewOutcome::Superseded)
                        };
                    }
                },
                None => false,
            };
            if !done {
                return self.render_held(st, &avp, ticket, started, limit.is_some(), progress);
            }
        }
        let area = view_area(full, &avp);
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
        if let Some(a) = st.audio.as_mut() {
            a.last_area = vr.area;
        }
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
                    .map(|(_, fp)| (samples_id(fp), *fp))
                    .ok_or_else(|| EditorError::Failed("出力の指紋がありません".into()))
            }) {
            Ok(id) => id,
            Err(e) => return failed(e),
        };
        let (id, out_fp) = id;
        let rev = st.session.rev().0;
        let value = vr.view.value().clone();
        let meta = value.audio_meta().cloned().unwrap_or_default();
        let changed = ChangedSpans::of(&vr.changed);
        self.publish_audio(rev, vr.view.clone(), id.clone());
        st.view = Some(vr.view.clone());
        // 公開した計画の申告した解析の結果を握る（次に重い手が来たら、この計画で描き続ける。M17 段 3）
        self.drop_solve(st);
        self.hold_published(st, rev, &id);
        // スペクトル（M17 段 4）：出力の鍵の移りを覚え、頼まれたタイルを作る（流し解きのあと、波形の段の前）
        if let Some(a) = st.audio.as_mut() {
            let mut key = [0u8; 16];
            key.copy_from_slice(&out_fp.0.0[..16]);
            a.spec.note_output(key, (rate, channels, frames), &changed);
        }
        let spec = self.advance_spec(st, &plan, &value, &avp, limit.as_mut(), progress, ticket);
        // 区間の平均の予約（タイルのあと。操作中は進めない）
        if let Some(l) = limit.as_mut().filter(|_| !avp.interacting) {
            self.advance_spec_average(st, &plan, l, progress, ticket);
        }
        let settled = !avp.interacting && !st.session.is_provisional();
        if let Some(a) = st.audio.as_mut() {
            a.peaks.note(rev, (rate, channels, frames), &changed);
            // 操作中でない確定の表示で出力が変わったときだけ、前の出力の計りかけを捨てる（ドラッグ中・仮の値の表示では残す。
            // 取り消して同じ出力に戻れば続きから計る。段 4B の検証の直し）
            if settled {
                a.measure.retarget(&id);
            }
        }
        // 裏の仕事の区切りの残り（操作中は波形の段・計りを進めない）：波形の段を先に、残った時間で長い音声の計りを進める。
        // 窓の途中でもチャンクごとに締め切りを見る（`slice`）
        if avp.interacting {
            *limit = None;
        }
        self.advance_peaks(
            st,
            rev,
            &value,
            &area,
            &avp,
            limit.as_mut(),
            progress,
            ticket,
        );
        if let Some(limit) = limit.as_mut() {
            self.advance_measure(st, &id, limit, progress, ticket);
        }
        self.count_resident(st);
        let (peaks, work, loudness) = match st.audio.as_mut() {
            Some(a) => {
                let info = a.peaks.report();
                let work = info.pending
                    || a.measure.running(&id)
                    || spec.iter().any(|s| s.pending)
                    || a.spec.average.running();
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
            work_stalled: work && limit.is_some() && progress.stalled(),
            warnings: vr.warnings.clone(),
            drawn_nodes: vr.drawn_nodes(),
            ms: started.elapsed().as_secs_f64() * 1000.0,
            solving: None,
            spec,
            spec_average: st
                .audio
                .as_ref()
                .map(|a| a.spec.average.statuses())
                .unwrap_or_default(),
            audition_id: String::new(),
            audition: None,
        };
        Ok(ViewOutcome::Shown(ViewInfo::Audio(Box::new(info))))
    }

    /// 流し解きの間の表示（モジュール `solve` の説明）：握る計画（最後に公開した rev）で頼まれた区間を描き、同じ rev で公開
    /// する。波形の段・計りは進めない（流し解きが先）。`work` はこの頼みが裏の仕事の頼みか（`work_stalled` を立てるため）。
    fn render_held(
        &self,
        st: &mut State,
        avp: &AudioViewport,
        ticket: &Ticket,
        started: Instant,
        work: bool,
        progress: &slice::WorkProgress,
    ) -> Result<ViewOutcome, EditorError> {
        let Some(held) = st.audio.as_ref().and_then(|a| a.held.as_ref()) else {
            return failed(EditorError::Failed("握る計画がありません".into()));
        };
        let (rev, plan, doc, id) = (
            held.rev,
            held.plan.clone(),
            held.doc.clone(),
            held.samples_id.clone(),
        );
        let Some(full) = plan
            .output_shapes()
            .into_iter()
            .find(|(o, _)| *o == st.output)
            .map(|(_, s)| s)
        else {
            return failed(EditorError::Failed("握る計画に出力がありません".into()));
        };
        let Some((rate, channels, frames)) = audio_shape(&full) else {
            return failed(EditorError::Failed(
                "握る計画の出力が音声ではありません".into(),
            ));
        };
        let area = view_area(&full, avp);
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
        let value = vr.view.value().clone();
        let meta = value.audio_meta().cloned().unwrap_or_default();
        let changed = ChangedSpans::of(&vr.changed);
        self.publish_audio(rev, vr.view.clone(), id.clone());
        st.view = Some(vr.view.clone());
        // スペクトル（握る計画で。手・表示の頼みの分だけ）
        let spec = self.advance_spec(
            st,
            &plan,
            &value,
            avp,
            None,
            &mut slice::WorkProgress::default(),
            ticket,
        );
        let solving = st
            .audio
            .as_ref()
            .and_then(|a| a.solve.as_ref())
            .map_or(0.0, solve::SolveRun::progress);
        let (peaks, loudness) = match st.audio.as_mut() {
            Some(a) => {
                a.last_area = vr.area;
                a.stale = true;
                (a.peaks.report(), a.measure.state(&id))
            }
            None => return failed(EditorError::Failed("音声の状態がありません".into())),
        };
        self.count_resident(st);
        let chain = plan.time_chain(st.output);
        let input_shape = st.shapes.get(&kura_engine::doc::InputId(0)).copied();
        let source_map = chain
            .as_ref()
            .zip(input_shape)
            .and_then(|(c, s)| timeline::source_map(c, &s));
        let nodes = chain
            .as_ref()
            .map(|c| handles::node_views(c, &doc))
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
            samples_id: id,
            changed,
            meta_changed: vr.meta_changed,
            valid: valid_runs(&value, frames),
            peaks,
            source_map,
            nodes,
            loudness,
            work: true,
            work_stalled: work && progress.stalled(),
            warnings: vr.warnings.clone(),
            drawn_nodes: vr.drawn_nodes(),
            ms: started.elapsed().as_secs_f64() * 1000.0,
            solving: Some(solving),
            spec,
            spec_average: st
                .audio
                .as_ref()
                .map(|a| a.spec.average.statuses())
                .unwrap_or_default(),
            audition_id: String::new(),
            audition: None,
        };
        Ok(ViewOutcome::Shown(ViewInfo::Audio(Box::new(info))))
    }

    /// 測るための数（[`AudioCounts`]。描いている途中なら描き終えるまで待つ）。音声のエディタでなければ・閉じていれば None。
    pub fn audio_counts(&self) -> Option<AudioCounts> {
        let guard = self.lock_state();
        let st = guard.as_ref()?;
        let audio = st.audio.as_ref()?;
        let source = match st.inputs.get(&InputId(0)).map(|i| &i.value) {
            Some(Value::Audio(a)) if a.frames() > 0 => match a.chunk(0, 0) {
                Ok(Chunk::Source(r)) => Some(r.src.stats()),
                _ => None,
            },
            _ => None,
        };
        Some(AudioCounts {
            area: audio.last_area,
            source,
            cache_bytes: st.cache.bytes(),
            cache_capacity: st.cache.capacity(),
            extra: st.lease.extra(),
            lease_bytes: st.lease.bytes(),
            peaks_chunk: audio.peaks.pace.per_chunk(),
            measure_chunk: audio.measure.pace.per_chunk(),
        })
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
            // 試聴の値（M17 段 6。id が同じ今と直前は同じ値を分け合うことがあるが、別に数える）
            let audition = [p.audition_now.as_ref(), p.audition_before.as_ref()]
                .into_iter()
                .flatten()
                .map(|(_, v)| v.value().resident_bytes())
                .fold(0u64, u64::saturating_add);
            // スペクトルのタイルの置き場（M17 段 4）
            views
                .saturating_add(peaks)
                .saturating_add(p.spec.bytes())
                .saturating_add(audition)
        };
        // 波形の段の作りかけと、スペクトルのチャンク・まとまりの要約（M17 段 4）
        let run = st.audio.as_ref().map_or(0, |a| {
            a.peaks
                .run_bytes()
                .saturating_add(a.spec.summaries.bytes())
                .saturating_add(a.spec.groups.0.bytes())
                .saturating_add(a.spec.params_bytes())
        });
        // 流し解きの作りかけ（進めている仕事の結果の見積もり）と、解けた結果のうちキャッシュにないもの（仕事だけが持つ。
        // キャッシュにあるものはキャッシュが数える）
        // （表示と試聴の流し解き。M17 段 6）
        let run_bytes = |s: &solve::SolveRun| {
            let finished = s.job.finished();
            let solved = finished.iter().fold(0u64, |a, x| a.saturating_add(x.bytes));
            let uncached = finished
                .iter()
                .filter(|x| !st.cache.contains_analysis(&x.key))
                .fold(0u64, |a, x| a.saturating_add(x.bytes));
            self.runtime
                .preview(|e| s.job.held_bytes(e))
                .saturating_sub(solved)
                .saturating_add(uncached)
        };
        let solving = st.audio.as_ref().map_or(0, |a| {
            [a.solve.as_ref(), a.audition.solve.as_ref()]
                .into_iter()
                .flatten()
                .map(run_bytes)
                .fold(0u64, u64::saturating_add)
        });
        st.lease
            .set_extra(bytes.saturating_add(run).saturating_add(solving));
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
