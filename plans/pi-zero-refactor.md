# Pi Zero demo refactoring plan

The `demos/pi-zero/` demo has grown into a substantial application with
hardware-specific features. Some of its functionality duplicates patterns in
the `irl` CLI, while other parts belong in library crates where both the
demo and the CLI could share them. This plan identifies what stays, what
moves, and what gaps remain.

## What the pi-zero demo does today

The demo (`pi-zero-demo`) is a multi-command binary with these capabilities:

1. **Publish** (`publish` subcommand)
   - Captures camera via libcamera subprocess (`rpicam-vid`), either as
     pre-encoded H.264 Annex-B or raw YUV for software/V4L2 re-encoding
   - Supports multiple encoder backends: libcamera HW, software openh264,
     V4L2 M2M, and a test pattern mode
   - Configurable resolution presets, bitrate, framerate
   - Optional relay push (for browser clients)
   - Prints connection ticket
   - Optionally displays ticket QR code on e-paper HAT with periodic
     refresh (12 h) and clean shutdown

2. **Watch** (`watch` subcommand)
   - Subscribes to a remote broadcast, decodes, and renders
   - DRM/KMS direct-to-HDMI rendering via GBM + EGL + GLES2 (headless, no
     window system)
   - Optional windowed mode via glutin + winit
   - VT graphics mode handling for clean console output

3. **E-paper** (`epaper-demo` subcommand)
   - Test pattern, QR code, and clear operations on a Waveshare 2.13"
     V4 e-paper HAT
   - Custom V4 EPD driver (`epd_v4.rs`) since `epd-waveshare` only
     supports V2/V3

4. **Framebuffer demo** (`fb-demo` subcommand)
   - Renders a local test pattern to HDMI (no network)

5. **Codec test** (`codec-test` subcommand)
   - On-device V4L2 encoder, decoder, and roundtrip testing
   - Saves decoded frames for visual inspection

## What should stay in the demo

These are tightly coupled to the Pi Zero hardware and UX. They have no
value as library code.

- **E-paper display** (`epaper.rs`, `epd_v4.rs`): Waveshare V4 HAT driver,
  QR rendering, periodic refresh, clean shutdown. The custom V4 protocol
  driver exists because no upstream crate supports it.
- **DRM/KMS rendering** (`watch.rs`): Direct-to-HDMI via DRM/GBM/EGL is
  specific to headless Pi setups. The irl CLI uses wgpu/egui instead.
- **VT mode switching** (`VtGuard`): Console graphics mode for headless
  operation.
- **Codec test harness** (`codec_test.rs`): On-device V4L2 HW codec
  validation. Useful as a standalone diagnostic tool when SSHed into the Pi.
- **Pi-specific build/deploy tooling** (`build.sh`, `prepare.sh`,
  `Makefile.toml`): Cross-compilation for aarch64 and deployment over SSH.

## What should move to library crates

### 1. libcamera capture into `rusty-capture`

The libcamera subprocess capture (`LibcameraH264Source`, `LibcameraYuvSource`)
currently lives in `rusty-codecs/src/libcamera.rs`. This is a capture
source, not a codec, and belongs in `rusty-capture` alongside the V4L2
and PipeWire backends.

Moving it to `rusty-capture` would let the standard `CameraCapturer::new()`
path discover libcamera cameras on Pi automatically, the same way V4L2 and
PipeWire cameras are discovered on desktop Linux. The demo and `irl` CLI
would both benefit without Pi-specific code.

**What to move:**
- `LibcameraYuvSource` to `rusty-capture` as a new backend behind a
  `libcamera` feature flag
- `LibcameraH264Source` is trickier because it produces pre-encoded H.264,
  not raw frames. It could remain in `rusty-codecs` as a `PreEncodedVideoSource`
  or move to `rusty-capture` with a separate trait. The pre-encoded path
  is the preferred one on Pi Zero because it avoids piping raw YUV.

**Effort:** Medium. The subprocess approach (`rpicam-vid`) is already
encapsulated cleanly. The main work is integrating it into the
`CameraCapturer` backend dispatch and `CaptureBackend` enum.

### 2. V4L2 M2M encoder integration

The V4L2 M2M encoder (`V4l2Encoder`, `V4l2Decoder`) is already in
`rusty-codecs` and accessible via the `v4l2` feature. The pi-zero demo
uses it correctly. No code needs to move here.

However, `irl publish --codec v4l2-h264` is already wired up in the CLI
via `VideoCodec::V4l2H264`. The gap is that the `irl` CLI does not know
about `libcamera` as a capture source, so it cannot replicate the
demo's preferred `--encoder libcamera` path.

### 3. Audio capture patterns

The pi-zero demo does not publish audio at all. `pi-zero-minimal` adds
best-effort audio using the standard `AudioBackend::default_input()` path,
which is the same pattern `irl publish` uses. No duplication here.

### 4. Relay push

Both the demo (`--relay <endpoint-id>`) and `irl publish --relay <id>`
support relay push. The demo uses a slightly different codepath
(`session.publish()` directly) while the CLI uses `transport::publish_broadcast`.
The underlying mechanism is the same. No code needs to move, but the
demo could switch to the shared transport setup if desired.

## What `irl` can already do

The `irl` CLI (`iroh-live-cli`) supports:

- **`irl publish`**: Camera, screen, file, and test-pattern sources.
  Multiple video sources, simulcast presets, all software and hardware
  codecs (H.264, AV1, VAAPI, V4L2, VideoToolbox). Default mic audio
  with codec and preset selection. Relay push, room publish, QR ticket
  display in terminal, egui preview window.

- **`irl play`**: Subscribe and render with egui/wgpu. Hardware and
  software decoder selection, fullscreen, audio device routing.

- **`irl record`**: Headless subscribe-and-save to raw bitstream files.

- **`irl call`**: Bidirectional 1:1 call with egui UI.

- **`irl room`**: Multi-party room with grid layout.

- **`irl devices`**: Lists cameras, screens, audio devices, codecs.

## Gap analysis

What the pi-zero demo can do that `irl` currently cannot:

| Feature | pi-zero demo | irl CLI | Gap |
|---------|-------------|---------|-----|
| libcamera capture | Yes (rpicam-vid subprocess) | No | `irl` has no libcamera backend; it relies on V4L2/PipeWire in `rusty-capture` |
| Pre-encoded H.264 from HW | Yes (`--encoder libcamera`) | No | `irl publish` only does raw-capture + encode; no `PreEncodedVideoSource` path |
| DRM/KMS HDMI rendering | Yes (`watch --fb`) | No | `irl play` uses egui/wgpu, which needs a window system |
| E-paper QR display | Yes (`--epaper`) | No (terminal QR only) | Pi-specific, should stay in demo |
| On-device codec testing | Yes (`codec-test`) | No | Diagnostic tool, should stay in demo |

### Path to closing the gaps

1. **libcamera capture in `rusty-capture`**: Add a `libcamera` feature
   that wraps `rpicam-vid` as a `VideoSource` backend. Once in place,
   `irl publish` on a Pi would "just work" with `--video cam`. The
   pre-encoded path needs a parallel `--pre-encoded` flag or a way for
   the capture backend to signal that it produces encoded output.

2. **Pre-encoded source support in `irl publish`**: The publish pipeline
   already supports `VideoInput::pre_encoded()` in `moq-media`. The
   `irl` CLI just needs a `--video libcamera-hw` source spec (or
   similar) that wires up `LibcameraH264Source`.

3. **Headless rendering**: Not a realistic `irl` feature. DRM/KMS
   rendering is fundamentally different from the egui/wgpu path. The
   demo should keep this.

### What pi-zero-minimal covers

The new `demos/pi-zero-minimal/` binary covers the simplest use case:
capture from the first available V4L2/PipeWire camera, optional mic,
publish, print ticket. It works on both x86_64 (for development) and
aarch64 (for Pi), and uses the same `CameraCapturer::new()` +
`AudioBackend::default_input()` pattern as `irl publish`.

Once libcamera support lands in `rusty-capture`, pi-zero-minimal would
automatically pick it up without changes.
