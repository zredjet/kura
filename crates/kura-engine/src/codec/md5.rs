//! MD5（RFC 1321）。FLAC の STREAMINFO の標本の MD5（書き出しと、読み取りの照らし）だけに使う（M16 段 1。md-5 の代わり）。
//!
//! 整数だけで決まるので、どのターゲットでも同じダイジェスト。表 `K` は RFC 1321 §3.4 の値（`⌊|sin(i + 1)| · 2³²⌋`。
//! テストで `math::sin` から求め直して照らす）。

/// 各段の左回転の量（RFC 1321 §3.4）。
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, //
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, //
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, //
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// 各段で使う語の番号（段 1：i、段 2：5i + 1、段 3：3i + 5、段 4：7i をそれぞれ 16 で割った余り）。
const G: [usize; 64] = {
    let mut g = [0usize; 64];
    let mut i = 0;
    while i < 64 {
        g[i] = match i / 16 {
            0 => i,
            1 => (5 * i + 1) % 16,
            2 => (3 * i + 5) % 16,
            _ => (7 * i) % 16,
        };
        i += 1;
    }
    g
};

/// 各段に足す定数（RFC 1321 §3.4 の T[1..64]）。
const K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// 流して求める MD5。
#[derive(Clone)]
pub(crate) struct Md5 {
    state: [u32; 4],
    /// 64 バイトに満たない残り。
    block: [u8; 64],
    filled: usize,
    /// 入力の長さ（バイト。2⁶⁴ で折り返す。RFC 1321 §3.2）。
    len: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    /// 初期の状態（RFC 1321 §3.3）。
    pub(crate) fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            block: [0; 64],
            filled: 0,
            len: 0,
        }
    }

    /// バイト列を足す。
    pub(crate) fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        if self.filled > 0 {
            let n = (64 - self.filled).min(data.len());
            self.block[self.filled..self.filled + n].copy_from_slice(&data[..n]);
            self.filled += n;
            data = &data[n..];
            if self.filled < 64 {
                return;
            }
            let block = self.block;
            self.compress(&block);
            self.filled = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for b in blocks {
            self.compress(b);
        }
        self.block[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }

    /// 詰め物と長さを足して、ダイジェスト（16 バイト）を返す（RFC 1321 §3.1・§3.2・§3.5）。
    pub(crate) fn finalize(mut self) -> [u8; 16] {
        let bits = self.len.wrapping_mul(8);
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        // 長さの 8 バイトの前で、64 で割って 56 余る所まで詰める
        let n = if self.filled < 56 {
            56 - self.filled
        } else {
            120 - self.filled
        };
        let len = self.len;
        self.update(&pad[..n]);
        self.update(&bits.to_le_bytes());
        // 詰め物は入力の長さに数えない（長さはもう書いた）
        self.len = len;
        let mut out = [0u8; 16];
        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *o = s.to_le_bytes();
        }
        out
    }

    /// 64 バイトの塊 1 つ（RFC 1321 §3.4）。
    fn compress(&mut self, block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (w, c) in m.iter_mut().zip(block.as_chunks::<4>().0) {
            *w = u32::from_le_bytes(*c);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        // 1 段：b ← b + ((a + F(b, c, d) + K[i] + X[g]) <<< s)。足し算は 2³² を法とするので、足す順を変えても同じ値。
        // 前の段の b を待たない項（a・K・X と、c・d だけで決まる部分）を先に足し、b に依る項を最後に足す（RFC 1321 の
        // F・G は、ビットの重ならない 2 つの項の OR なので、OR を足し算にしてよい）
        macro_rules! step {
            ($early:expr, $late:expr, $i:expr) => {{
                let f = a
                    .wrapping_add(K[$i])
                    .wrapping_add(m[G[$i]])
                    .wrapping_add($early)
                    .wrapping_add($late);
                a = d;
                d = c;
                c = b;
                b = b.wrapping_add(f.rotate_left(S[$i]));
            }};
        }
        for i in 0..16 {
            // F = (b & c) | (!b & d) = d ^ (b & (c ^ d))
            step!(0, d ^ (b & (c ^ d)), i);
        }
        for i in 16..32 {
            // G = (d & b) | (!d & c)：2 つの項はビットが重ならないので和
            step!(!d & c, d & b, i);
        }
        for i in 32..48 {
            // H = b ^ c ^ d
            step!(0, b ^ (c ^ d), i);
        }
        for i in 48..64 {
            // I = c ^ (b | !d)
            step!(0, c ^ (b | !d), i);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

/// バイト列の MD5。
#[cfg(test)]
pub(crate) fn digest(data: &[u8]) -> [u8; 16] {
    let mut m = Md5::new();
    m.update(data);
    m.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(d: [u8; 16]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// RFC 1321 §A.5 のテストの組。
    #[test]
    fn rfc1321_test_suite() {
        let cases = [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(hex(digest(input.as_bytes())), want, "{input:?}");
        }
    }

    /// 表 `K` は ⌊|sin(i + 1)| · 2³²⌋（RFC 1321 §3.4）。
    #[test]
    fn table_is_the_sine_table() {
        for (i, k) in K.iter().enumerate() {
            let v = (crate::math::sin((i + 1) as f64).abs() * 4_294_967_296.0) as u32;
            assert_eq!(*k, v, "K[{i}]");
        }
    }

    /// 切り方によらず同じ（64 バイトの境目の前後・1 バイトずつ・まとめて）。
    #[test]
    fn chunking_does_not_change_the_digest() {
        use rand_chacha::rand_core::Rng;
        let mut r = crate::determinism::rng(5);
        for len in [
            0usize, 1, 55, 56, 57, 63, 64, 65, 119, 120, 121, 128, 1000, 4097,
        ] {
            let data: Vec<u8> = (0..len).map(|_| r.next_u32() as u8).collect();
            let whole = digest(&data);
            let mut m = Md5::new();
            for b in &data {
                m.update(std::slice::from_ref(b));
            }
            assert_eq!(m.finalize(), whole, "1 バイトずつ（{len}）");
            let mut m = Md5::new();
            let mut rest = data.as_slice();
            while !rest.is_empty() {
                let n = (r.next_u32() as usize % 130).min(rest.len());
                m.update(&rest[..n]);
                rest = &rest[n..];
            }
            assert_eq!(m.finalize(), whole, "ばらばら（{len}）");
        }
    }
}
