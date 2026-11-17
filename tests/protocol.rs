use rowser::protocol::{Conn, Error, Hello, Msg, Version};
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, BufReader};

/// Longer than the protocol's maximum line length (64 KiB).
const TOO_LONG: usize = 64 * 1024 + 10;

fn current() -> Version {
    Hello::current().version()
}

async fn pair() -> (Conn<tokio::io::DuplexStream>, Conn<tokio::io::DuplexStream>) {
    let (a, b) = duplex(1 << 20);
    let (client, server) = tokio::join!(Conn::connect(a, Hello::current()), Conn::accept(b));
    let (server, hello) = server.unwrap();
    assert_eq!(hello.version(), current());
    (client.unwrap(), server)
}

fn all_messages() -> Vec<Msg> {
    vec![
        Msg::HostStamp {
            alias: "devbox".into(),
        },
        Msg::Open {
            url: "https://example.com/auth?redirect_uri=http://localhost:8400/cb".into(),
            reported_host: "devbox.internal".into(),
            callback_ports: vec![8400],
        },
        Msg::OpenResult {
            forward_id: Some("f1".into()),
            warnings: vec!["port 8400 busy".into()],
            error: None,
        },
        Msg::OpenResult {
            forward_id: None,
            warnings: vec![],
            error: Some("scheme not allowed".into()),
        },
        Msg::Forward {
            reported_host: "devbox".into(),
            callback_ports: vec![8080],
        },
        Msg::AttachForward {
            forward_id: "f1".into(),
        },
        Msg::Dial {
            conn_id: 7,
            port: 8400,
        },
        Msg::DataConn { conn_id: 7 },
        Msg::Ping,
        Msg::Pong { version: current() },
    ]
}

#[tokio::test]
async fn every_msg_variant_round_trips() {
    let (mut client, mut server) = pair().await;
    for msg in all_messages() {
        client.send(&msg).await.unwrap();
        assert_eq!(server.recv().await.unwrap(), Some(msg.clone()));
        server.send(&msg).await.unwrap();
        assert_eq!(client.recv().await.unwrap(), Some(msg));
    }
}

#[tokio::test]
async fn recv_returns_none_after_peer_closes() {
    let (client, mut server) = pair().await;
    drop(client);
    assert_eq!(server.recv().await.unwrap(), None);
}

#[tokio::test]
async fn minor_mismatch_is_accepted() {
    let (a, b) = duplex(1 << 16);
    let other = Version::new(current().major(), current().minor() + 1);
    let (client, server) = tokio::join!(
        Conn::connect(a, Hello::with_version(other)),
        Conn::accept(b)
    );
    assert!(client.is_ok());
    assert_eq!(server.unwrap().1.version(), other);
}

#[tokio::test]
async fn major_mismatch_fails_on_both_sides_with_both_versions() {
    let (a, b) = duplex(1 << 16);
    let other = Version::new(current().major() + 1, 0);
    let (client, server) = tokio::join!(
        Conn::connect(a, Hello::with_version(other)),
        Conn::accept(b)
    );

    match server.err().unwrap() {
        Error::VersionMismatch { local, remote } => {
            assert_eq!(local, current());
            assert_eq!(remote, other);
        }
        e => panic!("unexpected error: {e}"),
    }
    let err = client.err().unwrap();
    match &err {
        Error::VersionMismatch { local, remote } => {
            assert_eq!(*local, other);
            assert_eq!(*remote, current());
        }
        e => panic!("unexpected error: {e}"),
    }
    let text = err.to_string();
    assert!(text.contains("2.0"), "{text}");
    assert!(text.contains("1.0"), "{text}");
    assert!(text.contains("rowser remote install <host>"), "{text}");
    assert!(text.contains("reinstall"), "{text}");
}

#[tokio::test]
async fn malformed_json_is_an_error() {
    let (a, b) = duplex(1 << 16);
    let (_client, server) = tokio::join!(
        async move {
            let mut a = a;
            a.write_all(b"{not json\n").await.unwrap();
            a
        },
        Conn::accept(b)
    );
    assert!(matches!(server.err().unwrap(), Error::Malformed(_)));
}

#[tokio::test]
async fn malformed_message_after_handshake_is_an_error() {
    let (client, mut server) = pair().await;
    let (mut raw, _) = client.into_raw();
    raw.write_all(b"{\"type\":\"Nope\"}\n").await.unwrap();
    assert!(matches!(server.recv().await, Err(Error::Malformed(_))));
}

#[tokio::test]
async fn over_long_line_is_an_error() {
    let (client, mut server) = pair().await;
    let (mut raw, _) = client.into_raw();
    let writer = tokio::spawn(async move {
        let _ = raw.write_all(&vec![b'a'; TOO_LONG]).await;
        let _ = raw.write_all(b"\n").await;
        raw
    });
    assert!(matches!(server.recv().await, Err(Error::LineTooLong)));
    drop(server);
    let _ = writer.await;
}

#[tokio::test]
async fn recv_after_over_long_line_keeps_failing() {
    let (client, mut server) = pair().await;
    let (mut raw, _) = client.into_raw();
    let writer = tokio::spawn(async move {
        let _ = raw.write_all(&vec![b'a'; TOO_LONG]).await;
        let _ = raw.write_all(b"\n{\"type\":\"Ping\"}\n").await;
        raw
    });
    assert!(matches!(server.recv().await, Err(Error::LineTooLong)));
    assert!(matches!(server.recv().await, Err(Error::LineTooLong)));
    assert!(server.send(&Msg::Ping).await.is_err());
    drop(server);
    let _ = writer.await;
}

#[tokio::test]
async fn into_raw_keeps_bytes_after_data_conn() {
    let (mut client, server) = pair().await;
    client.send(&Msg::DataConn { conn_id: 3 }).await.unwrap();
    let (mut raw, _) = client.into_raw();
    raw.write_all(b"spliced bytes").await.unwrap();
    drop(raw);

    let mut server = server;
    assert_eq!(
        server.recv().await.unwrap(),
        Some(Msg::DataConn { conn_id: 3 })
    );
    let (stream, buffered) = server.into_raw();
    let mut rest = buffered;
    BufReader::new(stream).read_to_end(&mut rest).await.unwrap();
    assert_eq!(rest, b"spliced bytes");
}
