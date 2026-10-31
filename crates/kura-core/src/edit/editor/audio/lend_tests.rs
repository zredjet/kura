//! 保存に貸す入力（S24。M13 段 4C。docs/editing.md §14・§12.12）：音声のエディタは、遅延で開いた入力のうち読み取りの記録が
//! 分かるもの（WAV・AIFF の直接読み・作業用のファイル・アンカー）を開いたときに保存に貸し（`EngineServices::lend_input`）、
//! 閉じると引き上げる（`withdraw_input`）。保存は貸した入力を使い、元を読み直さない（段 3 の S21 をエディタの道で回す）。

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use kura_engine::audio::source::SourceStats;
use kura_engine::codec::CodecSet;
use kura_engine::codec::wav;
use kura_engine::doc::{DecodeOptions, FormatChoice};
use kura_engine::testgen::InputGen;
use kura_engine::value::Chunk;
use kura_engine::{Digest, Dim, NoMonitor, Shape, Value};
use rusqlite::params;
use serde_json::json;

use super::session_tests::*;
use super::work_tests::{Stage, Staged, pinned_decoder, plain_wav_bytes};
use crate::edit::anchors::options_digest;
use crate::edit::editor::{SaveMode, Step, Viewport, close, save};
use crate::edit::save::Stage as SaveStage;
use crate::edit::services::{LentAnchor, LentInput};
use crate::edit::{EngineFactory, RuntimeConfig, pool_sizes, versions};
use crate::jobs::JobState;
use crate::library::Library;

const WAIT: Duration = Duration::from_secs(120);

/// 元の中身の事実の行を置く（解析のジョブの代わり。固定の段で読んだ記録）。
fn place_facts(lib: &Library, id: i64, bytes: &[u8], format: &str) {
    let (_, stamp) = CodecSet::pinned()
        .decode(
            &mut std::io::Cursor::new(bytes),
            format,
            &Default::default(),
            &Default::default(),
            &NoMonitor,
        )
        .unwrap();
    let digest = stamp.value_digest.0.to_vec();
    let decoder = stamp.codec.clone();
    lib.db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO media_facts (blob_id, state, decoder, reproducible, value_digest, facts_v, analyzed_at)
                 SELECT blob_id, 'ok', ?2, 1, ?3, 1, 1 FROM items WHERE id = ?1",
                params![id, decoder, digest],
            )?;
            Ok(())
        })
        .unwrap();
}

/// アイテム `id`（元の中身 `hash`・既定の読み方）について、エディタが貸している入力（なければ None）。
fn lent(env: &Env, id: i64, hash: &[u8; 32]) -> Option<LentInput> {
    let digest = Digest(options_digest(&DecodeOptions::default()).unwrap());
    env.engine.borrow_input(id, hash, &digest)
}

fn hash_of(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// 遅延の値の源の統計（最初のチャンクの源）。
fn source_stats(value: &Value) -> SourceStats {
    let Value::Audio(a) = value else {
        panic!("音声ではありません")
    };
    match a.chunk(0, 0).unwrap() {
        Chunk::Source(r) => r.src.stats(),
        other => panic!("遅延の値ではありません: {other:?}"),
    }
}

/// 遅延の値の源の LRU の上限（最初のチャンクの源）。
fn source_cap(value: &Value) -> u64 {
    let Value::Audio(a) = value else {
        panic!("音声ではありません")
    };
    match a.chunk(0, 0).unwrap() {
        Chunk::Source(r) => r.src.resident_bytes(),
        other => panic!("遅延の値ではありません: {other:?}"),
    }
}

/// 保存の流れの計画の確保の直前に、見積もりの源の LRU とアンカーに昇格するかを記す割り込みを入れる（記したものを返す）。
fn watch_admits(env: &Env) -> Arc<Mutex<Vec<(u64, bool)>>> {
    let seen: Arc<Mutex<Vec<(u64, bool)>>> = Arc::default();
    let log = seen.clone();
    env.engine.set_hook(Some(Arc::new(move |stage| {
        if let SaveStage::Admit { resident, promote } = stage {
            log.lock().unwrap().push((resident, promote));
        }
    })));
    seen
}

/// 保存を積んで終わるまで待ち、成功を確かめる。
fn save_and_wait(env: &Env, session: &str, format: Option<FormatChoice>) -> serde_json::Value {
    let job = save(&env.env(), session, SaveMode::Overwrite, format)
        .unwrap()
        .job;
    assert!(env.jobs.wait_idle(WAIT));
    let record = env.jobs.get(job).unwrap();
    assert_eq!(record.state, JobState::Done, "{:?}", record.message);
    record
        .result_json
        .as_deref()
        .and_then(|r| serde_json::from_str(r).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// アイテムの今のファイルのバイト列。
fn saved_bytes(env: &Env, id: i64) -> Vec<u8> {
    let head = env
        .lib
        .db()
        .read(|c| versions::head(c, id))
        .unwrap()
        .unwrap();
    std::fs::read(env.lib.layout().content_path(&head.hash, &head.ext)).unwrap()
}

fn gain() -> Vec<Step> {
    vec![add(0, "audio.gain", json!({ "db": -3.0 }))]
}

/// S24（WAV）：記録の分かる WAV を遅延で開いたエディタは、入力を照らし済み（`verified`）で貸す。保存は貸した入力（エディタの
/// 源）を通り、元を読み直さない（源から読んだチャンクが増えない・照らしの回もない）。結果は貸さないときとバイト単位で同じ。
/// 閉じると貸し出しが消える。
#[test]
fn s24_lent_wav_is_not_read_again() {
    let bytes = noise_wav(8_000, 2, 3 * C + 17, 41);
    let hash = hash_of(&bytes);

    let env = Env::new();
    env.jobs.start().unwrap();
    let id = add_audio(&env.lib, &bytes, "wav");
    place_facts(&env.lib, id, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    assert!(opened.source.audio.as_ref().unwrap().lazy);
    let l = lent(&env, id, &hash).expect("記録の分かる WAV は貸す");
    assert!(
        l.verified,
        "中身のハッシュの名前のファイル・同じデコーダの記録は照らし済み"
    );
    assert!(l.work.is_none());
    assert!(matches!(l.anchor, LentAnchor::None));
    assert_eq!((l.item_id, l.format.as_str()), (id, "wav"));
    assert_eq!(l.decoded.codec, wav::DEC_ID);
    let (input, _) = env.editor(&s).published_input().unwrap();
    assert!(l.value == input, "貸すのはエディタの入力（同じ源）");

    env.step(&s, gain(), &Viewport::audio_idle());
    let before = source_stats(&input);
    save_and_wait(&env, &s, None);
    let after = source_stats(&input);
    assert_eq!(after.reads, before.reads, "保存は元を読み直さない");
    assert!(after.hits > before.hits, "保存は貸した入力を通った");
    let got = saved_bytes(&env, id);

    // 貸さないとき：同じ元・同じ手で、貸し出しを引き上げてから保存する
    let other = Env::new();
    other.jobs.start().unwrap();
    let oid = add_audio(&other.lib, &bytes, "wav");
    place_facts(&other.lib, oid, &bytes, "wav");
    let o = other.open(oid);
    other.engine.withdraw_input(&o.session);
    assert!(lent(&other, oid, &hash).is_none());
    let (other_input, _) = other.editor(&o.session).published_input().unwrap();
    other.step(&o.session, gain(), &Viewport::audio_idle());
    let other_before = source_stats(&other_input);
    save_and_wait(&other, &o.session, None);
    assert_eq!(
        source_stats(&other_input).hits,
        other_before.hits,
        "貸さなければ保存は自分で元を開く"
    );
    assert!(
        got == saved_bytes(&other, oid),
        "貸したときと貸さないときでバイト列が違う"
    );

    // 閉じると引き上げる
    close(&env.engine, &s);
    assert!(lent(&env, id, &hash).is_none(), "閉じたら貸し出しが消える");
}

/// WAV を、固定の段の WAV と同じに読むが識別子の違う段（`plain-wav-dec/1`。読んだ数を `decodes` に数える）だけで読み、
/// 固定の段の WAV に書くライブラリ（予算 128 MiB：取り置き 64 MiB、作業用のファイルの境は 32 MiB）。遅延の源で開かないので、直接読めない
/// 形式の長い音声と同じに作業用のファイルの道を通る（長い値を速く作れる。読み手は再現するので、保存はアンカーを作らない）。
fn plain_wav_env(decodes: &Arc<AtomicU64>) -> Env {
    staged_wav_env(decodes, "plain-wav-dec/1", false)
}

/// [`plain_wav_env`] と同じだが、段を再現しない読み手（`unreproducible-wav-dec/1`。Platform・External の代わり）と申告する
/// ライブラリ。保存は作業用のファイルをアンカーに昇格する。
fn unreproducible_wav_env(decodes: &Arc<AtomicU64>) -> Env {
    staged_wav_env(decodes, "unreproducible-wav-dec/1", true)
}

/// WAV を識別子 `id` の段（固定の段の WAV と同じに読み、`unreproducible` なら再現しないと申告する）だけで読むライブラリ。
fn staged_wav_env(decodes: &Arc<AtomicU64>, id: &'static str, unreproducible: bool) -> Env {
    let decodes = decodes.clone();
    Env::with(Arc::new(Staged {
        budget: 128 << 20,
        make: Box::new(move || {
            let pinned = CodecSet::pinned();
            let mut set = CodecSet::default();
            let stage = Stage::renamed(pinned_decoder("wav"), id, "wav").counting(&decodes);
            let stage = if unreproducible {
                stage.unreproducible()
            } else {
                stage
            };
            set.add_decoder(Arc::new(stage)).unwrap();
            let probe = Shape::Audio {
                rate: 8_000,
                channels: 1,
                frames: Dim::Exact(1),
            };
            set.add_encoder(pinned.encoder("wav", &probe).unwrap().clone())
                .unwrap();
            set
        }),
    }))
}

/// 作業用のファイルにする長さ（モノラル・f32 で 32 MiB を超える。表示の範囲も全体でなくなるので、エディタが持ち続ける分は
/// 取り置きの中に収まり、保存は残りの予算で動ける）。
const LONG: u64 = 129 * C;

/// S24（作業用のファイル）：直接読めない形式の長い音声は作業用のファイルから開き、作業用のファイルの参照ごと照らし済みで貸す。
/// 保存は元を読み直さず（デコードが 0）、貸した入力（エディタの作業用のファイルの源）を通る。結果は貸さないときとバイト単位で
/// 同じ。閉じると貸し出しと作業用のファイルが消える。
#[test]
fn s24_lent_work_file_is_not_decoded_again() {
    let bytes = plain_wav_bytes(8_000, LONG);
    let hash = hash_of(&bytes);

    let decodes = Arc::new(AtomicU64::new(0));
    let env = plain_wav_env(&decodes);
    env.jobs.start().unwrap();
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    assert!(opened.source.audio.as_ref().unwrap().lazy);
    assert_eq!(
        decodes.load(Ordering::SeqCst),
        1,
        "作業用のファイルを作るのに 1 回読む"
    );
    assert_eq!(env.engine.work_pcm().usage().0, 1);
    let l = lent(&env, id, &hash).expect("作業用のファイルから開いた入力は貸す");
    assert!(l.verified, "作業用のファイルの封の記録は照らし済み");
    assert!(l.work.is_some(), "作業用のファイルの参照ごと貸す");
    assert!(matches!(l.anchor, LentAnchor::None));
    assert_eq!(l.decoded.codec, "plain-wav-dec/1");
    drop(l);
    let (input, _) = env.editor(&s).published_input().unwrap();

    env.step(&s, gain(), &Viewport::audio_idle());
    let before = source_stats(&input);
    save_and_wait(&env, &s, None);
    assert_eq!(decodes.load(Ordering::SeqCst), 1, "保存は元を読み直さない");
    let after = source_stats(&input);
    assert!(
        after.reads + after.hits > before.reads + before.hits,
        "保存は貸した入力（作業用のファイルの源）を通った"
    );
    let got = saved_bytes(&env, id);

    // 貸さないとき：同じ元・同じ手で、貸し出しを引き上げてから保存する
    let other_decodes = Arc::new(AtomicU64::new(0));
    let other = plain_wav_env(&other_decodes);
    other.jobs.start().unwrap();
    let oid = add_audio(&other.lib, &bytes, "wav");
    let o = other.open(oid);
    other.engine.withdraw_input(&o.session);
    let (other_input, _) = other.editor(&o.session).published_input().unwrap();
    other.step(&o.session, gain(), &Viewport::audio_idle());
    let other_before = source_stats(&other_input);
    save_and_wait(&other, &o.session, None);
    let other_after = source_stats(&other_input);
    assert_eq!(
        other_after.reads + other_after.hits,
        other_before.reads + other_before.hits,
        "貸さなければ保存はエディタの源を通らない"
    );
    assert!(
        got == saved_bytes(&other, oid),
        "貸したときと貸さないときでバイト列が違う"
    );

    // 閉じると貸し出しが消え、作業用のファイルはほかに使い手がなければ消える
    close(&env.engine, &s);
    assert!(lent(&env, id, &hash).is_none());
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
}

/// 貸すもの・貸さないもの：アンカーから遅延で開いた入力は照らしていない（`verified: false`）印で、アンカーの記録で貸す。
/// 記録の分からない WAV（事実の行がない）と、丸ごと読んだ入力（短い FLAC）は貸さない。ほかを開くと前の貸し出しは消える。
#[test]
fn only_lazy_inputs_with_a_known_reading_are_lent() {
    let env = Env::new();
    // アンカー
    let bytes = noise_wav(8_000, 2, 2 * C + 9, 61);
    let hash = hash_of(&bytes);
    let id = add_audio(&env.lib, &bytes, "wav");
    let recorded = place_anchor(&env.lib, &bytes, "wav", |d| d.clone());
    let opened = env.open(id);
    assert!(opened.source.audio.as_ref().unwrap().lazy);
    let l = lent(&env, id, &hash).expect("アンカーから遅延で開いた入力は貸す");
    assert!(!l.verified, "アンカーの値は保存が照らす");
    assert!(l.work.is_none());
    assert!(matches!(&l.anchor, LentAnchor::Existing(a) if a.decoded == recorded));
    assert_eq!(l.decoded, recorded);
    drop(l);

    // 記録の分からない WAV：前の貸し出しは消え、新しいものは貸さない
    let plain = noise_wav(8_000, 1, C + 3, 62);
    let pid = add_audio(&env.lib, &plain, "wav");
    let opened = env.open(pid);
    assert!(opened.source.audio.as_ref().unwrap().lazy);
    assert!(
        lent(&env, id, &hash).is_none(),
        "前のエディタの貸し出しは消える"
    );
    assert!(
        lent(&env, pid, &hash_of(&plain)).is_none(),
        "記録の分からない WAV は貸さない"
    );

    // 丸ごと読んだ FLAC
    let value = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 2 * C,
        seed: 63,
    }
    .generate()
    .unwrap();
    let flac = encode(&value, "flac");
    let fid = add_audio(&env.lib, &flac, "flac");
    let opened = env.open(fid);
    assert!(!opened.source.audio.as_ref().unwrap().lazy);
    assert!(
        lent(&env, fid, &hash_of(&flac)).is_none(),
        "丸ごと読んだ入力は貸さない"
    );
}

/// 16 コアの Mac（`pool_sizes(16)`：プレビュー 4・一括 12）・予算 1 GiB（取り置き 512 MiB）・固定の段の組。
struct SixteenCores;

impl EngineFactory for SixteenCores {
    fn runtime_config(&self) -> RuntimeConfig {
        let (preview, bulk) = pool_sizes(16);
        RuntimeConfig {
            cores: Some(16),
            preview_threads: Some(preview),
            bulk_threads: Some(bulk),
            budget: Some(1 << 30),
            ..RuntimeConfig::default()
        }
    }

    fn codecs(&self, _work_dir: &Path) -> CodecSet {
        CodecSet::pinned()
    }
}

/// `channels` チャンネル・`frames` フレームの 16 ビットの WAV（決まった形の波。速く作れる）。
fn wide_wav_bytes(rate: u32, channels: u16, frames: u64) -> Vec<u8> {
    let block = u32::from(channels) * 2;
    let data = frames * u64::from(block);
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&u32::try_from(36 + data).unwrap().to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * block).to_le_bytes());
    b.extend_from_slice(&u16::try_from(block).unwrap().to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&u32::try_from(data).unwrap().to_le_bytes());
    // 2,000 フレームの周期を繰り返す
    let mut period = Vec::with_capacity(2_000 * block as usize);
    for i in 0..2_000u64 {
        for ch in 0..u64::from(channels) {
            period.extend_from_slice(&((((i + 7 * ch) % 2_000) as i16) - 1_000).to_le_bytes());
        }
    }
    while (b.len() as u64) < 44 + data {
        let rest = (44 + data - b.len() as u64) as usize;
        b.extend_from_slice(&period[..rest.min(period.len())]);
    }
    b
}

/// 貸した源の LRU（M13 段 4C の検証の直し）：エディタは源の LRU をプレビューのスレッドの式（max(プレビュー × 4 チャンク ×
/// ch × 256 KiB, 64 MiB)）の上限で開いて貸す。多チャンネルでは保存の式（max(min(64 MiB, 全体), 一括 × ch × 256 KiB)）より
/// 大きいので、保存は貸した値の源の上限どおりに数える（エディタが保存の途中で閉じても、保存が流して読む間に源の LRU は上限まで
/// 埋まる）。16 コア（プレビュー 4・一括 12）の 24 チャンネルでは、源の上限 96 MiB に対して保存の式は 72 MiB。
#[test]
fn lent_source_lru_is_counted_at_its_cap() {
    const CHANNELS: u16 = 24;
    let frames = 16 * C;
    let bytes = wide_wav_bytes(8_000, CHANNELS, frames);
    let hash = hash_of(&bytes);

    let env = Env::with(Arc::new(SixteenCores));
    let admits = watch_admits(&env);
    env.jobs.start().unwrap();
    let id = add_audio(&env.lib, &bytes, "wav");
    place_facts(&env.lib, id, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    assert!(opened.source.audio.as_ref().unwrap().lazy);
    let l = lent(&env, id, &hash).expect("記録の分かる WAV は貸す");
    let cap = source_cap(&l.value);
    let chunk = C * u64::from(CHANNELS) * 4;
    assert_eq!(cap, 16 * chunk, "エディタの源の LRU は 4 × 4 チャンク");
    let (_, bulk) = pool_sizes(16);
    assert!(
        cap > (64u64 << 20).max(bulk as u64 * chunk),
        "源の上限（{cap}）が保存の式を超える組で確かめる"
    );
    drop(l);

    // 短く切って保存する（書く量を小さく。入力は 16 チャンクの貸した値のまま）
    env.step(
        &s,
        vec![add(0, "audio.trim", json!({ "start": 0, "end": C }))],
        &Viewport::audio_idle(),
    );
    save_and_wait(&env, &s, None);
    let seen = admits.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let (resident, _) = seen[0];
    assert!(
        resident >= cap,
        "保存の見積もりの源の LRU（{resident}）が貸した源の上限（{cap}）より小さい"
    );
}

/// 保存のときのアンカー（元 `hash`・既定の読み方。なければ None）。
fn anchor_of(env: &Env, hash: &[u8; 32]) -> Option<crate::edit::anchors::Anchor> {
    let digest = options_digest(&DecodeOptions::default()).unwrap();
    let hash = *hash;
    env.lib
        .db()
        .read(move |c| crate::edit::anchors::find(c, &hash, &digest))
        .unwrap()
}

/// S24（再現しない読み手。M13 段 4C の検証の直し）：Platform・External のように再現しない読み手（`unreproducible-wav-dec/1`）で
/// 読む長い音声は作業用のファイルから開き、参照ごと照らし済み・アンカーなしで貸す。保存はデコードし直さず、貸した作業用の
/// ファイルをアンカーに昇格する（created。アンカーの中身は作業用のファイルと同じ）。結果は貸さないときとバイト単位で同じ。
/// 開いたまま 2 回目を保存すると、貸し出しは開いたときのまま（アンカーなし）でも同じ記録のアンカーがあるので、昇格（staging への
/// 写し・照らし・全体を読む hash_file）をやり直さずにそのアンカーを使う（used）。保存のあとに閉じると、貸し出しと作業用の
/// ファイルが消える。
#[test]
fn s24_lent_unreproducible_work_file_is_promoted_once() {
    let bytes = plain_wav_bytes(8_000, LONG);
    let hash = hash_of(&bytes);

    let decodes = Arc::new(AtomicU64::new(0));
    let env = unreproducible_wav_env(&decodes);
    let admits = watch_admits(&env);
    env.jobs.start().unwrap();
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    assert!(opened.source.audio.as_ref().unwrap().lazy);
    assert_eq!(
        decodes.load(Ordering::SeqCst),
        1,
        "作業用のファイルを作るのに 1 回読む"
    );
    let l = lent(&env, id, &hash).expect("作業用のファイルから開いた入力は貸す");
    assert!(l.verified, "作業用のファイルの封の記録は照らし済み");
    assert!(matches!(l.anchor, LentAnchor::None));
    assert_eq!(l.decoded.codec, "unreproducible-wav-dec/1");
    assert!(!l.decoded.reproducible, "再現しない読みの記録");
    let work_path = l
        .work
        .as_ref()
        .expect("作業用のファイルの参照ごと貸す")
        .pcm()
        .path
        .clone();
    let decoded = l.decoded.clone();
    drop(l);
    assert!(anchor_of(&env, &hash).is_none());

    // 1 回目：貸した作業用のファイルをアンカーに昇格する
    env.step(&s, gain(), &Viewport::audio_idle());
    let first = save_and_wait(&env, &s, None);
    assert_eq!(first["anchor"], "created", "{first}");
    assert_eq!(decodes.load(Ordering::SeqCst), 1, "保存は元を読み直さない");
    let a = anchor_of(&env, &hash).expect("保存がアンカーを作った");
    assert_eq!(a.decoded, decoded, "アンカーの記録は貸した入力の記録");
    let work = std::fs::read(&work_path).unwrap();
    assert!(
        std::fs::read(env.lib.layout().content_path(&a.hash, &a.ext)).unwrap() == work,
        "アンカーの中身は作業用のファイルと同じ"
    );
    let got = saved_bytes(&env, id);

    // 2 回目（開いたまま）：昇格をやり直さずにアンカーを使う
    env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -2.0 }))],
        &Viewport::audio_idle(),
    );
    let second = save_and_wait(&env, &s, None);
    assert_eq!(
        second["anchor"], "used",
        "2 回目は前の保存が作ったアンカーを使う：{second}"
    );
    let promoted: Vec<bool> = admits.lock().unwrap().iter().map(|(_, p)| *p).collect();
    assert_eq!(promoted, [true, false], "2 回目は昇格しない");
    assert_eq!(decodes.load(Ordering::SeqCst), 1, "保存は元を読み直さない");

    // 貸さないとき：同じ元・同じ手で、貸し出しを引き上げてから保存する
    let other_decodes = Arc::new(AtomicU64::new(0));
    let other = unreproducible_wav_env(&other_decodes);
    other.jobs.start().unwrap();
    let oid = add_audio(&other.lib, &bytes, "wav");
    let o = other.open(oid);
    other.engine.withdraw_input(&o.session);
    other.step(&o.session, gain(), &Viewport::audio_idle());
    let o1 = save_and_wait(&other, &o.session, None);
    assert_eq!(o1["anchor"], "created", "{o1}");
    assert!(
        got == saved_bytes(&other, oid),
        "貸したときと貸さないときでバイト列が違う"
    );
    assert_eq!(
        anchor_of(&other, &hash).unwrap().hash,
        a.hash,
        "貸さないときも同じアンカー"
    );

    // 閉じると貸し出しが消え、作業用のファイルはほかに使い手がなければ消える
    close(&env.engine, &s);
    assert!(lent(&env, id, &hash).is_none());
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
}

/// S24（再現しない読み手・保存の途中で閉じる。M13 段 4C の検証の直し）：昇格する保存が貸した入力を借りたあと（計画の確保の
/// 直前）にエディタを閉じても、保存が借りている作業用のファイルは保存の間は残り、保存は昇格して書き終える（created。アンカーの
/// 中身は作業用のファイルと同じ）。保存が終われば作業用のファイルは消える。
#[test]
fn s24_closing_the_editor_mid_save_keeps_the_lent_work_file() {
    let bytes = plain_wav_bytes(8_000, LONG);
    let hash = hash_of(&bytes);

    let decodes = Arc::new(AtomicU64::new(0));
    let env = unreproducible_wav_env(&decodes);
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (reached, go) = (Mutex::new(reached_tx), Mutex::new(go_rx));
    let armed = AtomicBool::new(true);
    env.engine.set_hook(Some(Arc::new(move |stage| {
        if matches!(stage, SaveStage::Admit { .. }) && armed.swap(false, Ordering::SeqCst) {
            reached.lock().unwrap().send(()).unwrap();
            let _ = go.lock().unwrap().recv();
        }
    })));
    env.jobs.start().unwrap();
    let id = add_audio(&env.lib, &bytes, "wav");
    let s = env.open(id).session;
    let l = lent(&env, id, &hash).expect("作業用のファイルから開いた入力は貸す");
    let work_path = l.work.as_ref().unwrap().pcm().path.clone();
    drop(l);
    let work_hash = hash_of(&std::fs::read(&work_path).unwrap());

    env.step(&s, gain(), &Viewport::audio_idle());
    let job = save(&env.env(), &s, SaveMode::Overwrite, None).unwrap().job;
    reached_rx
        .recv_timeout(WAIT)
        .expect("保存が貸した入力を借りた");
    close(&env.engine, &s);
    assert!(lent(&env, id, &hash).is_none(), "閉じたら貸し出しが消える");
    assert!(work_path.exists(), "保存が借りている作業用のファイルは残る");
    assert_eq!(env.engine.work_pcm().usage().0, 1);
    go_tx.send(()).unwrap();

    assert!(env.jobs.wait_idle(WAIT));
    let record = env.jobs.get(job).unwrap();
    assert_eq!(record.state, JobState::Done, "{:?}", record.message);
    let result: serde_json::Value =
        serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["anchor"], "created", "{result}");
    assert_eq!(decodes.load(Ordering::SeqCst), 1, "保存は元を読み直さない");
    assert_eq!(
        anchor_of(&env, &hash).unwrap().hash,
        work_hash,
        "アンカーの中身は作業用のファイルと同じ"
    );
    assert_eq!(
        env.engine.work_pcm().usage(),
        (0, 0),
        "保存が終われば作業用のファイルは消える"
    );
    assert!(!work_path.exists());
}
