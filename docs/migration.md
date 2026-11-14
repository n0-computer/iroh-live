# Migrating from the previous release

This version replaces the code on `main` that came before the port to moq's
`main` branch. Every library crate changes its public API, and nothing stays
compatible on the wire. This page lists what to change, from crate names to
behavior.

## Compatibility

- **No wire compatibility.** A node of this version cannot talk to a node of
  the previous one. Upgrade every node, the relay, and the Android app
  together.
- **Paths.** Broadcasts move from bare names, such as `hello`, to
  `live/<endpoint id>/<name>`. Rooms publish at
  `rooms/<topic>/<endpoint id>/<name>`.
- **Tickets.** `LiveTicket` (`iroh-live:...`) becomes `BroadcastTicket`
  (`ilv1:...`), which holds a full path. Room tickets have the new kind
  `room2`. Old tickets do not parse.
- **The moq dependency.** The workspace builds against moq's unreleased `main`
  branch, pinned by git rev, instead of a patched fork of the crates.io
  releases. iroh-live is a git dependency, see
  [Install](getting-started/install.md#use-the-library).
- **Features.** `iroh-live` has no default features. Ask for `full`, or pick
  from the [features reference](reference/features.md).

## Crates

| Before | Now |
|---|---|
| `iroh-moq` | `iroh-live-node` (the node) and `moq-transport-iroh` (MoQ sessions over iroh) |
| `iroh-rooms` | `iroh-live-rooms` |
| `moq-media` | `moq-ext-sources`, `moq-ext-publisher`, `moq-ext-player` and `moq-ext-adaptive` |
| `moq-media-egui` | `moq-ext-egui` |
| `moq-media-android` | The Android demo crate, `demos/android/rust` |
| (new) | `moq-ext-access`: grants, policies, token verifiers, layouts |

`iroh_live::media` still gathers the media crates, and `iroh_live::node` is
`iroh-live-node`. The thin wrappers over moq-audio, moq-video and hang are
gone, and the crates use the upstream types directly.

## Publish and subscribe

Before, `Live` bound the endpoint, owned the router, and published by name:

```rust,ignore
let live = Live::from_env().await?.with_router().spawn();
let broadcast = live.publish("hello")?;
broadcast.video().set(video::capture::Config::default())?;
println!("{}", LiveTicket::new(live.endpoint().id(), "hello"));

let sub = live.subscribe(ticket.endpoint, &ticket.broadcast_name).await?;
let tracks = sub.media().await;
```

Now the application binds the endpoint, creates a `Node`, mounts it on a
router it owns, and publishes through a layout:

```rust,ignore
let endpoint = EndpointConfig::from_env()?.bind().await?;
let layout = LiveLayout::default();
let node = Node::new(endpoint.clone(), layout.config());
let router = node.mount(Router::builder(endpoint)).spawn();
let broadcast: LocalBroadcast = layout.publish(&node, "hello")?;
broadcast.set_video(camera, VideoEncoding::single(VideoRendition::new("video")))?;
println!("{}", layout.ticket(node.id(), "hello")?);

let subscription = node.subscribe_ticket(&ticket).await?;
let player = subscription.remote_broadcast().play(PlayerConfig::default())?;
let mut frames = player.video_frames();
```

The [library quickstart](getting-started/library-quickstart.md) has both
sides as complete programs. A `Player` now owns decoding, rendition switches,
adaptation, A/V sync and stats, where the old code handed out one track per
medium.

## Renamed

| Before | Now |
|---|---|
| `Moq` | `Node` |
| `MoqSession` | `Session` |
| `Moq::protocol_handler` | `Node::mount(router_builder)` |
| `Moq::incoming_sessions`, `Incoming` | `Node::accepted()` |
| `MoqSession::dialed()` | `Session::direction()`, a `Direction` |
| `MoqSession::conn()` | `Session::connection()` |
| `LiveTicket` | `BroadcastTicket` |
| `LanPresence` | `EndpointConfig::mdns`, an `Mdns` |
| `VideoTrack`, `AudioTrack`, `MediaTracks` | `Player`, with `video_frames()` and `audio_frames()` |
| `RoomEvent`, `RoomEvents` | `Room::state()`, a watched `RoomState` |
| `LinkSample` | `NetworkSample` |
| `create_egui_wgpu_config` | `egui_wgpu_config` |

Names from development versions between the two releases:

| Before | Now |
|---|---|
| `EndpointOptions` | `EndpointConfig` |
| `media::remote_broadcast(&subscription)` | `subscription.remote_broadcast()`, from `media::SubscriptionExt` |
| `Audience::Everyone` | `Audience::Granted` |
| `RoomPeer` | `RoomMember` |
| `Player::video` | `Player::video_frames` |
| `Node::grant_for` | `Node::authorization_for` |
| `HungUp` | `HangUpReason` |
| `secret_key_file` | `load_or_create_secret_key` |
| `pull::PullState` | `pull::Pulls` |
| `dialed()` on a request | `direction()`, a `Direction` |
| `PlayerConfig { audio: bool, speaker }` | `PlayerConfig::audio`, an `AudioMode`, set with `with_audio` |
| `AudioSource::file(path, bool)` | `AudioSource::file(path, FileEnd)` |
| `RelayConfig::token`, `TokenFn`, `TokenSource` | `RelayConfig::tokens`, `RelayConfig::with_token`, and `RelayLink::tokens().set(..)` on a live link |

## Removed

| Removed | Use instead |
|---|---|
| `Live`, `LiveBuilder`, `BoundNode` | `EndpointConfig`, `Node`, and your own `Router` |
| `Calls::events` | `Calls::ringing()` |
| Room chat in the rooms crate | A broadcast with a JSON track, as `irl room` does |
| `GrantFn`, `Admission`, `Session::set_grant` | A `Policy`, see [Control access](how-to/control-access.md) |
| `Node::accept` | `Node::accepted()` |
| `RelayOffer`, `Transit` | Nothing. A node serves only its own publications |
| `RemoteBroadcast::record` | `RemoteBroadcast::mux_source` with a moq-mux export, see [Record](how-to/record.md) |
| `moq_transport_iroh::{MoqPreset, Congestion}` | `iroh_live_node::{MoqPreset, Congestion}`, also in `iroh_live` |

## Changed behavior

- **Access.** `NodeConfig::default()` lets every peer read and publish nothing.
  `LiveLayout::config()` lets each peer publish under its own id. A policy
  also decides the peers this node dials, so a policy that rejects a peer
  stops this node from dialing it. See [Access control](concepts/access.md).
- **Routes.** One route table takes every direct session and relay link. A
  subscription moves between a direct route and a relay route without ending.
- **Latency.** `PlayerConfig::latency` takes moq-net's `Delay`, and its `min`
  is now a floor. The playout delay follows the catalog's `delay` and the
  measured audio jitter above it.
- **Upstream types.** A `VideoRendition` holds moq-video's
  `encode::Config`, and `set_audio` takes moq-audio's `encode::Config`.
- **The relay.** It pulls tickets only for sessions whose token allows it, or
  with `--open-pulls`. Under `--auth-key`, browsers are decided by moq-auth's
  reference policy. See the [relay reference](reference/relay.md).
- **Config structs.** `EndpointConfig`, `NodeConfig`, `ConnectOptions`,
  `RelayConfig`, `PlayerConfig`, `VideoEncoding`, `VideoRendition` and
  `Adaptation` are `#[non_exhaustive]`. Build them from `Default` or a
  constructor, then use the `with_*` builders or set fields.
