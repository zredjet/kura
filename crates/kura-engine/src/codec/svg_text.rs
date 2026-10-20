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
//!   （[`own_scale`] の積）を [`MAX_TEXT_PATH_SCALE`] で断る。
//!
//! 手間の単位は、M4 Max の release で測った時間（ns）の目安。上限 [`MAX_TEXT_WORK`] は 1 件 100 ms ほど
//! （ラテン文字なら、1 つの塊は約 5,000 字、100 字ずつの text は合わせて約 3 万字、1 字ずつの span の塊は約 600 字。
//! 和文は 10 字のラベルで約 450 個）。字ごとの手間は文字の種類で大きく違う（輪郭の込み入った字・アラビア文字の字形の
//! 組み立て・左右の向きの混在）ので、字を 3 つの組（[`class_of`]）に分けて、組ごとに測った最悪の値で数える。

use std::collections::BTreeMap;
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
/// 単位が objectBoundingBox の中身の拡大（参照する要素の外接矩形の大きさ。座標は 10⁹ までなので 4×10⁹ とみなす）。
const OBB_SCALE: f64 = 4e9;
/// 大きさの分からない表示の領域（入れ子の svg・symbol。親の大きさの割合や use の幅で決まる）。
const UNKNOWN_VIEWPORT: f64 = 1e9;

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
    // 位置を付けた要素で、まだ最初の字が来ていないもの
    let mut pending: Vec<SvgNode<'_, '_>> = Vec::new();
    let close = |c: &mut Chunk, work: &mut u64, reshape: &mut u64| {
        *work = work.saturating_add(c.work());
        *reshape = (*reshape).max(c.reshape());
        *c = Chunk::default();
    };
    for d in t.descendants() {
        if d.is_element() {
            if positioned(&d) {
                pending.push(d);
            }
            continue;
        }
        let Some(s) = d.text().filter(|s| !s.is_empty()) else {
            continue;
        };
        if d.is_text() {
            let first = pending.iter().any(|p| d.ancestors().any(|a| a == *p));
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
                cur.path = cur.path.max(path_segments(&tp));
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

/// 変形のリストの拡大の上限（各関数の行列のノルムの上限の積）。読めなければ無限大。
pub(crate) fn transform_scale(v: &str) -> f64 {
    let mut s = 1.0f64;
    let mut rest = v.trim_matches(|c: char| is_xml_space(c) || c == ',');
    while !rest.is_empty() {
        let Some(open) = rest.find('(') else {
            return f64::INFINITY;
        };
        let Some(close) = rest.find(')') else {
            return f64::INFINITY;
        };
        if close < open {
            return f64::INFINITY;
        }
        let name = rest[..open].trim();
        let nums: Option<Vec<f64>> = rest[open + 1..close]
            .split(|c: char| is_xml_space(c) || c == ',')
            .filter(|t| !t.is_empty())
            .map(|t| t.parse::<f64>().ok().filter(|x| x.is_finite()))
            .collect();
        let Some(nums) = nums else {
            return f64::INFINITY;
        };
        let at = |k: usize| nums.get(k).copied().unwrap_or(0.0).abs();
        let one = match (name, nums.len()) {
            ("matrix", 6) => at(0) + at(1) + at(2) + at(3),
            ("translate", 1 | 2) | ("rotate", 1 | 3) => 1.0,
            ("scale", 1) => at(0),
            ("scale", 2) => crate::math::max(at(0), at(1)),
            ("skewX" | "skewY", 1) => {
                let r = nums.first().copied().unwrap_or(0.0) * (std::f64::consts::PI / 180.0);
                let c = crate::math::cos(r).abs();
                if c < 1e-9 {
                    f64::INFINITY
                } else {
                    1.0 + (crate::math::sin(r) / c).abs()
                }
            }
            _ => return f64::INFINITY,
        };
        s *= crate::math::max(one, 1.0);
        rest = rest[close + 1..].trim_start_matches(|c: char| is_xml_space(c) || c == ',');
    }
    s
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

/// 根の svg の大きさ（素直な数か px）。なければ None、ほかの単位・割合は分からない（UNKNOWN_VIEWPORT）。
fn root_viewport(n: &SvgNode<'_, '_>) -> Option<f64> {
    let len = |name: &str| {
        attr(n, name).map(|v| {
            let v = v.trim();
            let v = v.strip_suffix("px").unwrap_or(v);
            if plain_number(v) {
                v.parse::<f64>().map_or(UNKNOWN_VIEWPORT, f64::abs)
            } else {
                UNKNOWN_VIEWPORT
            }
        })
    };
    match (len("width"), len("height")) {
        (None, None) => None,
        (w, h) => Some(crate::math::max(
            w.unwrap_or(UNKNOWN_VIEWPORT),
            h.unwrap_or(UNKNOWN_VIEWPORT),
        )),
    }
}

/// 要素がその中身に掛ける拡大の上限（transform・pattern の patternTransform と viewBox・単位が objectBoundingBox の中身・
/// svg と symbol の viewBox）。`css_transform` は `<style>` に transform が書かれているとき（どの要素にも当たりうる）。
pub(crate) fn own_scale(n: &SvgNode<'_, '_>, is_root: bool, css_transform: bool) -> f64 {
    if css_transform || attr(n, "style").is_some_and(|s| s.contains("transform")) {
        return f64::INFINITY;
    }
    let mut s = attr(n, "transform").map_or(1.0, transform_scale);
    let obb = |name: &str| attr(n, name).map(str::trim) == Some("objectBoundingBox");
    match n.tag_name().name() {
        "pattern" => {
            s *= attr(n, "patternTransform").map_or(1.0, transform_scale);
            s *= view_box_scale(n, UNKNOWN_VIEWPORT);
            if obb("patternContentUnits") {
                s *= OBB_SCALE;
            }
        }
        "clipPath" if obb("clipPathUnits") => s *= OBB_SCALE,
        "mask" if obb("maskContentUnits") => s *= OBB_SCALE,
        "svg" if is_root => match root_viewport(n) {
            Some(v) if attr(n, "viewBox").is_some() => s *= view_box_scale(n, v),
            _ => {}
        },
        "svg" | "symbol" => s *= view_box_scale(n, UNKNOWN_VIEWPORT),
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
