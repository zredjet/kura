#!/usr/bin/env python3
"""照合用のエンコーダ（前増幅だけを切った aoTuV b6.03 の C）を、手を入れていない源から組み直す。

使い方（リポジトリのどこからでも）:
    python3 -I crates/vorbis-tables/oracle/build.py [--src <aotuv_lancer_vorbis_sys-0.1.6 の場所>] [--out <出力先>]
        [--cc clang] [--no-san] [--syslibm] [--allow-unverified-arch]
    python3 -I crates/vorbis-tables/oracle/build.py --write-hashes   # 一覧（inputs.sha256・outputs.sha256）を作り直す

手順（README.md の「組み方」も見る）:
1. 源（crates.io の aotuv_lancer_vorbis_sys 0.1.6。--src がなければ $CARGO_HOME と ~/.cargo の registry/src/*/ から探す。
   ワークスペースの依存ではないので cargo fetch では落ちない：crates.io の .crate を空のディレクトリに展開して --src で渡す）の
   使うファイルの SHA-256 を oracle/inputs.sha256 と照らす（1 つでも違えば止まる）。gen/inputs.sha256 と重なるファイルは、そちらの値とも同じことを確かめる。
2. 出力先の csrc/ に源を写し（並びは源のまま：csrc/vorbis_vendor/lib/… など）、patches/01-abl.diff・
   patches/02-kura-port.diff を順に当て（文脈が 1 行でも合わなければ止まる。ずらしも曖昧な当て方もしない）、
   lib/ の新しいファイル（abl.h・kura_libm.h・kura_port.h・kura_port.c）を足し、codebook の名前の表
   （kura_booknames.h）を books/*/*.h から作る。
3. 照合のテストが読む入力（test/in/ の synth_*.f32。NaN が途中に出るもの）を scripts/mkinput.py で作る。
4. 2・3 で作ったもの（源と違うファイル）の SHA-256 を oracle/outputs.sha256 と照らす。
5. rlibm（Rust の libm =0.2.16 の staticlib）を cargo で組み（target は出力先の rlibm-target/）、C を clang で組んで
   （-ffp-contract=off・-fwrapv）bin/ に置く。最後に、C のオブジェクトが OS の libm の超越関数を参照していないことを nm -u で確かめる。

標準ライブラリだけを使う。出力先の既定は <リポジトリ>/target/vorbis-oracle（git に入らない）。
確かめたのは Apple Silicon（arm64）の macOS と Apple clang だけ（README.md）。
"""

import argparse
import concurrent.futures
import glob
import hashlib
import os
import platform
import re
import shutil
import subprocess
import sys

ORACLE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.dirname(ORACLE)
REPO = os.path.dirname(os.path.dirname(CRATE))
CRATE_VERSION = "aotuv_lancer_vorbis_sys-0.1.6"

# 源から写すファイル（aotuv_lancer_vorbis_sys の直下からの相対の道）。組むソースと、それが #include するヘッダのすべて
# （arm64 で clang -MM で数えた）と、ライセンスの本文。SSE の経路のヘッダ（xmmlib.h）は写さない（x86_64 で組もうとすると
# ここで止まる。README.md の「x86_64」）。
INPUTS = [
    "vorbis_vendor/COPYING",
    "vorbis_vendor/include/vorbis/codec.h",
    "vorbis_vendor/include/vorbis/vorbisenc.h",
    "vorbis_vendor/lib/analysis.c",
    "vorbis_vendor/lib/backends.h",
    "vorbis_vendor/lib/bitrate.c",
    "vorbis_vendor/lib/bitrate.h",
    "vorbis_vendor/lib/block.c",
    "vorbis_vendor/lib/codebook.c",
    "vorbis_vendor/lib/codebook.h",
    "vorbis_vendor/lib/codec_internal.h",
    "vorbis_vendor/lib/envelope.c",
    "vorbis_vendor/lib/envelope.h",
    "vorbis_vendor/lib/floor0.c",
    "vorbis_vendor/lib/floor1.c",
    "vorbis_vendor/lib/highlevel.h",
    "vorbis_vendor/lib/info.c",
    "vorbis_vendor/lib/lookup.c",
    "vorbis_vendor/lib/lookup.h",
    "vorbis_vendor/lib/lookup_data.h",
    "vorbis_vendor/lib/lpc.c",
    "vorbis_vendor/lib/lpc.h",
    "vorbis_vendor/lib/lsp.c",
    "vorbis_vendor/lib/lsp.h",
    "vorbis_vendor/lib/mapping0.c",
    "vorbis_vendor/lib/masking.h",
    "vorbis_vendor/lib/mdct.c",
    "vorbis_vendor/lib/mdct.h",
    "vorbis_vendor/lib/misc.h",
    "vorbis_vendor/lib/os.h",
    "vorbis_vendor/lib/psy.c",
    "vorbis_vendor/lib/psy.h",
    "vorbis_vendor/lib/registry.c",
    "vorbis_vendor/lib/registry.h",
    "vorbis_vendor/lib/res0.c",
    "vorbis_vendor/lib/scales.h",
    "vorbis_vendor/lib/sharedbook.c",
    "vorbis_vendor/lib/smallft.c",
    "vorbis_vendor/lib/smallft.h",
    "vorbis_vendor/lib/synthesis.c",
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
    "ogg_vendor/COPYING",
    "ogg_vendor/include/ogg/config_types.h",
    "ogg_vendor/include/ogg/ogg.h",
    "ogg_vendor/include/ogg/os_types.h",
    "ogg_vendor/src/bitwise.c",
    "ogg_vendor/src/crctable.h",
    "ogg_vendor/src/framing.c",
]

# 当てる差分（順に）。01 は調査で入れた実行時のスイッチ（ABL）と範囲外の読み取りの修正、02 は KURA_PORT の手入れ。
PATCHES = ["patches/01-abl.diff", "patches/02-kura-port.diff"]
# 源にない、新しく足すファイル（lib/ から csrc/vorbis_vendor/lib/ へ）
NEW_FILES = ["abl.h", "kura_libm.h", "kura_port.h", "kura_port.c"]
LIB = "csrc/vorbis_vendor/lib"
BOOKNAMES = LIB + "/kura_booknames.h"

# 照合のテストが読む入力（test/in/）：（名前, rate, channels, 秒, seed）。scripts/mkinput.py の synth で作る
# （作業領域の確かめの入力と同じ作り方：3 秒、seed = rate + channels）。NaN が途中に出る（README.md §8.3）。
TEST_INPUTS = [
    ("synth_32000_1.f32", 32000, 1, 3, 32001),
    ("synth_32000_2.f32", 32000, 2, 3, 32002),
    ("synth_192000_1.f32", 192000, 1, 3, 192001),
]

# 組む C（vorbis_vendor/lib の .c。kura_port.c を含む）と libogg の .c
VSRC = [
    "mdct", "smallft", "block", "envelope", "window", "lsp", "lpc", "analysis", "synthesis", "psy", "info",
    "floor1", "floor0", "res0", "mapping0", "registry", "codebook", "sharedbook", "lookup", "bitrate",
    "vorbisenc", "kura_port",
]
OGGSRC = ["csrc/ogg_vendor/src/bitwise.c", "csrc/ogg_vendor/src/framing.c"]

# C を組む共通のフラグ（照合用のエンコーダ・oracle_setup・確かめの版のすべて）。CODEGEN は値に効くもの：
# -ffp-contract=off・-fno-fast-math：FMA に縮約しない・浮動小数の演算を組み替えない（移植の規則 2・3）。
# -fwrapv：符号付き整数のあふれを 2 の補数の折り返しと定める。極端な振幅（標本の絶対値がおよそ 1e9 以上）で residue の
#   量子化が INT_MIN・INT_MAX に張り付くと res0.c・psy.c の int があふれる（未定義動作）。移植は wrapping で写している
#   （codebook.rs・res0.rs・psy.rs の冒頭）ので、C でもそう定める。-fwrapv がないと -O2 と -O0 で出力が変わる（README.md §7.2）。
CFLAGS = ["-std=gnu99", "-w", "-DKURA_PORT", "-DHAVE_STDINT_H"]
CODEGEN = ["-ffp-contract=off", "-fno-fast-math", "-fwrapv"]

# C のオブジェクトが参照してはいけない OS の libm の超越関数（Mach-O は先頭に _ が付く）
OS_LIBM = re.compile(
    r"^_?(exp|expf|log|logf|log10|log2|pow|powf|sin|sinf|cos|cosf|tan|tanf|atan|atanf|atan2|acos|asin|hypot|"
    r"ldexp|frexp|__sincos_stret|__sincospi_stret|exp2|expm1|log1p|sinh|cosh|tanh)$"
)
MATH_REFS = re.compile(
    r"^_?(kura_(exp|log|pow|atan|sin|cos|acos|hypot|ldexp|frexp)|sqrtf?|floorf?|ceilf?|rintf?|fabsf?|lrintf?)$"
)


def die(msg):
    sys.stderr.write("build.py: " + msg + "\n")
    sys.exit(1)


def say(msg):
    print(msg, flush=True)


def disp(path):
    """表示用の道（リポジトリの中ならリポジトリからの相対）。"""
    rel = os.path.relpath(path, REPO)
    return path if rel.startswith("..") else rel


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        h.update(f.read())
    return h.hexdigest()


CRATE_URL = "https://crates.io/api/v1/crates/aotuv_lancer_vorbis_sys/0.1.6/download"


def cargo_homes():
    """源を探す cargo のホーム（環境変数 CARGO_HOME があればそれを先に、次に ~/.cargo。重なりは除く）。"""
    homes = []
    env = os.environ.get("CARGO_HOME")
    if env:
        homes.append(os.path.abspath(os.path.expanduser(env)))
    homes.append(os.path.join(os.path.expanduser("~"), ".cargo"))
    out = []
    for h in homes:
        if h not in out:
            out.append(h)
    return out


def find_src(arg):
    if arg:
        return os.path.abspath(arg)
    homes = cargo_homes()
    for home in homes:
        hits = sorted(glob.glob(os.path.join(home, "registry", "src", "*", CRATE_VERSION)))
        if hits:
            return hits[0]
    # aotuv_lancer_vorbis_sys はワークスペースのどの依存にもないので、cargo fetch では落ちない（README.md §2）
    where = "・".join(os.path.join(h, "registry", "src") for h in homes)
    die(f"{CRATE_VERSION} が {where} にありません。crates.io の {CRATE_URL}（tar.gz の .crate）を空のディレクトリに"
        f"展開して、できた {CRATE_VERSION} を --src で渡す（中身は inputs.sha256 で照らす。README.md §2）")


def read_hashes(path):
    with open(path) as f:
        return [tuple(line.split()) for line in f if line.strip() and not line.startswith("#")]


def write_hashes(path, rows, head):
    with open(path, "w") as f:
        for line in head:
            f.write("# " + line + "\n")
        for h, rel in rows:
            f.write(f"{h}  {rel}\n")
    say(f"{os.path.relpath(path, REPO)} を書いた（{len(rows)} 件）")


def compare_hashes(expected, actual, name):
    if expected == actual:
        return
    exp = dict((rel, h) for h, rel in expected)
    act = dict((rel, h) for h, rel in actual)
    for rel in sorted(set(exp) | set(act)):
        if exp.get(rel) != act.get(rel):
            sys.stderr.write(f"違う：{rel} 記録 {exp.get(rel)} / 実際 {act.get(rel)}\n")
    die(f"{name} と違います（意図した変更なら --write-hashes で作り直し、差を確かめる）")


# ---- 差分を当てる（unified diff。文脈が完全に合うときだけ当てる） ------------------------------------------


def parse_patch(path):
    """[(相対の道, [(old_start, old_count, new_start, new_count, [(印, 行のバイト列)])])]"""
    with open(path, "rb") as f:
        lines = f.read().splitlines(keepends=True)
    files = []
    i = 0
    hunk_re = re.compile(rb"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
    while i < len(lines):
        line = lines[i]
        if line.startswith(b"--- "):
            if i + 1 >= len(lines) or not lines[i + 1].startswith(b"+++ "):
                die(f"{path}: --- の次が +++ でない（{i + 1} 行目）")
            new = lines[i + 1][4:].rstrip(b"\r\n").split(b"\t")[0].decode()
            if not new.startswith("b/"):
                die(f"{path}: +++ の道は b/ で始める（{new}）")
            files.append((new[2:], []))
            i += 2
            continue
        m = hunk_re.match(line)
        if m:
            if not files:
                die(f"{path}: ファイルの見出しの前に @@ がある")
            os_, oc, ns, nc = (int(m.group(k)) if m.group(k) is not None else 1 for k in (1, 2, 3, 4))
            body = []
            i += 1
            o_seen = n_seen = 0
            while i < len(lines) and (o_seen < oc or n_seen < nc or lines[i].startswith(b"\\")):
                ln = lines[i]
                if ln.startswith(b"\\"):  # "\ No newline at end of file"：直前の行の改行を除く
                    mark, text = body[-1]
                    body[-1] = (mark, text.rstrip(b"\n"))
                    i += 1
                    continue
                mark, text = ln[:1], ln[1:]
                if mark == b" ":
                    o_seen += 1
                    n_seen += 1
                elif mark == b"-":
                    o_seen += 1
                elif mark == b"+":
                    n_seen += 1
                else:
                    die(f"{path}: 知らない行の印 {mark!r}（{i + 1} 行目）")
                body.append((mark, text))
                i += 1
            if o_seen != oc or n_seen != nc:
                die(f"{path}: @@ の行数が合わない（{line.decode().strip()}）")
            files[-1][1].append((os_, oc, ns, nc, body))
            continue
        i += 1
    return files


def apply_patch(root, path):
    for rel, hunks in parse_patch(path):
        target = os.path.join(root, rel)
        with open(target, "rb") as f:
            orig = f.read().splitlines(keepends=True)
        out = []
        cursor = 0
        for os_, oc, _ns, _nc, body in hunks:
            start = os_ - 1 if oc > 0 else os_
            if start < cursor:
                die(f"{path}: {rel} の @@ -{os_} が前の塊と重なる")
            old = [t for m, t in body if m in (b" ", b"-")]
            new = [t for m, t in body if m in (b" ", b"+")]
            if orig[start : start + len(old)] != old:
                die(f"{path}: {rel} の @@ -{os_},{oc} の文脈が源と合わない（源の版が違う？）")
            out.extend(orig[cursor:start])
            out.extend(new)
            cursor = start + len(old)
        out.extend(orig[cursor:])
        with open(target, "wb") as f:
            f.write(b"".join(out))


# ---- codebook の名前の表 ---------------------------------------------------------------------------


def booknames(lib):
    """books/*/*.h の「static const static_codebook 名前 = {」を拾い、名前とポインタの対応の表の C を作る。
    vorbisenc.c の末尾（KURA_PORT）で #include する（books は static なので、同じ翻訳単位でしか指せない）。"""
    pat = re.compile(r"^\s*static\s+const\s+static_codebook\s+([A-Za-z_][A-Za-z0-9_]*)\s*=", re.M)
    rows = []
    seen = set()
    for path in sorted(glob.glob(os.path.join(lib, "books", "*", "*.h"))):
        rel = os.path.relpath(path, os.path.join(lib, "books"))
        with open(path, encoding="latin-1") as f:
            text = f.read()
        for m in pat.finditer(text):
            name = m.group(1)
            if name in seen:
                die(f"codebook の名前が重なる：{name}")
            seen.add(name)
            rows.append((name, rel))
    out = [
        "/* 生成物：crates/vorbis-tables/oracle/build.py（手で直さない）。books/*.h の codebook の名前とポインタの対応。 */",
        "typedef struct { const static_codebook *book; const char *name; const char *file; } kura_bookname;",
        "const int kura_booknames_count = %d;" % len(rows),
        "const kura_bookname kura_booknames[%d] = {" % len(rows),
    ]
    out += ['  { &%s, "%s", "%s" },' % (name, name, rel) for name, rel in rows]
    out.append("};")
    return "\n".join(out) + "\n", len(rows)


# ---- 組み立て -------------------------------------------------------------------------------------


def assemble(src, out):
    """源を写し、差分を当て、新しいファイルと生成物を足す。源と違うファイルの (SHA-256, 相対の道) を返す。"""
    csrc = os.path.join(out, "csrc")
    for rel in INPUTS:
        dst = os.path.join(csrc, rel)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copyfile(os.path.join(src, rel), dst)
    for p in PATCHES:
        apply_patch(csrc, os.path.join(ORACLE, p))
    lib = os.path.join(out, LIB)
    for name in NEW_FILES:
        shutil.copyfile(os.path.join(ORACLE, "lib", name), os.path.join(lib, name))
    text, n = booknames(lib)
    with open(os.path.join(out, BOOKNAMES), "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    say(f"C を組み立てた：{disp(csrc)}（差分 {len(PATCHES)} 本・新しいファイル {len(NEW_FILES)} 個・codebook の名前 {n} 冊）")

    patched = set()
    for p in PATCHES:
        for rel, _ in parse_patch(os.path.join(ORACLE, p)):
            patched.add(os.path.join("csrc", rel))
    rels = sorted(patched) + [LIB + "/" + name for name in NEW_FILES] + [BOOKNAMES]
    return [(sha256_file(os.path.join(out, rel)), rel) for rel in rels]


def make_test_inputs(out):
    tin = os.path.join(out, "test", "in")
    os.makedirs(tin, exist_ok=True)
    mk = os.path.join(ORACLE, "scripts", "mkinput.py")
    jobs = []
    for name, rate, ch, secs, seed in TEST_INPUTS:
        cmd = [sys.executable, "-I", mk, "synth", os.path.join(tin, name), "--rate", str(rate), "--channels", str(ch),
               "--seconds", str(secs), "--seed", str(seed)]
        jobs.append(cmd)
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(jobs)) as ex:
        for r in ex.map(lambda c: subprocess.run(c, capture_output=True, text=True), jobs):
            if r.returncode != 0:
                die("入力を作れない：" + r.stderr[-2000:])
    say(f"照合のテストの入力を作った：{disp(tin)}（{len(TEST_INPUTS)} 個）")
    return [(sha256_file(os.path.join(tin, name)), "test/in/" + name) for name, *_ in TEST_INPUTS]


def run(cmd, **kw):
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if r.returncode != 0:
        die("失敗：" + " ".join(cmd) + "\n" + r.stdout[-2000:] + r.stderr[-4000:])
    return r


def build_rlibm(out):
    target = os.path.join(out, "rlibm-target")
    manifest = os.path.join(ORACLE, "rlibm", "Cargo.toml")
    run(["cargo", "build", "--release", "--locked", "-q", "--manifest-path", manifest, "--target-dir", target])
    lib = os.path.join(target, "release", "librlibm.a")
    if not os.path.exists(lib):
        die(f"{lib} ができていない")
    say(f"rlibm を組んだ：{disp(lib)}")
    return lib


def build_c(out, cc, rlibm, san, syslibm):
    csrc = os.path.join(out, "csrc")
    lib = os.path.join(out, LIB)
    vinc = os.path.join(csrc, "vorbis_vendor", "include")
    oinc = os.path.join(csrc, "ogg_vendor", "include")
    binp = os.path.join(out, "bin")
    objp = os.path.join(out, "obj")
    os.makedirs(binp, exist_ok=True)
    os.makedirs(objp, exist_ok=True)
    srcs = [os.path.join(lib, f + ".c") for f in VSRC] + [os.path.join(out, p) for p in OGGSRC]
    tools = os.path.join(ORACLE, "tools")
    # 共通：KURA_PORT、超越関数の差し替え（-include）、CODEGEN（FMA への縮約なし・符号付き整数のあふれは折り返し）
    base = [*CFLAGS, f"-I{lib}", f"-I{vinc}", f"-I{oinc}", *CODEGEN]
    common = [*CFLAGS, "-include", os.path.join(lib, "kura_libm.h"), f"-I{lib}", f"-I{vinc}", f"-I{oinc}", *CODEGEN]

    jobs = {
        "oracle_enc": [cc, "-O2", *common, *srcs, os.path.join(tools, "oracle_enc.c"), rlibm],
        "oracle_enc_O0": [cc, "-O0", *common, *srcs, os.path.join(tools, "oracle_enc.c"), rlibm],
        "oracle_setup": [cc, "-O2", *common, *srcs, os.path.join(tools, "oracle_setup.c"), rlibm],
        "pkt2ogg": [cc, "-O2", "-w", f"-I{oinc}", *[os.path.join(out, p) for p in OGGSRC],
                    os.path.join(tools, "pkt2ogg.c")],
        "ogg2pkt": [cc, "-O2", "-w", f"-I{oinc}", *[os.path.join(out, p) for p in OGGSRC],
                    os.path.join(tools, "ogg2pkt.c")],
    }
    if san:
        jobs["oracle_enc_san"] = [cc, "-O1", "-g", "-fno-omit-frame-pointer", "-fsanitize=address,undefined",
                                  "-fsanitize-recover=undefined", *common, *srcs,
                                  os.path.join(tools, "oracle_enc.c"), rlibm]
    if syslibm:
        # 比較専用：超越関数を差し替えない（OS の libm を使う）ほかは oracle_enc と同じ。照合には使わない
        jobs["oracle_enc_syslibm"] = [cc, "-O2", *base, *srcs, os.path.join(tools, "oracle_enc.c"), "-lm"]
    # 差し替えの確認用のオブジェクト（Rust の staticlib を除く C）
    objs = []
    for f in VSRC:
        o = os.path.join(objp, f + ".o")
        objs.append(o)
        jobs["obj:" + f] = [cc, "-O2", "-c", *common, os.path.join(lib, f + ".c"), "-o", o]
    o = os.path.join(objp, "oracle_enc.o")
    objs.append(o)
    jobs["obj:oracle_enc"] = [cc, "-O2", "-c", *common, os.path.join(tools, "oracle_enc.c"), "-o", o]
    for name in list(jobs):
        if not name.startswith("obj:"):
            jobs[name] = jobs[name] + ["-o", os.path.join(binp, name)]

    workers = max(1, min(len(jobs), os.cpu_count() or 4))
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as ex:
        futs = {ex.submit(subprocess.run, cmd, capture_output=True, text=True): name for name, cmd in jobs.items()}
        for fut in concurrent.futures.as_completed(futs):
            r = fut.result()
            if r.returncode != 0:
                die(f"{futs[fut]} を組めない：\n" + r.stderr[-4000:])

    # 差し替えの確認：C のオブジェクトが OS の libm の超越関数を参照していないこと
    nm = run(["nm", "-u", *objs]).stdout.split()
    bad = sorted(set(s for s in nm if OS_LIBM.match(s)))
    if bad:
        die("C のオブジェクトが OS の libm を参照している：" + " ".join(bad))
    refs = sorted(set(s for s in nm if MATH_REFS.match(s)))
    built = sorted(n for n in jobs if not n.startswith("obj:"))
    say(f"C を組んだ：{disp(binp)}/{{{','.join(built)}}}")
    say("C のオブジェクトが参照する数学関数（OS の libm の超越関数はなし）：" + " ".join(refs))
    return built


def tool_version(cmd):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True)
        return (r.stdout or r.stderr).splitlines()[0].strip()
    except (OSError, IndexError):
        return "?"


def main():
    ap = argparse.ArgumentParser(description="照合用のエンコーダを組む（README.md）")
    ap.add_argument("--src", help=f"{CRATE_VERSION} のディレクトリ（既定は $CARGO_HOME と ~/.cargo の registry/src/*/ から探す）")
    ap.add_argument("--out", default=os.path.join(REPO, "target", "vorbis-oracle"),
                    help="出力先（既定は <リポジトリ>/target/vorbis-oracle）")
    ap.add_argument("--cc", default="clang", help="C のコンパイラ（既定 clang。確かめたのは Apple clang）")
    ap.add_argument("--no-san", action="store_true", help="AddressSanitizer＋UBSan の版（oracle_enc_san）を組まない")
    ap.add_argument("--syslibm", action="store_true",
                    help="比較専用の oracle_enc_syslibm（超越関数を差し替えない）も組む")
    ap.add_argument("--allow-unverified-arch", action="store_true",
                    help="arm64 でなくても止まらない（確かめていない。README.md の「x86_64」）")
    ap.add_argument("--write-hashes", action="store_true",
                    help="inputs.sha256・outputs.sha256 を作り直す（源の版や差分・lib/ を変えたときだけ）")
    args = ap.parse_args()

    machine = platform.machine().lower()
    if machine not in ("arm64", "aarch64") and not args.allow_unverified_arch:
        die(f"確かめたのは arm64 だけです（今は {machine}）。x86_64 では aoTuV の SSE の経路が入り、別のエンコーダになる"
            "（README.md）。それでも組むなら --allow-unverified-arch")
    if sys.platform != "darwin":
        sys.stderr.write("build.py: 注意：確かめたのは macOS（Apple clang）だけです\n")

    src = find_src(args.src)
    out = os.path.abspath(args.out)
    actual = [(sha256_file(os.path.join(src, rel)), rel) for rel in INPUTS]
    inputs_path = os.path.join(ORACLE, "inputs.sha256")
    outputs_path = os.path.join(ORACLE, "outputs.sha256")
    if not args.write_hashes:
        compare_hashes(read_hashes(inputs_path), actual, "源が oracle/inputs.sha256")
    # gen/inputs.sha256（vorbis-tables の生成の入力）と重なるファイルは同じ値であること（同じ源を指している）
    gen = dict((rel, h) for h, rel in read_hashes(os.path.join(CRATE, "gen", "inputs.sha256")))
    for h, rel in actual:
        if rel in gen and gen[rel] != h:
            die(f"{rel} が gen/inputs.sha256 と違う（vorbis-tables の生成と違う源）")
    say(f"源：{src}（{len(INPUTS)} 個の SHA-256 が一致）")

    os.makedirs(out, exist_ok=True)
    made = assemble(src, out) + make_test_inputs(out)
    if args.write_hashes:
        write_hashes(inputs_path, actual, [
            f"照合用のエンコーダが {CRATE_VERSION} から写すファイル（build.py の INPUTS）。build.py --write-hashes の生成物",
        ])
        write_hashes(outputs_path, made, [
            "build.py が組み立てた C のうち源と違うファイルと、照合のテストの入力（出力先からの相対の道）。",
            "build.py --write-hashes の生成物",
        ])
        return
    compare_hashes(read_hashes(outputs_path), made, "組み立てた C・入力が oracle/outputs.sha256")
    say(f"組み立てた C と入力：{len(made)} 個の SHA-256 が一致")

    rlibm = build_rlibm(out)
    built = build_c(out, args.cc, rlibm, not args.no_san, args.syslibm)
    with open(os.path.join(out, "build-info.txt"), "w") as f:
        f.write(f"src: {src}\n")
        f.write(f"cc: {tool_version([args.cc, '--version'])}\n")
        f.write(f"cargo: {tool_version(['cargo', '--version'])}\n")
        f.write(f"machine: {platform.system()} {platform.machine()}\n")
        f.write(f"cflags: {' '.join(CFLAGS + CODEGEN)}\n")
        f.write(f"bin: {' '.join(built)}\n")
    say(f"できた：KURA_VORBIS_ORACLE={out}")


if __name__ == "__main__":
    main()
