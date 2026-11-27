# rowser

rowser lets programs on a headless remote host open URLs in the browser on the user's local
machine, and carries OAuth callbacks back to the remote host.

## Places

**Local machine**:
The machine the user sits at, with a desktop and a browser. Runs the Daemon.
_Avoid_: client, laptop, desktop

**Remote host**:
A headless machine the user connects to over SSH. Runs the Shim and the Outpost.
_Avoid_: server, box, headless machine

**Saved host**:
A Remote host that the user added to rowser, so that the Daemon keeps a Link to it.
_Avoid_: known host, paired host, configured host

## Parts

**Daemon**:
The long-running process on the Local machine that keeps the Links, receives Open requests and
opens URLs in the browser.
_Avoid_: server, agent, rowserd (except as the socket's file name)

**Shim**:
The `rowser open` command on the Remote host, also reached through the `xdg-open`,
`sensible-browser` and `www-browser` names and through `BROWSER`.
_Avoid_: client, opener, fake xdg-open

**Outpost**:
The long-running rowser process on a Remote host, at the far end of a Link, that takes the
Shim's Open requests and carries Callback forwards.
_Avoid_: agent, remote daemon, server, keeper

## Connection

**Link**:
The SSH connection that the Daemon keeps open to one Saved host, with the Outpost at its far end.
_Avoid_: tunnel, session, channel

**Outpost socket**:
The Unix socket on the Remote host where the Outpost takes the Shim's connections. There is one
for each user on the host.
_Avoid_: session socket, remote socket

**Host alias**:
The name of a Saved host, as the user gave it to `rowser host add` and as the Daemon gives it to
`ssh`. It is the trusted name of a Remote host.
_Avoid_: hostname, host label, reported host name

## Requests

**Open request**:
A Shim's request that the Daemon open one URL in the browser.
_Avoid_: open message, URL event, job

**Callback port**:
A loopback TCP port on the Remote host where a program waits for an OAuth redirect.
_Avoid_: redirect port, OAuth port, listener

**Callback forward**:
The temporary path that carries browser connections to a Callback port on the Local machine
through the Link to the same Callback port on the Remote host.
_Avoid_: port forward, tunnel, proxy

**Confirmed host**:
A Remote host whose Open requests wait for the user to approve them before the browser opens.
_Avoid_: untrusted host, prompt host

**Pending request**:
An Open request (or `rowser forward` request) that waits for the user to approve or deny it.
_Avoid_: prompt, question, approval

## Browser selection

**Route**:
A rule that sends the Open requests of matching Host aliases, and optionally only matching URLs,
to one Target.
_Avoid_: rule, mapping, binding

**Target**:
Where a Route sends a URL: a browser with an optional Profile, the system opener, or "ask".
_Avoid_: destination, handler

**Profile**:
One of a browser's separate user profiles, each with its own sign-ins.
_Avoid_: account, persona, space

## Local control

**Control socket**:
The Daemon's socket for rowser commands run by the same user on the Local machine.
_Avoid_: admin socket, API socket
