//! Ogg Vorbis /1 を流して書く（M13 段 3。docs/editing.md §9.7）：生産者を後ろ向きに 1 回、前向きに 1 回流して、一度に渡す形
//! （`encoder::encode`。/1 の正）とバイト単位で同じものを書く。
//!
//! 1. **後ろ向き**：窓を後ろから流し（`AudioProducer::produce` に窓 1 つの区間）、Vorbis の並びにして `sanitize` した標本を
//!    窓の中も後ろから [`block::PreLpc`] に足す（前の外挿の自己相関。和の順は一度に渡す形の `preextrapolate_aut` と同じ
//!    なのでビット単位で同じ）。同じ回で生産者のダイジェストが全体を覆うので、シリアル番号（A10）を作れる。
//! 2. **前向き**：窓を前から流し、`vorbis_analysis_buffer` → 書く → `vorbis_analysis_wrote_streaming`（前の外挿を呼ばない）。
//!    最初の窓のあとで、後ろ向きの回の自己相関で前の外挿を当てる（入力が 32 標本以下なら当てずに旗だけ）。窓ごとに
//!    `vorbis_analysis_plan` が None を返すまで計画を積み、`segment_blocks` 個たまったら [`BlockAnalyzer`] で区間を
//!    パケットにして [`StreamMux`] に渡し、読み終えた pcm と envelope の印を落とす。終わりに `vorbis_analysis_wrote(0)`
//!    （末尾の外挿は、一度に渡す形と同じく入力の頭からの位置で数える）を呼び、計画を最後まで積む。
//! 3. **同じになる理由**：pcm の中身は一度に渡す形と同じ（前の外挿は全体の自己相関、末尾の外挿は同じ最後の標本から）。
//!    envelope の探索は窓の全体がそろった位置だけを計り、状態を運び、読む印はもう変わらない所だけなので、標本が増える
//!    たびに続けても同じ計画になる。None を返す計画はカーソルのほかに状態を変えない。`mapping0` が dsp から読むのは `vi` と
//!    `backend_state` だけ。区間の大きさは結果に効かない。
//! 4. **`StreamMux`**：`encoder::page_plan` の決まりを最大 3 パケットの先読みで当てる（A6′・EOS は最後の 3 つで決まる）。
//!
//! メモリ：pcm は区間のブロックの幅 ＋ 窓 ＋ 余裕（長い窓 2 つ）× チャンネル × 4、区間の段 1 の結果、先読みの 3 パケット。

use std::collections::VecDeque;

use super::block::{self, PRE_ORDER, PreLpc, VorbisDspState};
use super::encoder::{
    BlockAnalyzer, COMMENT_MAX, FRAMES_MAX, MUX_CANCEL_EVERY, PacketWindow, blockflags_of,
    comment_list, internal, packet_window, sanitize, segment_blocks, serial_from_digest,
    vorbis_order,
};
use super::envelope::ve_envelope_trim;
use super::{CHANNELS_MAX, QUALITY_MAX, QUALITY_MIN, RATE_MAX, RATE_MIN};
use super::{info, setup};
use crate::audio::stream::{
    AudioProducer, AudioSink, DigestingProducer, StreamInfo, stream_windows,
};
use crate::codec::WriteSeek;
use crate::codec::ogg::{Flush, OggPacket, OggWriter};
use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::render::region::Spans;

/// 流して書く間の作業領域の固定の分（区間の段 1 の結果・先読みのパケット・pcm の余裕など。値によらない）。
pub(crate) const STREAM_FIXED: u64 = 32 << 20;

/// 生産者 `src` を流して、品質 `quality` の Ogg Vorbis を `out` に書く（一度に渡す形と同じバイト列）。書いたバイト数を返す。
pub(crate) fn encode_stream(
    src: &mut dyn AudioProducer,
    quality: i64,
    out: &mut dyn WriteSeek,
    mon: &dyn Monitor,
) -> Result<u64, EngineError> {
    encode_stream_with(src, quality, out, mon, None)
}

/// [`encode_stream`] の区間の大きさ（ブロックの数）を決められる形（`None` は `segment_blocks`。結果は区間の大きさに
/// よらない：テストが小さな区間で pcm を落とす道を通す）。
pub(crate) fn encode_stream_with(
    src: &mut dyn AudioProducer,
    quality: i64,
    out: &mut dyn WriteSeek,
    mon: &dyn Monitor,
    segment: Option<usize>,
) -> Result<u64, EngineError> {
    let info = src.info().clone();
    // 1. 形の確認（`encoder::prepare` と同じ）
    let channels = info.channels;
    if !(1..=usize::from(CHANNELS_MAX)).contains(&channels)
        || !(RATE_MIN..=RATE_MAX).contains(&info.rate)
    {
        return Err(EngineError::Unsupported(format!(
            "Ogg Vorbis に書けるのは 1〜{CHANNELS_MAX} チャンネル・{RATE_MIN}〜{RATE_MAX} Hz です"
        )));
    }
    if !(QUALITY_MIN..=QUALITY_MAX).contains(&quality) {
        return Err(EngineError::Unsupported(format!(
            "Ogg Vorbis の品質は {QUALITY_MIN}〜{QUALITY_MAX} です"
        )));
    }
    let frames = usize::try_from(info.frames)
        .ok()
        .filter(|f| *f <= FRAMES_MAX)
        .ok_or_else(|| {
            EngineError::Unsupported(format!(
                "Ogg Vorbis に書けるのは {FRAMES_MAX} フレームまでです"
            ))
        })?;
    let order = vorbis_order(channels)?;
    let q = i32::try_from(quality).map_err(|_| internal("品質の値"))?;

    // 3. 設定・4. ヘッダ
    let vi = setup::vorbis_encode_init_vbr(
        i64::try_from(channels).map_err(|_| internal("チャンネルの数"))?,
        i64::from(info.rate),
        q as f32 / 10.0,
    )?;
    let comments = comment_list(&info.meta, frames as u64);
    let headers = info::vorbis_analysis_headerout(&vi, super::VENDOR, &comments)?;
    if headers[1].packet.len() >= COMMENT_MAX {
        return Err(EngineError::Unsupported(
            "Ogg Vorbis のコメント（タグ）が大きすぎます（16 MiB まで）".into(),
        ));
    }

    // 生産者を自分でもダイジェストの包みで包む（シリアル番号は入力の値のダイジェストから。呼び手が包んでいなくても取れる）
    let mut src = DigestingProducer::new(src)?;
    let windows = stream_windows(frames as u64, None);

    // 後ろ向きの回：前の外挿の自己相関とダイジェスト
    let mut back = Backward {
        order,
        lpc: vec![PreLpc::default(); channels],
        next: frames as u64,
        pending: Vec::new(),
    };
    for (s, e) in windows.iter().rev() {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        src.produce(Some(&Spans::one(*s, *e)), &mut back, mon)?;
    }
    if back.next != 0 {
        return Err(internal("後ろ向きの回の長さ"));
    }
    let digest = src
        .digest()?
        .ok_or_else(|| internal("後ろ向きの回のダイジェスト"))?;
    let serial = serial_from_digest(&digest, quality)?;
    // 一度に渡す形の `_preextrapolate_helper` は入力の全体（N 標本）で、N > 32 のときだけ当てる
    let aut: Option<Vec<f64>> = (frames > PRE_ORDER * 2).then(|| {
        back.lpc
            .iter()
            .flat_map(|l| l.sums().iter().copied())
            .collect()
    });
    drop(back);

    // 前向きの回
    let vd = block::vorbis_analysis_init(vi)?;
    let analyzer = BlockAnalyzer::new(&vd)?;
    let segment = segment
        .unwrap_or_else(|| segment_blocks(channels, vd.vi.codec_setup.blocksizes[1]))
        .max(1);
    let mux = StreamMux::new(
        &headers,
        blockflags_of(&vd),
        vd.vi.codec_setup.blocksizes,
        frames as u64,
        serial,
    )?;
    let mut fwd = Forward {
        vd,
        order,
        aut,
        started: false,
        next: 0,
        plans: Vec::new(),
        segment,
        analyzer,
        mux,
        out,
        written: 0,
    };
    fwd.flush_pages()?;
    src.produce(None, &mut fwd, mon)?;
    if fwd.next != frames as u64 {
        return Err(internal("前向きの回の長さ"));
    }
    fwd.finish(mon)
}

/// 後ろ向きの回の受け手：窓を後ろから受け、チャンネルごとに前の外挿の自己相関に足す。区間 1 つ（窓 1 つ）の中で生産者が
/// 窓を細かく分けて前から渡したときは、続かない窓を写して取っておき、後ろから足す（ふつうの生産者は区間 1 つに窓 1 つ）。
struct Backward {
    order: &'static [usize],
    lpc: Vec<PreLpc>,
    /// 次に足す窓の終わり（後ろから順に続いて足す）。
    next: u64,
    /// まだ足せない（後ろが来ていない）窓の写し：(始まり, チャンネルごとの標本)。
    pending: Vec<(u64, Vec<Vec<f32>>)>,
}

impl Backward {
    /// 窓（フレーム `start` から。`next` で終わること）を自己相関に足す。
    fn add(&mut self, start: u64, planes: &[&[f32]], mon: &dyn Monitor) -> Result<(), EngineError> {
        let order = self.order;
        let lpc = &self.lpc;
        // チャンネルごとに独立（各和の順はチャンネルの中で決まる）
        let next = par_blocks(lpc.len(), mon, |i| {
            let src = *order.get(i).ok_or_else(|| internal("チャンネルの並び"))?;
            let plane = planes
                .get(src)
                .ok_or_else(|| internal("チャンネルの並び"))?;
            let mut l = lpc[i].clone();
            l.feed_back(plane, sanitize);
            Ok(l)
        })?;
        self.lpc = next;
        self.next = start;
        Ok(())
    }

    /// 取っておいた窓のうち、続くものを後ろから足す。
    fn drain(&mut self, mon: &dyn Monitor) -> Result<(), EngineError> {
        while let Some(k) = self
            .pending
            .iter()
            .position(|(s, p)| s + p.first().map_or(0, |x| x.len()) as u64 == self.next)
        {
            let (s, p) = self.pending.swap_remove(k);
            let views: Vec<&[f32]> = p.iter().map(Vec::as_slice).collect();
            self.add(s, &views, mon)?;
        }
        Ok(())
    }
}

impl AudioSink for Backward {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        if info.channels != self.lpc.len() {
            return Err(internal("後ろ向きの回のチャンネルの数"));
        }
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let n = planes.first().map_or(0, |p| p.len()) as u64;
        if planes.len() != self.lpc.len() || start + n > self.next {
            return Err(internal("後ろ向きの回の窓の並び"));
        }
        if start + n == self.next {
            self.add(start, planes, mon)?;
            self.drain(mon)
        } else {
            self.pending
                .push((start, planes.iter().map(|p| p.to_vec()).collect()));
            Ok(())
        }
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(internal("後ろ向きの回の窓の並び"))
        }
    }
}

/// 前向きの回の受け手：dsp に書き、計画を積み、区間をパケットにして Ogg に書く。
struct Forward<'o> {
    vd: VorbisDspState,
    order: &'static [usize],
    /// 前の外挿の自己相関（入力が 32 標本以下なら None）。
    aut: Option<Vec<f64>>,
    /// 最初の窓を書いて前の外挿を当てたか。
    started: bool,
    next: u64,
    plans: Vec<block::BlockPlan>,
    segment: usize,
    analyzer: BlockAnalyzer,
    mux: StreamMux,
    out: &'o mut dyn WriteSeek,
    written: u64,
}

impl Forward<'_> {
    /// Ogg の書き手が閉じたページを書き先へ出す。
    fn flush_pages(&mut self) -> Result<(), EngineError> {
        let pages = self.mux.take_pages();
        if !pages.is_empty() {
            self.out.write_all(&pages)?;
            self.written += pages.len() as u64;
        }
        Ok(())
    }

    /// 計画を積めるだけ積み、区間がたまったらパケットにする（`all` なら残りも）。
    fn plan(&mut self, all: bool, mon: &dyn Monitor) -> Result<(), EngineError> {
        while let Some(p) = block::vorbis_analysis_plan(&mut self.vd, mon)? {
            self.plans.push(p);
            if self.plans.len() >= self.segment {
                self.analyze(mon)?;
            }
        }
        if all && !self.plans.is_empty() {
            self.analyze(mon)?;
        }
        Ok(())
    }

    /// たまった計画の区間をパケットにして Ogg に渡し、読み終えた pcm と envelope の印を落とす。
    fn analyze(&mut self, mon: &dyn Monitor) -> Result<(), EngineError> {
        let plans = std::mem::take(&mut self.plans);
        let packets = self.analyzer.segment(&self.vd, &plans, mon)?;
        for (k, p) in packets.into_iter().enumerate() {
            self.mux.push(p)?;
            if (k + 1).is_multiple_of(MUX_CANCEL_EVERY) && mon.canceled() {
                return Err(EngineError::Canceled);
            }
        }
        self.flush_pages()?;
        // 次の計画の PCM は pcm_base（C の位置 0）より後ろから読む。envelope の探索も pcm_base より後ろだけを読む。末尾の
        // 外挿は入力の終わりの前 blocksizes[1] 標本を読む。余裕を長い窓 2 つ分とって、それより前を落とす
        let bs1 = usize::try_from(self.vd.vi.codec_setup.blocksizes[1]).unwrap_or(0);
        let cur = self.vd.pcm_base + usize::try_from(self.vd.pcm_current).unwrap_or(0);
        let keep = self
            .vd
            .pcm_base
            .min(cur.saturating_sub(bs1))
            .saturating_sub(2 * bs1);
        self.vd.trim_before(keep);
        ve_envelope_trim(&mut self.vd.backend_state.ve);
        Ok(())
    }

    /// 入力を渡し終えた：末尾の外挿（`vorbis_analysis_wrote(0)`）、計画を最後まで積んでパケットにし、Ogg を閉じる。
    fn finish(mut self, mon: &dyn Monitor) -> Result<u64, EngineError> {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        // 入力が空なら、一度に渡す形と同じく wrote(0) が前の外挿の旗を立てる（32 標本以下なので値は変えない）
        block::vorbis_analysis_wrote(&mut self.vd, 0, mon)?;
        self.plan(true, mon)?;
        self.mux.finish()?;
        self.flush_pages()?;
        Ok(self.written)
    }
}

impl AudioSink for Forward<'_> {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        if info.channels != self.vd.pcm.len() {
            return Err(internal("前向きの回のチャンネルの数"));
        }
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let n = planes.first().map_or(0, |p| p.len());
        if start != self.next || planes.len() != self.vd.pcm.len() {
            return Err(internal("前向きの回の窓の並び"));
        }
        if n == 0 {
            return Ok(());
        }
        let vals = i32::try_from(n).map_err(|_| internal("窓の大きさ"))?;
        let pos = block::vorbis_analysis_buffer(&mut self.vd, vals)?;
        for (i, dst) in self.vd.pcm.iter_mut().enumerate() {
            let src = *self
                .order
                .get(i)
                .ok_or_else(|| internal("チャンネルの並び"))?;
            let plane = planes
                .get(src)
                .ok_or_else(|| internal("チャンネルの並び"))?;
            let slot = dst
                .get_mut(pos..pos + n)
                .ok_or_else(|| internal("pcm の大きさ"))?;
            for (d, s) in slot.iter_mut().zip(plane.iter()) {
                *d = sanitize(*s);
            }
        }
        block::vorbis_analysis_wrote_streaming(&mut self.vd, vals)?;
        self.next += n as u64;
        if !self.started {
            // 入力の頭の 16 標本がそろったら（ふつうは最初の窓）、前の外挿を全体の自己相関で当てる（一度に渡す形の
            // _preextrapolate_helper と同じ値。prime は頭の 16 標本だけで決まる）。それまでは計画を積まない（旗が立つまで
            // 計画は None）
            match &self.aut {
                Some(aut) if self.next >= PRE_ORDER as u64 => {
                    block::preextrapolate_apply(&mut self.vd, aut)?;
                    self.vd.preextrapolate = 1;
                    self.started = true;
                }
                Some(_) => {}
                None => {
                    self.vd.preextrapolate = 1;
                    self.started = true;
                }
            }
        }
        self.plan(false, mon)
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// 先読みしている音声パケット（パケット・窓・中心・番号）。
struct Pending {
    packet: OggPacket,
    window: PacketWindow,
    center: i64,
    index: usize,
}

/// `encoder::page_plan`・`mux` の決まりを、最大 3 パケットの先読みで当てる Ogg の書き手（M13 段 3）。窓の旗の照らし合わせと
/// 中心 c_k は来た順に、A6′・EOS・最後のグラニュール（N）は最後の 3 つで決める。
pub(crate) struct StreamMux {
    w: OggWriter,
    blockflags: Vec<bool>,
    blocksizes: [i64; 2],
    mode_bits: u32,
    total: i64,
    pending: VecDeque<Pending>,
    count: usize,
    prev: Option<PacketWindow>,
    center: i64,
}

/// 先読みするパケットの数（A6′ は最後の 2〜3 つで決まる）。
const LOOKAHEAD: usize = 3;

impl StreamMux {
    /// ヘッダ 3 つを書いて始める（A1・A2・A3）。設定の確かめは `page_plan` と同じ。
    pub fn new(
        headers: &[OggPacket; 3],
        blockflags: Vec<bool>,
        blocksizes: [i64; 2],
        total: u64,
        serial: u32,
    ) -> Result<Self, EngineError> {
        let valid_size = |n: i64| (64..=8192).contains(&n) && n.count_ones() == 1;
        if !valid_size(blocksizes[0]) || !valid_size(blocksizes[1]) || blocksizes[0] > blocksizes[1]
        {
            return Err(internal("ブロックの大きさ"));
        }
        if blockflags.is_empty() || blockflags.len() > 64 {
            return Err(internal("mode の数"));
        }
        let modes = u32::try_from(blockflags.len()).map_err(|_| internal("mode の数"))?;
        let mode_bits = u32::try_from(vorbis_tables::ov_ilog(modes - 1)).unwrap_or(0);
        let total = i64::try_from(total).map_err(|_| internal("フレーム数"))?;
        let mut w = OggWriter::with_serial(serial);
        // A1：識別ヘッダだけで 1 ページ目（BOS）
        w.packet(&headers[0].packet, 0, false, Flush::After)?;
        // A2：コメントは 2 ページ目の頭から、setup を同じページに続け、setup で閉じる（A3：音声は新しいページから）
        w.packet(&headers[1].packet, 0, false, Flush::Never)?;
        w.packet(&headers[2].packet, 0, false, Flush::After)?;
        Ok(Self {
            w,
            blockflags,
            blocksizes,
            mode_bits,
            total,
            pending: VecDeque::new(),
            count: 0,
            prev: None,
            center: 0,
        })
    }

    /// 閉じたページ（まだ書き先に出していないもの）を取り出す。
    pub fn take_pages(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.w.out)
    }

    /// 音声パケットを 1 つ足す（窓の旗を前のパケットと照らし、中心を求める）。先読みが 3 つを超えたら、頭を書く。
    pub fn push(&mut self, packet: OggPacket) -> Result<(), EngineError> {
        let window = packet_window(&packet.packet, self.mode_bits, &self.blockflags)
            .ok_or_else(|| internal("音声パケットの頭"))?;
        let size = |w: &PacketWindow| self.blocksizes[usize::from(w.long)];
        if let Some(prev) = &self.prev {
            // 長いブロックの前後の窓の旗は、実際の隣のブロックの大きさと一致する（最初の前・最後の次は問わない）
            if window.long && window.prev_long != prev.long {
                return Err(internal("長いブロックの前の窓の旗"));
            }
            if prev.long && prev.next_long != window.long {
                return Err(internal("長いブロックの次の窓の旗"));
            }
            self.center += size(prev) / 4 + size(&window) / 4;
        }
        self.pending.push_back(Pending {
            packet,
            window,
            center: self.center,
            index: self.count,
        });
        self.count += 1;
        self.prev = Some(window);
        while self.pending.len() > LOOKAHEAD {
            let Some(p) = self.pending.pop_front() else {
                break;
            };
            // 最後の 3 つでないので、EOS でも A6′ でもない
            let flush = if p.index == 0 {
                Flush::After
            } else if p.window.long_to_short() {
                Flush::Never
            } else {
                Flush::Auto
            };
            self.w.packet(&p.packet.packet, p.center, false, flush)?;
        }
        Ok(())
    }

    /// 残りの先読みを、最後のパケットの決まり（A6′・EOS・グラニュール N）で書いて閉じる。
    pub fn finish(&mut self) -> Result<(), EngineError> {
        let count = self.count;
        if count == 0 {
            return Err(internal("音声パケットの数"));
        }
        let last = self.pending.back().map(|p| p.center);
        let before = (self.pending.len() >= 2)
            .then(|| self.pending.get(self.pending.len() - 2).map(|p| p.center))
            .flatten();
        let covered = match (count, last, before) {
            (1, _, _) => self.total == 0,
            (_, Some(last), Some(before)) => before < self.total && self.total <= last,
            _ => false,
        };
        if !covered {
            return Err(internal("最後のパケットとフレーム数"));
        }
        // A6′：最後のページの前で閉じるパケット（`page_plan` と同じ）
        let long_to_short_at = |k: usize| {
            self.pending
                .iter()
                .find(|p| p.index == k)
                .is_some_and(|p| p.window.long_to_short())
        };
        let before_last = match count {
            1 => None,
            2 => Some(0),
            _ if long_to_short_at(count - 2) => Some(count - 3),
            _ => Some(count - 2),
        };
        while let Some(p) = self.pending.pop_front() {
            let eos = p.index + 1 == count;
            let flush = if eos || p.index == 0 {
                Flush::After
            } else if p.window.long_to_short() {
                Flush::Never
            } else if Some(p.index) == before_last {
                Flush::After
            } else {
                Flush::Auto
            };
            let granule = if eos { self.total } else { p.center };
            self.w.packet(&p.packet.packet, granule, eos, flush)?;
        }
        // 書きかけのページがあれば閉じる（`OggWriter::finish` と同じ）
        self.w.flush_page(false);
        Ok(())
    }
}
