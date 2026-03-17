use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use axum::{extract::State, response::IntoResponse, routing::get};
use clap::Parser;
use include_dir::{Dir, include_dir};
use moq_relay::{AuthConfig, Cluster, ClusterConfig, Connection};
use tower_http::cors::{Any, CorsLayer};

static WEB_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Persistent data directory for certs, iroh key, and relay state.
struct RelayServer {
    data_dir: PathBuf,
}

impl RelayServer {
    /// Creates a relay with an explicit data directory.
    fn new(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let data_dir = path.into();
        std::fs::create_dir_all(&data_dir)?;
        Ok(Self { data_dir })
    }

    /// Creates a relay using `IROH_LIVE_RELAY_DATA` env var, or the
    /// platform data directory (`~/.local/share/iroh-live-relay` on Linux).
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

    /// Returns the iroh secret key path as a string for `IrohEndpointConfig`.
    ///
    /// `moq_native::IrohEndpointConfig` accepts a file path in its `secret`
    /// field. If the file does not exist, it generates a random key and writes
    /// it there. This keeps the endpoint ID stable across restarts.
    fn iroh_secret_path_str(&self) -> String {
        self.iroh_secret_key_path().to_string_lossy().into_owned()
    }
}

#[derive(Parser)]
#[command(about = "iroh-live relay: bridges iroh P2P and WebTransport/browser clients")]
struct Cli {
    /// Dev mode: self-signed certs, prints fingerprint
    #[arg(long)]
    dev: bool,

    /// Bind address for QUIC (noq WebTransport + iroh)
    #[arg(long, default_value = "[::]:4443")]
    bind: SocketAddr,

    /// Bind address for HTTP (static files, fingerprint endpoint)
    #[arg(long, default_value = "[::]:8080")]
    http_bind: SocketAddr,
}

/// Shared state for the HTTP server.
struct HttpState {
    tls_info: Arc<RwLock<moq_native::ServerTlsInfo>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install crypto provider");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    let relay = RelayServer::from_env()?;

    // Server: noq backend for WebTransport, self-signed certs in dev
    let mut server_config = moq_native::ServerConfig::default();
    server_config.bind = Some(cli.bind);
    server_config.backend = Some(moq_native::QuicBackend::Noq);
    server_config.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
    if cli.dev {
        server_config.tls.generate = vec!["localhost".to_string()];
    }

    let mut client_config = moq_native::ClientConfig::default();
    client_config.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);

    // Iroh: persistent secret key
    let mut iroh_config = moq_native::IrohEndpointConfig::default();
    iroh_config.enabled = Some(true);
    iroh_config.secret = Some(relay.iroh_secret_path_str());

    // Initialize server + client + iroh
    let server = server_config.init()?;
    let client = client_config.init()?;
    let iroh = iroh_config.bind().await?;
    let (mut server, client) = (
        server.with_iroh(iroh.clone()),
        client.with_iroh(iroh.clone()),
    );

    if let Some(ref iroh_ep) = iroh {
        tracing::info!(endpoint_id = %iroh_ep.id(), "iroh endpoint bound");
    }

    let tls_info = server.tls_info();

    // Auth: public access (no JWT required)
    let auth_config = AuthConfig {
        public: Some(String::new()),
        ..Default::default()
    };
    let auth = auth_config.init().await?;

    // Cluster: local-only (no remote nodes)
    let cluster = Cluster::new(ClusterConfig::default(), client);
    let cluster_handle = cluster.clone();
    tokio::spawn(async move {
        cluster_handle.run().await.expect("cluster failed");
    });

    // HTTP server: static files + TLS fingerprint
    let http_state = Arc::new(HttpState {
        tls_info: tls_info.clone(),
    });

    let static_router = axum::Router::new()
        .route("/certificate.sha256", get(serve_fingerprint))
        .route("/", get(serve_index))
        .route("/{*path}", get(serve_static))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([http::Method::GET]),
        )
        .with_state(http_state);

    let http_listener = tokio::net::TcpListener::bind(cli.http_bind).await?;
    let http_port = http_listener.local_addr()?.port();
    tracing::info!(http_port, "http listening");

    tokio::spawn(async move {
        axum::serve(http_listener, static_router)
            .await
            .expect("http server failed");
    });

    tracing::info!(bind = %cli.bind, "quic listening");

    if let Some(ref iroh_ep) = iroh {
        tracing::info!(
            iroh_addr = %iroh_ep.id(),
            "relay ready"
        );
    }

    // Accept loop: handles noq (WebTransport from browsers) and iroh (P2P from CLI)
    let mut conn_id = 0u64;
    while let Some(request) = server.accept().await {
        let transport = request.transport();
        tracing::debug!(conn_id, transport, "accepted connection");

        let conn = Connection {
            id: conn_id,
            request,
            cluster: cluster.clone(),
            auth: auth.clone(),
        };
        conn_id += 1;
        tokio::spawn(async move {
            if let Err(err) = conn.run().await {
                tracing::warn!(%err, "connection closed");
            }
        });
    }

    Ok(())
}

/// Serves the TLS certificate fingerprint for WebTransport dev mode.
async fn serve_fingerprint(State(state): State<Arc<HttpState>>) -> impl IntoResponse {
    let info = state.tls_info.read().expect("tls_info lock poisoned");
    info.fingerprints.first().cloned().unwrap_or_default()
}

/// Serves `index.html` from the embedded web directory.
async fn serve_index() -> impl IntoResponse {
    serve_embedded_file("index.html")
}

/// Serves static files from the embedded web directory.
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
