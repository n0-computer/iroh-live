//! The WebTransport layer under MoQ, for applications that drive moq-net
//! themselves.
//!
//! A [`Moq`](crate::Moq) node dials and accepts on its own, and most
//! applications never need this module. It exists for the ones that run
//! moq-net's client or server directly, such as a relay that decides what each
//! iroh peer may publish from its authenticated endpoint id: they need the same
//! ALPN negotiation and HTTP/3 handling as the node, and should not hand-roll a
//! second copy of it.
//!
//! An integration point: these functions return a
//! [`web_transport_iroh::Session`], so their signatures follow
//! web-transport-iroh's versioning, not this crate's.

use std::sync::Arc;

use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{ConnectOptions as IrohConnectOptions, Connection},
};
use n0_error::{AnyError, e};
use tracing::debug;

use crate::{ALPN, ConnectOptions, Error, alpns};

/// A dialed transport, and whether it went through HTTP/3.
pub(crate) struct Dialed {
    pub(crate) session: web_transport_iroh::Session,
    h3: bool,
}

impl Dialed {
    pub(crate) fn conn(&self) -> &Connection {
        self.session.conn()
    }

    pub(crate) fn is_h3(&self) -> bool {
        self.h3
    }
}

/// Dials `remote` and completes the WebTransport handshake.
///
/// Offers every version this build speaks rather than only the newest, so a
/// peer built against an older moq release still finds one in common, and
/// branches on what was actually negotiated: a raw QUIC session for a MoQ
/// ALPN, or an HTTP/3 CONNECT for WebTransport over H3. Cancellation safe:
/// dropping the future abandons the dial.
///
/// # Errors
///
/// Fails if the dial fails, or if the peer negotiates an ALPN this build does
/// not speak.
pub async fn dial(
    endpoint: &Endpoint,
    remote: impl Into<EndpointAddr>,
) -> Result<web_transport_iroh::Session, Error> {
    Ok(
        dial_with(endpoint, remote.into(), &ConnectOptions::default())
            .await?
            .session,
    )
}

/// Dials `remote` as [`dial`] does, with a token for an HTTP/3 CONNECT.
pub(crate) async fn dial_with(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    options: &ConnectOptions,
) -> Result<Dialed, Error> {
    let connect_error = |err: AnyError| {
        e!(Error::Connect {
            source: Arc::new(err)
        })
    };
    let others: Vec<Vec<u8>> = alpns()[1..].iter().map(|alpn| alpn.to_vec()).collect();
    let iroh_options = IrohConnectOptions::new().with_additional_alpns(others);
    let mut connecting = endpoint
        .connect_with_opts(remote, ALPN, iroh_options)
        .await
        .map_err(|err| connect_error(AnyError::from_std(err)))?;
    let alpn = connecting
        .alpn()
        .await
        .map_err(|err| connect_error(AnyError::from_std(err)))?;
    let alpn = String::from_utf8_lossy(&alpn).into_owned();
    let connection = connecting
        .await
        .map_err(|err| connect_error(AnyError::from_std(err)))?;
    debug!(%alpn, remote = %connection.remote_id().fmt_short(), "negotiated");
    if alpn == web_transport_iroh::ALPN_H3 {
        // The CONNECT target only has to identify the endpoint; iroh already
        // dialed a specific peer. A token rides the query, where an H3 server
        // looks for it.
        let mut url: url::Url = format!("https://{}/", connection.remote_id())
            .parse()
            .expect("an endpoint id is a valid host");
        if let Some(token) = &options.token {
            url.query_pairs_mut().append_pair("jwt", token);
        }
        let mut request = web_transport_proto::ConnectRequest::new(url);
        for alpn in moq_net::ALPNS {
            request = request.with_protocol(alpn.to_string());
        }
        let session = web_transport_iroh::Session::connect_h3(connection, request)
            .await
            .map_err(|err| connect_error(AnyError::from_std(err)))?;
        return Ok(Dialed { session, h3: true });
    }
    if !moq_net::ALPNS.contains(&alpn.as_str()) {
        return Err(e!(Error::UnsupportedAlpn { alpn }));
    }
    Ok(Dialed {
        session: web_transport_iroh::Session::raw(connection),
        h3: false,
    })
}

/// Completes the server half of the WebTransport handshake on `connection`.
///
/// The counterpart of [`dial`]. Returns the session and, for HTTP/3, the
/// CONNECT target (path and query); a raw session carries its path in the MoQ
/// setup instead. Cancellation safe: dropping the future drops the connection.
///
/// # Errors
///
/// Fails if the HTTP/3 exchange fails, or the connection negotiated an ALPN
/// this build does not speak.
pub async fn accept(
    connection: Connection,
) -> Result<(web_transport_iroh::Session, Option<String>), Error> {
    let (session, h3) = accept_transport(connection).await?;
    Ok((session, h3.map(|(target, _headers)| target)))
}

/// Completes the server half of the WebTransport handshake.
///
/// Returns the session and, for HTTP/3, the request target and headers. Raw
/// QUIC carries the MoQ stream directly and the target arrives in the MoQ
/// setup; H3 answers a CONNECT first, whose URL and headers are the request.
pub(crate) async fn accept_transport(
    connection: Connection,
) -> Result<
    (
        web_transport_iroh::Session,
        Option<(String, Vec<(String, String)>)>,
    ),
    Error,
> {
    let alpn = String::from_utf8_lossy(connection.alpn()).into_owned();
    let accept_error = |err: AnyError| {
        e!(Error::Connect {
            source: Arc::new(err)
        })
    };
    if alpn == web_transport_iroh::ALPN_H3 {
        let request = web_transport_iroh::H3Request::accept(connection)
            .await
            .map_err(|err| accept_error(AnyError::from_std(err)))?;
        let mut target = request.url.path().to_owned();
        if let Some(query) = request.url.query() {
            target.push('?');
            target.push_str(query);
        }
        let headers = request
            .headers
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
            })
            .collect();
        let mut response = web_transport_proto::ConnectResponse::OK;
        if let Some(protocol) = request.protocols.first() {
            response = response.with_protocol(protocol);
        }
        let session = request
            .respond(response)
            .await
            .map_err(|err| accept_error(AnyError::from_std(err)))?;
        return Ok((session, Some((target, headers))));
    }
    // The handler is mountable on any ALPN, so an unknown one is a named error
    // rather than a raw session that fails to parse a setup.
    if !moq_net::ALPNS.contains(&alpn.as_str()) {
        return Err(e!(Error::UnsupportedAlpn { alpn }));
    }
    Ok((web_transport_iroh::Session::raw(connection), None))
}
