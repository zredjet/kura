//! 時刻。保存・受け渡しはすべて UTC の Unix ミリ秒（`i64`）。

use std::time::{SystemTime, UNIX_EPOCH};

/// 現在時刻（UTC の Unix ミリ秒）。
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// ログ用に「2026-10-10 16:00:00.123Z」の形へ整形する（UTC）。
pub fn format_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// ISO 8601 の日時（`2026-08-30T09:22:01.5Z`、`2026-08-30 18:22:01+09:00` など）を UTC ミリ秒にする。
/// 時差がなければ UTC とみなす。読めなければ None。
pub fn parse_iso8601_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    let num = |s: &str| s.parse::<i64>().ok();
    let (date, rest) = (text.get(0..10)?, text.get(10..)?);
    let mut parts = date.split('-');
    let (y, m, d) = (
        num(parts.next()?)?,
        num(parts.next()?)?,
        num(parts.next()?)?,
    );
    let rest = rest.strip_prefix(['T', ' ']).unwrap_or(rest);
    let (h, mi, s) = if rest.len() >= 8 {
        (
            num(rest.get(0..2)?)?,
            num(rest.get(3..5)?)?,
            num(rest.get(6..8)?)?,
        )
    } else {
        (0, 0, 0)
    };
    let mut tail = rest.get(8..).unwrap_or("");
    let mut millis = 0;
    if let Some(frac) = tail.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        tail = frac.get(digits.len()..).unwrap_or("");
        let padded = format!("{digits:0<3}");
        millis = num(padded.get(0..3)?)?;
    }
    let offset_min = match tail {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = if tail.starts_with('-') { -1 } else { 1 };
            let digits: String = tail.chars().filter(char::is_ascii_digit).collect();
            let oh = num(digits.get(0..2)?)?;
            let om = digits.get(2..4).and_then(num).unwrap_or(0);
            sign * (oh * 60 + om)
        }
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(y, m, d);
    Some(((days * 86_400 + h * 3600 + mi * 60 + s) - offset_min * 60) * 1000 + millis)
}

/// 年月日を 1970-01-01 からの日数にする（Howard Hinnant の days_from_civil）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 1970-01-01 からの日数を年月日にする（Howard Hinnant の civil_from_days）。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_epoch_and_known_dates() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00.000Z");
        // 2026-10-10 16:06:58.942 UTC
        assert_eq!(format_utc(1_791_648_418_942), "2026-10-10 16:06:58.942Z");
        // うるう日
        assert_eq!(format_utc(951_782_400_000), "2000-02-29 00:00:00.000Z");
    }

    #[test]
    fn parses_iso8601_with_offsets() {
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso8601_ms("2026-10-10T16:06:58.942000Z"),
            Some(1_791_648_418_942)
        );
        // 日本時間の 2026-10-11 01:06:58.942 は同じ瞬間
        assert_eq!(
            parse_iso8601_ms("2026-10-11 01:06:58.942+09:00"),
            Some(1_791_648_418_942)
        );
        assert_eq!(parse_iso8601_ms("2000-02-29"), Some(951_782_400_000));
        assert_eq!(parse_iso8601_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso8601_ms("yesterday"), None);
    }

    #[test]
    fn now_is_after_2026() {
        assert!(now_ms() > 1_767_225_600_000);
    }
}
