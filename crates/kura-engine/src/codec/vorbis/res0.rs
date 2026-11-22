//! residue 1・2 の look・分類・符号化（`lib/res0.c`）。stereo は residue 2、uncoupled（1・3〜8 チャンネル）は residue 1。
//!
//! # 移す C
//!
//! | 関数 | 行 | 備考 |
//! |---|---|---|
//! | `res0_look` | 255–313 | `partbooks[class][stage]`（secondstages のビットが立っている段だけ、booklist を順に）、`stages = max(ov_ilog(secondstages))`、`phrasebook = fullbooks[groupbook]`、`partvals = parts^dim`。`decodemap` はデコーダだけなので作らなくてよい |
//! | `_encodepart` | 380–404 | `step = n/dim` 回 `local_book_besterror`（codebook.rs）と `vorbis_book_encode` |
//! | `_01class` | 406–468 | partition ごと・チャンネルごとに max と `ent`（|値| の和）、`ent *= scale`（`scale = 100./samples_per_partition` を float に。`int *= float` は float で掛けて int に切り捨て）、`classmetric1`・`classmetric2` で class を選ぶ |
//! | `_2class` | 473–526 | 交互に並べた partition で、mag（チャンネル 0）と ang（残り）の |値| の最大で class を選ぶ（`l` は `begin/ch` から） |
//! | `_01forward` | 528–640 | 段 s ごとに、partition の語（段 0 だけ。`partitions_per_word` 個をまとめて phrasebook で。`val < entries` のときだけ書く）、partition ごと・チャンネルごとに、その class のその段の book があれば `_encodepart`。`resbits`・`resvals` は統計なので持たない |
//! | `res1_class` | 735–745 | nonzero のチャンネルだけを前に詰めて `_01class`（C は呼ぶ側の配列 `in` を詰め直す。mapping0.c:1287–1290 が forward の前に詰め直す） |
//! | `res1_forward` | 715–733 | 同じく詰めて `_01forward` |
//! | `res2_class` | 759–768 | nonzero が 1 つでもあれば `_2class`（全チャンネル） |
//! | `res2_forward` | 773–799 | 全チャンネルを交互に並べた 1 本（`work[k*ch + i] = in[i][k]`、`n = pcmend/2`）にして `_01forward`（1 チャンネル） |
//!
//! - residue の値は整数（`int **`。量子化した残差）。VQ の book はすべて整数の中心の格子（maptype 1）。`_01forward` は
//!   段ごとに選んだ格子の値を引いていく（cascade）ので、渡した配列は書き換わる（residue 1 は mapping0 の `iwork` そのもの、
//!   residue 2 は交互に並べた写し）。
//! - `TRAIN_RES`・`TRAIN_RESAUX` の枝は移さない。`look->frames`・`postbits`・`phrasebits` などの統計は持たない。
//!
//! # 移さないもの
//!
//! `res0_pack`（info.rs）、`res0_unpack`・`_01inverse`・`res*_inverse`（デコーダ）、`res0_free_*`。residue 0 の forward は
//! テンプレートが使わない。
//!
//! # 型の対応
//!
//! - `vorbis_look_residue0`（res0.c:37–63）→ [`VorbisLookResidue0`]。`codebook *phrasebook`・`codebook ***partbooks` は
//!   `CodecSetupInfo::fullbooks` の番号（`usize`）で持つ（本体を写さない）。
//! - 分類（`long **`）→ `Vec<Vec<i64>>`（[チャンネル][partition]）。
//!
//! # 移植の注記
//!
//! - `res1_forward` の「nonzero のチャンネルを前に詰める」は、C が呼ぶ側のポインタの配列を上書きするのに対し、
//!   `input` の中の参照を入れ替える（前の `used` 個は C と同じ。残りの並びは C と違うが、C も mapping0 も読まない）。
//! - 分類（`_01class`・`_2class`）は C と同じく失敗しない。範囲の外の読み取り（C では起きない）は 0 として読む。
//!   符号化（`_01forward`）は、設定と長さ（`grouping`・`partitions_per_word` が正、`begin`〜`end` が配列の中、分類が
//!   `partitions` 未満）を確かめ、外れれば `Err`。
//! - `abs` のあふれ（`abs(INT_MIN)`）・int の和のあふれは 2 の補数で折り返す（照合用の arm64 の動き）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use vorbis_tables::{VorbisInfoResidue0, ov_ilog};

use super::bitpack::OggpackBuffer;
use super::codebook::{Codebook, local_book_besterror, vorbis_book_encode};
use crate::error::EngineError;

/// `vorbis_look_residue0`（res0.c:37–63）の encode に要る欄。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VorbisLookResidue0 {
    pub info: VorbisInfoResidue0,
    pub parts: i32,
    pub stages: i32,
    /// `fullbooks` の番号（C の `codebook *phrasebook`）。
    pub phrasebook: usize,
    /// [class][stage] の `fullbooks` の番号（C の `codebook ***partbooks`。本なしは `None`）。
    pub partbooks: Vec<Vec<Option<usize>>>,
    pub partvals: i32,
}

fn bad_setup(why: &str) -> EngineError {
    EngineError::Internal(format!("residue の設定が正しくない：{why}"))
}

/// `res0_look`（res0.c:255–313）。
pub(crate) fn res0_look(
    info: &VorbisInfoResidue0,
    fullbooks: &[Codebook],
) -> Result<VorbisLookResidue0, EngineError> {
    let parts = usize::try_from(info.partitions)
        .ok()
        .filter(|&p| p <= info.secondstages.len())
        .ok_or_else(|| bad_setup("partitions が範囲の外"))?;
    let phrasebook = usize::try_from(info.groupbook)
        .ok()
        .filter(|&b| b < fullbooks.len())
        .ok_or_else(|| bad_setup("groupbook がない"))?;
    let dim = fullbooks[phrasebook].dim;

    let mut acc = 0usize;
    let mut maxstage = 0;
    let mut partbooks = Vec::with_capacity(parts);
    for &ss in &info.secondstages[..parts] {
        let stages = ov_ilog(ss as u32);
        let mut books = Vec::new();
        if stages != 0 {
            if stages > maxstage {
                maxstage = stages;
            }
            books = vec![None; stages as usize];
            for (k, slot) in books.iter_mut().enumerate() {
                if ss & (1 << k) != 0 {
                    let b = info
                        .booklist
                        .get(acc)
                        .and_then(|&b| usize::try_from(b).ok())
                        .filter(|&b| b < fullbooks.len())
                        .ok_or_else(|| bad_setup("booklist の本がない"))?;
                    acc += 1;
                    *slot = Some(b);
                }
            }
        }
        partbooks.push(books);
    }

    let mut partvals: i32 = 1;
    for _ in 0..dim {
        partvals = partvals
            .checked_mul(info.partitions)
            .ok_or_else(|| bad_setup("partvals があふれる"))?;
    }

    Ok(VorbisLookResidue0 {
        info: *info,
        parts: info.partitions,
        stages: maxstage,
        phrasebook,
        partbooks,
        partvals,
    })
}

/// `_encodepart`（res0.c:380–404）：`vec`（n 個）を book で書く。書いたビット数を返す。
fn encodepart(
    opb: &mut OggpackBuffer,
    vec: &mut [i32],
    n: i32,
    book: &Codebook,
) -> Result<i32, EngineError> {
    // local_book_besterror の前提（整数の中心の格子、次元 1〜8、delta ≠ 0）を満たさない本は C も正しく書けない
    let dim = usize::try_from(book.dim)
        .ok()
        .filter(|d| (1..=8).contains(d))
        .ok_or_else(|| bad_setup("residue の VQ の本の次元が 1〜8 でない"))?;
    if book.c.maptype != 1 || book.delta == 0 {
        return Err(bad_setup("residue の VQ の本が整数の格子でない"));
    }
    let step = usize::try_from(n / dim as i32).unwrap_or(0);
    if vec.len() < step * dim {
        return Err(EngineError::Internal(
            "residue：partition が配列の外".into(),
        ));
    }
    let mut bits = 0i32;

    for v in vec.chunks_exact_mut(dim).take(step) {
        let entry = local_book_besterror(book, v);
        bits = bits.wrapping_add(vorbis_book_encode(book, entry, opb)?);
    }

    Ok(bits)
}

/// `(begin, n, samples_per_partition, partvals)`（`_01class`・`_2class`・`_01forward` の冒頭の「move all this setup out
/// later」）。`samples_per_partition` が 0 以下なら None。
fn partition_setup(info: &VorbisInfoResidue0) -> Option<(i32, i32, i32, usize)> {
    let spp = info.grouping;
    if spp <= 0 {
        return None;
    }
    // C の `int n = info->end - info->begin;`（long → int）
    let n = info.end.wrapping_sub(info.begin) as i32;
    let partvals = usize::try_from(n / spp).unwrap_or(0);
    Some((info.begin as i32, n, spp, partvals))
}

/// `abs(in[j][k])`（範囲の外は 0。C では起きない）。
fn abs_at(v: &[i32], k: i64) -> i32 {
    usize::try_from(k)
        .ok()
        .and_then(|k| v.get(k))
        .map_or(0, |x| x.wrapping_abs())
}

/// `_01class`（res0.c:406–468）。
fn class_01(look: &VorbisLookResidue0, input: &[&[i32]], ch: usize) -> Vec<Vec<i64>> {
    let info = &look.info;
    let ch = ch.min(input.len());
    let Some((_begin, _n, samples_per_partition, partvals)) = partition_setup(info) else {
        return vec![Vec::new(); ch];
    };
    let possible_partitions = info.partitions;
    let mut partword = vec![vec![0i64; partvals]; ch];
    // double の割り算を float に
    let scale = (100. / f64::from(samples_per_partition)) as f32;

    // we find the partition type for each partition of each channel.
    for i in 0..partvals {
        // C の `int offset = i*samples_per_partition + info->begin;`（long の和を int に）
        let offset = i64::from((i as i64 * i64::from(samples_per_partition) + info.begin) as i32);
        for (j, pw) in partword.iter_mut().enumerate() {
            let mut max: i32 = 0;
            let mut ent: i32 = 0;
            for k in 0..i64::from(samples_per_partition) {
                let a = abs_at(input[j], offset + k);
                if a > max {
                    max = a;
                }
                ent = ent.wrapping_add(a);
            }
            // int *= float：float で掛けて int に切り捨て（範囲の外は飽和。照合用の arm64 と同じ）
            ent = (ent as f32 * scale) as i32;

            let mut k = 0;
            while k < possible_partitions.saturating_sub(1) {
                let ku = k as usize;
                let (Some(&m1), Some(&m2)) = (info.classmetric1.get(ku), info.classmetric2.get(ku))
                else {
                    break;
                };
                if max <= m1 && (m2 < 0 || ent < m2) {
                    break;
                }
                k += 1;
            }

            pw[i] = i64::from(k);
        }
    }

    partword
}

/// `_2class`（res0.c:473–526）：stereo などの、partition の大きさがチャンネルの数の倍数のとき。
fn class_2(look: &VorbisLookResidue0, input: &[&[i32]], ch: usize) -> Vec<Vec<i64>> {
    let info = &look.info;
    let ch = ch.min(input.len());
    let Some((_begin, _n, samples_per_partition, partvals)) = partition_setup(info) else {
        return vec![Vec::new()];
    };
    if ch == 0 {
        return vec![vec![0; partvals]];
    }
    let possible_partitions = info.partitions;
    let mut partword = vec![vec![0i64; partvals]];

    let mut l: i64 = info.begin / ch as i64;
    for pw in partword[0].iter_mut() {
        let mut magmax: i32 = 0;
        let mut angmax: i32 = 0;
        let mut j = 0;
        while j < samples_per_partition {
            let a = abs_at(input[0], l);
            if a > magmax {
                magmax = a;
            }
            for v in &input[1..ch] {
                let a = abs_at(v, l);
                if a > angmax {
                    angmax = a;
                }
            }
            l += 1;
            j += ch as i32;
        }

        let mut j = 0;
        while j < possible_partitions.saturating_sub(1) {
            let ju = j as usize;
            let (Some(&m1), Some(&m2)) = (info.classmetric1.get(ju), info.classmetric2.get(ju))
            else {
                break;
            };
            if magmax <= m1 && angmax <= m2 {
                break;
            }
            j += 1;
        }

        *pw = i64::from(j);
    }

    partword
}

/// `_01forward`（res0.c:528–640）。`input` は段ごとに書き換わる（`local_book_besterror` が選んだ値を引く。cascade）。
fn forward_01(
    opb: &mut OggpackBuffer,
    look: &VorbisLookResidue0,
    fullbooks: &[Codebook],
    input: &mut [&mut [i32]],
    ch: usize,
    partword: &[Vec<i64>],
) -> Result<(), EngineError> {
    let info = &look.info;
    let (begin, _n, samples_per_partition, partvals) =
        partition_setup(info).ok_or_else(|| bad_setup("grouping が正でない"))?;
    let possible_partitions = i64::from(info.partitions);
    let phrasebook = fullbooks
        .get(look.phrasebook)
        .ok_or_else(|| bad_setup("phrasebook がない"))?;
    let partitions_per_word = usize::try_from(phrasebook.dim)
        .ok()
        .filter(|&d| d >= 1)
        .ok_or_else(|| bad_setup("phrasebook の次元が正でない"))?;
    if input.len() < ch || partword.len() < ch || partword[..ch].iter().any(|p| p.len() < partvals)
    {
        return Err(EngineError::Internal(
            "residue：チャンネルか分類が足りない".into(),
        ));
    }
    let spp = samples_per_partition as usize;
    let begin = usize::try_from(begin).map_err(|_| bad_setup("begin が負"))?;
    let end = begin + partvals * spp;
    if input[..ch].iter().any(|v| v.len() < end) {
        return Err(EngineError::Internal("residue：end が配列の外".into()));
    }
    let class_of = |c: i64| {
        usize::try_from(c)
            .ok()
            .filter(|&c| c < look.partbooks.len())
            .ok_or_else(|| EngineError::Internal(format!("residue：分類 {c} が範囲の外")))
    };

    // we code the partition words for each channel, then the residual words for a partition per channel until
    // we've written all the residual words for that partition word.  Then write the next partition channel
    // words...
    for s in 0..usize::try_from(look.stages).unwrap_or(0) {
        let mut i = 0usize;
        while i < partvals {
            // first we encode a partition codeword for each channel
            if s == 0 {
                for pw in &partword[..ch] {
                    let mut val: i64 = pw[i];
                    for k in 1..partitions_per_word {
                        val = val.wrapping_mul(possible_partitions);
                        if i + k < partvals {
                            val = val.wrapping_add(pw[i + k]);
                        }
                    }

                    // training hack
                    if val < phrasebook.entries {
                        vorbis_book_encode(phrasebook, val as i32, opb)?;
                    }
                }
            }

            // now we encode interleaved residual values for the partitions
            let mut k = 0;
            while k < partitions_per_word && i < partvals {
                let offset = i * spp + begin;

                for j in 0..ch {
                    let class = class_of(partword[j][i])?;
                    if info.secondstages[class] & 1i32.checked_shl(s as u32).unwrap_or(0) != 0 {
                        let statebook = look.partbooks[class].get(s).copied().flatten();
                        if let Some(b) = statebook {
                            let book = fullbooks
                                .get(b)
                                .ok_or_else(|| bad_setup("residue の本がない"))?;
                            encodepart(
                                opb,
                                &mut input[j][offset..offset + spp],
                                samples_per_partition,
                                book,
                            )?;
                        }
                    }
                }
                k += 1;
                i += 1;
            }
        }
    }

    Ok(())
}

/// `res1_class`（res0.c:735–745）：nonzero のチャンネルの分類（なければ `None`）。
pub(crate) fn res1_class(
    look: &VorbisLookResidue0,
    input: &[&[i32]],
    nonzero: &[i32],
) -> Option<Vec<Vec<i64>>> {
    let used: Vec<&[i32]> = input
        .iter()
        .zip(nonzero)
        .filter(|(_, nz)| **nz != 0)
        .map(|(v, _)| *v)
        .collect();
    if used.is_empty() {
        return None;
    }
    Some(class_01(look, &used, used.len()))
}

/// `res1_forward`（res0.c:715–733）。`input`（チャンネルごとの量子化した残差）は書き換わる（C と同じ）。
pub(crate) fn res1_forward(
    opb: &mut OggpackBuffer,
    look: &VorbisLookResidue0,
    fullbooks: &[Codebook],
    input: &mut [&mut [i32]],
    nonzero: &[i32],
    partword: Option<&[Vec<i64>]>,
) -> Result<(), EngineError> {
    let ch = input.len();
    if nonzero.len() < ch {
        return Err(EngineError::Internal(
            "residue：nonzero がチャンネルの数より短い".into(),
        ));
    }
    let mut used = 0usize;
    for (i, &nz) in nonzero.iter().enumerate().take(ch) {
        if nz != 0 {
            input.swap(used, i);
            used += 1;
        }
    }

    if used == 0 {
        return Ok(());
    }
    let partword = partword.ok_or_else(|| EngineError::Internal("residue：分類がない".into()))?;
    forward_01(opb, look, fullbooks, &mut input[..used], used, partword)
}

/// `res2_class`（res0.c:759–768）。
pub(crate) fn res2_class(
    look: &VorbisLookResidue0,
    input: &[&[i32]],
    nonzero: &[i32],
) -> Option<Vec<Vec<i64>>> {
    let ch = input.len();
    if !nonzero.iter().take(ch).any(|&nz| nz != 0) {
        return None;
    }
    Some(class_2(look, input, ch))
}

/// `res2_forward`（res0.c:773–799）：`pcmend` はブロックの大きさ。
pub(crate) fn res2_forward(
    opb: &mut OggpackBuffer,
    look: &VorbisLookResidue0,
    fullbooks: &[Codebook],
    input: &[&[i32]],
    nonzero: &[i32],
    partword: Option<&[Vec<i64>]>,
    pcmend: i32,
) -> Result<(), EngineError> {
    let ch = input.len();
    let n = usize::try_from(pcmend / 2)
        .map_err(|_| EngineError::Internal("residue：pcmend が負".into()))?;
    if nonzero.len() < ch || input.iter().any(|v| v.len() < n) {
        return Err(EngineError::Internal(
            "residue：nonzero か入力が短い".into(),
        ));
    }

    // don't duplicate the code; use a working vector hack for now and reshape ourselves into a single
    // channel res1
    let mut work = vec![0i32; ch * n];
    let mut used = 0usize;
    for (i, pcm) in input.iter().enumerate() {
        if nonzero[i] != 0 {
            used += 1;
        }
        for (j, &v) in pcm[..n].iter().enumerate() {
            work[j * ch + i] = v;
        }
    }

    if used == 0 {
        return Ok(());
    }
    let partword = partword.ok_or_else(|| EngineError::Internal("residue：分類がない".into()))?;
    let mut one: [&mut [i32]; 1] = [&mut work[..]];
    forward_01(opb, look, fullbooks, &mut one, 1, partword)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vorbis::codebook::vorbis_book_init_encode;
    use vorbis_tables::{BookId, SETUP_LIST};

    /// テンプレートの residue（VBR の本）から、`vorbis_encode_residue_setup` と同じ形の設定と fullbooks を組む
    /// （booklist・groupbook は fullbooks の番号。重複は除かない）。
    fn template_residue(
        t: usize,
        map: usize,
        res: usize,
        end: i64,
    ) -> (VorbisInfoResidue0, Vec<Codebook>) {
        let r = &SETUP_LIST[t].maps[map].res[res];
        let mut info = *r.res;
        info.grouping = r.grouping;
        info.end = end;
        let mut ids: Vec<BookId> = vec![r.book_aux];
        info.groupbook = 0;
        let mut acc = 0;
        for i in 0..info.partitions as usize {
            for k in 0..4 {
                if let Some(b) = r.books_base.books[i][k] {
                    info.secondstages[i] |= 1 << k;
                    info.booklist[acc] = ids.len() as i32;
                    ids.push(b);
                    acc += 1;
                }
            }
        }
        let books = ids
            .into_iter()
            .map(|id| vorbis_book_init_encode(id).unwrap())
            .collect();
        (info, books)
    }

    /// すべてのテンプレートの residue で look が作れ、段・本・partvals が設定と合う。
    #[test]
    fn look_for_every_template_residue() {
        let mut n = 0;
        for (t, setup) in SETUP_LIST.iter().enumerate() {
            for (m, maps) in setup.maps.iter().enumerate() {
                for r in 0..maps.res.len() {
                    let (info, books) = template_residue(t, m, r, 256);
                    let look = res0_look(&info, &books).unwrap();
                    assert_eq!(look.parts, info.partitions);
                    assert_eq!(look.phrasebook, 0);
                    let dim = books[0].dim as u32;
                    assert_eq!(look.partvals, info.partitions.pow(dim));
                    let mut acc = 0;
                    for (j, pb) in look.partbooks.iter().enumerate() {
                        let ss = info.secondstages[j];
                        assert_eq!(pb.len(), ov_ilog(ss as u32) as usize);
                        assert!(pb.len() <= look.stages as usize);
                        for (k, b) in pb.iter().enumerate() {
                            if ss & (1 << k) != 0 {
                                assert_eq!(*b, Some(info.booklist[acc] as usize));
                                acc += 1;
                                // residue の VQ の本は整数の格子
                                assert_eq!(books[b.unwrap()].c.maptype, 1);
                            } else {
                                assert_eq!(*b, None);
                            }
                        }
                    }
                    n += 1;
                }
            }
        }
        assert!(n >= 36);
        // 壊れた設定は Err
        let (mut info, books) = template_residue(1, 1, 0, 256);
        info.groupbook = 99;
        assert!(res0_look(&info, &books).is_err());
        let (mut info, books) = template_residue(1, 1, 0, 256);
        info.booklist[0] = 99;
        assert!(res0_look(&info, &books).is_err());
    }

    /// 分類の既知の値：`_01class` は max と |値| の和（×100/grouping）で、`_2class` は mag・ang の最大で選ぶ。
    #[test]
    fn classification_known_values() {
        let mut info = VorbisInfoResidue0 {
            begin: 0,
            end: 8,
            grouping: 4,
            partitions: 3,
            ..VorbisInfoResidue0::default()
        };
        info.classmetric1[..3].copy_from_slice(&[0, 2, 0]);
        info.classmetric2[..3].copy_from_slice(&[-1, 300, 0]);
        let look = VorbisLookResidue0 {
            info,
            parts: 3,
            stages: 0,
            phrasebook: 0,
            partbooks: vec![Vec::new(); 3],
            partvals: 9,
        };
        // partition 0：全部 0 → class 0。partition 1：max 2・和 5 → ent 125 < 300 → class 1
        let a = [0, 0, 0, 0, 2, -1, 1, 1];
        // max 2・和 8 → ent 200 → class 1 / max 3 → class 2（最後）
        let b = [2, -2, 2, -2, 0, 3, 0, 0];
        let c = [0; 8];
        assert_eq!(
            res1_class(&look, &[&a, &b, &c], &[1, 1, 0]).unwrap(),
            vec![vec![0, 1], vec![1, 2]]
        );
        assert_eq!(res1_class(&look, &[&a, &b], &[0, 0]), None);
        // _2class：l は begin/ch から、partition ごとに grouping/ch 個
        let mut info2 = info;
        info2.classmetric1[..3].copy_from_slice(&[0, 1, 0]);
        info2.classmetric2[..3].copy_from_slice(&[0, 0, 0]);
        let look2 = VorbisLookResidue0 {
            info: info2,
            ..look.clone()
        };
        let mag = [0, 0, 1, -1, 5, 0, 0, 0];
        let ang = [0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            res2_class(&look2, &[&mag, &ang], &[1, 0]).unwrap(),
            vec![vec![0, 1]]
        );
        assert_eq!(res2_class(&look2, &[&mag, &ang], &[0, 0]), None);
        let ang = [0, 1, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            res2_class(&look2, &[&mag, &ang], &[1, 1]).unwrap(),
            vec![vec![2, 1]]
        );
        // 範囲の外は 0 として読む（panic しない）
        assert_eq!(
            res1_class(&look, &[&a[..3]], &[1]).unwrap(),
            vec![vec![0, 0]]
        );
    }

    /// 符号化の性質：決定的で、cascade が残差を書き換え、全部 0 のチャンネルだけなら何も書かない。壊れた入力は Err。
    #[test]
    fn forward_is_deterministic_and_checks_inputs() {
        use rand_chacha::rand_core::Rng;
        let mut rng = crate::determinism::rng(11);
        // 44.1 kHz の stereo（residue 2）と uncoupled（residue 1）の長い窓
        for (t, rtype) in [(1usize, 2), (3usize, 1)] {
            let (info, books) = template_residue(t, 1, 1, 512);
            let look = res0_look(&info, &books).unwrap();
            let n = 1024usize; // pcmend/2 = 512 × ch（res2）または 512（res1）
            let ch = 2usize;
            let chans: Vec<Vec<i32>> = (0..ch)
                .map(|c| {
                    (0..n / 2)
                        .map(|i| {
                            let r = (rng.next_u32() % 9) as i32 - 4;
                            if i > 300 { 0 } else { r * (c as i32 + 1) }
                        })
                        .collect()
                })
                .collect();
            let encode = |nonzero: &[i32]| {
                let mut chans = chans.clone();
                let mut opb = OggpackBuffer::oggpack_writeinit();
                {
                    let refs: Vec<&[i32]> = chans.iter().map(|v| v.as_slice()).collect();
                    if rtype == 2 {
                        let pw = res2_class(&look, &refs, nonzero);
                        res2_forward(
                            &mut opb,
                            &look,
                            &books,
                            &refs,
                            nonzero,
                            pw.as_deref(),
                            n as i32,
                        )
                        .unwrap();
                    } else {
                        let pw = res1_class(&look, &refs, nonzero);
                        let mut muts: Vec<&mut [i32]> =
                            chans.iter_mut().map(|v| v.as_mut_slice()).collect();
                        res1_forward(&mut opb, &look, &books, &mut muts, nonzero, pw.as_deref())
                            .unwrap();
                    }
                }
                (opb, chans)
            };
            let (a, ca) = encode(&[1, 1]);
            let (b, cb) = encode(&[1, 1]);
            assert_eq!(a, b);
            assert_eq!(ca, cb);
            assert!(a.oggpack_bits() > 100);
            if rtype == 1 {
                // residue 1 は渡した配列を書き換える（cascade の残り）。各段は 0 の項目より悪くならないので、2 乗和は減る
                assert_ne!(ca, chans);
                let energy = |v: &[Vec<i32>]| {
                    v.iter()
                        .flatten()
                        .map(|&x| i64::from(x) * i64::from(x))
                        .sum::<i64>()
                };
                assert!(energy(&ca) < energy(&chans));
            }
            let (z, _) = encode(&[0, 0]);
            assert_eq!(z.oggpack_bits(), 0);
            // 分類がないのに nonzero があるのは Err
            let refs: Vec<&[i32]> = chans.iter().map(|v| v.as_slice()).collect();
            let mut opb = OggpackBuffer::oggpack_writeinit();
            assert!(res2_forward(&mut opb, &look, &books, &refs, &[1, 1], None, n as i32).is_err());
            // 短い入力は Err（panic しない）
            let short: Vec<&[i32]> = chans.iter().map(|v| &v[..100]).collect();
            assert!(
                res2_forward(&mut opb, &look, &books, &short, &[1, 1], None, n as i32).is_err()
            );
            let pw = vec![vec![0i64; 1]; 2];
            let mut c2 = chans.clone();
            let mut muts: Vec<&mut [i32]> = c2.iter_mut().map(|v| v.as_mut_slice()).collect();
            assert!(res1_forward(&mut opb, &look, &books, &mut muts, &[1, 1], Some(&pw)).is_err());
        }
    }
}

/// 照合（照合用の C がある手元だけ。`KURA_VORBIS_ORACLE`）。
#[cfg(test)]
mod oracle_tests {
    use super::*;
    use crate::codec::vorbis::codebook::testutil::{
        cases, floor_info, fullbooks, int, ints, residue_info,
    };
    use crate::codec::vorbis::floor1::{VorbisLookFloor1, floor1_encode, floor1_look};
    use crate::codec::vorbis::oracle;

    /// (c) 分類が RCLS と、(d) 音声パケット全体（頭 → チャンネルごとの floor → submap ごとの residue。mapping0.c:1204–1313
    /// の書く順）が照合用のパケットとバイト単位で一致する。入力はダンプ（レベル 1）の FFIT・RQPO・NZC と BLCK。
    #[test]
    fn classification_and_packets_match_oracle() {
        if oracle::root().is_none() {
            return;
        }
        let mut packets_checked = 0usize;
        let mut classified = [0usize; 3];
        let mut windows = [0usize; 2];
        let mut bytes = 0usize;
        for case in cases() {
            let ctx = format!("{case:?}");
            let input = oracle::signal(case.signal, case.rate, case.ch, case.frames, 3);
            let (packets, dump) = oracle::encode(&input, case.rate, case.q, Some(1)).unwrap();
            let dump = dump.unwrap();
            assert_eq!(dump.blocks.len() + 3, packets.len(), "{ctx}");
            let setup = oracle::setup(case.rate, case.ch, case.q, false).unwrap();
            let books = fullbooks(&setup);
            let flooks: Vec<VorbisLookFloor1> = setup["floors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| floor1_look(&floor_info(f)).unwrap())
                .collect();
            let rlooks: Vec<(i32, VorbisLookResidue0)> = setup["residues"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    let (t, info) = residue_info(r);
                    (t, res0_look(&info, &books).unwrap())
                })
                .collect();
            let modes = setup["modes"].as_array().unwrap();
            let maps = setup["maps"].as_array().unwrap();
            let modebits = ov_ilog((modes.len() - 1) as u32) as u32;

            for b in 0..dump.blocks.len() {
                let at = format!("{ctx} ブロック {b}");
                let info = dump.block_info(b);
                let (w, lw, nw, n) = (info[1], info[2], info[3], info[8] as usize);
                windows[w as usize] += 1;
                let map = &maps[int(&modes[w as usize]["mapping"]) as usize];
                let chmux = ints(&map["chmuxlist"]);
                let floorsub = ints(&map["floorsubmap"]);
                let ressub = ints(&map["residuesubmap"]);
                let submaps = int(&map["submaps"]) as usize;

                let mut opb = OggpackBuffer::oggpack_writeinit();
                // パケットの種類（音声 0）・mode・長い窓なら前後の窓
                opb.oggpack_write(0, 1).unwrap();
                opb.oggpack_write(w as u64, modebits).unwrap();
                if w != 0 {
                    opb.oggpack_write(lw as u64, 1).unwrap();
                    opb.oggpack_write(nw as u64, 1).unwrap();
                }
                // floor
                for c in 0..case.ch {
                    let look = &flooks[floorsub[chmux[c] as usize] as usize];
                    let mut post = dump.get(b, "FFIT", c as u16).unwrap().i32s();
                    let mut ilog = vec![0; n / 2];
                    let p = if post.is_empty() {
                        None
                    } else {
                        Some(&mut post[..])
                    };
                    let nz = floor1_encode(&mut opb, &books, look, p, &mut ilog, n as i32).unwrap();
                    assert_eq!(nz, dump.get(b, "NZ  ", c as u16).unwrap().i32s()[0], "{at}");
                }
                // coupling のあとの整数（psy.rs の担当）はダンプから
                let mut iwork: Vec<Vec<i32>> = (0..case.ch)
                    .map(|c| dump.get(b, "RQPO", c as u16).unwrap().i32s())
                    .collect();
                let nonzero: Vec<i32> = (0..case.ch)
                    .map(|c| dump.get(b, "NZC ", c as u16).unwrap().i32s()[0])
                    .collect();
                // submap ごとに分類と符号化
                for s in 0..submaps {
                    let (rtype, look) = &rlooks[ressub[s] as usize];
                    let members: Vec<usize> =
                        (0..case.ch).filter(|&c| chmux[c] as usize == s).collect();
                    let zerobundle: Vec<i32> = members
                        .iter()
                        .map(|&c| i32::from(nonzero[c] != 0))
                        .collect();
                    let classes = {
                        let bundle: Vec<&[i32]> =
                            members.iter().map(|&c| iwork[c].as_slice()).collect();
                        if *rtype == 2 {
                            res2_class(look, &bundle, &zerobundle)
                        } else {
                            res1_class(look, &bundle, &zerobundle)
                        }
                    };
                    // (c) RCLS
                    let tag = |j: usize| ((s << 8) | j) as u16;
                    match &classes {
                        None => {
                            assert!(dump.get(b, "RCLS", tag(0)).is_none(), "{at}：RCLS");
                            classified[0] += 1;
                        }
                        Some(pw) => {
                            for (j, p) in pw.iter().enumerate() {
                                let want: Vec<i64> = dump
                                    .get(b, "RCLS", tag(j))
                                    .unwrap_or_else(|| panic!("{at}：RCLS {j} がない"))
                                    .i32s()
                                    .into_iter()
                                    .map(i64::from)
                                    .collect();
                                assert_eq!(p, &want, "{at}：RCLS submap {s} の {j}");
                            }
                            assert!(dump.get(b, "RCLS", tag(pw.len())).is_none(), "{at}");
                            classified[*rtype as usize] += 1;
                        }
                    }
                    if *rtype == 2 {
                        let bundle: Vec<&[i32]> =
                            members.iter().map(|&c| iwork[c].as_slice()).collect();
                        res2_forward(
                            &mut opb,
                            look,
                            &books,
                            &bundle,
                            &zerobundle,
                            classes.as_deref(),
                            n as i32,
                        )
                        .unwrap();
                    } else {
                        let mut bundle: Vec<&mut [i32]> = iwork
                            .iter_mut()
                            .enumerate()
                            .filter(|(c, _)| chmux[*c] as usize == s)
                            .map(|(_, v)| v.as_mut_slice())
                            .collect();
                        res1_forward(
                            &mut opb,
                            look,
                            &books,
                            &mut bundle,
                            &zerobundle,
                            classes.as_deref(),
                        )
                        .unwrap();
                    }
                }
                // (d) パケット
                let want = &packets[3 + b].body;
                let got = opb.oggpack_get_buffer();
                if got != want.as_slice() {
                    let bit = got
                        .iter()
                        .zip(want)
                        .position(|(x, y)| x != y)
                        .map(|k| k * 8 + (got[k] ^ want[k]).trailing_zeros() as usize);
                    panic!(
                        "{at}：パケットが違う（{} と {} バイト、最初に違うビット {bit:?}）",
                        got.len(),
                        want.len()
                    );
                }
                let bend = dump.get(b, "BEND", 0xFFFF).unwrap().i64s();
                assert_eq!(
                    (opb.oggpack_bytes() as i64, opb.oggpack_bits() as i64),
                    (bend[0], bend[1]),
                    "{at}"
                );
                packets_checked += 1;
                bytes += got.len();
            }
        }
        eprintln!(
            "residue・パケット：{packets_checked} パケット（{bytes} バイト、短 {} 長 {}）、分類 res1 {}・res2 {}・なし {} が一致",
            windows[0], windows[1], classified[1], classified[2], classified[0]
        );
        assert!(windows[0] > 0 && windows[1] > 0);
        assert!(classified.iter().all(|&c| c > 0), "{classified:?}");
    }
}
