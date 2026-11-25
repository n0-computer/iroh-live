# Cargo features

This page is the single list of the workspace's Cargo features. Most
applications depend on `iroh-live` and pick features there. It passes each one
through to the crates below it.

## iroh-live

No feature is on by default. `player` is the receive side, `publisher` the
send side. A device feature turns on its side; a backend or codec feature
turns on no side and applies to whichever side is on, as in moq-cli.

| Feature | Enables | Implies | Platform |
|---|---|---|---|
| `full` | The usual app set | `media`, `capture`, `playback`, `render`, `record`, `pipewire`, `pulseaudio` | all |
| `media` | Both sides of `iroh_live::media` | `player`, `publisher` | all |
| `player` | Receive: `RemoteBroadcast`, `Player`, the decoders, `media::SubscriptionExt` | | all |
| `publisher` | Send: `LocalBroadcast`, the encoders, the sources, the test pattern | | all |
| `capture` | Camera, screen and microphone | `publisher` | all |
| `file` | `MediaFile` for MP4, `AudioSource::file` for WAV, MP3 and MP4 | `publisher` | all |
| `rpicam` | The Raspberry Pi camera through `rpicam-vid` | `publisher` | Linux |
| `playback` | Speaker output, `AudioMode::Play` | `player` | all |
| `render` | GPU rendering through wgpu, re-exported as `media::wgpu` | `player` | all |
| `record` | `RemoteBroadcast::mux_source`, for moq-mux exports | `player` | all |
| `aec` | Echo cancellation for the microphone | `capture`, `playback` | all |
| `pipewire` | PipeWire audio devices; with `publisher` also PipeWire's camera and the portal's screen picker with `PortalGrant`. Links `libpipewire-0.3` | no side | Linux |
| `pulseaudio` | PulseAudio audio devices | no side | Linux |
| `vaapi` | VA-API decode with `player`, encode with `publisher`, Intel and AMD | no side | Linux |
| `nvidia` | NVDEC with `player`, NVENC with `publisher` | no side | Linux |
| `v4l2` | The V4L2 memory-to-memory H.264 codec of ARM SoCs, such as a Pi 4 | no side | Linux |
| `mediacodec` | Android MediaCodec | no side | Android |
| `rooms` | `iroh_live::rooms`, the `iroh-live-rooms` crate | | all |
| `jwt` | `policy::JwtVerifier`, which verifies moq-relay's JWTs | | all |
| `rcan` | `policy::RcanVerifier`, which verifies rcan capability chains | | all |
| `qlog` | A qlog trace of every QUIC connection, written into the directory `QLOGDIR` names. Without `QLOGDIR`, nothing is written | | all |

Without `player` or `publisher`, `iroh-live` is the node, tickets, layouts
and calls, with no codec dependency. A `player`-only build has no
`moq-ext-publisher`; it still builds OpenH264's encoder, since moq-video
vendors OpenH264 as one library.

`player` decodes without a speaker; `playback` adds the speaker.

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
| `pipewire` | yes | PipeWire audio devices, PipeWire's camera as `cam`, and the portal's picker as `screen`, `screen:portal` and `window:portal` (Linux) |
| `pulseaudio` | yes | PulseAudio audio devices (Linux) |
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
| `moq-ext-publisher` | `capture`, `pulseaudio`, `aec`, `pipewire`, `file`, `rpicam`, `vaapi`, `nvidia`, `v4l2` | none |
| `moq-ext-player` | `playback`, `render`, `record`, `aec`, `pipewire`, `pulseaudio`, `vaapi`, `nvidia`, `v4l2` | none |
| `moq-ext-sources` | `pattern`, `file`, `capture`, `pulseaudio`, `aec`, `pipewire`, `rpicam`, `serde` | `pattern` |
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
