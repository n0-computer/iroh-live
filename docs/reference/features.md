# Cargo features

This page is the single list of the workspace's Cargo features. Most
applications depend on `iroh-live` and pick features there. It passes each one
through to the crates below it.

## iroh-live

No feature is on by default.

| Feature | Enables | Implies | Platform |
|---|---|---|---|
| `full` | What an app that publishes and plays wants | `media`, `capture`, `playback`, `render`, `sound-server`, `record` | all |
| `media` | `iroh_live::media` and `media::SubscriptionExt`: sources, publisher, player, codecs. H.264 in software always works | `play`, `publish` | all |
| `play` | The player half of `iroh_live::media`: `RemoteBroadcast`, `Player`, `media::SubscriptionExt` and the decoders, without `moq-ext-publisher` | | all |
| `publish` | The publisher half of `iroh_live::media`: `LocalBroadcast`, the encoders and the sources, the test pattern among them, without `moq-ext-player` | | all |
| `capture` | Camera, screen and microphone capture | `media` | all |
| `playback` | Speaker output, `AudioMode::Play` | `play` | all |
| `render` | GPU rendering of decoded frames through wgpu, re-exported as `media::wgpu` | `play` | all |
| `record` | `RemoteBroadcast::mux_source`, for moq-mux exports | `play` | all |
| `file` | `MediaFile` for MP4, `AudioSource::file` for WAV, MP3 and MP4 | `media` | all |
| `sound-server` | Audio devices through PipeWire or PulseAudio, not only ALSA | `media` | Linux |
| `aec` | Echo cancellation for the microphone | `capture`, and the player's speaker output | all |
| `pipewire` | PipeWire's shared camera as the default camera, and the desktop portal's screen picker with `PortalGrant`, which keeps its grant across runs. Links `libpipewire-0.3` | `media`, and capture in the media crates | Linux |
| `rpicam` | The Raspberry Pi camera through `rpicam-vid` | `media` | Linux |
| `vaapi` | VA-API hardware decode, and encode with `publish`, Intel and AMD | `play` | Linux |
| `nvidia` | NVDEC, and NVENC with `publish` | `play` | Linux |
| `v4l2` | The V4L2 memory-to-memory H.264 decoder of ARM SoCs, such as a Pi 4, and the encoder with `publish` | `play` | Linux |
| `rooms` | `iroh_live::rooms`, the `iroh-live-rooms` crate | | all |
| `jwt` | `policy::JwtVerifier`, which verifies moq-relay's JWTs | | all |
| `rcan` | `policy::RcanVerifier`, which verifies rcan capability chains | | all |
| `qlog` | A qlog trace of every QUIC connection, written into the directory `QLOGDIR` names. Without `QLOGDIR`, nothing is written | | all |

Without `media`, `play` or `publish`, `iroh-live` is the node, tickets,
layouts and calls, with no codec dependency.

An app that only subscribes and decodes, such as a server that receives
streams, takes `play` instead of `media`, plus `render`, `vaapi` or `nvidia`
as it needs them. It then builds no `moq-ext-publisher` and no test pattern.
It still builds OpenH264's encoder: moq-video's `openh264` feature vendors
OpenH264 as one C library, encoder and decoder together, and has no
decode-only option. The device features, `capture`, `file`, `sound-server`,
`pipewire` and `rpicam`, imply all of `media`.

A Linux-only feature compiles to nothing on other platforms. macOS, Windows
and Android get their capture and codec frameworks without a feature.

H.265 and AV1 have no software decoder, and H.265 has no software encoder.
They need a hardware backend, such as `vaapi` or `nvidia` on Linux or
VideoToolbox on macOS. See
[Platform support](platforms.md).

## iroh-live-cli

`irl` always has `capture`, `file`, `playback`, `render`, `rooms` and
`record`. Every other feature is on by default, so
`cargo install --path iroh-live-cli` builds every backend the platform can
reach.

| Feature | Default | Effect |
|---|---|---|
| `aec` | yes | Echo cancellation for `irl call` and `irl room` |
| `sound-server` | yes | PipeWire and PulseAudio devices (Linux) |
| `pipewire` | yes | PipeWire's camera as `cam`, and the portal's picker as `screen`, `screen:portal` and `window:portal` (Linux) |
| `rpicam` | yes | `--video rpicam` (Linux) |
| `vaapi`, `nvidia` | yes | Hardware encode and decode (Linux) |
| `v4l2` | yes | `--encoder v4l2` and the matching decoder (Linux) |
| `qlog` | no | A qlog trace of every connection into `QLOGDIR` |

On Linux, the default build needs `libpipewire` and `libclang`, see
[Install](../getting-started/install.md#build-irl-from-source).

## The lower crates

Applications that skip the facade pick the same features on the crates that
define them:

| Crate | Features | Default |
|---|---|---|
| `moq-ext-publisher` | `capture`, `sound-server`, `aec`, `pipewire`, `file`, `rpicam`, `vaapi`, `nvidia`, `v4l2` | none |
| `moq-ext-player` | `playback`, `render`, `record`, `aec`, `vaapi`, `nvidia`, `v4l2` | none |
| `moq-ext-sources` | `pattern`, `file`, `capture`, `sound-server`, `aec`, `pipewire`, `rpicam`, `serde` | `pattern` |
| `moq-ext-access` | `jwt`, `rcan` | none |
| `iroh-live-node` | `jwt`, `rcan`, forwarded to `moq-ext-access`, and `qlog`, forwarded to `iroh` | none |

`moq-ext-publisher` and `moq-ext-player` encode and decode with no feature at
all. They cannot open a device or draw until `capture`, `playback` or
`render` is on. The `test-util` features of `iroh-live-node`,
`moq-ext-player` and `iroh-live-rooms` hold test helpers and are not a public
API.

## qlog

With `qlog` on, every endpoint bound with `MoqPreset` or `EndpointConfig`, or
given `Congestion::transport_config`, writes one qlog file per QUIC connection
into the directory the `QLOGDIR` environment variable names. The variable is
read, and the directory created, when the transport config is built: as
`EndpointConfig::bind` binds, or as `MoqPreset` is applied to a builder.
Without `QLOGDIR` nothing is written, and setting it later has no effect on an
endpoint already built.
`iroh-live`, `irl` (`iroh-live-cli`) and `iroh-live-relay` forward the
feature. The relay traces its iroh connections, but not the browsers'.

Each file is named `iroh-live-<unix ms>-<connection id>-<client|server>.qlog`.
The connection id is the client's first destination id, so the client and
server files of one connection share it, and you can pair the two ends. Tools
such as [qvis](https://qvis.quictools.info/) show the packets, losses,
congestion window and RTT, and so where a stalled connection stops sending.

```sh
cargo install --path iroh-live-cli --features qlog
QLOGDIR=./qlog irl publish
```

Disk use has no limit. The trace logs every packet each side sends and
receives, so a file is about as large as the data of its connection, and
nothing rotates, caps or deletes the files. A publisher that sends 3 Mbit/s
writes about 1.3 GB per hour for each subscriber. Set `QLOGDIR` only while
you diagnose, and do not leave it set on a long-running node.

Two more things to know before you turn it on:

- `QLOGDIR` is a shared name. quiche, ngtcp2 and curl read it too, so a
  `QLOGDIR` set for one of them in a shell profile or service unit also
  starts the traces here.
- The traces hold the peers' IP addresses. Check that before you attach one
  to a public issue.
