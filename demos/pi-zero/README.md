# pi-zero-demo

Publishes a Raspberry Pi camera over iroh, and can show the ticket as a QR code
on a Waveshare 2.13" Touch e-Paper HAT. It also watches a remote stream,
rendered with GLES2 in a window or straight to HDMI.

The e-paper display is optional. Without the HAT, or with SPI off, the ticket
goes to the terminal only.

## Hardware

- **Board**: Raspberry Pi Zero 2 W. A Pi 4 or 5 should work.
- **Camera**: any Pi CSI camera module.
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

`publish` runs `rpicam-vid`, publishes the H.264 it writes, and prints a
ticket. `--epaper` also draws the ticket as a QR code on the HAT. `--relay`
also pushes the broadcast to a relay and redials it if the session drops, so
browsers can watch it there.

`watch` subscribes and renders. Without `--fb` it opens a window through glutin and winit, which needs the
`windowed` feature (on by default). With `--fb` it renders through DRM/KMS,
GBM, and EGL straight to HDMI, with no window system.

`fb-demo` renders a test pattern to HDMI with no network and no camera, to check
the display path alone. `epaper-demo` shows a checkerboard, then a QR code,
then clears the HAT, waiting for Enter between steps.

## How it works

On Raspberry Pi OS the CSI camera is only reachable through libcamera:
`/dev/video0` returns raw Bayer data that needs the ISP. `rpicam-vid` drives
the ISP and the Pi's hardware H.264 encoder. The demo reads its Annex-B output
and publishes it unchanged, so the Pi never encodes in software. `rpicam-vid`
has to be on `PATH`, and it ships with Raspberry Pi OS.

Watching on the Pi decodes H.264 in software. `src/gles.rs` draws the pictures
with GLES2 through `glow`, since the Pi Zero has no Vulkan and
`moq_video::render` cannot run there. I420 pictures go up as three `LUMINANCE`
textures and a shader converts them from BT.601 limited range. Any other
picture goes up as one RGBA texture.

## Building

Cross-compile from the repository root:

```sh
cargo make cross-sysroot-aarch64                            # once
cargo make cross-build-aarch64 -- -p pi-zero-demo --release
```

The binary is at `target/aarch64-unknown-linux-gnu/release/pi-zero-demo`. The
sysroot is built from Debian packages, so nothing has to be copied off a Pi.
See [cross/README.md](../../cross/README.md) for the prerequisites and a Docker
path for hosts without zig.

Building on the Pi also works, but slowly:

```sh
sudo apt install build-essential libasound2-dev libpipewire-0.3-dev pkg-config
cargo build -p pi-zero-demo --release
```

## Deploying

```sh
scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo pi@<PI_HOST>:~/
```

Or run `cargo make cross-deploy` in this directory. It builds, strips, and
copies the binary to `pi@$PI_HOST:~/pi-zero-demo`. `PI_HOST` defaults to
`livepizero`.

## Pi setup

Start from Raspberry Pi OS Bookworm, 64-bit, with SSH enabled.

### WiFi, before the first boot

`scripts/setup-network.sh` runs on the development machine. It writes a
NetworkManager profile onto the SD card's rootfs while the card is mounted, so
the Pi joins the network without a keyboard or a monitor:

```sh
./scripts/setup-network.sh <SSID> <PSK> [ROOTFS_PATH]
```

`ROOTFS_PATH` defaults to `/var/run/media/$USER/rootfs`.

### Camera

Plug the CSI ribbon into the small connector near the HDMI port, not the
display connector. The contacts face the board: lift the plastic clip, insert
the ribbon, and press the clip back down. Then:

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

The HAT plugs onto the 40-pin header with no extra wiring. The demo uses these
pins:

| Function | BCM GPIO | Board pin |
|----------|----------|-----------|
| SPI MOSI | 10       | 19        |
| SPI SCLK | 11       | 23        |
| SPI CE0  | 8        | 24        |
| DC       | 25       | 22        |
| RST      | 17       | 11        |
| BUSY     | 24       | 18        |

The touch controller uses I2C, which the demo does not need.

### Permissions

```sh
sudo usermod -aG video,spi,gpio $USER
```

Log out and back in.

## Running

```sh
RUST_LOG=info ./pi-zero-demo publish --epaper
```

The ticket is printed to the terminal whether or not the HAT works. Watch it
from a desktop with `irl watch <TICKET>`, or scan the QR code off the display.

### A stable ticket

The ticket names the endpoint id, which comes from a secret key. Without
`IROH_SECRET` the demo takes a new key on every start, so the ticket changes.
Pin one with 64 hex characters:

```sh
export IROH_SECRET=$(openssl rand -hex 32)   # once, then keep the value
./pi-zero-demo publish
```

### Starting on boot

[`scripts/pi-zero-demo.service`](scripts/pi-zero-demo.service) is a systemd
user unit that runs `publish --name pi-zero`.
[`scripts/install-service.sh`](scripts/install-service.sh) installs it, and
runs on the Pi:

```sh
scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo pi@<PI_HOST>:~/
scp -r demos/pi-zero/scripts pi@<PI_HOST>:~/
ssh pi@<PI_HOST> ./scripts/install-service.sh
```

The script checks that `~/pi-zero-demo` runs, and generates `IROH_SECRET` into
`~/.config/pi-zero-demo/env` on the first install. Later installs keep it. It
enables lingering, so the service starts at boot rather than at login, and
prints the ticket once the publisher is up. Run it again after copying a new
binary.

Extra options such as `--epaper` go in `PI_ZERO_DEMO_ARGS` in the same env
file. With `--epaper`, raise `RestartSec` in the unit to 180 or more: the panel
refreshes at every start, and its datasheet asks for 180 s between refreshes.
The ticket also goes to the journal: `journalctl --user -u pi-zero-demo -f`.

## Troubleshooting

**No camera.** Check the ribbon, run `rpicam-hello`, and check that the camera
is enabled in `raspi-config`. `v4l2-ctl --list-devices` should list `unicam`.
If it does not, the sensor is not detected at all.

**"could not display QR on e-paper".** SPI is off, the HAT is not connected,
or the permissions on `/dev/spidev0.0` or `/dev/gpiochip0` are wrong. The
broadcast keeps publishing.

**Nothing on HDMI with `--fb`.** Run `fb-demo` first. It leaves out the network
and the camera, and tests only DRM/KMS and GLES2.

**The stream stutters.** The Pi Zero's WiFi logs GSO errors
(`sendmsg: Input/output error`) that iroh recovers from, so those lines alone
are not the cause. Turning off WiFi power saving with
`sudo iw wlan0 set power_save off` removes one source of latency spikes.

**SSH is slow to connect.** Set `UseDNS no` in `/etc/ssh/sshd_config` on the Pi
and restart sshd. Set `GSSAPIAuthentication no` for the host in your
`~/.ssh/config`.

## E-paper driver

`epaper.rs` and `epd_v4.rs` are a driver for the V4 panel (SSD1680). The
`epd-waveshare` crate covers V2 and V3 only, and the V4 refresh command
differs. The driver follows the
[Waveshare precautions](https://www.waveshare.com/wiki/2.13inch_Touch_e-Paper_HAT_Manual#Precautions):

| # | Precaution | How the driver handles it |
|---|-----------|--------|
| 1 | No continuous partial refresh without a full one | Full refresh only, never partial |
| 2 | Do not leave powered on when not refreshing | `epd.sleep()` after every update |
| 3 | Minimum 180 s between refreshes, at least one per 24 h | Refresh every 12 h |
| 4 | Re-initialise after sleep before sending data | Every operation creates a fresh `Epd2in13V4` |
| 5 | Border waveform register | The defaults suit a QR code |
| 6 | Image size must match the display | The buffer is exactly 122x250 |
| 7 | Working voltage and level conversion | Handled by the HAT from V2.1 |
| 8 | The FPC cable is fragile | Physical handling |
| 9 | The screen is fragile | Physical handling |
| 10 | Clear before long-term storage | Cleared to white on Ctrl-C |
