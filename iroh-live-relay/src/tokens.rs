//! The relay's token verifier for iroh clients: JWTs and rcan capability chains.

use std::{collections::BTreeMap, fmt, str::FromStr};

use iroh::{EndpointId, PublicKey};
use moq_ext_access::policy::{
    Chain, Decision, PeerRequest, RcanVerifier, Rejection, TokenVerifier,
};
use n0_error::e;
use tracing::debug;

use crate::{Error, iroh_sessions::own_paths, keys::KeyFile};

/// A key whose rcan capability chains the relay accepts, and the root they must stay within.
///
/// The flag form is `KEY[@ROOT]`, as `--rcan-principal` takes it: the key in
/// endpoint id form, and a literal path without `*` or `**`. Without a root,
/// the principal's chains may grant anything on the relay. With one, a
/// chain's capability must be rooted at or below it.
///
/// # Examples
///
/// ```
/// use iroh_live_relay::RcanPrincipal;
///
/// let team = iroh::SecretKey::generate().public();
/// let whole_relay = RcanPrincipal::new(team);
/// let one_space = RcanPrincipal::new(team).with_root("spaces/team");
/// let parsed: RcanPrincipal = format!("{team}@spaces/team").parse()?;
/// assert_eq!(parsed, one_space);
/// assert_eq!(whole_relay.to_string(), team.to_string());
/// # Ok::<_, iroh_live_relay::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcanPrincipal {
    key: PublicKey,
    root: String,
}

impl RcanPrincipal {
    /// Creates a principal whose chains may grant anything on the relay.
    #[must_use]
    pub fn new(key: PublicKey) -> Self {
        Self {
            key,
            root: String::new(),
        }
    }

    /// Returns this principal limited to chains rooted at or below `root`.
    ///
    /// The relay refuses to start with a root that holds a wildcard or is
    /// not a valid path.
    #[must_use]
    pub fn with_root(mut self, root: impl Into<String>) -> Self {
        self.root = root.into();
        self
    }

    /// Returns the principal's public key.
    #[must_use]
    pub fn key(&self) -> PublicKey {
        self.key
    }

    /// Returns the root the principal's chains must stay within, empty for the whole relay.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Returns the root without its outer slashes, or why it is no valid root.
    pub(crate) fn checked_root(&self) -> Result<String, Error> {
        let root = self.root.trim_matches('/');
        let invalid = |why: &str| {
            e!(Error::AccessConfig {
                message: format!("--rcan-principal `{self}`: {why}"),
            })
        };
        if root.contains('*') {
            return Err(invalid("a root is a literal path, without * or **"));
        }
        if root.contains('\\') {
            return Err(invalid("a root holds no `\\`"));
        }
        if !root.is_empty() {
            moq_net::Path::new(root)
                .map_err(|err| invalid(&format!("the root is not a valid path: {err}")))?;
        }
        Ok(root.to_owned())
    }

    /// Reports whether the principal's chains may grant anything on the relay.
    pub(crate) fn at_relay_root(&self) -> bool {
        self.root.trim_matches('/').is_empty()
    }
}

impl fmt::Display for RcanPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.root.as_str() {
            "" => write!(f, "{}", self.key),
            root => write!(f, "{}@{root}", self.key),
        }
    }
}

impl FromStr for RcanPrincipal {
    type Err = Error;

    /// Parses `KEY[@ROOT]`, splitting at the first `@`, which a key never holds.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (key, root) = value.split_once('@').unwrap_or((value, ""));
        let key = key.parse::<PublicKey>().map_err(|err| {
            e!(Error::AccessConfig {
                message: format!(
                    "--rcan-principal `{value}`: the key is not an endpoint id: {err}"
                ),
            })
        })?;
        let principal = Self::new(key).with_root(root);
        principal.checked_root()?;
        Ok(principal)
    }
}

/// The relay's [`TokenVerifier`] for iroh clients: JWTs with a key file, chains with principals.
///
/// A token that starts with `rcan1:` is a capability chain. The relay decodes
/// it once and asks only the principals whose key issued its first link, so
/// a chain from no principal costs no more than its decoding. The first
/// principal that accepts it decides. Any other token is a JWT, which the
/// key file checks. A token of a kind the relay has no verifier for is a
/// rejection, so a client cannot get the anonymous rules by sending one.
///
/// Built by [`Config::iroh_tokens`](crate::Config::iroh_tokens), which
/// applies `--auth-own-paths` and `--allow-unexpiring-tokens`. Wrap it in a
/// [`Verify`](moq_ext_access::policy::Verify) to use it as a policy.
#[derive(Debug, Clone)]
pub struct IrohTokens {
    keys: Option<KeyFile>,
    /// Whether a verified JWT also lets the client publish at its own paths.
    keys_own_paths: bool,
    /// The principals' verifiers, by the principal's key.
    principals: BTreeMap<[u8; 32], Vec<Principal>>,
    /// Whether a token that never expires is accepted.
    allow_unexpiring: bool,
}

/// One principal's verifier, as [`IrohTokens`] holds it.
#[derive(Debug, Clone)]
struct Principal {
    verifier: RcanVerifier,
    /// Whether a chain it accepts also lets the client publish at its own paths.
    own_paths: bool,
}

impl IrohTokens {
    /// Creates a verifier of JWTs with `keys`, and of chains that `principals` issued.
    ///
    /// With `own_paths`, a verified JWT, and a chain from a principal at the
    /// relay root, also lets the client publish at its own paths. A
    /// principal rooted below the relay root never adds them.
    pub(crate) fn new(
        keys: Option<KeyFile>,
        principals: &[RcanPrincipal],
        own_paths: bool,
        allow_unexpiring: bool,
    ) -> Result<Self, Error> {
        let mut by_key: BTreeMap<[u8; 32], Vec<Principal>> = BTreeMap::new();
        for principal in principals {
            let root = principal.checked_root()?;
            by_key
                .entry(*principal.key.as_bytes())
                .or_default()
                .push(Principal {
                    verifier: RcanVerifier::new(principal.key.as_verifying_key(), root),
                    own_paths: own_paths && principal.at_relay_root(),
                });
        }
        Ok(Self {
            keys,
            keys_own_paths: own_paths,
            principals: by_key,
            allow_unexpiring,
        })
    }

    /// Verifies a JWT with the key file, and reports whether it adds the client's own paths.
    fn verify_jwt(
        &self,
        token: &str,
        request: &PeerRequest<EndpointId>,
    ) -> Result<(moq_ext_access::policy::Authorization, bool), Rejection> {
        let keys = self
            .keys
            .as_ref()
            .ok_or_else(|| Rejection::new("this relay verifies no JWTs, only rcan chains"))?;
        Ok((keys.verify(token, request)?, self.keys_own_paths))
    }

    /// Verifies a chain with the principals that issued it, and reports whether it adds the client's own paths.
    fn verify_chain(
        &self,
        token: &str,
        request: &PeerRequest<EndpointId>,
    ) -> Result<(moq_ext_access::policy::Authorization, bool), Rejection> {
        if self.principals.is_empty() {
            return Err(Rejection::new(
                "this relay verifies no rcan chains, only JWTs",
            ));
        }
        let chain: Chain = token.parse()?;
        let candidates = self
            .principals
            .get(chain.principal().as_bytes())
            .ok_or_else(|| Rejection::new("no rcan principal of this relay issued the chain"))?;
        let mut expired = None;
        for principal in candidates {
            match principal.verifier.verify_chain(&chain, request.peer()) {
                Ok(authorization) => return Ok((authorization, principal.own_paths)),
                Err(rejection) => {
                    debug!(
                        reason = rejection.reason(),
                        "an rcan principal rejected the chain"
                    );
                    if matches!(rejection.code(), moq_net::Error::Expired) {
                        expired = Some(rejection);
                    }
                }
            }
        }
        Err(expired.unwrap_or_else(|| Rejection::new("no rcan principal accepts the chain")))
    }
}

impl TokenVerifier<EndpointId> for IrohTokens {
    fn verify(&self, token: &str, request: &PeerRequest<EndpointId>) -> Decision {
        let (mut authorization, add_own_paths) = if Chain::is_chain(token) {
            self.verify_chain(token, request)?
        } else {
            self.verify_jwt(token, request)?
        };
        if authorization.expires.is_none() && !self.allow_unexpiring {
            return Err(Rejection::new(
                "the token never expires, which this relay refuses without --allow-unexpiring-tokens",
            ));
        }
        if add_own_paths {
            authorization
                .grant
                .publish
                .extend(own_paths(*request.peer()).iter().cloned());
        }
        Ok(authorization)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use iroh::SecretKey;
    use iroh_live_node::SessionRequest;
    use moq_ext_access::policy::{Event, Expires, MoqCapability};

    use super::*;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; 32])
    }

    fn request(peer: &SecretKey) -> PeerRequest<EndpointId> {
        PeerRequest::new(peer.public(), SessionRequest::default(), Event::Connect)
    }

    fn hour() -> Expires {
        Expires::valid_for(Duration::from_secs(3600))
    }

    /// Returns a chain from `principal` to `holder` that reads under `root`.
    fn reader_chain(principal: &SecretKey, holder: &SecretKey, root: &str) -> String {
        let capability = MoqCapability::new(root).with_subscribe(moq_net::Pattern::all());
        Chain::issue(
            principal.as_signing_key(),
            holder.public().as_verifying_key(),
            capability,
            hour(),
        )
        .to_token()
    }

    fn tokens(principals: &[RcanPrincipal], own_paths: bool) -> IrohTokens {
        IrohTokens::new(None, principals, own_paths, false).expect("valid principals")
    }

    #[test]
    fn rcan_principals_parse() {
        let team = key(1).public();
        let whole: RcanPrincipal = team.to_string().parse().expect("a key");
        assert_eq!(whole, RcanPrincipal::new(team));
        assert!(whole.at_relay_root());
        let space: RcanPrincipal = format!("{team}@spaces/team").parse().expect("a root");
        assert_eq!(space.root(), "spaces/team");
        assert_eq!(space.to_string(), format!("{team}@spaces/team"));
        assert_eq!(space.to_string().parse::<RcanPrincipal>().unwrap(), space);
        let slashed: RcanPrincipal = format!("{team}@/spaces/team/").parse().expect("slashes");
        assert_eq!(slashed.checked_root().unwrap(), "spaces/team");
        for bad in [
            format!("{team}@spaces/*"),
            format!("{team}@**"),
            format!("{team}@spaces//team"),
            format!("{team}@spaces\\"),
            "not-a-key".to_owned(),
            "not-a-key@spaces".to_owned(),
        ] {
            let err = bad.parse::<RcanPrincipal>().expect_err(&bad);
            assert!(matches!(err, Error::AccessConfig { .. }), "{err}");
        }
    }

    #[test]
    fn iroh_tokens_send_each_kind_to_its_verifier() {
        let (team, device) = (key(1), key(2));
        let verifier = tokens(&[RcanPrincipal::new(team.public())], false);
        let chain = reader_chain(&team, &device, "");
        let grant = verifier
            .verify(&chain, &request(&device))
            .expect("the principal accepts its chain")
            .grant;
        assert!(grant.allows_subscribe("live/someone/cam"));
        let jwt = verifier
            .verify("a.b.c", &request(&device))
            .expect_err("no key file");
        assert!(jwt.reason().contains("no JWTs"), "{jwt}");
        let no_principal = tokens(&[], false);
        let rejection = no_principal
            .verify(&chain, &request(&device))
            .expect_err("no principal");
        assert!(rejection.reason().contains("no rcan chains"), "{rejection}");
    }

    #[test]
    fn a_chain_from_no_principal_is_refused_before_any_verifier() {
        let (team, stranger, device) = (key(1), key(3), key(2));
        let verifier = tokens(&[RcanPrincipal::new(team.public())], false);
        let rejection = verifier
            .verify(&reader_chain(&stranger, &device, ""), &request(&device))
            .expect_err("a stranger's chain");
        assert!(rejection.reason().contains("issued"), "{rejection}");
    }

    #[test]
    fn the_first_principal_that_accepts_decides() {
        let (team, device) = (key(1), key(2));
        let chain = reader_chain(&team, &device, "spaces/b");
        let a = RcanPrincipal::new(team.public()).with_root("spaces/a");
        let b = RcanPrincipal::new(team.public()).with_root("spaces/b");
        for order in [[a.clone(), b.clone()], [b, a]] {
            let grant = tokens(&order, false)
                .verify(&chain, &request(&device))
                .expect("one root covers the chain")
                .grant;
            assert!(grant.allows_subscribe("spaces/b/cam"));
            assert!(!grant.allows_subscribe("spaces/a/cam"));
        }
    }

    #[test]
    fn an_expired_chain_keeps_its_code_across_principals() {
        let (team, device) = (key(1), key(2));
        let capability = MoqCapability::new("spaces/b").with_subscribe(moq_net::Pattern::all());
        let chain = Chain::issue(
            team.as_signing_key(),
            device.public().as_verifying_key(),
            capability,
            Expires::At(1),
        )
        .to_token();
        let principals = [
            RcanPrincipal::new(team.public()).with_root("spaces/a"),
            RcanPrincipal::new(team.public()).with_root("spaces/b"),
        ];
        let rejection = tokens(&principals, false)
            .verify(&chain, &request(&device))
            .expect_err("expired");
        assert!(
            matches!(rejection.code(), moq_net::Error::Expired),
            "{rejection}"
        );
    }

    #[test]
    fn own_paths_follow_the_flag_and_the_principals_root() {
        let (team, device) = (key(1), key(2));
        let own = format!("live/{}/cam", device.public());
        let at_root = reader_chain(&team, &device, "");
        let granted = |verifier: &IrohTokens, chain: &str| {
            verifier
                .verify(chain, &request(&device))
                .expect("accepted")
                .grant
        };
        let off = tokens(&[RcanPrincipal::new(team.public())], false);
        assert!(!granted(&off, &at_root).allows_publish(&own));
        let on = tokens(&[RcanPrincipal::new(team.public())], true);
        let grant = granted(&on, &at_root);
        assert!(grant.allows_publish(&own));
        assert!(grant.allows_publish(&format!("rooms/topic/{}/cam", device.public())));
        assert!(!grant.allows_publish(&format!("live/{}/cam", key(9).public())));
        // A principal below the relay root never adds them.
        let space = tokens(
            &[RcanPrincipal::new(team.public()).with_root("spaces/team")],
            true,
        );
        let chain = reader_chain(&team, &device, "spaces/team");
        assert!(!granted(&space, &chain).allows_publish(&own));
    }

    #[test]
    fn a_token_that_never_expires_is_refused_unless_allowed() {
        let (team, device) = (key(1), key(2));
        let chain = Chain::issue(
            team.as_signing_key(),
            device.public().as_verifying_key(),
            MoqCapability::new("").with_subscribe(moq_net::Pattern::all()),
            Expires::Never,
        )
        .to_token();
        let principals = [RcanPrincipal::new(team.public())];
        let rejection = tokens(&principals, false)
            .verify(&chain, &request(&device))
            .expect_err("never expires");
        assert!(rejection.reason().contains("never expires"), "{rejection}");
        IrohTokens::new(None, &principals, false, true)
            .expect("valid")
            .verify(&chain, &request(&device))
            .expect("allowed");
    }
}
