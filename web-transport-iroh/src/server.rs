use url::Url;

use crate::{Connect, ServerError, Session, Settings};

/// A QUIC-only WebTransport handshake, awaiting server decision.
pub struct QuicRequest {
    conn: iroh::endpoint::Connection,
}

/// An H3 WebTransport handshake, SETTINGS exchanged and CONNECT accepted,
/// awaiting server decision (respond OK / reject).
pub struct H3Request {
    conn: iroh::endpoint::Connection,
    settings: Settings,
    connect: Connect,
}

impl QuicRequest {
    /// Accept a new QUIC-only WebTransport session from a client.
    pub fn accept(conn: iroh::endpoint::Connection) -> Self {
        Self { conn }
    }

    pub fn conn(&self) -> &iroh::endpoint::Connection {
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
    pub async fn accept(conn: iroh::endpoint::Connection) -> Result<Self, ServerError> {
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

    pub fn conn(&self) -> &iroh::endpoint::Connection {
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
