//! feature `specta` のときだけ使う TS の型の補助（エディタとの境界の型を TS に出す。docs/editing.md §3、ADR-134）。
//!
//! specta の `serde_json::Value` の型は入れ子を展開する（inline）ので、引数の JSON（`Node.params`・手の値など）をそのまま
//! 出すと TS の型が無限になって書き出せない。名前の付いた再帰の型 [`JsonValue`] を `#[specta(type = …)]` で当てる。
//! 値の読み書き（serde）には使わない（型の名前を出すだけ）。

use std::collections::BTreeMap;

/// 任意の JSON の値（TS の `JsonValue`：null・真偽・数・文字列・配列・オブジェクト）。
#[derive(serde::Serialize, specta::Type)]
#[serde(untagged)]
#[allow(dead_code)] // 型を TS に出すためだけのもの（値を作らない）
pub enum JsonValue {
    Null(()),
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}
