# moq-ext-access

Access control for moq nodes, independent of the transport. No iroh
dependency.

Use it to say who may do what on a node, or to share those rules with code that
does not link a transport. It carries the shapes a token means on a moq relay,
without moq-auth's JWT and crypto stack.

- `Grant`: what a session may subscribe to and publish, as moq patterns
  (`*` one segment, `**` a subtree).
- `SessionRequest`: what a peer asked for when it opened a session.
- `Audience`: who may see a publication, generic over the peer id (`PeerId`),
  such as an iroh endpoint id.
- `visible_paths`: the paths a link may see, for the node to hand to moq as the
  subscribe half of the link's grant.

```rust
use moq_ext_access::Grant;

// A peer that may read anything and publish nothing.
let grant = Grant::subscribe_only();
```

[`iroh-live-node`](../iroh-live-node) enforces these on every live session.

## Features

None.
