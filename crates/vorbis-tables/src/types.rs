//! C の型の写し（aoTuV b6.03 の `vorbisenc.c`・`codec_internal.h`・`backends.h`・`psy.h`）。
//!
//! - 数値の型は C と同じ幅：`int` → `i32`、`long` → `i64`（LP64）、`float` → `f32`、`double` → `f64`、`char` → `u8`。
//! - フィールドの名前は C の名前を snake_case に（`dB`・`kHz` のような単位は 1 語：`max_curve_db`・`lowpass_khz`）。
//! - ポインタは `&'static`（配列を指すものは `&'static [T]`、NULL がありうる欄は `Option`）。codebook
//!   （`static_codebook *`）へのポインタは [`BookId`]（[`crate::BOOKS`] の添字）。
//! - 配列の大きさは C の定数（`P_BANDS` など）のまま。値の残り（C の初期化子で省いた要素）は 0。

/// `psy.h`：心理モデルの帯域の数（62 Hz〜16 kHz の半オクターブ）。
pub const P_BANDS: usize = 17;
/// `psy.h`：トーンの曲線の音量の段（30〜100 dB）。
pub const P_LEVELS: usize = 8;
/// `psy.h`：最も小さい段の音量（dB）。
pub const P_LEVEL_0: f64 = 30.0;
/// `psy.h`：ノイズのオフセットの曲線の数（0・2 はビットレートの管理だけ。VBR は 1）。
pub const P_NOISECURVES: usize = 3;
/// `psy.h`：noise compander の段の数。
pub const NOISE_COMPAND_LEVELS: usize = 40;
/// `envelope.h`：過渡の検出の帯域の数（aoTuV は 12。Xiph は先頭の 7）。
pub const VE_BANDS: usize = 12;
/// `codec_internal.h`：ビットレートの管理の blob の数（VBR は真ん中の 1 つ `PACKETBLOBS / 2` だけを使う）。
pub const PACKETBLOBS: usize = 15;
/// `backends.h`：floor1 のポストの数の上限（暗黙の 2 つを除く）。
pub const VIF_POSIT: usize = 63;
/// `backends.h`：floor1 の class の数の上限。
pub const VIF_CLASS: usize = 16;
/// `backends.h`：floor1 の partition の数の上限。
pub const VIF_PARTS: usize = 31;
/// `masking.h`：トーンの曲線の長さ（1/8 オクターブ刻み）。
pub const EHMER_MAX: usize = 56;
/// `masking.h`：トーンの曲線の中心の位置。
pub const EHMER_OFFSET: usize = 16;
/// `masking.h`：ATH の表の長さ。
pub const MAX_ATH: usize = 88;

/// codebook の番号（[`crate::BOOKS`] の添字）。C の `const static_codebook *` の代わり。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BookId(pub u16);

/// books.bin の中の 1 冊：C の記号名と、`vorbis_staticbook_pack` で詰めたバイト列の範囲・ビット数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Book {
    /// C の記号名（例 `_44c5_s_p1_0`）。
    pub name: &'static str,
    /// books.bin の中の先頭（バイト）。
    pub offset: u32,
    /// バイト数（= ⌈bits / 8⌉）。
    pub len: u32,
    /// 詰めた形のビット数（setup ヘッダにはこのビット数だけ書く。最後のバイトの余りは 0）。
    pub bits: u32,
}

/// `vorbis_info_mode`（`codec_internal.h`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VorbisInfoMode {
    pub blockflag: i32,
    pub windowtype: i32,
    pub transformtype: i32,
    pub mapping: i32,
}

/// `vorbis_info_mapping0`（`backends.h`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VorbisInfoMapping0 {
    /// submap の数（≤ 16）。
    pub submaps: i32,
    /// チャンネル → submap。
    pub chmuxlist: [i32; 256],
    /// submap → floor。
    pub floorsubmap: [i32; 16],
    /// submap → residue。
    pub residuesubmap: [i32; 16],
    pub coupling_steps: i32,
    pub coupling_mag: [i32; 256],
    pub coupling_ang: [i32; 256],
}

/// `vorbis_info_floor1`（`backends.h`）。`class_book`・`class_subbook` はテンプレートの中では floor の book の
/// 並び（`floor_books[x]`）の添字。`vorbis_encode_floor_setup` が全体の book の番号に足し直す。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VorbisInfoFloor1 {
    /// 0〜31。
    pub partitions: i32,
    /// 0〜15。
    pub partitionclass: [i32; VIF_PARTS],
    /// 1〜8。
    pub class_dim: [i32; VIF_CLASS],
    /// 0〜3（2^n 通り）。
    pub class_subs: [i32; VIF_CLASS],
    pub class_book: [i32; VIF_CLASS],
    /// [class][sub]。-1 は本なし。
    pub class_subbook: [[i32; 8]; VIF_CLASS],
    /// 1〜4。
    pub mult: i32,
    /// 先頭の 2 つ（0 と n）は暗黙。値は重ならない（vorbis-tables のテストで確かめる）。
    pub postlist: [i32; VIF_POSIT + 2],
    // ここから下はエンコーダの解析のための値
    pub maxover: f32,
    pub maxunder: f32,
    pub maxerr: f32,
    pub twofitweight: f32,
    pub twofitatten: f32,
    pub n: i32,
}

/// `vorbis_info_residue0`（`backends.h`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VorbisInfoResidue0 {
    pub begin: i64,
    pub end: i64,
    /// partition の大きさ。
    pub grouping: i32,
    /// class の数。
    pub partitions: i32,
    pub partvals: i32,
    pub groupbook: i32,
    pub secondstages: [i32; 64],
    pub booklist: [i32; 512],
    pub classmetric1: [i32; 64],
    pub classmetric2: [i32; 64],
}

/// `static_bookblock`（`vorbisenc.c`）：class × 段（cascade）の VQ の book。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticBookblock {
    pub books: [[Option<BookId>; 4]; 12],
}

/// `vorbis_residue_template`（`vorbisenc.c`）。`*_managed` はビットレートの管理のときだけ使う（VBR では使わない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VorbisResidueTemplate {
    /// 1（uncoupled）か 2（stereo）。
    pub res_type: i32,
    /// 0：lowpass で制限、1：point stereo で制限、2：LFE。
    pub limit_type: i32,
    pub grouping: i32,
    pub res: &'static VorbisInfoResidue0,
    pub book_aux: BookId,
    pub book_aux_managed: BookId,
    pub books_base: &'static StaticBookblock,
    pub books_base_managed: &'static StaticBookblock,
}

/// `vorbis_mapping_template`（`vorbisenc.c`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VorbisMappingTemplate {
    /// mode ごとの mapping（短・長の 2 つ）。
    pub map: &'static [VorbisInfoMapping0],
    pub res: &'static [VorbisResidueTemplate],
}

/// `vp_adjblock`（`vorbisenc.c`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VpAdjblock {
    pub block: [i32; P_BANDS],
}

/// `compandblock`（`vorbisenc.c`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compandblock {
    pub data: [i32; NOISE_COMPAND_LEVELS],
}

/// `att3`（`vorbisenc.c`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Att3 {
    pub att: [i32; P_NOISECURVES],
    pub boost: f32,
    pub decay: f32,
}

/// `noiseguard`（`vorbisenc.c`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Noiseguard {
    pub lo: i32,
    pub hi: i32,
    pub fixed: i32,
}

/// `noise3`（`vorbisenc.c`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Noise3 {
    pub data: [[i32; 17]; P_NOISECURVES],
}

/// `adj_stereo`（`vorbisenc.c`）。VBR は真ん中の列 `PACKETBLOBS / 2` だけを使う。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdjStereo {
    pub pre: [i32; PACKETBLOBS],
    pub post: [i32; PACKETBLOBS],
    /// C の `kHz`。
    pub khz: [f32; PACKETBLOBS],
    /// C の `lowpasskHz`。
    pub lowpass_khz: [f32; PACKETBLOBS],
}

/// `vorbis_info_psy_global`（`psy.h`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VorbisInfoPsyGlobal {
    pub eighth_octave_lines: i32,
    pub preecho_thresh: [f32; VE_BANDS],
    pub postecho_thresh: [f32; VE_BANDS],
    pub stretch_penalty: f32,
    pub preecho_minenergy: f32,
    pub ampmax_att_per_sec: f32,
    /// C の `coupling_pkHz`。
    pub coupling_pkhz: [i32; PACKETBLOBS],
    pub coupling_pointlimit: [[i32; PACKETBLOBS]; 2],
    pub coupling_prepointamp: [i32; PACKETBLOBS],
    pub coupling_postpointamp: [i32; PACKETBLOBS],
    pub sliding_lowpass: [[i32; PACKETBLOBS]; 2],
}

/// `vorbis_info_psy`（`psy.h`）。テンプレートは `_psy_info_template`（[`crate::PSY_INFO_TEMPLATE`]）だけで、
/// 値は `vorbisenc.c` が品質から埋める。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VorbisInfoPsy {
    pub blockflag: i32,
    pub ath_adjatt: f32,
    pub ath_maxatt: f32,
    pub tone_masteratt: [f32; P_NOISECURVES],
    pub tone_centerboost: f32,
    pub tone_decay: f32,
    pub tone_abs_limit: f32,
    pub toneatt: [f32; P_BANDS],
    pub noisemaskp: i32,
    pub noisemaxsupp: f32,
    pub noisewindowlo: f32,
    pub noisewindowhi: f32,
    pub noisewindowlomin: i32,
    pub noisewindowhimin: i32,
    pub noisewindowfixed: i32,
    pub noiseoff: [[f32; P_BANDS]; P_NOISECURVES],
    pub noisecompand: [f32; NOISE_COMPAND_LEVELS],
    /// aoTuV M5 だけが使う（kura は M5 を移さない）。
    pub noisecompand_high: [f32; NOISE_COMPAND_LEVELS],
    /// この版では読む所がない（vorbisenc.c が compand の補間の端数を入れるだけ。psy.h の注記は aoTuV M2 用）。
    pub flacint: f32,
    /// C の `max_curve_dB`。
    pub max_curve_db: f32,
    pub normal_p: i32,
    pub normal_start: i32,
    pub normal_partition: i32,
    pub normal_thresh: f64,
}

/// `aotuv_preset`（`psy.h`）。`psy.c` の `set_aotuv_psy`（[`crate::SET_AOTUV_PSY`]）の行。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AotuvPreset {
    /// M4（と M3・M9）。
    pub tonecomp_endp: i32,
    /// M4 の閾値。
    pub tonecomp_thres: f32,
    /// M2・M8（kura は移さない）。
    pub min_nn_lp: i32,
    /// M6・M7。
    pub tonefix_end: i32,
}

/// `ve_setup_data_template`（`vorbisenc.c`）。レート × チャンネルの制約ごとのテンプレート。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VeSetupDataTemplate {
    pub mappings: i32,
    pub rate_mapping: &'static [f64],
    pub quality_mapping: &'static [f64],
    /// aoTuV の前増幅（kura は移さない）。
    pub pre_amp: &'static [f32],
    /// 2：stereo、-1：何チャンネルでも（uncoupled）。
    pub coupling_restriction: i32,
    pub samplerate_min_restriction: i64,
    pub samplerate_max_restriction: i64,
    pub blocksize_short: &'static [i32],
    pub blocksize_long: &'static [i32],
    pub psy_tone_masteratt: &'static [Att3],
    /// C の `psy_tone_0dB`。
    pub psy_tone_0db: &'static [i32],
    /// C の `psy_tone_dBsuppress`。
    pub psy_tone_dbsuppress: &'static [i32],
    pub psy_tone_adj_impulse: &'static [VpAdjblock],
    pub psy_tone_adj_long: Option<&'static [VpAdjblock]>,
    pub psy_tone_adj_other: &'static [VpAdjblock],
    pub psy_noiseguards: &'static [Noiseguard],
    pub psy_noise_bias_impulse: &'static [Noise3],
    pub psy_noise_bias_padding: &'static [Noise3],
    pub psy_noise_bias_trans: Option<&'static [Noise3]>,
    pub psy_noise_bias_long: Option<&'static [Noise3]>,
    /// C の `psy_noise_dBsuppress`。
    pub psy_noise_dbsuppress: &'static [i32],
    pub psy_noise_compand: &'static [Compandblock],
    pub psy_noise_compand_short_mapping: &'static [f64],
    pub psy_noise_compand_long_mapping: Option<&'static [f64]>,
    pub psy_noise_normal_start: [&'static [i32]; 2],
    pub psy_noise_normal_partition: [&'static [i32]; 2],
    pub psy_noise_normal_thresh: &'static [f64],
    pub psy_ath_float: &'static [i32],
    pub psy_ath_abs: &'static [i32],
    pub psy_lowpass: &'static [f64],
    pub global_params: &'static [VorbisInfoPsyGlobal],
    pub global_mapping: &'static [f64],
    pub stereo_modes: Option<&'static [AdjStereo]>,
    /// floor ごとの book の並び（C の `_floor_Xx0_books` は NULL 1 つ）。
    pub floor_books: &'static [&'static [Option<BookId>]],
    pub floor_params: &'static [VorbisInfoFloor1],
    pub floor_mappings: i32,
    pub floor_mapping_list: &'static [&'static [i32]],
    pub maps: &'static [VorbisMappingTemplate],
}

/// C の `calloc` と同じ、すべて 0 の値（エンコーダが C の `_ogg_calloc` で作る構造体の初期値）。大きな配列を持つ型は
/// 標準の `derive(Default)` が使えないので手で書く。
macro_rules! zeroed_default {
    ($t:ty { $($f:ident: $v:expr),* $(,)? }) => {
        impl Default for $t {
            fn default() -> Self {
                Self { $($f: $v),* }
            }
        }
    };
}

zeroed_default!(VorbisInfoMapping0 {
    submaps: 0,
    chmuxlist: [0; 256],
    floorsubmap: [0; 16],
    residuesubmap: [0; 16],
    coupling_steps: 0,
    coupling_mag: [0; 256],
    coupling_ang: [0; 256],
});
zeroed_default!(VorbisInfoFloor1 {
    partitions: 0,
    partitionclass: [0; VIF_PARTS],
    class_dim: [0; VIF_CLASS],
    class_subs: [0; VIF_CLASS],
    class_book: [0; VIF_CLASS],
    class_subbook: [[0; 8]; VIF_CLASS],
    mult: 0,
    postlist: [0; VIF_POSIT + 2],
    maxover: 0.0,
    maxunder: 0.0,
    maxerr: 0.0,
    twofitweight: 0.0,
    twofitatten: 0.0,
    n: 0,
});
zeroed_default!(VorbisInfoResidue0 {
    begin: 0,
    end: 0,
    grouping: 0,
    partitions: 0,
    partvals: 0,
    groupbook: 0,
    secondstages: [0; 64],
    booklist: [0; 512],
    classmetric1: [0; 64],
    classmetric2: [0; 64],
});
zeroed_default!(VorbisInfoPsyGlobal {
    eighth_octave_lines: 0,
    preecho_thresh: [0.0; VE_BANDS],
    postecho_thresh: [0.0; VE_BANDS],
    stretch_penalty: 0.0,
    preecho_minenergy: 0.0,
    ampmax_att_per_sec: 0.0,
    coupling_pkhz: [0; PACKETBLOBS],
    coupling_pointlimit: [[0; PACKETBLOBS]; 2],
    coupling_prepointamp: [0; PACKETBLOBS],
    coupling_postpointamp: [0; PACKETBLOBS],
    sliding_lowpass: [[0; PACKETBLOBS]; 2],
});
zeroed_default!(VorbisInfoPsy {
    blockflag: 0,
    ath_adjatt: 0.0,
    ath_maxatt: 0.0,
    tone_masteratt: [0.0; P_NOISECURVES],
    tone_centerboost: 0.0,
    tone_decay: 0.0,
    tone_abs_limit: 0.0,
    toneatt: [0.0; P_BANDS],
    noisemaskp: 0,
    noisemaxsupp: 0.0,
    noisewindowlo: 0.0,
    noisewindowhi: 0.0,
    noisewindowlomin: 0,
    noisewindowhimin: 0,
    noisewindowfixed: 0,
    noiseoff: [[0.0; P_BANDS]; P_NOISECURVES],
    noisecompand: [0.0; NOISE_COMPAND_LEVELS],
    noisecompand_high: [0.0; NOISE_COMPAND_LEVELS],
    flacint: 0.0,
    max_curve_db: 0.0,
    normal_p: 0,
    normal_start: 0,
    normal_partition: 0,
    normal_thresh: 0.0,
});

/// 末尾を 0 で埋めた配列（生成したデータの長い配列を短く書くため。C の初期化子で省いた要素は 0）。
pub(crate) const fn pad_i32<const N: usize>(head: &[i32]) -> [i32; N] {
    let mut out = [0; N];
    let mut i = 0;
    while i < head.len() {
        out[i] = head[i];
        i += 1;
    }
    out
}

/// `pad_i32` の f32 版（埋めるのは +0.0）。
pub(crate) const fn pad_f32<const N: usize>(head: &[f32]) -> [f32; N] {
    let mut out = [0.0; N];
    let mut i = 0;
    while i < head.len() {
        out[i] = head[i];
        i += 1;
    }
    out
}

/// テストで、生成したデータを決まった順のバイト列にする（gen.py の `ser` と同じ規則）：整数・浮動小数のビットは
/// リトルエンディアン、固定の長さの配列は要素だけ、スライスと文字列は u32 の長さを前に、`Option` は 0 か 1 を前に、
/// 参照は指す先の中身、構造体はフィールドの並びのとおり。
#[cfg(test)]
pub(crate) trait Feed {
    fn feed(&self, out: &mut Vec<u8>);
}

#[cfg(test)]
mod feed_impls {
    use super::*;

    impl Feed for i32 {
        fn feed(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.to_le_bytes());
        }
    }
    impl Feed for i64 {
        fn feed(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.to_le_bytes());
        }
    }
    impl Feed for u32 {
        fn feed(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.to_le_bytes());
        }
    }
    impl Feed for f32 {
        fn feed(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.to_bits().to_le_bytes());
        }
    }
    impl Feed for f64 {
        fn feed(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.to_bits().to_le_bytes());
        }
    }
    impl Feed for str {
        fn feed(&self, out: &mut Vec<u8>) {
            (self.len() as u32).feed(out);
            out.extend_from_slice(self.as_bytes());
        }
    }
    impl Feed for BookId {
        fn feed(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.0.to_le_bytes());
        }
    }
    impl<T: Feed, const N: usize> Feed for [T; N] {
        fn feed(&self, out: &mut Vec<u8>) {
            for x in self {
                x.feed(out);
            }
        }
    }
    impl<T: Feed> Feed for [T] {
        fn feed(&self, out: &mut Vec<u8>) {
            (self.len() as u32).feed(out);
            for x in self {
                x.feed(out);
            }
        }
    }
    impl<T: Feed + ?Sized> Feed for &T {
        fn feed(&self, out: &mut Vec<u8>) {
            (**self).feed(out);
        }
    }
    impl<T: Feed> Feed for Option<T> {
        fn feed(&self, out: &mut Vec<u8>) {
            match self {
                None => out.push(0),
                Some(x) => {
                    out.push(1);
                    x.feed(out);
                }
            }
        }
    }

    /// 構造体はフィールドの並びのとおり。
    macro_rules! feed_struct {
        ($t:ty { $($f:ident),* $(,)? }) => {
            impl Feed for $t {
                fn feed(&self, out: &mut Vec<u8>) {
                    $( self.$f.feed(out); )*
                }
            }
        };
    }

    feed_struct!(Book {
        name,
        offset,
        len,
        bits
    });
    feed_struct!(VorbisInfoMode {
        blockflag,
        windowtype,
        transformtype,
        mapping
    });
    feed_struct!(VorbisInfoMapping0 {
        submaps,
        chmuxlist,
        floorsubmap,
        residuesubmap,
        coupling_steps,
        coupling_mag,
        coupling_ang
    });
    feed_struct!(VorbisInfoFloor1 {
        partitions,
        partitionclass,
        class_dim,
        class_subs,
        class_book,
        class_subbook,
        mult,
        postlist,
        maxover,
        maxunder,
        maxerr,
        twofitweight,
        twofitatten,
        n
    });
    feed_struct!(VorbisInfoResidue0 {
        begin,
        end,
        grouping,
        partitions,
        partvals,
        groupbook,
        secondstages,
        booklist,
        classmetric1,
        classmetric2
    });
    feed_struct!(StaticBookblock { books });
    feed_struct!(VorbisResidueTemplate {
        res_type,
        limit_type,
        grouping,
        res,
        book_aux,
        book_aux_managed,
        books_base,
        books_base_managed
    });
    feed_struct!(VorbisMappingTemplate { map, res });
    feed_struct!(VpAdjblock { block });
    feed_struct!(Compandblock { data });
    feed_struct!(Att3 { att, boost, decay });
    feed_struct!(Noiseguard { lo, hi, fixed });
    feed_struct!(Noise3 { data });
    feed_struct!(AdjStereo {
        pre,
        post,
        khz,
        lowpass_khz
    });
    feed_struct!(VorbisInfoPsyGlobal {
        eighth_octave_lines,
        preecho_thresh,
        postecho_thresh,
        stretch_penalty,
        preecho_minenergy,
        ampmax_att_per_sec,
        coupling_pkhz,
        coupling_pointlimit,
        coupling_prepointamp,
        coupling_postpointamp,
        sliding_lowpass
    });
    feed_struct!(VorbisInfoPsy {
        blockflag,
        ath_adjatt,
        ath_maxatt,
        tone_masteratt,
        tone_centerboost,
        tone_decay,
        tone_abs_limit,
        toneatt,
        noisemaskp,
        noisemaxsupp,
        noisewindowlo,
        noisewindowhi,
        noisewindowlomin,
        noisewindowhimin,
        noisewindowfixed,
        noiseoff,
        noisecompand,
        noisecompand_high,
        flacint,
        max_curve_db,
        normal_p,
        normal_start,
        normal_partition,
        normal_thresh
    });
    feed_struct!(AotuvPreset {
        tonecomp_endp,
        tonecomp_thres,
        min_nn_lp,
        tonefix_end
    });
    feed_struct!(VeSetupDataTemplate {
        mappings,
        rate_mapping,
        quality_mapping,
        pre_amp,
        coupling_restriction,
        samplerate_min_restriction,
        samplerate_max_restriction,
        blocksize_short,
        blocksize_long,
        psy_tone_masteratt,
        psy_tone_0db,
        psy_tone_dbsuppress,
        psy_tone_adj_impulse,
        psy_tone_adj_long,
        psy_tone_adj_other,
        psy_noiseguards,
        psy_noise_bias_impulse,
        psy_noise_bias_padding,
        psy_noise_bias_trans,
        psy_noise_bias_long,
        psy_noise_dbsuppress,
        psy_noise_compand,
        psy_noise_compand_short_mapping,
        psy_noise_compand_long_mapping,
        psy_noise_normal_start,
        psy_noise_normal_partition,
        psy_noise_normal_thresh,
        psy_ath_float,
        psy_ath_abs,
        psy_lowpass,
        global_params,
        global_mapping,
        stereo_modes,
        floor_books,
        floor_params,
        floor_mappings,
        floor_mapping_list,
        maps
    });
}
