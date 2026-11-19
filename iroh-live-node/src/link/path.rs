//! A summary of one network path of a session, for display.

use std::{fmt, time::Duration};

use iroh::{TransportAddr, endpoint::PathStats};

/// One open network path of a [`Session`](crate::Session), as
/// [`Session::network_paths`](crate::Session::network_paths) reads it.
///
/// The counters run from the path's start. The rates and the loss over an
/// interval are the differences between two readings of the same path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NetworkPath {
    /// The kind of address the path goes to.
    pub kind: NetworkPathKind,
    /// The remote address: `ip:port`, or the host of an iroh relay.
    pub addr: String,
    /// The remote address as iroh holds it.
    pub remote: TransportAddr,
    /// Whether application data goes over this path.
    pub selected: bool,
    /// The path's round trip estimate.
    pub rtt: Duration,
    /// The UDP payload bytes sent on the path.
    pub sent_bytes: u64,
    /// The UDP payload bytes received on the path.
    pub received_bytes: u64,
    /// The QUIC packets sent on the path, without path MTU probes.
    pub sent_packets: u64,
    /// The QUIC packets lost on the path, without path MTU probes.
    ///
    /// The probes are left out because an oversized probe is expected to be
    /// lost.
    pub lost_packets: u64,
    /// The congestion window, in bytes.
    pub cwnd: u64,
    /// The congestion controller's estimate of what the path carries, in bits per second.
    ///
    /// `None` for a controller that makes no estimate.
    pub bandwidth: Option<u64>,
}

impl NetworkPath {
    /// Builds the summary of a path to `remote` from its statistics.
    pub(crate) fn new(remote: &TransportAddr, selected: bool, stats: &PathStats) -> Self {
        let (kind, addr) = match remote {
            TransportAddr::Ip(addr) if addr.is_ipv4() => (NetworkPathKind::Ipv4, addr.to_string()),
            TransportAddr::Ip(addr) => (NetworkPathKind::Ipv6, addr.to_string()),
            TransportAddr::Relay(url) => (
                NetworkPathKind::Relay,
                url.host_str().map_or_else(
                    || url.to_string(),
                    |host| host.trim_end_matches('.').to_string(),
                ),
            ),
            other => (NetworkPathKind::Other, format!("{other:?}")),
        };
        Self {
            kind,
            addr,
            remote: remote.clone(),
            selected,
            rtt: stats.rtt,
            sent_bytes: stats.udp_tx.bytes,
            received_bytes: stats.udp_rx.bytes,
            sent_packets: stats.sent_packets.saturating_sub(stats.sent_plpmtud_probes),
            lost_packets: stats.lost_packets.saturating_sub(stats.lost_plpmtud_probes),
            cwnd: stats.cwnd,
            bandwidth: stats.bandwidth_estimate,
        }
    }
}

/// The kind of address a [`NetworkPath`] goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NetworkPathKind {
    /// A direct path to an IPv4 address.
    Ipv4,
    /// A direct path to an IPv6 address.
    Ipv6,
    /// A path through an iroh relay.
    Relay,
    /// A path over a custom transport.
    Other,
}

impl fmt::Display for NetworkPathKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ipv4 => "IPv4",
            Self::Ipv6 => "IPv6",
            Self::Relay => "relay",
            Self::Other => "other",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> PathStats {
        let mut stats = PathStats::default();
        stats.rtt = Duration::from_millis(30);
        stats.udp_tx.bytes = 5_000;
        stats.udp_rx.bytes = 2_000;
        stats.sent_packets = 12;
        stats.sent_plpmtud_probes = 2;
        stats.lost_packets = 3;
        stats.lost_plpmtud_probes = 1;
        stats.cwnd = 64_000;
        stats.bandwidth_estimate = Some(8_000_000);
        stats
    }

    #[test]
    fn the_counters_leave_out_mtu_probes() {
        let addr = TransportAddr::Ip("192.0.2.1:4433".parse().unwrap());
        let path = NetworkPath::new(&addr, true, &stats());
        assert_eq!(path.kind, NetworkPathKind::Ipv4);
        assert_eq!(path.addr, "192.0.2.1:4433");
        assert!(path.selected);
        assert_eq!(path.rtt, Duration::from_millis(30));
        assert_eq!((path.sent_bytes, path.received_bytes), (5_000, 2_000));
        assert_eq!((path.sent_packets, path.lost_packets), (10, 2));
        assert_eq!(path.cwnd, 64_000);
        assert_eq!(path.bandwidth, Some(8_000_000));
    }

    #[test]
    fn an_ipv6_path_shows_its_address() {
        let addr = TransportAddr::Ip("[2001:db8::1]:4433".parse().unwrap());
        let path = NetworkPath::new(&addr, false, &PathStats::default());
        assert_eq!(path.kind, NetworkPathKind::Ipv6);
        assert_eq!(path.addr, "[2001:db8::1]:4433");
    }

    #[test]
    fn a_relay_path_shows_the_relay_host() {
        let url = "https://euc1-1.relay.n0.iroh.link./".parse().unwrap();
        let path = NetworkPath::new(&TransportAddr::Relay(url), false, &PathStats::default());
        assert_eq!(path.kind, NetworkPathKind::Relay);
        assert_eq!(path.addr, "euc1-1.relay.n0.iroh.link");
        assert_eq!(path.kind.to_string(), "relay");
    }
}
