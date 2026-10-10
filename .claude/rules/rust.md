---
paths:
  - "crates/**/*.rs"
  - "src-tauri/**/*.rs"
---

# Rust のルール

- エラーは `kura-core` では `thiserror` の列挙型、Tauri 境界では `AppError { code, message }` に変換して返す。
- 本番コードで `unwrap()` / `expect()` / `panic!` を使わない。
- ログは `tracing`（`info!` / `warn!` / `error!`）。`println!` は使わない。
- async の中で SQLite・ハッシュ計算・大きなファイル I/O を直接呼ばない。`spawn_blocking` か専用スレッドへ。
- DB へのアクセスは `kura_core::db` 経由のみ。SQL は `prepare_cached` を使い、文字列連結で値を埋め込まない。
- 時刻は UTC の Unix ミリ秒（`i64`）で保存・受け渡しする。
- `kura-core` に `#[cfg(target_os = "macos")]` を書かない。OS 依存は `kura-mac` の trait 実装に寄せる。
- テストは同じファイル内の `#[cfg(test)] mod tests`。DB を使うテストはインメモリ（`:memory:`）でマイグレーションを流してから行う。
- 公開関数には 1〜2 行の doc コメント（日本語可）を付ける。
- 新しい依存クレートを追加する前に、既存の依存で済まないか確認し、理由を作業報告に書く。
