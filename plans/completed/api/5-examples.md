# API Examples

Example code against the proposed API. These demonstrate the ergonomic
improvements and serve as a design validation tool.

---

## 1. One-to-many streaming (moq-media only, no iroh-live)

The simplest use case: publish a camera feed, subscribe from another peer.
No rooms, no calls — just raw broadcast/subscribe.

```rust
use moq_media::{Broadcast, Subscription, AudioBackend, VideoTrack};
use moq_media::codec::VideoCodec;
use moq_media::format::{AudioPreset, VideoPreset};

// --- Publisher ---

async fn publish(session: MoqSession) -> Result<()> {
    let camera = CameraCapturer::open(0)?;
    let audio = AudioBackend::new(Default::default());
    let mic = audio.default_input().await?;

    let broadcast = Broadcast::new();
    broadcast.video().set(camera, VideoCodec::H264, [VideoPreset::P720, VideoPreset::P360])?;
    broadcast.audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

    // Connect broadcast to transport — encoding starts here
    session.publish("my-stream", broadcast.producer());

    // Broadcast lives until dropped.
    tokio::signal::ctrl_c().await?;
    Ok(())
}

// --- Subscriber ---

async fn subscribe(session: MoqSession) -> Result<()> {
    // subscribe() waits for the peer to announce, new() waits for the catalog
    let consumer = session.subscribe("my-stream").await?;
    let sub = Subscription::new("my-stream", consumer).await?;

    // video() is sync — uses cached catalog, starts decoder thread immediately
    let mut video = sub.video()?;
    let audio_backend = AudioBackend::new(Default::default());
    let _audio = sub.audio(&audio_backend).await?;

    // Render loop — current_frame() returns the latest decoded frame
    loop {
        if let Some(frame) = video.current_frame() {
            render(frame);
        }
        tokio::time::sleep(Duration::from_millis(16)).await; // ~60fps
    }
}
```

## 2. One-to-one call (iroh-live)

```rust
use iroh_live::prelude::*;
use moq_media::codec::VideoCodec;
use moq_media::format::VideoPreset;

// --- Caller ---

async fn caller(live: Live) -> Result<()> {
    let call = live.call(remote_addr).await?;

    // Publish local media
    let camera = CameraCapturer::open(0)?;
    call.local().video().set(camera, VideoCodec::H264, [VideoPreset::P720])?;

    let audio = AudioBackend::new(Default::default());
    let mic = audio.default_input().await?;
    call.local().audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

    // Wait for remote to announce their media (async, not polling)
    let remote = call.remote_ready().await?;
    let mut video = remote.video()?;
    let _audio = remote.audio(&audio).await?;

    loop {
        if let Some(frame) = video.current_frame() {
            render(frame);
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
}

// --- Receiver ---

async fn receiver(live: Live) -> Result<()> {
    use futures::StreamExt;
    let mut incoming = live.incoming_calls();

    while let Some(incoming_call) = incoming.next().await {
        println!("Call from {}", incoming_call.remote_id());
        let call = incoming_call.accept().await?;

        // Publish our media
        let camera = CameraCapturer::open(0)?;
        call.local().video().set(camera, VideoCodec::H264, [VideoPreset::P720])?;

        // Subscribe to caller's media
        let remote = call.remote_ready().await?;
        let mut video = remote.video()?;
        // render...
    }

    Ok(())
}
```

## 3. Multi-party room (iroh-live)

```rust
use iroh_live::prelude::*;
use futures::StreamExt;
use moq_media::AudioBackend;
use moq_media::codec::VideoCodec;
use moq_media::format::VideoPreset;

async fn room_participant(live: Live, ticket: RoomTicket) -> Result<()> {
    let room = live.join_room(ticket).await?;
    let audio = AudioBackend::new(Default::default());

    // Publish local media
    let camera = CameraCapturer::open(0)?;
    room.local().broadcast().video().set(camera, VideoCodec::H264, [VideoPreset::P720])?;

    let mic = audio.default_input().await?;
    room.local().broadcast().audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

    // Handle room events via Stream
    let mut events = room.events();
    while let Some(event) = events.next().await {
        match event {
            RoomEvent::ParticipantJoined(participant) => {
                println!("{} joined", participant.id());
                // participant carries a full Subscription handle —
                // call video()/audio() to start receiving
                let video = participant.subscription().video()?;
                let audio = participant.subscription().audio(&audio).await?;
                // render video, play audio...
            }
            RoomEvent::ParticipantLeft { participant, .. } => {
                println!("{participant} left");
                // cleanup rendering resources
            }
            RoomEvent::TrackPublished { participant, kind, rendition } => {
                // Fires when a remote participant adds/changes a rendition
                // after initial join. Most apps can ignore this — the
                // Subscription handles rendition switching automatically.
                println!("{participant} published {kind:?} rendition {rendition}");
            }
            RoomEvent::TrackUnpublished { participant, kind, rendition } => {
                println!("{participant} unpublished {kind:?} rendition {rendition}");
            }
        }
    }

    Ok(())
}
```

## 4. Zero-transcode relay (moq-media)

Forward a broadcast verbatim — no decoding, no re-encoding. Useful for CDN
fan-out, geographic relaying, or re-broadcasting a room participant's stream
to external subscribers.

```rust
use moq_media::{Broadcast, Subscription};

/// Relay a broadcast from one session to another without decoding.
async fn relay(
    source_session: MoqSession,
    dest_session: MoqSession,
) -> Result<()> {
    let consumer = source_session.subscribe("concert-stream").await?;
    let sub = Subscription::new("concert-stream", consumer).await?;

    // relay() copies the catalog and forwards all track data verbatim
    let relay = Broadcast::relay(&sub)?;
    dest_session.publish("concert-relay", relay.producer());

    // Relay runs until sub or relay is dropped.
    sub.closed().await;
    Ok(())
}
```

## 5. Transcoding relay (moq-media)

Decode at one quality, re-encode at another. `VideoTrack` implements
`VideoSource`, so it plugs directly into `Broadcast` as an input.

```rust
use moq_media::{Broadcast, Subscription};
use moq_media::codec::VideoCodec;
use moq_media::format::VideoPreset;

/// Receive 1080p, re-encode as 360p for bandwidth-constrained viewers.
async fn transcode_relay(
    source_session: MoqSession,
    dest_session: MoqSession,
) -> Result<()> {
    let consumer = source_session.subscribe("hd-stream").await?;
    let sub = Subscription::new("hd-stream", consumer).await?;

    // Subscribe to the highest quality video — spawns a decoder thread
    let video = sub.video()?;

    // VideoTrack implements VideoSource, so it can be fed into a Broadcast
    // as if it were a camera. The broadcast encodes the decoded frames.
    let relay = Broadcast::new();
    relay.video().set(video, VideoCodec::H264, [VideoPreset::P360])?;

    dest_session.publish("sd-stream", relay.producer());

    sub.closed().await;
    Ok(())
}
```

## 6. Camera dashboard with VideoTarget (moq-media)

Multiple simultaneous subscriptions with quality constraints. Each subscription
spawns its own decoder thread — plan resources accordingly.

```rust
use moq_media::{Subscription, VideoTarget};
use moq_media::subscribe::VideoOptions;

/// Subscribe to multiple camera feeds at low quality for a dashboard.
/// 16 cameras = 16 decoder threads.
async fn dashboard(sessions: Vec<(MoqSession, &str)>) -> Result<()> {
    let mut tracks = Vec::new();

    for (mut session, name) in sessions {
        let consumer = session.subscribe(name).await?;
        let sub = Subscription::new(name, consumer).await?;

        // VideoTarget constrains rendition selection — picks the best
        // rendition that fits within 320×180 pixels
        let video = sub.video_with(
            VideoOptions::default()
                .target(VideoTarget::default().max_pixels(320 * 180)),
        )?;
        tracks.push((name.to_string(), video));
    }

    // Render all tracks in a grid
    loop {
        for (name, track) in &mut tracks {
            if let Some(frame) = track.current_frame() {
                render_tile(name, frame);
            }
        }
        tokio::time::sleep(Duration::from_millis(33)).await; // ~30fps
    }
}
```

## 7. Room relay to external audience

Join a room and re-broadcast each participant's media to external
subscribers. Uses zero-transcode relay for efficiency.

```rust
use iroh_live::prelude::*;
use futures::StreamExt;

/// Join a room and relay all participants to external subscribers.
async fn room_to_stream(live: Live, ticket: RoomTicket) -> Result<()> {
    let room = live.join_room(ticket).await?;
    let mut relays = HashMap::new();

    let mut events = room.events();
    while let Some(event) = events.next().await {
        match event {
            RoomEvent::ParticipantJoined(p) => {
                // Zero-transcode relay — forwards packets verbatim
                let relay = Broadcast::relay(p.subscription())?;
                let name = format!("room-{}-{}", room.id(), p.id());
                live.publish(&name, relay.producer()).await?;
                relays.insert(p.id(), relay);
            }
            RoomEvent::ParticipantLeft { participant, .. } => {
                relays.remove(&participant); // drop stops relay
            }
            _ => {}
        }
    }

    Ok(())
}
```

## 8. Source replacement (camera switch)

Switch cameras or temporarily disable video without tearing down the
broadcast. Subscribers see a seamless transition.

```rust
let broadcast = Broadcast::new();

// Start with front camera
let camera_front = CameraCapturer::open(0)?;
broadcast.video().set(camera_front, VideoCodec::H264, [VideoPreset::P720])?;

// ... later: switch to rear camera (same codec/presets, new source)
let camera_rear = CameraCapturer::open(1)?;
broadcast.video().replace(camera_rear)?;
// Subscribers see new keyframe, same rendition names — seamless

// Temporarily disable video (catalog still advertises it)
broadcast.video().set_enabled(false);
// ... later ...
broadcast.video().set_enabled(true);

// Mute/unmute audio independently
broadcast.audio().set_muted(true);
broadcast.audio().set_muted(false);
```

## 9. Recording pipeline with frames() stream

Use the `Stream` interface for ordered frame processing. Unlike
`current_frame()` (which skips to latest), `frames()` delivers every
frame in sequence — important for recording.

```rust
use moq_media::{Subscription, Quality};
use moq_media::subscribe::VideoOptions;
use futures::StreamExt;

/// Subscribe at highest quality and write all frames to disk.
async fn record(mut session: MoqSession) -> Result<()> {
    let consumer = session.subscribe("stream-to-record").await?;
    let sub = Subscription::new("stream-to-record", consumer).await?;

    // Quality::High → picks the highest-resolution rendition
    let mut video = sub.video_with(
        VideoOptions::default().quality(Quality::High),
    )?;

    let mut writer = mp4_writer::create("recording.mp4")?;

    // frames() returns impl Stream — use StreamExt for ergonomic iteration
    let mut frames = video.frames();
    while let Some(frame) = frames.next().await {
        writer.write_frame(&frame)?;
    }

    writer.finalize()?;
    Ok(())
}
```

## 10. Reactive participant list with watcher

Use the watcher pattern for UI that reacts to participant changes without
processing every event.

```rust
use iroh_live::prelude::*;

/// Render a participant grid that updates reactively.
async fn participant_grid(room: Room) -> Result<()> {
    let mut watcher = room.remote_participants_watcher();

    loop {
        // .get() returns the current snapshot (Vec<RemoteParticipant>)
        let participants = watcher.get();
        update_grid_layout(&participants);

        // .changed() waits until the list changes (join/leave)
        watcher.changed().await;
    }
}
```

## 11. Builder-based setup

```rust
use iroh_live::prelude::*;

async fn setup() -> Result<()> {
    // Builder pattern for Live — configure before spawning
    let live = Live::builder()
        .secret_key(SecretKey::generate())
        .gossip(true)
        .spawn()
        .await?;

    // Create room and get shareable ticket
    let room = live.join_room(RoomTicket::generate()).await?;
    let ticket = room.ticket();
    println!("Share this ticket: {ticket}");

    // Publish with simulcast (multiple renditions)
    let camera = CameraCapturer::open(0)?;
    room.local().broadcast().video().set(
        camera, VideoCodec::H264, [VideoPreset::P720, VideoPreset::P360]
    )?;

    Ok(())
}
```

## 12. Audio-only streaming (studio link)

Shows that video and audio are fully independent — no need to create
a video slot for an audio-only broadcast.

```rust
use moq_media::{Broadcast, Subscription, AudioBackend};
use moq_media::format::AudioPreset;

// --- Studio side: publish audio-only ---

async fn studio_publish(session: MoqSession) -> Result<()> {
    let audio = AudioBackend::new(Default::default());
    let source = audio.default_input().await?;

    let broadcast = Broadcast::new();
    // Only set audio — no video needed
    broadcast.audio().set(source, AudioCodec::Opus, [AudioPreset::Hq])?;

    assert!(!broadcast.has_video());
    assert!(broadcast.has_audio());

    session.publish("monitor-mix", broadcast.producer());
    tokio::signal::ctrl_c().await?;
    Ok(())
}

// --- Musician side: subscribe to audio-only ---

async fn musician_subscribe(mut session: MoqSession) -> Result<()> {
    let consumer = session.subscribe("monitor-mix").await?;
    let sub = Subscription::new("monitor-mix", consumer).await?;

    assert!(!sub.has_video());
    assert!(sub.has_audio());

    let audio = AudioBackend::new(Default::default());
    let track = sub.audio(&audio).await?;

    // Audio plays through speakers automatically via AudioSink.
    // Wait until the stream ends.
    track.stopped().await;
    Ok(())
}
```

## 13. Subscription status monitoring

Watch the broadcast lifecycle for connection status UI.

```rust
use moq_media::{Subscription, BroadcastStatus};

async fn monitor_status(sub: &Subscription) {
    let mut watcher = sub.status_watcher();

    loop {
        let status = watcher.get();
        match status {
            BroadcastStatus::Connecting => show_spinner(),
            BroadcastStatus::Live => show_live_indicator(),
            BroadcastStatus::Ended => {
                show_ended_message();
                break;
            }
        }
        watcher.changed().await;
    }
}
```

## 14. Publishing to a relay server

Publish through a moq-relay for fan-out to many subscribers.
The API is the same — only the session establishment differs.

```rust
use iroh_live::prelude::*;

async fn publish_via_relay(live: Live) -> Result<()> {
    let relay = RelayConfig::new("https://relay.example.com")
        .token("publisher-jwt-token");

    let broadcast = Broadcast::new();
    broadcast.video().set(camera, VideoCodec::H264, [VideoPreset::P720])?;
    broadcast.audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

    // publish_to_relay connects to the relay and announces the broadcast
    let _session = live.publish_to_relay(relay, "concert", broadcast.producer()).await?;

    // Keep alive
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn subscribe_via_relay(live: Live) -> Result<()> {
    let relay = RelayConfig::new("https://relay.example.com")
        .token("subscriber-jwt-token");

    let (_session, sub) = live.subscribe_from_relay(relay, "concert").await?;
    let mut video = sub.video()?;
    let audio = AudioBackend::new(Default::default());
    let _audio = sub.audio(&audio).await?;

    loop {
        if let Some(frame) = video.current_frame() {
            render(frame);
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
}
```

## 15. Room via relay (SFU-like)

Identical room API — relay is just a transport option.

```rust
use iroh_live::prelude::*;
use futures::StreamExt;

async fn room_via_relay(live: Live, ticket: RoomTicket) -> Result<()> {
    // The only difference from example 3: pass RoomOptions with relay config
    let room = live.join_room_with(
        ticket,
        RoomOptions::default().relay(
            RelayConfig::new("https://relay.example.com")
                .token("room-jwt-token"),
        ),
    ).await?;

    // Everything below is identical to the P2P room API
    let audio = AudioBackend::new(Default::default());
    room.local().broadcast().video().set(camera, codec, presets)?;
    room.local().broadcast().audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

    let mut events = room.events();
    while let Some(event) = events.next().await {
        if let RoomEvent::ParticipantJoined(p) = event {
            let video = p.subscription().video()?;
            let audio = p.subscription().audio(&audio).await?;
            // render...
        }
    }

    Ok(())
}
```

## 16. Error handling

Realistic error handling patterns.

```rust
use iroh_live::prelude::*;
use moq_media::SubscribeError;

async fn robust_subscribe(live: Live, addr: EndpointAddr) -> Result<()> {
    let mut session = live.connect(addr).await?;

    let consumer = match session.subscribe("stream").await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Broadcast not available: {e}");
            return Ok(());
        }
    };

    let sub = Subscription::new("stream", consumer).await?;

    // Check what media is available before subscribing
    if sub.has_video() {
        match sub.video() {
            Ok(video) => { /* render */ }
            Err(SubscribeError::DecoderFailed(e)) => {
                eprintln!("No decoder for this codec: {e}");
                // Audio-only fallback
            }
            Err(e) => return Err(e.into()),
        }
    }

    if sub.has_audio() {
        let audio = AudioBackend::new(Default::default());
        let _track = sub.audio(&audio).await?;
    }

    sub.closed().await;
    Ok(())
}
```

---

## Line Count Comparison

| Use case | Current API | Proposed API |
|---|---|---|
| Publish camera + mic | ~20 lines | ~6 lines |
| Subscribe to video | ~10 lines | ~3 lines |
| Join room + handle events | ~40 lines | ~20 lines |
| Zero-transcode relay | Not possible without custom code | ~5 lines |
| Camera switch | Rebuild VideoRenditions, re-set | ~2 lines |
| Dashboard (N cameras) | Manual rendition lookup per source | ~6 lines per source |
| Recording pipeline | Custom frame loop | ~8 lines with `frames()` stream |
| Accept incoming calls | Not possible | ~10 lines |
| Audio-only streaming | Same complexity as A/V | ~5 lines (no video boilerplate) |
| Publish via relay | Not possible | Same as P2P + 2 lines relay config |
| Room via relay | Not possible | Same as P2P room + 1 option |
