//! 筆の点の列の詰め方（docs/editing.md §10）。文書を小さくするため、点の列は 1 つの文字列にして保存する。凍結する。
//!
//! - 各値は zigzag（`(v << 1) ^ (v >> 63)`）で符号なしにして、LEB128（7 ビットずつ下から、続くときは最上位を 1）で詰める。
//! - 並びは、最初の点の x・y（そのまま）、2 つ目からは前の点との差 dx・dy。
//! - バイト列は base64url（RFC 4648 §5。`A–Z a–z 0–9 - _`）で、`=` を付けない。
//! - 読むときは、詰め直して同じ文字列になるものだけを受け付ける（同じ点の列の書き方を 1 つにする）。座標は ±2^31 まで。
//!   フロント（TS）も同じ規則で詰める（`src/lib/paint.ts`、共有のベクタ `tests/vectors/paint/`）。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::geom::{IPoint, coord_ok};

/// 点の列（画素の整数座標）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PointList(pub Vec<IPoint>);

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn get_varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *bytes.get(*at)?;
        *at += 1;
        v |= u64::from(b & 0x7f).checked_shl(shift)?;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.len();
        let v = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..=n {
            out.push(char::from(ALPHABET[((v >> (18 - 6 * i)) & 0x3f) as usize]));
        }
    }
    out
}

fn unbase64(text: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| ALPHABET.iter().position(|a| *a == c).map(|i| i as u32);
    let bytes = text.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for chunk in bytes.chunks(4) {
        let mut v = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            v |= digit(*c)? << (18 - 6 * i);
        }
        let n = chunk.len() - 1;
        for i in 0..n {
            out.push((v >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

impl PointList {
    /// 詰めた文字列。
    pub fn pack(&self) -> String {
        let mut bytes = Vec::with_capacity(self.0.len() * 2);
        let mut prev = IPoint { x: 0, y: 0 };
        for (i, p) in self.0.iter().enumerate() {
            let (dx, dy) = if i == 0 {
                (p.x, p.y)
            } else {
                (p.x - prev.x, p.y - prev.y)
            };
            put_varint(zigzag(dx), &mut bytes);
            put_varint(zigzag(dy), &mut bytes);
            prev = *p;
        }
        base64(&bytes)
    }

    /// 詰めた文字列から読む。正規の書き方でない・座標が範囲の外なら理由を返す。
    pub fn unpack(text: &str) -> Result<Self, String> {
        let bytes = unbase64(text).ok_or("点の列の文字列が正しくありません")?;
        let mut at = 0;
        let mut points = Vec::new();
        let mut prev = IPoint { x: 0, y: 0 };
        while at < bytes.len() {
            let dx = get_varint(&bytes, &mut at).ok_or("点の列が途中で切れています")?;
            let dy = get_varint(&bytes, &mut at).ok_or("点の列が途中で切れています")?;
            let (dx, dy) = (unzigzag(dx), unzigzag(dy));
            let p = if points.is_empty() {
                IPoint { x: dx, y: dy }
            } else {
                IPoint {
                    x: prev.x.checked_add(dx).ok_or("点の座標があふれます")?,
                    y: prev.y.checked_add(dy).ok_or("点の座標があふれます")?,
                }
            };
            if !coord_ok(p.x) || !coord_ok(p.y) {
                return Err("点の座標が範囲の外です".into());
            }
            points.push(p);
            prev = p;
        }
        let list = Self(points);
        if list.pack() != text {
            return Err("点の列の書き方が正規の形ではありません".into());
        }
        Ok(list)
    }
}

impl Serialize for PointList {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.pack())
    }
}

impl<'de> Deserialize<'de> for PointList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::unpack(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn pts(v: &[(i64, i64)]) -> PointList {
        PointList(v.iter().map(|(x, y)| IPoint { x: *x, y: *y }).collect())
    }

    #[test]
    fn known_encodings() {
        assert_eq!(pts(&[]).pack(), "");
        // (0, 0) → 0x00 0x00
        assert_eq!(pts(&[(0, 0)]).pack(), "AAA");
        // (1, -1) → 0x02 0x01
        assert_eq!(pts(&[(1, -1)]).pack(), "AgE");
        // 64 → 128 → 0x80 0x01
        assert_eq!(pts(&[(64, 0)]).pack(), "gAEA");
        assert_eq!(PointList::unpack("AgE").unwrap(), pts(&[(1, -1)]));
    }

    #[test]
    fn rejects_non_canonical_and_broken_text() {
        // 0 を 2 バイトで書いたもの（0x80 0x00）
        assert!(PointList::unpack(&base64(&[0x80, 0x00, 0x00])).is_err());
        assert!(PointList::unpack("A").is_err());
        assert!(PointList::unpack("A=A").is_err());
        assert!(PointList::unpack("gA").is_err());
        // 範囲の外の座標
        assert!(PointList::unpack(&pts(&[(1 << 32, 0)]).pack()).is_err());
        // 余ったビットのある最後の文字（"AAB" は 0x00 0x00 と余り 1）
        assert!(PointList::unpack("AAB").is_err());
    }

    proptest! {
        #[test]
        fn round_trips(v in proptest::collection::vec((-(1i64 << 31)..=(1i64 << 31), -(1i64 << 31)..=(1i64 << 31)), 0..40)) {
            let list = pts(&v);
            let text = list.pack();
            prop_assert_eq!(PointList::unpack(&text).unwrap(), list);
            prop_assert!(text.bytes().all(|b| ALPHABET.contains(&b)));
        }
    }
}
