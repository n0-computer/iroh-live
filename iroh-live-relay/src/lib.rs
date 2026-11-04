//! A relay that serves iroh-live broadcasts to browsers.
//!
//! Browsers connect over WebTransport, iroh clients over iroh. Anyone may
//! connect and subscribe to anything. An iroh client may publish only at the
//! paths that name its endpoint id (see [`iroh_sessions`]), and a browser only
//! at names of one segment. A session that names a broadcast ticket gets that
//! broadcast pulled from its publisher (see [`pull`]).
//!
//! The binary wraps [`run`]. Another CLI can embed the relay by flattening
//! [`RelayConfig`] into its arguments.
//!
//! # Examples
//!
//! ```no_run
//! use clap::Parser;
//!
//! #[derive(Parser)]
//! struct Cli {
//!     #[command(flatten)]
//!     relay: iroh_live_relay::RelayConfig,
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
//! [`run`] returns on ctrl-c, after closing its listeners. Dropping its future
//! stops everything it started, accepted connections included.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::get,
};
use clap::Args;
use include_dir::{Dir, include_dir};
use iroh::SecretKey;
use iroh_live::{BroadcastTicket, EndpointOptions, Mdns};
use moq_relay::{Connection, cluster::Cluster};
use moq_tokio::tls::Certificates;
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error, info, warn};

mod iroh_sessions;
pub mod pull;

pub use self::iroh_sessions::{IrohSessions, browser_auth};

static WEB_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// The name this relay gives its auth requests.
const RELAY_NODE: &str = "iroh-live-relay";

/// Configuration for the relay server, as clap arguments.
#[derive(Args, Debug, Clone)]
pub struct RelayConfig {
    /// Bind address for WebTransport over QUIC.
    #[arg(long, default_value = "[::]:4443")]
    pub bind: SocketAddr,

    /// Bind address for the web viewer over HTTP.
    ///
    /// Defaults to the address --bind bound, over TCP.
    #[arg(long)]
    pub http_bind: Option<SocketAddr>,
}

/// Runs the relay server until ctrl-c or an error.
///
/// Install a rustls crypto provider first, as the crate example does.
///
/// # Errors
///
/// Fails if the relay's identity cannot be loaded, an endpoint or listener
/// cannot bind, or the cluster cannot start. A failure of the listener after
/// startup ends the relay without an error.
pub async fn run(config: RelayConfig) -> anyhow::Result<()> {
    let mut quic = moq_tokio::quic::Config::default();
    quic.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
    let connect = moq_tokio::connect::Config::default();

    let iroh_secret = secret_key()?;
    // mDNS finds a ticket's publisher on this machine or LAN at once, where
    // pkarr and DNS take seconds and need internet. `Announce`, since clients
    // reach the relay by endpoint id.
    let iroh_endpoint = EndpointOptions {
        secret_key: Some(iroh_secret),
        mdns: Mdns::Announce,
    }
    .bind()
    .await?;

    let mut server_config = moq_tokio::server::Config::default();
    server_config.listen.bind = Some(moq_tokio::listen::Bind::Addr(config.bind));
    // Self-signed TLS. No ACME yet.
    server_config.listen.tls.generate = vec!["localhost".to_string()];
    server_config.quic = quic.clone();
    // Not `server_config.iroh`: `IrohSessions` accepts iroh clients, since it
    // knows who they are.
    let server = server_config.init()?;
    info!(endpoint_id = %iroh_endpoint.id(), "iroh endpoint bound");
    println!("iroh endpoint: {}", iroh_endpoint.id());

    let certificates = server.certificates();

    let auth = browser_auth().init(RELAY_NODE, &connect.tls, false)?;

    let (cluster, driver) = Cluster::new(moq_relay::cluster::Options::new(Default::default()))?;
    let cluster = cluster.with_connect(connect.clone(), quic);
    // The origin makes no progress unless its driver runs.
    let _driver_task = AbortOnDropHandle::new(tokio::spawn(async move {
        if let Err(err) = driver.run().await {
            error!(%err, "the cluster driver stopped");
        }
    }));
    // Started before the task, so a cluster that cannot bind fails `run`.
    let started = cluster.clone().start().await?;
    let _cluster_task = AbortOnDropHandle::new(tokio::spawn(async move {
        if let Err(err) = started.run().await {
            error!(%err, "the cluster stopped");
        }
    }));

    // Pulls dial over the relay's own endpoint, with its stable identity.
    let pull_state = Arc::new(pull::PullState::new(iroh_endpoint.clone(), cluster.clone()));
    let iroh_router =
        IrohSessions::new(cluster.clone(), Some(pull_state.clone())).router(iroh_endpoint.clone());

    let quic_addr = server.local_addr()?;
    let quic_port = quic_addr.port();
    info!(bind = %quic_addr, "quic listening");

    let static_router = axum::Router::new()
        .route("/certificate.sha256", get(serve_fingerprint))
        .route("/", get(|| async { serve_embedded_file("index.html") }))
        .route(
            "/{*path}",
            get(|Path(path): Path<String>| async move { serve_embedded_file(&path) }),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([axum::http::Method::GET]),
        )
        .with_state(certificates);

    let http_bind = config.http_bind.unwrap_or(quic_addr);
    let http_listener = tokio::net::TcpListener::bind(http_bind).await?;
    let http_port = http_listener.local_addr()?.port();
    info!(http_port, "http listening");

    // Machine-parseable lines (used by e2e test fixtures).
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

    let _http_task = AbortOnDropHandle::new(tokio::spawn(async move {
        if let Err(err) = axum::serve(http_listener, static_router).await {
            error!(%err, "the http server stopped");
        }
    }));

    info!(iroh_addr = %iroh_endpoint.id(), "relay ready");

    let mut listener = server.listen().await?;
    // moq-tokio's listener does not stop on ctrl-c, so the loop watches for it.
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    let mut conn_id = 0u64;
    let mut connections = JoinSet::new();
    loop {
        let request = tokio::select! {
            request = listener.accept() => match request {
                Ok(request) => request,
                Err(err) => {
                    warn!(%err, "the listener stopped accepting");
                    break;
                }
            },
            _ = &mut interrupted => {
                info!("interrupted, closing the listeners");
                break;
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
        };
        let transport = request.transport();
        // A name that parses as a ticket starts a pull.
        let name = extract_name_from_url(&request);
        debug!(conn_id, %transport, ?name, "accepted connection");

        let pull_state = pull_state.clone();
        // A session that names a ticket reads the pulled broadcast at that name.
        let serving = name
            .as_deref()
            .map_or_else(|| cluster.clone(), |name| pull::serving(&cluster, name));
        let conn = Connection::new(request, serving, auth.clone()).with_id(conn_id);
        connections.spawn(async move {
            // Held by the task, so it lives exactly as long as this connection.
            let _pull = name.and_then(|name| pull_for(pull_state, name));
            conn.run().await;
        });
        conn_id += 1;
    }

    // Releases the listener's sockets before `run` returns.
    listener.close().await;
    if let Err(err) = iroh_router.shutdown().await {
        warn!(%err, "the iroh router did not shut down cleanly");
    }
    Ok(())
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

/// Returns the broadcast name in the path of a connection's request URL, if any.
fn extract_name_from_url(request: &moq_tokio::server::Request) -> Option<String> {
    iroh_sessions::requested_name(request.url()?.path())
}

/// Serves the SHA-256 fingerprint of the relay's self-signed certificate.
///
/// The web viewer pins it, so the browser accepts the QUIC connection.
async fn serve_fingerprint(State(certificates): State<Certificates>) -> impl IntoResponse {
    certificates
        .fingerprints()
        .first()
        .cloned()
        .unwrap_or_default()
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
