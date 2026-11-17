//! `rowser __keeper`: holds a Callback forward on the Remote host.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use tokio::net::{TcpStream, UnixStream};
use tokio::sync::mpsc;

use crate::callback::{FORWARD_TIMEOUT, LOOPBACK};
use crate::protocol::{Conn, Hello, Msg};

/// What a keeper needs to hold one Callback forward.
#[derive(Debug, Clone)]
pub struct KeeperArgs {
    /// The Session socket the Shim used.
    pub socket: PathBuf,
    pub forward_id: String,
    /// The remote Callback ports.
    pub ports: Vec<u16>,
    /// The forward ends this long after the keeper attaches.
    pub timeout: Duration,
    /// How often the keeper checks that a Callback port still listens.
    pub liveness: Duration,
}

impl KeeperArgs {
    /// Production values: 10-minute timeout, 2-second liveness check.
    pub fn new(socket: PathBuf, forward_id: String, ports: Vec<u16>) -> KeeperArgs {
        KeeperArgs {
            socket,
            forward_id,
            ports,
            timeout: FORWARD_TIMEOUT,
            liveness: Duration::from_secs(2),
        }
    }
}

/// Attaches to the forward, then dials the remote Callback port for each
/// `Dial` until the Callback ports stop listening (see `Liveness`), a dial
/// fails, the timeout passes, or the Daemon closes the control connection.
pub async fn run_keeper(args: KeeperArgs) -> ExitCode {
    match keep(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::debug!("keeper: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn connect(args: &KeeperArgs) -> crate::protocol::Result<Conn<UnixStream>> {
    let stream = UnixStream::connect(&args.socket).await?;
    Conn::connect(stream, Hello::current()).await
}

async fn keep(args: KeeperArgs) -> crate::protocol::Result<()> {
    let mut control = connect(&args).await?;
    control
        .send(&Msg::AttachForward {
            forward_id: args.forward_id.clone(),
        })
        .await?;
    let deadline = tokio::time::sleep(args.timeout);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(args.liveness);
    let mut liveness = Liveness::new(&args.ports);
    let (failed_tx, mut failed) = mpsc::channel::<()>(1);
    // A reader task, because `recv` loses a partial line when a select
    // drops it.
    let (msg_tx, mut msgs) = mpsc::channel(16);
    let reader = tokio::spawn(async move {
        loop {
            let msg = control.recv().await;
            let end = !matches!(msg, Ok(Some(_)));
            if msg_tx.send(msg).await.is_err() || end {
                break;
            }
        }
    });
    let _abort = AbortOnDrop(reader);
    loop {
        tokio::select! {
            _ = &mut deadline => return Ok(()),
            _ = tick.tick() => {
                if !liveness.check(in_use) {
                    return Ok(());
                }
            }
            _ = failed.recv() => return Ok(()),
            msg = msgs.recv() => match msg.transpose()?.flatten() {
                Some(Msg::Dial { conn_id, port }) if args.ports.contains(&port) => {
                    let args = args.clone();
                    let failed = failed_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = dial(&args, conn_id, port).await {
                            tracing::debug!("keeper: dial {port}: {e}");
                            let _ = failed.try_send(());
                        }
                    });
                }
                // Anything else, or EOF, ends the forward.
                _ => return Ok(()),
            },
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The remote-port liveness rule.
///
/// For each port the keeper watches one loopback address: the first address
/// in dial order (`LOOPBACK`) that it saw in use. That is the address a dial
/// reaches. Another program on the other address, such as a Daemon on the
/// same host, does not keep the forward alive.
///
/// The keeper ends when a port listened once and now no watched address is
/// in use. Before any port listened, only the timeout ends the forward, so
/// `rowser forward` may start before the program does.
struct Liveness {
    /// Per port: the index in `LOOPBACK` of the watched address.
    watched: Vec<(u16, Option<usize>)>,
}

impl Liveness {
    fn new(ports: &[u16]) -> Liveness {
        Liveness {
            watched: ports.iter().map(|&p| (p, None)).collect(),
        }
    }

    /// Whether the forward stays alive. `in_use` tells if something holds
    /// a port on an address.
    fn check(&mut self, in_use: impl Fn(IpAddr, u16) -> bool) -> bool {
        let mut listened_once = false;
        let mut listens = false;
        for (port, watched) in &mut self.watched {
            // An earlier address in dial order replaces the watched one.
            let last = watched.unwrap_or(LOOPBACK.len() - 1);
            if let Some(i) = (0..=last).find(|&i| in_use(LOOPBACK[i], *port)) {
                *watched = Some(i);
                listens = true;
            }
            listened_once |= watched.is_some();
        }
        listens || !listened_once
    }
}

/// Connects to the remote Callback `port`, then opens a data connection
/// through the Tunnel and splices the two.
async fn dial(args: &KeeperArgs, conn_id: u64, port: u16) -> io::Result<()> {
    let mut remote = connect_loopback(port).await?;
    let mut data = connect(args).await.map_err(io::Error::other)?;
    data.send(&Msg::DataConn {
        forward_id: args.forward_id.clone(),
        conn_id,
    })
    .await
    .map_err(io::Error::other)?;
    // A splice that breaks after the dial is not a dial failure.
    let _ = data.splice(&mut remote).await;
    Ok(())
}

async fn connect_loopback(port: u16) -> io::Result<TcpStream> {
    let mut last = None;
    for ip in LOOPBACK {
        match TcpStream::connect(SocketAddr::new(ip, port)).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::ErrorKind::ConnectionRefused.into()))
}

/// Whether something holds `port` on `ip`. It tries to bind the port: a bind
/// refused with "address in use" means a program holds it. This never
/// connects to the program.
fn in_use(ip: IpAddr, port: u16) -> bool {
    matches!(
        std::net::TcpListener::bind(SocketAddr::new(ip, port)),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse
    )
}

/// `rowser __keeper` for the binary.
pub fn keeper_main(socket: PathBuf, forward_id: String, ports: Vec<u16>) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(run_keeper(KeeperArgs::new(socket, forward_id, ports))),
        Err(_) => ExitCode::FAILURE,
    }
}
