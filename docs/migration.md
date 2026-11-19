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
| `iroh_live::Error::SecretKey`, `iroh_live::Error::Bind` | `EndpointError::SecretKey`, `EndpointError::Bind`, inside `iroh_live::Error::Endpoint` |
| `iroh_live::layout::LiveLayout` and the `layout` module | `iroh_live_node::layout`, still re-exported as `iroh_live::layout` |
| `iroh_live::{EndpointConfig, Mdns, load_or_create_secret_key}` | The same names in `iroh_live_node`, still re-exported from `iroh_live` |
| `iroh_live_node::Layout` | `iroh_live_node::layout::Layout` |
| `media::*` from every media crate | `media` lists its re-exports. Reach the rest through `moq_ext_player`, `moq_ext_publisher` and `moq_ext_sources` |
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
| `Player::video_timing(Duration)` | `Player::video_timing(Timestamp)`, with the frame's `timestamp` as it is |
| `Subscription::value_track` returning a `json::snapshot::Consumer` | A `ValueReader` with the same `latest()` and `next()`, which follows a restarted publisher |

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
| `PlayerConfig::record_arrivals`, `with_recorded_arrivals` | Nothing. The decoder reports each picture's arrival, so `FrameTiming::arrived` is always filled |

## Changed behavior

- **Errors.** `EndpointConfig::from_env` and `bind` return an
  `EndpointError`, which converts into `iroh_live::Error::Endpoint`. The
  facade's error carries its metadata in every variant, so a match names it:

  ```rust,ignore
  match err {
      iroh_live::Error::Endpoint(EndpointError::Bind { .. }, _) => { /* port in use */ }
      other => return Err(other),
  }
  ```

  The relay no longer depends on `iroh-live`, only on `iroh-live-node` and
  `iroh-live-rooms`.

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
  reference policy. See the [relay reference](reference/relay.md) and
  [Relay access modes](#relay-access-modes) below.
- **Value tracks.** `Node::publish_value` and `LocalBroadcast::value_track`
  return a `ValueWriter<T>`. `update(&value)` stamps the value with the time
  it is made and keeps the stamps increasing, so remove any
  `Timed::new(..., Timestamp::from(Instant::now()))` wrapper and any stamp
  counter of your own. Use `update_at(&value, at)` to choose the time
  yourself. The writer finishes the track when its last clone drops, so a
  drop guard that calls `finish` is no longer needed. `producer()` returns
  the moq-json producer for anything else. Every frame moq-net writes now
  carries a time: `write_frame` takes a `Timestamp`, and `Timestamp::now()`
  is gone.
- **Config structs.** `EndpointConfig`, `NodeConfig`, `ConnectOptions`,
  `RelayConfig`, `PlayerConfig`, `VideoEncoding`, `VideoRendition` and
  `Adaptation` are `#[non_exhaustive]`. Build them from `Default` or a
  constructor, then use the `with_*` builders or set fields.

## Relay access modes

The relay is open without auth flags, and private with any `--auth-*` or
`--rcan-*` flag. A private relay gives sessions without a token only the
`--auth-public*` patterns, or refuses them. See the
[relay reference](reference/relay.md#access).

| Before | Now |
|---|---|
| `--auth-key` alone: anonymous sessions read everything, tokens add rights | Anonymous sessions are refused. For the old rules, add `--auth-public-subscribe '**' --auth-public-publish '*' --auth-public-own-paths` |
| `--auth-public*` without a key bound browsers only, and iroh clients read everything | The patterns bind browsers and iroh clients alike |
| `--auth-url` alone: anonymous iroh clients read everything | They are refused. Add `--auth-key` or `--rcan-principal` to admit iroh clients with tokens |
| `--auth-key` with `--auth-url` or `--auth-public*` was a parse error | It works: the key verifies tokens, and the patterns are the public area |
| A verified iroh token's claims replaced the client's own paths | Unchanged by default. `--auth-own-paths` adds them |
| Tokens without `exp` were accepted | A private relay refuses them, unless `--allow-unexpiring-tokens` |
| A token session lasted until its token expired | It lasts at most `--max-session` (24 hours). At that bound the relay sends a GOAWAY, and the client dials a new session with the same token. It closes when the key file stops verifying its token |
| `--open-pulls` let any session pull | Only a session the relay admits to read the ticket's path. On an open relay that is still every session |
| An iroh session that named a ticket was decided at the ticket's string | It reads the ticket if its token reads the ticket's path |
| The viewer ignored `?jwt=` | It reads `#jwt=`, and `?jwt=` for older links, and sends the token only to its own origin |

For embedders and node code:

- `iroh_live_relay::Config` is `#[non_exhaustive]` and implements `Default`.
  Build it with `Config::default()` and set fields. A config that sets
  `iroh_policy` without auth settings needs `Config::access`.
- `pull::Access` is gone. `Pulls::with_open_pulls(true)` replaces
  `with_access(Access::Open)`, and the default stays the old
  `Access::Authorized`. `Pulls::open_pulls` replaces `Pulls::access`. The
  relay's mode is `AccessMode`, `AccessMode::Open` or `AccessMode::Private`,
  set through `Config::access`.
- `IrohSessions::new` refuses every session until it gets a policy.
  `admit` takes a `BrowserPolicy`, from `Config::policies()`.
- `RelayStatus` is `#[non_exhaustive]` and has `Refused(Refusal)`. A match
  over it needs a wildcard arm.
- A relay handshake refused with `Expired` is no longer retried as a failed
  dial.
- `encode_chain` is gone from `policy`. Build a chain with `Chain::issue` and
  `Chain::delegate`, and encode it with `Chain::to_token`. Parse a token with
  `str::parse::<Chain>`, which fails with a `ChainError`. `policy::Expires`
  re-exports rcan's expiry, so the `rcan` crate is no longer a dependency of
  your code.
