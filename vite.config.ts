import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

// Tauri 向けの設定（https://v2.tauri.app/start/frontend/vite/）
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    // 1420（Tauri の既定）は他のプロジェクトとぶつかりやすいので変えている
    port: 1430,
    strictPort: true,
    watch: { ignored: ['**/src-tauri/**', '**/crates/**', '**/target/**'] },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    // macOS 26 の WKWebView（Safari 26 相当）
    target: 'safari18',
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    // CSS を実際に処理する（CSS Modules の `?raw` を文字列として読むため。styles.test.ts）
    css: true,
  },
});
