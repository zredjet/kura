//! 解析（docs/editing.md §11）。値から事実（不透明な範囲・ラウドネスなど）を求める。値は変えない。
//!
//! - 解析は `Analysis` を実装し、分類ごとの一覧（`analysis/image/mod.rs`・`analysis/audio/mod.rs` の
//!   `register_analyses!`）に 1 行足す。全体の登録表（`registry`）は分類ごとの一覧を合わせたもの。
//! - 約束の単位は（解析 ID, 版）。同じ版・同じ引数・同じ値なら、いつでも同じ結果（正規の形のバイト列）を返す。
//!   - 操作が申告して使う解析（`Op::NEEDS` にある（ID, 版））は、操作と同じく凍結する。結果を変えるなら版を上げ、
//!     申告している操作の版がある限り旧版を `legacy` に残す（`Engine::new` が `check_needs` で確かめる）。申告している
//!     操作の版も上げ、新しい版で解析の今の版を申告する（今の版の操作が旧版の解析を申告していると、表示する値と
//!     操作が使う値がずれるので、`Engine::new` が `check_current_needs` で断る）。
//!   - 表示だけの解析は、版を上げたら旧版の実装を消して、`dropped` に（ID, 版）を載せてよい（kura-core が版で作り直す。
//!     ゴールデンのファイルは残し、照らさない）。
//!   - 速くする（結果は同じ）変更はそのまま変える（解析のゴールデン `goldens-analysis/` が見張る）。
//! - 引数（`Params`）は既定値つき：足りない項目は引数の記述（`ParamSchema`）の既定値で補い、知らない項目は拒む。
//!   正規の引数（既定値を補って正規の JSON にしたもの）が結果のキーの一部になる。
//! - 結果は正規の形（`AnalysisOutput`。既定は正規の JSON、波形の段はバイナリ）にして、ダイジェスト・ゴールデン・DB に使う。
//! - 操作は、実行器が先に解いた（か、その場で同じ関数で解いた）メモリ上の結果だけを使う（`ApplyCx::analysis`）。
//!   DB から読み直した結果は使わない。申告しない解析は使えない（`Internal`）。
//! - 決定性の規則は操作と同じ（`par_blocks`・`math`・`BTreeMap`・浮動小数の総和は `sum_blocks` か決まった順）。
//!   解析は乱数・時刻・環境を使わない（`AnalyzeCx` に種はない）。打ち切りのある数え方は、並列の順に依らない形で結果にする
//!   （「最初に見つけた n 個」を結果にしない）。
//! - フレームの列の扱い（各フレームをキャンバスの `offset` に置いたとき、フレームの外（覆われない部分）をどう見るか）は
//!   解析ごとに決める（`analysis/image/mod.rs` と各ファイルの冒頭。docs/editing.md §11.1）。

pub mod audio;
pub mod image;
pub mod loudness;
pub mod output;

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::digest::{Digest, Hasher, canonical_json};
use crate::doc::NodeId;
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::{Numeric, OpParams, ParamSchema, canonical_params};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::Value;

pub use output::{AnalysisOutput, Encoding, Real, Reasoned};

/// 引数のない解析の引数（`{}`。知らない項目は拒む）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

impl OpParams for NoParams {
    fn schema() -> ParamSchema {
        ParamSchema::default()
    }
}

/// 解析に渡す文脈（中止・上限・並列）。乱数は渡さない（解析は種を持たない）。
pub struct AnalyzeCx<'a> {
    pub mon: &'a dyn Monitor,
    pub limits: &'a Limits,
}

impl AnalyzeCx<'_> {
    /// 中止を頼まれていればエラーにする。`run` の最初と、フレーム・チャンネルの合間で呼ぶ。
    pub fn check(&self) -> Result<(), EngineError> {
        if self.mon.canceled() {
            Err(EngineError::Canceled)
        } else {
            Ok(())
        }
    }

    /// `0..count` のブロックを並列に計算し、添字の順に返す（`determinism::par_blocks`。ブロックの前で中止を確かめる）。
    pub fn par_blocks<T, F>(&self, count: usize, f: F) -> Result<Vec<T>, EngineError>
    where
        T: Send,
        F: Fn(usize) -> Result<T, EngineError> + Sync + Send,
    {
        crate::determinism::par_blocks(count, self.mon, f)
    }

    /// 作業領域を確保する（足りなければ `Limit`。大きさは先に `Limits` と見積もりで確かめておく）。
    pub fn reserve<T>(&self, n: usize) -> Result<Vec<T>, EngineError> {
        let mut v = Vec::new();
        v.try_reserve_exact(n)
            .map_err(|_| EngineError::Limit("解析の作業領域を確保できません".into()))?;
        Ok(v)
    }
}

/// 解析の見積もり（解く前に上限と照らす）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Estimate {
    /// 結果の正規の形の大きさ（バイト）の上の見積もり。`Limits::max_value_bytes` を超えれば解く前に `Limit`
    /// （`Engine::analyze`・`analyze_dyn`、操作の申告は `compile` で `TooLarge`）。
    pub bytes: u64,
    /// 手間（画素・標本などの数）の目安。本番の道では読まない（`analyze_many` の進捗は済んだ要求の数、
    /// `Plan::cost` は推論の手間だけ）。テストが使う：ファズ（`testing::fuzz_decode` が読めた値に解析を当てるとき）
    /// が重い解析を飛ばす目安と、適合テスト（E24）が解析の中で中止を確かめた回数の下限（手間 0 なら入口の 1 回で
    /// よい）。
    pub cost: u64,
}

/// ゴールデンと適合テストの例。名前・引数・入力は凍結する（変えずに新しい名前の例を足す）。
#[derive(Debug, Clone)]
pub struct AnalysisExample<P> {
    pub name: &'static str,
    pub params: P,
    pub input: InputGen,
}

/// 解析。
pub trait Analysis: Send + Sync + 'static {
    /// `<種類>.<名前>`（`image.opaque_bounds`）。内部用は名前で分かるようにする（`audio.loudness_gating`）。
    const ID: &'static str;
    const VERSION: u16;
    /// 受け付ける値の種類。
    const ACCEPTS: &'static [ValueKind];
    /// 数値の性質（ゴールデンのターゲット）。引数で変わるなら `numeric_for`。
    const NUMERIC: Numeric;
    /// 内部用（操作だけが申告して使う）。カタログに出さず、kura-core は DB に置かない。凍結は表に出すものと同じ。
    const INTERNAL: bool = false;
    /// 引数（既定値つき。知らない項目は拒む `deny_unknown_fields`）。引数のないものは `NoParams`。
    type Params: OpParams;
    /// 結果（正規の形を持つ型）。
    type Output: AnalysisOutput;

    /// その種類の値を受け付けるか。
    fn accepts(kind: ValueKind) -> bool {
        Self::ACCEPTS.contains(&kind)
    }
    /// 計算する。受け付けない種類・上限を超える値は呼ぶ側が断っている。最初に `cx.check()` を呼ぶ。
    fn run(p: &Self::Params, v: &Value, cx: &AnalyzeCx<'_>) -> Result<Self::Output, EngineError>;
    /// 例（1 つ以上）。
    fn examples() -> Vec<AnalysisExample<Self::Params>>;
    /// この引数で解くときの数値の性質。
    fn numeric_for(_p: &Self::Params) -> Numeric {
        Self::NUMERIC
    }
    /// この引数・入力の形で解くときの見積もり（結果の大きさは上の見積もり。適合テストが実際の大きさ以下を確かめる）。
    fn estimate(p: &Self::Params, input: &Shape) -> Estimate;
}

/// 型を消した例。
#[derive(Debug, Clone)]
pub struct DynAnalysisExample {
    pub name: &'static str,
    /// 正規の引数（既定値を補ったもの）。
    pub params: serde_json::Value,
    pub input: InputGen,
}

/// 型を消した結果（メモリ上の値。操作はこれを型に戻して使う）。
pub type AnyOutput = Arc<dyn Any + Send + Sync>;

/// 型を消した解析（登録表に入れる形）。
pub trait DynAnalysis: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> u16;
    fn accepts(&self) -> &'static [ValueKind];
    fn internal(&self) -> bool;
    fn encoding(&self) -> Encoding;
    fn schema(&self) -> ParamSchema;
    /// 引数を読み（足りない項目は既定値で補う）、範囲を確かめ、正規の JSON にしたものを返す。
    fn check_params(&self, raw: &serde_json::Value) -> Result<serde_json::Value, Issues>;
    /// この引数で解くときの数値の性質。
    fn numeric(&self, params: &serde_json::Value) -> Result<Numeric, Issues>;
    /// この引数・入力の形で解くときの見積もり。
    fn estimate(&self, params: &serde_json::Value, input: &Shape) -> Result<Estimate, Issues>;
    /// 解く（受け付けない種類は `Invalid`）。
    fn run_any(
        &self,
        params: &serde_json::Value,
        v: &Value,
        cx: &AnalyzeCx<'_>,
    ) -> Result<AnyOutput, EngineError>;
    /// 結果を正規の形にする。
    fn encode_any(&self, out: &AnyOutput) -> Result<Vec<u8>, EngineError>;
    /// 正規の形から読む（テストで偽の値を作るとき、kura-core が DB から読むとき）。
    fn decode_any(&self, bytes: &[u8]) -> Result<AnyOutput, EngineError>;
    /// 例（引数は正規の形）。
    fn examples(&self) -> Result<Vec<DynAnalysisExample>, EngineError>;
    /// 正規の形から読み直して、もう一度正規の形にすると同じバイト列になるか。
    fn round_trip(&self, bytes: &[u8]) -> Result<bool, EngineError>;
}

struct Entry<A: Analysis>(PhantomData<fn() -> A>);

/// 引数を読む：足りない項目は既定値で補い、範囲を確かめてから型に読む（知らない項目は型が拒む）。
fn parse_params<P: OpParams>(id: &str, raw: &serde_json::Value) -> Result<P, Issues> {
    let bad = |m: String| Issues::one(Issue::error(IssueCode::BadParams, m));
    let Some(given) = raw.as_object() else {
        return Err(bad(format!("{id} の引数はオブジェクトで書きます")));
    };
    let schema = P::schema();
    let mut full = match schema.defaults() {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    for (k, v) in given {
        full.insert(k.clone(), v.clone());
    }
    let full = serde_json::Value::Object(full);
    let ranges = schema.check_ranges(&full);
    if ranges.has_errors() {
        return Err(ranges);
    }
    serde_json::from_value(full).map_err(|e| bad(format!("{id} の引数が読めません: {e}")))
}

fn to_canonical_params<P: OpParams>(p: &P) -> Result<serde_json::Value, Issues> {
    canonical_params(&params_json(p)?).map_err(Issues::one)
}

fn bad_kind(id: &str, kind: ValueKind) -> EngineError {
    EngineError::Invalid(Issues::one(Issue::error(
        IssueCode::BadInput,
        format!("{id} は {kind:?} を解析できません"),
    )))
}

impl<A: Analysis> Entry<A> {
    fn typed(&self, params: &serde_json::Value) -> Result<A::Params, Issues> {
        parse_params(A::ID, params)
    }
}

/// 型付きの解析を、登録表に入れるのと同じ包みで型を消した形にする（`Engine::analyze` が `analyze_dyn` と同じ道で
/// 引数・種類・上限・見積もりを確かめるため。登録されていない試験用の解析にも使える）。
pub(crate) fn erased<A: Analysis>() -> impl DynAnalysis {
    Entry::<A>(PhantomData)
}

/// 正規の引数（`DynAnalysis::check_params` が返したもの）を型に読む（`analyze_dyn` が `run_any` で読むのと同じ）。
pub(crate) fn typed_params<A: Analysis>(
    canonical: &serde_json::Value,
) -> Result<A::Params, Issues> {
    parse_params(A::ID, canonical)
}

/// 型付きの引数を JSON にする（`Need::of` と同じく、正規の形にする前の値。範囲はまだ確かめていない）。
pub(crate) fn params_json<P: OpParams>(p: &P) -> Result<serde_json::Value, Issues> {
    serde_json::to_value(p)
        .map_err(|e| Issues::one(Issue::error(IssueCode::BadParams, e.to_string())))
}

impl<A: Analysis> DynAnalysis for Entry<A> {
    fn id(&self) -> &'static str {
        A::ID
    }

    fn version(&self) -> u16 {
        A::VERSION
    }

    fn accepts(&self) -> &'static [ValueKind] {
        A::ACCEPTS
    }

    fn internal(&self) -> bool {
        A::INTERNAL
    }

    fn encoding(&self) -> Encoding {
        A::Output::ENCODING
    }

    fn schema(&self) -> ParamSchema {
        A::Params::schema()
    }

    fn check_params(&self, raw: &serde_json::Value) -> Result<serde_json::Value, Issues> {
        // 小数の書き方（16.0 と 16、-0）をそろえてから読む
        let raw = canonical_params(raw).map_err(Issues::one)?;
        to_canonical_params(&self.typed(&raw)?)
    }

    fn numeric(&self, params: &serde_json::Value) -> Result<Numeric, Issues> {
        Ok(A::numeric_for(&self.typed(params)?))
    }

    fn estimate(&self, params: &serde_json::Value, input: &Shape) -> Result<Estimate, Issues> {
        Ok(A::estimate(&self.typed(params)?, input))
    }

    fn run_any(
        &self,
        params: &serde_json::Value,
        v: &Value,
        cx: &AnalyzeCx<'_>,
    ) -> Result<AnyOutput, EngineError> {
        if !A::accepts(v.kind()) {
            return Err(bad_kind(A::ID, v.kind()));
        }
        let p = self.typed(params).map_err(EngineError::Invalid)?;
        Ok(Arc::new(A::run(&p, v, cx)?))
    }

    fn encode_any(&self, out: &AnyOutput) -> Result<Vec<u8>, EngineError> {
        out.downcast_ref::<A::Output>()
            .ok_or_else(|| EngineError::Internal(format!("{} の結果の型が違います", A::ID)))?
            .to_canonical()
    }

    fn decode_any(&self, bytes: &[u8]) -> Result<AnyOutput, EngineError> {
        Ok(Arc::new(A::Output::from_canonical(bytes)?))
    }

    fn examples(&self) -> Result<Vec<DynAnalysisExample>, EngineError> {
        A::examples()
            .into_iter()
            .map(|e| {
                Ok(DynAnalysisExample {
                    name: e.name,
                    params: to_canonical_params(&e.params).map_err(EngineError::Invalid)?,
                    input: e.input,
                })
            })
            .collect()
    }

    fn round_trip(&self, bytes: &[u8]) -> Result<bool, EngineError> {
        let back = A::Output::from_canonical(bytes)?;
        Ok(back.to_canonical()? == bytes)
    }
}

/// 解析の記述（UI・kura-core のジョブのカタログ。内部用は出さない）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisDescriptor {
    pub id: &'static str,
    pub version: u16,
    pub accepts: &'static [ValueKind],
    pub encoding: Encoding,
    pub schema: ParamSchema,
}

/// 解析の登録表。（ID, 版）→ 実装。旧版（legacy）も入れる。実装を消した表示だけの解析の版は `dropped` に持つ。
#[derive(Default)]
pub struct AnalysisRegistry {
    all: BTreeMap<(&'static str, u16), Arc<dyn DynAnalysis>>,
    current: BTreeMap<&'static str, u16>,
    dropped: BTreeSet<(&'static str, u16)>,
}

/// ID は `<種類>.<名前>`（英小文字・数字・`_`）。
fn id_ok(id: &str) -> bool {
    let mut parts = id.split('.');
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if ok(a) && ok(b))
}

impl AnalysisRegistry {
    /// 空の表。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 今の版の解析を足す。同じ ID の今の版が 2 つあればエラー。
    pub fn add_current<A: Analysis>(&mut self) -> Result<(), String> {
        if self.current.insert(A::ID, A::VERSION).is_some() {
            return Err(format!("{} の今の版が 2 つあります", A::ID));
        }
        self.insert::<A>()
    }

    /// 結果を変えた旧版を足す（操作の旧版が申告しているもの）。
    pub fn add_legacy<A: Analysis>(&mut self) -> Result<(), String> {
        self.insert::<A>()
    }

    /// 実装を消した表示だけの解析の版を足す（ゴールデンのファイルは残り、照らさない）。
    pub fn add_dropped(&mut self, id: &'static str, v: u16) -> Result<(), String> {
        if self.all.contains_key(&(id, v)) {
            return Err(format!(
                "{id} の版 {v} は実装があるので dropped にできません"
            ));
        }
        if !self.dropped.insert((id, v)) {
            return Err(format!("{id} の版 {v} が dropped に 2 つあります"));
        }
        Ok(())
    }

    fn insert<A: Analysis>(&mut self) -> Result<(), String> {
        if !id_ok(A::ID) {
            return Err(format!(
                "{}：ID は <種類>.<名前>（英小文字・数字・_）",
                A::ID
            ));
        }
        if A::VERSION == 0 {
            return Err(format!("{}：版は 1 から", A::ID));
        }
        if A::ACCEPTS.is_empty() {
            return Err(format!("{}：受け付ける値の種類がありません", A::ID));
        }
        if self.dropped.contains(&(A::ID, A::VERSION)) {
            return Err(format!("{} の版 {} は dropped です", A::ID, A::VERSION));
        }
        if self
            .all
            .insert((A::ID, A::VERSION), Arc::new(Entry::<A>(PhantomData)))
            .is_some()
        {
            return Err(format!("{} の版 {} が 2 つあります", A::ID, A::VERSION));
        }
        Ok(())
    }

    /// その解析のその版の実装（旧版を含む）。
    pub fn get(&self, id: &str, v: u16) -> Option<Arc<dyn DynAnalysis>> {
        self.all
            .iter()
            .find(|((i, ver), _)| *i == id && *ver == v)
            .map(|(_, a)| a.clone())
    }

    /// 今の版の番号。
    pub fn current_version(&self, id: &str) -> Option<u16> {
        self.current.get(id).copied()
    }

    /// （ID, 版）を実装に結び付ける。版を省けば今の版。
    pub fn resolve(&self, id: &str, v: Option<u16>) -> Result<Arc<dyn DynAnalysis>, Issue> {
        let v = match v {
            Some(v) => v,
            None => self.current_version(id).ok_or_else(|| {
                Issue::error(IssueCode::UnknownOp, format!("知らない解析です（{id}）"))
            })?,
        };
        self.get(id, v).ok_or_else(|| {
            if self.all.keys().any(|(i, _)| *i == id) || self.is_dropped(id, v) {
                Issue::error(
                    IssueCode::UnknownVersion,
                    format!("解析 {id} の版 {v} の実装がありません"),
                )
            } else {
                Issue::error(IssueCode::UnknownOp, format!("知らない解析です（{id}）"))
            }
        })
    }

    /// すべての実装（旧版を含む）。（ID, 版）の順。
    pub fn all(&self) -> Vec<Arc<dyn DynAnalysis>> {
        self.all.values().cloned().collect()
    }

    /// 今の版の実装（内部用を含む）。
    pub fn current(&self) -> Vec<Arc<dyn DynAnalysis>> {
        self.all
            .iter()
            .filter(|((id, v), _)| self.current.get(id) == Some(v))
            .map(|(_, a)| a.clone())
            .collect()
    }

    /// 実装を消した表示だけの解析の版。
    pub fn dropped(&self) -> Vec<(&'static str, u16)> {
        self.dropped.iter().copied().collect()
    }

    /// 実装を消した版か。
    pub fn is_dropped(&self, id: &str, v: u16) -> bool {
        self.dropped.iter().any(|(i, ver)| *i == id && *ver == v)
    }

    /// カタログ（今の版で、内部用でないもの）。
    pub fn catalog(&self) -> Vec<AnalysisDescriptor> {
        self.current()
            .iter()
            .filter(|a| !a.internal())
            .map(|a| AnalysisDescriptor {
                id: a.id(),
                version: a.version(),
                accepts: a.accepts(),
                encoding: a.encoding(),
                schema: a.schema(),
            })
            .collect()
    }
}

/// 分類ごとの一覧を書く（`analysis/<分類>/mod.rs` で 1 回使う）。`register(&mut AnalysisRegistry)` を作る。
/// 解析を足すときは `current` に 1 行足す。結果を変えるなら版を上げ、申告されている旧版は `legacy` に、
/// 表示だけの旧版は実装を消して `dropped` に（ID, 版）を書く。
macro_rules! register_analyses {
    (current: [$($a:ty),* $(,)?], legacy: [$($old:ty),* $(,)?], dropped: [$(($id:expr, $v:expr)),* $(,)?] $(,)?) => {
        /// この分類の解析を登録表に足す。
        #[allow(unused_variables)]
        pub fn register(r: &mut $crate::analysis::AnalysisRegistry) -> Result<(), String> {
            $( r.add_current::<$a>()?; )*
            $( r.add_legacy::<$old>()?; )*
            $( r.add_dropped($id, $v)?; )*
            Ok(())
        }
    };
}
pub(crate) use register_analyses;

/// すべての解析の登録表（分類ごとの一覧を合わせる）。
pub fn registry() -> Result<AnalysisRegistry, String> {
    let mut r = AnalysisRegistry::empty();
    image::register(&mut r)?;
    audio::register(&mut r)?;
    Ok(r)
}

/// 解析の要求（kura-core のジョブ・ゴールデン・devtools）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisRequest {
    pub id: String,
    /// 版（省けば今の版）。
    #[serde(default)]
    pub v: Option<u16>,
    /// 引数（足りない項目は既定値）。
    #[serde(default = "empty_object")]
    pub params: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl AnalysisRequest {
    /// 今の版・既定の引数の要求。
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            v: None,
            params: empty_object(),
        }
    }
}

/// 解析の結果（kura-core が DB に置き、ゴールデンが凍結する形）。
#[derive(Debug, Clone, PartialEq)]
pub struct Analyzed {
    pub id: &'static str,
    pub version: u16,
    /// 正規の引数（既定値を補って正規の JSON にしたもの）。
    pub params: serde_json::Value,
    pub encoding: Encoding,
    /// 結果の正規の形。
    pub output: Vec<u8>,
}

impl Analyzed {
    /// 結果全体のダイジェスト（ドメイン `kura-analysis-v1`：ID・版・正規の引数・形の種類・結果）。
    /// 中身（どの値・どのファイルを解析したか）の識別は呼び手が別に持つ。
    pub fn digest(&self) -> Result<Digest, EngineError> {
        Ok(Hasher::new("kura-analysis-v1")
            .bytes_with_len(self.id.as_bytes())
            .u32(u32::from(self.version))
            .bytes_with_len(&canonical_json(&self.params)?)
            .u8(self.encoding.tag())
            .bytes_with_len(&self.output)
            .finish())
    }

    /// 結果の正規の形のダイジェスト（ゴールデンに使う）。
    pub fn output_digest(&self) -> Digest {
        output_digest(&self.output)
    }
}

/// 結果の正規の形のダイジェスト（ドメイン `kura-analysis-out-v1`、長さ付きのバイト列）。
pub fn output_digest(bytes: &[u8]) -> Digest {
    Hasher::new("kura-analysis-out-v1")
        .bytes_with_len(bytes)
        .finish()
}

/// 操作が実行器に先に解いてほしい解析 1 つ（`Op::needs` が返す）。
#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    /// どの入力の値を解析するか（操作の入力の番号）。
    pub input: usize,
    pub id: &'static str,
    pub v: u16,
    /// 正規の引数（既定値を補って正規の JSON にしたもの）。
    pub params: serde_json::Value,
}

/// 先に解いた結果を引くための鍵（入力の番号・ID・版・正規の引数のバイト列）。
type NeedKey = (usize, &'static str, u16, Vec<u8>);

impl Need {
    /// 型から作る。
    pub fn of<A: Analysis>(input: usize, p: &A::Params) -> Result<Self, Issues> {
        Ok(Self {
            input,
            id: A::ID,
            v: A::VERSION,
            params: to_canonical_params(p)?,
        })
    }

    fn key(&self) -> Result<NeedKey, EngineError> {
        Ok((self.input, self.id, self.v, canonical_json(&self.params)?))
    }
}

/// 同じ値（同じ中身の共有）か。先に解いた結果を、別の値に使わないための確かめ（結果には効かない）。
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Image(x), Value::Image(y)) => Arc::ptr_eq(x, y),
        (Value::Frames(x), Value::Frames(y)) => Arc::ptr_eq(x, y),
        (Value::Audio(x), Value::Audio(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

/// ノード 1 つ分の、先に解いた（かその場で解いた）解析の結果。
#[derive(Default)]
pub struct Solved {
    map: BTreeMap<NeedKey, (Value, AnyOutput)>,
    /// その場で解いた数（実行器は、申告と使った解析が食い違えば内部の誤りにする）。
    in_place: usize,
}

impl Solved {
    /// 先に解いた結果を入れる（実行器）。
    pub(crate) fn insert(
        &mut self,
        need: &Need,
        v: &Value,
        out: AnyOutput,
    ) -> Result<(), EngineError> {
        self.map.insert(need.key()?, (v.clone(), out));
        Ok(())
    }

    /// その場で解いた数。
    pub fn in_place(&self) -> usize {
        self.in_place
    }

    /// 型付きで引く。なければ（か別の値なら）、その場で同じ関数で解いて覚える（操作を直接呼ぶテスト・核の照合でも同じ値）。
    pub(crate) fn get_or_run<A: Analysis>(
        &mut self,
        input: usize,
        v: &Value,
        p: &A::Params,
        cx: &AnalyzeCx<'_>,
    ) -> Result<Arc<A::Output>, EngineError> {
        let key = Need::of::<A>(input, p)
            .map_err(EngineError::Invalid)?
            .key()?;
        if let Some((solved_for, any)) = self.map.get(&key)
            && same_value(solved_for, v)
        {
            return any
                .clone()
                .downcast::<A::Output>()
                .map_err(|_| EngineError::Internal(format!("{} の結果の型が違います", A::ID)));
        }
        if !A::accepts(v.kind()) {
            return Err(EngineError::Internal(format!(
                "{} は {:?} を解析できません（操作の申告の誤り）",
                A::ID,
                v.kind()
            )));
        }
        cx.limits.check(&v.shape()).map_err(EngineError::Limit)?;
        let out = Arc::new(A::run(p, v, cx)?);
        self.in_place += 1;
        let any: AnyOutput = out.clone();
        self.map.insert(key, (v.clone(), any));
        Ok(out)
    }
}

/// 実行器が、操作の申告した解析を解く口。既定（`Solve`）はその場で解く。段 10 でノードのキャッシュ
/// （入力の指紋 × ID × 版 × 引数）を差し込み、テストでは渡した値を記録したり、偽の値に差し替えたりする（E26）。
pub trait Solver: Sync {
    /// ノード `node` の申告 `need` を、その入力の値 `v` について解く。
    fn solve(
        &self,
        node: NodeId,
        need: &Need,
        v: &Value,
        analysis: &dyn DynAnalysis,
        cx: &AnalyzeCx<'_>,
    ) -> Result<AnyOutput, EngineError>;
}

/// その場で解く（既定）。
pub struct Solve;

impl Solver for Solve {
    fn solve(
        &self,
        _node: NodeId,
        need: &Need,
        v: &Value,
        analysis: &dyn DynAnalysis,
        cx: &AnalyzeCx<'_>,
    ) -> Result<AnyOutput, EngineError> {
        analysis.run_any(&need.params, v, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_a_kind_and_a_name() {
        for ok in ["image.alpha", "audio.loudness_gating", "audio.peaks2"] {
            assert!(id_ok(ok), "{ok}");
        }
        for bad in [
            "image",
            "image.",
            ".alpha",
            "Image.alpha",
            "image.alpha.x",
            "image.al-pha",
        ] {
            assert!(!id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn the_registry_is_consistent() {
        let r = registry().unwrap();
        assert!(!r.all().is_empty());
        // 内部用はカタログに出ない
        for d in r.catalog() {
            let a = r.get(d.id, d.version).unwrap();
            assert!(!a.internal());
        }
        assert!(
            r.current()
                .iter()
                .any(|a| a.internal() && r.catalog().iter().all(|d| d.id != a.id()))
        );
        assert!(r.resolve("image.nothing", None).is_err());
        assert!(matches!(
            r.resolve("image.opaque_bounds", Some(999)),
            Err(i) if i.code == IssueCode::UnknownVersion
        ));
        let mut r = AnalysisRegistry::empty();
        r.add_dropped("image.old", 1).unwrap();
        assert!(r.add_dropped("image.old", 1).is_err());
        assert!(r.is_dropped("image.old", 1));
        assert!(matches!(
            r.resolve("image.old", Some(1)),
            Err(i) if i.code == IssueCode::UnknownVersion
        ));
    }

    /// ダイジェストの式（docs/editing.md §11.1、ADR-152）：`kura-analysis-v1` = BLAKE3（ID（長さ付き）‖ 版 u32 ‖
    /// 正規の引数（長さ付き）‖ 形の種類 u8（JSON 0・バイナリ 1）‖ 結果（長さ付き））。式を組み立てて照らし、固定の値の
    /// 16 進も凍結する（式・`Hasher`・正規の JSON のどれが変わっても落ちる）。16 進は、`Hasher` を通さずに式どおり
    /// 並べたバイト列（長さは u64 LE、ドメインも長さ付き）を BLAKE3 にかけた値と照らして決めた。
    #[test]
    fn digest_formula_is_frozen() {
        let json = Analyzed {
            id: "image.opaque_bounds",
            version: 1,
            params: serde_json::json!({ "threshold": 7 }),
            encoding: Encoding::Json,
            output: br#"{"canvas":{"h":2,"w":3},"frames":[null],"union":null}"#.to_vec(),
        };
        let mut h = Hasher::new("kura-analysis-v1");
        h.bytes_with_len(b"image.opaque_bounds")
            .u32(1)
            .bytes_with_len(br#"{"threshold":7}"#)
            .u8(0)
            .bytes_with_len(&json.output);
        assert_eq!(json.digest().unwrap(), h.finish());
        assert_eq!(
            json.digest().unwrap().to_hex(),
            "99d17b94778707d7a1f48b18fb2587dcad30d8a698a490f178213bddc7bf7968"
        );
        let binary = Analyzed {
            id: "audio.peaks",
            version: 2,
            params: serde_json::json!({ "finest": 256, "coarsest": 1024 }),
            encoding: Encoding::Binary,
            output: vec![0, 1, 2, 255],
        };
        let mut h = Hasher::new("kura-analysis-v1");
        h.bytes_with_len(b"audio.peaks")
            .u32(2)
            .bytes_with_len(br#"{"coarsest":1024,"finest":256}"#)
            .u8(1)
            .bytes_with_len(&[0, 1, 2, 255]);
        assert_eq!(binary.digest().unwrap(), h.finish());
        assert_eq!(
            binary.digest().unwrap().to_hex(),
            "e67f179846aea7bdec1de5b9909cf46b7488cd7e2567c81d5fe4d2cb2e917155"
        );
        // 結果だけのダイジェスト（ゴールデンが照らす `kura-analysis-out-v1`）は別のドメイン
        assert_ne!(json.output_digest(), json.digest().unwrap());
        assert_eq!(
            json.output_digest(),
            Hasher::new("kura-analysis-out-v1")
                .bytes_with_len(&json.output)
                .finish()
        );
    }

    #[test]
    fn missing_params_take_the_defaults() {
        let r = registry().unwrap();
        let a = r.get("image.opaque_bounds", 1).unwrap();
        assert_eq!(
            a.check_params(&serde_json::json!({})).unwrap(),
            serde_json::json!({ "threshold": 0 })
        );
        assert_eq!(
            a.check_params(&serde_json::json!({ "threshold": 7.0 }))
                .unwrap(),
            serde_json::json!({ "threshold": 7 })
        );
        for bad in [
            serde_json::json!({ "threshold": 255 }),
            serde_json::json!({ "threshold": -1 }),
            serde_json::json!({ "thresh": 1 }),
            serde_json::json!([]),
        ] {
            assert!(a.check_params(&bad).is_err(), "{bad}");
        }
    }
}
