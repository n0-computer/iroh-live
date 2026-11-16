# Relays

Three different things in iroh-live are called a relay. This page tells them
apart, and the rest of these docs always name which one they mean.

| Name | What it is | Who runs it |
|---|---|---|
| *iroh relay* | Forwards encrypted QUIC packets between two endpoints that cannot reach each other directly. Knows nothing about media | n0 runs public ones, and iroh uses them by default |
| *iroh-live relay* | The `iroh-live-relay` server. A MoQ relay that reads broadcasts over iroh and serves them to many viewers, browsers included | You |
| *relay link* | A node's open session with a MoQ relay, from `Node::attach_relay` | Your node |

## iroh relays

iroh connects two endpoints directly when it can. On one network they reach
each other over the local link, and hole punching opens a direct path through
most NATs. When that fails, the packets go through an iroh relay, which adds
a round trip. The application does not see the change, except as a different
round trip and bandwidth, which the player adapts to.

## The iroh-live relay

A browser cannot dial an iroh endpoint. The iroh-live relay accepts
WebTransport sessions from browsers and iroh sessions from nodes, and serves
both from one MoQ relay cluster. It gets a broadcast in one of two ways:

- **Pull.** A viewer asks for a broadcast by its ticket. The relay dials the
  publisher over iroh and subscribes. Nothing is needed on the publisher's
  side.
- **Push.** The publisher attaches a relay link to the relay, and the relay
  receives every publication the publisher's grant and audience allow.

Either way, many viewers share one upstream session to the publisher.

By default the iroh-live relay lets anyone read every broadcast on it. See
[Run the iroh-live relay](../how-to/run-relay.md) before you expose one.

## Relay links

`Node::attach_relay(RelayConfig::iroh(relay_id))` keeps a session with a MoQ
relay, and redials with backoff when it drops. It works with the iroh-live
relay over iroh, and with any MoQ relay over `https://`.

- The relay sees every publication with the `Granted` audience, and never a
  `Peers` one.
- By default the relay's routes join the node's route table, so the node can
  subscribe to broadcasts on the relay. A node that only publishes sets
  `RelayConfig::with_consume(false)`.
- A relay link presents tokens on every dial, from `RelayConfig::tokens`.

## What a relay can see

A MoQ relay reads the media it forwards: MoQ has no end-to-end encryption
here. An iroh relay sees only encrypted packets. Attach a relay link only to
a relay you trust with the broadcasts, and that keeps each publisher to its
own paths.

## Where to go next

- [Serve browsers](../how-to/browsers.md) and
  [Publish through a relay](../how-to/publish-through-relay.md) are the tasks.
- [Relay reference](../reference/relay.md) lists the server's flags.
- [Peer-to-peer and the relay](../architecture/p2p-relay.md) explains how
  pulls start, share and end.
