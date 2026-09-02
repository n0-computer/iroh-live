# pi-zero-demo

Publishes a live camera stream from a Raspberry Pi Zero 2 W over iroh, and can
show the connection ticket as a QR code on a Waveshare 2.13" Touch e-Paper HAT.
It also watches a remote stream, rendering with GLES2 either in a window or
straight to HDMI.

The e-paper display is optional. Without the HAT, or with SPI disabled, the
binary still runs and prints the ticket to the terminal.

## Hardware

- **Board**: Raspberry Pi Zero 2 W. A Pi 4 or 5 should work.
- **Camera**: any Pi-compatible CSI camera module.
- **Display** (optional): [Waveshare 2.13inch Touch e-Paper HAT](https://www.waveshare.com/wiki/2.13inch_Touch_e-Paper_HAT_Manual),
  revision V4.

## Commands

```sh
./pi-zero-demo publish [--epaper] [--relay <ENDPOINT_ID>] [--name pi-zero]
                       [--width 640] [--height 360] [--fps 30] [--bitrate 500000]
./pi-zero-demo watch <TICKET> [--fb] [--fullscreen]
./pi-zero-demo fb-demo
./pi-zero-demo epaper-demo
```

`publish` runs `rpicam-vid`, publishes the Annex-B H.264 it produces, prints a
ticket, and optionally renders it as a QR code on the HAT. `--relay` also
connects to a relay; publishing is node-wide, so the announce follows the
connection with nothing further to configure.

`watch` subscribes and renders. Without `--fb` it opens a window through glutin
and winit, which needs the `windowed` feature, on by default. With `--fb` it
takes over the console and renders through DRM/KMS, GBM, and EGL directly to
HDMI, with no window system. `--endpoint-id` plus `--name` works in place of a
ticket.

`fb-demo` renders a generated pattern to HDMI with no network and no camera,
which isolates the display path. `epaper-demo` walks the HAT through a
checkerboard, a QR code, and a clear.

## How capture works

On Raspberry Pi OS the CSI camera is only reachable through the libcamera stack:
`/dev/video0` hands back raw Bayer data from the Unicam sensor, unusable without
the ISP. `rpicam-vid` drives that pipeline and the Pi's hardware H.264 encoder,
so this demo reads the Annex-B bytes from its stdout and publishes them
unchanged. The Pi never software-encodes, and the raw YUV pipe (about 10 MB/s at
640x360) never happens.

`moq_mux::codec::h264` splits the stream into access units and derives the
catalog entry from its first SPS, so nothing has to describe an encoding it did
not perform.

`rpicam-vid` has to be on `PATH`. It ships with Raspberry Pi OS.

The V4L2 stateful M2M path that drove the VideoCore codec directly, and the
`codec-test` subcommand that exercised it, were removed with the in-house codec
stack and have no upstream replacement. Watching on the Pi decodes H.264 in
software through openh264.

## Rendering

`src/gles.rs` is a GLES2 renderer over `glow`. The Pi Zero has no Vulkan and no
wgpu, so `moq_video::render` cannot draw there. Two upload routes are chosen from
the decoder's surface: I420 goes up as three `LUMINANCE` textures and is
converted with a BT.601 limited-range fragment shader, which is what openh264
produces, and packed RGBA goes up as a single texture. There is no zero-copy
path.

## Cross-compiling

```sh
cargo make cross-sysroot-aarch64                            # once, from the repo root
cargo make cross-build-aarch64 -- -p pi-zero-demo --release
```

The binary is at `target/aarch64-unknown-linux-gnu/release/pi-zero-demo`. See
[cross/README.md](../../cross/README.md) for prerequisites and the Docker path
for hosts that cannot install zig.

Building on the Pi works too, and is slower:

```sh
sudo apt install build-essential libasound2-dev libpipewire-0.3-dev pkg-config
cargo build -p pi-zero-demo --release
```

The sysroot under `cross/` is assembled from Debian packages, so nothing has to
be copied off a running Pi to build for one.

## Deploying

```sh
scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo pi@<PI_IP>:~/
```

Or `cargo make cross-deploy` from this directory, which builds, strips, and
copies to `$PI_HOST` (default `livepizero`).

## Pi setup

A fresh Raspberry Pi OS Bookworm, 64-bit, with SSH enabled.

### WiFi, before the first boot

`setup-network.sh` writes a NetworkManager profile onto a Bookworm rootfs while
the SD card is still mounted on the host, which brings the Pi up on the network
without a keyboard or a monitor:

```sh
./setup-network.sh <SSID> <PSK> [ROOTFS_PATH]
```

`ROOTFS_PATH` defaults to `/var/run/media/$USER/rootfs`.

### Camera

```sh
sudo raspi-config     # Interface Options -> Camera -> Enable
sudo reboot
rpicam-hello --timeout 2000
```

### SPI, for the e-paper HAT

```sh
sudo raspi-config     # Interface Options -> SPI -> Enable
sudo reboot
ls /dev/spidev0.0
```

The HAT plugs onto the 40-pin header with no extra wiring. Align pin 1 and press
it on. The touch controller uses I2C, which this demo does not need.

Active pins:

| Function | BCM GPIO | Board pin |
|----------|----------|-----------|
| SPI MOSI | 10       | 19        |
| SPI SCLK | 11       | 23        |
| SPI CE0  | 8        | 24        |
| DC       | 25       | 22        |
| RST      | 17       | 11        |
| BUSY     | 24       | 18        |

### Permissions

```sh
sudo usermod -aG video,spi,gpio $USER
```

Log out and back in.

`gpu_mem` no longer matters. It sized the VideoCore memory the M2M codec used,
and nothing here opens that codec.

### Camera cable

Plug the CSI ribbon into the small connector near the HDMI port, not the display
connector. Contacts face the board: lift the plastic clip, insert, press back
down.

## Running

```sh
RUST_LOG=info ./pi-zero-demo publish --epaper
```

The ticket is printed to the terminal whether or not the HAT works.

### Persistent secret key

iroh generates a new secret key on first run, so the ticket changes on every
restart unless you pin it. The first run logs the value:

```sh
export IROH_SECRET=abcdef...
./pi-zero-demo publish
```

## Watching from a desktop

```sh
irl watch <TICKET>
```

Or scan the QR code off the e-paper display.

## Troubleshooting

**No camera.** Check the ribbon cable, run `rpicam-hello`, and confirm the camera
is enabled in `raspi-config`. `rpicam-vid` must be on `PATH`. Running
`v4l2-ctl --list-devices` should list both `unicam` and `bcm2835-codec`; if it
does not, the sensor is not being detected at all and no amount of userspace
configuration will help.

**"could not display QR on e-paper".** SPI is off, the HAT is not connected, or
permissions on `/dev/spidev0.0` or `/dev/gpiochip0` are wrong. The stream keeps
publishing regardless.

**Nothing on HDMI with `--fb`.** Try `fb-demo` first: it removes the network and
the camera from the picture and leaves only DRM/KMS and GLES2.

**The stream stutters or drops.** The Pi Zero's WiFi produces GSO errors
(`sendmsg: Input/output error`) that iroh recovers from on its own, so those log
lines by themselves are not the cause. Pinning a nearby relay usually helps more
than anything else:

```sh
IROH_RELAY=https://euc1-1.relay.n0.iroh-canary.iroh.link./ ./pi-zero-demo publish
```

Turning off WiFi power saving with `sudo iw wlan0 set power_save off` removes
another source of latency spikes.

**SSH is slow to connect.** Set `UseDNS no` in `/etc/ssh/sshd_config` on the Pi
and restart sshd, and set `GSSAPIAuthentication no` for the host in your
`~/.ssh/config`.

## E-paper precautions

`epaper.rs` and `epd_v4.rs` are a hand-written driver for the V4 panel
(SSD1680), because `epd-waveshare` covers V2 and V3 only and the V4 refresh
command differs. The code respects every
[Waveshare precaution](https://www.waveshare.com/wiki/2.13inch_Touch_e-Paper_HAT_Manual#Precautions):

| # | Precaution | Status |
|---|-----------|--------|
| 1 | No continuous partial refresh without a full one | Full refresh only, never partial |
| 2 | Do not leave powered on when not refreshing | `epd.sleep()` after every update |
| 3 | Minimum 180 s between refreshes, at least one per 24 h | Periodic refresh every 12 h |
| 4 | Re-initialise after sleep before sending data | Every operation creates a fresh `Epd2in13` |
| 5 | Border waveform register | Not applicable: the defaults suit a QR code |
| 6 | Image size must match the display | The buffer is exactly 122x250 |
| 7 | Working voltage and level conversion | Handled by the HAT hardware from V2.1 |
| 8 | The FPC cable is fragile | Physical handling |
| 9 | The screen is fragile | Physical handling |
| 10 | Clear before long-term storage | Cleared to white on Ctrl-C |
