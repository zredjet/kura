---
paths:
  - "crates/kura-engine/**"
---

# 操作エンジン（kura-engine）のルール

- 仕様は `docs/editing.md` に従う。特に「同じ文書・同じ元なら同じ結果」（§8）と「元に対する累積の文書」（§4.2）を崩さない。
- kura-engine は OS・DB・Tauri を知らない。`#[cfg(target_os = …)]` を書かず、macOS の機能は kura-mac が trait を実装して注入する。Linux でも `cargo test -p kura-engine` が通ること。
- 決定性の規則（`crates/kura-engine/clippy.toml` で強制）：
  - 超越関数は `crate::math` だけ（OS の libm を呼ばない）。
  - `HashMap`・`HashSet` を使わない。出力に効く順序は `BTreeMap` か並べた `Vec` で決める。
  - 並列は `determinism::par_tiles` / `reduce_blocks` だけ。浮動小数を rayon の `sum`・`reduce` で畳まない。
  - 乱数は ChaCha8 だけ。種は `determinism::seed` で、ノードの種・元のハッシュ・用途から作る。
  - 描画の中で時刻を読まない。`unsafe` は使わない（`forbid`）。
  - 結果に効く依存は Cargo.toml で `=` で版を固定する。
- 操作を足すときは `ops/<分類>/<名前>.rs` を 1 つ作り（`infer`・`apply`・`examples`・テスト）、`ops/mod.rs` の `register_ops!` に 1 行足す。メタデータは自分で写さず、`SpaceMap`・`TimeMap` を申告する。
- 結果を変える修正は操作の版を上げ、旧版を `legacy` に残す。引数の形だけの変更は `migrate` で移し、旧版のゴールデンの例が同じ値になることをテストで確かめる。
- ゴールデン（`goldens/`）は追記だけ。既存の値を書き換えない。浮動小数を含む操作の値はターゲットごとの節に置く。
- 外から来たファイルを読むコードは、確保の前にヘッダから大きさを見積もって `Limits` と予算で判断し、`try_reserve_exact` で確保する。展開は上限付きの領域にだけ行う。第三者のデコーダの境界では `catch_unwind` する。
- 新しいパーサーには、壊れた入力のテスト（ChaCha8 で作る）とファズの対象を足す。
- 受け入れテスト E1〜E30（docs/editing.md §17）は、その段で実装するものを先に失敗する形で書く。
