//! End to end through a real, throwaway, unprivileged `sshd` on localhost:
//! `rowser remote install`, and a Link from a Daemon to a Saved host that
//! `rowser host add` installs.
//!
//! The Remote host is this machine. Runs only with `ROWSER_E2E=1` (Linux CI
//! and local runs); without it each test returns at once. Needs `sshd` (in
//! `/usr/sbin`, `/usr/bin` or on `PATH`), `ssh`, `ssh-keygen`, `bash` and
//! `setsid`. Each remote command runs under a temp HOME set inside the
//! command, never through `SetEnv`.

#![cfg(target_os = "linux")]

mod common;
#[path = "common/script.rs"]
mod script;

use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig};
use rowser::protocol::{Conn, Hello};
use script::write_script;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

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

/// A throwaway sshd and an ssh config for the Host alias `e2e-a`.
struct World {
    dir: TmpDir,
    sshd: Child,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.sshd.kill();
        let _ = self.sshd.wait();
    }
}

impl World {
    /// sshd listens on 127.0.0.2 with a high port.
    fn start() -> World {
        let addr = "127.0.0.2";
        let dir = TmpDir::new("e");
        let d = dir.0.display().to_string();
        for key in ["host_key", "id"] {
            run(Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", ""])
                .arg("-f")
                .arg(dir.0.join(key)));
        }
        std::fs::copy(dir.0.join("id.pub"), dir.0.join("authorized_keys")).unwrap();
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
                 KbdInteractiveAuthentication no\nPubkeyAuthentication yes\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.0.join("ssh_config"),
            format!(
                "Host *\n  HostName {addr}\n  Port {port}\n  IdentityFile {d}/id\n\
                 \x20 IdentitiesOnly yes\n  UserKnownHostsFile {d}/known_hosts\n\
                 \x20 GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n\
                 \x20 BatchMode yes\n  ControlMaster no\n\
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
        World { dir, sshd }
    }

    fn path(&self, name: &str) -> String {
        self.dir.0.join(name).display().to_string()
    }

    /// A dir of links to the tools a remote install needs, and no more: the
    /// real `xdg-mime` and `rowser` are not on it.
    fn toolbox(&self) -> PathBuf {
        let tools = self.dir.0.join("toolbox");
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
        tools
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_install_puts_the_binary_and_links_in_the_remote_home() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    let w = World::start();
    // sshd sets HOME from the passwd entry, so the `ssh` wrapper puts a temp
    // HOME in front of the remote command (`ssh -- <host> <command>`). The
    // install never touches the real home of the test user. The remote PATH is
    // a dir of linked tools only, so the real `xdg-mime` never runs and the
    // install writes `mimeapps.list` in the temp HOME.
    let home = w.dir.0.join("remote-home");
    std::fs::create_dir(&home).unwrap();
    let tools = w.toolbox();
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

/// The pid the remote command of the Link wrote (the Outpost's own pid).
fn outpost_pid(file: &Path) -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(file).ok()?.trim().parse().ok()?;
    // Never signal init or a process group.
    (pid > 1).then_some(pid)
}

/// Waits until the Outpost socket at `socket` answers a handshake.
async fn wait_connected(socket: &Path) {
    let end = Instant::now() + TIMEOUT;
    while Instant::now() < end {
        if let Ok(s) = tokio::net::UnixStream::connect(socket).await {
            if Conn::connect(s, Hello::current()).await.is_ok() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the Link did not connect");
}

/// A port that is free on 127.0.0.1 and on `[::1]`.
fn free_port() -> u16 {
    loop {
        let v4 = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        if TcpListener::bind(SocketAddr::new(V6, port)).is_ok() {
            return port;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_through_sshd_opens_a_detached_shim_url_and_forwards_the_callback() {
    if !enabled() {
        return;
    }
    let _lock = LOCK.lock().await;
    let w = World::start();
    let d = &w.dir.0;
    let local_home = d.join("local-home");
    let home = d.join("remote-home");
    let outpost = d.join("o");
    let pidfile = d.join("outpost.pid");
    for dir in [&local_home, &home] {
        std::fs::create_dir(dir).unwrap();
    }
    let tools = w.toolbox();
    // Every `ssh` (from `rowser host add` on PATH, and the Daemon's Link)
    // goes to the throwaway sshd with the temp config only. The remote
    // command goes in a file that the remote `/bin/sh` runs under `env -i`
    // with a temp HOME, the toolbox PATH and the test Outpost socket folder.
    let wrap = d.join("wrap");
    std::fs::create_dir(&wrap).unwrap();
    let ssh = wrap.join("ssh");
    script::write_executable(
        &ssh,
        &format!(
            "#!/bin/bash\n\
             a=(\"$@\")\n\
             n=1\n\
             [ \"${{a[-1]}}\" = __outpost ] && n=2\n\
             cmd=\"${{a[*]: -n}}\"\n\
             f=$(mktemp {d}/cmd.XXXXXX)\n\
             if [ $n = 2 ]; then printf 'echo $$ > %s\\nexec %s\\n' {pid} \"$cmd\" > \"$f\";\n\
             else printf '%s\\n' \"$cmd\" > \"$f\"; fi\n\
             exec /usr/bin/ssh -F {cfg} \"${{a[@]:0:${{#a[@]}}-n}}\" \
             \"env -i HOME={home} PATH={tools} ROWSER_TEST_OUTPOST_DIR={outpost} \
             XDG_CONFIG_HOME={home}/.config DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent/bus \
             /bin/sh $f\"\n",
            d = d.display(),
            pid = pidfile.display(),
            cfg = w.path("ssh_config"),
            home = home.display(),
            tools = tools.display(),
            outpost = outpost.display(),
        ),
    );
    let run = d.join("run");
    let config_file = d.join("cfg").join("rowser").join("config.toml");

    // A Daemon with no Saved host yet; `host add` makes it reload.
    let opener = RecordingOpener::default();
    let control = bind_sockets(&run).unwrap();
    let daemon = Daemon::new(
        DaemonConfig {
            ssh: ssh.clone(),
            link_dir: run.clone(),
            loopback: vec![V6],
            config_file: Some(config_file.clone()),
            ..DaemonConfig::default()
        },
        opener.clone(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve(control));

    let out = tokio::task::block_in_place(|| {
        Command::new(env!("CARGO_BIN_EXE_rowser"))
            .args(["host", "add", "e2e-a"])
            .env("PATH", format!("{}:/usr/bin:/bin", wrap.display()))
            .env("HOME", &local_home)
            .env("XDG_CONFIG_HOME", d.join("cfg"))
            .env("ROWSER_RUNTIME_DIR", &run)
            .env("ROWSER_TEST_LOCAL_BUILD", env!("CARGO_BIN_EXE_rowser"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    });
    assert!(out.status.success(), "{out:?}");
    let bin = home.join(".local/bin");
    assert_eq!(
        std::fs::read(bin.join("rowser")).unwrap(),
        std::fs::read(env!("CARGO_BIN_EXE_rowser")).unwrap()
    );
    let config = std::fs::read_to_string(&config_file).unwrap();
    assert!(config.contains("name = \"e2e-a\""), "{config}");
    assert!(
        config.contains("remote_path = \"~/.local/bin/rowser\""),
        "{config}"
    );

    let socket = outpost.join("outpost.sock");
    wait_connected(&socket).await;
    let first = outpost_pid(&pidfile).expect("no Outpost pid");

    // A detached Shim: its own session (setsid), no SSH session or desktop
    // in its environment, like a cron job or a background agent.
    let real_path = std::env::var("PATH").unwrap();
    let setsid = std::env::split_paths(&real_path)
        .map(|d| d.join("setsid"))
        .find(|p| p.is_file())
        .expect("no setsid on PATH");
    let detached = |url: &str| {
        let out = Command::new(&setsid)
            .arg("-w")
            .arg(bin.join("xdg-open"))
            .arg(url)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", format!("{}:{}", bin.display(), tools.display()))
            .env("ROWSER_TEST_OUTPOST_DIR", &outpost)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };

    let port = free_port();
    let remote = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let url = format!("https://example.com/auth?redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2Fcb");
    tokio::task::block_in_place(|| detached(&url));
    assert_eq!(opener.urls(), [Url::parse(&url).unwrap()]);

    // The browser's callback reaches the remote program and its reply comes back.
    let browser = async {
        let mut b = tokio::net::TcpStream::connect(SocketAddr::new(V6, port))
            .await
            .unwrap();
        b.write_all(b"GET /cb?code=42").await.unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(TIMEOUT, b.read_to_end(&mut got))
            .await
            .expect("the relay did not close")
            .unwrap();
        got
    };
    let program = async {
        let (mut s, _) = remote.accept().await.unwrap();
        let mut buf = [0u8; 15];
        s.read_exact(&mut buf).await.unwrap();
        s.write_all(b"HTTP/1.1 200 OK").await.unwrap();
        buf
    };
    let (got, asked) = tokio::join!(browser, program);
    assert_eq!(&asked, b"GET /cb?code=42");
    assert_eq!(got, b"HTTP/1.1 200 OK");

    // The Link drops (the Outpost dies) and comes back after the backoff.
    unsafe { libc::kill(first, libc::SIGKILL) };
    let end = Instant::now() + TIMEOUT;
    while outpost_pid(&pidfile) == Some(first) {
        assert!(Instant::now() < end, "the Link did not come back");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    wait_connected(&socket).await;
    let again = "https://example.com/again";
    tokio::task::block_in_place(|| detached(again));
    assert_eq!(opener.urls().last(), Some(&Url::parse(again).unwrap()));
}
