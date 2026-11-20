# A control socket for local commands

The Daemon gets a third socket, `rowserd-control.sock`, in its runtime directory (mode 0600,
directory 0700). rowser commands on the Local machine (`status`, `activity`, `pause`, `resume`,
`pending`, `approve`, `deny`, `reload`) use it to read and change the running Daemon. It speaks
the same NDJSON protocol and handshake as the direct and relay sockets, with new message types
(protocol 1.1). We chose this over signals, a state file or an HTTP port: signals carry no
answers, a state file races with the Daemon, and a TCP port is reachable by every local user
and by a browser.

## Consequences

- Trust comes from the socket file's mode and directory, as for the other sockets. Only the
  same user can connect. The control socket is never forwarded: the Wrapper's `-R` forward
  points only at its per-session socket, which relays to the relay socket.
- Control messages are accepted only on the control socket. The direct and relay sockets refuse
  them, so a Remote host can never pause, approve or reconfigure the Daemon.
- The menu bar app (#35) is a client of this socket. The Daemon keeps all state and logic, so
  the CLI and any later UI always agree.
