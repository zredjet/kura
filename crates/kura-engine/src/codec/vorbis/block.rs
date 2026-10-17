//! dsp の状態・入力・前後の外挿・ブロックの切り出し・グラニュール（`lib/block.c`・`lib/analysis.c`・`lib/bitrate.c`）。
//!
//! # 移す C
//!
//! | 関数 | ファイル:行 | 備考 |
//! |---|---|---|
//! | `_vds_shared_init` | block.c:181–303 | encode の枝（`encp = 1`）：`modebits = ov_ilog(modes-1)`、MDCT の look 2 つ、`window[k] = ov_ilog(blocksizes[k]) - 7`、FFT の look 2 つ（`drft_init(blocksizes[k])`）、`fullbooks`（全冊 `vorbis_book_init_encode`）、psy の look（`_vp_psy_init(psy_param[i], psy_g_param, blocksizes[blockflag]/2, rate)`）、`pcm_storage = blocksizes[1]`、`lW = W = 0`、`centerW = blocksizes[1]/2`、`pcm_current = centerW`、floor・residue の look。`halfrate_flag` は 0 |
//! | `vorbis_analysis_init` | block.c:306–330 | 上と `psy_g_look`、envelope の look（envelope.rs）、`sequence = 3`。aoTuV の mblock・tblock・lownoise_compand_level・impadnum（314–317。すべて 0）は dsp の外の `mapping0::AotuvState::new`（並列の書き出しは dsp を共有して読むため）。`vorbis_bitrate_init` は VBR では何もしない |
//! | `vorbis_block_init` | block.c:84–107 | `ampmax = -9999`。packetblob は VBR では `opb` の 1 つだけ |
//! | `vorbis_analysis_buffer` | block.c:412–437 | `pcm_current + vals >= pcm_storage` なら `pcm_storage = pcm_current + vals*2` に広げる（値は変わらないが、`_preextrapolate_helper` の作業長には効かない） |
//! | `_preextrapolate_helper` | block.c:439–485 | `preextrapolate = 1`。`pcm_current - centerW > 32` なら、チャンネルごとに `pcm[0..pcm_current]` を逆順にした work で `vorbis_lpc_from_data(work, lpc, pcm_current - centerW, 16)`、`vorbis_lpc_predict(lpc, work + pcm_current - centerW - 16, 16, work + pcm_current - centerW, centerW)`（prime と data は同じ配列の重なる場所。lpc.rs は prime を先に写す）、逆順に戻す。**作業長は `pcm_current`（そのときまでに渡した量）**：一度に渡す形では全体 |
//! | `vorbis_analysis_wrote` | block.c:490–561 | **前増幅（495–503）は移さない**。`vals <= 0`：まだなら `_preextrapolate_helper`、`blocksizes[1]*3` 広げて `eofflag = pcm_current`、チャンネルごとに `eofflag > 64` なら `n = min(eofflag, blocksizes[1])` 個の末尾から 32 次の LPC で `pcm_current - eofflag` 個を外挿、でなければ 0。`vals > 0`：`pcm_current += vals`、`pcm_current - centerW > blocksizes[1]` で初めて `_preextrapolate_helper` |
//! | `vorbis_analysis_blockout` | block.c:565–820 | `_ve_envelope_search` で nW（−1 で eof でなければ 0 を返す＝まだ）、`centerNext`、`blockbound` の確認、blocktype（長い窓：前後のどちらかが短ければ transition、両方長ければ long。短い窓：`_ve_envelope_mark` で impulse か padding）、`sequence`・`granulepos`・`pcmend`、**ampmax**（`if(vbi->ampmax > g->ampmax) g->ampmax = vbi->ampmax; g->ampmax = _vp_ampmax_decay(g->ampmax, v); vbi->ampmax = g->ampmax`。657–659。前のブロックの mapping0 が残した値を引き継ぐスカラーの再帰）、PCM を写す（`pcm[beginW .. beginW + pcmend]`。697 の `#else`）、eof の判定（712–718）、`movementW` だけずらす（envelope も。794–795 の `#else`）、lW・W・centerW、**granulepos**（803–814：eof の近くでは詰め物を数えない） |
//! | `vorbis_analysis` | analysis.c:29–63 | blob を空にして `mapping0_forward`、パケット（`e_o_s = vb->eofflag`、`granulepos = vb->granulepos`、`packetno = vb->sequence`） |
//! | `vorbis_bitrate_addblock`・`flushpacket` | bitrate.c:73–96・229–252 | VBR（`managed == 0`）では「今のブロックのパケットをそのまま出す」だけ。`vorbis_analysis` の戻りをそのまま使えばよい（移さない） |
//!
//! - グラニュール：最初の音声パケットは 0、以降は前の中心からの増え（`movementW`）、最後は入力の長さ N（詰め物を数えない）。
//!   Ogg のページのグラニュールは codec/ogg.rs が A7 で付ける（この値と同じになる）。
//! - 一度に渡す形：`vorbis_analysis_buffer(N)` に全入力を書いて `wrote(N)`、`wrote(0)`、それから blockout を 1 が返る間回す
//!   （mod.rs の照合用のエンコーダの 4）。ずらし（`memmove`・`_ve_envelope_shift`）は位置の数を C と同じに保てば、絶対の位置で
//!   持ってもよい（そのときは pcm を動かさずに `beginW` などを足し直す）。
//!
//! # 移さないもの
//!
//! デコーダ（`vorbis_synthesis_*`）、`_vorbis_block_alloc`・`_vorbis_block_ripcord`（`Vec` で持つ）、`vorbis_dsp_clear`・
//! `vorbis_block_clear`、SSE の枝（666–695・731–792）、ビットレートの管理、ヘッダの保管（`header*`）。
//!
//! # 型の対応
//!
//! - `vorbis_dsp_state`（codec.h:58–86）→ [`VorbisDspState`]（`pcmret`・`pcm_returned`・`*_bits` は持たない）。
//! - `private_state`（codec_internal.h:60–93）→ [`PrivateState`]（`transform[2]` は MDCT の look 2 つ。`header*`・`bms`・
//!   `sample_count` は持たない。aoTuV の欄（85–92：`lownoise_compand_level`・`mblock`・`tblock`・`lW_block_mode`・`lW_no`・
//!   `impadnum`）は書く状態なので `mapping0::AotuvState` に分けた）。
//! - `vorbis_block`（codec.h:88–120）と `vorbis_block_internal`（codec_internal.h:30–39）→ `VorbisBlock`（`pcm` はこの
//!   ブロックの写し、`opb` はパケット、`ampmax`・`blocktype` は internal の欄。`pcmdelay`・packetblob の残り・localstore は持たない）。
//! - `ogg_packet` → `codec::ogg::OggPacket`。
//!
//! # 移植の補足
//!
//! - **pcm は動かさない**：C は blockout のたびに `memmove(pcm, pcm + movementW, pcm_current)` で前に詰める（全体を一度に
//!   渡すと O(N²)）。ここでは動かさずに、C の `pcm[i][k]` を `pcm[i][pcm_base + k]` として読む（`pcm_base` は C がずらした
//!   量の和）。`pcm_current`・`centerW`・`eofflag`・`granulepos` は C と同じ値（C の座標）で持つので、比較・割り算はすべて
//!   C と同じ。C の memmove の後ろの残り（古い値）は読まれない（`pcm_current` より後ろは読む前に書く）。
//! - `pcm` の大きさは要る分だけ（C の `pcm_storage = pcm_current + vals*2` は数として持ち、`wrote` の確かめにだけ使う）。
//!   C の `int` の欄（`pcm_current` など）があふれる長さは `EngineError::Limit`（C では未定義の動き）。`pcm_storage` は
//!   `vals*2` が `int` に収まらないとき `i32::MAX` に止める（C では未定義。値には効かない）。
//! - `_vds_shared_init` は dsp の部分（[`vds_dsp_init`]：MDCT・窓・FFT・pcm・位置）と、codebook・psy・floor・residue の
//!   look（[`vds_backend_init`]）に分けた（順は C と違うが、互いに読まないので結果は同じ）。
//! - C の `vorbis_analysis_init` などの「失敗で 1 を返す」は `EngineError::Internal`。
//! - 照合（このファイルのテスト）は一度に渡す形。分けて渡す形（`oracle_enc --chunk`）も段 7 の移植のときに一時の試験で
//!   6 通り（チャンク 300〜5,000）のブロックの列と PCM0 が一致することを確かめた（`chunked_feeding_is_safe` は panic しない
//!   ことだけを見る）。
//! - ブロックの列の照合は mapping0 の手前まで（BLCK・PCM0・PCMW・MDCT・LFFT・AMPL）。`vbi->ampmax` の再帰（AMPG）を含む
//!   全体は stream_tests.rs の照合（パケットのビット一致）が見る。
//!
//! # 結合で足したもの（並列の書き出しのため。値は C と同じ）
//!
//! - `vorbis_analysis_blockout` を、計画（[`vorbis_analysis_plan`]：PCM の写しと ampmax を除いた部分。envelope・窓の長短・
//!   グラニュール・ずらしは C の順のまま）・ampmax（[`blockout_ampmax`]：657–659）・PCM の写しに分けた。書き出しは計画を
//!   全ブロックについて先に作り、PCM は計画の `start`（`pcm` を動かさないので絶対の位置）から [`block_pcm`] で読む。
//!   C の形の `vorbis_analysis_blockout`・`vorbis_analysis`・`vorbis_block_init`・`VorbisBlock` はテストの基準として残した
//!   （`#[cfg(test)]`。`vorbis_analysis` は aoTuV の状態 `mapping0::AotuvState` も受け取る。並列の書き出しと同じパケットになる
//!   ことを encoder.rs のテストが確かめる）。
//! - `_preextrapolate_helper` の自己相関（入力の全体 × 17 個のずれ）は、チャンネル × ずれごとに `par_blocks`（各ずれの和は
//!   C と同じ順）。逆順の work は作らず pcm を後ろから読む。
//! - 中止（[`Monitor`]）：`vorbis_analysis_wrote` は前の外挿の自己相関の `par_blocks` と、各和の [`CANCEL_SAMPLES`] 標本ごとに、
//!   `vorbis_analysis_plan` は envelope の探索（`ve_envelope_search`）の数千歩ごとに確かめ、頼まれていれば `Canceled` を返す
//!   （確かめるだけで値は変わらない。中止した dsp の状態は捨てる）。入力を一度に渡すので、最初の計画は入力の全体の
//!   envelope を 1 回で求める（長い入力では秒の単位）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use vorbis_tables::ov_ilog;

#[cfg(test)]
use super::bitpack::OggpackBuffer;
use super::codebook::vorbis_book_init_encode;
use super::envelope::{
    EnvelopeLookup, ve_envelope_init, ve_envelope_mark, ve_envelope_search, ve_envelope_shift,
};
use super::floor1::{VorbisLookFloor1, floor1_look};
use super::info::VorbisInfo;
use super::lpc::{lpc_from_autocorrelation, vorbis_lpc_from_data, vorbis_lpc_predict};
#[cfg(test)]
use super::mapping0::{AotuvState, mapping0_forward};
use super::mdct::{MdctLookup, mdct_init};
use super::psy::{
    VorbisLookPsy, VorbisLookPsyGlobal, vp_ampmax_decay_w, vp_global_look, vp_psy_init,
};
use super::res0::{VorbisLookResidue0, res0_look};
use super::smallft::{DrftLookup, drft_init};
#[cfg(test)]
use crate::codec::ogg::OggPacket;
use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::monitor::Monitor;

/// codec_internal.h:23–26。
pub(crate) const BLOCKTYPE_IMPULSE: i32 = 0;
/// codec_internal.h:24。
pub(crate) const BLOCKTYPE_PADDING: i32 = 1;
/// codec_internal.h:25。
pub(crate) const BLOCKTYPE_TRANSITION: i32 = 0;
/// codec_internal.h:26。
pub(crate) const BLOCKTYPE_LONG: i32 = 1;

/// `private_state`（codec_internal.h:60–93）のエンコードに要る欄。
#[derive(Debug, Clone, Default)]
pub(crate) struct PrivateState {
    pub ve: EnvelopeLookup,
    pub window: [i32; 2],
    /// C の `transform[2]`（MDCT の look。短・長）。
    pub transform: [MdctLookup; 2],
    pub fft_look: [DrftLookup; 2],
    pub modebits: i32,
    pub flr: Vec<VorbisLookFloor1>,
    pub residue: Vec<VorbisLookResidue0>,
    /// `psys` 個（0 impulse・1 padding・2 transition・3 long）。
    pub psy: Vec<VorbisLookPsy>,
    pub psy_g_look: VorbisLookPsyGlobal,
}

/// `vorbis_dsp_state`（codec.h:58–86）。
#[derive(Debug, Clone, Default)]
pub(crate) struct VorbisDspState {
    pub vi: VorbisInfo,
    /// [チャンネル][…]。C の `pcm[i][k]` は `pcm[i][pcm_base + k]`（冒頭の「移植の補足」）。
    pub pcm: Vec<Vec<f32>>,
    /// C の `pcm` の先頭の位置（blockout が C で memmove した量の和。kura の追加）。
    pub pcm_base: usize,
    pub pcm_storage: i32,
    pub pcm_current: i32,
    pub preextrapolate: i32,
    pub eofflag: i32,
    pub lw: i64,
    pub w: i64,
    pub nw: i64,
    pub center_w: i64,
    pub granulepos: i64,
    pub sequence: i64,
    pub backend_state: PrivateState,
}

/// `vorbis_block_init` の `vbi->ampmax`（block.c:99）。並列の書き出しの ampmax の再帰の始まり。
pub(crate) const VBI_AMPMAX_INIT: f32 = -9999.0;

/// `vorbis_block`（codec.h:88–120）と `vorbis_block_internal`（codec_internal.h:30–39）。直列の C の形（テストの基準）だけが使う。
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct VorbisBlock {
    /// このブロックの PCM（[チャンネル][pcmend]。mapping0 が窓・FFT をその場で書く）。
    pub pcm: Vec<Vec<f32>>,
    /// パケット（VBR の blob）。
    pub opb: OggpackBuffer,
    pub lw: i64,
    pub w: i64,
    pub nw: i64,
    pub pcmend: i32,
    pub mode: i32,
    pub eofflag: i32,
    pub granulepos: i64,
    pub sequence: i64,
    /// internal：前のブロックから引き継ぐ最大の振幅（dB）。
    pub ampmax: f32,
    /// internal：BLOCKTYPE_*。
    pub blocktype: i32,
}

/// `blocksizes[W]`（W は 0 か 1）。
fn bs_at(bs: &[i64; 2], w: i64) -> i64 {
    bs[usize::from(w != 0)]
}

/// C の `int` の欄に入れる（あふれは C では未定義：長すぎる入力として断る）。
fn to_int(x: i64) -> Result<i32, EngineError> {
    i32::try_from(x).map_err(|_| EngineError::Limit("Ogg Vorbis：入力が長すぎます".into()))
}

/// `_vds_shared_init` の dsp の部分（block.c:187–219・262–279）：大きさの確かめ、modebits、MDCT・窓・FFT の look、pcm、位置。
fn vds_dsp_init(vi: VorbisInfo) -> Result<VorbisDspState, EngineError> {
    let ci = &vi.codec_setup;
    let bs = ci.blocksizes;

    // if(ci==NULL|| ci->modes<=0|| ci->blocksizes[0]<64|| ci->blocksizes[1]<ci->blocksizes[0]) return 1;
    if ci.modes <= 0 || bs[0] < 64 || bs[1] < bs[0] {
        return Err(EngineError::Internal(format!(
            "vorbis_analysis_init：設定が正しくない（modes {}、blocksizes {:?}）",
            ci.modes, bs
        )));
    }
    // kura：窓の表（64〜8192）と FFT（2 の累乗。因数 2・4）が扱える大きさだけ（C では範囲の外の読み取り）
    if bs.iter().any(|&b| b > 8192 || b.count_ones() != 1) {
        return Err(EngineError::Internal(format!(
            "vorbis_analysis_init：ブロックの大きさ {bs:?} は扱えない"
        )));
    }
    let channels = usize::try_from(vi.channels)
        .ok()
        .filter(|&c| c >= 1)
        .ok_or_else(|| {
            EngineError::Internal(format!(
                "vorbis_analysis_init：チャンネルの数 {} が正しくない",
                vi.channels
            ))
        })?;
    // hs = ci->halfrate_flag（エンコードでは 0）
    let (b0, b1) = (bs[0] as i32, bs[1] as i32);

    let backend_state = PrivateState {
        modebits: ov_ilog((ci.modes - 1) as u32),
        // MDCT is tranform 0
        transform: [mdct_init(b0), mdct_init(b1)],
        // Vorbis I uses only window type 0
        window: [ov_ilog(b0 as u32) - 7, ov_ilog(b1 as u32) - 7],
        // analysis always needs an fft
        fft_look: [drft_init(b0), drft_init(b1)],
        ..PrivateState::default()
    };

    // initialize the storage vectors
    let pcm_storage = b1;
    let pcm = vec![vec![0.0_f32; pcm_storage as usize]; channels];

    // all vector indexes
    let center_w = bs[1] / 2;

    Ok(VorbisDspState {
        vi,
        pcm,
        pcm_base: 0,
        pcm_storage,
        pcm_current: center_w as i32,
        preextrapolate: 0,
        eofflag: 0,
        // all 1 (large block) or 0 (small block)
        lw: 0,
        w: 0,
        nw: 0,
        center_w,
        granulepos: 0,
        sequence: 0,
        backend_state,
    })
}

/// `_vds_shared_init` の残り（block.c:227–243・281–291）：codebook（fullbooks）・psy・floor・residue の look。
fn vds_backend_init(v: &mut VorbisDspState) -> Result<(), EngineError> {
    let rate = v.vi.rate;
    let ci = &mut v.vi.codec_setup;
    let books = usize::try_from(ci.books).unwrap_or(0);

    // finish the codebooks
    if ci.fullbooks.is_empty() && books > 0 {
        if ci.book_param.len() < books {
            return Err(EngineError::Internal(
                "vorbis_analysis_init：book_param が足りない".into(),
            ));
        }
        let mut full = Vec::with_capacity(books);
        for &id in &ci.book_param[..books] {
            full.push(vorbis_book_init_encode(id)?);
        }
        ci.fullbooks = full;
    }

    let psys = usize::try_from(ci.psys).unwrap_or(0);
    let floors = usize::try_from(ci.floors).unwrap_or(0);
    let residues = usize::try_from(ci.residues).unwrap_or(0);
    // C は ci->psy_param[i] などを数のぶん読む（足りなければ NULL の参照）
    if ci.psy_param.len() < psys
        || ci.floor_type.len() < floors
        || ci.floor_param.len() < floors
        || ci.residue_type.len() < residues
        || ci.residue_param.len() < residues
    {
        return Err(EngineError::Internal(
            "vorbis_analysis_init：psy・floor・residue の数と中身が食い違う".into(),
        ));
    }

    let mut psy = Vec::with_capacity(psys);
    for p in ci.psy_param.iter().take(psys) {
        let n = usize::try_from(p.blockflag)
            .ok()
            .and_then(|k| ci.blocksizes.get(k))
            .ok_or_else(|| {
                EngineError::Internal(format!(
                    "vorbis_analysis_init：psy の blockflag {} が正しくない",
                    p.blockflag
                ))
            })?;
        psy.push(vp_psy_init(p, &ci.psy_g_param, (n / 2) as i32, rate)?);
    }

    // initialize all the backend lookups
    let mut flr = Vec::with_capacity(floors);
    for (t, info) in ci.floor_type.iter().zip(&ci.floor_param).take(floors) {
        if *t != 1 {
            return Err(EngineError::Internal(format!(
                "vorbis_analysis_init：floor の種類 {t} は扱えない"
            )));
        }
        flr.push(floor1_look(info)?);
    }

    let mut residue = Vec::with_capacity(residues);
    for (t, info) in ci.residue_type.iter().zip(&ci.residue_param).take(residues) {
        // residue 0・1・2 の look はどれも res0_look（res0.c の exportbundle）
        if !(0..=2).contains(t) {
            return Err(EngineError::Internal(format!(
                "vorbis_analysis_init：residue の種類 {t} は扱えない"
            )));
        }
        residue.push(res0_look(info, &ci.fullbooks)?);
    }

    let b = &mut v.backend_state;
    b.psy = psy;
    b.flr = flr;
    b.residue = residue;
    Ok(())
}

/// `_vds_shared_init`（block.c:181–303。encode の枝）。
fn vds_shared_init(vi: VorbisInfo) -> Result<VorbisDspState, EngineError> {
    let mut v = vds_dsp_init(vi)?;
    vds_backend_init(&mut v)?;
    Ok(v)
}

/// `vorbis_analysis_init` の `_vds_shared_init` のあと（block.c:310–327。aoTuV の欄を除く）。
fn analysis_init_tail(v: &mut VorbisDspState) {
    let b = &mut v.backend_state;
    b.psy_g_look = vp_global_look(v.vi.channels);

    // Initialize the envelope state storage
    b.ve = ve_envelope_init(&v.vi);

    // vorbis_bitrate_init は VBR では何もしない

    // compressed audio packets start after the headers with sequence number 3
    v.sequence = 3;
}

/// `vorbis_analysis_init`（block.c:306–330。aoTuV の欄を除く）。
pub(crate) fn vorbis_analysis_init(vi: VorbisInfo) -> Result<VorbisDspState, EngineError> {
    let mut v = vds_shared_init(vi)?;
    analysis_init_tail(&mut v);
    Ok(v)
}

/// `vorbis_block_init`（block.c:84–107）。
#[cfg(test)]
pub(crate) fn vorbis_block_init(v: &VorbisDspState) -> VorbisBlock {
    VorbisBlock {
        pcm: vec![Vec::new(); v.pcm.len()],
        opb: OggpackBuffer::oggpack_writeinit(),
        // vbi->ampmax=-9999;
        ampmax: VBI_AMPMAX_INIT,
        ..VorbisBlock::default()
    }
}

/// `vorbis_analysis_buffer`（block.c:412–437）：`vals` 個を書ける大きさにして、書き始めの位置（`pcm_base + pcm_current`）を返す。
/// 呼ぶ側は `v.pcm[ch][pos .. pos + vals]` に書く。
pub(crate) fn vorbis_analysis_buffer(
    v: &mut VorbisDspState,
    vals: i32,
) -> Result<usize, EngineError> {
    let cur = i64::from(v.pcm_current);
    let vals = i64::from(vals.max(0));

    // Do we have enough storage space for the requested buffer? If not, expand the PCM storage
    if cur + vals >= i64::from(v.pcm_storage) {
        // v->pcm_storage=v->pcm_current+vals*2;（int に収まらなければ止める。値には効かない）
        v.pcm_storage = i32::try_from(cur + vals * 2).unwrap_or(i32::MAX);
    }

    let start = usize::try_from(cur)
        .ok()
        .and_then(|c| c.checked_add(v.pcm_base))
        .ok_or_else(|| EngineError::Internal("vorbis_analysis_buffer：位置が負".into()))?;
    let need = start + vals as usize;
    for p in &mut v.pcm {
        if p.len() < need {
            p.try_reserve_exact(need - p.len())
                .map_err(|_| EngineError::Limit("Ogg Vorbis：入力の領域を確保できません".into()))?;
            p.resize(need, 0.0);
        }
    }
    Ok(start)
}

/// 前の外挿の自己相関の和で中止を確かめる間隔（標本の数。和は区切っても同じ順に足すので値は変わらない）。
pub(crate) const CANCEL_SAMPLES: usize = 1 << 17;

/// `_preextrapolate_helper`（block.c:439–485）。
///
/// C は pcm を逆順に写した work（`pcm_current` 個）で 16 次の LPC を作る。自己相関（17 個のずれ × 入力の全体）が
/// 重いので、チャンネル × ずれごとに `par_blocks` で並列に求める（各ずれの和は C と同じ `i` の昇順。逆順の work は
/// 作らず、pcm を後ろから読む）。値は C と同じ（スレッド数によらない）。中止は `par_blocks` の仕事ごとと、各和の
/// [`CANCEL_SAMPLES`] 標本ごとに確かめる。
fn preextrapolate_helper(v: &mut VorbisDspState, mon: &dyn Monitor) -> Result<(), EngineError> {
    const ORDER: usize = 16;
    v.preextrapolate = 1;

    let cur = i64::from(v.pcm_current);
    if cur - v.center_w > (ORDER * 2) as i64 {
        let cur = cur as usize;
        let center_w = v.center_w as usize;
        let n = cur - center_w;
        let base = v.pcm_base;
        if v.pcm.iter().any(|p| p.len() < base + cur) {
            return Err(EngineError::Internal(
                "_preextrapolate_helper：pcm が足りない".into(),
            ));
        }
        let channels = v.pcm.len();

        // work[i] = pcm[cur-1-i]（i < n）は data = pcm[centerW..cur] の逆順：work[i] = data[n-1-i]。
        // aut[j] = Σ_{i=j}^{n-1} work[i]*work[i-j]（i の昇順）= data[n-1-j-t]*data[n-1-t]（t = i-j の昇順）
        // 区切り（rchunks）は 2 つの長さの同じスライスの後ろから同じ位置で切れるので、区切りの中を後ろから読むと、
        // 区切らずに後ろから読むのと同じ順に足す
        let pcm = &v.pcm;
        let aut = par_blocks(channels * (ORDER + 1), mon, |k| {
            let (c, j) = (k / (ORDER + 1), k % (ORDER + 1));
            let data = &pcm[c][base + center_w..base + cur];
            let mut d = 0.0_f64;
            for (ca, cb) in data[..n - j]
                .rchunks(CANCEL_SAMPLES)
                .zip(data[j..].rchunks(CANCEL_SAMPLES))
            {
                if mon.canceled() {
                    return Err(EngineError::Canceled);
                }
                for (a, b) in ca.iter().rev().zip(cb.iter().rev()) {
                    d += f64::from(*a) * f64::from(*b);
                }
            }
            Ok(d)
        })?;

        let mut lpc = [0.0_f32; ORDER];
        for (c, p) in v.pcm.iter_mut().enumerate() {
            let p = &mut p[base..base + cur];

            // prime as above（vorbis_lpc_from_data(work,lpc,cur-centerW,order) の自己相関のあと）
            lpc_from_autocorrelation(&aut[c * (ORDER + 1)..(c + 1) * (ORDER + 1)], &mut lpc);

            // run the predictor filter：prime は work[n-order..n] = pcm[cur-1-(n-order)..] を後ろへ読んだもの
            let prime: Vec<f32> = (0..ORDER).map(|t| p[cur - 1 - (n - ORDER + t)]).collect();
            let mut tail = vec![0.0_f32; center_w];
            vorbis_lpc_predict(&lpc, Some(&prime), ORDER as i32, &mut tail, center_w as i64);

            // for(j=0;j<v->pcm_current;j++) v->pcm[i][v->pcm_current-j-1]=work[j];
            // （work[0..n] は pcm を逆に写しただけなので、書き戻して変わるのは pcm[0..centerW] だけ）
            for (k, w) in tail.iter().enumerate() {
                p[cur - (n + k) - 1] = *w;
            }
        }
    }
    Ok(())
}

/// `vorbis_analysis_wrote`（block.c:490–561。前増幅を除く）：`vals` 個を書いた（0 以下は終わり）。前の外挿の中で `mon` の
/// 中止を確かめる（冒頭）。
pub(crate) fn vorbis_analysis_wrote(
    v: &mut VorbisDspState,
    vals: i32,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    let bs1 = v.vi.codec_setup.blocksizes[1];

    // pre-amplitude（495–503）は移さない（PREAMP を切った）

    if vals <= 0 {
        const ORDER: i32 = 32;
        let mut lpc = [0.0_f32; ORDER as usize];

        // if it wasn't done earlier (very short sample)
        if v.preextrapolate == 0 {
            preextrapolate_helper(v, mon)?;
        }

        // We're encoding the end of the stream.  Just make sure we have [at least] a few full blocks of zeroes at the end.
        // actually, we don't want zeroes; ... Extrapolate for the sake of cleanliness.
        let extra = to_int(bs1 * 3)?;
        vorbis_analysis_buffer(v, extra)?;
        v.eofflag = v.pcm_current;
        v.pcm_current = to_int(i64::from(v.pcm_current) + i64::from(extra))?;

        let base = v.pcm_base;
        let eof = v.eofflag as usize;
        let cur = v.pcm_current as usize;
        for p in &mut v.pcm {
            let p = &mut p[base..base + cur];
            if v.eofflag > ORDER * 2 {
                // make a predictor filter
                let mut n = i64::from(v.eofflag);
                if n > bs1 {
                    n = bs1;
                }
                let n = n as usize;
                vorbis_lpc_from_data(&p[eof - n..eof], &mut lpc, n as i32, ORDER);

                // run the predictor filter
                let (head, tail) = p.split_at_mut(eof);
                vorbis_lpc_predict(
                    &lpc,
                    Some(&head[eof - ORDER as usize..]),
                    ORDER,
                    tail,
                    (cur - eof) as i64,
                );
            } else {
                // not enough data to extrapolate ... zeroes will do.
                for x in &mut p[eof..cur] {
                    *x = 0.0;
                }
            }
        }
    } else {
        if i64::from(v.pcm_current) + i64::from(vals) > i64::from(v.pcm_storage) {
            // return(OV_EINVAL);
            return Err(EngineError::Internal(
                "vorbis_analysis_wrote：buffer で確保した数より多い".into(),
            ));
        }

        v.pcm_current += vals;
        // pcm は buffer が求めた分だけ確保している（C は pcm_storage まで確保する）。それを越えて wrote されたら、
        // C の未初期化の領域の代わりに 0 で広げる（buffer に書いたあとで wrote する正しい使い方では起きない）。
        let need = v.pcm_base + v.pcm_current as usize;
        for p in &mut v.pcm {
            if p.len() < need {
                p.resize(need, 0.0);
            }
        }

        // we may want to reverse extrapolate the beginning of a stream too... in case we're beginning on a cliff!
        if v.preextrapolate == 0 && i64::from(v.pcm_current) - v.center_w > bs1 {
            preextrapolate_helper(v, mon)?;
        }
    }
    Ok(())
}

/// ブロックの計画：`vorbis_analysis_blockout` が `vorbis_block` に書く欄のうち、PCM の写しと ampmax を除いたもの
/// （kura の追加。並列の書き出しは、先に全ブロックの計画を直列に作り、PCM は `start` から読む）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BlockPlan {
    pub lw: i64,
    pub w: i64,
    pub nw: i64,
    /// BLOCKTYPE_*。
    pub blocktype: i32,
    pub granulepos: i64,
    pub sequence: i64,
    /// ブロックの大きさ（`blocksizes[W]`）。
    pub pcmend: i32,
    /// 最後のブロックなら 1。
    pub eofflag: i32,
    /// このブロックの PCM の始まり（`VorbisDspState::pcm[ch]` の絶対の位置。C の `pcm + beginW`）。
    pub start: usize,
}

/// `vorbis_analysis_blockout`（block.c:565–820）のうち PCM の写しと ampmax（657–659）を除いたもの：次のブロックの計画
/// （まだ切り出せなければ None）。envelope の探索・窓の長短・グラニュール・ずらしは C と同じ順に行う。envelope の探索の
/// 中で `mon` の中止を確かめる（冒頭）。
pub(crate) fn vorbis_analysis_plan(
    v: &mut VorbisDspState,
    mon: &dyn Monitor,
) -> Result<Option<BlockPlan>, EngineError> {
    let bs = v.vi.codec_setup.blocksizes;
    let begin_w = v.center_w - bs_at(&bs, v.w) / 2;

    // check to see if we're started...
    if v.preextrapolate == 0 {
        return Ok(None);
    }

    // check to see if we're done...
    if v.eofflag == -1 {
        return Ok(None);
    }

    // By our invariant, we have lW, W and centerW set.  Search for the next boundary so we can determine nW
    {
        let bp = ve_envelope_search(v, mon)?;
        if bp == -1 {
            if v.eofflag == 0 {
                return Ok(None); // not enough data currently to search for a full long block
            }
            v.nw = 0;
        } else if bs[0] == bs[1] {
            v.nw = 0;
        } else {
            v.nw = bp;
        }
    }

    let center_next = v.center_w + bs_at(&bs, v.w) / 4 + bs_at(&bs, v.nw) / 4;

    {
        // center of next block + next block maximum right side.
        let blockbound = center_next + bs_at(&bs, v.nw) / 2;
        if i64::from(v.pcm_current) < blockbound {
            return Ok(None); // not enough data yet
        }
    }

    // fill in the block.  Note that for a short window, lW and nW are *short* regardless of actual settings in the stream
    let blocktype = if v.w != 0 {
        if v.lw == 0 || v.nw == 0 {
            BLOCKTYPE_TRANSITION
        } else {
            BLOCKTYPE_LONG
        }
    } else if ve_envelope_mark(v)? {
        BLOCKTYPE_IMPULSE
    } else {
        BLOCKTYPE_PADDING
    };

    let sequence = v.sequence;
    v.sequence += 1;
    let pcmend = bs_at(&bs, v.w);

    // copy the vectors（vb->pcm[i] = pcmdelay[i] + beginW。pcmdelay は pcm の先頭 pcmend + beginW 個の写し）
    let start = usize::try_from(begin_w)
        .ok()
        .and_then(|b| b.checked_add(v.pcm_base))
        .ok_or_else(|| EngineError::Internal("blockout：beginW が負".into()))?;
    let end = start + pcmend as usize;
    if v.pcm.iter().any(|p| p.len() < end) {
        return Err(EngineError::Internal("blockout：pcm が足りない".into()));
    }

    let mut plan = BlockPlan {
        lw: v.lw,
        w: v.w,
        nw: v.nw,
        blocktype,
        granulepos: v.granulepos,
        sequence,
        pcmend: pcmend as i32,
        eofflag: 0,
        start,
    };

    // handle eof detection: eof==0 means that we've not yet received EOF
    //                       eof>0  marks the last 'real' sample in pcm[]
    //                       eof<0  'no more to do'; doesn't get here
    if v.eofflag != 0 && v.center_w >= i64::from(v.eofflag) {
        v.eofflag = -1;
        plan.eofflag = 1;
        return Ok(Some(plan));
    }

    // advance storage vectors and clean up
    {
        let new_center_next = (bs[1] / 2) as i32;
        let movement_w = to_int(center_next - i64::from(new_center_next))?;

        if movement_w > 0 {
            ve_envelope_shift(&mut v.backend_state.ve, i64::from(movement_w))?;
            v.pcm_current -= movement_w;

            // memmove(v->pcm[i],v->pcm[i]+movementW,v->pcm_current*sizeof(*v->pcm[i]));（動かさずに先頭を進める）
            v.pcm_base += movement_w as usize;

            v.lw = v.w;
            v.w = v.nw;
            v.center_w = i64::from(new_center_next);

            if v.eofflag != 0 {
                v.eofflag -= movement_w;
                if v.eofflag <= 0 {
                    v.eofflag = -1;
                }
                // do not add padding to end of stream!
                if v.center_w >= i64::from(v.eofflag) {
                    v.granulepos += i64::from(movement_w) - (v.center_w - i64::from(v.eofflag));
                } else {
                    v.granulepos += i64::from(movement_w);
                }
            } else {
                v.granulepos += i64::from(movement_w);
            }
        }
    }

    // done
    Ok(Some(plan))
}

/// block.c:657–659（this tracks 'strongest peak' for later psychoacoustics）：前のブロックの mapping0 が残した
/// `vbi_ampmax` と大域の `g_ampmax` から、このブロック（窓 `w`）の始めの ampmax（C の新しい `vbi->ampmax`）。`g_ampmax` を進める。
pub(crate) fn blockout_ampmax(g_ampmax: &mut f32, vbi_ampmax: f32, vi: &VorbisInfo, w: i64) -> f32 {
    if vbi_ampmax > *g_ampmax {
        *g_ampmax = vbi_ampmax;
    }
    *g_ampmax = vp_ampmax_decay_w(*g_ampmax, vi, w);
    *g_ampmax
}

/// `vorbis_analysis_blockout`（block.c:565–820）：次のブロックを `vb` に切り出せたら true（[`vorbis_analysis_plan`]・
/// [`blockout_ampmax`]・PCM の写し。C と同じ値）。直列の C の形（テストの基準。並列の書き出しと同じパケットになることを
/// encoder.rs のテストが確かめる）。
#[cfg(test)]
pub(crate) fn vorbis_analysis_blockout(
    v: &mut VorbisDspState,
    vb: &mut VorbisBlock,
) -> Result<bool, EngineError> {
    let Some(p) = vorbis_analysis_plan(v, &crate::monitor::NoMonitor)? else {
        return Ok(false);
    };
    vb.lw = p.lw;
    vb.w = p.w;
    vb.nw = p.nw;
    vb.blocktype = p.blocktype;
    vb.sequence = p.sequence;
    vb.granulepos = p.granulepos;
    vb.pcmend = p.pcmend;
    vb.eofflag = p.eofflag;

    // this tracks 'strongest peak' for later psychoacoustics（blockout の中で C と同じ。W は計画のときの値）
    vb.ampmax = blockout_ampmax(
        &mut v.backend_state.psy_g_look.ampmax,
        vb.ampmax,
        &v.vi,
        p.w,
    );

    // copy the vectors
    vb.pcm.resize(v.pcm.len(), Vec::new());
    for (dst, src) in vb.pcm.iter_mut().zip(&v.pcm) {
        let src = src
            .get(p.start..p.start + p.pcmend as usize)
            .ok_or_else(|| EngineError::Internal("blockout：pcm が足りない".into()))?;
        dst.clear();
        dst.extend_from_slice(src);
    }
    Ok(true)
}

/// 計画 `p` のブロックの PCM の写し（チャンネルごとに `pcmend` 個。mapping0 の段 1 がその場で書き換える）。
pub(crate) fn block_pcm(v: &VorbisDspState, p: &BlockPlan) -> Result<Vec<Vec<f32>>, EngineError> {
    v.pcm
        .iter()
        .map(|src| {
            src.get(p.start..p.start + p.pcmend as usize)
                .map(<[f32]>::to_vec)
                .ok_or_else(|| EngineError::Internal("blockout：pcm が足りない".into()))
        })
        .collect()
}

/// `vorbis_analysis`（analysis.c:29–63）：`vb` を 1 パケットにする（mapping0.rs）。直列の C の形（テストの基準）。`st` は
/// C の `private_state` の aoTuV の欄（[`AotuvState`]。`AotuvState::new(channels)` から始めてブロックの順に渡す）。
#[cfg(test)]
pub(crate) fn vorbis_analysis(
    vb: &mut VorbisBlock,
    v: &VorbisDspState,
    st: &mut AotuvState,
) -> Result<OggPacket, EngineError> {
    // first things first.  Make sure encode is ready
    vb.opb.oggpack_reset();

    // we only have one mapping type (0), and we let the mapping code itself figure out what soft mode to use.
    mapping0_forward(vb, v, st)?;

    Ok(OggPacket {
        packet: vb.opb.oggpack_get_buffer().to_vec(),
        e_o_s: vb.eofflag != 0,
        granulepos: vb.granulepos,
        packetno: vb.sequence, // for sake of completeness
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vorbis::mdct::mdct_forward;
    use crate::codec::vorbis::oracle::{self, Signal, json_f32};
    use crate::codec::vorbis::scales::todb;
    use crate::codec::vorbis::smallft::drft_forward;
    use crate::codec::vorbis::window::{vorbis_apply_window, vorbis_window_get};
    use crate::monitor::NoMonitor;
    use serde_json::Value;

    /// 照合用の設定の JSON から、dsp の部分（blocksizes・modes・psy_global・チャンネル・レート）だけの `VorbisInfo`。
    fn info_from_setup(j: &Value) -> VorbisInfo {
        let mut vi = super::super::info::vorbis_info_init();
        vi.channels = j["vorbis_info"]["channels"].as_i64().unwrap() as i32;
        vi.rate = j["vorbis_info"]["rate"].as_i64().unwrap();
        let ci = &mut vi.codec_setup;
        ci.blocksizes = [
            j["blocksizes"][0].as_i64().unwrap(),
            j["blocksizes"][1].as_i64().unwrap(),
        ];
        ci.modes = j["modes"].as_array().unwrap().len() as i32;
        let g = &j["psy_global"];
        let gi = &mut ci.psy_g_param;
        gi.eighth_octave_lines = g["eighth_octave_lines"].as_i64().unwrap() as i32;
        for k in 0..vorbis_tables::VE_BANDS {
            gi.preecho_thresh[k] = json_f32(&g["preecho_thresh"][k]);
            gi.postecho_thresh[k] = json_f32(&g["postecho_thresh"][k]);
        }
        gi.stretch_penalty = json_f32(&g["stretch_penalty"]);
        gi.preecho_minenergy = json_f32(&g["preecho_minenergy"]);
        gi.ampmax_att_per_sec = json_f32(&g["ampmax_att_per_sec"]);
        vi
    }

    /// dsp の部分だけの `vorbis_analysis_init`（codebook・psy・floor・residue の look は他の班の担当なので作らない）。
    fn dsp_only(vi: VorbisInfo) -> VorbisDspState {
        let mut v = vds_dsp_init(vi).unwrap();
        analysis_init_tail(&mut v);
        v
    }

    /// 全入力を一度に渡す（照合用のエンコーダの 4）。
    fn feed(v: &mut VorbisDspState, planes: &[Vec<f32>]) {
        let frames = planes.first().map_or(0, Vec::len);
        if frames > 0 {
            let at = vorbis_analysis_buffer(v, frames as i32).unwrap();
            for (dst, src) in v.pcm.iter_mut().zip(planes) {
                dst[at..at + frames].copy_from_slice(src);
            }
            vorbis_analysis_wrote(v, frames as i32, &NoMonitor).unwrap();
        }
        vorbis_analysis_wrote(v, 0, &NoMonitor).unwrap();
    }

    /// ブロックの列（mapping0 の前まで）。ampmax の再帰は psy の担当の `_vp_ampmax_decay` を通るので比べない。
    fn blocks(v: &mut VorbisDspState) -> Vec<VorbisBlock> {
        let mut vb = vorbis_block_init(v);
        let mut out = Vec::new();
        while vorbis_analysis_blockout(v, &mut vb).unwrap() {
            out.push(vb.clone());
            assert!(out.len() < 1_000_000);
        }
        out
    }

    fn bits(x: &[f32]) -> Vec<u32> {
        x.iter().map(|v| v.to_bits()).collect()
    }

    /// f32 の列をビットで比べる（NaN は符号を問わない。README §8.3）。
    fn same_bits(a: &[f32], b: &[u32]) -> Option<usize> {
        if a.len() != b.len() {
            return Some(usize::MAX);
        }
        a.iter().zip(b).position(|(x, &y)| {
            let y = f32::from_bits(y);
            !(x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
        })
    }

    /// mapping0.c:792–905 の logfft と local_ampmax（照合のために試験の中で組む）。`pcm` は窓を掛けたもの。
    fn logfft(v: &VorbisDspState, w: usize, pcm: &[f32]) -> (Vec<f32>, f32) {
        let n = pcm.len();
        let scale = 4.0_f32 / n as f32;
        let scale_db = (f64::from(todb(scale)) + 0.345) as f32;
        let mut fft = pcm.to_vec();
        drft_forward(&v.backend_state.fft_look[w], &mut fft).unwrap();
        let mut out = vec![0.0_f32; n / 2];
        out[0] = (f64::from(scale_db + todb(fft[0])) + 0.345) as f32;
        let mut local_ampmax = out[0];
        let mut j = 1;
        while j < n - 1 {
            let temp = fft[j] * fft[j] + fft[j + 1] * fft[j + 1];
            let temp = (f64::from(scale_db + 0.5_f32 * todb(temp)) + 0.345) as f32;
            out[(j + 1) >> 1] = temp;
            if temp > local_ampmax {
                local_ampmax = temp;
            }
            j += 2;
        }
        if local_ampmax > 0.0 {
            local_ampmax = 0.0;
        }
        (out, local_ampmax)
    }

    /// 照合で比べた数（ブロックの種類ごと）。
    #[derive(Debug, Default, Clone, Copy)]
    struct Stats {
        cases: usize,
        blocks: usize,
        /// W = 0 の impulse・padding、W = 1 の transition・long。
        kinds: [usize; 4],
    }

    impl Stats {
        fn add(&mut self, o: Stats) {
            self.cases += o.cases;
            self.blocks += o.blocks;
            for (a, b) in self.kinds.iter_mut().zip(o.kinds) {
                *a += b;
            }
        }
    }

    /// 1 つの入力で、照合用のエンコーダのダンプ（レベル 2）とブロックごとに比べる（照合用のエンコーダがなければ None）。
    fn check_case(rate: u32, ch: usize, q: i32, planes: &[Vec<f32>]) -> Option<Stats> {
        let (_, dump) = oracle::encode(planes, rate, q, Some(2))?;
        let dump = dump.unwrap();
        let setup = oracle::setup(rate, ch, q, false).unwrap();
        let mut v = dsp_only(info_from_setup(&setup));
        feed(&mut v, planes);
        let got = blocks(&mut v);
        let frames = planes.first().map_or(0, Vec::len);
        let tag = format!("rate {rate} ch {ch} q {q} frames {frames}");
        assert_eq!(got.len(), dump.blocks.len(), "{tag}：ブロックの数");
        let bs = v.vi.codec_setup.blocksizes;
        let mut st = Stats {
            cases: 1,
            blocks: got.len(),
            ..Stats::default()
        };
        for (b, vb) in got.iter().enumerate() {
            let info = dump.block_info(b);
            let want = [
                b as i64,
                vb.w,
                vb.lw,
                vb.nw,
                i64::from(vb.blocktype),
                i64::from(vb.blocktype) | (vb.w << 1),
                vb.granulepos,
                vb.sequence,
                i64::from(vb.pcmend),
                i64::from(vb.eofflag),
            ];
            assert_eq!(info, want, "{tag} block {b}：BLCK");
            st.kinds[(i64::from(vb.blocktype) | (vb.w << 1)) as usize] += 1;
            let w = usize::from(vb.w != 0);
            for (c, pcm) in vb.pcm.iter().enumerate() {
                let c16 = c as u16;
                let rec = |t: &str| dump.get(b, t, c16).unwrap().f32_bits();
                assert_eq!(
                    same_bits(pcm, &rec("PCM0")),
                    None,
                    "{tag} block {b} ch {c}：PCM0"
                );
                let mut pw = pcm.clone();
                vorbis_apply_window(&mut pw, &v.backend_state.window, &bs, vb.lw, vb.w, vb.nw);
                assert_eq!(
                    same_bits(&pw, &rec("PCMW")),
                    None,
                    "{tag} block {b} ch {c}：PCMW"
                );
                let mut mdct = vec![0.0_f32; pw.len() / 2];
                mdct_forward(&v.backend_state.transform[w], &pw, &mut mdct);
                assert_eq!(
                    same_bits(&mdct, &rec("MDCT")),
                    None,
                    "{tag} block {b} ch {c}：MDCT"
                );
                let (lf, ampl) = logfft(&v, w, &pw);
                assert_eq!(
                    same_bits(&lf, &rec("LFFT")),
                    None,
                    "{tag} block {b} ch {c}：LFFT"
                );
                assert_eq!(
                    same_bits(&[ampl], &rec("AMPL")),
                    None,
                    "{tag} block {b} ch {c}：AMPL"
                );
            }
        }
        Some(st)
    }

    const RATES: [u32; 9] = [
        8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
    ];
    const QS: [i32; 5] = [-1, 0, 3, 5, 10];
    const CHANNELS: [usize; 5] = [1, 2, 3, 6, 8];
    const SIGNALS: [Signal; 5] = [
        Signal::Music,
        Signal::Impulses,
        Signal::Noise,
        Signal::Sine,
        Signal::Silence,
    ];

    /// 1 つのレートの照合：q ごとにチャンネルの数と入力を回し（すべてのレートで 1・2・3・6・8 チャンネル（2 は stereo、
    /// ほかは uncoupled のテンプレート）と 5 種の入力に届く）、Music と Impulses は毎回入れる（短い窓・長い窓の両方）。
    fn check_rate(ri: usize) {
        let rate = RATES[ri];
        let mut st = Stats::default();
        let frames = if rate >= 96_000 {
            rate as usize * 3 / 10
        } else {
            rate as usize * 7 / 10
        };
        let mut templates = Vec::new();
        for (qi, &q) in QS.iter().enumerate() {
            let ch = CHANNELS[(ri + qi) % CHANNELS.len()];
            let extra = SIGNALS[2 + (ri + qi) % 3];
            for (si, kind) in [Signal::Music, Signal::Impulses, extra]
                .into_iter()
                .enumerate()
            {
                let planes =
                    oracle::signal(kind, rate, ch, frames, (ri * 100 + qi * 10 + si) as u64);
                let Some(s) = check_case(rate, ch, q, &planes) else {
                    return;
                };
                st.add(s);
            }
            let setup = oracle::setup(rate, ch, q, false).unwrap();
            templates.push(format!(
                "q{q}/{ch}ch:{}",
                setup["template"]["name"].as_str().unwrap()
            ));
        }
        eprintln!(
            "照合 rate {rate}：{} 入力・{} ブロック（impulse {}・padding {}・transition {}・long {}）がビット一致。{}",
            st.cases,
            st.blocks,
            st.kinds[0],
            st.kinds[1],
            st.kinds[2],
            st.kinds[3],
            templates.join(" ")
        );
    }

    #[test]
    fn oracle_blocks_8000() {
        check_rate(0);
    }
    #[test]
    fn oracle_blocks_11025() {
        check_rate(1);
    }
    #[test]
    fn oracle_blocks_16000() {
        check_rate(2);
    }
    #[test]
    fn oracle_blocks_22050() {
        check_rate(3);
    }
    #[test]
    fn oracle_blocks_32000() {
        check_rate(4);
    }
    #[test]
    fn oracle_blocks_44100() {
        check_rate(5);
    }
    #[test]
    fn oracle_blocks_48000() {
        check_rate(6);
    }
    #[test]
    fn oracle_blocks_96000() {
        check_rate(7);
    }
    #[test]
    fn oracle_blocks_192000() {
        check_rate(8);
    }

    /// レート × チャンネル × q のすべての組（225 通り）を、短い Music（0.25 秒）で。
    #[test]
    fn oracle_blocks_matrix() {
        let mut st = Stats::default();
        for (ri, &rate) in RATES.iter().enumerate() {
            for &q in &QS {
                for &ch in &CHANNELS {
                    let planes =
                        oracle::signal(Signal::Music, rate, ch, rate as usize / 4, ri as u64 + 77);
                    let Some(s) = check_case(rate, ch, q, &planes) else {
                        return;
                    };
                    st.add(s);
                }
            }
        }
        eprintln!(
            "照合 全組：{} 入力・{} ブロック（impulse {}・padding {}・transition {}・long {}）がビット一致",
            st.cases, st.blocks, st.kinds[0], st.kinds[1], st.kinds[2], st.kinds[3]
        );
    }

    /// 端の長さ（0・1・32・33・64・65・1,000・ちょうど blocksizes[1]・その前後・3,000 フレーム）：外挿の条件
    /// （32 以下は前の外挿なし、eofflag が 64 以下は 0 埋め、blocksizes[1] 以下は wrote(0) で前の外挿）と最後のブロック。
    /// 入力は Music と直流（0.999）。
    #[test]
    fn oracle_blocks_edge_lengths() {
        let mut st = Stats::default();
        for (rate, ch, q) in [
            (44_100, 2, 5),
            (8_000, 1, -1),
            (48_000, 6, 0),
            (192_000, 8, 10),
            (22_050, 3, 3),
            (16_000, 2, 10),
        ] {
            let Some(setup) = oracle::setup(rate, ch, q, false) else {
                return;
            };
            let bs1 = setup["blocksizes"][1].as_i64().unwrap() as usize;
            for frames in [0, 1, 32, 33, 64, 65, 1000, bs1 - 1, bs1, bs1 + 1, 3000] {
                let planes = oracle::signal(Signal::Music, rate, ch, frames, frames as u64);
                st.add(check_case(rate, ch, q, &planes).unwrap());
                let dc: Vec<Vec<f32>> = (0..ch).map(|_| vec![0.999_f32; frames]).collect();
                st.add(check_case(rate, ch, q, &dc).unwrap());
            }
        }
        eprintln!(
            "照合 端の長さ：{} 入力・{} ブロックがビット一致",
            st.cases, st.blocks
        );
    }

    /// 照合用のエンコーダの設定（--look）の導出の表：窓・MDCT・FFT・envelope がビット一致する。
    #[test]
    fn oracle_look_tables() {
        let mut configs = 0;
        let mut sizes = std::collections::BTreeSet::new();
        for &rate in &RATES {
            for &q in &QS {
                for &ch in &CHANNELS {
                    let Some(j) = oracle::setup(rate, ch, q, true) else {
                        return;
                    };
                    let tag = format!("rate {rate} ch {ch} q {q}");
                    let v = dsp_only(info_from_setup(&j));
                    let b = &v.backend_state;
                    let look = &j["look"];
                    let f32s = |x: &Value| -> Vec<u32> {
                        x.as_array()
                            .unwrap()
                            .iter()
                            .map(|e| json_f32(e).to_bits())
                            .collect()
                    };
                    let i64s = |x: &Value| -> Vec<i64> {
                        x.as_array()
                            .unwrap()
                            .iter()
                            .map(|e| e.as_i64().unwrap())
                            .collect()
                    };
                    assert_eq!(
                        i64s(&look["window_type"]),
                        b.window.map(i64::from).to_vec(),
                        "{tag}"
                    );
                    assert_eq!(
                        look["modebits"].as_i64().unwrap(),
                        i64::from(b.modebits),
                        "{tag}"
                    );
                    for w in 0..2 {
                        assert_eq!(
                            f32s(&look["window"][w]),
                            bits(vorbis_window_get(b.window[w])),
                            "{tag} window {w}"
                        );
                        let m = &look["mdct"][w];
                        check_mdct(m, &b.transform[w], &format!("{tag} mdct {w}"));
                        let d = &look["drft"][w];
                        assert_eq!(
                            d["n"].as_i64().unwrap(),
                            i64::from(b.fft_look[w].n),
                            "{tag}"
                        );
                        assert_eq!(
                            f32s(&d["trigcache"]),
                            bits(&b.fft_look[w].trigcache),
                            "{tag} drft {w}"
                        );
                        assert_eq!(
                            i64s(&d["splitcache"]),
                            b.fft_look[w]
                                .splitcache
                                .iter()
                                .map(|&x| i64::from(x))
                                .collect::<Vec<_>>(),
                            "{tag} drft {w}"
                        );
                    }
                    let e = &look["envelope"];
                    let ve = &b.ve;
                    assert_eq!(e["ch"].as_i64().unwrap(), i64::from(ve.ch), "{tag}");
                    assert_eq!(
                        e["winlength"].as_i64().unwrap(),
                        i64::from(ve.winlength),
                        "{tag}"
                    );
                    assert_eq!(
                        e["searchstep"].as_i64().unwrap(),
                        i64::from(ve.searchstep),
                        "{tag}"
                    );
                    assert_eq!(
                        json_f32(&e["minenergy"]).to_bits(),
                        ve.minenergy.to_bits(),
                        "{tag}"
                    );
                    assert_eq!(e["storage"].as_i64().unwrap(), ve.storage, "{tag}");
                    assert_eq!(e["cursor"].as_i64().unwrap(), ve.cursor, "{tag}");
                    check_mdct(&e["mdct"], &ve.mdct, &format!("{tag} envelope mdct"));
                    assert_eq!(f32s(&e["mdct_win"]), bits(&ve.mdct_win), "{tag} mdct_win");
                    for (k, band) in ve.band.iter().enumerate() {
                        let jb = &e["band"][k];
                        assert_eq!(
                            jb["begin"].as_i64().unwrap(),
                            i64::from(band.begin),
                            "{tag}"
                        );
                        assert_eq!(jb["end"].as_i64().unwrap(), i64::from(band.end), "{tag}");
                        assert_eq!(f32s(&jb["window"]), bits(&band.window), "{tag} band {k}");
                        assert_eq!(
                            json_f32(&jb["total"]).to_bits(),
                            band.total.to_bits(),
                            "{tag} band {k}"
                        );
                    }
                    configs += 1;
                    sizes.insert(v.vi.codec_setup.blocksizes);
                }
            }
        }
        eprintln!(
            "照合 look：{configs} 設定（ブロックの大きさ {sizes:?}）の窓・MDCT・FFT・envelope の表がビット一致"
        );
    }

    fn check_mdct(m: &Value, l: &MdctLookup, tag: &str) {
        assert_eq!(m["n"].as_i64().unwrap(), i64::from(l.n), "{tag}");
        assert_eq!(m["log2n"].as_i64().unwrap(), i64::from(l.log2n), "{tag}");
        let trig: Vec<u32> = m["trig"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| json_f32(e).to_bits())
            .collect();
        assert_eq!(trig, bits(&l.trig), "{tag} trig");
        let bitrev: Vec<i64> = m["bitrev"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_i64().unwrap())
            .collect();
        assert_eq!(
            bitrev,
            l.bitrev.iter().map(|&x| i64::from(x)).collect::<Vec<_>>(),
            "{tag} bitrev"
        );
        assert_eq!(
            json_f32(&m["scale"]).to_bits(),
            l.scale.to_bits(),
            "{tag} scale"
        );
    }

    // ---- 照合用の C がなくても回る性質のテスト ----

    fn test_info(rate: i64, ch: i32, bs: [i64; 2]) -> VorbisInfo {
        let mut vi = super::super::info::vorbis_info_init();
        vi.channels = ch;
        vi.rate = rate;
        let ci = &mut vi.codec_setup;
        ci.blocksizes = bs;
        ci.modes = 2;
        // 44.1 kHz の q5 の psy_global（照合用の設定の値を写したもの）
        ci.psy_g_param.preecho_thresh = [
            f32::from_bits(0x411f_ffff),
            9.0,
            9.0,
            f32::from_bits(0x40ff_fffe),
            10.0,
            10.0,
            10.0,
            10.0,
            10.0,
            10.0,
            10.0,
            10.0,
        ];
        ci.psy_g_param.postecho_thresh = [
            -20.0,
            -17.5,
            f32::from_bits(0xc157_ffff),
            f32::from_bits(0xc157_ffff),
            -15.0,
            -15.0,
            -15.0,
            -15.0,
            -15.0,
            -20.0,
            -20.0,
            -20.0,
        ];
        ci.psy_g_param.stretch_penalty = 0.0;
        ci.psy_g_param.preecho_minenergy = -80.0;
        ci.psy_g_param.ampmax_att_per_sec = -6.0;
        vi
    }

    fn run(vi: VorbisInfo, planes: &[Vec<f32>]) -> Vec<VorbisBlock> {
        let mut v = dsp_only(vi);
        feed(&mut v, planes);
        blocks(&mut v)
    }

    /// 形：グラニュールは 0 から始まり、増えは前後の窓の 1/4 の和で、最後は入力の長さ。ブロックの大きさ・種類・packetno・eof。
    #[test]
    fn blocks_cover_the_input() {
        for (frames, bs) in [
            (0usize, [256_i64, 2048]),
            (1, [256, 2048]),
            (100, [512, 512]),
            (2048, [256, 2048]),
            (30_000, [256, 2048]),
            (30_000, [1024, 1024]),
        ] {
            let planes = oracle::signal(Signal::Music, 44_100, 2, frames, 7);
            let got = run(test_info(44_100, 2, bs), &planes);
            assert!(!got.is_empty());
            assert_eq!(got[0].granulepos, 0);
            assert_eq!(got[0].w, 0);
            assert_eq!(got[0].lw, 0);
            for (k, b) in got.iter().enumerate() {
                assert_eq!(b.sequence, 3 + k as i64);
                assert_eq!(i64::from(b.pcmend), bs_at(&bs, b.w));
                assert!(b.pcm.iter().all(|p| p.len() == b.pcmend as usize));
                assert_eq!(b.eofflag, i32::from(k + 1 == got.len()));
                if b.w != 0 {
                    let want = if b.lw == 0 || b.nw == 0 {
                        BLOCKTYPE_TRANSITION
                    } else {
                        BLOCKTYPE_LONG
                    };
                    assert_eq!(b.blocktype, want);
                } else {
                    assert!(b.blocktype == BLOCKTYPE_IMPULSE || b.blocktype == BLOCKTYPE_PADDING);
                }
                if k > 0 {
                    let prev = &got[k - 1];
                    // 前のブロックの nW がこのブロックの W、前の W が lW
                    assert_eq!(b.w, prev.nw);
                    assert_eq!(b.lw, prev.w);
                    let step = bs_at(&bs, prev.w) / 4 + bs_at(&bs, prev.nw) / 4;
                    if k + 1 < got.len() {
                        assert_eq!(b.granulepos - prev.granulepos, step);
                    } else {
                        assert!(b.granulepos - prev.granulepos <= step);
                    }
                }
            }
            assert_eq!(
                got.last().unwrap().granulepos,
                frames as i64,
                "frames {frames}"
            );
        }
    }

    /// 1 回目の前の外挿は入力の全体に依存し、ブロックの PCM は入力の値そのもの（ずらしても同じ位置の値）を写す。
    #[test]
    fn block_pcm_is_the_input_at_the_right_place() {
        let frames = 20_000;
        let planes = oracle::signal(Signal::Noise, 44_100, 1, frames, 11);
        let bs = [256_i64, 2048];
        let got = run(test_info(44_100, 1, bs), &planes);
        // ブロック k の中心は（C の座標で）centerW。入力の位置 = granulepos（最初は 0）からの増えで追える：
        // ブロックの左端の入力の位置 = 中心 - pcmend/2 - centerW（centerW = bs1/2 は外挿の分）
        let mut center = 0_i64; // 入力の位置で数えた中心（最初のブロックの中心は入力の 0）
        for (k, b) in got.iter().enumerate() {
            let left = center - i64::from(b.pcmend) / 2;
            for (i, &x) in b.pcm[0].iter().enumerate() {
                let at = left + i as i64;
                if (0..frames as i64).contains(&at) {
                    assert_eq!(
                        x.to_bits(),
                        planes[0][at as usize].to_bits(),
                        "block {k} i {i}"
                    );
                }
            }
            center += bs_at(&bs, b.w) / 4 + bs_at(&bs, b.nw) / 4;
        }
        // 外挿の部分（入力の前・後ろ）は有限
        assert!(got.iter().all(|b| b.pcm[0].iter().all(|x| x.is_finite())));
    }

    /// 打撃のある入力は短い窓を選び、無音は長い窓だけ。同じ入力なら同じブロックの列（決定性）。
    #[test]
    fn impulses_choose_short_windows() {
        let bs = [256_i64, 2048];
        let imp = oracle::signal(Signal::Impulses, 44_100, 2, 44_100, 5);
        let a = run(test_info(44_100, 2, bs), &imp);
        assert!(
            a.iter()
                .any(|b| b.w == 0 && b.blocktype == BLOCKTYPE_IMPULSE)
        );
        assert!(a.iter().any(|b| b.w == 1));
        let b = run(test_info(44_100, 2, bs), &imp);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(
                (x.w, x.lw, x.nw, x.blocktype, x.granulepos),
                (y.w, y.lw, y.nw, y.blocktype, y.granulepos)
            );
            for (p, q) in x.pcm.iter().zip(&y.pcm) {
                assert_eq!(bits(p), bits(q));
            }
        }
        let silence = vec![vec![0.0_f32; 44_100]; 2];
        let s = run(test_info(44_100, 2, bs), &silence);
        // 無音：途中は長い窓だけ（最初は W = 0 から始まり、envelope の履歴が 0 から始まるので短い窓が続くことがある。
        // 最後は eof で nW = 0）
        let ws: Vec<i64> = s.iter().map(|b| b.w).collect();
        assert!(
            ws[ws.len() / 2..ws.len() - 2].iter().all(|&w| w == 1),
            "{ws:?}"
        );
        assert!(
            s.iter()
                .all(|b| b.pcm.iter().all(|p| p.iter().all(|&x| x == 0.0)))
        );
    }

    /// 設定の誤りは Err（C の `return 1`）・wrote の数の誤り・初期化の値。
    #[test]
    fn init_checks_and_initial_state() {
        let mut vi = test_info(44_100, 2, [256, 2048]);
        let v = dsp_only(vi.clone());
        assert_eq!(v.pcm_current, 1024);
        assert_eq!(v.center_w, 1024);
        assert_eq!(v.pcm_storage, 2048);
        assert_eq!(v.sequence, 3);
        assert_eq!(v.backend_state.window, [2, 5]);
        assert_eq!(v.backend_state.modebits, 1);
        assert_eq!(v.backend_state.transform[1].n, 2048);
        assert_eq!(v.backend_state.fft_look[0].n, 256);
        assert_eq!(v.pcm.len(), 2);
        let vb = vorbis_block_init(&v);
        assert_eq!(vb.ampmax, -9999.0);
        for bad in [[32_i64, 2048], [512, 256], [256, 16384], [256, 1536]] {
            vi.codec_setup.blocksizes = bad;
            assert!(vds_dsp_init(vi.clone()).is_err(), "{bad:?}");
        }
        let mut vi = test_info(44_100, 0, [256, 2048]);
        assert!(vds_dsp_init(vi.clone()).is_err());
        vi.channels = 1;
        vi.codec_setup.modes = 0;
        assert!(vds_dsp_init(vi).is_err());
        // buffer で確保した数より多く書いたことにすると Err
        let mut v = dsp_only(test_info(44_100, 1, [256, 2048]));
        let at = vorbis_analysis_buffer(&mut v, 100).unwrap();
        assert_eq!(at, 1024);
        assert!(v.pcm[0].len() >= 1124);
        assert!(vorbis_analysis_wrote(&mut v, 10_000, &NoMonitor).is_err());
        // 始まる前（前の外挿の前）の blockout は false
        let mut vb = vorbis_block_init(&v);
        assert!(!vorbis_analysis_blockout(&mut v, &mut vb).unwrap());
        // C の pcm_storage（cur + vals*2）の中なら、buffer より多く wrote しても panic しない（0 で広げる）
        assert!(vorbis_analysis_wrote(&mut v, 150, &NoMonitor).is_ok());
        assert!(v.pcm[0].len() >= 1024 + 150);
        vorbis_analysis_wrote(&mut v, 0, &NoMonitor).unwrap();
        let mut n = 0;
        while vorbis_analysis_blockout(&mut v, &mut vb).unwrap() {
            n += 1;
        }
        assert!(n > 0);
        assert_eq!(vb.eofflag, 1);
        assert_eq!(vb.granulepos, 150);
        // 終わったあとの blockout は false
        assert!(!vorbis_analysis_blockout(&mut v, &mut vb).unwrap());
    }

    /// 分けて渡しても（wrote を何度か呼んで、その間にブロックを出しても）panic せず、最後のグラニュールは入力の長さ。
    /// （値は一度に渡したときと変わりうる：README §8.2。照合の対象は一度に渡す形だけ。）
    #[test]
    fn chunked_feeding_is_safe() {
        let frames = 30_000;
        let planes = oracle::signal(Signal::Music, 44_100, 2, frames, 21);
        let mut v = dsp_only(test_info(44_100, 2, [256, 2048]));
        let mut vb = vorbis_block_init(&v);
        let mut got = 0;
        for chunk in (0..frames).step_by(4096) {
            let m = (frames - chunk).min(4096);
            let at = vorbis_analysis_buffer(&mut v, m as i32).unwrap();
            for (dst, src) in v.pcm.iter_mut().zip(&planes) {
                dst[at..at + m].copy_from_slice(&src[chunk..chunk + m]);
            }
            vorbis_analysis_wrote(&mut v, m as i32, &NoMonitor).unwrap();
            while vorbis_analysis_blockout(&mut v, &mut vb).unwrap() {
                got += 1;
            }
        }
        vorbis_analysis_wrote(&mut v, 0, &NoMonitor).unwrap();
        while vorbis_analysis_blockout(&mut v, &mut vb).unwrap() {
            got += 1;
        }
        assert!(got > 20);
        assert_eq!(vb.eofflag, 1);
        assert_eq!(vb.granulepos, frames as i64);
        assert!(v.pcm_base > 0);
    }
}
