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

use crate::analysis::{Analysis, AnalyzeCx, AnyOutput, Need, Solved};
use crate::audio::AudioKernel;
use crate::determinism;
use crate::digest::{Digest, Hasher};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::limits::Limits;
use crate::mapping::{SpaceMap, TimeMap};
use crate::monitor::Monitor;
use crate::render::preview::Preview;
use crate::render::region::{Area, FrameTiles, Reach};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::Value;

/// 引数の種類（エディタの汎用の引数パネルはここから組む）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ParamKind {
    Int {
        min: i64,
        max: i64,
        step: i64,
    },
    Float {
        min: f64,
        max: f64,
        step: f64,
    },
    Bool,
    Enum {
        values: &'static [&'static str],
    },
    Rect,
    Point,
    Size,
    Ratio,
    Color {
        alpha: bool,
    },
    TimePos,
    Strokes,
    /// 長さ（画素・入力に対する比・縦横比を保つ自動）。
    Length,
    /// スプライトの格子（セルの大きさ・左上・間隔）。
    Grid,
    /// パレットの色の置き換え（番号と色の組）。
    PaletteEntries,
    /// 音声の区間（フレーム。`{start, end}`、end は含まない。null で消す）。
    FrameRange,
}

/// 引数の単位。
#[cfg_attr(feature = "specta", derive(specta::Type))]
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

/// 引数を表示する条件（docs/editing.md §7.2 の「表示の条件」）：同じ操作の列挙の引数 `key` の値が `values` のどれかのとき
/// だけ、この引数を出す（例：`image.convert` の `colors`・`dither` は `format` が `indexed8` のときだけ）。値の検証・既定値には
/// 効かない（出さない引数も文書に書いたまま読み、既定値を持つ）。条件の引数は同じ記述の `Enum` で、値はその列挙の中
/// （適合テストで確かめる）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Visible {
    pub key: &'static str,
    pub values: &'static [&'static str],
}

/// 引数 1 つの記述。`key` は serde のフィールド名と一致させる（適合テストで確かめる）。文言は ja.ts で `key` から引く。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamSpec {
    pub key: &'static str,
    pub kind: ParamKind,
    pub unit: Unit,
    #[cfg_attr(feature = "specta", specta(type = crate::specta_types::JsonValue))]
    pub default: serde_json::Value,
    /// 詳細な設定（ふだんは畳んでおく）。
    pub advanced: bool,
    /// 表示の条件（None はいつも出す）。
    pub visible: Option<Visible>,
}

impl ParamSpec {
    /// 列挙の引数 `key` の値が `values` のどれかのときだけ出す（[`Visible`]）。
    pub fn when(mut self, key: &'static str, values: &'static [&'static str]) -> Self {
        self.visible = Some(Visible { key, values });
        self
    }
}

/// 引数の記述の一覧。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ParamSchema {
    pub params: Vec<ParamSpec>,
}

impl ParamSchema {
    /// 追記だけの配列の引数（[`ParamKind::Strokes`]）の名前（段 10。セッションと実行器が、追記した分だけを読み・指紋を
    /// 求め直し・差分で当てる）。ない、または 2 つ以上あれば `None`（差分の口を使わない）。
    pub fn append_field(&self) -> Option<&'static str> {
        let mut found = self
            .params
            .iter()
            .filter(|p| matches!(p.kind, ParamKind::Strokes));
        let first = found.next()?;
        found.next().is_none().then_some(first.key)
    }

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
                // 区間は 0 ≤ start < end ≤ 2^40（後の計算であふれないよう読み込みで断る）
                ParamKind::FrameRange => {
                    !v.is_null() && {
                        let get = |k: &str| v.get(k).and_then(serde_json::Value::as_u64);
                        match (get("start"), get("end")) {
                            (Some(s), Some(e)) => {
                                s >= e || e > crate::ops::util::MAX_FRAME_PARAM as u64
                            }
                            _ => false,
                        }
                    }
                }
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
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PortSpec {
    pub name: &'static str,
    pub accepts: &'static [ValueKind],
}

/// 画像の操作をフレームの列に当てる方法。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Lift {
    /// 持ち上げない（受け付ける種類だけ）。
    None,
    /// フレームの列なら、各フレームに同じ操作を当てる（出力の大きさは推論で決まること）。
    PerFrame,
}

/// 結果の数値の性質（ゴールデンをターゲットごとに持つかどうか）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Numeric {
    /// 整数だけ。すべてのターゲットで同じ結果。
    Exact,
    /// 浮動小数を使う。ゴールデンはターゲットごと。
    Float,
}

/// メタデータの扱い。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MetaAccess {
    /// エンジンが `SpaceMap`・`TimeMap` で写す（ふつう）。
    Map,
    /// 操作が自分で書く（格子・ループ点を設定する操作）。
    Write,
}

/// 操作の型の記述。
#[cfg_attr(feature = "specta", derive(specta::Type))]
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

/// 描く間に、入力と出力の値のほかに使う作業領域の見積もり（バイト。§12 のメモリの予算。[`Op::work_bytes`]）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Work {
    /// 描く間に 1 つだけ持つ分（色の表・重みの表・描いている途中の平面など）。
    pub shared: u64,
    /// 並列の仕事（`par_blocks` の 1 ブロック・1 タイル）が 1 つごとに持つ分。同時に動く仕事の数（プールのスレッドの数。
    /// 呼び手が知っている）を掛けて足す。
    pub per_task: u64,
}

impl Work {
    /// 作業領域なし（既定）。
    pub const ZERO: Self = Self {
        shared: 0,
        per_task: 0,
    };

    /// 1 つだけ持つ分だけの作業領域。
    pub fn shared(bytes: u64) -> Self {
        Self {
            shared: bytes,
            per_task: 0,
        }
    }

    /// 同時に `tasks` 個の仕事が動くときの合計（あふれは `u64::MAX` に飽和する）。
    pub fn total(self, tasks: u64) -> u64 {
        self.shared
            .saturating_add(self.per_task.saturating_mul(tasks))
    }
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

/// 実行に渡す文脈。中止の確認・乱数・警告・申告した解析の結果はここを通す。作るのは `ApplyCx::new`。
pub struct ApplyCx<'a> {
    pub mon: &'a dyn Monitor,
    pub limits: &'a Limits,
    /// この操作の種（ノードの種・元のハッシュ・操作の ID から作ったもの）。
    pub seed: u64,
    /// 文書の全体の規則の版（`doc::Graph::rules`）。メタデータを自分で書く操作（`MetaAccess::Write`）が座標を写すときは、
    /// `mapping::map_image_meta_for` にこれを渡して、文書の規則の写し方を使う（§8.4）。
    pub rules: u32,
    pub warnings: Vec<Issue>,
    /// 実行器が先に解いた解析と、その場で解いた解析（§11.2）。
    analyses: Solved,
    /// 今の操作が申告しうる解析（`Op::NEEDS`。`DynOp` が操作を呼ぶ前に入れる）。
    declared: &'static [(&'static str, u16)],
}

impl<'a> ApplyCx<'a> {
    /// 文脈を作る（先に解いた解析は空）。
    pub fn new(mon: &'a dyn Monitor, limits: &'a Limits, seed: u64, rules: u32) -> Self {
        Self {
            mon,
            limits,
            seed,
            rules,
            warnings: Vec::new(),
            analyses: Solved::default(),
            declared: &[],
        }
    }

    /// 申告した解析の結果（入力 `input` の値 `v` を、引数 `p` で解いたもの）。実行器が先に解いていればそれを、
    /// なければその場で同じ関数で解いて覚える（操作を直接呼ぶテスト・核の照合でも同じ値になる）。
    /// （ID, 版）が操作の `NEEDS` になければ内部の誤り（申告せずに使うと、解析の版上げで操作の結果が黙って変わるため）。
    pub fn analysis<A: Analysis>(
        &mut self,
        input: usize,
        v: &Value,
        p: &A::Params,
    ) -> Result<Arc<A::Output>, EngineError> {
        if !self.declared.contains(&(A::ID, A::VERSION)) {
            return Err(EngineError::Internal(format!(
                "申告していない解析 {} v{} を使おうとしました（Op::NEEDS に足す）",
                A::ID,
                A::VERSION
            )));
        }
        let cx = AnalyzeCx {
            mon: self.mon,
            limits: self.limits,
        };
        self.analyses.get_or_run::<A>(input, v, p, &cx)
    }

    /// 実行器が先に解いた結果を入れる。
    pub(crate) fn preload(
        &mut self,
        need: &Need,
        v: &Value,
        out: AnyOutput,
    ) -> Result<(), EngineError> {
        self.analyses.insert(need, v, out)
    }

    /// 先に解いた結果がなく、その場で解いた解析の数（実行器が描いたあとに確かめる）。
    pub fn analyses_solved_in_place(&self) -> usize {
        self.analyses.in_place()
    }

    /// 今の操作が申告しうる解析を入れる（`DynOp` が操作を呼ぶ前に）。
    fn declare(&mut self, needs: &'static [(&'static str, u16)]) {
        self.declared = needs;
    }
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
    /// 申告しうる解析の（ID, 版）（§11.2）。操作の版ごとに固定する（申告する解析の版を上げるなら操作の版も上げる）。
    /// `Engine::new` が、すべてが登録されている（旧版の操作が使う旧版の解析を含む）ことと、今の版の操作の申告が
    /// どれも解析の今の版であることを確かめる。
    /// フレームに持ち上げる操作（`Lift::PerFrame`）は申告できない。
    const NEEDS: &'static [(&'static str, u16)] = &[];

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
    /// この引数・入力の形で、実行器に先に解いてほしい解析（描かない。`NEEDS` にあるものだけ）。`apply` は
    /// `ApplyCx::analysis` で、ここで返したのと同じ（入力・ID・版・引数）の結果を受け取る。食い違うと実行器が内部の誤りにする。
    fn needs(_p: &Self::Params, _inputs: &[Shape]) -> Result<Vec<Need>, Issues> {
        Ok(Vec::new())
    }
    /// 描く間に、入力と出力の値のほかに使う作業領域の見積もり（§12 のメモリの予算。描く前に予算を確保するのに使う）。
    /// `inputs` は `infer` に渡すのと同じ形（持ち上げる操作はフレームの画像の形）、`out` は推論した出力の形。値を変えない。
    /// 既定は 0（タイル・ブロック 1 つぶんの一時の領域は数えない）。値の数倍の領域を持つ操作だけが上書きする。
    fn work_bytes(_p: &Self::Params, _inputs: &[Shape], _out: &Shape) -> Work {
        Work::ZERO
    }
    /// 実装を消した古い版 `from` の引数を、結果を変えずにこの版へ移す。移せなければ理由を返す。
    /// 呼ばれるのは、`from` より新しい実装のある版のうち、この版がいちばん古いとき（すぐ上の版）。
    fn migrate(from: u16, _raw: serde_json::Value) -> Result<serde_json::Value, String> {
        Err(format!("版 {from} からは移せません"))
    }
    /// 適合テストとゴールデンの例（1 つ以上）。
    fn examples() -> Vec<Example<Self::Params>>;
    /// この引数で描くときの数値の性質（既定は `signature().numeric`）。最近傍のリサイズのように、
    /// 引数によって整数だけで済む操作が `Exact` を返すと、その例のゴールデンはすべてのターゲットで照らす。
    fn numeric_for(_p: &Self::Params) -> Numeric {
        Self::signature().numeric
    }
    /// 追記だけの引数（描画）の差分の適用：`prev_out` が引数 `prev` の出力のとき、引数 `p` の出力を返す。
    /// `p` が `prev` に追記したものでなければ None（全体を当て直す）。結果は `apply` で全体を当て直したものと
    /// ビット単位で同じであること（E21。docs/editing.md §10）。警告は足した分だけを出す（前の分は実行器が前回の警告を
    /// 前に足す）。実行器（`render_cached`）は、メタデータを写す操作（`MetaAccess::Map`）で写し方が恒等（`Identity`）の
    /// ときだけ、前回の描画の出力（メタデータを写したあとの値。写す前の値と同じ）を `prev_out` に渡して使う。
    fn apply_appended(
        _prev: &Self::Params,
        _prev_out: &Value,
        _p: &Self::Params,
        _cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        None
    }
    /// 音声を出す操作の「核」：前計算を 1 度だけして、出力の区間を描く純粋な関数にしたもの（docs/editing.md §5.3）。
    /// 音声を出す操作は実装し、`apply` もこれを `audio::render_all` で描く。区間ごとに描いても値が同じ（E18）。
    /// 音声を出さない操作は None。
    fn audio_kernel(
        _p: &Self::Params,
        _inputs: &[&Value],
        _cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        None
    }
    /// 追記だけの引数の読み込み（段 10。`Engine::compile_snapshot` がセッションの追記に使う）：読んだ引数 `prev` の追記だけの
    /// 配列 `field` に、正規の形の要素 `items` を足した引数を、足した分だけ読んで作る。結果（読めない要素の指摘も）は、
    /// 足したあとの引数の JSON を `DynOp::parse` で読んだものと同じであること。足した分だけで読めなければ None（全体を読む）。
    fn parse_appended(
        _prev: &Self::Params,
        _field: &str,
        _items: &[&serde_json::Value],
    ) -> Option<Result<Self::Params, Issues>> {
        None
    }
    /// 追記だけの引数の推論（段 10）：`prev_inferred` が同じ入力の形での `prev` の推論の結果のとき、`p`（`prev` に追記したもの）の
    /// 推論を、足した分だけで求める。結果（指摘も）は `infer` と同じであること。`p` が `prev` への追記でない、または足した分
    /// だけで求められなければ None（全体を推論する）。
    fn infer_appended(
        _prev: &Self::Params,
        _prev_inferred: &Inferred,
        _p: &Self::Params,
        _inputs: &[Shape],
        _cx: &InferCx<'_>,
    ) -> Option<Result<Inferred, Issues>> {
        None
    }

    // 段 10：範囲と変わった範囲（render/region.rs・delta.rs・area.rs）。どれも既定のままで正しく、遅くなるだけ。

    /// 範囲の描画（`apply_region`）を実装した操作は true。偽の操作と、`reach` が `Global` の引数では、実行器は全体を描く
    /// （入力も全体で）。真の操作は、`reach` が `Global` でない引数で `apply_region` が必ず Some を返すこと。
    const REGION: bool = false;
    /// 出力が入力 0 のどこに依るか（範囲の描画の逆写しと、変わった範囲を前へ写すのに使う。保守的であること）。`inputs` は
    /// `infer` に渡すのと同じ形（持ち上げる操作はフレームの画像の形）。2 つ目からの入力には、いつも全体で依るとみなす。
    /// 既定は `Reach::Global`。
    fn reach(_p: &Self::Params, _inputs: &[Shape]) -> Reach {
        Reach::Global
    }
    /// 入力が同じとき、引数を `old` から `new` に変えて出力が変わりうる範囲（出力のキャンバスの座標。持ち上げる操作は
    /// 1 フレームの画像の座標で、実行器が全フレームに広げる）。既定は `Area::All`。
    fn param_change(_old: &Self::Params, _new: &Self::Params, _inputs: &[Shape]) -> Area {
        Area::All
    }
    /// 出力が入力 0 と違いうる範囲（外す・入れるときの変わった範囲。出力と入力 0 の形が同じときだけ使う）。既定は `Area::All`。
    fn effect(_p: &Self::Params, _inputs: &[Shape]) -> Area {
        Area::All
    }
    /// 範囲の描画：`tiles` のタイル（フレームごと。値のタイル）だけを `apply` と同じ値で描き、ほかのタイルは `base`（前の出力。
    /// 大きさ・形式・パレットが同じ）のタイルを共有するか、無効なタイル（読むと誤り）にする。入力は `reach` の逆写しで求めた
    /// 範囲（`tiles` の矩形から）を覆うタイルだけが有効で、その外を読むと誤りになる。メタデータ・写し方・警告は `apply` と
    /// 同じにする（範囲によらない）。持ち上げる操作は 1 フレームずつ呼ばれる。実装しない操作は None（既定）。
    fn apply_region(
        _p: &Self::Params,
        _inputs: &[&Value],
        _tiles: &FrameTiles,
        _base: Option<&Value>,
        _cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        None
    }

    // 段 10：プレビューの段（render/preview.rs）。既定のままで正しく、等倍で描く（鎖の継ぎ目になる）だけ。

    /// プレビューの段での扱い（`Preview::Scaled`：引数を段に縮めて描ける、`Preview::Skip`：画素を描かずメタデータを段の座標で
    /// 書く）。既定は `Preview::FullOnly`（等倍でしか描けない。ここまでを等倍で描き、出力を縮めてから後ろを段で描く）。
    fn preview(_p: &Self::Params) -> Preview {
        Preview::FullOnly
    }
    /// 引数を段 `level`（1〜3。各軸 1/2^level）に縮めたもの（`preview` が Scaled・Skip のときだけ呼ぶ）。`inputs` は段の入力の形、
    /// `full` は等倍の計画での同じノードの入力の形（どちらも `infer` に渡すのと同じ形）。矩形を入力の中に収める必要のある操作は、
    /// 段の入力に収める（clamp。docs/editing.md §7.1）。大きさを決める操作は、等倍の出力の大きさの ⌈n/2^level⌉ を目指す。
    /// 結果は段の形で推論し直す（通らなければ Scaled は計画を段 0 に落とし、Skip はこのノードを外す）。既定は同じ引数。
    fn preview_params(
        p: &Self::Params,
        _level: u8,
        _inputs: &[Shape],
        _full: &[Shape],
    ) -> Self::Params {
        p.clone()
    }
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
    /// 申告しうる解析（`Op::NEEDS`）。
    fn static_needs(&self) -> &'static [(&'static str, u16)];
    /// この引数・入力の形で先に解く解析（`Op::needs`）。
    fn needs(&self, p: &AnyParams, inputs: &[Shape]) -> Result<Vec<Need>, Issues>;
    /// 描く間の作業領域の見積もり（`Op::work_bytes`）。
    fn work_bytes(&self, p: &AnyParams, inputs: &[Shape], out: &Shape)
    -> Result<Work, EngineError>;
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
    /// この引数（JSON）で描くときの数値の性質。
    fn numeric(&self, raw: &serde_json::Value) -> Result<Numeric, Issues>;
    /// 追記だけの引数の差分の適用（`Op::apply_appended`）。
    fn apply_appended(
        &self,
        prev: &AnyParams,
        prev_out: &Value,
        p: &AnyParams,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>>;
    /// 音声の核（`Op::audio_kernel`）。
    fn audio_kernel(
        &self,
        p: &AnyParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>>;
    /// 追記だけの引数の読み込み（`Op::parse_appended`）。
    fn parse_appended(
        &self,
        prev: &AnyParams,
        field: &str,
        items: &[&serde_json::Value],
    ) -> Option<Result<Box<AnyParams>, Issues>>;
    /// 追記だけの引数の推論（`Op::infer_appended`）。
    fn infer_appended(
        &self,
        prev: &AnyParams,
        prev_inferred: &Inferred,
        p: &AnyParams,
        inputs: &[Shape],
        cx: &InferCx<'_>,
    ) -> Option<Result<Inferred, Issues>>;
    /// 範囲の描画を実装しているか（`Op::REGION`）。
    fn region(&self) -> bool;
    /// 出力が入力 0 のどこに依るか（`Op::reach`）。
    fn reach(&self, p: &AnyParams, inputs: &[Shape]) -> Result<Reach, EngineError>;
    /// 引数を変えたときに出力が変わりうる範囲（`Op::param_change`）。
    fn param_change(
        &self,
        old: &AnyParams,
        new: &AnyParams,
        inputs: &[Shape],
    ) -> Result<Area, EngineError>;
    /// 出力が入力 0 と違いうる範囲（`Op::effect`）。
    fn effect(&self, p: &AnyParams, inputs: &[Shape]) -> Result<Area, EngineError>;
    /// 範囲の描画（`Op::apply_region`）。
    fn apply_region(
        &self,
        p: &AnyParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>>;
    /// プレビューの段での扱い（`Op::preview`）。
    fn preview(&self, p: &AnyParams) -> Result<Preview, EngineError>;
    /// 引数を段に縮めたもの（`Op::preview_params`）。
    fn preview_params(
        &self,
        p: &AnyParams,
        level: u8,
        inputs: &[Shape],
        full: &[Shape],
    ) -> Result<Box<AnyParams>, EngineError>;
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

    fn static_needs(&self) -> &'static [(&'static str, u16)] {
        O::NEEDS
    }

    fn needs(&self, p: &AnyParams, inputs: &[Shape]) -> Result<Vec<Need>, Issues> {
        let p = self
            .params(p)
            .map_err(|e| Issues::one(Issue::error(IssueCode::BadParams, e.to_string())))?;
        O::needs(p, inputs)
    }

    fn work_bytes(
        &self,
        p: &AnyParams,
        inputs: &[Shape],
        out: &Shape,
    ) -> Result<Work, EngineError> {
        Ok(O::work_bytes(self.params(p)?, inputs, out))
    }

    fn parse(&self, raw: &serde_json::Value) -> Result<Box<AnyParams>, Issues> {
        // 範囲・列挙を先に見る（どの引数がおかしいかを示せる）
        let ranges = O::Params::schema().check_ranges(raw);
        if ranges.has_errors() {
            return Err(ranges);
        }
        let typed: O::Params =
            serde_json::from_value(raw.clone()).map_err(|e| unreadable_params(O::ID, &e))?;
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
        cx.declare(O::NEEDS);
        O::apply(self.params(p)?, inputs, cx)
    }

    fn migrate(&self, from: u16, raw: serde_json::Value) -> Result<serde_json::Value, String> {
        O::migrate(from, raw)
    }

    fn numeric(&self, raw: &serde_json::Value) -> Result<Numeric, Issues> {
        let p = self.parse(raw)?;
        let p = self
            .params(p.as_ref())
            .map_err(|e| Issues::one(Issue::error(IssueCode::BadParams, e.to_string())))?;
        Ok(O::numeric_for(p))
    }

    fn apply_appended(
        &self,
        prev: &AnyParams,
        prev_out: &Value,
        p: &AnyParams,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        let (prev, p) = match (self.params(prev), self.params(p)) {
            (Ok(prev), Ok(p)) => (prev, p),
            (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
        };
        cx.declare(O::NEEDS);
        O::apply_appended(prev, prev_out, p, cx)
    }

    fn audio_kernel(
        &self,
        p: &AnyParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        cx.declare(O::NEEDS);
        match self.params(p) {
            Ok(p) => O::audio_kernel(p, inputs, cx),
            Err(e) => Some(Err(e)),
        }
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

    fn parse_appended(
        &self,
        prev: &AnyParams,
        field: &str,
        items: &[&serde_json::Value],
    ) -> Option<Result<Box<AnyParams>, Issues>> {
        let prev = match self.params(prev) {
            Ok(p) => p,
            Err(e) => return Some(Err(bad_typed_params(e))),
        };
        O::parse_appended(prev, field, items).map(|r| r.map(|p| Box::new(p) as Box<AnyParams>))
    }

    fn infer_appended(
        &self,
        prev: &AnyParams,
        prev_inferred: &Inferred,
        p: &AnyParams,
        inputs: &[Shape],
        cx: &InferCx<'_>,
    ) -> Option<Result<Inferred, Issues>> {
        let (prev, p) = match (self.params(prev), self.params(p)) {
            (Ok(prev), Ok(p)) => (prev, p),
            (Err(e), _) | (_, Err(e)) => return Some(Err(bad_typed_params(e))),
        };
        O::infer_appended(prev, prev_inferred, p, inputs, cx)
    }

    fn region(&self) -> bool {
        O::REGION
    }

    fn reach(&self, p: &AnyParams, inputs: &[Shape]) -> Result<Reach, EngineError> {
        Ok(O::reach(self.params(p)?, inputs))
    }

    fn param_change(
        &self,
        old: &AnyParams,
        new: &AnyParams,
        inputs: &[Shape],
    ) -> Result<Area, EngineError> {
        Ok(O::param_change(
            self.params(old)?,
            self.params(new)?,
            inputs,
        ))
    }

    fn effect(&self, p: &AnyParams, inputs: &[Shape]) -> Result<Area, EngineError> {
        Ok(O::effect(self.params(p)?, inputs))
    }

    fn apply_region(
        &self,
        p: &AnyParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        cx.declare(O::NEEDS);
        match self.params(p) {
            Ok(p) => O::apply_region(p, inputs, tiles, base, cx),
            Err(e) => Some(Err(e)),
        }
    }

    fn preview(&self, p: &AnyParams) -> Result<Preview, EngineError> {
        Ok(O::preview(self.params(p)?))
    }

    fn preview_params(
        &self,
        p: &AnyParams,
        level: u8,
        inputs: &[Shape],
        full: &[Shape],
    ) -> Result<Box<AnyParams>, EngineError> {
        Ok(Box::new(O::preview_params(
            self.params(p)?,
            level,
            inputs,
            full,
        )))
    }
}

/// 操作を登録表の外で型を消した形にする（プレビューの段の継ぎ目の縮小など、実行器が計画の中にだけ置く操作）。
pub(crate) fn entry<O: Op>() -> Arc<dyn DynOp> {
    Arc::new(OpEntry::<O>(PhantomData))
}

/// 型の違う引数を渡された誤りを、推論・読み込みの指摘にする。
fn bad_typed_params(e: EngineError) -> Issues {
    Issues::one(Issue::error(IssueCode::BadParams, e.to_string()))
}

/// 引数の JSON を操作 `id` の型に読めなかったときの指摘（`DynOp::parse` と `Op::parse_appended` が同じ文言にする）。
pub fn unreadable_params(id: &str, e: &dyn std::fmt::Display) -> Issues {
    Issues::one(Issue::error(
        IssueCode::BadParams,
        format!("{id} の引数が読めません: {e}"),
    ))
}

/// 操作の記述（UI のカタログ）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpDescriptor {
    pub id: &'static str,
    pub version: u16,
    pub signature: Signature,
    pub schema: ParamSchema,
}

/// 版の解決の結果。どちらも、正規の JSON に往復させた引数を持つ。
pub enum Resolved {
    /// その版の実装がある（今の版か旧版）。
    Direct(Arc<dyn DynOp>, serde_json::Value),
    /// 実装のない古い版：すぐ上の実装のある版（旧版のこともある）へ移した引数と、その版の実装。
    Migrated(Arc<dyn DynOp>, serde_json::Value),
}

/// 引数を正規の JSON に往復させる（有限でない数などはエラー）。
pub fn canonical_params(raw: &serde_json::Value) -> Result<serde_json::Value, Issue> {
    crate::digest::canonical_json(raw)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .ok_or_else(|| {
            Issue::error(
                IssueCode::BadParams,
                "引数を正規の形にできません（有限でない数など）",
            )
        })
}

/// 引数の指紋のドメイン。
const PARAMS_DOMAIN: &str = "kura-params-v1";
/// 追記だけの配列の連鎖のドメイン。
const APPEND_DOMAIN: &str = "kura-param-append-v1";

/// 追記だけの配列の連鎖の始まり（要素が 0 個のとき）。
pub fn append_chain_start() -> Digest {
    Hasher::new(APPEND_DOMAIN).finish()
}

/// 追記だけの配列の連鎖に要素を 1 つ足す（`blake3(ドメイン ‖ 前 ‖ 要素の正規の JSON)`）。要素を足した分だけ求め直せる。
pub fn append_chain(prev: Digest, item: &serde_json::Value) -> Result<Digest, EngineError> {
    let bytes = crate::digest::canonical_json(item)?;
    Ok(Hasher::new(APPEND_DOMAIN)
        .raw(&prev.0)
        .bytes_with_len(&bytes)
        .finish())
}

/// 引数の指紋（段 10。ノードのキャッシュとセッションの鍵。メモリの中だけで使い、保存しない。版をまたいで保つ約束もない）。
///
/// `canonical` は正規の形の引数（`Registry::resolve` が返すもの）。追記だけの配列（[`ParamSchema::append_field`]）が
/// あれば、その配列を要素ごとの連鎖（[`append_chain`]）にし、残りの項目の正規の JSON と、要素の数と合わせる。ほかは
/// 正規の JSON をそのまま。同じ文書からは、実行器（compile）でもセッションでも同じ指紋になる。既定値を書いたかどうかの
/// ような書き方の違いは別の指紋になる（キャッシュが外れるだけで、誤りにはならない）。
pub fn params_digest(
    schema: &ParamSchema,
    canonical: &serde_json::Value,
) -> Result<Digest, EngineError> {
    let field = schema.append_field();
    let items = field.and_then(|f| canonical.get(f)?.as_array());
    let (Some(field), Some(items)) = (field, items) else {
        let bytes = crate::digest::canonical_json(canonical)?;
        return Ok(Hasher::new(PARAMS_DOMAIN)
            .u8(0)
            .bytes_with_len(&bytes)
            .finish());
    };
    let mut chain = append_chain_start();
    for item in items {
        chain = append_chain(chain, item)?;
    }
    params_digest_with_chain(canonical, field, chain, items.len() as u64)
}

/// 追記だけの配列をすでに連鎖にしたときの引数の指紋（[`params_digest`] と同じ値。追記のたびに全部を求め直さないため）。
/// `canonical` の `field` の中身は読まない。
pub fn params_digest_with_chain(
    canonical: &serde_json::Value,
    field: &str,
    chain: Digest,
    count: u64,
) -> Result<Digest, EngineError> {
    let rest = match canonical {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != field)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        other => other.clone(),
    };
    let bytes = crate::digest::canonical_json(&rest)?;
    Ok(Hasher::new(PARAMS_DOMAIN)
        .u8(1)
        .bytes_with_len(&bytes)
        .bytes_with_len(field.as_bytes())
        .raw(&chain.0)
        .u64(count)
        .finish())
}

/// ノードの鍵（段 10）：操作 ID・文書に書いた版・引数の指紋（[`params_digest`]。文書に書いた引数の正規の形から）・種・
/// 有効か。ノード ID と入力は含めない（ID は継ぎ足しでずれ、入力は実行器の指紋が足す）。同じ登録表なら、鍵が同じノードは
/// 同じ実装・同じ引数に解決される。セッションと実行器が同じ式で求める。メモリの中だけで使う。
pub fn node_key(op: &str, v: u16, params: &Digest, seed: u64, enabled: bool) -> Digest {
    Hasher::new("kura-node-key-v1")
        .bytes_with_len(op.as_bytes())
        .u32(u32::from(v))
        .raw(&params.0)
        .u64(seed)
        .u8(u8::from(enabled))
        .finish()
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

    /// なくした操作の最後の版を足す（カタログに出さず新しく作れないが、作り直しには使い、古い版の移行先にもなる。§8.4）。
    pub fn add_retired<O: Op>(&mut self) -> Result<(), String> {
        if self.current.contains_key(O::ID) {
            return Err(format!(
                "{} は今の版があるので、なくした操作にできません",
                O::ID
            ));
        }
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
        // 持ち上げた各フレームの値は計画の段階でないので、先に解く解析を申告できない
        if sig.lift == Lift::PerFrame && !O::NEEDS.is_empty() {
            return Err(format!(
                "{}：フレームに持ち上げる操作は解析を申告できません",
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

    /// その操作のその版の実装。
    pub(crate) fn get(&self, id: &str, v: u16) -> Option<Arc<dyn DynOp>> {
        self.ops
            .iter()
            .find(|((op, ver), _)| *op == id && *ver == v)
            .map(|(_, i)| i.clone())
    }

    /// ノードの（操作, 版, 引数）を実装に結び付ける（docs/editing.md §8.4）。
    /// - 引数はまず正規の JSON に往復させる（メモリ上の 16.0・-0.0 と、保存して読み直した 16・0 を同じ引数にする。`migrate` にもこれを渡す）。
    /// - その版の実装（今の版か旧版）があればそれを使う。
    /// - なければ、すぐ上の実装のある版の `migrate` で引数を移し、その版の実装で描く（旧版を経由する鎖も表せる）。
    /// - 知らない操作・実装のある最新の版より新しい版・移せない版はエラー（なくした操作も、実装が残っていれば作り直せる）。
    pub fn resolve(&self, op: &str, v: u16, params: &serde_json::Value) -> Result<Resolved, Issue> {
        let params = canonical_params(params)?;
        if let Some(imp) = self.get(op, v) {
            return Ok(Resolved::Direct(imp, params));
        }
        let versions = self.versions(op);
        let Some(max) = versions.iter().max().copied() else {
            return Err(Issue::error(
                IssueCode::UnknownOp,
                format!("知らない操作です（{op}）"),
            ));
        };
        if v > max {
            return Err(Issue::error(
                IssueCode::UnknownVersion,
                format!("{op} の版 {v} はこの版の Kura より新しい"),
            ));
        }
        let Some(imp) = versions
            .into_iter()
            .find(|ver| *ver > v)
            .and_then(|next| self.get(op, next))
        else {
            return Err(Issue::error(
                IssueCode::UnknownOp,
                format!("知らない操作です（{op}）"),
            ));
        };
        match imp.migrate(v, params) {
            Ok(moved) => Ok(Resolved::Migrated(imp, canonical_params(&moved)?)),
            Err(why) => Err(Issue::error(
                IssueCode::UnknownVersion,
                format!("{op} の版 {v} を移せません: {why}"),
            )),
        }
    }

    /// `resolve` が（操作, 版）に使う実装：その版の実装か、なければすぐ上の実装のある版のもの（引数は移さない）。
    fn implementation(&self, op: &str, v: u16) -> Option<Arc<dyn DynOp>> {
        self.get(op, v).or_else(|| {
            let next = self.versions(op).into_iter().find(|ver| *ver > v)?;
            self.get(op, next)
        })
    }

    /// 追記だけの配列の引数の名前（[`ParamSchema::append_field`]。段 10）。`resolve` が使う実装の記述から引く
    /// （セッションと実行器が同じ記述で引数の指紋を求めるため）。知らない操作・新しすぎる版は None。
    pub fn append_field(&self, op: &str, v: u16) -> Option<&'static str> {
        self.implementation(op, v)?.schema().append_field()
    }

    /// 文書のノードの鍵（[`node_key`]。引数の指紋は、文書に書いた引数の正規の形を `resolve` が使う実装の記述で
    /// [`params_digest`] にしたもの）。セッション（`session`）と実行器の鍵の基準。知らない操作は記述なしで求める。
    pub fn doc_node_key(&self, node: &crate::doc::Node) -> Result<Digest, EngineError> {
        let canonical = canonical_params(&node.params)
            .map_err(|i| EngineError::Invalid(Issues::one(i.at(node.id))))?;
        let schema = self
            .implementation(&node.op, node.v)
            .map(|imp| imp.schema())
            .unwrap_or_default();
        let params = params_digest(&schema, &canonical)?;
        Ok(node_key(&node.op, node.v, &params, node.seed, node.enabled))
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
        visible: None,
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

    fn strokes_schema(fields: usize) -> ParamSchema {
        let mut params = vec![spec(
            "size",
            ParamKind::Int {
                min: 1,
                max: 255,
                step: 1,
            },
            Unit::Px,
            serde_json::json!(1),
        )];
        for k in ["actions", "more"].into_iter().take(fields) {
            params.push(spec(
                k,
                ParamKind::Strokes,
                Unit::None,
                serde_json::json!([]),
            ));
        }
        ParamSchema { params }
    }

    /// ノードの鍵は、操作・版・引数・種・有効かのどれを変えても変わる。
    #[test]
    fn node_key_covers_every_part() {
        let p = append_chain_start();
        let q = append_chain(p, &serde_json::json!(1)).unwrap();
        let base = node_key("image.crop", 1, &p, 7, true);
        assert_eq!(base, node_key("image.crop", 1, &p, 7, true));
        for other in [
            node_key("image.flip", 1, &p, 7, true),
            node_key("image.crop", 2, &p, 7, true),
            node_key("image.crop", 1, &q, 7, true),
            node_key("image.crop", 1, &p, 8, true),
            node_key("image.crop", 1, &p, 7, false),
        ] {
            assert_ne!(base, other);
        }
    }

    /// 追記だけの配列の名前は、Strokes がちょうど 1 つのときだけ。
    #[test]
    fn append_field_needs_exactly_one_strokes() {
        assert_eq!(strokes_schema(0).append_field(), None);
        assert_eq!(strokes_schema(1).append_field(), Some("actions"));
        assert_eq!(strokes_schema(2).append_field(), None);
    }

    /// 引数の指紋：追記を連鎖で足しても全体から求めたものと同じ。要素・ほかの項目・要素の数で変わり、キーの順と空白では
    /// 変わらない。追記の配列のない引数も、同じ JSON なら同じ。
    #[test]
    fn params_digest_chains_appended_items() {
        let schema = strokes_schema(1);
        let items: Vec<serde_json::Value> = (0..5)
            .map(|i| serde_json::json!({"tool": "pen", "points": [[i, i + 1]], "color": i}))
            .collect();
        let full = |n: usize, size: i64| {
            let p = serde_json::json!({"size": size, "actions": items[..n].to_vec()});
            params_digest(&schema, &canonical_params(&p).unwrap()).unwrap()
        };
        let mut chain = append_chain_start();
        for (n, item) in items.iter().enumerate() {
            chain = append_chain(chain, item).unwrap();
            let p = serde_json::json!({"size": 3, "actions": []});
            let stepped = params_digest_with_chain(
                &canonical_params(&p).unwrap(),
                "actions",
                chain,
                n as u64 + 1,
            )
            .unwrap();
            assert_eq!(stepped, full(n + 1, 3), "{n}");
        }
        assert_ne!(full(5, 3), full(4, 3));
        assert_ne!(full(5, 3), full(5, 4));
        assert_ne!(full(0, 3), full(1, 3));
        let mut changed = items.clone();
        changed[2] = serde_json::json!({"tool": "pen", "points": [[9, 9]], "color": 2});
        let p = serde_json::json!({"size": 3, "actions": changed});
        assert_ne!(
            params_digest(&schema, &canonical_params(&p).unwrap()).unwrap(),
            full(5, 3)
        );
        let reordered: serde_json::Value =
            serde_json::from_str(r#"{ "actions": [], "size": 3 }"#).unwrap();
        assert_eq!(
            params_digest(&schema, &canonical_params(&reordered).unwrap()).unwrap(),
            full(0, 3)
        );
        let plain = strokes_schema(0);
        let a = serde_json::json!({"size": 3, "x": [1, 2]});
        let b: serde_json::Value = serde_json::from_str(r#"{"x":[1,2],"size":3}"#).unwrap();
        assert_eq!(
            params_digest(&plain, &canonical_params(&a).unwrap()).unwrap(),
            params_digest(&plain, &canonical_params(&b).unwrap()).unwrap()
        );
    }

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
            matches!(r.resolve("test.dummy", 2, &serde_json::json!({})), Ok(Resolved::Direct(i, _)) if i.version() == 2)
        );
        assert!(
            matches!(r.resolve("test.dummy", 3, &serde_json::json!({})), Ok(Resolved::Direct(i, _)) if i.version() == 3)
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

    /// なくした操作：今の版を外しても、なくした版（v3）と移行（v1 → v2 旧版）で作り直せる。カタログには出ない。
    #[test]
    fn retired_ops_still_replay() {
        let mut r = Registry::empty();
        r.add_retired::<V3>().unwrap();
        r.add_legacy::<V2>().unwrap();
        assert!(r.catalog().iter().all(|d| d.id != "test.dummy"));
        assert!(
            matches!(r.resolve("test.dummy", 3, &serde_json::json!({ "x": 1 })), Ok(Resolved::Direct(i, _)) if i.version() == 3)
        );
        assert!(
            matches!(r.resolve("test.dummy", 1, &serde_json::json!({ "old": 2 })), Ok(Resolved::Migrated(i, _)) if i.version() == 2)
        );
        assert!(
            matches!(r.resolve("test.dummy", 4, &serde_json::json!({})), Err(i) if i.code == IssueCode::UnknownVersion)
        );
        let mut both = Registry::empty();
        both.add_current::<V3>().unwrap();
        assert!(both.add_retired::<V3>().is_err());
    }

    /// 引数は正規の形に往復してから移行に渡る（16.0 と 16 で同じ結果）。
    #[test]
    fn params_are_canonical_before_migration() {
        let r = table();
        let a = r.resolve("test.dummy", 1, &serde_json::json!({ "old": 4.0 }));
        let b = r.resolve("test.dummy", 1, &serde_json::json!({ "old": 4 }));
        match (a, b) {
            (Ok(Resolved::Migrated(_, x)), Ok(Resolved::Migrated(_, y))) => {
                assert_eq!(x, y);
                assert_eq!(x, serde_json::json!({ "x": 4 }));
            }
            _ => panic!(),
        }
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
