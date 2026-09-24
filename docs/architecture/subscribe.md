# Subscribing

`iroh_live_media::RemoteBroadcast` wraps a `moq_net::broadcast::Consumer`, reads
the catalog, and holds the subscription. `RemoteBroadcast::play` starts a
`Player` over it, which owns everything mutable about one playback: its
decoders, its playout clock, its rendition choice, and its statistics. Two
players of one broadcast are two playbacks and cannot interfere. Decoding is
upstream: `moq_video::decode::Consumer` and `moq_audio::decode::Consumer` pick a
backend from the catalog entry and hand back frames. Three things have no
upstream counterpart and live here.

Rendition selection is the first. `moq_mux::select` is fixed at construction, so
a subscriber that wants to follow its downlink has to choose for itself. The
second is the playout clock, which keeps audio and video aligned across two
independent decode paths. The third is the catalog extension, where the
publisher's identity rides alongside the media sections.

## Opening a broadcast

`RemoteBroadcast::from_moq(consumer)` starts reading the catalog and returns at
once. `catalog()` is a watcher over an `Option<Catalog>` that reads `None` until
the first catalog arrives, which keeps construction usable in a UI reconcile
loop; a caller that needs the catalog waits on the watcher. A background task
follows the catalog track and republishes each update.

`RemoteBroadcast::from_origin(origin, path)` follows a path in a route table
instead. When a change of route ends the broadcast, it is requested again
through the next route, and players see a new generation of the broadcast
rather than an end. The broadcast counts as closed only once no route serves the
path for three seconds, so `closed()` resolves about three seconds after a
publisher ends its broadcast. `RemoteBroadcast::from_resolved(origin, path,
consumer)` does the same starting from a consumer the caller already resolved in
that table: nothing waits for a first route, and a publisher that is gone by
then closes the broadcast rather than leaving it waiting.
`RemoteBroadcast::local(&broadcast)` reads a `LocalBroadcast` in-process, with
no transport at all.

`Catalog` wraps hang's catalog and is compared by snapshot identity: every
update the publisher sends is a new snapshot, which is the honest comparison
for a watcher, since hang's catalog carries floats and is only `PartialEq`.
`video()` lists the renditions largest first as `VideoRenditionInfo`, `audio()`
lists `AudioRenditionInfo`, and `metadata()` carries the publisher's display
name. `as_hang()` reaches hang's own shape for a caller that needs it.

In iroh-live, `Live::subscribe` resolves the path first and builds the
`RemoteBroadcast` with `from_resolved` on the transport's route table, returning
without waiting for the catalog, and attaches the link of whichever session or
relay serves it, so every player of the broadcast adapts.
`Live::remote_broadcast` does the same for a `Subscription` from a room or from
`Moq::subscribe`. See [adaptive bitrate](adaptive.md).

## Video decoding

A player runs three tasks. The selector turns the rendition mode, the catalog,
and the network into the rendition that should play. The video supervisor keeps
one decoder playing and at most one replacement warming up. The audio task
decodes into the output.

Each decoder is read by its own task, calling `decode::Consumer::read()` in a
plain loop and forwarding frames over a bounded channel two frames deep. The
supervisor selects over that channel and the control signals.

The split is structural rather than stylistic. `moq_video::decode::Consumer`
reads through a `Sink`, which upstream documents as not cancel-safe: dropping a
`read` future poisons the decoder and every later call fails. A `select!` cancels
every arm it does not pick, so a supervisor that selected directly on `read()`
would kill its own decoder on any control signal. The read has to live somewhere
nothing cancels it, and reach the supervisor over a channel.

The channel holds two frames because the supervisor only paces and forwards. A
deeper backlog there would be latency rather than throughput.

An access unit the decoder refuses is skipped rather than fatal. A live stream
loses pictures to a skipped group or a truncated access unit, and a decoder
without its reference chain refuses every picture until the next keyframe. The
reader gives up only after 300 refusals in a row, which spans several keyframe
intervals at any rate we publish, and counts what it skipped in the player's
stats.

## Switching renditions

A rendition switch and a decoder change are the same operation: the supervisor
opens a replacement decoder beside the incumbent, keeps forwarding the
incumbent's frames, and hands over once the replacement has caught up. The
rules live in `player/switch.rs` as plain transitions, so a test drives them
with no decoder and no network behind them.

- There is at most one replacement. A new request supersedes it as a whole, so
  a switch to C while B is warming never lands on B first.
- A replacement takes over only once its playhead has caught up with the
  incumbent's to within 100 ms, the same slack `@moq/watch` uses. The picture
  neither goes blank nor steps backwards by more than that across a switch.
- A replacement has 15 s from the request to taking over, covering the open,
  the wait for that track's next keyframe, and the catch-up. Past that it is
  abandoned and the incumbent keeps playing.
- When the incumbent ends, a warm replacement takes over at once, since there
  is no longer a picture to step back from.

`Player::set_rendition(mode)` changes the `RenditionMode` and returns
immediately. `Player::wait_for_rendition(name)` waits until a rendition is on
screen and fails with a `SwitchError` once a switch to it was superseded,
withdrawn or failed, the catalog shows no such rendition, or the video ended.
Callers bound the wait with `tokio::time::timeout`.

`Player::status()` is a watcher over `PlayerStatus`: the state of each slot, the
mode asked for, the rendition on screen, the one warming up to replace it, why
the last switch or pin could not be honoured, and which decoder backend is
running. Which backend opened is the first thing worth knowing when playback
looks wrong on a particular device, and it can change across a switch.

## Frame delivery

Decoded frames land in a latest-wins slot rather than a queue. `Player::video()`
returns a `VideoFrames` handle onto it, and every call returns another handle
onto the same stream, which survives rendition switches and decoder changes.
Each handle keeps its own cursor, so two readers both see every picture that is
current when they look rather than splitting the stream between them. A reader
that falls behind skips to the newest picture instead of draining a backlog.
`try_next()` polls without blocking, which is what a render loop wants, and
`next().await` waits for a picture newer than the last one this handle read.

## Audio playback

`moq_audio::playback::Engine` owns the output device and mixes every sink into
it. `iroh_live_media::AudioOutput` is one opened engine: the application opens
it with `AudioOutput::open(device)` and passes it to every `PlayerConfig` that
should play there, so device choice is explicit and several players share one
device stream. `AudioOutput::devices()` lists outputs and `switch(device)` moves
every player on the output to another device without interrupting them.
`AudioOutput::null()` discards what it is given, for headless use and tests; the
player still decodes, and the playout clock still runs off it. A `PlayerConfig`
with no output does not subscribe to audio at all.

The audio task writes frames straight to its sink and reports how much audio is
still queued ahead of the speaker to the player's playout clock on every frame.
That figure is the only latency either side can actually measure.

`Player::set_volume` sets the level, and `PlaybackStats::audio` carries the
buffered duration and the most recent peak for a meter. There is no audio
ladder, so the first audio rendition plays and there is nothing to switch
between. The audio task reopens on a new route to the broadcast, and retries a
track that ended or never opened when the catalog changes.

## Player configuration

`PlayerConfig` carries a `RenditionMode`, a `Latency`, the `AudioOutput`, and a
decoder selection. `Latency { min, max }` is how far behind live to run: the
playout clock holds each picture for `min`, and `max` becomes `max_age` on both
`moq_video::decode::Options` and `moq_audio::decode::Options`, which is where
upstream drops stale groups. The default holds for 100 ms and skips past
150 ms. The decoder selection becomes the video options' decoder `kind`,
choosing the backend.

`Player::set_latency` moves the hold at once. A changed `max` rebuilds the video
decoder behind the picture, which takes over once it has caught up, the same
way a switch does; an audio track already playing keeps the `max` it opened
with until it next reopens. `Player::set_decoder` opens the replacement backend
behind the picture too, and one that fails to open leaves the incumbent playing
and says so in `PlayerStatus::switch_error`.

See [playout and sync](playout.md) for what the clock does with `min`.

## Shutdown

Dropping the `Player` cancels the token its tasks watch, aborts them, and closes
the playout clock, which wakes anything blocked waiting for a frame's playout
time. Dropping a decoder's reader task drops the decoder with it. Each player
holds a clone of its `RemoteBroadcast`, so the subscription lasts while the
broadcast handle or any of its players does, and ends with the last of them.
`RemoteBroadcast::closed()` waits until the broadcast itself has ended.
