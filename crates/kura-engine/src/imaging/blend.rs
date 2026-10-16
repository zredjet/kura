//! Aseprite と同じ式のブレンド（RGBA8、ストレートのアルファ）。aseprite の読み取りと、段 5 の層の操作で使う。
//!
//! 式は Aseprite の `src/doc/blend_funcs.cpp`（MIT ライセンス）に合わせる。Rust への移植の asefile（MIT）とも照らす
//! （tests/aseprite_ref.rs）。整数の式は `mul_un8`・`div_un8` の丸め、色の差の割り算は 0 への切り捨て。
//! ソフトライトと色相・彩度・カラー・輝度は f64 の四則演算と `sqrt` だけで計算する（IEEE で一意。超越関数は使わない）。
//!
//! 通常（Normal）以外のモードは、Aseprite 1.3 の方式で、下の画素のアルファに応じて「通常で重ねた色」と「モードで混ぜた色」を
//! 混ぜ合わせる（`merge`）。下が透明なら通常と同じ。

/// ブレンドモード（aseprite の番号の順）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    Addition,
    Subtract,
    Divide,
}

impl BlendMode {
    /// すべてのモード（番号の順）。
    pub const ALL: [BlendMode; 19] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::Darken,
        Self::Lighten,
        Self::ColorDodge,
        Self::ColorBurn,
        Self::HardLight,
        Self::SoftLight,
        Self::Difference,
        Self::Exclusion,
        Self::Hue,
        Self::Saturation,
        Self::Color,
        Self::Luminosity,
        Self::Addition,
        Self::Subtract,
        Self::Divide,
    ];

    /// aseprite のファイルの番号から。知らない番号は None。
    pub fn from_code(code: u16) -> Option<Self> {
        Self::ALL.get(usize::from(code)).copied()
    }
}

/// `a·b / 255` の丸め（Aseprite の MUL_UN8）。
pub fn mul_un8(a: i32, b: i32) -> i32 {
    let t = a * b + 0x80;
    ((t >> 8) + t) >> 8
}

/// `a·255 / b` の丸め（Aseprite の DIV_UN8）。
pub fn div_un8(a: i32, b: i32) -> i32 {
    (a * 255 + b / 2) / b
}

fn ch(c: [u8; 4]) -> [i32; 4] {
    [
        i32::from(c[0]),
        i32::from(c[1]),
        i32::from(c[2]),
        i32::from(c[3]),
    ]
}

fn px(r: i32, g: i32, b: i32, a: i32) -> [u8; 4] {
    [
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
        a.clamp(0, 255) as u8,
    ]
}

/// 通常の重ね方（rgba_blender_normal）。`opacity` は層とセルの不透明度を掛けたもの。
pub fn normal(back: [u8; 4], src: [u8; 4], opacity: u8) -> [u8; 4] {
    let [br, bg, bb, ba] = ch(back);
    let [sr, sg, sb, sa] = ch(src);
    let op = i32::from(opacity);
    if ba == 0 {
        return px(sr, sg, sb, mul_un8(sa, op));
    }
    if sa == 0 {
        return back;
    }
    let sa = mul_un8(sa, op);
    let ra = sa + ba - mul_un8(ba, sa);
    let rr = br + (sr - br) * sa / ra;
    let rg = bg + (sg - bg) * sa / ra;
    let rb = bb + (sb - bb) * sa / ra;
    px(rr, rg, rb, ra)
}

/// 2 つの色を `opacity` の割合で混ぜる（rgba_blender_merge）。
fn merge(back: [u8; 4], src: [u8; 4], opacity: i32) -> [u8; 4] {
    let [br, bg, bb, ba] = ch(back);
    let [sr, sg, sb, sa] = ch(src);
    let (rr, rg, rb) = if ba == 0 {
        (sr, sg, sb)
    } else if sa == 0 {
        (br, bg, bb)
    } else {
        (
            br + mul_un8(sr - br, opacity),
            bg + mul_un8(sg - bg, opacity),
            bb + mul_un8(sb - bb, opacity),
        )
    };
    let ra = ba + mul_un8(sa - ba, opacity);
    if ra == 0 {
        px(0, 0, 0, 0)
    } else {
        px(rr, rg, rb, ra)
    }
}

/// `back` の上に `src` を `mode` で重ねる。
pub fn blend(mode: BlendMode, back: [u8; 4], src: [u8; 4], opacity: u8) -> [u8; 4] {
    if mode == BlendMode::Normal || back[3] == 0 {
        return normal(back, src, opacity);
    }
    let n = normal(back, src, opacity);
    let b = baseline(mode, back, src, opacity);
    let m = merge(n, b, i32::from(back[3]));
    let src_total = mul_un8(i32::from(src[3]), i32::from(opacity));
    let composite = mul_un8(i32::from(back[3]), src_total);
    merge(m, b, composite)
}

/// モードで混ぜた色を、通常の重ね方で重ねたもの（各モードの rgba_blender_*）。
fn baseline(mode: BlendMode, back: [u8; 4], src: [u8; 4], opacity: u8) -> [u8; 4] {
    let [br, bg, bb, _] = ch(back);
    let [sr, sg, sb, sa] = ch(src);
    let sep = |f: fn(i32, i32) -> i32| -> [u8; 4] {
        normal(back, px(f(br, sr), f(bg, sg), f(bb, sb), sa), opacity)
    };
    match mode {
        BlendMode::Normal => normal(back, src, opacity),
        BlendMode::Multiply => sep(multiply),
        BlendMode::Screen => sep(screen),
        BlendMode::Overlay => sep(|b, s| hard_light(s, b)),
        BlendMode::Darken => sep(|b, s| if b < s { b } else { s }),
        BlendMode::Lighten => sep(|b, s| if b > s { b } else { s }),
        BlendMode::ColorDodge => sep(color_dodge),
        BlendMode::ColorBurn => sep(color_burn),
        BlendMode::HardLight => sep(hard_light),
        BlendMode::SoftLight => sep(soft_light),
        BlendMode::Difference => sep(|b, s| (b - s).abs()),
        BlendMode::Exclusion => sep(|b, s| b + s - 2 * mul_un8(b, s)),
        BlendMode::Addition => sep(|b, s| (b + s).min(255)),
        BlendMode::Subtract => sep(|b, s| (b - s).max(0)),
        BlendMode::Divide => sep(divide),
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            let rgb = |c: [i32; 4]| {
                (
                    f64::from(c[0]) / 255.0,
                    f64::from(c[1]) / 255.0,
                    f64::from(c[2]) / 255.0,
                )
            };
            let (b3, s3) = (rgb(ch(back)), rgb(ch(src)));
            let (r, g, b) = match mode {
                BlendMode::Hue => {
                    let (s, l) = (sat(b3), lum(b3));
                    set_lum(set_sat(s3, s), l)
                }
                BlendMode::Saturation => {
                    let s = sat(s3);
                    let l = lum(b3);
                    set_lum(set_sat(b3, s), l)
                }
                BlendMode::Color => set_lum(s3, lum(b3)),
                _ => set_lum(b3, lum(s3)),
            };
            // int(255.0 * r)：0 への切り捨て
            let to = |v: f64| (255.0 * v) as i32;
            normal(back, px(to(r), to(g), to(b), sa), opacity)
        }
    }
}

fn multiply(b: i32, s: i32) -> i32 {
    mul_un8(b, s)
}

fn screen(b: i32, s: i32) -> i32 {
    b + s - mul_un8(b, s)
}

fn hard_light(b: i32, s: i32) -> i32 {
    if s < 128 {
        multiply(b, s << 1)
    } else {
        screen(b, (s << 1) - 255)
    }
}

fn color_dodge(b: i32, s: i32) -> i32 {
    if b == 0 {
        return 0;
    }
    let s = 255 - s;
    if b >= s { 255 } else { div_un8(b, s) }
}

fn color_burn(b: i32, s: i32) -> i32 {
    if b == 255 {
        return 255;
    }
    let b = 255 - b;
    if b >= s { 0 } else { 255 - div_un8(b, s) }
}

fn divide(b: i32, s: i32) -> i32 {
    if b == 0 {
        0
    } else if b >= s {
        255
    } else {
        div_un8(b, s)
    }
}

fn soft_light(b: i32, s: i32) -> i32 {
    let b = f64::from(b) / 255.0;
    let s = f64::from(s) / 255.0;
    let d = if b <= 0.25 {
        ((16.0 * b - 12.0) * b + 4.0) * b
    } else {
        b.sqrt()
    };
    let r = if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        b + (2.0 * s - 1.0) * (d - b)
    };
    (r * 255.0 + 0.5) as i32
}

fn lum((r, g, b): (f64, f64, f64)) -> f64 {
    0.3 * r + 0.59 * g + 0.11 * b
}

fn fmax(a: f64, b: f64) -> f64 {
    if a > b { a } else { b }
}

fn fmin(a: f64, b: f64) -> f64 {
    if a < b { a } else { b }
}

fn sat((r, g, b): (f64, f64, f64)) -> f64 {
    fmax(r, fmax(g, b)) - fmin(r, fmin(g, b))
}

fn clip_color((mut r, mut g, mut b): (f64, f64, f64)) -> (f64, f64, f64) {
    let l = lum((r, g, b));
    let n = fmin(r, fmin(g, b));
    let x = fmax(r, fmax(g, b));
    if n < 0.0 {
        r = l + (((r - l) * l) / (l - n));
        g = l + (((g - l) * l) / (l - n));
        b = l + (((b - l) * l) / (l - n));
    }
    if x > 1.0 {
        r = l + (((r - l) * (1.0 - l)) / (x - l));
        g = l + (((g - l) * (1.0 - l)) / (x - l));
        b = l + (((b - l) * (1.0 - l)) / (x - l));
    }
    (r, g, b)
}

fn set_lum(c: (f64, f64, f64), l: f64) -> (f64, f64, f64) {
    let d = l - lum(c);
    clip_color((c.0 + d, c.1 + d, c.2 + d))
}

/// 彩度を置く。Aseprite のマクロ（MIN・MID・MAX が同じ値の変数を指すことがある）と同じ選び方と代入の順にする。
fn set_sat((r, g, b): (f64, f64, f64), s: f64) -> (f64, f64, f64) {
    let mut c = [r, g, b];
    let min = if r < fmin(g, b) {
        0
    } else if g < b {
        1
    } else {
        2
    };
    let max = if r > fmax(g, b) {
        0
    } else if g > b {
        1
    } else {
        2
    };
    let mid = if r > g {
        if g > b {
            1
        } else if r > b {
            2
        } else {
            0
        }
    } else if g > b {
        if b > r { 2 } else { 0 }
    } else {
        1
    };
    if c[max] > c[min] {
        c[mid] = ((c[mid] - c[min]) * s) / (c[max] - c[min]);
        c[max] = s;
    } else {
        c[mid] = 0.0;
        c[max] = 0.0;
    }
    c[min] = 0.0;
    (c[0], c[1], c[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// asefile のテストと同じ値（Aseprite の式の確かめ）。
    #[test]
    fn known_values() {
        assert_eq!(
            normal([0, 205, 249, 255], [237, 118, 20, 255], 128),
            [118, 162, 135, 255]
        );
        assert_eq!(mul_un8(255, 255), 255);
        assert_eq!(mul_un8(128, 255), 128);
        assert_eq!(div_un8(128, 255), 128);
        // 下が透明なら、どのモードも通常と同じ
        for m in BlendMode::ALL {
            assert_eq!(
                blend(m, [9, 9, 9, 0], [200, 10, 30, 99], 200),
                normal([9, 9, 9, 0], [200, 10, 30, 99], 200)
            );
        }
        assert_eq!(BlendMode::from_code(18), Some(BlendMode::Divide));
        assert_eq!(BlendMode::from_code(19), None);
    }

    /// どの組み合わせでも落ちない（割り算の 0・範囲の外）。結果は 0〜255。
    #[test]
    fn total_over_a_grid() {
        let vals = [0u8, 1, 64, 127, 128, 200, 254, 255];
        for m in BlendMode::ALL {
            for &a in &vals {
                for &b in &vals {
                    for &op in &[0u8, 77, 255] {
                        let _ = blend(m, [a, b, a, b], [b, a, 255 - a, a], op);
                    }
                }
            }
        }
    }
}
