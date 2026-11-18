//! End to end: `rowser ssh` through a real, throwaway, unprivileged `sshd`
//! on localhost, with the Daemon in-process on the test Opener and Notifier.
//!
//! The Remote host is this machine: the Shim finds the Session socket in
//! `/tmp`, as on a real Remote host. Runs only with `ROWSER_E2E=1` (Linux
//! CI and local runs); needs `/usr/bin/sshd`, `ssh`, `ssh-keygen` and
//! `python3`. The tests hold one lock, so only one session set uses `/tmp`
//! at a time and "the newest socket" is well defined.

#![cfg(target_os = "linux")]

mod common;

use std::collections::HashSet;
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

/// The `rowser-*.sock` files in `/tmp` now.
fn tmp_sockets() -> HashSet<PathBuf> {
    std::fs::read_dir("/tmp")
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("rowser-") && name.ends_with(".sock")
        })
        .collect()
}

/// A throwaway sshd, a Daemon and an ssh config for the Host aliases
/// `e2e-a`, `e2e-b` and `e2e-jump` (ProxyJump through `e2e-a`).
struct World {
    dir: TmpDir,
    sshd: Child,
    opener: RecordingOpener,
    notifier: RecordingNotifier,
    env: WrapperEnv,
    before: HashSet<PathBuf>,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.sshd.kill();
        let _ = self.sshd.wait();
        // sshd leaves the remote Session sockets behind.
        for p in tmp_sockets().difference(&self.before) {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl World {
    async fn start() -> World {
        let dir = TmpDir::new("e");
        let d = dir.0.display().to_string();
        let before = tmp_sockets();
        for key in ["host_key", "id"] {
            run(Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", ""])
                .arg("-f")
                .arg(dir.0.join(key)));
        }
        std::fs::copy(dir.0.join("id.pub"), dir.0.join("authorized_keys")).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let host_pub = std::fs::read_to_string(dir.0.join("host_key.pub")).unwrap();
        let mut fields = host_pub.split_whitespace();
        let (kind, key) = (fields.next().unwrap(), fields.next().unwrap());
        std::fs::write(
            dir.0.join("known_hosts"),
            format!("[127.0.0.1]:{port} {kind} {key}\n"),
        )
        .unwrap();
        std::fs::write(
            dir.0.join("sshd_config"),
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\n\
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
                 Host *\n  HostName 127.0.0.1\n  Port {port}\n  IdentityFile {d}/id\n\
                 \x20 IdentitiesOnly yes\n  UserKnownHostsFile {d}/known_hosts\n\
                 \x20 GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n\
                 \x20 BatchMode yes\n  ExitOnForwardFailure yes\n  ControlMaster no\n\
                 \x20 ControlPath none\n  LogLevel ERROR\n"
            ),
        )
        .unwrap();
        let log = std::fs::File::create(dir.0.join("sshd.log")).unwrap();
        let sshd = Command::new("/usr/bin/sshd")
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
            TcpStream::connect(("127.0.0.1", port)).is_ok()
        });

        let bin = dir.0.join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rowser"), bin.join("xdg-open")).unwrap();

        let run_dir = dir.0.join("run");
        let (direct, relay) = bind_sockets(&run_dir).unwrap();
        let opener = RecordingOpener::default();
        let notifier = RecordingNotifier::default();
        // The local side of a Callback forward binds [::1] only, so the
        // remote program can listen on 127.0.0.1 with the same port.
        let config = DaemonConfig {
            loopback: vec![IpAddr::V6(Ipv6Addr::LOCALHOST)],
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(config, opener.clone(), notifier.clone());
        tokio::spawn(daemon.serve(direct, relay));
        let env = WrapperEnv {
            ssh: "ssh".into(),
            relay_socket: run_dir.join(RELAY_SOCKET),
            session_dir: run_dir,
        };
        World {
            dir,
            sshd,
            opener,
            notifier,
            env,
            before,
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
            command.into(),
        ];
        let env = self.env.clone();
        std::thread::spawn(move || run_ssh(args, env))
    }

    /// A script on the Remote host that writes `ready`, then waits for `done`.
    fn waiter(&self, ready: &str, done: &str) -> String {
        let path = self.dir.0.join(format!("wait-{ready}.sh"));
        let (r, d) = (self.path(ready), self.path(done));
        std::fs::write(
            &path,
            format!("#!/bin/sh\ntouch {r}\nwhile [ ! -e {d} ]; do sleep 0.05; done\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    assert_eq!(session.join().unwrap(), ExitCode::SUCCESS);
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
async fn an_oauth_callback_reaches_the_remote_program() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    let w = World::start().await;
    let script = w.dir.0.join("oauth.py");
    std::fs::write(&script, OAUTH_PY).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    assert_eq!(session.join().unwrap(), ExitCode::SUCCESS);
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
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(w.urls(), ["https://example.test/reattach"]);
    assert_eq!(w.hosts(), [NoticeHost::Alias("e2e-b".into())]);

    std::fs::write(w.path("done"), "").unwrap();
    assert_eq!(first.join().unwrap(), ExitCode::SUCCESS);
    assert_eq!(second.join().unwrap(), ExitCode::SUCCESS);
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
    assert_eq!(session.join().unwrap(), ExitCode::SUCCESS);
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
    // install never touches the real home of the test user.
    let home = w.dir.0.join("remote-home");
    std::fs::create_dir(&home).unwrap();
    let wrap = w.dir.0.join("wrap");
    std::fs::create_dir(&wrap).unwrap();
    let ssh = wrap.join("ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\n[ \"$1\" = -- ] && [ $# -eq 3 ] || exit 99\n\
             exec /usr/bin/ssh -F {} -- \"$2\" \"HOME={} $3\"\n",
            w.path("ssh_config"),
            home.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", wrap.display(), std::env::var("PATH").unwrap());

    let out = Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["remote", "install", "e2e-a", "--local-build"])
        .arg(env!("CARGO_BIN_EXE_rowser"))
        .env("PATH", path)
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
}
