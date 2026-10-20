//! テスト・ゴールデン・devtools で使う補助：操作 1 つの文書を作って描く。

pub mod bench;
pub mod platform;
pub mod preview;
pub mod region;
mod snapshot;

pub use platform::{FakePlatformDecoder, FakePlatformEncoder};

use std::collections::BTreeMap;

use crate::digest::{Digest, Hasher};
use crate::doc::{Graph, InputId};
use crate::error::EngineError;
use crate::monitor::{Monitor, NoMonitor};
use crate::op::DynExample;
use crate::render::{Engine, InputValue};
use crate::testgen::InputGen;
use crate::value::Value;

/// 例の入力の作り方から、元のハッシュの代わりのダイジェストを作る（種に使う）。
pub fn source_of(gens: &[InputGen]) -> Result<Digest, EngineError> {
    let bytes = crate::digest::canonical(&gens)?;
    Ok(Hasher::new("kura-testgen-v1")
        .bytes_with_len(&bytes)
        .finish())
}

/// 入力 1 つの文書に操作 1 つ（種 0）をつないだもの。
pub fn single_node_graph(
    op: &str,
    v: u16,
    params: serde_json::Value,
) -> Result<Graph, EngineError> {
    let mut g = Graph::new_single();
    g.push_linear(op, v, params, 0)?;
    Ok(g)
}

/// 文書を値に当てて、描いた結果をすべて（警告を含めて）返す。
pub fn render_full(
    engine: &Engine,
    graph: &Graph,
    input: Value,
    source: Digest,
    mon: &dyn Monitor,
) -> Result<crate::render::Rendered, EngineError> {
    let shapes = BTreeMap::from([(InputId(0), input.shape())]);
    let plan = engine.compile(graph, &shapes)?;
    let inputs = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input,
            source,
        },
    )]);
    engine.render(&plan, &inputs, mon)
}

/// 文書を値に当てて、出力 0 の値を返す。
pub fn render_with(
    engine: &Engine,
    graph: &Graph,
    input: Value,
    source: Digest,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    let mut rendered = render_full(engine, graph, input, source, mon)?;
    if rendered.outputs.is_empty() {
        return Err(EngineError::Internal("出力がありません".into()));
    }
    Ok(rendered.outputs.swap_remove(0).value)
}

/// 操作 1 つを値に当てる（既定のエンジン、元のハッシュは 0）。
pub fn render_value(
    op: &str,
    v: u16,
    params: serde_json::Value,
    input: Value,
) -> Result<Value, EngineError> {
    let engine = Engine::new()?;
    render_with(
        &engine,
        &single_node_graph(op, v, params)?,
        input,
        Digest::default(),
        &NoMonitor,
    )
}

/// 入力の作り方から値を作って、操作 1 つを当てる。
pub fn render_single(
    op: &str,
    v: u16,
    params: serde_json::Value,
    inputs: &[InputGen],
) -> Result<Value, EngineError> {
    let first = inputs
        .first()
        .ok_or_else(|| EngineError::Internal("入力がありません".into()))?;
    let engine = Engine::new()?;
    render_with(
        &engine,
        &single_node_graph(op, v, params)?,
        first.generate()?,
        source_of(inputs)?,
        &NoMonitor,
    )
}

/// 例を描いて、出力のダイジェストを返す（ゴールデンに使う）。
pub fn example_digest(
    engine: &Engine,
    op: &str,
    v: u16,
    example: &DynExample,
) -> Result<Digest, EngineError> {
    let first = example
        .inputs
        .first()
        .ok_or_else(|| EngineError::Internal("例に入力がありません".into()))?;
    let out = render_with(
        engine,
        &single_node_graph(op, v, example.params.clone())?,
        first.generate()?,
        source_of(&example.inputs)?,
        &NoMonitor,
    )?;
    out.digest()
}

thread_local! {
    /// `catch_own_panic` の中で起きた panic の場所（ファイル）。
    static PANIC_AT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static IN_CATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

static HOOK: std::sync::Once = std::sync::Once::new();
/// どのスレッドで起きたかによらず数える、自前のコードの panic の数。`par_blocks` の中（rayon の作業スレッド）で起きた panic は、
/// 呼んだスレッドの catch_unwind に届く前に作業スレッドでフックが走るので、スレッドごとの記録では見分けられないため。
static OWN_PANICS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn is_own(at: &str) -> bool {
    at.contains("kura-engine/src") || at.starts_with("src/")
}

/// テスト用：`f` を呼ぶ。panic したら場所を調べ、kura-engine の自前のコード（`crates/kura-engine/src`）の panic なら
/// `Err(場所)`、第三者のクレートの panic なら `Ok(Err(Decode))` にする（本番の `codec::contain` と同じく受け止める）。
/// 壊れた入力のテストで、自前のコードの panic を見逃さないために使う。作業スレッドで起きた自前の panic も数える。
pub fn catch_own_panic<T>(
    f: impl FnOnce() -> Result<T, EngineError>,
) -> Result<Result<T, EngineError>, String> {
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let at = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()));
            if at.as_deref().is_some_and(is_own) {
                OWN_PANICS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if IN_CATCH.with(std::cell::Cell::get) {
                PANIC_AT.with(|p| *p.borrow_mut() = at);
            } else {
                previous(info);
            }
        }));
    });
    let own_before = OWN_PANICS.load(std::sync::atomic::Ordering::SeqCst);
    IN_CATCH.with(|c| c.set(true));
    PANIC_AT.with(|p| *p.borrow_mut() = None);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    IN_CATCH.with(|c| c.set(false));
    let own_elsewhere = OWN_PANICS.load(std::sync::atomic::Ordering::SeqCst) != own_before;
    match r {
        Ok(v) => Ok(v),
        Err(_) => {
            let at = PANIC_AT.with(|p| p.borrow_mut().take()).unwrap_or_default();
            if is_own(&at) {
                Err(at)
            } else if own_elsewhere {
                Err(format!("作業スレッドの自前のコード（{at}）"))
            } else {
                Ok(Err(EngineError::Decode {
                    codec: "third-party".into(),
                    message: format!("第三者のクレートの panic（{at}）"),
                }))
            }
        }
    }
}

/// ファズで読むときの上限（小さく絞って、1 回を速くする）。
pub fn fuzz_limits() -> crate::Limits {
    crate::Limits {
        max_dim: 4_096,
        max_pixels: 1 << 20,
        max_frames: 64,
        max_value_bytes: 64 << 20,
        max_audio_frames: 1 << 20,
        max_meta_bytes: 1 << 20,
        ..crate::Limits::default()
    }
}

/// ファズの対象（`fuzz/` の cargo-fuzz と `tests/fuzz_smoke.rs` が使う）：`data` を `format` の各デコーダ（旧版を含む）で、
/// 上限を絞って形と中身を読み、読めた値に受け付けるすべての解析を当てる（`fuzz_analyses`）。見つけたい不具合（自前のコードの
/// panic・形と中身の食い違い・`Internal`）は `Err`、壊れたファイルとしてのエラーと第三者のクレートの panic は `Ok`。
pub fn fuzz_decode(format: &str, data: &[u8]) -> Result<(), String> {
    use std::io::Cursor;
    let limits = fuzz_limits();
    let set = crate::codec::CodecSet::pinned();
    let opts = crate::doc::DecodeOptions::default();
    for d in set.all_decoders_for(format) {
        let r = catch_own_panic(|| {
            let s = d.shape(&mut Cursor::new(data), format, &opts, &limits)?;
            limits.check(&s).map_err(EngineError::Limit)?;
            let v = d.decode(&mut Cursor::new(data), format, &opts, &limits, &NoMonitor)?;
            if !s.admits(&v.shape()) {
                return Err(EngineError::Internal(format!(
                    "{}: 形と中身が違う（{s:?} と {:?}）",
                    d.id(),
                    v.shape()
                )));
            }
            v.digest()?;
            fuzz_analyses(&v)
        });
        match r {
            Err(at) => return Err(format!("{}: 自前のコードの panic（{at}）", d.id())),
            Ok(Err(EngineError::Internal(m))) => return Err(format!("{}: {m}", d.id())),
            Ok(_) => {}
        }
    }
    Ok(())
}

/// ファズとテストの SVG の文字のフォント：字の少ない小さなフォント 2 つ（`KuraTest` は A・B・C・あ、`KuraFallback` は D・E・ب。
/// 書体にない字は代わりのフォントを探す）。集まりはプロセスで 1 度だけ作る。
pub fn fuzz_svg_fonts() -> crate::codec::SvgFonts {
    static FONTS: std::sync::OnceLock<crate::codec::SvgFonts> = std::sync::OnceLock::new();
    FONTS
        .get_or_init(|| {
            let font = |name: &str, family: &str, chars: &[char]| crate::codec::FontSource {
                name: name.into(),
                data: std::sync::Arc::new(crate::testfiles::tiny_ttf(family, chars)),
            };
            crate::codec::SvgFonts::new(
                vec![
                    font("a.ttf", "KuraTest", &['A', 'B', 'C', 'あ']),
                    font("b.ttf", "KuraFallback", &['D', 'E', 'ب']),
                ],
                crate::codec::GenericFamilies::default(),
            )
        })
        .clone()
}

/// ファズの対象（`fuzz/` の `decode_svg_text` と `tests/fuzz_smoke.rs` の `svg_text`）：文字を含む SVG を、小さなフォント 2 つ
/// （[`fuzz_svg_fonts`]）を渡した SVG のデコーダで、上限を絞って形と中身を読む。`fuzz_decode` の svg はフォントを渡さないので、
/// 文字のレイアウト・代わりのフォント・レイアウトの手間の見積もり（M11 の検証 S1）の道を通らない。見つけたい不具合（自前の
/// コードの panic・形と中身の食い違い・`Internal`）は `Err`、壊れたファイルとしてのエラーと第三者のクレートの panic は `Ok`。
pub fn fuzz_svg_text(data: &[u8]) -> Result<(), String> {
    use crate::codec::Decoder;
    use std::io::Cursor;
    let limits = fuzz_limits();
    let d = crate::codec::svg::SvgCodec::with_svg_fonts(fuzz_svg_fonts());
    let opts = crate::doc::DecodeOptions::default();
    let r = catch_own_panic(|| {
        let s = d.shape(&mut Cursor::new(data), "svg", &opts, &limits)?;
        limits.check(&s).map_err(EngineError::Limit)?;
        let v = d.decode(&mut Cursor::new(data), "svg", &opts, &limits, &NoMonitor)?;
        if !s.admits(&v.shape()) {
            return Err(EngineError::Internal(format!(
                "{}: 形と中身が違う（{s:?} と {:?}）",
                d.id(),
                v.shape()
            )));
        }
        v.digest().map(|_| ())
    });
    match r {
        Err(at) => Err(format!("{}: 自前のコードの panic（{at}）", d.id())),
        Ok(Err(EngineError::Internal(m))) => Err(m),
        Ok(_) => Ok(()),
    }
}

/// ファズの対象（`fuzz/` の `decode_peaks` と `tests/fuzz_smoke.rs` の `peaks`）：波形の段のバイナリ（`kura-peaks`。
/// kura-core がキャッシュから読み直すときのパーサー）を読む。読めたなら正規の形（書き直すと同じバイト列）であること。
/// 見つけたい不具合（自前のコードの panic・往復しない・`Internal`）は `Err`、壊れたバイト列としてのエラーは `Ok`。
pub fn fuzz_peaks(data: &[u8]) -> Result<(), String> {
    use crate::analysis::AnalysisOutput;
    use crate::analysis::audio::peaks::Peaks;
    let r = catch_own_panic(|| match Peaks::from_canonical(data) {
        Ok(p) => {
            if p.to_canonical()? == data {
                Ok(())
            } else {
                Err(EngineError::Internal(
                    "kura-peaks: 読めたのに正規の形に戻らない".into(),
                ))
            }
        }
        Err(e @ EngineError::Internal(_)) => Err(e),
        Err(_) => Ok(()),
    });
    match r {
        Err(at) => Err(format!("kura-peaks: 自前のコードの panic（{at}）")),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(())) => Ok(()),
    }
}

/// ファズの対象（`fuzz/` の `model_glb` と `tests/fuzz_smoke.rs` の `model_glb`）：glb の情報を読み、焼き込み、2 つの向きで
/// 小さく描く（上限は `ModelLimits::fuzz`）。見つけたい不具合（自前のコードの panic・`Internal`・情報と焼き込みの食い違い・
/// 大きさの食い違い）は `Err`、壊れた glb としてのエラー・上限は `Ok`。
pub fn fuzz_model(data: &[u8]) -> Result<(), String> {
    use crate::model::{RenderOptions, ViewDir};
    let limits = crate::model::ModelLimits::fuzz();
    let r = catch_own_panic(|| {
        let info = crate::model::read_info(data, &limits);
        let scene = crate::model::load_scene(data, &limits, &NoMonitor);
        match (&info, &scene) {
            (Err(EngineError::Internal(m)), _) | (_, Err(EngineError::Internal(m))) => {
                return Err(EngineError::Internal(m.clone()));
            }
            (Err(_), Ok(_)) => {
                return Err(EngineError::Internal(
                    "glb: 情報が読めないのに焼き込めた".into(),
                ));
            }
            (Ok(i), Ok(_)) if !i.unsupported.is_empty() => {
                return Err(EngineError::Internal(
                    "glb: 描けない印があるのに焼き込めた".into(),
                ));
            }
            _ => {}
        }
        let Ok(scene) = scene else {
            return Ok(());
        };
        for view in [ViewDir::ThreeQuarter, ViewDir::Front] {
            let opts = RenderOptions {
                size: 24,
                samples: 2,
                view,
            };
            match crate::model::render(&scene, &opts, &limits, &NoMonitor) {
                Ok(Value::Image(img)) => {
                    let (w, h) = (img.px.width(), img.px.height());
                    if w.max(h) > opts.size || img.px.format() != crate::value::PixelFormat::Rgba8 {
                        return Err(EngineError::Internal(format!(
                            "glb: 描いた画像の形が違う（{w}×{h}・{:?}）",
                            img.px.format()
                        )));
                    }
                }
                Ok(_) => {
                    return Err(EngineError::Internal("glb: 描いた値が画像でない".into()));
                }
                Err(e @ EngineError::Internal(_)) => return Err(e),
                Err(_) => {}
            }
        }
        Ok(())
    });
    match r {
        Err(at) => Err(format!("glb: 自前のコードの panic（{at}）")),
        Ok(Err(EngineError::Internal(m))) => Err(m),
        Ok(_) => Ok(()),
    }
}

/// ファズで、読めた値に解析を当てる手間の上限（`Estimate::cost`。1 回を速くする）。
const FUZZ_ANALYSIS_COST: u64 = 1 << 24;

/// ファズ：読めた値に、受け付けるすべての解析（今の版・旧版。既定の引数と例の引数）を上限を絞って当てる。
/// 見つけたいのは自前のコードの panic・`Internal`・見積もりを超える結果・往復しない正規の形（どれも `Internal` にする）。
/// 上限・中止などのエラーは壊れた入力としてのエラーと同じく通す。
fn fuzz_analyses(v: &Value) -> Result<(), EngineError> {
    let engine = Engine::with_limits(fuzz_limits())?;
    let shape = v.shape();
    for a in engine.analyses().all() {
        if !a.accepts().contains(&v.kind()) {
            continue;
        }
        let mut params = std::collections::BTreeSet::new();
        let mut raws = vec![a.schema().defaults()];
        raws.extend(a.examples()?.into_iter().map(|e| e.params));
        for raw in raws {
            let p = a.check_params(&raw).map_err(|i| {
                EngineError::Internal(format!("{} の例の引数が読めない: {i}", a.id()))
            })?;
            if !params.insert(crate::digest::canonical_json(&p)?) {
                continue;
            }
            let estimate = a.estimate(&p, &shape).map_err(EngineError::Invalid)?;
            if estimate.cost > FUZZ_ANALYSIS_COST {
                continue;
            }
            let req = crate::analysis::AnalysisRequest {
                id: a.id().to_string(),
                v: Some(a.version()),
                params: p,
            };
            let out = match engine.analyze_dyn(&req, v, &NoMonitor) {
                Ok(out) => out,
                Err(e @ EngineError::Internal(_)) => return Err(e),
                Err(_) => continue,
            };
            if out.output.len() as u64 > estimate.bytes {
                return Err(EngineError::Internal(format!(
                    "{}: 結果 {} バイトが見積もり {} を超えた",
                    a.id(),
                    out.output.len(),
                    estimate.bytes
                )));
            }
            if !a.round_trip(&out.output)? {
                return Err(EngineError::Internal(format!(
                    "{}: 正規の形が往復しない",
                    a.id()
                )));
            }
        }
    }
    Ok(())
}

/// 書き出しのファズの見出しの大きさ（設定を決める先頭のバイト）。並びは [`fuzz_encode`] の説明のとおり。
pub const FUZZ_ENCODE_HEADER: usize = 19;

/// 書き出しのファズで試すサンプルレート（範囲の端と外を含む。7,999・192,001 は書けない）。
const FUZZ_ENCODE_RATES: [u32; 20] = [
    8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 50_000, 64_000, 88_200,
    96_000, 176_400, 192_000, 9_999, 26_000, 49_999, 7_999, 192_001,
];

/// 書き出しのファズの長さの上限（フレーム。1 回を速くする）。
const FUZZ_ENCODE_MAX_FRAMES: u64 = 16_384;

/// 書き出しのファズのタグの数の上限（見出しに 2 バイトずつ置く）。
const FUZZ_ENCODE_MAX_TAGS: usize = 4;

/// 書き出しのファズで試すタグの名前：既知の名前、その別名（大文字・小文字・DESCRIPTION・小文字ちょうどの `cprt`）、
/// 同じ名前の大文字と小文字（読み取りが別の名前にする `CPRT` と `cprt` を含む）、ENCODER・ループの名前、書けない名前
/// （`=`・空・ASCII の外）、書かない名前（`vorbis:` でない）。新しい名前は末尾に足す（種の番号の意味を変えない）。
const FUZZ_TAG_KEYS: [&str; 30] = [
    "title",
    "artist",
    "album",
    "comment",
    "date",
    "genre",
    "copyright",
    "vorbis:TITLE",
    "vorbis:title",
    "vorbis:ARTIST",
    "vorbis:DESCRIPTION",
    "vorbis:description",
    "vorbis:COMMENT",
    "vorbis:FOO",
    "vorbis:foo",
    "vorbis:Foo",
    "vorbis:ENCODER",
    "vorbis:encoder",
    "vorbis:LOOPSTART",
    "vorbis:looplength",
    "vorbis:A=B",
    "vorbis:",
    "vorbis:KEY WITH SPACE",
    "vorbis:\u{e9}",
    "vorbis:~",
    "riff:ISBJ",
    "aiff:NAME",
    "Title",
    "vorbis:cprt",
    "vorbis:CPRT",
];

/// 書き出しのファズで試すタグの値の数（[`fuzz_tag_value`] の番号の範囲）。
const FUZZ_TAG_VALUES: usize = 13;

/// 書き出しのファズで試すタグの値：空・`=` を含む・非 ASCII・NUL・前後の空白、読み取りの上限（4,096 バイト）の前後と、
/// 切り詰めが文字の途中・空白の直後に来る長い値、とても長い値。
fn fuzz_tag_value(i: usize) -> String {
    match i % FUZZ_TAG_VALUES {
        0 => String::new(),
        1 => "x".into(),
        2 => "a=b".into(),
        3 => "=".into(),
        4 => "効果音".into(),
        5 => "LOOPSTART=1".into(),
        6 => " sp ".into(),
        7 => "\u{0}".into(),
        8 => "あ".repeat(1_400),
        9 => "é ".repeat(2_048),
        10 => "a".repeat(4_096),
        11 => "a".repeat(4_097),
        _ => "x".repeat(70_000),
    }
}

/// 書き出しのファズのメタデータ（見出しの 5〜18 バイト目）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FuzzEncodeMeta {
    /// ループ点の形：0 なし、1 は (a, b)、2 は (u64::MAX − a, b)、3 は (a, u64::MAX − b)（4 以上は 4 の余り）。
    pub loop_mode: u8,
    /// ループ点の a（見出しの 6〜7 バイト目）。
    pub loop_a: u16,
    /// ループ点の b（見出しの 8〜9 バイト目）。
    pub loop_b: u16,
    /// タグ：（名前の番号, 値の番号）。4 つまで（5 つめからは書かない）。
    pub tags: Vec<(u8, u8)>,
}

/// 書き出しのファズの入力を、見出しと標本から組む（種を作るときに使う）。`quality` は −1〜10、`rate` は
/// [`FUZZ_ENCODE_RATES`] の番号、`channels` は 1〜9、`frames` は 16,384 まで（大きいものは余りにされる）。メタデータはなし。
pub fn fuzz_encode_input(
    quality: i64,
    rate: u8,
    channels: u8,
    frames: u16,
    samples: &[f32],
) -> Vec<u8> {
    fuzz_encode_input_meta(
        quality,
        rate,
        channels,
        frames,
        &FuzzEncodeMeta::default(),
        samples,
    )
}

/// [`fuzz_encode_input`] にメタデータ（ループ点・タグ）を付けたもの。
pub fn fuzz_encode_input_meta(
    quality: i64,
    rate: u8,
    channels: u8,
    frames: u16,
    meta: &FuzzEncodeMeta,
    samples: &[f32],
) -> Vec<u8> {
    let q = u8::try_from(quality.saturating_add(1).clamp(0, 11)).unwrap_or(0);
    let mut out = vec![q, rate, channels.saturating_sub(1)];
    out.extend_from_slice(&frames.to_le_bytes());
    out.push(meta.loop_mode);
    out.extend_from_slice(&meta.loop_a.to_le_bytes());
    out.extend_from_slice(&meta.loop_b.to_le_bytes());
    let tags = &meta.tags[..meta.tags.len().min(FUZZ_ENCODE_MAX_TAGS)];
    out.push(u8::try_from(tags.len()).unwrap_or(0));
    for i in 0..FUZZ_ENCODE_MAX_TAGS {
        let (k, v) = tags.get(i).copied().unwrap_or((0, 0));
        out.extend_from_slice(&[k, v]);
    }
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// 見出しの 5〜18 バイト目から、入力の値に付けるメタデータを作る。
fn fuzz_meta_of(byte: &dyn Fn(usize) -> u8) -> crate::value::AudioMeta {
    use crate::value::{AudioMeta, LoopRange};
    let word = |i: usize| u64::from(u16::from_le_bytes([byte(i), byte(i + 1)]));
    let (a, b) = (word(6), word(8));
    let loop_range = match byte(5) % 4 {
        0 => None,
        1 => Some((a, b)),
        2 => Some((u64::MAX - a, b)),
        _ => Some((a, u64::MAX - b)),
    }
    .map(|(start, end)| LoopRange { start, end });
    let mut meta = AudioMeta {
        loop_range,
        ..AudioMeta::default()
    };
    for t in 0..usize::from(byte(10)) % (FUZZ_ENCODE_MAX_TAGS + 1) {
        let key = FUZZ_TAG_KEYS[usize::from(byte(11 + 2 * t)) % FUZZ_TAG_KEYS.len()];
        meta.tags.insert(
            key.to_string(),
            fuzz_tag_value(usize::from(byte(12 + 2 * t))),
        );
    }
    meta
}

/// `p` の `pos` から `len` バイトを取って `pos` を進める（足りなければ `Err`）。
fn take<'a>(p: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let end = pos
        .checked_add(len)
        .ok_or("コメントヘッダの長さがあふれます")?;
    let s = p
        .get(*pos..end)
        .ok_or("コメントヘッダが途中で切れています")?;
    *pos = end;
    Ok(s)
}

/// `p` の `pos` から小さいエンディアンの u32 を取る。
fn take_u32(p: &[u8], pos: &mut usize) -> Result<usize, String> {
    let b = take(p, pos, 4)?;
    usize::try_from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])).map_err(|e| e.to_string())
}

/// Ogg Vorbis のコメントヘッダ（2 つめのパケット）の項目を、書いたバイト列から読む（Vorbis I §5.2.1。ページは RFC 3533）。
/// 項目は最初の `=` で名前と値に分ける。形が違えば `Err`。
fn ogg_comment_entries(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
    // 最初の 2 つのパケット
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut cur = Vec::new();
    let mut at = 0usize;
    'pages: while at < bytes.len() && packets.len() < 2 {
        let head = bytes
            .get(at..at + 27)
            .ok_or("ページの見出しが途中で切れています")?;
        if &head[..4] != b"OggS" {
            return Err(format!("ページの印がありません（{at}）"));
        }
        let n = usize::from(head[26]);
        let lacing = bytes
            .get(at + 27..at + 27 + n)
            .ok_or("lacing が途中で切れています")?;
        let mut off = at + 27 + n;
        for &l in lacing {
            let seg = bytes
                .get(off..off + usize::from(l))
                .ok_or("ページの本体が途中で切れています")?;
            cur.extend_from_slice(seg);
            off += usize::from(l);
            if l < 255 {
                packets.push(std::mem::take(&mut cur));
                if packets.len() == 2 {
                    break 'pages;
                }
            }
        }
        at = off;
    }
    let p = packets.get(1).ok_or("コメントヘッダがありません")?;
    if p.get(..7) != Some(b"\x03vorbis".as_slice()) {
        return Err("コメントヘッダの印がありません".into());
    }
    let mut pos = 7usize;
    let vendor = take_u32(p, &mut pos)?;
    take(p, &mut pos, vendor)?;
    let count = take_u32(p, &mut pos)?;
    let mut out = Vec::new();
    for _ in 0..count {
        let len = take_u32(p, &mut pos)?;
        let entry = std::str::from_utf8(take(p, &mut pos, len)?)
            .map_err(|e| format!("コメントが UTF-8 でない: {e}"))?;
        let (k, v) = entry
            .split_once('=')
            .ok_or_else(|| format!("コメントに = がありません: {entry:?}"))?;
        out.push((k.to_string(), v.to_string()));
    }
    if take(p, &mut pos, 1)? != [1] {
        return Err("コメントヘッダの framing bit がありません".into());
    }
    Ok(out)
}

/// エラーの文に載せるコメントの並び（長い値は先頭 32 文字とバイト数だけ）。
fn show_comments(entries: &[(String, String)]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(k, v)| {
            if v.chars().count() > 32 {
                let head: String = v.chars().take(32).collect();
                format!("{k}={head:?}…（{} バイト）", v.len())
            } else {
                format!("{k}={v:?}")
            }
        })
        .collect();
    format!("[{}]", items.join(", "))
}

/// コメントの名前を、読み取りが同じものとみなす形にする：読み取りの表（`codec::symph::comment_name`）が付ける kura の
/// 名前（大文字と小文字だけの違い・DESCRIPTION と COMMENT・小文字ちょうどの `cprt` と COPYRIGHT は同じ名前になる）。
/// ループ点の名前と、読み取りがタグにしない名前は大文字にしたもの。
fn same_tag_name(name: &str) -> String {
    use crate::codec::symph::{CommentName, comment_name};
    match comment_name(name, true) {
        Some(CommentName::Tag(n)) => n,
        _ => name.to_ascii_uppercase(),
    }
}

/// 書いたコメントの、ENCODER を除いた（読み取りが同じとみなす名前, 値）を並べたもの。
fn comment_set(entries: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = entries
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("ENCODER"))
        .map(|(k, v)| (same_tag_name(k), v.clone()))
        .collect();
    out.sort();
    out
}

/// ファズの対象（`fuzz/` の cargo-fuzz と `tests/fuzz_smoke.rs` が使う）：自前のエンコーダの書き出し（今は `ogg` だけ）。
///
/// 先頭の [`FUZZ_ENCODE_HEADER`] バイトで設定とメタデータを決める（足りない見出しは 0 で埋める）：
///
/// | バイト | 意味 |
/// |---|---|
/// | 0 | 品質（12 の余り − 1：−1〜10） |
/// | 1 | レート（[`FUZZ_ENCODE_RATES`] の番号。範囲の外を含む） |
/// | 2 | チャンネル（9 の余り + 1：1〜9） |
/// | 3〜4 | 長さ（u16、16,385 の余り：0〜16,384 フレーム） |
/// | 5 | ループ点の形（4 の余り。0 なし・1 (a, b)・2 (u64::MAX − a, b)・3 (a, u64::MAX − b)） |
/// | 6〜7・8〜9 | ループ点の a・b（u16。長さの外・長さ 0・逆向きも作る） |
/// | 10 | タグの数（5 の余り：0〜4） |
/// | 11〜18 | タグ 4 つ分の（名前の番号, 値の番号）（`FUZZ_TAG_KEYS`・`fuzz_tag_value`。重複する名前・長い値・`=` を含む値） |
///
/// 残りを f32 のビット列（小さいエンディアン。NaN・∞・非正規化数・巨大な値を含む）として、インターリーブの標本を順に
/// 繰り返して埋める。`catch_own_panic` の中で書き、すべてのデコーダ（旧版を含む）で読み直して、長さ・レート・チャンネルが
/// 元と同じかを確かめる。メタデータは、書いたコメントヘッダを読んで次を確かめる：LOOPSTART・LOOPLENGTH は
/// `start < end ≤ 長さ` のときだけ正しい値で書く、読み取りが同じタグにまとめる名前（大文字と小文字だけの違い・
/// DESCRIPTION と COMMENT・小文字ちょうどの `cprt` と COPYRIGHT）が重複しない、ENCODER は `ogg-vorbis-kura-enc/1` が
/// 1 つだけ、読み直したループ点は書いたものと同じ、読み直したタグを書き直したときのコメントが書いたものと同じ（読み取りが
/// 同じタグにまとめる名前の違いを除いて）。
/// 見つけたいもの（自前のコードの panic・`Internal`・書けると答えた形を断る・書けない形を書く・自分の出力を読めない・
/// 読み直しの形の違い・メタデータの約束の破れ）は `Err`。
pub fn fuzz_encode(format: &str, data: &[u8]) -> Result<(), String> {
    use std::io::Cursor;
    use std::sync::Arc;

    use crate::codec::Encoder as _;
    use crate::codec::symph::{vorbis_comments_for, writable_loop};
    use crate::value::Audio;

    // 足りない見出しは 0 で埋めて読む
    let byte = |i: usize| data.get(i).copied().unwrap_or(0);
    let (encoder, opts) = match format {
        "ogg" => (
            crate::codec::vorbis::OggVorbisEncoder,
            serde_json::json!({ "quality": i64::from(byte(0) % 12) - 1 }),
        ),
        _ => return Err(format!("{format} の書き出しのファズはありません")),
    };
    let rate = FUZZ_ENCODE_RATES[usize::from(byte(1)) % FUZZ_ENCODE_RATES.len()];
    let channels = 1 + usize::from(byte(2) % 9);
    let frames = u64::from(u16::from_le_bytes([byte(3), byte(4)])) % (FUZZ_ENCODE_MAX_FRAMES + 1);
    let frames64 = frames;
    let frames = usize::try_from(frames).map_err(|e| e.to_string())?;
    let meta = fuzz_meta_of(&byte);
    let samples: Vec<f32> = data
        .get(FUZZ_ENCODE_HEADER..)
        .unwrap_or(&[])
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let mut planes = vec![Vec::with_capacity(frames); channels];
    for f in 0..frames {
        for (ch, p) in planes.iter_mut().enumerate() {
            let k = f * channels + ch;
            p.push(if samples.is_empty() {
                0.0
            } else {
                samples[k % samples.len()]
            });
        }
    }
    let audio = Audio::new(
        rate,
        planes.into_iter().map(Arc::new).collect(),
        meta.clone(),
    )
    .map_err(|e| format!("入力を組めません: {e}"))?;
    let value = Value::Audio(Arc::new(audio));
    let shape = value.shape();
    let writable = encoder.can_encode(format, &shape);
    let name = format!(
        "{}（{opts}・{rate} Hz・{channels} ch・{frames} フレーム・ループ {:?}・タグ {:?}）",
        encoder.id(),
        meta.loop_range,
        meta.tags.keys().collect::<Vec<_>>()
    );

    let mut bytes = Vec::new();
    let r = catch_own_panic(|| encoder.encode(&value, format, &opts, &mut bytes, &NoMonitor));
    match r {
        Err(at) => return Err(format!("{name}: 自前のコードの panic（{at}）")),
        Ok(Err(EngineError::Internal(m))) => return Err(format!("{name}: {m}")),
        Ok(Err(e)) if writable => {
            return Err(format!("{name}: 書けると答えた形を断りました: {e}"));
        }
        Ok(Err(_)) => return Ok(()),
        Ok(Ok(_)) if !writable => {
            return Err(format!("{name}: 書けないと答えた形を書きました"));
        }
        Ok(Ok(_)) => {}
    }

    // 書いたコメント：ループ点は範囲の中だけ、名前は重複しない、ENCODER は 1 つ
    let entries = ogg_comment_entries(&bytes).map_err(|e| format!("{name}: {e}"))?;
    let want_loop = writable_loop(meta.loop_range, frames64);
    let loop_entries: Vec<&(String, String)> = entries
        .iter()
        .filter(|(k, _)| matches!(k.to_ascii_uppercase().as_str(), "LOOPSTART" | "LOOPLENGTH"))
        .collect();
    let want_entries: Vec<(String, String)> = want_loop
        .map(|l| {
            vec![
                ("LOOPSTART".to_string(), l.start.to_string()),
                ("LOOPLENGTH".to_string(), (l.end - l.start).to_string()),
            ]
        })
        .unwrap_or_default();
    if loop_entries.into_iter().cloned().collect::<Vec<_>>() != want_entries {
        return Err(format!(
            "{name}: ループ点のコメントが違う（書いたもの {}、期待 {}）",
            show_comments(&entries),
            show_comments(&want_entries)
        ));
    }
    let mut seen = BTreeMap::new();
    for (k, _) in &entries {
        if seen.insert(same_tag_name(k), ()).is_some() {
            return Err(format!(
                "{name}: 同じ名前のコメントを 2 つ書いた（{k}）: {}",
                show_comments(&entries)
            ));
        }
    }
    let encoder_id = crate::codec::vorbis::ENC_ID;
    if !entries
        .iter()
        .any(|(k, v)| k == "ENCODER" && v == encoder_id)
    {
        return Err(format!(
            "{name}: ENCODER={encoder_id} がない: {}",
            show_comments(&entries)
        ));
    }

    let set = crate::codec::CodecSet::pinned();
    let limits = crate::Limits::default();
    let dopts = crate::doc::DecodeOptions::default();
    for d in set.all_decoders_for(format) {
        let r = catch_own_panic(|| {
            let s = d.shape(&mut Cursor::new(&bytes), format, &dopts, &limits)?;
            let v = d.decode(
                &mut Cursor::new(&bytes),
                format,
                &dopts,
                &limits,
                &NoMonitor,
            )?;
            Ok((s, v))
        });
        match r {
            Err(at) => {
                return Err(format!(
                    "{name} を {} で読む: 自前のコードの panic（{at}）",
                    d.id()
                ));
            }
            Ok(Err(e)) => return Err(format!("{name} を {} で読めません: {e}", d.id())),
            Ok(Ok((s, v))) => {
                let got = v.shape();
                if got != shape || !s.admits(&got) {
                    return Err(format!(
                        "{name} を {} で読み直した形が違う（ヘッダ {s:?}・中身 {got:?}）",
                        d.id()
                    ));
                }
                let Value::Audio(back) = &v else {
                    return Err(format!("{name} を {} で読むと音声でない", d.id()));
                };
                if back.meta.loop_range != want_loop {
                    return Err(format!(
                        "{name} を {} で読み直したループ点が違う（{:?}）",
                        d.id(),
                        back.meta.loop_range
                    ));
                }
                // 読み直したメタデータを書き直すときのコメント（ENCODER を除く）が、書いたものと同じ
                let again = vorbis_comments_for(&back.meta, frames64);
                if comment_set(&again) != comment_set(&entries) {
                    return Err(format!(
                        "{name} を {} で読み直したメタデータを書き直すとコメントが変わる（{} → {}）",
                        d.id(),
                        show_comments(&entries),
                        show_comments(&again)
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 追記だけの引数（`actions` の列）を持つノードを途中で切り、差分の適用（`Op::apply_appended`）が全体を当て直したものと
/// 同じかを確かめる（E21。実行器の経路の照合は `testing::snapshot` と tests/e4.rs）。違えば `Internal`。
fn check_appended(
    engine: &Engine,
    node: &crate::doc::Node,
    inputs: &[InputGen],
) -> Result<(), EngineError> {
    let Some(actions) = node.params.get("actions").and_then(|a| a.as_array()) else {
        return Ok(());
    };
    if actions.len() < 2 {
        return Ok(());
    }
    let Some(op) = engine.registry().get(&node.op, node.v) else {
        return Ok(());
    };
    let mut prev_raw = node.params.clone();
    prev_raw["actions"] = serde_json::Value::Array(actions[..actions.len() / 2].to_vec());
    let (Ok(prev), Ok(full)) = (op.parse(&prev_raw), op.parse(&node.params)) else {
        return Ok(());
    };
    let limits = fuzz_limits();
    for input in inputs {
        let value = input.generate()?;
        // 推論で手間を見積もり、大きなものは描かない
        let g = single_node_graph(&node.op, node.v, node.params.clone())?;
        let shapes = BTreeMap::from([(InputId(0), value.shape())]);
        match engine.compile(&g, &shapes) {
            Ok(plan) if plan.cost() <= 1 << 22 => {}
            _ => continue,
        }
        let mut cx = crate::op::ApplyCx::new(&NoMonitor, &limits, 0, crate::doc::RULES);
        let Ok(prev_out) = op.apply(prev.as_ref(), &[&value], &mut cx) else {
            continue;
        };
        let inc = op.apply_appended(prev.as_ref(), &prev_out.value, full.as_ref(), &mut cx);
        let whole = op.apply(full.as_ref(), &[&value], &mut cx);
        let same = match (&inc, &whole) {
            (None, _) | (Some(Err(_)), Err(_)) => true,
            (Some(Ok(a)), Ok(b)) => a.value == b.value,
            _ => false,
        };
        if !same {
            return Err(EngineError::Internal(format!(
                "{}：差分の適用が全体を当て直したものと違います（差分 {}・全体 {}）",
                node.op,
                inc.is_some_and(|r| r.is_ok()),
                whole.is_ok()
            )));
        }
    }
    Ok(())
}

/// キャッシュの道（`Engine::render_cached`）を、冷えた・温まった（当たりを描き直して照らす）・小さい上限の 3 通りで描き、
/// キャッシュなしの道（`plain`）と照らす（E4）。描けたかどうか・出力のダイジェスト・警告（順も）が同じでなければ `Internal`。
fn check_cached(
    engine: &Engine,
    plan: &crate::render::Plan,
    value: Value,
    plain: &Result<crate::render::Rendered, EngineError>,
) -> Result<(), EngineError> {
    use crate::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
    let inputs = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(value, Digest::default())?,
    )]);
    let cold = RenderOptions {
        digests: true,
        ..RenderOptions::default()
    };
    let verify = RenderOptions {
        verify_hits: true,
        ..cold
    };
    let mut big = NodeCache::new(1 << 30);
    let mut small = NodeCache::new(16 << 10);
    let runs = [
        (
            "冷えた",
            engine.render_cached(plan, &inputs, &mut big, &cold, &NoMonitor),
        ),
        (
            "温まった",
            engine.render_cached(plan, &inputs, &mut big, &verify, &NoMonitor),
        ),
        (
            "小さい上限",
            engine.render_cached(plan, &inputs, &mut small, &cold, &NoMonitor),
        ),
        (
            "小さい上限の 2 回目",
            engine.render_cached(plan, &inputs, &mut small, &verify, &NoMonitor),
        ),
    ];
    for (what, r) in runs {
        match (plain, r) {
            (_, Err(EngineError::Internal(m))) => {
                return Err(EngineError::Internal(format!(
                    "キャッシュの道（{what}）：{m}"
                )));
            }
            (Ok(p), Ok(c)) => {
                let want: Vec<Option<Digest>> = p.outputs.iter().map(|o| Some(o.digest)).collect();
                let got: Vec<Option<Digest>> = c.outputs.iter().map(|o| o.digest).collect();
                if want != got || p.warnings != c.warnings {
                    return Err(EngineError::Internal(format!(
                        "キャッシュの道（{what}）の値・警告がキャッシュなしと違います"
                    )));
                }
            }
            (Err(_), Err(_)) => {}
            (p, c) => {
                return Err(EngineError::Internal(format!(
                    "キャッシュの道（{what}）とキャッシュなしで、描けたかどうかが違います（なし {}・あり {}）",
                    p.is_ok(),
                    c.is_ok()
                )));
            }
        }
    }
    Ok(())
}

/// ファズの対象：文書の JSON を読み、形を確かめ、エンジン（上限を絞ったもの）で読み込んで、小さな入力（画像・パレットの画像・
/// フレームの列・音声）で計画を作って描く。見つけたいのは自前のコードの panic と `Internal`（推論と実際の食い違い、
/// 差分の適用と全体の食い違い、キャッシュの道とキャッシュなしの食い違い・キャッシュの当たりと描き直しの食い違い、
/// 範囲の描画と全体の食い違い・変わった範囲の推論の外の変化、プレビューの段の計画が作れない・等倍で描けるのに段で描けない・
/// 段の値が道によって違うことを含む）。
pub fn fuzz_document(data: &[u8]) -> Result<(), String> {
    use crate::geom::{IPoint, IRect};
    use crate::testgen::MetaGen;
    use crate::value::{LoopRange, PixelFormat, Slice};
    let r = catch_own_panic(|| {
        let engine = Engine::with_limits(fuzz_limits())?;
        let g = engine.load(data)?;
        let _ = g.check_structure(1_000);
        let inputs = [
            InputGen::Noise {
                w: 24,
                h: 16,
                fmt: PixelFormat::Rgba8,
                seed: 1,
            },
            InputGen::Noise {
                w: 12,
                h: 10,
                fmt: PixelFormat::Indexed8,
                seed: 2,
            },
            InputGen::NoiseFrames {
                w: 8,
                h: 6,
                fmt: PixelFormat::Rgba8,
                count: 3,
                seed: 3,
            },
            InputGen::Sine {
                rate: 44_100,
                channels: 2,
                frames: 1_000,
                freq: 440,
                loop_range: None,
            },
            // メタデータの写し方（格子・ピボット・スライス・ループ点・マーカー）にも届くように
            InputGen::WithMeta {
                base: Box::new(InputGen::Sheet {
                    cols: 3,
                    rows: 2,
                    cell: 6,
                }),
                meta: MetaGen {
                    slices: vec![Slice {
                        name: "s".into(),
                        rect: IRect {
                            x: 2,
                            y: 1,
                            w: 9,
                            h: 7,
                        },
                        center: Some(IRect {
                            x: 4,
                            y: 3,
                            w: 3,
                            h: 2,
                        }),
                        pivot: Some(IPoint { x: 5, y: 6 }),
                    }],
                    ..MetaGen::default()
                },
            },
            InputGen::WithMeta {
                base: Box::new(InputGen::Sine {
                    rate: 44_100,
                    channels: 1,
                    frames: 1_000,
                    freq: 440,
                    loop_range: Some(LoopRange {
                        start: 100,
                        end: 900,
                    }),
                }),
                meta: MetaGen {
                    markers: vec![(250, "m".into())],
                    ..MetaGen::default()
                },
            },
        ];
        for input in &inputs {
            let value = input.generate()?;
            let shapes = BTreeMap::from([(InputId(0), value.shape())]);
            let Ok(plan) = engine.compile(&g, &shapes) else {
                continue;
            };
            // 手間の大きな計画は描かない（1 回を速くする）
            if plan.cost() > 1 << 22 {
                continue;
            }
            let values = BTreeMap::from([(
                InputId(0),
                InputValue {
                    value: value.clone(),
                    source: Digest::default(),
                },
            )]);
            let plain = engine.render(&plan, &values, &NoMonitor);
            if let Err(EngineError::Internal(m)) = plain {
                return Err(EngineError::Internal(m));
            }
            // 段 10：範囲の描画と全体の描画の照合、変わった範囲の推論の保守性（種は文書のバイト列から）
            region::check_region_and_delta(&engine, &g, &value, crate::determinism::fnv1a64(data))?;
            // 段 10：プレビューの段の計画（作れるか段 0 に落ちる。等倍で描けるなら段でも描け、キャッシュの道と範囲の描画で同じ）
            let fins = BTreeMap::from([(
                InputId(0),
                crate::render::cache::FingerprintedInput::new(value.clone(), Digest::default())?,
            )]);
            preview::check_levels(
                &engine,
                &plan,
                &fins,
                plain.is_ok(),
                crate::determinism::fnv1a64(data),
            )?;
            check_cached(&engine, &plan, value, &plain)?;
        }
        for node in &g.nodes {
            check_appended(&engine, node, &inputs)?;
        }
        snapshot::check_snapshot(&engine, &g, &inputs)?;
        Ok(())
    });
    match r {
        Err(at) => Err(format!("自前のコードの panic（{at}）")),
        Ok(Err(EngineError::Internal(m))) => Err(m),
        Ok(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文書のファズは、描画のノードを途中で切って差分の適用を照らす（例の文書は通る）。
    #[test]
    fn document_fuzz_checks_appended_paint() {
        let engine = Engine::new().unwrap();
        let op = engine
            .registry()
            .all()
            .into_iter()
            .find(|o| o.id() == "paint.actions")
            .unwrap();
        let mut checked = 0;
        for ex in op.examples().unwrap() {
            if ex.params["actions"]
                .as_array()
                .is_some_and(|a| a.len() >= 2)
            {
                let g = single_node_graph("paint.actions", 1, ex.params.clone()).unwrap();
                let raw = g.to_canonical_json().unwrap();
                assert_eq!(fuzz_document(&raw), Ok(()), "{}", ex.name);
                checked += 1;
            }
        }
        assert!(checked > 0);
    }

    /// 書き出しのファズ：種の見出しは読み方と合い、書ける形・書けない形（レート・チャンネルの範囲の外）・空の入力・
    /// 標本のない入力・NaN と ∞ を含む入力のどれも見つけたものにならない。知らない形式は `Err`。
    #[test]
    fn encode_fuzz_accepts_valid_and_out_of_range_shapes() {
        let input = fuzz_encode_input(-1, 3, 9, 40_000, &[0.5]);
        assert_eq!(
            input[..FUZZ_ENCODE_HEADER],
            [
                0, 3, 8, 0x40, 0x9C, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(input[FUZZ_ENCODE_HEADER..], 0.5f32.to_le_bytes());
        assert_eq!(fuzz_encode_input(10, 0, 1, 0, &[])[0], 11);
        let meta = FuzzEncodeMeta {
            loop_mode: 2,
            loop_a: 0x1234,
            loop_b: 7,
            tags: vec![(1, 2), (3, 4), (5, 6), (7, 8), (9, 10)],
        };
        assert_eq!(
            fuzz_encode_input_meta(0, 1, 2, 3, &meta, &[]),
            [
                1, 1, 1, 3, 0, 2, 0x34, 0x12, 7, 0, 4, 1, 2, 3, 4, 5, 6, 7, 8
            ]
        );
        let odd = [0.25, f32::NAN, -f32::INFINITY, 1e30, -1e-40];
        for (q, rate, ch, frames, samples) in [
            (5, 7, 2, 3_000u16, &odd[..]),
            (10, 14, 8, 600, &odd[..]),
            (0, 0, 1, 0, &[][..]),
            (3, 12, 3, 100, &[][..]),
            // 9 ch・7,999 Hz・192,001 Hz は書けない（断ればよい）
            (5, 7, 9, 100, &odd[..]),
            (5, 18, 1, 100, &odd[..]),
            (5, 19, 2, 100, &odd[..]),
        ] {
            let data = fuzz_encode_input(q, rate, ch, frames, samples);
            assert_eq!(
                fuzz_encode("ogg", &data),
                Ok(()),
                "{q} {rate} {ch} {frames}"
            );
        }
        // 見出しが足りない入力も、0 で埋めて読む
        assert_eq!(fuzz_encode("ogg", &[]), Ok(()));
        assert_eq!(fuzz_encode("ogg", &[7, 7]), Ok(()));
        assert!(fuzz_encode("png", &[0; 8]).is_err());
    }

    /// 書き出しのファズのメタデータ：見出しの読み方（ループ点の 4 つの形・タグの数と番号）と、範囲の中・外のループ点、
    /// 長さ 0・逆向き・u64 の端のループ点、重複する名前・長い値・`=` を含む値のタグのどれも見つけたものにならない
    /// （書いたコメントのループ点・重複・ENCODER、読み直したループ点、書き直したときのコメントを `fuzz_encode` が照らす）。
    #[test]
    fn encode_fuzz_checks_metadata() {
        use crate::value::LoopRange;
        let meta_of = |data: &[u8]| fuzz_meta_of(&|i: usize| data.get(i).copied().unwrap_or(0));
        let mk = |loop_mode, loop_a, loop_b, tags: Vec<(u8, u8)>| FuzzEncodeMeta {
            loop_mode,
            loop_a,
            loop_b,
            tags,
        };
        let data = fuzz_encode_input_meta(0, 0, 1, 100, &mk(1, 10, 20, vec![]), &[]);
        assert_eq!(
            meta_of(&data).loop_range,
            Some(LoopRange { start: 10, end: 20 })
        );
        let data = fuzz_encode_input_meta(0, 0, 1, 100, &mk(2, 10, 20, vec![]), &[]);
        assert_eq!(
            meta_of(&data).loop_range,
            Some(LoopRange {
                start: u64::MAX - 10,
                end: 20
            })
        );
        let data = fuzz_encode_input_meta(0, 0, 1, 100, &mk(7, 10, 20, vec![]), &[]);
        assert_eq!(
            meta_of(&data).loop_range,
            Some(LoopRange {
                start: 10,
                end: u64::MAX - 20
            })
        );
        let data = fuzz_encode_input_meta(0, 0, 1, 100, &mk(4, 10, 20, vec![(0, 1), (13, 2)]), &[]);
        let m = meta_of(&data);
        assert_eq!(m.loop_range, None);
        assert_eq!(
            m.tags.into_iter().collect::<Vec<_>>(),
            vec![
                ("title".to_string(), "x".to_string()),
                ("vorbis:FOO".to_string(), "a=b".to_string())
            ]
        );
        let sine: Vec<f32> = (0..64)
            .map(|i| (0.5 * crate::math::sin(f64::from(i) * 0.3)) as f32)
            .collect();
        let dup = vec![(1, 4), (9, 1), (13, 2), (14, 3)];
        let long = vec![(0, 8), (3, 9), (10, 12), (11, 11)];
        let odd_keys = vec![(16, 1), (18, 1), (20, 1), (21, 7)];
        let aliases = vec![(7, 6), (8, 5), (15, 0), (17, 10)];
        // copyright と、読み取りが copyright にする小文字ちょうどの cprt（と、別の名前にする CPRT）
        let cprt = vec![(6, 1), (28, 2)];
        let cprt_upper = vec![(6, 1), (29, 3), (28, 2)];
        for (frames, meta) in [
            // 範囲の中
            (3_000u16, mk(1, 100, 2_000, dup.clone())),
            (3_000, mk(1, 0, 3_000, long.clone())),
            (3_000, mk(1, 2_999, 3_000, vec![])),
            // 範囲の外・長さ 0・逆向き・u64 の端
            (3_000, mk(1, 3_000, 3_005, odd_keys.clone())),
            (3_000, mk(1, 100, 3_100, aliases.clone())),
            (3_000, mk(1, 5, 5, dup)),
            (3_000, mk(1, 20, 10, long)),
            (3_000, mk(2, 0, 0, odd_keys)),
            (3_000, mk(3, 0, 0, aliases)),
            (3_000, mk(0, 0, 0, cprt)),
            (3_000, mk(0, 0, 0, cprt_upper)),
            (3_000, mk(0, 0, 0, vec![(28, 1)])),
            (3_000, mk(0, 0, 0, vec![(29, 1), (28, 2)])),
            (0, mk(1, 0, 1, vec![(2, 12)])),
        ] {
            for (q, rate, ch) in [(3, 7, 2), (-1, 0, 1), (10, 12, 3)] {
                let data = fuzz_encode_input_meta(q, rate, ch, frames, &meta, &sine);
                assert_eq!(
                    fuzz_encode("ogg", &data),
                    Ok(()),
                    "{q} {rate} {ch} {frames} {meta:?}"
                );
            }
        }
    }

    /// 並列の作業スレッドで起きた自前の panic も、自前の panic として見分ける（stable のファズが見逃さないように）。
    #[test]
    #[allow(clippy::panic)]
    fn own_panics_in_worker_threads_are_detected() {
        let r = catch_own_panic(|| {
            crate::determinism::par_blocks(64, &NoMonitor, |i| {
                if i == 37 {
                    panic!("わざと");
                }
                Ok(i)
            })
        });
        assert!(r.is_err(), "{r:?}");
        // 何も起きなければそのまま
        assert!(matches!(catch_own_panic(|| Ok(1)), Ok(Ok(1))));
    }
}
