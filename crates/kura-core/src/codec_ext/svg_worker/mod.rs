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
//! - やり取り（[`wire`]）：子の標準入力・標準出力の決まった形（版付き・印付き・長さ付き・上限付き）。親は子の返事を信用せず、
//!   大きさを確かめてから受け取る。子は頼みを長さどおりに読んだら答える（EOF を待たない）。子の起動は `tools::run_binary_kill`
//!   （環境変数を消す・新しいプロセスグループ・見張り・出力の上限。起動は `tools::spawn` の鍵の中で、ほかの起動のパイプの端を
//!   受け継がない）。
//! - kura-engine は OS・プロセスを知らないので、ここ（kura-core）に置く。`cfg(target_os)` は書かない（unix の
//!   `std::process` だけ）。働き手の入口は各 main（src-tauri・kura-devtools）が呼ぶだけ。テストはフォントなしの働き手
//!   `kura-svg-worker`（このクレートの bin）で Linux でも回る。

pub mod child;
mod parent;
pub mod wire;

use std::ffi::OsStr;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kura_engine::EngineError;
use kura_engine::codec::svg;
use kura_engine::codec::{CodecSet, PinnedOptions};

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
pub fn serve(opts: &PinnedOptions) -> ExitCode {
    let set = CodecSet::pinned_with(opts);
    let Some(decoder) = set.decoder_by_id(svg::DEC_ID) else {
        let _ = writeln!(std::io::stderr(), "SVG のデコーダがありません");
        return ExitCode::from(3);
    };
    let stamp = build_stamp(opts);
    let mut input = std::io::stdin().lock();
    let mut output = std::io::BufWriter::with_capacity(1 << 20, std::io::stdout().lock());
    let request = wire::read_request(&mut input, svg::MAX_SOURCE, &stamp);
    if let Ok(r) = &request {
        exit_after(r.deadline.saturating_add(SELF_GRACE));
    }
    let written = child::serve_request(decoder.as_ref(), &stamp, request, &mut output)
        .and_then(|()| output.flush());
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "返事を書けません: {e}");
            ExitCode::from(2)
        }
    }
}

/// 別のプロセスで読んだ SVG が時間の上限を超えたエラーか（サムネイルは ImageIO へ回さずに種類の絵にする）。
pub fn is_time_limit(err: &EngineError) -> bool {
    matches!(err.root(), EngineError::Limit(m) if m == TIME_LIMIT)
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
