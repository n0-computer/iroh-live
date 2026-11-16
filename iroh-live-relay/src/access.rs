//! The relay's access modes, and the policies it decides sessions with.
//!
//! The mode follows from the config: any `--auth-*` or `--rcan-*` flag makes
//! the relay private, and with none it is open. [`Config::policies`] checks
//! the config and builds what [`run`](crate::run) and the tests decide
//! sessions with.

use std::{sync::Arc, time::Duration};

use iroh::EndpointId;
use iroh_live_node::policy::DynPolicy;
use moq_auth::{Permissions, serve};
use moq_ext_access::policy::Verify;
use moq_net::{Pattern, Patterns};
use n0_error::e;

use crate::{
    Config, Error, IrohTokens, KeyFile,
    iroh_sessions::{anonymous_policy, relay_policy},
};

/// Whether a relay admits sessions without a token to everything, or only to its public area.
///
/// The relay infers it from the other settings unless
/// [`Config::access`](crate::Config::access) names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Access {
    /// Every session reads every broadcast.
    ///
    /// Browsers publish names of one segment, and iroh clients publish at
    /// their own paths, the paths that name their endpoint id. The relay
    /// verifies no tokens.
    Open,
    /// Token holders get what their token grants.
    ///
    /// Sessions without a token get the `--auth-public*` patterns, which may
    /// be nothing, and then the relay refuses them.
    Private,
}

/// The paths whose broadcasts name an iroh publisher, which anonymous sessions must not publish at.
const OWNED_PATHS: [&str; 2] = ["live/*/**", "rooms/*/*/**"];

/// The paths a relay pulls tickets at.
const PULLED_PATHS: [&str; 2] = ["live/*/*", "rooms/*/*/*"];

/// The policies a relay decides its sessions with, built by [`Config::policies`].
#[doc(hidden)]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Policies {
    /// The mode, given or inferred.
    pub access: Access,
    /// Decides browser sessions in [`admit`](crate::admit), or `None` when `--auth-url` does.
    pub browsers: Option<BrowserPolicy>,
    /// Decides iroh client sessions.
    pub iroh: Arc<dyn DynPolicy<EndpointId>>,
    /// Decides which iroh clients may pull under [`Access::Authorized`](crate::pull::Access::Authorized).
    pub pull_tokens: Option<Arc<dyn DynPolicy<EndpointId>>>,
    /// The key file iroh clients' JWTs are verified with, watched for changes.
    pub key_file: Option<KeyFile>,
    /// The longest a session admitted with a token lasts.
    pub max_session: Duration,
    /// The lines that describe the access at startup.
    pub summary: Vec<String>,
    /// What the operator should know about the access at startup.
    pub warnings: Vec<String>,
}

/// How [`admit`](crate::admit) decides a browser session.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct BrowserPolicy {
    /// moq-auth's reference policy, or `None` to give every session [`browser_grant`](crate::browser_grant).
    pub(crate) reference: Option<serve::Policy>,
    /// The same keys without a public rule, so only a token passes. For pulls.
    pub(crate) tokens: Option<serve::Policy>,
    /// The longest a session admitted with a token lasts.
    pub(crate) max_session: Option<Duration>,
    /// Whether a token without an expiry is accepted.
    pub(crate) allow_unexpiring: bool,
}

impl BrowserPolicy {
    /// Returns the policy of an open relay: every session gets [`browser_grant`](crate::browser_grant).
    #[must_use]
    pub fn open() -> Self {
        Self::default()
    }
}

impl Config {
    /// Returns the access mode: [`Config::access`](crate::Config::access), or the one the other settings imply.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::AccessConfig`] if the mode contradicts the other
    /// settings: [`Access::Open`] with auth settings, or an
    /// [`iroh_policy`](crate::Config::iroh_policy) without auth settings and
    /// without an explicit mode.
    pub fn access_mode(&self) -> Result<Access, Error> {
        let flags = self.auth_flags();
        match self.access {
            Some(Access::Open) if !flags.is_empty() => Err(config_error(format!(
                "Access::Open takes no auth settings, but {} is set; leave Config::access unset \
                 to make the relay private",
                flags.join(", ")
            ))),
            Some(access) => Ok(access),
            None if flags.is_empty() && self.iroh_policy.is_some() => Err(config_error(
                "Config::iroh_policy is set without auth settings, which would serve browsers \
                 every broadcast; set Config::access to Access::Open to mean that, or to \
                 Access::Private to refuse browsers without a token",
            )),
            None if flags.is_empty() => Ok(Access::Open),
            None => Ok(Access::Private),
        }
    }

    /// Returns the relay's token verifier for iroh clients, or `None` without a key or a principal.
    ///
    /// It verifies JWTs with [`auth_key`](Self::auth_key) and capability
    /// chains with [`rcan_principals`](Self::rcan_principals), and applies
    /// [`auth_own_paths`](Self::auth_own_paths) and
    /// [`allow_unexpiring_tokens`](Self::allow_unexpiring_tokens). An
    /// embedder that sets [`iroh_policy`](Self::iroh_policy) composes it in:
    ///
    /// ```no_run
    /// # fn run() -> Result<(), iroh_live_relay::Error> {
    /// use std::sync::Arc;
    ///
    /// use iroh_live_relay::{Config, relay_policy};
    /// use moq_ext_access::policy::{Policy, Verify};
    ///
    /// let contacts = std::collections::BTreeSet::<iroh::EndpointId>::new();
    /// let mut config = Config::default();
    /// config.auth_key = Some("public.jwk".into());
    /// let tokens = config.iroh_tokens()?.expect("a key is set");
    /// config.iroh_policy = Some(Arc::new(
    ///     Verify::new(tokens)
    ///         .with_fallback(relay_policy().filter(move |r| contacts.contains(r.peer()))),
    /// ));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Keys`] if the key file cannot be read, and with
    /// [`Error::AccessConfig`] if a principal's root is invalid.
    pub fn iroh_tokens(&self) -> Result<Option<IrohTokens>, Error> {
        let keys = self.auth_key.clone().map(KeyFile::load).transpose()?;
        self.tokens_with(keys)
    }

    /// Returns the token verifier over `keys` and the principals, or `None` with neither.
    fn tokens_with(&self, keys: Option<KeyFile>) -> Result<Option<IrohTokens>, Error> {
        if keys.is_none() && self.rcan_principals.is_empty() {
            return Ok(None);
        }
        IrohTokens::new(
            keys,
            &self.rcan_principals,
            self.auth_own_paths,
            self.allow_unexpiring_tokens,
        )
        .map(Some)
    }

    /// Checks the config and returns the policies the relay decides sessions with.
    ///
    /// [`run`](crate::run) calls it before it loads its identity or binds
    /// anything, so a mistake fails without touching the network.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::AccessConfig`] if the settings contradict each
    /// other or would admit no reader, and with [`Error::Keys`] if the key
    /// file cannot be read.
    #[doc(hidden)]
    pub fn policies(&self) -> Result<Policies, Error> {
        let access = self.access_mode()?;
        self.validate(access)?;
        let key_file = self.auth_key.clone().map(KeyFile::load).transpose()?;
        let key_source = self
            .auth_key
            .as_deref()
            .map(crate::load_keys)
            .transpose()?
            .map(|(_, source)| source);
        let tokens = self.tokens_with(key_file.clone())?;
        let anonymous = self.anonymous();

        let browsers = match (self.auth_url.is_some(), access) {
            (true, _) => None,
            (false, Access::Open) => Some(BrowserPolicy::open()),
            (false, Access::Private) => {
                let mut reference = serve::Policy::default();
                reference.keys.clone_from(&key_source);
                reference.public = anonymous.clone();
                let tokens = key_source.clone().map(|keys| {
                    let mut tokens = serve::Policy::default();
                    tokens.keys = Some(keys);
                    tokens
                });
                Some(BrowserPolicy {
                    reference: Some(reference),
                    tokens,
                    max_session: Some(self.max_session),
                    allow_unexpiring: self.allow_unexpiring_tokens,
                })
            }
        };
        let iroh: Arc<dyn DynPolicy<EndpointId>> = match (&self.iroh_policy, access, &tokens) {
            (Some(policy), _, _) => policy.clone(),
            (None, Access::Open, _) => Arc::new(relay_policy()),
            (None, Access::Private, Some(tokens)) => Arc::new(
                Verify::new(tokens.clone())
                    .with_fallback(anonymous_policy(&anonymous, self.auth_public_own_paths)),
            ),
            (None, Access::Private, None) => {
                anonymous_policy(&anonymous, self.auth_public_own_paths)
            }
        };
        // No fallback: a pull needs a token.
        let pull_tokens = tokens
            .clone()
            .map(|tokens| Arc::new(Verify::new(tokens)) as Arc<dyn DynPolicy<EndpointId>>);
        let (summary, warnings) = self.describe(access, tokens.is_some(), &anonymous);
        Ok(Policies {
            access,
            browsers,
            iroh,
            pull_tokens,
            key_file,
            max_session: self.max_session,
            summary,
            warnings,
        })
    }

    /// Returns the names of the auth settings this config sets, which make a relay private.
    fn auth_flags(&self) -> Vec<&'static str> {
        [
            ("--auth-key", self.auth_key.is_some()),
            ("--auth-url", self.auth_url.is_some()),
            ("--auth-public", !self.auth_public.is_empty()),
            (
                "--auth-public-publish",
                !self.auth_public_publish.is_empty(),
            ),
            (
                "--auth-public-subscribe",
                !self.auth_public_subscribe.is_empty(),
            ),
            ("--auth-public-own-paths", self.auth_public_own_paths),
            ("--auth-own-paths", self.auth_own_paths),
            ("--rcan-principal", !self.rcan_principals.is_empty()),
        ]
        .into_iter()
        .filter_map(|(flag, set)| set.then_some(flag))
        .collect()
    }

    /// Returns what a session without a token gets in private mode, rooted at `/`.
    fn anonymous(&self) -> Permissions {
        let publish: Patterns = self
            .auth_public
            .iter()
            .chain(&self.auth_public_publish)
            .cloned()
            .collect();
        let subscribe: Patterns = self
            .auth_public
            .iter()
            .chain(&self.auth_public_subscribe)
            .cloned()
            .collect();
        Permissions::new(publish, subscribe)
    }

    /// Reports whether something verifies tokens locally: a key file or a principal.
    fn verifies_tokens(&self) -> bool {
        self.auth_key.is_some() || !self.rcan_principals.is_empty()
    }

    /// Refuses settings that contradict each other or would admit no reader.
    fn validate(&self, access: Access) -> Result<(), Error> {
        let public = [
            ("--auth-public", &self.auth_public),
            ("--auth-public-publish", &self.auth_public_publish),
            ("--auth-public-subscribe", &self.auth_public_subscribe),
        ];
        for (flag, patterns) in public {
            for pattern in patterns.iter().filter(|pattern| pattern.is_literal()) {
                // A literal at the deepest level is already its own subtree.
                let subtree = Pattern::subtree(pattern.as_str())
                    .map_err(|err| config_error(format!("{flag} `{pattern}`: {err}")))?;
                if subtree != *pattern {
                    return Err(config_error(format!(
                        "{flag} `{pattern}` has no wildcard, so it names exactly one broadcast; \
                         write `{subtree}` for the subtree"
                    )));
                }
            }
        }
        for (flag, patterns) in &public[..2] {
            for pattern in patterns.iter() {
                if OWNED_PATHS
                    .iter()
                    .any(|owned| pattern.overlaps(&owned.parse().expect("a valid pattern")))
                {
                    return Err(config_error(format!(
                        "{flag} `{pattern}` lets sessions without a token publish at live/<id>/... \
                         or rooms/<topic>/<id>/..., where viewers cannot tell them from the real \
                         publisher; use a prefix outside live/ and rooms/, such as demo/**, or \
                         --auth-public-subscribe for read-only access"
                    )));
                }
            }
        }
        for principal in &self.rcan_principals {
            principal.checked_root()?;
        }
        if !self.verifies_tokens() {
            for (flag, set) in [
                ("--auth-own-paths", self.auth_own_paths),
                ("--allow-unexpiring-tokens", self.allow_unexpiring_tokens),
            ] {
                if set {
                    return Err(config_error(format!(
                        "{flag} applies to the tokens the relay verifies, but nothing verifies \
                         them; add --auth-key <PATH> or --rcan-principal <KEY>"
                    )));
                }
            }
        }
        let admits_a_reader = self.verifies_tokens()
            || self.auth_url.is_some()
            || self.iroh_policy.is_some()
            || !self.anonymous().subscribe.is_empty();
        if access == Access::Private && !admits_a_reader {
            let why = if self.auth_public_own_paths {
                "--auth-public-own-paths admits publishers only"
            } else {
                "sessions without a token may read nothing"
            };
            return Err(config_error(format!(
                "this relay would refuse every reader: {why}; add --auth-key <PATH>, \
                 --rcan-principal <KEY> or --auth-public-subscribe <PATTERN>"
            )));
        }
        Ok(())
    }

    /// Returns the startup lines that describe the access, and the warnings.
    fn describe(
        &self,
        access: Access,
        verifies_tokens: bool,
        anonymous: &Permissions,
    ) -> (Vec<String>, Vec<String>) {
        let mut lines = Vec::new();
        let mut warnings = Vec::new();
        let custom = "iroh clients: custom policy, not checked by the relay";
        match access {
            Access::Open => {
                let from = match self.access {
                    Some(_) => "set in Config::access",
                    None => "no --auth-* or --rcan-* flag",
                };
                lines.push(format!("access: open ({from})"));
                lines.push(
                    "  browsers: subscribe everything, publish names of one segment".to_owned(),
                );
                if self.iroh_policy.is_some() {
                    lines.push(format!("  {custom}"));
                } else {
                    lines.push(
                        "  iroh clients: subscribe everything, publish at their own paths"
                            .to_owned(),
                    );
                }
                warnings.push(
                    "access is open: anyone who reaches the relay over WebTransport, or knows its \
                     endpoint id, reads every broadcast on it; pass --auth-key <PATH> to restrict it"
                        .to_owned(),
                );
            }
            Access::Private => {
                let from = match self.access {
                    Some(_) => "set in Config::access".to_owned(),
                    None => format!("from {}", self.auth_flags().join(", ")),
                };
                lines.push(format!("access: private ({from})"));
                let browsers = describe(anonymous);
                let iroh = match (self.auth_public_own_paths, anonymous.is_empty()) {
                    (false, _) => browsers.clone(),
                    (true, true) => "read nothing, publish at their own paths".to_owned(),
                    (true, false) => format!("{browsers}, and publish at their own paths"),
                };
                match (&self.auth_url, &self.iroh_policy) {
                    (Some(url), _) => {
                        lines.push(format!("  browsers: decided by the auth server at {url}"))
                    }
                    (None, Some(_)) => {
                        lines.push(format!("  browsers without a token: {browsers}"));
                    }
                    (None, None) if browsers == iroh => lines.push(format!(
                        "  without a token: browsers and iroh clients {browsers}"
                    )),
                    (None, None) => {
                        lines.push(format!("  browsers without a token: {browsers}"));
                    }
                }
                if self.iroh_policy.is_some() {
                    lines.push(format!("  {custom}"));
                } else if self.auth_url.is_some() || browsers != iroh {
                    lines.push(format!("  iroh clients without a token: {iroh}"));
                }
                if let Some(path) = &self.auth_key {
                    let whom = match self.auth_url {
                        Some(_) => "for iroh clients",
                        None => "for browsers and iroh clients",
                    };
                    lines.push(format!("  JWTs: verified with {}, {whom}", path.display()));
                }
                if !self.rcan_principals.is_empty() {
                    let principals: Vec<String> = self
                        .rcan_principals
                        .iter()
                        .map(|principal| {
                            let root = principal.root().trim_matches('/');
                            let root = if root.is_empty() { "/" } else { root };
                            format!("{} at {root}", principal.key().fmt_short())
                        })
                        .collect();
                    lines.push(format!(
                        "  rcan chains: {} principal{} ({}), iroh clients only",
                        principals.len(),
                        if principals.len() == 1 { "" } else { "s" },
                        principals.join(", ")
                    ));
                }
                if verifies_tokens || self.auth_url.is_some() {
                    let own = if self.auth_own_paths {
                        ", and iroh clients also publish at their own paths"
                    } else {
                        ""
                    };
                    lines.push(format!(
                        "  with a token: the token's grant{own}, for at most {}",
                        format_duration(self.max_session)
                    ));
                }
                if self.auth_url.is_some() && !verifies_tokens && self.iroh_policy.is_none() {
                    warnings.push(
                        "iroh clients without a public pattern are refused, since --auth-url \
                         decides browsers only; add --auth-key <PATH> or --rcan-principal <KEY> \
                         to admit iroh publishers"
                            .to_owned(),
                    );
                }
                let http = self.http_bind.unwrap_or(self.bind);
                let browsers_admitted = self.auth_url.is_none()
                    && (self.auth_key.is_some() || !anonymous.subscribe.is_empty());
                if browsers_admitted && !http.ip().is_loopback() {
                    warnings.push(format!(
                        "the web viewer on {http} is plain HTTP, and so is the certificate pin it \
                         loads: a browser's token is only as safe as the network between it and \
                         the relay; put a TLS proxy in front, or use a trusted network"
                    ));
                }
            }
        }
        lines.push(format!(
            "  pulls: {}",
            self.describe_pulls(access, verifies_tokens)
        ));
        if self.open_pulls {
            let pullable = access == Access::Open
                || anonymous.subscribe.iter().any(|pattern| {
                    PULLED_PATHS
                        .iter()
                        .any(|pulled| pattern.overlaps(&pulled.parse().expect("a valid pattern")))
                });
            if pullable {
                warnings.push(
                    "--open-pulls: sessions without a token may make the relay dial any iroh \
                     endpoint"
                        .to_owned(),
                );
            }
        }
        (lines, warnings)
    }

    /// Returns which sessions may pull a ticket, for the startup line.
    fn describe_pulls(&self, access: Access, verifies_tokens: bool) -> &'static str {
        match (self.open_pulls, access, verifies_tokens) {
            (true, Access::Open, _) => "any session",
            (true, Access::Private, _) => "sessions the relay admits to read the ticket's path",
            (false, _, true) => "sessions whose token reads the ticket's path",
            (false, _, false) => "none; --open-pulls or a token verifier enables them",
        }
    }
}

/// Describes what `permissions` grant, as the startup line says it.
fn describe(permissions: &Permissions) -> String {
    let list = |patterns: &Patterns| {
        if patterns.is_empty() {
            "nothing".to_owned()
        } else {
            patterns
                .iter()
                .map(Pattern::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    if permissions.is_empty() {
        return "refused".to_owned();
    }
    format!(
        "subscribe {}, publish {}",
        list(&permissions.subscribe),
        list(&permissions.publish)
    )
}

/// Formats `duration` in whole hours, minutes or seconds, as `--max-session` takes it.
fn format_duration(duration: Duration) -> String {
    match duration.as_secs() {
        secs if secs > 0 && secs % 3600 == 0 => format!("{}h", secs / 3600),
        secs if secs > 0 && secs % 60 == 0 => format!("{}m", secs / 60),
        _ => humantime::format_duration(duration).to_string(),
    }
}

/// Returns an [`Error::AccessConfig`] with `message`.
fn config_error(message: impl Into<String>) -> Error {
    e!(Error::AccessConfig {
        message: message.into(),
    })
}

#[cfg(test)]
mod tests;
