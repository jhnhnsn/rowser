---
status: accepted (supersedes ADR 0002 and ADR 0003)
---

# The Daemon owns a Link to each Saved host

A Tunnel that lives inside the user's own `ssh` session (the Wrapper, `rowser ssh`) ends when that
session ends, but the programs that need a browser often outlive it: detached herdr agents,
tmux jobs, cron. So the Daemon keeps its own SSH connection, a Link, to each Saved host the user
added once with `rowser host add`, always open while it runs and reconnected with backoff. The
Host alias is trusted because the Daemon chose which host to dial, so no Host stamp, relay socket
or Wrapper is needed, and plain `ssh` works for the user's own sessions.

## Considered options

- Keep the Wrapper and add Links beside it: two models, two trust paths and two sets of docs,
  for no user the Links do not cover.
- A Link only while a `rowser ssh` or herdr session is open: does not fix detached jobs.
- A direct LAN or Tailscale connection from the Remote host to the Local machine: needs a network
  listener on the Local machine and authentication of rowser's own (ADR 0001).

## Consequences

- A Link must log in with no prompt (`BatchMode=yes`). A failed login stops the retries in a
  "needs login" state; `rowser host connect` logs in from a terminal and leaves a master on
  rowser's own control path for the Link to use.
- The Link uses `ControlMaster=no` with a control path of rowser's own, never the user's, so it
  never rides on, or becomes, the master of the user's sessions.
- The direct socket, the `ssh_config` `RemoteForward` setup, the Reported host name and
  `[unverified]` are removed: every request arrives on a Link and has a Host alias.
- The rate limit is per Saved host, because one Link carries many streams.
