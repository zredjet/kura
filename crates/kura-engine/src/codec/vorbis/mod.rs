//! Ogg Vorbis の書き出し（自作。`ogg-vorbis-kura-enc/1`。ADR-137・ADR-149）。
//!
//! 各ファイルは C の型の写しと関数（C の関数と 1 対 1）を置き、そのファイルの冒頭の `//!` に、どの C のファイルのどの関数・行を
//! 移したか、移さないもの、型の対応を書く。この冒頭は全体の地図。
//!
//! **段 7 の結合（M10）**：全ファイルの移植が終わり、照合用のエンコーダとストリーム全体（識別・setup・すべての音声パケット・
//! granulepos）でビット一致する。照らした範囲（`stream_tests`）：音声パケットは 9 レート × 1・2・3・6・8 チャンネル ×
//! q −1・0・3・5・10（全組 225 通りを 2 回、入力を変えて）、4・5・7 チャンネルとテンプレートの境のレート（12・24・50・64・
//! 88.2・176.4 kHz）は 8 件、入力は `oracle::Signal` の 6 種と直流、端の長さ（0・1・32・33・64・65・1,000・blocksizes[1] と
//! その前後）、数秒の入力、NaN が途中に出る入力、aoTuV の衝撃の入力（75 件）、極端な振幅（1e9〜±f32::MAX の雑音。24 件）、
//! 区間とスレッドの組。setup とヘッダ（識別・setup）は 9 レート × 1・2・3・6・8 チャンネル × q −1〜10 のすべて
//! （`setup.rs`・`info.rs` の照合）。q 1・2・4・6〜9 の音声パケットはリポジトリのテストでは照らしていない。
//! ブロックの解析は規則 12 のとおり並列（[`encoder::analyze_blocks`]）。
//!
//! # 作り方（ADR-149。ユーザーの決定）
//!
//! **libvorbis（aoTuV b6.03）のエンコーダの流れと表を Rust に移し、前増幅（PREAMP）だけを除いた aoTuV b6.03 と同じにする**。
//! aoTuV の独自の機能 **M1〜M9・12 帯域の envelope・aoTuV の ATH・aoTuV の hypot（`min_indemnity_dipole_hypot`）はすべて移す**。
//! 最初の決定（A′）は M2・M3・M5・M7・M8・M9 も切っていたが、品質の記録で M3（衝撃の音）と M7（トーン・短い効果音）が効くと
//! 分かったので、改訂で前増幅のほかはすべて移すことにした（エンコーダは公開前なので識別子は `ogg-vorbis-kura-enc/1` のまま）。
//! **前増幅（`vorbis_analysis_wrote` の `pre_amplitude`。音量を変えるので入れない）、ビットレートの管理（bitrate.c の管理の枝・
//! PACKETBLOBS の曲線・`floor1_interpolate_fit`）・SSE（xmmlib・各ファイルの `#ifdef __SSE__` の枝）は移さない**。VBR（品質）だけ。
//!
//! - C のソース（手を入れていないもの）：`aotuv_lancer_vorbis_sys` 0.1.6 の `vorbis_vendor/lib/`（以下 `lib/`）と
//!   `ogg_vendor/`。行番号はすべてこの手を入れていないソースのもの。
//! - 表は `vorbis-tables` クレート（BSD-3-Clause。C のヘッダから生成してコミット）。テンプレート `vorbis_tables::SETUP_LIST`
//!   （5.1 を除く 18 個）、codebook `vorbis_tables::BOOKS`・`unpack`、窓 `VWIN64`〜`VWIN8192`、`ATH`・`TONEMASKS`、
//!   `FLOOR1_FROMDB_LOOKUP`、`STEREO_THRESHHOLDS(_X)`、`SET_AOTUV_PSY`、`BAND_BEGIN`・`BAND_END` など。
//!
//! # 範囲（ADR-149）
//!
//! - チャンネル 1〜8。2 は stereo のテンプレート（coupling・residue 2）、それ以外は uncoupled（residue 1）。5.1 の専用の
//!   テンプレート（`ve_setup_44_51`）は使わず、6 チャンネルも uncoupled（`vorbis_tables::SETUP_LIST` に 5.1 がないので、
//!   `get_setup_template` は C と同じ並びで探して uncoupled に当たる）。
//! - レート 8,000〜192,000 Hz（libvorbis のすべてのレートのテンプレート：8・11・16・22・32・44.1・48 kHz と X（50〜200 kHz））。
//! - 品質は整数の q（−1〜10、既定 5）。libvorbis に渡す品質は **`q as f32 / 10.0`**（C では `(float)q / 10.f`）。
//!   照合用のエンコーダも同じ式で渡す（`vorbis_encode_init_vbr(&vi, ch, rate, (float)q/10.f)`）。
//!
//! # 正しさの基準：照合用のエンコーダ
//!
//! 前増幅だけを切った aoTuV の C を次のように組んだ「照合用のエンコーダ」と、**音声パケットがビット単位で一致**すること
//! （ヘッダは識別・setup も一致させる。コメントは vendor とタグが違うので対象外）。
//!
//! 1. 機能を切る：照合用のエンコーダの実行時のスイッチ（`crates/vorbis-tables/oracle/lib/abl.h`。調査で入れたもの）を
//!    `KURA_ABL=PREAMP`（照合用のエンコーダの既定）にする。`vorbis_analysis_wrote` で `pre_amplitude` を掛けない
//!    （block.c:495–503）。ほかの aoTuV の機能はすべて入れたまま。
//!    aoTuV のブロックをまたぐ状態（`private_state` の `mblock`（lastmdct）・`tblock`（tempmdct）・`lownoise_compand_level`・
//!    `lW_block_mode`・`lW_no`・`impadnum`）も C のとおりに持つ（[`mapping0::AotuvState`]。規則 12）。
//! 2. `-ffp-contract=off`（FMA に縮約しない）。SSE でない経路（`__SSE__` を定義しない：aarch64 で組む）。
//!    `-fwrapv`（符号付き整数のあふれを 2 の補数の折り返しと定める）：標本の絶対値がおよそ 1e9 を超えると residue の量子化が
//!    INT_MIN・INT_MAX に張り付き、res0.c の `_01class`（`abs(INT_MIN)`・`ent` の和）・`local_book_besterror`、psy.c の M6
//!    （`out*out`）・coupling で `int` があふれる。移植はそこを wrapping で写している（codebook.rs・res0.rs・psy.rs の冒頭）ので、
//!    C でもそう定める（`-fwrapv` のない `-O2` は `-O0` と違うパケットを出す。`stream_tests` の極端な振幅の照合）。浮動小数 →
//!    整数の範囲の外の変換は C では未定義のままで、aarch64 の飽和（NaN は 0。Rust の `as` と同じ）に頼る（規則 2）。
//! 3. 超越関数を Rust の `libm =0.2.16` と同じ実装に差し替える（`oracle/lib/kura_libm.h`：`sin`・`cos`・`log`・`exp`・`atan`・
//!    `acos`・`pow`・`hypot`、と `ldexp`・`frexp`。C は double の関数を呼ぶ。`ldexp`・`frexp` は IEEE で厳密なので値は変わらないが、
//!    まとめて差し替える）。`floor`・`ceil`・`rint`・`sqrt`・`fabs` は IEEE で一意なので差し替えない。
//! 4. 入力を一度に渡す：`vorbis_analysis_buffer(N)` に全部を書いて `vorbis_analysis_wrote(N)`、続けて `wrote(0)`、それから
//!    `vorbis_analysis_blockout`・`vorbis_analysis` を回す（libvorbis は渡し方で最初のパケットが変わる。block.c:553–557 の
//!    `_preextrapolate_helper` の作業長が `pcm_current`。最初の数ブロックの窓の長短が変わり、そのあとのブロックの位置がずれることも
//!    ある）。Rust 側の [`block`] は「一度に渡した」ときの C と同じ値を作る。
//!
//! 照合は aarch64 の C と、aarch64 の kura（ネイティブ）・x86_64 の kura（`--target x86_64-apple-darwin`、Rosetta）の両方で回して
//! ビット一致する（`codec::vorbis` の回るテスト 136 件（ほかに `#[ignore]` 1 件）のうち、照合用の C を呼ぶもの 53 件。そのうち `oracle_mux_ffmpeg_lengths` は
//! `KURA_FFMPEG` もあるときだけ回る）。照合用のエンコーダはリポジトリの `crates/vorbis-tables/oracle/` から組む（C のソースの
//! 写しは置かず、手を入れていない源に差分を当てる）。組み方と回し方は `crates/vorbis-tables/oracle/README.md` と `oracle.rs` の冒頭：
//!
//! ```sh
//! python3 -I crates/vorbis-tables/oracle/build.py
//! KURA_VORBIS_ORACLE=target/vorbis-oracle cargo test --release -p kura-engine --lib codec::vorbis
//! ```
//!
//! # 流れ（[`encoder`] が回す）
//!
//! ```text
//! setup::vorbis_encode_init_vbr(ch, rate, q/10)        vorbisenc.c：テンプレートを選び、codec_setup_info を埋める
//!   └ info::VorbisInfo { codec_setup: CodecSetupInfo { blocksizes, mode/map/floor/residue/book/psy の param, hi } }
//! info::vorbis_analysis_headerout                      info.c：識別・コメント（kura の vendor・タグ）・setup の 3 パケット
//! block::vorbis_analysis_init                          block.c：dsp と private_state（MDCT・FFT・envelope・psy・floor・residue の look、
//!                                                      codebook の encode 用の表）
//! block::vorbis_analysis_buffer + wrote(N) + wrote(0)  block.c：全入力、前後の LPC 外挿（lpc.c）
//! 段 0（直列）loop block::vorbis_analysis_plan        block.c の blockout：envelope.c で次の窓の長短 → ブロックの計画
//! 区間ごと（encoder::analyze_blocks）：
//!   段 0′（直列）mapping0::AotuvWindowState::next   aoTuV の窓の状態（lW_block_mode・lW_no・impadnum。block_mode の列だけで決まる）
//!   段 1（par_blocks）mapping0::mapping0_analyze      M2 の poste（psy::postnoise_detection。窓の前の PCM）→ 窓（window）→
//!                                                     MDCT（mdct）→ FFT（smallft）→ logfft・logmdct（scales::todb）
//!   段 2（直列）block::blockout_ampmax + mapping0::mapping0_ampmax   ampmax の減衰とブロックの最大（スカラーの再帰）
//!               mapping0::mapping0_lowcomp           M5 の lowcomp（psy::lb_loudnoise_fix。前の値と logmdct から）
//!               mapping0::mapping0_lastmdct          M3・M9 の lastmdct（このブロックが読む値を写し、logmdct で進める）
//!   段 3（par_blocks。M3 の tempmdct を引き継ぐ impulse の続きは 1 つの仕事の中で順に）mapping0::mapping0_encode
//!        psy::vp_noisemask（M7 の ntfix・M5・M2・M8・M9）→ psy::vp_tonemask
//!        → psy::vp_offset_and_mix（M3・M4・M1、最後に M3 の lastmdct）→ floor1::floor1_fit
//!        パケットの頭（種類・mode・前後の窓）→ floor1::floor1_encode（codebook::vorbis_book_encode）
//!        psy::vp_couple_quantize_normalize（coupling・量子化・noise normalization（M8 の npeak）・M6・M9 の epeak）
//!        res0::res1_class / res2_class → res0::res1_forward / res2_forward（codebook::local_book_besterror）
//!     → パケット（bitpack::OggpackBuffer）
//! （直列の C の形 block::vorbis_analysis_blockout → vorbis_analysis → mapping0::mapping0_forward はテストの基準として残す）
//! ogg::OggWriter（codec/ogg.rs）                       ページの切り方は kura の規則（libogg ではない）
//! ```
//!
//! # ファイルの役割（移植の単位）
//!
//! | ファイル | C | 中身 |
//! |---|---|---|
//! | `../ogg.rs` | ogg_vendor/src/framing.c（CRC だけ） | Ogg のページ（kura の切り方） |
//! | [`bitpack`] | ogg_vendor/src/bitwise.c・codebook.c:69–111 | `oggpack_write`（LSb から） |
//! | [`info`] | info.c・codebook.c・floor1.c・res0.c・mapping0.c の pack | `vorbis_info`・`codec_setup_info`、3 つのヘッダ |
//! | [`codebook`] | sharedbook.c・codebook.c・res0.c:316–378 | encode 用の codebook、符号語、VQ の最良の項目 |
//! | [`setup`] | vorbisenc.c | 品質 → テンプレート → `codec_setup_info` |
//! | [`window`] | window.c | Vorbis の窓（表は vorbis-tables） |
//! | [`mdct`] | mdct.c | 順方向の MDCT |
//! | [`smallft`] | smallft.c | 実数 FFT（`drft_forward`） |
//! | [`lpc`] | lpc.c | 前後の外挿の LPC |
//! | [`envelope`] | envelope.c | 過渡の検出（12 帯域）と窓の長短 |
//! | [`scales`] | scales.h | `todB`・`toOC`・`fromOC`・`toBARK`・`unitnorm` |
//! | [`psy`] | psy.c | 心理モデル（ノイズ・トーンのマスク、aoTuV の M1〜M9、coupling と量子化と noise normalization） |
//! | [`floor1`] | floor1.c | floor1 の look・当てはめ・符号化 |
//! | [`res0`] | res0.c | residue 1・2 の look・分類・符号化 |
//! | [`mapping0`] | mapping0.c | 1 ブロック → 1 パケット、aoTuV のブロックをまたぐ状態 |
//! | [`block`] | block.c・analysis.c・bitrate.c | dsp・入力・外挿・blockout・グラニュール |
//! | [`encoder`] | （kura） | 全体の流れ、チャンネルの並べ替え、NaN の処理、Ogg へ |
//!
//! # 移植の規則（ビット一致のため。すべてのファイルで守る）
//!
//! 1. **型をそのまま写す**：C の `float` は `f32`、`double` は `f64`、`int` は `i32`、`long` は `i64`（照合は LP64）、
//!    `ogg_int64_t` は `i64`、`ogg_uint32_t` は `u32`、codebook の長さの `char` は `u8`。構造体のフィールドの名前は C の名前の
//!    snake_case、関数の名前は C の名前の snake_case で先頭の `_` を外す（`_vp_noisemask` → `vp_noisemask`、数字で始まる
//!    `_01forward` → `forward_01`、`_2class` → `class_2`）。
//! 2. **C の暗黙の型変換をそのまま写す**（いちばん間違えやすい）：
//!    - `float` と `double` の演算は `double` で行う（`f * .5` は double、`f * .5f` は float）。リテラルの `f` の有無を見る。
//!    - `float` への代入・`float` の引数は 1 回だけ丸める（`float x = d1 * d2;` は double で掛けてから f32 に）。
//!      複合代入 `f *= 0.5` は `f = (float)((double)f * 0.5)`。
//!    - 整数と浮動小数の演算は浮動小数の側の型（`j + del`（int + float）は float で足す。例：vorbisenc.c:703 の
//!      `*base_setting=j+del` は float で足してから double に入れる）。`long` × `float` は float。
//!    - 浮動小数 → 整数は 0 へ切り捨て（`(int)x`・`int i = x;`）。範囲の外は C では未定義だが、照合用（aarch64）は飽和し
//!      NaN は 0。Rust の `as` も同じ。
//!    - `abs`・`labs` は整数、`fabs` は浮動小数（`abs()`）。
//! 3. **演算の順を変えない**：C の `a + b + c` は `(a + b) + c`。式を組み替えない、括弧を外さない、共通の部分式を
//!    まとめても順を変えない。FMA（`mul_add`・libm の `fma`・`fmaf`）は使わない（エンジンの clippy.toml は禁止していないので、
//!    このモジュールのテスト `no_fused_multiply_add` が、`vorbis/` のすべての `.rs` と `../ogg.rs` を実行時に読み、注釈を除いた
//!    コードで `.mul_add(`・`::mul_add`・`fma(`・`fmaf(` などの呼び出しを探す）。
//!    総和もループの順のまま（`Iterator::sum` を使わず、C と同じ順の `for` で足す。並列の部分和にしない）。
//! 4. **todB**（scales.h:43–51）：`(bits & 0x7fff_ffff) as f32 * 7.177_114_38e-7_f32 - 764.616_188_6_f32`（u32 → f32 の丸め、
//!    積の丸め、差の丸めの 3 回。`to_bits`）。libm を呼ばない。各所の `+ .345`（double）も C のとおり。
//! 5. **rint** は `round_ties_even()`（既定の丸めの向き）。`(int)rint(x)` は `x.round_ties_even() as i32`。C の `rint` は
//!    double の関数なので float の引数は double にしてから丸める（整数に丸めるので値は同じ）。
//! 6. **超越関数は `crate::math` だけ**（`sin`・`cos`・`ln`（C の `log`）・`exp`・`atan`・`pow`・`ldexp`）。C は double の関数を
//!    呼ぶので、float の引数は `f64` にして渡し、結果の型は C の式のとおり。`M_PI` は `std::f64::consts::PI`。超越関数を使うのは
//!    初期化（表・窓・Bark・オクターブ）だけで、ブロックごとの処理は四則・`sqrt`・`fabs`・丸め・ビット操作だけ。
//! 7. **C の `max`・`min` マクロ**（os.h:80–86：`max(x,y) ((x)<(y)?(y):(x))`）は比較の形のまま写す。`math::max32` は ±0 と NaN の
//!    扱いが違うので使わない（`f32::max` は clippy で禁止）。
//! 8. **並べ替え**：エンコーダの経路の `qsort` は floor1.c:209（`floor1_look` のポストの位置）だけ。C の `qsort` は安定でなく、
//!    同点の並びは libc で変わりうるが、ポストの位置は重ならない（vorbis-tables のテスト `floor_posts_are_distinct` で全テンプレート
//!    を確かめた）ので、どの正しい並べ替えでも同じ結果 → Rust の安定な `sort_by_key` でよい。floor1.c:170（unpack）と
//!    sharedbook.c:370（decode の init）はデコーダだけ。noise normalization（psy.c:4836）は `qsort` でなく aoTuV の部分的な選択ソート
//!    `ssort`（psy.c:4709–4726。`<` なので同点は先のものが勝ち、入れ替えで残りの並びが動く）なので、`sort` に置き換えず**そのまま写す**。
//! 9. **SSE の枝は移さない**：各ファイルの `#ifdef __SSE__ … #else … #endif` は `#else` の側（照合用も SSE なし）。
//! 10. **範囲の外の読み取り**：psy.c:500–501・504–505 は `noiseoff[j][17]`・`ntfix_offset[17]` を読む（大きさ 17。`del = 0` で
//!     0 倍されるので結果に効かない）。移植では読まない形にする（`inthalfoc + 1 < P_BANDS` のときだけ足す。照合用の patch と同じ）。
//!     M7 の `ntfix` の短い窓の枝（psy.c:3672–3675 の `nxplus > n`）は `spectral[n]` を読むが、設定のテンプレートでは通らない
//!     （`tonefix_end + 4 ≤ n`）ので、移植は範囲の外になる形では何もしない（psy.rs の冒頭）。ほかに C が初期化していない領域や
//!     範囲の外を読む所を見つけたら、移植の担当が報告する（C の振る舞いを再現できないため）。
//! 11. **エラー**：C の `exit(1)`（floor1.c:666）や「起きないはず」の枝は `EngineError::Internal`。`unwrap`・`expect`・`panic`・
//!     添字の範囲の外の panic を本番のコードに残さない（添字は `get` か、前に長さを確かめる）。`unsafe` は使わない。
//! 12. **並列**：決定性の規則のとおり `determinism::par_blocks` だけ。ブロックをまたぐ状態と、それを決める段・読む段：
//!
//!     | 状態（C） | 決まる所 | 読む所 | 依るもの |
//!     |---|---|---|---|
//!     | envelope の状態・窓の長短・グラニュール | 段 0（直列） | 段 1〜3 | 入力 |
//!     | `lW_block_mode`・`lW_no`・`impadnum`（mapping0.c:1297–1305） | 段 0′（直列） | 段 1（M2）・段 2（M5）・段 3（M3） | block_mode の列 |
//!     | `vbi->ampmax`・`g->ampmax`（block.c:657–659・mapping0.c:756・1184） | 段 2（直列） | 段 3（tonemask） | 段 1 の local_ampmax |
//!     | `lownoise_compand_level`（M5。mapping0.c:970–975） | 段 2（直列） | 段 3（noisemask・offset_and_mix） | 段 1 の logmdct |
//!     | `mblock`（lastmdct。psy.c:4461–4501） | 段 2（直列。読む値を写してから進める） | 段 3（M9・M3） | 段 1 の logmdct・block_mode・nW |
//!     | `tblock`（tempmdct。psy.c:4148–4272・4357） | 段 3 | 段 3（M3） | 前のブロックの段 3（noise・tone） |
//!
//!     tempmdct だけが前のブロックの段 3 の結果（noise・tone から決まる上書き）に依るが、触るのは 26 kHz 以上の impulse の
//!     ブロックだけで、前が impulse でなければ lastmdct から作り直す（`set_m3p`）。そこで段 3 は「続いた impulse のブロック」を
//!     1 つの仕事として順に回し、ほかは 1 ブロックずつ並列にした（区間の最初の仕事は前の区間の最後の tempmdct を引き継ぐ）。
//!     直列で C と同じ順に作って一致を確かめたうえで、ブロックの計画（envelope。直列）→ 区間ごとに段 0′（直列）→ 段 1（並列）→
//!     段 2（直列）→ 段 3（並列）に分けた（結果はスレッド数・区間の大きさによらない。encoder.rs と stream_tests.rs のテスト）。
//!     前の外挿の自己相関（入力の全体）もチャンネル × ずれごとに並列（各和の順は C のまま）。
//!
//! # kura の側で決めること（照合の対象外）
//!
//! - チャンネルの並び：kura の planes は WAV の並び。Vorbis のチャンネル `i` は `planes[ORDER[n − 1][i]]`
//!   （symphonia の `map_vorbis_channel` と同じ表。Vorbis I §4.3.9）。
//! - 入力の NaN は 0、±∞ は ±1 にしてから渡す（NaN のビットの形がターゲットで違い、todB がビットを値にするため）。1 を超える
//!   有限の値はそのまま（libvorbis と同じ）。照合用のエンコーダにも同じ値を渡す。
//! - コメントヘッダ：vendor は [`VENDOR`]（`"kura"`）。項目は E28 の約束（`symph::vorbis_comments_for` の LOOPSTART・LOOPLENGTH のあとに
//!   `ENCODER=ogg-vorbis-kura-enc/1`、そのあとにタグ。元の `ENCODER` は運ばない）。16 MiB 以上なら `Unsupported`。
//! - Ogg のページの切り方・グラニュール・シリアル番号は `codec/ogg.rs`（kura の規則）。空の音声（0 フレーム）も書く（libvorbis と
//!   同じく、短いブロックの 1 パケット）。
//! - 書くバイト列が変わる変更（表・心理モデル・ページの切り方・vendor・コメントの並び・既定の品質・NaN の扱い・libm の版）は
//!   エンコーダの版を上げる（`ogg-vorbis-kura-enc/2`）。範囲を広げるだけなら上げない。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

// C の関数と 1 対 1 に写すので引数が多い。
#![allow(clippy::too_many_arguments)]

pub(crate) mod bitpack;
pub(crate) mod block;
pub(crate) mod codebook;
pub(crate) mod encoder;
pub(crate) mod envelope;
pub(crate) mod floor1;
pub(crate) mod info;
pub(crate) mod lpc;
pub(crate) mod mapping0;
pub(crate) mod mdct;
#[cfg(test)]
pub(crate) mod oracle;
pub(crate) mod psy;
pub(crate) mod res0;
pub(crate) mod scales;
pub(crate) mod setup;
pub(crate) mod smallft;
#[cfg(test)]
mod stream_tests;
pub(crate) mod window;

use std::io::Write;

use serde_json::json;

use crate::codec::{Encoder, MetaCaps, Tier, caps_of};
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::Shape;
use crate::value::Value;

/// 書き出しの約束の版。書くバイト列が変わる変更は版を上げる（R2。旧版は残さなくてよい）。
pub(crate) const ENC_ID: &str = "ogg-vorbis-kura-enc/1";
/// コメントヘッダの vendor（時刻やアプリの版を入れない。R2）。
pub(crate) const VENDOR: &str = "kura";
/// 品質の下限（libvorbis の −0.1）。
pub(crate) const QUALITY_MIN: i64 = -1;
/// 品質の上限（libvorbis の 1.0）。
pub(crate) const QUALITY_MAX: i64 = 10;
/// 既定の品質（libvorbis の 0.5。44.1 kHz ステレオでおよそ 160 kbps）。既定を変えると `{}` のバイト列が変わる（版を上げる）。
pub(crate) const QUALITY_DEFAULT: i64 = 5;
/// 書けるサンプルレート（Hz）。
pub(crate) const RATE_MIN: u32 = 8_000;
/// 書けるサンプルレート（Hz）。
pub(crate) const RATE_MAX: u32 = 192_000;
/// 書けるチャンネルの数の上限（Vorbis I §4.3.9 の並びが決まっているのは 8 まで。symphonia も 9 以上は読めない）。
pub(crate) const CHANNELS_MAX: u16 = 8;

/// Vorbis のチャンネル `i` は kura の `planes[ORDER[n - 1][i]]`（n はチャンネルの数。symphonia の `map_vorbis_channel` と同じ表）。
pub(crate) const ORDER: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[0, 2, 1],
    &[0, 1, 2, 3],
    &[0, 2, 1, 3, 4],
    &[0, 2, 1, 4, 5, 3],
    &[0, 2, 1, 5, 6, 4, 3],
    &[0, 2, 1, 6, 7, 4, 5, 3],
];

/// 書き出しの作業領域のうち、値の大きさによらない分（§12 のメモリの予算。値は変えない）。段 10 の実測（44.1 kHz ステレオ
/// 4 秒の乱数を q5 で。読む・音量・書くの通しの /usr/bin/time -l の peak memory footprint）で、値に比例する分を約 6.3 MiB
/// 超えた（60 秒では比例の分に収まる）。
const FIXED_WORK: u64 = 8 << 20;

/// Ogg Vorbis のエンコーダ（自作。`ogg-vorbis-kura-enc/1`）。
pub struct OggVorbisEncoder;

impl Encoder for OggVorbisEncoder {
    fn id(&self) -> &'static str {
        ENC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    /// 書ける形だけ真（範囲の外は偽にして、次の段（外部のツール）へ回せるようにする）。チャンネル・レートと、長さの上限
    /// （`frames` の取りうる最大。`Dim::AtMost` も最大で見る）が [`encoder::FRAMES_MAX`] 以下。
    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "ogg"
            && matches!(shape, Shape::Audio { rate, channels, frames }
                if (1..=CHANNELS_MAX).contains(channels)
                    && (RATE_MIN..=RATE_MAX).contains(rate)
                    && frames.max() <= encoder::FRAMES_MAX as u64)
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "quality",
                ParamKind::Int {
                    min: QUALITY_MIN,
                    max: QUALITY_MAX,
                    step: 1,
                },
                Unit::None,
                json!(QUALITY_DEFAULT),
            )],
        }
    }

    /// ループ点（LOOPSTART・LOOPLENGTH）とタグ（形式の表の ogg の行と同じ）。
    fn caps(&self, format: &str, _opts: &serde_json::Value) -> MetaCaps {
        caps_of(format)
    }

    /// 値の 1.8 倍 ＋ 8 MiB：書き出しのあいだ入力全体の f32 の写し（C の `vorbis_analysis_buffer`。値と同じ大きさ）と、
    /// 区間ごとの窓・MDCT・心理モデルの作業を持つ。ピークは値の約 2.1〜2.8 倍（値を含む。§9.7：44.1 kHz ステレオ 3 分で値
    /// 64 MB に対して 168 MB）なので、大きいほうから値の 1 倍を引いた分。段 10 の実測（3 分の 44.1 kHz ステレオを q5 で、
    /// 捨てる書き出し先に書いたピークから値を引いたもの）：92 MiB（値 61 MiB の 1.5 倍）。値によらない分（[`FIXED_WORK`]）は、
    /// 短い音声で比例の分を超えた量から置いた。
    fn work_bytes(&self, _format: &str, shape: &Shape, _opts: &serde_json::Value) -> u64 {
        let v = crate::budget::value_bytes(shape);
        v.saturating_add(v.saturating_mul(4) / 5)
            .saturating_add(FIXED_WORK)
    }

    fn encode(
        &self,
        value: &Value,
        _format: &str,
        opts: &serde_json::Value,
        out: &mut dyn Write,
        mon: &dyn Monitor,
    ) -> Result<u64, EngineError> {
        let Value::Audio(audio) = value else {
            return Err(EngineError::Unsupported(
                "Ogg Vorbis には音声だけを書けます".into(),
            ));
        };
        // 設定の形（範囲・知らないキー）は呼ぶ側（`CodecSet::encode_bytes`）が `options` の表で確かめる。
        // 直接呼ばれたときのために、範囲の外は encoder::encode が `Unsupported` にする。
        let quality = match opts.get("quality") {
            None | Some(serde_json::Value::Null) => QUALITY_DEFAULT,
            Some(q) => q
                .as_i64()
                .ok_or_else(|| EngineError::Unsupported("Ogg Vorbis の品質は整数です".into()))?,
        };
        let bytes = encoder::encode(audio, quality, mon)?;
        out.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    /// 照合用の部品の自己確認：照合用のエンコーダがあれば、書いたパケットが読め、ダンプのブロックの数がパケットの数と合う。
    #[test]
    fn oracle_helper_reads_packets_and_dumps() {
        let input = super::oracle::signal(super::oracle::Signal::Music, 44_100, 2, 20_000, 1);
        let Some((packets, dump)) = super::oracle::encode(&input, 44_100, 5, Some(1)) else {
            return;
        };
        let dump = dump.unwrap();
        assert!(packets.len() > 3);
        assert_eq!(packets[0].body[0], 1);
        assert_eq!(dump.blocks.len(), packets.len() - 3);
        assert_eq!(dump.block_info(0)[0], 0);
        assert_eq!(packets.last().unwrap().granule, 20_000);
        let json = super::oracle::setup(44_100, 2, 5, false).unwrap();
        assert!(json.get("blocksizes").is_some());
    }

    use super::*;
    use crate::codec::{CodecSet, format_by_id};
    use crate::shape::Dim;

    fn audio(rate: u32, channels: u16) -> Shape {
        Shape::Audio {
            rate,
            channels,
            frames: Dim::Exact(10),
        }
    }

    /// 登録・識別子・書ける範囲・設定・能力が、決めたことと形式の表に食い違わない。
    #[test]
    fn registered_and_consistent_with_formats() {
        let set = CodecSet::pinned();
        let e = set.encoder_by_id(ENC_ID).expect("登録されている");
        assert_eq!(e.tier(), Tier::Pinned);
        assert!(e.can_encode("ogg", &audio(44_100, 2)));
        assert!(e.can_encode("ogg", &audio(8_000, 1)));
        assert!(e.can_encode("ogg", &audio(192_000, 8)));
        assert!(!e.can_encode("ogg", &audio(7_999, 1)));
        assert!(!e.can_encode("ogg", &audio(192_001, 1)));
        assert!(!e.can_encode("ogg", &audio(44_100, 9)));
        assert!(!e.can_encode("opus", &audio(48_000, 2)));
        assert!(!e.can_encode("flac", &audio(48_000, 2)));
        let ogg = format_by_id("ogg").expect("ogg の行");
        assert_eq!(e.caps("ogg", &json!({})), ogg.caps);
        assert!(ogg.caps.loop_points && ogg.caps.tags && !ogg.caps.markers);
        let schema = e.options("ogg");
        assert_eq!(schema.params.len(), 1);
        assert!(!schema.validate(&json!({ "quality": -1 })).has_errors());
        assert!(!schema.validate(&json!({ "quality": 10 })).has_errors());
        assert!(schema.validate(&json!({ "quality": -2 })).has_errors());
        assert!(schema.validate(&json!({ "quality": 11 })).has_errors());
        assert!(schema.validate(&json!({ "bitrate": 128 })).has_errors());
        // チャンネルの並べ替えの表は置換になっている
        for (n, order) in ORDER.iter().enumerate() {
            let mut sorted = order.to_vec();
            sorted.sort();
            assert_eq!(sorted, (0..=n).collect::<Vec<_>>());
        }
    }

    /// 長さの上限：`frames` の取りうる最大が `FRAMES_MAX` を超える形は書けると答えない（`Exact` も `AtMost` も）。
    /// 書けると答えた形は `encode_bytes` が次の段へ回さずこのエンコーダで書くので、`Unsupported` で終わらないように。
    #[test]
    fn can_encode_respects_frames_max() {
        let e = OggVorbisEncoder;
        let max = encoder::FRAMES_MAX as u64;
        let shape = |frames| Shape::Audio {
            rate: 48_000,
            channels: 2,
            frames,
        };
        for dim in [Dim::Exact, Dim::AtMost] {
            assert!(e.can_encode("ogg", &shape(dim(0))));
            assert!(e.can_encode("ogg", &shape(dim(max))));
            assert!(!e.can_encode("ogg", &shape(dim(max + 1))));
            assert!(!e.can_encode("ogg", &shape(dim(u64::MAX))));
        }
        // 既定の Limits の上限（48 kHz で 3 時間）は書ける範囲の中
        assert!(crate::limits::Limits::default().max_audio_frames <= max);
        // 上限を超える形は CodecSet が ogg-vorbis-kura-enc を選ばない（次の段へ回せる）
        let set = CodecSet::pinned();
        let picked = |frames| set.encoder("ogg", &shape(frames)).map(|e| e.id());
        assert_eq!(picked(Dim::Exact(max)), Some(ENC_ID));
        assert_ne!(picked(Dim::Exact(max + 1)), Some(ENC_ID));
        assert_ne!(picked(Dim::AtMost(max + 1)), Some(ENC_ID));
    }

    /// Rust のソースから注釈（`//` から行末・`/* */`）と文字列・文字のリテラルの中身を除いたもの（改行は残すので行番号は
    /// 元と同じ）。[`fma_calls`] が使う。
    fn code_only(src: &str) -> String {
        let s: Vec<char> = src.chars().collect();
        let mut out = String::with_capacity(src.len());
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut i = 0;
        while i < s.len() {
            let c = s[i];
            let next = s.get(i + 1).copied();
            if c == '/' && next == Some('/') {
                while i < s.len() && s[i] != '\n' {
                    i += 1;
                }
            } else if c == '/' && next == Some('*') {
                let mut depth = 0usize;
                while i < s.len() {
                    if s[i] == '/' && s.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if s[i] == '*' && s.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        if s[i] == '\n' {
                            out.push('\n');
                        }
                        i += 1;
                    }
                }
            } else if c == 'r'
                && (i == 0 || !ident(s[i - 1]) || (s[i - 1] == 'b' && (i < 2 || !ident(s[i - 2]))))
                && matches!(next, Some('"') | Some('#'))
            {
                // 生の文字列 r"…"・r#"…"#（br も）
                let mut j = i + 1;
                let mut hashes = 0;
                while s.get(j) == Some(&'#') {
                    hashes += 1;
                    j += 1;
                }
                if s.get(j) != Some(&'"') {
                    out.push(c);
                    i += 1;
                    continue;
                }
                out.push('"');
                j += 1;
                loop {
                    match s.get(j) {
                        None => break,
                        Some('"') if (1..=hashes).all(|h| s.get(j + h) == Some(&'#')) => {
                            j += 1 + hashes;
                            break;
                        }
                        Some('\n') => out.push('\n'),
                        Some(_) => {}
                    }
                    j += 1;
                }
                out.push('"');
                i = j;
            } else if c == '"' {
                out.push('"');
                i += 1;
                while i < s.len() && s[i] != '"' {
                    if s[i] == '\\' {
                        i += 1;
                    }
                    if s.get(i) == Some(&'\n') {
                        out.push('\n');
                    }
                    i += 1;
                }
                out.push('"');
                i += 1;
            } else if c == '\'' && next == Some('\\') {
                // 文字のリテラルのエスケープ（'\''・'\\'・'\u{…}'）：エスケープされた 1 文字を飛ばしてから閉じの ' まで
                i += 3;
                while i < s.len() && s[i] != '\'' {
                    i += 1;
                }
                out.push_str("' '");
                i += 1;
            } else if c == '\'' && s.get(i + 2) == Some(&'\'') {
                out.push_str("' '");
                i += 3;
            } else {
                out.push(c);
                i += 1;
            }
        }
        out
    }

    /// 注釈・リテラルを除いたコードの中の FMA の呼び出し（`mul_add`・`fma`・`fmaf` の識別子で、直後が `(`、または直前が `.`・
    /// `::`）。（行番号, 行）の並び。
    fn fma_calls(src: &str) -> Vec<(usize, String)> {
        let names = [
            concat!("mul", "_add"),
            concat!("fm", "a"),
            concat!("fm", "af"),
        ];
        let mut out = Vec::new();
        for (ln, line) in code_only(src).lines().enumerate() {
            let s: Vec<char> = line.chars().collect();
            let ident = |c: char| c.is_alphanumeric() || c == '_';
            let mut i = 0;
            while i < s.len() {
                if !ident(s[i]) || (i > 0 && ident(s[i - 1])) {
                    i += 1;
                    continue;
                }
                let start = i;
                while i < s.len() && ident(s[i]) {
                    i += 1;
                }
                let word: String = s[start..i].iter().collect();
                if !names.contains(&word.as_str()) {
                    continue;
                }
                let before: String = s[..start].iter().collect::<String>().trim_end().to_string();
                let after = s[i..].iter().find(|c| !c.is_whitespace());
                if after == Some(&'(') || before.ends_with('.') || before.ends_with("::") {
                    out.push((ln + 1, line.trim().to_string()));
                }
            }
        }
        out
    }

    /// 見張りの自己確認：FMA の呼び出しのいろいろな書き方を見つけ、注釈・文字列・ほかの識別子は見逃す。
    #[test]
    fn fma_calls_finds_every_form() {
        for src in [
            concat!("let y = a.mul", "_add(b, c);"),
            concat!("let y = a . mul", "_add (b, c);"),
            concat!("let y = f32::mul", "_add(a, b, c);"),
            concat!("let y = <f64>::mul", "_add(a, b, c);"),
            concat!("let f = f32::mul", "_add;"),
            concat!("let y = libm::fm", "af(a, b, c);"),
            concat!("let y = libm::fm", "a(a, b, c);"),
            concat!("use libm::fm", "a;"),
            concat!("let y = fm", "a (a, b, c);"),
            concat!("let s = \"//\"; let y = a.mul", "_add(b, c);"),
            concat!("let c = '\"'; let y = a.mul", "_add(b, c);"),
            concat!(
                "let c = '\\''; let d = '\\\\'; let y = a.mul",
                "_add(b, c);"
            ),
            concat!("/* x */ let y = fm", "af(a, b, c);"),
        ] {
            assert_eq!(fma_calls(src).len(), 1, "{src}");
        }
        for src in [
            concat!("// a.mul", "_add(b, c)"),
            concat!("//! f32::mul", "_add(a, b, c)"),
            concat!("/// libm::fm", "a(a, b, c)"),
            concat!("let y = a + b; // a.mul", "_add(b, c)"),
            concat!("let s = \"a.mul", "_add(b, c)\";"),
            concat!("let s = r#\"f32::mul", "_add(a, b, c)\"#;"),
            concat!("/* libm::fm", "af(a, b, c) */ let y = 1;"),
            concat!(
                "let y = x.fm",
                "ax(1.0); let fm",
                "a_like = 2; let mul",
                "_adder = 3;"
            ),
            concat!("fn f<'a>(x: &'a str) -> &'a str { x } // a.mul", "_add(b)"),
            "let y = a * b + c;",
        ] {
            assert!(fma_calls(src).is_empty(), "{src}");
        }
        // 行番号は元のまま（複数行の注釈・文字列のあと）
        let src = concat!(
            "/* a\nb */\nlet s = \"x\ny\";\nlet y = a.mul",
            "_add(b, c);\n"
        );
        assert_eq!(fma_calls(src).first().map(|x| x.0), Some(5));
    }

    /// 移植の規則 3：FMA（`mul_add`・libm の `fma`・`fmaf`）を使わない（照合用の C は `-ffp-contract=off`。エンジン全体の
    /// clippy.toml は `mul_add` を許しているので、このモジュールだけここで見張る）。`vorbis/` のすべての `.rs`（新しく足した
    /// ファイルも）と `../ogg.rs` を実行時に読み、注釈を除いたコードで呼び出しを探す（[`fma_calls`]）。
    #[test]
    fn no_fused_multiply_add() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/codec/vorbis");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .collect();
        files.sort();
        assert!(
            files.iter().any(|p| p.ends_with("mod.rs")) && files.len() >= 19,
            "{files:?}"
        );
        files.push(dir.join("../ogg.rs"));
        for f in &files {
            let src = std::fs::read_to_string(f).unwrap();
            let found = fma_calls(&src);
            assert!(
                found.is_empty(),
                "{} で FMA を使っている：{found:?}",
                f.display()
            );
        }
    }
}
