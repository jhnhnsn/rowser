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
    // Fixture: 100 -> 50 -> 1; 200 is unrelated.
    // 100 listens on 127.0.0.1:8085, 0.0.0.0:8086 (not loopback), 127.0.0.1:80 (below range).
    // 50 listens on [::1]:9000 and has an established 127.0.0.1:7000 (not LISTEN).
    // 200 listens on 127.0.0.1:8087. 1 listens on 127.0.0.1:8088.
    let url = Url::parse("https://example.com/").unwrap();
    let got = callback_ports(&url, &fixture(), 100);
    assert_eq!(got, BTreeSet::from([8085, 9000]));
}

#[cfg(target_os = "linux")]
#[test]
fn proc_walk_unites_with_url_ports() {
    let url = Url::parse(
        "https://a.example/?redirect_uri=http://localhost:8085/&callback=http://localhost:5555/",
    )
    .unwrap();
    let got = callback_ports(&url, &fixture(), 100);
    assert_eq!(got, BTreeSet::from([5555, 8085, 9000]));
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
