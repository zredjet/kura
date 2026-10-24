//! 一括の NFR（13：保存、14：一括加工、15：解析）と、その NFR-16 の照らし。どれも一括のプール（`EngineRuntime::bulk`）で
//! 回し、保存は予算の確保（`admit_bulk`。プールの外）を挟む。一括加工（M14 のジョブ）の予算の制御・DB は含めない。

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use kura_core::edit::EngineRuntime;
use kura_engine::codec::CodecSet;
use kura_engine::doc::{DecodeOptions, Graph, InputId, OutputId};
use kura_engine::render::estimate::{EstimateSpec, Sink, Source};
use kura_engine::render::{Engine, InputValue};
use kura_engine::testing::bench::cases::{
    analysis_requests, bulk_doc, bulk_png, save_doc, save_jpeg,
};
use kura_engine::testing::bench::{measure, program_audio, sprite_batch};
use kura_engine::{Digest, EngineError, Limits, NoMonitor};
use rayon::prelude::*;
use serde_json::json;

use super::report::{Record, Verdict};
use super::{Ctx, Photo, clock, load1};

fn hash(bytes: &[u8]) -> Digest {
    Digest(*blake3::hash(bytes).as_bytes())
}

fn at(case: &str) -> impl Fn(EngineError) -> String + '_ {
    move |e| format!("{case}: {e}")
}

/// 保存の 1 回：ファイル `path`（形式 `format`）を読み、元のハッシュを取り、ヘッダの形で計画して予算を確保し（プールの外）、
/// 一括のプールで読む・描く（出力のダイジェスト込み）・JPEG q90 で書き、`out` に書く。書いたバイト列と、確保した見積もりの
/// ピーク（バイト）を返す。予算を超える見込みなら描く前に `Limit`。
pub(crate) fn save_once(
    rt: &EngineRuntime,
    codecs: &CodecSet,
    graph: &Graph,
    path: &Path,
    format: &str,
    out: &Path,
) -> Result<(Vec<u8>, u64), EngineError> {
    let bytes = std::fs::read(path)?;
    let source = hash(&bytes);
    let decode = DecodeOptions::default();
    let (shape, _) = codecs.shape(
        &mut Cursor::new(bytes.as_slice()),
        format,
        &decode,
        &Limits::default(),
    )?;
    let shapes = BTreeMap::from([(InputId(0), shape)]);
    let plan = rt.bulk(|e| e.compile(graph, &shapes))?;
    let opts = save_jpeg();
    let spec = EstimateSpec::new(0)
        .input(InputId(0), Source::Decode { codecs, format })
        .output(
            OutputId(0),
            Sink::Encode {
                codecs,
                format: "jpeg",
                opts: &opts,
            },
        )
        .digests(true);
    let admission = rt.admit_bulk(&plan, spec, &NoMonitor)?;
    let encoded = rt.bulk(|e| -> Result<Vec<u8>, EngineError> {
        let (value, _) = codecs.decode(
            &mut Cursor::new(bytes.as_slice()),
            format,
            &decode,
            e.limits(),
            &NoMonitor,
        )?;
        let inputs = BTreeMap::from([(InputId(0), InputValue { value, source })]);
        let rendered = e.render(&plan, &inputs, &NoMonitor)?;
        let first = rendered
            .outputs
            .first()
            .ok_or_else(|| EngineError::Internal("出力がありません".into()))?;
        Ok(codecs
            .encode_bytes(&first.value, "jpeg", &opts, &NoMonitor)?
            .0)
    })?;
    let estimate = admission.estimate.peak;
    drop(admission);
    std::fs::write(out, &encoded)?;
    Ok((encoded, estimate))
}

/// 別のエンジン・`threads` スレッドのプールで、キャッシュなしに作り直した保存のバイト列（NFR-16）。
fn save_fresh(
    codecs: &CodecSet,
    graph: &Graph,
    photo: &Photo,
    threads: usize,
) -> Result<Vec<u8>, String> {
    let engine = Engine::new().map_err(|e| e.to_string())?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&photo.path).map_err(|e| e.to_string())?;
    pool.install(|| -> Result<Vec<u8>, EngineError> {
        let (value, _) = codecs.decode(
            &mut Cursor::new(bytes.as_slice()),
            photo.format,
            &DecodeOptions::default(),
            engine.limits(),
            &NoMonitor,
        )?;
        let shapes = BTreeMap::from([(InputId(0), value.shape())]);
        let plan = engine.compile(graph, &shapes)?;
        let inputs = BTreeMap::from([(
            InputId(0),
            InputValue {
                value,
                source: hash(&bytes),
            },
        )]);
        let r = engine.render(&plan, &inputs, &NoMonitor)?;
        let first = r
            .outputs
            .first()
            .ok_or_else(|| EngineError::Internal("出力がありません".into()))?;
        Ok(codecs
            .encode_bytes(&first.value, "jpeg", &save_jpeg(), &NoMonitor)?
            .0)
    })
    .map_err(|e| e.to_string())
}

/// NFR-16 のレコード（結果のダイジェストを、別のエンジンで作り直したものと比べる）。
fn match_record(
    ctx: &mut Ctx,
    case: &str,
    item: &str,
    threads: usize,
    mine: &Digest,
    fresh: &[(usize, Digest)],
) {
    let bad: Vec<String> = fresh
        .iter()
        .filter(|(_, d)| d != mine)
        .map(|(t, _)| format!("{t} スレッドで作り直した結果が違う"))
        .collect();
    let list: Vec<String> = fresh.iter().map(|(t, _)| t.to_string()).collect();
    let r = Record::check(
        ctx.run,
        16,
        "determinism",
        case,
        item,
        threads,
        if bad.is_empty() {
            Verdict::Pass
        } else {
            Verdict::Fail
        },
        bad.join("・"),
        load1(),
        json!({
            "paths": format!("一括（{threads} スレッド）＝別のエンジン（{} スレッド）", list.join("・")),
            "digest": mine.to_hex(),
        }),
    );
    ctx.push(r);
}

/// NFR-13：24MP の JPEG → 線形光で 50%（Lanczos3 で判定、bilinear も記録）→ JPEG q90。合成と `--photo` の実写真。
pub(super) fn save(ctx: &mut Ctx, timed: bool, check: bool) -> Result<(), String> {
    let mut photos = vec![ctx.synthetic()?.clone()];
    if let Some(real) = ctx.real() {
        photos.push(real.clone());
    }
    let z = ctx.sizes.clone();
    for threads in ctx.bulk_set() {
        let rt = ctx.runtime(threads)?;
        let judged = threads == ctx.opts.bulk_threads;
        for photo in &photos {
            for (filter, reference) in [("lanczos3", false), ("bilinear", true)] {
                let case = format!("save-{filter}{}", photo.suffix);
                let graph = save_doc(filter).map_err(|e| e.to_string())?;
                let out = ctx.tmp.join(format!("{case}-{threads}.jpg"));
                let load = load1();
                let (warmup, n) = if timed {
                    (z.save_warmup, z.save_n)
                } else {
                    (0, 1)
                };
                let c = clock();
                let mut last = Vec::new();
                let codecs = &ctx.codecs;
                let stats = measure(&c, warmup, n, |_| {
                    last = save_once(&rt, codecs, &graph, &photo.path, photo.format, &out)?.0;
                    Ok(())
                })
                .map_err(at(&case))?;
                if timed {
                    let mut r = Record::timed(
                        ctx.run,
                        13,
                        "save",
                        &case,
                        &format!(
                            "保存：JPEG → 線形光で 50%（{filter}）→ JPEG q90（{}）",
                            photo.label
                        ),
                        threads,
                        stats,
                        judged,
                        load,
                    )
                    .with_note(
                        "ファイルの読み書き・元のハッシュ・予算の確保・出力のダイジェストを含む",
                    );
                    if reference {
                        r = r.into_reference();
                    }
                    ctx.push(r);
                }
                if check && judged && !reference {
                    let mut fresh = Vec::new();
                    for t in ctx.check_threads() {
                        fresh.push((t, hash(&save_fresh(&ctx.codecs, &graph, photo, t)?)));
                    }
                    match_record(
                        ctx,
                        &case,
                        "保存の JPEG のバイト列",
                        threads,
                        &hash(&last),
                        &fresh,
                    );
                }
            }
        }
    }
    Ok(())
}

/// 一括の 1 件：PNG を読み、計画して描き（出力のダイジェスト込み）、PNG（既定の圧縮）で書く。書いたバイト列のハッシュを返す。
fn bulk_item(
    e: &Engine,
    codecs: &CodecSet,
    graph: &Graph,
    bytes: &[u8],
) -> Result<Digest, EngineError> {
    let (value, _) = codecs.decode(
        &mut Cursor::new(bytes),
        "png",
        &DecodeOptions::default(),
        e.limits(),
        &NoMonitor,
    )?;
    let shapes = BTreeMap::from([(InputId(0), value.shape())]);
    let plan = e.compile(graph, &shapes)?;
    let inputs = BTreeMap::from([(
        InputId(0),
        InputValue {
            value,
            source: hash(bytes),
        },
    )]);
    let r = e.render(&plan, &inputs, &NoMonitor)?;
    let first = r
        .outputs
        .first()
        .ok_or_else(|| EngineError::Internal("出力がありません".into()))?;
    let (out, _) = codecs.encode_bytes(&first.value, "png", &bulk_png(), &NoMonitor)?;
    Ok(hash(&out))
}

/// 件の結果のハッシュの列をまとめたハッシュ。
fn batch_digest(items: &[Digest]) -> Digest {
    let mut h = blake3::Hasher::new();
    for d in items {
        h.update(&d.0);
    }
    Digest(*h.finalize().as_bytes())
}

/// NFR-14：ドット絵の PNG 1,000 件（`testing::bench::sprite_batch`）のトリム → 最近傍 ×4。件を一括のプールで並べ、読み書きは
/// メモリの中（入力の PNG は先に作っておく。作る時間は含めない）。
pub(super) fn bulk(ctx: &mut Ctx, timed: bool, check: bool) -> Result<(), String> {
    let z = ctx.sizes.clone();
    let items: Vec<Vec<u8>> = sprite_batch(z.bulk_items, 1)
        .iter()
        .map(|g| {
            let v = g.generate()?;
            Ok(ctx
                .codecs
                .encode_bytes(&v, "png", &bulk_png(), &NoMonitor)?
                .0)
        })
        .collect::<Result<_, EngineError>>()
        .map_err(|e| e.to_string())?;
    let graph = bulk_doc().map_err(|e| e.to_string())?;
    for threads in ctx.bulk_set() {
        let rt = ctx.runtime(threads)?;
        let judged = threads == ctx.opts.bulk_threads;
        let load = load1();
        let (warmup, n) = if timed {
            (z.bulk_warmup, z.bulk_n)
        } else {
            (0, 1)
        };
        let c = clock();
        let mut last = Vec::new();
        let codecs = &ctx.codecs;
        let stats = measure(&c, warmup, n, |_| {
            last = rt.bulk(|e| {
                items
                    .par_iter()
                    .map(|b| bulk_item(e, codecs, &graph, b))
                    .collect::<Result<Vec<_>, _>>()
            })?;
            Ok(())
        })
        .map_err(at("bulk"))?;
        if timed {
            ctx.push(
                Record::timed(
                    ctx.run,
                    14,
                    "bulk",
                    "bulk",
                    &format!(
                        "一括：ドット絵の PNG {} 件（トリム → 最近傍 ×4、メモリの中で読み書き）",
                        z.bulk_items
                    ),
                    threads,
                    stats,
                    judged,
                    load,
                )
                .with_note("各回の通しの時間。判定は最大"),
            );
        }
        if check && judged {
            let mine = batch_digest(&last);
            let mut fresh = Vec::new();
            for t in ctx.check_threads() {
                let engine = Engine::new().map_err(|e| e.to_string())?;
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(t)
                    .build()
                    .map_err(|e| e.to_string())?;
                let codecs = &ctx.codecs;
                let got = pool
                    .install(|| {
                        items
                            .par_iter()
                            .map(|b| bulk_item(&engine, codecs, &graph, b))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .map_err(at("bulk"))?;
                fresh.push((t, batch_digest(&got)));
            }
            match_record(
                ctx,
                "bulk",
                "一括の PNG のバイト列（全件）",
                threads,
                &mine,
                &fresh,
            );
        }
    }
    Ok(())
}

/// 解析の 1 回：読んで、波形とラウドネス（トゥルーピーク）を解く。結果の正規の形を返す。
fn analyze_once(
    e: &Engine,
    codecs: &CodecSet,
    format: &str,
    bytes: &[u8],
) -> Result<Vec<Vec<u8>>, EngineError> {
    let (v, _) = codecs.decode(
        &mut Cursor::new(bytes),
        format,
        &DecodeOptions::default(),
        e.limits(),
        &NoMonitor,
    )?;
    e.analyze_many(&v, &analysis_requests(), &NoMonitor)
        .into_iter()
        .map(|r| r.map(|a| a.output))
        .collect()
}

/// NFR-15：5 分の曲（`testing::bench::program_audio`）の解析。WAV と FLAC（16 ビット）の読み取りを含む。
pub(super) fn analysis(ctx: &mut Ctx, timed: bool, check: bool) -> Result<(), String> {
    let z = ctx.sizes.clone();
    let audio = program_audio(z.analysis_rate, z.analysis_secs).map_err(|e| e.to_string())?;
    for format in ["wav", "flac"] {
        let (bytes, _) = ctx
            .codecs
            .encode_bytes(&audio, format, &json!({ "bits": 16 }), &NoMonitor)
            .map_err(|e| e.to_string())?;
        let case = format!("analysis-{format}");
        for threads in ctx.bulk_set() {
            let rt = ctx.runtime(threads)?;
            let judged = threads == ctx.opts.bulk_threads;
            let load = load1();
            let (warmup, n) = if timed {
                (z.analysis_warmup, z.analysis_n)
            } else {
                (0, 1)
            };
            let c = clock();
            let mut last = Vec::new();
            let codecs = &ctx.codecs;
            let stats = measure(&c, warmup, n, |_| {
                last = rt.bulk(|e| analyze_once(e, codecs, format, &bytes))?;
                Ok(())
            })
            .map_err(at(&case))?;
            if timed {
                ctx.push(Record::timed(
                    ctx.run,
                    15,
                    "analysis",
                    &case,
                    &format!(
                        "{} 秒の {} Hz ステレオ（{}・16 ビット）の読み取り ＋ 波形・ラウドネス（トゥルーピーク）",
                        z.analysis_secs,
                        z.analysis_rate,
                        format.to_uppercase()
                    ),
                    threads,
                    stats,
                    judged,
                    load,
                ));
            }
            if check && judged {
                let mine = batch_digest(&last.iter().map(|b| hash(b)).collect::<Vec<_>>());
                let mut fresh = Vec::new();
                for t in ctx.check_threads() {
                    let engine = Engine::new().map_err(|e| e.to_string())?;
                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(t)
                        .build()
                        .map_err(|e| e.to_string())?;
                    let got = pool
                        .install(|| analyze_once(&engine, &ctx.codecs, format, &bytes))
                        .map_err(at(&case))?;
                    fresh.push((
                        t,
                        batch_digest(&got.iter().map(|b| hash(b)).collect::<Vec<_>>()),
                    ));
                }
                match_record(ctx, &case, "解析の結果の正規の形", threads, &mine, &fresh);
            }
        }
    }
    Ok(())
}
