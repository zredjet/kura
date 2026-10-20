//! フィールド（docs/search-query.md §3）の名前と値の読み取り。

use super::ast::{
    AlphaSpec, CivilDate, DateSpec, FieldCond, KindValue, Op, Orientation, PlayedSpec, UnitSpec,
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
    Alpha,
    Pixel,
    Colors,
    Pot,
    Rate,
    Bits,
    Ch,
    Lufs,
    Peak,
    Loop,
    Anim,
}

/// フィールド 1 つ。
pub struct FieldDef {
    /// 英語名（AST の `field`）。
    pub key: &'static str,
    /// 日本語名（エラーの文に使う）。
    pub label: &'static str,
    /// 入力で使える名前（日本語・英語。英語は大文字小文字を問わない）。
    pub names: &'static [&'static str],
    pub(crate) kind: FieldKind,
}

/// フィールドの表（§3）。フロントは bindings の定数 `QUERY_FIELD_NAMES` でこの表を読む
/// （`透過:あり` を名前空間付きのタグとして扱わないため。表を 1 か所にする）。
pub const FIELDS: &[FieldDef] = &[
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
    // 素材（M11）
    def("alpha", "透過", &["透過", "alpha"], FieldKind::Alpha),
    def(
        "pixel",
        "ドット絵",
        &["ドット絵", "pixel"],
        FieldKind::Pixel,
    ),
    def("colors", "色数", &["色数", "colors"], FieldKind::Colors),
    // 日本語の入力では数字が全角になりやすいので「２のべき乗」も受ける
    def(
        "pot",
        "2のべき乗",
        &["2のべき乗", "２のべき乗", "pot"],
        FieldKind::Pot,
    ),
    def("rate", "レート", &["レート", "rate"], FieldKind::Rate),
    def("bits", "ビット", &["ビット", "bits"], FieldKind::Bits),
    def("ch", "チャンネル", &["チャンネル", "ch"], FieldKind::Ch),
    def(
        "lufs",
        "ラウドネス",
        &["ラウドネス", "lufs"],
        FieldKind::Lufs,
    ),
    def("peak", "ピーク", &["ピーク", "peak"], FieldKind::Peak),
    def("loop", "ループ", &["ループ", "loop"], FieldKind::Loop),
    def("anim", "アニメ", &["アニメ", "anim"], FieldKind::Anim),
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
        FieldKind::Alpha => {
            only_equality(op)?;
            let value = match value.trim().to_lowercase().as_str() {
                "あり" | "はい" | "yes" | "any" => AlphaSpec::Any,
                "なし" | "いいえ" | "no" | "none" => AlphaSpec::None,
                "二値" | "binary" => AlphaSpec::Binary,
                "半透明" | "partial" => AlphaSpec::Partial,
                _ => return Err("透過は あり・なし・半透明・二値 のどれかです".into()),
            };
            FieldCond::Alpha { op, value }
        }
        FieldKind::Pixel => {
            only_equality(op)?;
            FieldCond::Pixel {
                op,
                value: yes_no(value, label)?,
            }
        }
        FieldKind::Pot => {
            only_equality(op)?;
            FieldCond::Pot {
                op,
                value: yes_no(value, label)?,
            }
        }
        FieldKind::Loop => {
            only_equality(op)?;
            FieldCond::Loop {
                op,
                value: yes_no(value, label)?,
            }
        }
        FieldKind::Anim => {
            only_equality(op)?;
            FieldCond::Anim {
                op,
                value: yes_no(value, label)?,
            }
        }
        FieldKind::Colors => FieldCond::Colors {
            op,
            value: count(value, label)?,
        },
        FieldKind::Rate => FieldCond::Rate {
            op,
            value: parse_rate(value).ok_or_else(|| {
                "レートは Hz の数で指定してください（例：44100・44.1k・48kHz）".to_string()
            })?,
        },
        FieldKind::Bits => FieldCond::Bits {
            op,
            value: positive(strip_suffix(value, &["ビット", "bit"])).ok_or_else(|| {
                "ビットは 1 以上の数で指定してください（例：ビット:24）".to_string()
            })?,
        },
        FieldKind::Ch => {
            let v = value.trim();
            let channels = match v.to_lowercase().as_str() {
                "モノラル" | "mono" => Some(1),
                "ステレオ" | "stereo" => Some(2),
                _ => positive(v),
            };
            FieldCond::Ch {
                op,
                value: channels.ok_or_else(|| {
                    "チャンネルは 1 以上の数か、モノラル・ステレオで指定してください".to_string()
                })?,
            }
        }
        FieldKind::Lufs => {
            only_comparison(op, label, "ラウドネス>-14")?;
            FieldCond::Lufs {
                op,
                value: decibels(strip_suffix(value, &["lufs"])).ok_or_else(|| {
                    "ラウドネスは LUFS の数で指定してください（例：ラウドネス>-14）".to_string()
                })?,
            }
        }
        FieldKind::Peak => {
            only_comparison(op, label, "ピーク>-1")?;
            FieldCond::Peak {
                op,
                value: decibels(strip_suffix(value, &["dbtp", "db"])).ok_or_else(|| {
                    "ピークは dBTP の数で指定してください（例：ピーク>-1）".to_string()
                })?,
            }
        }
    })
}

/// 大小の比較だけのフィールド（ラウドネス・ピーク。実数なので一致では探さない）。
fn only_comparison(op: Op, label: &str, example: &str) -> Result<(), String> {
    if op.is_comparison() {
        Ok(())
    } else {
        Err(format!(
            "「{label}」は大小で比べてください（例：{example}）"
        ))
    }
}

/// あり・なし（はい・いいえ、yes・no、true・false）。
fn yes_no(value: &str, label: &str) -> Result<bool, String> {
    match value.trim().to_lowercase().as_str() {
        "あり" | "はい" | "yes" | "true" => Ok(true),
        "なし" | "いいえ" | "no" | "false" => Ok(false),
        _ => Err(format!("「{label}」は あり・なし で指定してください")),
    }
}

/// 末尾の単位（大文字小文字を問わない）を外す。
fn strip_suffix<'a>(value: &'a str, units: &[&str]) -> &'a str {
    let v = value.trim();
    let lower = v.to_lowercase();
    for unit in units {
        // 単位は ASCII かかなで、小文字にしても長さが変わらない
        if lower.ends_with(unit)
            && v.len() >= unit.len()
            && v.is_char_boundary(v.len() - unit.len())
        {
            return v[..v.len() - unit.len()].trim_end();
        }
    }
    v
}

/// 1 以上の整数。
fn positive(value: &str) -> Option<i64> {
    parse_int(value).filter(|v| *v >= 1)
}

/// 10 進の数（符号・小数点付き。指数・inf・NaN は受けない）を `10^digits` 倍の整数にする。
/// 0 から遠い方へ丸め、丸めずに済んだか（exact）も返す。符号は `-`・`+`・U+2212 の `−`。
fn scaled(value: &str, digits: u32) -> Option<(i64, bool)> {
    let v = value.trim();
    let (negative, body) = if let Some(rest) = v.strip_prefix(['-', '−']) {
        (true, rest)
    } else if let Some(rest) = v.strip_prefix('+') {
        (false, rest)
    } else {
        (false, v)
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let digits_only = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (int.is_empty() && frac.is_empty()) || !digits_only(int) || !digits_only(frac) {
        return None;
    }
    // 桁があふれないように（ここで扱う値はどれも 12 桁で足りる）
    if int.len() > 12 {
        return None;
    }
    let mut n: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac: Vec<i64> = frac.bytes().map(|b| i64::from(b - b'0')).collect();
    let keep = usize::try_from(digits).ok()?;
    for i in 0..keep {
        n = n
            .checked_mul(10)?
            .checked_add(frac.get(i).copied().unwrap_or(0))?;
    }
    let rest = frac.get(keep..).unwrap_or(&[]);
    let exact = rest.iter().all(|d| *d == 0);
    if rest.first().is_some_and(|d| *d >= 5) {
        n = n.checked_add(1)?;
    }
    Some((if negative { -n } else { n }, exact))
}

/// サンプリングレート（Hz の整数）：`44100`・`44100Hz`・`44.1k`・`48kHz`。0 以下と、Hz にして端数の出るものは受けない。
fn parse_rate(value: &str) -> Option<i64> {
    let v = strip_suffix(value, &["hz"]);
    let hz = match v.strip_suffix(['k', 'K']) {
        Some(khz) => match scaled(khz, 3)? {
            (hz, true) => hz,
            (_, false) => return None,
        },
        None => match scaled(v, 0)? {
            (hz, true) => hz,
            (_, false) => return None,
        },
    };
    (1..=100_000_000).contains(&hz).then_some(hz)
}

/// デシベルの値（LUFS・dBTP）を 1/1000 の整数にする。±1000 dB を超える値は受けない。
fn decibels(value: &str) -> Option<i64> {
    let (milli, _) = scaled(value, 3)?;
    (-1_000_000..=1_000_000).contains(&milli).then_some(milli)
}

fn count(value: &str, label: &str) -> Result<i64, String> {
    parse_int(value)
        .filter(|v| *v >= 0)
        .ok_or_else(|| format!("「{label}」は 0 以上の数で指定してください"))
}

fn parse_int(value: &str) -> Option<i64> {
    value.trim().parse().ok()
}

/// 種類の値の語（日本語の語・英語の名前）。候補（suggest）もこの順に出す。
pub const KIND_WORDS: [(&str, KindValue); 5] = [
    ("画像", KindValue::Image),
    ("動画", KindValue::Video),
    ("音楽", KindValue::Audio),
    ("フォント", KindValue::Font),
    ("3D", KindValue::Model),
];

/// 種類の語を読む：日本語の語（`3D` は大文字・小文字を問わない）か、英語の名前（`image`・`font`・`model` など）。
fn parse_kind(word: &str) -> Option<KindValue> {
    let lower = word.to_lowercase();
    KIND_WORDS
        .iter()
        .find(|(ja, _)| ja.to_lowercase() == lower)
        .map(|(_, kind)| *kind)
        .or_else(|| KindValue::parse(&lower))
}

fn parse_kinds(value: &str) -> Result<Vec<KindValue>, String> {
    const MESSAGE: &str = "種類は 画像・動画・音楽・フォント・3D のどれかです（「,」で複数）";
    let mut kinds = Vec::new();
    for part in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let kind = parse_kind(part).ok_or(MESSAGE)?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        return Err(MESSAGE.into());
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
        assert_eq!(find_field("２のべき乗").map(|f| f.key), Some("pot"));
        assert_eq!(find_field("LUFS").map(|f| f.key), Some("lufs"));
        assert!(find_field("作者").is_none());
    }

    /// 名前は表の中で重ならず、記号・空白を含まない（フロントは bindings の `QUERY_FIELD_NAMES` で同じ表を読む）。
    #[test]
    fn field_names_are_unique_plain_words() {
        let mut seen: Vec<String> = Vec::new();
        let mut keys: Vec<&str> = Vec::new();
        for def in FIELDS {
            assert!(!keys.contains(&def.key), "{}", def.key);
            keys.push(def.key);
            assert!(
                def.names.contains(&def.label) || def.label == "URL",
                "{}",
                def.key
            );
            for name in def.names {
                let lower = name.to_lowercase();
                assert!(!seen.contains(&lower), "{name} が重なっています");
                seen.push(lower);
                assert!(
                    name.chars().all(|c| !c.is_whitespace()
                        && !matches!(
                            c,
                            ':' | '=' | '<' | '>' | '!' | '(' | ')' | '"' | '|' | ',' | '-'
                        )
                        && super::super::lexer::normalize(c) == c),
                    "{name}"
                );
                assert_eq!(find_field(name).map(|f| f.key), Some(def.key), "{name}");
            }
        }
    }
}
