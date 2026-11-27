//! The remote `PATH` for a fake `ssh` that runs an install on this machine.
//! Include it with `#[path = "common/toolbox.rs"] mod toolbox;`.

use std::path::{Path, PathBuf};

/// `<dir>/toolbox`: only the tools the remote install script needs, linked
/// from the real `PATH`. It never holds the real `xdg-mime`.
pub fn remote_toolbox(dir: &Path) -> PathBuf {
    let tools = dir.join("toolbox");
    std::fs::create_dir_all(&tools).unwrap();
    let path = std::env::var("PATH").unwrap();
    for tool in [
        "sh", "mkdir", "rm", "cat", "chmod", "mv", "ln", "head", "awk",
    ] {
        let found = std::env::split_paths(&path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no {tool} on PATH"));
        let link = tools.join(tool);
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(found, link).unwrap();
        }
    }
    tools
}
