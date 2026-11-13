# Control access

Each section below is one recipe for a node's policy. [Access control](../concepts/access.md) explains the model. Check first
whether the defaults do what you need: a node built with `LiveLayout::config()`
lets every peer read, and publish only under its own id.

The types are in `iroh_live::node::policy`. The
[access reference](../reference/access.md) lists every policy, event and
limit.

## An open LAN node

On a network you trust, let every peer publish and read anything:

```rust,no_run
use iroh_live::{
    NodeConfig,
    node::{Grant, policy},
};

fn open_lan() -> NodeConfig {
    NodeConfig::default().with_policy(policy::Open(Grant::everything()))
}
```

## Live broadcasts, rooms and your own paths

`LiveLayout::config()` lets each peer publish under `live/<its id>/`. A node
that also runs rooms takes the union of both layouts,
`layout::config((LiveLayout::default(), RoomsLayout))`. A layout of your own
implements the `Layout` trait, and
[Publish at your own path](custom-paths.md) has a complete program with one.

## Contacts only

A filter narrows a policy. This node admits only its contacts, each with the
live layout's grant. The node does not know what the filter reads, so tell it
when the list changes with `Node::reauthorize(peer)` or
`Node::reauthorize_all()`:

```rust,no_run
use std::collections::BTreeSet;

use iroh::{Endpoint, EndpointId};
use iroh_live::{Node, NodeConfig, layout::LiveLayout, node::policy::Policy};
use n0_watcher::Watchable;

fn contacts_only(endpoint: Endpoint) -> (Node, Watchable<BTreeSet<EndpointId>>) {
    let contacts = Watchable::new(BTreeSet::new());
    let known = contacts.clone();
    let policy = LiveLayout::default()
        .policy()
        .filter(move |request| known.get().contains(request.peer()));
    let node = Node::new(endpoint, NodeConfig::default().with_policy(policy));
    (node, contacts)
}

fn remove_contact(node: &Node, contacts: &Watchable<BTreeSet<EndpointId>>, peer: EndpointId) {
    let mut without = contacts.get();
    without.remove(&peer);
    contacts.set(without).ok();
    // Decides again, rejects the peer, and closes every session with it.
    node.reauthorize(peer);
}
```

The filter covers both directions: a dial to a stranger fails with
`Error::Rejected`, and a stranger's call never rings.
[`iroh-live/examples/contacts_only.rs`](../../iroh-live/examples/contacts_only.rs)
runs this with three nodes in one process.

## A policy with I/O

A policy that needs I/O, such as a database lookup, implements the trait with
`async fn decide`. It can give the authorization an expiry, so the node asks
again when it passes:

```rust,no_run
use std::time::{Duration, SystemTime};

use iroh::EndpointId;
use iroh_live::node::{
    Grant,
    policy::{Authorization, Decision, PeerRequest, Policy, Rejection},
};

#[derive(Debug)]
struct Members;

async fn is_member(_peer: &EndpointId) -> bool {
    // Look the peer up in your database.
    true
}

impl Policy<EndpointId> for Members {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        if !is_member(request.peer()).await {
            return Err(Rejection::new("not a member"));
        }
        Ok(Authorization {
            grant: Grant::subscribe_only(),
            expires: Some(SystemTime::now() + Duration::from_secs(3600)),
        })
    }
}
```

When the member list changes, call `Node::reauthorize_all()`.

## Ask the user

A `Queue` hands each decision to the application, as a person who answers a
call does. The application answers each `Pending` with `admit` or `reject`,
and dropping one rejects it. A person needs longer than the default
`decide_timeout` of 10 seconds:

```rust,no_run
use std::time::Duration;

use iroh::EndpointId;
use iroh_live::{
    NodeConfig,
    node::{
        Grant,
        policy::{Queue, Rejection},
    },
};

async fn ask_the_user(_peer: &EndpointId) -> bool {
    // Show a prompt.
    true
}

fn asking() -> (NodeConfig, Queue) {
    let queue = Queue::new(16);
    let config = NodeConfig::default()
        .with_policy(queue.clone())
        .with_decide_timeout(Duration::from_secs(60));
    (config, queue)
}

async fn answer(queue: Queue) {
    while let Some(pending) = queue.next().await {
        if ask_the_user(pending.request().peer()).await {
            pending.admit(Grant::subscribe_only());
        } else {
            pending.reject(Rejection::new("declined"));
        }
    }
}
```

A full queue rejects at once with `TooManyRequests`. Some nodes admit every peer with a small grant, and ask only for more. Such
a policy waits for the queue with a timeout of its own, and falls back to the
small grant:

```rust,no_run
use std::time::Duration;

use iroh_live::node::{
    Grant,
    policy::{Decision, PeerRequest, Policy, Queue},
};

#[derive(Debug)]
struct AskForMore(Queue);

impl Policy<iroh::EndpointId> for AskForMore {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        match tokio::time::timeout(Duration::from_secs(30), self.0.decide(request)).await {
            Ok(Ok(authorization)) => Ok(authorization),
            _ => Ok(Grant::subscribe_only().into()),
        }
    }
}
```

## Require a token

To let a token decide, with JWTs or rcan chains, see [Use tokens](tokens.md).

## Where to go next

- [Access reference](../reference/access.md) lists the defaults, events,
  limits and errors.
- [Use tokens](tokens.md) for JWTs, rcan chains and relay links.
- [Authorization and audiences, explained](../architecture/auth-explained.md)
  walks through one session.
