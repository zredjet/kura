//! 品質 → テンプレート → `codec_setup_info`（`lib/vorbisenc.c`）。
//!
//! # 移す C（VBR の経路だけ）
//!
//! | 関数 | 行 | 備考 |
//! |---|---|---|
//! | `vorbis_encode_floor_setup` | 192–228 | `in + x[is]` を写し、`class_book`・`class_subbook`（≥ 0 のもの）に `ci->books` を足す。`books[x[is]][0..=maxbook]` を `book_param` に足す（重複を除かない） |
//! | `vorbis_encode_global_psych_setup` | 230–255 | `in + (int)x[is]` を写し、`x[is]` と `x[is+1]` を補間した添字で **先頭 4 帯域だけ** preecho・postecho の閾値を補間（249。残り 8 帯域は整数の段の値。上流の癖のまま）。`ds == 0 && is > 0` なら `is--; ds = 1`。`ampmax_att_per_sec = hi.amplitude_track_dBpersec` |
//! | `vorbis_encode_global_stereo` | 257–304 | `hi.stereo_point_setting` は **float** に入れてから使う（260）。VBR（`managed == 0`）は列 `PACKETBLOBS/2` の kHz を補間（284・291）し、`kHz*1000./rate*blocksizes[k]` を int に。`p == NULL`（stereo_modes なし）なら sliding_lowpass = blocksizes |
//! | `vorbis_encode_psyset_setup` | 306–334 | `_psy_info_template` を写し、`blockflag = block >> 1`、`noise_normalize_p` なら normal_* を整数の段で |
//! | `vorbis_encode_tonemask_setup` | 336–358 | tone_masteratt・centerboost・decay・max_curve_dB・toneatt を補間（double で計算して float に） |
//! | `vorbis_encode_compand_setup` | 361–415 | `noisecompand` の補間（x で添字を写してから。382–385 の `ds == 0 && is > 0` の扱い）。`noisecompand_high`（388–412）は M5 だけが、`flacint`（376）はどこも読まないが、照合用のエンコーダの設定（`oracle_setup` の psy の全項目）とビット単位で比べられるように C のとおり計算する（添字は範囲の中：kura の範囲で選ばれる 16 のテンプレート × q −1〜10 をテストで確かめた） |
//! | `vorbis_encode_peak_setup` | 417–427 | tone_abs_limit |
//! | `vorbis_encode_noisebias_setup` | 429–459 | noisemaxsupp・noisewindow*・noiseoff の補間、`min = noiseoff[j][0] + 6`（float）で下を止める、impulse に `impulse_noisetune` |
//! | `vorbis_encode_ath_setup` | 461–468 | |
//! | `book_dup_or_new` | 471–477 | 同じ book（同じ `BookId`）なら前の番号 |
//! | `vorbis_encode_blocksize_setup` | 479–490 | 整数の段 |
//! | `vorbis_encode_residue_setup` | 493–627 | residue を写し、grouping・secondstages・groupbook・booklist を VBR の側（`books_base`・`book_aux`。`managed` の枝 513–531 は通らない）で埋める。lowpass：floor の `n = freq/nyq*blocksize`（int に）、residue の `end`（type 2 は ch 倍。`+ .9` で切り上げ、`grouping` の倍数、`block_lowpassr`）。LFE（limit_type 2）は使わないが写してよい |
//! | `vorbis_encode_map_n_res_setup` | 630–657 | mode と mapping を 2 つ（短・長。ブロックの大きさが同じなら 1 つ）、submap ごとに residue |
//! | `setting_to_approx_bitrate` | 659–672 | 識別ヘッダの bitrate_nominal（`(r[is]*(1.-ds)+r[is+1]*ds)*ch` を long に） |
//! | `get_setup_template` | 674–713 | `vorbis_tables::SETUP_LIST` を先頭から（coupling_restriction が −1 か ch、レートの範囲、q が表の範囲）。`float low = map[j]; float high = map[j+1]; float del = (req-low)/(high-low);` は float に丸めてから、`*base_setting = j + del` は **float の足し算**を double に入れる（703） |
//! | `vorbis_encode_setup_init` | 722–905 | 上を順に呼ぶ。`ath_floating_dB` を −200〜−80、`amplitude_track_dBpersec` を −99999〜0 に止める。`i0`・`singleblock` の扱い。最後に bitrate の欄（VBR は nominal だけ） |
//! | `vorbis_encode_setup_setting` | 907–950 | `impulse_block_p = 1`、`noise_normalize_p = 1`、lowpass・ATH の補間、`amplitude_track_dBpersec = -6`、各 setting = base_setting。**pre_amplitude（932–934）は移さない** |
//! | `vorbis_encode_setup_vbr` | 952–975 | `quality += .0000001`（float に足して float に）、`>= 1.` なら `.9999`、`managed = 0`、`coupling_p = 1` |
//! | `vorbis_encode_init_vbr` | 977–995 | `setup_vbr` と `setup_init` |
//!
//! 補間はすべて C の式のまま（`a[is]*(1.-ds)+a[is+1]*ds`。`int` の表の値は double にしてから。結果を float・int に入れるときの
//! 丸め・切り捨ても C のとおり）。
//!
//! # 移さないもの
//!
//! ビットレートの管理（`vorbis_encode_setup_managed`・`vorbis_encode_init`・`vorbis_encode_ctl`。997–1274）、
//! `vorbis_encode_global_stereo` の managed の枝（270–282）、residue の managed の book（513–531）、前増幅。
//!
//! # 型の対応
//!
//! - テンプレートの型（`ve_setup_data_template` ほか）は `vorbis_tables`（`VeSetupDataTemplate`・`VorbisResidueTemplate` など）。
//! - `highlevel_encode_setup`（highlevel.h:24–59）→ [`HighlevelEncodeSetup`]（VBR に要る欄だけ。bitrate_*・managed・
//!   req 以外のビットレートの欄・`pre_amplitude` は持たない）。`highlevel_byblocktype` → [`HighlevelByblocktype`]。
//! - C の `ci->psy_param[4]`・`mode_param[64]` などのポインタの配列は `Vec`（[`super::info::CodecSetupInfo`]）。C が
//!   添字 `i` に書く所は、`Vec` が短ければ 0 の値（C の calloc）で伸ばしてから書く（[`put`]）。
//!
//! # 範囲の外
//!
//! C は表の添字を確かめない。移植は添字をすべて `get` で引き、範囲の外は `EngineError::Internal`（表が壊れている）にする。
//! `vorbis_tables` のテンプレートで、kura の範囲（チャンネル 1〜8・レート 8〜192 kHz・q −1〜10）では範囲の外に出ないことを
//! テストで確かめた（`every_supported_setting_builds`）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use vorbis_tables::{
    Att3, BookId, Compandblock, MODE_TEMPLATE, NOISE_COMPAND_LEVELS, Noise3, Noiseguard, P_BANDS,
    P_NOISECURVES, PACKETBLOBS, PSY_INFO_TEMPLATE, SETUP_LIST, VeSetupDataTemplate,
    VorbisInfoFloor1, VorbisInfoPsy, VorbisInfoPsyGlobal, VorbisMappingTemplate,
    VorbisResidueTemplate, VpAdjblock,
};

use super::info::{CodecSetupInfo, VorbisInfo, vorbis_info_init};
use crate::error::EngineError;

/// `highlevel_byblocktype`（highlevel.h:17–22）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct HighlevelByblocktype {
    pub tone_mask_setting: f64,
    pub tone_peaklimit_setting: f64,
    pub noise_bias_setting: f64,
    pub noise_compand_setting: f64,
}

/// `highlevel_encode_setup`（highlevel.h:24–59）の VBR に要る欄。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct HighlevelEncodeSetup {
    /// 選んだテンプレート（C の `const void *setup`）。
    pub setup: Option<&'static VeSetupDataTemplate>,
    pub base_setting: f64,
    pub impulse_noisetune: f64,
    /// 品質（C の `float req`）。
    pub req: f32,
    /// 常に 0（VBR）。
    pub managed: i32,
    pub impulse_block_p: i32,
    pub noise_normalize_p: i32,
    pub coupling_p: i32,
    pub stereo_point_setting: f64,
    /// C の `lowpass_kHz`。
    pub lowpass_khz: f64,
    pub lowpass_altered: i32,
    /// C の `ath_floating_dB`。
    pub ath_floating_db: f64,
    /// C の `ath_absolute_dB`。
    pub ath_absolute_db: f64,
    /// C の `amplitude_track_dBpersec`。
    pub amplitude_track_dbpersec: f64,
    pub trigger_setting: f64,
    /// highlevel.h:57 の注記は「padding・impulse・transition・long」だが、vorbisenc.c は psy の番号（0 impulse・1 padding・
    /// 2 transition・3 long）で引く。VBR ではどれも base_setting。
    pub block: [HighlevelByblocktype; 4],
}

/// LFE の lowpass（vorbisenc.c:492 の `LFE_FRQLIMIT`）。
const LFE_FRQLIMIT: i32 = 250;

/// 表の添字が範囲の外（C では範囲の外の読み取り。テンプレートが正しければ起きない）。
fn out_of_range(what: &str) -> EngineError {
    EngineError::Internal(format!("vorbisenc：{what} の添字が範囲の外です"))
}

/// `s[i]`（C の添字の読み取り。`i` は C の `int`）。範囲の外は `Internal`。
fn at<T: Copy>(s: &[T], i: i32, what: &str) -> Result<T, EngineError> {
    usize::try_from(i)
        .ok()
        .and_then(|i| s.get(i))
        .copied()
        .ok_or_else(|| out_of_range(what))
}

/// `&mut s[i]`。範囲の外は `Internal`。
fn at_mut<'a, T>(s: &'a mut [T], i: i32, what: &str) -> Result<&'a mut T, EngineError> {
    usize::try_from(i)
        .ok()
        .and_then(|i| s.get_mut(i))
        .ok_or_else(|| out_of_range(what))
}

/// C のポインタの配列（`ci->mode_param[i] = …` など）に書く：`v` が短ければ既定の値（C の calloc の 0）で伸ばす。
pub(crate) fn put<T: Default>(v: &mut Vec<T>, i: usize, x: T) {
    if v.len() <= i {
        v.resize_with(i + 1, T::default);
    }
    if let Some(slot) = v.get_mut(i) {
        *slot = x;
    }
}

/// C の補間 `a*(1.-ds)+b*ds`（double。FMA にしない）。
fn interp(a: f64, b: f64, ds: f64) -> f64 {
    a * (1. - ds) + b * ds
}

/// `ci->psy_param[block]`（`vorbis_encode_psyset_setup` が作ったもの）。
fn psy_mut(vi: &mut VorbisInfo, block: usize) -> Result<&mut VorbisInfoPsy, EngineError> {
    vi.codec_setup
        .psy_param
        .get_mut(block)
        .ok_or_else(|| out_of_range("psy_param"))
}

/// `vorbis_encode_floor_setup`（vorbisenc.c:192–228）。C の `int s`（呼ぶ側の double を int に切り捨てたもの）。
fn vorbis_encode_floor_setup(
    vi: &mut VorbisInfo,
    s: f64,
    books: &'static [&'static [Option<BookId>]],
    floors: &'static [VorbisInfoFloor1],
    x: &'static [i32],
) -> Result<(), EngineError> {
    let is = s as i32;
    let ci = &mut vi.codec_setup;
    let xis = at(x, is, "floor_mapping_list")?;
    let mut f = at(floors, xis, "floor_params")?;

    // books
    let partitions = f.partitions;
    let mut maxclass = -1;
    let mut maxbook = -1;
    for i in 0..partitions {
        let pc = at(&f.partitionclass, i, "partitionclass")?;
        if pc > maxclass {
            maxclass = pc;
        }
    }
    for i in 0..=maxclass {
        let cb = at_mut(&mut f.class_book, i, "class_book")?;
        if *cb > maxbook {
            maxbook = *cb;
        }
        *cb += ci.books;
        let subs = at(&f.class_subs, i, "class_subs")?;
        let subbooks = at_mut(&mut f.class_subbook, i, "class_subbook")?;
        let n = 1i32
            .checked_shl(u32::try_from(subs).map_err(|_| out_of_range("class_subs"))?)
            .ok_or_else(|| out_of_range("class_subs"))?;
        for k in 0..n {
            let sb = at_mut(subbooks, k, "class_subbook")?;
            if *sb > maxbook {
                maxbook = *sb;
            }
            if *sb >= 0 {
                *sb += ci.books;
            }
        }
    }

    let list = at(books, xis, "floor_books")?;
    for i in 0..=maxbook {
        let book = at(list, i, "floor_books")?
            .ok_or_else(|| EngineError::Internal("vorbisenc：floor の book がありません".into()))?;
        push_book(ci, book)?;
    }

    // for now, we're only using floor 1
    let n = ci.floors as usize;
    put(&mut ci.floor_type, n, 1);
    put(&mut ci.floor_param, n, f);
    ci.floors += 1;
    Ok(())
}

/// `vorbis_encode_global_psych_setup`（vorbisenc.c:230–255）。
fn vorbis_encode_global_psych_setup(
    vi: &mut VorbisInfo,
    s: f64,
    gparams: &'static [VorbisInfoPsyGlobal],
    x: &'static [f64],
) -> Result<(), EngineError> {
    let mut is = s as i32;
    let mut ds = s - f64::from(is);
    let ci = &mut vi.codec_setup;

    let mut g = at(
        gparams,
        at(x, is, "global_mapping")? as i32,
        "global_params",
    )?;

    ds = interp(
        at(x, is, "global_mapping")?,
        at(x, is + 1, "global_mapping")?,
        ds,
    );
    is = ds as i32;
    ds -= f64::from(is);
    if ds == 0. && is > 0 {
        is -= 1;
        ds = 1.;
    }

    // interpolate the trigger threshholds
    let lo = at(gparams, is, "global_params")?;
    let hi = at(gparams, is + 1, "global_params")?;
    for i in 0..4 {
        g.preecho_thresh[i] = interp(
            f64::from(lo.preecho_thresh[i]),
            f64::from(hi.preecho_thresh[i]),
            ds,
        ) as f32;
        g.postecho_thresh[i] = interp(
            f64::from(lo.postecho_thresh[i]),
            f64::from(hi.postecho_thresh[i]),
            ds,
        ) as f32;
    }
    g.ampmax_att_per_sec = ci.hi.amplitude_track_dbpersec as f32;
    ci.psy_g_param = g;
    Ok(())
}

/// `vorbis_encode_global_stereo`（vorbisenc.c:257–304。VBR の枝）。
fn vorbis_encode_global_stereo(
    vi: &mut VorbisInfo,
    p: Option<&'static [vorbis_tables::AdjStereo]>,
) -> Result<(), EngineError> {
    let rate = vi.rate as f64;
    let ci = &mut vi.codec_setup;
    // float s = hi->stereo_point_setting; int is = s; double ds = s - is;（float の引き算）
    let s = ci.hi.stereo_point_setting as f32;
    let is = s as i32;
    let ds = f64::from(s - is as f32);
    let bs0 = ci.blocksizes[0] as f64;
    let bs1 = ci.blocksizes[1] as f64;
    let g = &mut ci.psy_g_param;

    if let Some(p) = p {
        let lo = at(p, is, "stereo_modes")?;
        let hi = at(p, is + 1, "stereo_modes")?;
        g.coupling_prepointamp = lo.pre;
        g.coupling_postpointamp = lo.post;

        // VBR（managed == 0）：真ん中の列だけ
        const B: usize = PACKETBLOBS / 2;
        let khz = interp(f64::from(lo.khz[B]), f64::from(hi.khz[B]), ds) as f32;
        for i in 0..PACKETBLOBS {
            g.coupling_pointlimit[0][i] = (f64::from(khz) * 1000. / rate * bs0) as i32;
            g.coupling_pointlimit[1][i] = (f64::from(khz) * 1000. / rate * bs1) as i32;
            g.coupling_pkhz[i] = khz as i32;
        }

        let khz = interp(
            f64::from(lo.lowpass_khz[B]),
            f64::from(hi.lowpass_khz[B]),
            ds,
        ) as f32;
        for i in 0..PACKETBLOBS {
            g.sliding_lowpass[0][i] = (f64::from(khz) * 1000. / rate * bs0) as i32;
            g.sliding_lowpass[1][i] = (f64::from(khz) * 1000. / rate * bs1) as i32;
        }
    } else {
        for i in 0..PACKETBLOBS {
            g.sliding_lowpass[0][i] = ci.blocksizes[0] as i32;
            g.sliding_lowpass[1][i] = ci.blocksizes[1] as i32;
        }
    }
    Ok(())
}

/// `vorbis_encode_psyset_setup`（vorbisenc.c:306–334）。
fn vorbis_encode_psyset_setup(
    vi: &mut VorbisInfo,
    s: f64,
    nn_start: &'static [i32],
    nn_partition: &'static [i32],
    nn_thresh: &'static [f64],
    block: usize,
) -> Result<(), EngineError> {
    let ci = &mut vi.codec_setup;
    let is = s as i32;

    if block as i32 >= ci.psys {
        ci.psys = block as i32 + 1;
    }

    let mut p = PSY_INFO_TEMPLATE;
    p.blockflag = (block >> 1) as i32;

    if ci.hi.noise_normalize_p != 0 {
        p.normal_p = 1;
        p.normal_start = at(nn_start, is, "psy_noise_normal_start")?;
        p.normal_partition = at(nn_partition, is, "psy_noise_normal_partition")?;
        p.normal_thresh = at(nn_thresh, is, "psy_noise_normal_thresh")?;
    }
    put(&mut ci.psy_param, block, p);
    Ok(())
}

/// `vorbis_encode_tonemask_setup`（vorbisenc.c:336–358）。
fn vorbis_encode_tonemask_setup(
    vi: &mut VorbisInfo,
    s: f64,
    block: usize,
    att: &'static [Att3],
    max: &'static [i32],
    adj: &'static [VpAdjblock],
) -> Result<(), EngineError> {
    let is = s as i32;
    let ds = s - f64::from(is);
    let (a0, a1) = (
        at(att, is, "psy_tone_masteratt")?,
        at(att, is + 1, "psy_tone_masteratt")?,
    );
    let (m0, m1) = (
        at(max, is, "psy_tone_0dB")?,
        at(max, is + 1, "psy_tone_0dB")?,
    );
    let (j0, j1) = (
        at(adj, is, "psy_tone_adj")?,
        at(adj, is + 1, "psy_tone_adj")?,
    );
    let p = psy_mut(vi, block)?;

    // 0 and 2 are only used by bitmanagement, but there's no harm to always filling the values in here
    for k in 0..P_NOISECURVES {
        p.tone_masteratt[k] = interp(f64::from(a0.att[k]), f64::from(a1.att[k]), ds) as f32;
    }
    p.tone_centerboost = interp(f64::from(a0.boost), f64::from(a1.boost), ds) as f32;
    p.tone_decay = interp(f64::from(a0.decay), f64::from(a1.decay), ds) as f32;

    p.max_curve_db = interp(f64::from(m0), f64::from(m1), ds) as f32;

    for i in 0..P_BANDS {
        p.toneatt[i] = interp(f64::from(j0.block[i]), f64::from(j1.block[i]), ds) as f32;
    }
    Ok(())
}

/// `vorbis_encode_compand_setup`（vorbisenc.c:361–415）。`noisecompand_high`（M5 だけが読む）と `flacint`（読まれない）も
/// C のとおり埋める（照合用のエンコーダの設定と全項目で比べるため）。
fn vorbis_encode_compand_setup(
    vi: &mut VorbisInfo,
    s: f64,
    block: usize,
    compand: &'static [Compandblock],
    x: &'static [f64],
    mappings: i32,
) -> Result<(), EngineError> {
    let mut is = s as i32;
    let mut ds = s - f64::from(is);
    let mut hcm_stop = 5; // high compander limit

    // check mapping limit
    if hcm_stop > mappings {
        hcm_stop = mappings;
    }

    let flacint = ds as f32;

    // interpolate the compander mapping
    ds = interp(
        at(x, is, "compand_mapping")?,
        at(x, is + 1, "compand_mapping")?,
        ds,
    );
    is = ds as i32;
    ds -= f64::from(is);
    if ds == 0. && is > 0 {
        is -= 1;
        ds = 1.;
    }

    // high compander setup
    let mut ishcm = is;
    let mut dshcm = ds + 0.3;
    if dshcm > 1.0 {
        ishcm += 1;
        dshcm -= 1.;
    }
    let xstop = at(x, hcm_stop, "compand_mapping")?;
    if xstop < f64::from(ishcm) + dshcm {
        ishcm = xstop as i32;
        dshcm = xstop - f64::from(ishcm);
        if f64::from(ishcm) + dshcm < f64::from(is) + ds {
            ishcm = is;
            dshcm = ds;
        }
    }
    if dshcm == 0. && ishcm > 0 {
        // the same
        ishcm -= 1;
        dshcm = 1.;
    }

    let (c0, c1) = (
        at(compand, is, "psy_noise_compand")?,
        at(compand, is + 1, "psy_noise_compand")?,
    );
    let (h0, h1) = (
        at(compand, ishcm, "psy_noise_compand")?,
        at(compand, ishcm + 1, "psy_noise_compand")?,
    );
    let p = psy_mut(vi, block)?;
    // the place was borrowed...
    p.flacint = flacint;
    // interpolate the compander settings
    for i in 0..NOISE_COMPAND_LEVELS {
        p.noisecompand[i] = interp(f64::from(c0.data[i]), f64::from(c1.data[i]), ds) as f32;
    }
    // interpolate the high compander settings
    for i in 0..NOISE_COMPAND_LEVELS {
        p.noisecompand_high[i] = interp(f64::from(h0.data[i]), f64::from(h1.data[i]), dshcm) as f32;
    }
    Ok(())
}

/// `vorbis_encode_peak_setup`（vorbisenc.c:417–427）。
fn vorbis_encode_peak_setup(
    vi: &mut VorbisInfo,
    s: f64,
    block: usize,
    suppress: &'static [i32],
) -> Result<(), EngineError> {
    let is = s as i32;
    let ds = s - f64::from(is);
    let (s0, s1) = (
        at(suppress, is, "psy_tone_dBsuppress")?,
        at(suppress, is + 1, "psy_tone_dBsuppress")?,
    );
    let p = psy_mut(vi, block)?;
    p.tone_abs_limit = interp(f64::from(s0), f64::from(s1), ds) as f32;
    Ok(())
}

/// `vorbis_encode_noisebias_setup`（vorbisenc.c:429–459）。
fn vorbis_encode_noisebias_setup(
    vi: &mut VorbisInfo,
    s: f64,
    block: usize,
    suppress: &'static [i32],
    bias: &'static [Noise3],
    guard: &'static [Noiseguard],
    userbias: f64,
) -> Result<(), EngineError> {
    let is = s as i32;
    let ds = s - f64::from(is);
    let (s0, s1) = (
        at(suppress, is, "psy_noise_dBsuppress")?,
        at(suppress, is + 1, "psy_noise_dBsuppress")?,
    );
    let gd = guard
        .get(block)
        .copied()
        .ok_or_else(|| out_of_range("psy_noiseguards"))?;
    let (b0, b1) = (
        at(bias, is, "psy_noise_bias")?,
        at(bias, is + 1, "psy_noise_bias")?,
    );
    let p = psy_mut(vi, block)?;

    p.noisemaxsupp = interp(f64::from(s0), f64::from(s1), ds) as f32;
    p.noisewindowlomin = gd.lo;
    p.noisewindowhimin = gd.hi;
    p.noisewindowfixed = gd.fixed;

    for j in 0..P_NOISECURVES {
        for i in 0..P_BANDS {
            p.noiseoff[j][i] =
                interp(f64::from(b0.data[j][i]), f64::from(b1.data[j][i]), ds) as f32;
        }
    }

    // impulse blocks may take a user specified bias to boost the nominal/high noise encoding depth
    for j in 0..P_NOISECURVES {
        let min: f32 = p.noiseoff[j][0] + 6.; // the lowest it can go（float + int は float）
        for i in 0..P_BANDS {
            p.noiseoff[j][i] = (f64::from(p.noiseoff[j][i]) + userbias) as f32;
            if p.noiseoff[j][i] < min {
                p.noiseoff[j][i] = min;
            }
        }
    }
    Ok(())
}

/// `vorbis_encode_ath_setup`（vorbisenc.c:461–468）。
fn vorbis_encode_ath_setup(vi: &mut VorbisInfo, block: usize) -> Result<(), EngineError> {
    let (adj, max) = (
        vi.codec_setup.hi.ath_floating_db,
        vi.codec_setup.hi.ath_absolute_db,
    );
    let p = psy_mut(vi, block)?;
    p.ath_adjatt = adj as f32;
    p.ath_maxatt = max as f32;
    Ok(())
}

/// `book_dup_or_new`（vorbisenc.c:471–477）：同じ本がすでにあればその番号、なければ新しい番号（`books` を 1 つ増やす）。
/// C は新しい番号を返したあとで呼ぶ側が `book_param[番号]` に書くが、ここで書いておく（呼ぶ側は同じ本を書き直すだけ）。
fn book_dup_or_new(vi: &mut VorbisInfo, book: BookId) -> Result<i32, EngineError> {
    let ci = &mut vi.codec_setup;
    if let Some(i) = ci.book_param.iter().position(|&b| b == book) {
        return Ok(i as i32);
    }
    push_book(ci, book)
}

/// `ci->book_param[ci->books++] = book`（C の配列は 256 冊まで）。足した番号を返す。
fn push_book(ci: &mut CodecSetupInfo, book: BookId) -> Result<i32, EngineError> {
    if usize::try_from(ci.books).ok() != Some(ci.book_param.len()) || ci.books >= 256 {
        return Err(EngineError::Internal(
            "vorbisenc：book の数が正しくありません".into(),
        ));
    }
    ci.book_param.push(book);
    let n = ci.books;
    ci.books += 1;
    Ok(n)
}

/// `vorbis_encode_blocksize_setup`（vorbisenc.c:479–490）。
fn vorbis_encode_blocksize_setup(
    vi: &mut VorbisInfo,
    s: f64,
    shortb: &'static [i32],
    longb: &'static [i32],
) -> Result<(), EngineError> {
    let is = s as i32;
    let blockshort = at(shortb, is, "blocksize_short")?;
    let blocklong = at(longb, is, "blocksize_long")?;
    vi.codec_setup.blocksizes[0] = i64::from(blockshort);
    vi.codec_setup.blocksizes[1] = i64::from(blocklong);
    Ok(())
}

/// `vorbis_encode_residue_setup`（vorbisenc.c:493–627。VBR の book）。
fn vorbis_encode_residue_setup(
    vi: &mut VorbisInfo,
    number: usize,
    block: usize,
    res: &'static VorbisResidueTemplate,
) -> Result<(), EngineError> {
    let mut r = *res.res;
    {
        let ci = &mut vi.codec_setup;
        if ci.residues as usize <= number {
            ci.residues = number as i32 + 1;
        }
        r.grouping = res.grouping;
        put(&mut ci.residue_type, number, res.res_type);
    }

    // fill in all the books（VBR：books_base・book_aux）
    {
        let mut booklist = 0i32;
        for i in 0..r.partitions {
            let row = at(&res.books_base.books, i, "books_base")?;
            let ss = at_mut(&mut r.secondstages, i, "secondstages")?;
            for (k, b) in row.iter().enumerate() {
                if b.is_some() {
                    *ss |= 1 << k;
                }
            }
        }

        r.groupbook = book_dup_or_new(vi, res.book_aux)?;

        for i in 0..r.partitions {
            let row = at(&res.books_base.books, i, "books_base")?;
            for b in row.iter().flatten() {
                let bookid = book_dup_or_new(vi, *b)?;
                *at_mut(&mut r.booklist, booklist, "booklist")? = bookid;
                booklist += 1;
            }
        }
    }

    // lowpass setup/pointlimit
    let channels = vi.channels;
    let ci = &mut vi.codec_setup;
    let mut freq = ci.hi.lowpass_khz * 1000.;
    let nyq = vi.rate as f64 / 2.;
    let blocksize: i64 = ci
        .blocksizes
        .get(block)
        .copied()
        .ok_or_else(|| out_of_range("blocksizes"))?
        >> 1;
    let f = ci
        .floor_param
        .get_mut(block)
        .ok_or_else(|| out_of_range("floor_param"))?; // by convention

    // lowpass needs to be set in the floor and the residue.
    if freq > nyq {
        freq = nyq;
    }
    // in the floor, the granularity can be very fine; it doesn't alter the encoding structure, only the samples used to
    // fit the floor approximation
    f.n = (freq / nyq * blocksize as f64) as i32;

    // this res may by limited by the maximum pointlimit of the mode, not the lowpass. the floor is always lowpass limited.
    match res.limit_type {
        1 => {
            // point stereo limited（VBR：真ん中の列）
            freq = f64::from(ci.psy_g_param.coupling_pkhz[PACKETBLOBS / 2]) * 1000.;
            if freq > nyq {
                freq = nyq;
            }
        }
        2 => {
            // LFE channel; lowpass at ~ 250Hz
            freq = f64::from(LFE_FRQLIMIT);
        }
        _ => {}
    }

    let grouping = i64::from(r.grouping);
    if res.res_type == 2 {
        // residue 2 bundles together multiple channels; used by stereo and surround. Count the channels in use
        let mut ch = 0i32;
        let mut i = 0usize;
        while i < ci.maps as usize && ch == 0 {
            let mi = ci
                .map_param
                .get(i)
                .ok_or_else(|| out_of_range("map_param"))?;
            let mut j = 0i32;
            while j < mi.submaps && ch == 0 {
                if at(&mi.residuesubmap, j, "residuesubmap")? == number as i32 {
                    // we found a submap referencing theis residue backend
                    for k in 0..channels {
                        if at(&mi.chmuxlist, k, "chmuxlist")? == j {
                            // this channel belongs to the submap
                            ch += 1;
                        }
                    }
                }
                j += 1;
            }
            i += 1;
        }

        let q = (freq / nyq * blocksize as f64 * f64::from(ch)) / f64::from(r.grouping) + 0.9;
        r.end = i64::from((q as i32).wrapping_mul(r.grouping));
        // the blocksize and grouping may disagree at the end
        let total = blocksize * i64::from(ch);
        if r.end > total {
            r.end = total
                .checked_div(grouping)
                .ok_or_else(|| EngineError::Internal("vorbisenc：grouping が 0".into()))?
                * grouping;
        }
        if freq != f64::from(LFE_FRQLIMIT) {
            // lowpass (residue)
            let lowpassr = r.end.checked_div(i64::from(ch)).ok_or_else(|| {
                EngineError::Internal("vorbisenc：residue 2 のチャンネルがない".into())
            })?;
            *ci.block_lowpassr
                .get_mut(block)
                .ok_or_else(|| out_of_range("block_lowpassr"))? = lowpassr as i32;
        }
    } else {
        let q = (freq / nyq * blocksize as f64) / f64::from(r.grouping) + 0.9;
        r.end = i64::from((q as i32).wrapping_mul(r.grouping));
        // the blocksize and grouping may disagree at the end
        if r.end > blocksize {
            r.end = blocksize
                .checked_div(grouping)
                .ok_or_else(|| EngineError::Internal("vorbisenc：grouping が 0".into()))?
                * grouping;
        }
        if freq != f64::from(LFE_FRQLIMIT) {
            // lowpass (residue)
            *ci.block_lowpassr
                .get_mut(block)
                .ok_or_else(|| out_of_range("block_lowpassr"))? = r.end as i32;
        }
    }

    if r.end == 0 {
        // LFE channel
        r.end = grouping;
    }
    put(&mut ci.residue_param, number, r);
    Ok(())
}

/// `vorbis_encode_map_n_res_setup`（vorbisenc.c:630–657）。
fn vorbis_encode_map_n_res_setup(
    vi: &mut VorbisInfo,
    s: f64,
    maps: &'static [VorbisMappingTemplate],
) -> Result<(), EngineError> {
    let is = s as i32;
    let t = at(maps, is, "maps")?;
    let map = t.map;
    let res = t.res;
    let mut modes = 2usize;

    if vi.codec_setup.blocksizes[0] == vi.codec_setup.blocksizes[1] {
        modes = 1;
    }

    for i in 0..modes {
        let ci = &mut vi.codec_setup;
        let mode = MODE_TEMPLATE
            .get(i)
            .copied()
            .ok_or_else(|| out_of_range("_mode_template"))?;
        let m = map.get(i).copied().ok_or_else(|| out_of_range("map"))?;

        put(&mut ci.mode_param, i, mode);
        if i as i32 >= ci.modes {
            ci.modes = i as i32 + 1;
        }

        put(&mut ci.map_type, i, 0);
        put(&mut ci.map_param, i, m);
        if i as i32 >= ci.maps {
            ci.maps = i as i32 + 1;
        }

        for j in 0..m.submaps {
            let number = at(&m.residuesubmap, j, "residuesubmap")?;
            let r = usize::try_from(number)
                .ok()
                .and_then(|n| res.get(n))
                .ok_or_else(|| out_of_range("res"))?;
            vorbis_encode_residue_setup(vi, number as usize, i, r)?;
        }
    }
    Ok(())
}

/// `setting_to_approx_bitrate`（vorbisenc.c:659–672）。
fn setting_to_approx_bitrate(vi: &VorbisInfo) -> Result<f64, EngineError> {
    let hi = &vi.codec_setup.hi;
    let setup = hi
        .setup
        .ok_or_else(|| EngineError::Internal("vorbisenc：テンプレートがない".into()))?;
    let is = hi.base_setting as i32;
    let ds = hi.base_setting - f64::from(is);
    let ch = vi.channels;
    let r = setup.rate_mapping;

    Ok(interp(
        at(r, is, "rate_mapping")?,
        at(r, is + 1, "rate_mapping")?,
        ds,
    ) * f64::from(ch))
}

/// `get_setup_template`（vorbisenc.c:674–713。品質の経路 `q_or_bitrate == 0`）：合うテンプレートと base_setting。
pub(crate) fn get_setup_template(
    ch: i64,
    srate: i64,
    req: f64,
) -> Option<(&'static VeSetupDataTemplate, f64)> {
    for &t in SETUP_LIST.iter() {
        if (t.coupling_restriction == -1 || i64::from(t.coupling_restriction) == ch)
            && srate >= t.samplerate_min_restriction
            && srate <= t.samplerate_max_restriction
        {
            let mappings = t.mappings;
            let map = t.quality_mapping;

            // the template matches. Does the requested quality mode fall within this template's modes?
            let (Some(&first), Some(&last)) = (
                map.first(),
                usize::try_from(mappings).ok().and_then(|m| map.get(m)),
            ) else {
                continue;
            };
            if req < first {
                continue;
            }
            if req > last {
                continue;
            }
            let mut j = 0i32;
            while j < mappings {
                let (Some(&a), Some(&b)) = (map.get(j as usize), map.get(j as usize + 1)) else {
                    break;
                };
                if req >= a && req < b {
                    break;
                }
                j += 1;
            }
            // an all-points match
            let base_setting = if j == mappings {
                f64::from(j) - 0.001
            } else {
                let low = map.get(j as usize).copied()? as f32;
                let high = map.get(j as usize + 1).copied()? as f32;
                let del = ((req - f64::from(low)) / f64::from(high - low)) as f32;
                f64::from(j as f32 + del)
            };
            return Some((t, base_setting));
        }
    }
    None
}

/// `vorbis_encode_setup_init`（vorbisenc.c:722–905）。
// 上下の止め方は C の 2 つの比較のまま写す（移植の規則 7。`clamp` に置き換えない）
#[allow(clippy::manual_clamp)]
fn vorbis_encode_setup_init(vi: &mut VorbisInfo) -> Result<(), EngineError> {
    let mut i0 = 0usize;

    if vi.channels < 1 || vi.channels > 255 {
        return Err(EngineError::Unsupported(format!(
            "Ogg Vorbis：{} チャンネルは書けません",
            vi.channels
        )));
    }
    {
        let hi = &mut vi.codec_setup.hi;
        if hi.impulse_block_p == 0 {
            i0 = 1;
        }

        // too low/high an ATH floater is nonsensical, but doesn't break anything
        if hi.ath_floating_db > -80. {
            hi.ath_floating_db = -80.;
        }
        if hi.ath_floating_db < -200. {
            hi.ath_floating_db = -200.;
        }

        // again, bound this to avoid the app shooting itself int he foot too badly
        if hi.amplitude_track_dbpersec > 0. {
            hi.amplitude_track_dbpersec = 0.;
        }
        if hi.amplitude_track_dbpersec < -99999. {
            hi.amplitude_track_dbpersec = -99999.;
        }
    }

    // get the appropriate setup template; matches the fetch in previous stages
    let hi = vi.codec_setup.hi;
    let setup = hi
        .setup
        .ok_or_else(|| EngineError::Internal("vorbisenc：テンプレートがない".into()))?;
    let missing =
        |what: &str| EngineError::Internal(format!("vorbisenc：テンプレートに {what} がない"));

    // choose block sizes from configured sizes as well as paying attention to long_block_p and short_block_p. If the
    // configured short and long blocks are the same length, we set long_block_p and unset short_block_p
    vorbis_encode_blocksize_setup(
        vi,
        hi.base_setting,
        setup.blocksize_short,
        setup.blocksize_long,
    )?;
    let singleblock = vi.codec_setup.blocksizes[0] == vi.codec_setup.blocksizes[1];

    // floor setup; choose proper floor params. Allocated on the floor stack in order; if we alloc only a single long
    // floor, it's 0
    for i in 0..setup.floor_mappings {
        vorbis_encode_floor_setup(
            vi,
            hi.base_setting,
            setup.floor_books,
            setup.floor_params,
            at(setup.floor_mapping_list, i, "floor_mapping_list")?,
        )?;
    }

    // setup of [mostly] short block detection and stereo
    vorbis_encode_global_psych_setup(
        vi,
        hi.trigger_setting,
        setup.global_params,
        setup.global_mapping,
    )?;
    vorbis_encode_global_stereo(vi, setup.stereo_modes)?;

    // basic psych setup and noise normalization
    let [start_short, start_long] = setup.psy_noise_normal_start;
    let [part_short, part_long] = setup.psy_noise_normal_partition;
    let thresh = setup.psy_noise_normal_thresh;
    vorbis_encode_psyset_setup(vi, hi.base_setting, start_short, part_short, thresh, 0)?;
    vorbis_encode_psyset_setup(vi, hi.base_setting, start_short, part_short, thresh, 1)?;
    if !singleblock {
        vorbis_encode_psyset_setup(vi, hi.base_setting, start_long, part_long, thresh, 2)?;
        vorbis_encode_psyset_setup(vi, hi.base_setting, start_long, part_long, thresh, 3)?;
    }

    // tone masking setup
    vorbis_encode_tonemask_setup(
        vi,
        hi.block[i0].tone_mask_setting,
        0,
        setup.psy_tone_masteratt,
        setup.psy_tone_0db,
        setup.psy_tone_adj_impulse,
    )?;
    vorbis_encode_tonemask_setup(
        vi,
        hi.block[1].tone_mask_setting,
        1,
        setup.psy_tone_masteratt,
        setup.psy_tone_0db,
        setup.psy_tone_adj_other,
    )?;
    if !singleblock {
        vorbis_encode_tonemask_setup(
            vi,
            hi.block[2].tone_mask_setting,
            2,
            setup.psy_tone_masteratt,
            setup.psy_tone_0db,
            setup.psy_tone_adj_other,
        )?;
        vorbis_encode_tonemask_setup(
            vi,
            hi.block[3].tone_mask_setting,
            3,
            setup.psy_tone_masteratt,
            setup.psy_tone_0db,
            setup
                .psy_tone_adj_long
                .ok_or_else(|| missing("psy_tone_adj_long"))?,
        )?;
    }

    // noise companding setup
    vorbis_encode_compand_setup(
        vi,
        hi.block[i0].noise_compand_setting,
        0,
        setup.psy_noise_compand,
        setup.psy_noise_compand_short_mapping,
        setup.mappings,
    )?;
    vorbis_encode_compand_setup(
        vi,
        hi.block[1].noise_compand_setting,
        1,
        setup.psy_noise_compand,
        setup.psy_noise_compand_short_mapping,
        setup.mappings,
    )?;
    if !singleblock {
        let long_mapping = setup
            .psy_noise_compand_long_mapping
            .ok_or_else(|| missing("psy_noise_compand_long_mapping"))?;
        vorbis_encode_compand_setup(
            vi,
            hi.block[2].noise_compand_setting,
            2,
            setup.psy_noise_compand,
            long_mapping,
            setup.mappings,
        )?;
        vorbis_encode_compand_setup(
            vi,
            hi.block[3].noise_compand_setting,
            3,
            setup.psy_noise_compand,
            long_mapping,
            setup.mappings,
        )?;
    }

    // peak guarding setup
    vorbis_encode_peak_setup(
        vi,
        hi.block[i0].tone_peaklimit_setting,
        0,
        setup.psy_tone_dbsuppress,
    )?;
    vorbis_encode_peak_setup(
        vi,
        hi.block[1].tone_peaklimit_setting,
        1,
        setup.psy_tone_dbsuppress,
    )?;
    if !singleblock {
        vorbis_encode_peak_setup(
            vi,
            hi.block[2].tone_peaklimit_setting,
            2,
            setup.psy_tone_dbsuppress,
        )?;
        vorbis_encode_peak_setup(
            vi,
            hi.block[3].tone_peaklimit_setting,
            3,
            setup.psy_tone_dbsuppress,
        )?;
    }

    // noise bias setup
    vorbis_encode_noisebias_setup(
        vi,
        hi.block[i0].noise_bias_setting,
        0,
        setup.psy_noise_dbsuppress,
        setup.psy_noise_bias_impulse,
        setup.psy_noiseguards,
        if i0 == 0 { hi.impulse_noisetune } else { 0. },
    )?;
    vorbis_encode_noisebias_setup(
        vi,
        hi.block[1].noise_bias_setting,
        1,
        setup.psy_noise_dbsuppress,
        setup.psy_noise_bias_padding,
        setup.psy_noiseguards,
        0.,
    )?;
    if !singleblock {
        vorbis_encode_noisebias_setup(
            vi,
            hi.block[2].noise_bias_setting,
            2,
            setup.psy_noise_dbsuppress,
            setup
                .psy_noise_bias_trans
                .ok_or_else(|| missing("psy_noise_bias_trans"))?,
            setup.psy_noiseguards,
            0.,
        )?;
        vorbis_encode_noisebias_setup(
            vi,
            hi.block[3].noise_bias_setting,
            3,
            setup.psy_noise_dbsuppress,
            setup
                .psy_noise_bias_long
                .ok_or_else(|| missing("psy_noise_bias_long"))?,
            setup.psy_noiseguards,
            0.,
        )?;
    }

    vorbis_encode_ath_setup(vi, 0)?;
    vorbis_encode_ath_setup(vi, 1)?;
    if !singleblock {
        vorbis_encode_ath_setup(vi, 2)?;
        vorbis_encode_ath_setup(vi, 3)?;
    }

    vorbis_encode_map_n_res_setup(vi, hi.base_setting, setup.maps)?;

    // set bitrate readonlies（VBR：bitrate_av・bitrate_min・bitrate_max・bitrate_reservoir は 0）
    vi.bitrate_nominal = setting_to_approx_bitrate(vi)? as i64;
    vi.bitrate_lower = 0;
    vi.bitrate_upper = 0;
    vi.bitrate_window = 0;
    Ok(())
}

/// `vorbis_encode_setup_setting`（vorbisenc.c:907–950。前増幅を除く）。
fn vorbis_encode_setup_setting(
    vi: &mut VorbisInfo,
    channels: i64,
    rate: i64,
) -> Result<(), EngineError> {
    let setup = vi
        .codec_setup
        .hi
        .setup
        .ok_or_else(|| EngineError::Internal("vorbisenc：テンプレートがない".into()))?;

    vi.version = 0;
    vi.channels = i32::try_from(channels).map_err(|_| {
        EngineError::Unsupported(format!("Ogg Vorbis：{channels} チャンネルは書けません"))
    })?;
    vi.rate = rate;

    let hi = &mut vi.codec_setup.hi;
    hi.impulse_block_p = 1;
    hi.noise_normalize_p = 1;

    let is = hi.base_setting as i32;
    let ds = hi.base_setting - f64::from(is);

    hi.stereo_point_setting = hi.base_setting;

    if hi.lowpass_altered == 0 {
        hi.lowpass_khz = interp(
            at(setup.psy_lowpass, is, "psy_lowpass")?,
            at(setup.psy_lowpass, is + 1, "psy_lowpass")?,
            ds,
        );
    }

    // 前増幅（pre_amplitude）は移さない（ADR-149）

    hi.ath_floating_db = interp(
        f64::from(at(setup.psy_ath_float, is, "psy_ath_float")?),
        f64::from(at(setup.psy_ath_float, is + 1, "psy_ath_float")?),
        ds,
    );
    hi.ath_absolute_db = interp(
        f64::from(at(setup.psy_ath_abs, is, "psy_ath_abs")?),
        f64::from(at(setup.psy_ath_abs, is + 1, "psy_ath_abs")?),
        ds,
    );

    hi.amplitude_track_dbpersec = -6.;
    hi.trigger_setting = hi.base_setting;

    for b in hi.block.iter_mut() {
        b.tone_mask_setting = hi.base_setting;
        b.tone_peaklimit_setting = hi.base_setting;
        b.noise_bias_setting = hi.base_setting;
        b.noise_compand_setting = hi.base_setting;
    }
    Ok(())
}

/// `vorbis_encode_setup_vbr`（vorbisenc.c:952–975）。
fn vorbis_encode_setup_vbr(
    vi: &mut VorbisInfo,
    channels: i64,
    rate: i64,
    quality: f32,
) -> Result<(), EngineError> {
    if rate <= 0 {
        return Err(EngineError::Unsupported(format!(
            "Ogg Vorbis：レート {rate} Hz は書けません"
        )));
    }

    // quality += .0000001（float に double を足して float に）
    let mut quality = (f64::from(quality) + 0.0000001) as f32;
    if f64::from(quality) >= 1. {
        quality = 0.9999_f64 as f32;
    }

    let hi = &mut vi.codec_setup.hi;
    hi.req = quality;
    let (setup, base_setting) =
        get_setup_template(channels, rate, f64::from(quality)).ok_or_else(|| {
            EngineError::Unsupported(format!(
                "Ogg Vorbis：{channels} チャンネル・{rate} Hz・品質 {quality} に合うテンプレートがありません"
            ))
        })?;
    hi.setup = Some(setup);
    hi.base_setting = base_setting;

    vorbis_encode_setup_setting(vi, channels, rate)?;
    vi.codec_setup.hi.managed = 0;
    vi.codec_setup.hi.coupling_p = 1;
    Ok(())
}

/// `vorbis_encode_init_vbr`（vorbisenc.c:977–995）：`base_quality` は `q as f32 / 10.0`。
pub(crate) fn vorbis_encode_init_vbr(
    channels: i64,
    rate: i64,
    base_quality: f32,
) -> Result<VorbisInfo, EngineError> {
    let mut vi = vorbis_info_init();
    vorbis_encode_setup_vbr(&mut vi, channels, rate, base_quality)?;
    vorbis_encode_setup_init(&mut vi)?;
    Ok(vi)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::Value;
    use vorbis_tables::{BOOKS, StaticCodebook, unpack};

    use super::*;
    use crate::codec::vorbis::oracle;

    /// 照合するレート（すべてのテンプレートの帯：8・11・16・22・32・44.1・48 kHz と X）。
    pub(crate) const RATES: [u32; 9] = [
        8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
    ];
    /// 照合するチャンネルの数（2 は stereo、ほかは uncoupled）。
    pub(crate) const CHANNELS: [usize; 5] = [1, 2, 3, 6, 8];

    /// q（−1〜10）から libvorbis に渡す品質（C の `(float)q / 10.f`）。
    pub(crate) fn quality(q: i32) -> f32 {
        q as f32 / 10.0
    }

    pub(crate) fn setup_of(rate: u32, ch: usize, q: i32) -> VorbisInfo {
        vorbis_encode_init_vbr(ch as i64, i64::from(rate), quality(q))
            .unwrap_or_else(|e| panic!("{rate} Hz・{ch} ch・q{q}：{e}"))
    }

    /// テンプレートの名前（C の記号名。照合用の `oracle_setup` の `template.name` と同じ）。
    fn template_name(t: &VeSetupDataTemplate) -> &'static str {
        use vorbis_tables::*;
        let list: [(&str, &VeSetupDataTemplate); 18] = [
            ("ve_setup_48_stereo", &VE_SETUP_48_STEREO),
            ("ve_setup_44_stereo", &VE_SETUP_44_STEREO),
            ("ve_setup_48_uncoupled", &VE_SETUP_48_UNCOUPLED),
            ("ve_setup_44_uncoupled", &VE_SETUP_44_UNCOUPLED),
            ("ve_setup_32_stereo", &VE_SETUP_32_STEREO),
            ("ve_setup_32_uncoupled", &VE_SETUP_32_UNCOUPLED),
            ("ve_setup_22_stereo", &VE_SETUP_22_STEREO),
            ("ve_setup_22_uncoupled", &VE_SETUP_22_UNCOUPLED),
            ("ve_setup_16_stereo", &VE_SETUP_16_STEREO),
            ("ve_setup_16_uncoupled", &VE_SETUP_16_UNCOUPLED),
            ("ve_setup_11_stereo", &VE_SETUP_11_STEREO),
            ("ve_setup_11_uncoupled", &VE_SETUP_11_UNCOUPLED),
            ("ve_setup_8_stereo", &VE_SETUP_8_STEREO),
            ("ve_setup_8_uncoupled", &VE_SETUP_8_UNCOUPLED),
            ("ve_setup_X_stereo", &VE_SETUP_X_STEREO),
            ("ve_setup_X_uncoupled", &VE_SETUP_X_UNCOUPLED),
            ("ve_setup_XX_stereo", &VE_SETUP_XX_STEREO),
            ("ve_setup_XX_uncoupled", &VE_SETUP_XX_UNCOUPLED),
        ];
        list.iter()
            .find(|(_, x)| std::ptr::eq(*x, t))
            .map_or("?", |(n, _)| *n)
    }

    /// JSON（`oracle_setup --bits`）と移植の値を比べ、違う所を集める。整数は値、浮動小数はビット列で比べる。
    #[derive(Default)]
    struct Cmp {
        errs: Vec<String>,
        checked: usize,
    }

    impl Cmp {
        fn fail(&mut self, path: &str, msg: String) {
            self.errs.push(format!("{path}: {msg}"));
        }

        fn int(&mut self, path: &str, j: &Value, got: i64) {
            self.checked += 1;
            match j.as_i64() {
                Some(want) if want == got => {}
                _ => self.fail(path, format!("C {j} と Rust {got}")),
            }
        }

        fn f32(&mut self, path: &str, j: &Value, got: f32) {
            self.checked += 1;
            let want = oracle::json_f32(j);
            if want.to_bits() != got.to_bits() {
                self.fail(
                    path,
                    format!(
                        "C {want:e}（{:#010x}）と Rust {got:e}（{:#010x}）",
                        want.to_bits(),
                        got.to_bits()
                    ),
                );
            }
        }

        fn f64(&mut self, path: &str, j: &Value, got: f64) {
            self.checked += 1;
            let want = oracle::json_f64(j);
            if want.to_bits() != got.to_bits() {
                self.fail(
                    path,
                    format!(
                        "C {want:e}（{:#018x}）と Rust {got:e}（{:#018x}）",
                        want.to_bits(),
                        got.to_bits()
                    ),
                );
            }
        }

        fn arr<'a>(&mut self, path: &str, j: &'a Value, len: usize) -> &'a [Value] {
            let a = j.as_array().map_or(&[][..], Vec::as_slice);
            if a.len() != len {
                self.fail(path, format!("要素の数が C {} と Rust {len}", a.len()));
            }
            a
        }

        fn ints(&mut self, path: &str, j: &Value, got: &[i32]) {
            let a = self.arr(path, j, got.len());
            for (k, (v, g)) in a.iter().zip(got).enumerate() {
                self.int(&format!("{path}[{k}]"), v, i64::from(*g));
            }
        }

        fn f32s(&mut self, path: &str, j: &Value, got: &[f32]) {
            let a = self.arr(path, j, got.len());
            for (k, (v, g)) in a.iter().zip(got).enumerate() {
                self.f32(&format!("{path}[{k}]"), v, *g);
            }
        }
    }

    /// `vorbis_info_psy` の全項目。
    fn cmp_psy(c: &mut Cmp, path: &str, j: &Value, p: &VorbisInfoPsy) {
        let k = |name: &str| format!("{path}.{name}");
        c.int(&k("blockflag"), &j["blockflag"], i64::from(p.blockflag));
        c.f32(&k("ath_adjatt"), &j["ath_adjatt"], p.ath_adjatt);
        c.f32(&k("ath_maxatt"), &j["ath_maxatt"], p.ath_maxatt);
        c.f32s(
            &k("tone_masteratt"),
            &j["tone_masteratt"],
            &p.tone_masteratt,
        );
        c.f32(
            &k("tone_centerboost"),
            &j["tone_centerboost"],
            p.tone_centerboost,
        );
        c.f32(&k("tone_decay"), &j["tone_decay"], p.tone_decay);
        c.f32(&k("tone_abs_limit"), &j["tone_abs_limit"], p.tone_abs_limit);
        c.f32s(&k("toneatt"), &j["toneatt"], &p.toneatt);
        c.int(&k("noisemaskp"), &j["noisemaskp"], i64::from(p.noisemaskp));
        c.f32(&k("noisemaxsupp"), &j["noisemaxsupp"], p.noisemaxsupp);
        c.f32(&k("noisewindowlo"), &j["noisewindowlo"], p.noisewindowlo);
        c.f32(&k("noisewindowhi"), &j["noisewindowhi"], p.noisewindowhi);
        c.int(
            &k("noisewindowlomin"),
            &j["noisewindowlomin"],
            i64::from(p.noisewindowlomin),
        );
        c.int(
            &k("noisewindowhimin"),
            &j["noisewindowhimin"],
            i64::from(p.noisewindowhimin),
        );
        c.int(
            &k("noisewindowfixed"),
            &j["noisewindowfixed"],
            i64::from(p.noisewindowfixed),
        );
        let noff = c.arr(&k("noiseoff"), &j["noiseoff"], P_NOISECURVES);
        for (n, (v, g)) in noff.iter().zip(&p.noiseoff).enumerate() {
            c.f32s(&format!("{path}.noiseoff[{n}]"), v, g);
        }
        c.f32s(&k("noisecompand"), &j["noisecompand"], &p.noisecompand);
        c.f32s(
            &k("noisecompand_high"),
            &j["noisecompand_high"],
            &p.noisecompand_high,
        );
        c.f32(&k("flacint"), &j["flacint"], p.flacint);
        c.f32(&k("max_curve_dB"), &j["max_curve_dB"], p.max_curve_db);
        c.int(&k("normal_p"), &j["normal_p"], i64::from(p.normal_p));
        c.int(
            &k("normal_start"),
            &j["normal_start"],
            i64::from(p.normal_start),
        );
        c.int(
            &k("normal_partition"),
            &j["normal_partition"],
            i64::from(p.normal_partition),
        );
        c.f64(&k("normal_thresh"), &j["normal_thresh"], p.normal_thresh);
    }

    /// 照合用の `oracle_setup` の JSON のすべての項目を、移植の `VorbisInfo` と比べる。
    fn compare_setup(
        vi: &VorbisInfo,
        j: &Value,
        books_cache: &mut BTreeMap<u16, StaticCodebook>,
    ) -> Cmp {
        let mut c = Cmp::default();
        let ci = &vi.codec_setup;
        let hi = &ci.hi;
        let ch = vi.channels as usize;

        // vorbis_info（識別ヘッダの値）
        let v = &j["vorbis_info"];
        c.int("vorbis_info.version", &v["version"], i64::from(vi.version));
        c.int(
            "vorbis_info.channels",
            &v["channels"],
            i64::from(vi.channels),
        );
        c.int("vorbis_info.rate", &v["rate"], vi.rate);
        c.int(
            "vorbis_info.bitrate_upper",
            &v["bitrate_upper"],
            vi.bitrate_upper,
        );
        c.int(
            "vorbis_info.bitrate_nominal",
            &v["bitrate_nominal"],
            vi.bitrate_nominal,
        );
        c.int(
            "vorbis_info.bitrate_lower",
            &v["bitrate_lower"],
            vi.bitrate_lower,
        );
        c.int(
            "vorbis_info.bitrate_window",
            &v["bitrate_window"],
            vi.bitrate_window,
        );

        // テンプレート
        let t = &j["template"];
        match hi.setup {
            Some(s) => {
                let name = template_name(s);
                if t["name"].as_str() != Some(name) {
                    c.fail("template.name", format!("C {} と Rust {name}", t["name"]));
                }
                c.int(
                    "template.coupling_restriction",
                    &t["coupling_restriction"],
                    i64::from(s.coupling_restriction),
                );
                c.int(
                    "template.samplerate_min",
                    &t["samplerate_min"],
                    s.samplerate_min_restriction,
                );
                c.int(
                    "template.samplerate_max",
                    &t["samplerate_max"],
                    s.samplerate_max_restriction,
                );
                c.int("template.mappings", &t["mappings"], i64::from(s.mappings));
            }
            None => c.fail("template", "Rust にテンプレートがない".into()),
        }

        let bs = c.arr("blocksizes", &j["blocksizes"], 2);
        for (k, (jv, g)) in bs.iter().zip(&ci.blocksizes).enumerate() {
            c.int(&format!("blocksizes[{k}]"), jv, *g);
        }
        c.int("halfrate_flag", &j["halfrate_flag"], 0);
        c.ints("block_lowpassr", &j["block_lowpassr"], &ci.block_lowpassr);

        // mode
        let modes = c.arr("modes", &j["modes"], ci.modes as usize);
        for (k, (jm, m)) in modes.iter().zip(&ci.mode_param).enumerate() {
            let p = |name: &str| format!("modes[{k}].{name}");
            c.int(&p("blockflag"), &jm["blockflag"], i64::from(m.blockflag));
            c.int(&p("windowtype"), &jm["windowtype"], i64::from(m.windowtype));
            c.int(
                &p("transformtype"),
                &jm["transformtype"],
                i64::from(m.transformtype),
            );
            c.int(&p("mapping"), &jm["mapping"], i64::from(m.mapping));
        }
        if ci.mode_param.len() != ci.modes as usize {
            c.fail(
                "mode_param",
                format!("{} 個と modes {}", ci.mode_param.len(), ci.modes),
            );
        }

        // mapping
        let maps = c.arr("maps", &j["maps"], ci.maps as usize);
        for (k, (jm, m)) in maps.iter().zip(&ci.map_param).enumerate() {
            let p = |name: &str| format!("maps[{k}].{name}");
            let sub = m.submaps as usize;
            let steps = m.coupling_steps as usize;
            c.int(&p("type"), &jm["type"], i64::from(ci.map_type[k]));
            c.int(&p("submaps"), &jm["submaps"], i64::from(m.submaps));
            c.ints(&p("chmuxlist"), &jm["chmuxlist"], &m.chmuxlist[..ch]);
            c.ints(&p("floorsubmap"), &jm["floorsubmap"], &m.floorsubmap[..sub]);
            c.ints(
                &p("residuesubmap"),
                &jm["residuesubmap"],
                &m.residuesubmap[..sub],
            );
            c.int(
                &p("coupling_steps"),
                &jm["coupling_steps"],
                i64::from(m.coupling_steps),
            );
            c.ints(
                &p("coupling_mag"),
                &jm["coupling_mag"],
                &m.coupling_mag[..steps],
            );
            c.ints(
                &p("coupling_ang"),
                &jm["coupling_ang"],
                &m.coupling_ang[..steps],
            );
        }

        // floor
        let floors = c.arr("floors", &j["floors"], ci.floors as usize);
        for (k, (jf, f)) in floors.iter().zip(&ci.floor_param).enumerate() {
            let p = |name: &str| format!("floors[{k}].{name}");
            let parts = f.partitions as usize;
            let classes = f.partitionclass[..parts]
                .iter()
                .map(|&x| x + 1)
                .max()
                .unwrap_or(0) as usize;
            let posts = 2 + f.partitionclass[..parts]
                .iter()
                .map(|&x| f.class_dim[x as usize])
                .sum::<i32>() as usize;
            c.int(&p("type"), &jf["type"], i64::from(ci.floor_type[k]));
            c.int(&p("partitions"), &jf["partitions"], i64::from(f.partitions));
            c.ints(
                &p("partitionclass"),
                &jf["partitionclass"],
                &f.partitionclass[..parts],
            );
            c.ints(&p("class_dim"), &jf["class_dim"], &f.class_dim[..classes]);
            c.ints(
                &p("class_subs"),
                &jf["class_subs"],
                &f.class_subs[..classes],
            );
            c.ints(
                &p("class_book"),
                &jf["class_book"],
                &f.class_book[..classes],
            );
            let sb = c.arr(&p("class_subbook"), &jf["class_subbook"], classes);
            for (q, jv) in sb.iter().enumerate() {
                let n = 1usize << f.class_subs[q];
                c.ints(
                    &format!("floors[{k}].class_subbook[{q}]"),
                    jv,
                    &f.class_subbook[q][..n],
                );
            }
            c.int(&p("mult"), &jf["mult"], i64::from(f.mult));
            c.int(&p("posts"), &jf["posts"], posts as i64);
            c.ints(&p("postlist"), &jf["postlist"], &f.postlist[..posts]);
            c.f32(&p("maxover"), &jf["maxover"], f.maxover);
            c.f32(&p("maxunder"), &jf["maxunder"], f.maxunder);
            c.f32(&p("maxerr"), &jf["maxerr"], f.maxerr);
            c.f32(&p("twofitweight"), &jf["twofitweight"], f.twofitweight);
            c.f32(&p("twofitatten"), &jf["twofitatten"], f.twofitatten);
            c.int(&p("n"), &jf["n"], i64::from(f.n));
        }

        // residue
        let residues = c.arr("residues", &j["residues"], ci.residues as usize);
        for (k, (jr, r)) in residues.iter().zip(&ci.residue_param).enumerate() {
            let p = |name: &str| format!("residues[{k}].{name}");
            let parts = r.partitions as usize;
            let nbooks = r.secondstages[..parts]
                .iter()
                .map(|&s| (s as u32).count_ones() as usize)
                .sum::<usize>();
            c.int(&p("type"), &jr["type"], i64::from(ci.residue_type[k]));
            c.int(&p("begin"), &jr["begin"], r.begin);
            c.int(&p("end"), &jr["end"], r.end);
            c.int(&p("grouping"), &jr["grouping"], i64::from(r.grouping));
            c.int(&p("partitions"), &jr["partitions"], i64::from(r.partitions));
            c.int(&p("partvals"), &jr["partvals"], i64::from(r.partvals));
            c.int(&p("groupbook"), &jr["groupbook"], i64::from(r.groupbook));
            c.ints(
                &p("secondstages"),
                &jr["secondstages"],
                &r.secondstages[..parts],
            );
            c.ints(&p("booklist"), &jr["booklist"], &r.booklist[..nbooks]);
            c.ints(
                &p("classmetric1"),
                &jr["classmetric1"],
                &r.classmetric1[..parts],
            );
            c.ints(
                &p("classmetric2"),
                &jr["classmetric2"],
                &r.classmetric2[..parts],
            );
        }

        // book（名前と、books.bin から読み戻した値）
        let books = c.arr("books", &j["books"], ci.books as usize);
        for (k, (jb, id)) in books.iter().zip(&ci.book_param).enumerate() {
            let p = |name: &str| format!("books[{k}].{name}");
            let name = BOOKS[usize::from(id.0)].name;
            if jb["name"].as_str() != Some(name) {
                c.fail(&p("name"), format!("C {} と Rust {name}", jb["name"]));
            }
            let b = books_cache
                .entry(id.0)
                .or_insert_with(|| unpack(*id).unwrap());
            let used = b.lengthlist.iter().filter(|&&l| l > 0).count();
            let maxlen = b.lengthlist.iter().copied().max().unwrap_or(0);
            c.int(&p("index"), &jb["index"], k as i64);
            c.int(&p("dim"), &jb["dim"], b.dim);
            c.int(&p("entries"), &jb["entries"], b.entries);
            c.int(&p("used_entries"), &jb["used_entries"], used as i64);
            c.int(&p("max_length"), &jb["max_length"], i64::from(maxlen));
            c.int(&p("maptype"), &jb["maptype"], i64::from(b.maptype));
            // q_min・q_delta：C の表は負の long で書いたものもある（意味があるのは下位 32 ビット）
            for (key, got) in [("q_min", b.q_min), ("q_delta", b.q_delta)] {
                c.checked += 1;
                if jb[key].as_i64().map(|v| v as u32) != Some(got as u32) {
                    c.fail(&p(key), format!("C {} と Rust {got}", jb[key]));
                }
            }
            c.int(&p("q_quant"), &jb["q_quant"], i64::from(b.q_quant));
            c.int(
                &p("q_sequencep"),
                &jb["q_sequencep"],
                i64::from(b.q_sequencep),
            );
        }
        if ci.book_param.len() != ci.books as usize {
            c.fail(
                "book_param",
                format!("{} 冊と books {}", ci.book_param.len(), ci.books),
            );
        }

        // psy
        let psy = c.arr("psy", &j["psy"], ci.psys as usize);
        for (k, (jp, p)) in psy.iter().zip(&ci.psy_param).enumerate() {
            cmp_psy(&mut c, &format!("psy[{k}]"), jp, p);
        }
        if ci.psy_param.len() != ci.psys as usize {
            c.fail(
                "psy_param",
                format!("{} 個と psys {}", ci.psy_param.len(), ci.psys),
            );
        }

        // psy_global
        let g = &ci.psy_g_param;
        let jg = &j["psy_global"];
        let p = |name: &str| format!("psy_global.{name}");
        c.int(
            &p("eighth_octave_lines"),
            &jg["eighth_octave_lines"],
            i64::from(g.eighth_octave_lines),
        );
        c.f32s(
            &p("preecho_thresh"),
            &jg["preecho_thresh"],
            &g.preecho_thresh,
        );
        c.f32s(
            &p("postecho_thresh"),
            &jg["postecho_thresh"],
            &g.postecho_thresh,
        );
        c.f32(
            &p("stretch_penalty"),
            &jg["stretch_penalty"],
            g.stretch_penalty,
        );
        c.f32(
            &p("preecho_minenergy"),
            &jg["preecho_minenergy"],
            g.preecho_minenergy,
        );
        c.f32(
            &p("ampmax_att_per_sec"),
            &jg["ampmax_att_per_sec"],
            g.ampmax_att_per_sec,
        );
        c.ints(&p("coupling_pkHz"), &jg["coupling_pkHz"], &g.coupling_pkhz);
        for (name, got) in [
            ("coupling_pointlimit", &g.coupling_pointlimit),
            ("sliding_lowpass", &g.sliding_lowpass),
        ] {
            let a = c.arr(&p(name), &jg[name], 2);
            for (k, (jv, row)) in a.iter().zip(got).enumerate() {
                c.ints(&format!("psy_global.{name}[{k}]"), jv, row);
            }
        }
        c.ints(
            &p("coupling_prepointamp"),
            &jg["coupling_prepointamp"],
            &g.coupling_prepointamp,
        );
        c.ints(
            &p("coupling_postpointamp"),
            &jg["coupling_postpointamp"],
            &g.coupling_postpointamp,
        );
        c.int(&p("vbr_blob"), &jg["vbr_blob"], (PACKETBLOBS / 2) as i64);

        // highlevel（移植が持たない欄は VBR の値：set_in_stone 1、ビットレートの欄 0。pre_amplitude は移さない）
        let jh = &j["highlevel"];
        let p = |name: &str| format!("highlevel.{name}");
        c.int(&p("set_in_stone"), &jh["set_in_stone"], 1);
        c.f64(&p("base_setting"), &jh["base_setting"], hi.base_setting);
        c.f64(
            &p("impulse_noisetune"),
            &jh["impulse_noisetune"],
            hi.impulse_noisetune,
        );
        c.f32(&p("req"), &jh["req"], hi.req);
        c.int(&p("managed"), &jh["managed"], i64::from(hi.managed));
        for key in [
            "bitrate_min",
            "bitrate_av",
            "bitrate_max",
            "bitrate_reservoir",
        ] {
            c.int(&p(key), &jh[key], 0);
        }
        for key in ["bitrate_av_damp", "bitrate_reservoir_bias"] {
            c.f64(&p(key), &jh[key], 0.0);
        }
        c.int(
            &p("impulse_block_p"),
            &jh["impulse_block_p"],
            i64::from(hi.impulse_block_p),
        );
        c.int(
            &p("noise_normalize_p"),
            &jh["noise_normalize_p"],
            i64::from(hi.noise_normalize_p),
        );
        c.int(
            &p("coupling_p"),
            &jh["coupling_p"],
            i64::from(hi.coupling_p),
        );
        c.f64(
            &p("stereo_point_setting"),
            &jh["stereo_point_setting"],
            hi.stereo_point_setting,
        );
        c.f64(&p("lowpass_kHz"), &jh["lowpass_kHz"], hi.lowpass_khz);
        c.int(
            &p("lowpass_altered"),
            &jh["lowpass_altered"],
            i64::from(hi.lowpass_altered),
        );
        c.f64(
            &p("ath_floating_dB"),
            &jh["ath_floating_dB"],
            hi.ath_floating_db,
        );
        c.f64(
            &p("ath_absolute_dB"),
            &jh["ath_absolute_dB"],
            hi.ath_absolute_db,
        );
        c.f64(
            &p("amplitude_track_dBpersec"),
            &jh["amplitude_track_dBpersec"],
            hi.amplitude_track_dbpersec,
        );
        c.f64(
            &p("trigger_setting"),
            &jh["trigger_setting"],
            hi.trigger_setting,
        );
        let blocks = c.arr(&p("block"), &jh["block"], 4);
        for (k, (jb, b)) in blocks.iter().zip(&hi.block).enumerate() {
            let p = |name: &str| format!("highlevel.block[{k}].{name}");
            c.f64(
                &p("tone_mask_setting"),
                &jb["tone_mask_setting"],
                b.tone_mask_setting,
            );
            c.f64(
                &p("tone_peaklimit_setting"),
                &jb["tone_peaklimit_setting"],
                b.tone_peaklimit_setting,
            );
            c.f64(
                &p("noise_bias_setting"),
                &jb["noise_bias_setting"],
                b.noise_bias_setting,
            );
            c.f64(
                &p("noise_compand_setting"),
                &jb["noise_compand_setting"],
                b.noise_compand_setting,
            );
        }
        c
    }

    /// 照合 (a)：9 レート × 1・2・3・6・8 ch × q −1〜10 のすべてで、`oracle_setup` の設定の全項目（vorbis_info・テンプレート・
    /// blocksizes・modes・maps・floors・residues・books・psy・psy_global・highlevel）がビット単位で一致する。
    #[test]
    fn oracle_setup_matches_bit_for_bit() {
        if oracle::root().is_none() {
            return;
        }
        let mut cache = BTreeMap::new();
        let mut cases = 0;
        let mut checked = 0;
        let mut templates = BTreeSet::new();
        let mut shapes = BTreeSet::new();
        for rate in RATES {
            for ch in CHANNELS {
                for q in -1..=10 {
                    let json = oracle::setup(rate, ch, q, false).unwrap();
                    let vi = setup_of(rate, ch, q);
                    let c = compare_setup(&vi, &json, &mut cache);
                    assert!(
                        c.errs.is_empty(),
                        "{rate} Hz・{ch} ch・q{q}：{} か所が違う\n{}",
                        c.errs.len(),
                        c.errs
                            .iter()
                            .take(20)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    checked += c.checked;
                    cases += 1;
                    templates.insert(template_name(vi.codec_setup.hi.setup.unwrap()));
                    shapes.insert(vi.codec_setup.blocksizes);
                }
            }
        }
        assert_eq!(cases, 9 * 5 * 12);
        // kura の範囲（8 kHz 以上）で選ばれうるテンプレート 16 個すべてに届いた（XX は 8 kHz 未満だけ）
        assert_eq!(templates.len(), 16, "{templates:?}");
        // 1 種類の窓（512/512・1024/1024）と、短い窓・長い窓を持つ設定の両方に届いた
        assert!(shapes.iter().any(|b| b[0] == b[1]) && shapes.iter().any(|b| b[0] < b[1]));
        // 比べた値の数（540 通りで約 71 万）
        assert!(checked > 700_000, "{checked}");
    }

    /// kura の範囲（チャンネル 1〜8・レート 8〜192 kHz のテンプレートの境目・q −1〜10）のすべてで設定が作れ、形が正しい
    /// （照合用の C がなくても回る。表の添字が範囲の外に出ないことの確かめも兼ねる）。
    #[test]
    fn every_supported_setting_builds() {
        let rates = [
            8_000, 8_999, 9_000, 9_001, 11_025, 15_000, 15_001, 16_000, 19_000, 19_001, 22_050,
            26_000, 26_001, 32_000, 40_000, 40_001, 44_100, 45_000, 45_001, 48_000, 50_000, 50_001,
            96_000, 192_000,
        ];
        for rate in rates {
            for ch in 1..=8usize {
                for q in -1..=10 {
                    let vi = setup_of(rate, ch, q);
                    let ci = &vi.codec_setup;
                    let ctx = format!("{rate} Hz・{ch} ch・q{q}");
                    let t = ci.hi.setup.unwrap();
                    assert_eq!(
                        t.coupling_restriction,
                        if ch == 2 { 2 } else { -1 },
                        "{ctx}"
                    );
                    assert!(i64::from(rate) >= t.samplerate_min_restriction);
                    assert!(i64::from(rate) <= t.samplerate_max_restriction);
                    let [s, l] = ci.blocksizes;
                    assert!(s.count_ones() == 1 && l.count_ones() == 1, "{ctx}");
                    assert!(64 <= s && s <= l && l <= 8192, "{ctx}");
                    let single = s == l;
                    assert_eq!(ci.modes, if single { 1 } else { 2 }, "{ctx}");
                    assert_eq!(ci.maps, ci.modes, "{ctx}");
                    assert_eq!(ci.psys, if single { 2 } else { 4 }, "{ctx}");
                    assert_eq!(ci.psy_param.len(), ci.psys as usize, "{ctx}");
                    assert_eq!(ci.book_param.len(), ci.books as usize, "{ctx}");
                    assert!(ci.books > 0 && ci.books <= 256, "{ctx}");
                    // 識別ヘッダの値
                    assert_eq!(
                        (vi.channels as usize, vi.rate),
                        (ch, i64::from(rate)),
                        "{ctx}"
                    );
                    assert_eq!(
                        (vi.bitrate_lower, vi.bitrate_upper, vi.bitrate_window),
                        (0, 0, 0)
                    );
                    // 品質は (float)q/10 + 1e-7（q10 は .9999）
                    let want = if q == 10 { 0.9999 } else { f64::from(q) / 10.0 };
                    assert!((f64::from(ci.hi.req) - want).abs() < 1e-6, "{ctx}");
                    assert!(
                        ci.hi.base_setting >= 0.0 && ci.hi.base_setting < f64::from(t.mappings)
                    );
                    // floor：book の番号は範囲の中、n は窓の半分まで
                    for f in &ci.floor_param {
                        let parts = f.partitions as usize;
                        let classes = f.partitionclass[..parts].iter().max().map_or(0, |&m| m + 1);
                        for k in 0..classes as usize {
                            assert!(f.class_book[k] >= -1 && f.class_book[k] < ci.books, "{ctx}");
                            for &b in &f.class_subbook[k][..1 << f.class_subs[k]] {
                                assert!(b >= -1 && b < ci.books, "{ctx}");
                            }
                        }
                    }
                    for (k, f) in ci.floor_param.iter().enumerate().take(ci.modes as usize) {
                        assert!(f.n > 0 && i64::from(f.n) <= ci.blocksizes[k] / 2, "{ctx}");
                    }
                    // residue：種類（2 ch は residue 2、ほかは 1）、end は grouping の倍数で窓の中、book は範囲の中
                    for (k, r) in ci.residue_param.iter().enumerate() {
                        let ty = ci.residue_type[k];
                        assert_eq!(ty, if ch == 2 { 2 } else { 1 }, "{ctx}");
                        let span = ci.blocksizes[k] / 2 * if ty == 2 { ch as i64 } else { 1 };
                        assert!(r.end > 0 && r.end <= span, "{ctx}");
                        assert_eq!(r.end % i64::from(r.grouping), 0, "{ctx}");
                        assert!(r.groupbook >= 0 && r.groupbook < ci.books, "{ctx}");
                        let n: u32 = r.secondstages[..r.partitions as usize]
                            .iter()
                            .map(|&s| (s as u32).count_ones())
                            .sum();
                        assert!(
                            r.booklist[..n as usize]
                                .iter()
                                .all(|&b| b >= 0 && b < ci.books),
                            "{ctx}"
                        );
                        assert!(
                            i64::from(ci.block_lowpassr[k]) <= ci.blocksizes[k] / 2,
                            "{ctx}"
                        );
                    }
                    // mapping：2 ch は 1 段の coupling（0 と 1）
                    for m in &ci.map_param {
                        assert_eq!(m.coupling_steps, i32::from(ch == 2), "{ctx}");
                        assert_eq!(m.submaps, 1, "{ctx}");
                    }
                }
            }
        }
    }

    /// 既知の値：44.1 kHz ステレオの q5（照合用の `oracle_setup 44100 2 5 --bits` の値）ほか。
    #[test]
    fn known_values() {
        let vi = setup_of(44_100, 2, 5);
        let ci = &vi.codec_setup;
        assert!(std::ptr::eq(
            ci.hi.setup.unwrap(),
            &vorbis_tables::VE_SETUP_44_STEREO
        ));
        assert_eq!(ci.blocksizes, [256, 2048]);
        assert_eq!(vi.bitrate_nominal, 160_000);
        assert_eq!(ci.block_lowpassr, [120, 912]);
        // base_setting は整数にならない（float の j + del）：7 + 2^-20
        assert_eq!(ci.hi.base_setting.to_bits(), 0x401c_0000_4000_0000);
        assert_eq!(ci.hi.req.to_bits(), 0x3f00_0002);
        assert_eq!(ci.hi.lowpass_khz.to_bits(), 0x4033_8001_c800_0000);
        assert_eq!(
            (ci.modes, ci.maps, ci.floors, ci.residues, ci.psys),
            (2, 2, 2, 2, 4)
        );
        assert_eq!(ci.residue_param[0].end, 240);
        assert_eq!(ci.residue_param[1].end, 1824);
        assert_eq!(ci.floor_param[0].n, 113);
        assert_eq!(ci.floor_param[1].n, 905);
        assert_eq!(
            BOOKS[usize::from(ci.book_param[0].0)].name,
            "_huff_book_line_128x17_class1"
        );
        // ほかのテンプレートの名目のビットレート（X は rate_mapping が −1 なので負や 0）と窓
        assert_eq!(setup_of(96_000, 6, -1).bitrate_nominal, -6);
        assert_eq!(setup_of(192_000, 1, 10).bitrate_nominal, 0);
        assert_eq!(setup_of(8_000, 1, 10).bitrate_nominal, 41_997);
        assert_eq!(setup_of(8_000, 6, -1).bitrate_nominal, 60_000);
        assert_eq!(setup_of(16_000, 6, -1).codec_setup.blocksizes, [1024, 1024]);
        assert_eq!(setup_of(96_000, 6, -1).codec_setup.blocksizes, [512, 4096]);
    }

    /// 同じ引数なら同じ設定（決定性）。範囲の外は `Unsupported`。
    #[test]
    fn deterministic_and_rejects_out_of_range() {
        for (rate, ch, q) in [
            (44_100, 2, 5),
            (8_000, 1, -1),
            (192_000, 8, 10),
            (22_050, 3, 0),
        ] {
            let a = setup_of(rate, ch, q);
            let b = setup_of(rate, ch, q);
            assert_eq!(format!("{a:?}"), format!("{b:?}"));
        }
        for (ch, rate) in [(2, 0), (2, -1), (0, 44_100), (256, 44_100), (2, 250_000)] {
            let e = vorbis_encode_init_vbr(ch, rate, 0.5).unwrap_err();
            assert!(
                matches!(e, EngineError::Unsupported(_)),
                "{ch} ch・{rate} Hz：{e:?}"
            );
        }
        // 品質の上は .9999 に止まる（q10 と同じ）
        let a = vorbis_encode_init_vbr(2, 44_100, 1.5).unwrap();
        let b = setup_of(44_100, 2, 10);
        assert_eq!(
            a.codec_setup.hi.req.to_bits(),
            (0.9999_f64 as f32).to_bits()
        );
        assert_eq!(
            a.codec_setup.hi.base_setting.to_bits(),
            b.codec_setup.hi.base_setting.to_bits()
        );
        // 品質の下（−0.2 より下）に合うテンプレートはない
        assert!(vorbis_encode_init_vbr(2, 44_100, -0.5).is_err());
    }

    /// `get_setup_template` の端：品質が表の最後の点にちょうど当たると `j - .001`、最初の点なら 0。
    #[test]
    fn setup_template_edges() {
        let (t, base) = get_setup_template(2, 44_100, 1.0).unwrap();
        assert!(std::ptr::eq(t, &vorbis_tables::VE_SETUP_44_STEREO));
        assert_eq!(base.to_bits(), (12.0_f64 - 0.001).to_bits());
        // 8 kHz の表は −0.2・−0.1・0・1。表の点は float に丸めてから引くので、ちょうど点に当たっても 0 にならない
        // （double の −0.2 と float の −0.2 の差 ÷ float の幅）。2 つ目の点は j = 1 で、float の足し算 1 + del が 1 に丸まる
        let (_, base) = get_setup_template(1, 8_000, -0.2).unwrap();
        let low = -0.2_f64 as f32;
        let del = ((-0.2 - f64::from(low)) / f64::from(-0.1_f64 as f32 - low)) as f32;
        assert!(base > 0.0 && base < 1e-6);
        assert_eq!(base.to_bits(), f64::from(del).to_bits());
        let (_, base) = get_setup_template(1, 8_000, -0.1).unwrap();
        assert_eq!(base, 1.0);
        assert!(get_setup_template(1, 8_000, -0.2001).is_none());
        assert!(get_setup_template(2, 44_100, 1.0001).is_none());
        // 6 ch は uncoupled（5.1 のテンプレートは表にない）
        let (t, _) = get_setup_template(6, 44_100, 0.5).unwrap();
        assert!(std::ptr::eq(t, &vorbis_tables::VE_SETUP_44_UNCOUPLED));
    }
}
