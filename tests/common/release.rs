//! A fake GitHub Release for tests: a local HTTP server and SHA256SUMS lines.
//! The integration tests include this file with `#[path]`.

use std::io::{Read, Write};
use std::net::TcpListener;

/// Serves each `(path, body)` for `GET <path>` and 404 for any other path.
/// Returns `http://<addr><prefix>`, the base URL the routes sit under.
pub fn serve_all(prefix: &str, routes: Vec<(&'static str, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let hit = routes
                .iter()
                .find(|(path, _)| req.starts_with(&format!("GET {path} ")));
            let (status, body) = match hit {
                Some((_, body)) => ("200 OK", body.clone()),
                None => ("404 Not Found", Vec::new()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://{addr}{prefix}")
}

/// The `sha256sum` line for `body` named `name`, as the release workflow writes it.
/// Computed with `sha2`, so the tests need no `sha256sum` (macOS has none).
pub fn sums_line(name: &str, body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{hash}  {name}\n")
}
