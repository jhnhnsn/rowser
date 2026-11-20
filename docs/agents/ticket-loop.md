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
  local side binds only `[::1]`, and run the fake remote listener on `127.0.0.1` (the keeper
  dials `127.0.0.1` first). `--loopback` refuses non-loopback addresses. `rowser open` and
  `rowser forward` start a detached `rowser __keeper`. It watches only the address it dials,
  so the daemon on `[::1]` does not keep it alive. It ends when a port that listened once stops
  (2 s checks); before that, only the 10-minute timeout ends it. Kill leftover keepers with
  `pkill -f __keeper`.
- The Shim's no-tunnel fallback (graphical session) runs the first `xdg-open` on `PATH` that is
  not rowser: put a fake `xdg-open` script that records its argument first on `PATH`, set
  `DISPLAY`, and set the hidden `ROWSER_TEST_TUNNEL_DIR` to an empty temp dir (it replaces
  `/tmp` for socket discovery) and `ROWSER_SOCKET` to a missing path, so no real Tunnel answers.
  Never let it reach the real `xdg-open`.
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
  first on `PATH` that answers `uname -sm` and runs the copy command locally with `sh -c`, and set
  `HOME` to a temp dir. `--local-build <path>` skips the download; the hidden `--release-url
  <base>` points the download at a local HTTP server serving `rowser-<target>.tar.gz` and
  `SHA256SUMS` (`sha256sum` output; the download is refused without a matching entry).
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

Parallel worktrees: at most 3 at once.

## Daily cap

Rolling: 14.3% of the plan's weekly usage per day.

## Ticket budget

6

## Writing

ASD-STE100

## Status line

Added.
