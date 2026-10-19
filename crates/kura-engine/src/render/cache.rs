//! ノードのキャッシュ（docs/editing.md §12。段 10）。値の指紋・`NodeCache`・`Engine::render_cached`。
//!
//! - 指紋：ノードの鍵（`op::node_key`）・規則・派生した種・段・入力の指紋（順を含む）から作る。外したノードは入力 0 の
//!   指紋をそのまま使う。入力の指紋は値のダイジェスト（読むときに取った `DecodeStamp::value_digest` を使い回す）。
//!   指紋はメモリの中だけで使い、保存しない（版をまたいで保つ約束はない）。
//! - 覚えるのは、等倍で全体を描いた値（由来 `Exact`）と、その警告（操作の警告と写しの警告を分けて）だけ。エラー・中止・
//!   部分の値は入れない。当たりに使うのは `Exact` だけ。当たったら警告を同じ順で再生する。
//! - 申告した解析は (入力の指紋, ID, 版, 正規の引数) の鍵で同じ表に入れる（Solver の trait は変えない）。
//! - 追い出しは使った順の番号（tick）で行う（時刻を使わない）。共有したタイル・チャンネルは実体の番地ごとに 1 度だけ
//!   数える。番地は、表がその実体を持っている間だけ数え、項目を消すのと同じ手で数を減らす（解放した番地の使い回しで
//!   数を誤らない）。
//! - 固定：描く前に「当たる予定のもの」と focus の入力・出力を固定し、描いたあとは focus の入力・出力だけを残す。
//! - 1 つのキャッシュは 1 つのエンジンでだけ使う（エンジンの通し番号が違えば `Internal`）。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{
    Engine, Plan, PlannedNode, bad_input, check_node_shape, lookup, node_inputs, node_seed,
    pass_through, push_warnings,
};
use crate::analysis::{AnyOutput, DynAnalysis, Need};
use crate::codec::DecodeStamp;
use crate::digest::{Digest, Hasher, canonical_json};
use crate::doc::{InputId, NodeId, OutputId, PortRef, Quantize};
use crate::error::{EngineError, Issue};
use crate::monitor::Monitor;
use crate::op::{AnyParams, DynOp};
use crate::value::Value;

/// 値の指紋（キャッシュの鍵）。メモリの中だけで使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fingerprint(pub Digest);

impl Fingerprint {
    /// 値のダイジェスト（§8.6）を指紋にする（テスト・合成の値。大きな値では時間がかかる）。
    pub fn of_value(v: &Value) -> Result<Self, EngineError> {
        Ok(Self(v.digest()?))
    }

    /// 読み取りの記録の値のダイジェストを指紋にする（読むときに必ず取っているので、取り直さない）。
    pub fn of_decoded(stamp: &DecodeStamp) -> Self {
        Self(stamp.value_digest)
    }

    /// 親の指紋から派生した指紋（縮小した段の入力・アンカーから読んだ値など）。`domain` は用途、`args` は用途の引数。
    pub fn derive(domain: &str, parent: Fingerprint, args: &[u8]) -> Self {
        Self(
            Hasher::new("kura-fp-derived-v1")
                .bytes_with_len(domain.as_bytes())
                .raw(&parent.0.0)
                .bytes_with_len(args)
                .finish(),
        )
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 有効なノードの指紋：blake3("kura-fp-node-v1" ‖ ノードの鍵 ‖ 規則 ‖ 派生した種 ‖ 段 ‖ 入力の数 ‖ 入力の指紋（順）)。
/// 派生した種は操作に渡す種（`determinism::seed(ノードの種, 元, 操作 ID)`）。ノード ID は入れない（継ぎ足しでずれる）。
pub fn node_fingerprint(
    key: &Digest,
    rules: u32,
    seed: u64,
    level: u8,
    inputs: &[Fingerprint],
) -> Fingerprint {
    let mut h = Hasher::new("kura-fp-node-v1");
    h.raw(&key.0)
        .u32(rules)
        .u64(seed)
        .u8(level)
        .u32(u32::try_from(inputs.len()).unwrap_or(u32::MAX));
    for i in inputs {
        h.raw(&i.0.0);
    }
    Fingerprint(h.finish())
}

/// 量子化した出力の指紋：blake3("kura-fp-out-v1" ‖ 出力のノードの指紋 ‖ 量子化の版・ビット・ディザ・種 ‖ 元のハッシュ)。
/// 量子化は元のハッシュを直接使う（ディザの乱数）ので元を足す。
pub fn quantized_fingerprint(node: Fingerprint, q: &Quantize, source: &Digest) -> Fingerprint {
    let dither = match q.dither {
        crate::doc::Dither::None => 0,
        crate::doc::Dither::Tpdf => 1,
    };
    Fingerprint(
        Hasher::new("kura-fp-out-v1")
            .raw(&node.0.0)
            .u32(u32::from(q.v))
            .u32(u32::from(q.bits))
            .u8(dither)
            .u64(q.seed)
            .raw(&source.0)
            .finish(),
    )
}

/// 申告した解析の鍵：blake3("kura-fp-analysis-v1" ‖ 入力の指紋 ‖ 解析の ID ‖ 版 ‖ 正規の引数のバイト列)。
pub fn analysis_fingerprint(
    input: Fingerprint,
    id: &str,
    v: u16,
    params: &serde_json::Value,
) -> Result<Fingerprint, EngineError> {
    let bytes = canonical_json(params)?;
    Ok(Fingerprint(
        Hasher::new("kura-fp-analysis-v1")
            .raw(&input.0.0)
            .bytes_with_len(id.as_bytes())
            .u32(u32::from(v))
            .bytes_with_len(&bytes)
            .finish(),
    ))
}

/// 指紋の付いた入力（`render_cached` に渡す）。`source` は元のファイルの中身のハッシュ（乱数の種に使う）。
#[derive(Debug, Clone)]
pub struct FingerprintedInput {
    pub value: Value,
    pub source: Digest,
    pub fingerprint: Fingerprint,
}

impl FingerprintedInput {
    /// 値のダイジェストを指紋にした入力（テスト・合成の値）。
    pub fn new(value: Value, source: Digest) -> Result<Self, EngineError> {
        let fingerprint = Fingerprint::of_value(&value)?;
        Ok(Self {
            value,
            source,
            fingerprint,
        })
    }

    /// 読み取りの記録の値のダイジェストを指紋にした入力（`stamp` はこの値を読んだときの記録であること）。
    pub fn decoded(value: Value, source: Digest, stamp: &DecodeStamp) -> Self {
        Self {
            value,
            source,
            fingerprint: Fingerprint::of_decoded(stamp),
        }
    }
}

/// `render_cached` の選び。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderOptions {
    /// 出力のダイジェストを取る（保存・一括・照合。プレビューでは取らない）。取ったものは出力の指紋ごとに覚える。
    pub digests: bool,
    /// 編集中のノード。その入力と出力を固定する（追い出さない）。
    pub focus: Option<NodeId>,
    /// 当たっても描き直してダイジェストと警告を照らし、違えば `Internal`（指紋の抜けを見つける。テストとファズ用）。
    pub verify_hits: bool,
}

/// 項目の由来。当たり・差分の適用・解析・スポイトに使うのは `Exact` だけ（プレビューの段の計画のノードの値も、指紋の段で
/// 等倍と分かれるので `Exact` で入る。段の値が等倍の指紋に当たることはない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// 等倍で全体を描いた値。
    Exact,
    /// 前の版の値に、変わった範囲だけを描き直して継ぎはぎした値（配信にだけ使う）。
    Patched,
    /// 範囲だけを描いた値（配信にだけ使う）。
    Partial,
    /// プレビューの段（1〜）で描いた値（近似。配信にだけ使う。段 10。render/preview.rs）。キャッシュには入れない（段の計画の
    /// ノードの値は、指紋の段で等倍と分かれるので `Exact` で入る）。
    Scaled,
}

/// キャッシュの数（累計。`CachedRender::stats` は 1 回の描画の分）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// 値が当たったノード。
    pub hits: u64,
    /// 値が外れて描いたノード。
    pub misses: u64,
    /// 入れた項目（値・解析）。
    pub inserted: u64,
    /// 共有を除いた大きさが上限を超えて入れなかった項目。
    pub refused: u64,
    /// 追い出した項目。
    pub evicted: u64,
    /// 当たった解析。
    pub analysis_hits: u64,
    /// 解いた解析。
    pub analysis_solved: u64,
    /// 当たりを描き直して照らした数（`verify_hits`）。
    pub verified: u64,
    /// 覚えていた出力のダイジェストを使った数。
    pub digest_hits: u64,
    /// 外れたノードのうち、前の出力に差分の適用（`Op::apply_appended`）で描いた数。
    pub appended: u64,
    /// 外れた継ぎ目の縮小（プレビューの段）のうち、変わったタイルだけを縮め直した数。
    pub rescaled: u64,
}

impl CacheStats {
    /// `before` からの増え分。
    pub fn since(&self, before: &CacheStats) -> CacheStats {
        CacheStats {
            hits: self.hits.saturating_sub(before.hits),
            misses: self.misses.saturating_sub(before.misses),
            inserted: self.inserted.saturating_sub(before.inserted),
            refused: self.refused.saturating_sub(before.refused),
            evicted: self.evicted.saturating_sub(before.evicted),
            analysis_hits: self.analysis_hits.saturating_sub(before.analysis_hits),
            analysis_solved: self.analysis_solved.saturating_sub(before.analysis_solved),
            verified: self.verified.saturating_sub(before.verified),
            digest_hits: self.digest_hits.saturating_sub(before.digest_hits),
            appended: self.appended.saturating_sub(before.appended),
            rescaled: self.rescaled.saturating_sub(before.rescaled),
        }
    }
}

/// ノード ID ごとの前回の描画（差分の適用で、前の引数と前の出力を引くため。§10）。前の出力の値は `output` の指紋で
/// キャッシュから引く（`Exact` だけ）。
#[derive(Clone)]
pub struct LastRun {
    /// 描いた実装（解決した版）。
    pub op: Arc<dyn DynOp>,
    /// 型付きの引数（計画と共有する）。
    pub params: Arc<AnyParams>,
    /// ノードの鍵。
    pub key: Digest,
    /// 入力の指紋（計画の順）。
    pub inputs: Vec<Fingerprint>,
    /// 文書の規則の版。
    pub rules: u32,
    /// 操作に渡した種（派生した種）。
    pub seed: u64,
    /// 解像度の段。
    pub level: u8,
    /// 出力の指紋。
    pub output: Fingerprint,
    /// 操作の警告（ノードを付ける前）。
    pub apply_warnings: Vec<Issue>,
    /// メタデータを写したときの警告（ノードを付ける前）。
    pub map_warnings: Vec<Issue>,
}

/// 項目の中身。
enum Stored {
    Value(Value),
    Analysis(AnyOutput),
}

struct Entry {
    stored: Stored,
    origin: Origin,
    apply_warnings: Vec<Issue>,
    map_warnings: Vec<Issue>,
    /// 出力のダイジェスト（取ったときだけ）。
    digest: Option<Digest>,
    tick: u64,
    /// この項目が数えている実体の番地（重なりなし）。
    chunks: Vec<usize>,
    /// 実体の外で数えるバイト数（解析の結果）。
    own: u64,
}

/// 当たった値と警告。
struct Hit {
    value: Value,
    apply_warnings: Vec<Issue>,
    map_warnings: Vec<Issue>,
}

/// ノードのキャッシュ。エディタのセッションごとに 1 つ（`Send`。同時に 2 つの描画で使わない）。
pub struct NodeCache {
    engine: Option<u64>,
    entries: BTreeMap<Fingerprint, Entry>,
    /// 使った順の番号 → 指紋（小さいほど古い）。
    lru: BTreeMap<u64, Fingerprint>,
    tick: u64,
    /// 実体の番地 → (数えている項目・外の値の数, バイト数)。
    chunks: BTreeMap<usize, (u32, u64)>,
    /// 呼び手が持つ値（エディタの入力・配信のために持つ全体の値など）。数えるが追い出さない。登録した順。
    externals: Vec<(Value, Vec<usize>)>,
    bytes: u64,
    capacity: u64,
    pins: BTreeSet<Fingerprint>,
    pub(super) stats: CacheStats,
    last: BTreeMap<NodeId, LastRun>,
    /// 前回の `render_cached`（変わった範囲を求めるため。段 10。delta.rs）。数えない。
    prev: Option<super::delta::PrevRender>,
}

impl NodeCache {
    /// 上限 `capacity` バイトの空のキャッシュ（0 なら何も覚えない）。
    pub fn new(capacity: u64) -> Self {
        Self {
            engine: None,
            entries: BTreeMap::new(),
            lru: BTreeMap::new(),
            tick: 0,
            chunks: BTreeMap::new(),
            externals: Vec::new(),
            bytes: 0,
            capacity,
            pins: BTreeSet::new(),
            stats: CacheStats::default(),
            last: BTreeMap::new(),
            prev: None,
        }
    }

    /// 上限（バイト）。
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// 上限を変える（下げたら、固定していないものを古い順に追い出す）。
    pub fn set_capacity(&mut self, capacity: u64) {
        self.capacity = capacity;
        self.evict_to(capacity);
    }

    /// 数えているバイト数（共有した実体は 1 度だけ。外の値を含む）。
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// 固定した項目のバイト数（固定したものどうしの共有は 1 度だけ）。
    pub fn pinned_bytes(&self) -> u64 {
        let mut seen = BTreeSet::new();
        let mut total = 0u64;
        for e in self.pins.iter().filter_map(|fp| self.entries.get(fp)) {
            total = total.saturating_add(e.own);
            for a in &e.chunks {
                if seen.insert(*a) {
                    total = total.saturating_add(self.chunks.get(a).map_or(0, |c| c.1));
                }
            }
        }
        total
    }

    /// 固定していないものを古い順に追い出して、`target` バイト以下にする（固定と外の値は残る）。残ったバイト数を返す。
    pub fn shrink_to(&mut self, target: u64) -> u64 {
        self.evict_to(target);
        self.bytes
    }

    /// 項目（値・解析）の数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 項目がなければ true。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 等倍で全体を描いた値（当たりに使えるもの）を持っていれば true。
    pub fn contains(&self, fp: &Fingerprint) -> bool {
        self.entries
            .get(fp)
            .is_some_and(|e| e.origin == Origin::Exact && matches!(e.stored, Stored::Value(_)))
    }

    /// 固定している指紋。
    pub fn pins(&self) -> &BTreeSet<Fingerprint> {
        &self.pins
    }

    /// 累計の数。
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// ノードの前回の描画（差分の適用に使う）。
    pub fn last(&self, id: NodeId) -> Option<&LastRun> {
        self.last.get(&id)
    }

    /// 呼び手が持つ値（エディタの入力・配信のために持つ全体の値など）を数えに入れる（追い出さない。それと共有する項目は、
    /// 共有分を数え直さない。表示の値 `View` からは、由来 Exact のときのほかは `&Value` を取れない）。写しを持ち、
    /// [`NodeCache::unregister_external`] か [`NodeCache::clear_externals`] で外すまで数える。足したバイト数を返す。
    pub fn register_external(&mut self, v: &Value) -> u64 {
        let chunks = unique_chunks(v);
        let added = self.add_chunks(&chunks);
        self.externals
            .push((v.clone(), chunks.into_iter().map(|(a, _)| a).collect()));
        self.evict_to(self.capacity);
        added
    }

    /// 外の値を 1 つ外す（入力や配信のために持つ値を入れ替えるとき）。`v` と同じ実体（タイル・チャンネルの番地がすべて
    /// 同じ）の外の値のうち、最後に登録したものを外して写しを手放し、数えなくなったバイト数（ほかの外の値・項目と共有する分は
    /// 残るので含まない）を返す。登録していなければ `None`。
    pub fn unregister_external(&mut self, v: &Value) -> Option<u64> {
        let chunks: Vec<usize> = unique_chunks(v).into_iter().map(|(a, _)| a).collect();
        let at = self.externals.iter().rposition(|(_, c)| *c == chunks)?;
        let (value, held) = self.externals.remove(at);
        let before = self.bytes;
        // 数を減らしてから写しを落とす（番地は写しを落とすまで使い回されない）
        self.release_chunks(&held);
        drop(value);
        Some(before.saturating_sub(self.bytes))
    }

    /// 外の値をすべて外す。
    pub fn clear_externals(&mut self) {
        for (_, chunks) in std::mem::take(&mut self.externals) {
            self.release_chunks(&chunks);
        }
    }

    /// すべてを捨てる（エンジンへの縛りも解く）。
    pub fn clear(&mut self) {
        let keep = self.capacity;
        let stats = self.stats;
        *self = Self::new(keep);
        self.stats = stats;
    }

    /// テスト用：指紋に値を直接入れる（`verify_hits` が指紋の抜けを見つけるか・`Exact` 以外を当たりに使わないかの確かめ）。
    /// 上限に収まらなければ入れない（false）。
    #[doc(hidden)]
    pub fn insert_raw(&mut self, fp: Fingerprint, value: Value, origin: Origin) -> bool {
        self.insert(fp, Stored::Value(value), origin, Vec::new(), Vec::new(), 0)
    }

    /// エンジンに縛る（初めて使ったエンジン以外は `Internal`）。
    fn bind(&mut self, token: u64) -> Result<(), EngineError> {
        match self.engine {
            None => {
                self.engine = Some(token);
                Ok(())
            }
            Some(t) if t == token => Ok(()),
            Some(_) => Err(EngineError::Internal(
                "このキャッシュは別のエンジンで使ったものです（登録表・上限の違う値を混ぜない）"
                    .into(),
            )),
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// 項目を使ったことにする（使った順の番号を新しくする）。
    fn touch(&mut self, fp: &Fingerprint) {
        let tick = self.next_tick();
        if let Some(e) = self.entries.get_mut(fp) {
            self.lru.remove(&e.tick);
            e.tick = tick;
            self.lru.insert(tick, *fp);
        }
    }

    /// 等倍で全体を描いた値を引く（使った順を新しくする）。
    fn get_value(&mut self, fp: &Fingerprint) -> Option<Hit> {
        let hit = match self.entries.get(fp) {
            Some(Entry {
                stored: Stored::Value(v),
                origin: Origin::Exact,
                apply_warnings,
                map_warnings,
                ..
            }) => Hit {
                value: v.clone(),
                apply_warnings: apply_warnings.clone(),
                map_warnings: map_warnings.clone(),
            },
            _ => return None,
        };
        self.touch(fp);
        Some(hit)
    }

    /// 等倍で全体を描いた値（差分の適用の前の出力）。使った順は変えない（差分の適用を使えなかったときに、追い出しの順を
    /// 変えないため。使えたら新しい値が入る）。
    pub(super) fn exact_value(&self, fp: &Fingerprint) -> Option<Value> {
        match self.entries.get(fp) {
            Some(Entry {
                stored: Stored::Value(v),
                origin: Origin::Exact,
                ..
            }) => Some(v.clone()),
            _ => None,
        }
    }

    /// 解析の結果を引く。
    fn get_analysis(&mut self, fp: &Fingerprint) -> Option<AnyOutput> {
        let out = match self.entries.get(fp) {
            Some(Entry {
                stored: Stored::Analysis(a),
                ..
            }) => a.clone(),
            _ => return None,
        };
        self.touch(fp);
        Some(out)
    }

    /// 覚えた出力のダイジェスト。
    fn digest_of(&self, fp: &Fingerprint) -> Option<Digest> {
        self.entries.get(fp).and_then(|e| e.digest)
    }

    /// 出力のダイジェストを覚える（項目があるときだけ）。
    fn set_digest(&mut self, fp: &Fingerprint, d: Digest) {
        if let Some(e) = self.entries.get_mut(fp) {
            e.digest = Some(d);
        }
    }

    /// 実体を数えに足す（すでに数えている番地は数だけ増やす）。足したバイト数を返す。
    fn add_chunks(&mut self, chunks: &[(usize, u64)]) -> u64 {
        let mut added = 0u64;
        for (a, b) in chunks {
            let c = self.chunks.entry(*a).or_insert((0, *b));
            if c.0 == 0 {
                added = added.saturating_add(*b);
            }
            c.0 = c.0.saturating_add(1);
        }
        self.bytes = self.bytes.saturating_add(added);
        added
    }

    /// 実体の数を減らし、どこからも数えなくなった番地を消す。
    fn release_chunks(&mut self, chunks: &[usize]) {
        for a in chunks {
            if let Some(c) = self.chunks.get_mut(a) {
                c.0 = c.0.saturating_sub(1);
                if c.0 == 0 {
                    let b = c.1;
                    self.chunks.remove(a);
                    self.bytes = self.bytes.saturating_sub(b);
                }
            }
        }
    }

    /// 項目を入れる（同じ指紋があれば入れ替える）。共有を除いた大きさが上限を超えるものは入れない。入れたあと、上限まで
    /// 固定していないものを古い順に追い出す（入れたものがいちばん新しい）。残っていれば true。
    fn insert(
        &mut self,
        fp: Fingerprint,
        stored: Stored,
        origin: Origin,
        apply_warnings: Vec<Issue>,
        map_warnings: Vec<Issue>,
        own: u64,
    ) -> bool {
        // 範囲の描画で作った値（無効なタイルのある値）は入れない（当たりに使えないため）
        if let Stored::Value(v) = &stored
            && !v.is_complete()
        {
            self.stats.refused += 1;
            return false;
        }
        self.remove(&fp);
        let chunks = match &stored {
            Stored::Value(v) => unique_chunks(v),
            Stored::Analysis(_) => Vec::new(),
        };
        let new_bytes = chunks
            .iter()
            .filter(|(a, _)| !self.chunks.contains_key(a))
            .fold(own, |acc, (_, b)| acc.saturating_add(*b));
        if new_bytes > self.capacity {
            self.stats.refused += 1;
            return false;
        }
        self.add_chunks(&chunks);
        self.bytes = self.bytes.saturating_add(own);
        let tick = self.next_tick();
        self.lru.insert(tick, fp);
        self.entries.insert(
            fp,
            Entry {
                stored,
                origin,
                apply_warnings,
                map_warnings,
                digest: None,
                tick,
                chunks: chunks.into_iter().map(|(a, _)| a).collect(),
                own,
            },
        );
        self.stats.inserted += 1;
        self.evict_to(self.capacity);
        self.entries.contains_key(&fp)
    }

    /// 項目を消す（数を減らしてから値を落とす。番地は値を落とすまで使い回されない）。
    fn remove(&mut self, fp: &Fingerprint) -> bool {
        let Some(entry) = self.entries.remove(fp) else {
            return false;
        };
        self.lru.remove(&entry.tick);
        self.release_chunks(&entry.chunks);
        self.bytes = self.bytes.saturating_sub(entry.own);
        drop(entry);
        true
    }

    /// 固定していないものを古い順に追い出して、`limit` バイト以下にする。
    fn evict_to(&mut self, limit: u64) {
        while self.bytes > limit {
            let victim = self
                .lru
                .values()
                .find(|fp| !self.pins.contains(fp))
                .copied();
            let Some(fp) = victim else { break };
            self.remove(&fp);
            self.stats.evicted += 1;
        }
    }

    // 段 10：範囲の描画（area.rs）と変わった範囲（delta.rs）が使う口

    /// エンジンに縛る（`bind`）。
    pub(crate) fn bind_engine(&mut self, token: u64) -> Result<(), EngineError> {
        self.bind(token)
    }

    /// 等倍で全体を描いた値を引く（値・操作の警告・写しの警告）。当たりの数を数える。
    pub(crate) fn hit(&mut self, fp: &Fingerprint) -> Option<(Value, Vec<Issue>, Vec<Issue>)> {
        let hit = self.get_value(fp)?;
        self.stats.hits += 1;
        Some((hit.value, hit.apply_warnings, hit.map_warnings))
    }

    /// 前回の `render_cached` を取り出す。
    pub(crate) fn take_prev(&mut self) -> Option<super::delta::PrevRender> {
        self.prev.take()
    }

    /// 今回の `render_cached` を前回として覚える。
    pub(crate) fn set_prev(&mut self, prev: super::delta::PrevRender) {
        self.prev = Some(prev);
    }

    /// 等倍で全体を描いた（か当たった）ノードの、今回の描画を前回として覚える（次の差分の適用・継ぎ目の縮小の差分に使う）。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_last(
        &mut self,
        plan: &Plan,
        node: &PlannedNode,
        in_fps: Vec<Fingerprint>,
        source: &Digest,
        output: Fingerprint,
        apply_warnings: &[Issue],
        map_warnings: &[Issue],
    ) {
        self.last.insert(
            node.id,
            LastRun {
                op: node.op.clone(),
                params: node.params.clone(),
                key: node.key,
                inputs: in_fps,
                rules: plan.rules,
                seed: node_seed(node, source),
                level: node.level,
                output,
                apply_warnings: apply_warnings.to_vec(),
                map_warnings: map_warnings.to_vec(),
            },
        );
    }

    /// 描く前の固定：描いている途中の追い出しで当たりが外れないように、当たる予定のもの（等倍で全体を描いた値を持つノード・
    /// 出力）と focus の入力・出力を固定する。描いたあとに残す固定（focus の分）を返す。
    pub(super) fn pin_before(
        &mut self,
        plan: &Plan,
        fps: &Fingerprints,
        input_fps: &BTreeMap<InputId, Fingerprint>,
        focus: Option<NodeId>,
    ) -> BTreeSet<Fingerprint> {
        let focus = focus_pins(plan, fps, input_fps, focus);
        let mut pins = focus.clone();
        for node in plan.nodes.iter().filter(|n| n.enabled) {
            if let Some(fp) = fps.nodes.get(&node.id)
                && self.contains(fp)
            {
                pins.insert(*fp);
            }
        }
        for (_, fp) in &fps.outputs {
            if self.contains(fp) {
                pins.insert(*fp);
            }
        }
        self.pins = pins;
        focus
    }

    /// 描いたあと：focus の入力と出力だけを固定し、上限まで追い出し、今の計画にないノードの前回の描画を捨てる。
    pub(super) fn unpin_after(&mut self, plan: &Plan, focus: BTreeSet<Fingerprint>) {
        self.pins = focus;
        self.evict_to(self.capacity);
        let live: BTreeSet<NodeId> = plan.nodes.iter().map(|n| n.id).collect();
        self.last.retain(|id, _| live.contains(id));
    }
}

/// 値の実体の（番地, バイト数）。同じ番地は 1 つにする。
fn unique_chunks(v: &Value) -> Vec<(usize, u64)> {
    let mut list = Vec::new();
    v.chunks(&mut list);
    let unique: BTreeMap<usize, u64> = list.into_iter().collect();
    unique.into_iter().collect()
}

/// ノードと出力の指紋。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprints {
    /// ノードの出力の指紋（外したノードは入力 0 の指紋）。
    pub nodes: BTreeMap<NodeId, Fingerprint>,
    /// 出力の指紋（量子化した出力は量子化の指紋）。
    pub outputs: Vec<(OutputId, Fingerprint)>,
}

/// `render_cached` の出力 1 つ。
#[derive(Debug, Clone)]
pub struct CachedOutput {
    pub id: OutputId,
    pub value: Value,
    /// 出力の指紋。
    pub fingerprint: Fingerprint,
    /// ダイジェスト（`RenderOptions::digests` のときだけ）。
    pub digest: Option<Digest>,
}

/// `render_cached` の結果。値・警告は `Engine::render` と同じ。
#[derive(Debug, Clone)]
pub struct CachedRender {
    pub outputs: Vec<CachedOutput>,
    pub warnings: Vec<Issue>,
    /// この描画の数（当たり・外れ・解析など）。
    pub stats: CacheStats,
    /// 前回の `render_cached`（同じキャッシュ）から変わった範囲（段 10。前回がなければ全体）。
    pub changed: super::delta::Changed,
}

impl Engine {
    /// ノードと出力の指紋（描かない）。入力の指紋は `inputs` のもの。外したノードは入力 0 の指紋。
    pub fn fingerprints(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
    ) -> Result<Fingerprints, EngineError> {
        let source = primary_source(plan, inputs);
        let input_fps: BTreeMap<InputId, Fingerprint> =
            inputs.iter().map(|(k, v)| (*k, v.fingerprint)).collect();
        let mut nodes: BTreeMap<NodeId, Fingerprint> = BTreeMap::new();
        for node in &plan.nodes {
            let ins = node_input_fingerprints(node, &nodes, &input_fps)?;
            let fp = if node.enabled && node.op.id() == super::preview::DOWNSCALE_ID {
                // プレビューの段の継ぎ目の縮小：段の入力の派生の指紋
                let seam = ins.first().ok_or_else(|| {
                    EngineError::Internal("継ぎ目の縮小に入力がありません".into())
                })?;
                super::preview::level_fingerprint(*seam, node.level, plan.rules)
            } else if node.enabled {
                node_fingerprint(
                    &node.key,
                    plan.rules,
                    node_seed(node, &source),
                    node.level,
                    &ins,
                )
            } else {
                *ins.first()
                    .ok_or_else(|| EngineError::Internal("外したノードに入力がありません".into()))?
            };
            nodes.insert(node.id, fp);
        }
        let outputs = plan
            .outputs
            .iter()
            .map(|(id, from, _, q)| {
                let fp = lookup(&nodes, &input_fps, from)
                    .ok_or_else(|| bad_input("出力の入力がありません"))?;
                Ok((
                    *id,
                    match q {
                        Some(q) => quantized_fingerprint(fp, q, &source),
                        None => fp,
                    },
                ))
            })
            .collect::<Result<Vec<_>, EngineError>>()?;
        Ok(Fingerprints { nodes, outputs })
    }

    /// キャッシュを引きながら描く（エディタの道。値・警告は `render` と同じ）。当たったノードは値と警告を再生し、外れたノードは
    /// 描いて入れる。申告した解析も (入力の指紋, ID, 版, 引数) で引く。エラー・中止は入れない（中止までに描き終えたノードの
    /// 値は残る）。解析は既定の解き方（`Solve`）で解く。別のエンジンのキャッシュ・計画は `Internal`。
    pub fn render_cached(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        opts: &RenderOptions,
        mon: &dyn Monitor,
    ) -> Result<CachedRender, EngineError> {
        if plan.engine != self.token {
            return Err(EngineError::Internal(
                "別のエンジンで作った計画です（キャッシュの指紋が登録表に縛られないため）".into(),
            ));
        }
        if plan.level != 0 && opts.digests {
            return Err(EngineError::Unsupported(
                "縮小の段の値のダイジェストは取りません（段の値は保存・照合に使いません）".into(),
            ));
        }
        cache.bind(self.token)?;
        if inputs.len() != plan.inputs.len() {
            return Err(bad_input("入力の数が計画と違います"));
        }
        self.check_inputs(plan, |id| inputs.get(&id).map(|v| &v.value))?;
        let fps = self.fingerprints(plan, inputs)?;
        let input_fps: BTreeMap<InputId, Fingerprint> =
            inputs.iter().map(|(k, v)| (*k, v.fingerprint)).collect();
        let focus = cache.pin_before(plan, &fps, &input_fps, opts.focus);
        let before = cache.stats;
        let result = self.render_cached_inner(plan, inputs, &fps, &input_fps, cache, opts, mon);
        cache.unpin_after(plan, focus);
        let (outputs, warnings) = result?;
        let changed = super::delta::track(plan, inputs, cache, &outputs);
        Ok(CachedRender {
            outputs,
            warnings,
            stats: cache.stats.since(&before),
            changed,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_cached_inner(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        fps: &Fingerprints,
        input_fps: &BTreeMap<InputId, Fingerprint>,
        cache: &mut NodeCache,
        opts: &RenderOptions,
        mon: &dyn Monitor,
    ) -> Result<(Vec<CachedOutput>, Vec<Issue>), EngineError> {
        let source = primary_source(plan, inputs);
        let input_values: BTreeMap<InputId, Value> =
            inputs.iter().map(|(k, v)| (*k, v.value.clone())).collect();
        let mut values: BTreeMap<NodeId, Value> = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut progress = super::Progress::start(plan, mon);
        for (i, node) in plan.nodes.iter().enumerate() {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let fp = *fps
                .nodes
                .get(&node.id)
                .ok_or_else(|| EngineError::Internal("ノードの指紋がありません".into()))?;
            let ins = node_inputs(node, &values, &input_values)?;
            let (out, apply_warnings, map_warnings) = if !node.enabled {
                (pass_through(&ins)?, Vec::new(), Vec::new())
            } else {
                let in_fps = node_input_fingerprints(node, &fps.nodes, input_fps)?;
                let (value, apply_warnings, map_warnings) = match cache.get_value(&fp) {
                    Some(hit) => {
                        cache.stats.hits += 1;
                        if opts.verify_hits {
                            self.verify_hit(plan, node, &ins, &source, mon, &hit, fp)?;
                            cache.stats.verified += 1;
                        }
                        (hit.value, hit.apply_warnings, hit.map_warnings)
                    }
                    None => {
                        let run = self
                            .draw_miss(plan, node, &ins, &in_fps, &source, fp, cache, opts, mon)?;
                        (run.value, run.apply_warnings, run.map_warnings)
                    }
                };
                cache.record_last(
                    plan,
                    node,
                    in_fps,
                    &source,
                    fp,
                    &apply_warnings,
                    &map_warnings,
                );
                (value, apply_warnings, map_warnings)
            };
            check_node_shape(node, &out)?;
            let mut all = apply_warnings;
            all.extend(map_warnings);
            push_warnings(&mut warnings, node.id, all);
            values.insert(node.id, out);
            for id in plan.released_after(i) {
                values.remove(id);
            }
            progress.node_done(node);
        }
        let mut outputs = Vec::with_capacity(plan.outputs.len());
        for ((id, from, _, quantize), (_, out_fp)) in plan.outputs.iter().zip(&fps.outputs) {
            let mut value = lookup(&values, &input_values, from)
                .ok_or_else(|| EngineError::Internal("出力の値がありません".into()))?;
            if let (Some(q), Value::Audio(a)) = (quantize, &value) {
                value = quantized_output(cache, a, q, *out_fp, &source, opts.verify_hits, mon)?;
            }
            let digest = if opts.digests {
                Some(match cache.digest_of(out_fp) {
                    Some(d) => {
                        cache.stats.digest_hits += 1;
                        if opts.verify_hits && value.digest()? != d {
                            return Err(EngineError::Internal(format!(
                                "覚えた出力のダイジェストが値と違います（指紋 {out_fp}）"
                            )));
                        }
                        d
                    }
                    None => {
                        let d = value.digest()?;
                        cache.set_digest(out_fp, d);
                        d
                    }
                })
            } else {
                None
            };
            outputs.push(CachedOutput {
                id: *id,
                value,
                fingerprint: *out_fp,
                digest,
            });
        }
        Ok((outputs, warnings))
    }

    /// 外れたノードを描いて、由来 Exact で入れる（`render_cached` と `render_area` の外れの道。外れの数を数える）。継ぎ目の縮小は
    /// 変わったタイルだけ縮め直し（preview.rs）、前回の出力に差分の適用で描けるなら描き（incremental.rs）、描けなければ全体で
    /// 描く（申告した解析はキャッシュから引く）。`ins` は等倍で全体を描いた値（指紋 `in_fps` の値）であること。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_miss(
        &self,
        plan: &Plan,
        node: &PlannedNode,
        ins: &[Value],
        in_fps: &[Fingerprint],
        source: &Digest,
        fp: Fingerprint,
        cache: &mut NodeCache,
        opts: &RenderOptions,
        mon: &dyn Monitor,
    ) -> Result<super::NodeRun, EngineError> {
        cache.stats.misses += 1;
        let appended = if node.op.id() == super::preview::DOWNSCALE_ID {
            let run = self.render_rescaled(plan, node, ins, cache, opts.verify_hits, mon)?;
            if run.is_some() {
                cache.stats.rescaled += 1;
            }
            run
        } else {
            self.render_appended(plan, node, ins, in_fps, source, cache, opts, mon)?
        };
        let run = match appended {
            Some(run) => run,
            None => self.run_node(plan, node, ins, source, mon, &mut |need, v, a, acx| {
                solve_cached(cache, in_fps, need, v, a, acx, opts.verify_hits)
            })?,
        };
        cache.insert(
            fp,
            Stored::Value(run.value.clone()),
            Origin::Exact,
            run.apply_warnings.clone(),
            run.map_warnings.clone(),
            0,
        );
        Ok(run)
    }

    /// 当たった値を描き直して照らす（`verify_hits`）。解析も解き直す（キャッシュを使わない）。
    #[allow(clippy::too_many_arguments)]
    fn verify_hit(
        &self,
        plan: &Plan,
        node: &PlannedNode,
        ins: &[Value],
        source: &Digest,
        mon: &dyn Monitor,
        hit: &Hit,
        fp: Fingerprint,
    ) -> Result<(), EngineError> {
        let fresh = self.run_node(plan, node, ins, source, mon, &mut |need, v, a, acx| {
            a.run_any(&need.params, v, acx)
        })?;
        if fresh.value.digest()? != hit.value.digest()?
            || fresh.apply_warnings != hit.apply_warnings
            || fresh.map_warnings != hit.map_warnings
        {
            return Err(EngineError::Internal(format!(
                "{} のキャッシュの値が描き直したものと違います（指紋 {fp} の抜け）",
                node.op.id()
            ))
            .at(node.id));
        }
        Ok(())
    }
}

/// 量子化した出力（`render_cached` と `render_area` の同じ道）：量子化の指紋 `out_fp` でキャッシュを引き、なければ等倍で全体を
/// 描いた音声 `a` を量子化して由来 Exact で入れる。`verify` なら当たっても量子化し直して照らす。
pub(super) fn quantized_output(
    cache: &mut NodeCache,
    a: &crate::value::Audio,
    q: &Quantize,
    out_fp: Fingerprint,
    source: &Digest,
    verify: bool,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    if let Some(hit) = cache.get_value(&out_fp) {
        if verify {
            let fresh = crate::audio::quantize(a, q, source, mon)?;
            if fresh.digest()? != hit.value.digest()? {
                return Err(EngineError::Internal(format!(
                    "キャッシュの量子化した出力が描き直したものと違います（指紋 {out_fp}）"
                )));
            }
            cache.stats.verified += 1;
        }
        return Ok(hit.value);
    }
    let v = crate::audio::quantize(a, q, source, mon)?;
    cache.insert(
        out_fp,
        Stored::Value(v.clone()),
        Origin::Exact,
        Vec::new(),
        Vec::new(),
        0,
    );
    Ok(v)
}

/// 申告した解析をキャッシュから引き、なければ既定の解き方で解いて入れる。`verify` なら当たっても解き直して照らす。
fn solve_cached(
    cache: &mut NodeCache,
    in_fps: &[Fingerprint],
    need: &Need,
    v: &Value,
    a: &dyn DynAnalysis,
    acx: &crate::analysis::AnalyzeCx<'_>,
    verify: bool,
) -> Result<AnyOutput, EngineError> {
    let input = in_fps
        .get(need.input)
        .ok_or_else(|| EngineError::Internal("解析する入力の指紋がありません".into()))?;
    let key = analysis_fingerprint(*input, need.id, need.v, &need.params)?;
    if let Some(out) = cache.get_analysis(&key) {
        cache.stats.analysis_hits += 1;
        if verify {
            let fresh = a.run_any(&need.params, v, acx)?;
            if a.encode_any(&fresh)? != a.encode_any(&out)? {
                return Err(EngineError::Internal(format!(
                    "キャッシュの解析 {} v{} が解き直したものと違います（指紋 {key}）",
                    need.id, need.v
                )));
            }
            cache.stats.verified += 1;
        }
        return Ok(out);
    }
    let out = a.run_any(&need.params, v, acx)?;
    cache.stats.analysis_solved += 1;
    // 大きさは解析の見積もり（正規の形の上の見積もり）で数える
    let bytes = a.estimate(&need.params, &v.shape()).map_or(0, |e| e.bytes);
    cache.insert(
        key,
        Stored::Analysis(out.clone()),
        Origin::Exact,
        Vec::new(),
        Vec::new(),
        bytes,
    );
    Ok(out)
}

/// 主の入力の元のハッシュ（なければ 0）。
fn primary_source(plan: &Plan, inputs: &BTreeMap<InputId, FingerprintedInput>) -> Digest {
    plan.primary
        .and_then(|id| inputs.get(&id))
        .map(|v| v.source)
        .unwrap_or_default()
}

/// ノードの入力の指紋（計画の順）。
pub(super) fn node_input_fingerprints(
    node: &PlannedNode,
    nodes: &BTreeMap<NodeId, Fingerprint>,
    inputs: &BTreeMap<InputId, Fingerprint>,
) -> Result<Vec<Fingerprint>, EngineError> {
    node.inputs
        .iter()
        .map(|r| lookup(nodes, inputs, r))
        .collect::<Option<_>>()
        .ok_or_else(|| bad_input("ノードの入力の指紋がありません"))
}

/// focus のノードの入力と出力の指紋。
fn focus_pins(
    plan: &Plan,
    fps: &Fingerprints,
    inputs: &BTreeMap<InputId, Fingerprint>,
    focus: Option<NodeId>,
) -> BTreeSet<Fingerprint> {
    let mut pins = BTreeSet::new();
    let Some(node) = focus.and_then(|id| plan.nodes.iter().find(|n| n.id == id)) else {
        return pins;
    };
    for r in &node.inputs {
        let fp = match r {
            PortRef::Input { id } => inputs.get(id),
            PortRef::Node { id } => fps.nodes.get(id),
        };
        pins.extend(fp.copied());
    }
    pins.extend(fps.nodes.get(&node.id).copied());
    pins
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{AudioMeta, Image, ImageMeta, PixelFormat, Raster};

    fn image(w: u32, h: u32, v: u8) -> Value {
        Value::Image(Arc::new(Image {
            px: Raster::filled(w, h, PixelFormat::Gray8, None, &[v]).unwrap(),
            meta: ImageMeta::default(),
        }))
    }

    fn fp(n: u8) -> Fingerprint {
        Fingerprint(Digest([n; 32]))
    }

    /// 同じ実体は 1 度だけ数え、項目を消したときに同じ手で減らす。上限を超えたら古い順に、固定を除いて追い出す。
    #[test]
    fn counts_shared_chunks_once_and_evicts_oldest_unpinned() {
        let a = image(512, 512, 1); // 4 タイル × 64 KiB
        let tile = 256 * 256;
        let mut c = NodeCache::new(10 * tile);
        for i in 0..100 {
            assert!(c.insert_raw(fp(i), a.clone(), Origin::Exact));
        }
        assert_eq!(c.bytes(), 4 * tile);
        assert_eq!(c.len(), 100);
        // 1 タイルだけ違う値は 1 タイル分だけ増える
        let Value::Image(img) = &a else { panic!() };
        let mut px = img.px.clone();
        px.tile_mut(1, 1)[0] = 9;
        let b = Value::Image(Arc::new(Image {
            px,
            meta: ImageMeta::default(),
        }));
        assert!(c.insert_raw(fp(200), b.clone(), Origin::Exact));
        assert_eq!(c.bytes(), 5 * tile);
        // 全部消すと 0
        for i in 0..100 {
            assert!(c.remove(&fp(i)));
        }
        assert_eq!(c.bytes(), 4 * tile);
        assert!(c.remove(&fp(200)));
        assert_eq!(c.bytes(), 0);
        assert!(c.chunks.is_empty());
        // 上限：別々の値 3 つ（各 4 タイル）で 10 タイルの上限 → 古い 1 つを追い出す。固定は残る
        let vals: Vec<Value> = (0..3).map(|i| image(512, 512, 10 + i)).collect();
        c.pins.insert(fp(1));
        for (i, v) in vals.iter().enumerate() {
            c.insert_raw(fp(i as u8 + 1), v.clone(), Origin::Exact);
        }
        assert!(c.contains(&fp(1)) && !c.contains(&fp(2)) && c.contains(&fp(3)));
        assert_eq!(c.bytes(), 8 * tile);
        assert_eq!(c.pinned_bytes(), 4 * tile);
        // 上限より大きいものは入れない
        assert!(!c.insert_raw(fp(9), image(1024, 1024, 3), Origin::Exact));
        assert_eq!(c.stats().refused, 1);
        // 外の値は追い出さず、共有分は数え直さない
        let mut c = NodeCache::new(5 * tile);
        assert_eq!(c.register_external(&a), 4 * tile);
        assert!(c.insert_raw(fp(1), b, Origin::Exact));
        assert_eq!(c.bytes(), 5 * tile);
        c.clear_externals();
        assert_eq!(c.bytes(), 4 * tile);
        // 上限 0 は何も覚えない
        let mut z = NodeCache::new(0);
        assert!(!z.insert_raw(fp(1), a, Origin::Exact));
        assert!(z.is_empty() && z.bytes() == 0);
    }

    /// 外の値は 1 つずつ外せる（配信のために持つ表示の値を、手ごとに入れ替えて数える）。外すと、ほかの外の値・項目と共有して
    /// いない実体の分だけ減り、写しを手放す。登録していない値は None。同じ値を 2 度登録したら、2 度外すまで数える。
    #[test]
    fn externals_can_be_unregistered_one_by_one() {
        let tile = 256 * 256;
        let a = image(512, 512, 1); // 4 タイル
        let Value::Image(img) = &a else { panic!() };
        let mut px = img.px.clone();
        px.tile_mut(1, 1)[0] = 9;
        let b = Value::Image(Arc::new(Image {
            px,
            meta: ImageMeta::default(),
        })); // a と 3 タイルを共有
        let c = image(256, 256, 7); // 1 タイル
        let mut cache = NodeCache::new(10 * tile);
        assert_eq!(cache.register_external(&a), 4 * tile);
        assert_eq!(cache.register_external(&b), tile);
        assert!(cache.insert_raw(fp(1), c.clone(), Origin::Exact));
        assert_eq!(cache.bytes(), 6 * tile);
        let strong = |v: &Value| match v {
            Value::Image(i) => Arc::strong_count(i),
            _ => 0,
        };
        assert_eq!(strong(&a), 2, "キャッシュが写しを持つ");
        // a を外す：b と共有する 3 タイルは残り、a だけのタイル 1 つ分が減る
        assert_eq!(cache.unregister_external(&a), Some(tile));
        assert_eq!(cache.bytes(), 5 * tile);
        assert_eq!(strong(&a), 1, "写しを手放す");
        assert_eq!(cache.unregister_external(&a), None, "もう登録していない");
        // 項目と共有する外の値を外しても、項目の分は残る
        assert_eq!(cache.register_external(&c), 0);
        assert_eq!(cache.unregister_external(&c), Some(0));
        assert_eq!(cache.bytes(), 5 * tile);
        // 同じ値を 2 度登録したら 2 度外すまで数える
        assert_eq!(cache.register_external(&b), 0);
        assert_eq!(cache.unregister_external(&b), Some(0));
        assert_eq!(cache.bytes(), 5 * tile);
        assert_eq!(cache.unregister_external(&b), Some(4 * tile));
        assert_eq!(cache.bytes(), tile, "項目だけ");
        // 外した値の写しは手放して数えない。共有していた項目の分は、ほかの項目と同じく追い出せる（追い出しは外の値を見ない）
        cache.set_capacity(0);
        assert_eq!((cache.bytes(), cache.len()), (0, 0));
    }

    /// 由来が Exact でないもの・解析は、値の当たりに使わない。
    #[test]
    fn only_exact_values_hit() {
        let mut c = NodeCache::new(1 << 30);
        c.insert_raw(fp(1), image(4, 4, 1), Origin::Patched);
        c.insert_raw(fp(2), image(4, 4, 1), Origin::Partial);
        c.insert(
            fp(3),
            Stored::Analysis(Arc::new(1u32)),
            Origin::Exact,
            Vec::new(),
            Vec::new(),
            8,
        );
        assert!(c.get_value(&fp(1)).is_none());
        assert!(c.get_value(&fp(2)).is_none());
        assert!(c.get_value(&fp(3)).is_none());
        assert!(c.get_analysis(&fp(3)).is_some());
        assert!(!c.contains(&fp(1)));
        // 音声のチャンネルも実体として数える
        let audio = Value::Audio(Arc::new(
            crate::value::Audio::new(48_000, vec![Arc::new(vec![0.0; 100])], AudioMeta::default())
                .unwrap(),
        ));
        let before = c.bytes();
        c.insert_raw(fp(4), audio.clone(), Origin::Exact);
        c.insert_raw(fp(5), audio, Origin::Exact);
        assert_eq!(c.bytes(), before + 400);
    }

    /// 派生の指紋は、用途・親・引数のどれでも変わる。
    #[test]
    fn derived_fingerprints_differ() {
        let base = Fingerprint::derive("level", fp(1), &[1]);
        assert_eq!(base, Fingerprint::derive("level", fp(1), &[1]));
        for other in [
            Fingerprint::derive("anchor", fp(1), &[1]),
            Fingerprint::derive("level", fp(2), &[1]),
            Fingerprint::derive("level", fp(1), &[2]),
            Fingerprint::derive("level", fp(1), &[]),
        ] {
            assert_ne!(base, other);
        }
    }
}
