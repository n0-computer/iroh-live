# MoQ Transport Layer

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | stable |
| Applies to | iroh-moq |

iroh-moq connects iroh's QUIC endpoint to moq-lite broadcast primitives.
It handles connection lifecycle, broadcast routing, and session management,
giving moq-media a transport-agnostic boundary to publish and subscribe
through.

## Core types

`Moq` is the transport entry point. It wraps an iroh `Endpoint` and runs
an internal actor that manages connections, broadcast announcements, and
subscription routing. The actor deduplicates connections to the same peer
and routes subscription requests to the correct broadcast producer.

`MoqSession` represents a single peer connection. It holds the underlying
WebTransport session (via `web-transport-iroh`) and the moq-lite protocol
state. Each session supports both publishing local broadcasts and
subscribing to remote ones.

`MoqProtocolHandler` implements iroh's `ProtocolHandler` trait, registered
on a `Router` with the ALPN `moq-lite-03`. When the endpoint accepts an
incoming connection with this ALPN, the handler completes the MoQ
handshake and delivers the session to the actor.

## Broadcast model

Each broadcast has a name (a string) and contains named tracks. A track
is a sequence of groups, and each group starts with a keyframe. Groups
are ordered by sequence number; frames within a group are sequential.

On the publish side, `BroadcastProducer` (from moq-lite) owns the track
producers. moq-media's `LocalBroadcast` creates a `BroadcastProducer`,
builds a catalog track listing available renditions, and starts encoder
pipelines that write to `TrackProducer` via `MoqPacketSink`.

On the subscribe side, `BroadcastConsumer` (from moq-lite) receives track
data. moq-media's `RemoteBroadcast` wraps a `BroadcastConsumer`, watches
the catalog for available renditions, and creates `OrderedConsumer`
instances per track for the decoder pipelines.

## Session lifecycle

### Outgoing connections

`Moq::connect(remote)` initiates a QUIC connection, establishes a
WebTransport session, and completes the moq-lite client handshake. It
returns a `MoqSession`. If a connection to the same peer already exists,
the actor reuses it.

### Incoming connections

`Moq::incoming_sessions()` returns an `IncomingSessionStream`. Each
`IncomingSession` has already completed the MoQ handshake; the application
inspects the remote peer's identity and calls `accept()` or `reject()`.

The protocol handler accepts raw QUIC connections from the iroh Router,
promotes them to WebTransport sessions, runs the moq-lite server
handshake, and delivers the result to the actor. The actor notifies
the `IncomingSessionStream` via a broadcast channel.

## Publishing

`Moq::publish(name, producer)` registers a `BroadcastProducer` with the
actor. The actor announces the broadcast to all current and future
sessions. When a remote peer subscribes, the actor provides the
broadcast's consumer.

On a per-session level, `session.publish(name, consumer)` makes a
`BroadcastConsumer` available to the remote peer via the session's
`OriginProducer`.

## Subscribing

`session.subscribe(name)` waits for the remote peer to announce a
broadcast with the given name, then returns a `BroadcastConsumer`. If the
broadcast is already announced, it returns immediately. The call blocks
until the session closes if the name is never announced; callers that need
a timeout should wrap it in `tokio::time::timeout`.

## Connection access

`session.conn()` returns a reference to the underlying iroh `Connection`.
iroh-live uses this to poll path stats (RTT, loss, congestion window) for
network signal production, which feeds the adaptive rendition switching
algorithm in moq-media. See [adaptive.md](adaptive.md).

## Error handling

iroh-moq defines `Error` (connection and protocol failures) and
`SubscribeError` (track not announced, track closed, session closed) as
structured error types using `n0_error::stack_error`.
