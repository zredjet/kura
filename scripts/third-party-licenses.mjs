// 配布する Kura.app に入る第三者のソフトウェアのライセンス表記を THIRD_PARTY_LICENSES.md に書き出す。
// Rust は cargo-about（about.toml）、JavaScript は pnpm の本番の依存から作る。
//
//   pnpm run licenses          … 生成する
//   pnpm run licenses:check    … 生成物が最新か確かめる（依存を変えたのに作り直していなければ失敗）
//
// リポジトリの third_party/ に置いた写し（kura が手を入れ、ルートの `[patch.crates-io]` で差し替えたクレート）は、
// crates.io の同じ版と見分けられるよう、名前に改変したことと改変後のソースの在り処（リポジトリの URL と場所）を添える
// （`crateLabel`）。kura の自前のクレート（kura- で始まるもの）は、kura 自身のライセンスの表示は載せず、第三者のコードを
// 移した部分の表示だけを、どの部分かの説明を付けて載せる（`rustGroups`・`KURA_PORTED`）。
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const output = join(root, 'THIRD_PARTY_LICENSES.md');

function run(cmd, args) {
  return execFileSync(cmd, args, { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
}

/**
 * ルートの Cargo.toml の本文から、`[workspace.package]` の `repository`（kura のリポジトリの URL）と `license`
 * （kura 自身のライセンス。`MIT OR Apache-2.0` を選択肢の並び `['MIT', 'Apache-2.0']` にする）を読む。
 * TOML の全部は解さず、この表の `キー = "文字列"` の行だけを見る。どちらかが読めなければ止める。
 */
export function workspacePackage(toml) {
  const found = {};
  let inTable = false;
  for (const line of toml.split(/\r?\n/)) {
    if (/^\s*\[\[/.test(line)) {
      inTable = false;
      continue;
    }
    const header = /^\s*\[\s*([^[\]]+?)\s*\]\s*(#.*)?$/.exec(line);
    if (header) {
      inTable = header[1] === 'workspace.package';
      continue;
    }
    if (!inTable) continue;
    const kv = /^\s*(repository|license)\s*=\s*(?:"([^"\\]*)"|'([^']*)')\s*(#.*)?$/.exec(line);
    if (kv) found[kv[1]] = kv[2] ?? kv[3];
  }
  for (const key of ['repository', 'license']) {
    if (!found[key]) {
      throw new Error(
        `ルートの Cargo.toml の [workspace.package] に ${key} = "…" が見つかりません`,
      );
    }
  }
  const licenses = found.license.split(/\s+OR\s+/);
  if (licenses.some((id) => !/^[A-Za-z0-9.+-]+$/.test(id))) {
    throw new Error(
      `ルートの Cargo.toml の license（${found.license}）は「A OR B」の形でなければなりません（kura 自身のライセンスの選択肢として読むため）`,
    );
  }
  return { repository: found.repository.replace(/\/+$/, ''), licenses };
}

/** about.toml の本文から、本文を指定した（`[<名前>.clarify]` のある）クレートの名前を集める。 */
export function clarifiedCrates(aboutToml) {
  const names = new Set();
  for (const m of aboutToml.matchAll(/^\s*\[\s*([A-Za-z0-9_-]+)\.clarify\s*\]\s*(#.*)?$/gm)) {
    names.add(m[1]);
  }
  return names;
}

/**
 * 一覧に載せる Rust の依存の名前（`名前 版`）。cargo-about の JSON の `crate`（`name`・`version`・`source`・
 * `manifest_path`）を受け取る。
 *
 * path の依存（`source` がない）のうち、マニフェストが `<repoRoot>/third_party/<ディレクトリ>/Cargo.toml` のものは
 * kura が手を入れた写しなので、crates.io の同じ版と見分けられるよう、改変したことと改変後のソースの在り処を添える
 * （MPL-2.0 §3.2(a) などが求める、実行形式の受け手へのソースの入手方法の知らせ）。在り処は、.app の受け手がたどれる
 * よう、リポジトリの URL（`repository`。省くとルートの Cargo.toml の `[workspace.package]` から読む）と場所で書く。
 * 写しには変更の記録 `third_party/<ディレクトリ>/KURA.md` がなければならず、ないときは誤った注記を書かないよう止める。
 * それ以外（crates.io の依存、crates/ の下の自前のクレートなど）は名前と版だけ。
 */
export function crateLabel(crate, repoRoot, repository) {
  const label = `${crate.name} ${crate.version}`;
  if (crate.source != null || typeof crate.manifest_path !== 'string') return label;
  const rel = relative(repoRoot, crate.manifest_path).split(sep).join('/');
  const m = /^third_party\/([^/]+)\/Cargo\.toml$/.exec(rel);
  if (!m) return label;
  const dir = `third_party/${m[1]}`;
  if (!existsSync(join(repoRoot, dir, 'KURA.md'))) {
    throw new Error(
      `${dir}/KURA.md（kura の変更の記録）がありません。改変した写しには変更と理由を書いてください`,
    );
  }
  const url =
    repository ?? workspacePackage(readFileSync(join(repoRoot, 'Cargo.toml'), 'utf8')).repository;
  return `${label}（kura が改変。ソース：${url} の ${dir}/、変更は KURA.md）`;
}

/**
 * kura の自前のクレート（kura- で始まるもの）が、kura 自身のライセンス（ルートの Cargo.toml の
 * `[workspace.package]` の license の選択肢）とは別に持つライセンスと、それが及ぶ部分の説明。
 *
 * kura 自身のライセンスの表示は第三者のものではないので一覧に載せない。しかし第三者のコードを移した部分のライセンス
 * （BSD-3-Clause など。バイナリの配布物の資料に著作権の表示と条件を載せることを求める）は、移した部分として載せる。
 * その表示は、ほかのクレート（たとえば同じ LICENSE を持つ vorbis-tables）と本文が同じなら同じ節にまとまり、違えば
 * 自分の節になる。どちらでも落とさない。ここにない組み合わせが出たら、黙って一覧から消さないよう止める。
 */
export const KURA_PORTED = {
  'kura-engine': {
    // src/codec/vorbis/（表示は src/codec/vorbis/LICENSE）。ADR-149
    'BSD-3-Clause': 'codec/vorbis：aoTuV・libvorbis・libogg の移植',
  },
};

/** 1 つのライセンスの節に、`crate` を何という名前で載せるか（載せないなら空）。 */
function rustUser(id, crate, repoRoot, workspace) {
  if (!crate.name.startsWith('kura-')) return [crateLabel(crate, repoRoot, workspace.repository)];
  if (workspace.licenses.includes(id)) return [];
  const parts = Object.hasOwn(KURA_PORTED, crate.name) ? KURA_PORTED[crate.name] : {};
  if (!Object.hasOwn(parts, id)) {
    throw new Error(
      `${crate.name} が kura 自身のライセンス（${workspace.licenses.join('・')}）でない ${id} の表示を持っています。` +
        '第三者のコードを移した部分なら、scripts/third-party-licenses.mjs の KURA_PORTED にその部分の説明を足してください' +
        '（一覧から黙って落とさないよう止めます）',
    );
  }
  return [`${crate.name}（${parts[id]}）`];
}

/**
 * cargo-about の JSON（`licenses`）を { ライセンス, 本文, 使っているもの } の並びにする。
 *
 * - 使っているものの名前は `crateLabel`。kura- のクレートは `rustUser` の規則で載せる・載せないを決める。
 * - about.toml で本文を指定した（clarify した）クレートが、指定した本文でなく SPDX の雛形（`source_path` がない。
 *   著作権者の欄が `<year> <copyright holders>` のまま）で出てきたら止める。cargo-about は、clarify の checksum が
 *   合わない（依存の版を上げて LICENSE が変わった、など）と警告を出すだけで雛形に戻すため。
 *
 * `options`：`repoRoot`（リポジトリの根）、`workspace`（`workspacePackage` の結果）、`clarified`（`clarifiedCrates`）。
 */
export function rustGroups(about, { repoRoot, workspace, clarified }) {
  return about.licenses.map((l) => {
    if (l.source_path == null) {
      const lost = l.used_by.map((u) => u.crate.name).filter((name) => clarified.has(name));
      if (lost.length > 0) {
        throw new Error(
          `${lost.join('・')} の ${l.id} が、about.toml で指定した本文でなく雛形で出ました。` +
            'clarify の checksum が LICENSE と合っているか確かめてください（cargo-about の警告を見る）',
        );
      }
    }
    return {
      id: l.id,
      text: l.text,
      usedBy: l.used_by.flatMap((u) => rustUser(l.id, u.crate, repoRoot, workspace)),
    };
  });
}

/** cargo-about を src-tauri（配布する .app）について回し、`rustGroups` の並びにする。 */
function rustLicenses() {
  let json;
  try {
    json = run('cargo', [
      'about',
      'generate',
      '--manifest-path',
      'src-tauri/Cargo.toml',
      '--config',
      'about.toml',
      '--format',
      'json',
      '--offline',
      '--locked',
    ]);
  } catch (e) {
    throw new Error(
      `cargo-about を実行できませんでした。\`cargo install --locked cargo-about\` で入れてください。\n${e}`,
    );
  }
  return rustGroups(JSON.parse(json), {
    repoRoot: root,
    workspace: workspacePackage(readFileSync(join(root, 'Cargo.toml'), 'utf8')),
    clarified: clarifiedCrates(readFileSync(join(root, 'about.toml'), 'utf8')),
  });
}

const MIT_TEMPLATE = `MIT License

Copyright (c) {holder}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
`;

/** 載せるライセンス。`MIT OR Apache-2.0` のような二重ライセンスは Rust と揃えて MIT を選ぶ。 */
function chooseLicense(expr) {
  const options = expr.split(/\s+OR\s+/);
  return options.includes('MIT') ? 'MIT' : expr;
}

/** パッケージの中の LICENSE ファイルの本文。MIT でファイルがなければ、作者名を入れた MIT の原文を使う。 */
function licenseText(dir, id) {
  const files = readdirSync(dir);
  const pick = (re) => files.find((f) => re.test(f));
  const file =
    (id === 'MIT' && pick(/^licen[cs]e-mit(\.(md|txt))?$/i)) ||
    pick(/^(licen[cs]e|copying)(\.(md|txt))?$/i);
  if (file) return readFileSync(join(dir, file), 'utf8');
  if (id !== 'MIT') return null;
  // author が標準。Tauri のプラグインは authors（配列）に書いている
  const { author, authors } = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  const person = author ?? authors?.[0];
  const holder = typeof person === 'string' ? person : person?.name;
  return holder ? MIT_TEMPLATE.replace('{holder}', holder) : null;
}

/** 画面のコードに束ねられる npm の依存（本番の依存とその先）。 */
function jsLicenses() {
  const list = JSON.parse(run('pnpm', ['licenses', 'list', '--prod', '--json']));
  const groups = new Map();
  for (const [expr, pkgs] of Object.entries(list)) {
    const id = chooseLicense(expr);
    for (const pkg of pkgs) {
      pkg.versions.forEach((version, i) => {
        const text = licenseText(pkg.paths[i], id);
        if (text === null) {
          throw new Error(`${pkg.name} ${version} のライセンスの本文が見つかりません`);
        }
        const key = `${id}\n${text.trim()}`;
        const group = groups.get(key) ?? { id, text, usedBy: [] };
        group.usedBy.push(`${pkg.name} ${version}`);
        groups.set(key, group);
      });
    }
  }
  return [...groups.values()];
}

/** 節の中の名前と節を並べ、載せる名前がなくなった節（Rust の、kura 自身のライセンスのクレートだけだった節）を除く。 */
export function sortGroups(groups) {
  for (const g of groups) g.usedBy.sort((a, b) => a.localeCompare(b, 'en'));
  return groups
    .filter((g) => g.usedBy.length > 0)
    .sort((a, b) => a.id.localeCompare(b.id, 'en') || a.usedBy[0].localeCompare(b.usedBy[0], 'en'));
}

function section(title, groups) {
  const lines = [`## ${title}`, ''];
  for (const g of groups) {
    lines.push(`### ${g.id}`, '', `対象：${g.usedBy.join('、')}`, '', '````text');
    lines.push(g.text.replace(/\r\n/g, '\n').trimEnd(), '````', '');
  }
  return lines;
}

function summary(rust, js) {
  const count = new Map();
  for (const g of [...rust, ...js]) count.set(g.id, (count.get(g.id) ?? 0) + g.usedBy.length);
  const rows = [...count].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0], 'en'));
  return ['| ライセンス | 件数 |', '|---|--:|', ...rows.map(([id, n]) => `| ${id} | ${n} |`), ''];
}

function main() {
  const check = process.argv.includes('--check');
  const rust = sortGroups(rustLicenses());
  const js = sortGroups(jsLicenses());
  const doc = [
    '# 第三者のソフトウェアのライセンス',
    '',
    'Kura の配布物（Kura.app）に含まれる、第三者のソフトウェアのライセンス表記です。',
    '`pnpm run licenses` で生成しています。手で編集しないでください。',
    '',
    '二重ライセンスの依存は、`about.toml` の並びで先にあるライセンスを選んで載せています。',
    'yt-dlp・gallery-dl・ffmpeg・deno・mpv・Google Chrome などの外部ツールは Kura に同梱せず、',
    '利用者が別に入れたものを別のプロセスとして呼び出すだけなので、ここには含めていません。',
    '',
    ...summary(rust, js),
    ...section('Rust のライブラリ', rust),
    ...section('JavaScript のライブラリ', js),
  ].join('\n');

  if (check) {
    let current = '';
    try {
      current = readFileSync(output, 'utf8');
    } catch {
      // ないときは古いのと同じ扱い
    }
    if (current !== doc) {
      console.error(
        'THIRD_PARTY_LICENSES.md が古くなっています。`pnpm run licenses` で作り直してください。',
      );
      process.exit(1);
    }
    console.log('THIRD_PARTY_LICENSES.md は最新です。');
  } else {
    writeFileSync(output, doc);
    console.log(
      `THIRD_PARTY_LICENSES.md を書き出しました（Rust ${rust.length} 種・JavaScript ${js.length} 種）。`,
    );
  }
}

/** このファイルが `node scripts/third-party-licenses.mjs` で直に動かされたか（シンボリックリンクの道でも同じに見る）。 */
function isEntry() {
  if (!process.argv[1]) return false;
  try {
    return realpathSync(resolve(process.argv[1])) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

// 直に動かしたときだけ生成する（テストが crateLabel を読み込むときは動かさない）
if (isEntry()) {
  main();
}
