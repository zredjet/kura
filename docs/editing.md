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
  value/     画素（CoW のタイル）・音声（チャンネルごとの Arc<Vec<f32>>）・メタデータ
  shape      形と推論
  op, ops/   操作の trait・登録・各操作（image/ sprite/ paint/ palette/ audio/ meta/）
  paint/     描画の規則（筆・塗りつぶし・点の列）と動作の列（ActionList。§10）
  analysis/  解析（透過・ラウドネス・波形など。§11）
  codec/     読み書きの trait・固定のコーデック・形式の表・能力・Platform の受け口・アンカー・作り直しの記録
  render/    実行（§12）
    mod          compile・render（キャッシュなしの正）・最後の使い手・進捗
    cache        指紋・NodeCache・render_cached
    incremental  compile_snapshot・差分の適用
    estimate     メモリの見積もりと予算の確保（estimate・admit・admit_cached・CacheLease）
    region       範囲（Area・Reach・Tiles・PartialValue）
    delta        変わった範囲（delta・refine・Changed）
    area         範囲の描画（render_area・spoit）
    preview      プレビューの段（choose_level・downscale・plan_level）
    view         エディタの表示の経路（render_view・ViewRequest・View・ViewRender。§12.8）
  session/   エディタの履歴（純粋な構造。EditSession・Snapshot・AppendLog）
  budget     メモリの予算（MemoryBudget・Lease・Class）
  monitor    中止・進捗・予約（Latest・Ticket）
  testing/   テスト・ファズ・ベンチの補助（bench・region・snapshot・preview・platform）
  determinism, math, digest, error, limits
     ↑
kura-core      保存・版・GC・解析・一括加工のジョブ、DB（M11 以降）、ffmpeg の段（codec_ext::ffmpeg。段 9）、
               エンジンの実行の場（edit::EngineRuntime：2 つのプールと予算。段 10）
kura-mac       ImageIO・AudioToolbox によるコーデック（Platform の段。段 9）、物理メモリ（physical_memory。段 10）
kura-devtools  bench edit（§16）・見積もりの煙のテスト（mem_smoke。段 10）
src-tauri      コマンドと kura:// の /edit・/peaks（feature "specta" で TS の型を出す）
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
- 新しいノードの種（段 10。ADR-154）：エディタでノードを足すとき（`EditSession` の `Edit::AddNode`）、種を指定しなければ、払い出したノード ID の値（`next_id`）にする。種は文書に書くので、そのあとは ID によらない。§8.2(3) の「種がノード ID に依存しない」は、描くときに ID を読まないという意味で、足すときに種の初期値を ID から決めることとは別。レシピの継ぎ足し（§4.3）で ID をずらしても、種はそのまま。

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
- **文書の部品**（M12 の段 2。`doc_parts`。形は凍結：E34）：文書を 3 種類の部品に分け、同じ中身の部品は 1 つだけ持つ（DB の
  `edit_objects`・`edit_edges`。docs/data-model.md §3）。本文は正規の JSON、ダイジェストは「ドメイン付きの BLAKE3 に長さ付きの本文」
  （`Hasher::new(<ドメイン>).bytes_with_len(本文)`）で、本文だけから求め直して照らせる（`Node::digest` の `kura-node-v1` は使わない）。

  | 部品 | 本文 | ドメイン | 子 |
  |---|---|---|---|
  | chunk | 配列 1 つ（1〜64 要素。元の配列の `[64k, 64k+64)`） | `kura-doc-chunk-v1` | なし |
  | node | ノードのオブジェクト。切った引数の値は `[]`、最上位に `"$chunks": {<引数名>: {"count": N, "parts": ["<hex>", …]}}`（切った引数があるときだけ） | `kura-doc-node-v1` | 切れ目（引数名の UTF-8 のバイト順 → 切れ目の順） |
  | doc | 文書のオブジェクト。`"nodes"` をノードの部品のダイジェストの 16 進の配列にしたもの | `kura-doc-root-v1` | ノード（文書の順） |

  - 切る規則：ノードの `params` の最上位の値のうち、要素が `PART_CHUNK`（64。凍結）を超える配列を、位置 0 から 64 個ずつ切る。登録表に
    依らない（`Engine` なしで分けられる）。64 個以下の配列は切らない。切れ目の境界は追記だけの配列（`session::log::CHUNK`。§12.12）と
    同じなので、筆を足した版どうしで前の切れ目を共有する（200 本 → 210 本で新しく書く部品は最後の切れ目・ノード・根だけ。5 万本・約
    15 MB の文書に 10 本足して保存すると、新しく書くのは約 0.1 MB）。部品は文書の中の同じ値より浅く、`MAX_DEPTH` を超えない。
  - つなぐ（`doc_parts::join`）ときは、根から子をたどり、各部品について「ある・種類が合う・本文からのダイジェストが合う・本文が正規の
    JSON」を確かめ、`$chunks` の `count` と切れ目の数・長さ（最後だけ 1〜64、ほかは 64）・殻が `[]`・切っていない長い配列がないことを
    照らす（同じ文書は必ず同じ根になり、根が同じなら文書も同じ。DB では文書を根の行の ID で比べられる）。壊れていれば `Invalid`
    （`BadGraph`）。読む本文の合計が `MAX_DOC_BYTES` の 2 倍を超える組（同じ切れ目を何度も参照して膨らませるもの）と、つないだ文書が
    上限を超えるものは `Limit`。つないだバイト列は文書の読み込み（`Engine::load`）に通して使う。外の本文を読むので、壊れた入力の
    テスト（ChaCha8）とファズの対象 `doc_parts` を持つ。

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
- 実体は 256×256 の CoW タイル（`Arc<Vec<u8>>`。`value::TILE`）で持つ。Indexed8 の番号はパレットの色の数より小さいこと（作るときに確かめる）。
  - ペン・塗りつぶしでは、触れたタイルだけを複製する。取り消しの履歴とキャッシュがタイルを共有するので、メモリが増えにくい。
  - 操作の部品の口はタイル単位だけ（`Raster::update_tiles`、段 10 の `from_tiles_picked`・`derive_picked`：選んだタイルだけを `par_blocks` で作り、ほかは前の値のタイルを共有するか無効なタイルにする）。以前の「タイルと行の帯（64 行）の 2 つの口」の帯の口は作っていない（使う操作がない。`determinism::BAND_ROWS` は並列の分け方の定数として残っている）。行の帯を使うのは、平滑なフィルタの f32 の作業（下）と、ダイジェストの帯（§8.6）の中だけ。
  - プレビューは範囲（タイル）と縮小の段で描ける（§12.5・§12.7）。
- 範囲の描画の約束（段 10）：等倍で範囲を描いた値の有効なタイル（valid）の中は、全体を描いた値と同じ。次の 3 つで守る。
  1. 各タイルを `apply` と同じ閉包で作る。浮動小数の足す順は出力の画素ごとに決まる（タイル・帯の分け方によらない）。
  2. 乱数は絶対位置（`rng_at`。§8.2(3)）。
  3. 入力は、操作が申告した依存（`Op::reach` の逆写し）の中だけを読む。依存の外のタイルは無効なタイル（仮の中身と「読んだ」印を持つ。読むと記録され、`read_rect` は `Internal`）にしておくので、外を読めば失敗する形になる（実行器は捨てて全体で描き直す。§12.5）。申告の正しさは性質テスト（`reach_is_sound`：依存の外を壊しても範囲の中が変わらない）で見張る。
- タイルの 1 辺は 2 つの定数で持つ：`value::TILE`（値の実体と並列の分け方）と `render::region::VIEW_TILE`（配信と変わった範囲の報告の単位）。値はどちらも 256 だが意味が別なので、別の定数にする（片方を変えても、もう片方に効かない）。
- 範囲の座標系：画素はキャンバスの座標で持つ（1 枚の画像はその画像の座標、フレームの列は共通のキャンバスの座標。トリムしたフレームの画素 (x, y) はキャンバスの (offset.x + x, offset.y + y)）。範囲はフレームの番号（画像は 0）ごとの矩形の集合（64 個を超えたら外接矩形に畳む）。音声は全チャンネルに共通のフレームの区間。変わったタイルの報告（§12.6）だけは、フレームの画像の座標のタイル。
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

    // 段 10 で足したもの。どれも既定のままで正しい（遅いだけ）
    /// 描く間の作業領域（入力と出力の値のほか。§12.10）。既定は Work::ZERO
    fn work_bytes(p: &Self::Params, inputs: &[Shape], out: &Shape) -> Work;
    /// 追記だけの引数の読み込みと推論（§12.4）。既定は None（全体で読む・推論する）
    fn parse_appended(prev: &Self::Params, field: &str, items: &[&serde_json::Value]) -> Option<Result<Self::Params, Issues>>;
    fn infer_appended(prev: &Self::Params, prev_inferred: &Inferred, p: &Self::Params, inputs: &[Shape], cx: &InferCx) -> Option<Result<Inferred, Issues>>;
    /// 範囲と変わった範囲（§12.5・§12.6）。既定は false・Global・All・All・None（全体を描く・全体が変わったとみなす）
    const REGION: bool = false;
    fn reach(p: &Self::Params, inputs: &[Shape]) -> Reach;
    fn param_change(old: &Self::Params, new: &Self::Params, inputs: &[Shape]) -> Area;
    fn effect(p: &Self::Params, inputs: &[Shape]) -> Area;
    fn apply_region(p: &Self::Params, inputs: &[&Value], tiles: &FrameTiles, base: Option<&Value>, cx: &mut ApplyCx<'_>) -> Option<Result<Applied, EngineError>>;
    /// プレビューの段（§12.7）。既定は FullOnly（等倍で描く。鎖の継ぎ目になる）・同じ引数
    fn preview(p: &Self::Params) -> Preview;
    fn preview_params(p: &Self::Params, level: u8, inputs: &[Shape], full: &[Shape]) -> Self::Params;
}
```

- `Lift::PerFrame` と申告した画像の操作は、アニメやスプライトの集まりにもそのまま使える。透明のトリムのように「全フレームの外接の合併で切るか、フレームごとか」で意味が変わる操作は、`Lift::None` にして引数で扱う。
- `apply` が受け取る値にはメタデータも付いているが、出力のメタデータはエンジンが入力 0 のものを写して付け直す（§6）。メタデータを書く権限（`MetaAccess::Write`）を申告できるのは、メタデータを設定・消す操作（`meta.set_grid`・`meta.set_pivot`・`audio.set_loop`）と、値の種類を変えて格子を書く・消す操作（`sprite.split_grid`・`sprite.pack_grid`）だけ。Write の操作が座標を持つメタデータを写すときは、`ApplyCx::rules`（文書の全体の規則の版）を `mapping::map_image_meta_for` に渡して、文書の規則の写し方を使う（規則の版を上げても、古い文書を古い写し方で描けるように）。フレームに持ち上げる操作は書く権限を持てない（登録で断る）。
- 持ち上げのとき、各フレームはフレームの列のメタデータを持った 1 枚の画像として渡す（単体の画像と同じ見え方）。持ち上げられるのは、キャンバスいっぱいのフレームの列だけ（推論で `Trimmed` なら計画の段階で、`Unknown` なら実行のときに確かめて断る）。出力の大きさが決まらない（`AtMost`）入力でも、出力の大きさが入力で決まる操作なら持ち上げられ、写し方は実行で決まる。入力の大きさが決まっているのに出力が `AtMost` になる操作（中身で大きさが決まる）は、フレームごとに大きさが変わりうるので持ち上げない。
- `Applied` の `space`・`time` は実際の写し方。推論が `Unknown` なら必ず返し、そうでなければ返さないか推論と同じものを返す。実行器が確かめ、食い違えば内部の誤りにする（E9）。
- **段 10 で足したメソッド**（ADR-154）は、どれも既定のままで正しい（遅いだけ）。既定では、実行器は引数を全体で読み・推論し・当て直し、範囲の描画を使わずに全体を描き、変わった範囲を全体とみなし、プレビューでは等倍で描く（そこが継ぎ目になる）。等倍の値に効くのは `apply`（と `apply_appended`・`apply_region`・`audio_kernel`。どれも `apply` と同じ値を返す約束）だけ。
- `apply_appended`（段 5 からある口）を実行器（`render_cached` と、`render_area`・`render_view` の全体で描くノード）が使う条件：持ち上げない・先に解く解析がない・メタデータを写す操作（`MetaAccess::Map`）で写し方が恒等（`space`・`time` が `Identity`。写す前と写したあとの値が同じ）・前回の描画と実装（ID・版）・規則・派生した種・段・入力の指紋が同じで鍵が違う・前回の出力（由来 `Exact`）がキャッシュにある（§12.4）。`prev_out` には前回の描画の出力（メタデータを写したあとの値）を渡す。警告は足した分だけを出す（前の分は実行器が前回の警告を前に足す）。追記でなければ None（全体を当て直す）。
- `parse_appended`：読んだ引数 `prev` の追記だけの配列 `field` に、正規の形の要素 `items` を足した引数を、足した分だけ読んで作る。結果（読めない要素の指摘も）は、足したあとの JSON を全体で読んだものと同じ（文言は `op::unreadable_params` でそろえる）。`infer_appended`：同じ入力の形での `prev` の推論から、`p` の推論を足した分だけで求める。結果（指摘も）は `infer` と同じ。どちらも既定は None で、段 10 では `paint.actions` が実装する（全例のすべての切れ目で、全体と同じことを `parse_appended_equals_parse`・`infer_appended_equals_infer` が照らす）。5 万本の筆の文書でも、追記 1 回の推論が O(全点) にならない。
- `REGION`・`apply_region`（範囲の描画。§12.5）：`REGION` が真で、`reach` が `Global` でない引数では、`apply_region` が必ず Some を返す（`region_declarations_match_the_implementation` が見張る）。偽か `Global` なら、実行器は全体を描く（入力も全体で）。`apply_region` は `tiles`（フレームごとの値のタイル）だけを `apply` と同じ値で描き、ほかのタイルは `base`（前の出力。大きさ・形式・パレットが同じ）のタイルを共有するか、無効なタイルにする。入力は逆写しで求めた範囲を覆うタイルだけが有効で、その外を読むと誤りになる。メタデータ・写し方・警告は `apply` と同じ（範囲によらない）。持ち上げる操作は 1 フレームずつ呼ばれる。段 10 で実装した操作：`image.crop`・`flip`・`rotate90`・`canvas`・`resize`（最近傍・平滑・同じ大きさ）・`convert`（Indexed8 以外）・`meta.set_grid`・`meta.set_pivot`・`palette.replace`（どれも `apply` の閉包を関数に切り出して使い回す。値は同じ）。
- `reach`：出力が入力 0 のどこに依るか（`render::region::Reach`。§12.5）。保守的であること（大きめでよく、小さすぎてはいけない）。2 つ目からの入力には、いつも全体で依るとみなす。`param_change`：入力が同じとき、引数を `old` から `new` に変えて出力が変わりうる範囲。`effect`：出力が入力 0 と違いうる範囲（外す・入れるときに使う。出力と入力 0 の形が同じときだけ）。範囲はどれもキャンバスの座標（§5.2）で、持ち上げる操作は 1 フレームの画像の座標で返し、実行器が全フレームに広げる。段 10 の申告：`reach` は範囲の描画を持つ 9 つの操作と `paint.actions`（塗りつぶしがなければ `Same`）・`audio.gain`・`fade`・`mono`（`SameTime`）・`trim`（`Segments`）・`resample`（`Rate`）・`audio.set_loop`。`param_change`・`effect` は、`image.canvas`（`fill` だけを変えたら入力の外の部分）・`paint.actions`（`param_change` は共通の先頭と末尾を除いた違う動作の範囲の和、`effect` はすべての動作の範囲の和。ストローク 1 本の範囲は、点の列を 32 個までに区切り（1 区切り 8 点以上。境の点は両方の区切りに入れる）、区切りごとの外接矩形（`paint::stroke::stroke_bounds`。筆の広がりを足してキャンバスで切る）の和：道は隣り合う 2 点の外接矩形の中を通るので、和は押した画素をすべて覆い、1 つの外接矩形より締まる（`stroke_area_covers_long_strokes`）。塗りつぶしはそのフレーム全体（`param_change` では、違う動作のあるフレームなら、後ろの同じ塗りつぶしもそのフレーム全体））・`audio.fade`（区間）・`meta.*`・`audio.set_loop`（画素・標本が変わらない。`Empty`）と、`image.convert` の `effect`（同じ形式なら入力のまま）。`image.trim_alpha`・`sprite.split_grid`・`sprite.pack_grid`・`audio.normalize` は既定（`Global`）のまま。
- `preview`・`preview_params`（プレビューの段。§12.7）：`Preview::Scaled`（引数を段に縮めて描ける）・`Preview::Skip`（画素を描かず、メタデータを段の座標で書く）・`Preview::FullOnly`（既定。等倍でしか描けない）。`preview_params` は段 `level`（1〜3）の引数を返す。`inputs` は段の入力の形、`full` は等倍の計画での同じノードの入力の形（縦横比を保つ自動の軸を段の形だけから求めると、丸めのずれが広がるため）。矩形を入力の中に収める必要のある操作は段の入力に収め（clamp）、大きさを決める操作は等倍の出力の ⌈n/2^level⌉ を目指す。実行器は結果を段の形で推論し直し、通らなければ Scaled は計画を段 0 に落とし、Skip はそのノードを外す。操作ごとの扱いは §12.7 の表。
- `work_bytes` と `Work { shared, per_task }`（メモリの予算。§12.10）：描く間に、入力と出力の値のほかに使う作業領域の見積もり。`shared` は描く間に 1 つだけ持つ分（色の表・重みの表・途中の平面）、`per_task` は並列の仕事（`par_blocks` の 1 ブロック・1 タイル）ごとの分で、同時に動く仕事の数（プールのスレッドの数。呼び手が知っている）を `Work::total(tasks)` に渡して足す。`inputs` は `infer` と同じ形（持ち上げる操作はフレームの画像の形。フレームは順に当てるので 1 フレームぶん）。値を変えない。既定は 0（タイル・ブロック 1 つぶんの一時の領域は数えない）。値の数倍を持つ操作だけが上書きする：Indexed8 への `image.convert`（色の表）・平滑な `image.resize`（重みの表と列の作業。`imaging::resample::smooth_work`）・`render_all` で描く音声の 6 つ（`audio.gain`・`trim`・`fade`・`mono`・`normalize`・`resample`。出力 1 チャンネルぶん）・`paint.actions`（動作は順に当てるので、動作ごとの量の最大を `shared` で申告する。フレームの列はキャンバスの大きさで求める。差分の適用も全体の申告に収まる）。0 でない操作の一覧は `ops::tests::work_bytes_are_declared_where_values_are_multiplied` が見張る。
  - `paint.actions` の動作ごとの量（`PaintAction::work_bytes`）：塗りつぶしは `paint::fill::fill_work`。画素の集合 2 枚ぶん（同じ色の画素の集合と、それを集める間のタイルごとの語。連結なら、集めたあとは塗った画素の集合。1 枚は行を 64 ビットの語にそろえた `paint::mask::mask_bytes`）で、連結ならさらに区間の待ち行列の上限ちょうど（`paint::fill::queue_cap`：画素 2 つに 1 要素 ＋ 行の数の 2 倍 ＋ 65,536。1 要素 12 バイト）。待ち行列は描く前に上限ちょうどを `try_reserve_exact` で確保して伸ばさない（倍々に伸ばすと上限の 2 倍近くまで確保しうる。申告と、上限を超えたときの `Limit` が同じ式から決まる）。1px の市松を 8 連結で塗ると区間が 1 画素ずつになって待ち行列が画素の数に比例するので、上限で数える（ふつうの形では確保だけで触れる頁は少なく、見積もりは大きめに出る）。ストロークは画素の集合 1 枚 ＋ 筆の形の画素の列 ＋ 点の写し（どれも 1 点 16 バイト）。実測は §9.9。確かめ：`ops::paint::actions` の `work_covers_the_fill_and_stroke_buffers`・`paint::fill` の `work_counts_the_masks_and_the_whole_queue`、kura-devtools の `mem_smoke` の `png-fill-png`。
- 引数の指紋とノードの鍵（`op.rs`。段 10）：
  - `ParamSchema::append_field()`：追記だけの配列の引数（`ParamKind::Strokes`）の名前。ちょうど 1 つのときだけ返し、ない・2 つ以上なら None（差分の口を使わない）。`Registry::append_field(op, v)` は、`resolve` が使う実装（その版か、すぐ上の実装のある版）の記述から引く。
  - `params_digest(schema, 正規の引数)`：追記だけの配列があれば、その配列を要素ごとの連鎖（`append_chain`：blake3("kura-param-append-v1" ‖ 前 ‖ 要素の正規の JSON)）にし、残りの項目の正規の JSON と要素の数と合わせる。なければ正規の JSON そのまま（どちらもドメイン "kura-params-v1"）。追記した分だけ求め直せる（`params_digest_with_chain`）。既定値を書いたかどうかのような書き方の違いは別の指紋になる（キャッシュが外れるだけで、誤りにはならない）。
  - `node_key(op, 文書に書いた版, 引数の指紋, seed, enabled)`（"kura-node-key-v1"）。ノード ID と入力は含めない（ID は継ぎ足しでずれ、入力は実行器の指紋が足す）。移行するノードは、移す前（文書に書いたまま）の引数で数える。セッション（`Registry::doc_node_key`）と実行器（`compile`）が同じ式・同じ実装の記述で求める（`session_keys_equal_compile_keys`）。
  - どれもメモリの中だけで使い、保存しない。版をまたいで保つ約束もない。

### 7.2 引数の記述

- `OpParams::schema()` が、引数ごとの名前（serde のフィールド名と一致）・種類（整数・小数・真偽・列挙・矩形・点・大きさ・比・長さ（`Length`）・格子（`Grid`）・色・時刻・色の対応表（`PaletteEntries`）・アイテム・筆の点の列（`Strokes`））・範囲・刻み・単位（px・%・度・dB・LUFS・ms・サンプル・Hz・倍）・既定値・表示の条件を返す。
- 表示の条件（`ParamSpec.visible`。M12 の段 4）：`Visible { key, values }` は「同じ操作の列挙の引数 `key` の値が `values` のどれかのときだけ、この引数を出す」（`spec(…).when(key, values)`）。1 段だけ（条件の引数は条件を持たない）で、値の検証・既定値には効かない（出さない引数も文書に書いたまま読み、既定値を持つ）。今の条件は `image.convert` の `colors`・`dither`（`format` が `indexed8` のとき）と `matte`（アルファを外す `gray8`・`rgb8`・`gray16`・`rgb16` のとき）。条件の引数が同じ記述の列挙で、値がその列挙の中にあることを `tests/conformance.rs` の `visibility_conditions_name_enum_params` が確かめる。記述は feature `specta` で TS に出し、bindings の定数 `EDIT_OPS`（今の版のカタログ）でフロントが読む。
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
   - エンジンはスレッドプールを作らない（段 10）。`par_blocks` は呼ばれたスレッドのプールで回り、プールは呼び手が作って、その `install` の中で描く（kura-core の `EngineRuntime`。§12.11）。`tests/rules.rs`（`engine_src_creates_no_pools`）が、src（テストのモジュールを除く）に `ThreadPoolBuilder`・`.install(`・`::install`（UFCS の `rayon::ThreadPool::install(&pool, ..)`・`<ThreadPool>::install`・関数として取り出す `ThreadPool::install`）・`build_global` がないことを走査で確かめる（テストはプールを作るので、clippy.toml では禁じない。走査の確かめは `pool_scan_sees_code_and_skips_tests`）。
   - 段 10 で clippy.toml に足した禁止：rayon の `ThreadPool` の口（`join`・`scope`・`scope_fifo`・`in_place_scope*`・`spawn`・`spawn_fifo`・`spawn_broadcast`・`broadcast`・`current_num_threads`）、`rayon::scope_fifo`・`in_place_scope*`・`spawn_fifo`・`broadcast`・`spawn_broadcast`・`join_context`・`max_num_threads`、`par_sort*`（unstable を含む）・`par_chunks_exact*`・`par_rchunks*`・`par_windows`・`par_split*`・`par_chunk_by*`・`par_extend`、`std::thread::Builder::spawn`・`spawn_scoped`。
   - 段 10 の検証で足した禁止（`into_par_iter`・`par_iter` を通さずに並列のイテレータを作る口。`collect`・`map` などの終端は禁じていないので、作る口で止める）：`rayon::iter` の `split`・`repeat`・`repeat_n`・`repeatn`・`once`・`empty`・`walk_tree`・`walk_tree_prefix`・`walk_tree_postfix`、`ParallelDrainRange::par_drain`・`ParallelDrainFull::par_drain`、`rayon::str::ParallelString` の `par_chars`・`par_char_indices`・`par_bytes`・`par_encode_utf16`・`par_split`・`par_split_inclusive`・`par_split_terminator`・`par_lines`・`par_split_whitespace`・`par_split_ascii_whitespace`・`par_matches`・`par_match_indices`、`ParallelSlice::par_split_inclusive`・`par_array_windows`、`ParallelSliceMut::par_split_inclusive_mut`（26 個）。
   - 決定性の約束は値（ダイジェスト）だけ。複数のブロックが誤りを返したとき、`par_blocks` がどれを返すかはスケジュールで決まる（中止と上限が同時に起きたときに `Canceled` か `Limit` か、などは約束しない）。描けた値はスレッドの数によらない。
3. 乱数は ChaCha8 だけ。種は `determinism::seed(node.seed, 元のハッシュ, 用途)`（kura-core の `splitmix64`・`fnv1a64` と同じ値）で作り、鍵は `key_from_seed`（splitmix64 で 32 バイトに広げる。rand_core の `seed_from_u64`・`from_rng` は禁止）。乱数の流れは**絶対位置**で決める（`rng_at`：画像は行、音声はチャンネルを流れにして、列の中の位置へシークする）。タイル・ブロック・フレームの番号を流れに使わない。アニメのフレームはどれも同じ乱数になる（ディザのちらつきを防ぐ）。種がノード ID・画面の操作の順・一括の並びに依存しないので、エディタと一括で同じになる（E4・E5）。
4. 出力に効く場所で `HashMap`・`HashSet`・`RandomState`・`DefaultHasher` を使わない（`BTreeMap` か並べた `Vec`）。
5. 描画の中で時刻・環境変数・引数（`env::var*`・`vars*`・`args*`）を読まない。`#![forbid(unsafe_code)]`。メモリの予算（§12.10）の一括の待ちは、中止を 2 ms ごとに見回るのに `Condvar::wait_timeout` を使う。待つのは描く前（プールの外）で、時刻を読まず、値にも効かないので、この規則に反しない。ベンチの時計は呼び手が渡す（`testing::bench::measure`）。
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
  - 段 10 で足し方を速くした（値は同じ）：行を `DIGEST_BAND_BYTES`（4 MiB）までの帯に詰めてからまとめて足す（帯の行の数は max(1, 4 MiB ÷ 行のバイト数)。幅が 1 タイルに収まる画像はタイルをそのまま足す。帯の領域を取れなければ行ごとに足す）。足すバイト列は同じなので値は変わらない（`banded_digest_equals_row_by_row`：ChaCha8 で選んだ大きさ・形式・帯の行の数で、旧い足し方と同じ）。M4 Max で、4096² の RGBA8 が 55 → 31 ms、24MP の RGB8 が 61 → 36 ms。
  - プレビューの道では取らない：`render_cached` は `RenderOptions.digests` が真のとき（保存・一括・照合）だけ取り、取ったものは出力の指紋ごとに覚えて取り直さない（§12.3）。縮小の段の計画でダイジェストを頼むと `Unsupported`、範囲の描画の値（無効なタイルのある値）は `Value::digest` が `Internal` で断る（§12.5・§12.7）。
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
- 読む場所だけを変える口（M11 の決定 23）：`CodecSet::replace_decoder` は今の版のデコーダを、識別子・段・再現の約束・環境が同じものに並びを変えずに差し替える（旧版・違う約束のものは断る。読み取りの記録は前と同じ）。固定の段の SVG を別のプロセスで読む kura-core の `codec_ext::svg_worker` が使う（§9.5）。`CodecSet::shape_path_monitored` は形を読む口に見張りを渡す（`Decoder::shape_file_monitored`。中止は `Canceled`）。形を読むのに時間のかかりうるデコーダ（別のプロセスで読む SVG・ffmpeg の段）を、ジョブ（サムネイルの見積もり・解析の見出し）の中止で止める。
- 組み立て：`CodecSet::pinned()`（固定のコーデックと旧版）か `CodecSet::pinned_with(&PinnedOptions { svg_fonts })`（`svg_fonts` は `codec::fonts::SvgFonts`：フォントのファイルの中身をバイト列か mmap した領域のまま共有で受ける入れ物 `FontSource` の列から、名前の順に fontdb の集まりを作る。`SvgFonts::lazy` は文字を含む SVG を初めて読むときに作る。アプリは `kura_mac::pinned_options()` で、`/System/Library/Fonts` と `Supplemental` の ttf・otf・ttc を読み取り専用のボリュームなら mmap して渡す（M11 の決定 12。プロセスで 1 つの集まりを解析・サムネイル・アニメ・エディタで共通に使う。写す道は 370 ファイル・787 書体で物理フットプリント +2.6 MiB、全部をヒープに読むと +781 MiB）。渡さなければ文字を含む SVG は `Unsupported`）に、kura-mac・ffmpeg の段を `add_decoder`・`add_encoder` で足す。識別子はデコーダ（旧版を含む）・エンコーダのそれぞれで重ならないこと（重なれば `Err`。記録の識別子から 1 つに決まらなくなるため）。`all_decoders_for(format)` は旧版を含めて、その形式を読めるデコーダを返す（ファズ・照合のため）。
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
| aseprite | image | 自作（`aseprite-kura-dec/1`。合成した各フレーム）。`.ase` は Adobe の色見本と拡張子が重なるので、取り込みで先頭のバイト（0xA5E0）を見て、Aseprite でなければ取り込まない（`import::kind`。M11） | — |
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
| ttf・otf・ttc・woff・woff2 | font | —（値にしない。情報とサムネイルは kura-mac の CoreText（`kura_probe` の種類 3 と、字の形だけのアルファの PNG の見本）。M11） | — |
| glb | model | —（値にしない。情報と描画は自前の `model`：JSON の塊だけから数える `read_info`（`glb-kura-read/1`）と、焼き込み・ソフトウェアの描画で Rgba8 の画像にする `load_scene`・`render`（`model-kura-render/1`）。M11 の決定 9。§9.10） | — |
| 動画 | video | —（編集しない） | — |

- C のライブラリは同梱しない。C か ffmpeg がないと書けない形式は、エンコーダを Rust で自作する（Ogg Vorbis を最優先。MP3・不可逆 WebP・Opus は必要になった段で。ADR-137）。
- 音声のタグは、各コーデックが自前で読み書きする（WAV の INFO、AIFF の NAME・AUTH・`(c) `・ANNO、FLAC・Ogg の Vorbis コメント、m4a の iTunes のタグは読みだけ）。Vorbis コメントは、既知の名前（title など）を先に、`vorbis:<KEY>` をあとに書き、読み取りが同じ名前にまとめるもの（大文字小文字だけの違い・DESCRIPTION と COMMENT・小文字ちょうどの `cprt` と COPYRIGHT）が重なれば先の 1 つだけを書く（読み取りと書き出しは `codec/symph.rs` の 1 つの表 `KNOWN_TAGS`・`comment_name` で決める。読み取りは同じ名前の 2 つめ以降を捨てるので、書いて・読んで・書き直すと同じコメントになるように。FLAC はバイト列も同じ、Ogg はシリアル番号と CRC を除いて同じ）。外のファイルの同じキーの複数の値は、最初の値だけを運ぶ。読み取りが作らない形（`vorbis:foo` だけ・`vorbis:TITLE` のような別名だけ・`vorbis:DESCRIPTION` だけ・`vorbis:cprt` だけ）は、値は保たれるが、書き直すと名前か並びが変わる。`vorbis:CPRT` と `vorbis:cprt` は読み取りが別の名前にするので両方書く。symphonia がタグでなく章・画像として読む名前（CHAPTER…・METADATA_BLOCK_PICTURE・COVERART）を `vorbis:<KEY>` で書くと、読み直したときに値はタグに残らない。lofty は段 6 では入れない（ID3v2・MP4 のタグを書く段で選び直す。ADR-148）。MP3 の ID3 は段 6 では読まない。EXIF・ICC は運ぶ。PNG の tIME・MP4 の作成日時など、時刻は書かない（R2 のため）。

- EXIF の向き（Orientation）：JPEG・PNG（eXIf）・WebP は、読むときに向きを画素に当て、運ぶ EXIF の向きを 1 にする（同じ向きを 2 回当てない）。形（`shape`）も向きを当てた大きさを返す。PSD は画素が表示のとおりなので向きは当てず、EXIF の向きだけ 1 にする。

### 9.3 能力

`CodecSet::capabilities() -> Vec<FormatCaps>`（段 9。環境は組み立てた `CodecSet` そのもの）が、形式の表の順にすべての形式について、読み手・書き手（段の順で、先頭が既定）と、形式のメタデータの能力（可逆か・アルファ・ICC・アニメ・ループ点）、書き手ごとの書ける値の種類・保存できるメタデータ・選べる設定・再現できるか（R2）・環境（`env`）を返す。macOS の能力は kura-mac が問い合わせ（`kura_codec_caps`：UTI・FourCC・レートの範囲・OS の build・ImageIO の版）、ffmpeg は `-decoders`・`-encoders`・`-demuxers`・`-muxers`・`-h encoder=libmp3lame` と LAME の版を調べて、足せる段だけを足す。エディタの「保存の形式」と一括加工の事前チェックはここを見る。`kura-devtools edit caps` が今の環境の表を JSON で出す。

### 9.4 アンカー

- 再現を約束しないデコーダ（`reproducible()` が偽：Platform・External と、固定の段でも OS の libm を使う SVG）で読んだ元（HEIC・AVIF・HE-AAC・Opus・SVG・ffmpeg で読んだ音声など）は、初めて保存するときに、読み取った結果を可逆形式で残す（ADR-135。M12 の利用者の決定 7 で「初めて編集するとき」から改めた：保存のジョブが描いたのと同じ読み取りから作る）。OS・ffmpeg の更新で読み取り結果が変わっても、途中の版を作り直せるようにするため。Ogg Vorbis と 1・2 チャンネルの AAC-LC は段 9 で値を再現できるようにしたので、アンカーは要らない（ADR-153）。3 チャンネル以上の AAC と HE-AAC は後の段で読むので、アンカーを作る。
- 入れ物（段 9。`codec::anchor`、`anchor-kura-enc/1`・`anchor-kura-dec/1`）：画像は PNG・APNG（ICC は iCCP）、音声は f32 の WAV（3 チャンネル以上は拡張形式）。標準のチャンクだけでは値のダイジェストまで往復しない（`source`・タグ・ピボット・格子・表示時間など）ので、私用のチャンク（PNG は IDAT の前に `kuRa`（ヘッダ）と `kuEx`（EXIF の生のバイト列）、WAV は data のあとに `kura`）に、元の読み取りの記録（`DecodeStamp`）とメタデータの正確な値を書く。ヘッダは「BLAKE3（ドメイン `kura-anchor-header-v1`、長さ付きの JSON）32 バイト ‖ 正規の JSON（`{v, anchor, body, decoded, source?, format?, options, content}`）」。FLAC は使わない（非可逆の音声をデコードした値は 24 ビットの格子に乗らず、1 を超える標本・−0 もある）。キャンバスいっぱいでないフレームの列は断る（浮動小数の画素の形式は値にない。足すときはヘッダの `v` を上げて入れ物を増やす）。
- 約束：アンカーを読み直した値のダイジェストは、元の読み取りの記録（`DecodeStamp.value_digest`）と同じ。作るときに読み直して照らし（`write_verified`・`verify`）、読むたびにも照らす。中身は固定のデコーダ（`png-rs-dec/2`・`wav-kura-dec/1`。`BODY_DECODERS`）だけで読み、それらは旧版として永久に残す。バイト列（R2）は約束しない（PNG の圧縮は fast）。知らない版・知らない中のデコーダは `Unsupported`、壊れた・別の記録のものは `Mismatch::AnchorBroken`、上限は `Limit`。中止は、書くとき画像は 64 行ごと（APNG はフレームごと）・音声は 16,384 フレームごと、読むときは中のデコーダの間隔（PNG は 256 行ごと）。
- 作り直し（`codec::recorded::read_recorded`。E29）：アンカーがあればアンカーだけを読む（元は読まない）。照合は記録の値のダイジェストの一致だけで、環境（`env`）が違っても同じ値なら使える。アンカーがなければ、記録の識別子のデコーダで読み直す。食い違いは失敗ではなく `Recorded::Mismatched`（`SourceChanged`（今の値と記録を添える）・`DecoderGone`・`AnchorBroken`）で返し、呼び手（M12）がアンカーを使う・利用者に知らせる。乱数の種（`InputValue.source`）は、アンカーから読むときも元のファイルのハッシュのまま（ディザの出力を変えないため）。
- 大きさの目安：24MP の画像で約 28 MB（書き 0.21 秒・読み 0.29 秒）、3 分の 44.1 kHz ステレオで約 64 MB。4 GiB を超える音声は RIFF に収まらないので作れない（保存を失敗にする。M12）。
- 置き場所と鍵（M12 の段 2）：アンカーのファイルは `files/<hash>.<png|wav>`（実ファイルの台帳 `blobs` に載せ、整合性チェックの対象にする。台帳に同じ中身が別の拡張子であればその拡張子）。行は `edit_anchors` に「元のファイルの中身 × 読み方の設定のダイジェスト（`DecodeOptions` の正規の JSON の、ドメイン `kura-decode-options-v1` の BLAKE3）」ごとに 1 つ（`edit::anchors`）。版の入力（`item_version_inputs`）が `anchor_id` で指し、記録（`RecordedInput.anchor`）にアンカーのファイルの中身を書く。
- 作る時：最初の保存のジョブが、描くのと同じ読み取りから作る（§14 の「保存のジョブ」の段 3。`staging/edit-<job>-anchor.<png|wav>.part` に書いて `sync_all`、読み直して照らしてから、出力と一緒に予告 → 配置 → 1 トランザクションで台帳と行に載せる）。メモリの予算は、描く段のピークと「読む持ち越し ＋ 入力の値 ＋ 書いて読み直す分（`anchor::work_bytes`。読み直した値）」＝ 値の約 2 倍の大きいほうを確保する（`EngineRuntime::admit_bulk_with`。アンカーは読んだ直後・描く前に書くので、描く段とは重ならない）。どのデコーダが読むかは読むまで分からないので、元の形式に再現を約束しないデコーダがあれば、この分を見込む。
- 以後はアンカーから読む：同じ元 × 読み方の設定の保存・作り直しは、元を読まずにアンカーだけを読み（`read_recorded`。記録はアンカーの元の読み取りの記録）、OS のデコーダを呼ばない。
- アンカーが使えない（形・中身を読めない、ファイルがない、壊れている、別の値）とき：保存（上書き・別名）も版に戻す作り直しも、元をアンカーの記録の識別子のデコーダ（`decoded.codec`）で読み直し、値のダイジェストがアンカーの記録（`decoded.value_digest`）と同じなら、その値で続けて知らせを版の記録の警告に残す（入力の記録は同じ値のアンカーを指したまま。結果の `anchor` は null。アンカーのファイルは直さず、整合性チェックが数える）。値が違う・記録のデコーダがこの環境になければ `Mismatch`（何も変えない）。読み直すデコーダが Platform の段なら、見出しを読む前から `decoding` の印を付ける（§14 の表の 1・2）。保存の描いている間にゴミ箱を空にしてアンカーの行が片付けられたら、予告のトランザクションで `Conflict`（何も置かない。もう一度保存すれば元を読み直して作り直す）。

### 9.5 段 4 の画像のコーデックの約束

詳しい約束は各コーデックのソースの冒頭（`crates/kura-engine/src/codec/*.rs`）に書き、ここは要点だけ。どれも、形（`shape`）と中身（`decode`）が必ず一致するように、形式やフレームの数を決める判断は画素を展開しない読み取りで先に行う。書いたフレームの列を読み直すと、フレームが 1 つでもフレームの列に戻る（APNG は acTL、GIF は NETSCAPE の拡張が印になる）。

- **PNG**（`png-rs-dec/2`）：acTL と fcTL があれば、フレームが 1 つでもフレームの列（acTL のフレームの数と fcTL の数が違えば壊れたファイル）。キャンバスに fcTL の blend（SOURCE・OVER。OVER は `imaging::composite::over` の整数の式）で重ね、dispose（NONE・BACKGROUND・PREVIOUS）を当てたキャンバスいっぱいのフレームの列にする。インターレースのフレームは、フレームの大きさで Adam7 を展開する。パレットの APNG は、番号のまま重ねられるとき（半透明の色を OVER で重ねない・消すときに透明な色がある）だけ Indexed8。そのとき消した画素は消す色の番号（パレットの RGB でアルファ 0）になり、広げた色の RGB は仕様の「透明な黒」と違うが見えない（Rgba8 の経路と見える画素は同じ）。色・深さ・パレット・tRNS は png クレートが読んだものを使い（奇数長の tRNS・PLTE より前の tRNS などは png クレートに従う）、チャンクの並びは前から順に読む（acTL・fcTL・eXIf のほかは中身を読まずに飛ばす）。eXIf は IDAT より前のものだけで、その大きさはテキスト・ICC と合わせて `max_meta_bytes` に数える。インターレースでない 1 枚の画像は行ごとに読む（作業領域は 1 行と 1 タイル行）。`png-rs-dec/1`（APNG は既定の画像だけ、eXIf を読まない）は旧版として残す。書き出しは、キャンバスいっぱいのフレームの列を APNG（表示時間は 65,535 ms まで ms のまま）、EXIF を eXIf に書く。
- **JPEG**：zune-jpeg を SIMD なしで使い、どのターゲットでも同じ整数の経路。JFIF の解像度は自前で読む（zune-jpeg は読まない。マーカーを前から順に読む）。プログレッシブは係数を全部持つので、値の 3 倍が上限を超えれば断る。書き出しの設定は `quality`・`chroma`・`progressive`。透明な画素のある画像・16 ビットは書かない（先に背景の合成・8 ビットへの変換）。
- **GIF**：ブラウザと同じ合成（透明の番号は描かない、Background は透明、Previous は戻す、背景色は使わない）。フレームが 2 つ以上か NETSCAPE の拡張があればフレームの列（繰り返しは、拡張がなければ 1 回、0 なら無限、n なら n + 1 回）。局所パレットがなく大域パレットがあれば Indexed8（消す色が要れば透明な色をパレットの後ろに足す。256 色で足せなければ Rgba8）。書き出しは `alphaThreshold`（既定 128）で 2 値の透明にし、色が収まらなければ自作のメディアンカット（`imaging::quantize`。整数だけ）で減色、フレームの列は 1 つの大域パレット。1 回だけ再生するフレームの列は NETSCAPE の拡張を書かないので、1 フレームで 1 回の列は読み直すと 1 枚の画像になってしまう。そこで書かずに `Unsupported` にする（先に 1 枚の画像にする）。
- **WebP**：image-webp の合成（四則演算だけ）を、背景を透明にして使う。ICC と EXIF の合計を `max_meta_bytes` で抑える。書き出しは可逆の 1 枚だけ（不可逆・アニメは自作のエンコーダを作る段で。エンコーダの `caps` もアニメを書けないと答える）。
- **aseprite**：自作。層を下から合成（見えない層・参照層は描かない、ヘッダの印でグループを別に合成、層の不透明度の印、z-index）。ブレンドは `imaging::blend`（Aseprite の `blend_funcs.cpp` の式）で、asefile と 300 個のランダムな絵で一致を確かめている（`tests/aseprite_ref.rs`）。パレットの絵は、すべて通常・不透明度 255 で、パレットの色がどれも不透明（背景の層がないときの透明の番号を除く）なら番号のまま Indexed8、ほかは Rgba8。グレーは RGBA で合成して Rgba8。スライス（キャンバスの座標）・タグ（`ImageMeta.tags`）・ICC を運ぶ。タイルの反転と外部のタイルセットは段 4 では読まない（`Unsupported`）。グループの入れ子は 64 段まで。セルはキャンバスの範囲に切って描き、タイルマップはキャンバスに重なるタイルだけを描く（どちらも 64 行ごとに中止を確かめる）。1 フレームを描くあいだに展開するセル・タイルセットは合計 `max_value_bytes` まで（タイルの爆弾を断る）。フレームをまたいで使い回す展開済みのものは、タイルセットと、後のフレームのリンクが指すセル（それを描く最後のフレームまで）だけを、合計で値の大きさ（と `max_value_bytes`）まで持ち、超える分は持たずに使うたびに展開し直す（値は同じ。§9.9）。セルは、描く前にフレームごとの「層 → その層の最初のセル」の表を 1 度だけ作って引く（描く順の z・リンク・最後に使うフレームのどれも。ない層のセルや層の多いファイルでも、セルの数 × 層の数の手間にしない）。表を作る・引く手間 4,096 ごとに中止を確かめる。
- **PSD・PSB**：自作。合成済みの画像だけ（層があって合成済みの画像が本物でないものは `Unsupported`）。ビットマップ・グレー・インデックス・RGB の 8/16 ビット、圧縮なし・RLE。合成済みの画像の ZIP はツールによって流れの分け方が違い、確かめる相手もないので `Unsupported`（次の段へ）。PSB は長さの欄が広がる所（層とマスクの情報の長さは 8 バイト、RLE の行の長さは 4 バイト）を読み分け、どの長さもファイルの残りの長さと照らしてから使う。透明は、層の数が負か Mtrn などがあるときだけ。Photoshop の白の下塗りは、下塗りでしか成り立たない条件がすべての画素で成り立つときだけ戻す（ImageMagick などは下塗りしない）。CMYK・Lab・32 ビットは次の段へ。拡張子は psd・psb。
- **SVG**：resvg。結果は OS の libm に依存しうるので `reproducible()` は偽（アンカーを作る）。`<image>`（外部のファイル・data: のラスター画像・入れ子の SVG。pattern・mask などの中も）を含むものは `Unsupported`（resvg の raster-images は zune-jpeg の SIMD を有効にするので入れない）。文字は `PinnedOptions.svg_fonts` で渡したフォントだけで描き、渡していなければ `Unsupported`。usvg に渡す前に、ファイルの大きさ（16 MiB）・DTD の実体の展開（64 MiB）・要素の数（2²¹）・`<use>` の展開・marker の頂点ごとの複製（頂点の数 × marker の中の要素の数）を見積もって断る。DTD は引用符・注釈・PI を区別してバイト列のまま読み、パラメタ実体・外部の実体・マークアップや参照を含む実体は断る。入れ子の marker も断る。roxmltree・usvg・resvg は入れ子の深さだけ再帰し、スタックを使い切ると受け止められずに落ちるので、XML を読む前に生のバイト列で入れ子の深さを数えて 64 段で断り（2 MiB のスタックのデバッグのビルドで、重い形が 96 段まで動くことを測った）、参照（`<use>`・`url(#…)`・祖先から受け継ぐ fill・stroke・marker・CSS の規則）を展開した高さも 64 段で断る。参照の輪（pattern の中身が受け継いだ fill で自分を描き直す、など。ファズで見つけた）も断る。CSS の規則は、要素名・`.class`・`#id` の単純なセレクタだけ当たる要素を決め、それより複雑なセレクタはどの要素にも当たりうるとみなす。見積もりは再帰せずにたどり、手間（要素ごとの参照と辺の数の合計）は 2²⁴ まで。usvg はすべてのセレクタをすべての要素に照らし、子孫・`~` の結合子では祖先・兄弟をさかのぼって後戻りし（試す並びは、文書の深さ・兄弟の数から結合子の数だけ順に選ぶ組み合わせの数）、当たった規則と style 属性の宣言を宣言の数の 2 乗の手間で書き込むので、CSS のセレクタの数 × 要素の数と、これらの手間の合計（当たる要素の数は要素名・クラス・id ごとに数え、複雑なセレクタはすべての要素とする）をそれぞれ 2²⁷ までにする。セレクタの部分の数は 32 まで（simplecss は結合子の数だけ再帰する）。受け継ぐ参照は、受け継がないと分かっているもの（clip-path・mask・filter）のほかはすべて受け継ぐとみなす（`*fill` のような古い書き方の名前も usvg は fill として読む）。usvg は単位が objectBoundingBox の pattern・clipPath・mask と filter を、参照する要素ごとに中身ごと複製する（入れ子で指数に増える）ので、受け継いだ fill・stroke も使う図形ごとに数え、複製で作る要素の数を 2¹⁷ までにする。usvg は弧・円・楕円・角丸を半径に応じた数の曲線に直す（半径 10³⁰ の弧 1 つで 6 秒を超えた）ので、図形の属性・style・CSS の宣言の数の絶対値は 10⁹ まで（色・id・参照・名前の中の数字は数として読まない）。
  **文字のレイアウトの上限**（M11 の検証 S1。`codec/svg_text.rs`）：usvg は文字の塊（位置を付けた字から次の位置の前まで）ごとに字の数の 2 乗の手間をかけ（字ごとに塊の先頭から位置を数え直す）、塊の span ごとに塊を丸ごと形にし直し、書体にない字は集まりのすべての書体を調べる。木を作る `usvg::Tree::from_str` は中止を見ない（M4 Max で 'a' の 1 つの塊が 8 万字で 15 秒、120 KB の SVG 1 件でサムネイルと解析のジョブが各 58 秒、ライブラリを閉じるのも待たされた）ので、usvg に入る前に断る。手間は M4 Max の release の時間（ns）を目安にした単位で数え、上限は 1 件 1 億（100 ms ほど）。XML の木から `<text>` ごとに：要素 8,192 ＋ 字ごと（字の組で、ラテン文字・ギリシャ文字・キリル文字・結合文字・句読点 2,560、記号・CJK・絵文字 20,480、そのほか（アラビア文字・インドの文字・向きを変える制御文字・まれな文字）40,960）＋ 塊ごとに 字の数 ×（3 × 字の数 ＋ バイト数 ÷ 2）＋ span の数 × 字の数 × 塊のいちばん重い字の組の係数（256・512・32,768）。塊は `<text>` の始まりと、x・y のリストの最初が素直な数の text・tspan の最初の字（その節が空白だけなら分けない）で分け、textPath の前後・リストの 2 つめ以降では分けない（どれも塊を usvg より長く数える側）。textPath の中の字は、参照する図形の区間の数（弧を含む path は数の数 × 16、円・楕円・矩形は 256）× 2,048 を足し、塊の 2 乗の走査をもう 1 回足す。これを `<use>` の展開・参照ごとの複製・marker の頂点ごとの複製と同じに掛けて合計する。フォントを選ぶ・代わりを探す手間は書体の数で決まるので、usvg の既定の選び方を呼ぶたびに残りの手間から引き（選ぶ 1 回 8,192 ＋ 書体名の数 × 書体の数 × 2、代わりを探す 1 回 書体の数 × 160 ＋ いちばん重い塊を形にし直す分。同じ字・同じ使ったフォントの並びの答えは 1 つの木の中で覚えて調べ直さない）、尽きたら選ばずに木を作り終え、`Limit` にする。係数は字の種類ごとに測った最悪（字ごと：ラテン文字 2.2 µs、「龘」18 µs、アラビア文字 32 µs、「﷽」35 µs。span × 字：ラテン文字 205〜234 ns、和文・絵文字 352〜446 ns、アラビア文字 8.5 µs、左書きとアラビア文字を 1 字ずつ交互にした向きの並び 29.5 µs。どの書体にもない字を探す 1 回 118 µs（787 書体））。直したあと、上限の近くで受け付けた形はどれも 1 件 92 ms 以下だった（ラテン文字の 1 つの塊は約 4,900 字、和文の 1 つの塊は約 3,300 字、100 字ずつの text は合わせて約 2.5〜3 万字、1 字ずつの span の塊は約 600 字、和文の 10 字のラベルは約 450 個、アラビア文字の 10 字のラベルは約 130 個まで）。これを超える文字の多い SVG は `Limit` になり、サムネイルは failed、解析は `too_large` の failed になる。この範囲で受け入れる（M11 の検証のあとのユーザーの決定 20。上限を広げると、1 件の最悪の時間もその分伸びる）。textPath は字ごとに path の区間をたどり、弧長を変形の拡大に応じた精度で求める（拡大の幾何平均が f32 であふれると精度が 0 になり、区間ごとに 2²⁰ 回分ける。10²¹ の拡大で曲線 4 つ・100 字が 5 分を超えた）ので、textPath に至る変形の拡大の上限（transform・patternTransform・svg と symbol と pattern の viewBox の積を、`<use>` などの参照の先のいちばん大きいものに掛けたもの。viewBox を当てる表示の領域の大きさは、10⁹ と、文書の svg・use・pattern・symbol の幅・高さの単位つきの大きさ（in・cm・mm・pt・pc は 96 dpi の係数）のいちばん大きいもの。100% を超える割合（入れ子で掛け合わさる）・em・ex の幅・高さがあれば無限大。viewBox を持たない pattern は href の先から受け継ぐので、文書の pattern の viewBox の拡大のいちばん大きいもの。中身の単位が objectBoundingBox の pattern・clipPath・mask は無限大（mask の中身は、マスクする要素の外接矩形の大きさで並べ、外接矩形は中の変形で大きくできる）。読めない変形と CSS の transform は無限大。変形のリストは usvg（svgtypes）と同じ文法で読む：数は path と同じく符号・2 つ目の小数点・指数でも区切り（svgo などが出す `translate(10-5)`・`matrix(.5 0 0 .5-10-20)`・`translate(.5.5)`）、関数のあとはコンマ 1 つまで、svgtypes が読めない書き方（コンマで始まる・続く、指数や単位の残り、ほかの記号）は読めない変形。M11 の 2 回目の確認 S6：R1 の直しで、空白で分けて f64 にならない数を読めないとしたため、参照先の path にこの書き方の transform を持つふつうの textPath を断っていた）を 10¹² で断り（10¹² では区間 1 万・1,000 字で 9 ms）、marker の中の textPath は `Unsupported`。さらに、弧長の逆引き（kurbo の `inv_arclen`）は、精度 ÷ 区間の長さ が f64 の刻みに近づくと区間が縮まず、2⁻⁶³ を下回ると分ける回数の計算が折り返して、どちらも終わらない（検証のあとの確認 R1：参照先の path 自身の transform `scale(1e9) scale(1e9)`、許される祖先の拡大 10¹² と座標 10⁹、objectBoundingBox の mask、`1e9%` の入れ子の svg で、数百バイトの 1 字の SVG が止まった）ので、精度の比の上限 12 × 拡大の上限 × 参照先の座標の上限（usvg の精度は 0.5 ÷ max(拡大, 1)、区間の長さは制御点の折れ線 ≤ 6 × 座標の上限）を 10¹⁴ で断る。参照先の座標の上限は、usvg と同じく参照先の図形自身の transform と transform-origin（平行移動を含む。祖先の変形は使わない）で写したもの（1 より小さい拡大はそのまま使う。ただし usvg は合成の行列が f32 で「無効」（行の長さ √(a²+c²)・√(b²+d²) が 1.2 × 10⁻⁷ 以下か、値があふれる）な変形を変形なしに置き換えるので、拡大の上限の積が 1 未満で合成の行の長さが 10⁻⁶ 以下になりうる変形は、変形なしも含むよう拡大を 1 とみなす。S6 を直すときに見つけた穴：`scale(1e-8)` の参照先と祖先の拡大 10¹¹・座標 10⁹ が見積もりを通り、弧長の逆引きで止まった）：path は絶対の命令の数の最大と相対の命令の数の和の和の 4 倍（弧・S の反射）、T が続けば (2 × 続く数 + 1) 倍（制御点が反射で遠くなる）、rect・circle・ellipse・line は長さの単位つきの和、polyline・polygon は数の最大の 2 倍、em・ex の長さは無限大。M4 Max の release で、比が 7×10¹⁶（尖った曲線）〜10¹⁸（なめらかな曲線）を超えると止まり、それより下では 1 曲線 0.5 ms 以下、比 10¹⁵ までなら区間 1,000・字 3,000 でも時間は比によらなかった（字の塊の 2 乗の手間で決まる）。上限 10¹⁴ は止まり始める比の 1/700 で、ふつうの textPath（Inkscape の mm と viewBox・Illustrator の matrix・地図の座標 10⁶ のラベル）は 10⁵〜10¹¹ ほど。textPath の参照先の区間の数は id ごとに 1 度だけ数える（多くの文字の節・多くの textPath が長い path を参照しても、見積もりの手間が積にならない）。ファズの対象 `decode_svg_text`（`testing::fuzz_svg_text`。字の少ない小さなフォント 2 つを渡して、文字のレイアウト・代わりのフォント・見積もりの道を通す。`decode_svg` はフォントを渡さないので通らない）。stable の変異（release、60 秒）で `svg_text` 約 2,826 万回・`svg` 約 1,239 万回回し、何も見つかっていない。
  描く前に、resvg の描き方に合わせて木（mask・clipPath・フィルタの feImage・文字の輪郭・pattern の中身も）をたどり、資源を見積もる（`check_render`。超えたら `Unsupported`）：(1) 根からの道筋ごとに同時に生きる一時の画像の合計（層は 1 枚がキャンバスの 25 倍まで広がり入れ子の層は同時に生きる、mask と clip は層ごとに画像とマスク、フィルタはプリミティブの結果をすべて持つ、pattern のタイルは外側のタイルを持ったまま中身を描く）を絶対の予算 `max_value_bytes` まで、(2) 描く手間（画素の数。図形の外接矩形と描く面の小さい方・層の合成・プリミティブごとの式：feMorphology は層の画素 × 半径の矩形、feTurbulence はオクターブの数、feConvolveMatrix は行列の大きさ、ぼかしは半径の分。図形は、たどるたび（pattern・clipPath・mask の中身は参照ごと）に、点 × 20 ＋ 辺が覆う行 × 4（区間ごとの制御点の外接矩形の幅 ＋ 高さ × 次数 × 変形の拡大と、次数 × 描く面の高さ の小さい方。path ごとに 1 度だけ数える）、線は塗りの 4 倍と区間ごとの継ぎ目 8 点・線の太さの行、破線は 1 本 4 点と 2 つの端の行（長さの上限 × 破線の数 ÷ 周期。tiny-skia は 1 本の path に 100 万本まで））を 2³² とキャンバスの 64 倍の大きい方まで（resvg の描画は途中で止められないので、手間で時間を抑える）、(3) 変形のあとの座標が ±10⁸ 画素を超える図形を断る。文字の輪郭を 2 度目から描き直す分（pattern・clipPath・mask の中の文字を参照ごとに）は、文字のレイアウトの残りの手間から引き、尽きたら `Limit`（1 度描く分は字ごとの手間に入っている）。いずれも検証とファズで見つけた形（pattern のタイルの 209 GB の確保・半径 10³⁷ の楕円の遅い入力など）。点の重み（検証のあとの確認 R3）：M10 からは点を 1 単位で数えていたので、文字を userSpaceOnUse の pattern に入れて 2,000 回参照すると 12 秒、15,000 個の大きな曲線（9 万点）の pattern を 4 万回参照すると 13 分かかるものを受け付けていた。重みは M4 Max の release の時間（1 単位 ≈ 1 ns）に合わせた：小さな字の輪郭は 1 点 21 ns（和文 32 ns）、タイルいっぱいの曲線は 1 行 2.8〜3.4 ns、細かい破線は 1 本 32 ns、太さ 8 の線は塗りの 5 倍。直したあと、上限の近くで受け付けた形は、文字の pattern（'a' 4,000 字 × 6 参照）82 ms・（'a' 400 字 × 150 参照）97 ms・（「龘」1,000 字 × 5 参照）92 ms、文字のない大きな曲線の pattern（1,200 参照）3.4 秒、細かい破線 70 本 0.97 秒で、文字は 1 件 100 ms ほど（決定 20）、文字のない形は 2³²（画素の層の合成で 8 秒ほど）の範囲に収まった。残る穴：多くの長い辺が交わる形は、tiny-skia が行ごとに辺を並べ替える手間が交わりの数で増えるので、行の数で数えるだけでは足りない（2048² に 30 万本の長い線で 3.2 秒、見積もりの約 5 倍）。
  **別のプロセスで読む**（M11 の決定 23。ADR-164。kura-core の `codec_ext::svg_worker`）：2 回目の確認で、見積もりをすり抜けて数十秒〜数時間止まる形が続けて見つかった（S1：回転を含む変形の下の太い線の外接矩形を木を作る段で求める、1.4 KB で 7 分。S2：stop の多いグラデーションの塗り、4,000 枚で外挿 3.6 時間。S3：同じ offset の stop の 2 乗、99 万個で 183 秒。S4：細かい破線の丸い端、37 本で 310 秒。S5：回転した pattern の塗り、102 秒。S7：オクターブの多い feTurbulence、18 秒）。木を作る段（usvg）も描く段（resvg）も中止を見ないので、アプリ（サムネイル・解析・アニメの帯。M12 のエディタも）は、このデコーダを同じ識別子・段・再現の約束のまま `SvgWorkerDecoder` に差し替え（`install`。アプリと kura-devtools の `EngineFactory::codecs`）、形も値も呼ぶたびにアプリ自身を `--kura-svg-worker` 付きで起動した子に頼む。子は Tauri を始めずに、同じ固定の段のデコーダ（`kura_mac::pinned_options()` のシステムのフォント）で読んで返事を書いて終わる。値・識別子・版・`DecodeStamp` はプロセスの中と同じ（手元の 1,439 件と確認のケースで、値のダイジェスト・記録・エラーの言葉がすべて一致）。やり取りは子の標準入力・標準出力の決まった形（版付き・ビルドの印付き・長さ付き・上限付き。親は返事を信用せず、見出しを `Limits` で確かめ、中身の長さが見出しから決まる長さと等しいことを確かめてから、届いた分だけタイルに組む。子は頼みを長さどおりに読んだら答え、標準入力の EOF を待たない）。時間の上限は形 2 秒・値 10 秒で、超えたら子をグループごと SIGKILL して `Limit`（「SVG を読むのに時間がかかりすぎます」。サムネイルは ImageIO へ回さずに種類の絵、解析は `too_large`）、見張りの中止でも止めて `Canceled`、子が落ちた・壊れた返事は `Decode`。**実行ファイルとビルド**（M11 の 3 回目の確認 W2）：働き手の実行ファイルは起動のたびに求める（アプリと kura-devtools は kura-mac の `current_executable`。`proc_pidpath` で、動いているアプリを移す・改名しても今の場所を返す）。頼みと返事の先頭に、やり取りの版とは別に親と子のビルドの印（`build_stamp`：SVG のデコーダの識別子・kura-core と kura-engine の版・ビルドの種類（debug・release）・CPU・フォントの有無と総称の書体。フォントの数は、数えるのにシステムのフォントを開いて読む必要があるので入れない）を入れ、子は違えば頼みの後ろを読まずに専用のエラーを返し、親は返事の印・版・ビルドの印で照らす。起動できない・実行ファイルが分からない・別のビルド（働き手でないものの返事を含む）は、アイテムのせいではない一時的な誤り（`EngineError::CodecMissing`。`tool` が「SVG を読む働き手（理由）」。`is_unavailable`。子の返事からは作れない）にし、サムネイル・解析のジョブはそのアイテムを失敗にせず（`pending`・`stale` のまま）、その回のほかの結果を書いてジョブを失敗にして止める（次にジョブが積まれたときにやり直す）。同じビルドの別の写しに置き換えたもの（版も設定も同じ）は見分けない。**起動の鍵**（W1）：子の起動はプロセスで 1 つの鍵（`tools::spawn`）の中で行う（macOS の std はパイプを作ってから FD_CLOEXEC を付けるまでに隙間があり、そこで同時に起動した兄弟の子がパイプの端を受け継ぐと、ふつうの SVG の子が EOF を待ち続けて時間切れになった。120 秒・16 スレッド・遅い子 6 本の stress で 6.8 万回に 2 回、直したあとは 120 秒を 2 回で 11.2 万回に 0 回。鍵の分、この stress で読めた回数は 2 分で 5.4〜5.8 万回に減ったが、SVG だけのサムネイルのジョブは 130 件/秒で変わらない）。親が落ちた・終わったときも、子は頼みに書いた時間の上限に 2 秒を足したら自分で終わる（止まる SVG を読み続ける子を残さない）。子の数は同時の呼び出しの数まで。子のメモリは予算の外なので、入力の大きさ（16 MiB。親が先に断る）と上の見積もりを前段として残し、予算の見積もり（`work_bytes`）はプロセスの中のデコーダのものを使う（親が受け取る値の 1 倍）。**確保の上限**（ADR-164 (4)）：子の確保の総量に、エンジンの見積もりから段ごとに決めた上限を付ける。子の確保器（kura-mac の `ReturningAllocator`。テストの働き手 `kura-svg-worker` は `System` を包んだもの）が目印（kura-core の `svg_worker::AllocMeter`）を持って `serve` に渡し、働き手だけが数え始める（アプリの本体では確保ごとに旗を 1 度読むだけ）。数えるのは数え始めてからの確保の総量（解放した分を引き、16 バイトに切り上げる。取り直しは写すあいだ新旧の両方）で、上限を超える確保は断る（ヌルを返す）。上限は、頼みを読んだら 64 MiB ＋ 入力 × 32（前段の見積もり）、木を作る前に 64 MiB ＋ 実体を展開した文字の数 × 32 ＋ use・marker を展開した要素の数 × 2 KiB、値を読むときは描く前にキャンバス × 3（tiny-skia の画像・乗算を戻した行・値のタイル）＋ 描く前の見積もりの一時の画像（層・mask・clip・フィルタ・pattern のタイルの同時に持ついちばん大きい合計）＋ 128 MiB ＋ 入力 × 64 を、それぞれそのときの量に足したもの。木を作る前と描く前の見積もりは、デコーダの見積もりの口（`svg::PlanHook`。`Plan::Tree`・`Plan::Render`。値には効かない）で受け取る。断られた子は Rust の確保の失敗で abort するか（親は標準エラーの「memory allocation of … failed」で見分ける）、`try_reserve` などのエラーを返し（子は断った確保があれば `Decode` に直す）、親はどちらも `Decode`（「SVG を読むのにメモリを使いすぎます」。`is_memory_limit`）にする（サムネイルはほかの `Decode` と同じく ImageIO へ回し、ImageIO の道は SVG のサムネイルを作れないので種類の絵になる。解析は読めないものとして失敗）。確保器が数えていない働き手（各 main で目印を渡し忘れたなど）は、上限なしでは読まずに内部の誤りを返す。係数は、受け付ける正しい重い形の実測（`kura-devtools bench svg-worker --memory`：プロセスの中で同じデコーダ・同じ目印で数える）に余裕を見た値：木は 1 要素 0.5〜1.1 KB（marker の複製・塗りの rect・objectBoundingBox のグラデーションを写す use）、前段は文字 1 バイトあたり最大 16（小さな rect が 15 MB）、線の外接矩形を求める一時の外形で 1 本の太い線が 1 バイトあたり 15、描く段の一時は丸い継ぎ目の太い線 1 本（80 万区間）で入力 1 バイトあたり 76・丸い端の点線は 1 個 1.5 KB。上限に対する割合は、手元の 1,439 件が前段 4%・木 3%・描く段 26%、16000² の値が描く段 95%（キャンバス 3 枚の 3,072 MB。上限は一時の画像 35 MB と下駄を足した 3,242 MB）、地図風の線 30 万区間が 10〜18%、要素の多い木（小さな rect・path 15 MB、34〜68 万要素）が前段 41〜43%・木 25〜30%、use の展開（100 図形のアイコン × 3,600。145 KB で木 250〜370 MB）が 31〜45%、丸い端の点線 10 万個の 1 本が描く段 90%、丸い継ぎ目の太い線 1 本が 70%。これを超える形（丸い端の点線は、キャンバスが小さければ 1 本 9 万個ほどまで）は、プロセスの中では読めても働き手では `Decode` になる。実測（M4 Max・release）：子の起動は 1 回 約 5 ms（アプリと同じ枠組みを読み込むため。フォントなしの `kura-svg-worker` は 2 ms）、ふつうの SVG 1 件は形と値で 2 回起動して p50 11.7 ms・p95 19.9 ms・最大 78 ms（プロセスの中は p50 0.23 ms・最大 63 ms）、サムネイルのジョブ（SVG だけの 1,084 件）は 127 件/秒（プロセスの中は 745 件/秒。見積もりの形を 1 件ずつ読む分が多い）。上限を決めた実測：受け付ける重い形の形は 16 MiB の地図風で 0.24 秒、文字の上限の近くで 0.05 秒、値は地図風の線 30 万区間で 4.5 秒（コアの数の 16 件を同時に描くと 6.8 秒）、16000×16000（値 1 GiB）を描いて送るまで 2.7 秒。通しの確かめ（`kura-devtools import`）で、S1〜S5・S7 の 12 件のうち 11 件がサムネイル・解析とも時間で失敗になり（S4 の 1 本は 8.8 秒で描けた）、ジョブは先へ進み（サムネイル 43 件 42 秒・解析 81 秒）、ジョブの途中でライブラリを閉じるのは 14〜83 ms だった（プロセスの中では S の形 1 件で数分〜数時間待たされた）。確保の上限で、確認の形のうち見積もりを通って確保が膨らむものは時間より先に止まる（子の最大の常駐。前は時間で止めるまで）：S1 の 40 区間が 0.2 秒・58 MB（前は 2.5 GB）、S1 の 2 区間が 0.14 秒・39 MB、S2 の 4,000 枚が 0.08 秒・458 MB（前は 4.9 GB）、S4 の破線（1 本 100 万本）が 0.04 秒・48 MB（前は 1 本が 8.8 秒・1.27 GB で描け、37 本は時間で止めた）。ほかの S2・S3・S5・S7 は時間の上限で止まる（子は 210 MB 以下）。残ること：子のメモリは予算の外で、同時に読む子の数だけ重なる（1 つの子は上の上限まで。見積もりを通る最大の入力・キャンバスでは数 GB になりうる）。

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
- **並列と速さ**：ブロックの計画（envelope。直列）→ 区間（2²¹ 標本ぶんのブロック）ごとに、aoTuV の窓の状態（直列）→ 窓・MDCT・FFT（`par_blocks`）→ ampmax・M5 の lowcomp・M3 の lastmdct（直列）→ 心理モデル・floor・residue・パケット（`par_blocks`。M3 の tempmdct を前から引き継ぐ続いた衝撃のブロックは 1 つの仕事の中で順に）。結果はスレッド数・区間の大きさによらない（照合とテストで確かめる）。中止は、PCM の確保の前、入力の写しの 2¹⁷ フレームごと、前の外挿（仕事ごとと 2¹⁷ 標本ごと）、envelope の探索の 2,048 歩ごと、計画 32 個ごと、ブロックの解析のブロックごと、シリアル番号のダイジェストの前後と 16,384 標本ごと、Ogg の多重化の 512 パケットごとに確かめる（48 kHz・ステレオで、最初から頼んだ中止は 1 ms 未満で戻り、確かめの間隔の最大は PCM の確保で 30 分なら約 35 ms、3 時間なら約 0.2 秒）。3 分の 44.1 kHz ステレオの q5 で 0.27〜0.30 秒、1 スレッドで 1.45〜1.56 秒（`vorbis_bench_three_minutes_stereo`、16 コア。測るたびの揺れ）。26 kHz 以上で衝撃の多い入力（クリックの列など）は、続いた衝撃のブロックが 1 つの仕事になるので段 3 がほぼ直列になる（10 分のステレオで約 9 秒。負荷のある機械で測った）。メモリは、書き出しのあいだ入力全体の f32 の写し（C の `vorbis_analysis_buffer`。値と同じ大きさ）を持ち、ピークは値の約 2.1〜2.8 倍（44.1 kHz ステレオ 3 分で値 64 MB に対して 168 MB）。段 10 のメモリの予算（§12.10）で、エンコーダごとの倍率として扱う（§9.9）。
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
- **ImageIO**（`imageio-dec/1`）：HEIC・HEIF・AVIF（`public.heics`・`public.avis` のシーケンスはフレームの列、ほかは主画像 1 枚）、TIFF・BMP、固定の段が断った CMYK の JPEG・ZIP の PSD。全体を 1 回だけ展開し（HEIC は帯ごとに切り抜いて読むと値が変わる）、展開できたかは取り出したデータが NULL でないことで確かめる（ImageIO は切れた・壊れた入力でも成功を返し、16 ビットの PSD は展開できないまま成功を返す）。色空間はそのまま、ImageIO の色空間の ICC を運ぶ（CMYK・Lab だけ sRGB に変換して sRGB の ICC）。浮動小数・HDR は `Unsupported`。EXIF は運ばない（ImageIO に生の EXIF を返す口がない。HEIF の Exif 項目を読むのは後の段）。形（`shape`）は開くだけで展開しない（`CGImageSourceCreateImageAtIndex` の `CGImage` の属性。画素の形式のアルファは `alphaInfo` から）ので、kura-core の解析はこれを HEIC・HEIF の見出しとして使い、アルファを持つものだけを読む（M11 の決定 15。12MP の HEIC で 1 件 0.6 ms ほど。入力は全体を読むが、うち 0.15 ms）。静止画の PNG を ImageIO で読んだ値は png クレートと画素でビット一致する（リポジトリのテストで 81 例：生成した 9 形式 × 4 つの大きさの 36 例と ICC・向きの 1 例、コーパスの 44 例（1/2/4 ビット・グレーと RGB と 16 ビットの tRNS・tRNS なしのパレット・インターレース・gAMA・cHRM・sRGB・sBIT・壊れた ICC・P3 の ICC）。有効な ICC は同じバイト列）。パレットの BMP・TIFF の 5 例は、パレットから作った値と照らす。16 ビットのプレマルチプライドは、vImage の戻しが kura の式と 1 違う組が、すべての (c, a)（0 ≤ c ≤ a ≤ 65535・a ≥ 1、約 21.5 億）のうち 7,357（約 29 万に 1。a ≥ 28,549 にだけ出る）あるので、戻しは Rust の式で行う（8 ビットは全組で同じ）。メモリは値の約 3 倍を見込む。
- **AudioToolbox の読み取り**（`audiotoolbox-dec/1`）：メモリから `AudioFileOpenWithCallbacks` と `ExtAudioFile` で f32 に。m4a・aac は `aac `・`aach`・`aacp`、ogg・opus は `opus`、wav・aiff は PCM と ima4・ulaw・alaw・MS ADPCM・IMA（DVI）ADPCM。priming を切った長さ。3 チャンネル以上は配置が kura の並び（WAV の既定のマスク）と同じときだけ読む。AudioToolbox の Ogg Vorbis は先頭を 128 フレーム余分に切るので使わない。HE-AAC は同じ Mac でも実行ごとに値が揺れる（最大 4.8×10⁻⁷）ので、アンカーが前提。docs の以前の「AVFoundation」は、AVAssetReader が HE-AAC の長さを揺らしたので AudioToolbox にした。
- **AAC の書き出し**（`aac-audiotoolbox-enc/1`）：`ExtAudioFile` で m4a をメモリに。LC だけ、1〜6 チャンネル（7・8 は AAC の配置と kura の並びが合わないので書かない）、8〜48 kHz、既定 256 kbps（CBR・ABR・VBR を選べる）。`ClientChannelLayout` を必ず指定する（しないと 5.1 の後ろの右が LFE に入って消える）。iTunSMPB（priming 2,112）を書き、1・2 チャンネルは段 6 の読み取り（固定の段）でちょうどの長さとずれ 0 に戻る（3〜6 チャンネルは固定の段が読まないので、AudioToolbox の読み取りでちょうどの長さに戻ることを確かめる）。Apple が書く時刻は 0 に書き換える。同じプロセスなら同じバイト列だが、約束しない（R2 は偽）。
- **ffmpeg の段**（kura-core。External。アンカー）：読み取りは前の段が断った音声だけ（`opus-ffmpeg-dec/1`・`ogg-ffmpeg-dec/1`（Opus・FLAC・Speex）・`wav-ffmpeg-dec/1`・`aiff-ffmpeg-dec/1`（圧縮の WAV・AIFF）・`m4a-ffmpeg-dec/1`・`aac-ffmpeg-dec/1`・`mp3-ffmpeg-dec/1`（layer 1・2））。入力は `file:<絶対パス>` で渡し（mp4 の moov が後ろにあるものを読むため）、分離器とデコーダを強制して `-format_whitelist`・`-codec_whitelist` で同じ名前だけを許し、`-protocol_whitelist` は、読み取りは入力 `file`・出力 `pipe`、MP3 の書き出しは入力 `pipe`（標準入力の f32le）・出力 `file`（一時ファイル）。読み取りの出力は `pcm_f32le` の WAV を標準出力へ。標準エラーの `[error]` 以上の行は失敗にする（ffmpeg は壊れた Ogg でも終了コード 0 で短く出す）。ffprobe で形を確かめてから 1 回デコードして数え、形は `Exact`。Opus は libopus を優先する。MP3 の書き出し（`mp3-ffmpeg-enc/1`）は一時ファイルに書く（パイプでは Xing・LAME のフレームが書かれず gapless の長さが失われる）。既定は CBR 192 kbps（その版で選べなければ 192 以下の最大：MPEG-2 は 160、MPEG-2.5 は 64）、VBR は品質 0〜9（既定 2）、ID3 は書かない（ユーザーの決定）、1・2 チャンネル。`-ar`・`-ch_layout` を明示し（ないと黙って変換される）、標本は NaN → 0・±∞ → ±1・±1 に切り詰めてから渡す（LAME は NaN で abort する）。起動は環境変数を消し（`PATH=/usr/bin:/bin`・`LC_ALL=C` だけ）、新しいプロセスグループで（`tools::run_binary`）、見張りが中止・無出力・時間切れ・出力の上限でグループに TERM、2 秒後に KILL（1 回だけ。回収したあとに pgid が使い回されても、ほかのプロセスに送らないように。`kill` を起動できなかったときだけ、次の刻みで送り直す）する。子が終わったあとも中止・時間切れを見続け、グループの外に出た子孫がパイプを持って残るときは、止めたあとならすぐ（その理由のエラー）、止めていなければ子が終わってから 2 秒で KILL、さらに 2 秒で（`Stopped::Lingering`）、パイプを読むスレッドを見捨てて戻る。パイプに触るのは見捨てられるスレッドだけ。一時ファイル（入力の写し・MP3 の出力）は作業フォルダに、プロセスで 1 つの通し番号の名前で、ないことを確かめて作る（`create_new`。同じ作業フォルダの組が 2 つあっても、ほかのプロセスの残りがあっても、切り詰めたり消したりしない）。引数は `crates/kura-core/goldens-ffmpeg-args/`（追記だけ）に凍結し、引数を変えたら版を上げる（External のバイト列と値は凍結しない）。実物のテストは ffmpeg があるときだけ回る（`KURA_FFMPEG_DIR`・`KURA_FFMPEG_REQUIRED=1`。CI の macOS の job は Homebrew の ffmpeg を入れて必ず回し、Linux の job は回さない）。ffmpeg は同じ名前のコメントを `;` でつなぎ、MP2・ADPCM の長さは ffmpeg が出したままになる（固定の段と扱いが違う）。
- **symphonia の Vorbis・AAC を再現できるように**（ユーザーの決定。ADR-153）：symphonia-core（IMDCT・FFT の表）・symphonia-codec-aac（窓・TNS・逆量子化とスケールファクターの表・pulse）・symphonia-codec-vorbis（窓・floor 0・`float32_unpack`）の写しを `third_party/` に置いて std の超越関数を `libm =0.2.16` に差し替え（f32 の関数は f64 にして libm の f64 の関数で求めて f32 に丸める）、symphonia-codec-vorbis と symphonia-format-ogg の `lookup1_values` を libvorbis と同じく整数で求めるようにした。識別子は `ogg-vorbis-symphonia-dec/3`・`m4a-aac-symphonia-dec/2`・`aac-adts-symphonia-dec/2`（公開前なので旧版は残さない）、`reproducible()` は真、デコーダのゴールデンに値を凍結した。aarch64 と x86_64 でビット単位で同じ値（壊れた入力で NaN が出るときも、NaN を 1 つのビット列にそろえる）。AAC の pulse と Vorbis の floor 0 の差し替えは、値のゴールデンで照らしていない（そのフィクスチャがない。ソースの走査だけが守る）。自作の Ogg Vorbis のコーデックのゴールデンの読み直しの行（`/2` で値を持たない 35 行）は書き換えず、`/3` で形だけを照らし、`/3` で読み直した値は新しい名前の行（`<名前>-dec3`）で凍結した。floor 0 の Vorbis は実ファイルで照らしていない。
- **テスト**：kura-mac（macOS の CI）で ImageIO と png のビット一致、AAC を段 6 の読み取りで読み直してちょうどの長さ・ずれ 0、3〜6 チャンネルの並び（段の順に任せても、形も読みも AudioToolbox が答える）、壊れた入力・上限が展開の前に効く・中止・8 スレッドの同時の呼び出し・ABI の照合。kura-engine で偽の backend の adapter、能力の表、E29（`tests/e29.rs`）とアンカー（`tests/anchor.rs`。実際の SVG・Vorbis・AAC の値とランダムな 200 値の往復、凍結した版 1 のアンカー、崩したアンカー、上限）、アンカーのコーデックのゴールデン（`anchor-kura-dec@1.jsonl`）。kura-core で ffmpeg の引数のゴールデンと、ffmpeg があるときの実物のテスト。ファズの対象 `decode_anchor`。
- **アンカーのゴールデン**：`anchor-kura-dec@1.jsonl` はその場の書き手で書いて読むので、値の作り方を守るだけ。決まったバイト列から読めることは、凍結したアンカー `fixtures/anchor-v1-*`（17 本：9 つの画素の形式・パレット・ICC と EXIF のある・ない・シート・16 ビットと 8 ビットのフレームの列・ステレオとモノラルと 6 チャンネルの WAV）が守る。kura-mac のフィクスチャ（`crates/kura-mac/fixtures/`）も追記だけの検査の対象。
- **後の段に回したもの**：HEIF の EXIF、RAW、HDR、ゲインマップ・プレマルチプライドの HEIF のアルファ（ImageIO が黙って捨てる）、アンカーの保存への結線（M12）、RF64（4 GiB を超える WAV）、ffmpeg のタグの表をエンジンの表と共有すること、Opus の書き出し。

### 9.9 段 10 のコーデックの作業領域

メモリの予算（§12.10）の見積もりのため、`Decoder::work_bytes(format, shape)` と `Encoder::work_bytes(format, shape, opts)` を足した。読む・書く間に、値のほかに使う作業領域（バイト）を形から返す。値は変えない。書き出し先に渡すバイト列は含めない（`CodecSet::encode_bytes` の `Vec` は、見積もりが値の 1 倍まで別に数える）。既定は値の 1 倍（展開した行・平面や、行を詰めた写しを 1 つ持つとみなす。分からないものは多めに見積もる側に倒す）。`CodecSet::decode_work_bytes` は、読む途中で次の段へ回りうるので、その形式を読むデコーダの最大を返す（`encode_work_bytes` は選ぶエンコーダのもの）。ファイル全体をメモリに読んで持つデコーダのために、`Decoder::file_work_bytes(format, shape, len)`（呼び手がファイルの長さ `len` を知っているときの見積もり。既定は長さを見ない `work_bytes`）と `CodecSet::decode_file_work_bytes`（その最大）も足した。見積もり（§12.10）は、`EstimateSpec` に読む入力のファイルの長さ（`lens`。`EstimateSpec::input_len(入力, 長さ)` で決める）があればこちらを使う。

| 読み書き | 見積もり（値のほか） | 段 10 の実測（M4 Max・release。`/usr/bin/time -l` の peak memory footprint から入力・出力の値を引いた量） |
|---|---|---|
| 既定 | 値の 1 倍 | PNG の読み（値 3 MiB）：読む段の量（値を含む）が 7.2 MiB で、見積もり 6.0 MiB を値によらない分（約 1.2 MiB）だけ上回る |
| JPEG の読み | 値の 3 倍（プログレッシブは全成分の係数を持ったまま行に書く。ベースラインは形からは見分けられないので同じ） | 24MP：ベースライン 70 MiB（1.0 倍）・プログレッシブ 208 MiB（3.0 倍）。ベースラインは約 2 倍の見積もり過大 |
| ImageIO の読み | 値の 2 倍（全体を 1 回で展開し、行を値に写す。§9.8 の「値の約 3 倍（値を含む）」） | — |
| PNG の書き | 1 フレームの 3 倍（行を詰めた写し ＋ png クレートがメモリに組む IDAT。`fast` は無圧縮で組み直すぶん 4 倍） | 4096² の乱数の Rgba8：`balanced` 131 MiB（2.0 倍）・`fast` 219 MiB（3.4 倍） |
| JPEG の書き | 画素あたり「行の写し（Gray8 は 1・Rgb8 は 3・ほかは 4）＋ 2 × 成分」バイト。プログレッシブは ＋ 3 × 成分 | 24MP の写真らしい合成・q90：ベースライン 77 MiB（1.1 倍。見積もり 3 倍）・プログレッシブ 282 MiB（4.1 倍。見積もり 6 倍） |
| Ogg Vorbis の書き | 値の 1.8 倍 ＋ 8 MiB（入力全体の f32 の写しと区間ごとの作業。8 MiB は短い音声で比例の分を超えた量から置いた） | 3 分の 44.1 kHz ステレオ・q5：92 MiB（1.5 倍） |
| アンカーの書き・読み直し | 値の 1 倍（読み直した値。1 枚の画像と音声は帯・塊ごとに流して書く）。APNG は `fast` の PNG の 1 フレームの 4 倍と比べて大きいほう | — |
| aseprite の読み | ファイル全体（`file_work_bytes` は長さ。長さを渡さない `work_bytes` は値の大きさとみなす）＋ 持っておく展開したセル・タイルセット（値の大きさまで）＋ RGBA のキャンバス 5 枚（合成のキャンバス・展開中のセル 1 つ・タイルマップとタイルセット・別に合成するグループの入れ子 2 段）。展開したセルは、後のフレームのリンクが指すものだけを、それを描く最後のフレームまで持つ（リンクのないセルは 1 度しか描かない）。合計は値の大きさ（と `max_value_bytes`）まで持ち、超えた分は持たずに使うたびに展開し直す（値は同じ） | 3 スレッド。読む段（値を含む）の数え上げの確保のピーク／footprint と、長さを渡した見積もり（値 ＋ 作業領域）、MiB：512²・16 層・1 フレーム 16.7／18.5（21.7）、同・4 フレーム・リンクあり 23.8／26.1（27.7）、1024²・32 層・1 フレーム 125.9／129.7（145.9）、256²・20 層・50 フレーム 243.3／243.6（256.6） |

- 操作の作業領域（`Op::work_bytes`。§7.1）の実測（同じ測り方）：2048² の乱数を 256 色・誤差拡散に減色 381 MiB（見積もり 512 MiB）、Lanczos3 4096² → 2048²（16 スレッド）20 MiB（見積もり 17 MiB）、`audio.gain`（3 分ステレオ）32 MiB（見積もり 30 MiB）。Lanczos3 と gain で見積もりが実測を 2〜3 MiB 下回るのは、プールのスレッドの立ち上げなど、操作によらない分と見ている（推測）。
  - 塗りつぶし（`paint.actions`。§7.1）は、1px の市松を中央から 8 連結で塗る最悪の形で測った（3 スレッド。入力は予算の外 `Source::Held`。描く段の数え上げの確保のピーク／peak memory footprint から描かない子を引いた量と、見積もりのピーク、MiB）：4096² の Gray8 100.8／76.9（116.8）、4096² の Rgba8 100.8／125.1（164.8）、8192² の Gray8 400.9／315.5（464.9）。ふつうの形（4096² の Gray8 を 4 連結で塗る 64px の市松）は footprint 6.2 MiB で、見積もり（116.8 MiB）が大きく出る（待ち行列の上限を確保しても、触れる頁が少ない）。
- 予算の見積もりは、段をまたいで残る分（持ち越し）を足して、§12.10 の表の通しで実測の 1.09〜1.43 倍になった（形からは見分けられないベースラインの JPEG の行は 1.97 倍）。
- 残したこと：JPEG のプログレッシブの読みの上限の照らし（§9.5：値の 3 倍で断る）は、実測のピークが値を含めて 4 倍だった。断る入力が変わるので段 10 では変えていない（版の扱いを含めて判断が要る）。
- 残したこと（aseprite）：値より大きいタイルセットと持ちきれないリンクのセルは、使うたびに展開し直すので時間がかかりうる（値は同じ。段 10 の確認で、値の 17 倍の 4 MiB のタイルセットを 3 層・60 フレームで使う作った形が 1 回 11 ms → 1.7 s。展開し直す分も 1 フレームの展開の上限 `max_value_bytes` に数える）。描く層を毎フレームたどる手間（層 × フレームの数）も残る（中止は効く）。長さを渡さないと、ファイルは値の大きさとみなす（縮まない画素の層が多いファイルでは足りない）。キャンバスより大きいセル・タイルセット、3 段より深い別に合成するグループの入れ子、ファイルの構造（セルの数に比例する）は見積もりの外。形を読む `shape()` もファイル全体を読むが、予算の確保の前なので予算の外。ファイル全体を読むほかのデコーダ（FLAC・MP3・AAC・ImageIO）は、まだ長さを使っていない（値からの倍率だけ）。

### 9.10 M11 の 3D モデル（glb）

3D の素材は値にしない（形式の表で `decodes_to` は空）。情報とサムネイルのために、自前の読み手と描画を `kura_engine::model` に置く（M11 の決定 9：SceneKit などの OS の描画に頼らない。OS 非依存・決定性の規則・ファズの仕組みに載せる）。

- **読み手**（`glb-kura-read/1`）：glb（glTF 2.0 のバイナリ）だけ。JSON の塊と、最初の BIN の塊。外部のファイル・data: の URI の buffer と画像・疎な accessor・点と線のプリミティブは読まずに飛ばす（`Scene::warnings`）。JSON は serde_json に渡す前に、生のバイト列で入れ子の深さ（64 段）・オブジェクトの数（`max_json_objects`。serde で作る構造体の数を抑える）・値の数（`max_json_values`。配列の要素とオブジェクトのメンバー。serde で作る並びの長さを抑える）を数えて断る。serde が組み立てる確保が JSON の大きさに比べて膨らまないように、拡張の名前の並びは 64 個まで（名前は頭の 128 バイト）、文字列（版・accessor の type・alphaMode は頭の 16 バイト、generator は 256 バイト）は頭だけ、uri・mimeType は文字列があったという印だけ、属性は読み手が引くキー（POSITION・NORMAL・COLOR_0・TEXCOORD_n）だけ、材質とプリミティブの extensions は読み手が見る名前（unlit・Draco・meshopt）があるかだけ、accessor の min・max は頭の 16 個だけを持つ（受け付ける型・同じキーは後ろが勝つ・比べる結果は前の型と同じ。M11 の検証 S3：配列の要素と文字列に上限がなく、32 MiB の JSON の `extensionsUsed` に 1 字の名前を並べると 650 MiB、空の名前で 632 MiB（数える確保器で 393〜686 MiB）を、予算を確保する前に確保していた）。組み立てる確保の見積もり `ModelInfo::json_heap`（オブジェクトの数 × 3 × いちばん大きい構造体 ＋ 1 KiB、値の数 × 24 バイト、JSON の長さの和。情報としては書き出さない）を `estimate_load_bytes` に足す。ノードの木は再帰せずにたどり、積むときに印を付けて同じノードを 2 度積まない（輪・共有の子・範囲の外の番号を断つ）。accessor は成分の型・正規化（KHR_mesh_quantization）・間隔・範囲を確かめてから、`try_reserve_exact` で確保して読む。
- **情報**（`read_info`。インスペクタの素材の欄）：JSON の塊だけから数える。頂点・三角形（既定のシーンのノードの実体ごと）・範囲（POSITION の min・max の 8 つの角をノードの変換で写した和）・メッシュ・材質・テクスチャ・画像・アニメ・スキン・ノードの数・作った道具・使う／必須の拡張・描けない理由（`unsupported`：必須の Draco・meshopt・basisu など知らない拡張の名前と `skin`）。描くのに支障のない必須の拡張は KHR_mesh_quantization・KHR_texture_transform・KHR_materials_unlit・EXT_texture_webp・KHR_lights_punctual・KHR_materials_variants と `KHR_materials_` で始まる見た目の拡張（基本色で描く）。
- **焼き込み**（`load_scene`）：ノードの変換（matrix か T·R·S。四元数は正規化）を頂点に掛け、法線は余因子の行列で写す（裏返す変換は行列式で向きを戻す）。三角形・帯（奇数番めの向きを戻す）・扇。範囲の外の索引を含む三角形は捨てる。材質は基本色（線形）・基本色のテクスチャ（埋め込みの PNG・JPEG・WebP を固定の段のデコーダで読み、16 ビットは丸めて 8 ビットに。長辺が `max_texture_dim` を超えるあいだ、アルファで重みを付けた 2×2 の平均で縮める）・サンプラー（最近傍・繰り返し・端で止める・鏡）・KHR_texture_transform・頂点の色・alphaMode（OPAQUE・MASK・BLEND）・両面・unlit・放射。
- **描画**（`render`、`model-kura-render/1`）：正射影のカメラ（3/4 の眺め：右へ 30 度・上へ 20 度。正面の眺めもある）で、描く三角形の範囲を長辺 `size`（余白は長辺の 1/32）に収める（もう一辺は縦横の比）。走査は左上の規則、深さは手前（大きい値）を残す z バッファ。陰影は Lambert（環境 0.35 ＋ 向きのある光 0.65。光は左上の手前から）で、裏から見える面は見る側へ向ける。片面の材質の裏の面は描かない。BLEND は不透明と MASK のあとに、奥から手前へ（重心の深さ、同じなら番号の順）重ね、深さは書かない。超標本化（1 辺 1〜4）の標本を乗算済みで平均し、ストレートに戻して sRGB の 8 ビットにする（sRGB と線形の表は `math::pow` で作る）。背景は透明。値は Rgba8 の画像。出力の 16 行の帯ごとに `par_blocks` で並べ、帯の中は三角形の番号の順に描くので、どの標本も帯の分け方・スレッドの数によらない。描く手間（三角形の囲む矩形の標本の数の和）は、描く前に標本の数の `max_overdraw`（64）倍で断る。
- **サムネイル**（`thumbnail`）：情報で描けるかを確かめ、焼き込み、3/4 の眺めで描く（長辺 512 以下は 3×3、それより大きければ 2×2 の超標本化）。描けないものは `Unsupported`（表示の側は種類のアイコン）。
- **上限**（`ModelLimits`、既定）：ファイル 512 MiB・JSON 32 MiB・JSON のオブジェクト 2¹⁷・JSON の値 2²¹（ノード 2¹⁶ に 1 つずつ行列と名前と子を付けても収まる）・ノード 2¹⁶・プリミティブの実体 2¹⁶（同じメッシュを指すノードが多いと積になるので、情報を数えるときも超えたら断る）・頂点と三角形 2²²（実体ごとに数える）・テクスチャ 32 枚・テクスチャ 1 枚の画素 2²⁴・持つテクスチャの長辺 1,024・出力の長辺 4,096・手間 64 倍。見積もりは `ModelInfo::estimate_load_bytes`（焼き込み。glb のバイト列は含めない）と `Scene::estimate_render`（`shared` と帯ごとの `per_task`）。
- **決定性**：超越関数は `crate::math`（3/4 の眺めの向き・テクスチャの回転・sRGB の表）、浮動小数の min・max は `math`、並列は `par_blocks` だけ。aarch64 と x86_64（Rosetta）で同じダイジェストになることを確かめた。
- **テスト**：`tests/model.rs`（最小の glb の情報と三角形の画素・ノードの変換・輪と範囲の外の番号・テクスチャの最近傍・MASK と BLEND・片面の裏・見本の決定性・描けない印・上限・中止・見積もり、ChaCha8 の 1 万件の壊れた入力：でたらめ・バイトの変異・JSON の数の変異）、ゴールデン `goldens-model/model-kura-render@1.jsonl`（見本 `model::sample_glbs` × 2 つの眺めの、描いた値のダイジェストと情報。追記だけ）、ファズの対象 `model_glb`（`testing::fuzz_model`。情報と焼き込みの食い違い、描いた画像の形も見る）。cargo-fuzz（release ＋デバッグの検査 ＋ ASan）で 15 分・約 285 万回、stable の変異で 90 秒・約 906 万回回し、何も見つかっていない。
- **JSON の確保**（M11 の検証 S3。数える確保器、release）：上限の値の数を超える形（32 MiB の JSON に名前・数・属性・extensions を並べたもの）は serde に渡す前に 5〜20 ms で断る。読めるもののうち確保の山がいちばん大きかったのは、空の材質 13 万個（JSON 0.4 MiB）の 25〜35 MiB と、ノード 6 万（行列・名前・子つき。JSON 4.7 MiB）の 34〜40 MiB で、どれも見積もり `json_heap` 以下（`tests/model_json_heap.rs`。数える確保器のテストのバイナリで、形ごとに山 ≤ 見積もり、山 ≤ 64 MiB）。直したあと stable の変異の `model_glb`（種に名前・属性・extensions・extras の glb を足した）を release で 60 秒・約 806 万回回し、何も見つかっていない。読む時間も値の数に比例させる（属性の `TEXCOORD_n` は n → accessor の `BTreeMap`。M11 の確かめ R2：S3 の直しで `Vec` にして受けるたびに線形に探したので、違う n を並べると 2 乗になり、値の上限の内側の約 140 万個（JSON 28 MiB）で 1 回 249 秒、サムネイルのジョブは plan と描画で 2 回読むので約 8 分止まっていた。今は 162 ms、32 万個で 35 ms。違う n を 6 MiB 並べた確保の山は 6.0 MiB で見積もりの内側。`gltf::tests::many_distinct_texcoords_read_in_linear_time`・`tests/model_json_heap.rs`）。
- **実測**（M4 Max・release）：Khronos の Duck（2,399 頂点・4,212 三角形・テクスチャ 1 枚）は焼き込み 1.7 ms・512×3 の描画 8 ms・1600×2 の描画 25 ms。合成の格子（100 万頂点・200 万三角形・法線つき）は焼き込み 25 ms・描画 50 ms（512×3）／53 ms（1600×2）、焼き込みと 1600×2 の描画のピークは 223 MiB（glb の 46 MiB を含む。場面は 54 MiB）。

## 10. 描画の道具

```rust
pub struct PaintParams { pub actions: ActionList }  // 追記だけ。JSON では PaintAction の配列（段 9 までは Vec<PaintAction>）
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
- **塗りつぶし**：起点の色と各チャンネルの差がすべて許容差以下（16 ビットは 257 倍）の画素のうち、起点から 4 連結か 8 連結でたどれるもの（`contiguous` が偽なら画像全体の同じ色）。連結成分として定義するので、たどる順番に左右されない（E22）。Indexed8 は番号で比べ、許容差は 0。起点が画像の外なら何もしない。起点の座標は ±2^31 まで（読み込みで断る）で、トリムしたフレームの位置を引いてあふれる場合も「外」として扱う。実装は行を 64 ビットの語にそろえたビットの表で、同じ色の続き（区間）ごとに広げる（4096² の全面で 7 ms）。区間の待ち行列は、まだ塗れる画素がある範囲だけを積み、「画素 2 つに 1 要素 ＋ 行の数の 2 倍 ＋ 65,536」（`paint::fill::queue_cap`）までにする（市松模様の 8 連結を中央から塗ると山は画素の約 0.3。4×4 の周期の模様をすべて試した最悪。超えたら落ちずに `Limit`）。段 10 から、待ち行列は描く前に上限ちょうどを確保する（伸ばさない。作業領域の申告と同じ式。§7.1）。
- **色の引数**：Indexed8 は番号（置き換えだけ）、ほかは RGBA8。グレーの形式は灰色だけ、置き換えでアルファのない形式は不透明な色だけ。形式が分かる推論の段階で確かめる（パレットの番号の範囲は実行で確かめる）。
- **フレーム**：`frame` はフレームの番号（画像は 0）。座標はキャンバスの座標で、トリムしたフレームではフレームの外は描かない。
- **差分の適用**：引数が追記だけなので、前の出力に新しい動作だけを当てられる（`Op::apply_appended`）。全体を当て直した結果とビット単位で一致する（E21。`paint.actions` の例のすべての切れ目で確かめる）。段 10 から実行器（`render_cached` と、`render_area`・`render_view` の全体で描くノード）が使う（条件は §12.4）。
- **動作の列**（`paint::list::ActionList`。段 10）：動作を 1 つずつ `Arc` で持ち、64 個ずつの切れ目も `Arc` で持つ（最後の切れ目のほかは 64 個ちょうど）。追記した版は前の版の切れ目を共有するので、追記は O(足した分 ＋ 切れ目の数) で、前の動作（点の列）を写さない。前の版への追記かどうか（`appended_since`）は、共有している切れ目・動作を `Arc` の同一性で飛ばし、共有していないところだけ中身を比べる（追記で作った版どうしなら O(切れ目の数 ＋ 64)。別々に作った列は全部を比べ、答えは同じ）。JSON の書き方・読み方・誤りの文言は `Vec<PaintAction>` と同じ（文書の形は変わらない）。追記の読み込み（`parse_appended`）・推論（`infer_appended`）・変わった範囲（`param_change`）もこれで足した分だけになる（§15 の「点の多いペン」）。
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
- **結果のキー**（M11 で決めた。ADR-158）：元のファイルの中身（台帳 `blobs` の行。値のダイジェストではない）× ID × 版 × 正規の引数。kura-core の表 `analysis_results` は `UNIQUE(blob_id, analysis)` で、版と正規の引数は列に持ち、計画の要求と照らす（docs/data-model.md §3・§4）。元の中身で引くので、同じ中身の結果（計画の要求の ID・版・正規の引数と、読み取りの記録 `kura.read`）がそろっていれば読まずに使える。M12 で中身が変わると別の台帳の行になり、自然に解き直しになる。
  - 読んだときの記録（デコーダの識別子・再現の約束・値のダイジェスト・環境）は、事実の行（`media_facts`）に残す。
  - 再現を約束しないデコーダ（Platform・External・resvg）の結果も「その環境で読んだ事実」として持ち、OS の更新では解き直さない。解き直すのは、形式ごとの段の順のデコーダの識別子が変わったとき（settings の `analysis.decoders`。今の組にないデコーダで読んだもの・並びが変わった形式で失敗していたもの・読まずに形式から書いたもの）と、計画の指紋（`analysis.plan`：計画・投影・ドット絵の規則・読み取りの記録の版と、種類ごとの ID・版・既定の引数、形式ごとの扱い（JPEG は形式から、HEIC・HEIF は見出しで決める。決定 15））が変わったときだけ。デコーダの識別子が変わって戻したものは、同じ中身の結果がそろっていても使い回さずに読み直す（結果を使い回すのは、それを読んだデコーダ（事実の `decoder`）が今の組でその形式を読むもののどれかのときだけ）。投影（事実の列の作り方）だけが変わったときは、読み直さずに結果の表から作り直す（作り直しの印 `analysis.reproject` があるときだけ）。

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
- 実行器は解いた結果を `Solver` で受け渡す（既定の `Solve` はその場で解く。テストは記録・偽の値の差し替えに使う。E26）。段 10 のノードのキャッシュはこの口を使わず、`Engine::render_cached` の中で（入力の指紋, ID, 版, 正規の引数）の鍵（§12.2）で値と同じ表から引き、当たれば解かない（Solver の trait は変えていない。`render_with_solver` はキャッシュを使わない）。正規化の目標だけを変えると、2 回目からは計測を解き直さない（`analyses_are_reused_when_only_params_change`）。操作が使うのはメモリ上で解いた結果だけで、DB に置いた結果は使わない。

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
- **ドット絵かどうか**の判定は解析に入れない。M11 で kura-core の `analysis::facts::pixel_art_v1`（純粋な関数。版 `PIXEL_RULE` = 1）が、倍率（pixel_scale）・見える色の数（colors の `visible`）・半透明の画素の数（alpha の `partial`）と大きさから決める：svg・jpeg はいいえ、aseprite ははい。ほかは「拡大：可逆で、両方の軸が 2≤k≤64、元の大きさ（各辺 / k）が 4px 以上」か「等倍：見える色が 64 色以下・半透明の画素なし・長辺 512 以下」ならはい、どちらでもなければいいえ、材料が足りなければ決めない（NULL）。JPEG と、見出しでアルファがないと分かった HEIC・HEIF は読まずに形式から「いいえ」（アルファのある HEIC・HEIF は読んでほかの画像と同じ規則で決める。決定 15）。規則・定数を変えたら `PIXEL_RULE` を上げる（読み直さずに結果の表から事実を作り直す）。表示の実効値は「利用者の設定 ?? 自動の判定 ?? 偽」、検索は「利用者の設定 ?? 自動の判定」で、どちらもないものは検索の肯定・否定のどちらにも入れない（docs/data-model.md §4、docs/search-query.md §4）。
- フォント・3D の情報（書体名・字数、頂点数・大きさ）は kura-engine の解析（値を受ける）ではない。フォントは kura-mac の CoreText（`kura_probe` の種類 3）、3D は kura-engine の `model::read_info`（glb の JSON の塊だけ。§9.10）で読む。
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

段 10 で、ノードのキャッシュ・差分の計画・範囲・変わった範囲・プレビューの段・エディタの表示の経路・メモリの予算・予約の部品・セッションを入れた（ADR-154）。詳しい約束は各ソースの冒頭（`crates/kura-engine/src/render/*.rs`・`session/*.rs`・`budget.rs`・`monitor.rs`、`crates/kura-core/src/edit/runtime.rs`）に書き、ここは要点だけ。

### 12.1 口の一覧

```rust
impl Engine {   // 段 8 までにあるもの
    pub fn load(&self, raw: &[u8]) -> Result<Graph, EngineError>;               // 読み込みとスキーマの移行
    pub fn compile(&self, g: &Graph, input_shapes: &BTreeMap<InputId, Shape>) -> Result<Plan, Issues>; // 版の解決・引数の移行と検証・形の推論・解析の申告・資源の見積もり
    pub fn render(&self, plan: &Plan, inputs: &BTreeMap<InputId, InputValue>, mon: &dyn Monitor) -> Result<Rendered, EngineError>;
    pub fn render_with_solver(&self, plan: &Plan, inputs: &BTreeMap<InputId, InputValue>, mon: &dyn Monitor, solver: &dyn Solver) -> Result<Rendered, EngineError>;   // 申告した解析を解く口（§11.2）
    pub fn analyze<A: Analysis>(&self, v: &Value, p: &A::Params, mon: &dyn Monitor) -> Result<A::Output, EngineError>;   // §11.1（analyze_dyn・analyze_many も）
}
impl Engine {   // 段 10 で足したもの
    pub fn fingerprints(&self, plan: &Plan, inputs: &BTreeMap<InputId, FingerprintedInput>) -> Result<Fingerprints, EngineError>;   // §12.2
    pub fn render_cached(&self, plan: &Plan, inputs: &BTreeMap<InputId, FingerprintedInput>, cache: &mut NodeCache, opts: &RenderOptions, mon: &dyn Monitor) -> Result<CachedRender, EngineError>;   // §12.3
    pub fn compile_snapshot(&self, snap: &Snapshot, input_shapes: &BTreeMap<InputId, Shape>, prev: Option<&Plan>) -> Result<Plan, Issues>;   // §12.4
    pub fn render_area(&self, plan: &Plan, inputs: &BTreeMap<InputId, FingerprintedInput>, cache: &mut NodeCache, req: &AreaRequest, base: Option<&Base<'_>>, mon: &dyn Monitor) -> Result<AreaRender, EngineError>;   // §12.5
    pub fn render_view(&self, plan: &Plan, inputs: &BTreeMap<InputId, FingerprintedInput>, cache: &mut NodeCache, req: &ViewRequest, prev: Option<&View>, mon: &dyn Monitor) -> Result<ViewRender, EngineError>;   // §12.8（エディタの表示の経路）
    pub fn spoit(&self, plan: &Plan, inputs: &BTreeMap<InputId, FingerprintedInput>, cache: &mut NodeCache, at: &SpoitAt, mon: &dyn Monitor) -> Result<Option<Sample>, EngineError>;   // §12.5（E23）
    pub fn delta(&self, prev: &Plan, prev_inputs: &BTreeMap<InputId, FingerprintedInput>, next: &Plan, next_inputs: &BTreeMap<InputId, FingerprintedInput>) -> Result<Delta, EngineError>;   // §12.6
    pub fn plan_level(&self, plan: &Plan, level: u8) -> Result<LevelPlan, EngineError>;   // §12.7（choose_level は関数）
    pub fn estimate(&self, plan: &Plan, spec: &EstimateSpec<'_>, cache: Option<CacheProbe<'_>>) -> Result<MemoryEstimate, EngineError>;   // §12.10
    pub fn admit(&self, plan: &Plan, spec: &EstimateSpec<'_>, budget: &Arc<MemoryBudget>, class: Class, mon: &dyn Monitor) -> Result<Admission, EngineError>;
    pub fn admit_cached(&self, plan: &Plan, spec: &EstimateSpec<'_>, inputs: &BTreeMap<InputId, FingerprintedInput>, cache: &mut NodeCache, held: &mut CacheLease) -> Result<MemoryEstimate, EngineError>;
}
// CacheLease::open(budget, cache, want)・settle(cache)。書き出しは今は CodecSet::encode_bytes（§9.1。EncodedStamp を返す）。
```

- `render`・`render_with_solver` は値も型も段 8 のままで、キャッシュなしの正（保存・一括・作り直しの照合）として残す。エディタの道は、表示が `render_view`（§12.8。中で `delta`・`render_area`・`refine` を使う）、等倍の全体の値（照合のダイジェストなど）が `render_cached`、スポイトが `spoit`。どの道も値は `render` と同じ（E1・E4。表示の値は表示する範囲のタイルで）。
- `render` は使い終えた中間の値を、最後の使い手（出力が使う値は落とさない）のあとで落とす。進捗（`Monitor::progress`）は、始めに (0, 合計) を出し、ノードごとに推論の手間（`Inferred::cost`）の累積を出す。
- `Plan` は `Clone` で、ノードを `Arc` のまま共有する（版の違う計画・段の計画）。公開の口は `output_shapes`・`node_shape`・`node_needs`・`cost`・`node_key`・`level`・`released_after`・`node_ids`・`shares_node`・`node_inferred`。計画は作ったエンジンに縛られる（§12.3）。

### 12.2 指紋

- 入力の指紋は値のダイジェスト（§8.6）。読むときに必ず取っている `DecodeStamp.value_digest` を使い回す（`FingerprintedInput::decoded`。取り直さない）。テスト・合成の値は `Value::digest`（`FingerprintedInput::new`）。`InputValue` は変えていない。
- 有効なノードの指紋：blake3("kura-fp-node-v1" ‖ ノードの鍵（`op::node_key`。§7.1）‖ 規則 u32 ‖ 派生した種 u64 ‖ 段 u8 ‖ 入力の数 u32 ‖ 入力の指紋（`PortRef` の順））。派生した種は操作に渡す種（`determinism::seed(ノードの種, 主の入力の元, 操作 ID)`。`render` と同じ式なので元のハッシュも入る）。段はノードの段（§12.7。継ぎ目までのノードは 0）。ノード ID は入れない（レシピの継ぎ足しでずれる）。
- 外したノードの指紋は、入力 0 の指紋そのまま。
- 量子化した出力の指紋：blake3("kura-fp-out-v1" ‖ 出力のノードの指紋 ‖ 量子化の版 u32 ‖ ビット u32 ‖ ディザ u8 ‖ 種 u64 ‖ 元のハッシュ)（ディザの乱数が元を直接使うので元を足す）。
- 申告した解析の鍵：blake3("kura-fp-analysis-v1" ‖ 入力の指紋 ‖ 解析の ID（長さ付き）‖ 版 u32 ‖ 正規の引数（長さ付き））。
- 派生の指紋：blake3("kura-fp-derived-v1" ‖ 用途（長さ付き）‖ 親の指紋 ‖ 引数（長さ付き））。プレビューの段の継ぎ目の縮小は `derive("level", 継ぎ目の指紋, [段 u8, 規則 u32])`（§12.7）。
- 指紋はメモリの中だけで使い、保存しない。版をまたいで保つ約束もない（種は `Node.seed` に別に持つ）。`Engine::fingerprints` が、ノードと出力の指紋を描かずに返す。
- 確かめ（`tests/cache.rs` の `fingerprint_components`）：種・元・入力の値・有効か・引数・実装の版・入力の順・規則・段・鍵のどれでも変わり、ノード ID の付け替え・継ぎ足し・JSON の空白とキーの順では変わらない。セッションの鍵と `compile` の鍵が同じ（`session_keys_equal_compile_keys`）。

### 12.3 ノードのキャッシュ

- `render::cache::NodeCache`（エディタのセッションごとに 1 つ。上限はバイト。0 なら何も覚えない）を `render_cached` に渡す。結果は `CachedRender { outputs（値・指紋・ダイジェスト）, warnings, stats, changed }`。値と警告は `render` と同じ。
- 覚えるのは、等倍で全体を描いた値（由来 `Origin::Exact`）と、その警告（操作の警告と写しの警告を分けて）だけ。エラー・中止・部分の値は入れない（中止までに描き終えた前段の完全な値は残る）。無効なタイルのある値も入れない。当たり・差分の適用・解析・スポイトに使うのは `Exact` だけで、当たったら警告を同じ順で再生する。
  - 由来は `Exact`・`Patched`（前の値に変わった範囲だけを描き直して継ぎはぎしたもの）・`Partial`（範囲だけを描いたもの）・`Scaled`（段の計画を範囲で描いたもの）。`Exact` のほかは配信にだけ使い、キャッシュに入れない。
  - 段の計画のノードの値は、指紋の段で等倍と分かれるので `Exact` として入る（等倍の指紋に当たることはない）。
- 申告した解析も、同じ表に（入力の指紋, ID, 版, 正規の引数）の鍵で入れ、当たれば解かない（§11.2）。大きさは解析の `estimate().bytes` で数える。量子化した出力も指紋で引く。
- 出力のダイジェストは `RenderOptions.digests` のときだけ取り、出力の指紋ごとに覚える（出力の項目があるときだけ。入力をそのまま返す出力は毎回取る）。
- 追い出しは使った順の番号（tick。時刻を使わない）の古い順で、表は `BTreeMap`。共有を除いた大きさが上限を超える値は入れない（`stats.refused`）。入れたものがいちばん新しいので、古い固定していないものから追い出す。
- 数え方：共有したタイル・チャンネルは、実体の番地（`Arc`。`Value::chunks`）ごとに 1 度だけ数える。番地は、表がその実体を持っている間だけ数え、項目を消すのと同じ手で数を減らす（解放した番地の使い回しで数を誤らない）。同じ値を 100 回入れても 1 つぶん、筆は触れたタイルのぶんだけ増える。入力は自動では数えない：呼び手が `register_external` で数えに入れる（写しを持ち、追い出さない。それと共有する項目は数え直さない）。外すときは、1 つずつなら `unregister_external(&値)`（同じ実体（番地の列）の外の値のうち最後に登録したものを外して写しを手放し、数えなくなったバイト数を返す。ほかの外の値・項目と共有する分は残る。登録していなければ None）、すべてなら `clear_externals`。外した値の写しは手放し、その分は数えない（外した値は項目にならない。ほかの外の値・項目と共有する分は残り、項目の分はほかの項目と同じく追い出せる。追い出しは外の値を見ない）。
- 固定：描く前は「当たる予定のもの」と focus（`RenderOptions.focus`。編集中のノード）の入力・出力を、描いたあと（エラー・中止のときも）は focus の入力・出力だけを固定する。固定したものは追い出さないので、引数を動かしても前段を描き直さない。
- エンジンへの縛り：`Engine` ごとに通し番号を払い出し、キャッシュと計画を作ったエンジンに縛る。別のエンジンのキャッシュ・計画は `Internal`（登録表・上限の違う値を混ぜない）。`clear` は縛りも解く。`render` は計画を縛らない。
- `verify_hits`（テストとファズ）：当たっても描き直してダイジェストと警告を照らし、違えば `Internal`（指紋の抜けを見つける。`verify_hits_detects_a_wrong_entry`）。
- 前回の出力：ノードごとの前回の描画（`LastRun`：実装・型付きの引数・鍵・入力の指紋・規則・派生した種・段・出力の指紋・警告。差分の適用に使う。`render_cached` と、`render_area` の当たったノード・全体で描いたノードで更新する。範囲だけを描いたノードは更新しない）と、前回の `render_cached` の計画・入力の（指紋, 元）・出力の値（変わった範囲に使う。容量に数えず、上限 0 でも持つ）を持つ。
- 当たったノードより前にある外れたノードは、値が要らなくても描き直す（警告を再生するため。強い上限で上流が先に追い出されると無駄が出る。改善の余地）。
- 実測（M4 Max・release。`tests/cache.rs` の `bench_cache`。4096² の RGBA8 に反転 20 個の鎖）：キャッシュなし（ダイジェスト込み）65〜70 ms、冷えたキャッシュ 35〜41 ms（ダイジェストあり 67〜69 ms）、温まったキャッシュ 0.009 ms、10 番目を変えて 11 個を描き直す 24.8 ms、指紋だけ 0.004 ms。

### 12.4 差分の計画と差分の適用

- `Engine::compile_snapshot(snap, input_shapes, prev)`：セッションのスナップショット（§12.12）から計画を作る（描かない）。`prev` は前に作った計画で、同じエンジンの段 0 のものだけを使う。ノードごとに次の順で試す。
  1. 鍵・入力・入力の形が前と同じ：前の計画のノードを `Arc` のまま共有する（形が前と同じになったところで、後ろの推論は打ち切られる）。
  2. 鍵が同じで入力の形だけが違う：読んだ引数を使い回し、推論だけし直す。
  3. 追記（前の計画の元のスナップショットとの違いが `NodeChange::Appended`）：足した要素だけを `Op::parse_appended` で読む。外していない・持ち上げていない・入力の形が前と同じなら、推論も `Op::infer_appended` で足した分だけ（使えなければ全体で推論する）。
  4. ほか：`compile` と同じ道（resolve・parse・infer）。
- どの道でも、計画（各ノードの形・写し方・手間・申告・鍵、出力の形、最後の使い手、指摘）は `compile(&snap.to_graph(), …)` と同じ（`compile_snapshot_equals_compile`）。
- 追記の見分けは、前の計画が持つ元のスナップショット（`compile_snapshot` で作った計画だけが持つ）との `Snapshot::diff`（`Arc` の同一性）で行う。`compile` で作った計画を渡すと、共有と推論のやり直しは効くが、追記の速い道は効かない。移行したノード（実装の版が文書の版と違う）は全体で読む。セッションの鍵は、この登録表の `append_field` と食い違わないときだけ使い、食い違えば求め直す。
- 差分の適用（`render_cached` の外れの道。`render_area`・`render_view` の全体で描くノードも同じ道。§12.5）：次のすべてが成り立つとき、前回の出力に `Op::apply_appended` を当て、`Exact` としてキャッシュに入れる（E21 で、全体を当て直したものとビット単位で同じ）。
  - 持ち上げない・先に解く解析がない・メタデータの写しが恒等（`MetaAccess::Map` で `space`・`time` が `Identity`）。
  - 前回の描画（`LastRun`）と、実装（ID・版）・規則・派生した種・段・入力の指紋が同じで、鍵が違う。
  - 前回の出力（`Exact`）がキャッシュにある（引いても使った順は変えない。使えなかったときに追い出しの順を変えないため）。
- 警告は [前回の操作の警告 ＋ 足した分の操作の警告 ＋ 写しの警告] で組み直す（全体を当て直したときの順と同じ。`appended_warnings_follow_the_full_order`）。`verify_hits` なら全体で描き直して照らす。描いた数は `CacheStats::appended`。
- 描画のノードの引数を変える手・置き換えでは、引数を全体で読み直す（`compile` と同じ時間）。
- 呼び手（M12）は、版ごとに計画を残すのがよい：5 万本の筆の文書の取り消しで、最新の計画だけを渡すと描画のノードを全体で読み直して 235／258 ms（p50／p95）、取り消した先の版の計画を渡すと 0.07／0.13 ms（`tests/bench_append.rs`）。
- 実測（M4 Max・release・3 スレッド。`tests/bench_append.rs`）：4096² に筆 1 本（500 点）を足す差分の計画は 0.02〜0.04 ms、セッションの手は 0.01〜0.03 ms で、前の筆の本数（0・999・5 万本）によらない。5 万本の文書を最初に開いたときは、計画 255〜270 ms・描画 980 ms。

### 12.5 範囲と範囲の描画

- 範囲の型（`render::region`）：`Area`（`Empty`・`All`・`Pixels`（フレームの番号ごとの矩形の集合 `Rects`。64 個を超えたら外接矩形に畳む。外接矩形の幅・高さは u32 で頭打ちになるので、畳んだ矩形が元の矩形をすべて覆わなければ（遠く離れた矩形の並び）フレーム全体にする）・`Samples`（音声のフレームの区間 `Spans`。64 個を超えたら 1 つに畳む））。どれも保守的（大きめでよく、小さすぎてはいけない）。座標系は §5.2。タイルの集合は `Tiles`（1 辺を持つ格子）と、フレームごとの `FrameTiles`。トリムしたフレームの列はフレーム単位の全体で扱う（段 10）。
- `Reach`（`Op::reach` の申告。出力が入力 0 のどこに依るか）：`Global`（既定）・`Same`（同じ位置の画素）・`Affine`（平行移動・反転・90° の回転。逆写しは m⁻¹。入力の外に写る出力の画素はどこにも依らない）・`Resample`（最近傍の番号、平滑な重みの範囲）・`SameTime`・`Segments`（トリムの区間のずらし）・`Rate`（多相 FIR の窓）。`backward`（出力の範囲 → 描くのに要る入力 0 の範囲）と `forward`（入力 0 の変わった範囲 → 出力の変わりうる範囲）を持つ。2 つ目からの入力には、いつも全体で依る。
- `Engine::render_area(plan, inputs, cache, &AreaRequest { output, area }, base, mon) -> AreaRender { output, value: PartialValue, warnings, stats }`（エディタの表示の部品。表示の経路 §12.8 とスポイトが使う）：
  1. 呼び手の範囲を、出力のキャンバス（画像・フレームの列は [0, 幅) × [0, 高さ) とフレームの数。大きさの決まっていない軸は上限で切る。音声は長さ。フレーム全体と `All` はそのまま）に切ってから使う（遠い座標の足し算があふれず、畳みで覆いが欠けない。`render_view` の表示する範囲・スポイトの位置も同じ道を通る）。後ろから、各ノードの要る範囲を集める。範囲の描画を持つノード（有効で、`REGION` が真、`reach` が `Global` でなく、申告した解析がない）は、要る範囲を値のタイルに広げて `Reach::backward` で入力 0 へ写す。持たないノードは入力を全体で要る。外したノードは入力 0 へそのまま渡す。キャッシュに `Exact` の値があるノードで止める。
  2. 前から描く。当たりは値と警告を再生する。範囲を持つノードは、要るタイルだけを `apply_region` で描く（ほかは無効なタイル）。持たないノード（と、継ぎはぎをせず、すべてのタイルが要り、入力がどれも全体の値の範囲を持つノード）は全体を描く：入力がどれも等倍で全体を描いた値なら、`render_cached` の外れの道と同じ（継ぎ目の縮小の差分（§12.7）・差分の適用（§12.4）・申告した解析のキャッシュ・由来 `Exact` でキャッシュに入れる。cache.rs の `draw_miss`）で描き、当たったノードと合わせて描画を前回（`LastRun`）として覚える（次の差分の適用の前の出力になる）。そうでなければ（入力が範囲の描画で作った値）`apply` で素で描き、入れない。
  3. 継ぎはぎ：`Base { value: 前の版の出力, changed: 変わりうる範囲（§12.6） }` を渡すと、出力のノードは「変わりうる範囲」と「前の値の無効なタイル」だけを描き、ほかは前の値のタイルを共有する（由来 `Patched`）。外したノードを通り抜けた先の、範囲を持つノードにだけ当てる。前の値の形が違うときと、共有できるタイルが 0 のとき（形式・パレットが違いうる）は使わない。それでも継ぎはぎの描画が、前の値の大きさ・形式・パレットが今と違うことで断ったら（範囲の描画の部品の内部の誤り。変わりうる範囲が小さすぎた印）、前の値なしで 1 回描き直す（遅いだけで正しい。数は `AreaStats::rebased`。前の値なしでも同じ誤りなら、そのとき返す）。
- 範囲の描画が無効なタイル（申告した依存の外）を読んだら、その描画を捨てて、全体で描き直す（遅いだけで正しい。数は `AreaStats::fallbacks`）。テストは `fallbacks` と `rebased` が 0 であることを、ファズは `fallbacks` が 0 であることを確かめる。
- 量子化した出力（音声）は、`render`・`render_cached` と同じく量子化を当てる：出力のノードを等倍で全体を描けた（由来 `Exact`）ときは量子化の指紋（§12.2）でキャッシュを引き、なければ量子化して `Exact` で入れる（`render_cached` と同じ部品 `quantized_output`）。そうでなければ量子化だけを当てる。入力をそのまま出す出力も同じ。`exact()` で取り出す値は `render` の出力と同じ（`tests/region.rs` の `quantized_outputs_match_render`）。
- 範囲の値（`PartialValue`）はダイジェストを取れず、キャッシュに入らず、保存・一括・解析・スポイトに使わない（全体の値が要るときは `exact()` で、由来が `Exact` のものだけを取り出す）。範囲の描画は、全体の描画が失敗する入力でも通ることがある（範囲の外の中身で決まる誤り）ので、表示にだけ使う。警告は描いたノードと当たったノードの分だけ（`render` の警告の部分列で、順も同じ）。全体の警告は `render`・`render_cached` が正。
- スポイト（`Engine::spoit`。E23）：等倍の計画の出力の 1 画素を、1×1 の範囲を描いて拾う（前の値は使わない）。画像の外・音声は None。段の計画からは拾わない（`Unsupported`）。
- 範囲が効かない鎖：範囲は、範囲の描画を持つ局所の操作だけの鎖で効く（大きなシートの一部を等倍で描く、など）。範囲の描画を持たない操作は出力を全体で描き、入力も全体で要るので、それより前のノードも全体を描く。
  - 全体に依る操作（`reach` が `Global`。範囲の描画でも、変わった範囲の推論でも全体）：`image.trim_alpha`（出力の大きさが中身で決まる。後ろからの絞り込みもしない）・`sprite.split_grid`・`sprite.pack_grid`（範囲は後の段）・Indexed8 への `image.convert`（全フレームの色の表に依る）・塗りつぶしを含む `paint.actions`・`audio.normalize`（全体の計測に依る）。
  - `paint.actions` は、塗りつぶしがなくても範囲の描画を持たない（変わった範囲は筆の区切りの外接矩形の和で絞れるが（§7.1）、描くのは全体。追記は差分の適用が速い道で、`render_area`・`render_view` でも効く）。後ろに範囲を持つノードが続けば、それは変わったタイルだけを描く（§12.8）。
  - 音声の区間の継ぎはぎは作らない（平面を写す手間が残るため）。音声は変わった区間の推論だけ。
- 要る範囲をノードごとに値のタイルの格子へ広げるので、鎖が長いと描くタイルが増える（24MP の局所の鎖で 1 タイルを頼んで、全ノードで 47 枚）。内部のノードの前の版の部分の値（`Partial`）は使い回さず、キャッシュの `Exact` から描き直す（出力のタイルは、表示の経路（§12.8）が前回の表示から共有する）。
- 実測（M4 Max・release。`tests/bench_region.rs`。キャッシュは空）：24MP の局所の鎖（crop → flip → rotate90 → bilinear 3/4 → convert。出力 4425×2925）で、1 タイル 4.3 ms・1920×1080 の範囲 54.8 ms・全体（ダイジェスト込み）240 ms（3 スレッド。16 スレッドで 3.9・15.0・77 ms）。

### 12.6 変わった範囲

- 2 段で求める（E31）。
  1. 描く前の推論（`Engine::delta(前の計画, 前の入力, 今の計画, 今の入力) -> Delta { nodes, outputs }`）：前後の計画を NodeId で対応させ、ノードごとに変わりうる範囲を保守的に求める（前後を全体で描いた差は、必ずこの範囲に入る）。新しいノード、操作の ID・版、入力のつなぎ、種、持ち上げ、推論した形・入力の形が違えば全体。規則・段・エンジン・元（主の入力のハッシュ。すべての種に入る）が違えば全体。入力の値は指紋で比べる。外す・入れるは `effect` と入力 0 の差の和、引数の違いは `param_change`、上流の違いは `Reach::forward` で写す（引数と上流の両方が変われば和）。持ち上げる操作の範囲は全フレームに広げる。
     - 上流の全体（`All`）を入力の矩形に開いてから写す（位置を写す申告で、広げたキャンバスの縁などを外す）のは、入力 0 が Indexed8 以外の形のときだけ。前後で形が同じ（`node_delta` が確かめる）Indexed8 以外の値は画素（標本）とメタデータでしか違えないが、Indexed8 はパレットだけが違いうる（パレットは形に現れない）ので、上流の全体を全体のまま運ぶ（上流でパレットだけが変わると、canvas の縁の番号・出力のパレットも変わる）。メタデータの違いは画素の範囲に入れない（`meta_changed` で別に報告する）。
  2. 描いたあとの絞り込み（`delta::refine`）：手元に持っている前の出力の値と今の値のタイルを `Arc::ptr_eq` で比べ、共有したままのタイル（前の値で有効で、同じ実体）を外す（番地を覚えて比べない。前の値がなければ推論のまま）。無効にしたタイルも実体を残す（`Raster::invalidate`）ので、前の値で無効だったタイルは、同じ実体でも共有とみなさない（前回は配信していない）。形・形式・パレット・フレームの位置が違えば全体。
- `render_cached` は前回の計画・入力の指紋・出力の値をキャッシュに持ち、結果に `Changed { level, per_output, meta_changed }` を付ける。`OutputChange` は `All`・`Tiles`（フレームごとの変わったタイル。1 辺 `VIEW_TILE`、フレームの画像の座標）・`Samples`（音声の区間）。`meta_changed` は、メタデータ（格子・ピボット・スライス・ループ点など）か、フレームの表示時間・繰り返しが変わったら真。前回がなければ全体。
- 表示の経路（`render_view`。§12.8）は、前回の表示（`View`）を呼び手が持ち、同じ推論と絞り込みで変わったタイルを返す。
- フロントは、変わったタイルだけを `kura://` で取り直す（§12.13）。
- 実測（M4 Max・release。`tests/bench_region.rs`）：4096² の描画（筆 20 本）に筆 1 本を足すと、変わったタイルは 256 のうち 1〜4。推論と絞り込みは p95 0.013 ms。

### 12.7 プレビューの段

- 段 k（0〜3。`MAX_LEVEL`）の値は、等倍の大きさを各軸 ⌈n/2^k⌉ にした近似（24MP の段 2 は 1500×1000、段 3 は 750×500）。縮め方は `resize_smooth` の box（線形光・乗算済み）で、等倍から段ごとに直接作る（`downscale`）。
- ドット絵は常に等倍で描き、拡大は CSS の `image-rendering: pixelated` に任せる。
- 段の選び方（`choose_level(等倍の出力の形, 要る装置の画素, LevelHint)`。純粋な関数）：ドット絵の指定（`LevelHint.pixel_art`。M11 の判定か利用者の指定を呼び手が渡す。エンジンは中身を見て判定しない）・Indexed8・フレームの列・音声・大きさの決まっていない形は 0。ほかは、各軸が要る画素（CSS px × DPR × 拡大率）を下回らない最も粗い段。ドラッグ中は粗い段、止まったら細かい段か段 0 の範囲、といった方針は呼び手（M12）が持つ。
- 段の計画（`Engine::plan_level(等倍の計画, k) -> LevelPlan { plan, seam, fallback }`）：線形の鎖で、最後の有効な `FullOnly` のノード（継ぎ目。なければ入力）までを等倍のまま共有する（キャッシュの指紋も値も等倍の計画と同じ）。その出力を縮めるノード（`kura.preview.downscale`。登録表に入れず、ID は `SEAM_NODE` = u32::MAX）を挟み、後ろを、引数を段に縮めて（`Op::preview_params`）段の形で推論し直したノードで描く。種は元の source のまま。大きさが中身で決まる継ぎ目（trim の出力）も、上限の大きさで推論して段にする（縮めた矩形は ⌈n/2^k⌉ に収まる）。Skip の操作の推論が段で通らなければ、そのノードを外す。段 0 を頼んだら等倍の計画の写しを返す。段の計画からは作らない（`Unsupported`）。
- 操作ごとの扱い（`Op::preview`。ほかは既定の `FullOnly`）：

| 操作 | 扱い | 段の引数 |
|---|---|---|
| `image.crop` | Scaled | 始まりは床・終わりは天井で縮め、段の入力に収める |
| `image.flip`・`image.rotate90`・`palette.replace` | Scaled | そのまま |
| `image.canvas` | Scaled | 床・天井で縮める（入力の外に出てよいので収めない） |
| `image.resize` | Scaled | 等倍の出力の ⌈n/2^k⌉ を画素で指す |
| `image.convert` | Indexed8 へは FullOnly、ほかは Scaled | そのまま |
| `meta.set_grid` | Skip | 2^k で割り切れて段の入力に収まれば縮め、そうでなければ消す |
| `meta.set_pivot` | Skip | 偶数への丸めで縮め、段の入力に収める |

- 段 0 に落ちる 5 つの理由（`LevelFallback`。落ちたら等倍の計画の写しと理由を返す）：`NotLinear`（主の入力 1 つ・量子化しない出力 1 つ・各ノードの入力が直前の 1 つ、でない）・`NotImage`（継ぎ目か後ろの値が 1 枚の画像でない。フレームの列・音声）・`Indexed`（継ぎ目か後ろの値が Indexed8。パレットの番号の意味を等倍と同じに保つ）・`Analysis`（後ろのノードが解析を申告した）・`Infer`（段の形で推論が通らない Scaled の操作）。今の登録表の操作では、`Analysis`・`Infer` と「Skip の推論が通らなければ外す」道には届かない（crop・canvas・resize は引数を段の入力に収めて縮めるので推論が通り、`palette.replace` は Indexed8 を入力に取るので先に `Indexed` で落ち、解析を申告する Scaled の操作はなく、set_grid・set_pivot の段の引数はいつも推論を通る）。M11 で操作を足したときのために、試験用の操作で確かめる（`tests/preview.rs` の `fallback_reasons_of_level_nodes`）。
- 指紋：ノードの指紋の段は、そのノードの段（継ぎ目までは 0）。継ぎ目の縮小の値は派生の指紋（§12.2）で、ほかの項目と同じくキャッシュに入る（数え、使った順で追い出す。`register_external` は使わない：外の値にすると、継ぎ目が変わるたびに呼び手が古い縮小を外す（`unregister_external`）手間が要り、外し忘れると数えたまま残る。計画の中のノードなら、`render_cached`・`render_area`・`estimate`・`admit_cached`・`delta` をそのまま使える）。後ろのノードの鍵は等倍の文書の鍵のまま（段の引数は、鍵・段・入力の指紋から決まる）。
- 継ぎ目の縮小が外れたときは、等倍の値の変わったタイル（実体の同一性）から縮め直す段のタイルを `Reach::Resample` で求め、そのタイルだけを縮め直す（全体を縮め直したものとビット単位で同じ。数は `CacheStats::rescaled`）。後ろの引数だけを変えたときは縮め直さない。
- メタデータ：継ぎ目の縮小は格子・ピボット・スライスを倍率で段の座標に写す（割り切れない格子は消す）。Skip の操作（`meta.*`）は画素を描かずに、メタデータを段の座標で書く。段の警告は約束しない（等倍の計画・描画の警告が正。継ぎ目の縮小は警告を出さない）。
- 近似の約束：段の値は、保存・一括・スポイト・解析・ダイジェストに使わない。同じ文書・入力・段なら、スレッドの数によらず同じ値。版をまたいで保つ約束はしない（ゴールデンに入れず、縮め方を変えても操作の版は上げない）。
- 段の値を断る場所：`render`・`render_with_solver`（段の計画は `Unsupported`）、ダイジェストを頼む `render_cached`（`Unsupported`）、`spoit`（`Unsupported`）、段 1 からのノードが解析を申告していれば `Internal`（計画の段階で `Analysis` に落ちるので、ふつうは来ない）。`render_area` で描いた段の値は由来 `Scaled`。`render_cached` の段の出力は `Value` のまま返るので、保存の道に渡さないのは呼び手の約束（保存・一括は `render` を通す）。
- 近似の程度（`testgen` の Photo 640×424、段 1・2。等倍で描いて縮めたものとの差。設計の「最大の差 2」は、色を混ぜない操作でしか成り立たなかった）：

| 操作 | 最大の差 | PSNR |
|---|---|---|
| flip・rotate90・揃えた crop と canvas・Rgba8 と Rgb8 への変換・box | 0〜1 | — |
| Gray8 への変換 | 5〜6（輝度を符号化された値のまま取るため） | 54 dB |
| bilinear・Lanczos | 11〜19（重みが広く、縮める順で鋭い境の値が変わるため） | 42.5〜48.7 dB |

  E32 のテスト（`preview_approximates`）の上限は、最大の差 2・8・32 と PSNR 40 dB 以上。
- 実測（M4 Max・release。`tests/bench_preview.rs`。3／4／16 スレッドの p50）：24MP を段 2 に縮める継ぎ目は 62.0／47.0／16.7 ms（段 1 は 93.4／70.6／22.9、段 3 は 56.0／54.5／29.5 ms）で、継ぎ目が変わったときだけかかる。24MP に描いてから段 2 で描く鎖に筆 1 本（500 点・8px）を足すと、9.6／9.5／9.3 ms（compile 込み。毎回、変わったタイルだけを縮め直した）。

### 12.8 エディタの表示の経路

- `Engine::render_view(plan, inputs, cache, &ViewRequest { output, area, focus }, prev: Option<&View>, mon) -> ViewRender { view, changed, meta_changed, inferred, warnings, stats, area }`（`render/view.rs`）：エディタが 1 手ごとに呼ぶ表示の道。
  - `area` は表示する範囲（キャンバスの座標。全体は `Area::All`。`ViewRequest::whole`）、`focus` は編集中のノード（その入力と出力を固定する。`RenderOptions.focus` と同じ）。
  - `View` は今回の表示（出力・計画・入力の（指紋, 元）・表示の値。`Plan` とタイルの `Arc` を共有して持つ）で、次の `render_view` の `prev` に渡す。入力の値は持たない（推論は入力を指紋と元で比べるので、`render_cached` の前回と同じく（指紋, 元）だけを持つ。入力を差し替えて手放せば、古い値は前回の表示に残らない。`tests/view.rs` の `view_keeps_only_the_input_stamps`）。キャッシュの外で持ち、容量に数えない（`render_cached` の前回の出力と同じ扱い。タイルの多くはキャッシュの項目と実体を共有する）。表示の値から `&Value` は取れない（保存の道に渡さないため。由来 `Exact` のときだけ `exact()` で取れる）ので、ふつうの 1 手の表示（由来 `Patched`・`Partial`・`Scaled`）は `register_external` で数えることもできない（§12.10 の数えないもの）。`ViewRender` は、`changed`（`OutputChange`。前回がなければ `All`）・`meta_changed`（前回がなければ真）・`inferred`（推論した変わりうる範囲）・警告・`stats`（この描画の `CacheStats`）・`area`（`AreaStats`）を返す。
  - 描けなければ（誤り・中止）前回の表示はそのまま使える（キャッシュには描き終えたノードの値が残る）。別のエンジンの計画・キャッシュは `Internal`。
- 流れ：
  1. 推論：前回の表示の計画・入力から今の計画・入力への `Engine::delta`（§12.6）の、表示する出力の範囲。
  2. 描画：`render_area`（§12.5）を、範囲 = 表示する範囲、`Base { value: 前回の表示の値, changed: 推論 }` で呼ぶ。出力のノードは、表示する範囲のタイルのうち推論に掛かるものと前回の値で無効なものだけを描き、ほかは前回の値のタイルを共有する（継ぎはぎ。由来 `Patched`）。前回の値で有効なタイルのうち推論に掛からないものは、表示する範囲の外でも持ち越し、推論に掛かるものは無効にする。
     - 範囲を「変わった範囲」にしないのは、継ぎはぎが「要る範囲のうち、前の値で有効かつ推論の外のタイル」を共有する作りで、要る範囲を変わった範囲にすると共有するタイルが 0 になり、継ぎはぎされないため（`tests/delta.rs` と同じ使い方）。
  3. 報告：`refine(前回の表示の値, 今回の値, 推論 ∪ 前回は無効で今回は有効なタイル)`（§12.6）で、変わったタイル（`VIEW_TILE`）の一覧を作る。`meta_changed` はメタデータ・フレームの表示時間・繰り返しを比べる。
- 値の約束：表示する範囲のタイルは、`render` の同じタイルとビット単位で同じ（範囲の描画と継ぎはぎの約束。推論は保守的）。表示の値の由来は `Patched`・`Partial`・`Scaled`（段の計画）で、配信にだけ使う（継ぎはぎをせずに全体を描いたときだけ `Exact`）。保存・一括・解析・スポイト・ダイジェストは `render`・`render_cached`・`spoit` で。警告は `render_area` と同じ（描いたノードと当たったノードの分。全体の警告は `render_cached` が正）。
- 変わったタイルの一覧：表示する範囲の中で中身の変わったタイルを必ず含み、推論の範囲と「前回は無効で今回描いたタイル」（表示する範囲を広げた・動かした分）の中にある。描き直したタイルは新しい実体なので、中身が同じでも報告する（多めに出ることがある。中身で比べ直すことはしない）。前回の表示の値で無効だったタイルは、今回同じ実体（入力のタイルの通り抜け・キャッシュの当たり）で有効になっても報告する（無効にしたタイルも実体を残すので、実体だけでは共有とみなせない。§12.6 の絞り込み）。推論に掛かって無効にした、表示する範囲の外のタイルも報告に入る。形・形式・パレット・フレームの位置が違えば `All`。
- 配信の口（M12 が kura:// でタイル・メタデータを出すのに使う）：`View::value()` の `PartialValue` から、読むだけの口で取る。`format`（画素の形式）・`palette`（Indexed8 のパレット。`tile`・`read_rect` が返すパレットの番号を色にする）・`image_meta`（格子・ピボット・スライス・タグなど。キャンバスの座標）・`audio_meta`（ループ点・マーカーなど）・`frame_duration_ms(frame)`・`loop_count`、画素は `tile`・`read_rect`、有効なタイルは `valid`・`covers`。どれも保存の道に渡せる `&Value` は返さない。音声の平面は `exact()` で取る（音声は範囲の描画を持つ操作がなく、いつも等倍で全体を描くので由来 `Exact`）。確かめ：`tests/view.rs` の `view_values_expose_what_delivery_needs`（継ぎはぎ・範囲だけの表示でも `render` の値と同じものが取れる）。
- 前回の表示を使わない場合：前回が別の出力なら使わない（前回がないのと同じ。`changed` は `All`）。推論が誤りになる計画（申告が引数を読めないなど）は、推論を `All` にする。段・エンジン・元が前回と違えば、`delta` が `All` を返して共有するタイルがなくなり、表示する範囲を描き直す（形が違えば報告も `All`）。
- 範囲の描画を持たないノード（描画・塗りつぶし・`trim_alpha`・Indexed8 への `convert` など）は、`render_cached` の外れの道と同じに描く（キャッシュの当たり・差分の適用・申告した解析のキャッシュ・由来 `Exact` で入れる。§12.5）。筆を足すと、描画のノードは前の出力に足した分だけを描き（差分の適用）、後ろの範囲を持つノード（最近傍・canvas・convert など）は変わったタイルだけを描く。等倍の全体の値は作らない。
- 段の計画：計画は等倍のほか、プレビューの段の計画（`plan_level`。§12.7）でもよい。継ぎ目の縮小は範囲の描画を持つ（`Reach::Resample`）ので、局所の変更では要るタイルだけを縮める（その値はキャッシュに入れず、`CacheStats::rescaled` も増えない）。全体が要るときは、`render_cached` と同じ外れの道（等倍の値の変わったタイルだけを縮め直す。§12.7）を通る。段を替えると全体を描き、報告は `All`。
- 1 手の流れ（「エンジンの中」。§16）：
  1. 手を当てる（`EditSession::preview`・`commit`。§12.12）→ 前の計画を渡して `compile_snapshot`（§12.4）→ 写真は `choose_level` と `plan_level`（§12.7）。
  2. 確保（`admit_cached`。表示する計画（段の計画）で。待たない。§12.10）。確保だけはプールの外（プレビューの輪のスレッド）で行い、1 と 3 はプレビューのプールの中で行う（§12.11）。
  3. `render_view`。
  4. `settle` で確保を戻す。
- フロント（M12）は、表示する範囲（ビューポート。キャンバスの座標）を `ViewRequest.area` で渡し、`changed` のタイルだけを `kura://` で取り直す（§12.13）。前回の表示（`View`）は Rust の側で持つ（値を IPC で送らない）。
- `render_cached`（等倍の全体）は、照合のダイジェスト（NFR-16）の道として残る。ベンチ（`testing::bench::path::EditorPath`）は、NFR-11・12 を表示の経路（`show`・`render_preview`。表示する範囲は出力の全体）で測り、NFR-16 は同じエディタの経路で等倍の全体を描いたダイジェスト（`digests`）で照らす（§16.1）。
- テスト（`tests/view.rs`。E21・E31）：ランダムな文書・手の列（1 手、構造の手、2〜3 手をまとめたもの、入力の値だけの変更。§17 の E31）で表示する範囲もときどき動かし、キャッシュの上限 0・1 MiB・1 GiB で、有効なタイルが `render` と同じ・報告が前後の差を含む・全体での描き直しと前の値なしの描き直しが 0 回・警告が全体の描画の部分列（`view_equals_render_on_random_edit_sequences`。段 10 の検証で報告の抜けを見つけた種でも回す：`view_equals_render_on_found_seeds`）。Indexed8 の上流でパレットだけが変わり、後ろに canvas（広げる）・反転・最近傍が続いても、描けて値が `render` と同じで報告が全体（`view_handles_palette_changes_upstream`）。筆のあとに最近傍・canvas・convert が続く鎖と上流に塗りつぶしのある鎖で、筆は差分の適用で描かれ、後ろの範囲を持つノードは継ぎはぎになり、1・3・16 スレッドで報告も同じ。手を 2 つ当ててから描く（間の表示を飛ばす）手順も照らす（`view_redraws_only_changed_tiles_after_strokes`）。段 1・2 の計画で値が段の計画の全体の描画と同じで、段を替えると `All`（`view_at_levels_equals_the_level_render`）。表示する範囲を動かすと、新しく見えたタイルだけを描いて報告する（`view_follows_a_moving_viewport`）。
- 実測（§16.2）：筆のあとに最近傍 ×4 が続く文書（1024² → 4096²）の 8px の筆 1 本は、`render_cached` で等倍の全体を描くと p95 18〜19 ms、表示の経路で 1.9〜2.2 ms。描く最近傍のタイルは筆 1 本あたり平均 19 枚・最大 35 枚（paint の変わる範囲を 1 つの外接矩形にしていたときは 25・60 枚。§7.1）。
- 残したこと：報告するタイルは、描き直したタイルを中身によらず数えるので多め。`render_area` は内部のノードの `Partial` の値を使い回さない（§12.5）。

### 12.9 予約

- `monitor::Latest`：要求ごとに世代の番号（1 から増える）を払い出し（`issue(親)` → `Ticket`）、`supersede(g)` で g より古い世代の描画を止める（床。下がらない）。`Ticket` は `Monitor` で、世代が床より古くなるか、親（ジョブ・エディタを閉じるときの中止）が中止を頼めば止まり、進捗は親へ流す。時刻を使わない。
- 止められた描画は `Canceled` になり、キャッシュは汚れない（E24）。古い描画を止めるか、描き終えるのを待って最新を描くか（合流）の方針は、呼び手（M12 のエディタ）が持つ。

### 12.10 メモリの予算

- 1 つの予算（`budget::MemoryBudget`。バイトの重みのセマフォ）を、エディタ（対話。`Class::Interactive`）と一括（保存・一括加工のジョブ。`Class::Bulk`）で分け合う。macOS では大きな確保も失敗しない（1000 GiB の `try_reserve` が通る）ので、確保の失敗では予算を守れない。描く前の見積もりで確保し、確保した分（`Lease`）は落とすと返す（鍵が毒されていても返す）。
- 予算の既定は min(物理メモリ/4, 4 GiB)（`default_total`）。物理メモリは呼び手（kura-mac の `physical_memory`）が渡し、分からなければ 2 GiB。
- 振る舞い（ユーザーの決定：取り置き＋一括だけ待つ）：
  - 予算の全体を超える要求は、計画の段階（見積もり）で `Limit`。操作は 1 つも動かない（E25）。
  - 収まる要求なら、一括は描く前に確保を待つ（中止できる）。待つ順は来た順で、先に待っている一括（下の駐車したものを除く）を追い越さない（来た順と駐車は段 10 で決めた細部）。
  - エディタは待たない：今すぐ確保できなければ、キャッシュの固定していないものを古い順に追い出してもう一度、それでも足りなければ `Limit`。待っている一括を追い越してよい。
  - エディタを開いている間は min(1 GiB, 予算/2)（`default_reserved`）を取り置き（`set_reserved`）、一括は「予算 − max(取り置き, 対話の使用量)」までを使う。それを超える一括の要求は `Limit` にせず、収まるまで待つ（取り置きかエディタのキャッシュの確保のほうで超えるなら、エディタを閉じるまで）。取り置きは呼び手が決める。
  - 駐車：「予算 − max(取り置き, 持ち続ける対話)」を超える一括の要求（今の取り置きとエディタのキャッシュの確保のもとでは通りえないもの）は、待ちの列に残したまま先頭の判定から外し、後ろの収まる一括（エディタを開いたままの保存など）を塞がない（待つ確保 `acquire` も、待たない `try_acquire`・`try_grow_to` の「追い越さない」の判定も、駐車していない待ちだけを見る）。持ち続ける対話は手の合間も持つ対話の確保で、`CacheLease` が `settle`（開くときも）のたびに確保の全体を `Lease::set_standing` で申告する（入力と固定した値でキャッシュの上限を超えると、取り置きを超えうる）。描く間だけ伸ばす分（`admit_cached` のピーク）とプレビューの確保は入れない（すぐ返るので、それで駐車させると大きな一括が追い越され続ける。描く間に来た「予算 − 対話の使用量」を超える一括は、駐車せずに来た順で待ち、`settle` のあとに判定し直す）。取り置きか持ち続ける対話が下がる（エディタを閉じる）と駐車でなくなり、来た順の位置のまま先頭の判定に戻る（後から来た一括は、今すぐ収まってもそれを追い越さない）。駐車している間も中止できる。
  - `Usage`（全体・対話・一括・取り置き・持ち続ける対話 `standing`・待っている数（駐車したものを含む）・駐車している数 `parked`）は、ジョブの表示（予算を待っている理由。駐車はエディタを閉じるまで通らない）に使う。
- 数えるもの：デコードした元の値（一括は見積もりに入れ、エディタは `NodeCache::register_external` で数えて `CacheLease` が覆う）、キャッシュ（`CacheLease` が上限と今の大きさの大きいほうを覆う）、描く間の値と作業領域、書き出しの作業領域と書いたバイト列。
- 数えないもの：タイル・ブロック 1 つぶんの一時の領域、スレッドのスタック、計画・文書・セッションの履歴（履歴は `HistoryLimits` で別に抑える。予算には数えないが、NFR-17 の判定の実測には文書・履歴も入る：基準は実行の場を作るだけの子で、文書を開いたエディタの子との差を測る）。前回の表示（`View`。§12.8）と `render_cached` の前回の出力は、キャッシュの外で持ち、数えない（どちらも入力は（指紋, 元）だけで、入力の値は保たない。出力・表示の値のタイルの多くはキャッシュの項目と実体を共有する。表示の値からは由来 `Exact` のときのほか `&Value` を取れないので、`register_external` にも渡せない）。配信のために全体の値（`render_cached` の出力・`exact()` で取り出せる値）をキャッシュから追い出したあとも持つなら、M12 が `register_external` で数え、入れ替えるときに `unregister_external` で外す。
- NFR-07 とは別に数える（ユーザーの決定）：NFR-07 はライブラリの表示の分。編集の値とキャッシュは NFR-17 の予算で数え、エディタを閉じたら捨てる。
- M11 の使い手（kura-core。M11 の決定 2・8）：エディタのほかに、解析・サムネイル・試聴の PCM・アニメの帯が、ライブラリごとの同じ予算とプール（§12.11）を使う。
  - 解析のジョブとエンジンの道のサムネイルは一括（`Class::Bulk`）。どちらもジョブのスレッド（プールの外）で形を読んで見積もり、確保を待ってから一括のプールの中で読む・描く。ジョブが動いている間のメモリは NFR-07 の外として別に記録する（NFR-07 は表示の分）。
  - サムネイル（`media::engine_thumbs`）：同時の確保は 256 MiB まで（`INFLIGHT_CAP`）。見積もりの和がこれに収まるように小分けにし、小分けごとに確保する。1 件で超えるものは 1 件だけの小分けにする（予算の全体を超えれば確保で `Limit`）。値が 1 GiB（`VALUE_CAP`）を超えるもの・`Limit`・読めないものは、画像なら ImageIO の道へ回し、3D は種類の絵にする。
  - 解析（`analysis::job`）：1 件の見積もり（値の 2 倍 ＋ デコーダの作業領域）が min(予算/2, 1 GiB)（`ITEM_CAP`）を超えるものは、確保せずに失敗（`too_large`）にする。音声のデコーダは作業領域を値と同じに見積もるので音声は値の 3 倍になり、予算が 2 GiB 以上（物理メモリ 8 GB 以上）では 44.1 kHz ステレオで約 16.9 分・48 kHz ステレオで約 15.5 分を超える音声は解析しない（docs/requirements.md の「解析の長さの範囲」）。
  - 試聴の PCM（`/pcm`）とアニメの帯（`item_anim_get`）は対話（`Class::Interactive`）で、待たない（`try_acquire`）。利用者がその場で待つ短い表示の用意なのでジョブにしない。空いていなければ PCM は 503、帯は `Busy`。予算の全体を超える帯は `TooLarge`。読む・組むのはプレビューのプール。PCM は読んだ値を 1 件だけ覚え、その間は確保を持つ（60 秒使わなければ捨てる）。
- 確保器（M11。ADR-163）：アプリと kura-devtools は、1 MiB 以上の確保を `mmap` で取り `munmap` ですぐ OS へ返す `kura_mac::ReturningAllocator` を `#[global_allocator]` にした（libmalloc の large cache が解析・サムネイルのあとも空の領域を 445 MB 抱え、NFR-07 に戻らなかったため。`MallocLargeCache` は文書にない設定なので使わない）。下の段 10 の実測と見積もりの式（持ち越し）は libmalloc のときのもので、式は変えていない。
- 見積もり（`Engine::estimate`）：描かない純粋な関数。ヘッダから読んだ形で compile した計画で、読む前に呼べる。条件（`EstimateSpec`）は、入力の来し方（`Source::Held`（予算の外で数えている値。エディタの入力）・`Memory`・`Decode { codecs, format }`・`Decoder { decoder, format }`）、読む入力のファイルの長さ（`lens`。`EstimateSpec::input_len`。分かるものだけ）、書き出し（`Sink::Encode`・`Encoder`）、並列の数（`Work::total` に渡す）、ダイジェストを取るか。段ごとの量は、その段で生きている値と作業領域の最大。
  - 読む：読み終えた入力 ＋ 読んでいる入力の値 ＋ max(デコーダの作業領域（§9.9。ファイルの長さがあれば `Decoder::file_work_bytes`：ファイル全体を読んで持つ aseprite は長さを足す。なければ `Decoder::work_bytes`）, 読み取りの記録のダイジェストの帯)。
  - 描く：入力 ＋ 生きている途中の値（最後の使い手のあとで落ちる）＋ 今のノードの出力 ＋ 作業領域（`Op::work_bytes` の `Work::total(並列の数)`。持ち上げたノードはフレームの形で求める）＋ 申告した解析の結果（`estimate().bytes`）。量子化した値とダイジェストの帯（min(値, 4 MiB)）もここで数える。
  - 書く：入力 ＋ 出力 ＋ エンコーダの作業領域 ＋ 書いたバイト列（値の 1 倍までとみなす）。
  - 外したノードと、入力をそのまま返す出力は、元と同じ実体として数え直さない。キャッシュに当たるノードと量子化した出力は 0（描かない）。ほかは共有を見ずに新しい値として数える（多めに見積もる側）。
- ピーク ＝ max(読む, 描く ＋ 読む持ち越し, 書く ＋ 読む持ち越し ＋ 描く持ち越し)。読む持ち越しは読む段の作業領域（値を除く）の和、描く持ち越しは描く段の最大から描き終えて残る値を引いた分（書くときだけ数える）。macOS のアロケータ（large cache）は、返した大きな領域を手元に置き、ほかの大きさ・ほかのスレッドの確保には使い回さない（段の中では使い回す）ため。下の表で、段の最大だけで見積もると 1〜3・6 行目で実測が見積もりを 27〜54% 上回り、この式では実測の 1.09〜1.43 倍になった（形からは見分けられないベースラインの JPEG の 4 行目は 1.97 倍）。
- 照らし：
  - 一括（`Engine::admit`）：見積もりのピークが予算の全体を超えれば、何も確保せずに `Limit`。収まれば `acquire` で待つ（中止で `Canceled`。使用量は変えない）。返した `Admission` を描き終えるまで持つ。
  - エディタ（`Engine::admit_cached` と `CacheLease`）：開くとき `CacheLease::open(予算, キャッシュ, 望む上限)` でキャッシュの分を対話で確保する（空いていなければ、空いている分まで上限を下げる）。描くたびに、キャッシュの当たりを引いて見積もり、確保を「キャッシュの分 ＋ ピーク」まで待たずに伸ばす。伸ばせなければ、固定していないものを追い出して上限を下げ、見積もり直してもう一度（通ったら、確保を「下げた上限の分 ＋ ピーク」に合わせる）。追い出せるものがなくなっても足りなければ `Limit`（追い出した値は戻らないが、キャッシュの上限と確保は呼ぶ前のままに戻す。Limit のあとに `settle` を呼ばなくても、上限が下がったまま残らない。追い出したあとの見積もりが予算の全体を超えたときも同じ。最初の見積もりが予算の全体を超えるときは、何も変えずに `Limit`）。描いたあとは `settle` で伸ばした分を返し、予算が空いていれば上限を望む量に戻して、戻した確保を持ち続ける対話として申告する（上の駐車の条件）。
- 流れ：エディタは `register_external`（入力。替えるときは `unregister_external` で外す）→ `CacheLease::open` →（手ごとに）`admit_cached`（表示する計画で）→ `render_view`（§12.8）か `render_cached` → `settle`。一括は、ヘッダから compile → `admit`（プールの外。読むファイルの長さが分かれば `input_len` で渡す）→ 一括のプールで読み・描き・書き → 確保を落とす。kura-core の `EngineRuntime::admit_bulk`・`open_cache`・`admit_preview` が同じ流れで、並列の数はそれぞれのプールのスレッドの数。
- 判定の式（NFR-17。§16）：子プロセスの peak memory footprint − 基準（実行の場を作るだけの子。保存のケースはアプリのコーデックの組も作る子。文書・セッションの履歴は実測の側に入る）≤ 予算。macOS の CI の煙のテスト（kura-devtools の `mem_smoke`）は、実測 ≤ 見積もり × 1.25 ＋ 4 MiB（アロケータの断片・スレッドのスタック・タイル 1 枚ぶんの一時の領域の余裕）を確かめる。`mem_smoke` の基準は同じ子で、ファイルに触れる前（プール・エンジン・コーデックの組を作ったところ）まで回したもの（形を読むのにファイル全体を読む aseprite の分も読む段に入れる）。見積もりには読むファイルの長さを渡す。ケースは、プログレッシブの JPEG → 回転・Lanczos → PNG（`fast`）、PNG → Indexed8 → PNG、WAV → 音量 → Ogg Vorbis、層の多いアニメの aseprite（256²・8 層・4 フレーム・リンクあり）→ 反転 → APNG、1px の市松の 2048² の Gray8 の PNG → 8 連結の塗りつぶし → PNG。
- 段 10 の実測（M4 Max・release。peak memory footprint から基準を引いた量、MiB）：

| 通し（3 スレッド） | 実測 | 段の最大だけ | 今の式 |
|---|---|---|---|
| 24MP のプログレッシブ JPEG → 回転 → Lanczos 2000 → PNG（`fast`） | 406.3 | 274.7 | 449.8 |
| 同じ・16 スレッド | 423.8 | 274.7 | 463.0 |
| 同じ・読まずに合成の値から（16 スレッド） | 217.3 | 171.7 | 257.0 |
| 24MP のベースライン JPEG → Lanczos 3000 → JPEG | 185.1 | 274.7 | 364.5 |
| 1024² の PNG → Indexed8 → PNG | 103.4 | 132.0 | 139.0 |
| WAV 4 秒 → 音量 → Ogg Vorbis | 12.6 | 9.2 | 17.2 |
| WAV 60 秒 → 音量 → Ogg Vorbis | 94.4 | 117.1 | 135.2 |

  1 行目の通しを `MallocLargeCache=0` で起こすと 252 MiB で、段の最大の見積もりに収まった。
- 開く読み（M12 の段 4 で決めた）：エディタを開くときの元の読みも、読む段の見積もりを対話（`Class::Interactive`。待たない）で確保する（全体を超えれば `TooLarge`、空いていなければ `Busy`。読み終えたら手放し、入力は `register_external` で数える。§12.12）。
- 残したこと：エディタを開くときに読んだ作業領域の持ち越しは、読み終えたあとは予算の外（OS へ返す側は、M11 で 1 MiB 以上の確保をすぐ返す確保器をアプリに入れた（上の「確保器」）。持ち越しの式は変えていない）。JPEG のベースラインは、形だけでは見分けられないので約 2 倍に見積もる。前回の表示（`View`）と `render_cached` の前回の出力は数えていない（上の数えないもの）。aseprite の見積もりの外にあるもの（長さを渡さないとき・大きいセル・深いグループ・`shape()` の読み）と、まだ長さを使っていないデコーダは §9.9。

### 12.11 プール

- エンジンはスレッドプールを作らない（§8.2）。kura-core の `edit::EngineRuntime` が、エンジン 1 つ・プレビューと一括の 2 つのプール・予算を持ち、開いているライブラリに 1 つ置く（M11 の `edit::EngineServices`。解析・サムネイル・試聴の PCM・アニメの帯・M12 のエディタが同じ予算とプールを使う。最初に使うときに作り、ライブラリを開いている間は作り直さない。キャッシュは作ったエンジンに縛られるため。外部ツールの場所を変えたときに作り直すのはコーデックの組だけ。docs/architecture.md §15）。`Engine` には `preview(|e| …)`・`bulk(|e| …)` の閉包の中からだけ触れる（プールの `install` の中で描くので、グローバルのプールに漏れない）。閉包はプールの中から入れない（debug のビルドで止める）。
- 大きさの既定（`pool_sizes`）：プレビュー max(1, ⌊コア/4⌋)・一括 max(1, コア − プレビュー)。16 コアで 4・12、8 コアで 2・6、4 コアで 1・3（1 コアだけは 1・1）。設定（`RuntimeConfig`）で変えられる。スレッドの名前は `kura-edit-preview-N`・`kura-edit-bulk-N`。
- 分ける理由：一括を流している間も、プレビューのワーカーが空く（段 10 の調査：分けると p95 27.6 ms、相乗りすると 59 ms）。
- 予算の確保（待つことがある）は、閉包に入る前（プールの外。ジョブのスレッド・プレビューの輪のスレッド）で行う。ワーカーの中で待つと、ほかの描画のワーカーを塞ぐ（試作で 2.3〜3 倍遅い）。
- 値はプール・スレッドの数によらない（E1・NFR-16。kura-core の `two_pools_render_the_same_value_at_once`）。QoS（スレッドの優先度）は段 10 では試していない。

### 12.12 セッション

- `session::EditSession` は純粋な構造（OS・時刻・スレッドを使わず、描画・計画・キャッシュも持たない）。状態を変える関数は `&Engine` を受け取り、登録表（操作の版と追記だけの引数）と上限だけを読む。エディタの中の ⌘Z・⇧⌘Z はこれで実現する（ADR-141）。
- 履歴は文書のスナップショット（`Snapshot`：`Arc<Head>` と `Arc<[Arc<SNode>]>`。写すのは `Arc` 2 つ）の列で、逆操作は持たない。ノードは手のあいだで `Arc` のまま共有する。追記だけの配列の引数は `AppendLog`（64 個ずつの切れ目を `Arc` で持ち、連鎖のダイジェストは `op::append_chain`。§7.1）。`Snapshot` は Send＋Sync で、写して描くスレッドへ渡す。ノードの鍵は `Registry::doc_node_key` と同じ値。
- 手（`Edit`。9 種）：
  - `AddNode`（線形の列の `at` 番目に足す。ID は `next_id` から払い出し、種は指定がなければ ID の値（§4.1）。2 つ目からの入力は文書の入力か、`at` より前のノード）・`RemoveNode`・`MoveNode`（前後をつなぎ直す）。
  - `SetEnabled`・`SetParams`（引数をまとめて）・`SetParam`（項目 1 つ）・`AppendItems`（追記だけの配列に要素を足す）。
  - `SetOutput`（形式・量子化・展開。つなぐ先は変えない）・`Replace`（文書を丸ごと。今のスキーマの文書）。
- 構造の手（足す・消す・動かす）は線形の列だけで受ける：ノード i の入力 0 が 1 つ前のノード（最初のノードは文書の入力）で、出力 0 の from が最後のノード。線形でない文書への構造の手、範囲の外の位置、2 つ目からの入力の誤り、ほかのノードの 2 つ目からの入力に参照されているノードの削除は `Invalid`（`BadGraph`）。ノードの数の上限は `TooLarge`。
- 検証は変えたところだけで行う：形（`check_structure` と同じ規則）→ 値の深さ・大きさ → 文書の大きさ（`MAX_DOC_BYTES`。差分で求める）→ 読み直せること（E8。移行するノードだけ `resolve` に通す）。誤りの順は、文書を写して全体を確かめる参照の実装と同じ。断った手は、仮の値も含めて状態を変えない。
- まとめ方に時刻を使わない：
  - `CommitMode::Step`（1 手）・`CommitMode::Merge`（同じ引数を続けて変える手を 1 手にまとめる。鍵は `SetParam` が（ノード, 名前）、`SetParams` が（ノード）。まとめられない手は Step と同じ）・`seal`（まとめを閉じる）。
  - 仮の値（`preview`）は履歴に入らない（前の仮の値は捨てる）。`commit_preview`（番号を引き継ぐ）・`cancel_preview`。
  - ドラッグは preview を繰り返して離したときに Step、キーボードの矢印・数値の入力は Merge、blur・Enter で seal。
  - Merge で 1 つ前の手と同じ中身に戻ったら、その手を消してまとめを閉じる。土台になった手にはまとめない。何も変えない手は積まない（`noop`。16.0 と 16 は同じ）。
- `Rev`：新しい中身の状態ごとに振る番号。単調に増え、中身の違う状態に同じ番号を振らない。undo・redo は戻った先の手の番号を返す（同じ手には同じ番号）。`snapshot(rev)` は、履歴の手・今の仮の値・直前に捨てた仮の値 4 つまでを引ける（描いている途中の要求のため）。
- 上限（`HistoryLimits`）：1,000 手と、共有を除いた文書の 64 MiB の早いほうで、古い手から捨てる（カーソルは土台より前にならない）。バイトは「土台の文書の長さ ＋ 各手が 1 つ前の手と共有していない分」で、正規の JSON の長さで数える見積もり（決定的にするため、アロケータの値を使わない）。実メモリは、筆の追記で見積もりのほぼ 1 倍、引数の変更で約 10 倍、開いた文書は JSON の約 8 倍（serde_json の値。5 万本で 114 MB）。上限に実メモリの倍率を掛けるかは後で決める。
- 保存の印：`mark_saved`・`is_dirty`（開いた文書は保存したものとみなす）。M12 の段 4 で `mark_saved_at(rev)`（保存のジョブは積んだときの頭を描き、その間も編集を続けるので、ジョブが終わったときに積んだときの番号に印を付ける。番号は中身ごとに振るので、履歴から捨てた手の番号でもよい）・`clear_saved`（下書きから開いたとき：開いた文書はまだ保存していない）・`saved_rev` を足した。
- 手（`Edit`）と `CommitMode` は serde で JSON にできる（M12 の段 4。エディタとの通信）：`type` で種類を分け（`addNode`・`removeNode`・`moveNode`・`setEnabled`・`setParams`・`setParam`・`appendItems`・`setOutput`・`replace`）、項目は camelCase、`AddNode.seed` は 10 進の文字列（数でも読む。JS の数は 2^53 を超える整数を表せない）、知らない項目は断る。feature `specta` で TS の型になる（引数の JSON は名前の付いた `JsonValue`。specta の `serde_json::Value` の型は入れ子を展開して書き出せない）。
- 下書きの形式は作らない（M12）。頭の文書と土台の版から作り直す口（`open`・`open_json`。`open_json` は `Engine::load` を通して古いスキーマを移す）だけを持つ。作り直すと同じ文書・同じ鍵になる。
- **下書き**（M12 の段 2。kura-core の `edit::drafts`、表は docs/data-model.md §4「保存と版（M12）」）：エディタの頭の文書（内容アドレスの部品。§4.4）と、その土台（開いたときの今 `items.hash` と頭の版。保存の `Base`）だけを、アイテムごとに 1 つ持つ（`edit_drafts`）。アプリが落ちても、開き直すと同じ文書（同じ根）に戻せる（S17）。
  - 開く（`drafts::open`）と `fresh` を返す：土台が今と頭の版に等しく、ゴミ箱になく、土台を待っている保存のジョブ（`after_job`）が動いていない（待ち・実行中・一時停止でない）。偽なら、置いたあとに別の保存でアイテムが変わったので、その土台で保存すると `Conflict`（どうするかは利用者が選ぶ。UI は段 5）。
  - エディタが保存を積んだあとも編集を続けて置く下書きは、`after_job` にそのジョブを書く（土台はそのジョブの土台）。保存のジョブのトランザクション 2 が、保存した文書と同じ下書き（待っていないか、このジョブを待っていたもの）を消し、このジョブを待っていた下書きは土台を新しい頭に付け替える（`settle_after_save`。別名で保存は元のアイテムの下書きに同じ規則、土台は元のアイテムの頭のまま）。ジョブがもうコミットしていれば、置くときに付け替える。ほかの下書き（別の保存の前に置いたもの）は残り、開くと `fresh` が偽。
  - 捨てる（`drafts::discard`）・アイテムを消す（CASCADE）と行が消える。下書きを置き換える・捨てると前の文書の部品が孤立しうるので、GC の部品の印（settings の `gc.parts`）を置き、段 3 の GC が片付ける（ゴミ箱を空にする処理は、消したアイテムの分をその場で片付ける。§14「GC のジョブ」）。
  - エディタ（段 4）は、頭が変わるたび（確定・取り消し・やり直し）に、下書きのスレッド（`kura-edit-draft`。エディタごとに 1 つ）へ頭の文書のスナップショットと土台を頼む。頼みは最新の 1 つだけを持ち、前に置いてから 3 秒（`autosave::INTERVAL`）たってから置く（`to_graph`・部品に分けて DB に書くのはこのスレッドで、1 手の往復に載せない）。保存していない頭か保存のジョブが動いているときは置き（ジョブが動いていれば `after_job`）、保存した頭に戻ったら捨てる。保存を積むときにも頭を `after_job` 付きで頼む（保存した文書と同じなら保存のジョブが消す）。閉じるときは待っている頼みを置いてから止める（`autosave::tests::requests_are_thinned_and_the_last_one_is_written_on_close`：間隔の中の頼みは最新の 1 つだけを持ち、閉じるとそれを置く）。下書きから開いた文書は保存していないもの（`is_dirty` が真）。
- **エディタのセッション**（M12 の段 4。kura-core の `edit::editor`。IPC は docs/architecture.md §3、配信は §12.13）：開いているライブラリに 1 つ（`EngineServices` が持つ。M12 の決定）。`EditSession`・ノードのキャッシュ（`register_external` で入力を数え、`CacheLease` で確保）・版ごとの計画（rev → 計画の小さな LRU。8 つ。取り消しで描画のノードを読み直さない。§12.4）・前回の表示（`View`）・入力を持つ。
  - 開く（`editor::open`）：アイテムの頭の文書（下書きから開くなら下書きの文書と土台）と、元（アンカーがあればアンカー。使えなければ元を記録のデコーダで読み直して照らす。保存のジョブと同じ順）を読む。読む段の見積もり（`Engine::estimate` の読む段。デコーダの作業領域・ファイルの長さを含む）を対話で待たずに確保し、全体を超えれば `TooLarge`、空いていなければ `Busy`（§12.10 の「開く読み」の決定）。読み終えたら手放し、入力を `register_external` → 取り置き `set_reserved(default_reserved)` → `CacheLease::open`。前のエディタは閉じる。画像だけ（ゴミ箱のものは `Conflict`）。
  - 手（`editor::step`）：手をまとめて当て、描くのは最後に 1 回。仮の値・確定の手は、頭に当てた文書で計画を作れることを確かめてから仮の値にし（`EditSession::preview_if`。確定の手はそのあと `commit_preview`）、作れなければ断って状態を変えない（前の仮の値・その計画・表示もそのまま。それより前の手は残る。`rejected_steps_keep_the_previous_provisional`）。描画の手（`Paint`）は、末尾が有効な `paint.actions` ならそこへ追記、ほかなら末尾に足す 1 手（最初の 1 本も 1 手。M12 の決定）。段は Rust が `choose_level` で決め（要る装置の画素は等倍の大きさ × 1 画素あたりの装置の画素、操作中は 1 段粗く。ドット絵は DB の実効値）、確保（`admit_preview`）はプールの外、`render_view` はプレビューのプール。新しい表示の要求（手・`edit_view`）が来たら `Latest::supersede` で前の描画を止め（当てた手は残る）、止めた描画は `superseded` で返す（取って代わった要求が表示を返す）。描けなければ（予算・文書の誤り）表示だけを失敗にして、手は当たったまま返す（取り消せる）。予算が足りないとき、キャッシュの当たりを引いた見積もりが全体を超えれば `TooLarge`、ほかは `Busy`。
  - スポイト（`editor::sample`）：今の rev の等倍の計画で `spoit`（段の値からは拾わない。E23）。表示の要求ではないので、描いている途中の表示を止めない（`Latest` の世代を払い出さずに錠を待つ。止めると、止めた要求の代わりの表示がどこからも返らない。止まるのはエディタを閉じたときだけ。`sampling_does_not_supersede_a_render`）。
  - 保存（`editor::save`）：確定した頭を `to_graph` して保存のジョブへ（`start_save`）。出力の形式を変えるなら、先に `SetOutput` の手を確定する（取り消せる）。保存の間も編集を続けられ、同じアイテムの保存が待っている間は次の保存を断る（`Busy`。土台がずれて `Conflict` になるため）。待っているかは、エディタが積んだ保存に加えて DB の保存のジョブ（同じアイテムの待ち・実行中・一時停止の `edit`）で見る（`save::pending_save`。エディタを閉じて開き直しても効く。形式を変える手を確定する前に見て、`start_save` も部品を置くのと同じトランザクションで確かめ直す。`a_pending_save_blocks_the_next_save_even_after_reopening`）。ジョブが終わったことは次の要求（`edit_view` を含む）でジョブの行を見て知り、上書きは結果の版・中身で土台を更新して `mark_saved_at(積んだときの rev)`、別名で保存は印だけ（土台は元のアイテムの頭のまま）、版に戻したら開き直す（`needsReopen`。手・保存は `Conflict`）。保存が予算を待って駐車していれば、エディタのキャッシュを空にして確保を戻す（入力は数え直す。取り置きは外さない。M12 の決定）。
  - 閉じる（`editor::close`・`EngineServices::close_editor`。エディタを閉じる・ライブラリを閉じる・切り替える・終了のどれでも同じ。`LibrarySession::close` が DB を閉じる前に呼ぶ）：中止 → キャッシュを空に → 確保を落とす → 取り置きを 0（NFR-17・ADR-154）。取り置きは持ち主（立てたエディタのセッション。`EngineServices` が持つ）だけが外す（閉じ終わる前に別のアイテムを開いても、古いエディタの閉じ終わりが新しいエディタの取り置きを消さない。`closing_while_another_item_opens_keeps_the_new_reservation`）。`editor::tests` の `open_reserves_the_budget_and_close_returns_it` が、開くと対話の使用量と取り置きが 0 より大きく、閉じると 0 に戻ることを確かめる。
- 実測（M4 Max・release。`tests/bench_session.rs`）：筆 1 本（500 点）の追記の手は p95 8.0〜12.5 µs（筆 999〜5 万本の文書）、引数の変更の手は p50 1.9 µs、undo・redo は約 1 ns、開くのは 3.1 ms（999 本）〜166 ms（5 万本）。5 万本の文書の `to_graph` は 24 ms、正規の JSON は 140 ms（保存・下書きの道。to_graph を通さずに直接書けば縮められる）。

### 12.13 配信（M11・M12）

- M11 で入れたもの（ライブラリのファイル。どれも中身のハッシュから決まる。architecture.md §4）：波形の段 `/peaks/{hash}`（解析のジョブが置いた `kura-peaks`。immutable）、試聴の PCM `/pcm/{hash}/{start}-{end}`（元の中身をエンジンのコーデックの組で読み、フレームの半開区間を WAV にする。kura チャンクに同じ読み取りのループ点・長さ・デコーダの識別子。キャッシュしない）、アニメの帯 `/anim/{hash}/1`（`item_anim_get` が作った PNG。末尾は帯の並べ方の版。immutable で、格子の帯は `?r=`）。
- M12（段 4）：`kura://localhost/edit/{session}/{rev}/…` の画像のタイル（下の「URL の形」）。音声の WAV・PCM・波形は M13。`{rev}` は中身から決まる番号ではない（同じ中身に別の番号が付くこともある）が、セッションの中で番号を使い回さないので、同じ（セッション, rev）の中身は変わらない（応答を覚えてよい）。undo・redo は戻った先の手の番号を返すので、前に取ったタイルがそのまま使える。
- 手のあとは、変わったタイル（`render_view` の `changed`。§12.6・§12.8）だけを取り直す。
- **URL の形**（M12 の段 4。kura-core の `edit::editor::tile`、docs/architecture.md §4）：画像のタイルは `/edit/{session}/{rev}/{段}/{frame}/{tx}/{ty}`（M13 で `pcm`・`peaks` を足す）。段とタイルの位置だけを URL に入れ、表示する範囲（ビューポート）と段の求めは IPC（`edit_step`・`edit_view`）で渡す（段は Rust が決めて結果の `view.level` で返す）。タイルは 1 辺 256（`VIEW_TILE`）、フレームの画像の座標（右端・下端は小さい）。フロントは変わったタイルを、いつも結果の今の rev・段で取る（変わっていないタイルは手元に持っているものを使う）。
  - `session` は再起動をまたいで重ならない値（時刻・プロセス番号・通し番号のダイジェストの 16 桁 ＋ 通し番号。小文字の 16 進）。(session, rev, 段, フレーム, タイル) の中身は変わらないので `Cache-Control: public, max-age=31536000, immutable`。
  - 応えるのは、rev と段が、エディタが表示した今か直前の表示（`published`）と同じで、そのタイルが有効なときだけ（ほかは 404。閉じたセッション・知らないセッションも 404、エンジンがなければ 503）。配信は表示を写して読み、描いている途中の錠を待たない。
  - 中身はどの画素の形式でも RGBA8（ストレート。16 ビットは四捨五入、パレットは色に。`editor::tests` の `tiles_convert_every_pixel_format`（パレット・16 ビット・グレーとアルファ、右端・下端のタイル、RGBA8 と PNG）・`tiles_follow_the_viewed_frame`（フレームの列：見ているフレームだけを描く））。拡張子なし（か `.rgba`）は行を詰めた非圧縮（`w × h × 4`。`application/octet-stream`）、`.png` は PNG（速い圧縮。`image/png`。エディタのプレビューのプールで書く）。どちらにするかは段 5 で WebKit で測って決める。
  - 受け口（main の WebView だけ・Origin・GET/HEAD と 4 つのヘッダ）は他の経路と同じ（`serve::access`）。
- CORS はアプリの origin だけ、受け付けるのは main の WebView だけにする。この受け口（ラベル・Origin・メソッドの判定と、ACAO・`CSP: sandbox`・`nosniff`）と CSP の `connect-src`・`font-src` は M11 で入れた（判定は CI が回す kura-core の純粋な関数 `serve::access`。architecture.md §4.1・ADR-155）。`/edit/…` も同じ受け口を通る。
- ライブラリのファイル（元の中身）の試聴の PCM は M11 の `/pcm/{hash}/{start}-{end}`、編集中のもの（文書を当てた値）は `/edit/{session}/{rev}/…` で配る（architecture.md §4）。

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
- 中止：`par_blocks` がブロックの合間で `canceled()` を見るので、ほとんどの操作は自動で中止に対応する。操作は最初に `cx.check()` を呼ぶ（何もしない近道でも）。適合テストは、操作の中の 1〜4 回目の確認で中止できることを確かめる（E24）。段 10 から、キャッシュの道・範囲の描画・継ぎはぎ・継ぎ目の縮小の途中の中止も `Canceled` で、中止したノードの値はキャッシュに残らない（描き終えた前段の完全な値は残ってよい）。予約（§12.9）で新しい要求に取って代わられた描画も `Canceled`。
- メモリの予算（§12.10。段 10）：足りないときは `Limit`（`code()` は `TooLarge`）。全体を超える見込みの計画は、描く前に「メモリの予算を超える見込みです（見積もり … バイト。予算は … バイト）」で断る。エディタでキャッシュを追い出しても足りなければ「メモリの予算が足りません（見積もり … バイト。キャッシュを追い出しても、固定した値と入力で … バイトを使っています。予算は … バイト）」。どちらも操作は 1 つも動かない（E25）。一括が予算を待っている間の中止は `Canceled`（2 ms ごとに見る）で、使用量と待ちの列は変えない。
- 描くときの入力の値は、計画を作ったときの形と上限に照らしてから使う（ヘッダの形と実際のデコードの結果が食い違う場合に、上限を超えた値を描かない）。
- 進捗：ジョブ（kura-core）が `Monitor` を実装し、既存の `JobContext::progress` につなぐ。
- 上限の既定：1 辺 32,768px、画素数 2²⁸、ノード数・点の数・音声の長さも上限を持つ。設定で変えられる。
- **安全**（ダウンロードしたファイルも読むので）：
  - ヘッダから大きさを読み、`u64` の `checked_mul` で見積もり、上限（`Limits`）で判断してから `try_reserve_exact` で確保する。メモリの予算（§12.10）は、デコーダの中ではなく読む前に、呼び手が形とデコーダの作業領域の倍率（§9.9）を含む見積もりで確保する（デコーダには `Limits` だけを渡す。macOS では大きな確保も失敗しないので、確保の失敗では予算を守れない）。
  - zlib の展開は期待する大きさ（aseprite の cel なら幅 × 高さ × 画素の大きさちょうど）の上限付きの領域にだけ行う。超えたり余ったりしたら壊れたファイルとして扱う。第三者のデコーダの上限（png の `Limits` など）も `Limits` から設定する。
  - 第三者のデコーダの境界では `catch_unwind` してエラーに変える（自分のコードでは panic を禁じたまま）。
  - SVG の外部の参照（file・http）は拒否する（NFR-10）。DTD の実体の展開（入れ子を含む）と `<use>` の展開は、usvg に渡す前に見積もって上限で断る。SVG のファイルは 16 MiB まで。文字のレイアウトの手間（塊の字の数の 2 乗・span ごとの形にし直し・フォントを選ぶ手間・textPath の区間と拡大と弧長の精度）も usvg に渡す前に見積もり、1 件 100 ms ほどで断る（§9.5。M11 の検証 S1・検証のあとの確認 R1）。
  - SVG は、アプリでは別のプロセス（アプリ自身を `--kura-svg-worker` 付きで起動した子）で読み、形 2 秒・値 10 秒で子を止める（M11 の決定 23。§9.5）。子を起動・停止する口は kura-core の `tools::run_binary_kill`（環境変数を消す・新しいプロセスグループ・見張りの中止と時間切れでグループごと SIGKILL・標準出力の上限。起動は `tools::spawn` の鍵の中）。エンジンはプロセスを知らない（差し替えの口 `CodecSet::replace_decoder` だけを持つ）。
  - 段 4 の各デコーダの上限：PNG のフレームの数は fcTL を数える段階で、eXIf はテキスト・ICC と合わせて `max_meta_bytes` で、GIF は 1 フレームの作業領域（`max_pixels`）とフレームの数を LZW を展開しない走査で、WebP の ICC と EXIF の合計は `max_meta_bytes` で、aseprite はファイル全体を `max_value_bytes` で、セル・タイルセットの展開をそれぞれの大きさちょうどで、1 フレームを描く作業領域を `max_value_bytes` の予算で、持っておく展開したセル・タイルセットを値の大きさ（と `max_value_bytes`）まで（段 10。§9.9）で、グループの入れ子を 64 段で、PSD は層とマスクの情報・RLE の行の長さ・圧縮なしの画素をファイルの残りの長さと照らして抑える。
  - 大きな画像：16384² の RGBA（値 1 GiB）を PNG に書いて読み直すテスト（`tests/large.rs`、release で 3.8 秒）のピークのメモリは 3.3 GB（元の値と読んだ値の 2 GiB を含む）。PNG の書き出しは行を詰めた写しを 1 つ作る（値と同じ大きさ）。行ごとに書くとバイト列が変わり、エンコーダの版を上げることになるので、写しは作ったままにし、段 10 のメモリの予算の見積もりに数える（PNG の書きの作業領域は 1 フレームの 3 倍、`fast` は 4 倍。§9.9）。エンコーダの版は上げていない。
  - 段 6 の音声のデコーダの上限：symphonia・nanomp3 に渡すファイルは `max_value_bytes` まで読み込む。長さはデコードせずに数え（パケットの長さの和・MP3 のフレームの走査）、形を上限（`max_audio_frames`・`max_value_bytes`・`max_channels`・`max_sample_rate`）で確かめてから確保する。デコードの途中でも形の上限を超えたら止める。Vorbis は codebook の表の大きさを渡す前に見積もって断る。AIFF の COMM は 279 バイトまで、MARK・INST・テキストは WAV と同じ予算（マーカーの上限・`max_meta_bytes`）。symphonia の panic は `codec::contain` が受け止める。
  - 段 9 の外部のツールと OS の機能（詳しくは §9.8）：ffmpeg は入力をファイルのパスで渡し、分離器・デコーダ・プロトコルをホワイトリストで固定し、環境変数を消して新しいプロセスグループで動かし、見張りが中止・無出力・時間切れ・出力の上限でグループごと止める。出力（WAV）は上限で確かめてから確保する。ImageIO の展開は専用のスレッドで行い、時間切れで見捨てる（壊れた HEIC で戻らないことがある）。Swift の側は大きさをすべて確かめ、Rust が確保した領域にだけ書く。
  - パーサー（PNG・JPEG・GIF・WebP・aseprite・PSD・SVG・WAV・AIFF・FLAC・MP3・Ogg・m4a・AAC・アンカー・文書の読み込み・文書の部品）はファズにかける。
    - `fuzz/`（workspace の外。`cargo +nightly fuzz run <対象>`。対象は `kura_engine::testing::fuzz_decode`・`fuzz_encode`・`fuzz_peaks`・`fuzz_document`・`fuzz_doc_parts`・`fuzz_model`・`fuzz_svg_text` を呼ぶだけ）は、カバレッジを見ながら長く回す。nightly と cargo-fuzz が要る。
    - `tests/fuzz_smoke.rs` は同じ口を stable で回す変異のファズ。種（各形式の小さなファイル・フィクスチャ・文書）をバイトの書き換え・挿入・削除・つなぎ合わせで崩す。ふだんの `cargo test` で対象ごとに 150 回（`quick`）、`KURA_FUZZ_SECS=<秒> cargo test --release -p kura-engine --test fuzz_smoke long -- --ignored` で長く回す（`KURA_FUZZ_ONLY=svg,wav` で対象を選べる）。見つけた入力は `target/fuzz-smoke/` に残し、`replay` で流し直す。
    - 文書のファズは、上限を絞ったエンジンで読み込み、小さな入力（画像・パレットの画像・フレームの列・音声、格子・ピボット・スライス付きのシート、ループ点・マーカー付きの音声）で計画を作って描くところまで流す（段 5 から。新しい操作の実行の経路とメタデータの写し方にも届く）。追記だけの引数（`actions` の列）を持つノードは、途中で切って差分の適用（`Op::apply_appended`）が全体を当て直したものと同じかも照らす（E21）。段 10 から実行器も差分を使うので、実行器の経路（セッションで追記して `compile_snapshot` と `render_cached` の差分の適用で描く）も、文書のファズのセッションの道の照合（`testing::snapshot`）と `tests/e4.rs` の `appended_in_executor_equals_full` で照らす。
    - デコーダのファズは、上限を絞って（`testing::fuzz_limits`）その形式のすべてのデコーダ（旧版を含む）で形と中身を読み、自分のコードの panic（panic の場所が `crates/kura-engine/src/` のもの。`testing::catch_own_panic` が panic のフックで見分ける）・形と中身の食い違い・`Internal` を見つけたものとして扱う。壊れたファイルとしてのエラーと第三者のクレートの panic（`codec::contain` がエラーに変える）は正しい振る舞い。
    - 段 4 の長い実行（release で対象ごとに 40〜60 秒、合わせて約 2 億回）で、SVG の DTD の走査が多バイト文字の途中で文字列を切る panic を見つけて直した（`multibyte_text_around_the_dtd`）。直したあとの実行では何も見つかっていない。SVG と文書は、変異の多くが最初の検査（UTF-8・JSON）で断られるので回数ほど深くは試せていない。深い所はカバレッジを見る `fuzz/` に任せる。
    - 段 5 で、nightly の cargo-fuzz（libFuzzer。release ＋デバッグの検査 ＋ AddressSanitizer、`-fork` で並べて落ちても続ける）を 9 つの対象で回した。各 30 分を 2 回、直したあとに SVG・文書・aseprite を 10〜20 分ずつ、合わせて約 2.7 億回。見つけたのは SVG と自前の新しい検査だけで、どれも直した：参照の輪と深い入れ子によるスタックのあふれ（受け止められずにプロセスごと落ちる）、resvg の pattern のタイルの上限のない確保（209 GB）、巨大な半径の楕円の遅い描画、数の検査の無限ループ。最後の実行（SVG 約 1,360 万回・文書 約 200 万回）では何も見つかっていない。見つけた入力は `replay` で流し直せる形で残す（`fuzz/README.md`）。
    - カバレッジ（育った種で数えた行の割合。同じファイルのエンコーダと、テストの例を作る関数は除く）：PNG 92%・JPEG 84%・GIF 90%・WebP 84%・WAV 92%・aseprite 95%・PSD 94%・SVG 83%。文書は、操作 78〜99%・メタデータの写し方 80%・実行器 72%・描画 70〜93%。届いていないのは主に、中止・あふれ・確保の失敗の枝と、識別子を返す関数。カバレッジで、aseprite のパレットの番号のまま描く経路・メタデータの写し方・差分の適用に届いていないと分かったので、種・入力・照らし合わせを足した。
    - 段 6 で音声の 6 つの対象（`decode_aiff`・`decode_flac`・`decode_mp3`・`decode_ogg`・`decode_m4a`・`decode_aac`）を足した。種は自作の AIFF・FLAC の書き出しと、他のツールで作ったフィクスチャ（MP3・Ogg Vorbis・Opus・ALAC・AAC・HE-AAC）。stable の変異で対象ごとに 60 秒（合わせて約 980 万回）、cargo-fuzz で対象ごとに 15 分（合わせて約 2,360 万回）回し、何も見つかっていない。検証のあとの修正（MP3・AIFF・Ogg・m4a）のあとにも、WAV を含む 7 対象を stable の変異で 40 秒ずつ（合わせて約 1,170 万回）回し、何も見つかっていない。ファズで届かない形（巨大な codebook を名乗る Vorbis・グラニュールで詰めた Ogg・切れた MP3 など）は、検証で組んだ入力を単体のテストとフィクスチャに残した。カバレッジ（読み取りの関数の行。同じファイルのエンコーダ・テストを除く）：AIFF 90%・FLAC 90%・MP3 89%・Ogg・m4a・ADTS の読み取り（`lossy.rs`）93%・symphonia の共通の部品（`symph.rs`）81%。届いていないのは主に、中止・途中でチャンネルの数が変わる枝と、ファズでは組めない形（巨大な codebook を名乗る Vorbis。単体のテストで確かめる）。
    - 段 7 で自前のエンコーダの対象 `encode_ogg`（`testing::fuzz_encode`）を足した。先頭の 19 バイト（`FUZZ_ENCODE_HEADER`）で品質（−1〜10）・レート・チャンネル（1〜9）・長さ（0〜16,384 フレーム）・ループ点（なし・範囲の中・u64 の端に近い始まり／終わり）・タグ（0〜4 個。別名・大文字小文字違い・書けない名前、空・`=`・NUL・4,096 バイト前後・70,000 バイトの値）を決め、残りを f32 のビット列（NaN・∞・巨大な値を含む）として標本にする。書けない形（レート・チャンネルの範囲の外）は断ること、書けると答えた形は断らないこと、すべての ogg のデコーダ（旧版を含む）で読み直した長さ・レート・チャンネルが元と同じこと、コメントのループ点が範囲の中だけ正しい値で書かれ、名前が重ならず、読み直して書き直すと同じコメントになることを確かめる。検証で cargo-fuzz の `encode_ogg` を 31 分（約 6.9 万回）、`decode_ogg`（kura の出力 672 本を種に足して）を 7 分（約 220 万回）、stable の変異の `encode_ogg` を 300 秒（約 7.8 万回）回し、何も見つかっていない。メタデータを足したあとも、stable で 90 秒（約 2.8 万回）と cargo-fuzz で 5 分（約 1.1 万回）回して何も見つかっていない（エンコーダは 1 回が重いので回数は少ない）。カバレッジ（行）：psy 94%・envelope 92%・floor1 88%・res0 86%・block 82%・ogg 79%。届かないのはエラーの枝と、設定のテンプレートでは届かない枝（M7 の短い窓の範囲の外・5.1 の coupling など）。
    - 段 8 で、`fuzz_decode` が読めた値に登録されたすべての解析（見積もりの手間が 2²⁴ 以下のもの）を当て、結果が見積もり以下であることを確かめるようにした。波形の段のバイナリの読み手の対象 `decode_peaks`（`testing::fuzz_peaks`）を足した。release の stable の変異で各 20 秒（peaks 約 6,000 万回、wav・aiff・flac は解析込みで約 4〜23 万回）回し、何も見つかっていない。
    - 段 9 でアンカーの対象 `decode_anchor`（`tests/fuzz_smoke.rs` では `anchor`。種は EXIF 付きの画像・フレームの列・メタデータ付きの音声のアンカー）を足した。確認の回で、release の stable の変異で各 60 秒（anchor 約 1,224 万回・ogg 約 470 万回・m4a 約 29 万回・aac 約 14 万回）、cargo-fuzz（`-fork=4`）で各 10 分（decode_anchor 約 1,321 万回・decode_m4a 約 49 万回・decode_aac 約 20 万回）回し、何も見つかっていない（m4a・aac は、SBR の判定と多チャンネルの断り方を変えたあとの読み取りの経路）。
    - 段 10 で、文書のファズ（`fuzz_document`）に、描けた計画ごとの照合を足した：キャッシュの道（冷えた・温まった＋当たりを描き直す照合・16 KiB の上限で 2 回）とキャッシュなしの道で、描けたか・ダイジェスト・警告（順も）が同じ。範囲の描画と全体の描画、変わった範囲の推論の保守性（`testing::region`）。プレビューの段（`testing::preview`：段 1〜3 の計画が作れるか段 0 に落ちる、等倍で描けるなら段でも描け、キャッシュの道と範囲の描画で同じ値、継ぎ目までが等倍と同じ指紋）。セッションの道（`testing::snapshot`：`compile_snapshot` の計画が `compile` と同じ、追記を前後に分けた差分の計画と描画が全体と同じ）。ファズの対象は足していない（下書きの形式を作らないため）。範囲の班の確認の回で、文書を release で 180 秒回し、見つかったのは照合の側の誤り（出力の ID を 0 と決め打ちしていた）だけだった（直した）。M10 の完了の前に、cargo-fuzz（`-fork`）で全対象を各 10 分、stable の変異で各 60 秒回した（ユーザーの決定）。どれも何も見つかっていない（crash・oom・timeout が 0）。
      - cargo-fuzz（release ＋デバッグの検査 ＋ ASan、`-fork=4`、3 対象ずつ並べて 10 分。種は `export_seeds`）：decode_anchor 約 764 万回・decode_peaks 約 2,827 万回・decode_flac 約 70 万回・decode_ogg 約 62 万回・decode_png 約 49 万回・decode_aiff 約 28 万回・decode_m4a 約 26 万回・decode_wav 約 24 万回・decode_aac 約 19 万回・decode_webp 約 18 万回・decode_mp3 約 14 万回・decode_psd 約 12 万回・decode_svg 約 6 万回・decode_jpeg 約 4.5 万回・decode_gif 約 2.3 万回・decode_aseprite 約 2.2 万回（確認の直しのあと `-fork=8` で 10 分回し直して約 1.7 万回）・encode_ogg 約 1.6 万回。デコードの対象は読めた値に登録されたすべての解析を当てる（段 8）ので、画像の重い形式は 1 回が重い。
      - 文書（`document`）は、段 10 の照合を足した種（約 3,400 個）を ASan の下で最初に流すだけで約 30 分かかり、10 分の枠ではほとんど変異に進まなかった（release では同じ種が全部で約 67 秒、重い種で 1 個 0.5 秒。段 10 の照合を外しても時間は変わらない）。そこで種を 8 個に 1 個（420 個）にして 15 分回し、最初の流しが 162 秒、そのあと約 81 万回で何も見つかっていない。重い種 3 個が slow-unit（ASan の下で 10 秒を超える。timeout の 30 秒には届かない）として残った。
      - stable の変異（release、各 60 秒）：png 約 142 万回・jpeg 約 152 万回・gif 約 1,207 万回・webp 約 221 万回・aseprite 約 1,046 万回（直しのあと約 1,100 万回）・psd 約 390 万回・svg 約 1,165 万回・wav 約 21 万回・aiff 約 18 万回・flac 約 70 万回・mp3 約 16 万回・ogg 約 502 万回・encode_ogg 約 2 万回・m4a 約 28 万回・aac 約 13 万回・document 約 1,068 万回（直しのあと約 1,130 万回）・peaks 約 1 億 7,398 万回・anchor 約 1,237 万回。
    - M12 の段 2 で、文書の部品の対象 `doc_parts`（`testing::fuzz_doc_parts`。§4.4）を足した。`{` で始まる入力は文書の JSON として読み、分けてつなぎ直すと正規の JSON に戻ることを、ほかは部品の組（種類 1 バイト ‖ 長さ ‖ 本文。根は最後）としてつなぎ、つないだ文書を分け直すと同じ根になることを見る。種は文書の JSON と、切れ目 3 つに分かれる長い描画の文書の部品の組。確認の回で、stable の変異（デバッグ、40 秒）約 40 万回で何も見つからなかった。
    - 通常の `cargo test` にも、ChaCha8 でランダムな文書と壊れたファイルを作るテスト（形式ごとの `broken_files_are_errors` など）を入れる。

## 14. 保存と版（M12 以降。意味だけを決める。表は data-model.md に各段で書く）

- 実ファイルとして持つのは、アイテムごとに「元」と「今」の 2 つ。版の記録（文書・値のダイジェスト・出力のハッシュ・形式・コーデックの識別子・日時）はすべて残す（ADR-135）。
- 安全網：
  1. 置き換えたファイルは 7 日残してから消す（バックアップの保持と同じ長さ）。消してよいのは、置き換えから 7 日たち、かつ置き換えより前に作ったバックアップが残っていないとき（バックアップは「新しい 7 個」を残す決まりで日数ではないので、DB をバックアップから戻してもそのファイルがあるように。M12 の利用者の決定 2）。段 3 の GC が `replaced_at <= 今 − 7 日` かつ `replaced_at <` 残っているいちばん古いバックアップの作成時刻（名前の UTC。秒の単位なので、同じ秒の置き換えは残す側）で選ぶ（バックアップがなければ 7 日だけ。下の「GC のジョブ」）。
  2. 消す直前に、その版を文書から作り直して照合する。一致しなければ、そのファイルを残す（`kept`。版の一覧に印、整合性の確かめに件数。問題には数えない）。
  3. Platform・External でしか読めない元には、初めて保存するときにアンカーを作る（§9.4）。保存のジョブが、描いたのと同じ読み取りから作る（エディタを開く速さと対話の予算を守る。保存しない編集では作らない。M12 の利用者の決定 7）。
- **版に戻す**（元に戻す＝版 0、途中の版 k。段 2 の C）も新しい版を足す操作（版の記録は消さない）。版 n+1 を `kind = 'revert'`・`revert_to = k` で足し、文書は版 k の文書（版 0 と、版 0 に戻した版は空の文書で NULL）、入力は版 k の写し、値のダイジェストは版 k と同じ。アイテムは版 k の列（幅・高さ・長さ・拡張子・大きさ。版 0 なら版 0 の列と記録の mime・codec_info）に戻し、title・original_name・素材の設定は変えない。前の頭は上書きと同じく消し待ち（出力が戻した中身か版 0 と同じなら released）、版 k の状態は変えない。戻した結果が今と同じ中身なら版だけ足す（unchanged）。戻す先は 0 以上・頭の版より前（ほかは `Invalid`。まだ保存していないアイテムには戻す版がない）。
  - **使い直す**：版 k の中身が台帳にあり（版 k がまだ持っているか、ほかの版・アイテムが同じ中身を持っている）ファイルがあれば、台帳の門を持って 1 つのトランザクションで戻す（描かない・置かない）。版 0 はいつも hold なので、元に戻すのはいつもこちら。
  - **作り直す**：なければ、版 k の入力の記録（`RecordedInput`）で元を読み（`read_recorded`。アンカーがあればアンカー）、版 k の文書を描いて値のダイジェストを記録と照らし、記録のエンコーダ（`encode_bytes_with`。記録の codec・format・options）で書く。出力は作り直したファイル H′（記録のエンコーダならふつうは版 k と同じバイト列）で、予告 → 配置 → コミットは保存と同じ段 6〜9。
  - **作り直しの照合の決まり**（M12 の段 2 で推奨で採った決定）：値のダイジェストが記録と違えば保存しない（`Mismatch`。何も変えない）。アンカーが使えない（壊れている・ない・別の値）とき、元を記録の識別子のデコーダで読めれば元で読み直し（値が記録と違えば `Mismatch`）、知らせを版の記録の警告に残す（保存の上書き・別名も同じ。§9.4）。記録のデコーダもなければ `Mismatch`。記録のエンコーダがこの環境になければ、今の組の同じ形式のエンコーダで書き、新しい書き出しの記録を残す（値は照らし済み）。バイト列が版 k と違えば知らせだけ残す。
  - 戻した結果が別のアイテムの今と同じ中身なら `Conflict`（上書きと同じ。`items.hash` は一意なので、別名で保存した B を元に戻すと A の今と同じになる場合も `Conflict`）。
- 上書きは「アイテム ID を保ったまま中身を差し替える」操作。タグ・フォルダ・ユニット・プレイリスト・再生の記録・表示名・元のファイル名・素材の設定（格子・ピボット・ループ点）は変わらない（形式が変わる上書きでも title・original_name は変えない。名前は書き出しで直す。素材の設定が中身に合わなくなった知らせは段 5 の UI。M12 の段 2 の決定 10・11）。手順は下の「保存のジョブ」（staging に書く → ハッシュ → 予告 → `files/` に配置 → 1 トランザクションで `items.hash` を比較して置き換え、版を足す → 古いファイルは消し待ち）。
- `items.hash` を書き換えるのは保存の処理だけ。中身に依る列を書く UPDATE は、すべてハッシュを条件にする（古いハッシュで作ったサムネイルを書き込まないため）。M11 で解析（`analysis_state` と結果・事実の書き込み。`WHERE id = ? AND hash = ? AND analysis_state = 'pending'`）とサムネイル（`thumb_state`・`thumb_rev` と読んだメディア情報。`WHERE id = ? AND hash = ?`）に入れた。結果・事実は中身ごと（台帳の行 `blob_id`）に持つので、保存で中身が変わると別の行になる（docs/data-model.md §2）。M12 でプロキシ（`proxy_state` の判定と作った結果。`WHERE id = ? AND hash = ?`）にも入れた。
- 取り込みで、あるアイテムの今・元・過去の版と同じ中身なら、そのアイテムの重複として扱う。
- 上書きの結果が別のアイテム（ゴミ箱のものを含む）の今と同じ中身になったら保存しない（`Conflict`）。別のアイテムの元・過去の版と同じ中身になったときは、上書きは通し（台帳の行を共有する）、別名で保存は新しいアイテムを作らずに同じ中身のアイテムを示す（M12 の利用者の決定 3）。別名で保存の結果がどれかのアイテム（元のアイテム自身を含む）の今と同じ中身のときも、Conflict にせずそのアイテムを示す（`existing`。段 2 の決定 12）。上書きの出力が今と同じ中身なら、版だけ足してファイルは変えない（前の頭は released）。
- 別名で保存は、新しいアイテムを作る。新しいアイテムの元は、元のアイテムの元を引き継ぎ、版 1 に文書を置く（非破壊のままで、非可逆形式でも劣化は元から 1 回分。M12 の利用者の決定 1）。タグ・フォルダ・評価・メモと出どころ（`source_site`・`source_url`・`captured_at`）を写し、ユニット・プレイリスト・再生の記録・素材の設定（`asset_settings`）は写さない。`original_path` は空、`original_name` は「語幹．出力の拡張子」（決定 4、段 2 の決定 9）。来歴（どのアイテムのどの版から作ったか。`item_derivations` に土台の版と今の中身）を残す。元のアイテムには版の行を足さない。
- 編集を始めたときの土台（今の中身と頭の版）が保存のときに違えば `Conflict`（別の保存で今が変わった・今が同じでも版が足された）。編集の途中でゴミ箱に入れられたアイテムの保存も `Conflict`。
- 編集したアイテムでゴミ箱を空にすると、猶予中のファイル・元・アンカーもすぐ消す（ゴミ箱そのものが猶予の役をする。照合しない。M12 の利用者の決定 6）。手順は GC と同じ部品（台帳の門 → 'delete' の予告をコミット → 行を消して台帳を解放し、消えたアイテムの文書の部品を片付けてコミット → ファイルを消す → 予告を消す。`Library::empty_trash`。表は docs/data-model.md §4「保存と版（M12）」）。別名で保存したアイテムが元・アンカーを共有していれば、そのアイテムが残る限り残る（台帳の行は参照されている間は解放しない）。

**保存のジョブ**（M12 の段 2。`kura-core` の `edit::save`。`JobKind::Edit`、同時に 1 本、誰にも譲らない。表と SQL は docs/data-model.md §4「保存と版（M12）」）：

- 積む口（`start_save`）は、文書を内容アドレスの部品（§4.4）に分けて先に DB に置き、ジョブの入力には根のダイジェストだけを書く（版に戻すは文書を持たず、戻す先を積む前に確かめる）（`{"v":1,"itemId","base":{"hash","version"},"action":{"type":"overwrite","doc"}}`、別名は `{"type":"saveAs","doc","title","stem"}`、元に戻すは `{"type":"revert","to"}`）。保存できる文書は、入力が元 1 つ（Primary）・出力が 1 つのファイル（`Expand::One`）のものだけ（ほかは `Invalid`）。出力の形式は文書の `FormatChoice`（`Keep` は元の形式。書けなければ `Unsupported`）。保存でアイテムの種類は変えない。

  | 段 | すること | 強制終了したら |
  |---|---|---|
  | 0 確かめ | このジョブが頭にした版（`item_versions.job_id`）があれば、前回コミットしたので Done。前回 OS のデコーダで元を読んでいる途中で落ちていれば（途中経過の `decoding` が真）、失敗（`ToolFailed`）にして印を消す（やり直せば読み直す）。文書を読み（`Engine::load`）、土台を確かめる | — |
  | 1 予算 | 元（版 0。版の行がなければ今）の形をヘッダから読む（アンカーがあればアンカーの形）。元の形式に Platform の段のデコーダがあり、まだアンカーがなければ（アンカーが使えずに元を記録のデコーダで読み直すときは、そのデコーダが Platform の段なら）、見出しを読む前から途中経過 `{"v":1,"decoding":true}` をコミットし、2 で読み終えるまで保つ（落ちずに失敗・中止・一時停止したら偽に戻す。ADR-158 (9) と同じ決まり。SVG の働き手・ffmpeg は別のプロセスなので付けない）。計画を作り、一括の予算を確保する（アンカーを作りうるなら、ピークと「読む持ち越し ＋ 入力の値 ＋ アンカーを読み直す分」の大きいほう。超えれば `TooLarge`） | staging の掃除とジョブの再開で初めから（`decoding` が真なら 0 で失敗） |
  | 2 読む | アンカーがあればアンカーから（記録はアンカーの元の読み取りの記録）、なければ元を読む。アンカーが使えなければ、元をアンカーの記録の識別子のデコーダで読み直して値を照らす（§9.4。同じなら警告を残して続け、違えば `Mismatch`）。元の形式に Platform の段のデコーダがあれば、見出しを読む前から（1）付けた印を、読み終えたら偽に戻す | 同上（`decoding` が真なら 0 で失敗） |
  | 3 アンカー | 再現を約束しないデコーダで読み、まだアンカーがなければ、`staging/edit-<job>-anchor.<png\|wav>.part` に書いて `sync_all`、読み直して照らし（`anchor::verify`）、ハッシュを取る | 同上 |
  | 4 描く | `Engine::render`（保存は必ず `render`。値のダイジェストを版に残す） | 同上 |
  | 5 書く | エンコードし、生の BLAKE3 を H とし（`EncodedStamp.file_digest` はドメイン付きなので使わない）、`staging/edit-<job>-out.<ext>.part` に書いて `sync_all`。予算と値を手放す。ここまでは一時停止・中止で止まる（staging を消す） | 同上 |
  | 6 予告 | 台帳の門（取り込みの鍵の書きの側）を取る。トランザクション 1 で土台を確かめ直し、読んだアンカー（前からあったもの）の行がまだ同じ台帳の行を指して残っていることを確かめ直し（描いている間にゴミ箱を空にして片付けられていれば `Conflict`。「もう一度保存してください」）、H の持ち主（今 → 元 → 過去の版）で `Conflict`・`existing` を決め、拡張子を決め（台帳に同じ中身の行があればその拡張子、形式が今と同じなら今の拡張子、ほかは形式の代表）、台帳にない出力とアンカーを予告する。`Conflict`・`existing` なら何も置かずに staging を消して終わる | 開くときの予告の照合が予告の行を消し、ジョブは初めから |
  | 7 配置 | `files/` へ同じボリュームの中で改名する（同じ中身がもうあれば staging を消す。台帳に行があるのにファイルがなければ置いて直す） | 照合が台帳にないファイル（と派生のファイル）を消して予告の行を消し、ジョブは初めから |
  | 8 コミット | トランザクション 2 で土台と「別のアイテムの今と同じでない」を確かめ直し、最初の保存なら版 0、アンカー、アイテムの置き換え（別名は新しいアイテム、版に戻すは版 k の列）、前の頭、新しい頭、入力の記録を書き、下書きを片付け（§12.12）、自分の予告を消し、途中経過に結果を書く | 0 がこのジョブの版を見つけて Done（版は 1 つだけ。保存のあとのジョブも積む） |
  | 9 後始末 | 門を放し、staging の残りを消し、サムネイル・解析（画像・音声）・プロキシ（動画・音声）のジョブを積む | — |

  版に戻すときは、0 のあと、版 k の中身が台帳にあってファイルがあれば門を持って 1 つのトランザクションで戻し（使い直す）、なければ 1〜5 を版 k の文書と入力の記録で行い（作り直す。アンカーを作らない。値のダイジェストを照らす）、6〜9 は同じ。

  トランザクション 2 で `Conflict` になったら（門の間にゴミ箱に入れられたなど）、門を持ったまま、このジョブの予告のうち台帳に同じ拡張子で載っていない中身のファイルを消し、予告を消してから失敗にする。結果（`jobs.result_json`）は `{"outcome":"saved"|"unchanged","itemId","version","versionId","hash","ext","reused","anchor":"created"|"used"|null,"warnings"}`（`anchor` は作った・アンカーから読んだ・元から読んだ。アンカーが使えずに元を読み直したときも null）（版に戻すは `"revert":{"to","reused","rebuilt"}` も）か、別名で同じ中身のアイテムがあれば `{"outcome":"existing","itemId","role":"current"|"original"|"past","version","trashed"}`。
- **版のファイルの状態**（`item_versions.file_state`）：`hold` は今か元（消さない）。`waiting` は頭でなくなった版で、段 3 の GC が 7 日と「置き換えより前のバックアップが残っていない」の条件で消す。`kept` は GC の照合が合わず残すもの（もう GC の候補にならない。版に戻すときは使い直せる）。`released` はもうファイルを持たない行（`file_blob_id` は NULL）。頭でなくなる版の出力が新しい頭か版 0 と同じなら、その行がファイルを持つので、すぐ `released`。最初の保存では前の今は元そのもので、版 0 が `hold` で持つ（消し待ちにならない）。

**GC のジョブ**（M12 の段 3。`kura-core` の `edit::gc`。`JobKind::Gc`、同時に 1 本。表と SQL は docs/data-model.md §4「保存と版（M12）」、積む時と譲る相手は docs/architecture.md §5）：

- **候補**：`waiting` の版のうち、上の安全網 1 の条件を満たすもの（`gc::cutoff`。今の時刻とバックアップの一覧は差し替えられる：`GcClock`）を、置き換えの古い順に（部分索引 `idx_item_versions_waiting`）。
- **共有しているもの**：ほかの `hold` の版・アイテムの今・アンカーと台帳の行を共有している候補（版に戻すで使い直した版、上書き（同じアイテムでも別のアイテムでも）が同じ中身を出した版など）も、`released` にする前に照合する（下の「照合」）。一致すれば `released` にし（台帳の行はほかが持つので、ファイルは消えない）、合わなければ `kept`（台帳の行を参照し続けるので、共有相手が消えてもファイルは残る）、照らせなければ `waiting` のまま次回へ。共有を照合せずに `released` にすると、共有相手（ゴミ箱のアイテムなど）が消えたときに、一度も照合していない版のファイルが消えるため（安全網 2。ADR-170 (2)）。共有しているかは、照合が一致したあとに台帳の門の中で見る（下の「消す手順」）。
- **照合**（安全網 2。`save::verify_version`）：版の文書と入力の記録から作り直す（保存の版に戻すと同じ読み：アンカー → 元。予算は一括の予算）。どの候補も照らす。記録のエンコーダで書いたバイト列の生の BLAKE3 が出力と同じなら消し（共有していれば `released` にするだけ）、値かバイト列が記録と違えば `kept`。今は照らせないもの（記録のデコーダ・エンコーダがこの環境にない・大きすぎる・読めない）は `waiting` のまま次回へ。OS のデコーダ（Platform の段）でしか読めないとき（アンカーが使えない）も次回へ（GC はライブラリを開くたびに動くので、同じプロセスで OS のデコーダを呼ばない。落ちる読みを繰り返さない）。SVG はコーデックの組の働き手（別のプロセス）で読む。
- **消す手順**：台帳の門（`Library::ingest_exclusive`）を取り、トランザクション 1 で候補がまだ `waiting` で同じ台帳の行を指していることを確かめ直し（前から共有しているか、作り直している間に版に戻す・上書きで同じ中身が使われて共有になっていれば、`released` にするだけ。S12）、'delete' を予告してコミット → トランザクション 2 で版を `released`（`file_blob_id` は NULL）にして台帳の行を解放（どこからも参照されない行だけ消える）してコミット → 実ファイルと派生のファイル（サムネイル・ジャケット・プロキシ・波形・アニメの帯）を消す → 予告を消す。版の記録（文書・値のダイジェスト・出力のハッシュ・書き出しの記録）は残る。途中で落ちても、開くときの予告の照合が台帳にないファイルを消す（S13）。
- **アンカー**：どの版の入力からも参照されないアンカーの行とファイルも、同じ手順で片付ける（ふつうはない。ゴミ箱を空にする処理が片付ける）。
- **文書の部品**：根（版・下書きの文書と、終わっていない保存のジョブ・7 日以内に失敗・中止した保存のジョブ（やり直せるように）の入力の文書）から辿れない部品を消す。下書きを置き換える・捨てる、上書き・別名の保存のジョブが版を足さずに終わる（待ち・一時停止のまま中止された場合も：`JobHandler::canceled_while_idle` が中止と同じトランザクションで置く）と、部品が孤立しうるので印（settings の `gc.parts`）を置き、印があれば消し待ちの版がなくても GC を積む。GC は部品を片付けたトランザクションで印を消すが、終わっていない保存のジョブか 7 日以内に失敗・中止した保存のジョブがあれば残す（どちらも根として残した部品が、あとで孤立しうる。7 日を過ぎて根から外れると、残した印で GC が積まれて片付ける）。
- **止め方**：候補ごとに一時停止・中止・譲るを見る。カーソル（置き換えの時刻と版の行の ID）と数を候補ごとのトランザクションで途中経過に書き、再開はカーソルの次から（照らせなかった候補は、そのジョブではもう見ない）。結果は `{"v":1,"phase","cursor","released","shared","kept","deferred","anchors","parts","files"}`。
- **版 0** は最初の保存のときに足す（10 万件のライブラリに行を足さない。版の行のないアイテムは「版 0 は暗に今、頭の版は 0」）。版 0 にはアイテムのメディア情報（幅・高さ・長さと、記録の mime・codec_info）と元を読んだ記録を写し、元に戻すときにここから items を戻す。
- **電源断**：改名の前に `sync_all` するので、DB の最後のコミットを失っても残りうるのは「DB にないファイル」だけ（予告の照合か整合性チェックが数える）。置いたファイルの中身は改名の前に書き終えている（台帳に載せたファイルが書きかけになることはない）。

### 14.1 保存の受け入れテスト（M12。`kura-core` のテストとして実装する）

テストの場所：段 2 の保存・版に戻す・下書きは `crates/kura-core/src/edit/save_tests.rs`（S1〜S9・S14・S15・S17）、段 3 の GC と
ゴミ箱を空にする処理は `crates/kura-core/src/edit/gc_tests.rs`（S10〜S13・S16。本当の保存のあとに本物の GC のジョブを動かし、時計と
バックアップの一覧はテストが決める）。S14 の取り込み・
ダウンロードの重複の判定は `import/job.rs` の `duplicate_of_original_or_released_version_is_owned`・`download/register.rs` の
`download_duplicate_of_original_or_released_version_is_owned` も、S16 の DB の側は `items.rs` の
`emptying_the_trash_releases_versions_and_anchors_of_edited_items`。段 2 のテストで版のファイルを消していた補助（`simulate_gc`）は、
段 3 で本物の GC（`gc_version`：8 日進めた時計で GC のジョブを動かす）に置き換えた。GC のほかの確かめ：7 日とバックアップの条件の境界
（`gc_waits_seven_days_and_for_backups_made_before_the_replacement`）、共有している版も照合してから released
（`gc_verifies_shared_versions_before_releasing_them`）、共有相手をゴミ箱から消しても照合していない版のファイルは消えない（照合が
一致すれば作り直せ、合わなければ kept で残って使い直せる。`shared_versions_survive_their_partner_unless_verified`）、OS のデコーダを
呼ばず次回へ回して期限を過ぎた消し待ちに数える（`gc_never_calls_the_os_decoder`）、参照されないアンカーの片付けと、その途中の
強制終了（`gc_sweeps_unreferenced_anchors`）、部品の片付け（`gc_sweeps_orphaned_parts_and_keeps_roots_of_unfinished_or_recent_saves`・
`a_save_that_ends_without_a_version_marks_its_parts_for_gc`・`the_parts_mark_outlives_failed_saves_until_their_roots_expire`・
`canceling_an_idle_save_marks_its_parts`、`gc.rs` の `sweep_parts_keeps_reachable_parts_and_removes_orphans`・
`the_parts_mark_makes_gc_due_until_swept`）、一時停止・再開・中止（`gc_pauses_resumes_from_its_cursor_and_cancels`）、開いたあと・
バックアップの 1 時間ごとの確かめのあとに候補があるときだけ積む（`library/session.rs` の `gc_is_queued_on_open_only_when_due`・
`gc_is_queued_after_the_hourly_backup_check`、`library/backup.rs` の `scheduler_calls_on_check_after_each_check`）。ほかに、版に戻す先の確かめ
（`revert_targets_are_checked_before_queueing`）、作り直しが記録と合わなければ何も変えない（`revert_rebuild_that_does_not_match_the_record_is_refused`）、
戻す先が別のアイテムの今と同じなら Conflict（`revert_to_contents_another_item_holds_is_a_conflict`）、アンカーが壊れていれば元で読み直す
（`revert_falls_back_to_the_original_when_the_anchor_is_broken`）、保存の上書き・別名でもアンカーが壊れている・ないなら元で読み直す
（`saving_with_a_broken_or_missing_anchor_reads_the_original_again`）、描いている間にアンカーが片付けられた保存は Conflict
（`anchor_swept_while_saving_is_a_conflict`）、作り直しで記録のエンコーダがなければ今の組で書く
（`revert_rebuild_without_the_recorded_encoder_uses_the_current_one`）、置いたあと・コミットの前にゴミ箱に入れられたら置いたファイルと予告が
片付く（`trashed_between_placement_and_commit_cleans_up_placed_files`）、書いたところで中止・一時停止すると何も残らず、一時停止は続けると
保存する（`cancel_or_pause_after_writing_leaves_nothing`。上書きと作り直し）、作り直しを 4 段で強制終了しても戻せる
（`revert_rebuild_crash_at_each_stage_recovers`）。本当に落とす試験は `kura-devtools edit crash-loop`（architecture.md §13）。
どの S のテストも終わりに、整合性（DB にないファイル・ファイルのない台帳の行・参照されない台帳の行）が 0、予告が空、staging が空、
版の不変条件（`edit::versions::check_invariants`）が空、保存・GC のジョブが動いていないことを確かめる（`assert_consistent`）。

| ID | 条件 | 期待 | テスト |
|---|---|---|---|
| S1 | 上書き | アイテム ID・タグ・フォルダ・ユニット・プレイリスト・再生の記録は同じ。hash が変わり、サムネイル・解析は作り直しを待つ。版 0（元）と版 1 ができる | `s1_overwrite_keeps_identity_and_adds_versions_0_and_1` |
| S2 | 2 回上書き | 版 1 のファイルは消し待ち、元は保持 | `s2_second_overwrite_waits_version_1_and_holds_original` |
| S3 | 元に戻す | 今が元とバイト単位で同じ。戻した版が足され、前の今は消し待ち（今が版 1 以降のとき。最初の保存の前の今は元そのもので、版 0 が保持する） | `s3_revert_to_original_restores_bytes`（アイテムの列・codec_info が版 0 のものに戻り、所属・表示名は変わらない） |
| S4 | 途中の版に戻す | 猶予中ならそのファイルを使い直す。消えたあとは文書から作り直し、値のダイジェストが記録と一致する | `s4_revert_reuses_waiting_file`（ファイルを足さず、もう一度戻すと unchanged）・`s4_revert_rebuilds_after_gc`（記録のエンコーダで同じバイト列） |
| S5 | 別名で保存 | 新しいアイテムと来歴ができ、元のアイテムは変わらない。新しいアイテムの元は元のアイテムの元で、タグ・フォルダ・評価・メモ・出どころを写し、素材の設定は写さない。結果が元のアイテムの元・今と同じなら、そのアイテムを示す | `s5_save_as_inherits_original_and_copies_metadata`・`s5_save_as_matching_original_points_to_it`・`save_as_matching_its_own_current_is_existing` |
| S6 | 開いたあとに別の保存で今が変わった | Conflict で何も変わらない | `s6_conflict_when_head_changed`・`s6_conflict_when_version_added_with_same_hash`・`saving_a_trashed_item_is_a_conflict` |
| S7 | 出力が別のアイテム（ゴミ箱のものを含む）の今と同じ | Conflict。files/ に孤立したファイルがない（別のアイテムの元と同じなら通して台帳の行を共有する） | `s7_conflict_with_other_current_including_trash`・`s7_overwrite_matching_other_original_shares_ledger_row` |
| S8 | PNG → WebP の上書き | ext・mime・台帳の拡張子が新しくなる。古い今は消し待ち（今が版 1 以降のとき）。表示名・元のファイル名・素材の設定は変えない | `s8_png_to_webp_changes_ext_mime_and_ledger` |
| S9 | 描いたあと・予告のあと・置いたあと・コミットのあとで強制終了 | 開き直すと食い違い 0、版は 1 つだけ、ジョブは done | `s9_crash_at_each_stage_recovers`（上書き・別名 × 4 段）・`s9_crash_while_making_an_anchor_recovers`（偽の HEIC の最初の保存で、出力とアンカーを予告して置く × 上書き・別名 × 4 段）・`a_save_that_crashed_while_decoding_fails_once`・`a_save_that_crashed_while_reading_the_header_fails_once`（見出しを読む前に印をコミットしている） |
| S10 | 7 日を過ぎた版の GC | 照合が一致すれば台帳の行・ファイル・派生のファイルが消え、版の記録は残る | `s10_gc_after_seven_days_removes_the_file_and_keeps_the_record`（7 日に満たなければ残す。消したあとも版に戻せば作り直せる） |
| S11 | GC の照合が合わない（偽のデコーダの組） | 残す（kept）。ファイルは残る | `s11_gc_keeps_a_version_whose_rebuild_does_not_match`（同じ識別子で別の値を読むデコーダ。記録のエンコーダがない版は照らせないので waiting のまま次回へ。kept のファイルは版に戻すときに使い直せる） |
| S12 | GC が作り直している間に、同じ中身が使われる（版に戻すで使い直す・上書き（同じアイテムでも別のアイテムでも）が同じ中身を出す） | ファイルは消されない（台帳の門の中で確かめ直し、共有になったので released にするだけ）。同じ中身を取り込むと過去の版の重複になり（S14）台帳の行を参照しないので、照合したとおり消す（孤立したファイル・ファイルのない行は残らない） | `s12_contents_used_while_gc_rebuilds_are_not_removed`（照合のあと・門の前で止める割り込みで 3 とおり） |
| S13 | GC が台帳の行を解放したあと、ファイルを消す前に強制終了 | 開き直すと孤立したファイルが 0 | `s13_crash_after_releasing_the_ledger_row_leaves_no_orphan`（予告のあと・解放の前に落ちたときは、予告だけ消えて版とファイルは消し待ちのまま） |
| S14 | 取り込んだファイルが元・消えた過去の版と同じ | そのアイテムの重複になり、孤立したファイルがない | `s14_importing_saved_contents_is_a_duplicate`（本当の保存のあと） |
| S15 | HEIC の最初の保存 | アンカーが台帳に載り、OS のデコーダを外しても途中の版を作り直せる | `s15_heic_first_save_makes_anchor_and_reads_it_afterwards`（前半：アンカーの行・台帳の行・ファイル・入力の `anchor_id`、2 回目はアンカーから読む）・`s15_rebuilds_from_the_anchor_without_the_os_decoder`（後半：デコーダを組から外し、版 1 を GC してから版 1 に戻すとアンカーから作り直せて照合が合う） |
| S16 | 編集したアイテムでゴミ箱を空にする | 元・猶予中のファイル・アンカーが消え、参照されない台帳の行が 0。別名で保存したアイテムが元・アンカーを共有していれば残る | `s16_emptying_the_trash_of_edited_items`（偽の HEIC で元・アンカー・猶予中の版・今・下書きの文書の部品。別名の B が残れば元とアンカーは残る。台帳の行を解放したあと・ファイルを消す前に落ちても、開き直すと孤立したファイルは 0）・`emptying_the_trash_releases_versions_and_anchors_of_edited_items`（DB の側） |
| S17 | 下書きのある状態で強制終了 | 同じ文書から戻せる。土台が変わっていれば Conflict | `s17_draft_survives_crash_and_conflicts_when_base_changed`（開き直すと同じ根で fresh、保存で消える、間に別の上書きがあれば fresh が偽で保存は Conflict、保存を積んだあとの下書きは土台が付け替わる） |

## 15. 拡張のシナリオ

| シナリオ | 足すもの |
|---|---|
| 新しい操作「パレットの色の置き換え」 | `ops/palette/replace.rs` と登録の 1 行、ゴールデンの行。`SpaceMap::Identity`・`Lift::None`（入力に Frames も受け付ける。全フレームが 1 つのパレットを共有するので、トリムしたフレームの列にもそのまま当てられる）なので、格子・スライス・アニメはエンジンがそのまま写す |
| 新しい値の型「動画」でトリムだけ | `Value::Video(VideoRef { source, segments })`（展開しない参照）、`ops/video/trim.rs`（区間を書き換えるだけ）、kura-core の ffmpeg のエンコーダ。網羅的な `match` が扱いの要る場所を教える。結果は環境依存の段 |
| 引数の意味を変える（resize の "smooth" を bilinear から Lanczos へ） | 旧版で表せるなら `migrate` を書いて旧版を消す。表せない修正なら旧版を `legacy` に残す |
| 一括で「各アイテムの幅の 50%」 | 大きさの型（`Length::Scale`）で表す。キャンバス・ピボットの相対の指定（「上下左右に 2px」「下中央」）は、M14 の計画のときに、操作の版を上げるか別の操作にするかを決める。タグ由来の値などは M14 で `Bindable<T>`（結び付けの段で具体的な値にし、版に記録する） |
| 点の多いペン | 段 10 で実現した。型付きの引数は動作の列 `ActionList`（§10）、セッションの JSON は `AppendLog`（§12.12）で、どちらも 64 個ずつの切れ目を `Arc` で共有する。追記 1 回の読み込み・推論（`parse_appended`・`infer_appended`）・引数の指紋（追記の連鎖）・変わった範囲・描画（差分の適用）は O(足した分 ＋ 切れ目の数) で、前の筆の本数によらない（5 万本の文書でも、4096² に 1px の筆 1 本（500 点）で p95 0.29 ms 以下。§16）。画面に返すときは件数とダイジェストだけ |

## 16. 性能の目標（requirements.md の NFR-11〜17。M10 の段 10 で測った）

| 項目 | 目標 |
|---|---|
| 引数を変えてからプレビューが更新されるまで（24MP は縮小の段 2、2048² までのドット絵は等倍。減色のプレビューは対象外） | p95 50ms（エンジンの中 25ms） |
| 筆 1 本（500 点まで、32px まで、4096² まで）の確定 | 8ms |
| 塗りつぶし（全面、4096²） | 30ms |
| 保存：24MP の JPEG → 線形光で 50% → JPEG | 0.5 秒 |
| 一括：ドット絵の PNG 1,000 件（トリム・4 倍） | 5 秒 |
| 5 分の曲の解析（波形・ラウドネス・トゥルーピーク） | 2 秒 |
| メモリ | 予算（既定は min(物理メモリ/4, 4 GiB)。分からなければ 2 GiB）を超えない。超える入力は落ちずにエラー |

段 10 で書き足した目標の範囲（ユーザーの決定。requirements.md にも書き足した）。範囲の外のケースは、測って「範囲外」として記録し、判定しない（§16.2・§16.3）。`kura-devtools bench edit` は、筆の太さが 32px（`testing::bench::STROKE_SIZE_LIMIT`）を超えるものと減色のプレビューを、判定の条件で測っても達成・未達にせず判定「範囲外」（JSON の `verdict` は `out_of_scope`）の記録にし、注を付ける（`testing::bench` の `OUT_OF_SCOPE_STROKE`・`OUT_OF_SCOPE_REDUCE`）：

- 筆は 32px まで（64px の筆 1 本は操作そのものが約 23 ms かかる。§16.3）。
- 減色（Indexed8 への `image.convert`）のプレビューは対象外。描けるまで古い結果を出したまま、描けたら差し替える（2048² の 16 色のスプライトなら p95 22.6〜22.8 ms で目標の中だが、色の多い最悪の形（1024² の乱数を 256 色に）は段 5 の実測で 316〜496 ms）。

判定の条件として、24MP の写真は縮小の段 2（1500×1000）で判定する（段 1 は記録だけ。3 スレッドで Lanczos3・鎖の p95 64〜72 ms）。

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

### 16.1 段 10 の測り方

- 判定の機械と条件（ユーザーの決定）：Apple M4 Max（高性能 12・高効率 4、128 GB）、release、プレビューのプールと一括のプールを 3 スレッドにして判定する。4・16 スレッドでも回して記録する（判定はしない。アプリの 16 コアの既定のプールは 4・12。§12.11）。
- 「エンジンの中」（NFR-11・12）：編集（文書か `Edit`）を受けてから、プレビューの値と変わったタイルの一覧ができるまで。セッションの手・差分の計画（写真は段の計画）・予算の確保（`admit_cached`・`settle`。1 µs ほど）・表示の経路（`render_view`。§12.8。表示する範囲は出力の全体、ダイジェストなし）を含め、ダイジェスト・エンコード・IPC・WebKit を含めない。p95 50 ms の全体（WebKit の表示まで）は M12 で測る。
- 回数と統計：NFR-11・12 は慣らし 10 回のあと 30 回（p50・p95）、NFR-13 は慣らし 2 回のあと 10 回・NFR-15 は慣らし 1 回のあと 5 回（中央値・最大）、NFR-14 は慣らし 1 回のあと通し 3 回（最大で判定）。百分位は最近順位法（昇順の ⌈p·n/100⌉ 番目）で、20 回より少なければ p95 を出さず最大で判定する（`testing::bench`）。
- 毎回、引数を違う値にする（スライダーの i 段目。キャッシュに当てない）。測る前に、前の文書でキャッシュを温める（測らない）。
- ロードアベレージ（`sysctl -n vm.loadavg`）を回の前後とケースごとに記録する。ほかの作業でロードアベレージが 12 を超える中では p95 が大きく跳ねた（筆 5 万本の文書で 8〜15 ms。負荷が下がってから測り直すと下の表の値に戻った）ので、判定に使う値は負荷の低いとき（ロードアベレージ 4〜6）に測る。
- 入力は ChaCha8 と整数だけで作る（`InputGen::Photo`・`InputGen::PixelSprite`、`testing::bench` の `walk_stroke`・`program_audio`・`sprite_batch`。どれもダイジェストを凍結）。実写真は `--photo` で渡す（リポジトリに置かない・ダウンロードしない。段 10 では合成の写真だけで測った）。
- NFR-16：判定の条件の最後の文書・結果を、別のエンジン・キャッシュなしの一括の経路（1・3・16 スレッド）で作り直し、値のダイジェストと、固定のコーデックで書いたバイト列を照らす。エディタの側のダイジェストは、表示の経路で測ったのと同じエディタの経路（セッション・キャッシュ）で等倍の全体を描いたもの（`render_cached`。`EditorPath::digests`）。表示の値が `render` と同じことは `tests/view.rs` が確かめる（§12.8）。
- NFR-17：自分を子プロセスとして `/usr/bin/time -l` で起こし、peak memory footprint から基準を引いた量を予算と比べる（§12.10 の判定の式）。小さい予算では落ちずに `Limit` になること。
- 道具：`kura-devtools bench edit`（表・`--json`・`--markdown`。architecture.md §13 の表の形。判定は達成 `pass`・未達 `fail`・範囲外 `out_of_scope`・記録 `record`・見送り `skip`）と、`tests/bench_*.rs`（`#[ignore]`。部品ごとの実測）。

### 16.2 段 10 の実測

M4 Max・release・3 スレッド。`kura-devtools bench edit --runs 3`（コミット d1ce160。ロードアベレージは各回の始めで 4.8〜6.4、ケースごとの記録で 3.6〜10.4）の 3 回の幅。NFR-11・12 は表示の経路（§12.8）で測った「エンジンの中」（§16.1）。写真（合成の 6000×4000）は段 2 の計画で判定し、段 1 は記録。時間は ms。

| NFR | 項目 | p50 | p95 | 判定 |
|---|---|---|---|---|
| 11 | ドット絵 2048²：`palette.replace`（Indexed8） | 0.03 | 0.04 | 達成 |
| 11 | ドット絵 2048²：`convert`（Rgba8 → Rgb8 の下地の色） | 9.54〜10.2 | 13.6〜14.6 | 達成 |
| 11 | ドット絵 2048²：`canvas`（幅と高さを 1 ずつ） | 3.33〜3.34 | 3.60〜3.76 | 達成 |
| 11 | ドット絵 2048²：最近傍の拡大（2 倍前後の幅） | 12.6〜13.4 | 13.9〜14.1 | 達成 |
| 11 | ドット絵 2048²：`trim_alpha`（しきい値） | 1.37〜1.45 | 1.96〜2.05 | 達成 |
| 11 | ドット絵 2048²：`split_grid`（セルの大きさ） | 1.65〜1.82 | 2.03〜2.70 | 達成 |
| 11 | ドット絵 2048²：減色（Rgba8 → Indexed8、色の数を変える） | 21.5〜22.0 | 22.6〜22.8 | 範囲外 |
| 11 | 音声 5 分：`gain`（0.1 dB ずつ） | 8.75〜9.51 | 10.3〜12.2 | 達成 |
| 11 | 音声 5 分：`fade`（フェードインの長さ） | 12.4〜12.9 | 12.7〜14.7 | 達成 |
| 11 | 音声 5 分：`normalize` の目標（解析はキャッシュに当たる） | 37.5〜38.8 | 38.4〜40.1 | 未達 |
| 11 | 写真 24MP・段 2：`crop` の位置（1 画素ずつ） | 0.11〜0.19 | 0.16〜0.23 | 達成 |
| 11 | 写真 24MP・段 2：Lanczos3 の縮小（40〜60%） | 16.2〜16.4 | 18.6〜19.2 | 達成 |
| 11 | 写真 24MP・段 2：鎖（`crop` → Lanczos3 50% → Gray8）の `crop` | 16.5〜16.8 | 16.8〜17.2 | 達成 |
| 11 | 同・段 1：`crop`・Lanczos3・鎖 | 0.35〜0.72・59.3〜59.8・65.2〜65.7 | 0.49〜0.79・64.0〜64.8・67.8〜72.2 | 記録 |
| 12 | 筆 1 本（500 点）の確定、4096²、前の筆 0・999・5 万本：1px | 0.17〜0.21 | 0.20〜0.29 | 達成 |
| 12 | 同：8px | 0.43〜0.48 | 0.48〜0.59 | 達成 |
| 12 | 同：32px | 5.47〜5.78 | 5.74〜6.17 | 達成 |
| 12 | 同：64px | 22.3〜23.4 | 23.3〜24.6 | 範囲外 |
| 12 | 筆のあとに最近傍 ×4（1024² → 4096²・前の筆 999 本）：8px | 1.31〜1.37 | 1.86〜2.17 | 達成 |
| 12 | 同：32px | 6.51〜6.61 | 7.62〜8.04 | 未達（3 回のうち 1 回。境目） |
| 12 | 同（2048² → 8192²。参考）：8px・32px | 1.48〜1.51・6.79〜6.94 | 2.13〜2.15・7.73〜7.88 | 記録 |
| 12 | 全面の塗りつぶし 4096²（4 連結・つながった全面） | 27.3〜28.0 | 33.2〜33.7 | 未達 |
| 12 | 全面の塗りつぶし 4096²（画像全体の同じ色） | 26.4〜26.5 | 32.7〜33.1 | 未達 |

| NFR | 項目 | 結果 | 目標 | 判定 |
|---|---|---|---|---|
| 13 | 保存：24MP の合成の写真の JPEG → Lanczos3 50% → JPEG q90（ファイルの読み書き・元のハッシュ・予算の確保・出力のダイジェストを含む） | 中央値 414〜420・最大 418〜426 ms | 0.5 秒 | 達成 |
| 13 | 同・bilinear（参考） | 中央値 289〜294・最大 291〜314 ms | 0.5 秒 | 記録 |
| 14 | 一括：ドット絵の PNG 1,000 件（トリム → 最近傍 ×4、メモリの中で読み書き） | 通し 468〜510 ms（各回の最大） | 5 秒 | 達成 |
| 15 | 5 分の 44.1 kHz ステレオの読み取り ＋ 波形・ラウドネス（トゥルーピーク）：WAV・FLAC | 中央値 195〜198・336〜346 ms（最大 197〜198・337〜348 ms） | 2 秒 | 達成 |
| 16 | 23 のケース（NFR-11〜15 の文書・結果）を、別のエンジン・キャッシュなしの一括の経路（1・3・16 スレッド）で作り直す | 値のダイジェストと PNG・JPEG・WAV のバイト列・解析の正規の形が一致 | 一致 | 達成 |
| 17 | 保存の通し（24MP → Lanczos3 50% → JPEG q90、予算の確保つき） | 実測 187〜188 MiB・見積もり 364.5 MiB（予算 4 GiB） | 予算以内 | 達成 |
| 17 | 筆の文書のエディタ（4096²・筆 999 本 ＋ 8px を 20 本、キャッシュの確保つき） | 実測 163.5〜163.8 MiB・見積もり 206.8 MiB（予算 4 GiB） | 予算以内 | 達成 |
| 17 | 小さい予算での保存（128 MiB）・エディタを開く（64 MiB） | 落ちずに `Limit` | 超える入力は落ちずにエラー | 達成 |

スレッドの数の記録（`--threads 4,16 --only 11,12,13,14,15`。p50／p95、ms。保存・解析は中央値、一括は通し。1 回、ロードアベレージ 4.1〜5.2（ケースごとの記録で 3.9〜5.4））：

| 項目 | 3 | 4 | 16 |
|---|---|---|---|
| ドット絵 2048²：`convert` | 9.97／14.4 | 7.42／7.63 | 2.61／2.89 |
| ドット絵 2048²：最近傍の拡大 | 12.6／13.8 | 9.53／10.1 | 3.20／3.56 |
| ドット絵 2048²：減色 | 21.6／22.7 | 21.6／22.2 | 21.6／22.2 |
| 音声 5 分：`normalize` の目標 | 38.3／39.7 | 37.5／37.9 | 35.8／36.3 |
| 写真 24MP・段 2：Lanczos3・鎖 | 16.3／18.8・16.8／17.1 | 11.9／12.2・15.9／16.4 | 11.9／12.1・9.00／9.15 |
| 写真 24MP・段 1：Lanczos3・鎖 | 59.6／64.5・65.8／68.3 | 54.8／56.5・50.4／51.4 | 17.8／20.2・18.5／20.8 |
| 筆（前の筆 5 万本）：8px・32px・64px | 0.50／0.56・5.84／6.14・23.6／24.2 | 0.46／0.54・5.80／6.03・23.5／24.0 | 0.47／0.55・5.84／6.01・23.4／24.1 |
| 筆のあとに最近傍 ×4（1024²）：8px・32px | 1.33／1.96・6.79／7.61 | 1.04／1.46・6.56／7.25 | 0.89／1.11・6.31／6.88 |
| 同（2048²）：8px・32px | 1.52／2.14・7.08／7.78 | 1.23／1.55・6.70／7.45 | 0.95／1.11・6.38／6.65 |
| 全面の塗りつぶし：連結・画像全体の同じ色 | 28.3／35.6・26.6／33.3 | 20.1／21.1・19.7／20.0 | 7.21／7.49・6.73／6.92 |
| 保存（Lanczos3） | 420 | 358 | 227 |
| 一括 1,000 件 | 503 | 365 | 119 |
| 解析（WAV） | 199 | 171 | 136 |

- 筆・正規化・減色は、スレッドの数でほぼ変わらない。
- 段 10 の途中（コミット c8286e7。`render_cached` で等倍の全体を描いていたとき）との比較（p95）：筆のあとに最近傍 ×4 の 1024²・8px は 18.1〜19.3 → 1.86〜2.17 ms、32px は 23.0〜23.6 → 7.62〜8.04 ms、2048²（参考）は 60〜72 → 2.1〜7.9 ms。24MP の写真の鎖は、等倍で描いた仮の値 266〜268 ms → 段 2 で 16.8〜17.2 ms。

### 16.3 未達・範囲外と理由（段 10）

値を変えずに速くできるもの（ダイジェストを外す・差分の計画・差分の適用・縮小の段・キャッシュ・表示の経路・paint の変わる範囲の区切り）は段 10 で入れて測り、それでも外れるものを未達として理由つきで記録する（ユーザーの決定）。目標の範囲は §16 の頭の 2 つ（筆は 32px まで・減色のプレビューは対象外）だけで、その外のケース（64px の筆・減色）は「範囲外」として記録し、判定しない。

- **音声の正規化の引数の変更**（NFR-11。p95 38.4〜40.1 ms）：ラウドネスの計測はキャッシュに当たる（解き直さない）が、利得を求める `analysis::loudness::Measured::gain_to` が、候補の区間ごとに積分ラウドネスを全ブロックで数え直す（O(ブロック²)。5 分で 2,997 ブロック、32〜38 ms。使い捨てのテストで測って確かめた）。操作そのものの時間で、スレッドの数でもほぼ縮まない（16 スレッドで p95 36.3 ms）。値を変えずに速くする手は、段 10 では入れていない。
- **32px の筆のあとに最近傍 ×4 が続く文書**（NFR-12。1024² → 4096²。p95 7.62〜8.04 ms で、3 回のうち 1 回（負荷のいちばん低い回）が 8 ms を 0.04 ms 超えた。境目）：表示の経路（§12.8）で、筆は差分の適用、後ろの最近傍 ×4 は変わったタイル（筆 1 本あたり平均 19 枚）だけを描く。32px の筆そのものが 5.5〜5.9 ms、最近傍のタイルが 1〜2 ms。実行器の側で値を変えずに減らせるものは入れ終えた。さらに縮めるには、操作の実装（筆の描画か最近傍の範囲の描画）を速くする必要がある。4・16 スレッドでは p95 7.25・6.88 ms（1 回）。
- **全面の塗りつぶし**（NFR-12。3 スレッドで p95 32.7〜33.7 ms。4 スレッドで 21.1・20.0 ms、16 スレッドで 7.5・6.9 ms（1 回））：4096² の全画素を書く操作そのものの時間と見ている（スレッドの数で縮む）。3 スレッドでの目標 30 ms に届かない。
- **64px の筆**（NFR-12。範囲外。p95 23.3〜24.6 ms）：差分の計画と実行器はどちらも 0.1 ms 未満で、残りは筆を描く操作そのもの（前の筆の本数・スレッドの数によらない）。目標の範囲（筆は 32px まで）の外。
- **減色のプレビュー**（NFR-11。範囲外）：目標の範囲の外（§16 の頭）。2048² の 16 色のスプライトでは p95 22.6〜22.8 ms（記録）。
- 段 10 で解消したもの：8px の筆のあとに最近傍 ×4 が続く文書は、`render_cached` で等倍の全体を描いていたときは p95 18.1〜19.3 ms で未達だったが、表示の経路で 1.86〜2.17 ms になった。24MP の写真の引数の変更は、等倍で描くと p95 238〜268 ms（Lanczos3・鎖。段 10 の途中の値）だったが、段 2 の計画を表示の経路で描いて p95 0.16〜19.2 ms になった。

### 16.4 段 10 の部品の実測

- キャッシュ・差分の計画・範囲・変わった範囲・プレビューの段・セッション・メモリの見積もりの実測は、§12 の各項に書いた（§12.3〜§12.7・§12.10・§12.12）。
- 値のダイジェストは、帯に詰めて 4096² の RGBA8 が 31 ms、24MP の RGB8 が 36 ms（§8.6。保存・一括の時間に入る）。

### 16.5 M12 のアプリの道（エンジンの外）の実測

M12 の段 6（2026-10-24）。段 10 の `bench edit` はエンジンの中だけを測る。M12 で足したアプリの道を、アプリと同じ部品（`LibrarySession`・`EngineServices`・アプリのコーデックの組・取り込みと保存のジョブ）で測る道具を足した：`kura-devtools bench editor`（`edit::editor` の 1 手と kura:// の `/edit` のタイルの書き出し `tile::serve`、NFR-17 の取り置き・使用量）と `kura-devtools bench save`（保存のジョブの全体）。使い方・表・機械と負荷は architecture.md §13 の「M12 の実測」。どちらも新しいライブラリを作業用の場所に作り、bench edit と同じ生成器の入力を取り込みのジョブで入れ、同じ文書（`Edit::Replace` で 1 手で入れる。ノードの ID も同じ）・同じ手で測る。画面（IPC・WebKit）は含めない。判定の条件は §16.1 と同じ（M4 Max・プールを 3 スレッド・慣らし 10 回のあと 30 回）。

- **1 手（`edit::editor::step`）**：手を受けてから、表示の値と変わったタイルの一覧ができるまで（§16.1 の「エンジンの中」と同じ区切りを、アプリの口で測る）。段 10 の値とほぼ同じで、エディタのセッションが足す分（手の受け付け・下書きの依頼・文書の差分・表示の情報・配信用の表示の差し替え）は 0.03〜0.05 ms。NFR-11 の判定のケース（ドット絵 2048² の 6 操作、24MP の写真の段 2 の 3 つ）は p95 最大 18.7 ms で達成。NFR-12 の筆（1・8・32px、前の筆 0・999・5 万本）は p95 最大 6.41 ms で達成。未達は段 10 と同じ 2 つ：32px の筆のあとに最近傍 ×4（1024²）が p95 7.78〜8.01 ms（3 回のうち 1 回が 0.01 ms 超え）、全面の塗りつぶし 4096² が p95 29.7〜39.9 ms（4 スレッドで 21〜22 ms、16 スレッドで 8〜12 ms）。
- **タイルの書き出し**：RGBA8 は 1 枚 0.02〜0.07 ms（256 KiB）、PNG は 0.1〜0.6 ms（ドット絵 3.5〜42 KB、写真 70〜100 KB）。フロントと同じ 6 本同時で、変わったタイルの全部（4096² の 256 枚）を RGBA8 で 1.2〜2.0 ms、PNG で 6.4〜8.5 ms。1 手 ＋ RGBA8 のタイル（NFR-11 の 50 ms のうち Rust の中の分）は p95 最大 18.9 ms で、残り約 31 ms が IPC・WebKit の枠。
- **PNG のタイルは次の手を遅くする**：PNG はプレビューのプールで書くので（`tile::serve`）、1 手ごとに PNG を書き出すと、次の手が 20〜35% 遅くなる（最近傍 p50 12.7 → 17〜18 ms、写真の Lanczos3 16 → 20 ms、32px の筆 5.8 → 7.1〜7.9 ms で p95 は 3 回のうち 2 回 8 ms を超える、塗りつぶし 29 → 34〜38 ms）。RGBA8 だけなら段 10 と同じ値（タイルを書き出さない形とも同じ）。タイルの形式（段 5 で WebKit で測って決める。今は RGBA8）を PNG にするなら、PNG をプレビューのプールの外（小さな別のプール）で書く必要がある。RGBA8 のままなら、4096² の全面が変わる手で 64 MiB を送るので、IPC・WebKit の側の時間を画面で確かめる（見える範囲だけを描く形（等倍で 1600×1000）では 28 枚・7 MiB）。
- **確保器（ADR-163。M11）の影響**：1 MiB 以上の確保を毎回 `mmap` で取り直してページを 0 で埋め直すので、値が 1 つの大きな確保の操作（5 分のステレオの音声 約 100 MiB、減色）は段 10 の値より約 5 ms 遅い（bench edit を確保器ありと、外した一時のビルドで 2 組ずつ：利得 p50 8.4〜8.7 → 13.9〜14.0、正規化 36〜38 → 42 ms、減色 20 → 25.5〜26.2 ms）。画像はタイル（256 KiB）ごとの確保なので変わらない。M12 のエディタは画像だけなので M12 の判定は変わらないが、M13 の音声エディタの前に、確保器に解放した大きな領域を少しだけ（上限つき・時間で返す）使い回す口を足すか、エンジンが出力の領域を使い回すかを決める。
- **保存のジョブの全体**（`start_save` → ジョブ → 読む → 描く → 書く（`sync_all`）→ 予告 → 置く → コミット → Done の知らせ）：24MP の JPEG → 線形光で Lanczos3 50% → JPEG q90 の上書きが、中央値 454〜470 ms で NFR-13（0.5 秒）を達成（余裕 30〜46 ms）。同じ写真・同じ実行の場の段 10 の道（読み・描き・書き。414〜423 ms）より 33〜49 ms 長く、スレッドの数によらない。分かっている内訳は、待ち行列 0.6〜1.1 ms、積む呼び出し 0.35〜0.75 ms、出力の `sync_all`（macOS では `F_FULLFSYNC`。3.5 MB で約 7.5 ms）。DB は WAL・`synchronous = NORMAL` でコミットは同期しない。残り（約 25〜40 ms）は段ごとに分けていない（余裕を広げるなら、ジョブの段ごとの時間をログに出して分ける）。参考：途中の版に戻す（作り直し）451〜462 ms、別名で保存 458〜467 ms、元に戻す（使い直し）2.2〜5.6 ms、2 回目の上書き（bilinear）333〜339 ms、2048² のドット絵の PNG の上書き 95〜103 ms。
- **NFR-17**：エディタを閉じると、予算の使用量（取り置き・対話・持ち続ける分・一括）がすべて開く前に戻る（19 ケース × 3 回 × 3 つのスレッドの数）。開いている間は取り置き 1 GiB と、同じ量のキャッシュの確保。24MP の JPEG を開いて段 2 → 等倍で手を当てると、予算で数えた最大が 1032 MiB（段 2）・1111 MiB（等倍まで）、実際のフットプリント（基準の子との差）が 1166〜1187 MiB で、予算 4 GiB の内。
- **画面を使わないと測れないもの**：NFR-11 の 50 ms の全体（IPC の往復と DTO、kura:// のタイルの取得、`ImageData`・`createImageBitmap`・キャンバスへの描画）と、エディタを開いたときの NFR-07（ライブラリの表示の分。WebContent が持つタイルの絵を含む）。音声の NFR-11 は M13（エディタが画像だけのため）。

## 17. 受け入れテスト（`kura-engine` のテストとして実装する）

| ID | 内容 |
|---|---|
| E1 | 同じ文書と入力で 2 回描くとダイジェストが一致する。スレッド数を 1・3・16 にしても一致する。ノードのキャッシュの有無（なし・冷えた・温まった・上限 0・小さい上限）でも、2 つのプール（4 と 12 スレッド）で別々のキャッシュを持って同時に描いても一致する（段 10。`tests/cache.rs` の `every_example_is_the_same_with_and_without_the_cache`・`two_pools_render_the_same`、kura-core の `two_pools_render_the_same_value_at_once`） |
| E2 | 固定のコーデックだけの文書は、何度作り直しても値のダイジェスト（R1）が一致する。PNG と WAV は出力のバイト列（R2）も一致する |
| E3 | 版 2 は「元＋版 1 と版 2 の操作」から作られ、版 1 の出力を経由しない。非可逆形式で 2 回保存しても、値は元から 1 回エンコードしたものと同じ |
| E4 | 同じ文書を同じ元に当てると、エディタの経路（`EditSession`・差分の計画・ノードのキャッシュ・差分の適用。3 スレッド・キャッシュの上限 0〜1 GiB）と一括の経路（保存の JSON を別のエンジンで読み、キャッシュなしの `render`。1・16 スレッド）で、計画（指摘を含む）・出力の値・固定のコーデックで書いたバイト列（PNG・WAV）・警告が同じ。ランダムな編集の列（引数・追記・外す・置き換え・仮の値・まとめ方・取り消し・やり直し・中止・量子化を挟む）のどの時点でも（段 10 で書き換えた。`tests/e4.rs` の `random_edit_sequences_match_the_bulk_path`。差分の計画が `compile` と同じことは `compile_snapshot_equals_compile`、鍵が同じことは `session_keys_equal_compile_keys`、登録表の追記だけの配列の扱いが違うエンジンで `compile_snapshot` すると鍵を求め直してそのエンジンの `compile` と同じになることは `session_keys_follow_the_registry_of_the_engine`） |
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
| E18 | 区間ごとに描いても（チャンク 1・7・4,096・全体）値が同じ。画像は、範囲（タイル）ごとに描いても、全体を描いたものの同じタイルと同じ。操作が申告した依存（`Reach`）の外の入力を変えても、範囲の中は変わらない。i64 の端に近い・負の大きな座標の要求も、出力のキャンバスに切ってあふれずに描け、遠い矩形を混ぜて畳んでも頼んだタイルを描く。量子化した出力も `render` と同じ（段 10。`tests/region.rs` の `reach_is_sound`・`region_equals_full_crop`・`render_area_matches_full`・`far_requests_are_clipped_to_the_canvas`・`quantized_outputs_match_render`、`render::region` の `rects_merge_cover_and_fold`） |
| E19 | ラウドネスの計測が EBU Tech 3341 の試験信号で ±0.1 LU。正規化したあとの積分ラウドネスが目標の ±0.1 LU。LRA が EBU Tech 3342 の試験 1〜4 で 10・5・20・15 LU（段 8） |
| E20 | WAV の smpl・cue、AIFF の MARK・INST、Ogg の LOOPSTART・LOOPLENGTH を読んで書いても保たれる（FLAC の LOOPSTART・LOOPLENGTH も往復を確かめる。Ogg の書き出しは段 7 で足した。他のツールで書いた Ogg のループ点も読む）。範囲の外・長さ 0・逆向きのループ点はどの形式も書かない。名前の重なるタグは 1 つだけ書き、書いて・読んで・書き直すと同じコメント（FLAC はバイト列も同じ、Ogg はシリアル番号と CRC を除いて同じ） |
| E21 | 同じ点の列からは同じ画素ができる。差分で当てても全体を当て直しても同じ。ピクセルパーフェクトの規則どおり。TS と共有するベクタと一致する。実行器の経路（セッションで追記し、キャッシュにある前の出力に差分を当てる `render_cached`）でも同じ。表示の経路（`render_view`。§12.8）でも、筆の追記は描画のノードを差分の適用で描き、表示の値が `render` と同じ（段 10。`tests/e4.rs` の `appended_in_executor_equals_full`、`tests/view.rs` の `view_redraws_only_changed_tiles_after_strokes`） |
| E22 | 塗りつぶしは 4 連結・8 連結・許容差の定義どおりで、たどる順に左右されない |
| E23 | スポイトが返す色とインデックスが、描画結果の画素と一致する。スポイトは等倍の 1×1 の範囲を描いて拾い、縮小した段からは拾わない（段 10。`tests/region.rs` の `spoit_via_region`、`tests/preview.rs` の `level_values_are_not_for_saving`） |
| E24 | 中止すると `Canceled` になり、途中までの値もファイルも出ない。キャッシュの道・範囲の描画・継ぎはぎ・予算の待ち・新しい要求（予約）による中止でも同じで、中止したノードの値はキャッシュに残らない（描き終えた前段の完全な値は残ってよい）（段 10。`tests/cache.rs` の `cancel_keeps_the_cache_consistent`、`tests/region.rs` の `cancel_in_region_and_patch`、`tests/budget.rs` の `cancel_while_waiting_for_the_budget`、`monitor::tests::superseded_renders_are_canceled`） |
| E25 | 上限を超える出力は推論の段階で `Limit` になり、描かない。壊れたファイル・展開爆弾で落ちない。メモリの予算を超える計画は、見積もりの段階で `Limit` になり、操作が 1 つも動かない。見積もりは、描いた値で数えた生きている値以上（段 10。`tests/budget.rs` の `over_budget_plans_fail_before_rendering`・`estimate_bounds_every_example`・`estimate_bounds_chains`）。作業領域の申告は、データで量の変わる確保（塗りつぶしの集合と待ち行列・ストローク、aseprite のファイルと持っておくセル）を覆い、読むファイルの長さが見積もりに届く（`ops::paint::actions` の `work_covers_the_fill_and_stroke_buffers`・`paint::fill` の `work_counts_the_masks_and_the_whole_queue`・`codec::aseprite` の `work_covers_the_file_and_the_retained_cels`・`tests/budget.rs` の `file_lengths_reach_the_decoder_estimate`）。エディタが追い出しても足りずに `Limit` のときは、キャッシュの上限と確保を元に戻す（`interactive_fails_when_pinned_values_fill_the_budget`・`interactive_limit_after_eviction_restores_the_capacity_and_lease`）。今の取り置きとエディタのキャッシュの確保のもとで通りえない一括は駐車して、後ろの収まる一括を塞がない（`budget::tests::requests_over_the_reservation_park_and_do_not_block`・`standing_interactive_over_the_reservation_parks`・`tests/budget.rs` の `an_editor_lease_over_the_reservation_does_not_block_a_save`） |
| E26 | 解析と操作が一致する。申告した解析を実行器が渡した値と別に求めた値が同じで、操作はその値を使う（登録されたすべての操作の例）。意味の対：不透明な範囲とトリムで切り取る範囲、ラウドネスの値と正規化で使う値、色数とパレットへの変換、倍率と最近傍のリサイズ、格子と分割、無音とトリム、ループ点とループの設定、透過と形式の能力。登録されたすべての解析が対の表か「対のない解析」の表に載る（§11.5） |
| E27 | aseprite の合成が、照合相手の実装と一致する。段 4 の asefile との照合（300 個のランダムな絵。`tests/aseprite_ref.rs`）で満たす（段 8 でユーザーが決めた。Aseprite 本体で書き出したフィクスチャとは照らさない。ADR-152） |
| E28 | 自作の Ogg Vorbis の出力が、独立したデコーダ 2 つ以上（symphonia・lewton）で読め、長さ・位置・ループのコメントが保たれる。ページが §9.7 の規則に従い（「長・次は短」が途中と最後の前に来る実際の出力でも）、品質の段・スレッド数・チャンネルとレートの範囲が約束どおり。NaN・±∞ を含む入力が 0・±1 に置き換えた入力と同じバイト列（シリアル番号と CRC を除く）、3〜8 チャンネルの並びを lewton と仕様の表で確かめる（`tests/ogg_vorbis.rs` と `codec::vorbis::encoder` のテスト） |
| E29 | Platform・External のコーデックを通った版は、デコードのダイジェストとコーデックの識別子（と環境の版）が記録され、変わったら検出される（失敗ではなく食い違いとして返す）。アンカーがあればアンカーから作り直し、出力が記録と同じになる。壊れたアンカーは見分ける（`tests/e29.rs`。偽の Platform のデコーダで Linux でも回る） |
| E30 | §16 の性能を、`tests/bench_edit.rs`（`#[ignore]`）と `kura-devtools bench edit` で測り、§16 と architecture.md §13 に判定つきで記録する。NFR-11・12 の時間は表示の経路（`EditorPath::show`・`render_preview` → `Engine::render_view`。写真は段 2 の計画で、段 1 も記録）で測り、NFR-16 は同じエディタの経路で等倍の全体を描いたダイジェスト（`render_cached`。`EditorPath::digests`）を一括の経路のものと照らす。テストが断言するのは正しさだけ（エディタの経路と一括の経路のダイジェストが同じ、予算を超える計画は描く前に `Limit`、見積もりが実際以上）で、時間は表に出すだけ（段 10 で書き換えた。`e30_preview`・`e30_stroke`・`e30_save_bulk_analysis`・`e30_memory`、kura-devtools の `quick_writes_a_record_for_every_nfr`。目標の範囲の外（32px を超える筆・減色のプレビュー）を達成・未達にしないことは `out_of_scope_records_are_not_judged`） |
| E31 | 変わった範囲の推論は保守的：ランダムな文書へのランダムな編集の前後を全体で描いた差が、推論した範囲（`Engine::delta`）に入る。`render_cached` が報告する変わったタイル（`Arc` の同一性で絞ったもの）は、中身の変わったタイルを含み、推論の範囲の中にある（段 10。`tests/delta.rs` の `inferred_delta_contains_actual`・`changed_tiles_by_identity`。局所の編集が全体に落ちないことは `local_edits_do_not_fall_to_all`、継ぎはぎが毎回まっさらに描いたものと同じことは `incremental_equals_fresh`）。ランダムな手（`testing::region` の `random_edit`・`random_step`。種類は `EDIT_KINDS`・`STEP_KINDS`）は、ノード 1 つの引数・描画への追記・途中の動作の差し替え・有効の切り替え・フェードの長さ・ループ点・メタデータ・canvas の fill だけ・種だけと、線形の列の構造の手（置き換え・消す・動かす・足す）、2〜3 手をまとめて途中の版を飛ばすもの（`multi`）、入力の値だけの変更（`random_step_or_input`）。決まった場合：Indexed8 の上流でパレットだけが変わると後ろは全体（`palette_changes_upstream_stay_all`）、元・エンジン・段の違いで全体（`delta_falls_to_all_for_other_sources_engines_or_levels`）、持ち上げた canvas の fill だけの変更が全フレームに広がる（`lifted_param_change_spans_all_frames`）、前の値で無効だったタイルを同じ実体でも報告する（`refine_reports_tiles_that_were_holes`）、継ぎはぎが前の値のパレットの違いで断ったら前の値なしで描き直す（`render_area_redraws_without_a_base_of_another_palette`）。段 10 の検証で抜けを見つけた種でも回す（`inferred_delta_contains_actual_on_found_seeds`）。表示の経路（`render_view`。§12.8）の報告も、表示する範囲の中で中身の変わったタイルを含み、推論と「前回は無効で今回描いたタイル」の中にあり、表示する範囲のタイルは `render` と同じ（`tests/view.rs` の `view_equals_render_on_random_edit_sequences`・`view_equals_render_on_found_seeds`・`view_redraws_only_changed_tiles_after_strokes`・`view_at_levels_equals_the_level_render`・`view_follows_a_moving_viewport`・`view_handles_palette_changes_upstream`）。表示の値から配信に要るもの（形式・パレット・メタデータ・フレームの表示時間と繰り返し）が取れる（`view_values_expose_what_delivery_needs`） |
| E32 | プレビューの段：段 0（と段 0 に落ちた計画）は `render` とビット単位で同じ。段 1〜3 はスレッドの数によらず同じで、縮めた引数が段の形で推論を通る（通らなければ段 0 に落ちる）。継ぎ目までのノードは、等倍の計画と同じ指紋・同じ値。段の値は保存・解析・スポイト・ダイジェストに使えない（段 10。`tests/preview.rs` の `level0_equals_render`・`preview_deterministic`・`scaled_params_infer`・`prefix_before_switch_is_exact`・`level_values_are_not_for_saving`・`fallback_reasons`・`choose_level_rules`。今の登録表の操作では届かない段 0 に落ちる理由（`Infer`・`Analysis`）と Skip の操作を外す道は、試験用の操作で `fallback_reasons_of_level_nodes`。近似の程度は `preview_approximates`） |
| E33 | `EditSession`：素朴な参照の実装（文書を丸ごと写す）と、ランダムな編集の列のどの手のあとでも、同じ文書（正規の JSON）・Rev・取り消しの可否・履歴の長さ・誤りの種類・鍵になる。同じ列を 2 回当てると同じ Rev・鍵・文書。まとめ方・仮の値・上限・構造の手・検証（E8・E25）が約束どおりで、頭の文書から作り直すと同じ文書・同じ鍵になり、`to_graph` を compile・render すると素直に組んだ文書と同じ値（段 10。`tests/session.rs` の `matches_the_naive_model_on_random_sequences`・`same_sequence_gives_same_revs_keys_and_documents`・`reopening_from_the_head_gives_the_same_document_and_keys`・`to_graph_compiles_and_renders_like_the_plain_document` ほか） |
| E34 | 文書の部品（`doc_parts`。§4.4。M12 の段 2）の形は凍結：決めた文書の根と空の切れ目のダイジェストの 16 進が変わらない（`doc_parts::tests::parts_format_is_frozen`。変えるなら新しいドメイン（`-v2`）で別の形にする）。`PART_CHUNK` が追記だけの配列の切れ目（`session::log::CHUNK`）と同じ。分けてつなぐと文書の正規の JSON に戻り、分け直しても同じ組になる（ChaCha8 のランダムな文書でも）。追記した版どうしで前の切れ目を共有する（200 本 → 210 本で新しい部品は最後の切れ目・ノード・根だけ。`appending_shares_all_but_the_last_chunk`）。壊れた部品（足りない・本文がダイジェストと合わない・種類の違い・`count` の違い・切れ目の数と並びの違い・殻が `[]` でない・余分な項目・切っていない長い配列・正規でない本文、ChaCha8 のランダムな壊し方）は `Invalid`（`BadGraph`）で落ちない、同じ切れ目を参照して膨らませる組は `Limit`（`broken_parts_are_refused`・`random_documents_round_trip_and_random_damage_is_refused`・`amplification_is_limited`）。ファズの対象 `doc_parts`（`testing::fuzz_doc_parts`。つないだ文書を分け直すと同じ根になることも見る） |
