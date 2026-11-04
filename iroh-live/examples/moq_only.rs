//! The transport without `Live`: a `Node` mounted on a router.
//!
//! `Live` is a convenience over `Node` and an endpoint. An application with its
//! own path layout, grant or router mounts `Node` itself, and gets the same
//! sessions, route table and relays. This one serves a broadcast at a path of
//! its own, `demo/hello`, to everyone.
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

    // An empty broadcast in the route table; a real one writes tracks into
    // `publication.broadcast()`.
    let _publication = moq.publish("demo/hello", Audience::Everyone)?;
    println!("serving demo/hello as {}", endpoint.id());

    tokio::signal::ctrl_c().await?;
    moq.shutdown().await;
    router.shutdown().await?;
    endpoint.close().await;
    Ok(())
}
