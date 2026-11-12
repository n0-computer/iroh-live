//! moq-net's poll transport over an iroh connection.

use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Poll, ready},
    time::Duration,
};

use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{self, Connection, ConnectionError, ConnectionStats, PathStats},
};
use kio::{Bridge, Waiter};
use moq_net::transport::{
    Address, Wire,
    poll::{self, BiStreams, SendOptions, Ungrouped},
};
use n0_error::e;
use tracing::{debug, warn};

use crate::{ALPN, Error, RecvStream, SendStream, TransportError};

/// moq-net's poll transport over one iroh connection.
///
/// Implements [`moq_net::transport::poll::Session`] on iroh's QUIC connection
/// and streams directly. The request path and token travel in the MoQ SETUP,
/// since raw QUIC has no request URI.
///
/// What it can and cannot back:
///
/// - Streams order by their priority alone: iroh has no send groups, so
///   [`send_group`](poll::Session::send_group) returns `None`.
/// - [`wire`](poll::Session::wire) holds the UDP byte counts, refreshed from
///   the connection statistics whenever moq-net reads
///   [`metrics`](poll::Session::metrics).
/// - [`probe`](poll::Session::probe) is `None`: iroh's congestion controller
///   takes no probe target.
/// - [`receive_window`](poll::Session::receive_window) is `None`: iroh does not
///   report the receive windows in force. moq-net then never pauses a stream.
pub struct Transport {
    conn: Connection,
    // One slot per operation that may wait. iroh's futures for them borrow
    // the connection, so each slot stores one built from a clone of it, which
    // every task polling the operation shares.
    accept_uni: OpSlot<Result<endpoint::RecvStream, ConnectionError>>,
    accept_bi: OpSlot<Result<IrohBi, ConnectionError>>,
    open_uni: OpSlot<Result<endpoint::SendStream, ConnectionError>>,
    open_bi: OpSlot<Result<IrohBi, ConnectionError>>,
    recv_datagram: OpSlot<Result<Bytes, ConnectionError>>,
    closed: OpSlot<ConnectionError>,
    wire: Wire,
}

/// The halves of a bidirectional stream as iroh hands them out.
type IrohBi = (endpoint::SendStream, endpoint::RecvStream);

impl Transport {
    /// Wraps an incoming or outgoing `connection` that negotiated a MoQ ALPN.
    ///
    /// Hand the transport to [`moq_net::Server::accept_request`] for an
    /// accepted connection, or to [`moq_net::Client::connect`] for a dialed
    /// one. To dial with iroh's connect options, such as another
    /// [`Congestion`](crate::Congestion) controller for this one connection
    /// while the endpoint keeps its own for every other, connect through
    /// iroh and wrap the result:
    ///
    /// ```no_run
    /// # async fn run(endpoint: iroh::Endpoint, peer: iroh::EndpointId) -> Result<(), Box<dyn std::error::Error>> {
    /// use iroh::endpoint::ConnectOptions;
    /// use moq_transport_iroh::{ALPN, Congestion, Transport};
    ///
    /// let options = ConnectOptions::new().with_transport_config(Congestion::Cubic.transport_config());
    /// let connection = endpoint.connect_with_opts(peer, ALPN, options).await?.await?;
    /// let transport = Transport::new(connection)?;
    /// # drop(transport);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Fails with [`Error::UnsupportedAlpn`] if the connection negotiated an
    /// ALPN this build does not speak, which happens when a handler is mounted
    /// under an ALPN outside [`alpns`](crate::alpns).
    pub fn new(connection: Connection) -> Result<Self, Error> {
        let alpn = String::from_utf8_lossy(connection.alpn()).into_owned();
        let peer = connection.remote_id().fmt_short();
        if !moq_net::ALPNS.contains(&alpn.as_str()) {
            // Only a handler mounted under a wrong ALPN gets here.
            warn!(%alpn, %peer, "unsupported ALPN");
            return Err(e!(Error::UnsupportedAlpn { alpn }));
        }
        debug!(%alpn, %peer, side = ?connection.side(), "MoQ transport opened");
        Ok(Self {
            conn: connection,
            accept_uni: OpSlot::default(),
            accept_bi: OpSlot::default(),
            open_uni: OpSlot::default(),
            open_bi: OpSlot::default(),
            recv_datagram: OpSlot::default(),
            closed: OpSlot::default(),
            wire: Wire::new(),
        })
    }

    /// Dials `peer` and returns the MoQ transport over the connection.
    ///
    /// Offers only [`ALPN`], the newest MoQ version. iroh's
    /// [`Router`](iroh::protocol::Router) sorts the ALPNs it accepts, and TLS
    /// picks the first of the server's that the client offers, so offering
    /// older versions too would settle every dial on the oldest moq-lite. Hand
    /// the transport to [`moq_net::Client::connect`].
    ///
    /// Dropping the future abandons the dial.
    ///
    /// ```no_run
    /// # async fn run(endpoint: iroh::Endpoint, peer: iroh::EndpointId) -> Result<(), moq_transport_iroh::Error> {
    /// use moq_transport_iroh::Transport;
    ///
    /// let transport = Transport::connect(&endpoint, peer).await?;
    /// # drop(transport);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Connect`] if the dial fails, or with
    /// [`Error::UnsupportedAlpn`] if the peer negotiates an ALPN this build
    /// does not speak.
    pub async fn connect(
        endpoint: &Endpoint,
        peer: impl Into<EndpointAddr>,
    ) -> Result<Self, Error> {
        let connection = endpoint.connect(peer, ALPN).await.map_err(Error::connect)?;
        Self::new(connection)
    }

    /// Returns the iroh connection the transport runs over.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Starts an operation on a clone of the connection.
    fn start<T, F>(&self, op: fn(Connection) -> F) -> impl FnOnce() -> OpFuture<T> + '_
    where
        F: Future<Output = T> + Send + 'static,
    {
        move || Box::pin(op(self.conn.clone()))
    }
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transport")
            .field("peer", &self.conn.remote_id().fmt_short().to_string())
            .field("alpn", &String::from_utf8_lossy(self.conn.alpn()))
            .finish_non_exhaustive()
    }
}

impl poll::Session for Transport {
    type SendStream = SendStream;
    type RecvStream = poll::Buffered<RecvStream>;
    type Error = TransportError;

    fn poll_accept_uni(&self, waiter: &Waiter) -> Poll<Result<Self::RecvStream, TransportError>> {
        let start = self.start(|conn| async move { conn.accept_uni().await });
        poll_op(&self.accept_uni, waiter, start)
            .map_ok(RecvStream::buffered)
            .map_err(Into::into)
    }

    fn poll_accept_bi(&self, waiter: &Waiter) -> Poll<Result<BiStreams<Self>, TransportError>> {
        let start = self.start(|conn| async move { conn.accept_bi().await });
        poll_op(&self.accept_bi, waiter, start)
            .map_ok(|(send, recv)| (SendStream::new(send, 0), RecvStream::buffered(recv)))
            .map_err(Into::into)
    }

    fn poll_open_uni(
        &self,
        waiter: &Waiter,
        options: SendOptions<'_, Ungrouped>,
    ) -> Poll<Result<SendStream, TransportError>> {
        let start = self.start(|conn| async move { conn.open_uni().await });
        poll_op(&self.open_uni, waiter, start)
            .map_ok(|send| SendStream::new(send, options.priority))
            .map_err(Into::into)
    }

    fn poll_open_bi(
        &self,
        waiter: &Waiter,
        options: SendOptions<'_, Ungrouped>,
    ) -> Poll<Result<BiStreams<Self>, TransportError>> {
        let start = self.start(|conn| async move { conn.open_bi().await });
        poll_op(&self.open_bi, waiter, start)
            .map_ok(|(send, recv)| {
                (
                    SendStream::new(send, options.priority),
                    RecvStream::buffered(recv),
                )
            })
            .map_err(Into::into)
    }

    fn send_group(&self, _priority: i64) -> Option<Ungrouped> {
        None
    }

    fn send_datagram(
        &self,
        payload: Bytes,
        _options: SendOptions<'_, Ungrouped>,
    ) -> Result<(), TransportError> {
        self.conn.send_datagram(payload).map_err(Into::into)
    }

    fn poll_recv_datagram(&self, waiter: &Waiter) -> Poll<Result<Bytes, TransportError>> {
        let start = self.start(|conn| async move { conn.read_datagram().await });
        poll_op(&self.recv_datagram, waiter, start).map_err(Into::into)
    }

    fn max_datagram_size(&self) -> usize {
        self.conn.max_datagram_size().unwrap_or(0)
    }

    fn protocol(&self) -> Option<&str> {
        std::str::from_utf8(self.conn.alpn()).ok()
    }

    fn address(&self) -> Address {
        Address::Setup
    }

    fn close(&self, code: u32, reason: &str) {
        debug!(
            peer = %self.conn.remote_id().fmt_short(),
            code,
            reason,
            "closing the connection"
        );
        self.conn.close(code.into(), reason.as_bytes());
    }

    fn poll_closed(&self, waiter: &Waiter) -> Poll<TransportError> {
        let start = self.start(|conn| async move { conn.closed().await });
        poll_op(&self.closed, waiter, start).map(Into::into)
    }

    fn metrics(&self) -> impl moq_net::transport::Metrics {
        let stats = self.conn.stats();
        // moq-net reads the wire right after the metrics.
        self.wire.set_sent_bytes(stats.udp_tx.bytes);
        self.wire.set_received_bytes(stats.udp_rx.bytes);
        let path = self
            .conn
            .paths()
            .iter()
            .find(|path| path.is_selected())
            .map(|path| path.stats());
        Metrics { stats, path }
    }

    fn wire(&self) -> Option<Wire> {
        Some(self.wire.clone())
    }
}

/// A connection's statistics, and its selected path's.
struct Metrics {
    stats: ConnectionStats,
    path: Option<PathStats>,
}

impl moq_net::transport::Metrics for Metrics {
    fn bytes_lost(&self) -> Option<u64> {
        Some(self.stats.lost_bytes)
    }

    fn packets_sent(&self) -> Option<u64> {
        Some(self.stats.udp_tx.datagrams)
    }

    fn packets_received(&self) -> Option<u64> {
        Some(self.stats.udp_rx.datagrams)
    }

    fn packets_lost(&self) -> Option<u64> {
        Some(self.stats.lost_packets)
    }

    fn rtt(&self) -> Option<Duration> {
        self.path.map(|path| path.rtt)
    }

    /// Returns the congestion window over the round trip of the selected path.
    fn estimated_send_rate(&self) -> Option<u64> {
        let path = self.path?;
        let rtt = path.rtt.as_secs_f64();
        (path.cwnd > 0 && rtt > 0.0).then(|| (path.cwnd as f64 * 8.0 / rtt) as u64)
    }
}

/// A stored operation future.
pub(crate) type OpFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// An operation in flight, or `None` when idle, which every task polling it shares.
pub(crate) type OpSlot<T> = Bridge<Option<OpFuture<T>>>;

/// Polls the operation in `slot`, starting it first if the slot is idle.
///
/// Clears the slot once the operation resolves. The caller whose poll sees it
/// resolve takes the output, so a stream it yields is never lost, and the next
/// caller starts a new operation.
pub(crate) fn poll_op<T>(
    slot: &OpSlot<T>,
    waiter: &Waiter,
    start: impl FnOnce() -> OpFuture<T>,
) -> Poll<T> {
    slot.poll(waiter, |slot, cx| {
        let op = slot.get_or_insert_with(start);
        let output = ready!(op.as_mut().poll(cx));
        *slot = None;
        Poll::Ready(output)
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use iroh::endpoint::presets;
    use moq_net::transport::{
        CloseKind, Error as _,
        poll::{RecvStream as _, SendStream as _, Session as _},
    };

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Returns a dialed and an accepted transport, and the endpoints that drive them.
    ///
    /// The accepting endpoint speaks only `server_alpns`. With `client_alpn`
    /// the client connects with that ALPN instead of through [`dial`].
    async fn pair_with(
        client_alpn: Option<&[u8]>,
        server_alpns: Vec<&[u8]>,
    ) -> (Transport, Transport, [Endpoint; 2]) {
        let client = Endpoint::builder(presets::Minimal)
            .bind()
            .await
            .expect("bind client");
        let server = Endpoint::builder(presets::Minimal)
            .alpns(server_alpns.into_iter().map(<[u8]>::to_vec).collect())
            .bind()
            .await
            .expect("bind server");
        let incoming = async {
            let connection = server
                .accept()
                .await
                .expect("an incoming connection")
                .await
                .expect("handshake");
            Transport::new(connection).expect("a MoQ ALPN")
        };
        let outgoing = async {
            match client_alpn {
                Some(alpn) => Transport::new(
                    client
                        .connect(server.addr(), alpn)
                        .await
                        .map_err(Error::connect)?,
                ),
                None => Transport::connect(&client, server.addr()).await,
            }
        };
        let (dialed, accepted) =
            tokio::time::timeout(TIMEOUT, async { tokio::join!(outgoing, incoming) })
                .await
                .expect("connect in time");
        (dialed.expect("dial"), accepted, [client, server])
    }

    /// Returns a dialed and an accepted transport, the server speaking every MoQ ALPN.
    ///
    /// The server's ALPNs are sorted, as iroh's `Router` sorts them.
    pub(crate) async fn pair() -> (Transport, Transport, [Endpoint; 2]) {
        let mut alpns = crate::alpns();
        alpns.sort();
        pair_with(None, alpns).await
    }

    /// Runs `fut` to completion, failing the test after [`TIMEOUT`].
    async fn timely<F: Future>(what: &str, fut: F) -> F::Output {
        tokio::time::timeout(TIMEOUT, fut)
            .await
            .unwrap_or_else(|_| panic!("{what} timed out"))
    }

    /// Reads `recv` to its end.
    async fn read_to_end(recv: &mut poll::Buffered<RecvStream>) -> Result<Vec<u8>, TransportError> {
        let mut bytes = Vec::new();
        while let Some(chunk) = recv.read_chunk(usize::MAX).await? {
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// Opens a unidirectional stream and sends `payload` on it, so the peer can accept it.
    async fn open_with(transport: &Transport, payload: &'static [u8]) -> SendStream {
        let mut send = transport
            .open_uni(SendOptions::default())
            .await
            .expect("open uni");
        send.write_chunk(Bytes::from_static(payload))
            .await
            .expect("write");
        send
    }

    #[tokio::test]
    async fn dial_negotiates_the_newest_alpn() {
        let (client, server, _endpoints) = pair().await;
        assert_eq!(client.protocol(), std::str::from_utf8(ALPN).ok());
        assert_eq!(server.protocol(), client.protocol());
        assert_eq!(client.address(), Address::Setup);
    }

    #[tokio::test]
    async fn accept_refuses_a_foreign_alpn() {
        let client = Endpoint::builder(presets::Minimal)
            .bind()
            .await
            .expect("bind");
        let server = Endpoint::builder(presets::Minimal)
            .alpns(vec![b"other".to_vec()])
            .bind()
            .await
            .expect("bind");
        let incoming = async {
            server
                .accept()
                .await
                .expect("incoming")
                .await
                .expect("handshake")
        };
        let (dialed, accepted) = timely("connect", async {
            tokio::join!(client.connect(server.addr(), b"other"), incoming)
        })
        .await;
        let _client = dialed.expect("connect");
        assert!(matches!(
            Transport::new(accepted),
            Err(Error::UnsupportedAlpn { alpn, .. }) if alpn == "other"
        ));
    }

    #[tokio::test]
    async fn uni_stream_carries_bytes_and_fin() {
        let (client, server, _endpoints) = pair().await;
        let mut send = open_with(&client, b"hello ").await;
        send.write_chunk(Bytes::from_static(b"world"))
            .await
            .expect("write");
        send.finish().expect("finish");
        let mut recv = timely("accept", server.accept_uni()).await.expect("accept");
        let bytes = timely("read", read_to_end(&mut recv)).await.expect("read");
        assert_eq!(bytes, b"hello world");
        timely("acknowledgement", send.closed())
            .await
            .expect("acknowledged");
    }

    #[tokio::test]
    async fn bi_stream_carries_both_directions() {
        let (client, server, _endpoints) = pair().await;
        let (mut send, mut recv) = client
            .open_bi(SendOptions::default())
            .await
            .expect("open bi");
        send.write_chunk(Bytes::from_static(b"ping"))
            .await
            .expect("write");
        send.finish().expect("finish");
        let (mut reply, mut request) = timely("accept", server.accept_bi()).await.expect("accept");
        assert_eq!(
            timely("read", read_to_end(&mut request))
                .await
                .expect("read"),
            b"ping"
        );
        reply
            .write_chunk(Bytes::from_static(b"pong"))
            .await
            .expect("write");
        reply.finish().expect("finish");
        assert_eq!(
            timely("read", read_to_end(&mut recv)).await.expect("read"),
            b"pong"
        );
    }

    #[tokio::test]
    async fn open_bi_waits_for_credit_and_opens() {
        let (client, _server, _endpoints) = pair().await;
        let opened = kio::wait(|waiter| client.poll_open_bi(waiter, SendOptions::default()));
        timely("open", opened).await.expect("open");
    }

    #[tokio::test]
    async fn a_datagram_arrives() {
        let (client, server, _endpoints) = pair().await;
        assert!(client.max_datagram_size() > 0);
        client
            .send_datagram(Bytes::from_static(b"tick"), SendOptions::default())
            .expect("send");
        let datagram = timely("datagram", server.recv_datagram())
            .await
            .expect("receive");
        assert_eq!(datagram, Bytes::from_static(b"tick"));
    }

    #[tokio::test]
    async fn a_close_code_reaches_the_peer() {
        let (client, server, _endpoints) = pair().await;
        client.close(42, "bye");
        let remote = timely("remote close", server.closed()).await;
        assert_eq!(remote.session_error(), Some((42, "bye".to_owned())));
        let local = timely("local close", client.closed()).await;
        assert_eq!(local.close_kind(), CloseKind::Closed);
        assert_eq!(local.session_error(), None);
    }

    #[tokio::test]
    async fn a_stop_code_reaches_the_writer() {
        let (client, server, _endpoints) = pair().await;
        let send = open_with(&client, b"unwanted").await;
        let mut recv = timely("accept", server.accept_uni()).await.expect("accept");
        recv.stop(7);
        let err = timely("stop", send.closed()).await.expect_err("stopped");
        assert_eq!(err.stream_error(), Some(7));
    }

    #[tokio::test]
    async fn a_reset_code_reaches_the_reader() {
        let (client, server, _endpoints) = pair().await;
        let mut send = open_with(&client, b"partial").await;
        let mut recv = timely("accept", server.accept_uni()).await.expect("accept");
        send.reset(9);
        let err = timely("read", read_to_end(&mut recv))
            .await
            .expect_err("reset");
        assert_eq!(err.stream_error(), Some(9));
    }

    /// A writer that goes away mid-stream resets it, so the peer never reads it as complete.
    #[tokio::test]
    async fn dropping_an_unfinished_stream_resets_it() {
        let (client, server, _endpoints) = pair().await;
        let send = open_with(&client, b"cut off").await;
        let mut recv = timely("accept", server.accept_uni()).await.expect("accept");
        drop(send);
        let err = timely("read", read_to_end(&mut recv))
            .await
            .expect_err("reset");
        assert_eq!(err.stream_error(), Some(0));
    }

    #[tokio::test]
    async fn metrics_fill_the_wire_counters() {
        let (client, server, _endpoints) = pair().await;
        let mut send = open_with(&client, &[7; 4096]).await;
        send.finish().expect("finish");
        let mut recv = timely("accept", server.accept_uni()).await.expect("accept");
        timely("read", read_to_end(&mut recv)).await.expect("read");
        let metrics = client.metrics();
        let wire = client.wire().expect("a wire");
        assert!(wire.sent_bytes() >= 4096, "{}", wire.sent_bytes());
        assert!(wire.received_bytes() > 0);
        assert!(moq_net::transport::Metrics::packets_sent(&metrics).is_some_and(|sent| sent > 0));
        assert!(moq_net::transport::Metrics::rtt(&metrics).is_some());
        assert!(client.probe().is_none());
        assert!(client.receive_window().is_none());
    }

    /// Completes a MoQ handshake: the client over `client`, the server over `server`.
    ///
    /// Returns the handshake the server saw before admitting the session.
    async fn handshake(
        client: Transport,
        server: Transport,
        builder: moq_net::Client,
    ) -> (moq_net::Session, moq_net::server::Handshake) {
        let now = moq_net::time::Instant::now();
        // The client's driver writes the SETUP the server waits for, so it runs
        // alongside the accept.
        let connect = async {
            let (session, driver) = builder.connect(now, client).await.expect("connect");
            tokio::spawn(moq_net::time::run(driver));
            session
        };
        let accept = async {
            moq_net::Server::new()
                .accept_request(now, server)
                .await
                .expect("accept request")
        };
        timely("handshake", async { tokio::join!(connect, accept) }).await
    }

    /// The request path travels in the SETUP, since raw QUIC has no request URI.
    #[tokio::test]
    async fn the_setup_carries_the_request_path() {
        let (client, server, _endpoints) = pair().await;
        let url = url::Url::parse("moqt://iroh/room?jwt=abc").expect("url");
        let (_session, request) =
            handshake(client, server, moq_net::Client::new().with_url(url)).await;
        assert_eq!(request.path(), "/room?jwt=abc");
        let (_session, _driver) = timely("admit", request.ok()).await.expect("admit");
    }

    /// A version that carries a SETUP token hands the client's token to the server.
    #[tokio::test]
    async fn the_setup_carries_the_token() {
        let (client, server, _endpoints) = pair_with(Some(b"moqt-22"), vec![b"moqt-22"]).await;
        assert_eq!(client.protocol(), Some("moqt-22"));
        let token = moq_net::setup::Token {
            kind: moq_net::setup::Token::OUT_OF_BAND,
            value: b"letmein".to_vec(),
        };
        let builder = moq_net::Client::new().with_token(token.clone());
        let (_session, request) = handshake(client, server, builder).await;
        assert_eq!(request.token(), Some(&token));
    }
}
