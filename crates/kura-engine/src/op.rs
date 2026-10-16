//! 操作の trait と登録表（docs/editing.md §7）。
//!
//! 操作を 1 つ足すときは `ops/<分類>/<名前>.rs` を作って `Op` を実装し、`ops/mod.rs` の `register_ops!` に 1 行足す。
//! メタデータは自分で写さず、`infer` で `SpaceMap`・`TimeMap` を申告する（エンジンが写す）。

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;

use rand_chacha::ChaCha8Rng;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::determinism;
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::limits::Limits;
use crate::mapping::{SpaceMap, TimeMap};
use crate::monitor::Monitor;
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::Value;

/// 引数の種類（エディタの汎用の引数パネルはここから組む）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ParamKind {
    Int { min: i64, max: i64, step: i64 },
    Float { min: f64, max: f64, step: f64 },
    Bool,
    Enum { values: &'static [&'static str] },
    Rect,
    Point,
    Size,
    Ratio,
    Color { alpha: bool },
    TimePos,
    Strokes,
}

/// 引数の単位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Unit {
    None,
    Px,
    Percent,
    Degrees,
    Db,
    Lufs,
    Ms,
    Samples,
    Hz,
    Times,
}

/// 引数 1 つの記述。`key` は serde のフィールド名と一致させる（適合テストで確かめる）。文言は ja.ts で `key` から引く。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamSpec {
    pub key: &'static str,
    pub kind: ParamKind,
    pub unit: Unit,
    pub default: serde_json::Value,
    /// 詳細な設定（ふだんは畳んでおく）。
    pub advanced: bool,
}

/// 引数の記述の一覧。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ParamSchema {
    pub params: Vec<ParamSpec>,
}

impl ParamSchema {
    /// 既定値の引数（JSON のオブジェクト）。
    pub fn defaults(&self) -> serde_json::Value {
        let map: serde_json::Map<String, serde_json::Value> = self
            .params
            .iter()
            .map(|p| (p.key.to_string(), p.default.clone()))
            .collect();
        serde_json::Value::Object(map)
    }

    /// 範囲の外・列挙にない値を調べる。
    pub fn check_ranges(&self, raw: &serde_json::Value) -> Issues {
        let mut issues = Issues::none();
        for spec in &self.params {
            let Some(v) = raw.get(spec.key) else { continue };
            let bad = match &spec.kind {
                ParamKind::Int { min, max, step } => v.as_i64().is_some_and(|n| {
                    n < *min || n > *max || (*step > 1 && (n - *min) % *step != 0)
                }),
                ParamKind::Float { min, max, .. } => v
                    .as_f64()
                    .is_some_and(|n| !n.is_finite() || n < *min || n > *max),
                ParamKind::Enum { values } => v.as_str().is_some_and(|s| !values.contains(&s)),
                // 座標は ±2^31 まで（それより大きい値は、後の計算であふれないよう読み込みで断る）
                ParamKind::Rect | ParamKind::Point => {
                    ["x", "y"].iter().any(|k| {
                        v.get(k)
                            .and_then(serde_json::Value::as_i64)
                            .is_some_and(|n| !crate::geom::coord_ok(n))
                    }) || ["w", "h"].iter().any(|k| {
                        v.get(k)
                            .and_then(serde_json::Value::as_u64)
                            .is_some_and(|n| n > crate::geom::COORD_LIMIT as u64)
                    })
                }
                _ => false,
            };
            if bad {
                issues.push(
                    Issue::error(
                        IssueCode::OutOfRange,
                        format!("{} が範囲の外です", spec.key),
                    )
                    .param(spec.key),
                );
            }
        }
        issues
    }

    /// 厳しく確かめる（書き出しの設定など、serde の型を通さない引数に使う）：知らないキー・型の違い・範囲の外。
    pub fn validate(&self, raw: &serde_json::Value) -> Issues {
        let mut issues = Issues::none();
        let Some(map) = raw.as_object() else {
            issues.push(Issue::error(
                IssueCode::BadParams,
                "設定はオブジェクトで書きます",
            ));
            return issues;
        };
        for key in map.keys() {
            if !self.params.iter().any(|p| p.key == key) {
                issues.push(
                    Issue::error(IssueCode::BadParams, format!("知らない設定です（{key}）"))
                        .param(key.clone()),
                );
            }
        }
        for spec in &self.params {
            let Some(v) = map.get(spec.key) else { continue };
            let typed = match &spec.kind {
                ParamKind::Int { .. } => v.is_i64() || v.is_u64(),
                ParamKind::Float { .. } => v.is_number(),
                ParamKind::Bool => v.is_boolean(),
                ParamKind::Enum { .. } => v.is_string(),
                _ => v.is_object(),
            };
            if !typed {
                issues.push(
                    Issue::error(IssueCode::BadParams, format!("{} の型が違います", spec.key))
                        .param(spec.key),
                );
            }
        }
        issues.extend(self.check_ranges(raw));
        issues
    }
}

/// 操作の引数の型。
pub trait OpParams:
    Serialize + DeserializeOwned + Clone + PartialEq + Debug + Send + Sync + 'static
{
    /// 引数の記述。
    fn schema() -> ParamSchema;
}

/// 入力の口の記述。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PortSpec {
    pub name: &'static str,
    pub accepts: &'static [ValueKind],
}

/// 画像の操作をフレームの列に当てる方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Lift {
    /// 持ち上げない（受け付ける種類だけ）。
    None,
    /// フレームの列なら、各フレームに同じ操作を当てる（出力の大きさは推論で決まること）。
    PerFrame,
}

/// 結果の数値の性質（ゴールデンをターゲットごとに持つかどうか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Numeric {
    /// 整数だけ。すべてのターゲットで同じ結果。
    Exact,
    /// 浮動小数を使う。ゴールデンはターゲットごと。
    Float,
}

/// メタデータの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MetaAccess {
    /// エンジンが `SpaceMap`・`TimeMap` で写す（ふつう）。
    Map,
    /// 操作が自分で書く（格子・ループ点を設定する操作）。
    Write,
}

/// 操作の型の記述。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Signature {
    pub inputs: &'static [PortSpec],
    pub lift: Lift,
    pub numeric: Numeric,
    pub meta: MetaAccess,
}

/// 推論の結果。
#[derive(Debug, Clone, PartialEq)]
pub struct Inferred {
    pub shape: Shape,
    pub space: SpaceMap,
    pub time: TimeMap,
    /// 見積もりの手間（画素・フレームの数）。進捗と予約に使う。
    pub cost: u64,
}

/// 実行の結果。`space`・`time` は実際の写し方：推論が `Unknown` なら必ず返し、そうでなければ返さないか、推論と同じものを返す
/// （実行器が確かめ、食い違えば内部の誤りにする。E9）。
#[derive(Debug, Clone)]
pub struct Applied {
    pub value: Value,
    pub space: Option<SpaceMap>,
    pub time: Option<TimeMap>,
}

impl Applied {
    /// 写し方を推論のままにする結果。
    pub fn value(value: Value) -> Self {
        Self {
            value,
            space: None,
            time: None,
        }
    }
}

/// 推論に渡す文脈。
pub struct InferCx<'a> {
    pub limits: &'a Limits,
}

/// 実行に渡す文脈。中止の確認・乱数・警告はここを通す。
pub struct ApplyCx<'a> {
    pub mon: &'a dyn Monitor,
    pub limits: &'a Limits,
    /// この操作の種（ノードの種・元のハッシュ・操作の ID から作ったもの）。
    pub seed: u64,
    pub warnings: Vec<Issue>,
}

impl ApplyCx<'_> {
    /// 中止を頼まれていればエラーにする。
    pub fn check(&self) -> Result<(), EngineError> {
        if self.mon.canceled() {
            Err(EngineError::Canceled)
        } else {
            Ok(())
        }
    }

    /// 絶対位置の乱数（流れは画像なら行・音声ならチャンネル、`word` はその中の位置）。
    /// タイル・ブロック・フレームの番号を使わないので、分け方や持ち上げによらず同じ値になる（docs/editing.md §8.2）。
    pub fn rng_at(&self, stream: u64, word: u128) -> ChaCha8Rng {
        determinism::rng_at(self.seed, stream, word)
    }

    /// 警告を足す。
    pub fn warn(&mut self, issue: Issue) {
        self.warnings.push(issue);
    }
}

/// 適合テストとゴールデンの例。入力は作り方の記述から決定的に作る。
#[derive(Debug, Clone)]
pub struct Example<P> {
    pub name: &'static str,
    pub params: P,
    pub inputs: Vec<InputGen>,
}

/// 操作。
pub trait Op: Send + Sync + 'static {
    const ID: &'static str;
    const VERSION: u16;
    type Params: OpParams;

    /// 入力の種類と数、持ち上げ方、数値の性質、メタデータの扱い。
    fn signature() -> Signature;
    /// 形の推論とメタデータの写し方。描かない。意味の検証（矩形が入力の中にあるか）もここで行う。
    fn infer(p: &Self::Params, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues>;
    /// 実際に計算する。
    fn apply(
        p: &Self::Params,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError>;
    /// 実装を消した古い版 `from` の引数を、結果を変えずにこの版へ移す。移せなければ理由を返す。
    /// 呼ばれるのは、`from` より新しい実装のある版のうち、この版がいちばん古いとき（すぐ上の版）。
    fn migrate(from: u16, _raw: serde_json::Value) -> Result<serde_json::Value, String> {
        Err(format!("版 {from} からは移せません"))
    }
    /// 適合テストとゴールデンの例（1 つ以上）。
    fn examples() -> Vec<Example<Self::Params>>;
}

/// 型を消した引数。
pub type AnyParams = dyn Any + Send + Sync;

/// 型を消した例。
#[derive(Debug, Clone)]
pub struct DynExample {
    pub name: &'static str,
    pub params: serde_json::Value,
    pub inputs: Vec<InputGen>,
}

/// 型を消した操作（登録表に入れる形）。
pub trait DynOp: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> u16;
    fn signature(&self) -> Signature;
    fn schema(&self) -> ParamSchema;
    /// 引数を読み、範囲を確かめる。
    fn parse(&self, raw: &serde_json::Value) -> Result<Box<AnyParams>, Issues>;
    /// 読んだ引数を JSON に戻す（往復のテスト用）。
    fn params_json(&self, p: &AnyParams) -> Result<serde_json::Value, EngineError>;
    fn infer(&self, p: &AnyParams, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues>;
    fn apply(
        &self,
        p: &AnyParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError>;
    fn migrate(&self, from: u16, raw: serde_json::Value) -> Result<serde_json::Value, String>;
    fn examples(&self) -> Result<Vec<DynExample>, EngineError>;
}

/// `Op` を `DynOp` にする包み。
pub struct OpEntry<O: Op>(PhantomData<fn() -> O>);

impl<O: Op> OpEntry<O> {
    fn params<'a>(&self, p: &'a AnyParams) -> Result<&'a O::Params, EngineError> {
        p.downcast_ref::<O::Params>()
            .ok_or_else(|| EngineError::Internal(format!("{} の引数の型が違います", O::ID)))
    }
}

impl<O: Op> DynOp for OpEntry<O> {
    fn id(&self) -> &'static str {
        O::ID
    }

    fn version(&self) -> u16 {
        O::VERSION
    }

    fn signature(&self) -> Signature {
        O::signature()
    }

    fn schema(&self) -> ParamSchema {
        O::Params::schema()
    }

    fn parse(&self, raw: &serde_json::Value) -> Result<Box<AnyParams>, Issues> {
        // 範囲・列挙を先に見る（どの引数がおかしいかを示せる）
        let ranges = O::Params::schema().check_ranges(raw);
        if ranges.has_errors() {
            return Err(ranges);
        }
        let typed: O::Params = serde_json::from_value(raw.clone()).map_err(|e| {
            Issues::one(Issue::error(
                IssueCode::BadParams,
                format!("{} の引数が読めません: {e}", O::ID),
            ))
        })?;
        Ok(Box::new(typed))
    }

    fn params_json(&self, p: &AnyParams) -> Result<serde_json::Value, EngineError> {
        serde_json::to_value(self.params(p)?).map_err(|e| EngineError::Internal(e.to_string()))
    }

    fn infer(&self, p: &AnyParams, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        let p = self
            .params(p)
            .map_err(|e| Issues::one(Issue::error(IssueCode::BadParams, e.to_string())))?;
        O::infer(p, inputs, cx)
    }

    fn apply(
        &self,
        p: &AnyParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        O::apply(self.params(p)?, inputs, cx)
    }

    fn migrate(&self, from: u16, raw: serde_json::Value) -> Result<serde_json::Value, String> {
        O::migrate(from, raw)
    }

    fn examples(&self) -> Result<Vec<DynExample>, EngineError> {
        O::examples()
            .into_iter()
            .map(|e| {
                let params = serde_json::to_value(&e.params)
                    .map_err(|err| EngineError::Internal(err.to_string()))?;
                Ok(DynExample {
                    name: e.name,
                    params,
                    inputs: e.inputs,
                })
            })
            .collect()
    }
}

/// 操作の記述（UI のカタログ）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpDescriptor {
    pub id: &'static str,
    pub version: u16,
    pub signature: Signature,
    pub schema: ParamSchema,
}

/// 版の解決の結果。
pub enum Resolved {
    /// その版の実装がある（今の版か旧版）。
    Direct(Arc<dyn DynOp>),
    /// 実装のない古い版：すぐ上の実装のある版（旧版のこともある）へ移した引数と、その版の実装。
    Migrated(Arc<dyn DynOp>, serde_json::Value),
}

/// 操作の登録表。（操作 ID, 版）→ 実装。旧版（legacy）も入れる。
#[derive(Default)]
pub struct Registry {
    ops: BTreeMap<(&'static str, u16), Arc<dyn DynOp>>,
    current: BTreeMap<&'static str, u16>,
}

impl Registry {
    /// 空の表。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 今の版の操作を足す。同じ ID の今の版が 2 つあればエラー。
    pub fn add_current<O: Op>(&mut self) -> Result<(), String> {
        if self.current.insert(O::ID, O::VERSION).is_some() {
            return Err(format!("{} の今の版が 2 つあります", O::ID));
        }
        self.insert::<O>()
    }

    /// 結果を変えた旧版の操作を足す（新しく作れないが、作り直しに使う）。
    pub fn add_legacy<O: Op>(&mut self) -> Result<(), String> {
        self.insert::<O>()
    }

    fn insert<O: Op>(&mut self) -> Result<(), String> {
        let sig = O::signature();
        // 持ち上げた各フレームが書いたメタデータは、フレームの列のメタデータに戻せない
        if sig.lift == Lift::PerFrame && sig.meta == MetaAccess::Write {
            return Err(format!(
                "{}：フレームに持ち上げる操作はメタデータを書けません",
                O::ID
            ));
        }
        if self
            .ops
            .insert((O::ID, O::VERSION), Arc::new(OpEntry::<O>(PhantomData)))
            .is_some()
        {
            return Err(format!("{} の版 {} が 2 つあります", O::ID, O::VERSION));
        }
        Ok(())
    }

    /// 今の版の番号。
    pub fn current_version(&self, id: &str) -> Option<u16> {
        self.current.get(id).copied()
    }

    /// その操作の、実装のある版（旧版を含む）を古い順に。
    pub fn versions(&self, id: &str) -> Vec<u16> {
        self.ops
            .keys()
            .filter(|(op, _)| *op == id)
            .map(|(_, v)| *v)
            .collect()
    }

    fn get(&self, id: &str, v: u16) -> Option<Arc<dyn DynOp>> {
        self.ops
            .iter()
            .find(|((op, ver), _)| *op == id && *ver == v)
            .map(|(_, i)| i.clone())
    }

    /// ノードの（操作, 版, 引数）を実装に結び付ける（docs/editing.md §8.4）。
    /// - その版の実装（今の版か旧版）があればそれを使う。
    /// - なければ、すぐ上の実装のある版の `migrate` で引数を移し、その版の実装で描く（旧版を経由する鎖も表せる）。
    /// - 知らない操作・今の版より新しい版・移せない版はエラー。
    pub fn resolve(&self, op: &str, v: u16, params: &serde_json::Value) -> Result<Resolved, Issue> {
        if let Some(imp) = self.get(op, v) {
            return Ok(Resolved::Direct(imp));
        }
        let Some(cur) = self.current_version(op) else {
            return Err(Issue::error(
                IssueCode::UnknownOp,
                format!("知らない操作です（{op}）"),
            ));
        };
        if v > cur {
            return Err(Issue::error(
                IssueCode::UnknownVersion,
                format!("{op} の版 {v} はこの版の Kura より新しい"),
            ));
        }
        let Some(next) = self.versions(op).into_iter().find(|ver| *ver > v) else {
            return Err(Issue::error(
                IssueCode::UnknownOp,
                format!("知らない操作です（{op}）"),
            ));
        };
        let Some(imp) = self.get(op, next) else {
            return Err(Issue::error(
                IssueCode::UnknownOp,
                format!("知らない操作です（{op}）"),
            ));
        };
        match imp.migrate(v, params.clone()) {
            Ok(moved) => Ok(Resolved::Migrated(imp, moved)),
            Err(why) => Err(Issue::error(
                IssueCode::UnknownVersion,
                format!("{op} の版 {v} を移せません: {why}"),
            )),
        }
    }

    /// 今の版の操作。
    pub fn current(&self) -> Vec<Arc<dyn DynOp>> {
        self.ops
            .iter()
            .filter(|((id, v), _)| self.current.get(id) == Some(v))
            .map(|(_, i)| i.clone())
            .collect()
    }

    /// すべての実装（旧版を含む）。
    pub fn all(&self) -> Vec<Arc<dyn DynOp>> {
        self.ops.values().cloned().collect()
    }

    /// UI のカタログ（今の版だけ）。
    pub fn catalog(&self) -> Vec<OpDescriptor> {
        self.current()
            .iter()
            .map(|op| OpDescriptor {
                id: op.id(),
                version: op.version(),
                signature: op.signature(),
                schema: op.schema(),
            })
            .collect()
    }
}

/// 引数の記述を短く書く補助。
pub fn spec(
    key: &'static str,
    kind: ParamKind,
    unit: Unit,
    default: serde_json::Value,
) -> ParamSpec {
    ParamSpec {
        key,
        kind,
        unit,
        default,
        advanced: false,
    }
}

/// 推論の補助：入力の数と種類を確かめる（持ち上げは呼ぶ側が済ませている）。
pub fn expect_inputs(inputs: &[Shape], sig: &Signature) -> Result<(), Issues> {
    if inputs.len() != sig.inputs.len() {
        return Err(Issues::one(Issue::error(
            IssueCode::BadInput,
            format!(
                "入力の数が違います（{} 個のはずが {} 個）",
                sig.inputs.len(),
                inputs.len()
            ),
        )));
    }
    for (shape, port) in inputs.iter().zip(sig.inputs) {
        if !port.accepts.contains(&shape.kind()) {
            return Err(Issues::one(Issue::error(
                IssueCode::BadInput,
                format!("入力「{}」に {:?} は使えません", port.name, shape.kind()),
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct P {
        x: i64,
    }

    impl OpParams for P {
        fn schema() -> ParamSchema {
            ParamSchema {
                params: vec![spec(
                    "x",
                    ParamKind::Int {
                        min: 0,
                        max: 10,
                        step: 1,
                    },
                    Unit::None,
                    serde_json::json!(0),
                )],
            }
        }
    }

    const SIG: Signature = Signature {
        inputs: &[PortSpec {
            name: "image",
            accepts: &[ValueKind::Image],
        }],
        lift: Lift::None,
        numeric: Numeric::Exact,
        meta: MetaAccess::Map,
    };

    macro_rules! dummy {
        ($name:ident, $v:expr, $from:expr) => {
            struct $name;
            impl Op for $name {
                const ID: &'static str = "test.dummy";
                const VERSION: u16 = $v;
                type Params = P;
                fn signature() -> Signature {
                    SIG
                }
                fn infer(_: &P, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
                    Ok(Inferred { shape: inputs[0], space: SpaceMap::Identity, time: TimeMap::Identity, cost: 0 })
                }
                fn apply(_: &P, inputs: &[&Value], _: &mut ApplyCx<'_>) -> Result<Applied, EngineError> {
                    Ok(Applied::value(inputs[0].clone()))
                }
                fn migrate(from: u16, raw: serde_json::Value) -> Result<serde_json::Value, String> {
                    if from == $from { Ok(serde_json::json!({ "x": raw.get("old").and_then(|v| v.as_i64()).unwrap_or(0) })) } else { Err("移せない".into()) }
                }
                fn examples() -> Vec<Example<P>> {
                    Vec::new()
                }
            }
        };
    }

    // v1 は実装を消して v2 へ移す。v2 は結果を変えたので旧版に残す。v3 が今の版（v2 からは移せない）
    dummy!(V2, 2, 1);
    dummy!(V3, 3, 0);

    fn table() -> Registry {
        let mut r = Registry::empty();
        r.add_current::<V3>().unwrap();
        r.add_legacy::<V2>().unwrap();
        r
    }

    #[test]
    fn migration_goes_to_the_next_implemented_version_even_if_legacy() {
        let r = table();
        match r.resolve("test.dummy", 1, &serde_json::json!({ "old": 4 })) {
            Ok(Resolved::Migrated(imp, moved)) => {
                assert_eq!(imp.version(), 2);
                assert_eq!(moved, serde_json::json!({ "x": 4 }));
            }
            _ => panic!("v1 は v2（旧版）へ移るはず"),
        }
        assert!(
            matches!(r.resolve("test.dummy", 2, &serde_json::json!({})), Ok(Resolved::Direct(i)) if i.version() == 2)
        );
        assert!(
            matches!(r.resolve("test.dummy", 3, &serde_json::json!({})), Ok(Resolved::Direct(i)) if i.version() == 3)
        );
        assert!(
            matches!(r.resolve("test.dummy", 4, &serde_json::json!({})), Err(i) if i.code == IssueCode::UnknownVersion)
        );
        assert!(
            matches!(r.resolve("test.dummy", 0, &serde_json::json!({})), Err(i) if i.code == IssueCode::UnknownVersion)
        );
        assert!(
            matches!(r.resolve("test.none", 1, &serde_json::json!({})), Err(i) if i.code == IssueCode::UnknownOp)
        );
    }

    struct Bad;
    impl Op for Bad {
        const ID: &'static str = "test.bad";
        const VERSION: u16 = 1;
        type Params = P;
        fn signature() -> Signature {
            Signature {
                lift: Lift::PerFrame,
                meta: MetaAccess::Write,
                ..SIG
            }
        }
        fn infer(_: &P, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
            Ok(Inferred {
                shape: inputs[0],
                space: SpaceMap::Identity,
                time: TimeMap::Identity,
                cost: 0,
            })
        }
        fn apply(_: &P, inputs: &[&Value], _: &mut ApplyCx<'_>) -> Result<Applied, EngineError> {
            Ok(Applied::value(inputs[0].clone()))
        }
        fn examples() -> Vec<Example<P>> {
            Vec::new()
        }
    }

    #[test]
    fn registration_rejects_duplicates_and_bad_signatures() {
        let mut r = table();
        assert!(r.add_current::<V3>().is_err());
        assert!(r.add_legacy::<V2>().is_err());
        assert!(Registry::empty().add_current::<Bad>().is_err());
        assert!(crate::ops::registry().is_ok());
    }

    #[test]
    fn strict_validation_rejects_unknown_keys_types_and_steps() {
        let schema = ParamSchema {
            params: vec![
                spec(
                    "bits",
                    ParamKind::Int {
                        min: 8,
                        max: 32,
                        step: 8,
                    },
                    Unit::None,
                    serde_json::json!(16),
                ),
                spec(
                    "float",
                    ParamKind::Bool,
                    Unit::None,
                    serde_json::json!(false),
                ),
                spec(
                    "mode",
                    ParamKind::Enum {
                        values: &["a", "b"],
                    },
                    Unit::None,
                    serde_json::json!("a"),
                ),
            ],
        };
        assert!(schema.validate(&serde_json::json!({})).is_empty());
        assert!(
            schema
                .validate(&serde_json::json!({ "bits": 24, "float": true, "mode": "b" }))
                .is_empty()
        );
        for bad in [
            serde_json::json!({ "bits": 12 }),
            serde_json::json!({ "bits": "24" }),
            serde_json::json!({ "bits": 24.0 }),
            serde_json::json!({ "float": "true" }),
            serde_json::json!({ "mode": "c" }),
            serde_json::json!({ "mdoe": "a" }),
            serde_json::json!([1]),
        ] {
            assert!(schema.validate(&bad).has_errors(), "{bad}");
        }
    }

    #[test]
    fn coordinates_out_of_range_are_rejected() {
        let schema = ParamSchema {
            params: vec![spec(
                "rect",
                ParamKind::Rect,
                Unit::Px,
                serde_json::json!({}),
            )],
        };
        let ok = serde_json::json!({ "rect": { "x": -5, "y": 2, "w": 3, "h": 4 } });
        let far = serde_json::json!({ "rect": { "x": i64::MAX, "y": 0, "w": 1, "h": 1 } });
        let wide =
            serde_json::json!({ "rect": { "x": 0, "y": 0, "w": u64::from(u32::MAX), "h": 1 } });
        assert!(schema.check_ranges(&ok).is_empty());
        assert!(schema.check_ranges(&far).has_errors());
        assert!(schema.check_ranges(&wide).has_errors());
    }
}
