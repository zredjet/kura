//! SVG の文字のレイアウトの手間の見積もり（M11 の検証 S1）。
//!
//! usvg は文字の塊（位置を付けた字から次に位置を付けた字の前まで）ごとに、塊の字の数の 2 乗の手間をかけ
//! （字ごとに塊の先頭から字の位置を数え直す `code_point_at`）、塊の中の文字の並び（span）ごとに塊を丸ごと形にし直し、
//! 字を探すフォントがなければ集まりのすべての書体を調べる。木を作る `usvg::Tree::from_str` は中止を見ないので、入る前に断る。
//!
//! - [`text_cost`]：`<text>` 1 つの手間を、XML の木から保守的に見積もる（塊は usvg より長く・span は多めに数える）。
//!   `codec::svg::analyze` が `<use>` の展開・参照ごとの複製・marker の頂点ごとの複製を掛けて合計し、[`MAX_TEXT_WORK`] で断る。
//! - [`font_resolver`]：フォントを選ぶ・代わりのフォントを探す手間は書体の数で決まるので、usvg の既定の選び方をそのまま呼び、
//!   呼ぶたびに残りの手間から引く。尽きたら選ばずに印を立て、木を作り終えたら `Limit` にする（[`TextGuard`]）。
//! - textPath は、字ごとに path の区間をたどり、弧長を変形の拡大に応じた精度で求める（拡大の幾何平均が f32 であふれると精度が
//!   0 になり、区間ごとに 2²⁰ 回の分割になる）。そこで path の区間の数 × 字の数を手間に入れ、textPath に至る変形の拡大の上限
//!   （[`own_scale`] の積）を [`MAX_TEXT_PATH_SCALE`] で断る。弧長の逆引きは、精度が区間の長さに比べて f64 の刻みに近づくと
//!   終わらない（検証のあとの確認 R1）ので、精度の比の上限（12 × 拡大の上限 × 参照先の座標の上限 [`shape_extent`]）も
//!   [`MAX_TEXT_PATH_RANGE`] で断る。
//!
//! 手間の単位は、M4 Max の release で測った時間（ns）の目安。上限 [`MAX_TEXT_WORK`] は 1 件 100 ms ほど
//! （ラテン文字なら、1 つの塊は約 5,000 字、100 字ずつの text は合わせて約 3 万字、1 字ずつの span の塊は約 600 字。
//! 和文は 10 字のラベルで約 450 個）。字ごとの手間は文字の種類で大きく違う（輪郭の込み入った字・アラビア文字の字形の
//! 組み立て・左右の向きの混在）ので、字を 3 つの組（[`class_of`]）に分けて、組ごとに測った最悪の値で数える。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use resvg::usvg;

type SvgNode<'a, 'i> = usvg::roxmltree::Node<'a, 'i>;

/// SVG の名前空間。
const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// 文字のレイアウトの手間の上限（1 件 100 ms ほど）。
pub(crate) const MAX_TEXT_WORK: u64 = 100_000_000;
/// `<text>` 1 つごと（塊を作る・字を並べる・輪郭を作る。フォントを選ぶ分は [`font_resolver`] で数える）。
/// 1 字の `<text>` 4 万個で 1 個 17 µs（フォントを選ぶ分を含む）。
const TEXT_ELEMENT_WORK: u64 = 8_192;
/// 字ごと（形・輪郭・木の節・描く）の手間を、字の組（[`Class`]）ごとに。測った最悪（10 字の text に分けて読む）：
/// ラテン文字・ギリシャ文字・キリル文字・結合文字の重なり 2.2 µs、和文・記号・絵文字 4〜18 µs（「龘」）、
/// そのほか（アラビア文字 32 µs・「﷽」35 µs・楔形文字 20 µs）。
const CHAR_WORK: [u64; 3] = [2_560, 20_480, 40_960];
/// 塊の字の数の 2 乗の係数（'a' の 1 つの塊の 8,000 字で 183 ms＝2.9 ns、'あ' で 2.7 ns）。`textLength` があると
/// 塊のバイト数 × 字の数の走査が足される（0.34 ns）ので、塊の手間は 字の数 × (3 × 字の数 + バイト数 ÷ 2)。
const QUAD_WORK: u64 = 3;
/// 塊の span の数 × 字の数の係数を、塊の中のいちばん重い字の組ごとに（span ごとに塊を丸ごと形にし直し、字の並びを
/// たどる）。測った最悪（1 字の span の塊）：ラテン文字 205 ns（span ごとに違うフォントで 234 ns）、和文・記号・絵文字
/// 352〜446 ns、そのほか（アラビア文字の字形の組み立て 8.5 µs、左書きとアラビア文字を 1 字ずつ交互に並べた向きの並び
/// 29.5 µs）。
const SHAPE_WORK: [u64; 3] = [256, 512, 32_768];
/// textPath の中の字ごとの、path の区間ごとの手間（区間の弧長と、残りの字の位置との照合）。
const PATH_SEGMENT_WORK: u64 = 2_048;
/// フォントを選ぶ 1 回ごと（読み込みと指標）。
const SELECT_FONT_WORK: u64 = 8_192;
/// フォントを選ぶときの、書体名 × 書体ごとの手間（fontdb の query は 1 組 1.5 ns）。
const FAMILY_FACE_WORK: u64 = 2;
/// 代わりのフォントを探すときの、書体ごとの手間（字を持つかを cmap で調べる。どの書体にもない字で 787 書体を 118 µs）。
const FALLBACK_FACE_WORK: u64 = 160;

/// textPath に至る変形の拡大の上限。usvg は弧長の精度を 0.5 ÷ 拡大 にし、拡大の幾何平均が f32 であふれる（約 1.8×10¹⁹）と
/// 精度が 0 になって区間ごとに 2²⁰ 回分ける（10²¹ の拡大で区間 1 つに 94 ms、曲線 4 つ・100 字で 5 分を超えた）。
/// 10¹² の拡大では区間 1 万の path に 1,000 字で 9 ms だった。ふつうの SVG の拡大は 10³ ほど。
pub(crate) const MAX_TEXT_PATH_SCALE: f64 = 1e12;
/// textPath の弧長の精度の比（参照先の区間の長さ ÷ usvg の精度 `0.5 ÷ max(拡大, 1)`）の上限。kurbo の弧長の逆引き
/// （`inv_arclen`。usvg が字ごとに呼ぶ）は、精度 ÷ 区間の長さ が f64 の刻み（2⁻⁵²）に近づくと区間が縮まず、2⁻⁶³ を下回ると
/// 分ける回数の計算（`1 << n`）が折り返して、どちらも終わらない（検証のあとの確認 R1。M4 Max の release で、比が 7×10¹⁶ を
/// 超えると止まった）。比は 12 × 拡大の上限 × 参照先の座標の上限（[`shape_extent`]。区間の長さは制御点の折れ線の長さ
/// 6 × 座標の上限 以下）で見積もる。10¹⁴ は止まり始める比の 1/180 で、比が 10¹⁵ までなら区間 1,000・字 3,000 でも時間は
/// 比によらなかった（字の塊の 2 乗の手間で決まる）。
pub(crate) const MAX_TEXT_PATH_RANGE: f64 = 1e14;
/// 精度の比の見積もりの係数（区間の長さ ≤ 6 × 座標の上限、精度 = 0.5 ÷ 拡大）。
pub(crate) const TEXT_PATH_RANGE_FACTOR: f64 = 12.0;
/// 表示の領域の大きさの下限の上限（viewBox の幅・高さと、数だけの長さは 10⁹ まで）。単位のある長さで広がる分は
/// [`viewport_size`] で足す。
pub(crate) const UNKNOWN_VIEWPORT: f64 = 1e9;

/// 字の組（手間の重さ）。0：ラテン文字など、1：和文・記号・絵文字、2：そのほか。
type Class = usize;

/// 字の組。0 はラテン文字（拡張を含む）・ギリシャ文字・キリル文字・結合文字・一般の句読点（向きを変える制御文字を除く）、
/// 1 は記号・CJK（漢字・かな・ハングル・全角）・絵文字、2 はそのほか（アラビア文字・インドの文字など字形の組み立ての
/// 重いもの、向きを変える制御文字、まれな文字）。
pub(crate) fn class_of(c: char) -> Class {
    match u32::from(c) {
        0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069 => 2,
        0x0000..=0x052F | 0x1E00..=0x1EFF | 0x2000..=0x206F => 0,
        0x2070..=0x2BFF
        | 0x2E80..=0x9FFF
        | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFFEF
        | 0x1F000..=0x1FAFF => 1,
        _ => 2,
    }
}

/// `<text>` 1 つの見積もり。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TextCost {
    /// 手間（[`MAX_TEXT_WORK`] と同じ単位）。
    pub work: u64,
    /// いちばん重い塊を 1 度形にし直す手間（代わりのフォントが見つかると、usvg は塊を丸ごと形にし直す）。
    pub reshape: u64,
}

/// 文字の塊（usvg の `TextChunk` を多めに数えたもの）。
#[derive(Default)]
struct Chunk {
    chars: u64,
    bytes: u64,
    spans: u64,
    /// 塊の中のいちばん重い字の組。
    class: Class,
    /// textPath の中の字を含むなら、その path の区間の数（いちばん多いもの）。
    path: u64,
}

impl Chunk {
    fn work(&self) -> u64 {
        let quad = self.chars.saturating_mul(
            QUAD_WORK
                .saturating_mul(self.chars)
                .saturating_add(self.bytes / 2),
        );
        // textPath の塊は、字の位置を数え直す 2 乗の走査が 2 回ある
        let quad = if self.path > 0 {
            quad.saturating_add(
                self.chars
                    .saturating_mul(self.chars)
                    .saturating_mul(QUAD_WORK),
            )
        } else {
            quad
        };
        let shape = self.spans.saturating_mul(self.reshape());
        let path = self
            .path
            .saturating_mul(self.chars)
            .saturating_mul(PATH_SEGMENT_WORK);
        quad.saturating_add(shape).saturating_add(path)
    }

    /// 塊を 1 度形にし直す手間。
    fn reshape(&self) -> u64 {
        self.chars
            .saturating_mul(SHAPE_WORK.get(self.class).copied().unwrap_or(u64::MAX))
    }
}

fn is_xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// 名前空間のない属性の値。
fn attr<'a>(n: &SvgNode<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes()
        .find(|a| a.name() == name && a.namespace().is_none())
        .map(|a| a.value())
}

/// 素直な数（符号・数字・小数点 1 つまで。指数・単位なし）。usvg（svgtypes）がかならず長さとして読むものだけを受け付ける。
fn plain_number(t: &str) -> bool {
    let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
    !digits.is_empty()
        && digits.bytes().any(|b| b.is_ascii_digit())
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && digits.bytes().filter(|b| *b == b'.').count() <= 1
}

/// usvg がこの要素の最初の字で塊を分けると言い切れるか：SVG の text・tspan で、x か y のリストの最初が素直な数。
/// 言い切れないものは分けない（塊を長く数える側に倒す）。
fn positioned(n: &SvgNode<'_, '_>) -> bool {
    n.is_element()
        && n.tag_name().namespace() == Some(SVG_NS)
        && matches!(n.tag_name().name(), "text" | "tspan")
        && ["x", "y"].iter().any(|name| {
            attr(n, name).is_some_and(|v| {
                v.split(|c: char| is_xml_space(c) || c == ',')
                    .find(|t| !t.is_empty())
                    .is_some_and(plain_number)
            })
        })
}

/// `<text>` 1 つ（`t`）の手間を見積もる。`path_segments` は textPath の参照する図形の区間の数（参照できなければ 0）。
///
/// 塊は `<text>` の始まりと、位置を付けた text・tspan（[`positioned`]）の最初の字で分ける。位置を付けた要素の最初の文字の節が
/// 空白だけなら（usvg が空白を詰めて消すことがあるので）分けない。textPath の前後では分けない。リストの 2 つめ以降の位置でも
/// 分けない。どれも塊を usvg より長く数える側（2 乗の和・span × 字の数は、塊をつなぐと増える）。字の数は空白を詰める前の数。
pub(crate) fn text_cost(
    t: &SvgNode<'_, '_>,
    path_segments: &dyn Fn(&SvgNode<'_, '_>) -> u64,
) -> TextCost {
    let mut work = TEXT_ELEMENT_WORK;
    let mut reshape = 0u64;
    let mut cur = Chunk::default();
    // 位置を付けた要素で、まだ最初の字が来ていないもの（要素の番号）
    let mut pending: BTreeSet<usize> = BTreeSet::new();
    // textPath ごとの参照先の区間の数（同じ textPath の中の文字の節ごとに path を数え直さない）
    let mut segments: BTreeMap<usize, u64> = BTreeMap::new();
    let close = |c: &mut Chunk, work: &mut u64, reshape: &mut u64| {
        *work = work.saturating_add(c.work());
        *reshape = (*reshape).max(c.reshape());
        *c = Chunk::default();
    };
    for d in t.descendants() {
        if d.is_element() {
            if positioned(&d) {
                pending.insert(d.id().get() as usize);
            }
            continue;
        }
        let Some(s) = d.text().filter(|s| !s.is_empty()) else {
            continue;
        };
        if d.is_text() {
            let first = !pending.is_empty()
                && d.ancestors()
                    .any(|a| pending.contains(&(a.id().get() as usize)));
            if first && s.chars().any(|c| !is_xml_space(c)) && cur.chars > 0 {
                close(&mut cur, &mut work, &mut reshape);
            }
            pending.clear();
            let mut n = 0u64;
            for c in s.chars() {
                n += 1;
                let class = class_of(c);
                cur.class = cur.class.max(class);
                work = work.saturating_add(CHAR_WORK.get(class).copied().unwrap_or(u64::MAX));
            }
            cur.chars = cur.chars.saturating_add(n);
            cur.bytes = cur.bytes.saturating_add(s.len() as u64);
            cur.spans = cur.spans.saturating_add(1);
            if let Some(tp) = d
                .ancestors()
                .take_while(|a| a.id() != t.id())
                .find(|a| a.is_element() && a.tag_name().name() == "textPath")
            {
                let n = *segments
                    .entry(tp.id().get() as usize)
                    .or_insert_with(|| path_segments(&tp));
                cur.path = cur.path.max(n);
            }
        }
    }
    close(&mut cur, &mut work, &mut reshape);
    TextCost { work, reshape }
}

/// 図形を usvg が path に直したときの区間の数の見積もり（textPath の参照先）。弧・円・楕円・角丸は半径に応じた数の曲線に
/// 直るので（数の絶対値は 10⁹ まで）、弧を含む path は数の数 × 16、円・楕円・矩形は 256 とみなす。path に直せない要素は 0。
pub(crate) fn path_segments(n: &SvgNode<'_, '_>) -> u64 {
    let numbers = |s: &str| {
        s.split(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
            .filter(|t| !t.is_empty())
            .count() as u64
    };
    match n.tag_name().name() {
        "path" => attr(n, "d").map_or(0, |d| {
            let cmds = d
                .chars()
                .filter(|c| c.is_ascii_alphabetic() && !matches!(c, 'e' | 'E'))
                .count() as u64;
            let base = (numbers(d) / 2 + 1).max(cmds);
            if d.contains(['A', 'a']) {
                numbers(d).saturating_mul(16).max(base)
            } else {
                base
            }
        }),
        "polyline" | "polygon" => attr(n, "points").map_or(0, |p| numbers(p) / 2 + 1),
        "line" => 1,
        "rect" | "circle" | "ellipse" => 256,
        _ => 0,
    }
}

/// 変形のリストの関数ごとの（行列のノルムの上限, 平行移動の大きさの上限）。読めなければ None。
fn transform_parts(v: &str) -> Option<Vec<(f64, f64)>> {
    let mut out = Vec::new();
    let mut rest = v.trim_matches(|c: char| is_xml_space(c) || c == ',');
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let close = rest.find(')')?;
        if close < open {
            return None;
        }
        let name = rest[..open].trim();
        let nums: Vec<f64> = rest[open + 1..close]
            .split(|c: char| is_xml_space(c) || c == ',')
            .filter(|t| !t.is_empty())
            .map(|t| t.parse::<f64>().ok().filter(|x| x.is_finite()))
            .collect::<Option<Vec<f64>>>()?;
        let at = |k: usize| nums.get(k).copied().unwrap_or(0.0).abs();
        let one = match (name, nums.len()) {
            ("matrix", 6) => (at(0) + at(1) + at(2) + at(3), at(4) + at(5)),
            ("translate", 1 | 2) => (1.0, at(0) + at(1)),
            ("rotate", 1) => (1.0, 0.0),
            // 中心のまわりの回転は、中心を動かす平行移動を含む（c − R·c）
            ("rotate", 3) => (1.0, 2.0 * (at(1) + at(2))),
            ("scale", 1) => (at(0), 0.0),
            ("scale", 2) => (crate::math::max(at(0), at(1)), 0.0),
            ("skewX" | "skewY", 1) => {
                let r = nums.first().copied().unwrap_or(0.0) * (std::f64::consts::PI / 180.0);
                let c = crate::math::cos(r).abs();
                if c < 1e-9 {
                    (f64::INFINITY, 0.0)
                } else {
                    (1.0 + (crate::math::sin(r) / c).abs(), 0.0)
                }
            }
            _ => return None,
        };
        out.push(one);
        rest = rest[close + 1..].trim_start_matches(|c: char| is_xml_space(c) || c == ',');
    }
    Some(out)
}

/// 変形のリストの拡大の上限（各関数の行列のノルムの上限の積。1 より小さい拡大は 1 とみなす）。読めなければ無限大。
pub(crate) fn transform_scale(v: &str) -> f64 {
    transform_parts(v).map_or(f64::INFINITY, |parts| {
        parts
            .iter()
            .fold(1.0, |s, (one, _)| s * crate::math::max(*one, 1.0))
    })
}

/// 変形のリストが点 p を写す先の大きさの上限 `s × |p| + t` の (s, t)（左の関数ほど外側。|p| は長さ）。読めなければ無限大。
pub(crate) fn transform_bound(v: &str) -> (f64, f64) {
    transform_parts(v).map_or((f64::INFINITY, f64::INFINITY), |parts| {
        // A ∘ f の上限：|A(f(p))| ≤ sA (sf |p| + tf) + tA
        parts
            .iter()
            .fold((1.0, 0.0), |(s, t), (sf, tf)| (s * sf, s * tf + t))
    })
}

/// 長さの値（数と単位 1 つ）の大きさの上限（利用者の単位、96 dpi）。数だけ・px はそのまま、in・cm・mm・pt・pc は係数を掛け、
/// % は `viewport` の割合。em・ex（フォントの大きさは入れ子で掛け合わさるので上限が決まらない）と読めない値は無限大。
pub(crate) fn length_bound(v: &str, viewport: f64) -> f64 {
    let b = v.trim().as_bytes();
    let digit = |k: usize| b.get(k).is_some_and(u8::is_ascii_digit);
    let mut i = 0;
    if matches!(b.first(), Some(b'-' | b'+')) {
        i += 1;
    }
    let start_digits = i;
    while digit(i) {
        i += 1;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        while digit(i) {
            i += 1;
        }
    }
    if !(start_digits..i).any(digit) {
        return f64::INFINITY;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut k = i + 1;
        if matches!(b.get(k), Some(b'-' | b'+')) {
            k += 1;
        }
        if digit(k) {
            while digit(k) {
                k += 1;
            }
            i = k;
        }
    }
    let (Some(num), Some(unit)) = (b.get(..i), b.get(i..)) else {
        return f64::INFINITY;
    };
    let Some(x) = std::str::from_utf8(num)
        .ok()
        .and_then(|t| t.parse::<f64>().ok())
        .filter(|x| x.is_finite())
    else {
        return f64::INFINITY;
    };
    let factor = match unit {
        b"" | b"px" => 1.0,
        b"in" => 96.0,
        b"cm" => 96.0 / 2.54,
        b"mm" => 96.0 / 25.4,
        b"pt" => 4.0 / 3.0,
        b"pc" => 16.0,
        b"%" => viewport / 100.0,
        _ => f64::INFINITY,
    };
    if x == 0.0 { 0.0 } else { x.abs() * factor }
}

/// 表示の領域の大きさを決める要素（svg・use・pattern・symbol）の幅・高さが、表示の領域を広げうる大きさ（利用者の単位）。
/// 割合は 100% までなら親の領域より広がらないので 0、100% を超える割合（入れ子で掛け合わさる）と em・ex・読めない値は無限大。
/// 文書のこの値の最大と [`UNKNOWN_VIEWPORT`] の大きい方が、どの表示の領域の大きさの上限にもなる（svg の大きさを省くと
/// 親の 100%、use の大きさを省くと今の領域）。
pub(crate) fn viewport_size(n: &SvgNode<'_, '_>) -> f64 {
    if !matches!(n.tag_name().name(), "svg" | "use" | "pattern" | "symbol") {
        return 0.0;
    }
    ["width", "height"]
        .iter()
        .filter_map(|name| attr(n, name))
        .map(|v| {
            if v.trim().ends_with('%') {
                if length_bound(v, 100.0) <= 100.0 {
                    0.0
                } else {
                    f64::INFINITY
                }
            } else {
                length_bound(v, f64::INFINITY)
            }
        })
        .fold(0.0, crate::math::max)
}

/// path のデータが表す点（制御点を含む）の座標の大きさの上限。絶対の命令（大文字）の数の絶対値の最大を A、相対の命令（小文字）の
/// 数の絶対値の和を R として、端の点と書いた制御点はどれも A + R を超えない（絶対の命令で A 以内に戻り、相対の命令で R まで
/// 離れる。命令の引数の区切りを読み違えても、数がどちらの命令のものかは文字の位置で決まる）。弧（中心は端から半径の内、
/// 曲線の制御点は半径の約 1.14 倍の内）と S の反射を見込んで 4 倍、T の連続（制御点は前の制御点の反射で、続けるたびに
/// 2(A + R) ずつ遠くなる）は (2m + 1) 倍（m は続く T の数）。
pub(crate) fn path_extent(d: &str) -> f64 {
    let (mut abs_max, mut rel_sum, mut relative) = (0.0f64, 0.0f64, false);
    // 続く T の数（数の 2 つで 1 つ）と、その最大
    let (mut in_t, mut run, mut longest) = (false, 0u64, 0u64);
    scan_numbers(d, |t| match t {
        Token::Letter(c) => {
            relative = c.is_ascii_lowercase();
            in_t = matches!(c, b'T' | b't');
            if !in_t {
                run = 0;
            }
        }
        Token::Number(x) => {
            if relative {
                rel_sum += x.abs();
            } else {
                abs_max = crate::math::max(abs_max, x.abs());
            }
            if in_t {
                run += 1;
                longest = longest.max(run.div_ceil(2));
            }
        }
    });
    (abs_max + rel_sum) * crate::math::max(4.0, 2.0 * longest as f64 + 1.0)
}

/// polyline・polygon の点の座標の大きさの上限（x と y の絶対値の和の上限）。
fn points_extent(p: &str) -> f64 {
    let mut most = 0.0f64;
    scan_numbers(p, |t| {
        if let Token::Number(x) = t {
            most = crate::math::max(most, x.abs());
        }
    });
    2.0 * most
}

/// [`scan_numbers`] が渡すもの。
enum Token {
    /// 数の外の英字（path の命令）。
    Letter(u8),
    /// 数（読めない数は無限大）。
    Number(f64),
}

/// 値の中の数（SVG の数の書き方：符号・数字・小数点・指数）と、数の外の英字（指数の e・E を除く。path の命令）を順に `f` に
/// 渡す。どの枝でも 1 バイト以上進む。
fn scan_numbers(v: &str, mut f: impl FnMut(Token)) {
    let b = v.as_bytes();
    let digit = |k: usize| b.get(k).is_some_and(u8::is_ascii_digit);
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        let starts = digit(i)
            || (c == b'.' && digit(i + 1))
            || ((c == b'-' || c == b'+')
                && (digit(i + 1) || (b.get(i + 1) == Some(&b'.') && digit(i + 2))));
        if !starts {
            if c.is_ascii_alphabetic() && !matches!(c, b'e' | b'E') {
                f(Token::Letter(c));
            }
            i += 1;
            continue;
        }
        let start = i;
        if matches!(c, b'-' | b'+') {
            i += 1;
        }
        while digit(i) {
            i += 1;
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            while digit(i) {
                i += 1;
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            let mut k = i + 1;
            if matches!(b.get(k), Some(b'-' | b'+')) {
                k += 1;
            }
            if digit(k) {
                while digit(k) {
                    k += 1;
                }
                i = k;
            }
        }
        f(Token::Number(
            b.get(start..i)
                .and_then(|t| std::str::from_utf8(t).ok())
                .and_then(|t| t.parse::<f64>().ok())
                .unwrap_or(f64::INFINITY),
        ));
    }
}

/// textPath の参照先の図形が usvg で path になったときの、点（制御点を含む）の座標の大きさの上限（その図形自身の transform と
/// transform-origin を含む。祖先の変形は usvg も使わない）。path・rect・circle・ellipse・line・polyline・polygon のほかは
/// path にならない（textPath は字を並べない）ので 0。長さの割合は `viewport` の割合、CSS の transform は無限大。
pub(crate) fn shape_extent(n: &SvgNode<'_, '_>, viewport: f64, css_transform: bool) -> f64 {
    let len = |name: &str| attr(n, name).map_or(0.0, |v| length_bound(v, viewport));
    let geometry = match n.tag_name().name() {
        "path" => attr(n, "d").map_or(0.0, path_extent),
        "rect" => len("x") + len("y") + len("width") + len("height"),
        "circle" => len("cx") + len("cy") + 2.0 * len("r"),
        "ellipse" => len("cx") + len("cy") + 2.0 * (len("rx") + len("ry")),
        "line" => len("x1") + len("y1") + len("x2") + len("y2"),
        "polyline" | "polygon" => attr(n, "points").map_or(0.0, points_extent),
        _ => return 0.0,
    };
    if css_transform || attr(n, "style").is_some_and(|s| s.contains("transform")) {
        return f64::INFINITY;
    }
    let (s, mut t) = attr(n, "transform").map_or((1.0, 0.0), transform_bound);
    // transform-origin は、変形を translate(o) · T · translate(−o) にする（平行移動が (s + 1)|o| 増える）
    if let (Some(_), Some(o)) = (attr(n, "transform"), attr(n, "transform-origin")) {
        let origin: f64 = o
            .split(|c: char| is_xml_space(c) || c == ',')
            .filter(|t| !t.is_empty())
            .map(|t| match t {
                "left" | "center" | "right" | "top" | "bottom" => viewport,
                _ => length_bound(t, viewport),
            })
            .sum();
        t += (s + 1.0) * origin;
    }
    let e = s * geometry + t;
    if e.is_nan() { f64::INFINITY } else { e }
}

/// viewBox の拡大の上限（表示の領域の長さ ÷ viewBox の短い辺）。viewBox がなければ 1、読めなければ無限大。
fn view_box_scale(n: &SvgNode<'_, '_>, viewport: f64) -> f64 {
    let Some(vb) = attr(n, "viewBox") else {
        return 1.0;
    };
    let nums: Vec<f64> = vb
        .split(|c: char| is_xml_space(c) || c == ',')
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f64>().ok())
        .collect();
    match nums.as_slice() {
        [_, _, w, h] if w.is_finite() && h.is_finite() && *w > 0.0 && *h > 0.0 => {
            viewport / crate::math::min(*w, *h)
        }
        _ => f64::INFINITY,
    }
}

/// 根の svg の大きさの上限（幅と高さの大きい方）。どちらもなければ None（usvg は viewBox の大きさで描くので拡大しない）。
/// 割合は viewBox の長い辺の割合（usvg と同じ）、em・ex・読めない値は無限大。
fn root_viewport(n: &SvgNode<'_, '_>) -> Option<f64> {
    let vb = attr(n, "viewBox").map_or(100.0, |v| {
        v.split(|c: char| is_xml_space(c) || c == ',')
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse::<f64>().ok())
            .skip(2)
            .fold(0.0, |a, x| crate::math::max(a, x.abs()))
    });
    let (w, h) = (attr(n, "width"), attr(n, "height"));
    if w.is_none() && h.is_none() {
        return None;
    }
    Some(
        [w, h]
            .into_iter()
            .map(|v| v.map_or(vb, |v| length_bound(v, vb)))
            .fold(0.0, crate::math::max),
    )
}

/// [`own_scale`] が文書全体から受け取るもの。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScaleContext {
    /// `<style>` に transform が書かれている（どの要素にも当たりうる）。
    pub css_transform: bool,
    /// 表示の領域（入れ子の svg・use の symbol・pattern のタイル）の大きさの上限（[`viewport_size`] の最大と
    /// [`UNKNOWN_VIEWPORT`] の大きい方）。
    pub viewport: f64,
    /// pattern の viewBox の拡大のいちばん大きいもの（viewBox は href の先から受け継ぐので、自分の viewBox のない pattern に使う）。
    pub pattern_view_box: f64,
}

/// pattern の viewBox の拡大の上限（受け継がない分）。[`ScaleContext::pattern_view_box`] を作るのに使う。
pub(crate) fn pattern_view_box_scale(n: &SvgNode<'_, '_>, viewport: f64) -> f64 {
    if n.tag_name().name() == "pattern" {
        view_box_scale(n, viewport)
    } else {
        1.0
    }
}

/// 要素がその中身の文字のレイアウトに掛ける拡大の上限（transform・pattern の patternTransform と viewBox・svg と symbol の
/// viewBox）。中身の単位が objectBoundingBox の pattern・clipPath・mask は無限大（mask の中身は、マスクする要素の外接矩形の
/// 大きさで並べる。外接矩形は中の変形と座標で大きくできるので上限が決まらない。検証のあとの確認 R1）。
pub(crate) fn own_scale(n: &SvgNode<'_, '_>, is_root: bool, cx: &ScaleContext) -> f64 {
    if cx.css_transform || attr(n, "style").is_some_and(|s| s.contains("transform")) {
        return f64::INFINITY;
    }
    let mut s = attr(n, "transform").map_or(1.0, transform_scale);
    let obb = |name: &str| attr(n, name).map(str::trim) == Some("objectBoundingBox");
    match n.tag_name().name() {
        "pattern" => {
            s *= attr(n, "patternTransform").map_or(1.0, transform_scale);
            s *= if attr(n, "viewBox").is_none() && n.attributes().any(|a| a.name() == "href") {
                cx.pattern_view_box
            } else {
                view_box_scale(n, cx.viewport)
            };
            if obb("patternContentUnits") {
                s = f64::INFINITY;
            }
        }
        "clipPath" if obb("clipPathUnits") => s = f64::INFINITY,
        "mask" if obb("maskContentUnits") => s = f64::INFINITY,
        "svg" if is_root => match root_viewport(n) {
            Some(v) if attr(n, "viewBox").is_some() => s *= view_box_scale(n, v),
            _ => {}
        },
        "svg" | "symbol" => s *= view_box_scale(n, cx.viewport),
        _ => {}
    }
    s
}

/// 文字のレイアウトの残りの手間（フォントを選ぶ・代わりを探す分）。尽きたら印を立てる。
pub(crate) struct TextGuard {
    left: AtomicU64,
    over: AtomicBool,
}

impl TextGuard {
    /// 残り `left` の手間で始める。
    pub(crate) fn new(left: u64) -> Self {
        Self {
            left: AtomicU64::new(left),
            over: AtomicBool::new(false),
        }
    }

    /// `n` を引く。足りなければ印を立てて偽（以後はいつも偽）。
    fn spend(&self, n: u64) -> bool {
        if self.over.load(Ordering::Relaxed) {
            return false;
        }
        let left = self.left.load(Ordering::Relaxed);
        if left < n {
            self.over.store(true, Ordering::Relaxed);
            return false;
        }
        self.left.store(left - n, Ordering::Relaxed);
        true
    }

    /// 手間が尽きたか。
    pub(crate) fn over(&self) -> bool {
        self.over.load(Ordering::Relaxed)
    }

    /// 残りの手間（尽きていれば 0）。木を作ったあと、描く前の見積もり（文字の輪郭の描き直し）に渡す。
    pub(crate) fn left(&self) -> u64 {
        if self.over() {
            0
        } else {
            self.left.load(Ordering::Relaxed)
        }
    }
}

/// 代わりのフォントの選び方の覚え（字と、使ったフォントの並び → 選んだもの）。
type FallbackMemo =
    std::sync::Mutex<BTreeMap<(char, Vec<usvg::fontdb::ID>), Option<usvg::fontdb::ID>>>;

/// usvg の既定のフォントの選び方を、呼ぶたびに `guard` から手間を引いて呼ぶ。尽きたら選ばない（文字を描かない。呼び手は
/// 木を作り終えたら `guard.over()` を見て断る）。`faces` は集まりの書体の数、`reshape` はいちばん重い塊を 1 度形にし直す
/// 手間（代わりのフォントが見つかると塊を丸ごと形にし直す）。
///
/// 代わりのフォントの選び方は、同じ字・同じ使ったフォントの並びなら同じ答えになる（集まりは木を作るあいだ変わらない）ので、
/// 1 つの木の中で覚えて、2 度目からは調べずに同じ答えを返す（手間も引かない。結果は既定の選び方と同じ）。ラベルの多い和文の
/// 図では、同じ字で代わりを探すことが多い。
pub(crate) fn font_resolver(
    guard: Arc<TextGuard>,
    faces: u64,
    reshape: u64,
) -> usvg::FontResolver<'static> {
    let select = usvg::FontResolver::default_font_selector();
    let fallback = usvg::FontResolver::default_fallback_selector();
    let memo: FallbackMemo = std::sync::Mutex::new(BTreeMap::new());
    let g = guard.clone();
    usvg::FontResolver {
        select_font: Box::new(move |font, db| {
            let names = font.families().len() as u64 + 1;
            let cost = SELECT_FONT_WORK
                .saturating_add(names.saturating_mul(faces).saturating_mul(FAMILY_FACE_WORK));
            if g.spend(cost) {
                select(font, db)
            } else {
                None
            }
        }),
        select_fallback: Box::new(move |c, used, db| {
            let key = (c, used.to_vec());
            // 見つかれば塊を形にし直すので、その分は覚えた答えでも引く
            if let Some(hit) = memo.lock().ok().and_then(|m| m.get(&key).copied()) {
                return match hit {
                    Some(id) if guard.spend(reshape) => Some(id),
                    _ => None,
                };
            }
            let cost = faces
                .saturating_mul(FALLBACK_FACE_WORK)
                .saturating_add(reshape);
            if !guard.spend(cost) {
                return None;
            }
            let found = fallback(c, used, db);
            if let Ok(mut m) = memo.lock() {
                m.insert(key, found);
            }
            found
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(svg: &str) -> TextCost {
        let doc = usvg::roxmltree::Document::parse(svg).unwrap();
        let t = doc
            .descendants()
            .find(|n| n.tag_name().name() == "text")
            .unwrap();
        text_cost(&t, &|tp| {
            let id = tp
                .attributes()
                .find(|a| a.name() == "href")
                .and_then(|a| a.value().strip_prefix('#'))
                .unwrap_or("");
            doc.descendants()
                .filter(|n| n.attribute("id") == Some(id))
                .map(|n| path_segments(&n))
                .max()
                .unwrap_or(0)
        })
    }

    fn wrap(body: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">{body}</svg>"#)
    }

    /// 1 つの塊は字の数の 2 乗で、位置を付けた tspan で分けると 1 次になる。
    #[test]
    fn long_chunks_cost_quadratically() {
        let one = |n: usize| cost(&wrap(&format!("<text>{}</text>", "a".repeat(n))));
        assert_eq!(one(1000).reshape, 1000 * SHAPE_WORK[0]);
        assert!(one(2000).work > 3 * one(1000).work);
        let lines = cost(&wrap(&format!(
            "<text>{}</text>",
            r#"<tspan x="0" y="1">aaaaaaaaaa</tspan>"#.repeat(200)
        )));
        assert_eq!(lines.reshape, 10 * SHAPE_WORK[0]);
        assert!(lines.work.saturating_mul(3) < one(2000).work);
        // 上限の目安：'a' の 1 つの塊は約 5,000 字まで
        assert!(one(4_800).work < MAX_TEXT_WORK);
        assert!(one(5_500).work > MAX_TEXT_WORK);
    }

    /// 分けると言い切れない所（空白だけの最初の節・空の tspan・指数や単位のある位置・textPath・text 以外の要素の x）では
    /// 分けない。
    #[test]
    fn uncertain_splits_keep_the_chunk_long() {
        let a = "a".repeat(100);
        let same =
            |body: String| cost(&wrap(&format!("<text>{body}</text>"))).reshape / SHAPE_WORK[0];
        assert_eq!(same(format!(r#"{a}<tspan x="1"> </tspan>{a}"#)), 201);
        assert_eq!(same(format!(r#"{a}<tspan x="1"/>{a}"#)), 200);
        assert_eq!(same(format!(r#"{a}<tspan x="1e1">{a}</tspan>"#)), 200);
        assert_eq!(same(format!(r#"{a}<tspan x="1em">{a}</tspan>"#)), 200);
        assert_eq!(same(format!(r#"{a}<a x="1">{a}</a>"#)), 200);
        assert_eq!(same(format!(r#"{a}<tspan x=" 1, 2">  {a}</tspan>"#)), 102);
        assert_eq!(same(format!(r#"{a}<tspan y="-1.5">{a}</tspan>"#)), 100);
    }

    /// span の数 × 塊の字の数、textPath の区間の数 × 字の数を数える。
    #[test]
    fn spans_and_text_paths_add_work() {
        let spans = cost(&wrap(&format!(
            "<text>{}</text>",
            "<tspan>a</tspan>".repeat(600)
        )));
        assert!(spans.work > 600 * 600 * SHAPE_WORK[0]);
        let d = format!("M0 0{}", " L1 1".repeat(10_000));
        let tp = cost(&wrap(&format!(
            r##"<path id="p" d="{d}"/><text><textPath href="#p">{}</textPath></text>"##,
            "a".repeat(10)
        )));
        assert!(tp.work >= 10_000 * 10 * PATH_SEGMENT_WORK);
    }

    /// 長い path を参照する textPath の中の多くの文字の節でも、path の区間は textPath ごとに 1 度だけ数える（数える手間が
    /// 節の数 × path の長さにならない）。
    #[test]
    fn text_path_segments_are_counted_once_per_text_path() {
        let d = format!("M0 0{}", " L1 1".repeat(200_000));
        let svg = wrap(&format!(
            r##"<path id="p" d="{d}"/><text><textPath href="#p">{}</textPath></text>"##,
            "<tspan>a</tspan>".repeat(2_000)
        ));
        let doc = usvg::roxmltree::Document::parse(&svg).unwrap();
        let t = doc
            .descendants()
            .find(|n| n.tag_name().name() == "text")
            .unwrap();
        let path = doc
            .descendants()
            .find(|n| n.attribute("id") == Some("p"))
            .unwrap();
        let calls = std::cell::Cell::new(0u32);
        let c = text_cost(&t, &|_| {
            calls.set(calls.get() + 1);
            path_segments(&path)
        });
        assert_eq!(calls.get(), 1);
        assert!(c.work > MAX_TEXT_WORK);
    }

    /// 字の組ごとの手間：アラビア文字を 1 字含む塊は、塊の字すべてを重い組で形にし直す。和文のラベルは 450 個ほど読める。
    #[test]
    fn heavy_scripts_cost_more() {
        assert_eq!(class_of('a'), 0);
        assert_eq!(class_of('é'), 0);
        assert_eq!(class_of('Ж'), 0);
        assert_eq!(class_of('\u{0301}'), 0);
        assert_eq!(class_of('あ'), 1);
        assert_eq!(class_of('龘'), 1);
        assert_eq!(class_of('😀'), 1);
        assert_eq!(class_of('ب'), 2);
        assert_eq!(class_of('क'), 2);
        assert_eq!(class_of('\u{202E}'), 2);
        let latin = cost(&wrap(&format!("<text>{}</text>", "a".repeat(100))));
        let mixed = cost(&wrap(&format!("<text>{}ب</text>", "a".repeat(99))));
        assert_eq!(mixed.reshape, 100 * SHAPE_WORK[2]);
        assert!(mixed.work > latin.work * 10);
        let label = cost(&wrap("<text>東京都千代田区丸の内</text>"));
        let labels = MAX_TEXT_WORK / (label.work + 11_000);
        assert!((400..600).contains(&labels), "{labels}");
    }

    #[test]
    fn transform_scales_are_bounded() {
        assert_eq!(transform_scale(""), 1.0);
        assert_eq!(transform_scale("translate(5 6) rotate(30, 1, 2)"), 1.0);
        assert_eq!(transform_scale("scale(1e6) scale(2,3)"), 3e6);
        assert_eq!(transform_scale("matrix(2 0 0 -4 9 9)"), 6.0);
        assert!(transform_scale("skewX(89.9999999999)") > 1e9);
        assert!(transform_scale("scale(2) bogus(1)").is_infinite());
        assert!(transform_scale("scale(").is_infinite());
        assert!(transform_scale("scale(1e400)").is_infinite());
    }

    /// 検証のあとの確認 R1：変形の平行移動・長さの単位・path の座標・表示の領域の大きさの上限。
    #[test]
    fn extents_are_upper_bounds() {
        assert_eq!(transform_bound(""), (1.0, 0.0));
        assert_eq!(transform_bound("translate(3 -4)"), (1.0, 7.0));
        // 左ほど外側：scale(2) translate(5) は p ↦ 2p + 10
        assert_eq!(transform_bound("scale(2) translate(5)"), (2.0, 10.0));
        assert_eq!(transform_bound("translate(5) scale(2)"), (2.0, 5.0));
        assert_eq!(transform_bound("matrix(1 0 0 1 1e9 1e9)"), (2.0, 2e9));
        assert_eq!(transform_bound("rotate(90 10 20)"), (1.0, 60.0));
        // 1 より小さい拡大も、そのまま（座標を小さくする）
        assert_eq!(transform_bound("scale(1e-9)"), (1e-9, 0.0));
        assert!(transform_bound("scale(").0.is_infinite());
        assert_eq!(length_bound("12", 1.0), 12.0);
        assert_eq!(length_bound(" -3px ", 1.0), 3.0);
        assert_eq!(length_bound("1in", 1.0), 96.0);
        assert_eq!(length_bound("1e9in", 1.0), 96e9);
        assert_eq!(length_bound("50%", 200.0), 100.0);
        assert_eq!(length_bound("0em", 1.0), 0.0);
        for v in ["1em", "2ex", "1e9em", "auto", "", "px", "1vw", "--1"] {
            assert!(length_bound(v, 1.0).is_infinite(), "{v}");
        }
        // 絶対の命令は最大、相対の命令は和（相対の命令で重ねた座標を見落とさない）
        assert_eq!(path_extent("M0 0 L5 6 L-7 2"), 7.0 * 4.0);
        assert_eq!(
            path_extent(&format!("M0 0{}", " l1e9 1e9".repeat(1_000))),
            2e12 * 4.0
        );
        assert_eq!(path_extent("M10 10 l1 -2 L3 4"), (10.0 + 3.0) * 4.0);
        // T の連続は、続く数で制御点が遠くなる
        assert_eq!(
            path_extent(&format!("M0 0 Q1 1 1 0{}", " T0 0 T1 0".repeat(10))),
            41.0
        );
        assert_eq!(
            path_extent("M0 0 Q1 1 1 0 T0 0 L0 0 T1 0"),
            4.0,
            "T は続く間だけ"
        );
        // 区切りのない弧の印（01）は大きな数として読む（小さく読み違えない）
        assert!(path_extent("M0 0 a25 25 0 0150 50") >= (25.0 + 25.0 + 50.0 + 50.0) * 4.0);
        let shape = |s: &str| {
            let text = format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{s}</svg>"#);
            let d = usvg::roxmltree::Document::parse(&text).unwrap();
            let n = d
                .descendants()
                .find(|n| n.attribute("id") == Some("p"))
                .unwrap();
            shape_extent(&n, 1e9, false)
        };
        assert_eq!(shape(r#"<path id="p" d="M0 0 L5 6"/>"#), 24.0);
        assert_eq!(
            shape(r#"<path id="p" d="M0 0 L5 6" transform="scale(1e9) scale(1e9)"/>"#),
            24e18
        );
        assert_eq!(
            shape(r#"<rect id="p" x="1" y="2" width="1in" height="3"/>"#),
            102.0
        );
        assert_eq!(shape(r#"<circle id="p" cx="1" cy="2" r="3"/>"#), 9.0);
        assert_eq!(shape(r#"<polygon id="p" points="1,-7 3,4"/>"#), 14.0);
        assert_eq!(shape(r#"<use id="p" href="x" width="1e9em"/>"#), 0.0);
        assert!(shape(r#"<rect id="p" width="1e9em" height="1"/>"#).is_infinite());
        assert!(shape(r#"<path id="p" d="M0 0 L1 1" style="transform:scale(2)"/>"#).is_infinite());
        // transform-origin は、変形があるときだけ平行移動を足す（(s + 1)|o|）
        assert_eq!(
            shape(r#"<path id="p" d="M0 0 L1 1" transform="scale(2)" transform-origin="10 20"/>"#),
            2.0 * 4.0 + 3.0 * 30.0
        );
        assert_eq!(
            shape(r#"<path id="p" d="M0 0 L1 1" transform-origin="1e9em 0"/>"#),
            4.0
        );
        assert!(
            shape(r#"<path id="p" d="M0 0 L1 1" transform="scale(2)" transform-origin="1em 0"/>"#)
                .is_infinite()
        );
        // 表示の領域を広げうる大きさ：100% までの割合は広げない、em・100% を超える割合は分からない
        let vp = |s: &str| {
            let text = format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{s}</svg>"#);
            let d = usvg::roxmltree::Document::parse(&text).unwrap();
            let n = d
                .descendants()
                .find(|n| n.attribute("id") == Some("v"))
                .unwrap();
            viewport_size(&n)
        };
        assert_eq!(vp(r#"<svg id="v" width="100%" height="50%"/>"#), 0.0);
        assert_eq!(vp(r#"<use id="v" width="2in" height="3"/>"#), 192.0);
        assert_eq!(vp(r#"<rect id="v" width="1e9em"/>"#), 0.0);
        assert!(vp(r#"<svg id="v" width="101%"/>"#).is_infinite());
        assert!(vp(r#"<pattern id="v" width="1em"/>"#).is_infinite());
    }

    #[test]
    fn the_guard_stops_once_spent() {
        let g = TextGuard::new(10);
        assert!(g.spend(4));
        assert!(g.spend(6));
        assert!(!g.over());
        assert!(!g.spend(1));
        assert!(g.over());
        assert!(!g.spend(0));
    }
}
