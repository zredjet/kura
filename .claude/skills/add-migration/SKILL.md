---
name: add-migration
description: SQLite のスキーマ変更（テーブル・列・インデックスの追加など）を、マイグレーションファイル・ドキュメント・テスト込みで行う手順。DB の形を変えるときに使う。
---

変更内容は `$ARGUMENTS`。

1. `crates/kura-core/migrations/` の最大番号を確認し、次の番号で `NNNN_<短い説明>.sql` を作る。既存ファイルは編集しない。
   `crates/kura-core/src/db/migrate.rs` の `MIGRATIONS` に `plain(…)` か `rebuild(…)` で足す。
2. 既存データを壊さない SQL にする（`.claude/rules/database.md`）。
   - 列の追加は `DEFAULT` 付きの `ADD COLUMN`。CHECK 付きは全行を確かめるので、10 万件の時間を測る。
   - CHECK の値を足すだけなら、名前付きの CHECK を `ALTER TABLE … DROP CONSTRAINT …` → `ADD CONSTRAINT …` で入れ替える（作り直さない）。
   - 型や制約を変えるために表を作り直すときは、新しい表を作る → 移す → 古い表を DROP → 新しい表を RENAME。
     **ほかの表から外部キーで参照される表**（`items`・`blobs`・`folders`・`tags`・`units`・`playlists` など）は `Kind::Rebuild` にする
     （`Plain` のまま DROP すると、子の行が `ON DELETE CASCADE` で消える）。消える索引・トリガ（FTS のトリガも）は同じファイルで作り直す。
   - 新しい CHECK には名前を付ける（`<表>_<列>`・`<表>_<意味>`）。`INSERT OR REPLACE INTO items` は使わない。
3. `docs/data-model.md` の該当テーブル定義と説明（§3 のマイグレーションの表、§5 の当て方と時間）を更新する。
4. テストを追加する（`db/migrate.rs` の `mod tests`）：
   - 空の DB に全マイグレーションを適用できること
   - 1 つ前の版のスキーマに `fill_v6`（すべての表に行を入れる準備。表を足したらここにも行を足す）でデータを入れ、
     新しいマイグレーションの前後の `snapshot`（すべての表の行・索引とトリガの定義・FTS の照合）を、前からある列だけで比べて一致すること
   - 増えた表・索引・トリガがちょうど狙いのものであること
5. 新しいインデックスやクエリは `EXPLAIN QUERY PLAN` で全件スキャンにならないことを確認する。
6. 10 万件で時間を測る：`cargo run --release -p kura-devtools -- bench migrate --items 100000 --history --runs 3`
   （バックアップ・各マイグレーション・TRUNCATE の時間と、照合の結果が出る）。更新後の初回の起動に載る（一度きりの作業として NFR-01 の外で記録する。docs/data-model.md §5）。
7. `/verify` を実行する。
