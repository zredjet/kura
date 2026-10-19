//! テスト・ゴールデン・ファズの種のための glb の組み立て（決定的。下の層で組む）。

use serde_json::{Map, Value as Json, json};

use crate::error::EngineError;
use crate::testfiles::FileGen;

/// glTF の JSON と BIN の塊から glb を組む（JSON は空白で、BIN は 0 で 4 バイトに揃える。BIN が空なら塊を書かない）。
pub fn glb_bytes(json: &Json, bin: &[u8]) -> Vec<u8> {
    let mut j = json.to_string().into_bytes();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let mut b = bin.to_vec();
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
    let total = 12 + 8 + j.len() + if b.is_empty() { 0 } else { 8 + b.len() };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&j);
    if !b.is_empty() {
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&b);
    }
    out
}

/// glb を組む道具。accessor・画像などを足すと番号を返す。
#[derive(Debug, Clone, Default)]
pub struct GlbBuilder {
    bin: Vec<u8>,
    views: Vec<Json>,
    accessors: Vec<Json>,
    meshes: Vec<Json>,
    nodes: Vec<Json>,
    materials: Vec<Json>,
    textures: Vec<Json>,
    images: Vec<Json>,
    samplers: Vec<Json>,
    roots: Vec<u32>,
    top: Map<String, Json>,
}

impl GlbBuilder {
    /// 空の組み立て。
    pub fn new() -> Self {
        Self::default()
    }

    fn view(&mut self, bytes: &[u8]) -> u32 {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.views
            .push(json!({ "buffer": 0, "byteOffset": offset, "byteLength": bytes.len() }));
        self.views.len() as u32 - 1
    }

    fn accessor(&mut self, a: Json) -> u32 {
        self.accessors.push(a);
        self.accessors.len() as u32 - 1
    }

    /// f32 の accessor（`ty` は SCALAR・VEC2・VEC3・VEC4。min・max を付ける）。
    pub fn floats(&mut self, data: &[f32], ty: &str) -> u32 {
        let comps = match ty {
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => 1,
        };
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes);
        let mut lo = vec![f64::INFINITY; comps];
        let mut hi = vec![f64::NEG_INFINITY; comps];
        for (i, v) in data.iter().enumerate() {
            let k = i % comps;
            lo[k] = crate::math::min(lo[k], f64::from(*v));
            hi[k] = crate::math::max(hi[k], f64::from(*v));
        }
        self.accessor(json!({
            "bufferView": view, "componentType": 5126, "count": data.len() / comps, "type": ty,
            "min": lo, "max": hi,
        }))
    }

    /// 正規化した u16 の accessor（KHR_mesh_quantization の位置・色など。min・max は整数の値）。
    pub fn u16_normalized(&mut self, data: &[u16], ty: &str) -> u32 {
        let comps: usize = match ty {
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => 1,
        };
        let mut bytes: Vec<u8> = Vec::new();
        // 要素ごとに 4 バイトに揃える（VEC3 の u16 は 6 バイトなので、間隔 8 で並べる）
        let stride = (comps * 2).div_ceil(4) * 4;
        for chunk in data.chunks(comps) {
            let start = bytes.len();
            for v in chunk {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            bytes.resize(start + stride, 0);
        }
        let view = self.view(&bytes);
        if let Some(v) = self.views.last_mut() {
            v["byteStride"] = json!(stride);
        }
        let mut lo = vec![u16::MAX; comps];
        let mut hi = vec![0u16; comps];
        for (i, v) in data.iter().enumerate() {
            lo[i % comps] = lo[i % comps].min(*v);
            hi[i % comps] = hi[i % comps].max(*v);
        }
        self.accessor(json!({
            "bufferView": view, "componentType": 5123, "normalized": true, "count": data.len() / comps,
            "type": ty, "min": lo, "max": hi,
        }))
    }

    /// u16 の索引。
    pub fn indices16(&mut self, idx: &[u16]) -> u32 {
        let bytes: Vec<u8> = idx.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes);
        self.accessor(json!({ "bufferView": view, "componentType": 5123, "count": idx.len(), "type": "SCALAR" }))
    }

    /// u32 の索引。
    pub fn indices32(&mut self, idx: &[u32]) -> u32 {
        let bytes: Vec<u8> = idx.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes);
        self.accessor(json!({ "bufferView": view, "componentType": 5125, "count": idx.len(), "type": "SCALAR" }))
    }

    /// 埋め込みの画像。
    pub fn image(&mut self, bytes: &[u8], mime: &str) -> u32 {
        let view = self.view(bytes);
        self.images
            .push(json!({ "bufferView": view, "mimeType": mime }));
        self.images.len() as u32 - 1
    }

    /// テクスチャ（`sampler` は sampler の JSON。None なら既定）。
    pub fn texture(&mut self, image: u32, sampler: Option<Json>) -> u32 {
        let mut t = json!({ "source": image });
        if let Some(s) = sampler {
            self.samplers.push(s);
            t["sampler"] = json!(self.samplers.len() - 1);
        }
        self.textures.push(t);
        self.textures.len() as u32 - 1
    }

    /// 材質。
    pub fn material(&mut self, m: Json) -> u32 {
        self.materials.push(m);
        self.materials.len() as u32 - 1
    }

    /// メッシュ。
    pub fn mesh(&mut self, primitives: Vec<Json>) -> u32 {
        self.meshes.push(json!({ "primitives": primitives }));
        self.meshes.len() as u32 - 1
    }

    /// ノード。
    pub fn node(&mut self, n: Json) -> u32 {
        self.nodes.push(n);
        self.nodes.len() as u32 - 1
    }

    /// シーンの根に置く。
    pub fn root(&mut self, node: u32) {
        self.roots.push(node);
    }

    /// 最上位の項目（extensionsRequired・skins・animations など）を置く。
    pub fn set(&mut self, key: &str, v: Json) {
        self.top.insert(key.to_string(), v);
    }

    /// glTF の JSON。
    pub fn json(&self) -> Json {
        let mut root = Map::new();
        root.insert(
            "asset".into(),
            json!({ "version": "2.0", "generator": "kura-engine GlbBuilder" }),
        );
        root.insert("scene".into(), json!(0));
        root.insert("scenes".into(), json!([{ "nodes": self.roots }]));
        for (key, list) in [
            ("nodes", &self.nodes),
            ("meshes", &self.meshes),
            ("accessors", &self.accessors),
            ("bufferViews", &self.views),
            ("materials", &self.materials),
            ("textures", &self.textures),
            ("images", &self.images),
            ("samplers", &self.samplers),
        ] {
            if !list.is_empty() {
                root.insert(key.into(), Json::Array(list.clone()));
            }
        }
        if !self.bin.is_empty() {
            root.insert("buffers".into(), json!([{ "byteLength": self.bin.len() }]));
        }
        for (k, v) in &self.top {
            root.insert(k.clone(), v.clone());
        }
        Json::Object(root)
    }

    /// glb のバイト列。
    pub fn build(&self) -> Vec<u8> {
        glb_bytes(&self.json(), &self.bin)
    }
}

/// 2×2 の市松模様の PNG（左上と右下が赤、ほかが青。RGBA8）。
fn checker_png() -> Result<Vec<u8>, EngineError> {
    use crate::value::{Image, ImageMeta, PixelFormat, Raster, Value};
    let px = [
        255, 0, 0, 255, 0, 0, 255, 255, //
        0, 0, 255, 255, 255, 0, 0, 255,
    ];
    let v = Value::Image(std::sync::Arc::new(Image {
        px: Raster::from_rows(2, 2, PixelFormat::Rgba8, None, &px)?,
        meta: ImageMeta::default(),
    }));
    Ok(crate::codec::CodecSet::pinned()
        .encode_bytes(&v, "png", &json!({}), &crate::monitor::NoMonitor)?
        .0)
}

/// 見本の glb（ゴールデン・ファズの種・テスト）。名前と中身。追記だけ（名前と作り方を変えない）。
pub fn sample_glbs() -> Result<Vec<(&'static str, Vec<u8>)>, EngineError> {
    let mut out = Vec::new();
    let unlit = |rgba: [f64; 4]| json!({ "pbrMetallicRoughness": { "baseColorFactor": rgba }, "extensions": { "KHR_materials_unlit": {} } });

    // 三角形 1 つ（位置だけ・材質なし）
    let mut b = GlbBuilder::new();
    let pos = b.floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0], "VEC3");
    let mesh = b.mesh(vec![json!({ "attributes": { "POSITION": pos } })]);
    let n = b.node(json!({ "mesh": mesh }));
    b.root(n);
    out.push(("triangle", b.build()));

    // 立方体（面ごとの頂点・法線・面ごとの色の材質 6 つ・u16 の索引）
    let mut b = GlbBuilder::new();
    // 面ごとに（法線、横の向き、縦の向き、色）
    type Face = ([f32; 3], [f32; 3], [f32; 3], [f64; 4]);
    let faces: [Face; 6] = [
        (
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.9, 0.2, 0.2, 1.0],
        ),
        (
            [0.0, 0.0, -1.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.2, 0.9, 0.2, 1.0],
        ),
        (
            [1.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [0.0, 1.0, 0.0],
            [0.2, 0.2, 0.9, 1.0],
        ),
        (
            [-1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0],
            [0.9, 0.9, 0.2, 1.0],
        ),
        (
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [0.2, 0.9, 0.9, 1.0],
        ),
        (
            [0.0, -1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.9, 0.2, 0.9, 1.0],
        ),
    ];
    let mut prims = Vec::new();
    for (n, u, v, color) in faces {
        let mut p = Vec::new();
        let mut nn = Vec::new();
        for (su, sv) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            for k in 0..3 {
                p.push(n[k] * 0.5 + u[k] * su * 0.5 + v[k] * sv * 0.5);
            }
            nn.extend_from_slice(&n);
        }
        let pa = b.floats(&p, "VEC3");
        let na = b.floats(&nn, "VEC3");
        let ia = b.indices16(&[0, 1, 2, 0, 2, 3]);
        let m = b.material(json!({ "pbrMetallicRoughness": { "baseColorFactor": color } }));
        prims.push(
            json!({ "attributes": { "POSITION": pa, "NORMAL": na }, "indices": ia, "material": m }),
        );
    }
    let mesh = b.mesh(prims);
    let n = b.node(json!({ "mesh": mesh }));
    b.root(n);
    out.push(("cube", b.build()));

    // テクスチャの四角（2×2 の市松模様・最近傍・UV・unlit・両面）
    let mut b = GlbBuilder::new();
    let pa = b.floats(
        &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0],
        "VEC3",
    );
    let ua = b.floats(&[0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0], "VEC2");
    let ia = b.indices16(&[0, 1, 2, 0, 2, 3]);
    let img = b.image(&checker_png()?, "image/png");
    let tex = b.texture(img, Some(json!({ "magFilter": 9728, "minFilter": 9728 })));
    let m = b.material(json!({
        "pbrMetallicRoughness": { "baseColorTexture": { "index": tex } },
        "doubleSided": true, "extensions": { "KHR_materials_unlit": {} }
    }));
    let mesh = b.mesh(vec![
        json!({ "attributes": { "POSITION": pa, "TEXCOORD_0": ua }, "indices": ia, "material": m }),
    ]);
    let n = b.node(json!({ "mesh": mesh }));
    b.root(n);
    b.set("extensionsUsed", json!(["KHR_materials_unlit"]));
    out.push(("textured_quad", b.build()));

    // ノードの木（同じメッシュを 2 つの実体で：右へ 2 動かしたものと、親の移動と子の z 軸の 90 度の回転）
    let mut b = GlbBuilder::new();
    let pos = b.floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0], "VEC3");
    let m = b.material(unlit([0.1, 0.8, 0.3, 1.0]));
    let mesh = b.mesh(vec![
        json!({ "attributes": { "POSITION": pos }, "material": m }),
    ]);
    let a = b.node(json!({ "mesh": mesh, "translation": [2.0, 0.0, 0.0] }));
    let child = b.node(json!({ "mesh": mesh, "rotation": [0.0, 0.0, std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2] }));
    let parent = b.node(
        json!({ "children": [child], "translation": [0.0, 2.0, 0.0], "scale": [1.0, 1.0, 1.0] }),
    );
    b.root(a);
    b.root(parent);
    out.push(("nodes", b.build()));

    // 透明：MASK（頂点の色のアルファで半分を捨てる）と BLEND（半透明の赤）を重ねる
    let mut b = GlbBuilder::new();
    let pa = b.floats(
        &[0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 2.0, 2.0, 0.0, 0.0, 2.0, 0.0],
        "VEC3",
    );
    let ca = b.floats(
        &[
            1.0, 1.0, 1.0, 0.2, 1.0, 1.0, 1.0, 0.2, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        ],
        "VEC4",
    );
    let ia = b.indices16(&[0, 1, 2, 0, 2, 3]);
    let mask = b.material(json!({
        "pbrMetallicRoughness": { "baseColorFactor": [0.2, 0.4, 1.0, 1.0] }, "alphaMode": "MASK", "alphaCutoff": 0.5,
        "extensions": { "KHR_materials_unlit": {} }
    }));
    let pb = b.floats(
        &[0.5, 0.5, 0.5, 1.5, 0.5, 0.5, 1.5, 1.5, 0.5, 0.5, 1.5, 0.5],
        "VEC3",
    );
    let blend = b.material(json!({
        "pbrMetallicRoughness": { "baseColorFactor": [1.0, 0.0, 0.0, 0.5] }, "alphaMode": "BLEND",
        "extensions": { "KHR_materials_unlit": {} }
    }));
    let mesh = b.mesh(vec![
        json!({ "attributes": { "POSITION": pa, "COLOR_0": ca }, "indices": ia, "material": mask }),
        json!({ "attributes": { "POSITION": pb }, "indices": ia, "material": blend }),
    ]);
    let n = b.node(json!({ "mesh": mesh }));
    b.root(n);
    out.push(("alpha", b.build()));

    // 帯と扇（索引なし）と、KHR_mesh_quantization の正規化した u16 の位置
    let mut b = GlbBuilder::new();
    let strip = b.floats(
        &[
            0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 2.0, 0.0, 0.0,
        ],
        "VEC3",
    );
    let fan = b.u16_normalized(
        &[0, 0, 0, 65535, 0, 0, 65535, 65535, 0, 0, 65535, 0],
        "VEC3",
    );
    let m = b.material(unlit([0.8, 0.5, 0.1, 1.0]));
    let mesh = b.mesh(vec![
        json!({ "attributes": { "POSITION": strip }, "mode": 5, "material": m }),
        json!({ "attributes": { "POSITION": fan }, "mode": 6, "material": m }),
    ]);
    let s = b.node(json!({ "mesh": mesh }));
    b.root(s);
    b.set(
        "extensionsUsed",
        json!(["KHR_mesh_quantization", "KHR_materials_unlit"]),
    );
    b.set("extensionsRequired", json!(["KHR_mesh_quantization"]));
    out.push(("strip_fan_quantized", b.build()));

    // 埋め込みの PNG（下の層で組んだ 16 ビットの RGBA）のテクスチャと KHR_texture_transform（2 倍に繰り返す）
    let mut b = GlbBuilder::new();
    let pa = b.floats(
        &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0],
        "VEC3",
    );
    let ua = b.floats(&[0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0], "VEC2");
    let ia = b.indices32(&[0, 1, 2, 0, 2, 3]);
    let png16 = FileGen::Png {
        w: 5,
        h: 3,
        color: 6,
        depth: 16,
        interlace: false,
        palette_len: 0,
        trns: None,
        text: vec![],
        icc: None,
        phys: None,
        seed: 3,
        exif: None,
    }
    .bytes()?;
    let img = b.image(&png16, "image/png");
    let tex = b.texture(img, None);
    let m = b.material(json!({
        "pbrMetallicRoughness": { "baseColorTexture": { "index": tex,
            "extensions": { "KHR_texture_transform": { "scale": [2.0, 2.0], "rotation": 0.25 } } } }
    }));
    let mesh = b.mesh(vec![
        json!({ "attributes": { "POSITION": pa, "TEXCOORD_0": ua }, "indices": ia, "material": m }),
    ]);
    let n = b.node(json!({ "mesh": mesh, "matrix": [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.5, 0.5, 0.0, 1.0] }));
    b.root(n);
    b.set("extensionsUsed", json!(["KHR_texture_transform"]));
    out.push(("texture_transform", b.build()));

    Ok(out)
}
