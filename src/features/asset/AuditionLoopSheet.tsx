import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { Segmented } from '../../components/Segmented/Segmented';
import { ja } from '../../i18n/ja';
import { type AssetDto, type AssetSettingsPatchDto, setAssetSettings } from '../../ipc/asset';
import { TimeField } from '../editor/audio/TimeField';
import styles from './Asset.module.css';
import { probeFrames } from './audioRuntime';
import { clockText } from './describe';
import { fileLoopOf, type LoopRange, settingLoopOf } from './useLoopRange';

const t = ja.asset;

/** 回す区間の選び方：ファイルのループ点（なければ全体）か、区間を指定（素材の設定）。 */
type Mode = 'file' | 'range';

/**
 * 区間を確かめる（0 ≤ 始め < 終わり ≤ 長さ。長さが分からなければ終わりは見ない）。問題がなければ null、あれば理由。
 * 始めが 0 以上なのは時刻の欄（`TimeField`）が負を読まないことで守る。
 */
export function checkAuditionLoop(
  start: number | null,
  end: number | null,
  length: number | null,
  rate: number,
): string | null {
  if (start === null || end === null) return t.auditionLoopMissing;
  if (start >= end) return t.auditionLoopOrder;
  if (length !== null && end > length) return t.auditionLoopPastEnd(clockText(length, rate));
  return null;
}

function rangeText(range: LoopRange, rate: number): string {
  return t.loopRange(clockText(range[0], rate), clockText(range[1], rate));
}

interface AuditionLoopSheetProps {
  asset: AssetDto;
  onClose: () => void;
  /** 保存した・消した（読み直す）。 */
  onSaved: () => void;
  /** 長さ（フレーム）を /pcm の先頭の 1 フレームの応答で知る口（テストで差し替える）。 */
  probe?: (hash: string) => Promise<number | null>;
}

/**
 * 試聴のループのシート（design.md §3.1「試聴のループのシート」。M13 の決定 5。size="sm"）：上にセグメント「ファイルの
 * ループ点｜区間を指定」（ファイルにループ点がなければ左は「全体」）。区間を指定では始め・終わりを時刻（m:ss.mmm）で入れ、
 * 標本の位置を添える（エディタの時刻の欄と同じ部品）。長さは試聴と同じく /pcm の応答（読めなければ解析の長さ）で確かめる。
 * 「ファイルのループ点」を選んで保存すると設定を消す。ファイルは変えない。
 */
export function AuditionLoopSheet({
  asset,
  onClose,
  onSaved,
  probe = probeFrames,
}: AuditionLoopSheetProps) {
  const id = useId();
  const rate = asset.facts?.audio.sampleRate ?? 0;
  const file = fileLoopOf(asset);
  const setting = settingLoopOf(asset);
  const initial = setting ?? file;
  const [mode, setMode] = useState<Mode>(setting ? 'range' : 'file');
  const [start, setStart] = useState<number>(initial ? initial[0] : 0);
  // null は「まだ入れていない」：長さが分かれば長さ（全体の終わり）を出す
  const [end, setEnd] = useState<number | null>(initial ? initial[1] : null);
  const [probed, setProbed] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    probe(asset.hash).then(
      (total) => {
        if (alive && total !== null) setProbed(total);
      },
      // 混んでいる・読めない：解析の長さで確かめる（保存のときに Rust も確かめる）
      () => undefined,
    );
    return () => {
      alive = false;
    };
  }, [asset.hash, probe]);

  const length = probed ?? asset.facts?.audio.frames ?? null;
  const shownEnd = end ?? length;
  const error = mode === 'range' ? checkAuditionLoop(start, shownEnd, length, rate) : null;
  const note = failure ?? error;
  const whole: LoopRange | null = length !== null ? [0, length] : null;
  const fileRange = file ?? whole;

  const save = (patch: AssetSettingsPatchDto) => {
    setSaving(true);
    setFailure(null);
    setAssetSettings([asset.itemId], patch).then(
      () => {
        setSaving(false);
        onSaved();
        onClose();
      },
      (reason: unknown) => {
        setSaving(false);
        setFailure(reason instanceof Error ? reason.message : String(reason));
      },
    );
  };

  const submit = () => {
    if (mode === 'file') {
      if (setting) save({ loopRange: 'clear' });
      else onClose();
      return;
    }
    if (error === null && shownEnd !== null) {
      save({ loopRange: { set: { start, end: shownEnd } } });
    }
  };

  return (
    <Dialog labelledBy={id} onClose={onClose} size="sm">
      <h2 id={id} className={styles.sheetTitle}>
        {t.auditionLoopTitle}
      </h2>
      <div className={styles.form}>
        <Segmented
          label={t.auditionLoopGroup}
          value={mode}
          options={[
            { value: 'file', label: file ? t.auditionLoopFile : t.auditionLoopWhole },
            { value: 'range', label: t.auditionLoopCustom },
          ]}
          onChange={(next) => {
            setMode(next);
            setFailure(null);
          }}
        />
        {mode === 'file' ? (
          <p className={styles.loopValue}>{fileRange ? rangeText(fileRange, rate) : t.none}</p>
        ) : (
          <div className={styles.fields}>
            <TimeField label={t.auditionLoopStart} frames={start} rate={rate} onValue={setStart} />
            <TimeField label={t.auditionLoopEnd} frames={shownEnd} rate={rate} onValue={setEnd} />
          </div>
        )}
        <p className={styles.note}>{t.auditionLoopNote}</p>
        <p
          className={[styles.sheetNote, note ? styles.sheetError : ''].filter(Boolean).join(' ')}
          role={note ? 'alert' : undefined}
        >
          {note ?? (length !== null ? t.auditionLoopLength(clockText(length, rate)) : '')}
        </p>
      </div>
      <div className={styles.sheetActions}>
        <span className={styles.spacer} />
        <Button size="sm" onClick={onClose}>
          {ja.common.cancel}
        </Button>
        <Button size="sm" variant="primary" disabled={saving || error !== null} onClick={submit}>
          {ja.common.save}
        </Button>
      </div>
    </Dialog>
  );
}
