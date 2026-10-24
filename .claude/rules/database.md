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
  一度きりの全件の走査（起動時の指紋で pending・stale に戻す UPDATE など）は、テストに例外として理由を書く（今の例外：
  サムネイルの `thumbs.plan` の `plan_updates_scan_items_only_when_the_plan_changes`、解析の `analysis.plan`（種類と、扱いの変わった
  形式。決定 15）・`analysis.decoders` と
  投影の作り直し（`analysis.reproject` の印があるときだけ）の `sync_plan_reset_is_a_one_time_full_scan`、利用者が設定の
  「いま確かめる」を押したときだけ走る `items.blob_id` の確かめの `full_check_repairs_blob_ids_and_startup_leaves_them`。
  docs/data-model.md §4）。
  起動ごと・ジョブごとに走る問い合わせを例外にしない（印や指紋で、変わったときだけ走らせる）。
- 10 万件のダミーデータ（`kura-devtools seed`）で、タグ 3 つの AND 検索が 50ms 以内であることを保つ。

## 表を作り直すとき（docs/data-model.md §5）

- **ほかの表から外部キーで参照される表**（`items`・`blobs`・`folders`・`tags`・`units`・`playlists` など）を作り直すマイグレーションは
  `Kind::Rebuild` にする。runner がトランザクションの外で `foreign_keys` を切り、1 つのトランザクションで当て、
  `PRAGMA foreign_key_check` に新しい違反がないことを確かめてからコミットする。`Plain` のまま DROP すると、
  `ON DELETE CASCADE` で子の行が消える（トランザクションの中の `PRAGMA foreign_keys = OFF` は効かない）。
- 作り直しの手順は「新しい表を作る → 移す → 古い表を DROP → 新しい表を元の名前に RENAME」。古い表を先に改名しない
  （子の `REFERENCES` が改名後の名前を指したまま残る）。消えた索引・トリガ（FTS のトリガを含む）は同じファイルで作り直す。
- 参照されない表（`jobs` など）は `Plain` の作り直しでよい。`plain_migrations_never_drop_a_table_that_others_reference` が見張る。

## CHECK と書き込みの決まり

- CHECK には名前を付ける：列は `<表>_<列>`（`items_kind`）、表の制約は `<表>_<意味>`（`items_proxy_av`）。
  違反したときのエラーの文に名前が出る。
- 値を足すときは作り直さず、`ALTER TABLE <表> DROP CONSTRAINT <名前>; ALTER TABLE <表> ADD CONSTRAINT <名前> CHECK (…);`
  で行う（同梱の SQLite 3.53 以上。`named_checks_can_be_widened_without_a_rebuild` が版を見張る）。
- CHECK 付きの `ADD COLUMN` は全行を確かめる（10 万件で約 150ms）。起動の道に載るので、作り直すついでに足すか、CHECK なしにする。
- `INSERT OR REPLACE INTO items`（と `REPLACE INTO items`）は使わない。REPLACE は行を消してから入れるので、
  子の行が `ON DELETE CASCADE` で消える。既存の行を変えるときは `UPDATE` か `INSERT … ON CONFLICT … DO UPDATE` にする
  （`nothing_replaces_rows_of_items` が kura-core の src を走査して見張る）。
- `items` の `hash`・`ext`・`size_bytes` は台帳 `blobs` の写し（0007 のトリガ `items_blob_bi`・`items_blob_bu` が載せ、食い違えば
  `items_blob_mismatch` で断る）。台帳の行を消すのは `library::ledger::release` だけ（どの表からも参照されない行だけを消す）。
- 中身に依る書き込み（解析・サムネイル・プロキシの結果）は `WHERE id = ? AND hash = ?` のようにハッシュを条件にする
  （解析・サムネイルは M11、プロキシは M12 で入れた）。
- `items.blob_id` は台帳の行の ID の写し（0010 の AFTER のトリガ `items_blob_id_ai`・`items_blob_id_au` が hash から引いて入れる）。
  書き手は blob_id を書かない。検索・表示の読み取りで中身ごとの表（`media_facts`・`analysis_results`）を items から引くときは、
  台帳を hash で通らず `i.blob_id` で引く（一度きりの書き込み（`analysis::store::sync_decoders` など）は、写しの食い違いに
  左右されない正本の hash で台帳を通ってよい）。
