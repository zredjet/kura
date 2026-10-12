// 配布する Kura.app に入る第三者のソフトウェアのライセンス表記を THIRD_PARTY_LICENSES.md に書き出す。
// Rust は cargo-about（about.toml）、JavaScript は pnpm の本番の依存から作る。
//
//   pnpm run licenses          … 生成する
//   pnpm run licenses:check    … 生成物が最新か確かめる（依存を変えたのに作り直していなければ失敗）
import { execFileSync } from 'node:child_process';
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const output = join(root, 'THIRD_PARTY_LICENSES.md');
const check = process.argv.includes('--check');

function run(cmd, args) {
  return execFileSync(cmd, args, { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
}

/** cargo-about の結果を { ライセンス, 本文, 使っているもの } の並びにする。 */
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
  const about = JSON.parse(json);
  return about.licenses.map((l) => ({
    id: l.id,
    text: l.text,
    usedBy: l.used_by
      .map((u) => `${u.crate.name} ${u.crate.version}`)
      .filter((name) => !name.startsWith('kura-')),
  }));
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

function sortGroups(groups) {
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
