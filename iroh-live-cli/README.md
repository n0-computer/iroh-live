# iroh-live-cli

`irl`, a command line tool to publish, watch, record and call over
[iroh-live](../iroh-live).

Use it to try iroh-live without writing code: publish a camera, a screen or a
file, and watch it from another machine with the printed ticket.

```sh
cargo install --path iroh-live-cli
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

`irl <command> --help` lists the options.

## Features

| Feature | Default | Effect |
|---|---|---|
| `aec` | yes | echo cancellation for `call` and `room` |
| `playback` | yes | audio playback |
| `sound-server` | yes | PipeWire and PulseAudio devices |
| `pipewire` | no | Linux screen capture through the desktop portal |
| `rpicam` | no | `--video rpicam`, the Raspberry Pi camera |
| `vaapi`, `nvidia` | no | hardware encoders |
