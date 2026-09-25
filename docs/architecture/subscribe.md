# Subscribing

`iroh_live_media::RemoteBroadcast` reads a broadcast's catalog and holds the
subscription. `RemoteBroadcast::play` starts a `Player`, which owns its
decoders, its playout clock, its rendition choice and its statistics. Two
players of one broadcast are two separate playbacks. Decoding is upstream:
`moq_video::decode::Consumer` and `moq_audio::decode::Consumer` pick a backend
from the catalog entry and return frames. This crate adds rendition selection,
the decoder swap, and the [playout clock](playout.md). The code is in
`iroh-live-media/src/remote.rs` and `iroh-live-media/src/player/`.

## Opening a broadcast

Every constructor starts reading the catalog and returns at once. `catalog()`
watches an `Option<Catalog>` that reads `None` until the first catalog arrives.
A background task follows the catalog track and publishes each update.

`RemoteBroadcast::from_origin(origin, path)` follows a path in a route table.
When a change of route ends the broadcast, it is requested again through the
next route, and players see a new generation of the broadcast instead of an
end. The broadcast closes once no route has served the path for three seconds
(`REROUTE_PATIENCE`), so `closed()` resolves about three seconds after a
publisher ends its broadcast. `RemoteBroadcast::from_resolved(origin, path,
consumer)` does the same, starting from a consumer the caller already resolved
in that table. Nothing waits for a first route, and a publisher that is already
gone closes the broadcast. `RemoteBroadcast::local(&broadcast)` reads a
`LocalBroadcast` in-process, without a transport.

`Catalog` is hang's catalog behind an `Arc` and derefs to it. Two catalogs are
equal only when they are the same snapshot, so a watcher can tell an update
from a repeat. `ranked_video()` lists the video renditions largest first.

`Live::subscribe` resolves the path, builds the `RemoteBroadcast` with
`from_resolved`, and attaches the serving link's measurements with
`with_network`. `Live::remote_broadcast` does the same for a `Subscription`
from a room or from `Moq::subscribe`. See [adaptive bitrate](adaptive.md).

## Video decoding

A player runs three tasks. The selector (`select.rs`) turns the rendition mode,
the catalog and the network into the rendition that should play. The video
supervisor (`video_task.rs`) keeps one decoder playing and at most one
replacement warming up. The audio task (`audio.rs`) decodes into the output.

Each decoder is read by its own task, which calls `decode::Consumer::read()` in
a plain loop and forwards frames over a bounded channel two frames deep
(`READ_AHEAD`). The supervisor selects over that channel and its control
signals. The read cannot be an arm of the supervisor's `select!`: dropping a
`read` future poisons the decoder, and a `select!` drops every arm it does not
pick.

An access unit the decoder refuses is skipped. After a skipped group or a
truncated access unit, a decoder refuses every picture until the next keyframe.
The reader gives up after 300 refusals in a row
(`MAX_CONSECUTIVE_DECODE_FAILURES`), which spans several keyframe intervals,
and counts what it skipped in the player's stats.

## Switching renditions

A rendition switch and a decoder change are the same operation. The supervisor
opens a replacement decoder beside the incumbent, keeps showing the
incumbent's frames, and hands over once the replacement has caught up. The
rules are plain transitions in `player/switch.rs`, so tests drive them without
a decoder or a network.

- There is at most one replacement. A new request replaces it, so a switch to
  C while B is warming up never lands on B first.
- A replacement takes over once its playhead is within 100 ms of the
  incumbent's (`CATCH_UP_SLACK`, the slack `@moq/watch` uses). It waits for that
  at most one second after its first picture (`CATCH_UP_PATIENCE`), then takes
  over where it is.
- A replacement takes over on a picture it decoded, never on opening alone.
  With nothing playing, including after the incumbent ended, its first picture
  is enough.
- A replacement has `Adaptation::switch_deadline` (15 s by default) from the
  request to taking over. That covers the open, the wait for the track's next
  keyframe and the catch-up. Past it the replacement is given up and the
  incumbent keeps playing.

An automatic step down does not overlap the two decoders. See
[adaptive bitrate](adaptive.md#the-decision).

`Player::set_rendition(mode)` changes the `RenditionMode` and returns at once.
`Player::wait_for_rendition(name)` waits until the rendition is on screen. It
fails with a `SwitchError` when a switch to it is superseded, withdrawn or
fails, when the catalog has no such rendition, or when the video ended. Bound
the wait with `tokio::time::timeout`.

`Player::status()` watches a `PlayerStatus`: each slot's state, the mode, the
rendition on screen, the one warming up (`switching_to`), why the last switch
or pin failed (`switch_error`), and the decoder backend running.

## Frame delivery

Decoded frames land in a latest-wins slot. `Player::video()` returns a
`VideoFrames` handle onto it. Every call returns another handle onto the same
stream, which survives rendition switches and decoder changes. Each handle
keeps its own cursor, so two readers both see the current picture. A reader
that falls behind skips to the newest one. `try_next()` polls without blocking,
for a render loop, and `next().await` waits for a picture newer than the last
one the handle read.

## Audio playback

`AudioOutput` is one opened `moq_audio::playback::Engine`. The application
opens it with `AudioOutput::open(device)` and passes it to every `PlayerConfig`
that should play there, so several players share one device stream.
`AudioOutput::devices()` lists outputs, and `switch(device)` moves every player
on the output to another device. `AudioOutput::null()` discards audio, for
headless use and tests: the player still decodes, but nothing paces the audio.
A `PlayerConfig` with `audio: None` does not subscribe to audio at all.

The audio task plays the first audio rendition. It writes frames straight to
its sink and reports how much audio is queued ahead of the speaker to the
playout clock on every frame. `Player::set_volume` sets the level.
`PlaybackStats::audio` carries the buffered duration and the latest peak for a
meter. The task reopens when the broadcast moves to a new route. A track that
ended or never opened is tried again when the catalog changes, and every two
seconds.

## Player configuration

`PlayerConfig` is a struct literal with `Default`:

```rust
let config = PlayerConfig {
    audio: Some(output),
    ..Default::default()
};
```

It carries the `rendition` mode, the `latency`, the `audio` output, the
`decoder` backend and the `adaptation` thresholds and timers.
`Latency { min, max }` sets how far behind live to run. The playout clock holds
each picture for `min`, and `max` becomes `max_age` on the video and audio
decode options, where upstream drops stale groups. The default holds for
100 ms and skips past 150 ms. `decoder` becomes the video decode options'
`kind`.

`Player::set_latency` changes the hold at once. A changed `max` rebuilds the
video decoder behind the picture, and the new one takes over once it has
caught up, as in a switch. An audio track keeps its `max` until it next
reopens. `Player::set_decoder` also opens the new backend behind the picture. A
backend that fails to open leaves the incumbent playing and says why in
`PlayerStatus::switch_error`.

See [playout and sync](playout.md) for what the clock does with `min`.

## Shutdown

Dropping the `Player` cancels its tasks and closes its playout clock, which
wakes anything waiting for a frame's playout time. Dropping a reader task drops
its decoder. Each player holds a clone of its `RemoteBroadcast`, so the
subscription lasts until the last broadcast handle and the last player are
gone. `RemoteBroadcast::closed()` waits until the broadcast itself has ended.
