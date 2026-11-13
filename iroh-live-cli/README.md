# iroh-live-cli

`irl`, a command line tool to publish, watch, record and call over
[iroh-live](../iroh-live).

Use it to try iroh-live without writing code: publish a camera, a screen or a
file, and watch it from another machine with the printed ticket.

```sh
cargo install --locked --path iroh-live-cli
irl publish --video test          # prints a ticket
irl watch <ticket>
```

## Commands

- `publish`: publish a capture device or a media file.
- `watch`: subscribe to a broadcast and play it.
- `record`: record a broadcast to a file, without a window.
- `call`: place or answer a 1:1 video call.
- `room`: join a room, publish, and watch everyone else.
- `run`: run a multi-stream session described by a TOML file.
- `devices`: list cameras, displays and audio devices.

`irl <command> --help` lists the options, and
[docs/cli.md](../docs/reference/cli.md) describes them all.

The first Ctrl+C shuts down gracefully: it leaves rooms, closes broadcasts and
then the endpoint, so peers learn at once that the node is gone. A second
Ctrl+C exits at once, and a watchdog exits the process if the shutdown takes
longer than five seconds.

## Features

Every feature is on by default, and the Linux-only ones compile to nothing on
macOS and Windows. [Cargo features](../docs/reference/features.md) lists them.
