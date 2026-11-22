//! aoTuV beta 6.03（libvorbis 1.3.7 が土台）の Ogg Vorbis のエンコーダの表。kura-engine の自作のエンコーダ
//! （`codec::vorbis`、`ogg-vorbis-kura-enc/1`）が使う。
//!
//! aoTuV - Copyright (c) 2003-2021 Aoyumi / Copyright (c) 2002-2020 Xiph.org Foundation。BSD-3-Clause（`LICENSE`）。
//!
//! - 中身は `aotuv_lancer_vorbis_sys` 0.1.6 の `vorbis_vendor` の C のヘッダ（`lib/modes/*.h`・`lib/books/*.h`・
//!   `lib/masking.h`）と `window.c`・`floor1.c`・`psy.c`・`envelope.c` の表から `gen/gen.py` で生成し、生成物
//!   （`src/data.rs`・`src/books.rs`・`src/books.bin`）をコミットする。ビルドのときには生成しない。手順は `gen/README.md`。
//! - テンプレートは `vorbisenc.c` の `setup_list` から 5.1 の `ve_setup_44_51` を除いた 18 個（[`SETUP_LIST`]。
//!   並びは C のまま）。テンプレートからたどれるすべての構造体・配列を `pub static` にした（名前は C の名前の大文字）。
//! - codebook はテンプレートから使われるすべて（stereo・uncoupled・floor、すべてのレート。5.1 だけが使うものは除く。
//!   ビットレートの管理だけが使う `*_managed` も含む）を、libvorbis の `vorbis_staticbook_pack` で詰めた形で
//!   [`BOOKS_BIN`] に並べ、名前と範囲を [`BOOKS`] に置く。テンプレートは [`BookId`]（[`BOOKS`] の添字）で指す。
//!   [`unpack`] で `static_codebook` の写し（[`StaticCodebook`]）に戻せる。
//! - 値はコンパイラが C の初期化子から作ったビットのまま（浮動小数は最短の 10 進で書き、同じビットに戻ることを
//!   生成のときに有理数で確かめた）。テストが、Rust の値から作ったバイト列の SHA-256 を、生成の道具が C の値から
//!   計算したものと照らし、さらに凍結した値とも照らす。

#![forbid(unsafe_code)]

mod codebook;
mod types;

#[rustfmt::skip]
#[allow(clippy::approx_constant, clippy::excessive_precision, clippy::unreadable_literal)]
mod books;
#[rustfmt::skip]
#[allow(
    clippy::approx_constant,
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::redundant_static_lifetimes
)]
mod data;

pub use books::{BOOKS, BOOKS_BIN};
pub use codebook::{
    BitWriter, StaticCodebook, UnpackError, book_maptype1_quantvals, ov_ilog, pack, packed, unpack,
};
pub use data::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// books.bin を凍結する：生成の道具が計算した値と、ここに書き写した値の両方と照らす。
    /// 変わったら aoTuV の版か生成の道具を変えたということ（エンコーダの書くバイト列が変わるので、版を上げる）。
    #[test]
    fn books_bin_is_frozen() {
        let got = sha256_hex(BOOKS_BIN);
        assert_eq!(got, data::BOOKS_BIN_SHA256, "books.bin が生成の記録と違う");
        assert_eq!(
            got,
            "ae074742261f7a6597379cb42175c39675f1dc99728cb80a88ce202b6535e084"
        );
    }

    /// 生成したデータ（すべての static・SETUP_LIST・BOOKS）を凍結する。Rust の値から作ったバイト列が、gen.py が
    /// C の値から作ったものと同じ（10 進の書き方・型の写し方で値が変わっていない）ことも確かめる。
    #[test]
    fn data_is_frozen_and_matches_c() {
        let mut bytes = Vec::new();
        data::feed_all(&mut bytes);
        let got = sha256_hex(&bytes);
        assert_eq!(got, data::DATA_SHA256, "Rust の値が C の値と違う");
        assert_eq!(
            got,
            "0c0025b26cd01db81ccf33455a840276e1a415fa1e575bde8bc4e5093f9d57f0"
        );
    }

    /// すべての book が詰めた形から読み戻せ、詰め直すと C（`vorbis_staticbook_pack`）とビット単位で同じになる。
    #[test]
    fn every_book_unpacks_and_repacks() {
        let mut total = 0usize;
        for (i, b) in BOOKS.iter().enumerate() {
            let id = BookId(i as u16);
            let c = unpack(id).unwrap_or_else(|e| panic!("{}: {e}", b.name));
            assert_eq!(c.lengthlist.len() as i64, c.entries, "{}", b.name);
            assert!(c.lengthlist.iter().all(|&l| l <= 32), "{}", b.name);
            if c.maptype == 1 {
                let q = book_maptype1_quantvals(c.entries, c.dim);
                assert_eq!(c.quantlist.len() as i64, q, "{}", b.name);
                assert!(q.pow(c.dim as u32) <= c.entries && (q + 1).pow(c.dim as u32) > c.entries);
            }
            let mut w = BitWriter::default();
            pack(&c, &mut w).unwrap();
            let (bytes, bits) = packed(id).unwrap();
            assert_eq!(
                (w.bytes.as_slice(), w.bits),
                (bytes, bits as usize),
                "{}",
                b.name
            );
            assert_eq!(b.len as usize, (b.bits as usize).div_ceil(8), "{}", b.name);
            assert_eq!(b.offset as usize, total, "{}", b.name);
            total += b.len as usize;
        }
        assert_eq!(total, BOOKS_BIN.len());
        assert_eq!(BOOKS.len(), 598);
    }

    /// テンプレートは setup_list の 19 個から 5.1 を除いた 18 個。チャンネルの制約は stereo（2）か uncoupled（-1）だけ。
    #[test]
    fn templates_are_setup_list_without_5_1() {
        assert_eq!(SETUP_LIST.len(), 19 - 1);
        assert!(
            SETUP_LIST
                .iter()
                .all(|t| matches!(t.coupling_restriction, 2 | -1))
        );
        // 先頭は 48 kHz の stereo（5.1 を除いた C の並び）
        assert!(std::ptr::eq(SETUP_LIST[0], &VE_SETUP_48_STEREO));
        assert!(std::ptr::eq(SETUP_LIST[17], &VE_SETUP_XX_UNCOUPLED));
        // テンプレートが指す book はすべて範囲の中
        let in_range = |b: &BookId| usize::from(b.0) < BOOKS.len();
        for t in SETUP_LIST {
            for list in t.floor_books {
                assert!(list.iter().flatten().all(in_range));
            }
            for m in t.maps {
                for r in m.res {
                    assert!(in_range(&r.book_aux) && in_range(&r.book_aux_managed));
                    for bb in [r.books_base, r.books_base_managed] {
                        assert!(bb.books.iter().flatten().flatten().all(in_range));
                    }
                }
            }
        }
    }

    /// floor1 のポストの位置は重ならない。だから `floor1_look` の `qsort`（安定でない）は、どの正しい並べ替えでも
    /// 同じ順になる（Rust の安定な sort で写してよい）。
    #[test]
    fn floor_posts_are_distinct() {
        for (k, f) in FLOOR.iter().enumerate() {
            let posts = 2
                + (0..f.partitions as usize)
                    .map(|i| f.class_dim[f.partitionclass[i] as usize] as usize)
                    .sum::<usize>();
            let mut xs: Vec<i32> = f.postlist[..posts].to_vec();
            xs.sort();
            xs.dedup();
            assert_eq!(xs.len(), posts, "floor {k}");
        }
    }

    /// 窓の表は長さ n/2 で、0 から 1 へ増える（window.c の vwin64〜vwin8192）。
    #[test]
    fn windows_have_the_expected_shape() {
        let tables: [&[f32]; 8] = [
            &VWIN64, &VWIN128, &VWIN256, &VWIN512, &VWIN1024, &VWIN2048, &VWIN4096, &VWIN8192,
        ];
        for (k, w) in tables.iter().enumerate() {
            assert_eq!(w.len(), 32 << k);
            assert!(w.windows(2).all(|p| p[0] <= p[1]));
            assert!(w[0] > 0.0 && w[w.len() - 1] <= 1.0);
        }
        assert_eq!(FLOOR1_FROMDB_LOOKUP[255], 1.0);
    }

    #[test]
    fn ilog_and_quantvals_match_libvorbis() {
        assert_eq!(
            [
                ov_ilog(0),
                ov_ilog(1),
                ov_ilog(2),
                ov_ilog(255),
                ov_ilog(256)
            ],
            [0, 1, 2, 8, 9]
        );
        assert_eq!(book_maptype1_quantvals(6561, 8), 3);
        assert_eq!(book_maptype1_quantvals(6560, 8), 2);
        assert_eq!(book_maptype1_quantvals(25, 1), 25);
    }
}
