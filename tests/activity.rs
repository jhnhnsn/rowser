//! `rowser activity`: the Daemon's recent requests.

mod common;

use std::io::Write;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use common::TmpDir;
use rowser::control::{run_activity, ActivityArgs};
use rowser::daemon::testing::{Answer, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, Notifier, DIRECT_SOCKET, RELAY_SOCKET};
use rowser::protocol::{Conn, Hello, Msg};
use serde_json::Value;
use tokio::net::{TcpListener, UnixStream};

const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

async fn start_with(dir: &TmpDir, config: DaemonConfig, notifier: impl Notifier) -> PathBuf {
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let config = DaemonConfig {
        loopback: vec![V6],
        ..config
    };
    let daemon = Daemon::new(config, RecordingOpener::default(), notifier);
    tokio::spawn(daemon.serve(direct, relay, control));
    run
}

async fn start(dir: &TmpDir, config: DaemonConfig) -> PathBuf {
    start_with(dir, config, ScriptedNotifier::new(Answer::Approve)).await
}

/// A relay connection stamped with `alias`.
async fn relay(run: &Path, alias: &str) -> Conn<UnixStream> {
    let s = UnixStream::connect(run.join(RELAY_SOCKET)).await.unwrap();
    let mut conn = Conn::connect(s, Hello::current()).await.unwrap();
    conn.send(&Msg::HostStamp {
        alias: alias.into(),
    })
    .await
    .unwrap();
    conn
}

async fn open(conn: &mut Conn<UnixStream>, url: &str, ports: Vec<u16>) -> Msg {
    conn.send(&Msg::Open {
        url: url.into(),
        reported_host: "devbox".into(),
        callback_ports: ports,
    })
    .await
    .unwrap();
    conn.recv().await.unwrap().unwrap()
}

async fn free_port() -> u16 {
    TcpListener::bind(SocketAddr::new(V6, 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A manual callback forward of `port` from `devbox`.
fn forward_msg(port: u16) -> Msg {
    Msg::Forward {
        reported_host: "devbox".into(),
        callback_ports: vec![port],
    }
}

fn activity_blocking(run: PathBuf, json: bool) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_activity(
        ActivityArgs {
            runtime_dir: Ok(run),
            json,
            follow: false,
        },
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

async fn activity(run: PathBuf, json: bool) -> (ExitCode, String, String) {
    tokio::task::spawn_blocking(move || activity_blocking(run, json))
        .await
        .unwrap()
}

/// `rowser activity --json`, one value per line.
async fn entries(run: PathBuf) -> Vec<Value> {
    let (code, out, err) = activity(run, true).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    out.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_opened_url_shows_with_its_host_route_target_and_result() {
    let dir = TmpDir::new("act");
    let run = start(&dir, DaemonConfig::default()).await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/a", vec![]).await;

    let e = entries(run.clone()).await;
    assert_eq!(e.len(), 1);
    assert_eq!(e[0]["kind"], "open");
    assert_eq!(e[0]["host"], "g14");
    assert_eq!(e[0]["unverified"], false);
    assert_eq!(e[0]["url"], "https://example.com/a");
    assert_eq!(e[0]["route"], "[default]");
    assert_eq!(e[0]["target"], "system");
    assert_eq!(e[0]["result"], "opened");
    assert!(e[0]["time"].as_u64().unwrap() > 1_700_000_000);

    let (code, out, _) = activity(run, false).await;
    assert_eq!(code, ExitCode::SUCCESS);
    let line = out.lines().next().unwrap();
    assert!(
        line.ends_with("  g14  https://example.com/a  [default]  system  opened"),
        "{line}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_that_matched_shows_by_its_number() {
    let text = "[[route]]\nhost = \"db1\"\nopen = \"system\"\n\
                [[route]]\nhost = \"g14\"\nopen = \"system\"\n";
    let config = DaemonConfig {
        routes: rowser::routes::Routes::from_config(&rowser::config::parse(text).unwrap()),
        ..DaemonConfig::default()
    };
    let dir = TmpDir::new("act");
    let run = start(&dir, config).await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/", vec![]).await;
    assert_eq!(entries(run).await[0]["route"], "route 2");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reported_host_shows_as_unverified() {
    let dir = TmpDir::new("act");
    let run = start(&dir, DaemonConfig::default()).await;
    let s = UnixStream::connect(run.join(DIRECT_SOCKET)).await.unwrap();
    let mut conn = Conn::connect(s, Hello::current()).await.unwrap();
    open(&mut conn, "https://example.com/", vec![]).await;

    let e = entries(run.clone()).await;
    assert_eq!(e[0]["host"], "devbox");
    assert_eq!(e[0]["unverified"], true);
    assert_eq!(e[0]["route"], "[unverified]");
    let (_, out, _) = activity(run, false).await;
    assert!(out.contains("  devbox (unverified)  "), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_scheme_shows_as_scheme_refused() {
    let dir = TmpDir::new("act");
    let run = start(&dir, DaemonConfig::default()).await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "ftp://example.com/x", vec![]).await;

    let e = entries(run.clone()).await;
    assert_eq!(e[0]["url"], "ftp://example.com/x");
    assert_eq!(e[0]["result"], "scheme_refused");
    let (_, out, _) = activity(run, false).await;
    assert!(out.trim_end().ends_with("scheme refused"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rate_limited_request_shows_as_rate_limited() {
    let dir = TmpDir::new("act");
    let config = DaemonConfig {
        rate_max: 1,
        ..DaemonConfig::default()
    };
    let run = start(&dir, config).await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/1", vec![]).await;
    open(&mut conn, "https://example.com/2", vec![]).await;

    let e = entries(run.clone()).await;
    assert_eq!(e[0]["result"], "opened");
    assert_eq!(e[1]["url"], "https://example.com/2");
    assert_eq!(e[1]["result"], "rate_limited");
    let (_, out, _) = activity(run, false).await;
    assert!(out.trim_end().ends_with("rate limited"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_confirmation_shows_as_denied() {
    let dir = TmpDir::new("act");
    let config = DaemonConfig {
        confirm_hosts: vec!["g14".into()],
        ..DaemonConfig::default()
    };
    let run = start_with(&dir, config, ScriptedNotifier::new(Answer::Deny)).await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/", vec![]).await;
    assert_eq!(entries(run).await[0]["result"], "denied");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_confirmation_without_an_answer_shows_as_timed_out() {
    let dir = TmpDir::new("act");
    let config = DaemonConfig {
        confirm_hosts: vec!["g14".into()],
        confirm_timeout: Duration::from_millis(200),
        ..DaemonConfig::default()
    };
    let notifier = ScriptedNotifier::new(Answer::Hang);
    let run = start_with(&dir, config, notifier.clone()).await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/", vec![]).await;
    notifier.release(false);
    assert_eq!(entries(run).await[0]["result"], "timed_out");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_opener_shows_as_browser_missing() {
    let dir = TmpDir::new("act");
    let run = dir.0.join("run");
    let (direct, relay_l, control) = bind_sockets(&run).unwrap();
    let opener = rowser::daemon::CommandOpener::new("/nonexistent/rowser-open {url}").unwrap();
    let daemon = Daemon::new(
        DaemonConfig::default(),
        opener,
        ScriptedNotifier::new(Answer::Approve),
    );
    tokio::spawn(daemon.serve(direct, relay_l, control));
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/", vec![]).await;
    assert_eq!(entries(run).await[0]["result"], "browser_missing");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_request_with_a_callback_port_shows_the_port() {
    let dir = TmpDir::new("act");
    let run = start(&dir, DaemonConfig::default()).await;
    let port = free_port().await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/", vec![port]).await;
    let e = entries(run).await;
    assert_eq!(e[0]["kind"], "open");
    assert_eq!(e[0]["ports"], serde_json::json!([port]));
}

#[tokio::test(flavor = "multi_thread")]
async fn entry_201_drops_entry_1() {
    let dir = TmpDir::new("act");
    let config = DaemonConfig {
        rate_max: 1000,
        ..DaemonConfig::default()
    };
    let run = start(&dir, config).await;
    let mut conn = relay(&run, "g14").await;
    for n in 1..=201 {
        open(&mut conn, &format!("https://example.com/{n}"), vec![]).await;
    }
    let e = entries(run).await;
    assert_eq!(e.len(), 200);
    assert_eq!(e[0]["url"], "https://example.com/2");
    assert_eq!(e[199]["url"], "https://example.com/201");
}

/// Sends every write to a channel, so a test can read `--follow` output.
struct Lines(mpsc::Sender<Vec<u8>>);

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(buf.to_vec())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Reads output from `rx` until it holds `n` whole lines.
fn read_lines(rx: &mpsc::Receiver<Vec<u8>>, n: usize) -> Vec<String> {
    let mut text = String::new();
    while text.matches('\n').count() < n {
        let chunk = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        text.push_str(&String::from_utf8(chunk).unwrap());
    }
    text.lines().map(String::from).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn follow_prints_new_entries_as_they_come() {
    let dir = TmpDir::new("act");
    let run = start(&dir, DaemonConfig::default()).await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/old", vec![]).await;

    let (tx, rx) = mpsc::channel();
    let follow_run = run.clone();
    std::thread::spawn(move || {
        run_activity(
            ActivityArgs {
                runtime_dir: Ok(follow_run),
                json: true,
                follow: true,
            },
            &mut Lines(tx),
            &mut std::io::sink(),
        )
    });
    let lines = read_lines(&rx, 1);
    assert!(lines[0].contains("example.com/old"), "{lines:?}");

    open(&mut conn, "https://example.com/new", vec![]).await;
    let lines = read_lines(&rx, 1);
    let v: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(v["url"], "https://example.com/new");
    assert_eq!(v["id"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_port_records_the_requested_port() {
    let dir = TmpDir::new("act");
    let run = start(&dir, DaemonConfig::default()).await;
    let busy = TcpListener::bind(SocketAddr::new(V6, 0)).await.unwrap();
    let port = busy.local_addr().unwrap().port();
    let mut conn = relay(&run, "g14").await;
    conn.send(&forward_msg(port)).await.unwrap();
    conn.recv().await.unwrap();
    open(&mut conn, "https://example.com/", vec![port]).await;

    let e = entries(run.clone()).await;
    for entry in &e {
        assert_eq!(entry["ports"], serde_json::json!([port]));
        assert!(!entry["warnings"].as_array().unwrap().is_empty(), "{entry}");
    }
    let (_, out, _) = activity(run, false).await;
    assert!(!out.contains("still open"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_target_that_falls_back_shows_opened_with_a_warning() {
    let text = "[[route]]\nhost = \"g14\"\nopen = \"chrome:Work\"\n";
    let config = DaemonConfig {
        routes: rowser::routes::Routes::from_config(&rowser::config::parse(text).unwrap()),
        ..DaemonConfig::default()
    };
    let dir = TmpDir::new("act");
    let run = start(&dir, config).await;
    let mut conn = relay(&run, "g14").await;
    open(&mut conn, "https://example.com/", vec![]).await;
    let e = entries(run.clone()).await;
    assert_eq!(e[0]["result"], "opened");
    let (_, out, _) = activity(run, false).await;
    assert!(
        out.trim_end().ends_with(
            "opened  warning: target `chrome:Work` is not available \
             (browser `chrome` is not installed); fell back to [default] `system`"
        ),
        "{out}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn control_and_bidi_characters_do_not_reach_the_text_line() {
    let dir = TmpDir::new("act");
    let run = start(&dir, DaemonConfig::default()).await;
    let s = UnixStream::connect(run.join(DIRECT_SOCKET)).await.unwrap();
    let mut conn = Conn::connect(s, Hello::current()).await.unwrap();
    conn.send(&Msg::Open {
        url: "https://example.com/\u{202E}x".into(),
        reported_host: "ev\u{1b}[2Kil\u{2066}box".into(),
        callback_ports: vec![],
    })
    .await
    .unwrap();
    conn.recv().await.unwrap();
    let (_, out, _) = activity(run, false).await;
    assert!(out.contains("  ev[2Kilbox (unverified)  "), "{out}");
    assert!(
        !out.contains('\u{1b}') && !out.contains('\u{202E}'),
        "{out:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn activity_is_refused_on_the_relay_and_direct_sockets() {
    let dir = TmpDir::new("act");
    let run = start(&dir, DaemonConfig::default()).await;
    let s = UnixStream::connect(run.join(DIRECT_SOCKET)).await.unwrap();
    let direct = Conn::connect(s, Hello::current()).await.unwrap();
    for mut conn in [direct, relay(&run, "g14").await] {
        conn.send(&Msg::Activity { follow: true }).await.unwrap();
        match conn.recv().await.unwrap() {
            Some(Msg::OpenResult { error: Some(_), .. }) => {}
            other => panic!("{other:?}"),
        }
        assert!(conn.recv().await.unwrap_or(None).is_none());
    }
}

#[test]
fn activity_with_no_daemon_says_not_running_and_exits_1() {
    let dir = TmpDir::new("act");
    let (code, out, err) = activity_blocking(dir.0.join("run"), false);
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    assert_eq!(err, "rowser: the daemon is not running\n");
}
