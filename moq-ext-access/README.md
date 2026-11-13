# moq-ext-access

Access control for moq nodes, independent of the transport. No iroh
dependency.

Use it to say who may do what on a node, or to share those rules with code that
does not link a transport. It carries the shapes a token means on a moq relay.
The default build has no JWT or crypto stack. The `jwt` and `rcan` features
add token verifiers.

- `Grant`: what a session may subscribe to and publish, as moq patterns
  (`*` one segment, `**` a subtree).
- `SessionRequest`: what a peer asked for when it opened a session.
- `Audience`: who may see a publication, generic over the peer id (`PeerId`),
  such as an iroh endpoint id.
- `visible_paths`: the paths a link may see, for the node to hand to moq as the
  subscribe half of the link's grant.
- `policy`: who may hold a session, and with which grant. `Open`, `Closed`,
  `from_fn` and `filter` compose policies. `Queue` hands
  each decision to the application, and `Verify` decides with a token
  verifier.
- `Layout`: a path convention, which says where each peer publishes and who
  owns a path.

```rust
use moq_ext_access::{Grant, policy::{self, Policy}};

// Every peer may read and publish nothing, except mallory, who may not connect.
let policy = policy::Open(Grant::subscribe_only())
    .filter(|request: &policy::PeerRequest<String>| request.peer() != "mallory");
```

[`iroh-live-node`](../iroh-live-node) asks a node's policy on every session and
enforces the grants on every live session. [Access
control](../docs/how-to/control-access.md) covers the model.

## Features

`jwt` and `rcan` add the token verifiers. [Cargo features](../docs/reference/features.md) lists them.
