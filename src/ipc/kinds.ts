/**
 * アイテムの種類（`items.kind`）をフロントで扱う 1 か所（M11）。
 *
 * - 種類の一覧と性質は Rust が正本（`MediaKind`）。bindings.ts の定数 `ITEM_KINDS` を読むだけにし、
 *   種類の型・並び・述語はここから作る（`'image' | 'video' | 'audio'` のような型や並びを画面ごとに書かない）。
 * - ラベルは `ja.kinds`、アイコンは `components/icons/KindIcon.tsx`。
 * - 検索の語（`種類:フォント` の「フォント」）はラベルと別に持つ（Rust の `query::KIND_WORDS` を bindings の定数
 *   `QUERY_KIND_WORDS` で読む）。今はラベルと同じ語だが、ラベルを変えても式が壊れないようにする。
 */
import { ja } from '../i18n/ja';
import { ITEM_KINDS, type ItemKindDto, QUERY_KIND_WORDS } from './bindings';

type KindRow = (typeof ITEM_KINDS)[number];

/** アイテムの種類。 */
export type ItemKind = ItemKindDto;

/** 再生のキュー・プレーヤーに入る種類（画像・動画・音楽。フォント・3D は入らない。docs/playlists.md §1.1）。 */
export type PlayableKind = Extract<KindRow, { playable: true }>['kind'];

/** 長さを持つ種類（動画・音楽）。 */
export type TimedKind = Extract<KindRow, { hasDuration: true }>['kind'];

/** 種類を並べる順（フィルタバー・パレットの内訳・取り込みの件数・プレイリストの件数で共通）。 */
export const KIND_ORDER: readonly ItemKind[] = ITEM_KINDS.map((row) => row.kind);

/** 再生できる種類（並びは `KIND_ORDER` のまま）。 */
export const PLAYABLE_KINDS: readonly PlayableKind[] = ITEM_KINDS.filter(
  (row): row is Extract<KindRow, { playable: true }> => row.playable,
).map((row) => row.kind);

const ROWS = new Map<string, KindRow>(ITEM_KINDS.map((row) => [row.kind, row]));

/** ラベル（`ja.kinds`。種類を足して文言を書き忘れると型で落ちる）。 */
const LABELS: Readonly<Record<ItemKind, string>> = ja.kinds;

/** 検索の語（Rust の `KIND_WORDS`。英語の名前は `kind` そのもの）。語のない種類は英語の名前で書く。 */
const WORD_OF = new Map<string, string>(QUERY_KIND_WORDS.map(({ kind, word }) => [kind, word]));
const WORDS: Readonly<Record<ItemKind, string>> = Object.fromEntries(
  ITEM_KINDS.map(({ kind }) => [kind, WORD_OF.get(kind) ?? kind]),
) as Record<ItemKind, string>;

/** 種類の値か（DTO の文字列を確かめる。知らない値は偽）。 */
export function isItemKind(value: string | null | undefined): value is ItemKind {
  return value != null && ROWS.has(value);
}

/** 再生のキューに入る種類か（キューは Rust が作る。これは画面の分岐と、万一入ったときの守り）。 */
export function isPlayable(kind: string | null | undefined): kind is PlayableKind {
  return ROWS.get(kind ?? '')?.playable === true;
}

/** 長さを持つ種類か（長さのバッジ・列・読み上げ、再生の要素の先読み）。 */
export function hasDuration(kind: string | null | undefined): kind is TimedKind {
  return ROWS.get(kind ?? '')?.hasDuration === true;
}

/** 再生用プロキシの対象か（Rust の `kind IN ('video','audio')` と同じ）。 */
export function needsProxy(kind: string | null | undefined): boolean {
  return ROWS.get(kind ?? '')?.needsProxy === true;
}

/** 種類のラベル（「画像」「3D」）。知らない値は空文字。 */
export function kindLabel(kind: string | null | undefined): string {
  return isItemKind(kind) ? LABELS[kind] : '';
}

/** 検索の語（`種類:<語>` の語）。 */
export function kindWord(kind: ItemKind): string {
  return WORDS[kind];
}

/**
 * 検索の語から種類（Rust の `parse_kind` と同じ：日本語の語は大文字・小文字を問わず、英語は種類の名前）。
 * 知らない語は null。
 */
export function kindFromWord(word: string): ItemKind | null {
  const lower = word.trim().toLowerCase();
  for (const kind of KIND_ORDER) {
    if (WORDS[kind].toLowerCase() === lower || kind === lower) {
      return kind;
    }
  }
  return null;
}

/** 種類ごとの数。 */
export type KindCounts = Readonly<Record<ItemKind, number>>;

/** 複数形の欄（`images`・`fonts` など。取り込みの確認・プレイリストの中身）の DTO を種類ごとの数にする。 */
export function countsFromPlural(dto: {
  images: number;
  videos: number;
  audios: number;
  fonts: number;
  models: number;
}): KindCounts {
  return {
    image: dto.images,
    video: dto.videos,
    audio: dto.audios,
    font: dto.fonts,
    model: dto.models,
  };
}

/** 種類ごとの数を `KIND_ORDER` の順に並べ、0 件を除く（「画像 142 · 動画 44 · フォント 3」）。 */
export function kindBreakdown(
  counts: Partial<Record<ItemKind, number>>,
): { kind: ItemKind; label: string; count: number }[] {
  return KIND_ORDER.map((kind) => ({ kind, label: LABELS[kind], count: counts[kind] ?? 0 })).filter(
    (row) => row.count > 0,
  );
}
