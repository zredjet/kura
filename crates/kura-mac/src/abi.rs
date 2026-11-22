//! Platform のコーデックの C の口の型と番号（`swift/Sources/KuraMacC/include/kura_mac_ffi.h` と同じ形）。OS によらない
//! （変換と照合を Linux でもテストする）。関数の宣言と呼び出しは macOS の `bridge`。ヘッダが正本で、ここの構造体・番号と
//! `bridge` の extern の宣言は、下のテストがヘッダの文字列と照らす。

// macOS 以外では使う側（bridge と backend）がない（Linux ではテストだけが使う）
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::mem::{offset_of, size_of};

/// ABI の版（`KURA_FFI_ABI` と同じ値）。
pub(crate) const ABI: u32 = 1;

/// アルファの扱い（`KURA_ALPHA_*`）。
pub(crate) const ALPHA_NONE: i32 = 0;
pub(crate) const ALPHA_STRAIGHT: i32 = 1;
pub(crate) const ALPHA_PREMULTIPLIED: i32 = 2;
/// 画像の旗（`KURA_IMG_*`）。CONVERTED・SEQUENCE は調べ物の印（エンジンの adapter には渡す口がない）。
pub(crate) const IMG_HDR: u32 = 1;
#[allow(dead_code)]
pub(crate) const IMG_CONVERTED: u32 = 2;
#[allow(dead_code)]
pub(crate) const IMG_SEQUENCE: u32 = 4;
/// 音声の旗（`KURA_AUD_*`）。
pub(crate) const AUD_FLOAT: u32 = 1;
pub(crate) const AUD_PCM: u32 = 2;

/// `kura_image_info`。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ImageInfoC {
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub orientation: u32,
    pub pixel_format: i32,
    pub alpha: i32,
    pub loop_count: u32,
    pub flags: u32,
    pub icc_len: i64,
    pub exif_len: i64,
}

/// `kura_image_frame`。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ImageFrameC {
    pub width: u32,
    pub height: u32,
    pub pixel_format: i32,
    pub alpha: i32,
    pub duration_ms: u32,
    pub flags: u32,
}

/// `kura_audio_info`。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct AudioInfoC {
    pub rate: f64,
    pub channels: u32,
    pub format_id: u32,
    pub frames: i64,
    pub priming: i64,
    pub remainder: i64,
    pub bits: u32,
    pub flags: u32,
}

/// `kura_aac_settings`。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct AacSettingsC {
    pub rate: f64,
    pub channels: u32,
    pub channel_mask: u32,
    pub mode: u32,
    pub bitrate: u32,
    pub quality: u32,
    pub reserved: u32,
}

/// 構造体の大きさと各項目の位置（Rust の `#[repr(C)]`。`bridge::swift_layout` と同じ並び）。
pub(crate) fn rust_layout() -> Vec<i64> {
    let v = [
        size_of::<ImageInfoC>(),
        offset_of!(ImageInfoC, width),
        offset_of!(ImageInfoC, height),
        offset_of!(ImageInfoC, frames),
        offset_of!(ImageInfoC, orientation),
        offset_of!(ImageInfoC, pixel_format),
        offset_of!(ImageInfoC, alpha),
        offset_of!(ImageInfoC, loop_count),
        offset_of!(ImageInfoC, flags),
        offset_of!(ImageInfoC, icc_len),
        offset_of!(ImageInfoC, exif_len),
        size_of::<ImageFrameC>(),
        offset_of!(ImageFrameC, width),
        offset_of!(ImageFrameC, height),
        offset_of!(ImageFrameC, pixel_format),
        offset_of!(ImageFrameC, alpha),
        offset_of!(ImageFrameC, duration_ms),
        offset_of!(ImageFrameC, flags),
        size_of::<AudioInfoC>(),
        offset_of!(AudioInfoC, rate),
        offset_of!(AudioInfoC, channels),
        offset_of!(AudioInfoC, format_id),
        offset_of!(AudioInfoC, frames),
        offset_of!(AudioInfoC, priming),
        offset_of!(AudioInfoC, remainder),
        offset_of!(AudioInfoC, bits),
        offset_of!(AudioInfoC, flags),
        size_of::<AacSettingsC>(),
        offset_of!(AacSettingsC, rate),
        offset_of!(AacSettingsC, channels),
        offset_of!(AacSettingsC, channel_mask),
        offset_of!(AacSettingsC, mode),
        offset_of!(AacSettingsC, bitrate),
        offset_of!(AacSettingsC, quality),
        offset_of!(AacSettingsC, reserved),
    ];
    v.iter().map(|&n| n as i64).collect()
}

/// ヘッダ（正本）と Rust の宣言を文字列で照らす（macOS 以外でも回る）。ヘッダは決まった書き方（1 つの宣言は `;` で終わる・
/// 構造体は `typedef struct 名前 { … } 名前;`・番号は `#define 名前 値`）で書く。
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kura_engine::codec::platform as engine;

    use super::*;

    const HEADER: &str = include_str!("../swift/Sources/KuraMacC/include/kura_mac_ffi.h");
    const BRIDGE: &str = include_str!("bridge.rs");
    const ABI_RS: &str = include_str!("abi.rs");

    /// 行の注釈（`//`・`///`）を消す。
    fn strip_comments(text: &str) -> String {
        text.lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 空白をすべて消す（型の比べ方をそろえる）。
    fn squash(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// C の型（`const float *const *` など）を Rust の型にする（空白なし）。
    fn c_to_rust(c: &str) -> String {
        let c = c.replace('*', " * ");
        let mut tokens = c.split_whitespace().peekable();
        let mut pointee_const = false;
        if tokens.peek() == Some(&"const") {
            pointee_const = true;
            tokens.next();
        }
        let base = tokens.next().unwrap_or("");
        let mut rust = match base {
            "void" => "c_void",
            "bool" => "bool",
            "char" => "c_char",
            "uint8_t" => "u8",
            "int32_t" => "i32",
            "uint32_t" => "u32",
            "int64_t" => "i64",
            "float" => "f32",
            "double" => "f64",
            "kura_cancel_fn" => "Option<CancelFn>",
            "kura_image_info" => "ImageInfoC",
            "kura_image_frame" => "ImageFrameC",
            "kura_audio_info" => "AudioInfoC",
            "kura_aac_settings" => "AacSettingsC",
            other => panic!("知らない C の型です: {other}（{c}）"),
        }
        .to_string();
        while let Some(t) = tokens.next() {
            assert_eq!(t, "*", "{c}");
            rust = format!("*{}{rust}", if pointee_const { "const" } else { "mut" });
            pointee_const = tokens.peek() == Some(&"const");
            if pointee_const {
                tokens.next();
            }
        }
        rust
    }

    /// `型 名前` の並び（`,` 区切り）から型だけを取る。
    fn c_params(params: &str) -> Vec<String> {
        let params = params.trim();
        if params == "void" || params.is_empty() {
            return Vec::new();
        }
        params
            .split(',')
            .map(|p| {
                let p = p.trim();
                // 名前は最後の識別子（`*` の後ろ）
                let cut = p
                    .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .map_or(0, |i| i + 1);
                c_to_rust(&p[..cut])
            })
            .collect()
    }

    /// ヘッダの関数：名前 → (引数の型, 戻り値の型)（Rust の書き方。戻り値がなければ空）。
    fn header_functions() -> BTreeMap<String, (Vec<String>, String)> {
        let text = strip_comments(HEADER);
        let text: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join(" ");
        // 構造体の中身を消す
        let mut flat = String::new();
        let mut depth = 0;
        for ch in text.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ if depth == 0 => flat.push(ch),
                _ => {}
            }
        }
        let mut out = BTreeMap::new();
        for decl in flat.split(';').map(str::trim).filter(|d| !d.is_empty()) {
            if decl.starts_with("typedef") {
                continue;
            }
            let open = decl
                .find('(')
                .unwrap_or_else(|| panic!("関数ではありません: {decl}"));
            let close = decl.rfind(')').unwrap();
            let head = decl[..open].trim();
            let cut = head
                .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
                .map_or(0, |i| i + 1);
            let (ret, name) = (head[..cut].trim(), &head[cut..]);
            let ret = if ret == "void" {
                String::new()
            } else {
                c_to_rust(ret)
            };
            let prev = out.insert(name.to_string(), (c_params(&decl[open + 1..close]), ret));
            assert!(prev.is_none(), "{name} が 2 度あります");
        }
        out
    }

    /// bridge.rs の `unsafe extern "C" { … }` の関数：名前 → (引数の型, 戻り値の型)。
    fn bridge_functions() -> BTreeMap<String, (Vec<String>, String)> {
        let start = BRIDGE.find("unsafe extern \"C\" {").unwrap();
        let body = &BRIDGE[start..];
        let body = &body[body.find('{').unwrap() + 1..body.find("\n}").unwrap()];
        let body: String = strip_comments(body)
            .lines()
            .filter(|l| !l.trim_start().starts_with("#["))
            .collect::<Vec<_>>()
            .join(" ");
        let mut out = BTreeMap::new();
        for decl in body.split(';').map(str::trim).filter(|d| !d.is_empty()) {
            let decl = decl.strip_prefix("fn ").unwrap_or_else(|| panic!("{decl}"));
            let open = decl.find('(').unwrap();
            let close = decl.rfind(')').unwrap();
            let name = decl[..open].trim().to_string();
            let params: Vec<String> = decl[open + 1..close]
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| squash(p.split_once(':').unwrap().1))
                .collect();
            let ret = decl[close + 1..]
                .trim()
                .strip_prefix("->")
                .map(squash)
                .unwrap_or_default();
            let prev = out.insert(name.clone(), (params, ret));
            assert!(prev.is_none(), "{name} が 2 度あります");
        }
        out
    }

    /// bridge.rs の extern の宣言は、ヘッダのプロトタイプと関数の集合・引数の型・戻り値の型が同じ。
    #[test]
    fn bridge_declarations_match_the_header() {
        let (h, b) = (header_functions(), bridge_functions());
        assert_eq!(h.len(), 18, "{:?}", h.keys());
        assert_eq!(
            h.keys().collect::<Vec<_>>(),
            b.keys().collect::<Vec<_>>(),
            "関数の集合が違います"
        );
        for (name, sig) in &h {
            assert_eq!(Some(sig), b.get(name), "{name}");
        }
    }

    /// 型の読み方（ポインタの const はポインタの先の const）。
    #[test]
    fn c_types_map_to_rust() {
        for (c, rust) in [
            ("void *", "*mutc_void"),
            ("const uint8_t *", "*constu8"),
            ("float *const *", "*const*mutf32"),
            ("const float *const *", "*const*constf32"),
            ("const kura_aac_settings *", "*constAacSettingsC"),
            ("kura_cancel_fn", "Option<CancelFn>"),
            ("int64_t", "i64"),
        ] {
            assert_eq!(c_to_rust(c), rust, "{c}");
        }
        assert_eq!(c_params("void"), Vec::<String>::new());
        assert_eq!(c_params("int64_t *out, int64_t cap"), ["*muti64", "i64"]);
    }

    /// ヘッダの構造体：名前 → 項目の (名前, Rust の型)。
    fn header_structs() -> BTreeMap<String, Vec<(String, String)>> {
        let text = strip_comments(HEADER);
        let mut out = BTreeMap::new();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("typedef struct ") {
            rest = &rest[i + "typedef struct ".len()..];
            let open = rest.find('{').unwrap();
            let close = rest.find('}').unwrap();
            let name = rest[..open].trim().to_string();
            let fields = rest[open + 1..close]
                .split(';')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(|f| {
                    let (ty, field) = f.rsplit_once(char::is_whitespace).unwrap();
                    (field.to_string(), c_to_rust(ty))
                })
                .collect();
            out.insert(name, fields);
            rest = &rest[close..];
        }
        out
    }

    /// abi.rs の `#[repr(C)]` の構造体：名前 → 項目の (名前, 型)。
    fn rust_struct(name: &str) -> Vec<(String, String)> {
        let start = ABI_RS
            .find(&format!("pub(crate) struct {name} {{"))
            .unwrap_or_else(|| panic!("{name} がありません"));
        let body = &ABI_RS[start..];
        let body = &body[body.find('{').unwrap() + 1..body.find('}').unwrap()];
        body.split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(|f| {
                let (field, ty) = f.strip_prefix("pub ").unwrap().split_once(':').unwrap();
                (field.trim().to_string(), squash(ty))
            })
            .collect()
    }

    /// abi.rs の構造体は、ヘッダの構造体と項目の名前・並び・型が同じ（大きさと位置は起動時の照合と _Static_assert も見る）。
    #[test]
    fn structs_match_the_header() {
        let h = header_structs();
        let pairs = [
            ("kura_image_info", "ImageInfoC"),
            ("kura_image_frame", "ImageFrameC"),
            ("kura_audio_info", "AudioInfoC"),
            ("kura_aac_settings", "AacSettingsC"),
        ];
        assert_eq!(h.len(), pairs.len(), "{:?}", h.keys());
        for (c, rust) in pairs {
            assert_eq!(h.get(c), Some(&rust_struct(rust)), "{c} と {rust}");
        }
    }

    /// ヘッダの番号（`#define`）は Rust の定数と同じ値。
    #[test]
    fn numbers_match_the_header() {
        let defines: BTreeMap<&str, i64> = HEADER
            .lines()
            .filter_map(|l| {
                let mut t = l.strip_prefix("#define ")?.split_whitespace();
                let name = t.next()?;
                let value = t.next()?.trim_end_matches('u').parse().ok()?;
                Some((name, value))
            })
            .collect();
        let want: &[(&str, i64)] = &[
            ("KURA_FFI_ABI", i64::from(ABI)),
            ("KURA_OK", i64::from(engine::STATUS_OK)),
            ("KURA_UNSUPPORTED", i64::from(engine::STATUS_UNSUPPORTED)),
            ("KURA_FAILED", i64::from(engine::STATUS_FAILED)),
            ("KURA_CANCELED", i64::from(engine::STATUS_CANCELED)),
            ("KURA_INVALID", i64::from(engine::STATUS_INVALID)),
            ("KURA_MISMATCH", i64::from(engine::STATUS_MISMATCH)),
            ("KURA_LIMIT", i64::from(engine::STATUS_LIMIT)),
            ("KURA_ALPHA_NONE", i64::from(ALPHA_NONE)),
            ("KURA_ALPHA_STRAIGHT", i64::from(ALPHA_STRAIGHT)),
            ("KURA_ALPHA_PREMULTIPLIED", i64::from(ALPHA_PREMULTIPLIED)),
            ("KURA_IMG_HDR", i64::from(IMG_HDR)),
            ("KURA_IMG_CONVERTED", i64::from(IMG_CONVERTED)),
            ("KURA_IMG_SEQUENCE", i64::from(IMG_SEQUENCE)),
            ("KURA_AUD_FLOAT", i64::from(AUD_FLOAT)),
            ("KURA_AUD_PCM", i64::from(AUD_PCM)),
        ];
        // 値のない #define（インクルードガード）は数えない
        assert_eq!(defines, want.iter().copied().collect::<BTreeMap<_, _>>());
    }
}
