//! End to end: `rowser remote install` through a real, throwaway,
//! unprivileged `sshd` on localhost.
//!
//! The Remote host is this machine. Runs only with `ROWSER_E2E=1` (Linux CI
//! and local runs); without it each test returns at once. Needs `sshd` (in
//! `/usr/sbin`, `/usr/bin` or on `PATH`), `ssh` and `ssh-keygen`. The Link
//! end-to-end test through sshd is #84.

#![cfg(target_os = "linux")]

mod common;
#[path = "common/script.rs"]
mod script;

use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::TmpDir;
use script::write_script;

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
