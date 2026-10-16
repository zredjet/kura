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
  analyze/   解析（透過・ラウドネス・波形など）
  codec/     読み書きの trait・固定のコーデック・形式の表・能力
  render/    実行・キャッシュ・プレビューの段・範囲の描画
  session    エディタの履歴（純粋な構造）
  determinism, math, digest, error, limits
     ↑
kura-core   保存・版・GC・解析・一括加工のジョブ、DB（M11 以降）
kura-mac    ImageIO・AudioToolbox・AVFoundation によるコーデック（Platform の段）
src-tauri   コマンドと kura:// の /edit・/peaks（feature "specta" で TS の型を出す）
```

- エンジンは 1 つのクレートにまとめる。操作の旧版を凍結するとき（§8.4）、その操作が使う計算の部品も一緒に凍結できるようにするため（ADR-134）。
- specta は kura-engine の feature の中だけで使う（ADR-28 を kura-engine について置き換え。ADR-134）。

## 4. 文書

### 4.1 形

```rust
pub struct Graph {
    pub schema: u32,               // 文書の型（項目）の版。今は 2（§8.4）
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
    pub quantize: Option<Quantize>,// 整数への丸めとディザ。版を持つ
    pub expand: Expand,            // One か PerElement { name }（例 "{stem}_{index:03}"）
}
```

- 線形の操作の列は、どのノードも 1 つ前を入力にする特別な場合。エディタは出力からさかのぼった 1 本の鎖を操作のスタックとして見せ、入力が複数の操作のときだけ別の入力を選ぶ欄を出す。
- 出力が複数になる操作（スプライトの分割、マーカーでの分割）は、`Frames` や `Clips` の値を 1 つ返す。何個のファイルにするかは `OutputSpec.expand` が決める。口の数が入力によらず決まるので、同じレシピを 100 件に当てても形が崩れない。
- `DecodeOptions`（SVG のラスタ化の倍率、aseprite を合成で読むかレイヤーで読むか）は出力に効くので、文書に入れる。

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

- 文書・ノードのハッシュは、自前の正規化 JSON（キーを並べる、数値は最短の表記、空白なし。RFC 8785 に準じる）の BLAKE3 で取る。`serde_json::Value` の並びには頼らない（依存の feature で変わりうる）。
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
- 2 パスが要る操作（正規化）は、必要な解析（`Loudness(上流)`）を申告し、実行器が先に解く（§12）。
- 元のビット深度・サンプルレートは `AudioMeta.source` に残し、書き出しの既定値に使う。

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
- ループ点の入れ物：WAV は `smpl`（と `cue `）、AIFF は `MARK` と `INST`、Ogg・FLAC は `LOOPSTART`・`LOOPLENGTH` のコメント。

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
- `layers.flatten` は、レイヤーの値（`Layered`）を出すデコーダを作る段で足す（今の aseprite・PSD のデコーダは合成済みの画像を出す）。`audio.set_loop` と音声の操作は段 6。

## 8. 決定性と版

### 8.1 保証の段

| 段 | 保証すること | 条件 |
|---|---|---|
| R1（値） | 同じ文書・同じ元なら、エンコード前の値のダイジェストが永久に一致する | aarch64-apple-darwin で、固定のコーデックだけを通る |
| R2（ファイル） | 出力ファイルのバイト列も一致する | R1 に加えて固定のエンコーダ |
| 整数だけの操作 | すべてのターゲット（Linux を含む）で一致する | CI の ubuntu で確かめる（ADR-142） |
| 浮動小数の操作 | ゴールデンをターゲットごとに持つ | — |
| 環境依存 | 同じ macOS・同じ ffmpeg なら一致する。コーデックの識別子と版を記録し、画面に「環境に依存」と出す | ImageIO・AudioToolbox・AVFoundation・ffmpeg を通るとき |

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
6. 結果に効く依存は Cargo.toml で `=` で版を固定する（libm・blake3・rand_chacha、コーデックの png・zune-jpeg・jpeg-encoder・gif・image-webp・resvg・miniz_oxide）。上げるのは、ゴールデンが通るときだけ。SIMD の経路がターゲットで値を変えうる依存は、SIMD の feature を外す（zune-jpeg の neon・x86、jpeg-encoder の simd）。ほかの依存（dev 依存を含む）がついでに有効にしていないかを `scripts/check-engine-features.sh` が確かめ、CI でも回す。serde_json は固定しない（正規の JSON は自前で書き、読み込みは `float_roundtrip` で正しく丸めるので、結果は版によらない）。

### 8.3 数値計算は自前で書く

ラウドネス（ITU-R BS.1770-4 とトゥルーピーク）・サンプルレート変換（窓付き sinc の多相フィルタ）・リサイズの重み・ディザ・減色は、`math` を通して自分で書く（ADR-138）。第三者のクレート（ebur128・rubato・fast_image_resize など）は OS の libm を呼ぶことがあるので、dev 依存の照合相手とベンチの比較にだけ使う。

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
| 固定のデコーダが読む値 | 変えない。どうしても変えるなら、デコーダの約束の版を上げ、旧版を `CodecSet::add_legacy_decoder` で残す。作り直しは記録した識別子のデコーダで読む（`decode_with`） | 旧版のデコーダで同じ |
| 操作をなくす | なくさない。最後の版を `retired` に移す（カタログに出さず新しく作れないが、作り直しには使う） | 作り直せる |

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
  - エンコーダ（R2）`png-rs-enc@1.jsonl`：書いたバイト列のダイジェスト（`file`）と、それを読み直したデコーダの ID（`decoder`）・値のダイジェスト（`value`）。読み直しは、記録したデコーダ（旧版を含む）で照らす。バイト列が変わったら、エンコーダの約束の版を上げる。今ないエンコーダのファイルは照らさない（版を上げた旧版）。依存のクレート（png・fdeflate・flate2・miniz_oxide など）を上げてバイト列が変わったときも、ここで気づく。
  - デコーダ（R1 のうちデコーダだけの経路）`png-rs-dec@1.jsonl`：下の層で組んだファイル（`testfiles::FileGen`：1/2/4 ビット・tRNS・インターレース・64 ビットの浮動小数・拡張形式・他のツールのメタデータ・大きさの分からない data など）を読んだ値のダイジェスト。凍結した行は、その ID のデコーダ（今の版か旧版）で読んで照らし、その ID のデコーダがなければ失敗にする。
  - どちらも追記だけの検査の対象。再現を約束しないデコーダ（`reproducible()` が偽。SVG）は、デコーダのゴールデンに入れない。
- **共有のテストベクタ**（`crates/kura-engine/tests/vectors/`）：Rust と TS で同じ規則を実装するもの（描画の筆・ピクセルパーフェクト・点の列の詰め方・重ねる式）の入出力を、エンジンが JSON で凍結する（`tests/paint_vectors.rs`。足りなければ `KURA_BLESS=1`）。フロントは `src/lib/paint.test.ts` が同じファイルを読んで照らす。フィクスチャと同じく、消す・変えるのを認めない（追記だけの検査が見る）。
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
- 段 4 で入れたもの：§9.5。
- `Platform` は kura-mac が実装して注入する（`MediaBackend` と同じ形）。Swift とのあいだは C の型だけでやり取りし、画素や PCM は Rust が確保したバッファのポインタと長さで受け渡す（ADR-37 の改訂。M10 の段 9）。
- `External` は ffmpeg（同梱しない・任意）。PCM は標準入出力で流す。引数には `-fflags +bitexact -protocol_whitelist file,pipe` を付ける。

### 9.2 形式の表

`FormatDesc { id, exts, mime, item_kind, decodes_to, caps }` で、拡張子 → items.kind → 値の型 → 保存できるメタデータを 1 か所で決める。M11 で取り込みの判定（`import::kind`）もこの表を見る。

| 形式 | items.kind | 読む | 書く |
|---|---|---|---|
| PNG（APNG・16 ビット・PNG-8） | image | png（`png-rs-dec/2`。旧版 `/1`） | png（`png-rs-enc/1`。APNG・eXIf を含む） |
| JPEG | image | zune-jpeg（`jpeg-zune-dec/1`。CMYK・YCCK は次の段へ） | jpeg-encoder（`jpeg-rs-enc/1`） |
| GIF | image | gif（`gif-rs-dec/1`） | gif（`gif-rs-enc/1`。減色は自作） |
| WebP | image | image-webp（`webp-rs-dec/1`。非可逆・可逆・アニメ） | image-webp（`webp-rs-enc/1`。可逆の 1 枚だけ） |
| aseprite | image | 自作（`aseprite-kura-dec/1`。合成した各フレーム） | — |
| PSD・PSB | image | 自作（`psd-kura-dec/1`。合成済みの画像。ZIP は次の段へ） | — |
| SVG | image | resvg・usvg（`svg-resvg-dec/1`。再現は約束しない → アンカー。外部の参照は読まない。`<image>` と、フォントを渡していないときの文字は次の段へ） | — |
| HEIC・HEIF・AVIF・RAW | image | ImageIO（Platform。アンカー） | — |
| WAV・AIFF | audio | 自作（RIFF・smpl・cue・LIST・MARK・INST） | 自作 |
| FLAC | audio | symphonia | flacenc |
| MP3・AAC-LC・Vorbis・ALAC | audio | symphonia（AAC の priming は自作） | — |
| Ogg Vorbis | audio | symphonia | **自作のエンコーダと Ogg の多重化**（ADR-137） |
| AAC（m4a） | audio | symphonia | AudioToolbox（Platform） |
| MP3 | audio | symphonia | 自作のエンコーダができるまでは ffmpeg（External）。なければ形式を変えて保存する |
| HE-AAC・Opus | audio | AVFoundation（Platform。アンカー） | — |
| ttf・otf・woff・woff2 | font | —（解析だけ。M11） | — |
| glb ほか 1 ファイルの 3D | model | —（解析だけ。M11） | — |
| 動画 | video | —（編集しない） | — |

- C のライブラリは同梱しない。C か ffmpeg がないと書けない形式は、エンコーダを Rust で自作する（Ogg Vorbis を最優先。MP3・不可逆 WebP・Opus は必要になった段で。ADR-137）。
- 音声のタグは lofty で書く。EXIF・ICC は運ぶ。PNG の tIME・MP4 の作成日時など、時刻は書かない（R2 のため）。

- EXIF の向き（Orientation）：JPEG・PNG（eXIf）・WebP は、読むときに向きを画素に当て、運ぶ EXIF の向きを 1 にする（同じ向きを 2 回当てない）。形（`shape`）も向きを当てた大きさを返す。PSD は画素が表示のとおりなので向きは当てず、EXIF の向きだけ 1 にする。

### 9.3 能力

`capabilities(env) -> Vec<FormatCaps>` が、今の環境で読める・書ける形式と、可逆か・アルファ・ICC・アニメ・ループ点を保存できるか・再現できるか（R2）・選べる設定を返す。macOS の能力は kura-mac が問い合わせ（`kura_codec_caps`）、ffmpeg は `-encoders` を調べる。エディタの「保存の形式」と一括加工の事前チェックはここを見る。

### 9.4 アンカー

- 再現を約束しないデコーダ（`reproducible()` が偽：Platform・External と、固定の段でも OS の libm を使う SVG）で読んだ元（HEIC・RAW・AVIF・HE-AAC・Opus・SVG など）は、初めて編集するときに、読み取った結果を可逆形式で残す（画像は ICC 付きの PNG、音声は FLAC か f32 の WAV）。OS の更新で読み取り結果が変わっても、途中の版を作り直せるようにするため（ADR-135）。
- アンカーがあれば、作り直し・エディタはアンカーを元として読む（RAW・HEIC より速い）。
- アンカーは実ファイルの台帳（M12）に載せ、整合性チェックの対象にする。

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

```rust
pub trait Analysis: Send + Sync + 'static {
    const ID: &'static str;            // "image.alpha" など
    const VERSION: u16;                // 上がったら保存した結果を作り直す
    type Output: Serialize + DeserializeOwned + Debug + PartialEq;
    fn accepts(kind: ValueKind) -> bool;
    fn run(v: &Value, cx: &mut AnalyzeCx) -> Result<Self::Output, EngineError>;
}
```

| 解析 | 中身 | 使い手 |
|---|---|---|
| `image.alpha` | none / binary（0 か 255 だけ）/ partial、透明な画素の数 | 市松模様の表示、JPEG に書き出すときの警告 |
| `image.opaque_bounds` | 不透明な範囲の外接矩形 | トリム |
| `image.colors` | 257 色まで数える（並べた集合）→ パレットの候補 | PNG-8・GIF、ドット絵の判定 |
| `image.pixel_scale` | 同じ色の連続の長さの最大公約数 → 拡大の整数倍 | 表示を最近傍にするか、「1/4 に戻す」 |
| `image.sprite_grid` | 透明な溝と規則的な格子の推定 | 分割の候補 |
| `audio.peaks` | 1 ビンあたり 256・1,024・4,096・16,384・65,536 フレームの min/max（i16）の段 | 波形の表示 |
| `audio.loudness` | 積分ラウドネス・LRA・サンプルピーク・トゥルーピーク | 正規化、表示 |
| `audio.silence` | 前後の無音の範囲、クリップの数 | 自動トリム、警告 |
| `audio.loop` | ファイルに書かれたループ点 | ループの表示と試聴 |

- 結果のキーは「中身のハッシュ × 解析の ID × 版」。ライブラリのアイテムの結果は DB に置く（M11）。
- 操作も同じ関数を使う。画面に出る「余白」とトリムで切り取る量、表示するラウドネスと正規化で使う値が必ず一致する（E26）。
- フォント・3D の情報（書体名・字数、頂点数・大きさ）は値を持たない解析で、kura-mac か軽いパーサーで読む（M11）。

## 12. 実行・プレビュー・セッション

```rust
impl Engine {
    pub fn load(&self, raw: &[u8]) -> Result<Graph, EngineError>;               // 読み込みとスキーマの移行
    pub fn compile(&self, g: &Graph, inputs: &InputShapes) -> Result<Plan, Issues>; // 版の解決・引数の移行と検証・形の推論・資源の見積もり
    pub fn render(&self, plan: &Plan, inputs: &InputValues, cache: &dyn NodeCache, mon: &dyn Monitor) -> Result<Rendered, EngineError>;
    pub fn encode(&self, v: &Value, out: &OutputSpec, path: &Path, mon: &dyn Monitor) -> Result<EncodedStamp, EngineError>;
    pub fn analyze<A: Analysis>(&self, v: &Value, mon: &dyn Monitor) -> Result<A::Output, EngineError>;
}
```

- **キャッシュ**：ノードの指紋（`blake3(親の指紋 ‖ 操作 ‖ 版 ‖ 正規化した引数 ‖ 解像度の段)`）で途中の値を覚える。正しさには関わらない。編集中のノードの入力は固定して、引数を動かしても前段を計算し直さない。指紋は版をまたいで保つ必要はない（種は `Node.seed` に別に持つ）。
- **プレビューの段**：写真は縮小した段で描く（操作が引数を縮める）。ドット絵（`image.pixel_scale` が 1 以上、または利用者の指定）は常に等倍で描き、拡大は CSS の `image-rendering: pixelated` に任せる。大きなシートは範囲（ROI）を等倍で描く。
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
  - パーサー（PNG・JPEG・GIF・WebP・aseprite・PSD・SVG・WAV・文書の読み込み。段 6 から AIFF・Ogg など）はファズにかける。
    - `fuzz/`（workspace の外。`cargo +nightly fuzz run <対象>`。対象は `kura_engine::testing::fuzz_decode`・`fuzz_document` を呼ぶだけ）は、カバレッジを見ながら長く回す。nightly と cargo-fuzz が要る。
    - `tests/fuzz_smoke.rs` は同じ口を stable で回す変異のファズ。種（各形式の小さなファイル・フィクスチャ・文書）をバイトの書き換え・挿入・削除・つなぎ合わせで崩す。ふだんの `cargo test` で対象ごとに 150 回（`quick`）、`KURA_FUZZ_SECS=<秒> cargo test --release -p kura-engine --test fuzz_smoke long -- --ignored` で長く回す（`KURA_FUZZ_ONLY=svg,wav` で対象を選べる）。見つけた入力は `target/fuzz-smoke/` に残し、`replay` で流し直す。
    - 文書のファズは、上限を絞ったエンジンで読み込み、小さな入力（画像・パレットの画像・フレームの列・音声、格子・ピボット・スライス付きのシート、ループ点・マーカー付きの音声）で計画を作って描くところまで流す（段 5 から。新しい操作の実行の経路とメタデータの写し方にも届く）。追記だけの引数（`actions` の列）を持つノードは、途中で切って差分の適用（`Op::apply_appended`）が全体を当て直したものと同じかも照らす（E21。実行器が差分を使う段 10 までは、ファズでここを照らす）。
    - デコーダのファズは、上限を絞って（`testing::fuzz_limits`）その形式のすべてのデコーダ（旧版を含む）で形と中身を読み、自分のコードの panic（panic の場所が `crates/kura-engine/src/` のもの。`testing::catch_own_panic` が panic のフックで見分ける）・形と中身の食い違い・`Internal` を見つけたものとして扱う。壊れたファイルとしてのエラーと第三者のクレートの panic（`codec::contain` がエラーに変える）は正しい振る舞い。
    - 段 4 の長い実行（release で対象ごとに 40〜60 秒、合わせて約 2 億回）で、SVG の DTD の走査が多バイト文字の途中で文字列を切る panic を見つけて直した（`multibyte_text_around_the_dtd`）。直したあとの実行では何も見つかっていない。SVG と文書は、変異の多くが最初の検査（UTF-8・JSON）で断られるので回数ほど深くは試せていない。深い所はカバレッジを見る `fuzz/` に任せる。
    - 段 5 で、nightly の cargo-fuzz（libFuzzer。release ＋デバッグの検査 ＋ AddressSanitizer、`-fork` で並べて落ちても続ける）を 9 つの対象で回した。各 30 分を 2 回、直したあとに SVG・文書・aseprite を 10〜20 分ずつ、合わせて約 2.7 億回。見つけたのは SVG と自前の新しい検査だけで、どれも直した：参照の輪と深い入れ子によるスタックのあふれ（受け止められずにプロセスごと落ちる）、resvg の pattern のタイルの上限のない確保（209 GB）、巨大な半径の楕円の遅い描画、数の検査の無限ループ。最後の実行（SVG 約 1,360 万回・文書 約 200 万回）では何も見つかっていない。見つけた入力は `replay` で流し直せる形で残す（`fuzz/README.md`）。
    - カバレッジ（育った種で数えた行の割合。同じファイルのエンコーダと、テストの例を作る関数は除く）：PNG 92%・JPEG 84%・GIF 90%・WebP 84%・WAV 92%・aseprite 95%・PSD 94%・SVG 83%。文書は、操作 78〜99%・メタデータの写し方 80%・実行器 72%・描画 70〜93%。届いていないのは主に、中止・あふれ・確保の失敗の枝と、識別子を返す関数。カバレッジで、aseprite のパレットの番号のまま描く経路・メタデータの写し方・差分の適用に届いていないと分かったので、種・入力・照らし合わせを足した。
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
| E19 | ラウドネスの計測が EBU Tech 3341 の試験信号で ±0.1 LU。正規化したあとの積分ラウドネスが目標の ±0.1 LU |
| E20 | WAV の smpl・cue、AIFF の MARK・INST、Ogg の LOOPSTART・LOOPLENGTH を読んで書いても保たれる |
| E21 | 同じ点の列からは同じ画素ができる。差分で当てても全体を当て直しても同じ。ピクセルパーフェクトの規則どおり。TS と共有するベクタと一致する |
| E22 | 塗りつぶしは 4 連結・8 連結・許容差の定義どおりで、たどる順に左右されない |
| E23 | スポイトが返す色とインデックスが、描画結果の画素と一致する |
| E24 | 中止すると `Canceled` になり、途中までの値もファイルも出ない |
| E25 | 上限を超える出力は推論の段階で `Limit` になり、描かない。壊れたファイル・展開爆弾で落ちない |
| E26 | 解析と操作が一致する（不透明な範囲とトリムで切り取る範囲、ラウドネスの値と正規化で使う値） |
| E27 | aseprite の合成が、Aseprite で書き出したフィクスチャと一致する |
| E28 | 自作の Ogg Vorbis の出力が、独立したデコーダ 2 つ以上で読め、長さ・ループのコメントが保たれる |
| E29 | Platform・External のコーデックを通った版は、デコードのダイジェストとコーデックの識別子が記録され、変わったら検出される。アンカーがあればアンカーから作り直す |
| E30 | §16 の性能（`#[ignore]` 付きのベンチテストと `kura-devtools bench edit`） |
