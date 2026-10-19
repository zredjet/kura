/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** 開発時のスクロールの計測（`1` で有効）。 */
  readonly VITE_KURA_BENCH?: string;
  /** kura:// と CSP の自己点検（`1` で有効。`src/dev/selftest.ts`）。 */
  readonly VITE_KURA_SELFTEST?: string;
  /** 自己点検の F2 で、アプリ内ブラウザに開くページの URL（任意）。 */
  readonly VITE_KURA_SELFTEST_F2?: string;
}
