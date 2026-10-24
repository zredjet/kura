//! ffmpeg が標準出力に書く WAV（`pcm_f32le`）の読み取り。ヘッダから実際のレート・チャンネルを読み、インターリーブの f32 を
//! チャンネルごとに分ける。区切りがどこで切れても同じ結果になる（純粋な状態機械。プロセスを知らない）。
//!
//! - ffmpeg はパイプに書くので、RIFF と data の長さは 0xFFFFFFFF（長さを書き戻せない）。data の長さは読まず、終わりまで
//!   読む。fmt は IEEE float の 32 ビット（タグ 3）か、EXTENSIBLE（0xFFFE）で中身が IEEE float のものだけ。
//! - チャンネルの並びは WAV の並び（kura の planes の並びと同じ）のまま。EXTENSIBLE のマスクは記録だけして並べ替えない
//!   （固定の段の WAV のデコーダと同じ）。
//! - 上限：チャンネルの数・レートはヘッダを読んだときに、長さは `min(max_audio_frames, max_value_bytes / (4 · ch))`
//!   フレームを超えた時点で止める（`Limit`）。確保は上限の中で `try_reserve_exact`。

/// data より前のヘッダの長さの上限（fmt・LIST など。ffmpeg の出力は 100 バイトに満たない）。
pub const MAX_HEADER: usize = 64 * 1024;

/// EXTENSIBLE の中身の GUID のうち、先頭の 2 バイト（形式のタグ）より後ろ（KSDATAFORMAT_SUBTYPE_* の共通の部分）。
const GUID_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

/// ヘッダから読んだ形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavFormat {
    pub rate: u32,
    pub channels: u16,
    /// EXTENSIBLE のチャンネルのマスク（記録だけ）。
    pub mask: Option<u32>,
}

/// 読み取りの失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WavError {
    /// 上限を超える。
    Limit(String),
    /// 出力が壊れている・想定と違う。
    Bad(String),
}

/// 上限（kura-engine の `Limits` のうち、ここで使うもの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavLimits {
    pub max_audio_frames: u64,
    pub max_value_bytes: u64,
    pub max_channels: u64,
    pub max_sample_rate: u32,
}

impl From<&kura_engine::limits::Limits> for WavLimits {
    fn from(l: &kura_engine::limits::Limits) -> Self {
        Self {
            max_audio_frames: l.max_audio_frames,
            max_value_bytes: l.max_value_bytes,
            max_channels: l.max_channels,
            max_sample_rate: l.max_sample_rate,
        }
    }
}

/// 読み終えた結果。
#[derive(Debug, Clone, PartialEq)]
pub struct WavDone {
    pub format: WavFormat,
    /// 1 チャンネルあたりのフレームの数。
    pub frames: u64,
    /// チャンネルごとの標本（数えるだけのときは `None`）。
    pub planes: Option<Vec<Vec<f32>>>,
}

/// data の中を読んでいる状態。
struct Data {
    format: WavFormat,
    /// 1 フレームのバイト数（4 · ch）。
    frame_bytes: usize,
    max_frames: u64,
    /// 前の区切りから持ち越したフレームの端数（`frame_bytes` 未満）。
    carry: Vec<u8>,
}

/// ffmpeg の標準出力の WAV を区切りごとに受け取る。
pub struct WavStream {
    limits: WavLimits,
    /// 標本を積むか（偽なら数えるだけ）。
    store: bool,
    /// 最初に確保するフレームの数の目安（ffprobe の長さ。上限で抑える）。
    hint: u64,
    header: Vec<u8>,
    data: Option<Data>,
    frames: u64,
    planes: Vec<Vec<f32>>,
}

impl WavStream {
    /// 数えるだけ（形を決める回）。
    pub fn count(limits: WavLimits) -> Self {
        Self::new(limits, false, 0)
    }

    /// 標本を積む（値を読む回）。`hint` は最初に確保するフレームの数の目安。
    pub fn store(limits: WavLimits, hint: u64) -> Self {
        Self::new(limits, true, hint)
    }

    fn new(limits: WavLimits, store: bool, hint: u64) -> Self {
        Self {
            limits,
            store,
            hint,
            header: Vec::new(),
            data: None,
            frames: 0,
            planes: Vec::new(),
        }
    }

    /// これまでに読んだフレームの数。
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// ヘッダを読み終えていれば、その形。
    pub fn format(&self) -> Option<WavFormat> {
        self.data.as_ref().map(|d| d.format)
    }

    /// 標準出力の続きを受け取る。
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), WavError> {
        if self.data.is_some() {
            return self.push_data(bytes);
        }
        self.header.extend_from_slice(bytes);
        match parse_header(&self.header)? {
            None => {
                if self.header.len() > MAX_HEADER + 8 {
                    return Err(WavError::Bad("WAV のヘッダが長すぎます".into()));
                }
                Ok(())
            }
            Some((format, start)) => {
                self.begin(format)?;
                let rest = std::mem::take(&mut self.header);
                self.push_data(rest.get(start..).unwrap_or_default())
            }
        }
    }

    /// ヘッダを読んだ：上限を確かめ、積むなら確保する。
    fn begin(&mut self, format: WavFormat) -> Result<(), WavError> {
        let l = self.limits;
        if u64::from(format.channels) > l.max_channels {
            return Err(WavError::Limit(format!(
                "チャンネルの数が {} を超えます（{}）",
                l.max_channels, format.channels
            )));
        }
        if format.rate > l.max_sample_rate {
            return Err(WavError::Limit(format!(
                "サンプルレートが {} を超えます（{}）",
                l.max_sample_rate, format.rate
            )));
        }
        let frame_bytes = 4 * usize::from(format.channels);
        let max_frames = l
            .max_audio_frames
            .min(l.max_value_bytes / (4 * u64::from(format.channels)));
        if self.store {
            let mut planes: Vec<Vec<f32>> = Vec::new();
            planes
                .try_reserve_exact(usize::from(format.channels))
                .map_err(|_| WavError::Limit("チャンネルの表を確保できません".into()))?;
            let first = usize::try_from(self.hint.min(max_frames))
                .map_err(|_| WavError::Limit("音声が長すぎます".into()))?;
            for _ in 0..format.channels {
                let mut p = Vec::new();
                p.try_reserve_exact(first)
                    .map_err(|_| WavError::Limit(format!("{first} フレームを確保できません")))?;
                planes.push(p);
            }
            self.planes = planes;
        }
        self.data = Some(Data {
            format,
            frame_bytes,
            max_frames,
            carry: Vec::new(),
        });
        Ok(())
    }

    fn push_data(&mut self, mut bytes: &[u8]) -> Result<(), WavError> {
        let Some(data) = self.data.as_mut() else {
            return Err(WavError::Bad("WAV のヘッダを読む前です".into()));
        };
        let fb = data.frame_bytes;
        // 持ち越した端数を 1 フレームにする
        if !data.carry.is_empty() {
            let need = fb - data.carry.len();
            let take = need.min(bytes.len());
            data.carry.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if data.carry.len() < fb {
                return Ok(());
            }
            let frame = std::mem::take(&mut data.carry);
            let (max_frames, store) = (data.max_frames, self.store);
            accept(
                &mut self.frames,
                &mut self.planes,
                store,
                max_frames,
                fb,
                &frame,
            )?;
        }
        let Some(data) = self.data.as_mut() else {
            return Err(WavError::Bad("WAV のヘッダを読む前です".into()));
        };
        let whole = bytes.len() / fb * fb;
        let (head, tail) = bytes.split_at(whole);
        data.carry.extend_from_slice(tail);
        let (max_frames, store) = (data.max_frames, self.store);
        accept(
            &mut self.frames,
            &mut self.planes,
            store,
            max_frames,
            fb,
            head,
        )
    }

    /// 読み終える。ヘッダがない・フレームの端数が残る出力は壊れたものとして扱う。
    pub fn finish(self) -> Result<WavDone, WavError> {
        let Some(data) = self.data else {
            return Err(WavError::Bad("出力に WAV のヘッダがありません".into()));
        };
        if !data.carry.is_empty() {
            return Err(WavError::Bad(
                "出力の長さがフレームの大きさで割り切れません".into(),
            ));
        }
        Ok(WavDone {
            format: data.format,
            frames: self.frames,
            planes: self.store.then_some(self.planes),
        })
    }
}

/// 揃ったフレームの列（`bytes.len()` は `fb` の倍数）を数え、積むならチャンネルに分ける。
fn accept(
    frames: &mut u64,
    planes: &mut [Vec<f32>],
    store: bool,
    max_frames: u64,
    fb: usize,
    bytes: &[u8],
) -> Result<(), WavError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let n = (bytes.len() / fb) as u64;
    let total = frames.saturating_add(n);
    if total > max_frames {
        return Err(WavError::Limit(format!(
            "音声が上限（{max_frames} フレーム）を超えます"
        )));
    }
    if store {
        let need =
            usize::try_from(total).map_err(|_| WavError::Limit("音声が長すぎます".into()))?;
        let cap_max = usize::try_from(max_frames).unwrap_or(usize::MAX);
        for p in planes.iter_mut() {
            if p.capacity() < need {
                let grow = p
                    .capacity()
                    .saturating_mul(2)
                    .max(need)
                    .max(4_096)
                    .min(cap_max);
                p.try_reserve_exact(grow - p.len())
                    .map_err(|_| WavError::Limit("音声の領域を確保できません".into()))?;
            }
        }
        for frame in bytes.chunks_exact(fb) {
            for (p, s) in planes.iter_mut().zip(frame.as_chunks::<4>().0.iter()) {
                p.push(f32::from_le_bytes([s[0], s[1], s[2], s[3]]));
            }
        }
    }
    *frames = total;
    Ok(())
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// ヘッダを読む。足りなければ `None`（続きを待つ）、揃えば形と data の中身の始まりの位置。
fn parse_header(b: &[u8]) -> Result<Option<(WavFormat, usize)>, WavError> {
    let bad = |m: &str| WavError::Bad(m.to_string());
    if b.len() < 12 {
        return Ok(None);
    }
    if &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad("出力が WAV ではありません"));
    }
    let mut pos = 12usize;
    let mut fmt = None;
    loop {
        if b.len() < pos + 8 {
            return Ok(None);
        }
        let id = &b[pos..pos + 4];
        let size = u32_at(b, pos + 4).ok_or_else(|| bad("WAV のチャンクが壊れています"))?;
        let body = pos + 8;
        if id == b"data" {
            let f = fmt.ok_or_else(|| bad("WAV の fmt より前に data があります"))?;
            return Ok(Some((f, body)));
        }
        let padded = u64::from(size) + u64::from(size & 1);
        let end = (body as u64).saturating_add(padded);
        if end > MAX_HEADER as u64 {
            return Err(bad("WAV のヘッダが長すぎます"));
        }
        // MAX_HEADER 以下なので usize に収まる
        let end = end as usize;
        if b.len() < end {
            return Ok(None);
        }
        if id == b"fmt " {
            if fmt.is_some() {
                return Err(bad("WAV の fmt が 2 つあります"));
            }
            fmt = Some(parse_fmt(&b[body..body + size as usize])?);
        }
        pos = end;
    }
}

/// fmt を読む：IEEE float の 32 ビット（タグ 3）か、中身が IEEE float の EXTENSIBLE だけ。
fn parse_fmt(f: &[u8]) -> Result<WavFormat, WavError> {
    let bad = |m: String| WavError::Bad(m);
    let short = || WavError::Bad("WAV の fmt が短すぎます".into());
    let tag = u16_at(f, 0).ok_or_else(short)?;
    let channels = u16_at(f, 2).ok_or_else(short)?;
    let rate = u32_at(f, 4).ok_or_else(short)?;
    let block_align = u16_at(f, 12).ok_or_else(short)?;
    let bits = u16_at(f, 14).ok_or_else(short)?;
    let mask = match tag {
        3 => None,
        0xFFFE => {
            let cb = u16_at(f, 16).ok_or_else(short)?;
            if cb < 22 || f.len() < 40 {
                return Err(short());
            }
            let sub = u16_at(f, 24).ok_or_else(short)?;
            if sub != 3 || f[26..40] != GUID_TAIL {
                return Err(bad("WAV の中身が浮動小数ではありません".into()));
            }
            Some(u32_at(f, 20).ok_or_else(short)?)
        }
        other => {
            return Err(bad(format!(
                "WAV の形式が浮動小数ではありません（タグ {other:#x}）"
            )));
        }
    };
    if bits != 32 {
        return Err(bad(format!(
            "WAV の標本が 32 ビットではありません（{bits}）"
        )));
    }
    if channels == 0 || rate == 0 {
        return Err(bad("WAV のチャンネルの数かレートが 0 です".into()));
    }
    if u32::from(block_align) != 4 * u32::from(channels) {
        return Err(bad("WAV のブロックの大きさが合いません".into()));
    }
    Ok(WavFormat {
        rate,
        channels,
        mask,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::{Rng, SeedableRng};

    pub(crate) const LIMITS: WavLimits = WavLimits {
        max_audio_frames: 1 << 30,
        max_value_bytes: 1 << 34,
        max_channels: 32,
        max_sample_rate: 768_000,
    };

    /// ffmpeg と同じ形のヘッダ（長さは 0xFFFFFFFF。3 チャンネル以上は EXTENSIBLE）と、インターリーブした標本。
    pub(crate) fn wav_bytes(rate: u32, planes: &[Vec<f32>], extra_chunk: Option<&[u8]>) -> Vec<u8> {
        let ch = planes.len() as u16;
        let ext = ch > 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&(if ext { 40u32 } else { 18 }).to_le_bytes());
        b.extend_from_slice(&(if ext { 0xFFFEu16 } else { 3 }).to_le_bytes());
        b.extend_from_slice(&ch.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 4 * u32::from(ch)).to_le_bytes());
        b.extend_from_slice(&(4 * ch).to_le_bytes());
        b.extend_from_slice(&32u16.to_le_bytes());
        if ext {
            b.extend_from_slice(&22u16.to_le_bytes());
            b.extend_from_slice(&32u16.to_le_bytes());
            b.extend_from_slice(&0x3Fu32.to_le_bytes());
            b.extend_from_slice(&3u16.to_le_bytes());
            b.extend_from_slice(&GUID_TAIL);
        } else {
            b.extend_from_slice(&0u16.to_le_bytes());
        }
        if let Some(extra) = extra_chunk {
            b.extend_from_slice(b"LIST");
            b.extend_from_slice(&(extra.len() as u32).to_le_bytes());
            b.extend_from_slice(extra);
            if extra.len() % 2 == 1 {
                b.push(0);
            }
        }
        b.extend_from_slice(b"data");
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        let frames = planes.first().map_or(0, Vec::len);
        for i in 0..frames {
            for p in planes {
                b.extend_from_slice(&p[i].to_le_bytes());
            }
        }
        b
    }

    fn planes(ch: usize, frames: usize) -> Vec<Vec<f32>> {
        (0..ch)
            .map(|c| {
                (0..frames)
                    .map(|i| (i * 31 + c * 7) as f32 / 1000.0 - 0.5)
                    .collect()
            })
            .collect()
    }

    fn read_in(bytes: &[u8], cuts: &[usize], store: bool) -> Result<WavDone, WavError> {
        let mut s = if store {
            WavStream::store(LIMITS, 3)
        } else {
            WavStream::count(LIMITS)
        };
        let mut at = 0;
        for &c in cuts {
            let c = c.min(bytes.len());
            if c >= at {
                s.push(&bytes[at..c])?;
                at = c;
            }
        }
        s.push(&bytes[at..])?;
        s.finish()
    }

    /// どこで区切っても同じ値（ヘッダの途中・fmt の途中・フレームの途中）。数えるだけでも長さは同じ。
    #[test]
    fn any_split_gives_the_same_value() {
        for (rate, ch, frames, extra) in [
            (48_000, 2usize, 37usize, None),
            (44_100, 1, 5, Some(&b"INFOISFTx"[..])),
            (48_000, 6, 11, None),
            (8_000, 3, 0, None),
        ] {
            let p = planes(ch, frames);
            let bytes = wav_bytes(rate, &p, extra);
            let whole = read_in(&bytes, &[], true).unwrap();
            assert_eq!(whole.format.rate, rate);
            assert_eq!(usize::from(whole.format.channels), ch);
            assert_eq!(whole.frames, frames as u64);
            assert_eq!(whole.planes.as_deref(), Some(p.as_slice()));
            assert_eq!(whole.format.mask, (ch > 2).then_some(0x3F));
            for cut in 0..bytes.len() {
                assert_eq!(read_in(&bytes, &[cut], true).unwrap(), whole, "{cut}");
                assert_eq!(
                    read_in(&bytes, &[cut, cut + 3], false).unwrap().frames,
                    frames as u64
                );
            }
            // 1 バイトずつ
            let cuts: Vec<usize> = (0..bytes.len()).collect();
            assert_eq!(read_in(&bytes, &cuts, true).unwrap(), whole);
        }
    }

    #[test]
    fn broken_or_unexpected_headers_are_errors() {
        let good = wav_bytes(48_000, &planes(2, 4), None);
        let bad = |b: &[u8]| matches!(read_in(b, &[], true), Err(WavError::Bad(_)));
        // 空・ヘッダの途中で終わる
        assert!(bad(b""));
        assert!(bad(&good[..30]));
        // RIFF でない
        let mut b = good.clone();
        b[0] = b'X';
        assert!(bad(&b));
        // 整数の PCM（タグ 1）
        let mut b = good.clone();
        b[20] = 1;
        assert!(bad(&b));
        // 16 ビット
        let mut b = good.clone();
        b[34] = 16;
        assert!(bad(&b));
        // ブロックの大きさが合わない
        let mut b = good.clone();
        b[32] = 4;
        assert!(bad(&b));
        // フレームの端数
        assert!(bad(&good[..good.len() - 3]));
        // data が fmt より前
        let mut b = Vec::new();
        b.extend_from_slice(&good[..12]);
        b.extend_from_slice(b"data\xff\xff\xff\xff");
        assert!(bad(&b));
        // 長すぎるヘッダ（大きな LIST）
        let big = vec![b'x'; MAX_HEADER];
        assert!(bad(&wav_bytes(48_000, &planes(1, 1), Some(&big))));
        // EXTENSIBLE で中身が整数
        let mut b = wav_bytes(48_000, &planes(3, 2), None);
        b[44] = 1;
        assert!(bad(&b));
    }

    #[test]
    fn limits_stop_reading() {
        let bytes = wav_bytes(48_000, &planes(2, 100), None);
        let tight = |f: fn(&mut WavLimits)| {
            let mut l = LIMITS;
            f(&mut l);
            let mut s = WavStream::store(l, 1 << 40);
            s.push(&bytes).and_then(|_| s.finish().map(|_| ()))
        };
        assert!(matches!(
            tight(|l| l.max_audio_frames = 99),
            Err(WavError::Limit(_))
        ));
        assert!(tight(|l| l.max_audio_frames = 100).is_ok());
        // 値の大きさ：2 チャンネル × 4 バイト × 100 = 800
        assert!(matches!(
            tight(|l| l.max_value_bytes = 799),
            Err(WavError::Limit(_))
        ));
        assert!(tight(|l| l.max_value_bytes = 800).is_ok());
        assert!(matches!(
            tight(|l| l.max_channels = 1),
            Err(WavError::Limit(_))
        ));
        assert!(matches!(
            tight(|l| l.max_sample_rate = 44_100),
            Err(WavError::Limit(_))
        ));
    }

    /// 壊した出力（ChaCha8）でも panic せず、成功するか、エラーになる。
    #[test]
    fn random_damage_never_panics() {
        let good = wav_bytes(48_000, &planes(3, 64), Some(b"INFO"));
        let mut rng = ChaCha8Rng::seed_from_u64(9);
        for _ in 0..2_000 {
            let mut b = good.clone();
            for _ in 0..(rng.next_u32() % 8 + 1) {
                let i = rng.next_u32() as usize % b.len();
                b[i] = rng.next_u32() as u8;
            }
            let cut = rng.next_u32() as usize % (b.len() + 1);
            b.truncate(cut);
            let split = rng.next_u32() as usize % (b.len() + 1);
            let _ = read_in(&b, &[split], true);
        }
    }
}
