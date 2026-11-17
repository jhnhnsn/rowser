//! Callback forwards: local listeners and reverse-dial bookkeeping.

use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use crate::protocol::{Conn, Msg};

/// Lowest port that may be a Callback port.
const MIN_PORT: u16 = 1024;

type Control = Conn<UnixStream>;

/// All Callback forwards of one Daemon.
#[derive(Default)]
pub(super) struct Forwards {
    /// Forwards whose keeper has not attached yet.
    waiting: Mutex<HashMap<String, oneshot::Sender<Control>>>,
    /// Browser connections a keeper was asked to dial, by `conn_id`.
    pending: Mutex<HashMap<u64, TcpStream>>,
    next_conn: AtomicU64,
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
                Ok(ls) if !ls.is_empty() => listeners.extend(ls.into_iter().map(|l| (port, l))),
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
        let id = match random_id() {
            Ok(id) => id,
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
        let this = self.clone();
        let fid = id.clone();
        tokio::spawn(async move {
            this.run(listeners, rx, timeout).await;
            this.waiting.lock().unwrap().remove(&fid);
        });
        Created {
            forward_id: Some(id),
            warnings,
        }
    }

    /// Hands the keeper's control connection to its forward.
    pub fn attach(&self, forward_id: &str, control: Control) -> Result<(), Control> {
        let tx = self.waiting.lock().unwrap().remove(forward_id);
        match tx {
            Some(tx) => tx.send(control),
            None => Err(control),
        }
    }

    /// The browser connection a keeper was asked to dial as `conn_id`.
    pub fn take_pending(&self, conn_id: u64) -> Option<TcpStream> {
        self.pending.lock().unwrap().remove(&conn_id)
    }

    async fn run(
        &self,
        listeners: Vec<(u16, TcpListener)>,
        attach: oneshot::Receiver<Control>,
        timeout: Duration,
    ) {
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let (tx, mut rx) = mpsc::channel(64);
        let mut accepts = JoinSet::new();
        for (port, l) in listeners {
            let tx = tx.clone();
            accepts.spawn(async move {
                while let Ok((s, _)) = l.accept().await {
                    if tx.send((port, s)).await.is_err() {
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
                Some(x) = rx.recv() => queued.push(x),
            }
        };
        let mut dialed = vec![];
        for (port, s) in queued {
            if self.dial(&mut control, port, s, &mut dialed).await.is_err() {
                return self.forget(&dialed);
            }
        }
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                Some((port, s)) = rx.recv() => {
                    if self.dial(&mut control, port, s, &mut dialed).await.is_err() {
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
        port: u16,
        s: TcpStream,
        dialed: &mut Vec<u64>,
    ) -> crate::protocol::Result<()> {
        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().unwrap().insert(conn_id, s);
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

/// Splices a keeper's data connection to the browser connection `browser`.
pub(super) async fn splice(data: Control, mut browser: TcpStream) -> io::Result<()> {
    let (mut data, extra) = data.into_raw();
    browser.write_all(&extra).await?;
    tokio::io::copy_bidirectional(&mut data, &mut browser).await?;
    Ok(())
}

/// Binds `port` on every address in `loopback`. Addresses the host lacks are
/// skipped; any other failure is returned.
fn bind_port(port: u16, loopback: &[IpAddr]) -> io::Result<Vec<TcpListener>> {
    let mut out = vec![];
    for &ip in loopback {
        let std = match std::net::TcpListener::bind(SocketAddr::new(ip, port)) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::AddrNotAvailable => continue,
            Err(e) => return Err(e),
        };
        std.set_nonblocking(true)?;
        out.push(TcpListener::from_std(std)?);
    }
    Ok(out)
}

/// 32 hex characters from the OS random source.
fn random_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
