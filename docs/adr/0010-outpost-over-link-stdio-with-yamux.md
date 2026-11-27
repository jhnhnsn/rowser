---
status: accepted (supersedes the dial mechanism of ADR 0004)
---

# The Outpost speaks protocol 2.0 over the Link's stdio, with yamux

A Link runs `ssh -T <host> <remote_path> __outpost` and carries the protocol over its stdin and
stdout, not over an `ssh -R` socket forward. Many streams (Shim connections, Callback forward
connections, control exchanges) share that one pipe through `yamux`. The long-running Outpost at
the far end owns the Outpost socket `/tmp/rowser-<uid>/outpost.sock` and dials Callback ports
itself, so it needs no `AllowStreamLocalForwarding` on the Remote host, leaves no Stale sockets,
and replaces the detached Keeper.

## Considered options

- The Daemon opens the `-R` socket forward that the Wrapper used: less new code, but keeps the
  Stale socket problem (`StreamLocalBindUnlink` needs root), the per-session glob discovery and
  the Keepers, and fails on hosts that turn stream-local forwarding off.
- Our own frame format (stream id and length): no dependency, but we would write and test flow
  control ourselves.

## Consequences

- The Outpost socket path comes from the UID alone, never from `XDG_RUNTIME_DIR`, which cron
  jobs and services do not have but SSH logins do. The folder must belong to the user with mode
  0700; another user who creates it first only gets the Link refused with a clear error.
- When two Local machines have a Link to one host, the one that connected last takes the Open
  requests, and the other takes them again when it drops.
- The Outpost exits when its Link closes, which ends its Callback forwards. ADR 0004's idea stays:
  browser connections go back through the one SSH connection.
- Protocol 2.0; a 1.x peer gets the major version mismatch error.
