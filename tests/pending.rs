//! `rowser pending`, `rowser approve` and `rowser deny`, with a Notifier
//! that never answers (`--test-confirm hang`).

mod common;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use common::TmpDir;
use rowser::config::Target;
use rowser::control::{run_activity, run_approve, run_deny, run_pending, ActivityArgs};
use rowser::daemon::testing::{Answer, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{
    bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET, FORWARD_NOT_APPROVED, NOT_APPROVED,
};
use rowser::shim::{run_forward, run_open, ShimEnv};
use serde_json::Value;
use url::Url;

struct Started {
    run: PathBuf,
    opener: RecordingOpener,
    notifier: ScriptedNotifier,
}

async fn start(dir: &TmpDir) -> Started {
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let opener = RecordingOpener::default();
    let notifier = ScriptedNotifier::new(Answer::Hang);
    let config = DaemonConfig {
        confirm_hosts: vec!["*".into()],
        confirm_unverified: true,
        browser_roots: Some(chrome_roots(&dir.0.join("roots"))),
        ..DaemonConfig::default()
    };
    let daemon = Daemon::new(config, opener.clone(), notifier.clone());
    tokio::spawn(daemon.serve(direct, relay, control));
    Started {
        run,
        opener,
        notifier,
    }
}

/// Linux browser roots under `t` with Chrome and its Profile `Work (Profile 2)`.
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

/// Starts the Shim for `url` in the background; gives (exit, stderr).
fn shim_open(run: &Path, url: &str) -> tokio::task::JoinHandle<(ExitCode, String)> {
    let run = run.to_path_buf();
    let url = Url::parse(url).unwrap();
    tokio::spawn(async move {
        let mut env = shim_env(&run);
        let code = run_open(&url, &mut env).await;
        (code, String::from_utf8(env.stderr).unwrap())
    })
}

/// Starts `rowser forward <port>` in the background; gives (exit, stderr).
fn shim_forward(run: &Path, port: u16) -> tokio::task::JoinHandle<(ExitCode, String)> {
    let run = run.to_path_buf();
    tokio::spawn(async move {
        let mut env = shim_env(&run);
        let code = run_forward(port, &mut env).await;
        (code, String::from_utf8(env.stderr).unwrap())
    })
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

/// Runs a control command on a blocking thread; gives (exit, stdout, stderr).
async fn command<F>(f: F) -> (ExitCode, String, String)
where
    F: FnOnce(&mut Vec<u8>, &mut Vec<u8>) -> ExitCode + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = f(&mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    })
    .await
    .unwrap()
}

async fn pending(run: &Path) -> (ExitCode, String, String) {
    let run = run.to_path_buf();
    command(move |o, e| run_pending(Ok(run), o, e)).await
}

async fn approve(run: &Path, id: u64, open: Option<&str>) -> (ExitCode, String, String) {
    let run = run.to_path_buf();
    let open = open.map(str::to_owned);
    command(move |o, e| run_approve(Ok(run), id, open, o, e)).await
}

async fn deny(run: &Path, id: u64) -> (ExitCode, String, String) {
    let run = run.to_path_buf();
    command(move |o, e| run_deny(Ok(run), id, o, e)).await
}

async fn activity(run: &Path) -> Vec<Value> {
    let run = run.to_path_buf();
    let (code, out, err) = command(move |o, e| {
        run_activity(
            ActivityArgs {
                runtime_dir: Ok(run),
                json: true,
                follow: false,
            },
            o,
            e,
        )
    })
    .await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    out.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Waits until `rowser pending` lists `n` requests; gives its stdout.
async fn wait_pending(run: &Path, n: usize) -> String {
    for _ in 0..200 {
        let (code, out, err) = pending(run).await;
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        if out.lines().filter(|l| !l.starts_with("rowser:")).count() == n {
            return out;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no {n} pending requests");
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_lists_a_waiting_request_with_id_host_url_and_target() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let (code, out, _) = pending(&s.run).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, "rowser: no pending requests\n");

    let shim = shim_open(&s.run, "https://example.com/a");
    let out = wait_pending(&s.run, 1).await;
    let fields: Vec<&str> = out.trim_end().split("  ").collect();
    assert_eq!(fields[0], "1");
    assert_eq!(fields[1], "devbox (unverified)");
    assert_eq!(fields[2], "https://example.com/a");
    assert_eq!(fields[3], "system");
    assert!(fields[4].ends_with('s'), "{out}");
    assert_eq!(deny(&s.run, 1).await.0, ExitCode::SUCCESS);
    assert_eq!(shim.await.unwrap().0, ExitCode::FAILURE);
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_with_open_opens_in_the_given_target_and_closes_the_notification() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let shim = shim_open(&s.run, "https://example.com/a");
    wait_pending(&s.run, 1).await;

    let (code, out, err) = approve(&s.run, 1, Some("chrome:Work")).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(out, "rowser: approved 1\n");
    let (code, err) = shim.await.unwrap();
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        s.opener.opens(),
        [(
            Url::parse("https://example.com/a").unwrap(),
            Target::Browser {
                browser: "chrome".into(),
                profile: Some("Profile 2".into()),
            }
        )]
    );
    assert_eq!(s.notifier.dismissed(), 1);
    // A late answer from the notification changes nothing.
    s.notifier.release(false);
    assert_eq!(s.opener.opens().len(), 1);

    let e = activity(&s.run).await;
    assert_eq!(e[0]["result"], "opened");
    assert_eq!(e[0]["answered_by"], "terminal");
    assert_eq!(e[0]["target"], "chrome:Profile 2");
    assert_eq!(pending(&s.run).await.1, "rowser: no pending requests\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_without_open_opens_in_the_request_target() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let shim = shim_open(&s.run, "https://example.com/a");
    wait_pending(&s.run, 1).await;
    assert_eq!(approve(&s.run, 1, None).await.0, ExitCode::SUCCESS);
    assert_eq!(shim.await.unwrap().0, ExitCode::SUCCESS);
    assert_eq!(
        s.opener.opens(),
        [(Url::parse("https://example.com/a").unwrap(), Target::System)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_refuses_with_the_shim_error_and_a_second_answer_is_refused() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let shim = shim_open(&s.run, "https://example.com/a");
    wait_pending(&s.run, 1).await;

    let (code, out, err) = deny(&s.run, 1).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(out, "rowser: denied 1\n");
    let (code, err) = shim.await.unwrap();
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(err, format!("rowser: {NOT_APPROVED}\n"));
    assert!(s.opener.opens().is_empty());

    let (code, _, err) = approve(&s.run, 1, None).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(err, "rowser approve: no pending request 1\n");
    let (code, _, err) = deny(&s.run, 1).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(err, "rowser deny: no pending request 1\n");

    let e = activity(&s.run).await;
    assert_eq!(e.len(), 1);
    assert_eq!(e[0]["result"], "denied");
    assert_eq!(e[0]["answered_by"], "terminal");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_answer_first_wins_and_the_terminal_answer_is_refused() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let shim = shim_open(&s.run, "https://example.com/a");
    wait_pending(&s.run, 1).await;
    s.notifier.release(true);
    assert_eq!(shim.await.unwrap().0, ExitCode::SUCCESS);
    let (code, _, err) = deny(&s.run, 1).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(err, "rowser deny: no pending request 1\n");
    assert_eq!(s.opener.opens().len(), 1);
    assert_eq!(activity(&s.run).await[0]["answered_by"], "desktop");
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_with_a_missing_browser_is_refused_and_the_request_still_waits() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let shim = shim_open(&s.run, "https://example.com/a");
    wait_pending(&s.run, 1).await;
    let (code, _, err) = approve(&s.run, 1, Some("firefox")).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.starts_with("rowser approve: "), "{err}");
    let (code, _, err) = approve(&s.run, 1, Some("ask")).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.starts_with("rowser approve: "), "{err}");
    wait_pending(&s.run, 1).await;
    assert_eq!(deny(&s.run, 1).await.0, ExitCode::SUCCESS);
    assert_eq!(shim.await.unwrap().0, ExitCode::FAILURE);
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_refuses_a_rowser_forward_with_the_forward_error() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let forward = shim_forward(&s.run, 8085);
    wait_pending(&s.run, 1).await;
    assert_eq!(deny(&s.run, 1).await.0, ExitCode::SUCCESS);
    let (code, err) = forward.await.unwrap();
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(err, format!("rowser: {FORWARD_NOT_APPROVED}\n"));
    let e = activity(&s.run).await;
    assert_eq!(e[0]["result"], "denied");
    assert_eq!(e[0]["answered_by"], "terminal");
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_with_open_is_refused_for_a_rowser_forward_and_it_still_waits() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let forward = shim_forward(&s.run, 8085);
    wait_pending(&s.run, 1).await;
    let (code, _, err) = approve(&s.run, 1, Some("system")).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        err,
        "rowser approve: --open applies only to a URL, not to rowser forward\n"
    );
    wait_pending(&s.run, 1).await;
    assert_eq!(deny(&s.run, 1).await.0, ExitCode::SUCCESS);
    assert_eq!(forward.await.unwrap().0, ExitCode::FAILURE);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_id_is_an_error() {
    let dir = TmpDir::new("pnd");
    let s = start(&dir).await;
    let (code, _, err) = approve(&s.run, 9, None).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(err, "rowser approve: no pending request 9\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commands_say_when_the_daemon_is_not_running() {
    let dir = TmpDir::new("pnd");
    let run = dir.0.join("none");
    let (code, _, err) = pending(&run).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(err, "rowser: the daemon is not running\n");
    assert_eq!(deny(&run, 1).await.2, "rowser: the daemon is not running\n");
}
