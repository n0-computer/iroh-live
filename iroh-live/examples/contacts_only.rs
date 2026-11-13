//! A node on the live layout that admits only its contacts.
//!
//! Alice's policy is the layout's, narrowed by a filter over her contact
//! list. Bob is a contact and connects. Carol is not, and alice refuses her.
//! When alice removes bob from her contacts, she asks the node to decide about
//! him again, and the node closes his session.
//!
//! The filter covers both directions: alice's own dials to a stranger fail
//! too, with `Error::Rejected`.
//!
//! Run it with `cargo run -p iroh-live --example contacts_only`.

use std::collections::BTreeSet;

use iroh::{EndpointId, protocol::Router};
use iroh_live::{EndpointConfig, Node, layout::LiveLayout, node::policy::Policy};
use n0_watcher::Watchable;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let layout = LiveLayout::default();

    let bob = Node::new(EndpointConfig::default().bind().await?, layout.config());
    let carol = Node::new(EndpointConfig::default().bind().await?, layout.config());

    // Alice admits her contacts with the layout's grant, and nobody else.
    let contacts = Watchable::new(BTreeSet::<EndpointId>::from([bob.id()]));
    let known = contacts.clone();
    let policy = layout
        .policy()
        .filter(move |request| known.get().contains(request.peer()));
    let alice_endpoint = EndpointConfig::default().bind().await?;
    let alice = Node::new(
        alice_endpoint.clone(),
        iroh_live::NodeConfig::default().with_policy(policy),
    );
    let router = alice.mount(Router::builder(alice_endpoint.clone())).spawn();

    let session = bob.connect(alice_endpoint.addr()).await?;
    println!("bob connected");

    // Carol's dial may fail at once, or succeed and close when alice rejects
    // the session after its setup.
    match carol.connect(alice_endpoint.addr()).await {
        Err(err) => println!("carol was refused: {err}"),
        Ok(session) => println!("carol was refused: {}", session.closed().await),
    }

    // The filter reads the contact list on each decision. The node decides
    // again only when asked, so tell it that bob's standing changed.
    contacts.set(BTreeSet::new()).ok();
    alice.reauthorize(bob.id());
    println!("bob's session closed: {}", session.closed().await);

    bob.shutdown().await;
    carol.shutdown().await;
    bob.endpoint().close().await;
    carol.endpoint().close().await;
    router.shutdown().await?;
    Ok(())
}
