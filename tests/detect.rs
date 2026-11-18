use std::collections::BTreeSet;
use std::path::PathBuf;

use rowser::detect::{callback_ports, Procfs};
use url::Url;

fn no_proc() -> Procfs {
    Procfs::new("/nonexistent-rowser-procfs")
}

fn ports(url: &str) -> Vec<u16> {
    callback_ports(&Url::parse(url).unwrap(), &no_proc(), 1)
        .into_iter()
        .collect()
}

#[test]
fn redirect_uri_on_localhost_gives_its_port() {
    assert_eq!(
        ports("https://auth.example.com/authorize?redirect_uri=http://localhost:8085/cb"),
        vec![8085]
    );
}

#[test]
fn percent_encoded_redirect_uri_is_decoded() {
    assert_eq!(
        ports(
            "https://auth.example.com/a?client_id=x&redirect_uri=http%3A%2F%2F127.0.0.1%3A53682%2F"
        ),
        vec![53682]
    );
}

#[test]
fn redirect_url_and_callback_params_on_ipv6_loopback() {
    assert_eq!(
        ports(
            "https://a.example/?redirect_url=http://[::1]:9001/&callback=http://localhost:9002/x"
        ),
        vec![9001, 9002]
    );
}

#[test]
fn other_param_names_and_remote_hosts_are_ignored() {
    assert!(ports(
        "https://a.example/?next=http://localhost:8085/&redirect_uri=http://example.com:8086/"
    )
    .is_empty());
}

#[test]
fn default_ports_below_1024_are_dropped() {
    assert!(ports("https://a.example/?redirect_uri=http://localhost/cb").is_empty());
    assert!(ports("http://127.0.0.1/").is_empty());
}

#[test]
fn https_default_port_is_dropped_too() {
    assert!(ports("https://localhost/").is_empty());
}

#[test]
fn url_on_loopback_gives_its_own_port() {
    assert_eq!(ports("http://localhost:3000/login"), vec![3000]);
    assert_eq!(ports("http://[::1]:4000/"), vec![4000]);
}

#[test]
fn url_port_and_param_port_are_united_without_duplicates() {
    assert_eq!(
        ports("http://127.0.0.1:8085/?redirect_uri=http://localhost:8085/&callback=http://localhost:8090/"),
        vec![8085, 8090]
    );
}

#[test]
fn remote_url_port_is_ignored() {
    assert!(ports("https://example.com:8443/").is_empty());
}

fn fixture() -> Procfs {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proc");
    Procfs::new(root)
}

#[cfg(target_os = "linux")]
#[test]
fn proc_walk_finds_loopback_listeners_of_caller_and_ancestors() {
    // Fixture: 100 -> 50 -> 1 -> 0; 200 is unrelated.
    // 100 listens on 127.0.0.1:8085, 0.0.0.0:8086 (not loopback), 127.0.0.1:80 (below range),
    // 127.0.0.2:8089, [::ffff:127.0.0.2]:8090 and [::ffff:127.0.0.1]:8091.
    // 50 listens on [::1]:9000 and has an established 127.0.0.1:7000 (not LISTEN).
    // 200 listens on 127.0.0.1:8087. 1 listens on 127.0.0.1:8088.
    let url = Url::parse("https://example.com/").unwrap();
    let got = callback_ports(&url, &fixture(), 100);
    assert_eq!(got, BTreeSet::from([8085, 8088, 8091, 9000]));
}

#[cfg(target_os = "linux")]
#[test]
fn proc_walk_ignores_loopback_addresses_other_than_127_0_0_1() {
    let url = Url::parse("https://example.com/").unwrap();
    let got = callback_ports(&url, &fixture(), 100);
    assert!(!got.contains(&8089), "127.0.0.2 counted: {got:?}");
    assert!(!got.contains(&8090), "::ffff:127.0.0.2 counted: {got:?}");
    assert!(got.contains(&8091), "::ffff:127.0.0.1 missed: {got:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn proc_walk_includes_pid_1_when_it_is_an_ancestor() {
    let url = Url::parse("https://example.com/").unwrap();
    assert!(callback_ports(&url, &fixture(), 100).contains(&8088));
    assert_eq!(callback_ports(&url, &fixture(), 1), BTreeSet::from([8088]));
}

#[cfg(target_os = "linux")]
#[test]
fn proc_walk_unites_with_url_ports() {
    let url = Url::parse(
        "https://a.example/?redirect_uri=http://localhost:8085/&callback=http://localhost:5555/",
    )
    .unwrap();
    let got = callback_ports(&url, &fixture(), 100);
    assert_eq!(got, BTreeSet::from([5555, 8085, 8088, 8091, 9000]));
}

#[cfg(target_os = "linux")]
#[test]
fn real_proc_finds_own_loopback_listener() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = Url::parse("https://example.com/").unwrap();
    let got = callback_ports(&url, &Procfs::default(), std::process::id());
    assert!(got.contains(&port), "{port} not in {got:?}");
}

/// Removes a fake `/proc` dir when the test ends.
#[cfg(target_os = "linux")]
struct TmpDir(PathBuf);

#[cfg(target_os = "linux")]
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fake `/proc` in a fresh temp dir, removed when the returned guard drops. Each process is
/// `(pid, comm, ppid, socket inodes)`; `tcp` and `tcp6` lines are `(hex address:port, inode)`
/// LISTEN entries.
#[cfg(target_os = "linux")]
fn fake_proc(
    name: &str,
    procs: &[(u32, &str, u32, &[u64])],
    tcp: &[(&str, u64)],
    tcp6: &[(&str, u64)],
) -> (Procfs, TmpDir) {
    let root = std::env::temp_dir().join(format!("rowser-proc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let guard = TmpDir(root.clone());
    for (pid, comm, ppid, inodes) in procs {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        std::fs::write(
            dir.join("stat"),
            format!("{pid} ({comm}) S {ppid} {pid} {ppid} 0 0 0 0 0 0 0 0 0 0 0 20 0 1 0 1 0 0\n"),
        )
        .unwrap();
        for (fd, inode) in inodes.iter().enumerate() {
            std::os::unix::fs::symlink(
                format!("socket:[{inode}]"),
                dir.join("fd").join(fd.to_string()),
            )
            .unwrap();
        }
    }
    std::fs::create_dir_all(root.join("net")).unwrap();
    for (file, lines, remote) in [
        ("net/tcp", tcp, "00000000:0000"),
        ("net/tcp6", tcp6, "00000000000000000000000000000000:0000"),
    ] {
        let mut table = String::from("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n");
        for (i, (addr, inode)) in lines.iter().enumerate() {
            table.push_str(&format!(
                "   {i}: {addr} {remote} 0A 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0\n"
            ));
        }
        std::fs::write(root.join(file), table).unwrap();
    }
    (Procfs::new(root), guard)
}

#[cfg(target_os = "linux")]
#[test]
fn proc_walk_reads_comm_with_spaces_and_parentheses() {
    // 40 "a) (b" -> 30 "x) (sshd" (not sshd) -> 20 sshd. Both are walked; sshd stops the walk.
    let (procs, _dir) = fake_proc(
        "comm-parens",
        &[
            (40, "a) (b", 30, &[1]),
            (30, "x) (sshd", 20, &[2]),
            (20, "sshd", 1, &[3]),
        ],
        &[
            ("0100007F:1F95", 1),
            ("0100007F:1F96", 2),
            ("0100007F:08AE", 3),
        ],
        &[],
    );
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(
        callback_ports(&url, &procs, 40),
        BTreeSet::from([8085, 8086])
    );
}

#[cfg(target_os = "linux")]
#[test]
fn proc_walk_stops_at_sshd_listening_on_high_loopback_port() {
    // OpenSSH 9.8+: shell 30 -> sshd-session 20 -> sshd listener 10 -> init 1.
    // sshd listens on 127.0.0.1:2222 and [::1]:2222; the session holds 127.0.0.1:2223;
    // init 127.0.0.1:8088.
    let (procs, _dir) = fake_proc(
        "sshd-new",
        &[
            (30, "bash", 20, &[1]),
            (20, "sshd-session", 10, &[2]),
            (10, "sshd", 1, &[3, 5]),
            (1, "init", 0, &[4]),
        ],
        &[
            ("0100007F:1F95", 1),
            ("0100007F:08AF", 2),
            ("0100007F:08AE", 3),
            ("0100007F:1F98", 4),
        ],
        &[("00000000000000000000000001000000:08AE", 5)],
    );
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(callback_ports(&url, &procs, 30), BTreeSet::from([8085]));
}

#[cfg(target_os = "linux")]
#[test]
fn proc_walk_stops_at_pre_9_8_sshd_session() {
    // Before OpenSSH 9.8 the session process is also named "sshd".
    let (procs, _dir) = fake_proc(
        "sshd-old",
        &[
            (30, "bash", 20, &[1]),
            (20, "sshd", 10, &[2]),
            (10, "sshd", 1, &[3]),
        ],
        &[
            ("0100007F:1F95", 1),
            ("0100007F:08AF", 2),
            ("0100007F:08AE", 3),
        ],
        &[],
    );
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(callback_ports(&url, &procs, 30), BTreeSet::from([8085]));
}

#[test]
fn unknown_caller_pid_yields_url_ports_only() {
    let url = Url::parse("http://localhost:3000/").unwrap();
    assert_eq!(
        callback_ports(&url, &fixture(), 4242),
        BTreeSet::from([3000])
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn proc_walk_yields_nothing_off_linux() {
    let url = Url::parse("https://example.com/").unwrap();
    assert!(callback_ports(&url, &fixture(), 100).is_empty());
}
