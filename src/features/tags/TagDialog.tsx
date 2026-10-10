import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { askConfirm, askPick } from '../../components/Overlay/overlayStore';
import overlay from '../../components/Overlay/Overlays.module.css';
import { TagInput } from '../../components/TagInput/TagInput';
import { ja } from '../../i18n/ja';
import {
  deleteTag,
  mergeTag,
  suggestTags,
  tagDetail,
  type TagDetailDto,
  tagLabel,
  type TagRefDto,
  updateTag,
} from '../../ipc/tags';
import { formatCount } from '../../lib/format';
import { invalidateItems, invalidateUnits } from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import { bumpRevision } from '../../stores/revisionStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { attempt } from '../library/actions';
import { TagChips } from './TagChips';
import { closeTagDialog, useTagDialogStore } from './tagDialogStore';
import { TagField } from './TagField';
import styles from './TagDialog.module.css';

const t = ja.tagDialog;
const COLORS = ['blue', 'violet', 'amber', 'teal'] as const;

/** タグが変わったあとに読み直す（名前はタイル以外に出るので、インスペクタとサイドバーだけ）。 */
async function afterTagChange(listChanged: boolean): Promise<void> {
  invalidateItems([]);
  invalidateUnits();
  bumpRevision();
  await useSidebarStore.getState().load();
  if (listChanged) {
    await useLibraryStore.getState().loadList();
  }
}

/** タグの編集（名前空間・名前・色・別名・親タグ、統合・削除）。モックにない画面なので design.md §4 の部品で作る。 */
export function TagDialog() {
  const tagId = useTagDialogStore((s) => s.tagId);
  const [detail, setDetail] = useState<TagDetailDto | null>(null);

  useEffect(() => {
    if (tagId === null) {
      return;
    }
    let alive = true;
    tagDetail(tagId)
      .then((d) => alive && setDetail(d))
      .catch((error: unknown) => {
        console.warn('タグを読めませんでした', error);
        closeTagDialog();
      });
    return () => {
      alive = false;
    };
  }, [tagId]);

  if (tagId === null || detail?.tag.id !== tagId) {
    return null;
  }
  return <TagEditor key={tagId} detail={detail} />;
}

function TagEditor({ detail }: { detail: TagDetailDto }) {
  const titleId = useId();
  const nsId = useId();
  const nameId = useId();
  const [namespace, setNamespace] = useState(detail.tag.namespace);
  const [name, setName] = useState(detail.tag.name);
  const [color, setColor] = useState<string | null>(detail.tag.color);
  const [aliases, setAliases] = useState<string[]>(detail.aliases);
  const [parents, setParents] = useState<TagRefDto[]>(detail.parents);
  const label = tagLabel(detail.tag);

  const save = () =>
    void attempt(async () => {
      await updateTag(detail.tag.id, {
        namespace,
        name,
        color: color ?? '',
        aliases,
        parents: parents.map((p) => p.id),
      });
      closeTagDialog();
      await afterTagChange(false);
    });

  const merge = () =>
    void attempt(async () => {
      closeTagDialog();
      const picked = await askPick<TagRefDto>({
        title: t.mergeTitle(label),
        placeholder: t.mergePlaceholder,
        empty: t.noTag,
        search: async (q) =>
          (await suggestTags(q, 20))
            .filter((s) => s.tag.id !== detail.tag.id)
            .map((s) => ({
              key: `tag-${s.tag.id}`,
              label: tagLabel(s.tag),
              detail: ja.tagDialog.usage(formatCount(s.count)),
              value: s.tag,
            })),
      });
      if (!picked) {
        return;
      }
      const into = tagLabel(picked.value);
      const ok = await askConfirm({
        title: t.mergeConfirmTitle,
        message: t.mergeMessage(label, into),
        confirm: t.mergeConfirm,
      });
      if (!ok) {
        return;
      }
      await mergeTag(detail.tag.id, picked.value.id);
      notify(ja.notice.merged(label, into));
      await afterTagChange(false);
    });

  const remove = () =>
    void attempt(async () => {
      closeTagDialog();
      const ok = await askConfirm({
        title: t.deleteTitle,
        message: t.deleteMessage(label, formatCount(detail.count)),
        confirm: t.deleteConfirm,
      });
      if (!ok) {
        return;
      }
      await deleteTag(detail.tag.id);
      await afterTagChange(true);
    });

  const parentIds = new Set([detail.tag.id, ...parents.map((p) => p.id)]);
  return (
    <Dialog labelledBy={titleId} onClose={closeTagDialog} size="sm">
      <header className={styles.header}>
        <h2 id={titleId} className={overlay.title}>
          {t.title}
        </h2>
        <p className={styles.usage}>{t.usage(formatCount(detail.count))}</p>
      </header>

      <div className={styles.grid}>
        <div className={overlay.field}>
          <label htmlFor={nsId} className={overlay.label}>
            {t.namespace}
          </label>
          <input
            id={nsId}
            className={overlay.input}
            value={namespace}
            placeholder={t.namespaceHint}
            onChange={(e) => setNamespace(e.target.value)}
          />
        </div>
        <div className={overlay.field}>
          <label htmlFor={nameId} className={overlay.label}>
            {t.name}
          </label>
          <input
            id={nameId}
            className={overlay.input}
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </div>
      </div>

      <div className={overlay.field} role="group" aria-labelledby={`${titleId}-color`}>
        <span id={`${titleId}-color`} className={overlay.label}>
          {t.color}
        </span>
        <div className={styles.swatches}>
          {[null, ...COLORS].map((c) => (
            <button
              key={c ?? 'none'}
              type="button"
              className={styles.swatch}
              data-color={c ?? undefined}
              aria-label={ja.colors[c ?? 'none']}
              aria-pressed={color === c}
              onClick={() => setColor(c)}
            />
          ))}
        </div>
      </div>

      <div className={overlay.field}>
        <span className={overlay.label}>{t.aliases}</span>
        <TagInput
          tags={aliases}
          onChange={setAliases}
          placeholder={t.addAlias}
          inputLabel={t.addAlias}
          removeLabel={t.remove}
        />
        <p className={styles.hint}>{t.aliasesHint}</p>
      </div>

      <div className={overlay.field}>
        <span className={overlay.label}>{t.parents}</span>
        <TagChips
          tags={parents}
          onRemove={(tag) => setParents(parents.filter((p) => p.id !== tag.id))}
        />
        <TagField
          placeholder={t.addParent}
          label={t.addParent}
          existingOnly
          exclude={parentIds}
          onAdd={(_, tag) => {
            if (tag && !parentIds.has(tag.id)) {
              setParents([...parents, tag]);
            }
          }}
        />
        <p className={styles.hint}>{t.parentsHint}</p>
      </div>

      <footer className={styles.footer}>
        <div className={styles.secondary}>
          <Button size="sm" onClick={merge}>
            {t.merge}
          </Button>
          <Button size="sm" onClick={remove}>
            {t.delete}
          </Button>
        </div>
        <div className={overlay.actions}>
          <Button onClick={closeTagDialog}>{ja.common.cancel}</Button>
          <Button variant="primary" disabled={!name.trim()} onClick={save}>
            {ja.common.save}
          </Button>
        </div>
      </footer>
    </Dialog>
  );
}
