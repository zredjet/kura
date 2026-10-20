import { useEffect } from 'react';

import { isUnitEntry } from '../../ipc/items';
import { shouldPlaySfx } from '../../lib/asset/audition';
import { getCached } from '../../stores/itemCache';
import { useViewStore } from '../../stores/viewStore';
import { usePlayerStore } from '../player/playerStore';
import { audioRuntime, closeAudio } from './audioRuntime';
import { stopAudition, useAuditionStore } from './auditionStore';

/**
 * 選んだものが変わったときに、音を決める（ライブラリの画面に置く）。
 * - 効果音の即再生（決定 13。design.md §3.1）：10 秒以下の音声を 1 件だけ選んだら鳴らす。設定（sfxAudition）が
 *   オフ・プレーヤーの再生中・クイックルック中・複数選択では鳴らさない（shouldPlaySfx）。前の音は 10ms で下げて止める。
 * - ループの試聴は選んでいるものの操作なので、選ぶものが変わったら止める。
 * - クイックルックを開いたら効果音と試聴を止める（クイックルックは自分で鳴らす）。
 * 画面を離れるときは音を止めて、文脈をすべて閉じる。
 */
export function useSfxOnSelect(): void {
  useEffect(() => {
    let focus = useViewStore.getState().selection.focus;
    let quickLook = useViewStore.getState().quickLook;
    const unsubscribe = useViewStore.subscribe((view) => {
      if (view.quickLook !== quickLook) {
        quickLook = view.quickLook;
        if (quickLook) {
          audioRuntime().sfx.stop();
          if (useAuditionStore.getState().status !== 'idle') {
            stopAudition();
          }
        }
      }
      if (view.selection.focus === focus) {
        return;
      }
      focus = view.selection.focus;
      const audition = useAuditionStore.getState();
      if (audition.status !== 'idle' && audition.itemId !== focus) {
        stopAudition();
      }
      const { sfx } = audioRuntime();
      sfx.stop();
      const item = focus !== null && !isUnitEntry(focus) ? getCached(focus) : undefined;
      const player = usePlayerStore.getState();
      const play = shouldPlaySfx(item, {
        enabled: player.prefs.sfxAudition,
        playerPlaying: player.active && player.snapshot.playing,
        quickLook: view.quickLook,
        selectedCount: view.selection.ids.length,
      });
      if (play && item) {
        void sfx.play(item.hash);
      }
    });
    return () => {
      unsubscribe();
      closeAudio();
    };
  }, []);
}
