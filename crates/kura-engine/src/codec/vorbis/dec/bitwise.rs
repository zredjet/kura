//! パケットのビットの読み手（LSb から）。libogg の bitwise.c の読み（`oggpack_readinit`・`oggpack_look`（試験だけ）・
//! `oggpack_read`・`oggpack_read1`・`oggpack_bytes`・`oggpack_bits`。ogg_vendor/src/bitwise.c:258–508。`oggpack_adv` は
//! 読み取りの経路が使わないので移さない）と、aoTuV の
//! codebook.c の覗き（`vorbis_oggpack_look`・`vorbis_oggpack_adv`。codebook.c:114–155。codebook の復号だけが使う）。
//!
//! 型：C の `oggpack_buffer` の `endbyte`・`storage` は `long`（i64）、`endbit` は `int`（i32）。`ptr` は `endbyte` から
//! 決まるので持たない（あふれたあとの `ptr = NULL` は、どの読みも範囲の確かめで −1 を返す形で写る）。
//!
//! 違い：C はどの枝も範囲の外を読まない（確かめてから読む）が、移植はバイトを `get` で読み、範囲の外は 0 にする（届かない）。

/// `mask[]`（bitwise.c:27–34）。
const MASK: [u64; 33] = {
    let mut m = [0u64; 33];
    let mut i = 1;
    while i < 33 {
        m[i] = (1u64 << i) - 1;
        i += 1;
    }
    m
};

/// `oggpack_buffer`（読み）。
#[derive(Clone, Copy)]
pub(crate) struct OggpackReader<'a> {
    buf: &'a [u8],
    pub endbyte: i64,
    pub endbit: i32,
    pub storage: i64,
}

impl<'a> OggpackReader<'a> {
    /// `oggpack_readinit`（bitwise.c:258–262）。
    pub fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            endbyte: 0,
            endbit: 0,
            storage: buf.len() as i64,
        }
    }

    fn byte(&self, k: i64) -> u64 {
        usize::try_from(self.endbyte + k)
            .ok()
            .and_then(|i| self.buf.get(i))
            .map_or(0, |b| u64::from(*b))
    }

    /// 今の位置から（C の `ret=b->ptr[0]>>b->endbit; …`）`bits`（endbit を足した数）ビット分を集める。呼び手はどれも
    /// 求めた幅（`bits − endbit` ≤ 32）で切るので、8 バイトが範囲の内なら 8 バイトをまとめて読んで `endbit` だけずらす
    /// （切ったあとの値は C の集め方と同じ。集めないバイトは切る幅の外）。
    fn gather(&self, bits: i32) -> u64 {
        let e = self.endbit as u32;
        if let Some(w) = usize::try_from(self.endbyte)
            .ok()
            .and_then(|i| self.buf.get(i..i.checked_add(8)?))
            .and_then(|w| <[u8; 8]>::try_from(w).ok())
        {
            return u64::from_le_bytes(w) >> e;
        }
        let mut ret = self.byte(0) >> e;
        if bits > 8 {
            ret |= self.byte(1) << (8 - e);
            if bits > 16 {
                ret |= self.byte(2) << (16 - e);
                if bits > 24 {
                    ret |= self.byte(3) << (24 - e);
                    if bits > 32 && e != 0 {
                        ret |= self.byte(4) << (32 - e);
                    }
                }
            }
        }
        ret
    }

    /// あふれ（bitwise.c の `overflow:`）。
    fn overflow(&mut self) {
        self.endbyte = self.storage;
        self.endbit = 1;
    }

    /// `oggpack_look`（bitwise.c:269–329）。（読み取りの経路では使わない。照合の試験が使う）
    #[cfg(test)]
    pub fn look(&self, bits: i32) -> i64 {
        if !(0..=32).contains(&bits) {
            return -1;
        }
        let m = MASK[bits as usize];
        let bits = bits + self.endbit;
        if self.endbyte >= self.storage - 4 {
            if self.endbyte > self.storage - i64::from((bits + 7) >> 3) {
                return -1;
            } else if bits == 0 {
                return 0;
            }
        }
        (m & self.gather(bits)) as i64
    }

    /// `oggpack_read`（bitwise.c:374–457）。あふれたら −1。
    pub fn read(&mut self, bits: i32) -> i64 {
        if !(0..=32).contains(&bits) {
            self.overflow();
            return -1;
        }
        let m = MASK[bits as usize];
        let bits = bits + self.endbit;
        if self.endbyte >= self.storage - 4 {
            if self.endbyte > self.storage - i64::from((bits + 7) >> 3) {
                self.overflow();
                return -1;
            } else if bits == 0 {
                return 0;
            }
        }
        let ret = self.gather(bits) & m;
        self.endbyte += i64::from(bits / 8);
        self.endbit = bits & 7;
        ret as i64
    }

    /// `oggpack_read1`（bitwise.c:460–479）。（読み取りの経路では使わない。照合の試験が使う）
    #[cfg(test)]
    pub fn read1(&mut self) -> i64 {
        if self.endbyte >= self.storage {
            self.overflow();
            return -1;
        }
        let ret = (self.byte(0) >> self.endbit) & 1;
        self.endbit += 1;
        if self.endbit > 7 {
            self.endbit = 0;
            self.endbyte += 1;
        }
        ret as i64
    }

    /// `oggpack_bytes`（bitwise.c:502–504）。
    pub fn bytes(&self) -> i64 {
        self.endbyte + i64::from((self.endbit + 7) / 8)
    }

    /// `oggpack_bits`（bitwise.c:506–508）。（読み取りの経路では使わない。照合の試験が使う）
    #[cfg(test)]
    pub fn bits(&self) -> i64 {
        self.endbyte * 8 + i64::from(self.endbit)
    }

    /// aoTuV の `vorbis_oggpack_look`（codebook.c:114–147。SSE・MSVC でない経路）：範囲の確かめが libogg と違う。
    pub fn vlook(&self, bits: i32) -> i64 {
        let m = MASK[bits.clamp(0, 32) as usize];
        let bits = bits + self.endbit;
        if self.endbyte + 4 >= self.storage && self.endbyte * 8 + i64::from(bits) > self.storage * 8
        {
            return -1;
        }
        (m & self.gather(bits)) as i64
    }

    /// aoTuV の `vorbis_oggpack_adv`（codebook.c:149–154）：範囲を確かめずに進める。
    pub fn vadv(&mut self, bits: i32) {
        let bits = bits + self.endbit;
        self.endbyte += i64::from(bits / 8);
        self.endbit = bits & 7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vorbis::bitpack::OggpackBuffer;

    /// 書き手（移植済みの `oggpack_write`）で書いたものを読み戻す。
    #[test]
    fn reads_back_what_the_writer_wrote() {
        let mut w = OggpackBuffer::oggpack_writeinit();
        let items: Vec<(u64, i32)> = (0..400)
            .map(|i| {
                let bits: i32 = i * 7 % 33;
                let v = (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) & MASK[bits as usize];
                (v, bits)
            })
            .collect();
        for (v, bits) in &items {
            w.oggpack_write(*v, *bits as u32).unwrap();
        }
        let bytes = w.oggpack_get_buffer().to_vec();
        let mut r = OggpackReader::new(&bytes);
        for (v, bits) in &items {
            assert_eq!(r.look(*bits), *v as i64);
            assert_eq!(r.vlook(*bits), *v as i64);
            assert_eq!(r.read(*bits), *v as i64);
        }
        // 終わりを越えると −1 のまま
        assert_eq!(r.read(32), -1);
        assert_eq!(r.read(1), -1);
        assert_eq!(r.read1(), -1);
        assert_eq!(r.look(0), -1);
    }

    #[test]
    fn end_of_packet_rules() {
        let bytes = [0xffu8, 0x01];
        let mut r = OggpackReader::new(&bytes);
        assert_eq!(r.read(9), 0x1ff);
        assert_eq!(r.bits(), 9);
        assert_eq!(r.bytes(), 2);
        assert_eq!(r.read(7), 0);
        // ちょうど終わりで 0 ビットを読むと 0（bitwise.c:393 の `else if(!bits)return(0L);`）、1 ビットでも越えれば −1
        assert_eq!(r.read(0), 0);
        assert_eq!(r.read(1), -1);
        let mut r = OggpackReader::new(&bytes);
        assert_eq!(r.read(16), 0x01ff);
        assert_eq!(r.read(0), 0);
        assert_eq!(r.read1(), -1);
        // aoTuV の覗きは、ちょうど終わりまでなら読め、1 ビットでも越えれば −1
        let r = OggpackReader::new(&bytes);
        assert_eq!(r.vlook(16), 0x01ff);
        assert_eq!(r.vlook(17), -1);
    }
}
