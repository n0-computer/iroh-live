//! Binding an endpoint for MoQ.
//!
//! [`MoqPreset`] is iroh's N0 preset with a QUIC transport tuned for MoQ, and
//! [`EndpointOptions`] adds what a preset cannot: a secret key and mDNS.
//! [`secret_key_file`] keeps a key across restarts.

use std::{io::Write, path::Path, sync::Arc};

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
    /// Returns the default options with the secret key in `IROH_SECRET`, if set.
    ///
    /// Set `IROH_SECRET` to keep one endpoint id, and so the same tickets,
    /// across restarts.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::SecretKey`] if `IROH_SECRET` is set to something
    /// that is not a secret key.
    pub fn from_env() -> Result<Self, Error> {
        let secret_key = std::env::var_os("IROH_SECRET")
            .map(|key| key.to_string_lossy().parse())
            .transpose()
            .map_err(|source| e!(Error::SecretKey { source }))?;
        Ok(Self {
            secret_key,
            ..Self::default()
        })
    }

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

/// Loads the secret key stored at `path`, or generates one and stores it there.
///
/// The file holds the key's 32 bytes and is created readable by this user
/// only.
///
/// # Errors
///
/// Fails if the file cannot be read or written, or holds something other than
/// a key.
pub fn secret_key_file(path: &Path) -> std::io::Result<SecretKey> {
    match std::fs::read(path) {
        Ok(stored) => {
            let bytes = <[u8; 32]>::try_from(stored.as_slice()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{} does not hold a secret key", path.display()),
                )
            })?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            options.open(path)?.write_all(&key.to_bytes())?;
            info!(path = %path.display(), id = %key.public(), "stored a new secret key");
            Ok(key)
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a path in the temp dir that nothing else uses.
    fn scratch() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("iroh-moq-key-{}", SecretKey::generate().public()))
    }

    #[test]
    fn a_stored_key_reads_back() {
        let path = scratch();
        let created = secret_key_file(&path).expect("creates the key");
        let loaded = secret_key_file(&path).expect("loads the key");
        std::fs::remove_file(&path).expect("removes the scratch file");
        assert_eq!(created.to_bytes(), loaded.to_bytes());
    }

    #[test]
    fn a_file_that_is_not_a_key_is_refused() {
        let path = scratch();
        std::fs::write(&path, b"nowhere near a key").expect("writes the scratch file");
        let result = secret_key_file(&path);
        std::fs::remove_file(&path).expect("removes the scratch file");
        assert!(result.is_err());
    }
}
