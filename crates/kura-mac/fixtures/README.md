# kura-mac のフィクスチャ

Platform のコーデック（ImageIO）のテストで使う、kura の固定の段のエンコーダや `sips` では作れない形のファイルと、
CI の仮想マシンでは書けないことがある HEIC。消さない・変えない（足すときは新しい名前で。`scripts/check-goldens-append-only.sh`
が見張る）。PNG・TIFF・BMP のふつうのものとパレットの BMP・TIFF は、テストの中で kura の PNG のエンコーダ・`sips`・
テストの書き手で作る。

作り方：`swiftc -O tools/mkfix.swift -o /tmp/mkfix && /tmp/mkfix <出力のフォルダ>`（ImageIO の `CGImageDestination` で書く。
すべてを書き直すので、足すときは別のフォルダに書いて新しいものだけを写す）。macOS 26.7（25G229）・Swift 6.4 で作った。

| ファイル | 中身（ImageIO で読んだ形） | 使うテスト |
|---|---|---|
| `heics-3x24x16.heic` | HEIC のシーケンス（`public.heics`）。3 フレーム・24×16・不透明（noneSkipLast）・sRGB。`{HEICS}` の LoopCount 1、各フレームの DelayTime 0.25・0.5・0.5 秒 | シーケンスがキャンバスいっぱいのフレームの列になる |
| `tiff-premultiplied.tif` | 関連付けたアルファの TIFF（`premultipliedLast`）。RGBA8・7×5・sRGB | プレマルチプライドを Rust の整数の式でストレートに戻す |
| `tiff-premultiplied.rgba` | 上の TIFF に書いたプレマルチプライドの画素（RGBA8 を行の順に 140 バイト） | 同上の照合の元 |
| `tiff-float.tif` | 浮動小数の TIFF（RGBA の f32・4×4・Extended Linear sRGB） | HDR・浮動小数は扱えない（`Unsupported`） |
| `heic-gradient-300x200.heic` | HEIC（`public.heic`・品質 0.9）。300×200・不透明・sRGB。画素は r = x·255/300、g = y·255/200、b = 128 | HEIC を Platform の段で読む・元に近い（PSNR）・上限・中止・壊れた入力・同時の読み取り |
| `tiff-premultiplied16.tif` | 関連付けたアルファの 16 ビットの TIFF（`premultipliedLast`・16 ビット LE）。RGBA16・8×8・sRGB。先頭の 37 画素の R は、vImage の戻し（`vImageConvert_AnyToAny` で premultipliedLast → last）が kura の式と 1 違う (c, a) の組（macOS 26.7 の vImage を乱数と総当たりで探した。mkfix.swift に表） | 16 ビットのプレマルチプライドを Rust の整数の式で戻す（vImage に任せると落ちる） |
| `tiff-premultiplied16.rgba` | 上の TIFF に書いたプレマルチプライドの画素（RGBA16 の LE を行の順に 512 バイト） | 同上の照合の元 |
| `heic-decode-hangs.heic` | `heics-3x24x16.heic` の 2 バイトを変えたもの（0x166 を 0x0c：主画像の hvcC の中、0x1128 を 0x02：moov の stsd の entry_count の上位）。開くと 24×16 の 1 枚の画像だが、展開（`CGDataProviderCopyData`）が VideoToolbox の XPC の返事を待ったまま戻らない（Apple 側の不具合。`sips` も止まる。段 9 の検証の変異のファズで見つけた） | 展開の時間切れ・中止でスレッドとハンドルを見捨てる |

## PNG のコーパス（`png-*.png`）

段 9 の調査の道具（`pngcmp`。png 0.18.1 のエンコーダと kura-engine の `testfiles::FileGen` で書いた）で作った PNG。
画素は名前から決めた種の乱数（アルファは 0・65535・乱数を混ぜる）。ImageIO（`imageio-dec/1`）と固定の段（`png-rs-dec/2`）の
画素のビット一致を照らす（`imageio_matches_png_rs_on_the_corpus`）。ICC の `p3` は macOS の `Display P3.icc`（536 バイト）、
`gray22` は `Generic Gray Gamma 2.2 Profile.icc`（4,508 バイト）。

| ファイル | 形（IHDR）と足したチャンク |
|---|---|
| `png-gray1.png`・`png-gray2.png`・`png-gray4.png` | 61×13・グレー 1・2・4 ビット |
| `png-gray2-interlaced.png` | 37×29・グレー 2 ビット・インターレース |
| `png-gray2-trns.png`・`png-gray8-trns.png`・`png-gray16-trns.png` | 32×32・グレー 2・8・16 ビット・tRNS |
| `png-rgb8-trns.png`・`png-rgb16-trns.png` | 32×32・RGB 8・16 ビット・tRNS |
| `png-pal1.png`・`png-pal2.png` | 41×17・パレット 1・2 ビット（2・3 色。tRNS なし） |
| `png-pal8.png`・`png-pal8-trns.png` | 40×30・パレット 8 ビット（200 色）・tRNS なし／tRNS（150） |
| `png-pal4-trns.png` | 41×17・パレット 4 ビット（16 色）・tRNS（5） |
| `png-pal4-interlaced.png` | 37×29・パレット 4 ビット（16 色）・インターレース |
| `png-rgba8-interlaced.png` | 37×29・RGBA 8 ビット・インターレース |
| `png-gray8-gama1.png` | 32×32・グレー 8 ビット・gAMA 1.0 |
| `png-rgb8-gama045.png`・`png-rgb8-gama045-chrm.png` | 32×32・RGB 8 ビット・gAMA 0.45455（＋cHRM） |
| `png-rgba8-gama1.png` | 32×32・RGBA 8 ビット・gAMA 1.0 |
| `png-rgb16-gama08.png` | 32×32・RGB 16 ビット・gAMA 0.8 |
| `png-rgba8-srgb.png` | 32×32・RGBA 8 ビット・sRGB |
| `png-rgb8-sbit565.png` | 32×32・RGB 8 ビット・sBIT 5/6/5 |
| `png-rgb8-icc-bogus.png` | 32×32・RGB 8 ビット・壊れた iCCP（300 バイト） |
| `png-rgba8-icc-p3.png`・`png-rgba16-icc-p3.png` | 32×32・RGBA 8・16 ビット・iCCP（p3） |
| `png-gray8-icc-gray22.png` | 32×32・グレー 8 ビット・iCCP（gray22） |
| `png-gray8-icc-p3.png` | 32×32・グレー 8 ビット・iCCP（p3。色空間に合わない） |
| `png-pal8-icc-p3.png` | 32×32・パレット 8 ビット（64 色）・iCCP（p3） |
