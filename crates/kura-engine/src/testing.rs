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

/// 追記だけの引数（`actions` の列）を持つノードを途中で切り、差分の適用（`Op::apply_appended`）が全体を当て直したものと
/// 同じかを確かめる（E21。実行器が差分を使うのは段 10 からなので、ファズではここで照らす）。違えば `Internal`。
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
        let mut cx = crate::op::ApplyCx {
            mon: &NoMonitor,
            limits: &limits,
            seed: 0,
            rules: crate::doc::RULES,
            warnings: Vec::new(),
        };
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

/// ファズの対象：文書の JSON を読み、形を確かめ、エンジン（上限を絞ったもの）で読み込んで、小さな入力（画像・パレットの画像・
/// フレームの列・音声）で計画を作って描く。見つけたいのは自前のコードの panic と `Internal`（推論と実際の食い違い、
/// 差分の適用と全体の食い違いを含む）。
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
                    value,
                    source: Digest::default(),
                },
            )]);
            if let Err(EngineError::Internal(m)) = engine.render(&plan, &values, &NoMonitor) {
                return Err(EngineError::Internal(m));
            }
        }
        for node in &g.nodes {
            check_appended(&engine, node, &inputs)?;
        }
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
