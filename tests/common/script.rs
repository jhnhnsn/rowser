//! Fake programs for tests. The unit tests (src/test_support.rs) and the
//! integration tests both include this file with `#[path]`.

use std::path::Path;

/// Writes an executable `#!/bin/sh` script with `body` at `path`.
#[allow(dead_code)]
pub fn write_script(path: &Path, body: &str) {
    write_executable(path, &format!("#!/bin/sh\n{body}\n"));
}

/// Writes `contents` to `path` with mode 0755.
///
/// A child `sh` writes the file, so the write fd never exists in the test
/// process. If this process held it, a fork in another test thread could
/// inherit it until that child's exec, and running the file would then
/// fail with ETXTBSY (#55). A rename or an explicit close does not help:
/// the forked child holds its own copy of the fd.
pub fn write_executable(path: &Path, contents: &str) {
    let status = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
            "sh",
            contents,
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "cannot write {}", path.display());
}
