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

/// Whether a program listens on `port` at `ip` (see ADR 0005). This never
/// connects to the program.
///
/// Linux: a LISTEN entry in `/proc/net/tcp` or `/proc/net/tcp6`. A bind
/// probe would see a TIME-WAIT entry (the program closed a connection
/// first) as "in use" for about 60 s. Elsewhere, or if `/proc/net/tcp` is not
/// readable: the bind probe.
fn in_use(ip: IpAddr, port: u16) -> bool {
    #[cfg(target_os = "linux")]
    if let Some(listens) = proc_in_use(|p| std::fs::read_to_string(p), ip, port) {
        return listens;
    }
    bind_probe(ip, port)
}

/// The `/proc` answer for `in_use`, with `read` to read a file. `None` if
/// `/proc/net/tcp` is not readable. If `/proc/net/tcp6` is not readable
/// (IPv6 off), there are no IPv6 entries.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn proc_in_use(read: impl Fn(&str) -> io::Result<String>, ip: IpAddr, port: u16) -> Option<bool> {
    let v4 = read("/proc/net/tcp").ok()?;
    let v6 = read("/proc/net/tcp6").unwrap_or_default();
    Some(proc_listens(&v4, &v6, ip, port))
}

/// Whether the `/proc/net/tcp` (`v4`) and `/proc/net/tcp6` (`v6`) text
/// has a LISTEN entry that a dial to `ip`:`port` reaches: one on `ip`, or
/// on the unspecified address of the family. A `[::]` listener also takes
/// IPv4 dials unless it is IPv6-only, which `/proc` does not show; it
/// counts as listening on both.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn proc_listens(v4: &str, v6: &str, ip: IpAddr, port: u16) -> bool {
    let reaches = |addr: IpAddr| {
        addr == ip
            || addr.is_unspecified() && (addr.is_ipv6() || ip.is_ipv4())
            || matches!(addr, IpAddr::V6(a) if a.to_ipv4_mapped().map(IpAddr::V4) == Some(ip))
    };
    v4.lines()
        .chain(v6.lines())
        .filter_map(proc_listener)
        .any(|(addr, p)| p == port && reaches(addr))
}

/// The local address of one `/proc/net/tcp{,6}` line in state LISTEN
/// (`0A`). The address is hex in 32-bit words of host byte order.
fn proc_listener(line: &str) -> Option<(IpAddr, u16)> {
    let mut fields = line.split_whitespace();
    let local = fields.nth(1)?;
    if fields.nth(1)? != "0A" {
        return None;
    }
    let (addr, port) = local.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let mut bytes = Vec::with_capacity(16);
    for i in (0..addr.len()).step_by(8) {
        let word = u32::from_str_radix(addr.get(i..i + 8)?, 16).ok()?;
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    let ip = match bytes.len() {
        4 => IpAddr::from(<[u8; 4]>::try_from(bytes).ok()?),
        16 => IpAddr::from(<[u8; 16]>::try_from(bytes).ok()?),
        _ => return None,
    };
    Some((ip, port))
}

/// Whether something holds `port` on `ip`: a bind refused with "address in
/// use" means a program holds it.
fn bind_probe(ip: IpAddr, port: u16) -> bool {
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

/// Sample `/proc` text is in little-endian words.
#[cfg(all(test, target_endian = "little"))]
mod tests {
    use super::{proc_in_use, proc_listens};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    const HEAD: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
    const V4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

    fn v4(local: &str, st: &str) -> String {
        format!("{HEAD}   0: {local} 00000000:0000 {st} 00000000:00000000 00:00000000 00000000  1000        0 1 1 0\n")
    }

    fn v6(local: &str, st: &str) -> String {
        format!("{HEAD}   0: {local} 00000000000000000000000000000000:0000 {st} 00000000:00000000 00:00000000 00000000  1000        0 1 1 0\n")
    }

    #[test]
    fn a_listen_entry_on_the_dialed_address_and_port_listens() {
        let t = v4("0100007F:1F90", "0A");
        assert!(proc_listens(&t, HEAD, V4, 8080));
        assert!(!proc_listens(&t, HEAD, V4, 8081));
        assert!(!proc_listens(&t, HEAD, V6, 8080));
    }

    #[test]
    fn states_other_than_listen_do_not_listen() {
        for st in ["06", "01", "08", "07"] {
            assert!(
                !proc_listens(&v4("0100007F:1F90", st), HEAD, V4, 8080),
                "{st}"
            );
        }
    }

    #[test]
    fn an_ipv6_loopback_listener_listens_for_ipv6_only() {
        let t = v6("00000000000000000000000001000000:1F90", "0A");
        assert!(proc_listens(HEAD, &t, V6, 8080));
        assert!(!proc_listens(HEAD, &t, V4, 8080));
    }

    #[test]
    fn unspecified_listeners_take_dials_of_their_family_and_any_takes_both() {
        let any4 = v4("00000000:1F90", "0A");
        assert!(proc_listens(&any4, HEAD, V4, 8080));
        assert!(!proc_listens(&any4, HEAD, V6, 8080));
        let any6 = v6("00000000000000000000000000000000:1F90", "0A");
        assert!(proc_listens(HEAD, &any6, V4, 8080));
        assert!(proc_listens(HEAD, &any6, V6, 8080));
    }

    #[test]
    fn without_proc_net_tcp6_the_ipv4_text_alone_decides() {
        let t = v4("0100007F:1F90", "0A");
        let read = |path: &str| match path {
            "/proc/net/tcp" => Ok(t.clone()),
            _ => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        };
        assert_eq!(proc_in_use(read, V4, 8080), Some(true));
        assert_eq!(proc_in_use(read, V4, 8081), Some(false));
        assert_eq!(proc_in_use(read, V6, 8080), Some(false));
    }

    #[test]
    fn without_proc_net_tcp_proc_does_not_decide() {
        let read = |_: &str| Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(proc_in_use(read, V4, 8080), None);
    }

    #[test]
    fn another_address_or_a_malformed_line_does_not_listen() {
        assert!(!proc_listens(&v4("0101007F:1F90", "0A"), HEAD, V4, 8080));
        assert!(!proc_listens("   0: zz:1F90 0:0 0A\n", HEAD, V4, 8080));
    }
}
