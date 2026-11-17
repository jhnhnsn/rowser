# rowser module design

Vocabulary: see `GLOSSARY.md` for domain terms. "Module", "interface", "seam" and "adapter" are
used in the codebase-design sense (a module is deep when a lot of behaviour sits behind a small
interface).

## Crate layout

One package, `rowser`, with a library (`src/lib.rs`) and a thin binary (`src/main.rs`). The
library exists so integration tests in `tests/` drive the same modules the binary does.

```
src/
  main.rs            argv[0] + clap dispatch only
  cli.rs             clap definitions; maps args to module calls
  protocol/          wire messages, NDJSON codec, handshake
  detect/            Callback port detection (URL + /proc)
  discovery.rs       Shim's search for a working Session socket
  daemon/            Daemon: policy, Open requests, Callback forwards
    opener.rs        Opener port + CommandOpener adapter
    notifier.rs      Notifier port + DesktopNotifier adapter
    forward.rs       internal: local listeners, reverse-dial bookkeeping
    limit.rs         internal: per-connection rate limit
  shim/              `rowser open`, `rowser forward`, `__keeper`
  wrapper/           `rowser ssh`: destination resolution, relay with Host stamp
  service/           `rowser daemon install|uninstall` (systemd unit, launchd plist)
  remote_install/    `rowser remote install <host>`
  doctor.rs          `rowser doctor`
  platform.rs        default paths per OS (runtime dir, service file locations)
tests/               level-2 integration tests and the level-3 sshd e2e test
```

## Modules and their interfaces

### `protocol` — deep, in-process

Interface:

- `Conn::connect(stream, Hello) -> Result<Conn>` and `Conn::accept(stream) -> Result<(Conn, Hello)>`
  perform the version handshake. A major-version mismatch is a typed error that carries both
  versions; callers only format it.
- `Conn::send(&Msg)` / `Conn::recv() -> Result<Option<Msg>>`.
- `Conn::into_raw()` hands back the stream for byte splicing after a `DataConn` message.

Messages (one enum, `Msg`): `HostStamp{alias}`, `Open{url, reported_host, callback_ports}`,
`OpenResult{forward_id?, warnings, error?}`, `AttachForward{forward_id}`, `Dial{conn_id, port}`,
`DataConn{conn_id}`, `Ping`, `Pong{version}`.

Hidden: framing, max line length, serde details, version constants, handshake ordering.
Tests: `tokio::io::duplex` pairs; no adapters needed.

### `detect` — deep, in-process + local-substitutable

Interface: `callback_ports(url: &Url, procfs: &Procfs, caller_pid: u32) -> BTreeSet<u16>`.

`Procfs` is a root path (`/proc` in production, a fixture directory in tests), not a trait: the
real filesystem is the stand-in. Hidden: query-parameter names, loopback host matching, port range
filter, `/proc/<pid>/stat` ancestor walk, `/proc/net/tcp{,6}` LISTEN parsing, socket-inode to
pid matching. On non-Linux the `/proc` part yields nothing.

### `discovery` — deep, local-substitutable

Interface: `find_tunnel(dir: &Path, env_override: Option<PathBuf>) -> Result<Conn, NoTunnel>`.

Hidden: glob, UID ownership filter, newest-first ordering, handshake probe, Stale socket removal.
Tests use a temp directory and real Unix sockets.

### `daemon` — the deepest module

Interface:

```rust
Daemon::new(DaemonConfig, opener: impl Opener, notifier: impl Notifier) -> Daemon
Daemon::serve(self, direct: UnixListener, relay: UnixListener) -> impl Future<Output = Result<()>>
```

`DaemonConfig`: `open_command`, `confirm_hosts`, rate-limit values, forward timeout, loopback
bind addresses.

Everything else is hidden behind `serve`: Host stamp rules per socket, scheme allowlist, rate
limit, confirmation, `Ping`, Callback forward lifecycle (bind local port, busy-port warning,
queue connections until the keeper attaches, `Dial`, splice, 2-second liveness, 10-minute
timeout).

Ports (each has a production adapter and a test adapter, so both seams are real):

| Port | Production adapter | Test adapter |
| --- | --- | --- |
| `Opener::open(&Url) -> Result<()>` | `CommandOpener` (`--open-command` template; `xdg-open` / `open`) | records URLs |
| `Notifier::notify(Notice)` / `Notifier::confirm(Notice) -> bool` | `DesktopNotifier` (notify-rust; macOS dialog fallback for confirm) | records notices, scripted answers |

Time is **not** a port: tests use tokio's paused clock (`start_paused`). Paths and ports are
plain config values.

### `shim` — deep

Interface: `run_open(url, ShimEnv) -> ExitCode`, `run_forward(port, ShimEnv) -> ExitCode`,
`run_keeper(KeeperArgs) -> ExitCode`.

`ShimEnv`: tunnel directory, `ROWSER_SOCKET`, procfs root, caller pid, reported host name, path
to the own executable (for the keeper re-exec), stderr writer. Hidden: discovery, detection,
message flow, keeper spawn (`setsid`, re-exec), reverse-dial handling, remote-port liveness.

### `wrapper` — deep

Interface: `run_ssh(args: Vec<OsString>, WrapperEnv) -> ExitCode`.

`WrapperEnv`: ssh program path, relay socket path, per-session socket directory. Hidden:
`ssh -G` resolution with argv fallback, random remote path, per-session listener, Host stamp,
byte relay, daemon-not-running warning, cleanup, exit-code passthrough. Tests use a fake `ssh`
script for level 2 and the real `ssh` in level 3.

### `service`, `remote_install`, `doctor` — shallow on purpose

These are thin orchestration over the OS. Keep the pure parts (rendering the unit file and
plist, choosing the release asset from `uname -sm`, PATH-order checks) as functions with unit
tests; do not add ports around `systemctl`, `launchctl` or `ssh`. `remote_install` gets one
real seam: `BinarySource` (download release asset vs. use a local build), which has two
production adapters.

## Dependency direction

```
cli ─► daemon, shim, wrapper, service, remote_install, doctor
daemon ─► protocol
shim ─► protocol, discovery, detect
wrapper ─► protocol
doctor ─► discovery, protocol
discovery ─► protocol
```

`protocol` and `detect` depend on nothing inside the crate.

## Test levels mapped to modules

1. Unit: `protocol`, `detect`, `discovery`, `daemon::limit`, `wrapper` destination parsing,
   `service` rendering, `remote_install` asset choice, `doctor` checks.
2. Integration (`tests/`, Linux + macOS): shim ↔ daemon over real sockets, wrapper ↔ daemon with a
   fake `ssh`, Callback forward with a fake remote HTTP listener, all with the test adapters.
3. E2E (`tests/e2e_sshd.rs`, Linux CI only, `#[ignore]` locally unless `ROWSER_E2E=1`): throwaway
   `sshd`, `rowser ssh`, OAuth-style callback, tmux-style reattach (second session, shim picks
   newest socket), and a `ProxyJump` hop through the same `sshd`.
