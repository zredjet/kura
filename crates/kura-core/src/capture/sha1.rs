//! SHA-1（RFC 3174。M16 段 9。ADR-226）。WebSocket の握手の `Sec-WebSocket-Accept` を照らすためだけに使う（暗号の強さには
//! 頼らない）。

/// `data` の SHA-1。
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    // 末尾に 0x80、0 の詰め物、長さ（ビット、64 ビットのビッグエンディアン）を足して 64 バイトの倍数にする
    let mut tail = Vec::with_capacity(128);
    let (blocks, rest) = data.as_chunks::<64>();
    tail.extend_from_slice(rest);
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&bit_len.to_be_bytes());
    for block in blocks.iter().chain(tail.as_chunks::<64>().0) {
        compress(&mut h, block);
    }
    let mut out = [0u8; 20];
    for (o, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *o = v.to_be_bytes();
    }
    out
}

/// 64 バイトのブロックを 1 つ混ぜる。
fn compress(h: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (i, c) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*c);
    }
    for t in 16..80 {
        w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *h;
    for (t, &wt) in w.iter().enumerate() {
        let (f, k) = match t {
            0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let temp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(wt);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }
    for (x, v) in h.iter_mut().zip([a, b, c, d, e]) {
        *x = x.wrapping_add(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02X}")).collect()
    }

    /// RFC 3174 の §7.3 の 4 つのテストベクタと、空の入力・ブロックの境目の長さ。
    #[test]
    fn matches_the_rfc_3174_test_vectors() {
        assert_eq!(
            hex(&sha1(b"abc")),
            "A9993E364706816ABA3E25717850C26C9CD0D89D"
        );
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983E441C3BD26EBAAE4AA1F95129E5E54670F1"
        );
        assert_eq!(
            hex(&sha1(&vec![b'a'; 1_000_000])),
            "34AA973CD4C4DAA4F61EEB2BDBAD27316534016F"
        );
        assert_eq!(
            hex(&sha1(
                "0123456701234567012345670123456701234567012345670123456701234567"
                    .repeat(10)
                    .as_bytes()
            )),
            "DEA356A2CDDD90C7A7ECEDC5EBB563934F460452"
        );
        assert_eq!(hex(&sha1(b"")), "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709");
        // 55・56・63・64・65 バイト（詰め物が次のブロックへはみ出す境目）
        let want = [
            (55, "C1C8BBDC22796E28C0E15163D20899B65621D65A"),
            (56, "C2DB330F6083854C99D4B5BFB6E8F29F201BE699"),
            (63, "03F09F5B158A7A8CDAD920BDDC29B81C18A551F5"),
            (64, "0098BA824B5C16427BD7A1122A5A442A25EC644D"),
            (65, "11655326C708D70319BE2610E8A57D9A5B959D3B"),
        ];
        for (n, h) in want {
            assert_eq!(hex(&sha1(&vec![b'a'; n])), h, "{n}");
        }
    }
}
