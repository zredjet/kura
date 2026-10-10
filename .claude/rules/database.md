---
paths:
  - "crates/kura-core/src/db/**"
  - "crates/kura-core/migrations/**"
---

# データベースのルール

- スキーマの正は `docs/data-model.md`。変更するときは同じ作業で docs も更新する。
- スキーマ変更は `crates/kura-core/migrations/NNNN_説明.sql` を新規追加する。適用済みのファイルは絶対に編集しない。
- 接続時の PRAGMA：`journal_mode=WAL`、`synchronous=NORMAL`、`foreign_keys=ON`、`temp_store=MEMORY`、`mmap_size=268435456`、`cache_size=-65536`。
- 書き込みは単一ライタースレッドに集約する。読み取りは読み取り専用接続のプールから。
- 大量挿入は 500 件程度ずつトランザクションにまとめる。
- 新しいクエリを足したら、テストで `EXPLAIN QUERY PLAN` を確認し、全件スキャン（`SCAN items`）になっていないか見る。
- 10 万件のダミーデータ（`kura-devtools seed`）で、タグ 3 つの AND 検索が 50ms 以内であることを保つ。
