//! スペクトルのタイル（M17 段 4。docs/editing.md §18.4・§12.12・§12.13）。
//!
//! 式は kura-engine の `audio::spec`（`kura-spec/1`）にあり、ここはタイルを作って配る部品（波形の段と同じ置き方）。
//!
//! - 表示の頼み（`AudioViewport::spec`。2 つまで：前と後）が、ポート（ノードの入力・出力。なければ出力）・チャンネル・出力の
//!   フレームの区間を指す。表示の計画（流し解きの間は握る計画）で、そのポートの区間に掛かるチャンクのタイルを作る。区間は
//!   出力のフレームで受け、時間の写しでポートのフレームに写す（応えに写しの区分を返す）。
//! - 作ったタイルは公開の置き場（`SpecStore`。`Published` の中、16 MiB の LRU）に置き、配信（`/edit/{s}/spec/{key}/{ch}/{chunk}`）
//!   はそれを読むだけ（描かない・状態の錠を待たない）。鍵はポートの指紋の頭 16 バイトとチャンネルとチャンクで、中身は
//!   変わらない（immutable）。追い出されたら 404 で、フロントは頼み直す。
//! - 1 回の頼みで作る量：裏の仕事の頼み（`work`）は区切りの締め切りの残りで（流し解きのあと、波形の段の前）、ほかの頼み
//!   （手・表示）は `INLINE_CHUNKS` チャンクまで。操作中は向きが出力のポート（手ごとに鍵が変わる「後」）は作らない（画面は
//!   前 ＋ 曲線で描く）。残りは `pending` で知らせ、フロントは裏の仕事を頼む。
//! - ポートは [`Port`]（`Editor::resolve_port`）：文書の出力か、出力の時間の写しの連鎖（`Plan::time_chain`）にあるノードの
//!   入力 0・出力。鍵はポートの指紋（`Engine::fingerprints`・`Engine::port_fingerprint`）、写しはポートより下流の連鎖の
//!   `source_map`。文書の出力の標本は、公開した表示の値が覆っていればそこから読み、覆っていなければ出力を区間で流す
//!   （`stream_audio`）。ノードのポートは `Engine::stream_node_port` で流す（ポートに届く申告した解析が手元になければ
//!   断られるので、作らずに `pending` のまま）。どちらも読みは源の LRU に入れず、窓の前後 2N を足す。
//! - 部分の作り直し：文書の出力は「タイルを作った鍵」からの変わった区間を貯め（形が変われば全体）、新しい鍵では、貯めた
//!   区間 ±(4N − hop) に掛からないチャンクのタイルを前の鍵から写す（値は同じ）。ノードのポートは作り直す。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use kura_engine::audio::source::read_through;
use kura_engine::audio::spec::{SPEC_VERSION, SpecChannel, SpecChunk, SpecInput, SpecParams};
use kura_engine::audio::stream::{AudioSink, StreamInfo};
use kura_engine::doc::{NodeId, PortRef};
use kura_engine::render::Plan;
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::port::PortSide;
use kura_engine::render::region::{Area, PartialValue, Spans};
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{EngineError, Monitor};

use super::super::view::AudioViewport;
use super::super::{Editor, State, settle};
use super::slice::{Limit, Pace, WorkProgress};
use super::spec_average::{AverageJob, GroupStore, Summary, SummaryStore};
use super::timeline::{self, SourcePiece};
use super::{ChangedSpans, audio_shape};

/// 公開の置き場の上限（バイト）。
pub const SPEC_STORE_BYTES: u64 = 16 << 20;
/// 1 回の頼みのポートの数（前と後）。
pub const MAX_SPEC_PORTS: usize = 2;
/// 裏の仕事でない頼み（手・表示）で作るチャンクの数。
pub const INLINE_CHUNKS: u64 = 2;
/// 1 回のまとまりで作るチャンクの数の上限。
const MAX_GROUP_CHUNKS: u64 = 8;
/// 1 回のまとまりの一時の作業領域（流した標本と帯の値の行）の上限（バイト）。まとまりのチャンクの数をこれで切る。取り置きには
/// 数えない（区切りの中だけで持つ）。
pub const WORK_BYTES: u64 = 8 << 20;

/// 1 つのポートの 1 回の頼みで受け付けるチャンクの数：置き場の半分をポートの数で分けた量に入るタイルの数（頼みの順で切る）。
/// 頼んだタイルが、同じ頼みで作るほかのタイルに追い出されないようにする。
pub fn max_chunks(p: &SpecParams) -> u64 {
    let tile = TILE_HEADER as u64 + p.frames_per_chunk() * p.bands().len() as u64;
    (SPEC_STORE_BYTES / 2 / MAX_SPEC_PORTS as u64 / tile.max(1)).max(1)
}

/// 1 回のまとまりで作るチャンクの数（[`MAX_GROUP_CHUNKS`] と、作業領域 [`WORK_BYTES`] に入る数の小さいほう。1 以上）。
pub fn group_chunks(p: &SpecParams, channels: u16) -> u64 {
    let rows = p.work_bytes(0, p.frames_per_chunk());
    let planes = u64::from(channels) * AUDIO_CHUNK as u64 * 4;
    (WORK_BYTES / rows.saturating_add(planes).max(1)).clamp(1, MAX_GROUP_CHUNKS)
}
/// タイルの形式の印。
pub const TILE_MAGIC: &[u8; 4] = b"kst1";
/// タイルの頭の長さ（バイト）。
pub const TILE_HEADER: usize = 40;

/// ポートの向き。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpecSide {
    /// ノードの入力（前）。
    Input,
    /// ノードの出力（後）。
    Output,
}

/// スペクトルの頼み 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecRequest {
    /// ノード（None は文書の出力）。
    pub node: Option<NodeId>,
    pub side: SpecSide,
    pub channel: SpecChannel,
    /// 出力のフレームの区間（先のものほど先に作る）。
    pub spans: Vec<(u64, u64)>,
}

/// ポート 1 つのスペクトルの状態（表示の情報）。
#[derive(Debug, Clone, PartialEq)]
pub struct SpecPortInfo {
    pub node: Option<NodeId>,
    pub side: SpecSide,
    pub channel: SpecChannel,
    /// ポートの鍵（指紋の頭 16 バイトの 16 進。配信の URL に入れる）。
    pub key: String,
    /// ポートのレート・長さ（フレーム）。
    pub rate: u32,
    pub frames: u64,
    /// 式の窓の大きさ・hop・帯の数（`kura-spec/1`）。
    pub n: u32,
    pub hop: u32,
    pub bands: u32,
    /// 出力のフレーム → ポートのフレームの写しの区分（`in` がポート）。
    pub map: Vec<SourcePiece>,
    /// 1 回の頼みで受け付けるチャンクの数（[`max_chunks`]。超えた分は頼みの順で切り、`ready`・`pending` に入らない）。
    pub max_chunks: u64,
    /// 置き場にあるチャンクの区間（チャンクの番号の半開区間）。
    pub ready: Vec<(u64, u64)>,
    /// 頼んだ区間のうち、まだ作っていないチャンクがある。
    pub pending: bool,
}

/// タイルの鍵：（ポートの鍵, チャンネルの符号, チャンク）。
pub(crate) type TileKey = ([u8; 16], u16, u64);

/// 公開の置き場（`Published` の中。配信は錠を待たずにここを読む）。使った順の LRU。
#[derive(Default)]
pub(crate) struct SpecStore {
    tiles: BTreeMap<TileKey, (u64, Arc<Vec<u8>>)>,
    order: BTreeMap<u64, TileKey>,
    tick: u64,
    bytes: u64,
}

impl SpecStore {
    /// タイル（使った順を新しくする）。
    pub(crate) fn get(&mut self, key: &TileKey) -> Option<Arc<Vec<u8>>> {
        let (old, bytes) = self.tiles.get(key).map(|(t, b)| (*t, b.clone()))?;
        self.order.remove(&old);
        self.tick += 1;
        self.order.insert(self.tick, *key);
        if let Some(e) = self.tiles.get_mut(key) {
            e.0 = self.tick;
        }
        Some(bytes)
    }

    pub(crate) fn contains(&self, key: &TileKey) -> bool {
        self.tiles.contains_key(key)
    }

    /// 入れて、上限まで古い順に追い出す。
    pub(crate) fn put(&mut self, key: TileKey, bytes: Arc<Vec<u8>>) {
        if let Some((t, b)) = self.tiles.remove(&key) {
            self.order.remove(&t);
            self.bytes = self.bytes.saturating_sub(b.len() as u64);
        }
        self.tick += 1;
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        self.order.insert(self.tick, key);
        self.tiles.insert(key, (self.tick, bytes));
        while self.bytes > SPEC_STORE_BYTES {
            let Some((_, k)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, b)) = self.tiles.remove(&k) {
                self.bytes = self.bytes.saturating_sub(b.len() as u64);
            }
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// チャンネルの符号（タイルの頭と鍵）：番号はそのまま、M = 0xFFFD、S = 0xFFFE、和 = 0xFFFF。
pub fn chan_code(c: SpecChannel) -> u16 {
    match c {
        SpecChannel::Index(i) => i.min(0xFFFC),
        SpecChannel::Mid => 0xFFFD,
        SpecChannel::Side => 0xFFFE,
        SpecChannel::Sum => 0xFFFF,
    }
}

/// URL のチャンネルの綴り（番号の 10 進・`m`・`s`・`sum`）。
pub fn chan_token(c: SpecChannel) -> String {
    match c {
        SpecChannel::Index(i) => i.to_string(),
        SpecChannel::Mid => "m".into(),
        SpecChannel::Side => "s".into(),
        SpecChannel::Sum => "sum".into(),
    }
}

/// URL のチャンネルの綴りを読む。
pub fn parse_chan_token(s: &str) -> Option<SpecChannel> {
    match s {
        "m" => Some(SpecChannel::Mid),
        "s" => Some(SpecChannel::Side),
        "sum" => Some(SpecChannel::Sum),
        _ if !s.is_empty() && s.len() <= 5 && s.bytes().all(|b| b.is_ascii_digit()) => {
            let i: u32 = s.parse().ok()?;
            (i <= 0xFFFC && (s == "0" || !s.starts_with('0')))
                .then_some(SpecChannel::Index(i as u16))
        }
        _ => None,
    }
}

/// ポートの鍵の 16 進（32 字）。
pub fn key_hex(key: &[u8; 16]) -> String {
    crate::library::hex(key)
}

/// 16 進（32 字・小文字）を鍵に。
pub fn parse_key_hex(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// タイルのバイト列（`kst1`。リトルエンディアン）：印・式の番号（u16）・チャンネルの符号（u16）・レート（u32）・N（u32）・
/// hop（u32）・帯の数（u16）・1 列のフレーム数（u16。今は 1）・最初のフレームの番号（u64）・列の数（u32）・予約（u32 の 0）、
/// 続けて列 × 帯の u8（列が外）。
pub fn tile_bytes(p: &SpecParams, chan: SpecChannel, chunk: &SpecChunk) -> Vec<u8> {
    let bands = p.bands().len();
    let mut out = Vec::with_capacity(TILE_HEADER + chunk.data.len());
    out.extend_from_slice(TILE_MAGIC);
    out.extend_from_slice(&SPEC_VERSION.to_le_bytes());
    out.extend_from_slice(&chan_code(chan).to_le_bytes());
    out.extend_from_slice(&p.rate().to_le_bytes());
    out.extend_from_slice(&(p.n() as u32).to_le_bytes());
    out.extend_from_slice(&(p.hop() as u32).to_le_bytes());
    out.extend_from_slice(&(bands as u16).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&chunk.first_frame.to_le_bytes());
    out.extend_from_slice(&chunk.frames.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&chunk.data);
    out
}

/// 変わった区間（タイルを作った鍵から）。
#[derive(Debug, Clone, PartialEq)]
enum Dirty {
    All,
    Spans(Vec<(u64, u64)>),
}

impl Dirty {
    fn add(&mut self, changed: &ChangedSpans) {
        match (&mut *self, changed) {
            (Self::All, _) => {}
            (_, ChangedSpans::All) => *self = Self::All,
            (Self::Spans(v), ChangedSpans::Spans(s)) => v.extend(s.iter().copied()),
        }
    }

    /// チャンク `c` の窓（前後 `pad`）が変わった区間に掛かるか。
    fn touches(&self, c: u64, pad: u64) -> bool {
        match self {
            Self::All => true,
            Self::Spans(v) => {
                let size = AUDIO_CHUNK as u64;
                let (a, b) = ((c * size).saturating_sub(pad), (c + 1) * size + pad);
                v.iter().any(|(x, y)| *x < b && a < *y)
            }
        }
    }
}

/// 出力のポートの鍵の移り（部分の作り直し）。
#[derive(Debug, Clone)]
struct Lineage {
    /// タイルを作った鍵。
    base: [u8; 16],
    /// 今の鍵。
    cur: [u8; 16],
    /// base からの変わった区間。
    dirty: Dirty,
    /// 今の形（レート・チャンネル・長さ）。
    shape: (u32, u16, u64),
}

/// セッションのスペクトルの仕事（`AudioState`）。
#[derive(Default)]
pub(crate) struct SpecJob {
    params: BTreeMap<u32, Arc<SpecParams>>,
    /// タイルを作る速さ（区間の平均と分け合う）。
    pub(crate) pace: Pace,
    lineage: Option<Lineage>,
    /// チャンクの要約（区間の平均）。
    pub(crate) summaries: SummaryStore,
    /// まとまりの要約（区間の平均）。
    pub(crate) groups: GroupStore,
    /// 区間の平均の予約と結果。
    pub(crate) average: AverageJob,
    /// 前の頼みのポートごとの鍵（操作中に、鍵が変わったポートを作らないため）。
    last_keys: Vec<(Option<NodeId>, SpecSide, [u8; 16])>,
}

impl SpecJob {
    /// 持っている式の表のバイト数（取り置きに数える）。
    pub(crate) fn params_bytes(&self) -> u64 {
        self.params
            .values()
            .map(|p| p.resident_bytes())
            .fold(0, u64::saturating_add)
    }

    /// ポートの前の鍵を `key` に替え、前の鍵と同じだったか（初めてなら偽）。
    fn same_key(&mut self, node: Option<NodeId>, side: SpecSide, key: [u8; 16]) -> bool {
        match self
            .last_keys
            .iter_mut()
            .find(|(n, s, _)| *n == node && *s == side)
        {
            Some(e) => {
                let same = e.2 == key;
                e.2 = key;
                same
            }
            None => {
                self.last_keys.push((node, side, key));
                if self.last_keys.len() > 2 * MAX_SPEC_PORTS {
                    self.last_keys.remove(0);
                }
                false
            }
        }
    }

    /// レートの式（作ったものを使い回す）。
    pub(super) fn params(&mut self, rate: u32) -> Result<Arc<SpecParams>, EngineError> {
        if let Some(p) = self.params.get(&rate) {
            return Ok(p.clone());
        }
        let p = Arc::new(SpecParams::for_rate(rate)?);
        self.params.insert(rate, p.clone());
        Ok(p)
    }

    /// 出力を公開した（鍵 `key`・形 `shape`（レート・チャンネル・長さ）、前の表示からの変わった区間 `changed`）。レートか
    /// チャンネルが変われば全体、長さが変われば前と今の長さの間も変わったとする（尾のチャンクの窓は長さの外を 0 で読む）。
    pub(crate) fn note_output(
        &mut self,
        key: [u8; 16],
        shape: (u32, u16, u64),
        changed: &ChangedSpans,
    ) {
        match self.lineage.as_mut() {
            None => {
                self.lineage = Some(Lineage {
                    base: key,
                    cur: key,
                    dirty: Dirty::Spans(Vec::new()),
                    shape,
                });
            }
            Some(l) if l.cur == key => {}
            Some(l) => {
                if (l.shape.0, l.shape.1) != (shape.0, shape.1) {
                    l.dirty = Dirty::All;
                } else {
                    l.dirty.add(changed);
                    if l.shape.2 != shape.2 {
                        let span = (l.shape.2.min(shape.2), l.shape.2.max(shape.2));
                        l.dirty.add(&ChangedSpans::Spans(vec![span]));
                    }
                }
                l.cur = key;
                l.shape = shape;
            }
        }
    }
}

/// 区間 [start, end) を集める受け手（出力を流すとき）。
struct Collect {
    start: u64,
    planes: Vec<Vec<f32>>,
}

impl AudioSink for Collect {
    fn begin(&mut self, _info: &StreamInfo) -> Result<(), EngineError> {
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        _mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        for (dst, src) in self.planes.iter_mut().zip(planes) {
            let end = self.start + dst.len() as u64;
            let (a, b) = (start.max(self.start), (start + src.len() as u64).min(end));
            if a >= b {
                continue;
            }
            let (da, sa) = ((a - self.start) as usize, (a - start) as usize);
            let n = (b - a) as usize;
            dst[da..da + n].copy_from_slice(&src[sa..sa + n]);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// 頼んだ区間（ポートのフレーム）に掛かるチャンクを、頼みの順に重ねずに並べる（長さ `frames` の内）。
fn wanted_chunks(spans: &[(u64, u64)], frames: u64) -> Vec<u64> {
    let size = AUDIO_CHUNK as u64;
    let mut out: Vec<u64> = Vec::new();
    for &(a, b) in spans {
        let (a, b) = (a.min(frames), b.min(frames));
        if a >= b {
            continue;
        }
        for c in a / size..b.div_ceil(size) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

/// チャンクの番号の並び（昇順にしたもの）を半開区間の列に。
fn runs(mut chunks: Vec<u64>) -> Vec<(u64, u64)> {
    chunks.sort_unstable();
    chunks.dedup();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for c in chunks {
        match out.last_mut() {
            Some(r) if r.1 == c => r.1 = c + 1,
            _ => out.push((c, c + 1)),
        }
    }
    out
}

/// 頼みのポート（文書の出力か、ノードの入力 0・出力）を今の計画で解いたもの。
#[derive(Debug, Clone)]
pub(crate) struct Port {
    pub(crate) node: Option<NodeId>,
    pub(crate) side: SpecSide,
    /// 見積もりのポート（ノードのとき）。
    port_ref: Option<PortRef>,
    /// ポートの鍵（指紋の頭 16 バイト）。
    pub(crate) key: [u8; 16],
    pub(crate) rate: u32,
    pub(crate) channels: u16,
    /// ポートの長さ（フレーム）。
    pub(crate) frames: u64,
    /// 出力のフレーム → ポートのフレームの写しの区分。
    pub(crate) map: Vec<SourcePiece>,
}

impl Port {
    /// 出力のフレームの区間の列を、ポートのフレームの区間の列に写す（区分の中は比で写し、頭は切り捨て・終わりは切り上げ）。
    pub(crate) fn to_port(&self, spans: &[(u64, u64)]) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        for &(a, b) in spans {
            for p in &self.map {
                let (x, y) = (a.max(p.out_start), b.min(p.out_end));
                if x >= y {
                    continue;
                }
                let scale = |d: u64, up: bool| -> u64 {
                    let num = u128::from(d) * u128::from(p.in_rate);
                    let den = u128::from(p.out_rate.max(1));
                    let q = if up { num.div_ceil(den) } else { num / den };
                    u64::try_from(q).unwrap_or(u64::MAX)
                };
                let s = p.in_start.saturating_add(scale(x - p.out_start, false));
                let e = p.in_start.saturating_add(scale(y - p.out_start, true));
                if s < e {
                    out.push((s, e.min(self.frames)));
                }
            }
        }
        out
    }
}

impl From<SpecSide> for PortSide {
    fn from(s: SpecSide) -> Self {
        match s {
            SpecSide::Input => Self::Input,
            SpecSide::Output => Self::Output,
        }
    }
}

impl Editor {
    /// 頼みのポートを計画 `plan` で解く：ノードがなければ文書の出力（向きは出力だけ）、あればそのノードの入力 0・出力（出力の
    /// 時間の写しの連鎖にあるノードだけ）。写せない（`Unknown`・`Reset`）・長さの分からないポートは None。
    pub(in crate::edit::editor) fn resolve_port(
        &self,
        st: &State,
        plan: &Plan,
        node: Option<NodeId>,
        side: SpecSide,
    ) -> Option<Port> {
        let Some(node) = node else {
            if side != SpecSide::Output {
                return None;
            }
            let full = plan
                .output_shapes()
                .into_iter()
                .find(|(o, _)| *o == st.output)
                .map(|(_, s)| s)?;
            let (rate, channels, frames) = audio_shape(&full)?;
            let fps = self
                .runtime
                .preview(|e| e.fingerprints(plan, &st.inputs))
                .ok()?;
            let fp = fps
                .outputs
                .iter()
                .find(|(o, _)| *o == st.output)
                .map(|(_, f)| *f)?;
            let mut key = [0u8; 16];
            key.copy_from_slice(&fp.0.0[..16]);
            return Some(Port {
                node: None,
                side,
                port_ref: None,
                key,
                rate,
                channels,
                frames,
                map: vec![SourcePiece {
                    out_start: 0,
                    out_end: frames,
                    in_start: 0,
                    in_rate: rate,
                    out_rate: rate,
                }],
            });
        };
        let chain = plan.time_chain(st.output)?;
        let i = chain.iter().position(|l| l.node == node)?;
        let (shape, rest) = match side {
            SpecSide::Input => (chain[i].input, &chain[i..]),
            SpecSide::Output => (chain[i].output, &chain[i + 1..]),
        };
        let port_ref = plan.node_port(node, side.into()).ok()?;
        let (rate, channels, frames) = audio_shape(&shape)?;
        let map = timeline::source_map(rest, &shape)?;
        let fp = self
            .runtime
            .preview(|e| e.port_fingerprint(plan, &st.inputs, node, side.into()))
            .ok()?;
        let mut key = [0u8; 16];
        key.copy_from_slice(&fp.0.0[..16]);
        Some(Port {
            node: Some(node),
            side,
            port_ref: Some(port_ref),
            key,
            rate,
            channels,
            frames,
            map,
        })
    }

    /// ポートに届く申告した解析がすべて手元にあるか（なければ流すと断られる・錠の中で流し解く）。
    pub(in crate::edit::editor) fn port_solved(
        &self,
        st: &State,
        plan: &Plan,
        port: &Port,
    ) -> bool {
        let missing = self.runtime.preview(|e| match port.node {
            None => e.missing_solves(plan, &st.inputs, &st.cache, st.output),
            Some(node) => {
                e.missing_port_solves(plan, &st.inputs, &st.cache, node, port.side.into())
            }
        });
        missing.is_ok_and(|m| !m.unstreamable && m.tasks.is_empty())
    }

    /// 表示の頼みのスペクトル（モジュールの説明）：`plan` は表示の計画、`shown` はその表示の値。`limit` は裏の仕事の区切り
    /// （None は手・表示の頼み）。頼みのポートの状態を返す。
    #[allow(clippy::too_many_arguments)]
    pub(in crate::edit::editor) fn advance_spec(
        &self,
        st: &mut State,
        plan: &Plan,
        shown: &PartialValue,
        avp: &AudioViewport,
        mut limit: Option<&mut Limit>,
        progress: &mut WorkProgress,
        mon: &dyn Monitor,
    ) -> Vec<SpecPortInfo> {
        let mut infos = Vec::new();
        let mut inline = INLINE_CHUNKS;
        for req in avp.spec.iter().take(MAX_SPEC_PORTS) {
            let Some(info) = self.spec_port(
                st,
                plan,
                shown,
                req,
                avp,
                &mut limit,
                &mut inline,
                progress,
                mon,
            ) else {
                continue;
            };
            infos.push(info);
        }
        infos
    }

    #[allow(clippy::too_many_arguments)]
    fn spec_port(
        &self,
        st: &mut State,
        plan: &Plan,
        shown: &PartialValue,
        req: &SpecRequest,
        avp: &AudioViewport,
        limit: &mut Option<&mut Limit>,
        inline: &mut u64,
        progress: &mut WorkProgress,
        mon: &dyn Monitor,
    ) -> Option<SpecPortInfo> {
        let port = self.resolve_port(st, plan, req.node, req.side)?;
        if !req.channel.valid_for(usize::from(port.channels)) {
            return None;
        }
        let key = port.key;
        let code = chan_code(req.channel);
        let params = st.audio.as_mut()?.spec.params(port.rate).ok()?;
        // 置き場に収まる数まで（頼みの順で切る）
        let cap = max_chunks(&params);
        let mut wanted = wanted_chunks(&port.to_port(&req.spans), port.frames);
        wanted.truncate(usize::try_from(cap).unwrap_or(usize::MAX));
        let group = group_chunks(&params, port.channels);
        // 操作中は、前の頼みから鍵が変わったポート（ドラッグしているノードより下流。後は手ごとに変わる）を作らない（画面は
        // 前 ＋ 曲線で描く）
        let same_key = st.audio.as_mut()?.spec.same_key(req.node, req.side, key);
        let may_build = !avp.interacting || same_key;
        // 部分の作り直し（文書の出力だけ）：前の鍵のタイルのうち、変わった区間に掛からないものを写す
        let lineage = st
            .audio
            .as_ref()
            .and_then(|a| a.spec.lineage.clone())
            .filter(|_| port.node.is_none());
        if let Some(l) = lineage.filter(|l| l.cur == key && l.base != key) {
            let pad = (params.n() * kura_engine::audio::spec::LONG - params.hop()) as u64;
            let mut p = self.lock_published();
            for &c in &wanted {
                if p.spec.contains(&(key, code, c)) || l.dirty.touches(c, pad) {
                    continue;
                }
                if let Some(bytes) = p.spec.get(&(l.base, code, c)) {
                    p.spec.put((key, code, c), bytes);
                }
            }
        }
        let missing: Vec<u64> = {
            let p = self.lock_published();
            wanted
                .iter()
                .copied()
                .filter(|c| !p.spec.contains(&(key, code, *c)))
                .collect()
        };
        let mut built_any = false;
        let mut left = missing.clone();
        while may_build && !left.is_empty() {
            let want = match limit.as_deref_mut() {
                Some(l) => {
                    let pace = st.audio.as_ref().map(|a| a.spec.pace).unwrap_or_default();
                    l.grant(&pace, group)
                }
                None => (*inline).min(group),
            };
            if want == 0 {
                break;
            }
            // 先頭のチャンクから続いたものを want まで
            let c0 = left[0];
            let mut c1 = c0 + 1;
            while c1 - c0 < want && left.contains(&c1) {
                c1 += 1;
            }
            let started = Instant::now();
            match self.build_tiles(st, plan, shown, &port, &params, req.channel, c0, c1, mon) {
                Ok(()) => {
                    built_any = true;
                    let n = c1 - c0;
                    progress.moved = progress.moved.saturating_add(n);
                    if limit.is_none() {
                        *inline = inline.saturating_sub(n);
                    }
                    if let Some(a) = st.audio.as_mut() {
                        a.spec.pace.note(n, started.elapsed());
                    }
                    left.retain(|c| *c < c0 || *c >= c1);
                }
                Err(e) if matches!(e.root(), EngineError::Canceled) => break,
                Err(err) => {
                    // ポートに届く申告した解析がまだない（流し解きの前）ときは断られる。ほかは知らせる
                    if !matches!(err.root(), EngineError::Unsupported(_)) {
                        tracing::warn!(%err, "スペクトルのタイルを作れません");
                    }
                    progress.blocked = true;
                    break;
                }
            }
        }
        if built_any
            && port.node.is_none()
            && let Some(a) = st.audio.as_mut()
        {
            // 作った鍵を部分の作り直しの元にする
            if let Some(l) = a.spec.lineage.as_mut().filter(|l| l.cur == key) {
                l.base = key;
                l.dirty = Dirty::Spans(Vec::new());
            }
        }
        // 置き場と照らして決める（作ったあとに追い出されたものは作っていないのと同じ）
        let (ready, pending) = {
            let p = self.lock_published();
            let have: Vec<u64> = wanted
                .iter()
                .copied()
                .filter(|c| p.spec.contains(&(key, code, *c)))
                .collect();
            let pending = have.len() < wanted.len();
            (runs(have), pending)
        };
        Some(SpecPortInfo {
            node: port.node,
            side: req.side,
            channel: req.channel,
            key: key_hex(&key),
            rate: port.rate,
            frames: port.frames,
            n: params.n() as u32,
            hop: params.hop() as u32,
            bands: params.bands().len() as u32,
            map: port.map.clone(),
            max_chunks: cap,
            ready,
            pending,
        })
    }

    /// ポートのチャンク [c0, c1) のタイルを作って公開の置き場に入れ、チャンクの要約を覚える。
    #[allow(clippy::too_many_arguments)]
    fn build_tiles(
        &self,
        st: &mut State,
        plan: &Plan,
        shown: &PartialValue,
        port: &Port,
        params: &SpecParams,
        chan: SpecChannel,
        c0: u64,
        c1: u64,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let (a, b) = params.needed_span(c0, c1, port.frames);
        let planes = self.port_samples(st, plan, Some(shown), port, a, b, mon)?;
        let input = SpecInput {
            start: a,
            len: port.frames,
            planes: &planes,
        };
        let chunks = self
            .runtime
            .preview(|_| params.chunks(&input, chan, c0, c1, mon))?;
        self.keep_chunks(st, params, chan, port.key, &chunks, true);
        Ok(())
    }

    /// ポートの標本の区間 [a, b)（チャンネル × 区間）：文書の出力は [`Self::output_samples`]、ノードのポートは区間で流す
    /// （`Engine::stream_node_port`。ポートに届く申告した解析が手元になければ `Unsupported`。読みは源の LRU に入れない）。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn port_samples(
        &self,
        st: &mut State,
        plan: &Plan,
        shown: Option<&PartialValue>,
        port: &Port,
        a: u64,
        b: u64,
        mon: &dyn Monitor,
    ) -> Result<Vec<Vec<f32>>, EngineError> {
        let (Some(node), Some(port_ref)) = (port.node, port.port_ref) else {
            return self.output_samples(st, plan, shown, a, b, mon);
        };
        let len = usize::try_from(b.saturating_sub(a))
            .map_err(|_| EngineError::Limit("スペクトルの区間が長すぎます".into()))?;
        if len == 0 {
            return Ok(vec![Vec::new(); usize::from(port.channels)]);
        }
        let spec = EstimateSpec::new(0)
            .all_inputs(plan, Source::Held)
            .port_area(port_ref, Area::span(a, b));
        let admitted =
            self.runtime
                .admit_preview(plan, spec, &st.inputs, &mut st.cache, &mut st.lease);
        if let Err(e) = admitted {
            settle(st);
            return Err(EngineError::Limit(format!("スペクトルの区間の確保：{e}")));
        }
        // 受け手の区間は確保のあとに作る（一時の作業領域 `WORK_BYTES` の内）
        let planes = vec![vec![0f32; len]; usize::from(port.channels)];
        let mut sink = Collect { start: a, planes };
        let spans = Spans::one(a, b);
        let side = PortSide::from(port.side);
        let (inputs, cache) = (&st.inputs, &mut st.cache);
        let r = self.runtime.preview(|e| {
            read_through(|| {
                e.stream_node_port(
                    plan,
                    inputs,
                    cache,
                    node,
                    side,
                    Some(&spans),
                    &mut sink,
                    mon,
                )
            })
        });
        settle(st);
        r?;
        Ok(sink.planes)
    }

    /// 作ったチャンクの要約を覚え、`tiles` なら公開の置き場にタイルを入れる。
    pub(super) fn keep_chunks(
        &self,
        st: &mut State,
        params: &SpecParams,
        chan: SpecChannel,
        key: [u8; 16],
        chunks: &[SpecChunk],
        tiles: bool,
    ) {
        let code = chan_code(chan);
        if tiles {
            let mut p = self.lock_published();
            for c in chunks {
                p.spec
                    .put((key, code, c.chunk), Arc::new(tile_bytes(params, chan, c)));
            }
        }
        if let Some(a) = st.audio.as_mut() {
            for c in chunks {
                a.spec
                    .summaries
                    .put((key, code, c.chunk), Arc::new(Summary::of(c)));
            }
        }
    }

    /// 出力の標本の区間 [a, b)（チャンネル × 区間）：`shown`（公開した表示の値）が覆っていればそこから、なければ出力を
    /// 区間で流す（読みは源の LRU に入れない）。
    pub(super) fn output_samples(
        &self,
        st: &mut State,
        plan: &Plan,
        shown: Option<&PartialValue>,
        a: u64,
        b: u64,
        mon: &dyn Monitor,
    ) -> Result<Vec<Vec<f32>>, EngineError> {
        let full = plan
            .output_shapes()
            .into_iter()
            .find(|(o, _)| *o == st.output)
            .map(|(_, s)| s)
            .ok_or_else(|| EngineError::Internal("文書に出力がありません".into()))?;
        let Some((_, channels, _)) = audio_shape(&full) else {
            return Err(EngineError::Internal("出力が音声ではありません".into()));
        };
        let len = usize::try_from(b.saturating_sub(a))
            .map_err(|_| EngineError::Limit("スペクトルの区間が長すぎます".into()))?;
        if len == 0 {
            return Ok(vec![Vec::new(); usize::from(channels)]);
        }
        if let Some(shown) = shown.filter(|v| v.shape() == full && v.covers(&Area::span(a, b))) {
            let mut planes = vec![vec![0f32; len]; usize::from(channels)];
            for (ch, plane) in planes.iter_mut().enumerate() {
                shown.audio_read(ch, a, plane)?;
            }
            return Ok(planes);
        }
        // 出力に届く申告した解析が手元になければ流さない（描画の中の流し解きで、錠の中で入力の全体を流すため。ノードの
        // ポートの `stream_node_port` と同じ守り）
        let missing = self
            .runtime
            .preview(|e| e.missing_solves(plan, &st.inputs, &st.cache, st.output))?;
        if missing.unstreamable || !missing.tasks.is_empty() {
            return Err(EngineError::Unsupported(
                "出力を描くのに要る解析がまだ解けていません".into(),
            ));
        }
        let spec = EstimateSpec::new(0)
            .all_inputs(plan, Source::Held)
            .area(st.output, Area::span(a, b));
        let admitted =
            self.runtime
                .admit_preview(plan, spec, &st.inputs, &mut st.cache, &mut st.lease);
        if let Err(e) = admitted {
            settle(st);
            return Err(EngineError::Limit(format!("スペクトルの区間の確保：{e}")));
        }
        // 受け手の区間は確保のあとに作る（一時の作業領域 `WORK_BYTES` の内）
        let planes = vec![vec![0f32; len]; usize::from(channels)];
        let mut sink = Collect { start: a, planes };
        let mut spans = Spans::new();
        spans.push(a, b);
        let (inputs, cache, output) = (&st.inputs, &mut st.cache, st.output);
        let r = self.runtime.preview(|e| {
            read_through(|| {
                e.stream_audio(plan, inputs, cache, output, Some(&spans), &mut sink, mon)
            })
        });
        settle(st);
        r?;
        Ok(sink.planes)
    }

    /// 公開の置き場のタイル（配信用。なければ None）。
    pub fn published_spec(
        &self,
        key: &[u8; 16],
        chan: SpecChannel,
        chunk: u64,
    ) -> Option<Arc<Vec<u8>>> {
        self.lock_published()
            .spec
            .get(&(*key, chan_code(chan), chunk))
    }
}

/// 配信（`/edit/{s}/spec/{key}/{ch}/{chunk}`）：公開したタイルのバイト列。
pub fn serve_spec(
    engine: &crate::edit::EngineServices,
    session: &str,
    key: &str,
    channel: &str,
    chunk: u64,
) -> Result<Arc<Vec<u8>>, super::super::tile::TileError> {
    use super::super::tile::TileError;
    let editor = engine
        .editor_for(session)
        .ok_or_else(|| TileError::NotFound(format!("エディタ {session}")))?;
    let key = parse_key_hex(key).ok_or_else(|| TileError::NotFound(format!("鍵 {key}")))?;
    let chan = parse_chan_token(channel)
        .ok_or_else(|| TileError::NotFound(format!("チャンネル {channel}")))?;
    editor
        .published_spec(&key, chan, chunk)
        .ok_or_else(|| TileError::NotFound(format!("スペクトルのタイル {chunk}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// チャンネルの綴りと符号・鍵の 16 進は往復する。読めない綴りは断る。
    #[test]
    fn tokens_round_trip() {
        for c in [
            SpecChannel::Index(0),
            SpecChannel::Index(1),
            SpecChannel::Index(17),
            SpecChannel::Mid,
            SpecChannel::Side,
            SpecChannel::Sum,
        ] {
            assert_eq!(parse_chan_token(&chan_token(c)), Some(c));
        }
        for bad in ["", "01", "x", "65533", "-1", "M", "sums"] {
            assert_eq!(parse_chan_token(bad), None, "{bad}");
        }
        let key: [u8; 16] = core::array::from_fn(|i| (i * 17) as u8);
        assert_eq!(parse_key_hex(&key_hex(&key)), Some(key));
        assert_eq!(parse_key_hex("00"), None);
        assert_eq!(parse_key_hex(&"G".repeat(32)), None);
        assert_eq!(parse_key_hex(&"A".repeat(32)), None, "大文字は読まない");
    }

    /// 置き場は上限を超えたら古い順に追い出し、引くと新しくなる。
    #[test]
    fn store_is_an_lru() {
        let mut s = SpecStore::default();
        let tile = |n: usize| Arc::new(vec![0u8; n]);
        let big = (SPEC_STORE_BYTES / 4) as usize;
        for c in 0..4 {
            s.put(([0; 16], 0, c), tile(big));
        }
        assert_eq!(s.bytes(), SPEC_STORE_BYTES);
        assert!(s.get(&([0; 16], 0, 0)).is_some());
        s.put(([0; 16], 0, 9), tile(big));
        assert!(s.contains(&([0; 16], 0, 0)), "引いたものは残る");
        assert!(!s.contains(&([0; 16], 0, 1)), "いちばん古いものを追い出す");
        assert!(s.bytes() <= SPEC_STORE_BYTES);
    }

    /// タイルの並びは共有のベクタ（`tests/vectors/edit/spec_tile.json`）と同じ。
    #[test]
    fn tiles_match_the_shared_vector() {
        let raw = include_str!("../../../../tests/vectors/edit/spec_tile.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let tiles = v["tiles"].as_array().unwrap();
        assert_eq!(tiles.len(), 3);
        for t in tiles {
            let n = |k: &str| t[k].as_u64().unwrap();
            let p = SpecParams::for_rate(n("rate") as u32).unwrap();
            assert_eq!((p.n() as u64, p.hop() as u64), (n("n"), n("hop")));
            assert_eq!(p.bands().len() as u64, n("bands"));
            let chan = parse_chan_token(t["token"].as_str().unwrap()).unwrap();
            assert_eq!(u64::from(chan_code(chan)), n("channel"));
            let frames = n("frames") as usize;
            let chunk = SpecChunk {
                chunk: n("chunk"),
                first_frame: n("firstFrame"),
                frames: frames as u32,
                data: (0..frames * p.bands().len())
                    .map(|i| ((i * 37 + 11) % 256) as u8)
                    .collect(),
                sum: Vec::new(),
                max: Vec::new(),
            };
            assert_eq!(n("firstFrame"), n("chunk") * p.frames_per_chunk());
            assert_eq!(
                crate::library::hex(&tile_bytes(&p, chan, &chunk)),
                t["hex"].as_str().unwrap()
            );
        }
    }

    /// 帯の端は共有のベクタ（`tests/vectors/edit/spec_bands.json`。フロントの帯の表と照らす）と同じ。
    #[test]
    fn bands_match_the_shared_vector() {
        let raw = include_str!("../../../../tests/vectors/edit/spec_bands.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let rates = v["rates"].as_array().unwrap();
        assert_eq!(rates.len(), 3);
        for r in rates {
            let rate = r["rate"].as_u64().unwrap() as u32;
            let p = SpecParams::for_rate(rate).unwrap();
            assert_eq!(p.n() as u64, r["n"].as_u64().unwrap());
            assert_eq!(p.hop() as u64, r["hop"].as_u64().unwrap());
            let want = r["bands"].as_array().unwrap();
            assert_eq!(p.bands().len(), want.len(), "{rate}");
            for (b, w) in p.bands().iter().zip(want) {
                assert_eq!(b.lo.to_bits(), w[0].as_f64().unwrap().to_bits(), "{rate}");
                assert_eq!(b.hi.to_bits(), w[1].as_f64().unwrap().to_bits(), "{rate}");
                assert_eq!(b.long, w[2].as_bool().unwrap(), "{rate}");
            }
        }
    }

    /// 頼んだ区間のチャンクは頼みの順に重ねずに、長さで切る。半開区間にまとめる。
    #[test]
    fn chunks_and_runs() {
        let c = AUDIO_CHUNK as u64;
        assert_eq!(
            wanted_chunks(
                &[(3 * c + 5, 4 * c + 1), (0, 1), (3 * c, 3 * c + 9)],
                10 * c
            ),
            vec![3, 4, 0]
        );
        assert_eq!(wanted_chunks(&[(9 * c, 20 * c)], 10 * c), vec![9]);
        assert_eq!(runs(vec![4, 0, 3, 1, 9]), vec![(0, 2), (3, 5), (9, 10)]);
    }
}
