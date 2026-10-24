-- 0010（Plain）：items に台帳の行の ID（blob_id）を持たせる（M12。docs/data-model.md §2・§4 の「事実で引く」）。
-- 中身ごとの表（media_facts・analysis_results）は台帳の行の ID で持つので、検索の事実の条件を
-- 32 バイトのハッシュでなく整数の鍵で引く（M11 の決定 17。一致が 6〜9 万件の広い条件を 50ms 以内に）。
-- blob_id は items.hash の写し。CHECK も外部キーも付けない（付けると全行の確かめと、台帳の行を消すたびの索引が要る。
-- 正は hash の外部キー）。AFTER のトリガで hash から引いて入れる（BEFORE のトリガは NEW を書き換えられない。
-- 台帳への登録は 0007 の BEFORE のトリガが先に済ませている）。食い違いは設定の「いま確かめる」（整合性チェックの Full）で直す。適用後は編集しない。

ALTER TABLE items ADD COLUMN blob_id INTEGER;

UPDATE items SET blob_id = (SELECT b.id FROM blobs b WHERE b.hash = items.hash);

CREATE INDEX idx_items_blob ON items(blob_id);

CREATE TRIGGER items_blob_id_ai AFTER INSERT ON items BEGIN
  UPDATE items SET blob_id = (SELECT b.id FROM blobs b WHERE b.hash = new.hash) WHERE id = new.id;
END;
CREATE TRIGGER items_blob_id_au AFTER UPDATE OF hash ON items BEGIN
  UPDATE items SET blob_id = (SELECT b.id FROM blobs b WHERE b.hash = new.hash) WHERE id = new.id;
END;
