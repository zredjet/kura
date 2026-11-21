import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { askConfirm } from '../../components/Overlay/overlayStore';
import { Segmented } from '../../components/Segmented/Segmented';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import {
  type AttributionDto,
  createLicense,
  deleteLicense,
  type LicenseDto,
  type LicenseInputDto,
  type PermissionDto,
  updateLicense,
} from '../../ipc/credits';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';
import { bumpRevision } from '../../stores/revisionStore';
import styles from '../export/Export.module.css';
import { licenseFlags } from './CreditSection';
import { useLicenseStore } from './licenseStore';

const t = ja.licenses;

const ATTRIBUTIONS: readonly AttributionDto[] = ['required', 'optional', 'none'];
const PERMISSIONS: readonly PermissionDto[] = ['yes', 'no', 'unknown'];

const EMPTY: LicenseInputDto = {
  name: '',
  spdx: null,
  url: null,
  attribution: 'required',
  commercial: 'unknown',
  modify: 'unknown',
  shareAlike: false,
  note: null,
};

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function inputOf(license: LicenseDto): LicenseInputDto {
  return {
    name: license.name,
    spdx: license.spdx,
    url: license.url,
    attribution: license.attribution,
    commercial: license.commercial,
    modify: license.modify,
    shareAlike: license.shareAlike,
    note: license.note,
  };
}

/**
 * 設定の「ライセンス」タブ（M15 段 6。design.md §3.1）：既定の 13 行（変えられない）と利用者のライセンスの一覧。足す・編集・
 * 削除（付けている項目は未設定に戻る。その数を確かめてから）。
 */
export function LicensesTab() {
  const licenses = useLicenseStore((s) => s.list);
  const [editing, setEditing] = useState<LicenseDto | 'new' | null>(null);

  useEffect(() => {
    void useLicenseStore.getState().load();
  }, []);

  const remove = async (license: LicenseDto) => {
    const ok = await askConfirm({
      title: t.deleteTitle,
      message: t.deleteMessage(license.name, formatCount(license.items)),
      confirm: t.deleteConfirm,
    });
    if (!ok) return;
    try {
      const n = await deleteLicense(license.id);
      notify(t.deleted(formatCount(n)));
      bumpRevision();
      await useLicenseStore.getState().load();
    } catch (e) {
      notify(messageOf(e));
    }
  };

  return (
    <>
      <p className={styles.hint}>{t.hint}</p>
      <div className={styles.group}>
        <ul className={styles.licenseList}>
          {licenses.map((license) => (
            <li key={license.id} className={styles.licenseRow}>
              <span className={styles.licenseName}>
                {license.name}
                {license.builtin !== null && <span className={styles.badge}>{t.builtin}</span>}
                <span className={styles.hint}>
                  {[...licenseFlags(license), t.items(formatCount(license.items))].join(' · ')}
                </span>
              </span>
              {license.builtin === null && (
                <span className={styles.licenseButtons}>
                  <Button size="sm" onClick={() => setEditing(license)}>
                    {t.edit}
                  </Button>
                  <Button size="sm" onClick={() => void remove(license)}>
                    {t.delete}
                  </Button>
                </span>
              )}
            </li>
          ))}
        </ul>
      </div>
      <div>
        <Button size="sm" onClick={() => setEditing('new')}>
          {t.add}
        </Button>
      </div>
      {editing !== null && (
        <LicenseSheet
          license={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
        />
      )}
    </>
  );
}

/** 足す・編集のシート（名前・SPDX・URL・表記・商用・改変・継承・メモ）。 */
function LicenseSheet({ license, onClose }: { license: LicenseDto | null; onClose: () => void }) {
  const titleId = useId();
  const nameId = useId();
  const spdxId = useId();
  const urlId = useId();
  const noteId = useId();
  const [input, setInput] = useState<LicenseInputDto>(license ? inputOf(license) : EMPTY);
  const [busy, setBusy] = useState(false);
  const text = (v: string) => (v.trim() === '' ? null : v);

  const save = async () => {
    setBusy(true);
    try {
      const value = { ...input, name: input.name.trim() };
      if (license) {
        await updateLicense(license.id, value);
      } else {
        await createLicense(value);
      }
      bumpRevision();
      await useLicenseStore.getState().load();
      onClose();
    } catch (e) {
      notify(messageOf(e));
      setBusy(false);
    }
  };

  return (
    <Dialog labelledBy={titleId} onClose={onClose}>
      <div className={styles.head}>
        <h2 id={titleId} className={styles.title}>
          {license ? t.editTitle(license.name) : t.newTitle}
        </h2>
      </div>
      <div className={styles.body}>
        <div className={styles.group}>
          <label className={styles.field} htmlFor={nameId}>
            <span className={styles.rowLabel}>{t.name}</span>
            <input
              id={nameId}
              className={styles.input}
              value={input.name}
              autoComplete="off"
              onChange={(e) => setInput({ ...input, name: e.target.value })}
            />
          </label>
          <label className={styles.field} htmlFor={spdxId}>
            <span className={styles.rowLabel}>{t.spdx}</span>
            <input
              id={spdxId}
              className={styles.input}
              value={input.spdx ?? ''}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setInput({ ...input, spdx: text(e.target.value) })}
            />
          </label>
          <label className={styles.field} htmlFor={urlId}>
            <span className={styles.rowLabel}>{t.url}</span>
            <input
              id={urlId}
              className={styles.input}
              value={input.url ?? ''}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setInput({ ...input, url: text(e.target.value) })}
            />
          </label>
        </div>
        <div className={styles.group}>
          <div className={styles.row}>
            <span className={styles.rowLabel}>{t.attribution}</span>
            <Segmented<AttributionDto>
              label={t.attribution}
              value={input.attribution}
              options={ATTRIBUTIONS.map((a) => ({ value: a, label: ja.credit.attribution[a] }))}
              onChange={(attribution) => setInput({ ...input, attribution })}
            />
          </div>
          <div className={styles.row}>
            <span className={styles.rowLabel}>{t.commercial}</span>
            <Segmented<PermissionDto>
              label={t.commercial}
              value={input.commercial}
              options={PERMISSIONS.map((p) => ({ value: p, label: t.permission[p] }))}
              onChange={(commercial) => setInput({ ...input, commercial })}
            />
          </div>
          <div className={styles.row}>
            <span className={styles.rowLabel}>{t.modify}</span>
            <Segmented<PermissionDto>
              label={t.modify}
              value={input.modify}
              options={PERMISSIONS.map((p) => ({ value: p, label: t.permission[p] }))}
              onChange={(modify) => setInput({ ...input, modify })}
            />
          </div>
          <label className={styles.row}>
            <span className={styles.rowLabel}>{t.shareAlike}</span>
            <Switch
              checked={input.shareAlike}
              onChange={(shareAlike) => setInput({ ...input, shareAlike })}
            />
          </label>
        </div>
        <div className={styles.group}>
          <label className={styles.field} htmlFor={noteId}>
            <span className={styles.rowLabel}>{t.note}</span>
            <input
              id={noteId}
              className={styles.input}
              value={input.note ?? ''}
              autoComplete="off"
              onChange={(e) => setInput({ ...input, note: text(e.target.value) })}
            />
          </label>
        </div>
      </div>
      <div className={styles.actions}>
        <Button onClick={onClose}>{ja.common.cancel}</Button>
        <Button
          variant="primary"
          disabled={busy || input.name.trim() === ''}
          onClick={() => void save()}
        >
          {t.save}
        </Button>
      </div>
    </Dialog>
  );
}
