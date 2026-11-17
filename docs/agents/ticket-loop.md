# ticket-loop config

## Tickets

GitHub issues in `jhnhnsn/rowser`. A parent holds its tickets as sub-issues (also in the `v1`
milestone). Blockers are listed in each ticket body's `## Blocked by` section. Labels:
`ready-for-agent`, `ready-for-human`.

Read a ticket: `gh issue view <n> --repo jhnhnsn/rowser --json title,body,labels,comments`.

## Order

Parent #1. Inside it: the lowest-numbered ready ticket with no open blockers.

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
- `rowser ssh` uses the `ssh` on `PATH` and the Daemon in `$XDG_RUNTIME_DIR/rowser/`: put a fake
  `ssh` script first on `PATH` and set `XDG_RUNTIME_DIR` to a short temp dir to test it.
- Unix socket paths are limited to about 104–108 bytes: put test sockets in a short temp dir
  (e.g. `/tmp/rw.XXXX`), never under a long scratchpad or worktree path; remove it afterwards.
- Never open the real browser or show real desktop notifications in a test.
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

Each ticket: `git push origin main`, no publish step. The milestone ships at once: after the last
ticket the user tags `v0.1.0` and the release workflow (#12) builds binaries. Reserving the
crates.io name needs the user's token: `ready-for-human`.

## Changelog

`CHANGELOG.md`, Keep a Changelog format. Each shipped ticket adds an entry under
`## [Unreleased]`, newest first, for rowser users: what a user can now do, with the issue link.
The milestone summary goes in `## [0.1.0]` at tagging. Milestones still to come are not listed.

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
