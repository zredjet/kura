# ロードマップ

マイルストーンは上から順に進めます。1 マイルストーン＝Claude Code の 1〜3 セッションが目安です。
各マイルストーンの「依頼文」をそのまま Claude Code に貼れば始められます。完了条件をすべて満たしたら、このファイルのチェックを付けてコミットします。

共通の進め方：

1. 依頼文を貼る → Claude Code が関連 docs を読み、計画を出す（プランモード推奨）
2. 計画を確認して承認 → 実装
3. `/verify` がすべて通る → 完了条件を 1 つずつ確認した結果を報告してもらう
4. 自分で `pnpm tauri dev` を起動して触る → 気になる点を伝えて直す
5. コミット

---

## M0　土台づくり

- [x] cargo workspace（`kura-core` / `kura-mac` / `kura-devtools` / `src-tauri`）と pnpm ＋ Vite ＋ React ＋ TypeScript
- [x] Tauri 2 のウィンドウ設定（オーバーレイのタイトルバー、信号機の位置、最小サイズ）
- [x] `src/styles/tokens.css`（`docs/design/tokens.css` をコピー）とリセット CSS、`src/i18n/ja.ts`
- [x] 3 ペインの空の骨組み（サイドバー・ツールバー・グリッド領域・インスペクタ）をモックどおりの寸法で
- [x] tauri-specta による型生成、`AppError`、ログ（tracing）
- [x] package.json のスクリプト：`format` / `format:check` / `lint` / `typecheck` / `test`
- [x] Swift ヘルパーの雛形（`kura_reduce_transparency` を 1 つだけ実装して呼べることを確認）

**完了条件**：`pnpm tauri dev` で空の 3 ペインがモックと同じ配置で表示される／`/verify` が通る／`cargo test -p kura-core` が macOS 以外でも通る構成になっている

**依頼文**
> CLAUDE.md、docs/architecture.md、docs/design.md を読んで、docs/roadmap.md の M0 を実装してください。最初に計画（作るファイルと手順）を出して、私の承認を待ってください。ウィンドウとサイドバーの寸法は docs/design/mockups/Main.dc.html と Sidebar.dc.html の値に合わせてください。

---

## M1　ライブラリと取り込み

- [x] マイグレーション `0001_init.sql`（`docs/data-model.md` のとおり）と DB 接続（単一ライター＋読み取りプール）
- [x] ライブラリの作成・オープン（初回起動で場所を選ぶ）
- [x] ジョブ基盤（`jobs` テーブル、同時実行数、キャンセル、`job://progress`）
- [x] 取り込み：`import_preview` と `import_start`（走査、BLAKE3、重複判定、reflink、原子的な配置、バッチ挿入）
- [x] ドラッグ＆ドロップの重ね表示と取り込みシート（`Import.dc.html`）
- [x] 起動時の整合性チェック（staging の掃除、不整合の件数）

**完了条件**：1,000 ファイル（重複 50 件を含む）を落として、重複はスキップ、残りがコピーされ、進捗が出る／途中でアプリを強制終了しても次回起動で壊れていない／取り込み元のファイルが変化していない（テストでハッシュ比較）

**依頼文**
> docs/roadmap.md の M1 を進めてください。docs/data-model.md と docs/architecture.md §5・§6 が仕様です。取り込みのコアは kura-core に純粋に近い形で書き、テストを先に用意してください。

---

## M2　サムネイルとグリッド

- [x] `MediaBackend` trait と `kura-mac` の実装（ImageIO / AVFoundation）、ffmpeg へのフォールバック
- [x] サムネイル生成ジョブ（s / l の 2 サイズ）、音楽のジャケット
- [x] `kura://` プロトコル（thumb / art / file / play、Range 対応）
- [x] `items_query_ids`（ID と縦横比の生バイト）と `items_get`
- [x] ジャスティファイ配置の計算（純粋関数＋テスト）と仮想スクロールのグリッド、日付見出し
- [x] インスペクタ（表示のみ）、クイックルック
- [x] `kura-devtools seed`（10 万件のダミーライブラリ）

**完了条件**：10 万件のダミーで起動 1.5 秒以内・スクロール 60fps（Instruments か Safari の Web インスペクタで確認）／HEIC・MOV・FLAC のサムネイルが出る／動画のシークが効く（Range）

**依頼文**
> docs/roadmap.md の M2 を進めてください。性能目標は docs/requirements.md の NFR-01〜04 です。最初に kura-devtools seed を作り、ベンチマークで数値を出しながら進めてください。

---

## M3　整理（フォルダ・タグ・ユニット）

- [x] サイドバー（`Sidebar.dc.html`）：ライブラリ項目、スマートフォルダ、フォルダの木（作成・改名・移動・色・ドラッグで所属）（スマートフォルダの欄は M4 で出す。ADR-41）
- [x] タグ：名前空間・別名・親子、候補付き入力、一括付け外し
- [x] 評価・タイトル・メモの編集（複数選択対応）
- [x] ユニット：⌘G で set ユニット、フォルダから folder ユニット、編集・解除、インスペクタ、グリッドでの「まとめて表示」（`docs/playlists.md` §2、`docs/design.md` §3.1）
- [x] ゴミ箱

**完了条件**：フォルダ・タグ・ユニットの操作がキーボードだけでもできる／1 つのアイテムが複数フォルダ・複数ユニットに入る／ユニットのメンバー解決（§2.2）のテストが通る

**依頼文**
> docs/roadmap.md の M3 を進めてください。ユニットの仕様は docs/playlists.md §2 です。ユニットの UI はモックにないので、docs/design.md §3.1 に沿って作り、作る前に構成を文章で提案してください。

---

## M4　検索

- [x] 検索言語：字句解析・構文解析・AST・SQL 生成（`docs/search-query.md`）
- [x] FTS5 trigram とトリガー（テーブルとトリガーは M1 の `0001_init.sql` で作成済み。M4 では検索での利用）
- [x] ⌘K パレット（`Search.dc.html`）、フィルタバーのチップ（色のチップは M9 で検討。ADR-56）
- [x] スマートフォルダの保存・表示

**完了条件**：`docs/search-query.md` §7 のテストが通る／10 万件でタグ 3 つの AND が 50ms 以内

**依頼文**
> docs/roadmap.md の M4 を進めてください。仕様は docs/search-query.md です。パーサーはテスト駆動で、§1 の例をすべてテストケースにしてから実装してください。

---

## M5　再生

- [ ] `canPlayType` による再生可否の表、`proxy_state` の判定、プロキシ生成ジョブ（remux / VideoToolbox）
- [ ] `PlaybackController`（video / audio×2 / image、先読み）
- [ ] ミニプレーヤー（`Main.dc.html`）とシアター（`Player.dc.html`）
- [ ] 画像の間の BGM（`docs/playlists.md` §6.2）
- [ ] 再生イベントの記録（§6.4）

**完了条件**：MKV・WebM・OGG を含む混在キューが途切れずに再生できる／BGM のフェードが仕様どおり／再生済みの判定がテストで確認できる

**依頼文**
> docs/roadmap.md の M5 を進めてください。仕様は docs/architecture.md §8 と docs/playlists.md §6 です。この段階では、ライブラリの選択範囲をそのままキューにして再生できれば十分です（プレイリストは M6）。

---

## M6　プレイリスト（ファイル単位／ユニット単位）

- [ ] `resolve` と `build_queue`（純粋関数）、シャッフル（ChaCha8、重み付き、連続制限、ユニット内シャッフル）
- [ ] 受け入れテスト T1〜T15（`docs/playlists.md` §8）
- [ ] 手動プレイリスト：作成、追加（§3.5 の表、⌥ で反転）、並べ替え、削除、単位の切り替え、ファイルに固定
- [ ] スマートプレイリスト（`Playlist.dc.html`）：ルール編集、ファイル単位とユニット単位の評価
- [ ] シアターのユニット表示（「ユニット 3 / 12 · 2 / 10」、キューの折りたたみ、⇧→）
- [ ] 再開（`playlist_state`）、⌘K の ⇧↵（結果をランダム再生）

**完了条件**：T1〜T15 がすべて通る／同じプレイリストをファイル単位とユニット単位で切り替えても内容が失われない／シード固定で同じ順に再生される

**依頼文**
> docs/roadmap.md の M6 を進めてください。docs/playlists.md が正本です。まず §8 の受け入れテストをすべて書いて失敗することを確認し、そのあと kura-core::playlist を実装してください。UI はユニット単位の表示がモックにないので、docs/design.md §3.1 に沿って作ってください。

---

## M7　ダウンロード

- [ ] 外部ツールの検出・状態表示・更新案内（`Downloads.dc.html` の右下）
- [ ] URL の判定と規則、yt-dlp / gallery-dl のアダプタ（進捗の解析、ログ、キャンセル）
- [ ] メタデータ → タグ・タイトル・URL・投稿日、複数ファイルの投稿 → ユニット、`remote_sources`
- [ ] ダウンロード画面（キュー・タブ・再試行）

**完了条件**：YouTube の動画 1 本、pixiv の複数ページ作品 1 件を取得して、タグとユニットが付いた状態でライブラリに入る／ツールがないときに導入方法が表示される

**依頼文**
> docs/roadmap.md の M7 を進めてください。仕様は docs/architecture.md §9 です。yt-dlp と gallery-dl の引数は、実装前に手元の `--help` で確認してから決めてください。

---

## M8　キャプチャ

- [ ] chromiumoxide によるルール方式のキャプチャ（element / full_page / viewport、非表示、スクロール）
- [ ] ログイン用にブラウザを開く機能
- [ ] アプリ内ブラウザと「見たまま」キャプチャ
- [ ] キャプチャのルール編集 UI

**完了条件**：サンプルのルールで要素だけ・ページ全体の撮影ができ、タグ `キャプチャ` と `サイト:<ホスト>` 付きで取り込まれる

**依頼文**
> docs/roadmap.md の M8 を進めてください。仕様は docs/architecture.md §10 です。

---

## M9　仕上げ

- [ ] キーボードショートカット（`docs/design.md` §6）とメニューバー
- [ ] 「透明度を下げる」「視差効果を減らす」への対応、VoiceOver の確認
- [ ] 書き出し、DB の定期バックアップ
- [ ] 設定画面、空の状態
- [ ] 性能の再測定（NFR-01〜07）と改善
- [ ] リリースビルド（アドホック署名）
- [ ] （試験）ネイティブの Liquid Glass をサイドバーに使う案の検証（ADR-12）

**依頼文**
> docs/roadmap.md の M9 を進めてください。まず NFR-01〜07 を測定して表にし、目標に届いていないものから直してください。
