//! ALAC の読み取り（自作。M16 段 3・ADR-219）：magic cookie とパケットのデコード。
//!
//! 識別子 `m4a-alac-symphonia-dec/1` を変えない（ADR-216 (2)）ので、symphonia-codec-alac 0.6.1（と symphonia-common の
//! cookie の読み）と、受け付ける・断る判断、標本（32 ビットに左詰めした i32）、チャンネルの並び、フレームの数が同じになる
//! ように書く。正しいストリームでは Apple の参照実装・ffmpeg とも同じ値になる。違うのは非標準・壊れたストリームだけで、
//! どれも symphonia に合わせる（照合は dev 依存に残した symphonia と `tests`）：
//!
//! - チャンネルの並び：要素（SCE・CPE・LFE）に出た順の ALAC のチャンネル `k` を、面 [`CHANNEL_MAP`]`[n − 1][k]` に書く。
//!   7・8 チャンネルは symphonia の並び（7：L R C LFE Cs Ls Rs、8：L R C LFE Lc Rc Ls Rs）のまま。
//! - 予測：order 31 でも 1 次の積分のあとに立ち上がりと 31 次の予測を回す。mode 15 でも order 0 なら何もしない。mode 1〜14 は
//!   誤り。係数は i32 のまま増減する。
//! - 残差：ゼロの連が列の終わりを越えても誤りにしない。履歴の上限の判定は toggle を足したあとの値。`mb` は u32。
//! - 要素：END がなくても、CPE が入りきらなくても（そこで止まる）、要素ごとに標本の数が違っても（最後の要素の数）誤りに
//!   しない。CCE・PCE は誤り。
//! - 算術：symphonia が折り返さない演算（予測の差・係数の適応・ステレオの戻し）も、アプリ（release）の symphonia と同じく
//!   2³² で折り返す（壊れたストリームでだけ起きる。debug の symphonia は panic した）。
//!
//! 誤りの種類：cookie の版・大きさ・frame_length が 65,536 を超えるものは `Unsupported`（次の段へ）、ほかは `Decode`。

use crate::error::EngineError;

/// 読み取りの約束の版。
pub(crate) const DEC_ID: &str = "m4a-alac-symphonia-dec/1";

/// 1 パケットのフレームの数の上限（symphonia と同じ 4,096 × 16）。
pub(crate) const MAX_FRAME_LENGTH: u32 = 65_536;

/// ALAC のチャンネル `k`（要素に出た順）を書く面（チャンネルの数 − 1 で引く。symphonia と同じ）。
pub(crate) const CHANNEL_MAP: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[2, 0, 1],
    &[2, 0, 1, 3],
    &[2, 0, 1, 3, 4],
    &[2, 0, 1, 4, 5, 3],
    &[2, 0, 1, 5, 6, 4, 3],
    &[2, 4, 5, 0, 1, 6, 7, 3],
];

/// 48 バイトの cookie の `chan` の配置のタグ（チャンネルの数 − 1 で引く）。
const LAYOUT_TAGS: [u32; 8] = [
    0x0064_0001,
    0x0065_0002,
    0x0071_0003,
    0x0074_0004,
    0x0078_0005,
    0x007c_0006,
    0x008e_0007,
    0x007f_0008,
];

fn decode_err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: format!("ALAC：{}", message.into()),
    }
}

fn unsupported(message: &str) -> EngineError {
    EngineError::Unsupported(format!("{DEC_ID}: {message}"))
}

fn eob() -> EngineError {
    decode_err("パケットが途中で切れています")
}

/// magic cookie（`ALACSpecificConfig`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cookie {
    pub frame_length: u32,
    pub bit_depth: u8,
    pub pb: u8,
    pub mb: u8,
    pub kb: u8,
    pub channels: u8,
    pub sample_rate: u32,
}

impl Cookie {
    /// 24 か 48 バイト（`frma`・`alac` の 12 バイトの頭があればこの順に 1 回ずつ剥がす）。
    pub fn parse(buf: &[u8]) -> Result<Self, EngineError> {
        if buf.len() < 24 {
            return Err(unsupported("magic cookie が短すぎます"));
        }
        let mut b = buf;
        if b.get(4..8) == Some(b"frma") {
            b = &b[12..];
        }
        if b.get(4..8) == Some(b"alac") {
            b = &b[12..];
        }
        if b.len() != 24 && b.len() != 48 {
            return Err(unsupported("magic cookie の大きさが正しくありません"));
        }
        let be32 = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let c = Cookie {
            frame_length: be32(0),
            bit_depth: b[5],
            pb: b[6],
            mb: b[7],
            kb: b[8],
            channels: b[9],
            sample_rate: be32(20),
        };
        if b[4] > 0 {
            return Err(unsupported("ALAC の版 0 と互換ではありません"));
        }
        if c.bit_depth > 32 {
            return Err(decode_err("ビットの深さが正しくありません"));
        }
        if c.channels == 0 || c.channels > 8 {
            return Err(decode_err("チャンネルの数が正しくありません"));
        }
        if b.len() == 48 {
            if be32(24) != 24 {
                return Err(decode_err("チャンネルの配置の大きさが正しくありません"));
            }
            if &b[28..32] != b"chan" {
                return Err(decode_err("チャンネルの配置の識別子が正しくありません"));
            }
            if be32(32) != 0 {
                return Err(decode_err("チャンネルの配置の版が正しくありません"));
            }
            let Some(n) = LAYOUT_TAGS.iter().position(|t| *t == be32(36)) else {
                return Err(decode_err("チャンネルの配置のタグが正しくありません"));
            };
            if n + 1 != usize::from(c.channels) {
                return Err(decode_err("チャンネルの数が配置と合いません"));
            }
            if be32(40) != 0 || be32(44) != 0 {
                return Err(decode_err("チャンネルの配置の予約が 0 ではありません"));
            }
        }
        Ok(c)
    }
}

/// MSB から読むビットの読み手（読み終えた先は 0。足りなければ誤り）。
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    fn remaining(&self) -> usize {
        (self.b.len() * 8).saturating_sub(self.pos)
    }

    /// 今の位置から 64 ビット（先は 0）。上位に詰める。
    #[inline]
    fn window(&self) -> u64 {
        let at = self.pos / 8;
        let w = match self.b.get(at..at + 8) {
            Some(s) => u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]),
            None => {
                let mut t = [0u8; 8];
                let tail = self.b.get(at..).unwrap_or(&[]);
                t[..tail.len()].copy_from_slice(tail);
                u64::from_be_bytes(t)
            }
        };
        w << (self.pos % 8)
    }

    /// `w`（0〜32）ビットを符号なしで。残りが足りなければ誤り。
    #[inline]
    fn read(&mut self, w: u32) -> Result<u32, EngineError> {
        if w == 0 {
            return Ok(0);
        }
        if self.remaining() < w as usize {
            return Err(eob());
        }
        let v = (self.window() >> (64 - w)) as u32;
        self.pos += w as usize;
        Ok(v)
    }

    /// `w` ビットを符号つきで（`w` = 0 は 0、32 はそのまま）。
    fn read_signed(&mut self, w: u32) -> Result<i32, EngineError> {
        let v = self.read(w)?;
        let shift = (32 - w) % 32;
        Ok((v.wrapping_shl(shift) as i32) >> shift)
    }

    fn bit(&mut self) -> Result<bool, EngineError> {
        Ok(self.read(1)? == 1)
    }

    /// 1 のビットを最大 9 個まで数える（9 に達したら止める。9 未満で当たった 0 も読む）。
    #[inline]
    fn ones9(&mut self) -> Result<u32, EngineError> {
        let w = self.window();
        let ones = (w.leading_ones()).min(9);
        let need = if ones == 9 { 9 } else { ones as usize + 1 };
        if self.remaining() < need {
            return Err(eob());
        }
        self.pos += need;
        Ok(ones)
    }

    fn skip(&mut self, m: usize) -> Result<(), EngineError> {
        if self.remaining() < m {
            return Err(eob());
        }
        self.pos += m;
        Ok(())
    }

    fn realign(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    /// 適応 Golomb-Rice の 1 つの値（`w` は escape の幅）。1 つの値が読むのは最大 9 + 32 ビットなので、残りが 64 ビット
    /// 以上なら 1 つの窓から読む（読むビットと値は下の 1 つずつの形と同じ）。
    #[inline]
    fn rice(&mut self, k: u32, w: u32) -> Result<u32, EngineError> {
        if self.remaining() >= 64 && w > 0 {
            let win = self.window();
            let p = win.leading_ones().min(9);
            if p == 9 {
                self.pos += 9 + w as usize;
                return Ok(((win << 9) >> (64 - w)) as u32);
            }
            let rest = win << (p + 1);
            return Ok(match k {
                0 => {
                    self.pos += p as usize + 1;
                    0
                }
                1 => {
                    self.pos += p as usize + 1;
                    p
                }
                _ => {
                    // s が 0 なら k − 1 ビット、そうでなければもう 1 ビット b を読んで 2s + b − 1 を足す（分岐なしで同じ値）
                    let base = p * ((1 << k) - 1);
                    let s = (rest >> (64 - (k - 1))) as u32;
                    let nz = u32::from(s != 0);
                    let b = ((rest << (k - 1)) >> 63) as u32;
                    self.pos += (p + k + nz) as usize;
                    base + (2 * s + b).wrapping_sub(1).wrapping_mul(nz)
                }
            });
        }
        let p = self.ones9()?;
        if p == 9 {
            return self.read(w);
        }
        match k {
            0 => Ok(0),
            1 => Ok(p),
            _ => {
                let base = p * ((1 << k) - 1);
                let s = self.read(k - 1)?;
                if s == 0 {
                    return Ok(base);
                }
                let b = self.read(1)?;
                Ok(base + (2 * s + b) - 1)
            }
        }
    }
}

/// チャンネルごとの予測の欄。
#[derive(Debug, Clone, Default)]
struct ChannelHead {
    mode: u32,
    ds: u32,
    pb_factor: u32,
    order: usize,
    coefs: [i32; 32],
}

/// ALAC のデコーダ（パケットの間に持ち越す状態はない）。
pub(crate) struct AlacDecoder {
    pub cookie: Cookie,
    /// 出力の面（チャンネル × frame_length。32 ビットに左詰めした標本）。
    pub planes: Vec<Vec<i32>>,
    /// tail（下位のビット。最大 2 チャンネル × frame_length）。
    tail: Vec<u16>,
}

impl AlacDecoder {
    /// cookie を読み、緩衝を確保する（frame_length が [`MAX_FRAME_LENGTH`] を超えれば `Unsupported`）。
    pub fn new(cookie: &[u8]) -> Result<Self, EngineError> {
        let cookie = Cookie::parse(cookie)?;
        if cookie.frame_length > MAX_FRAME_LENGTH {
            return Err(unsupported("1 パケットのフレームが多すぎます"));
        }
        let n = cookie.frame_length as usize;
        let ch = usize::from(cookie.channels);
        let mut planes = Vec::with_capacity(ch);
        for _ in 0..ch {
            let mut p = Vec::new();
            p.try_reserve_exact(n)
                .map_err(|_| EngineError::Limit("ALAC の緩衝を確保できません".into()))?;
            p.resize(n, 0);
            planes.push(p);
        }
        let mut tail = Vec::new();
        tail.try_reserve_exact(ch.min(2) * n)
            .map_err(|_| EngineError::Limit("ALAC の緩衝を確保できません".into()))?;
        tail.resize(ch.min(2) * n, 0);
        Ok(Self {
            cookie,
            planes,
            tail,
        })
    }

    /// 1 つのパケットを読み、フレームの数を返す（標本は [`AlacDecoder::planes`] の頭からその数だけ）。
    pub fn decode(&mut self, packet: &[u8]) -> Result<usize, EngineError> {
        let fl = self.cookie.frame_length as usize;
        for p in &mut self.planes {
            p[..fl].fill(0);
        }
        let channels = usize::from(self.cookie.channels);
        let map = CHANNEL_MAP[channels - 1];
        let mut bits = Bits::new(packet);
        let mut next = 0usize;
        let mut frames = 0usize;
        loop {
            match bits.read(3)? {
                // SCE・LFE
                0 | 3 => {
                    frames = self.element(&mut bits, map[next], None)?;
                    next += 1;
                }
                // CPE
                1 => {
                    if next + 2 > channels {
                        break;
                    }
                    frames = self.element(&mut bits, map[next], Some(map[next + 1]))?;
                    next += 2;
                }
                // DSE
                4 => {
                    bits.read(4)?;
                    let align = bits.bit()?;
                    let mut count = bits.read(8)?;
                    if count == 255 {
                        count += bits.read(8)?;
                    }
                    if align {
                        bits.realign();
                    }
                    bits.skip(count as usize * 8)?;
                }
                // FIL
                6 => {
                    let mut count = bits.read(4)?;
                    if count == 15 {
                        count = 15 + bits.read(8)? - 1;
                    }
                    bits.skip(count as usize * 8)?;
                }
                // CCE・PCE
                2 | 5 => return Err(decode_err("扱えない要素（CCE・PCE）があります")),
                // END
                _ => break,
            }
            if next >= channels {
                break;
            }
        }
        let bd = u32::from(self.cookie.bit_depth);
        if bd < 32 {
            let shift = 32 - bd;
            for p in &mut self.planes {
                for s in &mut p[..frames] {
                    *s = s.wrapping_shl(shift);
                }
            }
        }
        Ok(frames)
    }

    /// SCE・LFE（`b` なし）か CPE の 1 つの要素。標本の数を返す。
    fn element(
        &mut self,
        bits: &mut Bits<'_>,
        a: usize,
        b: Option<usize>,
    ) -> Result<usize, EngineError> {
        let cookie = self.cookie;
        let bd = u32::from(cookie.bit_depth);
        bits.read(4)?;
        if bits.read(12)? != 0 {
            return Err(decode_err("要素の頭の使われないビットが 0 ではありません"));
        }
        let partial = bits.bit()?;
        let sh = bits.read(2)? * 8;
        let escape = bits.bit()?;
        if sh >= 24 || sh >= bd {
            return Err(decode_err("ずらすビットの数が正しくありません"));
        }
        let n = if partial {
            bits.read(32)? as usize
        } else {
            cookie.frame_length as usize
        };
        if n > cookie.frame_length as usize {
            return Err(decode_err("パケットのフレームが多すぎます"));
        }
        let (pa, pb) = pair(&mut self.planes, a, b);
        if escape {
            // 非圧縮：bit_depth ビットずつ（sh は見ない）、CPE は交互
            match pb {
                None => {
                    for s in &mut pa[..n] {
                        *s = bits.read_signed(bd)?;
                    }
                }
                Some(pb) => {
                    for i in 0..n {
                        pa[i] = bits.read_signed(bd)?;
                        pb[i] = bits.read_signed(bd)?;
                    }
                }
            }
            return Ok(n);
        }
        let cpe = pb.is_some();
        let bps = bd - sh + u32::from(cpe);
        if bps > 32 {
            return Err(decode_err("符号化のビットの数が 32 を超えます"));
        }
        let mix_bits = bits.read(8)?;
        let mix_res = bits.read_signed(8)?;
        if !cpe && (mix_bits != 0 || mix_res != 0) {
            return Err(decode_err("モノラルの要素に混ぜの欄があります"));
        }
        let mut heads = [ChannelHead::default(), ChannelHead::default()];
        for h in heads.iter_mut().take(1 + usize::from(cpe)) {
            h.mode = bits.read(4)?;
            h.ds = bits.read(4)?;
            h.pb_factor = (bits.read(3)? * u32::from(cookie.pb)) >> 2;
            h.order = bits.read(5)? as usize;
            for c in h.coefs.iter_mut().take(h.order) {
                *c = bits.read_signed(16)?;
            }
        }
        let tails = if sh > 0 {
            let count = n * (1 + usize::from(cpe));
            for t in &mut self.tail[..count] {
                *t = bits.read(sh)? as u16;
            }
            true
        } else {
            false
        };
        residual(bits, &mut pa[..n], &heads[0], cookie, bps)?;
        predict(&mut pa[..n], &mut heads[0], bps)?;
        if let Some(pb) = pb {
            residual(bits, &mut pb[..n], &heads[1], cookie, bps)?;
            predict(&mut pb[..n], &mut heads[1], bps)?;
            if mix_res != 0 {
                if mix_bits > 31 {
                    return Err(decode_err("ステレオの戻しのずらしが 31 を超えます"));
                }
                for (u, v) in pa[..n].iter_mut().zip(pb[..n].iter_mut()) {
                    let l = u
                        .wrapping_add(*v)
                        .wrapping_sub(v.wrapping_mul(mix_res) >> mix_bits);
                    *u = l;
                    *v = l.wrapping_sub(*v);
                }
            }
            if tails {
                for i in 0..n {
                    pa[i] = pa[i].wrapping_shl(sh) | i32::from(self.tail[2 * i]);
                    pb[i] = pb[i].wrapping_shl(sh) | i32::from(self.tail[2 * i + 1]);
                }
            }
        } else if tails {
            for (s, t) in pa[..n].iter_mut().zip(&self.tail[..n]) {
                *s = s.wrapping_shl(sh) | i32::from(*t);
            }
        }
        Ok(n)
    }
}

/// 違う 2 つの面（`b` があれば）を同時に借りる（`CHANNEL_MAP` は置換なので同じにならない）。
fn pair(planes: &mut [Vec<i32>], a: usize, b: Option<usize>) -> (&mut [i32], Option<&mut [i32]>) {
    match b {
        None => (&mut planes[a], None),
        Some(b) if a < b => {
            let (l, r) = planes.split_at_mut(b);
            (&mut l[a], Some(&mut r[0]))
        }
        Some(b) => {
            let (l, r) = planes.split_at_mut(a);
            (&mut r[0], Some(&mut l[b]))
        }
    }
}

/// `⌊log₂((x >> 9) + 3)⌋`。
#[inline]
fn lg3a(x: u32) -> u32 {
    31 - ((x >> 9) + 3).leading_zeros()
}

/// 適応 Golomb-Rice の残差を `out` に書く（`out` は 0 で埋めてある）。
fn residual(
    bits: &mut Bits<'_>,
    out: &mut [i32],
    h: &ChannelHead,
    c: Cookie,
    bps: u32,
) -> Result<(), EngineError> {
    let n = out.len();
    let kb = u32::from(c.kb);
    let mut mb = u32::from(c.mb);
    let mut toggle = 0u32;
    let mut skip_end = 0usize;
    for (i, slot) in out.iter_mut().enumerate() {
        if i < skip_end {
            continue;
        }
        let k = lg3a(mb).min(kb);
        let val = bits.rice(k, bps)?.wrapping_add(toggle);
        *slot = ((val >> 1) ^ 0u32.wrapping_sub(val & 1)) as i32;
        if val > 0xffff {
            mb = 0xffff;
        } else {
            mb = mb
                .wrapping_add(h.pb_factor * val)
                .wrapping_sub(h.pb_factor.wrapping_mul(mb) >> 9);
        }
        toggle = 0;
        if mb < 128 && i + 1 < n {
            let k = ((mb.leading_zeros() - 24) + ((mb + 16) >> 6)).min(kb);
            let z = bits.rice(k, 16)?;
            if z < 0xffff {
                toggle = 1;
            }
            mb = 0;
            skip_end = i + 1 + z as usize;
        }
    }
    Ok(())
}

/// 予測（`unpc_block` に当たる部分。symphonia の形）。
fn predict(out: &mut [i32], h: &mut ChannelHead, bps: u32) -> Result<(), EngineError> {
    if (1..=14).contains(&h.mode) {
        return Err(decode_err("予測の型が正しくありません"));
    }
    let n = out.len();
    let order = h.order;
    if order == 0 || n == 0 {
        return Ok(());
    }
    let shift = 32 - bps;
    let clip = |x: i32| x.wrapping_shl(shift) >> shift;
    if order == 31 || h.mode == 15 {
        for i in 1..n {
            out[i] = clip(out[i].wrapping_add(out[i - 1]));
        }
    }
    for i in 1..(order + 1).min(n) {
        out[i] = clip(out[i].wrapping_add(out[i - 1]));
    }
    let ds = h.ds;
    let round = (1i32 << ds) >> 1;
    for i in (order + 1)..n {
        // 過去の order + 1 個（hist[0] が base = out[i − order − 1]、hist[1 + j] が out[i − order + j]）
        let (past, cur) = out.split_at_mut(i);
        let hist = &past[i - order - 1..];
        let base = hist[0];
        let mut r = cur[0];
        let mut acc = 0i32;
        // C の形：Σ_{t=1..order} c[t−1]·(out[i−t] − base)（t = 1 が一番新しい hist[order]）
        for (c, x) in h.coefs[..order].iter().zip(hist[1..].iter().rev()) {
            acc = acc.wrapping_add(c.wrapping_mul(x.wrapping_sub(base)));
        }
        let pred = acc.wrapping_add(round) >> ds;
        cur[0] = clip(r.wrapping_add(base).wrapping_add(pred));
        // 係数の適応：j = 0..order（t = order − j。一番古い out[i − order] = hist[1] から、重み j + 1）
        let coefs = &mut h.coefs[..order];
        if r > 0 {
            for (j, x) in hist[1..].iter().enumerate() {
                let d = base.wrapping_sub(*x);
                let s = d.signum();
                let c = &mut coefs[order - 1 - j];
                *c = c.wrapping_sub(s);
                r = r.wrapping_sub(((j + 1) as i32).wrapping_mul(s.wrapping_mul(d) >> ds));
                if r <= 0 {
                    break;
                }
            }
        } else if r < 0 {
            for (j, x) in hist[1..].iter().enumerate() {
                let d = base.wrapping_sub(*x);
                let s = d.signum();
                let c = &mut coefs[order - 1 - j];
                *c = c.wrapping_add(s);
                r = r.wrapping_sub(
                    ((j + 1) as i32).wrapping_mul(s.wrapping_neg().wrapping_mul(d) >> ds),
                );
                if r >= 0 {
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::Rng;

    use super::*;

    /// MSB から書くビットの書き手。
    #[derive(Default)]
    pub(crate) struct BitWriter {
        pub b: Vec<u8>,
        at: usize,
    }

    impl BitWriter {
        pub fn put(&mut self, v: u64, n: usize) {
            for k in (0..n).rev() {
                if self.at.is_multiple_of(8) {
                    self.b.push(0);
                }
                if (v >> k) & 1 == 1 {
                    let l = self.b.len() - 1;
                    self.b[l] |= 0x80 >> (self.at % 8);
                }
                self.at += 1;
            }
        }
    }

    /// 24 バイトの cookie。
    pub(crate) fn cookie(
        frame_length: u32,
        bit_depth: u8,
        pb: u8,
        mb: u8,
        kb: u8,
        ch: u8,
    ) -> Vec<u8> {
        let mut c = frame_length.to_be_bytes().to_vec();
        c.extend_from_slice(&[0, bit_depth, pb, mb, kb, ch]);
        c.extend_from_slice(&255u16.to_be_bytes());
        c.extend_from_slice(&[0; 8]);
        c.extend_from_slice(&44_100u32.to_be_bytes());
        c
    }

    /// 結果（標本か、誤りの種類）を畳んだダイジェスト（M16 段 10。段 3〜9 は前の読み手（symphonia の `AlacDecoder`）と
    /// 1 つずつ照らしていた。外したあとは、照らし終えた結果を凍結する。`codec::frozen` と同じ考え）。
    fn fold(results: &[Result<Vec<Vec<i32>>, bool>]) -> String {
        let mut h = crate::digest::Hasher::new("kura-test-frozen-alac-v1");
        for r in results {
            match r {
                Ok(planes) => {
                    h.u8(1).u64(planes.len() as u64);
                    for p in planes {
                        h.u64(p.len() as u64);
                        for v in p {
                            h.u32(*v as u32);
                        }
                    }
                }
                Err(unsupported) => {
                    h.u8(if *unsupported { 2 } else { 0 });
                }
            }
        }
        h.finish().to_hex()
    }

    /// 自作の読み手。標本（面ごと）か誤り（`Unsupported` なら真）。
    pub(crate) fn ours(cookie: &[u8], packet: &[u8]) -> Result<Vec<Vec<i32>>, bool> {
        let unsup = |e: EngineError| matches!(e, EngineError::Unsupported(_));
        let mut dec = AlacDecoder::new(cookie).map_err(unsup)?;
        let n = dec.decode(packet).map_err(unsup)?;
        Ok(dec.planes.iter().map(|p| p[..n].to_vec()).collect())
    }

    /// 1 つの要素の頭（非圧縮か圧縮）を書く。
    fn element_head(
        w: &mut BitWriter,
        tag: u64,
        partial: Option<u32>,
        sh_bytes: u64,
        escape: bool,
    ) {
        w.put(tag, 3);
        w.put(0, 4);
        w.put(0, 12);
        w.put(u64::from(partial.is_some()), 1);
        w.put(sh_bytes, 2);
        w.put(u64::from(escape), 1);
        if let Some(n) = partial {
            w.put(u64::from(n), 32);
        }
    }

    /// 仕様の書き出しの実測（spec-alac §7 の値）：非圧縮・ずらしの無視・8 ビット・order 31・mode 15・32 ビットの折り返し。
    #[test]
    fn measured_vectors() {
        // 16 ビット・非圧縮で sh = 8：bit_depth ビットずつ読み、sh を無視する
        let c = cookie(4, 16, 40, 10, 14, 1);
        let mut w = BitWriter::default();
        element_head(&mut w, 0, None, 1, true);
        for v in [1u64, 2, 3, 0xffff] {
            w.put(v, 16);
        }
        w.put(7, 3);
        let got = ours(&c, &w.b).unwrap();
        assert_eq!(
            got[0].iter().map(|s| s >> 16).collect::<Vec<_>>(),
            [1, 2, 3, -1]
        );
        // 8 ビット・非圧縮
        let c = cookie(4, 8, 40, 10, 14, 1);
        let mut w = BitWriter::default();
        element_head(&mut w, 0, None, 0, true);
        for v in [0xffu64, 127, 0x80, 5] {
            w.put(v, 8);
        }
        w.put(7, 3);
        assert_eq!(
            ours(&c, &w.b).unwrap()[0],
            [-16_777_216, 2_130_706_432, -2_147_483_648, 83_886_080]
        );
        // 圧縮：mb 200・pbF 0（mb を 128 以上に固定）、残差はすべて escape（1 を 9 個と bps ビット）
        let compressed = |mode: u64, order: u64, residuals: &[u64], bd: u8, n: u32| {
            let c = cookie(n, bd, 40, 200, 14, 1);
            let mut w = BitWriter::default();
            element_head(&mut w, 0, None, 0, false);
            w.put(0, 8);
            w.put(0, 8);
            w.put(mode, 4);
            w.put(0, 4);
            w.put(0, 3);
            w.put(order, 5);
            for _ in 0..order {
                w.put(0, 16);
            }
            for r in residuals {
                w.put(0x1ff, 9);
                w.put(*r, usize::from(bd));
            }
            w.put(7, 3);
            (c, w.b)
        };
        // 残差 [1, 0, …]（符号化した値 2 → 1）
        let mut res = vec![2u64];
        res.extend(std::iter::repeat_n(0, 39));
        let shift = |v: Vec<i32>| v.iter().map(|s| s >> 16).collect::<Vec<_>>();
        // どの例も symphonia の読み取りと同じ（symphonia が出した値を書いた。段 3〜9 は symphonia とも照らしていた）
        let check = |c: &[u8], p: &[u8], want: Vec<i32>, name: &str| {
            let got = ours(c, p).unwrap();
            assert_eq!(shift(got[0].clone()), want, "{name}");
        };
        let (c, p) = compressed(0, 31, &res, 16, 40);
        let mut want: Vec<i32> = (1..=32).collect();
        want.extend([2, 4, 6, 8, 10, 12, 14, 16]);
        check(&c, &p, want, "order 31");
        let (c, p) = compressed(15, 0, &res, 16, 40);
        let mut want = vec![1];
        want.extend(std::iter::repeat_n(0, 39));
        check(&c, &p, want, "mode 15・order 0");
        let (c, p) = compressed(0, 1, &res, 16, 40);
        check(&c, &p, vec![1; 40], "order 1");
        // mode 15・order 1：係数の適応で値が育つ（段 3〜9 は symphonia と同じ値であることを見ていた。凍結した値）
        let (c, p) = compressed(15, 1, &res, 16, 40);
        assert_eq!(fold(&[ours(&c, &p)]), FROZEN_MODE15, "mode 15・order 1");
        let (c, p) = compressed(1, 0, &res, 16, 40);
        assert!(ours(&c, &p).is_err(), "mode 1");
        // 32 ビット・order 1・残差 [MAX, 1, 0, 0]（release の symphonia と同じく折り返す）
        let enc = |v: i64| ((v << 1) ^ (v >> 63)) as u64 & 0xffff_ffff;
        let (c, p) = compressed(0, 1, &[enc(i64::from(i32::MAX)), enc(1), 0, 0], 32, 4);
        assert_eq!(
            ours(&c, &p).unwrap()[0],
            [i32::MAX, i32::MIN, i32::MAX, i32::MIN]
        );
        // val = 0xFFFFFFFF は i32::MIN
        let (c, p) = compressed(0, 0, &[0xffff_ffff, 0, 0, 0], 32, 4);
        assert_eq!(ours(&c, &p).unwrap()[0], [i32::MIN, 0, 0, 0]);
        // 空のパケット・frame_length の上限
        assert!(ours(&cookie(4, 16, 40, 10, 14, 1), &[]).is_err());
        assert_eq!(ours(&cookie(65_537, 16, 40, 10, 14, 1), &[0xe0]), Err(true));
        assert_eq!(
            ours(&cookie(65_536, 16, 40, 10, 14, 1), &[0xe0]).unwrap()[0].len(),
            0
        );
    }

    /// 非圧縮の要素でチャンネルの並び（1〜8 チャンネル、Apple の要素の並び）。段 3〜9 は symphonia と照らしていた並び：
    /// 面 `p` に、要素に出た順の何番目のチャンネルが来るか。
    #[test]
    fn channel_order_matches_symphonia() {
        const ORDER: [&[u64]; 8] = [
            &[0],
            &[0, 1],
            &[1, 2, 0],
            &[1, 2, 0, 3],
            &[1, 2, 0, 3, 4],
            &[1, 2, 0, 5, 3, 4],
            &[1, 2, 0, 6, 5, 3, 4],
            &[3, 4, 0, 7, 1, 2, 5, 6],
        ];
        let layouts: [&[u64]; 8] = [
            &[0],
            &[1],
            &[0, 1],
            &[0, 1, 0],
            &[0, 1, 1],
            &[0, 1, 1, 3],
            &[0, 1, 1, 3, 0],
            &[0, 1, 1, 1, 3],
        ];
        for (i, elements) in layouts.iter().enumerate() {
            let ch = i as u8 + 1;
            let c = cookie(4, 16, 40, 10, 14, ch);
            let mut w = BitWriter::default();
            let mut k = 0u64;
            for &tag in elements.iter() {
                element_head(&mut w, tag, None, 0, true);
                let width = if tag == 1 { 2 } else { 1 };
                for _ in 0..4 {
                    for j in 0..width {
                        w.put((k + j + 1) * 100, 16);
                    }
                }
                k += width;
            }
            w.put(7, 3);
            let a = ours(&c, &w.b).unwrap();
            let order: Vec<u64> = a.iter().map(|p| (p[0] >> 16) as u64 / 100 - 1).collect();
            assert_eq!(order, ORDER[i], "{ch} チャンネル");
            for (p, &e) in a.iter().zip(ORDER[i]) {
                assert!(p.iter().all(|v| (*v >> 16) as u64 == (e + 1) * 100), "{ch}");
            }
        }
    }

    /// 乱数で作った構造のあるパケット（要素の並び・頭の欄・係数・tail・残差のビット・DSE・FIL・END・ごみ）。段 3〜9 は
    /// symphonia の読み取りと照らしていた（標本が同じか、どちらも同じ種類の誤りか。symphonia が panic した入力（debug の算術の
    /// あふれ）は比べなかった）。外したあとは結果を凍結する（[`fold`]）。
    #[test]
    fn random_packets_keep_their_values() {
        let mut rng = crate::determinism::rng(41);
        let mut results = Vec::new();
        for _ in 0..4_000 {
            let (c, p) = random_packet(&mut rng);
            results.push(ours(&c, &p));
        }
        let ok = results.iter().filter(|r| r.is_ok()).count();
        // どちらの枝も十分に通る
        assert!(ok > 1_000 && results.len() - ok > 300, "{ok}");
        assert_eq!(fold(&results), FROZEN_RANDOM);
    }

    /// 凍結した結果（[`fold`]。M16 段 10 で、symphonia と照らし終えた段 9 の実装で作った）。
    const FROZEN_MODE15: &str = "e6c19505a38cc33708f4c9fcc853a75da59f6145810b3cf6544adf3b95eae2dc";
    const FROZEN_RANDOM: &str = "60640f3966a37aca598fb5b28119d816bb6a9114f550545faf894a83838ce9a5";

    /// 乱数のパケット（cookie と中身）。
    pub(crate) fn random_packet(rng: &mut ChaCha8Rng) -> (Vec<u8>, Vec<u8>) {
        let r = |rng: &mut ChaCha8Rng, n: u32| rng.next_u32() % n;
        let ch = (r(rng, 8) + 1) as u8;
        let bd = match r(rng, 6) {
            0 => 16,
            1 => 20,
            2 => 24,
            3 => 32,
            _ => (r(rng, 33)) as u8,
        };
        let fl = match r(rng, 4) {
            0 => 4_096,
            _ => r(rng, 64) + 1,
        };
        let (pb, mb, kb) = if r(rng, 3) == 0 {
            (r(rng, 256) as u8, r(rng, 256) as u8, r(rng, 32) as u8)
        } else {
            (40, 10, 14)
        };
        let c = cookie(fl, bd, pb, mb, kb, ch);
        let mut w = BitWriter::default();
        let elements = r(rng, 6) + 1;
        for _ in 0..elements {
            let tag = match r(rng, 20) {
                0..=7 => 0,
                8..=14 => 1,
                15 => 3,
                16 => 4,
                17 => 6,
                18 => 7,
                _ => r(rng, 8),
            };
            match tag {
                0 | 1 | 3 => {
                    let cpe = tag == 1;
                    let max_sh = if bd >= 17 {
                        3
                    } else if bd >= 9 {
                        2
                    } else {
                        1
                    };
                    let sh = if r(rng, 10) == 0 {
                        r(rng, 4)
                    } else {
                        r(rng, max_sh)
                    };
                    let partial = (r(rng, 3) == 0).then(|| {
                        if r(rng, 20) == 0 {
                            fl + 1
                        } else {
                            r(rng, fl + 1)
                        }
                    });
                    let escape = r(rng, 4) == 0;
                    w.put(u64::from(tag), 3);
                    w.put(u64::from(r(rng, 16)), 4);
                    w.put(if r(rng, 50) == 0 { 1 } else { 0 }, 12);
                    w.put(u64::from(partial.is_some()), 1);
                    w.put(u64::from(sh), 2);
                    w.put(u64::from(escape), 1);
                    if let Some(n) = partial {
                        w.put(u64::from(n), 32);
                    }
                    let n = partial.unwrap_or(fl).min(fl);
                    if escape {
                        for _ in 0..n * (1 + u32::from(cpe)) {
                            w.put(u64::from(rng.next_u32()), usize::from(bd));
                        }
                    } else {
                        w.put(
                            if cpe {
                                u64::from(r(rng, 40))
                            } else if r(rng, 30) == 0 {
                                1
                            } else {
                                0
                            },
                            8,
                        );
                        w.put(if cpe { u64::from(r(rng, 256)) } else { 0 }, 8);
                        for _ in 0..1 + u32::from(cpe) {
                            w.put(
                                if r(rng, 8) == 0 {
                                    u64::from(r(rng, 16))
                                } else {
                                    15 * u64::from(r(rng, 2))
                                },
                                4,
                            );
                            w.put(u64::from(r(rng, 16)), 4);
                            w.put(u64::from(r(rng, 8)), 3);
                            let order = if r(rng, 6) == 0 {
                                r(rng, 32)
                            } else {
                                r(rng, 9)
                            };
                            w.put(u64::from(order), 5);
                            for _ in 0..order {
                                w.put(u64::from(rng.next_u32() & 0xffff), 16);
                            }
                        }
                        if sh > 0 {
                            for _ in 0..n * (1 + u32::from(cpe)) {
                                w.put(u64::from(rng.next_u32()), 8 * sh as usize);
                            }
                        }
                        // 残差のビット（乱数）。小さめの値が多くなるよう、0 の多い列も混ぜる
                        let bytes = r(rng, n * 4 + 8);
                        for _ in 0..bytes {
                            let v = if r(rng, 2) == 0 {
                                rng.next_u32() & r(rng, 256)
                            } else {
                                rng.next_u32()
                            };
                            w.put(u64::from(v & 0xff), 8);
                        }
                    }
                }
                4 => {
                    w.put(4, 3);
                    w.put(u64::from(r(rng, 16)), 4);
                    w.put(u64::from(r(rng, 2)), 1);
                    let count = r(rng, 300);
                    w.put(u64::from(count.min(255)), 8);
                    if count >= 255 {
                        w.put(u64::from(count - 255), 8);
                    }
                    for _ in 0..count {
                        w.put(u64::from(rng.next_u32() & 0xff), 8);
                    }
                }
                6 => {
                    w.put(6, 3);
                    let count = r(rng, 40);
                    w.put(u64::from(count.min(15)), 4);
                    if count >= 15 {
                        w.put(u64::from(count - 14), 8);
                    }
                    for _ in 0..count {
                        w.put(u64::from(rng.next_u32() & 0xff), 8);
                    }
                }
                t => w.put(u64::from(t), 3),
            }
        }
        if r(rng, 3) > 0 {
            w.put(7, 3);
        }
        let mut p = w.b;
        // 切る・ごみを足す
        match r(rng, 6) {
            0 if !p.is_empty() => {
                let cut = r(rng, p.len() as u32) as usize;
                p.truncate(cut);
            }
            1 => {
                for _ in 0..r(rng, 16) {
                    p.push(rng.next_u32() as u8);
                }
            }
            _ => {}
        }
        (c, p)
    }
}
