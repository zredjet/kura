# 編集（操作エンジン）

画像・音声の編集の正本です。実装は `crates/kura-engine`（M10）と、それを使う `kura-core`・`kura-mac`・`src-tauri`（M11 以降）。
エディタ（M12・M13）、一括加工（M14）、素材の表示の解析（M11）、持ち出しの変換（M15）は、すべてこのエンジンの上に作ります。
実装と食い違ったら、勝手に仕様を変えずに、この文書の更新案を出します（CLAUDE.md）。

## 1. 目的と範囲

- 対象は画像と音声。動画の編集は対象外（ADR-133）。ただし動画を「中身を展開しない値」として後から足せる形にしておく（§15）。
- 今後の操作・形式・機能の追加に耐える土台にする。そのために、次の 3 つを最初から約束する。
  1. **決定性**：同じ文書と同じ元からは、いつでも同じ結果ができる（§8）。
  2. **版の契約**：操作を直しても、保存済みの文書の結果は変わらない（§8.4）。
  3. **局所性**：操作を 1 つ足すのに触るファイルは 1〜2 個（§7.3）。
- エディタとの通信で送るのは文書（操作の列）だけ。画素・波形の実体は IPC で送らず、Rust が描いて `kura://` で配る（CLAUDE.md の「メディアを IPC で送らない」）。

## 2. 用語

| 用語 | 意味 |
|---|---|
| 文書（Graph） | 元に当てる操作のノードの並び。JSON で保存する |
| ノード（Node） | 操作 1 つ。操作の名前・版・引数・入力・種を持つ |
| 操作（Op） | `image.crop` のような処理の種類。版を持つ |
| 解析（Analysis） | `image.alpha` のような、値から事実を求める処理。版を持ち、結果は正規の形。操作が申告して使うこともある（§11） |
| 元 | アイテムを最初に取り込んだときのファイル。編集しても変えない |
| 今 | アイテムの今のファイル（`items.hash`）。編集していなければ元と同じ |
| 版 | 保存した 1 回分。元に対する文書と、その出力のハッシュなどを持つ |
| 値（Value） | 操作の入出力。画像・フレームの列・レイヤー・音声・クリップの列 |
| 形（Shape） | 値の型と大きさ。描かずに推論できる |
| ダイジェスト | 値をエンコードする前の中身の BLAKE3（§8.6）。`render_hash` とも呼ぶ |
| 固定のコーデック | Kura のバイナリに入っていて、版を固定した読み書き（§9） |
| アンカー | macOS・ffmpeg でしか読めない元を、読み取った結果のまま可逆形式で残したもの（§9.4） |
| レシピ | 名前を付けて保存した文書。一括加工で使う（M14） |
| セッション | エディタで 1 つのアイテムを開いているあいだの状態と、取り消しの履歴 |

## 3. 全体の構成

```text
kura-engine（OS・DB・Tauri を知らない。Linux でもテストが通る）
  doc/       文書の型・読み込み・正規の符号化・スキーマの移行
  value/     画素（CoW のタイル）・音声（ブロック）・メタデータ
  shape      形と推論
  op, ops/   操作の trait・登録・各操作（image/ sprite/ paint/ audio/ meta/）
  analysis/  解析（透過・ラウドネス・波形など。§11）
  codec/     読み書きの trait・固定のコーデック・形式の表・能力・Platform の受け口・アンカー・作り直しの記録
  render/    実行・キャッシュ・プレビューの段・範囲の描画
  session    エディタの履歴（純粋な構造）
  determinism, math, digest, error, limits
     ↑
kura-core   保存・版・GC・解析・一括加工のジョブ、DB（M11 以降）、ffmpeg の段（codec_ext::ffmpeg。段 9）
kura-mac    ImageIO・AudioToolbox によるコーデック（Platform の段。段 9）
src-tauri   コマンドと kura:// の /edit・/peaks（feature "specta" で TS の型を出す）
```

- エンジンは 1 つのクレートにまとめる。操作の旧版を凍結するとき（§8.4）、その操作が使う計算の部品も一緒に凍結できるようにするため（ADR-134）。
- specta は kura-engine の feature の中だけで使う（ADR-28 を kura-engine について置き換え。ADR-134）。

## 4. 文書

### 4.1 形

```rust
pub struct Graph {
    pub schema: u32,               // 文書の型（項目）の版。今は 3（§8.4）
    pub rules: u32,                // 作ったときの全体の規則（§6 の写し方・丸めなど）の版。今は 1（§8.4）
    pub inputs: Vec<InputSlot>,
    pub nodes: Vec<Node>,          // 位相順。入力は前のノードだけを指せる（循環しない）
    pub outputs: Vec<OutputSpec>,
    pub next_id: u32,              // ノード ID の払い出し。消しても使い回さない
}
pub struct InputSlot { pub id: InputId, pub role: InputRole, pub decode: DecodeOptions }
pub enum InputRole {
    Primary,                       // 編集・一括の対象。アイテムの元に結び付く
    Fixed { hash: Hash32 },        // 透かし・パレットの元など、中身を固定した素材
    Selection,                     // 一括の選択全体（N → 1 の結合）
}
pub struct Node {
    pub id: NodeId,
    pub op: String,                // "image.crop"
    pub v: u16,                    // 作ったときの操作の版。勝手に上げない
    pub params: RawParams,         // 保存は JSON。読み込むときにその版の型へ
    pub inputs: Vec<PortRef>,
    pub seed: u64,                 // 乱数を使う操作の種。作るときに決め、移行しても変えない
    pub enabled: bool,             // 一時的に外して見比べる（入出力の型が同じ操作だけ）
}
pub struct OutputSpec {
    pub id: OutputId,
    pub from: PortRef,
    pub format: FormatChoice,      // Keep（元と同じ形式）か To { format, options }
    pub quantize: Option<Quantize>,// 音声の整数への丸めとディザ（v・bits・dither・seed）。スキーマ 3 から
    pub expand: Expand,            // One か PerElement { name }（例 "{stem}_{index:03}"）
}
```

- 線形の操作の列は、どのノードも 1 つ前を入力にする特別な場合。エディタは出力からさかのぼった 1 本の鎖を操作のスタックとして見せ、入力が複数の操作のときだけ別の入力を選ぶ欄を出す。
- 出力が複数になる操作（スプライトの分割、マーカーでの分割）は、`Frames` や `Clips` の値を 1 つ返す。何個のファイルにするかは `OutputSpec.expand` が決める。口の数が入力によらず決まるので、同じレシピを 100 件に当てても形が崩れない。
- `DecodeOptions`（SVG のラスタ化の倍率、aseprite を合成で読むかレイヤーで読むか）は出力に効くので、文書に入れる。
- `Quantize`（段 6。`QUANTIZE_VERSION` = 1）は、音声の出力を書き出す前に整数の格子へ丸める約束：`{ v, bits（8・16・24）, dither（none・tpdf）, seed }`。s = 2^(bits−1)、x = 標本 · s（f64）、TPDF なら x に u1 + u2 − 1 を足し（u は `rng_at(seed', チャンネル, 2·フレーム)` から順に取った 2 つの u32 の (u + 0.5)/2³²、seed' = `determinism::seed(seed, 元のハッシュ, "audio.dither")`）、round_ties_even を [−s, s−1] で止めて q / s にする。WAV・AIFF・FLAC の書き出しの丸めと同じ格子なので、書き出すと q がそのまま書かれる。乱数の流れは絶対位置なので区間の切り方によらない（E18）。知らない版・範囲の外のビット数・音声でない出力は計画を作るときに断る。文書の型の項目なのでスキーマを 3 に上げた（2 → 3 は形を変えない移行）。

### 4.2 元に対する累積

- 版 n の文書は、常に「元」に対する操作の全部を持つ。前の版の出力を入力にする鎖にはしない（ADR-135）。
  - 非可逆形式（JPEG・Vorbis・MP3）で何回保存しても、劣化は元からの 1 回分。
  - 版 k を作り直すのに、ほかの版のファイルが要らない。
  - エディタは最新の文書をそのまま開ける。前の操作を直す・外すといった非破壊の編集ができる。
- 「元に戻す」「途中の版に戻す」も新しい版として足す。履歴は追記だけ。

### 4.3 レシピと継ぎ足し

- レシピは `Primary` がまだ結び付いていない文書。共有するファイル（`.kurarecipe`）は `{ kura: "recipe", schema, requires: [{op, v}], graph }`。`requires` で、読み込む前に動くかが分かる。
- 編集済みのアイテムにレシピを当てるときは、そのアイテムの今の文書の末尾にレシピのノードをつなぐ（`doc::compose(base, recipe)`）。レシピの入力（Primary 1 つ）を base の出力 0 に結び、レシピのノード ID は base の `next_id` だけずらし、種はそのまま。出力はレシピのもの。結果も元に対する累積のままで、編集の結果にレシピを当てたものと同じ値になる（E3）。
- 上書きか別名かは文書に入れず、実行するときに選ぶ。

### 4.4 正規の符号化とハッシュ

- 文書・ノードのハッシュは、自前の正規化 JSON（キーを並べる、数値は最短の表記、空白なし。RFC 8785 に準じる）の BLAKE3 で取る。非有限の小数（NaN・±∞）は正規の JSON にできないので、エラー（`Internal`）にする（段 8 で、黙って `null` にしていたのを直した。解析の結果は `Real` か理由付きの列挙で持つ。§11.1）。`serde_json::Value` の並びには頼らない（依存の feature で変わりうる）。
- 文書を DB に置くとき（M12）は、ノードを内容アドレスで分けて持つ。筆の点の多い文書を版ごとに丸ごと持つと、容量が保存回数に比例して増えるため。

## 5. 値と形

### 5.1 値

```rust
pub enum Value {
    Image(Arc<Image>),     // 1 枚の画像
    Frames(Arc<Frames>),   // アニメーション・スプライトの集まり（共通のキャンバスの中に置く。フレームごとに大きさ・位置が違ってよい）
    Layered(Arc<Layered>), // psd・aseprite のレイヤー。合成すると Image か Frames になる
    Audio(Arc<Audio>),
    Clips(Arc<Clips>),     // 分割した音声
}
```

- 将来の `Video`・`Model` は、中身を展開しない参照の値として足す（§15）。`Layered`・`Clips` は、その値を使う操作を作る段で足す。
- `Frames` は、すべてのフレームを共通のキャンバス（`canvas: Size`）の中の `offset` の位置に置く。形式・パレットはそろえる。フレームごとに大きさ・位置が違ってよい（トリムしたスプライト）。メタデータ（格子・ピボット・スライス）はキャンバスの座標。どのフレームもキャンバスいっぱいのときだけ「いっぱい」（`full`）で、フレームごとの持ち上げ（§7.1）はいっぱいのフレームの列にだけ当てる。
- `Audio` のチャンネル数は 1〜65,535。

### 5.2 画素

- アルファは常にストレートで持つ。乗算済み（premultiplied）にするのは、色を混ぜる計算の帯の中だけ。ドット絵では α=0 の画素の色も保つ必要があるため。
- 形式：`Indexed8 + Palette（256 色まで、RGBA8）`、`Gray8`、`GrayA8`、`Rgb8`、`Rgba8`、`Gray16`、`Rgba16`。チャンネル数は元のまま持つ（JPEG を RGBA にすると 33% 増えるため）。
- f32 と線形光は、フィルタ（平滑なリサイズ・任意角の回転）の行の帯の中だけで使う。画像全体を f32 にしない。
- 実体は 256×256 の CoW タイル（`Arc<Vec<u8>>`）で持つ。Indexed8 の番号はパレットの色の数より小さいこと（作るときに確かめる）。
  - ペン・塗りつぶしでは、触れたタイルだけを複製する。取り消しの履歴とキャッシュがタイルを共有するので、メモリが増えにくい。
  - プレビューは範囲（ROI）とタイル単位で描ける。
  - 操作の部品には、タイル単位と行の帯（64 行）単位の 2 つの口を用意する。
- 画素を動かすだけの操作（切り抜き・キャンバス・反転・90° 回転・最近傍の整数倍・トリム・分割・結合・描画）は、画素の形式によらないコードで書き、ビット単位で正確に動かす。Indexed8 はパレットのまま扱う。
- 色を計算する操作（平滑なリサイズ・フェード・色の置き換え）は、RGB(A) の 8/16 ビットだけを受ける。Indexed8 を渡すと、推論の段階で「RGBA8 になる」と分かるので、エディタはパレットが失われることを先に知らせられる。
- ICC は値に付けて運ぶ。v1 では色の変換をしない（sRGB 以外なら警告する）。線形光の計算は sRGB の伝達関数で行う。EXIF の向きは、デコードのときに画素へ反映して印を消す。
- 段 5 で凍結した式（詳しくはソースの冒頭）：
  - 形式の変換（`imaging::pixels`・`imaging::convert::convert_pixel`）：深さは 257 倍と `⌊(2v + 257)/514⌋`、グレーは Rec. 709 の整数の式 `⌊(2126R + 7152G + 722B + 5000)/10000⌋`（符号化された値のまま）、アルファを外すときは下地に `⌊(c·a + m·(M−a) + ⌊M/2⌋)/M⌋`。
  - 減色とディザ（`imaging::quantize`・`imaging::dither`）：メディアンカット、組織的（8×8 の Bayer。オフセットは `⌊(2B + 1 − 64)·S / 128⌋` の床で、位置はキャンバスの座標なのでトリムしたフレームでも同じ位置に同じしきい値）・誤差拡散（Floyd–Steinberg の 16 倍の整数）。アルファ 0 の画素は手を加えず誤差も運ばない。色の数が上限以下ならディザしない。色の表はフレームごとに作って併合し（作業領域は 1 フレームぶん＋色の表）、その大きさを推論で `max_value_bytes` と照らす。並べ替え・分割・割り当ての途中でも中止を確かめる。
  - リサイズ（`imaging::resample`）：最近傍は `⌊(2x+1)·in/(2·out)⌋` の整数。box・bilinear・Lanczos3 は線形光の乗算済みの f32 で、重みは f64 で求めて正規化し、横→縦の順に、元の行の小さい順に足す（タイル・帯の分け方によらない）。大きさの変わらない軸は混ぜない（重みは恒等。ただし片方の軸だけ変わるときは線形光を通るので α = 0 の色は 0 になる）。両方とも変わらないときは混ぜずにそのまま通す（Indexed8 は Rgba8 に広げるだけ）。ICC が sRGB に見えない画像を平滑なフィルタで混ぜたときは `ColorSpaceAssumed` の警告を出す（`imaging::icc::is_srgb`：説明のタグに「sRGB」を含むか）。

### 5.3 音声

- planar f32（チャンネルごとの配列）で持つ。並列のブロックは 4,096 フレーム（`determinism::AUDIO_BLOCK`。並列の分け方だけで、結果に効く区切りには使わない。§8.2）。位置は絶対フレーム（`u64`）で表す。
- int16 / int24 → f32 → 元のビット数の往復は、ちょうど元に戻る（24 ビットは f32 で正確に表せる）。手を加えていない経路の出力はビット一致させる。
- 操作は「絶対フレーム位置の関数」か「状態を順に運ぶフィルタ」のどちらかにし、区間ごとに描けるようにする。チャンクの切り方を変えても結果は同じ（E18）。
  - 段 6 から、音声を出す操作は「核」（`audio::AudioKernel`：`rate`・`channels`・`frames`・`render(ch, start, out)`）を `Op::audio_kernel` で返す。前計算（ラウドネスの計測・フィルタの表）は核を作るときに 1 度だけ行い、`render` は出力の区間を描く純粋な関数にする。`apply` は同じ核を `audio::render_all`（`AUDIO_BLOCK` ずつ `par_blocks`）で描くだけ。E18 は、登録されたすべての音声の操作の例で、核をチャンク 1・7・4,096・全体で描いた値（全体の長さ）が `apply` と同じか、レート・チャンネル数・長さが同じかを照らす。音声を出すのに核を返さない操作は失敗にする。
  - 状態を運ぶフィルタ（K 特性の IIR）は核にせず、解析の側で全体を順に流す（`analysis::loudness`）。
- 2 パスが要る操作（正規化）は、段 6 では `apply`（核を作るとき）の中で計測する。段 8 で、必要な解析を申告し、実行器が先に解く口（§11.2）を足し、正規化は内部用の解析 `audio.loudness_gating` を申告して使う形に移した。表示の `audio.loudness` と同じ関数なので、表示する値と正規化で使う値が一致する（E26）。
- 元のビット深度・サンプルレートは `AudioMeta.source` に残し、書き出しの既定値に使う。整数の PCM と f32 の変換は WAV・AIFF・FLAC で同じ式（`codec::pcm`：読みは v / 2^(幅−1)、書きは round_ties_even を範囲で止める。NaN は 0、ディザはしない）。左詰めの 32 ビットで出す FLAC・ALAC のデコーダの値も 32 ビットの式で読むと元の幅の式と同じ値になる。書き出しのビット数は、設定（`float` > `bits`）→ 元の形式 → 16 の順で決める（`codec::pcm::target`）。元の形式から決めるとき、浮動小数を書ける形式（WAV・AIFF）は元が浮動小数なら浮動小数（`float: false` を指定すれば 16 ビット）、書けない形式（FLAC）は整数の最大の幅（24）。元の幅が書けない形式では、それ以下の最大の幅にする（32 ビットの整数の元を FLAC に書くと 24）。指定した幅が書けなければ `Unsupported`。

### 5.4 形の推論

```rust
pub enum Dim { Exact(u64), AtMost(u64) }  // 中身で決まる操作（透明・無音のトリム）は AtMost
pub enum Shape {
    Image  { w: Dim, h: Dim, fmt: PixelFormat },
    Frames { count: Dim, w: Dim, h: Dim, fmt: PixelFormat, full: Fullness },  // w・h はキャンバス。Fullness は Full / Trimmed / Unknown
    Audio  { rate: u32, channels: u16, frames: Dim },
    // Layered・Clips はその値を作る段で足す
}
```

- 入力の形は、デコーダがヘッダだけ読んで出す。
- 形が分かると、描く前に、資源の上限（§13）・メモリの見積もり・保存で失うもの（アルファ・ループ点）を出せる。
- 大きさの引数は整数の比で持つ（`Ratio { num, den }`、`geom::Length::{Px(u32), Scale(Ratio), Auto}`。JSON は `{"px": n}`・`{"scale": {"num", "den"}}`・`"auto"`。Auto はもう一方の軸の倍率で縦横比を保つ）。浮動小数を使わないので丸めが一意になり、「幅の 50%」（`Scale(1/2)`）も推論の段階で入力の形から決まる。

## 6. メタデータの写し方

各操作は「座標をどう写すか」（`SpaceMap`）と「時刻をどう写すか」（`TimeMap`）を申告するだけでよい。座標や時刻を持つメタデータは、エンジンがまとめて写す。操作を書く人がメタデータを扱い忘れることがなく、新しいメタデータを足しても操作を直さなくてよい。

```rust
pub enum SpaceMap {
    Identity,
    Affine { m: IntAffine, out: Size },            // 90° 回転・反転・整数倍・平行移動（整数で正確）。out は出力の大きさ
    Scale  { sx: Ratio, sy: Ratio, out: Size },    // 任意の倍率（丸めは偶数への丸め）
    Unknown,   // 推論の段階では分からない（透明のトリム）。apply が実際の写像を返す
    Reset,     // 座標を持つメタデータを捨てる（警告を出す）
}
pub enum TimeMap {
    Identity,
    Segments { segs: Vec<Seg> },   // トリム・結合：区間ごとの平行移動と切り取り
    Rate { from: u32, to: u32 },   // リサンプル：pos' = round_half_even(pos × to / from)（整数で計算）
    Unknown, Reset,
}
```

| メタデータ | 規則 |
|---|---|
| スプライトの格子・ピボット・スライス | `SpaceMap` で写す。範囲からはみ出したものは警告を出して落とす。格子は、写したあとでセルが整数にならなければ落とす |
| ループ区間・マーカー（音声） | `TimeMap` で写し、出力の長さで詰める。一部がはみ出たら切り詰めて警告、空になったら落として警告 |
| ICC・DPI | そのまま運ぶ。保存できない形式なら落とす |
| EXIF の向き | デコードのときに画素へ反映し、印は持たない |
| そのほかの EXIF・PNG のテキスト・音声のタグ | そのまま運ぶ。書き出しの設定で残すかを決める |
| アニメのタイミング・aseprite のタグ | フレームを並べ替える・選ぶ操作が写す |
| ライブラリの情報（タグ・評価など） | 値には入れない。読むのは明示の操作か、結び付けられた引数（M14）だけ。読んだ値は版の記録に残す |

- 座標の計算はすべてあふれを確かめ、あふれたものは「範囲の外」として落とす（落ちない）。引数の座標は ±2^31 まで（`geom::COORD_LIMIT`。読み込みで確かめる）。
- 書き出し先で失うものは、エンコーダの `MetaCaps` と照らして、保存する前に一覧で出す（例：「jpg ではアルファが失われます」「mp3 ではループ点を保存できません」）。`MetaCaps` は、可逆か（`lossless`：ほかの項目の範囲の中の値ならそのまま戻るか）・透明（`alpha`：`None`・`Binary`（GIF のように 0 か 255）・`Full`）・パレット・色の数の上限（`max_colors`：GIF は 256。超えると減色）・16 ビット・ICC・EXIF・DPI・テキスト・アニメ・ループ点・マーカー・タグを持つ。形式の表の `caps` は形式として保存できるもの、エンコーダの `caps(format, options)` はそのエンコーダがその設定で実際に書けるもの（例：固定の WebP のエンコーダはアニメを書けない）で、事前の警告はエンコーダのほうを見る。
- ループ点の入れ物：WAV は `smpl`（と `cue `）、AIFF は `MARK` と `INST` の sustainLoop、Ogg・FLAC は `LOOPSTART`・`LOOPLENGTH` の Vorbis コメント（終わり = LOOPSTART + LOOPLENGTH。含まない）。どの形式も、ループ点は `start < end ≤ 長さ` のときだけ書く（範囲の外・長さ 0・逆向きは書かない。正規の作り手は作らないが、`AudioMeta` を直接組む呼び手のため）。MP3・m4a・AAC のループ点は読まない（`MetaCaps` も持たない）。FLAC はマーカーを持てない（`MetaCaps.markers` が偽なので、保存の前の一覧（上）に「マーカーを保存できません」と出る。エンコーダは黙って書かない）。

## 7. 操作

### 7.1 trait

```rust
pub trait Op: Send + Sync + 'static {
    const ID: &'static str;            // "image.crop"
    const VERSION: u16;
    type Params: OpParams;

    /// 入力の数と種類、出力、Frames などへの持ち上げ方（Lift）、乱数を使うか。
    fn signature() -> Signature;
    /// 形の推論とメタデータの写し方。描かない。
    fn infer(p: &Self::Params, inputs: &[Shape], cx: &InferCx) -> Result<Inferred, Issues>;
    /// 実際に計算する。中止の確認・乱数・警告は cx を通す。
    fn apply(p: &Self::Params, inputs: &[&Value], cx: &mut ApplyCx<'_>) -> Result<Applied, EngineError>;
    /// 実装を消した古い版 `from` の引数を、結果を変えずにこの版へ移す（§8.4）。
    fn migrate(from: u16, raw: serde_json::Value) -> Result<serde_json::Value, String>;
    /// 適合テスト（E9）とゴールデンの例。
    fn examples() -> Vec<Example<Self::Params>>;
    /// この引数で描くときの数値の性質（既定は signature().numeric）。最近傍のリサイズは Exact を返す
    fn numeric_for(p: &Self::Params) -> Numeric;
    /// 追記だけの引数（描画）の差分の適用：前の引数の出力に、追記した分だけを当てる（全体を当て直したものとビット単位で同じ）
    fn apply_appended(prev: &Self::Params, prev_out: &Value, p: &Self::Params, cx: &mut ApplyCx<'_>) -> Option<Result<Applied, EngineError>>;
    /// 音声を出す操作の核（§5.3）：前計算を 1 度だけして、出力の区間を描く純粋な関数にしたもの。apply も同じ核で描く
    fn audio_kernel(p: &Self::Params, inputs: &[&Value], cx: &mut ApplyCx<'_>) -> Option<Result<Box<dyn AudioKernel>, EngineError>>;
    /// 申告しうる解析（ID, 版）の一覧と、この引数・入力で実際に申告するもの（§11.2）。apply は cx.analysis で受け取る
    const NEEDS: &'static [(&'static str, u16)] = &[];
    fn needs(p: &Self::Params, inputs: &[Shape]) -> Result<Vec<Need>, Issues>;
}
```

- `Lift::PerFrame` と申告した画像の操作は、アニメやスプライトの集まりにもそのまま使える。透明のトリムのように「全フレームの外接の合併で切るか、フレームごとか」で意味が変わる操作は、`Lift::None` にして引数で扱う。
- `apply` が受け取る値にはメタデータも付いているが、出力のメタデータはエンジンが入力 0 のものを写して付け直す（§6）。メタデータを書く権限（`MetaAccess::Write`）を申告できるのは、メタデータを設定・消す操作（`meta.set_grid`・`meta.set_pivot`・`audio.set_loop`）と、値の種類を変えて格子を書く・消す操作（`sprite.split_grid`・`sprite.pack_grid`）だけ。Write の操作が座標を持つメタデータを写すときは、`ApplyCx::rules`（文書の全体の規則の版）を `mapping::map_image_meta_for` に渡して、文書の規則の写し方を使う（規則の版を上げても、古い文書を古い写し方で描けるように）。フレームに持ち上げる操作は書く権限を持てない（登録で断る）。
- 持ち上げのとき、各フレームはフレームの列のメタデータを持った 1 枚の画像として渡す（単体の画像と同じ見え方）。持ち上げられるのは、キャンバスいっぱいのフレームの列だけ（推論で `Trimmed` なら計画の段階で、`Unknown` なら実行のときに確かめて断る）。出力の大きさが決まらない（`AtMost`）入力でも、出力の大きさが入力で決まる操作なら持ち上げられ、写し方は実行で決まる。入力の大きさが決まっているのに出力が `AtMost` になる操作（中身で大きさが決まる）は、フレームごとに大きさが変わりうるので持ち上げない。
- `Applied` の `space`・`time` は実際の写し方。推論が `Unknown` なら必ず返し、そうでなければ返さないか推論と同じものを返す。実行器が確かめ、食い違えば内部の誤りにする（E9）。

### 7.2 引数の記述

- `OpParams::schema()` が、引数ごとの名前（serde のフィールド名と一致）・種類（整数・小数・真偽・列挙・矩形・点・大きさ・比・長さ（`Length`）・格子（`Grid`）・色・時刻・色の対応表（`PaletteEntries`）・アイテム・筆の点の列（`Strokes`））・範囲・刻み・単位（px・%・度・dB・LUFS・ms・サンプル・Hz・倍）・既定値・表示の条件を返す。
- 範囲の外かどうかは schema から自動で調べる。`check` には意味の検証（矩形が入力の中にあるか）だけを書く。
- エディタの汎用の引数パネルは schema から組む。切り抜きの枠・波形のハンドル・ペンなど専用の UI が要るものだけ、操作の ID ごとに部品を差し替える。文言は `src/i18n/ja.ts` に置き、schema の名前をキーにする。

### 7.3 登録

```rust
// crates/kura-engine/src/ops/mod.rs — 操作を足すときは、ここに 1 行足すだけ
register_ops! {
    current: [ image::Crop, image::Flip, audio::Gain, … ],
    legacy:  [ /* 結果を変えた旧版。新しく作れないが、作り直しには使う（§8.4） */ ],
    retired: [ /* なくした操作の最後の版。カタログに出さず新しく作れないが、作り直しには使う */ ],
}
```

- マクロが作るもの：`(id, v)` から実装を引く表（legacy・retired を含む）、今の版の引数のタグ付き和（specta で TS の判別共用体になる）、UI 用のカタログ（current だけ）。
- 新しい操作は `ops/<分類>/<名前>.rs` を 1 つ作り（テストと `examples()` を含む）、表に 1 行足す。適合テスト（E9 ほか）は自動で当たる。ゴールデンは行を足す。

### 7.4 操作の一覧（M10 で作るもの）

| 分類 | 操作 |
|---|---|
| 画像の幾何 | `image.crop`・`image.canvas`・`image.trim_alpha`・`image.flip`・`image.rotate90`・`image.resize`（最近傍・box・bilinear・Lanczos3） |
| 画像の変換 | `image.convert`（画素の形式・減色・ディザ）・`palette.replace` |
| スプライト | `sprite.split_grid`・`sprite.pack_grid`・`layers.flatten` |
| 描画 | `paint.actions`（§10） |
| メタデータ | `meta.set_grid`・`meta.set_pivot`・`audio.set_loop` |
| 音声 | `audio.trim`・`audio.fade`・`audio.gain`・`audio.normalize`・`audio.resample`・`audio.mono` |

- 段 5 で、画像の幾何・変換・スプライト（`layers.flatten` を除く）・描画・`meta.set_grid`・`meta.set_pivot` を入れた。各操作の約束はソースの冒頭（`crates/kura-engine/src/ops/<分類>/<名前>.rs`）に書く。
  - `image.canvas`：`rect` は新しいキャンバスを入力の座標で表す。`fill` が null（透明）なら、アルファのない形式はアルファ付きになる（広げる部分がなくても。形は引数と入力の形式だけで決まる）。Indexed8 はパレットの透明な色・同じ色を使い、なければ後ろに足す（256 色でいっぱいなら `Unsupported`）。`sprite.pack_grid` の `fill` も同じ。
  - `image.trim_alpha`：`threshold` 以下のアルファを透明とみなす。全部が透明なら左上の 1×1（ADR-147）。フレームの列は `union`（合併でキャンバスごと切る）か `perFrame`（キャンバスを保ち、各フレームを切って位置を持たせる）。`union` で合併と重ならないフレーム（全部が透明とみなす画素）は、そのフレームの左上の 1 画素を新しいキャンバスの左上に置く（パレットを変えない）。中身で大きさが決まるので持ち上げない。
  - `image.resize`：幅・高さは画素・比・自動（縦横比を保つ）。大きさが決まらない入力の自動の軸は、もう一方の軸から求めた上限（`max_dim` で切る）を推論にする。
  - `image.convert`：フレームの列は 1 つのパレットにそろえるので、持ち上げずに全フレームを扱う。
  - `sprite.split_grid`：格子は引数で渡す（推論で数が分かるように。エディタはメタデータの格子を初期値にする）。`skipEmpty` で透明なセルを除く。メタデータは最初のセルの座標に写し、格子は消す。`sprite.pack_grid`：キャンバスをセルにして並べ、格子を書く。分割してから同じ格子で結合すると元に戻る（E14）。
- `layers.flatten` は、レイヤーの値（`Layered`）を出すデコーダを作る段で足す（今の aseprite・PSD のデコーダは合成済みの画像を出す）。
- 段 6 で、`audio.trim`・`audio.fade`・`audio.mono`・`audio.set_loop`・`audio.normalize`・`audio.resample` を入れ、`audio.gain` を核に書き直した（値は同じ。ゴールデンは変わらない）。約束はソースの冒頭に書く。
  - `audio.trim`：`[start, end)`（フレーム。end が null なら終わりまで）。標本はそのまま写す（整数だけ）。ループ点・マーカーは `TimeMap::Segments`。空・長さを超える区間は断る。
  - `audio.fade`：`fadeIn`・`fadeOut`（フレーム）と `curve`（`linear`・`equalPower` = sin(πt/2)）。フレーム i の倍率は、イン c(i/a)・アウト c((N−1−i)/b) の積。長さ 0 は何も変えない（E17）。長さを超えるものは断る。
  - `audio.gain`：`(f64(x) · 10^(dB/20)) as f32`。0 dB は何も変えない。
  - `audio.mono`：チャンネルの順に f64 で足してチャンネルの数で割る。1 チャンネルはそのまま。
  - `audio.set_loop`：`loop`（`{start, end}`、end は含まない、0 ≤ start < end ≤ 長さ）か null（消す）。メタデータを書く操作。
  - `audio.normalize`：`target`（−69〜0 LUFS、既定 −23。ゲートは厳密な「>」なので −70 ちょうどには届かない）と `peakLimit`（−30〜0 dBTP、任意）。利得は、利得をかけたあとの積分ラウドネスがちょうど目標になる値で、`audio.gain` と同じ式で当てる（同じ dB のゲインとビット単位で同じ）。利得をかけると絶対ゲートをまたぐブロックが入れ替わるので、1 回の計測の差（目標 − 積分ラウドネス）では届かないことがある（静かな区間がゲートを越える入力で 1.6〜3.3 LU ずれた）。そこで計測したブロックのパワーを大きい順に並べ、上から m 個が絶対ゲートを通る利得の区間ごとに、相対ゲートは利得によらないので積分ラウドネスが「利得 + 定数」の直線になることを使い、区間の中に入る解を厳密に求める（`Measured::gain_to`。O(n log n)。|利得| が最小の解）。ブロックが 1 つでもあれば、−70 LKFS より静かな入力も含めて −69〜0 のどの目標にも届く（ChaCha8 で作った多区間の入力と、反復では届かなかった 81 段の階段の形で確かめる）。トゥルーピークが上限を超えるなら上限にちょうど届くまで下げて `GainLimited` の警告、計れない（ブロックのパワーがすべて 0（無音）・4 区間（⌊(fs + 5)/10⌋ 標本の 4 倍。約 400 ms）に満たない・3,363 Hz 以下・NaN や ∞ の標本）ときは何も変えずに `LoudnessUnmeasurable` の警告。
  - `audio.resample`：`rate`（1,000〜768,000 Hz）。多相 FIR（§8.3）。出力の長さは round_half_even(frames · to / from) で、ループ点・マーカーは `TimeMap::Rate`。同じレートなら何もしない。係数の表が `min(max_value_bytes, 256 MiB)` を超える組み合わせと、出力が 0 フレームになる入力は断る。

## 8. 決定性と版

### 8.1 保証の段

| 段 | 保証すること | 条件 |
|---|---|---|
| R1（値） | 同じ文書・同じ元なら、エンコード前の値のダイジェストが永久に一致する | aarch64-apple-darwin で、固定のコーデックだけを通る |
| R2（ファイル） | 出力ファイルのバイト列も一致する | R1 に加えて固定のエンコーダ |
| 整数だけの操作 | すべてのターゲット（Linux を含む）で一致する | CI の ubuntu で確かめる（ADR-142） |
| 浮動小数の操作 | ゴールデンをターゲットごとに持つ | — |
| 環境依存 | 一致を約束しない（同じ macOS でも HE-AAC は実行ごとに値が揺れた）。コーデックの識別子と環境の版（`DecodeStamp.env`・`EncodedStamp.env`）を記録し、読んだ元にアンカーを作り（§9.4）、画面に「環境に依存」と出す | ImageIO・AudioToolbox・ffmpeg を通るとき |

- 版の作り直しの照合（§8.6）は値のダイジェスト（R1）で行う。可逆形式（PNG・WAV・FLAC・GIF）は、中身が同じならバイト列の違いで作り直しを失敗にしない。
- バイト列（R2）はコーデックのゴールデン（§8.5）で見張る。依存の更新などで固定のエンコーダのバイト列が変わったら、エンコーダの約束の版を上げる（同じ版のまま黙って変えない）。デコーダが読む値（R1）は変えない（§8.4）。

### 8.2 規則（`crates/kura-engine/clippy.toml` で機械的に強制する）

1. 超越関数（`powf`・`exp`・`ln`・`log10`・`sin`・`cos` など）は `kura_engine::math` だけを使う。中身は `libm` クレート（版を `=` で固定）。OS の libm は版や OS で結果が変わるため。`sqrt`・`mul_add`・`round_ties_even`・飽和する `as` の変換は IEEE で一意なので使ってよい。
2. 並列は `determinism::par_blocks`（ブロックの添字の順に集める）だけを使う。rayon のほかの口（`into_par_iter`・`par_iter`・`par_chunks`・`join`・`scope`・`spawn`・`for_each`、`sum`・`reduce`・`fold`・`try_*`・`min_by`・`max_by`・`find_any`・`par_bridge`、`current_num_threads` など）と `std::thread::spawn`・`scope` は使わない（`par_blocks` の中の 1 か所だけ許す）。
   - 並列の分け方（`TILE`・`BAND_ROWS`・`AUDIO_BLOCK`）は速さのためだけの値で、変えても結果が変わらないように書く。
   - 浮動小数の総和は `determinism::sum_blocks` で取る：先頭から `SUM_BLOCK`（4,096 要素。**凍結**）ずつの区間の部分和を区間の順に足す。区間の切り方は結果に効くので、並列の分け方とは別の定数にしてある。変えるなら使う操作の版を上げる。`sum_ordered` は、ほかの決まった区切りで取った部分和を順に足すときに使う。
   - 浮動小数の `max`・`min` は `math::max`・`min`（IEEE 754-2019 の maximumNumber・minimumNumber。NaN は捨て、-0 < +0）を使う。std の `f64::max`・`min` は ±0 のどちらを返すかが決まっていない。
   - 等しい要素の扱いが std の版で変わりうる `sort_unstable*`・`select_nth_unstable*`・`binary_search*` は使わない（安定な `sort`・`sort_by`、`contains` などを使う）。
3. 乱数は ChaCha8 だけ。種は `determinism::seed(node.seed, 元のハッシュ, 用途)`（kura-core の `splitmix64`・`fnv1a64` と同じ値）で作り、鍵は `key_from_seed`（splitmix64 で 32 バイトに広げる。rand_core の `seed_from_u64`・`from_rng` は禁止）。乱数の流れは**絶対位置**で決める（`rng_at`：画像は行、音声はチャンネルを流れにして、列の中の位置へシークする）。タイル・ブロック・フレームの番号を流れに使わない。アニメのフレームはどれも同じ乱数になる（ディザのちらつきを防ぐ）。種がノード ID・画面の操作の順・一括の並びに依存しないので、エディタと一括で同じになる（E4・E5）。
4. 出力に効く場所で `HashMap`・`HashSet`・`RandomState`・`DefaultHasher` を使わない（`BTreeMap` か並べた `Vec`）。
5. 描画の中で時刻・環境変数・引数（`env::var*`・`vars*`・`args*`）を読まない。`#![forbid(unsafe_code)]`。
6. 結果に効く依存は Cargo.toml で `=` で版を固定する（libm・blake3・rand_chacha、コーデックの png・zune-jpeg・jpeg-encoder・gif・image-webp・resvg・miniz_oxide、段 6 の symphonia の部品のクレート（Vorbis は `third_party/` の直した写し（§9.7）、段 9 から core・AAC・Vorbis は超越関数を libm に差し替え、Vorbis と Ogg は `lookup1_values` を整数で求めるようにした写し（§9.8））・nanomp3・md-5）。段 7 の Ogg Vorbis の表はワークスペースの `vorbis-tables`（生成物を SHA-256 で凍結）。上げるのは、ゴールデンが通るときだけ。SIMD の経路がターゲットで値を変えうる依存は、SIMD の feature を外す（zune-jpeg の neon・x86、jpeg-encoder の simd、symphonia-core の opt-simd、nanomp3 の wide。md-5 は FLAC の MD5 を書くだけで値は実装によらない）。ほかの依存（dev 依存を含む）がついでに有効にしていないかを `scripts/check-engine-features.sh` が確かめ、CI でも回す。serde_json は固定しない（正規の JSON は自前で書き、読み込みは `float_roundtrip` で正しく丸めるので、結果は版によらない）。

### 8.3 数値計算は自前で書く

ラウドネス（ITU-R BS.1770-4 とトゥルーピーク）・サンプルレート変換（窓付き sinc の多相フィルタ）・リサイズの重み・ディザ・減色は、`math` を通して自分で書く（ADR-138）。第三者のクレート（ebur128・rubato・fast_image_resize など）は OS の libm を呼ぶことがあるので、dev 依存の照合相手とベンチの比較にだけ使う。

段 6 で凍結した式（詳しくはソースの冒頭。ADR-148）：

- **ラウドネス**（`analysis::loudness`）：K 特性は 2 次の直接形 I を 2 段、状態は f64。係数は libebur128 の式（`math::tan` で事前に歪めた双一次変換）で、2 段目の分子に `d(48 kHz) / d(fs)` を掛けて、どのレートでも BS.1770 の 48 kHz の表と同じ周波数特性にする（libebur128 は掛けないので 192 kHz で 0.03 dB ほど違う）。ゲーティングは S = ⌊(fs + 5)/10⌋ 標本ずつの二乗和の区間から 400 ms のブロックを `((q0 + q1) + q2) + q3` の順に作り、−70 LKFS と −10 LU のゲートをエネルギーで厳密な「>」で比べる。チャンネルの重みはチャンネルの数で決める（L・R・C は 1、4・5・6 チャンネルのサラウンド（Ls・Rs）と 8 チャンネルの側方は 1.41、8 チャンネルの後方は 1、LFE は計らない）。標本に NaN・∞ があれば、どの値も NaN（計れない）。トゥルーピークは Annex 2 の 48 タップ（12 タップ × 4 相）で、どのレートでも 4 倍にして、サンプルピークとの大きいほう。EBU Tech 3341 の試験信号 1〜6（と 44.1 kHz）で ±0.1 LU、トゥルーピークの 15〜19 で +0.2／−0.4 dB に入ることを確かめる（E19）。
- **サンプルレート変換**（`audio::resample`）：g = gcd(from, to)、L = to/g・M = from/g の多相 FIR。タップは K = 256 · from / min(from, to)（偶数に切り上げ）、遮断 0.963 × 低いほうのナイキスト、Kaiser 窓 β = 14.8（I0 は級数）。係数は相ごとに和で割って直流をちょうど通す。積和は f64 で k の順、最後に 1 回だけ f32 に丸める。位置は u128 で正確に。入力の範囲の外は 0。阻止域は −142 dB 以下、−0.1 dB の点は 0.942 × ナイキスト。係数の表はダイジェストで凍結している（`table_is_frozen`）。

### 8.4 版の方針

約束の単位は `(操作 ID, 版)`。同じ版・同じ引数・同じ入力なら、永久に同じ出力を返す。

| 変えたいもの | やること | 古い文書を作り直すと |
|---|---|---|
| 速くする（結果は同じ） | そのまま変える。ゴールデンと、遅い参照実装との差分テストが通ること | 同じ |
| 結果を変える（不具合の修正・改良） | 操作の版を上げ、旧版の実装を `legacy` に残す。旧版は新しく作れないが作り直しには使う。エディタは「古い版の操作です。更新すると結果が変わります」と出し、利用者が選んだときだけ書き換える | 旧版の実装で同じ |
| 引数の形だけ変える | 版を上げて `migrate` を書く。旧版のゴールデン（例の引数・入力も凍結してある）を移行して描くと旧版の値になるなら、旧版の実装は消してよい（ゴールデンのテストが自動で確かめる） | 同じ |
| 全体の規則（メタデータの写し方・丸め・量子化など、どの操作にも効く約束） | `doc::RULES` を上げ、古い規則の実装も残す。描画（`render`）は文書の `rules` で写し方を選ぶ。新しい文書は今の規則で作る。操作のゴールデンには今の規則の行を足し、古い規則の行も照らし続ける | 古い規則で同じ |
| 文書の型の項目 | 文書の `schema` を上げ、`SCHEMA_STEPS` に移行を 1 つ足す（形だけを移す。`rules` は変えない）。文書の型は知らない項目を受け付けないので、項目を足したら必ず上げる | 同じ |
| 固定のエンコーダのバイト列（依存の更新を含む） | エンコーダの約束の版を上げる（`png-rs-enc/1` → `/2`）。旧版は残さなくてよい（照合は値で行う。§8.1） | 値は同じ |
| Platform・External のデコーダの変換の約束 | 変換の約束を変えたら識別子の版を上げる（`imageio-dec/1` → `/2`、ffmpeg は引数のゴールデンが見張る）。旧版は残さない（作り直しはアンカーから読む。§9.4） | アンカーで同じ |
| 固定のデコーダが読む値 | 変えない。どうしても変えるなら、デコーダの約束の版を上げ、旧版を `CodecSet::add_legacy_decoder` で残す。作り直しは記録した識別子のデコーダで読む（`decode_with`） | 旧版のデコーダで同じ |
| 操作をなくす | なくさない。最後の版を `retired` に移す（カタログに出さず新しく作れないが、作り直しには使う） | 作り直せる |
| 解析の結果を変える | 解析の版を上げる。操作が申告する解析は旧版を `legacy` に残し、申告する操作の版も上げる（申告は操作の版ごとに固定する。今の版の操作は解析の今の版を申告していなければならず、`Engine::new` が確かめる）。表示だけの解析は旧版の実装を消して `dropped` に載せてよい（§11.1） | 操作が使う解析は旧版で同じ |

- エンジンの版（semver）は記録するだけで、作り直しに使う実装の選択には使わない。選ぶのは `schema`・`rules`・`(op, v)`・コーデックの識別子（`"png-rs-dec/1"` のように読み書きを分け、約束の版を含む）だけ。
- 文書のスキーマの移行は、表（`doc::SCHEMA_STEPS`）の関数で 1 段ずつ行う（スキーマ n → n+1。上げたら 1 つ足し、消さない）。スキーマ 1 → 2 は `rules: 1` を足す。
- 引数は、版の解決（`Registry::resolve`）の前に正規の JSON に往復させる。`migrate` には正規の形の引数が渡り、戻り値も正規の形に直す（メモリ上の `5.0` と読み直した `5` で、移行の結果が分かれない）。
- 実装のない古い版は、すぐ上の実装のある版（旧版・退役した版のこともある）の `migrate` で移し、その版の実装で描く。v1（消した）→ v2（旧版）→ v3（今）のように、旧版を経由する鎖も表せる。「新しすぎる版」は、実装のある版（旧版・退役した版を含む）の最大より上の版。
- 未知の操作・新しすぎる版・移せない版・新しすぎるスキーマ・知らない規則の版・知らない項目は、読み込み（`Engine::load`）でエラーにする（E8）。

### 8.5 ゴールデン

- `crates/kura-engine/goldens/<操作>/v<版>.jsonl` に、1 行に 1 つ、正規の JSON で `{"digest","inputs","name","params","rules","target"}` を並べる（`rules` は描いたときの全体の規則の版。1 のときは書かない）。ファイル名は `v<版>.jsonl` ちょうど（`v01` などの別名は失敗）。例の引数と入力の作り方（`testgen`。市松模様、乱数の画素、スプライトシート、乱数の音声など）も凍結するので、実装を消して移行した版（E7）や旧版（E6）も、今の登録表で解決して描き、凍結した値と照らす。バイナリのフィクスチャは原則として置かない（例外は下の「フィクスチャ」）。
- `target` は、整数だけの操作（`Numeric::Exact`）は `*`（すべてのターゲットで同じ値）、浮動小数を使う操作は `aarch64-macos` など。数値の性質は例の引数ごとに `numeric_for` で決まる（`image.resize` の最近傍の例は `*`、平滑なフィルタの例はターゲットごと）。浮動小数の操作の行は主なターゲット（aarch64-macos）にだけ必須で、ほかのターゲットでは、あれば照らす。数値の申告と行のターゲットが食い違えば失敗にする（申告を変えて照合を逃れられないように）。
- **ゴールデンは追記だけ**。既存の値を書き換えたら、その変更は通さない（マイグレーションと同じ扱い。CLAUDE.md の「やってはいけないこと」）。`scripts/check-goldens-append-only.sh` が基準のコミットからの差分で消えた・変わった行を見つけ、CI でも回す。
- 足りない行は `KURA_BLESS=1 cargo test -p kura-engine --test golden` で追記できる（既存の行は変えない。すべての操作を照らし終えてから、どこにも食い違いがないときだけ追記する）。例を消す・変える、同じ例・ターゲット・規則の行が 2 つある、今の登録表で描けない版のファイルがある、のどれも失敗にする（例は変えずに、新しい名前の例を足す）。
- 追記だけの検査（`scripts/check-goldens-append-only.sh`）は、改名（`git mv`）も旧いパスの削除として見て、ファイルを消した・改名したら失敗にする。
- bless は今の出力をそのまま凍結するだけなので、凍結する前に、画素や標本を直接確かめるテストで正しさを確かめておく。
- **作り直しの見本集**（§8.6 の 1）：`goldens-replay/replay.jsonl` に、複数のノードの文書（保存した JSON）・入力の作り方・値のダイジェストを凍結し、`Engine::load` で読み直して描いて照らす（`tests/replay.rs`）。凍結した `doc` が正本で、凍結したときのスキーマのまま読む（スキーマ 1 の見本はスキーマの移行を通る）。見本の定義（`examples()`）は、凍結した文書を今のスキーマへ移したものと一致すること、まだ凍結していない見本を足すことだけに使う。スキーマを上げたら、今のスキーマの見本を新しい名前で足す。`goldens-replay/` には `replay.jsonl` のほかのファイルを置けない。
- **コーデックのゴールデン**（`tests/codec_golden.rs`）：`goldens-codecs/<コーデックの ID（/ は @）>.jsonl`。ID は読み取り（`-dec`）と書き出し（`-enc`）で別にする（片方の版上げで、もう片方の照合が外れないように）。
  - エンコーダ（R2）`png-rs-enc@1.jsonl`：書いたバイト列のダイジェスト（`file`）と、それを読み直したデコーダの ID（`decoder`）・値のダイジェスト（`value`）。読み直しは、記録したデコーダ（旧版を含む）で照らす。バイト列が変わったら、エンコーダの約束の版を上げる。今ないエンコーダのファイルは照らさない（版を上げた旧版）。依存のクレート（png・fdeflate・flate2・miniz_oxide など）を上げてバイト列が変わったときも、ここで気づく。読み直しのデコーダが値の再現を約束しない行は `value` を持たず、書いたバイト列と、読み直した形（長さ・レート・チャンネル）だけを照らす。段 7・8 の `ogg-vorbis-kura-enc@1.jsonl` の 35 行は、そのころ再現できなかった symphonia の Vorbis（`/2`）で読み直す行で、段 9 からは後の版（`/3`）で形だけを照らし（`SUPERSEDED`）、`/3` で読み直した値は新しい名前の行（`<名前>-dec3`）が凍結する（ファイルは 70 行）。再現できるデコーダなのに `value` がない行、再現できないデコーダなのに `value` がある行は失敗にする。自作の Ogg Vorbis のバイト列は浮動小数の計算で決まるが、照合で aarch64 と x86_64 の一致を確かめたので、ほかのターゲットでも同じ行を照らす。CI は照合用の C なしで回るので、`ogg-vorbis-kura-enc@1.jsonl` は kura の範囲で選ばれる 16 のテンプレート（8 つのレートの族 × stereo・uncoupled。X は 96・192 kHz）と 9 レートそれぞれの stereo・uncoupled・1〜8 チャンネル・q −1 と 10・26 kHz 以上の衝撃（`bursts`。aoTuV の M3）・大きくなっていくトーン（`swell`。M9）を短い長さで覆い（段 8 までに 35 行。44.1 kHz・品質 0 の衝撃の 2 行は aoTuV の M2 にも届く。段 9 で `-dec3` の 35 行が加わった）、`ogg_templates_are_covered` が覆いの抜けを失敗にする（心理モデルの係数・M2・M3・M9 の定数を 1 つ変えると、2〜8 行が落ちることを確かめた。M2 の前の判定（`postnoise_detection` の閾値）の変更には、照合のテストを含めてまだどのテストも気づかない）。
  - デコーダ（R1 のうちデコーダだけの経路）`png-rs-dec@1.jsonl`：下の層で組んだファイル（`testfiles::FileGen`：1/2/4 ビット・tRNS・インターレース・64 ビットの浮動小数・拡張形式・他のツールのメタデータ・大きさの分からない data など）を読んだ値のダイジェスト。凍結した行は、その ID のデコーダ（今の版か旧版）で読んで照らし、その ID のデコーダがなければ失敗にする。
  - どちらも追記だけの検査の対象。再現を約束しないデコーダ（`reproducible()` が偽。SVG と Platform・External）は、デコーダのゴールデンに入れない（入れようとすると失敗にする）。それらは他のデコーダの読み取りとの照合（`tests/fixtures.rs`）で確かめ、編集するときはアンカーを作る（§9.4）。
- **解析のゴールデン**（`goldens-analysis/<ID>/v<版>.jsonl`。§11.5）：例ごとに正規の結果（2 KiB 以下のとき）とダイジェスト。追記だけの検査の対象。
- **共有のテストベクタ**（`crates/kura-engine/tests/vectors/`）：Rust と TS で同じ規則を実装するもの（描画の筆・ピクセルパーフェクト・点の列の詰め方・重ねる式）の入出力を、エンジンが JSON で凍結する（`tests/paint_vectors.rs`。足りなければ `KURA_BLESS=1`）。フロントは `src/lib/paint.test.ts` が同じファイルを読んで照らす。フィクスチャと同じく、消す・変えるのを認めない（追記だけの検査が見る）。
- 入力の作り方（`testgen::InputGen`）は凍結する（例の値が変わるため）。足すだけ。段 7・8 で、`bursts`・`swell`（Ogg Vorbis のゴールデン）、`upscale`・`crop`・`spriteSheet`・`swatches`（`count` 色を `clear` 未満の番号だけアルファ 0 で）・`pixels`（画素を 1 つずつ）・`nonFinite`（指定の位置の標本を NaN・±∞ に）を足した。NaN を含む入力の値のダイジェストは、§8.6 のとおり NaN のビット列をそろえるので安定する。
- **フィクスチャ**：自前のエンコーダでは作れない形（非可逆の WebP、libjpeg のプログレッシブ、他のツールの PSD など）は、他のツールで作った小さなファイルを `crates/kura-engine/fixtures/` に置く（作り方・道具の版は fixtures/README.md）。`tests/fixtures.rs` が他の実装の読み取り（sips・dwebp・djpeg・ImageMagick の PNG）と照らし、`FileGen::Fixture` でデコーダのゴールデンにも入れる。フィクスチャは消す・変えるのを認めない（追記だけの検査が見る）。

### 8.6 ダイジェストと作り直しの確認

- 値のダイジェスト（`Value::digest`。凍結する）：BLAKE3 に、ドメイン（長さ u64 ＋文字列）に続けて次を書く。数はすべて固定の幅のリトルエンディアン、可変長のものは長さ（u64）を前に付ける。
  - 画像 `kura-img-v1`：幅 u32 ‖ 高さ u32 ‖ 形式の番号 u8 ‖ パレット（あれば 1 ‖ 色の数 u32 ‖ RGBA の並び、なければ 0）‖ 行を上から詰めた画素 ‖ 正規の JSON のメタデータ（長さ付き）‖ ICC（あれば 1 ‖ 長さ付きのバイト列、なければ 0）‖ 拡張。
  - 拡張は、あるものだけを番号の順に（番号 u8 ‖ 長さ付きのバイト列）書く。ないときは何も書かないので、拡張を足す前の値のダイジェストは変わらない。番号 1 は EXIF。メタデータの JSON の項目を足すときも、空なら書かない（`#[serde(skip_serializing_if)]`）ようにして、前の値のダイジェストを変えない（例：アニメのタグ `tags`）。
  - フレームの列 `kura-frames-v1`：フレームの数 u32 ‖ 繰り返し u32 ‖ キャンバスの幅・高さ u32 ‖ 各フレーム（表示時間 u32 ‖ 位置 x・y i64 ‖ 画像と同じ画素の部分）‖ メタデータ ‖ ICC ‖ 拡張。
  - 音声 `kura-pcm-v1`：レート u32 ‖ チャンネル数 u32 ‖ フレーム数 u64 ‖ 各チャンネルの f32 のビット列（NaN は 0x7fc00000 にそろえる。-0.0 と +0.0 はそろえず、別の値として扱う）‖ 正規の JSON のメタデータ（長さ付き）。
  - 画素の並び・ダイジェストはタイルの分け方によらない。式は `value::tests::digest_formula_is_frozen` で凍結している。
- 版ごとに、値のダイジェスト・元のデコードのダイジェスト・使ったコーデックの識別子を記録する。
- 作り直しは 3 重に確かめる。
  1. CI：操作ごとのゴールデン（`goldens/`）、作り直しの見本集（`goldens-replay/`）、コーデックのゴールデン（`goldens-codecs/`）を、macOS と ubuntu で流す。
  2. 保存のあと：置き換えたファイルを消す直前に、その版を文書から作り直して照合する。一致しなければ、その版のファイルを残す（ADR-135）。
  3. `kura-devtools edit audit --library <path>`：実際のライブラリのすべての版を作り直して照合する。

## 9. コーデックと形式

### 9.1 trait と段

```rust
pub enum Tier { Pinned, Platform, External }
pub trait Decoder: Send + Sync {
    fn id(&self) -> &'static str;   // "png-rs-dec/2"（約束の版を含む。エンコーダは "png-rs-enc/1"）
    fn tier(&self) -> Tier;
    fn reproducible(&self) -> bool { self.tier() == Tier::Pinned }   // どの環境でも同じ値を読むか（偽ならアンカー。§9.4）
    fn can_decode(&self, format: &str) -> bool;
    fn shape(&self, src: &mut dyn ReadSeek, format: &str, o: &DecodeOptions, limits: &Limits) -> Result<Shape, EngineError>;  // ヘッダだけ
    fn decode(&self, src: &mut dyn ReadSeek, format: &str, o: &DecodeOptions, limits: &Limits, mon: &dyn Monitor) -> Result<Value, EngineError>;
}
pub trait Encoder: Send + Sync {
    fn id(&self) -> &'static str;
    fn tier(&self) -> Tier;
    fn can_encode(&self, format: &str, s: &Shape) -> bool;
    fn options(&self, format: &str) -> ParamSchema;
    fn caps(&self, format: &str, o: &serde_json::Value) -> MetaCaps;   // この設定で実際に保存できるもの（§6）
    fn encode(&self, v: &Value, format: &str, o: &serde_json::Value, out: &mut dyn Write, mon: &dyn Monitor) -> Result<u64, EngineError>;
}
```

- `CodecSet` が形式ごとに `Pinned → Platform → External` の順に試し、扱えないと答えた（`shape` か `decode` が `Unsupported`）ものは次の段へ回し、実際に読んだものを記録する（`MediaBackend` の `ChainBackend` と同じ考え方。例：固定の WAV のデコーダが扱わない IMA ADPCM は macOS に回す）。書き出しの設定は、正規の JSON に直してからエンコーダの記述（`options()`）で厳しく確かめる（知らないキー・型の違い・範囲の外はエラー。黙って既定に落とさない）。確かめた設定は `EncodedStamp.options` に残す。読むときは、ヘッダの形を上限で確かめてから中身を読み、中身の形がヘッダと合うかも確かめる。読み取りの記録（`DecodeStamp`：コーデック・段・再現できるか・値のダイジェスト）と書き出しの記録（`EncodedStamp`：コーデック・段・形式・バイト数・ファイルのダイジェスト）を版の記録に使う。第三者のコーデックの呼び出しは `codec::contain` で panic を受け止める。
- 読む値を変えたデコーダの旧版は `CodecSet::add_legacy_decoder` で残す。ふつうの読み取りでは選ばず、作り直しで記録の識別子（`DecodeStamp.codec`）から `decoder_by_id`・`decode_with` で選ぶ。
- 組み立て：`CodecSet::pinned()`（固定のコーデックと旧版）か `CodecSet::pinned_with(&PinnedOptions { svg_fonts })`（kura-mac が OS のフォントを渡す。渡さなければ文字を含む SVG は `Unsupported`）に、kura-mac・ffmpeg の段を `add_decoder`・`add_encoder` で足す。識別子はデコーダ（旧版を含む）・エンコーダのそれぞれで重ならないこと（重なれば `Err`。記録の識別子から 1 つに決まらなくなるため）。`all_decoders_for(format)` は旧版を含めて、その形式を読めるデコーダを返す（ファズ・照合のため）。
- WAV のメタデータ（cue・labl・INFO）は、数をマーカーの上限で、大きさを `max_meta_bytes` で抑える。1 項目ごとに文字の長さに 32 バイトを足して予算から引く（空の名前でも数だけで予算を使い切れるように）。cue を先に読んでいれば、cue にない ID の labl は予算を使わずに捨てる。
- 段 2 で入れたもの：PNG（`png` クレート `=0.18.1`。1/2/4 ビットは 8 ビットへ、tRNS はアルファへ、ICC・pHYs・テキストを運ぶ。圧縮したテキストは読まない）と WAV（自作。PCM 8/16/24/32・浮動小数 32/64・拡張形式、smpl・cue・adtl・INFO）。
- 段 4 で入れたもの：§9.5。段 6 で入れたもの：§9.6。段 7 で入れたもの：§9.7。段 9 で入れたもの：§9.8。
- `Platform` は kura-mac が実装して注入する（`MediaBackend` と同じ形）。Swift とのあいだは C の型と不透明なハンドルだけでやり取りし、画素や PCM は Rust が確保したバッファのポインタと長さで受け渡す（ADR-37 の改訂。段 9。§9.8）。
- `External` は ffmpeg（同梱しない・任意。kura-core が実装して注入する）。入力はファイルのパス（`file:`）で渡し、読んだ PCM は標準出力で流す。MP3 の書き出しは PCM を標準入力で渡し、一時ファイルに書く。引数（分離器・デコーダの強制、ホワイトリスト、`-fflags +bitexact` など）は §9.8。

### 9.2 形式の表

`FormatDesc { id, exts, mime, item_kind, decodes_to, caps }` で、拡張子 → items.kind → 値の型 → 保存できるメタデータを 1 か所で決める。M11 で取り込みの判定（`import::kind`）もこの表を見る。

| 形式 | items.kind | 読む | 書く |
|---|---|---|---|
| PNG（APNG・16 ビット・PNG-8） | image | png（`png-rs-dec/2`。旧版 `/1`） | png（`png-rs-enc/1`。APNG・eXIf を含む） |
| JPEG | image | zune-jpeg（`jpeg-zune-dec/1`。CMYK・YCCK は次の段の ImageIO へ） | jpeg-encoder（`jpeg-rs-enc/1`） |
| GIF | image | gif（`gif-rs-dec/1`） | gif（`gif-rs-enc/1`。減色は自作） |
| WebP | image | image-webp（`webp-rs-dec/1`。非可逆・可逆・アニメ） | image-webp（`webp-rs-enc/1`。可逆の 1 枚だけ） |
| aseprite | image | 自作（`aseprite-kura-dec/1`。合成した各フレーム） | — |
| PSD・PSB | image | 自作（`psd-kura-dec/1`。合成済みの画像。ZIP は次の段の ImageIO へ） | — |
| SVG | image | resvg・usvg（`svg-resvg-dec/1`。再現は約束しない → アンカー。外部の参照は読まない。`<image>` と、フォントを渡していないときの文字は次の段へ） | — |
| HEIC・HEIF・AVIF（静止画・シーケンス）・TIFF・BMP | image | ImageIO（`imageio-dec/1`。Platform。アンカー。§9.8） | — |
| RAW | image | —（段 9 では扱わない。形式の表・取り込みの判定にもない。扱う段で決める） | — |
| WAV | audio | 自作（`wav-kura-dec/1`。RIFF・smpl・cue・LIST） | 自作（`wav-kura-enc/1`） |
| AIFF・AIFF-C | audio | 自作（`aiff-kura-dec/1`。MARK・INST・NAME など） | 自作（`aiff-kura-enc/1`。整数は AIFF、浮動小数は AIFF-C の fl32） |
| FLAC | audio | symphonia（`flac-symphonia-dec/1`。MD5 を照らす） | 自作（`flac-kura-enc/1`。ADR-148） |
| MP3 | audio | nanomp3（`mp3-nanomp3-dec/1`。minimp3 とビット一致、libm を使わない。LAME の gapless）。layer 1・2 は ffmpeg（`mp3-ffmpeg-dec/1`） | 自作のエンコーダができるまでは ffmpeg（`mp3-ffmpeg-enc/1`。External。CBR 192 kbps 既定・VBR も。ID3 なし。§9.8）。なければ形式を変えて保存する |
| ALAC（m4a） | audio | symphonia（`m4a-alac-symphonia-dec/1`。整数だけ） | — |
| AAC-LC（m4a・ADTS） | audio | symphonia（`m4a-aac-symphonia-dec/2`・`aac-adts-symphonia-dec/2`。超越関数を libm に差し替えた写しで再現できる。priming は自作で切る。§9.8）。symphonia の AAC は 1・2 チャンネルだけ。3 チャンネル以上（形の段階で断る）と HE-AAC は AudioToolbox・ffmpeg へ（Platform・External なのでアンカー） | AudioToolbox（`aac-audiotoolbox-enc/1`。Platform。m4a だけ（ADTS は書かない）。LC・1〜6 チャンネル・既定 256 kbps。§9.8） |
| Ogg Vorbis | audio | symphonia（`ogg-vorbis-symphonia-dec/3`。3 チャンネル以上の residue 0/1 を直し、超越関数を libm に差し替えた写しで再現できる。§9.7・§9.8） | 自作（`ogg-vorbis-kura-enc/1`。aoTuV b6.03 の移植と Ogg の多重化。1〜8 チャンネル・8〜192 kHz。§9.7。ADR-149） |
| HE-AAC・Opus | audio | AudioToolbox（`audiotoolbox-dec/1`。Platform。アンカー。メモリから ExtAudioFile で読む。§9.8）。AudioToolbox が断るもの（多チャンネルの Opus など）と Ogg の FLAC・Speex、圧縮の WAV・AIFF、MP3 の layer 1・2 は ffmpeg（`opus-ffmpeg-dec/1` など。External。アンカー） | — |
| ttf・otf・woff・woff2 | font | —（解析だけ。M11） | — |
| glb ほか 1 ファイルの 3D | model | —（解析だけ。M11） | — |
| 動画 | video | —（編集しない） | — |

- C のライブラリは同梱しない。C か ffmpeg がないと書けない形式は、エンコーダを Rust で自作する（Ogg Vorbis を最優先。MP3・不可逆 WebP・Opus は必要になった段で。ADR-137）。
- 音声のタグは、各コーデックが自前で読み書きする（WAV の INFO、AIFF の NAME・AUTH・`(c) `・ANNO、FLAC・Ogg の Vorbis コメント、m4a の iTunes のタグは読みだけ）。Vorbis コメントは、既知の名前（title など）を先に、`vorbis:<KEY>` をあとに書き、読み取りが同じ名前にまとめるもの（大文字小文字だけの違い・DESCRIPTION と COMMENT・小文字ちょうどの `cprt` と COPYRIGHT）が重なれば先の 1 つだけを書く（読み取りと書き出しは `codec/symph.rs` の 1 つの表 `KNOWN_TAGS`・`comment_name` で決める。読み取りは同じ名前の 2 つめ以降を捨てるので、書いて・読んで・書き直すと同じコメントになるように。FLAC はバイト列も同じ、Ogg はシリアル番号と CRC を除いて同じ）。外のファイルの同じキーの複数の値は、最初の値だけを運ぶ。読み取りが作らない形（`vorbis:foo` だけ・`vorbis:TITLE` のような別名だけ・`vorbis:DESCRIPTION` だけ・`vorbis:cprt` だけ）は、値は保たれるが、書き直すと名前か並びが変わる。`vorbis:CPRT` と `vorbis:cprt` は読み取りが別の名前にするので両方書く。symphonia がタグでなく章・画像として読む名前（CHAPTER…・METADATA_BLOCK_PICTURE・COVERART）を `vorbis:<KEY>` で書くと、読み直したときに値はタグに残らない。lofty は段 6 では入れない（ID3v2・MP4 のタグを書く段で選び直す。ADR-148）。MP3 の ID3 は段 6 では読まない。EXIF・ICC は運ぶ。PNG の tIME・MP4 の作成日時など、時刻は書かない（R2 のため）。

- EXIF の向き（Orientation）：JPEG・PNG（eXIf）・WebP は、読むときに向きを画素に当て、運ぶ EXIF の向きを 1 にする（同じ向きを 2 回当てない）。形（`shape`）も向きを当てた大きさを返す。PSD は画素が表示のとおりなので向きは当てず、EXIF の向きだけ 1 にする。

### 9.3 能力

`CodecSet::capabilities() -> Vec<FormatCaps>`（段 9。環境は組み立てた `CodecSet` そのもの）が、形式の表の順にすべての形式について、読み手・書き手（段の順で、先頭が既定）と、形式のメタデータの能力（可逆か・アルファ・ICC・アニメ・ループ点）、書き手ごとの書ける値の種類・保存できるメタデータ・選べる設定・再現できるか（R2）・環境（`env`）を返す。macOS の能力は kura-mac が問い合わせ（`kura_codec_caps`：UTI・FourCC・レートの範囲・OS の build・ImageIO の版）、ffmpeg は `-decoders`・`-encoders`・`-demuxers`・`-muxers`・`-h encoder=libmp3lame` と LAME の版を調べて、足せる段だけを足す。エディタの「保存の形式」と一括加工の事前チェックはここを見る。`kura-devtools edit caps` が今の環境の表を JSON で出す。

### 9.4 アンカー

- 再現を約束しないデコーダ（`reproducible()` が偽：Platform・External と、固定の段でも OS の libm を使う SVG）で読んだ元（HEIC・AVIF・HE-AAC・Opus・SVG・ffmpeg で読んだ音声など）は、初めて編集するときに、読み取った結果を可逆形式で残す（ADR-135）。OS・ffmpeg の更新で読み取り結果が変わっても、途中の版を作り直せるようにするため。Ogg Vorbis と 1・2 チャンネルの AAC-LC は段 9 で値を再現できるようにしたので、アンカーは要らない（ADR-153）。3 チャンネル以上の AAC と HE-AAC は後の段で読むので、アンカーを作る。
- 入れ物（段 9。`codec::anchor`、`anchor-kura-enc/1`・`anchor-kura-dec/1`）：画像は PNG・APNG（ICC は iCCP）、音声は f32 の WAV（3 チャンネル以上は拡張形式）。標準のチャンクだけでは値のダイジェストまで往復しない（`source`・タグ・ピボット・格子・表示時間など）ので、私用のチャンク（PNG は IDAT の前に `kuRa`（ヘッダ）と `kuEx`（EXIF の生のバイト列）、WAV は data のあとに `kura`）に、元の読み取りの記録（`DecodeStamp`）とメタデータの正確な値を書く。ヘッダは「BLAKE3（ドメイン `kura-anchor-header-v1`、長さ付きの JSON）32 バイト ‖ 正規の JSON（`{v, anchor, body, decoded, source?, format?, options, content}`）」。FLAC は使わない（非可逆の音声をデコードした値は 24 ビットの格子に乗らず、1 を超える標本・−0 もある）。キャンバスいっぱいでないフレームの列は断る（浮動小数の画素の形式は値にない。足すときはヘッダの `v` を上げて入れ物を増やす）。
- 約束：アンカーを読み直した値のダイジェストは、元の読み取りの記録（`DecodeStamp.value_digest`）と同じ。作るときに読み直して照らし（`write_verified`・`verify`）、読むたびにも照らす。中身は固定のデコーダ（`png-rs-dec/2`・`wav-kura-dec/1`。`BODY_DECODERS`）だけで読み、それらは旧版として永久に残す。バイト列（R2）は約束しない（PNG の圧縮は fast）。知らない版・知らない中のデコーダは `Unsupported`、壊れた・別の記録のものは `Mismatch::AnchorBroken`、上限は `Limit`。中止は、書くとき画像は 64 行ごと（APNG はフレームごと）・音声は 16,384 フレームごと、読むときは中のデコーダの間隔（PNG は 256 行ごと）。
- 作り直し（`codec::recorded::read_recorded`。E29）：アンカーがあればアンカーだけを読む（元は読まない）。照合は記録の値のダイジェストの一致だけで、環境（`env`）が違っても同じ値なら使える。アンカーがなければ、記録の識別子のデコーダで読み直す。食い違いは失敗ではなく `Recorded::Mismatched`（`SourceChanged`（今の値と記録を添える）・`DecoderGone`・`AnchorBroken`）で返し、呼び手（M12）がアンカーを使う・利用者に知らせる。乱数の種（`InputValue.source`）は、アンカーから読むときも元のファイルのハッシュのまま（ディザの出力を変えないため）。
- 大きさの目安：24MP の画像で約 28 MB（書き 0.21 秒・読み 0.29 秒）、3 分の 44.1 kHz ステレオで約 64 MB。4 GiB を超える音声は RIFF に収まらないので作れない（保存を失敗にする。M12）。
- アンカーは実ファイルの台帳（M12）に、「元のファイル × 読み方の設定」ごとに載せ、整合性チェックの対象にする。保存の手順（どの時点で作るか、メモリの予算が値の 2 倍要ること）は M12 で決める。

### 9.5 段 4 の画像のコーデックの約束

詳しい約束は各コーデックのソースの冒頭（`crates/kura-engine/src/codec/*.rs`）に書き、ここは要点だけ。どれも、形（`shape`）と中身（`decode`）が必ず一致するように、形式やフレームの数を決める判断は画素を展開しない読み取りで先に行う。書いたフレームの列を読み直すと、フレームが 1 つでもフレームの列に戻る（APNG は acTL、GIF は NETSCAPE の拡張が印になる）。

- **PNG**（`png-rs-dec/2`）：acTL と fcTL があれば、フレームが 1 つでもフレームの列（acTL のフレームの数と fcTL の数が違えば壊れたファイル）。キャンバスに fcTL の blend（SOURCE・OVER。OVER は `imaging::composite::over` の整数の式）で重ね、dispose（NONE・BACKGROUND・PREVIOUS）を当てたキャンバスいっぱいのフレームの列にする。インターレースのフレームは、フレームの大きさで Adam7 を展開する。パレットの APNG は、番号のまま重ねられるとき（半透明の色を OVER で重ねない・消すときに透明な色がある）だけ Indexed8。そのとき消した画素は消す色の番号（パレットの RGB でアルファ 0）になり、広げた色の RGB は仕様の「透明な黒」と違うが見えない（Rgba8 の経路と見える画素は同じ）。色・深さ・パレット・tRNS は png クレートが読んだものを使い（奇数長の tRNS・PLTE より前の tRNS などは png クレートに従う）、チャンクの並びは前から順に読む（acTL・fcTL・eXIf のほかは中身を読まずに飛ばす）。eXIf は IDAT より前のものだけで、その大きさはテキスト・ICC と合わせて `max_meta_bytes` に数える。インターレースでない 1 枚の画像は行ごとに読む（作業領域は 1 行と 1 タイル行）。`png-rs-dec/1`（APNG は既定の画像だけ、eXIf を読まない）は旧版として残す。書き出しは、キャンバスいっぱいのフレームの列を APNG（表示時間は 65,535 ms まで ms のまま）、EXIF を eXIf に書く。
- **JPEG**：zune-jpeg を SIMD なしで使い、どのターゲットでも同じ整数の経路。JFIF の解像度は自前で読む（zune-jpeg は読まない。マーカーを前から順に読む）。プログレッシブは係数を全部持つので、値の 3 倍が上限を超えれば断る。書き出しの設定は `quality`・`chroma`・`progressive`。透明な画素のある画像・16 ビットは書かない（先に背景の合成・8 ビットへの変換）。
- **GIF**：ブラウザと同じ合成（透明の番号は描かない、Background は透明、Previous は戻す、背景色は使わない）。フレームが 2 つ以上か NETSCAPE の拡張があればフレームの列（繰り返しは、拡張がなければ 1 回、0 なら無限、n なら n + 1 回）。局所パレットがなく大域パレットがあれば Indexed8（消す色が要れば透明な色をパレットの後ろに足す。256 色で足せなければ Rgba8）。書き出しは `alphaThreshold`（既定 128）で 2 値の透明にし、色が収まらなければ自作のメディアンカット（`imaging::quantize`。整数だけ）で減色、フレームの列は 1 つの大域パレット。1 回だけ再生するフレームの列は NETSCAPE の拡張を書かないので、1 フレームで 1 回の列は読み直すと 1 枚の画像になってしまう。そこで書かずに `Unsupported` にする（先に 1 枚の画像にする）。
- **WebP**：image-webp の合成（四則演算だけ）を、背景を透明にして使う。ICC と EXIF の合計を `max_meta_bytes` で抑える。書き出しは可逆の 1 枚だけ（不可逆・アニメは自作のエンコーダを作る段で。エンコーダの `caps` もアニメを書けないと答える）。
- **aseprite**：自作。層を下から合成（見えない層・参照層は描かない、ヘッダの印でグループを別に合成、層の不透明度の印、z-index）。ブレンドは `imaging::blend`（Aseprite の `blend_funcs.cpp` の式）で、asefile と 300 個のランダムな絵で一致を確かめている（`tests/aseprite_ref.rs`）。パレットの絵は、すべて通常・不透明度 255 で、パレットの色がどれも不透明（背景の層がないときの透明の番号を除く）なら番号のまま Indexed8、ほかは Rgba8。グレーは RGBA で合成して Rgba8。スライス（キャンバスの座標）・タグ（`ImageMeta.tags`）・ICC を運ぶ。タイルの反転と外部のタイルセットは段 4 では読まない（`Unsupported`）。グループの入れ子は 64 段まで。セルはキャンバスの範囲に切って描き、タイルマップはキャンバスに重なるタイルだけを描く（どちらも 64 行ごとに中止を確かめる）。1 フレームを描くあいだに展開するセル・タイルセットは合計 `max_value_bytes` まで（タイルの爆弾を断る）。フレームをまたいで使い回す展開済みのキャッシュも合計 `max_value_bytes` までで、超える分は持たずに描くたびに展開する。
- **PSD・PSB**：自作。合成済みの画像だけ（層があって合成済みの画像が本物でないものは `Unsupported`）。ビットマップ・グレー・インデックス・RGB の 8/16 ビット、圧縮なし・RLE。合成済みの画像の ZIP はツールによって流れの分け方が違い、確かめる相手もないので `Unsupported`（次の段へ）。PSB は長さの欄が広がる所（層とマスクの情報の長さは 8 バイト、RLE の行の長さは 4 バイト）を読み分け、どの長さもファイルの残りの長さと照らしてから使う。透明は、層の数が負か Mtrn などがあるときだけ。Photoshop の白の下塗りは、下塗りでしか成り立たない条件がすべての画素で成り立つときだけ戻す（ImageMagick などは下塗りしない）。CMYK・Lab・32 ビットは次の段へ。拡張子は psd・psb。
- **SVG**：resvg。結果は OS の libm に依存しうるので `reproducible()` は偽（アンカーを作る）。`<image>`（外部のファイル・data: のラスター画像・入れ子の SVG。pattern・mask などの中も）を含むものは `Unsupported`（resvg の raster-images は zune-jpeg の SIMD を有効にするので入れない）。文字は `PinnedOptions.svg_fonts` で渡したフォントだけで描き、渡していなければ `Unsupported`。usvg に渡す前に、ファイルの大きさ（16 MiB）・DTD の実体の展開（64 MiB）・要素の数（2²¹）・`<use>` の展開・marker の頂点ごとの複製（頂点の数 × marker の中の要素の数）を見積もって断る。DTD は引用符・注釈・PI を区別してバイト列のまま読み、パラメタ実体・外部の実体・マークアップや参照を含む実体は断る。入れ子の marker も断る。roxmltree・usvg・resvg は入れ子の深さだけ再帰し、スタックを使い切ると受け止められずに落ちるので、XML を読む前に生のバイト列で入れ子の深さを数えて 64 段で断り（2 MiB のスタックのデバッグのビルドで、重い形が 96 段まで動くことを測った）、参照（`<use>`・`url(#…)`・祖先から受け継ぐ fill・stroke・marker・CSS の規則）を展開した高さも 64 段で断る。参照の輪（pattern の中身が受け継いだ fill で自分を描き直す、など。ファズで見つけた）も断る。CSS の規則は、要素名・`.class`・`#id` の単純なセレクタだけ当たる要素を決め、それより複雑なセレクタはどの要素にも当たりうるとみなす。見積もりは再帰せずにたどり、手間（要素ごとの参照と辺の数の合計）は 2²⁴ まで。usvg はすべてのセレクタをすべての要素に照らし、子孫・`~` の結合子では祖先・兄弟をさかのぼって後戻りし（試す並びは、文書の深さ・兄弟の数から結合子の数だけ順に選ぶ組み合わせの数）、当たった規則と style 属性の宣言を宣言の数の 2 乗の手間で書き込むので、CSS のセレクタの数 × 要素の数と、これらの手間の合計（当たる要素の数は要素名・クラス・id ごとに数え、複雑なセレクタはすべての要素とする）をそれぞれ 2²⁷ までにする。セレクタの部分の数は 32 まで（simplecss は結合子の数だけ再帰する）。受け継ぐ参照は、受け継がないと分かっているもの（clip-path・mask・filter）のほかはすべて受け継ぐとみなす（`*fill` のような古い書き方の名前も usvg は fill として読む）。usvg は単位が objectBoundingBox の pattern・clipPath・mask と filter を、参照する要素ごとに中身ごと複製する（入れ子で指数に増える）ので、受け継いだ fill・stroke も使う図形ごとに数え、複製で作る要素の数を 2¹⁷ までにする。usvg は弧・円・楕円・角丸を半径に応じた数の曲線に直す（半径 10³⁰ の弧 1 つで 6 秒を超えた）ので、図形の属性・style・CSS の宣言の数の絶対値は 10⁹ まで（色・id・参照・名前の中の数字は数として読まない）。
  描く前に、resvg の描き方に合わせて木（mask・clipPath・フィルタの feImage・文字の輪郭・pattern の中身も）をたどり、資源を見積もる（`check_render`。超えたら `Unsupported`）：(1) 根からの道筋ごとに同時に生きる一時の画像の合計（層は 1 枚がキャンバスの 25 倍まで広がり入れ子の層は同時に生きる、mask と clip は層ごとに画像とマスク、フィルタはプリミティブの結果をすべて持つ、pattern のタイルは外側のタイルを持ったまま中身を描く）を絶対の予算 `max_value_bytes` まで、(2) 描く手間（画素の数。図形の外接矩形と描く面の小さい方・層の合成・プリミティブごとの式：feMorphology は層の画素 × 半径の矩形、feTurbulence はオクターブの数、feConvolveMatrix は行列の大きさ、ぼかしは半径の分）を 2³² とキャンバスの 64 倍の大きい方まで（resvg の描画は途中で止められないので、手間で時間を抑える）、(3) 変形のあとの座標が ±10⁸ 画素を超える図形を断る。いずれも検証とファズで見つけた形（pattern のタイルの 209 GB の確保・半径 10³⁷ の楕円の遅い入力など）。

### 9.6 段 6 の音声のコーデックの約束

詳しい約束は各コーデックのソースの冒頭（`crates/kura-engine/src/codec/{aiff,flac,mp3,lossy,symph,pcm}.rs`）に書き、ここは要点だけ。どれも形（`shape`）と中身（`decode`）が必ず一致するように、長さはデコードせずに決める。形がちょうどの長さ（`Exact`）の形式は、デコードした長さが違えば壊れたファイル、上限（`AtMost`。MP3・AAC）の形式は、上限を超えれば壊れたファイルで、短いものは短く読む。

- **AIFF・AIFF-C**（自作）：AIFF の整数と、AIFF-C の `NONE`・`twos`・`in24`・`in32`・`raw `・`fl32`・`fl64`・`sowt`（16 ビットと、バイトの順によらない 8 ビット）。ほかの圧縮は `Unsupported`（次の段へ）。チャンクの数は 65,536 まで（WAV も同じ。小さなチャンクを並べたファイルの走査で止まらないように）、次のチャンクが今の位置なら seek しない（`BufReader` のバッファを捨てない）。フレーム数は SSND の長さで決め、COMM の numSampleFrames は使わない（CoreAudio・ffmpeg・libsndfile と同じ。ADR-148）。80 ビットのサンプルレートは整数だけで最も近い整数（偶数への丸め）に読む。INST の sustainLoop をループ点にし、その 2 つのマーカーはマーカーの一覧から除く。NAME・AUTH・`(c) `・ANNO をタグに。メタデータの数と大きさは WAV と同じ予算。書き出しは整数を AIFF、浮動小数（32 ビット）を AIFF-C の fl32 に。タグは読み取りと同じ形（4,096 バイトまで文字の境目で切り詰め、NUL まで、前後の空白なし）にして書く（書いて読み直したものが同じになるように。FLAC も 4,096 バイトに切り詰める）。
- **FLAC**：読みは symphonia（STREAMINFO の MD5 を照らす）。長さは STREAMINFO の総フレーム数（0 ならパケットの長さの和）で、違えば壊れたファイル（MD5 も照らすので、途中で切れた FLAC は読まない）。書きは自作（`flac-kura-enc/1`。ADR-148）：8・16・24 ビット、1〜8 チャンネル、4,096 フレームの固定ブロック、サブフレームは定数・生・固定の予測子 0〜4 から最小のもの（wasted bits を含む）、残差は分割ライス（次数 0〜8）、ステレオは 4 通りの組から最小、STREAMINFO の MD5、タグかループ点があれば VORBIS_COMMENT（vendor "kura"）にタグと LOOPSTART・LOOPLENGTH。どれも整数だけで決まるので、すべてのターゲットで同じバイト列。フレームは並列に作って番号の順に並べる。LPC は使わない（フィクスチャの掃引で元の 44%。flac 1.5.0 の `-0` は 49%、`-8` は 33%。LPC を足すならエンコーダの版を上げる）。空の音声は `Unsupported`（総フレーム数 0 は「不明」の意味で読み直せない。flac も空の入力は書かない）。出力は `flac -t`・metaflac・ffprobe で読め、標本が元と一致することを確かめた。
- **MP3**（nanomp3 `=0.2.0`、`alloc` だけ）：minimp3 とビット一致の純 Rust で、libm を使わないので値を再現できる（`reproducible()` は真）。layer 3 の MPEG-1・2・2.5 だけで、最初のフレームが layer 1・2（.mp3 の拡張子の MP2 など）なら `Unsupported`（nanomp3 は layer 1・2 の feature を入れないと、標本を出さずにエラーにもしない）。長さは開くときに決め、形はその上限（`AtMost`）：見つかったフレーム（`nanomp3::frames`）の標本の数の和（中身から出うる長さ）と、Xing・Info（LAME・Lavc）のタグの値（エンコーダの遅延と詰め物を切った長さ。nanomp3 はこのとき走査しない）の小さいほう。タグのフレームの数が 0（途中で止まった ffmpeg の出力の形）なら長さは分からないものとして全部読み、タグがなければ nanomp3 の全体の走査の値。最後の完全なフレームの終わりまでを読むので、途中で切れたファイルは短く読む（nanomp3 は切れた最後のフレームや末尾のゴミを「形式が変わった」エラーにする）。free format（ビットレート 0）は、`nanomp3::frames` が最後の 2 フレームを見つけられないので切らず、上限にその分を足す。途中でチャンネルの数・レートが変わるものは読まない。ID3 のタグは読まない。
- **symphonia**（部品のクレートを `=0.6.1`、既定の feature なし）：Probe を通さず、形式の表で読み口とデコーダを選ぶ。ファイルは `max_value_bytes` まで読み込んでから渡し、読み口は `read` のたびに、パケットごとにも中止を確かめる。壊れたパケットは飛ばさずエラーにする。
  - **Vorbis・AAC**：段 6 では、デコーダの中で std の超越関数が表を作るので値を再現できず、`reproducible()` を偽にしてアンカーを作る扱いだった。段 9 で超越関数を libm に差し替えた写しにして、再現できるようにした（識別子を上げた。§9.8・ADR-153）。以下は段 6 のときの記述：編集するときはアンカーを作る（§9.4）。FLAC・ALAC は整数だけで再現できる。m4a は中身で再現の可否が違うので ALAC 用と AAC 用のデコーダを分け、扱わない中身は `Unsupported` で次へ回す。
  - 長さ：ALAC・Vorbis はパケットの長さ（遅延・詰め物を除いた数）の和を `Exact` にする（Vorbis は最後のグラニュールで切る）。デコードした長さが形と違えば（長いときも）壊れたファイル。AAC は priming を自前で切る：iTunSMPB（Apple）があればその priming と長さ、なければ MP4 の elst（ffmpeg）の最初の空でない編集、どちらもなければ切らない（ADTS は常に切らない）。切るときは、切る前の長さ（パケットの長さの和）まで読んでから切る（編集リストが中身の後ろを大きく切るファイルも読む）。形は上限（`AtMost`。切り取りの長さは、中身から出うる長さで抑える）。
  - デコードする量：パケットの長さを数えるとき、切り取る前の長さ（`dur` と前後の切り取り。デコーダが実際に作る量）の和も `max_audio_frames`・`max_value_bytes` で確かめる（Ogg のページのグラニュールで長さを 0 近くまで詰めると、形は小さいまま大量のパケットのデコードを通せてしまう。検証で見つけた。2,000 パケット・4 KB のファイルで確かめる）。
  - HE-AAC：AudioSpecificConfig の明示の印（オブジェクトの種類 5・29、GASpecificConfig のあとの同期の拡張 0x2B7・種類 5・sbrPresentFlag = 1）があれば `Unsupported`（symphonia は半分のレートで読んでしまう。次の段の AudioToolbox・ffmpeg へ）。ffmpeg は SBR がないことを同期の拡張（sbrPresentFlag = 0）で明示するので、旗まで読む。帯域の中だけの暗黙の SBR（HE-AAC の ADTS、ASC に印のない m4a）は、段 9 から symphonia-codec-aac の写しが fill 要素の SBR の拡張（EXT_SBR_DATA・EXT_SBR_DATA_CRC）を見つけたかを知らせ（`sbr_seen()`）、形を決めるときは先頭の 16 パケットをデコードして、読むときは読み終わりにもう一度聞いて、見つかれば `Unsupported` で後の段（AudioToolbox・ffmpeg）へ回す（LC の値は変わらない）。16 パケットより後で初めて SBR が出るものは、形は固定の段（LC）が答え、読みは後の段になる。
  - Vorbis：デコーダを作る前に、setup ヘッダの codebook ごとに symphonia が確保する大きさ（VQ の表、項目ごとの配列、探索表）を見積もり、合計が `min(max_value_bytes, 256 MiB)` を超えれば `Unsupported`。探索表は B = clamp(最長, 4, 8) ビットずつ読むブロックの木で、symphonia は完全な木でなければ表を作る前に断るので、深さ d = kB のブロックの数はちょうど 2^d − Σ_{長さ ≤ d} 数 · 2^(d − 長さ)（`Vec` の伸びを見込んで 1 要素 16 バイト）。検証で、長さ 17 の符号語 131,072 個の codebook（約 20 バイト）1 つで 143 MB、8 つ（約 300 バイトの Ogg）で 1 GB を確保させられると分かったので、表まで数えるようにした（そのファイルは今は確保の前に断り、ピークのメモリは 2.5 MB。乱数の形 9,000 個・codebook 1〜255 個の形 1,500 個で、見積もりが symphonia の実際の確保を下回らないことを確かめた）。floor0 の表・residue の作業領域・DSP の固定の確保（symphonia の上限で、最大でおよそ 14 MiB）は予算の外。Ogg の中の Opus は `Unsupported`（次の段へ）。
  - タグ：Vorbis コメント（FLAC・Ogg）と MP4 の iTunes のタグから title・artist・album・comment・date・genre・copyright、FLAC・Ogg のほかのキーは `vorbis:<KEY>` のまま運ぶ（FLAC に書き戻せる）。ALAC の元のビット数は magic cookie から読む。
- 他のツールで作ったフィクスチャ（sox・flac・lame・afconvert・ffmpeg・libvorbis）で照らした（`tests/fixtures.rs`、作り方は fixtures/README.md）：FLAC・ALAC・AIFF は元とちょうど同じ値、MP3・AAC・Vorbis は他のデコーダ（ffmpeg・afconvert・lewton）と長さがちょうど同じで、信号と誤差の比が 3.4×10⁸ 以上（参照の 16 ビットの丸めの大きさ）。MP3 の free format は ffmpeg が読めないので LAME のデコーダと照らす。

### 9.7 段 7 の Ogg Vorbis の書き出しの約束

詳しい約束は `crates/kura-engine/src/codec/vorbis/mod.rs`（全体の地図・移植の規則・ブロックをまたぐ状態の表）、各ファイルの冒頭（どの C の関数・行を移したか）、`codec/ogg.rs`（ページ）に書き、ここは要点だけ（ADR-149）。

- **作り方**：aoTuV b6.03（libvorbis 1.3.7 が土台。crates.io の `aotuv_lancer_vorbis_sys` 0.1.6 に入っている C）のエンコーダの流れと表を Rust に移した（`ogg-vorbis-kura-enc/1`、`Tier::Pinned`）。aoTuV の独自の機能（M1〜M9・12 帯域の envelope・aoTuV の ATH と hypot）はすべて移し、**前増幅（`pre_amplitude`。音量を変える）だけを除く**。ビットレートの管理（bitrate.c の管理の枝）と SSE の枝は移さない（品質の VBR だけ）。表（テンプレート 18 個・codebook 598 冊・窓・ATH・マスク・aoTuV の M3・M7・M9 の表など）は `crates/vorbis-tables`（BSD-3-Clause。C のヘッダから `gen/` の道具で作ってコミットし、入力の C と生成物を SHA-256 で凍結）に置く。
- **正しさの基準**：前増幅だけを切った aoTuV の C を、FMA なし（`-ffp-contract=off`）・符号付き整数のあふれを 2 の補数で折り返す（`-fwrapv`）・SSE なし・超越関数を `libm =0.2.16` に差し替え・入力を一度に渡す形で組んだ「照合用のエンコーダ」（`crates/vorbis-tables/oracle/`。組み方は README。C の写しはコミットせず、crates.io の源に差分を当てて組む）と、識別・setup・すべての音声パケット・グラニュールがビット単位で一致する（コメントは vendor とタグが違うので除く）。照合のテストは `KURA_VORBIS_ORACLE=<組んだ場所>` のときだけ回る（`codec::vorbis` の回るテスト 136 件のうち 53 件。ffmpeg で長さを確かめる 1 件は `KURA_FFMPEG` も要る）。音声パケットを照らす範囲は、9 レート × 1・2・3・6・8 チャンネル × q −1・0・3・5・10、4・5・7 チャンネルとテンプレートの境のレートで 8 件、端の長さ、数秒の入力、NaN が途中に出る入力、aoTuV の衝撃の入力、極端な振幅（1e9〜`f32::MAX`）24 件、区間とスレッドの組、モジュールごとの中間値と導出の表。setup とヘッダは q −1〜10 のすべてで照らす（q 1・2・4・6〜9 の音声パケットは作業の記録で 312 本を照らした）。aarch64 の kura と x86_64 の kura（Rosetta）のどちらでも一致する。C の符号付き整数のあふれ（標本の絶対値がおよそ 1e9 を超えると residue の量子化で起きる）は未定義動作で、`-fwrapv` なしの `-O2` は `-O0` と違う出力を出すので、照合の基準は `-fwrapv` で定める（移植は wrapping の演算で写す）。float → int の範囲の外の変換は C では未定義のままで、aarch64 の飽和（Rust の `as` と同じ）に頼っている。
- **移植の規則**（mod.rs の 12 か条）：C の型（float → f32・double → f64・int → i32・long → i64）、暗黙の型変換、演算の順をそのまま写す。FMA（`mul_add`・`fma`）を使わない（モジュールのテスト `no_fused_multiply_add` が `vorbis/` のすべてのファイルと `ogg.rs` を読み、注釈とリテラルを除いたコードで呼び出しを探す）。超越関数は `math` だけで、使うのは初期化（表・窓・Bark）だけ。todB はビットの式、`rint` は `round_ties_even`、C の `max`・`min` マクロは比較の形のまま、aoTuV の部分的な選択ソート（`ssort`）はそのまま写す。C が範囲の外を読む所（`noiseoff[j][17]`、M7 の短い窓の `spectral[n]`）は、結果に効かないか設定のテンプレートでは届かないことを確かめて、読まない形にした。
- **範囲と設定**：1〜8 チャンネル、8,000〜192,000 Hz、2³⁰ − 2¹⁶ フレームまで（C の `int` の作業長があふれない範囲。48 kHz で約 6 時間）。`can_encode` はこの 3 つ（長さは形の最大）を見て、範囲の外は次の段へ回す。2 チャンネルは stereo のテンプレート（coupling・residue 2）、ほかは uncoupled（residue 1。5.1 の専用のテンプレートは使わない）。設定は `quality`（整数 −1〜10、既定 5。libvorbis の品質は `q / 10`）。元の品質から推定した値を渡すのは kura-core（M12 以降）。
- **入力**：WAV の並びの planes を Vorbis I §4.3.9 の並び（symphonia の `map_vorbis_channel` と同じ表）に直して渡す。NaN は 0、±∞ は ±1 にする（NaN のビットの形がターゲットで違い、todB がビットを値にするため）。1 を超える有限の値はそのまま。全体を一度に渡す（`vorbis_analysis_buffer(N)`・`wrote(N)`・`wrote(0)`）ので、1,024 フレームずつ渡す libvorbis の使い方とは、最初の外挿と envelope の先読みの差で最初の数ブロックの窓の長短が変わり、そのあとのブロックの位置がずれることがある（同じ入力なら常に同じバイト列）。
- **コメント**：vendor は `kura`。項目は LOOPSTART・LOOPLENGTH（`start < end ≤ 長さ` のときだけ）、`ENCODER=ogg-vorbis-kura-enc/1`、タグ（`symph::vorbis_comments_for`。名前の重なりを除く。§9.2）の順。元の `ENCODER`（`vorbis:ENCODER`）は運ばない（RPG ツクール MV/MZ は LOOPLENGTH の次の項目の長さの下位バイトまで数字として読みうるので、決まった ENCODER を番兵にする）。コメントのパケットが 16 MiB 以上なら `Unsupported`（symphonia の Ogg の読み口が読めるパケットの大きさ）。
- **Ogg のページ**（`codec/ogg.rs`。凍結。変えるとエンコーダの版を上げる）：1 ページ目は識別ヘッダだけ（BOS・グラニュール 0）、コメントと setup は 2 ページ目から（setup で閉じる）、最初の音声パケットだけのページ（グラニュール 0）、「長いブロックで次が短い」パケットの直後では閉じない、本体が 4,096 バイト以上になったパケットで閉じる、最後の音声パケットだけの最後のページ（直前のパケットが「長・次は短」ならその 2 つ）に EOS、各ページのグラニュールはそこで終わる最後のパケットの窓の中心（最後のページは総フレーム数）。音声が 1 ページだけのストリームは lewton・ffmpeg が末尾を切らず（最初の音声パケットだけのページはそのため）、「長・次は短」の直後で閉じたページは lewton が数え誤る（そのため閉じない）。最後の音声パケットだけの最後のページは、検証で外しても長さが変わらなかったが、規則を単純に保つために凍結した。lewton の `OggStreamReader` は、最後が「長・次は短」のあとの短いブロックで終わる形で末尾を切りきれないことがある（192 kHz・8 チャンネル・q10 の正弦 1,000 フレームを 1,152 と読む。lewton の数え方の不具合で、ページの切り方では避けられない。symphonia・ffmpeg・libvorbis は正しい）。シリアル番号は `BLAKE3("kura-ogg-serial-v1" ‖ 値のダイジェスト ‖ 設定の正規の JSON)` の先頭 4 バイト（乱数・時刻を使わない）。空の音声も書く（libvorbis と同じく短いブロックの 1 パケット）。
- **並列と速さ**：ブロックの計画（envelope。直列）→ 区間（2²¹ 標本ぶんのブロック）ごとに、aoTuV の窓の状態（直列）→ 窓・MDCT・FFT（`par_blocks`）→ ampmax・M5 の lowcomp・M3 の lastmdct（直列）→ 心理モデル・floor・residue・パケット（`par_blocks`。M3 の tempmdct を前から引き継ぐ続いた衝撃のブロックは 1 つの仕事の中で順に）。結果はスレッド数・区間の大きさによらない（照合とテストで確かめる）。中止は、PCM の確保の前、入力の写しの 2¹⁷ フレームごと、前の外挿（仕事ごとと 2¹⁷ 標本ごと）、envelope の探索の 2,048 歩ごと、計画 32 個ごと、ブロックの解析のブロックごと、シリアル番号のダイジェストの前後と 16,384 標本ごと、Ogg の多重化の 512 パケットごとに確かめる（48 kHz・ステレオで、最初から頼んだ中止は 1 ms 未満で戻り、確かめの間隔の最大は PCM の確保で 30 分なら約 35 ms、3 時間なら約 0.2 秒）。3 分の 44.1 kHz ステレオの q5 で 0.27〜0.30 秒、1 スレッドで 1.45〜1.56 秒（`vorbis_bench_three_minutes_stereo`、16 コア。測るたびの揺れ）。26 kHz 以上で衝撃の多い入力（クリックの列など）は、続いた衝撃のブロックが 1 つの仕事になるので段 3 がほぼ直列になる（10 分のステレオで約 9 秒。負荷のある機械で測った）。メモリは、書き出しのあいだ入力全体の f32 の写し（C の `vorbis_analysis_buffer`。値と同じ大きさ）を持ち、ピークは値の約 2.1〜2.8 倍（44.1 kHz ステレオ 3 分で値 64 MB に対して 168 MB）。段 10 のメモリの予算（§12）で、エンコーダごとの倍率として扱う。
- **品質と互換**（数字は ADR-149）：同じ q で、素の aoTuV b6.03（vorbis_rs）とビットレート・NMR が中央値でほぼ同じ（違いは入力の渡し方と前増幅の分だけ）。kura の出力 1,200 本（1・2・3・6・8 チャンネル × 8・22.05・44.1・48・96・192 kHz × q −1・3・5・10 × 長さ 0〜数秒）を symphonia・lewton・ffmpeg・libvorbis（sox）で読み、長さはすべて元とちょうど同じ。検証では、端の長さ 405 本（1〜8 チャンネル・8〜192 kHz）と約 33 万本の掃引も、symphonia と lewton の `OggStreamReader` で長さが元どおりだった（上の lewton の例外を除く）。チャンネルの並びは 1〜8 チャンネルで symphonia・ffmpeg（WAV の並びで出す）と lewton・sox（Vorbis の並びで出す）が期待どおり。品質の記録の道具は `kura-devtools vorbis-quality`（合成の音と手元の音で、ビットレート・帯域・NMR・LSD・長さを測る。ViSQOL・Zimtohrli は作業の環境だけで使い、リポジトリには入れない）。
- **symphonia の Vorbis の修正**：symphonia-codec-vorbis 0.6.1 は residue 0/1 を 2 チャンネル以上で読むとき、チャンネルの分類を次のチャンネルの範囲まで書いてしまい、1 チャンネルあたりの partition の数が分類の codebook の次元の倍数でない uncoupled の 3〜8 チャンネル（aoTuV b6.03 の q5、Xiph 1.3.7 の q3 など。品質（テンプレート）で起きたり起きなかったりする）を誤って読む。1 行を直した写しを `third_party/symphonia-codec-vorbis/`（MPL-2.0 のまま公開。`KURA.md` に変更・理由・確かめたこと）に置き、ワークスペース（と `fuzz/`）の `[patch.crates-io]` で使う。読む値が変わったので識別子を `ogg-vorbis-symphonia-dec/2` にした（公開前で、再現を約束しないデコーダの作り直しはアンカーから読むので、旧版の `/1` は残さない）。上流で直った版が出たら写しをやめる。 段 9 で、さらに超越関数を libm に差し替えて値を再現できるようにし、識別子を `ogg-vorbis-symphonia-dec/3` に上げた（§9.8）。
- **dev 依存**：lewton `=0.10.2`（読み直しの照合だけ。E28（`tests/ogg_vorbis.rs`）と `codec::vorbis::encoder` の照合のテスト。aoTuV で作った 3・5 チャンネルのフィクスチャは、lewton の出力を保存した参照と照らす（`tests/fixtures.rs`））。
- **Godot**：Godot 4 は libvorbis で読む。LOOPSTART・LOOPLENGTH は読まず、ループは取り込みの設定の `loop`・`loop_offset`（秒）で決め、ファイルの終わりからループする。つなぎ目なしにループさせるには、ループの終わりで切ったファイルに `loop_offset` を `(LOOPSTART + 0.5) / rate` 秒にする（Godot は秒を標本に切り捨てで直すため。Godot の再生を写したシミュレーションで確かめた）。M15 の持ち出しで使う。ユーザーが Godot 4.7.2 で確かめた結果：44.1 kHz ステレオの BGM（q0・q5・q10）と 48 kHz モノラルの効果音（0.3 秒・300 フレーム）は取り込め、再生でき、`loop_offset` を (LOOPSTART + 0.5) / rate 秒にしたループもつなぎ目に問題がなかった。長さ 0 の音声だけは、エラーを出さずに取り込めなかった（kura は libvorbis と同じく長さ 0 も書く。Godot 向けの持ち出しでどう扱うかは M15 で決める）。

### 9.8 段 9 の macOS と外部ツールの約束

詳しい約束は `crates/kura-engine/src/codec/{platform,caps,anchor,recorded}.rs`、`crates/kura-mac/src/codec/`・`swift/Sources/KuraMac/Codec/`・`swift/Sources/KuraMacC/include/kura_mac_ffi.h`、`crates/kura-core/src/codec_ext/ffmpeg/` の冒頭に書き、ここは要点だけ（ADR-153）。

- **段の組み立て**：固定の段 → Platform（kura-mac が `add_decoder`・`add_encoder` で足す。AAC の書き手は能力（AAC-LC のエンコーダと選べるビットレートの表）があるときだけ）→ External（kura-core の ffmpeg の段。見つからなければ足さない）。既定でない書き手は `CodecSet::encode_bytes_with(識別子, …)`（能力の表の `WriteCap.codec` をそのまま渡す。`encode_bytes` と同じ確認を通り、その識別子がその形を書けなければ次へ回さず `Unsupported`）で選ぶ。アプリの組は `kura_mac::full_codec_set(opts, 一時ファイルの置き場)`。固定の段でないのに `reproducible()` が真のコーデックは足せない（`Internal`）。
- **trait に足したもの**：`Decoder::env`・`Encoder::env`（`CodecEnv { tool, version, components }`：OS の版と build・ImageIO・AudioToolbox、ffmpeg と libav* の版・LAME の版・使ったデコーダ・指紋など）、`Encoder::reproducible`（既定は固定の段だけ）、`Decoder::shape_file`・`decode_file`（ファイルのパスで読む口。既定は開いて読み口に渡す。ffmpeg が上書きする）、`Decoder::shape_monitored`・`shape_file_monitored`（中止を見ながら形を返す。`CodecSet` が値を読む前に呼ぶ。既定は `shape`・`shape_file`。形を決めるのに 1 回デコードして数える ffmpeg が上書きし、写し・ffprobe・枠の待ち・数えるデコードのどこでも中止が効く）、`Encoder::writes_format`。`DecodeStamp`・`EncodedStamp` に `env` を、`EncodedStamp` に `reproducible`（R2。真なら書かない）を足した（ないとき・真のときは書かないので、段 8 までの記録とゴールデンは変わらない）。`DecodeStamp`・`EncodedStamp` は保存のために読み直せる（知らない項目は断る。`EncodedStamp` は `reproducible` がなければ真、`env` がなければ空として読む。固定の段の JSON は段 8 と同じ）。third_party の 4 つの写しは `KURA_PATCHED` を持ち、kura-engine が参照するので、`[patch.crates-io]` が外れて上流のクレートが組まれると組み立てが失敗する。
- **Platform の受け口（kura-engine の `codec::platform`。OS を知らない）**：画像（`ImageBackend`：開く → 形（大きさ・画素の形式・アルファがストレートかプレマルチプライドか・向き・フレーム数・ICC・HDR か）→ 帯ごとに Rust の領域へ書く）、音声（開く → 形（レート・チャンネル・長さ）→ チャンクごとに planar f32 へ書く）、AAC の書き出しの backend。adapter が `Decoder`・`Encoder` を実装し、上限で確保の前に断る・帯とチャンクの合間で中止・アルファのストレートへの戻し（8 ビットは ⌊(2·c·255 + a)/(2a)⌋。vImage と 8 ビットの全 65,536 組で一致）・EXIF の向き（kura の `orient`）・形と中身の照合を Rust の整数の式で行う。状態の番号は 0〜6（OK・Unsupported・Failed・Canceled・Invalid・Mismatch・Limit。Swift と同じ値）。Linux でも偽の backend でこの adapter を試す。
- **FFI**（ADR-37 の改訂）：C の型と不透明なハンドルだけで、2 段の呼び出し（Swift が開いて形を返す → Rust が上限を確かめて `try_reserve_exact` で確保 → Swift がそこへ書く）。Swift は Rust の領域を呼び出しのあいだだけ使い、保持しない（例外：音声の読み取りの入力のバイト列は、Swift がハンドルの寿命のあいだ読み、Rust は close まで落とさない）。エラーは状態の番号と、Rust が渡す 512 バイトの文言のバッファ。中止は Rust が帯の合間で `Monitor` を見て、Swift の中のループには C のコールバックを渡す。ABI の版（1）と構造体の大きさを起動時に照らす。`kura_mac_ffi.h` が全 18 関数のプロトタイプまで含めて正本で、Swift の `@_cdecl` の型は `FfiCheck.swift` がコンパイル時に、Rust の `extern "C"` の宣言・構造体・番号は `abi.rs` のテストがヘッダの文字列と照らす。Platform のコーデックの unsafe は kura-mac の `bridge.rs` だけ（kura-engine は `forbid` のまま。kura-mac のサムネイル・Finder・撮影の FFI は段 9 より前からのもの）。Swift の側は `!`・`try!`・`as!`・あふれる演算・確かめない変換を使わない。Swift の trap・Apple のライブラリの中のクラッシュは受け止められず、アプリごと落ちる（同じプロセスで動かすのはユーザーの決定。サムネイルと同じ）。ImageIO・VideoToolbox の中で展開が戻らないこともある（壊れた HEIC。Apple 側の不具合で sips も止まる）ので、展開は専用のスレッドで行い、時間切れ（20 秒 ＋ 0.25 秒 × ⌈画素 / 10⁶⌉。上限の 2²⁸ 画素で 87.25 秒）を過ぎたら、スレッドとハンドルを見捨てて `Decode` を返す。待つあいだも 20 ms ごとに中止を確かめ、頼まれたら見捨てて `Canceled`。見捨てたハンドルは閉じない（戻ればスレッドの中で閉じる）。戻っていないスレッドが 4 本に達したら、Platform の画像の段を `Unsupported` にする（呼ぶ前に確かめるだけなので、同時に呼ぶと超えることがある）。サムネイルの経路（`kura_image_thumbnails`）は今も止まりうる（取り込みの段で扱う）。
- **ImageIO**（`imageio-dec/1`）：HEIC・HEIF・AVIF（`public.heics`・`public.avis` のシーケンスはフレームの列、ほかは主画像 1 枚）、TIFF・BMP、固定の段が断った CMYK の JPEG・ZIP の PSD。全体を 1 回だけ展開し（HEIC は帯ごとに切り抜いて読むと値が変わる）、展開できたかは取り出したデータが NULL でないことで確かめる（ImageIO は切れた・壊れた入力でも成功を返し、16 ビットの PSD は展開できないまま成功を返す）。色空間はそのまま、ImageIO の色空間の ICC を運ぶ（CMYK・Lab だけ sRGB に変換して sRGB の ICC）。浮動小数・HDR は `Unsupported`。EXIF は運ばない（ImageIO に生の EXIF を返す口がない。HEIF の Exif 項目を読むのは後の段）。静止画の PNG を ImageIO で読んだ値は png クレートと画素でビット一致する（リポジトリのテストで 81 例：生成した 9 形式 × 4 つの大きさの 36 例と ICC・向きの 1 例、コーパスの 44 例（1/2/4 ビット・グレーと RGB と 16 ビットの tRNS・tRNS なしのパレット・インターレース・gAMA・cHRM・sRGB・sBIT・壊れた ICC・P3 の ICC）。有効な ICC は同じバイト列）。パレットの BMP・TIFF の 5 例は、パレットから作った値と照らす。16 ビットのプレマルチプライドは、vImage の戻しが kura の式と 1 違う組が、すべての (c, a)（0 ≤ c ≤ a ≤ 65535・a ≥ 1、約 21.5 億）のうち 7,357（約 29 万に 1。a ≥ 28,549 にだけ出る）あるので、戻しは Rust の式で行う（8 ビットは全組で同じ）。メモリは値の約 3 倍を見込む。
- **AudioToolbox の読み取り**（`audiotoolbox-dec/1`）：メモリから `AudioFileOpenWithCallbacks` と `ExtAudioFile` で f32 に。m4a・aac は `aac `・`aach`・`aacp`、ogg・opus は `opus`、wav・aiff は PCM と ima4・ulaw・alaw・MS ADPCM・IMA（DVI）ADPCM。priming を切った長さ。3 チャンネル以上は配置が kura の並び（WAV の既定のマスク）と同じときだけ読む。AudioToolbox の Ogg Vorbis は先頭を 128 フレーム余分に切るので使わない。HE-AAC は同じ Mac でも実行ごとに値が揺れる（最大 4.8×10⁻⁷）ので、アンカーが前提。docs の以前の「AVFoundation」は、AVAssetReader が HE-AAC の長さを揺らしたので AudioToolbox にした。
- **AAC の書き出し**（`aac-audiotoolbox-enc/1`）：`ExtAudioFile` で m4a をメモリに。LC だけ、1〜6 チャンネル（7・8 は AAC の配置と kura の並びが合わないので書かない）、8〜48 kHz、既定 256 kbps（CBR・ABR・VBR を選べる）。`ClientChannelLayout` を必ず指定する（しないと 5.1 の後ろの右が LFE に入って消える）。iTunSMPB（priming 2,112）を書き、1・2 チャンネルは段 6 の読み取り（固定の段）でちょうどの長さとずれ 0 に戻る（3〜6 チャンネルは固定の段が読まないので、AudioToolbox の読み取りでちょうどの長さに戻ることを確かめる）。Apple が書く時刻は 0 に書き換える。同じプロセスなら同じバイト列だが、約束しない（R2 は偽）。
- **ffmpeg の段**（kura-core。External。アンカー）：読み取りは前の段が断った音声だけ（`opus-ffmpeg-dec/1`・`ogg-ffmpeg-dec/1`（Opus・FLAC・Speex）・`wav-ffmpeg-dec/1`・`aiff-ffmpeg-dec/1`（圧縮の WAV・AIFF）・`m4a-ffmpeg-dec/1`・`aac-ffmpeg-dec/1`・`mp3-ffmpeg-dec/1`（layer 1・2））。入力は `file:<絶対パス>` で渡し（mp4 の moov が後ろにあるものを読むため）、分離器とデコーダを強制して `-format_whitelist`・`-codec_whitelist` で同じ名前だけを許し、`-protocol_whitelist` は、読み取りは入力 `file`・出力 `pipe`、MP3 の書き出しは入力 `pipe`（標準入力の f32le）・出力 `file`（一時ファイル）。読み取りの出力は `pcm_f32le` の WAV を標準出力へ。標準エラーの `[error]` 以上の行は失敗にする（ffmpeg は壊れた Ogg でも終了コード 0 で短く出す）。ffprobe で形を確かめてから 1 回デコードして数え、形は `Exact`。Opus は libopus を優先する。MP3 の書き出し（`mp3-ffmpeg-enc/1`）は一時ファイルに書く（パイプでは Xing・LAME のフレームが書かれず gapless の長さが失われる）。既定は CBR 192 kbps（その版で選べなければ 192 以下の最大：MPEG-2 は 160、MPEG-2.5 は 64）、VBR は品質 0〜9（既定 2）、ID3 は書かない（ユーザーの決定）、1・2 チャンネル。`-ar`・`-ch_layout` を明示し（ないと黙って変換される）、標本は NaN → 0・±∞ → ±1・±1 に切り詰めてから渡す（LAME は NaN で abort する）。起動は環境変数を消し（`PATH=/usr/bin:/bin`・`LC_ALL=C` だけ）、新しいプロセスグループで（`tools::run_binary`）、見張りが中止・無出力・時間切れ・出力の上限でグループに TERM、2 秒後に KILL（1 回だけ。回収したあとに pgid が使い回されても、ほかのプロセスに送らないように。`kill` を起動できなかったときだけ、次の刻みで送り直す）する。子が終わったあとも中止・時間切れを見続け、グループの外に出た子孫がパイプを持って残るときは、止めたあとならすぐ（その理由のエラー）、止めていなければ子が終わってから 2 秒で KILL、さらに 2 秒で（`Stopped::Lingering`）、パイプを読むスレッドを見捨てて戻る。パイプに触るのは見捨てられるスレッドだけ。一時ファイル（入力の写し・MP3 の出力）は作業フォルダに、プロセスで 1 つの通し番号の名前で、ないことを確かめて作る（`create_new`。同じ作業フォルダの組が 2 つあっても、ほかのプロセスの残りがあっても、切り詰めたり消したりしない）。引数は `crates/kura-core/goldens-ffmpeg-args/`（追記だけ）に凍結し、引数を変えたら版を上げる（External のバイト列と値は凍結しない）。実物のテストは ffmpeg があるときだけ回る（`KURA_FFMPEG_DIR`・`KURA_FFMPEG_REQUIRED=1`。CI の macOS の job は Homebrew の ffmpeg を入れて必ず回し、Linux の job は回さない）。ffmpeg は同じ名前のコメントを `;` でつなぎ、MP2・ADPCM の長さは ffmpeg が出したままになる（固定の段と扱いが違う）。
- **symphonia の Vorbis・AAC を再現できるように**（ユーザーの決定。ADR-153）：symphonia-core（IMDCT・FFT の表）・symphonia-codec-aac（窓・TNS・逆量子化とスケールファクターの表・pulse）・symphonia-codec-vorbis（窓・floor 0・`float32_unpack`）の写しを `third_party/` に置いて std の超越関数を `libm =0.2.16` に差し替え（f32 の関数は f64 にして libm の f64 の関数で求めて f32 に丸める）、symphonia-codec-vorbis と symphonia-format-ogg の `lookup1_values` を libvorbis と同じく整数で求めるようにした。識別子は `ogg-vorbis-symphonia-dec/3`・`m4a-aac-symphonia-dec/2`・`aac-adts-symphonia-dec/2`（公開前なので旧版は残さない）、`reproducible()` は真、デコーダのゴールデンに値を凍結した。aarch64 と x86_64 でビット単位で同じ値（壊れた入力で NaN が出るときも、NaN を 1 つのビット列にそろえる）。AAC の pulse と Vorbis の floor 0 の差し替えは、値のゴールデンで照らしていない（そのフィクスチャがない。ソースの走査だけが守る）。自作の Ogg Vorbis のコーデックのゴールデンの読み直しの行（`/2` で値を持たない 35 行）は書き換えず、`/3` で形だけを照らし、`/3` で読み直した値は新しい名前の行（`<名前>-dec3`）で凍結した。floor 0 の Vorbis は実ファイルで照らしていない。
- **テスト**：kura-mac（macOS の CI）で ImageIO と png のビット一致、AAC を段 6 の読み取りで読み直してちょうどの長さ・ずれ 0、3〜6 チャンネルの並び（段の順に任せても、形も読みも AudioToolbox が答える）、壊れた入力・上限が展開の前に効く・中止・8 スレッドの同時の呼び出し・ABI の照合。kura-engine で偽の backend の adapter、能力の表、E29（`tests/e29.rs`）とアンカー（`tests/anchor.rs`。実際の SVG・Vorbis・AAC の値とランダムな 200 値の往復、凍結した版 1 のアンカー、崩したアンカー、上限）、アンカーのコーデックのゴールデン（`anchor-kura-dec@1.jsonl`）。kura-core で ffmpeg の引数のゴールデンと、ffmpeg があるときの実物のテスト。ファズの対象 `decode_anchor`。
- **アンカーのゴールデン**：`anchor-kura-dec@1.jsonl` はその場の書き手で書いて読むので、値の作り方を守るだけ。決まったバイト列から読めることは、凍結したアンカー `fixtures/anchor-v1-*`（17 本：9 つの画素の形式・パレット・ICC と EXIF のある・ない・シート・16 ビットと 8 ビットのフレームの列・ステレオとモノラルと 6 チャンネルの WAV）が守る。kura-mac のフィクスチャ（`crates/kura-mac/fixtures/`）も追記だけの検査の対象。
- **後の段に回したもの**：HEIF の EXIF、RAW、HDR、ゲインマップ・プレマルチプライドの HEIF のアルファ（ImageIO が黙って捨てる）、アンカーの保存への結線（M12）、RF64（4 GiB を超える WAV）、ffmpeg のタグの表をエンジンの表と共有すること、Opus の書き出し。

## 10. 描画の道具

```rust
pub struct PaintParams { pub actions: Vec<PaintAction> }  // 追記だけ
pub enum PaintAction {
    Stroke { tool: Pen | Eraser, color: Option<PaintColor /* {"rgba":[..]} | {"index":n} */>, blend: Replace | Over,
             brush: Brush { shape: Square | Circle, size: 1..=255 }, pixel_perfect: bool, frame: u32,
             points: PointList /* 画素の整数座標。詰めた文字列で保存する */ },
    Fill   { at: IPoint, color: PaintColor, tolerance: u8, connectivity: Four | Eight, contiguous: bool, frame: u32 },
}
```

規則は `crates/kura-engine/src/paint/`（`stroke.rs`・`fill.rs`・`points.rs`）に凍結し、フロントの `src/lib/paint.ts` が同じ規則を実装する。

- **線**：点と点のあいだをブレゼンハム（両端を含む。向きは点の順）でつなぎ、区間の継ぎ目と続けて同じ点は 1 つにする。
- **ピクセルパーフェクト**：最後に残した点 p・いまの点 c・次の点 n が L 字（p と n が斜めに隣り合い、c がどちらとも縦か横で並ぶ）なら c を除く。最初と最後の点は除かない。
- **筆の形**：大きさ s の s×s のブロックを、左上が点から `⌊(s−1)/2⌋` だけ左上になるように置く。四角は全部、円はブロックの中の (i, j) のうち `(2i+1−s)² + (2j+1−s)² ≤ s² − s` のもの（3 は十字、4・5 は四隅を欠く）。
- **ストローク**：道の各点に筆の形を押した画素の和集合を求め、各画素に 1 回だけ色を置く。重なっても濃くならず、ストロークの中の順番に左右されない。
- **ペン**：置き換えは色をそのまま置く。重ねるは `imaging::composite::over` の整数の式（`div255` 相当の四捨五入）で 1 回だけ重ねる（アルファのない形式は、アルファ付きにして重ねてから戻す）。
- **消しゴム**：アルファを 0 にする（色はそのまま）。Indexed8 はパレットの透明な色（アルファ 0 の最初の色）の番号にし、なければ `Unsupported`。アルファのない形式では使えない。
- **塗りつぶし**：起点の色と各チャンネルの差がすべて許容差以下（16 ビットは 257 倍）の画素のうち、起点から 4 連結か 8 連結でたどれるもの（`contiguous` が偽なら画像全体の同じ色）。連結成分として定義するので、たどる順番に左右されない（E22）。Indexed8 は番号で比べ、許容差は 0。起点が画像の外なら何もしない。起点の座標は ±2^31 まで（読み込みで断る）で、トリムしたフレームの位置を引いてあふれる場合も「外」として扱う。実装は行を 64 ビットの語にそろえたビットの表で、同じ色の続き（区間）ごとに広げる（4096² の全面で 7 ms）。区間の待ち行列は、まだ塗れる画素がある範囲だけを積み、「画素 2 つに 1 要素 ＋ 行の数の 2 倍 ＋ 65,536」までにする（市松模様の 8 連結を中央から塗ると山は画素の約 0.3。4×4 の周期の模様をすべて試した最悪。超えたら落ちずに `Limit`）。
- **色の引数**：Indexed8 は番号（置き換えだけ）、ほかは RGBA8。グレーの形式は灰色だけ、置き換えでアルファのない形式は不透明な色だけ。形式が分かる推論の段階で確かめる（パレットの番号の範囲は実行で確かめる）。
- **フレーム**：`frame` はフレームの番号（画像は 0）。座標はキャンバスの座標で、トリムしたフレームではフレームの外は描かない。
- **差分の適用**：引数が追記だけなので、前の出力に新しい動作だけを当てられる（`Op::apply_appended`）。全体を当て直した結果とビット単位で一致する（E21。`paint.actions` の例のすべての切れ目で確かめる）。
- **スポイト**：操作ではない。描画結果の 1 画素を返す問い合わせ（`paint::sample(value, frame, x, y) -> Option<Sample { rgba, index, raw }>`。16 ビットは四捨五入で 8 ビット）で、履歴に入らない。縮小したプレビューからは拾わない（E23）。
- **描いている最中**：フロントが同じ規則で仮の画素を重ね描きし、確定したら Rust の描画結果に置き換える。両者を照らすテストベクタ（`crates/kura-engine/tests/vectors/paint/*.json`）を Rust と TS で共有する。
- **前の操作との関係**：描画より前の操作（切り抜きの位置など）をあとから変えても、描画の座標は描画のノードの入力の座標のまま動かない。エディタは「後ろの描画の位置はそのままです」と知らせる。
- **点の列の詰め方**：最初の点の x・y と、2 つ目からの差 dx・dy を zigzag の LEB128 にして base64url（`=` なし）にする。読むときは詰め直して同じ文字列になるものだけを受け付ける（書き方を 1 つにする）。
- **上限**：`paint.actions` のノード 1 つあたりの手間は `MAX_PAINT_WORK`（2³⁰）まで（推論で断る。超えた時点で数えるのをやめる）。手間は、ストロークが「道の点の数の上限 × 筆の画素の数 ＋ 画素の集合の大きさ（外接矩形と画像の重なり ÷ 64）＋ タイルの数」、塗りつぶしが「画像の画素の数」。1 本のストロークの中でも、筆を押した画素 65,536 個ごとに中止を確かめる。上限はノードごとなので、文書全体の手間はノードの数の上限（`max_nodes`）との積で抑えられる。文書の大きさは `doc::MAX_DOC_BYTES`（16 MiB）まで、入れ子の深さは `digest::MAX_DEPTH`（100 段。serde_json の読み込みの 128 段より浅い）まで。読むときは生のバイト列と保存の形（正規の JSON。移行と既定値を入れたあと）の両方で、保存するときは保存の形で確かめる。保存の形の大きさと深さは、書き出さずに数える（serde_json の直列化器に正規の数と文字の書き方を付けて、書いたバイト数を数える。並べ替えも写しも作らない）ので、読み込みと保存で同じ判定になり、読めた文書は必ず保存でき、保存できた文書は必ず読める。筆 1,000 本（各 50 点）の文書が約 0.3 MB なので、5 万本ほどまで入る（ADR-147）。

## 11. 解析

段 8 で入れた（ADR-152）。詳しい定義は各解析のソースの冒頭（`crates/kura-engine/src/analysis/{image,audio}/*.rs`）に書き、ここは要点だけ。

### 11.1 trait と登録

```rust
pub trait Analysis: Send + Sync + 'static {
    const ID: &'static str;            // "image.alpha"（<種類>.<名前>。英小文字・数字・_）
    const VERSION: u16;                // 結果が変わる変更で上げる（1 から）
    const ACCEPTS: &'static [ValueKind];
    const NUMERIC: Numeric;            // 整数だけ（Exact。全ターゲットで同じ）か浮動小数（Float）か
    const INTERNAL: bool = false;      // 内部用（カタログ・DB に出さず、操作の申告だけに使う）
    type Params: OpParams;             // 既定値つきの引数（知らない項目・範囲の外は拒む）
    type Output: AnalysisOutput;       // 結果の正規の形（正規の JSON か、決めたバイナリ）
    fn run(p: &Self::Params, v: &Value, cx: &AnalyzeCx<'_>) -> Result<Self::Output, EngineError>;
    fn examples() -> Vec<AnalysisExample<Self::Params>>;   // 適合テストとゴールデンの種
    fn numeric_for(p: &Self::Params) -> Numeric;
    fn estimate(p: &Self::Params, input: &Shape) -> Estimate;  // 結果の大きさと手間の見積もり（上限で断る）
}
```

- 登録は `register_analyses!`（`current`・`legacy`・`dropped`）。画像は `analysis/image/mod.rs`、音声は `analysis/audio/mod.rs` の一覧に 1 行足す。解析を 1 つ足すのに触るのは、`analysis/<種類>/<名前>.rs`、一覧（`<種類>/mod.rs`）の `mod` と `current` の行、E26 の表（下）の 1 行、ゴールデン `goldens-analysis/<ID>/v1.jsonl`（`KURA_BLESS=1` で作る）だけ。
- `AnalyzeCx`：中止（`check`）・上限（`reserve`）・並列（`par_blocks`）。乱数は渡さない。決定性の規則（§8.2）は操作と同じ。
- **結果の正規の形**：正規の JSON（§4.4）。非有限の数は持たない（JSON に書けず、「無音」と「計れない」が同じ形になるため）。浮動小数の値は `Real`（有限なら数、NaN・±∞ は `"nan"`・`"inf"`・`"-inf"`）か、理由付きの列挙 `Reasoned<T, R>`（`{"kind":"value","v":…}` か `{"kind":"<理由>"}`）で持つ。波形の段だけは決めたバイナリ（`kura-peaks`）。
- **ダイジェスト**：`kura-analysis-v1` = BLAKE3（ID（長さ付き）‖ 版 u32 ‖ 正規の引数（長さ付き）‖ 形の種類 u8（JSON 0・バイナリ 1）‖ 結果（長さ付き））。ゴールデンは結果だけの `kura-analysis-out-v1` で照らす。
- **版の契約**：操作が申告する解析（下の 11.2）は操作と同じく凍結し、結果を変えるなら版を上げて旧版を `legacy` に残す（申告されている版を `dropped` にするとエンジンを作れない。`Engine::new` の `check_needs`）。表示だけの解析（どの操作も申告しない解析。今は申告されるのが `image.opaque_bounds`・`audio.loudness_gating` だけなので、ほかの 8 つ）は、版を上げたら旧版の実装を消して `dropped` に載せてよい（凍結したゴールデンのファイルは残し、照らさない）。操作が新しく申告するようになったら、その版から凍結する。実装も `dropped` もない版のゴールデンは失敗。
- **フレームの列**：フレームの外（覆われない部分）の扱いは解析ごとに決め、各ソースの冒頭と下の表に書く（alpha は透明として数え、colors は数えず、pixel_scale はフレームの中だけで比べ、opaque_bounds は外を透明とみなすが、アルファのない形式では union をキャンバス全体にする（frames は各フレームの矩形のまま））。
- **結果のキー**（M11 で使う）：中身 × ID × 版 × 正規の引数。中身を元のファイルのハッシュにするか値のダイジェストにするか（再現を約束しないデコーダの元をどう扱うか）は M11 で決める。

```rust
impl Engine {
    pub fn analyze<A: Analysis>(&self, v: &Value, p: &A::Params, mon: &dyn Monitor) -> Result<A::Output, EngineError>;
    pub fn analyze_dyn(&self, req: &AnalysisRequest, v: &Value, mon: &dyn Monitor) -> Result<Analyzed, EngineError>;   // ID・版・引数の JSON から
    pub fn analyze_many(&self, v: &Value, reqs: &[AnalysisRequest], mon: &dyn Monitor) -> Vec<Result<Analyzed, EngineError>>;
    pub fn analysis_catalog(&self) -> Vec<AnalysisDescriptor>;   // 内部用を除く
}
```

値だけを受ける（ファイルを読むのは kura-core。読んだときの `DecodeStamp` を一緒に記録する）。型付きの `analyze` も `analyze_dyn` と同じ道（引数の正規化と範囲 → 受け付ける種類 → 値の上限 → 見積もり）で確かめ、正規の形から読み直した引数で解くので、同じ要求なら同じ結果・同じエラーになる。`Estimate` の `bytes` は解く前に上限と照らす（`cost` は本番では使わず、ファズで重い解析を飛ばす目安と E24 の中止の回数の下限に使う）。

### 11.2 操作が解析を申告する口

- 操作は `Op::NEEDS`（申告しうる (ID, 版) の一覧）と `Op::needs(p, 入力の形) -> Vec<Need>`（どの入力にどの解析を、どの引数で）を申告する。実行器は `compile` で申告を確かめ（`NEEDS` にある・登録済み・入力の種類を受け付ける・引数が正規・見積もりが上限内）、描くときに各ノードの `apply` の前に解いて、`ApplyCx::analysis::<A>(入力, 値, 引数)` で `Arc<A::Output>` を渡す。申告していない解析を使うと `Internal`。描く前に解いていない解析をその場で解いたときも `Internal`（申告と使い方の食い違い）。外したノードは解かない。`Lift::PerFrame` の操作は申告できない（登録で断る）。申告は操作の版ごとに固定し、今の版の操作は解析の今の版を申告していなければならない（`Engine::new` の `check_current_needs`。旧版・退役した版の操作は除く。試験用の `with_registries` は確かめない）。申告される解析の版を上げるときは、申告する操作の版も上げて、新しい版で今の版の解析を申告する（§8.4）。
- 段 8 で、`audio.normalize` v1 は内部用の `audio.loudness_gating` を、`image.trim_alpha` v1 は `image.opaque_bounds` を申告して使う形に移した（値は同じで、操作の版・ゴールデンは変えていない）。表示する値（`audio.loudness`・`image.opaque_bounds`）と操作が使う値は、同じ関数を通るので必ず一致する（E26）。
- 実行器は解いた結果を `Solver` で受け渡す。段 10 のキャッシュはここに「入力の指紋 × ID × 版 × 引数」で入る。操作が使うのはメモリ上で解いた結果だけで、DB に置いた結果は使わない。

### 11.3 画像の解析（どれも整数だけ。タイルごとに数えて和・和集合でまとめるので、スレッド・タイルの分け方によらない）

| 解析 | 引数 | 結果 | 使い手 |
|---|---|---|---|
| `image.alpha` | なし | `kind`（none / binary / partial）、透明・半透明・不透明の画素数、フレームの列で覆われない画素数 `uncovered`（kind に効く） | 市松模様の表示、保存の前の警告（`AlphaOutput::loss_in(MetaCaps.alpha)`：二値になる・消える）、検索 |
| `image.opaque_bounds` | `threshold`（0〜254、既定 0。トリムと同じ） | `canvas`・`union`（全部透明なら null。アルファのない形式のフレームの列はキャンバス全体：トリムが何もしないことに合わせる）・`frames`（フレームごとの矩形） | トリム（申告して使う）、余白の表示 |
| `image.colors` | なし | 色の数（257 で打ち切り）、256 色以下なら色の並び、`visible`（α = 0 をひとつにまとめた数）、`paletteLossless`、Indexed8 のパレットの `len`・`used` | PNG-8・GIF の候補、検索 |
| `image.pixel_scale` | なし | 軸ごとの `k`（倍率）・`phase`（位相）・`lines`、`lossless` | ドット絵の表示（最近傍）、「1/k に戻す」 |
| `image.sprite_grid`（推定） | なし | 背景（透明か四隅の色）、`best`（`sprite.split_grid` にそのまま渡せる `GridParam`、列・行・中身のあるセルの数・当てはまり方）、候補 3 つまで | 分割の候補 |

- **alpha**：16 ビットは元の深さで数える。Indexed8 は使われている番号のパレットの α だけを見る。全部不透明な RGBA は none（形式でなく中身で決める）。
- **colors**：`image.convert` が Indexed8 にするときと同じ 8 ビットの色で数える（速い読み手 `imaging::convert::for_each_rgba8_key` を使い、convert の `convert_pixel` と同じ色になることを単体・E26・素朴な参照で照らす）。256 色以下なら、色は詰めた値の小さい順で、convert が作るパレットと同じ。フレームに覆われない部分は数えない（alpha は数えるので、同じ値で「透明あり・透明な色なし」になりうる）。タイルごとに固定の大きさの表で数え、最後に並べ直す。
- **pixel_scale**（ユーザーの決定）：全行・全列の色の変わり目の位置（合併）の差の最大公約数を倍率 `k`、変わり目の位置の余りを位相 `phase` とする（端の欠けたブロックは数えない）。位相をずらして切り抜いた絵でも倍率が出る。同じ色の連続の長さの最大公約数（以前の定義）は、切り抜きで 1 に落ち、レンガ積みの模様を倍率と取り違える。厳密な定義だけで、JPEG・平滑な拡大・あとから書き足した 1 点では 1 に落ちる（許容差つきの推定は、それを使う機能を作る段で別の解析にする）。最近傍で k 倍すると `k`・`phase` がちょうど k 倍になり、`lines`・`lossless` は変わらない。変わり目が 1 本以下の軸は null。フレームの列は各フレームの中だけで比べ、キャンバスの座標で合併する（フレームの縁と覆われない部分の境は変わり目に数えないので、同じ見え方の 1 枚の画像と結果が違いうる。例：キャンバスの (1, 1) に置いた 4×4 の赤のフレームは null、同じ見え方の画像は k 4・位相 1）。
- **sprite_grid**：透明（なければ四隅と同じ背景色）を空とし、軸ごとに周期で畳んで「詰めた格子・周期の格子」を作り、整数の順位（中身のあるセルの多さ、セルの数、余白の整い方、空の割合、当てはまり方、位置、幅、周期）で 1 つ選ぶ。中身のあるセルが 2 つ以上の格子だけ。周期の上限 4,096・候補 3 つは版に凍結する定数。推定（合成のシートで確かめた。ラベル入り・はみ出しなどの実物では外れうる。中身がセルの縁に届かないシートでは、セルの大きさが画素から決まらない）。背景色のシートでは `split_grid` の `skipEmpty`（透明しか見ない）が背景だけのセルを残す。フレームの列は受けない（1 枚の画像だけ）。
- **ドット絵かどうか**の判定は解析に入れない（倍率・色数・アルファの事実から M11 が決める）。
- フォント・3D の情報（書体名・字数、頂点数・大きさ）は kura-engine の解析（値を受ける）ではなく、M11 で kura-core・kura-mac か軽いパーサーで読む（§9.2 の「解析だけ」）。
- 速さ（16384²、release・16 コア、負荷のある中の目安）：alpha 15〜41 ms、opaque_bounds 約 33 ms、colors 5〜82 ms（最悪は画素ごとに色が違う形）、pixel_scale 53〜168 ms、sprite_grid 139〜218 ms。値の大きさに比例する確保はしない。

### 11.4 音声の解析

| 解析 | 引数 | 結果 | 使い手 |
|---|---|---|---|
| `audio.loudness` | なし | `integrated`・`range`（LRA）・`samplePeak`・`truePeak`・`momentaryMax`・`shortTermMax`。どれも `Level`（計れた値か、計れない理由） | 表示・検索、正規化の値と一致（E26） |
| `audio.loudness_gating`（内部用） | なし | 積分・ピーク・ブロックのパワーの列・計れるか | 正規化（申告して使う） |
| `audio.peaks` | `finest`・`coarsest`（256・1,024・4,096・16,384・65,536 から。既定は 5 段とも） | 1 ビンあたりのフレーム数ごとの min/max（i16）の段（バイナリ `kura-peaks`） | 波形の表示（M11 の kura:// /peaks） |
| `audio.silence` | `thresholdDb`（−120〜0、既定 −60） | 音のある範囲 `content`（全部無音なら null）・前後の無音の長さ・クリップの列の数・1 を超える標本の数・NaN と ∞ の標本の数 | 自動トリム、警告 |
| `audio.loop` | なし | ループ点（書き出しと同じ判定）・マーカー（位置の順、1,024 個・名前 256 バイトまで）・落とした数 | ループの表示と試聴 |

- **loudness_gating**：見積もりは、計れないレート（3,363 Hz 以下。`loudness::measurable_rate`、K 特性を作れる境と同じ関数）ではブロック 0 個として数える（段 6 で描けた低いレートの長い音声の正規化を、計画で断らないため。描くと入力のまま `LoudnessUnmeasurable`）。
- **loudness**：段 6 の `loudness::analyze`（BS.1770。正規化と同じ関数）に、LRA・最大のモーメンタリ・最大の短期を足した。理由は優先の順に nonFinite（NaN・∞ を含む）・lowRate・tooShort・silent（計るチャンネルのブロックのパワーがすべて 0）・belowGate。積分と最大のモーメンタリは 4 区間（400 ms）、LRA と最大の短期は 30 区間（3 秒）より短いと tooShort（区間は ⌊(fs + 5)/10⌋ 標本）。400 ms より短い効果音は、規格どおり積分ラウドネスを出さず、ピークだけを出す（ユーザーの決定。正規化 v1 が何もしない条件と一致する）。最大のモーメンタリ・短期はゲートなしの最大。
- **LRA**（EBU Tech 3342 v4。ユーザーの決定で規格どおり 0.1 秒ごと）：短期ラウドネスを 30 区間（3 秒）の窓で 1 区間（0.1 秒）ずつずらして取り、−70 LUFS の絶対ゲートと「平均 −20 LU」の相対ゲート（どちらも「≥」。積分ラウドネスのゲートは BS.1770 の「>」のまま）を通した値の、10 と 95 パーセンタイル（順位は補間なしの ⌊((n − 1)·p + 50)/100⌋）の差。EBU の試験 1〜4（10・5・20・15 LU）は 48 kHz・44.1 kHz でちょうどの値。libebur128・ebur128 クレート・ffmpeg の `loudnorm`（libebur128 の移植）は短期を 1 秒ごとに取るので（規格の「2.9 秒以上の重なり」を満たさない）、音楽では 0.1〜0.4 LU 違う。ffmpeg の `ebur128` フィルタは 10 Hz で取り、kura と同じ値になる（1 秒ごとにした内部の関数は、dev 依存の ebur128 クレート（=0.1.10。libebur128 の移植）と 1e-6 LU で一致する）。EBU の試験 5・6 は実際の番組の音声なので、テストにない。
- 段 8 で `loudness::analyze` を値を変えずに速くした（K 特性・トゥルーピーク・サンプルピーク・有限の確かめを 1 つの `par_blocks` で。段 6 の実装をテストの参照に残し、ビット一致を確かめる）。5 分の 44.1 kHz ステレオで 0.109 → 0.061 秒（16 スレッド）。IIR を時間で割る並列化はしない（値が変わる）。
- **peaks**：16 ビットで書き出すときと同じ格子（`pcm::quantize(x, 16)`）で i16 の min/max を取る。標本から作るのは最も細かい段だけで、上の段は下の段 4 つから作る（整数なので並列・ターゲットで変わらない）。NaN は 0 として含め、ビンの数は ⌈N / B⌉。`kura-peaks` 版 1：ヘッダ 32 バイト＋段の表（1 段 24 バイト）、データはビンが外側・チャンネルが内側の [min, max] の i16（リトルエンディアン）、段の間は 8 バイト境界まで 0 で詰める。5 分の 44.1 kHz ステレオで、5 段 550,872 バイト・4,096 以上の 3 段 34,024 バイト。読み手（`from_canonical`）はファズにかける（`decode_peaks`）。
- **silence**：どれかのチャンネルで |x| が閾値を超える最初と最後のフレーム（NaN は無音、±∞ は音あり）。クリップは 16 ビットの格子の同じ端に 3 標本以上続く列（Audacity の既定と同じ）。閾値の dB を倍率に直すのが浮動小数なので、数値の性質は Float。将来の無音のトリムは同じ解析を同じ閾値で申告する（E26：解析の範囲でトリムすると、余白が 0 になる）。
- **loop**：`writable_loop`（`start < end ≤ 長さ`。書き出しと同じ判定）を通したループ点。表示するループ点と保存されるループ点が一致する。
- 速さ（5 分の 44.1 kHz ステレオ、release・16 コア）：波形の段 1.8 ms・ラウドネス 45 ms・無音 18 ms、4 つで 66 ms（1 スレッドで 0.47 秒）。WAV の読み取りを含めて 0.19 秒、FLAC で 0.42 秒（§16 の「5 分の曲の解析 2 秒」）。解析は値を丸ごと受けるので、値の上限（`max_value_bytes`）を超える長さ（48 kHz ステレオで約 3 時間）は解析できない。

### 11.5 テスト

- 適合（`tests/analysis_conformance.rs`）：登録されたすべての解析の例で、スレッド 1・3・16 で同じ結果（E1）、中止で `Canceled`（E24）、上限（E25）、正規の形の往復、引数の検証、受け付けない種類。
- ゴールデン：`goldens-analysis/<ID>/v<版>.jsonl`（1 行に `{"digest","input","name","output"?,"params","target"}`。`output` は JSON で 2 KiB 以下のときだけ書く。`target` は `numeric_for`）。追記だけ（`scripts/check-goldens-append-only.sh` が見る）。足りない行は `KURA_BLESS=1 cargo test -p kura-engine --test analysis_golden`。
- E26（`tests/analysis_ops.rs`）：(a) 登録されたすべての操作の例で、申告した解析を実行器が渡した値と `Engine::analyze_dyn` で別に求めた値が一致し、偽の値を渡すと出力が変わる（申告した解析を実際に使っている）。偽の値は欄ごと（`FAKES`。JSON は欄を書き換え、バイナリは型付きの値を `Perturb::Typed` で変える）で、操作が読むと申告した欄（対の表の `reads`）はどれかの例で出力を変え、読まない欄はどの例でも変えない。偽の値が読めない・元と同じなら、テストの誤りとして止める。`NEEDS` の各 (ID, 版) は、どれかの例の計画に載らなければならず、`NEEDS` のある操作で計画が空になる例は、理由つきの許可の一覧 `EMPTY_PLAN` に載せる（今は `image.trim_alpha` の `rgb-is-untouched`：アルファのない形式は解かない。一覧にない空の計画も、空でない計画の古い行も失敗）。音声の操作は核（`audio_kernel`）の経路も同じ偽の値で照らす（`kernel_registry`）。`src/ops/**` が申告で受け取るべき計算（解析の `run`・`analysis::loudness`・`imaging::bounds` の範囲・`Engine` と `analyze`・`analyze_many`）を直接呼ばないこと、`analysis` を別名にする `use` がないことを、ソースの走査で確かめる（関数の名前は呼び出し・`use`・パスのときだけ断り、変数・欄の名前は断らない。型の名前はどこでも断る。テストのモジュールは属性を挟んでも名前が何でも除く。部品として使うものは理由つきの許可の一覧 `DIRECT_OK`）。(b) 意味の対の表（`tests/analysis/pairs_{image,audio}.rs`）：トリム ↔ opaque_bounds、正規化 ↔ loudness・loudness_gating、convert ↔ colors（パレットが同じ）、convert ↔ alpha（形式の能力）、最近傍の resize ↔ pixel_scale、split_grid ↔ sprite_grid（推定した格子で分けると中身を落とさない）、trim ↔ silence、set_loop ↔ loop。(c) 覆いの検査：登録されたすべての解析は、対の表か「対のない解析」の表（今は `audio.peaks`）に載る（どちらにもなければ失敗）。対の表の行は (操作, 版, 解析, 版, reads, 検査) で、操作の今の版と申告のある旧版をすべて照らし、解析は登録表で引く（型の名前で引かない）。
- 境目のテスト：ゲートの「ちょうど」（LRA の相対ゲートの「≥」、積分の絶対ゲートと相対ゲートの「>」と `gain_to` の区間の端）、LRA の相対ゲートの平均が絶対ゲートを通ったブロックだけで取られること、colors の 256 色ちょうど・257 色ちょうど（1 タイル・タイルをまたぐ）、`kura-analysis-v1` の式の凍結（`analysis::tests::digest_formula_is_frozen`）、型付きと ID からの口の一致と低いレートの正規化（`tests/analysis_core.rs`）。境目の例（pixel_scale の lossless 偽・片方の軸だけ null、colors の visible < count、NaN・∞ を含む音声、逆向き・範囲の外のループ点など）もゴールデンに凍結した。
- ファズ：`testing::fuzz_decode` は読めた値に、見積もりの手間が 2²⁴ 以下のすべての解析を当て、結果が見積もり以下であることを確かめる。

## 12. 実行・プレビュー・セッション

```rust
impl Engine {   // 段 8 までにあるもの
    pub fn load(&self, raw: &[u8]) -> Result<Graph, EngineError>;               // 読み込みとスキーマの移行
    pub fn compile(&self, g: &Graph, input_shapes: &BTreeMap<InputId, Shape>) -> Result<Plan, Issues>; // 版の解決・引数の移行と検証・形の推論・解析の申告・資源の見積もり
    pub fn render(&self, plan: &Plan, inputs: &BTreeMap<InputId, InputValue>, mon: &dyn Monitor) -> Result<Rendered, EngineError>;
    pub fn render_with_solver(&self, plan: &Plan, inputs: &BTreeMap<InputId, InputValue>, mon: &dyn Monitor, solver: &dyn Solver) -> Result<Rendered, EngineError>;   // 申告した解析を解く口（§11.2）
    pub fn analyze<A: Analysis>(&self, v: &Value, p: &A::Params, mon: &dyn Monitor) -> Result<A::Output, EngineError>;   // §11.1（analyze_dyn・analyze_many も）
}
// 書き出しは今は CodecSet::encode_bytes（§9.1。EncodedStamp を返す）。ノードのキャッシュ（NodeCache）を渡す render は段 10 で足す。
```

- **キャッシュ**：ノードの指紋（`blake3(親の指紋 ‖ 操作 ‖ 版 ‖ 正規化した引数 ‖ 解像度の段)`）で途中の値を覚える。正しさには関わらない。編集中のノードの入力は固定して、引数を動かしても前段を計算し直さない。指紋は版をまたいで保つ必要はない（種は `Node.seed` に別に持つ）。
- **プレビューの段**：写真は縮小した段で描く（操作が引数を縮める）。ドット絵（`image.pixel_scale` の倍率が 2 以上、M11 の判定、または利用者の指定）は常に等倍で描き、拡大は CSS の `image-rendering: pixelated` に任せる。大きなシートは範囲（ROI）を等倍で描く。
- **変わった範囲**：推論で変わった範囲を求め、フロントは変わったタイルだけを取り直す。
- **予約**：最新の要求だけを生かし、古い描画はタイルの合間で中止する。プレビュー用と一括加工用のスレッドプールを分ける。
- **メモリの予算**：重み付きのセマフォ（バイト）で管理する。既定は物理メモリの 1/4、上限 4GiB。エディタと一括加工で共有し、一括加工はヘッダから見積もった量を描く前に確保する。
- **セッション**：`EditSession` は純粋な構造で、文書のスナップショットの列を持つ（逆操作は持たない）。スナップショットはノードを共有するので軽い。スライダーの連続した変更は 1 手にまとめ、ドラッグ中の値は履歴に残さない。エディタの中の ⌘Z・⇧⌘Z はこれで実現する（ADR-141）。
- **配信**（M12）：`kura://localhost/edit/{session}/{rev}/…`（画像のタイル・全体、音声の WAV・PCM・波形）と `/peaks/{hash}`。中身から決まる URL なので変わらない。CORS はアプリの origin だけ、受け付けるのは main の WebView だけにする。

## 13. エラー・中止・上限・安全

```rust
pub enum EngineError {
    Invalid(Issues),                 // 検証（ノード・引数・理由。未知の操作・新しすぎる版もここ）
    Unsupported(String),             // 扱えない形式など
    Limit(String),                   // 上限（推論の段階でも、描くときの入力の照合でも出す）
    Canceled,
    Decode { codec: String, message: String },   // どの入力かは、読む側（kura-core のジョブ）が付ける
    Encode { codec: String, message: String },
    CodecMissing { tool: String },   // kura-core の ToolMissing へ
    Mismatch(Mismatch),              // 作り直しで記録と食い違った（SourceChanged・DecoderGone・AnchorBroken。段 9。§9.4）
    Io(std::io::Error),
    Internal(String),                // 不変条件が破れた・操作の推論と実際が違う（panic しない）
    AtNode { node: NodeId, source: Box<EngineError> },  // 描くときにノードで起きた Unsupported・Limit・読み書きのエラー（段 5）
}
pub trait Monitor: Sync { fn canceled(&self) -> bool; fn progress(&self, done: u64, total: u64) {} }
pub struct Limits { max_dim, max_pixels, max_frames, max_value_bytes, max_audio_frames, max_channels, max_nodes,
                    max_sample_rate /* 768kHz */, max_markers /* 1 万 */, max_meta_bytes /* 16MiB：ICC・テキスト・名前の合計 */ }
```

- 警告（メタデータを落とした、16 ビットを 8 ビットにした、sRGB とみなして混ぜた）は `Rendered.warnings` に入れ、保存の確認に出す。
- 描くときのエラーには、どのノードで起きたかを付ける：`Invalid` は指摘の `node` に、`Unsupported`・`Limit`・`Decode`・`Encode` は `EngineError::AtNode { node, source }` で包む（`code()` は中身の種類、`root()` で中身、`node()` でノード）。中止と内部の誤りは包まない。上限を超える大きさの指摘は、どの操作でも `IssueCode::TooLarge`（原因の引数を `param` に）。配列の引数（描画の動作など）の誤りは、推論でも実行でも、文言の頭に何番目か（「動作 i：」）を付ける。
- 中止：`par_blocks` がブロックの合間で `canceled()` を見るので、ほとんどの操作は自動で中止に対応する。操作は最初に `cx.check()` を呼ぶ（何もしない近道でも）。適合テストは、操作の中の 1〜4 回目の確認で中止できることを確かめる（E24）。
- 描くときの入力の値は、計画を作ったときの形と上限に照らしてから使う（ヘッダの形と実際のデコードの結果が食い違う場合に、上限を超えた値を描かない）。
- 進捗：ジョブ（kura-core）が `Monitor` を実装し、既存の `JobContext::progress` につなぐ。
- 上限の既定：1 辺 32,768px、画素数 2²⁸、ノード数・点の数・音声の長さも上限を持つ。設定で変えられる。
- **安全**（ダウンロードしたファイルも読むので）：
  - ヘッダから大きさを読み、`u64` の `checked_mul` で見積もり、上限と予算で判断してから `try_reserve_exact` で確保する。
  - zlib の展開は期待する大きさ（aseprite の cel なら幅 × 高さ × 画素の大きさちょうど）の上限付きの領域にだけ行う。超えたり余ったりしたら壊れたファイルとして扱う。第三者のデコーダの上限（png の `Limits` など）も予算から設定する。
  - 第三者のデコーダの境界では `catch_unwind` してエラーに変える（自分のコードでは panic を禁じたまま）。
  - SVG の外部の参照（file・http）は拒否する（NFR-10）。DTD の実体の展開（入れ子を含む）と `<use>` の展開は、usvg に渡す前に見積もって上限で断る。SVG のファイルは 16 MiB まで。
  - 段 4 の各デコーダの上限：PNG のフレームの数は fcTL を数える段階で、eXIf はテキスト・ICC と合わせて `max_meta_bytes` で、GIF は 1 フレームの作業領域（`max_pixels`）とフレームの数を LZW を展開しない走査で、WebP の ICC と EXIF の合計は `max_meta_bytes` で、aseprite はファイル全体を `max_value_bytes` で、セル・タイルセットの展開をそれぞれの大きさちょうどで、1 フレームを描く作業領域を `max_value_bytes` の予算で、グループの入れ子を 64 段で、PSD は層とマスクの情報・RLE の行の長さ・圧縮なしの画素をファイルの残りの長さと照らして抑える。
  - 大きな画像：16384² の RGBA（値 1 GiB）を PNG に書いて読み直すテスト（`tests/large.rs`、release で 3.8 秒）のピークのメモリは 3.3 GB（元の値と読んだ値の 2 GiB を含む）。PNG の書き出しは行を詰めた写しを 1 つ作る（値と同じ大きさ。行ごとに書くとバイト列が変わるので、エンコーダの版を上げる段 10 のメモリの予算で扱う）。
  - 段 6 の音声のデコーダの上限：symphonia・nanomp3 に渡すファイルは `max_value_bytes` まで読み込む。長さはデコードせずに数え（パケットの長さの和・MP3 のフレームの走査）、形を上限（`max_audio_frames`・`max_value_bytes`・`max_channels`・`max_sample_rate`）で確かめてから確保する。デコードの途中でも形の上限を超えたら止める。Vorbis は codebook の表の大きさを渡す前に見積もって断る。AIFF の COMM は 279 バイトまで、MARK・INST・テキストは WAV と同じ予算（マーカーの上限・`max_meta_bytes`）。symphonia の panic は `codec::contain` が受け止める。
  - 段 9 の外部のツールと OS の機能（詳しくは §9.8）：ffmpeg は入力をファイルのパスで渡し、分離器・デコーダ・プロトコルをホワイトリストで固定し、環境変数を消して新しいプロセスグループで動かし、見張りが中止・無出力・時間切れ・出力の上限でグループごと止める。出力（WAV）は上限で確かめてから確保する。ImageIO の展開は専用のスレッドで行い、時間切れで見捨てる（壊れた HEIC で戻らないことがある）。Swift の側は大きさをすべて確かめ、Rust が確保した領域にだけ書く。
  - パーサー（PNG・JPEG・GIF・WebP・aseprite・PSD・SVG・WAV・AIFF・FLAC・MP3・Ogg・m4a・AAC・アンカー・文書の読み込み）はファズにかける。
    - `fuzz/`（workspace の外。`cargo +nightly fuzz run <対象>`。対象は `kura_engine::testing::fuzz_decode`・`fuzz_encode`・`fuzz_peaks`・`fuzz_document` を呼ぶだけ）は、カバレッジを見ながら長く回す。nightly と cargo-fuzz が要る。
    - `tests/fuzz_smoke.rs` は同じ口を stable で回す変異のファズ。種（各形式の小さなファイル・フィクスチャ・文書）をバイトの書き換え・挿入・削除・つなぎ合わせで崩す。ふだんの `cargo test` で対象ごとに 150 回（`quick`）、`KURA_FUZZ_SECS=<秒> cargo test --release -p kura-engine --test fuzz_smoke long -- --ignored` で長く回す（`KURA_FUZZ_ONLY=svg,wav` で対象を選べる）。見つけた入力は `target/fuzz-smoke/` に残し、`replay` で流し直す。
    - 文書のファズは、上限を絞ったエンジンで読み込み、小さな入力（画像・パレットの画像・フレームの列・音声、格子・ピボット・スライス付きのシート、ループ点・マーカー付きの音声）で計画を作って描くところまで流す（段 5 から。新しい操作の実行の経路とメタデータの写し方にも届く）。追記だけの引数（`actions` の列）を持つノードは、途中で切って差分の適用（`Op::apply_appended`）が全体を当て直したものと同じかも照らす（E21。実行器が差分を使う段 10 までは、ファズでここを照らす）。
    - デコーダのファズは、上限を絞って（`testing::fuzz_limits`）その形式のすべてのデコーダ（旧版を含む）で形と中身を読み、自分のコードの panic（panic の場所が `crates/kura-engine/src/` のもの。`testing::catch_own_panic` が panic のフックで見分ける）・形と中身の食い違い・`Internal` を見つけたものとして扱う。壊れたファイルとしてのエラーと第三者のクレートの panic（`codec::contain` がエラーに変える）は正しい振る舞い。
    - 段 4 の長い実行（release で対象ごとに 40〜60 秒、合わせて約 2 億回）で、SVG の DTD の走査が多バイト文字の途中で文字列を切る panic を見つけて直した（`multibyte_text_around_the_dtd`）。直したあとの実行では何も見つかっていない。SVG と文書は、変異の多くが最初の検査（UTF-8・JSON）で断られるので回数ほど深くは試せていない。深い所はカバレッジを見る `fuzz/` に任せる。
    - 段 5 で、nightly の cargo-fuzz（libFuzzer。release ＋デバッグの検査 ＋ AddressSanitizer、`-fork` で並べて落ちても続ける）を 9 つの対象で回した。各 30 分を 2 回、直したあとに SVG・文書・aseprite を 10〜20 分ずつ、合わせて約 2.7 億回。見つけたのは SVG と自前の新しい検査だけで、どれも直した：参照の輪と深い入れ子によるスタックのあふれ（受け止められずにプロセスごと落ちる）、resvg の pattern のタイルの上限のない確保（209 GB）、巨大な半径の楕円の遅い描画、数の検査の無限ループ。最後の実行（SVG 約 1,360 万回・文書 約 200 万回）では何も見つかっていない。見つけた入力は `replay` で流し直せる形で残す（`fuzz/README.md`）。
    - カバレッジ（育った種で数えた行の割合。同じファイルのエンコーダと、テストの例を作る関数は除く）：PNG 92%・JPEG 84%・GIF 90%・WebP 84%・WAV 92%・aseprite 95%・PSD 94%・SVG 83%。文書は、操作 78〜99%・メタデータの写し方 80%・実行器 72%・描画 70〜93%。届いていないのは主に、中止・あふれ・確保の失敗の枝と、識別子を返す関数。カバレッジで、aseprite のパレットの番号のまま描く経路・メタデータの写し方・差分の適用に届いていないと分かったので、種・入力・照らし合わせを足した。
    - 段 6 で音声の 6 つの対象（`decode_aiff`・`decode_flac`・`decode_mp3`・`decode_ogg`・`decode_m4a`・`decode_aac`）を足した。種は自作の AIFF・FLAC の書き出しと、他のツールで作ったフィクスチャ（MP3・Ogg Vorbis・Opus・ALAC・AAC・HE-AAC）。stable の変異で対象ごとに 60 秒（合わせて約 980 万回）、cargo-fuzz で対象ごとに 15 分（合わせて約 2,360 万回）回し、何も見つかっていない。検証のあとの修正（MP3・AIFF・Ogg・m4a）のあとにも、WAV を含む 7 対象を stable の変異で 40 秒ずつ（合わせて約 1,170 万回）回し、何も見つかっていない。ファズで届かない形（巨大な codebook を名乗る Vorbis・グラニュールで詰めた Ogg・切れた MP3 など）は、検証で組んだ入力を単体のテストとフィクスチャに残した。カバレッジ（読み取りの関数の行。同じファイルのエンコーダ・テストを除く）：AIFF 90%・FLAC 90%・MP3 89%・Ogg・m4a・ADTS の読み取り（`lossy.rs`）93%・symphonia の共通の部品（`symph.rs`）81%。届いていないのは主に、中止・途中でチャンネルの数が変わる枝と、ファズでは組めない形（巨大な codebook を名乗る Vorbis。単体のテストで確かめる）。
    - 段 7 で自前のエンコーダの対象 `encode_ogg`（`testing::fuzz_encode`）を足した。先頭の 19 バイト（`FUZZ_ENCODE_HEADER`）で品質（−1〜10）・レート・チャンネル（1〜9）・長さ（0〜16,384 フレーム）・ループ点（なし・範囲の中・u64 の端に近い始まり／終わり）・タグ（0〜4 個。別名・大文字小文字違い・書けない名前、空・`=`・NUL・4,096 バイト前後・70,000 バイトの値）を決め、残りを f32 のビット列（NaN・∞・巨大な値を含む）として標本にする。書けない形（レート・チャンネルの範囲の外）は断ること、書けると答えた形は断らないこと、すべての ogg のデコーダ（旧版を含む）で読み直した長さ・レート・チャンネルが元と同じこと、コメントのループ点が範囲の中だけ正しい値で書かれ、名前が重ならず、読み直して書き直すと同じコメントになることを確かめる。検証で cargo-fuzz の `encode_ogg` を 31 分（約 6.9 万回）、`decode_ogg`（kura の出力 672 本を種に足して）を 7 分（約 220 万回）、stable の変異の `encode_ogg` を 300 秒（約 7.8 万回）回し、何も見つかっていない。メタデータを足したあとも、stable で 90 秒（約 2.8 万回）と cargo-fuzz で 5 分（約 1.1 万回）回して何も見つかっていない（エンコーダは 1 回が重いので回数は少ない）。カバレッジ（行）：psy 94%・envelope 92%・floor1 88%・res0 86%・block 82%・ogg 79%。届かないのはエラーの枝と、設定のテンプレートでは届かない枝（M7 の短い窓の範囲の外・5.1 の coupling など）。
    - 段 8 で、`fuzz_decode` が読めた値に登録されたすべての解析（見積もりの手間が 2²⁴ 以下のもの）を当て、結果が見積もり以下であることを確かめるようにした。波形の段のバイナリの読み手の対象 `decode_peaks`（`testing::fuzz_peaks`）を足した。release の stable の変異で各 20 秒（peaks 約 6,000 万回、wav・aiff・flac は解析込みで約 4〜23 万回）回し、何も見つかっていない。
    - 段 9 でアンカーの対象 `decode_anchor`（`tests/fuzz_smoke.rs` では `anchor`。種は EXIF 付きの画像・フレームの列・メタデータ付きの音声のアンカー）を足した。確認の回で、release の stable の変異で各 60 秒（anchor 約 1,224 万回・ogg 約 470 万回・m4a 約 29 万回・aac 約 14 万回）、cargo-fuzz（`-fork=4`）で各 10 分（decode_anchor 約 1,321 万回・decode_m4a 約 49 万回・decode_aac 約 20 万回）回し、何も見つかっていない（m4a・aac は、SBR の判定と多チャンネルの断り方を変えたあとの読み取りの経路）。
    - 通常の `cargo test` にも、ChaCha8 でランダムな文書と壊れたファイルを作るテスト（形式ごとの `broken_files_are_errors` など）を入れる。

## 14. 保存と版（M12 以降。意味だけを決める。表は data-model.md に各段で書く）

- 実ファイルとして持つのは、アイテムごとに「元」と「今」の 2 つ。版の記録（文書・値のダイジェスト・出力のハッシュ・形式・コーデックの識別子・日時）はすべて残す（ADR-135）。
- 安全網：
  1. 置き換えたファイルは 7 日残してから消す（バックアップの保持と同じ長さ）。
  2. 消す直前に、その版を文書から作り直して照合する。一致しなければ、そのファイルを残す。
  3. Platform・External でしか読めない元には、初めて編集するときにアンカーを作る（§9.4）。
- 上書きは「アイテム ID を保ったまま中身を差し替える」操作。タグ・フォルダ・ユニット・プレイリスト・再生の記録は変わらない。手順：staging に書く → ハッシュ → `files/` に配置 → 1 トランザクションで `items.hash` を比較して置き換え、版を足す → 古いファイルを GC の待ち行列へ。
- `items.hash` を書き換えるのは保存の処理だけ。中身に依る列を書く UPDATE は、すべてハッシュを条件にする（古いハッシュで作ったサムネイルを書き込まないため）。
- 取り込みで、あるアイテムの今・元・過去の版と同じ中身なら、そのアイテムの重複として扱う。
- 上書きの結果が別のアイテムの今と同じ中身になったら保存しない（`Conflict`）。
- 保存の受け入れテスト（S1〜）は M12 の計画で足す。

## 15. 拡張のシナリオ

| シナリオ | 足すもの |
|---|---|
| 新しい操作「パレットの色の置き換え」 | `ops/palette/replace.rs` と登録の 1 行、ゴールデンの行。`SpaceMap::Identity`・`Lift::None`（入力に Frames も受け付ける。全フレームが 1 つのパレットを共有するので、トリムしたフレームの列にもそのまま当てられる）なので、格子・スライス・アニメはエンジンがそのまま写す |
| 新しい値の型「動画」でトリムだけ | `Value::Video(VideoRef { source, segments })`（展開しない参照）、`ops/video/trim.rs`（区間を書き換えるだけ）、kura-core の ffmpeg のエンコーダ。網羅的な `match` が扱いの要る場所を教える。結果は環境依存の段 |
| 引数の意味を変える（resize の "smooth" を bilinear から Lanczos へ） | 旧版で表せるなら `migrate` を書いて旧版を消す。表せない修正なら旧版を `legacy` に残す |
| 一括で「各アイテムの幅の 50%」 | 大きさの型（`Length::Scale`）で表す。キャンバス・ピボットの相対の指定（「上下左右に 2px」「下中央」）は、M14 の計画のときに、操作の版を上げるか別の操作にするかを決める。タグ由来の値などは M14 で `Bindable<T>`（結び付けの段で具体的な値にし、版に記録する） |
| 点の多いペン | `PaintParams.actions` を Arc で区切って持ち、差分の適用で 1 本ごとに O(そのストローク)。画面に返すときは件数とダイジェストだけ |

## 16. 性能の目標（M10 の段 10 で測って確定する。requirements.md の NFR-11〜17）

| 項目 | 目標 |
|---|---|
| 引数を変えてからプレビューが更新されるまで（24MP は縮小の段、2048² までのドット絵は等倍） | p95 50ms（エンジンの中 25ms） |
| 筆 1 本（500 点まで、4096² まで）の確定 | 8ms |
| 塗りつぶし（全面、4096²） | 30ms |
| 保存：24MP の JPEG → 線形光で 50% → JPEG | 0.5 秒 |
| 一括：ドット絵の PNG 1,000 件（トリム・4 倍） | 5 秒 |
| 5 分の曲の解析（波形・ラウドネス・トゥルーピーク） | 2 秒 |
| メモリ | 予算（既定は物理メモリの 1/4、上限 4GiB）を超えない。超える入力は落ちずにエラー |

段 5 の実測（`cargo test --release -p kura-engine --test bench_image -- --ignored --nocapture`、Apple M 系の Mac。操作そのもので、文書の計画と出力のダイジェストを含まない）：

| 項目 | 実測 |
|---|---|
| 筆 1 本の確定（差分の適用。1,024² に 999 本描いたあとの 50 点。1 px・円 8 px） | 0.1 ms・0.3 ms |
| 筆 1,000 本の当て直し（1,024²、各 50 点。1 px・円 8 px） | 34 ms・79 ms |
| 塗りつぶし（全面、4096²。連結・全体） | 8.5 ms・7.1 ms |
| 塗りつぶし（2048² の市松模様・8 連結。区間が 1 画素ずつの最悪に近い形） | 26 ms |
| Lanczos3 4096² → 1024²／bilinear 4096² → 4500² | 47 ms／52 ms |
| 最近傍 512² → 2048²（Indexed8）／トリム 4096²／90° 回転 4096² | 0.9 ms／1.5 ms／3.8 ms |
| 1024² の乱数の画素を 256 色に（なし・組織的・誤差拡散） | 353 ms・316 ms・496 ms（全画素の色が違う最悪の形。色の表の並べ替えと、色ごとの最近傍の探索が大半） |
| 文書の大きさ（筆 1,000 本・各 50 点） | 約 0.3 MB |

段 7 の実測（`cargo test --release -p kura-engine --lib vorbis_bench -- --ignored --nocapture`、16 コア）：Ogg Vorbis の書き出しは、3 分の 44.1 kHz ステレオで q5 が 0.27〜0.30 秒・q0 が 0.26〜0.30 秒・q10 が 0.30〜0.33 秒（1 スレッドで 1.45〜1.56・1.46〜1.57・1.61〜1.71 秒。2 回の測定の幅）。直列で残るのは、ブロックの計画（envelope。q5 で約 0.08 秒）がほとんど。

段 8 の実測（release・16 コア、負荷のある中の目安。§11.3・§11.4）：5 分の 44.1 kHz ステレオの解析（波形の段・ラウドネス・無音・ループ）は 66 ms（1 スレッドで 0.47 秒）、WAV の読み取りを含めて 0.19 秒、FLAC で 0.42 秒（「5 分の曲の解析 2 秒」を満たす）。60 分の 48 kHz ステレオのラウドネスは 0.69 秒。画像の解析は 16384² で 15〜218 ms（最も重いのは sprite_grid）。正規化の描画全体（計測を先に解く＋利得、5 分 48 kHz ステレオ）は 0.21 秒、トリムの描画（8192²）は 47 ms。

## 17. 受け入れテスト（`kura-engine` のテストとして実装する）

| ID | 内容 |
|---|---|
| E1 | 同じ文書と入力で 2 回描くとダイジェストが一致する。スレッド数を 1・3・16 にしても一致する |
| E2 | 固定のコーデックだけの文書は、何度作り直しても値のダイジェスト（R1）が一致する。PNG と WAV は出力のバイト列（R2）も一致する |
| E3 | 版 2 は「元＋版 1 と版 2 の操作」から作られ、版 1 の出力を経由しない。非可逆形式で 2 回保存しても、値は元から 1 回エンコードしたものと同じ |
| E4 | 同じ文書を同じ元に当てると、エディタの経路（セッション）と一括の経路の出力がバイト単位で同じ |
| E5 | 一括の対象の並びや件数を変えても、各アイテムの出力は変わらない |
| E6 | 結果を変えた操作の旧版のノードは旧版の実装で描かれ、旧版のゴールデンと一致する |
| E7 | 引数の形だけを変えた移行のあとも、旧版のゴールデンと一致する。文書のスキーマの移行も同じ |
| E8 | 未知の操作・新しすぎる操作の版・新しすぎるスキーマの文書は、読み込みでエラーになる。落ちない |
| E9 | すべての操作の例で、形の推論が実際の出力と合う（`Exact` は一致、`AtMost` はそれ以下）。`SpaceMap`・`TimeMap` も apply の結果と一致する |
| E10 | 切り抜いてから同じ量だけキャンバスを広げると、元の範囲の外を透明にしたものになる |
| E11 | 透明な余白のトリムは 2 回しても同じ。外接矩形を含む範囲で切り抜いてからトリムしても同じ |
| E12 | 最近傍で k 倍してから 1/k にすると元に戻る。Indexed8 はパレットを保つ |
| E13 | 90° 回転を 4 回、反転を 2 回で元に戻る |
| E14 | スプライトを分割してから同じ格子で結合すると元に戻る（間隔や空きを塗る `fill` を、元の形式で塗れる色にしたとき。透明の `fill` はアルファのない形式をアルファ付きにし、透明な色のないパレットには透明な色を足す） |
| E15 | 格子・ピボット・スライス・ループ点・マーカーが §6 の表のとおりに写る。範囲の外に出たものは警告付きで落ちる |
| E16 | 音声のトリムを 2 回するのは、合成した 1 回のトリムと同じ |
| E17 | ゲイン 0dB と長さ 0 のフェードは何も変えない。int16・int24 → f32 → 元のビット数の往復はビット一致 |
| E18 | 区間ごとに描いても（チャンク 1・7・4,096・全体）値が同じ |
| E19 | ラウドネスの計測が EBU Tech 3341 の試験信号で ±0.1 LU。正規化したあとの積分ラウドネスが目標の ±0.1 LU。LRA が EBU Tech 3342 の試験 1〜4 で 10・5・20・15 LU（段 8） |
| E20 | WAV の smpl・cue、AIFF の MARK・INST、Ogg の LOOPSTART・LOOPLENGTH を読んで書いても保たれる（FLAC の LOOPSTART・LOOPLENGTH も往復を確かめる。Ogg の書き出しは段 7 で足した。他のツールで書いた Ogg のループ点も読む）。範囲の外・長さ 0・逆向きのループ点はどの形式も書かない。名前の重なるタグは 1 つだけ書き、書いて・読んで・書き直すと同じコメント（FLAC はバイト列も同じ、Ogg はシリアル番号と CRC を除いて同じ） |
| E21 | 同じ点の列からは同じ画素ができる。差分で当てても全体を当て直しても同じ。ピクセルパーフェクトの規則どおり。TS と共有するベクタと一致する |
| E22 | 塗りつぶしは 4 連結・8 連結・許容差の定義どおりで、たどる順に左右されない |
| E23 | スポイトが返す色とインデックスが、描画結果の画素と一致する |
| E24 | 中止すると `Canceled` になり、途中までの値もファイルも出ない |
| E25 | 上限を超える出力は推論の段階で `Limit` になり、描かない。壊れたファイル・展開爆弾で落ちない |
| E26 | 解析と操作が一致する。申告した解析を実行器が渡した値と別に求めた値が同じで、操作はその値を使う（登録されたすべての操作の例）。意味の対：不透明な範囲とトリムで切り取る範囲、ラウドネスの値と正規化で使う値、色数とパレットへの変換、倍率と最近傍のリサイズ、格子と分割、無音とトリム、ループ点とループの設定、透過と形式の能力。登録されたすべての解析が対の表か「対のない解析」の表に載る（§11.5） |
| E27 | aseprite の合成が、照合相手の実装と一致する。段 4 の asefile との照合（300 個のランダムな絵。`tests/aseprite_ref.rs`）で満たす（段 8 でユーザーが決めた。Aseprite 本体で書き出したフィクスチャとは照らさない。ADR-152） |
| E28 | 自作の Ogg Vorbis の出力が、独立したデコーダ 2 つ以上（symphonia・lewton）で読め、長さ・位置・ループのコメントが保たれる。ページが §9.7 の規則に従い（「長・次は短」が途中と最後の前に来る実際の出力でも）、品質の段・スレッド数・チャンネルとレートの範囲が約束どおり。NaN・±∞ を含む入力が 0・±1 に置き換えた入力と同じバイト列（シリアル番号と CRC を除く）、3〜8 チャンネルの並びを lewton と仕様の表で確かめる（`tests/ogg_vorbis.rs` と `codec::vorbis::encoder` のテスト） |
| E29 | Platform・External のコーデックを通った版は、デコードのダイジェストとコーデックの識別子（と環境の版）が記録され、変わったら検出される（失敗ではなく食い違いとして返す）。アンカーがあればアンカーから作り直し、出力が記録と同じになる。壊れたアンカーは見分ける（`tests/e29.rs`。偽の Platform のデコーダで Linux でも回る） |
| E30 | §16 の性能（`#[ignore]` 付きのベンチテストと `kura-devtools bench edit`） |
