import { useMemo } from 'react';

import { openMenuBelow } from '../../../components/Menu/menuStore';
import { RatingInput } from '../../../components/RatingInput/RatingInput';
import { CloseIcon } from '../../../components/icons/CloseIcon';
import { FolderIcon } from '../../../components/icons/FolderIcon';
import { MoreIcon } from '../../../components/icons/MoreIcon';
import { ja } from '../../../i18n/ja';
import { isUnitEntry, selectionInfo, unitIdOf } from '../../../ipc/items';
import { thumbUrl } from '../../../ipc/media';
import { tagLabel } from '../../../ipc/tags';
import { formatCount } from '../../../lib/format';
import { getCached, getCachedUnit } from '../../../stores/itemCache';
import { useViewStore } from '../../../stores/viewStore';
import { TagChips } from '../../tags/TagChips';
import { TagField } from '../../tags/TagField';
import { splitTags } from '../../tags/splitTags';
import {
  changeTags,
  expandEntries,
  pickFolderFor,
  removeFromFolder,
  setNote,
  setRating,
} from '../actions';
import { itemMenu } from '../itemMenu';
import styles from './Inspector.module.css';
import { TAG_FOCUS_KEY } from './ItemInspector';
import { NoteField } from './NoteField';
import { useLoaded } from './useLoaded';

const t = ja.inspector;

/** 重ねて見せるサムネイル（最初の 3 件）。 */
function thumbOf(entry: number): string | null {
  if (isUnitEntry(entry)) {
    const cover = getCachedUnit(unitIdOf(entry))?.cover;
    return cover?.thumbState === 'ready' ? thumbUrl(cover.hash, 's') : null;
  }
  const item = getCached(entry);
  return item?.thumbState === 'ready' ? thumbUrl(item.hash, 's') : null;
}

/**
 * 複数選択のインスペクタ。評価・タグ・フォルダ・メモをまとめて変える（タイトルは 1 件ずつ）。
 * ユニットのタイルは、ユニット自体とメンバー全員に付ける（docs/playlists.md §2.1）。
 */
export function MultiInspector({ entries }: { entries: number[] }) {
  const key = useMemo(() => entries.join(','), [entries]);
  const trash = useViewStore((s) => s.view.kind === 'trash');
  const data = useLoaded(`multi-${key}`, async () => {
    const ids = await expandEntries(entries);
    return { ids, info: await selectionInfo(ids) };
  });
  const { full, partial } = useMemo(
    () => (data ? splitTags(data.info.tags, data.info.count) : { full: [], partial: [] }),
    [data],
  );
  const tagIds = useMemo(() => new Set(full.map((tag) => tag.id)), [full]);

  const thumbs = entries.slice(0, 3).map(thumbOf);
  return (
    <>
      <div className={styles.stack} aria-hidden="true">
        {thumbs.map((src, i) => (
          <span key={i} className={styles.stackCard} data-index={i}>
            {src && <img src={src} alt="" className={styles.stackImage} />}
          </span>
        ))}
      </div>

      <div className={styles.head}>
        <h2 className={styles.title}>{t.selected(formatCount(entries.length))}</h2>
        {data && data.info.count !== entries.length && (
          <p className={styles.meta}>{ja.library.itemCount(formatCount(data.info.count))}</p>
        )}
        {data && !trash && (
          <RatingInput
            value={data.info.rating}
            label={t.ratingGroup}
            starLabel={t.setRating}
            onChange={(rating) => void setRating(entries, rating)}
          />
        )}
        {data && data.info.rating === null && <p className={styles.sub}>{t.mixed}</p>}
      </div>

      {data && (
        <>
          <section className={styles.section}>
            <div className={styles.sectionHead}>
              <h3 className={styles.label}>{t.tags}</h3>
              <kbd className={styles.kbd}>T</kbd>
            </div>
            <TagChips
              tags={full}
              partial={partial}
              total={data.info.count}
              onRemove={(tag) => void changeTags(entries, [], [tag.id])}
              onApplyAll={(tag) => void changeTags(entries, [tagLabel(tag)], [])}
            />
            <TagField
              placeholder={t.addTag}
              label={t.addTagLabel}
              exclude={tagIds}
              focusKey={TAG_FOCUS_KEY}
              onAdd={(name) => void changeTags(entries, [name], [])}
            />
          </section>

          <section className={styles.section}>
            <div className={styles.sectionHead}>
              <h3 className={styles.label}>{t.folders}</h3>
              <kbd className={styles.kbd}>F</kbd>
            </div>
            {data.info.folders.length > 0 && (
              <ul className={styles.list}>
                {data.info.folders.map(({ folder, count }) => {
                  const path = folder.path.join(' / ');
                  return (
                    <li key={folder.id} className={styles.folder}>
                      <FolderIcon size={14} strokeWidth={1.8} />
                      <span className={styles.folderPath}>{path}</span>
                      {count < data.info.count && (
                        <span className={styles.partialCount}>
                          {count}/{data.info.count}
                        </span>
                      )}
                      <button
                        type="button"
                        className={styles.remove}
                        aria-label={t.removeFolder(path)}
                        onClick={() => void removeFromFolder(data.ids, folder.id)}
                      >
                        <CloseIcon size={9} strokeWidth={3} />
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
            <button
              type="button"
              className={styles.addLink}
              onClick={() => void pickFolderFor(data.ids)}
            >
              {t.addFolder}
            </button>
          </section>

          <NoteField
            key={`${key}-${data.info.sameNote ? (data.info.note ?? '') : '*'}`}
            initial={data.info.sameNote ? (data.info.note ?? '') : ''}
            placeholder={data.info.sameNote ? t.notePlaceholder : t.mixed}
            onSave={(note) => void setNote(entries, note)}
          />
        </>
      )}

      <div className={styles.actions}>
        <button
          type="button"
          className={styles.moreButton}
          aria-label={t.more}
          aria-haspopup="menu"
          onClick={(e) => openMenuBelow(e.currentTarget, ja.itemMenu.label, itemMenu(entries))}
        >
          <MoreIcon size={16} />
        </button>
      </div>
    </>
  );
}
