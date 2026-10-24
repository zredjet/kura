//! 版に戻す（元に戻す・途中の版 k に戻す。docs/editing.md §14「版に戻す」、docs/data-model.md §4「保存と版（M12）」）。
//!
//! 版 n+1 を `kind = 'revert'`・`revert_to = k`・`base_version = n` で足す。文書は版 k の文書（版 0 と、版 0 に戻した版は NULL）、
//! 入力は版 k の写し、値のダイジェストは版 k と同じ。アイテムは版 k の列（版 0 なら版 0 の列と記録の mime・codec_info）に
//! 戻す。title・original_name・素材の設定は変えない。戻した結果が今と同じ中身なら版だけ足す（unchanged）。前の頭は上書きと
//! 同じく waiting（出力が戻した中身か版 0 と同じなら released）。版 k の状態は変えない。
//!
//! - 使い直す：版 k の中身が台帳にあり（版 k がまだ持っているか、ほかの版・アイテムが同じ中身を持っている）、ファイルがあれば、
//!   台帳の門を持って 1 つのトランザクションで戻す（描かない・置かない）。版 0 はいつも hold なので、元に戻すのはいつもこちら。
//! - 作り直す：なければ、版 k の入力の記録で元を読み（アンカーがあればアンカー。使えなければ、元を記録のデコーダで読めれば元）、
//!   版 k の文書を描いて値のダイジェストを記録と照らし（違えば Mismatch で何も変えない。M12 の段 2 で推奨で採った決定）、記録の
//!   エンコーダ（この環境になければ今の組の同じ形式のもの。値は照らし済みなので、新しい書き出しの記録を残す）で書く。出力は
//!   作り直したファイル H′ で、予告 → 配置 → コミットは保存と同じ段 6〜9。

use super::*;
use crate::edit::versions::{RevertRecord, Version, VersionInput};

/// 版に戻す先の確かめ：0 以上で、頭の版 `head` より前（頭の版そのものには戻さない）。
pub(super) fn check_target(to: i64, head: i64) -> Result<()> {
    if head == 0 {
        return Err(CoreError::Invalid(
            "まだ保存していないアイテムには、戻す版がありません".into(),
        ));
    }
    if to < 0 || to >= head {
        return Err(CoreError::Invalid(format!(
            "戻す先の版 {to} がありません（戻せるのは 0〜{}）",
            head - 1
        )));
    }
    Ok(())
}

/// 戻す先の版。
#[derive(Debug, Clone)]
pub(super) struct Target {
    /// 戻す先の版の番号 k。
    pub(super) to: i64,
    /// 版 k の行。
    pub(super) version: Version,
    /// 版 k の入力（戻した版に写す）。
    pub(super) inputs: Vec<VersionInput>,
}

/// 戻した版のコミットに渡すもの（使い直したか作り直したか）。
#[derive(Debug, Clone)]
pub(super) struct Out {
    job: i64,
    item_id: i64,
    base: Base,
    to: i64,
    doc_id: Option<i64>,
    inputs: Vec<VersionInput>,
    hash: [u8; 32],
    ext: String,
    size: i64,
    format: Option<String>,
    value_digest: Option<Digest>,
    media: Media,
    /// アイテムに戻す mime・codec_info（版の記録にも残す）。
    media_record: MediaRecord,
    encoded: Option<EncodedStamp>,
    decoded: Option<DecodeStamp>,
    warnings: Vec<String>,
    /// ファイルを使い直した（描いていない）。偽なら作り直した。
    reused_file: bool,
    /// 出力が台帳にあった（ファイルを共有する。結果の `reused`）。
    shared: bool,
    /// 作り直しでアンカーから読んだ（結果の `anchor`）。
    anchor: Option<&'static str>,
}

impl Out {
    /// 使い直す：版 k の中身が台帳に拡張子 `ext` で載っていて、ファイルがある。
    fn reused(job: i64, item_id: i64, base: Base, target: &Target, ext: String) -> Result<Self> {
        let v = &target.version;
        let record = v.record()?;
        Ok(Self {
            job,
            item_id,
            base,
            to: target.to,
            doc_id: v.doc_id,
            inputs: target.inputs.clone(),
            hash: v.output_hash,
            media_record: media_record(&record, v, &ext, true),
            ext,
            size: v.size_bytes,
            format: v.format.clone(),
            value_digest: v.value_digest,
            media: Media {
                width: v.width,
                height: v.height,
                duration_ms: v.duration_ms,
            },
            encoded: record.encoded,
            decoded: record.decoded,
            warnings: record.warnings,
            reused_file: true,
            shared: true,
            anchor: None,
        })
    }

    /// 作り直した（段 1〜5 の `p`、予告で決めた `go`）。
    pub(super) fn rebuilt(p: &Prepared, target: &Target, go: &Go) -> Result<Self> {
        let v = &target.version;
        let record = v.record()?;
        let size = i64::try_from(p.out_size)
            .map_err(|_| CoreError::TooLarge(format!("出力が大きすぎます: {}", p.out_size)))?;
        Ok(Self {
            job: p.job,
            item_id: p.item_id,
            base: p.base,
            to: target.to,
            doc_id: v.doc_id,
            inputs: target.inputs.clone(),
            hash: p.out_hash,
            ext: go.ext.clone(),
            size,
            format: Some(p.out_format.clone()),
            value_digest: Some(p.value_digest),
            media: p.media,
            media_record: media_record(&record, v, &go.ext, p.out_hash == v.output_hash),
            encoded: Some(p.encoded.clone()),
            decoded: record.decoded,
            warnings: p.warnings.clone(),
            reused_file: false,
            shared: go.reused,
            anchor: p.anchor.label(),
        })
    }
}

/// 戻した版のアイテムの mime・codec_info：同じ中身（`same`）なら版 k の記録の値（拡張子が版 k と違えば mime は拡張子から）、
/// 作り直して中身が変わったら mime は拡張子から・codec_info は空（サムネイル・プロキシのジョブが読み直す）。
fn media_record(record: &VersionRecord, v: &Version, ext: &str, same: bool) -> MediaRecord {
    let recorded = record.media.clone().unwrap_or_default();
    let from_ext = || mime_for(ext).map(str::to_string);
    MediaRecord {
        mime: if same && ext == v.ext {
            recorded.mime.or_else(from_ext)
        } else {
            from_ext()
        },
        codec_info: if same { recorded.codec_info } else { None },
    }
}

impl EditHandler {
    /// 版 `to` に戻す（ジョブの本体）。使い直せれば門を持って 1 つのトランザクションで、なければ作り直して段 6〜9 へ。
    pub(super) fn revert(
        &self,
        ctx: &JobContext,
        item_id: i64,
        base: Base,
        to: i64,
    ) -> Result<JobEnd> {
        let library = ctx.library();
        let db = library.db();
        let layout = library.layout();
        let job = ctx.id();
        let head = db
            .read(|c| versions::head(c, item_id))?
            .ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id}")))?;
        if let Some(why) = base_conflict(&head, &base) {
            return Err(CoreError::Conflict(why));
        }
        check_target(to, head.version)?;
        let target = db.read(|c| {
            let version = versions::get(c, item_id, to)?
                .ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id} の版 {to}")))?;
            let inputs = versions::inputs(c, version.id)?;
            Ok(Target {
                to,
                version,
                inputs,
            })
        })?;
        ctx.progress(0.05, "戻す版のファイルを探しています");
        {
            // 使い直す：門を持って、台帳の行とファイルを確かめてからコミットする（その間に消されない）
            let gate = library.ingest_exclusive();
            let hash = target.version.output_hash;
            let listed = db.read(|c| ledger::ext_of(c, &hash))?;
            if let Some(ext) = listed.filter(|ext| layout.content_path(&hash, ext).is_file()) {
                let out = Out::reused(job, item_id, base, &target, ext)?;
                let outcome = db.write(move |tx| commit_revert(tx, &out, now_ms()))?;
                drop(gate);
                queue_follow_ups(ctx, head.kind);
                let message = if outcome["outcome"] == "unchanged" {
                    format!("版 {to} は今と同じ中身なので、版だけ足しました")
                } else {
                    format!("版 {to} に戻しました")
                };
                ctx.log(&message);
                return Ok(JobEnd::Done {
                    result_json: outcome.to_string(),
                    message: Some(message),
                });
            }
        }
        ctx.log(&format!(
            "版 {to} のファイルがないので、版の文書と入力の記録から作り直します"
        ));
        let (prepared, staging) =
            match self.prepare_rebuild(ctx, item_id, base, head.kind, target)? {
                Flow::Go(p) => p,
                Flow::Stop(end) => return Ok(end),
            };
        self.commit(ctx, prepared, staging)
    }

    /// 作り直しの段 1〜5：版 k の文書を読み、入力の記録で元を読み（アンカー → 元）、描いて値のダイジェストを記録と照らし、
    /// 記録のエンコーダで書いて staging に置く。
    fn prepare_rebuild(
        &self,
        ctx: &JobContext,
        item_id: i64,
        base: Base,
        kind: MediaKind,
        target: Target,
    ) -> Result<Flow<(Prepared, Staging)>> {
        let library = ctx.library();
        let db = library.db();
        let layout = library.layout();
        let job = ctx.id();
        let k = target.to;
        let v = &target.version;
        let doc_id = v.doc_id.ok_or_else(|| {
            CoreError::NotFound(format!(
                "版 {k} のファイル（元）がなく、作り直す文書もありません"
            ))
        })?;
        let want = v.value_digest.ok_or_else(|| {
            CoreError::Mismatch(format!(
                "版 {k} に値のダイジェストの記録がないので、作り直したものを照らせません"
            ))
        })?;
        let input = match target.inputs.as_slice() {
            [i] if i.input == 0 && i.role == InputRole::Primary => i.clone(),
            _ => {
                return Err(CoreError::Invalid(format!(
                    "版 {k} の入力の記録が、元 1 つの形ではありません"
                )));
            }
        };
        let rec: RecordedInput = serde_json::from_str(&input.recorded)
            .map_err(|e| CoreError::Invalid(format!("版 {k} の入力の記録を読めません: {e}")))?;
        let record = v.record()?;
        let bytes = db.read(|c| store::load_doc_bytes(c, doc_id))?;
        let runtime = self.engine.runtime()?.clone();
        let codecs = self.engine.codecs();
        let graph = runtime
            .bulk(|e| e.load(&bytes))
            .map_err(|e| from_engine("戻す版の文書を読めません", e))?;
        drop(bytes);
        check_doc(&graph)?;
        let original = db
            .read(|c| versions::original(c, item_id))?
            .ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id}")))?;
        if rec.source.0 != original.hash {
            return Err(CoreError::Invalid(format!(
                "版 {k} の入力の元が、アイテムの元と違います"
            )));
        }
        let original_path = layout.content_path(&original.hash, &original.ext);
        let anchor = match input.anchor_id {
            Some(id) => db.read(|c| anchors::get(c, id))?,
            None => None,
        };
        let limits: Limits = runtime.bulk(|e| e.limits().clone());
        let mon = JobMonitor(ctx);
        let mut warnings: Vec<String> = Vec::new();
        // 書き手：記録のエンコーダ（なければ今の組の同じ形式のもの）
        let (out_format, enc_opts, recorded_encoder) = match &record.encoded {
            Some(s) => (s.format.clone(), s.options.clone(), Some(s.codec.clone())),
            None => (
                v.format.clone().ok_or_else(|| {
                    CoreError::Invalid(format!("版 {k} に形式の記録がありません"))
                })?,
                json!({}),
                None,
            ),
        };
        let desc = format_by_id(&out_format).ok_or_else(|| {
            CoreError::Unsupported(format!("版 {k} の形式 {out_format} を知りません"))
        })?;
        if MediaKind::from(desc.item_kind) != kind {
            return Err(CoreError::Invalid(format!(
                "版 {k} の形式 {out_format} は、アイテムの種類（{}）と違います",
                kind.as_str()
            )));
        }

        // 1・2. 読む元ごとに：形 → 計画 → 予算 → 読む（アンカーが使えなければ、元を記録のデコーダで読み直す。read.rs）
        ctx.progress(0.1, "戻す版の元を読んでいます");
        let cx = read::ReadCx {
            ctx,
            runtime: &runtime,
            codecs: &codecs,
            layout,
            original: &original_path,
            limits: &limits,
            mon: &mon,
        };
        let plan_for = |how: &read::Reading<'_>, shape: &Shape| -> Result<Flow<_>> {
            let shapes = BTreeMap::from([(InputId(0), *shape)]);
            let plan = runtime
                .bulk(|e| e.compile(&graph, &shapes))
                .map_err(|issues| {
                    CoreError::Invalid(format!("版 {k} の文書を描けません：{issues}"))
                })?;
            let out_shape = plan
                .output_shapes()
                .first()
                .map(|(_, s)| *s)
                .ok_or_else(|| CoreError::Invalid("文書に出力がありません".into()))?;
            let encoder = recorded_encoder
                .as_deref()
                .and_then(|id| codecs.encoder_by_id(id))
                .filter(|e| e.can_encode(&out_format, &out_shape))
                .cloned();
            if encoder.is_none() && codecs.encoder(&out_format, &out_shape).is_none() {
                return Err(CoreError::Unsupported(format!(
                    "版 {k} の結果を {out_format} に書けません"
                )));
            }
            let sink = match &encoder {
                Some(e) => Sink::Encoder {
                    encoder: e.as_ref(),
                    format: &out_format,
                    opts: &enc_opts,
                },
                None => Sink::Encode {
                    codecs: &codecs,
                    format: &out_format,
                    opts: &enc_opts,
                },
            };
            let spec = EstimateSpec::new(0)
                .digests(true)
                .input(InputId(0), how.source(&codecs))
                .input_len(InputId(0), file_len(how.path(&original_path)))
                .output(OutputId(0), sink);
            let (_estimate, lease) = go!(
                ctx,
                "作り直しのメモリを確保できません",
                runtime.admit_bulk_with(&plan, spec, |_| 0, &mon)
            );
            Ok(Flow::Go((plan, lease, encoder)))
        };
        let read = read::read_recorded_input(
            &cx,
            &rec,
            anchor,
            &format!("版 {k} を作り直せません"),
            &mut warnings,
            plan_for,
        )?;
        let (value, _, anchor_use, (plan, lease, encoder)) = match read {
            Flow::Go(read) => read,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        let input = InputValue {
            value,
            source: rec.source,
        };

        // 4. 描いて照らす
        ctx.progress(0.5, "描き直しています");
        let inputs = BTreeMap::from([(InputId(0), input)]);
        let rendered = go!(
            ctx,
            "描けません",
            runtime.bulk(|e| e.render(&plan, &inputs, &mon))
        );
        drop(inputs);
        warnings.extend(rendered.warnings.iter().map(|w| w.message.clone()));
        let out = rendered
            .outputs
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::Invalid("描いた出力がありません".into()))?;
        if out.digest != want {
            return Err(CoreError::Mismatch(format!(
                "版 {k} を作り直した値が記録と違うので、戻しません（記録 {want}、作り直し {}）",
                out.digest
            )));
        }

        // 5. 書く
        ctx.progress(0.75, "書いています");
        let encoded = match &encoder {
            Some(e) => runtime.bulk(|_| {
                codecs.encode_bytes_with(e.id(), &out.value, &out_format, &enc_opts, &mon)
            }),
            None => runtime.bulk(|_| codecs.encode_bytes(&out.value, &out_format, &enc_opts, &mon)),
        };
        let (bytes, encoded) = go!(ctx, "書けません", encoded);
        if encoder.is_none() {
            warnings.push(format!(
                "記録のエンコーダ {} がないので、{} で書きました（値は記録と同じです）",
                recorded_encoder.as_deref().unwrap_or("（記録なし）"),
                encoded.codec
            ));
        }
        let media = media_of(&out.value.shape());
        let value_digest = out.digest;
        drop(out);
        drop(lease);
        let out_hash = *blake3::hash(&bytes).as_bytes();
        if out_hash != v.output_hash {
            warnings.push(format!(
                "作り直したファイルのバイト列は、版 {k} のものと違います（値は同じです）"
            ));
        }
        let out_size = bytes.len() as u64;
        let mut staging = Staging::default();
        std::fs::create_dir_all(layout.staging_dir())?;
        let ext0 = desc.exts.first().copied().unwrap_or("bin");
        let out_part = layout
            .staging_dir()
            .join(format!("edit-{job}-out.{ext0}.part"));
        staging.add(out_part.clone());
        write_synced(&out_part, &bytes)?;
        drop(bytes);
        if let Some(stop) = self.after_written(ctx, &mut staging) {
            return stop;
        }
        let ext_hint = v.ext.clone();
        Ok(Flow::Go((
            Prepared {
                job,
                mode: Mode::Revert,
                item_id,
                base,
                act: Act::Revert(Box::new(target)),
                kind,
                ext_hint,
                original,
                source_format: rec.format,
                opts: rec.options,
                decoded: rec.decoded,
                anchor: anchor_use,
                out_part,
                out_hash,
                out_size,
                out_format,
                value_digest,
                media,
                encoded,
                warnings,
            },
            staging,
        )))
    }
}

/// トランザクション 2（版に戻す）：土台を確かめ直し、別のアイテムの今と同じ中身でないことを確かめ、アイテムを版 k の列に戻し
/// （同じ中身なら日時だけ）、前の頭を手放し、戻した版と写した入力を書き、下書きを片付け、自分の予告を消して結果を書く。
pub(super) fn commit_revert(tx: &Connection, o: &Out, now: i64) -> Result<serde_json::Value> {
    let id = o.item_id;
    let head =
        versions::head(tx, id)?.ok_or_else(|| CoreError::NotFound(format!("アイテム {id}")))?;
    if let Some(why) = base_conflict(&head, &o.base) {
        return Err(CoreError::Conflict(why));
    }
    check_target(o.to, head.version)?;
    let other: Option<i64> = tx
        .prepare_cached("SELECT id FROM items WHERE hash = ?1 AND id <> ?2")?
        .query_row(params![&o.hash[..], id], |r| r.get(0))
        .optional()?;
    if let Some(other) = other {
        return Err(CoreError::Conflict(format!(
            "戻した結果が、別のアイテム（{other}）の今と同じ中身になります"
        )));
    }
    // 戻す中身の台帳の行（使い直すなら、もうある。作り直したものはここで載せる。items のトリガと同じ食い違いの確かめ）
    let size = u64::try_from(o.size)
        .map_err(|_| CoreError::Invalid(format!("出力の大きさが負です: {}", o.size)))?;
    let blob = ledger::register(tx, &o.hash, &o.ext, size, now)?;
    let unchanged = head.hash == o.hash;
    if unchanged {
        tx.prepare_cached("UPDATE items SET updated_at = ?2 WHERE id = ?1")?
            .execute(params![id, now])?;
    } else {
        let n = tx
            .prepare_cached(
                "UPDATE items SET hash = ?2, ext = ?3, size_bytes = ?4, mime = ?5, width = ?6, height = ?7,
                        duration_ms = ?8, codec_info = ?9, thumb_state = 'pending',
                        proxy_state = CASE WHEN kind IN ('video', 'audio') THEN 'unknown' ELSE 'not_needed' END,
                        analysis_state = CASE WHEN kind IN ('image', 'audio') THEN 'pending' ELSE 'not_needed' END,
                        updated_at = ?10
                  WHERE id = ?1 AND hash = ?11 AND trashed_at IS NULL",
            )?
            .execute(params![
                id,
                &o.hash[..],
                o.ext,
                o.size,
                o.media_record.mime,
                o.media.width,
                o.media.height,
                o.media.duration_ms,
                o.media_record.codec_info,
                now,
                &head.hash[..],
            ])?;
        if n != 1 {
            return Err(CoreError::Conflict(
                "戻す間に、アイテムが変わりました".into(),
            ));
        }
    }
    let original = versions::get(tx, id, 0)?
        .ok_or_else(|| CoreError::NotFound(format!("アイテム {id} の版 0")))?;
    versions::supersede_head(tx, id, head.version, &o.hash, &original.output_hash, now)?;
    let version = head.version + 1;
    let rebuilt = !o.reused_file;
    let record = VersionRecord {
        encoded: o.encoded.clone(),
        decoded: o.decoded.clone(),
        media: Some(o.media_record.clone()),
        warnings: o.warnings.clone(),
        revert: Some(RevertRecord {
            to: o.to,
            reused: o.reused_file,
            rebuilt,
        }),
        ..VersionRecord::new()
    };
    let vid = versions::insert_version(
        tx,
        &NewVersion {
            item_id: id,
            version,
            kind: VersionKind::Revert,
            doc_id: o.doc_id,
            base_version: Some(head.version),
            revert_to: Some(o.to),
            output_hash: o.hash,
            ext: &o.ext,
            size_bytes: o.size,
            format: o.format.as_deref(),
            value_digest: o.value_digest,
            width: o.media.width,
            height: o.media.height,
            duration_ms: o.media.duration_ms,
            record: &record,
            file_blob_id: Some(blob),
            file_state: FileState::Hold,
            replaced_at: None,
            job_id: Some(o.job),
            now,
        },
    )?;
    let inputs: Vec<NewInput<'_>> = o
        .inputs
        .iter()
        .map(|i| NewInput {
            input: i.input,
            role: i.role,
            source_hash: i.source_hash,
            value_digest: i.value_digest,
            recorded: &i.recorded,
            anchor_id: i.anchor_id,
        })
        .collect();
    versions::insert_inputs(tx, vid, &inputs)?;
    let saved_doc = match o.doc_id {
        Some(doc) => Some(doc),
        None => drafts::empty_doc_id(tx)?,
    };
    drafts::settle_after_save(
        tx,
        id,
        saved_doc,
        o.job,
        Base {
            hash: o.hash,
            version,
        },
    )?;
    ledger::clear_announced(tx, o.job)?;
    let outcome = json!({
        "outcome": if unchanged { "unchanged" } else { "saved" },
        "itemId": id,
        "version": version,
        "versionId": vid,
        "hash": hex(&o.hash),
        "ext": o.ext,
        "reused": o.shared,
        "anchor": o.anchor,
        "warnings": o.warnings,
        "revert": { "to": o.to, "reused": o.reused_file, "rebuilt": rebuilt },
    });
    save_partial(
        tx,
        o.job,
        &json!({ "v": 1, "committed": outcome }).to_string(),
        1.0,
        "保存しました",
    )?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_before_the_head() {
        assert!(matches!(check_target(0, 0), Err(CoreError::Invalid(_))));
        assert!(check_target(0, 1).is_ok());
        assert!(check_target(2, 3).is_ok());
        for (to, head) in [(3, 3), (4, 3), (-1, 3)] {
            assert!(
                matches!(check_target(to, head), Err(CoreError::Invalid(_))),
                "{to} {head}"
            );
        }
    }

    /// 戻した版のアイテムの mime・codec_info：同じ中身なら版の記録、拡張子が違えば mime は拡張子、中身が違えば codec_info は空。
    #[test]
    fn media_record_keeps_the_recorded_values_only_for_the_same_contents() {
        let v = Version {
            id: 1,
            item_id: 1,
            version: 0,
            kind: VersionKind::Original,
            doc_id: None,
            base_version: None,
            revert_to: None,
            output_hash: [1; 32],
            ext: "heic".into(),
            size_bytes: 1,
            format: Some("heic".into()),
            value_digest: None,
            width: None,
            height: None,
            duration_ms: None,
            record: String::new(),
            file_blob_id: None,
            file_state: FileState::Hold,
            replaced_at: None,
            job_id: None,
            created_at: 1,
        };
        let record = VersionRecord {
            media: Some(MediaRecord {
                mime: Some("image/heic-x".into()),
                codec_info: Some("{}".into()),
            }),
            ..VersionRecord::new()
        };
        assert_eq!(
            media_record(&record, &v, "heic", true),
            MediaRecord {
                mime: Some("image/heic-x".into()),
                codec_info: Some("{}".into())
            }
        );
        let other_ext = media_record(&record, &v, "png", true);
        assert_eq!(other_ext.mime.as_deref(), Some("image/png"));
        assert_eq!(other_ext.codec_info.as_deref(), Some("{}"));
        assert_eq!(
            media_record(&record, &v, "png", false),
            MediaRecord {
                mime: Some("image/png".into()),
                codec_info: None
            }
        );
    }
}
