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

/// Hidden test hook: the rowser version the Outpost gives in its `Pong`,
/// so a test can play an older Outpost.
pub const TEST_VERSION_VAR: &str = "ROWSER_TEST_OUTPOST_VERSION";

pub(crate) fn euid() -> u32 {
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
    check_socket_dir(path.parent().unwrap_or(Path::new("/")))?;
    let sock = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if sock.uid() != euid() {
        return Err(format!(
            "{} does not belong to the current user",
            path.display()
        ));
    }
    Ok(())
}

/// The folder part of `check_socket`: `dir` must be a folder of the current
/// user with mode 0700.
pub fn check_socket_dir(dir: &Path) -> Result<(), String> {
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
    // Open requests, and the next newest gets them when it goes. While the
    // Outpost runs it holds an flock on its own lock file: that is how the
    // others know it is live (a pid can be used again).
    let base = own_base();
    let own = dir.join(format!("{base}.sock"));
    let own_lock = dir.join(format!("{base}.lock"));
    let (listener, held, own_guard, lock_guard) = {
        let _dir_lock = lock_dir(dir)?;
        let held = take_lock(&own_lock, true)
            .map_err(|e| format!("{}: {e}", own_lock.display()))?
            .ok_or_else(|| format!("{} is locked", own_lock.display()))?;
        let lock_guard = RemoveOnDrop(own_lock.clone());
        let listener = UnixListener::bind(&own).map_err(|e| format!("{}: {e}", own.display()))?;
        let own_guard = RemoveOnDrop(own.clone());
        std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", own.display()))?;
        point_at(dir, &own)?;
        (listener, held, own_guard, lock_guard)
    };

    serve_on(io, &listener, &path, dir).await;
    // Hand the socket to the next newest Outpost, or remove it. No Shim can
    // connect to this Outpost from now on.
    drop(listener);
    let _dir_lock = lock_dir(dir)?;
    drop(own_guard);
    drop(lock_guard);
    drop(held);
    repair_locked(dir)
}

/// The Outpost's work until `io` closes: relays Shims from `listener`,
/// answers control streams and repairs the Outpost socket.
async fn serve_on<T>(io: T, listener: &UnixListener, path: &Path, dir: &Path)
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
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
}

/// How often an Outpost checks that the Outpost socket leads to the newest
/// live Outpost.
const REPAIR_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Prefix of each Outpost's own files: its socket (`.sock`), its lock
/// (`.lock`) and its temporary link (`.tmp`).
const OWN_PREFIX: &str = "link-";

/// The lock file in the Outpost socket folder that each Outpost holds while
/// it changes the folder.
const DIR_LOCK: &str = "outpost.lock";

/// A boot clock in nanoseconds. It does not step back, it counts sleep,
/// and all processes on one boot share it, so start times keep their order.
fn boot_nanos() -> u128 {
    #[cfg(target_os = "linux")]
    let clock = libc::CLOCK_BOOTTIME;
    // On macOS CLOCK_MONOTONIC counts sleep too.
    #[cfg(not(target_os = "linux"))]
    let clock = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid timespec for clock_gettime to fill.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec as u128 * 1_000_000_000 + ts.tv_nsec as u128
}

/// The base name of this Outpost's own files: the start time first, so
/// names sort from oldest to newest, then the pid and a counter, so two
/// Outposts in one process do not clash.
fn own_base() -> String {
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "{OWN_PREFIX}{:020}-{}-{n}",
        boot_nanos(),
        std::process::id()
    )
}

/// The base name of an own file `name` of some Outpost, if it is one.
fn base_of(name: &str) -> Option<&str> {
    if !name.starts_with(OWN_PREFIX) {
        return None;
    }
    [".sock", ".lock", ".tmp"]
        .iter()
        .find_map(|s| name.strip_suffix(s))
}

/// An exclusive `flock`, released when dropped.
struct Flock(#[allow(dead_code)] std::fs::File);

/// Takes an exclusive `flock` on `path`, which is made when missing. With
/// `wait` false, a lock that another holds gives `None`.
fn take_lock(path: &Path, wait: bool) -> io::Result<Option<Flock>> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    let op = if wait {
        libc::LOCK_EX
    } else {
        libc::LOCK_EX | libc::LOCK_NB
    };
    // SAFETY: the fd is open for as long as `file` lives.
    if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
        return Ok(Some(Flock(file)));
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(None)
    } else {
        Err(e)
    }
}

/// Locks the Outpost socket folder `dir` against the changes of other
/// Outposts.
fn lock_dir(dir: &Path) -> Result<Flock, String> {
    let path = dir.join(DIR_LOCK);
    take_lock(&path, true)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .ok_or_else(|| format!("{} is locked", path.display()))
}

/// True while the Outpost with the base name `base` in `dir` runs: it holds
/// the flock on its lock file. Call with the folder lock held.
fn alive(dir: &Path, base: &str) -> bool {
    let path = dir.join(format!("{base}.lock"));
    if std::fs::symlink_metadata(&path).is_err() {
        return false;
    }
    matches!(take_lock(&path, false), Ok(None))
}

/// Removes a file when dropped.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Points the Outpost socket in `dir` at the socket file `own`, in one
/// atomic rename: the socket never goes missing. Call with the folder lock
/// held.
fn point_at(dir: &Path, own: &Path) -> Result<(), String> {
    let tmp = own.with_extension("tmp");
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
    let _dir_lock = lock_dir(dir)?;
    repair_locked(dir)
}

/// `repair` with the folder lock held.
fn repair_locked(dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut live = std::collections::HashMap::new();
    let mut newest: Option<String> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(base) = base_of(&name) else { continue };
        let is_live = *live
            .entry(base.to_owned())
            .or_insert_with(|| alive(dir, base));
        if !is_live {
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
                rowser_version: std::env::var(TEST_VERSION_VAR)
                    .ok()
                    .filter(|v| !v.is_empty())
                    .or_else(crate::protocol::rowser_version),
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
