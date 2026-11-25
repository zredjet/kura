/**
 * 音量を合わせて聞く（M17 段 6。docs/design.md「試聴だけの重ね」）：鳴らしうる音（元・編集後と、外して聞いている間は外した
 * 音）のうち、いちばん小さいラウドネスに合わせて大きい音を下げる（上げない：0 dBFS を超えないため）。計れない音（無音など）は
 * 比べる対象から外し、下げない。比べる音が 2 つそろうまでは合わせない。
 */

/** 役ごとのラウドネス（LUFS。分からない・計れなければ null）。 */
export interface MatchLevels {
  input: number | null;
  output: number | null;
  audition: number | null;
}

/** 鳴らす音ごとの下げる量（dB。0 以下）：元と、編集後（外して聞いている間は外した音）。 */
export interface MatchGains {
  source: number;
  edited: number;
}

const known = (v: number | null): v is number => v !== null && Number.isFinite(v);

/** 下げる量（モジュールの説明）。`bypassing` は外した音を鳴らしているか。 */
export function matchGains(levels: MatchLevels, bypassing: boolean): MatchGains {
  const pool = [levels.input, levels.output, bypassing ? levels.audition : null].filter(known);
  if (pool.length < 2) return { source: 0, edited: 0 };
  const min = Math.min(...pool);
  const down = (v: number | null) => (known(v) ? Math.min(0, min - v) : 0);
  return {
    source: down(levels.input),
    edited: bypassing ? down(levels.audition) : down(levels.output),
  };
}

/** dB を倍率に。 */
export function dbToGain(db: number): number {
  return Math.pow(10, db / 20);
}
