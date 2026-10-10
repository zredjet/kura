//! 文字列の正規化と比較。

use std::cmp::Ordering;

use unicode_normalization::UnicodeNormalization;

/// NFC に正規化する。macOS のファイル名は分解形（NFD）のことがあり、
/// そのままタグやフォルダ名にすると入力した文字（NFC）と一致しないため。
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// 自然順の比較（「2」<「10」）。数字の並びは数値として比べ、それ以外は大文字・小文字を区別せずに比べる。
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let xs = take_digits(&mut ai);
                let ys = take_digits(&mut bi);
                let ord = cmp_digit_runs(&xs, &ys);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}

/// 数字の並びを数値として比べる（桁数が多い方が大きい。同じ値なら先頭の 0 が少ない方を先に）。
fn cmp_digit_runs(a: &str, b: &str) -> Ordering {
    let at = a.trim_start_matches('0');
    let bt = b.trim_start_matches('0');
    at.len()
        .cmp(&bt.len())
        .then_with(|| at.cmp(bt))
        .then_with(|| a.len().cmp(&b.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_composes_dakuten() {
        let decomposed = "\u{30AB}\u{3099}"; // カ + 濁点
        assert_eq!(nfc(decomposed), "ガ");
        assert_eq!(nfc("鎌倉"), "鎌倉");
    }

    #[test]
    fn natural_order_compares_numbers_by_value() {
        let mut names = vec!["img10.jpg", "img2.jpg", "img1.jpg", "IMG3.jpg", "img02.jpg"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            vec!["img1.jpg", "img2.jpg", "img02.jpg", "IMG3.jpg", "img10.jpg"]
        );
    }

    #[test]
    fn natural_order_handles_prefixes_and_japanese() {
        assert_eq!(natural_cmp("a", "ab"), Ordering::Less);
        assert_eq!(natural_cmp("第2話", "第10話"), Ordering::Less);
        assert_eq!(natural_cmp("鎌倉", "鎌倉"), Ordering::Equal);
    }
}
