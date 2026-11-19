//! A summary of one network path of a session, for display.

use std::{fmt, time::Duration};

use iroh::{
    TransportAddr,
    endpoint::{PathId, PathStats},
};

/// One open network path of a [`Session`](crate::Session), as returned by
/// [`Session::network_paths`](crate::Session::network_paths).
///
/// The counters run from the path's start, so rates and loss are the
/// difference between two readings with the same [`id`](Self::id). A path
/// that is closed and reopened gets a new id.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct NetworkPath {
    /// The path's id within the connection, stable while the path is open.
    pub id: PathId,
    /// The remote address as iroh holds it.
    pub remote: TransportAddr,
    /// Whether application data goes over this path.
    pub selected: bool,
    /// The round trip time estimate.
    pub rtt: Duration,
    /// The UDP payload bytes sent on the path, including path MTU probes
    /// and acknowledgement-only packets.
    pub sent_bytes: u64,
    /// The UDP payload bytes received on the path.
    pub received_bytes: u64,
    /// The QUIC packets sent on the path, without path MTU probes.
    pub sent_packets: u64,
    /// The QUIC packets lost on the path, without path MTU probes.
    pub lost_packets: u64,
    /// The congestion window, in bytes.
    pub cwnd: u64,
    /// The congestion controller's bandwidth estimate in bits per second,
    /// if it makes one.
    pub bandwidth: Option<u64>,
}

impl NetworkPath {
    /// Builds the summary of the path `id` to `remote` from its statistics.
    pub(crate) fn new(
        id: PathId,
        remote: &TransportAddr,
        selected: bool,
        stats: &PathStats,
    ) -> Self {
        Self {
            id,
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

    /// Returns the kind of address the path goes to.
    #[must_use]
    pub fn kind(&self) -> NetworkPathKind {
        match &self.remote {
            TransportAddr::Ip(addr) if addr.is_ipv4() => NetworkPathKind::Ipv4,
            TransportAddr::Ip(_) => NetworkPathKind::Ipv6,
            TransportAddr::Relay(_) => NetworkPathKind::Relay,
            _ => NetworkPathKind::Other,
        }
    }

    /// Returns the remote address for display: `ip:port`, or the host of an
    /// iroh relay.
    ///
    /// The format may change; use [`remote`](Self::remote) to act on the
    /// address.
    #[must_use]
    pub fn addr(&self) -> impl fmt::Display + '_ {
        DisplayAddr(&self.remote)
    }
}

/// Shows a [`TransportAddr`] as [`NetworkPath::addr`] describes.
struct DisplayAddr<'a>(&'a TransportAddr);

impl fmt::Display for DisplayAddr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            TransportAddr::Ip(addr) => write!(f, "{addr}"),
            TransportAddr::Relay(url) => match url.host_str() {
                Some(host) => f.write_str(host.trim_end_matches('.')),
                None => write!(f, "{url}"),
            },
            other => write!(f, "{other:?}"),
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
        let path = NetworkPath::new(PathId::ZERO, &addr, true, &stats());
        assert_eq!(path.id, PathId::ZERO);
        assert_eq!(path.kind(), NetworkPathKind::Ipv4);
        assert_eq!(path.addr().to_string(), "192.0.2.1:4433");
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
        let path = NetworkPath::new(PathId::ZERO, &addr, false, &PathStats::default());
        assert_eq!(path.kind(), NetworkPathKind::Ipv6);
        assert_eq!(path.addr().to_string(), "[2001:db8::1]:4433");
    }

    #[test]
    fn a_relay_path_shows_the_relay_host() {
        let url = "https://euc1-1.relay.n0.iroh.link./".parse().unwrap();
        let addr = TransportAddr::Relay(url);
        let path = NetworkPath::new(PathId::ZERO, &addr, false, &PathStats::default());
        assert_eq!(path.kind(), NetworkPathKind::Relay);
        assert_eq!(path.addr().to_string(), "euc1-1.relay.n0.iroh.link");
        assert_eq!(path.kind().to_string(), "relay");
    }
}
