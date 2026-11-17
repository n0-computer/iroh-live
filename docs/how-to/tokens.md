# Use tokens

A token lets a peer prove rights that its endpoint id alone does not give. A
node verifies it with a `Verify` policy, and the iroh-live relay with
`--auth-key` or `--rcan-principal`.
[Access control](../concepts/access.md) explains where tokens fit.

## Verify tokens on a node

`Verify` lets a token decide. With a token, the verifier decides, and a token
it rejects is a rejection. Without a token, the fallback decides, which is
`Closed` by default. The node dials with no token, so a node that dials needs
a fallback that admits the peers it dials.

A JWT from moq-relay's issuer, with the `jwt` feature:

```rust,ignore
use iroh_live::node::{Grant, policy::{JwtVerifier, Open, Verify}};

let keys: moq_auth::KeySet = /* the issuer's public keys */;
let policy = Verify::new(JwtVerifier::new(keys))
    .with_fallback(Open(Grant::subscribe_only()));
```

The claims' patterns become the grant, and the `exp` claim its expiry. A JWT
is a bearer token: anyone who holds it may use it.

## Delegate with rcan

An rcan chain is bound to a key. A *principal*, such as an account key,
issues a `MoqCapability` to a key, and each holder may delegate a narrower one
on, up to eight links. The last link must name the endpoint id of the peer
that presents it, so a stolen chain is useless without that peer's key. With
the `rcan` feature, the node that shares `spaces/alice` verifies chains from
alice's account key:

```rust,ignore
use iroh_live::node::{Grant, policy::{Open, RcanVerifier, Verify}};

let verifier = RcanVerifier::new(alice_account.public().as_verifying_key(), "spaces/alice");
let policy = Verify::new(verifier).with_fallback(Open(Grant::subscribe_only()));
```

`Chain` builds and reads chains. `Chain::issue` makes the first link.
`Chain::delegate` adds one from the key the last link names, and fails with a
`ChainError` for a capability wider than the one it delegates. `Chain::expires`
says when to sign the next device link. `Expires` is rcan's link expiry,
re-exported, so you do not need the `rcan` crate:

```rust,ignore
use std::time::Duration;

use iroh_live::node::policy::{Chain, Expires, MoqCapability};

let member = MoqCapability::new("").with_subscribe(iroh_live::moq_net::Pattern::all());
let to_alice = Chain::issue(
    team.as_signing_key(),
    alice.public().as_verifying_key(),
    member,
    Expires::valid_for(Duration::from_secs(30 * 24 * 3600)),
);
let to_camera = to_alice.delegate(
    alice.as_signing_key(),
    camera_id.as_verifying_key(),
    None, // the same capability
    Expires::valid_for(Duration::from_secs(8 * 3600)),
)?;
let token: String = to_camera.to_token();
// A token parses back into a chain, or fails with a `ChainError`.
let parsed: Chain = token.parse()?;
```

The device presents the encoded chain with `ConnectOptions::with_token` when it
dials. [`iroh-live-node/examples/delegated_publisher.rs`](../../iroh-live-node/examples/delegated_publisher.rs)
issues, delegates and presents a chain, with both sides in one process:

```sh
cargo run -p iroh-live-node --features rcan --example delegated_publisher
```

## Tokens for a relay link

A relay link presents its tokens on every dial, the first as the connection
credential. A JWT and an rcan chain travel the same way. `RelayLink::tokens().set`
replaces them on a live link, before the old ones expire:

```rust,no_run
use iroh::EndpointId;
use iroh_live::{Node, RelayConfig};

fn attach_with_token(
    node: &Node,
    relay: EndpointId,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let link = node.attach_relay(RelayConfig::iroh(relay).with_token(token.parse()?))?;
    // Later, with a fresh token:
    link.tokens().set(vec![token.parse()?]);
    Ok(())
}
```

A chain needs an `iroh://` relay link, and a node bound with the key its last
link names, through `EndpointConfig::with_secret_key`. An `https://` link
reaches the relay as a browser does, and browsers present JWTs only.

When the relay refuses the token, an `iroh://` link reports
`RelayStatus::Refused(Refusal::Unauthorized)`, or `Refusal::Expired` for an
expired one, and keeps redialing with backoff. Set a fresh token to get back
in.

## On the iroh-live relay

A relay with `--auth-key` or `--rcan-principal` is private: a session with a
valid token gets what the token grants, and one without a token gets only
the `--auth-public*` patterns, or nothing. See
[Run the iroh-live relay](run-relay.md#choose-an-access-mode) and the
[relay reference](../reference/relay.md#access).

- **JWTs** work for browsers and iroh clients. The relay verifies them with
  `--auth-key`. A JWT is a bearer token, so prefer one key per relay.
- **rcan chains** work for iroh clients only. The relay accepts a chain whose
  first link a `--rcan-principal` key issued, and whose last link names the
  client's endpoint id. With `KEY@ROOT`, the chain's capability must lie
  within that root.
- A token grants what it states. `--auth-own-paths` also lets token holders
  publish at their own paths, `live/<id>/**` and `rooms/*/<id>/**`, so one
  membership token with `--subscribe '**'` serves every member. It never
  applies to a principal limited to a root below the relay root.
- A token must expire, unless the relay runs `--allow-unexpiring-tokens`.

With `irl`, the principal's key and every chain come from the command line.
Keys live in `<config dir>/iroh-live/secret_keys/`, readable by this user
only:

```sh
# On the operator's machine: the principal. The relay gets the public key.
TEAM_KEY=$(irl key team)
iroh-live-relay --rcan-principal "$TEAM_KEY" --auth-own-paths

# On alice's machine and on her camera: their keys.
ALICE_KEY=$(irl key alice)
CAM_ID=$(irl key cam)

# The operator issues membership to alice for 30 days.
irl rcan sign --key team --to "$ALICE_KEY" --subscribe '**' --ttl 30d > alice.rcan

# Alice delegates it to her camera for 8 hours.
irl rcan sign --key alice --chain alice.rcan --to "$CAM_ID" --ttl 8h > cam.rcan

# The links, and what a relay with this principal grants the camera.
irl rcan show cam.rcan --principal "$TEAM_KEY"

# The camera publishes with its key and the chain.
irl publish --key cam --relay "$RELAY_ID" --relay-token-file cam.rcan
```

A JWT works the same way from `irl`: `--relay-token-file publisher.jwt`, or
the `IRL_RELAY_TOKEN` environment variable. Avoid `--relay-token <TOKEN>`,
since other users of the machine can read a command line.

In a browser, append the token to the viewer's address as a fragment, which
the browser never sends to a server:

```text
http://relay.example:4443/?name=<TICKET>#jwt=<TOKEN>
```

The viewer removes the token from the address bar, and sends it only to the
relay that served the page. `?jwt=` still works, but it reaches the server
and its logs, so do not use it in links you share.

## Where to go next

- [Access reference](../reference/access.md#tokens) lists how tokens travel
  and the verifiers' limits.
- [Control access](control-access.md) has the policies without tokens.
