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
    write_canonical(value, &mut out, usize::MAX)?;
    Ok(out)
}

/// 直列化できる値を正規の JSON にする。入れ子は `MAX_DEPTH` 段まで。
pub fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, EngineError> {
    canonical_within(value, usize::MAX)
}

/// 正規の JSON の入れ子の深さ（配列・オブジェクトの段）の上限。文書の読み込み（serde_json の 128 段）より浅くして、
/// 保存できた文書は必ず読めるようにする。直列化も再帰するので、深い値でスタックを使い切る前に断る。
pub const MAX_DEPTH: usize = 100;

/// 直列化できる値を正規の JSON にする。`max` バイトを超えるものは、先に長さを数えて（書き出さずに）`Limit` にする
/// （数・文字の書き方で元の JSON より何十倍にも膨らむ文書を、膨らませずに断る）。入れ子は `MAX_DEPTH` 段まで。
pub fn canonical_within<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, EngineError> {
    canonical_len_within(value, max)?;
    let v = serde_json::to_value(value)
        .map_err(|e| EngineError::Internal(format!("JSON にできません: {e}")))?;
    let mut out = Vec::new();
    write_canonical(&v, &mut out, max)?;
    Ok(out)
}

/// 正規の JSON の長さ（バイト）を、書き出さずに数える。キーの並びは長さに効かないので、serde_json の直列化器に
/// 正規の数と文字の書き方（`CanonicalFormatter`）を付けて、書いたバイト数を数える（値の写しも出力の確保も作らない）。
/// `max` バイトか `MAX_DEPTH` 段を超えたら、その時点でやめて `Limit`。
pub fn canonical_len_within<T: Serialize>(value: &T, max: usize) -> Result<usize, EngineError> {
    let deep = std::cell::Cell::new(false);
    let mut count = Counter { n: 0, max };
    let result = {
        let mut ser = serde_json::Serializer::with_formatter(
            &mut count,
            CanonicalFormatter {
                depth: 0,
                deep: &deep,
            },
        );
        value.serialize(&mut ser)
    };
    if deep.get() {
        return Err(EngineError::Limit(format!(
            "入れ子が深すぎます（上限 {MAX_DEPTH} 段）"
        )));
    }
    if count.n > max {
        return Err(EngineError::Limit(format!(
            "正規の JSON が大きすぎます（上限 {max} バイト）"
        )));
    }
    result.map_err(|e| EngineError::Internal(format!("JSON にできません: {e}")))?;
    Ok(count.n)
}

/// 書いたバイト数を数えるだけの書き込み先。`max` を超えたら書き込みの誤りにして直列化を止める。
struct Counter {
    n: usize,
    max: usize,
}

impl std::io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.n = self.n.saturating_add(buf.len());
        if self.n > self.max {
            return Err(std::io::Error::other("上限を超えました"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 正規の JSON と同じ長さになる書き方（`write_canonical` と同じ数・文字の書き方。空白なし）。深さも数える。
struct CanonicalFormatter<'a> {
    depth: usize,
    deep: &'a std::cell::Cell<bool>,
}

impl CanonicalFormatter<'_> {
    fn enter<W: ?Sized + std::io::Write>(&mut self, w: &mut W, open: &[u8]) -> std::io::Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.deep.set(true);
            return Err(std::io::Error::other("入れ子が深すぎます"));
        }
        w.write_all(open)
    }
}

impl serde_json::ser::Formatter for CanonicalFormatter<'_> {
    fn begin_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.enter(w, b"[")
    }

    fn end_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.depth = self.depth.saturating_sub(1);
        w.write_all(b"]")
    }

    fn begin_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.enter(w, b"{")
    }

    fn end_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.depth = self.depth.saturating_sub(1);
        w.write_all(b"}")
    }

    fn write_f64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: f64) -> std::io::Result<()> {
        // write_number と同じ：-0 は 0、表示は往復できる最短の 10 進（指数表記なし）
        let v = if v == 0.0 { 0.0 } else { v };
        write!(w, "{v}")
    }

    fn write_f32<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: f32) -> std::io::Result<()> {
        // serde_json::to_value は f32 を f64 にして持つ
        self.write_f64(w, f64::from(v))
    }

    fn write_char_escape<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        escape: serde_json::ser::CharEscape,
    ) -> std::io::Result<()> {
        use serde_json::ser::CharEscape as E;
        match escape {
            E::Quote => w.write_all(b"\\\""),
            E::ReverseSolidus => w.write_all(b"\\\\"),
            E::Solidus => w.write_all(b"/"),
            E::LineFeed => w.write_all(b"\\n"),
            E::CarriageReturn => w.write_all(b"\\r"),
            E::Tab => w.write_all(b"\\t"),
            E::Backspace => w.write_all(&unicode_escape(0x08)),
            E::FormFeed => w.write_all(&unicode_escape(0x0c)),
            E::AsciiControl(b) => w.write_all(&unicode_escape(b)),
        }
    }
}

/// 制御文字の `\u00XX`（16 進は小文字）。
fn unicode_escape(b: u8) -> [u8; 6] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    [
        b'\\',
        b'u',
        b'0',
        b'0',
        HEX[usize::from(b >> 4)],
        HEX[usize::from(b & 0xf)],
    ]
}

/// 正規の JSON のハッシュ（ドメイン付き）。
pub fn hash_canonical<T: Serialize>(domain: &str, value: &T) -> Result<Digest, EngineError> {
    let bytes = canonical(value)?;
    Ok(Hasher::new(domain).bytes_with_len(&bytes).finish())
}

fn write_canonical(
    value: &serde_json::Value,
    out: &mut Vec<u8>,
    max: usize,
) -> Result<(), EngineError> {
    use serde_json::Value as J;
    if out.len() > max {
        return Err(EngineError::Limit(format!(
            "正規の JSON が大きすぎます（上限 {max} バイト）"
        )));
    }
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
                write_canonical(item, out, max)?;
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
                    write_canonical(v, out, max)?;
                }
            }
            out.push(b'}');
        }
    }
    if out.len() > max {
        return Err(EngineError::Limit(format!(
            "正規の JSON が大きすぎます（上限 {max} バイト）"
        )));
    }
    Ok(())
}

fn write_number(n: &serde_json::Number, out: &mut Vec<u8>) -> Result<(), EngineError> {
    use std::io::Write as _;
    let written = if let Some(i) = n.as_i64() {
        write!(out, "{i}")
    } else if let Some(u) = n.as_u64() {
        write!(out, "{u}")
    } else if let Some(f) = n.as_f64() {
        if !f.is_finite() {
            return Err(EngineError::Internal(
                "有限でない数は正規の JSON にできません".into(),
            ));
        }
        // Rust の f64 の表示は「往復できる最短の 10 進」で、指数表記を使わない。整数の値は小数点なし。
        let f = if f == 0.0 { 0.0 } else { f };
        write!(out, "{f}")
    } else {
        return Err(EngineError::Internal("読めない数です".into()));
    };
    written.map_err(|e| EngineError::Internal(format!("数を書けません: {e}")))
}

/// 文字列を書く。逃がすのは `"`・`\`・制御文字（改行・復帰・タブは短い形、ほかは `\u00XX`）だけ。
/// 逃がしの要らない連続はまとめて写す（多バイト文字のバイトは 0x80 以上なので、バイトで見てよい）。
fn write_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escaped: &[u8] = match b {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x00..=0x1f => &unicode_escape(b),
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..i]);
        out.extend_from_slice(escaped);
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 乱数の JSON の値（制御文字・逃がす文字・多バイト文字の文字列、整数の端、小数・巨大な数・-0、入れ子）。
    fn random_value(r: &mut rand_chacha::ChaCha8Rng, depth: u32) -> serde_json::Value {
        use rand_chacha::rand_core::Rng;
        let pick = r.next_u32() % if depth > 4 { 5 } else { 7 };
        match pick {
            0 => serde_json::Value::Null,
            1 => serde_json::Value::Bool(r.next_u32().is_multiple_of(2)),
            2 => {
                let ends = [i64::MIN, i64::MAX, 0, -1, 1];
                json!(ends[(r.next_u32() % 5) as usize].wrapping_add(i64::from(r.next_u32() % 3)))
            }
            3 => {
                let f = [
                    0.1,
                    -0.0,
                    1e300,
                    -2.5e-300,
                    f64::from(0.1f32),
                    2.0,
                    u64::MAX as f64,
                    f64::from_bits(r.next_u64()),
                ][(r.next_u32() % 8) as usize];
                serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, Into::into)
            }
            4 => {
                let parts = [
                    "a", "\"", "\\", "\n", "\r", "\t", "\u{8}", "\u{c}", "\u{1}", "\u{1f}",
                    "\u{7f}", "あ", "😀", "/", "\u{2028}",
                ];
                let n = r.next_u32() % 8;
                json!(
                    (0..n)
                        .map(|_| parts[(r.next_u32() % 15) as usize])
                        .collect::<String>()
                )
            }
            5 => serde_json::Value::Array(
                (0..r.next_u32() % 4)
                    .map(|_| random_value(r, depth + 1))
                    .collect(),
            ),
            _ => {
                let mut m = serde_json::Map::new();
                for k in 0..r.next_u32() % 4 {
                    m.insert(format!("k{k}\u{1}é"), random_value(r, depth + 1));
                }
                serde_json::Value::Object(m)
            }
        }
    }

    /// 書き出さずに数えた長さは、書き出した正規の JSON の長さと必ず一致する（読み込みと保存の判定に使う）。
    #[test]
    fn counted_length_matches_the_written_form() {
        let mut r = crate::determinism::rng(7);
        for _ in 0..5_000 {
            let v = random_value(&mut r, 0);
            let bytes = canonical(&v).unwrap();
            assert_eq!(
                canonical_len_within(&v, usize::MAX).unwrap(),
                bytes.len(),
                "{v}"
            );
            // 上限ちょうどは通り、1 バイト足りなければ断る
            assert!(canonical_within(&v, bytes.len()).is_ok());
            if !bytes.is_empty() {
                assert!(matches!(
                    canonical_len_within(&v, bytes.len() - 1),
                    Err(EngineError::Limit(_))
                ));
            }
        }
        // f32 は f64 にして持つ（serde_json::to_value と同じ）
        let x = (0.1f32, -0.0f32, 3u8, -7i128, u128::from(u64::MAX), 'x');
        assert_eq!(
            canonical_len_within(&x, usize::MAX).unwrap(),
            canonical(&x).unwrap().len()
        );
    }

    /// 入れ子は MAX_DEPTH 段まで。深いものは、直列化の途中で断る（スタックを使い切らない）。
    #[test]
    fn deep_values_are_refused() {
        let nest = |n: usize| {
            let mut v = json!(1);
            for _ in 0..n {
                v = json!([v]);
            }
            v
        };
        assert!(canonical(&nest(MAX_DEPTH)).is_ok());
        assert!(matches!(
            canonical(&nest(MAX_DEPTH + 1)),
            Err(EngineError::Limit(_))
        ));
        assert!(matches!(
            canonical_len_within(&nest(MAX_DEPTH + 1), usize::MAX),
            Err(EngineError::Limit(_))
        ));
        // 長い文字列は、書き出す前に数えて断る（逃がしで 3 倍に膨らむ "\u{8}"）
        let big = json!("\u{8}".repeat(1 << 20));
        assert!(matches!(
            canonical_within(&big, 1 << 21),
            Err(EngineError::Limit(_))
        ));
    }

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
