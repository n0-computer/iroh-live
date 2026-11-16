# Quickstart: CLI

This page streams a camera from one machine to another with `irl`, then opens
the stream in a browser. It needs no code. [Install](install.md) `irl` first.

## Publish and watch

Publish the camera and microphone:

```sh
irl publish
```

It prints a ticket and the ticket as a QR code. On another machine, or in a
second terminal, play it:

```sh
irl watch <TICKET>
```

Without a camera, publish a test pattern and a beeping tone instead:

```sh
irl publish --test-source
```

The two machines find each other through iroh. On one network they connect
directly, and mDNS finds them without internet access. Across networks, iroh
punches through NAT where it can and forwards through an iroh relay where it
cannot.

## Keep the same ticket

Each run prints a new ticket, because `irl` makes a new key on every start.
Set `IROH_SECRET` to keep the key, and so the ticket:

```sh
export IROH_SECRET=$(openssl rand -hex 32)   # once, then keep the value
irl publish
```

The key is a secret, so keep it like a password. See
[Environment variables](../reference/environment.md).

## Share with a phone or a camera

`irl watch --scan` opens the camera and reads the ticket from a QR code held
up to it. The [Android demo](../how-to/android.md) scans the code that
`irl publish` prints.

## Open it in a browser

A browser cannot dial an iroh endpoint, so it goes through the iroh-live
relay. Run the relay from a checkout and keep `irl publish` running.
`--open-pulls` lets the relay fetch any ticket, which is fine on your own
machine:

```sh
cargo run -p iroh-live-relay -- --open-pulls
```

Open `http://localhost:4443/?name=<TICKET>` in Chromium, or in Firefox 153 or
newer. The relay reads the broadcast from the publisher over iroh and serves
it to the page. Use `http`, not `https`. [Serve browsers](../how-to/browsers.md)
says why, and how to reach viewers on other machines.

## Call and room

Two machines can call each other:

```sh
irl call            # prints this side's ticket and waits
irl call <TICKET>   # on the other machine
```

A room holds several participants:

```sh
irl room --display-name alice
irl room --display-name bob <TICKET>
```

## Next steps

- Every command and flag is in the [CLI reference](../reference/cli.md).
- To record a stream, see [Record a broadcast](../how-to/record.md).
- To trade quality against latency, see [Tune quality and latency](../how-to/tune.md).
- To write your own program, follow the [library quickstart](library-quickstart.md).
