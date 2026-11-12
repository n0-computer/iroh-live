//! A relay that serves iroh-live broadcasts to browsers.
//!
//! Browsers connect over WebTransport, iroh clients over iroh. Anyone may
//! subscribe to anything. An iroh client may publish only at paths that name
//! its endpoint id ([`relay_policy`]), and a browser only at names of one
//! segment. A session that names a broadcast ticket gets it pulled, see
//! [`pull`].
//!
//! With `--auth-key`, a session that presents a valid JWT gets what the token
//! grants instead. `--auth-url` and the `--auth-public` flags hand browser
//! sessions to moq-relay's own auth, which then pulls no tickets.
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
//! [`run`] returns once a drain started by SIGINT or SIGTERM ends, after
//! closing its listeners. Dropping its future stops the listeners and the
//! accepted connections. The tasks that dial and hold a pull end with the
//! last clone of their [`Pulls`](pull::Pulls): a linger window after
//! the last session that named the ticket, or later while a dial or a resolve
//! is in flight.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{extract::Path, response::IntoResponse, routing::get};
use clap::Args;
use include_dir::{Dir, include_dir};
use iroh::{EndpointId, SecretKey, protocol::Router};
use iroh_live::{BroadcastTicket, EndpointConfig, Mdns};
use iroh_live_node::{SessionRequest, policy::DynPolicy};
use moq_auth::{KeySet, lease};
use moq_ext_access::policy::{JwtVerifier, Verify};
use moq_net::Pattern;
use moq_relay::auth::Admissions;
use n0_error::{AnyError, e};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tracing::{debug, info, warn};

mod error;
mod iroh_sessions;
pub mod pull;

pub use self::{
    error::Error,
    iroh_sessions::{IrohSessions, browser_grant, relay_policy},
};

/// The bundled web viewer, embedded at build time.
static WEB_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Configuration for the relay server, as clap arguments.
#[derive(Args, Debug, Clone)]
pub struct Config {
    /// Bind address for WebTransport over QUIC.
    #[arg(long, default_value = "[::]:4443")]
    pub bind: SocketAddr,

    /// Bind address for the web viewer over HTTP.
    ///
    /// Defaults to the --bind address, over TCP.
    #[arg(long)]
    pub http_bind: Option<SocketAddr>,

    /// Verifies the JWTs that browsers and iroh clients present with the keys in this file.
    ///
    /// The file holds a JWK or a JWK set, as `moq auth` writes them. An invalid
    /// token is refused, and a session without a token keeps the public rules.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["auth_url", "auth_public", "auth_public_publish", "auth_public_subscribe"])]
    pub auth_key: Option<PathBuf>,

    /// Asks this auth server about every browser session, as moq-relay does.
    #[arg(long, value_name = "URL", conflicts_with_all = ["auth_public", "auth_public_publish", "auth_public_subscribe"])]
    pub auth_url: Option<url::Url>,

    /// Lets every browser session publish and subscribe at these patterns, as moq-relay does.
    #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
    pub auth_public: Vec<Pattern>,

    /// Lets every browser session publish at these patterns, as moq-relay does.
    #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
    pub auth_public_publish: Vec<Pattern>,

    /// Lets every browser session subscribe at these patterns, as moq-relay does.
    #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
    pub auth_public_subscribe: Vec<Pattern>,

    /// Decides the sessions of iroh clients, instead of the default.
    ///
    /// The default is [`relay_policy`], or with `--auth-key` a [`Verify`] of
    /// the same keys with [`relay_policy`] as its fallback.
    #[arg(skip)]
    pub iroh_policy: Option<Arc<dyn DynPolicy<EndpointId>>>,
}

/// Runs the relay server until a shutdown signal or an error.
///
/// The first SIGINT or SIGTERM drains the browser sessions, and a second one
/// ends the relay at once. Install a rustls crypto provider first, as the
/// crate example does.
///
/// # Errors
///
/// Fails with [`Error::NoDataDir`] or [`Error::Identity`] if the relay's
/// identity cannot be loaded, [`Error::Bind`] if the iroh endpoint cannot
/// bind, [`Error::Keys`] if the JWT keys cannot be read, and
/// [`Error::Relay`] if the moq relay cannot start or one of its listeners
/// fails.
pub async fn run(config: Config) -> Result<(), Error> {
    // mDNS finds a ticket's publisher on this machine or LAN at once, where
    // pkarr and DNS take seconds and need internet. `Announce`, since clients
    // reach the relay by endpoint id.
    let iroh_endpoint = EndpointConfig {
        secret_key: Some(secret_key()?),
        mdns: Mdns::Announce,
    }
    .bind()
    .await
    .map_err(|source| {
        e!(Error::Bind {
            source: Arc::new(source)
        })
    })?;
    info!(endpoint_id = %iroh_endpoint.id(), "iroh endpoint bound");
    println!("iroh endpoint: {}", iroh_endpoint.id());

    let keys = config.auth_key.as_deref().map(load_keys).transpose()?;
    let mut relay_config = moq_relay::Config::default();
    relay_config.auth.url = config.auth_url.clone();
    relay_config.auth.public = config.auth_public.clone();
    relay_config.auth.public_publish = config.auth_public_publish.clone();
    relay_config.auth.public_subscribe = config.auth_public_subscribe.clone();
    relay_config.listen.bind = Some(moq_tokio::listen::Bind::Addr(config.bind));
    // Self-signed TLS. No ACME yet.
    relay_config.listen.tls.generate = vec!["localhost".to_string()];
    relay_config.web.http.listen = Some(config.http_bind.unwrap_or(config.bind));
    // The viewer serves `/` and every other path, which the WebSocket
    // fallback would take.
    relay_config.web.ws = false;
    let mut relay = moq_relay::Relay::load(relay_config)
        .await
        .map_err(relay_error)?;
    let pulls = pull::Pulls::new(iroh_endpoint.clone(), relay.cluster().clone());
    // Without an auth source of moq-relay's own, `admit` decides every
    // browser session.
    let _admit_task = match relay.admissions() {
        Some(admissions) => Some(AbortOnDropHandle::new(tokio::spawn(admit(
            admissions,
            Some(pulls.clone()),
            keys.clone(),
        )))),
        None => {
            info!("moq-relay's auth decides browser sessions, which pull no tickets");
            None
        }
    };
    let iroh_policy = config.iroh_policy.unwrap_or_else(|| match keys {
        Some(keys) => Arc::new(Verify::new(JwtVerifier::new(keys)).with_fallback(relay_policy())),
        None => Arc::new(relay_policy()),
    });
    // Not moq-relay's iroh listener: `IrohSessions` accepts iroh clients,
    // since it knows who they are.
    let iroh_router = IrohSessions::new(relay.cluster().clone(), Some(pulls))
        .with_policy(iroh_policy)
        .mount(Router::builder(iroh_endpoint))
        .spawn();

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

/// Wraps an error of moq-relay, which reports anyhow errors.
fn relay_error(err: anyhow::Error) -> Error {
    e!(Error::Relay {
        source: Arc::new(AnyError::from_anyhow(err)),
    })
}

/// Answers the relay's admissions until the relay drops its auth.
///
/// A session gets [`browser_grant`] for its path, or with `keys` what its JWT grants until
/// the token expires. A session whose path names a ticket reads the ticket's
/// broadcast at that name, and with `pulls` set it holds a pull of that
/// broadcast while it runs.
///
/// [`run`] calls it. It is public for a binary that embeds the relay with a
/// moq-relay of its own, and for the tests.
pub async fn admit(mut admissions: Admissions, pulls: Option<pull::Pulls>, keys: Option<KeySet>) {
    let verifier = keys.map(JwtVerifier::new);
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
        let mut grant = match browser_session_grant(&admission, verifier.as_ref()) {
            Ok(grant) => grant,
            Err(err) => {
                info!(path = %admission.request.path, error = %err, "refusing a browser session");
                admission.refuse(err);
                continue;
            }
        };
        if let Some(ticket) = &ticket {
            // The session's root is the ticket it dialed, so this mounts the
            // broadcast at that name.
            grant
                .mounts
                .insert(String::new(), ticket.path().to_string());
        }
        debug!(path = %admission.request.path, ticket = ticket.is_some(), "admitting a session");
        // An attached admission verifies a token for a session already held.
        let pull = pulls
            .as_ref()
            .filter(|_| !admission.attached())
            .zip(name)
            .and_then(|(pulls, name)| pull_for(pulls.clone(), name));
        let Some(pull) = pull else {
            admission.grant(lease::Consumer::fixed(grant));
            continue;
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

/// Returns the grant of a browser session: its token's with `verifier`, else [`browser_grant`].
fn browser_session_grant(
    admission: &moq_relay::auth::Admission,
    verifier: Option<&JwtVerifier>,
) -> Result<moq_auth::Grant, moq_relay::auth::Error> {
    let request = &admission.request;
    let public = || {
        browser_grant(&request.path)
            .map_err(|err| moq_relay::auth::Error::Forbidden(err.to_string()))
    };
    let Some(verifier) = verifier else {
        return public();
    };
    let token = if admission.attached() {
        // The credential presented in band, for a session already held.
        request.auth.clone()
    } else {
        let target = match &request.query {
            Some(query) => format!("{}?{query}", request.path),
            None => request.path.clone(),
        };
        let setup = request
            .token
            .as_ref()
            .map(|token| (token.kind, token.value.as_slice()));
        SessionRequest::from_setup(&target, setup)
            .token()
            .map(str::to_owned)
    };
    let Some(token) = token else {
        return public();
    };
    verifier
        .verify_grant(&token, &request.path)
        .map_err(|err| moq_relay::auth::Error::Forbidden(format!("invalid token: {err}")))
}

/// Loads the JWT keys from a file that holds a JWK set or a single JWK.
fn load_keys(path: &std::path::Path) -> Result<KeySet, Error> {
    if let Ok(keys) = KeySet::from_file(path) {
        return Ok(keys);
    }
    let key = moq_auth::Key::from_file(path).map_err(|source| {
        e!(Error::Keys {
            path: path.to_owned(),
            source: Arc::new(source),
        })
    })?;
    Ok(KeySet {
        keys: vec![Arc::new(key)],
    })
}

/// Pulls the broadcast a session named, if the name is a ticket.
///
/// Runs alongside the session, so a session that names an unreachable ticket
/// still starts. Dropping the handle releases the session's hold on the pull.
pub(crate) fn pull_for(
    pulls: pull::Pulls,
    name: String,
) -> Option<AbortOnDropHandle<Option<pull::PullGuard>>> {
    // The name as requested, since the subscriber is announced under it.
    let ticket = name.parse::<BroadcastTicket>().ok()?;
    Some(AbortOnDropHandle::new(tokio::spawn(async move {
        match pulls.pull(&name, &ticket).await {
            Ok(guard) => Some(guard),
            Err(err) => {
                warn!(%name, error = format_args!("{err:#}"), "pull failed for the ticket in the URL");
                None
            }
        }
    })))
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
        .and_then(|()| iroh_live::load_or_create_secret_key(&path))
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
    }

    #[test]
    fn a_key_and_another_auth_source_conflict() {
        assert!(parse(&["--auth-key", "k", "--auth-url", "https://auth.example"]).is_err());
        assert!(parse(&["--auth-key", "k", "--auth-public", "anon/**"]).is_err());
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
        assert_eq!(single.expect("one key").keys.len(), 1);
        assert_eq!(both.expect("a set").keys.len(), 2);
    }
}
