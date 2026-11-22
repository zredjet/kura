"""組んだ照合用のエンコーダの確かめ（作業領域の verify.py を簡略にしたもの。README.md §7）。

使い方:
  python3 -I crates/vorbis-tables/oracle/scripts/check.py [--oracle DIR] [--ref DIR] [--inputs DIR]
      [--filter 正規表現] [--qs -1,0,5,10] [--no-san] [--jobs N] [--keep DIR]

  --oracle DIR : 確かめる出力先（bin/oracle_enc がある所。既定は <リポジトリ>/target/vorbis-oracle）
  --ref DIR    : 別の照合用のエンコーダ（bin/oracle_enc）。同じ入力で出力のパケットのファイルがバイト一致するか
                 （組み直したものが前のものと同じかを見る）
  --inputs DIR : 入力（planar の f32。名前は 種類_レート_チャンネル.f32）。既定は合成の入力（9 レート × 1・2・6・8 ch、
                 1 秒）と端の場合（0・1・64・1,000 フレーム、無音、直流）を DIR/check-in/ に作って使う
  --filter     : 入力の名前（.f32 を除く）を正規表現で選ぶ
  --qs         : q の並び（既定：clips・synth は -1,0,5,10、ほかは -1,5,10）
  --no-san     : oracle_enc_san を回さない（遅いので）
  --keep DIR   : oracle_enc の出力（名前_q.pkt）を DIR に残す

各ケースで、oracle_enc（-O2）の出力と次がバイト一致するかを見る：oracle_enc_O0（あれば）、もう一度の oracle_enc、
ダンプ（KURA_DUMP_LEVEL=2）を書きながら、malloc の中身を汚して（MallocPreScribble・MallocScribble）、oracle_enc_san
（あれば。誤りの終了も数える）、--ref の oracle_enc。終了コード：すべて一致 0、違いあり 1。
"""

import argparse
import concurrent.futures
import hashlib
import math
import os
import re
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ORACLE = os.path.dirname(HERE)
REPO = os.path.dirname(os.path.dirname(os.path.dirname(ORACLE)))
RATES = [8000, 11025, 16000, 22050, 32000, 44100, 48000, 96000, 192000]
CHANS = [1, 2, 6, 8]
NAME = re.compile(r"^([A-Za-z0-9]+)_(\d+)_(\d+)$")


def sha(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def run(cmd, env=None):
    e = dict(os.environ)
    for k in ("KURA_ABL", "KURA_DUMP", "KURA_DUMP_LEVEL"):
        e.pop(k, None)
    if env:
        e.update(env)
    return subprocess.run(cmd, capture_output=True, text=True, env=e)


def make_inputs(d):
    """既定の入力を作る（あれば作らない）。"""
    os.makedirs(d, exist_ok=True)
    mk = os.path.join(HERE, "mkinput.py")
    jobs = []
    for rate in RATES:
        for ch in CHANS:
            p = os.path.join(d, "synth_%d_%d.f32" % (rate, ch))
            if not os.path.exists(p):
                jobs.append([sys.executable, "-I", mk, "synth", p, "--rate", str(rate), "--channels", str(ch),
                             "--seconds", "1", "--seed", str(rate + ch)])
    with concurrent.futures.ThreadPoolExecutor(max_workers=os.cpu_count() or 4) as ex:
        for r in ex.map(lambda c: subprocess.run(c, capture_output=True, text=True), jobs):
            if r.returncode != 0:
                sys.exit("入力を作れない：" + r.stderr[-2000:])
    # 端の場合：ごく短い入力・無音・直流
    for rate, ch in ((44100, 2), (8000, 1), (48000, 6), (192000, 8)):
        for fr in (0, 1, 64, 1000):
            p = os.path.join(d, "short%d_%d_%d.f32" % (fr, rate, ch))
            if not os.path.exists(p):
                with open(p, "wb") as f:
                    for c in range(ch):
                        for i in range(fr):
                            f.write(struct.pack("<f", 0.5 * math.sin(0.05 * (i + 1) * (c + 1))))
        for kind, v in (("silence", 0.0), ("dc", 0.999)):
            p = os.path.join(d, "%s_%d_%d.f32" % (kind, rate, ch))
            if not os.path.exists(p):
                with open(p, "wb") as f:
                    f.write(struct.pack("<f", v) * (rate * ch))


def cases_of(d, filt, qs):
    out = []
    for name in sorted(os.listdir(d)):
        if not name.endswith(".f32"):
            continue
        m = NAME.match(name[:-4])
        if not m or (filt and not re.search(filt, name[:-4])):
            continue
        kind, rate, ch = m.group(1), int(m.group(2)), int(m.group(3))
        for q in qs if qs is not None else ((-1, 0, 5, 10) if kind in ("clips", "synth") else (-1, 5, 10)):
            out.append(("%s_q%d" % (name[:-4], q), os.path.join(d, name), rate, ch, q))
    return out


def check_case(case, a, tmp):
    name, path, rate, ch, q = case
    res = {}
    msgs = []
    bin_ = os.path.join(a.oracle, "bin")
    main_out = os.path.join(tmp, name + ".main.pkt")
    r = run([os.path.join(bin_, "oracle_enc"), path, main_out, str(rate), str(ch), str(q)])
    if r.returncode != 0:
        return name, {"main": False}, ["oracle_enc: 終了コード %d %s" % (r.returncode, r.stderr.strip()[-300:])]
    h = sha(main_out)
    dump = os.path.join(tmp, name + ".kdmp")
    checks = [("repeat", os.path.join(bin_, "oracle_enc"), None)]
    if os.path.exists(os.path.join(bin_, "oracle_enc_O0")):
        checks.append(("O0", os.path.join(bin_, "oracle_enc_O0"), None))
    checks.append(("dump", os.path.join(bin_, "oracle_enc"), {"KURA_DUMP": dump, "KURA_DUMP_LEVEL": "2"}))
    checks.append(("scribble", os.path.join(bin_, "oracle_enc"), {"MallocPreScribble": "1", "MallocScribble": "1"}))
    if not a.no_san and os.path.exists(os.path.join(bin_, "oracle_enc_san")):
        checks.append(("san", os.path.join(bin_, "oracle_enc_san"),
                       {"UBSAN_OPTIONS": "print_stacktrace=0", "ASAN_OPTIONS": "detect_leaks=0"}))
    if a.ref:
        checks.append(("ref", os.path.join(a.ref, "bin", "oracle_enc"), None))
    for tag, binary, env in checks:
        out = os.path.join(tmp, "%s.%s.pkt" % (name, tag))
        r = run([binary, path, out, str(rate), str(ch), str(q)], env=env)
        if r.returncode != 0:
            res[tag] = False
            msgs.append("%s: 終了コード %d %s" % (tag, r.returncode, r.stderr.strip()[-300:]))
        else:
            res[tag] = sha(out) == h
            if not res[tag]:
                c = run([sys.executable, "-I", os.path.join(HERE, "pktcmp.py"), main_out, out])
                msgs.append("%s: %s" % (tag, c.stdout.strip()))
        for p in (out, dump):
            if os.path.exists(p):
                os.remove(p)
    if a.keep:
        os.replace(main_out, os.path.join(a.keep, name + ".pkt"))
    else:
        os.remove(main_out)
    return name, res, msgs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--oracle", default=os.path.join(REPO, "target", "vorbis-oracle"))
    ap.add_argument("--ref")
    ap.add_argument("--inputs")
    ap.add_argument("--filter")
    ap.add_argument("--qs")
    ap.add_argument("--no-san", action="store_true")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--keep")
    a = ap.parse_args()
    a.oracle = os.path.abspath(a.oracle)
    if not os.path.exists(os.path.join(a.oracle, "bin", "oracle_enc")):
        sys.exit("%s/bin/oracle_enc がない（build.py で組む）" % a.oracle)
    if a.ref and not os.path.exists(os.path.join(a.ref, "bin", "oracle_enc")):
        sys.exit("%s/bin/oracle_enc がない" % a.ref)
    if a.keep:
        os.makedirs(a.keep, exist_ok=True)
    inputs = a.inputs
    if not inputs:
        inputs = os.path.join(a.oracle, "check-in")
        make_inputs(inputs)
    qs = [int(x) for x in a.qs.split(",")] if a.qs else None
    cases = cases_of(inputs, a.filter, qs)
    if not cases:
        sys.exit("ケースがない")
    totals = {}
    bad = 0
    with tempfile.TemporaryDirectory(prefix="kura-vorbis-oracle-check-") as tmp:
        with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as ex:
            for name, res, msgs in ex.map(lambda c: check_case(c, a, tmp), cases):
                for k, v in res.items():
                    t = totals.setdefault(k, [0, 0])
                    t[0] += bool(v)
                    t[1] += 1
                if msgs or not all(res.values()):
                    bad += 1
                    print("違い %s: %s" % (name, "; ".join(msgs) or res), flush=True)
    print("%d ケース（入力 %s）" % (len(cases), inputs))
    for k, (ok, n) in totals.items():
        print("  %-9s %d/%d 一致" % (k, ok, n))
    print("違いのあるケース：%d" % bad)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
