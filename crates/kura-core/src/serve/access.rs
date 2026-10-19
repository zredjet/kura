//! kura:// の受け付けと応答のヘッダ、CSP の確かめ（docs/architecture.md §4.1）。
//!
//! Tauri に依らない純粋な関数にして、CI（Linux）で回す。src-tauri はこれを HTTP の型に写すだけ。

/// kura:// を受け付ける WebView のラベル（メインウィンドウ）。
pub const MAIN_WEBVIEW: &str = "main";

/// `tauri build` のページの origin（macOS）。
pub const RELEASE_ORIGIN: &str = "tauri://localhost";

/// 受け付けるメソッド（`Allow`・`Access-Control-Allow-Methods` の値）。
pub const ALLOWED_METHODS: &str = "GET, HEAD, OPTIONS";

/// アプリのページの origin。起動時に 1 回決め、ACAO と移動の制限に使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppOrigin(String);

impl AppOrigin {
    /// `tauri dev`（`is_dev`）なら devUrl の origin、それ以外は `tauri://localhost`。
    /// devUrl が http(s) の origin として読めなければ `tauri://localhost`（ACAO に崩れた値を入れない）。
    pub fn new(is_dev: bool, dev_url: Option<&str>) -> Self {
        let dev = is_dev
            .then(|| dev_url.and_then(origin_of))
            .flatten()
            .filter(|o| o.starts_with("http://") || o.starts_with("https://"));
        Self(dev.unwrap_or_else(|| RELEASE_ORIGIN.to_string()))
    }

    /// origin の文字列（`scheme://host[:port]`）。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// main の WebView がこの URL へ移ってよいか（アプリの origin の中だけ）。
    pub fn allows_navigation(&self, url: &str) -> bool {
        origin_of(url).is_some_and(|o| o == self.0)
    }
}

/// URL の origin（`scheme://host[:port]`。scheme と host は小文字にし、http・https の既定のポートは省く）。
/// `scheme://host` の形でないもの（about:・data:・blob: など）・資格情報つき・ポートの崩れたものは None。
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let valid_scheme = scheme
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    if !valid_scheme {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return None;
    }
    // IPv6 は [..] のあと、それ以外は最後の : でポートを分ける
    let (host, port) = match authority.strip_prefix('[') {
        Some(after) => {
            let (inside, tail) = after.split_once(']')?;
            let port = match tail {
                "" => None,
                t => Some(t.strip_prefix(':')?),
            };
            let ok =
                !inside.is_empty() && inside.bytes().all(|b| b.is_ascii_hexdigit() || b == b':');
            if !ok {
                return None;
            }
            (format!("[{}]", inside.to_ascii_lowercase()), port)
        }
        None => {
            let (host, port) = match authority.rsplit_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (authority, None),
            };
            let ok = !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
            if !ok {
                return None;
            }
            (host.to_ascii_lowercase(), port)
        }
    };
    let port = match port {
        None => None,
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
            Some(p.parse::<u16>().ok()?)
        }
        Some(_) => return None,
    };
    let default_port = match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    Some(match port {
        Some(p) if Some(p) != default_port => format!("{scheme}://{host}:{p}"),
        _ => format!("{scheme}://{host}"),
    })
}

/// 受け付けの判定の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// 読んで返す（`head` なら本体なし）。
    Serve { head: bool },
    /// 事前確認（OPTIONS）に 204 で答える。
    Preflight,
    /// 断る。`cors` が偽ならヘッダを付けない（main 以外の WebView）。
    Reject { status: u16, cors: bool },
}

/// 要求を受け付けるか（ファイルに触る前に決める）。`origin` は `Origin` ヘッダ。
///
/// 判定の順：ラベル（main 以外は 403・ヘッダなし）→ Origin（あってアプリの origin でなければ 403。`null` も）→
/// メソッド（OPTIONS は事前確認、GET・HEAD は読む、ほかは 405）。メソッドは大文字・小文字を区別する（HTTP の規則）。
pub fn admit(label: &str, method: &str, origin: Option<&str>, app: &AppOrigin) -> Admit {
    if label != MAIN_WEBVIEW {
        return Admit::Reject {
            status: 403,
            cors: false,
        };
    }
    if origin.is_some_and(|o| o != app.as_str()) {
        return Admit::Reject {
            status: 403,
            cors: true,
        };
    }
    match method {
        "GET" => Admit::Serve { head: false },
        "HEAD" => Admit::Serve { head: true },
        "OPTIONS" => Admit::Preflight,
        _ => Admit::Reject {
            status: 405,
            cors: true,
        },
    }
}

/// main への応答すべて（エラーを含む）に付けるヘッダ。ACAO は固定のアプリの origin（`*` にしない）。
pub fn response_headers(app: &AppOrigin) -> Vec<(&'static str, &str)> {
    vec![
        ("Access-Control-Allow-Origin", app.as_str()),
        (
            "Access-Control-Expose-Headers",
            "Content-Range, Accept-Ranges, Content-Length",
        ),
        // 文書として開かれてもスクリプトを動かさない（kura:// の SVG。<img>・fetch などには効かない）
        ("Content-Security-Policy", "sandbox"),
        ("X-Content-Type-Options", "nosniff"),
    ]
}

/// 事前確認（204）に足すヘッダ。
pub fn preflight_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Access-Control-Allow-Methods", ALLOWED_METHODS),
        ("Access-Control-Allow-Headers", "Range"),
        ("Access-Control-Max-Age", "600"),
    ]
}

/// kura: で読む指令（img・media・fetch・フォント）。
const KURA_DIRECTIVES: [&str; 4] = ["img-src", "media-src", "connect-src", "font-src"];
/// connect-src が default-src を上書きしても invoke が通るために要るもの。
const IPC_SOURCES: [&str; 2] = ["ipc:", "http://ipc.localhost"];
/// どの指令にも入れない（広げすぎ）もの。
const FORBIDDEN_SOURCES: [&str; 4] = ["blob:", "data:", "*", "'unsafe-eval'"];

/// CSP（tauri.conf.json の `app.security.csp`）が kura:// の読み方に足りているかを確かめ、足りない点を返す。
///
/// 見ること（docs/architecture.md §4.1）：img・media・connect・font の各 src に `kura:`、connect-src に
/// `ipc:`・`http://ipc.localhost`、object-src・frame-src が `'none'` だけ、どの指令にも `blob:`・`data:`・
/// `*`・`'unsafe-eval'` がない、スクリプト（script-src、なければ default-src）と default-src に `kura:` がない。
/// 同じ指令が 2 度あれば、ブラウザと同じく最初のものだけを見る。
pub fn csp_problems(policy: &str) -> Vec<String> {
    let mut directives: Vec<(String, Vec<&str>)> = Vec::new();
    for part in policy.split(';') {
        let mut tokens = part.split_ascii_whitespace();
        let Some(name) = tokens.next() else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        if directives.iter().any(|(n, _)| *n == name) {
            continue;
        }
        directives.push((name, tokens.collect()));
    }
    let get = |name: &str| {
        directives
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    };
    let mut problems = Vec::new();
    if get("default-src").is_none() {
        problems.push("default-src がありません".to_string());
    }
    for name in KURA_DIRECTIVES {
        match get(name) {
            None => problems.push(format!("{name} がありません（kura: を読めません）")),
            Some(sources) if !sources.contains(&"kura:") => {
                problems.push(format!("{name} に kura: がありません"));
            }
            Some(_) => {}
        }
    }
    if let Some(sources) = get("connect-src") {
        for ipc in IPC_SOURCES {
            if !sources.contains(&ipc) {
                problems.push(format!(
                    "connect-src に {ipc} がありません（default-src を上書きするので invoke が止まります）"
                ));
            }
        }
    }
    for name in ["object-src", "frame-src"] {
        if get(name) != Some(["'none'"].as_slice()) {
            problems.push(format!("{name} が 'none' だけではありません"));
        }
    }
    for (name, sources) in &directives {
        for bad in FORBIDDEN_SOURCES {
            if sources.contains(&bad) {
                problems.push(format!("{name} に {bad} があります"));
            }
        }
    }
    for name in ["script-src", "default-src"] {
        if get(name).is_some_and(|s| s.contains(&"kura:")) {
            problems.push(format!(
                "{name} に kura: があります（kura:// のスクリプトが動きます）"
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV_URL: &str = "http://localhost:1430/";

    fn release() -> AppOrigin {
        AppOrigin::new(false, Some(DEV_URL))
    }

    fn dev() -> AppOrigin {
        AppOrigin::new(true, Some(DEV_URL))
    }

    #[test]
    fn app_origin_for_dev_and_release() {
        assert_eq!(release().as_str(), "tauri://localhost");
        assert_eq!(dev().as_str(), "http://localhost:1430");
        // dev でも devUrl がなければ tauri://localhost から読む
        assert_eq!(AppOrigin::new(true, None).as_str(), "tauri://localhost");
        assert_eq!(
            AppOrigin::new(true, Some("not a url")).as_str(),
            "tauri://localhost"
        );
        assert_eq!(
            AppOrigin::new(true, Some("HTTP://LocalHost:80/x?y#z")).as_str(),
            "http://localhost"
        );
    }

    #[test]
    fn origins_are_scheme_host_and_port() {
        assert_eq!(
            origin_of("tauri://localhost").as_deref(),
            Some("tauri://localhost")
        );
        assert_eq!(
            origin_of("tauri://localhost/index.html?a=1#b").as_deref(),
            Some("tauri://localhost")
        );
        assert_eq!(
            origin_of("http://localhost:1430/").as_deref(),
            Some("http://localhost:1430")
        );
        assert_eq!(
            origin_of("https://example.com:443/a").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            origin_of("http://[::1]:1430/").as_deref(),
            Some("http://[::1]:1430")
        );
        assert_eq!(
            origin_of("kura://localhost/file/x").as_deref(),
            Some("kura://localhost")
        );
        // 形の崩れたもの・資格情報つき・ポートの崩れ・origin のないもの
        for bad in [
            "about:blank",
            "about:srcdoc",
            "data:text/html,x",
            "blob:tauri://localhost/1",
            "http://user@localhost:1430/",
            "http://localhost:1430@evil.example/",
            "http://localhost:1430.evil.example/",
            "http://localhost:99999/",
            "http://:1430/",
            "://localhost",
            "",
        ] {
            assert_eq!(origin_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn main_navigates_only_within_the_app() {
        let release = release();
        assert!(release.allows_navigation("tauri://localhost"));
        assert!(release.allows_navigation("tauri://localhost/index.html"));
        assert!(!release.allows_navigation("http://localhost:1430/"));
        let dev = dev();
        assert!(dev.allows_navigation("http://localhost:1430/"));
        assert!(dev.allows_navigation("http://localhost:1430/src/main.tsx?t=1"));
        for app in [&release, &dev] {
            for url in [
                "kura://localhost/file/0000000000000000000000000000000000000000000000000000000000000000",
                "https://example.com/",
                "http://localhost:1431/",
                "http://127.0.0.1:1430/",
                "file:///etc/passwd",
                "about:blank",
                "about:srcdoc",
                "data:text/html,x",
                "blob:tauri://localhost/1",
                "javascript:alert(1)",
            ] {
                assert!(!app.allows_navigation(url), "{} → {url}", app.as_str());
            }
        }
    }

    #[test]
    fn rejects_other_webviews_before_touching_files() {
        let app = release();
        for label in ["capture-browser", "Main", "", "main2"] {
            assert_eq!(
                admit(label, "GET", None, &app),
                Admit::Reject {
                    status: 403,
                    cors: false
                },
                "{label}"
            );
            // メソッドや Origin がアプリのものでも、ラベルが先
            assert_eq!(
                admit(label, "OPTIONS", Some(app.as_str()), &app),
                Admit::Reject {
                    status: 403,
                    cors: false
                }
            );
        }
    }

    #[test]
    fn answers_only_get_head_and_options() {
        let app = release();
        let origin = Some(app.as_str());
        assert_eq!(
            admit(MAIN_WEBVIEW, "GET", origin, &app),
            Admit::Serve { head: false }
        );
        assert_eq!(
            admit(MAIN_WEBVIEW, "HEAD", origin, &app),
            Admit::Serve { head: true }
        );
        assert_eq!(
            admit(MAIN_WEBVIEW, "OPTIONS", origin, &app),
            Admit::Preflight
        );
        for method in [
            "POST", "PUT", "DELETE", "PATCH", "TRACE", "CONNECT", "get", "",
        ] {
            assert_eq!(
                admit(MAIN_WEBVIEW, method, origin, &app),
                Admit::Reject {
                    status: 405,
                    cors: true
                },
                "{method}"
            );
        }
    }

    #[test]
    fn rejects_foreign_origin_from_main() {
        let app = dev();
        // Origin がなければ受ける（crossorigin のない <img>・<audio>）
        assert_eq!(
            admit(MAIN_WEBVIEW, "GET", None, &app),
            Admit::Serve { head: false }
        );
        assert_eq!(
            admit(MAIN_WEBVIEW, "GET", Some("http://localhost:1430"), &app),
            Admit::Serve { head: false }
        );
        for foreign in [
            "null",
            "tauri://localhost",
            "http://localhost:1431",
            "http://127.0.0.1:1430",
            "https://example.com",
            "kura://localhost",
            "http://localhost:1430/",
            "",
        ] {
            for method in ["GET", "OPTIONS", "POST"] {
                assert_eq!(
                    admit(MAIN_WEBVIEW, method, Some(foreign), &app),
                    Admit::Reject {
                        status: 403,
                        cors: true
                    },
                    "{foreign} {method}"
                );
            }
        }
    }

    #[test]
    fn main_responses_carry_cors_sandbox_and_nosniff() {
        for app in [release(), dev()] {
            let headers = response_headers(&app);
            let get = |name: &str| {
                headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| *v)
            };
            assert_eq!(get("Access-Control-Allow-Origin"), Some(app.as_str()));
            assert_eq!(
                get("Access-Control-Expose-Headers"),
                Some("Content-Range, Accept-Ranges, Content-Length")
            );
            assert_eq!(get("Content-Security-Policy"), Some("sandbox"));
            assert_eq!(get("X-Content-Type-Options"), Some("nosniff"));
        }
        let pre = preflight_headers();
        assert!(pre.contains(&("Access-Control-Allow-Methods", ALLOWED_METHODS)));
        assert!(pre.contains(&("Access-Control-Allow-Headers", "Range")));
        assert!(pre.contains(&("Access-Control-Max-Age", "600")));
    }

    #[test]
    fn acao_is_never_wildcard() {
        for (is_dev, url) in [
            (false, None),
            (true, None),
            (true, Some("*")),
            (true, Some("http://*:1430/")),
            (true, Some(DEV_URL)),
        ] {
            let app = AppOrigin::new(is_dev, url);
            assert!(!app.as_str().contains('*'), "{}", app.as_str());
            for (name, value) in response_headers(&app) {
                assert!(!value.contains('*'), "{name}: {value}");
            }
            // Origin が * のふりをしても受けない
            assert_eq!(
                admit(MAIN_WEBVIEW, "GET", Some("*"), &app),
                Admit::Reject {
                    status: 403,
                    cors: true
                }
            );
        }
    }

    /// 今の（M10 までの）CSP。M11 で足りなくなった点を数える。
    const OLD_CSP: &str = "default-src 'self' ipc: http://ipc.localhost; img-src 'self' kura:; media-src 'self' kura:; style-src 'self' 'unsafe-inline'";

    fn has_problem(problems: &[String], needle: &str) -> bool {
        problems.iter().any(|p| p.contains(needle))
    }

    #[test]
    fn csp_problems_name_what_is_missing() {
        let problems = csp_problems(OLD_CSP);
        assert!(has_problem(&problems, "connect-src"), "{problems:?}");
        assert!(has_problem(&problems, "font-src"), "{problems:?}");
        assert!(has_problem(&problems, "object-src"), "{problems:?}");
        assert!(has_problem(&problems, "frame-src"), "{problems:?}");

        // connect-src を足しても ipc: を書き直し忘れると invoke が止まる
        let no_ipc = "default-src 'self' ipc: http://ipc.localhost; img-src 'self' kura:; media-src 'self' kura:; connect-src 'self' kura:; font-src 'self' kura:; object-src 'none'; frame-src 'none'";
        let problems = csp_problems(no_ipc);
        assert!(has_problem(&problems, "ipc:"), "{problems:?}");
        assert!(
            has_problem(&problems, "http://ipc.localhost"),
            "{problems:?}"
        );

        let good = "default-src 'self' ipc: http://ipc.localhost; img-src 'self' kura:; media-src 'self' kura:; connect-src 'self' ipc: http://ipc.localhost kura:; font-src 'self' kura:; style-src 'self' 'unsafe-inline'; object-src 'none'; frame-src 'none'";
        assert_eq!(csp_problems(good), Vec::<String>::new());
        // 指令の名前は大文字・小文字を問わず、空の指令や余分な空白は無視する
        let spaced = good.replace("; ", " ;  ").replace("img-src", "IMG-SRC");
        assert_eq!(csp_problems(&spaced), Vec::<String>::new());

        // 広げすぎ：blob:・data:・*・kura: のスクリプト・eval
        for (extra, needle) in [
            ("; img-src 'self' kura: data:", "data:"),
            ("; media-src 'self' kura: blob:", "blob:"),
            ("; connect-src *", "*"),
            ("; script-src 'self' kura:", "script-src"),
            ("; script-src 'self' 'unsafe-eval'", "unsafe-eval"),
        ] {
            // 後ろの同じ指令は無視されるので、前に置く
            let policy = format!("{}; {good}", extra.trim_start_matches("; "));
            assert!(
                has_problem(&csp_problems(&policy), needle),
                "{policy}: {:?}",
                csp_problems(&policy)
            );
        }
        // script-src がなければ default-src を見る
        let script_default = good.replace("default-src 'self'", "default-src 'self' kura:");
        assert!(has_problem(&csp_problems(&script_default), "default-src"));
        assert!(!csp_problems("").is_empty());
    }

    #[test]
    fn csp_in_tauri_conf_allows_kura_for_fetch_and_fonts() {
        let conf: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../src-tauri/tauri.conf.json"
        )))
        .unwrap();
        let csp = conf["app"]["security"]["csp"].as_str().unwrap();
        assert_eq!(csp_problems(csp), Vec::<String>::new(), "{csp}");
    }
}
