//! Linux `/proc` walk: loopback LISTEN sockets owned by a process and its ancestors.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

/// TCP state code for LISTEN in `/proc/net/tcp{,6}`.
const TCP_LISTEN: &str = "0A";

pub(super) fn listening_ports(root: &Path, caller_pid: u32) -> BTreeSet<u16> {
    let inodes = tree_socket_inodes(root, caller_pid);
    if inodes.is_empty() {
        return BTreeSet::new();
    }
    let mut ports = BTreeSet::new();
    for table in ["net/tcp", "net/tcp6"] {
        if let Ok(text) = fs::read_to_string(root.join(table)) {
            ports.extend(
                text.lines()
                    .skip(1)
                    .filter_map(parse_listen_line)
                    .filter(|(inode, _)| inodes.contains(inode))
                    .map(|(_, port)| port),
            );
        }
    }
    ports
}

/// Socket inodes held by `pid` and its ancestors. Stops before pid 1 (init is not the caller's).
fn tree_socket_inodes(root: &Path, pid: u32) -> HashSet<u64> {
    let mut inodes = HashSet::new();
    let mut seen = HashSet::new();
    let mut pid = pid;
    while pid > 1 && seen.insert(pid) {
        let dir = root.join(pid.to_string());
        if let Ok(entries) = fs::read_dir(dir.join("fd")) {
            for entry in entries.flatten() {
                if let Ok(target) = fs::read_link(entry.path()) {
                    if let Some(inode) = socket_inode(&target.to_string_lossy()) {
                        inodes.insert(inode);
                    }
                }
            }
        }
        match fs::read_to_string(dir.join("stat"))
            .ok()
            .and_then(|s| parent_pid(&s))
        {
            Some(ppid) => pid = ppid,
            None => break,
        }
    }
    inodes
}

fn socket_inode(target: &str) -> Option<u64> {
    target
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// The ppid field of `/proc/<pid>/stat`. The comm field may hold spaces and parentheses, so
/// parse from the last `)`.
fn parent_pid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// `(inode, port)` of a LISTEN line on a loopback address.
fn parse_listen_line(line: &str) -> Option<(u64, u16)> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.get(3)? != &TCP_LISTEN {
        return None;
    }
    let (addr, port) = fields.get(1)?.split_once(':')?;
    if !is_loopback_hex(addr)? {
        return None;
    }
    let port = u16::from_str_radix(port, 16).ok()?;
    let inode = fields.get(9)?.parse().ok()?;
    Some((inode, port))
}

/// Addresses are 32-bit words in host (little-endian) byte order.
fn is_loopback_hex(addr: &str) -> Option<bool> {
    match addr.len() {
        8 => {
            let word = u32::from_str_radix(addr, 16).ok()?;
            Some(Ipv4Addr::from(word.to_le_bytes()).is_loopback())
        }
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                let word = u32::from_str_radix(&addr[i * 8..i * 8 + 8], 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            let ip = Ipv6Addr::from(bytes);
            Some(ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()))
        }
        _ => None,
    }
}
