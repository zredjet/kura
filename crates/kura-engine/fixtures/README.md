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
