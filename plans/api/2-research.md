# API Research: Real-Time Media Toolkit Survey

Survey of existing real-time media APIs — what to steal, what to avoid.

Framing: most streaming toolkit APIs are actually bad. Verbose, callback-heavy,
leaky abstractions, ceremony-laden. We want to be better. This document calls
out both the good and the bad explicitly.

---

## 1. LiveKit (JS/TS SDK + Rust SDK)

The most popular open-source RTC toolkit. The JS SDK is the primary reference
because it's what app developers actually use day-to-day.

### Core Object Model

- `Room` — central object, represents a connected session
- `LocalParticipant` / `RemoteParticipant` — identity + track container
- `LocalTrackPublication` / `RemoteTrackPublication` — a published track
- `LocalTrack` / `RemoteTrack` — the actual media (audio/video)
- `TrackPublication` — the join point between track and room
- `RoomEvent` — enum of everything that can happen

### Construction / Connection

```ts
const room = new Room(options);
await room.connect(url, token);
// or: await room.connect(url, token, { autoSubscribe: true });
```

- Token-based auth (JWT), URL-based connection
- Options bag at construction, connect options separate
- `Room` exists before connection — you can attach event listeners first

### Publish Pattern

```ts
const tracks = await createLocalTracks({ audio: true, video: true });
await room.localParticipant.publishTrack(tracks[0]);
await room.localParticipant.publishTrack(tracks[1]);
```

- Capture and publish are separate steps (good)
- `publishTrack()` returns a `LocalTrackPublication`
- Can set encoding options, simulcast layers at publish time
- Unpublish by calling `unpublishTrack()` or dropping the publication

### Subscribe Pattern

```ts
room.on(RoomEvent.TrackSubscribed, (track, publication, participant) => {
  if (track.kind === Track.Kind.Video) {
    const element = track.attach();
    container.appendChild(element);
  }
});
```

- Subscription is automatic by default (`autoSubscribe: true`)
- Manual subscribe available via `publication.setSubscribed(true)`
- Track attachment to DOM is explicit (`track.attach()`)
- Detach on unsubscribe to avoid leaks

### Event Model

- Node.js `EventEmitter` pattern: `room.on(RoomEvent.Foo, callback)`
- ~30+ event types on Room alone
- Events carry relevant objects as callback args
- Participant-level events too: `participant.on(ParticipantEvent.Foo, ...)`

### What LiveKit Gets RIGHT

- **Stable object model**: `Room`, `Participant`, `TrackPublication` are long-lived,
  cheaply referenceable objects. You grab them once and hold on.
- **Participant-centric**: everything routes through participants, not raw streams
- **Auto-subscribe default**: most apps want all tracks, opt out for advanced cases
- **Publication as indirection**: track publication != track. You can see a publication
  exists before the track data arrives. Great for UI (show placeholder).
- **`Room` exists before connect**: attach listeners, then connect. No race conditions.

### What LiveKit Gets WRONG

- **Event soup**: 30+ event types, many redundant. `TrackSubscribed` vs
  `TrackPublished` vs `ParticipantConnected` — apps need 5-10 event handlers
  just to render a basic grid. This is the #1 complaint from LiveKit users.
- **Callback hell**: event-based API means lots of `room.on(...)` with closures
  that capture mutable state. In Rust this would be lifetime/borrow nightmares.
- **String-typed track sources**: `Track.Source.Camera`, `Track.Source.Microphone` —
  stringly typed, easy to typo, not extensible for custom sources.
- **Simulcast config is publish-time only**: can't change encoding parameters after
  publishing without republishing.
- **Rust SDK is a thin wrapper**: the Rust SDK mostly mirrors the JS API shape,
  which doesn't feel idiomatic. Callback-heavy, not stream/watcher-based.

---

## 2. Hang JS (local: `../moq/js/`)

The MoQ-native JS API built alongside this project. Important reference because
it shows broadcast-centric (not room-centric) API design.

### Core Object Model

- `Connection` — WebTransport session
- `Broadcast` — cohesive object with video/audio/chat/location/preview subcomponents
- `VideoTrack` / `AudioTrack` — media tracks with codec/resolution info
- `Catalog` — structured metadata about available tracks and renditions
- `Status` — connection/broadcast lifecycle state

### Construction / Connection

```ts
const conn = new Connection(url);
const broadcast = conn.publish(name);
// or
const broadcast = conn.subscribe(name);
```

- Connection first, then publish or subscribe on it
- Broadcast name is the addressing mechanism (not room + participant)
- No room/participant abstraction — broadcasts are the unit

### Publish Pattern

```ts
const broadcast = conn.publish("my-stream");
broadcast.video.setTrack(mediaStreamTrack);
broadcast.audio.setTrack(mediaStreamTrack);
```

- Direct track assignment on subcomponents
- Catalog is auto-generated from track metadata
- Preview frame support built in

### Subscribe Pattern

```ts
const broadcast = conn.subscribe("their-stream");
broadcast.video.target = { width: 1280, height: 720 };
const element = broadcast.video.attach();
```

- Declarative target resolution — the system picks the best rendition
- `attach()` returns a DOM element (like LiveKit)
- Catalog-aware: knows what renditions exist

### Event Model

- Property-based reactivity (getters/setters + change events)
- `broadcast.status` is watchable
- `broadcast.video.track` fires change when track arrives
- Lighter than LiveKit's event enum explosion

### What Hang Gets RIGHT

- **Broadcast as cohesive object**: one object with `.video`, `.audio`, `.chat`
  subcomponents. Not a bag of disconnected tracks.
- **Declarative selection**: `video.target = { width, height }` — express intent,
  not mechanism. The system figures out which rendition to use.
- **Catalog is first-class**: you can inspect what renditions/codecs are available
  before subscribing to media data.
- **Lightweight reactivity**: property changes, not 30 event types.
- **Preview built in**: `broadcast.preview` gives you a thumbnail without
  subscribing to the full video stream.

### What Hang Gets WRONG

- **No participant model**: fine for streaming, but for multi-party you need to
  build participant tracking yourself on top of broadcasts.
- **Connection-centric start**: you create a `Connection`, then create broadcasts
  on it. The connection is transport — it leaks into the API surface.
- **No room/group semantics**: if you want "everyone in this call," you roll your own.
- **Attach returns DOM element**: tight browser coupling. Not portable to native.

---

## 3. WebRTC (Browser API)

The baseline everyone knows. Also the API that has caused the most developer pain
in the history of real-time media.

### Core Object Model

- `RTCPeerConnection` — the connection (and everything else crammed into it)
- `MediaStream` / `MediaStreamTrack` — capture and playback
- `RTCRtpSender` / `RTCRtpReceiver` — send/receive channels
- `RTCSessionDescription` / `RTCIceCandidate` — signaling objects
- `RTCDataChannel` — arbitrary data

### Construction / Connection

```js
const pc = new RTCPeerConnection(config);
const offer = await pc.createOffer();
await pc.setLocalDescription(offer);
// ... send offer to remote via your own signaling ...
// ... receive answer ...
await pc.setRemoteDescription(answer);
// ... exchange ICE candidates ...
```

- SDP offer/answer exchange — the app is responsible for signaling
- ICE candidate trickling requires its own message channel
- Connection establishment takes ~50 lines minimum

### Publish Pattern

```js
const stream = await navigator.mediaDevices.getUserMedia({ video: true, audio: true });
stream.getTracks().forEach(track => pc.addTrack(track, stream));
```

- Add tracks to the peer connection
- Must renegotiate (new offer/answer) if adding tracks after initial connection
- `replaceTrack()` for swapping without renegotiation

### Subscribe Pattern

```js
pc.ontrack = (event) => {
  const [stream] = event.streams;
  videoElement.srcObject = stream;
};
```

- Single callback for all incoming tracks
- You figure out which track is which (audio vs video, which participant)
- Track ID correlation is manual

### Event Model

- DOM-style event listeners: `pc.addEventListener('track', ...)`
- `ontrack`, `onicecandidate`, `onnegotiationneeded`, `onconnectionstatechange`,
  `onicegatheringstatechange`, `onsignalingstatechange`, `oniceconnectionstatechange`
- Multiple overlapping state machines that are nearly impossible to reason about

### What WebRTC Gets RIGHT

- **`MediaStreamTrack` is universal**: one type for camera, screen, canvas, generated.
  The track abstraction itself is good.
- **`replaceTrack()` is seamless**: swap sources without renegotiation. Elegant.
- **`getUserMedia` constraints are declarative**: `{ video: { width: 1280 } }` — express
  intent, get best match.
- **Universally understood**: every RTC developer knows this API.

### What WebRTC Gets WRONG

- **SDP is a disaster**: the offer/answer model forces apps to implement their own
  signaling, manage SDP munging, handle location negotiation. This is the single
  biggest source of WebRTC pain.
- **ICE state machine is incomprehensible**: `iceConnectionState` vs
  `connectionState` vs `iceGatheringState` vs `signalingState`. Four overlapping
  state machines. Nobody gets this right on the first try.
- **No participant model**: `RTCPeerConnection` is a pipe between two endpoints.
  Multi-party requires an SFU and your own participant tracking.
- **Callback-based everything**: every state change is a separate event handler.
  Composing them into coherent app state is a nightmare.
- **Track identity is opaque**: incoming tracks have IDs, but mapping "this is
  Alice's camera" requires out-of-band signaling.
- **No catalog/capability discovery**: you don't know what the remote can send
  until they send it.
- **Srenegotiation is fragile**: adding/removing tracks triggers renegotiation,
  which can race with other changes.

---

## 4. GStreamer

Pipeline-based media framework. Not RTC-specific, but relevant for encode/decode/
processing patterns and how "media graphs" can work.

### Core Object Model

- `Pipeline` — top-level container, owns the graph
- `Element` — a processing node (source, filter, sink, encoder, decoder, ...)
- `Pad` — connection point on an element (src pad, sink pad)
- `Caps` — capability negotiation (format, resolution, framerate)
- `Bus` — message bus for events from the pipeline
- `Bin` — sub-container grouping elements

### Construction

```
gst-launch-1.0 videotestsrc ! x264enc ! mp4mux ! filesink location=out.mp4
```

```python
pipeline = Gst.Pipeline()
src = Gst.ElementFactory.make("v4l2src")
enc = Gst.ElementFactory.make("x264enc")
pipeline.add(src)
pipeline.add(enc)
src.link(enc)
pipeline.set_state(Gst.State.PLAYING)
```

- Build a graph of elements, link them, set state to PLAYING
- Elements are created by factory name (string-typed)
- Linking negotiates capabilities automatically

### Publish Pattern (sending media)

- Chain: source → encoder → payloader → network sink
- Each step is an element in the pipeline
- `appsrc` for pushing custom frames into the pipeline

### Subscribe Pattern (receiving media)

- Chain: network source → depayloader → decoder → sink
- `appsink` for pulling decoded frames out
- `autovideosink` / `autoaudiosink` for automatic output

### Event Model

- `Bus` messages: `EOS`, `ERROR`, `STATE_CHANGED`, `ELEMENT`, custom
- Pad probes for intercepting data flow
- Signal/callback system on elements (`g_signal_connect`)

### What GStreamer Gets RIGHT

- **Pipeline as a first-class concept**: declare the graph, the framework handles
  threading, buffering, synchronization, clock management. This is powerful.
- **Capability negotiation is automatic**: elements figure out compatible formats
  between themselves. You don't manually specify "NV12 → I420 → RGB."
- **Composition is natural**: complex pipelines are just longer chains. Adding an
  overlay or mixer is adding an element, not rewriting the app.
- **State management is simple**: `NULL → READY → PAUSED → PLAYING`. Four states.
  Linear. Comprehensible.

### What GStreamer Gets WRONG

- **String-typed element factories**: `Gst.ElementFactory.make("x264enc")` — typos
  are runtime errors. No type safety on element properties.
- **Pad linking is manual and error-prone**: `sometimes` pads appear dynamically,
  you must handle `pad-added` signals. Race conditions abound.
- **Error messages are terrible**: `"Internal data stream error"` tells you nothing.
  Debugging requires `GST_DEBUG=4` log spew.
- **Not designed for RTC latency**: pipeline model adds buffering at every stage.
  Getting low latency requires fighting the framework (setting `latency` on every
  element, `sync=false` on sinks, `tune=zerolatency` on encoders).
- **API is enormous**: thousands of elements, hundreds of properties. The learning
  curve is steep. Finding the right incantation for your use case requires tribal
  knowledge.

---

## 5. OBS WebSocket / libobs

How streaming software handles publish/subscribe. OBS is the dominant tool for
live streaming. `obs-websocket` is the remote control protocol; `libobs` is the
internal C API.

### Core Object Model

- `Scene` — a composited layout of sources
- `Source` — an input (camera, screen, image, browser, media file)
- `Output` — a destination (RTMP stream, recording, virtual camera)
- `Encoder` — video/audio encoding (x264, NVENC, QSV, etc.)
- `Service` — streaming service configuration (Twitch, YouTube, custom RTMP)
- `SceneItem` — a source placed in a scene with transform (position, scale, crop)

### Construction

- libobs: `obs_startup()` → create sources → create scenes → create outputs → start
- obs-websocket: connect to `ws://localhost:4455`, authenticate, send JSON-RPC

### Publish Pattern

```json
// obs-websocket: start streaming
{ "requestType": "StartStream" }

// or: start recording
{ "requestType": "StartRecord" }
```

- libobs: `obs_output_start(output)` — output pulls from the scene/encoder chain
- The scene graph determines what gets encoded
- Encoder selection is separate from source/scene setup

### Subscribe Pattern

- OBS is primarily a *publisher*, not a subscriber
- Media sources (`ffmpeg_source`, `vlc_source`) can consume remote streams
- `obs-browser` source can embed web content (including WebRTC)
- Virtual camera output makes OBS a source for other apps
- No first-class "subscribe to a remote participant" concept

### Event Model

- obs-websocket: JSON event messages over WebSocket
  - `StreamStateChanged`, `RecordStateChanged`, `SceneChanged`, `SourceCreated`, etc.
- libobs: signal/callback system (`signal_handler_connect`)
- Source-level signals: `activate`, `deactivate`, `show`, `hide`, `destroy`

### What OBS Gets RIGHT

- **Scene composition as a first-class concept**: sources are composable into
  scenes. Scenes are composable into other scenes. The composition model is
  intuitive and powerful.
- **Source/output/encoder separation**: capture, encoding, and destination are
  independent concerns. You can change your encoder without touching your sources.
  You can add an output (recording) without changing what you're encoding.
- **"Just works" defaults**: start streaming with one click. Sensible defaults for
  encoder settings, resolution, bitrate.
- **Hot-swappable everything**: switch scenes, add/remove sources, change encoder
  settings — all while live. No restart required.

### What OBS Gets WRONG

- **Scene/source model doesn't map to RTC**: OBS thinks in "scenes I compose and
  publish." RTC thinks in "participants who each have tracks." These are
  fundamentally different mental models.
- **obs-websocket is chatty**: controlling OBS remotely requires many round-trips.
  No batch operations. State synchronization is painful.
- **libobs C API is low-level and unsafe**: manual reference counting, string-based
  property access, global state. Not a model for a modern API.
- **No multiplexed output**: one output = one destination. To stream AND record at
  different qualities, you need multiple encoder instances.
- **Plugin-based extensibility is baroque**: writing an OBS plugin requires
  understanding the source/output/encoder type system, signal handlers, properties
  system, and OBS's threading model.

---

## Synthesis: Patterns to Adopt and Patterns to Avoid

### Patterns to Adopt

1. **Stable, long-lived handle objects** (from LiveKit)
   - `Room`, `Participant`, `Publication` — grab once, hold forever, cheap to clone.
   - In Rust: `Arc`-based handles with interior mutability. `Clone` is just an Arc bump.

2. **Object exists before it's "ready"** (from LiveKit)
   - Create `Room` → attach listeners → connect. No race between construction and
     first event.
   - In Rust: return the handle immediately, connect in background. State observable
     via watcher.

3. **Cohesive objects with typed subcomponents** (from Hang)
   - `broadcast.video`, `broadcast.audio` — not a flat list of untyped tracks.
   - Subcomponents are always present (possibly empty/inactive), not optional.

4. **Declarative intent, not mechanism** (from Hang + WebRTC getUserMedia)
   - `video.target = { width: 1280 }` — express what you want, not how to get it.
   - In Rust: builder/options pattern for preferences, system resolves.

5. **Catalog/capability as inspectable state** (from Hang)
   - Know what renditions exist before subscribing to data.
   - In Rust: `Watcher<Catalog>` — watch for changes, inspect current snapshot.

6. **Publication as indirection** (from LiveKit)
   - Publication exists before track data arrives. Great for UI (show name/placeholder).
   - Track data fills in asynchronously.

7. **Clean state machine** (from GStreamer)
   - Small number of states, linear transitions. `Connecting → Connected → Closed`.
   - Not four overlapping state machines (looking at you, WebRTC ICE).

8. **Source/encoder/output separation** (from OBS)
   - Capture, encoding, and transport are independent concerns.
   - Changing encoder settings doesn't require recreating the source.

9. **Drop-based cleanup** (idiomatic Rust, not from any toolkit)
   - Drop a publication → unpublishes. Drop a subscription → unsubscribes.
   - No explicit `close()` / `dispose()` / `release()` ceremony.

10. **Watcher for continuous state, Stream for discrete events** (Rust-native)
    - Connection state, participant list, track status → `Watcher` (always has current value)
    - "Participant joined," "track published" → `Stream` (sequence of events)
    - This replaces both LiveKit's event enum explosion and WebRTC's callback hell.

### Patterns to Avoid

1. **Event enum explosion** (LiveKit's 30+ RoomEvents)
   - Apps shouldn't need 10 event handlers to render a video grid.
   - Instead: watch the participant list, watch each participant's publications.
     Reactive, not event-driven.

2. **Callback-based event model** (WebRTC, LiveKit)
   - `room.on('event', callback)` is JavaScript-native but Rust-hostile.
   - Callbacks capture mutable state → borrow checker pain.
   - Instead: `Stream` of events that can be `.await`ed in an async loop.

3. **Signaling as app responsibility** (WebRTC SDP)
   - Never make the user implement their own signaling layer.
   - Connection should be: here's a ticket/URL/token → connect. Done.

4. **Overlapping state machines** (WebRTC ICE)
   - One state enum. One transition path. If it's complicated internally,
     hide that complexity.

5. **String-typed identifiers for well-known things** (GStreamer, LiveKit Track.Source)
   - Use Rust enums for known variants, string for custom/extension.
   - `TrackSource::Camera` not `"camera"`.

6. **Transport objects in the primary API surface** (Hang Connection, WebRTC PeerConnection)
   - Users shouldn't need to think about connections, sessions, or transports
     to do basic things.
   - Layer 1 hides transport. Layer 2 exposes it for advanced use.

7. **Manual linking/wiring** (GStreamer pad linking)
   - Pipeline setup should be declarative, not manual graph construction.
   - "I want to publish my camera" not "create source → link to encoder → link to
     payloader → link to transport."

8. **Requiring renegotiation for basic operations** (WebRTC)
   - Adding a track, changing resolution, switching sources — none of these should
     require protocol-level renegotiation visible to the app.

9. **Global/singleton state** (libobs)
   - Multiple independent sessions must be possible. No global init.
   - In Rust: all state owned by handle objects.

10. **Dispose/cleanup ceremony** (LiveKit `room.disconnect()`, WebRTC `pc.close()`)
    - Explicit cleanup is a bug source. People forget. Resources leak.
    - Drop handles, everything cleans up. Explicit `close()` available but optional.

### The North Star

The ideal API for a basic use case should look something like:

```rust
// Join
let room = Room::connect(ticket).await?;

// Publish
room.local().publish_camera().await?;
room.local().publish_microphone().await?;

// Subscribe (reactive)
let mut participants = room.remote_participants().watch();
while let Some(list) = participants.next().await {
    for p in &list {
        // Publications are already there, tracks fill in reactively
        if let Some(video) = p.video() {
            render(video);
        }
    }
}

// Cleanup: drop `room`, everything stops.
```

This is ~15 lines. LiveKit JS equivalent is ~40. Raw WebRTC is ~150.
That's the bar.
