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

`irl <command> --help` lists the options.

## Features

| Feature | Default | Effect |
|---|---|---|
| `aec` | yes | echo cancellation for `call` and `room` |
| `playback` | yes | audio playback |
| `sound-server` | yes | PipeWire and PulseAudio devices |
| `pipewire` | yes | PipeWire's default camera and the desktop portal's screen picker as the Linux defaults |
| `rpicam` | no | `--video rpicam`, the Raspberry Pi camera |
| `vaapi`, `nvidia` | no | hardware encode and decode on Intel and AMD (VA-API) or NVIDIA |
| `v4l2` | no | the V4L2 memory-to-memory H.264 codecs of ARM SoCs, such as a Pi 4 |
