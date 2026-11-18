# Crates and API docs

Most applications depend on `iroh-live` alone. It re-exports the node as
`iroh_live::node`, the media crates as `iroh_live::media`, moq-net as
`iroh_live::moq_net`, the value-track writer as `iroh_live::ValueWriter`, and
the rooms crate as `iroh_live::rooms` behind the `rooms` feature.

| Crate | What it is | Depends on iroh |
|---|---|---|
| [`iroh-live`](../../iroh-live) | The facade: `Calls`, `SubscriptionExt`, and the re-exports of the crates below | yes |
| [`iroh-live-node`](../../iroh-live-node) | The node: route table, publications and audiences, sessions, relay links, policies, link statistics, `BroadcastTicket`, `LiveLayout`, `EndpointConfig`, `MoqPreset` | yes |
| [`moq-transport-iroh`](../../moq-transport-iroh) | MoQ sessions over iroh connections: dial, accept, ALPN negotiation | yes |
| [`iroh-live-rooms`](../../iroh-live-rooms) | Rooms: gossip membership and members-only broadcasts. No media | yes |
| [`moq-ext-access`](../../moq-ext-access) | Grants, policies, token verifiers, the `Layout` trait and audiences | no |
| [`moq-ext-publisher`](../../moq-ext-publisher) | `LocalBroadcast`: encodes sources into a ladder of renditions | no |
| [`moq-ext-player`](../../moq-ext-player) | `RemoteBroadcast` and `Player`: rendition choice, A/V sync, recording input | no |
| [`moq-ext-adaptive`](../../moq-ext-adaptive) | Rendition choice and decoder handover as pure state machines | no |
| [`moq-ext-json`](../../moq-ext-json) | `ValueWriter`: a latest-value JSON track writer that stamps each update | no |
| [`moq-ext-sources`](../../moq-ext-sources) | Video and audio sources: test pattern, files, capture, push senders | no |
| [`moq-ext-egui`](../../moq-ext-egui) | egui video views and the debug overlay | no |
| [`iroh-live-cli`](../../iroh-live-cli) | The `irl` binary | yes |
| [`iroh-live-relay`](../../iroh-live-relay) | The iroh-live relay server | yes |

The `moq-ext-*` crates have no iroh dependency, so each could move into moq
upstream. [The architecture overview](../architecture/index.md) explains how
they fit together.

Capture, codecs and rendering come from moq's
[moq-video and moq-audio](https://doc.moq.dev/lib/rs/), which
`iroh_live::media::video` and `iroh_live::media::audio` re-export.

## API docs

The crates are not on crates.io yet, so docs.rs has no pages for them. The
documentation site hosts the [API docs](https://n0-computer.github.io/iroh-live/api/), built from `main` with every
feature on. Start at [`iroh_live`](https://n0-computer.github.io/iroh-live/api/iroh_live/). To build them from a
checkout:

```sh
cargo doc --workspace --all-features --no-deps --open
```

The crate-level docs of `iroh-live` list which futures are cancellation safe
and what dropping each one leaves behind.
