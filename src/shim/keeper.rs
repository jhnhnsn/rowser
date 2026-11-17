//! `rowser __keeper`: holds a Callback forward on the Remote host.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::mpsc;

use crate::protocol::{Conn, Hello, Msg};

const LOOPBACK: [IpAddr; 2] = [
    IpAddr::V4(Ipv4Addr::LOCALHOST),
    IpAddr::V6(Ipv6Addr::LOCALHOST),
];

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
            timeout: Duration::from_secs(600),
            liveness: Duration::from_secs(2),
        }
    }
}

/// Attaches to the forward, then dials the remote Callback port for each
/// `Dial` until no Callback port listens, a dial fails, the timeout passes,
/// or the Daemon closes the control connection.
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
    let s = UnixStream::connect(&args.socket).await?;
    Conn::connect(s, Hello::current()).await
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
                if !args.ports.iter().any(|&p| listens(p)) {
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

/// Connects to the remote Callback `port`, then opens a data connection
/// through the Tunnel and splices the two.
async fn dial(args: &KeeperArgs, conn_id: u64, port: u16) -> io::Result<()> {
    let mut remote = connect_loopback(port).await?;
    let mut data = connect(args).await.map_err(io::Error::other)?;
    data.send(&Msg::DataConn { conn_id })
        .await
        .map_err(io::Error::other)?;
    let (mut data, extra) = data.into_raw();
    remote.write_all(&extra).await?;
    // A splice that breaks after the dial is not a dial failure.
    let _ = tokio::io::copy_bidirectional(&mut data, &mut remote).await;
    Ok(())
}

async fn connect_loopback(port: u16) -> io::Result<TcpStream> {
    let mut last = None;
    for ip in LOOPBACK {
        match TcpStream::connect(SocketAddr::new(ip, port)).await {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::ErrorKind::ConnectionRefused.into()))
}

/// Whether something holds `port` on a loopback address. It tries to bind
/// the port: a bind refused with "address in use" means a program holds it.
/// This never connects to the program.
fn listens(port: u16) -> bool {
    LOOPBACK.iter().any(|&ip| {
        matches!(
            std::net::TcpListener::bind(SocketAddr::new(ip, port)),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse
        )
    })
}

/// `rowser __keeper` for the binary.
pub fn keeper_main(socket: PathBuf, forward_id: String, ports: Vec<u16>) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(run_keeper(KeeperArgs::new(socket, forward_id, ports))),
        Err(_) => ExitCode::FAILURE,
    }
}
