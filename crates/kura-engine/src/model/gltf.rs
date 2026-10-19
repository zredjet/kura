//! glb の入れ物・JSON の形・ノードの木のたどり・accessor の読み取り。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::IgnoredAny;

use super::{ModelLimits, bad};
use crate::error::EngineError;

/// glb の先頭の印（"glTF"）。
const MAGIC: u32 = 0x4654_6C67;
/// JSON の塊の種類（"JSON"）。
const CHUNK_JSON: u32 = 0x4E4F_534A;
/// BIN の塊の種類（"BIN\0"）。
const CHUNK_BIN: u32 = 0x004E_4942;
/// JSON の入れ子の深さの上限（serde_json は入れ子の深さだけ再帰する。glTF のふつうの深さは 6〜8）。
pub(crate) const MAX_JSON_DEPTH: usize = 64;

/// 読んだ glb：JSON の形と BIN の塊。
pub(crate) struct Glb<'a> {
    pub root: Root,
    pub bin: Option<&'a [u8]>,
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// glb を読む（入れ物と JSON。BIN は場所だけ）。
pub(crate) fn parse<'a>(bytes: &'a [u8], limits: &ModelLimits) -> Result<Glb<'a>, EngineError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(EngineError::Limit(format!(
            "glb が大きすぎます（{} バイト）",
            bytes.len()
        )));
    }
    if u32_at(bytes, 0) != Some(MAGIC) {
        return Err(bad("glb ではありません（先頭が glTF でない）"));
    }
    match u32_at(bytes, 4) {
        Some(2) => {}
        Some(v) => {
            return Err(EngineError::Unsupported(format!(
                "glb の版 {v} は読めません（2 だけ）"
            )));
        }
        None => return Err(bad("glb の見出しが短すぎます")),
    }
    let total = u32_at(bytes, 8).ok_or_else(|| bad("glb の見出しが短すぎます"))? as usize;
    let body = bytes
        .get(..total)
        .filter(|_| total >= 12)
        .ok_or_else(|| bad("glb の長さがファイルと合いません"))?;
    let mut at = 12usize;
    let mut json: Option<&[u8]> = None;
    let mut bin: Option<&[u8]> = None;
    let mut first = true;
    while at < body.len() {
        let len = u32_at(body, at).ok_or_else(|| bad("塊の見出しが途中で切れています"))? as usize;
        let kind = u32_at(body, at + 4).ok_or_else(|| bad("塊の見出しが途中で切れています"))?;
        let start = at + 8;
        let end = start
            .checked_add(len)
            .filter(|e| *e <= body.len())
            .ok_or_else(|| bad("塊の長さがファイルを超えます"))?;
        let data = &body[start..end];
        match kind {
            CHUNK_JSON if first => json = Some(data),
            CHUNK_JSON => return Err(bad("JSON の塊が 2 つあります")),
            CHUNK_BIN if !first && bin.is_none() => bin = Some(data),
            CHUNK_BIN if first => return Err(bad("先頭の塊が JSON でありません")),
            _ if first => return Err(bad("先頭の塊が JSON でありません")),
            // 知らない種類の塊・2 つめの BIN は読まない
            _ => {}
        }
        first = false;
        // 塊は 4 バイトに揃う（揃っていないものも読めるよう、次の見出しは 4 の倍数に切り上げた位置から）。必ず 8 バイト以上進む
        at = end
            .checked_add(3)
            .map(|e| e & !3)
            .ok_or_else(|| bad("塊の長さがファイルを超えます"))?;
    }
    let json = json.ok_or_else(|| bad("JSON の塊がありません"))?;
    if json.len() as u64 > limits.max_json_bytes {
        return Err(EngineError::Limit(format!(
            "glb の JSON が大きすぎます（{} バイト）",
            json.len()
        )));
    }
    check_json(json, MAX_JSON_DEPTH, limits.max_json_objects)?;
    let root: Root =
        serde_json::from_slice(json).map_err(|e| bad(format!("JSON が読めません: {e}")))?;
    if !root.asset.version.starts_with("2.") {
        return Err(EngineError::Unsupported(format!(
            "glTF の版 {} は読めません（2.x だけ）",
            root.asset.version
        )));
    }
    if root.nodes.len() as u64 > limits.max_nodes {
        return Err(EngineError::Limit(format!(
            "ノードが多すぎます（{}）",
            root.nodes.len()
        )));
    }
    Ok(Glb { root, bin })
}

/// JSON の入れ子の深さ（オブジェクトと配列）とオブジェクトの数を、生のバイト列で数えて断る（文字列の中の括弧は数えない）。
/// 1 バイトずつ必ず進む。
pub(crate) fn check_json(b: &[u8], max_depth: usize, max_objects: u64) -> Result<(), EngineError> {
    let (mut depth, mut objects) = (0usize, 0u64);
    let (mut in_str, mut esc) = (false, false);
    for &c in b {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > max_depth {
                    return Err(EngineError::Limit(format!(
                        "JSON の入れ子が深すぎます（{max_depth} 段を超える）"
                    )));
                }
                if c == b'{' {
                    objects += 1;
                    if objects > max_objects {
                        return Err(EngineError::Limit(format!(
                            "JSON のオブジェクトが多すぎます（{max_objects} を超える）"
                        )));
                    }
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

// ---- JSON の形（使う項目だけ。知らない項目は読み飛ばす：glTF は extras・extensions を足せる） ----

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Root {
    pub asset: Asset,
    pub scene: Option<u32>,
    pub scenes: Vec<SceneDef>,
    pub nodes: Vec<NodeDef>,
    pub meshes: Vec<MeshDef>,
    pub accessors: Vec<AccessorDef>,
    pub buffer_views: Vec<BufferViewDef>,
    pub buffers: Vec<BufferDef>,
    pub materials: Vec<MaterialDef>,
    pub textures: Vec<TextureDef>,
    pub images: Vec<ImageDef>,
    pub samplers: Vec<SamplerDef>,
    pub animations: Vec<IgnoredAny>,
    pub skins: Vec<IgnoredAny>,
    pub extensions_used: Vec<String>,
    pub extensions_required: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Asset {
    pub version: String,
    pub generator: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SceneDef {
    pub nodes: Vec<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct NodeDef {
    pub children: Vec<u32>,
    pub mesh: Option<u32>,
    pub skin: Option<u32>,
    pub matrix: Option<Box<[f64; 16]>>,
    pub translation: Option<[f64; 3]>,
    pub rotation: Option<[f64; 4]>,
    pub scale: Option<[f64; 3]>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct MeshDef {
    pub primitives: Vec<PrimitiveDef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct PrimitiveDef {
    pub attributes: BTreeMap<String, u32>,
    pub indices: Option<u32>,
    pub material: Option<u32>,
    pub mode: Option<u32>,
    pub extensions: BTreeMap<String, IgnoredAny>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct AccessorDef {
    pub buffer_view: Option<u32>,
    pub byte_offset: u64,
    pub component_type: u32,
    pub normalized: bool,
    pub count: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub min: Option<Vec<f64>>,
    pub max: Option<Vec<f64>>,
    pub sparse: Option<IgnoredAny>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct BufferViewDef {
    pub buffer: u32,
    pub byte_offset: u64,
    pub byte_length: u64,
    pub byte_stride: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct BufferDef {
    pub byte_length: u64,
    pub uri: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct MaterialDef {
    pub pbr_metallic_roughness: Option<PbrDef>,
    pub alpha_mode: Option<String>,
    pub alpha_cutoff: Option<f64>,
    pub double_sided: bool,
    pub emissive_factor: Option<[f64; 3]>,
    pub extensions: BTreeMap<String, IgnoredAny>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct PbrDef {
    pub base_color_factor: Option<[f64; 4]>,
    pub base_color_texture: Option<TextureInfoDef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TextureInfoDef {
    pub index: u32,
    pub tex_coord: u32,
    pub extensions: TextureInfoExt,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TextureInfoExt {
    #[serde(rename = "KHR_texture_transform")]
    pub transform: Option<TextureTransformDef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TextureTransformDef {
    pub offset: Option<[f64; 2]>,
    pub rotation: Option<f64>,
    pub scale: Option<[f64; 2]>,
    pub tex_coord: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TextureDef {
    pub source: Option<u32>,
    pub sampler: Option<u32>,
    pub extensions: TextureExt,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TextureExt {
    #[serde(rename = "EXT_texture_webp")]
    pub webp: Option<SourceRef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SourceRef {
    pub source: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ImageDef {
    pub buffer_view: Option<u32>,
    pub mime_type: Option<String>,
    pub uri: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SamplerDef {
    pub mag_filter: Option<u32>,
    pub wrap_s: Option<u32>,
    pub wrap_t: Option<u32>,
}

// ---- 変換の行列（列優先の 4×4。glTF と同じ並び） ----

pub(crate) type Mat4 = [f64; 16];

pub(crate) const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// a · b（列優先）。
pub(crate) fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + r] * b[c * 4 + k];
            }
            out[c * 4 + r] = s;
        }
    }
    out
}

/// ノードの局所の変換（matrix か、T · R · S）。
pub(crate) fn local(node: &NodeDef) -> Mat4 {
    if let Some(m) = &node.matrix {
        return **m;
    }
    let t = node.translation.unwrap_or([0.0; 3]);
    let s = node.scale.unwrap_or([1.0; 3]);
    let [mut x, mut y, mut z, mut w] = node.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let n = (x * x + y * y + z * z + w * w).sqrt();
    if n > 0.0 && n.is_finite() {
        (x, y, z, w) = (x / n, y / n, z / n, w / n);
    } else {
        (x, y, z, w) = (0.0, 0.0, 0.0, 1.0);
    }
    // 回転の行列（列優先）に、列ごとに拡大を掛ける
    let r = [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
    ];
    [
        r[0] * s[0],
        r[1] * s[0],
        r[2] * s[0],
        0.0,
        r[3] * s[1],
        r[4] * s[1],
        r[5] * s[1],
        0.0,
        r[6] * s[2],
        r[7] * s[2],
        r[8] * s[2],
        0.0,
        t[0],
        t[1],
        t[2],
        1.0,
    ]
}

/// 点を写す。
pub(crate) fn transform_point(m: &Mat4, p: [f64; 3]) -> [f64; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

/// 左上の 3×3 の行列式（負なら面の向きが裏返る）。
pub(crate) fn det3(m: &Mat4) -> f64 {
    m[0] * (m[5] * m[10] - m[9] * m[6]) - m[4] * (m[1] * m[10] - m[9] * m[2])
        + m[8] * (m[1] * m[6] - m[5] * m[2])
}

/// 法線を写す行列（左上の 3×3 の逆の転置。行列式に比例した余因子の行列で、向きだけを使う）。列優先の 3×3。
pub(crate) fn normal_matrix(m: &Mat4) -> [f64; 9] {
    let (a, b, c) = (m[0], m[4], m[8]);
    let (d, e, f) = (m[1], m[5], m[9]);
    let (g, h, i) = (m[2], m[6], m[10]);
    // 余因子の行列（= 逆の転置 × 行列式）。行列式の符号を掛けて、裏返る変換でも外向きを保つ
    let sign = if det3(m) < 0.0 { -1.0 } else { 1.0 };
    [
        sign * (e * i - f * h),
        sign * (c * h - b * i),
        sign * (b * f - c * e),
        sign * (f * g - d * i),
        sign * (a * i - c * g),
        sign * (c * d - a * f),
        sign * (d * h - e * g),
        sign * (b * g - a * h),
        sign * (a * e - b * d),
    ]
}

// ---- ノードの木 ----

/// 描くノードの実体：ノードの番号と世界の変換。
pub(crate) struct Instance {
    pub node: u32,
    pub world: Mat4,
}

/// 既定のシーン（`scene`、なければ `scenes[0]`、シーンがなければどのノードの子でもないノード）の根から、ノードの木を
/// 深さ優先・子の順にたどる。再帰しない。同じノードは 1 度だけ（輪・共有の子を断つ）。範囲の外の番号は飛ばす。
pub(crate) fn walk(root: &Root) -> Result<Vec<Instance>, EngineError> {
    let n = root.nodes.len();
    let roots: Vec<u32> = match root
        .scene
        .and_then(|s| root.scenes.get(s as usize))
        .or_else(|| root.scenes.first())
    {
        Some(s) => s.nodes.clone(),
        None => {
            let mut child = crate::codec::try_vec(n, false)?;
            for node in &root.nodes {
                for c in &node.children {
                    if let Some(slot) = child.get_mut(*c as usize) {
                        *slot = true;
                    }
                }
            }
            (0..n as u32).filter(|i| !child[*i as usize]).collect()
        }
    };
    // 積むときに印を付ける（同じノードを 2 度積まない。積む数はノードの数まで）
    let mut queued = crate::codec::try_vec(n, false)?;
    let mut out = Vec::new();
    let mut stack: Vec<(u32, Mat4)> = Vec::new();
    for r in roots.iter().rev() {
        if let Some(slot) = queued.get_mut(*r as usize)
            && !std::mem::replace(slot, true)
        {
            stack.push((*r, IDENTITY));
        }
    }
    while let Some((i, parent)) = stack.pop() {
        let Some(node) = root.nodes.get(i as usize) else {
            continue;
        };
        let world = mul(&parent, &local(node));
        for c in node.children.iter().rev() {
            if let Some(slot) = queued.get_mut(*c as usize)
                && !std::mem::replace(slot, true)
            {
                stack.push((*c, world));
            }
        }
        out.push(Instance { node: i, world });
    }
    Ok(out)
}

// ---- accessor ----

/// accessor の要素の成分の数。
pub(crate) fn components(kind: &str) -> Option<usize> {
    Some(match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT2" => 4,
        "MAT3" => 9,
        "MAT4" => 16,
        _ => return None,
    })
}

/// 成分の型のバイト数。
fn component_size(t: u32) -> Option<usize> {
    Some(match t {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        _ => return None,
    })
}

/// accessor が指す BIN の範囲と、要素の間隔。
struct View<'a> {
    data: &'a [u8],
    stride: usize,
    elem: usize,
    count: usize,
}

/// accessor の場所を確かめる（範囲・間隔・成分の型）。`bufferView` がなければ None（値はすべて 0）。外部・data: の
/// バッファ、疎な accessor は `Unsupported`。
fn view<'a>(
    root: &Root,
    bin: Option<&'a [u8]>,
    acc: &AccessorDef,
    comps: usize,
) -> Result<Option<View<'a>>, EngineError> {
    if acc.sparse.is_some() {
        return Err(EngineError::Unsupported(
            "疎な accessor は読めません".into(),
        ));
    }
    let csize =
        component_size(acc.component_type).ok_or_else(|| bad("accessor の成分の型が不明です"))?;
    let elem = csize * comps;
    let count = usize::try_from(acc.count).map_err(|_| bad("accessor の数が大きすぎます"))?;
    let Some(v) = acc.buffer_view else {
        return Ok(None);
    };
    let bv = root
        .buffer_views
        .get(v as usize)
        .ok_or_else(|| bad("bufferView の番号が範囲の外です"))?;
    let buffer = root
        .buffers
        .get(bv.buffer as usize)
        .ok_or_else(|| bad("buffer の番号が範囲の外です"))?;
    if bv.buffer != 0 || buffer.uri.is_some() {
        return Err(EngineError::Unsupported(
            "glb の BIN の塊でないバッファ（外部・data:）は読みません".into(),
        ));
    }
    let bin = bin.ok_or_else(|| bad("BIN の塊がありません"))?;
    let start = usize::try_from(bv.byte_offset).map_err(|_| bad("bufferView が範囲の外です"))?;
    let len = usize::try_from(bv.byte_length).map_err(|_| bad("bufferView が範囲の外です"))?;
    let data = start
        .checked_add(len)
        .and_then(|end| bin.get(start..end))
        .ok_or_else(|| bad("bufferView が BIN の塊を超えます"))?;
    let stride = match bv.byte_stride {
        Some(s) => usize::try_from(s).map_err(|_| bad("byteStride が不正です"))?,
        None => elem,
    };
    if stride < elem {
        return Err(bad("byteStride が要素より小さい"));
    }
    let offset = usize::try_from(acc.byte_offset).map_err(|_| bad("accessor が範囲の外です"))?;
    let data = data
        .get(offset..)
        .ok_or_else(|| bad("accessor が bufferView を超えます"))?;
    if count > 0 {
        let need = (count - 1)
            .checked_mul(stride)
            .and_then(|x| x.checked_add(elem))
            .ok_or_else(|| bad("accessor が bufferView を超えます"))?;
        if need > data.len() {
            return Err(bad("accessor が bufferView を超えます"));
        }
    }
    Ok(Some(View {
        data,
        stride,
        elem,
        count,
    }))
}

/// 1 成分を読む（正規化された整数は [0, 1]・[-1, 1] に）。
fn component(b: &[u8], t: u32, normalized: bool) -> f32 {
    match (t, b) {
        (5126, [a, b, c, d, ..]) => f32::from_le_bytes([*a, *b, *c, *d]),
        (5120, [a, ..]) => {
            let v = f32::from(*a as i8);
            if normalized {
                crate::math::max32(v / 127.0, -1.0)
            } else {
                v
            }
        }
        (5121, [a, ..]) => {
            let v = f32::from(*a);
            if normalized { v / 255.0 } else { v }
        }
        (5122, [a, b, ..]) => {
            let v = f32::from(i16::from_le_bytes([*a, *b]));
            if normalized {
                crate::math::max32(v / 32767.0, -1.0)
            } else {
                v
            }
        }
        (5123, [a, b, ..]) => {
            let v = f32::from(u16::from_le_bytes([*a, *b]));
            if normalized { v / 65535.0 } else { v }
        }
        (5125, [a, b, c, d, ..]) => u32::from_le_bytes([*a, *b, *c, *d]) as f32,
        _ => 0.0,
    }
}

/// 浮動小数の要素の列として読む（`N` 成分。accessor の成分がそれより少なければ残りは `fill`）。`max` を超える数は断る。
pub(crate) fn read_floats<const N: usize>(
    root: &Root,
    bin: Option<&[u8]>,
    index: u32,
    fill: [f32; N],
    max: u64,
) -> Result<Vec<[f32; N]>, EngineError> {
    let acc = root
        .accessors
        .get(index as usize)
        .ok_or_else(|| bad("accessor の番号が範囲の外です"))?;
    let comps = components(&acc.kind).ok_or_else(|| bad("accessor の型が不明です"))?;
    if comps > N || comps == 0 {
        return Err(bad("accessor の成分の数が合いません"));
    }
    if acc.count > max {
        return Err(EngineError::Limit(format!(
            "頂点が多すぎます（{}）",
            acc.count
        )));
    }
    let v = view(root, bin, acc, comps)?;
    let count = usize::try_from(acc.count).map_err(|_| bad("accessor の数が大きすぎます"))?;
    let mut out = Vec::new();
    out.try_reserve_exact(count)
        .map_err(|_| EngineError::Limit(format!("{count} 個の頂点を確保できません")))?;
    match v {
        None => out.resize(count, fill),
        Some(v) => {
            let csize = v.elem / comps;
            for i in 0..v.count {
                let at = i * v.stride;
                let e = &v.data[at..at + v.elem];
                let mut item = fill;
                for (k, slot) in item.iter_mut().enumerate().take(comps) {
                    *slot = component(&e[k * csize..], acc.component_type, acc.normalized);
                }
                out.push(item);
            }
        }
    }
    Ok(out)
}

/// 索引の列として読む（スカラーの符号なしの整数だけ）。
pub(crate) fn read_indices(
    root: &Root,
    bin: Option<&[u8]>,
    index: u32,
    max: u64,
) -> Result<Vec<u32>, EngineError> {
    let acc = root
        .accessors
        .get(index as usize)
        .ok_or_else(|| bad("accessor の番号が範囲の外です"))?;
    if acc.kind != "SCALAR" || !matches!(acc.component_type, 5121 | 5123 | 5125) {
        return Err(bad("索引の accessor の型が不正です"));
    }
    if acc.count > max {
        return Err(EngineError::Limit(format!(
            "索引が多すぎます（{}）",
            acc.count
        )));
    }
    let count = usize::try_from(acc.count).map_err(|_| bad("accessor の数が大きすぎます"))?;
    let mut out = Vec::new();
    out.try_reserve_exact(count)
        .map_err(|_| EngineError::Limit(format!("{count} 個の索引を確保できません")))?;
    match view(root, bin, acc, 1)? {
        None => out.resize(count, 0),
        Some(v) => {
            for i in 0..v.count {
                let at = i * v.stride;
                let e = &v.data[at..at + v.elem];
                out.push(match e {
                    [a] => u32::from(*a),
                    [a, b] => u32::from(u16::from_le_bytes([*a, *b])),
                    [a, b, c, d] => u32::from_le_bytes([*a, *b, *c, *d]),
                    _ => 0,
                });
            }
        }
    }
    Ok(out)
}

/// 画像の bufferView の中身。
pub(crate) fn view_bytes<'a>(
    root: &Root,
    bin: Option<&'a [u8]>,
    view: u32,
) -> Result<&'a [u8], EngineError> {
    let bv = root
        .buffer_views
        .get(view as usize)
        .ok_or_else(|| bad("bufferView の番号が範囲の外です"))?;
    let buffer = root
        .buffers
        .get(bv.buffer as usize)
        .ok_or_else(|| bad("buffer の番号が範囲の外です"))?;
    if bv.buffer != 0 || buffer.uri.is_some() {
        return Err(EngineError::Unsupported(
            "glb の BIN の塊でないバッファ（外部・data:）は読みません".into(),
        ));
    }
    let bin = bin.ok_or_else(|| bad("BIN の塊がありません"))?;
    let start = usize::try_from(bv.byte_offset).map_err(|_| bad("bufferView が範囲の外です"))?;
    let len = usize::try_from(bv.byte_length).map_err(|_| bad("bufferView が範囲の外です"))?;
    start
        .checked_add(len)
        .and_then(|end| bin.get(start..end))
        .ok_or_else(|| bad("bufferView が BIN の塊を超えます"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_depth_and_objects_are_counted_outside_strings() {
        let deep = "[".repeat(65) + &"]".repeat(65);
        assert!(matches!(
            check_json(deep.as_bytes(), 64, 100),
            Err(EngineError::Limit(_))
        ));
        let ok = "[".repeat(64) + &"]".repeat(64);
        assert!(check_json(ok.as_bytes(), 64, 100).is_ok());
        // 文字列の中の括弧・エスケープした引用符は数えない
        let s = format!(r#"{{"a":"{}\"{}"}}"#, "[".repeat(100), "{".repeat(100));
        assert!(check_json(s.as_bytes(), 64, 1).is_ok());
        assert!(matches!(
            check_json(b"[{},{},{}]", 64, 2),
            Err(EngineError::Limit(_))
        ));
    }

    #[test]
    fn trs_and_matrix_agree() {
        let node = NodeDef {
            translation: Some([1.0, 2.0, 3.0]),
            // z の周りに 90 度（x, y, z, w）
            rotation: Some([
                0.0,
                0.0,
                std::f64::consts::FRAC_1_SQRT_2,
                std::f64::consts::FRAC_1_SQRT_2,
            ]),
            scale: Some([2.0, 2.0, 2.0]),
            ..NodeDef::default()
        };
        let m = local(&node);
        let p = transform_point(&m, [1.0, 0.0, 0.0]);
        assert!(
            (p[0] - 1.0).abs() < 1e-12 && (p[1] - 4.0).abs() < 1e-12 && (p[2] - 3.0).abs() < 1e-12
        );
        assert!(det3(&m) > 0.0);
        let mirror = NodeDef {
            scale: Some([-1.0, 1.0, 1.0]),
            ..NodeDef::default()
        };
        assert!(det3(&local(&mirror)) < 0.0);
    }
}
