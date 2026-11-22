//! 解析の結果の正規の形（docs/editing.md §11）。
//!
//! - 結果の型は `AnalysisOutput` を実装する。既定は正規の JSON（`json_output!`）。波形の段のような大きな数の列だけは
//!   バイナリにしてよい（`Encoding::Binary`）。正規の形のバイト列が、ダイジェスト・ゴールデン・DB の値になる。
//! - 正規の JSON は有限でない数を書けない（`digest::canonical` がエラーにする）。小数で NaN・±∞ になりうる欄は `Real`
//!   （有限は数、NaN は `"nan"`、+∞ は `"inf"`、−∞ は `"-inf"`）、値がないときに理由を示したい欄は `Reasoned`
//!   （値は `{"kind":"value","v":…}`、理由は `{"kind":"<理由>"}`）で持つ。
//! - 正規の JSON では -0 は 0 になる（`digest`）。読み直すと -0 のビットは戻らないので、往復はバイト列で比べる。
//!   操作は DB から読み直した結果を使わない（実行器が解いたメモリ上の値だけ）ので、操作の結果には効かない。

use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, DeserializeOwned, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::EngineError;

/// 結果の正規の形の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Encoding {
    /// 正規の JSON（`digest::canonical`）。
    Json,
    /// 解析が決めたバイナリの並び（波形の段）。
    Binary,
}

impl Encoding {
    /// ダイジェストに書く番号（凍結する）。
    pub fn tag(self) -> u8 {
        match self {
            Self::Json => 0,
            Self::Binary => 1,
        }
    }
}

/// 解析の結果の型。正規の形（ダイジェスト・ゴールデン・保存に使うバイト列）を決める。
pub trait AnalysisOutput: fmt::Debug + PartialEq + Send + Sync + Sized + 'static {
    /// 正規の形の種類。
    const ENCODING: Encoding;
    /// 正規の形にする。
    fn to_canonical(&self) -> Result<Vec<u8>, EngineError>;
    /// 正規の形から読む（往復のテストと、kura-core が DB から読むとき。操作はこれを使わない）。
    /// 外から来たバイト列として扱い、壊れていればエラーにする（panic しない）。
    fn from_canonical(bytes: &[u8]) -> Result<Self, EngineError>;
}

/// 正規の JSON にする（`json_output!` が使う）。有限でない数があればエラー。
pub fn json_to_canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, EngineError> {
    crate::digest::canonical(value)
}

/// 正規の JSON から読む（`json_output!` が使う）。
pub fn json_from_canonical<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, EngineError> {
    serde_json::from_slice(bytes).map_err(|e| EngineError::Decode {
        codec: "kura-analysis".into(),
        message: format!("解析の結果が読めません: {e}"),
    })
}

/// 結果を正規の JSON で表す型に `AnalysisOutput` を付ける（型は `Serialize`・`Deserialize`、知らない項目を拒む）。
#[macro_export]
macro_rules! json_output {
    ($($t:ty),* $(,)?) => {$(
        impl $crate::analysis::AnalysisOutput for $t {
            const ENCODING: $crate::analysis::Encoding = $crate::analysis::Encoding::Json;
            fn to_canonical(&self) -> Result<Vec<u8>, $crate::EngineError> {
                $crate::analysis::output::json_to_canonical(self)
            }
            fn from_canonical(bytes: &[u8]) -> Result<Self, $crate::EngineError> {
                $crate::analysis::output::json_from_canonical(bytes)
            }
        }
    )*};
}

/// 有限でない値も持てる小数。正規の JSON では、有限は数（-0 は 0）、NaN は `"nan"`、+∞ は `"inf"`、−∞ は `"-inf"`。
/// 比較は値で行い、NaN どうしは等しいとみなす（-0 と 0 も等しい）。TS の型は手で書く（`number | "nan" | "inf" | "-inf"`。
/// `Serialize` が手書きなので derive できない）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Real(pub f64);

impl Real {
    /// 中の値。
    pub fn get(self) -> f64 {
        self.0
    }

    /// 有限ならその値。
    pub fn finite(self) -> Option<f64> {
        self.0.is_finite().then_some(self.0)
    }
}

impl From<f64> for Real {
    fn from(v: f64) -> Self {
        Self(v)
    }
}

impl PartialEq for Real {
    fn eq(&self, other: &Self) -> bool {
        (self.0.is_nan() && other.0.is_nan()) || self.0 == other.0
    }
}

impl Serialize for Real {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        if v.is_nan() {
            s.serialize_str("nan")
        } else if v == f64::INFINITY {
            s.serialize_str("inf")
        } else if v == f64::NEG_INFINITY {
            s.serialize_str("-inf")
        } else {
            s.serialize_f64(v)
        }
    }
}

impl<'de> Deserialize<'de> for Real {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Real;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("有限の数か \"nan\"・\"inf\"・\"-inf\"")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Real, E> {
                Ok(Real(v))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Real, E> {
                // 整数の値の小数は小数点なしで書く。f64 から書いた値なので、f64 に戻すとちょうど元に戻る
                Ok(Real(v as f64))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Real, E> {
                Ok(Real(v as f64))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Real, E> {
                match v {
                    "nan" => Ok(Real(f64::NAN)),
                    "inf" => Ok(Real(f64::INFINITY)),
                    "-inf" => Ok(Real(f64::NEG_INFINITY)),
                    _ => Err(E::invalid_value(de::Unexpected::Str(v), &self)),
                }
            }
        }
        d.deserialize_any(V)
    }
}

/// 値か、値がない理由。`R` は理由の列挙（単位の variant だけ。`#[serde(rename_all = "camelCase")]` で名前を文字列にする。
/// `"value"` という名前の理由は使えない）。正規の JSON では、値は `{"kind":"value","v":…}`、理由は `{"kind":"<理由>"}`。
/// 「計れない（短い・レートが低い）」と「無音」のように、表示で分けたい理由を型で持つ（`Real` の −∞ だけでは区別できない）。
///
/// TS に出すとき（tauri-specta）：`Serialize`・`Deserialize` を手で書いているので、derive した型はこの形にならない。
/// 型は手で書く。判別子 `kind` の合併型で、値の枝と、理由ごとの枝（`R` の variant を camelCase にした文字列）を並べる：
/// `{ kind: "value"; v: T } | { kind: "<理由 1>" } | { kind: "<理由 2>" } | …`。`audio.loudness` の `Level`
/// （`Reasoned<f64, Unmeasured>`）なら `{ kind: "value"; v: number } | { kind: "tooShort" } | { kind: "silent" } |
/// { kind: "belowGate" } | { kind: "lowRate" } | { kind: "nonFinite" }`。`v` が `Real` のときは
/// `number | "nan" | "inf" | "-inf"`（`Real` も手書きの `Serialize` なので、同じく手で書く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reasoned<T, R> {
    /// 値。
    Value(T),
    /// 値がない理由。
    Reason(R),
}

impl<T, R> Reasoned<T, R> {
    /// 値があればその値。
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Value(v) => Some(v),
            Self::Reason(_) => None,
        }
    }

    /// 理由があればその理由。
    pub fn reason(&self) -> Option<&R> {
        match self {
            Self::Value(_) => None,
            Self::Reason(r) => Some(r),
        }
    }
}

impl<T: Serialize, R: Serialize> Serialize for Reasoned<T, R> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(v) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("kind", "value")?;
                m.serialize_entry("v", v)?;
                m.end()
            }
            Self::Reason(r) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("kind", r)?;
                m.end()
            }
        }
    }
}

impl<'de, T: Deserialize<'de>, R: Deserialize<'de>> Deserialize<'de> for Reasoned<T, R> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T, R>(PhantomData<fn() -> (T, R)>);
        impl<'de, T: Deserialize<'de>, R: Deserialize<'de>> Visitor<'de> for V<T, R> {
            type Value = Reasoned<T, R>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("{\"kind\":\"value\",\"v\":…} か {\"kind\":\"<理由>\"}")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let mut kind: Option<String> = None;
                let mut v: Option<T> = None;
                while let Some(key) = m.next_key::<String>()? {
                    match key.as_str() {
                        "kind" if kind.is_none() => kind = Some(m.next_value()?),
                        "v" if v.is_none() => v = Some(m.next_value()?),
                        "kind" | "v" => {
                            return Err(de::Error::custom(format!("{key} が 2 つあります")));
                        }
                        other => return Err(de::Error::unknown_field(other, &["kind", "v"])),
                    }
                }
                let kind = kind.ok_or_else(|| de::Error::missing_field("kind"))?;
                if kind == "value" {
                    return v
                        .map(Reasoned::Value)
                        .ok_or_else(|| de::Error::missing_field("v"));
                }
                if v.is_some() {
                    return Err(de::Error::custom("理由には値を付けません"));
                }
                R::deserialize(de::value::StringDeserializer::<A::Error>::new(kind))
                    .map(Reasoned::Reason)
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::canonical;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    enum Why {
        TooShort,
        LowRate,
    }

    #[test]
    fn reals_write_non_finite_values_as_words_and_round_trip() {
        let values = [
            0.0,
            -0.0,
            1.5,
            -2.25e-300,
            1e21,
            f64::MAX,
            f64::MIN_POSITIVE,
            5e-324,
        ];
        for v in values {
            let bytes = canonical(&Real(v)).unwrap();
            let back: Real = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(back, Real(v));
            // -0 は 0 に寄せる。ほかはビットまで戻る
            if v != 0.0 {
                assert_eq!(back.0.to_bits(), v.to_bits(), "{v}");
            }
        }
        for (v, text) in [
            (f64::NAN, "\"nan\""),
            (f64::INFINITY, "\"inf\""),
            (f64::NEG_INFINITY, "\"-inf\""),
            (-0.0, "0"),
            (3.0, "3"),
        ] {
            assert_eq!(
                String::from_utf8(canonical(&Real(v)).unwrap()).unwrap(),
                text
            );
            let back: Real = serde_json::from_str(text).unwrap();
            assert_eq!(back, Real(v));
        }
        assert!(serde_json::from_str::<Real>("\"NaN\"").is_err());
        assert!(serde_json::from_str::<Real>("null").is_err());
        assert_eq!(Real(f64::NAN), Real(f64::NAN));
        assert_eq!(Real(f64::NAN).finite(), None);
        assert_eq!(Real(2.0).finite(), Some(2.0));
    }

    #[test]
    fn reasoned_values_have_a_kind_and_reject_unknown_shapes() {
        type L = Reasoned<f64, Why>;
        let cases: [(L, &str); 3] = [
            (Reasoned::Value(-23.5), r#"{"kind":"value","v":-23.5}"#),
            (Reasoned::Reason(Why::TooShort), r#"{"kind":"tooShort"}"#),
            (Reasoned::Reason(Why::LowRate), r#"{"kind":"lowRate"}"#),
        ];
        for (v, text) in cases {
            assert_eq!(String::from_utf8(canonical(&v).unwrap()).unwrap(), text);
            assert_eq!(serde_json::from_str::<L>(text).unwrap(), v);
        }
        for bad in [
            r#"{"kind":"value"}"#,
            r#"{"kind":"tooShort","v":1}"#,
            r#"{"kind":"silent"}"#,
            r#"{"v":1}"#,
            r#"{"kind":"value","v":1,"x":2}"#,
            r#"{"kind":"value","kind":"value","v":1}"#,
            r#"[1]"#,
        ] {
            assert!(serde_json::from_str::<L>(bad).is_err(), "{bad}");
        }
        // 有限でない値を値として持つと、正規の JSON にできない（Real を使う）
        assert!(canonical(&L::Value(f64::NAN)).is_err());
        let r: Reasoned<Real, Why> = Reasoned::Value(Real(f64::NEG_INFINITY));
        assert_eq!(
            String::from_utf8(canonical(&r).unwrap()).unwrap(),
            r#"{"kind":"value","v":"-inf"}"#
        );
    }
}
