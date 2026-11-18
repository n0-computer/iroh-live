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

`Calls::new` publishes this side at `live/<endpoint id>/call`, visible to
nobody. `Calls::call(peer)` rings the peer and waits until it answers, with no
time limit, so bound it with a timeout. Two peers that ring each other at once
get one `Call` on each side.

Run the program on one machine with no argument. It prints its ticket and
answers the first peer that rings. Run it on another machine with that
ticket, and it calls.

```rust,no_run
use std::time::Duration;

use iroh::protocol::Router;
use iroh_live::{
    BroadcastTicket, EndpointConfig, Node,
    calls::Calls,
    layout::LiveLayout,
    media::{
        AudioSource, LocalBroadcast, PlayerConfig, SubscriptionExt, VideoEncoding, VideoRendition,
        VideoSource, audio::Layout, audio_encoding, video,
    },
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let layout = LiveLayout::default();
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(endpoint.clone(), layout.config());
    // The peer dials in to read our side.
    let router = node.mount(Router::builder(endpoint)).spawn();

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

    let call = match std::env::args().nth(1) {
        Some(ticket) => {
            let peer = ticket.parse::<BroadcastTicket>()?.peer();
            tokio::time::timeout(Duration::from_secs(60), calls.call(peer)).await??
        }
        None => {
            println!("call me: {}", calls.ticket());
            let peer = calls.next_ring().await.ok_or("the call service ended")?;
            calls.answer(peer).await?
        }
    };
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
[Publish a camera, screen or file](publish.md), or follow the
[call tutorial](../getting-started/call-app.md), which adds a window, the
speaker and echo cancellation.

## Answering

A node learns that a peer rings when the peer's `call` path appears in its
route table. `Calls::next_ring()` waits for the first peer that rings, as in
the program above, and `Calls::ringing()` watches every peer that rings now,
for a list to pick from. `Calls::answer(peer)` takes one of their calls.
`Calls::ticket()` names this side, as text or a QR code for the caller.

`Call::ended()` resolves with a `HangUpReason` that says why the call ended.
Dropping the `Call`, or `Call::hang_up`, takes this side's path away from the
peer, which ends the peer's read. The broadcast keeps running for the next
call.

Each node's policy must let the other publish its `call` path, as
`LiveLayout::config()` does. A filter on it, such as a contact list, keeps
strangers from ringing: see [Control access](control-access.md#contacts-only).

## Where to go next

- [Run a room](rooms.md) for more than two people.
- [Use iroh-live on Android](android.md) uses the same convention, so a phone can call `irl call`.
