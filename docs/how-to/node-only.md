# Use the node without media

`LiveLayout` and the media crates are conveniences over `Node`. A program
with its own path layout or access rules, or with no media at all, uses
`Node` from `iroh_live::node` (the `iroh-live-node` crate). It gets the same
sessions, route table and relay links.

## The program

The default `NodeConfig` lets peers read anything and publish nothing, so
this reader grants the publisher `demo/**`. The publishing side is
[`iroh-live/examples/moq_only.rs`](../../iroh-live/examples/moq_only.rs),
which serves `demo/hello`:

```sh
cargo run -p iroh-live --example moq_only     # prints its endpoint id
```

```rust,no_run
use iroh::EndpointId;
use iroh_live::node::{Grant, Node, NodeConfig, MoqPreset, Reach, moq_net::Pattern, policy};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let publisher: EndpointId = std::env::args()
        .nth(1)
        .ok_or("usage: reader <id>")?
        .parse()?;
    let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
    let demo: Pattern = "demo/**".parse()?;
    let moq = Node::new(
        endpoint.clone(),
        NodeConfig::default().with_policy(policy::Open(Grant::publish_under(demo))),
    );
    // Mount your own protocols on the same router.
    let router = moq
        .mount(iroh::protocol::Router::builder(endpoint.clone()))
        .spawn();

    let subscription = moq
        .subscribe("demo/hello", Reach::Direct(publisher))
        .await?;
    println!("subscribed to {}", subscription.path());
    if let Some(link) = subscription.serving_link() {
        println!(
            "served by a {:?} link, rtt {:?}",
            link.kind, link.sample.rtt
        );
    }

    moq.shutdown().await;
    router.shutdown().await?;
    endpoint.close().await;
    Ok(())
}
```

`serving_link()` returns the link that serves the subscription now, with a
`NetworkSample` of its round trip and loss.

## The rest of the node

- `Node::sessions()` watches the open sessions, and `Session::connection()`
  is the iroh connection, whose `paths()` lists the network paths to the
  peer.
- `Session::moq()` is the moq-net session itself.
- `Node::accepted()` streams the sessions the node accepts.
- A `policy::Queue` lets the application admit each peer itself, see
  [Control access](control-access.md#ask-the-user).

## Where to go next

- [Send data and state](data.md) publishes tracks without media.
- [Transport](../architecture/transport.md) explains the node in depth.
