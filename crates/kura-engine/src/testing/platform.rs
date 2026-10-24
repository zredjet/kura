//! Platform・External の偽物（Linux でも回るテスト用。kura-core・kura-mac のテストも使ってよい）。
//!
//! - [`FakePlatformDecoder`]：段（既定 Platform）・識別子・形式・値・読み方（安定・読むたびに変わる・失敗）・環境を選べる
//!   デコーダ。E29（記録との食い違いの検出・アンカーからの作り直し）と、段の順のテストに使う。
//! - [`FakePlatformEncoder`]：能力の表・段の順のテスト用のエンコーダ。
//! - [`FakeImageBackend`]・[`FakeAudioBackend`]・[`FakeAacBackend`]：`codec::platform` の backend の偽物。adapter の
//!   画素・PCM・中止・上限・壊れた形を、OS なしで確かめる。開いた数・閉じた数・帯・読み書きの回数を数える。
//!
//! テスト用の補助だが本番のビルドにも入る（`pub mod testing`）ので、ここでも panic しない。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::codec::formats::{NONE, format_by_id};
use crate::codec::platform::aac::boxes;
use crate::codec::platform::{
    AacBackend, AacHandle, AacSettings, AudioBackend, AudioHandle, AudioInfo, BackendError,
    DecodedFrame, ImageBackend, ImageHandle, ImageInfo, PlatformAlpha, Priming,
};
use crate::codec::symph::read_all;
use crate::codec::{CodecEnv, Decoder, Encoder, MetaCaps, ReadSeek, Tier};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::ParamSchema;
use crate::shape::Shape;
use crate::testgen::InputGen;
use crate::value::{Audio, AudioSource, Frame, Frames, PixelFormat, Raster, Value};

// ---- 偽の Platform のデコーダ・エンコーダ ----

/// 偽のデコーダの読み方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeRead {
    /// いつも同じ値（同じ OS で読み直したとき）。
    Stable,
    /// 読むたびに値が変わる（OS の更新・HE-AAC の揺れを写す）：n 回目（0 から）の読み取りは、最初の標本のビット列に n を
    /// 足す（画像は最初の画素の最初のバイトに n を足す。256 回ごとに同じ値に戻る。パレットの画像は色の数で割った余り）。
    DriftPerRead,
    /// 失敗する。
    Fail(FakeFailure),
}

/// 偽のデコーダの失敗のしかた。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeFailure {
    /// 形の段階で `Unsupported`（次の段へ回る）。
    Unsupported,
    /// 形は読めるが、中身で `Decode`（壊れたファイル）。
    Decode,
}

/// 偽の Platform（か External）のデコーダ。値は `input` の作り方で決まり、入力のバイト列は（`max_value_bytes` まで）読むが
/// 値には使わない。`decode` を呼んだ回数を数える（`shape` は数えない）。
pub struct FakePlatformDecoder {
    id: &'static str,
    tier: Tier,
    formats: &'static [&'static str],
    input: InputGen,
    mode: FakeRead,
    env: Option<CodecEnv>,
    reads: AtomicU64,
}

impl FakePlatformDecoder {
    /// 段は Platform、読み方は安定、環境は [`FakePlatformDecoder::default_env`]。
    pub fn new(id: &'static str, formats: &'static [&'static str], input: InputGen) -> Self {
        Self {
            id,
            tier: Tier::Platform,
            formats,
            input,
            mode: FakeRead::Stable,
            env: Some(Self::default_env()),
            reads: AtomicU64::new(0),
        }
    }

    /// 既定の環境の記録。
    pub fn default_env() -> CodecEnv {
        CodecEnv::new("fake", "1.0").with("build", "F1")
    }

    /// 読むたびに値が変わるようにする。
    pub fn drifting(mut self) -> Self {
        self.mode = FakeRead::DriftPerRead;
        self
    }

    /// 失敗するようにする。
    pub fn failing(mut self, f: FakeFailure) -> Self {
        self.mode = FakeRead::Fail(f);
        self
    }

    /// 読み方を選ぶ。
    pub fn with_mode(mut self, mode: FakeRead) -> Self {
        self.mode = mode;
        self
    }

    /// 環境の記録を変える（`None` なら記録しない）。
    pub fn with_env(mut self, env: impl Into<Option<CodecEnv>>) -> Self {
        self.env = env.into();
        self
    }

    /// 段を変える（External の偽物にするとき）。どの段でも `reproducible()` は偽。
    pub fn with_tier(mut self, tier: Tier) -> Self {
        self.tier = tier;
        self
    }

    /// これまでに `decode` を呼んだ回数。
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }

    /// `n` 回目（0 から）の読み取りの値（安定なら n によらず同じ）。
    pub fn value_for(&self, n: u64) -> Result<Value, EngineError> {
        let v = self.input.generate()?;
        match self.mode {
            FakeRead::DriftPerRead => drift(v, n),
            _ => Ok(v),
        }
    }
}

/// 値を n だけずらす（[`FakeRead::DriftPerRead`]）。
fn drift(v: Value, n: u64) -> Result<Value, EngineError> {
    if n == 0 {
        return Ok(v);
    }
    let bump_px = |px: &mut Raster| {
        let colors = px.palette().map(|p| p.colors.len() as u64);
        let b = &mut px.tile_mut(0, 0)[0];
        *b = match colors {
            Some(c) if c > 0 => ((u64::from(*b) + n) % c) as u8,
            _ => b.wrapping_add(n as u8),
        };
    };
    Ok(match v {
        Value::Image(img) => {
            let mut img = Arc::unwrap_or_clone(img);
            bump_px(&mut img.px);
            Value::Image(Arc::new(img))
        }
        Value::Frames(fr) => {
            let mut frames: Vec<Frame> = fr.frames().to_vec();
            if let Some(f) = frames.first_mut() {
                bump_px(&mut f.px);
            }
            Value::Frames(Arc::new(Frames::new(
                frames,
                fr.canvas(),
                fr.meta.clone(),
                fr.loop_count,
            )?))
        }
        Value::Audio(a) => {
            let mut planes: Vec<Arc<Vec<f32>>> = a.planes().to_vec();
            if let Some(s) = planes
                .first_mut()
                .and_then(|p| Arc::make_mut(p).first_mut())
            {
                *s = f32::from_bits(s.to_bits().wrapping_add(n as u32));
            }
            Value::Audio(Arc::new(Audio::new(a.rate(), planes, a.meta.clone())?))
        }
    })
}

impl Decoder for FakePlatformDecoder {
    fn id(&self) -> &'static str {
        self.id
    }

    fn tier(&self) -> Tier {
        self.tier
    }

    fn reproducible(&self) -> bool {
        false
    }

    fn env(&self) -> Option<CodecEnv> {
        self.env.clone()
    }

    fn can_decode(&self, format: &str) -> bool {
        self.formats.contains(&format)
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        if self.mode == FakeRead::Fail(FakeFailure::Unsupported) {
            return Err(EngineError::Unsupported(format!(
                "{}: {format} のこの変種は読めません",
                self.id
            )));
        }
        read_all(src, limits)?;
        Ok(self.input.generate()?.shape())
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        _mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        read_all(src, limits)?;
        let n = self.reads.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            FakeRead::Fail(FakeFailure::Unsupported) => Err(EngineError::Unsupported(format!(
                "{}: {format} のこの変種は読めません",
                self.id
            ))),
            FakeRead::Fail(FakeFailure::Decode) => Err(EngineError::Decode {
                codec: self.id.to_string(),
                message: "偽の読み取りの失敗".into(),
            }),
            _ => self.value_for(n),
        }
    }
}

/// 偽の Platform（か External）のエンコーダ。形式の表の値の種類を受け付け、値のダイジェスト（32 バイト）を書く。
pub struct FakePlatformEncoder {
    id: &'static str,
    tier: Tier,
    formats: &'static [&'static str],
    env: Option<CodecEnv>,
}

impl FakePlatformEncoder {
    /// 段は Platform、環境は [`FakePlatformDecoder::default_env`]。
    pub fn new(id: &'static str, formats: &'static [&'static str]) -> Self {
        Self {
            id,
            tier: Tier::Platform,
            formats,
            env: Some(FakePlatformDecoder::default_env()),
        }
    }

    /// 環境の記録を変える。
    pub fn with_env(mut self, env: impl Into<Option<CodecEnv>>) -> Self {
        self.env = env.into();
        self
    }

    /// 段を変える。
    pub fn with_tier(mut self, tier: Tier) -> Self {
        self.tier = tier;
        self
    }
}

impl Encoder for FakePlatformEncoder {
    fn id(&self) -> &'static str {
        self.id
    }

    fn tier(&self) -> Tier {
        self.tier
    }

    fn reproducible(&self) -> bool {
        false
    }

    fn env(&self) -> Option<CodecEnv> {
        self.env.clone()
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        self.formats.contains(&format)
            && format_by_id(format).is_some_and(|f| f.decodes_to.contains(&shape.kind()))
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema::default()
    }

    fn caps(&self, _format: &str, _opts: &serde_json::Value) -> MetaCaps {
        NONE
    }

    fn encode(
        &self,
        value: &Value,
        _format: &str,
        _opts: &serde_json::Value,
        out: &mut dyn std::io::Write,
        _mon: &dyn Monitor,
    ) -> Result<u64, EngineError> {
        let d = value.digest()?;
        out.write_all(&d.0)?;
        Ok(d.0.len() as u64)
    }
}

// ---- backend の偽物 ----

/// 偽の backend が数えるもの。
#[derive(Debug, Default)]
pub struct FakeCounters {
    /// 開いた数。
    pub opens: AtomicU64,
    /// 閉じた（ハンドルを落とした）数。
    pub closes: AtomicU64,
    /// 画像の展開（`begin`）の数。
    pub begins: AtomicU64,
    /// 画像の帯（`read_rows`）の数。
    pub bands: AtomicU64,
    /// 音声の読み取り（`read`）の数。
    pub reads: AtomicU64,
    /// AAC の書き込み（`write`）の数。
    pub writes: AtomicU64,
}

fn add(c: &AtomicU64) -> u64 {
    c.fetch_add(1, Ordering::SeqCst)
}

/// 偽の画像（backend が「展開した」画素。アルファの扱いのまま）。
#[derive(Debug, Clone, PartialEq)]
pub struct FakeImage {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub alpha: PlatformAlpha,
    pub orientation: u8,
    /// フレームごとの画素（行を詰めたもの）と表示時間（ミリ秒）。
    pub frames: Vec<(Vec<u8>, u32)>,
    pub loop_count: u32,
    pub icc: Option<Vec<u8>>,
    pub exif: Option<Vec<u8>>,
    pub hdr: bool,
}

impl FakeImage {
    /// 1 枚の画像（向き 1・ICC と EXIF なし）。
    pub fn still(px: &Raster, alpha: PlatformAlpha) -> Self {
        Self {
            width: px.width(),
            height: px.height(),
            format: px.format(),
            alpha,
            orientation: 1,
            frames: vec![(px.to_rows().unwrap_or_default(), 0)],
            loop_count: 0,
            icc: None,
            exif: None,
            hdr: false,
        }
    }
}

/// 偽の画像の backend の失敗のしかた。
#[derive(Debug, Clone, PartialEq)]
pub enum ImageFault {
    /// 開くときに失敗する。
    Open(BackendError),
    /// 展開で失敗する。
    Begin(BackendError),
    /// 展開した大きさが開いたときと違う。
    DecodedSize(u32, u32),
    /// 展開した形式が開いたときと違う。
    DecodedFormat(PixelFormat),
    /// `n` 回目（0 から）の帯で失敗する。
    Band(u64, BackendError),
    /// ICC の長さを偽って申告する。
    IccLength(u64),
}

/// 偽の画像の backend（`codec::platform::ImageBackend`）。
pub struct FakeImageBackend {
    pub id: &'static str,
    pub formats: Vec<&'static str>,
    pub image: FakeImage,
    pub faults: Vec<ImageFault>,
    pub env: Option<CodecEnv>,
    counters: Arc<FakeCounters>,
}

impl FakeImageBackend {
    /// 識別子 `fake-imageio-dec/1`、形式 heic・heif・avif、環境は [`FakeImageBackend::default_env`]。
    pub fn new(image: FakeImage) -> Self {
        Self {
            id: "fake-imageio-dec/1",
            formats: vec!["heic", "heif", "avif"],
            image,
            faults: Vec::new(),
            env: Some(Self::default_env()),
            counters: Arc::default(),
        }
    }

    /// 既定の環境の記録。
    pub fn default_env() -> CodecEnv {
        CodecEnv::new("fake", "1.0").with("imageio", "0.1")
    }

    /// 数えたもの（ハンドルと共有する）。
    pub fn counters(&self) -> Arc<FakeCounters> {
        self.counters.clone()
    }
}

/// 偽の画像の backend のハンドル。
pub struct FakeImageHandle {
    image: FakeImage,
    faults: Vec<ImageFault>,
    counters: Arc<FakeCounters>,
    begun: Option<usize>,
    bands: u64,
    _bytes: Arc<Vec<u8>>,
}

impl Drop for FakeImageHandle {
    fn drop(&mut self) {
        add(&self.counters.closes);
    }
}

impl ImageBackend for FakeImageBackend {
    type Handle = FakeImageHandle;

    fn id(&self) -> &'static str {
        self.id
    }

    fn can_decode(&self, format: &str) -> bool {
        self.formats.contains(&format)
    }

    fn env(&self) -> Option<CodecEnv> {
        self.env.clone()
    }

    fn open(&self, bytes: Arc<Vec<u8>>, _format: &str) -> Result<FakeImageHandle, BackendError> {
        add(&self.counters.opens);
        for f in &self.faults {
            if let ImageFault::Open(e) = f {
                return Err(e.clone());
            }
        }
        Ok(FakeImageHandle {
            image: self.image.clone(),
            faults: self.faults.clone(),
            counters: self.counters.clone(),
            begun: None,
            bands: 0,
            _bytes: bytes,
        })
    }
}

impl ImageHandle for FakeImageHandle {
    fn info(&self) -> ImageInfo {
        let icc_len = self
            .faults
            .iter()
            .find_map(|f| match f {
                ImageFault::IccLength(n) => Some(*n),
                _ => None,
            })
            .unwrap_or_else(|| self.image.icc.as_ref().map_or(0, |v| v.len() as u64));
        ImageInfo {
            width: self.image.width,
            height: self.image.height,
            format: self.image.format,
            alpha: self.image.alpha,
            orientation: self.image.orientation,
            frames: u32::try_from(self.image.frames.len()).unwrap_or(u32::MAX),
            loop_count: self.image.loop_count,
            icc_len,
            exif_len: self.image.exif.as_ref().map_or(0, |v| v.len() as u64),
            hdr: self.image.hdr,
        }
    }

    fn begin(&mut self, index: u32, _mon: &dyn Monitor) -> Result<DecodedFrame, BackendError> {
        add(&self.counters.begins);
        let (mut width, mut height, mut format) =
            (self.image.width, self.image.height, self.image.format);
        for f in &self.faults {
            match f {
                ImageFault::Begin(e) => return Err(e.clone()),
                ImageFault::DecodedSize(w, h) => (width, height) = (*w, *h),
                ImageFault::DecodedFormat(fmt) => format = *fmt,
                _ => {}
            }
        }
        let i = index as usize;
        let (_, duration_ms) = self
            .image
            .frames
            .get(i)
            .ok_or_else(|| BackendError::Invalid(format!("フレーム {index} はありません")))?;
        self.begun = Some(i);
        Ok(DecodedFrame {
            width,
            height,
            format,
            alpha: self.image.alpha,
            duration_ms: *duration_ms,
        })
    }

    fn read_rows(
        &mut self,
        y0: u32,
        rows: u32,
        row_bytes: usize,
        out: &mut [u8],
    ) -> Result<(), BackendError> {
        add(&self.counters.bands);
        let n = self.bands;
        self.bands += 1;
        for f in &self.faults {
            if let ImageFault::Band(k, e) = f
                && *k == n
            {
                return Err(e.clone());
            }
        }
        let i = self
            .begun
            .ok_or_else(|| BackendError::Invalid("展開する前に帯を読みました".into()))?;
        let want_row = (self.image.width as usize).checked_mul(self.image.format.bytes_per_pixel());
        let len = (rows as usize).checked_mul(row_bytes);
        let end_row = y0.checked_add(rows);
        if want_row != Some(row_bytes)
            || len != Some(out.len())
            || end_row.is_none_or(|e| e > self.image.height)
        {
            return Err(BackendError::Invalid("帯の範囲・長さが合いません".into()));
        }
        let start = (y0 as usize).saturating_mul(row_bytes);
        let src = self
            .image
            .frames
            .get(i)
            .and_then(|(px, _)| px.get(start..start.saturating_add(out.len())))
            .ok_or_else(|| BackendError::Invalid("画素が足りません".into()))?;
        out.copy_from_slice(src);
        Ok(())
    }

    fn copy_icc(&mut self, out: &mut [u8]) -> Result<(), BackendError> {
        copy_exact(self.image.icc.as_deref(), out)
    }

    fn copy_exif(&mut self, out: &mut [u8]) -> Result<(), BackendError> {
        copy_exact(self.image.exif.as_deref(), out)
    }
}

fn copy_exact(src: Option<&[u8]>, out: &mut [u8]) -> Result<(), BackendError> {
    match src {
        Some(s) if s.len() == out.len() => {
            out.copy_from_slice(s);
            Ok(())
        }
        _ => Err(BackendError::Invalid("写す先の長さが合いません".into())),
    }
}

/// テスト用：ストレートの画素（行を詰めたもの）をその場でプレマルチプライドにする（`(c·a + 最大/2) / 最大`。四捨五入）。
pub fn premultiply_rows(fmt: PixelFormat, buf: &mut [u8]) {
    match fmt {
        PixelFormat::GrayA8 | PixelFormat::Rgba8 => {
            let n = fmt.bytes_per_pixel();
            for px in buf.chunks_exact_mut(n) {
                let a = u32::from(px[n - 1]);
                for c in &mut px[..n - 1] {
                    *c = ((u32::from(*c) * a + 127) / 255) as u8;
                }
            }
        }
        PixelFormat::GrayA16 | PixelFormat::Rgba16 => {
            let n = fmt.bytes_per_pixel();
            for px in buf.chunks_exact_mut(n) {
                let a = u64::from(u16::from_le_bytes([px[n - 2], px[n - 1]]));
                for c in px[..n - 2].as_chunks_mut::<2>().0 {
                    let v = u64::from(u16::from_le_bytes([c[0], c[1]]));
                    c.copy_from_slice(&(((v * a + 32_767) / 65_535) as u16).to_le_bytes());
                }
            }
        }
        _ => {}
    }
}

/// 偽の音声（backend が「デコードした」PCM）。
#[derive(Debug, Clone, PartialEq)]
pub struct FakeAudio {
    pub rate: u32,
    pub planes: Vec<Vec<f32>>,
    /// 申告する長さ（`None` は実際の長さ）。
    pub declared_frames: Option<u64>,
    /// 1 回の読み取りで書く数の上限（0 は求めた数まで）。
    pub chunk: usize,
    pub source: Option<AudioSource>,
    pub priming: Priming,
}

impl FakeAudio {
    /// 申告どおりの長さで、求めた数まで書く。
    pub fn new(rate: u32, planes: Vec<Vec<f32>>) -> Self {
        Self {
            rate,
            planes,
            declared_frames: None,
            chunk: 0,
            source: None,
            priming: Priming::None,
        }
    }

    fn frames(&self) -> usize {
        self.planes.first().map_or(0, Vec::len)
    }
}

/// 偽の音声の backend の失敗のしかた。
#[derive(Debug, Clone, PartialEq)]
pub enum AudioFault {
    /// 開くときに失敗する。
    Open(BackendError),
    /// `n` 回目（0 から）の読み取りで失敗する。
    Read(u64, BackendError),
    /// 求めた数より多く書いたと答える（約束違反）。
    Overrun,
}

/// 偽の音声の backend（`codec::platform::AudioBackend`）。
pub struct FakeAudioBackend {
    pub id: &'static str,
    pub formats: Vec<&'static str>,
    pub audio: FakeAudio,
    pub faults: Vec<AudioFault>,
    pub env: Option<CodecEnv>,
    counters: Arc<FakeCounters>,
}

impl FakeAudioBackend {
    /// 識別子 `fake-audiotoolbox-dec/1`、形式 m4a・aac・ogg・opus、環境は [`FakeAudioBackend::default_env`]。
    pub fn new(audio: FakeAudio) -> Self {
        Self {
            id: "fake-audiotoolbox-dec/1",
            formats: vec!["m4a", "aac", "ogg", "opus"],
            audio,
            faults: Vec::new(),
            env: Some(Self::default_env()),
            counters: Arc::default(),
        }
    }

    /// 既定の環境の記録。
    pub fn default_env() -> CodecEnv {
        CodecEnv::new("fake", "1.0").with("audiotoolbox", "0.1")
    }

    /// 数えたもの（ハンドルと共有する）。
    pub fn counters(&self) -> Arc<FakeCounters> {
        self.counters.clone()
    }
}

/// 偽の音声の backend のハンドル。
pub struct FakeAudioHandle {
    audio: FakeAudio,
    faults: Vec<AudioFault>,
    counters: Arc<FakeCounters>,
    pos: usize,
    calls: u64,
}

impl Drop for FakeAudioHandle {
    fn drop(&mut self) {
        add(&self.counters.closes);
    }
}

impl AudioBackend for FakeAudioBackend {
    type Handle = FakeAudioHandle;

    fn id(&self) -> &'static str {
        self.id
    }

    fn can_decode(&self, format: &str) -> bool {
        self.formats.contains(&format)
    }

    fn env(&self) -> Option<CodecEnv> {
        self.env.clone()
    }

    fn open(&self, _bytes: Arc<Vec<u8>>, _format: &str) -> Result<FakeAudioHandle, BackendError> {
        add(&self.counters.opens);
        for f in &self.faults {
            if let AudioFault::Open(e) = f {
                return Err(e.clone());
            }
        }
        Ok(FakeAudioHandle {
            audio: self.audio.clone(),
            faults: self.faults.clone(),
            counters: self.counters.clone(),
            pos: 0,
            calls: 0,
        })
    }
}

impl AudioHandle for FakeAudioHandle {
    fn info(&self) -> AudioInfo {
        AudioInfo {
            rate: self.audio.rate,
            channels: u16::try_from(self.audio.planes.len()).unwrap_or(u16::MAX),
            frames: self
                .audio
                .declared_frames
                .unwrap_or(self.audio.frames() as u64),
            priming: self.audio.priming,
            source: self.audio.source,
        }
    }

    fn read(&mut self, planes: &mut [&mut [f32]]) -> Result<usize, BackendError> {
        add(&self.counters.reads);
        let call = self.calls;
        self.calls += 1;
        let mut overrun = false;
        for f in &self.faults {
            match f {
                AudioFault::Read(k, e) if *k == call => return Err(e.clone()),
                AudioFault::Overrun => overrun = true,
                _ => {}
            }
        }
        let Some(want) = planes.first().map(|p| p.len()) else {
            return Err(BackendError::Invalid("チャンネルがありません".into()));
        };
        if planes.len() != self.audio.planes.len() || planes.iter().any(|p| p.len() != want) {
            return Err(BackendError::Invalid(
                "チャンネルの数・長さが合いません".into(),
            ));
        }
        if overrun {
            return Ok(want + 1);
        }
        let mut n = want.min(self.audio.frames().saturating_sub(self.pos));
        if self.audio.chunk > 0 {
            n = n.min(self.audio.chunk);
        }
        for (dst, src) in planes.iter_mut().zip(&self.audio.planes) {
            let s = src
                .get(self.pos..self.pos + n)
                .ok_or_else(|| BackendError::Invalid("標本が足りません".into()))?;
            dst[..n].copy_from_slice(s);
        }
        self.pos += n;
        Ok(n)
    }
}

/// 偽の AAC の backend の失敗のしかた。
#[derive(Debug, Clone, PartialEq)]
pub enum AacFault {
    /// 開くときに失敗する。
    Open(BackendError),
    /// `n` 回目（0 から）の書き込みで失敗する。
    Write(u64, BackendError),
    /// 書き終えるときに失敗する。
    Finish(BackendError),
    /// 出力の長さを上限より 1 大きく申告する。
    Oversize,
    /// m4a でないバイト列を出す。
    Broken,
}

/// 偽の AAC の backend（`codec::platform::AacBackend`）。出力は [`fake_m4a`] の形（時刻は 0 でない値）で、mdat に
/// 受け取った PCM（[`fake_m4a_payload`] で読める）を入れる。最後に開いたときの設定を覚える。
pub struct FakeAacBackend {
    pub id: &'static str,
    /// 書けるレート。
    pub rates: Vec<u32>,
    /// 書けるチャンネルの数の上限。
    pub max_channels: u16,
    /// 選べるビットレート（kbps。形によらず同じ）。
    pub bitrates: Vec<u32>,
    pub env: Option<CodecEnv>,
    pub fault: Option<AacFault>,
    last: Arc<Mutex<Option<AacSettings>>>,
    counters: Arc<FakeCounters>,
}

impl Default for FakeAacBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeAacBackend {
    /// 識別子 `fake-aac-enc/1`、AAC-LC のレート（8〜48 kHz）・8 チャンネルまで・32〜320 kbps。
    pub fn new() -> Self {
        Self {
            id: "fake-aac-enc/1",
            rates: vec![
                8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000,
            ],
            max_channels: 8,
            bitrates: vec![32, 48, 64, 96, 128, 160, 192, 256, 320],
            env: Some(Self::default_env()),
            fault: None,
            last: Arc::default(),
            counters: Arc::default(),
        }
    }

    /// 既定の環境の記録。
    pub fn default_env() -> CodecEnv {
        CodecEnv::new("fake", "1.0").with("audiotoolbox", "0.1")
    }

    /// 最後に開いたときの設定（ハンドルと共有する）。
    pub fn last_settings(&self) -> Arc<Mutex<Option<AacSettings>>> {
        self.last.clone()
    }

    /// 数えたもの。
    pub fn counters(&self) -> Arc<FakeCounters> {
        self.counters.clone()
    }
}

/// 偽の AAC の backend のハンドル。
pub struct FakeAacHandle {
    settings: AacSettings,
    max_out: u64,
    fault: Option<AacFault>,
    counters: Arc<FakeCounters>,
    planes: Vec<Vec<f32>>,
    calls: u64,
    out: Vec<u8>,
}

impl Drop for FakeAacHandle {
    fn drop(&mut self) {
        add(&self.counters.closes);
    }
}

impl AacBackend for FakeAacBackend {
    type Handle = FakeAacHandle;

    fn id(&self) -> &'static str {
        self.id
    }

    fn env(&self) -> Option<CodecEnv> {
        self.env.clone()
    }

    fn supports(&self, rate: u32, channels: u16) -> bool {
        self.rates.contains(&rate) && (1..=self.max_channels).contains(&channels)
    }

    fn bitrates(&self, _rate: u32, _channels: u16) -> Vec<u32> {
        self.bitrates.clone()
    }

    fn open(&self, settings: &AacSettings, max_out: u64) -> Result<FakeAacHandle, BackendError> {
        add(&self.counters.opens);
        if let Some(AacFault::Open(e)) = &self.fault {
            return Err(e.clone());
        }
        if let Ok(mut last) = self.last.lock() {
            *last = Some(*settings);
        }
        Ok(FakeAacHandle {
            settings: *settings,
            max_out,
            fault: self.fault.clone(),
            counters: self.counters.clone(),
            planes: vec![Vec::new(); usize::from(settings.channels)],
            calls: 0,
            out: Vec::new(),
        })
    }
}

impl AacHandle for FakeAacHandle {
    fn write(&mut self, planes: &[&[f32]]) -> Result<(), BackendError> {
        add(&self.counters.writes);
        let call = self.calls;
        self.calls += 1;
        if let Some(AacFault::Write(k, e)) = &self.fault
            && *k == call
        {
            return Err(e.clone());
        }
        let n = planes.first().map_or(0, |p| p.len());
        if planes.len() != usize::from(self.settings.channels)
            || planes.iter().any(|p| p.len() != n)
        {
            return Err(BackendError::Invalid(
                "チャンネルの数・長さが合いません".into(),
            ));
        }
        for (dst, src) in self.planes.iter_mut().zip(planes) {
            dst.extend_from_slice(src);
        }
        Ok(())
    }

    fn finish(&mut self, _mon: &dyn Monitor) -> Result<u64, BackendError> {
        if let Some(AacFault::Finish(e)) = &self.fault {
            return Err(e.clone());
        }
        let frames = self.planes.first().map_or(0, Vec::len);
        let mut payload = Vec::new();
        payload.extend_from_slice(&(self.planes.len() as u32).to_le_bytes());
        payload.extend_from_slice(&(self.calls as u32).to_le_bytes());
        payload.extend_from_slice(&(frames as u64).to_le_bytes());
        for p in &self.planes {
            for s in p {
                payload.extend_from_slice(&s.to_le_bytes());
            }
        }
        self.out = match self.fault {
            Some(AacFault::Broken) => b"not an mp4 file".to_vec(),
            _ => fake_m4a(&payload, 0xDEAD_BEEF),
        };
        match self.fault {
            Some(AacFault::Oversize) => Ok(self.max_out.saturating_add(1)),
            _ => Ok(self.out.len() as u64),
        }
    }

    fn copy(&mut self, out: &mut [u8]) -> Result<(), BackendError> {
        copy_exact(Some(&self.out), out)
    }
}

/// 箱を 1 つ書く。
fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + body.len());
    b.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    b
}

/// テスト用の小さな m4a の形：ftyp、64 ビットの大きさの free、moov（mvhd 版 0・trak（tkhd 版 0・mdia（mdhd 版 1・hdlr））、
/// 大きさ 0（終わりまで）の mdat（中身は `payload`）。作成・変更の時刻はどれも `time`（版 1 は上下の 32 ビットとも）。
pub fn fake_m4a(payload: &[u8], time: u32) -> Vec<u8> {
    let t = time.to_be_bytes();
    let full = |version: u8, times: &[u8], rest: usize| {
        let mut b = vec![version, 0, 0, 0];
        b.extend_from_slice(times);
        b.resize(b.len() + rest, 0);
        b
    };
    let t32: Vec<u8> = [t, t].concat();
    let t64: Vec<u8> = [t, t, t, t].concat();
    let mdia = [
        mp4_box(b"mdhd", &full(1, &t64, 16)),
        mp4_box(b"hdlr", &[0u8; 24]),
    ]
    .concat();
    let trak = [
        mp4_box(b"tkhd", &full(0, &t32, 72)),
        mp4_box(b"mdia", &mdia),
    ]
    .concat();
    let moov = [
        mp4_box(b"mvhd", &full(0, &t32, 88)),
        mp4_box(b"trak", &trak),
    ]
    .concat();
    let mut out = mp4_box(b"ftyp", b"M4A \0\0\0\0");
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(b"free");
    out.extend_from_slice(&24u64.to_be_bytes());
    out.extend_from_slice(&[0u8; 8]);
    out.extend(mp4_box(b"moov", &moov));
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(payload);
    out
}

/// [`FakeAacBackend`] の出力の mdat から、受け取った PCM（チャンネルごと）と書き込みの回数を読む。
pub fn fake_m4a_payload(m4a: &[u8]) -> Option<(Vec<Vec<f32>>, usize)> {
    let mdat = boxes(m4a, 0, m4a.len())
        .ok()?
        .into_iter()
        .find(|b| &b.kind == b"mdat")?;
    let p = m4a.get(mdat.body..mdat.end)?;
    let u32_at = |i: usize| -> Option<u32> {
        let b = p.get(i..i + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let channels = u32_at(0)? as usize;
    let calls = u32_at(4)? as usize;
    let frames = u64::from(u32_at(8)?) | (u64::from(u32_at(12)?) << 32);
    let frames = usize::try_from(frames).ok()?;
    let mut planes = Vec::new();
    let mut at = 16usize;
    for _ in 0..channels {
        let mut plane = Vec::new();
        for _ in 0..frames {
            plane.push(f32::from_bits(u32_at(at)?));
            at += 4;
        }
        planes.push(plane);
    }
    Some((planes, calls))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::codec::CodecSet;
    use crate::monitor::NoMonitor;

    fn gen_audio() -> InputGen {
        InputGen::AudioNoise {
            rate: 8_000,
            channels: 2,
            frames: 16,
            seed: 3,
        }
    }

    fn read(
        set: &CodecSet,
        format: &str,
    ) -> Result<(Value, crate::codec::DecodeStamp), EngineError> {
        set.decode(
            &mut Cursor::new(vec![9u8; 10]),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
    }

    /// 安定なら何度読んでも同じ値、読むたびに変わるなら毎回違う値（記録のダイジェストも）。記録は Platform・再現なし・環境つき。
    #[test]
    fn stable_and_drifting_reads() {
        let mut set = CodecSet::default();
        set.add_decoder(Arc::new(FakePlatformDecoder::new(
            "fake-stable-dec/1",
            &["wav"],
            gen_audio(),
        )))
        .unwrap();
        let a = read(&set, "wav").unwrap().1;
        let b = read(&set, "wav").unwrap().1;
        assert_eq!(a, b);
        assert_eq!(
            (a.tier, a.reproducible, a.env.clone()),
            (
                Tier::Platform,
                false,
                Some(FakePlatformDecoder::default_env())
            )
        );
        for input in [
            gen_audio(),
            InputGen::Noise {
                w: 3,
                h: 2,
                fmt: PixelFormat::Rgba16,
                seed: 1,
            },
            InputGen::Noise {
                w: 3,
                h: 2,
                fmt: PixelFormat::Indexed8,
                seed: 1,
            },
            InputGen::NoiseFrames {
                w: 3,
                h: 2,
                fmt: PixelFormat::Rgb8,
                count: 2,
                seed: 1,
            },
        ] {
            let fake =
                Arc::new(FakePlatformDecoder::new("fake-drift-dec/1", &["ogg"], input).drifting());
            let mut set = CodecSet::default();
            set.add_decoder(fake.clone()).unwrap();
            let digests: Vec<_> = (0..5)
                .map(|_| read(&set, "ogg").unwrap().1.value_digest)
                .collect();
            assert_eq!(fake.reads(), 5);
            for (i, d) in digests.iter().enumerate() {
                assert_eq!(*d, fake.value_for(i as u64).unwrap().digest().unwrap());
                assert!(digests[..i].iter().all(|e| e != d), "{i}");
            }
        }
    }

    /// 失敗のしかた：`Unsupported` は次の段へ回り、`Decode` はそこで止まる。識別子を変えた組では記録の識別子が見つからない。
    #[test]
    fn failures_and_missing_ids() {
        let mut set = CodecSet::default();
        set.add_decoder(Arc::new(
            FakePlatformDecoder::new("fake-a-dec/1", &["m4a"], gen_audio())
                .failing(FakeFailure::Unsupported),
        ))
        .unwrap();
        set.add_decoder(Arc::new(
            FakePlatformDecoder::new("fake-ext-dec/1", &["m4a"], gen_audio())
                .with_tier(Tier::External)
                .with_env(CodecEnv::new("ffmpeg", "9.0.2")),
        ))
        .unwrap();
        let stamp = read(&set, "m4a").unwrap().1;
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier),
            ("fake-ext-dec/1", Tier::External)
        );
        assert_eq!(stamp.env.map(|e| e.tool), Some("ffmpeg".into()));
        let mut broken = CodecSet::default();
        broken
            .add_decoder(Arc::new(
                FakePlatformDecoder::new("fake-b-dec/1", &["m4a"], gen_audio())
                    .failing(FakeFailure::Decode),
            ))
            .unwrap();
        assert!(matches!(
            read(&broken, "m4a"),
            Err(EngineError::Decode { .. })
        ));
        // 「識別子の版が変わった環境」：/2 だけの組では、/1 の記録で読み直せない
        let mut newer = CodecSet::default();
        newer
            .add_decoder(Arc::new(FakePlatformDecoder::new(
                "fake-a-dec/2",
                &["m4a"],
                gen_audio(),
            )))
            .unwrap();
        assert!(matches!(
            newer.decode_with(
                "fake-a-dec/1",
                &mut Cursor::new(vec![0u8]),
                "m4a",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor
            ),
            Err(EngineError::Unsupported(_))
        ));
    }

    /// 偽の m4a の形と、中身の読み戻し。
    #[test]
    fn fake_m4a_round_trips_payload() {
        let mut h = FakeAacBackend::new()
            .open(
                &AacSettings {
                    rate: 8_000,
                    channels: 2,
                    channel_mask: 3,
                    mode: crate::codec::platform::AacMode::Cbr,
                    bitrate_kbps: 64,
                    quality: 0,
                },
                1 << 20,
            )
            .unwrap();
        h.write(&[&[0.5, 0.25], &[-1.0, 1.0]]).unwrap();
        h.write(&[&[0.125], &[0.0]]).unwrap();
        let n = h.finish(&NoMonitor).unwrap();
        let mut out = vec![0u8; n as usize];
        h.copy(&mut out).unwrap();
        let (planes, calls) = fake_m4a_payload(&out).unwrap();
        assert_eq!(calls, 2);
        assert_eq!(planes, [vec![0.5, 0.25, 0.125], vec![-1.0, 1.0, 0.0]]);
        assert!(h.write(&[&[0.0]]).is_err());
    }
}
