//! iroh-live relay server: bridges iroh P2P and browser WebTransport clients.
//!
//! Admission is open: anyone may connect and subscribe to anything. What a
//! session may publish is not. An iroh client publishes only at the paths that
//! name its authenticated endpoint id (see [`iroh_sessions`]), and a browser
//! only at names of one segment, so nobody can publish a broadcast under
//! another publisher's path. Token auth for the rest is still to come.
//!
//! The binary is a thin wrapper around [`run`]; another CLI can embed the
//! relay by flattening [`RelayConfig`] into its own arguments.
//!
//! # Example
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
//! instead stops accepting, the cluster and the HTTP server, which the future
//! owns, while connections already accepted run on their own tasks until they
//! close.

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
use iroh::SecretKey;
use iroh_live::BroadcastTicket;
use iroh_moq::{EndpointOptions, Mdns};
use moq_relay::{Connection, cluster::Cluster};
use tokio_util::task::AbortOnDropHandle;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error, info, warn};

pub mod iroh_sessions;
pub mod pull;

pub use self::iroh_sessions::{IrohSessions, browser_auth, publish_scope};

static WEB_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// The name this relay reports in its auth requests.
///
/// Nothing reads it today, since every path is public, but the auth builder
/// wants one and a relay that later grows an auth server should say which relay
/// is asking.
const RELAY_NODE: &str = "iroh-live-relay";

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

    // Shared by both directions: moq-tokio keeps one QUIC configuration where
    // the server and the client used to carry one each.
    let mut quic = moq_tokio::quic::Config::default();
    quic.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
    let connect = moq_tokio::connect::Config::default();

    let iroh_secret = relay.iroh_secret_key()?;
    // Every MoQ-lite/IETF version plus WebTransport over HTTP/3, which is what
    // iroh-native MoQ clients (`irl`, `subscribe_test`) dial with.
    // `IrohSessions`' router accepts under the same set.
    let alpns = IrohSessions::alpns();
    // mDNS, for the same reason `irl` takes it: a ticket names an endpoint id and
    // no addresses, and pull mode's whole job is turning one of those into a
    // connection. Pkarr and DNS cover a publisher with internet, and they take a
    // few seconds to propagate after it starts; mDNS covers the publisher on this
    // machine or this LAN, and covers it immediately. Without it the relay was the
    // one component that could not resolve a ticket `irl watch` resolves fine,
    // and a pull of a just-started local publisher failed with "No addressing
    // information available" until pkarr caught up.
    //
    // `Announce` rather than `Lookup`: the relay accepts sessions, and a
    // publisher reaches it by endpoint id, so it has an address worth publishing.
    let iroh_endpoint = EndpointOptions::default()
        .with_secret_key(iroh_secret)
        .with_mdns(Mdns::Announce)
        .builder()
        .await
        .alpns(alpns)
        .bind()
        .await?;

    // The backend is left to its default, which is noq. The iroh endpoint is
    // part of the configuration now rather than attached after `init`.
    let mut server_config = moq_tokio::server::Config::default();
    server_config.listen.bind = Some(moq_tokio::listen::Bind::Addr(config.bind));
    // Self-signed TLS for dev mode. ACME/Let's Encrypt support is planned
    // but not yet implemented.
    server_config.listen.tls.generate = vec!["localhost".to_string()];
    server_config.quic = quic.clone();
    // Not `server_config.iroh`: iroh clients are accepted by `IrohSessions`
    // below, which knows who they are.
    let server = server_config.init()?;
    let client = connect.clone().init(quic)?.with_iroh(iroh_endpoint.clone());

    info!(endpoint_id = %iroh_endpoint.id(), "iroh endpoint bound");
    println!("iroh endpoint: {}", iroh_endpoint.id());

    let certificates = server.certificates();

    // Browsers and other non-iroh clients: subscribe to anything, publish at
    // names of one segment. No expiry and no auth server behind it yet.
    let auth = browser_auth().init(RELAY_NODE, &connect.tls)?;

    let cluster =
        Cluster::new(moq_relay::cluster::Options::new(Default::default()))?.with_client(client);
    // Started here rather than inside the task, so a cluster that cannot bind
    // fails `run` before the relay prints that it is listening. Owned here, so
    // both stop when the accept loop below returns rather than outliving the
    // relay they belong to. With no peers configured it has nothing to do and
    // the task ends at once.
    let started = cluster.clone().start().await?;
    let _cluster_task = AbortOnDropHandle::new(tokio::spawn(async move {
        if let Err(err) = started.run().await {
            error!(%err, "the cluster stopped");
        }
    }));

    // The relay's own endpoint dials the tickets too. A second one would give
    // the pulls a fresh identity on every restart and a second socket, relay
    // connection and holepunching state to keep alive, for nothing: dialling
    // out is unaffected by the ALPNs this one accepts on.
    let pull_state = Arc::new(pull::PullState::new(iroh_endpoint.clone(), cluster.clone()));
    let iroh_router =
        IrohSessions::new(cluster.clone(), Some(pull_state.clone())).router(iroh_endpoint.clone());

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
    // The one address a person types. `http` and not `https` on purpose: this
    // listener speaks plain HTTP, and the QUIC port next to it carries
    // WebTransport over HTTP/3 rather than anything a browser will open from
    // the address bar. Typing `https://localhost:{http_port}` reaches this
    // listener over TCP and fails inside TLS ("record that exceeded the maximum
    // permissible length"), which is the browser reading `HTTP/1.1 400` as a
    // TLS record.
    //
    // The self-signed certificate needs no exception either. The page fetches
    // its fingerprint from `/certificate.sha256` and pins it when it opens the
    // WebTransport session, so the browser never prompts.
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
    // The accept loop is ours, so the signal is too. moq-tokio's server used to
    // end on Ctrl-C by itself and no longer does: `accept` returning `None` now
    // means every listener has stopped, which a terminal interrupt never causes.
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    let mut conn_id = 0u64;
    loop {
        let request = tokio::select! {
            request = listener.accept() => match request {
                Some(request) => request,
                None => break,
            },
            _ = &mut interrupted => {
                info!("interrupted, closing the listeners");
                break;
            }
        };
        let transport = request.transport();
        // A name that happens to parse as a ticket is a pull request; anything
        // else is an ordinary broadcast name that the cluster already knows or
        // does not.
        let name = extract_name_from_url(&request);
        debug!(conn_id, %transport, ?name, "accepted connection");

        let pull_state = pull_state.clone();
        let conn = Connection::new(request, cluster.clone(), auth.clone()).with_id(conn_id);
        conn_id += 1;
        tokio::spawn(async move {
            // Held by the task, so it lives exactly as long as this connection.
            let _pull = name.and_then(|name| pull_for(pull_state, name));
            if let Err(err) = conn.run().await {
                warn!(conn_id, %err, "connection closed");
            }
        });
    }

    // Consumes the listener, so its sockets are released before `run` returns
    // rather than whenever the last clone of anything holding them drops.
    listener.close().await;
    if let Err(err) = iroh_router.shutdown().await {
        warn!(%err, "the iroh router did not shut down cleanly");
    }
    Ok(())
}

/// Pulls the broadcast a session named, if the name is a ticket, for as long
/// as the returned handle lives.
///
/// Alongside the session rather than before it: the dial can take as long as
/// the publisher takes to answer, and a client that named an unreachable ticket
/// should get a session that reports an empty broadcast rather than one that
/// never starts. The handle holds the guard, so dropping it with the session
/// tells the pull that this session stopped wanting the broadcast.
pub(crate) fn pull_for(
    pull_state: Arc<pull::PullState>,
    name: String,
) -> Option<AbortOnDropHandle<Option<pull::PullGuard>>> {
    // The requested spelling travels with the ticket: it is the path the
    // subscriber will be announced under, and the two have to agree.
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
            return self.stored_secret_key();
        }
        let key = SecretKey::generate();
        match write_private(&path, &key.to_bytes()) {
            Ok(()) => {
                info!(path = %path.display(), "generated the relay's iroh identity");
                Ok(key)
            }
            // Two relays started together on one data directory. The file is
            // created exclusively, so exactly one of them wrote its key and the
            // other reads it: the loser adopting the winner's identity is the
            // only outcome where both are the relay every ticket names.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                debug!(path = %path.display(), "another process wrote the identity first");
                self.stored_secret_key()
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Reads the identity that is already on disk.
    fn stored_secret_key(&self) -> anyhow::Result<SecretKey> {
        let path = self.iroh_secret_key_path();
        let stored = std::fs::read(&path)?;
        read_secret_key(&stored).map_err(|err| {
            anyhow::anyhow!(
                "{} holds {} bytes that are not an iroh secret key ({err}). Delete it to \
                 start over, which gives this relay a new endpoint id and invalidates \
                 every ticket that names the old one.",
                path.display(),
                stored.len(),
            )
        })
    }
}

/// Reads a stored iroh secret key: its 32 raw bytes.
fn read_secret_key(stored: &[u8]) -> anyhow::Result<SecretKey> {
    let bytes = <&[u8; 32]>::try_from(stored).map_err(|_| anyhow::anyhow!("not 32 bytes"))?;
    Ok(SecretKey::from_bytes(bytes))
}

/// Writes `contents` to a new `path`, readable by this user alone.
///
/// A secret key under the default umask is world readable, and every other user
/// on the machine can then be this relay.
///
/// Created exclusively rather than truncated, so a second relay racing this one
/// on the same data directory fails with
/// [`AlreadyExists`](std::io::ErrorKind::AlreadyExists) instead of writing a
/// second identity over the first. The caller reads the winner's.
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)?.write_all(contents)
}

struct HttpState {
    certificates: moq_tokio::tls::Certificates,
}

fn extract_name_from_url(request: &moq_tokio::server::Request) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored key reads back as the same identity.
    #[test]
    fn a_stored_key_reads_back() {
        let key = SecretKey::generate();
        let raw = key.to_bytes();
        assert_eq!(read_secret_key(&raw).unwrap().to_bytes(), raw);
    }

    /// A file that is not a key is an error rather than a reason to generate
    /// a new identity over the top of it.
    #[test]
    fn a_file_that_is_not_a_key_is_refused() {
        assert!(read_secret_key(b"").is_err());
        assert!(read_secret_key(b"nowhere near a key").is_err());
    }
}
