# Tickets

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | iroh-live |
| Platforms | all |

## LiveTicket

`LiveTicket` is the primary way to share connection information for a broadcast. It bundles the publisher's iroh endpoint address, the broadcast name, and optional relay URLs into a single string that a viewer can use to connect.

### Format

A ticket serializes to a URI:

```
iroh-live:<BASE64URL(postcard(EndpointAddr))>/<broadcast-name>
```

The endpoint address is postcard-encoded, then base64url-encoded (no padding). The broadcast name follows a `/` separator. A legacy format (`name@BASE32(addr)`) is still accepted for parsing but no longer produced.

### Creating and parsing

```rust
use iroh_live::ticket::LiveTicket;

// Create a ticket
let ticket = LiveTicket::new(endpoint_addr, "my-stream");

// Print it (Display trait produces the URI string)
println!("{ticket}");

// Parse from a string (FromStr trait)
let parsed: LiveTicket = ticket_string.parse()?;

// Add relay URLs for indirect connectivity
let ticket = LiveTicket::new(endpoint_addr, "my-stream")
    .with_relay_urls(["https://relay.example.com".to_string()]);
```

### Sharing

There are several ways to share a ticket with viewers:

- **Copy the string**: the `publish` example prints the ticket to the terminal. Copy and paste it to another machine.
- **QR code**: the Pi Zero demo renders the ticket as a QR code on an e-paper display. Any QR scanner can read it. The ticket string is short enough to fit comfortably in a QR code (well under 2000 characters).
- **Browser URL**: pass the ticket as a query parameter to the relay web viewer: `https://relay.example.com/?name=<TICKET>`. See [browser-relay.md](browser-relay.md).

## Call tickets

For 1:1 calls, use a `LiveTicket` with broadcast name `"call"`. The `Call`
type uses this convention internally.

```rust
use iroh_live::ticket::LiveTicket;

let ticket = LiveTicket::new(my_endpoint_addr, "call");
println!("Call me: {ticket}");
```

## RoomTicket

`RoomTicket` identifies a multi-party room. It contains a gossip topic ID and optional bootstrap peer IDs. Unlike `LiveTicket`, it does not contain a broadcast name, because each room participant publishes their own broadcast and discovers others via gossip.

```rust
use iroh_live::rooms::RoomTicket;

// Generate a new room
let ticket = RoomTicket::generate();

// Join an existing room
let parsed: RoomTicket = ticket_string.parse()?;
```

See [rooms.md](rooms.md) for details on the room API.
