-- 0004: プレイリスト（M6、docs/playlists.md §4・§6.5）。
-- スマートの生成日時（生成した並びはエントリに保存する）、シャッフルしないときの並び順、再開する位置のアイテム。
-- 適用後は編集しない。
ALTER TABLE playlists ADD COLUMN generated_at INTEGER;
ALTER TABLE playlists ADD COLUMN sort_key TEXT NOT NULL DEFAULT 'imported'
  CHECK (sort_key IN ('imported','captured','name','rating','duration','size'));
ALTER TABLE playlists ADD COLUMN sort_desc INTEGER NOT NULL DEFAULT 1;
ALTER TABLE playlist_state ADD COLUMN item_id INTEGER;
