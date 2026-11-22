# 照合用のエンコーダ（自作の Ogg Vorbis エンコーダの正しさの基準）

kura-engine の自作の Ogg Vorbis エンコーダ（`crates/kura-engine/src/codec/vorbis/`、ADR-149）は、
**前増幅（PREAMP）だけを切った aoTuV b6.03 の C を、FMA なし・符号付き整数のあふれを折り返す（`-fwrapv`）・超越関数を Rust の libm =0.2.16 に差し替え・入力を一度に渡す形で組んだエンコーダ**
（以下「照合用のエンコーダ」）と、ストリーム全体（識別・setup・すべての音声パケット・granulepos）でビット一致することを正しさの基準にする。
ここはその照合用のエンコーダを、手を入れていない源から組み直すための道具と差分を置く。

- C のソースの丸ごとの写しは置かない。crates.io の `aotuv_lancer_vorbis_sys` **0.1.6**（`vorbis_vendor` が aoTuV beta 6.03、
  `ogg_vendor` が libogg）の手を入れていない源に、ここの差分（`patches/`）と新しいファイル（`lib/`）を足して組む。
  源は `gen/`（vorbis-tables の生成）と同じもので、SHA-256 を照らしてから使う。
- 組んだものは既定で `target/vorbis-oracle/`（git に入らない）に出る。照合のテストはそこを環境変数 `KURA_VORBIS_ORACLE` で指して回す。
- CI では回らない（照合のテストは環境変数がなければ何もせずに通る）。移植を直したときに手元で回す（`.claude/rules/engine.md`）。

## ライセンス

aoTuV（Copyright (c) 2003-2021 Aoyumi）と libvorbis・libogg（Copyright (c) 2002-2020 Xiph.org Foundation）は BSD-3-Clause。
ここの差分（`patches/`）・新しいファイル（`lib/`）・道具（`tools/` ほか）はそれを元にした・それと組むもので、このディレクトリのファイルは
すべてこのクレートの `../LICENSE`（aoTuV の `COPYING` の写し。BSD-3-Clause）の下に置く。組むときは源の `vorbis_vendor/COPYING`・
`ogg_vendor/COPYING` も出力先の `csrc/` に写す。`rlibm/` が使う `libm` は MIT OR Apache-2.0。どれも配布する .app には入らない（テストの道具だけ）。

---

## 0. 要点

- **組み方**：`python3 -I crates/vorbis-tables/oracle/build.py` 一つ（§2）。源の SHA-256 を照らし、差分を当て、`rlibm`（Rust の staticlib）と
  C を組んで `target/vorbis-oracle/bin/` に `oracle_enc` ほかを出す。Apple Silicon で 10 秒ほど。
- **回し方**：`KURA_VORBIS_ORACLE=target/vorbis-oracle cargo test --release -p kura-engine --lib codec::vorbis`（§3.1）。
- **機能の組**：環境変数なしで `PREAMP` だけを切った状態（aoTuV の M1〜M9・12 帯域の envelope・aoTuV の ATH と hypot はすべて入ったまま）。
  6 ch も uncoupled（5.1 の専用のテンプレートを使わない）。超越関数は Rust の libm =0.2.16、`-ffp-contract=off`、`-fwrapv`
  （符号付き整数のあふれは 2 の補数の折り返し。§2・§7.8）、入力は全体を 1 回で渡してから `vorbis_analysis_wrote(0)`。
- **確かめたこと**（§7）：作業領域で、372 ケース（9 レート × 1/2/6/8 ch × q −1/0/5/10 × 素材 2 種＋端の場合 84）について、
  `-O2` と `-O0`・ASan＋UBSan・2 回目・ダンプの有無・malloc を汚す、がすべてバイト一致（ふつうの振幅の入力での話。標本の絶対値が
  およそ 1e9 を超えると C の `int` があふれ、`-fwrapv` なしでは `-O2` と `-O0` が違う。今は `-fwrapv` で組むので一致する。§7.8）。lewton・symphonia・ffmpeg で 372/372 が誤りなく読めた。
  手入れ（KURA_PORT）が計算に触れていないこと（手を入れていない写しのビルドと 40/40 一致）と、超越関数の差し替えの効き目も確かめた。
  リポジトリのここから組み直したものは、作業領域のものと 372/372 でパケットがバイト一致し、照合のテスト（`codec::vorbis`）がすべて通った（§7.6）。
- **確かめたのは Apple Silicon（arm64）の macOS と Apple clang だけ**。x86_64 で組むと aoTuV の SSE の経路が入り、別のエンコーダになる（§9）。
  kura の移植（Rust）のほうは x86_64（Rosetta）でも、この arm64 の C とビット一致する。

---

## 1. 中身

| パス | 何 |
|---|---|
| `build.py` | 組む（§2）。Python 3 の標準ライブラリだけ（`python3 -I` で動く） |
| `inputs.sha256` | 源から写すファイル（73 個）の SHA-256。`build.py` が照らす。`gen/inputs.sha256` と重なるものは同じ値であることも照らす |
| `outputs.sha256` | 組み立てた C のうち源と違うファイル（差分を当てたもの 9 個・新しいファイル 4 個・codebook の名前の表）と、照合のテストの入力（3 個）の SHA-256 |
| `patches/01-abl.diff` | 調査で入れた実行時のスイッチ（`ABL(x)`）と、`_vp_psy_init` の範囲外の読み取りの修正（block.c・envelope.c・masking.h・psy.c。§5.1） |
| `patches/02-kura-port.diff` | KURA_PORT の手入れ（floor1.c・info.c・mapping0.c・psy.c・res0.c・vorbisenc.c。§5.2）。01 のあとに当てる |
| `lib/abl.h` | スイッチの本体（`KURA_ABL` の読み取り。KURA_PORT では既定を `PREAMP` にする） |
| `lib/kura_libm.h` | 超越関数の差し替え（`-include` で全部の翻訳単位に読ませる。§6） |
| `lib/kura_port.{h,c}` | ダンプの書き出し（§4.3） |
| `tools/oracle_enc.c` | 照合用のエンコーダ（§3.2） |
| `tools/oracle_setup.c` | 設定（codec_setup_info ほか）と導出の表を JSON で出す（§3.3） |
| `tools/pkt2ogg.c`・`tools/ogg2pkt.c` | パケットのファイル ⇔ Ogg（§3.4） |
| `rlibm/` | Rust の staticlib。`libm = { version = "=0.2.16", default-features = false }`（kura のワークスペースと同じ指定）の関数を `kura_exp` などの名前で出す。`Cargo.toml` に空の `[workspace]` を置いて kura のワークスペースから独立させている（lint・整形・テスト・第三者のライセンスの一覧の対象にならない）。`Cargo.lock` もここに置く |
| `scripts/pktcmp.py` | 2 つのパケットのファイルの最初の違い（§3.5） |
| `scripts/kdump.py` | ダンプの一覧・表示・比較（§3.6） |
| `scripts/mkinput.py` | 入力（planar の f32）を作る（§3.7。`build.py` も照合のテストの入力をこれで作る） |
| `scripts/check.py` | 組んだものの確かめ（作業領域の verify.py を簡略にしたもの。§3.7） |

codebook の名前の表（`kura_booknames.h`）は組むときに `books/*/*.h` から作る（コミットしない。同じ源なら同じバイト列になり、`outputs.sha256` で照らす）。

---

## 2. 組み方

```sh
# リポジトリの根から（どこからでもよい）
python3 -I crates/vorbis-tables/oracle/build.py
#   --src <aotuv_lancer_vorbis_sys-0.1.6 の場所>  既定は $CARGO_HOME と ~/.cargo の registry/src/*/ から探す
#   --out <出力先>                                既定は <リポジトリ>/target/vorbis-oracle
#   --no-san                                      AddressSanitizer＋UBSan の版を組まない
#   --syslibm                                     比較専用の oracle_enc_syslibm も組む
```

- 要るもの：Apple Silicon の macOS、Apple clang（21.0 で確認）、cargo（`libm` 0.2.16 が要る。kura のワークスペースを一度組んでいれば
  レジストリにあるのでオフラインでよい）、python3（標準ライブラリだけ）。
- 源：`aotuv_lancer_vorbis_sys-0.1.6` が `$CARGO_HOME/registry/src/`（環境変数 `CARGO_HOME` があるとき）・`~/.cargo/registry/src/` に
  なければ、crates.io の `https://crates.io/api/v1/crates/aotuv_lancer_vorbis_sys/0.1.6/download`（tar.gz の `.crate`）を空のディレクトリに
  展開して `--src` で渡す。このクレートはワークスペース・`fuzz/`・`rlibm/` のどの依存にもないので、`cargo fetch` では落ちない。
  中身は `inputs.sha256` で照らすので、違う版なら止まる。
- 手順：
  1. 源の 73 個のファイルの SHA-256 を `inputs.sha256` と照らす（1 つでも違えば止まる）。
  2. 出力先の `csrc/` に源を写し（並びは源のまま：`csrc/vorbis_vendor/lib/…`・`csrc/ogg_vendor/…`）、`patches/01-abl.diff`・
     `patches/02-kura-port.diff` を順に当てる（自前の厳密な当て方：文脈が 1 行でも合わなければ止まる。ずらしも曖昧な当て方もしない）。
     `lib/` の 4 個を `csrc/vorbis_vendor/lib/` に足し、`kura_booknames.h`（812 冊）を作る。
  3. 照合のテストが読む入力（`test/in/synth_32000_1.f32`・`synth_32000_2.f32`・`synth_192000_1.f32`。NaN が途中に出るもの。§8.3）を
     `scripts/mkinput.py synth`（3 秒、seed = rate + channels）で作る。
  4. 2・3 で作ったもの（17 個）の SHA-256 を `outputs.sha256` と照らす。
  5. `rlibm` を `cargo build --release --locked`（target は出力先の `rlibm-target/`）で組み、C を clang で組む（下の表）。
  6. C のオブジェクト（Rust の staticlib を除く）が OS の libm の超越関数（`_exp`・`_log`・`_sin`・`__sincos_stret` など）を参照していないことを
     `nm -u` で確かめる。参照していれば止まる。今の参照は `_kura_acos _kura_atan _kura_cos _kura_exp _kura_ldexp _kura_log _kura_pow _kura_sin`
     だけ（sqrt・floor・rint・fabs は命令に展開されて関数の参照は残らない）。
- 共通のフラグ：`-std=gnu99 -w -DKURA_PORT -DHAVE_STDINT_H -include csrc/vorbis_vendor/lib/kura_libm.h -ffp-contract=off -fno-fast-math -fwrapv`
  （`build.py` の `CFLAGS`・`CODEGEN`。`build-info.txt` の `cflags:` に書く）
  - `-ffp-contract=off`・`-fno-fast-math`：FMA に縮約しない・浮動小数の演算を組み替えない（移植の規則 2・3）。
  - `-fwrapv`：C の符号付き整数のあふれ（未定義動作）を 2 の補数の折り返しと定める。極端な振幅（標本の絶対値がおよそ 1e9 以上）で
    residue の量子化が `INT_MIN`・`INT_MAX` に張り付くと、res0.c・psy.c の `int` があふれる。移植（kura）はそこを wrapping の演算で
    写している（`codebook.rs`・`res0.rs`・`psy.rs` の冒頭）ので、照合の基準もそう定める。`-fwrapv` がないと `-O2` と `-O0` で出力が変わり、
    照合の基準が最適化の水準で揺れる（§7.8）。ふつうの振幅の入力では出力は変わらない（`-fwrapv` のない前の組と `check.py` の 216 ケースで一致）。
- 出力（`--out`、既定 `target/vorbis-oracle/`）

| パス | 何 |
|---|---|
| `bin/oracle_enc` | `-O2`。**照合の本体** |
| `bin/oracle_enc_O0` | `-O0`。最適化で出力が変わらないことの確認 |
| `bin/oracle_enc_san` | `-O1 -g -fsanitize=address,undefined`（UB は報告して続ける）。メモリの誤り・未定義動作の確認（`--no-san` で組まない）。`-fwrapv` で組むので符号付き整数のあふれは報告しない（定めた動き。Apple clang 21） |
| `bin/oracle_setup` | `-O2`。設定の JSON |
| `bin/pkt2ogg`・`bin/ogg2pkt` | `-O2`。Ogg との出し入れ |
| `bin/oracle_enc_syslibm` | `--syslibm` のときだけ。超越関数を差し替えない（OS の libm）。**比較専用**で照合には使わない |
| `test/in/` | 照合のテストが読む入力 |
| `csrc/`・`obj/`・`rlibm-target/` | 組み立てた C・`nm -u` の確かめのオブジェクト・rlibm の target |
| `build-info.txt` | 源の場所・clang と cargo の版・機械・共通のフラグ（`cflags:`） |

- 差分・`lib/`・源の版を変えたときは `--write-hashes` で `inputs.sha256`・`outputs.sha256` を作り直し、差を確かめてから組み直す。
  照合用のエンコーダの出力が変わるなら、照合のテスト（と、変わったなら移植）も合わせる。

---

## 3. 使い方

### 3.1 照合のテスト

```sh
python3 -I crates/vorbis-tables/oracle/build.py
KURA_VORBIS_ORACLE=target/vorbis-oracle cargo test --release -p kura-engine --lib codec::vorbis
# x86_64 の kura（Rosetta）でも同じ arm64 の C と照らせる（作業領域で確かめた。rustup target add x86_64-apple-darwin が要る）
KURA_VORBIS_ORACLE=target/vorbis-oracle cargo test --release -p kura-engine --lib --target x86_64-apple-darwin codec::vorbis
```

- `KURA_VORBIS_ORACLE` は出力先（`bin/oracle_enc`・`bin/oracle_setup`・`test/in/` がある所）。相対の道はリポジトリの根から
  （`crates/kura-engine/src/codec/vorbis/oracle.rs` の `root()`）。
- ないとき（空のときも）は、照合のテストは何もせずに通る。入れたのに `bin/oracle_enc` や `test/in/` の入力がなければ止まる。
- `--release` で回す（デバッグのビルドでは移植の側が遅い）。照合のテストはストリーム全体（`stream_tests`：レート × チャンネル × q の全組、
  数秒の入力、端の長さ、NaN が途中に出る入力、極端な振幅、区間とスレッドの組）と、モジュールごと（設定・ヘッダ・ブロック・psy・floor・residue）。
- 数：`codec::vorbis` のテストは 137 件（うち `#[ignore]` の速さの記録 `vorbis_bench_three_minutes_stereo` が 1 件）。回る 136 件のうち、
  照合用の C を呼ぶものが 53 件（`KURA_VORBIS_ORACLE` を存在しない場所にすると、この 53 件だけが `root()` の確かめで落ちる。
  `KURA_FFMPEG` がなければ 52 件）、残りの 83 件は環境変数がなくても回る（CI で回る）。
- `KURA_FFMPEG`：あれば `oracle_mux_ffmpeg_lengths` が、照合用のエンコーダのパケットを kura の Ogg に包んだものを ffmpeg で
  （既定と `-cpuflags 0` で）読み、長さが入力とちょうど同じこと、値が lewton と 1e-4 以内であることを確かめる。値が実在するファイルなら
  それを、そうでなければ `/opt/homebrew/bin/ffmpeg` を使う（どちらもなければ何もせずに通る）。照合用のエンコーダも要る。

  ```sh
  KURA_FFMPEG=/opt/homebrew/bin/ffmpeg KURA_VORBIS_ORACLE=target/vorbis-oracle \
    cargo test --release -p kura-engine --lib codec::vorbis
  ```

### 3.2 oracle_enc（照合用のエンコーダ）

```sh
bin/oracle_enc in.f32 out.pkt RATE CHANNELS Q [--vendor 文字列]
```

- `in.f32`：planar の f32 リトルエンディアン（§4.1）。`Q` は整数 −1〜10。libvorbis には `quality = (float)Q / 10.0f` を `vorbis_encode_init_vbr` で渡す。
- 入力は **全体を 1 回の `vorbis_analysis_buffer`・`vorbis_analysis_wrote(N)` で渡し、そのあと `vorbis_analysis_wrote(0)`、それからブロックを出し切る**
  （`blockout` → `vorbis_analysis` → `vorbis_bitrate_addblock` → `vorbis_bitrate_flushpacket`）。空の入力（N=0）は `wrote(0)` だけ。
- コメントのヘッダは vendor が空（長さ 0）・タグなし。`--vendor` で vendor を入れられる（移植の vendor に合わせて照らすとき）。
- 標準エラーに 1 行の要約（ブロックの大きさ・有効な KURA_ABL のビット・パケットの数・バイト数）。
- 環境変数
  - `KURA_DUMP=ファイル`・`KURA_DUMP_LEVEL=1|2`（既定 1）：ブロックごとの中間値（§4.3）。書いても出力は変わらない（§7）。
  - `KURA_ABL=...`：機能の組を上書き。**なければ `PREAMP`**（前増幅だけを切る）。`KURA_ABL=` と空にすると aoTuV の機能をすべて入れた状態、
    `KURA_ABL=M2,M3,M5,M7,M8,M9,PREAMP` で最初の決定（A′）の組。名前は M1〜M9・ENV・PREAMP・HYPOT・ATH・NN・NOTONE・NOPOINT・LONGONLY・
    ALLPOINT・NOCOMPAND（`lib/abl.h`）。
- 確かめ専用のオプション（照合には使わない）
  - `--drain-between`：`wrote(N)` と `wrote(0)` の間でもブロックを出し切る。**出力が変わることがある**（§8.2）。
  - `--chunk K`：K フレームずつ渡し、そのたびに出し切る（手を入れていない写しとの一致を確かめるため。§7.4）。

### 3.3 oracle_setup（設定の JSON）

```sh
bin/oracle_setup RATE CHANNELS Q [--look] [--bits] > setup.json
```

- `vorbis_encode_init_vbr` のあとの値：`input`（rate・channels・q・quality・有効な KURA_ABL）、`vorbis_info`（ident ヘッダの値。`bitrate_nominal` を含む）、
  `template`（選ばれた `ve_setup_*` の名前・制約）、`blocksizes`、`halfrate_flag`、`block_lowpassr`、`modes`、`maps`（mapping0 の全項目）、
  `floors`（floor1 の全項目。`posts` は数えた値）、`residues`（residue の全項目。`booklist` は `secondstages` のビットの数だけ）、
  `books`（book_param の順に、名前・ヘッダのファイル・dim・entries・使われる項目の数・最長の符号長・maptype・q_*）、
  `psy`（`vorbis_info_psy` の全項目 × psys）、`psy_global`、`highlevel`（`highlevel_encode_setup` の全項目。`block[4]` を含む）。
- `--look`：`vorbis_analysis_init` が作る導出の表も出す。`look.window`・`look.mdct`（trig・bitrev・scale）・`look.drft`（trigcache 3n・splitcache 32）・
  `look.envelope`（envelope の MDCT・`mdct_win`・12 帯域の窓と total）・`look.floor`（sorted/forward/reverse_index・hi/loneighbor・quant_q）・
  `look.psy`（ath・octave・bark・noiseoffset[3]・ntfix_noiseoffset・tonecurves[17][8][58] と aoTuV の導出のスカラー）。
  **超越関数で作る表を Rust の初期化と照らすのに使う**。
- 浮動小数：既定は f32 を `%.9g`、f64 を `%.17g`。`--bits` で f32 は `"0x%08x"`、f64 は `"0x%016llx"` の文字列。NaN・無限大は `"nan"`・`"inf"`・`"-inf"`。
- codebook の名前は `kura_booknames.h`（`books/*.h` の `static const static_codebook 名前` を拾った 812 冊）とポインタで突き合わせる。

### 3.4 pkt2ogg・ogg2pkt

```sh
bin/pkt2ogg in.pkt out.ogg [serial] [--first-alone] [--last-alone]
  # 既定は libogg のページの切り方（ident だけのページ、comment＋setup のページ、音声は pageout、最後に flush）
  # --last-alone：最後の音声パケットの前でページを閉じる。--first-alone：最初の音声パケットだけのページ（§7.2）
bin/ogg2pkt in.ogg out.pkt            # 最初の論理ストリームのパケット（granulepos はページの最後のパケットだけに載る）
```

### 3.5 pktcmp.py（パケットの照合）

```sh
python3 -I crates/vorbis-tables/oracle/scripts/pktcmp.py a.pkt b.pkt [--ignore-comment] [--ignore-granule]
# 一致：identical: 924 packets (921 audio), 341850 bytes（終了コード 0）
# 違い：packet 351 (audio #348): bytes 1058 vs 1060; first differing bit 666 (byte 83, bit 2 from LSB)（終了コード 1）
```

- パケットの番号は 0 から（0〜2 がヘッダ、3 以降が音声。`audio #k` は音声の 0 からの番号で、ダンプの BLCK の通し番号と同じ）。
- ビットの位置は Vorbis の詰め方（各バイトの最下位ビットから）で数えた通し番号。
- 移植の vendor が違うなら `--ignore-comment`（パケット 1 を比べない）。

### 3.6 kdump.py（ダンプ）

```sh
python3 -I crates/vorbis-tables/oracle/scripts/kdump.py list a.kdmp [--block N]
python3 -I crates/vorbis-tables/oracle/scripts/kdump.py show a.kdmp --block N --tag MASK [--ch C]
python3 -I crates/vorbis-tables/oracle/scripts/kdump.py diff a.kdmp b.kdmp [--max 10] [--nan-equal]   # 最初に違う記録と要素
```

### 3.7 mkinput.py（入力）・check.py（確かめ）

```sh
S=crates/vorbis-tables/oracle/scripts
python3 -I $S/mkinput.py wav in.wav out.f32
python3 -I $S/mkinput.py clips out.f32 --rate 96000 --channels 6 --seconds 6 --src a.wav,b.wav,...   # ffmpeg が要る
python3 -I $S/mkinput.py synth out.f32 --rate 192000 --channels 8 --seconds 3 --seed 1
python3 -I $S/check.py [--oracle target/vorbis-oracle] [--ref 別の出力先] [--inputs DIR] [--filter 正規表現] [--no-san]
```

- `synth` は Python の `math`（OS の libm）を使うので、別の環境では 1 ulp ほど違う入力になりうる。照合のテストの入力は `outputs.sha256` で照らす。
- `check.py`：入力ごと × q ごとに、`oracle_enc`（-O2）の出力と、`oracle_enc_O0`・もう一度・ダンプ（レベル 2）を書きながら・malloc を汚して
  （MallocPreScribble・MallocScribble）・`oracle_enc_san`・`--ref` の `oracle_enc` の出力がバイト一致するかを見る。既定の入力は合成
  （9 レート × 1・2・6・8 ch、1 秒）と端の場合（0・1・64・1,000 フレーム、無音、直流）で、出力先の `check-in/` に作る。
  組み直したものを前のものと比べるときは `--ref`。

---

## 4. 形式

### 4.1 入力（planar の f32）

チャンネル 0 の全フレーム、チャンネル 1 の全フレーム、… の順に、f32 のリトルエンディアン。フレーム数 = ファイルの大きさ ÷ (4 × チャンネル数)
（割り切れなければ誤り）。値はそのまま `vorbis_analysis_buffer` に入れる（前増幅は切ってあるので掛けない）。

### 4.2 パケットのファイル

パケットごとに、次をつなげたもの（すべてリトルエンディアン、詰め物なし）。

| 欄 | 型 | 中身 |
|---|---|---|
| len | u32 | 本体のバイト数 |
| granulepos | i64 | `ogg_packet.granulepos`（ヘッダは 0） |
| flags | u8 | ビット 0 = b_o_s、ビット 1 = e_o_s |
| body | len バイト | パケットの本体 |

最初の 3 つがヘッダ（ident・comment・setup）、そのあとが音声。音声の `packetno` は 3 から（ファイルには書かない。並びの番号と同じ）。

### 4.3 ダンプの形式（KURA_DUMP）

記録の列。記録 ＝ 12 バイトの見出し＋値（すべてリトルエンディアン、詰め物なし）。

| 欄 | 型 | 中身 |
|---|---|---|
| tag | 4 バイトの ASCII | 下の表（4 文字に満たないものは空白で埋める。`NZ  `・`NZC `） |
| ch | u16 | チャンネル。ブロック全体の記録は 0xFFFF。`RCLS` だけは「submap × 256 ＋ 束の中の番号」 |
| type | u16 | 1 = i32、2 = f32 のビット列（u32 のまま）、3 = i64 |
| len | u32 | 値のバイト数（要素の数 = len ÷ 4 か 8） |
| value | len バイト | 要素の並び |

先頭に 1 つ `KDMP`（i64 × 8：版 1・レベル・rate・channels・q・KURA_ABL のビット・フレーム数・0）。そのあとブロックごとに、`BLCK` から `BEND` までが
並ぶ（mapping0_forward の中の書く順）。

| tag | レベル | ch | 型・要素数 | 中身（書く場所） |
|---|---|---|---|---|
| `BLCK` | 1 | − | i64 × 10 | 通し番号（0 から）・W・lW・nW・blocktype（libvorbis の値：短 0 impulse／1 padding、長 0 transition／1 long）・block_mode（= blocktype \| W<<1。aoTuV の 0 impulse・1 padding・2 transition・3 long）・granulepos（`vb->granulepos`）・packetno（`vb->sequence`、3 から）・n（ブロックの大きさ）・eofflag |
| `PCM0` | 2 | c | f32 × n | 窓を掛ける前の pcm（`_postnoise_detection` の直前） |
| `PCMW` | 2 | c | f32 × n | 窓を掛けたあと（`_vorbis_apply_window` の直後） |
| `MDCT` | 2 | c | f32 × n/2 | `mdct_forward` の出力 |
| `LFFT` | 2 | c | f32 × n/2 | logfft（FFT のパワーの dB。`local_ampmax` を決めたあと） |
| `AMPL` | 2 | c | f32 × 1 | local_ampmax（0 で頭打ちしたあと） |
| `AMPG` | 2 | − | f32 × 1 | global_ampmax（全チャンネルのあと） |
| `LMDC` | 2 | c | f32 × n/2 | logmdct |
| `LCMP` | 2 | c | f32 × 1 | `lb_loudnoise_fix` の結果（M5 の `*lowcomp`。前のブロックの値を引き継ぐ。q3 以上・26 kHz 未満は −1） |
| `NOIS` | 2 | c | f32 × n/2 | `_vp_noisemask` の出力（noise、オフセット前） |
| `EPEK` | 2 | c | f32 × n/2 | epeak（`_vp_noisemask` の最後の値。M9：長い窓・過渡で tonecomp_endp まで、ほかは 0） |
| `NPEK` | 2 | c | f32 × n/2/partition | npeak（`_vp_noisemask` の最後の値。M2・M8。**`_vp_offset_and_mix` の M3 が書き換える前**。量子化に渡るのは M3 のあとの値で、ダンプには書かない） |
| `TONE` | 2 | c | f32 × n/2 | `_vp_tonemask` の出力 |
| `MASK` | 2 | c | f32 × n/2 | 最終のマスク（`_vp_offset_and_mix` のあとの logmask） |
| `MDC1` | 2 | c | f32 × n/2 | `_vp_offset_and_mix` のあとの MDCT（**M1 で弱めたあとの値**。量子化に使うのはこれ） |
| `FFIT` | 1 | c | i32 × posts | `floor1_fit` の結果（0x8000 の印を含む）。floor を使わないブロック（NULL）は要素 0 |
| `FOUT` | 1 | c | i32 × posts | `floor1_encode` が符号化する値（予測との差を折り返したもの）。floor を使わないブロックは書かない |
| `FPST` | 1 | c | i32 × posts | `floor1_encode` のあとの post（mult で落とし、予測と同じ点に 0x8000 の印） |
| `NZ  ` | 1 | c | i32 × 1 | `floor1_encode` の戻り値（nonzero） |
| `ILOG` | 1 | c | i32 × n/2 | ilogmask（量子化した floor の 0〜255 の添字） |
| `RQPR` | 1 | c | i32 × n/2 | 量子化した residue の整数、**coupling の前**（partition ごとの per-channel の noise normalization のあと）。`lowpassr` より上は 0 |
| `RQPO` | 1 | c | i32 × n/2 | **coupling のあと**（`_vp_couple_quantize_normalize` の出力。residue の符号化に渡す値） |
| `NZC ` | 1 | c | i32 × 1 | coupling のあとの nonzero |
| `RCLS` | 1 | 下 | i32 × partvals | residue の分類（`_01class`・`_2class` の partword）。ch は「submap × 256 ＋ 束の中の番号」（res1 は nonzero のチャンネルを詰めた番号、res2 は 0 だけ） |
| `BEND` | 1 | − | i64 × 2 | パケットのバイト数・ビット数 |

- チャンネルの記録の順は「全チャンネルの PCM0〜AMPL → AMPG → チャンネルごとに LMDC〜FFIT → チャンネルごとに FOUT〜ILOG → RQPR（全チャンネル）→
  RQPO・NZC（全チャンネル）→ RCLS → BEND」。
- 量の目安：レベル 2 は 20 秒の 44.1 kHz stereo で約 108 MB、レベル 1 はその 2 割ほど。
- 移植でも同じ形式でダンプを書けば `kdump.py diff` でそのまま比べられる（f32 はビット列のまま比べる）。

### 4.4 設定の JSON

§3.3。キーの名前は C の構造体の項目名のまま。

---

## 5. 手を入れた場所と理由

**変えていないもの**：アルゴリズム・表・浮動小数の演算の順。手を入れていない源（cargo のレジストリ）は読むだけ。

### 5.1 `patches/01-abl.diff`・`lib/abl.h`（調査で入れたもの）

調査（M10 段 7）で aoTuV の機能を 1 つずつ切って効き目を測るために入れた実行時のスイッチ（`abl.h` の `ABL(x)`）。スイッチを切った状態では
手を入れていないビルドとバイト一致することを調査で確かめた。

| ファイル | 中身 |
|---|---|
| `psy.c` | M1・M2・M3（`set_m3p` のあとで `mp3.sw=0`）・M4・M5（`lb_loudnoise_fix` で −1 を返す）・M6・M7・M8・M9・HYPOT・NOPOINT/ALLPOINT・NN・NOTONE・NOCOMPAND・ATH の表の選択 |
| `psy.c` | `_vp_psy_init` の範囲外の読み取り（`noiseoff[j][17]`・`ntfix_offset[17]`。0 倍されるので結果に効かない）を 0 にした（移植の規則 10 と同じ） |
| `masking.h` | Xiph の ATH の表（ATH スイッチ用） |
| `envelope.c` | ENV（12 帯域 → 7 帯域） |
| `block.c` | PREAMP（前増幅を掛けない）・LONGONLY |

### 5.2 `patches/02-kura-port.diff`・`lib/`（KURA_PORT。どれも `#ifdef KURA_PORT` の中）

| ファイル | 変更 | 理由 |
|---|---|---|
| `abl.h` | `KURA_ABL` がないときの既定を `PREAMP` にする（あれば環境変数が優先） | ADR-149（改訂：前増幅のほかはすべて移す）の機能の組を、環境変数なしで再現するため |
| `vorbisenc.c`（`get_setup_template`） | `coupling_restriction==6`（5.1 の専用のテンプレート `ve_setup_44_51`）を飛ばす | 6 ch も uncoupled で書く（ADR-149） |
| `vorbisenc.c`（末尾） | codebook の名前の表（`kura_booknames.h`）と、選ばれたテンプレートの名前を返す `kura_setup_template` | `oracle_setup` 用。books も templates も `static` なので同じ翻訳単位に置く |
| `info.c`（`_vorbis_pack_comment`） | vendor を `vc->vendor`（NULL なら空）にする | コメントのヘッダの vendor を空にする（移植の vendor と分けて照らす） |
| `kura_libm.h`（新規、`-include`） | `<math.h>` を読んだあとに `exp(x)`→`kura_exp(x)` などの関数形式のマクロ。使わない超越関数は `#pragma GCC poison` | 超越関数を Rust の libm に差し替える（§6） |
| `kura_port.{h,c}`（新規） | ダンプの書き出し | §4.3 |
| `mapping0.c`・`floor1.c`・`res0.c`・`psy.c` | ダンプのフック（`if(KD1)`・`if(KD2)` の中で書くだけ。`psy.c` は coupling の前の整数を写す作業領域を calloc する） | §4.3。計算には触れない（ダンプの有無でパケットが同じことを全ケースで確かめた。§7） |

---

## 6. 超越関数の差し替え

- ビルドに入るソースの呼び出し（grep で数えた）：fabs 28・rint 26・sqrt 25・floor 11・**sin 10・log 10・cos 9・pow 4・exp 3・ldexp 2・atan 2・acos 2・frexp 1**・
  ceil 1・hypot 1（psytune.c の `FAST_HYPOT` だけ。ビルドに入らない）。float 版（expf など）は 0。
- 差し替えるもの：exp・log・pow・atan・sin・cos・acos・ldexp・frexp・hypot（`rlibm` が `libm::exp` などをそのまま呼ぶ。kura-engine の `crate::math` の
  exp・ln・pow・atan・sin・cos と同じ関数）。
- 差し替えないもの：sqrt・floor・ceil・fabs・rint（IEEE で一意。rint は既定の丸めの向き）。
- 実際に使われる場所（すべて初期化。ブロックごとの処理は四則・sqrt・rint・floor とビットの操作だけ）
  - `mdct.c`：log2n（`rint(log(n)/log(2))`）と trig の cos・sin
  - `smallft.c`：FFT のひねり因子の cos・sin（`drfti1`）
  - `envelope.c`：envelope の MDCT の窓と帯域の窓の sin
  - `psy.c`（shiftoc の log）、`scales.h` の `toOC`（log）・`fromOC`（exp）・`toBARK`（atan）・`fromdB`（exp）：`_vp_psy_init`・`setup_tone_curves` など
  - `sharedbook.c`：`_float32_pack`（log・ldexp）・`_float32_unpack`（ldexp）、`_book_maptype1_quantvals`（pow。整数で確かめ直す）
  - `lsp.c`（acos・cos・frexp）：floor0 用でエンコーダの経路には入らない
- `M_PI` は macOS の `<math.h>` の double。Rust の `std::f64::consts::PI` と同じ値。
- 確認：`build.py` の `nm -u`（§2）。差し替えの効き目は §7.5。

---

## 7. 確かめた結果

7.1〜7.5 と 7.7 は作業領域（M10 段 7）で回した確かめの要点。入出力（約 650 MB）と、デコーダの道具（lewton・symphonia の長さを測る小さなプログラム）・
調査の写しのビルドはリポジトリに入れていないので、そのまま再現はできない（組んだものの確かめは `scripts/check.py`）。

### 7.1 ケース

- 素材：`clips`（ffmpeg で再標本化した音楽。1 ch は guitar、2 ch は bgm、6・8 ch は 6 本の素材をチャンネルごとに 0.37 秒ずらして割り当て。6 秒）と
  `synth`（掃引・和音・雑音の断続・クリック・無音・矩形に近い大振幅。3 秒）を 8,000／11,025／16,000／22,050／32,000／44,100／48,000／96,000／
  192,000 Hz × 1・2・6・8 ch で作り、それぞれ q −1・0・5・10（288 ケース）。
- 端の場合（84 ケース）：44.1 kHz 2 ch・8 kHz 1 ch・48 kHz 6 ch・192 kHz 8 ch で、0・1・64・1,000・3,000 フレームの入力、1 秒の無音、1 秒の直流（0.999）、q −1・5・10。
- 各ケースで：`oracle_enc`（-O2）・`oracle_enc_O0`・もう一度・ダンプ（レベル 2）を書きながら・malloc を汚して・`--drain-between`（参考）・`oracle_enc_san` を回し、
  Ogg に包んで（libogg の既定と `--last-alone` の 2 通り）lewton・symphonia（gapless）・ffmpeg で読んだ。

### 7.2 結果

| 確かめ | 結果 |
|---|---|
| (a) -O2 と -O0 | **372/372 一致**（ふつうの振幅の入力。大振幅では `-fwrapv` が要る：§7.8） |
| (a) ASan＋UBSan のビルド | 372/372 一致、ASan の誤りなし（UBSan の報告は下） |
| (d) 2 回目・ダンプを書く（レベル 2）・malloc を汚す・組み直しの前後 | どれも 372/372 一致 |
| 参考：`--drain-between` | 303/372 一致、69 が違う（§8.2） |
| (b) 読めるか | lewton・symphonia・ffmpeg とも 372/372（誤りなし） |
| (b) 長さ（libogg の既定のページ） | symphonia 372/372、lewton 303/372、ffmpeg 305/372 |
| (b) 長さ（最後の音声パケットだけのページ） | **lewton・symphonia・ffmpeg とも 372/372** |
| 空の入力（0 フレーム） | ヘッダ 3＋音声 1 パケット。3 つのデコーダとも 0 フレーム |

- 長さのずれ（既定のページ）：69 件のうち 67 件は音声が全部 1 ページに収まる短いストリームで、lewton と ffmpeg の両方が長さを誤る。残り 2 件は lewton の
  不具合（長→短の移り目でページが終わると最後の切り詰めを誤る）。**最後の音声パケットの前でページを閉じる（`pkt2ogg --last-alone`）と全部なくなる**。
  効いたのは、音声のページが 2 つ以上になること（67 件。kura の切り方の A4）と、長→短の直後でページを閉じないこと（2 件。A5）の分。
  kura の検証では、最後の音声パケットだけの最後のページ（A6′）を外しても長さは変わらなかった（A6′ は規則を単純に保つための凍結。
  `crates/kura-engine/src/codec/ogg.rs` の冒頭と ADR-149 (6)）。
- UBSan の報告（どれも値には効かない、または clang がはっきり決めている動き）
  - `codebook.c`：aoTuV の自前のビット詰めが `uint32_t` を境界に揃わない番地に書く（値は変わらない）。
  - `floor1.c`（`val=-1-(val<<1)`）・`psy.c`（`((lo-1)<<16)+(hi-1)`）：負の数の左シフト。2 の補数のビット列として計算される（Rust の `<<` と同じ）。
  - `psy.c`：**NaN を int に変換**（10 件：synth 32 kHz q0 の 1・2・6・8 ch、synth 192 kHz q−1 の 1・2・6・8 ch、clips 96 kHz q10 の 6・8 ch）。§8.3。

### 7.3 テンプレート

すべての組み合わせで選べた（`oracle_setup` の `template.name`）。チャンネルが 2 なら `*_stereo`（residue 2、coupling 1 段）、1・6・8 なら `*_uncoupled`（residue 1）。

| レート | テンプレート | ブロック q−1 | q0 | q5 | q10 |
|---|---|---|---|---|---|
| 8,000 | `ve_setup_8_*` | 512/512 | 512/512 | 512/512 | 512/512 |
| 11,025 | `ve_setup_11_*` | 512/512 | 512/512 | 512/512 | 512/512 |
| 16,000 | `ve_setup_16_*` | 1024/1024 | 1024/1024 | 512/1024 | 512/1024 |
| 22,050 | `ve_setup_22_*` | 1024/1024 | 1024/1024 | 512/1024 | 512/1024 |
| 32,000 | `ve_setup_32_*` | 512/4096 | 256/2048 | 256/2048 | 256/2048 |
| 44,100 | `ve_setup_44_*` | 512/4096 | 256/2048 | 256/2048 | 256/2048 |
| 48,000 | `ve_setup_48_*` | 512/4096 | 256/2048 | 256/2048 | 256/2048 |
| 96,000・192,000 | `ve_setup_X_*` | 512/4096 | 256/2048 | 256/2048 | 256/2048 |

### 7.4 手入れが計算に触れていないこと

調査の写しのビルド（同じソースを `-O2 -ffp-contract=off`、macOS の libm、機能はすべて入り、1,024 フレームずつ渡す）の出力を `ogg2pkt` で取り出したものと、
`oracle_enc_syslibm` を `KURA_ABL=`（すべて入り）・`--chunk 1024` で回したものを `pktcmp.py --ignore-comment --ignore-granule` で比べ、素材 10 本 × q −1・0・5・10 で
**40/40 一致**（ident・setup・全部の音声パケット）。調査の写し自体は手を入れていない aoTuV とスイッチなしでバイト一致するので、KURA_PORT の手入れ
（ダンプのフック・vendor・テンプレートの選び方・名前の表）は計算を変えていない。

### 7.5 超越関数の差し替えの効き目

- `oracle_enc`（Rust の libm）と `oracle_enc_syslibm`（macOS の libm）で、§7.1 の入力すべて（372 ケース）の出力が **372/372 一致**。
- `oracle_setup --look --bits` の全部の値を 9 レート × 1・2 ch × 4 q（72 設定）で比べて **差 0**。
- MDCT の初期化と同じ引数の sin・cos 97,920 個のうち 4,452 個、log・exp・atan 60 万個のうち 26,633 個が double で違う（1 ulp 程度）が、f32 に丸めると 0 個。
- つまり今のところ macOS の libm でも同じ出力になるが、表は f32 に丸める直前の double で決まるので、OS・版が変われば揺れうる。照合用のエンコーダは
  Rust の libm を正とする（移植の `crate::math` と同じ関数）。

### 7.6 リポジトリから組み直したときの確かめ

ここ（`build.py`）から一から組んだものを、作業領域で組んで上の確かめに使ったものと比べた（Apple clang 21.0、macOS 26、arm64）。

- 組み立てた C は、作業領域の C とバイト一致（違うのは `kura_booknames.h` の 1 行目の「生成物」の注記だけ）。照合のテストの入力 3 個も一致。
- `scripts/check.py --ref <作業領域> --inputs <作業領域の test/in>`：§7.1 の 372 ケースで、`--ref` の `oracle_enc` と **372/372 がバイト一致**
  （保存してあった作業領域の出力のパケットのファイルとも 372/372 一致）。同時に -O0・2 回目・ダンプ・malloc を汚す・ASan＋UBSan も 372/372 一致。
- `oracle_setup --look --bits`：9 レート × 1・2・6・8 ch × 4 q（144 設定）で JSON が 144/144 バイト一致。`pkt2ogg`（3 通りのページの切り方）・`ogg2pkt` も 186/186 一致。
- `KURA_VORBIS_ORACLE=<組み直したもの> cargo test --release -p kura-engine --lib codec::vorbis`：**128 件すべて通った**（129 件のうち残りの 1 件は
  `#[ignore]` の速さの記録 `vorbis_bench_three_minutes_stereo`）。NaN が途中に出る入力も `test/in/` から読んで NaN に届いた。
  既定の出力先に組んで `KURA_VORBIS_ORACLE=target/vorbis-oracle`（相対の道）でも同じ。
- `scripts/check.py`（既定の入力、216 ケース）：-O0・2 回目・ダンプ・malloc を汚す・ASan＋UBSan とも 216/216 一致。

- `-fwrapv` を足して組み直したもの（M10 段 7 の検証のあと。Apple clang 21.0、arm64）：`KURA_FFMPEG`・`KURA_VORBIS_ORACLE` 付きの
  `cargo test --release -p kura-engine --lib codec::vorbis` が **135 件すべて通った**（照合用の C を呼ぶもの 53 件。極端な振幅の照合
  `oracle_stream_extreme_amplitudes` を含む）。`--target x86_64-apple-darwin`（Rosetta）の kura でも 135 件すべて通った。
  `scripts/check.py`（既定の入力、216 ケース）は -O0・2 回目・ダンプ・malloc を汚す・ASan＋UBSan とも
  216/216 一致、`--ref` で `-fwrapv` のない前の組とも 216/216 一致。

### 7.7 速さ（参考）

- `oracle_enc`（-O2）：44.1 kHz stereo 20 秒で約 0.2 秒。
- 全体を一度に渡すと libvorbis の `vorbis_analysis_blockout` がブロックごとに残りの pcm 全体を `memmove` する（block.c）ので **O(N²)**：48 kHz stereo で
  1 分 0.9 秒、3 分 5.9 秒、10 分 60 秒。出力には関係しない（移植は添字をずらすだけでよい）。照合の素材は数分までにする。

---

### 7.8 極端な振幅と `-fwrapv`（記録）

- M10 段 7 の検証で見つかった。標本の絶対値がおよそ 1e9〜1e10 以上（+180 dBFS 以上。細工した float の WAV などで届く。kura は 1 を超える
  有限の値を切らずに渡す）になると、量子化した residue が `INT_MIN`・`INT_MAX` に張り付き、C の次の所で符号付き整数があふれる
  （UBSan の報告。行番号は組み立てた `csrc/` のもの、括弧内は手を入れていない源の行）：
  - res0.c:437（436）`abs(in[j][offset+k])` の `abs(INT_MIN)`、res0.c:438（437）`ent+=abs(...)`（`_01class`）
  - res0.c:330（329）・375（374）（`local_book_besterror`）
  - psy.c:4829（4824）`q[j] = out[j]*out[j]*f[j]`（M6）、psy.c:4635–4644（4630–4639。coupling の `abs(A)>abs(B)`・`*Mag= -*Mag`）
  - res0.c:506（499）`abs(in[0][l])`（`_2class`。ステレオ）
- `-fwrapv` のない `-O2` は、ここで `-O0` と違うパケットを出した（例：振幅 1e10 の一様な雑音、8 kHz・1 ch・q−1・3,200 フレームで
  `packet 6 (audio #3): bytes 253 vs 304`。`_01class` の分類が `-O0` では 7、`-O2` では 0）。移植（kura。wrapping の演算）は `-O0` と同じ値に
  なり、`-O2` とは一致しなかった（検証の差分 1,424 件のうち 108 件）。`-fwrapv` で組んだものは `-O0`・kura と 1,424 件すべて一致した。
  そこで照合用のエンコーダは `-fwrapv` で組む（§2）。`stream_tests::oracle_stream_extreme_amplitudes`（1e9・1e10・1e12・1e30・f32::MAX の雑音と
  ±f32::MAX、1 チャンネルだけ 1e20 倍の 2・3・6 ch。24 件）がこの範囲を照らす（`-fwrapv` のない前の組では最初の件で落ちる）。
- **浮動小数 → 整数の範囲の外の変換**（psy.c:4754（4749）・4828（4823）の `out[j] = rint(res[j])`。res0.c:440（439）の `ent*=scale` も
  範囲の外になりうる）は、`-fwrapv` でも C では未定義のまま。照合用のエンコーダは aarch64 の変換（`fcvtzs`：範囲の外は飽和、NaN は 0）に
  頼っていて、それは Rust の `as` と同じ（移植の規則 2。§8.3）。`-fwrapv` で組んだ `oracle_enc_san` が大振幅の入力（1e12 の雑音・±3.4e38、
  1・2 ch）で報告したのは、この変換（psy.c:4754・4828。NaN も）と、§7.2 のいつも出る codebook.c の境界に揃わない書き込みだけ。
- §7.2 の「-O2 と -O0 が 372/372 一致」は、§7.1 の入力（ふつうの音声の振幅。1e9 よりずっと小さい）での話。

## 8. 移植で気をつけること（調査で分かったこと）

### 8.1 入力を一度に渡すことの意味（最初と最後の外挿）

- **最初**：`wrote(N)` で `N > blocksizes[1]` なら、その場で `_preextrapolate_helper`（block.c）が走る。作業長は `pcm_current = centerW + N`
  （`centerW = blocksizes[1]/2`）で、**入力の全体 N 標本を逆順にして 16 次の LPC**（`vorbis_lpc_from_data`：自己相関を double で i の昇順に足す）を作り、
  `centerW` 標本を入力の前に外挿する。つまり**最初のパケットは入力の全体に依存する**。`N ≤ blocksizes[1]` なら `wrote(0)` の中（終わりの外挿の前）で
  同じことをする。`N ≤ 32`（order×2）なら外挿しない（pcm は calloc の 0 のまま）。
- **最後**：`wrote(0)` で `eofflag = pcm_current = centerW + N`、`n = min(eofflag, blocksizes[1])` 標本から 32 次の LPC を作り、`3 × blocksizes[1]` 標本を外挿する
  （`eofflag ≤ 64` なら 0 で埋める）。
- 移植は「全体を受け取り、最初の外挿 → 最後の外挿 → ブロックの切り出し」の順にすれば、照合用のエンコーダと同じになる。
- 渡し方で変わるのは最初のパケットだけとは限らない：1,024 フレームずつ渡すと、最初の外挿の作業長と envelope の先読みが変わり、最初の数ブロックの窓の長短が
  変わることがある。そのあとのブロックの位置は前の窓の選び方で決まるので、ずれたまま最後まで続く。

### 8.2 `--drain-between` で出力が変わる理由

`wrote(N)` のあと `wrote(0)` の前にブロックを出し切ると、`blockout` が pcm を前に詰める（`pcm_current` が減る）ので、`wrote(0)` の時点の `eofflag` が小さくなり、
**最後の LPC の窓 `n = min(eofflag, blocksizes[1])` が短くなる**ことがある。その場合は最後の数ブロックの pcm（外挿の部分）が変わる。372 中 69 で違った。
照合の基準は「`wrote(0)` のあとで初めてブロックを出す」ほう。

### 8.3 NaN

- `bark_noise_hybridmp`（psy.c の非 SSE 版）の `R = (A + x*B) / D` は `D = tN*tXX − tX*tX` が 0 のとき 0/0 = NaN になりうる（`if (R < 0.f) R = 0.f;` では止まらない）。
  例：synth 32 kHz 1 ch q0 の block 13 の `NOIS[438]` が NaN、それが M1 で `MDC1[438]` にうつり、量子化で `(int)rint(NaN)` になる。
  照合のテストの `oracle_stream_nan_inputs`・`oracle_blocks_nan_inputs` が `test/in/` のこの入力を使う。
- C の `(int)` の変換は arm64（fcvtzs）では NaN→0・範囲外は飽和で、**Rust の `as` と同じ**。x86_64 の C（cvttss2si）は INT_MIN になるので、
  照合用のエンコーダは arm64 の動きを正とする。
- **NaN に触れうる所の比較・最大・最小は C の書き方のまま写す**。`os.h` の `max(x,y) ((x)<(y)?(y):(x))`・`min(x,y) ((x)>(y)?(y):(x))` は NaN をそのまま
  通すことがあり、`crate::math::max32`（NaN を捨てる）とは違う。
- NaN のビット列：arm64 の既定の NaN は 0x7fc00000、x86_64 は 0xffc00000（符号が立つ）。ダンプを比べるときは `kdump.py diff --nan-equal`。

### 8.4 そのほか、ビット一致のために

- 品質：`quality = (float)q/10.0f` を `vorbis_encode_setup_vbr` が `quality += .0000001`（float に double を足して float に戻す）、`≥ 1.0` なら `.9999`。
  `get_setup_template` は `float low, high, del` と `double req` を混ぜて `base_setting = j + del`。そのため **base_setting は整数にならない**
  （44.1 kHz の q5 で 7.000000954、q10 で 11.998999596）。補間の値は `oracle_setup` の `highlevel`・`psy` で確かめられる。
- ident ヘッダの `bitrate_nominal` は `setting_to_approx_bitrate`（rate_mapping の補間 × ch を long に）。X のテンプレート（96・192 kHz）は rate_mapping が
  −1 なので **−2（q≤5）や 0（q10）** になる。
- 負の数の左シフト（psy.c・floor1.c）は 2 の補数のビット列のまま。`psy.c` の `rate/(2*n)*i` は整数の割り算が先。
- コメントのヘッダ：照合用のエンコーダの vendor は空。移植の vendor と比べるときは `pktcmp.py --ignore-comment`、または `oracle_enc --vendor "移植の vendor"`。

### 8.5 Ogg のページ（移植の多重化の参考）

最後の音声パケットの前でページを閉じ、最後のパケットだけのページにすると、lewton・symphonia・ffmpeg の長さが 372/372 で入力と一致した（§7.2）。
libogg の既定のままだと、音声が 1 ページに収まる短いストリームで lewton と ffmpeg が長さを誤る。
効いたのは音声のページを 2 つ以上にすること（kura の A4）と、長→短の直後でページを閉じないこと（A5）で、最後のパケットだけのページ（A6′）そのものは長さに効かない（§7.2 の注）。

---

## 9. 推測・未確認

- **確かめたのは Apple Silicon（arm64）の macOS と Apple clang だけ**。`build.py` は arm64 でなければ止まる（`--allow-unverified-arch` で止まらない）。
  x86_64 で組むと aoTuV の SSE の経路（`#ifdef __SSE__`：block.c・envelope.c・mapping0.c・psy.c ほか）が入って別のエンコーダになる（`build.py` は
  SSE の経路のヘッダ `xmmlib.h` を写さないので、そのままではコンパイルで止まる）。x86_64 で使うなら `-U__SSE__ -U__SSE2__ -U__SSE3__ -U__SSE2_MATH__` などで
  SSE の経路を外す必要がある（**未確認**）。Linux（ELF）の `nm`・リンクも確かめていない。
- kura の移植（Rust）のほうは、x86_64-apple-darwin で組んだテスト（Rosetta）が arm64 のこの C を呼び、照合のテストがすべてビット一致した。
- 初期化の値の f32 への丸めで macOS の libm と Rust の libm の差が消えたのは、確かめた設定での話。ほかのレートでは違いうる（照合用のエンコーダは Rust の libm を
  使うので、照合の基準としては問題ない）。
- 未初期化のメモリの読み取りは ASan では見つからない。-O0／-O2、malloc を汚す、を全ケースで一致させたことで「出力に効く未初期化の読み取りはない」と見ているが、
  スタック（alloca）の中身を汚す確かめはしていない。
- NaN の符号に出力が依る所はコードを読んだ範囲では見つけていない。入力に NaN・無限大が入った場合は確かめていない（kura は入口で NaN を 0、
  ±∞ を ±1 にしてから渡し、照合用のエンコーダにも同じ値を渡す。`encoder.rs` の `sanitize`）。
- 10 分を超える入力は時間がかかるので回していない（§7.7）。
- `oracle_setup` の JSON の浮動小数は %.9g・%.17g。double を経由して f32 に戻すと、まれに丸めが 2 回になる可能性がある（`--bits` なら厳密）。
- 照合のテストの入力（`test/in/`）は Python の `math`（OS の libm）で作るので、別の環境では違うバイト列になりうる。そのときは `build.py` が `outputs.sha256` との
  違いで止まる。
