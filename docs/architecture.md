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
| `kura-mac` | `MediaBackend` trait の macOS 実装（サムネイル生成、メディア情報、音楽のジャケット取得、WKWebView のスナップショット、「透明度を下げる」設定の取得） | kura-core, swift-rs |
| `kura-devtools` | ダミーライブラリの生成、ベンチマーク | kura-core |
| `src-tauri`（`kura-app`） | コマンド、kura:// プロトコル、ウィンドウ、イベント、状態の保持 | すべて |

`kura-core` は `MediaBackend` trait（`thumbnail_image`、`thumbnail_video`、`audio_artwork`、`probe`）だけを知り、実装は起動時に注入します。テストでは偽物の実装を使います。Linux 上でも `kura-core` のテストが通るようにします。

## 3. IPC コマンド一覧（初期）

戻り値はすべて `Result<T, AppError>`。`AppError { code: string, message: string }`。code の例：`NotFound`、`InvalidQuery`、`Invalid`、`ToolMissing`、`Io`、`Db`、`Conflict`、`Internal`。

| 領域 | コマンド | 説明 |
|---|---|---|
| ライブラリ | `library_create(path)` / `library_open(path)` / `library_info()` | ライブラリの作成・オープン・概要 |
| | `library_restore()` → `{ info, lastPath }` | 起動時に前回のライブラリを開く。開けなければ `info` は空で前回の場所を返す |
| | `library_check_location(path)` → `{ path, status, syncService }` | 置き場所の確認（`.kuralib` の補完、`new` / `existingLibrary` / `occupied`、同期フォルダの中か） |
| 取り込み | `import_preview(paths)` | 種類別件数・合計サイズ・重複の見込み・フォルダ木を返す |
| | `import_start(paths, options)` → `JobId` | 取り込みジョブを開始 |
| 一覧 | `items_query_ids(query, sort)` → 生バイト | 条件に合う ID と縦横比を並び順で（`[件数 u32][id u32 × N][縦横比×1000 u16 × N]`、LE） |
| | （`items_query_ids` の `collapse_units` が true のとき） | set ユニットに属するアイテムを、最初に一致したメンバーの位置で 1 件のユニットにまとめる。ユニットは `0x8000_0000 | unit_id` で表し、縦横比は表紙のもの |
| | `items_get(ids)` → `ItemSummary[]` / `units_get(ids)` → `UnitSummary[]` | 最大 200 件 |
| | `item_detail(id)` → `ItemDetail` | タグ・フォルダ・ユニット・取得元を含む |
| 編集 | `items_update(ids, patch)` | タイトル・評価・メモ（複数選択対応） |
| | `items_trash(ids)` / `items_restore(ids)` / `trash_empty()` | ゴミ箱 |
| タグ | `tags_suggest(prefix, limit)` / `tags_apply(item_ids, add, remove)` / `tag_update(id, patch)` / `tag_merge(from, into)` | |
| フォルダ | `folders_tree()` / `folder_create` / `folder_update` / `folder_move` / `folder_delete` / `items_set_folders(item_ids, add, remove)` | |
| ユニット | `units_list(filter)` / `unit_create_set(name, item_ids)` / `unit_create_folder(name, folder_refs)` / `unit_update(id, patch)` / `unit_members(id)` / `unit_set_items(id, item_ids)` / `unit_delete(id)` | `docs/playlists.md` §2 |
| スマートフォルダ | `smart_folders_list` / `smart_folder_save(name, query)` / `smart_folder_delete` | |
| 検索 | `query_parse(text)` → `QueryAst` | ⌘K の候補表示・エラー表示用 |
| | `query_suggest(text, cursor)` → `Suggestion[]` | タグ・フィールド・保存済み検索の候補 |
| プレイリスト | `playlists_list()` / `playlist_create(name, kind, granularity)` / `playlist_update(id, patch)` / `playlist_delete(id)` | |
| | `playlist_entries(id)` / `playlist_add(id, targets, at, mode)` / `playlist_move(entry_ids, before_entry_id)` / `playlist_remove(entry_ids)` | `mode` は `auto` / `expand` / `keep`（§3.5） |
| | `playlist_set_granularity(id, granularity)` | 非破壊（§3.4） |
| | `playlist_snapshot_to_files(id, entry_ids?)` | 「ファイルに固定」 |
| | `playlist_build_queue(id, options)` → `Queue` | シード・開始位置を含む |
| | `quick_queue(query, granularity, shuffle)` → `Queue` | ⌘K の ⇧↵「結果をランダム再生」 |
| 再生 | `playback_report(event)` / `playback_save_state(playlist_id, state)` | |
| ジョブ | `jobs_list(filter)` / `job_cancel(id)` / `job_pause(id)` / `job_resume(id)` / `job_retry(id)` / `job_log(id)` | 進捗はイベント `job://progress` |
| ダウンロード | `download_detect(url)` → `DetectedSource` / `download_enqueue(url, options)` → `JobId` | |
| キャプチャ | `capture_rules_*` / `capture_run(url, rule_id?)` → `JobId` / `browser_open(url)` / `browser_snapshot(label)` | |
| ツール | `tools_status()` / `tools_update(tool)` / `tools_set_path(tool, path)` | |
| 設定 | `settings_get(keys)` / `settings_set(map)` | |
| 外観 | `appearance_get()` → `Appearance` | OS の表示設定（「透明度を下げる」）。起動時に `<html data-reduce-transparency>` へ反映する（§11） |

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

- Range は 1 区間だけ対応（`bytes=start-end` / `bytes=start-`）。不正な Range は 416。応答本体は最大 4MB ずつ返す。
- `Content-Type` は拡張子から決める。
- ファイルを開く処理は `spawn_blocking` で行う。
- `tauri.conf.json` の CSP（目安）：`default-src 'self' ipc: http://ipc.localhost; img-src 'self' kura:; media-src 'self' kura:; style-src 'self' 'unsafe-inline'`。外部のフォントや CDN は読み込まない（書体は SF Pro とヒラギノのみ）。

## 5. ジョブ

- `jobs` テーブルに記録し、アプリ再起動後も `queued` / `paused` を再開できるようにする（`running` で終わっていたものは `queued` に戻す）。
- アプリの終了・ライブラリの切り替えでは、実行中のジョブを区切りで止めて `queued` に戻す（次回の起動で自動で再開）。ユーザーが一時停止したものは `paused` のまま。
- 再開に必要な途中経過は、ジョブ自身が作業と同じトランザクションで `result_json` に保存する。
- ライブラリを開くときは、staging の掃除 → 整合性チェック → ジョブの開始、の順にする（チェック中に取り込みが進むと食い違いを誤って数えるため）。
- 種類ごとの同時実行数：import 1、thumbnail = CPU コア数、proxy 1、download 2、capture 1。
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
     d. メディア情報（MediaBackend::probe）：幅・高さ・長さ・撮影日時・コーデック（M2 で追加）
  3. バッチごとに 1 トランザクションで：
     - 既存（同じハッシュ）なら、オプションのフォルダ・タグ・ユニットへの所属だけ追加して「重複」と数える
       （「何もしない」を選んだら所属も足さない。ゴミ箱にあれば戻す）
     - 新規なら items と所属を挿入
     - 再開位置と途中の件数を jobs.result_json に保存
  4. サムネイル・プロキシ判定のジョブを積む（M2）
  5. 結果（追加数・重複数・失敗数と明細）を result_json に
```

- 対応拡張子：画像 `jpg jpeg png gif webp heic heif avif tif tiff bmp`、動画 `mp4 m4v mov webm mkv avi`、音楽 `mp3 m4a aac flac wav aif aiff ogg opus`。それ以外はスキップして件数を報告する。
- オプション：取り込み先フォルダ、フォルダ構成の再現、フォルダ名をタグに、ファイル情報からタグ（撮影日・撮影地・アーティスト・アルバム。M2）、全件に付けるタグ、フォルダごとにユニット作成。
  - フォルダ構成の再現：ドロップしたフォルダを取り込み先に対応させ、その下にサブフォルダを作る（旅行2026/鎌倉 → 写真/2026 旅行/鎌倉）。取り込み先がなければ、ドロップしたフォルダ名でいちばん上に作る（ADR-24）。
  - フォルダ名をタグに：ドロップしたフォルダからファイルの親までの各フォルダ名（ADR-24）。
  - フォルダごとにユニット：末端フォルダ（下にフォルダを持たないもの）ごとに set ユニット（origin = import、並びはファイル名の自然順）。
  - ファイル名・フォルダ名・タグは NFC に正規化する（ADR-25）。
- 起動時の整合性チェック：`staging/` の残骸を削除。DB にあってファイルがないもの、ファイルがあって DB にないものを数えて設定画面に出す（自動では消さない）。
  - 取り込み中に落ちた場合、配置済みで DB に入る前のファイルは「DB にないファイル」として残るが、再開した取り込みがそのまま使う（内容アドレスなので置き直しは不要）。
- 動作確認：`cargo run -p kura-devtools -- import --library <path> [--create] <src>...` でコマンドラインから取り込める（強制終了の試験用。`check --library <path>` で件数と食い違いを表示）。

## 7. サムネイルとメディア情報（kura-mac）

- Swift ヘルパー（swift-rs）で C 互換関数を公開する：
  - `kura_thumb_image(path, max_px, out_path)`：ImageIO の `CGImageSourceCreateThumbnailAtIndex`（HEIC・RAW・AVIF も扱える）→ JPEG（品質 0.82）
  - `kura_thumb_video(path, at_sec, max_px, out_path)`：AVFoundation の `AVAssetImageGenerator`（長さの 10% 地点）
  - `kura_audio_artwork(path, out_path)`：AVAsset のメタデータからジャケット
  - `kura_probe(path)` → JSON：幅・高さ・長さ・撮影日時・コーデック
  - `kura_reduce_transparency()` → bool
- macOS で扱えない形式（mkv など）は ffmpeg / ffprobe にフォールバックする。
- 目標：画像サムネイル 50 枚/秒以上（M1、SSD）。

## 8. 再生とプロキシ

- 再生は WKWebView の `<video>` / `<audio>` / `<img>` で行う（デザインを崩さないため）。
- 起動時に `canPlayType` で再生可能な形式の表を作って Rust に渡す。取り込み時に ffprobe の結果と照合し、`proxy_state` を決める。
- 再生できない形式はプロキシを作る：
  - コーデックが対応済み（例：mkv に入った H.264 / AAC）→ `ffmpeg -c copy` で mp4 に入れ替え（数秒）
  - それ以外 → VideoToolbox でハードウェアエンコード（`-c:v h264_videotoolbox` など）、音声は AAC 256kbps
- プロキシ作成中・失敗時は「外部プレイヤー（mpv または QuickTime Player）で開く」を出す。

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
- 署名は個人利用なのでアドホック署名（`bundle.macOS.signingIdentity: "-"`）。App Store には出さない。
- 書類・デスクトップ・ダウンロードを「ファイルを選択」で開くと macOS の許可ダイアログが出る。ドラッグ＆ドロップした項目は許可なしで読める。

## 12. 状態管理（フロント）

- Zustand のストアを機能ごとに分ける：`libraryStore`（現在のビュー・検索結果の ID 列・選択）、`itemCache`（ID → 詳細の LRU）、`playerStore`（キュー・現在位置・再生状態）、`jobsStore`（イベントで更新）。
- 検索結果の ID 列は `Uint32Array` で保持する。
- ジャスティファイ配置は「行の高さ目標・コンテナ幅・各アイテムの縦横比」から行分割を計算する純粋関数（`src/features/library/layout/justify.ts`）にし、テストする。縦横比は `items_query_ids` が ID と一緒に全件分返すので、詳細を取得する前に全体の高さと行分割が決まる（音楽は 1.0、不明は 1.0 として扱う）。
