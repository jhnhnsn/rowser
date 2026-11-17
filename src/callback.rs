//! Values that the Daemon and the keeper share for Callback forwards.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

/// The loopback addresses of a Callback forward, in dial order: the Daemon
/// binds each one, and the keeper dials them in this order.
pub const LOOPBACK: [IpAddr; 2] = [
    IpAddr::V4(Ipv4Addr::LOCALHOST),
    IpAddr::V6(Ipv6Addr::LOCALHOST),
];

/// How long a Callback forward lives at most.
pub const FORWARD_TIMEOUT: Duration = Duration::from_secs(600);
