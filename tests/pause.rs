//! `rowser pause` and `rowser resume`.

mod common;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use common::TmpDir;
use rowser::control::{parse_duration, run_pause, run_resume, run_status, StatusArgs};
use rowser::daemon::testing::{Answer, RecordingNotifier, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET};
use rowser::protocol::{Conn, Hello, Msg, CONTROL_SOCKET};
use rowser::shim::{run_forward, run_open, ShimEnv};
use serde_json::Value;
use tokio::net::UnixStream;
use url::Url;

async fn start(dir: &TmpDir) -> (PathBuf, RecordingOpener) {
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let opener = RecordingOpener::default();
    let daemon = Daemon::new(
        DaemonConfig::default(),
        opener.clone(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve(direct, relay, control));
    (run, opener)
}

fn shim_env(run: &Path) -> ShimEnv<Vec<u8>> {
    ShimEnv {
        tunnel_dir: run.join("no-tunnels"),
        socket_override: Some(run.join(DIRECT_SOCKET)),
        outpost_socket: None,
        reported_host: "devbox".into(),
        procfs: rowser::detect::Procfs::new("/nonexistent"),
        caller_pid: 1,
        keeper_exe: None,
        graphical: false,
        search_path: None,
        self_exe: None,
        in_fallback: false,
        stderr: Vec::new(),
    }
}

/// Runs the Shim for `url`; gives (exit, stderr).
async fn shim_open(run: &Path, url: &str) -> (ExitCode, String) {
    let mut env = shim_env(run);
    let code = run_open(&Url::parse(url).unwrap(), &mut env).await;
    (code, String::from_utf8(env.stderr).unwrap())
}

type Run = fn(PathBuf, &mut Vec<u8>, &mut Vec<u8>) -> ExitCode;

/// Runs a control command on a blocking thread; gives (exit, stdout, stderr).
async fn command(run: PathBuf, f: Run) -> (ExitCode, String, String) {
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = f(run, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    })
    .await
    .unwrap()
}

fn pause(run: PathBuf, out: &mut Vec<u8>, err: &mut Vec<u8>) -> ExitCode {
    run_pause(Ok(run), None, out, err)
}

fn pause_hour(run: PathBuf, out: &mut Vec<u8>, err: &mut Vec<u8>) -> ExitCode {
    run_pause(Ok(run), Some(Duration::from_secs(3600)), out, err)
}

fn resume(run: PathBuf, out: &mut Vec<u8>, err: &mut Vec<u8>) -> ExitCode {
    run_resume(Ok(run), out, err)
}

fn status_json(run: PathBuf, out: &mut Vec<u8>, err: &mut Vec<u8>) -> ExitCode {
    run_status(
        StatusArgs {
            runtime_dir: Ok(run),
            json: true,
        },
        out,
        err,
    )
}

fn status_text(run: PathBuf, out: &mut Vec<u8>, err: &mut Vec<u8>) -> ExitCode {
    run_status(
        StatusArgs {
            runtime_dir: Ok(run),
            json: false,
        },
        out,
        err,
    )
}

fn activity_json(run: PathBuf, out: &mut Vec<u8>, err: &mut Vec<u8>) -> ExitCode {
    rowser::control::run_activity(
        rowser::control::ActivityArgs {
            runtime_dir: Ok(run),
            json: true,
            follow: false,
        },
        out,
        err,
    )
}

async fn status(run: &Path) -> Value {
    let (code, out, err) = command(run.to_path_buf(), status_json).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    serde_json::from_str(&out).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_daemon_refuses_an_open_request_until_resume() {
    let dir = TmpDir::new("pau");
    let (run, opener) = start(&dir).await;

    let (code, out, err) = command(run.clone(), pause).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(out, "rowser: paused\n");

    let (code, err) = shim_open(&run, "https://example.com/a").await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        err,
        "rowser: paused; open this URL manually: https://example.com/a\n"
    );
    assert!(opener.urls().is_empty());
    let s = status(&run).await;
    assert_eq!(s["paused"], true);
    assert_eq!(s["paused_until"], Value::Null);

    let (code, out, err) = command(run.clone(), activity_json).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let e: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(e["result"], "paused");
    assert_eq!(e["url"], "https://example.com/a");

    let (code, out, err) = command(run.clone(), resume).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(out, "rowser: resumed\n");
    let (code, err) = shim_open(&run, "https://example.com/b").await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        opener.urls(),
        vec![Url::parse("https://example.com/b").unwrap()]
    );
    assert_eq!(status(&run).await["paused"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_daemon_refuses_rowser_forward() {
    let dir = TmpDir::new("pau");
    let (run, _) = start(&dir).await;
    assert_eq!(command(run.clone(), pause).await.0, ExitCode::SUCCESS);
    let mut env = shim_env(&run);
    assert_eq!(run_forward(8085, &mut env).await, ExitCode::FAILURE);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        "rowser: paused; port 8085 was not forwarded\n"
    );
    let (_, out, _) = command(run.clone(), activity_json).await;
    let e: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(e["kind"], "forward");
    assert_eq!(e["result"], "paused");
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_for_shows_until_when_in_status() {
    let dir = TmpDir::new("pau");
    let (run, _) = start(&dir).await;
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (code, out, err) = command(run.clone(), pause_hour).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.starts_with("rowser: paused until "), "{out}");
    let until = status(&run).await["paused_until"].as_u64().unwrap();
    assert!((before + 3600..=before + 3602).contains(&until), "{until}");
    let (_, text, _) = command(run.clone(), status_text).await;
    assert!(
        text.lines()
            .any(|l| l.starts_with("paused:            yes, until ")),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_when_not_paused_says_so() {
    let dir = TmpDir::new("pau");
    let (run, _) = start(&dir).await;
    let (code, out, _) = command(run, resume).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, "rowser: not paused\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_with_no_daemon_says_not_running_and_exits_1() {
    let dir = TmpDir::new("pau");
    let (code, out, err) = command(dir.0.join("run"), pause).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    assert_eq!(err, "rowser: the daemon is not running\n");
}

async fn control(run: &Path) -> Conn<UnixStream> {
    let s = UnixStream::connect(run.join(CONTROL_SOCKET)).await.unwrap();
    Conn::connect(s, Hello::current()).await.unwrap()
}

#[tokio::test(start_paused = true)]
async fn pause_for_ends_by_itself() {
    let dir = TmpDir::new("pau");
    let (run, opener) = start(&dir).await;
    let mut c = control(&run).await;
    c.send(&Msg::Pause {
        for_secs: Some(1800),
    })
    .await
    .unwrap();
    assert!(matches!(
        c.recv().await.unwrap(),
        Some(Msg::Paused { until: Some(_) })
    ));

    tokio::time::advance(Duration::from_secs(1799)).await;
    let (code, _) = shim_open(&run, "https://example.com/a").await;
    assert_eq!(code, ExitCode::FAILURE);

    tokio::time::advance(Duration::from_secs(2)).await;
    let (code, err) = shim_open(&run, "https://example.com/b").await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        opener.urls(),
        vec![Url::parse("https://example.com/b").unwrap()]
    );
}

/// Starts a Daemon that asks the user about every host and waits for
/// `ScriptedNotifier::release`.
async fn start_hung(dir: &TmpDir) -> (PathBuf, RecordingOpener, ScriptedNotifier) {
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let opener = RecordingOpener::default();
    let notifier = ScriptedNotifier::new(Answer::Hang);
    let config = DaemonConfig {
        confirm_hosts: vec!["*".into()],
        confirm_unverified: true,
        ..DaemonConfig::default()
    };
    let daemon = Daemon::new(config, opener.clone(), notifier.clone());
    tokio::spawn(daemon.serve(direct, relay, control));
    (run, opener, notifier)
}

async fn wait_for_confirm(notifier: &ScriptedNotifier) {
    while notifier.confirms().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_request_approved_after_pause_starts_is_refused() {
    let dir = TmpDir::new("pau");
    let (run, opener, notifier) = start_hung(&dir).await;
    let open = tokio::spawn({
        let run = run.clone();
        async move { shim_open(&run, "https://example.com/a").await }
    });
    wait_for_confirm(&notifier).await;
    assert_eq!(command(run.clone(), pause).await.0, ExitCode::SUCCESS);
    notifier.release(true);

    let (code, err) = open.await.unwrap();
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        err,
        "rowser: paused; open this URL manually: https://example.com/a\n"
    );
    assert!(opener.urls().is_empty());
    let (_, out, _) = command(run.clone(), activity_json).await;
    let e: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(e["result"], "paused");
}

#[tokio::test(flavor = "multi_thread")]
async fn rowser_forward_approved_after_pause_starts_is_refused() {
    let dir = TmpDir::new("pau");
    let (run, _, notifier) = start_hung(&dir).await;
    let forward = tokio::spawn({
        let run = run.clone();
        async move {
            let mut env = shim_env(&run);
            let code = run_forward(8085, &mut env).await;
            (code, String::from_utf8(env.stderr).unwrap())
        }
    });
    wait_for_confirm(&notifier).await;
    assert_eq!(command(run.clone(), pause).await.0, ExitCode::SUCCESS);
    notifier.release(true);

    let (code, err) = forward.await.unwrap();
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(err, "rowser: paused; port 8085 was not forwarded\n");
    let (_, out, _) = command(run.clone(), activity_json).await;
    let e: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(e["kind"], "forward");
    assert_eq!(e["result"], "paused");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pause_too_long_is_refused_and_the_daemon_stays_up() {
    let dir = TmpDir::new("pau");
    let (run, _) = start(&dir).await;
    let mut c = control(&run).await;
    c.send(&Msg::Pause {
        for_secs: Some(u64::MAX),
    })
    .await
    .unwrap();
    match c.recv().await.unwrap() {
        Some(Msg::OpenResult { error: Some(e), .. }) => assert_eq!(e, "duration too long"),
        other => panic!("{other:?}"),
    }
    assert_eq!(status(&run).await["paused"], false);
}

#[test]
fn a_new_daemon_on_the_same_runtime_directory_starts_not_paused() {
    let dir = TmpDir::new("pau");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let run = rt.block_on(async {
        let (run, _) = start(&dir).await;
        assert_eq!(command(run.clone(), pause).await.0, ExitCode::SUCCESS);
        assert_eq!(status(&run).await["paused"], true);
        run
    });
    drop(rt);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (again, _) = start(&dir).await;
        assert_eq!(again, run);
        assert_eq!(status(&run).await["paused"], false);
    });
}

#[test]
fn durations_read_hours_and_minutes() {
    assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
    assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
    assert_eq!(parse_duration("2h30m"), Ok(Duration::from_secs(9000)));
    assert_eq!(parse_duration("45s"), Ok(Duration::from_secs(45)));
    for bad in ["", "30", "m", "1x", "0m", "1m1h", "-1h"] {
        assert!(parse_duration(bad).is_err(), "{bad}");
    }
}
