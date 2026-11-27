import { useEffect, useState } from 'react';

import { ja } from '../../../i18n/ja';
import { eqCurve, type EditEqFirDto } from '../../../ipc/edit';
import editorStyles from '../Editor.module.css';
import { useEditorStore } from '../editorStore';
import { useAudioStore } from './audioStore';
import { eqJson, readEq } from './eqModel';

const t = ja.editor.audio.eqPhase;

/** 引数が変わってから頼むまでの待ち（ドラッグの途中で何度も FIR を設計させない）。 */
export const EQ_PHASE_NOTE_WAIT_MS = 150;

/** 前のリンギングの長さの書き方：0.1 秒以上は秒（小数 2 桁）、それより短ければミリ秒（1 以上）。 */
export function ringingTime(delayFrames: number, rate: number): string {
  const s = rate > 0 ? delayFrames / rate : 0;
  return s >= 0.1 ? t.seconds(s.toFixed(2)) : t.millis(Math.max(1, Math.round(s * 1_000)));
}

/**
 * 右パネルのイコライザの位相の欄の下の一言（M17 段 7。design.md §3.1）：直線位相のときだけ、前のリンギングの最大の長さ
 * （FIR の遅れ D。Rust が `edit_eq_curve` の `fir` で返す）と、長さの上限に届いたかを出す。面によらず（波形の面でも）、
 * 引数が変わって 150 ms 止まったら点 2 つの曲線を頼んで知る（出していた一言は、次が届くまで残す）。最小位相・段がない
 * ときは出さない。
 */
export function EqPhaseNote({ node, op, param }: { node: number; op: string; param: string }) {
  const params = useEditorStore((s) => s.doc.nodes[node]?.params);
  const rate = useAudioStore((s) => s.view?.nodes.find((n) => n.id === node)?.inRate ?? null);
  const channels = useAudioStore(
    (s) => s.view?.nodes.find((n) => n.id === node)?.inChannels ?? null,
  );
  const target = op === 'audio.eq' && param === 'phase';
  const eq = target ? readEq(params) : null;
  const linear = eq?.phase === 'linear';
  const key =
    linear && eq && rate !== null && channels !== null
      ? JSON.stringify([eqJson(eq), rate, channels])
      : null;
  const [fir, setFir] = useState<{ fir: EditEqFirDto | null; rate: number } | null>(null);
  useEffect(() => {
    if (key === null) return;
    const [p, r, ch] = JSON.parse(key) as [Parameters<typeof eqCurve>[0], number, number];
    let live = true;
    const timer = setTimeout(() => {
      eqCurve(p, r, ch, 2)
        .then((c) => {
          if (live) setFir({ fir: c.fir, rate: r });
        })
        .catch(() => {
          // 求められない（引数が断られた）：一言を出さない（指摘は欄が出す）
          if (live) setFir(null);
        });
    }, EQ_PHASE_NOTE_WAIT_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [key]);
  // レート・チャンネルがまだ分からない（表示の前）間は、前のノードの値を出さない
  if (key === null || !fir?.fir) return null;
  return (
    <>
      <p className={editorStyles.note}>{t.ringing(ringingTime(fir.fir.delayFrames, fir.rate))}</p>
      {fir.fir.capped && <p className={editorStyles.note}>{t.capped}</p>}
    </>
  );
}
