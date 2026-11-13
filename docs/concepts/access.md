# Access control

Access control in iroh-live answers two questions about each peer, from its
endpoint id. *Admission*: may the peer hold a session with this node?
*Authorization*: what may it read from this node, and what may it publish
into it?

## You may need nothing

The defaults are safe for the common cases:

| Setup | Every peer may |
|---|---|
| `NodeConfig::default()` | Read everything, and publish nothing |
| `LiveLayout::config()` | Read everything, and publish under `live/<its id>/` |
| A ticket on any node | `Node::subscribe_ticket` also lets the ticket's publisher publish that one path, while the subscription lives |

"Read everything" means every publication with the default audience. A
broadcast that should reach only some peers uses an audience, below.

## Policy and grant

A *policy* decides each peer when a session opens, and again when the
application asks or the decision expires. It returns a *grant*, two sets of
path patterns: what the peer may read, and what it may publish. A rejection
closes the session. Policies compose: a layout's policy, a filter such as a
contact list, a queue that asks a person, or a token verifier.

```rust,ignore
// Admits only contacts, each with the layout's grant.
let policy = LiveLayout::default()
    .policy()
    .filter(move |request| contacts.get().contains(request.peer()));
```

## Audience

The grant is an upper limit for every path. A publication's *audience*
narrows it for one broadcast:

- `Audience::Granted`, the default, reaches every session whose grant covers
  the path, attached relays included.
- `Audience::Peers(set)` reaches only the peers in the set, and never a
  relay. Calls and rooms use it.

When a set, a grant or a publication changes, the node updates every session
at once. A peer that loses a path stops reading it.

## The trust model

- The transport proves each peer's endpoint id. Nothing else a peer sends is
  authentic unless a token signs it.
- A peer that may publish only under its own id cannot pose as another
  publisher. The layouts keep peers to their own paths.
- A relay link sees every `Granted` publication, and may forward it to anyone
  the relay admits. Attach only relays that keep each publisher to its own
  paths.
- A broadcast ticket is not a secret. A room ticket is: anyone who has it
  can join and read the room.
- A JWT is a bearer token: anyone who holds it may use it. An rcan chain
  works only for the endpoint its last link names.

## Where to go next

- [Control access](../how-to/control-access.md) has recipes: an open LAN,
  contacts only, asking the user, and tokens.
- [Access reference](../reference/access.md) lists the policies, events,
  limits and errors.
- [Authorization and audiences, explained](../architecture/auth-explained.md)
  walks through one session's life.
