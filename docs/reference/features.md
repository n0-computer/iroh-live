# Cargo features

This page is the single list of the workspace's Cargo features. Most
applications depend on `iroh-live` and pick features there. It passes each one
through to the crates below it.

## iroh-live

No feature is on by default.

| Feature | Enables | Implies | Platform |
|---|---|---|---|
| `full` | What an app that publishes and plays wants | `media`, `capture`, `render`, `sound-server`, `record` | all |
| `media` | `iroh_live::media` and `media::SubscriptionExt`: sources, publisher, player, codecs. H.264 in software always works | | all |
| `capture` | Camera, screen and microphone capture | `media` | all |
| `playback` | Speaker output, `AudioMode::Play` | `media` | all |
| `render` | GPU rendering of decoded frames through wgpu | `media` | all |
| `record` | `RemoteBroadcast::mux_source`, for moq-mux exports | `media` | all |
| `file` | `AudioSource::file`, WAV and MP3 files | `media` | all |
| `sound-server` | Audio devices through PipeWire or PulseAudio, not only ALSA | `media` | Linux |
| `aec` | Echo cancellation for the microphone | `capture`, and the player's speaker output | all |
| `pipewire` | PipeWire's shared camera as the default camera, and the desktop portal's screen picker. Links `libpipewire-0.3` | `media`, and capture in the media crates | Linux |
| `rpicam` | The Raspberry Pi camera through `rpicam-vid` | `media` | Linux |
| `vaapi` | VA-API hardware encode and decode, Intel and AMD | `media` | Linux |
| `nvidia` | NVENC and NVDEC | `media` | Linux |
| `v4l2` | The V4L2 memory-to-memory H.264 codecs of ARM SoCs, such as a Pi 4 | `media` | Linux |
| `rooms` | `iroh_live::rooms`, the `iroh-live-rooms` crate | | all |
| `jwt` | `policy::JwtVerifier`, which verifies moq-relay's JWTs | | all |
| `rcan` | `policy::RcanVerifier`, which verifies rcan capability chains | | all |

Without `media`, `iroh-live` is the node, tickets, layouts and calls, with no
codec dependency.

A Linux-only feature compiles to nothing on other platforms. macOS, Windows
and Android get their capture and codec frameworks without a feature.

H.265 and AV1 have no software decoder, and H.265 has no software encoder.
They need a hardware backend, such as `vaapi` or `nvidia` on Linux or
VideoToolbox on macOS. See
[Platform support](platforms.md).

## iroh-live-cli

`irl` always has `capture`, `file`, `render`, `rooms` and `record`. Every
other feature is on by default, so `cargo install --path iroh-live-cli` builds
every backend the platform can reach.

| Feature | Default | Effect |
|---|---|---|
| `aec` | yes | Echo cancellation for `irl call` and `irl room` |
| `playback` | yes | Audio playback |
| `sound-server` | yes | PipeWire and PulseAudio devices (Linux) |
| `pipewire` | yes | PipeWire's camera as `cam`, and the portal's picker as `screen`, `screen:portal` and `window:portal` (Linux) |
| `rpicam` | yes | `--video rpicam` (Linux) |
| `vaapi`, `nvidia` | yes | Hardware encode and decode (Linux) |
| `v4l2` | yes | `--encoder v4l2` and the matching decoder (Linux) |

On Linux, the default build needs `libpipewire` and `libclang`, see
[Install](../getting-started/install.md#build-irl-from-source).

## The lower crates

Applications that skip the facade pick the same features on the crates that
define them:

| Crate | Features | Default |
|---|---|---|
| `moq-ext-publisher` | `capture`, `sound-server`, `aec`, `pipewire`, `rpicam`, `vaapi`, `nvidia`, `v4l2` | none |
| `moq-ext-player` | `playback`, `render`, `record`, `aec`, `vaapi`, `nvidia`, `v4l2` | none |
| `moq-ext-sources` | `pattern`, `file`, `capture`, `sound-server`, `aec`, `pipewire`, `rpicam` | `pattern` |
| `moq-ext-access` | `jwt`, `rcan` | none |
| `iroh-live-node` | `jwt`, `rcan`, forwarded to `moq-ext-access` | none |

`moq-ext-publisher` and `moq-ext-player` encode and decode with no feature at
all. They cannot open a device or draw until `capture`, `playback` or
`render` is on. The `test-util` features of `iroh-live-node`,
`moq-ext-player` and `iroh-live-rooms` hold test helpers and are not a public
API.
