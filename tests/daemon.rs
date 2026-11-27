mod common;
#[path = "common/script.rs"]
mod script;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use common::TmpDir;
use rowser::config::Target;
use rowser::daemon::testing::{Answer, RecordingNotifier, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{
    bind_sockets, CommandOpener, Daemon, DaemonConfig, NoticeHost, Notifier, Opener,
    FORWARD_NOT_APPROVED, NOT_APPROVED,
};
use rowser::protocol::{Conn, Hello, Msg, CONTROL_SOCKET};
use rowser::routes::Routes;
use script::write_script;
use tokio::net::UnixStream;
use url::Url;

/// The Saved hosts that have a test Link in each Harness.
const ALIASES: &[&str] = &["devbox", "b", "g14", "db1", "db.prod", "DB.prod"];

struct Harness<N = RecordingNotifier> {
    dir: TmpDir,
    opener: RecordingOpener,
    notifier: N,
}

impl Harness {
    async fn start() -> Harness {
        Harness::with(DaemonConfig::default(), RecordingNotifier::default()).await
    }
}

impl<N: Notifier + Clone> Harness<N> {
    /// A Daemon with `config` and `notifier`; the harness keeps a clone of it.
    async fn with(config: DaemonConfig, notifier: N) -> Harness<N> {
        let dir = TmpDir::new("d");
        let run = dir.0.join("run");
        let control = bind_sockets(&run).unwrap();
        let opener = RecordingOpener::default();
        let daemon = Daemon::new(config, opener.clone(), notifier.clone());
        let links = ALIASES
            .iter()
            .enumerate()
            .map(|(i, alias)| {
                let path = run.join(format!("link-{i}.sock"));
                (
                    alias.to_string(),
                    tokio::net::UnixListener::bind(path).unwrap(),
                )
            })
            .collect();
        tokio::spawn(daemon.serve_with_test_links(control, links));
        Harness {
            dir,
            opener,
            notifier,
        }
    }

    async fn connect(&self, name: &str) -> Conn<UnixStream> {
        let stream = UnixStream::connect(self.dir.0.join("run").join(name))
            .await
            .unwrap();
        Conn::connect(stream, Hello::current()).await.unwrap()
    }

    /// Connects to the test Link of the Saved host `alias`.
    async fn connect_link(&self, alias: &str) -> Conn<UnixStream> {
        let i = ALIASES.iter().position(|a| *a == alias).expect("alias");
        self.connect(&format!("link-{i}.sock")).await
    }

    fn run(&self) -> std::path::PathBuf {
        self.dir.0.join("run")
    }

    fn opened(&self) -> Vec<String> {
        self.opener.urls().iter().map(|u| u.to_string()).collect()
    }
}

fn open(url: &str) -> Msg {
    Msg::Open {
        url: url.into(),
        callback_ports: vec![],
    }
}

async fn request(conn: &mut Conn<UnixStream>, msg: Msg) -> Msg {
    conn.send(&msg).await.unwrap();
    conn.recv().await.unwrap().expect("reply")
}

fn error_of(reply: &Msg) -> Option<&str> {
    match reply {
        Msg::OpenResult { error, .. } => error.as_deref(),
        other => panic!("expected OpenResult, got {other:?}"),
    }
}

#[tokio::test]
async fn a_link_opens_http_and_https() {
    let h = Harness::start().await;
    let mut conn = h.connect_link("devbox").await;
    for url in ["http://example.com/a", "https://example.com/b?c=d"] {
        let reply = request(&mut conn, open(url)).await;
        assert_eq!(error_of(&reply), None, "{url}");
    }
    assert_eq!(
        h.opened(),
        ["http://example.com/a", "https://example.com/b?c=d"]
    );
}

#[tokio::test]
async fn other_schemes_are_rejected_and_not_opened() {
    let h = Harness::start().await;
    let mut conn = h.connect_link("devbox").await;
    for url in ["file:///etc/passwd", "javascript:alert(1)", "not a url"] {
        let reply = request(&mut conn, open(url)).await;
        assert!(error_of(&reply).is_some(), "{url}");
    }
    assert!(h.opened().is_empty());
    assert!(h.notifier.notices().is_empty());
}

#[tokio::test]
async fn a_link_uses_the_alias_of_its_saved_host() {
    let h = Harness::start().await;
    let mut conn = h.connect_link("devbox").await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/"]);
    let notices = h.notifier.notices();
    assert_eq!(notices[0].host, NoticeHost::Alias("devbox".into()));
    assert!(!notices[0].body().contains("unverified"));
}

#[tokio::test]
async fn ping_answers_pong_with_the_version_on_a_link() {
    let h = Harness::start().await;
    let mut link = h.connect_link("b").await;
    assert_eq!(
        request(&mut link, Msg::Ping).await,
        Msg::Pong {
            version: Hello::current().version(),
            rowser_version: Some(env!("CARGO_PKG_VERSION").into()),
        }
    );
}

#[tokio::test(start_paused = true)]
async fn rate_limit_allows_five_opens_per_ten_seconds_per_link() {
    let h = Harness::start().await;
    let mut conn = h.connect_link("devbox").await;
    for i in 0..5 {
        let reply = request(&mut conn, open(&format!("https://example.com/{i}"))).await;
        assert_eq!(error_of(&reply), None, "request {i}");
    }
    let reply = request(&mut conn, open("https://example.com/5")).await;
    assert!(error_of(&reply).is_some());

    // Another Link has its own budget.
    let mut other = h.connect_link("b").await;
    let reply = request(&mut other, open("https://example.com/other")).await;
    assert_eq!(error_of(&reply), None);

    tokio::time::advance(Duration::from_secs(10)).await;
    let reply = request(&mut conn, open("https://example.com/6")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened().len(), 7);
    assert!(!h.opened().contains(&"https://example.com/5".to_string()));
}

#[tokio::test(start_paused = true)]
async fn rate_limit_counts_rejected_scheme_requests() {
    let h = Harness::start().await;
    let mut conn = h.connect_link("devbox").await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert_eq!(error_of(&reply), None);
    let reply = request(&mut conn, open("file:///etc/passwd")).await;
    assert!(!error_of(&reply).unwrap().contains("rate limit"));
    for i in 0..3 {
        let reply = request(&mut conn, open(&format!("http://example.com/{i}"))).await;
        assert_eq!(error_of(&reply), None, "request {i}");
    }
    let reply = request(&mut conn, open("http://example.com/last")).await;
    assert!(error_of(&reply).unwrap().contains("rate limit"));
    assert_eq!(h.opened().len(), 4);
}

#[tokio::test]
async fn notice_body_strips_control_characters_and_escapes_markup() {
    let h = Harness::start().await;
    let mut conn = h.connect_link("devbox").await;
    request(&mut conn, open("https://example.com/?a=1&b=2\n<b>x</b>")).await;
    let body = h.notifier.notices()[0].body();
    assert_eq!(
        body,
        "From devbox: https://example.com/?a=1&amp;b=2%3Cb%3Ex%3C/b%3E"
    );
}

/// A Daemon with `--confirm-hosts` patterns and a scripted Notifier answer.
async fn confirm_harness(patterns: &[&str], answer: Answer) -> Harness<ScriptedNotifier> {
    let config = DaemonConfig {
        confirm_hosts: patterns.iter().map(|p| p.to_string()).collect(),
        ..DaemonConfig::default()
    };
    Harness::with(config, ScriptedNotifier::new(answer)).await
}

fn routes(text: &str) -> Routes {
    Routes::from_config(&rowser::config::parse(text).unwrap())
}

fn routed_harness(
    text: &str,
    answer: Answer,
) -> impl std::future::Future<Output = Harness<ScriptedNotifier>> {
    let config = DaemonConfig {
        routes: routes(text),
        ..DaemonConfig::default()
    };
    Harness::with(config, ScriptedNotifier::new(answer))
}

const G14: &str = "[[route]]\nhost = \"g14\"\nopen = \"system\"\n\n\
                   [[route]]\nhost = \"g14\"\nmatch = [\"*.google.com\"]\nopen = \"system\"\nask = true\n\n\
                   [default]\nopen = \"chrome:Work\"\n";

#[tokio::test]
async fn a_host_alias_reaches_the_opener_with_its_routes_target() {
    let h = routed_harness(G14, Answer::Deny).await;
    let mut conn = h.connect_link("g14").await;
    assert_eq!(
        error_of(&request(&mut conn, open("https://example.com/a")).await),
        None
    );
    assert_eq!(
        h.opener.opens(),
        [(Url::parse("https://example.com/a").unwrap(), Target::System)]
    );
    let notices = h.notifier.notices();
    assert_eq!(notices[0].target, Some(Target::System));
    assert!(!notices[0].body().contains("Opens in"));
    assert!(notices[0].warnings.is_empty());
    assert!(h.notifier.confirms().is_empty());
}

/// Linux browser roots under `t` with `google-chrome.desktop` and Chrome
/// Profiles `Personal (Default)` and `Work (Profile 2)`.
fn chrome_roots(t: &Path) -> rowser::browsers::Roots {
    let apps = t.join("share/applications");
    std::fs::create_dir_all(&apps).unwrap();
    std::fs::write(
        apps.join("google-chrome.desktop"),
        "[Desktop Entry]\nName=Google Chrome\nExec=/fake/chrome %U\n\
         MimeType=x-scheme-handler/https;\n",
    )
    .unwrap();
    let state = t.join("config/google-chrome");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("Local State"),
        r#"{"profile":{"info_cache":{"Default":{"name":"Personal"},"Profile 2":{"name":"Work"}}}}"#,
    )
    .unwrap();
    rowser::browsers::Roots {
        os: rowser::browsers::Os::Linux,
        app_dirs: vec![],
        data_dirs: vec![t.join("share")],
        home: t.join("home"),
        config_dir: t.join("config"),
    }
}

#[tokio::test]
async fn a_route_to_a_found_browser_reaches_the_opener_with_the_resolved_target() {
    let roots_dir = TmpDir::new("br");
    let text = "[[route]]\nhost = \"g14\"\nopen = \"chrome:Work\"\n\
                [[route]]\nhost = \"db1\"\nopen = \"firefox:Dev\"\n\
                [default]\nopen = \"chrome\"\n";
    let config = DaemonConfig {
        routes: routes(text),
        browser_roots: Some(chrome_roots(&roots_dir.0)),
        ..DaemonConfig::default()
    };
    let h = Harness::with(config, RecordingNotifier::default()).await;
    let mut conn = h.connect_link("g14").await;
    assert_eq!(
        error_of(&request(&mut conn, open("https://example.com/a")).await),
        None
    );
    let mut conn = h.connect_link("db1").await;
    assert_eq!(
        error_of(&request(&mut conn, open("https://example.com/b")).await),
        None
    );
    let chrome = |p: Option<&str>| Target::Browser {
        browser: "chrome".into(),
        profile: p.map(str::to_owned),
    };
    assert_eq!(
        h.opener.opens(),
        [
            (
                Url::parse("https://example.com/a").unwrap(),
                chrome(Some("Profile 2"))
            ),
            (Url::parse("https://example.com/b").unwrap(), chrome(None)),
        ]
    );
    let notices = h.notifier.notices();
    assert!(notices[0].warnings.is_empty());
    assert_eq!(
        notices[1].warnings,
        [
            "target `firefox:Dev` is not available (browser `firefox` is not installed); \
          fell back to [default] `chrome`"
        ]
    );
}

#[test]
fn command_opener_runs_the_browser_argv_without_a_shell() {
    let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TmpDir::new("bo");
    let log = dir.0.join("argv.log");
    let script = dir.0.join("fake-chrome");
    write_script(
        &script,
        &format!(
            "for a in \"$@\"; do printf '%s\\n' \"$a\" >> {}; done",
            log.display()
        ),
    );
    let roots = chrome_roots(&dir.0);
    std::fs::write(
        roots.data_dirs[0].join("applications/google-chrome.desktop"),
        format!(
            "[Desktop Entry]\nExec={} %U\nMimeType=x-scheme-handler/https;\n",
            script.display()
        ),
    )
    .unwrap();
    let opener = CommandOpener::new("/nonexistent/rowser-open {url}")
        .unwrap()
        .with_browsers(roots);
    let url = Url::parse("https://example.com/x?a=1;b=$(id)").unwrap();
    let target = Target::Browser {
        browser: "chrome".into(),
        profile: Some("Profile 2".into()),
    };
    opener.open(&url, &target).unwrap();
    let want = format!("--profile-directory=Profile 2\n{url}\n");
    for _ in 0..100 {
        if std::fs::read_to_string(&log).unwrap_or_default() == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{:?}", std::fs::read_to_string(&log));
}

#[tokio::test]
async fn an_unmatched_alias_goes_to_default_and_a_browser_target_falls_back_with_a_warning() {
    let h = routed_harness(G14, Answer::Deny).await;
    let mut conn = h.connect_link("db1").await;
    assert_eq!(
        error_of(&request(&mut conn, open("https://example.com/b")).await),
        None
    );
    assert_eq!(h.opener.opens()[0].1, Target::System);
    let body = h.notifier.notices()[0].body();
    assert!(
        body.contains(
            "target `chrome:Work` is not available (browser `chrome` is not installed); \
             fell back to `system`"
        ),
        "{body}"
    );
}

#[tokio::test]
async fn first_route_wins_so_ask_true_on_a_later_route_is_not_used() {
    let h = routed_harness(G14, Answer::Deny).await;
    let mut conn = h.connect_link("g14").await;
    assert_eq!(
        error_of(&request(&mut conn, open("https://mail.google.com/")).await),
        None
    );
    assert!(h.notifier.confirms().is_empty());
}

#[tokio::test]
async fn ask_true_on_a_route_waits_for_the_user() {
    let text = "[[route]]\nhost = \"g14\"\nopen = \"system\"\nask = true\n";
    let h = routed_harness(text, Answer::Deny).await;
    let mut conn = h.connect_link("g14").await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
    assert!(h.opened().is_empty());
    assert_eq!(h.notifier.confirms().len(), 1);
}

#[tokio::test]
async fn an_ask_target_opens_in_system_after_open() {
    let text = "[[route]]\nhost = \"g14\"\nopen = \"ask\"\n[default]\nopen = \"ask\"\n";
    let h = routed_harness(text, Answer::Approve).await;
    let mut conn = h.connect_link("g14").await;
    assert_eq!(
        error_of(&request(&mut conn, open("https://example.com/")).await),
        None
    );
    assert_eq!(h.opener.opens()[0].1, Target::System);
    assert_eq!(h.notifier.confirms().len(), 1);
}

#[tokio::test]
async fn a_browser_target_on_a_route_falls_back_to_an_ask_default_and_waits() {
    let text = "[[route]]\nhost = \"g14\"\nopen = \"chrome:Work\"\n[default]\nopen = \"ask\"\n";
    let h = routed_harness(text, Answer::Deny).await;
    let mut conn = h.connect_link("g14").await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
    assert!(h.opened().is_empty());
}

#[tokio::test]
async fn a_browser_target_falls_back_with_a_warning_in_the_notice() {
    let h = routed_harness(
        "[[route]]\nhost = \"g14\"\nopen = \"safari\"\n",
        Answer::Approve,
    )
    .await;
    let mut conn = h.connect_link("g14").await;
    request(&mut conn, open("https://example.com/")).await;
    let body = h.notifier.notices()[0].body();
    assert!(body.contains("target `safari` is not available"), "{body}");
}

#[tokio::test]
async fn confirm_hosts_asks_first_on_top_of_a_route() {
    let config = DaemonConfig {
        confirm_hosts: vec!["g14".into()],
        routes: routes(G14),
        ..DaemonConfig::default()
    };
    let h = Harness::with(config, ScriptedNotifier::new(Answer::Deny)).await;
    let mut conn = h.connect_link("g14").await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
}

/// Runs `rowser daemon` with `XDG_CONFIG_HOME` in `dir` and gives its exit
/// code and stderr. Only for a config that stops the Daemon.
fn daemon_with_config(dir: &Path, text: &str) -> (Option<i32>, String) {
    let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let conf = dir.join("config");
    std::fs::create_dir_all(conf.join("rowser")).unwrap();
    std::fs::write(conf.join("rowser").join("config.toml"), text).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["daemon", "--runtime-dir"])
        .arg(dir.join("run"))
        .arg("--test-confirm")
        .arg("deny")
        .env("XDG_CONFIG_HOME", &conf)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/nobus", dir.display()),
        )
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_invalid_config_file_stops_the_daemon_with_the_file_and_line() {
    let dir = TmpDir::new("d");
    let (code, stderr) =
        daemon_with_config(&dir.0, "[[route]]\nhost = \"g14\"\nopen = \"chrome:\"\n");
    assert_eq!(code, Some(1));
    let file = dir.0.join("config").join("rowser").join("config.toml");
    assert!(
        stderr.contains(&format!(
            "{}: line 3: invalid target `chrome:`",
            file.display()
        )),
        "{stderr}"
    );
    assert!(
        !dir.0.join("run").exists(),
        "no sockets for an invalid file"
    );
}

#[tokio::test]
async fn confirmed_host_opens_after_the_user_approves() {
    let h = confirm_harness(&["*.prod"], Answer::Approve).await;
    let mut conn = h.connect_link("db.prod").await;
    let reply = request(&mut conn, open("https://example.com/ok")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/ok"]);
    let asked = h.notifier.confirms();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].host, NoticeHost::Alias("db.prod".into()));
}

#[tokio::test]
async fn confirmed_host_is_not_opened_when_the_user_denies() {
    let h = confirm_harness(&["db.prod"], Answer::Deny).await;
    let mut conn = h.connect_link("DB.prod").await;
    let reply = request(&mut conn, open("https://example.com/no")).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
    assert!(h.opened().is_empty());
    assert_eq!(h.notifier.confirms().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn confirmed_host_is_not_opened_after_the_timeout() {
    let h = confirm_harness(&["db*"], Answer::Hang).await;
    let mut conn = h.connect_link("db1").await;
    let asked = tokio::spawn(async move {
        let reply = request(&mut conn, open("https://example.com/late")).await;
        error_of(&reply).map(str::to_owned)
    });
    // A running spawn_blocking task stops the paused clock from moving on its
    // own, so wait for the confirmation and then move the clock by hand.
    while h.notifier.confirms().is_empty() {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(!asked.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        asked.await.unwrap().as_deref(),
        Some("no confirmation within 60 seconds; the URL was not opened")
    );
    // A late approval does not open the URL.
    h.notifier.release(true);
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(h.opened().is_empty());
}

#[tokio::test]
async fn non_matching_host_opens_immediately() {
    let h = confirm_harness(&["*.prod"], Answer::Deny).await;
    let mut conn = h.connect_link("devbox").await;
    let reply = request(&mut conn, open("https://example.com/now")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/now"]);
    assert!(h.notifier.confirms().is_empty());
    assert_eq!(h.notifier.notices().len(), 1);
}

#[tokio::test]
async fn bind_refuses_an_existing_directory_open_to_others() {
    let dir = TmpDir::new("d");
    let run = dir.0.join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = bind_sockets(&run).unwrap_err();
    assert!(err.to_string().contains("0700"), "{err}");
    assert_eq!(mode(&run), 0o755, "the directory is not changed");
    assert!(!run.join(CONTROL_SOCKET).exists());
}

#[tokio::test]
async fn bind_accepts_an_existing_private_directory() {
    let dir = TmpDir::new("d");
    let run = dir.0.join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _listener = bind_sockets(&run).unwrap();
    assert_eq!(mode(&run.join(CONTROL_SOCKET)), 0o600);
}

#[tokio::test]
async fn the_control_socket_is_0600_in_a_0700_directory() {
    let dir = TmpDir::new("d");
    let run = dir.0.join("run");
    let _listener = bind_sockets(&run).unwrap();
    assert_eq!(mode(&run), 0o700);
    assert_eq!(mode(&run.join(CONTROL_SOCKET)), 0o600);
}

/// Held by tests that start child processes, and by the stale control socket
/// test: a fork in another test thread holds a copy of a closed listener
/// until exec, so the old socket can look live for a moment.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn bind_replaces_a_stale_control_socket_but_not_a_live_one() {
    let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TmpDir::new("d");
    let run = dir.0.join("run");
    drop(bind_sockets(&run).unwrap());
    let live = bind_sockets(&run).expect("a stale control socket is replaced");
    assert!(bind_sockets(&run).is_err(), "a live daemon is not replaced");
    drop(live);
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn command_opener_passes_the_url_as_one_argument() {
    let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TmpDir::new("d");
    let script = dir.0.join("rec.sh");
    write_script(&script, "printf '%s\\n' \"$#\" \"$1\" > \"$0.out\"");
    let pwned = dir.0.join("pwned");
    let raw = format!(
        "https://example.com/a b?x=1&y=$(touch {p});touch {p}|`touch {p}`'\"",
        p = pwned.display()
    );
    let url = Url::parse(&raw).unwrap();

    let opener = CommandOpener::new(&format!("{} {{url}}", script.display())).unwrap();
    opener.open(&url, &Target::System).unwrap();

    let out_path = dir.0.join("rec.sh.out");
    let mut out = String::new();
    for _ in 0..100 {
        out = std::fs::read_to_string(&out_path).unwrap_or_default();
        if out.ends_with('\n') && out.lines().count() == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(out, format!("1\n{}\n", url.as_str()));
    assert!(!pwned.exists());
}

#[test]
fn command_opener_returns_without_waiting_for_the_command_to_exit() {
    let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TmpDir::new("d");
    let script = dir.0.join("slow.sh");
    let go = dir.0.join("go");
    let done = dir.0.join("done");
    // Wait (at most about 10 s) for `go`, then touch `done`.
    write_script(
        &script,
        &format!(
            "i=0\nwhile [ ! -e '{go}' ] && [ $i -lt 500 ]; do sleep 0.02; i=$((i+1)); done\n\
             touch '{done}'",
            go = go.display(),
            done = done.display()
        ),
    );
    let opener = CommandOpener::new(&format!("{} {{url}}", script.display())).unwrap();
    let start = std::time::Instant::now();
    opener
        .open(
            &Url::parse("https://example.com/").unwrap(),
            &Target::System,
        )
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    std::fs::write(&go, "").unwrap();
    // Keep the dir until the command ends, so it never runs a deleted script.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !done.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(done.exists(), "the slow command did not finish");
}

#[test]
fn command_opener_reports_a_command_that_cannot_start() {
    let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let opener = CommandOpener::new("/nonexistent/rowser-opener {url}").unwrap();
    assert!(opener
        .open(
            &Url::parse("https://example.com/").unwrap(),
            &Target::System
        )
        .is_err());
    assert!(CommandOpener::new("   ").is_err());
}

#[test]
fn command_opener_rejects_url_in_the_program_word() {
    assert!(CommandOpener::new("{url}").is_err());
    assert!(CommandOpener::new("/usr/bin/{url} --new-tab").is_err());
    assert!(CommandOpener::new("xdg-open {url}").is_ok());
}

/// A free loopback port: bind port 0, read it, release it.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The local ports of the running forwards, from `rowser status`.
async fn forward_ports(run: &Path) -> Vec<u16> {
    let stream = UnixStream::connect(run.join(CONTROL_SOCKET)).await.unwrap();
    let mut control = Conn::connect(stream, Hello::current()).await.unwrap();
    match request(&mut control, Msg::Status).await {
        Msg::StatusReply { forwards, .. } => forwards.iter().map(|f| f.port).collect(),
        other => panic!("unexpected {other:?}"),
    }
}

fn open_with_port(url: &str, port: u16) -> Msg {
    Msg::Open {
        url: url.into(),
        callback_ports: vec![port],
    }
}

async fn forward_confirm_harness(answer: Answer) -> Harness<ScriptedNotifier> {
    let config = DaemonConfig {
        confirm_hosts: vec!["*.prod".into()],
        loopback: vec!["127.0.0.1".parse().unwrap()],
        ..DaemonConfig::default()
    };
    Harness::with(config, ScriptedNotifier::new(answer)).await
}

#[tokio::test]
async fn denied_url_with_callback_port_binds_no_forward() {
    let h = forward_confirm_harness(Answer::Deny).await;
    let port = free_port();
    let mut conn = h.connect_link("db.prod").await;
    let reply = request(&mut conn, open_with_port("https://example.com/", port)).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
    assert!(matches!(
        reply,
        Msg::OpenResult {
            forward_id: None,
            ..
        }
    ));
    // No forward holds the local port. (A bind check of the port can fail
    // when another socket takes the port, so ask `rowser status`.)
    assert_eq!(forward_ports(&h.run()).await, [] as [u16; 0], "port {port}");
}

/// The warning for a Callback port on a test Link, which has no yamux
/// session. Forwards through a real Link: tests/callback.rs.
fn no_link_warning(port: u16) -> String {
    format!("callback forwards need a Link to a Saved host; port {port} is not forwarded")
}

/// Expects `reply` to be accepted with no forward and the no-Link warning.
fn assert_no_link_forward(reply: &Msg, port: u16) {
    match reply {
        Msg::OpenResult {
            forward_id: None,
            warnings,
            error: None,
        } => assert_eq!(warnings, &[no_link_warning(port)]),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn approved_url_with_callback_port_on_a_test_link_gets_no_forward() {
    let h = forward_confirm_harness(Answer::Approve).await;
    let port = free_port();
    let mut conn = h.connect_link("db.prod").await;
    let reply = request(&mut conn, open_with_port("https://example.com/", port)).await;
    assert_no_link_forward(&reply, port);
    assert_eq!(forward_ports(&h.run()).await, [] as [u16; 0]);
}

fn manual_forward(port: u16) -> Msg {
    Msg::Forward {
        callback_ports: vec![port],
    }
}

#[tokio::test]
async fn denied_manual_forward_binds_nothing() {
    let h = forward_confirm_harness(Answer::Deny).await;
    let port = free_port();
    let mut conn = h.connect_link("db.prod").await;
    let reply = request(&mut conn, manual_forward(port)).await;
    assert_eq!(error_of(&reply), Some(FORWARD_NOT_APPROVED));
    assert_eq!(forward_ports(&h.run()).await, [] as [u16; 0]);
    let asked = h.notifier.confirms();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].host, NoticeHost::Alias("db.prod".into()));
    let body = asked[0].body();
    assert!(
        body.contains("db.prod") && body.contains(&port.to_string()),
        "{body}"
    );
}

#[tokio::test]
async fn approved_manual_forward_on_a_test_link_gets_no_forward_and_a_notice() {
    let h = forward_confirm_harness(Answer::Approve).await;
    let port = free_port();
    let mut conn = h.connect_link("db.prod").await;
    let reply = request(&mut conn, manual_forward(port)).await;
    assert_no_link_forward(&reply, port);
    assert_eq!(h.notifier.confirms().len(), 1);
    // The notice with the warning is sent off the request path.
    for _ in 0..100 {
        if !h.notifier.notices().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let notices = h.notifier.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].warnings, [no_link_warning(port)]);
}

#[tokio::test(start_paused = true)]
async fn manual_forward_binds_nothing_after_the_confirmation_timeout() {
    let h = forward_confirm_harness(Answer::Hang).await;
    let port = free_port();
    let mut conn = h.connect_link("db.prod").await;
    let asked = tokio::spawn(async move {
        let reply = request(&mut conn, manual_forward(port)).await;
        error_of(&reply).map(str::to_owned)
    });
    while h.notifier.confirms().is_empty() {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(60)).await;
    assert_eq!(
        asked.await.unwrap().as_deref(),
        Some("no confirmation within 60 seconds; the port was not forwarded")
    );
    h.notifier.release(true);
    assert_eq!(forward_ports(&h.run()).await, [] as [u16; 0], "port {port}");
}

#[tokio::test]
async fn manual_forward_from_a_non_matching_host_shows_a_notice_with_the_port() {
    let h = forward_confirm_harness(Answer::Deny).await;
    let port = free_port();
    let mut conn = h.connect_link("devbox").await;
    let reply = request(&mut conn, manual_forward(port)).await;
    assert_no_link_forward(&reply, port);
    assert!(h.notifier.confirms().is_empty());
    // The notice is sent off the request path.
    for _ in 0..100 {
        if !h.notifier.notices().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let notices = h.notifier.notices();
    assert_eq!(notices.len(), 1);
    let body = notices[0].body();
    assert!(
        body.contains("devbox") && body.contains(&port.to_string()),
        "{body}"
    );
}
