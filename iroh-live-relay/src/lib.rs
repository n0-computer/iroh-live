//! iroh-live relay server: bridges iroh P2P and browser WebTransport clients.
//!
//! Authentication is not yet implemented. The relay currently accepts all
//! connections. Adding auth is straightforward since MoQ supports token-based
//! authentication.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{extract::State, response::IntoResponse, routing::get};
use clap::Args;
use include_dir::{Dir, include_dir};
use iroh::{SecretKey, endpoint::presets};
use moq_relay::{AuthConfig, Cluster, ClusterConfig, Connection, PublicConfig, PublicDetailed};
use tokio_util::task::AbortOnDropHandle;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error, info, warn};

pub mod pull;

static WEB_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Configuration for the relay server. Can be embedded in another clap CLI
/// via `#[command(flatten)]`.
#[derive(Args, Debug, Clone)]
pub struct RelayConfig {
    /// Bind address for QUIC (WebTransport and iroh).
    #[arg(long, default_value = "[::]:4443")]
    pub bind: SocketAddr,

    /// Bind address for HTTP (static files and fingerprint endpoint).
    /// Defaults to the same as --bind.
    #[arg(long, default_value = "[::]:4443")]
    pub http_bind: SocketAddr,
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
    server_config.quic.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
    // Self-signed TLS for dev mode. ACME/Let's Encrypt support is planned
    // but not yet implemented (see plans/relay-browser.md).
    server_config.tls.generate = vec!["localhost".to_string()];

    let mut client_config = moq_native::ClientConfig::default();
    client_config.quic.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
    // Cloned before `client_config` is consumed by `init()` below; `AuthConfig::init`
    // only needs a borrow of the client TLS settings.
    let client_tls = client_config.tls.clone();

    let iroh_secret = relay.iroh_secret_key()?;
    // Register the MoQ ALPNs so the endpoint accepts iroh-native MoQ clients
    // (e.g. the `irl` CLI and `subscribe_test`). Mirrors the ALPN set that
    // `moq_native::iroh::EndpointConfig::bind` registers: every MoQ-lite/IETF
    // version plus the WebTransport-over-HTTP/3 ALPN. Without this the endpoint
    // rejects MoQ connections with "peer doesn't support any known protocol".
    let mut alpns: Vec<Vec<u8>> = moq_native::moq_net::ALPNS
        .iter()
        .map(|alpn| alpn.as_bytes().to_vec())
        .collect();
    alpns.push(
        moq_native::iroh::web_transport_iroh::ALPN_H3
            .as_bytes()
            .to_vec(),
    );
    let iroh_endpoint = iroh::Endpoint::builder(presets::N0)
        .secret_key(iroh_secret)
        .alpns(alpns)
        .bind()
        .await?;

    let server = server_config.init()?;
    let client = client_config.init()?;
    let mut server = server.with_iroh(iroh_endpoint.clone());
    let client = client.with_iroh(iroh_endpoint.clone());

    info!(endpoint_id = %iroh_endpoint.id(), "iroh endpoint bound");
    println!("iroh endpoint: {}", iroh_endpoint.id());

    let certificates = server.certificates();

    // TODO: Implement auth (free for all atm)
    let mut auth_config = AuthConfig::default();
    let prefixes = vec!["".to_string()];
    auth_config.public = Some(PublicConfig::Detailed(PublicDetailed {
        subscribe: prefixes.clone(),
        publish: prefixes,
        api: None,
    }));
    let auth = auth_config.init(&client_tls).await?;

    let cluster = Cluster::new(ClusterConfig::default())?.with_client(client);
    // Owned here, so both stop when the accept loop below returns rather than
    // outliving the relay they belong to.
    let cluster_handle = cluster.clone();
    let _cluster_task = AbortOnDropHandle::new(tokio::spawn(async move {
        if let Err(err) = cluster_handle.run().await {
            error!(%err, "the cluster stopped");
        }
    }));

    // The relay's own endpoint dials the tickets too. A second one would give
    // the pulls a fresh identity on every restart and a second socket, relay
    // connection and holepunching state to keep alive, for nothing: dialling
    // out is unaffected by the ALPNs this one accepts on.
    let pull_state = Arc::new(pull::PullState::new(iroh_endpoint.clone(), cluster.clone()));

    let http_state = Arc::new(HttpState { certificates });

    let quic_addr = server.local_addr()?;
    let quic_port = quic_addr.port();
    info!(bind = %quic_addr, "quic listening");

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

    let _http_task = AbortOnDropHandle::new(tokio::spawn(async move {
        if let Err(err) = axum::serve(http_listener, static_router).await {
            error!(%err, "the http server stopped");
        }
    }));

    info!(iroh_addr = %iroh_endpoint.id(), "relay ready");

    let mut conn_id = 0u64;
    while let Some(request) = server.accept().await {
        let transport = request.transport();
        // A name that happens to parse as a ticket is a pull request; anything
        // else is an ordinary broadcast name that the cluster already knows or
        // does not.
        let ticket = extract_name_from_url(&request)
            .and_then(|name| name.parse::<iroh_live::ticket::LiveTicket>().ok());
        debug!(conn_id, %transport, pull = ticket.is_some(), "accepted connection");

        let pull_state = pull_state.clone();
        let conn = Connection {
            id: conn_id,
            request,
            cluster: cluster.clone(),
            auth: auth.clone(),
        };
        conn_id += 1;
        tokio::spawn(async move {
            // Alongside the session rather than before it. The dial can take as
            // long as the publisher takes to answer, and a browser that named an
            // unreachable ticket should get a session that reports an empty
            // broadcast rather than one that never starts.
            //
            // The task holds the guard, so it lives exactly as long as this
            // connection: dropping the handle drops the guard whether the pull
            // finished or not, which is what tells the pull that this session
            // has stopped wanting the broadcast.
            let _pull = ticket.map(|ticket| {
                AbortOnDropHandle::new(tokio::spawn(async move {
                    match pull_state.pull(&ticket).await {
                        Ok(guard) => Some(guard),
                        Err(err) => {
                            warn!(%err, "pull failed for the ticket in the url");
                            None
                        }
                    }
                }))
            });
            if let Err(err) = conn.run().await {
                warn!(conn_id, %err, "connection closed");
            }
        });
    }

    Ok(())
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

    /// Loads the relay's iroh identity, generating and storing one on first run.
    ///
    /// The relay's endpoint id is what every published ticket names, so losing
    /// this file renames the relay and strands every ticket anyone is holding.
    /// An unreadable file is therefore an error rather than a reason to generate
    /// a new identity over the top of it.
    fn iroh_secret_key(&self) -> anyhow::Result<SecretKey> {
        let path = self.iroh_secret_key_path();
        if path.try_exists()? {
            let key = std::fs::read(&path)?;
            return Ok(SecretKey::from_bytes((&key[..]).try_into()?));
        }
        let key = SecretKey::generate();
        write_private(&path, &key.to_bytes())?;
        info!(path = %path.display(), "generated the relay's iroh identity");
        Ok(key)
    }
}

/// Writes `contents` to `path`, readable by this user alone.
///
/// A secret key under the default umask is world readable, and every other user
/// on the machine can then be this relay.
fn write_private(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)?.write_all(contents)?;
    Ok(())
}

struct HttpState {
    certificates: moq_native::tls::Certificates,
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
    state
        .certificates
        .fingerprints()
        .first()
        .cloned()
        .unwrap_or_default()
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
