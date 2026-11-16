# Publish through a relay

A publisher that viewers cannot reach directly pushes its broadcasts to an
iroh-live relay over a relay link. Viewers then read them from the relay.
[Relays](../concepts/relays.md) explains when this helps.

## With `irl`

```sh
irl publish --relay <RELAY_ENDPOINT_ID>
```

The relay prints its endpoint id on start. `irl publish` stays attached and
redials if the session drops, and prints the path at which viewers find the
broadcast on the relay, `live/<publisher id>/<name>`. With `--no-serve` it
accepts no direct viewers and prints no ticket.

A relay that only browsers read needs no push: it can pull a broadcast by its
ticket, see [Serve browsers](browsers.md).

## From Rust

`Node::attach_relay` returns a `RelayLink`. Every publication with the
`Granted` audience, which `LiveLayout::publish` uses, is then visible to the
relay, and a `Peers` one never is. A publisher that does not need the relay's
other routes sets `with_consume(false)`.

```rust,no_run
use iroh::{EndpointId, protocol::Router};
use iroh_live::{
    EndpointConfig, Node, RelayConfig,
    layout::LiveLayout,
    media::{LocalBroadcast, VideoEncoding, VideoRendition, VideoSource, video},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let relay: EndpointId = std::env::args()
        .nth(1)
        .ok_or("usage: relay <relay id>")?
        .parse()?;
    let layout = LiveLayout::default();
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(endpoint.clone(), layout.config());
    // Direct viewers still dial in. A relay-only publisher can skip the router
    // and close the endpoint itself.
    let router = node.mount(Router::builder(endpoint)).spawn();

    let link = node.attach_relay(RelayConfig::iroh(relay).with_consume(false))?;

    let broadcast: LocalBroadcast = layout.publish(&node, "pattern")?;
    let rate = video::FrameRate::new(30, 1).expect("a valid rate");
    broadcast.set_video(
        VideoSource::test_pattern(video::Size::new(640, 360), rate)?,
        VideoEncoding::single(VideoRendition::p360()),
    )?;
    println!("{}", layout.ticket(node.id(), "pattern")?);

    tokio::signal::ctrl_c().await?;
    link.detach().await;
    router.shutdown().await?;
    Ok(())
}
```

`RelayLink::status()` watches whether the link is connected. A relay started
with `--auth-key` accepts a JWT, which the link presents through
`RelayConfig::with_token`. See [Use tokens](tokens.md#tokens-for-a-relay-link).

## Reading from a relay

A viewer with a relay attached with the default config reaches a
broadcast on it with `Reach::Relays`, or with `Reach::Both(publisher)` to
also try a direct session:

```rust,no_run
use iroh::EndpointId;
use iroh_live::{Node, Reach, RelayConfig, Subscription, node::RelayLink};

async fn read_via_relay(
    node: &Node,
    relay: EndpointId,
    path: &str,
) -> Result<(RelayLink, Subscription), Box<dyn std::error::Error>> {
    // Stays attached until `detach` or the node shuts down.
    let link = node.attach_relay(RelayConfig::iroh(relay))?;
    let subscription = node.subscribe(path, Reach::Relays).await?;
    Ok((link, subscription))
}
```

This is how a Rust node reads a broadcast that a browser published on the
relay, as `iroh-live/examples/subscribe_test.rs` does. `irl watch` cannot,
since its tickets name a publisher, not a relay.

## Where to go next

- [Run the iroh-live relay](run-relay.md)
- [Serve browsers](browsers.md)
