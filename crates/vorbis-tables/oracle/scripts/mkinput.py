"""照合用のエンコーダの入力（planar の f32 リトルエンディアン）を作る。

使い方:
  mkinput.py wav   in.wav out.f32
      WAV（PCM 16/24 ビット・float 32）をそのまま planar にする（調査の enc.c と同じ換算：
      16 ビットは x/32768、24 ビットは x/8388608）。
  mkinput.py clips out.f32 --rate R --channels C --seconds S --src a.wav,b.wav,...
      ffmpeg で各ソースを R Hz の stereo の f32 にし、チャンネル c に
      ソース c % len(src) の（c // len(src)) % 2 番目のチャンネルを、c×0.37 秒ずらして入れる。
  mkinput.py synth out.f32 --rate R --channels C --seconds S [--seed N]
      合成の信号（チャンネルごとに違う：掃引・和音・雑音の断続・クリック・無音・大振幅）。
  mkinput.py info in.f32 --channels C
      フレーム数と、チャンネルごとの最大振幅を出す。
"""
import sys, struct, subprocess, math, argparse
from array import array

def write_planar(path, chans):
    n = len(chans[0])
    with open(path, 'wb') as f:
        for c in chans:
            assert len(c) == n
            a = array('f', c)
            if sys.byteorder != 'little':
                a.byteswap()
            f.write(a.tobytes())

def read_wav(path):
    data = open(path, 'rb').read()
    assert data[:4] == b'RIFF' and data[8:12] == b'WAVE'
    at = 12; fmt = None; body = None
    while at + 8 <= len(data):
        cid = data[at:at+4]; clen = struct.unpack('<I', data[at+4:at+8])[0]
        if cid == b'fmt ':
            tag, ch, rate = struct.unpack('<HHI', data[at+8:at+16])
            bits = struct.unpack('<H', data[at+22:at+24])[0]
            if tag == 0xFFFE:
                tag = struct.unpack('<H', data[at+32:at+34])[0]
            fmt = (tag, ch, rate, bits)
        elif cid == b'data':
            body = data[at+8:at+8+clen]
        at += 8 + clen + (clen & 1)
    tag, ch, rate, bits = fmt
    bps = bits // 8
    frames = len(body) // (bps * ch)
    chans = [[0.0] * frames for _ in range(ch)]
    if tag == 3 and bits == 32:
        a = array('f'); a.frombytes(body[:frames*ch*4])
        for i in range(frames):
            for c in range(ch):
                chans[c][i] = a[i*ch+c]
    elif tag == 1 and bits == 16:
        a = array('h'); a.frombytes(body[:frames*ch*2])
        for i in range(frames):
            for c in range(ch):
                chans[c][i] = a[i*ch+c] / 32768.0
    elif tag == 1 and bits == 24:
        for i in range(frames):
            for c in range(ch):
                o = (i*ch+c)*3
                x = body[o] | (body[o+1] << 8) | (body[o+2] << 16)
                if x & 0x800000: x -= 1 << 24
                chans[c][i] = x / 8388608.0
    else:
        raise SystemExit('unsupported wav %r' % (fmt,))
    return rate, chans

def ffmpeg_stereo(path, rate):
    out = subprocess.run(['ffmpeg', '-v', 'error', '-i', path, '-ar', str(rate), '-ac', '2',
                          '-f', 'f32le', '-acodec', 'pcm_f32le', '-'], check=True, capture_output=True).stdout
    a = array('f'); a.frombytes(out)
    if sys.byteorder != 'little': a.byteswap()
    return [a[0::2], a[1::2]]

def clips(args):
    srcs = args.src.split(',')
    n = int(round(args.seconds * args.rate))
    dec = [ffmpeg_stereo(s, args.rate) for s in srcs]
    chans = []
    for c in range(args.channels):
        s = dec[c % len(srcs)]
        lr = s[(c // len(srcs)) % 2]
        off = int(c * 0.37 * args.rate) % max(1, len(lr) - n) if len(lr) > n else 0
        seg = list(lr[off:off+n])
        seg += [0.0] * (n - len(seg))
        chans.append(seg)
    write_planar(args.out, chans)

def synth(args):
    rate, n = args.rate, int(round(args.seconds * args.rate))
    state = [args.seed * 2654435761 % (1 << 32) or 1]
    def rnd():  # 線形合同の乱数（決定的）
        state[0] = (state[0] * 1664525 + 1013904223) % (1 << 32)
        return state[0] / 4294967296.0 * 2 - 1
    chans = []
    nyq = rate / 2
    for c in range(args.channels):
        x = [0.0] * n
        kind = c % 4
        f0 = 55.0 * (1 + c)
        for i in range(n):
            t = i / rate
            seg = (t / args.seconds) * 6  # 6 区間
            v = 0.0
            if seg < 1:      # 掃引（20 Hz → ナイキストの 0.95 倍）
                f = 20 * (nyq * 0.95 / 20) ** seg
                ph = 20 * ((nyq * 0.95 / 20) ** seg - 1) / math.log(nyq * 0.95 / 20) * args.seconds / 6
                v = 0.5 * math.sin(2 * math.pi * ph)
            elif seg < 2:    # 和音
                for h in (1, 1.25, 1.5, 2.0):
                    if f0 * h * (1 + kind) < nyq:
                        v += 0.15 * math.sin(2 * math.pi * f0 * h * (1 + kind) * t)
            elif seg < 3:    # 雑音の断続
                v = 0.3 * rnd() if int(t * 8) % 2 == 0 else 0.0
            elif seg < 4:    # クリック（前エコーの試験）
                k = int(t * 4)
                v = 0.9 if abs(t - (k / 4 + 0.01 * c)) < 1.5 / rate else 0.002 * rnd()
            elif seg < 5:    # 無音
                v = 0.0
            else:            # 大振幅の矩形に近い波（クリップ寸前）
                v = 0.99 * (1 if math.sin(2 * math.pi * (110 + 10 * c) * t) >= 0 else -1)
            x[i] = v
        chans.append(x)
    write_planar(args.out, chans)

def info(args):
    a = array('f'); a.frombytes(open(args.inp, 'rb').read())
    if sys.byteorder != 'little': a.byteswap()
    n = len(a) // args.channels
    peaks = [max((abs(v) for v in a[c*n:(c+1)*n]), default=0.0) for c in range(args.channels)]
    print('frames', n, 'peaks', ' '.join('%.4f' % p for p in peaks))

def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest='cmd', required=True)
    w = sub.add_parser('wav'); w.add_argument('inp'); w.add_argument('out')
    for name in ('clips', 'synth'):
        p = sub.add_parser(name); p.add_argument('out')
        p.add_argument('--rate', type=int, required=True); p.add_argument('--channels', type=int, required=True)
        p.add_argument('--seconds', type=float, required=True)
        if name == 'clips': p.add_argument('--src', required=True)
        else: p.add_argument('--seed', type=int, default=1)
    i = sub.add_parser('info'); i.add_argument('inp'); i.add_argument('--channels', type=int, required=True)
    args = ap.parse_args()
    if args.cmd == 'wav':
        rate, chans = read_wav(args.inp); write_planar(args.out, chans)
        print('rate', rate, 'channels', len(chans), 'frames', len(chans[0]))
    elif args.cmd == 'clips': clips(args)
    elif args.cmd == 'synth': synth(args)
    else: info(args)

if __name__ == '__main__':
    main()
