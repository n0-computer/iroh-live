//! Binding an endpoint for live media.
//!
//! [`MediaPreset`] is iroh's N0 preset with a QUIC transport tuned for live
//! media, and [`EndpointOptions`] adds what a preset cannot: a secret key and
//! mDNS, which starts asynchronously. Both live here rather than in the media
//! crates so that a relay, which never touches a codec, binds its endpoint the
//! same way a publisher does.

use std::sync::Arc;

use iroh::{
    Endpoint, SecretKey,
    endpoint::{Builder, QuicTransportConfig, presets},
};
use n0_error::{AnyError, e};
use noq_proto::congestion::Bbr3Config;
use tracing::{debug, info, warn};

use crate::Error;

/// iroh's [`N0`](presets::N0) preset with a QUIC transport tuned for live media.
///
/// Works anywhere iroh takes a preset:
///
/// ```no_run
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let endpoint = iroh::Endpoint::bind(iroh_moq::MediaPreset).await?;
/// # Ok(())
/// # }
/// ```
///
/// The transport runs BBR3 in place of iroh's default, CUBIC. A media publisher
/// is application-limited: it sends the bitrate the encoder produces into a
/// window sized for whatever the link would take. CUBIC grows that window
/// until something is lost, so on a publisher the window says nothing about
/// the link, and the send-rate estimate moq-net carries to every subscriber
/// (`cwnd / rtt`) reads as room to spare on a link that has none. BBR3 sizes its
/// window from the delivery rate it measures, so the same figure tracks the
/// link. Installed on every endpoint because one endpoint publishes and
/// subscribes at once in a call.
#[derive(Debug, Clone, Copy, Default)]
pub struct MediaPreset;

impl presets::Preset for MediaPreset {
    fn apply(self, builder: Builder) -> Builder {
        presets::N0
            .apply(builder)
            .transport_config(transport_config())
    }
}

/// Returns the QUIC transport configuration [`MediaPreset`] installs.
fn transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .congestion_controller_factory(Arc::new(Bbr3Config::default()))
        .build()
}

/// How an endpoint uses mDNS on the local network.
///
/// mDNS is what lets a ticket, which names an endpoint id and no addresses,
/// resolve on a network with no route to the internet: two laptops on a
/// conference network still find each other, because the lookup never leaves
/// the link.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mdns {
    /// Publishes this endpoint's addresses and resolves others'.
    ///
    /// For a node others dial.
    #[default]
    Announce,
    /// Resolves others without publishing this endpoint.
    ///
    /// For a node nobody dials, which would otherwise advertise an endpoint
    /// that refuses every connection.
    Lookup,
    /// Uses no mDNS at all.
    Off,
}

/// A media endpoint with a key and mDNS, for applications that want both.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct EndpointOptions {
    /// The endpoint's identity. `None` generates an ephemeral one.
    pub secret_key: Option<SecretKey>,
    /// How the endpoint uses mDNS.
    pub mdns: Mdns,
}

impl EndpointOptions {
    /// Binds with `key` as the endpoint's identity.
    pub fn with_secret_key(mut self, key: SecretKey) -> Self {
        self.secret_key = Some(key);
        self
    }

    /// Sets how the endpoint uses mDNS.
    pub fn with_mdns(mut self, mdns: Mdns) -> Self {
        self.mdns = mdns;
        self
    }

    /// Returns an endpoint builder with [`MediaPreset`], the key and mDNS applied.
    ///
    /// For a caller that sets more before binding: a relay adds its ALPNs.
    ///
    /// Async because starting mDNS is. Not fallible: mDNS wants a multicast
    /// socket, and a sandbox or a phone without a multicast lock will not give
    /// it one. That costs local-network lookup and nothing else, so a failure is
    /// logged and the builder goes without it. Cancellation safe: dropping the
    /// future drops the lookup it started.
    pub async fn builder(self) -> Builder {
        let mut builder = Endpoint::builder(MediaPreset);
        if let Some(key) = self.secret_key {
            builder = builder.secret_key(key);
        }
        let announce = match self.mdns {
            Mdns::Announce => true,
            Mdns::Lookup => false,
            Mdns::Off => return builder,
        };
        let options = iroh_mdns_peer_lookup::Options::new()
            .announce(announce)
            .build();
        match iroh_mdns_peer_lookup::lookup(options).await {
            Ok(lookup) => {
                debug!(announce, "mDNS address lookup ready");
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

    /// Binds an endpoint with [`MediaPreset`] and these options.
    ///
    /// Cancellation safe: dropping the future binds nothing.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Bind`] if the endpoint's sockets cannot be bound.
    pub async fn bind(self) -> Result<Endpoint, Error> {
        let endpoint = self.builder().await.bind().await.map_err(|err| {
            e!(Error::Bind {
                source: AnyError::from_std(err)
            })
        })?;
        info!(id = %endpoint.id(), "endpoint bound");
        Ok(endpoint)
    }
}
