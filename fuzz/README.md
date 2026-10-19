# ファズ（cargo-fuzz）

kura-engine の外から来る入力を読むコード（デコーダと文書の読み込み・描画）を、カバレッジを見ながら変異させて試す。
自前のエンコーダの書き出し（外から来た値の標本を書く）も試す。
対象の本体は `kura_engine::testing::fuzz_decode`・`fuzz_encode`・`fuzz_document`・`fuzz_peaks`・`fuzz_model`（自前のコードの panic・形と中身の食い違い・`Internal` を見つける）。
対象は `fuzz_targets/` のファイル名（`decode_png`・`decode_jpeg`・`decode_gif`・`decode_webp`・`decode_aseprite`・`decode_psd`・`decode_svg`・`decode_wav`・`decode_aiff`・`decode_flac`・`decode_mp3`・`decode_ogg`・`decode_m4a`・`decode_aac`・`encode_ogg`・`document`・`decode_peaks`・`decode_anchor`・`model_glb`）。

- `decode_peaks`：波形の段のバイナリ（`kura-peaks`。解析 `audio.peaks` の正規の形で、kura-core がキャッシュから読み直すときのパーサー）を読み、読めたなら書き直すと同じバイト列に戻ることを確かめる（`tests/fuzz_smoke.rs` の対象の名前は `peaks`）。
- `model_glb`：3D モデル（glb。M11。`kura_engine::model`）の情報を読み（JSON の塊だけ）、焼き込み（accessor・ノードの木・埋め込みのテクスチャ）、2 つの向きで 24px に描く。情報と焼き込みの食い違い（情報が読めないのに焼き込めた・描けない印があるのに焼き込めた）と描いた画像の形も見る。種は `model::sample_glbs`（`tests/fuzz_smoke.rs` の対象の名前も `model_glb`）。
- `decode_anchor`：アンカー（段 9。PNG・APNG・f32 の WAV に私用のチャンクのヘッダを足したもの。`codec::anchor`）を読む。ヘッダのダイジェスト・正規の形・中の固定のデコーダ・値のダイジェストの照合の経路（`tests/fuzz_smoke.rs` の対象の名前は `anchor`）。

- `encode_ogg`：先頭の 19 バイト（`kura_engine::testing::FUZZ_ENCODE_HEADER`）で設定（品質 −1〜10・レート・チャンネル 1〜9・長さ 0〜16,384 フレーム）とメタデータ（ループ点：なし・(a, b)・始まりか終わりが u64 の端に近いもの。長さの外・長さ 0・逆向きを含む。タグ：0〜4 個。既知の名前とその別名（DESCRIPTION・小文字ちょうどの `cprt`）・大文字と小文字だけが違う名前・書けない名前と、空・`=` を含む・NUL・4,096 バイトの前後・70,000 バイトの値）を決め、残りを f32 のビット列（NaN・∞・巨大な値を含む）として標本にし、自作の Ogg Vorbis のエンコーダで書いて、すべての ogg のデコーダで読み直す（長さ・レート・チャンネルが元と同じか）。書けない形（レート・チャンネルの範囲の外）は断ること、書けると答えた形は断らないことも確かめる。メタデータは、書いたコメントヘッダを読んで、LOOPSTART・LOOPLENGTH が `start < end ≤ 長さ` のときだけ正しい値で書かれること、読み取りが同じタグにまとめる名前（大文字と小文字だけの違い・DESCRIPTION と COMMENT・小文字ちょうどの `cprt` と COPYRIGHT）が重複しないこと、`ENCODER=ogg-vorbis-kura-enc/1` があること、読み直したループ点が書いたものと同じこと、読み直したタグを書き直したときのコメントが書いたものと同じこと（読み取りが同じタグにまとめる名前の違いを除いて）を確かめる。見出しの並びは `kura_engine::testing::fuzz_encode` の説明、種の作り方は `fuzz_encode_input`・`fuzz_encode_input_meta`。

## 用意

- nightly と cargo-fuzz、カバレッジの集計には llvm-tools が要る。ルートのワークスペースには入れていない。

```bash
rustup toolchain install nightly --profile minimal --component llvm-tools-preview
```

```bash
cargo install --locked cargo-fuzz
```

- 種：`crates/kura-engine/tests/fuzz_smoke.rs` の `targets()`（各形式の小さなファイル・`fixtures/`・自作のエンコーダの出力・`encode_ogg` の見出しと標本）、デコーダのゴールデンに凍結した作り方（`FileGen`）、登録されたすべての操作の例（1 ノードの文書）。`fuzz/corpus/<対象>/` に書き出す（git には入れない）：

```bash
cargo test -p kura-engine --test fuzz_smoke export_seeds -- --ignored
```

## 回す

1 つの対象を回す（`fuzz/` で）：

```bash
cargo +nightly fuzz run decode_svg -- -max_total_time=1800 -rss_limit_mb=4096 -timeout=20
```

- 落ちても止めずに続けるときは `-fork=1 -ignore_crashes=1 -ignore_ooms=1 -ignore_timeouts=1` を足す。見つけた入力は `fuzz/artifacts/<対象>/` に残る。
- 既定のビルドは release ＋デバッグの検査（整数のあふれも panic になる）＋ AddressSanitizer。
- 見つけた入力は stable の再生のテストでも流せる（`target/fuzz-smoke/<形式>-<名前>.bin` に置いて `replay`）。直したら、`crates/kura-engine` のテスト（壊れた入力の回帰テスト）に足す。

## カバレッジを集計する

育った種（`fuzz/corpus/<対象>/`）で、どの行に届いたかを集計する：

```bash
cargo +nightly fuzz coverage decode_svg
```

```bash
LLVM_COV=$(rustc +nightly --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/llvm-cov; $LLVM_COV report target/aarch64-apple-darwin/coverage/aarch64-apple-darwin/release/decode_svg -instr-profile=coverage/decode_svg/coverage.profdata -ignore-filename-regex='(\.cargo/registry|rustc/|fuzz_targets)'
```

- 1 つのファイルの届いていない行（回数が 0 の行）を見る：

```bash
$LLVM_COV show target/aarch64-apple-darwin/coverage/aarch64-apple-darwin/release/decode_psd -instr-profile=coverage/decode_psd/coverage.profdata ../crates/kura-engine/src/codec/psd.rs | grep -E '^ +[0-9]+\| +0\|'
```

- 届いていないところに種で届くなら、`fuzz_smoke.rs` の種（`targets()`・`FileGen`・操作の例）を足して書き出し直す。

## nightly がないとき

同じ対象を stable で変異させる `crates/kura-engine/tests/fuzz_smoke.rs` を回す（カバレッジは見ない）：

```bash
KURA_FUZZ_SECS=60 cargo test --release -p kura-engine --test fuzz_smoke long -- --ignored --nocapture
```

- `KURA_FUZZ_ONLY=svg,wav` で対象を選べる。見つけた入力は `target/fuzz-smoke/<対象>-<回>.bin` に残る。
- 直したら、残った入力を流し直して確かめる：

```bash
cargo test --release -p kura-engine --test fuzz_smoke replay -- --ignored --nocapture
```
