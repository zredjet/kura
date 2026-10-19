import { type MouseEvent, useEffect, useMemo, useRef, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { InlineName } from '../../components/InlineName/InlineName';
import { type MenuEntry, openMenuBelow } from '../../components/Menu/menuStore';
import { MoreIcon } from '../../components/icons/MoreIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { PlaylistIcon } from '../../components/icons/PlaylistIcon';
import { ReshuffleIcon } from '../../components/icons/ReshuffleIcon';
import { SparkleIcon } from '../../components/icons/SparkleIcon';
import { Segmented } from '../../components/Segmented/Segmented';
import { ja } from '../../i18n/ja';
import { countsFromPlural, kindBreakdown } from '../../ipc/kinds';
import {
  getPlaylist,
  type PlaylistContentsDto,
  playlistContents,
  type PlaylistDto,
} from '../../ipc/playlists';
import { relativeDateTime } from '../../lib/dates';
import { formatCount, formatLongDuration } from '../../lib/format';
import { getCached, requestItems, useItemCacheVersion } from '../../stores/itemCache';
import { bumpRevision } from '../../stores/revisionStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useLoaded } from '../library/inspector/useLoaded';
import { MiniPlayer } from '../player/MiniPlayer';
import { usePlayerStore } from '../player/playerStore';
import { EntryList, Thumb } from './EntryList';
import { GeneratedTable } from './GeneratedTable';
import {
  deletePlaylistWithConfirm,
  playPlaylist,
  regenerate,
  renamePlaylist,
  snapshotPlaylist,
  switchGranularity,
} from './playlistActions';
import { PlaylistOptions } from './PlaylistSettings';
import styles from './PlaylistView.module.css';
import { SmartPanel } from './SmartPanel';

const t = ja.playlist;

/** プレイリストの画面（Playlist.dc.html）。手動は設定と中身の一覧、スマートはルールと「今回の並び」。 */
export function PlaylistView({ id }: { id: number }) {
  const playlist = useLoaded(`playlist-${id}`, () => getPlaylist(id));
  const contents = useLoaded(`contents-${id}`, () => playlistContents(id));
  const scrollRef = useRef<HTMLDivElement>(null);

  return (
    <div className={styles.view}>
      <div ref={scrollRef} className={styles.scroll}>
        {playlist && contents && (
          <div className={styles.stack}>
            <Header playlist={playlist} contents={contents} />
            {playlist.kind === 'smart' ? (
              <>
                <SmartPanel key={playlist.id} playlist={playlist} />
                <GeneratedTable playlist={playlist} contents={contents} scrollRef={scrollRef} />
              </>
            ) : (
              <>
                <section className={styles.panel} aria-label={t.settings}>
                  <PlaylistOptions playlist={playlist} />
                </section>
                <EntryList
                  playlist={playlist}
                  contents={contents}
                  scrollRef={scrollRef}
                  onChanged={bumpRevision}
                />
              </>
            )}
          </div>
        )}
      </div>
      <MiniPlayer />
    </div>
  );
}

/** 見出し：表紙・種類・題名と単位の切り替え・件数と長さ・再生などのボタン。 */
function Header({ playlist, contents }: { playlist: PlaylistDto; contents: PlaylistContentsDto }) {
  const [editing, setEditing] = useState(false);
  useItemCacheVersion((s) => s.version);
  const smart = playlist.kind === 'smart';

  // 表紙：先頭から、サムネイルのあるアイテムを 4 つ
  const coverIds = useMemo(
    () => Array.from(new Set(contents.flatItemIds.slice(0, 200))).slice(0, 24),
    [contents],
  );
  useEffect(() => {
    requestItems(coverIds);
  }, [coverIds]);
  const covers = coverIds
    .map((itemId) => getCached(itemId))
    .filter((item) => item?.thumbState === 'ready')
    .slice(0, 4);

  const parts = [t.items(formatCount(contents.itemCount))];
  for (const { label, count } of kindBreakdown(countsFromPlural(contents))) {
    parts.push(t.kindCount(label, formatCount(count)));
  }
  if (contents.totalMs > 0) {
    parts.push(t.approx(formatLongDuration(contents.totalMs)));
  }

  // 再生している最中は位置が古いので出さない（閉じたときに保存して読み直す）
  const playing = usePlayerStore((s) => s.queue?.playlist?.id === playlist.id);
  const resume = playing ? null : playlist.resume;
  const summary = useSidebarStore((s) => s.playlists.find((p) => p.id === playlist.id));

  const more = (e: MouseEvent<HTMLButtonElement>) => {
    const entries: MenuEntry[] = [];
    if (!smart) {
      entries.push(
        { label: t.snapshotAll, onSelect: () => void snapshotPlaylist(playlist.id, null) },
        'separator',
      );
    }
    entries.push({ label: t.rename, onSelect: () => setEditing(true) }, 'separator', {
      label: ja.playlistMenu.delete,
      danger: true,
      onSelect: () => {
        if (summary) {
          void deletePlaylistWithConfirm(summary);
        }
      },
    });
    openMenuBelow(e.currentTarget, t.more, entries);
  };

  return (
    <header className={styles.header}>
      <div className={styles.cover} aria-hidden="true">
        {[0, 1, 2, 3].map((i) => (
          <Thumb key={i} item={covers[i]} className="" />
        ))}
      </div>
      <div className={styles.headText}>
        <span className={styles.kindLabel}>
          {smart ? (
            <SparkleIcon size={14} strokeWidth={2} />
          ) : (
            <PlaylistIcon size={14} strokeWidth={2} />
          )}
          {smart ? t.smartLabel : t.manualLabel}
        </span>
        <div className={styles.titleRow}>
          {editing ? (
            <InlineName
              initial={playlist.name}
              label={ja.sidebar.playlistName}
              className={styles.titleInput}
              onCommit={(name) => {
                setEditing(false);
                void renamePlaylist(playlist.id, name);
              }}
              onCancel={() => setEditing(false)}
            />
          ) : (
            <h1 className={styles.title}>
              <button
                type="button"
                className={styles.titleButton}
                title={t.rename}
                onClick={() => setEditing(true)}
              >
                {playlist.name}
              </button>
            </h1>
          )}
          <Segmented
            label={t.granularity}
            value={playlist.granularity}
            options={[
              { value: 'file', label: t.file },
              { value: 'unit', label: t.unit },
            ]}
            onChange={(g) => {
              void switchGranularity(playlist.id, g).then(() => {
                if (smart) {
                  void regenerate(playlist.id);
                }
              });
            }}
          />
        </div>
        <span className={styles.summary}>{t.summary(parts)}</span>
        {smart && playlist.generatedAt !== null && (
          <span className={styles.subline}>
            {[
              t.generatedAt(relativeDateTime(playlist.generatedAt)),
              playlist.regenerateOnPlay ? t.regenerateOnPlay : null,
            ]
              .filter(Boolean)
              .join(' · ')}
          </span>
        )}
      </div>
      <div className={styles.actions}>
        <Button
          variant="primary"
          size="lg"
          disabled={contents.itemCount === 0 && !smart}
          onClick={() => void playPlaylist(playlist.id, playlist.name)}
        >
          <PlayIcon size={14} />
          {t.play}
        </Button>
        {resume && (
          <Button size="lg" onClick={() => void playPlaylist(playlist.id, playlist.name, true)}>
            {t.resume(`${formatCount(resume.slotIndex + 1)} / ${formatCount(contents.itemCount)}`)}
          </Button>
        )}
        {smart && (
          <Button size="lg" onClick={() => void regenerate(playlist.id)}>
            <ReshuffleIcon size={15} strokeWidth={2} />
            {t.regenerate}
          </Button>
        )}
        <button type="button" className={styles.more} aria-label={t.more} onClick={more}>
          <MoreIcon size={16} />
        </button>
      </div>
    </header>
  );
}
