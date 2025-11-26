use std::sync::Arc;

use iroh::{EndpointAddr, endpoint::ConnectOptions};
use quinn::TransportConfig;
use url::Url;

use crate::{ALPN, ClientError, Session};

/// A client for connecting to a WebTransport server.
pub struct Client {
    endpoint: iroh::Endpoint,
    config: Arc<TransportConfig>,
}

impl Client {
    /// Creates a client from an endpoint and a transport config.
    pub fn new(endpoint: iroh::Endpoint, config: Arc<quinn::TransportConfig>) -> Self {
        Self { endpoint, config }
    }

    /// Connect to a server.
    pub async fn connect(&self, addr: impl Into<EndpointAddr>) -> Result<Session, ClientError> {
        let addr = addr.into();
        let url: Url = format!("iroh://{}", addr.id).parse().unwrap();
        // Connect to the server using the addr we just resolved.
        let opts = ConnectOptions::new().with_transport_config(self.config.clone());
        let conn = self
            .endpoint
            .connect_with_opts(addr, ALPN.as_bytes(), opts)
            .await
            .map_err(Arc::new)?;
        let conn = conn.await.map_err(Arc::new)?;

        // Connect with the connection we established.
        Ok(Session::raw(conn, url))
    }
}
