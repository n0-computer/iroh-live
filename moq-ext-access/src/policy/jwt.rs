//! A token verifier for JWTs, the tokens moq-relay reads.

use std::{fmt, sync::Arc, time::SystemTime};

use moq_auth::KeySet;

use super::{Authorization, Decision, PeerRequest, Rejection, TokenVerifier};
use crate::{Grant, PeerId};

/// Verifies JWTs with a moq-auth [`KeySet`], as moq-relay does.
///
/// A valid token admits the peer with the publish and subscribe patterns its
/// claims grant at the requested path, and its `exp` claim becomes the
/// authorization's expiry. A token whose root does not overlap the requested
/// path is a rejection.
///
/// A JWT is a bearer token: whoever holds it may use it. When the peer has
/// an iroh key, prefer a capability chain bound to that key, which a thief
/// cannot use.
///
/// # Examples
///
/// ```
/// use moq_ext_access::policy::{JwtVerifier, Verify};
///
/// let keys = moq_auth::KeySet::default();
/// let policy = Verify::new(JwtVerifier::new(keys));
/// # drop(policy);
/// ```
#[derive(Clone)]
pub struct JwtVerifier {
    keys: Arc<KeySet>,
}

impl fmt::Debug for JwtVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JwtVerifier")
            .field("keys", &self.keys.keys.len())
            .finish()
    }
}

impl JwtVerifier {
    /// Creates a verifier that accepts tokens signed by a key in `keys`.
    #[must_use]
    pub fn new(keys: KeySet) -> Self {
        Self {
            keys: Arc::new(keys),
        }
    }
}

impl<P: PeerId> TokenVerifier<P> for JwtVerifier {
    fn verify(&self, token: &str, request: &PeerRequest<P>) -> Decision {
        let claims = self.keys.verify(token).map_err(rejection)?;
        let path = request.path();
        let permissions = claims.authorize(path).map_err(rejection)?;
        // The permissions are relative to the requested path, and a grant
        // names absolute paths.
        let absolute = |patterns: moq_auth::Patterns| {
            patterns
                .rooted(path)
                .map_err(|err| Rejection::new(format!("invalid token path: {err}")))
        };
        Ok(Authorization {
            grant: Grant {
                subscribe: absolute(permissions.subscribe)?,
                publish: absolute(permissions.publish)?,
            },
            expires: claims.expires.map(SystemTime::from),
            ..Default::default()
        })
    }
}

/// Returns the rejection for a token moq-auth refuses.
fn rejection(err: moq_auth::Error) -> Rejection {
    let code = match err {
        moq_auth::Error::TokenExpired => moq_net::Error::Expired,
        _ => moq_net::Error::Unauthorized,
    };
    Rejection::new(format!("invalid token: {err}")).with_code(code)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use moq_auth::{Algorithm, Claims, Key};

    use super::*;
    use crate::{SessionRequest, policy::Event};

    fn keys() -> KeySet {
        let key = Key::generate(Algorithm::HS256, None).expect("a key");
        KeySet {
            keys: vec![Arc::new(key)],
        }
    }

    /// Returns claims under `spaces/alice` that let the holder publish under `bob/`.
    fn claims() -> Claims {
        Claims::default()
            .with_root("spaces/alice")
            .with_publish(["bob/**".parse().expect("a pattern")])
            .with_subscribe(["**".parse().expect("a pattern")])
    }

    fn sign(keys: &KeySet, claims: &Claims) -> String {
        keys.sign(claims).expect("signed")
    }

    fn request(path: &str) -> PeerRequest<String> {
        PeerRequest::new(
            "bob".to_owned(),
            SessionRequest::new(path),
            false,
            Event::Connect,
        )
    }

    fn verify(verifier: &JwtVerifier, token: &str, path: &str) -> Decision {
        TokenVerifier::<String>::verify(verifier, token, &request(path))
    }

    #[test]
    fn a_valid_token_admits_with_its_patterns() {
        let keys = keys();
        let verifier = JwtVerifier::new(keys.clone());
        let token = sign(&keys, &claims());
        for path in ["/spaces/alice", "/", "/spaces/alice/bob"] {
            let grant = verify(&verifier, &token, path).expect("admitted").grant;
            assert!(grant.allows_publish("spaces/alice/bob/cam"), "at {path}");
            assert!(!grant.allows_publish("spaces/alice/carol/cam"), "at {path}");
            assert!(grant.allows_subscribe("spaces/alice/bob/cam"), "at {path}");
            assert!(!grant.allows_subscribe("spaces/eve/cam"), "at {path}");
        }
    }

    #[test]
    fn a_token_for_another_root_rejects() {
        let keys = keys();
        let verifier = JwtVerifier::new(keys.clone());
        let token = sign(&keys, &claims());
        let rejection = verify(&verifier, &token, "/spaces/eve").expect_err("rejected");
        assert!(matches!(rejection.code(), moq_net::Error::Unauthorized));
    }

    #[test]
    fn an_expired_token_rejects() {
        let keys = keys();
        let verifier = JwtVerifier::new(keys.clone());
        let mut claims = claims();
        claims.expires = Some(
            (SystemTime::now() - Duration::from_secs(60))
                .try_into()
                .expect("after 1970"),
        );
        let rejection = verify(&verifier, &sign(&keys, &claims), "/").expect_err("rejected");
        assert!(
            matches!(rejection.code(), moq_net::Error::Expired),
            "{rejection}"
        );
    }

    #[test]
    fn a_token_signed_by_another_key_rejects() {
        let verifier = JwtVerifier::new(keys());
        let token = sign(&keys(), &claims());
        let rejection = verify(&verifier, &token, "/").expect_err("rejected");
        assert!(matches!(rejection.code(), moq_net::Error::Unauthorized));
    }

    #[test]
    fn the_exp_claim_becomes_the_expiry() {
        let keys = keys();
        let verifier = JwtVerifier::new(keys.clone());
        let expires = SystemTime::now() + Duration::from_secs(3600);
        let mut claims = claims();
        claims.expires = Some(expires.try_into().expect("after 1970"));
        let authorization = verify(&verifier, &sign(&keys, &claims), "/").expect("admitted");
        let got = authorization.expires.expect("an expiry");
        // `exp` has whole seconds on the wire.
        let gap = got
            .duration_since(expires)
            .unwrap_or_else(|err| err.duration());
        assert!(gap < Duration::from_secs(1), "{got:?} vs {expires:?}");
        let no_exp = verify(&verifier, &sign(&keys, &self::claims()), "/").expect("admitted");
        assert_eq!(no_exp.expires, None);
    }
}
