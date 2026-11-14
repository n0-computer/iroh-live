# Troubleshooting and FAQ

Each entry starts from what you see. For more detail, run the command again
with `RUST_LOG=info`, or with a filter from
[Environment variables](reference/environment.md#rust_log).

## Connecting

### `irl watch` waits and never connects

- **The publisher is not running, or has a new key.** A ticket names the
  publisher's endpoint id, and without `IROH_SECRET` every run of
  `irl publish` makes a new one. Use the ticket from the current run, or set
  `IROH_SECRET` on the publisher.
- **The machines cannot find each other.** Without internet access, peers on
  one network find each other through mDNS, which some networks and
  container setups block. With internet access, iroh looks the endpoint id up
  through DNS and pkarr.
- **A firewall blocks UDP.** iroh needs outgoing UDP. When a direct path
  fails, it forwards through an iroh relay, which needs outgoing HTTPS.

### The ticket changes on every run

Set `IROH_SECRET` to a fixed key, see
[Keep the same ticket](getting-started/cli-quickstart.md#keep-the-same-ticket).
In Rust, bind with `EndpointConfig::from_env()`, not
`EndpointConfig::default()`.

### A Rust publisher logs that no peer can connect

The node is neither mounted on a router nor attached to a relay, so nobody can
dial it. Mount it with `node.mount(Router::builder(endpoint)).spawn()`, and
keep the router alive. See [Nodes and endpoints](concepts/nodes.md#mounting-the-node).

### A peer is rejected, or a dial fails with `Rejected`

The policy of this node or the other one refused the peer. A filter, such as
a contact list, covers both directions, so it also stops this node from
dialing a stranger. `NodeConfig::default()` lets peers publish nothing, so a
peer that publishes into it, such as a caller, needs `LiveLayout::config()`.
See [Control access](how-to/control-access.md).

## Picture and sound

### The window stays black for a moment

A viewer needs a keyframe before it can show a picture, and the publisher
sends one every two seconds by default. Lower `--keyframe-interval` on the
publisher to shorten the wait, at some cost in bitrate. A rendition switch
waits for a keyframe as well.

### There is no sound in my program

`PlayerConfig::default()` does not subscribe to audio. Use
`PlayerConfig::default().with_audio(AudioMode::Play(engine))`, which needs the `playback` feature. See
[Watch a broadcast](how-to/watch.md#play-the-audio).

### The camera is busy, or opens and delivers nothing

- On Linux without PipeWire, a camera opens through V4L2, which holds it
  exclusively. Close the other application, or use a build with the
  `pipewire` feature, where several applications share PipeWire's camera.
- On a Raspberry Pi, `/dev/video0` is the raw sensor and never delivers a
  picture. Use `--video rpicam`, see [Raspberry Pi](how-to/raspberry-pi.md).
- `irl devices` lists what this machine can open.

### Screen capture shows a dialog every time, or fails on Wayland

On Wayland, screen capture goes through the desktop portal's picker, which
needs the `pipewire` feature. `irl` keeps the portal's grant, so on a desktop
that keeps grants, the next run shares the same screen without asking.

### H.265 or AV1 fails to encode or decode

There is no software decoder for H.265 or AV1, and no software encoder for
H.265. They need a hardware backend, such as VA-API, NVIDIA or VideoToolbox.
Use H.264, the default, where no hardware backend exists. See
[Platform support](reference/platforms.md).

### A named encoder or decoder fails instead of falling back

A backend passed by name, such as `--decoder vaapi`, is the only one tried.
Use `auto` to fall back to software.

## Latency

### The picture lags

- Check the `Net` section of the stats overlay. A long round trip or loss
  points at the link, and the player then holds more and steps down the
  ladder.
- `--latency realtime` holds less than the default. `--latency smooth` holds
  more, which helps on Wi-Fi.
- On a Raspberry Pi 4, the hardware decoder adds about 700 ms. Use
  `--decoder openh264`.
- Through an iroh relay, every packet takes an extra round trip. `RUST_LOG`
  with `iroh_live_node::link=trace` shows whether the path is relayed.

[Tune quality and latency](how-to/tune.md) covers the settings.

## Browsers

### The page does not load, or reports an SSL error

Open the relay over `http://`, not `https://`. The page pins the relay's
self-signed certificate, which only works from an `http:` page. See
[Serve browsers](how-to/browsers.md).

### The browser connects, then fails with a 404 on `ws://`

The browser has no usable WebTransport and fell back to WebSocket, which the
relay does not serve. Use Chromium, or Firefox 153 or newer. Safari does not
work.

### The relay refuses a ticket

The relay pulls a ticket only for a session whose JWT grants reading its
path, or for any session with `--open-pulls`. A relay started with neither
pulls nothing. See [Relay reference](reference/relay.md#pulls).

### `irl watch` cannot play a broadcast that is on the relay

`irl watch` takes a ticket, which names a publisher, not a relay. A broadcast
that only exists on the relay, such as one a browser published, needs a node
that attaches to the relay and subscribes with `Reach::Relays`. See
[Publish through a relay](how-to/publish-through-relay.md#reading-from-a-relay).

## Building

### The build fails on Linux with a missing library

Install the system libraries in [Install](getting-started/install.md#build-irl-from-source).
A library build without `capture`, `pipewire` and `render` needs none of them.

### Types from moq do not match

Your crate builds a moq crate from another rev than iroh-live does. Use the
same git rev, see [Install](getting-started/install.md#use-the-library).

### Why is iroh-live not on crates.io?

It depends on moq's unreleased `main` branch, and a crate with git
dependencies cannot be published. See
[Install](getting-started/install.md#use-the-library).

## Known gaps

- Adaptation compares the link's estimate against each rung's advertised
  bitrate, which is the encoder's ceiling, not what it sends.
  `Adaptation::fit_ratio` covers the gap, and it was measured on few
  encoders and networks.
- A subscriber measures loss over its own packets, which are mostly
  acknowledgements, so its loss figure is rough.
- Windows builds have never been run.
- No node forwards another node's broadcasts. Only a relay does.
- A relay cannot revoke an rcan chain or a JWT before it expires.
