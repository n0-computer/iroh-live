# Introduction

iroh-live sends live audio and video between devices over
[iroh](https://github.com/n0-computer/iroh), written in Rust. Peers connect
directly, with no media server in the middle. The wire protocol is
[Media over QUIC](https://moq.dev/) (MoQ): every video rendition and every
audio track travels on its own QUIC streams, so a lost video packet never
delays audio.

A publisher encodes one source into several renditions, a *ladder*. Each
viewer's player picks the rendition that its link can carry, and switches
without a blank frame when the link changes. A playout clock keeps audio and
video in step. Capture, codecs and GPU rendering come from moq's
[moq-video and moq-audio](https://doc.moq.dev/lib/rs/).

## When to use it

iroh-live fits when devices should talk to each other without a server that
you run. Examples are a camera that a phone watches, a call between two apps,
a small room, or a fleet of Raspberry Pis. iroh finds a
direct path through most NATs, and falls back to an iroh relay that forwards
the encrypted packets.

A node serves each of its viewers itself, so its uplink limits how many can
watch. Browsers cannot dial an iroh endpoint at all. The iroh-live relay, a
server you run, covers both cases: it reads a broadcast once over iroh and
serves it to many viewers, browsers included over WebTransport.

## Status

iroh-live is an early tech preview, and its API changes often.

- It depends on the unreleased `main` branch of moq, pinned by git rev, so it
  is not on crates.io. [Install](getting-started/install.md#use-the-library)
  has the dependency line.
- Linux is the main target. macOS and Android are tested lightly, and Windows
  has never been run. See [Platform support](reference/platforms.md).
- The iroh-live relay lets anyone read every broadcast on it by default. See
  [Run the iroh-live relay](how-to/run-relay.md).

## The pieces

```text
  irl (CLI)          your application           iroh-live-relay
       \                    |                    /      (browsers)
        +------------ iroh-live (facade) -------+
           |                |                  |
    media: moq-ext-*    iroh-live-node     iroh-live-rooms
    (publisher, player,  (routes, access,   (gossip rooms)
     sources, adaptive)   relay links)
           |                |
    moq-video, moq-audio  moq-transport-iroh -- iroh
```

Most applications depend on `iroh-live` alone. The
[crates reference](reference/crates.md) describes each crate.

## Where to start

- To try it without code, follow the [CLI quickstart](getting-started/cli-quickstart.md).
- To build on it, follow the [library quickstart](getting-started/library-quickstart.md).
- To understand the model first, read [How it fits together](concepts/overview.md).
- If something does not work, see [Troubleshooting](troubleshooting.md).
