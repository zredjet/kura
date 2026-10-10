---
name: add-command
description: 新しい Tauri コマンド（フロントから呼ぶ Rust 関数）を、ロジック・テスト・型定義・フロントのラッパーまで一通り追加する手順。IPC を増やすときに使う。
---

コマンド名は `$ARGUMENTS`（例：`playlist_set_granularity`）。名前は `<対象>_<動詞>` のスネークケースにする。

1. **ロジック**：`crates/kura-core/src/<領域>/` に純粋関数またはサービス関数を書き、`#[cfg(test)]` でテストを書く。
2. **コマンド**：`src-tauri/src/commands/<領域>.rs` に薄いラッパーを書く。
   - 署名は `#[tauri::command] #[specta::specta] async fn name(state: State<'_, AppState>, ...) -> Result<T, AppError>`
   - 重い処理は `spawn_blocking`。30 秒以上かかりうる処理はジョブにして `JobId` を返す。
   - 引数・戻り値の型には `serde` と `specta::Type` を derive する。
3. **登録**：`src-tauri/src/lib.rs` の `collect_commands![]` に追加する。
4. **権限**：必要なら `src-tauri/capabilities/default.json` を更新する。
5. **型定義**：`cargo test -p kura-app` で `src/ipc/bindings.ts` を再生成する（`pnpm tauri dev` の起動時にも再生成される）。生成されたファイルは手で編集しない。
6. **フロント**：`src/ipc/<領域>.ts` にラッパーを追加し、エラーは `AppError.code` で分岐する。
7. **ドキュメント**：`docs/architecture.md` のコマンド一覧に 1 行追加する。
8. `/verify` を実行する。
