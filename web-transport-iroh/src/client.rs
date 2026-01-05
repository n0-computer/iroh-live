use std::sync::Arc;

use iroh::{EndpointAddr, endpoint::ConnectOptions};
use quinn::TransportConfig;
use url::Url;

use crate::{ALPN_H3, ClientError, Session};

/// A client for connecting to a WebTransport server.
pub struct Client {
    endpoint: iroh::Endpoint,
    config: Arc<TransportConfig>,
}

impl Client {
    pub fn new(endpoint: iroh::Endpoint) -> Self {
        Self::with_transport_config(endpoint, Default::default())
    }

    /// Creates a client from an endpoint and a transport config.
    pub fn with_transport_config(
        endpoint: iroh::Endpoint,
        config: Arc<quinn::TransportConfig>,
    ) -> Self {
        Self { endpoint, config }
    }

    /// Connect to a server.
    pub async fn connect_quic(
        &self,
        addr: impl Into<EndpointAddr>,
        alpn: &[u8],
    ) -> Result<Session, ClientError> {
        let conn = self.connect(addr, alpn).await?;
        Ok(Session::raw(conn))
    }

    pub async fn connect_h3(
        &self,
        addr: impl Into<EndpointAddr>,
        url: Url,
    ) -> Result<Session, ClientError> {
        let conn = self.connect(addr, ALPN_H3.as_bytes()).await?;
        // Connect with the connection we established.
        Session::connect_h3(conn, url).await
    }

    async fn connect(
        &self,
        addr: impl Into<EndpointAddr>,
        alpn: &[u8],
    ) -> Result<iroh::endpoint::Connection, ClientError> {
        let opts = ConnectOptions::new().with_transport_config(self.config.clone());
        let conn = self
            .endpoint
            .connect_with_opts(addr, alpn, opts)
            .await
            .map_err(|err| ClientError::Connect(Arc::new(err.into())))?;
        let conn = conn
            .await
            .map_err(|err| ClientError::Connect(Arc::new(err.into())))?;
        Ok(conn)
    }
}
