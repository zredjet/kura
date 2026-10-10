import { useId, useMemo, useState } from 'react';

import { Button } from '../../../components/Button/Button';
import { InlineName } from '../../../components/InlineName/InlineName';
import { RatingInput } from '../../../components/RatingInput/RatingInput';
import { Select } from '../../../components/Select/Select';
import { Switch } from '../../../components/Switch/Switch';
import { CloseIcon } from '../../../components/icons/CloseIcon';
import { FolderIcon } from '../../../components/icons/FolderIcon';
import { Waveform } from '../../../components/icons/Waveform';
import { ja } from '../../../i18n/ja';
import { unitEntry } from '../../../ipc/items';
import { thumbUrl } from '../../../ipc/media';
import { getUnitDetail, type OrderModeDto, type UnitFolderDto } from '../../../ipc/units';
import { formatCount, formatDuration } from '../../../lib/format';
import { useViewStore } from '../../../stores/viewStore';
import { TagChips } from '../../tags/TagChips';
import { TagField } from '../../tags/TagField';
import {
  changeUnitTags,
  dissolveUnits,
  pickRefFolder,
  setUnitFolderRefs,
  updateUnitWith,
} from '../actions';
import styles from './Inspector.module.css';
import { TAG_FOCUS_KEY } from './ItemInspector';
import { NoteField } from './NoteField';
import { useLoaded } from './useLoaded';

const t = ja.unitInspector;

/** 画像の既定の表示時間（設定画面は M9。docs/playlists.md §5.3）。 */
const DEFAULT_IMAGE_SECONDS = 8;

const ORDER_MODES: OrderModeDto[] = ['manual', 'name', 'imported', 'captured', 'duration'];

/**
 * ユニットのインスペクタ（design.md §3.1）：表紙、名前、種類、件数・合計時間、並び順、
 * ユニット内シャッフル、画像の表示時間、評価、タグ、参照フォルダ（フォルダ群のとき）、メモ。
 */
export function UnitInspector({ unitId, opened }: { unitId: number; opened: boolean }) {
  const detail = useLoaded(`unit-${unitId}`, () => getUnitDetail(unitId));
  const openUnit = useViewStore((s) => s.openUnit);
  const [editingName, setEditingName] = useState(false);
  const durationId = useId();
  const tagIds = useMemo(() => new Set(detail?.tags.map((tag) => tag.id) ?? []), [detail]);

  if (!detail) {
    return null;
  }
  const unit = detail.unit;
  const cover = unit.cover;
  const kindLabel = unit.kind === 'folder' ? t.folder : t.set;
  const modes = unit.kind === 'folder' ? ORDER_MODES.filter((m) => m !== 'manual') : ORDER_MODES;
  const setFolders = (folders: UnitFolderDto[]) => void setUnitFolderRefs(unitId, folders);

  return (
    <>
      <div className={styles.preview}>
        {cover?.thumbState === 'ready' && (
          <img className={styles.previewImage} src={thumbUrl(cover.hash, 'l')} alt="" />
        )}
        {cover?.kind === 'audio' && cover.thumbState !== 'ready' && (
          <Waveform className={styles.previewWave} />
        )}
      </div>

      <div className={styles.head}>
        {editingName ? (
          <InlineName
            initial={unit.name}
            label={t.name}
            className={styles.titleInput}
            onCommit={(name) => {
              setEditingName(false);
              void updateUnitWith(unitId, { name });
            }}
            onCancel={() => setEditingName(false)}
          />
        ) : (
          <h2 className={styles.title}>
            <button
              type="button"
              className={styles.titleButton}
              aria-label={ja.inspector.editTitle(unit.name)}
              onClick={() => setEditingName(true)}
            >
              {unit.name}
            </button>
          </h2>
        )}
        <p className={styles.meta}>
          {t.meta(kindLabel, formatCount(unit.count), formatDuration(unit.durationMs))}
        </p>
        <RatingInput
          value={unit.ownRating}
          label={ja.inspector.ratingGroup}
          starLabel={ja.inspector.setRating}
          onChange={(rating) => void updateUnitWith(unitId, { rating })}
        />
      </div>

      <section className={styles.section}>
        <label className={styles.fieldRow}>
          <span className={styles.fieldLabel}>{t.orderMode}</span>
          <Select
            value={detail.orderMode}
            options={modes.map((m) => ({ value: m, label: t.orderModes[m] }))}
            onChange={(value) => void updateUnitWith(unitId, { orderMode: value as OrderModeDto })}
          />
        </label>
        <label className={styles.fieldRow}>
          <span className={styles.fieldLabel}>{t.innerShuffle}</span>
          <Switch
            checked={detail.innerShuffle}
            onChange={(innerShuffle) => void updateUnitWith(unitId, { innerShuffle })}
          />
        </label>
        <div className={styles.fieldRow}>
          <label htmlFor={durationId} className={styles.fieldLabel}>
            {t.imageDuration}
          </label>
          <input
            key={detail.imageDurationMs ?? 'default'}
            id={durationId}
            type="number"
            min={1}
            step={1}
            className={styles.numberInput}
            defaultValue={
              detail.imageDurationMs === null ? '' : Math.round(detail.imageDurationMs / 1000)
            }
            placeholder={t.imageDurationDefault(t.seconds(String(DEFAULT_IMAGE_SECONDS)))}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === 'Enter') {
                e.currentTarget.blur();
              }
            }}
            onBlur={(e) => {
              const seconds = Number(e.currentTarget.value);
              const ms =
                e.currentTarget.value === '' || !(seconds > 0) ? 0 : Math.round(seconds * 1000);
              if (ms !== (detail.imageDurationMs ?? 0)) {
                void updateUnitWith(unitId, { imageDurationMs: ms });
              }
            }}
          />
        </div>
      </section>

      <section className={styles.section}>
        <div className={styles.sectionHead}>
          <h3 className={styles.label}>{ja.inspector.tags}</h3>
          <kbd className={styles.kbd}>T</kbd>
        </div>
        <TagChips
          tags={detail.tags}
          onRemove={(tag) => void changeUnitTags(unitId, [], [tag.id])}
        />
        <TagField
          placeholder={ja.inspector.addTag}
          label={ja.inspector.addTagLabel}
          exclude={tagIds}
          focusKey={TAG_FOCUS_KEY}
          onAdd={(name) => void changeUnitTags(unitId, [name], [])}
        />
      </section>

      {unit.kind === 'folder' && (
        <section className={styles.section}>
          <h3 className={styles.label}>{t.folders}</h3>
          <ul className={styles.list}>
            {detail.folders.map((folder) => {
              const path = (folder.path ?? []).join(' / ');
              return (
                <li key={folder.folderId} className={styles.refFolder}>
                  <span className={styles.folder}>
                    <FolderIcon size={14} strokeWidth={1.8} />
                    <span className={styles.folderPath}>{path}</span>
                    <button
                      type="button"
                      className={styles.remove}
                      aria-label={t.removeFolder(path)}
                      disabled={detail.folders.length <= 1}
                      onClick={() =>
                        setFolders(detail.folders.filter((f) => f.folderId !== folder.folderId))
                      }
                    >
                      <CloseIcon size={9} strokeWidth={3} />
                    </button>
                  </span>
                  <label className={styles.fieldRow}>
                    <span className={styles.sub}>{t.recursive}</span>
                    <Switch
                      checked={folder.recursive}
                      onChange={(recursive) =>
                        setFolders(
                          detail.folders.map((f) =>
                            f.folderId === folder.folderId ? { ...f, recursive } : f,
                          ),
                        )
                      }
                    />
                  </label>
                </li>
              );
            })}
          </ul>
          <button
            type="button"
            className={styles.addLink}
            onClick={() => void pickRefFolder(unitId, detail.folders)}
          >
            {t.addFolder}
          </button>
        </section>
      )}

      <NoteField
        key={`${unitId}-${detail.note ?? ''}`}
        initial={detail.note ?? ''}
        placeholder={ja.inspector.notePlaceholder}
        onSave={(note) => void updateUnitWith(unitId, { note })}
      />

      <div className={styles.unitActions}>
        {!opened && (
          <Button variant="primary" onClick={() => openUnit(unitId)}>
            {t.open}
          </Button>
        )}
        <Button onClick={() => void dissolveUnits(opened ? [] : [unitEntry(unitId)])}>
          {t.dissolve}
        </Button>
      </div>
    </>
  );
}
