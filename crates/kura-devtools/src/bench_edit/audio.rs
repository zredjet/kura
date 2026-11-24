//! 音声のエディタの 1 手（M13 段 4C の F15。design-stage46.md §1.13 の devtools）：`bench edit --audio-minutes <n>`（NFR-11 の
//! 往復の時間）と、`mem_smoke` の `audio-editor-60min`（NFR-17 のプロセスの山）が同じ道を通る。
//!
//! - 入力：n 分・48 kHz ステレオ・16 ビットの WAV（位置から決める雑音。値は丸ごと作らない）を一時の場所に書き、新しい
//!   ライブラリにアプリと同じ取り込みのジョブで入れる（解析のジョブ（波形の段・ラウドネス・事実）が動き終えるまで待つ）。
//!   利用者のライブラリには触れない。
//! - 道：kura-core の `edit::editor`（`open`・`step`・`edit_measure`・`close`）。アプリと同じエンジンの部品（アプリのコーデックの
//!   組・プールの大きさを決めた実行の場）。画面（IPC・WebKit）は含めない。
//! - 表示の要求：長さの 1/6 の位置（60 分なら 10 分）から 48 チャンク（区間で頼める上限 `MAX_VIEW_CHUNKS`。拡大して PCM で描く
//!   見えている区間。持ち続ける表示の値がいちばん大きい形）。短い音声（出力の値が 32 MiB 以下）は全体を描く。
//! - 文書：trim → fade → gain → set_loop（足す手 4 つ）。測る手は 4 種：gain の引数（`SetParams`）・フェードインの終わり・
//!   trim の終わり・ループ点（どれも `Step::Handle` の確定）。往復は「手 → 表示の区間の値」（`editor::step` が表示の値を公開して
//!   返るまで。手の中で直す波形の段を含む）。
//! - 裏の仕事：全体に効く手のあと、`edit_measure` でラウドネスを予約し、`work: true` の表示を続けて頼んで、波形の段と
//!   ラウドネスが揃うまでの時間（計算の時間。フロントは手が止まって 150 ms で最初を頼み、そのあとは応えのたびに次を頼む）を測る。
//! - 末尾にレート変換を足したあとの gain の手（`audio-editor-*-gain-resample`。区間のレート変換を毎回描き直す）も測る。NFR-11 の
//!   対象外で、範囲外の記録（ADR-187）。

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use kura_core::edit::editor::audio::AudioCounts;
use kura_core::edit::editor::audio::handles::{Handle, HandleMode, Marks};
use kura_core::edit::editor::audio::measure::{LoudnessState, measure};
use kura_core::edit::editor::{
    self, AudioViewInfo, AudioViewport, EditorEnv, OpenRequest, Step, ViewInfo, ViewOutcome,
    Viewport,
};
use kura_core::edit::{CodecSet, EngineFactory, EngineServices, RuntimeConfig};
use kura_core::library::{LibrarySession, Services};
use kura_engine::budget::Usage;
use kura_engine::doc::NodeId;
use kura_engine::session::{CommitMode, Edit};
use kura_engine::testing::bench::Stats;
use kura_engine::value::AUDIO_CHUNK;
use serde_json::json;

use super::report::Record;
use super::{Ctx, load1};

/// 入力のレート（60 分の 48 kHz ステレオ。design-stage46.md §1.13 の A18 と同じ形）。
pub(crate) const RATE: u32 = 48_000;
/// 区間で頼むチャンクの数（`MAX_VIEW_CHUNKS`）。
pub(crate) const AREA_CHUNKS: u64 = kura_core::edit::editor::audio::MAX_VIEW_CHUNKS as u64;
const C: u64 = AUDIO_CHUNK as u64;

/// エンジンの部品：プールの大きさと予算（なければ物理メモリから）を決め、コーデックはアプリの組。
pub(crate) struct AudioFactory {
    pub preview: usize,
    pub bulk: usize,
    pub budget: Option<u64>,
}

impl EngineFactory for AudioFactory {
    fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            physical_memory: kura_mac::physical_memory(),
            budget: self.budget,
            preview_threads: Some(self.preview),
            bulk_threads: Some(self.bulk),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, work_dir: &Path) -> CodecSet {
        crate::app_codecs(work_dir)
    }

    /// アプリと同じ耐久（一括加工の 1 件ずつの fsync と区切りの F_FULLFSYNC。M14 段 8 の一括のメモリの煙のテスト）。
    fn durability(&self) -> Arc<dyn kura_core::edit::Durability> {
        Arc::new(kura_mac::MacDurability)
    }
}

/// アプリと同じ渡しもの（ImageIO のメディア・アプリのコーデックの組・耐久）で、プールの大きさと予算を決めたもの。
pub(crate) fn services(preview: usize, bulk: usize, budget: Option<u64>) -> Services {
    Services::new(kura_mac::media_backend()).with_engine(Arc::new(AudioFactory {
        preview,
        bulk,
        budget,
    }))
}

/// 知らせを捨てる口。
pub(crate) fn quiet() -> kura_core::jobs::EventSink {
    Arc::new(|_| {})
}

/// 長さ `frames`・`rate`・ステレオ・16 ビットの WAV を `path` に流して書く（標本は位置から決める xorshift の雑音で、大きさが
/// 40 秒ごとに変わる。ゲートを通るブロックと通らないブロックを混ぜる）。
pub(crate) fn write_wav(path: &Path, rate: u32, frames: u64) -> Result<(), String> {
    use std::io::Write;
    let err = |e: std::io::Error| format!("{} を書けません: {e}", path.display());
    let data = frames
        .checked_mul(4)
        .filter(|d| *d <= u64::from(u32::MAX) - 36)
        .ok_or("WAV に書くには長すぎます")?;
    let mut f =
        std::io::BufWriter::with_capacity(4 << 20, std::fs::File::create(path).map_err(err)?);
    let mut head = Vec::with_capacity(44);
    head.extend_from_slice(b"RIFF");
    head.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    head.extend_from_slice(b"WAVEfmt ");
    head.extend_from_slice(&16u32.to_le_bytes());
    head.extend_from_slice(&1u16.to_le_bytes());
    head.extend_from_slice(&2u16.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&(rate * 4).to_le_bytes());
    head.extend_from_slice(&4u16.to_le_bytes());
    head.extend_from_slice(&16u16.to_le_bytes());
    head.extend_from_slice(b"data");
    head.extend_from_slice(&(data as u32).to_le_bytes());
    f.write_all(&head).map_err(err)?;
    let mut x: u32 = 0x9e37_79b9;
    let mut buf = Vec::with_capacity(1 << 20);
    for i in 0..frames {
        for c in 0..2i32 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let level = ((i / u64::from(rate)) % 40) as i32 * 300 + c * 50;
            let s = ((x >> 16) as i32 - 32_768) * level / 12_000;
            buf.extend_from_slice(&(s.clamp(-32_768, 32_767) as i16).to_le_bytes());
        }
        if buf.len() >= 1 << 20 {
            f.write_all(&buf).map_err(err)?;
            buf.clear();
        }
    }
    f.write_all(&buf).map_err(err)?;
    f.flush().map_err(err)
}

/// 新しいライブラリ `lib` を作り、`src`（WAV 1 つのフォルダ）をアプリと同じ取り込みのジョブで入れて、ジョブ（サムネイル・解析）
/// が動き終えるまで待って閉じる。取り込んだアイテムの ID。
pub(crate) fn prepare_library(lib: &Path, src: &Path) -> Result<i64, String> {
    let session = LibrarySession::create(lib, quiet(), services(1, 3, None))
        .map_err(|e| format!("ライブラリを作れません: {e}"))?;
    session.wait_ready();
    let items = crate::bench_app::import_all(&session, src)?;
    session.close();
    match items.values().copied().collect::<Vec<_>>().as_slice() {
        [id] => Ok(*id),
        other => Err(format!(
            "取り込んだアイテムが 1 つではありません: {other:?}"
        )),
    }
}

/// 表示の要求：長さの 1/6 の位置（チャンクに揃える）から 48 チャンク（長さで切る）。`visible` も同じ区間。
pub(crate) fn viewport(frames: u64, work: bool) -> AudioViewport {
    let at = (frames / 6) / C * C;
    let area = (at, (at + AREA_CHUNKS * C).min(frames));
    AudioViewport {
        visible: Some(area),
        spans: vec![area],
        interacting: false,
        focus: None,
        work,
        spec: Vec::new(),
    }
}

/// 測る手の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Gain,
    Fade,
    TrimEnd,
    Loop,
}

impl Kind {
    pub(crate) const ALL: [Kind; 4] = [Kind::Fade, Kind::TrimEnd, Kind::Loop, Kind::Gain];

    /// ケースの名前の後ろ。
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Gain => "gain",
            Self::Fade => "fade",
            Self::TrimEnd => "trim-end",
            Self::Loop => "loop",
        }
    }

    /// 表の文言。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Gain => "gain の引数（全体に効く。波形の段は裏の仕事）",
            Self::Fade => "フェードインの終わりのハンドル（頭の区間。波形の段は手の中で直す）",
            Self::TrimEnd => "trim の終わりのハンドル（長さが変わる）",
            Self::Loop => "ループ点のハンドル（メタデータだけ）",
        }
    }
}

/// 裏の仕事が揃うまでの数。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Settled {
    /// 波形の段が揃うまで（頼んだ回数・ナノ秒）。
    pub peaks: (u32, u64),
    /// ラウドネスが揃うまで（同じ。予約からの全体）。
    pub loudness: (u32, u64),
    /// 1 回の区切りの最大（ナノ秒）。
    pub slowest: u64,
    /// 計り終えたか（`Ready`）。
    pub ready: bool,
}

/// 測るあいだの数の最大（持ち続ける分・キャッシュ）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Held {
    pub max_extra: u64,
    pub max_cache: u64,
    pub max_lease: u64,
}

impl Held {
    fn note(&mut self, c: &AudioCounts) {
        self.max_extra = self.max_extra.max(c.extra);
        self.max_cache = self.max_cache.max(c.cache_bytes);
        self.max_lease = self.max_lease.max(c.lease_bytes);
    }
}

/// 手が効いたことを確かめる表示の値。
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// 標本が変わる（`samplesId`）。
    NewSamples,
    /// フェードのノードのフェードインの終わりの印。
    FadeIn(NodeId, u64),
    /// 出力の長さ。
    Frames(u64),
    /// ループ点。
    Loop((u64, u64)),
}

/// 開いた音声のエディタ 1 つ。
pub(crate) struct AudioEditor<'s> {
    env: EditorEnv<'s>,
    engine: &'s Arc<EngineServices>,
    sid: String,
    frames: u64,
    /// 入力のレート（手の大きさの「1 秒」）。
    rate: u64,
    vp: AudioViewport,
    /// trim・fade・gain・set_loop のノード（`setup` のあと）。
    nodes: Option<[NodeId; 4]>,
    /// 前の表示の `samplesId`。
    samples: String,
    /// 開く前の予算の使用量。
    pub before: Usage,
    /// 開くのにかかった時間（ナノ秒）と、開いたときの数（入力の源の LRU の上限がキャッシュに入る）。
    pub open_ns: u64,
    pub opened: AudioCounts,
    pub held: Held,
}

fn nanos(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// 表示した音声の結果（表示できなければエラー）。
fn shown(view: ViewOutcome) -> Result<AudioViewInfo, String> {
    match view {
        ViewOutcome::Shown(ViewInfo::Audio(info)) => Ok(*info),
        other => Err(format!("音声を表示できませんでした: {other:?}")),
    }
}

impl<'s> AudioEditor<'s> {
    /// アイテム `item`（長さ `frames`）を、区間の表示（裏の仕事なし）で開く。
    pub fn open(session: &'s LibrarySession, item: i64, frames: u64) -> Result<Self, String> {
        let env = EditorEnv::of(session).map_err(|e| e.to_string())?;
        let engine = session.engine().ok_or("エンジンの部品がありません")?;
        let rt = engine.runtime().map_err(|e| e.to_string())?;
        let before = rt.budget().usage().map_err(|e| e.to_string())?;
        let vp = viewport(frames, false);
        let t = Instant::now();
        let opened = editor::open(
            &env,
            &OpenRequest {
                item_id: item,
                viewport: Viewport::Audio(vp.clone()),
                from_draft: false,
            },
        )
        .map_err(|e| format!("開けません: {e}"))?;
        let open_ns = nanos(t);
        let info = shown(opened.outcome.view)?;
        if info.frames != frames {
            return Err(format!("長さが違います: {} ≠ {frames}", info.frames));
        }
        let mut me = Self {
            env,
            engine,
            sid: opened.session,
            frames,
            rate: u64::from(info.rate),
            vp,
            nodes: None,
            samples: info.samples_id.clone(),
            before,
            open_ns,
            opened: AudioCounts::default(),
            held: Held::default(),
        };
        me.opened = me.counts()?;
        me.held.note(&me.opened);
        Ok(me)
    }

    /// 測るための数。
    pub fn counts(&self) -> Result<AudioCounts, String> {
        self.engine
            .editor_for(&self.sid)
            .and_then(|e| e.audio_counts())
            .ok_or_else(|| "音声のエディタが開いていません".to_string())
    }

    /// 前に読んでから通した確保の見積もりのピークの最大（読むと 0 に戻る）。
    pub fn take_admitted_peak(&self) -> Result<u64, String> {
        Ok(self
            .engine
            .runtime()
            .map_err(|e| e.to_string())?
            .take_admitted_peak())
    }

    /// 手を当てて表示する（往復のナノ秒と表示）。
    fn apply(
        &mut self,
        steps: Vec<Step>,
        vp: &AudioViewport,
    ) -> Result<(u64, AudioViewInfo), String> {
        let t = Instant::now();
        let out = editor::step(&self.env, &self.sid, steps, &Viewport::Audio(vp.clone()))
            .map_err(|e| format!("手を当てられません: {e}"))?;
        let ns = nanos(t);
        if let Some(r) = out.rejected {
            return Err(format!("手を断られました: {:?}", r.problem));
        }
        let info = shown(out.view)?;
        self.samples = info.samples_id.clone();
        let c = self.counts()?;
        self.held.note(&c);
        Ok((ns, info))
    }

    /// 文書を作る：trim（終わりは 1 秒前）→ fade（フェードイン 1 秒）→ gain（−3 dB）→ set_loop（区間の頭の 1 秒）を 1 手ずつ足す。
    pub fn setup(&mut self) -> Result<(), String> {
        let r = self.rate.min(self.frames / 8).max(1);
        let at = self.vp.spans[0].0;
        let docs = [
            ("audio.trim", json!({ "start": 0, "end": self.frames - r })),
            (
                "audio.fade",
                json!({ "fadeIn": r, "fadeOut": 0, "curve": "linear" }),
            ),
            ("audio.gain", json!({ "db": -3.0 })),
            (
                "audio.set_loop",
                json!({ "loop": { "start": at, "end": at + r } }),
            ),
        ];
        let mut ids = Vec::new();
        for (at, (op, params)) in docs.into_iter().enumerate() {
            let vp = self.vp.clone();
            let out = editor::step(
                &self.env,
                &self.sid,
                vec![Step::Commit {
                    edit: Edit::AddNode {
                        at,
                        op: op.into(),
                        v: 1,
                        params,
                        seed: None,
                        extra_inputs: Vec::new(),
                    },
                    mode: CommitMode::Step,
                }],
                &Viewport::Audio(vp),
            )
            .map_err(|e| format!("{op} を足せません: {e}"))?;
            if let Some(r) = out.rejected {
                return Err(format!("{op} を足せません: {:?}", r.problem));
            }
            let id = out
                .created
                .first()
                .copied()
                .ok_or_else(|| format!("{op} のノードができません"))?;
            self.samples = shown(out.view)?.samples_id;
            ids.push(id);
        }
        let c = self.counts()?;
        self.held.note(&c);
        self.nodes = Some([ids[0], ids[1], ids[2], ids[3]]);
        Ok(())
    }

    /// 文書の末尾にレート変換を足す（M13 段 7。48 kHz は 44.1 kHz へ。相 147・タップの多い重い向き。--quick のレートは
    /// `resample_target`）。このあとの手は、見えている区間のレート変換を毎回描き直す。
    pub fn add_resample(&mut self) -> Result<(), String> {
        let target: u64 = if self.rate == 48_000 {
            44_100
        } else {
            u32::try_from(self.rate)
                .map(kura_engine::testing::bench::cases::resample_target)
                .map_err(|_| "レートが大きすぎます".to_string())?
                .into()
        };
        let vp = self.vp.clone();
        let out = editor::step(
            &self.env,
            &self.sid,
            vec![Step::Commit {
                edit: Edit::AddNode {
                    at: 4,
                    op: "audio.resample".into(),
                    v: 1,
                    params: json!({ "rate": target }),
                    seed: None,
                    extra_inputs: Vec::new(),
                },
                mode: CommitMode::Step,
            }],
            &Viewport::Audio(vp),
        )
        .map_err(|e| format!("audio.resample を足せません: {e}"))?;
        if let Some(r) = out.rejected {
            return Err(format!("audio.resample を足せません: {:?}", r.problem));
        }
        self.samples = shown(out.view)?.samples_id;
        Ok(())
    }

    /// 測る手の `i` 回目（回ごとに違う値）と、手が効いたことを確かめる表示の値。
    fn step_of(&self, kind: Kind, i: usize) -> Result<(Step, Expect), String> {
        let [trim, fade, gain, set_loop] = self.nodes.ok_or("文書を作っていません")?;
        let r = self.rate.min(self.frames / 8).max(1);
        // 回ごとに 480 フレーム（10 ms）ずつずらす（同じ値を続けて当てない）
        let d = 480 * (i as u64 % 64 + 1);
        let at = self.vp.spans[0].0;
        let commit = HandleMode::Commit(CommitMode::Step);
        Ok(match kind {
            Kind::Gain => (
                Step::Commit {
                    edit: Edit::SetParams {
                        node: gain,
                        params: json!({ "db": -3.0 - 0.01 * (i as f64 + 1.0) }),
                    },
                    mode: CommitMode::Step,
                },
                Expect::NewSamples,
            ),
            Kind::Fade => {
                let end = (r + d).min(self.frames / 4);
                (
                    Step::Handle {
                        handle: Handle::FadeIn { end },
                        node: Some(fade),
                        mode: commit,
                    },
                    Expect::FadeIn(fade, end),
                )
            }
            Kind::TrimEnd => {
                let end = self.frames.saturating_sub(r + d).max(self.frames / 2);
                (
                    Step::Handle {
                        handle: Handle::Trim { start: 0, end },
                        node: Some(trim),
                        mode: commit,
                    },
                    Expect::Frames(end),
                )
            }
            Kind::Loop => {
                let range = (at + d % r, at + r + d % r);
                (
                    Step::Handle {
                        handle: Handle::Loop { range: Some(range) },
                        node: Some(set_loop),
                        mode: commit,
                    },
                    Expect::Loop(range),
                )
            }
        })
    }

    /// 測る手の `i` 回目を当てる（往復のナノ秒と表示）。手が効いていなければ（同じ標本・長さ・ループ点・印のまま）誤り。
    pub fn step(&mut self, kind: Kind, i: usize) -> Result<(u64, AudioViewInfo), String> {
        let (step, expect) = self.step_of(kind, i)?;
        let vp = self.vp.clone();
        let before = self.samples.clone();
        let (ns, info) = self.apply(vec![step], &vp)?;
        let fade_end = |node: NodeId| {
            info.nodes
                .iter()
                .find(|n| n.id == node)
                .and_then(|n| match n.marks {
                    Some(Marks::Fade { in_end, .. }) => in_end,
                    _ => None,
                })
        };
        let ok = match expect {
            Expect::NewSamples => info.samples_id != before,
            Expect::FadeIn(node, end) => fade_end(node) == Some(end),
            Expect::Frames(end) => info.frames == end,
            Expect::Loop(range) => info.loop_range == Some(range),
        };
        if !ok {
            return Err(format!(
                "{} の {i} 回目の手が効いていません（{expect:?}）",
                kind.key()
            ));
        }
        Ok((ns, info))
    }

    /// ラウドネスを予約し（`measure` が真のとき）、`work: true` の表示を続けて頼んで、波形の段とラウドネスが揃うまで回す。
    pub fn settle(&mut self, measure_too: bool) -> Result<Settled, String> {
        let t = Instant::now();
        let mut out = Settled::default();
        if measure_too {
            match measure(&self.env, &self.sid).map_err(|e| format!("計れません: {e}"))? {
                LoudnessState::Ready { .. } => out.ready = true,
                LoudnessState::Failed { message, .. } => {
                    return Err(format!("計れません: {message}"));
                }
                _ => {}
            }
        }
        let vp = AudioViewport {
            work: true,
            ..self.vp.clone()
        };
        let mut calls = 0u32;
        let mut peaks_done = false;
        loop {
            calls += 1;
            let one = Instant::now();
            let (_, info) = self.apply(Vec::new(), &vp)?;
            out.slowest = out.slowest.max(nanos(one));
            if !info.peaks.pending && !peaks_done {
                peaks_done = true;
                out.peaks = (calls, nanos(t));
            }
            match &info.loudness {
                LoudnessState::Ready { .. } => out.ready = true,
                LoudnessState::Failed { message, .. } => {
                    return Err(format!("計れません: {message}"));
                }
                _ => {}
            }
            if !info.work {
                break;
            }
            if calls > 100_000 {
                return Err("裏の仕事が終わりません".into());
            }
        }
        out.loudness = (calls, nanos(t));
        if measure_too && !out.ready {
            return Err("ラウドネスを計り終えませんでした".into());
        }
        Ok(out)
    }

    /// 閉じて、閉じたあとの予算の使用量（取り置き・対話・持ち続ける分が開く前に戻ったか）と作業用のファイルの数。
    pub fn close(self) -> Result<(bool, Usage, usize), String> {
        editor::close(self.engine, &self.sid);
        let rt = self.engine.runtime().map_err(|e| e.to_string())?;
        let after = rt.budget().usage().map_err(|e| e.to_string())?;
        let b = &self.before;
        let same = after.reserved == 0
            && after.interactive == b.interactive
            && after.standing == b.standing
            && after.bulk == b.bulk;
        Ok((same, after, self.engine.work_pcm().usage().0))
    }
}

/// `AudioCounts` の JSON（記録の `extra`）。
pub(crate) fn counts_json(c: &AudioCounts) -> serde_json::Value {
    json!({
        "drawn_chunks": c.area.drawn_chunks,
        "region_nodes": c.area.region_nodes,
        "full_nodes": c.area.full_nodes,
        "fallbacks": c.area.fallbacks,
        "source_reads": c.source.map(|s| s.reads),
        "cache_bytes": c.cache_bytes,
        "extra_bytes": c.extra,
        "lease_bytes": c.lease_bytes,
    })
}

/// 長さの名前（60 分は `60min`、分で割り切れなければ秒）。
pub(crate) fn length_label(secs: u64) -> String {
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}min", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// `bench edit` の音声のエディタ（NFR-11）：開く時間、4 種の手の往復の p50／p95、全体に効く手のあと裏の仕事が揃うまでの時間を、
/// プレビューのプールの数（判定の数と `--threads`）ごとに記録する。長さは `--audio-minutes`（なければ NFR-11 の音声と同じ長さ）。
pub(super) fn bench(ctx: &mut Ctx) -> Result<(), String> {
    let secs = ctx
        .opts
        .audio_minutes
        .map_or(ctx.sizes.audio_secs as u64, |m| m * 60);
    // 本番の大きさは 48 kHz（60 分の A18・mem_smoke と同じ）、--quick は極小のレート
    let rate = if ctx.opts.quick {
        ctx.sizes.audio_rate
    } else {
        RATE
    };
    let frames = secs * u64::from(rate);
    let (lib, item) = ctx.audio_library(rate, frames)?;
    let label = length_label(secs);
    let what = format!(
        "音声のエディタ（{label}・{} kHz ステレオ 16 ビットの WAV・区間 {} チャンク）",
        rate / 1000,
        AREA_CHUNKS.min(frames.div_ceil(C))
    );
    let z = ctx.sizes.clone();
    for threads in ctx.preview_set() {
        let judged = threads == ctx.opts.preview_threads;
        let session = LibrarySession::open(
            &lib,
            quiet(),
            services(threads, ctx.opts.bulk_threads, ctx.opts.budget),
        )
        .map_err(|e| format!("ライブラリを開けません: {e}"))?;
        session.wait_ready();
        crate::bench_app::settle(&session)?;

        // 開く（開いて閉じるを繰り返す）
        let load = load1();
        let mut opens = Vec::new();
        for i in 0..z.save_warmup + z.save_n {
            let ed = AudioEditor::open(&session, item, frames)?;
            if i >= z.save_warmup {
                opens.push(ed.open_ns);
            }
            ed.close()?;
        }
        let stats = Stats::from_samples(opens).ok_or("開く時間がありません")?;
        ctx.push(
            Record::timed(
                ctx.run,
                11,
                "audio-editor-open",
                &format!("audio-editor-{label}-open"),
                &format!("{what}を開く（editor::open。最初の表示まで）"),
                threads,
                stats,
                judged,
                load,
            )
            .with_note("記録だけ"),
        );

        let mut ed = AudioEditor::open(&session, item, frames)?;
        ed.setup()?;
        // 足す手（全体に効く）のあとの裏の仕事：開いたときの土台（ライブラリの段）から作り直す
        settled(
            ctx,
            &mut ed,
            &what,
            &label,
            "setup",
            "文書を作ったあと",
            threads,
            judged,
        )?;
        for kind in Kind::ALL {
            let load = load1();
            ed.take_admitted_peak()?;
            let mut samples = Vec::new();
            let mut last = None;
            for i in 0..z.edit_warmup + z.edit_n {
                let (ns, info) = ed.step(kind, i)?;
                if i >= z.edit_warmup {
                    samples.push(ns);
                }
                last = Some(info);
            }
            let stats = Stats::from_samples(samples).ok_or("手の時間がありません")?;
            let counts = ed.counts()?;
            let peak = ed.take_admitted_peak()?;
            let pending = last.as_ref().is_some_and(|i| i.peaks.pending);
            let mut r = Record::timed(
                ctx.run,
                11,
                "preview",
                &format!("audio-editor-{label}-{}", kind.key()),
                &format!("{what}の 1 手の往復：{}", kind.label()),
                threads,
                stats,
                judged,
                load,
            )
            .with_note(
                "手 → 表示の区間の値（editor::step。IPC・WebKit・PCM の取得を含まない。NFR-11 の全体は 50 ms）",
            );
            r.extra = json!({
                "counts": counts_json(&counts),
                "admitted_peak_bytes": peak,
                "max_extra_bytes": ed.held.max_extra,
                "max_cache_bytes": ed.held.max_cache,
                "peaks_pending": pending,
                "steps": z.edit_warmup + z.edit_n,
            });
            ctx.push(r);
        }
        // 全体に効く gain の手のあとの裏の仕事
        settled(
            ctx,
            &mut ed,
            &what,
            &label,
            "gain",
            "gain の手のあと",
            threads,
            judged,
        )?;
        // 末尾にレート変換があるときの gain の手（見えている区間のレート変換を毎回描き直す。M13 段 7）
        ed.add_resample()?;
        let load = load1();
        let mut samples = Vec::new();
        for i in 0..z.edit_warmup + z.edit_n {
            // 前の gain の手と同じ値を当てない（上流をキャッシュに当てない）
            let (ns, _) = ed.step(Kind::Gain, 1_000 + i)?;
            if i >= z.edit_warmup {
                samples.push(ns);
            }
        }
        let stats = Stats::from_samples(samples).ok_or("手の時間がありません")?;
        ctx.push(
            Record::timed(
                ctx.run,
                11,
                "preview",
                &format!("audio-editor-{label}-gain-resample"),
                &format!("{what}の 1 手の往復：末尾にレート変換があるときの gain の引数（区間のレート変換を毎回描き直す）"),
                threads,
                stats,
                judged,
                load,
            )
            .with_note(kura_engine::testing::bench::OUT_OF_SCOPE_RESAMPLE)
            .out_of_scope(),
        );
        let (restored, after, work_files) = ed.close()?;
        if !restored || work_files > 0 {
            return Err(format!(
                "閉じたあと予算か作業用のファイルが戻りません: {after:?}・作業用のファイル {work_files}"
            ));
        }
        session.close();
    }
    Ok(())
}

/// 裏の仕事が揃うまで（ラウドネスを予約して `work: true` を続けて頼む）を測って、波形の段とラウドネスの 2 行を記録する。
#[allow(clippy::too_many_arguments)]
fn settled(
    ctx: &mut Ctx,
    ed: &mut AudioEditor<'_>,
    what: &str,
    label: &str,
    key: &str,
    when: &str,
    threads: usize,
    judged: bool,
) -> Result<(), String> {
    let load = load1();
    ed.take_admitted_peak()?;
    let s = ed.settle(true)?;
    let peak = ed.take_admitted_peak()?;
    let extra = json!({
        "peaks_calls": s.peaks.0,
        "loudness_calls": s.loudness.0,
        "slowest_slice_ms": s.slowest as f64 / 1e6,
        "admitted_peak_bytes": peak,
        "max_extra_bytes": ed.held.max_extra,
        "max_cache_bytes": ed.held.max_cache,
    });
    for (part, (calls, ns), name) in [
        ("peaks", s.peaks, "波形の段"),
        ("loudness", s.loudness, "波形の段とラウドネス"),
    ] {
        let stats = Stats::from_samples(vec![ns]).ok_or("時間がありません")?;
        let mut r = Record::timed(
            ctx.run,
            11,
            "audio-editor-work",
            &format!("audio-editor-{label}-work-{key}-{part}"),
            &format!("{what}：{when}、裏の仕事で{name}が揃うまで"),
            threads,
            stats,
            judged,
            load,
        )
        .with_note(&format!(
            "記録だけ。work: true を続けて頼んだ計算の時間（{calls} 回・1 回の最大 {:.1} ms。フロントは手が止まって \
             150 ms で最初を頼み、そのあとは応えのたびに次を頼む）",
            s.slowest as f64 / 1e6
        ));
        r.extra = extra.clone();
        ctx.push(r);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_is_chunk_aligned_and_clipped() {
        let frames = 60 * 60 * u64::from(RATE);
        let vp = viewport(frames, false);
        let (a, b) = vp.spans[0];
        assert_eq!(a % C, 0);
        assert_eq!(b - a, AREA_CHUNKS * C);
        assert!(a <= frames / 6 && frames / 6 < a + C);
        assert_eq!(vp.visible, Some((a, b)));
        let short = viewport(3 * C, true);
        assert_eq!(short.spans[0], (0, 3 * C));
        assert!(short.work);
    }

    #[test]
    fn wav_has_the_asked_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        write_wav(&path, 8_000, 1_234).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44 + 1_234 * 4);
        let (value, _) = kura_engine::codec::CodecSet::pinned()
            .decode_path(
                &path,
                "wav",
                &Default::default(),
                &Default::default(),
                &kura_engine::NoMonitor,
            )
            .unwrap();
        assert_eq!(
            value.shape(),
            kura_engine::Shape::Audio {
                rate: 8_000,
                channels: 2,
                frames: kura_engine::Dim::Exact(1_234),
            }
        );
    }
}
