//! A relay that serves iroh-live broadcasts to browsers.
//!
//! Browsers connect over WebTransport, iroh clients over iroh. What each
//! session may read and publish depends on the relay's access mode.
//!
//! An *open* relay, the default, verifies no tokens. Every session reads
//! every broadcast. An iroh client publishes only at its own paths, the
//! paths that name its endpoint id ([`relay_policy`]), and a browser only at
//! names of one segment. Use it on your own machine or a trusted network.
//!
//! Any `--auth-*` or `--rcan-*` flag makes the relay *private*. A session that
//! presents a valid token gets what the token grants, for at most
//! `--max-session`. A session without a token gets exactly the
//! `--auth-public*` patterns, the relay's public area, and is refused when
//! there are none. A presented token that does not verify is refused, never
//! narrowed to the public area. Browsers present JWTs, which moq-auth's
//! reference policy verifies with `--auth-key`, or an auth server decides
//! under `--auth-url`. iroh clients present a JWT, which a [`KeyFile`] of the
//! same keys verifies, or an rcan capability chain from a
//! `--rcan-principal`, as [`IrohTokens`] does. The key file is read again
//! when it changes, and the live iroh sessions are decided again then.
//!
//! A session that names a broadcast ticket gets it pulled, see [`pull`]: the
//! relay dials the ticket's publisher over iroh. Since that makes the relay
//! dial an endpoint the client chose, only a session whose token grants
//! reading the ticket's path may pull. `--open-pulls` lets any session that
//! the relay admits to read the ticket's path pull, which on an open relay is
//! every session. Either way the pulls are capped in total and per client,
//! and each client's new dials are rate limited ([`pull::Limits`]).
//!
//! The relay prints its access at startup, one line per kind of session.
//!
//! Another CLI can embed the relay by flattening [`Config`] into its arguments.
//!
//! # Examples
//!
//! ```no_run
//! use clap::Parser;
//!
//! #[derive(Parser)]
//! struct Cli {
//!     #[command(flatten)]
//!     relay: iroh_live_relay::Config,
//! }
//!
//! # async fn main_() -> Result<(), iroh_live_relay::Error> {
//! rustls::crypto::aws_lc_rs::default_provider()
//!     .install_default()
//!     .expect("no crypto provider installed yet");
//! iroh_live_relay::run(Cli::parse().relay).await
//! # }
//! ```
//!
//! An embedder that builds the config in code starts from the defaults:
//!
//! ```no_run
//! # async fn main_(team: iroh::PublicKey) -> Result<(), iroh_live_relay::Error> {
//! use iroh_live_relay::{Config, RcanPrincipal};
//!
//! let mut config = Config::default();
//! config.auth_key = Some("public.jwk".into());
//! config.auth_public_subscribe = vec!["demo/**".parse().expect("a pattern")];
//! config.rcan_principals = vec![RcanPrincipal::new(team)];
//! iroh_live_relay::run(config).await
//! # }
//! ```
//!
//! [`run`] returns once a drain started by SIGINT or SIGTERM ends, after
//! closing its listeners. Dropping its future stops the listeners and the
//! accepted connections. The tasks that dial and hold a pull end with the
//! last clone of their [`Pulls`](pull::Pulls): a linger window after
//! the last session that named the ticket, or later while a dial or a resolve
//! is in flight.

#![forbid(unsafe_code)]

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::{extract::Path, response::IntoResponse, routing::get};
use clap::{Args, FromArgMatches};
use include_dir::{Dir, include_dir};
use iroh::{EndpointId, SecretKey, protocol::Router};
use iroh_live_node::{BroadcastTicket, EndpointConfig, Mdns, policy::DynPolicy};
use moq_auth::{KeySet, lease, serve};
use moq_ext_access::policy::CHAIN_PREFIX;
use moq_net::Pattern;
use moq_relay::auth::Admissions;
use n0_error::{AnyError, e};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tracing::{debug, info, warn};

mod access;
mod error;
mod iroh_sessions;
mod keys;
pub mod pull;
mod tokens;

#[doc(hidden)]
pub use self::access::{BrowserPolicy, Policies};
pub use self::{
    access::Access,
    error::Error,
    iroh_sessions::{IrohSessions, browser_grant, relay_policy},
    keys::KeyFile,
    tokens::{IrohTokens, RcanPrincipal},
};

/// The bundled web viewer, embedded at build time.
static WEB_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Configuration for the relay server, as clap arguments.
///
/// Any `--auth-*` or `--rcan-*` flag makes the relay private, see the crate
/// docs. Start from [`Config::default`], which holds the command line's
/// defaults, to build one in code.
#[derive(Args, Debug, Clone)]
#[non_exhaustive]
pub struct Config {
    /// Bind address for WebTransport over QUIC.
    #[arg(long, default_value = "[::]:4443")]
    pub bind: SocketAddr,

    /// Bind address for the web viewer over HTTP.
    ///
    /// Defaults to the --bind address, over TCP.
    #[arg(long)]
    pub http_bind: Option<SocketAddr>,

    /// Verifies JWTs with the keys in this file, and makes the relay private.
    ///
    /// A session with a valid token gets what its claims grant, and one with
    /// an invalid token is refused. A session without a token gets the
    /// --auth-public patterns, or is refused without them. Browsers and iroh
    /// clients both present JWTs. A token grants what it states and no more:
    /// see --auth-own-paths.
    ///
    /// The file holds a JWK or a JWK set, as `moq auth` writes them. The relay
    /// reads it again when it changes. New sessions see the change at once,
    /// live iroh sessions within a second, and browser sessions when their
    /// token or --max-session expires.
    #[arg(long, value_name = "PATH")]
    pub auth_key: Option<PathBuf>,

    /// Asks this auth server about every browser session, as moq-relay does, and makes the relay private.
    ///
    /// The server decides browsers only. iroh clients present tokens that
    /// --auth-key or --rcan-principal verify, and are refused without them.
    #[arg(long, value_name = "URL", conflicts_with_all = ["auth_public", "auth_public_publish", "auth_public_subscribe"])]
    pub auth_url: Option<url::Url>,

    /// Lets sessions without a token publish and subscribe at these patterns, and makes the relay private.
    ///
    /// The relay's public area, for browsers and iroh clients alike. Patterns
    /// are rooted at `/`, as moq-relay reads them. A publish pattern may not
    /// reach `live/<id>/...` or `rooms/<topic>/<id>/...`, whose paths name an
    /// iroh publisher.
    #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
    pub auth_public: Vec<Pattern>,

    /// Lets sessions without a token publish at these patterns, and makes the relay private.
    #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
    pub auth_public_publish: Vec<Pattern>,

    /// Lets sessions without a token subscribe at these patterns, and makes the relay private.
    #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
    pub auth_public_subscribe: Vec<Pattern>,

    /// Lets iroh clients without a token publish at their own paths, and makes the relay private.
    ///
    /// Their own paths are `live/<id>/**` and `rooms/*/<id>/**`, the paths that
    /// name their endpoint id. They read only what the --auth-public patterns
    /// allow. Browsers have no endpoint id, so they gain nothing. Anyone can
    /// make a new endpoint id, so this lets anyone publish.
    #[arg(long)]
    pub auth_public_own_paths: bool,

    /// Lets iroh clients with a verified token also publish at their own paths, and makes the relay private.
    ///
    /// Applies to JWTs and to the chains of an --rcan-principal at the relay
    /// root, not to a principal limited to a root below it. Without it, a
    /// token grants exactly what it states, so a subscribe-only token
    /// publishes nothing.
    #[arg(long)]
    pub auth_own_paths: bool,

    /// Accepts rcan capability chains that this key issued, from iroh clients, and makes the relay private.
    ///
    /// The value is `KEY[@ROOT]`: the principal's public key in endpoint id
    /// form, as `irl key` prints it, and optionally the root its chains must
    /// stay within, a literal path without * or **. Repeat the flag for more
    /// principals. Browsers cannot present chains.
    #[arg(long = "rcan-principal", value_name = "KEY[@ROOT]")]
    pub rcan_principals: Vec<RcanPrincipal>,

    /// Bounds how long a session admitted with a token lasts.
    ///
    /// The session closes at the earlier of its token's expiry and this
    /// bound, and its client redials, presenting its token again.
    #[arg(long, value_name = "DURATION", default_value = "24h", value_parser = humantime::parse_duration)]
    pub max_session: Duration,

    /// Accepts tokens that never expire: JWTs without `exp`, and chains whose links never expire.
    ///
    /// A private relay refuses them by default, since --max-session is then
    /// the only bound on a leaked token.
    #[arg(long)]
    pub allow_unexpiring_tokens: bool,

    /// Lets every session the relay admits to read a ticket's path pull it.
    ///
    /// A session that names a broadcast ticket makes the relay dial the
    /// ticket's iroh endpoint. Without this flag, only a session whose token
    /// grants reading the ticket's path may do that. With it, any session
    /// the relay admits to read the path may: every session on an open relay,
    /// and on a private one also the sessions that the --auth-public
    /// patterns let read it. Meant for local and development use, or for a
    /// private relay whose public area holds no pullable paths.
    #[arg(long)]
    pub open_pulls: bool,

    /// Bounds the pulls the relay holds and starts.
    ///
    /// Defaults to [`pull::Limits::default`].
    #[arg(skip)]
    pub pull_limits: pull::Limits,

    /// The access mode, or `None` to infer it from the other settings.
    ///
    /// Inferred, any auth setting means [`Access::Private`] and none means
    /// [`Access::Open`]. [`Access::Open`] with an auth setting is refused, and
    /// so is an [`iroh_policy`](Self::iroh_policy) without auth settings,
    /// unless this names the mode.
    #[arg(skip)]
    pub access: Option<Access>,

    /// Decides the sessions of iroh clients, instead of the relay's own rules.
    ///
    /// It replaces the key file, the principals and the public patterns for
    /// iroh sessions, so it must refuse the clients it should. Compose it
    /// from [`Config::iroh_tokens`] to keep the relay's token verification.
    /// The key file and the principals still decide pulls.
    #[arg(skip)]
    pub iroh_policy: Option<Arc<dyn DynPolicy<EndpointId>>>,
}

impl Default for Config {
    /// Returns the config of an empty command line: an open relay on port 4443.
    fn default() -> Self {
        let command = <Self as Args>::augment_args(clap::Command::new("relay"));
        let matches = command
            .try_get_matches_from(["relay"])
            .expect("the defaults parse");
        Self::from_arg_matches(&matches).expect("the defaults parse")
    }
}

/// Runs the relay server until a shutdown signal or an error.
///
/// The first SIGINT or SIGTERM drains the browser sessions, and a second one
/// ends the relay at once. Install a rustls crypto provider first, as the
/// crate example does. The relay prints its endpoint id, its access and the
/// addresses it listens at to stdout, for an operator and for the browser
/// tests.
///
/// # Errors
///
/// Fails with [`Error::AccessConfig`] if the access settings contradict
/// each other, before it binds anything. Fails with [`Error::NoDataDir`] or
/// [`Error::Identity`] if the relay's identity cannot be loaded,
/// [`Error::Bind`] if the iroh endpoint cannot bind, [`Error::Keys`] if the
/// JWT keys cannot be read, and [`Error::Relay`] if the moq relay cannot
/// start or one of its listeners fails.
pub async fn run(config: Config) -> Result<(), Error> {
    let policies = config.policies()?;
    let secret_key = secret_key()?;
    // mDNS finds a ticket's publisher on this machine or LAN at once, where
    // pkarr and DNS take seconds and need internet. `Announce`, since clients
    // reach the relay by endpoint id.
    let iroh_endpoint = EndpointConfig::default()
        .with_secret_key(secret_key)
        .with_mdns(Mdns::Announce)
        .bind()
        .await
        .map_err(|source| {
            e!(Error::Bind {
                source: Arc::new(source)
            })
        })?;
    info!(endpoint_id = %iroh_endpoint.id(), "iroh endpoint bound");
    println!("iroh endpoint: {}", iroh_endpoint.id());
    for line in &policies.summary {
        println!("{line}");
    }
    info!(access = ?policies.access, summary = %policies.summary.join("\n"), "access");
    let mut warnings = policies.warnings.clone();
    if config
        .rcan_principals
        .iter()
        .any(|principal| principal.key() == iroh_endpoint.id())
    {
        warnings.push(
            "an --rcan-principal is the relay's own endpoint id, whose secret key sits in the \
             relay's data directory; use a key kept apart from any node"
                .to_owned(),
        );
    }
    for warning in &warnings {
        warn!("{warning}");
        println!("warning: {warning}");
    }

    let mut relay_config = moq_relay::Config::default();
    // The public patterns stay with the relay's own `admit`, which applies
    // them to browsers and iroh clients alike.
    relay_config.auth.url = config.auth_url.clone();
    // HTTP shares the QUIC port unless told otherwise. On port 0 moq-relay
    // would draw a separate port for each, and the page, which dials the port
    // it was served from, would find no QUIC listener there.
    let (quic_bind, http_bind) = match config.http_bind {
        Some(http_bind) => (config.bind, http_bind),
        None => {
            let bind = shared_port(config.bind).map_err(|err| {
                relay_error(
                    anyhow::Error::new(err).context("no port is free for both QUIC and HTTP"),
                )
            })?;
            (bind, bind)
        }
    };
    relay_config.listen.bind = Some(moq_tokio::listen::Bind::Addr(quic_bind));
    // Self-signed TLS. No ACME yet.
    relay_config.listen.tls.generate = vec!["localhost".to_string()];
    relay_config.web.http.listen = Some(http_bind);
    // The viewer serves `/` and every other path, which the WebSocket
    // fallback would take.
    relay_config.web.ws = false;
    let mut relay = moq_relay::Relay::load(relay_config)
        .await
        .map_err(relay_error)?;
    let access = if config.open_pulls {
        pull::Access::Open
    } else {
        pull::Access::Authorized
    };
    let pulls = pull::Pulls::new(iroh_endpoint.clone(), relay.cluster().clone())
        .with_access(access)
        .with_limits(config.pull_limits);
    // Without an auth server, `admit` decides every browser session.
    let _admit_task =
        match (relay.admissions(), policies.browsers.clone()) {
            (Some(admissions), Some(browsers)) => Some(AbortOnDropHandle::new(tokio::spawn(
                admit(admissions, Some(pulls.clone()), browsers),
            ))),
            _ => {
                info!("the auth server decides browser sessions, which pull no tickets");
                None
            }
        };
    // Not moq-relay's iroh listener: `IrohSessions` accepts iroh clients,
    // since it knows who they are.
    let iroh_sessions =
        IrohSessions::new(relay.cluster().clone(), Some(pulls)).with_policies(&policies);
    let iroh_router = iroh_sessions.mount(Router::builder(iroh_endpoint)).spawn();

    let quic_port = relay.quic_addr().map_err(relay_error)?.port();
    let http_port = relay
        .web_addrs()
        .http
        .ok_or_else(|| relay_error(anyhow::anyhow!("the HTTP listener is not bound")))?
        .port();
    // The e2e test fixtures parse this line.
    println!("http port: {http_port}");
    // Plain `http`: the page pins the QUIC certificate's fingerprint from
    // `/certificate.sha256`, so the browser never prompts.
    println!("iroh-live relay listening at http://localhost:{http_port}");
    if quic_port == http_port {
        println!("  WebTransport on UDP {quic_port}; the page above connects to it for you");
    } else {
        println!("  WebTransport on UDP {quic_port}, web viewer on TCP {http_port}");
    }
    println!("  needs a browser with WebTransport: Chromium, or Firefox 153+");

    // moq-relay's routes serve `/certificate.sha256` and apply CORS to GETs.
    let routes = relay
        .web()
        .routes()
        .route("/", get(|| async { serve_embedded_file("index.html") }))
        .route(
            "/{*path}",
            get(|Path(path): Path<String>| async move { serve_embedded_file(&path) }),
        );
    let result = relay.with_web(routes).run().await.map_err(relay_error);
    if let Err(err) = iroh_router.shutdown().await {
        warn!(error = %err, "the iroh router did not shut down cleanly");
    }
    result
}

/// Returns `bind` with a port that is free over both UDP and TCP, when `bind` asks for any port.
///
/// The probe sockets close before moq-relay binds the port, so another
/// process can take it in between, and the relay then fails to start rather
/// than serve QUIC and HTTP on different ports.
fn shared_port(bind: SocketAddr) -> std::io::Result<SocketAddr> {
    if bind.port() != 0 {
        return Ok(bind);
    }
    let mut last_err = None;
    for _ in 0..16 {
        let udp = std::net::UdpSocket::bind(bind)?;
        let addr = udp.local_addr()?;
        match std::net::TcpListener::bind(addr) {
            Ok(_) => return Ok(addr),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::ErrorKind::AddrInUse.into()))
}

/// Wraps an error of moq-relay, which reports anyhow errors.
fn relay_error(err: anyhow::Error) -> Error {
    e!(Error::Relay {
        source: Arc::new(AnyError::from_anyhow(err)),
    })
}

/// Answers the relay's admissions until the relay drops its auth.
///
/// `policy` decides each session: on an open relay every session gets
/// [`browser_grant`], and on a private one moq-auth's reference policy
/// decides with the relay's keys and public patterns. The same admissions
/// cover WebTransport sessions and moq-relay's HTTP routes, `/announced` and
/// `/fetch`, so both get the same rules.
///
/// A session whose path names a ticket reads the ticket's broadcast at that
/// name, and is decided at the broadcast's path, so its token must reach that
/// path. With `pulls` set it holds a pull of the broadcast while it runs, if
/// the pulls' [`Access`](pull::Access) lets it pull and their
/// [`Limits`](pull::Limits) have room. A session that may not pull is refused.
///
/// [`run`] calls it with the policy from [`Config::policies`]. It is public
/// for a binary that embeds the relay with a moq-relay of its own, and for
/// the tests.
#[doc(hidden)]
pub async fn admit(mut admissions: Admissions, pulls: Option<pull::Pulls>, policy: BrowserPolicy) {
    let mut held = JoinSet::new();
    loop {
        let admission = tokio::select! {
            admission = admissions.next() => match admission {
                Some(admission) => admission,
                None => break,
            },
            Some(_) = held.join_next(), if !held.is_empty() => continue,
        };
        let name = iroh_sessions::requested_name(&admission.request.path);
        let ticket = name
            .as_deref()
            .and_then(|name| name.parse::<BroadcastTicket>().ok());
        // A ticket's session reads the ticket's broadcast at its root, so it
        // is decided where that broadcast lives.
        let decided_at = ticket.as_ref().map(|ticket| format!("/{}", ticket.path()));
        let grant = browser_session_grant(&admission, &policy, decided_at.as_deref()).await;
        let mut grant = match grant {
            Ok(grant) => grant,
            Err(err) => {
                info!(path = %admission.request.path, error = %err, "refusing a browser session");
                admission.refuse(err);
                continue;
            }
        };
        let Some((ticket, name)) = ticket.zip(name) else {
            debug!(path = %admission.request.path, "admitting a session");
            admission.grant(lease::Consumer::fixed(grant));
            continue;
        };
        // Whether the session may read the ticket's broadcast, which is the
        // grant's root since it was decided at the broadcast's path.
        let reads_ticket = grant.subscribe.path.matches("");
        // The session's root is the ticket it dialed, so this mounts the
        // broadcast at that name.
        grant
            .mounts
            .insert(String::new(), ticket.path().to_string());
        debug!(path = %admission.request.path, "admitting a ticket's session");
        // An attached admission verifies a token for a session already held.
        let Some(pulls) = pulls.as_ref().filter(|_| !admission.attached()) else {
            admission.grant(lease::Consumer::fixed(grant));
            continue;
        };
        let request = &admission.request;
        let pull = match may_pull(pulls, &policy, reads_ticket, request, &ticket).await {
            Ok(()) => pulls.start(pull::Client::from_addr(request.remote), &name, &ticket),
            Err(err) => Err(err),
        };
        let pull = match pull {
            Ok(pull) => hold_pull(pull, name),
            Err(err) => {
                info!(path = %request.path, error = %err, "refusing a browser session's pull");
                admission.refuse(moq_relay::auth::Error::Forbidden(err.to_string()));
                continue;
            }
        };
        // The lease ends with the session, and the pull's claim with it.
        let (lease, consumer) = lease::Producer::new(grant);
        admission.grant(consumer);
        held.spawn(async move {
            let _pull = pull;
            lease.closed().await;
        });
    }
}

/// Checks that a browser session may pull `ticket`, under the pulls' [`Access`](pull::Access).
///
/// Under [`Access::Open`](pull::Access::Open) the session must be admitted to
/// read the ticket's path, which `reads_ticket` says. Under
/// [`Access::Authorized`](pull::Access::Authorized) the session's token,
/// verified without the public patterns, must grant reading the path.
async fn may_pull(
    pulls: &pull::Pulls,
    policy: &BrowserPolicy,
    reads_ticket: bool,
    request: &moq_auth::Request,
    ticket: &BroadcastTicket,
) -> Result<(), Error> {
    let denied = || {
        e!(Error::PullDenied {
            path: ticket.path().clone()
        })
    };
    if pulls.access() == pull::Access::Open {
        return if reads_ticket { Ok(()) } else { Err(denied()) };
    }
    let tokens = policy.tokens.as_ref().ok_or_else(denied)?;
    let mut at_ticket = request.clone();
    at_ticket.path = format!("/{}", ticket.path());
    // The grant is rooted at the ticket's path, so the broadcast is its root.
    match tokens.decide(&at_ticket).await {
        Ok(grant) if grant.subscribe.path.matches("") => Ok(()),
        _ => Err(denied()),
    }
}

/// Returns the grant of a browser session, as `policy` decides it.
///
/// The session is decided at `path` if set, else at the path it dialed. An
/// attached admission is decided on the credential it presented in band.
async fn browser_session_grant(
    admission: &moq_relay::auth::Admission,
    policy: &BrowserPolicy,
    path: Option<&str>,
) -> Result<moq_auth::Grant, moq_relay::auth::Error> {
    let request = &admission.request;
    let forbidden = |reason: String| moq_relay::auth::Error::Forbidden(reason);
    let Some(reference) = &policy.reference else {
        return browser_grant(path.unwrap_or(&request.path))
            .map_err(|err| forbidden(err.to_string()));
    };
    let mut request = request.clone();
    if let Some(path) = path {
        path.clone_into(&mut request.path);
    }
    if let Some(token) = request.auth.clone().filter(|_| admission.attached()) {
        request.token = Some(moq_auth::Token {
            kind: moq_auth::Token::OUT_OF_BAND,
            value: token.into_bytes(),
        });
        request.query = None;
    }
    let token = presented_token(&request);
    if token
        .as_deref()
        .is_some_and(|token| token.starts_with(CHAIN_PREFIX))
    {
        return Err(forbidden(
            "rcan chains are for iroh clients; a browser presents a JWT".to_owned(),
        ));
    }
    let mut grant = reference
        .decide(&request)
        .await
        .map_err(|denial| forbidden(denial.to_string()))?;
    if token.is_none() {
        return Ok(grant);
    }
    if grant.expires.is_none() && !policy.allow_unexpiring {
        return Err(forbidden(
            "the token never expires, which this relay refuses without --allow-unexpiring-tokens"
                .to_owned(),
        ));
    }
    if let Some(bound) = policy.max_session {
        let latest = std::time::SystemTime::now() + bound;
        let expires = grant
            .expires
            .map(std::time::SystemTime::from)
            .map_or(latest, |expires| expires.min(latest));
        grant.expires = Some(
            expires
                .try_into()
                .map_err(|_| forbidden("the session's expiry is out of range".to_owned()))?,
        );
    }
    Ok(grant)
}

/// Returns the token a browser session presents: in its SETUP, else as `?jwt=` in its URL.
fn presented_token(request: &moq_auth::Request) -> Option<String> {
    if let Some(token) = &request.token {
        return String::from_utf8(token.value.clone()).ok();
    }
    let query = request.query.as_deref()?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, value)| key == "jwt" && !value.is_empty())
        .map(|(_, value)| value.into_owned())
}

/// Loads the JWT keys from a file that holds a JWK set or a single JWK.
///
/// Returns the keys, and the file as moq-auth's policy reads it again when it changes.
fn load_keys(path: &std::path::Path) -> Result<(KeySet, serve::Keys), Error> {
    if let Ok(keys) = KeySet::from_file(path) {
        return Ok((keys, serve::Keys::Set(path.to_owned())));
    }
    let key = moq_auth::Key::from_file(path).map_err(|source| {
        e!(Error::Keys {
            path: path.to_owned(),
            source: Arc::new(source),
        })
    })?;
    let keys = KeySet {
        keys: vec![Arc::new(key)],
    };
    Ok((keys, serve::Keys::File(path.to_owned())))
}

/// Holds `pull` alongside the session that asked for `name`, and logs a failed dial.
///
/// Runs alongside the session, so a session that names an unreachable ticket
/// still starts. Dropping the handle releases the session's hold on the pull.
pub(crate) fn hold_pull(
    pull: pull::PullGuard,
    name: String,
) -> AbortOnDropHandle<Option<pull::PullGuard>> {
    AbortOnDropHandle::new(tokio::spawn(async move {
        match pull.connected().await {
            Ok(()) => Some(pull),
            Err(err) => {
                warn!(%name, error = format_args!("{err:#}"), "pull failed for the ticket in the URL");
                None
            }
        }
    }))
}

/// Loads the relay's iroh identity, generating and storing one on first run.
///
/// Every ticket through this relay names its endpoint id, so a file that does
/// not hold a key is an error, and never a reason to make a new identity.
fn secret_key() -> Result<SecretKey, Error> {
    let dir = match std::env::var_os("IROH_LIVE_RELAY_DATA") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::data_dir()
            .ok_or_else(|| e!(Error::NoDataDir))?
            .join("iroh-live-relay"),
    };
    let path = dir.join("iroh_secret_key");
    std::fs::create_dir_all(&dir)
        .and_then(|()| iroh_live_node::load_or_create_secret_key(&path))
        .map_err(|source| {
            e!(Error::Identity {
                path,
                source: Arc::new(source)
            })
        })
}

/// Serves a file of the embedded web viewer, or a 404.
fn serve_embedded_file(path: &str) -> axum::response::Response {
    let mime = mime_from_path(path);
    match WEB_DIR.get_file(path) {
        Some(file) => (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, mime)],
            file.contents(),
        )
            .into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

/// Returns the content type for a file name, from its extension.
fn mime_from_path(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        relay: Config,
    }

    fn parse(args: &[&str]) -> Result<Config, clap::Error> {
        Cli::try_parse_from(std::iter::once("relay").chain(args.iter().copied()))
            .map(|cli| cli.relay)
    }

    /// Any port resolves to one that UDP and TCP can both bind, and a fixed port stays.
    #[test]
    fn any_port_resolves_to_one_free_for_quic_and_http() {
        let bind = shared_port("127.0.0.1:0".parse().unwrap()).expect("a free port");
        assert_ne!(bind.port(), 0);
        let _udp = std::net::UdpSocket::bind(bind).expect("UDP binds the port");
        let _tcp = std::net::TcpListener::bind(bind).expect("TCP binds the port");
        let fixed: SocketAddr = "127.0.0.1:4443".parse().unwrap();
        assert_eq!(shared_port(fixed).unwrap(), fixed);
    }

    #[test]
    fn the_auth_flags_parse() {
        Cli::command().debug_assert();
        let config = parse(&[]).expect("no flags");
        assert!(config.auth_key.is_none() && config.auth_public.is_empty());
        let config = parse(&["--auth-key", "keys.jwk"]).expect("a key");
        assert_eq!(
            config.auth_key.as_deref(),
            Some(std::path::Path::new("keys.jwk"))
        );
        let config = parse(&["--auth-public", "anon/**,demo/**"]).expect("patterns");
        assert_eq!(config.auth_public.len(), 2);
        let config = parse(&["--max-session", "90m"]).expect("a duration");
        assert_eq!(config.max_session, Duration::from_secs(90 * 60));
        assert_eq!(
            parse(&[]).unwrap().max_session,
            Duration::from_secs(24 * 3600)
        );
    }

    #[test]
    fn the_default_config_is_the_empty_command_line() {
        let config = Config::default();
        assert_eq!(config.bind, "[::]:4443".parse().unwrap());
        assert!(config.auth_key.is_none() && config.access.is_none());
        assert_eq!(config.max_session, parse(&[]).unwrap().max_session);
    }

    #[test]
    fn a_key_and_another_auth_source_combine_but_a_url_and_patterns_conflict() {
        parse(&["--auth-key", "k", "--auth-url", "https://auth.example"]).expect("key and url");
        parse(&["--auth-key", "k", "--auth-public", "anon/**"]).expect("key and patterns");
        assert!(
            parse(&[
                "--auth-url",
                "https://auth.example",
                "--auth-public",
                "anon/**"
            ])
            .is_err()
        );
    }

    /// Returns a path in the temp dir that nothing else uses.
    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!(
            "iroh-live-relay-keys-{}",
            SecretKey::generate().public()
        ))
    }

    fn generated() -> moq_auth::Key {
        moq_auth::Key::generate(moq_auth::Algorithm::HS256, None).expect("a key")
    }

    #[test]
    fn a_missing_key_file_is_a_keys_error() {
        let path = scratch();
        let Err(err) = load_keys(&path) else {
            panic!("a missing file loaded");
        };
        assert!(
            matches!(&err, Error::Keys { path: named, .. } if *named == path),
            "{err:#}"
        );
    }

    #[test]
    fn a_file_that_holds_no_key_is_a_keys_error() {
        let path = scratch();
        std::fs::write(&path, b"nowhere near a key").expect("writes the scratch file");
        let result = load_keys(&path);
        std::fs::remove_file(&path).expect("removes the scratch file");
        let Err(err) = result else {
            panic!("a file without a key loaded");
        };
        assert!(matches!(err, Error::Keys { .. }), "{err:#}");
    }

    #[test]
    fn a_single_key_and_a_key_set_both_load() {
        let path = scratch();
        let key = generated();
        key.to_file(&path).expect("writes the key");
        let single = load_keys(&path);
        let set = KeySet {
            keys: vec![Arc::new(generated()), Arc::new(generated())],
        };
        set.to_file(&path).expect("writes the set");
        let both = load_keys(&path);
        std::fs::remove_file(&path).expect("removes the scratch file");
        let (single, source) = single.expect("one key");
        assert_eq!(single.keys.len(), 1);
        assert!(matches!(source, serve::Keys::File(named) if named == path));
        let (both, source) = both.expect("a set");
        assert_eq!(both.keys.len(), 2);
        assert!(matches!(source, serve::Keys::Set(named) if named == path));
    }
}
