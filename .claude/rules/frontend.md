---
paths:
  - "src/**/*.{ts,tsx,css}"
---

# フロントエンドのルール

- 色・余白・角丸・文字サイズは `src/styles/tokens.css` の CSS 変数だけを使う。色コードの直書き禁止。
- コンポーネントは `src/components/`（汎用部品）と `src/features/<機能>/`（画面）に分ける。
- IPC は `src/ipc/bindings.ts`（自動生成、手で編集しない）を `src/ipc/` のラッパー経由で呼ぶ。
- アイテムの種類（`items.kind`）の型・並び・述語は `src/ipc/kinds.ts` だけから使う（bindings の定数 `ITEM_KINDS` を読む。`'image' | 'video' | 'audio'` のような型や種類の並びを画面ごとに書かない。並びは 画像・動画・音楽・フォント・3D）。検索のフィールド名・種類の語・記号の表も bindings の定数（`QUERY_FIELD_NAMES`・`QUERY_KIND_WORDS`・`QUERY_SYMBOLS`）を読み、手で写さない（M11）。
- 画像・動画・音声とその派生（サムネイル・ジャケット・波形・試聴の PCM・アニメの帯）の URL は `src/ipc/media.ts` の関数だけで組み立てる（`kura://localhost/...`。`kura://` の文字列をほかに書かない）。
  - `thumbUrl(hash, size, rev)`：`/thumb/{hash}/{s|l}`。`rev`（`thumbRev`）が 0 より大きいときだけ `?r={rev}` を付ける（作り直したサムネイルを WebKit のメモリのキャッシュから出さないため。`rev` は省けない）。呼ぶのは `src/components/MediaImage` だけ（市松模様・ドット絵の最近傍・フォントの mask をそこで当てる）。サムネイルを出す所はすべて `MediaImage` を通す。
  - サムネイルがあるかは `hasThumb(thumbState)`（`ready` か、作り直すまで今の絵を出す `stale`）。`thumbState === 'ready'` で判定しない。
  - `artUrl(hash)`・`fileUrl(hash)`・`playUrl(hash)`：ジャケット・元のファイル・再生用（Range 対応）。
  - `peaksUrl(hash)`：`/peaks/{hash}`（波形の段 `kura-peaks`。immutable）。`fetch` して `src/lib/asset/peaks.ts` の `parsePeaks` で読む。
  - `pcmUrl(hash, start, end)`：`/pcm/{hash}/{start}-{end}`（試聴の PCM。フレームの半開区間で、0 以上の整数・`start < end`。キャッシュしない）。`fetch` して `src/lib/asset/wav.ts` の `readPcmWav` で読む（`decodeAudioData` は使わない。文脈のレートに直されてループ点が標本で合わなくなる）。
  - `animUrl(hash, rev)`：`/anim/{hash}/1`（アニメの帯の PNG。immutable。帯の並べ方を変えたら番号を上げる）。`rev` は `item_anim_get` の `AnimDto.rev`（格子の帯は格子ごとに変わる）で、0 より大きいときだけ `?r={rev}`。
  - `media.test.ts` が、`kura://` の組み立て・`thumbUrl` の呼び出し・`thumbState` の `'ready'` だけの判定がほかのファイルにないことを見張る（注釈は除く。自己点検の `src/dev/selftest.ts` は壊れた URL をわざと組むので外す）。
- ループの試聴は `src/lib/assetAudio.ts` の `Audition` を通す。`AudioBufferSourceNode` の `loopStart`・`loopEnd` を使わない（秒に直すと標本がずれる。本体だけのバッファを `loop` で回す）。文脈は `ContextPool` から元のレートで取る。
  - 画面からは `src/features/asset/auditionStore.ts`（始める・止める・L のキー。プレーヤーを一時停止する）を通し、文脈の持ち主は `src/features/asset/audioRuntime.ts`（試聴と効果音で分け合い、ライブラリの画面を離れるときに閉じる）。長さ（フレーム）は DB にないので `Audition.startProbed`（/pcm の先頭の 1 フレームの応答で長さ・ループ点を知る）で始める。
- 一覧・グリッド・キューは必ず仮想スクロール。1 万件を DOM に並べない。
- 検索結果は ID 配列（`Int32Array` など）で保持し、詳細は見えている範囲だけ取得してキャッシュする。
- ドメインデータを `localStorage` に置かない（サイドバーの開閉など見た目の設定だけ可）。
- インタラクティブ要素は `<button>` / `<a>` / `<input>`。`div` に `onClick` を付けない。アイコンだけのボタンには `aria-label`。
- アニメーション（画面の移り変わり）は 120〜200ms の ease-out。`prefers-reduced-motion` のときは切る。
- 素材のアニメ・スプライトのコマ送りは上の規則の外で、ファイルのコマの長さ（0ms は 100ms）で描く（canvas の `drawImage`。経過時間から `src/lib/asset/timeline.ts` の `frameAt` でコマを決め、タイマーの刻みに頼らない）。ただし `prefers-reduced-motion` のときは自動で再生しない（先頭のコマと再生ボタンを出し、押したときだけ回す）。
- UI 文言は `src/i18n/ja.ts` から参照する。
- テストは Vitest + Testing Library。ロジック（キュー操作、検索トークン化）はフックや純粋関数に切り出してテストする。
