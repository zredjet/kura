# Kura（仮称）— Mac 用メディアライブラリ

画像・音楽・動画をライブラリへコピーして管理し、仮想フォルダ・タグ・ユニット（管理単位）で整理し、
種類を混ぜたプレイリストで再生する個人用 macOS アプリ。Tauri 2（Rust）＋ React / TypeScript。

## 最優先の原則

1. **性能**：10 万件のライブラリで、起動から操作可能まで 1.5 秒以内、グリッドのスクロール 60fps。重い処理はすべて Rust 側で行う。
2. **元ファイルを壊さない**：取り込みは常にコピー（同一ボリュームでは APFS クローン）。取り込み元の移動・変更・削除は一切しない。
3. **デザイン**：`docs/design.md` のトークンと部品だけで組む。UI ライブラリや CSS フレームワークは追加しない。
4. **仕様は docs が正**：実装と docs が食い違ったら、勝手に仕様を変えない。質問するか、docs の更新案を出す。

## 技術スタック

- アプリ：Tauri 2.x（最新安定版）、macOS 専用（Apple Silicon、macOS 26 以降を想定）
- Rust：cargo workspace。rusqlite（bundled, WAL）、tokio、rayon、blake3、reflink-copy、thiserror、tracing、rand_chacha
- macOS ネイティブ：Swift ヘルパー（ImageIO / AVFoundation / WebKit）を swift-rs 経由で呼ぶ（`crates/kura-mac`）
- フロント：React + TypeScript + Vite、状態は Zustand、仮想スクロールは TanStack Virtual、スタイルは CSS Modules + CSS 変数
- 型付き IPC：tauri-specta（デバッグ起動時に `src/ipc/bindings.ts` を自動出力）
- 外部ツール（同梱しない）：yt-dlp、gallery-dl、deno、ffmpeg / ffprobe、Google Chrome（キャプチャ用）、mpv（任意）

## リポジトリ構成

```
Cargo.toml                 # workspace
crates/kura-core/          # OS 非依存のドメインロジック（DB, 取り込み, 検索, ユニット, プレイリスト, ジョブ）
crates/kura-core/migrations/  # SQL マイグレーション（連番、既存は編集禁止）
crates/kura-mac/           # macOS 専用：サムネイル, メディア情報, スナップショット（Swift ヘルパー）
crates/kura-devtools/      # ダミーライブラリ生成・ベンチマーク
src-tauri/                 # Tauri アプリ本体：commands/, protocol/（kura://）, events, window
src/                       # React フロント：features/, components/, styles/, ipc/, stores/
docs/                      # 仕様・設計（下記）
```

## コマンド

| 目的 | コマンド |
|---|---|
| 開発起動 | `pnpm tauri dev` |
| 全チェック | `/verify` スキル |
| Rust テスト | `cargo test --workspace` |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| 整形 | `cargo fmt --all` と `pnpm run format` |
| 型チェック | `pnpm run typecheck` |
| フロントのテスト | `pnpm test` |
| 依存のライセンス一覧 | `pnpm run licenses`（依存を変えたら実行。`THIRD_PARTY_LICENSES.md` を作り直す。要 cargo-about） |
| ダミーデータ生成 | `cargo run -p kura-devtools -- seed --items 100000 --out /tmp/bench.kuralib` |
| リリースビルド | `pnpm tauri build` |

## 実装ルール（全体）

- **IPC**：コマンドは `src-tauri/src/commands/` に薄く書き、ロジックは `kura-core` に置く。戻り値は `Result<T, AppError>`。
- **メディアを IPC で送らない**：サムネイル・本体・再生用プロキシは `kura://localhost/...`（カスタムプロトコル、Range 対応）で配信する。base64 化は禁止。
- **一覧は ID 先取り**：検索結果は ID 列だけを先に返し、画面に見えている分の詳細だけを取得する。全件の詳細を JS に載せない。
- **長い処理はジョブ**：取り込み・サムネイル・プロキシ生成・ダウンロード・キャプチャは `jobs` テーブル経由。進捗は Channel / イベントで送る。
- **ブロックしない**：SQLite・ハッシュ計算・ファイル I/O は専用スレッドか `spawn_blocking`。DB 書き込みは単一ライター。
- **DB**：スキーマ変更は新しいマイグレーションの追加のみ。`docs/data-model.md` も同時に更新する。
- **OS 依存の分離**：macOS 専用コードは `kura-mac` に閉じ込める。`cargo test -p kura-core` は Linux でも通ること。
- **外部ツールのパス**：GUI アプリはシェルの PATH を引き継がない。`/opt/homebrew/bin`・`/usr/local/bin` を探し、設定で上書き可能にする。
- **乱数**：シャッフル等は必ずシード付き ChaCha8（`docs/playlists.md`）。`thread_rng` や `StdRng` は使わない。

## デザインルール

- 色・余白・角丸・文字サイズは `src/styles/tokens.css` の変数のみ（直書きの色コード禁止）。
- ガラス表現は浮いている部品（サイドバー、インスペクタ、再生バー、ダイアログ）だけ。コンテンツ領域には使わない。
- ボタンは `<button>`、リンクは `<a>`、入力は `<input>` + `<label>`。アイコンだけのボタンには `aria-label`。
- アイコンはインライン SVG（stroke 1.8〜2.0、`currentColor`）。絵文字は使わない。
- UI 文言は日本語。文字列は `src/i18n/ja.ts` に集約する。

## 作業の進め方

- 作業は `docs/roadmap.md` のマイルストーン単位で進める。着手前に該当マイルストーンと関連 docs を読み、計画を提示してから実装する。
- 新しいロジックには必ずテストを書く。検索・ユニット・プレイリスト・取り込みは純粋関数に分けてテストする。
- 終える前に `/verify` を実行し、すべて通るまで完了としない。
- UI を変更したら `pnpm tauri dev` で表示を確認する（できない環境ならその旨を報告する）。
- 仕様判断が必要な点は推測で実装せず質問する。決まったことは `docs/decisions.md` に追記する。
- コミットは小さく、`feat(playlist): ユニット単位のシャッフルを追加` の形式で書く。

## ドキュメント（必要なときに読む）

- `docs/requirements.md` — 機能要件・非機能要件・対象外
- `docs/architecture.md` — 構成、IPC コマンド、kura:// プロトコル、ジョブ、外部ツール、キャプチャ
- `docs/data-model.md` — SQLite スキーマ、インデックス、クエリ方針
- `docs/playlists.md` — **ユニット単位／ファイル単位**、シャッフル、再生仕様（プレイリスト・再生に触るときは必読）
- `docs/search-query.md` — 検索クエリ言語と ⌘K パレット
- `docs/design.md`、`docs/design/tokens.css`、`docs/design/mockups/` — デザイン
- `docs/roadmap.md` — マイルストーンと完了条件
- `docs/decisions.md` — 設計判断の記録

## 用語

- **アイテム**：ライブラリ内の 1 ファイル（`items`）
- **ユニット（管理単位）**：アイテムのまとまり。`set`（ファイル群）と `folder`（フォルダ群）の 2 種類
- **仮想フォルダ**：ライブラリ内の論理フォルダ。実ファイルの置き場所とは無関係
- **単位（granularity）**：プレイリストの扱い方。`file`（ファイル単位）または `unit`（ユニット単位）
- **ライブラリ**：`*.kuralib` フォルダ。DB・実ファイル・サムネイル・プロキシを含む

## やってはいけないこと

- 取り込み元ファイルの変更・移動・削除
- `files/` 配下を DB を通さずに削除・改名
- メディアの base64 化（`data:` URL を含む）
- `localStorage` へのライブラリデータ保存（表示設定などの軽いものは可）
- UI ライブラリ・CSS フレームワークの追加
- 適用済みマイグレーションの編集
- 本番コードでの `unwrap()` / `expect()`（テストと起動時の自明な不変条件を除く）
