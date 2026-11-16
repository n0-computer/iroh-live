# Summary

[Introduction](introduction.md)

# Getting started

- [Install](getting-started/install.md)
- [Quickstart: CLI](getting-started/cli-quickstart.md)
- [Quickstart: library](getting-started/library-quickstart.md)

# Guides

- [Build an app](how-to/index.md)
  - [Publish a camera, screen or file](how-to/publish.md)
  - [Watch a broadcast and render it](how-to/watch.md)
  - [Make a one-to-one call](how-to/call.md)
  - [Run a room](how-to/rooms.md)
  - [Record a broadcast](how-to/record.md)
  - [Tune quality and latency](how-to/tune.md)
  - [Control access](how-to/control-access.md)
  - [Use tokens](how-to/tokens.md)
  - [Publish through a relay](how-to/publish-through-relay.md)
  - [Publish at your own path](how-to/custom-paths.md)
  - [Send data without media](how-to/data.md)
- [Run the iroh-live relay](how-to/run-relay.md)
  - [Serve browsers](how-to/browsers.md)
- [Platforms](how-to/platforms.md)
  - [Use iroh-live on Android](how-to/android.md)
  - [Publish from a Raspberry Pi](how-to/raspberry-pi.md)

# Concepts

- [How it fits together](concepts/overview.md)
- [Nodes, sessions and routes](concepts/nodes.md)
- [Broadcasts, tracks and renditions](concepts/broadcasts.md)
- [Paths, layouts and tickets](concepts/paths.md)
- [Access control](concepts/access.md)
- [Relays](concepts/relays.md)
- [Calls and rooms](concepts/calls-and-rooms.md)

# Reference

- [`irl` CLI reference](reference/cli.md)
- [Relay flags and endpoints](reference/relay.md)
- [Cargo features](reference/features.md)
- [Environment variables and files](reference/environment.md)
- [Access defaults and policy API](reference/access.md)
- [Ticket formats](reference/tickets.md)
- [Player settings](reference/player.md)
- [Platform support](reference/platforms.md)
- [Crates and API docs](reference/crates.md)
- [Glossary](glossary.md)

# Internals

- [Internals overview](architecture/index.md)
  - [The node and its transport](architecture/transport.md)
  - [Authorization and audiences, explained](architecture/auth-explained.md)
  - [Peer-to-peer and the relay](architecture/p2p-relay.md)
  - [Publishing](architecture/publish.md)
  - [Subscribing](architecture/subscribe.md)
  - [Adaptive rendition switching](architecture/adaptive.md)
  - [Playout and A/V sync](architecture/playout.md)
  - [Instrumentation and tests](architecture/devtools.md)

# Help

- [Troubleshooting and FAQ](troubleshooting.md)
- [Migrating from the previous release](migration.md)
- [Contributing](contributing.md)
