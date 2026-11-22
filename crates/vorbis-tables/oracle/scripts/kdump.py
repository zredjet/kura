"""ダンプ（KURA_DUMP の出力。形式は README）を読む・比べる。

使い方:
  kdump.py list a.kdmp [--block N]          記録の一覧（タグ・チャンネル・型・要素数）
  kdump.py show a.kdmp --block N --tag TAG [--ch C]   値を出す（f32 は値とビット列）
  kdump.py diff a.kdmp b.kdmp [--max N] [--nan-equal]
                                            先頭から記録を順に比べ、最初に違う記録と要素を出す
                                            （先頭の KDMP の違いは知らせるだけで続ける）。
                                            --nan-equal：f32 の NaN はビット列が違っても同じとみなす
                                            （x86_64 の既定の NaN は符号ビットが立つ。README §8）
終了コード（diff）：一致 0、違いあり 1。
"""
import sys, struct, argparse

TYPES = {1: ('i32', 4, '<i'), 2: ('f32', 4, '<I'), 3: ('i64', 8, '<q')}

def records(path):
    data = open(path, 'rb').read()
    at = 0; block = -1
    while at < len(data):
        tag = data[at:at+4].decode('ascii'); ch, typ, ln = struct.unpack('<HHI', data[at+4:at+12])
        at += 12
        name, size, fmt = TYPES[typ]
        raw = data[at:at+ln]; at += ln
        vals = [struct.unpack(fmt, raw[i:i+size])[0] for i in range(0, ln, size)]
        if tag == 'BLCK':
            block = vals[0]
        yield block, tag, ch, typ, vals

def f32(u):
    return struct.unpack('<f', struct.pack('<I', u))[0]

def fmt_val(typ, v):
    if typ == 2:
        return '%.9g (0x%08x)' % (f32(v), v)
    return str(v)

def chs(ch):
    return '-' if ch == 0xFFFF else str(ch)

def cmd_list(a):
    for block, tag, ch, typ, vals in records(a.a):
        if a.block is not None and block != a.block: continue
        extra = ''
        if tag in ('BLCK', 'KDMP', 'BEND'):
            extra = ' ' + ' '.join(map(str, vals))
        print('%6d %s ch=%-3s %s n=%d%s' % (block, tag, chs(ch), TYPES[typ][0], len(vals), extra))

def cmd_show(a):
    for block, tag, ch, typ, vals in records(a.a):
        if block != a.block or tag != a.tag: continue
        if a.ch is not None and ch != a.ch: continue
        print('block %d %s ch=%s n=%d' % (block, tag, chs(ch), len(vals)))
        for i, v in enumerate(vals):
            print('  [%d] %s' % (i, fmt_val(typ, v)))

def cmd_diff(a):
    A = list(records(a.a)); B = list(records(a.b))
    for k in range(max(len(A), len(B))):
        if k >= len(A) or k >= len(B):
            print('record count differs: %d vs %d' % (len(A), len(B))); return 1
        ra, rb = A[k], B[k]
        if ra[1:4] != rb[1:4] or len(ra[4]) != len(rb[4]):
            print('record %d differs in kind: block %d %s ch=%s %s n=%d  vs  block %d %s ch=%s %s n=%d' % (
                k, ra[0], ra[1], chs(ra[2]), TYPES[ra[3]][0], len(ra[4]),
                rb[0], rb[1], chs(rb[2]), TYPES[rb[3]][0], len(rb[4])))
            return 1
        if ra[4] != rb[4] and ra[1] == 'KDMP':
            print('note: KDMP header differs: %s vs %s (continuing)' % (ra[4], rb[4]))
            continue
        if ra[4] != rb[4]:
            block, tag, ch, typ, va = ra; vb = rb[4]
            isnan = lambda u: (u & 0x7f800000) == 0x7f800000 and (u & 0x7fffff) != 0
            idx = [i for i in range(len(va)) if va[i] != vb[i]
                   and not (a.nan_equal and typ == 2 and isnan(va[i]) and isnan(vb[i]))]
            if not idx:
                continue
            print('first difference: record %d, block %d, %s ch=%s: %d of %d elements differ' % (
                k, block, tag, chs(ch), len(idx), len(va)))
            for i in idx[:a.max]:
                print('  [%d] %s  vs  %s' % (i, fmt_val(typ, va[i]), fmt_val(typ, vb[i])))
            return 1
    print('identical: %d records' % len(A))
    return 0

def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest='cmd', required=True)
    l = sub.add_parser('list'); l.add_argument('a'); l.add_argument('--block', type=int)
    s = sub.add_parser('show'); s.add_argument('a'); s.add_argument('--block', type=int, required=True)
    s.add_argument('--tag', required=True); s.add_argument('--ch', type=int)
    d = sub.add_parser('diff'); d.add_argument('a'); d.add_argument('b'); d.add_argument('--max', type=int, default=10)
    d.add_argument('--nan-equal', action='store_true')
    a = ap.parse_args()
    if a.cmd == 'show' and len(a.tag) < 4: a.tag = a.tag.ljust(4)
    return {'list': cmd_list, 'show': cmd_show, 'diff': cmd_diff}[a.cmd](a) or 0

if __name__ == '__main__':
    sys.exit(main())
