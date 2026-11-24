//! メモリの見積もりと予算の照らし（docs/editing.md §12。NFR-17・E25。段 10）。
//!
//! - 見積もり（[`Engine::estimate`]）は描かずに、計画と入力の形（ヘッダから読んだもの）と、読み方・書き方の指定から求める
//!   純粋な関数。デコードの前に呼べる（一括は、読む前に予算を確保する）。
//! - 段ごとの量（その段で生きている値と作業領域の最大）：
//!   - 読む：読み終えた入力 ＋ 読んでいる入力の値 ＋ max(デコーダの作業領域（[`Decoder::work_bytes`]。ファイルの長さを
//!     渡したら [`Decoder::file_work_bytes`]）, 読み取りの記録のダイジェストの帯)。
//!   - 描く：入力 ＋ 生きている途中の値（最後の使い手のあとで落ちる。`release_schedule`）＋ 今のノードの出力 ＋ 作業領域
//!     （[`crate::op::Op::work_bytes`] の `Work::total(並列の数)`。持ち上げたノードはフレームの形で求める）＋ 申告した解析の
//!     結果。出力の量子化した値とダイジェストの帯もここで数える。
//!   - 書く：入力 ＋ 出力 ＋ エンコーダの作業領域（[`Encoder::work_bytes`]）＋ 書いたバイト列（`CodecSet::encode_bytes` の
//!     `Vec`。値の 1 倍までとみなす）。
//! - ピーク ＝ max(読む, 描く ＋ 読む持ち越し, 書く ＋ 読む持ち越し ＋ 描く持ち越し)。段の中で返した領域はその段で使い回すが、
//!   段をまたぐと使い回さずにプロセスに残るとみなす。読む持ち越しは読む段の作業領域（値を除く）の和、描く持ち越しは描く段の
//!   最大から描き終えて残る値を引いた分。macOS のアロケータは返した大きな領域を手元に置き（large cache）、ほかの大きさ・ほかの
//!   スレッドの確保には使い回さない。24MP のプログレッシブの JPEG を読んで回して縮めて書く通し（3 スレッド）で、読む作業領域
//!   211 MiB のうち約 206 MiB が描く間も残り（`MallocLargeCache=0` で起こすと残らない）、描く段で返した途中の値も書く間に
//!   残った。docs/editing.md §12.10 の表の通しで、段の最大だけで見積もると 7 行のうち 4 行で実測が見積もりを
//!   27〜54% 上回り、この式では実測の 1.09〜1.43 倍になった（形からは見分けられないベースラインの JPEG の行は 1.97 倍）。
//! - 外したノードと、入力をそのまま返す出力は、元と同じ実体として数え直さない。キャッシュに当たるノードは描かないので 0
//!   （値はキャッシュが数え、[`CacheLease`] が予算で覆う）。そのほかは共有を見ずに新しい値として数える（多めに見積もる側）。
//! - 数えないもの：タイル・ブロック 1 つぶんの一時の領域、スレッドのスタック、計画・文書・セッションの履歴（予算の外。
//!   履歴は `session::HistoryLimits` で別に抑える）。
//!
//! 予算の照らし（[`Engine::admit`]・[`Engine::admit_cached`]）：見積もりのピークが予算の全体を超える計画は、何も確保せずに
//! `Limit`（操作は 1 つも動かない）。一括は確保できるまで待つ（中止できる）。エディタ（対話）は待たない：足りなければ
//! キャッシュの固定していないものを追い出してもう一度、それでも足りなければ `Limit`（追い出した値は戻らないが、キャッシュの
//! 上限と確保は元のまま）。確保は待つことがあるので、スレッドプールの外（ジョブのスレッド・プレビューの輪のスレッド）で行い、
//! 描き終えるまで持つ。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::cache::{FingerprintedInput, NodeCache};
use super::{Engine, Plan, PlannedNode};
use crate::budget::{Class, Lease, MemoryBudget};
use crate::codec::{CodecSet, Decoder, Encoder};
use crate::doc::{InputId, NodeId, OutputId, PortRef};
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::shape::Shape;
use crate::value::raster::DIGEST_BAND_BYTES;

/// 入力の値をどこから得るか（見積もりでの数え方）。
#[derive(Clone, Copy)]
pub enum Source<'a> {
    /// 呼び手が予算の外で数えている値（エディタの入力は `NodeCache::register_external` で数え、[`CacheLease`] が覆う）。
    /// 見積もりに入れない。
    Held,
    /// メモリにある値（合成・テスト）。値だけ数える。
    Memory,
    /// 形式 `format` から読む（その形式を読むデコーダの作業領域の最大。[`CodecSet::decode_work_bytes`]）。
    Decode {
        codecs: &'a CodecSet,
        format: &'a str,
    },
    /// 決まったデコーダで読む（記録した識別子で作り直すとき）。
    Decoder {
        decoder: &'a dyn Decoder,
        format: &'a str,
    },
    /// 遅延の値（チャンクは源の参照。M13 段 3）：値は数えず、源の LRU の上限 `resident` を読む段・描く段・書く段のどれにも
    /// 足す（読む段の作業領域は 0。保存は貸りた入力でも自分で数える）。
    Lazy { resident: u64 },
}

impl std::fmt::Debug for Source<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held => write!(f, "Held"),
            Self::Memory => write!(f, "Memory"),
            Self::Decode { format, .. } => write!(f, "Decode({format})"),
            Self::Decoder { decoder, format } => write!(f, "Decoder({}, {format})", decoder.id()),
            Self::Lazy { resident } => write!(f, "Lazy({resident})"),
        }
    }
}

/// 出力の書き出し（見積もりでの数え方）。
#[derive(Clone, Copy)]
pub enum Sink<'a> {
    /// 形式 `format`・設定 `opts` で書く（[`CodecSet::encoder`] が選ぶエンコーダ）。
    Encode {
        codecs: &'a CodecSet,
        format: &'a str,
        opts: &'a serde_json::Value,
    },
    /// 決まったエンコーダで書く（書き出しの記録の識別子で書き直すとき）。
    Encoder {
        encoder: &'a dyn Encoder,
        format: &'a str,
        opts: &'a serde_json::Value,
    },
}

impl std::fmt::Debug for Sink<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode { format, .. } => write!(f, "Encode({format})"),
            Self::Encoder {
                encoder, format, ..
            } => write!(f, "Encoder({}, {format})", encoder.id()),
        }
    }
}

/// 見積もりの条件：入力をどう得るか・出力をどう書くか・並列の数。
#[derive(Debug, Clone, Default)]
pub struct EstimateSpec<'a> {
    /// 入力ごとの来し方。書いていない入力は [`Source::Memory`]（値だけ数える）。
    pub inputs: BTreeMap<InputId, Source<'a>>,
    /// 読む入力のファイルの長さ（バイト。分かるものだけ）。読む段の作業領域を [`Decoder::file_work_bytes`] で求める
    /// （ファイル全体を読んで持つデコーダが長さを足す）。読まない入力では使わない。
    pub lens: BTreeMap<InputId, u64>,
    /// 出力ごとの書き出し。書いていない出力は書き出さない。
    pub outputs: BTreeMap<OutputId, Sink<'a>>,
    /// 同時に動く並列の仕事の数（描くプールのスレッドの数。`Work::total` に渡す）。0 は 1 とみなす。
    pub tasks: u64,
    /// 出力のダイジェストを取るか（帯の作業領域を数える。保存・一括・照合）。
    pub digests: bool,
    /// 範囲の描画（`Engine::render_area`・表示の経路・音声の流れの窓）の見積もり：出力と範囲（M13 段 1）。None は全体の描画。
    /// 範囲を持つ音声のノードは描くチャンクの分だけ、要らないノードは 0、流し解きは入力の窓 1 つを流す分と解析の結果を数える
    /// （画像は全体のまま。保守的）。
    pub area: Option<(OutputId, crate::render::region::Area)>,
    /// 流れ（`Engine::stream_audio` の全体）の見積もり：出力の窓（`STREAM_CHUNKS` チャンク）1 つを描く間のピークを、どの窓を
    /// 描いても超えないように数える（M13 段 1 の検証の直し。窓 0 の区間だけの見積もりは、窓ごとの最大ではない）。`area` より
    /// 先に見る。その出力を書き出すなら（`outputs`）、流して書く量で数える（M13 段 3：窓の出力 ＋ エンコーダの
    /// `Encoder::stream_work_bytes` ＋ 値のダイジェストの作業領域 ＋ 遅延の入力の `resident`。書いたバイト列は持たない）。
    pub stream: Option<OutputId>,
}

impl<'a> EstimateSpec<'a> {
    /// 描くだけの見積もり（入力はメモリにあって数える・書き出さない・ダイジェストを取らない）。`tasks` は並列の数。
    pub fn new(tasks: u64) -> Self {
        Self {
            tasks,
            ..Self::default()
        }
    }

    /// 入力 `id` の来し方を決める。
    pub fn input(mut self, id: InputId, src: Source<'a>) -> Self {
        self.inputs.insert(id, src);
        self
    }

    /// 入力 `id` を読むファイルの長さを決める（[`Source::Decode`]・[`Source::Decoder`] のときに使う）。
    pub fn input_len(mut self, id: InputId, len: u64) -> Self {
        self.lens.insert(id, len);
        self
    }

    /// 計画のすべての入力の来し方を `src` にする（エディタは [`Source::Held`]）。
    pub fn all_inputs(mut self, plan: &Plan, src: Source<'a>) -> Self {
        for id in plan.inputs.keys() {
            self.inputs.insert(*id, src);
        }
        self
    }

    /// 出力 `id` を書き出す。
    pub fn output(mut self, id: OutputId, sink: Sink<'a>) -> Self {
        self.outputs.insert(id, sink);
        self
    }

    /// 出力のダイジェストを取るかを決める。
    pub fn digests(mut self, on: bool) -> Self {
        self.digests = on;
        self
    }

    /// 並列の数を決める。
    pub fn tasks(mut self, tasks: u64) -> Self {
        self.tasks = tasks;
        self
    }

    /// 出力 `output` の範囲 `area` だけを描く見積もりにする（範囲の描画・表示の経路）。音声の流れの前の確保は
    /// [`EstimateSpec::stream`] を使う。
    pub fn area(mut self, output: OutputId, area: crate::render::region::Area) -> Self {
        self.area = Some((output, area));
        self
    }

    /// 出力 `output` を流す（`Engine::stream_audio` で全体を窓ごとに描く）見積もりにする：窓 1 つを描く間のピークを、どの窓
    /// でも超えないように数える（窓が 3 つまでなら全部の窓、ほかは窓 0・最後の窓と、間の窓を覆う窓 1 の広げた数え方の
    /// 最大。`render/estimate.rs` の `stream_windows_bound`）。
    pub fn stream(mut self, output: OutputId) -> Self {
        self.stream = Some(output);
        self
    }
}

/// ノード 1 つの見積もり。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeEstimate {
    /// ノード。
    pub id: NodeId,
    /// 新しく作る値のバイト数（外したノード・キャッシュに当たるノードは 0）。
    pub value: u64,
    /// 作業領域（`Work::total(並列の数)` ＋ 申告した解析の結果）。
    pub work: u64,
    /// キャッシュに当たる（描かない）。
    pub hit: bool,
    /// このノードを描いている間の量（生きている値 ＋ 出力 ＋ 作業領域）。
    pub at: u64,
}

/// 計画を描く（読む・書く）間のメモリの見積もり（バイト）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryEstimate {
    /// ピーク：max(読む, 描く ＋ 読む持ち越し, 書く ＋ 読む持ち越し ＋ 描く持ち越し)。予算に照らす量。
    pub peak: u64,
    /// 読む間の最大（読まなければ 0）。
    pub decode: u64,
    /// 描く間の最大（出力の量子化・ダイジェストを含む。持ち越しを含まない）。
    pub render: u64,
    /// 書く間の最大（書かなければ 0。持ち越しを含まない）。
    pub encode: u64,
    /// 読む持ち越し：読む段の作業領域（値を除く）の和。読み終えて返しても、描く・書く間プロセスに残るとみなす分。
    pub carried_decode: u64,
    /// 描く持ち越し：描く段の最大から、描き終えて残る値（`retained`）を引いた分。書く間もプロセスに残るとみなす
    /// （書かなければ数えない）。
    pub carried_render: u64,
    /// 描き終えたあとも残る値（入力 ＋ 出力。[`Source::Held`] の入力とキャッシュに当たった値は除く）。
    pub retained: u64,
    /// ノードごと（計画の順）。
    pub nodes: Vec<NodeEstimate>,
    /// 流して書くときに出力を全体で流す回数（`Encoder::stream_passes`。進捗の重み。流して書かなければ 0。M13 段 3）。
    pub stream_passes: u32,
}

/// 見積もりでキャッシュの当たりを引くためのもの（エディタ）。`inputs` は `render_cached` に渡すのと同じもの。
#[derive(Clone, Copy)]
pub struct CacheProbe<'a> {
    pub cache: &'a NodeCache,
    pub inputs: &'a BTreeMap<InputId, FingerprintedInput>,
}

/// 値の実体の帳簿：実体ごとのバイト数と、指している値の数。外したノード・入力をそのまま返す出力は同じ実体を指す。
#[derive(Default)]
struct Ledger {
    bytes: Vec<u64>,
    refs: Vec<u32>,
    live: u64,
}

impl Ledger {
    /// 新しい実体（生きている）。番号を返す。
    fn add(&mut self, bytes: u64) -> usize {
        self.bytes.push(bytes);
        self.refs.push(1);
        self.live = self.live.saturating_add(bytes);
        self.bytes.len() - 1
    }

    /// 同じ実体を指す値を 1 つ増やす。
    fn share(&mut self, b: usize) {
        if let Some(r) = self.refs.get_mut(b) {
            *r = r.saturating_add(1);
        }
    }

    /// 値を 1 つ落とす（どの値も指さなくなった実体は生きている量から引く）。
    fn release(&mut self, b: usize) {
        if let Some(r) = self.refs.get_mut(b)
            && *r > 0
        {
            *r -= 1;
            if *r == 0 {
                self.live = self.live.saturating_sub(self.bytes[b]);
            }
        }
    }
}

/// ダイジェストの帯の作業領域（値のダイジェストは行を `DIGEST_BAND_BYTES` までの帯に詰めて足す。値より大きくはならない）。
fn digest_work(shape: &Shape) -> u64 {
    shape.value_bytes().min(DIGEST_BAND_BYTES as u64)
}

/// 予算の全体を超える見込みの誤り（描く前に断る）。
fn over_total(peak: u64, total: u64) -> EngineError {
    EngineError::Limit(format!(
        "メモリの予算を超える見込みです（見積もり {peak} バイト。予算は {total} バイト）"
    ))
}

/// 長さ `frames` の音声を流す窓（`STREAM_CHUNKS` チャンクずつ。`audio::stream::stream_windows` と同じ切り方）の、どれを
/// 描いても見積もりが下回らない窓の候補：（区間, 広げて数えるか）。窓が 3 つまでなら全部の窓をそのまま。4 つ以上なら、窓 0
/// （左端で切れる）と最後の窓（短い・右端で切れる）をそのまま、間の窓（長さが同じ）は窓 1 を、出力のノードのほかの要る区間を
/// 両側へ 1 チャンク広げて数える（間の窓どうしは、要る区間の長さがほぼ同じで、チャンクへの揃い方だけが違う。揃え方の違いは
/// ノードごとに 1 チャンクまでで、広げた区間はそれを覆い、上流へも長い区間として伝わる）。長さ 0 は空の区間 1 つ。
fn stream_windows_bound(frames: u64) -> Vec<(crate::render::region::Area, bool)> {
    use crate::render::region::Area;
    let w = (crate::audio::stream::STREAM_CHUNKS * crate::value::AUDIO_CHUNK) as u64;
    let count = frames.div_ceil(w);
    let span = |k: u64| Area::span(k * w, (k.saturating_add(1) * w).min(frames));
    match count {
        0 => vec![(Area::Empty, false)],
        1..=3 => (0..count).map(|k| (span(k), false)).collect(),
        _ => vec![(span(0), false), (span(1), true), (span(count - 1), false)],
    }
}

/// 範囲の描画の量（[`Engine::estimate`] の `EstimateSpec::area`）。
#[derive(Default)]
struct RegionCost {
    /// 描くノード（要る範囲があり、キャッシュに当たらないもの）。ほかは 0。
    drawn: BTreeSet<NodeId>,
    /// 範囲を持つ音声のノードの値の量（ほかは全体の値）。
    bytes: BTreeMap<NodeId, u64>,
    /// 流し解きの量（入力のポートを窓で描いて流す間の最大 ＋ 解析の結果。核で描く間の量との大きいほうを数える）。
    presolve: BTreeMap<NodeId, u64>,
    /// 描いたノードが解いて、描画の終わりまで持つ解析の結果（範囲の描画の中の表 `Presolved` が持つ。M17 段 1）：このノードの
    /// 申告の結果と、その流し解きの中で解いた上流の結果（入れ子）。`render_stage` は鍵ごとに 1 度だけ数え、このノードを
    /// 描いたあとの段に足し続ける（上流の結果は、このノードを描く間にも生きている）。
    held: BTreeMap<NodeId, Held>,
}

/// 描画の終わりまで持つ解析の結果：（解析するノード, 申告の番号）→ 結果の見積もりのバイト数。同じ表は同じ鍵になるので、
/// 入れ子の流し解きで何度出てきても 1 度だけ数える（M17 段 1）。
type Held = BTreeMap<(NodeId, usize), u64>;

/// 流し解きの見積もりの覚え（上流のポートのノード, 長さ）→（窓を描いて流す間の最大, その中で解いて持つ結果）。流し解きは
/// 上流のポートの全体を流すので、外の窓によらない。入れ子の鎖で同じポートを何度も数えないようにする（k 段で 3^k 回に
/// ならない。M17 段 1）。
type PresolveMemo = BTreeMap<(NodeId, u64), (u64, Held)>;

/// キャッシュに当たるもの（描かないノードと、量子化した出力の番号）。
#[derive(Default)]
struct Hits {
    nodes: BTreeSet<NodeId>,
    outputs: BTreeSet<usize>,
}

impl Engine {
    /// 計画を描く（読む・書く）間のメモリの見積もり（描かない）。入力の形は計画のもの（ヘッダから読んだ形で compile して
    /// おけば、読む前に見積もれる）。`cache` を渡すと、当たるノード（`render_cached` が描かないもの）を引く。
    pub fn estimate(
        &self,
        plan: &Plan,
        spec: &EstimateSpec<'_>,
        cache: Option<CacheProbe<'_>>,
    ) -> Result<MemoryEstimate, EngineError> {
        let Some(out) = spec.stream else {
            return self.estimate_with(plan, spec, cache, false, None);
        };
        // 流れ：どの窓を描いても超えない見積もり（窓の候補ごとの見積もりの、ピークの最大）
        let frames = match plan.outputs.iter().find(|o| o.0 == out) {
            Some((_, _, Shape::Audio { frames, .. }, _)) => frames.max(),
            Some(_) => {
                return Err(EngineError::Unsupported(
                    "音声でない出力は流せません".into(),
                ));
            }
            None => {
                return Err(EngineError::Internal(format!(
                    "見積もりの出力 {} はありません",
                    out.0
                )));
            }
        };
        let mut most: Option<MemoryEstimate> = None;
        for (window, pad) in stream_windows_bound(frames) {
            let mut one = spec.clone();
            one.stream = None;
            one.area = Some((out, window));
            let e = self.estimate_with(plan, &one, cache, pad, Some(out))?;
            if most.as_ref().is_none_or(|m| e.peak > m.peak) {
                most = Some(e);
            }
        }
        most.ok_or_else(|| EngineError::Internal("流れの窓の見積もりがありません".into()))
    }

    /// [`Engine::estimate`] の本体（`pad` は範囲の描画の見積もりで、出力のノードのほかの要る区間を両側へ 1 チャンク広げる。
    /// `streamed` は流す出力（書き出すなら流して書く量で数える）。
    fn estimate_with(
        &self,
        plan: &Plan,
        spec: &EstimateSpec<'_>,
        cache: Option<CacheProbe<'_>>,
        pad: bool,
        streamed: Option<OutputId>,
    ) -> Result<MemoryEstimate, EngineError> {
        let hits = match cache {
            Some(probe) => self.cache_hits(plan, probe)?,
            None => Hits::default(),
        };
        let tasks = spec.tasks.max(1);
        let mut ledger = Ledger::default();
        // 読む：入力を順に読む。読み終えた入力は描き終えるまで呼び手が持つ。返した作業領域は残るとみなす（持ち越し）
        let mut decode = 0u64;
        let mut carried = 0u64;
        let mut input_buf: BTreeMap<InputId, usize> = BTreeMap::new();
        for (id, shape) in &plan.inputs {
            let src = spec.inputs.get(id).copied().unwrap_or(Source::Memory);
            let bytes = match src {
                Source::Held => 0,
                // 遅延の値は源の LRU の上限だけを持つ（どの段にも足す）
                Source::Lazy { resident } => resident,
                _ => shape.value_bytes(),
            };
            let len = spec.lens.get(id).copied();
            let work = match (src, len) {
                (Source::Decode { codecs, format }, None) => {
                    Some(codecs.decode_work_bytes(format, shape))
                }
                (Source::Decode { codecs, format }, Some(len)) => {
                    Some(codecs.decode_file_work_bytes(format, shape, len))
                }
                (Source::Decoder { decoder, format }, None) => {
                    Some(decoder.work_bytes(format, shape))
                }
                (Source::Decoder { decoder, format }, Some(len)) => {
                    Some(decoder.file_work_bytes(format, shape, len))
                }
                (Source::Held | Source::Memory | Source::Lazy { .. }, _) => None,
            };
            if let Some(work) = work {
                // 読み取りの記録のために、読んだ値のダイジェストも取る
                let work = work.max(digest_work(shape));
                let at = ledger
                    .live
                    .saturating_add(carried)
                    .saturating_add(bytes)
                    .saturating_add(work);
                decode = decode.max(at);
                carried = carried.saturating_add(work);
            }
            input_buf.insert(*id, ledger.add(bytes));
        }
        // 描く（範囲の描画なら、後ろからの走査で描くノードと量を求めておく）
        let region = match &spec.area {
            Some((out, area)) => self.output_region(plan, *out, area, &hits, cache, tasks, pad)?,
            None => None,
        };
        let (mut render, nodes) =
            self.render_stage(plan, &mut ledger, &input_buf, &hits, region.as_ref(), tasks)?;
        // 出力：量子化した値は新しい値（量子化は音声の出力だけ）。範囲の描画は頼んだ区間のチャンクの分だけを量子化する
        // （入力をそのまま出す出力・外したノードだけの鎖・全体で描いた出力のノードも同じ。render/area.rs の `AudioFinish`）。
        // ダイジェストは出力ごとに帯を持つ（範囲の描画は頼んだ出力の描いた分だけ）
        for (k, (id, _, shape, quantize)) in plan.outputs.iter().enumerate() {
            let bytes = match &spec.area {
                Some((out, _)) if out != id => continue,
                Some((_, area)) => match shape {
                    Shape::Audio {
                        channels, frames, ..
                    } => {
                        let target = super::area::clip_to_canvas(area, shape);
                        super::audio::covered_frames(&target, frames.max())
                            .saturating_mul(u64::from(*channels))
                            .saturating_mul(4)
                    }
                    _ => shape.value_bytes(),
                },
                None => shape.value_bytes(),
            };
            if quantize.is_some() && !hits.outputs.contains(&k) {
                ledger.add(bytes);
                render = render.max(ledger.live);
            }
            if spec.digests && spec.area.is_none() {
                render = render.max(ledger.live.saturating_add(digest_work(shape)));
            }
        }
        let retained = ledger.live;
        // 書く：出力を順に書く。書いたバイト列は書き終えるまで持つとみなす
        let mut encode = 0u64;
        let mut files = 0u64;
        let mut writes = false;
        let mut stream_passes = 0u32;
        for (id, _, shape, _) in &plan.outputs {
            let Some(sink) = spec.outputs.get(id) else {
                continue;
            };
            writes = true;
            if streamed == Some(*id) {
                // 流して書く（M13 段 3）：窓の出力（描く段の残り `retained` に入っている）＋ エンコーダの流れの作業領域 ＋
                // 値のダイジェスト（R1）の作業領域。書いたバイト列はファイルへ出すので持たない
                let (work, passes) = match *sink {
                    Sink::Encode {
                        codecs,
                        format,
                        opts,
                    } => (
                        codecs.encode_stream_work_bytes(format, shape, opts),
                        codecs
                            .encoder(format, shape)
                            .map_or(1, |e| e.stream_passes(format, opts)),
                    ),
                    Sink::Encoder {
                        encoder,
                        format,
                        opts,
                    } => (
                        encoder.stream_work_bytes(format, shape, opts),
                        encoder.stream_passes(format, opts),
                    ),
                };
                let channels = match shape {
                    Shape::Audio { channels, .. } => usize::from(*channels),
                    _ => 0,
                };
                let digest = if spec.digests {
                    crate::audio::digest::PcmTreeDigest::work_bytes(channels, tasks)
                } else {
                    0
                };
                stream_passes = stream_passes.max(passes);
                encode = encode.max(
                    retained
                        .saturating_add(files)
                        .saturating_add(work)
                        .saturating_add(digest),
                );
                continue;
            }
            let work = match *sink {
                Sink::Encode {
                    codecs,
                    format,
                    opts,
                } => codecs.encode_work_bytes(format, shape, opts),
                Sink::Encoder {
                    encoder,
                    format,
                    opts,
                } => encoder.work_bytes(format, shape, opts),
            };
            let file = shape.value_bytes();
            files = files.saturating_add(file);
            encode = encode.max(retained.saturating_add(files).saturating_add(work));
        }
        // 段をまたぐと、前の段で返した領域は使い回さないとみなす（描き終えて残る値は書く段で数えている）
        let carried_render = if writes {
            render.saturating_sub(retained)
        } else {
            0
        };
        Ok(MemoryEstimate {
            peak: decode.max(render.saturating_add(carried)).max(
                encode
                    .saturating_add(carried)
                    .saturating_add(carried_render),
            ),
            decode,
            render,
            encode,
            carried_decode: carried,
            carried_render,
            retained,
            nodes,
            stream_passes,
        })
    }

    /// ノードの作業領域：操作の申告（持ち上げたノードはフレームの画像の形で。フレームは順に当てるので 1 フレーム分）を
    /// 並列の数で足したものと、申告した解析の結果の見積もり。
    fn node_work(
        &self,
        node: &PlannedNode,
        in_shapes: &[Shape],
        tasks: u64,
    ) -> Result<u64, EngineError> {
        let mut ins = in_shapes.to_vec();
        let mut out = node.inferred.shape;
        if node.lifted {
            if let Some(frame) = ins.first().and_then(Shape::frame_image) {
                ins[0] = frame;
            }
            out = out.frame_image().unwrap_or(out);
        }
        let mut work = node
            .op
            .work_bytes(node.params.as_ref(), &ins, &out)?
            .total(tasks);
        for need in &node.needs {
            let a = self.solvable(need)?;
            let shape = in_shapes.get(need.input).ok_or_else(|| {
                EngineError::Internal("解析する入力の形がありません".into()).at(node.id)
            })?;
            let estimate = a.estimate(&need.params, shape).map_err(|_| {
                EngineError::Internal(format!(
                    "{} v{} の見積もりができません（計画を作ったときは通りました）",
                    need.id, need.v
                ))
                .at(node.id)
            })?;
            work = work.saturating_add(estimate.bytes);
        }
        Ok(work)
    }

    /// 描く段の量（読み終えた入力を持った `ledger` から）：計画の順に、描くノードの値と作業領域を足し、最後の使い手のあとで
    /// 落とす。`region` があれば範囲の描画の量（描かないノードは 0、範囲を持つ音声のノードは描くチャンクの分。流し解きは核で
    /// 描く前に終わるので、描く間の量との大きいほう）。返すのは段の最大とノードごとの見積もり。
    fn render_stage(
        &self,
        plan: &Plan,
        ledger: &mut Ledger,
        input_buf: &BTreeMap<InputId, usize>,
        hits: &Hits,
        region: Option<&RegionCost>,
        tasks: u64,
    ) -> Result<(u64, Vec<NodeEstimate>), EngineError> {
        let mut render = ledger.live;
        let mut shapes: BTreeMap<NodeId, Shape> = BTreeMap::new();
        let mut node_buf: BTreeMap<NodeId, usize> = BTreeMap::new();
        let mut nodes = Vec::with_capacity(plan.nodes.len());
        // 描画の終わりまで持つ解析の結果のうち、もう数えたもの（同じ表を 2 度数えない）
        let mut counted: BTreeSet<(NodeId, usize)> = BTreeSet::new();
        let buf_of = |node_buf: &BTreeMap<NodeId, usize>, r: &PortRef| match r {
            PortRef::Input { id } => input_buf.get(id).copied(),
            PortRef::Node { id } => node_buf.get(id).copied(),
        };
        for (i, node) in plan.nodes.iter().enumerate() {
            let in_shapes = node
                .inputs
                .iter()
                .map(|r| super::lookup(&shapes, &plan.inputs, r))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| EngineError::Internal("ノードの入力の形がありません".into()))?;
            let hit = node.enabled && hits.nodes.contains(&node.id);
            let empty = Held::new();
            let (value, work, presolve, held) = match region {
                _ if !node.enabled || hit => (0, 0, 0, &empty),
                Some(rc) if !rc.drawn.contains(&node.id) => (0, 0, 0, &empty),
                Some(rc) => (
                    rc.bytes
                        .get(&node.id)
                        .copied()
                        .unwrap_or_else(|| node.inferred.shape.value_bytes()),
                    self.node_work(node, &in_shapes, tasks)?,
                    rc.presolve.get(&node.id).copied().unwrap_or(0),
                    rc.held.get(&node.id).unwrap_or(&empty),
                ),
                None => (
                    node.inferred.shape.value_bytes(),
                    self.node_work(node, &in_shapes, tasks)?,
                    0,
                    &empty,
                ),
            };
            // 流し解きの中で解いた上流の結果で、まだ数えていないもの（このノードの結果は作業領域に数えてある）
            let inner = held
                .iter()
                .filter(|(k, _)| k.0 != node.id && !counted.contains(k))
                .fold(0u64, |a, (_, b)| a.saturating_add(*b));
            // 流し解きは核で描く前に終わる（入力の窓を流す間の最大と、描く間の最大の大きいほう）。描く間も、流し解きの中で
            // 解いた上流の結果は生きている
            let at = ledger.live.saturating_add(
                value
                    .saturating_add(work)
                    .saturating_add(inner)
                    .max(presolve),
            );
            render = render.max(at);
            let buf = if node.enabled {
                ledger.add(value)
            } else {
                // 外したノードは入力 0 をそのまま通す（同じ実体）
                let first = node
                    .inputs
                    .first()
                    .and_then(|r| buf_of(&node_buf, r))
                    .ok_or_else(|| {
                        EngineError::Internal("外したノードの入力がありません".into())
                    })?;
                ledger.share(first);
                first
            };
            node_buf.insert(node.id, buf);
            shapes.insert(node.id, node.inferred.shape);
            for id in plan.released_after(i) {
                if let Some(b) = node_buf.get(id) {
                    ledger.release(*b);
                }
            }
            // 解いた結果は描画の終わりまで持つ（落とさない。鍵ごとに 1 度だけ）
            let mut keep = 0u64;
            for (k, b) in held {
                if counted.insert(*k) {
                    keep = keep.saturating_add(*b);
                }
            }
            if keep > 0 {
                ledger.add(keep);
            }
            nodes.push(NodeEstimate {
                id: node.id,
                value,
                work,
                hit,
                at,
            });
        }
        Ok((render, nodes))
    }

    /// 範囲の描画の出力 `output`・範囲 `area` の量（入力をそのまま出す出力は None）。
    #[allow(clippy::too_many_arguments)]
    fn output_region(
        &self,
        plan: &Plan,
        output: OutputId,
        area: &crate::render::region::Area,
        hits: &Hits,
        cache: Option<CacheProbe<'_>>,
        tasks: u64,
        pad: bool,
    ) -> Result<Option<RegionCost>, EngineError> {
        let Some((_, from, shape, _)) = plan.outputs.iter().find(|o| o.0 == output) else {
            return Err(EngineError::Internal(format!(
                "見積もりの出力 {} はありません",
                output.0
            )));
        };
        let PortRef::Node { id } = from else {
            return Ok(None);
        };
        let probe = match cache {
            Some(p) => Some((super::area::PassCx::new(self, plan, p.inputs)?, p.cache)),
            None => None,
        };
        let target = super::area::clip_to_canvas(area, shape);
        Ok(Some(self.region_cost(
            plan,
            *id,
            target,
            hits,
            probe.as_ref(),
            tasks,
            0,
            pad,
            &mut PresolveMemo::new(),
        )?))
    }

    /// 範囲の描画の量（`region_needs` と同じ走査を、値なしで）：描くノード、範囲を持つ音声のノードの値の量（描くチャンクの
    /// 長さの和 × チャンネル × 4）、流し解きの量（入力のポートを窓 `STREAM_CHUNKS` チャンクで描く間の、どの窓でも超えない
    /// 最大 ＋ 解析の結果）。`pad` なら、出力のノードのほかの要る区間を両側へ 1 チャンク広げて数える（流れの間の窓を覆う）。
    #[allow(clippy::too_many_arguments)]
    fn region_cost(
        &self,
        plan: &Plan,
        node_id: NodeId,
        target: crate::render::region::Area,
        hits: &Hits,
        probe: Option<&(super::area::PassCx<'_>, &NodeCache)>,
        tasks: u64,
        depth: usize,
        pad: bool,
        memo: &mut PresolveMemo,
    ) -> Result<RegionCost, EngineError> {
        use crate::value::AUDIO_CHUNK;
        let shapes = super::area::Shapes::of(plan);
        let audio_ok = |n: &PlannedNode, s: &[Shape]| -> Result<bool, EngineError> {
            Ok(self.audio_regional(n, s)?
                && match probe {
                    Some((cx, cache)) => self.needs_ready(cx, cache, n)?,
                    None => self.needs_streamable(&shapes, n)?,
                })
        };
        let walk = self.region_needs(
            plan,
            &shapes,
            node_id,
            target,
            false,
            pad,
            &audio_ok,
            &|n| hits.nodes.contains(&n.id),
        )?;
        let index: BTreeMap<NodeId, &PlannedNode> =
            plan.nodes.iter().map(|n| (n.id, n.as_ref())).collect();
        let mut rc = RegionCost {
            drawn: walk
                .need
                .keys()
                .filter(|id| !walk.hits.contains(id))
                .copied()
                .collect(),
            ..RegionCost::default()
        };
        // 描いた値が完全な値になるか（範囲の描画の前から描く道 `port_pass` と同じ判定）：外したノードは入力 0 のまま、当たりは
        // 完全、音声の範囲を持つノードはすべてのチャンクが要り入力がどれも完全なら全体で描く、範囲を持たないノードは全体で描く
        // （入力が部分の値なら全体で描き直す）。画像の範囲を持つノードは部分の値とみなす（音声の解析の入力にならない）。
        // 流し解きは、申告した解析の入力がここで部分の値になるときだけ起きる（入力が完全なら値に当てて解く）。広げて数える
        // ときは、範囲を持つノードを部分の値とみなす（広げた区間がすべてのチャンクを覆っても、実際の窓では覆わないことがある）
        let mut complete: BTreeMap<NodeId, bool> = BTreeMap::new();
        for node in plan.nodes.iter().filter(|n| walk.need.contains_key(&n.id)) {
            let port_ok = |r: &PortRef, complete: &BTreeMap<NodeId, bool>| match r {
                PortRef::Input { .. } => true,
                PortRef::Node { id } => complete.get(id).copied().unwrap_or(false),
            };
            let c = if !node.enabled {
                node.inputs.first().is_some_and(|r| port_ok(r, &complete))
            } else if walk.hits.contains(&node.id) {
                true
            } else if let Some(rounded) = walk.regional.get(&node.id) {
                let all_chunks = !pad
                    && match node.inferred.shape {
                        Shape::Audio {
                            frames: crate::shape::Dim::Exact(n),
                            ..
                        } => usize::try_from(n).is_ok_and(|n| {
                            super::audio::chunks_covering(rounded, n).count()
                                == n.div_ceil(AUDIO_CHUNK)
                        }),
                        _ => false,
                    };
                all_chunks && node.inputs.iter().all(|r| port_ok(r, &complete))
            } else {
                true
            };
            complete.insert(node.id, c);
        }
        // 描くノードが解く申告の結果は、どの道（流し解き・値に当てる・全体で描く）でもこの描画の表に入り、終わりまで持つ
        for id in &rc.drawn {
            let Some(node) = index.get(id).filter(|n| n.enabled) else {
                continue;
            };
            for (k, need) in node.needs.iter().enumerate() {
                let Some(shape) = node.inputs.get(need.input).and_then(|r| shapes.port(r)) else {
                    continue;
                };
                let bytes = self
                    .solvable(need)?
                    .estimate(&need.params, &shape)
                    .map_or(0, |e| e.bytes);
                rc.held.entry(*id).or_default().insert((*id, k), bytes);
            }
        }
        for (id, rounded) in &walk.regional {
            let Some(node) = index.get(id) else { continue };
            let Shape::Audio {
                channels,
                frames: crate::shape::Dim::Exact(n),
                ..
            } = node.inferred.shape
            else {
                continue;
            };
            let n = usize::try_from(n).unwrap_or(usize::MAX);
            let frames: u64 = super::audio::chunks_covering(rounded, n)
                .iter()
                .map(|k| (n - k * AUDIO_CHUNK).min(AUDIO_CHUNK) as u64)
                .sum();
            rc.bytes.insert(
                *id,
                frames.saturating_mul(u64::from(channels)).saturating_mul(4),
            );
            // 流し解き：入力が部分の値で、解析がキャッシュにないもの
            let mut extra = 0u64;
            let mut inner_all = Held::new();
            for need in &node.needs {
                let Some(r) = node.inputs.get(need.input) else {
                    continue;
                };
                let complete = match r {
                    PortRef::Input { .. } => true,
                    PortRef::Node { id } => complete.get(id).copied().unwrap_or(false),
                };
                let cached = match probe {
                    Some((cx, cache)) => {
                        let fp = match r {
                            PortRef::Input { id } => cx.input_fps.get(id).copied(),
                            PortRef::Node { id } => cx.fps.nodes.get(id).copied(),
                        };
                        match fp {
                            Some(fp) => {
                                let key = super::cache::analysis_fingerprint(
                                    fp,
                                    need.id,
                                    need.v,
                                    &need.params,
                                )?;
                                cache.contains_analysis(&key)
                            }
                            None => false,
                        }
                    }
                    None => false,
                };
                if complete || cached {
                    continue;
                }
                let in_shape = shapes
                    .port(r)
                    .ok_or_else(|| EngineError::Internal("解析する入力の形がありません".into()))?;
                let result = self
                    .solvable(need)?
                    .estimate(&need.params, &in_shape)
                    .map_or(0, |e| e.bytes);
                let (window, inner_held): (u64, Held) = match (r, in_shape) {
                    (
                        PortRef::Node { id: up },
                        Shape::Audio {
                            frames: crate::shape::Dim::Exact(m),
                            ..
                        },
                    ) if depth < plan.nodes.len() => {
                        if let Some(known) = memo.get(&(*up, m)) {
                            known.clone()
                        } else {
                            // 窓 0 の区間だけでは窓ごとの最大にならない（縮めるリサンプルでは、間の窓が上流のノードごとに
                            // 1 チャンク多く掛かる）ので、どの窓でも超えない数え方（`stream_windows_bound`）の最大
                            let (mut most, mut inner_held) = (0u64, Held::new());
                            for (window, pad) in stream_windows_bound(m) {
                                let inner = self.region_cost(
                                    plan,
                                    *up,
                                    window,
                                    hits,
                                    probe,
                                    tasks,
                                    depth + 1,
                                    pad,
                                    memo,
                                )?;
                                let mut fresh = Ledger::default();
                                let inputs: BTreeMap<InputId, usize> =
                                    plan.inputs.keys().map(|k| (*k, fresh.add(0))).collect();
                                let at = self
                                    .render_stage(
                                        plan,
                                        &mut fresh,
                                        &inputs,
                                        hits,
                                        Some(&inner),
                                        tasks,
                                    )?
                                    .0;
                                most = most.max(at);
                                for set in inner.held.values() {
                                    for (k, b) in set {
                                        let e = inner_held.entry(*k).or_insert(0);
                                        *e = (*e).max(*b);
                                    }
                                }
                            }
                            memo.insert((*up, m), (most, inner_held.clone()));
                            (most, inner_held)
                        }
                    }
                    _ => (0, Held::new()),
                };
                extra = extra.saturating_add(window).saturating_add(result);
                for (k, b) in inner_held {
                    let e = inner_all.entry(k).or_insert(0);
                    *e = (*e).max(b);
                }
            }
            if extra > 0 {
                rc.presolve.insert(*id, extra);
            }
            if !inner_all.is_empty() {
                let set = rc.held.entry(*id).or_default();
                for (k, b) in inner_all {
                    let e = set.entry(k).or_insert(0);
                    *e = (*e).max(b);
                }
            }
        }
        Ok(rc)
    }

    /// キャッシュに当たるもの（有効なノードの値と、量子化した出力）。別のエンジンの計画は `Internal`。
    fn cache_hits(&self, plan: &Plan, probe: CacheProbe<'_>) -> Result<Hits, EngineError> {
        if plan.engine != self.token {
            return Err(EngineError::Internal(
                "別のエンジンで作った計画です（キャッシュの指紋が登録表に縛られないため）".into(),
            ));
        }
        let fps = self.fingerprints(plan, probe.inputs)?;
        let mut hits = Hits::default();
        for node in plan.nodes.iter().filter(|n| n.enabled) {
            if fps
                .nodes
                .get(&node.id)
                .is_some_and(|fp| probe.cache.contains(fp))
            {
                hits.nodes.insert(node.id);
            }
        }
        for (k, ((_, _, _, q), (_, fp))) in plan.outputs.iter().zip(&fps.outputs).enumerate() {
            if q.is_some() && probe.cache.contains(fp) {
                hits.outputs.insert(k);
            }
        }
        Ok(hits)
    }

    /// 描く前に予算を確保する（§12）。見積もりが予算の全体を超えれば、何も確保せずに `Limit`（操作は 1 つも動かない）。
    /// 一括は確保できるまで待つ（`mon` の中止で `Canceled`。使用量は変えない）。対話は待たない（今すぐ確保できなければ
    /// `Limit`。キャッシュを追い出して確かめ直すのは [`Engine::admit_cached`]）。プールの外で呼び、返したものを描き終えるまで持つ。
    pub fn admit(
        &self,
        plan: &Plan,
        spec: &EstimateSpec<'_>,
        budget: &Arc<MemoryBudget>,
        class: Class,
        mon: &dyn Monitor,
    ) -> Result<Admission, EngineError> {
        let estimate = self.estimate(plan, spec, None)?;
        if estimate.peak > budget.total() {
            return Err(over_total(estimate.peak, budget.total()));
        }
        let lease = budget.acquire(estimate.peak, class, mon)?;
        Ok(Admission { estimate, lease })
    }

    /// エディタ（対話）の確保：キャッシュの当たりを引いて見積もり、`held` を「キャッシュの分 ＋ ピーク」まで待たずに伸ばす。
    /// 伸ばせなければ、キャッシュの固定していないものを古い順に追い出して上限を下げ、見積もり直してもう一度（通ったら、
    /// `held` を「下げた上限の分 ＋ ピーク」に合わせる）。追い出せるものがなくなっても足りなければ `Limit`：追い出した値は
    /// 戻らないが、キャッシュの上限と `held` は呼ぶ前のままにする（Limit のあとに settle を呼ばなくても、上限が下がったまま
    /// 残らない。追い出したあとの見積もりが予算の全体を超えたときも同じ）。最初の見積もりが予算の全体を超えれば、何も
    /// 変えずに `Limit`。描いたあとは [`CacheLease::settle`] で伸ばした分を返す。`spec` の入力はふつう [`Source::Held`]（入力は `register_external` でキャッシュが数える）。
    pub fn admit_cached(
        &self,
        plan: &Plan,
        spec: &EstimateSpec<'_>,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        held: &mut CacheLease,
    ) -> Result<MemoryEstimate, EngineError> {
        let total = held.budget.total();
        // Limit のときに戻す上限（確保は通るまで縮めないので、そのまま残る）
        let start_capacity = cache.capacity();
        let mut lowered = false;
        // 空きを読んでから伸ばすまでの間に、ほかの確保が空きを返したときだけ、追い出さずに確かめ直す（回数を限る）
        let mut recheck = 0;
        loop {
            let estimate = self.estimate(plan, spec, Some(CacheProbe { cache, inputs }))?;
            if estimate.peak > total {
                if lowered {
                    cache.set_capacity(start_capacity);
                }
                return Err(over_total(estimate.peak, total));
            }
            // 持ち続ける表示の分（`CacheLease::extra`）もキャッシュの分に足す
            let base = cache_floor(cache);
            let need = base
                .saturating_add(held.extra)
                .saturating_add(estimate.peak);
            if need <= total && held.lease.try_grow_to(need)? {
                if lowered {
                    // 上限を下げて通った：確保を「下げた上限の分 ＋ ピーク」まで返す（今の確保が need 以上で通ったとき）
                    held.lease.shrink_to(need)?;
                }
                return Ok(estimate);
            }
            // 足りない：今すぐ持てる量（今の確保 ＋ 予算の空き）からピークを引いた分まで、キャッシュの上限を下げる
            let reach = held.lease.bytes().saturating_add(held.free()?);
            let target = reach
                .saturating_sub(estimate.peak)
                .saturating_sub(held.extra);
            if target >= base && need <= total && recheck < RECHECKS {
                recheck += 1;
                continue;
            }
            cache.set_capacity(cache.capacity().min(target));
            lowered = true;
            let after = cache_floor(cache);
            if after >= base {
                // 追い出した値は戻せない。上限は呼ぶ前に戻す（確保は縮めていない）
                cache.set_capacity(start_capacity);
                return Err(EngineError::Limit(format!(
                    "メモリの予算が足りません（見積もり {} バイト。キャッシュを追い出しても、固定した値と入力で {after} バイト\
                     を使っています。予算は {total} バイト）",
                    estimate.peak
                )));
            }
            // 追い出した当たりの分は、見積もり直しで外れとして数える（確保はまだ縮めない：Limit のときに元のまま残すため。
            // 確保は上限の分より大きいままなので、次の try_grow_to は下がった need に対して確かめる）
        }
    }
}

/// [`Engine::admit_cached`] が、空きの読み違い（ほかのスレッドの確保・返却と重なったとき）で確かめ直す回数の上限。
const RECHECKS: u32 = 3;

/// キャッシュの分の確保に要る量：上限（描く間に入れうる量）と今の大きさ（固定と外の値で上限を超えうる）の大きいほう。
fn cache_floor(cache: &NodeCache) -> u64 {
    cache.capacity().max(cache.bytes())
}

/// 描く前に確保した予算（[`Engine::admit`]）。描き終えるまで持ち、落とすと返す。
#[derive(Debug)]
pub struct Admission {
    /// 照らした見積もり。
    pub estimate: MemoryEstimate,
    /// 確保（見積もりのピークの分）。
    pub lease: Lease,
}

/// エディタのキャッシュの分の確保（§12：キャッシュの値と入力も予算に数える）。確保は、描いていない間はキャッシュの上限と
/// 今の大きさの大きいほう、描く間は [`Engine::admit_cached`] がそれに見積もりのピークを足した分まで伸ばす。対話の確保なので
/// 待たない。描いていない間の分は、手の合間も持ち続ける対話として予算に申告する（`settle`。一括の駐車の条件に入る。描く間
/// だけ伸ばす分は入れない）。エディタを開いている間の取り置き（`MemoryBudget::set_reserved`）は呼び手が決める。
pub struct CacheLease {
    budget: Arc<MemoryBudget>,
    lease: Lease,
    /// 望むキャッシュの上限（予算が空いていれば、ここまで戻す）。
    want: u64,
    /// キャッシュの外で持ち続ける分（M13 段 4：音声のエディタの公開した表示の値・波形の段。[`CacheLease::set_extra`]）。
    extra: u64,
}

impl CacheLease {
    /// エディタを開くとき：キャッシュの上限を `want`（ふつうは取り置きの量）にして、その分を対話で確保する。待たない：
    /// 予算が空いていなければ、空いている分まで上限を下げる（描いたあとの [`CacheLease::settle`] で戻す）。
    pub fn open(
        budget: &Arc<MemoryBudget>,
        cache: &mut NodeCache,
        want: u64,
    ) -> Result<Self, EngineError> {
        let lease = budget.try_acquire(0, Class::Interactive)?.ok_or_else(|| {
            EngineError::Internal("0 バイトの対話の確保が通りませんでした".into())
        })?;
        let mut held = Self {
            budget: Arc::clone(budget),
            lease,
            want,
            extra: 0,
        };
        held.settle(cache)?;
        Ok(held)
    }

    /// 確保しているバイト数。
    pub fn bytes(&self) -> u64 {
        self.lease.bytes()
    }

    /// 望むキャッシュの上限。
    pub fn want(&self) -> u64 {
        self.want
    }

    /// キャッシュの外で持ち続ける分（バイト）。
    pub fn extra(&self) -> u64 {
        self.extra
    }

    /// キャッシュの外で持ち続ける分を `bytes` にする（M13 段 4。音声のエディタが公開した表示の値と波形の段）。確保は
    /// 描いていない間も「キャッシュの分 ＋ extra」になり（足りなければキャッシュの上限を下げる）、`admit_cached` も extra を
    /// 足して伸ばす。持ち続ける対話の申告にも入る。効くのは次の [`CacheLease::settle`] から。
    pub fn set_extra(&mut self, bytes: u64) {
        self.extra = bytes;
    }

    /// 描いたあと・外の値を足したあと：確保をキャッシュの分（上限と今の大きさの大きいほう）に戻す。上限が望む量より
    /// 小さければ、予算の空きの分まで戻す（待たない）。固定した値と外の値（入力）だけで予算に収まらなければ、確保できる
    /// だけ確保して `Limit`（開くときなら、エディタを開けない）。戻した確保は手の合間も持ち続ける対話として予算に申告する
    /// （`Lease::set_standing`。取り置きを超えていれば、それを超える一括は駐車して後ろの一括を塞がない）。
    pub fn settle(&mut self, cache: &mut NodeCache) -> Result<(), EngineError> {
        let r = self.fit(cache);
        let standing = self.lease.bytes();
        let marked = self.lease.set_standing(standing);
        r.and(marked)
    }

    /// 確保をキャッシュの分に戻す（[`CacheLease::settle`] の本体）。
    fn fit(&mut self, cache: &mut NodeCache) -> Result<(), EngineError> {
        let total = self.budget.total();
        let extra = self.extra;
        let reach = self.lease.bytes().saturating_add(self.free()?);
        cache.set_capacity(self.want.min(reach.saturating_sub(extra)));
        let need = cache_floor(cache).saturating_add(extra);
        if need <= self.lease.bytes() {
            return self.lease.shrink_to(need);
        }
        if need <= total && self.lease.try_grow_to(need)? {
            return Ok(());
        }
        // 空きをほかの確保が先に使った：今の確保に収まる上限にする（固定と外の値は残る）
        cache.set_capacity(self.lease.bytes().saturating_sub(extra));
        let need = cache_floor(cache).saturating_add(extra);
        if need <= self.lease.bytes() || (need <= total && self.lease.try_grow_to(need)?) {
            return Ok(());
        }
        Err(EngineError::Limit(format!(
            "メモリの予算が足りません（キャッシュの固定した値と入力で {need} バイト。確保できたのは {} バイト・予算は \
             {total} バイト）",
            self.lease.bytes()
        )))
    }

    /// 予算の空き（全体 − 対話 − 一括）。
    fn free(&self) -> Result<u64, EngineError> {
        let u = self.budget.usage()?;
        Ok(u.total.saturating_sub(u.interactive.saturating_add(u.bulk)))
    }
}

impl std::fmt::Debug for CacheLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheLease")
            .field("bytes", &self.lease.bytes())
            .field("want", &self.want)
            .field("extra", &self.extra)
            .finish()
    }
}
