import { useEffect, useState } from 'react';

import glass from '../../components/glass/glass.module.css';
import { FolderIcon } from '../../components/icons/FolderIcon';
import { StarIcon } from '../../components/icons/StarIcon';
import { ja } from '../../i18n/ja';
import { getItemDetail, type ItemDetailDto } from '../../ipc/items';
import { thumbUrl } from '../../ipc/media';
import { dateTimeLabel } from '../../lib/dates';
import { shortHash } from '../../lib/format';
import { useViewStore } from '../../stores/viewStore';
import { itemMeta } from './describeItem';
import styles from './Inspector.module.css';

const t = ja.inspector;

/** 選択中のアイテムの詳細（Main.dc.html のインスペクタ。M2 は表示だけ、編集は M3）。 */
export function Inspector() {
  const selectedId = useViewStore((s) => s.selectedId);
  const [detail, setDetail] = useState<ItemDetailDto | null>(null);

  useEffect(() => {
    if (selectedId === null) {
      return;
    }
    let alive = true;
    getItemDetail(selectedId)
      .then((d) => alive && setDetail(d))
      .catch((error: unknown) => console.warn('詳細を取得できませんでした', error));
    return () => {
      alive = false;
    };
  }, [selectedId]);

  const shown = selectedId !== null && detail?.item.id === selectedId ? detail : null;
  return (
    <aside className={`${glass.panel} ${styles.inspector}`} aria-label={ja.library.inspectorLabel}>
      {shown ? <Detail detail={shown} /> : <p className={styles.empty}>{t.empty}</p>}
    </aside>
  );
}

function Detail({ detail }: { detail: ItemDetailDto }) {
  const item = detail.item;
  const source = detail.sourceUrl ?? detail.originalPath ?? t.local;
  return (
    <>
      <div className={styles.preview}>
        {item.thumbState === 'ready' ? (
          <img className={styles.previewImage} src={thumbUrl(item.hash, 'l')} alt="" />
        ) : (
          <span className={styles.previewNote}>
            {item.thumbState === 'pending' ? t.noThumbnail : ''}
          </span>
        )}
      </div>

      <div className={styles.head}>
        <h2 className={styles.title}>{item.title}</h2>
        <p className={styles.meta}>{itemMeta(item)}</p>
        <div className={styles.stars} role="img" aria-label={t.rating(item.rating)}>
          {[1, 2, 3, 4, 5].map((n) => (
            <span key={n} className={n <= item.rating ? styles.starOn : styles.starOff}>
              <StarIcon size={16} strokeWidth={1.7} filled={n <= item.rating} />
            </span>
          ))}
        </div>
      </div>

      <section className={styles.section}>
        <h3 className={styles.label}>{t.tags}</h3>
        {detail.tags.length > 0 ? (
          <ul className={styles.tags}>
            {detail.tags.map((tag) => (
              <li key={tag.id} className={styles.tag}>
                {tag.namespace && <span className={styles.namespace}>{tag.namespace}</span>}
                {tag.name}
              </li>
            ))}
          </ul>
        ) : (
          <p className={styles.none}>{t.noTags}</p>
        )}
      </section>

      <section className={styles.section}>
        <h3 className={styles.label}>{t.folders}</h3>
        {detail.folders.length > 0 ? (
          <ul className={styles.list}>
            {detail.folders.map((folder) => (
              <li key={folder.id} className={styles.folder}>
                <FolderIcon size={14} strokeWidth={1.8} />
                {folder.path.join(' / ')}
              </li>
            ))}
          </ul>
        ) : (
          <p className={styles.none}>{t.noFolders}</p>
        )}
      </section>

      {detail.units.length > 0 && (
        <section className={styles.section}>
          <h3 className={styles.label}>{t.units}</h3>
          <ul className={styles.list}>
            {detail.units.map((unit) => (
              <li key={unit.id} className={styles.value}>
                {unit.name}
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className={styles.section}>
        <h3 className={styles.label}>{t.source}</h3>
        <p className={styles.value}>{source}</p>
        <p className={styles.sub}>{t.imported(dateTimeLabel(item.importedAt))}</p>
      </section>

      <section className={styles.section}>
        <h3 className={styles.label}>{t.note}</h3>
        <p className={detail.note ? styles.value : styles.none}>{detail.note ?? t.noNote}</p>
      </section>

      <dl className={styles.info}>
        <dt>{t.hash}</dt>
        <dd className={styles.mono}>{shortHash(item.hash)}</dd>
      </dl>
    </>
  );
}
