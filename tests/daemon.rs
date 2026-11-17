use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rowser::daemon::testing::{Answer, RecordingNotifier, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{
    bind_sockets, CommandOpener, Daemon, DaemonConfig, NoticeHost, Opener, DIRECT_SOCKET,
    RELAY_SOCKET,
};
use rowser::protocol::{Conn, Hello, Msg};
use tokio::net::UnixStream;
use url::Url;

/// A short temp dir under /tmp (Unix socket paths are limited to ~104 bytes).
struct TmpDir(PathBuf);

impl TmpDir {
    fn new() -> TmpDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = PathBuf::from(format!("/tmp/rw.{}.{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    dir: TmpDir,
    opener: RecordingOpener,
    notifier: RecordingNotifier,
}

impl Harness {
    async fn start() -> Harness {
        let dir = TmpDir::new();
        let run = dir.0.join("run");
        let (direct, relay) = bind_sockets(&run).unwrap();
        let opener = RecordingOpener::default();
        let notifier = RecordingNotifier::default();
        let daemon = Daemon::new(DaemonConfig::default(), opener.clone(), notifier.clone());
        tokio::spawn(daemon.serve(direct, relay));
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
) -> (Harness, ScriptedNotifier) {
    let dir = TmpDir::new();
    let (direct, relay) = bind_sockets(&dir.0.join("run")).unwrap();
    let opener = RecordingOpener::default();
    let notifier = ScriptedNotifier::new(answer);
    let config = DaemonConfig {
        confirm_hosts: patterns.iter().map(|p| p.to_string()).collect(),
        confirm_unverified: unverified,
        confirm_timeout: Duration::from_secs(1),
        ..DaemonConfig::default()
    };
    let daemon = Daemon::new(config, opener.clone(), notifier.clone());
    tokio::spawn(daemon.serve(direct, relay));
    let h = Harness {
        dir,
        opener,
        notifier: RecordingNotifier::default(),
    };
    (h, notifier)
}

#[tokio::test]
async fn confirmed_host_opens_after_the_user_approves() {
    let (h, n) = confirm_harness(&["*.prod"], false, Answer::Approve).await;
    let mut conn = h.connect_relay("db.prod").await;
    let reply = request(&mut conn, open("https://example.com/ok")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/ok"]);
    let asked = n.confirms();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].host, NoticeHost::Alias("db.prod".into()));
}

#[tokio::test]
async fn confirmed_host_is_not_opened_when_the_user_denies() {
    let (h, n) = confirm_harness(&["db.prod"], false, Answer::Deny).await;
    let mut conn = h.connect_relay("DB.prod").await;
    let reply = request(&mut conn, open("https://example.com/no")).await;
    assert_eq!(error_of(&reply), Some("the user denied the URL"));
    assert!(h.opened().is_empty());
    assert_eq!(n.confirms().len(), 1);
}

#[tokio::test]
async fn confirmed_host_is_not_opened_after_the_timeout() {
    let (h, n) = confirm_harness(&["db*"], false, Answer::Hang).await;
    let mut conn = h.connect_relay("db1").await;
    let reply = request(&mut conn, open("https://example.com/late")).await;
    assert_eq!(
        error_of(&reply),
        Some("no confirmation within 1 seconds; the URL was not opened")
    );
    // A late approval does not open the URL.
    n.release(true);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.opened().is_empty());
}

#[tokio::test]
async fn non_matching_host_opens_immediately() {
    let (h, n) = confirm_harness(&["*.prod"], false, Answer::Deny).await;
    let mut conn = h.connect_relay("devbox").await;
    let reply = request(&mut conn, open("https://example.com/now")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/now"]);
    assert!(n.confirms().is_empty());
    assert_eq!(n.notices().len(), 1);
}

#[tokio::test]
async fn direct_socket_ignores_confirm_hosts_without_confirm_unverified() {
    let (h, n) = confirm_harness(&["devbox.*"], false, Answer::Deny).await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let reply = request(&mut conn, open("https://example.com/d")).await;
    assert_eq!(error_of(&reply), None);
    assert_eq!(h.opened(), ["https://example.com/d"]);
    assert!(n.confirms().is_empty());
}

#[tokio::test]
async fn confirm_unverified_matches_the_reported_host_name() {
    let (h, n) = confirm_harness(&["devbox.*"], true, Answer::Deny).await;
    let mut conn = h.connect(DIRECT_SOCKET).await;
    let reply = request(&mut conn, open("https://example.com/d")).await;
    assert_eq!(error_of(&reply), Some("the user denied the URL"));
    assert!(h.opened().is_empty());
    let asked = n.confirms();
    assert_eq!(
        asked[0].host,
        NoticeHost::Reported("devbox.internal".into())
    );
}

#[tokio::test]
async fn bind_refuses_an_existing_directory_open_to_others() {
    let dir = TmpDir::new();
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
    let dir = TmpDir::new();
    let run = dir.0.join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _listeners = bind_sockets(&run).unwrap();
    assert_eq!(mode(&run.join(DIRECT_SOCKET)), 0o600);
}

#[tokio::test]
async fn sockets_are_0600_in_a_0700_directory() {
    let dir = TmpDir::new();
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
    let dir = TmpDir::new();
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
    let dir = TmpDir::new();
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
    opener.open(&url).unwrap();

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
    let dir = TmpDir::new();
    let script = dir.0.join("slow.sh");
    std::fs::write(&script, "#!/bin/sh\nsleep 5\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let opener = CommandOpener::new(&format!("{} {{url}}", script.display())).unwrap();
    let start = std::time::Instant::now();
    opener
        .open(&Url::parse("https://example.com/").unwrap())
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn command_opener_reports_a_command_that_cannot_start() {
    let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let opener = CommandOpener::new("/nonexistent/rowser-opener {url}").unwrap();
    assert!(opener
        .open(&Url::parse("https://example.com/").unwrap())
        .is_err());
    assert!(CommandOpener::new("   ").is_err());
}

#[test]
fn command_opener_rejects_url_in_the_program_word() {
    assert!(CommandOpener::new("{url}").is_err());
    assert!(CommandOpener::new("/usr/bin/{url} --new-tab").is_err());
    assert!(CommandOpener::new("xdg-open {url}").is_ok());
}
