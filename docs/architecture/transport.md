# Transport

`iroh-moq` binds an iroh `Endpoint` to a MoQ origin. It is the only crate in the
workspace that knows about both iroh and moq-net, and it is deliberately small:
one file, an actor for session lifetime, and the handshake.

For what MoQ itself is, read [the moq-lite layer
page](https://doc.moq.dev/concept/layer/moq-lite) and [the iroh transport
page](https://doc.moq.dev/concept/layer/iroh) upstream. This page covers what we
add on top.

## Publishing is node-wide

`Moq` owns one `moq_net::origin::Producer` for the whole endpoint.
`Moq::publish(path)` creates a broadcast on it and returns a
`moq_net::broadcast::Producer` synchronously. The broadcast is created with
`Route::new().with_announce(true)`, so every peer with a session discovers it
without asking for the path by name, and a session opened later picks it up on
its own.

There is no per-session publish. A moq-net session takes exactly one publisher
origin and the node origin is it. That is a change from the previous model, where
a broadcast was registered against each session and the actor kept
republish-on-connect bookkeeping. The bookkeeping is gone, and so is the failure
mode where two concurrent calls collided on one broadcast name: `Call` now
publishes under `calls/<endpoint id>` rather than a fixed `call`.

## Sessions

`Moq::connect(remote)` dials a peer and returns a `MoqSession`. The actor
deduplicates: a second `connect` to a peer we already have a session with returns
that session, and concurrent dials to the same peer coalesce onto one connect
rather than racing.

Incoming connections arrive through `MoqProtocolHandler`, which implements iroh's
`ProtocolHandler`. `Moq::incoming_sessions()` yields `IncomingSession` values
whose MoQ handshake has already completed, so an application can read
`remote_id()` and decide between `accept()` and `reject()`.

`MoqSession::subscribe(path)` waits for the peer to announce a broadcast at that
path and returns its consumer. It waits indefinitely if the announce never comes,
so a caller that needs a deadline wraps it in a timeout.

`MoqSession::conn()` exposes the iroh `Connection`. That is what iroh-live's
`spawn_signal_producer` and `spawn_stats_recorder` poll for path stats, which
feed [adaptive rendition switching](adaptive.md). `MoqSession::session()` exposes
the `moq_net::Session` for its own bandwidth estimates.

Both `MoqSession::connect` and `MoqSession::accept` return the session alongside
a `moq_net::Driver` that has to be polled for the session to make progress. The
actor joins each driver into the `JoinSet` it already owns, which gives shutdown
a single place to wait.

## ALPN negotiation

`iroh_moq::ALPN` is `moq_net::ALPNS[0]`, the newest MoQ version this build
speaks, so it tracks the moq-net dependency rather than a string someone has to
remember to bump. `iroh_moq::alpns()` returns the whole `moq_net::ALPNS` list
newest first, with HTTP/3 appended last.

Register all of them. `Live::register_protocols` mounts the handler once per
ALPN, and the dial offers the rest through
`ConnectOptions::with_additional_alpns`. A single hardcoded ALPN is an interop
bug that only appears once the two sides drift, which is when it is hardest to
diagnose.

HTTP/3 is last because WebTransport over H3 needs framing that not every H3
endpoint supports, so it is the fallback rather than the preference.

Both halves of the handshake branch on what was negotiated. Raw QUIC carries the
MoQ stream directly. H3 answers a CONNECT first: the client builds a
`web_transport_proto::ConnectRequest` listing every moq-lite ALPN as a protocol,
and the server replies `ConnectResponse::OK` echoing the first one requested. An
ALPN this build does not speak is `Error::UnsupportedAlpn`, a named error rather
than a session of the wrong shape.

`moq_native::iroh` already does all of this, and delegating to it stays on the
upstream wish list. It is not usable here today: `moq-native` has a mandatory
`clap` dependency, which is a poor thing to put in the dependency graph of a
transport library, and its `accept` and `connect` are `pub(crate)`, reachable
only through `Client` and `Server`, which want to own the endpoint and the accept
loop. An iroh application already owns both.

## Errors

`iroh_moq::Error` covers dial, handshake, and protocol failures, including
`UnsupportedAlpn`. `SubscribeError` has a single variant, `NotAnnounced`, for a
session that closed before the broadcast appeared. Both are `n0_error` stack
errors.
