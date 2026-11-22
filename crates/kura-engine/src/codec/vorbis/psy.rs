//! 聴覚の心理モデル（`lib/psy.c`。aoTuV b6.03）：マスクの曲線、aoTuV の M1〜M9、coupling・量子化・noise normalization。
//!
//! # 移す C（SSE でない枝。`#ifdef __SSE__` の側は移さない）
//!
//! | 関数 | 行 | 備考 |
//! |---|---|---|
//! | `_vp_global_look` | 121–131 | `ampmax = -9999.` |
//! | `min_curve`・`max_curve`・`attenuate_curve` | 154–169 | |
//! | `setup_tone_curves` | 171–350 | `vorbis_tables::TONEMASKS`・`ATH`（aoTuV の ATH）。`abs(EHMER_OFFSET-k)` は int、`fromOC`・`toOC` は double（scales.rs）、`floor`・`ceil`、`int lo_bin = fromOC(..)/binHz`（double → int の切り捨て）。各曲線は `EHMER_MAX + 2` 個（先頭の 2 つは fencepost） |
//! | `_vp_psy_init` | 352–576 | SSE でない側は 515 行まで（516–575 は SSE の表）。`shiftoc = rint(log(eighth_octave_lines*8.f)/log(2.f))-1`、`firstoc`・`maxoc` は `toOC(...)*(1<<(shiftoc+1))`（double → long）。aoTuV の `n25p = n/4`・`n33p = n/3`・`n75p = n25p*3`（373–375。M5。`nn25pt`・`nn50pt`・`nn75pt` はどこも読まないので持たない）。rate の段（380–439）：`m_val`（M1）、短い窓の `m3n`（M3。`vorbis_tables::M3N32` ほか、長い窓は 0）と `set_aotuv_psy[select]`（`vorbis_tables::SET_AOTUV_PSY`）の tonecomp_endp・tonecomp_thres（M4・M3・M9）・min_nn_lp（M2・M8）・tonefix_end（M6・M7）。ATH の曲線（443–460）、Bark の窓（462–473。`toBARK(rate/(2*n)*i)` の `rate/(2*n)` は long の割り算）、octave（475–476）、トーンの曲線、ノイズのオフセットと M7 の `ntfix_noiseoffset`（482–507。**`noiseoff[j][inthalfoc+1]`・`ntfix_offset[inthalfoc+1]` は `inthalfoc + 1 < P_BANDS` のときだけ**読む。C は 17 番目（範囲の外）を読むが 0 倍される） |
//! | `_postnoise_detection` | 619–648 | **M2**。前が impulse の過渡（2048 以上）で、窓の前の PCM の 2 つ目の 4 分の 1 と 3 つ目の |x| の和（double）から後エコーの強さ。mapping0 の段 1 が呼ぶ |
//! | `seed_curve` | 652–717（#else 707–716） | `choice = (int)((amp+dBoffset-P_LEVEL_0)*.1f)` を 0〜P_LEVELS−1 に |
//! | `seed_loop` | 719–771（#else 748–760） | |
//! | `seed_chase` | 773–932（#else 851–881・914–927） | 積み上げは n 個 |
//! | `max_seeds` | 936–1085（#else 1053–1084） | `NEGINF = -9999.f` |
//! | `bark_noise_hybridmp` | 3480–3638（SSE でない版） | 重み y² の累積和（float）。`w = y * y * .5`（double を含む）。`lo = b[i] >> 16`、`hi = b[i] & 0xffff`（long）。3 つのループと `fixed > 0` の 3 つのループ。`D` で割る（0 になる入力は C も同じ値を作る：NaN・∞ を入れないことは encoder.rs が保つ） |
//! | `ntfix` | 3645–3768 | **M7**。短い窓（block_mode ≤ 1）：logmdct の山の幅（前に 3・後ろに 4 まで）で work を下げる（`tolerance` 9、n = 256 は 15、`strength` .6）。過渡（2）：work の 8 個ずつの平均（double）の山の前後 3 組を下げる。下げる量は `min(ntfix_noiseoffset, noiseoffset[1] + |noiseoffset[1][0]|)` まで。作業領域 temp・inmod は 256 個の 0 から（C の alloca と memset） |
//! | `_vp_noisemask` | 3770–4074 | `bark_noise_hybridmp(n, bark, logmdct, logmask, 140., -1)`、`work = logmdct - logmask`、2 回目は `0.` と `noisewindowfixed`、`work = logmdct - work`、M7 の `ntfix`（3875）。compander（3878–3889・4006–4012）：`int dB = logmask[i]+.5` を 0〜39、`epeak[i] = work[i] + stn_compand[dB]`（`vorbis_tables::STN_COMPAND`）、`logmask[i] = work[i] + noisecompand[dB]`。**M5**（`noise_compand_level > 0` なら `n33p` まで `noisecompand_high` との間を補間）。npeak を 0 に（4016）。**M2**（4018–4026：poste > 0 なら min_nn_lp までのマスクを下げ npeak を −1）、**M8**（4034–4053：npeak を 0〜1）、**M9**（4060–4073：長い窓・過渡で tonecomp_endp まで、logmdct と lastmdct の差を epeak に。残りは 0） |
//! | `_vp_tonemask` | 4076–4142（#else 4125–4136） | `att = local_specmax + ath_adjatt`、下限 `ath_maxatt`、`logmask = ath + att`、`seed_loop`・`max_seeds` |
//! | `set_m3p` | 4148–4272 | **M3 PRE**。26 kHz 以上の impulse のブロック（n = 128・256）：前も impulse なら tempmdct を 5（256 は 10）下げて引き継ぎ、そうでなければ lastmdct − 5（10）から作り直す。noise_rate・noise_center・tone_rate を lW_no・impadnum から。`freq_bfn128`・`freq_bfn256`（`vorbis_tables`）で tempmdct を広げる（`+= 5./(float)fb` は double） |
//! | `_vp_offset_and_mix` | 4274–4502 | VBR は `offset_select = 1`・`bit_managed = 0`。`hsrate = rate >= 26000`。**M5** の `low_compand`（4310–4312。`*= (toneatt-25.)` は double）、**M3**（`set_m3p` 4314–4317、本体 4339–4400：lastmdct・tempmdct と比べてマスクを下げ、tempmdct を上書きし、npeak を直す）、**M4**（4303–4308・4319–4331・4412–4423）、**M1**（4434–4459：`m1_de = 1.0-((val-m1_coeffi)*0.005*m_val)` は double で計算して float に、`mdct[i] *= m1_de`）、**M3 SET lastmdct**（4461–4501。[`m3_set_lastmdct`]） |
//! | `_vp_ampmax_decay` | 4504–4515 | `secs = (float)n/rate`、`amp += secs*ampmax_att_per_sec`、下限 −9999 |
//! | `flag_lossless` | 4584–4624 | `res[j] = mdct[j]/floor[j]`、3 値の印（1 lossless・0 point・−1 保留）。`point1 -= enpeak[j]`（M9 の epeak） |
//! | `lossless_coupling`・`lossless_couplingf` | 4626–4658 | square polar（int と float） |
//! | `min_indemnity_dipole_hypot` | 4660–4673 | aoTuV の hypot（`thnor = 0.94`。移す） |
//! | `ssort` | 4709–4726 | 部分的な選択ソート（**そのまま写す**。`sort` に置き換えない。mod.rs の規則 8） |
//! | `noise_normalize` | 4732–4854 | `acc = 0` から（4742）。`rint`・`sqrt`・`unitnorm`。`acc += acc*nepeak*nepeak`（M8 の npeak）、`nepeak < -0.5`（M2・M3 の −1）なら noise normalization をしない |
//! | `_vp_couple_quantize_normalize` | 4858–5142 | partition ごと：floor（`vorbis_tables::FLOOR1_FROMDB_LOOKUP`。psy.c:4517 の表は floor1.c の表と同じ値）、`flag_lossless`、チャンネルごとの noise normalization、coupling（**M6** 5007–5047、lossless 5052–5063、point 5064–5111 と aoTuV の hypot）、point の partition の再 normalization、lowpassr より上を 0、nonzero を組で立てる |
//! | `lb_loudnoise_fix` | 5144–5180 | **M5**。48/44.1/32 kHz（m_val ≥ .5）の q3 未満（normal_thresh ≤ .45）だけ。過渡と長いブロックの境（2↔3）で `n25p`〜`n75p` の logmdct の和（double。−130 で下を止める）÷ n から 0〜1（−1 は使わない）。ほかのブロックは前の値をそのまま返す。mapping0 の段 2 が呼ぶ |
//!
//! # 移植で決めたこと
//!
//! - `toOC`・`fromOC`・`toBARK`・`unitnorm`（scales.h）は scales.rs の関数（[`to_oc`]・[`from_oc`]・[`to_bark_long`]・
//!   [`unitnorm`]）を使う。段 7 の移植ではこのファイルの中に同じ式の写しを置いていたが、結合のときに寄せた（照合の look の表・
//!   ブロックの値・ストリーム全体がビットで変わらないことを確かめた）。
//! - C の式の `double` の定数を `float` に入れる所（`p->m_val = .93;`・`mp->noise_rate = 0.3;` など）は [`d2f`] で double から
//!   丸める（10 進から直接 f32 に丸めると、まれに 2 回の丸めの結果と違いうるため）。
//! - C の `max`・`min` マクロ（M2・M3・M7・M8 の `min(min(poste,30.f), …)` など）は [`c_max`]・[`c_min`]（比較の形のまま）。
//!   `if(temp<=0)continue;` のような比較の向きも変えない（NaN の扱いが同じになるように、`continue` の形のまま写す）。
//! - aoTuV の M3 の状態の型：`local_mod3_psy`（psy.h:165–172）は `LocalMod3Psy`（関数の中だけ）、`local_mod4_psy` は
//!   `_vp_offset_and_mix` の中の変数（`lp_pos` は M4 の本体で使わないので持たない）。lastmdct・tempmdct はチャンネルの領域を
//!   引数で受け取る（C の `b->mblock + i*2048`・`b->tblock + i*256`。持ち主は mapping0.rs の `AotuvState`）。
//! - [`m3_set_lastmdct`]（M3 SET lastmdct）と [`m3_mdctbuf_flag`] は、並列の書き出しの段 2 が lastmdct を先に進めるためにも
//!   呼ぶ（`_vp_offset_and_mix` の最後と同じ値）。
//! - 範囲の外の読み書き：ブロックごとの関数は、引数の長さを先に確かめ（短ければ何もしない）、look の値から作る添字は `get`
//!   で読む。C が範囲の外を読むのは未定義の動き（設定のテンプレートでは起きない：`bark_noise_hybridmp` の `fixed` は 15 か 100
//!   で `n` ≥ 64、M2・M8 の partition は `min_nn_lp` を越えても n の中、M3 の `freq_bfn` の広がりは n の中（テスト
//!   `m3_tables_stay_in_range`）、M7 の山の探索は `tonefix_end + 4 ≤ n`）なので、そこで打ち切る。M7 の短い窓の `nxplus > n` の枝
//!   （psy.c:3672–3675）は `spectral[n]`（範囲の外）を読むので、その形では何もしない（テンプレートでは通らない）。
//!   `_vp_couple_quantize_normalize` は `Result` なので、floor の添字（0〜255）・coupling のチャンネル・nepeak の長さが合わなければ
//!   `EngineError::Internal`。`lowpassr > n` のとき C は `n` より先の partition でも回る（出力には効かない）が、移植はそこで止める。
//! - C の `int` のあふれ（未定義）は、照合用（arm64）と同じく 2 の補数で回す（`wrapping_*`）。
//! - [`couple_quantize_normalize_impl`] は coupling の前の量子化の値（ダンプの `RQPR`）を写す口を持つ（照合のテストだけが使う）。
//!
//! # 照合（テスト。`KURA_VORBIS_ORACLE` があるときだけ C と照らす）
//!
//! - (a) `oracle::setup(rate, ch, q, true)` の `look.psy`（ath・octave・bark・noiseoffset・ntfix_noiseoffset・tonecurves と導出の
//!   スカラー：m3n・min_nn_lp・n25p・n33p・n75p ほか）と [`vp_psy_init`] がビットで一致：9 レート × 1・2 チャンネル ×
//!   q −1・0・3・5・10（stereo・uncoupled のすべてのテンプレート、短い窓・長い窓の psy のすべて）と 44.1 kHz の 3・6・8 チャンネル。
//! - (b) ダンプ（レベル 2）をブロックの順に読み、aoTuV の状態（窓の状態・lowcomp・lastmdct・tempmdct）を C と同じ順に進めながら、
//!   各ブロック・各チャンネルで、PCM0 から M2 の poste、LMDC と前の値から LCMP（M5）、LMDC・lastmdct から NOIS・EPEK・NPEK、
//!   LFFT・AMPL・AMPG から TONE、NOIS・TONE・MDCT・LMDC・lastmdct・tempmdct から MASK・MDC1 がビットで一致。(c) MDC1・ILOG・NZ・
//!   EPEK と、M3 で直したあとの npeak（C のダンプの NPEK は M3 の前）から RQPR・RQPO・NZC が一致。入力は `oracle::signal` の 6 種類
//!   （9 レート × 1・2・3・6・8 チャンネル × 5 つの q。M2 に届く `Bursts` を含む）と、途中に NaN が出る照合用の入力（README §8.3）。
//!   M2・M3・M5・M8・M9 が働いた数も数える（M2 は q0 の Bursts と NaN の入力、ほかは 26 kHz 以上のどのレートでも届く）。
//! - 照合用の C がなくても回る性質のテスト（形・範囲・決定性・既知の値・square polar の往復・M2・M3・M5・M7・M8・M9 の振る舞い
//!   など）も置く。
//!
//! # 移さないもの
//!
//! - SSE の枝と表（`findex` など）、`_vp_psy_clear`・`_vi_psy_free` など。
//! - ビットレートの管理だけが通る枝（`_vp_offset_and_mix` の `offset_select` 0・2 と `bit_managed`）は C のとおり写すが、VBR は
//!   通らない。
//!
//! # 型の対応
//!
//! - `vorbis_look_psy`（psy.h:99–152。SSE でない欄。`endeg_start`・`endeg_end`・`nn25pt`・`nn50pt`・`nn75pt` はどこも読まないので
//!   持たない）→ [`VorbisLookPsy`]。`tonecurves`（`float ***`）は `Vec<Vec<Vec<f32>>>`（[P_BANDS][P_LEVELS][EHMER_MAX + 2]）、
//!   `noiseoffset`（`float **`）は `Vec<Vec<f32>>`（[3][n]）、`ntfix_noiseoffset`（`float *`）は `Vec<f32>`（n）、`octave`・`bark`
//!   （`long *`）は `Vec<i64>`、`m3n`（`int[4]`）は `[i32; 4]`、`vi` は `VorbisInfoPsy` の写し。
//! - `vorbis_look_psy_global`（psy.h:90–96）→ [`VorbisLookPsyGlobal`]（`gi` は `CodecSetupInfo::psy_g_param` を引く）。
//! - `vorbis_info_psy`・`vorbis_info_psy_global` は `vorbis_tables` の型。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

// C の関数を 1 対 1 に写すので、添字のループを C の形のまま書く。
#![allow(clippy::needless_range_loop)]

use vorbis_tables::{
    ATH, EHMER_MAX, EHMER_OFFSET, FLOOR1_FROMDB_LOOKUP, FREQ_BFN128, FREQ_BFN256, M3N32, M3N32X2,
    M3N44, M3N44X2, M3N48, M3N48X2, MAX_ATH, NOISE_COMPAND_LEVELS, NTFIX_OFFSET, P_BANDS,
    P_LEVEL_0, P_LEVELS, P_NOISECURVES, SET_AOTUV_PSY, STEREO_THRESHHOLDS, STEREO_THRESHHOLDS_X,
    STN_COMPAND, TONEMASKS, VorbisInfoMapping0, VorbisInfoPsy, VorbisInfoPsyGlobal,
};

#[cfg(test)]
use super::block::VorbisDspState;
use super::info::VorbisInfo;
use super::scales::{from_oc, to_bark_long, to_oc, unitnorm};
use crate::error::EngineError;
use crate::math;

/// psy.c:34。
pub(crate) const NEGINF: f32 = -9999.0;

/// `vorbis_look_psy`（psy.h:99–152）のうち移す機能が使う欄。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct VorbisLookPsy {
    /// ブロックの大きさの半分。
    pub n: i32,
    pub vi: VorbisInfoPsy,
    /// [P_BANDS][P_LEVELS][EHMER_MAX + 2]。
    pub tonecurves: Vec<Vec<Vec<f32>>>,
    /// [P_NOISECURVES][n]。
    pub noiseoffset: Vec<Vec<f32>>,
    /// n 個。
    pub ath: Vec<f32>,
    /// n 個（C の `long *octave`）。
    pub octave: Vec<i64>,
    /// n 個（C の `long *bark`。`((lo-1)<<16) + (hi-1)`）。
    pub bark: Vec<i64>,
    pub firstoc: i64,
    pub shiftoc: i64,
    pub eighth_octave_lines: i32,
    pub total_octave_lines: i32,
    pub rate: i64,
    /// M3 の境（C は `int m3n[4]` で、使うのは先頭の M3C = 3 個。短い窓の 26 kHz 以上だけ 0 でない）。
    pub m3n: [i32; 4],
    /// M1 の係数（26 kHz 未満は 0）。
    pub m_val: f32,
    /// M4（と M3・M9）。
    pub tonecomp_endp: i32,
    /// M4。
    pub tonecomp_thres: f32,
    /// M7 のオフセット（n 個）。
    pub ntfix_noiseoffset: Vec<f32>,
    /// M2（と M8）。
    pub min_nn_lp: i32,
    /// M6・M7。
    pub tonefix_end: i32,
    /// `n/4`（M5）。
    pub n25p: i32,
    /// `n/3`（M5）。
    pub n33p: i32,
    /// `n25p*3`（M5）。
    pub n75p: i32,
}

/// `vorbis_look_psy_global`（psy.h:90–96）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct VorbisLookPsyGlobal {
    pub ampmax: f32,
    pub channels: i32,
}

fn internal(what: &str) -> EngineError {
    EngineError::Internal(format!("Vorbis の心理モデル：{what}"))
}

/// C の `float x = <double の定数>;`（double から f32 へ 1 回だけ丸める）。
#[inline]
fn d2f(x: f64) -> f32 {
    x as f32
}

/// C の `rint`（double の関数）を int に：`(int)rint(x)`。NaN は 0、範囲の外は飽和（照合用の arm64 と同じ）。
#[inline]
fn rint_i32(x: f32) -> i32 {
    f64::from(x).round_ties_even() as i32
}

/// C の `-rint(x)` を int に（double で符号を変えてから変換する）。
#[inline]
fn neg_rint_i32(x: f32) -> i32 {
    (-f64::from(x).round_ties_even()) as i32
}

/// C の `fabs`（double の関数）を float に入れたもの。
#[inline]
fn fabs_f(x: f32) -> f32 {
    f64::from(x).abs() as f32
}

/// C の `max(x,y)` マクロ（os.h：`((x)<(y)?(y):(x))`。比較の形のまま。NaN・±0 の扱いも C と同じ）。
#[inline]
fn c_max(x: f32, y: f32) -> f32 {
    if x < y { y } else { x }
}

/// C の `min(x,y)` マクロ（os.h：`((x)>(y)?(y):(x))`）。
#[inline]
fn c_min(x: f32, y: f32) -> f32 {
    if x > y { y } else { x }
}

/// `partition`（psy.c:3781・4293・4873）：`normal_p ? normal_partition : 16`。
fn partition_of(vi: &VorbisInfoPsy) -> i32 {
    if vi.normal_p != 0 {
        vi.normal_partition
    } else {
        16
    }
}

/// `long` の右シフト（シフトの量が範囲の外なら 0。`_vp_psy_init` が 0〜30 に保つ）。
fn shr_long(x: i64, s: i64) -> i64 {
    u32::try_from(s)
        .ok()
        .and_then(|s| x.checked_shr(s))
        .unwrap_or(0)
}

/// `_vp_global_look`（psy.c:121–131）。
pub(crate) fn vp_global_look(channels: i32) -> VorbisLookPsyGlobal {
    VorbisLookPsyGlobal {
        ampmax: -9999.0,
        channels,
    }
}

/// `min_curve`（psy.c:154–158）。
fn min_curve(c: &mut [f32], c2: &[f32]) {
    for (x, &y) in c.iter_mut().zip(c2).take(EHMER_MAX) {
        if y < *x {
            *x = y;
        }
    }
}

/// `max_curve`（psy.c:159–163）。
fn max_curve(c: &mut [f32], c2: &[f32]) {
    for (x, &y) in c.iter_mut().zip(c2).take(EHMER_MAX) {
        if y > *x {
            *x = y;
        }
    }
}

/// `attenuate_curve`（psy.c:165–169）。
fn attenuate_curve(c: &mut [f32], att: f32) {
    for x in c.iter_mut().take(EHMER_MAX) {
        *x += att;
    }
}

/// `setup_tone_curves` の「曲線を bin に描いて最小を取る」（psy.c:276–293・301–318）。`oc` は曲線の位置の帯域（C の `k` か `i`）。
fn brute_curve(brute_buffer: &mut [f32], curve: &[f32; EHMER_MAX], oc: f64, bin_hz: f64, n: i32) {
    let mut l: i32 = 0;
    for j in 0..EHMER_MAX {
        let mut lo_bin = (from_oc(j as f64 * 0.125 + oc * 0.5 - 2.0625) / bin_hz) as i32;
        let mut hi_bin = (from_oc(j as f64 * 0.125 + oc * 0.5 - 1.9375) / bin_hz + 1.0) as i32;

        if lo_bin < 0 {
            lo_bin = 0;
        }
        if lo_bin > n {
            lo_bin = n;
        }
        if lo_bin < l {
            l = lo_bin;
        }
        if hi_bin < 0 {
            hi_bin = 0;
        }
        if hi_bin > n {
            hi_bin = n;
        }

        while l < hi_bin && l < n {
            if let Some(b) = brute_buffer.get_mut(l as usize)
                && *b > curve[j]
            {
                *b = curve[j];
            }
            l += 1;
        }
    }

    while l < n {
        if let Some(b) = brute_buffer.get_mut(l as usize)
            && *b > curve[EHMER_MAX - 1]
        {
            *b = curve[EHMER_MAX - 1];
        }
        l += 1;
    }
}

/// `setup_tone_curves`（psy.c:171–350）。
fn setup_tone_curves(
    curveatt_db: &[f32; P_BANDS],
    bin_hz: f32,
    n: i32,
    center_boost: f32,
    center_decay_rate: f32,
) -> Vec<Vec<Vec<f32>>> {
    let mut ath = [0.0_f32; EHMER_MAX];
    let mut workc = vec![[[0.0_f32; EHMER_MAX]; P_LEVELS]; P_BANDS];
    let mut athc = [[0.0_f32; EHMER_MAX]; P_LEVELS];
    let mut brute_buffer = vec![0.0_f32; usize::try_from(n).unwrap_or(0)];

    for i in 0..P_BANDS {
        // ATH を足し戻して、低い曲線が −∞ に落ちないようにする（181–200）
        let ath_offset = i * 4;
        for j in 0..EHMER_MAX {
            let mut min = 999.0_f32;
            for k in 0..4 {
                let v = if j + k + ath_offset < MAX_ATH {
                    ATH[j + k + ath_offset]
                } else {
                    ATH[MAX_ATH - 1]
                };
                if min > v {
                    min = v;
                }
            }
            ath[j] = min;
        }

        let w = &mut workc[i];
        // 50 dB の曲線を 30・40 dB に写す（204–207）
        w[2..8].copy_from_slice(&TONEMASKS[i]);
        w[0] = TONEMASKS[i][0];
        w[1] = TONEMASKS[i][0];

        // 中心の持ち上げ・減衰（210–217）
        for j in 0..P_LEVELS {
            for k in 0..EHMER_MAX {
                let dist = (EHMER_OFFSET as i32 - k as i32).abs();
                let mut adj = center_boost + dist as f32 * center_decay_rate;
                if f64::from(adj) < 0.0 && center_boost > 0.0 {
                    adj = 0.0;
                }
                if f64::from(adj) > 0.0 && center_boost < 0.0 {
                    adj = 0.0;
                }
                w[j][k] += adj;
            }
        }

        // 0 dB に正規化し、ATH を重ねた曲線を作る（221–226）
        for j in 0..P_LEVELS {
            let level = if j < 2 { 2.0 } else { j as f64 };
            let att = (f64::from(curveatt_db[i]) + 100.0 - level * 10.0 - P_LEVEL_0) as f32;
            attenuate_curve(&mut w[j], att);
            athc[j] = ath;
            let att_ath = (100.0 - f64::from(j as f32 * 10.0_f32) - P_LEVEL_0) as f32;
            attenuate_curve(&mut athc[j], att_ath);
            max_curve(&mut athc[j], &w[j]);
        }

        // 大きい曲線を抑える（238–241）
        for j in 1..P_LEVELS {
            let prev = athc[j - 1];
            min_curve(&mut athc[j], &prev);
            min_curve(&mut w[j], &athc[j]);
        }
    }

    let bin_hz_d = f64::from(bin_hz);
    let mut ret = vec![vec![vec![0.0_f32; EHMER_MAX + 2]; P_LEVELS]; P_BANDS];
    for i in 0..P_BANDS {
        // どのオクターブの曲線を重ねるか（258–263）
        let bin = (from_oc(i as f64 * 0.5) / bin_hz_d).floor() as i32;
        let mut lo_curve = (to_oc(f64::from(bin as f32 * bin_hz + 1.0)) * 2.0).ceil() as i32;
        let mut hi_curve = (to_oc(f64::from((bin + 1) as f32 * bin_hz)) * 2.0).floor() as i32;
        if lo_curve > i as i32 {
            lo_curve = i as i32;
        }
        if lo_curve < 0 {
            lo_curve = 0;
        }
        if hi_curve >= P_BANDS as i32 {
            hi_curve = P_BANDS as i32 - 1;
        }

        for m in 0..P_LEVELS {
            brute_buffer.fill(999.0);

            // 曲線を bin に描いて、最小を取る（273–295）
            for k in lo_curve..=hi_curve {
                if let Some(wk) = usize::try_from(k).ok().and_then(|k| workc.get(k)) {
                    brute_curve(&mut brute_buffer, &wk[m], f64::from(k), bin_hz_d, n);
                }
            }

            // 次の半オクターブまで同じく（298–320。位置は i、曲線は i+1）
            if i + 1 < P_BANDS {
                brute_curve(&mut brute_buffer, &workc[i + 1][m], i as f64, bin_hz_d, n);
            }

            for j in 0..EHMER_MAX {
                let bin = (from_oc(j as f64 * 0.125 + i as f64 * 0.5 - 2.0) / bin_hz_d) as i32;
                ret[i][m][j + 2] = match usize::try_from(bin) {
                    Ok(b) if bin < n => brute_buffer.get(b).copied().unwrap_or(-999.0),
                    _ => -999.0,
                };
            }

            // fencepost（337–344）
            let mut j = 0;
            while j < EHMER_OFFSET {
                if ret[i][m][j + 2] > -200.0 {
                    break;
                }
                j += 1;
            }
            ret[i][m][0] = j as f32;

            let mut j = EHMER_MAX - 1;
            while j > EHMER_OFFSET + 1 {
                if ret[i][m][j + 2] > -200.0 {
                    break;
                }
                j -= 1;
            }
            ret[i][m][1] = j as f32;
        }
    }

    ret
}

/// `_vp_psy_init`（psy.c:352–576。SSE でない側は 515 まで）。`n` はブロックの大きさの半分。
pub(crate) fn vp_psy_init(
    vi: &VorbisInfoPsy,
    gi: &VorbisInfoPsyGlobal,
    n: i32,
    rate: i64,
) -> Result<VorbisLookPsy, EngineError> {
    let nn = usize::try_from(n)
        .ok()
        .filter(|&x| x >= 2)
        .ok_or_else(|| internal("ブロックの大きさが小さすぎます"))?;
    if !(1..=i64::from(i32::MAX)).contains(&rate) {
        return Err(internal("サンプルレートが範囲の外です"));
    }
    let n64 = i64::from(n);
    let n_f = f64::from(n);
    let rate_f = rate as f64;

    let eighth_octave_lines = gi.eighth_octave_lines;
    // 359
    let shiftoc = ((math::ln(f64::from(eighth_octave_lines as f32 * 8.0_f32))
        / math::ln(f64::from(2.0_f32)))
    .round_ties_even()
        - 1.0) as i64;
    // `1<<(shiftoc+1)`（int）
    let octscale = u32::try_from(shiftoc + 1)
        .ok()
        .filter(|&s| s <= 30)
        .map(|s| 1_i32 << s)
        .ok_or_else(|| internal("eighth_octave_lines が範囲の外です"))?;
    let octscale_f = f64::from(octscale);

    // 361–363
    let firstoc = (to_oc(f64::from(0.25_f32 * rate as f32) * 0.5 / n_f) * octscale_f
        - f64::from(eighth_octave_lines)) as i64;
    let maxoc = (to_oc(f64::from((n as f32 + 0.25_f32) * rate as f32) * 0.5 / n_f) * octscale_f
        + f64::from(0.5_f32)) as i64;
    let total_octave_lines = maxoc.wrapping_sub(firstoc).wrapping_add(1) as i32;
    if total_octave_lines <= 0 {
        return Err(internal("オクターブの線の数が範囲の外です"));
    }

    // AoTuV HF weighting etc.（373–378。nn25pt・nn50pt・nn75pt はどこも読まないので持たない）
    let n25p = n / 4;
    let n33p = n / 3;
    let n75p = n25p.wrapping_mul(3);

    // aoTuV の rate の段（380–439）。m3n は短い窓（128・256）だけ表から、ほかは 0（memset のまま）
    let block_select = |base: usize, m3n_s: &[i32; 3], m3n_x2: &[i32; 3]| match n {
        128 => (Some(base), *m3n_s),
        256 => (Some(base + 1), *m3n_x2),
        1024 => (Some(base + 2), [0; 3]),
        2048 => (Some(base + 3), [0; 3]),
        _ => (None, [0; 3]),
    };
    let (m_val, (select, m3n3)) = if rate < 26000 {
        (0.0, (None, [0; 3]))
    } else if rate < 38000 {
        (d2f(0.93), block_select(0, &M3N32, &M3N32X2))
    } else if rate > 46000 {
        (d2f(1.205), block_select(4, &M3N48, &M3N48X2))
    } else {
        (1.0, block_select(8, &M3N44, &M3N44X2))
    };
    let m3n = [m3n3[0], m3n3[1], m3n3[2], 0];
    let (tonecomp_endp, tonecomp_thres, min_nn_lp, tonefix_end) =
        match select.and_then(|s| SET_AOTUV_PSY.get(s)) {
            Some(a) => (
                a.tonecomp_endp,
                a.tonecomp_thres,
                a.min_nn_lp,
                a.tonefix_end,
            ),
            None => (0, 0.25, 0, 0),
        };

    // ATH（443–460）
    let mut ath = vec![0.0_f32; nn];
    let mut j: i64 = 0;
    for i in 0..MAX_ATH - 1 {
        let endpos =
            (from_oc((i + 1) as f64 * 0.125 - 2.0) * 2.0 * n_f / rate_f).round_ties_even() as i32;
        let endpos = i64::from(endpos);
        let mut base = ATH[i];
        if j < endpos {
            let delta = (ATH[i + 1] - base) / (endpos - j) as f32;
            while j < endpos && j < n64 {
                if let Some(a) = ath.get_mut(j as usize) {
                    *a = (f64::from(base) + 100.0) as f32;
                }
                base += delta;
                j += 1;
            }
        }
    }
    {
        // C は p->ath[j-1]・p->ath[j-2] を読む
        let ju = usize::try_from(j)
            .ok()
            .filter(|&x| x >= 2)
            .ok_or_else(|| internal("ATH の曲線が短すぎます"))?;
        let mut cs = ath[ju - 1];
        let ds = ath[ju - 1] - ath[ju - 2];
        for a in ath.iter_mut().skip(ju) {
            *a = cs;
            cs += ds;
        }
    }

    // Bark の窓（462–473）
    let mut bark = vec![0_i64; nn];
    {
        let mut lo: i64 = -99;
        let mut hi: i64 = 1;
        let step = rate / (2 * n64);
        for i in 0..n64 {
            let b = to_bark_long(step * i) as f32;

            while lo + i64::from(vi.noisewindowlomin) < i
                && to_bark_long(step * lo) < f64::from(b - vi.noisewindowlo)
            {
                lo += 1;
            }

            while hi <= n64
                && (hi < i + i64::from(vi.noisewindowhimin)
                    || to_bark_long(step * hi) < f64::from(b + vi.noisewindowhi))
            {
                hi += 1;
            }

            bark[i as usize] = ((lo - 1) << 16) + (hi - 1);
        }
    }

    // octave（475–476）
    let mut octave = vec![0_i64; nn];
    for (i, o) in octave.iter_mut().enumerate() {
        *o = (to_oc(f64::from(i as f32 + 0.25_f32) * 0.5 * rate_f / n_f) * octscale_f
            + f64::from(0.5_f32)) as i64;
    }

    let tonecurves = setup_tone_curves(
        &vi.toneatt,
        (rate_f * 0.5 / n_f) as f32,
        n,
        vi.tone_centerboost,
        vi.tone_decay,
    );

    // ノイズのオフセットと M7 のオフセット（482–507）
    let mut noiseoffset = vec![vec![0.0_f32; nn]; P_NOISECURVES];
    let mut ntfix_noiseoffset = vec![0.0_f32; nn];
    for i in 0..nn {
        let mut halfoc = (to_oc((i as f64 + 0.5) * rate_f / (2.0 * n_f)) * 2.0) as f32;

        if halfoc < 0.0 {
            halfoc = 0.0;
        }
        if halfoc >= (P_BANDS - 1) as f32 {
            halfoc = (P_BANDS - 1) as f32;
        }
        let inthalfoc = halfoc as i32;
        let del = halfoc - inthalfoc as f32;
        let ih = usize::try_from(inthalfoc).unwrap_or(0);

        for (no, off) in noiseoffset.iter_mut().zip(vi.noiseoff.iter()) {
            let lo_term = f64::from(off.get(ih).copied().unwrap_or(0.0)) * (1.0 - f64::from(del));
            // C は inthalfoc == 16 のとき noiseoff[j][17]（範囲の外）を読んで 0 倍する。照合用の patch と同じく 0. を足す
            let hi_term = match off.get(ih + 1) {
                Some(&v) => f64::from(v * del),
                None => 0.0,
            };
            no[i] = (lo_term + hi_term) as f32;
        }

        // setup ntfix offset（503–505。ntfix_offset は float。[17] は同じく範囲の外なので 0. を足す）
        let lo_term =
            f64::from(NTFIX_OFFSET.get(ih).copied().unwrap_or(0.0)) * (1.0 - f64::from(del));
        let hi_term = match NTFIX_OFFSET.get(ih + 1) {
            Some(&v) => f64::from(v * del),
            None => 0.0,
        };
        ntfix_noiseoffset[i] = (lo_term + hi_term) as f32;
    }

    // ブロックごとの関数が前提にすること：octave は firstoc からの位置が seed の範囲に入る
    let tol = i64::from(total_octave_lines);
    let in_seed = |o: i64| (0..tol).contains(&(o - firstoc));
    if !octave.iter().all(|&o| in_seed(o))
        || octave[0] - firstoc - i64::from(eighth_octave_lines >> 1) < 0
    {
        return Err(internal("オクターブの位置が範囲の外です"));
    }

    Ok(VorbisLookPsy {
        n,
        vi: *vi,
        tonecurves,
        noiseoffset,
        ath,
        octave,
        bark,
        firstoc,
        shiftoc,
        eighth_octave_lines,
        total_octave_lines,
        rate,
        m3n,
        m_val,
        tonecomp_endp,
        tonecomp_thres,
        ntfix_noiseoffset,
        min_nn_lp,
        tonefix_end,
        n25p,
        n33p,
        n75p,
    })
}

/// `_postnoise_detection`（psy.c:619–648。aoTuV M2）：前が impulse（`lw_mode == 0`）の過渡のブロック（`mode == 2`、
/// `nn ≥ 2048`）で、後エコーの強さ。負なら後処理（`_vp_noisemask` の 4019–4026）をしない。`pcm` は窓を掛ける前のブロックの
/// PCM（`nn` 個）。`upt`・`unt` は double。
pub(crate) fn postnoise_detection(pcm: &[f32], nn: i32, mode: i32, lw_mode: i32) -> f32 {
    let sn = nn >> 2;
    let mn = sn + sn;
    let en = sn + (nn >> 1);
    let mut ret: f32 = -1.0;
    let mut upt: f64 = 0.0;
    let mut unt: f64 = 0.0;

    if mode != 2 {
        return ret; // only trans. block
    }
    if lw_mode != 0 {
        return ret;
    }
    if nn < 2048 {
        return ret;
    }

    // nn ≥ 2048 なので 0 < sn < mn < en ≤ nn。pcm が短い（C では範囲の外を読む）なら後処理をしない
    let (Ok(s), Ok(m), Ok(e)) = (
        usize::try_from(sn),
        usize::try_from(mn),
        usize::try_from(en),
    ) else {
        return ret;
    };
    let (Some(up), Some(un)) = (pcm.get(s..m), pcm.get(m..e)) else {
        return ret;
    };
    for &x in up {
        upt += f64::from(x).abs();
    }
    for &x in un {
        unt += f64::from(x).abs();
    }
    if unt / f64::from(sn) > 0.01 {
        return ret;
    }

    upt *= upt;
    unt *= unt;
    unt *= 15.0;

    if upt > unt {
        ret = (upt - unt) as f32;
        if f64::from(ret) < 0.1 {
            ret = -1.0;
        }
    }
    ret
}

/// `seed_curve`（psy.c:652–717。#else 707–716）。`curves` はその帯域の P_LEVELS 本。
fn seed_curve(
    seed: &mut [f32],
    curves: &[Vec<f32>],
    amp: f32,
    oc: i32,
    n: i32,
    linesper: i32,
    db_offset: f32,
) {
    let mut choice = ((f64::from(amp + db_offset) - P_LEVEL_0) * f64::from(0.1_f32)) as i32;
    // os.h の max・min マクロ
    choice = if choice < 0 { 0 } else { choice };
    let top = P_LEVELS as i32 - 1;
    choice = if choice > top { top } else { choice };
    let Some(posts) = usize::try_from(choice).ok().and_then(|c| curves.get(c)) else {
        return;
    };
    let (Some(&post0), Some(&post1)) = (posts.first(), posts.get(1)) else {
        return;
    };
    let curve = posts.get(2..).unwrap_or(&[]);
    let post1 = post1 as i32;
    let mut seedptr = (oc as f32 + (post0 - EHMER_OFFSET as f32) * linesper as f32
        - (linesper >> 1) as f32) as i32;

    let mut i = post0 as i32;
    while i < post1 {
        if seedptr > 0 {
            let c = usize::try_from(i).ok().and_then(|i| curve.get(i)).copied();
            let s = usize::try_from(seedptr).ok().and_then(|p| seed.get_mut(p));
            if let (Some(c), Some(s)) = (c, s) {
                let lin = amp + c;
                if *s < lin {
                    *s = lin;
                }
            }
        }
        seedptr = seedptr.wrapping_add(linesper);
        if seedptr >= n {
            break;
        }
        i += 1;
    }
}

/// `seed_loop`（psy.c:719–771。#else 748–760）。曲線は `p.tonecurves`。
fn seed_loop(p: &VorbisLookPsy, f: &[f32], flr: &[f32], seed: &mut [f32], specmax: f32) {
    let n = usize::try_from(p.n)
        .unwrap_or(0)
        .min(f.len())
        .min(flr.len())
        .min(p.octave.len());
    let db_offset = p.vi.max_curve_db - specmax;

    // 山の値で作業の列を満たす
    let mut i = 0;
    while i < n {
        let mut max = f[i];
        let mut oc = p.octave[i];
        while i + 1 < n && p.octave[i + 1] == oc {
            i += 1;
            if f[i] > max {
                max = f[i];
            }
        }

        if max + 6.0 > flr[i] {
            oc = shr_long(oc, p.shiftoc);

            if oc >= P_BANDS as i64 {
                oc = P_BANDS as i64 - 1;
            }
            if oc < 0 {
                oc = 0;
            }

            if let Some(curves) = usize::try_from(oc).ok().and_then(|o| p.tonecurves.get(o)) {
                seed_curve(
                    seed,
                    curves,
                    max,
                    (p.octave[i] - p.firstoc) as i32,
                    p.total_octave_lines,
                    p.eighth_octave_lines,
                    db_offset,
                );
            }
        }
        i += 1;
    }
}

/// `seed_chase`（psy.c:773–932。#else 851–881・914–927）。
fn seed_chase(seeds: &mut [f32], linesper: i32, n: i64) {
    let nn = usize::try_from(n).unwrap_or(0).min(seeds.len());
    let lp = i64::from(linesper);
    let mut posstack = vec![0_i64; nn];
    let mut ampstack = vec![0.0_f32; nn];
    let mut stack = 0_usize;
    let mut pos = 0_usize;

    for i in 0..nn {
        let ii = i as i64;
        if stack < 2 {
            posstack[stack] = ii;
            ampstack[stack] = seeds[i];
            stack += 1;
        } else {
            loop {
                if seeds[i] < ampstack[stack - 1] {
                    posstack[stack] = ii;
                    ampstack[stack] = seeds[i];
                    stack += 1;
                    break;
                }
                if ii < posstack[stack - 1] + lp
                    && stack > 1
                    && ampstack[stack - 1] <= ampstack[stack - 2]
                    && ii < posstack[stack - 2] + lp
                {
                    // 完全に重なるので stack-1 は要らない。外す
                    stack -= 1;
                    continue;
                }
                posstack[stack] = ii;
                ampstack[stack] = seeds[i];
                stack += 1;
                break;
            }
        }
    }

    // 積み上げに残った位置だけが効く。順に流す
    for i in 0..stack {
        let mut endpos = if i + 1 < stack && ampstack[i + 1] > ampstack[i] {
            posstack[i + 1]
        } else {
            // +1 は大事（短い窓で bin 0 を落とさない）
            posstack[i] + lp + 1
        };
        if endpos > nn as i64 {
            endpos = nn as i64;
        }
        while (pos as i64) < endpos {
            seeds[pos] = ampstack[i];
            pos += 1;
        }
    }
}

/// `max_seeds`（psy.c:936–1085。#else 1053–1084）。
fn max_seeds(p: &VorbisLookPsy, seed: &mut [f32], flr: &mut [f32]) {
    let n = i64::from(p.total_octave_lines);
    let linesper = p.eighth_octave_lines;
    let pn = i64::from(p.n)
        .min(flr.len() as i64)
        .min(p.octave.len() as i64);
    let mut linpos: i64 = 0;

    seed_chase(seed, linesper, n); // マスクのため

    let seed: &[f32] = seed;
    let at = |pos: i64| usize::try_from(pos).ok().and_then(|p| seed.get(p)).copied();
    let oct = |k: i64| p.octave[k as usize];

    let Some(&oct0) = p.octave.first() else {
        return;
    };
    let mut pos = oct0 - p.firstoc - i64::from(linesper >> 1);

    while linpos + 1 < pn {
        let Some(mut min_v) = at(pos) else {
            return;
        };
        let mut end = ((oct(linpos) + oct(linpos + 1)) >> 1) - p.firstoc;
        if min_v > p.vi.tone_abs_limit {
            min_v = p.vi.tone_abs_limit;
        }
        while pos < end {
            pos += 1;
            let Some(s) = at(pos) else {
                return;
            };
            if (s > NEGINF && s < min_v) || min_v == NEGINF {
                min_v = s;
            }
        }

        end = pos + p.firstoc;
        while linpos < pn && oct(linpos) <= end {
            if flr[linpos as usize] < min_v {
                flr[linpos as usize] = min_v;
            }
            linpos += 1;
        }
    }

    let Some(min_v) = at(n - 1) else {
        return;
    };
    while linpos < pn {
        if flr[linpos as usize] < min_v {
            flr[linpos as usize] = min_v;
        }
        linpos += 1;
    }
}

/// `bark_noise_hybridmp`（psy.c:3480–3638。SSE でない版）。
fn bark_noise_hybridmp(n: i32, b: &[i64], f: &[f32], noise: &mut [f32], offset: f32, fixed: i32) {
    let Ok(nn) = usize::try_from(n) else {
        return;
    };
    if nn == 0 || b.len() < nn || f.len() < nn || noise.len() < nn {
        return;
    }

    let mut nv = vec![0.0_f32; nn];
    let mut xv = vec![0.0_f32; nn];
    let mut xxv = vec![0.0_f32; nn];
    let mut yv = vec![0.0_f32; nn];
    let mut xyv = vec![0.0_f32; nn];

    let (mut t_n, mut t_x, mut t_xx, mut t_y, mut t_xy) =
        (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32);

    let mut r: f32;
    let mut a = 0.0_f32;
    let mut bb = 0.0_f32;
    let mut d = 1.0_f32;

    let mut y = f[0] + offset;
    if y < 1.0 {
        y = 1.0;
    }

    let w = (f64::from(y * y) * 0.5) as f32;

    t_n += w;
    t_x += w;
    t_y += w * y;

    nv[0] = t_n;
    xv[0] = t_x;
    xxv[0] = t_xx;
    yv[0] = t_y;
    xyv[0] = t_xy;

    let mut x = 1.0_f32;
    for i in 1..nn {
        let mut y = f[i] + offset;
        if y < 1.0 {
            y = 1.0;
        }

        let w = y * y;

        t_n += w;
        t_x += w * x;
        t_xx += w * x * x;
        t_y += w * y;
        t_xy += w * x * y;

        nv[i] = t_n;
        xv[i] = t_x;
        xxv[i] = t_xx;
        yv[i] = t_y;
        xyv[i] = t_xy;
        x += 1.0;
    }

    let n64 = i64::from(n);
    let mut i = 0_usize;
    let mut x = 0.0_f32;
    while i < nn {
        let lo = i64::from((b[i] >> 16) as i32);
        let hi = i64::from((b[i] & 0xffff) as i32);
        if lo >= 0 || -lo >= n64 {
            break;
        }
        if hi >= n64 {
            break;
        }
        let (hi, ml) = (hi as usize, (-lo) as usize);

        t_n = nv[hi] + nv[ml];
        t_x = xv[hi] - xv[ml];
        t_xx = xxv[hi] + xxv[ml];
        t_y = yv[hi] + yv[ml];
        t_xy = xyv[hi] - xyv[ml];

        a = t_y * t_xx - t_x * t_xy;
        bb = t_n * t_xy - t_x * t_y;
        d = t_n * t_xx - t_x * t_x;
        r = (a + x * bb) / d;
        if r < 0.0 {
            r = 0.0;
        }

        noise[i] = r - offset;
        i += 1;
        x += 1.0;
    }

    while i < nn {
        let lo = i64::from((b[i] >> 16) as i32);
        let hi = i64::from((b[i] & 0xffff) as i32);
        if lo < 0 || lo >= n64 {
            break;
        }
        if hi >= n64 {
            break;
        }
        let (hi, lo) = (hi as usize, lo as usize);

        t_n = nv[hi] - nv[lo];
        t_x = xv[hi] - xv[lo];
        t_xx = xxv[hi] - xxv[lo];
        t_y = yv[hi] - yv[lo];
        t_xy = xyv[hi] - xyv[lo];

        a = t_y * t_xx - t_x * t_xy;
        bb = t_n * t_xy - t_x * t_y;
        d = t_n * t_xx - t_x * t_x;
        r = (a + x * bb) / d;
        if r < 0.0 {
            r = 0.0;
        }

        noise[i] = r - offset;
        i += 1;
        x += 1.0;
    }

    while i < nn {
        r = (a + x * bb) / d;
        if r < 0.0 {
            r = 0.0;
        }

        noise[i] = r - offset;
        i += 1;
        x += 1.0;
    }

    if fixed <= 0 {
        return;
    }

    let half = i64::from(fixed / 2);
    let fixed = i64::from(fixed);
    i = 0;
    x = 0.0;
    while i < nn {
        let hi = i as i64 + half;
        let lo = hi - fixed;
        if hi >= n64 {
            break;
        }
        if lo >= 0 {
            break;
        }
        // C は -lo >= n でも読む（範囲の外。テンプレートでは起きない）
        if -lo >= n64 {
            break;
        }
        let (hi, ml) = (hi as usize, (-lo) as usize);

        t_n = nv[hi] + nv[ml];
        t_x = xv[hi] - xv[ml];
        t_xx = xxv[hi] + xxv[ml];
        t_y = yv[hi] + yv[ml];
        t_xy = xyv[hi] - xyv[ml];

        a = t_y * t_xx - t_x * t_xy;
        bb = t_n * t_xy - t_x * t_y;
        d = t_n * t_xx - t_x * t_x;
        r = (a + x * bb) / d;

        if r - offset < noise[i] {
            noise[i] = r - offset;
        }
        i += 1;
        x += 1.0;
    }
    while i < nn {
        let hi = i as i64 + half;
        let lo = hi - fixed;
        if hi >= n64 {
            break;
        }
        if lo < 0 {
            break;
        }
        let (hi, lo) = (hi as usize, lo as usize);

        t_n = nv[hi] - nv[lo];
        t_x = xv[hi] - xv[lo];
        t_xx = xxv[hi] - xxv[lo];
        t_y = yv[hi] - yv[lo];
        t_xy = xyv[hi] - xyv[lo];

        a = t_y * t_xx - t_x * t_xy;
        bb = t_n * t_xy - t_x * t_y;
        d = t_n * t_xx - t_x * t_x;
        r = (a + x * bb) / d;

        if r - offset < noise[i] {
            noise[i] = r - offset;
        }
        i += 1;
        x += 1.0;
    }
    while i < nn {
        r = (a + x * bb) / d;
        if r - offset < noise[i] {
            noise[i] = r - offset;
        }
        i += 1;
        x += 1.0;
    }
}

/// `ntfix` の作業領域の大きさ（psy.c:3657–3658 の `alloca(256*sizeof(*temp))`）。
const NTFIX_WORK: usize = 256;

/// `ntfix`（psy.c:3645–3768。aoTuV M7）：ノイズの計算でトーンの成分を低く見積もるのを直す（短い窓：山の幅で `noise` を
/// 下げる。過渡のブロック：8 個ずつの平均の山で下げる。長いブロックは何もしない）。`spectral` は logmdct、`noise` は
/// `_vp_noisemask` の work（n 個）。
///
/// C は作業領域 temp・inmod（256 個、0 で埋める）を読み書きする。添字は設定のテンプレートでは範囲の中（短い窓は
/// `tonefix_end + 4 ≤ n`、過渡は `tonefix_end + 16 ≤ n` で `tonefix_end/8 < 256`）。範囲の外を読む形（C では未定義の動き。
/// `nxplus > n` の枝の `spectral[n]` など）になるときは何もしない。
fn ntfix(p: &VorbisLookPsy, spectral: &[f32], noise: &mut [f32], block_mode: i32) {
    let n = p.n;
    let mut nx = p.tonefix_end;
    let Some(&no1_0) = p.noiseoffset.get(1).and_then(|v| v.first()) else {
        return;
    };
    let limit = fabs_f(no1_0);

    if nx == 0 {
        return;
    }
    let (Ok(nu), Some(no1)) = (usize::try_from(n), p.noiseoffset.get(1)) else {
        return;
    };
    // noise と表は n 個（spectral は短い窓の枝だけが読む）
    let len = nu
        .min(noise.len())
        .min(no1.len())
        .min(p.ntfix_noiseoffset.len());

    let mut temp = [0.0_f32; NTFIX_WORK];
    let mut inmod = [0.0_f32; NTFIX_WORK];

    if block_mode <= 1 {
        // short (impulse, padding)
        const FREQ_UPC: i32 = 3;
        const FREQ_UNC: i32 = 4;
        let mut nxplus = nx + FREQ_UNC;

        let mut tolerance = 9.0_f32;
        let strength = 0.6_f32;
        if n == 256 {
            tolerance = 15.0;
        }
        if nxplus > n {
            nx = n;
            nxplus = n - FREQ_UNC;
        }
        // 読む範囲：spectral・inmod は nxplus まで、山の幅は nx + 2 まで、temp・noise は nx まで
        let (Ok(nxu), Ok(nxp)) = (usize::try_from(nx), usize::try_from(nxplus)) else {
            return;
        };
        let len = len.min(spectral.len());
        if nxu + 3 > len || nxp > len || nxu + 3 > NTFIX_WORK || nxp > NTFIX_WORK {
            return;
        }

        for i in 0..nxp {
            if spectral[i] < -70.0 {
                inmod[i] = (-70.0 + f64::from(spectral[i] + 70.0) * 0.1) as f32;
            } else {
                inmod[i] = spectral[i];
            }
        }
        let fu = FREQ_UNC as usize;
        let mut i = fu;
        while i < nxu {
            // nx is alredy small than n
            if spectral[i] > spectral[i - 1] && spectral[i] > spectral[i + 1] {
                // peak detection
                let mut ps = i - 1;
                let mut pe = i + 1;
                let upper = i - FREQ_UPC as usize;
                let under = i + fu;
                // search a range
                let mut j = ps;
                while j > upper {
                    if spectral[j + 1] < spectral[j] {
                        break;
                    }
                    ps = j;
                    j -= 1;
                }
                j = pe;
                while j < under {
                    if spectral[j - 1] < spectral[j] {
                        break;
                    }
                    pe = j;
                    j += 1;
                }
                {
                    let mut ss = inmod[i] - inmod[ps];
                    ss = c_max(ss, inmod[i] - inmod[pe]);
                    if ss > tolerance {
                        if spectral[i] > noise[i] {
                            ss -= tolerance;
                            ss *= strength;
                        }
                        for t in &mut temp[ps..=pe] {
                            *t = c_max(ss, *t);
                            if *t < 0.0 {
                                *t = 0.0;
                            }
                        }
                    }
                }
                i = pe;
            }
            i += 1;
        }
        for i in fu - 1..nxu {
            let test = c_min(p.ntfix_noiseoffset[i], no1[i] + limit); // limit
            if temp[i] > test {
                temp[i] = test;
            }
            noise[i] -= temp[i];
        }
    } else if block_mode == 2 {
        // transition (start/stop)：become average every eight coefficients.
        let Ok(nxu) = usize::try_from(nx) else {
            return;
        };
        let groups = nxu.div_ceil(8);
        // 平均は 8 × groups 個を読み、山の両側（i + 3 の 8 倍まで）を書く。temp は groups + 1 番目（0）まで読む
        if groups * 8 > len || (nxu / 8 + 2) * 8 >= len || groups + 1 > NTFIX_WORK {
            return;
        }
        let mut k = 0usize;
        let mut i = 0usize;
        while i < nxu {
            let mut na: f64 = 0.0;
            for j in 0..8 {
                na += f64::from(noise[i + j]);
            }
            na /= 8.0;
            temp[k] = na as f32;
            i += 8;
            k += 1;
        }
        // nx/=8;
        let nx8 = nxu / 8;
        for i in 3..nx8 {
            if temp[i] > temp[i - 1] && temp[i] > temp[i + 1] {
                // peak detection
                let mut thres;
                let a;
                if temp[i - 1] > temp[i - 2] {
                    thres = temp[i - 2];
                    a = i - 3;
                } else {
                    thres = temp[i - 1];
                    a = i - 2;
                }
                let b = i + 3;
                thres = temp[i] - thres;
                if f64::from(thres) > 2.0 {
                    let eightimes = i * 8;
                    let test = c_min(p.ntfix_noiseoffset[eightimes], no1[eightimes] + limit); // limit
                    thres = c_min(thres - 2.0, test);
                    for v in &mut noise[a * 8..=b * 8] {
                        *v -= thres;
                    }
                }
            }
        }
    }
    // long block は何もしない
}

/// `_vp_noisemask`（psy.c:3770–4074。SSE でない枝）：ノイズのマスク（`logmask`）と、M9 の `epeak`・M2・M8 の `npeak`。
/// `noise_compand_level` は M5 の `lb_loudnoise_fix` の値、`lastmdct` は前のブロックまでの logmdct（M3 の SET lastmdct が
/// 書いたもの。M9 が読む）、`poste` は M2 の `_postnoise_detection` の値、`block_mode` は 0 impulse・1 padding・2 transition・
/// 3 long。
///
/// 長さ：`logmdct`・`logmask`・`epeak` は n 個、`npeak` は n/partition 個（足りなければ何もしない）。`lastmdct` は M9 が
/// `tonecomp_endp` 個まで読む。
pub(crate) fn vp_noisemask(
    p: &VorbisLookPsy,
    noise_compand_level: f32,
    logmdct: &[f32],
    lastmdct: &[f32],
    epeak: &mut [f32],
    npeak: &mut [f32],
    logmask: &mut [f32],
    poste: f32,
    block_mode: i32,
) {
    let Ok(n) = usize::try_from(p.n) else {
        return;
    };
    let partition = partition_of(&p.vi);
    let Ok(part) = usize::try_from(partition) else {
        return;
    };
    let nparts = if part > 0 { n.div_ceil(part) } else { 0 };
    let (Some(logmdct), Some(logmask), Some(epeak), Some(npeak), Some(no1)) = (
        logmdct.get(..n),
        logmask.get_mut(..n),
        epeak.get_mut(..n),
        npeak.get_mut(..nparts),
        p.noiseoffset.get(1).and_then(|v| v.get(..n)),
    ) else {
        return;
    };
    if part == 0 {
        return;
    }
    let mut work = vec![0.0_f32; n];

    bark_noise_hybridmp(p.n, &p.bark, logmdct, logmask, 140.0, -1);

    for i in 0..n {
        work[i] = logmdct[i] - logmask[i];
    }

    bark_noise_hybridmp(p.n, &p.bark, &work, logmask, 0.0, p.vi.noisewindowfixed);

    for i in 0..n {
        work[i] = logmdct[i] - work[i];
    }

    // M7
    ntfix(p, logmdct, &mut work, block_mode);

    // noise compand & aoTuV M5 extension & pre sotre tone peak.
    let db_of = |m: f32| {
        let mut db = (f64::from(m) + 0.5) as i32;
        if db >= NOISE_COMPAND_LEVELS as i32 {
            db = NOISE_COMPAND_LEVELS as i32 - 1;
        }
        if db < 0 {
            db = 0;
        }
        db as usize
    };
    let mut i = 0usize;
    if noise_compand_level > 0.0 {
        let thter = usize::try_from(p.n33p).unwrap_or(0).min(n);
        while i < thter {
            let db = db_of(logmask[i]);
            epeak[i] = work[i] + STN_COMPAND[db] as f32;
            logmask[i] = work[i] + p.vi.noisecompand[db]
                - ((p.vi.noisecompand[db] - p.vi.noisecompand_high[db]) * noise_compand_level);
            i += 1;
        }
    }
    // 4006–4012
    while i < n {
        let db = db_of(logmask[i]);
        epeak[i] = work[i] + STN_COMPAND[db] as f32;
        logmask[i] = work[i] + p.vi.noisecompand[db];
        i += 1;
    }

    // initialization of npeak(nepeak)（4016）
    for v in npeak.iter_mut() {
        *v = 0.0;
    }

    // min_nn_lp は n より小さい（表の値）。partition の端が n を越える（C では範囲の外）なら、そこで打ち切る
    let lp = usize::try_from(p.min_nn_lp).unwrap_or(0);

    // reduction of post-echo. (postprocessing of aoTuV M2)（4019–4026）。C の continue の向き（NaN の扱い）のまま
    if poste > 0.0 {
        let (mut next, mut k) = (0usize, 0usize);
        while next < lp && next + part <= n {
            let i = next;
            let kk = k;
            next += part;
            k += 1;
            let temp = c_min(c_min(poste, 30.0), no1[i] + 30.0); // limit
            if temp <= 0.0 {
                continue;
            }
            npeak[kk] = -1.0; // this time, don't use noise normalization
            for v in &mut logmask[i..i + part] {
                *v -= temp;
            }
        }
    }

    // AoTuV M8 MAIN（4034–4053）：store non peak logmask(floor) for noise normalization.
    // npeak (each partition) = min [0 ~ 1.0f] max
    {
        let nt = 4.0_f32;
        let (mut next, mut k) = (0usize, 0usize);
        while next < lp && next + part <= n {
            let i = next;
            let kk = k;
            next += part;
            k += 1;
            let o = no1[i + part - 1] + 6.0; // threshold of offset
            let mut me = 0.0_f32; // max energy (per partition)
            let mut avge = 0.0_f32; // average energy (per partition)

            if o <= 0.0 {
                continue;
            }
            if f64::from(npeak[kk]) < -0.5 {
                continue;
            }

            for j in 0..part {
                let temp = logmdct[i + j] - logmask[i + j];
                if me < temp {
                    me = temp;
                }
                avge += logmdct[i + j];
            }
            if avge < (-95_i32).wrapping_mul(partition) as f32 {
                continue; // limit (MDCT AVG. /dB)
            }

            if me < nt {
                npeak[kk] = c_min(o, nt - me) / nt;
            }
        }
    }

    // AoTuV M9 MAIN（4060–4073）：store peak impuse for coupling stereo. (to epeak)
    let mut i = 0usize;
    if block_mode > 1 {
        // long and trans. block
        let endp = usize::try_from(p.tonecomp_endp)
            .unwrap_or(0)
            .min(n)
            .min(lastmdct.len());
        while i < endp {
            let temp = logmdct[i] - epeak[i];
            epeak[i] = 0.0;
            if temp >= 12.0 {
                let mi = logmdct[i] - lastmdct[i];
                if mi >= 1.0 {
                    epeak[i] = mi;
                }
            }
            i += 1;
        }
    }
    for v in &mut epeak[i..] {
        *v = 0.0;
    }
}

/// `_vp_tonemask`（psy.c:4076–4142。#else 4125–4136）：トーンのマスクと ATH（`logmask`）。
pub(crate) fn vp_tonemask(
    p: &VorbisLookPsy,
    logfft: &[f32],
    logmask: &mut [f32],
    global_specmax: f32,
    local_specmax: f32,
) {
    let (Ok(n), Ok(tol)) = (usize::try_from(p.n), usize::try_from(p.total_octave_lines)) else {
        return;
    };
    let (Some(logfft), Some(logmask), Some(ath)) =
        (logfft.get(..n), logmask.get_mut(..n), p.ath.get(..n))
    else {
        return;
    };

    let mut seed = vec![NEGINF; tol];
    let mut att = local_specmax + p.vi.ath_adjatt;

    // ATH（大域の最大でなく、このチャンネルの最大から決めた量だけ下に浮かせる）
    if att < p.vi.ath_maxatt {
        att = p.vi.ath_maxatt;
    }

    for i in 0..n {
        logmask[i] = ath[i] + att;
    }

    // トーンのマスク
    seed_loop(p, logfft, logmask, &mut seed, global_specmax);
    max_seeds(p, &mut seed, logmask);
}

/// `local_mod3_psy`（psy.h:165–172。aoTuV M3 の作業の値）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct LocalMod3Psy {
    sw: i32,
    mdctbuf_flag: i32,
    noise_rate: f32,
    noise_rate_low: f32,
    noise_center: f32,
    tone_rate: f32,
}

/// M3 の `mdctbuf_flag`（`set_m3p` の 4157–4170 の前半）：高いレートで、ビットレートの管理をしないか offset_select が 2 なら
/// 1（lastmdct・tempmdct を使う）。段 2 が lastmdct を進めるか決めるのにも使う（[`m3_set_lastmdct`]）。
pub(crate) fn m3_mdctbuf_flag(hs_rate: bool, bit_managed: i32, offset_select: i32) -> i32 {
    i32::from(hs_rate && (bit_managed == 0 || offset_select == 2))
}

/// `set_m3p`（psy.c:4148–4272。@ M3 PRE）：M3 の値を決め、impulse のブロックで tempmdct を作る（前も impulse なら引き継いで
/// 下げ、そうでなければ lastmdct から作り直す）。`n` は 128・256 だけが M3 を使う（`freq_bfn128`・`freq_bfn256`）。
///
/// 長さ：`logmdct`・`lastmdct`・`tempmdct` は n 個以上（C の tempmdct は 256 個）。足りなければ M3 を使わない（C では範囲の外）。
fn set_m3p(
    mp: &mut LocalMod3Psy,
    lw_no: i32,
    impadnum: i32,
    n: i32,
    hs_rate: i32,
    toneatt: f32,
    logmdct: &[f32],
    lastmdct: &[f32],
    tempmdct: &mut [f32],
    block_mode: i32,
    lw_block_mode: i32,
    bit_managed: i32,
    offset_select: i32,
) {
    // lower sampling rate
    if hs_rate == 0 {
        mp.sw = 0;
        mp.mdctbuf_flag = 0;
        return;
    }

    // set flag for lastmdct and tempmdct
    if bit_managed == 0 || offset_select == 2 {
        mp.mdctbuf_flag = 1;
    } else {
        mp.mdctbuf_flag = 0;
        if offset_select == 0 {
            // high noise scene
            mp.sw = 0;
            return;
        }
    }

    // non impulse
    if block_mode != 0 {
        mp.sw = 0;
        return;
    }

    // M3 PRE（n は 128・256 だけ。C の tempmdct・lastmdct・logmdct は n 個以上）
    let fbn: &[i32] = match n {
        128 => &FREQ_BFN128,
        256 => &FREQ_BFN256,
        _ => {
            mp.sw = 0;
            return;
        }
    };
    let nu = fbn.len();
    let (Some(logmdct), Some(lastmdct), Some(tempmdct)) = (
        logmdct.get(..nu),
        lastmdct.get(..nu),
        tempmdct.get_mut(..nu),
    ) else {
        mp.sw = 0;
        return;
    };

    // tempmdct を下げる量（128 は 5、256 は 10。tempmdct[i] -= 5 は float、+= 5./(float)… は double）
    let pre: i32;
    if n == 128 {
        pre = 5;
        let count = if toneatt < 3.0 { 2 } else { 3 }; // q6~ は 2

        if lw_block_mode == 0 {
            // last window "short" - type "impulse"
            if lw_no < 8 {
                // impulse - @impulse case1
                mp.noise_rate = (0.7 - f64::from((lw_no - 1) as f32 / 17.0_f32)) as f32;
                mp.noise_center = lw_no.wrapping_mul(count) as f32;
                mp.tone_rate = (8 - lw_no) as f32;
            } else {
                // impulse - @impulse case2
                mp.noise_rate = d2f(0.3);
                mp.noise_center = 25.0;
                mp.tone_rate = 0.0;
                if lw_no.wrapping_mul(count) < 24 {
                    mp.noise_center = lw_no.wrapping_mul(count) as f32;
                }
            }
            if mp.mdctbuf_flag == 1 {
                for t in tempmdct.iter_mut() {
                    *t -= 5.0;
                }
            }
        } else {
            // non_impulse - @Short(impulse) case
            mp.noise_rate = d2f(0.7);
            mp.noise_center = 0.0;
            mp.tone_rate = 8.0;
            if mp.mdctbuf_flag == 1 {
                for (t, &l) in tempmdct.iter_mut().zip(lastmdct) {
                    *t = l - 5.0;
                }
            }
        }
        mp.noise_rate_low = 0.0;
        mp.sw = 1;
        if impadnum != 0 {
            mp.noise_rate = (f64::from(mp.noise_rate) * (f64::from(impadnum) * 0.125)) as f32;
        }
    } else {
        // for q-1/-2 44.1kHz/48kHz
        pre = 10;
        if lw_block_mode == 0 {
            // last window "short" - type "impulse"
            let count = 6;
            if lw_no < 4 {
                // impulse - @impulse case1
                mp.noise_rate = (0.4 - f64::from((lw_no - 1) as f32 / 11.0_f32)) as f32;
                mp.noise_center = lw_no.wrapping_mul(count).wrapping_add(12) as f32;
                mp.tone_rate = 8_i32.wrapping_sub(lw_no.wrapping_mul(2)) as f32;
            } else {
                // impulse - @impulse case2
                mp.noise_rate = d2f(0.2);
                mp.noise_center = 30.0;
                mp.tone_rate = 0.0;
            }
            if mp.mdctbuf_flag == 1 {
                for t in tempmdct.iter_mut() {
                    *t -= 10.0;
                }
            }
        } else {
            // non_impulse - @Short(impulse) case
            mp.noise_rate = d2f(0.6);
            mp.noise_center = 12.0;
            mp.tone_rate = 8.0;
            if mp.mdctbuf_flag == 1 {
                for (t, &l) in tempmdct.iter_mut().zip(lastmdct) {
                    *t = l - 10.0;
                }
            }
        }
        mp.noise_rate_low = 0.0;
        mp.sw = 1;
        if impadnum != 0 {
            mp.noise_rate = (f64::from(mp.noise_rate) * (f64::from(impadnum) * 0.0625)) as f32;
        }
    }
    let up = f64::from(pre); // 5. または 10.（double）
    for i in 0..nu {
        let fb = fbn[i];
        let cell = 75.0_f32 / fb as f32;
        for j in 1..fb {
            let ju = j as usize; // 1 ≤ j < fb
            let freqbuf = logmdct[i] - (cell * j as f32);
            // i + fb ≤ n（表の形。テスト `m3_tables_stay_in_range`）
            let (Some(&t), Some(&fbj)) = (tempmdct.get(i + ju), fbn.get(i + ju)) else {
                break;
            };
            if t < freqbuf && mp.mdctbuf_flag == 1 {
                tempmdct[i + ju] = (f64::from(t) + up / f64::from(fbj as f32)) as f32;
            }
        }
    }

    // higher noise curve (lower rate) && managed mode
    if bit_managed != 0 && offset_select == 0 && mp.sw != 0 {
        mp.noise_rate = (f64::from(mp.noise_rate) * 0.2) as f32;
    }
}

/// M3 SET lastmdct（psy.c:4461–4501。`_vp_offset_and_mix` の最後）：次のブロックの M3・M9 が読む lastmdct を、このブロックの
/// logmdct（n 個）から作る。短い窓（block_mode 0・1）は次が長ければ 8 倍に引き伸ばし（8n 個）、短ければそのまま。過渡（2）は
/// 次が短ければ 8 個ずつの最小（n/8 個）、長ければそのまま。長い窓（3）はそのまま。書くのはこの先頭の部分だけで、残りは前の
/// 値のまま（C の `mblock` は 2048 個）。`lastmdct` が短い（C では範囲の外）なら書かない。
pub(crate) fn m3_set_lastmdct(
    lastmdct: &mut [f32],
    logmdct: &[f32],
    n: i32,
    block_mode: i32,
    nw_modenumber: i32,
) {
    const MAG: usize = 8;
    let Ok(n) = usize::try_from(n) else {
        return;
    };
    let Some(logmdct) = logmdct.get(..n) else {
        return;
    };
    match block_mode {
        0 | 1 => {
            // short block (n==128 or 256 only)
            if nw_modenumber != 0 {
                // next long (trans.) block
                let Some(dst) = lastmdct.get_mut(..n * MAG) else {
                    return;
                };
                for (d, &v) in dst.as_chunks_mut::<MAG>().0.iter_mut().zip(logmdct) {
                    d.fill(v);
                }
            } else if let Some(dst) = lastmdct.get_mut(..n) {
                dst.copy_from_slice(logmdct); // next short block
            }
        }
        2 => {
            // trans. block
            if nw_modenumber == 0 {
                // next short block
                let nsh = n >> 3; // 1/8
                let Some(dst) = lastmdct.get_mut(..nsh) else {
                    return;
                };
                for (i, d) in dst.iter_mut().enumerate() {
                    let ni = i * MAG;
                    *d = logmdct[ni];
                    for j in 1..MAG {
                        if *d > logmdct[ni + j] {
                            *d = logmdct[ni + j];
                        }
                    }
                }
            } else if let Some(dst) = lastmdct.get_mut(..n) {
                dst.copy_from_slice(logmdct); // next long block
            }
        }
        3 => {
            // long block (n==1024 or 2048)
            if let Some(dst) = lastmdct.get_mut(..n) {
                dst.copy_from_slice(logmdct);
            }
        }
        _ => {}
    }
}

/// `_vp_offset_and_mix`（psy.c:4274–4502）：noise・tone にオフセットを足して重ね（M3・M4）、M1 で MDCT を弱め、M3 の
/// lastmdct を次のブロックのために書く。`mdct` は M1 で、`npeak` は M3 で、`tempmdct` は M3 で、`lastmdct` は最後に書き換える。
/// `low_compand` は M5 の `lb_loudnoise_fix` の値、`nw_modenumber` は次の窓（nW）、`lw_block_mode`・`lw_no`・`impadnum` は
/// mapping0 の aoTuV の窓の状態（前のブロックまで）。
pub(crate) fn vp_offset_and_mix(
    p: &VorbisLookPsy,
    noise: &[f32],
    tone: &[f32],
    offset_select: i32,
    bit_managed: i32,
    logmask: &mut [f32],
    mdct: &mut [f32],
    logmdct: &[f32],
    lastmdct: &mut [f32],
    tempmdct: &mut [f32],
    low_compand: f32,
    npeak: &mut [f32],
    end_block: i32,
    block_mode: i32,
    nw_modenumber: i32,
    lw_block_mode: i32,
    lw_no: i32,
    impadnum: i32,
) {
    let Ok(n) = usize::try_from(p.n) else {
        return;
    };
    let Ok(sel) = usize::try_from(offset_select) else {
        return;
    };
    let partition = partition_of(&p.vi);
    let Some(part) = usize::try_from(partition).ok().filter(|&x| x > 0) else {
        return;
    };
    let (Some(noise), Some(tone), Some(logmask), Some(mdct), Some(logmdct)) = (
        noise.get(..n),
        tone.get(..n),
        logmask.get_mut(..n),
        mdct.get_mut(..n),
        logmdct.get(..n),
    ) else {
        return;
    };
    let (Some(noiseoffset), Some(&toneatt)) = (
        p.noiseoffset.get(sel).and_then(|v| v.get(..n)),
        p.vi.tone_masteratt.get(sel),
    ) else {
        return;
    };

    let hsrate = i32::from(p.rate >= 26000); // high sampling rate is 1

    // Init for aoTuV M3
    let mut mp3 = LocalMod3Psy::default();

    // Init for aoTuV M4（lp_pos は M4 の本体で使わない）
    let mut mp4_start = p.vi.normal_start;
    let mut mp4_end = p.tonecomp_endp;
    let mp4_thres = p.tonecomp_thres;
    let mut mp4_end_block = end_block;

    // Collapse of low(mid) frequency is prevented. (for 32/44/48kHz q-2)
    let mut low_compand = low_compand;
    if low_compand < 0.0 || f64::from(toneatt) < 25.0 {
        low_compand = 0.0;
    } else {
        low_compand = (f64::from(low_compand) * (f64::from(toneatt) - 25.0)) as f32;
    }

    // @ M3 PRE
    set_m3p(
        &mut mp3,
        lw_no,
        impadnum,
        p.n,
        hsrate,
        toneatt,
        logmdct,
        lastmdct,
        tempmdct,
        block_mode,
        lw_block_mode,
        bit_managed,
        offset_select,
    );
    // M3 の本体が読む範囲（set_m3p が sw を立てたときは n ≤ 長さ）
    if mp3.sw != 0 && (lastmdct.len() < n || tempmdct.len() < n || npeak.len() < n.div_ceil(part)) {
        mp3.sw = 0;
    }

    // @ M4 PRE
    mp4_end_block = mp4_end_block.wrapping_add(p.vi.normal_partition);
    if mp4_end_block > p.n {
        mp4_end_block = p.n;
    }
    if hsrate == 0 {
        mp4_end = mp4_end_block; // for M4
    } else if p.vi.normal_thresh > 1.0 {
        mp4_start = 9999;
    }

    for i in 0..n {
        let ii = i as i32;
        let mut val = noise[i] + noiseoffset[i];
        let mut tval = tone[i] + toneatt;
        if ii <= mp4_start {
            tval -= low_compand;
        }
        if val > p.vi.noisemaxsupp {
            val = p.vi.noisemaxsupp;
        }

        // AoTuV M3 MAIN（4345–4400）：Dynamic impulse block noise control. 48/44.1/32kHz only.
        if mp3.sw != 0
            && val > tval
            && val > lastmdct[i]
            && logmdct[i] > (tempmdct[i] + mp3.noise_center)
        {
            let mut toneac = 0;
            let mut rate_mod;
            let mainth;

            if mp3.mdctbuf_flag == 1 {
                tempmdct[i] = logmdct[i]; // overwrite
            }
            if logmdct[i] > lastmdct[i] {
                rate_mod = mp3.noise_rate;
            } else {
                rate_mod = mp3.noise_rate_low;
            }
            // edit tone masking
            if impadnum == 0 && ii < p.tonecomp_endp && (val - lastmdct[i]) > 20.0 {
                let dbsub = logmdct[i] - lastmdct[i];
                if dbsub > 25.0 {
                    toneac = 1;
                    if tval > -100.0 && (logmdct[i] - tval) < 48.0 {
                        // limit
                        let mut tr_cur = mp3.tone_rate;
                        if dbsub < 35.0 {
                            tr_cur *= (35.0 - dbsub) * 0.1;
                        }
                        tval -= tr_cur;
                        if tval < -100.0 {
                            tval = -100.0; // lower limit = -100
                        }
                        if (logmdct[i] - tval) > 48.0 {
                            tval = logmdct[i] - 48.0; // limit
                        }
                    }
                }
            }
            // main work
            if ii > p.m3n[0] {
                mainth = 30.0_f32;
            } else if ii > p.m3n[1] {
                mainth = 20.0;
            } else if ii > p.m3n[2] {
                mainth = 10.0;
                rate_mod *= 0.5;
            } else {
                mainth = 10.0;
                rate_mod *= 0.3;
            }
            let valmask = if (val - tval) > mainth {
                ((val - tval - mainth) * 0.1 + mainth) * rate_mod
            } else {
                (val - tval) * rate_mod
            };

            if (val - valmask) > lastmdct[i] {
                val -= valmask;
            } else {
                val = lastmdct[i];
            }

            if toneac != 0 {
                let temp = val - c_max(lastmdct[i], -140.0); // limit
                if temp > 20.0 {
                    val -= (temp - 20.0) * 0.2;
                }
            }

            // exception of npeak(nepeak) for M8
            let k = i / part;
            if toneac == 1 {
                npeak[k] = -1.0;
            } else if npeak[k] > 0.0 {
                npeak[k] = 0.0;
            }
        }

        // AoTuV M4 MAIN（4412–4423）
        if val > tval {
            logmask[i] = val;
        } else if ii > mp4_start && ii < mp4_end {
            if logmdct[i] < tval {
                if logmdct[i] < val {
                    tval -= (tval - val) * mp4_thres;
                } else {
                    tval = logmdct[i];
                }
            }
            logmask[i] = tval;
        } else {
            logmask[i] = tval;
        }

        // AoTuV M1（4434–4459）
        if offset_select == 1 {
            let m1_coeffi = d2f(-17.2); // coeffi is a -17.2dB threshold
            val -= logmdct[i]; // val == mdct line value relative to floor in dB

            let m1_de = if val > m1_coeffi {
                // mdct value is > -17.2 dB below floor
                let de = (1.0 - f64::from(val - m1_coeffi) * 0.005 * f64::from(p.m_val)) as f32;
                if de < 0.0 { d2f(0.0001) } else { de }
            } else {
                // mdct value is <= -17.2 dB below floor
                (1.0 - f64::from(val - m1_coeffi) * 0.0003 * f64::from(p.m_val)) as f32
            };

            mdct[i] *= m1_de;
        }
    }

    // @ M3 SET lastmdct
    if mp3.mdctbuf_flag == 1 {
        m3_set_lastmdct(lastmdct, logmdct, p.n, block_mode, nw_modenumber);
    }
}

/// `lb_loudnoise_fix`（psy.c:5144–5180。aoTuV M5）：過渡と長いブロックの境（2→3・3→2）で、高い帯域の大きさから低い帯域の
/// noise compand の強さ（0 ふつう〜1.0 高い。負は使わない）を決める。ほかのブロックは前の値をそのまま返す。48/44.1/32 kHz の
/// q3 未満だけ（ほかは −1）。`hi_th` は double。
pub(crate) fn lb_loudnoise_fix(
    p: &VorbisLookPsy,
    noise_compand_level: f32,
    logmdct: &[f32],
    block_mode: i32,
    lw_block_mode: i32,
) -> f32 {
    let n = p.n;
    let mut hi_th: f64 = 0.0;

    if f64::from(p.m_val) < 0.5 {
        return -1.0; // 48/44.1/32kHz only
    }
    if p.vi.normal_thresh > 0.45 {
        return -1.0; // under q3
    }

    // There is a specific limit for a threshold calculation.
    if !((block_mode == 2 && lw_block_mode == 3) || (block_mode == 3 && lw_block_mode == 2)) {
        return noise_compand_level;
    }

    // calculation of a threshold.（nq1 = n25p、nq3 = n75p。n より小さい）
    let (Ok(nq1), Ok(nq3)) = (usize::try_from(p.n25p), usize::try_from(p.n75p)) else {
        return noise_compand_level;
    };
    let Some(band) = logmdct.get(nq1..nq3.max(nq1)) else {
        return noise_compand_level;
    };
    for &v in band {
        if v > -130.0 {
            hi_th += f64::from(v);
        } else {
            hi_th += -130.0;
        }
    }
    hi_th /= f64::from(n);

    // calculation of a high_compand_level
    if hi_th > -40.0 {
        -1.0
    } else if hi_th < -50.0 {
        1.0
    } else {
        (1.0 - ((hi_th + 50.0) / 10.0)) as f32
    }
}

/// `_vp_ampmax_decay`（psy.c:4504–4515）。窓は `vd->W`（C の形。テストの基準。書き出しは [`vp_ampmax_decay_w`] を使う）。
#[cfg(test)]
pub(crate) fn vp_ampmax_decay(amp: f32, vd: &VorbisDspState) -> f32 {
    vp_ampmax_decay_w(amp, &vd.vi, vd.w)
}

/// `_vp_ampmax_decay`（psy.c:4504–4515）の窓を引数で渡す形（blockout の計画を先に進めてから ampmax を決めるため。値は同じ）。
pub(crate) fn vp_ampmax_decay_w(amp: f32, vi: &VorbisInfo, w: i64) -> f32 {
    let ci = &vi.codec_setup;
    let gi = &ci.psy_g_param;

    let blocksize = usize::try_from(w)
        .ok()
        .and_then(|w| ci.blocksizes.get(w))
        .copied()
        .unwrap_or(0);
    let n = (blocksize / 2) as i32;
    let secs = n as f32 / vi.rate as f32;

    let mut amp = amp + secs * gi.ampmax_att_per_sec;
    if amp < -9999.0 {
        amp = -9999.0;
    }
    amp
}

/// `flag_lossless`（psy.c:4584–4624）。
fn flag_lossless(
    limit: i32,
    prepoint: f32,
    postpoint: f32,
    prepoint_r: f32,
    postpoint_r: f32,
    res: &mut [f32],
    mdct: &[f32],
    enpeak: &[f32],
    floor: &[f32],
    flag: &mut [i32],
    i: i32,
    jn: i32,
) {
    let pointlimit = limit.wrapping_sub(i);
    let mut point1;
    let mut point2;
    let mut ps1 = 0.0_f32;
    let mut ps2 = 0.0_f32;
    let mut ps = false;

    if pointlimit > 0 {
        point1 = prepoint;
        point2 = prepoint_r;
        if pointlimit.wrapping_sub(jn) <= 0 {
            // pointlimit は 1〜32
            ps1 = (postpoint - prepoint) / jn as f32;
            ps2 = (postpoint_r - prepoint_r) / jn as f32;
            ps = true;
        }
    } else {
        point1 = postpoint;
        point2 = postpoint_r;
    }

    let n = usize::try_from(jn)
        .unwrap_or(0)
        .min(res.len())
        .min(mdct.len())
        .min(enpeak.len())
        .min(floor.len())
        .min(flag.len());
    for j in 0..n {
        if ps {
            point1 += ps1;
            point2 += ps2;
        }
        let bakp1 = point1;

        res[j] = mdct[j] / floor[j];
        let r = fabs_f(res[j]);
        point1 -= enpeak[j]; // enpeak を映す
        if point1 < prepoint {
            point1 = prepoint;
        }
        flag[j] = if r < point1 {
            if r < point2 { 0 } else { -1 }
        } else {
            1
        };
        point1 = bakp1;
    }
}

/// `lossless_coupling`（psy.c:4626–4641）。
fn lossless_coupling(mag: &mut i32, ang: &mut i32) {
    let a = *mag;
    let b = *ang;

    if a.wrapping_abs() > b.wrapping_abs() {
        *ang = if a > 0 {
            a.wrapping_sub(b)
        } else {
            b.wrapping_sub(a)
        };
    } else {
        *ang = if b > 0 {
            a.wrapping_sub(b)
        } else {
            b.wrapping_sub(a)
        };
        *mag = b;
    }

    // 同じ値になる 2 つの組を 1 つにまとめる
    if *ang >= mag.wrapping_abs().wrapping_mul(2) {
        *ang = ang.wrapping_neg();
        *mag = mag.wrapping_neg();
    }
}

/// `lossless_couplingf`（psy.c:4643–4658）。
fn lossless_couplingf(mag: &mut f32, ang: &mut f32) {
    let a = *mag;
    let b = *ang;

    if f64::from(a).abs() > f64::from(b).abs() {
        *ang = if a > 0.0 { a - b } else { b - a };
    } else {
        *ang = if b > 0.0 { a - b } else { b - a };
        *mag = b;
    }

    // 同じ値になる 2 つの組を 1 つにまとめる（`fabs(*Mag)*2` は double）
    if f64::from(*ang) >= f64::from(*mag).abs() * 2.0 {
        *ang = -*ang;
        *mag = -*mag;
    }
}

/// `min_indemnity_dipole_hypot`（psy.c:4660–4673）：aoTuV の point stereo の大きさ。
fn min_indemnity_dipole_hypot(a: f32, b: f32, threv: f32) -> f32 {
    let thnor = d2f(0.94);
    let a2 = fabs_f(a * thnor);
    let b2 = fabs_f(b * thnor);

    if f64::from(a) > 0.0 {
        if f64::from(b) > 0.0 {
            return a2 + b2;
        }
        if a > -b {
            return a2 - b2 * threv;
        }
        return -(b2 - a2 * threv);
    }
    if f64::from(b) < 0.0 {
        return -(a2 + b2);
    }
    if -a > b {
        return -(a2 - b2 * threv);
    }
    b2 - a2 * threv
}

/// `ssort`（psy.c:4709–4726）：`sort`（`q` の添字の並び）の先頭 `bthresh` 個を、`q` の大きい順に選んで入れ替える。
fn ssort(range: i32, bthresh: i32, sort: &mut [usize], q: &[f32]) {
    let mut bthresh = bthresh;
    if range < bthresh {
        bthresh = range;
    }
    let range = usize::try_from(range).unwrap_or(0).min(sort.len());
    let bthresh = usize::try_from(bthresh).unwrap_or(0).min(range);
    let at = |k: usize| q.get(k).copied().unwrap_or(f32::NAN);
    for i in 0..bthresh {
        let mut large = i;
        for j in i + 1..range {
            // 大きい順
            if at(sort[large]) < at(sort[j]) {
                large = j;
            }
        }
        sort.swap(i, large);
    }
}

/// `noise_normalize`（psy.c:4732–4854）。`flags` は C の NULL（チャンネルごと）なら `None`。戻り値は acc（C は渡された acc を
/// 4742 で 0 にするので `_acc` は読まない）。
fn noise_normalize(
    p: &VorbisLookPsy,
    limit: i32,
    r: &[f32],
    q: &mut [f32],
    f: &[f32],
    res: &mut [f32],
    flags: Option<&[i32]>,
    _acc: f32,
    nepeak: f32,
    i: i32,
    n: i32,
    out: &mut [i32],
) -> f32 {
    let vi = &p.vi;
    let mut len = usize::try_from(n)
        .unwrap_or(0)
        .min(r.len())
        .min(q.len())
        .min(f.len())
        .min(res.len())
        .min(out.len());
    if let Some(fl) = flags {
        len = len.min(fl.len());
    }
    let mut sort: Vec<usize> = Vec::with_capacity(len);
    let mut start = if vi.normal_p != 0 {
        vi.normal_start.wrapping_sub(i)
    } else {
        n
    };
    if start > n || f64::from(nepeak) < -0.5 {
        start = n;
    }
    let start = usize::try_from(start).unwrap_or(0).min(len);

    // この帯域のエネルギーだけを見る古い振る舞い（4742。渡された acc は使わない）
    let mut acc = 0.0_f32;

    // noise normalization をしない所でも *out は埋める
    let mut j = 0;
    match flags {
        None => {
            // チャンネルごと（lossless・mono）
            while j < start {
                out[j] = rint_i32(res[j]);
                j += 1;
            }
        }
        Some(fl) => {
            while j < start {
                if fl[j] != 1 {
                    // M6 のための residue
                    let ve = f64::from(q[j] / f[j]).sqrt() as f32;
                    if r[j] < 0.0 {
                        out[j] = neg_rint_i32(ve);
                        res[j] = -ve;
                    } else {
                        out[j] = rint_i32(ve);
                        res[j] = ve;
                    }
                }
                j += 1;
            }
        }
    }

    // noise normalization の部分の大きさを並べる
    match flags {
        Some(fl) => {
            while j < len {
                // flags[j] != 1 は lossy に coupling した要素（lossless の要素は飛ばす）
                if fl[j] != 1 {
                    let mut ve = q[j] / f[j];
                    if ve < 0.25 && (j as i32) >= limit.wrapping_sub(i) {
                        acc += ve;
                        sort.push(j); // q は flag のない要素の |r|
                        // M6 のための residue
                        res[j] = if r[j] < 0.0 {
                            (-f64::from(ve).sqrt()) as f32
                        } else {
                            f64::from(ve).sqrt() as f32
                        };
                    } else {
                        // 0 でない量子化は acc に数えない。*out と q はここで決まる
                        ve = f64::from(ve).sqrt() as f32;
                        if r[j] < 0.0 {
                            out[j] = neg_rint_i32(ve);
                            res[j] = -ve;
                        } else {
                            out[j] = rint_i32(ve);
                            res[j] = ve;
                        }
                        q[j] = out[j].wrapping_mul(out[j]) as f32 * f[j];
                    }
                }
                j += 1;
            }
        }
        None => {
            // チャンネルごと（lossless・mono）
            while j < len {
                let ve = res[j] * res[j];
                if ve < 0.25 {
                    acc += ve;
                    sort.push(j);
                } else {
                    out[j] = rint_i32(res[j]);
                    q[j] = out[j].wrapping_mul(out[j]) as f32 * f[j];
                }
                j += 1;
            }
        }
    }

    acc += acc * nepeak * nepeak;

    let count = sort.len();
    if count > 0 {
        let mut iacc = (acc as i32).wrapping_add(1);
        if iacc > n {
            iacc = n;
        }
        // 選択ソート（要素が少ないときは qsort より速い）
        ssort(count as i32, iacc, &mut sort, q);

        for &e in &sort {
            if f64::from(acc) >= vi.normal_thresh {
                out[e] = unitnorm(r[e]) as i32;
                acc -= 1.0;
                q[e] = f[e];
            } else {
                out[e] = 0;
                q[e] = 0.0;
            }
        }
    }

    acc
}

/// 2 つの違う添字の要素を同時に借りる（C の `raw[Mi]`・`raw[Ai]` など）。
fn pair_mut<T>(v: &mut [T], a: usize, b: usize) -> Option<(&mut T, &mut T)> {
    if a == b {
        return None;
    }
    if a < b {
        let (lo, hi) = v.split_at_mut(b);
        Some((lo.get_mut(a)?, hi.first_mut()?))
    } else {
        let (lo, hi) = v.split_at_mut(a);
        let x = hi.first_mut()?;
        Some((x, lo.get_mut(b)?))
    }
}

/// `existe(x,y)`（psy.c:36）：`(x<-y || x>=y)`。y は double の定数。
fn existe(x: f32, y: f64) -> bool {
    f64::from(x) < -y || f64::from(x) >= y
}

/// `refer_phase(a,b)`（psy.c:37）：符号が逆か。
fn refer_phase(a: f32, b: f32) -> bool {
    (f64::from(a) > 0.0 && f64::from(b) < 0.0) || (f64::from(b) > 0.0 && f64::from(a) < 0.0)
}

/// `_vp_couple_quantize_normalize`（psy.c:4858–5142）：`iwork`（チャンネルごとの floor の整数 → 量子化した残差）。
pub(crate) fn vp_couple_quantize_normalize(
    blobno: i32,
    g: &VorbisInfoPsyGlobal,
    p: &VorbisLookPsy,
    vi: &VorbisInfoMapping0,
    mdct: &[Vec<f32>],
    enpeak: &[Vec<f32>],
    nepeak: &mut [Vec<f32>],
    iwork: &mut [Vec<i32>],
    nonzero: &mut [i32],
    sliding_lowpass: i32,
    ch: i32,
    lowpassr: i32,
) -> Result<(), EngineError> {
    couple_quantize_normalize_impl(
        blobno,
        g,
        p,
        vi,
        mdct,
        enpeak,
        nepeak,
        iwork,
        nonzero,
        sliding_lowpass,
        ch,
        lowpassr,
        None,
    )
}

/// [`vp_couple_quantize_normalize`] の本体。`pre` があれば、partition ごとのチャンネルごとの noise normalization のあと
/// （coupling の前）の `iwork` を写す（照合用のダンプの `RQPR` と同じ値。`lowpassr` より上は触らない）。
fn couple_quantize_normalize_impl(
    blobno: i32,
    g: &VorbisInfoPsyGlobal,
    p: &VorbisLookPsy,
    vi: &VorbisInfoMapping0,
    mdct: &[Vec<f32>],
    enpeak: &[Vec<f32>],
    nepeak: &mut [Vec<f32>],
    iwork: &mut [Vec<i32>],
    nonzero: &mut [i32],
    sliding_lowpass: i32,
    ch: i32,
    lowpassr: i32,
    mut pre: Option<&mut [Vec<i32>]>,
) -> Result<(), EngineError> {
    let n = p.n;
    let nu = usize::try_from(n).map_err(|_| internal("ブロックの大きさ"))?;
    let chu = usize::try_from(ch).map_err(|_| internal("チャンネルの数"))?;
    if mdct.len() < chu
        || enpeak.len() < chu
        || nepeak.len() < chu
        || iwork.len() < chu
        || nonzero.len() < chu
        || (0..chu).any(|k| mdct[k].len() < nu || enpeak[k].len() < nu || iwork[k].len() < nu)
    {
        return Err(internal("量子化の作業領域の大きさ"));
    }
    let partition = partition_of(&p.vi);
    let part = usize::try_from(partition)
        .ok()
        .filter(|&x| x > 0)
        .ok_or_else(|| internal("partition の大きさ"))?;
    let blob = usize::try_from(blobno).map_err(|_| internal("blob の番号"))?;
    let blockflag = usize::try_from(p.vi.blockflag).map_err(|_| internal("blockflag"))?;

    let limit = g
        .coupling_pointlimit
        .get(blockflag)
        .and_then(|v| v.get(blob))
        .copied()
        .ok_or_else(|| internal("coupling_pointlimit"))?;
    let threshold = |table: &[f64; 9], amp: Option<&i32>| {
        amp.and_then(|&a| usize::try_from(a).ok())
            .and_then(|a| table.get(a))
            .map(|&v| v as f32)
            .ok_or_else(|| internal("stereo_threshholds の添字"))
    };
    let pre_amp = g.coupling_prepointamp.get(blob);
    let post_amp = g.coupling_postpointamp.get(blob);
    let prepoint = threshold(&STEREO_THRESHHOLDS, pre_amp)?;
    let postpoint = threshold(&STEREO_THRESHHOLDS, post_amp)?;
    let mut prepoint_x = threshold(&STEREO_THRESHHOLDS_X, pre_amp)?;
    let mut postpoint_x = threshold(&STEREO_THRESHHOLDS_X, post_amp)?;

    let steps = usize::try_from(vi.coupling_steps).map_err(|_| internal("coupling_steps"))?;
    let mut couples = Vec::with_capacity(steps);
    for step in 0..steps {
        let mi = vi
            .coupling_mag
            .get(step)
            .and_then(|&m| usize::try_from(m).ok());
        let ai = vi
            .coupling_ang
            .get(step)
            .and_then(|&a| usize::try_from(a).ok());
        match (mi, ai) {
            (Some(mi), Some(ai)) if mi < chu && ai < chu && mi != ai => couples.push((mi, ai)),
            _ => return Err(internal("coupling のチャンネル")),
        }
    }

    // 量子化していないエネルギー（負は振幅が負）
    let mut raw = vec![vec![0.0_f32; part]; chu];
    // 量子化したエネルギー（flag が立っていれば）、でなければ mdct
    let mut quant = vec![vec![0.0_f32; part]; chu];
    // floor のエネルギー
    let mut floor = vec![vec![0.0_f32; part]; chu];
    // M6 のための residue
    let mut res = vec![vec![0.0_f32; part]; chu];
    // raw の要素の状態の印
    let mut flag = vec![vec![0_i32; part]; chu];
    // 0 でない印の作業領域
    let mut nz = vec![0_i32; chu];
    // エネルギーの過不足
    let mut acc = vec![0.0_f32; chu + steps];
    // M6 の変数
    let mut side_resdef = vec![-1.0_f32; steps];

    // 位相を戻す量の下限
    if prepoint_x < prepoint {
        prepoint_x = prepoint;
    }
    if postpoint_x < prepoint {
        postpoint_x = prepoint;
    }

    // M6 の位相の比
    let prae = if vi.coupling_steps == 1 {
        d2f(0.34)
    } else {
        d2f(0.825)
    };

    let nepeak_at = |nepeak: &[Vec<f32>], k: usize, pi: usize| {
        nepeak
            .get(k)
            .and_then(|v| v.get(pi))
            .copied()
            .ok_or_else(|| internal("nepeak の大きさ"))
    };

    // partition ごと
    let mut i: i32 = 0;
    let mut pi: usize = 0;
    while i < lowpassr {
        let jn = if partition > n - i { n - i } else { partition };
        if jn <= 0 {
            // lowpassr > n：C はこの先も回るが、出力に効く書き込みはない（作業領域の外を読む）
            break;
        }
        let iu = i as usize;
        let jnu = jn as usize;

        nz.copy_from_slice(&nonzero[..chu]);

        // 下ごしらえ
        for fl in &mut flag {
            fl.fill(0); // stereo の印を消す
        }
        let mut track = 0;
        for k in 0..chu {
            let iout = &mut iwork[k][iu..iu + jnu];
            if nz[k] != 0 {
                for j in 0..jnu {
                    // floor の大きさ
                    floor[k][j] = usize::try_from(iout[j])
                        .ok()
                        .and_then(|x| FLOOR1_FROMDB_LOOKUP.get(x))
                        .copied()
                        .ok_or_else(|| internal("floor の値が範囲の外です"))?;
                }

                // residue
                flag_lossless(
                    limit,
                    prepoint,
                    postpoint,
                    prepoint_x,
                    postpoint_x,
                    &mut res[k],
                    &mdct[k][iu..],
                    &enpeak[k][iu..],
                    &floor[k],
                    &mut flag[k],
                    i,
                    jn,
                );

                for j in 0..jnu {
                    // quant・raw の初期値
                    let m = mdct[k][iu + j];
                    raw[k][j] = m * m;
                    quant[k][j] = raw[k][j];
                    if m < 0.0 {
                        raw[k][j] *= -1.0;
                    }
                    floor[k][j] *= floor[k][j];
                }

                // チャンネルごとの noise normalization（lossless・mono のため）
                let ne = nepeak_at(nepeak, k, pi)?;
                acc[track] = noise_normalize(
                    p,
                    limit,
                    &raw[k],
                    &mut quant[k],
                    &floor[k],
                    &mut res[k],
                    None,
                    acc[track],
                    ne,
                    i,
                    jn,
                    iout,
                );
            } else {
                for j in 0..jnu {
                    floor[k][j] = 1e-10;
                    raw[k][j] = 0.0;
                    quant[k][j] = 0.0;
                    res[k][j] = 0.0;
                    flag[k][j] = 0;
                    iout[j] = 0;
                }
                acc[track] = 0.0;
            }
            track += 1;
        }

        if let Some(pre) = pre.as_deref_mut() {
            for (k, dst) in pre.iter_mut().enumerate().take(chu) {
                if let Some(dst) = dst.get_mut(iu..iu + jnu) {
                    dst.copy_from_slice(&iwork[k][iu..iu + jnu]);
                }
            }
        }

        // coupling
        for (step, &(mi, ai)) in couples.iter().enumerate() {
            if nz[mi] == 0 && nz[ai] == 0 {
                continue;
            }
            nz[mi] = 1;
            nz[ai] = 1;
            let mut pointflag = 0;

            let pair_err = || internal("coupling のチャンネル");
            let (re_m, re_a) = pair_mut(&mut raw, mi, ai).ok_or_else(pair_err)?;
            let (qe_m, qe_a) = pair_mut(&mut quant, mi, ai).ok_or_else(pair_err)?;
            let (floor_m, floor_a) = pair_mut(&mut floor, mi, ai).ok_or_else(pair_err)?;
            let (res_m, res_a) = pair_mut(&mut res, mi, ai).ok_or_else(pair_err)?;
            let (f_m, f_a) = pair_mut(&mut flag, mi, ai).ok_or_else(pair_err)?;
            let (iw_m, iw_a) = pair_mut(iwork, mi, ai).ok_or_else(pair_err)?;
            let i_m = &mut iw_m[iu..iu + jnu];
            let i_a = &mut iw_a[iu..iu + jnu];

            // M6：stereo の閾値を動かす（5012–5047）
            if p.tonefix_end > i {
                let mut rp = 0_i32;
                let mut pp = 0_i32;
                let mut residue_def = 0.0_f32;

                // 音のエネルギーの位相の比と residue の偏り
                for j in 0..jnu {
                    if existe(res_m[j], 0.5) || existe(res_a[j], 0.5) {
                        if refer_phase(re_m[j], re_a[j]) {
                            rp += 1;
                        } else {
                            pp += 1;
                        }
                        residue_def = (f64::from(residue_def)
                            + (f64::from(res_m[j]).abs() - f64::from(res_a[j]).abs()).abs())
                            as f32;
                    }
                }
                let ap = rp + pp;

                // 結果から lossless の印を立てる
                if ap != 0 {
                    // residue の偏り（正規化）
                    residue_def /= ap as f32;
                    let temp_def = residue_def;
                    if side_resdef[step] > 0.0 {
                        residue_def =
                            (f64::from(temp_def) * 0.5 + f64::from(side_resdef[step]) * 0.5) as f32;
                    }
                    side_resdef[step] = temp_def;
                    if residue_def > 1.0 {
                        for j in 0..jnu {
                            if f_m[j] == -1 || f_a[j] == -1 {
                                f_m[j] = 1;
                            }
                        }
                    }
                    // 音のエネルギーの位相の比
                    if rp as f32 / ap as f32 >= prae {
                        for j in 0..jnu {
                            if (f_m[j] == -1 || f_a[j] == -1) && refer_phase(re_m[j], re_a[j]) {
                                f_m[j] = 1;
                            }
                        }
                    }
                } else {
                    side_resdef[step] = -1.0;
                }
            }

            for j in 0..jnu {
                let jj = j as i32;
                if jj < sliding_lowpass.wrapping_sub(i) {
                    if f_m[j] == 1 || f_a[j] == 1 {
                        // lossless coupling
                        re_m[j] = (f64::from(re_m[j]).abs() + f64::from(re_a[j]).abs()) as f32;
                        qe_m[j] += qe_a[j];
                        f_m[j] = 1;
                        f_a[j] = 1;

                        lossless_couplingf(&mut res_m[j], &mut res_a[j]);

                        // iM・iA を coupling する
                        lossless_coupling(&mut i_m[j], &mut i_a[j]);
                    } else {
                        // lossy（point）coupling：aoTuV の hypot
                        let (hp_l, hp_h) = if vi.coupling_steps == 1 || step == 3 {
                            (0.18_f32, 0.12_f32) // step == 3 は 5.1 チャンネルの coupling のため
                        } else {
                            (0.18_f32, 0.04_f32)
                        };
                        let threv = if jj < limit.wrapping_sub(i) {
                            hp_l
                        } else {
                            hp_h
                        };
                        re_m[j] = min_indemnity_dipole_hypot(re_m[j], re_a[j], threv);

                        qe_m[j] = fabs_f(re_m[j]);
                        re_a[j] = 0.0;
                        qe_a[j] = 0.0;
                        f_a[j] = 1;
                        i_a[j] = 0;
                        res_a[j] = 0.0;

                        let ne_m = nepeak_at(nepeak, mi, pi)?;
                        let ne_a = nepeak_at(nepeak, ai, pi)?;
                        let new_m = if f64::from(ne_m) < -0.5 || f64::from(ne_a) < -0.5 {
                            -1.0 // noise normalization をしない
                        } else if ne_m > ne_a {
                            ne_a
                        } else {
                            ne_m
                        };
                        if let Some(v) = nepeak.get_mut(mi).and_then(|v| v.get_mut(pi)) {
                            *v = new_m;
                        }

                        // 印
                        pointflag |= 1;
                    }
                }
                let sum = floor_m[j] + floor_a[j];
                floor_m[j] = sum;
                floor_a[j] = sum;
            }

            // point stereo の大きさの列を正規化する
            if pointflag != 0 {
                let ne = nepeak_at(nepeak, mi, pi)?;
                acc[track] = noise_normalize(
                    p,
                    limit,
                    re_m,
                    qe_m,
                    floor_m,
                    res_m,
                    Some(f_m),
                    acc[track],
                    ne,
                    i,
                    jn,
                    i_m,
                );
            }
            track += 1;
        }

        i += partition;
        pi += 1;
    }

    if lowpassr < n {
        let from = usize::try_from(lowpassr).unwrap_or(0);
        for w in iwork.iter_mut().take(chu) {
            for v in w.iter_mut().take(nu).skip(from) {
                *v = 0;
            }
        }
    }

    for &(mi, ai) in &couples {
        // 0 のチャンネルと 0 でないチャンネルの coupling は、両方を 0 でないにする
        if nonzero[mi] != 0 || nonzero[ai] != 0 {
            nonzero[mi] = 1;
            nonzero[ai] = 1;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vorbis::oracle::{self, Dump, Signal, json_f32, json_f64};
    use serde_json::Value;
    use vorbis_tables::PSY_INFO_TEMPLATE;

    // ---- 照合の設定の JSON → 型（setup.rs がまだないので、試験の中で oracle::setup の JSON から組む） ----

    fn f32_list(v: &Value) -> Vec<f32> {
        v.as_array().unwrap().iter().map(json_f32).collect()
    }

    fn i32_list(v: &Value) -> Vec<i32> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| i32::try_from(x.as_i64().unwrap()).unwrap())
            .collect()
    }

    fn f32_array<const N: usize>(v: &Value) -> [f32; N] {
        f32_list(v).try_into().unwrap()
    }

    fn i32_array<const N: usize>(v: &Value) -> [i32; N] {
        i32_list(v).try_into().unwrap()
    }

    /// 前から埋め、残りは 0（C の配列の欄のうち使う所だけが JSON にある）。
    fn i32_prefix<const N: usize>(v: &Value) -> [i32; N] {
        let mut out = [0; N];
        for (o, x) in out.iter_mut().zip(i32_list(v)) {
            *o = x;
        }
        out
    }

    fn int(v: &Value) -> i32 {
        i32::try_from(v.as_i64().unwrap()).unwrap()
    }

    /// `vorbis_info_psy` の JSON（oracle_setup の `psy[i]`）。
    fn psy_from_json(v: &Value) -> VorbisInfoPsy {
        let noiseoff = v["noiseoff"].as_array().unwrap();
        VorbisInfoPsy {
            blockflag: int(&v["blockflag"]),
            ath_adjatt: json_f32(&v["ath_adjatt"]),
            ath_maxatt: json_f32(&v["ath_maxatt"]),
            tone_masteratt: f32_array(&v["tone_masteratt"]),
            tone_centerboost: json_f32(&v["tone_centerboost"]),
            tone_decay: json_f32(&v["tone_decay"]),
            tone_abs_limit: json_f32(&v["tone_abs_limit"]),
            toneatt: f32_array(&v["toneatt"]),
            noisemaskp: int(&v["noisemaskp"]),
            noisemaxsupp: json_f32(&v["noisemaxsupp"]),
            noisewindowlo: json_f32(&v["noisewindowlo"]),
            noisewindowhi: json_f32(&v["noisewindowhi"]),
            noisewindowlomin: int(&v["noisewindowlomin"]),
            noisewindowhimin: int(&v["noisewindowhimin"]),
            noisewindowfixed: int(&v["noisewindowfixed"]),
            noiseoff: [
                f32_array(&noiseoff[0]),
                f32_array(&noiseoff[1]),
                f32_array(&noiseoff[2]),
            ],
            noisecompand: f32_array(&v["noisecompand"]),
            noisecompand_high: f32_array(&v["noisecompand_high"]),
            flacint: json_f32(&v["flacint"]),
            max_curve_db: json_f32(&v["max_curve_dB"]),
            normal_p: int(&v["normal_p"]),
            normal_start: int(&v["normal_start"]),
            normal_partition: int(&v["normal_partition"]),
            normal_thresh: json_f64(&v["normal_thresh"]),
        }
    }

    /// `vorbis_info_psy_global` の JSON（oracle_setup の `psy_global`）。
    fn psy_global_from_json(v: &Value) -> VorbisInfoPsyGlobal {
        let two = |k: &str| {
            let a = v[k].as_array().unwrap();
            [i32_array(&a[0]), i32_array(&a[1])]
        };
        VorbisInfoPsyGlobal {
            eighth_octave_lines: int(&v["eighth_octave_lines"]),
            preecho_thresh: f32_array(&v["preecho_thresh"]),
            postecho_thresh: f32_array(&v["postecho_thresh"]),
            stretch_penalty: json_f32(&v["stretch_penalty"]),
            preecho_minenergy: json_f32(&v["preecho_minenergy"]),
            ampmax_att_per_sec: json_f32(&v["ampmax_att_per_sec"]),
            coupling_pkhz: i32_array(&v["coupling_pkHz"]),
            coupling_pointlimit: two("coupling_pointlimit"),
            coupling_prepointamp: i32_array(&v["coupling_prepointamp"]),
            coupling_postpointamp: i32_array(&v["coupling_postpointamp"]),
            sliding_lowpass: two("sliding_lowpass"),
        }
    }

    /// `vorbis_info_mapping0` の JSON（oracle_setup の `maps[i]`）。
    fn mapping_from_json(v: &Value) -> VorbisInfoMapping0 {
        VorbisInfoMapping0 {
            submaps: int(&v["submaps"]),
            chmuxlist: i32_prefix(&v["chmuxlist"]),
            floorsubmap: i32_prefix(&v["floorsubmap"]),
            residuesubmap: i32_prefix(&v["residuesubmap"]),
            coupling_steps: int(&v["coupling_steps"]),
            coupling_mag: i32_prefix(&v["coupling_mag"]),
            coupling_ang: i32_prefix(&v["coupling_ang"]),
        }
    }

    /// 照合用の設定から、psys 個の look（`_vds_shared_init` と同じく n = blocksizes[blockflag] / 2）。
    fn looks_from_setup(js: &Value, rate: u32) -> (VorbisInfoPsyGlobal, Vec<VorbisLookPsy>) {
        let g = psy_global_from_json(&js["psy_global"]);
        let blocksizes: Vec<i64> = js["blocksizes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect();
        let looks = js["psy"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pj| {
                let vi = psy_from_json(pj);
                let n = blocksizes[usize::try_from(vi.blockflag).unwrap()] / 2;
                vp_psy_init(&vi, &g, i32::try_from(n).unwrap(), i64::from(rate)).unwrap()
            })
            .collect();
        (g, looks)
    }

    // ---- 比べ方 ----

    /// f32 をビットで比べる（NaN は符号・中身を問わず同じとみなす。照合用の README §8.3）。
    fn same_f32(a: f32, b: f32) -> bool {
        a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
    }

    fn assert_f32s(what: &str, got: &[f32], want: &[f32]) {
        assert_eq!(got.len(), want.len(), "{what}：長さ");
        if let Some(k) = (0..got.len()).find(|&k| !same_f32(got[k], want[k])) {
            panic!(
                "{what}[{k}]：{} (0x{:08x}) と C {} (0x{:08x})",
                got[k],
                got[k].to_bits(),
                want[k],
                want[k].to_bits()
            );
        }
    }

    fn assert_i32s(what: &str, got: &[i32], want: &[i32]) {
        assert_eq!(got.len(), want.len(), "{what}：長さ");
        if let Some(k) = (0..got.len()).find(|&k| got[k] != want[k]) {
            panic!("{what}[{k}]：{} と C {}", got[k], want[k]);
        }
    }

    fn count_nan(v: &[f32]) -> usize {
        v.iter().filter(|x| x.is_nan()).count()
    }

    // ---- CI で回る性質のテスト（照合用の C がなくても回る） ----

    /// 44.1 kHz の psy（noise window は q5 の短い窓のもの）。導出のスカラーと表の先頭・末尾は照合用の
    /// `oracle_setup 44100 2 5 --look` で確かめた値（ATH・octave・firstoc などは vi によらず、bark は noise window の
    /// 4 つの値だけで決まる）。
    fn template_psy() -> VorbisInfoPsy {
        VorbisInfoPsy {
            blockflag: 0,
            noisewindowlo: 0.5,
            noisewindowhi: 0.5,
            noisewindowlomin: 3,
            noisewindowhimin: 3,
            noisewindowfixed: 15,
            normal_p: 1,
            normal_start: 9999,
            normal_partition: 8,
            normal_thresh: 6.0,
            ..PSY_INFO_TEMPLATE
        }
    }

    fn template_global() -> VorbisInfoPsyGlobal {
        VorbisInfoPsyGlobal {
            eighth_octave_lines: 8,
            ampmax_att_per_sec: -6.0,
            ..VorbisInfoPsyGlobal::default()
        }
    }

    #[test]
    fn init_known_values_44k_short() {
        let look = vp_psy_init(&template_psy(), &template_global(), 128, 44_100).unwrap();
        assert_eq!(look.n, 128);
        assert_eq!(look.firstoc, -42);
        assert_eq!(look.shiftoc, 5);
        assert_eq!(look.eighth_octave_lines, 8);
        assert_eq!(look.total_octave_lines, 585);
        assert_eq!(look.m_val.to_bits(), 0x3f80_0000);
        assert_eq!(look.tonecomp_endp, 90);
        assert_eq!(look.tonecomp_thres.to_bits(), 0x3f66_6666);
        assert_eq!(look.tonefix_end, 76);
        let ath: Vec<u32> = look.ath.iter().map(|x| x.to_bits()).collect();
        assert_eq!(
            ath[..6],
            [
                0x41f8_0000,
                0x4130_0000,
                0x40a0_0000,
                0x4040_0000,
                0x4000_0000,
                0x3f80_0000
            ]
        );
        assert_eq!(ath[125..], [0x4281_1746, 0x4282_e8bb, 0x4284_ba30]);
        assert_eq!(look.octave[..6], [-33, 114, 168, 202, 227, 247]);
        assert_eq!(look.octave[125..], [540, 540, 541]);
        assert_eq!(
            look.bark[..6],
            [-262_142, -196_605, -131_068, -65_531, 6, 65_543]
        );
        assert_eq!(look.bark[125..], [7_078_016, 7_143_552, 7_209_088]);
    }

    #[test]
    fn init_known_values_44k_long() {
        let vi = VorbisInfoPsy {
            noisewindowlomin: 10,
            noisewindowhimin: 10,
            noisewindowfixed: 100,
            blockflag: 1,
            ..template_psy()
        };
        let look = vp_psy_init(&vi, &template_global(), 1024, 44_100).unwrap();
        assert_eq!(look.firstoc, -234);
        assert_eq!(look.total_octave_lines, 777);
        assert_eq!((look.tonecomp_endp, look.tonefix_end), (720, 608));
        let ath: Vec<u32> = look.ath.iter().map(|x| x.to_bits()).collect();
        assert_eq!(ath[..3], [0x428a_0000, 0x4254_0000, 0x4224_0000]);
        assert_eq!(ath[1021..], [0x4285_1a6e, 0x4285_5547, 0x4285_9020]);
        assert_eq!(look.octave[..6], [-225, -77, -23, 10, 35, 55]);
        assert_eq!(look.octave[1021..], [541, 541, 542]);
        assert_eq!(look.bark[..3], [-720_887, -655_350, -589_813]);
        assert_eq!(look.bark[1021..], [57_934_848, 58_000_384, 58_065_920]);
    }

    /// すべてのレート・窓の大きさで、表の形・範囲・決定性。
    #[test]
    fn init_shapes_ranges_and_determinism() {
        let g = template_global();
        for &rate in &[
            8_000_i64, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
        ] {
            for &n in &[64, 128, 256, 512, 1024, 2048, 4096] {
                let vi = template_psy();
                let look = vp_psy_init(&vi, &g, n, rate).unwrap();
                let nn = usize::try_from(n).unwrap();
                assert_eq!(look.ath.len(), nn);
                assert_eq!(look.octave.len(), nn);
                assert_eq!(look.bark.len(), nn);
                assert_eq!(look.noiseoffset.len(), P_NOISECURVES);
                assert!(look.noiseoffset.iter().all(|v| v.len() == nn));
                assert!(look.ath.iter().all(|x| x.is_finite()));
                // octave は増えていき、seed の範囲に入る
                assert!(look.octave.windows(2).all(|w| w[0] <= w[1]));
                let tol = i64::from(look.total_octave_lines);
                assert!(
                    look.octave
                        .iter()
                        .all(|&o| (0..tol).contains(&(o - look.firstoc)))
                );
                // bark の hi − 1 は n 以下、lo − 1 は i より小さい
                for (i, &b) in look.bark.iter().enumerate() {
                    assert!((b & 0xffff) <= i64::from(n), "rate {rate} n {n} bark[{i}]");
                    assert!((b >> 16) < i as i64, "rate {rate} n {n} bark[{i}]");
                }
                // トーンの曲線：[17][8][58]、fencepost は 0〜16 と 17〜55
                assert_eq!(look.tonecurves.len(), P_BANDS);
                for band in &look.tonecurves {
                    assert_eq!(band.len(), P_LEVELS);
                    for c in band {
                        assert_eq!(c.len(), EHMER_MAX + 2);
                        assert!((0.0..=EHMER_OFFSET as f32).contains(&c[0]));
                        assert!(((EHMER_OFFSET + 1) as f32..EHMER_MAX as f32).contains(&c[1]));
                        assert!(c[2..].iter().all(|x| x.is_finite() && *x <= 999.0));
                    }
                }
                // M1・M4・M6 は 26 kHz 以上だけ
                if rate < 26_000 {
                    assert_eq!(look.m_val.to_bits(), 0);
                    assert_eq!(look.tonefix_end, 0);
                } else {
                    assert!(look.m_val > 0.0);
                }
                assert_eq!(vp_psy_init(&vi, &g, n, rate).unwrap(), look);
            }
        }
        assert!(vp_psy_init(&template_psy(), &g, 1, 44_100).is_err());
        assert!(vp_psy_init(&template_psy(), &g, 128, 0).is_err());
        let bad = VorbisInfoPsyGlobal {
            eighth_octave_lines: 0,
            ..g
        };
        assert!(vp_psy_init(&template_psy(), &bad, 128, 44_100).is_err());
    }

    /// ノイズのオフセットは帯域の値の線形補間（すべて 1 なら 1、帯域の番号なら 0〜16 で増える）。
    #[test]
    fn noise_offset_interpolates_bands() {
        let mut vi = template_psy();
        vi.noiseoff = [[1.0; P_BANDS], [0.0; P_BANDS], [0.0; P_BANDS]];
        for (b, v) in vi.noiseoff[1].iter_mut().enumerate() {
            *v = b as f32;
        }
        let look = vp_psy_init(&vi, &template_global(), 1024, 48_000).unwrap();
        assert!(
            look.noiseoffset[0]
                .iter()
                .all(|x| x.to_bits() == 1.0_f32.to_bits())
        );
        assert!(look.noiseoffset[1].windows(2).all(|w| w[0] <= w[1]));
        assert!(
            look.noiseoffset[1]
                .iter()
                .all(|&x| (0.0..=16.0).contains(&x))
        );
        assert!(look.noiseoffset[2].iter().all(|x| x.to_bits() == 0));
    }

    /// 平らな logmdct からはほぼ平らなノイズのマスク。26 kHz 未満（aoTuV の M7・M8・M9 の表が 0）では epeak・npeak は 0。
    #[test]
    fn noisemask_flat_input() {
        let vi = VorbisInfoPsy {
            noisecompand: std::array::from_fn(|k| k as f32),
            ..template_psy()
        };
        let look = vp_psy_init(&vi, &template_global(), 256, 22_050).unwrap();
        assert_eq!(
            (look.tonefix_end, look.min_nn_lp, look.tonecomp_endp),
            (0, 0, 0)
        );
        let logmdct = vec![-60.0_f32; 256];
        let lastmdct = vec![0.0_f32; 256];
        let mut epeak = vec![7.0_f32; 256];
        let mut npeak = vec![7.0_f32; 256 / 8];
        let mut logmask = vec![0.0_f32; 256];
        vp_noisemask(
            &look,
            -1.0,
            &logmdct,
            &lastmdct,
            &mut epeak,
            &mut npeak,
            &mut logmask,
            -1.0,
            3,
        );
        assert!(epeak.iter().all(|x| x.to_bits() == 0));
        assert!(npeak.iter().all(|x| x.to_bits() == 0));
        // 1 回目の当てはめは −60 付近、差は 0 なので 2 回目は下限の 1 付近、compander は dB の添字（0）の値 0 を足す
        for (i, &m) in logmask.iter().enumerate() {
            assert!((-61.5..=-58.5).contains(&m), "logmask[{i}] = {m}");
        }
        // 決定的
        let mut again = vec![0.0_f32; 256];
        vp_noisemask(
            &look, -1.0, &logmdct, &lastmdct, &mut epeak, &mut npeak, &mut again, -1.0, 3,
        );
        assert_eq!(
            logmask.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            again.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
    }

    /// aoTuV の M2・M8・M9（44.1 kHz の長い窓）：M2 は poste > 0 で min_nn_lp までのマスクを下げて npeak を −1 に、M8 の npeak は
    /// 0〜1、M9 の epeak は tonecomp_endp まで（前のブロックより 1 dB 以上大きい所だけ）で、そこより上は 0。
    #[test]
    fn noisemask_aotuv_m2_m8_m9() {
        let vi = VorbisInfoPsy {
            noisecompand: std::array::from_fn(|k| k as f32),
            blockflag: 1,
            ..template_psy()
        };
        let look = vp_psy_init(&vi, &template_global(), 1024, 44_100).unwrap();
        assert_eq!(
            (look.tonecomp_endp, look.min_nn_lp, look.tonefix_end),
            (720, 696, 608)
        );
        let part = usize::try_from(partition_of(&look.vi)).unwrap();
        let logmdct: Vec<f32> = (0..1024)
            .map(|i| if i % 37 == 5 { -10.0 } else { -60.0 })
            .collect();
        let lastmdct = vec![-80.0_f32; 1024];
        let run = |poste: f32, block_mode: i32| {
            let mut epeak = vec![0.0_f32; 1024];
            let mut npeak = vec![0.0_f32; 1024 / part];
            let mut logmask = vec![0.0_f32; 1024];
            vp_noisemask(
                &look,
                -1.0,
                &logmdct,
                &lastmdct,
                &mut epeak,
                &mut npeak,
                &mut logmask,
                poste,
                block_mode,
            );
            (epeak, npeak, logmask)
        };
        let (epeak, npeak, base) = run(-1.0, 3);
        assert!(npeak.iter().all(|&x| (0.0..=1.0).contains(&x)));
        assert!(epeak[720..].iter().all(|x| x.to_bits() == 0));
        assert!(epeak[..720].iter().any(|&x| x >= 1.0));
        assert!(epeak.iter().all(|&x| x == 0.0 || x >= 1.0));
        // 短い窓・過渡では M9 は… 過渡は動き、padding（1）は epeak を 0 にする
        let (epeak_pad, ..) = run(-1.0, 1);
        assert!(epeak_pad.iter().all(|x| x.to_bits() == 0));
        // M2：poste = 10 なら min_nn_lp（696）までの partition のマスクが 10（noiseoffset[1] + 30 より小さい）下がる
        let (_, npeak2, low) = run(10.0, 3);
        let lp = 696usize.div_ceil(part) * part;
        for i in 0..lp {
            assert_eq!(low[i].to_bits(), (base[i] - 10.0).to_bits(), "logmask[{i}]");
        }
        assert_eq!(low[lp..], base[lp..]);
        assert!(npeak2[..lp / part].iter().all(|&x| x == -1.0));
    }

    /// M7（ntfix）：短い窓では山の幅でノイズを下げ、過渡では 8 個ずつの平均の山で下げる。長い窓・26 kHz 未満は変えない。
    #[test]
    fn ntfix_lowers_noise_at_peaks() {
        let g = template_global();
        let short = vp_psy_init(&template_psy(), &g, 128, 44_100).unwrap();
        let mut spectral = vec![-60.0_f32; 128];
        spectral[20] = -20.0;
        spectral[19] = -40.0;
        spectral[21] = -40.0;
        let noise0 = vec![-30.0_f32; 128];
        let mut noise = noise0.clone();
        ntfix(&short, &spectral, &mut noise, 0);
        assert!(noise[19..=21].iter().all(|&x| x < -30.0));
        assert!(
            noise[..18]
                .iter()
                .all(|x| x.to_bits() == (-30.0_f32).to_bits())
        );
        let mut same = noise0.clone();
        ntfix(&short, &spectral, &mut same, 3);
        assert_eq!(same, noise0);
        let low = vp_psy_init(&template_psy(), &g, 128, 22_050).unwrap();
        let mut same = noise0.clone();
        ntfix(&low, &spectral, &mut same, 0);
        assert_eq!(same, noise0);
        // 過渡：noise の 8 個ずつの平均に山があれば、その前後 3 組を下げる
        let vi = VorbisInfoPsy {
            blockflag: 1,
            ..template_psy()
        };
        let long = vp_psy_init(&vi, &g, 1024, 44_100).unwrap();
        let mut noise: Vec<f32> = vec![-50.0; 1024];
        for v in &mut noise[80..88] {
            *v = -30.0;
        }
        let before = noise.clone();
        ntfix(&long, &spectral, &mut noise, 2);
        // 山は 10 組目（80〜87）。前の組（9）が前々（8）より大きくないので a = 8、b = 13 の組の頭まで（64〜104）
        assert!(
            noise[64..=104]
                .iter()
                .zip(&before[64..=104])
                .all(|(a, b)| a < b)
        );
        assert_eq!(noise[..64], before[..64]);
        assert_eq!(noise[105..], before[105..]);
    }

    /// M3 の表：`freq_bfn128`・`freq_bfn256` の広がり（i + fb[i]）は n を越えない（set_m3p の tempmdct[i+j] が範囲の中）。
    #[test]
    fn m3_tables_stay_in_range() {
        for t in [&FREQ_BFN128[..], &FREQ_BFN256[..]] {
            for (i, &fb) in t.iter().enumerate() {
                assert!(fb >= 1 && i + fb as usize <= t.len(), "i {i} fb {fb}");
            }
        }
    }

    /// M5（lb_loudnoise_fix）：過渡と長いブロックの境だけで決め、ほかは前の値。26 kHz 未満・q3 以上（normal_thresh > .45）は −1。
    #[test]
    fn loudnoise_fix_levels() {
        let g = template_global();
        let vi = VorbisInfoPsy {
            normal_thresh: 0.2,
            blockflag: 1,
            ..template_psy()
        };
        let look = vp_psy_init(&vi, &g, 1024, 44_100).unwrap();
        let quiet = vec![-120.0_f32; 1024];
        let loud = vec![-20.0_f32; 1024];
        assert_eq!(lb_loudnoise_fix(&look, 0.25, &quiet, 3, 3), 0.25);
        assert_eq!(lb_loudnoise_fix(&look, 0.25, &quiet, 2, 3), 1.0);
        assert_eq!(lb_loudnoise_fix(&look, 0.25, &loud, 3, 2), -1.0);
        // hi_th = −45 なら 1 − (5/10) = 0.5
        let mid = vec![-90.0_f32; 1024];
        assert_eq!(lb_loudnoise_fix(&look, 0.25, &mid, 3, 2), 0.5);
        let q5 = vp_psy_init(&template_psy(), &g, 1024, 44_100).unwrap();
        assert_eq!(lb_loudnoise_fix(&q5, 0.25, &quiet, 2, 3), -1.0);
        let low = vp_psy_init(&vi, &g, 1024, 22_050).unwrap();
        assert_eq!(lb_loudnoise_fix(&low, 0.25, &quiet, 2, 3), -1.0);
    }

    /// M2（_postnoise_detection）：前が impulse の過渡（2048 以上）で、前半が大きく後半が静かなときだけ正。
    #[test]
    fn postnoise_detection_cases() {
        let mut pcm = vec![0.0_f32; 2048];
        for v in &mut pcm[512..1024] {
            *v = 0.5;
        }
        let r = postnoise_detection(&pcm, 2048, 2, 0);
        let want = ((512.0_f64 * 0.5) * (512.0 * 0.5)) as f32;
        assert_eq!(r.to_bits(), want.to_bits());
        assert_eq!(postnoise_detection(&pcm, 2048, 3, 0), -1.0);
        assert_eq!(postnoise_detection(&pcm, 2048, 2, 1), -1.0);
        assert_eq!(postnoise_detection(&pcm[..1024], 1024, 2, 0), -1.0);
        // 後半が大きい（平均 0.01 を越える）なら −1
        let loud = vec![0.5_f32; 2048];
        assert_eq!(postnoise_detection(&loud, 2048, 2, 0), -1.0);
        // 短い pcm（C では範囲の外）でも panic しない
        assert_eq!(postnoise_detection(&pcm[..100], 2048, 2, 0), -1.0);
    }

    /// M3 SET lastmdct：短い窓は次が長ければ 8 倍、過渡は次が短ければ 8 個ずつの最小、ほかはそのまま。書かない所は前の値。
    #[test]
    fn m3_lastmdct_update() {
        let logmdct: Vec<f32> = (0..128).map(|i| i as f32).collect();
        let mut last = vec![-1.0_f32; 2048];
        m3_set_lastmdct(&mut last, &logmdct, 128, 0, 1);
        assert!((0..1024).all(|k| last[k] == (k / 8) as f32));
        assert!(last[1024..].iter().all(|&x| x == -1.0));
        let mut last = vec![-1.0_f32; 2048];
        m3_set_lastmdct(&mut last, &logmdct, 128, 1, 0);
        assert_eq!(last[..128], logmdct[..]);
        assert!(last[128..].iter().all(|&x| x == -1.0));
        let long: Vec<f32> = (0..1024).map(|i| -(i as f32)).collect();
        let mut last = vec![-1.0_f32; 2048];
        m3_set_lastmdct(&mut last, &long, 1024, 2, 0);
        assert!((0..128).all(|k| last[k] == -((k * 8 + 7) as f32)));
        assert!(last[128..].iter().all(|&x| x == -1.0));
        let mut last = vec![-1.0_f32; 2048];
        m3_set_lastmdct(&mut last, &long, 1024, 3, 1);
        assert_eq!(last[..1024], long[..]);
        // 短すぎる lastmdct（C では範囲の外）には書かない
        let mut short = vec![-1.0_f32; 100];
        m3_set_lastmdct(&mut short, &logmdct, 128, 0, 1);
        assert!(short.iter().all(|&x| x == -1.0));
    }

    /// aoTuV の窓の状態（mapping0.c:1297–1305）と、M3 の impulse の続き：前が impulse なら tempmdct を引き継いで下げ、
    /// そうでなければ lastmdct から作り直す。impulse でないブロックは tempmdct を触らない。
    #[test]
    fn m3_tempmdct_carry_and_reset() {
        let g = template_global();
        let look = vp_psy_init(&template_psy(), &g, 128, 44_100).unwrap();
        let part = usize::try_from(partition_of(&look.vi)).unwrap();
        let noise = vec![-20.0_f32; 128];
        let tone = vec![-80.0_f32; 128];
        let logmdct = vec![-10.0_f32; 128];
        let run = |temp: &mut Vec<f32>, block_mode: i32, lw_block_mode: i32, lw_no: i32| {
            let mut last = vec![-50.0_f32; 2048];
            let mut mask = vec![0.0_f32; 128];
            let mut mdct = vec![0.1_f32; 128];
            let mut npeak = vec![0.5_f32; 128 / part];
            vp_offset_and_mix(
                &look,
                &noise,
                &tone,
                1,
                0,
                &mut mask,
                &mut mdct,
                &logmdct,
                &mut last,
                temp,
                -1.0,
                &mut npeak,
                100,
                block_mode,
                0,
                lw_block_mode,
                lw_no,
                0,
            );
            (mask, npeak, last)
        };
        // padding は触らない
        let mut temp = vec![3.0_f32; 256];
        let (mask_pad, npeak_pad, last) = run(&mut temp, 1, 0, 1);
        assert!(temp.iter().all(|&x| x == 3.0));
        assert!(npeak_pad.iter().all(|&x| x == 0.5));
        assert_eq!(last[..128], logmdct[..]);
        // 前が padding の impulse：lastmdct − 5 から作り直す → 大きな logmdct は tempmdct を上書きし、M3 がマスクを下げる
        let (mask_imp, npeak_imp, _) = run(&mut temp, 0, 1, 1);
        assert!(temp[..128].iter().any(|&x| x == -10.0));
        assert!(temp[128..].iter().all(|&x| x == 3.0));
        assert!(mask_imp.iter().zip(&mask_pad).any(|(a, b)| a < b));
        assert!(npeak_imp.iter().all(|&x| x == 0.0 || x == -1.0));
        // 前も impulse：tempmdct を引き継いで 5 下げる（tempmdct が大きいと M3 は働かず、上書きもしない）
        let mut temp = vec![100.0_f32; 256];
        let (mask_carry, ..) = run(&mut temp, 0, 0, 1);
        assert!(temp[..128].iter().all(|&x| x == 95.0));
        assert!(temp[128..].iter().all(|&x| x == 100.0));
        assert_eq!(mask_carry, mask_pad);
        // 窓の状態
        let s = super::super::mapping0::AotuvWindowState::default();
        let s = s.next(0);
        assert_eq!((s.lw_block_mode, s.lw_no, s.impadnum), (0, 1, 0));
        let s = s.next(1);
        assert_eq!((s.lw_block_mode, s.lw_no, s.impadnum), (1, 1, 1));
        let s = s.next(1);
        assert_eq!((s.lw_block_mode, s.lw_no, s.impadnum), (1, 2, 2));
        let s = (0..10).fold(s, |s, _| s.next(1));
        assert_eq!((s.lw_no, s.impadnum), (12, 8));
        let s = s.next(2);
        assert_eq!((s.lw_block_mode, s.lw_no, s.impadnum), (2, 1, 0));
    }

    /// トーンがない（logfft がとても低い）とき、トーンのマスクは ATH ＋ att だけ（att の下限は ath_maxatt）。
    #[test]
    fn tonemask_without_tones_is_ath() {
        let vi = VorbisInfoPsy {
            ath_adjatt: -10.0,
            ath_maxatt: -20.0,
            ..template_psy()
        };
        let look = vp_psy_init(&vi, &template_global(), 128, 44_100).unwrap();
        let logfft = vec![-400.0_f32; 128];
        let mut logmask = vec![0.0_f32; 128];
        vp_tonemask(&look, &logfft, &mut logmask, -30.0, -30.0);
        for i in 0..128 {
            assert_eq!(logmask[i].to_bits(), (look.ath[i] - 20.0).to_bits());
        }
        // 大きなトーンがあれば、そのまわりのマスクが上がる（tone_abs_limit で頭を抑えない設定で）
        let vi = VorbisInfoPsy {
            tone_abs_limit: 200.0,
            ..vi
        };
        let look = vp_psy_init(&vi, &template_global(), 128, 44_100).unwrap();
        let mut logfft = vec![-400.0_f32; 128];
        logfft[40] = 60.0;
        let mut tone = vec![0.0_f32; 128];
        vp_tonemask(&look, &logfft, &mut tone, 60.0, -30.0);
        let base: Vec<f32> = look.ath.iter().map(|&a| a - 20.0).collect();
        assert!(tone.iter().zip(&base).all(|(&t, &b)| t >= b));
        assert!(tone[40] > base[40]);
        assert!(
            tone[..30]
                .iter()
                .zip(&base)
                .all(|(t, b)| t.to_bits() == b.to_bits())
        );
    }

    /// M1：26 kHz 未満（m_val = 0）と offset_select ≠ 1 では MDCT を変えない。26 kHz 以上では弱める（下限 0.0001 倍）。
    #[test]
    fn offset_and_mix_m1() {
        let g = template_global();
        let noise = vec![-50.0_f32; 128];
        let tone = vec![-80.0_f32; 128];
        let logmdct = vec![-40.0_f32; 128];
        let mdct: Vec<f32> = (0..128).map(|i| (i as f32 - 64.0) * 0.01).collect();
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        // 長いブロックの形（block_mode 3）で呼ぶ（M3 は impulse だけ）
        let mix =
            |p: &VorbisLookPsy, sel: i32, mask: &mut [f32], m: &mut [f32], logmdct: &[f32]| {
                let mut last = vec![0.0_f32; 2048];
                let mut temp = vec![0.0_f32; 256];
                let mut npeak = vec![0.0_f32; 128];
                vp_offset_and_mix(
                    p, &noise, &tone, sel, 0, mask, m, logmdct, &mut last, &mut temp, -1.0,
                    &mut npeak, 100, 3, 1, 3, 1, 0,
                );
            };
        let low = vp_psy_init(&template_psy(), &g, 128, 22_050).unwrap();
        let mut m = mdct.clone();
        let mut mask = vec![0.0_f32; 128];
        mix(&low, 1, &mut mask, &mut m, &logmdct);
        assert_eq!(bits(&m), bits(&mdct));
        let high = vp_psy_init(&template_psy(), &g, 128, 44_100).unwrap();
        let mut m = mdct.clone();
        mix(&high, 0, &mut mask, &mut m, &logmdct);
        assert_eq!(bits(&m), bits(&mdct));
        // noise ＋ オフセット（PSY_INFO_TEMPLATE の noiseoff は 0 番の帯域が −1 で、ほかは 0）が tone より上：マスクは noise の側
        assert!(mask.iter().all(|&x| (-51.0..=-50.0).contains(&x)));
        let mut m = mdct.clone();
        mix(&high, 1, &mut mask, &mut m, &logmdct);
        for (a, b) in m.iter().zip(&mdct) {
            assert!(a.abs() <= b.abs());
        }
        // マスク（noisemaxsupp = −0 で頭打ち）より 250 dB 下の MDCT：1 − (267.2 × 0.005) < 0 なので 0.0001 倍
        let quiet = vec![-250.0_f32; 128];
        let mut m = vec![1.0_f32; 128];
        mix(&high, 1, &mut mask, &mut m, &quiet);
        let floor = d2f(0.0001);
        assert!(m.iter().all(|x| x.to_bits() == floor.to_bits()));
    }

    #[test]
    fn ampmax_decay_known_value() {
        let mut vd = VorbisDspState::default();
        vd.vi.rate = 44_100;
        vd.vi.codec_setup.blocksizes = [256, 2048];
        vd.vi.codec_setup.psy_g_param = template_global();
        vd.w = 1;
        let want = -10.0_f32 + (1024.0_f32 / 44_100.0_f32) * -6.0_f32;
        assert_eq!(vp_ampmax_decay(-10.0, &vd).to_bits(), want.to_bits());
        vd.w = 0;
        let want = -10.0_f32 + (128.0_f32 / 44_100.0_f32) * -6.0_f32;
        assert_eq!(vp_ampmax_decay(-10.0, &vd).to_bits(), want.to_bits());
        assert_eq!(
            vp_ampmax_decay(-9999.0, &vd).to_bits(),
            (-9999.0_f32).to_bits()
        );
        assert_eq!(
            vp_global_look(3),
            VorbisLookPsyGlobal {
                ampmax: -9999.0,
                channels: 3
            }
        );
    }

    /// Vorbis I の逆の square polar（mapping0_inverse）。
    fn inverse_polar(mag: i32, ang: i32) -> (i32, i32) {
        if mag > 0 {
            if ang > 0 {
                (mag, mag - ang)
            } else {
                (mag + ang, mag)
            }
        } else if ang > 0 {
            (mag, mag + ang)
        } else {
            (mag - ang, mag)
        }
    }

    /// lossless coupling は逆の square polar で元に戻る。float の版は整数の版と同じ値。
    #[test]
    fn lossless_coupling_round_trips() {
        for a in -40..=40 {
            for b in -40..=40 {
                let (mut m, mut n) = (a, b);
                lossless_coupling(&mut m, &mut n);
                assert_eq!(inverse_polar(m, n), (a, b), "({a}, {b}) → ({m}, {n})");
                // float の版は同じ値（(0, 0) は C と同じく (−0, −0) になる）
                let (mut mf, mut nf) = (a as f32, b as f32);
                lossless_couplingf(&mut mf, &mut nf);
                assert_eq!((mf as i32, nf as i32), (m, n), "float の版 ({a}, {b})");
            }
        }
        let (mut mf, mut nf) = (0.0_f32, 0.0_f32);
        lossless_couplingf(&mut mf, &mut nf);
        assert_eq!((mf.to_bits(), nf.to_bits()), (0x8000_0000, 0x8000_0000));
    }

    #[test]
    fn dipole_hypot_known_values() {
        let t = d2f(0.94);
        let check = |a: f32, b: f32, th: f32, want: f32| {
            assert_eq!(
                min_indemnity_dipole_hypot(a, b, th).to_bits(),
                want.to_bits(),
                "({a}, {b}, {th})"
            );
        };
        check(1.0, 1.0, 0.18, t + t);
        check(-1.0, -1.0, 0.18, -(t + t));
        check(2.0, -1.0, 0.5, 2.0 * t - t * 0.5);
        check(1.0, -2.0, 0.5, -(2.0 * t - t * 0.5));
        check(-2.0, 1.0, 0.5, -(2.0 * t - t * 0.5));
        check(-1.0, 2.0, 0.5, 2.0 * t - t * 0.5);
        check(0.0, 0.0, 0.5, 0.0);
    }

    /// 先頭の bthresh 個だけが大きい順に選ばれ、同じ値は先のものが勝つ（入れ替えで残りの並びが動く）。
    #[test]
    fn ssort_partial_selection() {
        let q = [0.1_f32, 0.3, 0.2, 0.3, 0.05];
        let mut sort = vec![0, 1, 2, 3, 4];
        ssort(5, 2, &mut sort, &q);
        assert_eq!(sort, vec![1, 3, 2, 0, 4]);
        let mut sort = vec![0, 1, 2, 3, 4];
        ssort(3, 9, &mut sort, &q);
        assert_eq!(sort, vec![1, 2, 0, 3, 4]);
        let mut sort = vec![0, 1, 2];
        ssort(3, -1, &mut sort, &q);
        assert_eq!(sort, vec![0, 1, 2]);
    }

    #[test]
    fn unitnorm_and_scales() {
        assert_eq!(unitnorm(3.5).to_bits(), 1.0_f32.to_bits());
        assert_eq!(unitnorm(-0.25).to_bits(), (-1.0_f32).to_bits());
        assert_eq!(unitnorm(-0.0).to_bits(), (-1.0_f32).to_bits());
        assert_eq!(unitnorm(0.0).to_bits(), 1.0_f32.to_bits());
        // オクターブ 0 は約 62.5 Hz（scales.h の注記の 63.5 Hz より少し低い）
        assert!((from_oc(0.0) - 62.5).abs() < 0.05);
        assert!(to_oc(from_oc(0.0)).abs() < 1e-5);
        assert!((to_oc(125.0) - 1.0).abs() < 0.01);
        assert!((from_oc(to_oc(1000.0)) - 1000.0).abs() < 0.5);
        // Bark：0 Hz は 0、16 kHz は約 24
        assert_eq!(to_bark_long(0).to_bits(), 0);
        assert!((23.0..25.0).contains(&to_bark_long(16_000)));
    }

    fn couple_look(normal_p: i32) -> VorbisLookPsy {
        let vi = VorbisInfoPsy {
            normal_p,
            ..template_psy()
        };
        vp_psy_init(&vi, &template_global(), 64, 44_100).unwrap()
    }

    fn stereo_map() -> VorbisInfoMapping0 {
        let mut m = VorbisInfoMapping0 {
            submaps: 1,
            coupling_steps: 1,
            ..VorbisInfoMapping0::default()
        };
        m.coupling_ang[0] = 1;
        m
    }

    /// noise normalization なし（normal_p = 0 ではすべて rint）・mono：`rint(mdct / floor)`。lowpassr より上は 0。
    #[test]
    fn couple_quantize_mono_rounds() {
        let look = couple_look(0);
        let mono = VorbisInfoMapping0 {
            submaps: 1,
            ..VorbisInfoMapping0::default()
        };
        let mdct = vec![
            (0..64)
                .map(|i| (i as f32 - 32.0) * 0.37)
                .collect::<Vec<f32>>(),
        ];
        let mut iwork = vec![vec![200_i32; 64]];
        let mut nonzero = vec![1];
        let mut nepeak = vec![vec![0.0_f32; 64 / 16]];
        vp_couple_quantize_normalize(
            7,
            &template_global(),
            &look,
            &mono,
            &mdct,
            &[vec![0.0; 64]],
            &mut nepeak,
            &mut iwork,
            &mut nonzero,
            64,
            1,
            48,
        )
        .unwrap();
        let floor = FLOOR1_FROMDB_LOOKUP[200];
        for j in 0..64 {
            let want = if j < 48 {
                rint_i32(mdct[0][j] / floor)
            } else {
                0
            };
            assert_eq!(iwork[0][j], want, "j = {j}");
        }
        assert_eq!(nonzero, vec![1]);
    }

    /// 両方 0 のチャンネルは 0 のまま。片方が 0 でなければ両方 0 でないにする。範囲の外の floor・チャンネルは誤り。
    #[test]
    fn couple_quantize_nonzero_and_errors() {
        let look = couple_look(1);
        let g = template_global();
        let map = stereo_map();
        let mdct = vec![vec![0.5_f32; 64], vec![-0.25_f32; 64]];
        let enpeak = vec![vec![0.0_f32; 64]; 2];
        let run = |iwork: &mut Vec<Vec<i32>>, nonzero: &mut Vec<i32>| {
            let mut nepeak = vec![vec![0.0_f32; 64 / 8]; 2];
            vp_couple_quantize_normalize(
                7,
                &g,
                &look,
                &map,
                &mdct,
                &enpeak,
                &mut nepeak,
                iwork,
                nonzero,
                64,
                2,
                64,
            )
        };
        let mut iwork = vec![vec![100; 64], vec![100; 64]];
        let mut nonzero = vec![0, 0];
        run(&mut iwork, &mut nonzero).unwrap();
        assert!(iwork.iter().all(|w| w.iter().all(|&x| x == 0)));
        assert_eq!(nonzero, vec![0, 0]);

        let mut iwork = vec![vec![100; 64], vec![0; 64]];
        let mut nonzero = vec![1, 0];
        run(&mut iwork, &mut nonzero).unwrap();
        assert_eq!(nonzero, vec![1, 1]);
        let first = iwork.clone();
        let mut iwork = vec![vec![100; 64], vec![0; 64]];
        let mut nonzero = vec![1, 0];
        run(&mut iwork, &mut nonzero).unwrap();
        assert_eq!(iwork, first, "決定的");

        let mut iwork = vec![vec![256; 64], vec![0; 64]];
        assert!(run(&mut iwork, &mut vec![1, 0]).is_err());
        let mut bad = stereo_map();
        bad.coupling_ang[0] = 0;
        let mut nepeak = vec![vec![0.0_f32; 8]; 2];
        let result = vp_couple_quantize_normalize(
            7,
            &g,
            &look,
            &bad,
            &mdct,
            &enpeak,
            &mut nepeak,
            &mut vec![vec![100; 64]; 2],
            &mut [1, 1],
            64,
            2,
            64,
        );
        assert!(result.is_err());
    }

    /// bark_noise_hybridmp：平らな入力はほぼ平ら、1 より小さい値は 1 に持ち上げてから当てはめる。
    #[test]
    fn bark_noise_flat() {
        let look = vp_psy_init(&template_psy(), &template_global(), 128, 44_100).unwrap();
        let f = vec![20.0_f32; 128];
        let mut noise = vec![0.0_f32; 128];
        bark_noise_hybridmp(128, &look.bark, &f, &mut noise, 0.0, 15);
        for (i, &x) in noise.iter().enumerate() {
            assert!((x - 20.0).abs() < 0.01, "noise[{i}] = {x}");
        }
        let f = vec![-500.0_f32; 128];
        bark_noise_hybridmp(128, &look.bark, &f, &mut noise, 0.0, -1);
        assert!(noise.iter().all(|&x| (x - 1.0).abs() < 1e-3));
    }

    /// seed_chase：山の値をつなぎ、山から後ろへ linesper ＋ 1 の幅まで伸ばす。
    #[test]
    fn seed_chase_spreads_peaks() {
        let mut seeds = vec![NEGINF; 32];
        seeds[4] = -10.0;
        seeds[20] = -5.0;
        seed_chase(&mut seeds, 8, 32);
        let v = |r: std::ops::RangeInclusive<usize>, want: f32| {
            assert!(
                seeds[r.clone()]
                    .iter()
                    .all(|x| x.to_bits() == want.to_bits()),
                "{r:?}"
            );
        };
        // C の積み上げを手でたどった値：山 −10 は 4 から 4 + 8 まで、山 −5 は 20 から 20 + 8 まで
        v(0..=3, NEGINF);
        v(4..=12, -10.0);
        v(13..=19, NEGINF);
        v(20..=28, -5.0);
        v(29..=31, NEGINF);
    }

    // ---- 照合（KURA_VORBIS_ORACLE があるときだけ C と照らす。なければ何もせずに通る） ----

    const RATES: [u32; 9] = [
        8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
    ];
    const QS: [i32; 5] = [-1, 0, 3, 5, 10];

    /// 照合のテストは重い（C を呼び、大きなダンプを読む）ので 1 つずつ回す（時間の上限を確かめるほかのテストに響かない
    /// ように）。照合用の C がなければすぐに返るので待たない。
    fn oracle_turn() -> std::sync::MutexGuard<'static, ()> {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        TURN.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// (a) `oracle_setup --look` の look.psy（psys 個）と、vp_psy_init の表がビットで一致する。
    fn check_look(rate: u32, ch: usize, q: i32) -> bool {
        let Some(js) = oracle::setup(rate, ch, q, true) else {
            return false;
        };
        let (_, looks) = looks_from_setup(&js, rate);
        let want = js["look"]["psy"].as_array().unwrap();
        assert_eq!(looks.len(), want.len());
        for (k, (look, w)) in looks.iter().zip(want).enumerate() {
            let what = |f: &str| format!("rate {rate} ch {ch} q {q} psy {k} {f}");
            assert_eq!(i64::from(look.n), w["n"].as_i64().unwrap(), "{}", what("n"));
            for (name, got) in [
                ("firstoc", look.firstoc),
                ("shiftoc", look.shiftoc),
                ("eighth_octave_lines", i64::from(look.eighth_octave_lines)),
                ("total_octave_lines", i64::from(look.total_octave_lines)),
                ("rate", look.rate),
                ("tonecomp_endp", i64::from(look.tonecomp_endp)),
                ("tonefix_end", i64::from(look.tonefix_end)),
            ] {
                assert_eq!(got, w[name].as_i64().unwrap(), "{}", what(name));
            }
            assert_f32s(&what("m_val"), &[look.m_val], &[json_f32(&w["m_val"])]);
            assert_f32s(
                &what("tonecomp_thres"),
                &[look.tonecomp_thres],
                &[json_f32(&w["tonecomp_thres"])],
            );
            assert_f32s(&what("ath"), &look.ath, &f32_list(&w["ath"]));
            let longs = |key: &str| -> Vec<i64> {
                w[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_i64().unwrap())
                    .collect()
            };
            assert_eq!(look.octave, longs("octave"), "{}", what("octave"));
            assert_eq!(look.bark, longs("bark"), "{}", what("bark"));
            let noff = w["noiseoffset"].as_array().unwrap();
            assert_eq!(noff.len(), P_NOISECURVES);
            for (j, v) in noff.iter().enumerate() {
                assert_f32s(
                    &what(&format!("noiseoffset[{j}]")),
                    &look.noiseoffset[j],
                    &f32_list(v),
                );
            }
            let tc = w["tonecurves"].as_array().unwrap();
            assert_eq!(tc.len(), P_BANDS);
            for (i, band) in tc.iter().enumerate() {
                let band = band.as_array().unwrap();
                assert_eq!(band.len(), P_LEVELS);
                for (m, curve) in band.iter().enumerate() {
                    assert_f32s(
                        &what(&format!("tonecurves[{i}][{m}]")),
                        &look.tonecurves[i][m],
                        &f32_list(curve),
                    );
                }
            }
        }
        true
    }

    fn check_looks(rates: &[u32]) {
        let mut n = 0;
        for &rate in rates {
            for q in QS {
                for ch in [1, 2] {
                    if !check_look(rate, ch, q) {
                        return;
                    }
                    n += 1;
                }
            }
        }
        eprintln!("look.psy：{n} 設定がビット一致（{rates:?}）");
    }

    #[test]
    fn oracle_look_tables_low_rates() {
        let _turn = oracle_turn();
        check_looks(&RATES[..3]);
    }

    #[test]
    fn oracle_look_tables_mid_rates() {
        let _turn = oracle_turn();
        check_looks(&RATES[3..6]);
    }

    #[test]
    fn oracle_look_tables_high_rates() {
        let _turn = oracle_turn();
        check_looks(&RATES[6..]);
        // 3・6・8 チャンネルも uncoupled のテンプレート
        for ch in [3, 6, 8] {
            if !check_look(44_100, ch, 5) {
                return;
            }
        }
    }

    /// 照合の結果の数（m2〜m9 は aoTuV の機能が働いた数：M2 の poste > 0 のブロック、M3 が tempmdct を変えた impulse の
    /// ブロック、M5 の lowcomp > 0 のブロック、M7 の ntfix がノイズのマスクを変えたブロック、M8 の npeak > 0 の partition、
    /// M9 の epeak ≠ 0 の要素。ブロックはチャンネルごとに数える）。
    #[derive(Debug, Default)]
    struct Stats {
        blocks: usize,
        short: usize,
        long: usize,
        channel_blocks: usize,
        nan: usize,
        point: usize,
        m2: usize,
        m3: usize,
        m5: usize,
        m7: usize,
        m8: usize,
        m9: usize,
    }

    fn rec<'a>(dump: &'a Dump, b: usize, tag: &str, ch: u16) -> &'a oracle::Record {
        dump.get(b, tag, ch)
            .unwrap_or_else(|| panic!("ブロック {b} に {tag} ch {ch} がない"))
    }

    /// (b)・(c) oracle::signal の入力で [`check_input`]。
    fn check_blocks(
        signal: Signal,
        rate: u32,
        ch: usize,
        q: i32,
        frames: usize,
        stats: &mut Stats,
    ) -> bool {
        let input = oracle::signal(signal, rate, ch, frames, 7);
        check_input(&format!("{signal:?}"), &input, rate, q, stats)
    }

    /// (b)・(c) 1 つの入力を照合用のエンコーダで書いてダンプ（レベル 2）を読み、ブロックごとに
    /// NOIS・EPEK・NPEK・TONE・MASK・MDC1 と RQPR・RQPO・NZC を照らす。
    fn check_input(label: &str, input: &[Vec<f32>], rate: u32, q: i32, stats: &mut Stats) -> bool {
        let ch = input.len();
        let Some((_, dump)) = oracle::encode(input, rate, q, Some(2)) else {
            return false;
        };
        let dump = dump.unwrap();
        let js = oracle::setup(rate, ch, q, false).unwrap();
        let (g, looks) = looks_from_setup(&js, rate);
        let maps: Vec<VorbisInfoMapping0> = js["maps"]
            .as_array()
            .unwrap()
            .iter()
            .map(mapping_from_json)
            .collect();
        let floor_n: Vec<i32> = js["floors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| int(&f["n"]))
            .collect();
        let block_lowpassr = i32_list(&js["block_lowpassr"]);
        let chu16 = u16::try_from(ch).unwrap();
        // aoTuV のブロックをまたぐ状態（C の private_state。ブロックの順に進める）
        let mut st = super::super::mapping0::AotuvState::new(ch);
        // M7 が働いたかを数えるための、tonefix_end を 0 にした look（ntfix が何もしない）
        let no_m7: Vec<VorbisLookPsy> = looks
            .iter()
            .map(|l| VorbisLookPsy {
                tonefix_end: 0,
                ..l.clone()
            })
            .collect();

        for b in 0..dump.blocks.len() {
            let info = dump.block_info(b);
            let w = usize::try_from(info[1]).unwrap();
            let nw = i32::try_from(info[3]).unwrap();
            let blocktype = usize::try_from(info[4]).unwrap();
            let block_mode = i32::try_from(info[5]).unwrap();
            let window = st.window;
            let look = &looks[blocktype + if w != 0 { 2 } else { 0 }];
            let n = usize::try_from(look.n).unwrap();
            assert_eq!(usize::try_from(info[8]).unwrap() / 2, n);
            let partition = usize::try_from(partition_of(&look.vi)).unwrap();
            let case = |what: &str, c: u16| {
                format!(
                    "{label} rate {rate} ch {ch} q {q} block {b} (W {w} type {blocktype}) {what} ch {c}"
                )
            };
            stats.blocks += 1;
            if w == 0 {
                stats.short += 1;
            } else {
                stats.long += 1;
            }
            let ampg = rec(&dump, b, "AMPG", 0xFFFF).f32s()[0];
            // M3 で書き換えたあとの npeak（量子化が読む。C のダンプの NPEK は M3 の前）
            let mut npeak_after = Vec::with_capacity(ch);

            for c in 0..chu16 {
                stats.channel_blocks += 1;
                let k = usize::from(c);
                let lmdc = rec(&dump, b, "LMDC", c).f32s();

                // M2：_postnoise_detection（窓を掛ける前の PCM0 から）
                let pcm0 = rec(&dump, b, "PCM0", c).f32s();
                let poste =
                    postnoise_detection(&pcm0, 2 * look.n, block_mode, window.lw_block_mode);
                if poste > 0.0 {
                    stats.m2 += 1;
                }
                // M5：lb_loudnoise_fix（前のブロックの値から）
                st.lowcomp[k] =
                    lb_loudnoise_fix(look, st.lowcomp[k], &lmdc, block_mode, window.lw_block_mode);
                let lcmp = rec(&dump, b, "LCMP", c).f32s();
                assert_f32s(&case("LCMP", c), &st.lowcomp[k..=k], &lcmp);
                if st.lowcomp[k] > 0.0 {
                    stats.m5 += 1;
                }

                // (b) _vp_noisemask
                let mut noise = vec![0.0_f32; n];
                let mut epeak = vec![1.0_f32; n];
                let mut npeak = vec![1.0_f32; n.div_ceil(partition)];
                vp_noisemask(
                    look,
                    st.lowcomp[k],
                    &lmdc,
                    &st.lastmdct[k],
                    &mut epeak,
                    &mut npeak,
                    &mut noise,
                    poste,
                    block_mode,
                );
                let nois = rec(&dump, b, "NOIS", c).f32s();
                stats.nan += count_nan(&nois);
                assert_f32s(&case("NOIS", c), &noise, &nois);
                {
                    let mut without = vec![0.0_f32; n];
                    let mut e2 = vec![0.0_f32; n];
                    let mut n2 = vec![0.0_f32; n.div_ceil(partition)];
                    vp_noisemask(
                        &no_m7[blocktype + if w != 0 { 2 } else { 0 }],
                        st.lowcomp[k],
                        &lmdc,
                        &st.lastmdct[k],
                        &mut e2,
                        &mut n2,
                        &mut without,
                        poste,
                        block_mode,
                    );
                    if without
                        .iter()
                        .zip(&noise)
                        .any(|(a, b)| a.to_bits() != b.to_bits())
                    {
                        stats.m7 += 1;
                    }
                }
                let epek = rec(&dump, b, "EPEK", c).f32s();
                assert_f32s(&case("EPEK", c), &epeak, &epek);
                assert_f32s(&case("NPEK", c), &npeak, &rec(&dump, b, "NPEK", c).f32s());
                stats.m9 += epek.iter().filter(|&&x| x != 0.0).count();
                stats.m8 += npeak.iter().filter(|&&x| x > 0.0).count();

                // (b) _vp_tonemask
                let lfft = rec(&dump, b, "LFFT", c).f32s();
                let ampl = rec(&dump, b, "AMPL", c).f32s()[0];
                let mut tone = vec![0.0_f32; n];
                vp_tonemask(look, &lfft, &mut tone, ampg, ampl);
                let tone_c = rec(&dump, b, "TONE", c).f32s();
                assert_f32s(&case("TONE", c), &tone, &tone_c);

                // (b) _vp_offset_and_mix（M1 で MDCT を、M3 で npeak・tempmdct を、最後に lastmdct を書き換える）
                let mut mask = vec![0.0_f32; n];
                let mut mdct = rec(&dump, b, "MDCT", c).f32s();
                let temp_before = st.tempmdct[k].clone();
                vp_offset_and_mix(
                    look,
                    &nois,
                    &tone_c,
                    1,
                    0,
                    &mut mask,
                    &mut mdct,
                    &lmdc,
                    &mut st.lastmdct[k],
                    &mut st.tempmdct[k],
                    st.lowcomp[k],
                    &mut npeak,
                    floor_n[w],
                    block_mode,
                    nw,
                    window.lw_block_mode,
                    window.lw_no,
                    window.impadnum,
                );
                if block_mode == 0 && st.tempmdct[k] != temp_before {
                    stats.m3 += 1;
                }
                let mask_c = rec(&dump, b, "MASK", c).f32s();
                let mdc1 = rec(&dump, b, "MDC1", c).f32s();
                stats.nan += count_nan(&mask_c) + count_nan(&mdc1);
                assert_f32s(&case("MASK", c), &mask, &mask_c);
                assert_f32s(&case("MDC1", c), &mdct, &mdc1);
                npeak_after.push(npeak);
            }
            st.window = window.next(block_mode);

            // (c) _vp_couple_quantize_normalize
            let per_ch_f = |tag: &str| -> Vec<Vec<f32>> {
                (0..chu16).map(|c| rec(&dump, b, tag, c).f32s()).collect()
            };
            let per_ch_i = |tag: &str| -> Vec<Vec<i32>> {
                (0..chu16).map(|c| rec(&dump, b, tag, c).i32s()).collect()
            };
            let mdc1 = per_ch_f("MDC1");
            let enpeak = per_ch_f("EPEK");
            let mut nepeak = npeak_after;
            let mut iwork = per_ch_i("ILOG");
            let mut nonzero: Vec<i32> = per_ch_i("NZ").into_iter().map(|v| v[0]).collect();
            // mapping0.c:778–781 の residue の lowpass
            let normal_partition = look.vi.normal_partition;
            let mut lowpassr = block_lowpassr[w];
            if lowpassr % normal_partition != 0 {
                lowpassr = (lowpassr / normal_partition + 1) * normal_partition;
            }
            let blob = vorbis_tables::PACKETBLOBS / 2;
            let mut pre = vec![vec![0_i32; n]; ch];
            couple_quantize_normalize_impl(
                i32::try_from(blob).unwrap(),
                &g,
                look,
                &maps[w],
                &mdc1,
                &enpeak,
                &mut nepeak,
                &mut iwork,
                &mut nonzero,
                g.sliding_lowpass[w][blob],
                i32::try_from(ch).unwrap(),
                lowpassr,
                Some(&mut pre),
            )
            .unwrap();
            for c in 0..chu16 {
                let k = usize::from(c);
                assert_i32s(&case("RQPR", c), &pre[k], &rec(&dump, b, "RQPR", c).i32s());
                let rqpo = rec(&dump, b, "RQPO", c).i32s();
                assert_i32s(&case("RQPO", c), &iwork[k], &rqpo);
                if k > 0 && maps[w].coupling_steps > 0 {
                    // point stereo で angle が 0 に落ちた要素を数える
                    stats.point += rqpo
                        .iter()
                        .zip(&pre[k])
                        .filter(|(o, p)| **o == 0 && **p != 0)
                        .count();
                }
                assert_i32s(
                    &case("NZC", c),
                    &nonzero[k..=k],
                    &rec(&dump, b, "NZC", c).i32s(),
                );
            }
        }
        true
    }

    /// 1 つのレートの照合：Music × 1・2 チャンネル × q −1・0・3・5・10、ほかの入力と 3・6・8 チャンネルを少し。
    fn check_rate(rate: u32) {
        let _turn = oracle_turn();
        let r = usize::try_from(rate).unwrap();
        let mut stats = Stats::default();
        let mut cases = Vec::new();
        for q in QS {
            for ch in [1, 2] {
                cases.push((Signal::Music, ch, q, r * 6 / 10));
            }
        }
        cases.push((Signal::Impulses, 2, 5, r / 2));
        cases.push((Signal::Impulses, 1, -1, r / 2));
        cases.push((Signal::Noise, 1, 0, r / 4));
        cases.push((Signal::Noise, 2, 10, r / 4));
        cases.push((Signal::Sine, 2, 3, r / 4));
        cases.push((Signal::Silence, 1, -1, r / 4));
        cases.push((Signal::Silence, 2, 5, r / 8));
        cases.push((Signal::Bursts, 2, 0, r));
        cases.push((Signal::Bursts, 1, -1, r));
        let extra: &[usize] = match rate {
            8_000 | 44_100 | 192_000 => &[3, 6, 8],
            16_000 | 48_000 => &[6],
            _ => &[],
        };
        for &ch in extra {
            cases.push((Signal::Music, ch, 3, r * 6 / 10));
        }
        let n = cases.len();
        for (signal, ch, q, frames) in cases {
            if !check_blocks(signal, rate, ch, q, frames, &mut stats) {
                return;
            }
        }
        eprintln!("rate {rate}：{n} 入力 {stats:?} がビット一致");
        assert!(stats.blocks > 0);
        if rate >= 16_000 {
            assert!(
                stats.short > 0 && stats.long > 0,
                "短い窓と長い窓の両方に届く"
            );
        }
    }

    /// 照合用のエンコーダの確かめの入力（`test/in/` の planar の f32。`crates/vorbis-tables/oracle/build.py` が作る）。
    /// 照合用のエンコーダがなければ None。あるのに入力がなければ止まる（何もせずに通らないように）。
    fn oracle_input(name: &str, ch: usize) -> Option<Vec<Vec<f32>>> {
        let path = oracle::root()?.join("test/in").join(name);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{}：{e}（build.py で作る）", path.display()));
        let samples: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let frames = samples.len() / ch;
        Some(samples.chunks_exact(frames).map(<[f32]>::to_vec).collect())
    }

    /// NaN が途中に出る入力（照合用の README §8.3：`bark_noise_hybridmp` の 0/0 が NOIS に出て、M1 で MDC1 に移り、
    /// 量子化で `(int)rint(NaN)` に届く）。NaN は符号を問わず同じとみなして照らす。
    #[test]
    fn oracle_blocks_nan_inputs() {
        let _turn = oracle_turn();
        let mut stats = Stats::default();
        for (name, rate, ch, q) in [
            ("synth_32000_1.f32", 32_000, 1, 0),
            ("synth_32000_2.f32", 32_000, 2, 0),
            ("synth_192000_1.f32", 192_000, 1, -1),
        ] {
            let Some(input) = oracle_input(name, ch) else {
                return;
            };
            assert!(check_input(name, &input, rate, q, &mut stats));
        }
        eprintln!("NaN の入力：{stats:?} がビット一致");
        assert!(stats.nan > 0, "NaN に届いていない");
    }

    #[test]
    fn oracle_blocks_8000() {
        check_rate(8_000);
    }

    #[test]
    fn oracle_blocks_11025() {
        check_rate(11_025);
    }

    #[test]
    fn oracle_blocks_16000() {
        check_rate(16_000);
    }

    #[test]
    fn oracle_blocks_22050() {
        check_rate(22_050);
    }

    #[test]
    fn oracle_blocks_32000() {
        check_rate(32_000);
    }

    #[test]
    fn oracle_blocks_44100() {
        check_rate(44_100);
    }

    #[test]
    fn oracle_blocks_48000() {
        check_rate(48_000);
    }

    #[test]
    fn oracle_blocks_96000() {
        check_rate(96_000);
    }

    #[test]
    fn oracle_blocks_192000() {
        check_rate(192_000);
    }
}
