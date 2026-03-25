# CLI Tool Consolidation Plan

Consolidate examples (publish, push, watch, call, rooms, room-publish-file)
into a single CLI tool. The `split` and `watch-wgpu` examples stay as-is
(split for patchbay debugging, watch-wgpu for minimal non-egui rendering).

**Crate:** `tools/iroh-live-cli`
**Binary:** `irl`

Other name candidates considered: ilk, liv, molt, lux, iroh-av.

## Commands

```
irl devices                              # list cameras, screens, audio, codecs
irl publish [capture] [source-opts]      # publish live capture (default subcommand)
irl publish file <FILE> [file-opts]      # publish media file
irl play <TICKET>                        # subscribe and render
irl call [TICKET]                        # 1:1 bidirectional call
irl room [TICKET]                        # multi-party room
```

### Transport model

All publish paths share the same transport flags:

```
--name <STRING>            broadcast name (default: "hello")
--relay <ID_OR_URL>        additionally push to relay
--room <ROOM_TICKET>       additionally publish into a room
--no-serve                 don't accept incoming connections (push-only)
--no-qr                    suppress terminal QR code
```

By default, `publish` serves locally (binds endpoint, accepts incoming
subscribers, prints ticket). `--relay` and `--room` are additive — they
push to additional targets on top of local serving. `--no-serve` disables
the incoming-connection path for push-only scenarios.

No `--log-level`; use `RUST_LOG` env var.

### `irl devices`

Lists available input/output devices grouped by type, prints to stdout, exits.

```
cameras:
  0: Integrated Camera (default)
  1: USB Webcam

screens:
  0: eDP-1 — 2560×1600 (default)
  1: HDMI-1 — 1920×1080

audio inputs:
  0: Built-in Mic (default)
  1: USB Audio

audio outputs:
  0: Built-in Speakers (default)
  1: HDMI Audio
```

### `irl publish`

Publishes live capture sources. Default is camera + mic, headless (no window).
Add `--preview` to open an egui window with local video and controls.

```
irl publish                                 # cam + mic, headless
irl publish --preview                       # cam + mic with egui preview
irl publish --screen                        # screen + mic
irl publish --video none                    # audio only (mic)
irl publish --video cam:1 --audio 1         # specific devices by id
irl publish --video screen:pipewire:eDP-1   # specific screen backend + id
irl publish --test-source                   # synthetic SMPTE pattern
```

**Source selection:**

```
--video <SOURCE>      # cam[:<id>] | screen[:<backend>:<id>] | test | none
--audio <SOURCE>      # <id> | none
```

Multiple `--video` and `--audio` flags allowed for multi-source publish.
`--screen` is shorthand for `--video screen`. Bare `irl publish` defaults to
first camera + default mic.

**Encoding:**

```
--codec <CODEC>               # h264 | av1 (default: best available)
--video-presets <LIST>         # comma-separated: p180,p360,p720 (simulcast)
--audio-preset <PRESET>       # lq | hq (default: hq)
```

**Other:**

```
--preview                     # open egui window with local preview + controls
--no-qr                       # suppress terminal QR code
```

**Clap design:** Flat `PublishArgs` struct. `--video`/`--audio` are
`Vec<String>` parsed into typed `SourceSpec` after clap. This keeps clap
simple while supporting the expressive grammar.

```rust
#[derive(Args)]
struct PublishArgs {
    #[clap(long)]
    video: Vec<String>,
    #[clap(long)]
    audio: Vec<String>,
    #[clap(long)]
    screen: bool,
    #[clap(long)]
    test_source: bool,
    #[clap(long)]
    codec: Option<VideoCodec>,
    #[clap(long, value_delimiter = ',')]
    video_presets: Option<Vec<VideoPreset>>,
    #[clap(long)]
    audio_preset: Option<AudioPreset>,
    #[clap(long)]
    preview: bool,
    #[clap(long)]
    no_qr: bool,
}
```

### `irl file`

Publishes a media file. Separate command because the option set is different
(format, transcode, stdin), but shares `SharedArgs` (relay, name) and the
underlying broadcast publish code.

```
irl file video.mp4                        # auto-detect format
irl file --transcode video.mkv            # re-encode via ffmpeg
cat stream.h264 | irl file --format avc3  # stdin
irl file video.mp4 --room <TICKET>        # publish into a room
```

```
<FILE>                        # positional, optional (stdin if omitted)
--format <FMT>                # fmp4 | avc3 (default: fmp4)
--transcode                   # re-encode with ffmpeg
--room <ROOM_TICKET>          # publish into a room instead of standalone
```

### `irl play`

Subscribe and render. Replaces `watch.rs`.

```
irl play <TICKET>                   # egui window, auto decoder
irl play --endpoint-id X --name Y   # ticket alternative
irl play <TICKET> --video none      # audio only, no window opened
irl play <TICKET> --decoder sw      # force software decoder
irl play <TICKET> --audio-device 1  # specific audio output
```

```
<TICKET>                            # positional (or --endpoint-id + --name)
--video none                        # audio only, no window opened
--decoder <BACKEND>                 # auto | sw (default: auto)
--audio-device <ID>                 # audio output device
--fullscreen                        # start fullscreen
```

### `irl call`

1:1 bidirectional call. Combines publish + play. Inherits publish source and
encoding options plus play-side decoder/audio options via `#[clap(flatten)]`.

```
irl call                            # setup screen, wait for peer
irl call <TICKET>                   # auto-dial
irl call --video screen             # share screen
```

### `irl room`

Multi-party room. Same flatten pattern as call.

```
irl room                            # create new room
irl room <TICKET>                   # join existing
irl room <TICKET> --screen          # share screen
```

## What gets removed from examples/

| Example | Disposition |
|---------|------------|
| `publish.rs` | **Removed** — merged into `irl publish` |
| `push.rs` | **Removed** — merged into `irl file` |
| `watch.rs` | **Removed** — merged into `irl play` |
| `call.rs` | **Removed** — merged into `irl call` |
| `rooms.rs` | **Removed** — merged into `irl room` |
| `room-publish-file.rs` | **Removed** — merged into `irl file --room` |
| `common/` | **Removed** — import/transcode code moves into `irl file`; remaining examples don't use it (split has a dead `mod common;` line to clean up) |
| `watch-wgpu.rs` | **Kept** — minimal non-egui wgpu renderer, different purpose |
| `frame_dump.rs` | **Kept** — test/debug utility |
| `subscribe_test.rs` | **Kept** — CI helper |
| `split.rs` | **Kept** — patchbay debug tool |

## UI Screens

All video windows share a common overlay framework built on `moq-media-egui`.
The shared UI code lives in `ui.rs` and provides chrome, controls, and layout
helpers that each command composes.

### Chrome (all modes)

```
┌───────────────────────────────────────────────────┐
│ [ticket: iroh:abc123...]                     [⛶]  │  ← top bar
│ [NET: 45ms ▸] [RENDER: h264 720p ▸] [TIME: ▸]   │  ← debug overlay (collapsed)
│                                                    │
│                   VIDEO AREA                       │
│                                                    │
│ [controls: src ▾] [codec ▾] [rendition ▾] ...    │  ← bottom bar
└────────────────────────────────────────────────────┘
```

- **Top bar:** ticket string (click to copy), fullscreen toggle (top-right).
  Translucent overlay, same style as existing stat bars.
- **Debug overlay:** sits below the top bar. Collapsed by default, shows
  section headers (NET, CAPTURE, RENDER, TIME) with a key metric each.
  Click to expand full detail panels. Keeps the bottom bar uncluttered.
- **Bottom bar:** mode-specific controls only (source selectors, rendition,
  codec, etc.). Always visible in interactive modes (publish --preview, call,
  room). In play mode, all bars appear on mouse movement and hide after
  ~2 seconds of stillness.

The controls themselves are shared functions that each screen composes:
`source_selector()`, `codec_selector()`, `preset_selector()`,
`rendition_selector()`, `decoder_selector()`, `audio_selector()`. Each takes
relevant state and returns whether something changed (triggering a re-publish
or re-subscribe). This avoids duplicating control logic across commands.

### `irl play` screen

```
┌───────────────────────────────────────────────────┐
│ [ticket: iroh:abc123...]                     [⛶]  │
│ [NET ▸] [RENDER ▸] [TIME ▸]                      │
│                                                    │
│              REMOTE VIDEO (fills window)           │
│                                                    │
│ [rendition ▾] [decoder ▾]                         │
└────────────────────────────────────────────────────┘
```

All bars auto-hide when mouse is still.

### `irl publish --preview` screen

```
┌───────────────────────────────────────────────────┐
│ [ticket: iroh:abc123...]                     [⛶]  │
│ [CAPTURE ▸] [NET ▸]                              │
│                                                    │
│             LOCAL VIDEO PREVIEW                    │
│                                                    │
│ [src ▾] [codec ▾] [presets ▾] [audio ▾]          │
└────────────────────────────────────────────────────┘
```

Bars always visible (interactive mode).

### `irl call` screen

```
┌───────────────────────────────────────────────────┐
│ [ticket: iroh:abc123...]                     [⛶]  │
│ [NET ▸] [CAPTURE ▸] [RENDER ▸] [TIME ▸]         │
│                                                    │
│              REMOTE VIDEO (fills window)           │
│                                       ┌──────────┐│
│                                       │  LOCAL   ││
│                                       │   PiP    ││
│                                       └──────────┘│
│ [src ▾] [codec ▾] [rendition ▾] [audio ▾]        │
└────────────────────────────────────────────────────┘
```

Remote fills window, local PiP bottom-right (~240px wide, 16:9).

### `irl room` screen

Grid mode (default, self included in grid):
```
┌───────────────────────────────────────────────────┐
│ [ticket: iroh:abc123...]                     [⛶]  │
│ [NET ▸] [RENDER ▸] [TIME ▸]                      │
│ ┌──────────┐ ┌──────────┐ ┌──────────┐           │
│ │  PEER A  │ │  PEER B  │ │   SELF   │           │
│ └──────────┘ └──────────┘ └──────────┘           │
│ ┌──────────┐ ┌──────────┐                         │
│ │  PEER C  │ │  PEER D  │                         │
│ └──────────┘ └──────────┘                         │
│ [src ▾] [codec ▾] [self: grid | pip ▾]            │
└────────────────────────────────────────────────────┘
```

PiP mode (self as overlay, peers fill grid):
```
┌───────────────────────────────────────────────────┐
│ [ticket: iroh:abc123...]                     [⛶]  │
│ [NET ▸] [RENDER ▸] [TIME ▸]                      │
│ ┌─────────────────┐ ┌─────────────────┐          │
│ │     PEER A      │ │     PEER B      │  ┌─────┐ │
│ └─────────────────┘ └─────────────────┘  │SELF │ │
│ ┌─────────────────┐ ┌─────────────────┐  │ PiP │ │
│ │     PEER C      │ │     PEER D      │  └─────┘ │
│ └─────────────────┘ └─────────────────┘          │
│ [src ▾] [codec ▾] [self: grid | pip ▾]            │
└────────────────────────────────────────────────────┘
```

Toggle switches self between grid participant and PiP overlay.

## Module structure

```
tools/iroh-live-cli/
├── Cargo.toml
└── src/
    ├── main.rs              # clap CLI, subcommand dispatch
    ├── args.rs              # SharedArgs, PublishArgs, PlayArgs, FileArgs,
    │                        #   CallArgs, RoomArgs, SourceSpec parsing
    ├── transport.rs         # shared: setup_live(), publish_broadcast(),
    │                        #   publish_producer(), print_ticket()
    ├── source.rs            # shared: setup_video(), setup_audio() for capture
    ├── import.rs            # shared: Import decoder, ffmpeg transcode, is_h264
    ├── ui.rs                # shared UI: top_bar(), fullscreen_button(),
    │                        #   MouseHide, DeviceSelectors, RemoteView
    ├── devices.rs           # devices command: list codecs, cameras, screens, audio
    ├── publish.rs           # publish command (thin: transport + source)
    ├── file.rs              # file command (thin: transport + import)
    ├── play.rs              # play command: subscribe, egui render, audio-only
    ├── call.rs              # call command: dial/accept, PiP, uses DeviceSelectors + RemoteView
    └── room.rs              # room command: gossip join, peer grid, self-view toggle
```

Key architectural decisions:

- **transport.rs** handles the serve/connect duality that both `publish` and
  `file` share: bind endpoint, publish locally, optionally push to relay,
  print ticket with QR. Both commands call the same transport functions with
  different broadcast types (`LocalBroadcast` vs `BroadcastProducer`).

- **ui.rs** provides `DeviceSelectors` (video source, audio in/out, codec,
  preset) used by call, room, and future publish --preview. `RemoteView`
  wraps a `RemoteBroadcast` with video rendering, resubscribe logic,
  rendition selector, and debug overlay — used by play, call, and room.

- **Async consistency**: `#[tokio::main]` on the entry point. GUI commands
  (play, call, room) do async setup in the tokio context, then call
  `eframe::run_native()` which blocks the main thread. Inside eframe,
  `tokio::runtime::Handle::current()` spawns async work on the still-running
  worker threads. No duplicate runtimes.

## Feature flags

```toml
[features]
default = [
    "h264", "opus", "capture", "wgpu",
    "vaapi", "videotoolbox", "dmabuf-import", "metal-import",
]

# Codecs — delegate to moq-media
h264 = ["moq-media/h264"]
opus = ["moq-media/opus"]
av1 = ["moq-media/av1"]

# Hardware acceleration
vaapi = ["moq-media/vaapi"]
videotoolbox = ["moq-media/videotoolbox"]
dmabuf-import = ["moq-media/dmabuf-import"]
metal-import = ["moq-media/metal-import"]
v4l2 = ["moq-media/v4l2"]

# Capture
capture = ["moq-media/capture"]

# Rendering
wgpu = ["moq-media/wgpu", "dep:moq-media-egui"]
```

All features delegate to `moq-media`; no `rusty-codecs` references needed.
`av1` not in default (matches workspace convention). Dependencies use
`{ workspace = true }` for all in-repo crates.

## Implementation order

1. **Scaffold** — crate, Cargo.toml, main.rs with clap subcommands, args.rs
2. **`devices`** — straightforward, no UI
3. **`publish`** — headless: source setup, broadcast, QR
4. **`play`** — subscribe, egui render, audio-only path
5. **`ui.rs`** — shared chrome, control widgets, auto-hide
6. **`publish --preview`** — egui window reusing ui.rs
7. **`file`** — import, format, transcode (moves common/import.rs code here)
8. **`call`** — combine publish + play, PiP, dial/accept
9. **`room`** — combine publish + play, grid, self-view toggle
10. **Cleanup** — remove merged examples, remove common/, clean dead mod line in split.rs

## Future work: Raspberry Pi

The pi-zero demo currently has several platform-specific features that don't
map cleanly onto the general CLI:

| Pi-zero feature | What it does | Path to irl support |
|-----------------|-------------|---------------------|
| **libcamera capture** | CSI camera via `rpicam-vid`, pre-encoded H.264 or YUV | Abstract into `rusty-capture` behind a `libcamera` feature flag |
| **V4L2 M2M encode** | Hardware H.264 encoder on Pi's V4L2 device | Already in `rusty-codecs` via `v4l2` feature, works today |
| **DRM/KMS rendering** | Direct HDMI output via GBM + EGL/GLES2 (674 loc) | Abstract into a `gles` rendering backend in `rusty-codecs` or `moq-media-egui` |
| **Windowed GLES2** | glutin + winit + glow | Same GLES2 abstraction would cover this |
| **E-paper QR** | Waveshare 2.13" HAT via SPI | Pi-specific, stays in pi-zero demo |
| **Codec test** | V4L2 encoder/decoder PSNR benchmark | Hardware validation, stays in pi-zero demo |
| **Framebuffer demo** | Test pattern direct to HDMI | Hardware validation, stays in pi-zero demo |

Once libcamera capture and GLES2 rendering are abstracted into the library
crates, `irl` gains Pi support without any platform-specific code in the CLI
itself. Until then, the pi-zero demo stays as a separate binary sharing
library code.

**What works on Pi today without changes:**
`irl devices` (V4L2 listing), `irl publish --test-source` (software encode),
`irl file` (no platform deps), `irl play --video none` (audio only).
