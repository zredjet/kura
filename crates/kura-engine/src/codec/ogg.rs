//! Ogg の多重化（RFC 3533。書くだけ）。Ogg Vorbis の書き出し（`codec::vorbis`）が使う。Opus でも使い回せる形にする。
//!
//! この書き手は「パケットとその閉じ方（[`Flush`]）」を受け取ってページにするだけで、Vorbis を知らない。どのパケットの
//! あとで閉じるか（下の A1〜A7 の規則）は呼び手（`vorbis::encoder::mux`）がパケットごとに決めて渡す。
//!
//! # 正（照合の基準）について
//!
//! 自作のエンコーダの照合（「照合用のエンコーダ」と音声パケットがビット単位で一致）は**音声パケットだけ**が対象で、
//! ページの切り方は libogg（`ogg_vendor/src/framing.c` の `ogg_stream_pageout`・`flush`）に合わせない。ここは
//! 段 7 の調査と検証で決めた、他のデコーダ（vorbisfile・lewton・symphonia・ffmpeg・stb_vorbis）で長さと位置が保たれる規則で
//! 切る（`docs/decisions.md` の ADR-149 (6) に記録した。照合用のエンコーダの出力をデコーダで読んだ確かめの要点は
//! `crates/vorbis-tables/oracle/README.md` の §7.2・§8.5）。
//!
//! # ページの形（RFC 3533。リトルエンディアン）
//!
//! `OggS`、版 0、旗（0x01 続き・0x02 BOS・0x04 EOS だけ）、グラニュール（i64）、シリアル（u32）、ページ番号（u32、0 から 1 ずつ）、
//! CRC（u32）、セグメントの数（0〜255）、lacing（各 0〜255。255 は「続く」、255 未満でパケットの終わり、長さが 255 の倍数の
//! パケットは最後に 0）、本体。
//!
//! CRC：多項式 0x04C11DB7、反射なし、初期値 0、最後の XOR なし。CRC の欄を 0 にしたページ全体にかける。表は
//! framing.c:110–125 と同じ作り方（`crc = i << 24` を 8 回 `(crc << 1) ^ (crc & 0x80000000 ? 0x04c11db7 : 0)`）、
//! 更新は `crc = (crc << 8) ^ T[((crc >> 24) ^ byte) & 0xff]`（framing.c:255–271 の `ogg_page_checksum_set`）。
//! 検査値：`"123456789"` → `0x89A1897F`。
//!
//! # ページの切り方（凍結する。変えるとバイト列が変わるので、使うエンコーダの版を上げる）
//!
//! - A1：1 ページ目は識別ヘッダ（30 バイト）だけ。BOS、グラニュール 0。
//! - A2：コメントヘッダは 2 ページ目の頭から、setup は同じページに続けてよい。setup の終わりでページを閉じる
//!   （ヘッダのページのグラニュールは 0、どのパケットも終わらないページは −1）。
//! - A3：最初の音声パケットは新しいページから。
//! - A4：最初の音声パケット（窓の 0 番。標本を出さない）だけで 1 ページを作り、グラニュール 0。音声のページを必ず 2 つ以上にする
//!   （音声が 1 ページだけのストリームは、lewton・ffmpeg が最後のページのグラニュールで末尾を切らずに長く読む。libvorbisfile は
//!   切る）。音声パケットが 1 つしかなければ、それが EOS のページ。
//! - A5：「長いブロックで次の旗が短い」パケットの直後ではページを閉じない（次のパケットのあとで閉じる）。そこで閉じたページが
//!   あると、lewton（`OggStreamReader`）が末尾の切り詰めを誤って長く読むことがある。段 7 の検証で A5 と A6′ を外した並べ方を
//!   読むと、長さを誤ったのは lewton だけで（ffmpeg・libvorbisfile・symphonia は誤らなかった）、端の長さ 405 件のうち 4 件、
//!   掃引 3,132 本のうち元では正しい 16 本で長く読んだ。
//! - 以降：パケットが終わった時点で本体が [`PAGE_FILL`] バイト以上なら閉じる。lacing が 255 に達したらパケットの途中でも
//!   閉じて、次のページを続き（0x01）で始める。
//! - A6：最後のパケットを含むページに EOS を立てる。空の EOS ページを足さない。
//! - A6′：最後の音声パケットだけで最後のページを作る（その前のパケットのあとで閉じる）。ただし前のパケットが A5 に当たるときは
//!   A5 を優先し、最後のページはその 2 つになる。**長さのための規則ではなく、規則を単純に保つための凍結**（変えるとバイト列が
//!   変わる）。段 7 の検証で、A6′ だけを外した並べ方（A4・A5・[`PAGE_FILL`]・A6 は残す）の 3,060 本（最後のページに音声
//!   パケットが 2 つ以上のものが 2,800 本）を読むと、symphonia・ffmpeg・libvorbisfile（sox）は誤りが 0 本、lewton の
//!   `OggStreamReader` は A6′ のある元と同じ 15 本（下の「lewton の読み口の限界」）で、A6′ は長さを何も変えなかった。
//!   照合用のエンコーダの確かめ（`oracle/README.md` §7.2）で長さを誤った 69 件も、内訳は「音声が 1 ページ」の 67 件（A4 の
//!   理由）と lewton の「長・次は短」の 2 件（A5 の理由）で、最後のページに複数のパケットがあること自体が原因の例はない。
//! - A7：各ページのグラニュールは、そのページで**終わる**最後のパケットの終わりの位置（窓の中心 c_k。最初の音声パケットは 0、
//!   以降 c_k = c_{k−1} + n_{k−1}/4 + n_k/4）。終わるパケットがなければ −1。最後のページは総フレーム数 N（末尾の詰め物を除く）。
//! - A10：シリアル番号は決定的に決める（乱数・時刻を使わない。決め方は `vorbis::encoder::serial_of`：ドメイン
//!   `kura-ogg-serial-v1` のもとで、入力の値のダイジェストと設定の正規の JSON を BLAKE3 にかけた先頭 4 バイト）。
//!
//! # lewton の読み口の限界（ページの切り方では避けられない）
//!
//! lewton 0.10.2 の `OggStreamReader` は、「長・次は短」のブロックのあとの短いブロックで終わり、最後の詰め物（総フレーム数より
//! 先の分）が lewton の余分より大きいストリームで、末尾を切りきれずに長く読む。A4〜A6′ を守っても起き、ほかのページの割り方でも
//! 合わせられない（`vorbis::encoder` のテストはこの形を「避けられない」として数える）。段 7 の検証では kura の出力 3,060 本の
//! うち 15 本（どれも 192 kHz・8 ch・q10 の正弦。例：1,000 フレームを 1,152 フレームと読む）で起き、同じファイルを ffmpeg・
//! libvorbisfile（sox）・symphonia は正しい長さで読んだ。このためテスト（`tests/ogg_vorbis.rs`）と
//! `kura-devtools vorbis-quality` は lewton の低い層（パケットの読み取り）だけを使い、最後のページのグラニュールで自分で切る。
//!
//! 書き手の側の細かい決まり（[`OggWriter::packet`]）：
//!
//! - [`Flush::Never`] のパケットでも lacing が 255 で埋まればそこで閉じるしかない。そうならないよう、Never のパケットの
//!   終わりでちょうど lacing が埋まるときは、**その前で**ページを閉じる（前のパケットも Never のときと、パケットの lacing の
//!   数そのものが 255 の倍数のときは避けられないので、埋まったところで閉じる）。
//! - EOS を立てたパケットは、閉じ方の指示によらずそのページを閉じる。
//!
//! ライセンス：CRC の表と更新は libogg（`ogg_vendor/src/framing.c`）に倣った（Copyright (c) 2002-2020 Xiph.org Foundation）。
//! BSD-3-Clause で、条件と免責は `vorbis/LICENSE`（ADR-149）。

use crate::error::EngineError;

/// ページを閉じる本体の大きさの目安（バイト）。libogg の既定と同じ 4,096。凍結（変えるならエンコーダの版を上げる）。
pub(crate) const PAGE_FILL: usize = 4_096;

/// 1 ページの lacing の数の上限。
pub(crate) const MAX_SEGMENTS: usize = 255;

/// ページの見出し（lacing の表の前まで）の大きさ（書き手は欄を順に書くので、使うのはテストのページの読み取りだけ）。
#[cfg(test)]
pub(crate) const HEADER_LEN: usize = 27;

/// 旗：前のページのパケットの続きで始まる。
pub(crate) const FLAG_CONTINUED: u8 = 0x01;
/// 旗：論理ストリームの最初のページ。
pub(crate) const FLAG_BOS: u8 = 0x02;
/// 旗：論理ストリームの最後のページ。
pub(crate) const FLAG_EOS: u8 = 0x04;

/// libogg の `ogg_packet` の写し（書き出しに要るものだけ）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OggPacket {
    /// パケットのバイト列。
    pub packet: Vec<u8>,
    /// 最後のパケットか（C の `e_o_s`）。
    pub e_o_s: bool,
    /// このパケットの終わりの位置（C の `granulepos`。ヘッダは 0）。ページのグラニュールは [`OggWriter`] が A7 で決める。
    pub granulepos: i64,
    /// 通し番号（C の `packetno`。ヘッダは 0〜2、音声は 3 から）。
    pub packetno: i64,
}

/// ページを閉じるときの指示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flush {
    /// 規則（PAGE_FILL・lacing）のとおりに閉じる。
    Auto,
    /// このパケットのあとで必ず閉じる（A1・A2・A4・A6′）。
    After,
    /// このパケットのあとでは閉じない（A5）。lacing が 255 で埋まるときだけは閉じる（その前で閉じて避ける）。
    Never,
}

/// Ogg の書き手（1 本の論理ストリーム）。
#[derive(Debug, Default)]
pub(crate) struct OggWriter {
    /// シリアル番号（A10）。
    pub serial: u32,
    /// 次のページの番号。
    pub pageno: u32,
    /// 書いたページ。
    pub out: Vec<u8>,
    /// 書きかけのページの lacing。
    pub lacing: Vec<u8>,
    /// 書きかけのページの本体。
    pub body: Vec<u8>,
    /// 書きかけのページで終わった最後のパケットのグラニュール（なければ −1）。
    pub granule: i64,
    /// 最初のページを書いたか（BOS は最初のページだけ）。
    pub bos_done: bool,
    /// 書きかけのページが前のページのパケットの続きで始まるか（旗 0x01）。
    pub continued: bool,
    /// 書きかけのページで最後に終わったパケットが [`Flush::Never`] だったか（その直後で閉じないため）。
    pub hold: bool,
}

impl OggWriter {
    /// 書き手を作る。
    pub fn with_serial(serial: u32) -> Self {
        Self {
            serial,
            granule: -1,
            ..Self::default()
        }
    }

    /// パケットを 1 つ足す。`granule` はこのパケットの終わりの位置（A7）、`eos` は最後のパケット（そのページに EOS を立てて
    /// 閉じる）、`flush` は閉じ方。lacing が 255 に達したらパケットの途中でもページを閉じて続ける。
    pub fn packet(
        &mut self,
        data: &[u8],
        granule: i64,
        eos: bool,
        flush: Flush,
    ) -> Result<(), EngineError> {
        // このパケットの lacing の数（長さが 255 の倍数なら最後に 0 を足すので、いつも len/255 + 1）
        let segs = data.len() / 255 + 1;
        // A5：閉じないパケットの終わりでちょうど lacing が埋まると、その直後で閉じることになる → 先に閉じて避ける
        // （新しいページから始めても埋まる長さ（segs が 255 の倍数）なら避けられないので閉じない）
        if flush == Flush::Never
            && !eos
            && !self.hold
            && !self.lacing.is_empty()
            && (self.lacing.len() + segs).is_multiple_of(MAX_SEGMENTS)
            && !segs.is_multiple_of(MAX_SEGMENTS)
        {
            self.flush_page(false);
        }
        let mut rest = data;
        let mut started = false;
        loop {
            if self.lacing.len() >= MAX_SEGMENTS {
                // ページが埋まった：閉じて、パケットの途中なら次のページを続きで始める
                self.flush_page(false);
                self.continued = started;
            }
            let take = rest.len().min(255);
            let (seg, tail) = rest.split_at(take);
            self.lacing.push(
                u8::try_from(take).map_err(|_| {
                    EngineError::Internal("Ogg の lacing が 255 を超えました".into())
                })?,
            );
            self.body.extend_from_slice(seg);
            rest = tail;
            started = true;
            if take < 255 {
                break;
            }
        }
        // パケットがこのページで終わった
        self.granule = granule;
        self.hold = flush == Flush::Never;
        if eos {
            self.flush_page(true);
            return Ok(());
        }
        let close = match flush {
            Flush::After => true,
            Flush::Never => false,
            Flush::Auto => self.body.len() >= PAGE_FILL,
        };
        if close || self.lacing.len() >= MAX_SEGMENTS {
            self.flush_page(false);
        }
        Ok(())
    }

    /// 書きかけのページを閉じる（空なら何もしない）。`eos` で EOS を立てる。
    pub fn flush_page(&mut self, eos: bool) {
        if self.lacing.is_empty() {
            return;
        }
        let mut flags = 0u8;
        if self.continued {
            flags |= FLAG_CONTINUED;
        }
        if !self.bos_done {
            flags |= FLAG_BOS;
        }
        if eos {
            flags |= FLAG_EOS;
        }
        // lacing は packet() で 255 以下に保っている
        let segments = u8::try_from(self.lacing.len()).unwrap_or(u8::MAX);
        let start = self.out.len();
        self.out.extend_from_slice(b"OggS");
        self.out.push(0);
        self.out.push(flags);
        self.out.extend_from_slice(&self.granule.to_le_bytes());
        self.out.extend_from_slice(&self.serial.to_le_bytes());
        self.out.extend_from_slice(&self.pageno.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.out.push(segments);
        self.out.extend_from_slice(&self.lacing);
        self.out.extend_from_slice(&self.body);
        if let Some(page) = self.out.get_mut(start..) {
            page_checksum_set(page);
        }
        // 2^32 ページ（100 GB を超える）には届かない
        self.pageno = self.pageno.wrapping_add(1);
        self.bos_done = true;
        self.continued = false;
        self.hold = false;
        self.granule = -1;
        self.lacing.clear();
        self.body.clear();
    }

    /// 書いた全体を返す（書きかけのページがあれば閉じてから）。
    pub fn finish(mut self) -> Vec<u8> {
        self.flush_page(false);
        self.out
    }
}

/// CRC の表（framing.c:110–125 の `_ogg_crc_entry` と同じ作り方）。
const CRC_TABLE: [u32; 256] = crc_table();

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x8000_0000 != 0 {
                (r << 1) ^ 0x04c1_1db7
            } else {
                r << 1
            };
            j += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
}

/// Ogg の CRC32（多項式 0x04C11DB7、反射なし、初期値 0）。framing.c:110–125・255–271 と同じ値。
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        let idx = ((crc >> 24) as u8) ^ b;
        crc = (crc << 8) ^ CRC_TABLE[usize::from(idx)];
    }
    crc
}

/// ページの CRC の欄（22〜25 バイト目）を 0 にしてから CRC を計算して書き込む（framing.c の `ogg_page_checksum_set`）。
/// CRC の欄に届かない短いものは何もしない。
pub(crate) fn page_checksum_set(page: &mut [u8]) {
    let Some(field) = page.get_mut(22..26) else {
        return;
    };
    field.fill(0);
    let crc = crc32(page);
    if let Some(field) = page.get_mut(22..26) {
        field.copy_from_slice(&crc.to_le_bytes());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use rand_chacha::rand_core::Rng;

    use super::*;

    /// 読み直したページ。
    #[derive(Debug, Clone)]
    pub(crate) struct Page {
        pub flags: u8,
        pub granule: i64,
        pub serial: u32,
        pub seq: u32,
        pub lacing: Vec<u8>,
        pub body: Vec<u8>,
    }

    /// ページに分ける（形・版・旗・CRC を確かめる）。
    pub(crate) fn pages(bytes: &[u8]) -> Vec<Page> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() {
            let h = &bytes[at..];
            assert!(h.len() >= HEADER_LEN, "ページの見出しが切れている（{at}）");
            assert_eq!(&h[0..4], b"OggS", "ページの印（{at}）");
            assert_eq!(h[4], 0, "版");
            let flags = h[5];
            assert_eq!(flags & !0x07, 0, "未定義の旗");
            let granule = i64::from_le_bytes(h[6..14].try_into().unwrap());
            let serial = u32::from_le_bytes(h[14..18].try_into().unwrap());
            let seq = u32::from_le_bytes(h[18..22].try_into().unwrap());
            let crc = u32::from_le_bytes(h[22..26].try_into().unwrap());
            let n = usize::from(h[26]);
            let lacing = h[27..27 + n].to_vec();
            let body_len: usize = lacing.iter().map(|l| usize::from(*l)).sum();
            let total = 27 + n + body_len;
            let mut page = h[..total].to_vec();
            page[22..26].fill(0);
            assert_eq!(
                crc32(&page),
                crc,
                "CRC（ページ {seq}。表の計算は crc_matches_check_value_and_definition で定義と照らす）"
            );
            out.push(Page {
                flags,
                granule,
                serial,
                seq,
                body: h[27 + n..total].to_vec(),
                lacing,
            });
            at += total;
        }
        out
    }

    /// ページからパケットを組み立てる：（パケット, それが終わったページの番号）。続きの旗とパケットの切れ目が合うことも確かめる。
    pub(crate) fn packets(pages: &[Page]) -> Vec<(Vec<u8>, usize)> {
        let mut out = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        let mut open = false;
        for (i, p) in pages.iter().enumerate() {
            assert_eq!(
                p.flags & FLAG_CONTINUED != 0,
                open,
                "ページ {i}：続きの旗と前のページの終わりが合わない"
            );
            let mut off = 0usize;
            for &l in &p.lacing {
                cur.extend_from_slice(&p.body[off..off + usize::from(l)]);
                off += usize::from(l);
                open = true;
                if l < 255 {
                    out.push((std::mem::take(&mut cur), i));
                    open = false;
                }
            }
        }
        assert!(!open, "最後のパケットが終わっていません");
        out
    }

    /// パケットごとの（始まったページ, 終わったページ）。
    pub(crate) fn packet_spans(pages: &[Page]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut start: Option<usize> = None;
        for (i, p) in pages.iter().enumerate() {
            for &l in &p.lacing {
                let s = *start.get_or_insert(i);
                if l < 255 {
                    out.push((s, i));
                    start = None;
                }
            }
        }
        out
    }

    /// パケット `k` の直後でページを閉じたか（`k` がそのページで終わる最後のパケットで、ページの最後の lacing が 255 未満）。
    pub(crate) fn closed_after(pages: &[Page], spans: &[(usize, usize)], k: usize) -> bool {
        let page = spans[k].1;
        spans.get(k + 1).is_none_or(|(start, _)| *start != page)
            && pages[page].lacing.last().is_some_and(|l| *l < 255)
    }

    /// 1 ビットずつの CRC（表を使わない定義どおりの計算）。
    fn crc32_bitwise(data: &[u8]) -> u32 {
        let mut crc = 0u32;
        for &b in data {
            crc ^= u32::from(b) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04C1_1DB7
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    /// 検査値と、表の計算が定義どおりの計算と同じこと。
    #[test]
    fn crc_matches_check_value_and_definition() {
        assert_eq!(crc32(b"123456789"), 0x89A1_897F);
        assert_eq!(crc32(b""), 0);
        let mut r = crate::determinism::rng(7);
        for len in [1usize, 2, 3, 27, 255, 4_096, 10_000] {
            let data: Vec<u8> = (0..len).map(|_| (r.next_u32() & 0xff) as u8).collect();
            assert_eq!(crc32(&data), crc32_bitwise(&data), "{len}");
        }
        // libogg の表の最初のいくつか（framing.c の crc_lookup[0]）
        assert_eq!(&CRC_TABLE[..4], &[0, 0x04c1_1db7, 0x0982_3b6e, 0x0d43_26d9]);
        assert_eq!(CRC_TABLE[255], 0xb1f7_40b4);
        // 短いものには何もしない
        let mut short = vec![1u8; 20];
        page_checksum_set(&mut short);
        assert_eq!(short, vec![1u8; 20]);
    }

    /// 1 ページの形（見出しの各欄・CRC）を、手で組んだバイト列と比べる。
    #[test]
    fn single_page_layout_is_exact() {
        let mut w = OggWriter::with_serial(0x1234_5678);
        w.packet(b"abc", 0, false, Flush::After).unwrap();
        w.packet(&[7u8; 300], 77, true, Flush::Auto).unwrap();
        let bytes = w.finish();
        let mut want = Vec::new();
        // 1 ページ目：BOS、グラニュール 0、ページ 0、lacing [3]
        let mut p0 = b"OggS".to_vec();
        p0.extend_from_slice(&[0, FLAG_BOS]);
        p0.extend_from_slice(&0i64.to_le_bytes());
        p0.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        p0.extend_from_slice(&0u32.to_le_bytes());
        p0.extend_from_slice(&[0; 4]);
        p0.extend_from_slice(&[1, 3]);
        p0.extend_from_slice(b"abc");
        let crc = crc32_bitwise(&p0);
        p0[22..26].copy_from_slice(&crc.to_le_bytes());
        want.extend_from_slice(&p0);
        // 2 ページ目：EOS、グラニュール 77、ページ 1、lacing [255, 45]
        let mut p1 = b"OggS".to_vec();
        p1.extend_from_slice(&[0, FLAG_EOS]);
        p1.extend_from_slice(&77i64.to_le_bytes());
        p1.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        p1.extend_from_slice(&1u32.to_le_bytes());
        p1.extend_from_slice(&[0; 4]);
        p1.extend_from_slice(&[2, 255, 45]);
        p1.extend_from_slice(&[7u8; 300]);
        let crc = crc32_bitwise(&p1);
        p1[22..26].copy_from_slice(&crc.to_le_bytes());
        want.extend_from_slice(&p1);
        assert_eq!(bytes, want);
    }

    /// lacing：長さ 0・255 の倍数・ページをまたぐ大きなパケットが、そのまま組み立て直せる。
    #[test]
    fn lacing_and_spanning_packets_round_trip() {
        let lens = [
            0usize, 1, 254, 255, 256, 510, 511, 65_024, 65_025, 65_026, 200_000, 3,
        ];
        let mut w = OggWriter::with_serial(5);
        let mut want = Vec::new();
        for (i, &len) in lens.iter().enumerate() {
            let data: Vec<u8> = (0..len).map(|k| (k * 31 + i) as u8).collect();
            let eos = i + 1 == lens.len();
            w.packet(&data, i as i64, eos, Flush::Auto).unwrap();
            want.push(data);
        }
        let ps = pages(&w.finish());
        let pk = packets(&ps);
        assert_eq!(pk.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), want);
        for (i, p) in ps.iter().enumerate() {
            assert!(p.lacing.len() <= MAX_SEGMENTS);
            assert_eq!(p.seq as usize, i);
            assert_eq!(p.serial, 5);
            assert_eq!(p.flags & FLAG_BOS != 0, i == 0);
            assert_eq!(p.flags & FLAG_EOS != 0, i + 1 == ps.len());
            // グラニュール：そのページで終わる最後のパケットの値、なければ −1
            let last = pk.iter().rposition(|(_, page)| *page == i);
            assert_eq!(p.granule, last.map_or(-1, |k| k as i64), "ページ {i}");
        }
        // 255 の倍数のパケットは最後に 0 の lacing
        assert!(ps.iter().any(|p| p.lacing.contains(&0)));
        // 200,000 バイト（785 セグメント）のパケットは 4 ページにまたがり、真ん中のページはどのパケットも終わらない（−1）
        let middle: Vec<&Page> = ps.iter().filter(|p| p.granule == -1).collect();
        assert_eq!(middle.len(), 2);
        assert!(
            middle
                .iter()
                .all(|p| p.lacing == [255; 255] && p.flags == FLAG_CONTINUED)
        );
    }

    /// 閉じ方：After は閉じる、Never は PAGE_FILL を超えても閉じない、Auto は PAGE_FILL 以上で閉じる。
    #[test]
    fn flush_modes_close_pages_as_told() {
        let mut w = OggWriter::with_serial(1);
        w.packet(&[1u8; 30], 0, false, Flush::After).unwrap();
        w.packet(&[2u8; 5_000], 1, false, Flush::Never).unwrap();
        w.packet(&[3u8; 10], 2, false, Flush::Auto).unwrap();
        w.packet(&[4u8; 4_000], 3, false, Flush::Auto).unwrap();
        w.packet(&[5u8; 95], 4, false, Flush::Auto).unwrap();
        w.packet(&[6u8; 1], 5, false, Flush::Auto).unwrap();
        w.packet(&[7u8; 1], 6, true, Flush::Never).unwrap();
        let ps = pages(&w.finish());
        let pk = packets(&ps);
        let page_of: Vec<usize> = pk.iter().map(|(_, p)| *p).collect();
        // 0 | 1 2 (5,010 ≥ 4,096 で閉じる) | 3 4 (4,095 < 4,096) 5 (4,096 で閉じる) | 6（EOS は Never でも閉じる）
        assert_eq!(page_of, vec![0, 1, 1, 2, 2, 2, 3]);
        assert_eq!(
            ps.iter().map(|p| p.granule).collect::<Vec<_>>(),
            vec![0, 2, 5, 6]
        );
        assert_eq!(ps[3].flags, FLAG_EOS);
        // 空のページは閉じても何も書かない
        let mut w = OggWriter::with_serial(1);
        w.flush_page(true);
        assert!(w.finish().is_empty());
        // EOS を付けずに終えると、書きかけのページを EOS なしで閉じる
        let mut w = OggWriter::with_serial(1);
        w.packet(b"x", 9, false, Flush::Never).unwrap();
        let ps = pages(&w.finish());
        assert_eq!(ps.len(), 1);
        assert_eq!((ps[0].flags, ps[0].granule), (FLAG_BOS, 9));
    }

    /// Never のパケットで lacing がちょうど 255 に達するときは、その前で閉じる（Never の直後で閉じない）。
    /// 前のパケットも Never なら避けられないので、埋まったところで閉じる。
    #[test]
    fn never_packet_avoids_filling_the_lacing() {
        let mut w = OggWriter::with_serial(1);
        for i in 0..254 {
            w.packet(&[1], i, false, Flush::Auto).unwrap();
        }
        // 255 個目が Never：その前で閉じ、次のページの頭に置く
        w.packet(&[2], 254, false, Flush::Never).unwrap();
        w.packet(&[3], 255, true, Flush::Auto).unwrap();
        let ps = pages(&w.finish());
        assert_eq!(
            ps.iter().map(|p| p.lacing.len()).collect::<Vec<_>>(),
            vec![254, 2]
        );
        assert_eq!(ps[0].granule, 253);
        // 2 セグメントのパケット（255 バイト）でもちょうど埋まるなら同じ
        let mut w = OggWriter::with_serial(1);
        for i in 0..253 {
            w.packet(&[1], i, false, Flush::Auto).unwrap();
        }
        w.packet(&[2u8; 255], 253, false, Flush::Never).unwrap();
        w.packet(&[3], 254, true, Flush::Auto).unwrap();
        let ps = pages(&w.finish());
        assert_eq!(
            ps.iter().map(|p| p.lacing.len()).collect::<Vec<_>>(),
            vec![253, 3]
        );
        // 前も Never：避けられないので 255 で閉じる
        let mut w = OggWriter::with_serial(1);
        for i in 0..253 {
            w.packet(&[1], i, false, Flush::Auto).unwrap();
        }
        w.packet(&[1], 253, false, Flush::Never).unwrap();
        w.packet(&[2], 254, false, Flush::Never).unwrap();
        w.packet(&[3], 255, true, Flush::Auto).unwrap();
        let ps = pages(&w.finish());
        assert_eq!(
            ps.iter().map(|p| p.lacing.len()).collect::<Vec<_>>(),
            vec![255, 1]
        );
    }

    /// でたらめな長さ・閉じ方の列でも、ページの不変の性質が保たれる（ChaCha8 で作る）。
    #[test]
    fn random_streams_keep_page_invariants() {
        for seed in 0..40u64 {
            let mut r = crate::determinism::rng(seed);
            let count = 1 + (r.next_u32() % 400) as usize;
            let mut w = OggWriter::with_serial(seed as u32);
            let mut want = Vec::new();
            let mut flushes = Vec::new();
            for i in 0..count {
                let len = match r.next_u32() % 10 {
                    0 => 0,
                    1..=5 => (r.next_u32() % 40) as usize,
                    6 | 7 => (r.next_u32() % 3_000) as usize,
                    8 => 255 * (1 + (r.next_u32() % 4) as usize),
                    _ => (r.next_u32() % 70_000) as usize,
                };
                let flush = match r.next_u32() % 5 {
                    0 => Flush::After,
                    1 => Flush::Never,
                    _ => Flush::Auto,
                };
                let salt = r.next_u32();
                let data: Vec<u8> = (0..len as u32)
                    .map(|k| (k.wrapping_mul(0x9E37_79B1).wrapping_add(salt) >> 24) as u8)
                    .collect();
                let eos = i + 1 == count;
                w.packet(&data, i as i64 * 10, eos, flush).unwrap();
                want.push(data);
                flushes.push(flush);
            }
            let bytes = w.finish();
            let ps = pages(&bytes);
            let pk = packets(&ps);
            assert_eq!(pk.len(), count, "{seed}");
            // パケット k の直後でページを閉じたか（k がそのページで終わる最後のパケットで、ページの最後の lacing が 255 未満）
            let closed_after = |k: usize| {
                let page = pk[k].1;
                pk.get(k + 1).is_none_or(|(_, next)| *next != page)
                    && ps[page].lacing.last().is_some_and(|l| *l < 255)
            };
            for (k, ((got, page), data)) in pk.iter().zip(&want).enumerate() {
                assert_eq!(got, data, "{seed}：パケット {k}");
                if flushes[k] == Flush::After {
                    assert!(closed_after(k), "{seed}：After のパケット {k}");
                }
                // Never の直後で閉じるのは、lacing が埋まって避けられないときだけ
                if flushes[k] == Flush::Never && k + 1 < count && closed_after(k) {
                    let segs = data.len() / 255 + 1;
                    let prev_never = k > 0 && flushes[k - 1] == Flush::Never;
                    assert!(
                        ps[*page].lacing.len() == MAX_SEGMENTS
                            && (prev_never || segs.is_multiple_of(MAX_SEGMENTS)),
                        "{seed}：Never のパケット {k} の直後で閉じた"
                    );
                }
                // Auto のパケットの直後で閉じたなら、PAGE_FILL 以上か lacing が埋まったか、次が Never で先に閉じたとき
                if flushes[k] == Flush::Auto && k + 1 < count && closed_after(k) {
                    let p = &ps[*page];
                    assert!(
                        p.body.len() >= PAGE_FILL
                            || p.lacing.len() == MAX_SEGMENTS
                            || flushes[k + 1] == Flush::Never,
                        "{seed}：パケット {k} のあとで早く閉じた"
                    );
                }
                // PAGE_FILL に達したら閉じる（Never の保留を除く）
                if flushes[k] != Flush::Never && k + 1 < count && !closed_after(k) {
                    let page = &ps[*page];
                    let upto: usize = {
                        // ページの頭からパケット k の終わりまでの本体の大きさ
                        let mut seen = 0usize;
                        let mut size = 0usize;
                        let ends_before = pk[..k].iter().filter(|(_, p)| *p == pk[k].1).count();
                        for &l in &page.lacing {
                            size += usize::from(l);
                            if l < 255 {
                                seen += 1;
                                if seen == ends_before + 1 {
                                    break;
                                }
                            }
                        }
                        size
                    };
                    assert!(
                        upto < PAGE_FILL,
                        "{seed}：パケット {k} のあとで閉じなかった"
                    );
                }
            }
            for (i, p) in ps.iter().enumerate() {
                assert!(!p.lacing.is_empty() && p.lacing.len() <= MAX_SEGMENTS);
                assert_eq!(p.seq as usize, i);
                assert_eq!(p.flags & FLAG_BOS != 0, i == 0);
                assert_eq!(p.flags & FLAG_EOS != 0, i + 1 == ps.len());
                let last = pk.iter().rposition(|(_, page)| *page == i);
                assert_eq!(p.granule, last.map_or(-1, |k| k as i64 * 10));
            }
        }
    }
}
