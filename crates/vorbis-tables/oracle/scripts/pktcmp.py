"""2 つのパケットのファイル（oracle_enc の出力の形式）を比べる。

使い方: pktcmp.py a.pkt b.pkt [--ignore-comment] [--ignore-granule] [--quiet]
  最初に違うパケットの番号（0 から。0〜2 はヘッダ）と、その中の最初に違うビットの位置を出す。
  ビットの位置は Vorbis の詰め方（各バイトの最下位ビットから）で数えた通し番号
  （= バイトの位置 × 8 + バイトの中の LSB からのビットの番号）。
  長さ・granulepos・旗（b_o_s・e_o_s）の違いも出す。
  --ignore-comment：パケット 1（コメントのヘッダ。vendor の文字列が違ってよい）を比べない。
  --ignore-granule：granulepos を比べない（ogg2pkt で Ogg から取り出したものはページの最後の
                    パケットにしか granulepos がない）。
終了コード：一致 0、違いあり 1、読めない 2。
"""
import sys, struct

def read(path):
    data = open(path, 'rb').read()
    out = []; at = 0
    while at < len(data):
        if at + 13 > len(data):
            raise ValueError('%s: truncated header at %d' % (path, at))
        ln, gp, fl = struct.unpack('<IqB', data[at:at+13])
        at += 13
        body = data[at:at+ln]
        if len(body) != ln:
            raise ValueError('%s: truncated body at %d' % (path, at))
        at += ln
        out.append((gp, fl, body))
    return out

def first_bit(a, b):
    n = min(len(a), len(b))
    for i in range(n):
        if a[i] != b[i]:
            x = a[i] ^ b[i]
            bit = (x & -x).bit_length() - 1
            return i, bit, i * 8 + bit
    if len(a) != len(b):
        return n, 0, n * 8
    return None

def main():
    args = [x for x in sys.argv[1:] if not x.startswith('--')]
    ignore_comment = '--ignore-comment' in sys.argv
    ignore_granule = '--ignore-granule' in sys.argv
    quiet = '--quiet' in sys.argv
    if len(args) != 2:
        print(__doc__); return 2
    try:
        A = read(args[0]); B = read(args[1])
    except (OSError, ValueError) as e:
        print('error:', e); return 2
    diffs = 0
    for k in range(max(len(A), len(B))):
        if ignore_comment and k == 1:
            continue
        if k >= len(A) or k >= len(B):
            print('packet count differs: %d vs %d (first missing packet %d)' % (len(A), len(B), k))
            return 1
        (ga, fa, pa), (gb, fb, pb) = A[k], B[k]
        msgs = []
        if ga != gb and not ignore_granule: msgs.append('granulepos %d vs %d' % (ga, gb))
        if fa != fb: msgs.append('flags %d vs %d' % (fa, fb))
        if len(pa) != len(pb): msgs.append('bytes %d vs %d' % (len(pa), len(pb)))
        fb_ = first_bit(pa, pb)
        if fb_ is not None:
            byte, bit, pos = fb_
            msgs.append('first differing bit %d (byte %d, bit %d from LSB)' % (pos, byte, bit))
        if msgs:
            print('packet %d%s: %s' % (k, ' (header)' if k < 3 else ' (audio #%d)' % (k - 3), '; '.join(msgs)))
            return 1
    if not quiet:
        tot = sum(len(p) for _, _, p in A)
        print('identical: %d packets (%d audio), %d bytes' % (len(A), max(0, len(A) - 3), tot))
    return 0

if __name__ == '__main__':
    sys.exit(main())
