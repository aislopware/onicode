//! The network interface that holds an address, and binding a socket to it.
//!
//! A socket bound to a tailnet address alone may still be routed out of another interface on
//! macOS (a VPN or a Wi-Fi route that wins the lookup); bound to Tailscale's interface as well
//! (`IP_BOUND_IF`), it sends and takes packets on the tunnel only.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU32;

/// The index of the interface that holds `ip`, if one does.
///
/// # Errors
///
/// What `getifaddrs` returns.
pub fn index_of(ip: IpAddr) -> io::Result<Option<NonZeroU32>> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` writes the head of a list it allocates; it is freed below.
    if unsafe { libc::getifaddrs(&raw mut list) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut found = None;
    let mut at = list;
    while !at.is_null() {
        // SAFETY: `at` is a node of the list `getifaddrs` returned, alive until it is freed.
        let entry = unsafe { &*at };
        // SAFETY: `ifa_addr` is null or points at a sockaddr of the family it names.
        if unsafe { sockaddr_ip(entry.ifa_addr) } == Some(ip) {
            // SAFETY: `ifa_name` is the interface's NUL-terminated name.
            found = NonZeroU32::new(unsafe { libc::if_nametoindex(entry.ifa_name) });
            break;
        }
        at = entry.ifa_next;
    }
    // SAFETY: `list` came from `getifaddrs` and is freed once; no reference into it outlives this.
    unsafe { libc::freeifaddrs(list) };
    Ok(found)
}

/// Every address on this machine's interfaces that lies in the tailnet's ranges: the
/// addresses the system Tailscale client gave this node, read without its `LocalAPI` (iOS has
/// none a third-party app can reach).
///
/// # Errors
///
/// What `getifaddrs` returns.
pub fn tailnet_addrs() -> io::Result<Vec<IpAddr>> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` writes the head of a list it allocates; it is freed below.
    if unsafe { libc::getifaddrs(&raw mut list) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut found = Vec::new();
    let mut at = list;
    while !at.is_null() {
        // SAFETY: `at` is a node of the list `getifaddrs` returned, alive until it is freed.
        let entry = unsafe { &*at };
        // SAFETY: `ifa_addr` is null or points at a sockaddr of the family it names.
        if let Some(ip) = unsafe { sockaddr_ip(entry.ifa_addr) }
            && onicode_net::admission::on_tailnet(ip)
            && !found.contains(&ip)
        {
            found.push(ip);
        }
        at = entry.ifa_next;
    }
    // SAFETY: `list` came from `getifaddrs` and is freed once; no reference into it outlives this.
    unsafe { libc::freeifaddrs(list) };
    // IPv4 first, as the daemon lists a node's addresses.
    found.sort_by_key(IpAddr::is_ipv6);
    Ok(found)
}

/// The address `sa` holds, for IPv4 and IPv6.
///
/// # Safety
///
/// `sa` is null or points at a valid sockaddr whose family says its type.
unsafe fn sockaddr_ip(sa: *const libc::sockaddr) -> Option<IpAddr> {
    if sa.is_null() {
        return None;
    }
    // SAFETY: the caller's contract: `sa` points at a sockaddr.
    let family = i32::from(unsafe { (*sa).sa_family });
    match family {
        libc::AF_INET => {
            // SAFETY: the family says it is a `sockaddr_in`.
            let sin = unsafe { &*sa.cast::<libc::sockaddr_in>() };
            Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                sin.sin_addr.s_addr,
            ))))
        }
        libc::AF_INET6 => {
            // SAFETY: the family says it is a `sockaddr_in6`.
            let sin6 = unsafe { &*sa.cast::<libc::sockaddr_in6>() };
            Some(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)))
        }
        _ => None,
    }
}

/// Binds `socket` to the interface that holds `ip`.
///
/// # Errors
///
/// `NotFound` when no interface holds `ip`; what the OS says to the binding otherwise.
pub fn bind_to_interface(socket: &socket2::Socket, ip: IpAddr) -> io::Result<()> {
    let index = index_of(ip)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("no interface holds {ip}"))
    })?;
    bind_index(socket, ip, index)
}

fn bind_index(socket: &socket2::Socket, ip: IpAddr, index: NonZeroU32) -> io::Result<()> {
    match ip {
        IpAddr::V4(_) => socket.bind_device_by_index_v4(Some(index)),
        IpAddr::V6(_) => socket.bind_device_by_index_v6(Some(index)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loopback is an interface of every machine; an address nothing holds is none.
    #[test]
    fn loopback_has_an_interface_and_a_stranger_has_none() {
        assert!(index_of(IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap().is_some());
        assert!(index_of("192.0.2.77".parse().unwrap()).unwrap().is_none());
    }

    /// Whatever the machine's tailnet addresses are, none of them is outside the tailnet's
    /// ranges, and each is held by an interface.
    #[test]
    fn tailnet_addresses_are_the_tailnets_and_held_here() {
        for ip in tailnet_addrs().unwrap() {
            assert!(onicode_net::admission::on_tailnet(ip), "{ip}");
            assert!(index_of(ip).unwrap().is_some(), "{ip}");
        }
    }
}
