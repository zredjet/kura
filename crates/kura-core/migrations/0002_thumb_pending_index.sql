-- 0002: サムネイル未作成のアイテムを新しい順に取り出すための部分索引（サムネイルのジョブが使う）。
-- 適用後は編集しない。
CREATE INDEX idx_items_thumb_pending ON items(imported_at DESC, id DESC) WHERE thumb_state = 'pending';
