# ファズ（cargo-fuzz）

kura-engine の外から来る入力を読むコード（デコーダと文書の読み込み・描画）を、カバレッジを見ながら変異させて試す。
対象の本体は `kura_engine::testing::fuzz_decode`・`fuzz_document`（自前のコードの panic・形と中身の食い違い・`Internal` を見つける）。
対象は `fuzz_targets/` のファイル名（`decode_png`・`decode_jpeg`・`decode_gif`・`decode_webp`・`decode_aseprite`・`decode_psd`・`decode_svg`・`decode_wav`・`document`）。

## 用意

- nightly と cargo-fuzz、カバレッジの集計には llvm-tools が要る。ルートのワークスペースには入れていない。

```bash
rustup toolchain install nightly --profile minimal --component llvm-tools-preview
```

```bash
cargo install --locked cargo-fuzz
```

- 種：`crates/kura-engine/tests/fuzz_smoke.rs` の `targets()`（各形式の小さなファイル・`fixtures/`）、デコーダのゴールデンに凍結した作り方（`FileGen`）、登録されたすべての操作の例（1 ノードの文書）。`fuzz/corpus/<対象>/` に書き出す（git には入れない）：

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
