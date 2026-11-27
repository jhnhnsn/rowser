# ticket-loop config

## Tickets

GitHub issues in `jhnhnsn/rowser`. A parent holds its tickets as sub-issues (also in the GitHub
milestone of the same version: #1 in `v0.1.0`, #38 in `v1.0`). Blockers are listed in each ticket body's `## Blocked by` section. Labels:
`ready-for-agent`, `ready-for-human`.

Read a ticket: `gh issue view <n> --repo jhnhnsn/rowser --json title,body,labels,comments`.

## Order

The run's parent: #1 (v0.1.0) or #38 (v1.0). Inside it: the lowest-numbered ready ticket with no
open blockers.

## Checks

Run each, one per call; all must pass:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

## Test queue

rowser is a CLI; test it from the terminal.

- Run: `cargo build`, then `target/debug/rowser …`.
- Drive: Bash. Run the daemon, shim and wrapper against temporary sockets.
- Test hooks: `--runtime-dir <tmp>`, `ROWSER_SOCKET`, `--open-command` writing URLs to a file,
  the test Opener and Notifier adapters, `ROWSER_E2E=1` with a throwaway unprivileged `sshd` on
  localhost (`/usr/bin/sshd` is installed).
- Confirmations: hidden daemon flags `--test-confirm approve|deny|hang` (scripted Notifier, no
  desktop) and `--confirm-timeout <secs>` (default 60).
- Callback forwards on one machine: run the daemon with the hidden `--loopback ::1` so the
  local side binds only `[::1]`, and run the fake remote listener on `127.0.0.1` (the Outpost
  dials `127.0.0.1` first). `--loopback` refuses non-loopback addresses. Forwards go only
  through a Link (#79): use a `[[host]]` with a fake `ssh` that runs `rowser __outpost` locally
  (see the Links item). The Outpost holds the forward; the Shim exits and leaves no process.
  Every 2 s the Outpost checks only the address it dials (LISTEN entries, ADR 0005), so the
  daemon's `[::1]` listener does not keep a forward alive. A forward ends when a port that
  listened once stops; before that, only the timeout ends it. Kill the Outpost (the fake
  `ssh` can write its pid) to test a Link drop. The hidden daemon flag
  `--forward-timeout <secs>` replaces the 10-minute timeout; `rowser daemon install` refuses it.
  A request on the Wrapper's sockets gets no forward, only the "need a Link" warning.
- The Shim's no-tunnel fallback (graphical session) runs the first `xdg-open` on `PATH` that is
  not rowser: put a fake `xdg-open` script that records its argument first on `PATH`, set
  `DISPLAY`, and set the hidden `ROWSER_TEST_TUNNEL_DIR` to an empty temp dir (it replaces
  `/tmp` for socket discovery) and `ROWSER_SOCKET` to a missing path, so no real Tunnel answers.
  Never let it reach the real `xdg-open`.
- Links (#77): the Shim tries the Outpost socket `/tmp/rowser-<uid>/outpost.sock` after
  `ROWSER_SOCKET`. Always set the hidden `ROWSER_TEST_OUTPOST_DIR` to a short temp dir
  (`/tmp/rw.XXXX/o`) for `rowser __outpost` and for any Shim a test runs, so no test uses the
  real Outpost socket. To test a Link, give the Daemon a `[[host]]` and a fake `ssh` (the
  `DaemonConfig::ssh` field in tests) that drops its options and alias and execs
  `<remote_path> __outpost` on this machine, with HOME and `ROWSER_TEST_OUTPOST_DIR` set to temp
  dirs. It must never run the real `ssh`. See `tests/link.rs`. A Link retries after 1 s, 2 s, 4 s
  and so on, so wait for the Outpost socket to answer before you send a URL. Two Local
  machines (#80): run two Daemons with separate runtime dirs whose fake `ssh` set the same
  `ROWSER_TEST_OUTPOST_DIR`; see `tests/two_links.rs`.
- `rowser host add|remove|list|connect` (#78) runs the `ssh` on `PATH`: put a fake `ssh` first on
  `PATH` that logs its arguments, prints a path and then a `rowser <version>` line for the find
  (`command -v rowser && rowser --version`; exit 1 for a missing rowser), exits 0 for `-N -f`
  (the master), and never runs the real `ssh`. A missing rowser or another version runs the
  install, so the fake `ssh` must also answer `uname -sm` and run the `sh -c` install with
  HOME=<tmp> set inside the fake remote command (see the #81 item below). Set `HOME`,
  `XDG_CONFIG_HOME` and `ROWSER_RUNTIME_DIR` to short temp dirs. For a running Daemon, start one
  in that runtime dir as `tests/link.rs` does. See `tests/host.rs`.
- Installs from `host add` and the Daemon's automatic update (#81): the hidden
  `ROWSER_TEST_LOCAL_BUILD=<path>` replaces the release download (for `rowser host add` and
  `rowser daemon`). The fake `ssh` must answer `-- <host> uname -sm` and run the last argument of
  `-- <host> sh -c '…'` with `/bin/sh -c` under a temp HOME and a toolbox `PATH`, as in the
  remote install item. `host add` reads `rowser <version>` on the line after the path. The hidden
  `ROWSER_TEST_OUTPOST_VERSION=<v>` makes `rowser __outpost` give `<v>` in its `Pong`: a fake
  older Outpost is a script `~/.local/bin/rowser` (temp HOME) that sets it and execs the real
  build. See `tests/update.rs`.
- `rowser ssh` uses the `ssh` on `PATH` and the Daemon in `$ROWSER_RUNTIME_DIR` (absolute) or
  else `$XDG_RUNTIME_DIR/rowser/`: put a fake `ssh` script first on `PATH` and set one of them to
  a short temp dir to test it. Unset `ROWSER_RUNTIME_DIR` when a test relies on the default.
- `rowser daemon install|uninstall` writes the unit under `$XDG_CONFIG_HOME/systemd/user/` and
  runs the `systemctl` on `PATH`: set `XDG_CONFIG_HOME` to a temp dir and put a fake `systemctl`
  script first on `PATH` that records its arguments. Never touch the real user units. Check that
  daemon flags given before `install` appear in the unit's `ExecStart`. The macOS (launchd) path
  is `cfg`-gated; on Linux test it through `service::launchd_install` / `launchd_uninstall`, which
  take the `launchctl` program path: `cargo test --lib launchd` runs them with a fake script.
- `rowser remote install <host>` runs the `ssh`, `curl` and `tar` on `PATH`: put a fake `ssh`
  first on `PATH` that answers `uname -sm` and runs the copy command locally with `sh -c` under
  a remote `PATH` of `$FAKE_REMOTE_PATH`, and set `HOME` to a temp dir. The install runs
  `xdg-mime` when the remote `PATH` has it, so set `FAKE_REMOTE_PATH` to a dir of linked tools
  only (`sh mkdir rm cat chmod mv ln head awk`), plus a fake `xdg-mime` that logs its arguments
  when wanted. Never let the real `xdg-mime` run. Also unset `XDG_CONFIG_HOME` and
  `XDG_DATA_HOME` and set `DBUS_SESSION_BUS_ADDRESS` to a dead path (`unix:path=/nonexistent/…`).
  `--local-build <path>` skips the download; the hidden `--release-url <base>` points the
  download at a local HTTP server serving `rowser-<target>.tar.gz` and `SHA256SUMS`
  (`sha256sum` output; the download is refused without a matching entry). The sshd e2e test
  puts `HOME=<tmp> PATH=<toolbox>` in front of the remote command, never through `SetEnv`.
- `install.sh` (the Release's `rowser-installer.sh`): run `sh install.sh` with `HOME`,
  `ROWSER_INSTALL_DIR` and `XDG_CONFIG_HOME` set to temp dirs, and the hidden
  `ROWSER_INSTALL_BASE_URL=<base>` pointing at a local HTTP server serving
  `rowser-<target>.tar.gz` and `SHA256SUMS`. Put a fake `uname` first on `PATH` to pick the
  system. Never run it against the real GitHub Release.
- `rowser browsers [--json]` reads `HOME`, `XDG_DATA_HOME`, `XDG_DATA_DIRS` and
  `XDG_CONFIG_HOME` on Linux: set all four to a temp tree with fake `.desktop` files,
  `Local State` and `~/.mozilla/firefox/profiles.ini` files. It only reads; it opens nothing.
- Unix socket paths are limited to about 104–108 bytes: put test sockets in a short temp dir
  (e.g. `/tmp/rw.XXXX`), never under a long scratchpad or worktree path; remove it afterwards.
- Never open the real browser or show real desktop notifications in a test.
- `rowser daemon` reads `$XDG_CONFIG_HOME/rowser/config.toml`: always set `XDG_CONFIG_HOME` to a
  temp dir (with or without a `rowser/config.toml`) so the real config is never read. Without a
  file, `[unverified]` is `ask`, so direct-socket opens wait for `--test-confirm`.
- Run the live daemon with `env DBUS_SESSION_BUS_ADDRESS=unix:path=<tmp>/nobus` so no real desktop
  notification shows (a failed notification only logs a warning).
- Manual macOS checks (notifications, confirm dialogs) go on the ticket as a `ready-for-human` note;
  the loop runs on Linux.
- A test plan measures: exact URLs received, exit codes, stderr text, callback bytes round-tripped.

## Reference

`docs/spec.md`, `docs/adr/`, `GLOSSARY.md`, `docs/design.md`. The spec wins over a ticket. Always
the user's decision: any change to the spec or an ADR; any change to the security model (sockets,
trust, scheme allowlist, rate limit); any change to user-facing command or flag names.

## Ship

Each ticket: `git push origin main`, no publish step. A milestone ships at once: after its last
ticket the user tags it (`v0.1.0`, then `v1.0.0`) and the release workflow (#12) builds binaries. Reserving the
crates.io name needs the user's token: `ready-for-human`.

## Changelog

`CHANGELOG.md`, Keep a Changelog format. Each shipped ticket adds an entry under
`## [Unreleased]`, newest first, for rowser users: what a user can now do, with the issue link.
The milestone summary goes in `## [<version>]` at tagging. Milestones still to come are not listed.

## Stops

Built-in stops, plus: a ticket needs a secret or an account (e.g. crates.io); a test fails twice
for the same cause after the fix pass.

## Models

Coordinator: Opus, medium effort.
Researcher: Haiku, usual effort.
Implementer: Opus, low effort.
Standards reviewer: Sonnet, usual effort.
Spec reviewer: Opus, high effort.

## Commits

Short imperative summary; body has `Closes #<n>`. No `Co-Authored-By` lines. One commit per ticket
plus its fix commit.

## Concurrency

Parallel worktrees: at most 2 at once.

## Daily cap

Rolling: 14.3% of the plan's weekly usage per day.

## Ticket budget

4

## Writing

ASD-STE100

## Status line

Added.
