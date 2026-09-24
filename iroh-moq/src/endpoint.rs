//! Binding an endpoint for live media.
//!
//! The QUIC transport configuration every endpoint in this family binds with,
//! and the local-network address lookup a short ticket leans on. Both live here
//! rather than in the media crates so that a relay, which never touches a codec,
//! binds its endpoint the same way a publisher does.

use std::sync::Arc;

use iroh::endpoint::{Builder as EndpointBuilder, QuicTransportConfig};
use noq_proto::congestion::Bbr3Config;
use tracing::{debug, warn};

/// Returns the QUIC transport configuration every iroh-live endpoint binds
/// with.
///
/// BBR3 in place of iroh's default, CUBIC. A media publisher is
/// application-limited: it sends the bitrate the encoder produces into a
/// window sized for whatever the link would take. CUBIC grows that window
/// until something is lost, so on a publisher the window says nothing about
/// the link, and the send-rate estimate the transport derives from it and
/// moq-net carries to every subscriber (`cwnd / rtt`) reads as room to spare
/// on a link that has none. BBR3 sizes its window from the delivery rate it
/// measures, so the same figure tracks the link, which is what the subscriber's
/// rendition choice is made from. Installed on every endpoint rather than on
/// publishers alone because one endpoint publishes and subscribes at once in
/// a call, and there is no second configuration for the other direction.
pub fn transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .congestion_controller_factory(Arc::new(Bbr3Config::default()))
        .build()
}

/// Whether an endpoint answers for itself over mDNS, or only looks others up.
///
/// The distinction matters to a node that nobody is meant to dial: `irl` run
/// with `--no-serve` accepts no sessions, so announcing it on the local network
/// would advertise an endpoint that refuses every connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanPresence {
    /// Announce this endpoint and answer mDNS queries for it, so peers on the
    /// same network can resolve its id without leaving the network.
    Announce,
    /// Resolve other endpoints without publishing anything about this one.
    LookupOnly,
}

impl LanPresence {
    /// Returns the presence an endpoint that accepts sessions when `serve`
    /// holds should take on the local network.
    pub fn serving(serve: bool) -> Self {
        if serve {
            Self::Announce
        } else {
            Self::LookupOnly
        }
    }

    fn announces(self) -> bool {
        matches!(self, Self::Announce)
    }
}

/// Adds mDNS address lookup to `builder`.
///
/// A ticket names an endpoint id and no addresses, which leaves two ways to
/// turn that id into somewhere to send packets. Pkarr and DNS cover the case
/// where both ends have internet. mDNS covers the case where neither does: two
/// laptops on a conference network with no route out still find each other,
/// because the lookup never leaves the link. Between them they do the job the
/// addresses in a ticket used to do, and they do it with addresses that are
/// current rather than however old the ticket is.
///
/// Not fallible: mDNS wants a multicast socket, and a sandbox or a phone
/// without a multicast lock will not give it one. That costs local-network
/// lookup and nothing else, so a failure is logged and the endpoint binds
/// without it.
pub async fn with_mdns(builder: EndpointBuilder, presence: LanPresence) -> EndpointBuilder {
    let options = iroh_mdns_peer_lookup::Options::new()
        .announce(presence.announces())
        .build();
    match iroh_mdns_peer_lookup::lookup(options).await {
        Ok(lookup) => {
            debug!(announce = presence.announces(), "mDNS address lookup ready");
            builder.address_lookup(lookup)
        }
        Err(err) => {
            warn!(
                error = %err,
                "could not start mDNS address lookup; peers on this network will \
                 only be found through pkarr and DNS, which need internet"
            );
            builder
        }
    }
}
