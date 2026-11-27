//! Callback forwards: local listeners, relayed through the Link's Outpost.

use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use super::NoticeHost;
use crate::mux;
use crate::protocol::{Conn, ForwardInfo, Hello, Msg};

/// Lowest port that may be a Callback port.
const MIN_PORT: u16 = 1024;

/// All Callback forwards of one Daemon.
#[derive(Default)]
pub(super) struct Forwards {
    /// Every running forward, for `rowser status`.
    active: Mutex<HashMap<String, Active>>,
    /// Ends a running forward (`close`).
    stops: Mutex<HashMap<String, oneshot::Sender<()>>>,
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
    /// The local ports that bound.
    pub ports: Vec<u16>,
    pub warnings: Vec<String>,
}

impl Forwards {
    /// Binds each port on every loopback address and starts a forward for the
    /// ports that bound, through the Link of `link`. Ports that cannot be
    /// bound give warnings. With no Link (a request on a socket), nothing is
    /// forwarded and each port gives a warning. When the Outpost's watch
    /// stream cannot open, nothing is forwarded and a warning says so.
    pub async fn create(
        self: &Arc<Self>,
        ports: &[u16],
        host: &NoticeHost,
        link: Option<&mux::Opener>,
        loopback: &[IpAddr],
        timeout: Duration,
        on_end: impl FnOnce(&str, Duration) + Send + 'static,
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
            if link.is_none() {
                warnings.push(format!(
                    "callback forwards need a Link to a Saved host; port {port} is not forwarded"
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
        let none = |warnings| Created {
            forward_id: None,
            ports: vec![],
            warnings,
        };
        let Some(link) = link.filter(|_| !listeners.is_empty()) else {
            return none(warnings);
        };
        let id = match random_bytes::<16>() {
            Ok(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            Err(e) => {
                warnings.push(format!("could not make a forward id: {e}"));
                return none(warnings);
            }
        };
        let mut bound: Vec<u16> = listeners.iter().map(|(port, _)| *port).collect();
        // One listener per loopback address: list each port once.
        bound.sort_unstable();
        bound.dedup();
        // Open the watch stream before the reply: when it fails, no forward
        // starts, and the reply says so.
        let watch = match open(
            link,
            &Msg::Watch {
                ports: bound.clone(),
            },
        )
        .await
        {
            Ok(conn) => conn,
            Err(e) => {
                let list = bound
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                warnings.push(format!(
                    "the Outpost cannot watch the callback ports ({e}); port {list} is not forwarded"
                ));
                return none(warnings);
            }
        };
        let ports = bound.clone();
        let (host, unverified) = host.parts();
        let host = host.to_string();
        self.active.lock().unwrap().insert(
            id.clone(),
            Active {
                ports: bound.clone(),
                host,
                unverified,
                started: tokio::time::Instant::now(),
            },
        );
        let (stop_tx, stop) = oneshot::channel();
        self.stops.lock().unwrap().insert(id.clone(), stop_tx);
        let this = self.clone();
        let forward_id = id.clone();
        let link = link.clone();
        let started = tokio::time::Instant::now();
        tokio::spawn(async move {
            run(link, watch, listeners, stop, timeout).await;
            this.stops.lock().unwrap().remove(&forward_id);
            this.active.lock().unwrap().remove(&forward_id);
            on_end(&forward_id, started.elapsed());
        });
        Created {
            forward_id: Some(id),
            ports,
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

    /// Ends a forward and frees its ports.
    pub fn close(&self, forward_id: &str) {
        if let Some(stop) = self.stops.lock().unwrap().remove(forward_id) {
            let _ = stop.send(());
        }
    }
}

/// Runs one forward until the Outpost closes the watch stream (the ports
/// stopped listening, or the Link dropped), a dial fails, `stop` fires or
/// `timeout` passes. Dropping the listeners frees the local ports;
/// connections already relayed go on until they close.
async fn run(
    link: mux::Opener,
    mut watch: Conn<mux::Stream>,
    listeners: Vec<(u16, TcpListener)>,
    stop: oneshot::Receiver<()>,
    timeout: Duration,
) {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline, stop);
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
    let (failed_tx, mut failed) = mpsc::channel::<()>(1);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            _ = &mut stop => break,
            // The Outpost sends nothing on the watch stream: any message or
            // EOF ends the forward.
            _ = watch.recv() => break,
            _ = failed.recv() => break,
            Some((port, browser)) = rx.recv() => {
                let link = link.clone();
                let failed = failed_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = relay(&link, port, browser).await {
                        tracing::debug!("forward: dial {port}: {e}");
                        let _ = failed.try_send(());
                    }
                });
            }
        }
    }
}

/// A new Link stream after the handshake, with `first` sent on it.
async fn open(link: &mux::Opener, first: &Msg) -> crate::protocol::Result<Conn<mux::Stream>> {
    let mut conn = Conn::connect(link.open().await?, Hello::current()).await?;
    conn.send(first).await?;
    Ok(conn)
}

/// Asks the Outpost to dial the remote `port`, then relays `browser` to it.
/// An error means the dial failed.
async fn relay(link: &mux::Opener, port: u16, mut browser: TcpStream) -> io::Result<()> {
    let mut conn = open(link, &Msg::Dial { port })
        .await
        .map_err(io::Error::other)?;
    match conn.recv().await.map_err(io::Error::other)? {
        Some(Msg::OpenResult { error: None, .. }) => {}
        Some(Msg::OpenResult { error: Some(e), .. }) => return Err(io::Error::other(e)),
        other => return Err(io::Error::other(format!("unexpected reply: {other:?}"))),
    }
    // A relay that breaks after the dial is not a dial failure.
    let _ = conn.splice(&mut browser).await;
    Ok(())
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
