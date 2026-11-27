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
  discovery.rs       Shim's handshake with the first socket that answers
  daemon/            Daemon: policy, Open requests, Callback forwards
    opener.rs        Opener port + CommandOpener adapter
    notifier.rs      Notifier port + DesktopNotifier adapter
    forward.rs       internal: local listeners, one Link stream per browser connection
    limit.rs         internal: rate limit for each Saved host
    confirm.rs       internal: `--confirm-hosts` matching and the confirmation timeout
    link.rs          internal: one Link for each Saved host (ssh child, backoff, Link state)
    testing.rs       test adapters for the Opener and Notifier ports
  shim/              `rowser open`, `rowser forward`
  mux.rs             yamux over one byte pipe (the Link); the Daemon is the client side
  outpost.rs         `rowser __outpost` on a Saved host: the Outpost socket, its relay, and the
                     remote side of Callback forwards (`Watch`, `Dial`)
  liveness.rs        internal: remote-port liveness and the loopback dial (ADR 0005)
  service.rs         `rowser daemon install|uninstall` (systemd unit, launchd plist, their paths)
  remote_install.rs  `rowser remote install <host>`
  doctor.rs          `rowser doctor`
  logging.rs         tracing subscriber on stderr, level from ROWSER_LOG (default warn)
  platform.rs        default paths per OS (runtime dir or ROWSER_RUNTIME_DIR, browser
                     names)
  test_support.rs    `#[cfg(test)]` helpers for unit tests; includes tests/common/script.rs
tests/               level-2 integration tests and the level-3 sshd e2e test
install.sh           Release installer (rowser-installer.sh); hidden test seam ROWSER_INSTALL_BASE_URL
```

## Modules and their interfaces

### `protocol` — deep, in-process

Interface:

- `Conn::connect(stream, Hello) -> Result<Conn>` and `Conn::accept(stream) -> Result<(Conn, Hello)>`
  perform the version handshake. A major-version mismatch is a typed error that carries both
  versions; callers only format it.
- `Conn::send(&Msg)` / `Conn::recv() -> Result<Option<Msg>>`.
- `Conn::into_raw()` hands back the stream for byte splicing after a `Dial` reply.

Messages (one enum, `Msg`): `Open{url, callback_ports}`, `Forward{callback_ports}` (`rowser forward`; the reply is an `OpenResult`),
`OpenResult{forward_id?, warnings, error?}`, `Watch{ports}`, `Dial{port}`, `Ping`,
`Pong{version, rowser_version?}` (`rowser_version` is the answering side's `CARGO_PKG_VERSION`,
protocol 2.0; the Daemon and the Outpost send it). `Watch` and `Dial` go from the Daemon to the
Outpost on a new Link stream (#79). On a `Watch` stream the Outpost checks the remote ports every 2
seconds and closes the stream when none listens. On a `Dial` stream it dials `127.0.0.1`, then
`[::1]`, replies with an `OpenResult` (an `error` when the dial fails or the port is below 1024),
and then splices.

Control messages (protocol 1.1, control socket only; `Msg::is_control` names them, and a Link
stream answers them with an `OpenResult` error and closes): `Status`,
`StatusReply{daemon_version, protocol_version, config_file?, config_file_exists, paused,
paused_until?, forwards: [ForwardInfo{port, host, age_secs}], links: [LinkInfo{host, state, since,
last_connected?, outpost_version?, outpost_rowser_version?}]}` (2.0: one entry for each Saved
host), `Reconnect{host}` (2.0, `rowser host connect`; the reply is `Reconnected{host}`, or an
`OpenResult` error when the Daemon has no Link to `host`), `Reload` (the Daemon reads the config file again; the reply is `Reloaded{routes}` with the number
of Routes, or an `OpenResult` error with the file, line and reason, and the old Routes stay).
`Activity{follow}` (`rowser activity`; the reply is `ActivityReply{entries: [ActivityEntry]}`,
oldest first, and with `follow` an `ActivityUpdate{entry}` follows for each new entry and for an
entry whose Callback forward closed, with the same `id` and now `open_secs`, until the client
closes the connection).
`Pause{for_secs?}` (the reply is `Paused{until?}`, Unix seconds) and `Resume` (the reply is
`Resumed{was_paused}`).
`Pending` (`rowser pending`; the reply is `PendingReply{requests: [PendingInfo{id, kind, host,
url?, ports, target?, age_secs}]}`, oldest first) and `AnswerPending{id, approve,
open?}` (`rowser approve <id> [--open <target>]` and `rowser deny <id>`; the reply is
`Answered{id}`, or an `OpenResult` error `no pending request <id>` for an unknown or already
answered id, or for an `open` Target that is `ask`, not found, or given for a `rowser forward`
request; the request then still waits). `ActivityEntry.answered_by` is `terminal` or `desktop`.
The Daemon keeps its Routes behind a lock, so a reload applies to the next Open request.
`control::reload(runtime_dir)` is the client call `rowser reload` and the `rowser route`
commands share. The control socket also answers `Ping`. `CONTROL_SOCKET` (the socket's file
name) lives in `protocol`, so the client side does not depend on `daemon`.

The version is 2.0 (#77). Peers with the same major interoperate. Serde ignores unknown fields,
so a Daemon of this version reads an `Open` from an older 2.0 Shim that still sends
`reported_host`; an older 2.0 Daemon refuses an `Open` without it (#82).

Hidden: framing, max line length, serde details, version constants, handshake ordering.
Tests: `tokio::io::duplex` pairs; no adapters needed.

### `detect` — deep, in-process + local-substitutable

Interface: `callback_ports(url: &Url, procfs: &Procfs, caller_pid: u32) -> BTreeSet<u16>`.

`Procfs` is a root path (`/proc` in production, a fixture directory in tests), not a trait: the
real filesystem is the stand-in. Hidden: query-parameter names, loopback host matching, port range
filter, `/proc/<pid>/stat` ancestor walk, `/proc/net/tcp{,6}` LISTEN parsing, socket-inode to
pid matching. On non-Linux the `/proc` part yields nothing.

### `discovery` — deep, local-substitutable

Interface: `find_socket(paths: &[PathBuf]) -> Result<(Conn, PathBuf), NoTunnel>`: the first
path that answers the handshake, in order (`ROWSER_SOCKET`, else the Outpost socket). Hidden:
the handshake probe. Tests use a temp directory and real Unix sockets.

### `daemon` — the deepest module

Interface:

```rust
Daemon::new(DaemonConfig, opener: impl Opener, notifier: impl Notifier) -> Daemon
Daemon::serve(self, control: UnixListener) -> impl Future<Output = Result<()>>
bind_sockets(dir: &Path) -> Result<UnixListener>   // the control socket only
```

Test seam (`#[doc(hidden)]`): `Daemon::serve_with_test_links(control, Vec<(alias,
UnixListener)>)`. Each listener stands in for the Link of the Saved host `alias`: a connection
on it is an Open request from that host, with no yamux session, so a Callback port gets the
"need a Link" warning. `tests/common/mod.rs` has `test_link`.

`DaemonConfig`: `open_command`, `confirm_hosts`, rate-limit values, forward timeout, loopback
bind addresses.

Everything else is hidden behind `serve`: the Links, scheme allowlist, rate
limit, confirmation, `Ping`, Callback forward lifecycle (bind local port, busy-port warning,
a `Watch` stream on the Link, a `Dial` stream for each browser connection, splice, 10-minute
timeout). A forward ends when the `Watch` stream closes (the remote port stopped listening or
the Link dropped), when a dial fails, or at the timeout. A request on a test Link (no yamux
session) gets no forward, only the warning `callback forwards need a Link to a Saved host; port
<p> is not forwarded`. The hidden daemon flag `--forward-timeout <secs>` is a
test hook for the timeout.

Ports (each has a production adapter and a test adapter, so both seams are real):

| Port | Production adapter | Test adapter |
| --- | --- | --- |
| `Opener::open(&Url) -> Result<()>` | `CommandOpener` (`--open-command` template; `xdg-open` / `open`) | records URLs |
| `Notifier::notify(Notice)` / `Notifier::confirm(Notice, &Dismiss) -> bool` | `DesktopNotifier` (notify-rust off macOS; osascript notices and dialog on macOS) | records notices, scripted answers |

Time is **not** a port: tests use tokio's paused clock (`start_paused`). Paths and ports are
plain config values.

### Links: `mux`, `outpost`, `daemon/link` (#77)

Protocol 2.0. The Daemon keeps one Link to each `[[host]]`: it runs `ssh -T -o BatchMode=yes
-o ControlMaster=no -o ControlPath="<runtime dir>/cm-%C" -o ServerAliveInterval=15
-o ServerAliveCountMax=3 <name> <remote_path> __outpost` and runs yamux over the child's stdio.
The `ControlPath` value is in double quotes, so the macOS runtime directory (with a space) stays
one value.

- `mux::start(io, Side) -> (Opener, inbound streams, driver)`: one yamux connection. Each
  stream carries the usual NDJSON handshake, so `Conn` is unchanged.
- `outpost::serve(io, dir)`: listens on its own socket file (mode 0600) and points
  `<dir>/outpost.sock` at it (see Two Links below), in a folder that must be
  mode 0700 and belong to the user (`prepare_dir` refuses others), and relays each Shim
  connection, byte for byte, on a new stream to the Daemon. The handshake runs from end to
  end, so a 1.x Shim gets the mismatch error from the Daemon. `outpost_dir()` is
  `/tmp/rowser-<uid>`, or the hidden test hook `ROWSER_TEST_OUTPOST_DIR`.
- `daemon/link`: `Backoff` (1 s, doubles, 60 s maximum; reset after a `Pong` on a control
  stream), `LinkState` (`Connecting`, `Connected`, `NeedsLogin`, `VersionMismatch`). An ssh login failure before
  the first connection (exit status 255 and `Permission denied (…)` on stderr, not from a
  `rowser __outpost:` line) sets `NeedsLogin`, shows one notice and stops retries.
  Requests from a Link carry the Host alias and share one rate limit for each Saved host.
  `serve` and a reload call `sync_links` to add or remove Links.
- Link status (#78): each Link task shares a `LinkStatus` (`protocol::LinkInfo`: state, since,
  last connection, the Outpost's protocol version and rowser version from its `Pong`) with the
  control handler. `LinkState` is in
  `protocol` and adds `VersionMismatch` (a major mismatch on the first stream, from the
  handshake or from the `Pong`'s `rowser_version`). `StatusReply.links` lists them.
  `Msg::Reconnect { host }` aborts the Link task and starts a new one at once with the same
  status, which also ends `NeedsLogin` and `VersionMismatch`.
- Version mismatch and the automatic update (#81): with `auto_update`, the Link task runs
  `remote_install::install_with` once (an `Ssh` with `BatchMode=yes`, `ControlMaster=no` and
  rowser's control path; the host's `default_browser`; `DaemonConfig::update_local_build` or the
  release download) and connects again. The `Subject::Link(LinkEvent::Updated)` notice shows
  when that connection reaches an Outpost of this major version. When the install fails, or
  is skipped (`auto_update = false`), or the Outpost still mismatches (`remote_path` is not
  where the install writes), the task shows one `LinkEvent::VersionMismatch` notice
  (`after_update` names `rowser host add <host>` in place of `rowser remote install <host>`)
  and ends: no more attempts, as each one starts an Outpost that takes `outpost.sock`. A new
  task ends the wait, with one more update try: `Reconnect` (sent by `rowser host connect`,
  by `rowser remote install` and by a `host add` that installed), a changed `[[host]]` entry
  on reload, or a Daemon restart. `rowser reload` alone keeps the waiting task.
- `Subject::Link(LinkEvent)` (`NeedsLogin`, `VersionMismatch`, `Updated`) is never a request:
  `Subject::is_request` is false, and it is never confirmed or pending.

### Host commands: `host` (#78)

`rowser host add <host> [--no-default-browser]` runs `ssh <host> exec "$SHELL" -lc 'command -v
rowser && rowser --version'` (`FIND_COMMAND`) and reads the path line and the `rowser <version>`
line after it (a login-shell profile can print lines before them). When the path is missing (or
a name, not a path) or the version is not this one, it runs `remote_install::install` (with the
default-browser part unless `--no-default-browser`), and `remote_path` is
`~/.local/bin/rowser`; else the found path. It saves the entry with `config::add_host`. An entry
of the same name is updated in place: `remote_path` (its decor stays) and `default_browser`
(a plain `add` means true, spec); other keys and comments stay, so the same `add` twice gives
the same text. After an install it also sends `Reconnect`, so a Link that waits in version
mismatch connects again. `remove` uses `config::remove_host`; a file header above the
removed entry moves to the next table. Both then reload a running Daemon. `list` reads the file
and the Daemon's `StatusReply.links` (`outpost <rowser version>`), or prints `daemon not
running`, or `no Saved hosts` for an empty list. The reload note is `control::reload_note`,
shared with `route`. `connect` runs `ssh -o
ControlMaster=yes -o ControlPersist=yes -o ControlPath="<runtime dir>/cm-%C" -N -f <host>` in the
terminal (`host::master_args`, the same `daemon::control_path_option` as the Link), then sends
`Reconnect`; an `OpenResult` error (no Link: the Daemon has not loaded the host) exits 1 and
names `rowser reload`.

`DaemonConfig` adds `hosts`, `ssh` (the program; tests give a fake that runs `rowser __outpost`
locally) and `link_dir` (the `ControlPath` folder; empty by default, and a Link refuses an
empty one). The Shim uses `ROWSER_SOCKET`, else the Outpost socket (`ShimEnv::outpost_socket`) (#82).
Before it uses the Outpost socket, the Shim checks `outpost::check_socket`: the folder belongs to
the user with mode 0700 and the socket belongs to the user. On a failed check it prints a
warning and acts as with no Link. It does not check `ROWSER_SOCKET`.

Two Links to one host (#80): each Outpost listens on its own socket file
`link-<start nanos>-<pid>-<n>.sock` in the Outpost socket folder (the start time is from the
boot clock, which does not step back), and `outpost.sock` is a hard link to the newest live one,
swapped in by an atomic rename, so the socket never goes missing. While it runs, each Outpost
holds an `flock` on its own `link-….lock` file; a file whose lock nobody holds is dead (a pid
can be used again). Every 500 ms each Outpost removes the files of dead Outposts and points
`outpost.sock` at the newest live file; on exit it closes its listener first, then hands the
socket on, or removes it when no Outpost is left. All changes to the folder happen under an
`flock` on `outpost.lock`. A killed Outpost hands nothing on, so the next check of another
Outpost repairs the socket.

### `shim` — deep

Interface: `run_open(url, ShimEnv) -> ExitCode`, `run_forward(port, ShimEnv) -> ExitCode`.
The Shim detects Callback ports, sends the request and exits; it starts no other process.

`ShimEnv`: `ROWSER_SOCKET`, the Outpost socket, procfs root, caller pid, `graphical` (`WAYLAND_DISPLAY` or `DISPLAY` set),
`search_path` (`PATH`, for the real `xdg-open`), `self_exe` (skipped as an `xdg-open`),
`in_fallback` (the hidden `ROWSER_SHIM_FALLBACK` guard), stderr writer. With no Link it prints
`rowser: no Link to your Local machine; open this URL manually: <url>` and `NO_LINK_HINT` and
exits 1, or in a graphical session hands the URL to the real `xdg-open`. Hidden: discovery,
detection, message flow, the real `xdg-open` lookup (it runs with `ROWSER_SHIM_FALLBACK=1`, and a
Shim that sees it does not run `xdg-open` again, so `BROWSER=rowser` or a copy of rowser on `PATH`
cannot loop). Remote-port liveness (LISTEN entries in `/proc/net/tcp{,6}` on Linux, a bind
probe elsewhere; ADR 0005) is in `liveness`, and the Outpost runs it.

The Wrapper (`rowser ssh`) is removed (#82): `cli` prints `SSH_REMOVED` and exits 2.

### `service`, `remote_install`, `doctor` — shallow on purpose

These are thin orchestration over the OS. Keep the pure parts (rendering the unit file and
plist, choosing the release asset from `uname -sm`, PATH-order checks) as functions with unit
tests; do not add ports around `systemctl`, `launchctl` or `ssh`. `remote_install` gets one
real seam: `BinarySource` (download release asset vs. use a local build), which has two
production adapters. `install` returns `Installed`, which says whether the remote script
wrote `~/.config/environment.d/rowser.conf` (`environment_d`, `ENVIRONMENT_D_CONTENT`), whether
it wrote `~/.local/share/applications/rowser.desktop` (`desktop`), and how rowser became the
default for `http` and `https` (`default_by`: `DefaultBy::XdgMime`, `DefaultBy::MimeappsList`,
or none). A kept user file, a failed write or a failed `xdg-mime` is a warning, not an error.
`install(host, source, default_browser)` uses the `ssh` on `PATH`; `install_with(&Ssh, …)` takes
an `Ssh { program, options }` (the options go before `-- <host>`), which the Daemon's automatic
update uses. Without `default_browser` (`--no-default-browser`) the remote script installs only
the binary and writes the marker `NO_DEFAULT_BROWSER_MARKER`; a full install removes it.
`source_for(local_build, release_url)` picks the `BinarySource`; `test_local_build()` reads the
hidden `ROWSER_TEST_LOCAL_BUILD` hook for `host add` and the Daemon. After a successful
`rowser remote install`, `install_main` sends `Reconnect` for the host to a running Daemon.

## Dependency direction

```
cli ─► daemon, shim, service, remote_install, doctor, route, host
route ─► config, routes, control
daemon ─► protocol, remote_install
host ─► config, control, protocol, remote_install, daemon (control_path_option)
shim ─► protocol, discovery, detect
doctor ─► config, browsers, control, discovery, protocol, platform
remote_install ─► platform, control, protocol
discovery ─► protocol
```

`protocol` and `detect` depend on nothing inside the crate.

## Test levels mapped to modules

1. Unit: `protocol`, `detect`, `discovery`, `daemon::limit`, `service` rendering,
   `remote_install` asset choice, `doctor` checks.
2. Integration (`tests/`, Linux + macOS): shim ↔ daemon over real sockets (test Links), Links
   with a fake `ssh` that runs `rowser __outpost` locally, Callback forward with a fake remote
   HTTP listener, all with the test adapters.
3. E2E (`tests/e2e_sshd.rs`, Linux only; each test returns at once unless `ROWSER_E2E=1`): throwaway
   `sshd` and `rowser remote install`. A Link through `sshd` is #84.

Every fake script at any level is written with `write_script`/`write_executable` from
`tests/common/script.rs`, so the test process never holds the write fd (ETXTBSY, #55).

## v1.0 additions (#38)

New modules, in the same style (deep modules, few ports):

```
src/
  config/            config file: path rules, load + validate (toml), edit in place (toml_edit)
  routes/            Route matching: (Host alias, URL) -> (Route index, Target); pure
  browsers/          browser + Profile discovery per OS; resolves a Target to an argv
  route.rs           `rowser route list|add|remove|move|test`: edits the config file, then reloads
  host.rs            `rowser host add|remove|list|connect` (#78): edits `[[host]]`, then reloads
                     or reconnects; runs the `ssh` on `PATH`
  control.rs         client side of the control socket: status, reload, pause, resume,
                     pending, approve, deny, activity; `reload_note` for `route` and `host`
  daemon/control.rs  internal: control socket handlers (status, activity, pause, pending, reload,
                     reconnect)
  daemon/activity.rs internal: in-memory ring of the last 200 requests
  daemon/pending.rs  internal: confirmations that wait, with ids, for `rowser pending|approve|deny`
```

- `routes` is pure and has no ports: `Routes::from_config(&Config)`, `Routes::pick(alias: &str,
  &Url) -> Pick { route: Option<usize>, target: Target, ask: bool }`. `rowser route test` and the
  Daemon call it; `rowser doctor` checks Targets with `Browsers::normalize` only. `Target`
  (`system`, `ask`, `<browser>[:<profile>]`) lives in `config`, which parses it.
- `Routes::resolve(alias: &str, &Url, &Browsers) -> Resolved { route, target, ask, warnings }`
  applies the fallback rules on top of `pick`: an `ask` Target waits, then opens in
  `[default]`; a browser Target that `Browsers::normalize` rejects (browser or profile missing,
  or no argv can be built, such as a Linux browser with no usable `Exec`) falls back to
  `[default]` with a warning; and a `[default]` of `ask` makes that fallback wait too (ADR
  0008). A found browser Target comes back with its profile as a folder name. The Daemon calls
  `resolve`, not `pick`.
- `routes::glob_match` is the one glob matcher, for Routes and `--confirm-hosts`.
- `Notice.target: Option<Target>` carries the Target of an Open request (`None` for a `rowser
  forward` request); the notice body names it when it is not `system`.
- `browsers` takes its roots as plain values (`/Applications`, `~/Applications`, XDG data dirs,
  the browsers' profile directories) so tests point it at temp dirs. `Browsers::scan(roots)`
  (records `roots.os` in `Browsers.os`); `Browsers::find(&Target)` looks a profile up by folder
  name, then display name; `Browsers::normalize(&Target) -> Result<Target, String>` gives the
  Target with the profile folder name (Firefox: the `Path` folder from `profiles.ini`), and
  after a scan also checks that an argv can be built; `Browsers::command(Os, &Target, &str) ->
  Result<Vec<String>, String>` is a pure argv builder over `browsers::argv(Os, &Browser,
  Option<&Profile>, url)`. It runs nothing. Firefox maps the folder to `-P <name>`. No port:
  there is one adapter.
- The Opener port changes to `Opener::open(&Url, &Target) -> Result<()>`.
  `CommandOpener::new(template)?.with_browsers(roots)` scans under `roots` and runs
  `Browsers::command` for a browser Target (without `with_browsers` it refuses one), and the
  `--open-command` template for `system`. The test adapter records (URL, Target).
- `DaemonConfig.browser_roots: Option<Roots>` is where the Daemon scans for browsers on each
  Open request; `None` means no browsers, so every browser Target falls back to `[default]`.
- `daemon::serve` takes the control listener (`bind_sockets` gives it). Control messages are
  refused on a Link.
- `control` (client side): `control::request(runtime_dir, &Msg) -> Result<Msg, RequestError>`
  is the one helper for every control command; `RequestError::NotRunning` prints
  `rowser: the daemon is not running`. `control::run_status(StatusArgs, out, err)` is
  `rowser status`; the runtime directory comes from `platform::runtime_dir`, as for `rowser daemon`.
  `open_control(runtime_dir)` connects and does the handshake for `request` and for
  `rowser activity --follow`, which keeps its connection open.
- `rowser activity`: `daemon/activity.rs` keeps the ring (200 entries) and a broadcast channel
  for `follow`; `Forwards::create` takes an `on_end` callback that reports how long the forward
  stayed open. `Resolved.route` carries the Route index, shown as `route <n>` (from 1)
  or `[default]`. `control::run_activity(ActivityArgs, out, err)` is the client;
  its text lines pass through `protocol::printable`, which the Notifier also uses, so a forged
  host name or URL cannot move text with control or bidi characters.
- `rowser pause` / `rowser resume`: `Inner.pause` holds the pause in memory (a restart ends
  it), with its end on the tokio clock (so tests use `start_paused`) and in Unix seconds for
  `StatusReply.paused_until`. A pause whose end passed is cleared when next read. `handle`
  checks it before the rate limit, and again after a confirmation the user approved, and
  answers with an `OpenResult` error (`paused; open this URL manually: <url>`, or for
  `rowser forward` `paused; port <p> was not forwarded`), which the Shim prints after
  `rowser: ` and exits 1, so the Shim needs no change. `control::parse_duration` reads
  `--for`; a duration the clocks cannot hold gets the error `duration too long`.
- `rowser pending` / `approve` / `deny`: `daemon/pending.rs` lists each confirmation while it
  waits, with an id that counts up from 1. `Inner::confirm` races the Notifier against the
  terminal answer; the first answer takes the request out of the list, so a later one gets
  `no pending request <id>`. A terminal answer calls `Dismiss::dismiss`, and the Notifier closes
  what it registered with `Dismiss::on_dismiss`: on Linux a replacement notification with the
  same id, closed at once (which also ends `wait_for_action`); on macOS a SIGTERM to the
  `osascript` dialog. A desktop answer after a dismiss is ignored. `approve --open` replaces
  the request's Target after `Browsers::normalize`; `--open ask` and a missing browser are
  refused. The test `ScriptedNotifier` counts dismissals (`dismissed()`), and a dismissed hung
  confirmation returns. `control::{run_pending, run_approve, run_deny}` are the clients.
- `[unverified]` (#82): `config::parse` ignores the entry and sets `Config::has_unverified`. The
  Daemon logs `config::UNVERIFIED_IGNORED` once at start and on each reload that reads it, and
  `rowser doctor` shows it as one `config` warning.
- Dependency direction: `cli ─► config, routes, browsers, control, route`;
  `route ─► config, routes, browsers, control`; `control ─► protocol,
  platform`; `daemon ─► config, routes, browsers, protocol`; `routes ─► config, browsers`
  (types and `Browsers::normalize`); `browsers ─► config` (`Target`). For `doctor`, see
  Dependency direction.
- `rowser doctor` on the Local machine: `DoctorEnv` also carries `config_file`, `runtime_dir`,
  `browser_roots` and `ssh` (`SSH_CONNECTION` set). `run_doctor` runs the Local checks
  (`check_config`, `check_routes` over every browser Target with `Browsers::normalize`,
  `check_daemon` with `control::request_async(&Msg::Status)`) when a config file or the
  runtime directory exists, and the Remote checks when the Shim's socket (`ROWSER_SOCKET`, else
  the Outpost socket) exists or `ssh` is set, or when neither side is found. A Daemon that does not answer is a warning.
- `rowser route`: `route::run_route(RouteArgs { config_file, runtime_dir, action, browsers },
  out, err)`. Warnings go to `err`.
  `config::{add_route, remove_route, move_route}` are text-to-text edits with `toml_edit`
  (comments stay; a comment above a Route moves with it); each checks the text before and the
  result after with `config::parse`. `config::write` writes through a temp file and a rename.
  After a change it calls `control::reload`; `route test` calls `Routes::pick`, and
  `Routes::resolve` for a browser Target. `route add` writes a found profile as its folder
  name with the display name as a trailing comment.
- `DaemonConfig.config_file` is the config file path the Daemon resolved (`config::path`);
  `rowser status` shows it, with `(no file)` when no file was there at start or at the last
  valid reload. The Daemon keeps that flag with the Routes, behind one lock.
