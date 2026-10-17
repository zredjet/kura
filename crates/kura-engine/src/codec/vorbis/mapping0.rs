//! 1 ブロック → 1 パケット（`lib/mapping0.c` の `mapping0_forward`。VBR・SSE でない枝）と、aoTuV のブロックをまたぐ状態。
//!
//! # 移す C：`mapping0_forward`（mapping0.c:738–1322）
//!
//! 1. 準備（738–781）：`blocktype` と `modenumber = W`、`block_mode = blocktype | (modenumber << 1)`（0 impulse・1 padding・
//!    2 transition・3 long。[`block_mode_of`]）、`psy_look = psy[blocktype + (W ? 2 : 0)]`、`partition`、residue の lowpass
//!    （`block_lowpassr[W]` を `normal_partition` の倍数に切り上げ。778–781）。`global_ampmax = vbi->ampmax`（blockout が
//!    減衰させた値）。
//! 2. チャンネルごと（783–902）：`scale = 4.f/n`、`scale_dB = todB(&scale) + .345`（float + double → float）、
//!    **M2 の `poste[i] = _postnoise_detection(pcm, n, block_mode, b->lW_block_mode)`（822。窓を掛ける前）**、
//!    `_vorbis_apply_window`（window.rs）、`mdct_forward`（mdct.rs。gmdct へ）、`drft_forward`（smallft.rs。pcm をその場で）、
//!    `logfft[0] = scale_dB + todB(pcm) + .345`、`logfft[(j+1)>>1] = scale_dB + .5f*todB(&temp) + .345`（866–884 の `#else`。
//!    temp = re² + im²）と `local_ampmax`（0 以下に止める）、`global_ampmax` を更新（チャンネルの順に）。`logfft` は pcm の前半を
//!    使い回す（C と同じ領域でなくてよいが値は同じに）。
//! 3. チャンネルごと（904–1183）：`lastmdct = b->mblock + i*2048`・`tempmdct = b->tblock + i*256`・`lowcomp =
//!    b->lownoise_compand_level + i`（923–926）、`logmdct = todB(mdct) + .345`（936–950 の `#else`）、
//!    **M5 の `*lowcomp = lb_loudnoise_fix(psy_look, *lowcomp, logmdct, block_mode, b->lW_block_mode)`（970–975）**、
//!    `_vp_noisemask(psy_look, *lowcomp, logmdct, lastmdct, enpeak, nepeak, noise, poste[i], block_mode)`（psy.rs）、
//!    `_vp_tonemask(…, global_ampmax, local_ampmax[i])`、`_vp_offset_and_mix(psy_look, noise, tone, 1, 0, logmask, mdct, logmdct,
//!    lastmdct, tempmdct, *lowcomp, nepeak, vif->n, block_mode, vb->nW, b->lW_block_mode, b->lW_no, b->impadnum)`（M1 で gmdct、
//!    M3 で nepeak・tempmdct、最後に lastmdct を書き換える）、floor の型の確認（1088）、`floor_posts[i][PACKETBLOBS/2] =
//!    floor1_fit(…)`。**ビットレートの管理の fit（1095–1181）は移さない**。
//! 4. `vbi->ampmax = global_ampmax`（1184）。
//! 5. パケット（1200–1315。VBR は k = PACKETBLOBS/2 の 1 回だけ）：`0`（1 ビット）、mode（`modebits` ビット）、長い窓なら
//!    lW・nW（各 1 ビット）、チャンネルごとに `floor1_encode`（nonzero と ilogmask）、`_vp_couple_quantize_normalize(k, …,
//!    epeak, npeak, …, sliding_lowpass[W][k], ch, lowpass_residue)`、submap ごとに residue の class と forward（1271–1295。class の
//!    前と forward の前の 2 回、nonzero の束と iwork の束を作り直す）、**aoTuV の窓の状態の更新（1297–1305：impadnum・lW_no・
//!    lW_block_mode。[`AotuvWindowState::next`]）**。
//!
//! - `scale_dB`・`logfft`・`logmdct` の `+ .345` は double の定数（float に double を足して float に入れる）。
//! - `#if 0` の解析の出力、`ANALYSIS` の枝は移さない。
//!
//! # aoTuV のブロックをまたぐ状態（[`AotuvState`]）
//!
//! C の `private_state` の `mblock`（lastmdct。チャンネルごとに 2048 個）・`tblock`（tempmdct。256 個）・
//! `lownoise_compand_level`（チャンネルごと）・`lW_block_mode`・`lW_no`・`impadnum`（codec_internal.h:85–92。block.c:313–317 と
//! `_vds_shared_init` の calloc で 0 から）。並列の書き出しは dsp（`VorbisDspState`）を共有して読むので、この状態は dsp の外に持つ。
//!
//! # 段に分ける（mod.rs の移植の規則 12）
//!
//! `mapping0_forward` を、ブロックをまたぐ値の前後で分けた。直列の `mapping0_forward`（テストの基準）は、これらを C と同じ値になる
//! 順に続けて呼ぶだけ。並列の書き出し（encoder.rs）は段 1・段 3 をブロックごとに並列に、段 0′・段 2 を直列に回す。
//!
//! - 段 0′ [`AotuvWindowState::next`]：aoTuV の窓の状態（block_mode の列だけで決まる。1297–1305）。
//! - 段 1 [`mapping0_analyze`]：上の 2（M2 の poste・窓・MDCT・FFT・logfft・local_ampmax）と、3 のうち `logmdct`（そのチャンネルの
//!   MDCT だけで決まり、M1 で MDCT を書き換える前に作る。C と同じ値）。前のブロックの段 1〜3 に依らない（窓の状態は段 0′）。
//! - 段 2 [`mapping0_ampmax`]：`global_ampmax` を `local_ampmax` でチャンネルの順に更新する（C の 902 行。比較の形のまま）。
//!   [`mapping0_lowcomp`]：M5 の lowcomp を進める（970–975。前の値とこのブロックの logmdct だけ）。[`mapping0_lastmdct`]：
//!   このブロックが読む lastmdct（先頭 n/2 個）を写し、M3 SET lastmdct（psy.c:4461–4501）で進める（このブロックの logmdct・
//!   block_mode・nW だけ。C では `_vp_offset_and_mix` の最後で進めるが、間で lastmdct を読む所はないので同じ値）。
//! - 段 3 [`mapping0_encode`]：3 の残り（noise・tone・offset_and_mix・floor1_fit）と 5（パケット）。`global_ampmax`・lowcomp・
//!   lastmdct の写しは段 2 の値。tempmdct（M3）だけは前のブロックの段 3 に依る（26 kHz 以上の impulse の続き）ので、呼ぶ側が
//!   ブロックの順に渡す（encoder.rs）。
//!
//! # 移さないもの
//!
//! `mapping0_unpack`・`mapping0_inverse`（デコーダ）、`mapping0_pack`（info.rs）、SSE の `mapping_forward_sub0`・`sub1`
//! （234–735）、`mapping0_free_info`、ビットレートの管理の fit と PACKETBLOBS の残りの blob。
//!
//! # 型の対応
//!
//! - `vorbis_block`・`vorbis_block_internal` → `block::VorbisBlock`、`vorbis_dsp_state`・`private_state` → `block::VorbisDspState`・
//!   `block::PrivateState`（読むだけ。統計の欄は持たない）と [`AotuvState`]（aoTuV の欄。書く）。
//! - 作業領域（`_vorbis_block_alloc`・`alloca` の gmdct・epeak・npeak・iwork・floor_posts・noise・tone・poste）は関数の中の `Vec`
//!   （poste は段 1 の結果 [`Analysis::poste`]）。C の `vb->pcm[i]`（logfft・logmdct・logmask に使い回す領域）は [`Analysis::spec`]。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use vorbis_tables::PACKETBLOBS;

use super::bitpack::OggpackBuffer;
#[cfg(test)]
use super::block::VorbisBlock;
use super::block::{PrivateState, VorbisDspState};
use super::floor1::{floor1_encode, floor1_fit};
use super::mdct::mdct_forward;
use super::psy::{
    VorbisLookPsy, lb_loudnoise_fix, m3_mdctbuf_flag, m3_set_lastmdct, postnoise_detection,
    vp_couple_quantize_normalize, vp_noisemask, vp_offset_and_mix, vp_tonemask,
};
use super::res0::{res1_class, res1_forward, res2_class, res2_forward};
use super::scales::todb;
use super::smallft::drft_forward;
use super::window::vorbis_apply_window;
use crate::error::EngineError;

fn internal(what: &str) -> EngineError {
    EngineError::Internal(format!("mapping0：{what}"))
}

/// C の `b->mblock` のチャンネルごとの大きさ（mapping0.c:923 の `2048 is max block size (n/2)`）。
pub(crate) const LASTMDCT_LEN: usize = 2048;
/// C の `b->tblock` のチャンネルごとの大きさ（mapping0.c:924 の `256 is max block size (n/2) for aoTuV M3`）。
pub(crate) const TEMPMDCT_LEN: usize = 256;

/// `block_mode`（mapping0.c:770–772）：`blocktype | (W << 1)`（0 impulse・1 padding・2 transition・3 long）。
pub(crate) fn block_mode_of(blocktype: i32, w: i64) -> i32 {
    blocktype | (i32::from(w != 0) << 1)
}

/// aoTuV の窓の状態（`private_state` の `lW_block_mode`・`lW_no`・`impadnum`。codec_internal.h:90–92）。ブロックの
/// `block_mode` の列だけで決まる（書き出しは段 0 の計画のあとに直列に作る）。初めは 0（`_vds_shared_init` の calloc と
/// block.c:317）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AotuvWindowState {
    /// 前のブロックの block_mode（初めは 0 = impulse）。
    pub lw_block_mode: i32,
    /// 前のブロックと同じ block_mode が続いた数。
    pub lw_no: i32,
    /// impulse → padding のあとの短いブロックの数（M3）。
    pub impadnum: i32,
}

impl AotuvWindowState {
    /// mapping0.c:1297–1305：`block_mode` のブロックを書いたあとの状態。
    pub(crate) fn next(self, block_mode: i32) -> Self {
        let mut s = self;
        // if (present window == (long or trans.) ) impadnum=0 [reset]
        if block_mode >= 2 {
            s.impadnum = 0;
        }
        // if (last window == impulse) && (present window == padding) impadnum=1
        if s.lw_block_mode == 0 && block_mode == 1 {
            s.impadnum = 1;
        } else if s.impadnum != 0 && s.impadnum < 8 {
            s.impadnum += 1;
        }
        // if (current block type == last block type) lW_no++
        if s.lw_block_mode == block_mode {
            s.lw_no = s.lw_no.wrapping_add(1);
        } else {
            s.lw_no = 1;
        }
        s.lw_block_mode = block_mode;
        s
    }
}

/// aoTuV が `private_state` に持つブロックをまたぐ状態（codec_internal.h:85–92。block.c:313–317 で 0 に初期化）。
/// 直列の C の形（`mapping0_forward`）はこれを 1 つ持って進める。並列の書き出し（encoder.rs）は窓の状態を段 0、lowcomp・lastmdct を
/// 段 2、tempmdct を段 3 で進める（値は同じ）。
#[derive(Debug, Clone, Default)]
pub(crate) struct AotuvState {
    /// `lW_block_mode`・`lW_no`・`impadnum`。
    pub window: AotuvWindowState,
    /// `lownoise_compand_level`（チャンネルごと。M5）。
    pub lowcomp: Vec<f32>,
    /// `mblock`（チャンネルごとに [`LASTMDCT_LEN`] 個。M3・M9 の lastmdct）。
    pub lastmdct: Vec<Vec<f32>>,
    /// `tblock`（チャンネルごとに [`TEMPMDCT_LEN`] 個。M3 の tempmdct）。
    pub tempmdct: Vec<Vec<f32>>,
}

impl AotuvState {
    /// `vorbis_analysis_init` の aoTuV の初期化（block.c:313–317。すべて 0）。
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            window: AotuvWindowState::default(),
            lowcomp: vec![0.0; channels],
            lastmdct: vec![vec![0.0; LASTMDCT_LEN]; channels],
            tempmdct: vec![vec![0.0; TEMPMDCT_LEN]; channels],
        }
    }
}

/// 段 1 の結果（1 ブロック・全チャンネル）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Analysis {
    /// チャンネルごとに n 個：前半は logfft、後半は logmdct（C の `vb->pcm[i]` の使い方と同じ。C は段 3 で前半を logmask に使い回すが、
    /// 段 3 は別の領域に書くので、段 1 の結果は書き換えない）。
    pub spec: Vec<Vec<f32>>,
    /// チャンネルごとに n/2 個：MDCT（C の `gmdct`。段 3 は M1 で書き換える写しを作る）。
    pub gmdct: Vec<Vec<f32>>,
    /// チャンネルごと：C の `local_ampmax`（0 以下）。
    pub local_ampmax: Vec<f32>,
    /// チャンネルごと：M2 の `poste`（`_postnoise_detection`。窓を掛ける前の PCM から）。
    pub poste: Vec<f32>,
}

/// `blocksizes[W]` の添字。
fn wi(w: i64) -> usize {
    usize::from(w != 0)
}

/// 段 1（mapping0.c:783–902 と 936–950）：`pcm`（チャンネルごとに `blocksizes[W]` 個。この関数が持ち、logfft・logmdct の
/// 領域に使い回す）から、M2 の poste・窓・MDCT・FFT・logfft・local_ampmax・logmdct。`lw_block_mode` は aoTuV の窓の状態
/// （前のブロックの block_mode。段 0 の計画から決まる）。
pub(crate) fn mapping0_analyze(
    vd: &VorbisDspState,
    lw: i64,
    w: i64,
    nw: i64,
    blocktype: i32,
    lw_block_mode: i32,
    mut pcm: Vec<Vec<f32>>,
) -> Result<Analysis, EngineError> {
    let ci = &vd.vi.codec_setup;
    let b = &vd.backend_state;
    let wix = wi(w);
    let n = usize::try_from(ci.blocksizes[wix])
        .ok()
        .filter(|&n| n >= 4)
        .ok_or_else(|| internal("ブロックの大きさ"))?;
    let channels = usize::try_from(vd.vi.channels).map_err(|_| internal("チャンネルの数"))?;
    if pcm.len() != channels || pcm.iter().any(|p| p.len() != n) {
        return Err(internal("pcm の大きさ"));
    }
    let transform = b
        .transform
        .get(wix)
        .ok_or_else(|| internal("MDCT の look"))?;
    let fft = b.fft_look.get(wix).ok_or_else(|| internal("FFT の look"))?;
    let half = n / 2;
    let block_mode = block_mode_of(blocktype, w);

    let mut gmdct = Vec::with_capacity(channels);
    let mut local_ampmax = Vec::with_capacity(channels);
    let mut poste = Vec::with_capacity(channels);
    for p in &mut pcm {
        // float scale=4.f/n;（int を float にして割る）
        let scale = 4.0_f32 / n as f32;
        // scale_dB=todB(&scale) + .345;
        let scale_db = (f64::from(todb(scale)) + 0.345) as f32;

        // set postnoise flag（822。M2）
        poste.push(postnoise_detection(p, n as i32, block_mode, lw_block_mode));

        // window the PCM data
        vorbis_apply_window(p, &b.window, &ci.blocksizes, lw, w, nw);

        // transform the PCM data（only MDCT right now....）
        let mut mdct = vec![0.0_f32; half];
        mdct_forward(transform, p, &mut mdct);

        // FFT yields more accurate tonal estimation (not phase sensitive)
        drft_forward(fft, p)?;
        // logfft[0]=scale_dB+todB(pcm)  + .345;
        p[0] = (f64::from(scale_db + todb(p[0])) + 0.345) as f32;
        let mut amp = p[0];
        // for(j=1;j<n-1;j+=2)：logfft[(j+1)>>1] は j 以下なので、その場で前から書いてよい（C と同じ）
        let mut j = 1usize;
        while j + 1 < n {
            let temp = p[j] * p[j] + p[j + 1] * p[j + 1];
            let temp = (f64::from(scale_db + 0.5_f32 * todb(temp)) + 0.345) as f32;
            p[(j + 1) >> 1] = temp;
            if temp > amp {
                amp = temp;
            }
            j += 2;
        }
        if amp > 0.0 {
            amp = 0.0;
        }
        local_ampmax.push(amp);

        // logmdct[j]=todB(mdct+j)  + .345;（mapping0.c:936–950。logmdct = logfft + n/2）
        for (dst, &m) in p[half..].iter_mut().zip(&mdct) {
            *dst = (f64::from(todb(m)) + 0.345) as f32;
        }
        gmdct.push(mdct);
    }
    Ok(Analysis {
        spec: pcm,
        gmdct,
        local_ampmax,
        poste,
    })
}

/// 段 2（mapping0.c:756・900–902）：前のブロックから引き継いだ `global_ampmax`（blockout が減衰させた `vbi->ampmax`）を、
/// チャンネルの順に `local_ampmax` で更新する。戻りは C の `vbi->ampmax = global_ampmax`（1184）。
pub(crate) fn mapping0_ampmax(global_ampmax: f32, local_ampmax: &[f32]) -> f32 {
    let mut g = global_ampmax;
    for &l in local_ampmax {
        if l > g {
            g = l;
        }
    }
    g
}

/// `blocktype` と W の psy の look（mapping0.c:764 の `b->psy + blocktype + (vb->W ? 2 : 0)`）。
fn psy_look_of(b: &PrivateState, blocktype: i32, w: i64) -> Result<&VorbisLookPsy, EngineError> {
    usize::try_from(blocktype)
        .ok()
        .map(|t| t + if w != 0 { 2 } else { 0 })
        .and_then(|k| b.psy.get(k))
        .ok_or_else(|| internal("psy の look"))
}

/// 段 2 の M5（mapping0.c:970–975）：チャンネルごとに `lowcomp`（C の `*lowcomp`。前のブロックの値）を
/// `lb_loudnoise_fix` で進める。値はこのブロックの logmdct（段 1）と窓の状態だけで決まる。
pub(crate) fn mapping0_lowcomp(
    vd: &VorbisDspState,
    blocktype: i32,
    w: i64,
    lw_block_mode: i32,
    a: &Analysis,
    lowcomp: &mut [f32],
) -> Result<(), EngineError> {
    let psy_look = psy_look_of(&vd.backend_state, blocktype, w)?;
    let block_mode = block_mode_of(blocktype, w);
    let half = usize::try_from(psy_look.n).map_err(|_| internal("ブロックの大きさ"))?;
    if lowcomp.len() != a.spec.len() {
        return Err(internal("lowcomp のチャンネルの数"));
    }
    for (lc, spec) in lowcomp.iter_mut().zip(&a.spec) {
        let logmdct = spec.get(half..).ok_or_else(|| internal("logmdct"))?;
        *lc = lb_loudnoise_fix(psy_look, *lc, logmdct, block_mode, lw_block_mode);
    }
    Ok(())
}

/// 段 2 の M3・M9 の lastmdct：チャンネルごとに、このブロックが読む lastmdct（前のブロックまでの値の先頭 n/2 個）を写して返し、
/// `lastmdct`（C の `b->mblock`）をこのブロックの logmdct で進める（`_vp_offset_and_mix` の最後の M3 SET lastmdct と同じ。
/// 進めるのは `mdctbuf_flag` が 1（26 kHz 以上。VBR は bit_managed = 0・offset_select = 1）のときだけ）。
pub(crate) fn mapping0_lastmdct(
    vd: &VorbisDspState,
    blocktype: i32,
    w: i64,
    nw: i64,
    a: &Analysis,
    lastmdct: &mut [Vec<f32>],
) -> Result<Vec<Vec<f32>>, EngineError> {
    let psy_look = psy_look_of(&vd.backend_state, blocktype, w)?;
    let block_mode = block_mode_of(blocktype, w);
    let half = usize::try_from(psy_look.n).map_err(|_| internal("ブロックの大きさ"))?;
    if lastmdct.len() != a.spec.len() {
        return Err(internal("lastmdct のチャンネルの数"));
    }
    let flag = m3_mdctbuf_flag(psy_look.rate >= 26000, 0, 1);
    let mut snap = Vec::with_capacity(lastmdct.len());
    for (lm, spec) in lastmdct.iter_mut().zip(&a.spec) {
        snap.push(
            lm.get(..half)
                .ok_or_else(|| internal("lastmdct の大きさ"))?
                .to_vec(),
        );
        if flag == 1 {
            let logmdct = spec.get(half..).ok_or_else(|| internal("logmdct"))?;
            m3_set_lastmdct(lm, logmdct, psy_look.n, block_mode, i32::from(nw != 0));
        }
    }
    Ok(snap)
}

/// 段 3（mapping0.c:904–1183 の logmdct のあとと 1200–1315）：心理モデル・floor・coupling・residue で 1 パケットを `opb` に書く。
/// `a` は段 1 の結果（読むだけ。並列の書き出しはブロックごとに共有の結果から呼ぶ）、`global_ampmax` は段 2 の値、`window` は
/// このブロックの前の aoTuV の窓の状態、`lowcomp` は段 2 で進めたこのブロックの M5 の値。`lastmdct`（チャンネルごと。先頭 n/2 個は
/// 前のブロックまでの値。`_vp_offset_and_mix` が最後にこのブロックの値で書き換える）と `tempmdct`（チャンネルごと。M3 が読み書き
/// する。impulse のブロックだけが触る）は C の `b->mblock`・`b->tblock` に当たる。
pub(crate) fn mapping0_encode(
    vd: &VorbisDspState,
    lw: i64,
    w: i64,
    nw: i64,
    blocktype: i32,
    a: &Analysis,
    global_ampmax: f32,
    window: AotuvWindowState,
    lowcomp: &[f32],
    lastmdct: &mut [Vec<f32>],
    tempmdct: &mut [Vec<f32>],
    opb: &mut OggpackBuffer,
) -> Result<(), EngineError> {
    let vi = &vd.vi;
    let ci = &vi.codec_setup;
    let b = &vd.backend_state;
    let wix = wi(w);
    let channels = usize::try_from(vi.channels).map_err(|_| internal("チャンネルの数"))?;
    let n = usize::try_from(ci.blocksizes[wix]).map_err(|_| internal("ブロックの大きさ"))?;
    let half = n / 2;
    if a.spec.len() != channels
        || a.gmdct.len() != channels
        || a.local_ampmax.len() != channels
        || a.poste.len() != channels
        || a.spec.iter().any(|p| p.len() != n)
        || a.gmdct.iter().any(|p| p.len() != half)
    {
        return Err(internal("段 1 の結果の大きさ"));
    }
    if lowcomp.len() != channels
        || lastmdct.len() != channels
        || tempmdct.len() != channels
        || lastmdct.iter().any(|v| v.len() < half)
    {
        return Err(internal("aoTuV の状態の大きさ"));
    }

    // vorbis_info_floor1 *vif=ci->floor_param[vb->W];
    let vif = ci
        .floor_param
        .get(wix)
        .ok_or_else(|| internal("floor_param"))?;
    // int modenumber=vb->W;（mode の番号は W。パケットには W を modebits ビットで書く）
    // vorbis_info_mapping0 *info=ci->map_param[modenumber];
    let info = ci.map_param.get(wix).ok_or_else(|| internal("map_param"))?;
    // vorbis_look_psy *psy_look=b->psy+blocktype+(vb->W?2:0);
    let psy_look = psy_look_of(b, blocktype, w)?;
    // int partition=(psy_look->vi->normal_p ? psy_look->vi->normal_partition : 16);
    let partition = if psy_look.vi.normal_p != 0 {
        psy_look.vi.normal_partition
    } else {
        16
    };
    let partition = usize::try_from(partition)
        .ok()
        .filter(|&p| p > 0)
        .ok_or_else(|| internal("partition"))?;

    // set modenumber+blocktype（0=Impulse 1=Padding 2=Transition 3=Long）
    let block_mode = block_mode_of(blocktype, w);

    // prepare lowpass filter of residue
    let np = psy_look.vi.normal_partition;
    if np == 0 {
        return Err(internal("normal_partition が 0"));
    }
    let mut lowpass_residue = ci.block_lowpassr[wix];
    if lowpass_residue % np != 0 {
        lowpass_residue = (lowpass_residue / np + 1) * np;
    }

    // gmdct は M1（offset_and_mix）が書き換え、量子化が読む。段 1 の結果は書き換えないので写す
    let mut gmdct: Vec<Vec<f32>> = a.gmdct.clone();
    let mut epeak: Vec<Vec<f32>> = vec![vec![0.0_f32; half]; channels];
    // C は n/2/partition 個（n/2 は partition の倍数）
    let mut npeak: Vec<Vec<f32>> = vec![vec![0.0_f32; half.div_ceil(partition)]; channels];
    let mut iwork: Vec<Vec<i32>> = vec![vec![0_i32; half]; channels];
    let mut floor_posts: Vec<Option<Vec<i32>>> = Vec::with_capacity(channels);

    {
        let mut noise = vec![0.0_f32; half];
        let mut tone = vec![0.0_f32; half];
        // C の logmask（= logfft の領域。tonemask が logfft を読んだあとで offset_and_mix が書く）。別の領域にしても値は同じ
        let mut logmask = vec![0.0_f32; half];

        for i in 0..channels {
            // the encoder setup assumes that all the modes used by any specific bitrate tweaking use the same floor
            let submap = info
                .chmuxlist
                .get(i)
                .and_then(|&s| usize::try_from(s).ok())
                .ok_or_else(|| internal("chmuxlist"))?;

            let (logfft, logmdct) = a.spec[i].split_at(half);
            let mdct = &mut gmdct[i];

            // M5 の lb_loudnoise_fix（970–975）は段 2（mapping0_lowcomp）で済ませた値が lowcomp[i]

            // first step; noise masking.
            vp_noisemask(
                psy_look,
                lowcomp[i],
                logmdct,
                &lastmdct[i],
                &mut epeak[i],
                &mut npeak[i],
                &mut noise,
                a.poste[i],
                block_mode,
            );

            // second step: 'all the other crap'; tone masking, peak limiting and ATH
            vp_tonemask(
                psy_look,
                logfft,
                &mut tone,
                global_ampmax,
                a.local_ampmax[i],
            );

            // third step; we offset the noise vectors, overlay tone masking.
            vp_offset_and_mix(
                psy_look,
                &noise,
                &tone,
                1,
                0, // vorbis_bitrate_managed(vb)：VBR は 0
                &mut logmask,
                mdct,
                logmdct,
                &mut lastmdct[i],
                &mut tempmdct[i],
                lowcomp[i],
                &mut npeak[i],
                vif.n,
                block_mode,
                i32::from(nw != 0),
                window.lw_block_mode,
                window.lw_no,
                window.impadnum,
            );

            // this algorithm is hardwired to floor 1 for now; abort out if we're *not* floor1.
            let fl = info
                .floorsubmap
                .get(submap)
                .and_then(|&f| usize::try_from(f).ok())
                .ok_or_else(|| internal("floorsubmap"))?;
            if ci.floor_type.get(fl) != Some(&1) {
                return Err(internal("floor の種類が 1 でない"));
            }
            let flr = b.flr.get(fl).ok_or_else(|| internal("floor の look"))?;

            // floor_posts[i][PACKETBLOBS/2]=floor1_fit(vb,b->flr[..],logmdct,logmask);
            floor_posts.push(floor1_fit(flr, logmdct, &logmask)?);

            // ビットレートの管理の fit（1095–1181）は移さない
        }
    }

    // 1200–1315：VBR は k = PACKETBLOBS/2 の 1 回だけ
    let k = PACKETBLOBS / 2;

    // start out our new packet blob with packet type and mode
    // Encode the packet type
    opb.oggpack_write(0, 1)?;
    // Encode the modenumber
    // Encode frame mode, pre,post windowsize, then dispatch
    let modebits = u32::try_from(b.modebits).map_err(|_| internal("modebits"))?;
    opb.oggpack_write(u64::from(w != 0), modebits)?;
    if w != 0 {
        opb.oggpack_write(u64::from(lw != 0), 1)?;
        opb.oggpack_write(u64::from(nw != 0), 1)?;
    }

    // encode floor, compute masking curve, sep out residue
    let mut nonzero = vec![0_i32; channels];
    for i in 0..channels {
        let submap = info
            .chmuxlist
            .get(i)
            .and_then(|&s| usize::try_from(s).ok())
            .ok_or_else(|| internal("chmuxlist"))?;
        let fl = info
            .floorsubmap
            .get(submap)
            .and_then(|&f| usize::try_from(f).ok())
            .ok_or_else(|| internal("floorsubmap"))?;
        let flr = b.flr.get(fl).ok_or_else(|| internal("floor の look"))?;
        nonzero[i] = floor1_encode(
            opb,
            &ci.fullbooks,
            flr,
            floor_posts[i].as_deref_mut(),
            &mut iwork[i],
            n as i32,
        )?;
    }

    // our iteration is now based on masking curve, not prequant and coupling.  Only one prequant/coupling step
    // quantize/couple
    let sliding_lowpass = ci
        .psy_g_param
        .sliding_lowpass
        .get(wix)
        .and_then(|v| v.get(k))
        .copied()
        .ok_or_else(|| internal("sliding_lowpass"))?;
    vp_couple_quantize_normalize(
        k as i32,
        &ci.psy_g_param,
        psy_look,
        info,
        &gmdct,
        &epeak,
        &mut npeak,
        &mut iwork,
        &mut nonzero,
        sliding_lowpass,
        vi.channels,
        lowpass_residue,
    )?;

    // classify and encode by submap
    let submaps = usize::try_from(info.submaps).map_err(|_| internal("submaps"))?;
    for s in 0..submaps {
        let resnum = info
            .residuesubmap
            .get(s)
            .and_then(|&r| usize::try_from(r).ok())
            .ok_or_else(|| internal("residuesubmap"))?;
        let look = b
            .residue
            .get(resnum)
            .ok_or_else(|| internal("residue の look"))?;
        let rtype = *ci
            .residue_type
            .get(resnum)
            .ok_or_else(|| internal("residue の種類"))?;

        let members: Vec<usize> = (0..channels)
            .filter(|&j| info.chmuxlist.get(j).copied() == Some(s as i32))
            .collect();
        let zerobundle: Vec<i32> = members
            .iter()
            .map(|&j| i32::from(nonzero[j] != 0))
            .collect();

        match rtype {
            1 => {
                let classifications = {
                    let bundle: Vec<&[i32]> =
                        members.iter().map(|&j| iwork[j].as_slice()).collect();
                    res1_class(look, &bundle, &zerobundle)
                };
                let mut bundle: Vec<&mut [i32]> = iwork
                    .iter_mut()
                    .enumerate()
                    .filter(|(j, _)| info.chmuxlist.get(*j).copied() == Some(s as i32))
                    .map(|(_, v)| v.as_mut_slice())
                    .collect();
                res1_forward(
                    opb,
                    look,
                    &ci.fullbooks,
                    &mut bundle,
                    &zerobundle,
                    classifications.as_deref(),
                )?;
            }
            2 => {
                let bundle: Vec<&[i32]> = members.iter().map(|&j| iwork[j].as_slice()).collect();
                let classifications = res2_class(look, &bundle, &zerobundle);
                res2_forward(
                    opb,
                    look,
                    &ci.fullbooks,
                    &bundle,
                    &zerobundle,
                    classifications.as_deref(),
                    n as i32,
                )?;
            }
            t => return Err(internal(&format!("residue の種類 {t} は扱えない"))),
        }
    }

    // impadnum・lW_no・lW_block_mode の更新（1297–1305）は呼ぶ側（AotuvWindowState::next）
    Ok(())
}

/// `mapping0_forward`（mapping0.c:738–1322）：`vb.pcm` から 1 パケットを `vb.opb` に書き、`vb.ampmax` を更新する
/// （段 1・段 2・段 3 を続けて呼ぶ。C と同じ値）。直列の C の形（テストの基準）。
#[cfg(test)]
pub(crate) fn mapping0_forward(
    vb: &mut VorbisBlock,
    vd: &VorbisDspState,
    st: &mut AotuvState,
) -> Result<(), EngineError> {
    // vb->mode=modenumber;
    vb.mode = i32::from(vb.w != 0);
    let pcm = std::mem::take(&mut vb.pcm);
    let window = st.window;
    let a = mapping0_analyze(
        vd,
        vb.lw,
        vb.w,
        vb.nw,
        vb.blocktype,
        window.lw_block_mode,
        pcm,
    )?;
    let global_ampmax = mapping0_ampmax(vb.ampmax, &a.local_ampmax);
    // vbi->ampmax=global_ampmax;
    vb.ampmax = global_ampmax;
    // *lowcomp=lb_loudnoise_fix(…)（C はチャンネルごとに noisemask の直前。チャンネルの間で読み合わないので先にまとめてよい）
    mapping0_lowcomp(
        vd,
        vb.blocktype,
        vb.w,
        window.lw_block_mode,
        &a,
        &mut st.lowcomp,
    )?;
    let res = mapping0_encode(
        vd,
        vb.lw,
        vb.w,
        vb.nw,
        vb.blocktype,
        &a,
        global_ampmax,
        window,
        &st.lowcomp,
        &mut st.lastmdct,
        &mut st.tempmdct,
        &mut vb.opb,
    );
    st.window = window.next(block_mode_of(vb.blocktype, vb.w));
    // blockout が次のブロックの pcm を書く領域として返す（中身は書き直される）
    vb.pcm = a.spec;
    res
}
