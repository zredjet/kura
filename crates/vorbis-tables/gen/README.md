# vorbis-tables の生成の道具

`src/data.rs`・`src/books.rs`・`src/books.bin` は、ここの道具で aoTuV の C のソースから作り、生成物をコミットする。
ビルドのときには生成しない（Cargo の build.rs はない）。手で書き換えない。

同じ源から、自作のエンコーダの照合の基準（照合用のエンコーダ）も組む。その道具は `../oracle/`（README.md）。

## 入力（どの版のどのファイルから作ったか）

- クレート `aotuv_lancer_vorbis_sys` **0.1.6**（crates.io）の `vorbis_vendor`（**aoTuV beta 6.03（2020）**、libvorbis 1.3.7 が土台。
  `lib/info.c` の vendor は `aoTuV Beta 6.03 (2020)`）と `ogg_vendor`（libogg）。
  `gen.py` は `--src` がなければ `$CARGO_HOME/registry/src/*/`（環境変数があるとき）と `~/.cargo/registry/src/*/` から
  `aotuv_lancer_vorbis_sys-0.1.6/` を探す。このクレートは kura のワークスペース・`fuzz/`・`oracle/rlibm/` のどの依存でもないので、
  `cargo fetch` では落ちない（ほかのプロジェクトで使っていればあることがある）。なければ crates.io の
  `https://crates.io/api/v1/crates/aotuv_lancer_vorbis_sys/0.1.6/download`（tar.gz の `.crate`）を空のディレクトリに展開して
  `--src` で渡す（「再生成の手順」）。中身は `inputs.sha256` で照らすので、違う版なら止まる。
- 使うファイルと SHA-256 は `inputs.sha256`（`gen.py` が生成の前に照らし、1 つでも違えば止まる）。主なもの：

  | ファイル | SHA-256 | 何を取るか |
  |---|---|---|
  | `vorbis_vendor/lib/vorbisenc.c` | `5ebeb0ff…545a` | 先頭〜`setup_list`（型・`_mode_template`・`_map_nominal`・テンプレートの並び） |
  | `vorbis_vendor/lib/modes/*.h`（19 個。5.1 の `residue_44p51.h`・`setup_44p51.h` も `vorbisenc.c` が `#include` するので照らす。中身は移さない） | `inputs.sha256` | テンプレート・psy・floor・residue の表 |
  | `vorbis_vendor/lib/books/*/*.h`（4 個） | `inputs.sha256` | codebook |
  | `vorbis_vendor/lib/masking.h` | `988af08c…2f07` | `ATH`・`tonemasks` |
  | `vorbis_vendor/lib/window.c` | `6a1ecba1…d0ae` | `vwin64`〜`vwin8192`（float のまま） |
  | `vorbis_vendor/lib/floor1.c` | `5f6711fe…e676` | `FLOOR1_fromdB_LOOKUP` |
  | `vorbis_vendor/lib/psy.c` | `c2c38a5c…1fdae` | `stereo_threshholds`・`stereo_threshholds_X`・`set_aotuv_psy`、aoTuV の M3・M7・M9 の表（`m3n32`・`m3n44`・`m3n48`・`m3n32x2`・`m3n44x2`・`m3n48x2`・`freq_bfn128`・`freq_bfn256`・`stn_compand`・`ntfix_offset`。大きさのマクロ `M3C` も写す）（と、`FLOOR1_fromdB_LOOKUP` が floor1.c と同じことの確認） |
  | `vorbis_vendor/lib/envelope.c` | `1f70ae90…1b80` | `band_begin`・`band_end`（12 帯域） |
  | `vorbis_vendor/lib/codebook.c`・`sharedbook.c`、`ogg_vendor/src/bitwise.c` | `inputs.sha256` | `vorbis_staticbook_pack`（codebook を詰める）を組んで使う |
  | `vorbis_vendor/COPYING` | `0c72574d…8156` | `../LICENSE` に写した（aoTuV の Aoyumi と Xiph.org の著作権表示） |

## 再生成の手順

```sh
# リポジトリの根から。Python 3（標準ライブラリだけ）と C のコンパイラ（cc）が要る
python3 -I crates/vorbis-tables/gen/gen.py            # --src <aotuv_lancer_vorbis_sys-0.1.6 の場所> で場所を渡せる
cargo test -p vorbis-tables                            # 生成の記録（SHA-256）と凍結した値を照らす
```

源が cargo の置き場所にないとき（上の「入力」）は、crates.io の `.crate` を空のディレクトリに展開して `--src` で渡す
（`../oracle/README.md` §2 と同じ源。`.crate` の SHA-256 は crates.io の索引の `cksum` と同じ値）：

```sh
src=$(mktemp -d)
curl -fsSL -o "$src/aotuv_lancer_vorbis_sys-0.1.6.crate" \
  https://crates.io/api/v1/crates/aotuv_lancer_vorbis_sys/0.1.6/download
shasum -a 256 "$src/aotuv_lancer_vorbis_sys-0.1.6.crate"   # Linux は sha256sum
#   5bc4fd1a61860d2f1198b60bedd30910eaffa978f1ee6214dfb24ac70d589225 であること
tar -xzf "$src/aotuv_lancer_vorbis_sys-0.1.6.crate" -C "$src"   # aotuv_lancer_vorbis_sys-0.1.6/ ができる
python3 -I crates/vorbis-tables/gen/gen.py --src "$src/aotuv_lancer_vorbis_sys-0.1.6"
```

- 出力の最後に books.bin とデータの SHA-256 が出る。今の値：
  - `books.bin`：`ae074742261f7a6597379cb42175c39675f1dc99728cb80a88ce202b6535e084`（598 冊、82,082 バイト）
  - データ（すべての static・`SETUP_LIST`・`BOOKS` を決まった順のバイト列にしたもの）：
    `0c0025b26cd01db81ccf33455a840276e1a415fa1e575bde8bc4e5093f9d57f0`（`pub static` 249 個。テンプレート 18 個と `SETUP_LIST` を含む）
- 同じ入力なら同じバイト列になる（並びは C のヘッダの定義の順、浮動小数の 10 進は決まった規則）。
- 版を上げるとき（別の aoTuV・libvorbis）は `--write-hashes` で `inputs.sha256` を作り直し、差を見てから生成する。
  値が変わればテストの凍結した値が合わなくなる。エンコーダ（`ogg-vorbis-kura-enc`）の書くバイト列が変わるので、
  エンコーダの版も上げる。
- `--keep <dir>` で C のダンプ（`dump.json`）を残せる（調べるとき）。

## 何をしているか

1. **切り出し**：`vorbisenc.c` の先頭から `setup_list` の定義の終わりまでを文字列のまま `vorbisenc_head.inc` に、
   `floor1.c`・`psy.c`・`envelope.c` の static の表を `extracted.inc` に写す（名前の衝突する `FLOOR1_fromdB_LOOKUP` を
   改名するだけで、中身は変えない。表の大きさに使うマクロ `M3C` の `#define` の行も先頭に写す）。
2. **名前の表**：上と `modes/*.h`・`books/*.h`・`masking.h`・`window.c` の static の定義を探し、
   `{名前, ファイル, 番地, 大きさ, 配列か}` の表（`symtab.inc`）を作る。C のプログラムはポインタから名前と要素の数を引く。
3. **ダンプ**（`dump.c`）：上の 3 つの `.inc` と、手を入れていないヘッダ・`window.c` を `#include` して組み、
   `setup_list` を先頭からたどる（5.1 の `ve_setup_44_51` だけ除く）。テンプレートが指すすべての構造体・配列・codebook を
   名前つきで JSON に書く。値はコンパイラが初期化子から作ったビットのまま（浮動小数はビット列）。codebook は
   libvorbis の `vorbis_staticbook_pack` で詰めたバイト列とビット数も書く。
   - `-U__SSE__ -U__SSE2__ -U__SSE3__` で組み、どのホストでも SSE でない経路（aarch64 と同じ）にする。
4. **Rust の生成**（`gen.py`）：
   - codebook は C が詰めたバイト列を `books.bin` に並べ（各冊はバイトの境目から）、名前・範囲・ビット数を `BOOKS` に。
     ここでも同じ規則で詰め直し、C と一致することを確かめる。並びは C のヘッダの定義の順（`BookId` = 添字）。
   - 構造体・配列は `pub static`（名前は C の名前の先頭の `_` を除いて大文字）。型は `src/types.rs`（手で書いた C の型の写し）。
   - 浮動小数は最短の 10 進で書き、その 10 進を有理数で f32・f64 に正しく丸めて同じビットになることを確かめる
     （Rust の字句の読み方は正しく丸める）。
   - すべての static を決まった順のバイト列（`types.rs` の `Feed` と同じ規則）にした SHA-256 を `DATA_SHA256` に書く。
     Rust のテストが Rust の値から同じバイト列を作って照らす（10 進の書き方や型の写し方で値が変わっていないことの確認）。

## 含めるもの・含めないもの

- テンプレート：`setup_list` の 19 個から 5.1（`ve_setup_44_51`）を除いた 18 個（`SETUP_LIST`、並びは C のまま。
  6 チャンネルも uncoupled で書くため）。
- codebook：テンプレートから使われるすべて（stereo・uncoupled・floor、すべてのレート）。ビットレートの管理だけが使う
  `book_aux_managed`・`books_base_managed` の先も、テンプレートの型を C のまま写すために含める。5.1 だけが使う
  `res_books_51.h` の冊は含めない。どのテンプレートからも指されない冊も含めない。
- テンプレートから指されないがエンコーダが使う表：`_psy_info_template`・`_mode_template`・`_map_nominal`・`ATH`・
  `tonemasks`・`vwin64`〜`vwin8192`・`FLOOR1_fromdB_LOOKUP`・`stereo_threshholds(_X)`・`set_aotuv_psy`・`band_begin`・`band_end`、
  aoTuV の M3・M7・M9 の表（`m3n*`・`freq_bfn*`・`stn_compand`・`ntfix_offset`。ADR-149 の改訂で M2・M3・M5・M7・M8・M9 も移すことに
  なったので足した）。
- 含めない：ビットレートの管理、SSE の表、デコーダの表（`lookup_data.h` など）。MDCT・FFT・過渡の検出の窓の三角関数の表は、
  C も実行時に作るので、エンジン（`crate::math`）が作る。
