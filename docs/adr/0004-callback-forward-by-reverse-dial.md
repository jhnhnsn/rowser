# Callback forwards reverse-dial through the Tunnel

A Callback forward needs connections to flow from the Local machine to the Remote host, but the
Tunnel only lets the Remote host connect to the Local machine. Instead of adding a dynamic
`ssh -L` (which needs a ControlMaster socket the user may not have configured) we reverse-dial:
the Shim keeps a control connection open; for each browser connection to the local Callback
port, the Daemon asks the Shim, and the Shim opens a fresh connection through the Tunnel and
joins it to the remote Callback port. Everything travels over the one forwarded socket, so it
works with plain `ssh` and with any `ssh_config`.

## Consequences

- The Shim must not block its caller, because many programs wait for `xdg-open` to exit. It hands
  the Callback forward to a detached background process (`setsid`) and exits as soon as the Daemon
  accepts the Open request. Detection of the caller's listening ports happens before detaching.
- The background process ends the forward when the remote Callback port stops listening, when a
  dial fails, or after 10 minutes.
