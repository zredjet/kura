/**
 * 計測用のスクロール（`VITE_KURA_BENCH=1` で起動・ビルドしたときだけ動く）。
 * 一覧が出てから少し待ち、一定の速さで下までスクロールしてフレーム間隔を集め、結果を console.warn で出す
 * （開発サーバーがターミナルに転送する）。開発ビルドなので本番より厳しめの値になる。
 */

import { commands } from '../../ipc/bindings';

const WAIT_MS = 2000;
const DURATION_MS = 10_000;
const PX_PER_FRAME = 40;

export function runScrollBench(el: HTMLElement, rows: number): () => void {
  let cancelled = false;
  const timer = setTimeout(() => {
    const frames: number[] = [];
    let last = performance.now();
    const started = last;
    const step = (now: number) => {
      if (cancelled) {
        return;
      }
      frames.push(now - last);
      last = now;
      el.scrollTop += PX_PER_FRAME;
      if (now - started < DURATION_MS && el.scrollTop + el.clientHeight < el.scrollHeight) {
        requestAnimationFrame(step);
      } else {
        report(frames, rows, el.scrollTop);
      }
    };
    requestAnimationFrame(step);
  }, WAIT_MS);
  return () => {
    cancelled = true;
    clearTimeout(timer);
  };
}

function report(frames: number[], rows: number, scrolled: number) {
  const sorted = [...frames.slice(1)].sort((a, b) => a - b);
  const pick = (p: number) =>
    sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))] ?? 0;
  const total = frames.slice(1).reduce((a, b) => a + b, 0);
  const result = {
    frames: sorted.length,
    rows,
    scrolledPx: Math.round(scrolled),
    fps: Math.round((sorted.length / total) * 1000 * 10) / 10,
    p50ms: Math.round(pick(0.5) * 10) / 10,
    p95ms: Math.round(pick(0.95) * 10) / 10,
    p99ms: Math.round(pick(0.99) * 10) / 10,
    over20ms: sorted.filter((f) => f > 20).length,
    since_navigation_ms: Math.round(performance.now()),
  };
  const json = JSON.stringify(result);
  console.warn(`[bench] scroll ${json}`);
  // リリースビルドでは開発サーバーがないので、Rust のログにも残す
  void commands.benchReport('scroll', json);
}
