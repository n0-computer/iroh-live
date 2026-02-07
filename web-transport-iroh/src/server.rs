use std::fmt;

use iroh::endpoint::Connection;
use url::Url;

use crate::{Connect, ServerError, Session, Settings};

/// A QUIC-only WebTransport handshake, awaiting server decision.
pub struct QuicRequest {
    conn: Connection,
}

impl fmt::Debug for QuicRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuicRequest")
            .field("conn", &self.conn)
            .finish()
    }
}

/// An H3 WebTransport handshake, SETTINGS exchanged and CONNECT accepted,
/// awaiting server decision (respond OK / reject).
pub struct H3Request {
    conn: Connection,
    settings: Settings,
    connect: Connect,
}

impl fmt::Debug for H3Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("H3Request")
            .field("conn", &self.conn)
            .field("connect", &self.connect)
            .finish_non_exhaustive()
    }
}

impl QuicRequest {
    /// Accept a new QUIC-only WebTransport session from a client.
    pub fn accept(conn: Connection) -> Self {
        Self { conn }
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Accept the session.
    pub fn ok(self) -> Session {
        Session::raw(self.conn)
    }

    /// Reject the session.
    pub fn close(self, status: http::StatusCode) {
        self.conn
            .close(status.as_u16().into(), status.as_str().as_bytes());
    }
}

impl H3Request {
    /// Accept a new H3 WebTransport session from a client.
    pub async fn accept(conn: Connection) -> Result<Self, ServerError> {
        // Perform the H3 handshake by sending/receiving SETTINGS frames.
        let settings = Settings::connect(&conn).await?;

        // Accept the CONNECT request but don't send a response yet.
        let connect = Connect::accept(&conn).await?;

        Ok(Self {
            conn,
            settings,
            connect,
        })
    }

    /// Returns the URL provided by the client.
    pub fn url(&self) -> &Url {
        self.connect.url()
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Accept the session, returning a 200 OK.
    pub async fn ok(mut self) -> Result<Session, ServerError> {
        self.connect.respond(http::StatusCode::OK).await?;
        Ok(Session::new_h3(self.conn, self.settings, self.connect))
    }

    /// Reject the session, returning your favorite HTTP status code.
    pub async fn close(mut self, status: http::StatusCode) -> Result<(), ServerError> {
        self.connect.respond(status).await?;
        Ok(())
    }
}
