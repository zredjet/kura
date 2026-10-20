import { useMemo, useState } from 'react';

import { Button } from '../../../components/Button/Button';
import { InlineName } from '../../../components/InlineName/InlineName';
import { openMenuBelow } from '../../../components/Menu/menuStore';
import { RatingInput } from '../../../components/RatingInput/RatingInput';
import { CloseIcon } from '../../../components/icons/CloseIcon';
import { FolderIcon } from '../../../components/icons/FolderIcon';
import { LinkIcon } from '../../../components/icons/LinkIcon';
import { MoreIcon } from '../../../components/icons/MoreIcon';
import { UnitIcon } from '../../../components/icons/UnitIcon';
import { KindArt } from '../../../components/KindArt/KindArt';
import { ja } from '../../../i18n/ja';
import { getItemDetail } from '../../../ipc/items';
import { isPlayable } from '../../../ipc/kinds';
import { thumbUrl } from '../../../ipc/media';
import { dateTimeLabel } from '../../../lib/dates';
import { shortHash } from '../../../lib/format';
import { useViewStore } from '../../../stores/viewStore';
import { AssetSection } from '../../asset/AssetSection';
import { useAssetStage } from '../../asset/AssetStage';
import { useAssetData } from '../../asset/useAssetData';
import { TagChips } from '../../tags/TagChips';
import { TagField } from '../../tags/TagField';
import {
  changeTags,
  pickFolderFor,
  removeFromFolder,
  restoreEntries,
  setNote,
  setRating,
  setTitle,
} from '../actions';
import { itemMenu } from '../itemMenu';
import { itemMeta } from '../describeItem';
import styles from './Inspector.module.css';
import { NoteField } from './NoteField';
import { useLoaded } from './useLoaded';

const t = ja.inspector;

/** T キーで移るタグの入力欄の印。 */
export const TAG_FOCUS_KEY = 'inspector-tags';

/** 1 件のアイテムの詳細と編集（Main.dc.html のインスペクタ）。 */
export function ItemInspector({ id }: { id: number }) {
  const detail = useLoaded(`item-${id}`, () => getItemDetail(id));
  const openUnit = useViewStore((s) => s.openUnit);
  const [editingTitle, setEditingTitle] = useState(false);
  const tagIds = useMemo(() => new Set(detail?.tags.map((tag) => tag.id) ?? []), [detail]);
  // 素材の欄（M11）：アニメ・フォントの見本はプレビューの枠を置き換える
  const assetData = useAssetData(id);
  const assetStage = useAssetStage(assetData);

  if (!detail) {
    return null;
  }
  const item = detail.item;
  const source = detail.sourceUrl ?? detail.originalPath ?? t.local;
  const trashed = detail.trashedAt !== null;
  return (
    <>
      <div className={styles.preview} data-replaced={assetStage ? 'true' : undefined}>
        {item.thumbState === 'ready' ? (
          <img className={styles.previewImage} src={thumbUrl(item.hash, 'l')} alt="" />
        ) : !isPlayable(item.kind) ? (
          <KindArt kind={item.kind} />
        ) : (
          <span className={styles.previewNote}>
            {item.thumbState === 'pending' ? t.noThumbnail : ''}
          </span>
        )}
      </div>
      {assetStage}

      <div className={styles.head}>
        {editingTitle ? (
          <InlineName
            initial={item.title}
            label={t.titleLabel}
            className={styles.titleInput}
            onCommit={(title) => {
              setEditingTitle(false);
              void setTitle(id, title);
            }}
            onCancel={() => setEditingTitle(false)}
          />
        ) : (
          <h2 className={styles.title}>
            <button
              type="button"
              className={styles.titleButton}
              aria-label={t.editTitle(item.title)}
              onClick={() => setEditingTitle(true)}
            >
              {item.title}
            </button>
          </h2>
        )}
        <p className={styles.meta}>{itemMeta(item)}</p>
        {!trashed && (
          <RatingInput
            value={item.rating}
            label={t.ratingGroup}
            starLabel={t.setRating}
            onChange={(rating) => void setRating([id], rating)}
          />
        )}
      </div>

      <AssetSection data={assetData} item={item} />

      {trashed && (
        <section className={styles.trashed}>
          <p className={styles.sub}>{t.trashed(dateTimeLabel(detail.trashedAt ?? 0))}</p>
          <Button size="sm" onClick={() => void restoreEntries([id])}>
            {t.restore}
          </Button>
        </section>
      )}

      <section className={styles.section}>
        <div className={styles.sectionHead}>
          <h3 className={styles.label}>{t.tags}</h3>
          <kbd className={styles.kbd}>T</kbd>
        </div>
        <TagChips tags={detail.tags} onRemove={(tag) => void changeTags([id], [], [tag.id])} />
        <TagField
          placeholder={t.addTag}
          label={t.addTagLabel}
          exclude={tagIds}
          focusKey={TAG_FOCUS_KEY}
          onAdd={(name) => void changeTags([id], [name], [])}
        />
      </section>

      <section className={styles.section}>
        <div className={styles.sectionHead}>
          <h3 className={styles.label}>{t.folders}</h3>
          <kbd className={styles.kbd}>F</kbd>
        </div>
        {detail.folders.length > 0 && (
          <ul className={styles.list}>
            {detail.folders.map((folder) => {
              const path = folder.path.join(' / ');
              return (
                <li key={folder.id} className={styles.folder}>
                  <FolderIcon size={14} strokeWidth={1.8} />
                  <span className={styles.folderPath}>{path}</span>
                  <button
                    type="button"
                    className={styles.remove}
                    aria-label={t.removeFolder(path)}
                    onClick={() => void removeFromFolder([id], folder.id)}
                  >
                    <CloseIcon size={9} strokeWidth={3} />
                  </button>
                </li>
              );
            })}
          </ul>
        )}
        <button type="button" className={styles.addLink} onClick={() => void pickFolderFor([id])}>
          {t.addFolder}
        </button>
      </section>

      {detail.units.length > 0 && (
        <section className={styles.section}>
          <h3 className={styles.label}>{t.units}</h3>
          <ul className={styles.list}>
            {detail.units.map((unit) => (
              <li key={unit.id}>
                <button
                  type="button"
                  className={styles.unitLink}
                  aria-label={t.openUnit(unit.name)}
                  onClick={() => openUnit(unit.id)}
                >
                  <UnitIcon size={14} strokeWidth={1.8} />
                  {unit.name}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className={styles.section}>
        <h3 className={styles.label}>{t.source}</h3>
        <p className={styles.source}>
          <LinkIcon size={14} strokeWidth={1.8} />
          <span className={styles.value}>{source}</span>
        </p>
        <p className={styles.sub}>{t.imported(dateTimeLabel(item.importedAt))}</p>
      </section>

      <NoteField
        key={`${id}-${detail.note ?? ''}`}
        initial={detail.note ?? ''}
        placeholder={t.notePlaceholder}
        onSave={(note) => void setNote([id], note)}
      />

      <dl className={styles.info}>
        <dt>{t.hash}</dt>
        <dd className={styles.mono}>{shortHash(item.hash)}</dd>
      </dl>

      <div className={styles.actions}>
        <button
          type="button"
          className={styles.moreButton}
          aria-label={t.more}
          aria-haspopup="menu"
          onClick={(e) => openMenuBelow(e.currentTarget, ja.itemMenu.label, itemMenu([id]))}
        >
          <MoreIcon size={16} />
        </button>
      </div>
    </>
  );
}
