//! ダイジェストと正規の符号化（docs/editing.md §4.4・§8.6）。
//!
//! - 値のダイジェストは、ドメイン（`"kura-img-v1"` など）で始めた BLAKE3。並列のハッシュも木の形なので決定的。
//! - 文書・引数のハッシュは、自前の正規化 JSON（キーを UTF-8 のバイト順に並べ、空白なし、浮動小数は最短の往復表記）で取る。
//!   `serde_json::Value` の並び（依存の feature で変わりうる）には頼らない。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::EngineError;

/// 32 バイトのダイジェスト。文字列では小文字の 16 進。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// 16 進の文字列にする。
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 64 桁の小文字の 16 進から読む。
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 64
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(out))
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::from_hex(&text)
            .ok_or_else(|| serde::de::Error::custom("64 桁の小文字の 16 進ではありません"))
    }
}

/// ドメインを分けたハッシュの書き手。数は固定の幅のリトルエンディアンで書く。
pub struct Hasher(blake3::Hasher);

impl Hasher {
    /// ドメイン（例 `"kura-img-v1"`）で始める。
    pub fn new(domain: &str) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(&(domain.len() as u64).to_le_bytes());
        h.update(domain.as_bytes());
        Self(h)
    }

    /// バイト列をそのまま足す（長さは足さない。可変長のものは `bytes_with_len` を使う）。
    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update(bytes);
        self
    }

    /// 長さ付きのバイト列を足す。
    pub fn bytes_with_len(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update(&(bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
        self
    }

    /// u8 を足す。
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.update(&[v]);
        self
    }

    /// u32 を足す。
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }

    /// u64 を足す。
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }

    /// 終える。
    pub fn finish(&self) -> Digest {
        Digest(*self.0.finalize().as_bytes())
    }
}

/// JSON の値を正規の形のバイト列にする（キーを並べ、空白なし）。NaN・無限大は値にできない。
pub fn canonical_json(value: &serde_json::Value) -> Result<Vec<u8>, EngineError> {
    let mut out = Vec::new();
    write_canonical(value, &mut out)?;
    Ok(out)
}

/// 直列化できる値を正規の JSON にする。
pub fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, EngineError> {
    let v = serde_json::to_value(value)
        .map_err(|e| EngineError::Internal(format!("JSON にできません: {e}")))?;
    canonical_json(&v)
}

/// 正規の JSON のハッシュ（ドメイン付き）。
pub fn hash_canonical<T: Serialize>(domain: &str, value: &T) -> Result<Digest, EngineError> {
    let bytes = canonical(value)?;
    Ok(Hasher::new(domain).bytes_with_len(&bytes).finish())
}

fn write_canonical(value: &serde_json::Value, out: &mut Vec<u8>) -> Result<(), EngineError> {
    use serde_json::Value as J;
    match value {
        J::Null => out.extend_from_slice(b"null"),
        J::Bool(true) => out.extend_from_slice(b"true"),
        J::Bool(false) => out.extend_from_slice(b"false"),
        J::Number(n) => write_number(n, out)?,
        J::String(s) => write_string(s, out),
        J::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out)?;
            }
            out.push(b']');
        }
        J::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            out.push(b'{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                if let Some(v) = map.get(key) {
                    write_canonical(v, out)?;
                }
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn write_number(n: &serde_json::Number, out: &mut Vec<u8>) -> Result<(), EngineError> {
    if let Some(i) = n.as_i64() {
        out.extend_from_slice(i.to_string().as_bytes());
    } else if let Some(u) = n.as_u64() {
        out.extend_from_slice(u.to_string().as_bytes());
    } else if let Some(f) = n.as_f64() {
        if !f.is_finite() {
            return Err(EngineError::Internal(
                "有限でない数は正規の JSON にできません".into(),
            ));
        }
        // Rust の f64 の表示は「往復できる最短の 10 進」で、指数表記を使わない。整数の値は小数点なし。
        let f = if f == 0.0 { 0.0 } else { f };
        out.extend_from_slice(format!("{f}").as_bytes());
    } else {
        return Err(EngineError::Internal("読めない数です".into()));
    }
    Ok(())
}

fn write_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes())
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_and_drops_spaces() {
        let v =
            json!({ "b": 1, "a": [true, null, "x\"y"], "c": { "z": 1.5, "y": -0.0, "x": 2.0 } });
        let bytes = canonical_json(&v).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"a":[true,null,"x\"y"],"b":1,"c":{"x":2,"y":0,"z":1.5}}"#
        );
    }

    #[test]
    fn canonical_json_keeps_unicode_and_escapes_controls() {
        let v = json!({ "名前": "ドット\u{1}" });
        assert_eq!(
            String::from_utf8(canonical_json(&v).unwrap()).unwrap(),
            "{\"名前\":\"ドット\\u0001\"}"
        );
    }

    #[test]
    fn floats_use_shortest_round_trip_without_exponent() {
        let v = json!([0.1, 1e21, 123.0, -2.5e-7]);
        assert_eq!(
            String::from_utf8(canonical_json(&v).unwrap()).unwrap(),
            "[0.1,1000000000000000000000,123,-0.00000025]"
        );
    }

    /// 正規の JSON に書いた小数を読み直すと、ビット単位で同じ値に戻る（serde_json の float_roundtrip が要る）。
    #[test]
    fn floats_survive_write_and_read() {
        let mut r = crate::determinism::rng(42);
        use rand_chacha::rand_core::Rng;
        let mut values = vec![
            31.447_295_999_999_99,
            0.1,
            1e-300,
            123_456.789,
            -0.000_123,
            f64::MAX,
            f64::MIN_POSITIVE,
        ];
        values.extend(
            (0..10_000)
                .map(|_| f64::from_bits(r.next_u64() >> 2))
                .filter(|v| v.is_finite()),
        );
        for v in values {
            let bytes = canonical_json(&serde_json::json!(v)).unwrap();
            let back: f64 = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                back.to_bits(),
                v.to_bits(),
                "{v} -> {}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }

    #[test]
    fn hex_round_trip_and_domains_differ() {
        let a = Hasher::new("a").u32(1).finish();
        let b = Hasher::new("b").u32(1).finish();
        assert_ne!(a, b);
        assert_eq!(Digest::from_hex(&a.to_hex()), Some(a));
        assert_eq!(Digest::from_hex("AB"), None);
        assert_eq!(Digest::from_hex(&"G".repeat(64)), None);
    }
}
