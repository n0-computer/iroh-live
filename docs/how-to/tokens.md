# Use tokens

A token lets a peer prove rights that its endpoint id alone does not give. A
node verifies it with a `Verify` policy, and a relay with `--auth-key`.
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

The device presents the encoded chain with `ConnectOptions::with_token` when it
dials. [`iroh-live-node/examples/delegated_publisher.rs`](../../iroh-live-node/examples/delegated_publisher.rs)
issues, delegates and presents a chain, with both sides in one process:

```sh
cargo run -p iroh-live-node --features rcan --example delegated_publisher
```

## Tokens for a relay link

A relay link presents its tokens on every dial. `RelayLink::tokens().set`
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

## On the iroh-live relay

`--auth-key <PATH>` makes the relay verify JWTs signed with a key in the file.
A session without a token keeps the default rules, so the relay stays
readable by anyone. See [Run the iroh-live relay](run-relay.md) and the
[relay reference](../reference/relay.md#access-flags).

## Where to go next

- [Access reference](../reference/access.md#tokens) lists how tokens travel
  and the verifiers' limits.
- [Control access](control-access.md) has the policies without tokens.
