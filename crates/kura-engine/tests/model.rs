//! glb の読み手と描画（M11 の決定 9。`kura_engine::model`）。
//!
//! - 情報：最小の glb・ノードの変換・描けない印（必須の Draco・スキン）。
//! - 描画：正面の眺めで三角形・ノードの変換・テクスチャ（最近傍）・MASK と BLEND・片面の裏の画素を照らす。
//! - 壊れた入力：ChaCha8 で作った 1 万件（でたらめなバイト列と、見本の glb のバイトの変異・JSON の数の変異）を、ファズの
//!   対象と同じ `testing::fuzz_model` に通す（自前のコードの panic・`Internal` がないこと）。
//! - 上限：JSON の深さ・オブジェクトの数・ファイル・頂点・テクスチャ・出力の大きさ・描く手間。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use kura_engine::model::{
    GlbBuilder, ModelLimits, RenderOptions, ViewDir, glb_bytes, load_scene, read_info, render,
    sample_glbs, thumbnail,
};
use kura_engine::value::Image;
use kura_engine::{EngineError, NoMonitor, Value};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::json;

fn sample(name: &str) -> Vec<u8> {
    sample_glbs()
        .unwrap()
        .into_iter()
        .find(|(n, _)| *n == name)
        .unwrap()
        .1
}

fn front(bytes: &[u8], size: u32) -> std::sync::Arc<Image> {
    let limits = ModelLimits::default();
    let scene = load_scene(bytes, &limits, &NoMonitor).unwrap();
    let v = render(
        &scene,
        &RenderOptions {
            size,
            samples: 1,
            view: ViewDir::Front,
        },
        &limits,
        &NoMonitor,
    )
    .unwrap();
    let Value::Image(img) = v else { panic!() };
    img
}

fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
    img.px.pixel(x, y).unwrap().try_into().unwrap()
}

/// unlit の単色の三角形（(0,0)・(1,0)・(0,1)。`ccw` が偽なら正面から見て時計回り）。
fn unlit_triangle(rgba: [f64; 4], ccw: bool, double_sided: bool) -> Vec<u8> {
    let mut b = GlbBuilder::new();
    let p = if ccw {
        [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0]
    };
    let pos = b.floats(&p, "VEC3");
    let m = b.material(json!({
        "pbrMetallicRoughness": { "baseColorFactor": rgba }, "doubleSided": double_sided,
        "extensions": { "KHR_materials_unlit": {} }
    }));
    let mesh = b.mesh(vec![
        json!({ "attributes": { "POSITION": pos }, "material": m }),
    ]);
    let n = b.node(json!({ "mesh": mesh }));
    b.root(n);
    b.build()
}

#[test]
fn reads_info_from_a_minimal_glb() {
    let info = read_info(&sample("triangle"), &ModelLimits::default()).unwrap();
    assert_eq!((info.vertices, info.triangles, info.parts), (3, 1, 1));
    assert_eq!((info.meshes, info.nodes, info.materials), (1, 1, 0));
    assert_eq!(info.bounds.unwrap().size(), [1.0, 1.0, 0.0]);
    assert_eq!(info.generator.as_deref(), Some("kura-engine GlbBuilder"));
    assert!(info.unsupported.is_empty());

    let cube = read_info(&sample("cube"), &ModelLimits::default()).unwrap();
    assert_eq!((cube.vertices, cube.triangles, cube.materials), (24, 12, 6));
    let b = cube.bounds.unwrap();
    assert_eq!((b.min, b.max), ([-0.5; 3], [0.5; 3]));

    let sf = read_info(&sample("strip_fan_quantized"), &ModelLimits::default()).unwrap();
    // 帯 5 頂点 → 3 枚、扇 4 頂点 → 2 枚
    assert_eq!((sf.vertices, sf.triangles), (9, 5));
    assert_eq!(sf.extensions_required, ["KHR_mesh_quantization"]);
    assert!(sf.unsupported.is_empty());
}

/// 最小の glb の三角形の画素：正面・64px・超標本化なしで、左下の半分が unlit の赤、右上と余白が透明。
#[test]
fn renders_a_triangle() {
    let img = front(&unlit_triangle([1.0, 0.0, 0.0, 1.0], true, false), 64);
    assert_eq!((img.px.width(), img.px.height()), (64, 64));
    assert_eq!(px(&img, 10, 50), [255, 0, 0, 255]);
    assert_eq!(px(&img, 3, 61), [255, 0, 0, 255]);
    assert_eq!(px(&img, 50, 10)[3], 0);
    assert_eq!(px(&img, 0, 63)[3], 0, "余白");
    assert_eq!(px(&img, 63, 0)[3], 0);
    // 陰影つき（材質なしの白）でも同じ形で、色は灰色（環境と向きの光の和）
    let lit = front(&sample("triangle"), 64);
    let c = px(&lit, 10, 50);
    assert_eq!(c[3], 255);
    assert!(
        c[0] == c[1] && c[1] == c[2] && c[0] > 150 && c[0] < 255,
        "{c:?}"
    );
}

/// ノードの変換を頂点に焼き込む：右へ 2 動かした実体と、親の移動 (0, 2) と子の z 軸の 90 度の回転。範囲は x −1〜3・y 0〜3。
#[test]
fn bakes_node_transforms() {
    let bytes = sample("nodes");
    let info = read_info(&bytes, &ModelLimits::default()).unwrap();
    let b = info.bounds.unwrap();
    for k in 0..3 {
        assert!((b.min[k] - [-1.0, 0.0, 0.0][k]).abs() < 1e-9, "{b:?}");
        assert!((b.max[k] - [3.0, 3.0, 0.0][k]).abs() < 1e-9, "{b:?}");
    }
    assert_eq!((info.vertices, info.triangles, info.parts), (6, 2, 2));
    // 64px：範囲 4×3 を 60px に（15 px/単位）、余白 2 → 64×49
    let img = front(&bytes, 64);
    assert_eq!((img.px.width(), img.px.height()), (64, 49));
    let green = |c: [u8; 4]| c[3] == 255 && c[1] > c[0] && c[1] > c[2];
    assert!(green(px(&img, 50, 44)), "右の実体 {:?}", px(&img, 50, 44));
    assert!(green(px(&img, 14, 14)), "回した子 {:?}", px(&img, 14, 14));
    assert_eq!(px(&img, 5, 5)[3], 0);
    assert_eq!(px(&img, 40, 10)[3], 0);
    assert_eq!(px(&img, 14, 40)[3], 0);
}

/// ノードの輪・共有の子・範囲の外の番号でも止まらず、同じノードは 1 度だけ描く。
#[test]
fn node_cycles_and_bad_indices_are_cut() {
    let base = json!({
        "asset": { "version": "2.0" },
        "scenes": [{ "nodes": [0, 0, 99] }],
        "nodes": [
            { "children": [1, 1, 0, 7], "mesh": 0 },
            { "children": [0, 1], "mesh": 0, "translation": [2.0, 0.0, 0.0] }
        ],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "accessors": [{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                        "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] }],
        "bufferViews": [{ "buffer": 0, "byteLength": 36 }],
        "buffers": [{ "byteLength": 36 }]
    });
    let bin: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let bytes = glb_bytes(&base, &bin);
    let info = read_info(&bytes, &ModelLimits::default()).unwrap();
    assert_eq!((info.parts, info.triangles), (2, 2));
    let scene = load_scene(&bytes, &ModelLimits::default(), &NoMonitor).unwrap();
    assert_eq!(scene.triangle_count(), 2);
}

/// テクスチャ（2×2 の市松模様・最近傍・unlit）：四角の左上と右下が赤、右上と左下が青。
#[test]
fn renders_embedded_textures() {
    let img = front(&sample("textured_quad"), 64);
    let (red, blue) = ([255, 0, 0, 255], [0, 0, 255, 255]);
    assert_eq!(px(&img, 10, 10), red);
    assert_eq!(px(&img, 50, 10), blue);
    assert_eq!(px(&img, 10, 50), blue);
    assert_eq!(px(&img, 50, 50), red);
    // 16 ビットの PNG と KHR_texture_transform も読める
    let scene = load_scene(
        &sample("texture_transform"),
        &ModelLimits::default(),
        &NoMonitor,
    )
    .unwrap();
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
}

/// MASK（頂点の色のアルファが cutoff より小さい下の側を捨てる）と BLEND（半透明の赤を手前に重ねる）。
#[test]
fn alpha_modes_mask_and_blend() {
    let img = front(&sample("alpha"), 64);
    // 範囲 2×2 を 60px に（30 px/単位）、余白 2
    let top = px(&img, 9, 9);
    assert_eq!(top[3], 255);
    assert!(top[2] == 255 && top[0] < 150, "MASK の残った側 {top:?}");
    assert_eq!(px(&img, 9, 54)[3], 0, "MASK で捨てた側");
    let over = px(&img, 32, 32);
    assert_eq!(over[3], 255);
    assert!(
        over[0] > 150 && over[2] > 150,
        "青の上に半透明の赤 {over:?}"
    );
    let alone = px(&img, 32, 44);
    assert!(
        (126..=130).contains(&alone[3]),
        "透明の上の半透明 {alone:?}"
    );
    assert_eq!(&alone[..3], &[255, 0, 0]);
}

/// 片面の材質は裏から見えない（正面から見て時計回り）。両面なら見える。
#[test]
fn single_sided_back_faces_are_culled() {
    let back = front(&unlit_triangle([0.0, 1.0, 0.0, 1.0], false, false), 32);
    assert!(back.px.to_rows().unwrap().chunks(4).all(|p| p[3] == 0));
    let both = front(&unlit_triangle([0.0, 1.0, 0.0, 1.0], false, true), 32);
    assert_eq!(px(&both, 5, 25), [0, 255, 0, 255]);
}

/// 見本はどれも、3/4 の眺めのサムネイルが描け、何度描いても同じ値になる。
#[test]
fn samples_render_deterministically() {
    for (name, bytes) in sample_glbs().unwrap() {
        let a = thumbnail(&bytes, 128, &ModelLimits::default(), &NoMonitor)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let b = thumbnail(&bytes, 128, &ModelLimits::default(), &NoMonitor).unwrap();
        assert_eq!(a.digest().unwrap(), b.digest().unwrap(), "{name}");
        let Value::Image(img) = a else { panic!() };
        assert_eq!(img.px.width().max(img.px.height()), 128, "{name}");
        let rows = img.px.to_rows().unwrap();
        assert!(
            rows.chunks(4).any(|p| p[3] == 255),
            "{name}: 何も描かれていない"
        );
        assert_eq!(&rows[..4], &[0, 0, 0, 0], "{name}: 角は透明");
    }
}

/// 必須の Draco・meshopt・知らない拡張・スキンは、情報だけ出して描かない（決定 9）。必須でない Draco は、圧縮していない
/// accessor で描く。
#[test]
fn unsupported_models_are_marked_and_not_rendered() {
    let tri = |extra: serde_json::Value| {
        let mut b = GlbBuilder::new();
        let pos = b.floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0], "VEC3");
        let mesh = b.mesh(vec![json!({ "attributes": { "POSITION": pos },
            "extensions": { "KHR_draco_mesh_compression": { "bufferView": 0, "attributes": { "POSITION": 0 } } } })]);
        let n = b.node(json!({ "mesh": mesh }));
        b.root(n);
        for (k, v) in extra.as_object().unwrap() {
            b.set(k, v.clone());
        }
        b.build()
    };
    let limits = ModelLimits::default();
    for (extra, mark) in [
        (
            json!({ "extensionsRequired": ["KHR_draco_mesh_compression"] }),
            "KHR_draco_mesh_compression",
        ),
        (
            json!({ "extensionsRequired": ["EXT_meshopt_compression"] }),
            "EXT_meshopt_compression",
        ),
        (
            json!({ "extensionsRequired": ["KHR_future_thing"] }),
            "KHR_future_thing",
        ),
        (json!({ "skins": [{ "joints": [0] }] }), "skin"),
    ] {
        let bytes = tri(extra);
        let info = read_info(&bytes, &limits).unwrap();
        assert_eq!(info.unsupported, [mark]);
        assert!(matches!(
            load_scene(&bytes, &limits, &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
        assert!(matches!(
            thumbnail(&bytes, 64, &limits, &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
    }
    let used_only = tri(json!({ "extensionsUsed": ["KHR_draco_mesh_compression"] }));
    assert!(
        read_info(&used_only, &limits)
            .unwrap()
            .unsupported
            .is_empty()
    );
    assert!(thumbnail(&used_only, 32, &limits, &NoMonitor).is_ok());
    // 材質の拡張は必須でも基本色で描く
    let material_ext = tri(json!({ "extensionsRequired": ["KHR_materials_transmission"] }));
    assert!(thumbnail(&material_ext, 32, &limits, &NoMonitor).is_ok());
}

/// 上限：JSON の深さ・オブジェクトの数・ファイル・頂点・三角形・テクスチャ（飛ばして描く）・出力の大きさ・描く手間。
#[test]
fn limits_are_checked_before_allocating() {
    let limits = ModelLimits::default();
    let tri = sample("triangle");
    // JSON の深さ：根のオブジェクトと extras の配列 63 段で 64 段は読み、64 段（合わせて 65 段）は断る
    let nested = |n: usize| {
        let text = format!(
            r#"{{"asset":{{"version":"2.0"}},"extras":{}{}}}"#,
            "[".repeat(n),
            "]".repeat(n)
        );
        glb_bytes(
            &serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            &[],
        )
    };
    assert!(read_info(&nested(63), &limits).is_ok());
    assert!(matches!(
        read_info(&nested(64), &limits),
        Err(EngineError::Limit(_))
    ));
    // オブジェクトの数
    let many = glb_bytes(
        &json!({ "asset": { "version": "2.0" }, "extras": vec![json!({}); 200] }),
        &[],
    );
    let few = ModelLimits {
        max_json_objects: 100,
        ..limits.clone()
    };
    assert!(matches!(read_info(&many, &few), Err(EngineError::Limit(_))));
    // ファイルの大きさ・頂点・三角形
    let tiny = ModelLimits {
        max_file_bytes: 64,
        ..limits.clone()
    };
    assert!(matches!(read_info(&tri, &tiny), Err(EngineError::Limit(_))));
    let two = ModelLimits {
        max_vertices: 2,
        ..limits.clone()
    };
    assert!(matches!(
        load_scene(&tri, &two, &NoMonitor),
        Err(EngineError::Limit(_))
    ));
    let none = ModelLimits {
        max_triangles: 0,
        ..limits.clone()
    };
    assert!(matches!(
        load_scene(&tri, &none, &NoMonitor),
        Err(EngineError::Limit(_))
    ));
    // テクスチャが大きすぎれば飛ばして基本色で描く
    let small_tex = ModelLimits {
        max_texture_pixels: 3,
        ..limits.clone()
    };
    let scene = load_scene(&sample("textured_quad"), &small_tex, &NoMonitor).unwrap();
    assert_eq!(scene.warnings.len(), 1, "{:?}", scene.warnings);
    // 出力の大きさ・超標本化
    let scene = load_scene(&tri, &limits, &NoMonitor).unwrap();
    for (size, samples) in [(0, 1), (limits.max_output_dim + 1, 1), (32, 0), (32, 5)] {
        let opts = RenderOptions {
            size,
            samples,
            view: ViewDir::Front,
        };
        assert!(matches!(
            render(&scene, &opts, &limits, &NoMonitor),
            Err(EngineError::Limit(_))
        ));
    }
    // 描く手間：画面いっぱいの四角を 25 枚（囲む矩形が画面いっぱいの三角形 50 枚）重ねる。既定の 64 倍には収まり、20 倍は超える
    let mut b = GlbBuilder::new();
    let mut p = Vec::new();
    let mut idx = Vec::new();
    for i in 0..25u32 {
        let z = i as f32 * 0.01;
        p.extend_from_slice(&[0.0, 0.0, z, 1.0, 0.0, z, 1.0, 1.0, z, 0.0, 1.0, z]);
        idx.extend_from_slice(&[i * 4, i * 4 + 1, i * 4 + 2, i * 4, i * 4 + 2, i * 4 + 3]);
    }
    let pos = b.floats(&p, "VEC3");
    let ia = b.indices32(&idx);
    let mesh = b.mesh(vec![
        json!({ "attributes": { "POSITION": pos }, "indices": ia }),
    ]);
    let n = b.node(json!({ "mesh": mesh }));
    b.root(n);
    let layered = load_scene(&b.build(), &limits, &NoMonitor).unwrap();
    let opts = RenderOptions {
        size: 64,
        samples: 1,
        view: ViewDir::Front,
    };
    assert!(render(&layered, &opts, &limits, &NoMonitor).is_ok());
    let thin = ModelLimits {
        max_overdraw: 20,
        ..limits.clone()
    };
    assert!(matches!(
        render(&layered, &opts, &thin, &NoMonitor),
        Err(EngineError::Limit(_))
    ));
}

/// 見積もり（予算の確保に使う）は、焼き込んだ場面と描いた画像より小さくない。
#[test]
fn estimates_cover_the_scene_and_the_output() {
    let limits = ModelLimits::default();
    for (name, bytes) in sample_glbs().unwrap() {
        let info = read_info(&bytes, &limits).unwrap();
        let scene = load_scene(&bytes, &limits, &NoMonitor).unwrap();
        assert!(info.estimate_load_bytes(&limits) >= scene.bytes(), "{name}");
        let opts = RenderOptions {
            size: 256,
            samples: 2,
            view: ViewDir::ThreeQuarter,
        };
        let est = scene.estimate_render(&opts);
        assert!(est.shared >= 256 * 256 * 4 && est.per_task > 0, "{name}");
        let bigger = scene.estimate_render(&RenderOptions { size: 512, ..opts });
        assert!(
            bigger.shared > est.shared && bigger.per_task > est.per_task,
            "{name}"
        );
    }
}

/// 中止すると `Canceled`。
#[test]
fn render_can_be_canceled() {
    let flag = kura_engine::monitor::CancelFlag::default();
    flag.cancel();
    let scene = load_scene(&sample("cube"), &ModelLimits::default(), &NoMonitor).unwrap();
    let opts = RenderOptions {
        size: 64,
        samples: 2,
        view: ViewDir::ThreeQuarter,
    };
    assert!(matches!(
        render(&scene, &opts, &ModelLimits::default(), &flag),
        Err(EngineError::Canceled)
    ));
}

// ---- 壊れた入力 ----

const INTERESTING: [u32; 9] = [
    0,
    1,
    3,
    0x7F,
    0xFF,
    0xFFFF,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
];

/// バイトの変異（ビットの反転・書き換え・境界の値・挿入・削除・切り詰め）。
fn mutate_bytes(r: &mut ChaCha8Rng, seed: &[u8]) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..1 + r.next_u32() % 4 {
        if b.is_empty() {
            b.push(0);
        }
        let at = r.next_u32() as usize % b.len();
        match r.next_u32() % 6 {
            0 => b[at] ^= 1 << (r.next_u32() % 8),
            1 => b[at] = r.next_u32() as u8,
            2 => {
                let v = INTERESTING[r.next_u32() as usize % INTERESTING.len()].to_le_bytes();
                for (k, x) in v.iter().enumerate() {
                    if let Some(c) = b.get_mut(at + k) {
                        *c = *x;
                    }
                }
            }
            3 => {
                let n = 1 + r.next_u32() as usize % 8;
                let ins: Vec<u8> = (0..n).map(|_| r.next_u32() as u8).collect();
                b.splice(at..at, ins);
            }
            4 => {
                let n = (1 + r.next_u32() as usize % 16).min(b.len() - at);
                b.drain(at..at + n);
            }
            _ => b.truncate(at.max(1)),
        }
    }
    b
}

/// JSON の数の変異：見本の glb の JSON の中の数（番号・数・場所・間隔・成分の型・行列）を、境界の値やでたらめな値に変え、
/// 同じ BIN で組み直す（JSON のまま壊れずに、読み手の深い所まで届く）。
fn mutate_json(r: &mut ChaCha8Rng, seed: &[u8]) -> Vec<u8> {
    let jlen = u32::from_le_bytes(seed[12..16].try_into().unwrap()) as usize;
    let mut json: serde_json::Value = serde_json::from_slice(&seed[20..20 + jlen]).unwrap();
    let bin = seed.get(20 + jlen + 8..).unwrap_or(&[]).to_vec();
    fn count(v: &serde_json::Value) -> usize {
        match v {
            serde_json::Value::Number(_) => 1,
            serde_json::Value::Array(a) => a.iter().map(count).sum(),
            serde_json::Value::Object(o) => o.values().map(count).sum(),
            _ => 0,
        }
    }
    /// k 番めの数を `new` に替える（先頭から深さ優先に数える）。
    fn replace(v: &mut serde_json::Value, k: &mut usize, new: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Number(_) => {
                if *k == 0 {
                    *v = new.clone();
                    return true;
                }
                *k -= 1;
                false
            }
            serde_json::Value::Array(a) => a.iter_mut().any(|x| replace(x, k, new)),
            serde_json::Value::Object(o) => o.values_mut().any(|x| replace(x, k, new)),
            _ => false,
        }
    }
    for _ in 0..1 + r.next_u32() % 3 {
        let n = count(&json);
        if n == 0 {
            break;
        }
        let mut k = r.next_u32() as usize % n;
        let new = match r.next_u32() % 5 {
            0 => json!(INTERESTING[r.next_u32() as usize % INTERESTING.len()]),
            1 => json!(r.next_u32() % 64),
            2 => json!(-(i64::from(r.next_u32() % 1000))),
            3 => json!(f64::from(r.next_u32()) * 1e-3 - 1e6),
            _ => json!([1e300, -1e300, 0.5, 5121.0][r.next_u32() as usize % 4]),
        };
        replace(&mut json, &mut k, &new);
    }
    glb_bytes(&json, &bin)
}

/// でたらめなバイト列（glb の見出しで始まるものを混ぜる）。
fn random_bytes(r: &mut ChaCha8Rng) -> Vec<u8> {
    let n = r.next_u32() as usize % 600;
    let mut b: Vec<u8> = (0..n).map(|_| r.next_u32() as u8).collect();
    if r.next_u32().is_multiple_of(2) && b.len() >= 20 {
        b[..4].copy_from_slice(b"glTF");
        b[4..8].copy_from_slice(&2u32.to_le_bytes());
        let total = b.len() as u32;
        b[8..12].copy_from_slice(&total.to_le_bytes());
        b[16..20].copy_from_slice(b"JSON");
    }
    b
}

/// 壊れた入力で panic しない：ChaCha8 の 1 万件（でたらめ・バイトの変異・JSON の数の変異）と、見本そのもの。
#[test]
fn rejects_bad_input_without_panicking() {
    let seeds = sample_glbs().unwrap();
    for (name, s) in &seeds {
        kura_engine::testing::fuzz_model(s).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let mut r = kura_engine::determinism::rng(0x6C_62_67);
    let mut kinds = [0usize; 3];
    // 焼き込みまで届いた数（変異が読み手の深い所を通ること）
    let mut reached = [0usize; 3];
    for i in 0..10_000 {
        let kind = i % 3;
        let seed = &seeds[r.next_u32() as usize % seeds.len()].1;
        let input = match kind {
            0 => random_bytes(&mut r),
            1 => mutate_bytes(&mut r, seed),
            _ => mutate_json(&mut r, seed),
        };
        kinds[kind] += 1;
        let limits = ModelLimits::fuzz();
        if load_scene(&input, &limits, &NoMonitor).is_ok() {
            reached[kind] += 1;
        }
        if let Err(e) = kura_engine::testing::fuzz_model(&input) {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/fuzz-smoke");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("model_glb-{i}.glb"));
            let _ = std::fs::write(&path, &input);
            panic!("{i}（{kind}）: {e}（{} に書いた）", path.display());
        }
    }
    assert_eq!(kinds.iter().sum::<usize>(), 10_000);
    eprintln!("焼き込みまで届いた数（でたらめ・バイト・JSON）: {reached:?}");
    assert!(reached[1] > 100 && reached[2] > 1_000, "{reached:?}");
}
