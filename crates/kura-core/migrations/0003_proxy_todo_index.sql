-- 0003: 再生用プロキシ（docs/architecture.md §8）。画像はプロキシを作らないので not_needed にし、
-- 判定待ち（unknown）・作成待ち（pending）のアイテムを新しい順に取り出すための部分索引を足す（プロキシのジョブが使う）。
-- 適用後は編集しない。
UPDATE items SET proxy_state = 'not_needed' WHERE kind = 'image' AND proxy_state = 'unknown';
CREATE INDEX idx_items_proxy_unknown ON items(imported_at DESC, id DESC) WHERE proxy_state = 'unknown';
CREATE INDEX idx_items_proxy_pending ON items(imported_at DESC, id DESC) WHERE proxy_state = 'pending';
