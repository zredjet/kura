//! フィールド（docs/search-query.md §3）の名前と値の読み取り。

use super::ast::{
    CivilDate, DateSpec, FieldCond, KindValue, Op, Orientation, PlayedSpec, UnitSpec,
};

/// フィールドの種類（§3 の表の行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Type,
    Rating,
    Dur,
    Added,
    Taken,
    Played,
    Plays,
    Folder,
    Unit,
    Source,
    Name,
    Url,
    Note,
    Tags,
    W,
    H,
    Orient,
}

/// フィールド 1 つ。
pub(crate) struct FieldDef {
    /// 英語名（AST の `field`）。
    pub key: &'static str,
    /// 日本語名（エラーの文に使う）。
    pub label: &'static str,
    pub names: &'static [&'static str],
    pub kind: FieldKind,
}

pub(crate) const FIELDS: &[FieldDef] = &[
    def("type", "種類", &["種類", "type"], FieldKind::Type),
    def("rating", "評価", &["評価", "rating"], FieldKind::Rating),
    def("dur", "長さ", &["長さ", "dur"], FieldKind::Dur),
    def("added", "追加", &["追加", "added"], FieldKind::Added),
    def("taken", "撮影", &["撮影", "taken"], FieldKind::Taken),
    def("played", "再生", &["再生", "played"], FieldKind::Played),
    def(
        "plays",
        "再生回数",
        &["再生回数", "plays"],
        FieldKind::Plays,
    ),
    def(
        "folder",
        "フォルダ",
        &["フォルダ", "folder"],
        FieldKind::Folder,
    ),
    def("unit", "ユニット", &["ユニット", "unit"], FieldKind::Unit),
    def("source", "取得元", &["取得元", "source"], FieldKind::Source),
    def("name", "名前", &["名前", "name"], FieldKind::Name),
    def("url", "URL", &["url"], FieldKind::Url),
    def("note", "メモ", &["メモ", "note"], FieldKind::Note),
    def("tags", "タグ数", &["タグ数", "tags"], FieldKind::Tags),
    def("w", "幅", &["幅", "w"], FieldKind::W),
    def("h", "高さ", &["高さ", "h"], FieldKind::H),
    def("orient", "向き", &["向き", "orient"], FieldKind::Orient),
];

const fn def(
    key: &'static str,
    label: &'static str,
    names: &'static [&'static str],
    kind: FieldKind,
) -> FieldDef {
    FieldDef {
        key,
        label,
        names,
        kind,
    }
}

/// 名前（日本語・英語。英語は大文字小文字を問わない）からフィールドを探す。
pub(crate) fn find_field(name: &str) -> Option<&'static FieldDef> {
    let lower = name.to_lowercase();
    FIELDS
        .iter()
        .find(|f| f.names.iter().any(|n| *n == name || *n == lower))
}

/// 値を読む。失敗したら理由を返す（位置は呼ぶ側が値の範囲にする）。
pub(crate) fn build(def: &FieldDef, op: Op, value: &str) -> Result<FieldCond, String> {
    let label = def.label;
    let only_equality = |op: Op| -> Result<(), String> {
        if op.is_comparison() {
            Err(format!(
                "「{label}」は大小を比べられません（「:」「=」「!=」を使ってください）"
            ))
        } else {
            Ok(())
        }
    };
    let text = |value: &str| -> Result<String, String> {
        let v = value.trim();
        if v.is_empty() {
            Err(format!("「{label}」の値を入力してください"))
        } else {
            Ok(v.to_string())
        }
    };
    Ok(match def.kind {
        FieldKind::Type => {
            only_equality(op)?;
            FieldCond::Type {
                op,
                value: parse_kinds(value)?,
            }
        }
        FieldKind::Rating => {
            let value = parse_int(value)
                .filter(|v| (0..=5).contains(v))
                .ok_or_else(|| "評価は 0〜5 の数で指定してください".to_string())?;
            FieldCond::Rating { op, value }
        }
        FieldKind::Dur => FieldCond::Dur {
            op,
            value: parse_duration_ms(value).ok_or_else(|| {
                "長さは「30秒」「3分」「1時間」「1:30」の形で指定してください".to_string()
            })?,
        },
        FieldKind::Added => FieldCond::Added {
            op,
            value: parse_date(value)?,
        },
        FieldKind::Taken => FieldCond::Taken {
            op,
            value: parse_date(value)?,
        },
        FieldKind::Played => {
            let v = value.trim();
            if matches!(v, "未" | "never" | "なし") {
                only_equality(op)?;
                FieldCond::Played {
                    op,
                    value: PlayedSpec::Never,
                }
            } else {
                FieldCond::Played {
                    op,
                    value: PlayedSpec::Date(parse_date(value)?),
                }
            }
        }
        FieldKind::Plays => FieldCond::Plays {
            op,
            value: count(value, label)?,
        },
        FieldKind::Folder => {
            only_equality(op)?;
            let path: Vec<String> = value
                .split('/')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if path.is_empty() {
                return Err("フォルダのパスを入力してください（例：写真/2026 旅行）".into());
            }
            FieldCond::Folder { op, value: path }
        }
        FieldKind::Unit => {
            only_equality(op)?;
            let v = text(value)?;
            let value = match v.as_str() {
                "あり" | "any" => UnitSpec::Any,
                "なし" | "none" => UnitSpec::None,
                _ => UnitSpec::Name(v),
            };
            FieldCond::Unit { op, value }
        }
        FieldKind::Source => {
            only_equality(op)?;
            FieldCond::Source {
                op,
                value: text(value)?,
            }
        }
        FieldKind::Name => {
            only_equality(op)?;
            FieldCond::Name {
                op,
                value: text(value)?,
            }
        }
        FieldKind::Url => {
            only_equality(op)?;
            FieldCond::Url {
                op,
                value: text(value)?,
            }
        }
        FieldKind::Note => {
            only_equality(op)?;
            FieldCond::Note {
                op,
                value: text(value)?,
            }
        }
        FieldKind::Tags => FieldCond::Tags {
            op,
            value: count(value, label)?,
        },
        FieldKind::W => FieldCond::W {
            op,
            value: count(value, label)?,
        },
        FieldKind::H => FieldCond::H {
            op,
            value: count(value, label)?,
        },
        FieldKind::Orient => {
            only_equality(op)?;
            let value = match value.trim() {
                "縦" | "portrait" => Orientation::Portrait,
                "横" | "landscape" => Orientation::Landscape,
                "正方形" | "square" => Orientation::Square,
                _ => return Err("向きは 縦・横・正方形 のどれかです".into()),
            };
            FieldCond::Orient { op, value }
        }
    })
}

fn count(value: &str, label: &str) -> Result<i64, String> {
    parse_int(value)
        .filter(|v| *v >= 0)
        .ok_or_else(|| format!("「{label}」は 0 以上の数で指定してください"))
}

fn parse_int(value: &str) -> Option<i64> {
    value.trim().parse().ok()
}

fn parse_kinds(value: &str) -> Result<Vec<KindValue>, String> {
    let mut kinds = Vec::new();
    for part in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let kind = match part.to_lowercase().as_str() {
            "画像" | "image" => KindValue::Image,
            "動画" | "video" => KindValue::Video,
            "音楽" | "audio" => KindValue::Audio,
            _ => return Err("種類は 画像・動画・音楽 のどれかです（「,」で複数）".into()),
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        return Err("種類は 画像・動画・音楽 のどれかです（「,」で複数）".into());
    }
    Ok(kinds)
}

/// 長さ（ミリ秒）：`30秒` `3分` `1時間` `1時間30分` `90s` `1:30` `1:02:03`、数だけなら秒。
fn parse_duration_ms(value: &str) -> Option<i64> {
    let v = value.trim().to_lowercase();
    if v.is_empty() {
        return None;
    }
    if v.contains(':') {
        let parts: Vec<&str> = v.split(':').collect();
        if parts.len() > 3
            || parts
                .iter()
                .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
        let mut seconds = 0i64;
        for part in parts {
            seconds = seconds * 60 + part.parse::<i64>().ok()?;
        }
        return Some(seconds * 1000);
    }
    let mut total = 0f64;
    let mut number = String::new();
    let mut rest = v.as_str();
    let mut matched_unit = false;
    while let Some(c) = rest.chars().next() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let units: [(&str, f64); 9] = [
            ("時間", 3600.0),
            ("hour", 3600.0),
            ("h", 3600.0),
            ("分", 60.0),
            ("min", 60.0),
            ("m", 60.0),
            ("秒", 1.0),
            ("sec", 1.0),
            ("s", 1.0),
        ];
        let (unit, scale) = units.iter().find(|(u, _)| rest.starts_with(u))?;
        let n: f64 = number.parse().ok()?;
        total += n * scale;
        number.clear();
        rest = &rest[unit.len()..];
        matched_unit = true;
    }
    if !number.is_empty() {
        if matched_unit {
            return None;
        }
        total += number.parse::<f64>().ok()?;
    }
    if !total.is_finite() || total < 0.0 {
        return None;
    }
    Some((total * 1000.0).round() as i64)
}

/// 日付：`今日` `昨日` `7日以内` `30日以上前` `2026` `2026-10` `2026-10-01` `A..B`。
fn parse_date(value: &str) -> Result<DateSpec, String> {
    let v = value.trim();
    let err = || {
        "日付は「今日」「昨日」「7日以内」「30日以上前」「2026-10」「2026-10-01..2026-10-08」の形で指定してください".to_string()
    };
    match v.to_lowercase().as_str() {
        "今日" | "today" => return Ok(DateSpec::Today),
        "昨日" | "yesterday" => return Ok(DateSpec::Yesterday),
        _ => {}
    }
    if let Some(n) = v.strip_suffix("日以内") {
        return n
            .trim()
            .parse()
            .map(DateSpec::WithinDays)
            .map_err(|_| err());
    }
    if let Some(n) = v.strip_suffix("日以上前") {
        return n
            .trim()
            .parse()
            .map(DateSpec::OlderThanDays)
            .map_err(|_| err());
    }
    if let Some((a, b)) = v.split_once("..") {
        let (from, _) = parse_civil(a).ok_or_else(err)?;
        let (_, to) = parse_civil(b).ok_or_else(err)?;
        if to < from {
            return Err("期間の終わりが始まりより前です".into());
        }
        return Ok(DateSpec::Range { from, to });
    }
    let (from, to) = parse_civil(v).ok_or_else(err)?;
    Ok(DateSpec::Range { from, to })
}

/// `2026` `2026-10` `2026-10-01`（`/` でも可）を、その期間の最初の日と最後の日にする。
fn parse_civil(s: &str) -> Option<(CivilDate, CivilDate)> {
    let parts: Vec<&str> = s.trim().split(['-', '/']).collect();
    let num = |p: &str| -> Option<u32> {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            None
        } else {
            p.parse().ok()
        }
    };
    let y = i32::try_from(num(parts.first()?)?).ok()?;
    if !(1000..=9999).contains(&y) {
        return None;
    }
    match parts.len() {
        1 => Some((CivilDate { y, m: 1, d: 1 }, CivilDate { y, m: 12, d: 31 })),
        2 => {
            let m = num(parts[1])?;
            let last = days_in_month(y, m)?;
            Some((CivilDate { y, m, d: 1 }, CivilDate { y, m, d: last }))
        }
        3 => {
            let m = num(parts[1])?;
            let d = num(parts[2])?;
            if d == 0 || d > days_in_month(y, m)? {
                return None;
            }
            let date = CivilDate { y, m, d };
            Some((date, date))
        }
        _ => None,
    }
}

fn days_in_month(y: i32, m: u32) -> Option<u32> {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    Some(match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_durations() {
        assert_eq!(parse_duration_ms("1時間30分"), Some(5_400_000));
        assert_eq!(parse_duration_ms("1.5分"), Some(90_000));
        assert_eq!(parse_duration_ms("45"), Some(45_000));
        assert_eq!(parse_duration_ms("1h30m"), Some(5_400_000));
        assert_eq!(parse_duration_ms("3分20"), None);
        assert_eq!(parse_duration_ms("1::3"), None);
        assert_eq!(parse_duration_ms("そこそこ"), None);
    }

    #[test]
    fn reads_dates() {
        assert!(parse_date("2026-13").is_err());
        assert!(parse_date("2026-02-30").is_err());
        assert!(parse_date("2026-10-08..2026-10-01").is_err());
        assert_eq!(
            parse_date("2026/10/08").unwrap(),
            DateSpec::Range {
                from: CivilDate {
                    y: 2026,
                    m: 10,
                    d: 8
                },
                to: CivilDate {
                    y: 2026,
                    m: 10,
                    d: 8
                }
            }
        );
    }

    #[test]
    fn finds_fields_by_either_name() {
        assert_eq!(find_field("種類").map(|f| f.key), Some("type"));
        assert_eq!(find_field("URL").map(|f| f.key), Some("url"));
        assert_eq!(find_field("Type").map(|f| f.key), Some("type"));
        assert!(find_field("作者").is_none());
    }
}
