//! `rowser doctor`: checks the Remote host setup and does a live round trip.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::discovery::find_tunnel_path;
use crate::platform::{BROWSER_NAMES, TUNNEL_DIR};
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
    /// `XDG_CONFIG_HOME`, else `$HOME/.config`.
    pub config_home: Option<PathBuf>,
    /// `XDG_DATA_HOME`, else `$HOME/.local/share`.
    pub data_home: Option<PathBuf>,
    /// `WAYLAND_DISPLAY` or `DISPLAY` is set.
    pub graphical: bool,
    /// This rowser, which the fallback `xdg-open` must not be.
    pub self_exe: Option<PathBuf>,
    pub stdout: W,
}

/// An absolute `XDG_*` directory from `var`, else `home/default`.
fn xdg_dir(var: &str, home: Option<&Path>, default: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(default)))
}

impl DoctorEnv<std::io::Stdout> {
    /// The environment of the running process.
    pub fn from_process() -> DoctorEnv<std::io::Stdout> {
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        DoctorEnv {
            path: std::env::var_os("PATH"),
            browser: std::env::var_os("BROWSER"),
            config_home: xdg_dir("XDG_CONFIG_HOME", home.as_deref(), ".config"),
            data_home: xdg_dir("XDG_DATA_HOME", home.as_deref(), ".local/share"),
            graphical: ["WAYLAND_DISPLAY", "DISPLAY"]
                .iter()
                .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty())),
            self_exe: std::env::current_exe().ok(),
            home,
            tunnel_dir: PathBuf::from(TUNNEL_DIR),
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

/// True when `path` is a file with an execute bit, as a shell `PATH` search needs.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
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
        // A shell runs only the first executable match, so test only that one.
        path.and_then(|p| {
            std::env::split_paths(p)
                .map(|d| d.join(command))
                .find(|f| is_executable(f))
        })
        .is_some_and(|f| is_rowser(&f))
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

/// The fix for each default-browser warning, appended to its detail.
pub const FIX: &str = "; fix: run `rowser remote install <host>` on the Local machine";

fn warn_fix(name: &'static str, detail: String) -> Check {
    Check::new(Status::Warn, name, format!("{detail}{FIX}"))
}

/// `<config_home>/environment.d/rowser.conf` must exist and set `BROWSER`
/// and `PATH`, so systemd user services get rowser too.
pub fn check_environment_d(config_home: &Path) -> Check {
    const NAME: &str = "environment.d";
    let f = config_home.join("environment.d/rowser.conf");
    let Ok(text) = std::fs::read_to_string(&f) else {
        return warn_fix(NAME, format!("{} does not exist", f.display()));
    };
    let sets = |key: &str| {
        text.lines().any(|l| {
            l.trim_start()
                .strip_prefix(key)
                .is_some_and(|r| r.starts_with('='))
        })
    };
    for key in ["BROWSER", "PATH"] {
        if !sets(key) {
            return warn_fix(NAME, format!("{} does not set {key}", f.display()));
        }
    }
    Check::new(
        Status::Ok,
        NAME,
        format!("{} sets BROWSER and PATH", f.display()),
    )
}

/// `<data_home>/applications/rowser.desktop` must exist.
pub fn check_desktop(data_home: &Path) -> Check {
    const NAME: &str = "rowser.desktop";
    let f = data_home.join("applications/rowser.desktop");
    if f.is_file() {
        Check::new(Status::Ok, NAME, format!("{} exists", f.display()))
    } else {
        warn_fix(NAME, format!("{} does not exist", f.display()))
    }
}

const HTTPS_TYPE: &str = "x-scheme-handler/https";

/// The first handler for `https` under `[Default Applications]` in a `mimeapps.list`.
fn mimeapps_default(text: &str) -> Option<String> {
    let mut in_defaults = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_defaults = line == "[Default Applications]";
        } else if in_defaults {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == HTTPS_TYPE {
                    return v
                        .split(';')
                        .map(str::trim)
                        .find(|s| !s.is_empty())
                        .map(String::from);
                }
            }
        }
    }
    None
}

/// The default handler for `https` must be `rowser.desktop`: from
/// `xdg-mime query default` when `xdg-mime` is on `path`, else from
/// `<config_home>/mimeapps.list`.
pub fn check_default_handler(path: Option<&OsStr>, config_home: &Path) -> Check {
    const NAME: &str = "default browser";
    let xdg_mime = path.and_then(|p| {
        std::env::split_paths(p)
            .filter(|d| d.is_absolute())
            .map(|d| d.join("xdg-mime"))
            .find(|f| is_executable(f))
    });
    let (source, found) = match xdg_mime {
        Some(prog) => {
            let mut cmd = std::process::Command::new(prog);
            cmd.args(["query", "default", HTTPS_TYPE]);
            if let Some(p) = path {
                cmd.env("PATH", p);
            }
            let found = cmd
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty());
            ("xdg-mime".to_string(), found)
        }
        None => {
            let m = config_home.join("mimeapps.list");
            let found = std::fs::read_to_string(&m)
                .ok()
                .and_then(|t| mimeapps_default(&t));
            (m.display().to_string(), found)
        }
    };
    match found.as_deref() {
        Some("rowser.desktop") => Check::new(
            Status::Ok,
            NAME,
            format!("{HTTPS_TYPE} opens rowser.desktop ({source})"),
        ),
        Some(other) => warn_fix(
            NAME,
            format!("{HTTPS_TYPE} opens {other}, not rowser.desktop ({source})"),
        ),
        None => warn_fix(NAME, format!("no default for {HTTPS_TYPE} ({source})")),
    }
}

/// With a graphical session, a real `xdg-open` (not rowser) must be on
/// `path`, for the Shim's local fallback.
pub fn check_fallback(graphical: bool, path: Option<&OsStr>, self_exe: Option<&Path>) -> Check {
    const NAME: &str = "fallback";
    if !graphical {
        return Check::new(Status::Ok, NAME, "no graphical session; no fallback needed");
    }
    match path.and_then(|p| crate::shim::real_xdg_open(p, self_exe)) {
        Some(real) => Check::new(
            Status::Ok,
            NAME,
            format!("{} opens URLs when no tunnel answers", real.display()),
        ),
        None => Check::new(
            Status::Warn,
            NAME,
            "graphical session but no real xdg-open on PATH; \
             fix: install xdg-utils on this host",
        ),
    }
}

/// Finds a Tunnel and sends `Ping`; reports the Daemon's protocol version
/// and any version skew.
pub async fn check_tunnel(tunnel_dir: &Path, socket_override: Option<PathBuf>) -> Vec<Check> {
    const NAME: &str = "tunnel";
    let (mut conn, socket) = match find_tunnel_path(tunnel_dir, socket_override).await {
        Ok(found) => found,
        Err(e) => {
            // A mismatch means a Tunnel exists; its message names both
            // versions and the update command, so the "no tunnel" hint is wrong.
            return match e.mismatch {
                Some(m) => vec![Check::new(Status::Fail, "version", m.to_string())],
                None => vec![Check::new(
                    Status::Fail,
                    NAME,
                    "no tunnel; connect with `rowser ssh <host>` or set ROWSER_SOCKET",
                )],
            };
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
    match (&env.config_home, &env.data_home) {
        (Some(config), Some(data)) => {
            checks.push(check_environment_d(config));
            checks.push(check_desktop(data));
            checks.push(check_default_handler(env.path.as_deref(), config));
        }
        _ => checks.push(Check::new(
            Status::Warn,
            "default browser",
            "HOME is not set; cannot check the default browser",
        )),
    }
    checks.push(check_fallback(
        env.graphical,
        env.path.as_deref(),
        env.self_exe.as_deref(),
    ));
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
        std::fs::set_permissions(
            bin.join("rowser"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
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

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn environment_d_setting_browser_and_path_is_ok() {
        let h = home("e", false);
        let f = h.0.join(".config/environment.d/rowser.conf");
        write(&f, "# x\nBROWSER=rowser\nPATH=$HOME/.local/bin:$PATH\n");
        assert_eq!(check_environment_d(&h.0.join(".config")).status, Status::Ok);
    }

    #[test]
    fn environment_d_missing_or_incomplete_warns_with_the_fix() {
        let h = home("E", false);
        let c = check_environment_d(&h.0.join(".config"));
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.ends_with(FIX), "{}", c.detail);
        let f = h.0.join(".config/environment.d/rowser.conf");
        write(&f, "PATH=$HOME/.local/bin:$PATH\n");
        let c = check_environment_d(&h.0.join(".config"));
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("does not set BROWSER"), "{}", c.detail);
        write(&f, "BROWSER=rowser\n");
        let c = check_environment_d(&h.0.join(".config"));
        assert!(c.detail.contains("does not set PATH"), "{}", c.detail);
    }

    #[test]
    fn desktop_file_present_is_ok_and_missing_warns() {
        let h = home("d", false);
        let data = h.0.join(".local/share");
        let c = check_desktop(&data);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.ends_with(FIX), "{}", c.detail);
        write(
            &data.join("applications/rowser.desktop"),
            "[Desktop Entry]\n",
        );
        assert_eq!(check_desktop(&data).status, Status::Ok);
    }

    /// A `bin` dir in `h` with a fake `xdg-mime` that prints `answer`.
    fn fake_xdg_mime(h: &Tmp, answer: &str) -> OsString {
        let bin = h.0.join("fakebin");
        let f = bin.join("xdg-mime");
        write(&f, &format!("#!/bin/sh\necho '{answer}'\n"));
        std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        bin.into_os_string()
    }

    #[test]
    fn default_handler_from_xdg_mime() {
        let h = home("x", false);
        let cfg = h.0.join(".config");
        let p = fake_xdg_mime(&h, "rowser.desktop");
        let c = check_default_handler(Some(&p), &cfg);
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
        let h = home("X", false);
        let p = fake_xdg_mime(&h, "firefox.desktop");
        let c = check_default_handler(Some(&p), &cfg);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("firefox.desktop"), "{}", c.detail);
        assert!(c.detail.ends_with(FIX), "{}", c.detail);
    }

    #[test]
    fn default_handler_from_mimeapps_list_without_xdg_mime() {
        let h = home("l", false);
        let cfg = h.0.join(".config");
        let empty = h.0.join("nobin").into_os_string();
        let c = check_default_handler(Some(&empty), &cfg);
        assert_eq!(c.status, Status::Warn);
        let m = cfg.join("mimeapps.list");
        write(
            &m,
            "[Added Associations]\nx-scheme-handler/https=rowser.desktop;\n\
             [Default Applications]\nx-scheme-handler/https=firefox.desktop;\n",
        );
        let c = check_default_handler(Some(&empty), &cfg);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("firefox.desktop"), "{}", c.detail);
        write(
            &m,
            "[Default Applications]\nx-scheme-handler/http=rowser.desktop\n\
             x-scheme-handler/https = rowser.desktop;other.desktop;\n",
        );
        let c = check_default_handler(Some(&empty), &cfg);
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
    }

    #[test]
    fn fallback_needs_a_real_xdg_open_only_with_a_graphical_session() {
        let h = home("g", true);
        let rowser = h.0.join(".local/bin/rowser");
        let p = h.0.join(".local/bin").into_os_string();
        assert_eq!(
            check_fallback(false, Some(&p), Some(&rowser)).status,
            Status::Ok
        );
        let c = check_fallback(true, Some(&p), Some(&rowser));
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("no real xdg-open"), "{}", c.detail);
        let real = h.0.join("usr/bin/xdg-open");
        write(&real, "");
        std::fs::set_permissions(&real, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let p = std::env::join_paths([h.0.join(".local/bin"), h.0.join("usr/bin")]).unwrap();
        let c = check_fallback(true, Some(&p), Some(&rowser));
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
        assert!(c.detail.contains(&*real.to_string_lossy()), "{}", c.detail);
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
    fn browser_shadowed_by_an_earlier_path_entry_warns() {
        let h = home("f", true);
        let other = h.0.join("usr/bin");
        std::fs::create_dir_all(&other).unwrap();
        let fake = other.join("xdg-open");
        std::fs::write(&fake, "").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let p = std::env::join_paths([other, h.0.join(".local/bin")]).unwrap();
        let c = check_browser(Some("xdg-open".as_ref()), Some(&p));
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
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
