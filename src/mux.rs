//! `yamux` over a byte pipe (a Link's stdio), with tokio streams (ADR 0010).

use std::collections::VecDeque;
use std::io;
use std::task::Poll;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

/// One stream of a Link, as a tokio stream.
pub type Stream = Compat<yamux::Stream>;

/// Which end of the pipe this is: the Daemon is the client, the Outpost the server.
#[derive(Debug, Clone, Copy)]
pub enum Side {
    Daemon,
    Outpost,
}

type OpenReply = oneshot::Sender<io::Result<Stream>>;

/// Opens new streams on the pipe while its driver task runs.
#[derive(Clone)]
pub struct Opener {
    tx: mpsc::UnboundedSender<OpenReply>,
}

impl Opener {
    /// A new outbound stream. Fails when the pipe has closed.
    pub async fn open(&self) -> io::Result<Stream> {
        let (tx, rx) = oneshot::channel();
        let closed = || io::Error::new(io::ErrorKind::BrokenPipe, "the link closed");
        self.tx.send(tx).map_err(|_| closed())?;
        rx.await.map_err(|_| closed())?
    }
}

/// Starts the driver task for `io`. It returns the `Opener`, the inbound
/// streams (the channel closes when the pipe closes), and the task.
pub fn start<T>(
    io: T,
    side: Side,
) -> (
    Opener,
    mpsc::UnboundedReceiver<Stream>,
    tokio::task::JoinHandle<()>,
)
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mode = match side {
        Side::Daemon => yamux::Mode::Client,
        Side::Outpost => yamux::Mode::Server,
    };
    let mut conn = yamux::Connection::new(io.compat(), yamux::Config::default(), mode);
    let (open_tx, mut open_rx) = mpsc::unbounded_channel::<OpenReply>();
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut waiting: VecDeque<OpenReply> = VecDeque::new();
        std::future::poll_fn(|cx| {
            while let Poll::Ready(Some(tx)) = open_rx.poll_recv(cx) {
                waiting.push_back(tx);
            }
            while !waiting.is_empty() {
                match conn.poll_new_outbound(cx) {
                    Poll::Ready(r) => {
                        let tx = waiting.pop_front().expect("not empty");
                        let _ = tx.send(r.map(|s| s.compat()).map_err(io::Error::other));
                    }
                    Poll::Pending => break,
                }
            }
            loop {
                match conn.poll_next_inbound(cx) {
                    Poll::Ready(Some(Ok(s))) => {
                        let _ = in_tx.send(s.compat());
                    }
                    Poll::Ready(Some(Err(e))) => {
                        tracing::debug!("link closed: {e}");
                        return Poll::Ready(());
                    }
                    Poll::Ready(None) => return Poll::Ready(()),
                    Poll::Pending => return Poll::Pending,
                }
            }
        })
        .await;
    });
    (Opener { tx: open_tx }, in_rx, task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn streams_opened_on_one_side_arrive_on_the_other() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let (daemon, _din, _t1) = start(a, Side::Daemon);
        let (_outpost, mut oin, _t2) = start(b, Side::Outpost);
        let mut s = daemon.open().await.unwrap();
        s.write_all(b"ping").await.unwrap();
        s.flush().await.unwrap();
        let mut r = oin.recv().await.unwrap();
        let mut buf = [0u8; 4];
        r.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn the_inbound_channel_closes_with_the_pipe() {
        let (a, b) = tokio::io::duplex(1024);
        let (_o, mut oin, _t) = start(b, Side::Outpost);
        drop(a);
        assert!(oin.recv().await.is_none());
    }
}
