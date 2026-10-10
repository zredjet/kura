---
paths:
  - "src/**/*.{ts,tsx,css}"
---

# フロントエンドのルール

- 色・余白・角丸・文字サイズは `src/styles/tokens.css` の CSS 変数だけを使う。色コードの直書き禁止。
- コンポーネントは `src/components/`（汎用部品）と `src/features/<機能>/`（画面）に分ける。
- IPC は `src/ipc/bindings.ts`（自動生成、手で編集しない）を `src/ipc/` のラッパー経由で呼ぶ。
- 画像・動画・音声の URL は `src/ipc/media.ts` の `thumbUrl()` / `playUrl()` で組み立てる（`kura://localhost/...`）。
- 一覧・グリッド・キューは必ず仮想スクロール。1 万件を DOM に並べない。
- 検索結果は ID 配列（`Int32Array` など）で保持し、詳細は見えている範囲だけ取得してキャッシュする。
- ドメインデータを `localStorage` に置かない（サイドバーの開閉など見た目の設定だけ可）。
- インタラクティブ要素は `<button>` / `<a>` / `<input>`。`div` に `onClick` を付けない。アイコンだけのボタンには `aria-label`。
- アニメーションは 120〜200ms の ease-out。`prefers-reduced-motion` のときは切る。
- UI 文言は `src/i18n/ja.ts` から参照する。
- テストは Vitest + Testing Library。ロジック（キュー操作、検索トークン化）はフックや純粋関数に切り出してテストする。
