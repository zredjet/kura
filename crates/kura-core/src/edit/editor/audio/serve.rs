//! `/edit` の音声のルートの中身（M13 段 4。docs/editing.md §12.13、docs/architecture.md §4）。
//!
//! - `/edit/{s}/{rev}/pcm/{a}-{b}`（[`serve_pcm`]）：エディタが公開した今か直前の表示（rev が同じもの）の、出力のフレーム
//!   `[a, b)` の PCM。M11 の WAV（kura チャンク付き。`asset::pcm::write_wav`）で、kura チャンクは `{ start: a, total: 出力の長さ,
//!   loop: 出力のループ, decoder: "kura-edit/1:" + samplesId }`。区間が表示の値の有効なチャンクに覆われなければ 404。
//! - `/edit/{s}/input/pcm/{a}-{b}`（[`serve_input_pcm`]）：元（文書の入力）の PCM。遅延の値なら源の LRU を通って読む。kura チャンクは
//!   `{ total: 元の長さ, loop: 元のループ, decoder: 元の読み取りのデコーダ }`。
//! - どれも描いている途中の錠を待たない（`published` を短く持って写すだけ）。予算は M11 の `/pcm` と同じく、写す分と書く分を
//!   対話で待たずに確保する（足りなければ 503）。範囲の外・浮動小数で 64 MiB を超える長さは 416。PCM は残させない（no-store）。

use kura_engine::Value;
use kura_engine::render::region::{Area, PartialValue};

use crate::asset::pcm::{
    self, KuraChunk, MAX_PCM_BYTES, PcmError, sample_format, wav_len, write_wav,
};
use crate::edit::EngineServices;

/// 出力の PCM の kura チャンクのデコーダの識別子の頭（`kura-edit/1:{samplesId}`）。
pub const EDIT_DECODER_PREFIX: &str = "kura-edit/1:";

/// 区間を確かめる：`start < end ≤ total`（でなければ 416）、浮動小数で 64 MiB 以下（でなければ 416）。返すのは浮動小数のバイト数。
fn check_span(start: u64, end: u64, total: u64, channels: u64) -> Result<u64, PcmError> {
    if start >= end || end > total {
        return Err(PcmError::OutOfRange { start, end, total });
    }
    let floats = (end - start).saturating_mul(channels).saturating_mul(4);
    if floats > MAX_PCM_BYTES {
        return Err(PcmError::TooLong { bytes: floats });
    }
    Ok(floats)
}

/// 書く PCM の形（チャンネル数・レート・全体の長さ・メタデータ・kura チャンクのデコーダ）。
struct Form<'a> {
    channels: usize,
    rate: u32,
    total: u64,
    meta: &'a kura_engine::value::AudioMeta,
    decoder: &'a str,
}

/// チャンネルごとに区間を読んで WAV にする（写す分・書く分を予算で数える）。
fn write_span(
    engine: &EngineServices,
    form: &Form<'_>,
    (start, end, floats): (u64, u64, u64),
    read: impl Fn(usize, u64, &mut [f32]) -> Result<(), kura_engine::EngineError>,
) -> Result<Vec<u8>, PcmError> {
    let Form {
        channels,
        rate,
        total,
        meta,
        decoder,
    } = *form;
    let runtime = engine
        .runtime_if_built()
        .ok_or_else(|| PcmError::Unavailable("実行の場がありません".into()))?;
    let n = usize::try_from(end - start).map_err(|_| PcmError::TooLong { bytes: floats })?;
    let _copy = pcm::acquire(runtime.budget(), floats)?;
    let mut owned = Vec::with_capacity(channels);
    for ch in 0..channels {
        let mut buf = Vec::new();
        buf.try_reserve_exact(n)
            .map_err(|_| PcmError::TooLong { bytes: floats })?;
        buf.resize(n, 0.0);
        read(ch, start, &mut buf).map_err(|e| PcmError::Failed(e.to_string()))?;
        owned.push(buf);
    }
    let planes: Vec<&[f32]> = owned.iter().map(Vec::as_slice).collect();
    let format = sample_format(meta.source.as_ref(), &planes);
    let _writing = pcm::acquire(
        runtime.budget(),
        wav_len(end - start, channels as u64, format, decoder.len()),
    )?;
    let loop_range = meta
        .loop_range
        .filter(|l| l.start < l.end && l.end <= total)
        .map(|l| (l.start, l.end));
    write_wav(
        &planes,
        rate,
        format,
        &KuraChunk {
            start,
            total,
            loop_range,
            decoder,
        },
    )
}

/// 公開した出力の表示の値の、フレーム `[start, end)` の PCM の WAV（モジュールの説明）。
pub fn serve_pcm(
    engine: &EngineServices,
    session: &str,
    rev: u64,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, PcmError> {
    let editor = engine.editor_for(session).ok_or(PcmError::NotFound)?;
    let (value, samples_id): (PartialValue, String) =
        editor.published_audio(rev).ok_or(PcmError::NotFound)?;
    let kura_engine::Shape::Audio {
        rate,
        channels,
        frames,
    } = value.shape()
    else {
        return Err(PcmError::NotFound);
    };
    let total = frames.max();
    let floats = check_span(start, end, total, u64::from(channels))?;
    if !value.covers(&Area::span(start, end)) {
        return Err(PcmError::NotFound);
    }
    let meta = value.audio_meta().cloned().unwrap_or_default();
    let decoder = format!("{EDIT_DECODER_PREFIX}{samples_id}");
    let form = Form {
        channels: usize::from(channels),
        rate,
        total,
        meta: &meta,
        decoder: &decoder,
    };
    write_span(engine, &form, (start, end, floats), |ch, at, out| {
        value.audio_read(ch, at, out)
    })
}

/// 元（文書の入力）の、フレーム `[start, end)` の PCM の WAV（モジュールの説明）。
pub fn serve_input_pcm(
    engine: &EngineServices,
    session: &str,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, PcmError> {
    let editor = engine.editor_for(session).ok_or(PcmError::NotFound)?;
    let (value, decoder) = editor.published_input().ok_or(PcmError::NotFound)?;
    let Value::Audio(a) = value else {
        return Err(PcmError::NotFound);
    };
    let total = a.frames() as u64;
    let floats = check_span(start, end, total, a.channels() as u64)?;
    let form = Form {
        channels: a.channels(),
        rate: a.rate(),
        total,
        meta: &a.meta,
        decoder: &decoder,
    };
    write_span(engine, &form, (start, end, floats), |ch, at, out| {
        a.read(ch, at, out)
    })
}
