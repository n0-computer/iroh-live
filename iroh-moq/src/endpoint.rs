//! Binding an endpoint for MoQ.
//!
//! [`MoqPreset`] is iroh's N0 preset with a QUIC transport tuned for MoQ, and
//! [`EndpointOptions`] adds what a preset cannot: a secret key and mDNS.

use std::sync::Arc;

use iroh::{
    Endpoint, SecretKey,
    endpoint::{Builder, QuicTransportConfig, presets},
};
use n0_error::{AnyError, e};
use noq_proto::congestion::Bbr3Config;
use tracing::{debug, info, warn};

use crate::Error;

/// iroh's [`N0`](presets::N0) preset with BBR3 congestion control.
///
/// Works anywhere iroh takes a preset:
///
/// ```no_run
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let endpoint = iroh::Endpoint::bind(iroh_moq::MoqPreset).await?;
/// # Ok(())
/// # }
/// ```
///
/// moq-net sends every subscriber the publisher's send-rate estimate,
/// `cwnd / rtt`. A live publisher sends less than the link could take, and
/// under CUBIC, iroh's default, its window grows until something is lost, so
/// the estimate shows room to spare on a full link. BBR3 sizes the window from
/// the delivery rate it measures, so the estimate tracks the link.
#[derive(Debug, Clone, Copy, Default)]
pub struct MoqPreset;

impl presets::Preset for MoqPreset {
    fn apply(self, builder: Builder) -> Builder {
        presets::N0
            .apply(builder)
            .transport_config(transport_config())
    }
}

/// Returns the QUIC transport configuration [`MoqPreset`] installs.
fn transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .congestion_controller_factory(Arc::new(Bbr3Config::default()))
        .build()
}

/// How an endpoint uses mDNS on the local network.
///
/// mDNS lets an endpoint id resolve on a network without internet access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mdns {
    /// Publishes this endpoint's addresses and resolves others'.
    ///
    /// For a node others dial.
    #[default]
    Announce,
    /// Resolves others without publishing this endpoint.
    ///
    /// For a node nobody dials.
    Lookup,
    /// Uses no mDNS at all.
    Off,
}

/// An endpoint with [`MoqPreset`], a key and mDNS.
#[derive(Debug, Clone, Default)]
pub struct EndpointOptions {
    /// The endpoint's identity. `None` generates an ephemeral one.
    pub secret_key: Option<SecretKey>,
    /// How the endpoint uses mDNS.
    pub mdns: Mdns,
}

impl EndpointOptions {
    /// Returns an endpoint builder with [`MoqPreset`], the key and mDNS applied.
    ///
    /// For a caller that sets more before binding. If mDNS cannot start, as in
    /// a sandbox or on a phone without a multicast lock, logs a warning and
    /// goes without it.
    pub async fn builder(self) -> Builder {
        let mut builder = Endpoint::builder(MoqPreset);
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

    /// Binds an endpoint with [`MoqPreset`] and these options.
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
