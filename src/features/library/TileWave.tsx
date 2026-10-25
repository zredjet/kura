import { type CSSProperties, useReducer } from 'react';

import { Waveform } from '../../components/icons/Waveform';
import { waveUrl } from '../../ipc/media';
import styles from './TileWave.module.css';

/**
 * 確かめた実波形の URL（真：読めた、偽：読めなかった）。仮想スクロールでタイルが作り直されても、同じ URL を確かめ直さない
 * （読めたものは mask だけ、読めなかったものは飾りの波形だけを出し、要求を出さない）。
 */
const outcomes = new Map<string, boolean>();
/** 覚える数の上限（古いものから忘れる。忘れたものは次に出すときにもう一度だけ確かめる）。 */
const MAX_OUTCOMES = 10_000;

function remember(url: string, ok: boolean): void {
  outcomes.delete(url);
  outcomes.set(url, ok);
  if (outcomes.size > MAX_OUTCOMES) {
    const oldest = outcomes.keys().next().value;
    if (oldest !== undefined) {
      outcomes.delete(oldest);
    }
  }
}

interface TileWaveProps {
  hash: string;
  /** 枠の位置・大きさと色（`color`）。飾りの波形と同じクラスを渡す。 */
  className?: string;
}

/**
 * ジャケットのない音声のタイルの実波形（docs/design.md §4 のタイル）。Rust が波形の段から作る棒のアルファの PNG
 * （`waveUrl`）を `mask-image` にして、色は `className` の文字の色（`--text-secondary`）。読めなければ（段のファイルが
 * ない・壊れている）飾りの波形に戻る。
 *
 * mask の画像は読めたかを知らせないので、最初の 1 回だけ同じ URL を隠した `<img>` でも読んで `error` を見る。WebKit は
 * mask の画像を CORS（anonymous）で読むので、`<img>` も `crossOrigin="anonymous"` にして同じ要求の形にし、メモリのキャッシュで
 * 1 回の読み込みを分け合う（docs/architecture.md §4.2 の P7）。結果は URL ごとに覚え、次からは確かめない。
 */
export function TileWave({ hash, className }: TileWaveProps) {
  // 読めなかったときに描き直す（結果は outcomes に置く）
  const [, refresh] = useReducer((n: number) => n + 1, 0);
  const url = waveUrl(hash);
  const known = outcomes.get(url);
  if (known === false) {
    return <Waveform className={className} />;
  }
  const mask = `url("${url}")`;
  const style: CSSProperties = { WebkitMaskImage: mask, maskImage: mask };
  return (
    <>
      <span
        className={className ? `${className} ${styles.wave}` : styles.wave}
        data-render="mask"
        style={style}
        aria-hidden="true"
      />
      {known === undefined && (
        <img
          src={url}
          crossOrigin="anonymous"
          alt=""
          hidden
          decoding="async"
          onLoad={() => remember(url, true)}
          onError={() => {
            remember(url, false);
            refresh();
          }}
        />
      )}
    </>
  );
}
