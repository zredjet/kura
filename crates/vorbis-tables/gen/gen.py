#!/usr/bin/env python3
"""vorbis-tables の生成物（src/data.rs・src/books.rs・src/books.bin）を aoTuV の C のヘッダから作る。

使い方（リポジトリのどこからでも）:
    python3 -I crates/vorbis-tables/gen/gen.py [--src <aotuv_lancer_vorbis_sys-0.1.6 の場所>]
    python3 -I crates/vorbis-tables/gen/gen.py --write-hashes   # 入力の SHA-256 の一覧を作り直す（版を上げたときだけ）

手順（README.md も見る）:
1. 入力のファイルの SHA-256 を gen/inputs.sha256 と照らす（違えば止まる）。
2. 一時ディレクトリに、vorbisenc.c の先頭（setup_list まで）・floor1.c・psy.c・envelope.c の static の表を
   文字列のまま切り出し（改名だけ）、すべての static の定義の名前の表（symtab.inc）を作る。
3. gen/dump.c を cc で組み（-U__SSE__ で SSE でない経路。libvorbis の codebook.c・sharedbook.c と libogg の
   bitwise.c を一緒に組んで vorbis_staticbook_pack で codebook を詰める）、走らせて JSON を得る。
4. JSON から Rust の生成物を書く。codebook は C が詰めたバイト列を books.bin にし、ここでも同じ規則で詰め直して
   一致を確かめる。浮動小数は最短の 10 進で書き、その 10 進が正しく丸めて同じビットになることを有理数で確かめる。
5. 生成したデータを決まった順のバイト列にした SHA-256 を data.rs に書く（Rust のテストが同じバイト列を作って照らす）。

標準ライブラリだけを使う。生成物は決定的（同じ入力なら同じバイト列）。
"""

import argparse
import glob
import hashlib
import json
import os
import re
import struct
import subprocess
import sys
import tempfile
from decimal import Decimal
from fractions import Fraction

GEN = os.path.dirname(os.path.abspath(__file__))
KEEP = None
CRATE = os.path.dirname(GEN)
CRATE_VERSION = "aotuv_lancer_vorbis_sys-0.1.6"
# 源の入手先（cargo の置き場所になければ、これを空のディレクトリに展開して --src で渡す。README.md）
CRATE_URL = "https://crates.io/api/v1/crates/aotuv_lancer_vorbis_sys/0.1.6/download"

# 生成に使う入力（aotuv_lancer_vorbis_sys の直下からの相対の道）。dump.c が #include・リンクするものをすべて挙げる。
INPUTS = [
    "vorbis_vendor/COPYING",
    "vorbis_vendor/include/vorbis/codec.h",
    "vorbis_vendor/include/vorbis/vorbisenc.h",
    "vorbis_vendor/lib/backends.h",
    "vorbis_vendor/lib/bitrate.h",
    "vorbis_vendor/lib/codebook.c",
    "vorbis_vendor/lib/codebook.h",
    "vorbis_vendor/lib/codec_internal.h",
    "vorbis_vendor/lib/envelope.c",
    "vorbis_vendor/lib/envelope.h",
    "vorbis_vendor/lib/floor1.c",
    "vorbis_vendor/lib/highlevel.h",
    "vorbis_vendor/lib/masking.h",
    "vorbis_vendor/lib/mdct.h",
    "vorbis_vendor/lib/misc.h",
    "vorbis_vendor/lib/os.h",
    "vorbis_vendor/lib/psy.c",
    "vorbis_vendor/lib/psy.h",
    "vorbis_vendor/lib/scales.h",
    "vorbis_vendor/lib/sharedbook.c",
    "vorbis_vendor/lib/smallft.h",
    "vorbis_vendor/lib/vorbisenc.c",
    "vorbis_vendor/lib/window.c",
    "vorbis_vendor/lib/window.h",
    "vorbis_vendor/lib/books/coupled/res_books_51.h",
    "vorbis_vendor/lib/books/coupled/res_books_stereo.h",
    "vorbis_vendor/lib/books/floor/floor_books.h",
    "vorbis_vendor/lib/books/uncoupled/res_books_uncoupled.h",
    "vorbis_vendor/lib/modes/floor_all.h",
    "vorbis_vendor/lib/modes/psych_11.h",
    "vorbis_vendor/lib/modes/psych_16.h",
    "vorbis_vendor/lib/modes/psych_44.h",
    "vorbis_vendor/lib/modes/psych_8.h",
    "vorbis_vendor/lib/modes/residue_16.h",
    "vorbis_vendor/lib/modes/residue_44.h",
    "vorbis_vendor/lib/modes/residue_44p51.h",
    "vorbis_vendor/lib/modes/residue_44u.h",
    "vorbis_vendor/lib/modes/residue_8.h",
    "vorbis_vendor/lib/modes/setup_11.h",
    "vorbis_vendor/lib/modes/setup_16.h",
    "vorbis_vendor/lib/modes/setup_22.h",
    "vorbis_vendor/lib/modes/setup_32.h",
    "vorbis_vendor/lib/modes/setup_44.h",
    "vorbis_vendor/lib/modes/setup_44p51.h",
    "vorbis_vendor/lib/modes/setup_44u.h",
    "vorbis_vendor/lib/modes/setup_8.h",
    "vorbis_vendor/lib/modes/setup_X.h",
    "ogg_vendor/include/ogg/config_types.h",
    "ogg_vendor/include/ogg/ogg.h",
    "ogg_vendor/include/ogg/os_types.h",
    "ogg_vendor/src/bitwise.c",
]

# 名前の表を作るために static の定義を探すファイル（この順に BookId・static の並びが決まる）
SCAN = [
    "lib/masking.h",
    "lib/window.c",
    "lib/modes/floor_all.h",
    "lib/modes/psych_44.h",
    "lib/modes/psych_16.h",
    "lib/modes/psych_11.h",
    "lib/modes/psych_8.h",
    "lib/modes/residue_44.h",
    "lib/modes/residue_44u.h",
    "lib/modes/residue_44p51.h",
    "lib/modes/residue_16.h",
    "lib/modes/residue_8.h",
    "lib/modes/setup_44.h",
    "lib/modes/setup_44u.h",
    "lib/modes/setup_44p51.h",
    "lib/modes/setup_32.h",
    "lib/modes/setup_8.h",
    "lib/modes/setup_11.h",
    "lib/modes/setup_16.h",
    "lib/modes/setup_22.h",
    "lib/modes/setup_X.h",
    "lib/books/floor/floor_books.h",
    "lib/books/coupled/res_books_stereo.h",
    "lib/books/uncoupled/res_books_uncoupled.h",
    "lib/books/coupled/res_books_51.h",
]

# C のファイルから切り出す static の表：（ファイル, C の名前, 生成の中での名前）
EXTRACT = [
    ("lib/floor1.c", "FLOOR1_fromdB_LOOKUP", "FLOOR1_fromdB_LOOKUP__floor1"),
    ("lib/psy.c", "FLOOR1_fromdB_LOOKUP", "FLOOR1_fromdB_LOOKUP__psy"),
    ("lib/psy.c", "stereo_threshholds", "stereo_threshholds"),
    ("lib/psy.c", "stereo_threshholds_X", "stereo_threshholds_X"),
    ("lib/psy.c", "set_aotuv_psy", "set_aotuv_psy"),
    ("lib/envelope.c", "band_begin", "band_begin"),
    ("lib/envelope.c", "band_end", "band_end"),
    # aoTuV の M3（m3n*・freq_bfn*）・M9 の前の compander（stn_compand）・M7（ntfix_offset）
    ("lib/psy.c", "m3n32", "m3n32"),
    ("lib/psy.c", "m3n44", "m3n44"),
    ("lib/psy.c", "m3n48", "m3n48"),
    ("lib/psy.c", "m3n32x2", "m3n32x2"),
    ("lib/psy.c", "m3n44x2", "m3n44x2"),
    ("lib/psy.c", "m3n48x2", "m3n48x2"),
    ("lib/psy.c", "freq_bfn128", "freq_bfn128"),
    ("lib/psy.c", "freq_bfn256", "freq_bfn256"),
    ("lib/psy.c", "stn_compand", "stn_compand"),
    ("lib/psy.c", "ntfix_offset", "ntfix_offset"),
]

# 切り出す表の大きさに使うマクロ：（ファイル, マクロの名前）。`#define 名前 値` の行をそのまま写す
EXTRACT_DEFINES = [
    ("lib/psy.c", "M3C"),
]


def die(msg):
    sys.stderr.write("gen.py: " + msg + "\n")
    sys.exit(1)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        h.update(f.read())
    return h.hexdigest()


def cargo_homes():
    """源を探す cargo の置き場所：環境変数 CARGO_HOME（あれば先に）と ~/.cargo。同じものは 1 回だけ。"""
    homes = []
    env = os.environ.get("CARGO_HOME")
    if env:
        homes.append(os.path.abspath(os.path.expanduser(env)))
    default = os.path.join(os.path.expanduser("~"), ".cargo")
    if default not in homes:
        homes.append(default)
    return homes


def find_src(arg):
    if arg:
        return os.path.abspath(arg)
    homes = cargo_homes()
    for home in homes:
        hits = sorted(glob.glob(os.path.join(home, "registry", "src", "*", CRATE_VERSION)))
        if hits:
            return hits[0]
    where = "・".join(os.path.join(h, "registry", "src") for h in homes)
    die(
        f"{CRATE_VERSION} が {where} にありません。このクレートは kura の依存ではないので cargo fetch では落ちない。"
        f"crates.io の {CRATE_URL}（tar.gz の .crate）を空のディレクトリに展開し、"
        f"できた {CRATE_VERSION}/ を --src で渡す（中身は inputs.sha256 で照らす。README.md）"
    )


# ---- C の文字列の処理 --------------------------------------------------------------

def strip_comments(text):
    """/* */ と // の注釈を消す（文字列の中の // は表のファイルにはないので気にしない）。行の数は保つ。"""
    text = re.sub(r"/\*.*?\*/", lambda m: "\n" * m.group(0).count("\n"), text, flags=re.S)
    return re.sub(r"//[^\n]*", "", text)


DEF_RE = re.compile(
    r"^static\s+(?:_MM_ALIGN16\s+)?(?:const\s+)?[A-Za-z_][\w\s\*]*?\b([A-Za-z_]\w*)\s*((?:\[[^\]]*\]\s*)*)=",
    re.M,
)


def definitions(text):
    """static の定義の（名前, 配列か）を出てくる順に返す。"""
    out = []
    for m in DEF_RE.finditer(strip_comments(text)):
        out.append((m.group(1), bool(m.group(2).strip())))
    return out


def extract_static(text, name):
    """`static ... name[...] = { ... };` の文字列をそのまま返す（括弧を数えて終わりを探す）。"""
    m = re.search(r"^static[^;{}]*?\b" + re.escape(name) + r"\s*(?:\[[^\]]*\]\s*)*=\s*\{", text, re.M)
    if not m:
        die(f"{name} の定義が見つかりません")
    depth = 0
    i = m.end() - 1
    while i < len(text):
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                end = text.index(";", i) + 1
                return text[m.start():end]
        i += 1
    die(f"{name} の終わりが見つかりません")


def vorbisenc_head(text):
    """vorbisenc.c の先頭から setup_list の定義の終わりまで。"""
    m = re.search(r"static const ve_setup_data_template \*const setup_list\[\]=\{.*?\};", text, re.S)
    if not m:
        die("vorbisenc.c の setup_list が見つかりません")
    return text[: m.end()] + "\n"


# ---- 浮動小数の 10 進 ------------------------------------------------------------

def f32_bits_of_fraction(q):
    """有理数 q を f32 に正しく丸めた（最近接・偶数）ビット列。"""
    if q == 0:
        return 0
    sign = 0x80000000 if q < 0 else 0
    a = abs(q)
    e = a.numerator.bit_length() - a.denominator.bit_length()
    if Fraction(2) ** e > a:
        e -= 1
    while Fraction(2) ** (e + 1) <= a:
        e += 1
    if e < -126:
        m = a / Fraction(2) ** -149
        expo = 0
    else:
        m = a / Fraction(2) ** (e - 23)
        expo = e + 127
    fl = m.numerator // m.denominator
    rem = m - fl
    if rem > Fraction(1, 2) or (rem == Fraction(1, 2) and fl % 2 == 1):
        fl += 1
    if expo == 0:
        if fl >= 1 << 23:  # 非正規数から正規数へ繰り上がった
            return sign | (1 << 23) | (fl - (1 << 23))
        return sign | fl
    if fl == 1 << 24:
        fl = 1 << 23
        expo += 1
    if expo >= 255:
        die("f32 があふれる")
    return sign | (expo << 23) | (fl - (1 << 23))


def rust_literal(s):
    """10 進の文字列を Rust の浮動小数の字句にする（指数が -5〜15 なら位取りで書く。値は変えない）。"""
    if "inf" in s or "nan" in s:
        die(f"有限でない値 {s}")
    d = Decimal(s)
    if d != 0 and -5 <= d.adjusted() < 16:
        s = format(d, "f")
    if "." not in s and "e" not in s:
        s += ".0"
    if Decimal(s) != d:
        die(f"10 進の書き直しで値が変わった {s}")
    return s


def f32_literal(bits):
    (x,) = struct.unpack("<f", struct.pack("<I", bits))
    if bits == 0x80000000:
        return "-0.0"
    for p in range(1, 10):
        s = "%.*g" % (p, x)
        if f32_bits_of_fraction(Fraction(s)) == bits:
            return rust_literal(s)
    die(f"f32 0x{bits:08x} の 10 進が見つかりません")


def f64_literal(bits):
    (x,) = struct.unpack("<d", struct.pack("<Q", bits))
    s = repr(x)
    if struct.unpack("<Q", struct.pack("<d", float(s)))[0] != bits:
        die(f"f64 0x{bits:016x} の 10 進が戻らない")
    return rust_literal(s)


# ---- 型の記述（Rust の型・書き方・決まった順のバイト列） -----------------------------------

class Prim:
    def __init__(self, rust, fmt, ser):
        self.rust = rust
        self.fmt = fmt
        self.ser = ser


def _i32(v):
    return struct.pack("<i", v)


I32 = Prim("i32", lambda v: str(v), _i32)
I64 = Prim("i64", lambda v: str(v), lambda v: struct.pack("<q", v))
F32 = Prim("f32", lambda v: f32_literal(int(v, 16)), lambda v: struct.pack("<I", int(v, 16)))
F64 = Prim("f64", lambda v: f64_literal(int(v, 16)), lambda v: struct.pack("<Q", int(v, 16)))


class BookRef:
    rust = "BookId"


class Arr:
    def __init__(self, elem, n):
        self.elem = elem
        self.n = n
        self.rust = f"[{rust_type(elem)}; {n}]"


class Slice:
    """`&'static [T]`：中身は別の static の名前で持つ。"""

    def __init__(self, elem):
        self.elem = elem
        self.rust = f"&'static [{rust_type(elem)}]"


class Ref:
    """`&'static T`（構造体 1 つ）：別の static の名前で持つ。"""

    def __init__(self, elem):
        self.elem = elem
        self.rust = f"&'static {rust_type(elem)}"


class Opt:
    def __init__(self, inner):
        self.inner = inner
        self.rust = f"Option<{rust_type(inner)}>"


class Struct:
    def __init__(self, name, fields):
        self.name = name
        self.rust = name
        self.fields = fields  # [(rust の名前, json の名前, 型)]


def rust_type(t):
    return t.rust


ATT3 = Struct("Att3", [("att", "att", Arr(I32, 3)), ("boost", "boost", F32), ("decay", "decay", F32)])
VPADJ = Struct("VpAdjblock", [("block", "block", Arr(I32, 17))])
NOISEGUARD = Struct("Noiseguard", [("lo", "lo", I32), ("hi", "hi", I32), ("fixed", "fixed", I32)])
NOISE3 = Struct("Noise3", [("data", "data", Arr(Arr(I32, 17), 3))])
COMPAND = Struct("Compandblock", [("data", "data", Arr(I32, 40))])
ADJSTEREO = Struct(
    "AdjStereo",
    [
        ("pre", "pre", Arr(I32, 15)),
        ("post", "post", Arr(I32, 15)),
        ("khz", "kHz", Arr(F32, 15)),
        ("lowpass_khz", "lowpasskHz", Arr(F32, 15)),
    ],
)
PSYGLOBAL = Struct(
    "VorbisInfoPsyGlobal",
    [
        ("eighth_octave_lines", "eighth_octave_lines", I32),
        ("preecho_thresh", "preecho_thresh", Arr(F32, 12)),
        ("postecho_thresh", "postecho_thresh", Arr(F32, 12)),
        ("stretch_penalty", "stretch_penalty", F32),
        ("preecho_minenergy", "preecho_minenergy", F32),
        ("ampmax_att_per_sec", "ampmax_att_per_sec", F32),
        ("coupling_pkhz", "coupling_pkHz", Arr(I32, 15)),
        ("coupling_pointlimit", "coupling_pointlimit", Arr(Arr(I32, 15), 2)),
        ("coupling_prepointamp", "coupling_prepointamp", Arr(I32, 15)),
        ("coupling_postpointamp", "coupling_postpointamp", Arr(I32, 15)),
        ("sliding_lowpass", "sliding_lowpass", Arr(Arr(I32, 15), 2)),
    ],
)
FLOOR1 = Struct(
    "VorbisInfoFloor1",
    [
        ("partitions", "partitions", I32),
        ("partitionclass", "partitionclass", Arr(I32, 31)),
        ("class_dim", "class_dim", Arr(I32, 16)),
        ("class_subs", "class_subs", Arr(I32, 16)),
        ("class_book", "class_book", Arr(I32, 16)),
        ("class_subbook", "class_subbook", Arr(Arr(I32, 8), 16)),
        ("mult", "mult", I32),
        ("postlist", "postlist", Arr(I32, 65)),
        ("maxover", "maxover", F32),
        ("maxunder", "maxunder", F32),
        ("maxerr", "maxerr", F32),
        ("twofitweight", "twofitweight", F32),
        ("twofitatten", "twofitatten", F32),
        ("n", "n", I32),
    ],
)
RESIDUE0 = Struct(
    "VorbisInfoResidue0",
    [
        ("begin", "begin", I64),
        ("end", "end", I64),
        ("grouping", "grouping", I32),
        ("partitions", "partitions", I32),
        ("partvals", "partvals", I32),
        ("groupbook", "groupbook", I32),
        ("secondstages", "secondstages", Arr(I32, 64)),
        ("booklist", "booklist", Arr(I32, 512)),
        ("classmetric1", "classmetric1", Arr(I32, 64)),
        ("classmetric2", "classmetric2", Arr(I32, 64)),
    ],
)
BOOKBLOCK = Struct("StaticBookblock", [("books", "books", Arr(Arr(Opt(BookRef()), 4), 12))])
RESTEMPLATE = Struct(
    "VorbisResidueTemplate",
    [
        ("res_type", "res_type", I32),
        ("limit_type", "limit_type", I32),
        ("grouping", "grouping", I32),
        ("res", "res", Ref(RESIDUE0)),
        ("book_aux", "book_aux", BookRef()),
        ("book_aux_managed", "book_aux_managed", BookRef()),
        ("books_base", "books_base", Ref(BOOKBLOCK)),
        ("books_base_managed", "books_base_managed", Ref(BOOKBLOCK)),
    ],
)
MAPPING0 = Struct(
    "VorbisInfoMapping0",
    [
        ("submaps", "submaps", I32),
        ("chmuxlist", "chmuxlist", Arr(I32, 256)),
        ("floorsubmap", "floorsubmap", Arr(I32, 16)),
        ("residuesubmap", "residuesubmap", Arr(I32, 16)),
        ("coupling_steps", "coupling_steps", I32),
        ("coupling_mag", "coupling_mag", Arr(I32, 256)),
        ("coupling_ang", "coupling_ang", Arr(I32, 256)),
    ],
)
MAPTEMPLATE = Struct("VorbisMappingTemplate", [("map", "map", Slice(MAPPING0)), ("res", "res", Slice(RESTEMPLATE))])
PSY = Struct(
    "VorbisInfoPsy",
    [
        ("blockflag", "blockflag", I32),
        ("ath_adjatt", "ath_adjatt", F32),
        ("ath_maxatt", "ath_maxatt", F32),
        ("tone_masteratt", "tone_masteratt", Arr(F32, 3)),
        ("tone_centerboost", "tone_centerboost", F32),
        ("tone_decay", "tone_decay", F32),
        ("tone_abs_limit", "tone_abs_limit", F32),
        ("toneatt", "toneatt", Arr(F32, 17)),
        ("noisemaskp", "noisemaskp", I32),
        ("noisemaxsupp", "noisemaxsupp", F32),
        ("noisewindowlo", "noisewindowlo", F32),
        ("noisewindowhi", "noisewindowhi", F32),
        ("noisewindowlomin", "noisewindowlomin", I32),
        ("noisewindowhimin", "noisewindowhimin", I32),
        ("noisewindowfixed", "noisewindowfixed", I32),
        ("noiseoff", "noiseoff", Arr(Arr(F32, 17), 3)),
        ("noisecompand", "noisecompand", Arr(F32, 40)),
        ("noisecompand_high", "noisecompand_high", Arr(F32, 40)),
        ("flacint", "flacint", F32),
        ("max_curve_db", "max_curve_dB", F32),
        ("normal_p", "normal_p", I32),
        ("normal_start", "normal_start", I32),
        ("normal_partition", "normal_partition", I32),
        ("normal_thresh", "normal_thresh", F64),
    ],
)
MODE = Struct(
    "VorbisInfoMode",
    [
        ("blockflag", "blockflag", I32),
        ("windowtype", "windowtype", I32),
        ("transformtype", "transformtype", I32),
        ("mapping", "mapping", I32),
    ],
)
AOTUVPRESET = Struct(
    "AotuvPreset",
    [
        ("tonecomp_endp", "tonecomp_endp", I32),
        ("tonecomp_thres", "tonecomp_thres", F32),
        ("min_nn_lp", "min_nn_lp", I32),
        ("tonefix_end", "tonefix_end", I32),
    ],
)

KIND_ELEM = {
    "f32": F32,
    "f64": F64,
    "i32": I32,
    "Att3": ATT3,
    "VpAdjblock": VPADJ,
    "Noiseguard": NOISEGUARD,
    "Noise3": NOISE3,
    "Compandblock": COMPAND,
    "AdjStereo": ADJSTEREO,
    "VorbisInfoPsyGlobal": PSYGLOBAL,
    "VorbisInfoFloor1": FLOOR1,
    "VorbisInfoResidue0": RESIDUE0,
    "StaticBookblock": BOOKBLOCK,
    "VorbisResidueTemplate": RESTEMPLATE,
    "VorbisMappingTemplate": MAPTEMPLATE,
    "VorbisInfoMapping0": MAPPING0,
    "BookList": Opt(BookRef()),  # _floor_Xx0_books は {0}（NULL 1 つ）
    "VorbisInfoPsy": PSY,
    "VorbisInfoMode": MODE,
    "AotuvPreset": AOTUVPRESET,
}

# テンプレート（ve_setup_data_template）。NULL がありうる欄は Option。
TEMPLATE_FIELDS = [
    ("mappings", "mappings", I32),
    ("rate_mapping", "rate_mapping", Slice(F64)),
    ("quality_mapping", "quality_mapping", Slice(F64)),
    ("pre_amp", "pre_amp", Slice(F32)),
    ("coupling_restriction", "coupling_restriction", I32),
    ("samplerate_min_restriction", "samplerate_min_restriction", I64),
    ("samplerate_max_restriction", "samplerate_max_restriction", I64),
    ("blocksize_short", "blocksize_short", Slice(I32)),
    ("blocksize_long", "blocksize_long", Slice(I32)),
    ("psy_tone_masteratt", "psy_tone_masteratt", Slice(ATT3)),
    ("psy_tone_0db", "psy_tone_0dB", Slice(I32)),
    ("psy_tone_dbsuppress", "psy_tone_dBsuppress", Slice(I32)),
    ("psy_tone_adj_impulse", "psy_tone_adj_impulse", Slice(VPADJ)),
    ("psy_tone_adj_long", "psy_tone_adj_long", Opt(Slice(VPADJ))),
    ("psy_tone_adj_other", "psy_tone_adj_other", Slice(VPADJ)),
    ("psy_noiseguards", "psy_noiseguards", Slice(NOISEGUARD)),
    ("psy_noise_bias_impulse", "psy_noise_bias_impulse", Slice(NOISE3)),
    ("psy_noise_bias_padding", "psy_noise_bias_padding", Slice(NOISE3)),
    ("psy_noise_bias_trans", "psy_noise_bias_trans", Opt(Slice(NOISE3))),
    ("psy_noise_bias_long", "psy_noise_bias_long", Opt(Slice(NOISE3))),
    ("psy_noise_dbsuppress", "psy_noise_dBsuppress", Slice(I32)),
    ("psy_noise_compand", "psy_noise_compand", Slice(COMPAND)),
    ("psy_noise_compand_short_mapping", "psy_noise_compand_short_mapping", Slice(F64)),
    ("psy_noise_compand_long_mapping", "psy_noise_compand_long_mapping", Opt(Slice(F64))),
    ("psy_noise_normal_start", "psy_noise_normal_start", Arr(Slice(I32), 2)),
    ("psy_noise_normal_partition", "psy_noise_normal_partition", Arr(Slice(I32), 2)),
    ("psy_noise_normal_thresh", "psy_noise_normal_thresh", Slice(F64)),
    ("psy_ath_float", "psy_ath_float", Slice(I32)),
    ("psy_ath_abs", "psy_ath_abs", Slice(I32)),
    ("psy_lowpass", "psy_lowpass", Slice(F64)),
    ("global_params", "global_params", Slice(PSYGLOBAL)),
    ("global_mapping", "global_mapping", Slice(F64)),
    ("stereo_modes", "stereo_modes", Opt(Slice(ADJSTEREO))),
    ("floor_books", "floor_books", Slice(Slice(Opt(BookRef())))),
    ("floor_params", "floor_params", Slice(FLOOR1)),
    ("floor_mappings", "floor_mappings", I32),
    ("floor_mapping_list", "floor_mapping_list", Slice(Slice(I32))),
    ("maps", "maps", Slice(MAPTEMPLATE)),
]
TEMPLATE = Struct("VeSetupDataTemplate", TEMPLATE_FIELDS)


def rust_name(c_name):
    n = c_name.lstrip("_").upper()
    if not n or n[0].isdigit():
        die(f"Rust の名前にできない {c_name}")
    return n


class Gen:
    def __init__(self, dump, symtab_order):
        self.dump = dump
        self.syms = dump["symbols"]
        self.order = symtab_order  # C の名前 → 定義の順
        self.books = sorted(
            (n for n, s in self.syms.items() if s["kind"] == "Book"), key=lambda n: self.order[n]
        )
        self.book_id = {n: i for i, n in enumerate(self.books)}
        self.rust_names = {}
        for n, s in self.syms.items():
            if s["kind"] == "Book":
                continue
            r = rust_name(n.replace("__floor1", ""))
            if r in self.rust_names.values():
                die(f"Rust の名前が重なる {r}")
            self.rust_names[n] = r
        for t in dump["templates"]:
            self.rust_names[t["name"]] = rust_name(t["name"])

    # 値を Rust の式に
    def expr(self, t, v):
        if isinstance(t, Prim):
            return t.fmt(v)
        if isinstance(t, BookRef):
            return f"BookId({self.book_id[v]}) /* {v} */"
        if isinstance(t, Opt):
            return "None" if v is None else f"Some({self.expr(t.inner, v)})"
        if isinstance(t, (Slice, Ref)):
            if v is None:
                die("NULL を Option でない欄に入れようとした")
            if isinstance(t, Slice) and not self.syms[v]["array"] and self.syms[v]["kind"] not in (
                "BookListList",
                "IntArrayList",
            ):
                die(f"{v} は配列でないのにスライスで指している")
            if isinstance(t, Ref) and self.syms[v]["array"]:
                die(f"{v} は配列なのに 1 つの構造体として指している")
            return "&" + self.rust_names[v]
        if isinstance(t, Arr):
            if len(v) != t.n:
                die(f"配列の長さ {len(v)} != {t.n}")
            if isinstance(t.elem, Prim) and t.elem in (I32, F32) and t.n >= 32:
                zero = 0 if t.elem is I32 else "0x00000000"
                k = len(v)
                while k > 0 and v[k - 1] == zero:
                    k -= 1
                if t.n - k >= 8:
                    helper = "pad_i32" if t.elem is I32 else "pad_f32"
                    return f"{helper}(&[{', '.join(self.expr(t.elem, x) for x in v[:k])}])"
            return "[" + ", ".join(self.expr(t.elem, x) for x in v) + "]"
        if isinstance(t, Struct):
            parts = [f"{rf}: {self.expr(ft, v[jf])}" for rf, jf, ft in t.fields]
            return f"{t.name} {{ " + ", ".join(parts) + " }"
        die("知らない型")

    # 決まった順のバイト列（Rust の Feed と同じ規則）
    def ser(self, t, v):
        if isinstance(t, Prim):
            return t.ser(v)
        if isinstance(t, BookRef):
            return struct.pack("<H", self.book_id[v])
        if isinstance(t, Opt):
            return b"\x00" if v is None else b"\x01" + self.ser(t.inner, v)
        if isinstance(t, Ref):
            s = self.syms[v]
            if s["count"] != 1:
                die(f"{v} は 1 つの構造体ではない")
            return self.ser(t.elem, s["values"][0])
        if isinstance(t, Slice):
            s = self.syms[v]
            return struct.pack("<I", s["count"]) + b"".join(self.ser(t.elem, x) for x in s["values"])
        if isinstance(t, Arr):
            return b"".join(self.ser(t.elem, x) for x in v)
        if isinstance(t, Struct):
            return b"".join(self.ser(ft, v[jf]) for _, jf, ft in t.fields)
        die("知らない型")

    def static_type(self, name):
        s = self.syms[name]
        kind = s["kind"]
        if name == "tonemasks":
            return Arr(Arr(Arr(F32, 56), 6), 17), s["values"][:]
        if kind == "BookListList":
            return Arr(Slice(Opt(BookRef())), s["count"]), s["values"]
        if kind == "IntArrayList":
            return Arr(Slice(I32), s["count"]), s["values"]
        elem = KIND_ELEM[kind]
        if not s["array"]:
            if s["count"] != 1:
                die(f"{name}：配列でないのに {s['count']} 個")
            return elem, s["values"][0]
        return Arr(elem, s["count"]), s["values"]


def nest(values, shape):
    if len(shape) == 1:
        return values
    step = len(values) // shape[0]
    return [nest(values[i * step:(i + 1) * step], shape[1:]) for i in range(shape[0])]


def generate(src, out_dir, build):
    V = os.path.join(src, "vorbis_vendor")
    O = os.path.join(src, "ogg_vendor")

    def read(rel):
        with open(os.path.join(V, rel), encoding="latin-1") as f:
            return f.read()

    # 1. 切り出し
    head = vorbisenc_head(read("lib/vorbisenc.c"))
    with open(os.path.join(build, "vorbisenc_head.inc"), "w", encoding="latin-1") as f:
        f.write(head)
    extracted = []
    for rel, cname, newname in EXTRACT:
        text = extract_static(read(rel), cname)
        text = re.sub(r"\b" + re.escape(cname) + r"\b", newname, text, count=1)
        extracted.append((rel, newname, text))
    defines = []
    for rel, macro in EXTRACT_DEFINES:
        m = re.search(r"^#define\s+" + re.escape(macro) + r"\s+[^\n]*$", read(rel), re.M)
        if not m:
            die(f"{rel} に #define {macro} が見つかりません")
        defines.append((rel, m.group(0)))
    with open(os.path.join(build, "extracted.inc"), "w", encoding="latin-1") as f:
        f.write("/* extracted by gen.py (renamed only) */\n")
        for rel, line in defines:
            f.write(f"/* {rel} */\n{line}\n")
        for rel, _, text in extracted:
            f.write(f"/* {rel} */\n{text}\n")

    # 2. 名前の表
    entries = []  # (C の名前, ファイル, 配列か)
    seen = set()

    def add(name, rel, is_array):
        if name in seen:
            return
        seen.add(name)
        entries.append((name, rel, is_array))

    for name, is_array in definitions(head):
        add(name, "lib/vorbisenc.c", is_array)
    for rel in SCAN:
        for name, is_array in definitions(read(rel)):
            add(name, rel, is_array)
    for rel, newname, text in extracted:
        for name, is_array in definitions(text):
            add(name, rel, is_array)
    with open(os.path.join(build, "symtab.inc"), "w") as f:
        f.write("struct sym { const char *name; const char *file; const void *p; size_t size; int array; };\n")
        f.write("static const struct sym SYMS[] = {\n")
        for name, rel, is_array in entries:
            f.write(f'  {{"{name}", "{rel}", (const void *)&{name}, sizeof({name}), {int(is_array)}}},\n')
        f.write("};\n")
    order = {name: i for i, (name, _, _) in enumerate(entries)}
    arrayness = {name: a for name, _, a in entries}

    # 3. 組んで走らせる
    exe = os.path.join(build, "dump")
    cmd = [
        os.environ.get("CC", "cc"),
        "-std=gnu99",
        "-w",
        "-O0",
        "-U__SSE__",
        "-U__SSE2__",
        "-U__SSE3__",
        "-DHAVE_STDINT_H",
        "-I" + build,
        "-I" + os.path.join(V, "lib"),
        "-I" + os.path.join(V, "include"),
        "-I" + os.path.join(O, "include"),
        os.path.join(GEN, "dump.c"),
        os.path.join(V, "lib", "codebook.c"),
        os.path.join(V, "lib", "sharedbook.c"),
        os.path.join(O, "src", "bitwise.c"),
        "-o",
        exe,
        "-lm",
    ]
    subprocess.run(cmd, check=True)
    raw = subprocess.run([exe], check=True, capture_output=True).stdout
    if KEEP:
        with open(os.path.join(KEEP, "dump.json"), "wb") as f:
            f.write(raw)
    dump = json.loads(raw)
    for name, s in dump["symbols"].items():
        s["array"] = arrayness[name]
    if dump["setup_list_len"] != 19:
        die(f"setup_list の数が {dump['setup_list_len']}（19 を期待）")

    g = Gen(dump, order)

    # 4. codebook：C が詰めたバイト列と、ここで詰め直したものを照らす
    blob = bytearray()
    book_rows = []
    for name in g.books:
        b = g.syms[name]["values"][0]
        packed = bytes.fromhex(b["packed"])
        mine, bits = pack_book(b)
        if mine != packed or bits != b["bits"]:
            die(f"{name}：gen.py の詰め方が vorbis_staticbook_pack と違う")
        if (bits + 7) // 8 != len(packed):
            die(f"{name}：ビット数とバイト数が合わない")
        book_rows.append((name, len(blob), len(packed), bits))
        blob += packed

    # 5. Rust の生成物
    statics = []  # (C の名前, Rust の名前, 型, 値, ファイル)
    for name in sorted(g.syms, key=lambda n: order[n]):
        s = g.syms[name]
        if s["kind"] == "Book":
            continue
        t, v = g.static_type(name)
        if name == "tonemasks":
            v = nest(v, [17, 6, 56])
        statics.append((name, g.rust_names[name], t, v, s["file"]))
    for tpl in dump["templates"]:
        statics.append((tpl["name"], g.rust_names[tpl["name"]], TEMPLATE, tpl, tpl["file"]))

    data_ser = bytearray()
    lines = []
    w = lines.append
    w("// @generated by crates/vorbis-tables/gen/gen.py から。手で書き換えない（README.md の手順で作り直す）。")
    w("//")
    w("// aoTuV - Copyright (c) 2003-2021 Aoyumi / Copyright (c) 2002-2020 Xiph.org Foundation（BSD-3-Clause。LICENSE）。")
    w(f"// 入力：{CRATE_VERSION} の vorbis_vendor（aoTuV beta 6.03）。")
    w("")
    w("use crate::types::*;")
    w("")
    for cname, rname, t, v, rel in statics:
        w(f"/// C の `{cname}`（`{rel}`）。")
        w(f"pub static {rname}: {rust_type(t)} = {g.expr(t, v)};")
        w("")
        data_ser += ser_name(rname) + g.ser(t, v)
    setup = [tpl for tpl in dump["templates"]]
    w("/// vorbisenc.c の `setup_list` から 5.1 の `ve_setup_44_51` を除いたもの（並びは C のまま。先頭から合うものを選ぶ）。")
    w(f"pub static SETUP_LIST: [&VeSetupDataTemplate; {len(setup)}] = [")
    for tpl in setup:
        w(f"    &{g.rust_names[tpl['name']]},")
    w("];")
    w("")
    data_ser += ser_name("SETUP_LIST") + b"".join(g.ser(TEMPLATE, tpl) for tpl in setup)
    # BOOKS も同じバイト列に入れる
    data_ser += ser_name("BOOKS") + b"".join(
        ser_str(n) + struct.pack("<III", off, ln, bits) for n, off, ln, bits in book_rows
    )
    blob_sha = hashlib.sha256(blob).hexdigest()
    data_sha = hashlib.sha256(data_ser).hexdigest()
    w("/// 上のすべての static（並びのまま）と SETUP_LIST・BOOKS を決まった順のバイト列にした SHA-256（gen.py が C の値から")
    w("/// 計算した。テストが Rust の値から同じバイト列を作って照らす）。")
    w("#[cfg(test)]")
    w(f'pub(crate) const DATA_SHA256: &str = "{data_sha}";')
    w("/// books.bin の SHA-256（gen.py が計算した）。")
    w("#[cfg(test)]")
    w(f'pub(crate) const BOOKS_BIN_SHA256: &str = "{blob_sha}";')
    w("")
    w("/// テストが決まった順のバイト列を作るために、すべての static を並びのとおりに渡す。")
    w("#[cfg(test)]")
    w("pub(crate) fn feed_all(out: &mut Vec<u8>) {")
    for _, rname, _, _, _ in statics:
        w(f'    feed_static("{rname}", &{rname}, out);')
    w('    feed_static("SETUP_LIST", &SETUP_LIST, out);')
    w('    feed_static("BOOKS", &crate::books::BOOKS, out);')
    w("}")
    w("")
    w("#[cfg(test)]")
    w("fn feed_static<T: crate::types::Feed + ?Sized>(name: &str, v: &T, out: &mut Vec<u8>) {")
    w("    name.feed(out);")
    w("    v.feed(out);")
    w("}")
    with open(os.path.join(out_dir, "src", "data.rs"), "w") as f:
        f.write("\n".join(lines) + "\n")

    bl = []
    b = bl.append
    b("// @generated by crates/vorbis-tables/gen/gen.py から。手で書き換えない（README.md の手順で作り直す）。")
    b("//")
    b("// aoTuV - Copyright (c) 2003-2021 Aoyumi / Copyright (c) 2002-2020 Xiph.org Foundation（BSD-3-Clause。LICENSE）。")
    b("")
    b("use crate::types::Book;")
    b("")
    b("/// テンプレートから使われるすべての codebook（5.1 だけが使うものは除く）を、libvorbis の `vorbis_staticbook_pack`")
    b("/// で詰めた形で並べたもの（各冊はバイトの境目から始まり、最後のバイトの余りのビットは 0）。")
    b('pub static BOOKS_BIN: &[u8] = include_bytes!("books.bin");')
    b("")
    b("/// codebook の名前（C の記号名）と books.bin の中の範囲。添字が `BookId`。並びは C のヘッダの定義の順。")
    b(f"pub static BOOKS: [Book; {len(book_rows)}] = [")
    for n, off, ln, bits in book_rows:
        b(f'    Book {{ name: "{n}", offset: {off}, len: {ln}, bits: {bits} }},')
    b("];")
    with open(os.path.join(out_dir, "src", "books.rs"), "w") as f:
        f.write("\n".join(bl) + "\n")
    with open(os.path.join(out_dir, "src", "books.bin"), "wb") as f:
        f.write(bytes(blob))

    print(f"templates: {len(setup)}  books: {len(book_rows)} ({len(blob)} bytes)  statics: {len(statics)}")
    print(f"books.bin sha256: {blob_sha}")
    print(f"data sha256:      {data_sha}")


def ser_str(s):
    b = s.encode()
    return struct.pack("<I", len(b)) + b


def ser_name(s):
    return ser_str(s)


# ---- codebook の詰め方（libvorbis の codebook.c の vorbis_staticbook_pack と同じ規則。照らすためだけ） ----------

class Bits:
    def __init__(self):
        self.buf = bytearray()
        self.bit = 0

    def write(self, value, n):
        value &= (1 << n) - 1 if n < 64 else (1 << 64) - 1
        for i in range(n):
            if self.bit % 8 == 0:
                self.buf.append(0)
            if (value >> i) & 1:
                self.buf[-1] |= 1 << (self.bit % 8)
            self.bit += 1


def ilog(v):
    r = 0
    while v:
        r += 1
        v >>= 1
    return r


def maptype1_quantvals(entries, dim):
    vals = 1
    while (vals + 1) ** dim <= entries:
        vals += 1
    return vals


def pack_book(b):
    w = Bits()
    dim, entries, ll = b["dim"], b["entries"], b["lengthlist"]
    w.write(0x564342, 24)
    w.write(dim, 16)
    w.write(entries, 24)
    i = 1
    while i < entries:
        if ll[i - 1] == 0 or ll[i] < ll[i - 1]:
            break
        i += 1
    ordered = i == entries
    if ordered:
        count = 0
        w.write(1, 1)
        w.write(ll[0] - 1, 5)
        for i in range(1, entries):
            this, last = ll[i], ll[i - 1]
            if this > last:
                for _ in range(last, this):
                    w.write(i - count, ilog(entries - count))
                    count = i
        w.write(entries - count, ilog(entries - count))
    else:
        w.write(0, 1)
        if all(x != 0 for x in ll):
            w.write(0, 1)
            for x in ll:
                w.write(x - 1, 5)
        else:
            w.write(1, 1)
            for x in ll:
                if x == 0:
                    w.write(0, 1)
                else:
                    w.write(1, 1)
                    w.write(x - 1, 5)
    w.write(b["maptype"], 4)
    if b["maptype"] in (1, 2):
        w.write(b["q_min"], 32)
        w.write(b["q_delta"], 32)
        w.write(b["q_quant"] - 1, 4)
        w.write(b["q_sequencep"], 1)
        qv = maptype1_quantvals(entries, dim) if b["maptype"] == 1 else entries * dim
        if len(b["quantlist"]) != qv:
            die("quantlist の長さが合わない")
        for q in b["quantlist"]:
            w.write(abs(q), b["q_quant"])
    elif b["maptype"] != 0:
        die("知らない maptype")
    return bytes(w.buf), w.bit


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", help=f"{CRATE_VERSION} のディレクトリ（既定は $CARGO_HOME と ~/.cargo の registry/src/*/ から探す）")
    ap.add_argument("--write-hashes", action="store_true", help="gen/inputs.sha256 を作り直す")
    ap.add_argument("--keep", help="C のダンプ（dump.json）をこのディレクトリに残す（調べるとき）")
    args = ap.parse_args()
    global KEEP
    KEEP = args.keep
    src = find_src(args.src)
    hashes_path = os.path.join(GEN, "inputs.sha256")
    actual = [(sha256_file(os.path.join(src, rel)), rel) for rel in INPUTS]
    if args.write_hashes:
        with open(hashes_path, "w") as f:
            for h, rel in actual:
                f.write(f"{h}  {rel}\n")
        print(f"{hashes_path} を書いた")
        return
    with open(hashes_path) as f:
        expected = [tuple(line.split()) for line in f if line.strip()]
    if expected != actual:
        for (eh, er), (ah, ar) in zip(expected, actual):
            if (eh, er) != (ah, ar):
                sys.stderr.write(f"違う：{er} {eh} != {ar} {ah}\n")
        die("入力が inputs.sha256 と違います（版を上げるなら --write-hashes で作り直し、差を確かめる）")
    with tempfile.TemporaryDirectory(prefix="vorbis-tables-gen-") as build:
        generate(src, CRATE, build)


if __name__ == "__main__":
    main()
