//! Callback port detection: from the URL itself and, on Linux, from `/proc`.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

use url::{Host, Url};

#[cfg(target_os = "linux")]
mod proc;

/// Query parameters that carry a callback URL.
const CALLBACK_PARAMS: [&str; 3] = ["redirect_uri", "redirect_url", "callback"];

/// Lowest port considered a callback port.
const MIN_PORT: u16 = 1024;

/// Root of a `/proc` filesystem: `/proc` in production, a fixture directory in tests.
#[derive(Debug, Clone)]
pub struct Procfs {
    // Read only by the Linux `/proc` scan; other targets keep it so `Procfs::new` is portable.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    root: PathBuf,
}

impl Procfs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[cfg(target_os = "linux")]
    fn root(&self) -> &std::path::Path {
        &self.root
    }
}

impl Default for Procfs {
    fn default() -> Self {
        Self::new("/proc")
    }
}

/// Callback ports for `url`: loopback ports in its callback query parameters, its own port when
/// it points at loopback, and (Linux) loopback TCP ports that `caller_pid` and its ancestors
/// listen on. Only ports 1024–65535.
pub fn callback_ports(url: &Url, procfs: &Procfs, caller_pid: u32) -> BTreeSet<u16> {
    let mut ports = BTreeSet::new();
    ports.extend(loopback_port(url));
    for (name, value) in url.query_pairs() {
        if CALLBACK_PARAMS.contains(&name.as_ref()) {
            if let Ok(inner) = Url::parse(&value) {
                ports.extend(loopback_port(&inner));
            }
        }
    }
    ports.extend(proc_ports(procfs, caller_pid));
    ports.retain(|p| *p >= MIN_PORT);
    ports
}

fn loopback_port(url: &Url) -> Option<u16> {
    let loopback = match url.host()? {
        Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(a) => a == Ipv4Addr::LOCALHOST,
        Host::Ipv6(a) => a == Ipv6Addr::LOCALHOST,
    };
    if loopback {
        url.port_or_known_default()
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn proc_ports(procfs: &Procfs, caller_pid: u32) -> BTreeSet<u16> {
    proc::listening_ports(procfs.root(), caller_pid)
}

#[cfg(not(target_os = "linux"))]
fn proc_ports(_procfs: &Procfs, _caller_pid: u32) -> BTreeSet<u16> {
    BTreeSet::new()
}
