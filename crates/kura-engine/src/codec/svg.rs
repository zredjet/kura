//! SVG（resvg・usvg。版は `=` で固定。`svg-resvg-dec/1`）。書き出しはしない。
//!
//! - resvg は中で std の三角関数（OS の libm）を使うので、同じ環境では同じ値だが、OS の版で変わりうる。そこで
//!   `reproducible()` を偽にして、初めて編集するときに読み取った値を可逆形式で残す（アンカー。docs/editing.md §9.4）。
//! - 大きさは SVG の幅・高さ（ユーザー単位、96 dpi）を切り上げた画素。Rgba8（乗算済みの値を戻したもの）。
//! - `<image>`（外部のファイル・data: のラスター画像・入れ子の SVG）を含むものは、この段では `Unsupported`（次の段へ）。
//!   手元のファイルは読まない。resvg の raster-images は zune-jpeg の SIMD を有効にするので入れない。入れ子の SVG は、
//!   外側と同じ見積もりを通らないので読まない。
//! - 文字は、渡したフォント（`SvgCodec::with_svg_fonts`。`fonts::SvgFonts`）だけで描く。フォントを渡していないのに文字が
//!   あれば `Unsupported`。遅れて作る集まり（`SvgFonts::lazy`）は、文字のある SVG を読むときに初めて作る。
//! - 展開爆弾を避けるため、usvg に渡す前に、ファイルの大きさ・DTD の実体の展開の大きさ・要素の数・`<use>` の展開・
//!   marker の頂点ごとの複製を見積もって断る。DTD は引用符・注釈を区別して読み、パラメタ実体・外部の実体・
//!   マークアップや参照を含む実体は断る（`Unsupported`）。入れ子の marker も断る。
//! - roxmltree・usvg は入れ子の深さだけ再帰し、スタックを使い切ると受け止められずに落ちる。そこで XML を読む前に、
//!   生のバイト列で入れ子の深さを数えて 64 段で断り、参照（use・url(#…)・祖先から受け継ぐ fill・stroke・marker・CSS の規則）を
//!   展開した高さも 64 段で断る（`MAX_DEPTH`）。参照の輪（pattern の中身が受け継いだ fill で自分を描き直す、など）も断る。
//!   描く時間は中止できない（resvg を途中で止める口がない）。

use std::io::SeekFrom;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use resvg::tiny_skia;
use resvg::usvg;

use crate::codec::fonts::SvgFonts;
use crate::codec::svg_text;
use crate::codec::{Decoder, ReadSeek, Tier, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};
use crate::value::{Image, ImageMeta, PixelFormat, Raster, Value};

const DEC_ID: &str = "svg-resvg-dec/1";
/// 読む SVG の大きさの上限（バイト）。
const MAX_SOURCE: u64 = 16 << 20;
/// DTD の実体を展開したあとの文字の数の上限。
const MAX_EXPANDED: u64 = 64 << 20;
/// 要素の数（`<use>` の展開を含む）の上限。
const MAX_ELEMENTS: u64 = 1 << 21;
/// 入れ子の深さ（要素の段の数。use・pattern などの参照の展開を含む）の上限。roxmltree・usvg・resvg は入れ子の深さだけ
/// 再帰するので、作業スレッドの既定のスタック（2 MiB）で、デバッグのビルドでも余裕のある深さに抑える。各段に svg・不透明度・
/// clip-path・mask・filter を付けた重い形で、デバッグのビルドは 96 段まで動き 128 段であふれた（release は 160 段でも動く）。
/// 受け付ける深さはビルドで変えない（同じ文書の結果をそろえる）。ふつうの SVG の入れ子は 20 段ほど。
const MAX_DEPTH: usize = 64;

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: e.to_string(),
    }
}

/// SVG の読み取り。文字を描くフォントを持てる。
#[derive(Clone, Default)]
pub struct SvgCodec {
    fonts: Option<SvgFonts>,
}

impl SvgCodec {
    /// 文字を描くフォントの集まりをそのまま渡す。
    pub fn with_fonts(fonts: Arc<usvg::fontdb::Database>) -> Self {
        Self::with_svg_fonts(SvgFonts::from_database(fonts))
    }

    /// 文字を描くフォントの入れ物を渡す（kura-mac が OS のフォントのファイルを mmap して渡す）。
    pub fn with_svg_fonts(fonts: SvgFonts) -> Self {
        Self { fonts: Some(fonts) }
    }

    /// 文字を描くフォントの集まり。文字がなければ、遅れて作る集まりを作らずに空の集まりを使う。文字があるのに書体が
    /// ひとつもなければ `Unsupported`。
    fn fontdb(&self, has_text: bool) -> Result<Arc<usvg::fontdb::Database>, EngineError> {
        if !has_text {
            return Ok(match &self.fonts {
                Some(f) if f.is_built() => f.database(),
                _ => Arc::new(usvg::fontdb::Database::new()),
            });
        }
        match self.fonts.as_ref().map(SvgFonts::database) {
            Some(db) if !db.is_empty() => Ok(db),
            _ => Err(EngineError::Unsupported(
                "文字を含む SVG は、フォントを渡したときだけ読めます".into(),
            )),
        }
    }
}

fn read_source(src: &mut dyn ReadSeek) -> Result<String, EngineError> {
    let len = src.seek(SeekFrom::End(0))?;
    if len > MAX_SOURCE {
        return Err(EngineError::Limit("SVG が大きすぎます".into()));
    }
    src.seek(SeekFrom::Start(0))?;
    let mut b = try_vec(len as usize, 0u8)?;
    src.read_exact(&mut b)
        .map_err(|_| err("ファイルが読めません"))?;
    // svgz（gzip）は読まない
    if b.starts_with(&[0x1f, 0x8b]) {
        return Err(EngineError::Unsupported(
            "圧縮した SVG（svgz）は読めません".into(),
        ));
    }
    String::from_utf8(b).map_err(|_| err("UTF-8 ではありません"))
}

/// 前置き（XML の宣言・注釈・PI・空白）の後に DOCTYPE があれば、その内部サブセットと、本文の始まりを返す。
/// 引用符・注釈・PI の中の `[`・`]`・`>` は区切りとして扱わない。
fn internal_subset(text: &str) -> Result<Option<(&str, usize)>, EngineError> {
    // バイト列で走査する（多バイト文字の途中で文字列を切らないように）。区切りはどれも ASCII なので、
    // 返す位置は必ず文字の境目になる。
    let b = text.as_bytes();
    let at = |i: usize, pat: &[u8]| b.get(i..).is_some_and(|r| r.starts_with(pat));
    let find = |from: usize, pat: &[u8]| {
        b.get(from..)?
            .windows(pat.len())
            .position(|w| w == pat)
            .map(|k| from + k)
    };
    let mut i = if text.starts_with('\u{feff}') { 3 } else { 0 };
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if at(i, b"<?") {
            i = find(i, b"?>").ok_or_else(|| err("処理命令が閉じていません"))? + 2;
        } else if at(i, b"<!--") {
            i = find(i + 4, b"-->").ok_or_else(|| err("注釈が閉じていません"))? + 3;
        } else if at(i, b"<!DOCTYPE") {
            break;
        } else {
            return Ok(None);
        }
    }
    // DOCTYPE の名前と外部の識別子（引用符の中は読み飛ばす）
    let mut j = i + 9;
    let mut quote: Option<u8> = None;
    while j < b.len() {
        match (quote, b[j]) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b[j]),
            (None, b'>') => return Ok(None),
            (None, b'[') => break,
            _ => {}
        }
        j += 1;
    }
    if j >= b.len() {
        return Err(err("DOCTYPE が閉じていません"));
    }
    let start = j + 1;
    let mut k = start;
    while k < b.len() {
        if at(k, b"<!--") {
            k = find(k + 4, b"-->").ok_or_else(|| err("注釈が閉じていません"))? + 3;
            continue;
        }
        if at(k, b"<?") {
            k = find(k + 2, b"?>").ok_or_else(|| err("処理命令が閉じていません"))? + 2;
            continue;
        }
        match b[k] {
            q @ (b'"' | b'\'') => {
                k = find(k + 1, &[q]).ok_or_else(|| err("引用符が閉じていません"))? + 1;
                continue;
            }
            b']' => {
                let mut e = k + 1;
                while e < b.len() && b[e].is_ascii_whitespace() {
                    e += 1;
                }
                if b.get(e) != Some(&b'>') {
                    return Err(err("DOCTYPE が閉じていません"));
                }
                let subset = text
                    .get(start..k)
                    .ok_or_else(|| err("DOCTYPE が正しくありません"))?;
                return Ok(Some((subset, e + 1)));
            }
            _ => k += 1,
        }
    }
    Err(err("DOCTYPE が閉じていません"))
}

/// 内部サブセットの実体の宣言を読む。パラメタ実体・外部の実体・マークアップを含む値・入れ子の参照は断る
/// （展開の大きさを、本文の参照の数 × 値の長さで正しく見積もれるものだけを読む）。
fn entities(subset: &str) -> Result<Vec<(String, usize)>, EngineError> {
    let b = subset.as_bytes();
    let mut out = Vec::new();
    let mut k = 0;
    let unsupported = |m: &str| EngineError::Unsupported(format!("この DTD は読めません（{m}）"));
    while k < b.len() {
        let rest = &subset[k..];
        if let Some(r) = rest.strip_prefix("<!--") {
            k += r.find("-->").ok_or_else(|| err("注釈が閉じていません"))? + 7;
        } else if let Some(r) = rest.strip_prefix("<?") {
            k += r
                .find("?>")
                .ok_or_else(|| err("処理命令が閉じていません"))?
                + 4;
        } else if let Some(r) = rest.strip_prefix("<!ENTITY") {
            let mut t = r.trim_start();
            if t.starts_with('%') {
                return Err(unsupported("パラメタ実体"));
            }
            let name_end = t
                .find(|c: char| c.is_whitespace())
                .ok_or_else(|| err("ENTITY が正しくありません"))?;
            let name = t[..name_end].to_string();
            t = t[name_end..].trim_start();
            let q = t
                .chars()
                .next()
                .ok_or_else(|| err("ENTITY が正しくありません"))?;
            if q != '"' && q != '\'' {
                return Err(unsupported("外部の実体"));
            }
            let value_end = t[1..]
                .find(q)
                .ok_or_else(|| err("引用符が閉じていません"))?;
            let value = &t[1..1 + value_end];
            if value.contains('<') || value.contains('&') || value.contains('%') {
                return Err(unsupported("マークアップか参照を含む実体"));
            }
            out.push((name, value.len()));
            let after = &t[1 + value_end + 1..];
            let close = after
                .find('>')
                .ok_or_else(|| err("ENTITY が閉じていません"))?;
            k = subset.len() - after.len() + close + 1;
        } else if rest.starts_with("<!") {
            // ELEMENT・ATTLIST・NOTATION は引用符を見ながら '>' まで飛ばす
            let mut e = 2;
            let mut quote: Option<u8> = None;
            let rb = rest.as_bytes();
            while e < rb.len() {
                match (quote, rb[e]) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, b'"' | b'\'') => quote = Some(rb[e]),
                    (None, b'>') => break,
                    _ => {}
                }
                e += 1;
            }
            k += e + 1;
        } else if rest.starts_with('%') {
            return Err(unsupported("パラメタ実体の参照"));
        } else {
            k += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    Ok(out)
}

/// DTD の実体を展開したときの文字の数を見積もり、上限を超えれば断る。
fn check_entities(text: &str) -> Result<(), EngineError> {
    let Some((subset, body_at)) = internal_subset(text)? else {
        return Ok(());
    };
    let body = &text[body_at..];
    let mut total = body.len() as u64;
    for (name, len) in entities(subset)? {
        let count = body.matches(&format!("&{name};")).count() as u64;
        total = total.saturating_add(count.saturating_mul(len as u64));
    }
    if total > MAX_EXPANDED {
        return Err(EngineError::Limit("実体を展開すると大きすぎます".into()));
    }
    Ok(())
}

/// `marker` を使うときに描く形の頂点の数の見積もり（数の数 ÷ 2 + 1。上限あり）。
fn vertices(n: &usvg::roxmltree::Node<'_, '_>) -> u64 {
    let numbers = |s: &str| {
        s.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == 'E'))
            .filter(|t| !t.is_empty())
            .count() as u64
    };
    match n.tag_name().name() {
        "path" => n.attribute("d").map_or(0, |d| {
            // 命令の文字も頂点になりうる（相対の連続など）。数の数と命令の数の大きいほう
            let cmds = d
                .chars()
                .filter(|c| c.is_ascii_alphabetic() && !matches!(c, 'e' | 'E'))
                .count() as u64;
            (numbers(d) / 2 + 1).max(cmds)
        }),
        "polyline" | "polygon" => n.attribute("points").map_or(0, |p| numbers(p) / 2 + 1),
        "line" => 2,
        _ => 0,
    }
}

/// 要素の marker の参照（属性と style）の id。
fn marker_refs<'a>(n: &usvg::roxmltree::Node<'a, '_>) -> Vec<&'a str> {
    let mut out = Vec::new();
    for a in n.attributes() {
        let name = a.name();
        if name.starts_with("marker") || name == "style" {
            let v = a.value();
            let mut rest = v;
            while let Some(i) = rest.find("url(#") {
                let tail = &rest[i + 5..];
                let end = tail.find(')').unwrap_or(tail.len());
                out.push(tail[..end].trim_end_matches(['"', '\'']).trim());
                rest = &tail[end..];
            }
        }
    }
    out
}

/// XML を読む前に、生のバイト列で要素の入れ子の深さを数え、`MAX_DEPTH` を超えれば断る。
/// roxmltree・usvg は入れ子の深さだけ再帰するので、深すぎる文書はスタックを使い切って落ちる（受け止められない）。
/// 注釈・CDATA・PI は飛ばし、タグの中の引用符の中の `>` は区切りにしない。壊れた文書では多めに数える（断る側に倒れる）。
fn check_depth(text: &str) -> Result<(), EngineError> {
    let b = text.as_bytes();
    let start = internal_subset(text)?.map_or(0, |(_, body)| body);
    let find = |from: usize, pat: &[u8]| {
        b.get(from..)
            .and_then(|r| r.windows(pat.len()).position(|w| w == pat))
            .map(|k| from + k)
    };
    let (mut i, mut depth) = (start, 0usize);
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            i = find(i + 4, b"-->").map_or(b.len(), |k| k + 3);
            continue;
        }
        if rest.starts_with(b"<![CDATA[") {
            i = find(i + 9, b"]]>").map_or(b.len(), |k| k + 3);
            continue;
        }
        if rest.starts_with(b"<?") {
            i = find(i + 2, b"?>").map_or(b.len(), |k| k + 2);
            continue;
        }
        // タグの終わりの '>'（引用符の中は区切りにしない）
        let mut j = i + 1;
        let mut quote: Option<u8> = None;
        while j < b.len() {
            match (quote, b[j]) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(b[j]),
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
        } else if !rest.starts_with(b"<!") {
            // 中身のない要素（`<x/>`）も 1 段に数える（参照を展開した高さの数え方とそろえる）
            if depth + 1 > MAX_DEPTH {
                return Err(EngineError::Unsupported(format!(
                    "入れ子が {MAX_DEPTH} 段より深い SVG は読めません"
                )));
            }
            if b.get(j.wrapping_sub(1)) != Some(&b'/') {
                depth += 1;
            }
        }
        i = j + 1;
    }
    Ok(())
}

/// 数として読む値の絶対値の上限。usvg は弧・円・楕円・角丸を、半径の大きさに応じた数の曲線に直すので、極端な半径で時間が
/// かかる（半径 10^30 の弧 1 つで 6 秒を超え、ファズの遅い入力は半径 10^37 の楕円だった）。描く前の見積もり（`check_render`）
/// より前の usvg の木を作る段で起きるので、XML を読んだところで断る。ふつうの SVG の数は、地図の座標でも 10^8 より小さい。
const MAX_NUMBER: f64 = 1e9;
/// SVG の名前空間（usvg はこの名前空間の要素だけを読む）。
const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// 属性・宣言の値の中の数の絶対値が `MAX_NUMBER` を超えれば断る。色（`#` に続く英数字）と `url(…)` の中は読まない。
/// 識別子の中の数字（`a1e30` など）は数として読まない（path のデータは命令の文字に数が続くので、`path_data` で区別する）。
fn check_numbers(v: &str, path_data: bool) -> Result<(), EngineError> {
    let b = v.as_bytes();
    let digit = |k: usize| b.get(k).is_some_and(u8::is_ascii_digit);
    let word = |k: usize| {
        b.get(k)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    };
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'#' {
            i += 1;
            while word(i) {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"url(") {
            i = b[i..]
                .iter()
                .position(|&x| x == b')')
                .map_or(b.len(), |k| i + k + 1);
            continue;
        }
        let starts = digit(i)
            || (c == b'.' && digit(i + 1))
            || ((c == b'-' || c == b'+')
                && (digit(i + 1) || (b.get(i + 1) == Some(&b'.') && digit(i + 2))));
        if !starts {
            i += 1;
            continue;
        }
        if !path_data && i > 0 && word(i - 1) {
            // 識別子の続き（今の 1 バイトは必ず進める。`0+3` の `+` のように、続きに入らない文字で止まらないように）
            i += 1;
            while word(i) || matches!(b.get(i), Some(b'-' | b'.')) {
                i += 1;
            }
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
        let value = std::str::from_utf8(&b[start..i])
            .ok()
            .and_then(|t| t.parse::<f64>().ok());
        if value.is_some_and(|x| x.abs() > MAX_NUMBER) {
            return Err(EngineError::Unsupported(
                "極端に大きな数を含む SVG は読めません".into(),
            ));
        }
    }
    Ok(())
}

/// 文字列の中の `url(#id)` の id。
fn url_ids(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = v;
    while let Some(i) = rest.find("url(") {
        let tail = &rest[i + 4..];
        let end = tail.find(')').unwrap_or(tail.len());
        let id = tail[..end].trim().trim_matches(['"', '\'']).trim();
        if let Some(id) = id.strip_prefix('#') {
            out.push(id);
        }
        rest = &tail[end..];
    }
    out
}

/// 受け継がれる性質（ある要素に書くと、その中身にも効くもの）。
fn inheritable(prop: &str) -> bool {
    // usvg（simplecss）が名前をどう読むか（`*fill` のような古い書き方・大文字）に頼らないように、受け継がない参照と分かるもの
    // （clip-path・mask・filter）のほかは、受け継ぐとみなす（見積もりの辺を多めに取る。検証で `*fill` の輪の見逃しを見つけた）
    let name = prop
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii_alphabetic())
        .to_ascii_lowercase();
    !matches!(name.as_str(), "clip-path" | "mask" | "filter")
}

/// 宣言の並び（`a: b; c: url(#x)`）から、(性質, 参照する id)。
fn decl_refs(decls: &str) -> Vec<(&str, &str)> {
    decls
        .split(';')
        .filter_map(|d| d.split_once(':'))
        .flat_map(|(prop, value)| url_ids(value).into_iter().map(move |id| (prop.trim(), id)))
        .collect()
}

/// CSS の規則の数（参照を含むもの）の上限。
const MAX_CSS_RULES: usize = 4_096;
/// セレクタの部分（結合子でつないだもの）の数の上限。simplecss は結合子の数だけ再帰するので、長い連鎖でスタックを使い切る
/// （検証で、`g+g+…` の 11,000 段であふれた）。ふつうの CSS は数段。
const MAX_SELECTOR_PARTS: usize = 32;
/// CSS の手間の上限。usvg はすべてのセレクタをすべての要素に照らし（セレクタの数 × 要素の数。ふつうの SVG は数百 × 数万ほど）、
/// 子孫・`~` の結合子では祖先・兄弟をさかのぼって後戻りし（文書の深さ・兄弟の数の、結合子の数の乗）、当たった規則と style 属性の
/// 宣言を、宣言の数の 2 乗の手間で書き込む（検証で、1 つの規則の 8 万の宣言に 47 秒を測った）。これらの合計を抑える。
const MAX_CSS_MATCHES: u64 = 1 << 27;
/// usvg が参照ごとに中身を複製して作る要素の数の上限（単位が objectBoundingBox の pattern・clipPath・mask と filter）。
/// 複製は入れ子で掛け算になり、木を作る時間は数より速く伸びる（2^18 ほどで十数秒を測った）ので、要素の数の上限より小さくする。
const MAX_CLONES: u64 = 1 << 17;
/// 辺の種類：数に入れない（輪と高さだけ）・数に入れる（子・use の展開）・複製として数える（usvg が参照ごとに中身を複製する）。
const EDGE_LINK: u8 = 0;
const EDGE_COUNT: u8 = 1;
const EDGE_CLONE: u8 = 2;
/// 参照の見積もりの手間（要素ごとの参照と辺の数の合計）の上限。ふつうは要素の数の数倍。中身を持つ要素は祖先の参照も
/// 辺にするので、深い入れ子に参照の多い要素を重ねると増える。
const MAX_ANALYZE_WORK: u64 = 1 << 24;

/// CSS の注釈（`/* … */`）を空白 1 つに置き換える（引用符の中は残す）。`<style>` の本文と `style` 属性の両方に使う。
fn strip_css_comments(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                clean.push(c);
            }
            (Some(_), '\\') => {
                clean.push(c);
                if let Some(n) = chars.next() {
                    clean.push(n);
                }
            }
            (Some(_), c) => clean.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                clean.push(c);
            }
            (None, '/') if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                clean.push(' ');
            }
            (None, c) => clean.push(c),
        }
    }
    clean
}

/// CSS を (セレクタ, 宣言の並び) の組に分ける。注釈は取り除き、引用符の中の `{`・`}`・`;`・`/*` は区切りにしない
/// （usvg が使う simplecss と同じ区切り方。読めない形は、区切りの外の残りとして捨てずにセレクタ「*」の宣言に回す）。
fn css_blocks(text: &str) -> Vec<(String, String)> {
    let clean = strip_css_comments(text);
    let mut out = Vec::new();
    let (mut sel, mut body) = (String::new(), String::new());
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for c in clean.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            if depth == 0 {
                sel.push(c)
            } else {
                body.push(c)
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                if depth == 0 {
                    sel.push(c)
                } else {
                    body.push(c)
                }
            }
            '{' => {
                depth += 1;
                if depth > 1 {
                    body.push(c);
                }
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push((std::mem::take(&mut sel), std::mem::take(&mut body)));
                } else {
                    body.push(c);
                }
            }
            c => {
                if depth == 0 {
                    sel.push(c)
                } else {
                    body.push(c)
                }
            }
        }
    }
    // 閉じていない規則・入れ子の規則（@media など）の中身も、どの要素にも当たりうるものとして残す
    if !body.trim().is_empty() || !sel.trim().is_empty() {
        out.push(("*".into(), format!("{sel};{body}")));
    }
    out
}

/// 単純なセレクタ：(要素名, `.` か `#`, 名前)。None は複雑なセレクタ（どの要素にも当たりうるとみなす）。
fn simple_selector(s: &str) -> Option<(Option<String>, Option<char>, String)> {
    let s = s.trim();
    let ident = |t: &str| {
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let (tag, rest) = match s.find(['.', '#']) {
        Some(k) => (&s[..k], &s[k..]),
        None => (s, ""),
    };
    let tag = (!tag.is_empty()).then(|| tag.to_string());
    if tag.as_deref().is_some_and(|t| !ident(t)) {
        return None;
    }
    match rest.chars().next() {
        None => tag.map(|t| (Some(t), None, String::new())),
        Some(k) => {
            let name = &rest[1..];
            ident(name).then(|| (tag, Some(k), name.to_string()))
        }
    }
}

/// CSS の規則の参照の索引。単純なセレクタは要素名・クラス・id で引き、複雑なセレクタの参照は `global` に集める。
#[derive(Default)]
struct Css {
    /// (要素名の条件, 参照の並び) を、引く鍵ごとに。
    by_tag: std::collections::BTreeMap<String, Vec<(Option<String>, usize)>>,
    by_class: std::collections::BTreeMap<String, Vec<(Option<String>, usize)>>,
    by_id: std::collections::BTreeMap<String, Vec<(Option<String>, usize)>>,
    refs: Vec<Vec<(String, String)>>,
    /// 複雑なセレクタの規則の参照（どの要素にも当たりうる）。
    global: Vec<(String, String)>,
    /// セレクタの数（参照のない規則も。usvg が要素ごとに照らす数）。
    selectors: u64,
    /// セレクタごとの (当たる要素の見積もりの鍵, 規則の宣言の数)。参照のない規則も。
    costs: Vec<(SelKind, u64)>,
}

/// セレクタの当たる要素の見積もりの鍵：単純なもの（要素名・クラス・id。その要素の数）か、複雑なもの（すべての要素。
/// 子孫の結合子と `~` の数）。
enum SelKind {
    Tag(String),
    Class(String),
    Id(String),
    Complex { descendants: u32, siblings: u32 },
}

/// 組み合わせの数 C(n, k)（あふれたら u64 の最大）。
fn choose(n: u64, k: u32) -> u64 {
    let k = u64::from(k);
    if k > n {
        return 0;
    }
    let mut r: u128 = 1;
    for i in 0..k {
        r = r * u128::from(n - i) / u128::from(i + 1);
        if r > u128::from(u64::MAX) {
            return u64::MAX;
        }
    }
    r as u64
}

/// 宣言の数（`a: b` の形のもの）。
fn decl_count(decls: &str) -> u64 {
    decls.split(';').filter(|d| d.contains(':')).count() as u64
}

/// セレクタの結合子を数える：(部分の数, 子孫の結合子の数, `~` の数)。括弧・引用符の中は見ない。
fn combinators(sel: &str) -> (usize, u32, u32) {
    let (mut parts, mut descendants, mut siblings) = (1usize, 0u32, 0u32);
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut pending: Option<char> = None;
    let mut started = false;
    for c in sel.trim().chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if depth > 0 => {}
            ' ' | '\t' | '\n' | '\r' | '\x0c' => {
                if started && pending.is_none() {
                    pending = Some(' ');
                }
                continue;
            }
            '>' | '+' | '~' => {
                if started {
                    pending = Some(c);
                }
                continue;
            }
            _ => {}
        }
        if let Some(p) = pending.take() {
            parts += 1;
            match p {
                ' ' => descendants += 1,
                '~' => siblings += 1,
                _ => {}
            }
        }
        started = true;
    }
    (parts, descendants, siblings)
}

impl Css {
    fn add(&mut self, text: &str) -> Result<(), EngineError> {
        for (sel, decls) in css_blocks(text) {
            check_numbers(&decls, false)?;
            self.selectors = self.selectors.saturating_add(sel.split(',').count() as u64);
            let n = decl_count(&decls);
            for one in sel.split(',') {
                let kind = match simple_selector(one) {
                    Some((Some(t), None, _)) => SelKind::Tag(t),
                    Some((_, Some('.'), name)) => SelKind::Class(name),
                    Some((_, Some(_), name)) => SelKind::Id(name),
                    _ => {
                        let (parts, descendants, siblings) = combinators(one);
                        if parts > MAX_SELECTOR_PARTS {
                            return Err(EngineError::Unsupported(format!(
                                "結合子が {MAX_SELECTOR_PARTS} 段より多い CSS のセレクタを含む SVG は読めません"
                            )));
                        }
                        SelKind::Complex {
                            descendants,
                            siblings,
                        }
                    }
                };
                self.costs.push((kind, n));
            }
            let refs: Vec<(String, String)> = decl_refs(&decls)
                .into_iter()
                .map(|(p, i)| (p.to_string(), i.to_string()))
                .collect();
            if refs.is_empty() {
                continue;
            }
            if self.refs.len() >= MAX_CSS_RULES {
                return Err(EngineError::Unsupported(
                    "参照を含む CSS の規則が多すぎる SVG は読めません".into(),
                ));
            }
            let k = self.refs.len();
            self.refs.push(refs.clone());
            for one in sel.split(',') {
                match simple_selector(one) {
                    Some((tag, None, _)) => {
                        if let Some(t) = tag {
                            self.by_tag.entry(t).or_default().push((None, k));
                        }
                    }
                    Some((tag, Some('.'), name)) => {
                        self.by_class.entry(name).or_default().push((tag, k))
                    }
                    Some((tag, Some(_), name)) => {
                        self.by_id.entry(name).or_default().push((tag, k))
                    }
                    None => self.global.extend(refs.iter().cloned()),
                }
            }
        }
        Ok(())
    }

    /// 要素に当たる単純なセレクタの規則（`refs` の番号）。
    fn rules_for(&self, n: &SvgNode<'_, '_>) -> Vec<usize> {
        let tag = n.tag_name().name();
        let mut hits: Vec<usize> = Vec::new();
        let mut take = |list: Option<&Vec<(Option<String>, usize)>>| {
            for (t, k) in list.into_iter().flatten() {
                if t.as_deref().is_none_or(|t| t == tag) {
                    hits.push(*k);
                }
            }
        };
        take(self.by_tag.get(tag));
        if let Some(classes) = n.attribute("class") {
            for c in classes.split_whitespace() {
                take(self.by_class.get(c));
            }
        }
        for a in n.attributes().filter(|a| a.name() == "id") {
            take(self.by_id.get(a.value()));
        }
        hits.sort();
        hits.dedup();
        hits
    }
}

/// 要素の数（`<use>` の参照先と、marker の頂点ごとの複製を足したもの）・参照を展開した入れ子の高さ・参照の輪・文字の
/// レイアウトの手間（`svg_text`）を見積もる。文字の要素があるかも返す。木を再帰でたどらない（深い文書で自分のスタックを
/// 使い切らないように）。
///
/// 辺は、子要素・`<use>` の参照先（数にも入れる）・ほかの href・`url(#…)` で参照する中身を持つ要素
/// （pattern・marker・clipPath・mask・filter）。中身を持つ要素は、祖先の受け継がれる性質（fill・stroke・marker）の参照も辺にする
/// （中身がその参照を受け継いで、自分を描き直し続けるため。usvg はこの輪を見つけられずにスタックを使い切る）。
/// CSS の規則の参照は、当たる要素の属性と同じに扱う。
fn check_elements(text: &str) -> Result<Facts, EngineError> {
    let opt = usvg::roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: MAX_ELEMENTS as u32,
        ..Default::default()
    };
    let doc = usvg::roxmltree::Document::parse_with_options(text, opt).map_err(|e| match e {
        usvg::roxmltree::Error::NodesLimitReached => EngineError::Limit("要素が多すぎます".into()),
        other => err(other),
    })?;
    analyze(&doc)
}

type SvgNode<'a, 'i> = usvg::roxmltree::Node<'a, 'i>;

/// usvg に渡す前に XML の木から分かったこと。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Facts {
    /// 文字の要素（text・tspan・textPath）があるか。
    has_text: bool,
    /// 文字のレイアウトの手間の見積もり（`<use>` の展開・参照ごとの複製・marker の複製を掛けたもの。フォントを選ぶ分は除く）。
    text_work: u64,
    /// いちばん重い文字の塊を 1 度形にし直す手間（代わりのフォントが見つかったとき）。
    reshape: u64,
}

/// href の値の参照先の id（前後の空白を除き、`#` のあと最初の空白まで。usvg が使う svgtypes の IRI と同じ読み方）。
fn href_id(v: &str) -> Option<&str> {
    let id = v.trim_start().strip_prefix('#')?;
    let id = id.split(char::is_whitespace).next().unwrap_or(id);
    (!id.is_empty()).then_some(id)
}

/// 文書の要素の索引と CSS の規則。参照の解き方は usvg より広く取る（見積もりの辺が、usvg が実際にたどる辺を含むように）：
/// id は名前空間を問わず（`xml:id` も）、同じ id の要素はすべて、href も名前空間を問わずすべてを辺にする。
struct Index<'a, 'i> {
    by_id: std::collections::BTreeMap<&'a str, Vec<SvgNode<'a, 'i>>>,
    css: Css,
    /// 要素ごとの、参照する中身を持つ要素（pattern・marker など）と、その参照が受け継がれる性質か。
    /// 要素の番号（roxmltree の NodeId）で引く。先に 1 度だけ解いておく（祖先の参照を見直すたびに解かない）。
    refs: Vec<Vec<(bool, SvgNode<'a, 'i>)>>,
    /// 要素ごとの、受け継ぐ参照（自分と祖先の fill・stroke・marker）の表の番号（`lists` の位置）。
    inh: Vec<usize>,
    /// 受け継ぐ参照の表。0 は空。自分の参照を持たない要素は親の表を指す。
    lists: Vec<Vec<SvgNode<'a, 'i>>>,
    /// これまでの見積もりの手間（`MAX_ANALYZE_WORK` まで）。
    work: std::cell::Cell<u64>,
}

impl<'a, 'i> Index<'a, 'i> {
    /// 手間を足す。上限を超えたら `Limit`。
    fn spend(&self, n: u64) -> Result<(), EngineError> {
        self.work.set(self.work.get().saturating_add(n));
        if self.work.get() > MAX_ANALYZE_WORK {
            return Err(EngineError::Limit("参照が多すぎる SVG は読めません".into()));
        }
        Ok(())
    }

    /// 要素が直接持つ参照（属性と、当たる CSS の規則）のうち、中身を持つ要素への参照を解く。
    /// 参照先（同じ id の要素すべて）は、積む前に手間に数える（参照の数 × 同じ id の要素の数で膨らむ形を、確保の前に断る）。
    fn resolve(&self, n: &SvgNode<'a, 'i>) -> Result<Vec<(bool, SvgNode<'a, 'i>)>, EngineError> {
        let mut out: Vec<(bool, SvgNode<'a, 'i>)> = Vec::new();
        let budget = MAX_ANALYZE_WORK.saturating_sub(self.work.get());
        let mut seen = 1u64;
        let mut over = false;
        // 同じ参照（受け継ぐか × id）は 1 度だけ解く
        let mut done: std::collections::BTreeSet<(bool, String)> =
            std::collections::BTreeSet::new();
        let mut add = |prop: &str, id: &str| {
            seen = seen.saturating_add(1);
            let inh = inheritable(prop);
            if over || !done.insert((inh, id.to_string())) {
                return;
            }
            let targets = self.by_id.get(id).map_or(0, Vec::len) as u64;
            seen = seen.saturating_add(targets);
            if seen > budget {
                over = true;
                return;
            }
            out.extend(
                self.targets(id)
                    .filter(|t| Self::referable(t))
                    .map(|t| (inh, t)),
            );
        };
        for a in n.attributes() {
            if a.name() == "style" {
                let clean = strip_css_comments(a.value());
                for (prop, id) in decl_refs(&clean) {
                    add(prop, id);
                }
            } else {
                for id in url_ids(a.value()) {
                    add(a.name(), id);
                }
            }
        }
        for k in self.css.rules_for(n) {
            for (prop, id) in self.css.refs.get(k).into_iter().flatten() {
                add(prop, id);
            }
        }
        if over {
            return Err(EngineError::Limit("参照が多すぎる SVG は読めません".into()));
        }
        out.sort_by_key(|(inh, t)| (t.id().get(), *inh));
        out.dedup_by_key(|(inh, t)| (t.id().get(), *inh));
        self.spend(seen + out.len() as u64)?;
        Ok(out)
    }

    fn own(&self, n: &SvgNode<'a, 'i>) -> &[(bool, SvgNode<'a, 'i>)] {
        self.refs
            .get(n.id().get() as usize)
            .map_or(&[], Vec::as_slice)
    }

    fn targets(&self, id: &str) -> impl Iterator<Item = SvgNode<'a, 'i>> + '_ {
        self.by_id.get(id).into_iter().flatten().copied()
    }

    fn referable(n: &SvgNode<'_, '_>) -> bool {
        matches!(
            n.tag_name().name(),
            "pattern" | "marker" | "clipPath" | "mask" | "filter"
        )
    }

    fn container(n: &SvgNode<'_, '_>) -> bool {
        Self::referable(n) || n.tag_name().name() == "symbol"
    }

    /// 塗り・線（受け継ぐ fill・stroke・marker）を自分で使う要素。use は、複製した中身が受け継ぐ。
    fn paints(n: &SvgNode<'_, '_>) -> bool {
        matches!(
            n.tag_name().name(),
            "path"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "text"
                | "tspan"
                | "textPath"
                | "use"
        )
    }

    /// usvg がこの要素を、参照する要素ごとに中身ごと複製するか（単位が objectBoundingBox のとき。pattern は href の先の
    /// 属性も受け継ぐ）。決めかねるもの（href の先が 1 つに決まらないなど）は複製するとみなす。
    fn cloned_per_use(&self, t: &SvgNode<'a, 'i>) -> bool {
        let attr = |n: &SvgNode<'a, 'i>, name: &str| -> Option<&'a str> {
            n.attributes()
                .find(|a| a.name() == name)
                .map(|a| a.value().trim())
        };
        match t.tag_name().name() {
            "pattern" => {
                let (mut units, mut content) = (None, None);
                let mut cur = Some(*t);
                for _ in 0..MAX_DEPTH {
                    let Some(n) = cur else {
                        break;
                    };
                    units = units.or(attr(&n, "patternUnits"));
                    content = content.or(attr(&n, "patternContentUnits"));
                    let mut next: Vec<SvgNode<'a, 'i>> = Vec::new();
                    for a in n.attributes().filter(|a| a.name() == "href") {
                        if let Some(id) = href_id(a.value()) {
                            next.extend(self.targets(id));
                        }
                    }
                    cur = match next.as_slice() {
                        [] => None,
                        [one] => Some(*one),
                        _ => return true,
                    };
                }
                !(units == Some("userSpaceOnUse") && content != Some("objectBoundingBox"))
            }
            "clipPath" => attr(t, "clipPathUnits") == Some("objectBoundingBox"),
            "mask" => {
                attr(t, "maskUnits") != Some("userSpaceOnUse")
                    || attr(t, "maskContentUnits") == Some("objectBoundingBox")
            }
            "filter" => true,
            _ => false,
        }
    }

    /// 参照の辺の種類：usvg が複製するなら `EDGE_CLONE`、共有するなら `EDGE_LINK`。marker は頂点ごとの複製として別に数える。
    fn ref_kind(&self, t: &SvgNode<'a, 'i>) -> u8 {
        if t.tag_name().name() != "marker" && self.cloned_per_use(t) {
            EDGE_CLONE
        } else {
            EDGE_LINK
        }
    }

    /// 辺：(先, 種類)。見積もりの手間が上限を超えたら `Limit`。
    ///
    /// - 子要素（数に入れる）。`<use>`・`<feImage>` の参照先（自分の所に作るので数に入れる）、ほかの href（数に入れない）。
    /// - 自分に当たる clip-path・mask・filter（受け継がない参照）。
    /// - 受け継ぐ参照（自分と祖先の fill・stroke・marker）：塗る要素は使う。中身を持つ要素は、中身が受け継いで自分を描き直す輪を
    ///   見つけるために辺にする（数に入れない。usvg はこの輪を見つけられずにスタックを使い切る）。
    fn successors(&self, n: SvgNode<'a, 'i>) -> Result<Vec<(SvgNode<'a, 'i>, u8)>, EngineError> {
        let mut out: Vec<(SvgNode<'a, 'i>, u8)> = n
            .children()
            .filter(|c| c.is_element())
            .map(|c| (c, EDGE_COUNT))
            .collect();
        let instantiates = matches!(n.tag_name().name(), "use" | "feImage");
        for a in n.attributes().filter(|a| a.name() == "href") {
            if let Some(id) = href_id(a.value()) {
                let kind = if instantiates { EDGE_COUNT } else { EDGE_LINK };
                out.extend(self.targets(id).map(|t| (t, kind)));
            }
        }
        for (inh, t) in self.own(&n) {
            if !*inh {
                out.push((*t, self.ref_kind(t)));
            }
        }
        let list = self
            .inh
            .get(n.id().get() as usize)
            .and_then(|k| self.lists.get(*k))
            .map_or(&[][..], Vec::as_slice);
        if Self::paints(&n) {
            out.extend(list.iter().map(|t| (*t, self.ref_kind(t))));
        } else if Self::container(&n) {
            out.extend(list.iter().map(|t| (*t, EDGE_LINK)));
        }
        self.spend(out.len() as u64)?;
        Ok(out)
    }
}

fn analyze(doc: &usvg::roxmltree::Document<'_>) -> Result<Facts, EngineError> {
    let mut index = Index {
        by_id: std::collections::BTreeMap::new(),
        css: Css::default(),
        refs: Vec::new(),
        inh: Vec::new(),
        lists: Vec::new(),
        work: std::cell::Cell::new(0),
    };
    let mut has_text = false;
    let mut css_markers = false;
    // `<style>` に transform が書かれているか（どの要素の拡大にもなりうる。textPath の見積もり）
    let mut css_transform = false;
    let mut count = 0usize;
    let mut elements = 0u64;
    // CSS の手間の見積もりに使う：要素名・クラスごとの数、文書の深さ・兄弟の最大数、style 属性の宣言の手間
    let mut tags: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    let mut classes: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    let (mut max_depth, mut max_siblings, mut style_cost) = (1u64, 1u64, 0u64);
    for n in doc.descendants() {
        count = count.max(n.id().get() as usize + 1);
        if !n.is_element() {
            continue;
        }
        elements += 1;
        *tags.entry(n.tag_name().name()).or_default() += 1;
        if let Some(c) = n.attribute("class") {
            for one in c.split_whitespace() {
                *classes.entry(one).or_default() += 1;
            }
        }
        max_depth = max_depth.max(n.ancestors().filter(|a| a.is_element()).count() as u64);
        max_siblings = max_siblings.max(n.children().filter(|c| c.is_element()).count() as u64);
        if let Some(style) = n.attribute("style") {
            let d = decl_count(style) + 1;
            style_cost = style_cost.saturating_add(d.saturating_mul(d));
        }
        for a in n.attributes().filter(|a| a.name() == "id") {
            index.by_id.entry(a.value()).or_default().push(n);
        }
        // 数の大きさ（SVG の要素の、名前空間のないか SVG の属性。名前・参照の属性は読まない）
        if n.tag_name().namespace() == Some(SVG_NS) {
            for a in n.attributes() {
                if !matches!(a.namespace(), None | Some(SVG_NS)) {
                    continue;
                }
                match a.name() {
                    "id" | "class" | "href" | "lang" | "font-family" => {}
                    "style" => check_numbers(&strip_css_comments(a.value()), false)?,
                    name => check_numbers(a.value(), name == "d")?,
                }
            }
        }
        match n.tag_name().name() {
            "text" | "tspan" | "textPath" => has_text = true,
            "style" => {
                if let Some(t) = n.text() {
                    if t.contains("marker") {
                        css_markers = true;
                    }
                    if t.contains("transform") {
                        css_transform = true;
                    }
                    index.css.add(t)?;
                }
            }
            _ => {}
        }
    }
    if index.css.selectors.saturating_mul(elements) > MAX_CSS_MATCHES {
        return Err(EngineError::Limit(
            "CSS の規則と要素が多すぎる SVG は読めません".into(),
        ));
    }
    // 当たる要素ごとに宣言の数の 2 乗（規則を読む分の 1 回も足す）、複雑なセレクタはすべての要素で後戻りの照合
    let mut css_cost = style_cost;
    for (kind, decls) in &index.css.costs {
        let matched = match kind {
            SelKind::Tag(t) => tags.get(t.as_str()).copied().unwrap_or(0),
            SelKind::Class(c) => classes.get(c.as_str()).copied().unwrap_or(0),
            SelKind::Id(i) => index.by_id.get(i.as_str()).map_or(0, |v| v.len() as u64),
            SelKind::Complex {
                descendants,
                siblings,
            } => {
                // simplecss は結合子ごとに祖先（兄弟）を 1 つずつ試して残りを照らすので、試す並びは順に選ぶ組み合わせの数
                let backtrack =
                    choose(max_depth, *descendants).saturating_mul(choose(max_siblings, *siblings));
                css_cost = css_cost.saturating_add(elements.saturating_mul(backtrack));
                elements
            }
        };
        css_cost = css_cost.saturating_add(
            matched
                .saturating_add(1)
                .saturating_mul(decls.saturating_mul(*decls)),
        );
    }
    if css_cost > MAX_CSS_MATCHES {
        return Err(EngineError::Limit(
            "CSS を当てる手間が大きすぎる SVG は読めません".into(),
        ));
    }
    let mut refs = vec![Vec::new(); count];
    for n in doc.descendants().filter(|n| n.is_element()) {
        if let Some(slot) = refs.get_mut(n.id().get() as usize) {
            *slot = index.resolve(&n)?;
        }
    }
    index.refs = refs;
    // 受け継ぐ参照の表：自分の参照と親の表の和。自分の参照を持たない要素は親の表を指す（文書の順なので親が先）
    let mut lists: Vec<Vec<SvgNode<'_, '_>>> = vec![Vec::new()];
    let mut inh = vec![0usize; count];
    for n in doc.descendants().filter(|n| n.is_element()) {
        let parent = n
            .parent_element()
            .and_then(|p| inh.get(p.id().get() as usize).copied())
            .unwrap_or(0);
        let own: Vec<SvgNode<'_, '_>> = index
            .own(&n)
            .iter()
            .filter(|(i, _)| *i)
            .map(|(_, t)| *t)
            .collect();
        let k = if own.is_empty() {
            parent
        } else {
            let mut list = lists.get(parent).cloned().unwrap_or_default();
            list.extend(own);
            list.sort_by_key(|t| t.id().get());
            list.dedup_by_key(|t| t.id().get());
            index.spend(list.len() as u64)?;
            lists.push(list);
            lists.len() - 1
        };
        if let Some(slot) = inh.get_mut(n.id().get() as usize) {
            *slot = k;
        }
    }
    index.inh = inh;
    index.lists = lists;
    let shapes = |n: &SvgNode<'_, '_>| {
        matches!(
            n.tag_name().name(),
            "path" | "polyline" | "polygon" | "line"
        )
    };
    // marker の中で marker を使う SVG は断る（複製が段の数の指数で増える）
    for m in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "marker")
    {
        let nested = m.descendants().skip(1).any(|d| {
            d.is_element() && (!marker_refs(&d).is_empty() || (css_markers && shapes(&d)))
        });
        if nested {
            return Err(EngineError::Unsupported(
                "入れ子の marker を含む SVG は読めません".into(),
            ));
        }
    }
    // 複雑なセレクタの規則はどの要素にも当たりうるので、その参照先が中身を持てば、中身の要素からも自分への参照になる（輪）。
    // 中身を href の先から借りるもの（中身のない pattern が別の pattern を指す）も同じ
    for (_, id) in &index.css.global {
        if index.targets(id).any(|t| {
            Index::referable(&t)
                && (t.children().any(|c| c.is_element())
                    || t.attributes().any(|a| a.name() == "href"))
        }) {
            return Err(EngineError::Unsupported(
                "複雑なセレクタの CSS で中身のある pattern・marker などを参照する SVG は読めません"
                    .into(),
            ));
        }
    }
    // marker の中の textPath は断る（頂点ごとに複製され、拡大は線の太さで決まる）
    let marker_text_path = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "marker")
        .any(|m| {
            m.descendants()
                .any(|d| d.is_element() && d.tag_name().name() == "textPath")
        });
    if marker_text_path {
        return Err(EngineError::Unsupported(
            "marker の中に textPath を含む SVG は読めません".into(),
        ));
    }
    // `<text>` ごとの文字のレイアウトの手間（svg_text）。textPath の参照先は、同じ id の図形の区間の数のいちばん多いもの
    // （id ごとに 1 度だけ数える。多くの textPath が長い path を参照しても、path を数え直さない）
    let seg_memo: std::cell::RefCell<std::collections::BTreeMap<String, u64>> =
        std::cell::RefCell::new(std::collections::BTreeMap::new());
    let segments = |tp: &SvgNode<'_, '_>| -> u64 {
        tp.attributes()
            .filter(|a| a.name() == "href")
            .filter_map(|a| href_id(a.value()))
            .map(|id| {
                if let Some(n) = seg_memo.borrow().get(id) {
                    return *n;
                }
                let n = index
                    .targets(id)
                    .map(|t| svg_text::path_segments(&t))
                    .max()
                    .unwrap_or(0);
                seg_memo.borrow_mut().insert(id.to_string(), n);
                n
            })
            .max()
            .unwrap_or(0)
    };
    let mut own_text = vec![0u64; count];
    let mut reshape = 0u64;
    for t in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "text")
    {
        let c = svg_text::text_cost(&t, &segments);
        if let Some(slot) = own_text.get_mut(t.id().get() as usize) {
            *slot = c.work;
        }
        reshape = reshape.max(c.reshape);
    }
    // 帰りがけの順に、要素の数（use の展開と参照ごとの複製を含む）・複製で作る要素の数・高さ（参照の展開を含む）・文字の
    // レイアウトの手間（同じ）・textPath に至る変形の拡大の上限（参照の先のいちばん大きいもの × 自分の拡大）を求める。輪は断る
    let mut text_work = vec![0u64; count];
    let mut path_scale = vec![0f64; count];
    let mut state = vec![0u8; count];
    let mut size = vec![0u64; count];
    let mut clones = vec![0u64; count];
    let mut height = vec![0usize; count];
    let at = |n: &SvgNode<'_, '_>| n.id().get() as usize;
    let root = doc.root_element();
    state[at(&root)] = 1;
    let mut stack = vec![(root, index.successors(root)?, 0usize)];
    while let Some((n, succ, pos)) = stack.last_mut() {
        if let Some((s, _)) = succ.get(*pos).copied() {
            *pos += 1;
            match state[at(&s)] {
                0 => {
                    state[at(&s)] = 1;
                    let next = index.successors(s)?;
                    stack.push((s, next, 0));
                }
                1 => {
                    return Err(EngineError::Unsupported(
                        "自分を参照する要素（use・pattern・marker など）を含む SVG は読めません"
                            .into(),
                    ));
                }
                _ => {}
            }
            continue;
        }
        let i = at(n);
        let (mut total, mut h, mut cloned) = (1u64, 0usize, 0u64);
        let mut tw = own_text[i];
        let mut ps = if n.tag_name().name() == "textPath" {
            1.0
        } else {
            0.0
        };
        for (s, kind) in succ.iter() {
            if *kind >= EDGE_COUNT {
                total = total.saturating_add(size[at(s)]);
                cloned = cloned.saturating_add(clones[at(s)]);
                tw = tw.saturating_add(text_work[at(s)]);
            }
            if *kind == EDGE_CLONE {
                cloned = cloned.saturating_add(size[at(s)]);
            }
            h = h.max(height[at(s)]);
            // 共有の参照（EDGE_LINK）の先も、参照する要素の拡大を受けるとみなす（多めに取る）
            ps = crate::math::max(ps, path_scale[at(s)]);
        }
        size[i] = total;
        clones[i] = cloned;
        height[i] = h + 1;
        text_work[i] = tw;
        path_scale[i] = if ps > 0.0 {
            ps * svg_text::own_scale(n, *n == root, css_transform)
        } else {
            0.0
        };
        state[i] = 2;
        if tw > svg_text::MAX_TEXT_WORK {
            return Err(text_limit());
        }
        if total > MAX_ELEMENTS {
            return Err(EngineError::Limit(
                "use・参照を展開すると要素が多すぎます".into(),
            ));
        }
        if cloned > MAX_CLONES {
            return Err(EngineError::Limit(
                "pattern・mask などを参照ごとに複製すると要素が多すぎます".into(),
            ));
        }
        if height[i] > MAX_DEPTH {
            return Err(EngineError::Unsupported(format!(
                "参照を展開すると入れ子が {MAX_DEPTH} 段より深くなる SVG は読めません"
            )));
        }
        stack.pop();
    }
    let mut total = size[at(&root)];
    let mut total_text = text_work[at(&root)];
    if path_scale[at(&root)] > svg_text::MAX_TEXT_PATH_SCALE {
        return Err(EngineError::Limit(
            "textPath を大きく拡大する SVG は読めません".into(),
        ));
    }
    // marker の複製：頂点の数 × marker の中身の大きさ（CSS で付くものは、いちばん大きい marker とみなす）。文字の手間も同じ
    let markers: Vec<SvgNode<'_, '_>> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "marker")
        .collect();
    let largest = markers.iter().map(|m| size[at(m)]).max().unwrap_or(0);
    let largest_text = markers.iter().map(|m| text_work[at(m)]).max().unwrap_or(0);
    for n in doc.descendants().filter(|n| n.is_element() && shapes(n)) {
        let (per_vertex, per_vertex_text): (u64, u64) = if css_markers {
            (largest, largest_text)
        } else {
            marker_refs(&n)
                .iter()
                .flat_map(|id| index.targets(id))
                .map(|m| (size[at(&m)], text_work[at(&m)]))
                .fold((0u64, 0u64), |(a, b), (x, y)| {
                    (a.saturating_add(x), b.saturating_add(y))
                })
        };
        let v = vertices(&n);
        total = total.saturating_add(v.saturating_mul(per_vertex));
        total_text = total_text.saturating_add(v.saturating_mul(per_vertex_text));
        if total > MAX_ELEMENTS {
            return Err(EngineError::Limit(
                "marker を展開すると要素が多すぎます".into(),
            ));
        }
        if total_text > svg_text::MAX_TEXT_WORK {
            return Err(text_limit());
        }
    }
    Ok(Facts {
        has_text,
        text_work: total_text,
        reshape,
    })
}

/// 文字のレイアウトの手間が上限を超えるときのエラー。
fn text_limit() -> EngineError {
    EngineError::Limit("文字が多すぎる（並べる手間が大きすぎる）SVG は読めません".into())
}

impl SvgCodec {
    fn tree(&self, text: &str) -> Result<usvg::Tree, EngineError> {
        self.tree_within(text, svg_text::MAX_TEXT_WORK)
    }

    /// 文字のレイアウトの手間を `max_text_work` までにして木を作る（テストは小さな上限で、フォントを選ぶ分の見張りを確かめる）。
    fn tree_within(&self, text: &str, max_text_work: u64) -> Result<usvg::Tree, EngineError> {
        check_entities(text)?;
        check_depth(text)?;
        let facts = check_elements(text)?;
        if facts.text_work > max_text_work {
            return Err(text_limit());
        }
        let fontdb = self.fontdb(facts.has_text)?;
        // フォントを選ぶ・代わりを探す手間は書体の数で決まる。残りの手間から引き、尽きたら選ばずに断る（svg_text）
        let guard = Arc::new(svg_text::TextGuard::new(
            max_text_work.saturating_sub(facts.text_work),
        ));
        let font_resolver = if facts.has_text {
            svg_text::font_resolver(guard.clone(), fontdb.len() as u64, facts.reshape)
        } else {
            usvg::FontResolver::default()
        };
        // <image> の参照（data でも外部でも）は、この段では描かない。見つけたら印を立てて、読み取りを断る
        let images = Arc::new(AtomicBool::new(false));
        let (a, b) = (images.clone(), images.clone());
        let opt = usvg::Options {
            resources_dir: None,
            image_href_resolver: usvg::ImageHrefResolver {
                resolve_data: Box::new(move |_, _, _| {
                    a.store(true, Ordering::Relaxed);
                    None
                }),
                resolve_string: Box::new(move |_, _| {
                    b.store(true, Ordering::Relaxed);
                    None
                }),
            },
            fontdb,
            font_resolver,
            ..usvg::Options::default()
        };
        let tree = usvg::Tree::from_str(text, &opt).map_err(err)?;
        if guard.over() {
            return Err(text_limit());
        }
        if images.load(Ordering::Relaxed) {
            return Err(EngineError::Unsupported(
                "画像（埋め込みの SVG・ラスター画像・外部のファイル）を含む SVG は、この段では読めません".into(),
            ));
        }
        Ok(tree)
    }
}

/// 描く手間（画素の数で数える）の下限の上限。キャンバスが大きければ、その 64 倍まで許す。
const MAX_RENDER_WORK: u64 = 1 << 32;
/// 描く前にたどる要素の数の上限（pattern の中身は参照ごとにたどり直す）。
const MAX_RENDER_NODES: u64 = 1 << 22;
/// 変形のあとの座標（キャンバスの画素）の上限。tiny-skia は巨大な曲線を描くのに時間がかかる（半径 10^37 の楕円で 1 秒を超える。
/// ファズで見つけた）。ふつうの SVG の座標は、はみ出すものでもキャンバスの数倍ほど。
const MAX_EXTENT: f32 = 1e8;

/// 描く前にたどる木の項目（グループか図形）。変形・根からの道筋で同時に生きる一時の画像のバイト数・描く面の大きさ（画素）と組にして積む。
enum Visit<'t> {
    Group(&'t usvg::Group),
    Node(&'t usvg::Node),
}

/// 長さ（画素）を整数にする。無限・負は `max`。
fn span(len: f32, max: u64) -> u64 {
    let len = f64::from(len);
    if len.is_finite() && len >= 0.0 {
        crate::math::min(len.ceil() + 4.0, max as f64) as u64
    } else {
        max
    }
}

/// 半径・標準偏差（利用者の単位）を画素にする。無限・巨大なものは大きな値（予算を超えさせる）。
fn radius_px(v: f32, scale: f32) -> u64 {
    let x = (f64::from(v) * f64::from(scale.abs())).ceil();
    if x.is_finite() && x >= 0.0 {
        crate::math::min(x, 1e15) as u64
    } else {
        1 << 50
    }
}

/// フィルタのプリミティブ 1 つの手間（層の大きさ `lw × lh` 画素、変形の倍率 `sx・sy`）。resvg の実装の回る数に合わせる：
/// feMorphology は画素ごとに半径の矩形を、feTurbulence はオクターブの数だけ、feConvolveMatrix は行列の大きさだけ回り、
/// ぼかしは行・列ごとに半径の分を足す。
fn primitive_work(p: &usvg::filter::Primitive, lw: u64, lh: u64, sx: f32, sy: f32) -> u64 {
    use usvg::filter::Kind;
    let r = lw.saturating_mul(lh);
    let blur = |x: f32, y: f32| {
        lw.saturating_add(lh)
            .saturating_mul(3)
            .saturating_mul(radius_px(x, sx).saturating_add(radius_px(y, sy)))
    };
    match p.kind() {
        Kind::Morphology(m) => r
            .saturating_mul(
                radius_px(m.radius_x().get(), sx)
                    .saturating_mul(2)
                    .saturating_add(1)
                    .min(lw),
            )
            .saturating_mul(
                radius_px(m.radius_y().get(), sy)
                    .saturating_mul(2)
                    .saturating_add(1)
                    .min(lh),
            ),
        Kind::Turbulence(t) => r
            .saturating_mul(u64::from(t.num_octaves()).max(1))
            .saturating_mul(4),
        Kind::ConvolveMatrix(c) => r.saturating_mul(
            u64::from(c.matrix().columns()).saturating_mul(u64::from(c.matrix().rows())),
        ),
        Kind::GaussianBlur(b) => r
            .saturating_mul(8)
            .saturating_add(blur(b.std_dev_x().get(), b.std_dev_y().get())),
        Kind::DropShadow(d) => r
            .saturating_mul(12)
            .saturating_add(blur(d.std_dev_x().get(), d.std_dev_y().get())),
        Kind::DiffuseLighting(_) | Kind::SpecularLighting(_) => r.saturating_mul(16),
        _ => r.saturating_mul(4),
    }
}

/// 描く前の資源の見積もり。resvg の描き方（`render_group`・`fill_path`・`render_pattern_pixmap`・`filter::apply`・mask・clip）に
/// 合わせて木をたどり、次を断る（`Unsupported`。ほかの段のデコーダなら読めるかもしれない）：
/// (1) 根からの道筋ごとに同時に生きる一時の画像（層・mask と clip の画像・フィルタの結果・pattern のタイル）の合計が、絶対の予算
/// （`limits.max_value_bytes`）を超えるもの。resvg の層は 1 枚がキャンバスの 25 倍（(-2w,-2h,5w,5h)）まで広がり、入れ子の層は
/// 同時に生きる。フィルタはプリミティブの結果をすべて最後まで持ち、pattern のタイルには上限がない。
/// (2) 描く手間（画素の数：図形の外接矩形と描く面の小さい方・層の合成・フィルタのプリミティブ）の合計が上限を超えるもの。
/// resvg の描画は途中で止められないので、手間で時間を抑える。
/// (3) 変形のあとの座標が `MAX_EXTENT` を超える図形。
/// 木（mask・clipPath・フィルタの feImage・文字の輪郭・pattern の中身も。pattern は参照ごとに）は再帰せずにたどる。
/// 変形の倍率は平行移動によらないので、層のずらしは見ない。
fn check_render(tree: &usvg::Tree, w: u32, h: u32, limits: &Limits) -> Result<(), EngineError> {
    let (cw, ch) = (u64::from(w), u64::from(h));
    let max_bytes = limits.max_value_bytes;
    let max_work = cw
        .saturating_mul(ch)
        .saturating_mul(64)
        .max(MAX_RENDER_WORK);
    let too_much =
        |what: &str| EngineError::Unsupported(format!("{what}が大きすぎる SVG は読めません"));
    let mut work = 0u64;
    let mut visited = 0u64;
    let mut stack: Vec<(Visit<'_>, tiny_skia::Transform, u64, (u64, u64))> = vec![(
        Visit::Group(tree.root()),
        tiny_skia::Transform::default(),
        0,
        (cw, ch),
    )];
    while let Some((visit, parent, live, surf)) = stack.pop() {
        visited += 1;
        if visited > MAX_RENDER_NODES {
            return Err(EngineError::Limit("描く要素が多すぎます".into()));
        }
        match visit {
            Visit::Group(g) => {
                let ts = parent.pre_concat(g.transform());
                let (mut live, mut surf) = (live, surf);
                if g.should_isolate() {
                    // 層：layer_bounding_box を変形し、(-2w,-2h,5w,5h) に切った大きさ（resvg と同じ。作れなければ描かない）
                    let Some(b) = g.layer_bounding_box().transform(ts) else {
                        continue;
                    };
                    let (lw, lh) = (span(b.width(), cw * 5), span(b.height(), ch * 5));
                    let layer = lw.saturating_mul(lh);
                    // mask・clip（入れ子も）は、それぞれ層と同じ大きさの画像とマスク（5 バイト／画素）を持つ
                    let mut inner: Vec<(&usvg::Group, tiny_skia::Transform)> = Vec::new();
                    let mut mask = g.mask();
                    while let Some(m) = mask {
                        inner.push((m.root(), ts));
                        mask = m.mask();
                    }
                    let mut clip = g.clip_path();
                    while let Some(c) = clip {
                        inner.push((c.root(), ts.pre_concat(c.transform())));
                        clip = c.clip_path();
                    }
                    let masks = inner.len() as u64;
                    // フィルタは順に当て、1 つのフィルタの中ではプリミティブの結果（と元の画像・アルファ）をすべて持つ
                    let (sx, sy) = ts.get_scale();
                    let mut filter_bytes = 0u64;
                    for f in g.filters() {
                        let n = f.primitives().len() as u64 + 2;
                        filter_bytes = filter_bytes.max(n.saturating_mul(layer).saturating_mul(4));
                        for prim in f.primitives() {
                            work = work.saturating_add(primitive_work(prim, lw, lh, sx, sy));
                            if let usvg::filter::Kind::Image(img) = prim.kind() {
                                inner.push((img.root(), tiny_skia::Transform::from_scale(sx, sy)));
                            }
                        }
                    }
                    live = live
                        .saturating_add(layer.saturating_mul(4))
                        .saturating_add(masks.saturating_mul(layer).saturating_mul(5))
                        .saturating_add(filter_bytes);
                    if live > max_bytes {
                        return Err(too_much("描くための一時の画像（層・mask・フィルタ）"));
                    }
                    work = work.saturating_add(layer.saturating_mul(masks + 1));
                    surf = (lw, lh);
                    // mask・clip・feImage の中身は、層と同じ道筋（同じ一時の画像を持ったまま）で描く
                    for (root, t) in inner {
                        stack.push((Visit::Group(root), t, live, surf));
                    }
                }
                for n in g.children() {
                    stack.push((Visit::Node(n), ts, live, surf));
                }
            }
            Visit::Node(node) => match node {
                usvg::Node::Group(g) => stack.push((Visit::Group(g.as_ref()), parent, live, surf)),
                usvg::Node::Text(text) => {
                    stack.push((Visit::Group(text.flattened()), parent, live, surf))
                }
                usvg::Node::Image(_) => {}
                usvg::Node::Path(path) => {
                    let ts = parent;
                    let sw = path.stroke().map_or(0.0, |s| s.width().get());
                    let Some(b) = path
                        .data()
                        .bounds()
                        .outset(sw / 2.0 + 1.0, sw / 2.0 + 1.0)
                        .and_then(|r| r.transform(ts))
                    else {
                        return Err(too_much("図形の座標"));
                    };
                    if !(b.left() >= -MAX_EXTENT
                        && b.top() >= -MAX_EXTENT
                        && b.right() <= MAX_EXTENT
                        && b.bottom() <= MAX_EXTENT)
                    {
                        return Err(too_much("図形の座標"));
                    }
                    let area = span(b.width(), surf.0).saturating_mul(span(b.height(), surf.1));
                    work = work
                        .saturating_add(area)
                        .saturating_add(path.data().points().len() as u64);
                    let paints = [
                        path.fill().map(|f| f.paint()),
                        path.stroke().map(|s| s.paint()),
                    ];
                    for paint in paints.into_iter().flatten() {
                        if let usvg::Paint::Pattern(pat) = paint {
                            // タイル：pattern の矩形 × 変形の倍率（resvg の render_pattern_pixmap と同じ）。外側のタイルを持ったまま中身を描く
                            let (sx, sy) = ts.pre_concat(pat.transform()).get_scale();
                            let r = pat.rect();
                            let (tw, th) = (
                                f64::from((r.width() * sx).round()),
                                f64::from((r.height() * sy).round()),
                            );
                            if !(tw.is_finite() && th.is_finite())
                                || tw * th * 4.0 > max_bytes as f64
                            {
                                return Err(too_much("pattern のタイル"));
                            }
                            let (tw, th) = (
                                crate::math::max(tw, 1.0) as u64,
                                crate::math::max(th, 1.0) as u64,
                            );
                            let tile = tw.saturating_mul(th);
                            let live = live.saturating_add(tile.saturating_mul(4));
                            if live > max_bytes {
                                return Err(too_much("pattern のタイル"));
                            }
                            work = work.saturating_add(tile);
                            stack.push((
                                Visit::Group(pat.root()),
                                tiny_skia::Transform::from_scale(sx, sy),
                                live,
                                (tw, th),
                            ));
                        }
                    }
                }
            },
        }
        if work > max_work {
            return Err(too_much("描く手間"));
        }
    }
    Ok(())
}

fn size_of(tree: &usvg::Tree, limits: &Limits) -> Result<(u32, u32), EngineError> {
    let s = tree.size().to_int_size();
    let (w, h) = (s.width(), s.height());
    if u64::from(w) > limits.max_dim || u64::from(h) > limits.max_dim {
        return Err(EngineError::Limit("SVG の大きさが上限を超えます".into()));
    }
    Ok((w, h))
}

impl Decoder for SvgCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn reproducible(&self) -> bool {
        false
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "svg"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let text = read_source(src)?;
        let tree = self.tree(&text)?;
        let (w, h) = size_of(&tree, limits)?;
        check_render(&tree, w, h, limits)?;
        Ok(Shape::Image {
            w: Dim::Exact(u64::from(w)),
            h: Dim::Exact(u64::from(h)),
            fmt: PixelFormat::Rgba8,
        })
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let text = read_source(src)?;
        let tree = self.tree(&text)?;
        let (w, h) = size_of(&tree, limits)?;
        check_render(&tree, w, h, limits)?;
        limits
            .check(&Shape::Image {
                w: Dim::Exact(u64::from(w)),
                h: Dim::Exact(u64::from(h)),
                fmt: PixelFormat::Rgba8,
            })
            .map_err(EngineError::Limit)?;
        let mut pixmap = tiny_skia::Pixmap::new(w, h)
            .ok_or_else(|| EngineError::Limit("画素を確保できません".into()))?;
        resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let mut rows = try_vec(w as usize * h as usize * 4, 0u8)?;
        for (dst, p) in rows.chunks_exact_mut(4).zip(pixmap.pixels()) {
            let c = p.demultiply();
            dst.copy_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        let px = Raster::from_rows(w, h, PixelFormat::Rgba8, None, &rows)?;
        Ok(Value::Image(Arc::new(Image {
            px,
            meta: ImageMeta::default(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use std::io::Cursor;

    fn decode(svg: &str) -> Result<Value, EngineError> {
        let c = SvgCodec::default();
        let shape = c.shape(
            &mut Cursor::new(svg.as_bytes().to_vec()),
            "svg",
            &DecodeOptions::default(),
            &Limits::default(),
        )?;
        let v = c.decode(
            &mut Cursor::new(svg.as_bytes().to_vec()),
            "svg",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )?;
        assert!(shape.admits(&v.shape()));
        Ok(v)
    }

    #[test]
    fn renders_shapes_and_sizes() {
        let v = decode(r##"<svg xmlns="http://www.w3.org/2000/svg" width="10.5" height="4"><rect x="0" y="0" width="5" height="4" fill="#ff0000"/></svg>"##).unwrap();
        let Value::Image(img) = v else { panic!() };
        assert_eq!((img.px.width(), img.px.height()), (11, 4));
        assert_eq!(img.px.pixel(0, 0), Some(&[255u8, 0, 0, 255][..]));
        assert_eq!(img.px.pixel(8, 2), Some(&[0u8, 0, 0, 0][..]));
        // 同じ環境では何度描いても同じ
        let a = decode(r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><circle cx="10" cy="10" r="7" fill="#0080ff" opacity="0.5" transform="rotate(30 10 10)"/></svg>"##).unwrap();
        let b = decode(r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><circle cx="10" cy="10" r="7" fill="#0080ff" opacity="0.5" transform="rotate(30 10 10)"/></svg>"##).unwrap();
        assert_eq!(a, b);
        assert!(!SvgCodec::default().reproducible());
    }

    /// 画像（外部のファイル・data: のラスター・入れ子の SVG。pattern や mask の中も）・フォントのない文字は Unsupported。
    #[test]
    fn images_and_text_are_unsupported() {
        let unsupported = |svg: &str| matches!(decode(svg), Err(EngineError::Unsupported(_)));
        let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let x = r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4""#;
        assert!(unsupported(&format!(
            r#"<svg {x}><image width="4" height="4" xlink:href="/etc/hosts"/></svg>"#
        )));
        assert!(unsupported(&format!(
            r#"<svg {x}><image width="4" height="4" xlink:href="{png}"/></svg>"#
        )));
        assert!(unsupported(&format!(
            r##"<svg {x}><defs><pattern id="p" width="4" height="4" patternUnits="userSpaceOnUse"><image width="4" height="4" xlink:href="{png}"/></pattern></defs><rect width="4" height="4" fill="url(#p)"/></svg>"##
        )));
        let inner =
            "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIxIiBoZWlnaHQ9IjEiLz4=";
        assert!(unsupported(&format!(
            r#"<svg {x}><image width="4" height="4" xlink:href="data:image/svg+xml;base64,{inner}"/></svg>"#
        )));
        assert!(unsupported(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><text x="0" y="15">Kura</text></svg>"#
        ));
    }

    fn decode_with(c: &SvgCodec, svg: &str) -> Result<Value, EngineError> {
        c.decode(
            &mut Cursor::new(svg.as_bytes().to_vec()),
            "svg",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
    }

    fn tiny_fonts() -> Vec<crate::codec::fonts::FontSource> {
        vec![crate::codec::fonts::FontSource {
            name: "kura.ttf".into(),
            data: Arc::new(crate::testfiles::tiny_ttf("KuraTest", &['A', 'B'])),
        }]
    }

    /// 決定 12：フォントの入れ物（バイト列を共有で受ける）を渡すと、文字を含む SVG を描く。字は正方形（x 0.1〜0.7 em・
    /// 高さ 0.7 em）なので、16px の「AB」は (2..11, 4..15) と (18..27, 4..15) あたりが塗られ、字の間は透明。
    #[test]
    fn text_is_drawn_with_font_sources() {
        let c = SvgCodec::with_svg_fonts(SvgFonts::new(
            tiny_fonts(),
            crate::codec::fonts::GenericFamilies::default(),
        ));
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><text x="0" y="16" font-family="KuraTest" font-size="16" fill="#000">AB</text></svg>"##;
        let Value::Image(img) = decode_with(&c, svg).unwrap() else {
            panic!()
        };
        let alpha = |x, y| img.px.pixel(x, y).unwrap()[3];
        assert_eq!(alpha(6, 10), 255, "A の中");
        assert_eq!(alpha(22, 10), 255, "B の中");
        assert_eq!(alpha(14, 10), 0, "字の間");
        assert_eq!(alpha(6, 1), 0, "字の上");
        assert_eq!(alpha(36, 10), 0, "字の後ろ");
        // 総称の書体名でも、名前のない書体でもなく、知らない書体名なら fontdb が最初の書体で代える（今の resvg の振る舞い）
        assert!(decode_with(&c, &svg.replace("KuraTest", "NoSuchFont")).is_ok());
    }

    /// 遅れて作るフォントは、文字のない SVG では作らず、文字のある SVG を読むときに 1 度だけ作る。
    #[test]
    fn lazy_fonts_are_built_only_for_text() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let k = calls.clone();
        let fonts = SvgFonts::lazy(crate::codec::fonts::GenericFamilies::default(), move || {
            k.fetch_add(1, Ordering::SeqCst);
            tiny_fonts()
        });
        let c = SvgCodec::with_svg_fonts(fonts.clone());
        decode_with(
            &c,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="#f00"/></svg>"##,
        )
        .unwrap();
        assert!(!fonts.is_built());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let text = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><text y="16" font-family="KuraTest" font-size="16">A</text></svg>"#;
        decode_with(&c, text).unwrap();
        decode_with(&c, text).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // 入れ物がひとつも読めなければ、文字のある SVG は読まない
        let empty = SvgCodec::with_svg_fonts(SvgFonts::lazy(
            crate::codec::fonts::GenericFamilies::default(),
            Vec::new,
        ));
        assert!(matches!(
            decode_with(&empty, text),
            Err(EngineError::Unsupported(_))
        ));
    }

    fn text_codec() -> SvgCodec {
        SvgCodec::with_svg_fonts(SvgFonts::new(
            tiny_fonts(),
            crate::codec::fonts::GenericFamilies::default(),
        ))
    }

    fn text_svg(body: &str) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" font-family="KuraTest" font-size="1">{body}</svg>"#
        )
    }

    /// 検証 S1：文字の塊の長さの 2 乗でかかるレイアウト（usvg は中止を見ない）を、usvg に入る前に断る。1 つの長い塊・
    /// 多くの text・use で増やした text・2 乗にならない形のどれでも、手間の合計で決める。ふつうの量の文字は読む。
    #[test]
    fn text_layout_work_is_refused_before_usvg() {
        let c = text_codec();
        let limit =
            |body: &str| matches!(decode_with(&c, &text_svg(body)), Err(EngineError::Limit(_)));
        // 1 つの塊（'a' で約 5,000 字まで）
        assert!(limit(&format!(
            "<text y=\"1\">{}</text>",
            "A".repeat(6_000)
        )));
        // 100 字ずつの text（合わせて約 3.5 万字まで）
        let line = format!("<text y=\"1\">{}</text>", "AB".repeat(50));
        assert!(limit(&line.repeat(400)));
        // defs の text を use で増やす
        assert!(limit(&format!(
            r##"<defs><text id="t" y="1">{}</text></defs>{}"##,
            "A".repeat(1_000),
            r##"<use href="#t"/>"##.repeat(400)
        )));
        // 1 字ずつの tspan（span ごとに塊を形にし直す）
        assert!(limit(&format!(
            "<text y=\"1\">{}</text>",
            "<tspan>A</tspan>".repeat(1_000)
        )));
        // ふつうの量：100 字のラベル 200 個、位置を付けた行の長い段落
        assert!(decode_with(&c, &text_svg(&line.repeat(200))).is_ok());
        let para = format!(
            "<text>{}</text>",
            (0..300)
                .map(|i| format!(r#"<tspan x="0" y="{i}">{}</tspan>"#, "AB".repeat(40)))
                .collect::<String>()
        );
        assert!(decode_with(&c, &text_svg(&para)).is_ok());
    }

    /// 検証 S1：フォントを選ぶ・代わりのフォントを探す手間（書体の数で決まる）は、木を作りながら残りの手間から引き、尽きたら
    /// 選ばずに `Limit` にする。
    #[test]
    fn font_selection_is_charged_while_building() {
        let c = text_codec();
        let svg = text_svg(&r#"<text y="1">AB</text>"#.repeat(10));
        let facts = check_elements(&svg).unwrap();
        assert!(facts.has_text && facts.text_work > 0);
        assert_eq!(facts.reshape, 2 * 256);
        assert!(matches!(
            c.tree_within(&svg, facts.text_work),
            Err(EngineError::Limit(_))
        ));
        assert!(c.tree_within(&svg, facts.text_work + 1_000_000).is_ok());
        // 'C' は書体にない字：代わりを探す分（書体の数 × 160 ＋ 塊の字の数 × 256）も数える
        let missing = text_svg(r#"<text y="1">ACCC</text>"#);
        let facts = check_elements(&missing).unwrap();
        assert!(matches!(
            c.tree_within(&missing, facts.text_work + 9_000),
            Err(EngineError::Limit(_))
        ));
        assert!(c.tree_within(&missing, facts.text_work + 20_000).is_ok());
        // 同じ字・同じフォントで代わりを探すのは 1 度だけ数える（10 個の text：フォントを選ぶ 10 回 × 8,196 ＋ 代わりを
        // 探す 1 回 1,184。覚えなければ 10 回で 93,800）
        let ten = text_svg(&r#"<text y="1">ACCC</text>"#.repeat(10));
        let facts = check_elements(&ten).unwrap();
        assert!(c.tree_within(&ten, facts.text_work + 83_200).is_ok());
        assert!(matches!(
            c.tree_within(&ten, facts.text_work + 83_100),
            Err(EngineError::Limit(_))
        ));
    }

    /// 検証 S1：textPath は字ごとに path をたどり、拡大が大きいと弧長を求める精度が上がって止まらなくなる（10²¹ で区間
    /// 1 つに 94 ms、曲線 4 つで 5 分を超えた）。区間の数 × 字の数を数え、textPath に至る変形の拡大の上限で断る。
    #[test]
    fn text_paths_are_bounded() {
        let c = text_codec();
        let tp = |scale: &str, d: &str, n: usize| {
            text_svg(&format!(
                r##"<defs><path id="p" d="{d}"/></defs><g transform="{scale}"><text><textPath href="#p">{}</textPath></text></g>"##,
                "A".repeat(n)
            ))
        };
        let curve = "M0 0 C1 2 3 4 5 6";
        assert!(decode_with(&c, &tp("scale(2)", curve, 10)).is_ok());
        // 1 つずつは 10⁹ までの数でも、重ねると 10¹⁸
        assert!(matches!(
            decode_with(&c, &tp("scale(1e9) scale(1e9)", curve, 10)),
            Err(EngineError::Limit(_))
        ));
        // use と入れ子の svg の viewBox でも重なる
        let nested = text_svg(&format!(
            r##"<defs><path id="p" d="{curve}"/><g id="g" transform="scale(1e9)"><svg viewBox="0 0 0.001 0.001"><text><textPath href="#p">AB</textPath></text></svg></g></defs><use href="#g"/>"##
        ));
        assert!(matches!(
            decode_with(&c, &nested),
            Err(EngineError::Limit(_))
        ));
        // 区間の多い path に沿う長い文字
        let d = format!("M0 0{}", " L1 1".repeat(20_000));
        assert!(matches!(
            decode_with(&c, &tp("", &d, 5_000)),
            Err(EngineError::Limit(_))
        ));
        // marker の中の textPath は読まない
        let marker = text_svg(&format!(
            r##"<defs><path id="p" d="{curve}"/><marker id="m"><text><textPath href="#p">A</textPath></text></marker></defs><path d="M0 0 L1 1" marker-end="url(#m)"/>"##
        ));
        assert!(matches!(
            decode_with(&c, &marker),
            Err(EngineError::Unsupported(_))
        ));
    }

    /// 展開爆弾：入れ子の実体・実体の値の中の '>'・注釈の中の偽の DOCTYPE・use・marker の鎖・頂点の多い marker・巨大な大きさ。
    #[test]
    fn expansion_bombs_are_refused() {
        let a = "a".repeat(1_000);
        let refs = |n: usize, name: &str| format!("&{name};").repeat(n);
        // 入れ子の実体はそれ自体を断る
        let nested = format!(
            r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "{a}"><!ENTITY b "&a;&a;">]><svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><desc>&b;</desc></svg>"#
        );
        assert!(matches!(decode(&nested), Err(EngineError::Unsupported(_))));
        // 値に '>' を含む大きな実体を大量に参照（引用符の中の '>' で値を短く数えない）
        let gt = format!(
            r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a ">{big}">]><svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><desc>{r}</desc></svg>"#,
            big = "x".repeat(100_000),
            r = refs(1_000, "a")
        );
        assert!(matches!(decode(&gt), Err(EngineError::Limit(_))));
        // 注釈の中の偽の DOCTYPE に惑わされない
        let fake = format!(
            r#"<?xml version="1.0"?><!-- <!DOCTYPE x []> --><!DOCTYPE svg [<!ENTITY a "{big}">]><svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><desc>{r}</desc></svg>"#,
            big = "x".repeat(100_000),
            r = refs(1_000, "a")
        );
        assert!(matches!(decode(&fake), Err(EngineError::Limit(_))));
        // パラメタ実体・外部の実体は読まない
        assert!(matches!(
            decode(
                r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY % p "x">]><svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#
            ),
            Err(EngineError::Unsupported(_))
        ));
        assert!(matches!(
            decode(
                r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY e SYSTEM "file:///etc/hosts">]><svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><desc>&e;</desc></svg>"#
            ),
            Err(EngineError::Unsupported(_))
        ));
        // use の倍々
        let mut uses = String::from(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="1" height="1"><defs><rect id="l0" width="1" height="1"/>"##,
        );
        for i in 1..30 {
            uses.push_str(&format!(
                r##"<g id="l{i}"><use xlink:href="#l{p}"/><use xlink:href="#l{p}"/></g>"##,
                p = i - 1
            ));
        }
        uses.push_str(r##"</defs><use xlink:href="#l29"/></svg>"##);
        assert!(matches!(decode(&uses), Err(EngineError::Limit(_))));
        let selfref = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="1" height="1"><g id="a"><use xlink:href="#a"/></g></svg>"##;
        assert!(decode(selfref).is_err());
        // marker の鎖（marker の中で marker を使う）
        let chain = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><marker id="m1"><path d="M0 0 L1 1 L2 2" marker-mid="url(#m2)"/></marker><marker id="m2"><rect width="1" height="1"/></marker></defs><path d="M0 0 L5 5 L9 9" marker-mid="url(#m1)"/></svg>"##;
        assert!(matches!(decode(chain), Err(EngineError::Unsupported(_))));
        // 頂点の多いパス × 大きな marker
        let mut big_marker = String::from(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><marker id="m">"##,
        );
        for _ in 0..4_000 {
            big_marker.push_str(r#"<rect width="1" height="1"/>"#);
        }
        big_marker.push_str("</marker></defs><path d=\"M0 0");
        for i in 0..2_000 {
            big_marker.push_str(&format!(" L{} {}", i % 10, i % 7));
        }
        big_marker.push_str("\" marker-mid=\"url(#m)\"/></svg>");
        assert!(matches!(decode(&big_marker), Err(EngineError::Limit(_))));
        let huge = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="100000"/>"#;
        assert!(matches!(decode(huge), Err(EngineError::Limit(_))));
        // Illustrator 風の単純な実体は読める
        let ai = r##"<?xml version="1.0"?><!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [<!ENTITY ns_svg "http://www.w3.org/2000/svg"><!ENTITY ns_flows "http://ns.adobe.com/Flows/1.0/">]><svg xmlns="&ns_svg;" width="2" height="2"><rect width="2" height="2" fill="#00ff00"/></svg>"##;
        assert!(decode(ai).is_ok());
        // 小さな marker は読める
        let small = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><marker id="m" markerWidth="2" markerHeight="2"><rect width="1" height="1"/></marker></defs><path d="M0 0 L5 5 L9 9" stroke="black" marker-mid="url(#m)"/></svg>"##;
        assert!(decode(small).is_ok());
    }

    fn nested(depth: usize) -> String {
        let mut s =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">"#);
        for _ in 0..depth {
            s.push_str("<g>");
        }
        s.push_str(r#"<rect width="2" height="2"/>"#);
        for _ in 0..depth {
            s.push_str("</g>");
        }
        s.push_str("</svg>");
        s
    }

    /// 入れ子の深さ：64 段（根の svg と葉の rect を含む）までは読め、それより深いものは XML を読む前に断る
    /// （roxmltree・usvg はスタックを使い切って落ちるため。このテストは 2 MiB のテストのスレッドで、デバッグのビルドで走る）。
    #[test]
    fn deep_nesting_is_refused_before_parsing() {
        assert!(decode(&nested(62)).is_ok());
        assert!(matches!(
            decode(&nested(63)),
            Err(EngineError::Unsupported(_))
        ));
        // roxmltree が落ちる深さでも、落ちずに断る
        assert!(matches!(
            decode(&nested(100_000)),
            Err(EngineError::Unsupported(_))
        ));
        // 注釈・CDATA・引用符の中の '<'・'>' は数えない
        let tricky = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><!-- {} --><desc><![CDATA[{}]]></desc><g id="a>b"/></svg>"#,
            "<g>".repeat(1_000),
            "<g>".repeat(1_000)
        );
        assert!(decode(&tricky).is_ok());
        // use で展開した高さも数える
        let mut chain = String::from(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4"><defs><g id="u0"><rect width="1" height="1"/></g>"##,
        );
        for i in 1..40 {
            chain.push_str(&format!(
                r##"<g id="u{i}"><use xlink:href="#u{}"/></g>"##,
                i - 1
            ));
        }
        chain.push_str(r##"</defs><use xlink:href="#u39"/></svg>"##);
        assert!(matches!(decode(&chain), Err(EngineError::Unsupported(_))));
    }

    /// 参照の輪：根の fill が pattern を指すと、pattern の中身がそれを受け継いで自分を描き直し続ける（ファズで見つけた。
    /// usvg はスタックを使い切って落ちる）。CSS で受け継ぐ形も断る。輪にならない CSS の参照（Illustrator の書き出し）は読む。
    #[test]
    fn reference_cycles_are_refused() {
        let x = r#"xmlns="http://www.w3.org/2000/svg" width="8" height="6""#;
        let cycles = [
            format!(
                r##"<svg {x} fill="url(#p)"><defs><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern></defs><rect width="8" height="6"/></svg>"##
            ),
            format!(
                r##"<svg {x}><g stroke="url(#p)"><pattern id="p" width="2" height="2"><rect width="1" height="1"/></pattern><rect width="8" height="6"/></g></svg>"##
            ),
            format!(
                r##"<svg {x}><style>.a {{ fill: url(#p) }}</style><g class="a"><pattern id="p" width="2" height="2"><rect width="1" height="1"/></pattern></g></svg>"##
            ),
            format!(
                r##"<svg {x}><style>g > rect {{ clip-path: url(#c) }}</style><clipPath id="c"><g><rect width="1" height="1"/></g></clipPath><g><rect width="8" height="6"/></g></svg>"##
            ),
            format!(
                r##"<svg {x}><defs><pattern id="p" width="2" height="2"><rect width="1" height="1" fill="url(#q)"/></pattern><pattern id="q" width="2" height="2"><rect width="1" height="1" fill="url(#p)"/></pattern></defs><rect width="8" height="6" fill="url(#p)"/></svg>"##
            ),
            format!(
                r##"<svg {x}><defs><mask id="m"><rect width="8" height="6" mask="url(#m)"/></mask></defs><rect width="8" height="6" mask="url(#m)"/></svg>"##
            ),
        ];
        for doc in cycles {
            let r = crate::testing::catch_own_panic(|| decode(&doc));
            assert!(
                matches!(r, Ok(Err(EngineError::Unsupported(_)))),
                "{doc}: {r:?}"
            );
        }
        let fine = [
            format!(
                r##"<svg {x}><style>.st0{{clip-path:url(#c);}} .st1{{fill:url(#g)}}</style><defs><clipPath id="c"><rect width="6" height="4"/></clipPath><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs><g class="st0"><rect class="st1" width="8" height="6"/></g></svg>"##
            ),
            format!(
                r##"<svg {x}><defs><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1" fill="#000"/></pattern></defs><rect width="8" height="6" fill="url(#p)"/></svg>"##
            ),
        ];
        for doc in fine {
            assert!(decode(&doc).is_ok(), "{doc}");
        }
    }

    /// pattern のタイルが大きすぎるもの（ファズで見つけた。外接矩形の比の巨大なタイル）は、描く前に断る。
    #[test]
    fn huge_pattern_tiles_are_refused() {
        let x = r#"xmlns="http://www.w3.org/2000/svg" width="16" height="12" viewBox="0 0 8 6""#;
        let tile = |svg: &str| matches!(decode(svg), Err(EngineError::Unsupported(m)) if m.contains("pattern のタイル"));
        let huge = format!(
            r##"<svg {x}><defs><pattern id="p" width="3" height="2"><rect width="1" height="1"/></pattern></defs><ellipse cx="4" cy="3" rx="46666" ry="46666" fill="url(#p)"/></svg>"##
        );
        assert!(tile(&huge));
        // 入れ子の pattern の中の大きなタイルも
        let nested = format!(
            r##"<svg {x}><defs><pattern id="q" width="40000" height="40000" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="2" height="2" fill="url(#q)"/></pattern></defs><rect width="8" height="6" fill="url(#p)"/></svg>"##
        );
        assert!(tile(&nested));
        // フィルタの feImage の中身・mask の中身の pattern も
        let pat = r#"<pattern id="p" width="3" height="2"><rect width="1" height="1"/></pattern>"#;
        let big = r#"<ellipse id="e" cx="4" cy="3" rx="46666" ry="46666" fill="url(#p)"/>"#;
        let fe = format!(
            r##"<svg {x}><defs>{pat}{big}<filter id="f"><feImage href="#e"/></filter></defs><rect width="8" height="6" filter="url(#f)"/></svg>"##
        );
        assert!(tile(&fe));
        let mask = format!(
            r##"<svg {x}><defs>{pat}<mask id="m">{big}</mask></defs><rect width="8" height="6" mask="url(#m)"/></svg>"##
        );
        assert!(tile(&mask));
        let fine = format!(
            r##"<svg {x}><defs><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern></defs><rect width="8" height="6" fill="url(#p)"/></svg>"##
        );
        assert!(decode(&fine).is_ok());
    }

    /// 描く前の資源の見積もり：同時に生きる一時の画像と描く手間の上限で、資源の爆弾を描く前に断る（検証で見つけた形）。
    #[test]
    #[allow(clippy::disallowed_methods)] // 断るまでの時間（描かずに断ること）を確かめるだけ
    fn render_budget_refuses_resource_bombs() {
        let refused = |svg: &str| {
            let t = std::time::Instant::now();
            let r = decode(svg);
            assert!(
                t.elapsed() < std::time::Duration::from_secs(5),
                "{:?}",
                t.elapsed()
            );
            matches!(r, Err(EngineError::Unsupported(_) | EngineError::Limit(_)))
        };
        let svg = |w: u32, body: &str| {
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{w}">{body}</svg>"#
            )
        };
        let region = r#"filterUnits="userSpaceOnUse" x="-1e6" y="-1e6" width="2e6" height="2e6""#;
        // フィルタのプリミティブの結果はすべて同時に生きる（20,000 個 × 層 320² × 4 バイト）
        let floods = svg(
            64,
            &format!(
                r#"<filter id="f" {region}>{}</filter><rect width="8" height="8" filter="url(#f)"/>"#,
                "<feFlood/>".repeat(20_000)
            ),
        );
        assert!(refused(&floods));
        // 層はキャンバスの 25 倍まで広がる（16384² のキャンバスで 1 枚 25 GiB）
        let layer = svg(
            16_384,
            r#"<g opacity=".5"><rect x="-1e6" y="-1e6" width="2e6" height="2e6"/></g>"#,
        );
        assert!(refused(&layer));
        // 手間：feMorphology は画素ごとに半径の矩形を、feTurbulence はオクターブの数だけ、ぼかしは半径の分だけ回る
        for prim in [
            r#"<feMorphology radius="1e6"/>"#,
            r#"<feTurbulence baseFrequency=".05" numOctaves="4000000000"/>"#,
            r#"<feGaussianBlur stdDeviation="1e30"/>"#,
        ] {
            let bomb = svg(
                256,
                &format!(
                    r#"<filter id="f" {region}>{prim}</filter><rect width="256" height="256" filter="url(#f)"/>"#
                ),
            );
            assert!(refused(&bomb), "{prim}");
        }
        let kernel = vec!["1"; 200 * 200].join(" ");
        let convolve = svg(
            512,
            &format!(
                r#"<filter id="f"><feConvolveMatrix order="200" kernelMatrix="{kernel}"/></filter><rect width="512" height="512" filter="url(#f)"/>"#
            ),
        );
        assert!(refused(&convolve));
        // 変形のあとの座標が極端な図形（ファズで見つけた遅い入力の形）
        let far = svg(
            16,
            r#"<g transform="rotate(11 4 3)"><ellipse cx="4" cy="3" ry="1.3e37" stroke="black"/></g>"#,
        );
        assert!(refused(&far));
        // ふつうの SVG は読める：mask・不透明度・ぼかし・共有の hatch を 2,000 回使うもの
        let lines: String = (0..600)
            .map(|i| {
                format!(
                    r#"<line x1="{i}" y1="0" x2="{i}" y2="8" stroke="black" stroke-width=".01"/>"#
                )
            })
            .collect();
        let rects: String = (0..2_000)
            .map(|i| {
                format!(
                    r#"<rect x="{}" y="{}" width="5" height="5" fill="url(#h)"/>"#,
                    i % 40 * 6,
                    i / 40 * 6
                )
            })
            .collect();
        let fine = svg(
            256,
            &format!(
                r#"<defs><pattern id="h" width="8" height="8" patternUnits="userSpaceOnUse">{lines}</pattern><mask id="m"><rect width="256" height="256" fill="white"/></mask><filter id="b"><feGaussianBlur stdDeviation="2"/></filter></defs><g opacity=".8" mask="url(#m)"><g filter="url(#b)">{rects}</g></g>"#
            ),
        );
        assert!(decode(&fine).is_ok());
    }

    /// 極端に大きな数（usvg が弧・円を細かい曲線に直して時間がかかる）は XML を読んだところで断る。色・id・参照・名前の中の
    /// 数字は数として読まない。
    #[test]
    #[allow(clippy::disallowed_methods)] // 断るまでの時間（描かずに断ること）を確かめるだけ
    fn huge_numbers_are_refused_before_usvg() {
        for v in [
            "M0 0 A 1e30 1e30 0 1 1 1e30 0",
            "M1e10 0",
            "M 0,0 L-4000000000,1",
        ] {
            assert!(check_numbers(v, true).is_err(), "{v}");
        }
        for v in ["1e10", "scale(2e9)", "-1e300", "1e999", ".5e10"] {
            assert!(check_numbers(v, false).is_err(), "{v}");
        }
        for v in [
            "#1e30ff",
            "url(#p1e30)",
            "a1e30 b2e40",
            "Font1e30, serif",
            "M0 0 L 999999999 -999999999 Z",
            "matrix(1 0 0 1 1e8 -1e8)",
            "0.5e-30",
        ] {
            assert!(check_numbers(v, v.starts_with('M')).is_ok(), "{v}");
        }
        // どんな並びでも終わる（ファズで見つけた無限ループ：数字のあとの `+`）。英数字と記号のすべての 3 文字の並びで確かめる
        let alphabet = b"0a.+-e#u(lr)E ,";
        for &x in alphabet {
            for &y in alphabet {
                for &z in alphabet {
                    let v = String::from_utf8(vec![x, y, z, b'1']).unwrap();
                    let _ = check_numbers(&v, false);
                    let _ = check_numbers(&v, true);
                }
            }
        }
        assert!(check_numbers("0+3", false).is_ok());
        let x = r#"xmlns="http://www.w3.org/2000/svg" width="8" height="8""#;
        let arc = format!(r#"<svg {x}><path d="M0 0 A 1e30 1e30 0 1 1 1e30 0"/></svg>"#);
        let t = std::time::Instant::now();
        assert!(matches!(decode(&arc), Err(EngineError::Unsupported(_))));
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        let css = format!(
            r#"<svg {x}><style>.c1e30 {{ stroke-width: 1e37 }}</style><rect class="c1e30" width="4" height="4"/></svg>"#
        );
        assert!(matches!(decode(&css), Err(EngineError::Unsupported(_))));
        let names = format!(
            r##"<svg {x}><defs><linearGradient id="g1e30"><stop stop-color="#1e30ff"/></linearGradient></defs><style>.c1e30 {{ fill: url(#g1e30) }}</style><rect id="r2e40" class="c1e30" width="4" height="4"/></svg>"##
        );
        assert!(decode(&names).is_ok());
    }

    /// usvg が参照ごとに中身を複製する pattern（単位が objectBoundingBox）の入れ子は、木を作る前に断る（指数に増える）。
    #[test]
    #[allow(clippy::disallowed_methods)] // 断るまでの時間（描かずに断ること）を確かめるだけ
    fn nested_pattern_clones_are_refused_before_usvg() {
        let x = r#"xmlns="http://www.w3.org/2000/svg" width="8" height="8""#;
        let mut defs = String::from(
            r#"<pattern id="p0" width=".5" height=".5"><rect width="1" height="1"/></pattern>"#,
        );
        for i in 1..24 {
            defs.push_str(&format!(
                r##"<pattern id="p{i}" width=".5" height=".5"><rect width=".5" height=".5" fill="url(#p{})"/><rect x=".5" width=".5" height=".5" fill="url(#p{})"/></pattern>"##,
                i - 1,
                i - 1
            ));
        }
        let svg = format!(
            r##"<svg {x}><defs>{defs}</defs><rect width="8" height="8" fill="url(#p23)"/></svg>"##
        );
        let t = std::time::Instant::now();
        assert!(matches!(decode(&svg), Err(EngineError::Limit(_))));
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        // 受け継いだ fill も、使う図形ごとに複製として数える
        let mut inherited = String::from(
            r#"<pattern id="p0" width=".5" height=".5"><rect width="1" height="1"/></pattern>"#,
        );
        for i in 1..24 {
            inherited.push_str(&format!(
                r##"<pattern id="p{i}" width=".5" height=".5"><g fill="url(#p{})"><rect width=".5" height=".5"/><rect x=".5" width=".5" height=".5"/></g></pattern>"##,
                i - 1
            ));
        }
        let svg = format!(
            r##"<svg {x}><defs>{inherited}</defs><rect width="8" height="8" fill="url(#p23)"/></svg>"##
        );
        assert!(matches!(decode(&svg), Err(EngineError::Limit(_))));
        // 浅い入れ子と、共有される（userSpaceOnUse の）pattern は読める
        let shallow = format!(
            r##"<svg {x}><defs><pattern id="a" width=".5" height=".5"><circle r=".2" cx=".25" cy=".25"/></pattern></defs>{}</svg>"##,
            r##"<rect width="8" height="8" fill="url(#a)"/>"##.repeat(1_000)
        );
        assert!(decode(&shallow).is_ok());
    }

    /// CSS の照合と宣言の手間（検証で見つけた形）は、usvg に渡す前に断る。ふつうの CSS は読める。
    #[test]
    #[allow(clippy::disallowed_methods)] // 断るまでの時間（usvg に渡さずに断ること）を確かめるだけ
    fn css_costs_are_bounded() {
        let x = r#"xmlns="http://www.w3.org/2000/svg" width="8" height="8""#;
        // 上限で断る形は単独のデバッグのビルドで 1 つ 0.5 秒ほど。同じテストのバイナリの重いテスト（段 7 の Vorbis の照合など）と
        // 並ぶと数倍に延びるので、閾値は 10 秒にする。断らなかったときの形（宣言の数の 2 乗・組み合わせの数の後戻り）は数分かかる。
        let quick = |svg: &str| {
            let t = std::time::Instant::now();
            let r = decode(svg);
            assert!(
                t.elapsed() < std::time::Duration::from_secs(10),
                "{:?}",
                t.elapsed()
            );
            r
        };
        // 隣接の結合子の長い連鎖（simplecss は段の数だけ再帰する）
        let chain = format!(
            "<svg {x}><style>{}g{{fill:red}}</style>{}</svg>",
            "g+".repeat(11_000),
            "<g/>".repeat(11_000)
        );
        assert!(matches!(quick(&chain), Err(EngineError::Unsupported(_))));
        // 深い入れ子での子孫の結合子の後戻り（組み合わせの数で増える）
        let deep = format!(
            "<svg {x}><style>y g g g g g g rect{{fill:red}}</style>{}<rect/>{}</svg>",
            "<g>".repeat(61),
            "</g>".repeat(61)
        );
        assert!(matches!(quick(&deep), Err(EngineError::Limit(_))));
        // 同じ id の要素が多く、1 つの要素がそれを何度も参照する（同じ参照は 1 度だけ解くので、辺は膨らまない）
        let dup = format!(
            r#"<svg {x}><defs>{}</defs><rect width="4" height="4" style="{}"/></svg>"#,
            r#"<clipPath id="p"/>"#.repeat(3_000),
            "fill:url(#p);".repeat(3_000)
        );
        let _ = quick(&dup);
        // 1 つの規則・style 属性の宣言が多い（usvg は宣言の数の 2 乗の手間）
        let decls = format!(
            "<svg {x}><style>g{{{}}}</style><g/></svg>",
            "opacity:1;".repeat(80_000)
        );
        assert!(matches!(quick(&decls), Err(EngineError::Limit(_))));
        let style = format!(
            r#"<svg {x}><g style="{}"/></svg>"#,
            "opacity:1;".repeat(300_000)
        );
        assert!(matches!(quick(&style), Err(EngineError::Limit(_))));
        // ふつうの CSS：クラスの規則 300 個 × 要素 5,000、子孫の結合子 2 つ
        let rules: String = (0..300)
            .map(|i| format!(".st{i}{{fill:#123456;stroke:#000;stroke-width:1}}"))
            .collect();
        let shapes: String = (0..5_000)
            .map(|i| format!(r#"<rect class="st{}" width="1" height="1"/>"#, i % 300))
            .collect();
        let fine = format!(
            "<svg {x}><style>{rules} svg g rect{{opacity:.9}}</style>{}{shapes}{}</svg>",
            "<g>".repeat(10),
            "</g>".repeat(10)
        );
        assert!(decode(&fine).is_ok());
        // 結合子の数え方
        assert_eq!(combinators("svg g > rect + circle ~ path"), (5, 1, 1));
        assert_eq!(combinators(r#"a[title="x > y"]  b"#), (2, 1, 0));
        assert_eq!(choose(61, 6), 55_525_372);
    }

    /// usvg が CSS をすべての要素に照らす手間（セレクタの数 × 要素の数）と、自前の参照の見積もりの手間に上限がある。
    #[test]
    fn css_and_reference_work_is_bounded() {
        let x = r#"xmlns="http://www.w3.org/2000/svg" width="4" height="4""#;
        let rules: String = (0..12_000).map(|i| format!(".c{i}{{}}")).collect();
        let many = format!(
            "<svg {x}><style>{rules}</style>{}</svg>",
            "<g/>".repeat(12_000)
        );
        assert!(matches!(decode(&many), Err(EngineError::Limit(_))));
        // 深い入れ子のどの段にも参照の多い規則が当たり、その中に図形が並ぶ（図形はどれも受け継いだ参照をすべて使う）
        let rules: String = (0..4_000)
            .map(|i| format!("g{{fill:url(#p{i})}}"))
            .collect();
        let patterns: String = (0..4_000)
            .map(|i| format!(r#"<pattern id="p{i}"/>"#))
            .collect();
        let deep = format!(
            "<svg {x}><style>{rules}</style><defs>{patterns}</defs>{}{}{}</svg>",
            "<g>".repeat(60),
            "<rect/>".repeat(5_000),
            "</g>".repeat(60)
        );
        assert!(matches!(decode(&deep), Err(EngineError::Limit(_))));
        // 参照先のない規則は、いくつ重なっても手間にならない
        let dangling = format!(
            "<svg {x}><style>{rules}</style>{}{}{}</svg>",
            "<g>".repeat(60),
            "<clipPath/>".repeat(120),
            "</g>".repeat(60)
        );
        assert!(decode(&dangling).is_ok());
        // ふつうの大きさなら読める
        let rules: String = (0..300).map(|i| format!(".c{i}{{fill:#000}}")).collect();
        let fine = format!(
            "<svg {x}><style>{rules}</style>{}</svg>",
            r#"<rect class="c1" width="1" height="1"/>"#.repeat(3_000)
        );
        assert!(decode(&fine).is_ok());
    }

    /// 参照の解き方を usvg より広く取る：href の空白・名前空間、同じ id の重複、xml:id、CSS の注釈・引用符・@media の中の規則。
    #[test]
    fn reference_resolution_covers_what_usvg_follows() {
        let x = r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="8" height="6""#;
        let cycles = [
            // 古い書き方の性質の名前（`*fill`）も受け継ぐ参照として扱う（style 属性と CSS）
            format!(
                r##"<svg {x} style="*fill:url(#p)"><defs><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern></defs><rect width="8" height="6"/></svg>"##
            ),
            format!(
                r##"<svg {x}><style>svg{{*fill:url(#p)}}</style><defs><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern></defs><rect width="8" height="6"/></svg>"##
            ),
            // 複雑なセレクタの参照先が、中身を href の先から借りる
            format!(
                r##"<svg {x}><style>svg rect{{fill:url(#p)}}</style><defs><pattern id="p" xlink:href="#q"/><pattern id="q" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern></defs><rect width="8" height="6"/></svg>"##
            ),
            // href の前の空白（usvg は " #a" も a として解く）
            format!(r##"<svg {x}><g id="a"><use href=" #a"/></g></svg>"##),
            // 名前空間の違う href を並べても、どれかが輪なら断る
            format!(
                r##"<svg {x}><g id="a"><use xlink:href="#b" href="#a"/></g><rect id="b"/></svg>"##
            ),
            // 同じ id の 2 つめ（usvg の url の参照は後の要素を使う）
            format!(
                r##"<svg {x}><defs><pattern id="p" width="2" height="2"><rect width="1" height="1"/></pattern><pattern id="p" width="2" height="2"><rect width="1" height="1" fill="url(#p)"/></pattern></defs><rect width="8" height="6" fill="url(#p)"/></svg>"##
            ),
            // xml:id
            format!(
                r##"<svg {x}><defs><pattern xml:id="q" width="2" height="2"><rect width="1" height="1" fill="url(#q)"/></pattern></defs><rect width="8" height="6" fill="url(#q)"/></svg>"##
            ),
            // 注釈の中の '}' のあとの受け継ぐ参照
            format!(
                r##"<svg {x}><style>svg {{ /* }} */ fill: url(#p) }}</style><pattern id="p" width="2" height="2"><rect width="1" height="1"/></pattern></svg>"##
            ),
            // style 属性の中の注釈
            format!(
                r##"<svg {x} style="/**/fill: url(#p)"><pattern id="p" width="2" height="2"><rect width="1" height="1"/></pattern></svg>"##
            ),
            // @media の中の規則
            format!(
                r##"<svg {x}><style>@media screen {{ g {{ fill: url(#p) }} }}</style><g><pattern id="p" width="2" height="2"><rect width="1" height="1"/></pattern></g></svg>"##
            ),
        ];
        for doc in cycles {
            let r = crate::testing::catch_own_panic(|| decode(&doc));
            assert!(
                matches!(r, Ok(Err(EngineError::Unsupported(_)))),
                "{doc}: {r:?}"
            );
        }
        let fine = [
            // 引用符の中の '}'・注釈のある、輪にならない CSS
            format!(
                r##"<svg {x}><style>/* a }} b */ .st0 {{ fill: url("#g") }} .st1{{font-family:"a}}b"}}</style><defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs><rect class="st0 st1" width="8" height="6"/></svg>"##
            ),
            // symbol を use で使う
            format!(
                r##"<svg {x}><defs><symbol id="s"><rect width="2" height="2"/></symbol></defs><use href="#s"/><use xlink:href="#s" x="3"/></svg>"##
            ),
            // gradient の href の鎖
            format!(
                r##"<svg {x}><defs><linearGradient id="a"><stop offset="0" stop-color="#f00"/></linearGradient><linearGradient id="b" href="#a"/></defs><rect width="8" height="6" fill="url(#b)"/></svg>"##
            ),
        ];
        for doc in fine {
            assert!(decode(&doc).is_ok(), "{doc}");
        }
        // 参照を含む CSS の規則が多すぎるものは断る
        let mut many = format!(r#"<svg {x}><style>"#);
        for i in 0..5_000 {
            many.push_str(&format!(".c{i}{{fill:url(#g)}}"));
        }
        many.push_str("</style></svg>");
        assert!(matches!(decode(&many), Err(EngineError::Unsupported(_))));
    }

    /// DTD・前置きの中の多バイト文字で落ちない（長いファズで見つけた、文字の途中で文字列を切る panic）。
    #[test]
    fn multibyte_text_around_the_dtd() {
        let svg = r#"xmlns="http://www.w3.org/2000/svg" width="4" height="4""#;
        for doc in [
            format!(
                r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY ns "鞍"><!-- 注釈 -->]><svg {svg}/>"#
            ),
            format!(r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY ns "x">]>鞍<svg {svg}/>"#),
            r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY ns "x"><circle/></svg>ü鞍"#.to_string(),
            format!(r#"<?xml version="1.0"?><!DOCTYPE svg [ü鞍 'あ' "い" <?pi う?>]><svg {svg}/>"#),
            format!(r#"<!-- 前置きの注釈 --><?pi え?><svg {svg}/>"#),
        ] {
            let r = crate::testing::catch_own_panic(|| decode(&doc));
            assert!(r.is_ok(), "{doc}: {r:?}");
        }
        assert!(
            decode(&format!(
                r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY ns "鞍">]><svg {svg}/>"#
            ))
            .is_ok()
        );
    }
}
