//! Default paths and commands per OS.

use std::ffi::OsString;
use std::path::PathBuf;

/// Names under which rowser acts as `rowser open` (installed as symlinks).
pub const BROWSER_NAMES: [&str; 3] = ["xdg-open", "sensible-browser", "www-browser"];

/// Where the Shim looks for Session sockets in production.
pub const TUNNEL_DIR: &str = "/tmp";

/// The Daemon's runtime directory: `$ROWSER_RUNTIME_DIR` when set and not
/// empty, else `$XDG_RUNTIME_DIR/rowser/` on Linux,
/// `~/Library/Application Support/rowser/` on macOS.
pub fn runtime_dir() -> Result<PathBuf, String> {
    runtime_dir_from(|k| std::env::var_os(k))
}

fn runtime_dir_from(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, String> {
    if let Some(dir) = var("ROWSER_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(target_os = "macos")]
    {
        let home = var("HOME").ok_or("HOME is not set; pass --runtime-dir")?;
        Ok(PathBuf::from(home).join("Library/Application Support/rowser"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = var("XDG_RUNTIME_DIR")
            .filter(|d| !d.is_empty())
            .ok_or("XDG_RUNTIME_DIR is not set; pass --runtime-dir")?;
        Ok(PathBuf::from(dir).join("rowser"))
    }
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
