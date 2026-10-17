// scripts/third-party-licenses.mjs の Rust の部分（第三者のライセンスの一覧に載せる依存の名前と節）。
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

import {
  UPSTREAM_FILES,
  UPSTREAM_TEXTS,
  assertFilledIn,
  clarifiedCrates,
  crateLabel,
  rustGroups,
  sortGroups,
  upstreamTexts,
  workspacePackage,
} from './third-party-licenses.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const registry = 'registry+https://github.com/rust-lang/crates.io-index';

describe('crateLabel', () => {
  it('crates.io の依存は名前と版だけ', () => {
    const crate = {
      name: 'blake3',
      version: '1.8.7',
      source: registry,
      manifest_path: '/home/u/.cargo/registry/src/index.crates.io-0/blake3-1.8.7/Cargo.toml',
    };
    expect(crateLabel(crate, root)).toBe('blake3 1.8.7');
  });

  it('third_party/ の写し（path の依存）には改変とソースの在り処を添える', () => {
    const crate = {
      name: 'symphonia-codec-vorbis',
      version: '0.6.1',
      source: null,
      manifest_path: join(root, 'third_party', 'symphonia-codec-vorbis', 'Cargo.toml'),
    };
    // .app の受け手がたどれるよう、リポジトリの URL（ルートの Cargo.toml の workspace.package.repository）を書く
    expect(crateLabel(crate, root)).toBe(
      'symphonia-codec-vorbis 0.6.1（kura が改変。ソース：https://github.com/zredjet/kura の third_party/symphonia-codec-vorbis/、変更は KURA.md）',
    );
    expect(crateLabel(crate, root, 'https://example.com/r')).toBe(
      'symphonia-codec-vorbis 0.6.1（kura が改変。ソース：https://example.com/r の third_party/symphonia-codec-vorbis/、変更は KURA.md）',
    );
  });

  it('crates.io の同じ名前・版でも、出どころが registry なら注記しない', () => {
    const crate = {
      name: 'symphonia-codec-vorbis',
      version: '0.6.1',
      source: registry,
      manifest_path: join(root, 'third_party', 'symphonia-codec-vorbis', 'Cargo.toml'),
    };
    expect(crateLabel(crate, root)).toBe('symphonia-codec-vorbis 0.6.1');
  });

  it('third_party/ の外の path の依存（自前のクレート）は名前と版だけ', () => {
    const crate = {
      name: 'vorbis-tables',
      version: '0.1.0',
      source: null,
      manifest_path: join(root, 'crates', 'vorbis-tables', 'Cargo.toml'),
    };
    expect(crateLabel(crate, root)).toBe('vorbis-tables 0.1.0');
  });

  it('リポジトリの外の third_party/ は写しとみなさない', () => {
    const crate = {
      name: 'x',
      version: '1.0.0',
      source: null,
      manifest_path: join(root, '..', 'elsewhere', 'third_party', 'x', 'Cargo.toml'),
    };
    expect(crateLabel(crate, root)).toBe('x 1.0.0');
  });

  it('third_party/ の下の入れ子のクレートは写しの根ではないので注記しない', () => {
    const crate = {
      name: 'y',
      version: '1.0.0',
      source: null,
      manifest_path: join(root, 'third_party', 'symphonia-codec-vorbis', 'sub', 'Cargo.toml'),
    };
    expect(crateLabel(crate, root)).toBe('y 1.0.0');
  });

  it('写しに KURA.md がなければ止める（誤った注記を書かない）', () => {
    const crate = {
      name: 'no-such-copy',
      version: '0.0.1',
      source: null,
      manifest_path: join(root, 'third_party', 'no-such-copy', 'Cargo.toml'),
    };
    expect(() => crateLabel(crate, root)).toThrow('third_party/no-such-copy/KURA.md');
  });
});

describe('workspacePackage', () => {
  it('ルートの Cargo.toml からリポジトリの URL と kura 自身のライセンスを読む', () => {
    const ws = workspacePackage(readFileSync(join(root, 'Cargo.toml'), 'utf8'));
    expect(ws).toEqual({
      repository: 'https://github.com/zredjet/kura',
      licenses: ['MIT', 'Apache-2.0'],
    });
  });

  it('[workspace.package] の表の中だけを見る', () => {
    const toml = [
      '[package]',
      'repository = "https://example.com/not-this"',
      '[workspace.package]',
      "repository = 'https://example.com/r/'  # 末尾の / は外す",
      'license = "MIT OR Apache-2.0"',
      '[ package ]',
      'license = "GPL-3.0"',
      '[[bin]]',
      'license = "BSD-3-Clause"',
    ].join('\n');
    expect(workspacePackage(toml)).toEqual({
      repository: 'https://example.com/r',
      licenses: ['MIT', 'Apache-2.0'],
    });
  });

  it('URL かライセンスがなければ止める', () => {
    expect(() => workspacePackage('[workspace.package]\nlicense = "MIT"\n')).toThrow('repository');
    expect(() => workspacePackage('[workspace.package]\nrepository = "https://x"\n')).toThrow(
      'license',
    );
    expect(() =>
      workspacePackage('[workspace]\n[workspace.dependencies]\nrepository = "https://x"\n'),
    ).toThrow('repository');
  });

  it('「A OR B」でないライセンスは読まない', () => {
    const toml =
      '[workspace.package]\nrepository = "https://x"\nlicense = "(MIT OR Apache-2.0) AND BSD-3-Clause"\n';
    expect(() => workspacePackage(toml)).toThrow('A OR B');
  });
});

describe('clarifiedCrates', () => {
  it('about.toml で本文を指定したクレートの名前を集める', () => {
    const toml = [
      'accepted = ["MIT"]',
      '[libm.clarify]',
      'license = "MIT"',
      '[[libm.clarify.files]]',
      '[ring_x-y.clarify]  # 注記',
      '[other]',
    ].join('\n');
    expect([...clarifiedCrates(toml)]).toEqual(['libm', 'ring_x-y']);
  });

  it('リポジトリの about.toml は libm と、本文を拾われないクレートの本文を指定している', () => {
    const names = clarifiedCrates(readFileSync(join(root, 'about.toml'), 'utf8'));
    for (const name of ['libm', 'tauri-plugin-dialog', 'blake3', 'dpi', 'dunce']) {
      expect(names.has(name)).toBe(true);
    }
  });
});

describe('upstreamTexts', () => {
  it('リポジトリの写しは SHA-256 が合い、UPSTREAM_TEXTS のすべての版の本文を返す', () => {
    const texts = upstreamTexts(root);
    expect(Object.keys(texts)).toEqual(Object.keys(UPSTREAM_TEXTS));
    // objc2 の MIT だけのクレートには、Steven Sheldon の表示と作者の表示の 2 つの本文を載せる
    const objc2 = texts['objc2 0.6.5'].MIT;
    expect(objc2).toHaveLength(2);
    expect(objc2[0]).toContain('Copyright (c) Steven Sheldon');
    expect(objc2[1]).toContain('Copyright 2026 Mads Marquart');
    expect(texts['dispatch2 0.3.1'].MIT).toEqual([objc2[1]]);
    expect(texts['specta 2.0.0-rc.25'].MIT[0]).toContain('Copyright (c) 2022 Oscar Beaumont');
  });

  it('licenses/upstream/ のファイルはすべて UPSTREAM_FILES にあり、UPSTREAM_TEXTS から使われる', () => {
    const dir = join(root, 'licenses', 'upstream');
    const onDisk = readdirSync(dir, { recursive: true, withFileTypes: true })
      .filter((e) => e.isFile())
      .map((e) => relative(dir, join(e.parentPath, e.name)).split(sep).join('/'))
      .sort();
    expect(onDisk).toEqual(Object.keys(UPSTREAM_FILES).sort());
    const usedPaths = new Set(
      Object.values(UPSTREAM_TEXTS).flatMap((byId) => Object.values(byId).flat()),
    );
    expect([...usedPaths].sort()).toEqual(Object.keys(UPSTREAM_FILES).sort());
  });

  it('写しの SHA-256 が合わない・ファイルがない・UPSTREAM_FILES にない場所なら止める', () => {
    const path = 'objc2-ee9a7ada/LICENSE-MIT.txt';
    const texts = { 'objc2 0.6.5': { MIT: [path] } };
    const wrong = { [path]: { ...UPSTREAM_FILES[path], sha256: '0'.repeat(64) } };
    expect(() => upstreamTexts(root, wrong, texts)).toThrow('SHA-256');
    const missing = { 'no-such/LICENSE': UPSTREAM_FILES[path] };
    expect(() => upstreamTexts(root, missing, { 'x 1.0.0': { MIT: ['no-such/LICENSE'] } })).toThrow(
      'licenses/upstream/no-such/LICENSE',
    );
    expect(() => upstreamTexts(root, {}, texts)).toThrow('UPSTREAM_FILES');
  });
});

describe('assertFilledIn', () => {
  const group = (id, text) => ({ id, text, usedBy: ['x 1.0.0'] });

  it('雛形の著作権者の欄が残った本文は止める', () => {
    expect(() =>
      assertFilledIn([group('MIT', 'MIT License\n\nCopyright (c) <year> <copyright holders>\n')]),
    ).toThrow('MIT（x 1.0.0）');
    expect(() => assertFilledIn([group('BSD-3-Clause', 'Copyright (c) <year> <owner>. ')])).toThrow(
      'BSD-3-Clause',
    );
  });

  it('著作権表示のある本文と、Apache-2.0 の付録の書き方の例は通す', () => {
    const apache =
      'APPENDIX: How to apply the Apache License\n\nCopyright [yyyy] [name of copyright owner]\n';
    expect(() =>
      assertFilledIn([
        group('MIT', 'Copyright (c) 2022 Oscar Beaumont'),
        group('Apache-2.0', apache),
      ]),
    ).not.toThrow();
  });
});

describe('rustGroups', () => {
  const workspace = {
    repository: 'https://github.com/zredjet/kura',
    licenses: ['MIT', 'Apache-2.0'],
  };
  const options = { repoRoot: root, workspace, clarified: new Set(['libm']) };
  const own = (name) => ({
    name,
    version: '0.1.0',
    source: null,
    manifest_path: join(root, 'crates', name, 'Cargo.toml'),
  });
  const dep = (name, version) => ({
    name,
    version,
    source: registry,
    manifest_path: `/home/u/.cargo/registry/src/index.crates.io-0/${name}-${version}/Cargo.toml`,
  });
  const license = (id, text, crates, sourcePath = '/x/LICENSE') => ({
    id,
    text,
    source_path: sourcePath,
    used_by: crates.map((crate) => ({ crate, path: null })),
  });
  const engineBsd = 'kura-engine（codec/vorbis：aoTuV・libvorbis・libogg の移植）';
  const shape = (groups) => groups.map((g) => [g.id, g.text, g.usedBy]);

  it('kura- のクレートの kura 自身のライセンス（MIT・Apache-2.0）は載せない', () => {
    const about = {
      licenses: [
        license('MIT', 'mit', [own('kura-core'), own('kura-engine'), dep('blake3', '1.8.7')]),
        license('Apache-2.0', 'apache', [own('kura-mac')]),
      ],
    };
    expect(shape(sortGroups(rustGroups(about, options)))).toEqual([
      ['MIT', 'mit', ['blake3 1.8.7']],
    ]);
  });

  it('kura-engine の BSD-3-Clause（移植した部分）は、vorbis-tables と本文が同じなら同じ節に載る', () => {
    const about = {
      licenses: [license('BSD-3-Clause', 'aotuv', [own('kura-engine'), own('vorbis-tables')])],
    };
    expect(shape(sortGroups(rustGroups(about, options)))).toEqual([
      ['BSD-3-Clause', 'aotuv', [engineBsd, 'vorbis-tables 0.1.0']],
    ]);
  });

  it('kura-engine の BSD-3-Clause は、本文がほかと違っても自分の節で載る（黙って消えない）', () => {
    const about = {
      licenses: [
        license('BSD-3-Clause', 'aotuv + libogg', [own('kura-engine')]),
        license('BSD-3-Clause', 'aotuv', [own('vorbis-tables')]),
        license('MIT', 'template', [own('kura-engine')], null),
      ],
    };
    expect(shape(sortGroups(rustGroups(about, options)))).toEqual([
      ['BSD-3-Clause', 'aotuv + libogg', [engineBsd]],
      ['BSD-3-Clause', 'aotuv', ['vorbis-tables 0.1.0']],
    ]);
  });

  it('説明のない kura- のクレートの、kura 自身のものでないライセンスは止める', () => {
    const unknownPart = { licenses: [license('BSD-3-Clause', 'x', [own('kura-core')])] };
    expect(() => rustGroups(unknownPart, options)).toThrow('kura-core');
    const unknownId = { licenses: [license('Zlib', 'x', [own('kura-engine')])] };
    expect(() => rustGroups(unknownId, options)).toThrow('KURA_PORTED');
  });

  it('本文を指定したクレートが雛形（source_path がない）で出たら止める', () => {
    const fellBack = { licenses: [license('MIT', 'template', [dep('libm', '0.2.16')], null)] };
    expect(() => rustGroups(fellBack, options)).toThrow(
      'libm 0.2.16 の MIT：about.toml で本文を指定したのに',
    );
  });

  it('本文を指定していないクレートの雛形も、写しがなければ止める', () => {
    const other = { licenses: [license('MIT', 'template', [dep('specta', '2.0.0')], null)] };
    expect(() => rustGroups(other, options)).toThrow('specta 2.0.0 の MIT：');
    expect(() => rustGroups(other, options)).toThrow('UPSTREAM_TEXTS');
    // 写しはライセンスごと：同じクレートでもほかのライセンスの写ししかなければ止める
    const otherId = { ...options, upstream: { 'specta 2.0.0': { 'Apache-2.0': ['apache'] } } };
    expect(() => rustGroups(other, otherId)).toThrow('specta 2.0.0 の MIT：');
  });

  it('kura- のクレートの、載せない（kura 自身のライセンスの）雛形では止めない', () => {
    const about = { licenses: [license('MIT', 'template', [own('kura-core')], null)] };
    expect(shape(sortGroups(rustGroups(about, options)))).toEqual([]);
  });

  it('雛形の節のクレートを、上流の本文の写しの節に移す（1 つのクレートに写しが 2 つなら 2 つの節）', () => {
    const about = {
      licenses: [
        license(
          'MIT',
          'template',
          [own('kura-core'), dep('objc2', '0.6.5'), dep('dispatch2', '0.3.1')],
          null,
        ),
      ],
    };
    const upstream = {
      'objc2 0.6.5': { MIT: ['sheldon', 'marquart'] },
      'dispatch2 0.3.1': { MIT: ['marquart'] },
    };
    expect(shape(sortGroups(rustGroups(about, { ...options, upstream })))).toEqual([
      ['MIT', 'marquart', ['dispatch2 0.3.1', 'objc2 0.6.5']],
      ['MIT', 'sheldon', ['objc2 0.6.5']],
    ]);
  });

  it('写しの本文と書き出す本文が同じ節があれば、そこにまとめる', () => {
    const about = {
      licenses: [
        license('BSD-3-Clause', 'dropbox', [dep('alloc-no-stdlib', '3.0.0')]),
        license('BSD-3-Clause', 'template', [dep('alloc-stdlib', '0.3.0')], null),
      ],
    };
    const upstream = { 'alloc-stdlib 0.3.0': { 'BSD-3-Clause': ['dropbox\r\n'] } };
    expect(shape(sortGroups(rustGroups(about, { ...options, upstream })))).toEqual([
      ['BSD-3-Clause', 'dropbox', ['alloc-no-stdlib 3.0.0', 'alloc-stdlib 0.3.0']],
    ]);
  });

  it('書き出す本文が同じ節は、cargo-about が分けても 1 つにまとめる（CRLF・末尾の改行だけの違い）', () => {
    const about = {
      licenses: [
        license('MIT', 'mit\r\ntext\n', [dep('tauri', '2.12.2')]),
        license('MIT', 'mit\ntext', [dep('tauri-plugin-fs', '2.6.0')]),
        license('Apache-2.0', 'mit\ntext', [dep('other', '1.0.0')]),
      ],
    };
    expect(shape(sortGroups(rustGroups(about, options)))).toEqual([
      ['Apache-2.0', 'mit\ntext', ['other 1.0.0']],
      ['MIT', 'mit\r\ntext\n', ['tauri 2.12.2', 'tauri-plugin-fs 2.6.0']],
    ]);
  });

  it('使われなかった写しがあれば止める（依存の版が変わった、cargo-about が本文を見つけた）', () => {
    const upstream = { 'objc2 0.6.4': { MIT: ['marquart'] } };
    const bumped = { licenses: [license('MIT', 'mit', [dep('blake3', '1.8.7')])] };
    expect(() => rustGroups(bumped, { ...options, upstream })).toThrow('objc2 0.6.4 の MIT');
    const found = { licenses: [license('MIT', 'mit', [dep('objc2', '0.6.4')])] };
    expect(() => rustGroups(found, { ...options, upstream })).toThrow('UPSTREAM_TEXTS');
  });
});
