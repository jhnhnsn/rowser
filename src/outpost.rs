//! The Outpost (`rowser __outpost`): the far end of a Link on a Saved host.
//!
//! It speaks the protocol with the Daemon over its stdin and stdout
//! (`yamux`, ADR 0010), listens on the Outpost socket, and relays each Shim
//! connection to the Daemon as a new stream. It also holds the remote side
//! of each Callback forward: it watches the Callback ports and dials them
//! for each browser connection. It exits when stdio closes, which ends its
//! forwards.
//!
//! Several Outposts of one user (one per Local machine with a Link here)
//! share the Outpost socket: it is a hard link to the own socket file of
//! the newest live Outpost (`repair`).

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;

use crate::liveness::{connect_loopback, in_use, Liveness};
use crate::mux::{self, Side};
use crate::protocol::{Conn, Hello, Msg};

/// File name of the Outpost socket in its folder.
pub const OUTPOST_SOCKET: &str = "outpost.sock";

/// Hidden test hook: replaces the Outpost socket folder
/// `/tmp/rowser-<uid>`, so a test never touches the real one.
pub const TEST_DIR_VAR: &str = "ROWSER_TEST_OUTPOST_DIR";

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The Outpost socket folder: `/tmp/rowser-<uid>`, from the UID alone
/// (only the hidden test hook replaces it).
pub fn outpost_dir() -> PathBuf {
    match std::env::var_os(TEST_DIR_VAR).filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(format!("/tmp/rowser-{}", euid())),
    }
}

/// The Outpost socket path in `dir`.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join(OUTPOST_SOCKET)
}

/// Creates `dir` with mode 0700 when it is missing. An existing `dir` must
/// belong to the current user and have mode 0700, else it is refused.
pub fn prepare_dir(dir: &Path) -> Result<(), String> {
    if let Err(e) = std::fs::DirBuilder::new().mode(0o700).create(dir) {
        if e.kind() != io::ErrorKind::AlreadyExists {
            return Err(format!("cannot create {}: {e}", dir.display()));
        }
    }
    // symlink_metadata: a symlink to another folder is refused too.
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    if meta.uid() != euid() {
        return Err(format!(
            "{} does not belong to the current user; remove it and connect again",
            dir.display()
        ));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(format!(
            "{} has mode {mode:03o}; it must be 0700",
            dir.display()
        ));
    }
    Ok(())
}

/// The Shim's check before it connects to the Outpost socket at `path`: its
/// folder must belong to the current user with mode 0700, and the socket
/// must belong to the current user. Another user who made the folder first
/// gets no URLs (ADR 0010).
pub fn check_socket(path: &Path) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    if meta.uid() != euid() {
        return Err(format!(
            "{} does not belong to the current user",
            dir.display()
        ));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(format!(
            "{} has mode {mode:03o}; it must be 0700",
            dir.display()
        ));
    }
    let sock = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if sock.uid() != euid() {
        return Err(format!(
            "{} does not belong to the current user",
            path.display()
        ));
    }
    Ok(())
}

/// Runs the Outpost over `io` with its socket in `dir` until `io` closes.
/// The socket is removed at the end.
pub async fn serve<T>(io: T, dir: &Path) -> Result<(), String>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    prepare_dir(dir)?;
    let path = socket_path(dir);
    // Each Outpost listens on its own socket file and the Outpost socket is
    // a hard link to the newest one: the Link that connected last takes the
    // Open requests, and the next newest gets them when it goes.
    let own = dir.join(own_name(std::process::id()));
    let listener = UnixListener::bind(&own).map_err(|e| format!("{}: {e}", own.display()))?;
    let _own = RemoveOnDrop(own.clone());
    std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", own.display()))?;
    point_at(dir, &own)?;

    let (opener, mut inbound, driver) = mux::start(io, Side::Outpost);
    let accept = async {
        loop {
            match listener.accept().await {
                Ok((mut shim, _)) => {
                    let opener = opener.clone();
                    tokio::spawn(async move {
                        match opener.open().await {
                            Ok(mut stream) => {
                                let _ = tokio::io::copy_bidirectional(&mut shim, &mut stream).await;
                            }
                            Err(e) => tracing::debug!("no stream for a Shim: {e}"),
                        }
                    });
                }
                Err(e) => tracing::warn!("accept on {}: {e}", path.display()),
            }
        }
    };
    let control = async {
        while let Some(stream) = inbound.recv().await {
            tokio::spawn(async move {
                if let Err(e) = answer_control(stream).await {
                    tracing::debug!("control stream ended: {e}");
                }
            });
        }
    };
    // An Outpost that was killed cannot hand the socket back: each one
    // checks often that the socket leads to the newest live Outpost.
    let repairs = async {
        let mut tick = tokio::time::interval(REPAIR_INTERVAL);
        loop {
            tick.tick().await;
            if let Err(e) = repair(dir) {
                tracing::warn!("{e}");
            }
        }
    };
    tokio::select! {
        _ = accept => {}
        _ = control => {}
        _ = driver => {}
        _ = repairs => {}
    }
    // Hand the socket to the next newest Outpost, or remove it.
    drop(_own);
    repair(dir)
}

/// How often an Outpost checks that the Outpost socket leads to the newest
/// live Outpost.
const REPAIR_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Prefix of each Outpost's own socket file (and its temporary link).
const OWN_PREFIX: &str = "link-";

/// The own socket file name of the Outpost `pid`: the start time first, so
/// names sort from oldest to newest.
fn own_name(pid: u32) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{OWN_PREFIX}{nanos:020}-{pid}.sock")
}

/// The pid in an own socket file name (or its `.tmp` link), if it is one.
fn pid_of(name: &str) -> Option<i32> {
    let rest = name.strip_prefix(OWN_PREFIX)?;
    let rest = rest
        .strip_suffix(".sock")
        .or_else(|| rest.strip_suffix(".tmp"))?;
    rest.split_once('-')?.1.parse().ok().filter(|p| *p > 1)
}

/// True while the process `pid` exists.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Removes a file when dropped.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Points the Outpost socket in `dir` at the socket file `own`, in one
/// atomic rename: the socket never goes missing.
fn point_at(dir: &Path, own: &Path) -> Result<(), String> {
    let tmp = dir.join(format!("{OWN_PREFIX}0-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::fs::hard_link(own, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let path = socket_path(dir);
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// Removes the files of Outposts that are gone, and points the Outpost
/// socket at the newest live Outpost, or removes it when none is left.
fn repair(dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut newest: Option<String> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = pid_of(&name) else { continue };
        if !alive(pid) {
            let _ = std::fs::remove_file(entry.path());
        } else if name.ends_with(".sock") && newest.as_ref().is_none_or(|n| name > *n) {
            newest = Some(name);
        }
    }
    let path = socket_path(dir);
    let current = std::fs::symlink_metadata(&path).map(|m| m.ino()).ok();
    match newest {
        Some(name) => {
            let own = dir.join(name);
            let want = std::fs::symlink_metadata(&own).map(|m| m.ino()).ok();
            if want.is_some() && want != current {
                point_at(dir, &own)?;
            }
        }
        None if current.is_some() => {
            let _ = std::fs::remove_file(&path);
        }
        None => {}
    }
    Ok(())
}

/// A failed `OpenResult` carrying `text`.
fn error_reply(text: impl Into<String>) -> Msg {
    Msg::OpenResult {
        forward_id: None,
        warnings: vec![],
        error: Some(text.into()),
    }
}

/// A Daemon-to-Outpost stream: answers `Ping`, watches the ports of a
/// Callback forward (`Watch`), or relays one browser connection (`Dial`).
async fn answer_control(stream: mux::Stream) -> crate::protocol::Result<()> {
    let (mut conn, _) = Conn::accept(stream).await?;
    while let Some(msg) = conn.recv().await? {
        let reply = match msg {
            Msg::Ping => Msg::Pong {
                version: Hello::current().version(),
                rowser_version: crate::protocol::rowser_version(),
            },
            Msg::Watch { ports } => return watch(conn, &ports).await,
            Msg::Dial { port } => return dial(conn, port).await,
            _ => error_reply("unexpected message"),
        };
        conn.send(&reply).await?;
    }
    Ok(())
}

/// Holds the stream open while the forward's Callback ports pass the
/// liveness rule, and closes it when they stop. The Daemon ends the forward
/// when the stream closes. Ends early when the Daemon closes the stream.
async fn watch(mut conn: Conn<mux::Stream>, ports: &[u16]) -> crate::protocol::Result<()> {
    let mut liveness = Liveness::new(ports);
    let mut tick = tokio::time::interval(crate::callback::LIVENESS_INTERVAL);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if !liveness.check(in_use) {
                    return Ok(());
                }
            }
            // The Daemon sends nothing more: any message or EOF ends the watch.
            _ = conn.recv() => return Ok(()),
        }
    }
}

/// Dials the remote loopback `port` and relays the stream to it. A port
/// below 1024 or a failed dial gets an error reply, and the Daemon ends the
/// forward.
async fn dial(mut conn: Conn<mux::Stream>, port: u16) -> crate::protocol::Result<()> {
    if port < 1024 {
        return conn
            .send(&error_reply(format!("port {port} is not a callback port")))
            .await;
    }
    let mut remote = match connect_loopback(port).await {
        Ok(s) => s,
        Err(e) => {
            return conn
                .send(&error_reply(format!("cannot dial port {port}: {e}")))
                .await
        }
    };
    conn.send(&Msg::OpenResult {
        forward_id: None,
        warnings: vec![],
        error: None,
    })
    .await?;
    // A relay that breaks after the dial is not a dial failure.
    let _ = conn.splice(&mut remote).await;
    Ok(())
}

/// `rowser __outpost` for the binary.
pub fn outpost_main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rowser __outpost: {e}");
            return ExitCode::FAILURE;
        }
    };
    let io = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    let result = runtime.block_on(serve(io, &outpost_dir()));
    // A blocking stdin read must not hold the exit.
    runtime.shutdown_timeout(std::time::Duration::from_millis(100));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rowser __outpost: {e}");
            ExitCode::FAILURE
        }
    }
}
