# CLI reference (`irl`)

The `irl` binary lives in the `iroh-live-cli` crate. It has seven commands:
`devices`, `publish`, `watch`, `call`, `room`, `record`, and `run`. Watching in a
window needs the `render` feature, which is on by default; `--no-video` plays
audio without it, and `record` and `run` are headless whatever the build. `call`
and `room` are windows and nothing else, so a build without `render` does not
offer them at all.

## Commands

### `irl devices`

Lists the cameras, displays, and audio devices this machine offers. Every
identifier it prints is one the `--video` and `--audio` specifiers accept, so
the output doubles as the argument reference for `irl publish`.

Windows and applications appear on macOS only: they are ScreenCaptureKit
concepts. On Linux the displays section reports that listing is unavailable,
because the xdg-desktop-portal picker owns that choice. The audio outputs section
needs the `playback` feature.

A build with the `rpicam` feature also prints a Raspberry Pi camera section, from
`rpicam-vid --list-cameras`. It names the reason instead of a camera when the
binary is not installed. Nothing there opens a camera.

### `irl publish`

Publishes a capture device or a media file. Prints a ticket, and a QR code of
it, that `irl watch` takes.

Source specifiers name a kind of source and optionally which device of that
kind. There is no backend segment: `moq_video::capture::Source` names a device
and lets the platform pick the backend that reaches it, so the old
`cam:v4l2:1` form described a choice the caller no longer makes.

| `--video` | Meaning |
|---|---|
| `cam` | The default camera. This is the default for `--video` |
| `cam:<id>` | A camera by the id `irl devices` reports |
| `screen`, `screen:<id>` | A whole display |
| `window:<id>` | One window (macOS) |
| `app:<id>` | Every window of one application (macOS) |
| `rpicam` | The Raspberry Pi camera through `rpicam-vid`, already encoded. Needs the `rpicam` feature |
| `file:<path>` | A media file, republished rather than encoded |
| `test`, `test:timing` | The timing pattern: a sweeping bar, stripes, a frame counter, a clock, and a marker that flashes with the tone's beep |
| `test:gradient` | A moving gradient, which proves only that frames are arriving |
| `none` | Publish no video |

| `--audio` | Meaning |
|---|---|
| `mic`, `mic:<id>` | A microphone. This is the default for `--audio` |
| `system` | Everything the machine is playing (macOS) |
| `file:<path>[:loop]` | An audio file, decoded and encoded |
| `test`, `test:beeps` | A beep every second, on the media time the timing pattern's marker flashes on |
| `test:tone` | An unbroken sine tone |
| `none` | Publish no audio |

Anything `--audio` does not recognise is taken as a device name, so an
ALSA-style `hw:0,1` works as written.

Encoding flags:

| Flag | Description |
|---|---|
| `--test-source` | Force `--video test --audio test`, overriding both if they were given |
| `--codec <CODEC>` | `h264` (default) or `h265`, which needs hardware |
| `--encoder <KIND>` | `auto` (default), `hardware`, `software`, or a backend name such as `vaapi`, `nvenc`, `v4l2`, `videotoolbox`, `openh264` |
| `--renditions <LIST>` | The simulcast ladder, comma-separated. Each rung is `<height>p`, `<width>x<height>`, or `<name>:<width>x<height>`, with an optional `@<fps>`; a bare name encodes at the source's own resolution. Default: one rendition named `video`, unscaled |
| `--bitrate <BPS>` | Target video bitrate. Omit to derive one from the resolution |
| `--width`, `--height`, `--fps` | Capture hints; the device snaps to its nearest supported mode |
| `--keyframe-interval <SECONDS>` | How often a keyframe is inserted. Default 2, the broadcast figure; 1 for a call or a demo where somebody is waiting for a first picture |
| `--no-cursor` | Hide the pointer in screen, window, and application capture |
| `--audio-codec <CODEC>` | `opus` (default) or `pcm` |
| `--audio-bitrate <BPS>` | Opus only; PCM's bitrate follows from its layout |

Transport flags:

| Flag | Description |
|---|---|
| `--name <NAME>` | Broadcast path, as it appears in the ticket (default: `hello`) |
| `--relay <ENDPOINT_ID>` | Also connect to a relay, which then carries the broadcast on |
| `--no-serve` | Do not accept incoming subscribers, and print no ticket. Useful only with `--relay` |
| `--no-qr` | Suppress the terminal QR code |
| `--fullscreen` | Start the preview window in fullscreen |

File flags, for a `file:` video source:

| Flag | Description |
|---|---|
| `--format <FMT>` | `fmp4` (default) or `avc3` |
| `--transcode` | Re-mux (or re-encode) through ffmpeg first, which a plain MP4 needs |

Publishing to a relay is now just a connection. Every broadcast this node
publishes is announced on every MoQ session it has, so `--relay` connects and
the announce follows.

`--preview` opens a window showing what is being published. It draws the frames
already on their way to the encoders, so it costs no extra decode. It needs the
`render` feature, on by default, and it is not available for a file source, whose
tracks are republished as they are.

`--video rpicam` publishes what the Pi's hardware encoder produced, so there is
no encode of ours to steer. `--codec` and `--encoder` are refused rather than
ignored, and `--renditions` takes at most one rung. `--preview` is refused for
the reason a file source is: no raw picture ever reaches us. `--width`,
`--height`, `--fps`, and `--bitrate` do apply, because they become `rpicam-vid`
arguments.

Some backend names need a feature flag: `vaapi`, `nvenc`, and `v4l2` are absent
from a default build, and naming one a build was compiled without fails at
encoder selection rather than at the flag. [docs/platforms.md](platforms.md) says
which platform has which.

#### Frame rate

Two flags can name one, and `--fps` wins where they disagree.

`--fps` is the capture rate for the whole publish, and it caps the ladder. A
rung asking for more than it allows gets what the capture actually runs at.

An `@<fps>` suffix on a rendition asks for the same thing from one rung:
`irl publish --renditions 720p@60`. It is a capture rate rather than a per-rung
encode rate, because a ladder is captured once and `fan_out` hands every frame
to every encoder, so there is a single frame rate for the whole ladder. Where
rungs disagree, the highest wins: `--renditions high:1280x720@60,low:640x360@30`
captures at 60 and encodes both rungs from those pictures. The `low` rung says
so in the log rather than silently getting something other than what it asked
for.

With neither, the capture asks for 30. A device that cannot reach 30 substitutes
the nearest rate it has, which is what makes the default the lower of 30 and the
fastest mode the device offers. macOS camera capture is the exception: the
AVFoundation backend ignores a requested rate, so the default is withheld there
and the device's own rate stands.

Every publish logs the rate it settled on and where the number came from:

```
INFO capture frame rate requested; the device runs at the nearest rate it
     supports fps=60 origin="--renditions"
```

`origin` is one of `--fps`, `--renditions`, `--fps, capping the ladder`,
`default`, or `device`. The rate a device actually negotiated appears
separately, on the `publishing video rendition` line each rung logs.

### `irl watch`

Subscribes to a broadcast and plays it. `irl play` is an alias.

| Flag | Description |
|---|---|
| `<TICKET>` | Connection ticket, as `irl publish` printed it |
| `--endpoint-id <ID>` | Remote endpoint id, instead of a ticket. Needs `--name` |
| `--name <NAME>` | Broadcast path, alongside `--endpoint-id` |
| `--no-video` | Play audio only; no window opens |
| `--scan` | Read the ticket off a QR code held up to the camera |
| `--scan-camera <SPEC>` | Which camera `--scan` reads: `cam`, `cam:<id>`, or `rpicam`. Omit to let it choose |
| `--rendition <NAME>` | Hold one rendition instead of following the downlink |
| `--decoder <KIND>` | `auto` (default), `hardware`, `software`, or a backend name such as `vaapi`, `nvdec`, `v4l2`, `videotoolbox`, `openh264` |
| `--latency <MODE>` | `realtime`, `balanced` (default), or `smooth` |
| `--audio-output <ID>` | Play through this device, as the first column of `irl devices` lists it |
| `--fullscreen` | Start in fullscreen |

`--scan` supplies the ticket from the camera instead of the command line, which
is what a machine with a touchscreen and no keyboard needs. The window opens on
the camera picture, and as soon as a frame carries a QR code that parses as a
ticket it connects to that broadcast. The player keeps a Scan button, so a run
started with a ticket can still be pointed somewhere else.

`--scan-camera` takes the grammar `--video` takes, restricted to sources that
hand over pixels: `cam` for the default camera, `cam:<id>` for one `irl devices`
lists, or `rpicam` for the Raspberry Pi camera, which here always means its raw
pictures. Omitted, the scanner takes the Pi camera where the build can reach one
and the default camera otherwise. That guess exists because a Pi's `/dev/video0`
is the raw sensor: it opens at any geometry you ask for and then never delivers
a frame, which leaves a black preview and no error. A Pi with a USB webcam is
the case the guess gets wrong, and `--scan-camera cam` is the answer to it. If
a camera opens and sends nothing for five seconds, the screen says so and names

The decoder reads through a soft picture. A laptop webcam has to get close to a
small panel such as the Pi Zero's e-paper to resolve its modules, and close is
out of focus; the scanner sharpens each frame and asks two decoders in turn,
which reads through about twice the blur a plain decoder does. Decoding runs on
its own thread, so the preview keeps moving while it works.
the flag.

Without `--rendition` the video track adapts: the subscription's transport
signals drive rendition selection, and a switch opens the replacement decoder
alongside the incumbent so the picture does not go blank. The window's
rendition combo switches between the two modes at any time.

`--decoder` picks which backend decodes the video, the viewing side of
`--encoder`. The choice is also a latency choice on a Raspberry Pi 4: measured
against a clock drawn into the picture, `--decoder v4l2`, which `auto` picks
there, showed a frame about a second after it was drawn, and `--decoder
openh264` about 360ms, both at a steady 30fps. The hardware decoder holds
roughly 700ms of pictures in its own queue. It exists to spare the CPU, which a
Pi Zero needs and a Pi 4 at 720p does not, so on a Pi 4 name `openh264` when
the delay matters. A backend named here is the only one tried, so a machine without it
fails rather than quietly falling back to software, which is what makes the flag
worth having when a driver is the thing under suspicion. The window's decoder
combo changes it mid-playback: the replacement opens alongside the incumbent
exactly as a rendition switch does, and the combo shows the backend actually
running next to the choice that asked for it.

`--latency` chooses how much slack the player keeps against a link that
delivers unevenly. Every frame is held back a little so that one arriving late
still has somewhere to land, and that hold is the largest delay the player adds
on its own; the rest of the pipeline's delay belongs to the encoder, the link
and the display.

| Mode | Holds | Skips at | For |
|---|---|---|---|
| `realtime` | 60ms | 100ms | A conversation, where being behind is worse than an occasional jump |
| `balanced` | 100ms | 150ms | The default, and what the player did before this flag existed |
| `smooth` | 400ms | 600ms | Watching rather than talking, over Wi-Fi or a mobile link |

The two columns move together on purpose: the skip threshold is what the
decoder is told to treat as too much buffered media, and setting it below the
slack the clock is deliberately holding would throw away the very frames that
slack exists to wait for. Measured end to end from a dev machine to a
Raspberry Pi 4 the whole pipeline runs at about 900ms, so this flag moves a
useful part of the delay but not most of it; `plans/v2/latency.md` accounts for
the rest.

### `irl call`

Opens a 1:1 video call. Both peers publish their own camera and microphone and
subscribe to the other's, so there is no caller and no callee once the session
is up: the difference is only who dialed.

| Flag | Description |
|---|---|
| `<TICKET>` | Ticket of the peer to call. Omit to wait for somebody to call this node |
| `--decoder <KIND>` | `auto` (default), `hardware`, `software`, or a backend name such as `vaapi`, `nvdec`, `v4l2`, `videotoolbox`, `openh264` |
| `--latency <MODE>` | `realtime`, `balanced` (default), or `smooth`, as `irl watch` takes it |
| `--scan-camera <SPEC>` | Which camera the scan screen reads, as `irl watch` takes it |
| `--no-qr` | Suppress the terminal QR code |
| `--fullscreen` | Start in fullscreen |

Every capture flag `irl publish` takes applies here too and describes what this
node sends: `--video`, `--audio`, the codec and bitrate flags, `--renditions`,
the geometry and `--fps`, `--keyframe-interval`, and `--test-source`.
`--decoder` describes the other direction: how the peer's picture is decoded.

The window starts on a waiting screen showing this node's ticket, a box to paste
the peer's into, and the local camera. A ticket given on the command line is
dialed straight away, so the two halves of a call are `irl call` on one machine
and `irl call <TICKET>` on the other. Once connected, the peer fills the window
and the local camera moves to a corner. Moving the pointer brings up the ticket
bar, the stats overlay, and the rendition and volume controls; leaving it still
hides them again.

Hanging up on either side returns both windows to the waiting screen, ready for
the next call. The path a peer publishes on is `calls/<its endpoint id>`, which
`irl watch` can subscribe to like any other broadcast if all that is wanted is
one direction.

### `irl room`

Joins a multi-party room: publishes this node's camera into it and draws
everybody else's in a grid, with a chat panel underneath.

| Flag | Description |
|---|---|
| `<TICKET>` | Ticket of the room to join. Omit to open a new one |
| `--display-name <NAME>` | Name the other participants see (default: this node's short endpoint id) |
| `--decoder <KIND>` | `auto` (default), `hardware`, `software`, or a backend name such as `vaapi`, `nvdec`, `v4l2`, `videotoolbox`, `openh264` |
| `--no-qr` | Suppress the terminal QR code |
| `--fullscreen` | Start in fullscreen |

Every `irl publish` capture flag applies here too and describes what this node
sends, and `--decoder` describes how every other participant's picture is
decoded.

Rooms are a gossip topic plus the MoQ subscriptions that follow from it. Every
participant announces the names of its broadcasts on the topic and subscribes to
every name it sees, so this is a full mesh and a small-group design: there is no
selective forwarding. The ticket a window prints includes itself as a bootstrap
peer, so it is the one to pass on to the next participant.

Chat rides on the same broadcast as the video, on a track named `chat`, so a
peer subscribed for the picture gets the messages without a second
subscription. Joining and leaving appear in the panel as they happen.

Leaving is derived from media, not from membership: a participant's tile
disappears when its broadcast closes or its session drops. A peer that joined
the topic and published nothing is not shown at all. See
[the rooms guide](guide/rooms.md) for why, and what replaces it.

### `irl record`

Subscribes to a broadcast and writes it to a file. No window, no decoder: the
encoded frames come off the wire and go straight into the container, so a
recording costs about what a subscription costs.

| Flag | Description |
|---|---|
| `<TICKET>` | Connection ticket, as `irl publish` printed it |
| `--endpoint-id <ID>` | Remote endpoint id, instead of a ticket. Needs `--name` |
| `--name <NAME>` | Broadcast path, alongside `--endpoint-id` |
| `-o`, `--output <PATH>` | The file to write (default: `recording.mp4`) |
| `--format <FMT>` | `fmp4` or `mkv`, overriding what `--output`'s extension implies |
| `--rendition <NAME>` | Keep one video rendition instead of every rung the catalog offers |
| `--duration <SECS>` | Stop after this many seconds, instead of on Ctrl+C |
| `--latency <MILLIS>` | How long a stalled group is waited for before it is skipped (default: 2000) |

The container follows `--output`'s extension: `.mp4`, `.m4v`, and `.m4s` are
written as fragmented MP4, `.mkv` and `.webm` as Matroska. Anything else needs
`--format`.

Both containers are fragmented, so the file on disk is complete at every chunk
boundary: a recording stopped with Ctrl+C, or one whose process was killed, is
still playable up to the last fragment written.

Without `--rendition`, a simulcast broadcast is recorded whole and the file
carries one video track per rung. Players pick the largest.

### `irl run`

Runs a whole session from a TOML file: one endpoint publishing several
broadcasts, subscribing to several others, and recording any of them. That is
what a pile of `irl publish` and `irl watch` processes cannot do between them,
because each of those binds an endpoint of its own.

```sh
irl run session.toml
```

The session is headless. A `[[recv]]` block plays audio and can record to a
file, but no window opens: a window owns the main thread and there is only one
of those.

```toml
# Optional. Names a stored endpoint identity under
# <config dir>/iroh-live/secret_keys/<name>.key, generated on first use, so the
# tickets this session prints are the same on every run.
secret_key_name = "studio"

[[send]]
name = "camera"              # broadcast path, and the label in the output
video = "cam"                # every other key is an `irl publish` capture flag,
audio = "mic"                # under the flag's own name and with its default
codec = "h264"
encoder = "auto"
renditions = ["low:320x180", "720p"]
bitrate = 3_000_000
width = 1280
height = 720
fps = 30
no_cursor = false
audio_codec = "opus"
audio_bitrate = 96_000

[[send]]
name = "screen"
video = "screen"
audio = "none"

[[recv]]
name = "friend"
ticket = "iroh-live:..."     # as `irl publish` or another `irl run` printed it
audio_output = "default"     # or "none" to leave the speakers alone
record = "friend.mp4"        # optional; the container follows the extension
rendition = "low"            # optional; which video rendition to record
```

`name` is the only required key in a `[[send]]` block, and `name` and `ticket`
the only ones in a `[[recv]]`. An unrecognised key is an error rather than a
value silently dropped, so a typo is reported with the key that caused it.

A block that fails to start is reported and the rest of the session runs
without it. The session ends on Ctrl+C, which flushes every recording before
the endpoint closes.

## Examples

Publish the default camera and microphone, and print a ticket:

```sh
irl publish
```

Publish a generated pattern and tone, no hardware needed:

```sh
irl publish --test-source
```

That pair is built to be measured rather than looked at: the bar sweeps the
width every two seconds, the counter numbers each frame, the clock is UTC to
the millisecond, and the marker at the bottom is lit for exactly as long as
each beep sounds. Photograph the publisher and the player together and the
difference between the two clocks is the latency; watch the marker while the
beeps play and A/V sync is something you see rather than estimate.

Publish a camera as a two-rung simulcast ladder:

```sh
irl publish --video cam:/dev/video0 --renditions low:320x180,720p
```

Ask a camera or a PipeWire screen share for 60 frames per second, which it gives
if it has a mode that fast:

```sh
irl publish --video cam:/dev/video0 --renditions 720p@60
```

Publish a fragmented MP4, re-muxing a plain one on the way:

```sh
irl publish --video file:recording.mp4 --transcode
```

Watch it:

```sh
irl watch <TICKET>
```

Wait for a call, and call somebody from another machine:

```sh
irl call
irl call <TICKET>
```

Open a room, and join it from two more machines:

```sh
irl room --display-name alice
irl room --display-name bob <TICKET>
irl room --display-name carol <TICKET>
```

Record ten seconds of it to a fragmented MP4:

```sh
irl record <TICKET> -o clip.mp4 --duration 10
```

Publish two broadcasts and record a third, all from one endpoint:

```sh
irl run session.toml
```

## What is not here

`relay` was dropped in the move to the upstream media stack, and is recoverable
from the `main` branch.

The relay itself is not gone, only the subcommand that embedded it. Run it as its
own binary with `cargo run -p iroh-live-relay`, and see
[the browser relay guide](guide/browser-relay.md).
