# Example Rewrites

This document rewrites the current examples against the proposed three-layer API.
The goal is not to show every option. It is to show what the common flows should
feel like once the redesign lands.

We explicitly call out async and sync boundaries because this matters a lot for:

- terminal examples
- GUI apps
- capture/render loops
- media callbacks

We skip egui- and gpui-specific widget details. The important part is where the
API crosses between async orchestration and synchronous rendering or device code.

See `3-sketch.md` for the proposed API surface, `3a-glossary.md` for term
definitions, and `4-impl.md` for the migration sequence.

## Design Goals For The Examples

Each example should make these things obvious:

- what layer of the API it uses
- what object owns what lifetime
- where media is published
- where media is subscribed
- where async orchestration ends
- where synchronous UI or rendering begins

If the examples still read like transport plumbing, the redesign has failed.

## Mapping From Current Examples

| Current example | Proposed primary layer | Proposed replacement style |
|---|---|---|
| `publish.rs` | Broadcast | publish a local broadcast and print a ticket |
| `watch.rs` | Broadcast | subscribe to a remote broadcast and render/play |
| `push.rs` | Raw + Broadcast | open raw session or remote broadcast for file push |
| `room-publish-file.rs` | Product or Broadcast | join room, publish file as a publication |
| `rooms.rs` | Product | join/create room, publish local media, subscribe to participants |
| `../iroh-live-apps/.../room.rs` | Product | UI shell around `Room` and publications |

## Example 1: Publish a Camera and Microphone

This replaces the current `iroh-live/examples/publish.rs`.

The current version builds:

- `Endpoint`
- `Router`
- `Live`
- `PublishBroadcast`
- `AudioRenditions`
- `VideoRenditions`

and then manually publishes the producer.

The new example should make the broadcast object the center.

```rust
use iroh_live::{
    Live,
    broadcast::BroadcastTicket,
    media::{
        audio_backend::AudioBackend,
        capture::CameraCapturer,
        format::{AudioCodec, AudioPreset, VideoCodec, VideoPreset},
    },
};
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let live = Live::builder().from_env().spawn().await?;
    let audio = AudioBackend::default();

    let broadcast = live.create_broadcast();

    broadcast.video().set_source(
        CameraCapturer::new()?,
        VideoCodec::H264,
        [VideoPreset::P180, VideoPreset::P720],
    )?;

    broadcast.audio().set_source(
        audio.default_input().await?,
        AudioCodec::Opus,
        [AudioPreset::Hq],
    )?;

    let ticket = live.publish_broadcast("hello", &broadcast)?;
    println!("broadcast ticket: {ticket}");

    tokio::signal::ctrl_c().await?;
    Ok(())
}
```

### Why this is better

- the example has one publishing object: `broadcast`
- local media configuration is grouped by media kind
- the ticket comes from the published broadcast, not from manual endpoint/name assembly

### Async/sync boundary

- `AudioBackend::default_input()` is async because device setup may block
- publishing is async because network publication is async
- capture and encoding run internally after setup; the example itself stays simple

## Example 2: Watch a Remote Broadcast

This replaces the current `iroh-live/examples/watch.rs`.

The current example has to coordinate:

- `LiveTicket`
- `Live`
- `watch_and_listen`
- `MoqSession`
- `SubscribeBroadcast`
- renderer setup

The new example should center on `RemoteBroadcast`.

```rust
use iroh_live::{
    Live,
    broadcast::{BroadcastTicket, SubscribeVideoOptions},
    media::{
        audio_backend::AudioBackend,
        codec::DefaultDecoders,
    },
};
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let ticket: BroadcastTicket = std::env::args().nth(1).unwrap().parse()?;
    let live = Live::builder().spawn().await?;
    let audio = AudioBackend::default();

    let broadcast = live.subscribe_broadcast(ticket).await?;

    let video = broadcast.subscribe_video_with::<DefaultDecoders>(
        SubscribeVideoOptions::default()
            .viewport(1280, 720),
    )?;

    let _audio = broadcast.subscribe_audio::<DefaultDecoders>(&audio).await?;

    run_viewer_loop(video)?;
    Ok(())
}
```

### Async/sync boundary

- subscribing and opening audio output are async
- decoded video consumption can then happen in a synchronous render loop
- the viewer loop should not own network/session orchestration if it does not need to

A simple synchronous render loop boundary:

```rust
fn run_viewer_loop(mut video: moq_media::subscribe::VideoTrack) -> Result<()> {
    loop {
        if let Some(frame) = video.current_frame() {
            render_frame(&frame);
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}
```

The important point is that network subscription is async setup. Frame rendering
is usually synchronous and UI-driven.

## Example 3: One-to-One Call With Camera and Screen Share

This is the new example that should replace a lot of today's direct-session usage.
It demonstrates multi-broadcast complexity: the caller publishes both camera and
screen, then subscribes to whatever the remote sends.

```rust
use iroh_live::{
    Live,
    CallTicket,
    BroadcastKind::{self, *},
    media::{
        audio_backend::AudioBackend,
        capture::{CameraCapturer, ScreenCapturer},
        codec::DefaultDecoders,
        format::{AudioCodec, VideoCodec, VideoPreset},
    },
};
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let ticket: CallTicket = std::env::args().nth(1).unwrap().parse()?;
    let live = Live::builder().from_env().spawn().await?;
    let audio = AudioBackend::default();

    let call = live.call(ticket).await?;

    // Publish camera + microphone.
    call.local(Camera).video().set_source(
        CameraCapturer::new()?,
        VideoCodec::H264,
        [VideoPreset::P720],
    )?;
    call.local(Camera).audio().set_source(
        audio.default_input().await?,
        AudioCodec::Opus,
        Default::default(),
    )?;

    // Publish screen-share (video only, no audio).
    call.local(Screen).video().set_source(
        ScreenCapturer::new()?,
        VideoCodec::H264,
        [VideoPreset::P1080],
    )?;

    // Subscribe to remote broadcasts as they appear.
    // The remote may send camera, screen, or both — handle dynamically.
    let call2 = call.clone();
    tokio::spawn(async move {
        let audio = AudioBackend::default();
        loop {
            let Ok((kind, broadcast)) = call2.recv_remote().await else {
                break;
            };
            println!("remote published: {kind}");

            if broadcast.has_video() {
                let video = broadcast.subscribe_video::<DefaultDecoders>().unwrap();
                // Hand off to UI for rendering...
            }
            if broadcast.has_audio() {
                let _audio = broadcast.subscribe_audio::<DefaultDecoders>(&audio).await.unwrap();
            }
        }
    });

    call.closed().await;
    Ok(())
}
```

### Why this matters

This example demonstrates real multi-broadcast complexity:

- Publishing two broadcasts (camera + screen) with one API pattern
- Dynamic subscription via `recv_remote()` — no need to know in advance what the remote will send
- `BroadcastKind` keeps it type-safe: the `match` on `kind` makes intent clear
- No `MoqSession`, no `subscribe()`, no raw announced broadcasts

### When `wait_remote` is better

If you know the remote will send exactly one camera broadcast, `wait_remote(Camera)`
is simpler:

```rust
let remote_cam = call.wait_remote(Camera).await?;
let video = remote_cam.subscribe_video::<DefaultDecoders>()?;
```

Use `recv_remote()` when you need to handle dynamic or unknown broadcast sets.

## Example 4: Accept an Incoming Call With Screen Share

This example exercises the inbound side with multi-broadcast handling.

```rust
use iroh_live::{
    Live,
    BroadcastKind::{self, *},
    media::{
        audio_backend::AudioBackend,
        capture::ScreenCapturer,
        codec::DefaultDecoders,
        format::{VideoCodec, VideoPreset},
    },
};
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let live = Live::builder().from_env().spawn().await?;
    let audio = AudioBackend::default();

    loop {
        let incoming = live.accept_call().await?;
        println!("incoming from {}", incoming.remote_participant());

        let call = incoming.accept().await?;

        // Publish our screen (no camera in this example — a presentation scenario).
        call.local(Screen).video().set_source(
            ScreenCapturer::new()?,
            VideoCodec::H264,
            [VideoPreset::P1080],
        )?;

        // Subscribe to all remote broadcasts as they arrive.
        loop {
            let Ok((kind, broadcast)) = call.recv_remote().await else {
                break; // Call ended.
            };

            match kind {
                Camera => {
                    if broadcast.has_audio() {
                        let _audio = broadcast
                            .subscribe_audio::<DefaultDecoders>(&audio)
                            .await?;
                    }
                    if broadcast.has_video() {
                        let _video = broadcast
                            .subscribe_video::<DefaultDecoders>()?;
                        // Hand off to renderer...
                    }
                }
                Screen => {
                    if broadcast.has_video() {
                        let _screen = broadcast
                            .subscribe_video::<DefaultDecoders>()?;
                        // Show in a separate screen-share panel...
                    }
                }
                BroadcastKind::Named(name) => {
                    println!("ignoring unknown broadcast: {name}");
                }
            }
        }
    }
}
```

### Why this matters

- Shows the callee publishing screen only (presentation mode)
- `recv_remote()` + match on `BroadcastKind` handles camera and screen distinctly
- Unknown broadcast kinds are handled gracefully
- Shows a real scenario where camera and screen need different UI treatment

### Async/sync boundary

- `live.accept_call().await` is async orchestration
- acceptance is async
- local device opening is async
- `recv_remote()` drives the subscription loop
- after subscription, playback is driven by internal media tasks

## Example 5: Join a Room and Publish Local Media

This replaces the current `iroh-live/examples/rooms.rs` setup pattern.

```rust
use iroh_live::{
    Live,
    RoomTicket,
    RoomEvent,
    BroadcastKind::{self, *},
    media::{
        audio_backend::AudioBackend,
        capture::{CameraCapturer, ScreenCapturer},
        format::{AudioCodec, AudioPreset, VideoCodec, VideoPreset},
    },
};
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let live = Live::builder().from_env().spawn().await?;
    let audio = AudioBackend::default();

    let room = match std::env::args().nth(1) {
        Some(ticket) => live.join_room(ticket.parse::<RoomTicket>()?).await?,
        None => live.join_room(RoomTicket::generate()).await?,
    };

    // Publish camera + microphone.
    room.local_participant().broadcast(Camera).video().set_source(
        CameraCapturer::new()?,
        VideoCodec::H264,
        [VideoPreset::P720],
    )?;
    room.local_participant().broadcast(Camera).audio().set_source(
        audio.default_input().await?,
        AudioCodec::Opus,
        [AudioPreset::Hq],
    )?;

    // Publish screen-share.
    room.local_participant().broadcast(Screen).video().set_source(
        ScreenCapturer::new()?,
        VideoCodec::H264,
        [VideoPreset::P1080],
    )?;

    println!("room ticket: {}", room.ticket());

    loop {
        match room.recv().await? {
            RoomEvent::ParticipantJoined(participant) => {
                println!("joined: {}", participant.id());
            }
            RoomEvent::BroadcastPublished { participant, kind, broadcast } => {
                println!("{} published {:?} (video={}, audio={})",
                    participant, kind, broadcast.has_video(), broadcast.has_audio());
            }
            RoomEvent::BroadcastUnpublished { participant, kind } => {
                println!("{} unpublished {:?}", participant, kind);
            }
            RoomEvent::ParticipantLeft { participant, .. } => {
                println!("left: {}", participant);
            }
        }
    }
}
```

### Why this is better

- the room owns room semantics
- local publishing of both camera and screen uses the same pattern
- remote activity is participant/broadcast-shaped
- all four event types are shown

## Example 6: Room Viewer / UI Shell

This is the most important rewrite for the GUI apps. We skip the widget details
and focus on ownership and boundary lines.

The current room UI has to:

- own a runtime
- hold `Room`
- react to `RoomEvent::BroadcastSubscribed`
- manually build remote track views from `(session, broadcast)`
- bridge async room/network state into synchronous UI state

The new pattern should be:

```rust
use iroh_live::BroadcastKind::{self, *};

struct App {
    room: iroh_live::Room,
    remotes: std::collections::HashMap<iroh_live::ParticipantId, ParticipantView>,
}

struct ParticipantView {
    camera_video: Option<moq_media::subscribe::VideoTrack>,
    camera_audio: Option<moq_media::subscribe::AudioTrack>,
    screen_video: Option<moq_media::subscribe::VideoTrack>,
}

async fn join_room(live: &iroh_live::Live, ticket: iroh_live::RoomTicket) -> n0_error::Result<App> {
    let room = live.join_room(ticket).await?;
    Ok(App {
        room,
        remotes: Default::default(),
    })
}

async fn pump_room_events(
    app: &mut App,
    audio: &iroh_live::media::audio_backend::AudioBackend,
) -> n0_error::Result<()> {
    loop {
        match app.room.recv().await? {
            iroh_live::RoomEvent::ParticipantJoined(participant) => {
                app.remotes.entry(participant.id()).or_insert_with(|| ParticipantView {
                    camera_video: None,
                    camera_audio: None,
                    screen_video: None,
                });
            }
            iroh_live::RoomEvent::BroadcastPublished { participant, kind, broadcast } => {
                let view = app.remotes.entry(participant).or_insert_with(|| ParticipantView {
                    camera_video: None,
                    camera_audio: None,
                    screen_video: None,
                });

                match kind {
                    Camera => {
                        if broadcast.has_video() && view.camera_video.is_none() {
                            view.camera_video = Some(
                                broadcast.subscribe_video::<moq_media::codec::DefaultDecoders>()?
                            );
                        }
                        if broadcast.has_audio() && view.camera_audio.is_none() {
                            view.camera_audio = Some(
                                broadcast.subscribe_audio::<moq_media::codec::DefaultDecoders>(audio).await?
                            );
                        }
                    }
                    Screen => {
                        if broadcast.has_video() && view.screen_video.is_none() {
                            view.screen_video = Some(
                                broadcast.subscribe_video::<moq_media::codec::DefaultDecoders>()?
                            );
                        }
                    }
                    BroadcastKind::Named(_) => {
                        // Custom broadcasts: skip or handle per-application logic.
                    }
                }
            }
            iroh_live::RoomEvent::BroadcastUnpublished { participant, kind } => {
                if let Some(view) = app.remotes.get_mut(&participant) {
                    match kind {
                        Camera => {
                            view.camera_video = None;
                            view.camera_audio = None;
                        }
                        Screen => {
                            view.screen_video = None;
                        }
                        _ => {}
                    }
                }
            }
            iroh_live::RoomEvent::ParticipantLeft { participant, .. } => {
                app.remotes.remove(&participant);
            }
        }
    }
}

fn render(app: &mut App) {
    for (id, view) in &mut app.remotes {
        // Camera tile.
        if let Some(video) = view.camera_video.as_mut() {
            if let Some(frame) = video.current_frame() {
                draw_camera_tile(id, &frame);
            }
        }
        // Screen-share tile (typically larger, separate layout).
        if let Some(video) = view.screen_video.as_mut() {
            if let Some(frame) = video.current_frame() {
                draw_screen_tile(id, &frame);
            }
        }
    }
}
```

### Async/sync boundary

This boundary is the one we should preserve across all native GUI integrations:

- async side:
  - join room
  - receive room events
  - subscribe/unsubscribe based on `BroadcastKind`
  - open devices
  - network/media orchestration
- sync side:
  - draw current video frame (camera and screen separately)
  - handle UI events
  - update layout

The product API should make that bridge easy by supporting:

- cheap cloneable handles
- synchronous `current_frame()` access for rendering
- async subscription setup

### Why screen-share matters here

The `BroadcastKind` match in the event handler is the key pattern. Camera and screen
broadcasts arrive through the same event stream but need different UI treatment:
camera tiles are small and per-participant, screen-share tiles are large and typically
shown one at a time. The API makes this distinction natural without extra plumbing.

## Example 7: Push a File Into a Remote Peer

This replaces the spirit of `push.rs`.

This is intentionally an advanced example, because pushing pre-encoded media
should remain close to the broadcast/raw layers.

```rust
use iroh_live::{
    Live,
    broadcast::BroadcastTicket,
};
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let live = Live::builder().from_env().spawn().await?;

    let target: BroadcastTicket = std::env::args().nth(1).unwrap().parse()?;
    let remote = live.subscribe_broadcast(target).await?;

    let local = live.create_broadcast();
    publish_mp4_file_into(local.video(), "input.mp4").await?;
    let _ticket = live.publish_broadcast("file", &local)?;

    let _ = remote;
    Ok(())
}
```

### Why this stays advanced

This workflow is not a normal call or room interaction.
It is exactly the kind of case where the broadcast layer should shine.

## Example 8: Publish a File Into a Room

This replaces `room-publish-file.rs`.

```rust
use iroh_live::{Live, RoomTicket, BroadcastKind::*};
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let live = Live::builder().from_env().spawn().await?;
    let room = live.join_room(RoomTicket::generate()).await?;

    room.local_participant().broadcast(Camera).video().set_source(
        file_video_source("clip.mp4")?,
        iroh_live::media::format::VideoCodec::H264,
        [iroh_live::media::format::VideoPreset::P720],
    )?;

    println!("room ticket: {}", room.ticket());
    tokio::signal::ctrl_c().await?;
    Ok(())
}
```

This example should be simple because file-as-source should look like any other
source at the API level.

## Example 9: Publish a File and Watch It Elsewhere

This is the simplest "source -> network -> viewer" workflow. No camera, no screen,
no room. Just a broadcast with a file source on one side and a subscriber on the other.

### Publisher side

```rust
use iroh_live::Live;
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let live = Live::builder().from_env().spawn().await?;

    let broadcast = live.create_broadcast();
    broadcast.video().set_source(
        file_video_source("concert.mp4")?,
        iroh_live::media::format::VideoCodec::H264,
        [iroh_live::media::format::VideoPreset::P720],
    )?;

    let ticket = live.publish_broadcast("concert", &broadcast)?;
    println!("{ticket}");

    tokio::signal::ctrl_c().await?;
    Ok(())
}
```

### Subscriber side

```rust
use iroh_live::{
    Live,
    broadcast::BroadcastTicket,
    media::codec::DefaultDecoders,
};
use n0_error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let ticket: BroadcastTicket = std::env::args().nth(1).unwrap().parse()?;
    let live = Live::builder().spawn().await?;

    let broadcast = live.subscribe_broadcast(ticket).await?;
    let mut video = broadcast.subscribe_video::<DefaultDecoders>()?;

    loop {
        if let Some(frame) = video.current_frame() {
            render_frame(&frame);
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}
```

### Why this matters

This is the broadcast layer doing what it was designed for: no rooms, no
participants, no call semantics. Just publish a media source and let someone
else watch it. The `file_video_source` is any `impl VideoSource` — the API
doesn't care whether it's a file, a camera, or a test pattern.

## Example Style Rules

Once the redesign lands, examples should follow these rules:

1. The first screenful should reveal the layer in use.
2. Product examples should not mention `MoqSession`, `BroadcastProducer`, or raw track names.
3. Broadcast examples may mention broadcasts and catalog, but should not require raw producer/consumer handling unless the point is raw transport.
4. Raw examples should say clearly that they are raw/advanced.
5. UI examples should identify the async/sync boundary explicitly.
6. Every example should make ownership and cleanup obvious.
7. Multi-broadcast examples (camera + screen) should show the `BroadcastKind` pattern and how different broadcast kinds receive different UI/logic treatment.

## What Success Looks Like

We should know the redesign worked if:

- `publish` becomes a broadcast example, not a transport example
- `watch` becomes a broadcast example, not a tuple-of-session-and-track example
- `rooms` becomes a participant/broadcast example, not a gossip+subscription example
- UI examples stop reconstructing participant state from sessions manually
- multi-broadcast (camera + screen) feels natural, not bolted on
- file and pipeline examples still feel direct, not boxed into room/call abstractions
- a "publish file, watch elsewhere" example uses the broadcast layer directly and feels as natural as camera/microphone examples
