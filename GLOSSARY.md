# rowser

rowser lets programs on a headless remote host open URLs in the browser on the user's local
machine, and carries OAuth callbacks back to the remote host.

## Places

**Local machine**:
The machine the user sits at, with a desktop and a browser. Runs the Daemon and the Wrapper.
_Avoid_: client, laptop, desktop

**Remote host**:
A headless machine the user connects to over SSH. Runs the Shim.
_Avoid_: server, box, headless machine

## Parts

**Daemon**:
The long-running process on the Local machine that receives Open requests and opens URLs in
the browser.
_Avoid_: server, agent, rowserd (except as the socket's file name)

**Wrapper**:
The `rowser ssh` command on the Local machine that starts an SSH session with a Tunnel and
relays the Tunnel to the Daemon for the life of that session.
_Avoid_: ssh shim, launcher

**Shim**:
The `rowser open` command on the Remote host, also reached through the `xdg-open`,
`sensible-browser` and `www-browser` names and through `BROWSER`.
_Avoid_: client, opener, fake xdg-open

## Connection

**Tunnel**:
The SSH remote forward that joins a Session socket on the Remote host to the Wrapper (or
directly to the Daemon) on the Local machine.
_Avoid_: forward (reserved for Callback forward), pipe, channel

**Session socket**:
The Unix socket on the Remote host that one SSH session's Tunnel listens on.
_Avoid_: remote socket, opener socket

**Stale socket**:
A Session socket file left behind after its SSH session ended, which no longer accepts
connections.
_Avoid_: dead socket, orphan

**Direct socket**:
The Daemon's socket for Tunnels that reach it without a Wrapper. Requests on it carry only a
Reported host name.
_Avoid_: main socket, daemon socket

**Relay socket**:
The Daemon's socket that only Wrappers connect to. Every connection on it carries a Host alias.
_Avoid_: wrapper socket, trusted socket

**Host stamp**:
The Host alias that a Wrapper attaches to a connection it relays.
_Avoid_: host header, tag

**Host alias**:
The host name the user typed to the Wrapper. It is the trusted name of a Remote host.
_Avoid_: hostname, host label

**Reported host name**:
The host name that a Shim claims for itself. It is shown to the user but never trusted.
_Avoid_: hostname

## Requests

**Open request**:
A Shim's request that the Daemon open one URL in the browser.
_Avoid_: open message, URL event, job

**Callback port**:
A loopback TCP port on the Remote host where a program waits for an OAuth redirect.
_Avoid_: redirect port, OAuth port, listener

**Callback forward**:
The temporary path that carries browser connections to a Callback port on the Local machine
through the Tunnel to the same Callback port on the Remote host.
_Avoid_: port forward, tunnel (reserved for Tunnel), proxy

**Confirmed host**:
A Remote host whose Open requests wait for the user to approve them before the browser opens.
_Avoid_: untrusted host, prompt host
