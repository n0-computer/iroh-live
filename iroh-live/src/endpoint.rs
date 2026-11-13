//! Binding an endpoint: [`MoqPreset`] plus a secret key and mDNS.

use std::{io::Write, path::Path};

use iroh::{Endpoint, SecretKey, endpoint::Builder};
use iroh_live_node::MoqPreset;
use n0_error::e;
use tracing::{debug, info, warn};

use crate::Error;

/// How an endpoint uses mDNS on the local network.
///
/// mDNS lets an endpoint id resolve on a network without internet access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mdns {
    /// Publishes this endpoint's addresses and resolves others'.
    #[default]
    Announce,
    /// Resolves others without publishing this endpoint.
    Lookup,
    /// Uses no mDNS at all.
    Off,
}

/// An endpoint with [`MoqPreset`], a key and mDNS.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct EndpointConfig {
    /// The endpoint's identity. `None` generates an ephemeral one.
    pub secret_key: Option<SecretKey>,
    /// How the endpoint uses mDNS.
    pub mdns: Mdns,
}

impl EndpointConfig {
    /// Returns this config with `secret_key` as the endpoint's identity.
    #[must_use]
    pub fn with_secret_key(mut self, secret_key: SecretKey) -> Self {
        self.secret_key = Some(secret_key);
        self
    }

    /// Returns this config with `mdns` as how the endpoint uses mDNS.
    #[must_use]
    pub fn with_mdns(mut self, mdns: Mdns) -> Self {
        self.mdns = mdns;
        self
    }

    /// Returns the default config with the secret key in `IROH_SECRET`, if set.
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

    /// Applies this config to `builder` and returns it.
    ///
    /// This sets [`MoqPreset`], the key and mDNS, for a caller that builds
    /// its own endpoint. The preset sets the relay mode, the address lookups
    /// and the QUIC transport config with BBR3, so make your own settings
    /// after this call, not before. For another congestion controller, set
    /// `transport_config(Congestion::Cubic.transport_config())` afterwards
    /// (see [`Congestion`](crate::Congestion)).
    ///
    /// If mDNS cannot start, as in a sandbox or on a phone without a multicast
    /// lock, logs a warning and goes without it.
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// use iroh::{RelayMode, endpoint::Builder};
    /// use iroh_live::EndpointConfig;
    ///
    /// let endpoint = EndpointConfig::from_env()?
    ///     .apply(Builder::empty())
    ///     .await
    ///     .relay_mode(RelayMode::Disabled)
    ///     .bind()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn apply(self, builder: Builder) -> Builder {
        let mut builder = builder.preset(MoqPreset);
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
                    "could not start the mDNS address lookup. Peers on this network are \
                     found only through pkarr and DNS, which need internet access."
                );
                builder
            }
        }
    }

    /// Binds an endpoint with [`MoqPreset`] and this config.
    ///
    /// The endpoint accepts no ALPN until a router mounts handlers on it. The
    /// caller owns it: close it, or shut down the router built on it, which
    /// closes it.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Bind`] if the endpoint's sockets cannot be bound.
    pub async fn bind(self) -> Result<Endpoint, Error> {
        let endpoint = self
            .apply(Builder::empty())
            .await
            .bind()
            .await
            .map_err(|source| e!(Error::Bind { source }))?;
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
pub fn load_or_create_secret_key(path: &Path) -> std::io::Result<SecretKey> {
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
        std::env::temp_dir().join(format!("iroh-live-key-{}", SecretKey::generate().public()))
    }

    #[test]
    fn a_stored_key_reads_back() {
        let path = scratch();
        let created = load_or_create_secret_key(&path).expect("creates the key");
        let loaded = load_or_create_secret_key(&path).expect("loads the key");
        std::fs::remove_file(&path).expect("removes the scratch file");
        assert_eq!(created.to_bytes(), loaded.to_bytes());
    }

    #[test]
    fn a_file_that_is_not_a_key_is_refused() {
        let path = scratch();
        std::fs::write(&path, b"nowhere near a key").expect("writes the scratch file");
        let result = load_or_create_secret_key(&path);
        std::fs::remove_file(&path).expect("removes the scratch file");
        assert!(result.is_err());
    }
}
