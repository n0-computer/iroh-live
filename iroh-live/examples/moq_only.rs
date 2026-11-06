//! The transport with a path scheme of its own: a `Node` mounted on a router.
//!
//! The node takes any path and any grant. An application with its own layout
//! uses `Node` directly and gets the same sessions, route table and relays,
//! without `iroh_live::layout::LiveLayout`. This one serves a broadcast at a
//! path of its own, `demo/hello`, to everyone.
//!
//! Run it with `cargo run -p iroh-live --example moq_only`.

use iroh::protocol::Router;
use iroh_live::{Audience, EndpointOptions, Node, NodeConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    // `EndpointOptions` is optional too: any endpoint built with `MoqPreset`
    // or a preset of your own will do.
    let endpoint = EndpointOptions::from_env()?.bind().await?;
    // The default config lets peers subscribe to anything and publish
    // nothing. Set `grant` to let them publish somewhere.
    let moq = Node::new(endpoint.clone(), NodeConfig::default());
    let router = moq.mount(Router::builder(endpoint.clone())).spawn();

    // An empty broadcast in the route table. A real one writes tracks into
    // `publication.broadcast()`.
    let _publication = moq.publish("demo/hello", Audience::Everyone)?;
    println!("serving demo/hello as {}", endpoint.id());

    tokio::signal::ctrl_c().await?;
    moq.shutdown().await;
    // Also closes the endpoint.
    router.shutdown().await?;
    Ok(())
}
