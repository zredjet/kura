# アーキテクチャ

## 1. 全体像

```text
┌──────────────────────── Kura.app (Tauri 2 / macOS) ─────────────────────────┐
│ WKWebView（React）                                                          │
│   features/library  search  playlists  player  import  downloads           │
│        │ invoke（型付き, tauri-specta）     │ <img>/<video>/<audio>         │
│        │ Channel / イベント（進捗）          │  kura://localhost/...        │
├────────┼────────────────────────────────────┼────────────────────────────────┤
│ src-tauri（薄い層）                                                          │
│   commands/   protocol/kura.rs（Range 対応）   events   window（Mac 設定）    │
├────────┼────────────────────────────────────────────────────────────────────┤
│ kura-core（OS 非依存・テスト容易）                                             │
│   db/  import/  query/（検索言語）  unit/  playlist/  jobs/  download/  capture/ │
│        │                           │                                         │
│ kura-mac（macOS 専用）              │ 外部プロセス                             │
│   Swift ヘルパー：ImageIO,          │   yt-dlp（+ deno, ffmpeg）               │
│   AVFoundation, WebKit snapshot    │   gallery-dl                            │
│                                     │   ffmpeg / ffprobe（プロキシ・情報）       │
│                                     │   Google Chrome（headless, CDP）          │
└─────────────────────────────────────────────────────────────────────────────┘
           │
   Kura Library.kuralib/（library.sqlite, files/, thumbs/, proxies/, staging/ …）
```

## 2. クレート構成と責務

| クレート | 責務 | 依存してよいもの |
|---|---|---|
| `kura-core` | DB、マイグレーション、取り込み、ハッシュ、検索言語の解析と SQL 生成、ユニット、プレイリスト（純粋関数）、ジョブ管理、ダウンロード・キャプチャの手順 | rusqlite, blake3, reflink-copy, rand_chacha, serde, thiserror, tracing, tokio |
| `kura-mac` | `MediaBackend` trait の macOS 実装（サムネイル生成、メディア情報、音楽のジャケット取得、WKWebView のスナップショット、「透明度を下げる」設定の取得）と、ffmpeg との振り分け | kura-core, serde_json, swift-rs（ビルド時のリンクのみ） |
| `kura-devtools` | ダミーライブラリの生成、ベンチマーク、コマンドラインからの取り込みと確認（§13） | kura-core, kura-mac |
| `src-tauri`（`kura-app`） | コマンド、kura:// プロトコル、ウィンドウ、イベント、状態の保持 | すべて |

`kura-core` は `MediaBackend` trait（`probe`、`image_thumbnails`、`video_thumbnails`、`audio_artwork`）だけを知り、実装（`kura_mac::media_backend()`）はライブラリを開くときに注入します。テストでは偽物の実装（`media::fake::FakeBackend`）を使います。Linux 上でも `kura-core` のテストが通るようにします。

## 3. IPC コマンド一覧（初期）

戻り値はすべて `Result<T, AppError>`。`AppError { code: string, message: string }`。code の例：`NotFound`、`InvalidQuery`、`Invalid`、`ToolMissing`、`Io`、`Db`、`Conflict`、`Internal`。

| 領域 | コマンド | 説明 |
|---|---|---|
| ライブラリ | `library_create(path)` / `library_open(path)` / `library_info()` | ライブラリの作成・オープン・概要 |
| | `library_restore()` → `{ info, lastPath }` | 起動時に前回のライブラリを開く。開けなければ `info` は空で前回の場所を返す |
| | `library_check_location(path)` → `{ path, status, syncService }` | 置き場所の確認（`.kuralib` の補完、`new` / `existingLibrary` / `occupied`、同期フォルダの中か） |
| 取り込み | `import_preview(paths)` | 種類別件数・合計サイズ・重複の見込み・フォルダ木を返す |
| | `import_start(paths, options)` → `JobId` | 取り込みジョブを開始 |
| 一覧 | `items_query_ids(utcOffsetMinutes, view?, collapseUnits?, query?, sort?)` → 生バイト | 一覧の ID と縦横比を並び順で、日付見出しのグループ付きで返す。`[件数 u32][id u32 × N][縦横比×1000 u16 × N][グループ数 u32][開始位置 u32 × G][日 i32 × G]`（LE）。縦横比が分からないもの（音楽など）は 1000。日は `utcOffsetMinutes` の時差で区切った 1970-01-01 からの日数。tauri-specta を通さず `tauri::ipc::Response` で返す（フロントには ArrayBuffer で届く。ラッパーは `src/ipc/items.ts`。ADR-33） |
| | （`view`） | `{ kind: 'all' \| 'recent' \| 'unsorted' \| 'untagged' \| 'trash' }`、`{ kind: 'random', seed }`、`{ kind: 'folder', id }`（子孫フォルダの分も含む）、`{ kind: 'unit', id }`（ユニットの並び順）、`{ kind: 'smart', id }`（スマートフォルダ）。`query` は検索の文字列（docs/search-query.md。解析できなければ `InvalidQuery`）で、一覧の種類の条件に AND で重ねる。`sort` は `{ key: imported \| captured \| name \| rating \| duration \| size, desc }`。日付の見出しは追加日・撮影日で並べたときだけ（撮影日のないものは日 `-2147483648` のグループ）。ランダム・ユニットの中は見出しなし、ゴミ箱は入れた日の新しい順で、並び順は使わない |
| | （`collapseUnits` が true のとき） | set ユニットに属するアイテムを、最初に一致したメンバーの位置で 1 件のユニットにまとめる。ユニットは `0x8000_0000 \| unit_id` で表し、縦横比は表紙のもの。ゴミ箱とユニットの中ではまとめない |
| | `items_get(ids)` → `ItemSummary[]` / `units_get(ids)` → `UnitSummary[]` | 最大 200 件。要求した順で返し、ないものは省く |
| | `item_detail(id)` → `ItemDetail` | タグ・フォルダ・ユニット（set のメンバー、または参照フォルダに入っている folder ユニット）・取得元を含む |
| | `items_selection_info(ids)` → `SelectionInfo` | 複数選択のインスペクタ用：評価とメモ（全件が同じときだけ）、タグとフォルダ（付いている件数付き） |
| | `library_counts()` → `{ all, recent, unsorted, untagged, trash }` | サイドバーの件数。グリッドを出したあとに別に取る（NFR-01 に響かないように） |
| 編集 | `items_update(ids, patch)` | タイトル（1 件のときだけ）・評価・メモ（全件に同じ値）。メモの空文字は消す |
| | `items_trash(ids)` / `items_restore(ids)` → 件数 / `trash_empty()` → `{ items, bytes, failed }` | ゴミ箱。空にするときは DB の行を先に消し、そのあと実ファイル・サムネイル・ジャケット・プロキシを消す（ADR-46） |
| タグ | `tags_suggest(prefix, limit)` → `{ tag, count, alias }[]` | 前方一致 → 部分一致、使われている数の多い順。「名前空間:名前」なら名前空間で絞る。別名でも一致する |
| | `tags_apply(itemIds, add: 名前[], remove: id[])` → 付けたタグの ID | 名前は「名前」か「名前空間:名前」（全角の「：」も可、NFC）。なければ作り、別名なら正式タグに置き換える |
| | `tag_detail(id)` / `tag_update(id, patch)` / `tag_merge(from, into)` / `tag_delete(id)` | 名前空間・名前・色・別名・親タグ（`aliases`・`parents` は置き換え、循環は不可）。統合すると `from` の名前は別名として残る |
| フォルダ | `folders_tree(withCounts?)` | すべてのフォルダ（木はフロントで組む）。件数は子孫フォルダの分を含み、同じアイテムは 1 回（ADR-40）。参照する folder ユニットの ID も返す |
| | `folder_create(parentId, name)` / `folder_update(id, { name?, color? })` / `folder_move(id, parentId, beforeId)` / `folder_delete(id)` | 同じ親の下で名前は重複不可（`Conflict`）、「/」は不可。自分の中へは移せない。削除は中のフォルダごと（アイテムは残る。参照がなくなった folder ユニットも消す） |
| | `items_set_folders(itemIds, add, remove)` | フォルダへの所属の付け外し |
| ユニット | `unit_detail(id)` / `units_list(filter, kind?, limit)` | インスペクタ用の詳細、選ぶための一覧（更新の新しい順） |
| | `unit_create_set(name, itemIds)` / `unit_create_folder(name, folders)` | set は渡した順＝manual、表紙は先頭。folder は名前順 |
| | `unit_update(id, patch)` / `unit_tags_apply(id, add, remove)` | 名前・表紙・並び順・ユニット内シャッフル・画像の表示時間（0 で既定）・評価・メモ（空文字で消す）。folder は manual にできない |
| | `unit_add_items(id, itemIds)` / `unit_remove_items(id, itemIds)` / `unit_reorder(id, itemIds)` / `unit_set_folders(id, folders)` / `unit_delete(id)` | set のメンバーの追加・削除・並べ替え（並べ替えると manual になる）、folder の参照フォルダの置き換え、解除（アイテムは残る）。`docs/playlists.md` §2 |
| スマートフォルダ | `smart_folders_list(withCounts?, utcOffsetMinutes)` / `smart_folder_save(name, query)` / `smart_folder_update(id, name?, query?)` / `smart_folder_delete(id)` | 条件は検索の文字列のまま保存（解析できない条件は保存しない）。件数は一致するアイテムの数 |
| 検索 | `query_parse(text, view?, utcOffsetMinutes)` → `{ error?, clauses, counts? }` | ⌘K のチップ・件数・エラー表示用。`error` は位置（UTF-16）と理由、`clauses` はいちばん外側の AND の条件、`counts` は開いている一覧の中で一致する種類ごとの件数 |
| | `query_suggest(word, limit)` → `Suggestion[]` | 入力中の語の候補：タグ → フィールド名・値 → 保存済みの検索（`kind` / `insert` / `prefix` / `label` / `count` / `id`） |
| プレイリスト | `playlists_list()` → `PlaylistSummary[]` / `playlist_get(id)` → `Playlist` | サイドバーの一覧（並び順・件数）と、画面用の設定一式（保存した再開の位置 `resume` を含む） |
| | `playlist_create(name, kind, granularity)` / `playlist_update(id, patch)` / `playlist_delete(id)` | smart は作るとき シャッフル・条件なし（すべて）・上限 50 件で始める。`patch` は名前・設定・条件（`rule`：`{ match, clauses }`、解析できなければ `InvalidQuery`）・上限・並び順。画像の表示時間は 1〜60 秒 |
| | `playlist_set_granularity(id, granularity)` | 非破壊（§3.4）。値を変えるだけ |
| | `playlist_contents(id)` → `{ rows, flatItemIds, itemCount, images, audios, videos, totalMs }` | 画面の一覧：エントリごとの行（名前・表紙・件数・長さ・種類、unit / folder は中身のアイテム）と、展開後の件数・合計 |
| | `playlist_add(id, sources, invert, groupName?, before?)` → `{ entryIds, unitId? }` | §3.5 の表どおりに追加する（`invert` は ⌥、`groupName` は「1 つのユニットにまとめて追加」で set ユニットを作る）。`before` のエントリの前に、なければ末尾に |
| | `playlist_move(id, entryIds, before?)` / `playlist_remove(id, entryIds)` | 並べ替え（pos の中間値、差が 1e-9 未満なら振り直す）・削除 |
| | `playlist_snapshot_to_files(id, entryIds?)` → 元のエントリ / `playlist_restore_entries(id, entries)` | 「ファイルに固定」（省けば全体）と、その取り消し（ADR-82） |
| | `playlist_regenerate(id, utcOffsetMinutes)` | smart の並びを新しいシードで作り直す（「作り直す」。ADR-79） |
| | `playlist_save_state(id, state?)` | 再開の位置（シード・スロット番号・アイテム・再生位置）を保存する。null で消す（§6.5、ADR-86） |
| 再生 | `playback_support_set(entries)` | 起動時にフロントが `canPlayType` で作った「入れ物 × コーデック」の表（§8）。表が前回と変わっていれば、作り済み以外のプロキシの判定をやり直す |
| | `play_queue(source, request, utcOffsetMinutes)` → `Queue` | 再生するキューを作る（M6 で `player_queue` を置き換え）。`source` は `playlist { id }`（その設定どおり。シード・smart の並びを保存する）、`selection { entries }`（アイテムと `0x8000_0000 \| unit_id` を並び順のまま。ユニットを含めばユニット単位）、`view { view, query, collapseUnits }`（⌘K の ⇧↵：開いている一覧の中で条件に合うもの）。`request` は `resume`・`shuffle`（S で切り替え）・`reseed`（並べ直す）・`startItem`（そのアイテムのユニットを先頭に）・`fromItem`（並びはそのままでそこから）・`seed`。`Queue` は Slot 列を列ごとの配列で持つ：`itemIds` / `unitIds` / `indexInUnit` / `unitLen` / `imageDurationMs`（音楽・動画は null）と、ユニット単位のときのまとまり `groups`・`groupIndex`、`seed`（16 進）・`startIndex`・`startPositionMs`・`totalMs`、プレイリストならその設定 `playlist`（ADR-84） |
| | `playback_report({ itemId, unitId, unitStart, playlistId, playedMs, completed, durationMs })` → 記録したか | スロットを離れたときに送る。再生済みの判定（playlists.md §6.4）は Rust がし、再生済みなら `play_events` と回数・日時を更新する |
| | `proxy_request(ids, force)` | 再生キューの今と次のプロキシを先に作ってもらう。`force` は元のまま再生できなかったものを作成待ちに戻す（§8） |
| | `open_external(hash)` → `mpv` \| `quickTime` | 元のファイルを外部プレイヤーで開く（mpv があれば mpv） |
| | `player_prefs_get()` / `player_prefs_set(prefs)` | 再生の設定（音量・画像の間も直前の音楽を流す・BGM をループ・画像の表示時間の既定値）。app.json に保存 |
| ジョブ | `jobs_list(filter)` / `job_cancel(id)` / `job_pause(id)` / `job_resume(id)` / `job_retry(id)` / `job_log(id)` | 進捗はイベント `job://progress` |
| ダウンロード | `download_detect(url)` → `{ url, site, siteLabel, tool, rule?, playlist, toolFound }` | URL の取得方法（規則 → サイトの表 → yt-dlp の汎用）。§9 |
| | `download_defaults()` / `download_enqueue(url, options)` → JobId | 前回の選択（ライブラリの settings）と、取得を積む（選んだ内容を次回の既定にする）。`options` は画質（best / 1080 / 720 / 480）・音声のみ・保存先のフォルダ・自動タグ（投稿者・サイトのタグ）・再生リストをユニットに |
| | `downloads_list(limit?)` → `DownloadJob[]` | ダウンロードのジョブを新しい順に（URL・サイト・ツール・題名・件数・入れたアイテムとユニット） |
| | `download_prefs_get()` / `download_prefs_set(prefs)` | Cookie の読み込み元と規則（app.json。規則の正規表現と引数を確かめる。ADR-99） |
| ジョブ | `job_retry(id)` / `job_prioritize(id)` / `job_log(id)` → 文字 | 再試行・先に取得・ログの末尾 256KB（ADR-103） |
| キャプチャ | `capture_rules_*` / `capture_run(url, rule_id?)` → `JobId` / `browser_open(url)` / `browser_snapshot(label)` | |
| ツール | `tools_status()` → `ToolInfo[]` / `tools_set_path(tool, path?)` / `tools_update()` → `{ brewCommand?, outputs }` | 版・場所・Homebrew 版か。場所の指定（app.json）。「更新を確認」（ADR-100） |
| 設定 | `settings_get(keys)` / `settings_set(map)` | |
| 外観 | `appearance_get()` → `Appearance` | OS の表示設定（「透明度を下げる」）。起動時に `<html data-reduce-transparency>` へ反映する（§11） |
| 開発 | `bench_report(name, result)` | `VITE_KURA_BENCH=1` で起動したときのスクロール計測の結果をログに書く（§13） |

変更（patch）の型は、省いた項目を変えない。色・メモの空文字は「消す」の意味（`Option<Option<T>>` を JSON で表しにくいため）。

コマンドの追加は `.claude/skills/add-command` の手順で行い、この表に追記します。

## 4. kura:// プロトコル

`tauri::Builder::register_asynchronous_uri_scheme_protocol("kura", …)` で登録します。macOS では `kura://localhost/<path>` の形で参照します。

| パス | 返すもの | ヘッダ |
|---|---|---|
| `/thumb/{hash}/s` | 長辺 512px サムネイル | `Cache-Control: public, max-age=31536000, immutable` |
| `/thumb/{hash}/l` | 長辺 1600px サムネイル | 同上 |
| `/art/{hash}` | 音楽のジャケット | 同上 |
| `/file/{hash}` | 元ファイル | `Accept-Ranges: bytes`、Range があれば 206 と `Content-Range` |
| `/play/{hash}` | 再生用：プロキシがあればプロキシ、なければ元ファイル | 同上 |

- Range は 1 区間だけ対応（`bytes=start-end` / `bytes=start-` / `bytes=-末尾の長さ`）。範囲外・不正な Range は 416。応答本体は最大 4MB ずつ返す（`bytes=start-` には先頭から 4MB だけ）。
- `{hash}` は 64 桁の小文字 16 進だけを受け付ける。それ以外とファイルがないものは 404、ライブラリが開かれていなければ 503。
- 実際の置き場所：`thumbs/ab/<hash>_s.jpg`・`_l.jpg`、`art/ab/<hash>.jpg`、`proxies/ab/<hash>.mp4`、`files/ab/cd/<hash>.<ext>`（`/file`・`/play` は拡張子が分からないので `files/ab/cd/` の中を探す）。
- WKWebView の `<video>` はサンプル単位の小さな Range（数百バイト）を多数送ってくる（60 秒・1.4MB の MOV の再生で約 3,000 回）。今は 1 回ごとにファイルを開いて読む。大きな動画での負荷は M5 で測る。
- `RUST_LOG=kura_app_lib=debug` で要求ごとに uri・range・status・bytes をログに出す。
- `Content-Type` は拡張子から決める。
- ファイルを開く処理は `spawn_blocking` で行う。
- `tauri.conf.json` の CSP（目安）：`default-src 'self' ipc: http://ipc.localhost; img-src 'self' kura:; media-src 'self' kura:; style-src 'self' 'unsafe-inline'`。外部のフォントや CDN は読み込まない（書体は SF Pro とヒラギノのみ）。

## 5. ジョブ

- `jobs` テーブルに記録し、アプリ再起動後も `queued` / `paused` を再開できるようにする（`running` で終わっていたものは `queued` に戻す）。
- アプリの終了・ライブラリの切り替えでは、実行中のジョブを区切りで止めて `queued` に戻す（次回の起動で自動で再開）。ユーザーが一時停止したものは `paused` のまま。
- 再開に必要な途中経過は、ジョブ自身が作業と同じトランザクションで `result_json` に保存する。
- ライブラリを開くときは、staging の掃除 → 整合性チェック → ジョブの開始、の順にする（チェック中に取り込みが進むと食い違いを誤って数えるため）。
- 種類ごとの同時実行数：import 1、thumbnail 1（ジョブの中で CPU コア数の並列。ADR-31）、proxy 1、download 2、capture 1。
- 後続のジョブは `ensure_queued(kind, input)` で積む（同じ種類が `queued` ならもう積まない）。サムネイルのジョブは、取り込みのバッチを書いたあとと、ライブラリを開いたとき（`pending` が残っていれば）に積む。
- 各ジョブはキャンセルトークンを持つ。外部プロセスはキャンセル時に SIGTERM、5 秒後に SIGKILL。
- 進捗は `job://progress` イベント（`{ id, kind, state, progress, message, resultJson }`）を 200ms 以上の間隔で間引いて送る（状態の変わり目は間引かない）。
- ログは `logs/job-<id>.log` に書き、UI の「ログ」から開ける。

## 6. 取り込みパイプライン

```text
import_preview:  走査（walkdir）→ 対応拡張子で絞る → 種類別件数・合計サイズ
                 重複の見込み：(サイズ, 先頭と末尾 64KB の BLAKE3) で既存と照合（同じ取り込みの中の重なりも数える）
import_start:    ジョブとして実行
  1. 走査（隠しファイル、*.kuralib・*.photoslibrary などのパッケージ、シンボリックリンクは除く。
     名前の自然順に並べるので、再開しても同じ順になる）
  2. 500 件（または合計 1GB）ずつ、最大 6 並列で（ADR-21）
     a. staging/import-<job>-<n>.<ext> へ reflink（APFS クローン、異なるボリュームなら通常コピー）
     b. コピーした側の BLAKE3（1MB 以上は mmap で並列）。コピー後に取るので、名前と中身が必ず一致する（ADR-19）
     c. files/ab/cd/<hash>.<ext> へ rename（原子的。同じものがすでにあればコピーを消す）
     d. メディア情報（MediaBackend::probe）：幅・高さ・長さ・撮影日時・コーデック、音楽はアーティスト・アルバム・曲名
  3. バッチごとに 1 トランザクションで：
     - 既存（同じハッシュ）なら、オプションのフォルダ・タグ・ユニットへの所属だけ追加して「重複」と数える
       （「何もしない」を選んだら所属も足さない。ゴミ箱にあれば戻す）
     - 新規なら items と所属を挿入
     - 再開位置と途中の件数を jobs.result_json に保存
  4. 新しいアイテムがあればサムネイルのジョブを積む（`ensure_queued`。プロキシの判定は M5）
  5. 結果（追加数・重複数・失敗数と明細）を result_json に
```

- 対応拡張子：画像 `jpg jpeg png gif webp heic heif avif tif tiff bmp`、動画 `mp4 m4v mov webm mkv avi`、音楽 `mp3 m4a aac flac wav aif aiff ogg opus`。それ以外はスキップして件数を報告する。
- オプション：取り込み先フォルダ、フォルダ構成の再現、フォルダ名をタグに、ファイル情報からタグ（`アーティスト:`・`アルバム:`。撮影日・撮影地は付けない。ADR-30）、全件に付けるタグ、フォルダごとにユニット作成。
  - フォルダ構成の再現：ドロップしたフォルダを取り込み先に対応させ、その下にサブフォルダを作る（旅行2026/鎌倉 → 写真/2026 旅行/鎌倉）。取り込み先がなければ、ドロップしたフォルダ名でいちばん上に作る（ADR-24）。
  - フォルダ名をタグに：ドロップしたフォルダからファイルの親までの各フォルダ名（ADR-24）。
  - フォルダごとにユニット：末端フォルダ（下にフォルダを持たないもの）ごとに set ユニット（origin = import、並びはファイル名の自然順）。
  - ファイル名・フォルダ名・タグは NFC に正規化する（ADR-25）。
- 起動時の整合性チェック：`staging/` の残骸を削除。DB にあってファイルがないもの、ファイルがあって DB にないものを数えて設定画面に出す（自動では消さない）。
  - 取り込み中に落ちた場合、配置済みで DB に入る前のファイルは「DB にないファイル」として残るが、再開した取り込みがそのまま使う（内容アドレスなので置き直しは不要）。
- 動作確認：`cargo run -p kura-devtools -- import --library <path> [--create] <src>...` でコマンドラインから取り込める（強制終了の試験用。`check --library <path>` で件数と食い違いを表示）。

## 7. サムネイルとメディア情報（kura-mac）

- Swift ヘルパー（`crates/kura-mac/swift`、Swift 5 の言語モード）が C 互換関数を公開し、Rust から `extern "C"` で呼ぶ（swift-rs はビルド時のリンクにだけ使う）。戻り値は 0 成功・1 扱えない形式・2 失敗・3 ジャケットなし：
  - `kura_image_thumbnails(path, out_l, out_s, 1600, 512)`：ImageIO の `CGImageSourceCreateThumbnailAtIndex`（HEIC・RAW・AVIF も扱える。向きを反映）→ JPEG（品質 0.82）
  - `kura_video_thumbnails(path, at_sec, out_l, out_s, 1600, 512)`：AVFoundation の `AVAssetImageGenerator`（長さの 10% 地点。長さが分からなければ 1 秒）
  - `kura_audio_artwork(path, out_art, out_l, out_s, 1600, 512)`：AVAsset のメタデータからジャケット（元の画像を `art/`、縮小を `thumbs/` に）
  - `kura_probe(path, kind)` → JSON（`kura_free_string` で解放）：幅・高さ・長さ・撮影日時・コーデック・アーティスト・アルバム・曲名
  - `kura_reduce_transparency()` → bool
- Swift のランタイムは OS のもの（`/usr/lib/swift`）を使う。リンクする実行ファイル（kura-mac のテスト、src-tauri、kura-devtools）の build.rs で rpath を足す。
- 形式ごとの試す順（`kura_mac::media_backend()`）：flac・ogg・opus・mkv・webm・avi は ffmpeg / ffprobe を先に、それ以外は macOS を先に。前の実装が「扱えない形式」と答えたときだけ次を試す（ADR-32）。
- ffmpeg / ffprobe は `/opt/homebrew/bin` → `/usr/local/bin` → PATH の順で探す（`kura_core::tools`。設定での上書きは M8）。外部プロセスは時間制限付きで動かす。
- サムネイルのジョブ：`thumb_state = 'pending'` を新しい順に 64 件ずつ取り出し（`idx_items_thumb_pending`）、CPU コア数の並列で作って 1 トランザクションで書く。M1 で取り込んだものは、ここで probe して幅・高さなどの空欄を埋める。ジャケットのない音楽は `failed`（グリッドでは音楽の絵を出す。ADR-34）。
- 目標：画像サムネイル 50 枚/秒以上（NFR-04）。実測は §13。

## 8. 再生とプロキシ

- 再生は WKWebView の `<video>` / `<audio>` / `<img>` で行う（デザインを崩さないため）。
- 起動時に `canPlayType` で再生可能な形式の表を作って Rust に渡す。取り込み時に ffprobe の結果と照合し、`proxy_state` を決める。
- 再生できない形式はプロキシを作る：
  - コーデックが対応済み（例：mkv に入った H.264 / AAC）→ `ffmpeg -c copy` で mp4 に入れ替え（数秒）
  - それ以外 → VideoToolbox でハードウェアエンコード（`-c:v h264_videotoolbox` など）、音声は AAC 256kbps
- プロキシ作成中・失敗時は「外部プレイヤー（mpv または QuickTime Player）で開く」を出す。

### 8.1 実装（M5）

- **再生可否の表**：フロント（`src/features/player/support.ts`）が「入れ物（mp4・webm・ogg・mp3・flac・wav・aiff・aac）× ffprobe のコーデック名」ごとに `canPlayType` を調べ、空でなければ再生できるとする。表はアプリのメモリに持ち、ライブラリを開くたびに渡す。表の指紋を `settings` の `playback.support` に記録し、変わっていれば `not_needed` / `pending` / `failed` の動画・音楽を `unknown` に戻す（ADR-61）。
- **判定**（`kura_core::playback::support::plan`、純粋関数）：入れ物は拡張子で決める（mov・m4v・m4a は mp4、opus は ogg、mkv・avi などは「その他」）。元の入れ物で映像・音声とも再生できれば `not_needed`。そうでなければ `pending` で、プロキシ（mp4）では mp4 で再生できるストリームはコピー、できないストリームだけ変換する（ADR-63）。映像も音声もなければ `failed`。画像はプロキシを作らない（取り込み時に `not_needed`、ADR-65）。
- **プロキシのジョブ**（`kind = 'proxy'`、同時に 1 つ）：取り込みのあとに積む。`unknown` を新しい順に 16 件ずつ ffprobe（4 並列）で判定し、`pending` を 1 件ずつ作る。再生キューの今と次 3 件（`proxy_request`）を先に処理する。一時ファイル（`.mp4.part`）に書いてから置き換え、一時停止・中止・アプリの終了では ffmpeg を止める（`pending` のまま残り、次回に続きから）。ffmpeg の進み具合が 2 分止まったら失敗にする。ffmpeg がなければ何もしない（`unknown` のまま、元のファイルで再生を試す）。
- **ffmpeg の引数**：`-map 0:<映像> -map 0:<音声>`（ジャケット・字幕・データは落とす）、コピーは `-c copy`（HEVC は `-tag:v hvc1`）、映像の変換は `h264_videotoolbox -b:v 5M/10M/30M（720p/1080p/それ以上）-pix_fmt yuv420p -allow_sw 1`、音声の変換は `aac -b:a 256k`、`-movflags +faststart -f mp4`。
- **再生できなかったとき**：`not_needed` / `unknown` のものが再生エラーになったら `proxy_request(force)` で `pending` に戻し、判定が「不要」でもすべて変換する。プロキシでもだめなら「外部プレイヤーで開く」「もう一度変換する」を出す（ADR-64）。
- **配信**：`/play/{hash}` はプロキシがあればプロキシ、なければ元ファイル。作り直したプロキシは `?a=<回数>` を付けて読み直す。`.opus` は `audio/ogg` で配る（ADR-73）。
- **再生**（`src/features/player/engine/PlaybackController.ts`）：キューを順に再生するだけ（playlists.md §6）。`<video>` はシアターに置いたものを使い、BGM 用の `<audio>` 2 つと先読み用の要素はコントローラが作る。シアターは再生中ずっと DOM に残し、ミニプレーヤーのときは隠す（ADR-71）。ウィンドウが隠れても止まらないよう、WebView のバックグラウンドの抑制を切る（§11、ADR-72）。

### 8.2 プレイリストの再生（M6）

- **キュー**（`kura_core::playlist`）：`play_queue` がプレイリスト・選択・開いている一覧からエントリを作り、`resolve`（DB から読んだスナップショットを展開）→ `build_queue`（純粋関数、playlists.md §5）でスロット列にする。プレイリストはシードと smart の並びを保存するので書き込みのトランザクションで作る（ADR-79・84）。
- **フロント**：`PlaybackController` は受け取ったキューを順に再生するだけ。シャッフル（S）・並べ直す・リピートの全体で先頭に戻るときは、`play_queue` にもう一度頼んで作り直す（`startItem` で今のアイテムを先頭に、`reseed` で新しいシード）。ユニット単位のときはキューの `groups` / `groupIndex` で「ユニット 3 / 12 · 2 / 10」・⇧← ⇧→・キューの折りたたみを出す。
- **再開**：プレイリストの再生中は位置を 5 秒ごと・スロットが変わったとき・閉じたときに `playlist_save_state` で保存する（ADR-86）。

## 9. ダウンロード

| 段階 | 内容 |
|---|---|
| 判定 | URL のホストからツールを選ぶ。youtube.com / youtu.be / nicovideo.jp / soundcloud.com など → yt-dlp。pixiv.net / x.com / 各種 booru など → gallery-dl。ユーザー定義の規則（正規表現 → ツール＋追加引数）を優先 |
| 実行 | `staging/dl-<job>/` に保存。download-archive は `archives/` に置き、再取得を防ぐ |
| 進捗 | yt-dlp は `--newline` と `--progress-template` で 1 行ずつ機械可読に、gallery-dl はファイル完了ごとに数える（引数は実装時に各ツールの `--help` で確認） |
| メタデータ | yt-dlp は `--write-info-json`、gallery-dl は `--write-metadata` |
| 登録 | 取り込みパイプラインに渡す。タグ化：投稿者 → `投稿者:<名前>`、pixiv の作者 → `作者:<名前>`、各サイトのタグ → タグ、タイトル → title、URL → source_url、投稿日 → captured_at |
| ユニット | 複数ファイルの投稿は投稿ごとに set ユニット。再生リストは設定に従う |
| 認証 | Cookie は「Firefox から読む」を推奨（Safari・Chrome も選べるが、キーチェーンや権限の確認が出る）。pixiv は gallery-dl の OAuth（`gallery-dl oauth:pixiv`）を、手順を表示してユーザーにターミナルで実行してもらう |
| ツールの場所 | `/opt/homebrew/bin` → `/usr/local/bin` → 設定値の順に探す。見つからなければ `brew install yt-dlp gallery-dl deno ffmpeg` を案内 |
| 更新 | 「更新を確認」で `yt-dlp -U`（Homebrew 版なら `brew upgrade` を案内） |

### 9.1 実装（M7）

- **判定**（`kura_core::download::detect`、純粋関数）：規則（正規表現）→ ホストの表（youtube・niconico・soundcloud・vimeo・twitch・bilibili・tiktok・bandcamp は yt-dlp、pixiv・fanbox・x・bluesky・misskey・instagram・tumblr・deviantart・artstation・imgur・reddit・各種 booru は gallery-dl）→ 表にないホストは yt-dlp の汎用。YouTube の再生リストのページは中の動画をまとめて、再生リストの中の 1 本の URL は `--no-playlist`。
- **ジョブ**（`kind = 'download'`、同時に 2 つ）：`staging/dl-<job>/` にツールで取得し、終わったら作業フォルダのメディアのファイルを取り込みと同じ方法で配置して、1 つのトランザクションで登録する（ADR-92）。進捗は `KURA` のタブ区切りの行（ADR-93）。一時停止・中止はプロセスグループごと止める（ADR-94）。子プロセスの PATH には指定した場所と Homebrew の場所を足す（yt-dlp が ffmpeg・deno を、gallery-dl がうごイラの変換に ffmpeg を見つけられるように）。
- **yt-dlp の引数**：`--newline --color never --progress --progress-delta 0.5 --no-mtime --write-info-json --progress-template … --print before_dl:… --print after_move:… -o %(id)s.%(ext)s -P <作業フォルダ> --download-archive archives/yt-dlp.txt --ffmpeg-location <ffmpeg> --js-runtimes deno:<deno> -S res,ext:mp4:m4a`（画質・音声のみは ADR-97）、Cookie は `--cookies-from-browser`。
- **gallery-dl の引数**：`--no-colors --write-metadata --no-mtime --ugoira mp4 -D <作業フォルダ> --download-archive archives/gallery-dl.sqlite3 --Print prepare:… --Print after:… --Print skip:…`、Cookie は `--cookies-from-browser`。pixiv は `gallery-dl oauth:pixiv` を一度ターミナルで実行してもらう。
- **登録**（`download::meta`・`register`）：ADR-96。タグは `item_tags.source = 'download'`、ユニットは `units.origin = 'download'` と URL、`remote_sources` にサイト・ID・ページ番号。
- **ツールの場所**：`kura_core::tools::find` は app.json の指定 → `/opt/homebrew/bin` → `/usr/local/bin` → PATH の順。ffmpeg・ffprobe・mpv の場所の指定もプロキシ・外部プレイヤーに効く。

## 10. キャプチャ

- **ルールで自動キャプチャ**：chromiumoxide（Rust の CDP クライアント）でインストール済みの Google Chrome をヘッドレス起動する。プロファイルは `~/Library/Application Support/<bundle id>/capture-profile`。
  1. ルールの `viewport_width` で開く → `wait_ms` 待つ（または対象セレクタの出現）
  2. `hide_selectors` を CSS で非表示にする
  3. `scroll_times` 回スクロールして遅延読み込みを発火させ、先頭に戻す
  4. `element`：対象要素の範囲を clip して撮影。`full_page`：`captureBeyondViewport` でページ全体。`viewport`：表示範囲
  5. PNG を staging に保存 → 取り込み（タグ `キャプチャ`、`サイト:<ホスト>`、source_url）
- **ログイン**：「ログイン用に開く」で同じプロファイルの Chrome を画面ありで起動し、ユーザーにログインしてもらう。
- **見たままキャプチャ**：アプリ内の別ウィンドウ（WebviewWindow）でページを開き、ボタンで WKWebView の `takeSnapshot` を呼んで表示範囲を保存する（v1 は表示範囲のみ）。

## 11. ウィンドウと macOS 固有の設定

- `tauri.conf.json` のウィンドウ：`titleBarStyle: "Overlay"`、`hiddenTitle: true`、`trafficLightPosition: { "x": 25, "y": 35 }`（サイドバーの中に信号機を置く）、`minWidth: 960`、`minHeight: 600`。
  - 信号機の位置は macOS 26 で実測して決めた。ネイティブの信号機は直径 14px・間隔 23px で、モックの絵（12px・20px）と寸法が違うため、1 つ目の左端（x=25）と縦の中心（y=33、サイドバー先頭の 24px 行の中央）をモックに合わせている。`y` と実際の上端の関係は「上端 = y − 9」。
- ガラス表現は CSS の `backdrop-filter` で行う（`docs/decisions.md` ADR-12）。ウィンドウの透明化・private API は使わない。
- 「透明度を下げる」がオンなら `<html data-reduce-transparency="true">` を付け、ガラスを不透明な面に置き換える。
- ファイルのドラッグ＆ドロップは `getCurrentWebview().onDragDropEvent` で受ける（`enter` で重ね表示、`drop` で取り込みシート）。
- `backgroundThrottling: "disabled"`（M5）：既定では、ウィンドウがほかのウィンドウに隠れると WebKit が動画の再生開始や CSS アニメーションを止め、再生キューが動画のところで止まる。メディアプレーヤーなので隠れていても再生を続ける（ADR-72）。
- メニューバーはフロントで組む（`src/app/appMenu.ts`）。項目は Tauri の既定のメニューと同じで、文言は ja.ts。「すべてを選択」（⌘A）だけは自前の項目で、入力欄なら文字、それ以外は一覧のすべてを選ぶ（ADR-76）。アプリの言語は `src-tauri/Info.plist` で日本語と宣言し、OS が出す文言も日本語にする（ADR-77）。
- 署名は個人利用なのでアドホック署名（`bundle.macOS.signingIdentity: "-"`）。App Store には出さない。
- 書類・デスクトップ・ダウンロードを「ファイルを選択」で開くと macOS の許可ダイアログが出る。ドラッグ＆ドロップした項目は許可なしで読める。

## 12. 状態管理（フロント）

- Zustand のストアを機能ごとに分ける：`libraryStore`（開いているライブラリ・検索結果の ID 列）、`viewStore`（選択・表示の設定）、`itemCache`（ID → 詳細の LRU）、`playerStore`（キュー・現在位置・再生状態）、`jobsStore`（イベントで更新）。
- 検索結果の ID 列は `Uint32Array` で保持する。
- ジャスティファイ配置は「行の高さ目標・コンテナ幅・各アイテムの縦横比」から行分割を計算する純粋関数（`src/features/library/layout/justify.ts`）にし、テストする。縦横比は `items_query_ids` が ID と一緒に全件分返すので、詳細を取得する前に全体の高さと行分割が決まる（音楽は 1.0、不明は 1.0 として扱う）。
- `itemCache` は 4,000 件の LRU（アイテムとユニットで 1 つずつ）。見えている行の ID を 200 件ずつまとめて `items_get`・`units_get` する。編集したものは捨てずに取り直す（届くまで前の内容を出す）。取り込み中の一覧の取り直しは 1 秒、サムネイルのイベントでの取り直しは 0.5 秒にまとめる。
- 選択は「選んだ順の ID・範囲の基点・注目している ID」を持つ純粋関数（`src/features/library/selection.ts`）。選んだ順は ⌘G のユニットの並び順に使う。
- 編集は `src/features/library/actions.ts` に集め、IPC のあとで画面のデータを読み直す：`revisionStore` を進める（インスペクタ・サイドバーが読み直す）、変わったアイテム・ユニットを取り直す、一覧はその変更で中身が変わる種類（未分類・タグなし・フォルダなど）を開いているときだけ読み直す。失敗は `noticeStore` の一行で知らせる。
- サイドバーの件数とフォルダの木は `sidebarStore`（開いているフォルダだけ `localStorage` の `kura.sidebar`）。
- 右クリックのメニュー（`components/Menu`）と、名前の入力・確認・候補から選ぶダイアログ（`components/Overlay` の `askText`・`askConfirm`・`askPick`）は 1 か所で描き、どこからでも開ける。
- アプリの中のドラッグ（タイル → フォルダ、フォルダの移動、ユニットの中の並べ替え）はポインタイベントで自前に行う（`dragState.ts`）。Tauri のファイルドロップの処理が WebView の HTML5 ドラッグのイベントをすべて受け取り、JS に `dragover` / `drop` が届かないため（ADR-44）。
- WebKit ではボタンを押してもフォーカスが移らないので、タイルとサイドバーの行は押したときにフォーカスを移す（続けて押したキーがそこに届くように）。
- 検索の条件は `viewStore.filter`（チップの文字列の列）。⌘K パレット（`features/search`）・ツールバーの検索欄・フィルタバーは、どれもこの列を読み書きする。一覧を切り替えると外し、ユニットを開いたら預けて、戻ったら戻す（ADR-53）。パレットの開き方（新しいスマートフォルダ・条件の編集）は `searchStore`。
- 並び順は `viewStore.sort`（表示の設定として `localStorage` の `kura.view` に残す）。
- `viewStore` は選択中の ID・サムネイルの大きさ（行の高さ 110 / 140 / 180 / 230 / 300px の 5 段階）・インスペクタの開閉・クイックルックを持つ。大きさとインスペクタの開閉だけを `localStorage`（`kura.view`）に残す。

## 13. 開発用のツールと計測

| コマンド・環境変数 | 用途 |
|---|---|
| `cargo run --release -p kura-devtools -- seed --items 100000 --out <path>` | ダミーライブラリを作る（画像 7・動画 2・音楽 1、2,000 タグ、60 フォルダ、200 件ごとに 10 件の set ユニット、ひな形 JPEG をクローンで配置、シード付き ChaCha8）。10 万件で約 28 秒 |
| `kura-devtools bench query --library <path>` | 開く・`query_ids`・`items_get`（200 件）・`item_detail`、サイドバーの一覧の種類・件数・タグの候補の中央値 |
| `kura-devtools bench thumbs --src <dir> [--threads N]` | 画像サムネイルの生成速度（枚/秒） |
| `kura-devtools import` / `check` | コマンドラインからの取り込みと整合性の確認（§6） |
| `kura-devtools download --library <path> <url>...` | アプリと同じダウンロードのジョブで取得して入れる（§9。既定の選択・Cookie なし） |
| `KURA_LIBRARY=<path>` | アプリをこのライブラリで起動する（「前回のライブラリ」は書き換えない） |
| `VITE_KURA_BENCH=1` | グリッドを自動でスクロールし、フレーム時間を `bench_report` でログに書く |

M2 の実測（Apple Silicon・16 コア、リリースビルド、10 万件）：

| 項目 | 結果 | 目標 |
|---|---|---|
| `query_ids` | 18ms（606KB） | — |
| `items_get`（200 件） | 0.16ms | — |
| 起動から最初の `items_get` まで | 628ms | NFR-01：1.5 秒以内 |
| スクロール（601 フレーム） | p50 17ms・p99 18ms、20ms 超え 0 | NFR-02：60fps |
| 画像サムネイル（4000×3000 の JPEG・HEIC） | 1 並列 27・4 並列 101・8 並列 185・16 並列 211 枚/秒 | NFR-04：50 枚/秒以上 |

M3 の実測（同じ環境、10 万件・ユニット 500 個、`bench query` の中央値。2 回測った幅）：

| 一覧・件数 | 結果 |
|---|---|
| 未分類（約 5 万件） | 21〜24ms |
| タグなし（約 1.7 万件） | 20〜24ms |
| フォルダ（子孫を含む、約 5 千件） | 3ms |
| ランダム（10 万件） | 9〜12ms |
| まとめて表示（10 万件 → 95,500 件） | 20〜30ms |
| サイドバーの件数（ライブラリの 5 項目＋フォルダ 60 個） | 40〜64ms（グリッドのあとに別に取る） |
| タグの候補（2,000 個から） | 6〜10ms |

M4 の実測（同じ環境、10 万件、`bench query` の検索の中央値）：

| 検索 | 一致 | 結果 | 目標 |
|---|---|---|---|
| タグ 3 つの AND（よく使うタグ） | 32 件 | 1.0ms | NFR-03：50ms 以内 |
| タグ 3 つの AND（名前空間なし） | 5 件 | 0.7ms | 同上 |
| タグ 3 つの AND（珍しいタグを含む） | 0 件 | 0.5ms | 同上 |
| 全文検索（3 文字以上、FTS5 trigram） | 6 件 | 0.2ms | NFR-03：100ms 以内 |
| 全文検索（2 文字、LIKE） | 3,691 件 | 21ms | 同上 |
| 種類・評価・追加日の組み合わせ | 10,439 件 | 7.5ms | — |
| OR・除外・種類の組み合わせ | 5,763 件 | 15ms | — |
| 名前順（10 万件、自然順） | — | 135ms | — |
| 撮影日順（10 万件） | — | 53ms | — |

M5 の実測（同じ環境）：

| 項目 | 結果 |
|---|---|
| 10 万件の選択からキューを作る（`player_queue` の中身） | 25ms＋合計の長さ 13ms（`cargo test --release -p kura-core queue -- --ignored`） |
| 起動から判定とプロキシの作成まで（動画・音楽 8 件、mkv 2 件の入れ替え・avi 1 件の変換） | 0.5 秒 |
| 混在キュー 12 件（mkv×2・webm・ogg・opus・flac・avi・mp4・画像 4、ウィンドウは隠した状態） | 途中で止まらず全件を「最後まで再生」で記録。スロットの切り替えは画像 0〜0.1 秒・音楽と動画 0.5〜1.4 秒（読み込み） |

M6 の実測（同じ環境、リリースビルド。`cargo test --release -p kura-core t15 -- --ignored --nocapture`、3 回の幅）：

| 項目 | 結果 | 目標 |
|---|---|---|
| 1 万エントリ・5 万アイテムの `resolve + build_queue`（ファイル単位、シャッフル・重み付き・連続制限） | 11〜14ms | playlists.md §5.6：50ms 以内 |
| 同じ（ユニット単位） | 6〜9ms | 同上 |
