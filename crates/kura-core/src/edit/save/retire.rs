//! 退役する読み手の記録の入力を、その読み手で元から読み直してアンカーにする 1 件（M16 段 5。移し替えのジョブ
//! `edit::retire` の部品。docs/editing.md §9.4「退役する読み手の移し替え」）。
//!
//! 1. 読む（[`promote_recorded`]）：元を記録の識別子のデコーダで作業用の PCM（封をした f32 WAV。アンカーと同じバイト列）へ流して
//!    読み（版に戻す作り直しと同じ `stream::read_input` の記録の読み）、値のダイジェストが記録と同じなら、作業用のファイルを
//!    staging に写して照らす（`stream::promote`。値を丸ごと持たない）。封の読み取りの記録が版の記録と同じでなければ移さない
//!    （アンカーから読むときの照らし `anchor::verify_stream` は記録の全体を比べる）。
//! 2. 載せる（[`commit_retired`]）：保存のジョブと同じ手順で、台帳の門を持って予告をコミット → `files/` へ置く → 1 つのトランザクションで
//!    台帳とアンカーの行（鍵は `anchors::retired_key`）に載せ、同じアイテムの記録の同じ入力に `anchor_id` と記録の `anchor` を付け、
//!    予告を消す（呼び手の途中経過も同じトランザクションで書く）。途中で落ちたら、開くときの `ledger::settle_pending` が片付ける。

use kura_engine::budget::Class;

use super::*;

/// 読んだ結果。
pub(crate) enum Promote {
    /// staging に書いて照らしたアンカー（[`commit_retired`] に渡す。落とすと書きかけを消す）。
    Made(MadeAnchor),
    /// 元を読み直した値・記録が版の記録と違う（理由）。
    Mismatch(String),
    /// 記録のデコーダがこの環境にない（理由）。
    Unavailable(String),
}

/// staging に書いて照らしたアンカー。
pub(crate) struct MadeAnchor {
    staging: Staging,
    file: NewAnchorFile,
}

impl MadeAnchor {
    /// アンカーのファイルの大きさ（バイト）。
    pub(crate) fn size(&self) -> u64 {
        self.file.size
    }
}

/// 元 `original`（`original_path`）を、記録 `rec` の識別子のデコーダで読み直してアンカーにする（台帳には載せない）。
/// 一時停止・中止は止め方に。読めない・違うは [`Promote`] の理由で、ほかの誤り（入出力・上限・空きなど）は Err。
pub(crate) fn promote_recorded(
    engine: &EngineServices,
    ctx: &dyn UnitCx,
    original: &OriginalFile,
    original_path: &Path,
    rec: &RecordedInput,
) -> Result<Flow<Promote>> {
    let runtime = engine.runtime()?.clone();
    let codecs = engine.codecs();
    let limits: Limits = runtime.bulk(|e| e.limits().clone());
    let mon = UnitMonitor(ctx);
    let layout = ctx.library().layout();
    let cx = stream::StreamCx {
        ctx,
        runtime: &runtime,
        codecs: &codecs,
        engine,
        layout,
        original: original_path,
        original_hash: original.hash,
        format: &rec.format,
        opts: &rec.options,
        limits: &limits,
        mon: &mon,
        class: Class::Bulk,
    };
    // 進捗は送らない（ジョブが対象の数で送る。GC の照合と同じ）。重みは作業用の PCM に読む 1、昇格の照らし 0.5
    let mut passes = stream::Passes::new(&cx, 1.5, false);
    let mut warnings: Vec<String> = Vec::new();
    let mut work = None;
    let cant = "旧版の読み取りを残せません";
    let read = stream::read_input(
        &cx,
        None,
        stream::Want::Recorded {
            decoded: &rec.decoded,
            platform: false,
        },
        cant,
        &mut warnings,
        &mut passes,
        &mut work,
    )?;
    let input = match read {
        Flow::Go(stream::StreamRead::Read(input)) => input,
        Flow::Go(stream::StreamRead::Mismatch(why)) => return Ok(Flow::Go(Promote::Mismatch(why))),
        Flow::Go(stream::StreamRead::Unavailable(why)) => {
            return Ok(Flow::Go(Promote::Unavailable(why)));
        }
        Flow::Stop(end) => return Ok(Flow::Stop(end)),
    };
    let shape = input.value.shape();
    drop(input);
    let work = work.ok_or_else(|| {
        CoreError::Invalid(format!(
            "{cant}：{} の読みに作業用のファイルがありません",
            rec.format
        ))
    })?;
    if work.pcm().decoded != rec.decoded {
        return Ok(Flow::Go(Promote::Mismatch(format!(
            "{cant}：元を読み直した記録が版の記録と違います（{}）",
            rec.decoded.codec
        ))));
    }
    // 照らしの回の量（`verify_stream` の源の LRU 32 MiB ＋ 窓 ＋ ダイジェスト。保存の昇格と同じ式）
    let tasks = runtime.bulk_threads() as u64;
    let _lease = go!(
        ctx,
        "旧版の読み取りを照らすメモリを確保できません",
        runtime.budget().acquire(
            stream::verify_bytes(&shape, tasks, 32 << 20),
            Class::Bulk,
            &mon
        )
    );
    let mut staging = Staging::default();
    std::fs::create_dir_all(layout.staging_dir())?;
    let pass = stream::Pass {
        passes: &passes,
        weight: 0.5,
        message: "旧版の読み取りを照らしています",
    };
    let made = stream::promote(&cx, &work, &rec.decoded, &ctx.tag(), &mut staging, pass)?;
    match made {
        Flow::Go(AnchorUse::New(file)) => Ok(Flow::Go(Promote::Made(MadeAnchor { staging, file }))),
        Flow::Go(_) => Err(CoreError::Invalid(
            "旧版の読み取りのアンカーを作れませんでした".into(),
        )),
        Flow::Stop(end) => Ok(Flow::Stop(end)),
    }
}

/// 載せた結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Placed {
    /// アンカーの行の ID。
    pub anchor_id: i64,
    /// 結び付けた版の入力の数。
    pub linked: usize,
}

/// 呼び手が同じトランザクションで書くもの（結び付けた版の入力の数を受ける。ジョブの途中経過）。
pub(crate) type After = Box<dyn FnOnce(&Connection, usize) -> Result<()> + Send>;

/// [`promote_recorded`] で作ったアンカーを台帳とアンカーの行に載せ、アイテム `item_id` の記録 `rec` と同じ入力に結び付ける
/// （モジュールの説明の 2）。結び付ける入力がもうなければ（読んでいる間にゴミ箱を空にした・記録が変わった）、何も書かずに None
/// （参照されないアンカーを残さない。`after` も呼ばない）。予告のあとでやめるときは、置いたファイルのうち台帳にないものを消して
/// 予告を消す。
pub(crate) fn commit_retired(
    ctx: &dyn UnitCx,
    made: MadeAnchor,
    item_id: i64,
    rec: &RecordedInput,
    after: After,
) -> Result<Option<Placed>> {
    let MadeAnchor { staging, file } = made;
    let library = ctx.library();
    let db = library.db();
    let layout = library.layout();
    let job = ctx.job();
    let gate = library.ingest_exclusive();
    // 予告（台帳にまだない中身だけ。台帳に同じ中身があればその拡張子で置く）
    let hash = file.hash;
    let default_ext = file.ext;
    let (ext, announced) = db.write(move |tx| {
        let listed = ledger::ext_of(tx, &hash)?;
        let ext = listed.clone().unwrap_or_else(|| default_ext.to_string());
        let announced = match listed {
            Some(_) => None,
            None => Some(ledger::announce(
                tx,
                &hash,
                &ext,
                ledger::PendingKind::Place,
                Some(job),
                now_ms(),
            )?),
        };
        Ok((ext, announced))
    })?;
    // 置く
    if let Err(err) = place(
        &Staged {
            path: file.part.clone(),
            hash: file.hash,
            size: file.size,
        },
        &layout.content_path(&file.hash, &ext),
    ) {
        undo_placement(db, layout, job);
        return Err(err);
    }
    #[cfg(test)]
    if ctx.fault() == Some(Fault::AfterPlace) {
        staging.keep();
        return Err(crashed(Fault::AfterPlace));
    }
    // 載せて結び付ける
    let (rec, size) = (rec.clone(), file.size);
    let committed = db.write(move |tx| {
        let rows = same_inputs(tx, item_id, &rec)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let now = now_ms();
        let anchor_id = anchors::insert(
            tx,
            &NewAnchor {
                source: rec.source.0,
                options: &rec.options,
                decoded: &rec.decoded,
                hash,
                ext: &ext,
                size,
                now,
                retired: true,
            },
        )?;
        let linked = set_anchor(tx, rows, anchor_id, &hash)?;
        if let Some(id) = announced {
            ledger::clear_pending(tx, &[id])?;
        }
        after(tx, linked)?;
        Ok(Some(Placed { anchor_id, linked }))
    });
    let placed = match committed {
        Ok(Some(p)) => Some(p),
        Ok(None) => {
            // 置いたファイル（台帳にないもの）と予告を片付ける
            undo_placement(db, layout, job);
            None
        }
        Err(err) => {
            undo_placement(db, layout, job);
            return Err(err);
        }
    };
    drop(gate);
    drop(staging);
    Ok(placed)
}

/// アイテム `item_id` の版の入力のうち、アンカーがなく、記録が `rec` と同じ元・形式・読み方・読み取りの記録のものに、アンカー
/// `anchor_id`（ファイルの中身 `hash`）を付ける（呼び手のトランザクションの中で。版に戻した版は戻した先の入力の記録を写して
/// 持つので、同じ元の入力が何行もある）。付けた数を返す。
pub(crate) fn link_inputs(
    tx: &Connection,
    item_id: i64,
    anchor_id: i64,
    hash: &[u8; 32],
    rec: &RecordedInput,
) -> Result<usize> {
    let rows = same_inputs(tx, item_id, rec)?;
    set_anchor(tx, rows, anchor_id, hash)
}

/// 同じ記録の入力（版の行の ID・入力の番号・記録）。版の行は item_id の索引で、入力は版の行の主キーで引く。
fn same_inputs(
    tx: &Connection,
    item_id: i64,
    rec: &RecordedInput,
) -> Result<Vec<(i64, i64, RecordedInput)>> {
    let options = canonical(&rec.options)
        .map_err(|e| from_engine("読み方の設定を正規の形にできません", e))?;
    let mut stmt = tx.prepare_cached(
        "SELECT i.version_id, i.input, i.recorded
           FROM item_versions v JOIN item_version_inputs i ON i.version_id = v.id
          WHERE v.item_id = ?1 AND +i.anchor_id IS NULL",
    )?;
    let rows = stmt.query_map([item_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (version_id, input, recorded) = row?;
        let Ok(row) = serde_json::from_str::<RecordedInput>(&recorded) else {
            continue;
        };
        let same_options = canonical(&row.options).is_ok_and(|o| o == options);
        if row.source == rec.source
            && row.format == rec.format
            && row.decoded == rec.decoded
            && row.anchor.is_none()
            && same_options
        {
            out.push((version_id, input, row));
        }
    }
    Ok(out)
}

/// 入力 `rows` に、アンカー `anchor_id`（ファイルの中身 `hash`）と記録の `anchor` を付ける。付けた数を返す。
fn set_anchor(
    tx: &Connection,
    rows: Vec<(i64, i64, RecordedInput)>,
    anchor_id: i64,
    hash: &[u8; 32],
) -> Result<usize> {
    let mut update = tx.prepare_cached(
        "UPDATE item_version_inputs SET anchor_id = ?1, recorded = ?2
          WHERE version_id = ?3 AND input = ?4 AND anchor_id IS NULL",
    )?;
    let mut linked = 0;
    for (version_id, input, mut row) in rows {
        row.anchor = Some(Digest(*hash));
        let json =
            canonical(&row).map_err(|e| from_engine("入力の記録を正規の形にできません", e))?;
        let json = String::from_utf8(json)
            .map_err(|e| CoreError::Invalid(format!("入力の記録が UTF-8 ではありません: {e}")))?;
        linked += update.execute(params![anchor_id, json, version_id, input])?;
    }
    Ok(linked)
}
