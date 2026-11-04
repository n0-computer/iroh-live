# client-gpu demo

A client sends live video to a headless "gpu" node. The node applies a CPU
filter the client selects, draws a wall-clock watermark, and sends the video
back. The client shows what it sent next to what came back, and measures the
time between the two.

Everything runs peer to peer over iroh and MoQ. No relay of ours is involved.

## Run

Start the gpu node. It prints its endpoint id:

```sh
cargo run -p client-gpu-demo --bin gpu
```

Start the client with that id. It opens a window:

```sh
cargo run -p client-gpu-demo --bin client <endpoint id>
```

Use `--release` for numbers that mean something: the debug build spends most
of the latency in the encoder and the blur.

Without a window, the client prints the latency statistics once a second:

```sh
cargo run -p client-gpu-demo --bin client <endpoint id> --headless 30 --filter blur
```

The gpu node takes `--kbps` for the bitrate of the returned video. Set
`RUST_LOG=info` to see what it does. On one machine the two find each other
over mDNS or the local addresses. Across networks they need the iroh relay and
discovery the default preset uses.

## The window

The left panel has:

- **GPU filter** (live): none, invert or a box blur. It travels on a control
  track, see below.
- **Video** (live): source (test pattern, camera, screen), rendition (180p,
  360p, 720p) and target bitrate. "Apply video" stops the source and starts a
  new one on the same broadcast. The test pattern works everywhere.
- **Returned video latency** (live): the player's minimum and maximum latency
  for the video that comes back. `min` is how long each picture is held for
  jitter, `max` how old a picture may be before it is skipped.
- **Congestion controller** (reconnect): BBR3, CUBIC or NewReno on the
  client's endpoint. A controller is part of the QUIC transport config of an
  endpoint, so switching tears the session down and builds a new one with the
  current settings. It shapes only what the client sends. The gpu node always
  uses BBR3.

## How it works

```
client                                         gpu node
------                                         --------
source -> stamp frame id -> encode ----------> decode (software, no latency buffer)
           (capture time kept locally)              |
                                                    v
                                             filter, watermark, re-stamp id
                                                    |
render <- decode <-------------------------------- encode
 |
 v
latency = now - capture time of the frame with that id
```

Each side offers its broadcast as a call, `Live::call`, to the other, and
answers the other's, as `irl call` does. The gpu node learns of a client when
the client's `live/<id>/call` appears in its route table. The control track is a
second small broadcast, `live/<client id>/ctl`, offered to the gpu node only.
Each frame on its one track is the filter name. The client sends it on a change
and once a second.

## What the latency number measures

It starts when a frame leaves the capture source in the client, before it is
stamped and encoded. We cannot see how long the camera took before that. It
ends right after the client uploads the returned picture to its GPU texture for
drawing. Display scanout after that is not included. The path in between is the
client's stamp and encode, the network, the gpu node's decode, filter, watermark
and encode, the network back, and the client's jitter buffer and decode.

To match a returned picture with the one sent, the client paints a 16 bit word
in the top left corner of every frame: a 12 bit frame id and a 4 bit check,
as black and white blocks (the strip you see in both views). The gpu node reads
the word before filtering and paints it again after. The client reads it from
the returned frame and looks up the capture instant of that id. The pixels
carry the id because a timestamp would not: the publisher rebases every
source timestamp onto its own clock at the first frame, so the pts of a
returned frame has no fixed relation to the client's capture time. The block
width is a fraction of the frame width, so the word survives other renditions,
and the check rejects what the encoder garbles. The client and the gpu node
need no synchronised clocks, since both ends of the measurement are on the
client.

The statistics are over the last 600 samples. The plot shows them, with the
median in gray and the 95th percentile in orange.

## Known limits

- The filters and the watermark run on the CPU, in I420. The decoders are
  forced to software so that frames have CPU pixels. Hardware decoders and
  GPU surfaces that cannot be read back are not supported.
- The gpu node serves one client at a time: its call offer lives at one path, so a second client is refused until the first leaves.
- The gpu node's returned video has a fixed rendition: the size of the input
  and `--kbps`. Only the client's own video is tunable from the window.
- Changing the source stops the old one before opening the new one, so there is
  a short gap, and a failure to open shows as a notice with the video stopped.
- The screen source is the main display. On Linux that needs X11 or XWayland.
- Switching the congestion controller at runtime is not possible, so it
  reconnects. The reconnect creates a new endpoint and so a new endpoint id for
  the client, unless `IROH_SECRET` is set.
- The frame-id strip is part of the picture, in both views.
- Latency of a frame that the player or the gpu node drops is not sampled. A
  stream that drops many frames shows fewer samples, not worse numbers.
