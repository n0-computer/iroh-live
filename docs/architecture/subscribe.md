# Subscribing

`moq_media::subscribe::RemoteBroadcast` wraps a `moq_net::broadcast::Consumer`,
reads the catalog, and hands out a `VideoTrack` and an `AudioTrack`. Decoding is
upstream: `moq_video::decode::Consumer` and `moq_audio::decode::Consumer` pick a
backend from the catalog entry and hand back frames. Three things have no
upstream counterpart and live here.

Rendition selection is the first. `moq_mux::select` is fixed at construction, so
a subscriber that wants to follow its downlink has to choose for itself. The
second is the playout clock, which keeps audio and video aligned across two
independent decode paths. The third is the catalog extension, where chat and
publisher identity ride alongside the media sections.

## Opening a broadcast

`RemoteBroadcast::new(name, consumer)` subscribes and waits for the first
catalog before returning. Waiting is deliberate: a handle returned before the
first catalog would answer "not yet" to every question a caller could ask. A
background task then follows the catalog track and republishes each update
through a `Watchable<CatalogSnapshot>`, reachable with `catalog()` for the
current value and `catalog_watcher()` for the stream of changes.

`CatalogSnapshot` is an `Arc<Catalog>` compared by pointer identity. hang's
catalog carries floats, so it is only `PartialEq`, and `Watchable` needs `Eq`.
Every update allocates a fresh snapshot, which makes pointer equality the honest
comparison.

`RemoteBroadcast::media()` opens whichever of video and audio the broadcast
turned out to carry and returns them in a `MediaTracks`. `video()` picks the
best video rendition, `video_rendition(name)` picks one by name, and `audio()`
opens the first audio track. In iroh-live, `Live::subscribe` wraps all of this
in a `Subscription` that also wires up the transport signal producer.

## Video decoding

Each rendition is decoded by its own task, reading `decode::Consumer::read()` in
a plain loop and forwarding frames over a bounded channel two frames deep. A
supervisor task selects over that channel and the control signals.

The split is structural rather than stylistic. `moq_video::decode::Consumer`
reads through a `Sink`, which upstream documents as not cancel-safe: dropping a
`read` future poisons the decoder and every later call fails. A `select!` cancels
every arm it does not pick, so a supervisor that selected directly on `read()`
would kill its own decoder on any control signal. The read has to live somewhere
nothing cancels it, and reach the supervisor over a channel.

The channel holds two frames because the supervisor only paces and forwards. A
deeper backlog there would be latency rather than throughput.

## Switching renditions

`VideoTrack::set_rendition(name)` requests a switch and returns immediately. The
supervisor opens the replacement decoder alongside the incumbent, keeps
forwarding the incumbent's frames, and hands over on the replacement's first
frame. The picture does not go blank across the change. `switched_to(name)`
waits for the handover when a caller needs to know it happened; a switch that
never lands, because the rendition left the catalog or its decoder failed to
open, leaves that future pending.

`enable_adaptation(signals)` hands the same request channel to the adaptation
task, which decides for itself. See [adaptive bitrate](adaptive.md).

`rendition_watcher()` and `decoder_watcher()` report which rendition is playing
and which decoder backend opened. Which backend opened is the first thing worth
knowing when playback looks wrong on a particular device, and it can change
across a switch.

## Frame delivery

Decoded frames land in a latest-wins slot rather than a queue. A renderer that
falls behind skips to the newest picture instead of draining a backlog.
`VideoTrack::take()` polls it without blocking, which is what a render loop
wants, and `recv()` awaits the next frame.

## Audio playback

`moq_audio::playback::Engine` owns the output device and mixes every sink into
it, so a process watching several broadcasts opens one engine and one sink per
broadcast. `moq_media::playback` owns that one engine, opening it lazily on
first use. `playback::devices()` lists outputs, `playback::open(config)` chooses
one before the first subscription, and `playback::switch(config)` moves every
playing track to another device without interrupting it.

The audio decode task writes frames straight to its sink and reports
`sink.buffered()` to the playout clock on every frame. That figure, how much
audio is still queued ahead of the speaker, is the only latency either side can
actually measure.

`AudioTrack` exposes `set_volume`, `volume`, and `peak`, all of which delegate
to the sink's `moq_audio::playback::Control`. There is no audio ladder, so there
is nothing to switch between.

## Playback policy

`PlaybackPolicy` carries a `SyncMode`, a `max_latency`, a decoder selection, and
the GPU-frames request. `max_latency` becomes `latency_max` on both
`moq_video::decode::Config` and `moq_audio::decode::Config`, which is where
upstream drops stale groups. The default is 150 ms. The decoder selection
becomes that config's `kind`, choosing the video backend.

`set_playback_policy` affects tracks opened afterwards; tracks already running
keep the policy they were created with until `VideoTrack::reopen_decoder` builds
the decoder again from the current one.

See [playout and sync](playout.md) for what `SyncMode` does.

## Shutdown

`RemoteBroadcast::shutdown()` cancels the token every decode task watches and
closes the playout clock, which wakes anything blocked waiting for a frame's
playout time. Dropping a `VideoTrack` aborts its supervisor, which drops the
reader task, which drops the decoder.
