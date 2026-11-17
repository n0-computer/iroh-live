//! The JWT keys of iroh clients, read again when their file changes.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use iroh::EndpointId;
use moq_ext_access::policy::{Decision, JwtVerifier, PeerRequest, Rejection, TokenVerifier};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::Error;

/// A [`TokenVerifier`] of JWTs with the keys in a file, read again when the file changes.
///
/// The file holds a JWK or a JWK set, as `--auth-key` takes it. Before each
/// token the verifier compares the file's modification time and size with
/// the ones it last read, as moq-auth's reference policy does for browsers,
/// so a rotated or revoked key takes effect for the next session without a
/// restart. A file that is gone rejects every token until it is back, and the
/// relay closes the sessions its keys admitted. A file that is there but no
/// longer parses, as it is while an editor rewrites it, rejects new tokens
/// but leaves live sessions alone: they keep the keys last read until the
/// file parses again. The relay also checks the file once a second, and
/// decides its live iroh sessions again when it changed.
///
/// Wrap it in a [`Verify`](moq_ext_access::policy::Verify) to use it as a policy.
#[derive(Debug, Clone)]
pub struct KeyFile {
    path: PathBuf,
    /// The keys as last read, with the stamp of the file they were read from.
    cached: Arc<Mutex<Option<Cached>>>,
    /// Counts the changes to the file that the verifier noticed.
    changes: Arc<watch::Sender<u64>>,
}

#[derive(Debug)]
struct Cached {
    stamp: Stamp,
    verifier: JwtVerifier,
    /// The stamp of a later version of the file that did not parse, if the last read failed.
    unparsed: Option<Stamp>,
}

/// What identifies a version of the file: its modification time and size.
type Stamp = (SystemTime, u64);

impl KeyFile {
    /// Reads the keys in `path`, which is read again whenever it changes.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Keys`] if the file holds neither a JWK nor a JWK set.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let stamp = stamp(&path);
        let (keys, _) = crate::load_keys(&path)?;
        let cached = stamp.map(|stamp| Cached {
            stamp,
            verifier: JwtVerifier::new(keys),
            unparsed: None,
        });
        Ok(Self {
            path,
            cached: Arc::new(Mutex::new(cached)),
            changes: Arc::new(watch::Sender::new(0)),
        })
    }

    /// Returns a receiver that sees a new value whenever the verifier notices a change to the file.
    pub(crate) fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Reads the file again if it changed since the verifier last read it.
    pub(crate) fn refresh(&self) {
        // A rejection here is logged by `current`, and the next token gets it too.
        self.current().ok();
    }

    /// Tells the receivers of [`changes`](Self::changes) that the file changed.
    fn changed(&self) {
        self.changes.send_modify(|count| *count += 1);
    }

    /// Returns the verifier of the keys the file holds now.
    fn current(&self) -> Result<JwtVerifier, Rejection> {
        let unreadable = || {
            Rejection::new("the relay cannot read its JWT keys")
                .with_code(moq_net::Error::Unauthorized)
        };
        let mut cached = self.cached.lock().expect("poisoned");
        // Stamped before reading, so a write that races the read is caught at the next token.
        let Some(stamp) = stamp(&self.path) else {
            if cached.take().is_some() {
                warn!(path = %self.path.display(), "the JWT key file is gone, refusing tokens");
                self.changed();
            }
            return Err(unreadable());
        };
        if let Some(cached) = cached.as_ref() {
            if cached.stamp == stamp {
                return Ok(cached.verifier.clone());
            }
            if cached.unparsed == Some(stamp) {
                return Err(unreadable());
            }
        }
        match crate::load_keys(&self.path) {
            Ok((keys, _)) => {
                info!(path = %self.path.display(), "read the changed JWT key file");
                let verifier = JwtVerifier::new(keys);
                *cached = Some(Cached {
                    stamp,
                    verifier: verifier.clone(),
                    unparsed: None,
                });
                self.changed();
                Ok(verifier)
            }
            Err(err) => {
                // A file caught mid-rewrite does not parse for a moment. Live
                // sessions keep the keys last read, so that moment does not
                // close them; new tokens wait until the file parses again.
                // Revoking every token takes removing the file or writing a
                // set without the key.
                warn!(
                    path = %self.path.display(),
                    error = format_args!("{err:#}"),
                    "the JWT key file holds no key, refusing new tokens until it does"
                );
                if let Some(cached) = cached.as_mut() {
                    cached.unparsed = Some(stamp);
                }
                Err(unreadable())
            }
        }
    }
}

impl TokenVerifier<EndpointId> for KeyFile {
    fn verify(&self, token: &str, request: &PeerRequest<EndpointId>) -> Decision {
        self.current()?.verify(token, request)
    }
}

/// Returns the stamp of the file at `path`, or `None` if it cannot be read.
fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Writes `contents` to `path` with a modification time `secs` after the epoch.
    fn write(path: &Path, contents: &str, secs: u64) {
        std::fs::write(path, contents).unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    /// Generates a JWK as `--auth-key` takes it.
    fn jwk() -> String {
        let key = moq_auth::Key::generate(moq_auth::Algorithm::HS256, None).unwrap();
        key.to_str().unwrap()
    }

    #[test]
    fn a_file_that_does_not_parse_keeps_live_sessions() {
        let path = std::env::temp_dir().join(format!(
            "iroh-live-relay-keys-{}.jwk",
            iroh::SecretKey::generate().public()
        ));
        write(&path, &jwk(), 1);
        let keys = KeyFile::load(&path).unwrap();
        let mut changes = keys.changes();

        // Truncated mid-rewrite: new tokens are refused, but nothing changed.
        write(&path, "", 2);
        assert!(keys.current().is_err());
        assert!(!changes.has_changed().unwrap());
        // Asked again for the same version, it still refuses without a change.
        assert!(keys.current().is_err());
        assert!(!changes.has_changed().unwrap());

        // Written in full with another key, the change is signalled.
        write(&path, &jwk(), 3);
        assert!(keys.current().is_ok());
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();

        // Removed, the change is signalled too.
        std::fs::remove_file(&path).unwrap();
        assert!(keys.current().is_err());
        assert!(changes.has_changed().unwrap());
    }
}
