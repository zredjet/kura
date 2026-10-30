//! `kura-pcm-v1` を区間の順によらず取る（M13 段 2。docs/editing.md §8.6）：[`PcmTreeDigest`]。
//!
//! 値のダイジェストは blake3("kura-pcm-v1" の前置き ‖ チャンネル 0 の全標本 ‖ チャンネル 1 … ‖ メタデータ) で、標本は
//! チャンネルの順に並ぶ（`audio::stream::PcmDigest`）。流れ（デコード・計画の出力）は窓ごとに全チャンネルを渡すので、
//! チャンネルの順に足すにはチャンネルの数だけ流し直すことになる。ここでは blake3 の木（hazmat）を使い、各区間のバイト列の
//! 位置（入力の中のオフセット）を決めて部分木の連鎖値を求め、最後に標準の木の形に組む。式（バイト列）は `PcmDigest` と同じ
//! なので、ダイジェストはビット単位で同じ（性質テストで照らす）。
//!
//! - 入力のバイト列の位置：前置き P = 35 バイト（ドメインの長さ 8 ＋ "kura-pcm-v1" 11 ＋ rate 4 ＋ channels 4 ＋ frames 8）、
//!   チャンネル c の標本 i は P + 4(cF + i)、メタデータ（長さ 8 ＋ 正規の JSON）は S = P + 4CF から。
//! - 区間 [a, b) は、1 KiB の塊に揃わない頭と尻を断片（塊ごとの作業列）に入れ、間を揃った 2 の冪の部分木
//!   （`hazmat::max_subtree_len` と残りの長さで決める。並列のために 1 MiB まで）に分けて連鎖値にする。入力の中に収まる揃った
//!   2 の冪の塊は、全体でなければ必ず標準の木（左に寄せた木）の節になる。断片の塊が埋まれば、その塊も連鎖値にする。
//! - 連鎖値は位置の表（`BTreeMap`）に入れ、隣の同じ大きさの兄弟とすぐまとめる（親が全体になるものはまとめない）。どの節も
//!   [0, S] の中にあり、全体の長さ L は S ＋ 8 以上なので、`finish` の前に根はできない。
//! - `finish`：メタデータを足し、残りの断片を塊ごとに連鎖値にして、標準の木を `left_subtree_len` で根から下る（明示の積み上げで。
//!   再帰しない）。表に（位置, 長さ）があればそれを、なければ二つに割る。根は `merge_subtrees_root`。
//! - 小さい音声（4CF ≤ 1 MiB）は木を使わず、標本を持って `finish` で `PcmDigest` に通す（L ≤ 1 KiB の端を避ける）。
//! - hazmat の関数は使い方を誤ると assert（panic）する。前提は作り方で守り、さらに `push`・`finish` の中を `catch_unwind` で
//!   包んで `Internal` にする。hazmat はこのファイルの外で使わない（.claude/rules/engine.md）。

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use blake3::hazmat::{
    ChainingValue, HasherExt, Mode, left_subtree_len, max_subtree_len, merge_subtrees_non_root,
    merge_subtrees_root,
};

use crate::audio::stream::PcmDigest;
use crate::determinism::par_blocks;
use crate::digest::{Digest, canonical};
use crate::error::EngineError;
use crate::monitor::{Monitor, NoMonitor};
use crate::value::AudioMeta;

/// blake3 の塊の長さ（バイト）。
const CHUNK: u64 = blake3::CHUNK_LEN as u64;
/// 前置きの長さ（ドメインの長さ 8 ＋ "kura-pcm-v1" 11 ＋ rate 4 ＋ channels 4 ＋ frames 8）。
const PREFIX: u64 = 8 + 11 + 4 + 4 + 8;
/// 標本をバイト列に変えて足す数（作業列は 256 KiB。値に効かない）。
const BATCH: usize = 64 * 1024;
/// 1 つの仕事の部分木の大きさの上限（並列の分け方だけ。値に効かない）。
const SUBTREE_MAX: u64 = 1 << 20;
/// 木を使わない大きさ（標本のバイト数 4CF がこれ以下なら標本を持つ）。
const SMALL_BYTES: u64 = 1 << 20;

/// 標本を足した区間の表（チャンネルごと。[start, end) を重ならずに持ち、隣り合うものはまとめる）。
#[derive(Debug, Clone, Default)]
struct Covered(BTreeMap<u64, u64>);

impl Covered {
    /// [s, e) を足す。重なれば false（何も足さない）。
    fn insert(&mut self, s: u64, e: u64) -> bool {
        if s >= e {
            return true;
        }
        if let Some((_, pe)) = self.0.range(..=s).next_back()
            && *pe > s
        {
            return false;
        }
        if self.0.range(s..e).next().is_some() {
            return false;
        }
        let mut start = s;
        let mut end = e;
        if let Some((ps, pe)) = self.0.range(..s).next_back().map(|(a, b)| (*a, *b))
            && pe == s
        {
            self.0.remove(&ps);
            start = ps;
        }
        if let Some(ne) = self.0.get(&e).copied() {
            self.0.remove(&e);
            end = ne;
        }
        self.0.insert(start, end);
        true
    }

    /// [0, n) を覆っているか。
    fn covers(&self, n: u64) -> bool {
        n == 0 || self.0.get(&0) == Some(&n)
    }
}

/// 埋めている途中の塊（1 KiB）。
struct Frag {
    buf: Vec<u8>,
    filled: u64,
}

/// 区間のバイト列の作り方（標本か、生のバイト列）。
enum Piece<'a> {
    /// 位置 `at` から始まる標本（4 バイトずつ。NaN は 0x7fc00000 にそろえる）。
    Samples { at: u64, samples: &'a [f32] },
    /// 位置 `at` から始まる生のバイト列。
    Raw { at: u64, data: &'a [u8] },
}

impl Piece<'_> {
    fn start(&self) -> u64 {
        match self {
            Piece::Samples { at, .. } | Piece::Raw { at, .. } => *at,
        }
    }

    fn end(&self) -> u64 {
        match self {
            Piece::Samples { at, samples } => at + 4 * samples.len() as u64,
            Piece::Raw { at, data } => at + data.len() as u64,
        }
    }

    /// 絶対の位置 [x, x + n) のバイト列を `out` の後ろに足す（区間の中であること）。
    fn write(&self, x: u64, n: usize, out: &mut Vec<u8>) {
        match self {
            Piece::Raw { at, data } => {
                let from = (x - at) as usize;
                out.extend_from_slice(&data[from..from + n]);
            }
            Piece::Samples { at, samples } => {
                let rel = x - at;
                let first = (rel / 4) as usize;
                let skip = (rel % 4) as usize;
                let last = ((rel + n as u64).div_ceil(4)) as usize;
                let base = out.len();
                for s in &samples[first..last] {
                    // NaN は 1 つの値にそろえる（`PcmDigest` と同じ）
                    let bits = if s.is_nan() { 0x7fc0_0000 } else { s.to_bits() };
                    out.extend_from_slice(&bits.to_le_bytes());
                }
                out.drain(base..base + skip);
                out.truncate(base + n);
            }
        }
    }
}

/// 木の状態（大きい音声）。
struct Tree {
    /// 連鎖値の表：位置 →（長さ, 連鎖値）。どれも標準の木の節。
    nodes: BTreeMap<u64, (u64, ChainingValue)>,
    /// 埋めている途中の塊：塊の番号 → 作業列。
    frags: BTreeMap<u64, Frag>,
    /// 全体の長さ（`finish` で決まる。それまでは u64::MAX）。
    total: u64,
}

impl Tree {
    /// 連鎖値を表に入れ、隣の同じ大きさの兄弟とまとめる（親が全体になるものはまとめない）。
    fn insert(&mut self, mut pos: u64, mut len: u64, mut cv: ChainingValue) {
        loop {
            let parent = len * 2;
            let (sib, left) = if pos.is_multiple_of(parent) {
                (pos + len, true)
            } else {
                (pos - len, false)
            };
            let parent_pos = if left { pos } else { sib };
            let whole = parent_pos == 0 && parent >= self.total;
            if len.is_power_of_two()
                && len >= CHUNK
                && !whole
                && let Some(&(sl, scv)) = self.nodes.get(&sib)
                && sl == len
            {
                self.nodes.remove(&sib);
                cv = if left {
                    merge_subtrees_non_root(&cv, &scv, Mode::Hash)
                } else {
                    merge_subtrees_non_root(&scv, &cv, Mode::Hash)
                };
                pos = parent_pos;
                len = parent;
                continue;
            }
            self.nodes.insert(pos, (len, cv));
            return;
        }
    }

    /// [x, x + n) のバイト列を塊ごとの断片に足す（埋まった塊は連鎖値にする）。
    fn add_frag(&mut self, piece: &Piece<'_>, x: u64, n: u64) -> Result<(), EngineError> {
        let end = x + n;
        let mut at = x;
        while at < end {
            let k = at / CHUNK;
            let take = ((k + 1) * CHUNK).min(end) - at;
            self.add_in_chunk(piece, at, take)?;
            at += take;
        }
        Ok(())
    }

    /// 塊の中の [x, x + n) を足す。
    fn add_in_chunk(&mut self, piece: &Piece<'_>, x: u64, n: u64) -> Result<(), EngineError> {
        let k = x / CHUNK;
        let off = (x - k * CHUNK) as usize;
        let f = self.frags.entry(k).or_insert_with(|| Frag {
            buf: vec![0; CHUNK as usize],
            filled: 0,
        });
        let mut tmp = Vec::with_capacity(n as usize);
        piece.write(x, n as usize, &mut tmp);
        f.buf[off..off + n as usize].copy_from_slice(&tmp);
        f.filled += n;
        if f.filled > CHUNK {
            return Err(EngineError::Internal(
                "音声のダイジェストの塊が重なりました".into(),
            ));
        }
        let full = f.filled == CHUNK && (k + 1) * CHUNK <= self.total;
        if full && let Some(f) = self.frags.remove(&k) {
            let cv = chunk_cv(k * CHUNK, &f.buf);
            self.insert(k * CHUNK, CHUNK, cv);
        }
        Ok(())
    }
}

/// 位置 `pos` の塊（1 KiB 以下）の連鎖値。
fn chunk_cv(pos: u64, bytes: &[u8]) -> ChainingValue {
    let mut h = blake3::Hasher::new();
    h.set_input_offset(pos);
    h.update(bytes);
    h.finalize_non_root()
}

/// バイト列の区間（位置, 長さ）の列。
type Ranges = Vec<(u64, u64)>;

/// 区間を、頭と尻の断片（[x, x + n) の組）と、揃った部分木（位置, 長さ）に分ける。
fn split(a: u64, b: u64) -> (Ranges, Ranges) {
    let first = a.div_ceil(CHUNK) * CHUNK;
    let last = b / CHUNK * CHUNK;
    if first >= last {
        return (vec![(a, b - a)], Vec::new());
    }
    let mut trees = Vec::new();
    let mut x = first;
    while x < last {
        let rest = last - x;
        let mut size = 1u64 << (63 - rest.leading_zeros());
        if let Some(m) = max_subtree_len(x) {
            size = size.min(m);
        }
        size = size.min(SUBTREE_MAX);
        trees.push((x, size));
        x += size;
    }
    (vec![(a, first - a), (last, b - last)], trees)
}

/// 部分木 [x, x + size) の連鎖値（`piece` から、`BATCH` 個ずつバイト列にして足す）。
fn subtree_cv(piece: &Piece<'_>, x: u64, size: u64) -> ChainingValue {
    let mut h = blake3::Hasher::new();
    h.set_input_offset(x);
    let step = (BATCH * 4) as u64;
    let mut buf = Vec::with_capacity(BATCH * 4 + 8);
    let mut at = x;
    while at < x + size {
        let n = step.min(x + size - at);
        buf.clear();
        piece.write(at, n as usize, &mut buf);
        h.update(&buf);
        at += n;
    }
    h.finalize_non_root()
}

/// 小さい音声の状態（標本を持つ）。
struct Small {
    planes: Vec<Vec<f32>>,
}

enum State {
    Small(Small),
    Tree(Tree),
}

/// `kura-pcm-v1` のダイジェストを、区間をどの順に受けても 1 回流すだけで作る（モジュールの説明）。持つ量は、並列の仕事ごとの
/// 作業列（256 KiB）と連鎖値の表・断片（区間の境ごとに 1 KiB）。小さい音声（4CF ≤ 1 MiB）は標本を持つ。
pub struct PcmTreeDigest {
    rate: u32,
    channels: usize,
    frames: u64,
    covered: Vec<Covered>,
    state: State,
}

/// panic を受け止めて `Internal` にする（hazmat の assert）。
fn guarded<T>(f: impl FnOnce() -> Result<T, EngineError>) -> Result<T, EngineError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(EngineError::Internal(
            "音声のダイジェストの木の組み方を誤りました".into(),
        )),
    }
}

impl PcmTreeDigest {
    /// レート `rate`・`channels` チャンネル・長さ `frames` の音声のダイジェストを作り始める。
    pub fn new(rate: u32, channels: usize, frames: u64) -> Result<Self, EngineError> {
        let too_long = || EngineError::Limit("音声が長すぎます".into());
        let samples = (channels as u64).checked_mul(frames).ok_or_else(too_long)?;
        let bytes = samples.checked_mul(4).ok_or_else(too_long)?;
        PREFIX
            .checked_add(bytes)
            .and_then(|s| s.checked_add(1 << 40))
            .ok_or_else(too_long)?;
        let state = if bytes <= SMALL_BYTES {
            State::Small(Small {
                planes: (0..channels)
                    .map(|_| crate::codec::try_vec(frames as usize, 0f32))
                    .collect::<Result<_, _>>()?,
            })
        } else {
            let mut tree = Tree {
                nodes: BTreeMap::new(),
                frags: BTreeMap::new(),
                total: u64::MAX,
            };
            let mut prefix = Vec::with_capacity(PREFIX as usize);
            prefix.extend_from_slice(&11u64.to_le_bytes());
            prefix.extend_from_slice(b"kura-pcm-v1");
            prefix.extend_from_slice(&rate.to_le_bytes());
            prefix.extend_from_slice(&(channels as u32).to_le_bytes());
            prefix.extend_from_slice(&frames.to_le_bytes());
            tree.add_frag(
                &Piece::Raw {
                    at: 0,
                    data: &prefix,
                },
                0,
                PREFIX,
            )?;
            State::Tree(tree)
        };
        Ok(Self {
            rate,
            channels,
            frames,
            covered: vec![Covered::default(); channels],
            state,
        })
    }

    /// 並列の仕事 `tasks` 個で足すときの作業領域の見積もり（バイト）：小さい音声の標本（1 MiB まで）か、仕事ごとの作業列
    /// （256 KiB）と、連鎖値の表・断片の分。
    pub fn work_bytes(channels: usize, tasks: u64) -> u64 {
        let per_task = (BATCH * 4 + 8) as u64 + 1024;
        SMALL_BYTES
            .max(tasks.max(1).saturating_mul(per_task))
            .saturating_add((channels as u64).saturating_mul(4 * CHUNK))
            .saturating_add(64 << 10)
    }

    /// 足した区間を確かめて覚える（範囲の外・重なりは `Internal`）。
    fn claim(&mut self, ch: usize, start: u64, n: u64) -> Result<(), EngineError> {
        let end = start.checked_add(n);
        let ok = ch < self.channels
            && end.is_some_and(|e| e <= self.frames)
            && self.covered[ch].insert(start, start + n);
        if ok {
            Ok(())
        } else {
            Err(EngineError::Internal(format!(
                "音声のダイジェストの区間が範囲の外か重なります（チャンネル {ch}・{start} から {n} フレーム・長さ {}）",
                self.frames
            )))
        }
    }

    /// 標本の位置（バイト）。
    fn sample_pos(&self, ch: usize, start: u64) -> u64 {
        PREFIX + 4 * (ch as u64 * self.frames + start)
    }

    /// チャンネル `ch` のフレーム `start` からの標本を足す（順は問わない。範囲の外・重なりは `Internal`）。
    pub fn push(&mut self, ch: usize, start: u64, samples: &[f32]) -> Result<(), EngineError> {
        self.push_pieces(&[(ch, start, samples)], None)
    }

    /// フレーム `start` からの全チャンネルの標本（チャンネルの順・同じ長さ）を足す。部分木の連鎖値は `par_blocks` で並列に
    /// 求める（値に効かない）。
    pub fn push_window(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        if planes.len() != self.channels {
            return Err(EngineError::Internal(format!(
                "音声のダイジェストの窓のチャンネルの数が違います（{} と {}）",
                planes.len(),
                self.channels
            )));
        }
        let n = planes.first().map_or(0, |p| p.len());
        if planes.iter().any(|p| p.len() != n) {
            return Err(EngineError::Internal(
                "音声のダイジェストの窓のチャンネルの長さがそろっていません".into(),
            ));
        }
        let pieces: Vec<(usize, u64, &[f32])> = planes
            .iter()
            .enumerate()
            .map(|(ch, p)| (ch, start, *p))
            .collect();
        self.push_pieces(&pieces, Some(mon))
    }

    fn push_pieces(
        &mut self,
        pieces: &[(usize, u64, &[f32])],
        par: Option<&dyn Monitor>,
    ) -> Result<(), EngineError> {
        for &(ch, start, samples) in pieces {
            self.claim(ch, start, samples.len() as u64)?;
        }
        let frames = self.frames as usize;
        if let State::Small(s) = &mut self.state {
            for &(ch, start, samples) in pieces {
                let start = start as usize;
                if start + samples.len() > frames {
                    return Err(EngineError::Internal(
                        "音声のダイジェストの区間が範囲の外です".into(),
                    ));
                }
                s.planes[ch][start..start + samples.len()].copy_from_slice(samples);
            }
            return Ok(());
        }
        let located: Vec<Piece<'_>> = pieces
            .iter()
            .map(|&(ch, start, samples)| Piece::Samples {
                at: self.sample_pos(ch, start),
                samples,
            })
            .collect();
        let State::Tree(tree) = &mut self.state else {
            return Ok(());
        };
        guarded(|| add_pieces(tree, &located, par))
    }

    /// すべての標本を足したか。
    pub fn is_complete(&self) -> bool {
        self.covered.iter().all(|c| c.covers(self.frames))
    }

    /// 全標本を足したあと、メタデータを足してダイジェストを返す（足りなければ `Internal`）。
    pub fn finish(self, meta: &AudioMeta) -> Result<Digest, EngineError> {
        if !self.is_complete() {
            return Err(EngineError::Internal(format!(
                "音声のダイジェストの標本が足りません（長さ {}）",
                self.frames
            )));
        }
        match self.state {
            State::Small(s) => {
                let mut d = PcmDigest::new(self.rate, self.channels, self.frames);
                for (ch, p) in s.planes.iter().enumerate() {
                    d.push(ch, 0, p)?;
                }
                d.finish(meta)
            }
            State::Tree(mut tree) => {
                let json = canonical(meta)?;
                let s = PREFIX + 4 * self.channels as u64 * self.frames;
                let mut suffix = Vec::new();
                suffix
                    .try_reserve_exact(8 + json.len())
                    .map_err(|_| EngineError::Limit("メタデータが大きすぎます".into()))?;
                suffix.extend_from_slice(&(json.len() as u64).to_le_bytes());
                suffix.extend_from_slice(&json);
                let total = s + suffix.len() as u64;
                guarded(|| finish_tree(&mut tree, s, &suffix, total))
            }
        }
    }
}

/// 区間を木に足す：断片は表へ、揃った部分木は（`par` があれば並列に）連鎖値にして表へ。
fn add_pieces(
    tree: &mut Tree,
    pieces: &[Piece<'_>],
    par: Option<&dyn Monitor>,
) -> Result<(), EngineError> {
    let mut jobs: Vec<(usize, u64, u64)> = Vec::new();
    for (i, p) in pieces.iter().enumerate() {
        let (a, b) = (p.start(), p.end());
        if a == b {
            continue;
        }
        let (frags, trees) = split(a, b);
        for (x, n) in frags {
            tree.add_frag(p, x, n)?;
        }
        jobs.extend(trees.into_iter().map(|(x, size)| (i, x, size)));
    }
    let cvs: Vec<ChainingValue> = match par {
        Some(mon) => par_blocks(jobs.len(), mon, |j| {
            let (i, x, size) = jobs[j];
            Ok(subtree_cv(&pieces[i], x, size))
        })?,
        None => jobs
            .iter()
            .map(|&(i, x, size)| subtree_cv(&pieces[i], x, size))
            .collect(),
    };
    for ((_, x, size), cv) in jobs.iter().zip(cvs) {
        tree.insert(*x, *size, cv);
    }
    Ok(())
}

/// メタデータを足し、残りの塊を連鎖値にして、標準の木を根から下って根を作る。
fn finish_tree(tree: &mut Tree, s: u64, suffix: &[u8], total: u64) -> Result<Digest, EngineError> {
    tree.total = total;
    add_pieces(
        tree,
        &[Piece::Raw {
            at: s,
            data: suffix,
        }],
        Some(&NoMonitor),
    )?;
    // 残りの断片（区間の境の塊と、最後の短い塊）：埋まっているはず（最後の塊は短くてよい）
    let frags = std::mem::take(&mut tree.frags);
    for (k, f) in frags {
        let pos = k * CHUNK;
        let len = CHUNK.min(total - pos);
        if f.filled != len {
            return Err(EngineError::Internal(format!(
                "音声のダイジェストの塊 {k} が埋まっていません（{} / {len}）",
                f.filled
            )));
        }
        let cv = chunk_cv(pos, &f.buf[..len as usize]);
        tree.nodes.insert(pos, (len, cv));
    }
    if total <= CHUNK {
        return Err(EngineError::Internal(
            "音声のダイジェストの木が 1 塊に収まります（小さい音声の道を通るはず）".into(),
        ));
    }
    let left = left_subtree_len(total);
    let l = subtree(tree, 0, left)?;
    let r = subtree(tree, left, total - left)?;
    Ok(Digest(*merge_subtrees_root(&l, &r, Mode::Hash).as_bytes()))
}

/// 標準の木の節 [pos, pos + len) の連鎖値（表になければ二つに割って下る。明示の積み上げで）。
fn subtree(tree: &Tree, pos: u64, len: u64) -> Result<ChainingValue, EngineError> {
    enum Task {
        Visit(u64, u64),
        Merge,
    }
    let mut tasks = vec![Task::Visit(pos, len)];
    let mut vals: Vec<ChainingValue> = Vec::new();
    while let Some(t) = tasks.pop() {
        match t {
            Task::Visit(p, l) => {
                if let Some(&(nl, cv)) = tree.nodes.get(&p)
                    && nl == l
                {
                    vals.push(cv);
                } else if l <= CHUNK {
                    return Err(EngineError::Internal(format!(
                        "音声のダイジェストの塊がありません（{p} から {l} バイト）"
                    )));
                } else {
                    let left = left_subtree_len(l);
                    tasks.push(Task::Merge);
                    tasks.push(Task::Visit(p + left, l - left));
                    tasks.push(Task::Visit(p, left));
                }
            }
            Task::Merge => {
                let (Some(r), Some(l)) = (vals.pop(), vals.pop()) else {
                    return Err(EngineError::Internal(
                        "音声のダイジェストの木の組み方を誤りました".into(),
                    ));
                };
                vals.push(merge_subtrees_non_root(&l, &r, Mode::Hash));
            }
        }
    }
    match (vals.pop(), vals.is_empty()) {
        (Some(cv), true) => Ok(cv),
        _ => Err(EngineError::Internal(
            "音声のダイジェストの木の組み方を誤りました".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::rng;
    use crate::value::{AUDIO_CHUNK, Marker};
    use rand_chacha::rand_core::Rng;

    fn want(rate: u32, planes: &[Vec<f32>], meta: &AudioMeta) -> Digest {
        let frames = planes.first().map_or(0, Vec::len) as u64;
        let mut d = PcmDigest::new(rate, planes.len(), frames);
        for (ch, p) in planes.iter().enumerate() {
            d.push(ch, 0, p).unwrap();
        }
        d.finish(meta).unwrap()
    }

    fn planes(r: &mut impl Rng, channels: usize, frames: usize) -> Vec<Vec<f32>> {
        (0..channels)
            .map(|_| {
                (0..frames)
                    .map(|_| match r.next_u32() % 50 {
                        0 => f32::NAN,
                        1 => f32::from_bits(0xffc0_0001),
                        2 => -0.0,
                        _ => (r.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0,
                    })
                    .collect()
            })
            .collect()
    }

    /// 区間の切り方（重ならず全体を覆う）。
    fn cuts(r: &mut impl Rng, frames: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < frames {
            let len = match r.next_u32() % 4 {
                0 => 1 + (r.next_u32() as usize % 7),
                1 => AUDIO_CHUNK,
                2 => 1 + (r.next_u32() as usize % (3 * AUDIO_CHUNK)),
                _ => 1 + (r.next_u32() as usize % 4_096),
            };
            let end = (at + len).min(frames);
            out.push((at, end));
            at = end;
        }
        out
    }

    /// 性質テスト（E42）：ランダムな長さ（0〜3C、1 MiB の境の前後）・チャンネル（1〜8）・区間の順（前から・後ろから・
    /// ランダム・チャンネルを混ぜる）・NaN で、`PcmDigest` と同じ。
    #[test]
    fn tree_digest_equals_channel_order_digest() {
        let mut r = rng(4201);
        let meta = AudioMeta {
            markers: vec![Marker {
                pos: 3,
                name: "m".into(),
            }],
            ..AudioMeta::default()
        };
        let mut lengths: Vec<(usize, usize)> = vec![
            (1, 0),
            (2, 0),
            (1, 1),
            (1, 262_144),
            (1, 262_143),
            (1, 262_145),
            (2, 131_072),
            (2, 131_073),
            (4, 65_536 + 1),
            (8, 33_000),
            (3, 2 * AUDIO_CHUNK + 37),
            (1, 3 * AUDIO_CHUNK),
        ];
        for _ in 0..10 {
            let ch = 1 + (r.next_u32() as usize % 8);
            let frames = r.next_u32() as usize % (3 * AUDIO_CHUNK + 1);
            lengths.push((ch, frames));
        }
        for (case, (channels, frames)) in lengths.into_iter().enumerate() {
            let p = planes(&mut r, channels, frames);
            let expect = want(44_100, &p, &meta);
            for order in 0..4 {
                let mut d = PcmTreeDigest::new(44_100, channels, frames as u64).unwrap();
                let c = cuts(&mut r, frames);
                match order {
                    // 窓ごとに全チャンネル（前から）
                    0 => {
                        for &(s, e) in &c {
                            let views: Vec<&[f32]> = p.iter().map(|x| &x[s..e]).collect();
                            d.push_window(s as u64, &views, &NoMonitor).unwrap();
                        }
                    }
                    // 後ろから
                    1 => {
                        for &(s, e) in c.iter().rev() {
                            for ch in (0..channels).rev() {
                                d.push(ch, s as u64, &p[ch][s..e]).unwrap();
                            }
                        }
                    }
                    // ランダム（チャンネルを混ぜる）
                    _ => {
                        let mut all: Vec<(usize, usize, usize)> = c
                            .iter()
                            .flat_map(|&(s, e)| (0..channels).map(move |ch| (ch, s, e)))
                            .collect();
                        for i in (1..all.len()).rev() {
                            let j = r.next_u32() as usize % (i + 1);
                            all.swap(i, j);
                        }
                        for (ch, s, e) in all {
                            d.push(ch, s as u64, &p[ch][s..e]).unwrap();
                        }
                    }
                }
                assert!(d.is_complete());
                assert_eq!(
                    d.finish(&meta).unwrap(),
                    expect,
                    "{case}: {channels} × {frames}・順 {order}"
                );
            }
        }
    }

    /// 重なり・範囲の外は `Internal`、足りなければ `finish` が `Internal`。
    #[test]
    fn overlaps_and_gaps_are_internal() {
        for frames in [100u64, 300_000] {
            let x = vec![0.5f32; 10];
            let mut d = PcmTreeDigest::new(8_000, 2, frames).unwrap();
            d.push(0, 5, &x).unwrap();
            assert!(matches!(d.push(0, 10, &x), Err(EngineError::Internal(_))));
            assert!(matches!(d.push(0, 0, &x), Err(EngineError::Internal(_))));
            assert!(matches!(d.push(2, 0, &x), Err(EngineError::Internal(_))));
            assert!(matches!(
                d.push(1, frames - 5, &x),
                Err(EngineError::Internal(_))
            ));
            d.push(0, 15, &x).unwrap();
            assert!(!d.is_complete());
            assert!(matches!(
                d.finish(&AudioMeta::default()),
                Err(EngineError::Internal(_))
            ));
        }
    }

    /// 大きなメタデータ（後置きが何 KiB にもまたがる）でも同じ。
    #[test]
    fn large_meta_suffix() {
        let mut r = rng(4202);
        let p = planes(&mut r, 2, 140_000);
        let meta = AudioMeta {
            markers: (0..400)
                .map(|i| Marker {
                    pos: i,
                    name: format!("marker-{i}-{}", "x".repeat(i as usize % 50)),
                })
                .collect(),
            ..AudioMeta::default()
        };
        let mut d = PcmTreeDigest::new(48_000, 2, 140_000).unwrap();
        let views: Vec<&[f32]> = p.iter().map(Vec::as_slice).collect();
        d.push_window(0, &views, &NoMonitor).unwrap();
        assert_eq!(d.finish(&meta).unwrap(), want(48_000, &p, &meta));
    }

    #[test]
    fn covered_merges_and_rejects_overlaps() {
        let mut c = Covered::default();
        assert!(c.insert(5, 10));
        assert!(c.insert(0, 5));
        assert!(c.insert(10, 12));
        assert!(!c.insert(11, 13));
        assert!(!c.insert(3, 4));
        assert!(c.covers(12));
        assert!(!c.covers(13));
        assert!(c.insert(20, 30));
        assert!(!c.insert(19, 21));
        assert!(!c.covers(30));
    }
}
