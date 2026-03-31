# pi-zero-minimal

Minimal Raspberry Pi camera publisher. Publishes pre-encoded H.264 from
libcamera directly to the MoQ transport — no software encoding, minimal
CPU usage on the Pi Zero 2 W.

## Build

Native (on the Pi):

```sh
cargo build -p pi-zero-minimal --release
```

Cross-compile from x86_64 (see [cross/README.md](../../cross/README.md)
for sysroot setup):

```sh
cargo make cross-sysroot-aarch64             # one-time
cargo make cross-build-aarch64 -- -p pi-zero-minimal --release
```

The binary is at `target/aarch64-unknown-linux-gnu/release/pi-zero-minimal`.

## Deploy

```sh
scp target/aarch64-unknown-linux-gnu/release/pi-zero-minimal pi@raspberrypi:~/
```

## Run

```sh
# On the Pi
./pi-zero-minimal

# Prints a ticket — watch from any other machine:
irl play <TICKET>
```

Requires `rpicam-vid` on the Pi (installed by default on Raspberry Pi OS).

## What it does

1. Starts an iroh-live session with a router (accepts connections)
2. Launches `rpicam-vid` as a subprocess for hardware H.264 encoding
3. Feeds encoded NALs directly to the MoQ transport (no decode/re-encode)
4. Prints the connection ticket
5. Waits for Ctrl-C
