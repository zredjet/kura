//! EXIF（TIFF の形のバイト列）の向き（Orientation、タグ 0x0112）だけを読み書きする。
//!
//! - JPEG の APP1 は先頭に `Exif\0\0` が付く。値に持つときは外し、書くときに付ける（`strip_header`）。
//! - 読むときは向きを画素に当て、EXIF の中の向きを 1 に書き換えて運ぶ（同じ向きを 2 回当てないように）。
//! - 壊れた EXIF は、向きを 1 として扱い、バイト列はそのまま運ぶ（落とさない）。読むのは IFD0 の項目だけ。

/// `Exif\0\0` で始まっていれば外す。
pub fn strip_header(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"Exif\0\0").unwrap_or(b)
}

/// バイト順。
#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

impl Order {
    fn u16(self, b: &[u8], at: usize) -> Option<u16> {
        let s = b.get(at..at.checked_add(2)?)?;
        Some(match self {
            Order::Little => u16::from_le_bytes([s[0], s[1]]),
            Order::Big => u16::from_be_bytes([s[0], s[1]]),
        })
    }

    fn u32(self, b: &[u8], at: usize) -> Option<u32> {
        let s = b.get(at..at.checked_add(4)?)?;
        Some(match self {
            Order::Little => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            Order::Big => u32::from_be_bytes([s[0], s[1], s[2], s[3]]),
        })
    }

    fn put_u16(self, v: u16) -> [u8; 2] {
        match self {
            Order::Little => v.to_le_bytes(),
            Order::Big => v.to_be_bytes(),
        }
    }
}

/// 向きの値が入っている位置（バイト順と、値の 2 バイトの位置）。
fn orientation_slot(tiff: &[u8]) -> Option<(Order, usize)> {
    let order = match tiff.get(0..2)? {
        b"II" => Order::Little,
        b"MM" => Order::Big,
        _ => return None,
    };
    if order.u16(tiff, 2)? != 42 {
        return None;
    }
    let ifd = order.u32(tiff, 4)? as usize;
    let count = order.u16(tiff, ifd)? as usize;
    for i in 0..count {
        let e = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        let tag = order.u16(tiff, e)?;
        if tag == 0x0112 {
            // 型は SHORT（3）、数は 1。値は項目の値の欄の先頭 2 バイト
            if order.u16(tiff, e + 2)? != 3 || order.u32(tiff, e + 4)? != 1 {
                return None;
            }
            let at = e + 8;
            tiff.get(at..at + 2)?;
            return Some((order, at));
        }
    }
    None
}

/// 向き（1〜8）。ない・壊れている・範囲の外なら 1。
pub fn orientation(tiff: &[u8]) -> u8 {
    orientation_slot(tiff)
        .and_then(|(order, at)| order.u16(tiff, at))
        .and_then(|v| u8::try_from(v).ok())
        .filter(|v| (1..=8).contains(v))
        .unwrap_or(1)
}

/// 向きを 1 に書き換えたバイト列（向きの項目がなければそのまま）。
pub fn with_orientation_reset(tiff: &[u8]) -> Vec<u8> {
    let mut out = tiff.to_vec();
    if let Some((order, at)) = orientation_slot(tiff) {
        out[at..at + 2].copy_from_slice(&order.put_u16(1));
    }
    out
}

/// テスト用：向きの項目 1 つだけを持つ最小の EXIF を作る。
pub fn minimal(orientation: u16, big_endian: bool) -> Vec<u8> {
    let order = if big_endian {
        Order::Big
    } else {
        Order::Little
    };
    let mut b = Vec::new();
    b.extend_from_slice(if big_endian { b"MM" } else { b"II" });
    b.extend_from_slice(&order.put_u16(42));
    let u32b = |v: u32| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    b.extend_from_slice(&u32b(8));
    b.extend_from_slice(&order.put_u16(1));
    b.extend_from_slice(&order.put_u16(0x0112));
    b.extend_from_slice(&order.put_u16(3));
    b.extend_from_slice(&u32b(1));
    b.extend_from_slice(&order.put_u16(orientation));
    b.extend_from_slice(&[0, 0]);
    b.extend_from_slice(&u32b(0));
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_resets_orientation_in_both_byte_orders() {
        for big in [false, true] {
            for o in 1..=8u16 {
                let e = minimal(o, big);
                assert_eq!(orientation(&e), o as u8);
                let reset = with_orientation_reset(&e);
                assert_eq!(orientation(&reset), 1);
                assert_eq!(reset.len(), e.len());
            }
        }
        let mut with_header = b"Exif\0\0".to_vec();
        with_header.extend(minimal(6, false));
        assert_eq!(orientation(strip_header(&with_header)), 6);
    }

    /// 壊れた EXIF は落ちずに 1。範囲の外の値も 1。
    #[test]
    fn broken_exif_is_orientation_1() {
        assert_eq!(orientation(&[]), 1);
        assert_eq!(orientation(b"II*\0\xff\xff\xff\xff"), 1);
        assert_eq!(orientation(&minimal(9, false)), 1);
        let e = minimal(6, true);
        for cut in 0..e.len() {
            let _ = orientation(&e[..cut]);
            let _ = with_orientation_reset(&e[..cut]);
        }
    }
}
