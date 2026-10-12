---
name: verify
description: Rust とフロントの整形・lint・型チェック・テストをまとめて実行し、結果を要約する。作業の完了前、コミット前、「チェックして」と頼まれたときに使う。
---

次の順に実行し、失敗したら直してから次へ進む。直せない失敗は原因と対処案を報告する。

1. `cargo fmt --all -- --check`（失敗したら `cargo fmt --all` を実行して再確認）
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `pnpm run format:check`（失敗したら `pnpm run format`）
5. `pnpm run lint`
6. `pnpm run typecheck`
7. `pnpm test`
8. `pnpm run licenses:check`（失敗したら `pnpm run licenses` で `THIRD_PARTY_LICENSES.md` を作り直す。cargo-about がなければ `cargo install --locked cargo-about`）

macOS 以外の環境で `kura-mac` や `src-tauri` がビルドできない場合は、`cargo test -p kura-core` と `cargo test -p kura-devtools` だけ実行し、その旨を明記する。

最後に次の形で 5 行以内に報告する：

```
verify: OK / NG
Rust: fmt ✓ clippy ✓ test ✓（N 件）
Front: format ✓ lint ✓ typecheck ✓ test ✓（N 件） licenses ✓
修正したこと: …
残課題: …
```
