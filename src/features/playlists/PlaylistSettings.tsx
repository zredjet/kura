import type { MouseEvent, ReactNode } from 'react';

import { type MenuEntry, openMenuBelow } from '../../components/Menu/menuStore';
import { CheckIcon } from '../../components/icons/CheckIcon';
import { ChevronDownIcon } from '../../components/icons/ChevronDownIcon';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import type { PlaylistDto, PlaylistPatchDto } from '../../ipc/playlists';
import { updatePlaylistSettings } from './playlistActions';
import styles from './PlaylistView.module.css';

const t = ja.playlist;

/** 選ぶボタン（押すとメニュー）。 */
export function Choice<T extends string | number>({
  label,
  value,
  options,
  onChange,
  small = false,
}: {
  label: string;
  value: T;
  options: readonly { value: T; label: string }[];
  onChange: (value: T) => void;
  small?: boolean;
}) {
  const current = options.find((o) => o.value === value)?.label ?? String(value);
  const open = (e: MouseEvent<HTMLButtonElement>) => {
    e.currentTarget.focus();
    const entries: MenuEntry[] = options.map((o) => ({
      label: o.label,
      icon: o.value === value ? <CheckIcon size={12} strokeWidth={2.4} /> : null,
      onSelect: () => onChange(o.value),
    }));
    openMenuBelow(e.currentTarget, label, entries);
  };
  return (
    <button
      type="button"
      className={small ? `${styles.select} ${styles.selectSmall}` : styles.select}
      aria-label={`${label}：${current}`}
      aria-haspopup="menu"
      onClick={open}
    >
      <span>{current}</span>
      <ChevronDownIcon size={10} strokeWidth={2.6} />
    </button>
  );
}

const IMAGE_SECONDS = [3, 5, 8, 10, 15, 20, 30];

function OptionRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className={styles.option}>
      <span>{label}</span>
      {children}
    </div>
  );
}

function SwitchRow({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (on: boolean) => void;
}) {
  return (
    <label className={styles.option}>
      {label}
      <Switch checked={checked} onChange={onChange} />
    </label>
  );
}

/** 並び順の選び方（シャッフルと重み付けをまとめて 1 つに）。 */
type ManualOrder = 'playlist' | 'random' | 'weighted';

function manualOrder(p: PlaylistDto): ManualOrder {
  if (!p.shuffle) {
    return 'playlist';
  }
  return p.weightByRating ? 'weighted' : 'random';
}

function orderPatch(order: ManualOrder): PlaylistPatchDto {
  return {
    shuffle: order !== 'playlist',
    weightByRating: order === 'weighted' ? true : order === 'random' ? false : undefined,
  };
}

/**
 * 再生の設定（Playlist.dc.html のルールの下段と同じ並び）。手動のプレイリストでは単独で、
 * スマートではルールの下に出す。`children` はスマートだけの項目（上限など）。
 */
export function PlaylistOptions({
  playlist,
  onChange,
  children,
}: {
  playlist: PlaylistDto;
  onChange?: () => void;
  children?: ReactNode;
}) {
  const update = (patch: PlaylistPatchDto) => {
    void updatePlaylistSettings(playlist.id, patch).then((ok) => {
      if (ok) {
        onChange?.();
      }
    });
  };
  const smart = playlist.kind === 'smart';
  return (
    <div className={styles.options}>
      {!smart && (
        <OptionRow label={t.order}>
          <Choice
            label={t.order}
            value={manualOrder(playlist)}
            options={[
              { value: 'playlist', label: t.orderPlaylist },
              { value: 'random', label: t.orderRandom },
              { value: 'weighted', label: t.orderWeighted },
            ]}
            onChange={(order) => update(orderPatch(order))}
          />
        </OptionRow>
      )}
      {children}
      {smart && (
        <SwitchRow
          label={t.regenerateOnPlay}
          checked={playlist.regenerateOnPlay}
          onChange={(on) => update({ regenerateOnPlay: on })}
        />
      )}
      <SwitchRow
        label={t.seedLocked}
        checked={playlist.seedLocked}
        onChange={(on) => update({ seedLocked: on })}
      />
      <SwitchRow
        label={t.noSameKindRun}
        checked={playlist.maxSameKindRun > 0}
        onChange={(on) => update({ maxSameKindRun: on ? 2 : 0 })}
      />
      <OptionRow label={t.imageDuration}>
        <Choice
          label={t.imageDuration}
          value={Math.round(playlist.imageDurationMs / 1000)}
          options={IMAGE_SECONDS.map((n) => ({ value: n, label: t.seconds(String(n)) }))}
          onChange={(n) => update({ imageDurationMs: n * 1000 })}
        />
      </OptionRow>
      <SwitchRow
        label={t.bgmThroughImages}
        checked={playlist.bgmThroughImages}
        onChange={(on) => update({ bgmThroughImages: on })}
      />
      <OptionRow label={t.repeat}>
        <Choice
          label={t.repeat}
          value={playlist.repeatMode}
          options={[
            { value: 'off', label: t.repeatOff },
            { value: 'all', label: t.repeatAll },
            { value: 'one', label: t.repeatOne },
          ]}
          onChange={(repeatMode) => update({ repeatMode })}
        />
      </OptionRow>
      {!smart && playlist.granularity === 'file' && (
        <SwitchRow
          label={t.dedup}
          checked={playlist.dedupItems}
          onChange={(on) => update({ dedupItems: on })}
        />
      )}
    </div>
  );
}
