# CLI reference (`irl`)

The `irl` binary is in the `iroh-live-cli` crate. It has seven commands:
`devices`, `publish`, `watch`, `call`, `room`, `record`, and `run`.

The default features are `aec`, `playback`, and `sound-server`. `record` and
`run` never open a window.

## `irl devices`

Lists the cameras, displays, and audio devices of this machine. Each line
starts with the identifier that `--video`, `--audio`, and `--audio-output`
take.

Windows and applications are listed on macOS only. On Linux the desktop portal
picks the display, so the displays section may report that listing is
unavailable. The audio outputs section needs the `playback` feature. A build
with the `rpicam` feature adds a Raspberry Pi camera section, from
`rpicam-vid --list-cameras`.

## `irl publish`

Publishes a capture device or a media file, and prints a ticket and its QR code
for `irl watch`.

### Sources

A source specifier names a kind of source and, optionally, a device of that
kind. The platform picks the capture backend.

| `--video` | Meaning |
|---|---|
| `cam` | The default camera. This is the default |
| `cam:<id>` | A camera, by the id `irl devices` prints |
| `screen`, `screen:<id>` | A whole display |
| `window:<id>` | One window (macOS) |
| `app:<id>` | Every window of one application (macOS) |
| `rpicam` | The Raspberry Pi camera through `rpicam-vid`, encoded by the Pi's hardware. Needs the `rpicam` feature |
| `rpicam:raw` | The Raspberry Pi camera's raw pictures, encoded here. Needs the `rpicam` feature |
| `file:<path>[:loop]` | A media file, republished without encoding |
| `test`, `test:timing` | The timing pattern: a sweeping bar, stripes, a frame counter, a clock, and a marker that flashes with the test tone's beep |
| `test:gradient` | A moving gradient |
| `none` | No video |

| `--audio` | Meaning |
|---|---|
| `mic`, `mic:<id>` | A microphone. `mic` is the default |
| `system` | Everything the machine plays (macOS) |
| `file:<path>[:loop]` | An audio file, decoded and encoded |
| `test`, `test:beeps` | A beep every second, in step with the timing pattern's marker |
| `test:tone` | A continuous sine tone |
| `none` | No audio |

Any other `--audio` value is taken as a device name, so an ALSA name such as
`hw:0,1` works as written.

### Flags

Capture and encoding:

| Flag | Description |
|---|---|
| `--video <SPEC>` | Video source (default: `cam`) |
| `--audio <SPEC>` | Audio source (default: `mic`) |
| `--test-source` | Publish `--video test --audio test`, overriding both flags |
| `--codec <CODEC>` | `h264` (default) or `h265`. H.265 needs a hardware encoder |
| `--encoder <KIND>` | `auto` (default), `hardware` (`hw`), `software` (`sw`), or one backend: `videotoolbox`, `mediafoundation`, `mediacodec`, `nvenc`, `vaapi`, `v4l2`, `openh264` |
| `--renditions <LIST>` | The simulcast ladder, comma-separated. A rung is `<height>p`, `<width>x<height>`, or `<name>:<width>x<height>`, with an optional `@<fps>`. A bare name encodes at the source's size. Default: one rendition named `video` at the source's size |
| `--keyframe-interval <SECONDS>` | Seconds between keyframes (default: 2). A viewer waits up to this long for a first picture, and a rendition switch waits as long. Use 1 for a call or a demo |
| `--bitrate <BITS_PER_SECOND>` | Target video bitrate for every rung. Omit to derive one from the resolution |
| `--width`, `--height` | Requested capture size. The device picks its nearest mode |
| `--fps` | Requested capture frame rate, see below |
| `--no-cursor` | Hide the pointer in screen, window, and application capture |
| `--audio-codec <CODEC>` | `opus` (default) or `pcm` |
| `--audio-bitrate <BITS_PER_SECOND>` | Target audio bitrate. Opus only |

Transport:

| Flag | Description |
|---|---|
| `--name <NAME>` | Broadcast name, as the ticket carries it (default: `hello`) |
| `--relay <ENDPOINT_ID>` | Also push the broadcast to this relay |
| `--no-serve` | Accept no incoming sessions and print no ticket. Only useful with `--relay` |
| `--no-qr` | Do not print the QR code |

Window and file source:

| Flag | Description |
|---|---|
| `--preview` | Open a window that shows what is published |
| `--fullscreen` | Start the preview window in fullscreen |
| `--format <FMT>` | Container of a `file:` video source: `fmp4` (default) or `avc3` |
| `--transcode` | Re-mux or re-encode a `file:` video source through ffmpeg first. A plain MP4 and a looped file need it |

A backend name is the only backend tried: if it is missing, the command fails
and does not fall back to software. Some backends need a feature: `vaapi`,
`nvidia` (for `nvenc`), and `v4l2`. A backend this build lacks fails when the
encoder opens. [Platform support](platforms.md) says which platform has which.

`--relay` attaches the node to the relay and redials it when the session
drops. Every public broadcast of the node goes to the relay. The link only
publishes: it does not add the relay's routes to this node's route table. The
command prints the path at which viewers find the broadcast on the relay. Use a
relay that keeps each publisher to its own paths, as `iroh-live-relay` does. On
one where anyone may publish anywhere, a viewer can be served a forgery.

`--preview` draws the frames that go to the encoders, so it decodes nothing. A
file source and `--video rpicam` cannot be previewed, because their frames
arrive encoded.

`--video rpicam` publishes the H.264 that the Pi's hardware encoder produced.
It refuses `--codec h265`, any `--encoder` other than `auto`, and more than one
rung in `--renditions`. `--width`, `--height`, `--fps`, and `--bitrate` become
`rpicam-vid` arguments. Use `--video rpicam:raw` to encode the pictures here.

### Frame rate

`--fps` sets the capture rate for the whole ladder. An `@<fps>` suffix on a
rung, as in `--renditions 720p@60`, also sets the capture rate. A ladder is
captured once and every rung gets the same frames, so the highest `@<fps>`
wins. With `--renditions high:1280x720@60,low:640x360@30`, both rungs encode at
60, and the log warns about `low`. Where `--fps` and a rung disagree, `--fps`
wins.

With neither, the capture asks for 30. A device that cannot reach the requested
rate uses its nearest rate. On macOS a camera ignores the requested rate, so
nothing is requested there by default.

The log names the rate and where it came from:

```
INFO capture frame rate requested; the device runs at the nearest rate it supports fps=60 origin="--renditions"
```

`origin` is `--fps`, `--renditions`, `--fps, capping the ladder`, `default`, or
`device`. Each rung logs the rate the device delivers on its
`publishing video rendition` line.

## `irl watch`

Subscribes to a broadcast and plays it. `irl play` is an alias.

| Flag | Description |
|---|---|
| `<TICKET>` | The ticket `irl publish` printed |
| `--endpoint-id <ID>` | The publisher's endpoint id, instead of a ticket. Needs `--name` |
| `--name <NAME>` | The broadcast name, with `--endpoint-id` |
| `--no-video` | Play audio only. No window opens |
| `--rendition <NAME>` | Play this rendition only, instead of adapting to the link |
| `--fullscreen` | Start in fullscreen |
| `--scan` | Read the ticket from a QR code held up to the camera |
| `--scan-camera <SPEC>` | The camera `--scan` reads: `cam`, `cam:<id>`, or `rpicam` |
| `--decoder <KIND>` | `auto` (default), `hardware` (`hw`), `software` (`sw`), or one backend: `videotoolbox`, `mediafoundation`, `mediacodec`, `nvdec`, `vaapi`, `v4l2`, `openh264` |
| `--latency <MODE>` | `realtime`, `balanced` (default), or `smooth` |
| `--audio-output <ID>` | Play through this device, by the id `irl devices` prints. Needs `playback` |

With `--scan`, the window opens on the camera picture and connects as soon as
it reads a ticket. The window always has a Scan button, so a player started
with a ticket can switch to another. Without `--scan-camera`, the scanner uses
the Raspberry Pi camera if the build has `rpicam`, and the default camera
otherwise. On a Pi, `/dev/video0` is the raw sensor and never delivers a
picture, which is why the default camera is not the first choice there. Pass
`--scan-camera cam` on a Pi with a USB webcam. If a camera opens and sends
nothing for five seconds, the window says so.

Without `--rendition`, the player picks renditions from the link that serves
the broadcast, a direct session or a relay. A switch opens the new decoder next
to the current one, so the picture does not go blank. The window has a combo box
to switch between adaptive and fixed renditions.

A backend passed to `--decoder` is the only one tried. The window's decoder
combo box changes the decoder during playback, in the same way as a rendition
switch. On a Raspberry Pi 4, `auto` picks the V4L2 hardware decoder, which adds
latency. See [Raspberry Pi](guide/raspberry-pi.md#watching-on-a-pi-4-name-the-decoder).

`--latency` sets how long the player holds each frame before it shows it, and
how far behind it may fall before it skips ahead:

| Mode | Holds | Skips at | For |
|---|---|---|---|
| `realtime` | 60 ms | 100 ms | Conversations |
| `balanced` | 100 ms | 150 ms | The default |
| `smooth` | 400 ms | 600 ms | Watching over Wi-Fi or a mobile link |

## `irl call`

Opens a one-to-one video call. Both sides publish their camera and microphone
as the broadcast `call` and subscribe to the other's.

| Flag | Description |
|---|---|
| `<TICKET>` | The peer's call ticket. Omit to wait for a call |
| `--decoder <KIND>` | As for `irl watch` |
| `--latency <MODE>` | As for `irl watch` |
| `--scan-camera <SPEC>` | The camera the scan screen reads, as for `irl watch` |
| `--no-qr` | Do not print the QR code |
| `--fullscreen` | Start in fullscreen |

Every capture and encoding flag of `irl publish` applies too, and describes
what this node sends. `--decoder` and `--latency` describe how the peer's side
plays.

The window starts on a waiting screen. It shows this node's ticket, a box to
paste the peer's ticket into, and the local camera. Scanning the other side's
QR code also places the call. Once connected, the peer fills the window and
the local camera moves to a corner. Hanging up on either side returns both
windows to the waiting screen.

`irl watch` can play one side of a call, since it is an ordinary broadcast
named `call`.

## `irl room`

Joins a room with several participants. It publishes this node's camera and
microphone into the room, shows the others in a grid, and has a chat panel.

| Flag | Description |
|---|---|
| `<TICKET>` | The room ticket. Omit to open a new room |
| `--display-name <NAME>` | The name the others see (default: this node's short endpoint id) |
| `--decoder <KIND>` | As for `irl watch` |
| `--latency <MODE>` | As for `irl watch` |
| `--no-qr` | Do not print the QR code |
| `--fullscreen` | Start in fullscreen |

Every capture and encoding flag of `irl publish` applies too.

Every participant subscribes to every other, so rooms suit small groups. The
ticket a window prints lists this node as the bootstrap peer, so pass that one
on to the next participant.

Chat is one more broadcast in the room, named `chat`. It carries `moq-room`'s
chat track, which holds the last ten seconds of messages, so a participant who
joins sees what was said in that time.

A tile disappears when its broadcast closes or its session drops. It comes
back if the participant still lists the broadcast. A participant who publishes
nothing gets no tile. See [Rooms](guide/rooms.md).

## `irl record`

Subscribes to a broadcast and writes it to a file. It does not decode: the
encoded frames go straight into the container.

| Flag | Description |
|---|---|
| `<TICKET>` | The ticket `irl publish` printed |
| `--endpoint-id <ID>` | The publisher's endpoint id, instead of a ticket. Needs `--name` |
| `--name <NAME>` | The broadcast name, with `--endpoint-id` |
| `-o`, `--output <PATH>` | The file to write (default: `recording.mp4`) |
| `--format <FMT>` | `fmp4` or `mkv`, overriding the extension of `--output` |
| `--rendition <NAME>` | Record this video rendition only |
| `--duration <SECONDS>` | Stop after this long. Omit to record until Ctrl+C |
| `--latency <MILLISECONDS>` | How long to wait for a stalled group before skipping it (default: 2000) |

The extension picks the container: `.mp4`, `.m4v`, and `.m4s` are fragmented
MP4, and `.mkv` and `.webm` are Matroska. Other extensions need `--format`.
Both containers are fragmented, so a recording that was interrupted or killed
plays up to the last fragment written.

Without `--rendition`, the file has one video track per rung.

## `irl run`

Runs a session from a TOML file: one endpoint that publishes several
broadcasts, subscribes to others, and records any of them. Separate
`irl publish` and `irl watch` processes would each bind their own endpoint.

```sh
irl run session.toml
```

The session opens no window. A `[[recv]]` block plays audio and can record.

```toml
# Optional. Keeps the endpoint id, and so the tickets, across runs. The key is
# stored at <config dir>/iroh-live/secret_keys/<name>.key.
secret_key_name = "studio"

[[send]]
name = "camera"              # broadcast name
video = "cam"                # the other keys are `irl publish` flags,
audio = "mic"                # with the same defaults
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
ticket = "iroh-live:..."     # as `irl publish` printed it
audio_output = "default"     # or "none"
record = "friend.mp4"        # optional, the extension picks the container
rendition = "low"            # optional, the rendition to record
```

A `[[send]]` block needs only `name`, and a `[[recv]]` block needs `name` and
`ticket`. `[[send]]` does not take a `file:` video source. An unknown key is an
error.

A block that fails to start is reported, and the rest of the session runs.
Ctrl+C ends the session and finishes every recording before the endpoint
closes.

## Examples

Publish the default camera and microphone:

```sh
irl publish
```

Publish the timing pattern and the beeps, without any devices:

```sh
irl publish --test-source
```

Photograph the publisher's screen and the player together, and the difference
between the two clocks is the latency. The marker is lit while each beep
sounds, so A/V sync is visible too.

Publish a camera as a two-rung ladder, captured at 60 frames per second:

```sh
irl publish --video cam:/dev/video0 --renditions low:320x180,720p@60
```

Publish a plain MP4:

```sh
irl publish --video file:recording.mp4 --transcode
```

Watch a broadcast:

```sh
irl watch <TICKET>
```

Wait for a call on one machine, and call it from another:

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

Record ten seconds of a broadcast:

```sh
irl record <TICKET> -o clip.mp4 --duration 10
```

The relay is its own binary: `cargo run -p iroh-live-relay`. See
[Browser relay](guide/browser-relay.md).
