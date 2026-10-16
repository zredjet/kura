# ファズ（cargo-fuzz）

kura-engine の外から来る入力を読むコード（デコーダと文書の読み込み）を、カバレッジを見ながら変異させて試す。
対象の本体は `kura_engine::testing::fuzz_decode`・`fuzz_document`（自前のコードの panic・形と中身の食い違い・`Internal` を見つける）。

- nightly と cargo-fuzz が要る（`rustup toolchain install nightly`、`cargo install cargo-fuzz`）。ルートのワークスペースには入れていない。
- 回し方：`cargo +nightly fuzz run decode_png -- -max_total_time=600`（対象は `fuzz_targets/` のファイル名）。
- 種：`crates/kura-engine/tests/fuzz_smoke.rs` の `targets()` と同じものを使うとよい（`fixtures/` のファイルもそのまま種になる）。
- 見つけた入力は、`crates/kura-engine` のテスト（壊れた入力のテスト）に足してから直す。

nightly がない環境では、同じ対象を stable で変異させる `crates/kura-engine/tests/fuzz_smoke.rs` を回す：

```bash
KURA_FUZZ_SECS=60 cargo test --release -p kura-engine --test fuzz_smoke long -- --ignored --nocapture
```

- `KURA_FUZZ_ONLY=svg,wav` で対象を選べる。見つけた入力は `target/fuzz-smoke/<対象>-<回>.bin` に残る。
- 直したら、残った入力を流し直して確かめる：

```bash
cargo test --release -p kura-engine --test fuzz_smoke replay -- --ignored --nocapture
```
