# フィクスチャ（他のツールで作ったファイル）

自前のエンコーダでは作れない形（非可逆の WebP、libjpeg のプログレッシブ、ImageMagick の PSD など）を読む確かめに使う。
`tests/fixtures.rs` が他の実装の読み取り（参照の PNG）と照らし、`tests/codec_golden.rs` が自前のデコーダの値を凍結する
（`FileGen::Fixture`）。

- **消さない・変えない**（`scripts/check-goldens-append-only.sh` が見る）。足すときは新しい名前で。
- 作ったときの道具：macOS 26.7（sips）、ImageMagick 7.1.2-32 Q16-HDRI、libwebp 1.6.0（cwebp・dwebp・img2webp）、libjpeg-turbo 3.2.0（djpeg）。
- 元の画像：`src-rgb.png` は `magick -seed 7 -size 40x30 plasma:fractal`、`src-rgba.png` は赤→青のグラデーションに、
  白→透明のグラデーションのアルファを付けたもの。どちらも 8 ビットにしてから使う（`src8.png`・`srca8.png`）。

| ファイル | 作り方 | 参照 |
|---|---|---|
| `psd-rgb8.psd` | `magick src-rgb.png -depth 8 psd-rgb8.psd` | `psd-rgb8.sips.png`（`sips -s format png`） |
| `psd-rgb8-raw.psd` | 同上に `-compress None` | `psd-rgb8-raw.sips.png` |
| `psd-rgba8.psd` | `magick src-rgba.png -depth 8 psd-rgba8.psd`（合成は下塗りなし、層の数は −1） | `psd-rgba8.source.png`（元の画像。sips は乗算済みのような値を出すので使わない） |
| `psd-gray8.psd` | `magick src-rgb.png -depth 8 -colorspace Gray` | `psd-gray8.sips.png` |
| `psd-indexed.psd` | `magick src-rgb.png -depth 8 -colors 16 -type Palette`（`-depth 8` がないと Q16 版は 16 ビットの番号を書く壊れたファイルになる） | `psd-indexed.sips.png` |
| `psd-rgb16.psd` | `magick src-rgb.png -depth 16` | `psd-rgb16.magick.png`（`magick 'psd-rgb16.psd[0]' -depth 16`。sips は 16 ビットの PSD を読めなかった） |
| `psb-rgb8.psb` | `magick src8.png -depth 8 psb-rgb8.psb`（拡張子で PSB になる） | `psb-rgb8.sips.png`（`sips -s format png`） |
| `psb-rgba8.psb` | `magick srca8.png -depth 8 psb-rgba8.psb` | `psb-rgba8.source.png`（元の画像 `srca8.png`） |
| `psd-cmyk.psd` | `magick src-rgb.png -colorspace CMYK` | 読めない（`Unsupported`）こと |
| `webp-lossy.webp` | `cwebp -q 75 src8.png` | `webp-lossy.dwebp.png`（`dwebp`） |
| `webp-lossy-alpha.webp` | `cwebp -q 75 -alpha_q 100 srca8.png` | `webp-lossy-alpha.dwebp.png` |
| `webp-anim.webp` | `img2webp -loop 3 -lossless f1.png -d 120 f2.png -d 80 -lossy -q 70 f3.png`（f1：透明に赤い四角、f2：半透明の緑の四角、f3：src8） | `webp-anim.magick-{0,1,2}.png`（`magick webp-anim.webp -coalesce`。最初のフレームの外側は ImageMagick だけが不透明な黒にする） |
| `jpeg-progressive.jpg` | `magick src8.png -quality 85 -interlace Plane` | `jpeg-progressive.djpeg.png`（`djpeg` の PPM を PNG に） |
| `jpeg-gray.jpg` | `magick src8.png -quality 90 -colorspace Gray` | `jpeg-gray.djpeg.png` |
| `jpeg-422.jpg` | `magick src8.png -quality 90 -sampling-factor 2x1` | `jpeg-422.djpeg.png` |
| `jpeg-restart.jpg` | `magick src8.png -quality 90 -define jpeg:restart-interval=1` | `jpeg-restart.djpeg.png` |
| `jpeg-cmyk.jpg` | `magick src8.png -quality 90 -colorspace CMYK` | 読めない（`Unsupported`）こと |

照らした結果（`tests/fixtures.rs`）：PSD・PSB と非可逆の WebP は参照とちょうど同じ、アニメの WebP は描いた範囲で同じ、
JPEG は libjpeg-turbo と IDCT・間引きの戻し方が違うので 1〜7 段の差（値そのものはデコーダのゴールデンで凍結する）。

## 音声（段 6）

自前のエンコーダがない形式（MP3・AAC・ALAC・Vorbis・Opus）と、他の実装が書いた FLAC・AIFF を読む確かめに使う。
`tests/fixtures.rs` が、可逆の形式は元の WAV とちょうど同じ値か、非可逆の形式は他のデコーダの読み取り（16 ビットに
丸めて FLAC で保存した参照）と長さがちょうど同じで値が 60 dB 以上の比で近いかを照らす。再現できるデコーダ（FLAC・
ALAC・AIFF・MP3）の値は `tests/codec_golden.rs` で凍結する。

- 作ったときの道具：SoX 14.4.2、flac 1.5.0、LAME 4.0、FFmpeg 9.0.2、macOS 26.7 の afconvert・afinfo、vorbis_rs 0.5.6
  （aoTuV 20110424、libvorbis 1.3.7 ベース。作業用の小さな Rust のプログラムで呼んだ）、lewton 0.10.2（同じく作業用）。
- 元の音：`audio-src16.wav` は `sox -n -r 44100 -c 2 -b 16 audio-src16.wav synth 1.0 sine 100-8000 sine 8000-100 gain -6`
  （左右で逆向きの掃引。44,100 フレーム）、`audio-src24.wav` は
  `sox -n -r 48000 -c 2 -b 24 audio-src24.wav synth 0.5 sine 200-12000 sine 300 gain -3`（24,000 フレーム。拡張形式）。

| ファイル | 作り方 | 参照 |
|---|---|---|
| `flac-16.flac` | `flac -8 -o flac-16.flac audio-src16.wav`（SEEKTABLE・PADDING 付き） | 元の WAV とちょうど同じ |
| `flac-24.flac` | `flac -5 -o flac-24.flac audio-src24.wav` | 同上 |
| `alac-16.m4a` | `afconvert -f m4af -d alac audio-src16.wav alac-16.m4a`（iTunSMPB 付き） | 同上 |
| `alac-24.m4a` | `ffmpeg -i audio-src24.wav -c:a alac -sample_fmt s32p alac-24.m4a` | 同上（元の深さ 24 を読む） |
| `aiff-24.aiff` | `sox audio-src24.wav aiff-24.aiff trim 0 4800s`（COMT 付き） | 元の先頭 4,800 フレーム |
| `aifc-f32.aif` | 元の先頭 4,410 フレームを `afconvert -f AIFC -d BEF32`（FVER・長い圧縮の名前付き） | 元の先頭 4,410 フレーム |
| `mp3-lame.mp3` | `lame -b 128 --tt Sweep --ta kura audio-src16.wav`（ID3v2・LAME タグ） | `mp3-lame.ffmpeg.flac`（gapless で 44,100） |
| `mp3-notag.mp3` | `lame -t -b 128 audio-src16.wav`（Xing・LAME タグなし） | `mp3-notag.ffmpeg.flac`（切らずに 46,080） |
| `mp3-mpeg2-mono.mp3` | `lame -m m --resample 22.05 -b 64 audio-src16.wav`（MPEG-2） | `mp3-mpeg2-mono.ffmpeg.flac`（22,050） |
| `ogg-vorbis-loop.ogg` | vorbis_rs（品質 0.5、serial 1）。コメント `TITLE=Sweep`・`LOOPSTART=4410`・`LOOPLENGTH=22050` | `ogg-vorbis-loop.lewton.flac`（44,100） |
| `ogg-vorbis-ffmpeg.ogg` | `ffmpeg -i audio-src16.wav -c:a vorbis -strict -2 …`（FFmpeg の実験的な Vorbis エンコーダ） | `ogg-vorbis-ffmpeg.lewton.flac`（44,160。最後のグラニュールが詰め物を含む） |
| `aac-lc-afconvert.m4a` | `afconvert -f m4af -d aac -b 128000 audio-src16.wav`（iTunSMPB：priming 2,112） | `aac-lc-afconvert.afconvert.flac`（`afconvert -f WAVE -d LEI16`。44,100） |
| `aac-lc-ffmpeg.m4a` | `ffmpeg -i audio-src16.wav -c:a aac -b:a 128k`（elst：priming 1,024。ASC に SBR なしの明示） | `aac-lc-ffmpeg.ffmpeg.flac`（44,100） |
| `aac-lc.aac` | 同上に `-f adts` | `aac-lc.ffmpeg.flac`（切らずに 46,080） |
| `aac-he.m4a` | `afconvert -f m4af -d aach -b 64000 audio-src16.wav` | 読めない（`Unsupported`）こと |
| `ogg-opus.ogg` | `ffmpeg -i audio-src16.wav -c:a libopus -b:a 96k -f ogg` | Ogg Vorbis のデコーダでは読めない（`Unsupported`）こと |
| `mp3-free.mp3` | `lame -S --freeformat -b 40 audio-src16.wav`（LAME が 16 kHz の MPEG-2 に変える。free format） | `mp3-free.lame.flac`（`lame --decode`。ffmpeg は free format を読めない。16,000） |
| `mp3-info-frames0.mp3` | `mp3-lame.mp3` の Info のフレームの数（`Info` の 8 バイト後ろの 4 バイト）を 0 に書き換えたもの（途中で止まった ffmpeg の出力の形） | `mp3-info-frames0.ffmpeg.flac`（末尾を切らずに 44,975） |
| `mp2-layer2.mp3` | `ffmpeg -i audio-src16.wav -c:a mp2 -b:a 128k -f mp2`（.mp3 の拡張子の MPEG layer II） | MP3 のデコーダでは読めない（`Unsupported`）こと |
| `ogg-vorbis-granule-trim.ogg` | 段 6 の検証で作ったもの（作業用のプログラム）：8 ch・ブロック 8192 の Vorbis で、1 バイトの音声パケットが 2,000 個、最後のグラニュールが 1 | 形は小さいが、切り取る前のデコードの量（約 262 MB）を上限 64 MiB で断ること |
| `ogg-vorbis-3ch.ogg` | 段 7：vorbis_rs（品質 0.5、serial 1、コメント `TITLE=Channels`）。作業用のプログラム（`vorbis-enc` の写しで、チャンネル数を引数で受ける。`vorbis-enc-mc 3 ogg-vorbis-3ch.ogg`）が下の音を作り、f32 のまま 1,024 フレームずつ渡す。2 チャンネル以外なので coupling なし・全チャンネルで residue 1 | `ogg-vorbis-3ch.lewton.flac`（22,050。lewton の出力の並び（Vorbis の並び：L・C・R）のまま保存。比べるときに kura の WAV の並びに直す） |
| `ogg-vorbis-5ch.ogg` | 同上（`vorbis-enc-mc 5 ogg-vorbis-5ch.ogg`。チャンネル 3 は無音） | `ogg-vorbis-5ch.lewton.flac`（22,050。Vorbis の並び：L・C・R・Ls・Rs のまま） |

- 参照の `*.ffmpeg.flac`・`*.lewton.flac`・`*.afconvert.flac` は、そのデコーダの出力を 16 ビットに（ディザなしで）丸め、
  `flac -8` で保存したもの。端（±1）に張り付いた標本は比べない。
- 段 7 の `ogg-vorbis-3ch.ogg`・`ogg-vorbis-5ch.ogg`（symphonia-codec-vorbis 0.6.1 の 3 ch 以上の不具合の回帰の試験。
  `third_party/symphonia-codec-vorbis/KURA.md`）：
  - 音（0.5 秒・44.1 kHz。t = n / 44100、n = 0〜22,049。チャンネル k は Vorbis の並びで 0 始まり）：k = 0 は 0.4·sin(2π·300·t)、
    k = 1 は 0.2·sin(2π·1000·t) + 0.1·雑音、k = 2 は 0.3·sin(2π·(200·t + 3900·t²))（200 Hz → 8,000 Hz の掃引）、k = 3 は無音、
    k = 4 は 0.3·雑音。雑音は 32 ビットの LCG（x ← 1664525·x + 1013904223、種 1。上位 24 ビットを −1〜1 に）の 1 本の列から、
    フレームごとにチャンネルの順に引く。sin は作業用のプログラムの f64（OS の libm）。
  - 参照：lewton 0.10.2 の `OggStreamReader`（作業用の `lewton-probe`。最後のグラニュールで切って 22,050 フレーム）の f32 を、
    `round(x·32768)`（偶数への丸め、−32768〜32767。端に張り付いた標本はなし）で 16 ビットにし、
    `flac -8 --force-raw-format --endian=little --sign=signed --channels=N --bps=16 --sample-rate=44100` で保存した。
  - 照らした結果（`tests/fixtures.rs` の `multichannel_vorbis_matches_lewton`）：長さはちょうど同じ、信号と誤差の比はチャンネルごとに
    3.0×10⁸〜1.0×10⁹（無音のチャンネルは無音）。直す前の symphonia-codec-vorbis 0.6.1 では失敗する。
- `ogg-vorbis-ffmpeg.ogg` は、FFmpeg 自身のデコーダだけが右のチャンネルを違う値に読む（symphonia と lewton は一致する）。
  そのため Vorbis の参照は lewton にした。
- 照らした結果（`tests/fixtures.rs`）：FLAC・ALAC・AIFF はちょうど同じ。信号と誤差の比は MP3 が 3.4〜7.4×10⁸（free format は
  1.4×10⁹）、Vorbis が 1.6×10⁹、AAC が 3.9×10⁸〜1.6×10⁹（どれも参照の 16 ビットの丸めの大きさ）。
