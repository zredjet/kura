//! SVG を別のプロセスで読む（決定 23。docs/editing.md §9.5、docs/architecture.md §7）。
//!
//! - なぜ：usvg・resvg には、描く前の見積もりをすり抜けて数十秒〜数時間止まる形がある（M11 の 2 回目の確認の S1〜S5・S7：
//!   回転を含む変形の下の太い線の外接矩形、同じ offset の stop の 2 乗、stop の多いグラデーションの塗り、細かい破線の丸い端、
//!   回転した pattern の塗り、オクターブの多い feTurbulence）。木を作る段も描く段も中止を見ないので、同じプロセスでは止められ
//!   ない（サムネイル・解析のジョブが止まり、ライブラリを閉じるのも待たされる）。
//! - 形：固定の段の SVG のデコーダ（`svg-resvg-dec/1`）を、同じ識別子・段・再現の約束・環境のまま [`SvgWorkerDecoder`] に
//!   差し替える（[`install`]。アプリと kura-devtools の `EngineFactory::codecs` で組むとき）。形（`shape`）も値（`decode`）も、
//!   呼ぶたびにアプリ自身を [`WORKER_ARG`] 付きで起動した子に頼む。子は Tauri を始めずに [`serve`] で、同じ固定の段の SVG の
//!   デコーダ（`kura_mac::pinned_options()` のシステムのフォント）で読み、返事を書いて終わる。値・識別子・版・`DecodeStamp` は
//!   プロセスの中の固定の段と同じ（決定性・作り直しの照合を変えない）。
//! - 時間：形は [`SHAPE_DEADLINE`]、値は [`DECODE_DEADLINE`]（実測と理由は定数の注釈）。超えたら子をグループごと SIGKILL して
//!   `Limit`（[`TIME_LIMIT`]。サムネイルは ImageIO へ回さず種類の絵にする）。見張り（`Monitor`）の中止でも子を止めて
//!   `Canceled`。子が落ちた・壊れた返事は `Decode`。親が落ちた・終わったときも、子は頼みの時間の上限に [`SELF_GRACE`] を
//!   足したら自分で終わる（止まる SVG を読み続ける子を残さない）。
//! - 実行ファイルとビルド（M11 の 3 回目の確認 W2）：実行ファイルのパスは呼ぶたびに求め直す（[`WorkerCommand`] の求める口。
//!   アプリと kura-devtools は kura-mac の `current_executable`（自分のプロセスの今の実行ファイル。改名・移動を追う））。頼みと
//!   返事の先頭に親と子のビルドの印（[`build_stamp`]）を入れ、子も親も照らす。起動できない・実行ファイルが分からない・印が
//!   合わない（別のビルド）は、アイテムのせいではない一時的な誤り（[`is_unavailable`]。`CodecMissing`）にする。サムネイル・
//!   解析のジョブは、そのアイテムを失敗にせずに残し、ジョブをその場で止める（次にジョブが積まれたときにやり直す）。
//! - 子の数は同時の呼び出しの数まで（サムネイルは一括のプールの並列、解析は 1 件ずつ）。子のメモリは予算の外なので、入力の
//!   大きさの上限（`svg::MAX_SOURCE`。親が先に断る）と子の中の見積もり（文字・textPath・描く手間）を前段として残し、予算の
//!   見積もり（`work_bytes`）はプロセスの中のデコーダのものを使う。
//! - 確保の上限（[`memory`]。ADR-164 (4)）：見積もりを通ったまま木を作る段・描く段で確保が膨らむ形（確認の S2 の 4,000 枚で
//!   4.9 GB、S1 の 40 区間で 2.5 GB、S4 の破線 1 本で 1.27 GB）を、時間の上限より先に止める。子の確保器（アプリは kura-mac の
//!   `ReturningAllocator`）が持つ目印 [`AllocMeter`] を [`serve`] に渡し、子は頼みを読んだら前段の分（入力の大きさに比例）、
//!   デコーダの見積もりの口（`svg::PlanHook`）で、木を作る前に木の分（展開した文字と要素の数に比例）、値を読むときは描く前に
//!   描く分（キャンバスの 3 枚とエンジンの見積もりの一時の画像）を、そのときの量に足した上限で数える。超える確保は断り、子は
//!   落ちる（Rust の abort）か読めないエラーを返し、親はどちらも `Decode`（[`MEMORY_LIMIT`]。[`is_memory_limit`]）にする。
//!   確保器が数えていない働き手は、上限なしでは読まずに内部の誤りを返す。
//! - やり取り（[`wire`]）：子の標準入力・標準出力の決まった形（版付き・印付き・長さ付き・上限付き）。親は子の返事を信用せず、
//!   大きさを確かめてから受け取る。子は頼みを長さどおりに読んだら答える（EOF を待たない）。子の起動は `tools::run_binary_kill`
//!   （環境変数を消す・新しいプロセスグループ・見張り・出力の上限。起動は `tools::spawn` の鍵の中で、ほかの起動のパイプの端を
//!   受け継がない）。
//! - kura-engine は OS・プロセスを知らないので、ここ（kura-core）に置く。`cfg(target_os)` は書かない（unix の
//!   `std::process` だけ）。働き手の入口は各 main（src-tauri・kura-devtools）が呼ぶだけ。テストはフォントなしの働き手
//!   `kura-svg-worker`（このクレートの bin）で Linux でも回る。

pub mod child;
pub mod memory;
mod parent;
pub mod wire;

use std::ffi::OsStr;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kura_engine::EngineError;
use kura_engine::codec::svg::{self, Plan, SvgCodec};
use kura_engine::codec::{CodecSet, PinnedOptions};

pub use memory::{AllocMeter, MEMORY_LIMIT};
pub use parent::{Deadlines, ExeResolver, SvgWorkerDecoder, WorkerCommand};

/// 働き手として起動する引数（`<アプリ> --kura-svg-worker`。ほかの引数はとらない）。
pub const WORKER_ARG: &str = "--kura-svg-worker";

/// 時間の上限を超えたときの `Limit` の言葉。
pub const TIME_LIMIT: &str = "SVG を読むのに時間がかかりすぎます";

/// 働き手を使えない（一時的な誤り）ときの `EngineError::CodecMissing` の `tool` の頭（[`is_unavailable`]）。
pub const WORKER_TOOL: &str = "SVG を読む働き手";

/// 形（木を作り、描く前の見積もりをする）の時間の上限。M4 Max の release の実測（docs/editing.md §9.5）：子の起動は 1 回
/// 約 5 ms、手元のふつうの SVG 1,439 件は形も値も 1 件 80 ms 以下、16 MiB の地図風の SVG（短い path 4.5 万本）の形が
/// 0.24 秒、文字の上限の近く（決定 20）が 0.05 秒。その 8 倍ほどにして、木を作る段で止まる確認の S1（2 区間で 2.6 秒）・
/// S3（同じ offset の stop 40 万個で 14 秒）を止める。
pub const SHAPE_DEADLINE: Duration = Duration::from_secs(2);

/// 値（木を作り直して描き、画素を親へ送る）の時間の上限。実測（同上）：見積もりが受け付ける重い形のうち、地図風の線
/// 30 万区間が 1 件 4.5 秒（コアの数の 16 件を同時に描くと 6.8 秒）、16000×16000 の値（1 GiB）は描いて送るまで 2.7 秒。
/// これを同時に描いても受け付けつつ、確認の S2（stop 10 万の塗り 10 枚で 32 秒）・S4（5 分）・S5（100 秒）・S7（18 秒）を
/// 止める値にする（5 秒では地図風のものを同時に描くと止めてしまう）。
pub const DECODE_DEADLINE: Duration = Duration::from_secs(10);

/// 子が自分で終わるまでの、頼みの時間の上限に足す猶予。ふだんは親が上限で子を止めるので届かない。親が落ちた・終わった
/// ときに、止まる SVG を読み続ける子を残さない（[`serve`]）。
pub const SELF_GRACE: Duration = Duration::from_secs(2);

/// 子が自分で終わるときの終了コード（EX_TEMPFAIL）。
const SELF_DEADLINE_EXIT: i32 = 75;

/// 頼みを読むまでの確保の上限（入力の上限と、見出し・返事を書く区切り・ビルドの印の分）。
const REQUEST_ALLOWANCE: u64 = svg::MAX_SOURCE + (16 << 20);

/// `after` たったら、このプロセスを終える（働き手の中だけで使う。答えを書き終えれば、その前にふつうに終わる）。
fn exit_after(after: Duration) {
    let spawned = std::thread::Builder::new()
        .name("kura-svg-worker-deadline".into())
        .spawn(move || {
            std::thread::sleep(after);
            let _ = writeln!(std::io::stderr(), "時間の上限を過ぎたので終わります");
            std::process::exit(SELF_DEADLINE_EXIT);
        });
    if spawned.is_err() {
        // 見張りを起こせないなら、親の見張り（時間の上限で SIGKILL）だけに頼る
        let _ = writeln!(std::io::stderr(), "自分で終わる見張りを起こせません");
    }
}

/// 親と子が同じビルド・同じ設定かを照らす印（頼みと返事の先頭に入れる。M11 の 3 回目の確認 W2）：SVG のデコーダの識別子・
/// kura-core と kura-engine の版・ビルドの種類（debug・release）・CPU・フォントの有無と総称の書体（`opts`）。フォントの数は
/// 入れない（数えるにはシステムのフォントを開いて読む必要があり、文字のない SVG でも子の起動ごとにかかる）。
pub fn build_stamp(opts: &PinnedOptions) -> String {
    let fonts = opts.svg_fonts.as_ref().map(|f| {
        let g = f.generic();
        serde_json::json!({
            "serif": g.serif,
            "sansSerif": g.sans_serif,
            "cursive": g.cursive,
            "fantasy": g.fantasy,
            "monospace": g.monospace,
        })
    });
    serde_json::json!({
        "decoder": svg::DEC_ID,
        "core": env!("CARGO_PKG_VERSION"),
        "engine": kura_engine::VERSION,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "arch": std::env::consts::ARCH,
        "fonts": fonts,
    })
    .to_string()
}

/// 働き手を使えない一時的な誤り（起動できない・実行ファイルが分からない・別のビルド）。アイテムのせいではないので、
/// サムネイル・解析は失敗にせずにジョブを止める（[`is_unavailable`]）。
pub(crate) fn unavailable(why: impl std::fmt::Display) -> EngineError {
    EngineError::CodecMissing {
        tool: format!("{WORKER_TOOL}（{why}）"),
    }
}

/// 働き手を使えない一時的な誤りか（起動できない・実行ファイルが分からない・別のビルド。子の返事からは作れない）。
pub fn is_unavailable(err: &EngineError) -> bool {
    matches!(err.root(), EngineError::CodecMissing { tool } if tool.starts_with(WORKER_TOOL))
}

/// 働き手を使えない（`why`：[`is_unavailable`] のエラーの言葉）のでジョブ（`job`：「サムネイルの作成」など）を止めたときの
/// 言葉（ジョブの失敗の説明とログ）。
pub fn job_stopped(job: &str, why: impl std::fmt::Display) -> String {
    format!(
        "{why}。{job}を止めました。SVG のアイテムは失敗にせずに残し、次に{job}が動くとき（ライブラリを開き直す・\
         取り込むなど）にやり直します。アプリを動かしたまま移動・置き換えたときは、アプリを開き直してください"
    )
}

/// 引数が働き手の起動（`<exe> --kura-svg-worker` だけ）か。各 main が最初に呼ぶ。
pub fn is_worker_invocation() -> bool {
    let mut args = std::env::args_os().skip(1);
    args.next().as_deref() == Some(OsStr::new(WORKER_ARG)) && args.next().is_none()
}

/// 働き手として動く：標準入力の頼みを 1 つ読み、固定の段の SVG のデコーダ（`opts` のフォントつき）で読み、標準出力に返事を
/// 書いて終わる（Tauri もログも始めない）。頼みのビルドの印が自分の印（[`build_stamp`]）と違えば、読まずに専用のエラーを
/// 返す。返事を書けなければ 2。親がいなくなっても、頼みの時間の上限に [`SELF_GRACE`] を足したら自分で終わる（75）。
/// `meter` はこのプロセスの確保器の目印（確保の上限。[`memory`]）。
pub fn serve(opts: &PinnedOptions, meter: &'static AllocMeter) -> ExitCode {
    // 頼みを読む前から数える（確保器が数えていなければ、上限なしでは読まない）
    meter.start(REQUEST_ALLOWANCE);
    let metered = counts_allocations(meter);
    let stamp = build_stamp(opts);
    let mut input = std::io::stdin().lock();
    let mut output = std::io::BufWriter::with_capacity(1 << 20, std::io::stdout().lock());
    let request = wire::read_request(&mut input, svg::MAX_SOURCE, &stamp);
    if let Ok(r) = &request {
        exit_after(r.deadline.saturating_add(SELF_GRACE));
    }
    let written = answer_capped(opts, &stamp, request, meter, metered, &mut output)
        .and_then(|()| output.flush());
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "返事を書けません: {e}");
            ExitCode::from(2)
        }
    }
}

/// `meter` がこのプロセスの確保を数えているか（確保器が `meter` を持つか）。小さな確保を 1 つして確かめる。
fn counts_allocations(meter: &AllocMeter) -> bool {
    let before = meter.used();
    let probe: Vec<u8> = std::hint::black_box(Vec::with_capacity(4_096));
    let counted = meter.used() >= before.saturating_add(4_096);
    drop(probe);
    counted
}

/// 頼み（読めなかったら理由）に、確保の上限を付けて答える（[`serve`] の後半）。前段の見積もりの分
/// （[`memory::precheck_allowance`]）を今の量に足した上限で読み始め、エンジンの見積もりの口で、木を作る前は木の分
/// （[`memory::tree_allowance`]）、描く前は描く分（[`memory::render_allowance`]）を、そのときの量に足した上限にする。
/// `metered` が偽（確保器が数えていない）なら、読まずに内部の誤りを返す。
fn answer_capped(
    opts: &PinnedOptions,
    stamp: &str,
    request: Result<wire::Request, wire::RequestError>,
    meter: &'static AllocMeter,
    metered: bool,
    output: &mut dyn Write,
) -> std::io::Result<()> {
    let len = match &request {
        Ok(_) if !metered => {
            return wire::write_error(
                output,
                stamp,
                &EngineError::Internal(
                    "SVG の働き手の確保器が確保を数えていません（上限なしでは読みません）".into(),
                ),
            );
        }
        Ok(r) => match &r.source {
            wire::Source::Bytes(b) => b.len() as u64,
            // 開けなければ 0（デコーダが入出力のエラーを返す）
            wire::Source::Path(p) => std::fs::metadata(p).map_or(0, |m| m.len()),
        },
        Err(_) => 0,
    };
    meter.set_limit(meter.used().saturating_add(memory::precheck_allowance(len)));
    let decoder = SvgCodec::pinned(opts).with_plan_hook(Arc::new(move |plan| {
        let allowance = match plan {
            Plan::Tree(p) => memory::tree_allowance(p),
            Plan::Render(p) => memory::render_allowance(p, len),
        };
        meter.set_limit(meter.used().saturating_add(allowance));
    }));
    child::serve_request(&decoder, stamp, request, output, Some(meter))
}

/// 別のプロセスで読んだ SVG が時間の上限を超えたエラーか（サムネイルは ImageIO へ回さずに種類の絵にする）。
pub fn is_time_limit(err: &EngineError) -> bool {
    matches!(err.root(), EngineError::Limit(m) if m == TIME_LIMIT)
}

/// 別のプロセスで読んだ SVG が確保の上限を超えたエラーか（`Decode`。[`MEMORY_LIMIT`]）。
pub fn is_memory_limit(err: &EngineError) -> bool {
    matches!(err.root(), EngineError::Decode { codec, message } if codec == svg::DEC_ID && message == MEMORY_LIMIT)
}

/// 組の SVG のデコーダを、`cmd` の働き手に頼むものに差し替える（識別子・段・再現の約束はそのまま）。`opts` は組を作った
/// 固定の段の設定（親のビルドの印 [`build_stamp`] にする。働き手も同じ設定で動くこと）。実行ファイルは読むたびに求め、
/// 求められない・起動できない・印が合わないときは、その理由の一時的な誤り（[`is_unavailable`]）を返す（同じプロセスでは
/// 読まない）。
pub fn install(
    set: &mut CodecSet,
    opts: &PinnedOptions,
    cmd: WorkerCommand,
) -> Result<(), EngineError> {
    install_with(set, opts, cmd, Deadlines::default())
}

/// [`install`] の時間の上限を決められる形（テスト・計測用）。
pub fn install_with(
    set: &mut CodecSet,
    opts: &PinnedOptions,
    cmd: WorkerCommand,
    deadlines: Deadlines,
) -> Result<(), EngineError> {
    let inner = set
        .decoder_by_id(svg::DEC_ID)
        .cloned()
        .ok_or_else(|| EngineError::Internal("SVG のデコーダがありません".into()))?;
    set.replace_decoder(Arc::new(
        SvgWorkerDecoder::new(inner, cmd, build_stamp(opts)).with_deadlines(deadlines),
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::Limits;
    use kura_engine::codec::svg::{RenderPlan, TreePlan};
    use kura_engine::doc::DecodeOptions;

    /// `body` を `op` で頼み、確保の上限を付けて答えた返事。
    fn answered(
        meter: &'static AllocMeter,
        metered: bool,
        op: wire::Op,
        body: &[u8],
    ) -> wire::Reply {
        let stamp = build_stamp(&PinnedOptions::default());
        let mut req = Vec::new();
        wire::write_request(
            &mut req,
            &stamp,
            op,
            wire::Input::Bytes(body),
            "svg",
            &DecodeOptions::default(),
            &Limits::default(),
            Duration::from_secs(60),
        )
        .unwrap();
        let request = wire::read_request(&mut req.as_slice(), svg::MAX_SOURCE, &stamp);
        let mut out = Vec::new();
        answer_capped(
            &PinnedOptions::default(),
            &stamp,
            request,
            meter,
            metered,
            &mut out,
        )
        .unwrap();
        let mut r = wire::ReplyReader::new(op, &Limits::default(), &stamp);
        r.push(&out).unwrap();
        r.finish().unwrap()
    }

    const SVG: &[u8] =
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="20"><rect width="5" height="5"/></svg>"##;

    /// 上限は、木を作る前に今の量に木の分（展開した文字と要素の数から）を足したもの、値を読むときは描く前に描く分（大きさと
    /// 一時の画像の見積もりから）を足したもの（このテストのプロセスの確保器は数えないので、今の量は 0 のまま）。前段で
    /// 断るものは、頼みを読んだときの前段の分のまま。
    #[test]
    fn the_limit_follows_the_engine_estimates() {
        static METER: AllocMeter = AllocMeter::new();
        METER.start(REQUEST_ALLOWANCE);
        let len = SVG.len() as u64;
        assert!(matches!(
            answered(&METER, true, wire::Op::Shape, SVG),
            wire::Reply::Shape(_)
        ));
        let tree = TreePlan {
            text_bytes: len,
            elements: 2,
        };
        assert_eq!(METER.limit(), memory::tree_allowance(&tree));
        let svgz = b"\x1f\x8b\x08\x00";
        assert!(matches!(
            answered(&METER, true, wire::Op::Shape, svgz),
            wire::Reply::Error(EngineError::Unsupported(_))
        ));
        assert_eq!(METER.limit(), memory::precheck_allowance(svgz.len() as u64));
        assert!(matches!(
            answered(&METER, true, wire::Op::Decode, SVG),
            wire::Reply::Value(_)
        ));
        let plan = RenderPlan {
            width: 30,
            height: 20,
            scratch_bytes: 0,
        };
        assert_eq!(METER.limit(), memory::render_allowance(&plan, len));
    }

    /// 確保器が目印で数えていなければ（このテストのプロセスの確保器は `System`）、上限なしでは読まずに内部の誤りを返す。
    #[test]
    fn a_worker_whose_allocator_does_not_count_refuses_to_read() {
        static METER: AllocMeter = AllocMeter::new();
        METER.start(REQUEST_ALLOWANCE);
        assert!(!counts_allocations(&METER));
        for op in [wire::Op::Shape, wire::Op::Decode] {
            match answered(&METER, false, op, SVG) {
                wire::Reply::Error(EngineError::Internal(m)) => assert!(m.contains("確保"), "{m}"),
                other => panic!("{other:?}"),
            }
        }
    }

    /// 確保の上限のエラーを見分ける（SVG のデコーダの `Decode` で、言葉が `MEMORY_LIMIT` のものだけ）。
    #[test]
    fn memory_limits_are_told_apart() {
        let decode = |codec: &str, message: &str| EngineError::Decode {
            codec: codec.into(),
            message: message.into(),
        };
        assert!(is_memory_limit(&decode(svg::DEC_ID, MEMORY_LIMIT)));
        assert!(!is_memory_limit(&decode(svg::DEC_ID, "壊れています")));
        assert!(!is_memory_limit(&decode("png-rs-dec/1", MEMORY_LIMIT)));
        assert!(!is_memory_limit(&EngineError::Limit(MEMORY_LIMIT.into())));
        assert!(!is_time_limit(&decode(svg::DEC_ID, MEMORY_LIMIT)));
    }
}
