//! レシピ（docs/editing.md §4.3・§17 の E48・E49・E50。M14 段 1）。
//!
//! - E48：`.kurarecipe` の読み書き（往復・正規でない JSON・`requires` の照らし・段ごとの誤り・壊れた入力・読み書きの上限の対称）。
//! - E49：継ぎ足し（`doc::compose`）の出力の継ぎ方・ランダムな文書での構造・値・結合則・空のレシピ。
//! - E50：結び付け（`Engine::instantiate`）の値の作り方・誤り・純粋さ・エディタの経路との一致。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::codec::CodecSet;
use kura_engine::doc::{
    self, DecodeOptions, Dither, Expand, FormatChoice, Graph, InputId, InputRole, InputSlot,
    NodeId, OutputId, PortRef, QUANTIZE_VERSION, Quantize, compose,
};
use kura_engine::ops::util::GridParam;
use kura_engine::recipe::{
    BindError, Binding, Facts, MAX_BINDINGS, MAX_RECIPE_BYTES, PivotFact, RangePart, Recipe,
    RecipeKind, Requirement, SettingKey, Source, TimeAnchor,
};
use kura_engine::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use kura_engine::render::{Engine, InputValue};
use kura_engine::session::{CommitMode, Edit, EditSession, HistoryLimits};
use kura_engine::shape::{Dim, Shape};
use kura_engine::testgen::InputGen;
use kura_engine::testing::catch_own_panic;
use kura_engine::testing::region::{below, random_chain, random_input};
use kura_engine::value::{PixelFormat, Value};
use kura_engine::{Digest, EngineError, IssueCode, Limits, NoMonitor, determinism};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::{Value as J, json};

// ---------------------------------------------------------------------------------------------
// 補助
// ---------------------------------------------------------------------------------------------

fn chain(steps: &[(&str, J)]) -> Graph {
    let mut g = Graph::new_single();
    for (op, params) in steps {
        g.push_linear(op, 1, params.clone(), 0).unwrap();
    }
    g
}

fn bind(node: u32, param: &str, part: Option<RangePart>, from: Source) -> Binding {
    Binding {
        node: NodeId(node),
        param: param.into(),
        part,
        from,
    }
}

fn setting(key: SettingKey) -> Source {
    Source::Setting { key }
}

fn time(ms: u64, anchor: TimeAnchor) -> Source {
    Source::Time { ms, anchor }
}

fn grid_json(cell: u32) -> J {
    json!({ "cellW": cell, "cellH": cell, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 })
}

/// §1.2 の例から、B の操作（image.pad）を除いたもの：素材の格子で切ってコマの時間を付け、並べ直す。
fn sheet_recipe() -> Recipe {
    let g = chain(&[
        (
            "sprite.split_grid",
            json!({ "grid": grid_json(16), "durationMs": 100, "skipEmpty": false }),
        ),
        (
            "sprite.pack_grid",
            json!({ "columns": 2, "gapX": 0, "gapY": 0, "fill": null }),
        ),
    ]);
    Recipe::new(
        "コマを並べ直す".into(),
        "素材の格子で切って、2 列に並べ直す".into(),
        RecipeKind::Each,
        g,
        vec![
            bind(1, "grid", None, setting(SettingKey::Grid)),
            bind(1, "durationMs", None, setting(SettingKey::FrameMs)),
        ],
    )
    .unwrap()
}

/// 秒のレシピ：頭 100 ms と終わり 250 ms を切り、50 ms と 80 ms のフェード、全体をループ。
fn audio_recipe() -> Recipe {
    let g = chain(&[
        ("audio.trim", json!({ "start": 0, "end": null })),
        (
            "audio.fade",
            json!({ "fadeIn": 0, "fadeOut": 0, "curve": "linear" }),
        ),
        (
            "audio.set_loop",
            json!({ "loop": { "start": 0, "end": 1 } }),
        ),
    ]);
    Recipe::new(
        "頭と終わりを切ってループ".into(),
        String::new(),
        RecipeKind::Each,
        g,
        vec![
            bind(3, "loop", Some(RangePart::End), time(0, TimeAnchor::End)),
            bind(1, "start", None, time(100, TimeAnchor::Start)),
            bind(2, "fadeOut", None, time(80, TimeAnchor::Start)),
            bind(1, "end", None, time(250, TimeAnchor::End)),
            bind(2, "fadeIn", None, time(50, TimeAnchor::Start)),
            bind(
                3,
                "loop",
                Some(RangePart::Start),
                time(0, TimeAnchor::Start),
            ),
        ],
    )
    .unwrap()
}

/// N → 1 の形だけのレシピ（段 6 の中身。段 1 は形の確かめだけ）。
fn merge_recipe() -> Recipe {
    let mut g = chain(&[(
        "sprite.pack_grid",
        json!({ "columns": 4, "gapX": 1, "gapY": 1, "fill": null }),
    )]);
    g.inputs[0].role = InputRole::Selection;
    Recipe::new(
        "シートにまとめる".into(),
        "選んだ画像を 4 列に".into(),
        RecipeKind::Merge,
        g,
        Vec::new(),
    )
    .unwrap()
}

/// 試聴のループ（素材の設定）を結ぶレシピ。
fn loop_recipe() -> Recipe {
    let g = chain(&[
        ("audio.gain", json!({ "db": -3.0 })),
        ("audio.set_loop", json!({ "loop": null })),
    ]);
    Recipe::new(
        "ループを入れる".into(),
        String::new(),
        RecipeKind::Each,
        g,
        vec![bind(2, "loop", None, setting(SettingKey::Loop))],
    )
    .unwrap()
}

/// 格子を付け直すレシピ（meta.set_grid の結び付け。前の操作は座標を変えない）。
fn set_grid_recipe() -> Recipe {
    let g = chain(&[
        ("paint.actions", json!({ "actions": [] })),
        ("meta.set_grid", json!({ "grid": null })),
    ]);
    Recipe::new(
        "格子を付け直す".into(),
        String::new(),
        RecipeKind::Each,
        g,
        vec![bind(2, "grid", None, setting(SettingKey::Grid))],
    )
    .unwrap()
}

fn shapes(v: &Value) -> BTreeMap<InputId, Shape> {
    BTreeMap::from([(InputId(0), v.shape())])
}

fn one_shape(s: Shape) -> BTreeMap<InputId, Shape> {
    BTreeMap::from([(InputId(0), s)])
}

fn render(engine: &Engine, g: &Graph, input: &Value, source: Digest) -> Result<Value, EngineError> {
    let plan = engine
        .compile(g, &shapes(input))
        .map_err(EngineError::Invalid)?;
    let mut out = engine.render(
        &plan,
        &BTreeMap::from([(
            InputId(0),
            InputValue {
                value: input.clone(),
                source,
            },
        )]),
        &NoMonitor,
    )?;
    Ok(out.outputs.swap_remove(0).value)
}

fn file_value(r: &Recipe) -> J {
    serde_json::from_slice(&r.to_file().unwrap()).unwrap()
}

fn bytes(v: &J) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

/// 誤りの種類（`Limit` か、指摘のコード）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Limit,
    Code(IssueCode),
}

fn codes(e: &EngineError) -> Vec<Want> {
    match e {
        EngineError::Limit(_) => vec![Want::Limit],
        EngineError::Invalid(issues) => issues.0.iter().map(|i| Want::Code(i.code)).collect(),
        _ => Vec::new(),
    }
}

/// ピボットを素材の設定にするレシピ（`meta.set_pivot` の結び付け。前の操作は座標を変えない）。
fn pivot_recipe() -> Recipe {
    let g = chain(&[
        ("paint.actions", json!({ "actions": [] })),
        ("meta.set_pivot", json!({ "pivot": null })),
    ]);
    Recipe::new(
        "ピボットを素材の設定に".into(),
        String::new(),
        RecipeKind::Each,
        g,
        vec![bind(2, "pivot", None, setting(SettingKey::Pivot))],
    )
    .unwrap()
}

// ---------------------------------------------------------------------------------------------
// E48：ファイル
// ---------------------------------------------------------------------------------------------

/// 書いて読むと同じ（レシピが等しく、書き直したバイト列も同じ）。正規の形はキーを並べ、空白なし、BOM なし。
#[test]
fn recipe_files_round_trip() {
    let engine = Engine::new().unwrap();
    for r in [
        sheet_recipe(),
        audio_recipe(),
        merge_recipe(),
        loop_recipe(),
        set_grid_recipe(),
        pivot_recipe(),
    ] {
        let file = r.to_file().unwrap();
        let text = String::from_utf8(file.clone()).unwrap();
        assert!(text.starts_with(r#"{"bindings":"#), "{text}");
        assert!(text.contains(r#""kura":"recipe""#) && text.contains(r#""schema":1"#));
        assert!(!text.contains('\n') && !text.contains(": "));
        let loaded = engine.load_recipe(&file).unwrap();
        assert_eq!(loaded.recipe, r);
        assert!(loaded.notes.is_empty());
        assert_eq!(loaded.recipe.to_file().unwrap(), file);
        assert_eq!(loaded.recipe.digest().unwrap(), r.digest().unwrap());
        // requires は文書から求めたもの（op のバイト列の順・v の順、重なりなし）
        let v: J = serde_json::from_slice(&file).unwrap();
        let req: Vec<Requirement> = serde_json::from_value(v["requires"].clone()).unwrap();
        assert_eq!(req, r.requires());
        assert!(req.windows(2).all(|w| w[0] < w[1]));
    }
    // 結び付けは (node, param, part) の順に並ぶ（part なしが先）
    let a = audio_recipe();
    let keys: Vec<(u32, &str, Option<RangePart>)> = a
        .bindings
        .iter()
        .map(|b| (b.node.0, b.param.as_str(), b.part))
        .collect();
    assert_eq!(
        keys,
        [
            (1, "end", None),
            (1, "start", None),
            (2, "fadeIn", None),
            (2, "fadeOut", None),
            (3, "loop", Some(RangePart::Start)),
            (3, "loop", Some(RangePart::End)),
        ]
    );
    // 名前が違えばダイジェストも違う
    let mut other = sheet_recipe();
    other.name.push('2');
    assert_ne!(other.digest().unwrap(), sheet_recipe().digest().unwrap());
    // §1.2 の形：結び付けの JSON
    let v = file_value(&sheet_recipe());
    assert_eq!(
        v["bindings"],
        json!([
            { "node": 1, "param": "durationMs", "from": { "type": "setting", "key": "frameMs" } },
            { "node": 1, "param": "grid", "from": { "type": "setting", "key": "grid" } },
        ])
    );
    assert_eq!(
        file_value(&audio_recipe())["bindings"][4],
        json!({ "node": 3, "param": "loop", "part": "start", "from": { "type": "time", "ms": 0, "anchor": "start" } })
    );
}

/// 正規でない JSON（整えたもの・キーの順の違い・BOM つき・並べていない結び付け・重なった requires・省いた項目）も読め、
/// 書くと正規の形になる。
#[test]
fn loose_files_are_read_and_written_canonically() {
    let engine = Engine::new().unwrap();
    for r in [sheet_recipe(), audio_recipe(), merge_recipe()] {
        let canonical = r.to_file().unwrap();
        let v: J = serde_json::from_slice(&canonical).unwrap();
        let mut variants: Vec<Vec<u8>> = Vec::new();
        variants.push(serde_json::to_vec_pretty(&v).unwrap());
        let mut bom = b"\xEF\xBB\xBF".to_vec();
        bom.extend_from_slice(&canonical);
        variants.push(bom);
        // キーの順を逆に書く
        let map = v.as_object().unwrap();
        let parts: Vec<String> = map
            .iter()
            .rev()
            .map(|(k, x)| format!("{}: {}", serde_json::to_string(k).unwrap(), x))
            .collect();
        variants.push(format!("{{\n  {}\n}}", parts.join(",\n  ")).into_bytes());
        // 結び付けを逆に並べ、requires を重ねて逆に並べる
        let mut w = v.clone();
        if let Some(b) = w["bindings"].as_array_mut() {
            b.reverse();
        }
        if let Some(q) = w["requires"].as_array_mut() {
            let copy = q.clone();
            q.extend(copy);
            q.reverse();
        }
        variants.push(bytes(&w));
        // 空の説明と結び付けは省ける
        let mut w = v.clone();
        if w["description"] == json!("") {
            w.as_object_mut().unwrap().remove("description");
        }
        if w["bindings"] == json!([]) {
            w.as_object_mut().unwrap().remove("bindings");
        }
        variants.push(bytes(&w));
        // 数を 16.0 と書く・seed を数で書く
        let text = String::from_utf8(canonical.clone()).unwrap();
        variants.push(text.replace(r#""seed":"0""#, r#""seed":0"#).into_bytes());
        for (i, raw) in variants.iter().enumerate() {
            let loaded = engine
                .load_recipe(raw)
                .unwrap_or_else(|e| panic!("{} の変形 {i}: {e}", r.name));
            assert_eq!(loaded.recipe, r, "{i}");
            assert_eq!(loaded.recipe.to_file().unwrap(), canonical, "{i}");
        }
    }
}

/// requires は文書から求め直して照らす（少なく・多く・版を違えて書いたものは断る）。
#[test]
fn requires_must_match_the_document() {
    let engine = Engine::new().unwrap();
    let v = file_value(&sheet_recipe());
    let mut fewer = v.clone();
    fewer["requires"].as_array_mut().unwrap().pop();
    let mut more = v.clone();
    more["requires"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "op": "image.flip", "v": 1 }));
    let mut wrong = v.clone();
    wrong["requires"][0]["v"] = json!(2);
    let mut extra_field = v.clone();
    extra_field["requires"][0]["note"] = json!("x");
    let mut missing = v.clone();
    missing.as_object_mut().unwrap().remove("requires");
    for bad in [fewer, more, wrong, extra_field, missing] {
        let e = engine.load_recipe(&bytes(&bad)).unwrap_err();
        assert_eq!(codes(&e), [Want::Code(IssueCode::BadGraph)], "{e}");
    }
}

/// 確かめの各段の誤りが、それぞれ決めた誤り（コード）になる（docs/editing.md §4.3 の表）。
#[test]
fn each_stage_refuses_with_its_code() {
    use IssueCode::*;
    let engine = Engine::new().unwrap();
    let sheet = file_value(&sheet_recipe());
    let audio = file_value(&audio_recipe());
    let merge = file_value(&merge_recipe());
    let edit = |base: &J, f: &dyn Fn(&mut J)| {
        let mut v = base.clone();
        f(&mut v);
        bytes(&v)
    };
    let mut cases: Vec<(&str, Vec<u8>, Want)> = vec![
        ("JSON でない", b"{".to_vec(), Want::Code(BadGraph)),
        ("配列", b"[]".to_vec(), Want::Code(BadGraph)),
        (
            "Kura のレシピでない",
            edit(&sheet, &|v| v["kura"] = json!("doc")),
            Want::Code(BadGraph),
        ),
        (
            "kura がない",
            edit(&sheet, &|v| {
                v.as_object_mut().unwrap().remove("kura");
            }),
            Want::Code(BadGraph),
        ),
        (
            "新しすぎる schema",
            edit(&sheet, &|v| v["schema"] = json!(2)),
            Want::Code(SchemaTooNew),
        ),
        (
            "新しすぎる schema は壊れているより先",
            edit(&sheet, &|v| {
                v["schema"] = json!(99);
                v["kind"] = json!("zip");
                v.as_object_mut().unwrap().remove("graph");
            }),
            Want::Code(SchemaTooNew),
        ),
        (
            "schema 0",
            edit(&sheet, &|v| v["schema"] = json!(0)),
            Want::Code(BadGraph),
        ),
        (
            "schema が文字",
            edit(&sheet, &|v| v["schema"] = json!("1")),
            Want::Code(BadGraph),
        ),
        (
            "知らない種類",
            edit(&sheet, &|v| v["kind"] = json!("zip")),
            Want::Code(BadGraph),
        ),
        (
            "知らない項目",
            edit(&sheet, &|v| v["author"] = json!("x")),
            Want::Code(BadGraph),
        ),
        (
            "名前がない",
            edit(&sheet, &|v| {
                v.as_object_mut().unwrap().remove("name");
            }),
            Want::Code(BadGraph),
        ),
        (
            "知らない操作",
            edit(&sheet, &|v| {
                v["graph"]["nodes"][1]["op"] = json!("image.future");
                v["requires"][0]["op"] = json!("image.future");
            }),
            Want::Code(UnknownOp),
        ),
        (
            "知らない操作は requires の食い違いより先",
            edit(&sheet, &|v| {
                v["graph"]["nodes"][1]["op"] = json!("image.future")
            }),
            Want::Code(UnknownOp),
        ),
        (
            "新しすぎる版",
            edit(&sheet, &|v| {
                v["graph"]["nodes"][0]["v"] = json!(9);
                v["requires"][1]["v"] = json!(9);
            }),
            Want::Code(UnknownVersion),
        ),
        (
            "移せない版",
            edit(&sheet, &|v| {
                v["graph"]["nodes"][0]["v"] = json!(0);
                v["requires"][1]["v"] = json!(0);
            }),
            Want::Code(UnknownVersion),
        ),
        (
            "引数の型",
            edit(&sheet, &|v| {
                v["graph"]["nodes"][0]["params"]["skipEmpty"] = json!("no")
            }),
            Want::Code(BadParams),
        ),
        (
            "引数の知らない項目",
            edit(&sheet, &|v| {
                v["graph"]["nodes"][1]["params"]["rows"] = json!(2)
            }),
            Want::Code(BadParams),
        ),
        (
            "引数の範囲",
            edit(&sheet, &|v| {
                v["graph"]["nodes"][0]["params"]["durationMs"] = json!(0)
            }),
            Want::Code(OutOfRange),
        ),
        (
            "固定の素材",
            edit(&sheet, &|v| {
                v["graph"]["inputs"][0]["role"] =
                    json!({ "type": "fixed", "hash": Digest([7; 32]) })
            }),
            Want::Code(BadInput),
        ),
        (
            "1 件ずつに Selection",
            edit(&sheet, &|v| {
                v["graph"]["inputs"][0]["role"] = json!({ "type": "selection" })
            }),
            Want::Code(BadInput),
        ),
        (
            "N → 1 に Primary",
            edit(&merge, &|v| {
                v["graph"]["inputs"][0]["role"] = json!({ "type": "primary" })
            }),
            Want::Code(BadInput),
        ),
        (
            "入力 2 つ",
            edit(&sheet, &|v| {
                v["graph"]["inputs"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({ "id": 1, "role": { "type": "primary" } }))
            }),
            Want::Code(BadInput),
        ),
        (
            "出力 2 つ",
            edit(&sheet, &|v| {
                let first = v["graph"]["outputs"][0].clone();
                let mut second = first.clone();
                second["id"] = json!(1);
                v["graph"]["outputs"] = json!([first, second]);
            }),
            Want::Code(BadGraph),
        ),
        (
            "フレームごとのファイル",
            edit(&sheet, &|v| {
                v["graph"]["outputs"][0]["expand"] =
                    json!({ "type": "perElement", "name": "{stem}_{index}" })
            }),
            Want::Code(BadGraph),
        ),
        (
            "Keep に量子化",
            edit(&audio, &|v| {
                v["graph"]["outputs"][0]["quantize"] =
                    json!({ "v": 1, "bits": 16, "dither": "tpdf", "seed": "3" })
            }),
            Want::Code(BadGraph),
        ),
        (
            "読み方の知らない項目",
            edit(&sheet, &|v| {
                v["graph"]["inputs"][0]["decode"] = json!({ "scale": 2 })
            }),
            Want::Code(BadGraph),
        ),
        (
            "文書のスキーマが新しすぎる",
            edit(&sheet, &|v| v["graph"]["schema"] = json!(doc::SCHEMA + 1)),
            Want::Code(SchemaTooNew),
        ),
        (
            "知らない規則の版",
            edit(&sheet, &|v| v["graph"]["rules"] = json!(doc::RULES + 1)),
            Want::Code(SchemaTooNew),
        ),
        (
            "文書の形",
            edit(&sheet, &|v| v["graph"]["nextId"] = json!(1)),
            Want::Code(BadGraph),
        ),
        // 結び付け
        (
            "ないノード",
            edit(&sheet, &|v| v["bindings"][0]["node"] = json!(9)),
            Want::Code(BadGraph),
        ),
        (
            "外したノード",
            edit(&sheet, &|v| {
                v["graph"]["nodes"][0]["enabled"] = json!(false)
            }),
            Want::Code(BadParams),
        ),
        (
            "ない引数",
            edit(&sheet, &|v| v["bindings"][0]["param"] = json!("speed")),
            Want::Code(BadParams),
        ),
        (
            "種類の違い",
            edit(&sheet, &|v| v["bindings"][0]["from"]["key"] = json!("grid")),
            Want::Code(BadParams),
        ),
        (
            "結び付けられない引数",
            edit(&sheet, &|v| {
                v["bindings"][0]["param"] = json!("skipEmpty");
            }),
            Want::Code(BadParams),
        ),
        (
            "区間でない引数の片方",
            edit(&audio, &|v| v["bindings"][0]["part"] = json!("start")),
            Want::Code(BadParams),
        ),
        (
            "区間に秒を片方なしで",
            edit(&audio, &|v| {
                v["bindings"][4].as_object_mut().unwrap().remove("part");
                v["bindings"].as_array_mut().unwrap().remove(5);
            }),
            Want::Code(BadParams),
        ),
        (
            "見本の区間が null で片方",
            edit(&audio, &|v| {
                v["graph"]["nodes"][2]["params"]["loop"] = json!(null)
            }),
            Want::Code(BadParams),
        ),
        (
            "素材の設定を片方に",
            edit(&audio, &|v| {
                v["bindings"][4]["from"] = json!({ "type": "setting", "key": "loop" })
            }),
            Want::Code(BadParams),
        ),
        (
            "重なり",
            edit(&sheet, &|v| {
                let b = v["bindings"][0].clone();
                v["bindings"].as_array_mut().unwrap().push(b);
            }),
            Want::Code(BadGraph),
        ),
        (
            "全体と片方の両方",
            edit(&audio, &|v| {
                v["bindings"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({ "node": 3, "param": "loop", "from": { "type": "setting", "key": "loop" } }))
            }),
            Want::Code(BadGraph),
        ),
        (
            "長すぎる秒",
            edit(&audio, &|v| {
                v["bindings"][0]["from"]["ms"] = json!((1u64 << 32) + 1)
            }),
            Want::Code(OutOfRange),
        ),
        (
            "知らない出どころ",
            edit(&sheet, &|v| {
                v["bindings"][0]["from"] = json!({ "type": "tag", "key": "x" })
            }),
            Want::Code(BadGraph),
        ),
        (
            "結び付けの知らない項目",
            edit(&sheet, &|v| v["bindings"][0]["scale"] = json!(2)),
            Want::Code(BadGraph),
        ),
        (
            "N → 1 に結び付け",
            edit(
                &merge,
                &|v| v["bindings"] = json!([{ "node": 1, "param": "columns", "from": { "type": "setting", "key": "frameMs" } }]),
            ),
            Want::Code(BadGraph),
        ),
        (
            "65 個の結び付け",
            edit(&audio, &|v| {
                let list: Vec<J> = (0..=MAX_BINDINGS)
                    .map(|i| json!({ "node": 1, "param": format!("p{i}"), "from": { "type": "time", "ms": 1, "anchor": "start" } }))
                    .collect();
                v["bindings"] = J::Array(list);
            }),
            Want::Code(BadGraph),
        ),
        // 名前と説明
        (
            "空の名前",
            edit(&sheet, &|v| v["name"] = json!("")),
            Want::Code(BadGraph),
        ),
        (
            "長すぎる名前",
            edit(&sheet, &|v| v["name"] = json!("a".repeat(257))),
            Want::Code(BadGraph),
        ),
        (
            "長すぎる名前（多バイト）",
            edit(&sheet, &|v| v["name"] = json!("あ".repeat(86))),
            Want::Code(BadGraph),
        ),
        (
            "名前に制御文字",
            edit(&sheet, &|v| v["name"] = json!("a\u{1}b")),
            Want::Code(BadGraph),
        ),
        (
            "名前に改行",
            edit(&sheet, &|v| v["name"] = json!("a\nb")),
            Want::Code(BadGraph),
        ),
        (
            "名前に DEL",
            edit(&sheet, &|v| v["name"] = json!("a\u{7f}")),
            Want::Code(BadGraph),
        ),
        (
            "長すぎる説明",
            edit(&sheet, &|v| v["description"] = json!("a".repeat(4097))),
            Want::Code(BadGraph),
        ),
        (
            "説明に制御文字",
            edit(&sheet, &|v| v["description"] = json!("a\u{7}")),
            Want::Code(BadGraph),
        ),
        (
            "名前が文字でない",
            edit(&sheet, &|v| v["name"] = json!(3)),
            Want::Code(BadGraph),
        ),
    ];
    // ノードが多すぎる（文書の上限 max_nodes）
    let mut many = Graph::new_single();
    for _ in 0..=Limits::default().max_nodes {
        many.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
            .unwrap();
    }
    let mut v = sheet.clone();
    v["graph"] = serde_json::to_value(&many).unwrap();
    v["bindings"] = json!([]);
    v["requires"] = json!([{ "op": "image.flip", "v": 1 }]);
    cases.push(("ノードが多すぎる", bytes(&v), Want::Code(TooLarge)));
    // 大きすぎる（JSON を読む前に）
    let mut huge = r#"{"kura":"recipe","pad":""#.as_bytes().to_vec();
    huge.resize(MAX_RECIPE_BYTES + 1, b'a');
    cases.push(("大きすぎる", huge, Want::Limit));
    // 深すぎる（封筒の全体で digest::MAX_DEPTH 段。serde_json の 128 段より浅くても）
    let mut deep = json!(1);
    for _ in 0..kura_engine::digest::MAX_DEPTH {
        deep = json!([deep]);
    }
    cases.push((
        "深すぎる",
        edit(
            &sheet,
            &|v| {
                v["graph"]["outputs"][0]["format"] =
                    json!({ "type": "to", "format": "png", "options": { "x": deep.clone() } })
            },
        ),
        Want::Limit,
    ));
    // 書き直すと膨らむ数（1e300 は正規の形で 301 桁）
    let pad = format!("[{}]", vec!["1e300"; 14_000].join(","));
    let inflated = {
        let text = String::from_utf8(bytes(&sheet)).unwrap();
        text.replacen(
            r#""format":{"type":"keep"}"#,
            &format!(r#""format":{{"type":"to","format":"png","options":{{"x":{pad}}}}}"#),
            1,
        )
    };
    assert!(inflated.len() < MAX_RECIPE_BYTES);
    cases.push(("膨らむ数", inflated.into_bytes(), Want::Limit));

    for (name, raw, want) in &cases {
        let got = engine.load_recipe(raw);
        match got {
            Ok(_) => panic!("{name}：読めてしまいました"),
            Err(e) => {
                let found = codes(&e);
                assert!(
                    found.contains(want),
                    "{name}：{want:?} のはずが {found:?}（{e}）"
                );
                // 新しい Kura が要るものは、壊れているより先（ほかの指摘を混ぜない）
                if matches!(want, Want::Code(SchemaTooNew | UnknownOp | UnknownVersion)) {
                    assert!(
                        found.iter().all(|w| matches!(
                            w,
                            Want::Code(SchemaTooNew | UnknownOp | UnknownVersion)
                        )),
                        "{name}：{found:?}"
                    );
                }
            }
        }
    }
    // 境の内側は読める：256 バイトの名前（多バイト）・4,096 バイトの説明・改行とタブ・64 個の結び付けまで
    let ok = [
        edit(&sheet, &|v| {
            v["name"] = json!(format!("{}a", "あ".repeat(85)))
        }),
        edit(&sheet, &|v| v["description"] = json!("a".repeat(4096))),
        edit(&sheet, &|v| v["description"] = json!("一行目\n\t二行目")),
    ];
    for raw in ok {
        assert!(engine.load_recipe(&raw).is_ok());
    }
}

/// 壊れた入力：ChaCha8 の壊し方（バイトの反転・切り詰め・断片の差し込み・型の入れ替え・入れ子・上限の前後の長さ）で
/// 1,000 回。落ちず、`Internal` にならず、読めたものは書いて読み直すと同じ。
#[test]
fn broken_files_never_panic() {
    let engine = Engine::new().unwrap();
    let seeds: Vec<Vec<u8>> = [
        sheet_recipe(),
        audio_recipe(),
        merge_recipe(),
        loop_recipe(),
    ]
    .iter()
    .map(|r| r.to_file().unwrap())
    .collect();
    let mut r = determinism::rng(0x004e_c19e);
    let mut read = 0;
    for i in 0..1_000 {
        let base = &seeds[below(&mut r, seeds.len() as u64) as usize];
        let input = corrupt(&mut r, base, &seeds);
        let got = catch_own_panic(|| engine.load_recipe(&input));
        match got {
            Err(at) => panic!("{i}：自前のコードの panic（{at}）"),
            Ok(Err(EngineError::Internal(m))) => panic!("{i}：内部の誤り {m}"),
            Ok(Err(_)) => {}
            Ok(Ok(loaded)) => {
                read += 1;
                let again = loaded.recipe.to_file().unwrap();
                let back = engine.load_recipe(&again).unwrap();
                assert_eq!(back.recipe, loaded.recipe, "{i}");
                assert_eq!(back.recipe.to_file().unwrap(), again, "{i}");
            }
        }
    }
    // 壊し方が強すぎて何も読めない、ということがない
    assert!(read > 20, "{read}");
}

/// 壊し方 1 回。
fn corrupt(r: &mut ChaCha8Rng, base: &[u8], seeds: &[Vec<u8>]) -> Vec<u8> {
    let mut b = base.to_vec();
    match below(r, 8) {
        // バイトの反転
        0 => {
            for _ in 0..1 + below(r, 4) {
                let at = below(r, b.len() as u64) as usize;
                b[at] ^= 1 << below(r, 8);
            }
        }
        // 切り詰め
        1 => b.truncate(below(r, b.len() as u64) as usize),
        // 別の種の断片の差し込み
        2 => {
            let other = &seeds[below(r, seeds.len() as u64) as usize];
            let from = below(r, other.len() as u64) as usize;
            let n = (1 + below(r, 64) as usize).min(other.len() - from);
            let at = below(r, b.len() as u64) as usize;
            b.splice(at..at, other[from..from + n].iter().copied());
        }
        // JSON の値の型の入れ替え
        3 | 4 => {
            let mut v: J = serde_json::from_slice(base).unwrap();
            let replacement = match below(r, 7) {
                0 => J::Null,
                1 => json!(true),
                2 => json!(-1),
                3 => json!(1e300),
                4 => json!("x"),
                5 => json!([]),
                _ => json!({}),
            };
            swap_random(r, &mut v, replacement);
            b = bytes(&v);
        }
        // 入れ子 1〜150 段
        5 => {
            let depth = 1 + below(r, 150) as usize;
            let mut v: J = serde_json::from_slice(base).unwrap();
            let mut deep = json!(1);
            for _ in 0..depth {
                deep = json!([deep]);
            }
            let fmt = json!({ "type": "to", "format": "png", "options": { "x": deep } });
            v["graph"]["outputs"][0]["format"] = fmt;
            b = bytes(&v);
        }
        // 上限の前後の長さ（空白で詰める）
        6 => {
            let target = MAX_RECIPE_BYTES - 2 + below(r, 4) as usize;
            b.resize(target, b' ');
        }
        // 数を書き換える
        _ => {
            let text = String::from_utf8_lossy(base).into_owned();
            let nums = [
                "0",
                "-1",
                "65536",
                "4294967296",
                "1e300",
                "1.5",
                "18446744073709551616",
            ];
            let n = nums[below(r, nums.len() as u64) as usize];
            b = text
                .replacen(":1", &format!(":{n}"), 1 + below(r, 3) as usize)
                .into_bytes();
        }
    }
    b
}

/// 値の木のどこか 1 つを `with` に替える（たどる道を JSON Pointer にしてから替える）。
fn swap_random(r: &mut ChaCha8Rng, v: &mut J, with: J) {
    let mut path = String::new();
    let mut at: &J = v;
    for _ in 0..1 + below(r, 6) {
        let next = match at {
            J::Object(map) if !map.is_empty() => {
                let k = below(r, map.len() as u64) as usize;
                map.iter()
                    .nth(k)
                    .map(|(key, x)| (key.replace('~', "~0").replace('/', "~1"), x))
            }
            J::Array(list) if !list.is_empty() => {
                let k = below(r, list.len() as u64) as usize;
                list.get(k).map(|x| (k.to_string(), x))
            }
            _ => None,
        };
        match next {
            Some((step, x)) => {
                path.push('/');
                path.push_str(&step);
                at = x;
            }
            None => break,
        }
    }
    if let Some(slot) = v.pointer_mut(&path) {
        *slot = with;
    }
}

/// 書けたレシピは必ず読め、読めたレシピは必ず書ける：深さ（封筒の全体で digest::MAX_DEPTH 段）と大きさ（MAX_RECIPE_BYTES
/// ちょうど）の境で、書く側と読む側が同じところで断る。
#[test]
fn written_recipes_always_read_back() {
    let engine = Engine::new().unwrap();
    // 深さ：出力の設定（文書の中で 5 段目）の下に入れ子を置く
    let with_options = |options: J| {
        let mut r = sheet_recipe();
        r.graph.outputs[0].format = FormatChoice::To {
            format: "png".into(),
            options,
        };
        r
    };
    let nest = |n: usize| {
        let mut deep = json!(1);
        for _ in 0..n {
            deep = json!([deep]);
        }
        json!({ "x": deep })
    };
    let mut last_ok = None;
    for n in 80..=110 {
        let r = with_options(nest(n));
        let written = r.to_file();
        let raw = serde_json::to_vec(&{
            let mut v = file_value(&sheet_recipe());
            v["graph"]["outputs"][0]["format"] =
                json!({ "type": "to", "format": "png", "options": nest(n) });
            v
        })
        .unwrap();
        let read = engine.load_recipe(&raw);
        assert_eq!(written.is_ok(), read.is_ok(), "深さ {n}");
        if let Ok(bytes) = written {
            assert_eq!(engine.load_recipe(&bytes).unwrap().recipe, r);
            last_ok = Some(n);
        } else {
            assert!(matches!(written, Err(EngineError::Limit(_))));
            assert!(matches!(read, Err(EngineError::Limit(_))));
        }
    }
    let last_ok = last_ok.unwrap();
    assert!((85..105).contains(&last_ok), "{last_ok}");
    // 大きさ：正規の形がちょうど MAX_RECIPE_BYTES なら書けて読め、1 バイト多ければどちらも断る。生のファイルは 1e300 を
    // 短く書いて上限より小さくしても（正規の形では 301 桁）、読む側は正規の形の長さで断る
    let with_y = |x: String| with_options(json!({ "x": x, "y": 1e300 }));
    let long_form =
        String::from_utf8(kura_engine::digest::canonical_json(&json!(1e300)).unwrap()).unwrap();
    let short = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .replacen(&long_form, "1e300", 1)
            .into_bytes()
    };
    let empty = with_y(String::new()).to_file().unwrap().len();
    let fill = MAX_RECIPE_BYTES - empty;
    let exact = with_y("a".repeat(fill));
    let bytes = exact.to_file().unwrap();
    assert_eq!(bytes.len(), MAX_RECIPE_BYTES);
    assert_eq!(engine.load_recipe(&bytes).unwrap().recipe, exact);
    assert_eq!(engine.load_recipe(&short(&bytes)).unwrap().recipe, exact);
    let over = with_y("a".repeat(fill + 1));
    assert!(matches!(over.to_file(), Err(EngineError::Limit(_))));
    assert!(matches!(
        Recipe::new(
            over.name.clone(),
            over.description.clone(),
            RecipeKind::Each,
            over.graph.clone(),
            over.bindings.clone()
        ),
        Err(EngineError::Limit(_))
    ));
    let mut v = file_value(&exact);
    v["graph"]["outputs"][0]["format"]["options"]["x"] = json!("a".repeat(fill + 1));
    let raw = short(&kura_engine::digest::canonical_json(&v).unwrap());
    assert!(raw.len() < MAX_RECIPE_BYTES);
    assert!(matches!(
        engine.load_recipe(&raw),
        Err(EngineError::Limit(_))
    ));
}

/// 作る口（`Recipe::new`）も同じ確かめ：固定の素材・出力 2 つ・Keep と量子化・名前・結び付けの形を断る。
#[test]
fn recipe_new_refuses_bad_shapes() {
    let ok = sheet_recipe();
    let try_new = |kind, graph: Graph, bindings: Vec<Binding>, name: &str| {
        Recipe::new(name.into(), String::new(), kind, graph, bindings)
    };
    let mut fixed = ok.graph.clone();
    fixed.inputs[0].role = InputRole::Fixed {
        hash: Digest([1; 32]),
    };
    assert!(try_new(RecipeKind::Each, fixed, Vec::new(), "x").is_err());
    let mut keepq = audio_recipe().graph;
    keepq.outputs[0].quantize = Some(Quantize {
        v: QUANTIZE_VERSION,
        bits: 16,
        dither: Dither::None,
        seed: 1,
    });
    assert!(try_new(RecipeKind::Each, keepq.clone(), Vec::new(), "x").is_err());
    keepq.outputs[0].format = FormatChoice::To {
        format: "wav".into(),
        options: json!({ "bits": 16 }),
    };
    assert!(try_new(RecipeKind::Each, keepq, Vec::new(), "x").is_ok());
    assert!(try_new(RecipeKind::Each, ok.graph.clone(), Vec::new(), "").is_err());
    assert!(try_new(RecipeKind::Merge, ok.graph.clone(), Vec::new(), "x").is_err());
    let dup = vec![
        bind(1, "grid", None, setting(SettingKey::Grid)),
        bind(1, "grid", None, setting(SettingKey::Grid)),
    ];
    assert!(try_new(RecipeKind::Each, ok.graph.clone(), dup, "x").is_err());
    let mut old = ok.graph.clone();
    old.schema = 2;
    assert!(try_new(RecipeKind::Each, old, Vec::new(), "x").is_err());
    // 書く口も同じ確かめ（フィールドを直に変えたもの）
    let mut broken = ok.clone();
    broken.graph.outputs.push(broken.graph.outputs[0].clone());
    broken.graph.outputs[1].id = OutputId(1);
    assert!(broken.to_file().is_err());
    let mut unnamed = ok.clone();
    unnamed.name.clear();
    assert!(unnamed.to_file().is_err());
}

/// 結び付けの DB の列の形（`bindings_json`・`bindings_from_json`）：並べて書き、往復し、上限で断る。
#[test]
fn bindings_json_round_trips() {
    let a = audio_recipe();
    let mut shuffled = a.bindings.clone();
    shuffled.reverse();
    let raw = kura_engine::recipe::bindings_json(&shuffled).unwrap();
    assert_eq!(
        raw,
        kura_engine::recipe::bindings_json(&a.bindings).unwrap()
    );
    assert_eq!(
        kura_engine::recipe::bindings_from_json(&raw).unwrap(),
        a.bindings
    );
    let many: Vec<Binding> = (0..=MAX_BINDINGS)
        .map(|i| bind(1, &format!("p{i}"), None, time(1, TimeAnchor::Start)))
        .collect();
    assert!(kura_engine::recipe::bindings_json(&many).is_err());
    let raw = serde_json::to_vec(&many).unwrap();
    assert!(kura_engine::recipe::bindings_from_json(&raw).is_err());
    assert!(kura_engine::recipe::bindings_from_json(b"[{\"node\":1}]").is_err());
    assert!(kura_engine::recipe::bindings_from_json(&vec![b' '; 70_000]).is_err());
}

// ---------------------------------------------------------------------------------------------
// E49：継ぎ足し
// ---------------------------------------------------------------------------------------------

fn quantize(r: &mut ChaCha8Rng) -> Quantize {
    Quantize {
        v: QUANTIZE_VERSION,
        bits: [8u16, 16, 24][below(r, 3) as usize],
        dither: if below(r, 2) == 0 {
            Dither::Tpdf
        } else {
            Dither::None
        },
        seed: r.next_u64(),
    }
}

/// ランダムな出力の形式（画像は png・webp、音声は wav・flac。`recipe` なら Keep に量子化を付けない）。
fn random_output(r: &mut ChaCha8Rng, g: &mut Graph, audio: bool, recipe: bool) {
    let out = &mut g.outputs[0];
    let formats: &[(&str, J)] = if audio {
        &[
            ("wav", json!({ "bits": 16 })),
            ("wav", json!({ "bits": 24 })),
            ("flac", json!({})),
        ]
    } else {
        &[("png", json!({})), ("webp", json!({}))]
    };
    out.format = if below(r, 3) == 0 {
        FormatChoice::Keep
    } else {
        let (f, o) = &formats[below(r, formats.len() as u64) as usize];
        FormatChoice::To {
            format: (*f).into(),
            options: o.clone(),
        }
    };
    out.quantize = if audio && below(r, 2) == 0 && !(recipe && out.format == FormatChoice::Keep) {
        Some(quantize(r))
    } else {
        None
    };
}

/// 継いだ文書の出力は §4.3 の表どおり（Keep は頭の形式・設定・量子化、To はレシピのもの）。
fn expected_output(base: &Graph, recipe: &Graph) -> (FormatChoice, Option<Quantize>) {
    match recipe.outputs[0].format {
        FormatChoice::Keep => (base.outputs[0].format.clone(), base.outputs[0].quantize),
        FormatChoice::To { .. } => (recipe.outputs[0].format.clone(), recipe.outputs[0].quantize),
    }
}

/// E49 の性質：ランダムな base とレシピ（画像と音声、出力の形式・量子化つき）で 200 組。構造・ID・種・next_id・出力が
/// 規則どおり、継いだ文書の値が「量子化を外した base の値に、出力を継いだレシピを当てた値」と同じ、結合則、空のレシピは base と同じ。
#[test]
fn compose_follows_the_rules_on_random_documents() {
    let engine = Engine::new().unwrap();
    let mut r = determinism::rng(0xe49_0001);
    let mut rendered = 0;
    let mut kept = 0;
    let mut audio_cases = 0;
    for case in 0..200 {
        let input = random_input(&mut r).generate().unwrap();
        let audio = matches!(input.shape(), Shape::Audio { .. });
        audio_cases += usize::from(audio);
        let source = Digest(*blake3::hash(&(case as u64).to_le_bytes()).as_bytes());
        let len = below(&mut r, 4) as usize;
        let mut base = random_chain(&engine, &mut r, &input.shape(), len, false);
        random_output(&mut r, &mut base, audio, false);
        let shape_after = |g: &Graph| {
            engine
                .compile(g, &shapes(&input))
                .ok()
                .and_then(|p| p.output_shapes().first().map(|o| o.1))
        };
        let Some(mid) = shape_after(&base) else {
            continue;
        };
        let len = 1 + below(&mut r, 3) as usize;
        let mut r1 = random_chain(&engine, &mut r, &mid, len, false);
        random_output(&mut r, &mut r1, audio, true);
        let g = compose(&base, &r1).unwrap();
        let at = format!("組 {case}");
        // (a) 構造・ID・種・next_id
        assert!(g.check_structure(1_000).is_empty(), "{at}");
        assert_eq!(g.nodes.len(), base.nodes.len() + r1.nodes.len(), "{at}");
        assert_eq!(&g.nodes[..base.nodes.len()], base.nodes.as_slice(), "{at}");
        for (n, m) in g.nodes[base.nodes.len()..].iter().zip(&r1.nodes) {
            assert_eq!(n.id, NodeId(m.id.0 + base.next_id), "{at}");
            assert_eq!(
                (n.seed, &n.op, n.v, &n.params, n.enabled),
                (m.seed, &m.op, m.v, &m.params, m.enabled),
                "{at}"
            );
        }
        assert_eq!(g.next_id, base.next_id + r1.next_id, "{at}");
        assert_eq!(g.inputs, base.inputs, "{at}");
        // (b) 出力
        let (format, q) = expected_output(&base, &r1);
        assert_eq!(
            (&g.outputs[0].format, g.outputs[0].quantize),
            (&format, q),
            "{at}"
        );
        assert_eq!(g.outputs[0].expand, Expand::One, "{at}");
        kept += usize::from(r1.outputs[0].format == FormatChoice::Keep);
        // (c) 値：量子化を外した base の値に、出力を継いだレシピを当てた値と同じ
        let mut plain_base = base.clone();
        plain_base.outputs[0].quantize = None;
        let mut r1_out = r1.clone();
        r1_out.outputs[0].format = format.clone();
        r1_out.outputs[0].quantize = q;
        let whole = render(&engine, &g, &input, source);
        let two = render(&engine, &plain_base, &input, source)
            .and_then(|mid| render(&engine, &r1_out, &mid, source));
        match (whole, two) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a.digest().unwrap(), b.digest().unwrap(), "{at}");
                rendered += 1;
            }
            (Err(_), Err(_)) => {}
            (a, b) => panic!("{at}：描けたかが違います {:?} {:?}", a.err(), b.err()),
        }
        // (d) 結合則
        let Some(after) = shape_after(&g) else {
            continue;
        };
        let len = below(&mut r, 3) as usize;
        let mut r2 = random_chain(&engine, &mut r, &after, len, false);
        random_output(&mut r, &mut r2, audio, true);
        let left = compose(&compose(&base, &r1).unwrap(), &r2).unwrap();
        let right = compose(&base, &compose(&r1, &r2).unwrap()).unwrap();
        assert_eq!(left, right, "{at}：結合則");
        // (e) 空のレシピを継ぐと base と同じ（next_id が 1 増えるだけ）
        let empty = compose(&base, &Graph::new_single()).unwrap();
        let mut want = base.clone();
        want.next_id += 1;
        assert_eq!(empty, want, "{at}");
    }
    assert!(rendered > 100, "{rendered}");
    assert!(kept > 30 && kept < 170, "{kept}");
    assert!(audio_cases > 10, "{audio_cases}");
}

/// 継ぐ規則の表（音声）：base `To{wav,{bits:16}}`＋量子化 Q ＋ レシピ Keep → 同じ形式・Q。レシピ `To{flac}`・量子化なし →
/// `To{flac}`・量子化なし。レシピ Keep＋量子化・base の出力 2 つ・PerElement は断る。継いだ文書が大きすぎれば `Limit`。
#[test]
fn compose_inherits_the_head_output() {
    let q = Quantize {
        v: QUANTIZE_VERSION,
        bits: 16,
        dither: Dither::Tpdf,
        seed: 9,
    };
    let mut base = chain(&[("audio.gain", json!({ "db": -1.0 }))]);
    base.outputs[0].format = FormatChoice::To {
        format: "wav".into(),
        options: json!({ "bits": 16 }),
    };
    base.outputs[0].quantize = Some(q);
    let recipe = chain(&[("audio.gain", json!({ "db": 2.0 }))]);
    let g = compose(&base, &recipe).unwrap();
    assert_eq!(g.outputs[0].format, base.outputs[0].format);
    assert_eq!(g.outputs[0].quantize, Some(q));
    let mut flac = recipe.clone();
    flac.outputs[0].format = FormatChoice::To {
        format: "flac".into(),
        options: json!({}),
    };
    let g = compose(&base, &flac).unwrap();
    assert_eq!(g.outputs[0].format, flac.outputs[0].format);
    assert_eq!(g.outputs[0].quantize, None);
    let mut keepq = recipe.clone();
    keepq.outputs[0].quantize = Some(q);
    assert!(compose(&base, &keepq).is_err());
    let mut two = base.clone();
    let mut second = two.outputs[0].clone();
    second.id = OutputId(1);
    two.outputs.push(second);
    assert!(compose(&two, &recipe).is_err());
    let mut per = base.clone();
    per.outputs[0].expand = Expand::PerElement {
        name: "{stem}_{index}".into(),
    };
    assert!(compose(&per, &recipe).is_err());
    let mut per_recipe = recipe.clone();
    per_recipe.outputs[0].expand = Expand::PerElement {
        name: "{stem}_{index}".into(),
    };
    assert!(compose(&base, &per_recipe).is_err());
    // 大きすぎる継ぎ足し：どちらも上限の 6 割ほどの文書
    let big = |n: usize| {
        let mut g = Graph::new_single();
        g.push_linear(
            "paint.actions",
            1,
            json!({ "actions": [], "pad": "a".repeat(n) }),
            0,
        )
        .unwrap();
        g
    };
    let half = doc::MAX_DOC_BYTES * 6 / 10;
    assert!(big(half).to_canonical_json().is_ok());
    assert!(matches!(
        compose(&big(half), &big(half)),
        Err(EngineError::Limit(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// E50：結び付け
// ---------------------------------------------------------------------------------------------

fn sheet_input() -> Value {
    InputGen::Sheet {
        cols: 4,
        rows: 2,
        cell: 16,
    }
    .generate()
    .unwrap()
}

fn audio_input(rate: u32, frames: u64) -> Value {
    InputGen::AudioNoise {
        rate,
        channels: 2,
        frames,
        seed: 3,
    }
    .generate()
    .unwrap()
}

fn full_facts() -> Facts {
    Facts {
        grid: Some(GridParam {
            cell_w: 16,
            cell_h: 16,
            offset_x: 0,
            offset_y: 0,
            gap_x: 0,
            gap_y: 0,
        }),
        pivot: Some(PivotFact { x: 7.5, y: 3.0 }),
        loop_range: Some(kura_engine::ops::audio::set_loop::FrameRange {
            start: 100,
            end: 900,
        }),
        frame_ms: Some(120),
    }
}

/// 結び付けのないレシピでは compose と同じ文書（記録なし）。同じ入力からは同じ文書・同じ記録（2 回・並びを変えた一括でも）。
#[test]
fn instantiate_is_pure() {
    let engine = Engine::new().unwrap();
    let base = chain(&[("image.flip", json!({ "axis": "horizontal" }))]);
    let plain = Recipe::new(
        "左右".into(),
        String::new(),
        RecipeKind::Each,
        chain(&[("image.flip", json!({ "axis": "vertical" }))]),
        Vec::new(),
    )
    .unwrap();
    let input = sheet_input();
    let inst = engine
        .instantiate(&base, &plain, &shapes(&input), &Facts::default())
        .unwrap();
    assert_eq!(inst.graph, compose(&base, &plain.graph).unwrap());
    assert!(inst.bound.is_empty());
    // 一括の並び・件数によらない
    let items: Vec<(Graph, Value, Facts)> = (0..5u32)
        .map(|i| {
            let mut f = full_facts();
            f.frame_ms = Some(50 + i * 10);
            let base = chain(&[(
                "image.flip",
                json!({ "axis": if i % 2 == 0 { "vertical" } else { "horizontal" } }),
            )]);
            (base, sheet_input(), f)
        })
        .collect();
    let recipe = sheet_recipe();
    let run = |(b, v, f): &(Graph, Value, Facts)| {
        let x = engine.instantiate(b, &recipe, &shapes(v), f).unwrap();
        (
            x.graph.to_canonical_json().unwrap(),
            kura_engine::recipe::bound_json(&x.bound).unwrap(),
        )
    };
    let forward: Vec<_> = items.iter().map(run).collect();
    let backward: Vec<_> = items.iter().rev().map(run).collect();
    let other = Engine::new().unwrap();
    let alone: Vec<_> = items
        .iter()
        .map(|(b, v, f)| {
            let x = other.instantiate(b, &recipe, &shapes(v), f).unwrap();
            (
                x.graph.to_canonical_json().unwrap(),
                kura_engine::recipe::bound_json(&x.bound).unwrap(),
            )
        })
        .collect();
    for i in 0..items.len() {
        assert_eq!(forward[i], backward[items.len() - 1 - i]);
        assert_eq!(forward[i], alone[i]);
    }
    assert_ne!(forward[0], forward[1]);
}

/// 素材の設定の値の書き方：格子は count を除いてそのまま、コマの時間はそのまま、ループはそのまま、ピボットは切り捨て。
/// 記録（BoundValue）には読んだ値と書いた値。
#[test]
fn settings_are_written_as_read() {
    let engine = Engine::new().unwrap();
    let base = Graph::new_single();
    let input = sheet_input();
    let mut facts = full_facts();
    facts.grid = Some(GridParam {
        cell_w: 8,
        cell_h: 16,
        offset_x: 0,
        offset_y: 0,
        gap_x: 0,
        gap_y: 0,
    });
    let inst = engine
        .instantiate(&base, &sheet_recipe(), &shapes(&input), &facts)
        .unwrap();
    let split = &inst.graph.nodes[0];
    assert_eq!(
        split.params["grid"],
        json!({ "cellW": 8, "cellH": 16, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 })
    );
    assert_eq!(split.params["durationMs"], json!(120));
    assert_eq!(split.params["skipEmpty"], json!(false));
    assert_eq!(inst.bound.len(), 2);
    assert_eq!(inst.bound[0].param, "durationMs");
    assert_eq!(inst.bound[0].node, NodeId(base.next_id + 1));
    assert_eq!(
        (inst.bound[0].read.clone(), inst.bound[0].value.clone()),
        (json!(120), json!(120))
    );
    assert_eq!(inst.bound[1].value, split.params["grid"]);
    // 描ける（64×32 のシートから 8×16 のコマが 16 → 2 列に 8 行）
    let out = render(&engine, &inst.graph, &input, Digest::default()).unwrap();
    assert_eq!(
        out.shape(),
        Shape::Image {
            w: Dim::Exact(16),
            h: Dim::Exact(128),
            fmt: PixelFormat::Rgba8
        }
    );
    // ループ
    let audio = audio_input(8_000, 1_000);
    let inst = engine
        .instantiate(&base, &loop_recipe(), &shapes(&audio), &full_facts())
        .unwrap();
    assert_eq!(
        inst.graph.nodes[1].params["loop"],
        json!({ "start": 100, "end": 900 })
    );
    // ピボット（meta.set_pivot）：切り捨て（7.5 → 7、−0.5 → −1、2.99 → 2）
    let img = InputGen::Noise {
        w: 20,
        h: 10,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    for ((x, y), want) in [
        ((7.5, 3.0), json!({ "x": 7, "y": 3 })),
        ((0.0, 9.99), json!({ "x": 0, "y": 9 })),
        ((20.0, 10.0), json!({ "x": 20, "y": 10 })),
    ] {
        let mut f = full_facts();
        f.pivot = Some(PivotFact { x, y });
        let inst = engine
            .instantiate(&base, &pivot_recipe(), &shapes(&img), &f)
            .unwrap();
        assert_eq!(inst.graph.nodes[1].params["pivot"], want);
        assert_eq!(inst.bound[0].read, json!({ "x": x, "y": y }));
        assert_eq!(inst.bound[0].value, want);
        let out = render(&engine, &inst.graph, &img, Digest::default()).unwrap();
        let Value::Image(i) = out else { panic!() };
        assert_eq!(i.meta.pivot, serde_json::from_value(want).ok());
    }
    // −0.5 → −1 は画像の外（操作の検証で断る）
    let mut f = full_facts();
    f.pivot = Some(PivotFact { x: -0.5, y: 2.0 });
    assert!(matches!(
        engine.instantiate(&base, &pivot_recipe(), &shapes(&img), &f),
        Err(BindError::OutOfRange {
            node: NodeId(2),
            ..
        })
    ));
    // 有限でない・±2^31 の外は直す前に断る
    for (x, y) in [
        (f64::NAN, 1.0),
        (1.0, f64::INFINITY),
        (3e9, 1.0),
        (1.0, -3e9),
    ] {
        let mut f = full_facts();
        f.pivot = Some(PivotFact { x, y });
        assert!(
            matches!(
                engine.instantiate(&base, &pivot_recipe(), &shapes(&img), &f),
                Err(BindError::OutOfRange {
                    node: NodeId(2),
                    ..
                })
            ),
            "{x} {y}"
        );
    }
}

/// `n / d` を最も近い整数に（ちょうど半分は偶数へ）。テストの側で別に書く。
fn half_even(n: u128, d: u128) -> u128 {
    let (q, rem) = (n / d, n % d);
    if rem * 2 > d || (rem * 2 == d && q % 2 == 1) {
        q + 1
    } else {
        q
    }
}

/// 秒の直し：`round_half_even(ms·rate/1000)`（レート 8k・22.05k・44.1k・48k・96k、ms 0・1・999・1,500、終わりから）。前に
/// `audio.resample` があれば変えたあとのレートを使う。
#[test]
fn seconds_become_frames_at_the_node_rate() {
    let engine = Engine::new().unwrap();
    let base = Graph::new_single();
    let trim_start = |ms| {
        Recipe::new(
            "頭を切る".into(),
            String::new(),
            RecipeKind::Each,
            chain(&[("audio.trim", json!({ "start": 0, "end": null }))]),
            vec![bind(1, "start", None, time(ms, TimeAnchor::Start))],
        )
        .unwrap()
    };
    let trim_end = |ms| {
        Recipe::new(
            "終わりを切る".into(),
            String::new(),
            RecipeKind::Each,
            chain(&[("audio.trim", json!({ "start": 0, "end": null }))]),
            vec![bind(1, "end", None, time(ms, TimeAnchor::End))],
        )
        .unwrap()
    };
    for rate in [8_000u32, 22_050, 44_100, 48_000, 96_000] {
        let frames = u64::from(rate) * 2 + 7;
        let shape = Shape::Audio {
            rate,
            channels: 1,
            frames: Dim::Exact(frames),
        };
        for ms in [0u64, 1, 999, 1_500] {
            let want = half_even(u128::from(ms) * u128::from(rate), 1_000) as u64;
            let inst = engine
                .instantiate(&base, &trim_start(ms), &one_shape(shape), &Facts::default())
                .unwrap();
            assert_eq!(
                inst.graph.nodes[0].params["start"],
                json!(want),
                "{rate} {ms}"
            );
            assert_eq!(
                inst.bound[0].read,
                json!({ "rate": rate, "frames": frames })
            );
            let inst = engine
                .instantiate(&base, &trim_end(ms), &one_shape(shape), &Facts::default())
                .unwrap();
            assert_eq!(
                inst.graph.nodes[0].params["end"],
                json!(frames - want),
                "{rate} {ms}"
            );
        }
    }
    // 例：44.1 kHz で 1,500 ms → 66,150、48 kHz → 72,000、22.05 kHz で 1 ms → 22
    let at = |rate: u32, ms| {
        let shape = Shape::Audio {
            rate,
            channels: 2,
            frames: Dim::Exact(200_000),
        };
        engine
            .instantiate(&base, &trim_start(ms), &one_shape(shape), &Facts::default())
            .unwrap()
            .graph
            .nodes[0]
            .params["start"]
            .clone()
    };
    assert_eq!(at(44_100, 1_500), json!(66_150));
    assert_eq!(at(48_000, 1_500), json!(72_000));
    assert_eq!(at(22_050, 1), json!(22));
    // 前に resample があれば、変えたあとのレート
    let resampled = Recipe::new(
        "48k にして頭を切る".into(),
        String::new(),
        RecipeKind::Each,
        chain(&[
            ("audio.resample", json!({ "rate": 48_000 })),
            ("audio.trim", json!({ "start": 0, "end": null })),
        ]),
        vec![bind(2, "start", None, time(1_500, TimeAnchor::Start))],
    )
    .unwrap();
    let shape = Shape::Audio {
        rate: 44_100,
        channels: 2,
        frames: Dim::Exact(200_000),
    };
    let inst = engine
        .instantiate(&base, &resampled, &one_shape(shape), &Facts::default())
        .unwrap();
    assert_eq!(inst.graph.nodes[1].params["start"], json!(72_000));
    // 頭の文書の resample も効く（入力の形は継いだ文書の推論）
    let head = chain(&[("audio.resample", json!({ "rate": 22_050 }))]);
    let inst = engine
        .instantiate(
            &head,
            &trim_start(1_000),
            &one_shape(shape),
            &Facts::default(),
        )
        .unwrap();
    assert_eq!(inst.graph.nodes[1].params["start"], json!(22_050));
    // 区間の片方：全体をループ
    let audio = audio_input(8_000, 4_000);
    let inst = engine
        .instantiate(&base, &audio_recipe(), &shapes(&audio), &Facts::default())
        .unwrap();
    let trimmed = 4_000 - 800 - 2_000;
    assert_eq!(
        inst.graph.nodes[0].params,
        json!({ "start": 800, "end": 2_000 })
    );
    assert_eq!(inst.graph.nodes[1].params["fadeIn"], json!(400));
    assert_eq!(inst.graph.nodes[1].params["fadeOut"], json!(640));
    assert_eq!(
        inst.graph.nodes[2].params["loop"],
        json!({ "start": 0, "end": trimmed })
    );
    let out = render(&engine, &inst.graph, &audio, Digest::default()).unwrap();
    let Value::Audio(a) = out else { panic!() };
    assert_eq!(a.frames() as u64, trimmed);
    assert_eq!(
        a.meta.loop_range.map(|l| (l.start, l.end)),
        Some((0, trimmed))
    );
}

/// 誤り：値がない（Missing）、前に座標・時刻を変える操作（Moved。外したノードは恒等）、長さが決まらない（NoLength）、
/// 音声でない（NotAudio）、範囲・検証を通らない（OutOfRange）、前の部分が推論できない（Invalid）、N → 1（Kind）、
/// 規則の版（Recipe）。
#[test]
fn binding_errors_are_specific() {
    let engine = Engine::new().unwrap();
    let base = Graph::new_single();
    let sheet = sheet_input();
    // Missing：キーごと
    let mut no_grid = full_facts();
    no_grid.grid = None;
    match engine.instantiate(&base, &sheet_recipe(), &shapes(&sheet), &no_grid) {
        Err(BindError::Missing { node, param, key }) => {
            assert_eq!(
                (node, param.as_str(), key),
                (NodeId(1), "grid", SettingKey::Grid)
            );
        }
        other => panic!("{other:?}"),
    }
    let mut no_ms = full_facts();
    no_ms.frame_ms = None;
    assert!(matches!(
        engine.instantiate(&base, &sheet_recipe(), &shapes(&sheet), &no_ms),
        Err(BindError::Missing {
            key: SettingKey::FrameMs,
            ..
        })
    ));
    let audio = audio_input(8_000, 1_000);
    assert!(matches!(
        engine.instantiate(&base, &loop_recipe(), &shapes(&audio), &Facts::default()),
        Err(BindError::Missing {
            key: SettingKey::Loop,
            node: NodeId(2),
            ..
        })
    ));
    // Moved：格子の前に座標を変える操作（crop・flip）。外したノードは恒等。頭の文書の操作は数えない
    let moved = |steps: &[(&str, J)], enabled: bool| {
        let mut g = chain(steps);
        g.push_linear(
            "sprite.split_grid",
            1,
            json!({ "grid": grid_json(16), "durationMs": 100, "skipEmpty": false }),
            0,
        )
        .unwrap();
        if !enabled {
            g.nodes[0].enabled = false;
        }
        let n = g.nodes.len() as u32;
        Recipe::new(
            "x".into(),
            String::new(),
            RecipeKind::Each,
            g,
            vec![bind(n, "grid", None, setting(SettingKey::Grid))],
        )
        .unwrap()
    };
    let crop = (
        "image.crop",
        json!({ "rect": { "x": 16, "y": 0, "w": 32, "h": 32 } }),
    );
    let flip = ("image.flip", json!({ "axis": "horizontal" }));
    let gain_free = ("paint.actions", json!({ "actions": [] }));
    match engine.instantiate(
        &base,
        &moved(&[crop.clone(), flip.clone()], true),
        &shapes(&sheet),
        &full_facts(),
    ) {
        Err(BindError::Moved { node, param, by }) => {
            assert_eq!((node, param.as_str(), by), (NodeId(3), "grid", NodeId(1)));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        engine.instantiate(
            &base,
            &moved(std::slice::from_ref(&flip), true),
            &shapes(&sheet),
            &full_facts()
        ),
        Err(BindError::Moved { by: NodeId(1), .. })
    ));
    assert!(
        engine
            .instantiate(
                &base,
                &moved(std::slice::from_ref(&flip), false),
                &shapes(&sheet),
                &full_facts()
            )
            .is_ok()
    );
    assert!(
        engine
            .instantiate(
                &base,
                &moved(&[gain_free], true),
                &shapes(&sheet),
                &full_facts()
            )
            .is_ok()
    );
    let edited = chain(std::slice::from_ref(&flip));
    assert!(
        engine
            .instantiate(&edited, &moved(&[], true), &shapes(&sheet), &full_facts())
            .is_ok()
    );
    // Moved：ループの前に時刻を変える操作（trim）。コマの時間は座標を持たないので見ない
    let trim_then_loop = Recipe::new(
        "x".into(),
        String::new(),
        RecipeKind::Each,
        chain(&[
            ("audio.trim", json!({ "start": 10, "end": null })),
            ("audio.set_loop", json!({ "loop": null })),
        ]),
        vec![bind(2, "loop", None, setting(SettingKey::Loop))],
    )
    .unwrap();
    assert!(matches!(
        engine.instantiate(&base, &trim_then_loop, &shapes(&audio), &full_facts()),
        Err(BindError::Moved {
            node: NodeId(2),
            by: NodeId(1),
            ..
        })
    ));
    let gain_then_loop = loop_recipe();
    assert!(
        engine
            .instantiate(&base, &gain_then_loop, &shapes(&audio), &full_facts())
            .is_ok()
    );
    // NoLength：長さの決まらない入力に終わりからの秒
    let open = Shape::Audio {
        rate: 8_000,
        channels: 1,
        frames: Dim::AtMost(10_000),
    };
    assert!(matches!(
        engine.instantiate(&base, &audio_recipe(), &one_shape(open), &Facts::default()),
        Err(BindError::NoLength {
            node: NodeId(1),
            ..
        })
    ));
    // NotAudio：画像に秒
    assert!(matches!(
        engine.instantiate(&base, &audio_recipe(), &shapes(&sheet), &Facts::default()),
        Err(BindError::NotAudio {
            node: NodeId(1),
            ..
        })
    ));
    // OutOfRange：コマの時間が 65,535 を超える・ループが長さを超える・終わりからの秒が長さより長い
    let mut long_ms = full_facts();
    long_ms.frame_ms = Some(70_000);
    match engine.instantiate(&base, &sheet_recipe(), &shapes(&sheet), &long_ms) {
        Err(BindError::OutOfRange {
            node,
            param,
            issues,
        }) => {
            assert_eq!((node, param.as_str()), (NodeId(1), "durationMs"));
            assert!(issues.has_errors());
        }
        other => panic!("{other:?}"),
    }
    let mut far_loop = full_facts();
    far_loop.loop_range = Some(kura_engine::ops::audio::set_loop::FrameRange {
        start: 10,
        end: 5_000,
    });
    assert!(matches!(
        engine.instantiate(&base, &loop_recipe(), &shapes(&audio), &far_loop),
        Err(BindError::OutOfRange {
            node: NodeId(2),
            ..
        })
    ));
    let short = audio_input(8_000, 100);
    assert!(matches!(
        engine.instantiate(&base, &audio_recipe(), &shapes(&short), &Facts::default()),
        Err(BindError::OutOfRange {
            node: NodeId(1),
            ..
        })
    ));
    // Invalid：結び付けたノードより前が、このアイテムで推論できない（10×10 を 32×32 で切る）
    let small = InputGen::Noise {
        w: 10,
        h: 10,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    assert!(matches!(
        engine.instantiate(&base, &moved(&[crop], true), &shapes(&small), &full_facts()),
        Err(BindError::Invalid(_))
    ));
    // Kind：N → 1 のレシピ
    assert!(matches!(
        engine.instantiate(&base, &merge_recipe(), &shapes(&sheet), &full_facts()),
        Err(BindError::Kind)
    ));
    // Recipe：規則の版の違う頭・壊したレシピ
    let mut other_rules = base.clone();
    other_rules.rules = doc::RULES + 1;
    assert!(matches!(
        engine.instantiate(
            &other_rules,
            &sheet_recipe(),
            &shapes(&sheet),
            &full_facts()
        ),
        Err(BindError::Recipe(_))
    ));
    let mut broken = sheet_recipe();
    broken.graph.nodes[0].op = "image.future".into();
    assert!(matches!(
        engine.instantiate(&base, &broken, &shapes(&sheet), &full_facts()),
        Err(BindError::Recipe(EngineError::Invalid(_)))
    ));
}

/// 記録の形と、Facts のダイジェスト（同じ値なら同じ、変われば違う、有限でないピボットは断る）。
#[test]
fn bound_records_and_fact_digests() {
    let engine = Engine::new().unwrap();
    let audio = audio_input(8_000, 4_000);
    let inst = engine
        .instantiate(
            &Graph::new_single(),
            &audio_recipe(),
            &shapes(&audio),
            &Facts::default(),
        )
        .unwrap();
    let raw = kura_engine::recipe::bound_json(&inst.bound).unwrap();
    let back: Vec<kura_engine::recipe::BoundValue> = serde_json::from_slice(&raw).unwrap();
    assert_eq!(back, inst.bound);
    assert_eq!(inst.bound.len(), 6);
    assert_eq!(inst.bound[4].part, Some(RangePart::Start));
    assert_eq!(
        inst.bound[0].read,
        json!({ "rate": 8_000, "frames": 4_000 })
    );
    let f = full_facts();
    assert_eq!(f.digest().unwrap(), full_facts().digest().unwrap());
    let mut g = f.clone();
    g.frame_ms = Some(121);
    assert_ne!(f.digest().unwrap(), g.digest().unwrap());
    assert_ne!(f.digest().unwrap(), Facts::default().digest().unwrap());
    g.pivot = Some(PivotFact {
        x: f64::NAN,
        y: 0.0,
    });
    assert!(matches!(g.digest(), Err(EngineError::Invalid(_))));
    // Facts は知らない項目を拒む（kura-core が壊れた欄を None にして渡す）
    assert!(serde_json::from_value::<Facts>(json!({ "grid": null, "tag": 1 })).is_err());
    assert_eq!(
        serde_json::from_value::<Facts>(json!({})).unwrap(),
        Facts::default()
    );
}

// ---------------------------------------------------------------------------------------------
// E50：エディタの経路との一致（E4 の延長）
// ---------------------------------------------------------------------------------------------

/// 描いた出力（値のダイジェストと、固定のコーデックで書いた PNG・WAV のバイト列）。
type Outputs = Vec<(Digest, Vec<u8>)>;

/// エディタの経路：頭を `EditSession` で開き、レシピのノードを結び付けた値で末尾に足して（外したノードは外し、To の形式は
/// 出力に置く）、差分の計画とキャッシュで描く。
fn editor_render(
    engine: &Engine,
    base: &Graph,
    bound: &Graph,
    input: &Value,
    source: Digest,
) -> Result<(Graph, Outputs, Vec<kura_engine::Issue>), EngineError> {
    let mut s = EditSession::open(engine, base, Digest([1; 32]), HistoryLimits::default())?;
    for node in &bound.nodes[base.nodes.len()..] {
        let at = s.head().to_graph().nodes.len();
        let made = s.commit(
            engine,
            Edit::AddNode {
                at,
                op: node.op.clone(),
                v: node.v,
                params: node.params.clone(),
                seed: Some(node.seed),
                extra_inputs: Vec::new(),
            },
            CommitMode::Step,
        )?;
        if !node.enabled {
            s.commit(
                engine,
                Edit::SetEnabled {
                    node: made.created.unwrap(),
                    enabled: false,
                },
                CommitMode::Step,
            )?;
        }
    }
    let out = &bound.outputs[0];
    s.commit(
        engine,
        Edit::SetOutput {
            output: out.id,
            format: out.format.clone(),
            quantize: out.quantize,
            expand: out.expand.clone(),
        },
        CommitMode::Step,
    )?;
    let snap = s.current().clone();
    let shapes = shapes(input);
    let plan = engine
        .compile_snapshot(&snap, &shapes, None)
        .map_err(EngineError::Invalid)?;
    let fp = BTreeMap::from([(InputId(0), FingerprintedInput::new(input.clone(), source)?)]);
    let mut cache = NodeCache::new(1 << 30);
    let got = engine.render_cached(
        &plan,
        &fp,
        &mut cache,
        &RenderOptions {
            digests: true,
            focus: None,
            verify_hits: false,
        },
        &NoMonitor,
    )?;
    let codecs = CodecSet::pinned();
    let outputs = got
        .outputs
        .into_iter()
        .map(|o| (o.digest.unwrap(), encoded(&codecs, &o.value)))
        .collect();
    Ok((snap.to_graph(), outputs, got.warnings))
}

fn encoded(codecs: &CodecSet, v: &Value) -> Vec<u8> {
    let format = match v {
        Value::Audio(_) => "wav",
        _ => "png",
    };
    codecs
        .encode_bytes(v, format, &json!({}), &NoMonitor)
        .map(|(b, _)| b)
        .unwrap_or_default()
}

/// 一括の経路：結び付けた文書を正規の JSON にして、別のエンジンで読み直して描く。
fn bulk_render(
    engine: &Engine,
    bound: &Graph,
    input: &Value,
    source: Digest,
) -> Result<(Outputs, Vec<kura_engine::Issue>), EngineError> {
    let g = engine.load(&bound.to_canonical_json()?)?;
    let plan = engine
        .compile(&g, &shapes(input))
        .map_err(EngineError::Invalid)?;
    let out = engine.render(
        &plan,
        &BTreeMap::from([(
            InputId(0),
            InputValue {
                value: input.clone(),
                source,
            },
        )]),
        &NoMonitor,
    )?;
    let codecs = CodecSet::pinned();
    Ok((
        out.outputs
            .iter()
            .map(|o| (o.digest, encoded(&codecs, &o.value)))
            .collect(),
        out.warnings,
    ))
}

/// 警告のノードを文書の中の位置にする（エディタの手 AddNode は ID を `next_id` から振るので、継いだ文書の ID とずれうる。
/// ノードの鍵に ID は入らないので、値・バイト列は同じ）。
fn positioned(
    g: &Graph,
    warnings: &[kura_engine::Issue],
) -> Vec<(Option<usize>, IssueCode, String)> {
    warnings
        .iter()
        .map(|w| {
            let at = w
                .node
                .and_then(|id| g.nodes.iter().position(|n| n.id == id));
            (at, w.code, w.message.clone())
        })
        .collect()
}

/// ノードの中身（ID と入力を除く）。
fn contents(g: &Graph) -> Vec<(String, u16, J, u64, bool)> {
    g.nodes
        .iter()
        .map(|n| (n.op.clone(), n.v, n.params.clone(), n.seed, n.enabled))
        .collect()
}

/// E50（E4 の延長）：結び付けた文書を一括の経路（正規の JSON・別のエンジン・キャッシュなしの render）で描いた値・PNG と
/// WAV のバイト列・警告が、エディタの経路と同じ。ランダムな頭の文書と、格子・ピボット・ループ・秒・コマの時間を結ぶ決まった
/// レシピ 6 つ。継いだ文書とエディタの文書は、ノード ID を除いて同じ。
#[test]
fn bound_documents_match_the_editor_path() {
    let editor = Engine::new().unwrap();
    let bulk = Engine::new().unwrap();
    let mut r = determinism::rng(0xe50_0004);
    let mut audio_disabled = audio_recipe();
    audio_disabled.graph.nodes[1].enabled = false;
    audio_disabled.bindings.retain(|b| b.node != NodeId(2));
    let mut to_wav = loop_recipe();
    to_wav.graph.outputs[0].format = FormatChoice::To {
        format: "wav".into(),
        options: json!({ "bits": 16 }),
    };
    to_wav.graph.outputs[0].quantize = Some(Quantize {
        v: QUANTIZE_VERSION,
        bits: 16,
        dither: Dither::Tpdf,
        seed: 5,
    });
    let recipes: Vec<(Recipe, InputGen)> = vec![
        (
            sheet_recipe(),
            InputGen::Sheet {
                cols: 4,
                rows: 2,
                cell: 16,
            },
        ),
        (
            set_grid_recipe(),
            InputGen::Sheet {
                cols: 3,
                rows: 3,
                cell: 16,
            },
        ),
        (
            pivot_recipe(),
            InputGen::Noise {
                w: 24,
                h: 16,
                fmt: PixelFormat::Rgba8,
                seed: 4,
            },
        ),
        (
            audio_recipe(),
            InputGen::AudioNoise {
                rate: 22_050,
                channels: 2,
                frames: 30_000,
                seed: 6,
            },
        ),
        (
            audio_disabled,
            InputGen::AudioNoise {
                rate: 44_100,
                channels: 1,
                frames: 40_000,
                seed: 7,
            },
        ),
        (
            to_wav,
            InputGen::AudioNoise {
                rate: 8_000,
                channels: 2,
                frames: 5_000,
                seed: 8,
            },
        ),
    ];
    let mut matched = 0;
    for (k, (recipe, make)) in recipes.iter().enumerate() {
        for round in 0..6 {
            let at = format!("レシピ {k} の {round}");
            let input = make.generate().unwrap();
            let source = Digest(*blake3::hash(format!("{k}-{round}").as_bytes()).as_bytes());
            // 頭の文書：ランダムな編集（素材の設定は頭の座標なので、頭が座標を変えても当てられる）
            let len = below(&mut r, 3) as usize;
            let base = random_chain(&editor, &mut r, &input.shape(), len, false);
            let mut facts = full_facts();
            facts.frame_ms = Some(40 + below(&mut r, 200) as u32);
            facts.pivot = Some(PivotFact {
                x: below(&mut r, 8) as f64 + 0.5,
                y: below(&mut r, 8) as f64,
            });
            if editor.compile(&base, &shapes(&input)).is_err() {
                continue;
            }
            let inst = match editor.instantiate(&base, recipe, &shapes(&input), &facts) {
                Ok(i) => i,
                Err(BindError::Invalid(_) | BindError::OutOfRange { .. }) => continue,
                Err(e) => panic!("{at}：{e}"),
            };
            let (session_doc, ed, ed_warn) =
                match editor_render(&editor, &base, &inst.graph, &input, source) {
                    Ok(x) => x,
                    Err(e) => {
                        // 描けないなら一括の経路も描けない
                        assert!(
                            bulk_render(&bulk, &inst.graph, &input, source).is_err(),
                            "{at}：{e}"
                        );
                        continue;
                    }
                };
            let (bk, bk_warn) = bulk_render(&bulk, &inst.graph, &input, source).unwrap();
            assert_eq!(ed, bk, "{at}：値とバイト列");
            assert_eq!(
                positioned(&session_doc, &ed_warn),
                positioned(&inst.graph, &bk_warn),
                "{at}：警告"
            );
            assert_eq!(
                contents(&session_doc),
                contents(&inst.graph),
                "{at}：ノードの中身"
            );
            assert_eq!(
                session_doc.outputs[0].format, inst.graph.outputs[0].format,
                "{at}"
            );
            matched += 1;
        }
    }
    assert!(matched >= 24, "{matched}");
}

// ---------------------------------------------------------------------------------------------
// エディタの文書からレシピを作る（§1.8。段 7 が使う口）
// ---------------------------------------------------------------------------------------------

/// 外すノード（無効の行・描画）を除き、外したノードを指していた入力・出力は入力 0 に付け替え、ID を 1, 2, … と付け直し、
/// 結び付けも付け直す。形式は選んだときだけ入れる。
#[test]
fn recipes_are_made_from_editor_documents() {
    let engine = Engine::new().unwrap();
    let mut doc = Graph::new_single();
    doc.next_id = 10;
    let a = doc
        .push_linear(
            "sprite.split_grid",
            1,
            json!({ "grid": grid_json(16), "durationMs": 100, "skipEmpty": false }),
            7,
        )
        .unwrap();
    let b = doc
        .push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
        .unwrap();
    let c = doc
        .push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    let d = doc
        .push_linear(
            "sprite.pack_grid",
            1,
            json!({ "columns": 2, "gapX": 0, "gapY": 0, "fill": null }),
            3,
        )
        .unwrap();
    doc.nodes[2].enabled = false;
    doc.outputs[0].format = FormatChoice::To {
        format: "png".into(),
        options: json!({}),
    };
    let keep = |n: &kura_engine::Node| n.enabled && n.op != "paint.actions";
    let bindings = vec![
        bind(a.0, "grid", None, setting(SettingKey::Grid)),
        bind(a.0, "durationMs", None, setting(SettingKey::FrameMs)),
    ];
    let r = Recipe::from_doc(
        "見本から".into(),
        String::new(),
        &doc,
        &keep,
        false,
        bindings.clone(),
    )
    .unwrap();
    assert_eq!(r.graph.nodes.len(), 2);
    assert_eq!(r.graph.nodes[0].id, NodeId(1));
    assert_eq!(r.graph.nodes[0].seed, 7);
    assert_eq!(r.graph.nodes[1].id, NodeId(2));
    assert_eq!(r.graph.nodes[1].op, "sprite.pack_grid");
    assert_eq!(
        r.graph.nodes[1].inputs,
        vec![PortRef::Node { id: NodeId(1) }]
    );
    assert_eq!(r.graph.outputs[0].from, PortRef::Node { id: NodeId(2) });
    assert_eq!(r.graph.next_id, 3);
    assert_eq!(r.graph.outputs[0].format, FormatChoice::Keep);
    assert!(r.bindings.iter().all(|b| b.node == NodeId(1)));
    assert!(engine.check_recipe(&r).unwrap().is_empty());
    let _ = (b, c, d);
    // 形式を含める
    let with = Recipe::from_doc(
        "見本から".into(),
        String::new(),
        &doc,
        &keep,
        true,
        Vec::new(),
    )
    .unwrap();
    assert_eq!(with.graph.outputs[0].format, doc.outputs[0].format);
    // 出力のノードを外すと、出力はその入力 0 へ（続けて外したノードは順にたどる）
    let only_split = |n: &kura_engine::Node| n.op == "sprite.split_grid";
    let r = Recipe::from_doc(
        "x".into(),
        String::new(),
        &doc,
        &only_split,
        false,
        Vec::new(),
    )
    .unwrap();
    assert_eq!(r.graph.outputs[0].from, PortRef::Node { id: NodeId(1) });
    let none = |_: &kura_engine::Node| false;
    let r = Recipe::from_doc("x".into(), String::new(), &doc, &none, false, Vec::new()).unwrap();
    assert_eq!(r.graph.outputs[0].from, PortRef::Input { id: InputId(0) });
    assert_eq!(r.graph.next_id, 1);
    // 外すノードへの結び付けは断る
    assert!(Recipe::from_doc("x".into(), String::new(), &doc, &none, false, bindings).is_err());
    // 見本で当てると、継いだ値のまま描ける
    let input = sheet_input();
    let r = Recipe::from_doc(
        "見本から".into(),
        String::new(),
        &doc,
        &keep,
        false,
        vec![bind(a.0, "grid", None, setting(SettingKey::Grid))],
    )
    .unwrap();
    let inst = engine
        .instantiate(&Graph::new_single(), &r, &shapes(&input), &full_facts())
        .unwrap();
    assert!(render(&engine, &inst.graph, &input, Digest::default()).is_ok());
    let _ = InputSlot {
        id: InputId(0),
        role: InputRole::Primary,
        decode: DecodeOptions::default(),
    };
}
