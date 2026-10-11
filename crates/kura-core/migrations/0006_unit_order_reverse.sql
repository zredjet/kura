-- ユニットの並びを逆にする（docs/playlists.md §2.1・§2.2、ADR-130）。今あるユニットは 0（そのまま）。
ALTER TABLE units ADD COLUMN order_reverse INTEGER NOT NULL DEFAULT 0 CHECK (order_reverse IN (0, 1));
