//! A relay that serves iroh-live broadcasts to browsers.
//!
//! Browsers connect over WebTransport, iroh clients over iroh. Anyone may
//! connect and subscribe to anything. An iroh client may publish only at the
//! paths that name its endpoint id (see [`IrohSessions`]), and a browser only
//! at names of one segment. A session that names a broadcast ticket gets that
//! broadcast pulled from its publisher (see [`pull`]).
//!
//! The binary wraps [`run`]. Another CLI can embed the relay by flattening
//! [`Config`] into its arguments.
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
//! # async fn main_() -> anyhow::Result<()> {
//! rustls::crypto::aws_lc_rs::default_provider()
//!     .install_default()
//!     .expect("no crypto provider installed yet");
//! iroh_live_relay::run(Cli::parse().relay).await
//! # }
//! ```
//!
//! # Cancellation safety
//!
//! [`run`] returns once a drain started by SIGINT or SIGTERM ends, after
//! closing its listeners. Dropping its future stops the listeners and the
//! accepted connections. The tasks that dial and hold a pull end with the
//! last clone of their [`PullState`](pull::PullState): a linger window after
//! the last session that named the ticket, or later while a dial or a resolve
//! is in flight.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{extract::Path, response::IntoResponse, routing::get};
use clap::Args;
use include_dir::{Dir, include_dir};
use iroh::SecretKey;
use iroh_live::{BroadcastTicket, EndpointConfig, Mdns};
use moq_auth::lease;
use moq_relay::auth::Admissions;
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tracing::{debug, info, warn};

mod iroh_sessions;
pub mod pull;

pub use self::iroh_sessions::{IrohSessions, browser_grant};

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
}

/// Runs the relay server until a shutdown signal or an error.
///
/// The first SIGINT or SIGTERM drains the browser sessions, and a second one
/// ends the relay at once. Install a rustls crypto provider first, as the
/// crate example does.
///
/// # Errors
///
/// Fails if the relay's identity cannot be loaded, an endpoint or listener
/// cannot bind, the cluster cannot start, or a listener fails.
pub async fn run(config: Config) -> anyhow::Result<()> {
    // mDNS finds a ticket's publisher on this machine or LAN at once, where
    // pkarr and DNS take seconds and need internet. `Announce`, since clients
    // reach the relay by endpoint id.
    let iroh_endpoint = EndpointConfig {
        secret_key: Some(secret_key()?),
        mdns: Mdns::Announce,
        ..EndpointConfig::default()
    }
    .bind()
    .await?;
    info!(endpoint_id = %iroh_endpoint.id(), "iroh endpoint bound");
    println!("iroh endpoint: {}", iroh_endpoint.id());

    let mut relay_config = moq_relay::Config::default();
    relay_config.listen.bind = Some(moq_tokio::listen::Bind::Addr(config.bind));
    // Self-signed TLS. No ACME yet.
    relay_config.listen.tls.generate = vec!["localhost".to_string()];
    relay_config.web.http.listen = Some(config.http_bind.unwrap_or(config.bind));
    // The viewer serves `/` and every other path, which the WebSocket
    // fallback would take.
    relay_config.web.ws = false;
    // No `[auth]` source, so `admit` decides every session.
    let mut relay = moq_relay::Relay::load(relay_config).await?;
    let admissions = relay
        .admissions()
        .context("the relay config names an auth source")?;

    // Pulls dial over the relay's own endpoint, with its stable identity.
    let pulls = Arc::new(pull::PullState::new(
        iroh_endpoint.clone(),
        relay.cluster().clone(),
    ));
    let _admit_task = AbortOnDropHandle::new(tokio::spawn(admit(admissions, Some(pulls.clone()))));
    // Not moq-relay's iroh listener: `IrohSessions` accepts iroh clients,
    // since it knows who they are.
    let iroh_router = IrohSessions::new(relay.cluster().clone(), Some(pulls)).router(iroh_endpoint);

    let quic_port = relay.quic_addr()?.port();
    let http_port = relay
        .web_addrs()
        .http
        .context("the http listener is not bound")?
        .port();
    // The e2e test fixtures parse this line.
    println!("http port: {http_port}");
    // `http`, since this listener speaks plain HTTP. The page pins the QUIC
    // certificate's fingerprint from `/certificate.sha256`, so the browser
    // never prompts.
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
    let result = relay.with_web(routes).run().await;
    if let Err(err) = iroh_router.shutdown().await {
        warn!(%err, "the iroh router did not shut down cleanly");
    }
    result
}

/// Answers the relay's admissions until the relay drops its auth.
///
/// Every session gets [`browser_grant`]. A session whose path names a ticket
/// reads the ticket's broadcast at that name, see [`pull::mounted`]. With
/// `pulls` set, it also holds a pull of that broadcast while it runs.
pub async fn admit(mut admissions: Admissions, pulls: Option<Arc<pull::PullState>>) {
    // One task per session that holds a pull, until the session ends.
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
        let mut grant = browser_grant();
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

/// Pulls the broadcast a session named, if the name is a ticket.
///
/// Runs alongside the session, so a session that names an unreachable ticket
/// still starts. Dropping the handle drops the session's claim on the pull.
pub(crate) fn pull_for(
    pull_state: Arc<pull::PullState>,
    name: String,
) -> Option<AbortOnDropHandle<Option<pull::PullGuard>>> {
    // The name as requested, since the subscriber is announced under it.
    let ticket = name.parse::<BroadcastTicket>().ok()?;
    Some(AbortOnDropHandle::new(tokio::spawn(async move {
        match pull_state.pull(&name, &ticket).await {
            Ok(guard) => Some(guard),
            Err(err) => {
                warn!(%err, "pull failed for the ticket in the url");
                None
            }
        }
    })))
}

/// Loads the relay's iroh identity, generating and storing one on first run.
///
/// Every ticket through this relay names its endpoint id, so a file that does
/// not hold a key is an error, and never a reason to make a new identity.
fn secret_key() -> anyhow::Result<SecretKey> {
    let dir = match std::env::var_os("IROH_LIVE_RELAY_DATA") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::data_dir()
            .context("no platform data directory")?
            .join("iroh-live-relay"),
    };
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("iroh_secret_key");
    iroh_live::secret_key_file(&path).with_context(|| {
        format!(
            "cannot load the relay's identity. Deleting {} starts over, with a new \
             endpoint id that invalidates every ticket naming the old one",
            path.display()
        )
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
