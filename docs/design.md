# rowser module design

Vocabulary: see `GLOSSARY.md` for domain terms. "Module", "interface", "seam" and "adapter" are
used in the codebase-design sense (a module is deep when a lot of behaviour sits behind a small
interface).

## Crate layout

One package, `rowser`, with a library (`src/lib.rs`) and a thin binary (`src/main.rs`). The
library exists so integration tests in `tests/` drive the same modules the binary does.

```
src/
  main.rs            calls cli and runs the result; no logic
  cli.rs             clap definitions + argv[0] and first-argument URL dispatch; maps args to module calls
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
  service.rs         `rowser daemon install|uninstall` (systemd unit, launchd plist, their paths)
  remote_install.rs  `rowser remote install <host>`
  doctor.rs          `rowser doctor`
  logging.rs         tracing subscriber on stderr, level from ROWSER_LOG (default warn)
  platform.rs        default paths per OS (runtime dir or ROWSER_RUNTIME_DIR, tunnel dir,
                     browser names)
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
`Forward{reported_host, callback_ports}` (`rowser forward`; the reply is an `OpenResult`),
`OpenResult{forward_id?, warnings, error?}`, `AttachForward{forward_id}`, `Dial{conn_id, port}`,
`DataConn{forward_id, conn_id}`, `Ping`, `Pong{version}`. A `conn_id` is random, and the Daemon
accepts a `DataConn` only when its `forward_id` is the forward that sent that `Dial`.

Control messages (protocol 1.1, control socket only; `Msg::is_control` names them, and the direct
and relay sockets answer them with an `OpenResult` error and close the connection): `Status`,
`StatusReply{daemon_version, protocol_version, config_file?, config_file_exists, paused,
forwards: [ForwardInfo{port, host, unverified, age_secs}]}`. The control socket also answers
`Ping`. `CONTROL_SOCKET` (the socket's file name) lives in `protocol`, so the client side does
not depend on `daemon`.

The version is 1.1: 1.1 adds the control messages. Peers with the same major interoperate, so a
1.0 Shim or Wrapper still works with a 1.1 Daemon. A Daemon built
before `Forward` cannot parse it and closes the connection, so `rowser forward` prints "daemon closed the
connection" and exits 1.

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
Daemon::serve(self, direct: UnixListener, relay: UnixListener, control: UnixListener)
    -> impl Future<Output = Result<()>>
bind_sockets(dir: &Path) -> Result<(UnixListener, UnixListener, UnixListener)>
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
message flow, keeper spawn (`setsid`, re-exec), reverse-dial handling, remote-port liveness
(LISTEN entries in `/proc/net/tcp{,6}` on Linux, a bind probe elsewhere; ADR 0005).

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
production adapters. `install` returns `Installed`, which says whether the remote script
wrote `~/.config/environment.d/rowser.conf` (`ENVIRONMENT_D_CONTENT`); a kept user file or a
failed write is a warning, not an error.

## Dependency direction

```
cli ─► daemon, shim, wrapper, service, remote_install, doctor
daemon ─► protocol
shim ─► protocol, discovery, detect
wrapper ─► protocol
doctor ─► discovery, protocol, platform
remote_install ─► platform
discovery ─► protocol
```

`protocol` and `detect` depend on nothing inside the crate.

## Test levels mapped to modules

1. Unit: `protocol`, `detect`, `discovery`, `daemon::limit`, `wrapper` destination parsing,
   `service` rendering, `remote_install` asset choice, `doctor` checks.
2. Integration (`tests/`, Linux + macOS): shim ↔ daemon over real sockets, wrapper ↔ daemon with a
   fake `ssh`, Callback forward with a fake remote HTTP listener, all with the test adapters.
3. E2E (`tests/e2e_sshd.rs`, Linux only; each test returns at once unless `ROWSER_E2E=1`): throwaway
   `sshd`, `rowser ssh`, OAuth-style callback, tmux-style reattach (second session, shim picks
   newest socket), and a `ProxyJump` hop through the same `sshd`.

## v1.0 additions (#38)

New modules, in the same style (deep modules, few ports):

```
src/
  config/            config file: path rules, load + validate (toml), edit in place (toml_edit)
  routes/            Route matching: (Host alias or unverified, URL) -> (Route index, Target); pure
  browsers/          browser + Profile discovery per OS; resolves a Target to an argv
  daemon/control.rs  internal: control socket handlers (status, activity, pause, pending, reload)
  daemon/activity.rs internal: in-memory ring of the last 200 requests
```

- `routes` is pure and has no ports: `Routes::from_config(&Config)`, `Routes::pick(&RequestHost,
  &Url) -> Pick { route: Option<usize>, target: Target, ask: bool }`. `rowser route test`, the
  Daemon and `rowser doctor` all call it. `Target` (`system`, `ask`, `<browser>[:<profile>]`)
  lives in `config`, which parses it.
- `Routes::resolve(&RequestHost, &Url) -> Resolved { target, ask, warnings }` applies the
  fallback rules on top of `pick`: an `ask` Target waits, then opens in `[default]`; a browser
  Target that is not available falls back to `[default]` with a warning; and a `[default]` of
  `ask` makes that fallback wait too (ADR 0008). The Daemon calls `resolve`, not `pick`.
- `routes::glob_match` is the one glob matcher, for Routes and `--confirm-hosts`.
- `Notice.target: Option<Target>` carries the Target of an Open request (`None` for a `rowser
  forward` request); the notice body names it when it is not `system`.
- `browsers` takes its roots as plain values (`/Applications`, `~/Applications`, XDG data dirs,
  the browsers' profile directories) so tests point it at temp dirs. `Browsers::scan(roots)`
  and `Browsers::command(&Target, &Url) -> Result<Command>`. No port: there is one adapter.
- The Opener port changes to `Opener::open(&Url, &Target) -> Result<()>`. `CommandOpener`
  runs `browsers::command` for a browser Target and the `--open-command` template for
  `system`. The test adapter records (URL, Target).
- `daemon::serve` takes the control listener as a third socket (`bind_sockets` gives all three).
  Control messages are refused on the direct and relay sockets.
- `control` (client side): `control::request(runtime_dir, &Msg) -> Result<Msg, RequestError>`
  is the one helper for every control command; `RequestError::NotRunning` prints
  `rowser: the daemon is not running`. `control::run_status(StatusArgs, out, err)` is
  `rowser status`; the runtime directory comes from `platform::runtime_dir`, as for `rowser ssh`.
- The Wrapper registers its Session with the Daemon (protocol 1.1) and keeps that connection
  open until `ssh` exits, so `rowser status` lists Sessions.
- Dependency direction: `cli ─► config, routes, browsers, control`; `control ─► protocol,
  platform`; `daemon ─► config, routes, browsers, protocol`; `routes ─► config` types only;
  `doctor ─► config, routes, browsers`.
- `DaemonConfig.config_file` is the config file path the Daemon resolved (`config::path`);
  `rowser status` shows it, with `(no file)` when no file was there at start.
