# Kura 開発キット

Claude Code で Kura（仮称）を開発するための初期ファイル一式です。新しいリポジトリの直下にこのまま置いて使います。

## 中身

```text
CLAUDE.md                     # Claude Code が毎回読むプロジェクトの指示（200 行未満）
.claude/
  settings.json               # 許可するコマンド・確認するコマンド・禁止するコマンド
  rules/                      # ファイルの種類ごとに自動で読み込まれるルール
    rust.md  frontend.md  database.md  playlist.md
  skills/                     # /コマンドで呼べる手順
    verify/        # /verify：整形・lint・型・テストを一括実行
    add-command/   # /add-command <名前>：Tauri コマンドを一通り追加
    add-migration/ # /add-migration <内容>：DB スキーマ変更
docs/
  requirements.md             # 機能要件・非機能要件・対象外
  architecture.md             # 構成・IPC・kura:// ・ジョブ・取り込み・再生・DL・キャプチャ
  data-model.md               # SQLite スキーマ
  playlists.md                # ユニット（管理単位）とプレイリストの仕様、受け入れテスト
  search-query.md             # 検索言語と ⌘K パレット
  design.md                   # デザイン方針・部品・キーボード・アクセシビリティ
  design/tokens.css           # デザイントークン（src/styles/ にコピーして使う）
  design/mockups/*.dc.html    # 画面ごとのモック（値の参照用）
  decisions.md                # 設計判断の記録
  roadmap.md                  # マイルストーン M0〜M9、完了条件、Claude Code への依頼文
```

## 事前に入れておくもの

| もの | 入れ方 | 用途 |
|---|---|---|
| Xcode | App Store | Swift ヘルパーのビルド、Instruments での性能計測 |
| Rust | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` | Tauri・コア |
| Node.js（LTS）と pnpm | 公式インストーラか Homebrew、`corepack enable` | フロント |
| 外部ツール | `brew install yt-dlp gallery-dl deno ffmpeg` | ダウンロード・プロキシ・メディア情報 |
| Google Chrome | 公式サイト | キャプチャ（M8） |
| mpv（任意） | `brew install mpv` | 「外部プレイヤーで開く」 |
| Claude Code | 公式の手順 | 開発 |

## 始め方

1. 空のフォルダを作って `git init` する。
2. このキットの中身（`CLAUDE.md`、`.claude/`、`docs/`、この README）をフォルダの直下に置いて最初のコミットをする。
3. そのフォルダで `claude` を起動する（フォルダを信頼するか聞かれたら許可。許可しないと `.claude/settings.json` の allow が効かない）。
4. `docs/roadmap.md` の **M0 の依頼文**を貼る。計画が出たら確認して承認する。
5. 以降、M1、M2…と順に進める。

## 進めるときのコツ

- **1 マイルストーン＝新しいセッション**。前の話題を引きずらないよう、区切りで `/clear` するか新しく起動する。
- **計画を先に**。大きめの変更はプランモードで計画を出させ、ファイル構成と手順を確認してから実装させる。
- **仕様を変えたくなったら docs から**。「docs/playlists.md の §3.5 をこう変えたい。docs を更新してから実装して」のように頼むと、実装と仕様がずれない。
- **完了の判断は /verify と完了条件**。「できました」だけで終わらせず、完了条件を 1 つずつ確認した結果を出してもらう。
- **画面は自分で触る**。`pnpm tauri dev` を起動して見た目と操作感を確かめ、気になった点をそのまま伝える。モックと比べるときは、このチャットのデザインキャンバスを横に開く。
- **指示ファイルの点検**。CLAUDE.md や rules が増えてきたら、Claude Code の `/doctor prompt-audit` で矛盾や古い記述を点検できる。

## まだ決めていないこと（必要になったら決める）

- アプリ名（「Kura」は仮称）。バンドル ID は `dev.kura.Kura` に決めた（`docs/decisions.md` ADR-18）
- ライトモードの有無（v1 はダークのみ）
- ユニットの UI 上の呼び名（「ユニット」で仮置き。「まとまり」なども可）
