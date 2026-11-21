//! `rowser route list|add|remove|move|test` against a config file in a temp
//! dir (never the real `~/.config/rowser`).

mod common;

use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use common::TmpDir;
use rowser::daemon::testing::{Answer, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET, RELAY_SOCKET};
use rowser::protocol::{Conn, Hello, Msg};
use rowser::route::{run_route, RouteAction, RouteArgs};
use tokio::net::UnixStream;

const HAND: &str = "# my routes, by hand\n\
                    [[route]]\nhost = \"g14\"  # the laptop\nopen = \"system\"\n\n\
                    [default]\nopen = \"system\"\n";

/// Runs one `rowser route` command; gives (exit, stdout, stderr).
fn route(file: &Path, run: &Path, action: RouteAction) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_route(
        RouteArgs {
            config_file: Ok(file.to_path_buf()),
            runtime_dir: Ok(run.to_path_buf()),
            action,
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

fn add(host: &str, matches: &[&str], open: &str, ask: bool) -> RouteAction {
    RouteAction::Add {
        host: host.into(),
        matches: matches.iter().map(|m| m.to_string()).collect(),
        open: open.into(),
        ask,
    }
}

fn list(file: &Path, run: &Path) -> String {
    let (code, out, err) = route(file, run, RouteAction::List);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    out
}

#[test]
fn add_appends_a_route_and_keeps_the_comments() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("rowser/config.toml");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, HAND).unwrap();
    let run = dir.0.join("run");

    let (code, out, err) = route(
        &file,
        &run,
        add("db*", &["*.google.com"], "chrome:Work", true),
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        out,
        "rowser: added route 2\n\
         rowser: the daemon is not running; the change applies when it starts\n"
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# my routes, by hand\n"), "{text}");
    assert!(text.contains("# the laptop"), "{text}");
    assert_eq!(
        list(&file, &run),
        "1  g14  system\n\
         2  db*  match *.google.com  chrome:Work  ask\n\
         [unverified]  ask\n\
         [default]  system\n"
    );
}

#[test]
fn add_creates_the_file_and_its_directory() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("cfg/rowser/config.toml");
    let run = dir.0.join("run");
    let (code, _, err) = route(&file, &run, add("g14", &[], "safari", false));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        list(&file, &run),
        "1  g14  safari\n[unverified]  ask\n[default]  system\n"
    );
}

#[test]
fn an_invalid_open_is_refused_and_the_file_is_unchanged() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    std::fs::write(&file, HAND).unwrap();
    let run = dir.0.join("run");
    for bad in [":x", "chrome:", ""] {
        let (code, out, err) = route(&file, &run, add("g14", &[], bad, false));
        assert_eq!(code, ExitCode::FAILURE, "{bad}");
        assert_eq!(out, "");
        assert!(err.starts_with("rowser route add: "), "{err}");
    }
    let (code, _, err) = route(&file, &run, add("", &[], "system", false));
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("host is empty"), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), HAND);
}

#[test]
fn an_invalid_file_is_not_changed() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    let bad = "[[route]]\nhost = \"g14\"\nopen = 7\n";
    std::fs::write(&file, bad).unwrap();
    let run = dir.0.join("run");
    let (code, _, err) = route(&file, &run, add("db1", &[], "system", false));
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("line 3"), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), bad);
}

#[test]
fn move_and_remove_keep_the_comments() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    std::fs::write(&file, HAND).unwrap();
    let run = dir.0.join("run");
    route(&file, &run, add("db1", &[], "firefox", false));
    route(&file, &run, add("db2", &[], "safari", false));

    let (code, out, err) = route(&file, &run, RouteAction::Move { n: 3, to: 1 });
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.starts_with("rowser: moved route 3 to 1\n"), "{out}");
    assert_eq!(
        list(&file, &run),
        "1  db2  safari\n2  g14  system\n3  db1  firefox\n\
         [unverified]  ask\n[default]  system\n"
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("host = \"g14\"  # the laptop"), "{text}");

    let (code, out, err) = route(&file, &run, RouteAction::Remove { n: 1 });
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.starts_with("rowser: removed route 1\n"), "{out}");
    assert_eq!(
        list(&file, &run),
        "1  g14  system\n2  db1  firefox\n[unverified]  ask\n[default]  system\n"
    );
    let text = std::fs::read_to_string(&file).unwrap();
    // A comment above a Route moves with it.
    assert!(
        text.contains("# my routes, by hand\n[[route]]\nhost = \"g14\"  # the laptop"),
        "{text}"
    );
    // The result is still a valid file.
    rowser::config::load(&file).unwrap();
}

#[test]
fn move_keeps_one_blank_line_between_routes_and_none_at_the_top() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    std::fs::write(
        &file,
        "# r1\n[[route]]\nhost = \"g14\"\nopen = \"system\"\n",
    )
    .unwrap();
    let run = dir.0.join("run");
    route(&file, &run, add("db1", &[], "firefox", false));
    let (code, _, err) = route(&file, &run, RouteAction::Move { n: 2, to: 1 });
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "[[route]]\nhost = \"db1\"\nopen = \"firefox\"\n\n\
         # r1\n[[route]]\nhost = \"g14\"\nopen = \"system\"\n"
    );
}

const HEADED: &str = "# my routes, by hand\n\n\
                      # the laptop\n[[route]]\nhost = \"g14\"\nopen = \"system\"\n\n\
                      [[route]]\nhost = \"db1\"\nopen = \"firefox\"\n";

#[test]
fn the_file_header_stays_at_the_top_on_move_and_remove() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    let run = dir.0.join("run");
    std::fs::write(&file, HEADED).unwrap();
    let (code, _, err) = route(&file, &run, RouteAction::Move { n: 2, to: 1 });
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# my routes, by hand\n\n\
         [[route]]\nhost = \"db1\"\nopen = \"firefox\"\n\n\
         # the laptop\n[[route]]\nhost = \"g14\"\nopen = \"system\"\n"
    );

    std::fs::write(&file, HEADED).unwrap();
    let (code, _, err) = route(&file, &run, RouteAction::Remove { n: 1 });
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# my routes, by hand\n\n[[route]]\nhost = \"db1\"\nopen = \"firefox\"\n"
    );
}

#[test]
fn remove_route_1_of_hand_takes_its_comment_and_keeps_default() {
    // HAND has no blank line under its first comment: the comment is
    // Route 1's own, so it goes with Route 1.
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    std::fs::write(&file, HAND).unwrap();
    let run = dir.0.join("run");
    let (code, _, err) = route(&file, &run, RouteAction::Remove { n: 1 });
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "[default]\nopen = \"system\"\n"
    );
}

#[test]
fn list_with_no_file_shows_only_the_fallbacks() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("missing/config.toml");
    let run = dir.0.join("run");
    assert_eq!(list(&file, &run), "[unverified]  ask\n[default]  system\n");
    assert!(!file.exists());
}

#[cfg(unix)]
#[test]
fn a_change_keeps_a_symlinked_file_and_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TmpDir::new("rt");
    let real = dir.0.join("dotfiles/config.toml");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, HAND).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let file = dir.0.join("config.toml");
    std::os::unix::fs::symlink(&real, &file).unwrap();
    let run = dir.0.join("run");
    let (code, _, err) = route(&file, &run, add("db1", &[], "firefox", false));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(std::fs::symlink_metadata(&file)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(std::fs::read_to_string(&real).unwrap().contains("db1"));
    let mode = std::fs::metadata(&real).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn remove_and_move_refuse_a_number_out_of_range() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    std::fs::write(&file, HAND).unwrap();
    let run = dir.0.join("run");
    for action in [
        RouteAction::Remove { n: 0 },
        RouteAction::Remove { n: 2 },
        RouteAction::Move { n: 1, to: 2 },
        RouteAction::Move { n: 2, to: 1 },
    ] {
        let (code, _, err) = route(&file, &run, action);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            err.contains("no route 2") || err.contains("no route 0"),
            "{err}"
        );
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), HAND);
}

fn test_action(host: &str, url: &str, unverified: bool) -> RouteAction {
    RouteAction::Test {
        host: host.into(),
        url: url.parse().unwrap(),
        unverified,
    }
}

#[test]
fn route_test_prints_the_target_and_the_route_and_needs_no_daemon() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    std::fs::write(
        &file,
        "[[route]]\nhost = \"g14\"\nmatch = [\"*.google.com\"]\nopen = \"chrome:Work\"\nask = true\n\
         [[route]]\nhost = \"g1?\"\nopen = \"safari\"\n",
    )
    .unwrap();
    let run = dir.0.join("run");
    let t = |host, url, unv| {
        let (code, out, err) = route(&file, &run, test_action(host, url, unv));
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        out
    };
    assert_eq!(
        t("g14", "https://mail.google.com/x", false),
        "chrome:Work  route 1  ask\n"
    );
    assert_eq!(t("g14", "https://example.com/", false), "safari  route 2\n");
    assert_eq!(
        t("db1", "https://example.com/", false),
        "system  [default]\n"
    );
    assert_eq!(
        t("g14", "https://example.com/", true),
        "ask  [unverified]\n"
    );
}

const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

/// The Target the Daemon opened and the `route` label of its activity
/// entry for one Open request, as `<target>  <label>`, the `route test`
/// form.
async fn daemon_route(file: &Path, alias: Option<&str>, url: &str) -> String {
    let dir = TmpDir::new("rtd");
    let run: PathBuf = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let config = DaemonConfig {
        loopback: vec![V6],
        routes: rowser::routes::Routes::from_config(&rowser::config::load(file).unwrap()),
        ..DaemonConfig::default()
    };
    let opener = RecordingOpener::default();
    let daemon = Daemon::new(
        config,
        opener.clone(),
        ScriptedNotifier::new(Answer::Approve),
    );
    tokio::spawn(daemon.serve(direct, relay, control));
    let socket = if alias.is_some() {
        RELAY_SOCKET
    } else {
        DIRECT_SOCKET
    };
    let s = UnixStream::connect(run.join(socket)).await.unwrap();
    let mut conn = Conn::connect(s, Hello::current()).await.unwrap();
    if let Some(a) = alias {
        conn.send(&Msg::HostStamp { alias: a.into() })
            .await
            .unwrap();
    }
    conn.send(&Msg::Open {
        url: url.into(),
        reported_host: "devbox".into(),
        callback_ports: vec![],
    })
    .await
    .unwrap();
    conn.recv().await.unwrap().unwrap();
    let reply = tokio::task::spawn_blocking(move || {
        rowser::control::request(&run, &Msg::Activity { follow: false })
    })
    .await
    .unwrap()
    .unwrap();
    let Msg::ActivityReply { entries } = reply else {
        panic!("{reply:?}")
    };
    let opens = opener.opens();
    assert_eq!(opens.len(), 1, "{opens:?}");
    // Until browser Targets open (#47), the Daemon opens a browser Target
    // with `system` and names the picked Target in a warning.
    let fell_back = entries[0]
        .warnings
        .iter()
        .find_map(|w| w.strip_prefix("target `")?.split('`').next());
    let target = fell_back.map_or_else(|| opens[0].1.to_string(), str::to_string);
    format!("{target}  {}", entries[0].route.clone().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_reloads_a_running_daemon() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let config = DaemonConfig {
        loopback: vec![V6],
        config_file: Some(file.clone()),
        ..DaemonConfig::default()
    };
    let daemon = Daemon::new(
        config,
        RecordingOpener::default(),
        ScriptedNotifier::new(Answer::Approve),
    );
    tokio::spawn(daemon.serve(direct, relay, control));
    let (f, r) = (file.clone(), run.clone());
    let (code, out, err) =
        tokio::task::spawn_blocking(move || route(&f, &r, add("g14", &[], "system", false)))
            .await
            .unwrap();
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        out,
        "rowser: added route 1\nrowser: config reloaded (1 route)\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_that_refuses_the_reload_is_reported_and_the_change_stays() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    // The Daemon reads another, invalid file, so it refuses the reload.
    let other = dir.0.join("other.toml");
    std::fs::write(&other, "[[route]]\nhost = \"g14\"\nopen = 7\n").unwrap();
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let config = DaemonConfig {
        loopback: vec![V6],
        config_file: Some(other),
        ..DaemonConfig::default()
    };
    let daemon = Daemon::new(
        config,
        RecordingOpener::default(),
        ScriptedNotifier::new(Answer::Approve),
    );
    tokio::spawn(daemon.serve(direct, relay, control));
    let (f, r) = (file.clone(), run.clone());
    let (code, out, err) =
        tokio::task::spawn_blocking(move || route(&f, &r, add("g14", &[], "system", false)))
            .await
            .unwrap();
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.starts_with("rowser: added route 1\nrowser: the daemon did not reload: "),
        "{out}"
    );
    assert!(out.contains("line 3"), "{out}");
    assert!(std::fs::read_to_string(&file).unwrap().contains("g14"));
}

#[tokio::test(flavor = "multi_thread")]
async fn route_test_agrees_with_the_daemon() {
    let dir = TmpDir::new("rt");
    let file = dir.0.join("config.toml");
    std::fs::write(
        &file,
        "[[route]]\nhost = \"db1\"\nopen = \"firefox\"\n\
         [[route]]\nhost = \"g14\"\nmatch = [\"*.google.com\"]\nopen = \"chrome:Work\"\n\
         [[route]]\nhost = \"g*\"\nopen = \"safari\"\n\
         [unverified]\nopen = \"system\"\n",
    )
    .unwrap();
    let run = dir.0.join("run");
    for (alias, url) in [
        (Some("g14"), "https://mail.google.com/"),
        (Some("g14"), "https://example.com/"),
        (Some("db1"), "https://example.com/"),
        (Some("x"), "https://example.com/"),
        (None, "https://example.com/"),
    ] {
        let want = daemon_route(&file, alias, url).await;
        let f = file.clone();
        let r = run.clone();
        let action = test_action(alias.unwrap_or("devbox"), url, alias.is_none());
        let (code, out, err) = tokio::task::spawn_blocking(move || route(&f, &r, action))
            .await
            .unwrap();
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let got = out.trim_end().to_string();
        assert_eq!(got, want, "{alias:?} {url}");
    }
}
