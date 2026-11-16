# Make a one-to-one call

A call is two broadcasts: each side publishes its own to the other peer only,
and plays the other's. [Calls and rooms](../concepts/calls-and-rooms.md)
explains the convention.

## With `irl`

```sh
irl call             # shows this side's ticket and QR code, and waits
irl call <TICKET>    # on the other machine
```

Scanning the other side's QR code from the waiting screen also places the
call. Every capture flag of `irl publish` applies to what this side sends.

## From Rust

`Calls::new` publishes this side at `live/<id>/call`, visible to nobody.
`Calls::call(peer)` rings the peer and waits until it answers, with no time
limit, so bound it with a timeout. Both programs run the same code with the
other's endpoint id, which each prints on start. Two peers that ring each
other at once get one `Call` on each side.

```rust,no_run
use std::time::Duration;

use iroh::{EndpointId, protocol::Router};
use iroh_live::{
    EndpointConfig, Node,
    calls::Calls,
    layout::LiveLayout,
    media::{
        AudioSource, LocalBroadcast, PlayerConfig, SubscriptionExt, VideoEncoding, VideoRendition,
        VideoSource, audio::Layout, audio_encoding, video,
    },
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let peer: EndpointId = std::env::args()
        .nth(1)
        .ok_or("usage: call <peer id>")?
        .parse()?;
    let layout = LiveLayout::default();
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(endpoint.clone(), layout.config());
    // The peer dials in to read our side.
    let router = node.mount(Router::builder(endpoint)).spawn();
    println!("our id: {}", node.id());

    let (calls, broadcast): (Calls, LocalBroadcast) = Calls::new(&node, &layout)?;
    let rate = video::FrameRate::new(30, 1).expect("a valid rate");
    broadcast.set_video(
        VideoSource::test_pattern(video::Size::new(640, 360), rate)?,
        VideoEncoding::single(VideoRendition::p360()),
    )?;
    broadcast.set_audio(
        AudioSource::test_pattern(Layout::Mono)?,
        audio_encoding::voice(),
    )?;

    let call = tokio::time::timeout(Duration::from_secs(60), calls.call(peer)).await??;
    let player = call.subscription().remote_broadcast().play(PlayerConfig::default())?;
    let mut frames = player.video_frames();
    while let Some(frame) = frames.next().await {
        println!("peer frame {}", frame.size());
    }

    // Dropping the call hangs up too.
    call.hang_up();
    router.shutdown().await?;
    Ok(())
}
```

The program uses test sources. Swap in a camera and a microphone as in
[Publish](publish.md).

## Answering

A node learns that a peer rings when the peer's `call` path appears in its
route table. `Calls::ringing()` watches the peers that ring now, and
`Calls::answer(peer)` takes one of their calls:

```rust,no_run
use iroh::Watcher;
use iroh_live::calls::{Call, Calls};

async fn answer_first(calls: &Calls) -> Result<Call, Box<dyn std::error::Error>> {
    let mut ringing = calls.ringing();
    let peer = loop {
        if let Some(peer) = ringing.get().first().copied() {
            break peer;
        }
        ringing.updated().await?;
    };
    Ok(calls.answer(peer).await?)
}
```

`Call::ended()` resolves with a `HangUpReason` that says why the call ended.
Dropping the `Call`, or `Call::hang_up`, takes this side's path away from the
peer, which ends the peer's read. The broadcast keeps running for the next
call.

Each node's policy must let the other publish its `call` path, as
`LiveLayout::config()` does. A filter on it, such as a contact list, keeps
strangers from ringing: see [Control access](control-access.md#contacts-only).

## Where to go next

- [Run a room](rooms.md) for more than two people.
- [Android](android.md) uses the same convention, so a phone can call `irl call`.
