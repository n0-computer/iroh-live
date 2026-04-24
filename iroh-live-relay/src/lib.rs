//! iroh-live relay server: bridges iroh P2P and browser WebTransport clients.
//!
//! Publishers and subscribers reach the relay either through a raw iroh QUIC
//! connection (MoQ ALPN) or through an HTTP/3 WebTransport session tunneled
//! over iroh. The HTTP/3 path carries URL-level context, which is how we
//! propagate JWT auth tokens via the `?jwt=<token>` query parameter.
//!
//! # Auth
//!
//! When `--auth-key <path>` is set, the relay enforces JWT auth against a
//! JSON Web Key loaded from the file. Tokens embed prefix-scoped
//! `subscribe`/`publish` claims (see `moq_token::Claims`) that govern which
//! broadcast paths a connection may read or write. When no auth key is set,
//! the relay falls back to the permissive mode used during development:
//! every path is publicly subscribable and publishable.
//!
//! # Metrics
//!
//! A Prometheus-compatible `/metrics` endpoint is served from the HTTP
//! listener. It exposes active broadcast and connection counters so that
//! operators (for example the n0des service) can scrape the relay without
//! running a separate exporter.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{extract::State, response::IntoResponse, routing::get};
use clap::Args;
use include_dir::{Dir, include_dir};
use moq_relay::{AuthConfig, Cluster, ClusterConfig, Connection, PublicConfig, PublicDetailed};
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, info, warn};

mod pull;

static WEB_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Configuration for the relay server. Can be embedded in another clap CLI
/// via `#[command(flatten)]`.
#[derive(Args, Debug, Clone)]
pub struct RelayConfig {
    /// Bind address for QUIC (WebTransport and iroh).
    #[arg(long, default_value = "[::]:4443")]
    pub bind: SocketAddr,

    /// Bind address for HTTP (static files, certificate fingerprint, and
    /// `/metrics`). Defaults to the same as `--bind`.
    #[arg(long, default_value = "[::]:4443")]
    pub http_bind: SocketAddr,

    /// Path to a JWK file used to verify JWT auth tokens.
    ///
    /// When set, publishers and subscribers must present a valid token via
    /// the `?jwt=<token>` URL query parameter. Paths outside the token's
    /// `subscribe`/`publish` prefixes are rejected. When unset, the relay
    /// runs in permissive development mode: all paths are public.
    #[arg(long, env = "IROH_LIVE_RELAY_AUTH_KEY")]
    pub auth_key: Option<PathBuf>,
}

/// Runs the relay server. Blocks until the accept loop ends (ctrl-c or error).
///
/// Call `rustls::crypto::aws_lc_rs::default_provider().install_default()`
/// before calling this if no crypto provider has been installed yet.
pub async fn run(config: RelayConfig) -> anyhow::Result<()> {
    let relay = RelayServer::from_env()?;

    let mut server_config = moq_native::ServerConfig::default();
    server_config.bind = Some(config.bind.to_string());
    server_config.backend = Some(moq_native::QuicBackend::Noq);
    server_config.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
    // Self-signed TLS for dev mode. ACME/Let's Encrypt support is planned
    // but not yet implemented (see plans/relay-browser.md).
    server_config.tls.generate = vec!["localhost".to_string()];

    let mut client_config = moq_native::ClientConfig::default();
    client_config.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);

    let mut iroh_config = moq_native::IrohEndpointConfig::default();
    iroh_config.enabled = Some(true);
    iroh_config.secret = Some(relay.iroh_secret_path_str());

    let server = server_config.init()?;
    let client = client_config.init()?;
    let iroh = iroh_config.bind().await?;
    let (mut server, client) = (
        server.with_iroh(iroh.clone()),
        client.with_iroh(iroh.clone()),
    );

    if let Some(ref iroh_ep) = iroh {
        info!(endpoint_id = %iroh_ep.id(), "iroh endpoint bound");
        println!("iroh endpoint: {}", iroh_ep.id());
    }

    let tls_info = server.tls_info();

    let auth_config = build_auth_config(&config)?;
    let auth_mode = if auth_config.key.is_some() {
        AuthMode::Jwt
    } else {
        AuthMode::Permissive
    };
    let auth = auth_config.init().await?;
    info!(?auth_mode, "auth initialised");

    let cluster = Cluster::new(ClusterConfig::default(), client);
    let cluster_handle = cluster.clone();
    tokio::spawn(async move {
        cluster_handle.run().await.expect("cluster failed");
    });

    let pull_state = if iroh.is_some() {
        let pull_ep = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
        let pull_live = iroh_live::Live::new(pull_ep);
        Some(Arc::new(pull::PullState::new(pull_live, cluster.clone())))
    } else {
        None
    };

    let metrics = Arc::new(Metrics::new(auth_mode));
    // Track the number of live local broadcasts by tailing the cluster
    // primary origin. Each announce increments the gauge and decrements when
    // the consumer closes.
    spawn_broadcast_metrics(cluster.primary.consume(), metrics.clone());

    let http_state = Arc::new(HttpState {
        tls_info: tls_info.clone(),
        metrics: metrics.clone(),
    });

    let quic_addr = server.local_addr()?;
    let quic_port = quic_addr.port();
    info!(bind = %quic_addr, "quic listening");

    let static_router = axum::Router::new()
        .route("/certificate.sha256", get(serve_fingerprint))
        .route("/metrics", get(serve_metrics))
        .route("/", get(serve_index))
        .route("/{*path}", get(serve_static))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([http::Method::GET]),
        )
        .with_state(http_state);

    let http_bind = if config.http_bind == config.bind {
        quic_addr
    } else {
        config.http_bind
    };
    let http_listener = tokio::net::TcpListener::bind(http_bind).await?;
    let http_port = http_listener.local_addr()?.port();
    info!(http_port, "http listening");

    // Machine-parseable lines (used by e2e test fixtures).
    println!("http port: {http_port}");
    // Human-friendly clickable URLs.
    println!("iroh-live relay listening at http://localhost:{http_port}");
    println!("iroh-live relay listening at https://localhost:{quic_port}");

    tokio::spawn(async move {
        axum::serve(http_listener, static_router)
            .await
            .expect("http server failed");
    });

    if let Some(ref iroh_ep) = iroh {
        info!(iroh_addr = %iroh_ep.id(), "relay ready");
    }

    let mut conn_id = 0u64;
    while let Some(request) = server.accept().await {
        let transport = request.transport();
        metrics.connections_total.fetch_add(1, Ordering::Relaxed);
        metrics.connections_active.fetch_add(1, Ordering::Relaxed);
        debug!(conn_id, transport, "accepted connection");

        let ticket = pull_state.as_ref().and_then(|_| {
            let name = extract_name_from_url(&request)?;
            debug!("path: {name}");
            let parsed = name.parse::<iroh_live::ticket::LiveTicket>();
            debug!("parsed: {parsed:?}");
            parsed.ok()
        });
        debug!(conn_id, transport, ?ticket, "parsed ticket");

        let pull_clone = pull_state.clone();
        let conn = Connection {
            id: conn_id,
            request,
            cluster: cluster.clone(),
            auth: auth.clone(),
        };
        conn_id += 1;
        let metrics_conn = metrics.clone();
        tokio::spawn(async move {
            if let (Some(ticket), Some(pull)) = (ticket, pull_clone)
                && let Err(err) = pull.pull(&ticket).await
            {
                warn!(%err, "pull failed for ticket in URL");
            }
            if let Err(err) = conn.run().await {
                warn!(%err, "connection closed");
            }
            metrics_conn
                .connections_active
                .fetch_sub(1, Ordering::Relaxed);
        });
    }

    Ok(())
}

/// Assembles the [`AuthConfig`] from CLI flags.
///
/// If `--auth-key` is set, the relay runs with JWT auth only (no public
/// fallback). Otherwise it opens the gates with a catch-all public prefix,
/// which is what the previous builds did unconditionally.
fn build_auth_config(config: &RelayConfig) -> anyhow::Result<AuthConfig> {
    let mut auth_config = AuthConfig::default();
    if let Some(path) = config.auth_key.as_deref() {
        anyhow::ensure!(path.is_file(), "auth key path is not a file: {path:?}");
        auth_config.key = Some(path.to_string_lossy().into_owned());
    } else {
        let prefixes = vec!["".to_string()];
        auth_config.public = Some(PublicConfig::Detailed(PublicDetailed {
            subscribe: prefixes.clone(),
            publish: prefixes,
            api: None,
        }));
    }
    Ok(auth_config)
}

/// Spawns a task that counts the number of primary broadcasts on the cluster
/// origin and mirrors it into a gauge.
///
/// `OriginConsumer::announced` yields `Some((path, Some(consumer)))` when a
/// broadcast becomes active and `Some((path, None))` when it ends. We tail
/// the consumer's `closed` future to decrement the gauge when a publisher
/// goes away without an explicit Ended marker arriving first.
fn spawn_broadcast_metrics(mut consumer: moq_lite::OriginConsumer, metrics: Arc<Metrics>) {
    tokio::spawn(async move {
        while let Some((_, maybe)) = consumer.announced().await {
            let Some(bc) = maybe else { continue };
            metrics.broadcasts_active.fetch_add(1, Ordering::Relaxed);
            metrics.broadcasts_total.fetch_add(1, Ordering::Relaxed);
            let m = metrics.clone();
            tokio::spawn(async move {
                bc.closed().await;
                m.broadcasts_active.fetch_sub(1, Ordering::Relaxed);
            });
        }
    });
}

// -- Internal helpers --------------------------------------------------------

struct RelayServer {
    data_dir: PathBuf,
}

impl RelayServer {
    fn new(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let data_dir = path.into();
        std::fs::create_dir_all(&data_dir)?;
        Ok(Self { data_dir })
    }

    fn from_env() -> anyhow::Result<Self> {
        let path = match std::env::var("IROH_LIVE_RELAY_DATA") {
            Ok(p) => PathBuf::from(p),
            Err(_) => dirs::data_dir()
                .expect("no platform data directory")
                .join("iroh-live-relay"),
        };
        Self::new(path)
    }

    fn iroh_secret_key_path(&self) -> PathBuf {
        self.data_dir.join("iroh_secret_key")
    }

    fn iroh_secret_path_str(&self) -> String {
        self.iroh_secret_key_path().to_string_lossy().into_owned()
    }
}

/// Snapshot of relay state exported on `/metrics`.
struct Metrics {
    connections_total: AtomicU64,
    connections_active: AtomicU64,
    broadcasts_total: AtomicU64,
    broadcasts_active: AtomicU64,
    auth_mode: AuthMode,
}

impl Metrics {
    fn new(auth_mode: AuthMode) -> Self {
        Self {
            connections_total: AtomicU64::new(0),
            connections_active: AtomicU64::new(0),
            broadcasts_total: AtomicU64::new(0),
            broadcasts_active: AtomicU64::new(0),
            auth_mode,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum AuthMode {
    Permissive,
    Jwt,
}

impl AuthMode {
    fn as_str(self) -> &'static str {
        match self {
            AuthMode::Permissive => "permissive",
            AuthMode::Jwt => "jwt",
        }
    }
}

struct HttpState {
    tls_info: Arc<RwLock<moq_native::ServerTlsInfo>>,
    metrics: Arc<Metrics>,
}

fn extract_name_from_url(request: &moq_native::Request) -> Option<String> {
    let url = request.url()?;
    debug!("url: {url}");
    if url.path().len() > 1 {
        Some(url.path()[1..].to_string())
    } else {
        None
    }
}

async fn serve_fingerprint(State(state): State<Arc<HttpState>>) -> impl IntoResponse {
    let info = state.tls_info.read().expect("tls_info lock poisoned");
    info.fingerprints.first().cloned().unwrap_or_default()
}

async fn serve_metrics(State(state): State<Arc<HttpState>>) -> impl IntoResponse {
    let m = &state.metrics;
    let body = format!(
        "# HELP iroh_live_relay_connections_total Accepted connections since startup.\n\
         # TYPE iroh_live_relay_connections_total counter\n\
         iroh_live_relay_connections_total {}\n\
         # HELP iroh_live_relay_connections_active Currently open connections.\n\
         # TYPE iroh_live_relay_connections_active gauge\n\
         iroh_live_relay_connections_active {}\n\
         # HELP iroh_live_relay_broadcasts_total Broadcasts announced since startup.\n\
         # TYPE iroh_live_relay_broadcasts_total counter\n\
         iroh_live_relay_broadcasts_total {}\n\
         # HELP iroh_live_relay_broadcasts_active Currently announced broadcasts.\n\
         # TYPE iroh_live_relay_broadcasts_active gauge\n\
         iroh_live_relay_broadcasts_active {}\n\
         # HELP iroh_live_relay_info Static relay metadata.\n\
         # TYPE iroh_live_relay_info gauge\n\
         iroh_live_relay_info{{auth_mode=\"{}\"}} 1\n",
        m.connections_total.load(Ordering::Relaxed),
        m.connections_active.load(Ordering::Relaxed),
        m.broadcasts_total.load(Ordering::Relaxed),
        m.broadcasts_active.load(Ordering::Relaxed),
        m.auth_mode.as_str(),
    );
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    )
}

async fn serve_index() -> impl IntoResponse {
    serve_embedded_file("index.html")
}

async fn serve_static(axum::extract::Path(path): axum::extract::Path<String>) -> impl IntoResponse {
    serve_embedded_file(&path)
}

fn serve_embedded_file(path: &str) -> axum::response::Response {
    let mime = mime_from_path(path);
    match WEB_DIR.get_file(path) {
        Some(file) => (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, mime)],
            file.contents().to_vec(),
        )
            .into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

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
