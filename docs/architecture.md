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
│   commands/  protocol.rs（kura://・Range 対応）  navigation.rs（main の窓） │
│   engine.rs（エンジンの部品）  state.rs  dto.rs（型・イベント）  config.rs  │
│   error.rs（AppError と code）  shortcut.rs  logging.rs                     │
├────────┼────────────────────────────────────────────────────────────────────┤
│ kura-core（OS 非依存・テスト容易）                                             │
│   db/  import/  query/（検索言語）  unit/  playlist/  jobs/  download/  capture/ │
│        │  kura-engine（編集の操作エンジン。OS 非依存。§15・docs/editing.md）    │
│        │                           │                                         │
│ kura-mac（macOS 専用）              │ 外部プロセス                             │
│   Swift ヘルパー：ImageIO,          │   yt-dlp（+ deno, ffmpeg）               │
│   AVFoundation, CoreText, WebKit   │   gallery-dl                            │
│                                     │   ffmpeg / ffprobe（プロキシ・情報）       │
│                                     │   Google Chrome（headless, CDP）          │
└─────────────────────────────────────────────────────────────────────────────┘
           │
   Kura Library.kuralib/（library.sqlite, library.lock, files/, thumbs/, proxies/, staging/ …）
```

## 2. クレート構成と責務

| クレート | 責務 | 依存してよいもの |
|---|---|---|
| `kura-core` | DB、マイグレーション、取り込み、ハッシュ、検索言語の解析と SQL 生成、ユニット、プレイリスト（純粋関数）、ジョブ管理、ダウンロード・キャプチャの手順、編集の ffmpeg の段（`codec_ext::ffmpeg`。段 9）、SVG を別のプロセスで読む働き手（`codec_ext::svg_worker`。M11 の決定 23：親の側のデコーダ・やり取りの形・子の本体。§7）、編集のエンジンの実行の場（`edit::runtime` の `EngineRuntime`：エンジン 1 つ・プレビューと一括の 2 つのスレッドプール・メモリの予算。段 10、§15）と、それとコーデックの組をライブラリごとに持つ `edit::services` の `EngineServices`（M11）。M11 で足したもの：解析（`analysis`：計画・事実の投影・ドット絵の判定 v1・読み書き・ジョブ。§5）、素材の欄（`asset`：事実と設定と実効値、試聴の PCM `asset::pcm`、アニメの帯 `asset::anim`）、サムネイルの道と作り方の指紋（`media::route`）・エンジンの道（`media::engine_thumbs`）・作っている途中の印（`media::inflight`）・キャッシュの容量（`media::usage`）（§7）、台帳（`library::ledger`）、DB を新しい版にするときの空きの確かめと知らせ（`library::upgrade`。§14）、ライブラリのロック（`library::LibraryLock`。M12。§3）、kura:// の経路と受け付けの判定（`serve`・`serve::access`。§4） | kura-engine, rusqlite, blake3, reflink-copy, rand_chacha, rayon, serde, thiserror, tracing, tokio |
| `kura-engine` | 編集の操作エンジン：文書・値・形・メタデータの写し方・操作と登録・描画・解析・コーデック（固定の Rust のもの）・実行とプレビュー・エディタの履歴。段 10 の部品は、実行器の `render/`（ノードのキャッシュ `cache`・差分の計画と差分の適用 `incremental`・範囲 `region`・範囲の描画とスポイト `area`・変わった範囲 `delta`・エディタの表示の経路 `view`（`Engine::render_view`）・プレビューの段 `preview`・メモリの見積もり `estimate`）、エディタのセッション `session/`（`EditSession`）、メモリの予算 `budget`、予約の部品（`monitor` の `Latest`・`Ticket`）、ベンチのワークロード `testing::bench`。M11 で 3D（glb）の読み手とソフトウェアの描画 `model`（SceneKit を使わない。決定 9。ファズの対象 `model_glb`。docs/editing.md §9.10）と、SVG の文字を描くフォントの入れ物を受ける口（`codec::SvgFonts`。中身は kura-mac が渡す）を足した。スレッドプールは作らない（呼び手のプールの中で描く）。DB・ジョブ・Tauri を知らない（ADR-134・154、§15） | serde, serde_json, thiserror, blake3, rayon, rand_chacha, libm と、版を固定したコーデックのクレート（docs/editing.md §9）。specta は feature |
| `kura-mac` | `MediaBackend` trait の macOS 実装（サムネイル生成、メディア情報、音楽のジャケット取得、WKWebView のスナップショット、「透明度を下げる」設定の取得）と、ffmpeg との振り分け。編集の Platform の段のコーデック（ImageIO・AudioToolbox の読み取り、AudioToolbox の AAC の書き出し、`kura_codec_caps`。段 9、ADR-153）と、アプリのコーデックの組 `full_codec_set`。物理メモリの大きさ `physical_memory`（編集のメモリの予算の既定に渡す。段 10）。M11 で足したもの：ImageIO のアルファを保つサムネイル・CoreText のフォントのサムネイルと情報（`thumbs`）、SVG の文字を描くシステムのフォント（`fonts`：mmap して `pinned_options` で渡す）、1 MiB 以上の確保を OS へすぐ返す確保器（`alloc` の `ReturningAllocator`。アプリと kura-devtools の `#[global_allocator]`。§7、ADR-163） | kura-core, kura-engine, serde, serde_json, tracing, libc（macOS のみ。mmap）, swift-rs（ビルド時のリンクのみ） |
| `kura-devtools` | ダミーライブラリの生成、ベンチマーク（編集の `bench edit`。NFR-11〜17）、コマンドラインからの取り込みと確認、編集のメモリの見積もりの煙のテスト（`mem_smoke`）。M11 で形式の見本（`assets`）、kura:// と CSP の自己点検のライブラリ（`selftest-library`）、`bench thumbs-job`・`bench svg-worker`・`bench analyze`・`bench migrate`・`bench query --fake-facts`（§13） | kura-core, kura-engine, kura-mac |
| `src-tauri`（`kura-app`） | コマンド（戻り値の `AppError` と code は `error.rs`。§3）、kura:// プロトコル、ウィンドウ（main の移動の制限 `navigation`。§11）、イベント、状態の保持（開く処理の鍵 `AppState::opening`）、エンジンの部品を作る口（`engine` の `MacEngineFactory`。§15）。`main` は `--kura-svg-worker` で起動されたら Tauri を始めずに SVG の働き手として動く（§7） | すべて |

`kura-core` は `MediaBackend` trait（`probe`、`image_thumbnails`、`video_thumbnails`、`audio_artwork`、`font_thumbnails`（M11））と、エンジンの部品を作る口 `edit::EngineFactory`（M11。§15）だけを知り、実装（`kura_mac::media_backend()`・`MacEngineFactory`）はライブラリを開くときに注入します。テストでは偽物の実装（`media::fake::FakeBackend`）を使います。Linux 上でも `kura-core` のテストが通るようにします。

## 3. IPC コマンド一覧（初期）

戻り値はすべて `Result<T, AppError>`。`AppError { code: string, message: string }`。code の例：`NotFound`、`InvalidQuery`、`Invalid`、`ToolMissing`、`Io`、`Db`、`Conflict`、`Internal`。ライブラリを新しい版にできずに開けないときは `LibraryTooNew`（新しい版の Kura で作られた）・`NoSpace`（空き容量）・`BackupFailed`（当てる前のバックアップ）・`UpgradeFailed`（当てる失敗。失敗した 1 つを戻した。それより前に当てたものは残り、途中の版なら `message` がそう言う）で、フロントは code で文言を選び、`message` を説明に出す（M11。docs/data-model.md §5）。ほかのアプリ（Kura を 2 つ動かした・開発版と本番版・`kura-devtools`）が同じライブラリを開いているときは `LibraryInUse`（ライブラリのロック `library.lock` を取れない。開く側は DB・バックアップ・staging の掃除・知らせのどれもしない。画面は見出し「このライブラリはほかのアプリで開かれています」と、もう一方を終了してから開き直す説明を `ja.ts` から出す。`message` は見出しを含むので説明には使わない）（M12。docs/data-model.md §1）。別のライブラリへ切り替えるとき（`library_open`）は、Rust が今のライブラリを閉じる前に開く先のロックを取り（`AppState::replace_session`）、取れなければ今のライブラリを開いたまま `LibraryInUse` を返す（フロントは知らせの一行）。開いているのと同じライブラリを開き直すとき（場所の書き方が違っても、ロックファイルのデバイスと i ノードで見分ける）は、閉じてから開く。作るとき・ライブラリでない場所を開くときの失敗は、今までどおり前のライブラリも閉じている。その場で用意する表示（アニメの帯）が大きすぎるときは `TooLarge`（1 辺 8,192・16M 画素・メモリの予算の全体を超える。やり直しても同じ）、メモリの予算が空いていないときは `Busy`（少し待てば通る。画面は「混んでいます。もう一度押してください」）（M11）。保存（M12）の kura-core のエラーは、`TooLarge`（文書・値・メモリの予算の全体を超える）が `TooLarge`、`Unsupported`（書けない出力の形式など）が `Invalid`、`Mismatch`（途中の版を作り直した値が記録と合わない・アンカーが壊れている）が `Conflict` になる。

| 領域 | コマンド | 説明 |
|---|---|---|
| ライブラリ | `library_create(path)` / `library_open(path)` / `library_info()` | ライブラリの作成・オープン・概要 |
| | `library_restore()` → `{ info, lastPath, error? }` | 起動時に前回のライブラリ（`KURA_LIBRARY` があればそれ）を開く。前回の場所が見つからなければ `info` も `error` も空で前回の場所を返し、開けなければ `error`（`AppError`。上の code）に理由を入れて返す（失敗にはしない） |
| | イベント `library://upgrade`（`{ phase: started \| finished \| failed, path, fromVersion, toVersion, dbBytes, elapsedMs, error }`） | `library_restore`・`library_open` の中で DB を新しい版にするとき、`started` のあと `finished` か `failed` を 1 つ送る（当てるものがない・新しいライブラリでは送らない）。フロントは `started` から 300ms を超えたら「ライブラリを更新しています」を出す（M11 の決定 1。`stores/upgradeStore`）。知らせは開く IPC の中で来るので、フロントは開く IPC より先に購読し（`watchUpgrades`）、`library_restore` は起動の間に 1 回だけ呼ぶ（`libraryStore.restore`）。Rust は開く・作る・戻す処理を 1 つの鍵（`AppState::opening`）で直列にする |
| | `library_check_location(path)` → `{ path, status, syncService }` | 置き場所の確認（`.kuralib` の補完、`new` / `existingLibrary` / `occupied`、同期フォルダの中か） |
| 取り込み | `import_preview(paths)` | 種類別件数・合計サイズ・重複の見込み・フォルダ木を返す |
| | `import_start(paths, options)` → `JobId` | 取り込みジョブを開始 |
| 一覧 | `items_query_ids(utcOffsetMinutes, view?, collapseUnits?, query?, sort?)` → 生バイト | 一覧の ID と縦横比を並び順で、日付見出しのグループ付きで返す。`[件数 u32][id u32 × N][縦横比×1000 u16 × N][グループ数 u32][開始位置 u32 × G][日 i32 × G]`（LE）。縦横比が分からないもの（音楽など）は 1000。日は `utcOffsetMinutes` の時差で区切った 1970-01-01 からの日数。tauri-specta を通さず `tauri::ipc::Response` で返す（フロントには ArrayBuffer で届く。ラッパーは `src/ipc/items.ts`。ADR-33） |
| | （`view`） | `{ kind: 'all' \| 'recent' \| 'unsorted' \| 'untagged' \| 'trash' }`、`{ kind: 'random', seed }`、`{ kind: 'folder', id }`（子孫フォルダの分も含む）、`{ kind: 'unit', id }`（ユニットの並び順）、`{ kind: 'smart', id }`（スマートフォルダ）。`query` は検索の文字列（docs/search-query.md。解析できなければ `InvalidQuery`）で、一覧の種類の条件に AND で重ねる。`sort` は `{ key: imported \| captured \| name \| rating \| duration \| size, desc }`。日付の見出しは追加日・撮影日で並べたときだけ（撮影日のないものは日 `-2147483648` のグループ）。ランダム・ユニットの中は見出しなし、ゴミ箱は入れた日の新しい順で、並び順は使わない |
| | （`collapseUnits` が true のとき） | set ユニットに属するアイテムを、最初に一致したメンバーの位置で 1 件のユニットにまとめる。ユニットは `0x8000_0000 \| unit_id` で表し、縦横比は表紙のもの。ゴミ箱とユニットの中ではまとめない |
| | `items_get(ids)` → `ItemSummary[]` / `units_get(ids)` → `UnitSummary[]` | 最大 200 件。要求した順で返し、ないものは省く。M11：概要に素材の表示の値（`thumbRev`・`analysisState`・`alpha`・`pixelArt`（実効値）・`pixelArtSetting`（null は自動）・`animated`・`frameCount`（格子があれば帯で使う数）・`hasLoop`）を足し、ユニットの表紙（`cover`）にも `thumbRev`・`analysisState`・`alpha`・`pixelArt`。実効値は素材の欄（`item_asset_get`）と同じ関数（`kura_core::asset::effective`）で決める |
| | `item_detail(id)` → `ItemDetail` | タグ・フォルダ・ユニット（set のメンバー、または参照フォルダに入っている folder ユニット）・取得元を含む |
| 素材（M11） | `item_asset_get(itemId)` → `AssetDto` | インスペクタの素材の欄：解析の進み具合（`analysisState`。DB の綴り）、解析の事実（`facts`：画像は透過・色数・ドット絵の倍率と自動の判定・コマの数、音声はラウドネス・ピーク・ループ点・レート・チャンネル・ビット。台帳の行を経て `media_facts` から。未解析は null）、利用者の設定（`settings`：ドット絵・倍率・格子・コマの長さ・ピボット・タグ。`asset_settings`）、実効値（`effective`：ドット絵は「設定 ?? 自動 ?? 偽」、アニメとして回せるか、コマの数）、格子の推定（`gridEstimate`。M12：解析の結果 `image.sprite_grid` が今のエンジンの版のときだけ、最良の候補の格子・列・行・中身のあるセルの数。使うコマの数は中身のあるセルの数で、すべてに中身があれば null。版違い・候補なし・読めない・画像の大きさに合わないものは null。格子のシートの「推定を使う」が使い、自動では当てない）、フォントの情報（`font`：書体名・スタイル・字の数・形式）、3D の情報（`model`：頂点・三角形・材質・大きさ（範囲の max − min。`codec_info` の `model.size`）・描けない拡張）。DB だけを読む |
| | `item_asset_settings_set(ids, patch)` → 変えた数 | 右クリックとメニューバーの「ドット絵として表示 ▸」の複数の選択にも使う（一覧のキャッシュにない ID もそのまま送る）。画像だけに付ける（ほかの種類・ない ID は飛ばす）。`patch` は省いた項目を変えない：`pixelArt`（`auto`・`yes`・`no`）、`pixelScale`・`frameMs`（0 は自動）、`grid`・`pivot`（`"clear"` か `{ set }`）、`animTag`（空文字は自動）。セルが 1 つもできない格子・セルの数を超えるコマの数があれば、どれも変えずに `Invalid`。すべて自動に戻れば行を消す |
| | `item_anim_get(itemId)` → `AnimDto` | アニメの帯の情報（`hash`・`version`・`rev`・`canvasW`・`canvasH`・`cols`・`frameCount`・`durations`（ms。0 はそのまま）・`tags`（`name`・`from`・`to`・`direction`（`forward`・`reverse`・`pingPong`・`pingPongReverse`）・`repeat`））。帯がなければその場で作って `anim/ab/<hash>-1.png` に書く（ジョブにしない。§4）。コマ f の左上は `((f % cols) × canvasW, ⌊f / cols⌋ × canvasH)`。帯は `/anim/{hash}/{version}`、`rev` が 0 でなければ `?r={rev}` を付ける（格子を変えると変わる）。1 枚の画像で格子もなければ `Invalid`、大きすぎれば `TooLarge`、混んでいれば `Busy` |
| | `items_selection_info(ids)` → `SelectionInfo` | 複数選択のインスペクタ用：評価とメモ（全件が同じときだけ）、タグとフォルダ（付いている件数付き） |
| | `library_counts()` → `{ all, recent, unsorted, untagged, trash, captures }` | サイドバーの件数。グリッドを出したあとに別に取る（NFR-01 に響かないように） |
| エディタ（M12） | `edit_open(itemId, viewport, fromDraft?)` → `{ session, itemId, baseVersion, source, pixelArt, draft: { exists, fresh, updatedAt, opened }, result }` | 画像エディタを開く（ライブラリに 1 つ。前のエディタは閉じる。画像だけ）。元を読み（アンカーがあればアンカー）、予算の取り置きを立てる。下書きがあれば `draft.exists`（`fresh` が偽なら、その土台で保存すると `Conflict`）。`fromDraft: true` なら下書きの文書と土台で開く（保存していない文書）。`result` は下の `edit_step` と同じ形（文書は全体）。読む予算が全体を超えれば `TooLarge`、空いていなければ `Busy` |
| | `edit_step(session, steps, viewport)` → `{ status, created, rejected, doc, view, notices }` | 手をまとめて当て、描くのは最後に 1 回（docs/editing.md §12.12）。手は `commit { edit, mode }`（`edit` は kura-engine の `Edit`、`mode` は `step`・`merge`）・`preview { edit }`・`commitPreview { mode }`・`cancelPreview`・`undo`・`redo`・`seal`・`paint { actions }`（描画の動作を 1 手で：末尾が有効な `paint.actions` なら追記、ほかなら末尾に足す）。`viewport` は `{ frame, rect（等倍のキャンバスの座標。null は全体）, scale（等倍の 1 画素あたりの装置の画素）, interacting, focus }` で、段は Rust が決める。`status` は `{ rev, head, provisional, canUndo, canRedo, dirty, saving, needsReopen }`、`doc` は前に返した文書からの差分（`base`・`order`・変わったノード（追記の配列は数だけ）・`outputs`）、`view` は `shown { view: { rev, level, …, changed } }`・`superseded`（新しい要求に取って代わられた。手は残る）・`failed { problem }`（手は当たったまま）。計画を作れない手は `rejected { index, problem }` で断り、状態を変えない（指摘はノード・引数つき）。`changed` のタイルを §4 の `/edit` で取り直す |
| | `edit_view(session, viewport)` → 同上 | 表示だけ（パン・ズーム・段の変更）。保存のジョブが終わったあとの状態（土台・`dirty`・`needsReopen`）もこれで取り直す |
| | `edit_sample(session, frame, x, y)` → `{ rgba, index }?` | スポイト：今の rev の等倍の値の 1 画素（キャンバスの座標。画像の外は null） |
| | `edit_close(session)` | 閉じる（中止 → キャッシュを空に → 確保を落とす → 取り置きを 0。待っている下書きは置く）。今のセッションでなければ何もしない。ライブラリを閉じる・切り替える・終了でも同じことをする |
| | `edit_save(session, mode, format?)` → `{ job, status, doc }` | 保存のジョブを積む（`mode` は `overwrite`・`saveAs { title, stem }`・`revert { to }`。`format` は kura-engine の `FormatChoice` で、渡すと先に出力の形式を変える手を確定する）。保存の間も編集を続けられ、保存が待っている間は次の保存を `Busy` で断る。終わったら `edit_view` で状態を取り直す（版に戻したら `needsReopen` が真になるので開き直す） |
| | `edit_versions(itemId)` → `EditVersionDto[]` | 版の一覧（版の順。`version`・`kind`（`original`・`edit`・`revert`）・`revertTo`・`ext`・`format`・大きさ・`fileState`・`createdAt`。まだ保存していなければ空） |
| | `edit_draft_discard(itemId)` → bool | 下書きを捨てる（あったら真。開いているエディタの待っている下書きも取り消す） |
| 編集 | `items_update(ids, patch)` | タイトル（1 件のときだけ）・評価・メモ（全件に同じ値）。メモの空文字は消す |
| | `entries_update(itemIds, unitIds, { rating?, note?, addTags?, removeTags? })` → `{ itemIds, members }` | 選択（アイテムとユニット）に評価・メモ・タグをまとめて付け外しする。ユニットはユニット自体とメンバー全員に（docs/playlists.md §2.1）。1 つのトランザクション。返すのは変えたアイテム（重複なし）と、そのうちメンバーの数。タグの名前は「名前」か「名前空間:名前」（全角の「：」も可、NFC）で、なければ作り、別名なら正式タグに置き換える |
| | `items_trash(ids)` / `items_restore(ids)` → 件数 / `trash_empty()` → `{ items, bytes, failed }` | ゴミ箱。空にするときは DB の行を先に消し、同じトランザクションでどの表からも参照されなくなった台帳の行を解放する（`ledger::release`。M11）。コミットのあとで、解放した中身の実ファイルと派生のファイル（サムネイル・ジャケット・プロキシ・波形の段・アニメの帯。`Layout::derived_paths`）を消す（ADR-46、docs/data-model.md §4「台帳で数える」）。取り込みとはライブラリの 1 つの鍵で直列にする。`failed` は消せなかったファイルの数 |
| タグ | `tags_suggest(prefix, limit)` → `{ tag, count, alias }[]` | 前方一致 → 部分一致、使われている数の多い順。「名前空間:名前」なら名前空間で絞る。別名でも一致する |
| | `tag_detail(id)` / `tag_update(id, patch)` / `tag_merge(from, into)` / `tag_delete(id)` | 名前空間・名前・色・別名・親タグ（`aliases`・`parents` は置き換え、循環は不可）。統合すると `from` の名前は別名として残る |
| フォルダ | `folders_tree(withCounts?)` | すべてのフォルダ（木はフロントで組む）。件数は子孫フォルダの分を含み、同じアイテムは 1 回（ADR-40）。参照する folder ユニットの ID も返す |
| | `folder_create(parentId, name)` / `folder_update(id, { name?, color? })` / `folder_move(id, parentId, beforeId)` / `folder_delete(id)` | 同じ親の下で名前は重複不可（`Conflict`）、「/」は不可。自分の中へは移せない。削除は中のフォルダごと（アイテムは残る。参照がなくなった folder ユニットも消す） |
| | `items_set_folders(itemIds, add, remove)` | フォルダへの所属の付け外し |
| ユニット | `unit_detail(id)` / `units_list(filter, kind?, limit)` | インスペクタ用の詳細、選ぶための一覧（更新の新しい順） |
| | `unit_create_set(name, itemIds)` / `unit_create_folder(name, folders)` | set は渡した順＝manual、表紙は先頭。folder は名前順 |
| | `unit_update(id, patch)` | 名前・表紙・並び順・逆順・ユニット内シャッフル・画像の表示時間（0 で既定）。folder は manual にできない。評価・メモ・タグは `entries_update` で（メンバーにも付ける） |
| | `unit_add_items(id, itemIds)` / `unit_remove_items(id, itemIds)` / `unit_reorder(id, itemIds)` / `unit_set_folders(id, folders)` / `unit_delete(id)` | set のメンバーの追加・削除・並べ替え（並べ替えると manual になる）、folder の参照フォルダの置き換え、解除（アイテムは残る）。`docs/playlists.md` §2 |
| スマートフォルダ | `smart_folders_list(withCounts?, utcOffsetMinutes)` / `smart_folder_save(name, query)` / `smart_folder_update(id, name?, query?)` / `smart_folder_delete(id)` | 条件は検索の文字列のまま保存（解析できない条件は保存しない）。件数は一致するアイテムの数 |
| 検索 | `query_parse(text, view?, utcOffsetMinutes)` → `{ error?, clauses, counts? }` | ⌘K のチップ・件数・エラー表示用。`error` は位置（UTF-16）と理由、`clauses` はいちばん外側の AND の条件、`counts` は開いている一覧の中で一致する種類ごとの件数 |
| | `query_suggest(word, limit)` → `Suggestion[]` | 入力中の語の候補：タグ → フィールド名・値 → 保存済みの検索（`kind` / `insert` / `prefix` / `label` / `count` / `id`） |
| プレイリスト | `playlists_list()` → `PlaylistSummary[]` / `playlist_get(id)` → `Playlist` | サイドバーの一覧（並び順・件数）と、画面用の設定一式（保存した再開の位置 `resume` を含む） |
| | `playlists_reorder(ids)` | サイドバーの並びを変える（`ids` の順。含まれないものは後ろに今の順で。M9） |
| | `playlist_create(name, kind, granularity)` / `playlist_update(id, patch)` / `playlist_delete(id)` | smart は作るとき シャッフル・条件なし（すべて）・上限 50 件で始める。`patch` は名前・設定・条件（`rule`：`{ match, clauses }`、解析できなければ `InvalidQuery`）・上限・並び順。画像の表示時間は 1〜60 秒 |
| | `playlist_set_granularity(id, granularity)` | 非破壊（§3.4）。値を変えるだけ |
| | `playlist_contents(id)` → `{ rows, flatEntryIds, flatItemIds, itemCount, images, audios, videos, fonts, models, totalMs }` | 画面の一覧：エントリごとの行（名前・表紙・件数・長さ・種類、unit / folder は中身のアイテム）と、展開後のアイテムとそのエントリ（ファイル単位の一覧。並び順）・件数・合計。種類ごとの件数のうちフォント・3D（`fonts`・`models`。M11）は、一覧には出すがキューには入らないもの |
| | `playlist_add(id, sources, invert, groupName?, before?)` → `{ entryIds, unitId? }` | §3.5 の表どおりに追加する（`invert` は ⌥、`groupName` は「1 つのユニットにまとめて追加」で set ユニットを作る）。`before` のエントリの前に、なければ末尾に |
| | `playlist_move(id, entryIds, before?)` / `playlist_remove(id, entryIds)` | 並べ替え（pos の中間値、差が 1e-9 未満なら振り直す）・削除 |
| | `playlist_snapshot_to_files(id, entryIds?)` → 元のエントリ / `playlist_restore_entries(id, entries)` | 「ファイルに固定」（省けば全体）と、その取り消し（ADR-82） |
| | `playlist_regenerate(id, utcOffsetMinutes)` | smart の並びを新しいシードで作り直す（「作り直す」。ADR-79） |
| | `playlist_save_state(id, state?)` | 再開の位置（シード・スロット番号・アイテム・再生位置）を保存する。null で消す（§6.5、ADR-86） |
| 再生 | `playback_support_set(entries)` | 起動時にフロントが `canPlayType` で作った「入れ物 × コーデック」の表（§8）。表が前回と変わっていれば、作り済み以外のプロキシの判定をやり直す |
| | `play_queue(source, request, utcOffsetMinutes)` → `Queue` | 再生するキューを作る（M6 で `player_queue` を置き換え）。`source` は `playlist { id }`（その設定どおり。シード・smart の並びを保存する）、`selection { entries }`（アイテムと `0x8000_0000 \| unit_id` を並び順のまま。ユニットを含めばユニット単位）、`view { view, query, collapseUnits }`（⌘K の ⇧↵：開いている一覧の中で条件に合うもの）。`request` は `resume`・`shuffle`（S で切り替え）・`reseed`（並べ直す）・`startItem`（そのアイテムのユニットを先頭に）・`fromItem`（並びはそのままでそこから。キューにない種類（フォント・3D）なら、一覧でその行より後ろの最初に再生できるものから。ユニット単位のエントリの行と開いた中身の先頭のメンバーの行は、そのエントリの最初のスロットから。docs/playlists.md §1.1）・`fromEntry`（`fromItem` を押した一覧の行のエントリ。同じアイテムが複数のエントリにあっても、その行のエントリで探す。M11）・`seed`。`Queue` は Slot 列を列ごとの配列で持つ：`itemIds` / `unitIds` / `indexInUnit` / `unitLen` / `imageDurationMs`（音楽・動画は null）と、ユニット単位のときのまとまり `groups`・`groupIndex`、`seed`（16 進）・`startIndex`・`startPositionMs`・`totalMs`、プレイリストならその設定 `playlist`（ADR-84） |
| | `playback_report({ itemId, unitId, unitStart, playlistId, playedMs, completed, durationMs })` → 記録したか | スロットを離れたときに送る。再生済みの判定（playlists.md §6.4）は Rust がし、再生済みなら `play_events` と回数・日時を更新する |
| | `proxy_request(ids, force)` | 再生キューの今と次のプロキシを先に作ってもらう。`force` は元のまま再生できなかったものを作成待ちに戻す（§8） |
| | `open_external(hash)` → `mpv` \| `quickTime` | 元のファイルを外部プレイヤーで開く（mpv があれば mpv） |
| | `player_prefs_get()` / `player_prefs_set(prefs)` | 再生の設定（音量・画像の間も直前の音楽を流す・BGM をループ・画像の表示時間の既定値・選んだ短い音声をすぐ鳴らす `sfxAudition`（M11。既定でオン。前の app.json にない場合もオン））。app.json に保存 |
| ジョブ | `jobs_list(filter)` / `job_cancel(id)` / `job_pause(id)` / `job_resume(id)` / `job_retry(id)` / `job_log(id)` | 進捗はイベント `job://progress` |
| 解析 | `request_analysis(ids)` → 待っているか | インスペクタで選んだものを先に解析する（まだなら解析のジョブを積む。取り込み・サムネイルが動いている間はそれを待つ。§5） |
| | `retry_analysis(ids)` → 戻した数 | 解析できなかったもの（failed）を解析待ちに戻し、先に解く |
| 書き出し | `export_start(dest, entries)` → JobId | 選んだもの（一覧の ID。ユニットは印付き）を書き出すジョブを積む。書き出し先はあるフォルダで、ライブラリの中は受け付けない（§14） |
| バックアップ | `backups_list()` → `Backup[]` / `backup_now()` → `Backup` | DB のバックアップの一覧（新しい順）と、今すぐ作る（§14） |
| | `reveal_path(path)` | Finder で見せる（書き出したもの・バックアップ・ライブラリ） |
| タグ | `tags_list()` → `TagListEntry[]` | すべてのタグと、付いているアイテムの数（ゴミ箱を除く）・そのうち自動で付いた数（`item_tags.source` が download / capture）・ユニットの数・絞り込みの条件。名前空間なしを後ろに、名前空間ごとに付いている数の多い順（ADR-126） |
| 設定 | `library_usage()` → `{ libraryBytes, volumeTotalBytes?, volumeAvailableBytes? }` / `library_check()` → `Integrity` | ライブラリのファイルの合計（ゴミ箱を含む）とボリュームの容量・空き、整合性チェックをいま行う（M9） |
| | `library_cache_usage()` → `{ thumbsBytes, thumbsFiles, peaksBytes, peaksFiles, animBytes, animFiles }` | 中身から作ったキャッシュの大きさ：サムネイル（`thumbs/` と `art/`）・波形（`peaks/`）・アニメの帯（`anim/`）。ファイルを数えるので設定の画面を開いたときだけ呼ぶ（ゴミ箱の分を含む。M11 の決定 4。§7） |
| | `download_archives()` → `ArchiveStatus[]` / `download_archive_clear(tool)` | 取得済みの記録の件数と、消す（`archives/` のツールごとの 1 ファイル。M9） |
| ダウンロード | `download_detect(url)` → `{ url, site, siteLabel, tool, rule?, playlist, toolFound, capture?, previous? }` | URL の取得方法（規則 → サイトの表 → yt-dlp の汎用）。`previous` は前に同じものをダウンロードしたとき（ジョブ・題名・日時・入れた数・今ライブラリにある数。ADR-132）。§9 |
| | `download_defaults()` / `download_enqueue(url, options, redownload)` → JobId | 前回の選択（ライブラリの settings）と、取得を積む（選んだ内容を次回の既定にする）。`options` は画質（best / 1080 / 720 / 480）・音声のみ・保存先のフォルダ・自動タグ（投稿者・サイトのタグ）・再生リストをユニットに。`redownload` は取得済みの記録にあるものも取り直す（ADR-132） |
| | `downloads_list(limit?)` → `DownloadJob[]` | ダウンロードのジョブを新しい順に（URL・サイト・ツール・題名・件数・入れたアイテムとユニット） |
| | `download_prefs_get()` / `download_prefs_set(prefs)` | Cookie の読み込み元と規則（app.json。規則の正規表現と引数を確かめる。ADR-99） |
| ジョブ | `job_retry(id)` / `job_prioritize(id)` / `job_log(id)` → 文字 | 再試行・先に取得・ログの末尾 256KB（ADR-103） |
| キャプチャ | `capture_rules_list()` / `capture_rule_save(id?, rule)` / `capture_rule_delete(id)` / `capture_rules_reorder(ids)` | ルール（名前・URL パターン・撮り方・対象・非表示・幅・スクロール・待ち・保存先・有効）。保存のときに確かめる（http(s) のパターン、要素のみなら対象が要る、幅 320〜3840） |
| | `capture_run(url, ruleId)` → JobId | 撮影のジョブを積む（`download_detect` の `capture` に一致したルールが入る） |
| | `capture_login(url?)` / `capture_login_status()` → bool / `capture_login_snapshot(fullPage)` → `{ title, url, itemIds, added, ruleName?, truncated }` / `capture_login_close()` | ログイン用の Chrome を開く（開いていれば新しいタブで URL）、開いているか、前面のタブを撮る、閉じる（ADR-111）。開いているあいだは ⌃⌥⌘C でも撮れ、結果はイベント `capture://login-shot`（`{ ok, message, result? }`。ADR-112） |
| | `browser_open(url)` / `browser_status()` → URL? / `browser_close()` / `browser_snapshot()` → 同上 | 見たままキャプチャ（アプリ内ブラウザ。ADR-109） |
| ツール | `tools_status()` → `ToolInfo[]` / `tools_set_path(tool, path?)` / `tools_update()` → `{ brewCommand?, outputs }` | 版・場所・Homebrew 版か。場所の指定（app.json）。「更新を確認」（ADR-100） |
| 設定 | `settings_get(keys)` / `settings_set(map)` | |
| 外観 | `appearance_get()` → `Appearance` | OS の表示設定（「透明度を下げる」）。起動時に `<html data-reduce-transparency>` へ反映する（§11） |
| 開発 | `bench_report(name, result)` | `VITE_KURA_BENCH=1` で起動したときのスクロール計測と、`VITE_KURA_SELFTEST=1` の自己点検（§4.2）・`VITE_KURA_CHECK=1` の試聴の継ぎ目の自己点検（`audition-seam`）の結果をログに書く（§13） |

変更（patch）の型は、省いた項目を変えない。色・メモの空文字は「消す」の意味（`Option<Option<T>>` を JSON で表しにくいため）。

bindings.ts には、コマンドとイベントのほかに Rust の表を定数で出す（`Builder::constant`。M11）：`ITEM_KINDS`（種類の順・キューに入るか・プロキシ・長さ。フロントは `src/ipc/kinds.ts` だけが読む）、`QUERY_FIELD_NAMES`（検索のフィールドの名前）・`QUERY_KIND_WORDS`（種類の語）・`QUERY_SYMBOLS`（半角と同じに扱う記号）（docs/search-query.md §3）、`ANALYSIS_ERROR_CODES`（解析の失敗の理由。`AssetFactsDto.errorCode` の値で、フロントは `src/ipc/asset.ts` から読み、理由ごとの文言と照らす）、`EDIT_OPS`（M12。編集の操作のカタログ：今の版の操作の ID・版・入力（`signature`）・引数の記述（`schema`：種類・範囲・単位・既定値・表示の条件 `visible`）。エディタの「操作を追加」と汎用の引数パネルはこれを読む。docs/editing.md §7.2）。エディタとの境界の型（手 `Edit`・`CommitMode`・文書 `Graph`・`OutputSpec`・`FormatChoice`・指摘 `Issue`・カタログ `OpDescriptor`）は、kura-engine の feature `specta` で出す（引数の JSON は `JsonValue`）。

コマンドの追加は `.claude/skills/add-command` の手順で行い、この表に追記します。

## 4. kura:// プロトコル

`tauri::Builder::register_asynchronous_uri_scheme_protocol("kura", …)` で登録します。macOS では `kura://localhost/<path>` の形で参照します。

| パス | 返すもの | ヘッダ |
|---|---|---|
| `/thumb/{hash}/s` | 長辺 512px サムネイル（`png`、`jpg` の順に探す。M11） | `Cache-Control: public, max-age=31536000, immutable` |
| `/thumb/{hash}/l` | 長辺 1600px サムネイル | 同上 |
| `/art/{hash}` | 音楽のジャケット | 同上 |
| `/file/{hash}` | 元ファイル | `Accept-Ranges: bytes`、Range があれば 206 と `Content-Range` |
| `/play/{hash}` | 再生用：プロキシがあればプロキシ、なければ元ファイル | 同上 |
| `/peaks/{hash}` | 波形の段（`kura-peaks` 版 1。`peaks/ab/<hash>.peaks`。解析のジョブが書く。M11） | `immutable`。解析していなければ 404 |
| `/pcm/{hash}/{start}-{end}` | 試聴用の PCM（WAV。フレーム `[start, end)`。その場で作る。M11） | キャッシュしない（`no-store`）。Range は見ない |
| `/anim/{hash}/{版}` | アニメの帯の PNG（`anim/ab/<hash>-<版>.png`。`item_anim_get` が書く。今の版は 1。M11） | `immutable`（格子の帯は `?r={rev}` で区別する） |
| `/edit/{session}/{rev}/{段}/{frame}/{tx}/{ty}` | エディタの表示のタイル（1 辺 256・フレームの画像の座標の RGBA8。`{ty}` か `{ty}.rgba` は行を詰めた非圧縮、`{ty}.png` は PNG。その場で表示の値から作る。M12） | `immutable`。Range は見ない |

- Range は 1 区間だけ対応（`bytes=start-end` / `bytes=start-` / `bytes=-末尾の長さ`）。範囲外・不正な Range は 416。応答本体は最大 4MB ずつ返す（`bytes=start-` には先頭から 4MB だけ）。
- `{hash}` は 64 桁の小文字 16 進だけを受け付ける。それ以外とファイルがないものは 404、ライブラリが開かれていなければ 503。`/pcm` の `{start}-{end}` は `start < end` の 10 進（u64 にあふれるもの・逆の範囲・片側だけは 404）。
- 実際の置き場所：`thumbs/ab/<hash>_s.{png,jpg}`・`_l.{png,jpg}`（透明のあるものは PNG。両方あれば PNG を返す）、`art/ab/<hash>.jpg`、`proxies/ab/<hash>.mp4`、`peaks/ab/<hash>.peaks`、`anim/ab/<hash>-1.png`、`files/ab/cd/<hash>.<ext>`（`/file`・`/play` は拡張子が分からないので `files/ab/cd/` の中を探す）。帯の版が今のもの（1）でなければ 404。
- **`/pcm`（試聴の PCM。kura-core の `asset::pcm`）**：files/ の音声のファイルを、ライブラリのエンジンのコーデックの組（解析と同じ）で読み、区間を WAV にする。並びは `RIFF`・`fmt `（16 バイト。形式 1 = 16 ビット整数か 3 = 32 ビット浮動小数）・`kura`・`data` の順で、`kura` チャンク（版 1）に開始フレーム・元のフレーム数・ループ点・デコーダの識別子を入れる（同じ読み取りの値。試聴は DB ではなくこれを使う。正は `crates/kura-core/tests/vectors/pcm/wav.json`、TS は `src/lib/asset/wav.ts`）。元が 16 ビット以下の整数で、区間の標本がすべて 16 ビットの格子の上にあれば 16 ビット、ほかは浮動小数（非可逆の形式は浮動小数）。ループ点は「始まり < 終わり ≤ 長さ」のものだけ。
  - 読んだ結果は (ライブラリ, ハッシュ, デコーダの識別子) で直近の 1 件だけ覚える（前奏と本体・継ぎ目の前後を続けて取るので、2 回目からは読まない。読むのは 1 つずつ）。覚えている間はそのライブラリの予算（対話）を持つ。置き場はライブラリごと（`EngineServices::pcm_cache`）で、ライブラリを閉じる・切り替えるとすぐ手放す（予算を 2 つにしない。M11 の検証 E2）。
  - フロント（`src/lib/assetAudio.ts` の `Audition.startProbed`）は、まず `[0, 1)` を取って `kura` チャンクの長さ・ループ点を知り、それで計画した区間（本体・前奏・継ぎ目の前後）を取る。長さ（フレーム）は items・media_facts の列にも `item_asset_get` の DTO にもない（DB では解析の読み取りの記録 `kura.read`（analysis_results）にだけある）うえ、PCM と同じ読み取りの長さ・ループ点を使えば、デコーダが変わっても標本がずれないため。最初の要求で読んで覚えるので、続く要求は読まない（M11）。覚えている間は値の分の予算（対話）を持ち、60 秒使わなければ捨てる。
  - 音声でない・ファイルがなければ 404、範囲が音声の外か、浮動小数で 64 MiB を超える長さは 416、エンジンがない・予算（対話。待たない）が空いていなければ 503、読めなければ 500。どれも §4.1 の 4 つのヘッダを付ける。
- **`/edit`（エディタのタイル。kura-core の `edit::editor::tile`。docs/editing.md §12.13）**：`{session}` は `edit_open` が返す値（小文字の 16 進、1〜64 文字）、`{段}` は 0〜3、ほかは符号・空白のない 10 進。応えるのは、rev と段がエディタの表示した今か直前の表示と同じで、そのタイルが有効なときだけ（`edit_step`・`edit_view` の `view.rev`・`view.level` で取る）。古い rev・違う段・範囲の外・閉じた（知らない）セッション・読めない綴りは 404、エンジンがなければ 503。中身は (セッション, rev, 段, フレーム, タイル) で決まる（rev はセッションの中で中身ごとに振り、セッションは再起動をまたいで重ならない）ので immutable。描いている途中の錠を待たない。非圧縮か PNG かは段 5 で WebKit で測って決める。
- **`/pcm` と `item_anim_get` をジョブにしない理由**（M11 の決定 8）：利用者がその場で待つ短い表示の用意で、長くても 0.4 秒ほど（release・M4 Max の実測：3 分の 44.1kHz ステレオの最初の要求が WAV 91ms・FLAC 177ms・Ogg Vorbis 144ms、2 回目から 0.1〜0.6ms。帯は GIF 64 コマ 128² が 25ms、APNG 64 コマ 256² が 80ms、上限いっぱいの 8,192 × 2,048 が 189ms、すでにあれば 0.1ms 未満）。`spawn_blocking` で読み、予算は対話（`Class::Interactive`。待たない）で確保し、足りなければ 503 か `Busy`・`TooLarge` で返す。ジョブの列に並べると、解析・サムネイルのジョブの後ろで待たされる。ステレオの Opus はアプリの組では AudioToolbox が読み、3 分（48kHz）の最初の要求が 0.32 秒。例外：AudioToolbox が断って ffmpeg に回る形式（多チャンネルの Opus・Ogg の FLAC と Speex・圧縮の WAV と AIFF・MP3 の layer 1・2。docs/editing.md §9.2）は ffmpeg を 2 回起こす（形を数える回と読む回）ので遅く、3 分の最初の要求が Ogg の FLAC（ステレオ）で約 0.3 秒、6 チャンネルの Opus で約 1.4 秒かかる（2026-10-20、release・M4 Max、ffmpeg 9.0.2。アプリと同じ `full_codec_set` で `asset::pcm::serve` を呼んで測った）。
- WKWebView の `<video>` はサンプル単位の小さな Range（数百バイト）を多数送ってくる（60 秒・1.4MB の MOV の再生で約 3,000 回）。今は 1 回ごとにファイルを開いて読む。大きな動画での負荷は M5 で測る。
- `RUST_LOG=kura_app_lib=debug` で要求ごとに label・method・uri・Origin・range・status・bytes をログに出す。
- `Content-Type` は拡張子から決める。`nosniff` を付けるので、`<img>` で出す SVG は `image/svg+xml` で返す（`application/octet-stream` では表示されない）。
- ファイルを開く処理は `spawn_blocking` で行う。受け付けの判定はその前に行う。

### 4.1 受け付けと応答のヘッダ（M11）

判定と付けるヘッダは `kura-core` の `serve`（`admit`・`response_headers`・`AppOrigin`）に純粋な関数として置き、CI（Linux）で回す。src-tauri はそれを HTTP の型に写すだけ。

| 要求 | 応答 | CORS のヘッダ |
|---|---|---|
| main 以外の WebView（`capture-browser` など。`UriSchemeContext::webview_label()`。ページから偽れない） | 403。ファイルに触れない（`spawn_blocking` の前に返す） | 付けない |
| main から、`Origin` があってアプリの origin と違う（`null` を含む） | 403 | 付ける |
| main から `OPTIONS` | 204（`Access-Control-Allow-Methods: GET, HEAD, OPTIONS`・`Access-Control-Allow-Headers: Range`・`Access-Control-Max-Age: 600`）。WebKit は独自の scheme に事前確認を送らないが、来ても害がないように答える | 付ける |
| main から `GET`・`HEAD`・`OPTIONS` 以外 | 405（`Allow: GET, HEAD, OPTIONS`）。kura:// は読むだけ | 付ける |
| main から `GET`・`HEAD`（`Origin` なし、またはアプリの origin） | 200・206・404・416・500・503（`HEAD` は本体なし） | 付ける |

main への応答には、エラーを含めてすべて次を付ける（エラーに ACAO がないと、fetch が状態を読めず TypeError になる）。

| ヘッダ | 値 | 理由 |
|---|---|---|
| `Access-Control-Allow-Origin` | アプリの origin（起動時に 1 回決める固定の値。`tauri dev` なら devUrl の `scheme://host:port`、それ以外は `tauri://localhost`）。`*` は使わない | fetch・`crossorigin` の要素・`getImageData`・`decodeAudioData`・CSS の `mask-image` が読める。`*` だとどの origin からでも読める |
| `Access-Control-Expose-Headers` | `Content-Range, Accept-Ranges, Content-Length` | Range の fetch で位置を読む |
| `Content-Security-Policy` | `sandbox` | kura:// の SVG が文書として開かれても、スクリプトが動かない（`<img>`・fetch・フォント・音声・動画・デコードには効かないことを確かめた） |
| `X-Content-Type-Options` | `nosniff` | 中身から型を推測させない |

アプリの origin は `url::Url::origin()` に任せない（`tauri:` に対して opaque を返すので、Tauri と同じく手で組む）。

**CSP**（`tauri.conf.json` の `app.security.csp`。`csp_problems` が足りない点を返し、tauri.conf.json を読むテストが CI で見張る）：

```
default-src 'self' ipc: http://ipc.localhost; img-src 'self' kura:; media-src 'self' kura:;
connect-src 'self' ipc: http://ipc.localhost kura:; font-src 'self' kura:; style-src 'self' 'unsafe-inline';
object-src 'none'; frame-src 'none'
```

- `connect-src` は `default-src` を上書きするので、`ipc: http://ipc.localhost` を書き直す（書かないと invoke が止まる）。
- `blob:`・`data:`・`*` は足さない（メディアの base64 を禁じるのと同じ向き）。`script-src`（なければ `default-src`）に `kura:` を入れない。
- 外部のフォントや CDN は読み込まない（書体は SF Pro とヒラギノのみ）。例外は、ライブラリのフォントを試し表示するときの kura:// の `FontFace`（`font-src kura:`）。
- `pnpm tauri dev` では CSP が付かない（devUrl から読むため。実測）。CSP は `pnpm tauri build --debug --no-bundle` で確かめる。

### 4.2 WebKit での合否の基準（selftest。M11）

`kura-devtools selftest-library` で作ったライブラリを `KURA_LIBRARY` で開き、`VITE_KURA_SELFTEST=1` のフロント（`src/dev/selftest.ts`）が次を順に試して `bench_report('selftest', …)` でログに書く（§13）。`build`＝`pnpm tauri build --debug --no-bundle`（tauri://localhost から読み、CSP が付く）、`dev`＝`pnpm tauri dev`（http://localhost:1430、CSP なし）。合格の条件は、P1〜P8 が両方で通り、F1〜F4 が両方で止まり、予期しない CSP の違反（`securitypolicyviolation`）が 0 件で、main への応答のログすべてに ACAO があること。1 つでも外れたら種類の追加に進まない。

**通るべきもの**

| | 読み方 | 期待する結果 | 効く CSP の行・ヘッダ |
|---|---|---|---|
| P1 | `<img>`（crossorigin なし）で `/thumb/{hash}/s`（JPEG）と `/file/{hash}`（アルファ付き PNG・`<script>` 入りの SVG） | `load` し、`naturalWidth` > 0 | `img-src kura:`。SVG は `image/svg+xml` |
| P2 | `fetch`：`/file/{hash}` の全体と `Range: bytes=0-99`、`/peaks/{hash}` | 全体は 200 で長さと先頭のマジック（`RIFF`・`glTF`）が合う。Range は 206 で `Content-Range` を読める。`/peaks` は 200 なら先頭が `kurapeak`（解析していなければ 404） | `connect-src kura:`、ACAO・ACEH |
| P3 | `fetch`：ない中身の `/file/{hash}` と、経路の外 | 状態 404 を読める（TypeError にならない） | `connect-src kura:`、エラーにも ACAO |
| P4 | `new FontFace(name, 'url(kura://…/file/{ttf})')` を `load` して `document.fonts` に足す | `loaded` になり、代わりの書体と字の幅が変わる | `font-src kura:` |
| P5 | `fetch` した WAV を `decodeAudioData`、`<audio crossorigin="anonymous">` で `/play/{hash}` | 長さ（フレーム数）が合う。`loadedmetadata` で長さが合う | `connect-src kura:`・`media-src kura:`、ACAO |
| P6 | `<img crossOrigin="anonymous">` で PNG を読み、canvas の `getImageData` | 汚れずに読め、左上の画素が [255, 0, 0, 128] | `img-src kura:`、ACAO |
| P7 | CSS の `mask-image: url(kura://…/file/{png})`（フォントのサムネイルの色付けと同じ形） | 違反がなく、要求が `Origin` 付きで 200・ACAO がアプリの origin（ログの `p=P7`。CSS の画像の読み込みはページから見えず、WebKit は kura:// の Resource Timing を残さない） | `img-src kura:`、ACAO（mask の画像は CORS で読む） |
| P8 | 既存の invoke（`appearance_get`）と、結果の報告（`bench_report`） | 通る | `connect-src ipc: http://ipc.localhost` |

**止まるべきもの**

| | 読み方 | 期待する結果 | 効くもの |
|---|---|---|---|
| F1 | よその origin：`sandbox="allow-scripts"` の `srcdoc` の iframe（`Origin: null`）から kura:// を fetch | 読めない（枠が開かないか、403） | 移動の制限（§11。`about:srcdoc` を止める）、`admit` の Origin の判定 |
| F2 | ほかの WebView：アプリ内ブラウザ（`capture-browser`）で開いた http のページから kura:// の `<img>` と fetch | 403・CORS のヘッダなし（ログに `label=capture-browser`）。`<img>` は `error`、fetch は TypeError | `admit` のラベルの判定 |
| F3 | `GET`・`HEAD`・`OPTIONS` 以外：main から `POST`・`PUT`・`DELETE` の fetch | 405 を読める（中身を返さない）。`OPTIONS` は 204、`HEAD` は 200 で本体なし（`Content-Length` は読める） | `admit` のメソッドの判定 |
| F4 | main の外への移動：main を kura:// の SVG と https のページへ移す、iframe に kura:// の SVG | 移動は `on_navigation` が止め（ログ）、ページはそのまま。iframe は `frame-src` の違反（build）か移動の制限（dev）で開かず、SVG のスクリプトは動かない | `frame-src 'none'`、移動の制限（§11）、応答の `CSP: sandbox` |

**結果**（M11 の段 1。macOS 26.7・tauri 2.12.2・wry 0.57.0。`kura-devtools selftest-library` のライブラリ。両方とも合格）

| | build（tauri://localhost、CSP あり） | dev（http://localhost:1430、CSP なし） |
|---|---|---|
| P1 | 通る（サムネイル 64・PNG 4・SVG 32px） | 同じ |
| P2 | 通る（WAV 200・44,144 バイト・`RIFF`、glb 200・`glTF`、Range 206・`bytes 0-99/44144`、`/peaks` は段 3 まで 404） | 同じ |
| P3 | 通る（ない中身・経路の外とも 404 を読める） | 同じ |
| P4 | 通る（`loaded`、字の幅 244.34 → 240.04） | 同じ |
| P5 | 通る（`decodeAudioData` 22,050 フレーム、`<audio crossorigin>` 0.5 秒。Range で 206 が 11 回） | 同じ |
| P6 | 通る（[255, 0, 0, 128]） | 同じ |
| P7 | 通る（違反なし。ログで `Origin: tauri://localhost`・200・ACAO 一致） | 通る（`Origin: http://localhost:1430`・200・ACAO 一致） |
| P8 | 通る | 同じ |
| F1 | 止まる（`about:srcdoc` を `on_navigation` が止め、枠が開かない） | 同じ |
| F2 | 止まる（`<img>`・fetch とも 403・ACAO なし。ページでは `error`・TypeError） | 同じ |
| F3 | 止まる（POST・PUT・DELETE は 405、OPTIONS は 204、HEAD は 200・本体 0） | 同じ |
| F4 | 止まる（iframe は `frame-src` の違反。main の kura:// と https への移動は `on_navigation` が止め、ページはそのまま。SVG のスクリプトは一度も動かない） | 止まる（iframe も `on_navigation` が止める。ほかは同じ） |
| 予期しない違反 | 0 件（予期したものは F4 の `frame-src` の 1 件） | 0 件 |
| main への応答のログ | 33 件すべてに ACAO（`tauri://localhost`） | 33 件すべてに ACAO（`http://localhost:1430`） |

- WKWebView は iframe の読み込みにも移動の判定（`decidePolicyForNavigationAction`）を呼ぶので、`on_navigation` は枠にも効く（wry は main の枠かどうかを見ずに渡す）。
- 独自の scheme には事前確認（OPTIONS）を送らない（POST・PUT・DELETE もそのまま届いた。M11 の調べと同じ）。OPTIONS が来るのはページが自分で送ったときだけ。
- 通常のビルド（`VITE_KURA_SELFTEST` なし）の出力には selftest のコードが入らない（条件の中の動的 import が消える）。
- M11 の最後のコミットでも build（`VITE_KURA_SELFTEST=1 VITE_KURA_CHECK=1`）で確かめた：P1〜P8 が通り、F1・F3・F4 が止まり、予期しない違反 0 件、main への応答 34 件すべてに ACAO。F2 はビルドのときに `VITE_KURA_SELFTEST_F2` を渡さなかったので見送った（受け付けの判定は段 1 から変えていない）。

## 5. ジョブ

- `jobs` テーブルに記録し、アプリ再起動後も `queued` / `paused` を再開できるようにする（`running` で終わっていたものは `queued` に戻す）。
- アプリの終了・ライブラリの切り替えでは、実行中のジョブを区切りで止めて `queued` に戻す（次回の起動で自動で再開）。ユーザーが一時停止したものは `paused` のまま。
- 再開に必要な途中経過は、ジョブ自身が作業と同じトランザクションで `result_json` に保存する。
- ライブラリを開くときは、ライブラリのロック（`library.lock`。取れなければ何もせずに `LibraryInUse`。§3）→ staging の掃除 → 台帳の予告の照合（M12。`ledger::settle_pending`。保存・GC が途中で落ちて台帳にないファイルを片付ける。docs/data-model.md §4）→ 整合性チェック → ジョブの開始、の順にする（チェック中に取り込みが進むと食い違いを誤って数えるため）。閉じるときは、ジョブを止めて DB を閉じてからロックを放す（`LibrarySession::close`）。
- 種類ごとの同時実行数：import 1、thumbnail 1（ジョブの中で CPU コア数の並列。ADR-31）、proxy 1、download 2、capture 1、export 1、analyze 1、edit 1（M12 の保存）、gc 1（M12 の段 3）。
- 後続のジョブは `ensure_queued(kind, input)` で積む（同じ種類が `queued` ならもう積まない。そのライブラリで動かす種類（ハンドラ）がなければ積まない。analyze は一時停止中のものがあれば積まない：利用者の一時停止を次の取り込みが勝手に外さない）。サムネイルのジョブは、取り込みのバッチを書いたあとと、ライブラリを開いたとき（起動のスレッドで前回の途中の印を片付けて作り方の指紋を照らしたあと、`pending`・`stale`・疑わしいもの（settings の `thumbs.suspects`）が残っていれば。`media::prepare_on_open`。§7）に積む。
- **譲る**（M11）：種類ごとに譲る相手を網羅の match で決める（`JobKind::yields_to`。今は analyze が import と thumbnail（NFR-05・NFR-04 を守るため）と edit（M12 の保存。同じ一括のプールと予算を使い、利用者が待っている）に譲り、gc（M12 の段 3。急がない片付け）が import・thumbnail・edit・analyze・export・download・capture に譲る（照合の作り直しは一括のプールと予算を使い、消すときは台帳の門を書きの側で持つので、取り込み・ダウンロード・キャプチャを待たせない。proxy は再生の都合で動くので譲らない）。download はネットワーク待ちなので、edit は利用者が待つので譲らない）。譲る相手が動いている・待っている間はスケジューラが取り出さず、動いているジョブは `should_yield` で知って区切りで `JobEnd::Yield` を返す（`queued` に戻り、相手が終わると続きから動く。一時停止にはしない）。M14 の一括を足すときも、ここで解析が譲るかを決める。
- **解析のジョブ**（`kind = 'analyze'`、M11。`kura-core/src/analysis/`）：エンジンの部品のあるライブラリだけで動く。取り込み・ダウンロード・キャプチャのバッチを書いたあと、ライブラリを開いたとき（起動のスレッドで整合性チェックのあとに計画の指紋 `analysis.plan` を比べ、変わった種類と扱いの変わった形式（決定 15 の更新では、形式から書いていた HEIC・HEIF）を `pending` に戻したうえで、待っているものか、投影の版・ドット絵の規則が変わった印 `analysis.reproject` があれば）、`request_analysis`・`retry_analysis`、外部ツールの場所を変えたとき（デコーダの指紋を比べ直す）に積む。`pending` を新しい順に（優先の指定を先に）16 件ずつ取り出し、1 件ずつヘッダだけ読んで見積もり（値の 2 倍＋デコーダの作業領域）、min(予算/2, 1 GiB) を超えれば確保せずに失敗、収まれば一括の予算（Class::Bulk）をプールの外で確保して一括のプールで読んで解く（決定 2）。JPEG は読まずに形式から事実を書く（決定 5）。HEIC・HEIF は見出し（Platform の段の形 `CodecSet::shape_path`。ImageIO が開くだけで展開しない `CGImage` のアルファの扱い）を読み、アルファを持つものだけをほかの画像と同じく読んで解き、ほかと見出しを読めないものは形式から書く（決定 15。12MP の HEIC で見出しは 1 件 p50 0.62 ms・1,000 件のジョブ全体で 0.74 秒）。同じ中身の結果がそろい、それを読んだデコーダが今の組にあれば読まない（デコーダの識別子が変わって戻したものは読み直す）。16 件か 500 ms ごとに 1 つのトランザクションで書く（`WHERE id = ? AND hash = ? AND analysis_state = 'pending'`）。波形の段は `peaks/ab/<hash>.peaks` に一時ファイルから改名してコミットの前に置く。OS のデコーダ（Platform の段）を通りうる形式（コーデックの組にその形式の Platform のデコーダがあるもの。どの段が読むかによらない。アプリの組では HEIC・HEIF の見出しと AVIF・TIFF・BMP、固定の段が先に読む PSD・WAV・AIFF・Ogg・Opus・m4a・AAC）を読むときは、読む前に処理中の印（settings の `analysis.inflight`）をコミットし、書いたら消す。次に動いたときに印が残っていれば、そのアイテムで落ちたとみなして `failed`（`crashed`）にして先へ進む（同じファイルで落ち続けない。印は強制終了・電源断と見分けないので、そのときもその 1 件は、固定の段が読んでいても `crashed` になる。M11 の確かめ C1）。失敗は記録し、指紋が変わるか利用者が頼むまで解き直さない（docs/data-model.md §4「解析の読み書き」）。
- **保存のジョブ**（`kind = 'edit'`、M12 の段 2。`kura-core/src/edit/save.rs`。手順と意味は docs/editing.md §14、表は docs/data-model.md §4「保存と版（M12）」）：エンジンの部品のあるライブラリだけで動く（`LibrarySession` が登録する）。積む口は `edit::start_save`（上書き・別名で保存・版に戻す）。入力は小さく、文書は積む前に内容アドレスの部品で DB に置いて根のダイジェストだけを書く（`{"v":1,"itemId","base":{"hash","version"},"action":{"type":"overwrite"|"saveAs"|"revert",…}}`）。結果は `{"outcome":"saved"|"unchanged"|"existing",…}`（版に戻すは `revert` も）。同時に 1 本で、誰にも譲らない（利用者が待っている）。解析は保存に譲る（同じ一括のプールと予算）。描く・書く間は台帳の門を持たず、予告 → 配置 → コミット → 後始末だけ取り込みの鍵の書きの側（`Library::ingest_exclusive`）を持つ（取り込み・ダウンロード・キャプチャは読みの側。ゴミ箱を空にする処理・段 3 の GC も書きの側）。途中の確かめ（`decoding`）と結果（`committed`）は `result_json` に作業と同じトランザクションで書き、再開ではこのジョブの版（`item_versions.job_id`）があれば Done。強制終了は開く順の staging の掃除と予告の照合が片付け、ジョブは初めからやり直す（`kura-devtools edit crash-loop` で確かめる。§13）。下書き（`edit::drafts`）はジョブではなく、エディタが置く（docs/editing.md §12.12）。保存のジョブは、入力が指す文書の部品を置くのと同じトランザクションで積む（`JobManager::enqueue_with`）。
- **GC のジョブ**（`kind = 'gc'`、M12 の段 3。`kura-core/src/edit/gc.rs`。意味は docs/editing.md §14「GC のジョブ」、表と SQL は docs/data-model.md §4「保存と版（M12）」）：エンジンの部品のあるライブラリだけで動く（`LibrarySession` が登録する。照合で作り直すため）。置き換えから 7 日たち、かつ置き換えより前に作ったバックアップが残っていない消し待ちの版を、照合してから消し（合わなければ kept、照らせなければ次回へ）、参照されないアンカーと、根から辿れない文書の部品を片付ける。**積む時**：ライブラリを開いたあと（起動のスレッドで、整合性チェック・ジョブの開始・解析の計画の照合のあと）、保存のジョブのコミットのあと（保存の後続のジョブと一緒に）、バックアップの係の 1 時間ごとの確かめのあと（`backup::Scheduler` の `on_check`。バックアップが増える・古いものが消えると、消してよい版が変わる）。どれも候補（部分索引 `idx_item_versions_waiting` の 1 行か、文書の部品の印 settings の `gc.parts`）があるときだけ `ensure_queued` で積む（`gc::queue_if_due`）。**譲る相手**は上の「譲る」。候補ごとに一時停止・中止・譲るを見て、カーソルを候補ごとのトランザクションで `result_json` に書き、再開はカーソルの次から。消すときだけ台帳の門（`Library::ingest_exclusive`）を持ち、'delete' の予告 → 台帳から外すコミット → ファイルを消す → 予告を消す（落ちても開く順の予告の照合が片付ける）。照合で OS のデコーダは呼ばない（アンカーが使えないときは次回へ）。時計とバックアップの一覧は `GcClock` で差し替えられる（アプリは本物。`kura-devtools edit crash-loop` の子だけが 8 日進めた時計を `Services::with_gc_clock` で渡す）。ゴミ箱を空にする処理（`Library::empty_trash`）も同じ部品（'delete' の予告・`ledger::remove_files`・`gc::sweep_parts`）で消す。
- 各ジョブはキャンセルトークンを持つ。外部プロセスはキャンセル時に SIGTERM、5 秒後に SIGKILL（編集のコーデックの ffmpeg（`tools::run_binary`）は 2 秒後に 1 回。docs/editing.md §9.8）。
- **子の起動の鍵**（M11 の 3 回目の確認 W1。ADR-164 (6)）：kura の本番のコード（kura-core・kura-mac・src-tauri）は、外部ツール・Chrome・mpv・SVG の働き手などの子をすべて `tools::spawn`・`tools::status`（プロセスで 1 つの鍵の中で `Command::spawn`。待つのは鍵の外）で起動する（`tools::run_with_timeout`・`run_streaming`・`run_binary` もこれを通る）。macOS の std はパイプを `pipe()` で作ってから別の呼び出しで FD_CLOEXEC を付けるので、鍵がないと、その間に別のスレッドが起動した子がパイプの端を受け継ぎ、パイプの相手の EOF が来なくなる。`tools` のテストが src を見張り（鍵の外の `.spawn()`・`.status()`・`.output()` を断る）、`kura-core/tests/spawn_lock.rs` が子の開いているファイル記述子で漏れがないことを確かめる。
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
     d. メディア情報（MediaBackend::probe）：幅・高さ・長さ・撮影日時・コーデック、音楽はアーティスト・アルバム・曲名、
        フォントは書体名など。OS のデコーダで読むので、バッチの前に処理中の印を付けてから a〜d を続けて並列に行う
        （§7 の「取り込み・ダウンロード・キャプチャの probe の印」。media::inflight::ProbeBatch。M11 の検証 S2）
  3. バッチごとに 1 トランザクションで：
     - 既存（同じハッシュ）なら、オプションのフォルダ・タグ・ユニットへの所属だけ追加して「重複」と数える
       （「何もしない」を選んだら所属も足さない。ゴミ箱にあれば戻す）
     - 新規なら items と所属を挿入
     - 再開位置と途中の件数を jobs.result_json に保存
  4. 新しいアイテムがあればサムネイルのジョブを積む（`ensure_queued`。プロキシの判定は M5）
  5. 結果（追加数・重複数・失敗数と明細）を result_json に
```

- 対応拡張子：画像 `jpg jpeg png apng gif webp aseprite ase psd psb svg heic heif avif tif tiff bmp`、動画 `mp4 m4v mov webm mkv avi`、音楽 `mp3 m4a aac flac wav aif aiff ogg opus`、フォント `ttf otf ttc woff woff2`、3D `glb`（M11）。それ以外はスキップして件数を報告する。
  - 拡張子・種類・MIME は kura-engine の形式の表（`codec::formats::FORMATS`）が正で、`import::kind` の `classify`・`mime_for`・`format_id` と kura:// の Content-Type はそこから引く（行を足すと取り込める形式が増える）。取り込み・ダウンロード・キャプチャは同じ `classify_file` を通す。
  - `.ase`・`.aseprite` は先頭のバイト（4 バイト目からのマジック 0xA5E0）で Aseprite かを見分け、違えば取り込まない（`.ase` は Adobe の色見本と拡張子が重なる）。スキップの件数に含め、理由を result_json の `rejected` とジョブのログに出す。
  - 種類の文字列（`items.kind`）は `MediaKind`（`FromSql`・`parse`）で読み、知らない値を既定の種類に落とさない。種類の表はフロントにも bindings の定数 `ITEM_KINDS`（順・キューに入るか・プロキシ・長さ）で出す。
- オプション：取り込み先フォルダ、フォルダ構成の再現、フォルダ名をタグに、ファイル情報からタグ（`アーティスト:`・`アルバム:`。撮影日・撮影地は付けない。ADR-30）、全件に付けるタグ、フォルダごとにユニット作成。
  - フォルダ構成の再現：ドロップしたフォルダを取り込み先に対応させ、その下にサブフォルダを作る（旅行2026/鎌倉 → 写真/2026 旅行/鎌倉）。取り込み先がなければ、ドロップしたフォルダ名でいちばん上に作る（ADR-24）。
  - フォルダ名をタグに：ドロップしたフォルダからファイルの親までの各フォルダ名（ADR-24）。
  - フォルダごとにユニット：末端フォルダ（下にフォルダを持たないもの）ごとに set ユニット（origin = import、並びはファイル名の自然順）。
  - ファイル名・フォルダ名・タグは NFC に正規化する（ADR-25）。
- 起動時の整合性チェック：`staging/` の残骸を削除。DB にあってファイルがないもの、ファイルがあって DB にないものを数えて設定画面に出す（自動では消さない）。設定の「いま確かめる」（`library_check`）も同じ `library::check_integrity` を通り、加えて items の台帳の行の ID の写し（`blob_id`）の食い違いを直し（M12）、GC の照合が合わずに残した版（`keptVersions`）と 7 日を過ぎた消し待ちの版（`overdueVersions`）を数える（M12 の段 3。問題には数えない。起動時は null）。
  - 取り込み中に落ちた場合、配置済みで DB に入る前のファイルは「DB にないファイル」として残るが、再開した取り込みがそのまま使う（内容アドレスなので置き直しは不要）。
- 動作確認：`cargo run -p kura-devtools -- import --library <path> [--create] <src>...` でコマンドラインから取り込める（強制終了の試験用。`check --library <path>` で件数と食い違いを表示）。

## 7. サムネイルとメディア情報（kura-mac・kura-engine）

- Swift ヘルパー（`crates/kura-mac/swift`、Swift 5 の言語モード）が C 互換関数を公開し、Rust から `extern "C"` で呼ぶ（swift-rs はビルド時のリンクにだけ使う）。書き先はどれも拡張子のない stem（`thumbs/ab/<hash>_l`・`_s`。kura-core の `media::ThumbTargets`）で、書いた形式の拡張子を足す（M11）。戻り値は 0 成功（サムネイルは JPEG を書いた）・1 扱えない形式・2 失敗・3 ジャケットなし・4 PNG を書いた：
  - `kura_image_thumbnails(path, out_l_stem, out_s_stem, 1600, 512, keep_alpha)`：ImageIO の `CGImageSourceCreateThumbnailAtIndex`（HEIC・RAW・AVIF も扱える。向きを反映）。`keep_alpha` が 0 でなく、元がアルファを持ち（`kCGImagePropertyHasAlpha` かアルファの列）、sRGB の premultipliedLast に描き直した長辺 1600 に透明な画素（α < 255）が 1 つでもあれば PNG を書いて 4 を返す（M11 の決定 3・4）。そうでなければ JPEG（品質 0.82）で 0。`MediaBackend::image_thumbnails`（`MacBackend`）はいつも `keep_alpha = 1`（`kura_mac::image_thumbnails_keep_alpha` と同じ）
  - `kura_font_thumbnails(path, samples, out_l_stem, out_s_stem, 1600, 512)`（M11）：CoreText。先頭のバイトで ttf・otf・ttc・woff・woff2 だけを読む。見本の字は `samples`（改行で区切った候補「永あAa」「Aa」）のうち書体がすべての字を持つもの（なければ字の番号 1 から形のある字を 4 つ）。正方形の透明の地に、形の範囲が幅の 80%・高さの 70% に収まる大きさで真ん中に、字の形だけのアルファ（グレーとアルファ、字は黒）で描いた PNG を書いて 4 を返す。色は表示の側が CSS の mask-image で付ける（`MediaBackend::font_thumbnails`・`kura_mac::font_thumbnails`）
  - `kura_video_thumbnails(path, at_sec, out_l_stem, out_s_stem, 1600, 512)`：AVFoundation の `AVAssetImageGenerator`（長さの 10% 地点。長さが分からなければ 1 秒）。いつも JPEG
  - `kura_audio_artwork(path, out_art, out_l_stem, out_s_stem, 1600, 512)`：AVAsset のメタデータからジャケット（元の画像を `art/`、縮小を `thumbs/` に。いつも JPEG）
  - `kura_probe(path, kind)` → JSON（`kura_free_string` で解放）：幅・高さ・長さ・撮影日時・コーデック・アーティスト・アルバム・曲名。`kind` は 0 画像・1 動画・2 音楽・3 フォント（M11。`{"codec": 形式, "font": {"family","style","fullName","postscriptName","glyphs","faces","variable"}}`。`MacBackend::probe(Font)`・`kura_mac::probe_font`）。知らない種類は NULL
  - `kura_reduce_transparency()` → bool
- `MediaBackend`（kura-core の trait。M11 の形）：`probe`・`image_thumbnails`（書いた形式 `ThumbFormat` を返す）・`video_thumbnails`（JPEG）・`audio_artwork`（`Option<ThumbFormat>`。ジャケットがなければ None）・`font_thumbnails`（PNG）。新しいメソッドには既定の実装を置かない（`ChainBackend`・`RoutedBackend`・`NoBackend`・`FfmpegBackend`・`MacBackend`・テストの偽物のすべてに書く。転送の書き忘れをコンパイルで見つける）。ffmpeg の道はいつも JPEG で、フォントは扱えない。
- メディア情報（`media::MediaInfo`）はフォントの `font`（書体名・スタイル・字の数・書体の数・可変か）と 3D の `model`（頂点・三角形・メッシュ・材質・テクスチャ・アニメ・スキン・ノードの数、大きさ（範囲の max − min）、作った道具、必須の拡張、描けない理由 `unsupported`）を持ち、`items.codec_info` の JSON の `font`・`model` に入れる（M11 の既定。フォント・3D の幅・高さは空）。フォントは取り込みの probe（CoreText）で、3D はサムネイルのジョブが kura-engine の glb の読み手で、見出しと JSON の塊だけを読んで数える（BIN は読まない）。
- 3D モデル（glb）の情報とサムネイルは kura-mac ではなく kura-engine の `model`（自前の読み手とソフトウェアの描画。SceneKit を使わない。M11 の決定 9。docs/editing.md §9.10）。描けないもの（必須の Draco・meshopt・知らない拡張・スキン）は情報だけ入れて `failed`（種類の絵）。
- SVG の文字を描くフォント（M11 の決定 12）：`kura_mac::pinned_options()` が、`/System/Library/Fonts` と `Supplemental` の ttf・otf・ttc を読み取り専用のボリュームなら mmap してエンジンの `SvgFonts` に渡す（文字を含む SVG を初めて読むときに作る。プロセスで 1 つを共通に使う。370 ファイル・787 書体で物理フットプリント +2.6 MiB）。`codec_set`・`full_codec_set` にはこれを渡す。unsafe は `src/fonts.rs` の写像の中だけ。
- **SVG を別のプロセスで読む**（M11 の決定 23。ADR-164。正本は docs/editing.md §9.5）：usvg・resvg には見積もりをすり抜けて数十秒〜数時間止まる形があり、どちらも中止を見ないので、アプリのコーデックの組（`MacEngineFactory::codecs`・kura-devtools の `app_codecs`）は固定の段の SVG のデコーダを、同じ識別子のまま kura-core の `codec_ext::svg_worker::SvgWorkerDecoder` に差し替える。形も値も、呼ぶたびにアプリ自身を `--kura-svg-worker` 付きで起動した子に頼む（環境変数を消す・新しいプロセスグループ。`tools::run_binary_kill`。起動は `tools::spawn` の鍵の中）。実行ファイルは起動のたびに kura-mac の `current_executable`（`proc_pidpath`。動いているアプリを移す・改名しても今の場所を返す。`std::env::current_exe` は起動したときのパスのまま）で求める（`WorkerCommand::own_exe`）。頼みと返事の先頭に親と子のビルドの印（`svg_worker::build_stamp`：SVG のデコーダの識別子・kura-core と kura-engine の版・ビルドの種類・CPU・フォントの有無と総称の書体）を入れ、子も親も照らす。子は `main` の最初で Tauri もログも始めずに `svg_worker::serve(&kura_mac::pinned_options(), ALLOC.meter())` を呼び、標準入力の頼みを 1 つ処理して終わる。時間の上限は形 2 秒・値 10 秒（超えたら子をグループごと SIGKILL して `Limit`）、ジョブの中止（ライブラリを閉じるとき）でも子を止める。子の起動は 1 回 約 5 ms。サムネイルは時間切れの SVG を ImageIO へ回さず `failed`（種類の絵）にし、解析は `too_large` にして先へ進む。起動できない・実行ファイルが分からない・別のビルドは、アイテムのせいではない一時的な誤り（`CodecMissing`。`svg_worker::is_unavailable`）で、サムネイル・解析のジョブはその SVG を失敗にせず（`pending`・`stale` のまま）、その回のほかの結果を書いてから、ジョブを失敗（`ToolFailed`。説明は理由と「アプリを動かしたまま移動・置き換えたときは開き直す」案内）にして止める。同じアイテムを拾い続けず、次にジョブが積まれたとき（ライブラリを開く・取り込む・解析を頼むなど）にやり直す。子の数は同時の呼び出しの数まで（サムネイルは一括のプールの並列）。子のメモリは予算の外で、入力の 16 MiB とエンジンの見積もりに加え、子の確保の総量の上限で抑える（ADR-164 (4)）：アプリの確保器 `kura_mac::ReturningAllocator` が目印（`svg_worker::AllocMeter`）を持ち、`main` が `serve(&kura_mac::pinned_options(), ALLOC.meter())` で渡す。働き手だけが数え、上限（エンジンの見積もりの口 `svg::PlanHook` で、木を作る前と描く前に見積もりから広げる）を超える確保を断る。断られた子は落ちるか読めないエラーを返し、親は `Decode`（「SVG を読むのにメモリを使いすぎます」）にする。
- 編集の Platform の段のコーデック（段 9。ADR-153）は、上の C 文字列の関数とは別の口（`swift/Sources/KuraMacC/include/kura_mac_ffi.h`、ABI の版 1）で呼ぶ：不透明なハンドルと 2 段の呼び出し（Swift が開いて形を返す → Rust が上限を確かめて確保 → Swift がそこへ書く）、状態の番号 0〜6（OK・扱えない・失敗・中止・誤った呼び出し・食い違い・上限。kura-engine の `codec::platform` と同じ値）、Rust が渡す文言のバッファ、中止のコールバック。Platform のコーデックの unsafe は `src/bridge.rs` だけ。詳しくは docs/editing.md §9.8。
- Swift のランタイムは OS のもの（`/usr/lib/swift`）を使う。リンクする実行ファイル（kura-mac のテスト、src-tauri、kura-devtools）の build.rs で rpath を足す。
- 形式ごとの試す順（`kura_mac::media_backend()`）：flac・ogg・opus・mkv・webm・avi は ffmpeg / ffprobe を先に、それ以外は macOS を先に。前の実装が「扱えない形式」と答えたときだけ次を試す（ADR-32）。
- ffmpeg / ffprobe は `/opt/homebrew/bin` → `/usr/local/bin` → PATH の順で探す（`kura_core::tools`。設定での上書きは M8）。外部プロセスは時間制限付きで動かす。
- サムネイルのジョブ（`media::ThumbnailHandler`）：`thumb_state = 'pending'` を新しい順に 64 件ずつ取り出し（`idx_items_thumb_pending`）、なくなったら `'stale'`（作り方が変わったので、今の絵を出したまま作り直すもの。`idx_items_thumb_stale`）を取り出す。M1 で取り込んだものは、ここで probe して幅・高さなどの空欄（と 0。ImageIO は SVG を 0×0 と読む）を埋める。ジャケットのない音楽は `failed`（グリッドでは音楽の絵を出す。ADR-34）。
  - **道の割り振り**（M11 の決定 3。純粋な関数 `media::thumb_route`）：

    | 形式 | 道 |
    |---|---|
    | png（apng）・gif・webp・aseprite（ase）・psd（psb）・svg | エンジン（`media::engine_thumbs`。kura-engine のコーデックの組で読む） |
    | jpeg・heic・heif・avif・tiff・bmp（形式の表にない画像も） | ImageIO（アルファを保つ。`MediaBackend::image_thumbnails`） |
    | ttf・otf・ttc・woff・woff2 | CoreText（`MediaBackend::font_thumbnails`） |
    | glb | kura-engine の `model`（ソフトウェアの描画。`media::engine_thumbs`） |
    | 動画・音楽 | 今までの道（AVFoundation・ffmpeg） |

  - **エンジンの道**：ジョブのスレッド（プールの外）で 1 件ずつ形を読んで（`shape_path_monitored`。別のプロセスで読む SVG の子はジョブの中止で止める。glb は見出しと JSON の塊）見積もり、同時の確保が 256 MiB（M11 の決定 2。`INFLIGHT_CAP`。予算の全体のほうが小さければそちら）に収まる小分けにして（順は保つ。1 件で 256 MiB を超えるものは 1 件だけの小分けにし、同時の確保はその 1 件の分になる。予算の全体を超えるものは確保で断られて ImageIO の道へ回る）、一括の予算（`Class::Bulk`）から確保してから、実行の場（`EngineServices`）の一括のプール（`kura-edit-bulk-*`）の中でアイテムを並べて描く。サムネイルのプール（`kura-thumb-*`）の中ではエンジンを呼ばない。先頭のフレームをキャンバスに置き（offset の外は透明）、Box で長辺 1600・512 に縮める（拡大しない。ドット絵の拡大は表示の側）。長辺 1600 に透明な画素があれば PNG（`compression: fast`）、なければ JPEG（品質 82。Rgb8 に落として書く）。色の ICC は引き継ぐ（グレーのものは捨てる）。値が 1 GiB を超えるもの・`Limit`・`Unsupported`・読めないもの・予算に収まらないものは ImageIO の道へ回し、理由をジョブのログに残す（aseprite・svg は ImageIO も読めないので `failed`）。ただし SVG の時間切れ（「SVG を読むのに時間がかかりすぎます」）は回さずに `failed`。glb は描けなければ `failed`。見積もりの小分けは `media::engine_thumbs::{image_estimate, model_estimate}`。エンジンの部品はライブラリを開いただけでは作らず、エンジンの道のアイテムに最初に出会ったときに作る。
  - **書き方**：OS の道はサムネイルのプールで CPU コア数の並列。結果は 1 トランザクションで書き、書いたら `thumb_rev` を +1（URL の `?r=`）。UPDATE は `WHERE id = ? AND hash = ?`（読んだあとに中身が変わっていたら書かない）。書いたら別の形式の古いファイルを消す（`ThumbTargets::remove_others`）。`stale` を作り直せなかったときは、前の絵のまま `ready` に戻す（`thumb_rev` は変えない）。`kura://…/thumb/{hash}/{s,l}` は png、jpg の順に探す（§4）。
  - **作り方の指紋**（settings の `'thumbs.plan'`。`media::route::sync_plan`）：形式の表の行ごとの作り方の名前（`engine-thumbs/1`・`imageio-alpha/1`・`imageio/1`（jpeg）・`coretext/1`・`model-kura-render/1`・`video/1`・`audio/1`）。ライブラリを開くとき（ジョブを動かす前に）前回と比べ、変わった形式の `ready` を `stale` に、`failed` を `pending` に戻す。指紋のないライブラリ（M10 まで）は「画像はすべて ImageIO の JPEG、フォント・3D は作らない」と比べる（png・gif・webp・aseprite・psd・svg・heic・heif・avif・tiff・bmp とフォント・3D が作り直しになり、jpeg・動画・音楽は変わらない）。指紋が変わったときだけの全件の走査（10 万件の seed で png・heic の 46,860 件を stale にして 70ms）。
  - **作っている途中の印**（critic 8。`media::inflight`）：OS のデコーダ（ImageIO・CoreText・AVFoundation）とエンジンの Platform の段はアプリのプロセスの中で動くので、壊れたファイルでプロセスごと落ちうる。作る前に作っているアイテムを settings の `'thumbs.inflight'`（`{solo, ids}`）に書き、結果と同じトランザクションで消す。開くときに残っていれば、まとめての印（64 件）は疑わしいもの（`'thumbs.suspects'`）にして 1 件ずつ・1 件だけの印で作り直し、1 件だけの印のアイテムは `failed`（`stale` は前の絵のまま `ready`）にして先へ進む（同じファイルで落ちるのは多くても 2 回）。開く道は `media::prepare_on_open`（印の片付け → 指紋 → pending・stale・疑わしいものがあればジョブを積む）。
  - **取り込み・ダウンロード・キャプチャの probe の印**（M11 の検証 S2。`media::inflight::ProbeBatch`）：取り込みの probe（CoreText のフォント・ImageIO・AVFoundation）も同じプロセスで読むので、印がないと、落ちたジョブが次の起動で同じ範囲をやり直して同じファイルで落ち続けた（CoreText の読み込みを abort に差し替えると、開くたびに落ちた）。ファイルはまだアイテムでないので取り込み元のパス（取り込みの再開・ダウンロードのやり直しで同じになる）で覚え、バッチ（500 件）の前に、まとめて読むファイルのパスをジョブごとの印として settings の `'probe.inflight'`（`[{job, solo, paths}]`）に書いてコミットし、コピー・ハッシュ・配置・読み取りを続けて並列に行い、結果を書くトランザクションで消す（`settle_probes`）。開くときに残っていれば（サムネイルの印と同じ `inflight::recover`）、まとめての印のファイルは `'probe.suspects'` にして次に 1 件ずつ・1 件だけの印で読み、1 件だけの印のファイルは `'probe.crashed'` にしてもう読まない（情報なしで入れ、サムネイルは `failed`。作り終えた絵は変えない）。同じ差し替えで、落ちるのは 2 回で止まり、3 回目からは開ける。同じファイルで落ちるのは多くても 2 回。印の書き込みはバッチごとに 1 回で、取り込みの速さは変わらない（4,000 件の JPEG で直す前 7,030〜7,270 件/秒・直したあと 7,050〜7,220 件/秒）。
  - **キャッシュの容量**（M11 の決定 4 で ADR-122 を広げる）：`library_cache_usage`（§3）がサムネイル（`thumbs/` と `art/`）・波形（`peaks/`）・アニメ（`anim/`）のファイルを別々に数える（`media::usage`。1 段目のフォルダを最大 8 本のスレッドで。10 万件の seed のサムネイル 21 万個で 251〜328ms（温まった状態））。
- 目標：画像サムネイル 50 枚/秒以上（NFR-04）。実測は §13。M11（`bench thumbs-job`）：PNG の多い組（`assets --bench 24`：2048² のアルファ付き PNG・3000×2000 の不透明な PNG・JPEG を 24 枚ずつ、64² のスプライト 96 枚の 168 件）で 105〜112 枚/秒。大きな透過 PNG だけ（48 件）では 31 枚/秒（同時の確保 512 MiB なら 46 枚/秒）で、エンジンを使わない（M10 の ImageIO の JPEG）と 194 枚/秒。大きな透過 PNG の多いライブラリでは NFR-04 を下回る（M11 の決定 2。判定の範囲は requirements.md の NFR-04）。
- **大きな確保を OS へすぐ返す確保器**（M11。ADR-163。`kura_mac::ReturningAllocator`）：アプリ（src-tauri）と kura-devtools の `#[global_allocator]`。1 MiB 以上の確保だけを `mmap` で取り、解放したら `munmap` ですぐ返す。それより小さいもの（エンジンの 256² のタイル・音声のブロックなど）と、ページより大きな揃えは今までどおり `System`（libmalloc）。どちらの道かは `Layout` の大きさと揃えだけで決まる（解放・取り直しでも同じ `Layout` が渡るので目印を持たない）。`mmap` の領域は 0 で埋まっているので、0 で埋めた確保は書かない。macOS 以外は `System` のまま。確保の量を数える目印（`svg_worker::AllocMeter`）も持ち、SVG の働き手のときだけ数えて上限を付ける（ADR-164 (4)。アプリの本体は確保ごとに旗を 1 度読むだけ）。
  - 理由：解析・エンジンのサムネイルは数 MB〜数百 MB を確保しては捨てる。libmalloc は解放した大きな領域（large cache）を使い回し用に抱えたままにし、ジョブのあとも 150 秒たってアプリ 588 MB（うち空の MALLOC_LARGE 445 MB）・合計 712 MB のまま、NFR-07 の 500 MB に戻らなかった。`malloc_zone_pressure_relief` でも 48 KB しか返らない。large cache を切る設定（環境変数 `MallocLargeCache`）は文書にないので使わない。確保器を入れたあとは、ジョブのあとアプリ 124 MB・合計 253 MB に戻り、解析の速さは −1.6%（誤差の範囲。§13 の M11 の実測）。
  - `#[global_allocator]` を置くのはアプリと kura-devtools の `main.rs` だけなので、kura-core・kura-engine・kura-mac などのテストは libmalloc のまま。メモリを測るもの（`bench analyze`・`bench thumbs-job`、kura-devtools のテストの `mem_smoke` の子）は kura-devtools の中なので、アプリと同じ確保器で測る。

## 8. 再生とプロキシ

- 再生は WKWebView の `<video>` / `<audio>` / `<img>` で行う（デザインを崩さないため）。
- 起動時に `canPlayType` で再生可能な形式の表を作って Rust に渡す。取り込み時に ffprobe の結果と照合し、`proxy_state` を決める。
- 再生できない形式はプロキシを作る：
  - コーデックが対応済み（例：mkv に入った H.264 / AAC）→ `ffmpeg -c copy` で mp4 に入れ替え（数秒）
  - それ以外 → VideoToolbox でハードウェアエンコード（`-c:v h264_videotoolbox` など）、音声は AAC 256kbps
- プロキシ作成中・失敗時は「外部プレイヤー（mpv または QuickTime Player）で開く」を出す。

### 8.1 実装（M5）

- **再生可否の表**：フロント（`src/features/player/support.ts`）が「入れ物（mp4・webm・ogg・mp3・flac・wav・aiff・aac）× ffprobe のコーデック名」ごとに `canPlayType` を調べ、空でなければ再生できるとする。表はアプリのメモリに持ち、ライブラリを開くたびに渡す。表の指紋を `settings` の `playback.support` に記録し、変わっていれば `not_needed` / `pending` / `failed` の動画・音楽を `unknown` に戻す（ADR-61）。
- **判定**（`kura_core::playback::support::plan`、純粋関数）：入れ物は拡張子で決める（mov・m4v・m4a は mp4、opus は ogg、mkv・avi などは「その他」）。元の入れ物で映像・音声とも再生できれば `not_needed`。そうでなければ `pending` で、プロキシ（mp4）では mp4 で再生できるストリームはコピー、できないストリームだけ変換する（ADR-63）。映像も音声もなければ `failed`。動画・音楽（`video`・`audio`）以外の種類（画像・フォント・3D）はプロキシを作らない（取り込み時に `not_needed`、ADR-65。M11 で種類を足したので「画像は」から広げた。プロキシの SQL は `+kind IN ('video','audio')`、表の制約 `items_proxy_av`）。
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
- **再ダウンロード**（ADR-132）：`download_detect` が `download::history::find_previous` でダウンロードのジョブの履歴から同じものを指す URL を探す（`target_key`：YouTube は動画・再生リストの ID、ほかはホスト（`www.`・`m.` を除く）・パス・追跡用を除いたクエリ。twitter.com は x.com）。ライブラリに入れたことのあるジョブのいちばん新しいもの、なければ取得済みとして飛ばしたジョブ。見つかれば画面が「スキップして追加」「再ダウンロード」を聞く。再ダウンロードのジョブ（入力の `redownload`）は、`--download-archive` に作業フォルダの中の空の記録（`.kura-archive.txt`・`.kura-archive.sqlite3`）を渡し、ツールが終わったら（失敗でも）その中身をふだんの記録に足す（`archive::merge`。重複は足さない）。一時停止から再開したときは、その記録で取り終えたものを飛ばす。

## 10. キャプチャ

- **ルールで自動キャプチャ**：インストール済みの Google Chrome をアプリ専用のプロファイル（`~/Library/Application Support/<bundle id>/capture-profile`）でヘッドレス起動し、DevTools（CDP）で操作する。CDP のクライアントは tungstenite で作った小さなもの（ADR-104。当初は chromiumoxide）。
  1. ルールの `viewport_width` で開く → `wait_ms` 待つ（または対象セレクタの出現）
  2. `hide_selectors` を非表示にする
  3. `scroll_times` 回スクロールして遅延読み込みを発火させ、先頭に戻す
  4. `element`：対象要素の範囲を clip して撮影。`full_page`：`captureBeyondViewport` でページ全体。`viewport`：表示範囲
  5. PNG を staging に保存 → 取り込み（タグ `キャプチャ`、`サイト:<ホスト>`、source_url）
- **ログイン**：「ログイン用に開く」で同じプロファイルの Chrome を画面ありで起動し、ユーザーにログインしてもらう。開いているページはそのまま撮れる（ADR-111）。
- **見たままキャプチャ**：アプリ内の別ウィンドウ（WebviewWindow）でページを開き、ボタンで WKWebView の `takeSnapshot` を呼んで表示範囲を保存する（v1 は表示範囲のみ）。

### 10.1 実装（M8）

- **ルール**（`kura_core::capture::rules`）：`capture_rules` の CRUD と並べ替え。URL パターンの `*` は `/` を含む任意の文字列（大文字小文字は区別しない）。URL の欄ではキャプチャのルールをダウンロードより先に見る（ADR-105）。
- **ジョブ**（`kind = 'capture'`、同時に 1 つ）：Chrome を `--headless --remote-debugging-port=0` で起動 → プロファイルの `DevToolsActivePort` の 127.0.0.1 に接続 → 新しいページを作って手順 1〜4（`capture::shoot`。ADR-108）→ Chrome をプロセスグループごと止める → PNG を取り込みと同じ方法で配置して登録（`item_tags.source = 'capture'`、`source_site = 'capture'`）。ルールの `last_run_at` / `last_status`（`ok` / `failed`）も記録する。ログイン用の Chrome がプロファイルを使っていれば撮らない。
- **ログイン用の Chrome**（`capture::login`）：DevTools を 127.0.0.1 の空いているポートで開いて起動し、アプリがつないだまま持つ（`AppState`）。前面のタブを `shoot_live`（読み込み・画面の大きさを変えず、隠す要素は撮るあいだだけ）で撮り、その場で登録する。アプリを終えるとき・「閉じる」で閉じる（ADR-111）。
- **⌃⌥⌘C**（`src-tauri/src/shortcut.rs`）：`tauri-plugin-global-shortcut` で、ログイン用の Chrome を開いたときに登録し、閉じたとき（「閉じる」・Chrome が閉じられたのを `capture_login_status` で見つけたとき）に外す。押されたら別のスレッドで、前面のアプリのプロセス（kura-mac の `frontmost_pid`。NSWorkspace）がログイン用の Chrome か確かめ、`capture_login_snapshot` と同じ処理で撮る。シャッター音・エラーの音（kura-mac の `play_capture_sound`。NSSound）を鳴らし、`capture://login-shot` で画面に知らせる（ADR-112）。
- **見たままキャプチャ**：`browser_open` でラベル `capture-browser` のウィンドウを開く（アプリの権限なし、http / https だけ）。`browser_snapshot` で Swift の `kura_snapshot_webview` が PNG を書き、その場で登録する（ADR-109）。
- **セキュリティ**：ADR-107。
- **確認**：`cargo test -p kura-core capture -- --ignored` で、127.0.0.1 の小さなページを本物の Chrome で撮る（ヘッドレスの要素のみ・ページ全体・表示範囲と、ログイン用の Chrome の開いているタブ）。

## 11. ウィンドウと macOS 固有の設定

- `tauri.conf.json` のウィンドウ：`titleBarStyle: "Overlay"`、`hiddenTitle: true`、`trafficLightPosition: { "x": 25, "y": 35 }`（サイドバーの中に信号機を置く）、`minWidth: 960`、`minHeight: 600`。
  - 信号機の位置は macOS 26 で実測して決めた。ネイティブの信号機は直径 14px・間隔 23px で、モックの絵（12px・20px）と寸法が違うため、1 つ目の左端（x=25）と縦の中心（y=33、サイドバー先頭の 24px 行の中央）をモックに合わせている。`y` と実際の上端の関係は「上端 = y − 9」。
- **main の移動の制限**（M11）：main を kura:// の SVG などへ移すと、そのページのスクリプトから invoke が通ってしまう（M11 の調べで実測）。そこで main の窓は `tauri.conf.json` では `"create": false` にし、setup の最後（`AppState` を manage したあと）で `navigation::create_main_window` が `WebviewWindowBuilder::from_config`（Tauri が設定の窓を作るときと同じ口なので、上の属性はそのまま）に `on_navigation` を付けて作る。許すのはアプリの origin の中だけ（`tauri build` は `tauri://localhost`、`tauri dev` は devUrl の origin。判定は kura-core の `AppOrigin::allows_navigation`）で、kura:// ・ほかの http(s)・`about:`・`data:`・`blob:`・`file:` は止めてログ（warn）に残す。WKWebView は iframe の読み込みにも同じ判定を呼ぶので、枠も開かない（`frame-src 'none'` と重ねる）。アプリ内ブラウザ（`capture-browser`）は別の窓で、http(s) だけを許す（§10.1）。
- ガラス表現は CSS の `backdrop-filter` で行う（`docs/decisions.md` ADR-12）。ウィンドウの透明化・private API は使わない。ネイティブの `NSGlassEffectView` は M9 で試作して見送った（ADR-124）。
- 「透明度を下げる」がオンなら `<html data-reduce-transparency="true">` を付け、ガラスを不透明な面に置き換える。
- ファイルのドラッグ＆ドロップは `getCurrentWebview().onDragDropEvent` で受ける（`enter` で重ね表示、`drop` で取り込みシート）。
- フルスクリーン（M9 のあと。ADR-128）：シアターのボタン・F・緑のボタン・メニューのどれでも、ウィンドウの大きさの変化で状態を読み（`watchFullscreen`）、シアターの右のパネルを閉じる・戻す。出入りが終わったら kura-mac の `keep_webview_focus_on_fullscreen` が WKWebView にキーボードのフォーカスを戻す（そうしないと esc などが届かない）。
- `backgroundThrottling: "disabled"`（M5）：既定では、ウィンドウがほかのウィンドウに隠れると WebKit が動画の再生開始や CSS アニメーションを止め、再生キューが動画のところで止まる。メディアプレーヤーなので隠れていても再生を続ける（ADR-72）。
- メニューバーはフロントで組む（`src/app/appMenu.ts`）。文言は ja.ts。「すべてを選択」（⌘A）だけは自前の項目で、入力欄なら文字、それ以外は一覧のすべてを選ぶ（ADR-76）。Kura の操作の項目は、押すとそのキーのイベントをフォーカスのある要素から送る（`replayKey`）ので、キーの処理と同じ条件で動く。表示形式・まとめて表示のチェックとサイドバーの項目の文字は、表示の設定に合わせて書き換える（ADR-116）。「項目 › ドット絵として表示 ▸」（M11）はキーを送らず、一覧で選んでいる画像に直接当てる（`pixelArtMenu.ts` の `applyPixelArtToSelection`。右クリックの ▸ と同じ当て方）。▸ の無効とチェックは、選択・表示・一覧のキャッシュの変化を 100ms にまとめて合わせ直す（`pixelArtSyncer`。ADR-160 (3)）。アプリの言語は `src-tauri/Info.plist` で日本語と宣言し、OS が出す文言も日本語にする（ADR-77）。
- 署名は個人利用なのでアドホック署名（`bundle.macOS.signingIdentity: "-"`）。App Store には出さない。
  - リリース（M9。ADR-123）：`pnpm tauri build` で `target/release/bundle/macos/Kura.app` と `bundle/dmg/Kura_<版>_aarch64.dmg` ができる。`codesign -dv` は `Signature=adhoc`（flags adhoc,runtime）、`codesign --verify --deep --strict` は通る。公証はしないので `spctl --assess` は rejected になる。
  - 自分の Mac でビルドしたものは隔離の属性が付かないので、`/Applications` にコピー（`ditto`）すればそのまま開ける。ほかの Mac へ dmg で渡すと Gatekeeper に止められるので、システム設定 › プライバシーとセキュリティの「このまま開く」で開く。
  - 新しい版で既存のライブラリを開くと、足りないマイグレーションを当てる（M9 は 0005、M11 は 0007〜0009）。当てる前に空きを確かめてバックアップを作り（§14）、300ms を超えたら「ライブラリを更新しています」を出す（§3）。当てたあとは前の版では開けない（「新しい版の Kura で作られています」）。
- 書類・デスクトップ・ダウンロードを「ファイルを選択」で開くと macOS の許可ダイアログが出る。ドラッグ＆ドロップした項目は許可なしで読める。

## 12. 状態管理（フロント）

- Zustand のストアを機能ごとに分ける：`libraryStore`（開いているライブラリ・検索結果の ID 列）、`viewStore`（選択・表示の設定）、`itemCache`（ID → 詳細の LRU）、`playerStore`（キュー・現在位置・再生状態）、`jobsStore`（イベントで更新）。
- 検索結果の ID 列は `Uint32Array` で保持する。
- ジャスティファイ配置は「行の高さ目標・コンテナ幅・各アイテムの縦横比」から行分割を計算する純粋関数（`src/features/library/layout/justify.ts`）にし、テストする。縦横比は `items_query_ids` が ID と一緒に全件分返すので、詳細を取得する前に全体の高さと行分割が決まる（音楽は 1.0、不明は 1.0 として扱う）。
- `itemCache` は 4,000 件の LRU（アイテムとユニットで 1 つずつ）。見えている行の ID を 200 件ずつまとめて `items_get`・`units_get` する。編集したものは捨てずに取り直す（届くまで前の内容を出す）。取り込み中の一覧の取り直しは 1 秒、サムネイルのイベントでの取り直しは 0.5 秒（`pending` と作り直し中の `stale`）、解析のイベント（`analyze`）での取り直しは 1 秒（`analysisState` が `pending` のアイテムと表紙）にまとめる。取り直して中身が変わらなければ前の行のまま（描き直さない）。
- サムネイルを出す所はすべて `components/MediaImage`（M11）：`thumbUrl` に `thumbRev` を付け、`stale` も今の絵を出し（あるかは `hasThumb`）、解析で透過と分かった画像に市松模様、ドット絵を広げるときだけ最近傍（プレビューは装置の画素の整数倍）、フォントは mask で色を付ける（docs/design.md §3.1）。アニメの画像は、シアターとクイックルックで元のファイルを `<img>` に置かない（WebKit が回すので、`stillOriginal` で大きいサムネイルにする）。`thumbUrl` を呼ぶのは MediaImage だけで、`media.test.ts` がほかのファイルでの呼び出し・`kura://` の組み立て・`thumbState === 'ready'` だけの判定を見張る（.claude/rules/frontend.md）。
- **種類**（M11）：種類の型・並び・述語（キューに入るか・長さを持つか）は `src/ipc/kinds.ts` だけが bindings の定数 `ITEM_KINDS` から作る（画面ごとに `'image' | 'video' | 'audio'` を書かない）。ラベルは `ja.kinds`、検索の語は `QUERY_KIND_WORDS`。
- **ライブラリの更新**（M11）：`upgradeStore` がイベント `library://upgrade` を受け、`started` から 300ms を超えたら「ライブラリを更新しています」を出す（§3）。開けなかった理由は知らせではなく開く IPC の戻り値（`libraryStore.restoreError`）で出す。開いているライブラリから別のライブラリを開く・作るのに失敗したら、`library_info` で確かめ、どれも開いていなければ（Rust は前のライブラリを閉じてから開く）初回の画面に戻して `restoreError` に理由と場所を持つ。
- **試聴と効果音**（M11）：ループの試聴は `features/asset/auditionStore`（素材の欄のボタン・L のキー・メニューが同じ口を通す。一度に 1 つ。始めるとプレーヤーを一時停止し、効果音を止める。クイックルック中は始めない。プレーヤーが鳴り始めたとき・その試聴のボタンのある素材の欄（シアターの情報のパネルを含む）が消えたときに止める）。鳴らし方は `src/lib/assetAudio.ts` の `Audition`（本体だけのバッファを `loop` で回す。`/pcm` の `kura` チャンクの長さ・ループ点を使う `startProbed`）。効果音の即再生は `useSfxOnSelect`（`PlayerPrefs.sfxAudition`）。どちらも `features/asset/audioRuntime` の文脈（元のレートごと、`ContextPool`。上限 3。15 秒鳴らさなければ閉じ、ライブラリの画面を離れたらすぐ閉じる）を分け合う。素材の欄の値は `useAssetData`（解析待ちなら `request_analysis` を 1 回、解析のジョブの知らせで 500ms にまとめて読み直す）。
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
| `cargo run --release -p kura-devtools -- seed --items 100000 --out <path> [--analysis pending\|ready]` | ダミーライブラリを作る（画像 7・動画 2・音楽 1、2,000 タグ、60 フォルダ、200 件ごとに 10 件の set ユニット、ひな形 JPEG をクローンで配置、シード付き ChaCha8）。10 万件で約 28 秒。M11：今の作り方の指紋（サムネイルの `thumbs.plan`・解析の `analysis.plan`）をいつも置く（置かないと、初めて開いたときに M10 の作り方と比べて png・heic などの約 47% が作り直しになり、計測にジョブが混ざる）。`--analysis ready` は画像・音声に偽の事実（透過・色数・ドット絵・アニメ・ラウドネス・ループ）を解析のジョブと同じ書き込み（`analysis::store::write`）で入れて解析済みにする（NFR-02・03・07 の計測用。JPEG・HEIC は形式からの事実）。既定の `pending` は解析待ちのまま |
| `kura-devtools bench query --library <path> [--fake-facts]` | 開く・`query_ids`・`items_get`（200 件）・`item_detail`、サイドバーの一覧の種類・件数・タグの候補・タグの一覧の中央値（タグの一覧は 10 万件・タグ 2,000 個で 19〜22ms）。`--fake-facts`（M11）は画像・音声に擬似の事実（`media_facts`。`decoder = 'kura-devtools-fake'`）と素材の設定を入れて、素材のフィールド（透過・ドット絵・色数・レート・ラウドネス・ループ・アニメなど）の一覧とパレットの件数も測り、終わったら消す（本物の事実のあるライブラリでは断る。`analysis_state` は pending のままなので、「未解析 N 件」はいちばん重い場合） |
| `kura-devtools bench thumbs --src <dir> [--threads N]` | 画像サムネイルの生成速度（枚/秒。ジョブを通さず `kura_mac::media_backend()` の `image_thumbnails` を並べて呼ぶので、M11 ではいつも ImageIO の道。エンジンの道は `bench thumbs-job`） |
| `kura-devtools bench thumbs-job --src <dir> [--threads N] [--engine-cap <MiB>] [--no-engine] [--svg-in-process] [--out <dir>]` | サムネイルのジョブの速さ（M11。NFR-04）：`--src` を新しいライブラリにアプリと同じ渡しもので取り込み、できたサムネイルを消して pending に戻してから、サムネイルのジョブだけを動かす（道の割り振り・エンジンの一括の予算と一括のプールも本番と同じ）。道ごとの件数と作れなかった数を出す。`--engine-cap` は同時の確保の上限（既定 256 MiB）、`--no-engine` はエンジンの道を使わない（M10 の ImageIO の道と比べる）。メモリのピークは `/usr/bin/time -l` で外から測る。測り終えたらライブラリを消す。`--svg-in-process` は SVG を別のプロセスで読まない組で比べる（M11 の決定 23） |
| `kura-devtools bench svg-worker [--src <dir>] [--limit N] [--parallel N] [--spawns N] [--each] [--deadline-ms N] [--memory]` | SVG の働き手の手間（M11 の決定 23）：子の起動 1 回の時間（小さな SVG の形を `--spawns` 回）と、`--src` の SVG を 1 件ずつプロセスの中と働き手で読んだ時間（p50・p95・最大）・結果（値・記録・エラー）の一致。`--parallel` は働き手で N 件ずつ同時に、`--each` は 1 件ずつの形と値の時間（時間の上限を決める実測。`--deadline-ms` で上限を変える）。`--memory` は代わりに、`--src` の SVG を 1 件ずつプロセスの中で（働き手と同じデコーダ・同じ確保器の目印で数えて）読み、前段・木を作る段・描く段の確保のいちばん大きい量と働き手の確保の上限、その割合を出す（確保の上限を決める実測。ADR-164 (4)）。M4 Max の実測は docs/editing.md §9.5 |
| `kura-devtools assets --out <dir> [--bench <N>]` | 形式の見本を書く（M11。形式の表の画像・フォント・3D の行ごとに 1 つ以上：アルファ付きの PNG・ドット絵・タグ付きの aseprite・アニメの WebP・GIF・APNG・PSD・PSB・文字のある SVG・JPEG、`sips` で作るアルファ付きの HEIC・HEIF・AVIF・TIFF・BMP、システムのフォントの写しと包み直した WOFF・WOFF2、見本の glb。動画・音声は ffmpeg（mp4・mp3・aac・flac・opus・aiff）と kura-engine の書き手（10 秒以下の効果音・ループ点付きの Ogg Vorbis・smpl 付きの WAV））。macOS のテスト `every_image_font_and_model_format_gets_a_thumbnail_and_probe` がこれを取り込んで、行ごとにサムネイルと情報ができることを確かめる。`--bench <N>` は代わりに `bench thumbs-job` 用の PNG の多い組（2048² のアルファ付き PNG・3000×2000 の不透明な PNG・JPEG を N 枚ずつ、64² のスプライトを 4N 枚）を書く。作れなかったもの（`sips` が HEIC を書けない環境・ffmpeg がない）は理由を出す |
| `kura-devtools bench migrate (--library <path> \| --items <件数> [--history]) [--runs 3] [--work <dir>] [--json <file>]` | マイグレーションの時間と照合（M11。docs/data-model.md §5）：既存のライブラリの DB の写し（元には触らない）か、合成したスキーマ 6 の DB（`--history` で再生の記録・エントリ・取得元も）を `Library::open` で開き（当てる前のバックアップ → 当てる → TRUNCATE。空きの確かめはアプリが渡すものなので、ここではしない）、段ごとの時間・DB と WAL の大きさ・開き直す時間（更新後の 2 回目）を出す。最後の写しで、表ごとの行数が当てる前と同じ・`foreign_key_check` が空・FTS の照合・`quick_check`・台帳と items の 1 対 1 を確かめ、外れたら失敗にする |
| `kura-devtools bench import --src <dir> [--threads N] [--out <dir>]` | 取り込みの速さ（MB/秒・件/秒）と、同じファイルをハッシュだけした速さ。新しいライブラリを `--out`（既定は `--src` と同じ場所）に作って入れ、測り終えたら消す（M9） |
| `kura-devtools bench backup --library <path>` | DB のバックアップを 1 つ作る速さ（10 万件・58MB で 0.25 秒。M9） |
| `kura-devtools bench analyze --kind image\|audio [--limit N] [--src <dir>] [--work <dir>] [--json <file>]` | 解析のジョブの速さとメモリ（M11）：作業フォルダに見本のライブラリを作り（`--src` のファイルか、中身の違う合成の見本）、自分を子プロセスとして `/usr/bin/time -l` で起こして、アプリと同じエンジンの部品（物理メモリからの予算・`full_codec_set`）で解析のジョブを 1 本動かす。通しの件数/秒、読んで解いた 1 件の p50・p95・最大（ジョブの結果の `readMs`）、HEIC・HEIF の見出しの読み取りの件数・p50・p95・最大（`headerMs` の `header_*`。決定 15）、peak memory footprint を 1 行の JSON で出す |
| `kura-devtools import [--close-after-ms N]` / `check` | コマンドラインからの取り込みと整合性の確認（§6）。`--close-after-ms` は N ms 待ってジョブの途中でライブラリを閉じ、閉じるのにかかった時間を出す（M11 の決定 23 の確かめ） |
| `kura-devtools download --library <path> <url>...` | アプリと同じダウンロードのジョブで取得して入れる（§9。既定の選択・Cookie なし） |
| `kura-devtools edit crash-loop --library <新しい場所> [--runs 50] [--seed 1] [--min-ms 10] [--max-ms 500]` | 保存の本当に落とす試験（M12 の段 2。docs/editing.md §14・§14.1）：`--library` の場所に新しいライブラリを作り（もう何かあれば断る。本物のライブラリを指さない）、ドット絵の PNG を 1 つ取り込む。回ごとに自分を子として起こし（`edit crash-loop-child`。アプリと同じ渡しもの）、子は保存を繰り返す（ふだんは塗りつぶし・反転の上書き、版が 2 つ以上あれば 4 回に 1 回は版に戻す）。子だけ GC の時計を 8 日進めて開くので（`Services::with_gc_clock(GcClock::shifted(..))`。M12 の段 3）、開いたあと・保存のコミットのあとに GC が積まれ、置き換えたばかりの版を照合して消す（GC は保存に譲るので、2 回に 1 回は GC が動き終えるのを待ってから次の保存を積む）。親は ChaCha8 で決めた 10〜500 ms 待って SIGKILL し、落とした直後の DB と staging を見て（実行中の保存・GC のジョブ・台帳の予告（うち 'delete'）・staging の書きかけ）から開き直し（親の時計は本物なので GC は版を消さない）、ジョブが動き終えたあとに食い違いを数える：開いたときと終わったあとの整合性（ファイルのない台帳の行・DB にないファイル・参照されない台帳の行）、台帳のファイルの中身と hash、版の不変条件（`check_invariants`）、予告の行、staging のファイル、動き終えていない・失敗した保存と GC のジョブ、照合が合わずに残した版（固定の段のコーデックなので 0 のはず）、保存したと答えたのに版のないジョブ、アイテムの数。結果は 1 つの JSON（`mismatches`・`atCrash`・回ごとの `perRun`）で標準出力に書き、食い違いがあれば失敗で終わる。子のログは `<ライブラリ>.crash-loop.log`。M4 Max・debug のビルドで 50 回ずつ（2026-10-24）：seed 1 は食い違い 0（保存の途中で落ちた回 45・staging に書きかけ 19、版 461 のうち版に戻す 127、約 29 秒）、seed 2 は食い違い 0（保存の途中 39・予告のあとコミットの前 2・staging に書きかけ 18、版 623 のうち版に戻す 149、約 29 秒）。GC を混ぜた段 3（2026-10-24、ほかのビルドが走って負荷の高い同じ機械）：seed 1 は食い違い 0（保存の途中 37・GC の途中 7・予告 1・staging に書きかけ 14、版 299 のうち版に戻す 85・GC のジョブ 159 本・released 295・kept 0、約 27 秒）、seed 2 は食い違い 0（保存の途中 27・GC の途中 4・予告 2（うち GC の 'delete' 1）・staging に書きかけ 14、版 425 のうち版に戻す 97・GC のジョブ 201 本・released 420・kept 0、約 26 秒）、seed 3 を 200 回でも食い違い 0（保存の途中 144・GC の途中 39・予告 5（うち 'delete' 2）・staging に書きかけ 20、版 725・GC のジョブ 421 本・kept 0、約 132 秒） |
| `kura-devtools edit shape --in <file>` / `edit caps` / `edit render --doc <文書.json> --in <file> --out <file> [--format <形式>] [--options <JSON>]` | 編集のエンジンを手で確かめる（形式と形の表示、今の環境の能力の表、文書を当てて書き出し、ダイジェスト・記録・警告を表示。コーデックはアプリと同じ `full_codec_set`（固定の段 → macOS → ffmpeg）。docs/editing.md） |
| `kura-devtools bench edit [--preview-threads 3] [--bulk-threads 3] [--threads 4,16] [--budget 4GiB] [--photo <file>] [--quick] [--only 11,12] [--runs 3] [--json <file>] [--markdown <file\|->]` | 編集の性能（NFR-11〜17・E30。docs/editing.md §16）を、アプリと同じ `EngineRuntime`（§15）と `full_codec_set` で測る。判定はプレビュー・一括のプールを 3 スレッドにしたもの（ADR-154）で、`--threads` に並べた数は記録だけ。NFR-11・12 は表示の経路（`Engine::render_view`。24MP の写真は段 2 で判定し、段 1 も記録）で、慣らし 10 回のあと 30 回（p95 は最近順位法）、回の前後とケースごとにロードアベレージを記録する。NFR-16 は最後の文書・結果を別のエンジン・キャッシュなしの一括の経路（1・3・16 スレッド）で作り直して照らす。NFR-17 は自分を子プロセスとして `/usr/bin/time -l` で起こし、peak memory footprint から基準を引く。表（標準出力）・JSON・Markdown（下の M10 の表の形）で出す。判定は達成・未達・範囲外（決めた目標の範囲の外：太さ 32px を超える筆と減色のプレビュー。測っても達成・未達にしない。JSON の `verdict` は `out_of_scope`）・記録・見送り。`--quick` は極小の煙のテスト（debug でも回る。筆は 1・8・64px で、範囲外の記録の道も通す）。実写真は `--photo` で渡す（なければ合成の写真） |
| `cargo test -p kura-devtools mem_smoke` | 編集のメモリの見積もりの煙のテスト（段 10）：固定の小さな入力（プログレッシブの JPEG → 回転・縮小 → PNG、PNG → Indexed8 → PNG、WAV → 音量 → Ogg Vorbis、層の多いアニメの aseprite（256²・8 層・4 フレーム・リンクあり）→ 反転 → APNG、1px の市松の 2048² の PNG → 8 連結の塗りつぶし → PNG）を子プロセスで回し、peak memory footprint から基準（同じ子で、ファイルに触れる前に終えるもの）を引いた量がエンジンの見積もり（`Engine::estimate`。読むファイルの長さも渡す）× 1.25 ＋ 4 MiB 以下かを確かめる（読む段だけの量も）。macOS の CI で回る（`/usr/bin/time -l` のない Linux では見送る） |
| `KURA_LIBRARY=<path>` | アプリをこのライブラリで起動する（「前回のライブラリ」は書き換えない）。まだコミットしていないマイグレーションのある dev・debug のビルドは、必ずこれで seed か写しのライブラリに向ける（identifier が release と同じで、なければ app.json の前回のライブラリ＝本物を開いて当ててしまう。当てたものは直しても当て直されず、前の版では開けなくなる） |
| `VITE_KURA_BENCH=1` | グリッドを自動でスクロールし、フレーム時間を `bench_report` でログに書く |
| `kura-devtools selftest-library --out <path> [--web <dir>] [--font <ttf>]` | kura:// と CSP の自己点検用のライブラリを作る（M11。§4.2）。アルファ付き PNG・JPEG・WAV は DB に入れ、`<script>` 入りの SVG・ttf（システムのフォントの写し）・glb は `files/` に直に置き、WAV の `kura-peaks` と、中身のハッシュの目録（決まった名前 `5e1f7e57…` で `/file` から読める）も置く。`--web` には F2 のページ `f2.html` を書く。整合性の確かめでは、DB にないファイルが 4 つ孤立して数えられる |
| `VITE_KURA_CHECK=1` | ループの試聴の継ぎ目の自己点検（M11。`src/dev/seamCheck.ts`）。試聴を始めるたびに、同じ PCM と同じ組み立て（本体を loop・前奏のあとに本体・継ぎ目の前後を続けて。始める時刻はフレームの境、バッファの中の始まりは「フレーム ÷ レート」の秒）を OfflineAudioContext で描き、PCM を計画の順につないだ期待の標本と 1 標本ずつ照らして、`bench_report('audition-seam', {hash, mode, rate, channels, windows, seams, frames, mismatches, first, ms})` でログに書く。完了条件は `mismatches` が 0。本物との違い：始める時刻の起点は 0（本物は文脈の今の時刻の少し先）。描く長さは窓ごとに 30 秒まで（前奏と本体が長ければ、前奏の終わりの 1 秒前からの窓と本体の継ぎ目の 1 秒前からの窓に分ける）。通常のビルドには入らない（条件の中の動的 import が消える。`src/dev/seamCheck.test.ts` が読み込む所を見張る） |
| `VITE_KURA_SELFTEST=1`（と任意の `VITE_KURA_SELFTEST_F2=<f2.html の URL>`） | 起動時に §4.2 の P1〜P8・F1〜F4 を試し、`bench_report('selftest')` と `selftest-nav` でログに書く。selftest のライブラリを `KURA_LIBRARY` で開き、`RUST_LOG=info,kura_app_lib=debug` で要求ごとのログ（label・Origin・status・ACAO）も出す。CSP は `VITE_KURA_SELFTEST=1 pnpm tauri build --debug --no-bundle` で作った `target/debug/kura-app` で確かめる（`pnpm tauri dev` には CSP が付かない）。F2 のページは `python3 -m http.server 18432 --bind 127.0.0.1 -d <dir>` などで配る |

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

M9 の実測（NFR-01〜07。Apple M4 Max（高性能 12・高効率 4 コア）、内蔵 SSD、リリースビルド、10 万件のダミーライブラリ。ほかのアプリが動いている状態（ロードアベレージ 5〜9）で測った）：

| NFR | 項目 | 結果 | 目標 | 判定 |
|---|---|---|---|---|
| 01 | 起動から最初のタイルの詳細を返すまで（起動した時刻からログの時刻まで、3 回） | 0.41〜0.60 秒 | 1.5 秒以内 | 達成 |
| 02 | `VITE_KURA_BENCH=1` の自動スクロール（547 フレーム） | 59.8fps・p50 17ms・p99 20ms、20ms 超え 5 | 60fps | 達成 |
| 02 | 同じ（表示形式を入れたあと、601 フレーム） | ジャスティファイ 60fps・p99 18ms・20ms 超え 1、グリッド 60.1fps・p99 20ms・20ms 超え 0 | 60fps | 達成 |
| 03 | タグ 3 つの AND／全文検索（3 文字・2 文字） | 0.5〜0.9ms／0.19ms・20ms | 50ms／100ms 以内 | 達成 |
| 04 | 画像サムネイル（4000×3000、8 並列） | JPEG 165 枚/秒、HEIC（ImageIO で書いたもの）52〜61 枚/秒、HEIC（Photoshop で書いたもの）34〜38 枚/秒 | 50 枚/秒以上 | JPEG と一般的な HEIC は達成。HEIC はデコードが上限（4 並列で頭打ち。下の注） |
| 05 | 取り込み（大きいファイル 1GB×4、6 並列） | 1.07 → **3.45 GB/秒**（ハッシュの読み方を直した。ADR-113）。ハッシュだけなら 5.2〜6.9 GB/秒、ディスクの読み込み（キャッシュなし）は 5.9〜7.1 GB/秒 | ディスクの速度が上限 | 直して達成 |
| 05 | 取り込み（284KB の JPEG×8,000、6 並列、3 回） | 3,000〜3,700 件/秒（0.84〜1.05 GB/秒）。ハッシュだけなら 7.5 GB/秒 | 同上 | 達成（ハッシュではなく、1 件ごとのクローン・rename・メディア情報の読み取りが上限） |
| 06 | 1 万エントリ・5 万アイテムのキュー（3 回） | ファイル単位 10〜17ms、ユニット単位 6〜10ms | 50ms 以内 | 達成 |
| 07 | 10 万件を表示（アプリ＋WebContent＋GPU＋Networking の phys_footprint の合計） | 自動スクロールのあと 296MB、末尾までスクロールしたあと 379MB（表示形式を入れたあとの自動スクロールのあと 306MB） | 500MB 以内 | 達成 |

- NFR-04 の HEIC：`CGImageSourceCreateThumbnailAtIndex` を通さずにデコードだけしても同じ速さで、縮小しながらのデコード（`kCGImageSourceSubsampleFactor`）も速くならない。HEVC のデコードが上限なので、アプリ側では変えない。
- NFR-05 の小さいファイル：並列数 6 と 8 は誤差の範囲、16 は遅い（APFS のクローンと rename の取り合い）。ADR-21 の 6 並列のままにする（ADR-114）。HDD はこの環境にないので測っていない。
- 乱数のデータで作った画像・動画は、メディア情報の読み取りが ffprobe へのフォールバック（プロセスの起動）になり、1 件ごとに遅くなる。取り込みの計測には中身の正しいファイルを使う。

M10 の実測（NFR-11〜17。`kura-devtools bench edit --runs 3`。Apple M4 Max（高性能 12・高効率 4 コア）、128 GB、macOS 26.7、リリースビルド、プレビューのプール 3・一括のプール 3（判定の条件。ADR-154）、予算 4 GiB（取り置き 1 GiB）、合成の入力（ChaCha8）、コミット d1ce160、ロードアベレージ 4.8〜6.4（各回の始め。ケースごとの記録は 3.6〜10.4）で測った。3 回の幅。NFR-11・12 の時間は「エンジンの中」（編集を受けてから、表示の経路（`Engine::render_view`。docs/editing.md §12.8）でプレビューの値と変わったタイルの一覧ができるまで。セッションの手・差分の計画・写真の段の計画・予算の確保を含み、ダイジェスト・エンコード・IPC・WebKit を含めない。表示する範囲は出力の全体）。24MP の写真は段 2 で判定し、段 1 は記録）：

| NFR | 項目 | 結果 | 目標 | 判定 |
|---|---|---|---|---|
| 11 | ドット絵 2048²：パレットの色の差し替え（Indexed8） | p50 0.03・p95 0.04 ms | p95 25 ms | 達成 |
| 11 | ドット絵 2048²：形式の変換（Rgba8 → Rgb8 の下地の色） | p50 9.54〜10.2・p95 13.6〜14.6 ms | p95 25 ms | 達成 |
| 11 | ドット絵 2048²：キャンバスの大きさ | p50 3.33〜3.34・p95 3.60〜3.76 ms | p95 25 ms | 達成 |
| 11 | ドット絵 2048²：最近傍の拡大 | p50 12.6〜13.4・p95 13.9〜14.1 ms | p95 25 ms | 達成 |
| 11 | ドット絵 2048²：透明な縁のトリム | p50 1.37〜1.45・p95 1.96〜2.05 ms | p95 25 ms | 達成 |
| 11 | ドット絵 2048²：格子の分割 | p50 1.65〜1.82・p95 2.03〜2.70 ms | p95 25 ms | 達成 |
| 11 | ドット絵 2048²：減色（16 色のスプライトを Rgba8 → Indexed8、色の数を変える） | p50 21.5〜22.0・p95 22.6〜22.8 ms | 対象外（requirements.md の NFR-11） | 範囲外 |
| 11 | 5 分の 44.1 kHz ステレオ：利得 | p50 8.75〜9.51・p95 10.3〜12.2 ms | p95 25 ms | 達成 |
| 11 | 同じ：フェードインの長さ | p50 12.4〜12.9・p95 12.7〜14.7 ms | p95 25 ms | 達成 |
| 11 | 同じ：正規化の目標（解析はキャッシュに当たる） | p50 37.5〜38.8・p95 38.4〜40.1 ms | p95 25 ms | 未達（下の注） |
| 11 | 24MP の写真・段 2：切り抜きの位置 | p50 0.11〜0.19・p95 0.16〜0.23 ms | p95 25 ms | 達成 |
| 11 | 同じ：Lanczos3 の縮小（40〜60%） | p50 16.2〜16.4・p95 18.6〜19.2 ms | p95 25 ms | 達成 |
| 11 | 同じ：鎖（切り抜き → Lanczos3 50% → グレー）の切り抜き | p50 16.5〜16.8・p95 16.8〜17.2 ms | p95 25 ms | 達成 |
| 11 | 24MP の写真・段 1：切り抜き・Lanczos3・鎖 | p95 0.49〜0.79・64.0〜64.8・67.8〜72.2 ms | — | 記録 |
| 12 | 4096² の筆 1 本（500 点）の確定：1・8・32px（前の筆 0・999・5 万本） | p95 0.20〜0.29・0.48〜0.59・5.74〜6.17 ms（本数によらない） | p95 8 ms | 達成 |
| 12 | 同じ：64px | p50 22.3〜23.4・p95 23.3〜24.6 ms | p95 8 ms（太さ 32px まで。requirements.md の NFR-12） | 範囲外 |
| 12 | 筆のあとに最近傍 ×4 が続く文書（1024² → 4096²・前の筆 999 本）：8px | p50 1.31〜1.37・p95 1.86〜2.17 ms | p95 8 ms | 達成 |
| 12 | 同じ：32px | p50 6.51〜6.61・p95 7.62〜8.04 ms | p95 8 ms | 未達（3 回のうち 1 回。下の注） |
| 12 | 同じ（2048² → 8192²）：8・32px | p95 2.13〜2.15・7.73〜7.88 ms | — | 記録 |
| 12 | 全面の塗りつぶし 4096²：4 連結のつながった全面・画像全体の同じ色 | p95 33.2〜33.7・32.7〜33.1 ms | p95 30 ms | 未達（下の注） |
| 13 | 保存：合成の写真 6000×4000 の JPEG → 線形光で 50%（Lanczos3）→ JPEG q90（ファイルの読み書き・元のハッシュ・予算の確保・出力のダイジェストを含む） | 中央値 414〜420・最大 418〜426 ms（bilinear は中央値 289〜294 ms） | 中央値 0.5 秒 | 達成 |
| 14 | 一括：ドット絵の PNG 1,000 件（トリム → 最近傍 ×4、メモリの中で読み書き） | 通し 468〜510 ms（各回の最大） | 5 秒 | 達成 |
| 15 | 5 分の 44.1 kHz ステレオの読み取り ＋ 波形・ラウドネス（トゥルーピーク）：WAV・FLAC | 中央値 195〜198・336〜346 ms | 中央値 2 秒 | 達成 |
| 16 | 23 ケース（NFR-11〜15 の判定の条件の最後の文書・結果） | エディタの経路（3 スレッド・キャッシュ・差分。等倍の全体を描いたダイジェスト）と、別のエンジンで作り直した一括の経路（1・3・16 スレッド）で、値のダイジェスト・固定のコーデックの PNG と WAV のバイト列・保存の JPEG・一括の PNG のバイト列・解析の正規の形がすべて一致 | 一致 | 達成 |
| 17 | 保存の通し（6000×4000 → Lanczos3 50% → JPEG、予算の確保つき） | 実測 187〜188 MiB（見積もり 364.5 MiB） | 予算 4 GiB 以内 | 達成 |
| 17 | エディタ（4096² ＋ 筆 999 本 ＋ 8px を 20 本、キャッシュの確保つき） | 実測 163.5〜163.8 MiB（見積もり 206.8 MiB） | 予算 4 GiB 以内 | 達成 |
| 17 | 小さい予算：保存（128 MiB）・6000×4000 の写真をエディタで開く（64 MiB） | どちらも落ちずに Limit | 超える入力はエラー | 達成 |

スレッドの数による違い（`--threads 4,16 --only 11,12,13,14,15`、1 回、ロードアベレージ 4.1〜5.2。p50／p95、ms。保存・解析は中央値、一括は通し）：

| 項目 | 3 | 4 | 16 |
|---|---|---|---|
| ドット絵 2048²：形式の変換・最近傍の拡大 | 9.97／14.4・12.6／13.8 | 7.42／7.63・9.53／10.1 | 2.61／2.89・3.20／3.56 |
| ドット絵 2048²：減色 | 21.6／22.7 | 21.6／22.2 | 21.6／22.2 |
| 正規化の目標 | 38.3／39.7 | 37.5／37.9 | 35.8／36.3 |
| 24MP の写真・段 2：Lanczos3・鎖 | 16.3／18.8・16.8／17.1 | 11.9／12.2・15.9／16.4 | 11.9／12.1・9.00／9.15 |
| 24MP の写真・段 1：Lanczos3・鎖 | 59.6／64.5・65.8／68.3 | 54.8／56.5・50.4／51.4 | 17.8／20.2・18.5／20.8 |
| 筆（前の筆 5 万本）：8・32・64px | 0.50／0.56・5.84／6.14・23.6／24.2 | 0.46／0.54・5.80／6.03・23.5／24.0 | 0.47／0.55・5.84／6.01・23.4／24.1 |
| 筆のあとに最近傍 ×4（1024²）：8・32px | 1.33／1.96・6.79／7.61 | 1.04／1.46・6.56／7.25 | 0.89／1.11・6.31／6.88 |
| 全面の塗りつぶし：連結・画像全体の同じ色 | 28.3／35.6・26.6／33.3 | 20.1／21.1・19.7／20.0 | 7.21／7.49・6.73／6.92 |
| 保存（Lanczos3） | 420 | 358 | 227 |
| 一括 1,000 件 | 503 | 365 | 119 |
| 解析（WAV） | 199 | 171 | 136 |

- 筆・正規化・減色は、スレッドの数でほぼ変わらない。24MP の写真は段 2（各軸 1/4。bench edit の `PHOTO_LEVEL`）で判定する（段 1 は 3 スレッドで p95 64〜72 ms）。
- 段 10 の途中（コミット c8286e7。`render_cached` で等倍の全体を描いていたとき）との比較（p95）：筆のあとに最近傍 ×4 の 1024²・8px は 18.1〜19.3 → 1.86〜2.17 ms、32px は 23.0〜23.6 → 7.62〜8.04 ms、2048² は 60〜72 → 2.1〜7.9 ms。24MP の写真の鎖は、等倍で描いた仮の値 266〜268 ms → 段 2 で 16.8〜17.2 ms。
- NFR-11 の正規化：ラウドネスの解析そのものはキャッシュに当たる（解いた回数 0）が、目標の利得を求める `analysis::loudness::Measured::gain_to` が候補ごとに積分ラウドネスを全ブロック（5 分で 2,997 ブロック）で数え直し（O(ブロック²)）、32〜38 ms かかる（使い捨てのテストで確かめた）。操作そのものの時間で、16 スレッドでも p95 36 ms。速くするには、値を変えずに数え直しを減らす並べ方が要る（後の段）。
- NFR-12 の 64px の筆と全面の塗りつぶし：差分の計画と実行器はどちらも 0.1 ms 未満で、残りは描く操作そのもの。64px は範囲外（目標の範囲は筆 32px まで）で、記録だけにして判定しない。塗りつぶしは 4 スレッドで p95 約 21 ms、16 スレッドで約 7 ms。
- NFR-12 の 32px の筆のあとに最近傍 ×4 が続く文書：表示の経路で、筆は差分の適用、下流の最近傍 ×4 は変わったタイル（筆 1 本あたり平均 19 枚）だけを描く。32px の筆そのものが 5.5〜5.9 ms、最近傍のタイルが 1〜2 ms で、p95 7.62〜8.04 ms（3 回のうち 1 回が 0.04 ms 超え。境目）。実行器の側で値を変えずに減らせるものは入れ終えた（さらに縮めるには、筆か最近傍の範囲の描画の実装を速くする）。4・16 スレッドでは p95 7.25・6.88 ms。
- 減色（Indexed8 への変換）は 2048² の 16 色のスプライトでは目標に入るが、色の多い最悪の形（1024² の乱数を 256 色に。docs/editing.md §16 の段 5 の実測で 316〜496 ms）では届かないので、プレビューの対象外にした（古い結果を出したまま、描けたら差し替える）。表の判定は範囲外（記録だけで判定しない）。
- NFR-17 の判定は「実測 ≤ 予算」。実測が見積もり × 1.25 ＋ 4 MiB を超えたら注を出す（今回はなし）。基準は、実行の場を作るだけの子（保存のケースはアプリのコーデックの組 `full_codec_set` も作る子）で、保存で 8.3〜8.4 MiB、エディタで 2.5 MiB。エディタの実測には、開いた文書（筆 999 本）とセッションの履歴も入る。
- 負荷：2 回目の途中でケースごとのロードアベレージが 10.4 まで上がった（正規化・写真・筆のケース）が、p95 は 1・3 回目と大きく変わらない（違いは最大で段 1 の鎖の約 4 ms）。32px の筆のあとに最近傍 ×4 の 8 ms 超えは、いちばん負荷の低い 1 回目（ケースのロードアベレージ 3.6）で出た。段 10 の最初の試し（ロードアベレージ 12.8〜16.3）では、5 万本の筆の p95 が 8〜15 ms に跳ねた。

M11 の実測（NFR-01〜07 の測り直しと M11 の計測。2026-10-20。Apple M4 Max（高性能 12・高効率 4 コア）、128 GB、macOS 26.7、内蔵 SSD、リリースビルド、コミット fbd7580（確保器 `kura_mac::ReturningAllocator` を入れたあと。NFR-05 の M10 は d6edff9）。ほかのアプリが動いている状態（ロードアベレージ 3.4〜7）で測った。seed は 2 つ：更新後の初回は M10 の seed 10 万件（スキーマ 6。DB 51.8 MB ＋ WAL 10 MB）の APFS の写し、落ち着いた状態は M11 の seed 10 万件（`seed --items 100000 --analysis ready`。画像 70,151 件・音声 9,968 件に偽の事実、作り方の指紋あり））：

| NFR | 項目 | 結果 | 目標 | 判定 |
|---|---|---|---|---|
| 01 | 起動から最初のタイルの詳細を返すまで（M11 の seed、4 回。1 回目は初めて開いたとき） | 0.40〜0.58 秒（0.581・0.401・0.410・0.397） | 1.5 秒以内 | 達成 |
| 01 | 更新後の初回（M10 の seed の写し、3 回。スキーマ 6 → 9 を含む） | 1.39〜1.43 秒（うち更新 0.86〜0.90 秒：当てる前の確かめとバックアップ約 0.15 秒・0007〜0009・TRUNCATE。ログの `upgrade_ms`。サムネイルの作り方の指紋で 46,860 件を stale にする 71 ms は、開いたあとに起動のスレッドで最初のタイルの詳細と並んで動き、この時間には入らない） | NFR-01 の外（M11 の決定 1） | 記録（1.5 秒にも収まった） |
| 01 | 更新直後の 2 回目（同じ写し、3 回。サムネイルの作り直しと解析が走っている） | 0.39 秒（0.390・0.394・0.391） | 1.5 秒以内 | 達成 |
| 02 | `VITE_KURA_BENCH=1` の自動スクロール（M11 の seed、601 フレーム、有効な 3 回） | 60〜60.1fps・p50 17ms・p95 17ms・p99 18ms、20ms 超え 0 | 60fps | 達成 |
| 03 | 素材の事実のフィールドで一致が数千件まで（`bench query --fake-facts`。`透過:あり`・`ドット絵:あり`・`色数<=16`・`アニメ:あり`・`レート:48k`・`ビット:24`・`チャンネル:ステレオ`・`ラウドネス>-14`・`ピーク>-1`・`ループ:あり`・組み合わせ） | 一覧 0.6〜9.6 ms・パレット 10〜24 ms | 50 ms（計画の目安） | 達成 |
| 03 | 素材の事実のフィールドで一致が 6〜9 万件（広い条件） | `-透過:あり` 43 ms（パレット 51 ms）、`透過!=あり` 72 ms（75 ms）、`ドット絵:なし` 91 ms（114 ms） | 50 ms（計画の目安） | 目安を超える（下の注。M12 で直す：決定 17） |
| 03 | タグ 3 つの AND／全文検索 | 0.5〜0.9 ms／0.16〜19 ms | 50 ms／100 ms 以内 | 達成 |
| 04 | サムネイルのジョブ（`bench thumbs-job`、`assets --bench 24` の 168 件：2048² のアルファ付き PNG・3000×2000 の不透明な PNG・JPEG を 24 枚ずつ、64² のスプライト 96 枚。エンジン 144・ImageIO 24） | 105〜112 枚/秒 | 50 枚/秒以上 | 達成 |
| 04 | 大きな透過 PNG だけ（48 件。同時の確保 256 MiB） | 31 枚/秒（512 MiB なら 46 枚/秒）。エンジンを使わない（M10 の ImageIO の JPEG）と 194 枚/秒 | 50 枚/秒以上 | 下回る（判定の範囲。requirements.md の NFR-04） |
| 05 | 取り込み（35 KB の JPEG 4,000 件、M10 と M11 を交互に 3 回ずつ） | M10 7,561〜7,880 件/秒、M11 7,450〜7,666 件/秒（−2%） | ディスクの速度が上限 | 達成（誤差の範囲。台帳のトリガで遅くならない） |
| 07 | 10 万件を表示（自動スクロールのあと、アプリ＋WebContent＋GPU＋Networking の phys_footprint の合計、有効な 3 回） | 334〜339 MB（アプリ 53〜57・WebContent 248〜249・GPU 27〜28・Networking 5） | 500 MB 以内 | 達成 |
| 07 | ジョブが動いている間（225 件のライブラリで、サムネイル 206 件と解析 217 件を GUI で走らせる。5 分の曲 5 本を含む） | 最大 553 MB（アプリ 150 MB）。ジョブのあとアプリ 124 MB・合計 253 MB に戻る | NFR-07 の外（NFR-17 の一括の予算で数える。M11 の決定 2） | 記録 |
| 07 | 同じ、確保器を入れる前（libmalloc のまま） | 最大 956〜968 MB（アプリ 588 MB）。ジョブのあとも 150 秒たってアプリ 588 MB・合計 712 MB のまま（空の MALLOC_LARGE 445 MB を libmalloc が抱えたまま） | — | 確保器で直した（§7、ADR-163） |

- **マイグレーション**（`bench migrate --runs 3`、M10 の seed 10 万件）：開く処理の全体 713〜715 ms（バックアップ 42〜43 ms、0007 667〜671 ms、0008 1 ms、0009 0.2 ms、TRUNCATE 0.7 ms）。21 表の行数 504,767 が当てる前と同じ、`foreign_key_check` 0 件、FTS の照合と `quick_check` は ok、台帳と items は 1 対 1（10 万行）。段 1 の値（ロードアベレージ 9〜12）は docs/data-model.md §5。
- **NFR-01 の測り方**：開いたことのログ（「起動から前回のライブラリを開くまで」「起動から最初のタイルの詳細を返すまで」）の `upgraded`・`upgrade_ms` で、更新後の初回と 2 回目以降を分ける（docs/data-model.md §5）。
- **NFR-03 の広い条件**：遅いのは、数万件の 32 バイトのハッシュで items を引くところ（一致する事実の集合を作るだけなら 5 ms）。整数の鍵で引けば約 13.5 ms の見込み（seed で台帳の ID と items の ID がたまたま一致することを使って測った値。列を足したときの値は推測）。比べると、既存の `種類:画像`（7 万件）は 9〜18 ms。M12 で台帳と版の表を触るときに items に `blob_id` を持たせて直す（M11 の検証のあとのユーザーの決定 17。roadmap.md の M12・ADR-161）。 **M12 で直した**（ADR-166）：`items.blob_id`（0010）で引き、⌘K の件数と未解析の件数を接続 2 つで同時に数えて、広い条件も一覧 27〜37 ms・パレット 25〜40 ms（2026-10-24、同じ写し・同じ機械で M11 の形は 44〜103 ms・53〜127 ms）。
- **アニメの検索を「格子の設定 ?? 解析のコマ」にしたあと**（M11 の決定 16。2026-10-20、同じ写しに擬似の格子 719 件を足して設定は 1,436 件。ロードアベレージ 9〜10、3 回。コミット a67c536）：`アニメ:あり`（4,233 件）一覧 7.8〜8.4 ms・パレット 26〜27 ms、`種類:画像 アニメ:あり` 7.7〜8.6 ms・27〜28 ms、`透過:あり アニメ:あり` 10〜11 ms・22〜23 ms、`透過:あり アニメ:なし` 11〜12 ms・29〜30 ms、`種類:画像 ドット絵:あり` 3.2〜3.9 ms・26〜28 ms（同じ AND の種類を `+i.kind` にする前は、`種類:画像 アニメ:あり` 27 ms・46 ms、`種類:画像 ドット絵:あり` 22 ms・45 ms）。広い条件の `アニメ:なし`（65,918 件）は 90〜93 ms・98〜102 ms、`-アニメ:あり`（95,767 件）は 50〜51 ms・67〜68 ms、`種類:画像 ドット絵:なし` は 96 ms・120 ms（種類の索引を使えば 84 ms・101 ms）で、上の広い条件と同じ原因なので M12 で直す。
- **一覧の概要**（`items_get` の 200 件。M11 の seed、素材の値を JOIN する）：0.25〜0.27 ms（M2 は 0.16 ms）。
- **解析**（`bench analyze --src`。実ファイルの見本。確保器を入れたあと。予算 4 GiB・一括のプール 12）：画像 17 件（JPEG 2 件は読まない）で 136 件/秒・1 件の p50 3.0 ms・p95 38 ms（2048² の PSD・PNG）・ピーク 43 MB。音声 10 件（5 分の曲 5 本を含む）で 7.06 件/秒・p50 30 ms・p95 329 ms・ピーク 150 MB（確保器を入れる前は 483 MB。速さは −1.6% で誤差の範囲）。
- **HEIC の見出し**（決定 15。`bench analyze --src`、ImageIO で書いた中身の違う 4032×3024 の不透明な HEIC（平均 2.71 MB）。ページキャッシュに載った状態。ロードアベレージ 11〜12）：1 件のジョブで見出し 2.1〜2.3 ms（ImageIO の最初の呼び出しを含む）。1,000 件のジョブで、見出しは p50 0.62〜0.63 ms・p95 0.70〜0.72 ms・最大 2.1〜2.2 ms、ジョブ全体（1 件ごとに処理中の印と書き込みの 2 回のコミットを含む）は 0.73〜0.74 秒（約 1,350 件/秒）、ピーク 19 MB。うちファイル全体を読むのは p50 0.15 ms（Platform の段の形は入力を全体読んで ImageIO に渡す）。8064×6048（6.2 MB）の見出しは 1 件で 2.9 ms。アルファのある 3000×2000 の HEIC は見出し 2.0 ms ＋ 読んで解くのに 128 ms・ピーク 111 MB。ページキャッシュにないとき（`purge` は管理者の権限が要るので測っていない）は、ファイルを読む分が増える。
- **試聴の PCM・アニメの帯**：§4 の「`/pcm` と `item_anim_get` をジョブにしない理由」。
- **kura:// と CSP の自己点検**：§4.2（最後のコミットでも build で確かめた）。
- **形式の見本**（`assets`・`assets --bench 24` の出力と解析の計測の実ファイル、合わせて 227 件を `kura-devtools import`）：取り込み・サムネイル・解析が 5 秒で終わる。サムネイルを作れなかった 18 件はすべてジャケットのない音声（ADR-34）で、画像・フォント・3D はすべて作れた。透過があるのにサムネイルが JPEG のものは 0 件。

## 14. 書き出しとバックアップ（M9）

- **書き出し**（`kura_core::export`、ジョブ `kind = 'export'`。ADR-120）：
  - 選んだアイテム（重ねて選んだものは 1 回）を書き出し先のフォルダへ、ユニットはユニット名のフォルダの中に並び順でコピーする。読むのはライブラリの `files/` だけで、同じボリュームならクローン。
  - ファイル名は元のファイル名（`/`・`:`・制御文字は `_`、先頭の `.` は `_`、255 バイトまで）。書き出し先の既存のファイル・フォルダは上書きしない：一時ファイル（`.kura-export-<job>-<n>.part`）にコピーしてから、すでにあれば失敗するハードリンクで名前を付ける（あれば「名前 2.jpg」）。ハードリンクが使えないボリュームでは、ないことを確かめて名前を変える。ユニットのフォルダは `create_dir`（あれば「鎌倉 2」）。
  - 最後に `kura-export.json`（あれば「kura-export 2.json」）を書く：`{ app, version, exportedAt, items: [{ file, originalName, title, kind, hash, sizeBytes, width, height, durationMs, rating, note, tags, folders, units, sourceSite, sourceUrl, capturedAt, importedAt }] }`。日時は ISO 8601 の UTC、タグは「名前空間:名前」、フォルダはいちばん上からの名前の列。
  - 一時停止・再開できる（位置・書いたファイル・ユニットのフォルダの名前を `result_json` に保存）。中止しても書いたファイルは消さない。ライブラリのファイルがないものは失敗として数えて続ける。
  - 入口：ファイル › 書き出す…（⌘⇧E）、項目のメニュー。進捗は取り込みと同じカプセルに出し、終わったら「Finder で表示」。
- **バックアップ**（`kura_core::library::backup`。ADR-121）：
  - SQLite の backup API で、読み取り専用の別の接続から `backups/library-YYYYMMDD-HHMMSS.sqlite`（UTC）へ少しずつ写す（書き込みは止めない）。一時ファイルに写してから名前を変え、1 つのファイルで開けるよう `journal_mode = DELETE` にする。
  - 開いて 20 秒後と、そのあと 1 時間ごとに確かめ、前回から 24 時間以上たっていれば作る。新しい 7 個を残し、`backups/` の中のこの形の名前のものだけ消す。設定画面から今すぐ作れる。
  - 戻すのはアプリの外で手で行う（docs/data-model.md §1）。
- **マイグレーションの前**（M11。docs/data-model.md §5、ADR-156）：未適用のマイグレーションがある既存のライブラリ（版 1 以上）を開くときは、当てる前に次の順で確かめる。
  1. 空き：DB の大きさ（頁の数 × 頁の大きさ）から要る空きを見積もる（`library::upgrade::space_needed`＝DB の 3 倍 ＋ 64 MiB。当てる前のバックアップ 1 つと、作り直す表を DB の中と WAL に 1 つずつ）。ボリュームの空き（アプリが渡す `UpgradeHooks::free_space`。macOS は kura-mac の `volume_capacity` の「使用可能」）が足りなければ、バックアップも取らずに断る（`NoSpace`。ライブラリは変えない）。空きが読めなければ確かめずに進む（ログに warn）。10 万件の seed（DB 51.8 MB）で測った増え方の最大は 119.6 MB（DB の 2.31 倍）で、見積もり（約 222 MB）に収まる。
  2. バックアップ：書き込みの接続から backup API で、いつもの名前（`backups/library-YYYYMMDD-HHMMSS.sqlite`・DELETE モード）に一度に写す。7 個の保持に乗る。失敗したら当てずに開くのを失敗にする（ディスクの空きが足りなければ `NoSpace`、ほかは `BackupFailed`。当てる途中で空きが尽きたときも `NoSpace`）。新しいライブラリ（版 0）では作らない。
  - 当てる・知らせ・失敗の code は §3（`library://upgrade`・`LibraryTooNew`・`NoSpace`・`BackupFailed`・`UpgradeFailed`）と docs/data-model.md §5。

## 15. 編集（M10〜。正本は docs/editing.md）

- **クレート**：`kura-engine` がエンジン本体（ADR-134）。kura-core は保存・版・GC・解析と一括加工のジョブと DB（M11 以降）と ffmpeg の段（段 9）、kura-mac は ImageIO・AudioToolbox によるコーデック（Platform の段。段 9）、src-tauri はコマンドと kura:// の `/edit`・`/peaks`（M11・M12）を受け持つ。
- **文書**：元に対する累積の操作のノードの並び（docs/editing.md §4、ADR-135）。エディタとの通信で送るのは文書だけで、画素・波形は Rust が描いて kura:// で配る。
- **決定性と版**：docs/editing.md §8（ADR-136・138）。kura-engine の `clippy.toml` で禁じる API を機械的に強制し、ゴールデンは追記だけにする。
- **コーデック**：Pinned（Rust）→ Platform（kura-mac）→ External（ffmpeg）の順。C のライブラリは同梱せず、足りないエンコーダは自作する（ADR-137）。
- **実行の場**（段 10。ADR-154、docs/editing.md §12）：kura-core の `edit::EngineRuntime` が、エンジン 1 つ・プレビュー（エディタ）と一括（保存・一括加工のジョブ）の 2 つのスレッドプール・メモリの予算（`kura_engine::budget::MemoryBudget`）を持つ。
- **エンジンの部品**（M11）：開いているライブラリごとに `edit::EngineServices` を 1 つ持つ（`LibrarySession::engine`）。中身は実行の場（`OnceLock`）とコーデックの組（固定の段 → Platform → ffmpeg）で、どちらも最初に使うとき（解析・エンジンのサムネイルのジョブ、試聴の PCM・アニメの帯の `spawn_blocking`）に作り、ライブラリを開くだけでは作らない（NFR-01 の起動の道に載せない）。作る口（`edit::EngineFactory`）はアプリが `Services { media, engine, upgrade }` で `LibrarySession::open`・`create` に渡す（src-tauri の `MacEngineFactory` と kura-devtools の同じもの：予算は `kura_mac::physical_memory`、コーデックの組は `kura_mac::full_codec_set(&kura_mac::pinned_options(), staging/engine)`）。ライブラリを切り替えると別物になり（ffmpeg の一時ファイルはそのライブラリの `staging/engine`。開くときの掃除で消え、閉じるときに空なら消す）、外部ツールの場所を変えると（`tools_set_path`）コーデックの組だけを次に使うときに作り直す（ffmpeg を探し直す。実行の場は作り直さず、予算を 2 つにしない。ノードのキャッシュは作ったエンジンに縛られるので、ライブラリを開いている間はエンジンを作り直さない）。プールの大きさの既定は `pool_sizes`（プレビュー max(1, ⌊コア/4⌋)、一括 max(1, コア − プレビュー)。16 コアで 4・12。スレッド名は `kura-edit-preview-N`・`kura-edit-bulk-N`）、予算の既定は `default_total(kura_mac::physical_memory())`（min(物理メモリ/4, 4 GiB)。分からなければ 2 GiB）。エンジンはスレッドプールを作らず、`Engine` には `preview`・`bulk` の閉包の中からだけ触れる（値はプール・スレッドの数によらない。NFR-16）。
- **予算の確保の口**（どれもプールの外で呼ぶ。待つことのある `admit_bulk` と描く前の `admit_preview` は、プールの中から呼ぶと debug のビルドで止める。`open_cache` は待たず、確かめていない）：一括は `admit_bulk`（見積もって全体を超えれば何も確保せずに Limit、収まれば確保できるまで待つ。中止できる。エディタの取り置きとキャッシュの確保のもとで通りえない一括は駐車して、後ろの収まる一括を塞がない）で確保し、返したものを `bulk` で描き・書き終えるまで持つ。エディタは開くときに `open_cache`（キャッシュの上限の分を `CacheLease` で確保。デコードした元の値は `NodeCache::register_external` で数え、替えるときは `unregister_external` で外す）、描くたびに `admit_preview`（待たない。足りなければキャッシュの固定していないものを追い出し、それでも足りなければ Limit。Limit のときはキャッシュの上限と確保を戻す）、描いたあとに `CacheLease::settle`（戻した確保を、手の合間も持ち続ける対話として予算に申告する。駐車の条件に入る）。見積もりの並列の数は、描くプールのスレッドの数。エディタを開いている間の取り置き（`MemoryBudget::set_reserved`。既定は `default_reserved`＝min(1 GiB, 予算/2)）は、M12 のエディタ（`edit::editor`）が開く・閉じるときに置く・外す。エディタを開くときの元の読みも、読む段の見積もりを対話で待たずに確保する（足りなければ `Busy`・`TooLarge`）。編集の値とキャッシュは NFR-17 の予算で数え、NFR-07 には数えない（エディタを閉じたら捨てる）。
- **表示の経路**（段 10。docs/editing.md §12.8）：エディタは 1 手ごとに `Engine::render_view` を呼ぶ。前回の表示（`View`）から変わりうる範囲を推論し（`delta`）、表示する範囲（ビューポート）を、前回の表示の値に変わりうるタイルだけを描き直して継ぎはぎし（`render_area`）、変わったタイルの一覧を返す（`refine`）。値は表示する範囲のタイルで `render` とビット単位で同じで、配信にだけ使う（保存・一括・照合は `render`・`render_cached`）。配信に要るもの（形式・パレット・メタデータ・フレームの表示時間と繰り返し）は、表示の値（`PartialValue`）の読むだけの口で取る（音声は `exact()`）。M12 のフロントはビューポートを渡し、変わったタイルだけを kura:// で取り直す。
- **保存と版**（M12 の段 2。正本は docs/editing.md §12.12・§14、表は docs/data-model.md §4「保存と版（M12）」）：kura-core の `edit::save`（保存のジョブ `EditHandler`・積む口 `start_save`。上書き・別名で保存・版に戻す（`save/revert.rs`）。§5）、`edit::store`（文書を内容アドレスの部品 `edit_objects`・`edit_edges` で置く・読む）、`edit::versions`（版の行・入力・不変条件 `check_invariants`）、`edit::anchors`（アンカーの行）、`edit::drafts`（下書き：頭の文書と土台をアイテムごとに 1 つ）、`edit::gc`（M12 の段 3：GC のジョブ `GcHandler`・時計 `GcClock`・候補 `due`・文書の部品の片付け `sweep_parts`。照合は `save::verify_version`。§5）。台帳の予告と照合は `library::ledger`、重複の判定は `import::store::find_owner`。段 4 のエディタのセッションもここに足す。
- **エディタのセッション**（M12 の段 4。正本は docs/editing.md §12.12・§12.13）：kura-core の `edit::editor`（`open`・`step`・`view`・`sample`・`save`・`close`・`discard_draft`・`version_list`、配信の `tile::serve`）。開いているライブラリに 1 つで `EngineServices` が持ち（`current_editor`・`editor_for`・`close_editor`）、kura:// の `/edit` もそこから引く。中身は `EditSession`・ノードのキャッシュと確保（`CacheLease`）・入力・版ごとの計画の LRU・前回の表示・表示した今と直前の表示（配信用）・下書きのスレッド（`kura-edit-draft`）。IPC（§3 の `edit_*`）は src-tauri の `commands/edit.rs`（DTO もそこ）で、どれも `spawn_blocking`（ジョブにしない。保存だけが保存のジョブを積む）。閉じるのは `edit_close`・`LibrarySession::close`（ライブラリを閉じる・切り替える・終了）・`EngineServices` の drop で、どれも同じ（中止 → キャッシュを空に → 確保を落とす → 取り置きを 0）。
- **ベンチ**：`kura-devtools bench edit`（§13。NFR-11〜17・E30）は同じ `EngineRuntime` で、アプリと同じ順（プレビューのプールでセッションの手を当てて計画（写真は段の計画）を作る → プールの外で `admit_preview` → プレビューのプールで表示の経路（`render_view`）→ `settle`）に回す。NFR-16 の照合は、同じエディタの経路で等倍の全体を描いた（`render_cached`）ダイジェストで行う。見積もりの倍率が実装の変更で足りなくなるのは、`mem_smoke`（§13）が macOS の CI で捕まえる。
- **CI**：`.github/workflows/ci.yml` で ubuntu と macOS のテストを回す（ADR-142）。
- 実装の進み具合は docs/roadmap.md の M10 を見る。実測は §13 に足す。
