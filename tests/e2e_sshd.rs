//! End to end: `rowser ssh` through a real, throwaway, unprivileged `sshd`
//! on localhost, with the Daemon in-process on the test Opener and Notifier.
//!
//! The Remote host is this machine: the Shim finds the Session socket in
//! `/tmp`, as on a real Remote host. Runs only with `ROWSER_E2E=1` (Linux
//! CI and local runs); without it each test returns at once. Needs `sshd`
//! (in `/usr/sbin`, `/usr/bin` or on `PATH`), `ssh`, `ssh-keygen` and
//! `python3`. The tests hold one lock, so only one session set of this test
//! uses `/tmp` at a time and "the newest socket" is well defined. A real
//! `rowser ssh` that the user starts during the run can still make a newer
//! socket and break the reattach test.

#![cfg(target_os = "linux")]

mod common;
#[path = "common/script.rs"]
mod script;

use std::ffi::OsString;
use std::net::{IpAddr, Ipv6Addr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, NoticeHost, RELAY_SOCKET};
use rowser::wrapper::{run_ssh, WrapperEnv};
use script::{write_executable, write_script};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const TIMEOUT: Duration = Duration::from_secs(30);

fn enabled() -> bool {
    let on = std::env::var("ROWSER_E2E").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set ROWSER_E2E=1 to run the sshd end-to-end test");
    }
    on
}

fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap();
    assert!(status.success(), "{cmd:?}: {status}");
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Joins a session thread, or fails the test after `TIMEOUT`.
fn join(what: &str, session: std::thread::JoinHandle<ExitCode>) -> ExitCode {
    wait_until(&format!("{what} to end"), || session.is_finished());
    session.join().unwrap()
}

/// The `sshd` binary: `/usr/sbin` (Debian, Ubuntu), `/usr/bin` (Arch), or
/// the first on `PATH`.
fn sshd_path() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    ["/usr/sbin".into(), "/usr/bin".into()]
        .into_iter()
        .chain(std::env::split_paths(&path))
        .map(|d: PathBuf| d.join("sshd"))
        .find(|p| p.is_file())
        .expect("no sshd in /usr/sbin, /usr/bin or PATH")
}

/// A throwaway sshd, a Daemon and an ssh config for the Host aliases
/// `e2e-a`, `e2e-b` and `e2e-jump` (ProxyJump through `e2e-a`).
struct World {
    dir: TmpDir,
    sshd: Child,
    opener: RecordingOpener,
    notifier: RecordingNotifier,
    env: WrapperEnv,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.sshd.kill();
        let _ = self.sshd.wait();
        // sshd leaves the remote Session sockets behind. Remove only those
        // of this test's sessions: the `-R` specs that the ssh shim logged.
        let log = std::fs::read_to_string(self.dir.0.join("forwards")).unwrap_or_default();
        for spec in log.lines() {
            if let Some((remote, _)) = spec.split_once(':') {
                if remote.starts_with("/tmp/rowser-") {
                    let _ = std::fs::remove_file(remote);
                }
            }
        }
    }
}

impl World {
    async fn start() -> World {
        World::start_on("127.0.0.2").await
    }

    /// sshd listens on `addr` with a high port.
    async fn start_on(addr: &str) -> World {
        let dir = TmpDir::new("e");
        let d = dir.0.display().to_string();
        for key in ["host_key", "id"] {
            run(Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", ""])
                .arg("-f")
                .arg(dir.0.join(key)));
        }
        std::fs::copy(dir.0.join("id.pub"), dir.0.join("authorized_keys")).unwrap();
        // With OpenSSH 9.8+ the sshd listener is an ancestor of the remote
        // program. The /proc walk stops at sshd (#29), so a high sshd port on
        // 127.0.0.1 is not a Callback port.
        let port = TcpListener::bind((addr, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let host_pub = std::fs::read_to_string(dir.0.join("host_key.pub")).unwrap();
        let mut fields = host_pub.split_whitespace();
        let (kind, key) = (fields.next().unwrap(), fields.next().unwrap());
        std::fs::write(
            dir.0.join("known_hosts"),
            format!("[{addr}]:{port} {kind} {key}\n"),
        )
        .unwrap();
        std::fs::write(
            dir.0.join("sshd_config"),
            format!(
                "Port {port}\nListenAddress {addr}\nHostKey {d}/host_key\n\
                 PidFile {d}/sshd.pid\nAuthorizedKeysFile {d}/authorized_keys\n\
                 UsePAM no\nStrictModes no\nPasswordAuthentication no\n\
                 KbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
                 AllowTcpForwarding yes\nAllowStreamLocalForwarding yes\n\
                 StreamLocalBindUnlink yes\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.0.join("ssh_config"),
            format!(
                "Host e2e-jump\n  ProxyJump e2e-a\n\
                 Host *\n  HostName {addr}\n  Port {port}\n  IdentityFile {d}/id\n\
                 \x20 IdentitiesOnly yes\n  UserKnownHostsFile {d}/known_hosts\n\
                 \x20 GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n\
                 \x20 BatchMode yes\n  ExitOnForwardFailure yes\n  ControlMaster no\n\
                 \x20 ControlPath none\n  LogLevel ERROR\n"
            ),
        )
        .unwrap();
        let log = std::fs::File::create(dir.0.join("sshd.log")).unwrap();
        let sshd = Command::new(sshd_path())
            .arg("-D")
            .arg("-e")
            .arg("-f")
            .arg(dir.0.join("sshd_config"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        wait_until("sshd to listen", || {
            TcpStream::connect((addr, port)).is_ok()
        });

        let bin = dir.0.join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rowser"), bin.join("xdg-open")).unwrap();
        // The ssh that `rowser ssh` runs: it logs the `-R` spec (the first
        // two arguments), so Drop knows this test's Session sockets.
        let ssh_shim = dir.0.join("ssh-shim");
        write_script(
            &ssh_shim,
            &format!("[ \"$1\" = -R ] && echo \"$2\" >> {d}/forwards\nexec ssh \"$@\""),
        );

        let run_dir = dir.0.join("run");
        let (direct, relay, control) = bind_sockets(&run_dir).unwrap();
        let opener = RecordingOpener::default();
        let notifier = RecordingNotifier::default();
        // The local side of a Callback forward binds [::1] only, so the
        // remote program can listen on 127.0.0.1 with the same port.
        let config = DaemonConfig {
            loopback: vec![IpAddr::V6(Ipv6Addr::LOCALHOST)],
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(config, opener.clone(), notifier.clone());
        tokio::spawn(daemon.serve(direct, relay, control));
        let env = WrapperEnv {
            ssh: ssh_shim,
            relay_socket: run_dir.join(RELAY_SOCKET),
            session_dir: run_dir,
        };
        World {
            dir,
            sshd,
            opener,
            notifier,
            env,
        }
    }

    fn path(&self, name: &str) -> String {
        self.dir.0.join(name).display().to_string()
    }

    fn xdg_open(&self) -> String {
        self.path("bin/xdg-open")
    }

    /// `rowser ssh -F <config> <alias> <command>` in a thread.
    fn ssh(&self, alias: &str, command: &str) -> std::thread::JoinHandle<ExitCode> {
        let args: Vec<OsString> = vec![
            "-F".into(),
            self.path("ssh_config").into(),
            alias.into(),
            // Never the real Outpost socket of this user.
            format!("ROWSER_TEST_OUTPOST_DIR={} {command}", self.path("outpost")).into(),
        ];
        let env = self.env.clone();
        std::thread::spawn(move || run_ssh(args, env))
    }

    /// A script on the Remote host that writes `ready`, then waits for `done`.
    fn waiter(&self, ready: &str, done: &str) -> String {
        let path = self.dir.0.join(format!("wait-{ready}.sh"));
        let (r, d) = (self.path(ready), self.path(done));
        write_script(
            &path,
            &format!("touch {r}\nwhile [ ! -e {d} ]; do sleep 0.05; done"),
        );
        path.display().to_string()
    }

    fn urls(&self) -> Vec<String> {
        self.opener.urls().iter().map(|u| u.to_string()).collect()
    }

    fn hosts(&self) -> Vec<NoticeHost> {
        self.notifier
            .notices()
            .into_iter()
            .map(|n| n.host)
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn xdg_open_on_the_remote_host_opens_the_url_with_the_host_alias() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    let w = World::start().await;
    let session = w.ssh("e2e-a", &format!("{} https://example.test/", w.xdg_open()));
    assert_eq!(join("the session", session), ExitCode::SUCCESS);
    assert_eq!(w.urls(), ["https://example.test/"]);
    assert_eq!(w.hosts(), [NoticeHost::Alias("e2e-a".into())]);
}

/// The remote program: listens on 127.0.0.1, opens a URL whose
/// `redirect_uri` names its port, writes the port, then writes the request
/// line of the first request it gets and answers `ok`.
const OAUTH_PY: &str = r#"#!/usr/bin/env python3
import os, socket, subprocess, sys
xdg, port_file, request_file = sys.argv[1:4]
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(1)
port = s.getsockname()[1]
url = f"https://auth.example.test/authorize?redirect_uri=http://localhost:{port}/cb"
if subprocess.run([xdg, url]).returncode != 0:
    sys.exit(1)
with open(port_file + ".tmp", "w") as f:
    f.write(str(port))
os.rename(port_file + ".tmp", port_file)
s.settimeout(30)
c, _ = s.accept()
data = b""
while b"\r\n\r\n" not in data:
    chunk = c.recv(4096)
    if not chunk:
        break
    data += chunk
with open(request_file + ".tmp", "wb") as f:
    f.write(data.split(b"\r\n")[0])
os.rename(request_file + ".tmp", request_file)
c.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
c.close()
"#;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "the Wrapper no longer forwards callbacks (#79); move to a Link: #85"]
async fn an_oauth_callback_reaches_the_remote_program() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    oauth_callback(World::start().await).await;
}

/// #29: sshd on 127.0.0.1 with a high port. The /proc walk stops at sshd, so
/// the sshd port is not a Callback port, and the session ends when the
/// remote program stops listening (not at the 10-minute timeout).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "the Wrapper no longer forwards callbacks (#79); move to a Link: #85"]
async fn an_oauth_callback_works_when_sshd_listens_on_a_high_loopback_port() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    oauth_callback(World::start_on("127.0.0.1").await).await;
}

async fn oauth_callback(w: World) {
    let script = w.dir.0.join("oauth.py");
    write_executable(&script, OAUTH_PY);
    let (port_file, request_file) = (w.path("port"), w.path("request"));
    let session = w.ssh(
        "e2e-a",
        &format!(
            "{} {} {port_file} {request_file}",
            script.display(),
            w.xdg_open()
        ),
    );
    wait_until("the remote program's port", || {
        Path::new(&port_file).exists()
    });
    let port: u16 = std::fs::read_to_string(&port_file)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        w.urls(),
        [format!(
            "https://auth.example.test/authorize?redirect_uri=http://localhost:{port}/cb"
        )]
    );

    let mut browser = tokio::net::TcpStream::connect(("::1", port)).await.unwrap();
    browser
        .write_all(b"GET /cb?code=e2e HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    tokio::time::timeout(TIMEOUT, browser.read_to_string(&mut reply))
        .await
        .expect("no reply through the Callback forward")
        .unwrap();
    assert!(reply.ends_with("\r\n\r\nok"), "{reply:?}");
    assert_eq!(
        std::fs::read_to_string(&request_file).unwrap(),
        "GET /cb?code=e2e HTTP/1.1"
    );
    // A browser closes the connection after a `Connection: close` reply.
    // While it stays open, the splice holds the remote program's
    // side of the connection, and the forward (and so ssh) stays alive.
    drop(browser);
    assert_eq!(join("the session", session), ExitCode::SUCCESS);
    assert_eq!(w.hosts(), [NoticeHost::Alias("e2e-a".into())]);
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_reattach_the_shim_uses_the_newest_session_socket() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    let w = World::start().await;
    let first = w.ssh("e2e-a", &w.waiter("ready-a", "done"));
    wait_until("the first session", || {
        Path::new(&w.path("ready-a")).exists()
    });
    // mtimes must differ so that "newest" is well defined.
    std::thread::sleep(Duration::from_millis(50));
    let second = w.ssh("e2e-b", &w.waiter("ready-b", "done"));
    wait_until("the second session", || {
        Path::new(&w.path("ready-b")).exists()
    });

    // Like a tmux server started in the first session: a process in no
    // session, with no ROWSER_SOCKET, finds the Session socket in /tmp.
    let out = Command::new(w.xdg_open())
        .arg("https://example.test/reattach")
        .env_remove("ROWSER_SOCKET")
        // Never the real Outpost socket of this user.
        .env("ROWSER_TEST_OUTPOST_DIR", w.path("outpost"))
        // With no tunnel, a graphical session would run the real xdg-open.
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(w.urls(), ["https://example.test/reattach"]);
    assert_eq!(w.hosts(), [NoticeHost::Alias("e2e-b".into())]);

    std::fs::write(w.path("done"), "").unwrap();
    assert_eq!(join("the first session", first), ExitCode::SUCCESS);
    assert_eq!(join("the second session", second), ExitCode::SUCCESS);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxyjump_session_opens_the_url_with_the_target_alias() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    let w = World::start().await;
    let session = w.ssh(
        "e2e-jump",
        &format!("{} https://example.test/jump", w.xdg_open()),
    );
    assert_eq!(join("the session", session), ExitCode::SUCCESS);
    // The hop: one login to e2e-a (the jump host), one to the target.
    let log = std::fs::read_to_string(w.path("sshd.log")).unwrap();
    assert_eq!(log.matches("Accepted publickey").count(), 2, "{log}");
    assert_eq!(w.urls(), ["https://example.test/jump"]);
    assert_eq!(w.hosts(), [NoticeHost::Alias("e2e-jump".into())]);
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_install_puts_the_binary_and_links_in_the_remote_home() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    let w = World::start().await;
    // sshd sets HOME from the passwd entry, so the `ssh` wrapper puts a temp
    // HOME in front of the remote command (`ssh -- <host> <command>`). The
    // install never touches the real home of the test user. The remote PATH is
    // a dir of linked tools only, so the real `xdg-mime` never runs and the
    // install writes `mimeapps.list` in the temp HOME.
    let home = w.dir.0.join("remote-home");
    std::fs::create_dir(&home).unwrap();
    let tools = w.dir.0.join("toolbox");
    std::fs::create_dir(&tools).unwrap();
    let real_path = std::env::var("PATH").unwrap();
    for tool in [
        "sh", "mkdir", "rm", "cat", "chmod", "mv", "ln", "head", "awk", "uname",
    ] {
        let found = std::env::split_paths(&real_path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no {tool} on PATH"));
        std::os::unix::fs::symlink(found, tools.join(tool)).unwrap();
    }
    let wrap = w.dir.0.join("wrap");
    std::fs::create_dir(&wrap).unwrap();
    let ssh = wrap.join("ssh");
    write_script(
        &ssh,
        &format!(
            "[ \"$1\" = -- ] && [ $# -eq 3 ] || exit 99\n\
             exec /usr/bin/ssh -F {} -- \"$2\" \"HOME={} PATH={} $3\"",
            w.path("ssh_config"),
            home.display(),
            tools.display()
        ),
    );
    let path = format!("{}:{}", wrap.display(), std::env::var("PATH").unwrap());

    let out = Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["remote", "install", "e2e-a", "--local-build"])
        .arg(env!("CARGO_BIN_EXE_rowser"))
        .env("PATH", path)
        // No Daemon runs there, so the install tells none to reconnect.
        .env("ROWSER_RUNTIME_DIR", home.join("run"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let bin = home.join(".local/bin");
    assert_eq!(
        std::fs::read(bin.join("rowser")).unwrap(),
        std::fs::read(env!("CARGO_BIN_EXE_rowser")).unwrap()
    );
    let mode = std::fs::metadata(bin.join("rowser"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    for name in ["xdg-open", "sensible-browser", "www-browser"] {
        assert_eq!(
            std::fs::read_link(bin.join(name)).unwrap(),
            Path::new("rowser")
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("export BROWSER=rowser"), "{stdout}");
    assert!(home
        .join(".local/share/applications/rowser.desktop")
        .is_file());
    assert_eq!(
        std::fs::read_to_string(home.join(".config/mimeapps.list")).unwrap(),
        "[Default Applications]\nx-scheme-handler/http=rowser.desktop\n\
         x-scheme-handler/https=rowser.desktop\n"
    );
    assert!(stdout.contains("in ~/.config/mimeapps.list"), "{stdout}");
}
