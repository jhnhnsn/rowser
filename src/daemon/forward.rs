//! Callback forwards: local listeners and reverse-dial bookkeeping.

use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use super::NoticeHost;
use crate::protocol::{Conn, ForwardInfo, Msg};

/// Lowest port that may be a Callback port.
const MIN_PORT: u16 = 1024;

type Control = Conn<UnixStream>;

/// A browser connection that waits for the keeper's data connection.
struct Pending {
    forward_id: String,
    browser: TcpStream,
}

/// All Callback forwards of one Daemon.
#[derive(Default)]
pub(super) struct Forwards {
    /// Forwards whose keeper has not attached yet.
    waiting: Mutex<HashMap<String, oneshot::Sender<Control>>>,
    /// Browser connections a keeper was asked to dial, by `conn_id`. The ids
    /// are random, and a `DataConn` must also name the forward.
    pending: Mutex<HashMap<u64, Pending>>,
    /// Every running forward, for `rowser status`.
    active: Mutex<HashMap<String, Active>>,
}

/// What `rowser status` shows of a running forward.
struct Active {
    ports: Vec<u16>,
    host: String,
    unverified: bool,
    started: tokio::time::Instant,
}

/// What `Forwards::create` did.
pub(super) struct Created {
    pub forward_id: Option<String>,
    pub warnings: Vec<String>,
}

impl Forwards {
    /// Binds each port on every loopback address and starts a forward for the
    /// ports that bound. Ports that cannot be bound give warnings.
    pub fn create(
        self: &Arc<Self>,
        ports: &[u16],
        host: &NoticeHost,
        loopback: &[IpAddr],
        timeout: Duration,
    ) -> Created {
        let mut warnings = vec![];
        let mut listeners = vec![];
        for &port in ports {
            if port < MIN_PORT {
                warnings.push(format!(
                    "port {port} is not a callback port (only 1024-65535); it is not forwarded"
                ));
                continue;
            }
            match bind_port(port, loopback) {
                Ok(bound) if !bound.is_empty() => {
                    listeners.extend(bound.into_iter().map(|listener| (port, listener)))
                }
                Ok(_) => warnings.push(format!(
                    "no loopback address is available for local port {port}"
                )),
                Err(e) if e.kind() == io::ErrorKind::AddrInUse => warnings.push(format!(
                    "local port {port} is busy; the callback cannot be forwarded"
                )),
                Err(e) => warnings.push(format!("could not bind local port {port}: {e}")),
            }
        }
        if listeners.is_empty() {
            return Created {
                forward_id: None,
                warnings,
            };
        }
        let id = match random_bytes::<16>() {
            Ok(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            Err(e) => {
                warnings.push(format!("could not make a forward id: {e}"));
                return Created {
                    forward_id: None,
                    warnings,
                };
            }
        };
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(id.clone(), tx);
        let mut bound: Vec<u16> = listeners.iter().map(|(port, _)| *port).collect();
        // One listener per loopback address: list each port once.
        bound.sort_unstable();
        bound.dedup();
        let (host, unverified) = match host {
            NoticeHost::Alias(a) => (a.clone(), false),
            NoticeHost::Reported(r) => (r.clone(), true),
        };
        self.active.lock().unwrap().insert(
            id.clone(),
            Active {
                ports: bound,
                host,
                unverified,
                started: tokio::time::Instant::now(),
            },
        );
        let this = self.clone();
        let forward_id = id.clone();
        tokio::spawn(async move {
            this.run(&forward_id, listeners, rx, timeout).await;
            this.waiting.lock().unwrap().remove(&forward_id);
            this.active.lock().unwrap().remove(&forward_id);
        });
        Created {
            forward_id: Some(id),
            warnings,
        }
    }

    /// One entry per local port of each running forward, oldest first.
    pub fn list(&self) -> Vec<ForwardInfo> {
        let active = self.active.lock().unwrap();
        let mut all: Vec<&Active> = active.values().collect();
        all.sort_by_key(|a| a.started);
        all.into_iter()
            .flat_map(|a| {
                a.ports.iter().map(|&port| ForwardInfo {
                    port,
                    host: a.host.clone(),
                    unverified: a.unverified,
                    age_secs: a.started.elapsed().as_secs(),
                })
            })
            .collect()
    }

    /// Ends a forward whose keeper has not attached, and frees its ports.
    pub fn close(&self, forward_id: &str) {
        // Dropping the sender ends `run`, which drops the listeners.
        self.waiting.lock().unwrap().remove(forward_id);
    }

    /// Hands the keeper's control connection to its forward.
    pub fn attach(&self, forward_id: &str, control: Control) -> Result<(), Control> {
        let tx = self.waiting.lock().unwrap().remove(forward_id);
        match tx {
            Some(tx) => tx.send(control),
            None => Err(control),
        }
    }

    /// The browser connection that the keeper of `forward_id` was asked to
    /// dial as `conn_id`. A wrong forward id leaves the connection pending.
    pub fn take_pending(&self, forward_id: &str, conn_id: u64) -> Option<TcpStream> {
        let mut pending = self.pending.lock().unwrap();
        match pending.get(&conn_id) {
            Some(p) if p.forward_id == forward_id => pending.remove(&conn_id).map(|p| p.browser),
            _ => None,
        }
    }

    async fn run(
        &self,
        forward_id: &str,
        listeners: Vec<(u16, TcpListener)>,
        attach: oneshot::Receiver<Control>,
        timeout: Duration,
    ) {
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let (tx, mut rx) = mpsc::channel(64);
        let mut accepts = JoinSet::new();
        for (port, listener) in listeners {
            let tx = tx.clone();
            accepts.spawn(async move {
                while let Ok((browser, _)) = listener.accept().await {
                    if tx.send((port, browser)).await.is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);

        let mut queued = vec![];
        tokio::pin!(attach);
        let mut control = loop {
            tokio::select! {
                _ = &mut deadline => return,
                c = &mut attach => match c {
                    Ok(c) => break c,
                    Err(_) => return,
                },
                Some(accepted) = rx.recv() => queued.push(accepted),
            }
        };
        let mut dialed = vec![];
        for (port, browser) in queued {
            let dial = self.dial(&mut control, forward_id, port, browser, &mut dialed);
            if dial.await.is_err() {
                return self.forget(&dialed);
            }
        }
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                Some((port, browser)) = rx.recv() => {
                    let dial = self.dial(&mut control, forward_id, port, browser, &mut dialed);
                    if dial.await.is_err() {
                        break;
                    }
                }
                // The keeper sends nothing after attaching: any message or
                // EOF ends the forward.
                _ = control.recv() => break,
            }
        }
        self.forget(&dialed);
    }

    async fn dial(
        &self,
        control: &mut Control,
        forward_id: &str,
        port: u16,
        browser: TcpStream,
        dialed: &mut Vec<u64>,
    ) -> crate::protocol::Result<()> {
        let conn_id = u64::from_ne_bytes(random_bytes::<8>()?);
        self.pending.lock().unwrap().insert(
            conn_id,
            Pending {
                forward_id: forward_id.into(),
                browser,
            },
        );
        dialed.push(conn_id);
        control.send(&Msg::Dial { conn_id, port }).await
    }

    /// Drops browser connections the keeper never picked up.
    fn forget(&self, dialed: &[u64]) {
        let mut pending = self.pending.lock().unwrap();
        for id in dialed {
            pending.remove(id);
        }
    }
}

/// Binds `port` on every address in `loopback`. Addresses the host lacks are
/// skipped; any other failure is returned.
fn bind_port(port: u16, loopback: &[IpAddr]) -> io::Result<Vec<TcpListener>> {
    let mut out = vec![];
    for &ip in loopback {
        let std = match std::net::TcpListener::bind(SocketAddr::new(ip, port)) {
            Ok(listener) => listener,
            Err(e) if e.kind() == io::ErrorKind::AddrNotAvailable => continue,
            Err(e) => return Err(e),
        };
        std.set_nonblocking(true)?;
        out.push(TcpListener::from_std(std)?);
    }
    Ok(out)
}

/// `N` bytes from the OS random source.
fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}
