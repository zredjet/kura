//! 全体の流れ（kura の側）：値 → libvorbis と同じ手順のパケット → Ogg。
//!
//! # 流れ（[`encode`]）
//!
//! 1. 形の確認（`OggVorbisEncoder::can_encode` と同じ範囲。直接呼ばれたときのために `Unsupported`）。
//! 2. 入力：Vorbis の並びへ（`super::ORDER`）。NaN は 0、±∞ は ±1（[`sanitize`]。照合用のエンコーダにも同じ値を渡す）。
//! 3. `setup::vorbis_encode_init_vbr(ch, rate, q as f32 / 10.0)`。
//! 4. コメント（[`comment_list`]。E28 の約束）と `info::vorbis_analysis_headerout` で 3 つのヘッダ。コメントのパケットが
//!    16 MiB（`1 << 24`）以上なら `Unsupported`（symphonia の Ogg の読み口がパケットを 16 MiB までしか読まない。FLAC の
//!    meta_block と同じ上限）。
//! 5. `block::vorbis_analysis_init`、全入力を `vorbis_analysis_buffer(N)` に書いて `wrote(N)`、`wrote(0)`
//!    （N = 0 なら `wrote(0)` だけ）。1〜5 は [`prepare`]。
//! 6. ブロックの解析（mod.rs の規則 12。[`analyze_blocks`]）：
//!    - 段 0（直列）：`block::vorbis_analysis_plan` を全ブロックについて回して計画の列（envelope・窓の長短・blocktype・
//!      グラニュール・PCM の位置）。C の `vorbis_analysis_blockout` の順のまま。
//!    - 区間（[`segment_blocks`] ブロック）ごとに、段 0′（aoTuV の窓の状態 `AotuvWindowState`。block_mode の列だけ）を直列 →
//!      段 1（`mapping0_analyze`：M2 の poste・窓・MDCT・FFT・logfft・logmdct・local_ampmax）を `par_blocks` → 段 2（ampmax の
//!      再帰。block.c:657–659 と mapping0.c:900–902・1184。M5 の lowcomp。M3・M9 の lastmdct を写して進める）を直列 → 段 3
//!      （`mapping0_encode`：心理モデル・floor・coupling・residue・パケット）を `par_blocks`。
//!    - ブロックをまたぐ状態（mod.rs の規則 12 の表）：ampmax のスカラー 2 つ（C の `g->ampmax` と `vbi->ampmax`）、aoTuV の窓の
//!      状態、lowcomp、lastmdct は段 0′・段 2 が運び、段 3 はその値を読むだけ。M3 の tempmdct だけは前のブロックの段 3 に依る
//!      （26 kHz 以上で、前も impulse の impulse のブロック）ので、段 3 は続いた impulse のブロックを 1 つの仕事として順に回す
//!      （`stage3_units`。区間の最初の仕事は前の区間の最後の tempmdct を引き継ぐ）。`par_blocks` は添字の順に集めるので、結果は
//!      スレッド数・区間の大きさによらない（このファイルのテストと照合で確かめる）。メモリは区間ずつ（全体の段 1 の結果を持たない）。
//!    - 直列の `vorbis_analysis_blockout` → `vorbis_analysis`（C の形）と同じパケットになる（テストで照らす）。
//!    - 中止（[`Monitor::canceled`]。確かめるだけで値は変わらない）：pcm の確保の前、入力の写し（[`COPY_CHUNK`] フレーム
//!      ごと）、前の外挿の自己相関（`par_blocks` の仕事ごとと、各和の `block::CANCEL_SAMPLES` 標本ごと）、計画（envelope の
//!      探索の `envelope::CANCEL_STEPS` 歩ごと。入力を一度に渡すので最初の計画が入力の全体の envelope を求める。ほかに計画
//!      [`CANCEL_EVERY`] 個ごと）、ブロックの解析（`par_blocks` がブロックごと）、シリアル番号のダイジェスト（前と後と、
//!      `value::DIGEST_CHUNK` 標本の塊ごと。値は `Value::digest` と同じ）、mux（音声パケット [`MUX_CANCEL_EVERY`] 個ごと）。
//!      48 kHz ステレオ・q5 を Apple Silicon（負荷 9〜14）で測ると、確かめの間隔の最大は pcm の確保（入力と同じ大きさを 0 で
//!      埋める）で、30 分で約 35 ms、3 時間（既定の `Limits` の上限）で約 0.2 秒。ほかの間隔は 30 分で 4 ms まで、3 時間で
//!      25 ms まで（mux の前の `page_plan`）。最初から頼んだ中止は 1 ms 未満で戻る。途中で頼んだ中止は、次の確かめまでの
//!      時間に、書きかけのパケットの列などを捨てる時間（30 分で 2〜11 ms、3 時間で 13〜60 ms）を足したころに戻る。最後の
//!      確かめのあと（mux の残りの 511 パケットまでと片付け。30 分で約 13 ms、3 時間で約 60 ms）に頼んだ中止は効かず、`Ok` が返る。
//! 7. Ogg（[`mux`]。codec/ogg.rs）：シリアル番号（[`serial_of`]）、ページの規則（A1〜A7・A6′・A10）、グラニュール（c_k、最後は N）。
//!
//! 「パケットの列 → Ogg」（[`mux`]・[`page_plan`]・[`packet_window`]）は音声の生成（block・mapping0）から独立していて、
//! 照合用のエンコーダのパケットでそのまま試せる（このファイルのテスト）。
//!
//! # 決まっていること
//!
//! - コメント：vendor は `super::VENDOR`（"kura"）。項目は `symph::vorbis_comments_for(&meta, frames)` の LOOPSTART・LOOPLENGTH（あれば）の
//!   直後に `ENCODER=ogg-vorbis-kura-enc/1`、続いて残りのタグ。元の `ENCODER`（`vorbis:ENCODER`。名前は大文字小文字を
//!   問わない）は運ばない（E28：RPG ツクール MV/MZ の読み方に合わせ、LOOPLENGTH の次の項目の長さの下位バイトが数字に
//!   ならない番兵になる）。
//! - 空の音声（N = 0）も書く（libvorbis と同じく、短いブロック 1 つ・全チャンネルの floor を「使わない」の 1 パケット。
//!   グラニュール 0・EOS）。
//! - シリアル番号（A10）：`BLAKE3("kura-ogg-serial-v1" のドメイン ‖ 入力の値のダイジェスト（`Value::digest`、
//!   `kura-pcm-v1`。標本・レート・メタデータ）‖ 長さ付きの設定の正規の JSON（`{"quality":q}`。既定を埋めた実際の値）)` の
//!   先頭 4 バイト（リトルエンディアン）。同じ値・同じ設定なら同じ番号、違う音を連結（chain）しても番号がまず重ならない。
//! - ページの閉じ方（[`page_plan`]）：識別ヘッダのあとで閉じる（A1）、コメントのあとでは閉じない（setup を同じページに
//!   続ける。A2）、setup のあとで閉じる（A2・A3）、最初の音声パケットのあとで閉じる（A4）、「長いブロックで次の旗が短い」
//!   パケットのあとでは閉じない（A5）、最後の音声パケットの前で閉じる（A6′。前のパケットが A5 に当たるときは A5 を優先）、
//!   最後の音声パケットで EOS（A6）、ほかは本体が `PAGE_FILL` 以上で閉じる。
//! - グラニュール（A7）：パケットの最初のバイトの mode と旗から窓の大きさを読み、c_0 = 0、c_k = c_{k−1} + n_{k−1}/4 + n_k/4。
//!   最後の音声パケットのページは N。block が付けた `OggPacket::granulepos` は使わない（同じ値になる。照合のテストで確かめる）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use serde_json::json;

use super::bitpack::OggpackBuffer;
use super::block::{BlockPlan, VorbisDspState};
use super::mapping0::{
    AotuvState, AotuvWindowState, LASTMDCT_LEN, TEMPMDCT_LEN, block_mode_of, mapping0_ampmax,
    mapping0_analyze, mapping0_encode, mapping0_lastmdct, mapping0_lowcomp,
};
use super::{CHANNELS_MAX, ENC_ID, ORDER, QUALITY_MAX, QUALITY_MIN, RATE_MAX, RATE_MIN};
use super::{block, info, setup};
use crate::codec::ogg::{Flush, OggPacket, OggWriter};
use crate::determinism::par_blocks;
use crate::digest::{Hasher, canonical_json};
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::value::{Audio, AudioMeta};

/// コメントのパケットの上限（バイト。この大きさ以上は `Unsupported`）。symphonia の Ogg の読み口が読めるパケットの大きさ。
pub(crate) const COMMENT_MAX: usize = 1 << 24;

/// 書けるフレーム数の上限。C の `int` の作業長（block.c の `pcm_current + vals * 2` など）があふれない範囲
/// （48 kHz でおよそ 6 時間）。
pub(crate) const FRAMES_MAX: usize = (1 << 30) - (1 << 16);

/// 中止を確かめる間隔（計画の数）。
const CANCEL_EVERY: usize = 32;

/// [`mux`] で中止を確かめる間隔（音声パケットの数。確かめるだけで値は変わらない）。
pub(crate) const MUX_CANCEL_EVERY: usize = 512;

/// シリアル番号のハッシュのドメイン（変えるとバイト列が変わる。エンコーダの版を上げる）。
const SERIAL_DOMAIN: &str = "kura-ogg-serial-v1";

/// 値を Ogg Vorbis のバイト列にする（`quality` は −1〜10）。書けない形（チャンネル・レート・品質・長さが範囲の外、
/// コメントが大きすぎる）は `Unsupported`。
pub(crate) fn encode(
    audio: &Audio,
    quality: i64,
    mon: &dyn Monitor,
) -> Result<Vec<u8>, EngineError> {
    let e = encode_packets(audio, quality, mon)?;
    // 7. Ogg（シリアル番号のダイジェストは入力の全体を 1 スレッドで読むので、その前と後と、ダイジェストの塊ごとに中止を
    // 確かめる。mux は MUX_CANCEL_EVERY パケットごと）
    if mon.canceled() {
        return Err(EngineError::Canceled);
    }
    let serial = serial_of(audio, quality, mon)?;
    if mon.canceled() {
        return Err(EngineError::Canceled);
    }
    mux(
        &e.headers,
        &e.packets,
        &e.blockflags,
        e.blocksizes,
        e.frames as u64,
        serial,
        mon,
    )
}

/// [`encode_packets`] の結果：Ogg に包む前のパケットの列と、包むのに要る設定。
#[derive(Debug, Clone)]
pub(crate) struct Encoded {
    /// 識別・コメント・setup。
    pub headers: [OggPacket; 3],
    /// 音声パケット（`granulepos`・`e_o_s` は libvorbis と同じ値）。
    pub packets: Vec<OggPacket>,
    /// mode ごとの blockflag。
    pub blockflags: Vec<bool>,
    /// 短・長の窓の大きさ。
    pub blocksizes: [i64; 2],
    /// 入力のフレーム数 N。
    pub frames: usize,
}

/// 流れの 1〜6（冒頭）：ヘッダ 3 つと音声パケットの列（Ogg に包む前）。
pub(crate) fn encode_packets(
    audio: &Audio,
    quality: i64,
    mon: &dyn Monitor,
) -> Result<Encoded, EngineError> {
    encode_packets_with(audio, quality, mon, None)
}

/// [`encode_packets`] の区間の大きさ（ブロックの数）を決められる形（`None` は [`segment_blocks`]。結果は区間の大きさに
/// よらない：テストが確かめる）。
pub(crate) fn encode_packets_with(
    audio: &Audio,
    quality: i64,
    mon: &dyn Monitor,
    segment: Option<usize>,
) -> Result<Encoded, EngineError> {
    let Prepared { headers, mut vd } = prepare(audio, quality, mon)?;

    // 6. ブロックの解析：段 0（計画。直列）→ 区間ごとに段 1（並列）・段 2（直列）・段 3（並列）
    let mut plans: Vec<BlockPlan> = Vec::new();
    while let Some(p) = block::vorbis_analysis_plan(&mut vd, mon)? {
        plans.push(p);
        if plans.len().is_multiple_of(CANCEL_EVERY) && mon.canceled() {
            return Err(EngineError::Canceled);
        }
    }
    let segment = segment
        .unwrap_or_else(|| segment_blocks(audio.channels(), vd.vi.codec_setup.blocksizes[1]));
    let packets = analyze_blocks(&vd, &plans, segment, mon)?;
    if mon.canceled() {
        return Err(EngineError::Canceled);
    }
    Ok(Encoded {
        headers,
        packets,
        blockflags: blockflags_of(&vd),
        blocksizes: vd.vi.codec_setup.blocksizes,
        frames: audio.frames(),
    })
}

/// 流れの 1〜5 の結果：ヘッダと、全入力を渡し終えた dsp の状態。
pub(crate) struct Prepared {
    pub headers: [OggPacket; 3],
    pub vd: VorbisDspState,
}

/// 流れの 1〜5（冒頭）：形の確認・設定・ヘッダ・全入力を一度に渡す。入力の写し（[`write_vorbis_channel`]）と前の外挿
/// （`block::vorbis_analysis_wrote`）の中で `mon` の中止を確かめる。
pub(crate) fn prepare(
    audio: &Audio,
    quality: i64,
    mon: &dyn Monitor,
) -> Result<Prepared, EngineError> {
    // 1. 形の確認
    let channels = audio.channels();
    if !(1..=usize::from(CHANNELS_MAX)).contains(&channels)
        || !(RATE_MIN..=RATE_MAX).contains(&audio.rate())
    {
        return Err(EngineError::Unsupported(format!(
            "Ogg Vorbis に書けるのは 1〜{CHANNELS_MAX} チャンネル・{RATE_MIN}〜{RATE_MAX} Hz です"
        )));
    }
    if !(QUALITY_MIN..=QUALITY_MAX).contains(&quality) {
        return Err(EngineError::Unsupported(format!(
            "Ogg Vorbis の品質は {QUALITY_MIN}〜{QUALITY_MAX} です"
        )));
    }
    let frames = audio.frames();
    if frames > FRAMES_MAX {
        return Err(EngineError::Unsupported(format!(
            "Ogg Vorbis に書けるのは {FRAMES_MAX} フレームまでです"
        )));
    }
    let order = vorbis_order(channels)?;
    let q = i32::try_from(quality).map_err(|_| internal("品質の値"))?;
    let vals = i32::try_from(frames).map_err(|_| internal("フレーム数"))?;

    // 3. 設定（C と同じく float の q / 10）
    let vi = setup::vorbis_encode_init_vbr(
        i64::try_from(channels).map_err(|_| internal("チャンネルの数"))?,
        i64::from(audio.rate()),
        q as f32 / 10.0,
    )?;

    // 4. ヘッダ
    let comments = comment_list(&audio.meta, frames as u64);
    let headers = info::vorbis_analysis_headerout(&vi, super::VENDOR, &comments)?;
    if headers[1].packet.len() >= COMMENT_MAX {
        return Err(EngineError::Unsupported(
            "Ogg Vorbis のコメント（タグ）が大きすぎます（16 MiB まで）".into(),
        ));
    }

    // 5. 全入力を一度に渡す（Vorbis の並び・NaN と ±∞ を直した値）。pcm の確保（入力と同じ大きさを 0 で埋める）の前にも
    // 中止を確かめる
    if mon.canceled() {
        return Err(EngineError::Canceled);
    }
    let mut vd = block::vorbis_analysis_init(vi)?;
    if frames > 0 {
        let pos = block::vorbis_analysis_buffer(&mut vd, vals)?;
        if vd.pcm.len() != channels {
            return Err(internal("pcm のチャンネルの数"));
        }
        let end = pos
            .checked_add(frames)
            .ok_or_else(|| internal("pcm の位置"))?;
        for (i, dst) in vd.pcm.iter_mut().enumerate() {
            let slot = dst
                .get_mut(pos..end)
                .ok_or_else(|| internal("pcm の大きさ"))?;
            write_vorbis_channel(audio, order, i, slot, mon)?;
        }
        block::vorbis_analysis_wrote(&mut vd, vals, mon)?;
    }
    block::vorbis_analysis_wrote(&mut vd, 0, mon)?;
    Ok(Prepared { headers, vd })
}

/// 入力の写しで中止を確かめる間隔（フレームの数。確かめるだけで値は変わらない）。
pub(crate) const COPY_CHUNK: usize = 1 << 17;

/// Vorbis のチャンネル `i` の符号化する値を `dst` に写す：kura の planes（WAV の並び）の `order[i]` 番を [`sanitize`] した
/// もの（`order` は [`vorbis_order`]）。[`prepare`] が dsp の pcm に書く値で、テストの [`vorbis_planes`] も同じ関数を通る。
/// [`COPY_CHUNK`] フレームごとに `mon` の中止を確かめる。
fn write_vorbis_channel(
    audio: &Audio,
    order: &[usize],
    i: usize,
    dst: &mut [f32],
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    let src = order
        .get(i)
        .and_then(|&s| audio.planes().get(s))
        .ok_or_else(|| internal("チャンネルの並び"))?;
    if src.len() != dst.len() {
        return Err(internal("pcm の大きさ"));
    }
    for (d, s) in dst.chunks_mut(COPY_CHUNK).zip(src.chunks(COPY_CHUNK)) {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        for (d, s) in d.iter_mut().zip(s) {
            *d = sanitize(*s);
        }
    }
    Ok(())
}

/// mode ごとの blockflag。
pub(crate) fn blockflags_of(vd: &VorbisDspState) -> Vec<bool> {
    vd.vi
        .codec_setup
        .mode_param
        .iter()
        .map(|m| m.blockflag != 0)
        .collect()
}

/// 区間の大きさの目安（区間のブロックの標本 × チャンネルの数の上限）。速さとメモリのためだけの値で、結果に効かない。
/// 段 1 の結果は 1 標本あたり 1.5 個の f32 なので、区間の中間はおよそ 12 MB まで（ほかに段 3 の作業領域とパケット）。
const SEGMENT_SAMPLES: usize = 1 << 21;

/// 区間のブロックの数（長い窓 `long` × チャンネルの数で [`SEGMENT_SAMPLES`] を割ったもの。1 以上）。
pub(crate) fn segment_blocks(channels: usize, long: i64) -> usize {
    let per = usize::try_from(long)
        .unwrap_or(1)
        .max(1)
        .saturating_mul(channels.max(1));
    (SEGMENT_SAMPLES / per).max(1)
}

/// 段 2 の結果（1 ブロック）：段 3 が読むブロックをまたぐ値。
struct Carried {
    /// C の `vbi->ampmax`（mapping0 の global_ampmax）。
    global_ampmax: f32,
    /// M5 の lowcomp（チャンネルごと。このブロックの `lb_loudnoise_fix` のあと）。
    lowcomp: Vec<f32>,
    /// M3・M9 の lastmdct（チャンネルごとに n/2 個。このブロックの前の値）。
    lastmdct: Vec<Vec<f32>>,
}

/// 段 3 の仕事の分け方：M3 の tempmdct を引き継ぐのは「前も impulse の impulse のブロック」だけ（psy.c の set_m3p：
/// `lW_block_mode == 0` なら前の tempmdct から、ほかは lastmdct から作り直す。impulse でないブロックは触らない）なので、
/// 続いた impulse のブロックを 1 つの仕事（順に回す）にまとめ、ほかは 1 ブロックずつにする。26 kHz 未満は M3 を使わない
/// （`mdctbuf_flag` が 0）ので 1 ブロックずつ。区間の最初の仕事は前の区間の最後の tempmdct を引き継ぐ。
fn stage3_units(
    chunk: &[BlockPlan],
    windows: &[AotuvWindowState],
    m3: bool,
) -> Vec<(usize, usize)> {
    let mut units: Vec<(usize, usize)> = Vec::new();
    for (k, (p, w)) in chunk.iter().zip(windows).enumerate() {
        let carry = m3 && block_mode_of(p.blocktype, p.w) == 0 && w.lw_block_mode == 0;
        match units.last_mut() {
            Some(u) if carry => u.1 = k + 1,
            _ => units.push((k, k + 1)),
        }
    }
    units
}

/// 段 1〜3（冒頭の 6）：計画の列からパケットの列（`segment` ブロックずつ）。
pub(crate) fn analyze_blocks(
    vd: &VorbisDspState,
    plans: &[BlockPlan],
    segment: usize,
    mon: &dyn Monitor,
) -> Result<Vec<OggPacket>, EngineError> {
    let channels = usize::try_from(vd.vi.channels).map_err(|_| internal("チャンネルの数"))?;
    // C の g->ampmax（_vp_global_look の −9999）と vbi->ampmax（vorbis_block_init の −9999）
    let mut g_ampmax = vd.backend_state.psy_g_look.ampmax;
    let mut vbi_ampmax = block::VBI_AMPMAX_INIT;
    // aoTuV のブロックをまたぐ状態（C の private_state の欄）。段 0・段 2・段 3 がそれぞれの部分を進める
    let mut st = AotuvState::new(channels);
    // M3 が tempmdct を使うか（26 kHz 以上。psy の look のレートはどれも同じ）
    let m3 = vd
        .backend_state
        .psy
        .first()
        .is_some_and(|p| p.rate >= 26000);
    let mut packets = Vec::with_capacity(plans.len());
    for chunk in plans.chunks(segment.max(1)) {
        // 段 0 の続き（直列）：aoTuV の窓の状態（block_mode の列だけで決まる。mapping0.c:1297–1305）
        let mut windows = Vec::with_capacity(chunk.len());
        for p in chunk {
            windows.push(st.window);
            st.window = st.window.next(block_mode_of(p.blocktype, p.w));
        }
        // 段 1（ブロックごとに独立）：M2 の poste・窓・MDCT・FFT・logfft・logmdct・local_ampmax
        let analyses = par_blocks(chunk.len(), mon, |k| {
            let p = &chunk[k];
            let lw_block_mode = windows
                .get(k)
                .ok_or_else(|| internal("窓の状態の数"))?
                .lw_block_mode;
            mapping0_analyze(
                vd,
                p.lw,
                p.w,
                p.nw,
                p.blocktype,
                lw_block_mode,
                block::block_pcm(vd, p)?,
            )
        })?;
        // 段 2（C の順に直列）：ampmax の再帰、M5 の lowcomp、M3・M9 の lastmdct（どれも段 1 の値だけから進む）
        let mut carried = Vec::with_capacity(chunk.len());
        for ((p, a), w) in chunk.iter().zip(&analyses).zip(&windows) {
            let start = block::blockout_ampmax(&mut g_ampmax, vbi_ampmax, &vd.vi, p.w);
            vbi_ampmax = mapping0_ampmax(start, &a.local_ampmax);
            mapping0_lowcomp(vd, p.blocktype, p.w, w.lw_block_mode, a, &mut st.lowcomp)?;
            let snap = mapping0_lastmdct(vd, p.blocktype, p.w, p.nw, a, &mut st.lastmdct)?;
            carried.push(Carried {
                global_ampmax: vbi_ampmax,
                lowcomp: st.lowcomp.clone(),
                lastmdct: snap,
            });
        }
        // 段 3（段 2 の値があれば、M3 の tempmdct を引き継ぐ impulse の続きを除いてブロックごとに独立）
        let units = stage3_units(chunk, &windows, m3);
        let carry_in = &st.tempmdct;
        let out = par_blocks(units.len(), mon, |u| {
            let (s, e) = *units.get(u).ok_or_else(|| internal("段 3 の仕事"))?;
            // 最初の仕事だけが前の区間の tempmdct を引き継ぐ（ほかの仕事の最初のブロックは tempmdct を読まない）
            let mut temp = if u == 0 {
                carry_in.clone()
            } else {
                vec![vec![0.0_f32; TEMPMDCT_LEN]; channels]
            };
            // lastmdct の作業領域（先頭 n/2 個に段 2 の値を写す。そこより後ろは読まれない）
            let mut last = vec![vec![0.0_f32; LASTMDCT_LEN]; channels];
            let mut pk = Vec::with_capacity(e - s);
            for k in s..e {
                let (p, a, c, w) = (
                    chunk.get(k).ok_or_else(|| internal("計画の数"))?,
                    analyses.get(k).ok_or_else(|| internal("段 1 の数"))?,
                    carried.get(k).ok_or_else(|| internal("段 2 の数"))?,
                    *windows.get(k).ok_or_else(|| internal("窓の状態の数"))?,
                );
                for (dst, src) in last.iter_mut().zip(&c.lastmdct) {
                    dst.get_mut(..src.len())
                        .ok_or_else(|| internal("lastmdct の大きさ"))?
                        .copy_from_slice(src);
                }
                let mut opb = OggpackBuffer::oggpack_writeinit();
                mapping0_encode(
                    vd,
                    p.lw,
                    p.w,
                    p.nw,
                    p.blocktype,
                    a,
                    c.global_ampmax,
                    w,
                    &c.lowcomp,
                    &mut last,
                    &mut temp,
                    &mut opb,
                )?;
                pk.push(OggPacket {
                    packet: opb.oggpack_get_buffer().to_vec(),
                    e_o_s: p.eofflag != 0,
                    granulepos: p.granulepos,
                    packetno: p.sequence,
                });
            }
            Ok((pk, temp))
        })?;
        for (pk, temp) in out {
            packets.extend(pk);
            // 次の区間へ：最後の仕事の tempmdct（最後のブロックが impulse でなければ次は読まない）
            st.tempmdct = temp;
        }
    }
    Ok(packets)
}

fn internal(what: &str) -> EngineError {
    EngineError::Internal(format!("Ogg Vorbis の書き出し：{what}が正しくありません"))
}

/// 入力の標本を符号化できる値にする：NaN は 0、±∞ は ±1、ほかはそのまま（−0 も、1 を超える有限の値もそのまま）。
pub(crate) fn sanitize(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else if x == f32::INFINITY {
        1.0
    } else if x == f32::NEG_INFINITY {
        -1.0
    } else {
        x
    }
}

/// チャンネルの数 → Vorbis のチャンネル `i` に入れる kura の plane の番号の並び（`super::ORDER`）。
fn vorbis_order(channels: usize) -> Result<&'static [usize], EngineError> {
    channels
        .checked_sub(1)
        .and_then(|i| ORDER.get(i))
        .copied()
        .ok_or_else(|| {
            EngineError::Unsupported(format!(
                "Ogg Vorbis に書けるのは 1〜{CHANNELS_MAX} チャンネルです"
            ))
        })
}

/// kura の planes（WAV の並び）を Vorbis の並びの、符号化する値（[`sanitize`] 済み）にする（[`prepare`] と同じ
/// [`write_vorbis_channel`] を通る。テストが使う）。
#[cfg(test)]
pub(crate) fn vorbis_planes(audio: &Audio) -> Result<Vec<Vec<f32>>, EngineError> {
    let order = vorbis_order(audio.channels())?;
    (0..order.len())
        .map(|i| {
            let mut v = vec![0.0_f32; audio.frames()];
            write_vorbis_channel(audio, order, i, &mut v, &crate::monitor::NoMonitor)?;
            Ok(v)
        })
        .collect()
}

/// コメントの項目（E28 の並び）：LOOPSTART・LOOPLENGTH（あれば）、`ENCODER=ogg-vorbis-kura-enc/1`、残りのタグ
/// （`symph::vorbis_comments_for` の順。元の ENCODER は落とす）。`frames` は音声の長さ（ループ点は `start < end ≤ frames` だけ書く）。
pub(crate) fn comment_list(meta: &AudioMeta, frames: u64) -> Vec<(String, String)> {
    let all = crate::codec::symph::vorbis_comments_for(meta, frames);
    let mut out = Vec::with_capacity(all.len() + 1);
    let mut rest = Vec::with_capacity(all.len());
    for (key, value) in all {
        if key == "LOOPSTART" || key == "LOOPLENGTH" {
            out.push((key, value));
        } else if !key.eq_ignore_ascii_case("ENCODER") {
            rest.push((key, value));
        }
    }
    out.push(("ENCODER".to_string(), ENC_ID.to_string()));
    out.extend(rest);
    out
}

/// シリアル番号（A10。冒頭の「決まっていること」）：入力の値のダイジェストと設定の正規の JSON から。ダイジェストは
/// `Value::digest` と同じ値で、その塊（`value::DIGEST_CHUNK` 標本）ごとに `mon` の中止を確かめる。
pub(crate) fn serial_of(
    audio: &Audio,
    quality: i64,
    mon: &dyn Monitor,
) -> Result<u32, EngineError> {
    let digest = crate::value::audio_digest(audio, &mut || {
        if mon.canceled() {
            Err(EngineError::Canceled)
        } else {
            Ok(())
        }
    })?;
    let settings = canonical_json(&json!({ "quality": quality }))?;
    let d = Hasher::new(SERIAL_DOMAIN)
        .raw(&digest.0)
        .bytes_with_len(&settings)
        .finish();
    Ok(u32::from_le_bytes([d.0[0], d.0[1], d.0[2], d.0[3]]))
}

/// 音声パケットの窓（Vorbis I §4.3.1 の頭：種類 1 ビット・mode・長いブロックなら前と次の窓の旗）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PacketWindow {
    /// mode の番号。
    pub mode: usize,
    /// 長いブロック（mode の blockflag）。
    pub long: bool,
    /// 前の窓の旗（長いブロックだけ。短いブロックは偽）。
    pub prev_long: bool,
    /// 次の窓の旗（長いブロックだけ。短いブロックは偽）。
    pub next_long: bool,
}

impl PacketWindow {
    /// 「長いブロックで次の窓の旗が短い」（A5 の対象）。
    pub fn long_to_short(self) -> bool {
        self.long && !self.next_long
    }
}

/// 音声パケットの最初のビットから mode と窓の旗を読む。`mode_bits` は `ov_ilog(modes - 1)`（mode は 64 個までなので 0〜6）、
/// `blockflags` は mode ごとの blockflag。音声パケットでない（最初のビットが 1）・短すぎる・mode が範囲の外なら None。
/// ビットは各バイトの最下位から読む（Vorbis I §2）。
pub(crate) fn packet_window(
    packet: &[u8],
    mode_bits: u32,
    blockflags: &[bool],
) -> Option<PacketWindow> {
    if mode_bits > 6 {
        return None;
    }
    let bit = |i: u32| -> Option<bool> {
        let byte = packet.get(usize::try_from(i / 8).ok()?)?;
        Some((byte >> (i % 8)) & 1 == 1)
    };
    if bit(0)? {
        return None;
    }
    let mut mode = 0usize;
    for j in 0..mode_bits {
        if bit(1 + j)? {
            mode |= 1 << j;
        }
    }
    let long = *blockflags.get(mode)?;
    let (prev_long, next_long) = if long {
        (bit(1 + mode_bits)?, bit(2 + mode_bits)?)
    } else {
        (false, false)
    };
    Some(PacketWindow {
        mode,
        long,
        prev_long,
        next_long,
    })
}

/// 音声パケットごとのページの指示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PagePlan {
    /// パケットの窓。
    pub window: PacketWindow,
    /// 窓の中心 c_k（そこまでにデコーダが返す標本の数）。
    pub center: i64,
    /// このパケットで終わるページのグラニュール（c_k。最後のパケットは N）。
    pub granule: i64,
    /// 最後のパケット（EOS）。
    pub eos: bool,
    /// 閉じ方。
    pub flush: Flush,
}

/// 音声パケットの列から、ページのグラニュールと閉じ方を決める（冒頭の「決まっていること」）。長いブロックの前後の窓の旗が
/// 実際の隣のブロックの大きさと合わない（グラニュールの c_k は窓の大きさの列から決まるので、旗と食い違うとデコーダの
/// 数える長さと合わなくなる）、N が最後のパケットの範囲にない（c_{K−2} < N ≤ c_{K−1}、K = 1 なら N = 0）なら `Internal`。
pub(crate) fn page_plan(
    packets: &[OggPacket],
    blockflags: &[bool],
    blocksizes: [i64; 2],
    total: u64,
) -> Result<Vec<PagePlan>, EngineError> {
    let valid_size = |n: i64| (64..=8192).contains(&n) && n.count_ones() == 1;
    if !valid_size(blocksizes[0]) || !valid_size(blocksizes[1]) || blocksizes[0] > blocksizes[1] {
        return Err(internal("ブロックの大きさ"));
    }
    if blockflags.is_empty() || blockflags.len() > 64 {
        return Err(internal("mode の数"));
    }
    if packets.is_empty() {
        return Err(internal("音声パケットの数"));
    }
    let modes = u32::try_from(blockflags.len()).map_err(|_| internal("mode の数"))?;
    let mode_bits = u32::try_from(vorbis_tables::ov_ilog(modes - 1)).unwrap_or(0);
    let windows = packets
        .iter()
        .map(|p| {
            packet_window(&p.packet, mode_bits, blockflags)
                .ok_or_else(|| internal("音声パケットの頭"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let size = |w: &PacketWindow| blocksizes[usize::from(w.long)];
    let count = windows.len();
    let mut centers = Vec::with_capacity(count);
    let mut c = 0i64;
    for (k, w) in windows.iter().enumerate() {
        if k > 0 {
            let prev = &windows[k - 1];
            // 長いブロックの前後の窓の旗は、実際の隣のブロックの大きさと一致する（最初の前・最後の次は問わない）
            if w.long && w.prev_long != prev.long {
                return Err(internal("長いブロックの前の窓の旗"));
            }
            if prev.long && prev.next_long != w.long {
                return Err(internal("長いブロックの次の窓の旗"));
            }
            c += size(prev) / 4 + size(w) / 4;
        }
        centers.push(c);
    }
    let total = i64::try_from(total).map_err(|_| internal("フレーム数"))?;
    let last = centers[count - 1];
    let covered = if count == 1 {
        total == 0
    } else {
        centers[count - 2] < total && total <= last
    };
    if !covered {
        return Err(internal("最後のパケットとフレーム数"));
    }
    // A6′：最後のページの前で閉じるパケット。ふつうは最後のひとつ前、それが A5 に当たるならもうひとつ前
    // （最後のページは「長・次は短」と最後のパケットの 2 つ。そのひとつ前のパケットは次が長いので A5 に当たらない）
    let before_last = match count {
        1 => None,
        2 => Some(0),
        _ if windows[count - 2].long_to_short() => Some(count - 3),
        _ => Some(count - 2),
    };
    Ok(windows
        .iter()
        .zip(&centers)
        .enumerate()
        .map(|(k, (&window, &center))| {
            let eos = k + 1 == count;
            let flush = if eos || k == 0 {
                // A6・A4（最初の音声パケットだけで 1 ページ）
                Flush::After
            } else if window.long_to_short() {
                // A5
                Flush::Never
            } else if Some(k) == before_last {
                // A6′：最後の音声パケット（と A5 で保留したもの）だけで最後のページ
                Flush::After
            } else {
                Flush::Auto
            };
            PagePlan {
                window,
                center,
                granule: if eos { total } else { center },
                eos,
                flush,
            }
        })
        .collect())
}

/// ヘッダと音声パケットを Ogg のページにする（codec/ogg.rs の規則）。`blockflags` は mode ごとの blockflag（パケットの mode
/// から窓の大きさを引くため）、`blocksizes` は短・長の窓の大きさ、`total` は総フレーム数 N（最後のページのグラニュール）。
/// 音声パケットを [`MUX_CANCEL_EVERY`] 個書くごとに `mon` の中止を確かめる（確かめる数は ⌊パケットの数 / MUX_CANCEL_EVERY⌋）。
pub(crate) fn mux(
    headers: &[OggPacket; 3],
    packets: &[OggPacket],
    blockflags: &[bool],
    blocksizes: [i64; 2],
    total: u64,
    serial: u32,
    mon: &dyn Monitor,
) -> Result<Vec<u8>, EngineError> {
    let plan = page_plan(packets, blockflags, blocksizes, total)?;
    let mut w = OggWriter::with_serial(serial);
    // A1：識別ヘッダだけで 1 ページ目（BOS）
    w.packet(&headers[0].packet, 0, false, Flush::After)?;
    // A2：コメントは 2 ページ目の頭から、setup を同じページに続け、setup で閉じる（A3：音声は新しいページから）
    w.packet(&headers[1].packet, 0, false, Flush::Never)?;
    w.packet(&headers[2].packet, 0, false, Flush::After)?;
    for (k, (p, pl)) in packets.iter().zip(&plan).enumerate() {
        w.packet(&p.packet, pl.granule, pl.eos, pl.flush)?;
        if (k + 1).is_multiple_of(MUX_CANCEL_EVERY) && mon.canceled() {
            return Err(EngineError::Canceled);
        }
    }
    Ok(w.finish())
}

#[cfg(test)]
mod tests {
    // 照合のテストは環境変数（KURA_VORBIS_ORACLE・KURA_FFMPEG）を読み、外部のプログラムを呼ぶ
    #![allow(clippy::disallowed_methods)]

    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rand_chacha::rand_core::Rng;

    use super::*;
    use crate::codec::ogg::tests::{Page, closed_after, packet_spans, packets, pages};
    use crate::codec::ogg::{FLAG_BOS, FLAG_CONTINUED, FLAG_EOS, MAX_SEGMENTS, PAGE_FILL};
    use crate::codec::vorbis::oracle::{self, Signal};
    use crate::monitor::NoMonitor;
    use crate::value::{LoopRange, Value};

    // ---- 部品 ----

    fn audio_of(rate: u32, planes: Vec<Vec<f32>>, meta: AudioMeta) -> Audio {
        Audio::new(rate, planes.into_iter().map(Arc::new).collect(), meta).unwrap()
    }

    /// mode と窓の旗から音声パケットを作る（頭のビットだけ正しく、残りはでたらめ）。
    fn fake_packet(w: PacketWindow, mode_bits: u32, len: usize, r: &mut impl Rng) -> Vec<u8> {
        let salt = r.next_u32();
        let mut p: Vec<u8> = (0..len.max(1) as u32)
            .map(|i| (i.wrapping_mul(0x9E37_79B1).wrapping_add(salt) >> 24) as u8)
            .collect();
        let mut bits = vec![false];
        for j in 0..mode_bits {
            bits.push((w.mode >> j) & 1 == 1);
        }
        if w.long {
            bits.push(w.prev_long);
            bits.push(w.next_long);
        }
        if p.len() * 8 < bits.len() {
            p.resize(bits.len().div_ceil(8), 0);
        }
        for (i, b) in bits.iter().enumerate() {
            if *b {
                p[i / 8] |= 1 << (i % 8);
            } else {
                p[i / 8] &= !(1 << (i % 8));
            }
        }
        p
    }

    /// 長短の並び（true が長い）から、窓の旗を隣の実際の大きさで付けた窓の列（最後の次は長い）。
    fn windows_of(longs: &[bool]) -> Vec<PacketWindow> {
        longs
            .iter()
            .enumerate()
            .map(|(k, &long)| PacketWindow {
                mode: usize::from(long),
                long,
                prev_long: long && k > 0 && longs[k - 1],
                next_long: long && longs.get(k + 1).copied().unwrap_or(true),
            })
            .collect()
    }

    fn centers_of(windows: &[PacketWindow], blocksizes: [i64; 2]) -> Vec<i64> {
        let mut c = 0;
        let mut out = Vec::new();
        for (k, w) in windows.iter().enumerate() {
            if k > 0 {
                c += blocksizes[usize::from(windows[k - 1].long)] / 4
                    + blocksizes[usize::from(w.long)] / 4;
            }
            out.push(c);
        }
        out
    }

    fn header(kind: u8, len: usize) -> OggPacket {
        let mut p = vec![kind];
        p.extend_from_slice(b"vorbis");
        p.resize(len.max(8), 0x55);
        if let Some(last) = p.last_mut() {
            *last = 1;
        }
        OggPacket {
            packet: p,
            ..OggPacket::default()
        }
    }

    fn ogg_packets(bodies: &[Vec<u8>]) -> Vec<OggPacket> {
        bodies
            .iter()
            .enumerate()
            .map(|(i, b)| OggPacket {
                packet: b.clone(),
                packetno: i as i64 + 3,
                ..OggPacket::default()
            })
            .collect()
    }

    /// lewton の音声パケットの出力の長さ（audio.rs:1060–1137：`right_win_start − left_win_start`。最初のパケットは捨てる）を
    /// 窓の旗から計算したもの（照合用のエンコーダのない CI で使う。照合のテストでは本物の lewton の出力を使う）。
    fn lewton_model_outs(windows: &[PacketWindow], blocksizes: [i64; 2]) -> Vec<u64> {
        let rws = |w: &PacketWindow| {
            let n = blocksizes[usize::from(w.long)];
            if w.long_to_short() {
                (3 * n - blocksizes[0]) / 4
            } else {
                n / 2
            }
        };
        let lws = |w: &PacketWindow| {
            let n = blocksizes[usize::from(w.long)];
            if w.long && !w.prev_long {
                (n - blocksizes[0]) / 4
            } else {
                0
            }
        };
        windows
            .iter()
            .enumerate()
            .map(|(k, w)| if k == 0 { 0 } else { (rws(w) - lws(w)) as u64 })
            .collect()
    }

    /// lewton の `OggStreamReader`（inside_ogg.rs:140–205）の末尾の切り詰めを写したもの：音声パケットごとの lewton の出力の
    /// 長さ `outs` と Ogg のページから、読み口が返す総数を出す（ogg 0.8 の `last_in_page`・`last_in_stream`・`absgp_page`）。
    fn lewton_reader_total(outs: &[u64], ps: &[Page], spans: &[(usize, usize)]) -> u64 {
        let mut cur: Option<u64> = None;
        let mut total = 0u64;
        for (k, &out) in outs.iter().enumerate() {
            let i = k + 3;
            let end = spans[i].1;
            let gp = ps[end].granule as u64;
            let last_in_page = spans.get(i + 1).is_none_or(|(start, _)| *start != end);
            let last_in_stream = last_in_page && ps[end].flags & FLAG_EOS != 0;
            if k == 0 {
                // 最初の音声パケットは窓を作るためだけに読み、捨てる
                cur = Some(gp);
                continue;
            }
            let mut n = out;
            if let (Some(a), true) = (cur, last_in_stream) {
                n = n.min(gp.saturating_sub(a));
            }
            if last_in_page {
                cur = Some(gp);
            } else if let Some(a) = cur.as_mut() {
                *a += n;
            }
            total += n;
        }
        total
    }

    /// パケットごとの、そのパケットが終わったページの本体の頭からパケットの終わりまでの大きさ（そこで閉じたなら、閉じた
    /// ときのページの本体の大きさ）。
    fn packet_end_offsets(ps: &[Page]) -> Vec<usize> {
        let mut out = Vec::new();
        for p in ps {
            let mut off = 0usize;
            for &l in &p.lacing {
                off += usize::from(l);
                if l < 255 {
                    out.push(off);
                }
            }
        }
        out
    }

    /// ページの規則（ogg.rs の A1〜A7・A6′・PAGE_FILL）を、実装と別に書き下して確かめる。`bodies` は 3 つのヘッダと
    /// 音声パケットの本体（ビット単位で同じに戻ること）。読み直したページと、パケットごとの（始まり, 終わり）のページを返す。
    fn check_page_rules(
        bytes: &[u8],
        bodies: &[Vec<u8>],
        blockflags: &[bool],
        blocksizes: [i64; 2],
        total: u64,
        name: &str,
    ) -> (Vec<Page>, Vec<(usize, usize)>) {
        let ps = pages(bytes);
        let pk = packets(&ps);
        let spans = packet_spans(&ps);
        assert_eq!(pk.len(), bodies.len(), "{name}：パケットの数");
        for (i, ((got, _), want)) in pk.iter().zip(bodies).enumerate() {
            assert!(got == want, "{name}：パケット {i} がビット単位で同じでない");
        }
        let count = bodies.len() - 3;
        let audio = |k: usize| k + 3;
        let mode_bits = vorbis_tables::ov_ilog(blockflags.len() as u32 - 1) as u32;
        let windows: Vec<PacketWindow> = bodies[3..]
            .iter()
            .map(|b| packet_window(b, mode_bits, blockflags).unwrap())
            .collect();
        let centers = centers_of(&windows, blocksizes);
        // ページごとの、そこで終わる最後のパケット。パケットごとの、終わったページの本体の頭からその終わりまでの大きさ
        let mut last_on_page: Vec<Option<usize>> = vec![None; ps.len()];
        let end_offset = packet_end_offsets(&ps);
        for (j, (_, end)) in spans.iter().enumerate() {
            last_on_page[*end] = Some(j);
        }
        for (i, p) in ps.iter().enumerate() {
            assert_eq!(p.seq as usize, i, "{name}：ページ番号");
            assert_eq!(p.serial, ps[0].serial, "{name}：シリアル番号");
            assert!(
                !p.lacing.is_empty() && p.lacing.len() <= MAX_SEGMENTS,
                "{name}"
            );
            // パケットの途中で閉じるのは lacing が埋まったときだけ
            if p.lacing.last() == Some(&255) {
                assert_eq!(
                    p.lacing.len(),
                    MAX_SEGMENTS,
                    "{name}：ページ {i} を途中で閉じた"
                );
            }
            // A6：EOS は最後のページだけ（空の EOS ページはない）。BOS は最初だけ
            assert_eq!(
                p.flags & FLAG_EOS != 0,
                i + 1 == ps.len(),
                "{name}：EOS（ページ {i}）"
            );
            assert_eq!(p.flags & FLAG_BOS != 0, i == 0, "{name}：BOS（ページ {i}）");
            // A7：そのページで終わる最後のパケットの位置。ヘッダは 0、最後の音声パケットは N、なければ −1
            let want = match last_on_page[i] {
                None => -1,
                Some(j) if j < 3 => 0,
                Some(j) if j + 1 == spans.len() => total as i64,
                Some(j) => centers[j - 3],
            };
            assert_eq!(p.granule, want, "{name}：ページ {i} のグラニュール");
        }
        // A1：1 ページ目は識別ヘッダだけ
        assert_eq!(ps[0].flags, FLAG_BOS, "{name}");
        assert_eq!(spans[0], (0, 0), "{name}");
        assert!(closed_after(&ps, &spans, 0), "{name}：A1");
        // A2：コメントは 2 ページ目の頭から、setup でページを閉じる
        assert_eq!(spans[1].0, 1, "{name}：A2");
        assert_eq!(ps[1].flags & FLAG_CONTINUED, 0, "{name}");
        assert!(closed_after(&ps, &spans, 2), "{name}：A2（setup で閉じる）");
        // A3・A4：最初の音声パケットは新しいページで、それだけで 1 ページ、グラニュール 0
        let first = spans[audio(0)];
        assert_eq!(first.0, spans[2].1 + 1, "{name}：A3");
        assert_eq!(first.0, first.1, "{name}：A4");
        assert_eq!(
            ps[first.0].lacing.len(),
            bodies[3].len() / 255 + 1,
            "{name}：A4（ほかのパケットがない）"
        );
        assert_eq!(ps[first.0].granule, 0, "{name}：A4");
        // A6′：最後のページは最後の音声パケットだけ。前が「長・次は短」なら A5 を優先してその 2 つ
        let last_page = ps.len() - 1;
        assert_eq!(spans[audio(count - 1)].1, last_page, "{name}：A6");
        let first_on_last = spans.iter().position(|(_, end)| *end == last_page).unwrap();
        let a5_last = count >= 3 && windows[count - 2].long_to_short();
        let before_last = if a5_last {
            count - 3
        } else {
            count.saturating_sub(2)
        };
        if count >= 2 {
            assert_eq!(first_on_last, audio(before_last + 1), "{name}：A6′");
            assert_eq!(
                spans[audio(before_last + 1)].0,
                last_page,
                "{name}：A6′（前のページから続かない）"
            );
        }
        // EOS の前のページはグラニュールを持つ（ffmpeg が末尾の切り詰めに使う）
        if count >= 2 {
            assert!(
                ps[last_page - 1].granule >= 0,
                "{name}：EOS の前のページのグラニュール"
            );
        }
        for k in 1..count.saturating_sub(1) {
            if !closed_after(&ps, &spans, audio(k)) {
                continue;
            }
            let p = &ps[spans[audio(k)].1];
            if windows[k].long_to_short() {
                // A5：「長・次は短」の直後では閉じない（lacing が埋まって避けられないときだけ）
                assert_eq!(p.lacing.len(), MAX_SEGMENTS, "{name}：A5（音声 {k}）");
            } else if k != before_last {
                // ほかは本体が PAGE_FILL 以上か lacing が埋まったとき、または次の A5 のパケットで埋まるのを避けたとき
                assert!(
                    p.body.len() >= PAGE_FILL
                        || p.lacing.len() == MAX_SEGMENTS
                        || windows[k + 1].long_to_short(),
                    "{name}：音声 {k} のあとで早く閉じた"
                );
            }
        }
        // 閉じずに続けたなら、本体はまだ PAGE_FILL 未満（A5 で保留したものを除く）
        for k in 1..count.saturating_sub(2) {
            if closed_after(&ps, &spans, audio(k)) || windows[k].long_to_short() {
                continue;
            }
            let upto = end_offset[audio(k)];
            assert!(
                upto < PAGE_FILL,
                "{name}：音声 {k} のあとで閉じなかった（{upto}）"
            );
        }
        (ps, spans)
    }

    // ---- CI で回る性質のテスト ----

    /// NaN は 0、±∞ は ±1、ほかはビットのまま（−0・1 を超える値・非正規化数も）。
    #[test]
    fn sanitize_tames_only_non_finite() {
        assert_eq!(sanitize(f32::NAN).to_bits(), 0);
        assert_eq!(sanitize(-f32::NAN).to_bits(), 0);
        assert_eq!(sanitize(f32::from_bits(0x7f80_0001)).to_bits(), 0);
        assert_eq!(sanitize(f32::INFINITY), 1.0);
        assert_eq!(sanitize(f32::NEG_INFINITY), -1.0);
        for x in [
            0.0f32,
            -0.0,
            0.5,
            -1.0,
            1.5,
            -3.0e9,
            f32::MAX,
            f32::MIN,
            f32::MIN_POSITIVE,
            1e-45,
        ] {
            assert_eq!(sanitize(x).to_bits(), x.to_bits(), "{x}");
        }
    }

    /// kura の planes の並び（WAV：`WAVE_FORMAT_EXTENSIBLE` の既定の channel mask のビットの順。FL・FR・FC・LFE・BL・BR・
    /// BC・SL・SR）。`ORDER` とは独立に書いた表。
    const WAV_LAYOUT: [&[&str]; 8] = [
        &["FC"],
        &["FL", "FR"],
        &["FL", "FR", "FC"],
        &["FL", "FR", "BL", "BR"],
        &["FL", "FR", "FC", "BL", "BR"],
        &["FL", "FR", "FC", "LFE", "BL", "BR"],
        &["FL", "FR", "FC", "LFE", "BC", "SL", "SR"],
        &["FL", "FR", "FC", "LFE", "BL", "BR", "SL", "SR"],
    ];
    /// Vorbis I §4.3.9 のチャンネルの並び（`ORDER` とは独立に書いた表）。名前は WAV の表と同じ記号で書く：front left/center/
    /// right = FL・FC・FR、rear left/right = BL・BR（WAV の back）、rear center = BC、side left/right = SL・SR、LFE。
    /// 1：mono、2：L R、3：L C R、4：FL FR RL RR、5：FL C FR RL RR、6（5.1）：FL C FR RL RR LFE、
    /// 7（6.1）：FL C FR SL SR RC LFE、8（7.1）：FL C FR SL SR RL RR LFE。
    const VORBIS_LAYOUT: [&[&str]; 8] = [
        &["FC"],
        &["FL", "FR"],
        &["FL", "FC", "FR"],
        &["FL", "FR", "BL", "BR"],
        &["FL", "FC", "FR", "BL", "BR"],
        &["FL", "FC", "FR", "BL", "BR", "LFE"],
        &["FL", "FC", "FR", "SL", "SR", "BC", "LFE"],
        &["FL", "FC", "FR", "SL", "SR", "BL", "BR", "LFE"],
    ];

    /// Vorbis の並び：1〜8 チャンネルのすべてで、Vorbis のチャンネル i に入る kura の plane が、WAV の並びと
    /// Vorbis I §4.3.9 の並びを独立に書いた表（[`WAV_LAYOUT`]・[`VORBIS_LAYOUT`]）から決まるものと同じ（`ORDER` と
    /// 実際の [`vorbis_planes`] の両方）。値は sanitize 済み。範囲の外は断る。
    #[test]
    fn planes_follow_the_vorbis_order() {
        for n in 1..=8usize {
            let (wav, vorbis) = (WAV_LAYOUT[n - 1], VORBIS_LAYOUT[n - 1]);
            assert_eq!((wav.len(), vorbis.len()), (n, n));
            // 表どうしは同じ名前の置換
            let mut a = wav.to_vec();
            let mut b = vorbis.to_vec();
            a.sort();
            b.sort();
            assert_eq!(a, b, "{n} ch の表");
            let want: Vec<usize> = vorbis
                .iter()
                .map(|name| wav.iter().position(|w| w == name).unwrap())
                .collect();
            assert_eq!(ORDER[n - 1], &want[..], "{n} ch の ORDER");
            // 実際の並べ替え：plane p は (p + 1) × 10、NaN・∞ は sanitize される
            let planes: Vec<Vec<f32>> = (0..n)
                .map(|p| vec![(p as f32 + 1.0) * 10.0, f32::NAN, f32::INFINITY])
                .collect();
            let a = audio_of(44_100, planes, AudioMeta::default());
            let v = vorbis_planes(&a).unwrap();
            assert_eq!(v.len(), n);
            for (i, p) in v.iter().enumerate() {
                assert_eq!(
                    p,
                    &vec![(want[i] as f32 + 1.0) * 10.0, 0.0, 1.0],
                    "{n} ch の Vorbis の {i}（{}）",
                    vorbis[i]
                );
            }
        }
        // 例：7.1（WAV：FL FR FC LFE BL BR SL SR）→ Vorbis：FL FC FR SL SR BL BR LFE
        let a = audio_of(
            48_000,
            (0..8).map(|c| vec![c as f32]).collect(),
            AudioMeta::default(),
        );
        let got: Vec<f32> = vorbis_planes(&a).unwrap().iter().map(|p| p[0]).collect();
        assert_eq!(got, vec![0.0, 2.0, 1.0, 6.0, 7.0, 4.0, 5.0, 3.0]);
        // 6.1（WAV：FL FR FC LFE BC SL SR）→ Vorbis：FL FC FR SL SR BC LFE
        let a = audio_of(
            48_000,
            (0..7).map(|c| vec![c as f32]).collect(),
            AudioMeta::default(),
        );
        let got: Vec<f32> = vorbis_planes(&a).unwrap().iter().map(|p| p[0]).collect();
        assert_eq!(got, vec![0.0, 2.0, 1.0, 5.0, 6.0, 4.0, 3.0]);
        let unsupported =
            |r: Result<Vec<u8>, EngineError>| matches!(r, Err(EngineError::Unsupported(_)));
        let mon = crate::monitor::NoMonitor;
        let a = audio_of(
            48_000,
            (0..9).map(|_| vec![0.0]).collect(),
            AudioMeta::default(),
        );
        assert!(matches!(
            vorbis_planes(&a),
            Err(EngineError::Unsupported(_))
        ));
        assert!(unsupported(encode(&a, 5, &mon)));
        // 範囲の外のレート・品質も Unsupported
        let a = audio_of(7_999, vec![vec![0.0]], AudioMeta::default());
        assert!(unsupported(encode(&a, 5, &mon)));
        let a = audio_of(192_001, vec![vec![0.0]], AudioMeta::default());
        assert!(unsupported(encode(&a, 5, &mon)));
        let a = audio_of(44_100, vec![vec![0.0]], AudioMeta::default());
        assert!(unsupported(encode(&a, 11, &mon)));
        assert!(unsupported(encode(&a, -2, &mon)));
    }

    /// コメントの並び（E28）：LOOPSTART・LOOPLENGTH・ENCODER・残りのタグ。元の ENCODER は落とす。
    #[test]
    fn comment_list_follows_e28() {
        let mut meta = AudioMeta::default();
        assert_eq!(
            comment_list(&meta, 0),
            vec![("ENCODER".to_string(), "ogg-vorbis-kura-enc/1".to_string())]
        );
        meta.loop_range = Some(LoopRange {
            start: 4_410,
            end: 26_460,
        });
        meta.tags = BTreeMap::from([
            ("title".to_string(), "効果音".to_string()),
            ("vorbis:ENCODER".to_string(), "other".to_string()),
            ("vorbis:encoder".to_string(), "other2".to_string()),
            ("vorbis:ZZZ".to_string(), "z".to_string()),
            ("artist".to_string(), "誰か".to_string()),
        ]);
        let list = comment_list(&meta, 26_460);
        let keys: Vec<&str> = list.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "LOOPSTART",
                "LOOPLENGTH",
                "ENCODER",
                "ARTIST",
                "TITLE",
                "ZZZ"
            ]
        );
        assert_eq!(list[0].1, "4410");
        assert_eq!(list[1].1, "22050");
        assert_eq!(list[2].1, ENC_ID);
        // 番兵：ENCODER の項目の長さの下位バイトは数字にならない
        let entry = format!("ENCODER={ENC_ID}");
        assert!(!((entry.len() & 0xFF) as u8).is_ascii_digit());
    }

    /// シリアル番号：同じ値・設定なら同じ、標本・メタデータ・品質・レートが違えば変わる。式を凍結する。
    #[test]
    fn serial_is_deterministic_and_content_addressed() {
        let planes = || vec![vec![0.25f32, -0.5, 0.0]; 2];
        let base = audio_of(44_100, planes(), AudioMeta::default());
        let s = serial_of(&base, 5, &NoMonitor).unwrap();
        assert_eq!(s, serial_of(&base.clone(), 5, &NoMonitor).unwrap());
        // 凍結：式（ドメイン・ダイジェスト・正規の JSON）を変えるとこの値が変わる（エンコーダの版を上げる）
        let d = Value::Audio(Arc::new(base.clone())).digest().unwrap();
        let mut h = Hasher::new("kura-ogg-serial-v1");
        h.raw(&d.0).bytes_with_len(br#"{"quality":5}"#);
        let want = h.finish();
        assert_eq!(s, u32::from_le_bytes(want.0[..4].try_into().unwrap()));
        let mut meta = AudioMeta::default();
        meta.tags.insert("title".into(), "x".into());
        let mut other = planes();
        other[1][2] = 0.1;
        let others = [
            serial_of(&base, 4, &NoMonitor).unwrap(),
            serial_of(
                &audio_of(48_000, planes(), AudioMeta::default()),
                5,
                &NoMonitor,
            )
            .unwrap(),
            serial_of(
                &audio_of(44_100, other, AudioMeta::default()),
                5,
                &NoMonitor,
            )
            .unwrap(),
            serial_of(&audio_of(44_100, planes(), meta), 5, &NoMonitor).unwrap(),
        ];
        for o in others {
            assert_ne!(o, s);
        }
    }

    /// パケットの頭の読み取り：種類のビット、mode（0〜6 ビット）、長いブロックだけ前後の旗。
    #[test]
    fn packet_window_reads_mode_and_flags() {
        let two = [false, true];
        let w = |mode, long, prev_long, next_long| {
            Some(PacketWindow {
                mode,
                long,
                prev_long,
                next_long,
            })
        };
        // 0b0000_0010：音声・mode 1（長）・前 0・次 0
        assert_eq!(
            packet_window(&[0b0000_0010], 1, &two),
            w(1, true, false, false)
        );
        assert_eq!(
            packet_window(&[0b0000_1110], 1, &two),
            w(1, true, true, true)
        );
        assert_eq!(
            packet_window(&[0b0000_1010, 0xff], 1, &two),
            w(1, true, false, true)
        );
        // 短いブロックは旗を読まない（後ろのビットは何でもよい）
        assert_eq!(
            packet_window(&[0b1111_1100], 1, &two),
            w(0, false, false, false)
        );
        // mode が 1 つ（8・11 kHz）：mode のビットはない
        assert_eq!(
            packet_window(&[0b1111_1110], 0, &[false]),
            w(0, false, false, false)
        );
        // ヘッダ（最初のビットが 1）・空・mode が範囲の外
        assert_eq!(packet_window(&[0b0000_0001], 1, &two), None);
        assert_eq!(packet_window(&[], 1, &two), None);
        assert_eq!(
            packet_window(&[0b0000_0110], 2, &[false, true, false]),
            None
        );
        // 6 ビットの mode（64 個）：旗は 2 バイト目にまたがる
        let mut flags = vec![false; 64];
        flags[45] = true;
        let p = [(45u8 << 1) | 0x80, 0b0000_0001];
        assert_eq!(packet_window(&p, 6, &flags), w(45, true, true, true));
        assert_eq!(packet_window(&p[..1], 6, &flags), None);
    }

    /// 合成したパケットの列（長短の並び・大きさ・N をいろいろ）で、ページの規則が守られ、lewton の読み口の数え方の写しで
    /// 長さがちょうど N になる。
    #[test]
    fn mux_follows_page_rules_on_synthetic_streams() {
        let mut r = crate::determinism::rng(149);
        let mut patterns: Vec<Vec<bool>> = vec![
            vec![false],
            vec![false, false],
            vec![false, true],
            vec![false, true, false],
            vec![false, true, true, false],
            vec![false, true, true, false, false],
            vec![false, true, true, true, true, true],
            vec![false; 9],
            vec![false, true, false, true, false, true, false, true],
        ];
        for _ in 0..24 {
            let n = 2 + (r.next_u32() % 400) as usize;
            let mut p = vec![false];
            for _ in 1..n {
                p.push(!r.next_u32().is_multiple_of(4));
            }
            patterns.push(p);
        }
        let mut cases = 0;
        let mut unavoidable = 0;
        for (pi, longs) in patterns.iter().enumerate() {
            for blocksizes in [[256i64, 2048], [512, 4096], [512, 512]] {
                let single = blocksizes[0] == blocksizes[1];
                if single && longs.iter().any(|l| *l) {
                    continue;
                }
                let blockflags: Vec<bool> = if single {
                    vec![false]
                } else {
                    vec![false, true]
                };
                let mode_bits = vorbis_tables::ov_ilog(blockflags.len() as u32 - 1) as u32;
                let windows = windows_of(longs);
                let centers = centers_of(&windows, blocksizes);
                let k = longs.len();
                let totals: Vec<i64> = if k == 1 {
                    vec![0]
                } else {
                    let (a, b) = (centers[k - 2], centers[k - 1]);
                    vec![a + 1, (a + b) / 2, b]
                };
                for (si, size_kind) in ["tiny", "mixed", "big"].iter().enumerate() {
                    // 大きなパケットは時間がかかるので N を 1 つだけ（N による違いはページの割り方に出ない）
                    let totals = if si == 2 {
                        &totals[totals.len() / 2..][..1]
                    } else {
                        &totals[..]
                    };
                    for &total in totals {
                        let bodies_audio: Vec<Vec<u8>> = windows
                            .iter()
                            .map(|w| {
                                let len = match si {
                                    0 => 1 + (r.next_u32() % 3) as usize,
                                    1 => (r.next_u32() % 1_500) as usize,
                                    _ => 2_000 + (r.next_u32() % 9_000) as usize,
                                };
                                fake_packet(*w, mode_bits, len, &mut r)
                            })
                            .collect();
                        let headers = [
                            header(1, 30),
                            header(3, 40 + pi),
                            header(5, 3_000 + 997 * si),
                        ];
                        let pkts = ogg_packets(&bodies_audio);
                        let bytes = mux(
                            &headers,
                            &pkts,
                            &blockflags,
                            blocksizes,
                            total as u64,
                            77,
                            &NoMonitor,
                        )
                        .unwrap();
                        let mut bodies: Vec<Vec<u8>> =
                            headers.iter().map(|h| h.packet.clone()).collect();
                        bodies.extend(bodies_audio);
                        let name = format!("#{pi}・{blocksizes:?}・{size_kind}・N={total}");
                        let (ps, spans) = check_page_rules(
                            &bytes,
                            &bodies,
                            &blockflags,
                            blocksizes,
                            total as u64,
                            &name,
                        );
                        // lewton の読み口の数え方の写しで長さ N。「長・次は短」のあとの短いブロックで終わり、最後の詰め物が
                        // lewton の余分より大きいときは、どのページの割り方でも合わせられない（数えるだけ）
                        let outs = lewton_model_outs(&windows, blocksizes);
                        let got = lewton_reader_total(&outs, &ps, &spans);
                        let extra = (blocksizes[1] - blocksizes[0]) / 4;
                        if k >= 3
                            && windows[k - 2].long_to_short()
                            && total - centers[k - 2] < extra
                        {
                            unavoidable += 1;
                        } else {
                            assert_eq!(got, total as u64, "{name}：lewton の読み口の長さ");
                        }
                        cases += 1;
                    }
                }
            }
        }
        assert!(cases > 200, "{cases}");
        assert!(unavoidable < cases / 4, "{unavoidable}/{cases}");
    }

    /// E28 のページの規則を実際の書き出しで：[`check_page_rules`]（A1〜A7・A6′・PAGE_FILL・グラニュール。実装と別に
    /// 書き下したもの）を、次の 2 種類の入力に当てる。
    /// - 「長いブロックで次が短い」パケットが途中にも最後の前にも出る入力：22,050 Hz の 1 kHz の正弦（`Signal::Sine`）の
    ///   真ん中に 8 標本のクリック（±0.45）を足したもので、長さを探して見つけた組（真ん中のクリックの前が「長・次は短」、
    ///   終わりの外挿で最後の前も「長・次は短」になる。窓の列はチャンネルの数によらない：どのチャンネルも同じ値なので）。
    ///   計画（[`page_plan`]）が A5 の途中の枝（保留して閉じない）と A6′ の A5 の枝（最後のページを「長・次は短」と最後の
    ///   パケットの 2 つにする）を通り、書いたページもそのとおりであること、途中のページが PAGE_FILL で閉じたことを確かめる。
    ///   この入力では「長・次は短」のところでページの本体が PAGE_FILL に届かないので、A5 の途中の枝は書くバイト列を変えない
    ///   （その枝を守るのは計画の値の検査と、下の雑音の入力）。「長・次は短」が最後の前に来る入力は少ない（探した範囲：
    ///   `oracle::signal` の正弦・音楽・雑音・衝撃・Bursts × 8 レート × 1〜3 ch × q −1・0・3・5・10 × 100〜9,000 フレーム、
    ///   ほかに 0.5〜1.5 秒の音楽・衝撃・Bursts。見つかったのは 22,050 Hz の正弦だけ）。
    /// - A5 の途中の枝が書くバイト列を変える入力：コーデックのゴールデンと同じ雑音（`InputGen::AudioNoise` の 4 つ。A5 の
    ///   途中の枝を外すと、どれもバイト列が変わる）。「長・次は短」のパケットのところで本体が PAGE_FILL 以上になったのに
    ///   閉じなかったページがあることを、計画を見ずにページの側で確かめる（A5 を外すと check_page_rules の A5 の検査が落ちる）。
    #[test]
    fn e28_page_rules_hold_on_real_output() {
        let mon = crate::monitor::NoMonitor;
        let rate = 22_050u32;
        // （チャンネル, q, 長さ, PAGE_FILL で閉じた途中のページの数の下限）。6 ch（uncoupled）は 6 ページの音声、
        // 2 ch（coupling）・1 ch の短いもの（窓の列 ssXssLXs）はページが少ない
        for (ch, q, frames, filled_min) in [
            (6usize, 10i64, 40_582usize, 3usize),
            (2, 10, 40_582, 1),
            (1, 10, 2_325, 0),
        ] {
            let mut planes = oracle::signal(Signal::Sine, rate, ch, frames, 1);
            for p in &mut planes {
                for (k, x) in p[frames / 2..frames / 2 + 8].iter_mut().enumerate() {
                    *x += if k % 2 == 0 { 0.45 } else { -0.45 };
                }
            }
            let audio = audio_of(rate, planes, AudioMeta::default());
            let name = format!("{rate} Hz・{ch} ch・q{q}・{frames} フレーム");
            let bytes = encode(&audio, q, &mon).unwrap();
            let e = encode_packets(&audio, q, &mon).unwrap();
            let mut bodies: Vec<Vec<u8>> = e.headers.iter().map(|h| h.packet.clone()).collect();
            bodies.extend(e.packets.iter().map(|p| p.packet.clone()));
            let (ps, spans) = check_page_rules(
                &bytes,
                &bodies,
                &e.blockflags,
                e.blocksizes,
                frames as u64,
                &name,
            );
            let plan = page_plan(&e.packets, &e.blockflags, e.blocksizes, frames as u64).unwrap();
            let count = plan.len();
            let audio_at = |k: usize| k + 3;
            // A6′ の A5 の枝：最後の前が「長・次は短」なので、その前（count − 3）で閉じ、最後のページは 2 つのパケット
            assert!(plan[count - 2].window.long_to_short(), "{name}");
            assert_eq!(plan[count - 2].flush, Flush::Never, "{name}");
            assert_eq!(plan[count - 3].flush, Flush::After, "{name}");
            assert!(closed_after(&ps, &spans, audio_at(count - 3)), "{name}");
            assert_eq!(spans[audio_at(count - 2)].0, ps.len() - 1, "{name}");
            // A5 の途中の枝：最後の前より前の「長・次は短」のあとでは閉じない
            let mids: Vec<usize> = (1..count - 2)
                .filter(|&k| plan[k].window.long_to_short())
                .collect();
            assert!(!mids.is_empty(), "{name}");
            for &k in &mids {
                assert_eq!(plan[k].flush, Flush::Never, "{name}：音声 {k}");
                assert!(!closed_after(&ps, &spans, audio_at(k)), "{name}：音声 {k}");
            }
            // 途中のページ：PAGE_FILL で閉じたもの（A4・A6′ のほか）。グラニュール（A7）は check_page_rules が全ページで見る
            let filled = (1..count - 2)
                .filter(|&k| plan[k].flush == Flush::Auto && closed_after(&ps, &spans, audio_at(k)))
                .count();
            assert!(
                filled >= filled_min,
                "{name}：PAGE_FILL で閉じたページ {filled}"
            );
            eprintln!(
                "{name}：{count} パケット・{} ページ、途中の「長・次は短」{:?}、PAGE_FILL で閉じた途中のページ {filled}",
                ps.len(),
                mids
            );
        }

        // A5 の保留が書くバイト列を変える入力（コーデックのゴールデンの ogg-noise-22k-5ch-q2・44k-stereo-q10・44k-7ch-q1・
        // 48k-stereo-q9 と同じ値と品質）
        for (rate, ch, frames, seed, q) in [
            (22_050u32, 5u16, 4_000u64, 66u64, 2i64),
            (44_100, 2, 6_000, 70, 10),
            (44_100, 7, 4_000, 71, 1),
            (48_000, 2, 8_000, 72, 9),
        ] {
            let value = crate::testgen::InputGen::AudioNoise {
                rate,
                channels: ch,
                frames,
                seed,
            }
            .generate()
            .unwrap();
            let Value::Audio(audio) = value else {
                panic!("音声でない")
            };
            let name = format!("雑音 {rate} Hz・{ch} ch・q{q}・{frames} フレーム・種 {seed}");
            let bytes = encode(&audio, q, &mon).unwrap();
            let e = encode_packets(&audio, q, &mon).unwrap();
            let mut bodies: Vec<Vec<u8>> = e.headers.iter().map(|h| h.packet.clone()).collect();
            bodies.extend(e.packets.iter().map(|p| p.packet.clone()));
            let (ps, spans) =
                check_page_rules(&bytes, &bodies, &e.blockflags, e.blocksizes, frames, &name);
            // 「長・次は短」で本体が PAGE_FILL 以上になったのに閉じなかったところ（A5 がなければ閉じていた）
            let mode_bits = vorbis_tables::ov_ilog(e.blockflags.len() as u32 - 1) as u32;
            let ends = packet_end_offsets(&ps);
            let count = e.packets.len();
            let held: Vec<usize> = (1..count.saturating_sub(1))
                .filter(|&k| {
                    let w = packet_window(&e.packets[k].packet, mode_bits, &e.blockflags).unwrap();
                    w.long_to_short()
                        && !closed_after(&ps, &spans, k + 3)
                        && ends[k + 3] >= PAGE_FILL
                })
                .collect();
            assert!(!held.is_empty(), "{name}：A5 の保留が効いたところがない");
            eprintln!(
                "{name}：{count} パケット・{} ページ、A5 で保留したところ {held:?}",
                ps.len()
            );
        }
    }

    /// 大きなコメントはページをまたぎ（またいだページは −1）、setup は同じページに続く。
    #[test]
    fn mux_handles_big_comments() {
        let headers = [header(1, 30), header(3, 100_000), header(5, 4_000)];
        let w = windows_of(&[false, false]);
        let mut r = crate::determinism::rng(1);
        let audio: Vec<Vec<u8>> = w.iter().map(|w| fake_packet(*w, 1, 1, &mut r)).collect();
        let bytes = mux(
            &headers,
            &ogg_packets(&audio),
            &[false, true],
            [256, 2048],
            100,
            9,
            &NoMonitor,
        )
        .unwrap();
        let mut bodies: Vec<Vec<u8>> = headers.iter().map(|h| h.packet.clone()).collect();
        bodies.extend(audio);
        let (ps, spans) = check_page_rules(
            &bytes,
            &bodies,
            &[false, true],
            [256, 2048],
            100,
            "big comment",
        );
        assert_eq!(spans[1], (1, 2));
        assert_eq!(ps[1].granule, -1);
        assert_eq!(spans[2], (2, 2));
        assert_eq!(ps[2].flags & FLAG_CONTINUED, FLAG_CONTINUED);
        assert_eq!(ps.len(), 5);
        assert!(ps.iter().all(|p| p.serial == 9));
    }

    /// 音声パケットの列がおかしいとき（なし・旗が隣と合わない・N が最後のパケットの範囲の外・ヘッダが混ざる・設定が
    /// おかしい）は Internal。
    #[test]
    fn mux_rejects_inconsistent_streams() {
        let headers = [header(1, 30), header(3, 20), header(5, 100)];
        let mut r = crate::determinism::rng(2);
        let mut mk = |ws: &[PacketWindow]| {
            ogg_packets(
                &ws.iter()
                    .map(|w| fake_packet(*w, 1, 10, &mut r))
                    .collect::<Vec<_>>(),
            )
        };
        let flags = [false, true];
        let bs = [256i64, 2048];
        let good = windows_of(&[false, true, true, false]);
        let c = centers_of(&good, bs);
        let n = (c[2] + 1) as u64;
        let bad = |r: Result<Vec<u8>, EngineError>| matches!(r, Err(EngineError::Internal(_)));
        assert!(mux(&headers, &mk(&good), &flags, bs, n, 1, &NoMonitor).is_ok());
        assert!(bad(mux(&headers, &[], &flags, bs, 0, 1, &NoMonitor)));
        // N が範囲の外（c_{K−2} 以下・c_{K−1} より大きい）
        assert!(bad(mux(
            &headers,
            &mk(&good),
            &flags,
            bs,
            c[2] as u64,
            1,
            &NoMonitor
        )));
        assert!(bad(mux(
            &headers,
            &mk(&good),
            &flags,
            bs,
            (c[3] + 1) as u64,
            1,
            &NoMonitor,
        )));
        // 1 パケットなら N = 0 だけ
        let one = windows_of(&[false]);
        assert!(mux(&headers, &mk(&one), &flags, bs, 0, 1, &NoMonitor).is_ok());
        assert!(bad(mux(&headers, &mk(&one), &flags, bs, 1, 1, &NoMonitor)));
        // 旗が隣と合わない
        let mut wrong = good.clone();
        wrong[1].prev_long = true;
        assert!(bad(mux(
            &headers,
            &mk(&wrong),
            &flags,
            bs,
            n,
            1,
            &NoMonitor
        )));
        let mut wrong = good.clone();
        wrong[2].next_long = true;
        assert!(bad(mux(
            &headers,
            &mk(&wrong),
            &flags,
            bs,
            n,
            1,
            &NoMonitor
        )));
        // ヘッダのような頭・mode の表・ブロックの大きさがおかしい
        let mut pk = mk(&good);
        pk[1].packet[0] |= 1;
        assert!(bad(mux(&headers, &pk, &flags, bs, n, 1, &NoMonitor)));
        assert!(bad(mux(&headers, &mk(&good), &[], bs, n, 1, &NoMonitor)));
        assert!(bad(mux(
            &headers,
            &mk(&good),
            &flags,
            [2048, 256],
            n,
            1,
            &NoMonitor
        )));
        assert!(bad(mux(
            &headers,
            &mk(&good),
            &flags,
            [256, 3000],
            n,
            1,
            &NoMonitor
        )));
    }

    // ---- ブロックの解析の並列（mod.rs の規則 12） ----

    /// 直列の C の形（`vorbis_analysis_blockout` → `vorbis_analysis` を 1 ブロックずつ）で作ったパケットの列。
    fn serial_packets(audio: &Audio, quality: i64) -> Vec<OggPacket> {
        let Prepared { mut vd, .. } = prepare(audio, quality, &crate::monitor::NoMonitor).unwrap();
        let mut vb = block::vorbis_block_init(&vd);
        let mut st = crate::codec::vorbis::mapping0::AotuvState::new(audio.channels());
        let mut out = Vec::new();
        while block::vorbis_analysis_blockout(&mut vd, &mut vb).unwrap() {
            out.push(block::vorbis_analysis(&mut vb, &vd, &mut st).unwrap());
        }
        out
    }

    /// 並列の書き出しは、区間の大きさ（1・2・7・64・既定・全体）とスレッド数（1・3・16）によらず同じパケットの列で、
    /// 直列の C の形とも同じ（パケット・granulepos・EOS・packetno）。窓の長短・ampmax の再帰・aoTuV の状態（窓の状態・M5 の
    /// lowcomp・M3/M9 の lastmdct・M3 の tempmdct）が区間をまたぐ入力で。
    #[test]
    fn parallel_analysis_matches_serial_for_any_segments_and_threads() {
        for (kind, rate, ch, q, frames, seed) in [
            (Signal::Music, 44_100u32, 2usize, 5i64, 30_000usize, 1u64),
            (Signal::Impulses, 48_000, 6, 0, 20_000, 2),
            (Signal::Music, 16_000, 1, 10, 12_000, 3),
            (Signal::Noise, 8_000, 3, -1, 5_000, 4),
            (Signal::Sine, 22_050, 2, 3, 70, 5),
            (Signal::Silence, 44_100, 1, 5, 0, 6),
            // aoTuV の M2・M3：impulse の続き（M3 の tempmdct）が区間をまたぐ
            (Signal::Bursts, 44_100, 2, 0, 40_000, 7),
            (Signal::Bursts, 32_000, 1, -1, 30_000, 8),
        ] {
            let planes = oracle::signal(kind, rate, ch, frames, seed);
            let audio = audio_of(rate, planes, AudioMeta::default());
            let want = serial_packets(&audio, q);
            let name = format!("{kind:?}・{rate} Hz・{ch} ch・q{q}・{frames} フレーム");
            assert!(!want.is_empty(), "{name}");
            for threads in [1usize, 3, 16] {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                for segment in [Some(1), Some(2), Some(7), Some(64), None, Some(usize::MAX)] {
                    let got = pool
                        .install(|| {
                            encode_packets_with(&audio, q, &crate::monitor::NoMonitor, segment)
                        })
                        .unwrap();
                    assert_eq!(
                        got.packets.len(),
                        want.len(),
                        "{name}：{threads} スレッド・区間 {segment:?}"
                    );
                    for (k, (g, w)) in got.packets.iter().zip(&want).enumerate() {
                        assert!(
                            g == w,
                            "{name}：{threads} スレッド・区間 {segment:?} の音声パケット {k} が直列と違う"
                        );
                    }
                }
            }
        }
    }

    /// 区間の大きさの目安：チャンネルが多いほど区間のブロックは少なく、いつも 1 以上。
    #[test]
    fn segment_blocks_bounds_memory() {
        assert_eq!(segment_blocks(2, 2048), 512);
        assert_eq!(segment_blocks(8, 2048), 128);
        assert_eq!(segment_blocks(1, 512), 4096);
        assert_eq!(segment_blocks(8, 1 << 40), 1);
        assert_eq!(segment_blocks(0, 0), SEGMENT_SAMPLES);
    }

    /// 中止：書き出しの途中で中止を頼まれたら `Canceled`。
    #[test]
    fn encode_can_be_canceled() {
        let flag = crate::monitor::CancelFlag::default();
        flag.cancel();
        let planes = oracle::signal(Signal::Music, 44_100, 2, 50_000, 9);
        let audio = audio_of(44_100, planes, AudioMeta::default());
        assert!(matches!(
            encode(&audio, 5, &flag),
            Err(EngineError::Canceled)
        ));
        // 途中（計画のあと、ブロックの解析の中）で
        let after = crate::monitor::CancelAfter::new(40);
        assert!(matches!(
            encode(&audio, 5, &after),
            Err(EngineError::Canceled)
        ));
        assert!(after.calls() >= 40);
    }

    /// 安く作れる長い入力（雑音のような値。`oracle::signal` は sin を標本ごとに呼ぶのでデバッグのビルドでは遅い）。
    fn cheap_audio(rate: u32, ch: usize, frames: usize) -> Audio {
        let planes = (0..ch)
            .map(|c| {
                (0..frames)
                    .map(|i| {
                        let x = (i as u32)
                            .wrapping_mul(2_654_435_761)
                            .wrapping_add(c as u32 * 40_503)
                            >> 8;
                        (x as f32 / 16_777_216.0 - 0.5) * 0.5
                    })
                    .collect()
            })
            .collect();
        audio_of(rate, planes, AudioMeta::default())
    }

    /// 中止が早く効く：入力を一度に渡すので、写し・前の外挿（入力の全体の自己相関）・最初の計画（入力の全体の
    /// envelope）が長い入力では秒の単位になる。その中でも入力の長さに比例した回数だけ中止を確かめ、頼まれていれば途中で
    /// `Canceled` を返す（時間ではなく `canceled()` の呼び出しの数で見る）。[`prepare`] が呼ぶ数は入力と設定で決まるので
    /// 等式で確かめる：確かめの場所（pcm の確保の前・写しの [`COPY_CHUNK`] フレームごと・前の外挿の `par_blocks` の仕事
    /// ごと・その和の `block::CANCEL_SAMPLES` 標本ごと）のどれか 1 つが抜けても数が合わない。
    #[test]
    fn cancel_is_seen_inside_prepare_and_the_first_plan() {
        use crate::monitor::CancelAfter;
        let (rate, ch, frames) = (44_100u32, 2usize, 44_100 * 20);
        let audio = cheap_audio(rate, ch, frames);
        // 最初から中止：prepare も encode も最初の確かめ（pcm の確保の前）で戻る
        let first = CancelAfter::new(1);
        assert!(matches!(
            prepare(&audio, 5, &first),
            Err(EngineError::Canceled)
        ));
        assert_eq!(first.calls(), 1);
        let first = CancelAfter::new(1);
        assert!(matches!(
            encode(&audio, 5, &first),
            Err(EngineError::Canceled)
        ));
        assert_eq!(first.calls(), 1);

        // prepare が canceled() を呼ぶ数：確保の前の 1 回 ＋ 写し（チャンネルごとに COPY_CHUNK フレームずつ）＋ 前の外挿の
        // par_blocks の仕事（チャンネル × 17 のずれ）ごとに 1 回 ＋ ずれ j の和（長さは作業長 − j）を CANCEL_SAMPLES 標本
        // ずつに区切った数（切り上げ）。前の外挿の作業長は入力の全体（pcm_current − centerW = frames）。wrote(0) では
        // 前の外挿を済ませているので確かめない
        const LAGS: usize = 17;
        let prealloc = 1usize;
        let copy = ch * frames.div_ceil(COPY_CHUNK);
        let jobs = ch * LAGS;
        let sums = ch
            * (0..LAGS)
                .map(|j| (frames - j).div_ceil(block::CANCEL_SAMPLES))
                .sum::<usize>();
        let count = CancelAfter::new(0);
        let Prepared { mut vd, .. } = prepare(&audio, 5, &count).unwrap();
        assert_eq!(i64::from(vd.eofflag) - vd.center_w, frames as i64);
        assert_eq!(
            count.calls() as usize,
            prealloc + copy + jobs + sums,
            "確保の前 {prealloc}・写し {copy}・前の外挿の仕事 {jobs}・和の区切り {sums}"
        );
        // 写しの最後の確かめで中止を頼むと、前の外挿（並列の仕事）に入らずに、ちょうどそこで戻る
        let end_of_copy = CancelAfter::new((prealloc + copy) as u64);
        assert!(matches!(
            prepare(&audio, 5, &end_of_copy),
            Err(EngineError::Canceled)
        ));
        assert_eq!(end_of_copy.calls() as usize, prealloc + copy);
        // 前の外挿の途中で中止を頼むと、そこで Canceled（写しが終わったあと、自己相関の最初の仕事の中）
        let mid = CancelAfter::new((prealloc + copy + 2) as u64);
        assert!(matches!(
            prepare(&audio, 5, &mid),
            Err(EngineError::Canceled)
        ));

        // 最初の計画は入力の全体の envelope を 1 回で求め、その中で envelope::CANCEL_STEPS 歩ごとに確かめる
        let steps = (frames / 64) as i32;
        let count = CancelAfter::new(0);
        assert!(
            block::vorbis_analysis_plan(&mut vd, &count)
                .unwrap()
                .is_some()
        );
        assert!(
            count.calls() as i32 >= steps / super::super::envelope::CANCEL_STEPS - 1,
            "{} 回",
            count.calls()
        );
        // 最初の計画の途中（2 回目の確かめ。入力の 3 分の 1 ほどのところ）で中止を頼むと、計画が Canceled を返す
        let Prepared { mut vd, .. } = prepare(&audio, 5, &NoMonitor).unwrap();
        let stop = CancelAfter::new(2);
        assert!(matches!(
            block::vorbis_analysis_plan(&mut vd, &stop),
            Err(EngineError::Canceled)
        ));
        assert_eq!(stop.calls(), 2);
    }

    /// 書き出しの終わり：シリアル番号（入力の全体のダイジェスト）の前と後と、ダイジェストの塊（`value::DIGEST_CHUNK`
    /// 標本）ごとに中止を確かめ、mux は音声パケット [`MUX_CANCEL_EVERY`] 個ごとに確かめる。encode が encode_packets より
    /// 多く呼ぶ数は入力と設定で決まる（2 ＋ チャンネル × ceil(フレーム数 / DIGEST_CHUNK) ＋ ⌊パケットの数 /
    /// MUX_CANCEL_EVERY⌋）。ダイジェストの途中で頼んだ中止は、ダイジェストの終わりを待たずにそこで戻る。
    #[test]
    fn cancel_is_seen_around_and_inside_the_serial_digest() {
        use crate::monitor::CancelAfter;
        use crate::value::DIGEST_CHUNK;
        let (ch, frames) = (2usize, 40_000usize);
        let audio = cheap_audio(22_050, ch, frames);
        let digest_chunks = ch * frames.div_ceil(DIGEST_CHUNK);
        assert!(digest_chunks >= 4);
        let a = CancelAfter::new(0);
        let e = encode_packets(&audio, 3, &a).unwrap();
        let b = CancelAfter::new(0);
        encode(&audio, 3, &b).unwrap();
        let before = a.calls();
        assert_eq!(
            b.calls() as usize,
            before as usize + 2 + digest_chunks + e.packets.len() / MUX_CANCEL_EVERY
        );
        // ダイジェストの前の確かめ・最初のチャンネルの最後の塊・後の確かめで頼んだ中止は、どれもちょうどそこで戻る
        // （ダイジェストは直列なので、呼ぶ数が頼んだ回数と同じ）
        for k in [
            before + 1,
            before + 1 + frames.div_ceil(DIGEST_CHUNK) as u64,
            before + 2 + digest_chunks as u64,
        ] {
            let stop = CancelAfter::new(k);
            assert!(matches!(
                encode(&audio, 3, &stop),
                Err(EngineError::Canceled)
            ));
            assert_eq!(stop.calls(), k);
        }
    }

    /// mux の中止：音声パケットを [`MUX_CANCEL_EVERY`] 個書くごとに確かめ、頼まれていればそこで `Canceled`（書き終えて
    /// からではなく）。確かめるだけで、書くバイト列は変わらない。
    #[test]
    fn mux_checks_cancel_every_few_hundred_packets() {
        use crate::monitor::CancelAfter;
        let mut r = crate::determinism::rng(5);
        let count = MUX_CANCEL_EVERY * 2 + 7;
        let longs = vec![false; count];
        let w = windows_of(&longs);
        let audio: Vec<Vec<u8>> = w.iter().map(|w| fake_packet(*w, 1, 3, &mut r)).collect();
        let pkts = ogg_packets(&audio);
        let headers = [header(1, 30), header(3, 60), header(5, 400)];
        let bs = [256i64, 2048];
        let total = centers_of(&w, bs)[count - 1] as u64;
        let want = mux(&headers, &pkts, &[false, true], bs, total, 3, &NoMonitor).unwrap();
        let n = CancelAfter::new(0);
        let got = mux(&headers, &pkts, &[false, true], bs, total, 3, &n).unwrap();
        assert!(got == want);
        assert_eq!(n.calls() as usize, count / MUX_CANCEL_EVERY);
        for k in 1..=(count / MUX_CANCEL_EVERY) as u64 {
            let stop = CancelAfter::new(k);
            assert!(matches!(
                mux(&headers, &pkts, &[false, true], bs, total, 3, &stop),
                Err(EngineError::Canceled)
            ));
            assert_eq!(stop.calls(), k);
        }
    }

    /// Ogg のバイト列のシリアル番号と CRC を 0 にしたもの（ページの見出しの 14..18 と 22..26）。
    fn without_serial_and_crc(bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        let mut at = 0usize;
        while at < out.len() {
            assert_eq!(&out[at..at + 4], b"OggS");
            let nseg = usize::from(out[at + 26]);
            let body: usize = out[at + 27..at + 27 + nseg]
                .iter()
                .map(|&l| usize::from(l))
                .sum();
            out[at + 14..at + 18].fill(0);
            out[at + 22..at + 26].fill(0);
            at += 27 + nseg + body;
        }
        assert_eq!(at, out.len());
        out
    }

    /// 非有限の標本の規則（冒頭の流れの 2。変えるとバイト列が変わる）：実際の書き出し（[`prepare`] の経路）
    /// で、NaN（−NaN・ペイロード付きの NaN も）と ±∞ を含む入力と、それを 0・±1 に置き換えた入力が、シリアル番号と CRC
    /// （シリアル番号は入力のダイジェストから決まるので違う）を除いてバイト一致する。1 を超える有限の値は切らない
    /// （1.5 と 1.0 は違うバイト列）。
    #[test]
    fn non_finite_samples_encode_as_tamed_values() {
        let specials = [
            (f32::NAN, 0.0f32),
            (-f32::NAN, 0.0),
            (f32::from_bits(0x7f80_0001), 0.0),
            (f32::from_bits(0xffc0_1234), 0.0),
            (f32::from_bits(0x7fff_ffff), 0.0),
            (f32::INFINITY, 1.0),
            (f32::NEG_INFINITY, -1.0),
        ];
        let rate = 44_100u32;
        let base = oracle::signal(Signal::Music, rate, 2, 11_025, 31);
        let mut wild = base.clone();
        let mut tame = base.clone();
        let mut k = 0usize;
        for c in 0..2 {
            for i in (100 + 37 * c..11_025).step_by(613) {
                let (w, t) = specials[k % specials.len()];
                wild[c][i] = w;
                tame[c][i] = t;
                k += 1;
            }
        }
        assert!(k >= 2 * specials.len());
        assert!(wild.iter().flatten().any(|x| x.is_nan()));
        let wild = audio_of(rate, wild, AudioMeta::default());
        let tame = audio_of(rate, tame, AudioMeta::default());
        let mon = crate::monitor::NoMonitor;
        for q in [-1i64, 5, 10] {
            let a = encode(&wild, q, &mon).unwrap();
            let b = encode(&tame, q, &mon).unwrap();
            assert_ne!(
                serial_of(&wild, q, &NoMonitor).unwrap(),
                serial_of(&tame, q, &NoMonitor).unwrap()
            );
            assert!(
                without_serial_and_crc(&a) == without_serial_and_crc(&b),
                "q{q}：非有限の値を含む入力と 0・±1 に置き換えた入力の書き出しが違う"
            );
        }
        // 1 を超える有限の値はそのまま渡す（±1 で切らない）
        let mut over = base.clone();
        let mut one = base;
        over[0][5_000] = 1.5;
        one[0][5_000] = 1.0;
        let a = encode(&audio_of(rate, over, AudioMeta::default()), 5, &mon).unwrap();
        let b = encode(&audio_of(rate, one, AudioMeta::default()), 5, &mon).unwrap();
        assert!(without_serial_and_crc(&a) != without_serial_and_crc(&b));
    }

    // ---- 照合（KURA_VORBIS_ORACLE があるときだけ） ----

    static TEMP_N: AtomicU64 = AtomicU64::new(0);

    /// ffmpeg の置き場所（KURA_FFMPEG があるときだけ。値が実在するファイルならそれ、でなければ /opt/homebrew/bin/ffmpeg）。
    fn ffmpeg() -> Option<PathBuf> {
        let v = std::env::var_os("KURA_FFMPEG")?;
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
        let d = PathBuf::from("/opt/homebrew/bin/ffmpeg");
        d.is_file().then_some(d)
    }

    /// ffmpeg で読んだ planes（`cpuflags0` で `-cpuflags 0`）。ffmpeg の並びは WAV と同じ（kura の planes の並び）。
    fn ffmpeg_decode(
        ffmpeg: &Path,
        bytes: &[u8],
        channels: usize,
        cpuflags0: bool,
    ) -> Vec<Vec<f32>> {
        let n = TEMP_N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("kura-ogg-mux-{}-{n}.ogg", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-v", "error", "-nostdin"]);
        if cpuflags0 {
            cmd.args(["-cpuflags", "0"]);
        }
        cmd.arg("-i")
            .arg(&path)
            .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"]);
        let out = cmd.output().unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "ffmpeg：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout.len() % (4 * channels), 0);
        let mut planes = vec![Vec::new(); channels];
        for (i, b) in out.stdout.chunks_exact(4).enumerate() {
            planes[i % channels].push(f32::from_le_bytes(b.try_into().unwrap()));
        }
        planes
    }

    /// symphonia（kura の Ogg Vorbis の読み口）で読む。
    fn decode_symphonia(bytes: &[u8]) -> Arc<Audio> {
        let (v, _) = crate::codec::CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes.to_vec()),
                "ogg",
                &crate::doc::DecodeOptions::default(),
                &crate::limits::Limits::default(),
                &crate::monitor::NoMonitor,
            )
            .unwrap_or_else(|e| panic!("symphonia で読めません：{e}"));
        match v {
            Value::Audio(a) => a,
            _ => panic!("音声ではありません"),
        }
    }

    /// lewton の低い層で読む：音声パケットごとの出力の長さと、つないだ planes（最後のグラニュールで切る前）。
    fn decode_lewton(bodies: &[Vec<u8>]) -> (Vec<u64>, Vec<Vec<f32>>) {
        let ident = lewton::header::read_header_ident(&bodies[0]).unwrap();
        lewton::header::read_header_comment(&bodies[1]).unwrap();
        let setup = lewton::header::read_header_setup(
            &bodies[2],
            ident.audio_channels,
            (ident.blocksize_0, ident.blocksize_1),
        )
        .unwrap();
        let mut pwr = lewton::audio::PreviousWindowRight::new();
        let mut planes: Vec<Vec<f32>> = vec![Vec::new(); usize::from(ident.audio_channels)];
        let mut outs = Vec::new();
        for p in &bodies[3..] {
            let out: Vec<Vec<f32>> =
                lewton::audio::read_audio_packet_generic(&ident, &setup, p, &mut pwr).unwrap();
            outs.push(out.first().map_or(0, Vec::len) as u64);
            for (dst, src) in planes.iter_mut().zip(out) {
                dst.extend_from_slice(&src);
            }
        }
        (outs, planes)
    }

    /// 照合の記録（範囲に届いたかを確かめる）。
    #[derive(Debug, Default)]
    struct Stats {
        cases: usize,
        long: usize,
        short: usize,
        long_to_short: usize,
        single_mode: usize,
        stereo: usize,
        uncoupled: usize,
        a5_before_last: usize,
        /// lewton の読み口の長さが N にならず、それがページの割り方で避けられない場合（あってはならない：各テストが空を確かめる）。
        unavoidable: Vec<String>,
        lacing_full: usize,
        pages: usize,
    }

    /// 照合の 1 件。
    #[derive(Debug, Clone, Copy)]
    struct Case {
        kind: Signal,
        rate: u32,
        ch: usize,
        q: i32,
        frames: usize,
        seed: u64,
        dump: bool,
    }

    /// 照合用のエンコーダのパケットを自分の mux で Ogg にし、規則・グラニュール・ダンプの窓・デコーダの長さを確かめる。
    /// （Ogg のバイト列, lewton の planes（Vorbis の並び・N で切ったもの））を返す。照合用のエンコーダがなければ None。
    fn run_case(c: Case, stats: &mut Stats) -> Option<(Vec<u8>, Vec<Vec<f32>>)> {
        let Case {
            kind,
            rate,
            ch,
            q,
            frames,
            seed,
            dump,
        } = c;
        let input = oracle::signal(kind, rate, ch, frames, seed);
        let (pkts, dmp) = oracle::encode(&input, rate, q, dump.then_some(1))?;
        let json = oracle::setup(rate, ch, q, false)?;
        let name = format!("{kind:?}・{rate} Hz・{ch} ch・q{q}・{frames} フレーム");
        let blocksizes: [i64; 2] = [
            json["blocksizes"][0].as_i64().unwrap(),
            json["blocksizes"][1].as_i64().unwrap(),
        ];
        let blockflags: Vec<bool> = json["modes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["blockflag"].as_i64().unwrap() != 0)
            .collect();
        let template = json["template"]["name"].as_str().unwrap_or("");
        if template.ends_with("_stereo") {
            stats.stereo += 1;
        } else if template.ends_with("_uncoupled") {
            stats.uncoupled += 1;
        }
        if blockflags.len() == 1 {
            stats.single_mode += 1;
        }
        let headers: [OggPacket; 3] = [0usize, 1, 2].map(|i| OggPacket {
            packet: pkts[i].body.clone(),
            e_o_s: false,
            granulepos: pkts[i].granule,
            packetno: i as i64,
        });
        let audio: Vec<OggPacket> = pkts[3..]
            .iter()
            .enumerate()
            .map(|(k, p)| OggPacket {
                packet: p.body.clone(),
                e_o_s: p.flags & 2 != 0,
                granulepos: p.granule,
                packetno: k as i64 + 3,
            })
            .collect();
        let plan = page_plan(&audio, &blockflags, blocksizes, frames as u64).unwrap();
        // グラニュール：libvorbis のパケットの granulepos と、窓の旗から数えた c_k（最後は N）が同じ
        for (k, (p, pl)) in audio.iter().zip(&plan).enumerate() {
            assert_eq!(p.granulepos, pl.granule, "{name}：音声 {k} のグラニュール");
            assert_eq!(p.e_o_s, pl.eos, "{name}：音声 {k} の EOS");
            if pl.window.long {
                stats.long += 1;
            } else {
                stats.short += 1;
            }
            if pl.window.long_to_short() {
                stats.long_to_short += 1;
            }
        }
        // ダンプの窓（BLCK：W・lW・nW・block_mode・granulepos）と、パケットの頭から読んだ窓が同じ
        if let Some(d) = &dmp {
            assert_eq!(d.blocks.len(), audio.len(), "{name}");
            for (k, pl) in plan.iter().enumerate() {
                let b = d.block_info(k);
                assert_eq!(b[1] != 0, pl.window.long, "{name}：ブロック {k} の W");
                // libvorbis の mode は 0 が短・1 が長（mode が 1 つなら 0）＝ W
                assert_eq!(
                    b[1] as usize, pl.window.mode,
                    "{name}：ブロック {k} の mode"
                );
                assert_eq!(
                    b[5],
                    b[4] | (b[1] << 1),
                    "{name}：ブロック {k} の block_mode"
                );
                if pl.window.long {
                    assert_eq!(
                        (b[2] != 0, b[3] != 0),
                        (pl.window.prev_long, pl.window.next_long),
                        "{name}：ブロック {k} の lW・nW"
                    );
                }
                assert_eq!(b[6], pl.granule, "{name}：ブロック {k} の granulepos");
            }
        }
        let bytes = mux(
            &headers,
            &audio,
            &blockflags,
            blocksizes,
            frames as u64,
            0x6b75_7261,
            &NoMonitor,
        )
        .unwrap();
        let bodies: Vec<Vec<u8>> = pkts.iter().map(|p| p.body.clone()).collect();
        let (ps, spans) = check_page_rules(
            &bytes,
            &bodies,
            &blockflags,
            blocksizes,
            frames as u64,
            &name,
        );
        stats.pages += ps.len();
        if ps.iter().any(|p| p.lacing.len() == MAX_SEGMENTS) {
            stats.lacing_full += 1;
        }
        let count = audio.len();
        if count >= 3 && plan[count - 2].window.long_to_short() {
            stats.a5_before_last += 1;
        }
        // symphonia：長さ・チャンネル・レート
        let sym = decode_symphonia(&bytes);
        assert_eq!(sym.frames(), frames, "{name}：symphonia の長さ");
        assert_eq!((sym.channels(), sym.rate()), (ch, rate), "{name}");
        // lewton（低い層。最後のページのグラニュールで切る）：長さと、symphonia との値の差
        let (outs, mut lew) = decode_lewton(&bodies);
        for p in &mut lew {
            assert!(
                p.len() >= frames,
                "{name}：lewton の出力が N より短い（{}）",
                p.len()
            );
            p.truncate(frames);
        }
        // 値：symphonia（planes は kura の並び＝WAV）と lewton（Vorbis の並び）が、すべてのチャンネルの数で 1e-4 以内。
        // symphonia は third_party/ の直した写し（residue 0・1 を 3 チャンネル以上で読むときの分類の語のはみ出しを直したもの。
        // 読み取りの識別子 /2）を使うので、3 チャンネル以上（uncoupled）も lewton と同じ値になる
        for c in 0..ch {
            let d = sym.planes()[ORDER[ch - 1][c]]
                .iter()
                .zip(&lew[c])
                .map(|(x, y)| (x - y).abs())
                .fold(0f32, crate::math::max32);
            assert!(
                d <= 1e-4,
                "{name}：チャンネル {c} で symphonia と lewton の差 {d}"
            );
        }
        // lewton の読み口（OggStreamReader）の数え方の写しでも N。合わないなら、ページの割り方で避けられない場合
        // （「長・次は短」のあとの短いブロックで終わり、最後の詰め物が lewton の余分より大きい）だけを記録し、それも
        // 各テストの最後で 0 件であることを確かめる（今の照合の入力では起きない）
        let reader = lewton_reader_total(&outs, &ps, &spans);
        if reader != frames as u64 {
            let k = count - 2;
            let extra = (blocksizes[1] - blocksizes[0]) / 4;
            let unavoidable = count >= 3
                && plan[k].window.long_to_short()
                && (frames as i64) - plan[k].center < extra;
            assert!(unavoidable, "{name}：lewton の読み口の長さ {reader}");
            stats.unavoidable.push(format!("{name}：{reader}"));
        }
        stats.cases += 1;
        Some((bytes, lew))
    }

    const SIGNALS: [Signal; 5] = [
        Signal::Music,
        Signal::Noise,
        Signal::Sine,
        Signal::Silence,
        Signal::Impulses,
    ];
    const RATES: [u32; 9] = [
        8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
    ];
    const CHANNELS: [usize; 5] = [1, 2, 3, 6, 8];
    const QS: [i32; 5] = [-1, 0, 3, 5, 10];

    /// 照合：すべてのレート × チャンネル（stereo・uncoupled のすべてのテンプレート）で、照合用のエンコーダのパケットを
    /// 自分の Ogg にして、規則・グラニュール・ダンプの窓・symphonia と lewton の長さが合う。
    #[test]
    fn oracle_mux_all_templates() {
        let mut stats = Stats::default();
        for (ri, &rate) in RATES.iter().enumerate() {
            for (ci, &ch) in CHANNELS.iter().enumerate() {
                // 2 秒（96 kHz 以上の 6・8 ch は 1 秒）
                let secs = if rate >= 96_000 && ch >= 6 { 1 } else { 2 };
                let case = Case {
                    kind: SIGNALS[(ri * 2 + ci) % SIGNALS.len()],
                    rate,
                    ch,
                    q: QS[(ri + ci) % QS.len()],
                    frames: rate as usize * secs + 123,
                    seed: 1 + ri as u64,
                    dump: ri % 3 == 0,
                };
                if run_case(case, &mut stats).is_none() {
                    return;
                }
            }
        }
        assert_eq!(stats.cases, 45);
        assert!(stats.stereo == 9 && stats.uncoupled == 36, "{stats:?}");
        assert!(
            stats.single_mode > 0 && stats.long > 0 && stats.short > 0 && stats.long_to_short > 0,
            "{stats:?}"
        );
        assert!(stats.unavoidable.is_empty(), "{stats:?}");
        eprintln!("oracle_mux_all_templates：{stats:?}");
    }

    /// 照合：入力の長さ（0・1・37・1,000・4,410・44,101・3 秒）と品質・信号を回す。
    #[test]
    fn oracle_mux_lengths() {
        let mut stats = Stats::default();
        let configs = [
            (44_100u32, 2usize),
            (8_000, 1),
            (48_000, 6),
            (22_050, 3),
            (192_000, 8),
            (32_000, 2),
            (16_000, 1),
        ];
        let mut i = 0usize;
        for &(rate, ch) in &configs {
            for frames in [0usize, 1, 37, 1_000, 4_410, 44_101, rate as usize * 3] {
                let case = Case {
                    kind: SIGNALS[(i / 2) % SIGNALS.len()],
                    rate,
                    ch,
                    q: QS[i % QS.len()],
                    frames,
                    seed: 100 + i as u64,
                    dump: i.is_multiple_of(4),
                };
                i += 1;
                if run_case(case, &mut stats).is_none() {
                    return;
                }
            }
        }
        assert_eq!(stats.cases, configs.len() * 7);
        assert!(stats.unavoidable.is_empty(), "{stats:?}");
        eprintln!("oracle_mux_lengths：{stats:?}");
    }

    /// 照合：短い窓の多い入力（Impulses・Music）と無音（小さなパケットで lacing が埋まる）を、品質ごとに。
    #[test]
    fn oracle_mux_short_windows_and_tiny_packets() {
        let mut stats = Stats::default();
        let mut i = 0usize;
        for &rate in &[16_000u32, 22_050, 32_000, 44_100, 48_000, 96_000] {
            for &q in &QS {
                for kind in [Signal::Impulses, Signal::Music, Signal::Silence] {
                    i += 1;
                    let case = Case {
                        kind,
                        rate,
                        ch: [1usize, 2, 3][i % 3],
                        q,
                        frames: rate as usize * 2 + (i * 37) % 1_000,
                        seed: 500 + i as u64,
                        dump: false,
                    };
                    if run_case(case, &mut stats).is_none() {
                        return;
                    }
                }
            }
        }
        // 長い無音：1〜2 バイトのパケットが 255 個を超え、lacing でページが埋まる
        for (rate, ch, secs, q) in [
            (8_000u32, 1usize, 12usize, 0),
            (44_100, 2, 8, 5),
            (11_025, 3, 9, -1),
        ] {
            let case = Case {
                kind: Signal::Silence,
                rate,
                ch,
                q,
                frames: rate as usize * secs + 77,
                seed: 1,
                dump: false,
            };
            if run_case(case, &mut stats).is_none() {
                return;
            }
        }
        assert!(
            stats.long_to_short > 0 && stats.lacing_full > 0,
            "{stats:?}"
        );
        assert!(stats.unavoidable.is_empty(), "{stats:?}");
        eprintln!("oracle_mux_short_windows_and_tiny_packets：{stats:?}");
    }

    /// 照合＋ffmpeg（KURA_FFMPEG があるときだけ）：既定と `-cpuflags 0` で長さが入力とちょうど同じ。
    #[test]
    fn oracle_mux_ffmpeg_lengths() {
        let Some(ff) = ffmpeg() else {
            return;
        };
        let mut stats = Stats::default();
        let mut i = 0usize;
        for &(rate, ch) in &[
            (44_100u32, 2usize),
            (8_000, 1),
            (48_000, 6),
            (22_050, 3),
            (96_000, 8),
            (32_000, 2),
        ] {
            for frames in [0usize, 1, 37, 1_000, 4_410, 44_101, rate as usize * 2] {
                i += 1;
                let case = Case {
                    kind: SIGNALS[i % SIGNALS.len()],
                    rate,
                    ch,
                    q: QS[i % QS.len()],
                    frames,
                    seed: 900 + i as u64,
                    dump: false,
                };
                let Some((bytes, lew)) = run_case(case, &mut stats) else {
                    return;
                };
                for cpuflags0 in [false, true] {
                    let ffp = ffmpeg_decode(&ff, &bytes, ch, cpuflags0);
                    assert_eq!(
                        ffp[0].len(),
                        frames,
                        "{case:?}：ffmpeg の長さ（cpuflags 0 = {cpuflags0}）"
                    );
                    // 値も lewton と同じ（ffmpeg の並びは WAV＝kura の並び。3 チャンネル以上も）
                    for (c, b) in lew.iter().enumerate() {
                        let d = ffp[ORDER[ch - 1][c]]
                            .iter()
                            .zip(b)
                            .map(|(x, y)| (x - y).abs())
                            .fold(0f32, crate::math::max32);
                        assert!(
                            d <= 1e-4,
                            "{case:?}：チャンネル {c} で ffmpeg（cpuflags 0 = {cpuflags0}）と lewton の差 {d}"
                        );
                    }
                }
            }
        }
        assert!(stats.unavoidable.is_empty(), "{stats:?}");
        eprintln!("oracle_mux_ffmpeg_lengths：{stats:?}");
    }
}
