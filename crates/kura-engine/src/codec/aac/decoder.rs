//! AAC-LC の raw_data_block のデコード（ISO/IEC 14496-3 4.4・4.6）：要素（SCE・CPE・LFE・DSE・PCE・FIL・END）、ICS（ics_info・
//! section・scalefactor・pulse・TNS・スペクトル）、逆量子化、PNS・M/S・intensity・TNS、フィルタバンク。
//!
//! 道具の順は ISO と同じ：逆量子化 → PNS → M/S → intensity → TNS → フィルタバンク。PNS の雑音は ChaCha8（ADR-220 (3)。
//! 決まった種から流れの頭から順に使う）。M/S は両方のチャンネルの帯が普通の符号表（1〜11）のときだけ、PNS の帯は M/S の
//! 印があれば両方のチャンネルに同じ乱数の並びを使う（ISO 4.6.13.3）。

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;

use super::bits::{Bits, eob};
use super::config::Config;
use super::filterbank::Filterbank;
use super::huffman::{Spectral, books};
use super::{decode_err, unsupported};
use crate::error::EngineError;

/// 窓の並び（EIGHT_SHORT のほかは長い窓）。
const EIGHT_SHORT: u8 = 2;
/// 符号表の番号。
const ZERO_HCB: u8 = 0;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;
/// LC の TNS の次数の上限（長い窓・短い窓）。
const TNS_MAX_ORDER_LONG: usize = 12;
const TNS_MAX_ORDER_SHORT: usize = 7;
/// PNS の雑音の種（決まった値）。
const PNS_SEED: u64 = 0x6161_632d_706e_7331;

/// PNS の雑音の源。
// 小さな変種は試験だけ（本番は ChaCha8 だけなので、包まずに持つ）
#[allow(clippy::large_enum_variant)]
pub(crate) enum Noise {
    /// ChaCha8（ADR-220 (3)）。M/S の印のある帯で両方とも雑音なら、同じ並びを使う（ISO 4.6.13.3）。値は帯ごとに、乱数の
    /// バイト列（`fill_bytes`）の 1 バイト b ごとに 2b − 255（−255〜255 の奇数で、平均がちょうど 0）。帯ごとにエネルギーで
    /// 正規化するので大きさは効かず、ChaCha8 のブロックを作る手間が小さく、2 乗の和は整数でちょうど求まる。
    ChaCha(ChaCha8Rng),
    /// 試験だけ：symphonia と同じ LCG（`(state >> 16) as i16`）と、チャンネルごとに別の並び（symphonia の形。PNS のほかの
    /// 道を symphonia と照らすため）。
    #[cfg(test)]
    Lcg(u32),
    /// 試験だけ：ffmpeg（9.0 の浮動小数の読み手）と同じ LCG（状態をそのまま `i32` にした値）。チャンネルをまたいで 1 つの
    /// 並びを順に使い、M/S の印のある帯も別の並び（ffmpeg の形。PNS の帯まで ffmpeg と波形で照らすため）。
    #[cfg(test)]
    Ffmpeg(u32),
}

impl Noise {
    /// 帯 `dst` に雑音の生の値を書き、その 2 乗の和を返す。
    fn fill(&mut self, dst: &mut [f32]) -> f64 {
        match self {
            Self::ChaCha(r) => {
                let mut bytes = [0u8; 1024];
                let bytes = &mut bytes[..dst.len().min(1024)];
                r.fill_bytes(bytes);
                // x² ≤ 65,025 で、帯（1,024 まで）の和も u32 に収まる
                let mut energy = 0u32;
                for (v, b) in dst.iter_mut().zip(bytes.iter()) {
                    let x = 2 * i32::from(*b) - 255;
                    energy += (x * x) as u32;
                    *v = x as f32;
                }
                f64::from(energy)
            }
            #[cfg(test)]
            Self::Lcg(state) => {
                let mut energy = 0f64;
                for v in dst.iter_mut() {
                    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    *v = f32::from(((*state as i32) >> 16) as i16);
                    energy += f64::from(*v) * f64::from(*v);
                }
                energy
            }
            #[cfg(test)]
            Self::Ffmpeg(state) => {
                let mut energy = 0f64;
                for v in dst.iter_mut() {
                    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    *v = *state as i32 as f32;
                    energy += f64::from(*v) * f64::from(*v);
                }
                energy
            }
        }
    }

    fn correlated(&self) -> bool {
        match self {
            Self::ChaCha(_) => true,
            #[cfg(test)]
            Self::Lcg(_) | Self::Ffmpeg(_) => false,
        }
    }
}

/// 配置（channel_configuration 1〜7）ごとの要素の並びと、書く面（kura の並び：WAV のチャンネルの印の順）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Sce(usize),
    Cpe(usize, usize),
    Lfe(usize),
}

fn layout(config: u8) -> &'static [Slot] {
    use Slot::*;
    match config {
        1 => &[Sce(0)],
        2 => &[Cpe(0, 1)],
        3 => &[Sce(2), Cpe(0, 1)],
        4 => &[Sce(2), Cpe(0, 1), Sce(3)],
        5 => &[Sce(2), Cpe(0, 1), Cpe(3, 4)],
        6 => &[Sce(2), Cpe(0, 1), Cpe(4, 5), Lfe(3)],
        _ => &[Sce(2), Cpe(6, 7), Cpe(0, 1), Cpe(4, 5), Lfe(3)],
    }
}

/// ics_info。
#[derive(Debug, Clone, Copy, Default)]
struct Info {
    seq: u8,
    shape: u8,
    max_sfb: usize,
    groups: usize,
    group_len: [usize; 8],
}

impl Info {
    fn short(&self) -> bool {
        self.seq == EIGHT_SHORT
    }

    fn windows(&self) -> usize {
        if self.short() { 8 } else { 1 }
    }
}

/// 1 つの TNS の濾波器。
#[derive(Debug, Clone, Copy, Default)]
struct TnsFilter {
    length: usize,
    order: usize,
    down: bool,
    /// LPC の係数 a[1..=order]（a[0] = 1）。
    lpc: [f32; TNS_MAX_ORDER_LONG + 1],
}

/// 1 つのチャンネルの ICS。
struct Ics {
    info: Info,
    band_type: [[u8; 64]; 8],
    /// スケールファクター・雑音のエネルギー・intensity の位置（帯の種類で意味が違う）。
    sf: [[i32; 64]; 8],
    /// 窓ごとの TNS の濾波器（数と並び）。
    tns: [[TnsFilter; 4]; 8],
    tns_count: [usize; 8],
    /// スペクトル（窓の順。短い窓は 128 ずつ）。
    spec: [f32; 1024],
    /// 量子化した値（pulse を足す間だけ）。
    quant: [i32; 1024],
}

impl Ics {
    fn new() -> Box<Self> {
        Box::new(Self {
            info: Info::default(),
            band_type: [[0; 64]; 8],
            sf: [[0; 64]; 8],
            tns: [[TnsFilter::default(); 4]; 8],
            tns_count: [0; 8],
            spec: [0.0; 1024],
            quant: [0; 1024],
        })
    }
}

/// チャンネルの状態（重ね合わせの後ろ半分と、前のフレームの窓の形）。
struct Channel {
    overlap: Vec<f32>,
    prev_shape: u8,
}

/// AAC-LC のデコーダ。
pub(crate) struct AacDecoder {
    cfg: Config,
    swb_long: &'static [usize],
    swb_short: &'static [usize],
    tns_long: usize,
    tns_short: usize,
    fb: Filterbank,
    states: Vec<Channel>,
    pub(crate) noise: Noise,
    ics: [Box<Ics>; 2],
    /// 出す標本（面 × 1024）。
    pub planes: Vec<Vec<f32>>,
    /// x^(4/3)（0〜8191）。
    pow43: Vec<f32>,
    /// 2^((sf − 100)/4)（sf 0〜255）。
    gain: Vec<f32>,
}

impl AacDecoder {
    pub fn new(cfg: Config) -> Result<Self, EngineError> {
        let index = aac_tables::SUBBAND_INFO
            .iter()
            .position(|(min, _, _)| cfg.rate >= *min)
            .ok_or_else(|| decode_err("サンプルレートが正しくありません"))?;
        let (_, swb_long, swb_short) = aac_tables::SUBBAND_INFO[index];
        let pow43 = (0..8192)
            .map(|q| crate::math::pow(f64::from(q), 4.0 / 3.0) as f32)
            .collect();
        let gain = (0..256)
            .map(|sf| crate::math::pow(2.0, (f64::from(sf) - 100.0) / 4.0) as f32)
            .collect();
        Ok(Self {
            cfg,
            swb_long,
            swb_short,
            tns_long: aac_tables::TNS_MAX_LONG_BANDS[index],
            tns_short: aac_tables::TNS_MAX_SHORT_BANDS[index],
            fb: Filterbank::new(),
            states: (0..cfg.channels)
                .map(|_| Channel {
                    overlap: vec![0.0; 1024],
                    prev_shape: 0,
                })
                .collect(),
            noise: Noise::ChaCha(crate::determinism::rng(PNS_SEED)),
            ics: [Ics::new(), Ics::new()],
            planes: vec![vec![0.0; 1024]; cfg.channels],
            pow43,
            gain,
        })
    }

    /// 1 つの raw_data_block を読み、1,024 フレームを [`AacDecoder::planes`] に書く。SBR の拡張（帯域の中の HE-AAC の印）・
    /// CCE・配置と合わない要素の並びは `Unsupported`、壊れたものは `Decode`。
    pub fn decode(&mut self, packet: &[u8]) -> Result<usize, EngineError> {
        let mut bits = Bits::new(packet);
        let slots = layout(self.cfg.layout);
        let mut next = 0usize;
        loop {
            match bits.read(3)? {
                // SCE・LFE
                id @ (0 | 3) => {
                    bits.read(4)?;
                    let plane = match (slots.get(next), id) {
                        (Some(Slot::Sce(p)), 0) | (Some(Slot::Lfe(p)), 3) => *p,
                        _ => return Err(mismatch()),
                    };
                    next += 1;
                    self.single(&mut bits, plane)?;
                }
                // CPE
                1 => {
                    bits.read(4)?;
                    let Some(Slot::Cpe(a, b)) = slots.get(next).copied() else {
                        return Err(mismatch());
                    };
                    next += 1;
                    self.pair(&mut bits, a, b)?;
                }
                2 => return Err(unsupported("CCE（coupling）のある AAC は次の段で読みます")),
                // DSE
                4 => {
                    bits.read(4)?;
                    let align = bits.bit()?;
                    let mut count = bits.read(8)?;
                    if count == 255 {
                        count += bits.read(8)?;
                    }
                    if align {
                        bits.align();
                    }
                    bits.skip(count as usize * 8)?;
                }
                // PCE（配置は channel_configuration で決まっているので読み飛ばす）
                5 => skip_pce(&mut bits)?,
                // FIL
                6 => {
                    let mut count = bits.read(4)? as usize;
                    if count == 15 {
                        // cnt = 15 + esc_count − 1（esc_count は 0 でもよい）
                        count = 14 + bits.read(8)? as usize;
                    }
                    if count > 0 {
                        let ext = bits.read(4)?;
                        if ext == 13 || ext == 14 {
                            return Err(unsupported("HE-AAC（SBR）は次の段で読みます"));
                        }
                        bits.skip(count * 8 - 4)?;
                    }
                }
                // END
                _ => break,
            }
        }
        if next != slots.len() {
            return Err(mismatch());
        }
        Ok(1024)
    }

    /// SCE・LFE。
    fn single(&mut self, bits: &mut Bits<'_>, plane: usize) -> Result<(), EngineError> {
        let [ics, _] = &mut self.ics;
        let ctx = Ctx {
            swb_long: self.swb_long,
            swb_short: self.swb_short,
            pow43: &self.pow43,
            gain: &self.gain,
        };
        ctx.ics(bits, None, ics)?;
        pns(&ctx, ics, None, &mut self.noise, &[[false; 64]; 8]);
        tns(&ctx, ics, self.tns_long, self.tns_short);
        self.synth(0, plane);
        Ok(())
    }

    /// CPE。
    fn pair(&mut self, bits: &mut Bits<'_>, a: usize, b: usize) -> Result<(), EngineError> {
        let ctx = Ctx {
            swb_long: self.swb_long,
            swb_short: self.swb_short,
            pow43: &self.pow43,
            gain: &self.gain,
        };
        let common = bits.bit()?;
        let mut ms = [[false; 64]; 8];
        let mut ms_present = 0;
        let info = if common {
            let info = ctx.ics_info(bits)?;
            ms_present = bits.read(2)?;
            match ms_present {
                0 => {}
                1 => {
                    for row in ms.iter_mut().take(info.groups) {
                        for used in row.iter_mut().take(info.max_sfb) {
                            *used = bits.bit()?;
                        }
                    }
                }
                2 => {
                    for row in ms.iter_mut().take(info.groups) {
                        row[..info.max_sfb].fill(true);
                    }
                }
                _ => return Err(decode_err("ms_mask_present が正しくありません")),
            }
            Some(info)
        } else {
            None
        };
        let [l, r] = &mut self.ics;
        ctx.ics(bits, info.as_ref(), l)?;
        ctx.ics(bits, info.as_ref(), r)?;
        let pair_ms = if common { ms } else { [[false; 64]; 8] };
        pns(&ctx, l, Some(r), &mut self.noise, &pair_ms);
        if common {
            mid_side(&ctx, l, r, &ms);
            intensity(&ctx, l, r, &ms, ms_present == 1);
        }
        tns(&ctx, l, self.tns_long, self.tns_short);
        tns(&ctx, r, self.tns_long, self.tns_short);
        self.synth(0, a);
        self.synth(1, b);
        Ok(())
    }

    /// `ics[i]` のスペクトルを面 `plane` の 1,024 フレームにする（フィルタバンクと重ね合わせ）。
    fn synth(&mut self, i: usize, plane: usize) {
        let ics = &self.ics[i];
        let st = &mut self.states[plane];
        self.fb.synthesize(
            &ics.spec,
            ics.info.seq,
            ics.info.shape,
            st.prev_shape,
            &mut st.overlap,
            &mut self.planes[plane],
        );
        st.prev_shape = ics.info.shape;
    }
}

/// ICS を読むのに使う表。
struct Ctx<'a> {
    swb_long: &'static [usize],
    swb_short: &'static [usize],
    pow43: &'a [f32],
    gain: &'a [f32],
}

impl Ctx<'_> {
    /// 窓の帯の区切り（長い窓か短い窓）。
    fn swb(&self, info: &Info) -> &'static [usize] {
        if info.short() {
            self.swb_short
        } else {
            self.swb_long
        }
    }

    fn ics_info(&self, bits: &mut Bits<'_>) -> Result<Info, EngineError> {
        if bits.bit()? {
            return Err(decode_err("ics_reserved_bit が 0 ではありません"));
        }
        let mut info = Info {
            seq: bits.read(2)? as u8,
            shape: bits.read(1)? as u8,
            ..Info::default()
        };
        if info.short() {
            info.max_sfb = bits.read(4)? as usize;
            let grouping = bits.read(7)?;
            info.groups = 1;
            info.group_len[0] = 1;
            for k in 0..7 {
                if grouping & (1 << (6 - k)) != 0 {
                    info.group_len[info.groups - 1] += 1;
                } else {
                    info.groups += 1;
                    info.group_len[info.groups - 1] = 1;
                }
            }
        } else {
            info.max_sfb = bits.read(6)? as usize;
            info.groups = 1;
            info.group_len[0] = 1;
            if bits.bit()? {
                return Err(decode_err("AAC-LC に予測の欄があります"));
            }
        }
        if info.max_sfb > self.swb(&info).len() - 1 {
            return Err(decode_err("max_sfb が帯の数を超えます"));
        }
        Ok(info)
    }

    /// individual_channel_stream（common_window なら `common` の ics_info）。スペクトルは逆量子化まで（PNS の帯は 0）。
    fn ics(
        &self,
        bits: &mut Bits<'_>,
        common: Option<&Info>,
        ics: &mut Ics,
    ) -> Result<(), EngineError> {
        let global_gain = bits.read(8)? as i32;
        ics.info = match common {
            Some(i) => *i,
            None => self.ics_info(bits)?,
        };
        let info = ics.info;
        let swb = self.swb(&info);
        let win_len = if info.short() { 128 } else { 1024 };
        // section_data
        let (esc, sect_bits) = if info.short() { (7, 3) } else { (31, 5) };
        for g in 0..info.groups {
            ics.band_type[g].fill(ZERO_HCB);
            let mut k = 0usize;
            // 区間の数（長さ 0 の区間も構文では許される。前の symphonia と同じく 64 まで）
            let mut sections = 0usize;
            while k < info.max_sfb {
                sections += 1;
                if sections > 64 {
                    return Err(decode_err("区間が多すぎます"));
                }
                let cb = bits.read(4)? as u8;
                if cb == 12 {
                    return Err(decode_err("予約の符号表（12）があります"));
                }
                let mut len = 0usize;
                loop {
                    let inc = bits.read(sect_bits)?;
                    len += inc as usize;
                    if inc != esc {
                        break;
                    }
                }
                if k + len > info.max_sfb {
                    return Err(decode_err("区間が max_sfb を超えます"));
                }
                ics.band_type[g][k..k + len].fill(cb);
                k += len;
            }
        }
        // scale_factor_data
        let b = books();
        let mut sf = global_gain;
        let mut is_pos = 0i32;
        let mut noise = global_gain - 90;
        let mut noise_first = true;
        for g in 0..info.groups {
            for sfb in 0..info.max_sfb {
                ics.sf[g][sfb] = match ics.band_type[g][sfb] {
                    ZERO_HCB => 0,
                    INTENSITY_HCB | INTENSITY_HCB2 => {
                        is_pos += b.scf.decode(bits)? as i32 - 60;
                        is_pos
                    }
                    NOISE_HCB => {
                        if noise_first {
                            noise_first = false;
                            noise += bits.read(9)? as i32 - 256;
                        } else {
                            noise += b.scf.decode(bits)? as i32 - 60;
                        }
                        noise
                    }
                    _ => {
                        sf += b.scf.decode(bits)? as i32 - 60;
                        if !(0..256).contains(&sf) {
                            return Err(decode_err("スケールファクターが範囲の外です"));
                        }
                        sf
                    }
                };
            }
        }
        // pulse_data
        let mut pulses = [(0usize, 0i32); 4];
        let mut n_pulse = 0usize;
        if bits.bit()? {
            if info.short() {
                return Err(decode_err("短い窓に pulse があります"));
            }
            n_pulse = bits.read(2)? as usize + 1;
            let start = bits.read(6)? as usize;
            let mut k = *swb
                .get(start)
                .ok_or_else(|| decode_err("pulse の帯が範囲の外です"))?;
            for p in pulses.iter_mut().take(n_pulse) {
                k += bits.read(5)? as usize;
                let amp = bits.read(4)? as i32;
                *p = (k, amp);
            }
            if k >= 1024 {
                return Err(decode_err("pulse の位置が範囲の外です"));
            }
        }
        // tns_data
        ics.tns_count = [0; 8];
        if bits.bit()? {
            let short = info.short();
            let max_order = if short {
                TNS_MAX_ORDER_SHORT
            } else {
                TNS_MAX_ORDER_LONG
            };
            for w in 0..info.windows() {
                let n_filt = bits.read(if short { 1 } else { 2 })? as usize;
                ics.tns_count[w] = n_filt;
                if n_filt == 0 {
                    continue;
                }
                let coef_res = bits.read(1)?;
                for f in 0..n_filt {
                    let filt = &mut ics.tns[w][f];
                    filt.length = bits.read(if short { 4 } else { 6 })? as usize;
                    filt.order = bits.read(if short { 3 } else { 5 })? as usize;
                    if filt.order > max_order {
                        return Err(decode_err("TNS の次数が大きすぎます"));
                    }
                    if filt.order > 0 {
                        filt.down = bits.bit()?;
                        let compress = bits.read(1)?;
                        let width = 3 + coef_res - compress;
                        let mut refl = [0f32; TNS_MAX_ORDER_LONG];
                        let iq = (f64::from(1u32 << (coef_res + 2)) - 0.5)
                            / (std::f64::consts::PI / 2.0);
                        let iq_m = (f64::from(1u32 << (coef_res + 2)) + 0.5)
                            / (std::f64::consts::PI / 2.0);
                        for c in refl.iter_mut().take(filt.order) {
                            let v = bits.read(width)?;
                            // width ビットの 2 の補数
                            let s = ((v << (32 - width)) as i32) >> (32 - width);
                            let x = f64::from(s);
                            *c = crate::math::sin(if s >= 0 { x / iq } else { x / iq_m }) as f32;
                        }
                        // 反射係数から LPC（a[0] = 1）
                        let mut a = [0f32; TNS_MAX_ORDER_LONG + 1];
                        a[0] = 1.0;
                        for m in 1..=filt.order {
                            let k = refl[m - 1];
                            let mut next = a;
                            for i in 1..m {
                                next[i] = a[i] + k * a[m - i];
                            }
                            next[m] = k;
                            a = next;
                        }
                        filt.lpc = a;
                    }
                }
            }
        }
        // gain_control_data
        if bits.bit()? {
            return Err(unsupported(
                "gain control（SSR）のある AAC は次の段で読みます",
            ));
        }
        // spectral_data（窓の組ごとに、帯ごとに組の窓を順に）。pulse がなければ（ほとんど）読みながら逆量子化まで、あれば
        // 量子化した値に pulse を足してから
        let (pow43, gains) = (self.pow43, self.gain);
        let dequant = |gain: f32| {
            move |d: &mut f32, q: i32| -> Result<(), EngineError> {
                let Some(p) = pow43.get(q.unsigned_abs() as usize) else {
                    return Err(decode_err("量子化した値が大きすぎます"));
                };
                let v = p * gain;
                *d = if q < 0 { -v } else { v };
                Ok(())
            }
        };
        ics.spec.fill(0.0);
        if n_pulse == 0 {
            let mut win0 = 0usize;
            for g in 0..info.groups {
                for sfb in 0..info.max_sfb {
                    let cb = ics.band_type[g][sfb];
                    if matches!(cb, ZERO_HCB | NOISE_HCB | INTENSITY_HCB | INTENSITY_HCB2) {
                        continue;
                    }
                    let (lo, hi) = (swb[sfb], swb[sfb + 1]);
                    let book = &books().spectral[usize::from(cb) - 1];
                    let put = dequant(gains[ics.sf[g][sfb] as usize]);
                    for w in 0..info.group_len[g] {
                        let base = (win0 + w) * win_len;
                        spectral(
                            bits,
                            book,
                            cb == 11,
                            &mut ics.spec[base + lo..base + hi],
                            put,
                        )?;
                    }
                }
                win0 += info.group_len[g];
            }
            return Ok(());
        }
        ics.quant.fill(0);
        let mut win0 = 0usize;
        for g in 0..info.groups {
            for sfb in 0..info.max_sfb {
                let cb = ics.band_type[g][sfb];
                if matches!(cb, ZERO_HCB | NOISE_HCB | INTENSITY_HCB | INTENSITY_HCB2) {
                    continue;
                }
                let (lo, hi) = (swb[sfb], swb[sfb + 1]);
                let book = &books().spectral[usize::from(cb) - 1];
                for w in 0..info.group_len[g] {
                    let base = (win0 + w) * win_len;
                    let out = &mut ics.quant[base + lo..base + hi];
                    spectral(bits, book, cb == 11, out, |d: &mut i32, q| {
                        *d = q;
                        Ok(())
                    })?;
                }
            }
            win0 += info.group_len[g];
        }
        // pulse を量子化した値に足す（普通の帯だけ。0 は − 側）
        for &(k, amp) in pulses.iter().take(n_pulse) {
            let sfb = swb.iter().rposition(|o| *o <= k).unwrap_or(0);
            if sfb < info.max_sfb && (1..=11).contains(&ics.band_type[0][sfb]) {
                let q = &mut ics.quant[k];
                *q = if *q > 0 { *q + amp } else { *q - amp };
            }
        }
        // 逆量子化
        let mut win0 = 0usize;
        for g in 0..info.groups {
            for sfb in 0..info.max_sfb {
                if !(1..=11).contains(&ics.band_type[g][sfb]) {
                    continue;
                }
                let put = dequant(gains[ics.sf[g][sfb] as usize]);
                let (lo, hi) = (swb[sfb], swb[sfb + 1]);
                for w in 0..info.group_len[g] {
                    let base = (win0 + w) * win_len;
                    for k in base + lo..base + hi {
                        put(&mut ics.spec[k], ics.quant[k])?;
                    }
                }
            }
            win0 += info.group_len[g];
        }
        Ok(())
    }
}

/// 帯 `out` の符号語を表 `book` で読み、値（escape まで読んだ量子化した値）を 1 つずつ `put` に渡す（`escape` は表 11）。
/// 符号語と符号のビット（符号なしの表の 0 でない値ごとに 1 ビット）は、手元に持つ 64 ビットの窓から読み（符号語は 19 ビット、
/// 符号は 4 ビットまで。窓の残りが 32 ビットを切ったら詰め直す）、値は記号と符号のビットの組の表から引く。
fn spectral<T>(
    bits: &mut Bits<'_>,
    book: &Spectral,
    escape: bool,
    out: &mut [T],
    mut put: impl FnMut(&mut T, i32) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let dim = book.dim;
    if !out.len().is_multiple_of(dim) {
        return Err(decode_err("符号語が帯の終わりを越えます"));
    }
    let total = bits.total();
    let mut pos = bits.pos();
    let mut buf = bits.window_at(pos);
    let mut have = 64 - (pos % 8) as u32;
    for dst in out.chunks_exact_mut(dim) {
        if have < 32 {
            buf = bits.window_at(pos);
            have = 64 - (pos % 8) as u32;
        }
        let (sym, len, nz) = book
            .vlc
            .lookup(buf)
            .ok_or_else(|| decode_err("ハフマンの符号が正しくありません"))?;
        let signs = if nz == 0 {
            0
        } else {
            ((buf << len) >> (64 - nz)) as usize
        };
        let used = len + nz;
        buf <<= used;
        have -= used;
        pos += used as usize;
        if pos > total {
            return Err(eob());
        }
        let v = book
            .values(sym as usize, signs)
            .ok_or_else(|| decode_err("ハフマンの符号が正しくありません"))?;
        if escape && (v[0].unsigned_abs() == 16 || v[1].unsigned_abs() == 16) {
            // escape は一般の読み手で
            bits.set_pos(pos);
            for (d, x) in dst.iter_mut().zip(v) {
                let mut q = i32::from(*x);
                if q.abs() == 16 {
                    let mut prefix = 0u32;
                    while bits.bit()? {
                        prefix += 1;
                        if prefix > 8 {
                            return Err(decode_err("escape の値が大きすぎます"));
                        }
                    }
                    let word = bits.read(prefix + 4)? as i32;
                    let e = (1 << (prefix + 4)) + word;
                    q = if q < 0 { -e } else { e };
                }
                put(d, q)?;
            }
            pos = bits.pos();
            buf = bits.window_at(pos);
            have = 64 - (pos % 8) as u32;
        } else {
            for (d, x) in dst.iter_mut().zip(v) {
                put(d, i32::from(*x))?;
            }
        }
    }
    bits.set_pos(pos);
    Ok(())
}

/// PNS（雑音の帯）。`r` があれば CPE で、M/S の印のある帯で両方とも雑音なら同じ乱数の並びを使う。帯の値は
/// 生の雑音 × (2^(nrg/4) / √(2 乗の和))（ISO 4.6.13.3。掛ける数は f64 で求めて f32 に丸める）。
fn pns(ctx: &Ctx<'_>, l: &mut Ics, mut r: Option<&mut Ics>, rng: &mut Noise, ms: &[[bool; 64]; 8]) {
    let correlated = rng.correlated();
    let info = l.info;
    let swb = ctx.swb(&info);
    let win_len = if info.short() { 128 } else { 1024 };
    let mut buf = [0f32; 1024];
    // 2^(nrg/4)（普通の帯の利得の表 2^((sf − 100)/4) と同じ式なので、その値）。壊れた入力で範囲を外れた nrg は、ffmpeg と
    // 同じく −100〜155 に収める（倍率を有限にする。正しいファイルはこの範囲）
    let level = |nrg: i32| {
        let i = (nrg.clamp(-100, 155) + 100) as usize;
        ctx.gain.get(i).map_or(0.0, |g| f64::from(*g))
    };
    let scale = |nrg: i32, energy: f64| {
        if energy > 0.0 {
            (level(nrg) / energy.sqrt()) as f32
        } else {
            0.0
        }
    };
    let put = |dst: &mut [f32], unit: &[f32], s: f32| {
        for (d, u) in dst.iter_mut().zip(unit) {
            *d = u * s;
        }
    };
    // 2 つのチャンネルは同じ ics_info（common_window）とは限らないので、それぞれの形でたどる
    let mut win0 = 0usize;
    for (g, ms_g) in ms.iter().enumerate().take(info.groups) {
        for sfb in 0..info.max_sfb {
            if l.band_type[g][sfb] != NOISE_HCB {
                continue;
            }
            let (lo, hi) = (swb[sfb], swb[sfb + 1]);
            // CPE で M/S の印があり、右も雑音なら同じ並び
            let mut both = r
                .as_deref_mut()
                .filter(|r| correlated && ms_g[sfb] && r.band_type[g][sfb] == NOISE_HCB);
            for w in 0..info.group_len[g] {
                let base = (win0 + w) * win_len;
                let unit = &mut buf[..hi - lo];
                let energy = rng.fill(unit);
                put(
                    &mut l.spec[base + lo..base + hi],
                    unit,
                    scale(l.sf[g][sfb], energy),
                );
                if let Some(r) = both.as_deref_mut() {
                    put(
                        &mut r.spec[base + lo..base + hi],
                        unit,
                        scale(r.sf[g][sfb], energy),
                    );
                }
            }
        }
        win0 += info.group_len[g];
    }
    // 右の残りの雑音の帯（左と同じ並びにしなかったもの）
    if let Some(r) = r {
        let info = r.info;
        let swb = ctx.swb(&info);
        let win_len = if info.short() { 128 } else { 1024 };
        let mut win0 = 0usize;
        for (g, ms_g) in ms.iter().enumerate().take(info.groups) {
            for sfb in 0..info.max_sfb {
                if r.band_type[g][sfb] != NOISE_HCB
                    || (correlated && ms_g[sfb] && l.band_type[g][sfb] == NOISE_HCB)
                {
                    continue;
                }
                let (lo, hi) = (swb[sfb], swb[sfb + 1]);
                for w in 0..info.group_len[g] {
                    let base = (win0 + w) * win_len;
                    let unit = &mut buf[..hi - lo];
                    let energy = rng.fill(unit);
                    put(
                        &mut r.spec[base + lo..base + hi],
                        unit,
                        scale(r.sf[g][sfb], energy),
                    );
                }
            }
            win0 += info.group_len[g];
        }
    }
}

/// M/S（両方の帯が普通の符号表のときだけ）。
fn mid_side(ctx: &Ctx<'_>, l: &mut Ics, r: &mut Ics, ms: &[[bool; 64]; 8]) {
    let info = l.info;
    let swb = ctx.swb(&info);
    let win_len = if info.short() { 128 } else { 1024 };
    let mut win0 = 0usize;
    for (g, ms_g) in ms.iter().enumerate().take(info.groups) {
        for sfb in 0..info.max_sfb {
            if !ms_g[sfb] || l.band_type[g][sfb] >= NOISE_HCB || r.band_type[g][sfb] >= NOISE_HCB {
                continue;
            }
            let (lo, hi) = (swb[sfb], swb[sfb + 1]);
            for w in 0..info.group_len[g] {
                let base = (win0 + w) * win_len;
                for k in base + lo..base + hi {
                    let (m, s) = (l.spec[k], r.spec[k]);
                    l.spec[k] = m + s;
                    r.spec[k] = m - s;
                }
            }
        }
        win0 += info.group_len[g];
    }
}

/// intensity（右の帯が intensity の符号表のとき、左から作る）。
fn intensity(ctx: &Ctx<'_>, l: &Ics, r: &mut Ics, ms: &[[bool; 64]; 8], ms_mask: bool) {
    let info = r.info;
    let swb = ctx.swb(&info);
    let win_len = if info.short() { 128 } else { 1024 };
    let mut win0 = 0usize;
    for (g, ms_g) in ms.iter().enumerate().take(info.groups) {
        for sfb in 0..info.max_sfb {
            let cb = r.band_type[g][sfb];
            if cb != INTENSITY_HCB && cb != INTENSITY_HCB2 {
                continue;
            }
            let mut sign = if cb == INTENSITY_HCB { 1.0f32 } else { -1.0 };
            if ms_mask && ms_g[sfb] {
                sign = -sign;
            }
            // 0.5^(is_position/4)。壊れた入力で範囲を外れた位置は、ffmpeg と同じく −155〜100 に収める（倍率を有限にする）
            let pos = r.sf[g][sfb].clamp(-155, 100);
            let scale = sign * crate::math::pow(0.5, f64::from(pos) / 4.0) as f32;
            let (lo, hi) = (swb[sfb], swb[sfb + 1]);
            for w in 0..info.group_len[g] {
                let base = (win0 + w) * win_len;
                for k in base + lo..base + hi {
                    r.spec[k] = l.spec[k] * scale;
                }
            }
        }
        win0 += info.group_len[g];
    }
}

/// TNS（スペクトルに全極の濾波器）。
fn tns(ctx: &Ctx<'_>, ics: &mut Ics, max_long: usize, max_short: usize) {
    let info = ics.info;
    let swb = ctx.swb(&info);
    let (win_len, max_bands) = if info.short() {
        (128, max_short)
    } else {
        (1024, max_long)
    };
    let num_swb = swb.len() - 1;
    for w in 0..info.windows() {
        let mut top = num_swb;
        for f in 0..ics.tns_count[w] {
            let filt = &ics.tns[w][f];
            let bottom = top.saturating_sub(filt.length);
            let lim = |b: usize| swb[b.min(max_bands).min(info.max_sfb)];
            let (start, end) = (lim(bottom), lim(top));
            top = bottom;
            if filt.order == 0 || end <= start {
                continue;
            }
            let spec = &mut ics.spec[w * win_len + start..w * win_len + end];
            let mut state = [0f32; TNS_MAX_ORDER_LONG];
            let n = spec.len();
            for i in 0..n {
                let k = if filt.down { n - 1 - i } else { i };
                let mut y = spec[k];
                for (c, s) in filt.lpc[1..=filt.order].iter().zip(&state) {
                    y -= c * s;
                }
                state.copy_within(0..filt.order - 1, 1);
                state[0] = y;
                spec[k] = y;
            }
        }
    }
}

/// 要素の並びが channel_configuration の配置（ISO の表 1.19）と合わない（2 チャンネルに SCE が 2 つ・要素が足りないなど）。
/// 壊れたファイルと決めつけず、ほかの並べ方で読める次の段（AudioToolbox・ffmpeg）へ回す。
fn mismatch() -> EngineError {
    unsupported("要素の並びが配置（channel_configuration）と合わない AAC は次の段で読みます")
}

/// PCE を読み飛ばす（ISO/IEC 14496-3 4.4.1.1 program_config_element）。
fn skip_pce(bits: &mut Bits<'_>) -> Result<(), EngineError> {
    bits.read(4)?;
    bits.read(2)?;
    bits.read(4)?;
    let front = bits.read(4)?;
    let side = bits.read(4)?;
    let back = bits.read(4)?;
    let lfe = bits.read(2)?;
    let assoc = bits.read(3)?;
    let cc = bits.read(4)?;
    // mono_mixdown（4）・stereo_mixdown（4）・matrix_mixdown（2 + 1）
    for width in [4, 4, 3] {
        if bits.bit()? {
            bits.read(width)?;
        }
    }
    bits.skip((front + side + back) as usize * 5)?;
    bits.skip(lfe as usize * 4)?;
    bits.skip(assoc as usize * 4)?;
    bits.skip(cc as usize * 5)?;
    bits.align();
    let comment = bits.read(8)?;
    bits.skip(comment as usize * 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_cover_every_channel_once() {
        for config in 1..=7u8 {
            let cfg = Config::new(44_100, config).unwrap();
            let mut seen = vec![0; cfg.channels];
            for s in layout(config) {
                match s {
                    Slot::Sce(p) | Slot::Lfe(p) => seen[*p] += 1,
                    Slot::Cpe(a, b) => {
                        seen[*a] += 1;
                        seen[*b] += 1;
                    }
                }
            }
            assert!(seen.iter().all(|n| *n == 1), "{config}");
        }
    }

    /// MSB から書くビットの書き手（試験の raw_data_block を組む）。
    #[derive(Default)]
    struct W {
        out: Vec<u8>,
        n: usize,
    }

    impl W {
        fn put(&mut self, v: u32, bits: u32) -> &mut Self {
            for k in (0..bits).rev() {
                if self.n.is_multiple_of(8) {
                    self.out.push(0);
                }
                if (v >> k) & 1 == 1 {
                    let last = self.out.len() - 1;
                    self.out[last] |= 0x80 >> (self.n % 8);
                }
                self.n += 1;
            }
            self
        }
    }

    /// モノラルの SCE（長い窓・max_sfb 1・帯 0 は符号表 0）。`zero_section` なら、その前に符号表 1・長さ 0 の区間を置く
    /// （構文では許される。前の symphonia・ffmpeg も読む。見直しの指摘）。
    fn silent_sce(w: &mut W, zero_section: bool) {
        w.put(0, 3).put(0, 4).put(100, 8); // SCE・tag・global_gain
        w.put(0, 1).put(0, 2).put(0, 1).put(1, 6).put(0, 1); // ics_info
        if zero_section {
            w.put(1, 4).put(0, 5);
        }
        w.put(0, 4).put(1, 5); // 符号表 0・長さ 1
        w.put(0, 1).put(0, 1).put(0, 1); // pulse・TNS・gain control なし
    }

    #[test]
    fn zero_length_sections_and_short_fill_counts_are_read() {
        let mut dec = AacDecoder::new(Config::new(44_100, 1).unwrap()).unwrap();
        for zero_section in [false, true] {
            let mut w = W::default();
            silent_sce(&mut w, zero_section);
            w.put(7, 3);
            assert_eq!(dec.decode(&w.out).unwrap(), 1024, "{zero_section}");
            assert!(dec.planes[0][..1024].iter().all(|v| *v == 0.0));
        }
        // FIL の count = 15・esc_count = 0（cnt = 14。esc_count − 1 であふれない。見直しの指摘）
        let mut w = W::default();
        silent_sce(&mut w, false);
        w.put(6, 3).put(15, 4).put(0, 8).put(0, 4);
        for _ in 0..14 * 8 - 4 {
            w.put(0, 1);
        }
        w.put(7, 3);
        assert_eq!(dec.decode(&w.out).unwrap(), 1024);
        // 区間が 64 を超えるもの（長さ 0 の区間を並べる）は壊れたファイル
        let mut w = W::default();
        w.put(0, 3).put(0, 4).put(100, 8);
        w.put(0, 1).put(0, 2).put(0, 1).put(1, 6).put(0, 1);
        for _ in 0..65 {
            w.put(1, 4).put(0, 5);
        }
        w.put(0, 4).put(1, 5).put(0, 3).put(7, 3);
        assert!(matches!(
            dec.decode(&w.out),
            Err(EngineError::Decode { .. })
        ));
        // 配置と合わない要素（モノラルに CPE）は次の段へ
        let mut w = W::default();
        w.put(1, 3).put(0, 4).put(0, 32);
        assert!(matches!(
            dec.decode(&w.out),
            Err(EngineError::Unsupported(_))
        ));
    }
}
