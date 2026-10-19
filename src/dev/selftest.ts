/**
 * kura:// と CSP の自己点検（`VITE_KURA_SELFTEST=1` で起動・ビルドしたときだけ動く。docs/architecture.md §4.2）。
 *
 * `kura-devtools selftest-library` で作ったライブラリを `KURA_LIBRARY` で開き、P1〜P8・F1・F3・F4 を順に試して、
 * 結果を `bench_report('selftest', …)` でログに書く（`pnpm tauri dev` では console.warn でターミナルにも出る）。
 * F2（ほかの WebView）は `VITE_KURA_SELFTEST_F2` に F2 のページの URL があるときだけ、アプリ内ブラウザで開く
 * （判定は Rust のログの `label=capture-browser` の 403 と、そのページを配る http サーバーのアクセスログ）。
 * F4 の main の移動は最後に試し、止まれば `selftest-nav` をもう一度書く（移ってしまえば書かれない）。
 */

import { commands } from '../ipc/bindings';

const K = 'kura://localhost';
/** 目録の決まった名前（kura-devtools の selftest::MANIFEST_HASH と同じ）。 */
export const MANIFEST_HASH = '5e1f7e57'.repeat(8);
/** SVG のスクリプトが動いたときに親へ送る印。 */
const SVG_SCRIPT_RAN = 'kura-selftest-svg-script-ran';

type Fixture = { hash: string; ext?: string };
type Manifest = {
  version: number;
  png: Fixture & { width: number; topLeft: number[] };
  jpg: Fixture & { width: number };
  wav: Fixture & { rate: number; frames: number };
  svg: Fixture;
  glb: Fixture;
  ttf: Fixture | null;
  missing: string;
};

/** 1 つの試しの結果（`ok` が null なら見送り）。 */
export type Check = { ok: boolean | null; detail: unknown };
/** CSP の違反（どの試しの最中に起きたか）。 */
export type Violation = { directive: string; blocked: string; source: string; during: string };

/** 違反が予期したものか（止まるべきものの試し F の最中だけ）。 */
export function isExpected(v: Violation): boolean {
  return /^F\d/.test(v.during);
}

/** 合否：見送り以外の試しがすべて通り、予期しない違反が 0 件。 */
export function judge(checks: Record<string, Check>, violations: Violation[]) {
  const failed = Object.entries(checks)
    .filter(([, c]) => c.ok === false)
    .map(([name]) => name);
  const skipped = Object.entries(checks)
    .filter(([, c]) => c.ok === null)
    .map(([name]) => name);
  const unexpected = violations.filter((v) => !isExpected(v));
  return {
    pass: failed.length === 0 && unexpected.length === 0,
    failed,
    skipped,
    unexpected,
    expected: violations.filter(isExpected),
  };
}

let during = 'early';
const violations: Violation[] = [];

function record(directive: string, blocked: string, source: string, when = during) {
  violations.push({ directive, blocked, source, during: when });
}

/** 違反を集め始める（ReportingObserver の buffered で、読み込む前に起きたものも拾う）。 */
function watchViolations() {
  document.addEventListener('securitypolicyviolation', (e) =>
    record(e.violatedDirective, e.blockedURI, e.sourceFile),
  );
  if (typeof ReportingObserver !== 'undefined') {
    const seen = new Set<string>();
    const observer = new ReportingObserver(
      (reports) => {
        for (const r of reports) {
          const body = r.body as unknown as Record<string, unknown> | null;
          const key = JSON.stringify(body);
          // securitypolicyviolation と同じものは数えない（起動直後の buffered のものだけを足す）
          if (during === 'early' && !seen.has(key)) {
            seen.add(key);
            record(
              String(body?.effectiveDirective ?? body?.violatedDirective ?? '?'),
              String(body?.blockedURL ?? body?.blockedURI ?? ''),
              String(body?.sourceFile ?? ''),
              'early-report',
            );
          }
        }
      },
      { types: ['csp-violation'], buffered: true },
    );
    observer.observe();
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function withTimeout<T>(p: Promise<T>, ms = 5000): Promise<T> {
  return Promise.race([
    p,
    new Promise<T>((_, reject) => setTimeout(() => reject(new Error('timeout')), ms)),
  ]);
}

const errorText = (e: unknown) => (e instanceof Error ? `${e.name}: ${e.message}` : String(e));

function loadImage(src: string, cors = false): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    if (cors) {
      img.crossOrigin = 'anonymous';
    }
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('img error'));
    img.src = src;
  });
}

async function fetchInfo(url: string, init?: RequestInit) {
  const r = await fetch(url, init);
  const body = new Uint8Array(await r.arrayBuffer());
  return {
    status: r.status,
    len: body.byteLength,
    magic: String.fromCharCode(...body.slice(0, 8)),
    contentRange: r.headers.get('content-range'),
    contentLength: r.headers.get('content-length'),
  };
}

function textWidth(family: string) {
  const g = document.createElement('canvas').getContext('2d');
  if (!g) {
    return 0;
  }
  g.font = `40px "${family}", serif`;
  return g.measureText('iiiiiWWWWW').width;
}

function stage(): HTMLElement {
  const el = document.createElement('div');
  el.setAttribute('aria-hidden', 'true');
  el.style.cssText =
    'position:fixed;left:0;top:0;width:16px;height:16px;opacity:0.01;pointer-events:none;z-index:-1;overflow:hidden';
  document.body.appendChild(el);
  return el;
}

/** 目録を待つ（ライブラリが開くまで 503）。 */
async function waitForManifest(): Promise<Manifest> {
  const deadline = performance.now() + 30_000;
  for (;;) {
    try {
      const r = await fetch(`${K}/file/${MANIFEST_HASH}?p=manifest`);
      if (r.ok) {
        return (await r.json()) as Manifest;
      }
      if (r.status === 404) {
        throw new Error('目録がありません（KURA_LIBRARY が selftest のライブラリではない）');
      }
    } catch (e) {
      if (e instanceof Error && e.message.startsWith('目録')) {
        throw e;
      }
    }
    if (performance.now() > deadline) {
      throw new Error('30 秒待ってもライブラリが開きません');
    }
    await sleep(250);
  }
}

async function run(
  checks: Record<string, Check>,
  name: string,
  fn: () => Promise<Check>,
  ms?: number,
) {
  during = name;
  try {
    checks[name] = await withTimeout(fn(), ms);
  } catch (e) {
    checks[name] = { ok: false, detail: errorText(e) };
  }
  // 違反のイベントは少し遅れて届く
  await sleep(150);
}

async function report(name: string, value: unknown) {
  const json = JSON.stringify(value);
  console.warn(`[selftest] ${name} ${json}`);
  await commands.benchReport(name, json).catch(() => undefined);
}

async function probe(m: Manifest, el: HTMLElement): Promise<Record<string, Check>> {
  const checks: Record<string, Check> = {};
  const file = (f: Fixture, p: string) => `${K}/file/${f.hash}?p=${p}`;

  await run(checks, 'P1', async () => {
    const widths = {
      thumb: (await loadImage(`${K}/thumb/${m.jpg.hash}/s?p=P1`)).naturalWidth,
      png: (await loadImage(file(m.png, 'P1'))).naturalWidth,
      svg: (await loadImage(file(m.svg, 'P1'))).naturalWidth,
    };
    return { ok: Object.values(widths).every((w) => w > 0), detail: widths };
  });

  await run(checks, 'P2', async () => {
    const wav = await fetchInfo(file(m.wav, 'P2'));
    const glb = await fetchInfo(file(m.glb, 'P2'));
    const range = await fetchInfo(file(m.wav, 'P2-range'), { headers: { Range: 'bytes=0-99' } });
    const peaks = await fetchInfo(`${K}/peaks/${m.wav.hash}?p=P2`);
    const peaksOk = (peaks.status === 200 && peaks.magic === 'kurapeak') || peaks.status === 404;
    const ok =
      wav.status === 200 &&
      wav.magic.startsWith('RIFF') &&
      String(wav.len) === wav.contentLength &&
      glb.status === 200 &&
      glb.magic.startsWith('glTF') &&
      range.status === 206 &&
      range.len === 100 &&
      range.contentRange === `bytes 0-99/${wav.len}` &&
      peaksOk;
    return { ok, detail: { wav, glb, range, peaks } };
  });

  await run(checks, 'P3', async () => {
    const missing = await fetchInfo(`${K}/file/${m.missing}?p=P3`);
    const outside = await fetchInfo(`${K}/nope?p=P3`);
    return {
      ok: missing.status === 404 && outside.status === 404,
      detail: { missing: missing.status, outside: outside.status },
    };
  });

  await run(checks, 'P4', async () => {
    if (!m.ttf) {
      return {
        ok: null,
        detail: 'ttf がありません（selftest-library がフォントを見つけられなかった）',
      };
    }
    const base = textWidth('kura-selftest-no-such-family');
    const face = new FontFace('kura-selftest-p4', `url(${file(m.ttf, 'P4')})`);
    await face.load();
    document.fonts.add(face);
    const width = textWidth('kura-selftest-p4');
    return {
      ok: face.status === 'loaded' && width !== base,
      detail: { status: face.status, base, width },
    };
  });

  await run(checks, 'P5', async () => {
    const r = await fetch(file(m.wav, 'P5'));
    const ctx = new OfflineAudioContext(1, 1, m.wav.rate);
    const buffer = await ctx.decodeAudioData(await r.arrayBuffer());
    const audio = document.createElement('audio');
    audio.crossOrigin = 'anonymous';
    audio.preload = 'metadata';
    audio.muted = true;
    const duration = await new Promise<number>((resolve, reject) => {
      audio.onloadedmetadata = () => resolve(audio.duration);
      audio.onerror = () => reject(new Error(`media error ${audio.error?.code ?? '?'}`));
      audio.src = `${K}/play/${m.wav.hash}?p=P5`;
      el.appendChild(audio);
    });
    audio.remove();
    const expected = m.wav.frames / m.wav.rate;
    return {
      ok: buffer.length === m.wav.frames && Math.abs(duration - expected) < 0.02,
      detail: { frames: buffer.length, rate: buffer.sampleRate, duration },
    };
  });

  await run(checks, 'P6', async () => {
    const img = await loadImage(file(m.png, 'P6'), true);
    const canvas = document.createElement('canvas');
    canvas.width = img.naturalWidth;
    canvas.height = img.naturalHeight;
    const g = canvas.getContext('2d');
    if (!g) {
      throw new Error('canvas の 2d がありません');
    }
    g.drawImage(img, 0, 0);
    const px = Array.from(g.getImageData(0, 0, 1, 1).data);
    const ok = px.every((v, i) => Math.abs(v - (m.png.topLeft[i] ?? -99)) <= 2);
    return { ok, detail: { px } };
  });

  await run(checks, 'P7', async () => {
    const url = file(m.png, 'P7');
    const box = document.createElement('div');
    box.style.cssText = 'width:16px;height:16px;background:currentColor';
    box.style.setProperty('mask-image', `url("${url}")`);
    box.style.setProperty('-webkit-mask-image', `url("${url}")`);
    box.style.setProperty('mask-size', 'contain');
    el.appendChild(box);
    // CSS の画像の読み込みはページから見えない（WebKit は kura:// の Resource Timing を残さない）。
    // ページで分かるのは違反がないことだけで、Origin 付きで 200 になったかは Rust のログ（p=P7）で見る
    await sleep(1000);
    const entry = performance.getEntriesByName(url)[0] as PerformanceResourceTiming | undefined;
    const blocked = violations.some((v) => v.blocked.includes('p=P7'));
    const computed = getComputedStyle(box).getPropertyValue('mask-image');
    return {
      ok: !blocked && computed.includes('kura://'),
      detail: {
        blocked,
        timing: entry ? entry.initiatorType : null,
        computed: computed.slice(0, 40),
        judge: 'ログの p=P7 が Origin 付きで 200',
      },
    };
  });

  await run(checks, 'P8', async () => {
    const r = await commands.appearanceGet();
    return { ok: r.status === 'ok', detail: r.status };
  });

  // ここから止まるべきもの
  const messages: unknown[] = [];
  const onMessage = (e: MessageEvent) => messages.push(e.data);
  window.addEventListener('message', onMessage);

  await run(checks, 'F1', async () => {
    const frame = document.createElement('iframe');
    frame.sandbox.add('allow-scripts');
    frame.srcdoc = `<script>fetch(${JSON.stringify(file(m.png, 'F1'))}).then((r) => parent.postMessage({ f1: 'status-' + r.status }, '*')).catch((e) => parent.postMessage({ f1: 'error-' + e.name }, '*'));</script>`;
    let loaded = false;
    frame.onload = () => (loaded = true);
    el.appendChild(frame);
    await sleep(2000);
    frame.remove();
    const got = messages
      .filter((d): d is { f1: string } => typeof d === 'object' && d !== null && 'f1' in d)
      .map((d) => d.f1);
    // 読めてしまう（200）ときだけ外れ。開かない・403・CORS の失敗はどれも止まっている
    return { ok: !got.includes('status-200'), detail: { loaded, got } };
  });

  await run(
    checks,
    'F2',
    async () => {
      const url = import.meta.env.VITE_KURA_SELFTEST_F2;
      if (!url) {
        return { ok: null, detail: 'VITE_KURA_SELFTEST_F2 がないので見送り（手で確かめる）' };
      }
      const opened = await commands.browserOpen(url);
      await sleep(4000);
      await commands.browserClose();
      return {
        ok: null,
        detail: { opened: opened.status, judge: 'ログの label=capture-browser の 403 で判定' },
      };
    },
    10_000,
  );

  await run(checks, 'F3', async () => {
    const statuses: Record<string, number | string> = {};
    for (const method of ['POST', 'PUT', 'DELETE']) {
      try {
        statuses[method] = (await fetch(file(m.png, `F3-${method}`), { method })).status;
      } catch (e) {
        statuses[method] = errorText(e);
      }
    }
    const options = await fetch(file(m.png, 'F3-OPTIONS'), { method: 'OPTIONS' });
    const head = await fetchInfo(file(m.png, 'F3-HEAD'), { method: 'HEAD' });
    const ok =
      ['POST', 'PUT', 'DELETE'].every((k) => statuses[k] === 405) &&
      options.status === 204 &&
      head.status === 200 &&
      head.len === 0;
    return { ok, detail: { statuses, options: options.status, head } };
  });

  await run(checks, 'F4', async () => {
    const before = messages.length;
    const frame = document.createElement('iframe');
    let loaded = false;
    frame.onload = () => (loaded = true);
    frame.src = file(m.svg, 'F4-iframe');
    el.appendChild(frame);
    await sleep(2000);
    frame.remove();
    const ran = messages.slice(before).includes(SVG_SCRIPT_RAN);
    // main の移動は最後に（このあと試す）
    return { ok: !ran, detail: { iframeLoaded: loaded, scriptRan: ran } };
  });

  window.removeEventListener('message', onMessage);
  return checks;
}

/** 自己点検を始める（main.tsx から 1 回だけ呼ぶ）。 */
export async function startSelftest() {
  watchViolations();
  const started = performance.now();
  const base = {
    mode: import.meta.env.DEV ? 'dev' : 'build',
    origin: location.origin,
    reportingObserver: typeof ReportingObserver !== 'undefined',
  };
  let manifest: Manifest;
  try {
    manifest = await waitForManifest();
  } catch (e) {
    await report('selftest', { ...base, pass: false, error: errorText(e) });
    return;
  }
  // 画面が出そろうまで待つ（アプリの UI の違反も数える）
  await sleep(1500);
  const el = stage();
  const checks = await probe(manifest, el);
  during = 'after';
  await sleep(300);
  const verdict = judge(checks, violations);
  await report('selftest', {
    ...base,
    ...verdict,
    ms: Math.round(performance.now() - started),
    checks,
  });

  // F4 の続き：main を kura:// の SVG と、外のページへ移そうとする。止まればここに戻ってこられる
  during = 'F4-nav';
  const stay = location.href;
  location.href = `${K}/file/${manifest.svg.hash}?p=F4-nav`;
  await sleep(1500);
  location.assign('https://example.invalid/?p=F4-nav');
  await sleep(1500);
  await report('selftest-nav', {
    ...base,
    stayed: location.href === stay,
    violations: violations.filter((v) => v.during === 'F4-nav'),
  });
}
