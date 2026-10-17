//! codebook（encode の側）：符号語の割り当て・書き出し・VQ の最良の項目。
//!
//! # 移す C
//!
//! | 関数 | ファイル:行 | 備考 |
//! |---|---|---|
//! | `ov_ilog` | sharedbook.c:35–39 | `vorbis_tables::ov_ilog` と同じ |
//! | `_float32_unpack` | sharedbook.c:66–80 | `mant = val & 0x1fffff`（double）、符号、`exp = (val & 0x7fe00000) >> 21`、`exp - 20 - 768` を ±63 に止め、`ldexp(mant, exp)`（`crate::math::ldexp`。正確）。戻りは float |
//! | `_make_words` | sharedbook.c:85–169 | 長さ → 符号語（marker の方法。項目の順に「まだ使われていない最も小さい符号語」）。あふれる木は NULL（→ `Err`）、足りない木も NULL（項目 1 つで長さ 1 の本だけ例外）。最後に符号語のビットを反転（LSb から書くため）。encode では `sparsecount = 0`（長さ 0 の項目にも場所を取る） |
//! | `_book_maptype1_quantvals` | sharedbook.c:174–209 | `vorbis_tables::book_maptype1_quantvals`（C は pow で見当を付けて整数で確かめ直すので、整数だけで同じ値）。**maptype 0 の本にも呼ぶ**（init_encode が全冊に呼ぶ） |
//! | `vorbis_book_init_encode` | sharedbook.c:303–317 | `codelist = _make_words(lengthlist, entries, 0)`、`quantvals`、`minval = (int)rint(_float32_unpack(q_min))`、`delta = (int)rint(_float32_unpack(q_delta))` |
//! | `vorbis_book_encode` | codebook.c:402–410 | `a` が範囲の外なら何も書かずに 0。`codelist[a]` を `lengthlist[a]` ビット書き、長さを返す（codebook.c の `vorbis_oggpack_write` は libogg と同じビット列。bitpack.rs） |
//! | `local_book_besterror` | res0.c:316–378 | 整数の中心の格子（maptype 1、次元 ≤ 8）で、`a` を格子に丸めて項目の番号を作る（`del != 1` と `== 1` の 2 つの枝）。その項目が使われていなければ（長さ 0）全探索（「vq/ の道具が作った値の並び 0, −1, +1, −2, +2 …」を前提にした `e[]` の進め方）。見つかれば `a -= p`（cascade の次の段は残りを符号化） |
//!
//! # 移さないもの
//!
//! decode の側（`vorbis_book_init_decode`・`_book_unquantize`・`decode_*`・`vorbis_staticbook_unpack`。codebook の読み戻しは
//! vorbis-tables の `unpack`）、`_float32_pack`（books.bin はすでに詰めてある）、`vorbis_book_besterror`・`_best`。
//!
//! # 型の対応
//!
//! - `static_codebook`（codebook.h:35–55）→ `vorbis_tables::StaticCodebook`（books.bin から `vorbis_tables::unpack` で作る）。
//!   setup ヘッダの `vorbis_staticbook_pack` は info.rs（books.bin の詰めた形をビットのまま写す）。
//! - `codebook`（codebook.h:57–79。encode に要る欄だけ）→ [`Codebook`]。`c`（static へのポインタ）は持ち物にする。
//!
//! # 移植の注記
//!
//! - C の整数のあふれ（未定義）は 2 の補数で折り返す形（`wrapping_*`）に写した（照合用の arm64 の動き）。
//! - `local_book_besterror` の全探索の `e[]` の進め方は、最後の項目のあとで `e[dim]` を読み書きする（`while(e[j]>=maxval)
//!   e[j++]=0;`）。次元 7 以下では配列の中（使われない場所）なので結果に効かない。**次元 8 では C は `e[8]`（配列の外）を
//!   読み書きする**（未定義の振る舞い）。移植は `e` を 9 個にして同じ進め方をする（`e[8]` は 0 から）。次元 8 の本
//!   （`_*c*_s_p1_0`。81/6561 項目）は class の条件（mag が ±1 以内・ang が 0）で使われる項目にしか当たらないので、
//!   全探索に入らない（照合でも入らなかった）。
//! - C は `_make_words` の NULL（木が壊れている）を `vorbis_book_init_encode` で調べないが、移植は `Err` にする（books.bin の
//!   全冊で木が正しいことをテストで確かめる）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use vorbis_tables::{BookId, StaticCodebook};

use super::bitpack::OggpackBuffer;
use crate::error::EngineError;

/// `codebook`（codebook.h:57–79）の encode に要る欄。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Codebook {
    /// どの本か（books.bin の番号）。
    pub id: BookId,
    pub dim: i64,
    pub entries: i64,
    pub used_entries: i64,
    /// C の `const static_codebook *c`。
    pub c: StaticCodebook,
    /// 項目ごとの符号語（ビットを反転したもの。長さ 0 の項目は 0）。
    pub codelist: Vec<u32>,
    pub quantvals: i32,
    pub minval: i32,
    pub delta: i32,
}

/// sharedbook.c:46–48。
const VQ_FMAN: i64 = 21;
/// sharedbook.c:48。
const VQ_FEXP_BIAS: i64 = 768;

/// `_float32_unpack`（sharedbook.c:66–80）。
pub(crate) fn float32_unpack(val: i64) -> f32 {
    let mut mant = (val & 0x1f_ffff) as f64;
    // C の `int sign = val & 0x80000000;`（0 か 0 でないかだけを見る）
    let sign = val & 0x8000_0000 != 0;
    let mut exp = (val & 0x7fe0_0000) >> VQ_FMAN;
    if sign {
        mant = -mant;
    }
    exp = exp - (VQ_FMAN - 1) - VQ_FEXP_BIAS;
    // clamp excessive exponent values（C の `if (exp>63) exp=63; if (exp<-63) exp=-63;`）
    exp = exp.clamp(-63, 63);
    // ldexp は double を返し、戻りの float に 1 回だけ丸める
    crate::math::ldexp(mant, exp as i32) as f32
}

/// `_make_words`（sharedbook.c:85–169）：長さの表から符号語（ビット反転済み）。木があふれる・足りないときは `Err`。
pub(crate) fn make_words(l: &[u8], n: i64, sparsecount: i64) -> Result<Vec<u32>, EngineError> {
    let bad = |why: &str| EngineError::Internal(format!("codebook の長さの表が正しくない：{why}"));
    let n = usize::try_from(n).map_err(|_| bad("項目の数が負"))?;
    let l = l
        .get(..n)
        .ok_or_else(|| bad("長さの表が項目の数より短い"))?;
    let size = if sparsecount != 0 {
        usize::try_from(sparsecount).map_err(|_| bad("sparsecount が負"))?
    } else {
        n
    };
    let mut r = vec![0u32; size];
    let mut marker = [0u32; 33];
    let mut count = 0usize;

    for &len in l {
        let length = usize::from(len);
        if length > 0 {
            if length > 32 {
                return Err(bad("33 ビット以上の符号語"));
            }
            let mut entry = marker[length];

            // when we claim a node for an entry, we also claim the nodes below it
            if length < 32 && (entry >> length) != 0 {
                // the lengths must specify an overpopulated tree
                return Err(bad("木があふれる"));
            }
            *r.get_mut(count)
                .ok_or_else(|| bad("sparsecount が足りない"))? = entry;
            count += 1;

            // Look to see if the next shorter marker points to the node above. if so, update it and repeat.
            for j in (1..=length).rev() {
                if marker[j] & 1 != 0 {
                    // have to jump branches
                    if j == 1 {
                        marker[1] = marker[1].wrapping_add(1);
                    } else {
                        marker[j] = marker[j - 1] << 1;
                    }
                    break;
                }
                marker[j] = marker[j].wrapping_add(1);
            }

            // prune the tree; the implicit invariant says all the longer markers were dangling from our
            // just-taken node.  Dangle them from our *new* node.
            for j in length + 1..33 {
                if (marker[j] >> 1) == entry {
                    entry = marker[j];
                    marker[j] = marker[j - 1] << 1;
                } else {
                    break;
                }
            }
        } else if sparsecount == 0 {
            count += 1;
        }
    }

    // any underpopulated tree must be rejected (single-entry codebooks are a retconned extension).
    if !(count == 1 && marker[2] == 2) {
        for (i, &m) in marker.iter().enumerate().skip(1) {
            let mask = (0xffff_ffff_u64 >> (32 - i)) as u32;
            if m & mask != 0 {
                return Err(bad("木が足りない"));
            }
        }
    }

    // bitreverse the words because our bitwise packer/unpacker is LSb endian
    count = 0;
    for &len in l {
        let word = r.get(count).copied().unwrap_or(0);
        let mut temp = 0u32;
        for j in 0..u32::from(len) {
            temp <<= 1;
            temp |= (word >> j) & 1;
        }
        if sparsecount != 0 {
            if len != 0 {
                if let Some(slot) = r.get_mut(count) {
                    *slot = temp;
                }
                count += 1;
            }
        } else {
            if let Some(slot) = r.get_mut(count) {
                *slot = temp;
            }
            count += 1;
        }
    }
    Ok(r)
}

/// `vorbis_book_init_encode`（sharedbook.c:303–317）。
pub(crate) fn vorbis_book_init_encode(id: BookId) -> Result<Codebook, EngineError> {
    let c = vorbis_tables::unpack(id)
        .map_err(|e| EngineError::Internal(format!("codebook {}：{e}", id.0)))?;
    let codelist = make_words(&c.lengthlist, c.entries, 0)?;
    let quantvals = vorbis_tables::book_maptype1_quantvals(c.entries, c.dim) as i32;
    // C の `(int)rint(float)`：rint は double の関数（整数に丸めるので値は同じ）
    let minval = f64::from(float32_unpack(c.q_min)).round_ties_even() as i32;
    let delta = f64::from(float32_unpack(c.q_delta)).round_ties_even() as i32;
    Ok(Codebook {
        id,
        dim: c.dim,
        entries: c.entries,
        used_entries: c.entries,
        c,
        codelist,
        quantvals,
        minval,
        delta,
    })
}

/// `vorbis_book_encode`（codebook.c:402–410）：項目 `a` の符号語を書き、そのビット数を返す。
pub(crate) fn vorbis_book_encode(
    book: &Codebook,
    a: i32,
    b: &mut OggpackBuffer,
) -> Result<i32, EngineError> {
    if a < 0 || i64::from(a) >= book.c.entries {
        return Ok(0);
    }
    let i = a as usize;
    let (Some(&len), Some(&word)) = (book.c.lengthlist.get(i), book.codelist.get(i)) else {
        return Err(EngineError::Internal(format!(
            "codebook {}：項目 {a} の符号語がない",
            book.id.0
        )));
    };
    b.oggpack_write(u64::from(word), u32::from(len))?;
    Ok(i32::from(len))
}

/// `local_book_besterror`（res0.c:316–378）：`a`（dim 個の整数）に最も近い項目の番号。`a` から選んだ格子の値を引く。
///
/// C と同じく、整数の中心の格子（maptype 1、次元 1〜8）を前提にする。前提を満たさない本（次元が 0 か 9 以上・`a` が
/// 次元より短い・`delta` が 0 で C が 0 で割る）では何もせずに −1 を返す（呼ぶ側の `encodepart` が先に断る）。
pub(crate) fn local_book_besterror(book: &Codebook, a: &mut [i32]) -> i32 {
    let Ok(dim) = usize::try_from(book.dim) else {
        return -1;
    };
    if dim == 0 || dim > 8 || a.len() < dim || book.delta == 0 {
        return -1;
    }
    let minval = book.minval;
    let del = book.delta;
    let qv = book.quantvals;
    let ze = qv >> 1;
    let mut index: i32 = 0;
    // assumes integer/centered encoder codebook maptype 1 no more than dim 8
    let mut p = [0i32; 8];

    let clamp = |m: i32| {
        if m < 0 {
            0
        } else if m >= qv {
            qv.wrapping_sub(1)
        } else {
            m
        }
    };
    let fold = |v: i32| {
        if v < ze {
            (ze.wrapping_sub(v) << 1).wrapping_sub(1)
        } else {
            v.wrapping_sub(ze) << 1
        }
    };
    if del != 1 {
        for o in (0..dim).rev() {
            let v = a[o]
                .wrapping_sub(minval)
                .wrapping_add(del >> 1)
                .wrapping_div(del);
            let m = fold(v);
            index = index.wrapping_mul(qv).wrapping_add(clamp(m));
            p[o] = v.wrapping_mul(del).wrapping_add(minval);
        }
    } else {
        for o in (0..dim).rev() {
            let v = a[o].wrapping_sub(minval);
            let m = fold(v);
            index = index.wrapping_mul(qv).wrapping_add(clamp(m));
            p[o] = v.wrapping_mul(del).wrapping_add(minval);
        }
    }

    let used = usize::try_from(index)
        .ok()
        .and_then(|i| book.c.lengthlist.get(i))
        .is_some_and(|&l| l > 0);
    if !used {
        let c = &book.c;
        let mut best: i32 = -1;
        // assumes integer/centered encoder codebook maptype 1 no more than dim 8（C の `e[8]` の外の 1 個を足す。冒頭の注記）
        let mut e = [0i32; 9];
        let maxval = book
            .minval
            .wrapping_add(book.delta.wrapping_mul(book.quantvals.wrapping_sub(1)));
        for (i, &len) in c.lengthlist.iter().enumerate() {
            if len > 0 {
                let mut this: i32 = 0;
                for j in 0..dim {
                    let val = e[j].wrapping_sub(a[j]);
                    this = this.wrapping_add(val.wrapping_mul(val));
                }
                if best == -1 || this < best {
                    p.copy_from_slice(&e[..8]);
                    best = this;
                    index = i as i32;
                }
            }
            // assumes the value patterning created by the tools in vq/
            let mut j = 0;
            while j < e.len() && e[j] >= maxval {
                e[j] = 0;
                j += 1;
            }
            if let Some(ej) = e.get_mut(j) {
                if *ej >= 0 {
                    *ej = ej.wrapping_add(book.delta);
                }
                *ej = ej.wrapping_neg();
            }
        }
    }

    if index > -1 {
        for (ai, pi) in a.iter_mut().zip(p.iter()).take(dim) {
            *ai = ai.wrapping_sub(*pi);
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use vorbis_tables::BOOKS;

    fn all_books() -> Vec<Codebook> {
        (0..BOOKS.len())
            .map(|i| vorbis_book_init_encode(BookId(i as u16)).unwrap())
            .collect()
    }

    /// 符号語の割り当ての既知の値（長さ → 最も小さい符号語から、ビットを反転して LSb から書ける形）。
    #[test]
    fn make_words_known_values() {
        // 長さ 2 が 4 つ：00・01・10・11 → 反転して 0・2・1・3
        assert_eq!(make_words(&[2, 2, 2, 2], 4, 0).unwrap(), vec![0, 2, 1, 3]);
        // 1・2・3・3：0・10・110・111 → 反転 0・01・011・111
        assert_eq!(
            make_words(&[1, 2, 3, 3], 4, 0).unwrap(),
            vec![0, 0b01, 0b011, 0b111]
        );
        // 長さ 0 の項目にも場所を取る（sparsecount = 0）
        assert_eq!(make_words(&[1, 0, 1], 3, 0).unwrap(), vec![0, 0, 1]);
        // sparsecount があれば詰める
        assert_eq!(make_words(&[1, 0, 1], 3, 2).unwrap(), vec![0, 1]);
        // あふれる木・足りない木
        assert!(make_words(&[1, 1, 1], 3, 0).is_err());
        assert!(make_words(&[2, 2, 2], 3, 0).is_err());
        // 項目 1 つで長さ 1 だけは例外
        assert_eq!(make_words(&[1], 1, 0).unwrap(), vec![0]);
        assert!(make_words(&[2], 1, 0).is_err());
        // 33 ビット以上・短い表
        assert!(make_words(&[33, 1], 2, 0).is_err());
        assert!(make_words(&[1], 2, 0).is_err());
    }

    /// `_float32_unpack` の既知の値。
    #[test]
    fn float32_unpack_known_values() {
        let one = (VQ_FEXP_BIAS + 20) << 21 | 1;
        assert_eq!(float32_unpack(one), 1.0);
        assert_eq!(float32_unpack(one | 0x8000_0000), -1.0);
        assert_eq!(float32_unpack((VQ_FEXP_BIAS + 20) << 21 | 3), 3.0);
        assert_eq!(float32_unpack((VQ_FEXP_BIAS + 21) << 21 | 3), 6.0);
        assert_eq!(float32_unpack((VQ_FEXP_BIAS + 19) << 21 | 1), 0.5);
        assert_eq!(float32_unpack(0), 0.0);
        // 指数は ±63 に止める
        assert_eq!(
            float32_unpack(0x7fe0_0000 | 1),
            crate::math::ldexp(1.0, 63) as f32
        );
        assert_eq!(float32_unpack(1), crate::math::ldexp(1.0, -63) as f32);
        // C の表の負の long（下位 32 ビットだけが意味を持つ）と同じ値
        assert_eq!(
            float32_unpack(-533_725_184),
            float32_unpack(-533_725_184_i64 & 0xffff_ffff)
        );
    }

    /// books.bin の全冊：木が正しく（符号語が作れ）、符号語は接頭語にならず、書いたビット列を読み戻せる。
    #[test]
    fn every_book_has_prefix_free_codewords_and_roundtrips() {
        for book in all_books() {
            let name = BOOKS[usize::from(book.id.0)].name;
            assert_eq!(book.codelist.len() as i64, book.entries, "{name}");
            assert_eq!(book.used_entries, book.entries, "{name}");
            // 全部の項目を書き、LSb から 1 ビットずつ読んで木をたどって戻す
            let mut opb = OggpackBuffer::oggpack_writeinit();
            let mut total = 0usize;
            for a in 0..book.entries as i32 {
                let bits = vorbis_book_encode(&book, a, &mut opb).unwrap();
                assert_eq!(bits, i32::from(book.c.lengthlist[a as usize]), "{name}");
                total += bits as usize;
            }
            assert_eq!(opb.oggpack_bits(), total, "{name}");
            // 範囲の外は書かずに 0
            assert_eq!(vorbis_book_encode(&book, -1, &mut opb).unwrap(), 0);
            assert_eq!(
                vorbis_book_encode(&book, book.entries as i32, &mut opb).unwrap(),
                0
            );
            assert_eq!(opb.oggpack_bits(), total, "{name}");
            let bytes = opb.oggpack_get_buffer().to_vec();
            let bit = |k: usize| (bytes[k / 8] >> (k % 8)) & 1;
            // (長さ, 先頭から読んだ順の符号語) → 項目
            let mut table = std::collections::BTreeMap::new();
            for (a, &len) in book.c.lengthlist.iter().enumerate() {
                if len > 0 {
                    let w = book.codelist[a];
                    let msb_first: u32 =
                        (0..u32::from(len)).fold(0, |acc, j| (acc << 1) | ((w >> j) & 1));
                    assert!(table.insert((len, msb_first), a).is_none(), "{name}");
                }
            }
            let mut at = 0usize;
            for (a, &len) in book.c.lengthlist.iter().enumerate() {
                if len == 0 {
                    continue;
                }
                let mut acc = 0u32;
                let mut got = None;
                for l in 1..=32u8 {
                    acc = (acc << 1) | u32::from(bit(at));
                    at += 1;
                    if let Some(&x) = table.get(&(l, acc)) {
                        got = Some(x);
                        break;
                    }
                }
                assert_eq!(got, Some(a), "{name}");
            }
            assert_eq!(at, total, "{name}");
        }
    }

    /// VQ の本（maptype 1）は整数の中心の格子で、`local_book_besterror` の全探索の `e[]` の進め方（0, −δ, +δ, −2δ …）が
    /// 項目の値（`_book_unquantize` の並び）と一致する（C の前提を、すべての本で確かめる）。
    #[test]
    fn vq_books_are_centered_integer_lattices() {
        let mut n = 0;
        for book in all_books() {
            if book.c.maptype != 1 {
                continue;
            }
            n += 1;
            let name = BOOKS[usize::from(book.id.0)].name;
            let dim = book.dim as usize;
            assert!((1..=8).contains(&dim), "{name}");
            assert_eq!(book.c.q_sequencep, 0, "{name}");
            let mindel = float32_unpack(book.c.q_min);
            let delta = float32_unpack(book.c.q_delta);
            assert_eq!(mindel, book.minval as f32, "{name}");
            assert_eq!(delta, book.delta as f32, "{name}");
            let qv = book.quantvals as i64;
            assert_eq!(
                book.minval,
                -(book.delta * (book.quantvals - 1) / 2),
                "{name}"
            );
            let maxval = book.minval + book.delta * (book.quantvals - 1);
            let mut e = [0i32; 9];
            for j in 0..book.entries {
                // _book_unquantize（maptype 1）の値
                let mut indexdiv = 1i64;
                for &ek in e.iter().take(dim) {
                    let index = ((j / indexdiv) % qv) as usize;
                    let val = (book.c.quantlist[index] as f32).abs() * delta + mindel;
                    assert_eq!(val, ek as f32, "{name} 項目 {j}");
                    indexdiv *= qv;
                }
                let mut k = 0;
                while k < 9 && e[k] >= maxval {
                    e[k] = 0;
                    k += 1;
                }
                if e[k] >= 0 {
                    e[k] += book.delta;
                }
                e[k] = -e[k];
            }
        }
        assert!(n > 300, "VQ の本が {n} 冊");
    }

    /// `local_book_besterror`：本の範囲の中の入力で、選んだ項目は使われる項目、`a` からその項目の値を引き、2 乗誤差は
    /// 使われる項目の中で最小（格子の丸めで使われる項目に当たればその点、当たらなければ全探索で最初の最小）。
    #[test]
    fn besterror_picks_used_entries_and_subtracts() {
        use rand_chacha::rand_core::Rng;
        let mut rng = crate::determinism::rng(7);
        let books: Vec<Codebook> = all_books()
            .into_iter()
            .filter(|b| b.c.maptype == 1)
            .collect();
        let value_of = |b: &Codebook, entry: usize| -> Vec<i32> {
            let qv = b.quantvals as usize;
            let mut div = 1usize;
            (0..b.dim as usize)
                .map(|_| {
                    let idx = (entry / div) % qv;
                    div *= qv;
                    b.c.quantlist[idx] as i32 * b.delta + b.minval
                })
                .collect()
        };
        let check = |b: &Codebook, orig: &[i32]| {
            let name = BOOKS[usize::from(b.id.0)].name;
            let mut a = orig.to_vec();
            let idx = local_book_besterror(b, &mut a);
            assert!(idx >= 0, "{name}");
            let idx = idx as usize;
            assert!(b.c.lengthlist[idx] > 0, "{name}");
            let v = value_of(b, idx);
            for k in 0..orig.len() {
                assert_eq!(a[k], orig[k] - v[k], "{name}");
            }
            let err = |v: &[i32]| -> i64 {
                v.iter()
                    .zip(orig)
                    .map(|(x, y)| i64::from(x - y) * i64::from(x - y))
                    .sum()
            };
            let best = (0..b.entries as usize)
                .filter(|&e| b.c.lengthlist[e] > 0)
                .map(|e| err(&value_of(b, e)))
                .min()
                .unwrap();
            assert_eq!(err(&v), best, "{name}");
        };
        let mut sparse = 0;
        for b in &books {
            let dim = b.dim as usize;
            let maxval = b.minval + b.delta * (b.quantvals - 1);
            let span = (maxval - b.minval + 1) as u32;
            for _ in 0..20 {
                let orig: Vec<i32> = (0..dim)
                    .map(|_| (rng.next_u32() % span) as i32 + b.minval)
                    .collect();
                check(b, &orig);
            }
            // 格子の上の使われる点はその点に当たり、残りは 0
            for entry in (0..b.entries as usize)
                .filter(|&e| b.c.lengthlist[e] > 0)
                .take(50)
            {
                let mut a = value_of(b, entry);
                assert_eq!(local_book_besterror(b, &mut a), entry as i32);
                assert!(a.iter().all(|&x| x == 0));
            }
            // 使われない項目の値は丸めでその項目に当たるので、全探索に入る
            if let Some(unused) = (0..b.entries as usize).find(|&e| b.c.lengthlist[e] == 0) {
                sparse += 1;
                check(b, &value_of(b, unused));
            }
        }
        assert!(sparse > 20, "使われない項目のある VQ の本が {sparse} 冊");
        // 前提を満たさない本・短い a は何もしない
        let mut b = books[0].clone();
        let mut a = vec![1; 9];
        b.dim = 9;
        assert_eq!(local_book_besterror(&b, &mut a), -1);
        assert_eq!(a, vec![1; 9]);
        b.dim = 2;
        b.delta = 0;
        assert_eq!(local_book_besterror(&b, &mut a), -1);
        let b = books[0].clone();
        let mut short = vec![0; b.dim as usize - 1];
        assert_eq!(local_book_besterror(&b, &mut short), -1);
    }
}

/// 照合のテストの部品（oracle_setup の JSON から floor・residue・codebook の設定を組む。setup.rs がまだないため）。
#[cfg(test)]
pub(crate) mod testutil {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use serde_json::Value;
    use vorbis_tables::{BOOKS, BookId, VorbisInfoFloor1, VorbisInfoResidue0};

    use super::{Codebook, vorbis_book_init_encode};
    use crate::codec::vorbis::oracle::{self, Signal};

    /// JSON の整数の配列。
    pub(crate) fn ints(v: &Value) -> Vec<i64> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect()
    }

    /// JSON の整数。
    pub(crate) fn int(v: &Value) -> i64 {
        v.as_i64().unwrap()
    }

    /// C の記号名から books.bin の番号。
    pub(crate) fn book_id(name: &str) -> BookId {
        let i = BOOKS
            .iter()
            .position(|b| b.name == name)
            .unwrap_or_else(|| panic!("{name} が books.bin にない"));
        BookId(i as u16)
    }

    /// 設定の `books`（book_param の順）から `fullbooks`。
    pub(crate) fn fullbooks(setup: &Value) -> Vec<Codebook> {
        setup["books"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                let book = vorbis_book_init_encode(book_id(b["name"].as_str().unwrap())).unwrap();
                assert_eq!(book.dim, int(&b["dim"]));
                assert_eq!(book.entries, int(&b["entries"]));
                assert_eq!(i64::from(book.c.maptype), int(&b["maptype"]));
                book
            })
            .collect()
    }

    /// 設定の `floors[i]` から `vorbis_info_floor1`（JSON にない欄は 0）。
    pub(crate) fn floor_info(f: &Value) -> VorbisInfoFloor1 {
        let mut info = VorbisInfoFloor1 {
            partitions: int(&f["partitions"]) as i32,
            mult: int(&f["mult"]) as i32,
            maxover: oracle::json_f32(&f["maxover"]),
            maxunder: oracle::json_f32(&f["maxunder"]),
            maxerr: oracle::json_f32(&f["maxerr"]),
            twofitweight: oracle::json_f32(&f["twofitweight"]),
            twofitatten: oracle::json_f32(&f["twofitatten"]),
            n: int(&f["n"]) as i32,
            ..VorbisInfoFloor1::default()
        };
        for (d, s) in info
            .partitionclass
            .iter_mut()
            .zip(ints(&f["partitionclass"]))
        {
            *d = s as i32;
        }
        for (d, s) in info.class_dim.iter_mut().zip(ints(&f["class_dim"])) {
            *d = s as i32;
        }
        for (d, s) in info.class_subs.iter_mut().zip(ints(&f["class_subs"])) {
            *d = s as i32;
        }
        for (d, s) in info.class_book.iter_mut().zip(ints(&f["class_book"])) {
            *d = s as i32;
        }
        for (d, s) in info
            .class_subbook
            .iter_mut()
            .zip(f["class_subbook"].as_array().unwrap())
        {
            for (dd, ss) in d.iter_mut().zip(ints(s)) {
                *dd = ss as i32;
            }
        }
        for (d, s) in info.postlist.iter_mut().zip(ints(&f["postlist"])) {
            *d = s as i32;
        }
        info
    }

    /// 設定の `residues[i]` から `vorbis_info_residue0`（JSON にない欄は 0）と型（1 か 2）。
    pub(crate) fn residue_info(r: &Value) -> (i32, VorbisInfoResidue0) {
        let mut info = VorbisInfoResidue0 {
            begin: int(&r["begin"]),
            end: int(&r["end"]),
            grouping: int(&r["grouping"]) as i32,
            partitions: int(&r["partitions"]) as i32,
            partvals: int(&r["partvals"]) as i32,
            groupbook: int(&r["groupbook"]) as i32,
            ..VorbisInfoResidue0::default()
        };
        for (d, s) in info.secondstages.iter_mut().zip(ints(&r["secondstages"])) {
            *d = s as i32;
        }
        for (d, s) in info.booklist.iter_mut().zip(ints(&r["booklist"])) {
            *d = s as i32;
        }
        for (d, s) in info.classmetric1.iter_mut().zip(ints(&r["classmetric1"])) {
            *d = s as i32;
        }
        for (d, s) in info.classmetric2.iter_mut().zip(ints(&r["classmetric2"])) {
            *d = s as i32;
        }
        (int(&r["type"]) as i32, info)
    }

    /// 照合の 1 ケース。
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Case {
        pub rate: u32,
        pub ch: usize,
        pub q: i32,
        pub signal: Signal,
        pub frames: usize,
    }

    /// 9 レート × 1・2・3・6・8 チャンネル × q（−1・0・3・5・10）× 5 種の素材の全部（1,125 ケース。0.6 秒、60,000 フレーム
    /// まで、3 チャンネル以上はその半分）。stereo・uncoupled のすべてのテンプレート（レートの帯）と、短い窓・長い窓の両方に
    /// 届く。デバッグのビルドで floor の照合（レベル 2 のダンプ）が約 1 分、residue・パケットの照合が約 35 秒。
    pub(crate) fn cases() -> Vec<Case> {
        const RATES: [u32; 9] = [
            8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
        ];
        const QS: [i32; 5] = [-1, 0, 3, 5, 10];
        const SIGNALS: [Signal; 5] = [
            Signal::Music,
            Signal::Impulses,
            Signal::Noise,
            Signal::Sine,
            Signal::Silence,
        ];
        let frames = |rate: u32| (rate as usize * 3 / 5).min(60_000);
        let mut out = Vec::new();
        for &rate in &RATES {
            for ch in [1usize, 2, 3, 6, 8] {
                for &q in &QS {
                    for &signal in &SIGNALS {
                        out.push(Case {
                            rate,
                            ch,
                            q,
                            signal,
                            frames: if ch > 2 {
                                frames(rate) / 2
                            } else {
                                frames(rate)
                            },
                        });
                    }
                }
            }
        }
        out
    }
}
