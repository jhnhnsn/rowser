use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
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
    let mut conn = h.connect(RELAY_SOCKET).await;
    conn.send(&Msg::HostStamp {
        alias: "devbox".into(),
    })
    .await
    .unwrap();
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
    let mut relay = h.connect(RELAY_SOCKET).await;
    relay
        .send(&Msg::HostStamp { alias: "b".into() })
        .await
        .unwrap();
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

#[tokio::test]
async fn sockets_are_0600_in_a_0700_directory() {
    let dir = TmpDir::new();
    let run = dir.0.join("run");
    let _listeners = bind_sockets(&run).unwrap();
    assert_eq!(mode(&run), 0o700);
    assert_eq!(mode(&run.join(DIRECT_SOCKET)), 0o600);
    assert_eq!(mode(&run.join(RELAY_SOCKET)), 0o600);
}

#[tokio::test]
async fn bind_replaces_a_stale_socket_but_not_a_live_one() {
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

    let out = std::fs::read_to_string(dir.0.join("rec.sh.out")).unwrap();
    assert_eq!(out, format!("1\n{}\n", url.as_str()));
    assert!(!pwned.exists());
}

#[test]
fn command_opener_reports_a_failed_command() {
    let opener = CommandOpener::new("false {url}").unwrap();
    assert!(opener
        .open(&Url::parse("https://example.com/").unwrap())
        .is_err());
    assert!(CommandOpener::new("   ").is_err());
}
