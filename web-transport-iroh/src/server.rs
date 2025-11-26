use std::sync::Arc;

use iroh::EndpointId;
use n0_future::{StreamExt, boxed::BoxFuture};
use url::Url;

use crate::{ServerError, Session};

/// A WebTransport server that accepts new sessions.
pub struct Server {
    endpoint: iroh::Endpoint,
    accept: n0_future::FuturesUnordered<BoxFuture<Result<Request, ServerError>>>,
}

impl Server {
    /// Creates a new server with a manually constructed [`Endpoint`].
    pub fn new(endpoint: iroh::Endpoint) -> Self {
        Self {
            endpoint,
            accept: Default::default(),
        }
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// Accept a new WebTransport session Request from a client.
    pub async fn accept(&mut self) -> Option<Request> {
        loop {
            tokio::select! {
                res = self.endpoint.accept() => {
                    let conn = res?;
                    self.accept.push(Box::pin(async move {
                        let conn = conn.await.map_err(Arc::new)?;
                        Request::accept(conn).await
                    }));
                }
                Some(res) = self.accept.next() => {
                    if let Ok(session) = res {
                        return Some(session)
                    }
                }
            }
        }
    }
}

/// A mostly complete WebTransport handshake, just awaiting the server's decision on whether to accept or reject the session based on the URL.
pub struct Request {
    conn: iroh::endpoint::Connection,
    url: Url,
}

impl Request {
    /// Accept a new WebTransport session from a client.
    pub async fn accept(conn: iroh::endpoint::Connection) -> Result<Self, ServerError> {
        let url: Url = format!("iroh://{}", conn.remote_id()).parse().unwrap();
        // Return the resulting request with a reference to the settings/connect streams.
        Ok(Self { url, conn })
    }

    /// Returns the URL provided by the client.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Accept the session, returning a 200 OK.
    pub async fn ok(self) -> Result<Session, quinn::WriteError> {
        Ok(Session::raw(self.conn, self.url))
    }

    /// Reject the session, returing your favorite HTTP status code.
    pub async fn close(self, status: http::StatusCode) -> Result<(), quinn::WriteError> {
        self.conn
            .close(status.as_u16().into(), status.as_str().as_bytes());
        Ok(())
    }
}
