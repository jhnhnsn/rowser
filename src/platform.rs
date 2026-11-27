//! Default paths and commands per OS.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Names under which rowser acts as `rowser open` (installed as symlinks).
pub const BROWSER_NAMES: [&str; 3] = ["xdg-open", "sensible-browser", "www-browser"];

/// The Daemon's runtime directory: `$ROWSER_RUNTIME_DIR` when set and not
/// empty, else `$XDG_RUNTIME_DIR/rowser/` on Linux,
/// `~/Library/Application Support/rowser/` on macOS.
pub fn runtime_dir() -> Result<PathBuf, String> {
    runtime_dir_from(|k| std::env::var_os(k))
}

/// `$ROWSER_RUNTIME_DIR` when set and not empty. A relative value is
/// refused: each process would resolve it against its own current
/// directory, so the Daemon and `rowser status` could use different ones.
pub fn rowser_runtime_dir_from(
    var: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<PathBuf>, String> {
    let Some(dir) = var("ROWSER_RUNTIME_DIR").filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    let dir = PathBuf::from(dir);
    if dir.is_relative() {
        return Err(format!(
            "ROWSER_RUNTIME_DIR must be an absolute path, not {}",
            dir.display()
        ));
    }
    Ok(Some(dir))
}

pub(crate) fn runtime_dir_from(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, String> {
    if let Some(dir) = rowser_runtime_dir_from(&var)? {
        return Ok(dir);
    }
    #[cfg(target_os = "macos")]
    {
        let home = var("HOME").ok_or(
            "HOME is not set; set ROWSER_RUNTIME_DIR (or pass --runtime-dir to rowser daemon)",
        )?;
        Ok(PathBuf::from(home).join("Library/Application Support/rowser"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = var("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).ok_or(
            "XDG_RUNTIME_DIR is not set; set ROWSER_RUNTIME_DIR \
             (or pass --runtime-dir to rowser daemon)",
        )?;
        Ok(PathBuf::from(dir).join("rowser"))
    }
}

/// The first line of the `environment.d` file `rowser remote install`
/// writes. A file without it is the user's own: rowser keeps it.
pub const ENVIRONMENT_D_MARK: &str =
    "# Written by rowser remote install; rowser overwrites this file.";

/// True when `path` is a file with an execute bit, as a shell `PATH` search needs.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The first `<dir>/<name>` on `path` (absolute dirs only) that is
/// executable and that `accept` takes.
pub fn find_on_path(path: &OsStr, name: &str, accept: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|f| is_executable(f) && accept(f))
}

/// The first executable `xdg-open` on `search_path` whose canonical path is
/// not the canonical path of `self_exe`: the real one, for the Shim's local
/// fallback.
pub fn real_xdg_open(search_path: &OsStr, self_exe: Option<&Path>) -> Option<PathBuf> {
    let own = self_exe.and_then(|p| std::fs::canonicalize(p).ok());
    find_on_path(search_path, "xdg-open", |f| {
        std::fs::canonicalize(f).is_ok_and(|real| Some(&real) != own.as_ref())
    })
}

/// The default `--open-command`.
pub fn default_open_command() -> &'static str {
    if cfg!(target_os = "macos") {
        "open {url}"
    } else {
        "xdg-open {url}"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.into())
    }

    #[test]
    fn rowser_runtime_dir_wins_over_the_default() {
        let vars = [
            ("ROWSER_RUNTIME_DIR", "/tmp/rt"),
            ("XDG_RUNTIME_DIR", "/run/u"),
            ("HOME", "/h"),
        ];
        assert_eq!(runtime_dir_from(env(&vars)), Ok(PathBuf::from("/tmp/rt")));
    }

    #[test]
    fn a_relative_rowser_runtime_dir_is_refused() {
        let vars = [("ROWSER_RUNTIME_DIR", "rt"), ("XDG_RUNTIME_DIR", "/run/u")];
        let err = runtime_dir_from(env(&vars)).unwrap_err();
        assert!(err.contains("ROWSER_RUNTIME_DIR"), "{err}");
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn a_missing_default_names_the_variable_not_an_ssh_flag() {
        let err = runtime_dir_from(env(&[])).unwrap_err();
        assert!(err.contains("set ROWSER_RUNTIME_DIR"), "{err}");
    }

    #[test]
    fn an_empty_rowser_runtime_dir_gives_the_default() {
        let vars = [
            ("ROWSER_RUNTIME_DIR", ""),
            ("XDG_RUNTIME_DIR", "/run/u"),
            ("HOME", "/h"),
        ];
        let want = if cfg!(target_os = "macos") {
            PathBuf::from("/h/Library/Application Support/rowser")
        } else {
            PathBuf::from("/run/u/rowser")
        };
        assert_eq!(runtime_dir_from(env(&vars)), Ok(want));
    }
}
