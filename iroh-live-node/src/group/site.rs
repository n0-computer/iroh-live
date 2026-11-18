//! Site detection: which members of a group share a network egress.
//!
//! Members behind one NAT, or one public address, share a site. A node takes
//! the smallest public IPv4 address its endpoint knows of, which includes the
//! address that QUIC address discovery and port mapping report, and hashes
//! it with the group id. The hash keeps the address out of the hint, and a
//! member of two groups shows two unrelated site ids.

// TODO(groups): sites for IPv6-only networks, by global /56 prefix, and an
// explicit site name the application sets.

use std::net::{IpAddr, Ipv4Addr};

use iroh::{Endpoint, TransportAddr};
use moq_ext_access::GroupId;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};

/// The site of a group member: a hash of its public address, salted with the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct SiteId([u8; 8]);

impl SiteId {
    /// Returns the site id of public address `addr` in group `group`.
    pub(crate) fn new(group: GroupId, addr: Ipv4Addr) -> Self {
        let mut material = [0u8; 36];
        material[..32].copy_from_slice(group.as_bytes());
        material[32..].copy_from_slice(&addr.octets());
        let hash = blake3::derive_key("iroh-live group site v1", &material);
        Self(hash[..8].try_into().expect("a hash is 32 bytes"))
    }
}

/// Returns the smallest public IPv4 address of `addrs`.
///
/// Leaves out private, loopback, link-local, unspecified, broadcast and
/// multicast addresses, and the shared range of carrier-grade NAT,
/// 100.64.0.0/10. Everything else counts as public, the benchmarking range
/// 198.18.0.0/15 that network labs use included.
pub(crate) fn public_v4(addrs: impl IntoIterator<Item = IpAddr>) -> Option<Ipv4Addr> {
    addrs
        .into_iter()
        .filter_map(|addr| match addr {
            IpAddr::V4(addr) => Some(addr),
            IpAddr::V6(addr) => addr.to_ipv4_mapped(),
        })
        .filter(|addr| is_public(*addr))
        .min()
}

/// Reports whether `addr` is a public IPv4 address, as [`public_v4`] counts them.
fn is_public(addr: Ipv4Addr) -> bool {
    let [first, second, ..] = addr.octets();
    let carrier_nat = first == 100 && (second & 0b1100_0000) == 64;
    !(addr.is_private()
        || addr.is_loopback()
        || addr.is_link_local()
        || addr.is_unspecified()
        || addr.is_broadcast()
        || addr.is_multicast()
        || carrier_nat)
}

/// Returns the public IPv4 address of `endpoint`, as it changes.
pub(crate) fn watch_public_v4(
    endpoint: &Endpoint,
) -> impl Watcher<Value = Option<Ipv4Addr>> + use<> {
    #[cfg(test)]
    let fixed = test_hook::public_v4(endpoint.id());
    endpoint.watch_addr().map(move |addr| {
        #[cfg(test)]
        if fixed.is_some() {
            return fixed;
        }
        public_v4(addr.addrs.iter().filter_map(|addr| match addr {
            TransportAddr::Ip(addr) => Some(addr.ip()),
            _ => None,
        }))
    })
}

/// Lets tests on localhost give a node a public address, which localhost lacks.
#[cfg(test)]
pub(crate) mod test_hook {
    use std::{
        collections::HashMap,
        net::Ipv4Addr,
        sync::{Mutex, OnceLock},
    };

    use iroh::EndpointId;

    /// The public addresses tests set, by endpoint id.
    fn addrs() -> &'static Mutex<HashMap<EndpointId, Ipv4Addr>> {
        static ADDRS: OnceLock<Mutex<HashMap<EndpointId, Ipv4Addr>>> = OnceLock::new();
        ADDRS.get_or_init(Default::default)
    }

    /// Makes `addr` the public address of endpoint `id`, for groups created afterwards.
    pub(crate) fn set_public_v4(id: EndpointId, addr: Ipv4Addr) {
        addrs().lock().expect("poisoned").insert(id, addr);
    }

    /// Returns the public address a test set for endpoint `id`.
    pub(super) fn public_v4(id: EndpointId) -> Option<Ipv4Addr> {
        addrs().lock().expect("poisoned").get(&id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(addr: &str) -> IpAddr {
        addr.parse().expect("an address")
    }

    #[test]
    fn non_public_addresses_are_left_out() {
        for addr in [
            "10.0.0.1",
            "172.16.4.2",
            "192.168.1.10",
            "100.64.0.1",
            "100.127.255.254",
            "169.254.3.3",
            "127.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
        ] {
            assert_eq!(public_v4([v4(addr)]), None, "{addr}");
        }
        assert!(public_v4([v4("::1"), v4("fe80::1")]).is_none());
    }

    #[test]
    fn the_smallest_public_address_wins() {
        let addrs = [
            v4("192.168.1.10"),
            v4("203.0.113.9"),
            v4("198.18.0.7"),
            v4("100.128.0.1"),
        ];
        assert_eq!(public_v4(addrs), Some("100.128.0.1".parse().expect("addr")));
        assert_eq!(
            public_v4([v4("::ffff:198.18.0.7")]),
            Some("198.18.0.7".parse().expect("addr")),
            "an IPv4-mapped address counts"
        );
    }

    #[test]
    fn no_public_address_gives_none() {
        assert_eq!(public_v4([]), None);
        assert_eq!(public_v4([v4("10.1.2.3"), v4("2001:db8::1")]), None);
    }

    #[test]
    fn one_address_in_two_groups_gives_two_sites() {
        let addr: Ipv4Addr = "198.18.0.7".parse().expect("addr");
        let one = GroupId::from_bytes([1; 32]);
        let two = GroupId::from_bytes([2; 32]);
        assert_eq!(SiteId::new(one, addr), SiteId::new(one, addr));
        assert_ne!(SiteId::new(one, addr), SiteId::new(two, addr));
        assert_ne!(
            SiteId::new(one, addr),
            SiteId::new(one, "198.18.0.8".parse().expect("addr"))
        );
    }
}
