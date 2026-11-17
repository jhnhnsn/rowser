//! Default paths and commands per OS.

use std::path::PathBuf;

/// The Daemon's runtime directory: `$XDG_RUNTIME_DIR/rowser/` on Linux,
/// `~/Library/Application Support/rowser/` on macOS.
pub fn runtime_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or("HOME is not set; pass --runtime-dir")?;
        Ok(PathBuf::from(home).join("Library/Application Support/rowser"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
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
