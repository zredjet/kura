import { useId, useState } from 'react';
import { createPortal } from 'react-dom';

import { Button } from '../../components/Button/Button';
import { Segmented } from '../../components/Segmented/Segmented';
import { RepeatIcon } from '../../components/icons/RepeatIcon';
import { WarningIcon } from '../../components/icons/WarningIcon';
import { ja } from '../../i18n/ja';
import {
  type AssetChoiceDto,
  type AssetDto,
  retryAnalysis,
  setAssetSettings,
} from '../../ipc/asset';
import type { ItemSummaryDto } from '../../ipc/items';
import { planAudition } from '../../lib/asset/audition';
import { invalidateItems } from '../../stores/itemCache';
import { notify } from '../../stores/noticeStore';
import styles from './Asset.module.css';
import { audioRuntime } from './audioRuntime';
import {
  auditionBlocker,
  auditionTarget,
  startAudition,
  stopAudition,
  useAuditionStore,
} from './auditionStore';
import {
  alphaText,
  animText,
  audioFormatText,
  colorsText,
  countText,
  failureText,
  fontFormatText,
  gridText,
  loopTexts,
  loudnessText,
  modelSizeText,
  peakText,
  pixelAutoText,
  sizeText,
  unsupportedText,
} from './describe';
import { GridSheet } from './GridSheet';
import type { AssetData } from './useAssetData';
import { loopOf } from './useLoopRange';
import { WaveformView } from './WaveformView';

const t = ja.asset;

function fail(error: unknown): void {
  notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
}

function choiceOf(setting: boolean | null): AssetChoiceDto {
  return setting === null ? 'auto' : setting ? 'yes' : 'no';
}

/** 解析の状態の行（解析しています… / 解析できませんでした（理由）と「もう一度解析する」）。 */
function AnalysisStatus({ asset, onRetried }: { asset: AssetDto; onRetried: () => void }) {
  if (asset.analysisState === 'pending') {
    return (
      <p className={styles.status} role="status">
        {t.analyzing}
      </p>
    );
  }
  const failed = asset.analysisState === 'failed' || asset.facts?.ok === false;
  if (!failed) {
    return null;
  }
  return (
    <p className={styles.status}>
      <span>{t.analysisFailed(failureText(asset.facts?.errorCode))}</span>
      <button
        type="button"
        className={styles.textButton}
        onClick={() => retryAnalysis([asset.itemId]).then(onRetried, fail)}
      >
        {t.retryAnalysis}
      </button>
    </p>
  );
}

/** 画像の行：大きさ・透過・色数・ドット絵（3 択と自動の判定）・アニメ・格子。 */
function ImageRows({ data, onChanged }: { data: AssetData; onChanged: () => void }) {
  const { asset, anim } = data;
  const [grid, setGrid] = useState(false);
  if (!asset) return null;
  const image = asset.facts?.image;
  const choose = (value: AssetChoiceDto) => {
    setAssetSettings([asset.itemId], { pixelArt: value }).then(() => {
      invalidateItems([asset.itemId]);
      onChanged();
    }, fail);
  };
  const frames = asset.effective.frameCount ?? image?.frameCount ?? null;
  return (
    <>
      <dt>{t.size}</dt>
      <dd>{sizeText(asset.width, asset.height)}</dd>
      <dt>{t.alpha}</dt>
      <dd>{alphaText(image)}</dd>
      <dt>{t.colors}</dt>
      <dd>{colorsText(image)}</dd>
      <dt>{t.pixelArt}</dt>
      <dd className={styles.control}>
        <Segmented
          label={t.pixelArtGroup}
          value={choiceOf(asset.settings.pixelArt)}
          options={[
            { value: 'auto', label: t.pixelChoices.auto },
            { value: 'yes', label: t.pixelChoices.yes },
            { value: 'no', label: t.pixelChoices.no },
          ]}
          onChange={choose}
        />
        <span className={styles.sub}>{pixelAutoText(image)}</span>
      </dd>
      <dt>{t.anim}</dt>
      <dd>
        {animText(frames, anim.status === 'ready' ? anim.anim : null)}
        {anim.status === 'tooLarge' && <span className={styles.sub}>{t.animTooLarge}</span>}
        {anim.status === 'failed' && <span className={styles.sub}>{t.animFailed}</span>}
      </dd>
      <dt>{t.grid}</dt>
      <dd className={styles.inline}>
        <span>{gridText(asset.settings.grid, asset.effective.frameCount)}</span>
        <button
          type="button"
          className={styles.textButton}
          aria-label={t.gridEditLabel}
          onClick={() => setGrid(true)}
        >
          {t.gridEdit}
        </button>
        {grid &&
          // インスペクタのガラス（backdrop-filter）は fixed の子の基準になるので、シートは body に出す
          createPortal(
            <GridSheet
              asset={asset}
              onClose={() => setGrid(false)}
              onSaved={() => {
                invalidateItems([asset.itemId]);
                onChanged();
              }}
            />,
            document.body,
          )}
      </dd>
    </>
  );
}

/** 本体が長すぎて、継ぎ目の前後だけを試聴することになりそうか（長さは概要の長さから見積もる）。 */
function seamOnly(asset: AssetDto, durationMs: number | null): boolean {
  const audio = asset.facts?.audio;
  if (!audio?.sampleRate || !audio.channels) return false;
  const loop = loopOf(asset);
  const estimate = Math.round(((durationMs ?? 0) * audio.sampleRate) / 1000);
  const frames = Math.max(estimate, loop?.[1] ?? 0);
  if (frames <= 0) return false;
  const plan = planAudition({ frames, rate: audio.sampleRate, channels: audio.channels, loop });
  return plan?.mode === 'seam';
}

/** 音声の行（形式・ラウドネス・ピーク・ループ）と、波形・ループの試聴。 */
function AudioRows({ asset }: { asset: AssetDto }) {
  const audio = asset.facts?.audio;
  const loop = loopTexts(audio);
  return (
    <>
      <dt>{t.format}</dt>
      <dd>{audioFormatText(audio)}</dd>
      <dt>{t.loudness}</dt>
      <dd>{loudnessText(audio)}</dd>
      <dt>{t.peak}</dt>
      <dd>{peakText(audio)}</dd>
      <dt>{t.loop}</dt>
      <dd>
        {loop ? loop.time : audio?.hasLoop === false ? t.loopNone : t.none}
        {loop && <span className={styles.sub}>{loop.frames}</span>}
      </dd>
    </>
  );
}

/** 試聴中の位置を読む（波形の線。同じ関数を渡し続ける）。 */
const readPosition = () => audioRuntime().audition.position();

function AuditionControls({ asset, durationMs }: { asset: AssetDto; durationMs: number | null }) {
  const state = useAuditionStore();
  const target = auditionTarget(asset);
  const blocker = auditionBlocker(asset);
  const active = state.status !== 'idle' && state.itemId === asset.itemId;
  const playing = active && state.status === 'playing';
  const loop = loopOf(asset);
  const seam = active && state.mode !== null ? state.mode === 'seam' : seamOnly(asset, durationMs);
  return (
    <>
      <WaveformView
        hash={asset.hash}
        version={asset.facts?.analyzedAt ?? null}
        loop={loop}
        position={playing ? readPosition : null}
      />
      <div className={styles.audition}>
        <Button
          size="sm"
          aria-pressed={active}
          disabled={!target && !active}
          onClick={() => {
            if (active) stopAudition();
            else if (target) startAudition(target, false);
          }}
        >
          <RepeatIcon size={13} />
          {active ? t.auditionStop : t.audition}
        </Button>
        <Button size="sm" disabled={!target} onClick={() => target && startAudition(target, true)}>
          {t.fromStart}
        </Button>
      </div>
      {blocker ? (
        <p className={styles.note}>{blocker}</p>
      ) : seam ? (
        <p className={styles.note}>{t.seamOnly}</p>
      ) : null}
    </>
  );
}

/** フォントの行：書体名・スタイル・字の数・形式。 */
function FontRows({ asset }: { asset: AssetDto }) {
  const font = asset.font;
  return (
    <>
      <dt>{t.family}</dt>
      <dd>{font?.family ?? t.none}</dd>
      <dt>{t.style}</dt>
      <dd>{font?.style ?? t.none}</dd>
      <dt>{t.glyphs}</dt>
      <dd>
        {font?.glyphs === null || font?.glyphs === undefined
          ? t.none
          : t.glyphsValue(countText(font.glyphs))}
      </dd>
      <dt>{t.fontFormat}</dt>
      <dd>{fontFormatText(font, asset.ext)}</dd>
    </>
  );
}

/** 3D の行：頂点・三角形・大きさ・材質。 */
function ModelRows({ asset }: { asset: AssetDto }) {
  const model = asset.model;
  return (
    <>
      <dt>{t.vertices}</dt>
      <dd>{countText(model?.vertices)}</dd>
      <dt>{t.triangles}</dt>
      <dd>{countText(model?.triangles)}</dd>
      <dt>{t.modelSize}</dt>
      <dd>{modelSizeText(model?.size ?? null)}</dd>
      <dt>{t.materials}</dt>
      <dd>{countText(model?.materials)}</dd>
    </>
  );
}

/**
 * インスペクタの「素材」の欄（design.md §3.1「素材の欄」。見出しの直後）。解析の事実・利用者の設定・実効値と、
 * フォント・3D の情報（item_asset_get。DB だけを読む）。動画には出さない。
 */
export function AssetSection({ data, item }: { data: AssetData; item: ItemSummaryDto }) {
  const headingId = useId();
  const { asset, reload } = data;
  if (!asset || asset.itemId !== item.id || asset.kind === 'video') {
    return null;
  }
  const unsupported =
    asset.kind === 'model' ? unsupportedText(asset.model?.unsupported ?? []) : null;
  return (
    <section className={styles.section} aria-labelledby={headingId}>
      <h3 id={headingId} className={styles.label}>
        {t.section}
      </h3>
      <AnalysisStatus asset={asset} onRetried={reload} />
      <dl className={styles.info}>
        {asset.kind === 'image' && <ImageRows data={data} onChanged={reload} />}
        {asset.kind === 'audio' && <AudioRows asset={asset} />}
        {asset.kind === 'font' && <FontRows asset={asset} />}
        {asset.kind === 'model' && <ModelRows asset={asset} />}
      </dl>
      {unsupported && (
        <p className={`${styles.note} ${styles.warning}`}>
          <WarningIcon size={12} />
          {unsupported}
        </p>
      )}
      {asset.kind === 'audio' && <AuditionControls asset={asset} durationMs={item.durationMs} />}
    </section>
  );
}
