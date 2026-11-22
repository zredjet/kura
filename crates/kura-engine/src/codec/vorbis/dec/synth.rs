//! 合成（デコーダの dsp と block）。
//!
//! | 関数 | C |
//! |---|---|
//! | [`Dsp::new`] | block.c:181–305 `_vds_shared_init`（`encp == 0` の側）・822–851 `vorbis_synthesis_restart`・`vorbis_synthesis_init` |
//! | [`Dsp::synthesis`] | synthesis.c:25–90 `vorbis_synthesis`（数えるだけのときは 93–140 `vorbis_synthesis_trackonly` と同じく値を作らない） |
//! | `mapping0_inverse` | mapping0.c:1324–1579（SSE でない経路。coupling は 1528–1553） |
//! | `blockin` | block.c:905–1173 `vorbis_synthesis_blockin`（SSE でない経路） |
//! | [`Dsp::pcmout`]・[`Dsp::read`] | block.c:1175–1199 |
//!
//! **数えるだけ**（形を読むとき）：`synthesis` の `with_pcm` を偽にすると、パケットの頭（種類・mode・前後の窓）だけを読み、
//! floor・residue・MDCT・重ね合わせをしない。`blockin` の位置の計算（`centerW`・`pcm_returned`・`pcm_current`・グラニュールの
//! 切り取り）は値を作るときと同じ式なので、出る標本の数は値を作るときと同じ（C の `trackonly` は `vb->pcm` を NULL にして位置を
//! 進めないので、そのままは使えない）。数えるだけの `Dsp`（[`Dsp::new`] の `with_pcm` が偽）は codebook の復号の表・floor と
//! residue の look を作らない（形を読む間に表を持たない。符号語の木の確かめも値を作るときに回る）。
//!
//! `halfrate_flag` は 0（`hs = 0`）。`vb->sequence` は Ogg のパケットの番号（libogg の `packetno`。ヘッダが 0〜2）。

use vorbis_tables::{VorbisInfoMapping0, VorbisInfoMode};

use super::bad_header;
use super::bitwise::OggpackReader;
use super::codebook::{DecodeBook, ov_ilog};
use super::floor::{Floor0Look, floor0_inverse1, floor1_inverse1, floor1_inverse2};
use super::info::{Floor, IdHeader, Setup};
use super::res0::Res0Look;
use crate::codec::vorbis::floor1::{VorbisLookFloor1, floor1_look};
use crate::codec::vorbis::mdct::{MdctLookup, mdct_backward, mdct_init};
use crate::codec::vorbis::window::vorbis_window_get;
use crate::error::EngineError;

/// floor の look。
#[derive(Debug, Clone)]
enum FloorLook {
    Zero(Floor0Look),
    One(Box<VorbisLookFloor1>),
}

/// inverse1 の結果（C の `void *floormemo`）。
enum FloorMemo {
    None,
    Zero(Vec<f32>),
    One(Vec<i32>),
}

impl FloorMemo {
    fn is_some(&self) -> bool {
        !matches!(self, FloorMemo::None)
    }
}

/// 復号した 1 ブロック（`vorbis_block` の復号の欄）。
pub(crate) struct Block {
    pub w: i64,
    pub granulepos: i64,
    pub sequence: i64,
    pub eofflag: bool,
    /// 値を作ったか（数えるだけなら偽）。
    pub with_pcm: bool,
}

/// `vorbis_dsp_state` と `private_state` の復号の欄。
pub(crate) struct Dsp {
    pub channels: usize,
    pub blocksizes: [i64; 2],
    modebits: i32,
    modes: Vec<VorbisInfoMode>,
    maps: Vec<VorbisInfoMapping0>,
    /// 値を作れる（復号の表と look を作った）か。
    with_pcm: bool,
    books: Vec<DecodeBook>,
    floors: Vec<FloorLook>,
    residues: Vec<(i32, Res0Look)>,
    transform: [MdctLookup; 2],
    window: [i32; 2],
    /// `v->pcm`（チャンネルごとに `blocksizes[1]`）。
    pcm: Vec<Vec<f32>>,
    /// ブロックの作業列（`vb->pcm`。チャンネルごとに `blocksizes[1]`）。
    block_pcm: Vec<Vec<f32>>,
    lw: i64,
    w: i64,
    center_w: i64,
    pub pcm_current: i64,
    pub pcm_returned: i64,
    granulepos: i64,
    sequence: i64,
    eofflag: bool,
    sample_count: i64,
}

impl Dsp {
    /// `vorbis_synthesis_init`（`_vds_shared_init` の復号の側と `vorbis_synthesis_restart`）。codebook の表の量は setup を
    /// 読むときに予算で確かめてある（`info::unpack_books`）。`with_pcm` が偽なら数えるだけ（表と look を作らない。冒頭）。
    pub fn new(id: &IdHeader, setup: &Setup, with_pcm: bool) -> Result<Self, EngineError> {
        let blocksizes = id.blocksizes;
        if setup.modes.is_empty() || blocksizes[0] < 64 || blocksizes[1] < blocksizes[0] {
            return Err(bad_header("setup が正しくありません"));
        }
        let mut books = Vec::new();
        let mut floors = Vec::new();
        let mut residues = Vec::new();
        if with_pcm {
            books.reserve_exact(setup.books.len());
            for b in &setup.books {
                books.push(
                    DecodeBook::init(b)?
                        .ok_or_else(|| bad_header("codebook の符号語の木が正しくありません"))?,
                );
            }
            floors = setup
                .floors
                .iter()
                .map(|f| match f {
                    Floor::Zero(i) => Ok(FloorLook::Zero(Floor0Look::new(i))),
                    Floor::One(i) => floor1_look(i)
                        .map(|l| FloorLook::One(Box::new(l)))
                        .map_err(|_| bad_header("floor 1 の設定が正しくありません")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            residues = setup
                .residues
                .iter()
                .zip(&setup.residue_types)
                .map(|(r, t)| Ok((*t, Res0Look::new(r, &books)?)))
                .collect::<Result<Vec<_>, EngineError>>()?;
        }
        let channels = id.channels as usize;
        let storage = blocksizes[1] as usize;
        let pcm = vec![vec![0f32; storage]; channels];
        let block_pcm = vec![vec![0f32; storage]; channels];
        Ok(Self {
            channels,
            blocksizes,
            modebits: ov_ilog((setup.modes.len() - 1) as u32),
            modes: setup.modes.clone(),
            maps: setup.maps.clone(),
            with_pcm,
            books,
            floors,
            residues,
            transform: [
                mdct_init(blocksizes[0] as i32),
                mdct_init(blocksizes[1] as i32),
            ],
            window: [
                ov_ilog(blocksizes[0] as u32) - 7,
                ov_ilog(blocksizes[1] as u32) - 7,
            ],
            pcm,
            block_pcm,
            lw: 0,
            w: 0,
            // vorbis_synthesis_restart
            center_w: blocksizes[1] >> 1,
            pcm_current: blocksizes[1] >> 1,
            pcm_returned: -1,
            granulepos: -1,
            sequence: -1,
            eofflag: false,
            sample_count: -1,
        })
    }

    /// `vorbis_synthesis`（synthesis.c:25–90）。音声のパケットでなければ誤り（ADR-217 (2)）。`with_pcm` が偽なら値を作らない。
    pub fn synthesis(
        &mut self,
        packet: &[u8],
        granulepos: i64,
        packetno: i64,
        eos: bool,
        with_pcm: bool,
    ) -> Result<Block, EngineError> {
        let mut opb = OggpackReader::new(packet);
        // Check the packet type
        if opb.read(1) != 0 {
            return Err(bad_header("音声のパケットではありません"));
        }
        // read our mode and pre/post windowsize
        let mode = opb.read(self.modebits);
        let Some(m) = usize::try_from(mode)
            .ok()
            .and_then(|m| self.modes.get(m))
            .copied()
        else {
            return Err(bad_header("音声のパケットの mode が正しくありません"));
        };
        let w = i64::from(m.blockflag);
        if w != 0 {
            // this doesn;t get mapped through mode selection as it's used only for window selection
            let _lw = opb.read(1);
            let nw = opb.read(1);
            if nw == -1 {
                return Err(bad_header("音声のパケットが切れています"));
            }
        }
        let block = Block {
            w,
            granulepos,
            sequence: packetno,
            eofflag: eos,
            with_pcm,
        };
        if with_pcm {
            if !self.with_pcm {
                return Err(EngineError::Internal(
                    "数えるだけの Vorbis の合成で値を作ろうとした".into(),
                ));
            }
            let map = self.maps[m.mapping as usize];
            self.mapping0_inverse(&map, w, &mut opb)?;
        }
        Ok(block)
    }

    /// `mapping0_inverse`（mapping0.c:1324–1579）。`self.block_pcm` に窓の長さ `n` の値を作る。
    fn mapping0_inverse(
        &mut self,
        info: &VorbisInfoMapping0,
        w: i64,
        opb: &mut OggpackReader<'_>,
    ) -> Result<(), EngineError> {
        let n = self.blocksizes[w as usize];
        let half = (n / 2) as usize;
        let channels = self.channels;
        // recover the spectral envelope; store it in the PCM vector for now
        let mut memos: Vec<FloorMemo> = Vec::with_capacity(channels);
        for i in 0..channels {
            let submap = info.chmuxlist[i] as usize;
            let floor = info.floorsubmap[submap] as usize;
            let memo = match &self.floors[floor] {
                FloorLook::Zero(l) => match floor0_inverse1(l, &self.books, opb)? {
                    Some(v) => FloorMemo::Zero(v),
                    None => FloorMemo::None,
                },
                FloorLook::One(l) => match floor1_inverse1(l, &self.books, opb) {
                    Some(v) => FloorMemo::One(v),
                    None => FloorMemo::None,
                },
            };
            memos.push(memo);
            self.block_pcm[i][..half].fill(0.0);
        }
        let mut nonzero: Vec<bool> = memos.iter().map(FloorMemo::is_some).collect();
        // channel coupling can 'dirty' the nonzero listing
        for i in 0..info.coupling_steps as usize {
            let (mag, ang) = (info.coupling_mag[i] as usize, info.coupling_ang[i] as usize);
            if nonzero[mag] || nonzero[ang] {
                nonzero[mag] = true;
                nonzero[ang] = true;
            }
        }
        // recover the residue into our working vectors
        for i in 0..info.submaps {
            let mut bundle: Vec<&mut [f32]> = Vec::new();
            let mut zerobundle: Vec<bool> = Vec::new();
            for (j, p) in self.block_pcm.iter_mut().enumerate() {
                if info.chmuxlist[j] == i {
                    zerobundle.push(nonzero[j]);
                    bundle.push(&mut p[..n as usize]);
                }
            }
            let (kind, look) = &self.residues[info.residuesubmap[i as usize] as usize];
            look.inverse(*kind, &self.books, opb, n, &mut bundle, &zerobundle)?;
        }
        // channel coupling（mag と ang は違うチャンネル：mapping0_unpack が確かめる）
        for i in (0..info.coupling_steps as usize).rev() {
            let (mag, ang) = (info.coupling_mag[i] as usize, info.coupling_ang[i] as usize);
            let Some((pcm_m, pcm_a)) = two_mut(&mut self.block_pcm, mag, ang) else {
                return Err(bad_header("coupling のチャンネルが正しくありません"));
            };
            for (pm, pa) in pcm_m[..half].iter_mut().zip(&mut pcm_a[..half]) {
                let (m, a) = (*pm, *pa);
                let (nm, na) = if m > 0.0 {
                    if a > 0.0 { (m, m - a) } else { (m + a, m) }
                } else if a > 0.0 {
                    (m, m + a)
                } else {
                    (m - a, m)
                };
                *pm = nm;
                *pa = na;
            }
        }
        // compute and apply spectral envelope
        for (i, memo) in memos.iter_mut().enumerate() {
            let submap = info.chmuxlist[i] as usize;
            let floor = info.floorsubmap[submap] as usize;
            let out = &mut self.block_pcm[i];
            match (&mut self.floors[floor], memo) {
                (FloorLook::Zero(l), FloorMemo::Zero(lsp)) => {
                    l.inverse2(w as usize, n, Some(lsp), out);
                }
                (FloorLook::Zero(l), _) => {
                    l.inverse2(w as usize, n, None, out);
                }
                (FloorLook::One(l), FloorMemo::One(fit)) => {
                    floor1_inverse2(l, half as i32, Some(fit), out);
                }
                (FloorLook::One(l), _) => {
                    floor1_inverse2(l, half as i32, None, out);
                }
            }
        }
        // transform the PCM data; takes PCM vector, vb; modifies PCM vector
        for p in self.block_pcm.iter_mut() {
            mdct_backward(&self.transform[w as usize], &mut p[..n as usize]);
        }
        Ok(())
    }

    /// `vorbis_synthesis_blockin`（block.c:905–1173）。切り取る前に出せるようになった標本の数（重ね合わせで決まった長さ。最初の
    /// ブロックは 0）を返す（デコードする量の上限の確かめに使う。値には効かない）。
    pub fn blockin(&mut self, vb: &Block) -> Result<i64, EngineError> {
        // 読み手はいつも出せる標本を読み終えてから次を入れるので、ここに来るのはグラニュールで位置が折り返したとき
        // だけ（下の「trim the beginning」）。C の `decoder_example` は OV_EINVAL を無視して以後なにも出さない。kura は壊れた
        // ファイルとして誤りにする（ADR-217 (2)）
        if self.pcm_current > self.pcm_returned && self.pcm_returned != -1 {
            return Err(bad_header("グラニュールが正しくありません"));
        }
        self.lw = self.w;
        self.w = vb.w;
        if self.sequence == -1 || self.sequence + 1 != vb.sequence {
            // out of sequence; lose count
            self.granulepos = -1;
            self.sample_count = -1;
        }
        self.sequence = vb.sequence;
        let bs = self.blocksizes;
        let n = (bs[self.w as usize] >> 1) as usize;
        let n0 = (bs[0] >> 1) as usize;
        let n1 = (bs[1] >> 1) as usize;
        let (this_center, prev_center) = if self.center_w != 0 { (n1, 0) } else { (0, n1) };
        if vb.with_pcm {
            for j in 0..self.channels {
                let pcm = &mut self.pcm[j];
                let p = &self.block_pcm[j];
                // the overlap/add section
                if self.lw != 0 {
                    if self.w != 0 {
                        // large/large
                        let win = vorbis_window_get(self.window[1]);
                        overlap_add(&mut pcm[prev_center..], p, &win[..n1]);
                    } else {
                        // large/small
                        let win = vorbis_window_get(self.window[0]);
                        overlap_add(&mut pcm[prev_center + n1 / 2 - n0 / 2..], p, &win[..n0]);
                    }
                } else if self.w != 0 {
                    // small/large
                    let win = vorbis_window_get(self.window[0]);
                    let pcm = &mut pcm[prev_center..];
                    let p = &p[n1 / 2 - n0 / 2..];
                    overlap_add(pcm, p, &win[..n0]);
                    let rest = n0..n1 / 2 + n0 / 2;
                    pcm[rest.clone()].copy_from_slice(&p[rest]);
                } else {
                    // small/small
                    let win = vorbis_window_get(self.window[0]);
                    overlap_add(&mut pcm[prev_center..], p, &win[..n0]);
                }
                // the copy section
                pcm[this_center..this_center + n].copy_from_slice(&p[n..n + n]);
            }
        }
        // 位置（数えるだけのときも同じ式）
        self.center_w = if self.center_w != 0 { 0 } else { n1 as i64 };
        // deal with initial packet state
        if self.pcm_returned == -1 {
            self.pcm_returned = this_center as i64;
            self.pcm_current = this_center as i64;
        } else {
            self.pcm_returned = prev_center as i64;
            self.pcm_current =
                prev_center as i64 + (bs[self.lw as usize] / 4 + bs[self.w as usize] / 4);
        }
        let nominal = self.pcm_current - self.pcm_returned;
        // track the frame number
        if self.sample_count == -1 {
            self.sample_count = 0;
        } else {
            self.sample_count += bs[self.lw as usize] / 4 + bs[self.w as usize] / 4;
        }
        if self.granulepos == -1 {
            // only set if we have a position to set to
            if vb.granulepos != -1 {
                self.granulepos = vb.granulepos;
                // is this a short page?
                if self.sample_count > self.granulepos {
                    // corner case; if this is both the first and last audio page, then spec says the end is cut
                    let mut extra = self.sample_count.wrapping_sub(vb.granulepos);
                    if extra < 0 {
                        extra = 0;
                    }
                    if vb.eofflag {
                        // trim the end. Guard against corrupt/malicious frames that set EOP and a backdated granpos
                        if extra > self.pcm_current - self.pcm_returned {
                            extra = self.pcm_current - self.pcm_returned;
                        }
                        self.pcm_current -= extra;
                    } else {
                        // trim the beginning（C は -fwrapv で折り返す）
                        self.pcm_returned = self.pcm_returned.wrapping_add(extra);
                        if self.pcm_returned > self.pcm_current {
                            self.pcm_returned = self.pcm_current;
                        }
                    }
                }
            }
        } else {
            // C は -fwrapv で折り返す
            self.granulepos = self
                .granulepos
                .wrapping_add(bs[self.lw as usize] / 4 + bs[self.w as usize] / 4);
            if vb.granulepos != -1 && self.granulepos != vb.granulepos {
                if self.granulepos > vb.granulepos {
                    let mut extra = self.granulepos.wrapping_sub(vb.granulepos);
                    if extra != 0 && vb.eofflag {
                        // partial last frame.  Strip the extra samples off
                        if extra > self.pcm_current - self.pcm_returned {
                            extra = self.pcm_current - self.pcm_returned;
                        }
                        if extra < 0 {
                            extra = 0;
                        }
                        self.pcm_current -= extra;
                    }
                }
                self.granulepos = vb.granulepos;
            }
        }
        // Update, cleanup
        if vb.eofflag {
            self.eofflag = true;
        }
        Ok(nominal)
    }

    /// `vorbis_synthesis_pcmout`（block.c:1175–1188）：出せる標本の数（`pcm_returned` から）。
    pub fn pcmout(&self) -> i64 {
        if self.pcm_returned > -1 && self.pcm_returned < self.pcm_current {
            self.pcm_current - self.pcm_returned
        } else {
            0
        }
    }

    /// 出せる標本（Vorbis のチャンネル `ch` の、`pcm_returned` からの `len` 個）。
    pub fn pcm_slice(&self, ch: usize, len: usize) -> &[f32] {
        let start = self.pcm_returned as usize;
        &self.pcm[ch][start..start + len]
    }

    /// `vorbis_synthesis_read`（block.c:1190–1199）。
    pub fn read(&mut self, n: i64) -> Result<(), EngineError> {
        if n != 0 && self.pcm_returned + n > self.pcm_current {
            return Err(EngineError::Internal(
                "Vorbis：出した標本より多く読もうとしました".into(),
            ));
        }
        self.pcm_returned += n;
        Ok(())
    }
}

/// 重ね合わせ（block.c の `pcm[i]=pcm[i]*w[n-i-1] + p[i]*w[i]`。`n` は `win` の長さ。式と順は C のまま）。
fn overlap_add(pcm: &mut [f32], p: &[f32], win: &[f32]) {
    let n = win.len();
    for ((x, &v), (&wr, &wf)) in pcm[..n]
        .iter_mut()
        .zip(&p[..n])
        .zip(win.iter().rev().zip(win))
    {
        *x = *x * wr + v * wf;
    }
}

/// 違う 2 つのチャンネルの列を同時に借りる（同じか範囲の外なら None）。
fn two_mut(v: &mut [Vec<f32>], a: usize, b: usize) -> Option<(&mut [f32], &mut [f32])> {
    if a == b || a >= v.len() || b >= v.len() {
        return None;
    }
    if a < b {
        let (l, r) = v.split_at_mut(b);
        Some((&mut l[a], &mut r[0]))
    } else {
        let (l, r) = v.split_at_mut(a);
        Some((&mut r[0], &mut l[b]))
    }
}
