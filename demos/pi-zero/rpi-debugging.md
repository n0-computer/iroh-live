# Pi Zero 2 Demo — Quick Start

Run the `pi-zero-demo` binary on a Raspberry Pi Zero 2 W to publish a
camera stream over iroh. Watch the stream from any machine on the same
network (or through a relay) using the `watch` example.

## Prerequisites

| What | On the Pi |
|------|-----------|
| OS | Raspberry Pi OS Bookworm (64-bit, aarch64) |
| Camera | CSI camera (OV5647 / IMX219) connected to the flat ribbon cable |
| Enable camera | `sudo raspi-config` → Interface Options → Camera → Enable |
| Enable SPI (for e-paper) | `sudo raspi-config` → Interface Options → SPI → Enable |
| Dev packages | `sudo apt install libasound2-dev libgbm-dev libdrm-dev libegl-dev` |
| Test camera | `rpicam-still --output test.jpg` (should produce a JPEG) |

## Build and deploy

From your development machine (x86_64 Linux):

```sh
# One-time setup
rustup target add aarch64-unknown-linux-gnu
sudo pacman -S aarch64-linux-gnu-gcc   # Arch
# sudo apt install gcc-aarch64-linux-gnu  # Debian/Ubuntu

# Create Pi sysroot (needs SSH access to Pi)
mkdir -p ~/pi-sysroot
rsync -az pi@<PI_IP>:/usr/lib/aarch64-linux-gnu ~/pi-sysroot/usr/lib/
rsync -az pi@<PI_IP>:/usr/include ~/pi-sysroot/usr/
rsync -az pi@<PI_IP>:/lib/aarch64-linux-gnu ~/pi-sysroot/lib/

# Build + deploy
./demos/pi-zero/build.sh --deploy <PI_HOSTNAME>
```

Or with `cargo-make`:

```sh
cd demos/pi-zero
PI_HOST=livepizero cargo make deploy
```

## Publish

```sh
# On the Pi
./pi-zero-demo publish --epaper
```

Flags:
- `--epaper` — show the connection ticket as a QR code on the e-paper HAT
- `--encoder hardware` (default) — use rpicam-vid's internal H.264 HW encoder
- `--encoder software` — raw YUV capture + openh264 software encoder
- `--relay <ENDPOINT_ID>` — also publish to a relay for browser clients

The ticket is always printed to the terminal regardless of `--epaper`.

## Watch (from another machine)

```sh
# Using the iroh-live watch example (egui window)
cargo run --release --example watch -- --ticket "iroh-live:..."

# Or from another Pi (framebuffer rendering, no window system)
./pi-zero-demo watch --ticket "iroh-live:..." --fb
```

## E-paper test

```sh
./pi-zero-demo epaper-demo
```

Runs a three-step test: checkerboard pattern → QR code → clear to white.
Press Enter between steps. If nothing appears, check:

1. Is SPI enabled? (`ls /dev/spidev0.0`)
2. Is the HAT properly seated on the GPIO header?
3. Is it a V4 controller? (product SKU 20716 = V4)

## Troubleshooting

### Camera not working
- `rpicam-still --output test.jpg` — if this fails, the camera isn't connected or enabled
- Check `v4l2-ctl --list-devices` — you should see `unicam` and `bcm2835-codec`

### SSH is slow
- Add `UseDNS no` to `/etc/ssh/sshd_config` on the Pi, restart sshd
- Add `GSSAPIAuthentication no` to your `~/.ssh/config`
- Disable WiFi power save: `sudo iw wlan0 set power_save off`

### Stream stutters or disconnects
- Pin the relay: `IROH_RELAY=https://euc1-1.relay.n0.iroh-canary.iroh.link./ ./pi-zero-demo publish`
- The Pi's WiFi triggers GSO errors (`sendmsg: Input/output error`) — iroh recovers automatically

### Build fails with cross-compilation errors
- Check the sysroot has libasound, libgbm, libdrm, libegl dev files
- The ffmpeg feature cannot be cross-compiled — use V4L2 HW encoder instead
- See `build.sh` header comments for detailed prerequisites
