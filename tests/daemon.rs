mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use common::TmpDir;
use rowser::config::Target;
use rowser::daemon::testing::{Answer, RecordingNotifier, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{
    bind_sockets, CommandOpener, Daemon, DaemonConfig, NoticeHost, Notifier, Opener, DIRECT_SOCKET,
    FORWARD_NOT_APPROVED, NOT_APPROVED, RELAY_SOCKET,
};
use rowser::protocol::{Conn, Hello, Msg};
use rowser::routes::Routes;
use tokio::net::UnixStream;
use url::Url;

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
        let (direct, relay, control) = bind_sockets(&run).unwrap();
        let opener = RecordingOpener::default();
        let daemon = Daemon::new(config, opener.clone(), notifier.clone());
        tokio::spawn(daemon.serve(direct, relay, control));
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

    /// Connects to the relay socket and sends the Host stamp `alias`.
    async fn connect_relay(&self, alias: &str) -> Conn<UnixStream> {
        let mut conn = self.connect(RELAY_SOCKET).await;
        conn.send(&Msg::HostStamp {
            alias: alias.into(),
        })
        .await
        .unwrap();
        conn
    }

    fn opened(&self) -> Vec<String> {
        self.opener.urls().iter().map(|u| u.to_string()).collect()
    }
}

fn open(url: &str) -> Msg {
    Msg::Open {
        url: url.into(),
        reported_host: "devbox.internal".into(),
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
async fn direct_socket_opens_http_and_https() {
    let h = Harness::start().await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
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
    let mut conn = h.connect(DIRECT_SOCKET).await;
    for url in ["file:///etc/passwd", "javascript:alert(1)", "not a url"] {
        let reply = request(&mut conn, open(url)).await;
        assert!(error_of(&reply).is_some(), "{url}");
    }
    assert!(h.opened().is_empty());
    assert!(h.notifier.notices().is_empty());
}

#[tokio::test]
async fn direct_socket_notice_marks_the_reported_host_as_unverified() {
    let h = Harness::start().await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    request(&mut conn, open("https://example.com/")).await;
    let notices = h.notifier.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].host,
        NoticeHost::Reported("devbox.internal".into())
    );
    assert!(notices[0].body().contains("devbox.internal (unverified)"));
}

#[tokio::test]
async fn direct_socket_rejects_a_host_stamp() {
    let h = Harness::start().await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let reply = request(
        &mut conn,
        Msg::HostStamp {
            alias: "devbox".into(),
        },
    )
    .await;
    assert!(error_of(&reply).is_some());
    // The daemon closes the connection after the rejection.
    assert!(matches!(conn.recv().await, Ok(None) | Err(_)));
    assert!(h.opened().is_empty());
}

#[tokio::test]
async fn relay_socket_uses_the_host_alias_from_the_stamp() {
    let h = Harness::start().await;
    let mut conn = h.connect_relay("devbox").await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/"]);
    let notices = h.notifier.notices();
    assert_eq!(notices[0].host, NoticeHost::Alias("devbox".into()));
    assert!(!notices[0].body().contains("unverified"));
}

#[tokio::test]
async fn relay_socket_rejects_a_connection_without_a_stamp() {
    let h = Harness::start().await;
    let mut conn = h.connect(RELAY_SOCKET).await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert!(error_of(&reply).is_some());
    assert!(matches!(conn.recv().await, Ok(None) | Err(_)));
    assert!(h.opened().is_empty());
}

#[tokio::test]
async fn ping_answers_pong_with_the_version_on_both_sockets() {
    let h = Harness::start().await;
    let mut direct = h.connect(DIRECT_SOCKET).await;
    assert_eq!(
        request(&mut direct, Msg::Ping).await,
        Msg::Pong {
            version: Hello::current().version()
        }
    );
    let mut relay = h.connect_relay("b").await;
    assert_eq!(
        request(&mut relay, Msg::Ping).await,
        Msg::Pong {
            version: Hello::current().version()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn rate_limit_allows_five_opens_per_ten_seconds_per_connection() {
    let h = Harness::start().await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    for i in 0..5 {
        let reply = request(&mut conn, open(&format!("https://example.com/{i}"))).await;
        assert_eq!(error_of(&reply), None, "request {i}");
    }
    let reply = request(&mut conn, open("https://example.com/5")).await;
    assert!(error_of(&reply).is_some());

    // Another connection has its own budget.
    let mut other = h.connect(DIRECT_SOCKET).await;
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
    let mut conn = h.connect(DIRECT_SOCKET).await;
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
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let forged = Msg::Open {
        url: "https://example.com/?a=1&b=2".into(),
        reported_host: "devbox\n<b>trusted</b> <a href=\"x\">\u{202e}".into(),
        callback_ports: vec![],
    };
    request(&mut conn, forged).await;
    let body = h.notifier.notices()[0].body();
    assert_eq!(
        body,
        "From devbox&lt;b&gt;trusted&lt;/b&gt; &lt;a href=&quot;x&quot;&gt; (unverified): \
         https://example.com/?a=1&amp;b=2"
    );
}

/// A Daemon with `--confirm-hosts` patterns and a scripted Notifier answer.
async fn confirm_harness(
    patterns: &[&str],
    unverified: bool,
    answer: Answer,
) -> Harness<ScriptedNotifier> {
    let config = DaemonConfig {
        confirm_hosts: patterns.iter().map(|p| p.to_string()).collect(),
        confirm_unverified: unverified,
        // Without this, `[unverified]` asks by default; these tests check
        // `--confirm-hosts` alone.
        routes: routes("[unverified]\nopen = \"system\"\n"),
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
    let mut conn = h.connect_relay("g14").await;
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

#[tokio::test]
async fn an_unmatched_alias_goes_to_default_and_a_browser_target_falls_back_with_a_warning() {
    let h = routed_harness(G14, Answer::Deny).await;
    let mut conn = h.connect_relay("db1").await;
    assert_eq!(
        error_of(&request(&mut conn, open("https://example.com/b")).await),
        None
    );
    assert_eq!(h.opener.opens()[0].1, Target::System);
    let body = h.notifier.notices()[0].body();
    assert!(
        body.contains("target `chrome:Work` is not available yet; opened with `system`"),
        "{body}"
    );
}

#[tokio::test]
async fn first_route_wins_so_ask_true_on_a_later_route_is_not_used() {
    let h = routed_harness(G14, Answer::Deny).await;
    let mut conn = h.connect_relay("g14").await;
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
    let mut conn = h.connect_relay("g14").await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
    assert!(h.opened().is_empty());
    assert_eq!(h.notifier.confirms().len(), 1);
}

#[tokio::test]
async fn an_ask_target_opens_in_system_after_open() {
    let text = "[[route]]\nhost = \"g14\"\nopen = \"ask\"\n[default]\nopen = \"ask\"\n";
    let h = routed_harness(text, Answer::Approve).await;
    let mut conn = h.connect_relay("g14").await;
    assert_eq!(
        error_of(&request(&mut conn, open("https://example.com/")).await),
        None
    );
    assert_eq!(h.opener.opens()[0].1, Target::System);
    assert_eq!(h.notifier.confirms().len(), 1);
}

#[tokio::test]
async fn a_browser_target_on_unverified_falls_back_to_an_ask_default_and_waits() {
    let text = "[unverified]\nopen = \"safari\"\n[default]\nopen = \"ask\"\n";
    let h = routed_harness(text, Answer::Deny).await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let msg = Msg::Open {
        url: "https://example.com/".into(),
        reported_host: "devbox".into(),
        callback_ports: vec![],
    };
    assert_eq!(error_of(&request(&mut conn, msg).await), Some(NOT_APPROVED));
    assert!(h.opened().is_empty());
    assert_eq!(h.notifier.confirms().len(), 1);
}

#[tokio::test]
async fn a_browser_target_on_a_route_falls_back_to_an_ask_default_and_waits() {
    let text = "[[route]]\nhost = \"g14\"\nopen = \"chrome:Work\"\n[default]\nopen = \"ask\"\n";
    let h = routed_harness(text, Answer::Deny).await;
    let mut conn = h.connect_relay("g14").await;
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
    let mut conn = h.connect_relay("g14").await;
    request(&mut conn, open("https://example.com/")).await;
    let body = h.notifier.notices()[0].body();
    assert!(
        body.contains("target `safari` is not available yet"),
        "{body}"
    );
}

#[tokio::test]
async fn a_reported_host_named_like_a_route_never_uses_that_route() {
    let h = routed_harness(G14, Answer::Deny).await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let msg = Msg::Open {
        url: "https://example.com/".into(),
        reported_host: "g14".into(),
        callback_ports: vec![],
    };
    // `[unverified]` defaults to `ask`; the `g14` Route would open at once.
    assert_eq!(error_of(&request(&mut conn, msg).await), Some(NOT_APPROVED));
    assert!(h.opened().is_empty());
    assert_eq!(h.notifier.confirms().len(), 1);
}

#[tokio::test]
async fn confirm_hosts_asks_first_on_top_of_a_route() {
    let config = DaemonConfig {
        confirm_hosts: vec!["g14".into()],
        routes: routes(G14),
        ..DaemonConfig::default()
    };
    let h = Harness::with(config, ScriptedNotifier::new(Answer::Deny)).await;
    let mut conn = h.connect_relay("g14").await;
    let reply = request(&mut conn, open("https://example.com/")).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
}

/// Runs `rowser daemon` with `XDG_CONFIG_HOME` in `dir` and gives its exit
/// code and stderr. Only for a config that stops the Daemon.
fn daemon_with_config(dir: &Path, text: &str) -> (Option<i32>, String) {
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
    let h = confirm_harness(&["*.prod"], false, Answer::Approve).await;
    let mut conn = h.connect_relay("db.prod").await;
    let reply = request(&mut conn, open("https://example.com/ok")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/ok"]);
    let asked = h.notifier.confirms();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].host, NoticeHost::Alias("db.prod".into()));
}

#[tokio::test]
async fn confirmed_host_is_not_opened_when_the_user_denies() {
    let h = confirm_harness(&["db.prod"], false, Answer::Deny).await;
    let mut conn = h.connect_relay("DB.prod").await;
    let reply = request(&mut conn, open("https://example.com/no")).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
    assert!(h.opened().is_empty());
    assert_eq!(h.notifier.confirms().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn confirmed_host_is_not_opened_after_the_timeout() {
    let h = confirm_harness(&["db*"], false, Answer::Hang).await;
    let mut conn = h.connect_relay("db1").await;
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
    let h = confirm_harness(&["*.prod"], false, Answer::Deny).await;
    let mut conn = h.connect_relay("devbox").await;
    let reply = request(&mut conn, open("https://example.com/now")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/now"]);
    assert!(h.notifier.confirms().is_empty());
    assert_eq!(h.notifier.notices().len(), 1);
}

#[tokio::test]
async fn direct_socket_ignores_confirm_hosts_without_confirm_unverified() {
    let h = confirm_harness(&["devbox.*"], false, Answer::Deny).await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let reply = request(&mut conn, open("https://example.com/d")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/d"]);
    assert!(h.notifier.confirms().is_empty());
}

#[tokio::test]
async fn confirm_unverified_matches_the_reported_host_name() {
    let h = confirm_harness(&["devbox.*"], true, Answer::Deny).await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let reply = request(&mut conn, open("https://example.com/d")).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
    assert!(h.opened().is_empty());
    let asked = h.notifier.confirms();
    assert_eq!(
        asked[0].host,
        NoticeHost::Reported("devbox.internal".into())
    );
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
    assert!(!run.join(DIRECT_SOCKET).exists());
}

#[tokio::test]
async fn bind_accepts_an_existing_private_directory() {
    let dir = TmpDir::new("d");
    let run = dir.0.join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _listeners = bind_sockets(&run).unwrap();
    assert_eq!(mode(&run.join(DIRECT_SOCKET)), 0o600);
}

#[tokio::test]
async fn sockets_are_0600_in_a_0700_directory() {
    let dir = TmpDir::new("d");
    let run = dir.0.join("run");
    let _listeners = bind_sockets(&run).unwrap();
    assert_eq!(mode(&run), 0o700);
    assert_eq!(mode(&run.join(DIRECT_SOCKET)), 0o600);
    assert_eq!(mode(&run.join(RELAY_SOCKET)), 0o600);
}

/// Held by tests that start child processes, and by the Stale socket test: a
/// fork in another test thread holds a copy of a closed listener until exec,
/// so the Stale socket can look live for a moment.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn bind_replaces_a_stale_socket_but_not_a_live_one() {
    let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TmpDir::new("d");
    let run = dir.0.join("run");
    drop(bind_sockets(&run).unwrap());
    let live = bind_sockets(&run).expect("stale sockets are replaced");
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
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$1\" > \"$0.out\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ni=0\nwhile [ ! -e '{go}' ] && [ $i -lt 500 ]; do sleep 0.02; i=$((i+1)); done\n\
             touch '{done}'\n",
            go = go.display(),
            done = done.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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

fn open_with_port(url: &str, port: u16) -> Msg {
    Msg::Open {
        url: url.into(),
        reported_host: "devbox.internal".into(),
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
    let mut conn = h.connect_relay("db.prod").await;
    let reply = request(&mut conn, open_with_port("https://example.com/", port)).await;
    assert_eq!(error_of(&reply), Some(NOT_APPROVED));
    assert!(matches!(
        reply,
        Msg::OpenResult {
            forward_id: None,
            ..
        }
    ));
    // The local port is still free: no forward holds it.
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
}

#[tokio::test]
async fn approved_url_with_callback_port_gets_a_forward() {
    let h = forward_confirm_harness(Answer::Approve).await;
    let port = free_port();
    let mut conn = h.connect_relay("db.prod").await;
    let reply = request(&mut conn, open_with_port("https://example.com/", port)).await;
    assert_eq!(error_of(&reply), None);
    assert!(matches!(
        reply,
        Msg::OpenResult {
            forward_id: Some(_),
            ..
        }
    ));
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());
}

fn manual_forward(port: u16) -> Msg {
    Msg::Forward {
        reported_host: "devbox.internal".into(),
        callback_ports: vec![port],
    }
}

#[tokio::test]
async fn denied_manual_forward_binds_nothing() {
    let h = forward_confirm_harness(Answer::Deny).await;
    let port = free_port();
    let mut conn = h.connect_relay("db.prod").await;
    let reply = request(&mut conn, manual_forward(port)).await;
    assert_eq!(error_of(&reply), Some(FORWARD_NOT_APPROVED));
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
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
async fn approved_manual_forward_binds_the_port() {
    let h = forward_confirm_harness(Answer::Approve).await;
    let port = free_port();
    let mut conn = h.connect_relay("db.prod").await;
    let reply = request(&mut conn, manual_forward(port)).await;
    assert_eq!(error_of(&reply), None);
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());
    assert_eq!(h.notifier.confirms().len(), 1);
    assert!(h.notifier.notices().is_empty());
}

#[tokio::test(start_paused = true)]
async fn manual_forward_binds_nothing_after_the_confirmation_timeout() {
    let h = forward_confirm_harness(Answer::Hang).await;
    let port = free_port();
    let mut conn = h.connect_relay("db.prod").await;
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
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
}

#[tokio::test]
async fn manual_forward_from_a_non_matching_host_shows_a_notice_with_the_port() {
    let h = forward_confirm_harness(Answer::Deny).await;
    let port = free_port();
    let mut conn = h.connect_relay("devbox").await;
    let reply = request(&mut conn, manual_forward(port)).await;
    assert_eq!(error_of(&reply), None);
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

#[tokio::test]
async fn manual_forward_on_the_direct_socket_matches_the_reported_host_with_confirm_unverified() {
    let config = DaemonConfig {
        confirm_hosts: vec!["devbox.*".into()],
        confirm_unverified: true,
        loopback: vec!["127.0.0.1".parse().unwrap()],
        ..DaemonConfig::default()
    };
    let h = Harness::with(config, ScriptedNotifier::new(Answer::Deny)).await;
    let port = free_port();
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let reply = request(&mut conn, manual_forward(port)).await;
    assert_eq!(error_of(&reply), Some(FORWARD_NOT_APPROVED));
    assert_eq!(
        h.notifier.confirms()[0].host,
        NoticeHost::Reported("devbox.internal".into())
    );
}

struct FailingOpener;

impl Opener for FailingOpener {
    fn open(&self, _: &Url, _: &Target) -> std::io::Result<()> {
        Err(std::io::Error::other("no browser"))
    }
}

#[tokio::test]
async fn a_failed_open_closes_the_forward() {
    let dir = TmpDir::new("d");
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let config = DaemonConfig {
        loopback: vec!["127.0.0.1".parse().unwrap()],
        ..DaemonConfig::default()
    };
    let daemon = Daemon::new(config, FailingOpener, RecordingNotifier::default());
    tokio::spawn(daemon.serve(direct, relay, control));
    let stream = UnixStream::connect(run.join(DIRECT_SOCKET)).await.unwrap();
    let mut conn = Conn::connect(stream, Hello::current()).await.unwrap();
    let port = free_port();
    let reply = request(&mut conn, open_with_port("https://example.com/", port)).await;
    assert!(error_of(&reply).is_some());
    // The forward task can drop its listeners after the reply, so
    // wait (with a deadline) for the port to come free.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the failed open did not free local port {port}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
