//! SVG（resvg・usvg。版は `=` で固定。`svg-resvg-dec/1`）。書き出しはしない。
//!
//! - resvg は中で std の三角関数（OS の libm）を使うので、同じ環境では同じ値だが、OS の版で変わりうる。そこで
//!   `reproducible()` を偽にして、初めて編集するときに読み取った値を可逆形式で残す（アンカー。docs/editing.md §9.4）。
//! - 大きさは SVG の幅・高さ（ユーザー単位、96 dpi）を切り上げた画素。Rgba8（乗算済みの値を戻したもの）。
//! - `<image>`（外部のファイル・data: のラスター画像・入れ子の SVG）を含むものは、この段では `Unsupported`（次の段へ）。
//!   手元のファイルは読まない。resvg の raster-images は zune-jpeg の SIMD を有効にするので入れない。入れ子の SVG は、
//!   外側と同じ見積もりを通らないので読まない。
//! - 文字は、渡したフォント（`SvgCodec::with_fonts`）だけで描く。フォントを渡していないのに文字があれば `Unsupported`。
//! - 展開爆弾を避けるため、usvg に渡す前に、ファイルの大きさ・DTD の実体の展開の大きさ・要素の数・`<use>` の展開・
//!   marker の頂点ごとの複製を見積もって断る。DTD は引用符・注釈を区別して読み、パラメタ実体・外部の実体・
//!   マークアップや参照を含む実体は断る（`Unsupported`）。入れ子の marker も断る。
//!   描く時間は中止できない（resvg を途中で止める口がない）。

use std::io::SeekFrom;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use resvg::tiny_skia;
use resvg::usvg;

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

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: e.to_string(),
    }
}

/// SVG の読み取り。文字を描くフォントを持てる。
#[derive(Clone, Default)]
pub struct SvgCodec {
    fonts: Arc<usvg::fontdb::Database>,
}

impl SvgCodec {
    /// 文字を描くフォントを渡す（kura-mac が OS のフォントを読んで渡す）。
    pub fn with_fonts(fonts: Arc<usvg::fontdb::Database>) -> Self {
        Self { fonts }
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

/// 要素の数（`<use>` の参照先と、marker の頂点ごとの複製を足したもの）を見積もる。文字の要素があるかも返す。
fn check_elements(text: &str) -> Result<bool, EngineError> {
    let opt = usvg::roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: MAX_ELEMENTS as u32,
        ..Default::default()
    };
    let doc = usvg::roxmltree::Document::parse_with_options(text, opt).map_err(|e| match e {
        usvg::roxmltree::Error::NodesLimitReached => EngineError::Limit("要素が多すぎます".into()),
        other => err(other),
    })?;
    let mut by_id: std::collections::BTreeMap<&str, usvg::roxmltree::Node<'_, '_>> =
        std::collections::BTreeMap::new();
    let mut has_text = false;
    let mut css_markers = false;
    for n in doc.descendants().filter(|n| n.is_element()) {
        if let Some(id) = n.attribute("id") {
            by_id.entry(id).or_insert(n);
        }
        match n.tag_name().name() {
            "text" | "tspan" | "textPath" => has_text = true,
            "style" => {
                if n.text().is_some_and(|t| t.contains("marker")) {
                    css_markers = true;
                }
            }
            _ => {}
        }
    }
    let shapes = |n: &usvg::roxmltree::Node<'_, '_>| {
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
    fn size<'a, 'i>(
        n: usvg::roxmltree::Node<'a, 'i>,
        by_id: &std::collections::BTreeMap<&str, usvg::roxmltree::Node<'a, 'i>>,
        stack: &mut Vec<u32>,
        memo: &mut std::collections::BTreeMap<u32, u64>,
    ) -> Result<u64, EngineError> {
        let id = n.id().get();
        if let Some(v) = memo.get(&id) {
            return Ok(*v);
        }
        if stack.contains(&id) || stack.len() > 64 {
            return Err(err("use が自分を指しています"));
        }
        stack.push(id);
        let mut total = 1u64;
        for c in n.children().filter(|c| c.is_element()) {
            total = total.saturating_add(size(c, by_id, stack, memo)?);
        }
        if n.tag_name().name() == "use" {
            let href = n
                .attributes()
                .find(|a| a.name() == "href")
                .map(|a| a.value())
                .unwrap_or_default();
            if let Some(target) = href.strip_prefix('#').and_then(|id| by_id.get(id)) {
                total = total.saturating_add(size(*target, by_id, stack, memo)?);
            }
        }
        stack.pop();
        if total > MAX_ELEMENTS {
            return Err(EngineError::Limit(
                "use を展開すると要素が多すぎます".into(),
            ));
        }
        memo.insert(id, total);
        Ok(total)
    }
    let mut memo = std::collections::BTreeMap::new();
    let mut total = size(doc.root_element(), &by_id, &mut Vec::new(), &mut memo)?;
    // marker の複製：頂点の数 × marker の中身の大きさ（CSS で付くものは、いちばん大きい marker とみなす）
    let markers: Vec<u64> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "marker")
        .map(|m| size(m, &by_id, &mut Vec::new(), &mut memo))
        .collect::<Result<_, _>>()?;
    let largest = markers.iter().copied().max().unwrap_or(0);
    for n in doc.descendants().filter(|n| n.is_element() && shapes(n)) {
        let refs = marker_refs(&n);
        let per_vertex: u64 = if css_markers {
            largest
        } else {
            refs.iter()
                .filter_map(|id| by_id.get(id))
                .map(|m| size(*m, &by_id, &mut Vec::new(), &mut memo))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .sum()
        };
        total = total.saturating_add(vertices(&n).saturating_mul(per_vertex));
        if total > MAX_ELEMENTS {
            return Err(EngineError::Limit(
                "marker を展開すると要素が多すぎます".into(),
            ));
        }
    }
    Ok(has_text)
}

impl SvgCodec {
    fn tree(&self, text: &str) -> Result<usvg::Tree, EngineError> {
        check_entities(text)?;
        let has_text = check_elements(text)?;
        if has_text && self.fonts.is_empty() {
            return Err(EngineError::Unsupported(
                "文字を含む SVG は、フォントを渡したときだけ読めます".into(),
            ));
        }
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
            fontdb: self.fonts.clone(),
            ..usvg::Options::default()
        };
        let tree = usvg::Tree::from_str(text, &opt).map_err(err)?;
        if images.load(Ordering::Relaxed) {
            return Err(EngineError::Unsupported(
                "画像（埋め込みの SVG・ラスター画像・外部のファイル）を含む SVG は、この段では読めません".into(),
            ));
        }
        Ok(tree)
    }
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
