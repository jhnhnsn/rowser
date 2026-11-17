//! `rowser doctor`: checks the Remote host setup and does a live round trip.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cli::BROWSER_NAMES;
use crate::discovery::find_tunnel_path;
use crate::protocol::{Hello, Msg};

/// The result of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

/// One reported item: a status and a line of detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub status: Status,
    pub name: &'static str,
    pub detail: String,
}

impl Check {
    fn new(status: Status, name: &'static str, detail: impl Into<String>) -> Check {
        Check {
            status,
            name,
            detail: detail.into(),
        }
    }
}

/// What `rowser doctor` reads from its environment.
pub struct DoctorEnv<W: Write> {
    /// `PATH`.
    pub path: Option<OsString>,
    /// `BROWSER`.
    pub browser: Option<OsString>,
    /// `HOME`.
    pub home: Option<PathBuf>,
    /// Directory searched for `rowser-*.sock`.
    pub tunnel_dir: PathBuf,
    /// `ROWSER_SOCKET`, tried before the directory.
    pub socket_override: Option<PathBuf>,
    pub stdout: W,
}

impl DoctorEnv<std::io::Stdout> {
    /// The environment of the running process.
    pub fn from_process() -> DoctorEnv<std::io::Stdout> {
        DoctorEnv {
            path: std::env::var_os("PATH"),
            browser: std::env::var_os("BROWSER"),
            home: std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            tunnel_dir: PathBuf::from(crate::shim::TUNNEL_DIR),
            socket_override: std::env::var_os("ROWSER_SOCKET")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            stdout: std::io::stdout(),
        }
    }
}

/// `~/.local/bin` must come before `/usr/bin` on `PATH`, so the symlinks
/// win over the system `xdg-open`.
pub fn check_path(path: Option<&OsStr>, home: &Path) -> Check {
    const NAME: &str = "PATH";
    let local = home.join(".local/bin");
    let entries: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let local_at = entries.iter().position(|e| *e == local);
    let usr_at = entries.iter().position(|e| e == Path::new("/usr/bin"));
    match (local_at, usr_at) {
        (None, _) => Check::new(
            Status::Warn,
            NAME,
            format!("{} is not on PATH", local.display()),
        ),
        (Some(l), Some(u)) if u < l => Check::new(
            Status::Warn,
            NAME,
            format!("{} comes after /usr/bin", local.display()),
        ),
        _ => Check::new(
            Status::Ok,
            NAME,
            format!("{} is before /usr/bin", local.display()),
        ),
    }
}

/// True when `path` resolves to a file named `rowser`.
fn is_rowser(path: &Path) -> bool {
    std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n == "rowser"))
        .unwrap_or(false)
}

/// The browser symlinks in `~/.local/bin` must resolve to rowser.
pub fn check_symlinks(home: &Path) -> Check {
    const NAME: &str = "symlinks";
    let bin = home.join(".local/bin");
    let bad: Vec<&str> = BROWSER_NAMES
        .iter()
        .copied()
        .filter(|n| !is_rowser(&bin.join(n)))
        .collect();
    if bad.is_empty() {
        Check::new(
            Status::Ok,
            NAME,
            format!("{} resolve to rowser", BROWSER_NAMES.join(", ")),
        )
    } else {
        Check::new(
            Status::Warn,
            NAME,
            format!(
                "{} in {} do not resolve to rowser",
                bad.join(", "),
                bin.display()
            ),
        )
    }
}

/// `BROWSER` must name rowser (or one of its symlinks), directly or
/// through `PATH`.
pub fn check_browser(browser: Option<&OsStr>, path: Option<&OsStr>) -> Check {
    const NAME: &str = "BROWSER";
    let Some(value) = browser.filter(|v| !v.is_empty()) else {
        return Check::new(Status::Warn, NAME, "BROWSER is not set");
    };
    let shown = value.to_string_lossy();
    let command = Path::new(value);
    let resolves = if command.components().count() > 1 {
        is_rowser(command)
    } else {
        path.map(|p| std::env::split_paths(p).any(|d| is_rowser(&d.join(command))))
            .unwrap_or(false)
    };
    if resolves {
        Check::new(Status::Ok, NAME, format!("BROWSER={shown} is rowser"))
    } else {
        Check::new(
            Status::Warn,
            NAME,
            format!("BROWSER={shown} does not resolve to rowser"),
        )
    }
}

/// Finds a Tunnel and sends `Ping`; reports the Daemon's protocol version
/// and any version skew.
pub async fn check_tunnel(tunnel_dir: &Path, socket_override: Option<PathBuf>) -> Vec<Check> {
    const NAME: &str = "tunnel";
    let (mut conn, socket) = match find_tunnel_path(tunnel_dir, socket_override).await {
        Ok(found) => found,
        Err(e) => {
            let mut out = vec![];
            if let Some(m) = e.mismatch {
                out.push(Check::new(Status::Fail, "version", m.to_string()));
            }
            out.push(Check::new(
                Status::Fail,
                NAME,
                "no tunnel; connect with `rowser ssh <host>` or set ROWSER_SOCKET",
            ));
            return out;
        }
    };
    let fail = |detail: String| vec![Check::new(Status::Fail, NAME, detail)];
    if let Err(e) = conn.send(&Msg::Ping).await {
        return fail(format!("{}: {e}", socket.display()));
    }
    let version = match conn.recv().await {
        Ok(Some(Msg::Pong { version })) => version,
        Ok(Some(other)) => return fail(format!("unexpected reply from daemon: {other:?}")),
        Ok(None) => return fail("daemon closed the connection".into()),
        Err(e) => return fail(format!("{}: {e}", socket.display())),
    };
    let ours = Hello::current().version();
    let mut out = vec![Check::new(
        Status::Ok,
        NAME,
        format!("{}: daemon protocol {version}", socket.display()),
    )];
    if version == ours {
        out.push(Check::new(
            Status::Ok,
            "version",
            format!("daemon and this rowser speak {ours}"),
        ));
    } else {
        out.push(Check::new(
            Status::Warn,
            "version",
            format!(
                "version skew: daemon speaks {version}, this rowser speaks {ours}; \
                 update with `rowser remote install <host>` or reinstall rowser locally"
            ),
        ));
    }
    out
}

/// Runs every check, prints one line each, and exits 1 when any fails.
pub async fn run_doctor<W: Write>(env: &mut DoctorEnv<W>) -> ExitCode {
    let mut checks = vec![];
    match &env.home {
        Some(home) => {
            checks.push(check_path(env.path.as_deref(), home));
            checks.push(check_symlinks(home));
        }
        None => checks.push(Check::new(
            Status::Warn,
            "PATH",
            "HOME is not set; cannot check ~/.local/bin",
        )),
    }
    checks.push(check_browser(env.browser.as_deref(), env.path.as_deref()));
    checks.extend(check_tunnel(&env.tunnel_dir, env.socket_override.clone()).await);
    for c in &checks {
        let _ = writeln!(
            env.stdout,
            "{:<4} {}: {}",
            c.status.label(),
            c.name,
            c.detail
        );
    }
    if checks.iter().any(|c| c.status == Status::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `rowser doctor` for the binary.
pub fn doctor_main() -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(run_doctor(&mut DoctorEnv::from_process())),
        Err(e) => {
            eprintln!("rowser: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/u";

    fn path(p: &str) -> Check {
        check_path(Some(p.as_ref()), Path::new(HOME))
    }

    #[test]
    fn path_with_local_bin_first_is_ok() {
        assert_eq!(path("/home/u/.local/bin:/usr/bin").status, Status::Ok);
    }

    #[test]
    fn path_without_usr_bin_is_ok() {
        assert_eq!(path("/bin:/home/u/.local/bin").status, Status::Ok);
    }

    #[test]
    fn path_with_local_bin_after_usr_bin_warns() {
        let c = path("/usr/bin:/home/u/.local/bin");
        assert_eq!(c.status, Status::Warn);
        assert_eq!(c.detail, "/home/u/.local/bin comes after /usr/bin");
    }

    #[test]
    fn path_without_local_bin_warns() {
        let c = path("/usr/local/bin:/usr/bin");
        assert_eq!(c.status, Status::Warn);
        assert_eq!(c.detail, "/home/u/.local/bin is not on PATH");
        assert_eq!(check_path(None, Path::new(HOME)).status, Status::Warn);
    }

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A home with `~/.local/bin/rowser` and, when `links`, its symlinks.
    fn home(tag: &str, links: bool) -> Tmp {
        let dir = PathBuf::from(format!("/tmp/rw.doc{tag}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("rowser"), "").unwrap();
        if links {
            for n in BROWSER_NAMES {
                std::os::unix::fs::symlink("rowser", bin.join(n)).unwrap();
            }
        }
        Tmp(dir)
    }

    #[test]
    fn symlinks_to_rowser_are_ok() {
        let h = home("s", true);
        assert_eq!(check_symlinks(&h.0).status, Status::Ok);
    }

    #[test]
    fn missing_symlinks_warn_by_name() {
        let h = home("m", false);
        let bin = h.0.join(".local/bin");
        std::os::unix::fs::symlink("rowser", bin.join("xdg-open")).unwrap();
        std::os::unix::fs::symlink("/bin/true", bin.join("www-browser")).unwrap();
        let c = check_symlinks(&h.0);
        assert_eq!(c.status, Status::Warn);
        assert!(
            c.detail.starts_with("sensible-browser, www-browser in "),
            "{}",
            c.detail
        );
    }

    #[test]
    fn unset_or_empty_browser_warns() {
        let c = check_browser(None, None);
        assert_eq!(
            (c.status, c.detail.as_str()),
            (Status::Warn, "BROWSER is not set")
        );
        assert_eq!(check_browser(Some("".as_ref()), None).status, Status::Warn);
    }

    #[test]
    fn browser_found_on_path_is_ok() {
        let h = home("b", true);
        let p = h.0.join(".local/bin").into_os_string();
        assert_eq!(
            check_browser(Some("rowser".as_ref()), Some(&p)).status,
            Status::Ok
        );
        assert_eq!(
            check_browser(Some("xdg-open".as_ref()), Some(&p)).status,
            Status::Ok
        );
    }

    #[test]
    fn browser_as_a_path_to_rowser_is_ok() {
        let h = home("a", true);
        let b = h.0.join(".local/bin/rowser").into_os_string();
        assert_eq!(check_browser(Some(&b), None).status, Status::Ok);
    }

    #[test]
    fn browser_naming_another_program_warns() {
        let h = home("o", true);
        let p = h.0.join(".local/bin").into_os_string();
        let c = check_browser(Some("firefox".as_ref()), Some(&p));
        assert_eq!(c.status, Status::Warn);
        assert_eq!(c.detail, "BROWSER=firefox does not resolve to rowser");
    }
}
