//! テスト・ゴールデン・devtools で使う補助：操作 1 つの文書を作って描く。

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

/// テスト用：`f` を呼ぶ。panic したら場所を調べ、kura-engine の自前のコード（`crates/kura-engine/src`）の panic なら
/// `Err(場所)`、第三者のクレートの panic なら `Ok(Err(Decode))` にする（本番の `codec::contain` と同じく受け止める）。
/// 壊れた入力のテストで、自前のコードの panic を見逃さないために使う。
pub fn catch_own_panic<T>(
    f: impl FnOnce() -> Result<T, EngineError>,
) -> Result<Result<T, EngineError>, String> {
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if IN_CATCH.with(std::cell::Cell::get) {
                let at = info
                    .location()
                    .map(|l| format!("{}:{}", l.file(), l.line()));
                PANIC_AT.with(|p| *p.borrow_mut() = at);
            } else {
                previous(info);
            }
        }));
    });
    IN_CATCH.with(|c| c.set(true));
    PANIC_AT.with(|p| *p.borrow_mut() = None);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    IN_CATCH.with(|c| c.set(false));
    match r {
        Ok(v) => Ok(v),
        Err(_) => {
            let at = PANIC_AT.with(|p| p.borrow_mut().take()).unwrap_or_default();
            if at.contains("kura-engine/src") || at.starts_with("src/") {
                Err(at)
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
/// 上限を絞って形と中身を読む。見つけたい不具合（自前のコードの panic・形と中身の食い違い・`Internal`）は `Err`、
/// 壊れたファイルとしてのエラーと第三者のクレートの panic は `Ok`。
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
            Ok(())
        });
        match r {
            Err(at) => return Err(format!("{}: 自前のコードの panic（{at}）", d.id())),
            Ok(Err(EngineError::Internal(m))) => return Err(format!("{}: {m}", d.id())),
            Ok(_) => {}
        }
    }
    Ok(())
}

/// ファズの対象：文書の JSON を読み、形を確かめ、エンジンで読み込んで計画を作る（入力は小さな画像と音声の形）。
pub fn fuzz_document(data: &[u8]) -> Result<(), String> {
    let r = catch_own_panic(|| {
        let engine = Engine::new()?;
        let g = engine.load(data)?;
        let _ = g.check_structure(1_000);
        for shape in [
            crate::shape::Shape::Image {
                w: crate::shape::Dim::Exact(64),
                h: crate::shape::Dim::Exact(48),
                fmt: crate::value::PixelFormat::Rgba8,
            },
            crate::shape::Shape::Audio {
                rate: 44_100,
                channels: 2,
                frames: crate::shape::Dim::Exact(1_000),
            },
        ] {
            let _ = engine.compile(&g, &BTreeMap::from([(InputId(0), shape)]));
        }
        Ok(())
    });
    match r {
        Err(at) => Err(format!("自前のコードの panic（{at}）")),
        Ok(Err(EngineError::Internal(m))) => Err(m),
        Ok(_) => Ok(()),
    }
}
