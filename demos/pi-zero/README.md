# pi-zero-demo

Publishes a live camera stream from a Raspberry Pi Zero 2 W over iroh, and
displays the connection ticket as a QR code on a Waveshare 2.13" Touch e-Paper
HAT.

The e-paper display is optional --if the HAT is not connected or SPI is not enabled, the binary still runs and prints the ticket to the terminal.

## Hardware

- **Board**: Raspberry Pi Zero 2 W
- **Camera**: CSI camera module (any Pi-compatible camera that exposes V4L2)
- **Display** (optional): [Waveshare 2.13inch Touch e-Paper HAT](https://www.waveshare.com/wiki/2.13inch_Touch_e-Paper_HAT_Manual)

## What it does

1. Starts an iroh endpoint and creates a media broadcast.
2. Captures the camera at 720p via V4L2.
3. Encodes H.264 --uses the VideoCore hardware encoder (V4L2 M2M) when
   available, otherwise falls back to openh264 software encoding.
4. Publishes the stream under the name `pi-zero`.
5. Prints the `LiveTicket` string to the terminal (always, regardless of HAT).
6. If the e-paper HAT is attached: renders the ticket as a QR code on the
   display with a full refresh, then immediately puts the display to sleep
   (zero power draw, image retained). A background task re-displays the QR
   every 12 hours to satisfy the datasheet's 24 h refresh requirement.
7. Waits for Ctrl-C. On shutdown, clears the e-paper to white (datasheet:
   clear before storage) and shuts down the iroh session.

## Cross-compiling

The Pi Zero 2 W runs a 64-bit ARM (aarch64) Linux. Build on your host machine.

Cross-compilation needs an aarch64 sysroot with C library headers (ALSA for audio, PipeWire for camera capture). The simplest approach is to grab the sysroot from a running Pi.

### Option A: sysroot from your Pi (recommended)

One-time setup on the Pi to install dev headers:

```sh
# On the Pi:
sudo apt install libasound2-dev
```

One-time setup on your host machine to pull the sysroot and install the cross-compiler:

```sh
mkdir -p ~/pi-sysroot
rsync -az pi@<PI_IP>:/usr/lib/aarch64-linux-gnu ~/pi-sysroot/usr/lib/
rsync -az pi@<PI_IP>:/usr/include ~/pi-sysroot/usr/
rsync -az pi@<PI_IP>:/lib/aarch64-linux-gnu ~/pi-sysroot/lib/

rustup target add aarch64-unknown-linux-gnu
# Arch Linux:
sudo pacman -S aarch64-linux-gnu-gcc
# Debian/Ubuntu:
#   sudo apt install gcc-aarch64-linux-gnu
```

Build with the included script (sets up pkg-config, linker search paths, etc.):

```sh
./demos/pi-zero/build.sh

# Or with a custom sysroot path:
PI_SYSROOT=/path/to/sysroot ./demos/pi-zero/build.sh
```

The script validates the sysroot, fixes missing `.so` symlinks, and passes the right `-L` flags so the linker can find aarch64 libraries like ALSA.

### Option B: build natively on the Pi

Slower but avoids all cross-compilation issues:

```sh
# On the Pi:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo apt install build-essential libasound2-dev libpipewire-0.3-dev pkg-config
cargo build -p pi-zero-demo --release
```

The binary is at `target/release/pi-zero-demo` (native) or
`target/aarch64-unknown-linux-gnu/release/pi-zero-demo` (cross-compiled).

## Deploying to the Pi

Copy the binary over SSH:

```sh
scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo pi@<PI_IP>:~/
```

## Pi setup

These steps assume a fresh Raspberry Pi OS (Bookworm, 64-bit) with SSH enabled.

### 1. Enable the camera

```sh
# On the Pi:
sudo raspi-config
# -> Interface Options -> Camera -> Enable
# (On Bookworm this enables the libcamera stack which exposes V4L2 devices)

# Reboot after enabling:
sudo reboot
```

After reboot, verify the camera is detected:

```sh
# Should list /dev/video0 (or similar)
v4l2-ctl --list-devices

# Quick test capture (optional, needs libcamera-apps):
libcamera-hello --timeout 2000
```

### 2. Enable SPI (for the e-paper HAT)

```sh
sudo raspi-config
# -> Interface Options -> SPI -> Enable
sudo reboot
```

Verify SPI is available:

```sh
ls /dev/spidev0.0
# Should exist after enabling SPI
```

### 3. Enable I2C (for the touch controller --optional)

The touch controller on the HAT uses I2C. This demo does not use touch input,
but if you want to use it in the future:

```sh
sudo raspi-config
# -> Interface Options -> I2C -> Enable
sudo reboot
```

### 4. Permissions

The binary needs access to `/dev/video*` (camera), `/dev/spidev*` (SPI), and
`/dev/gpiochip0` (GPIO). Either run as root or add your user to the required
groups:

```sh
sudo usermod -aG video,spi,gpio $USER
# Log out and back in for group changes to take effect
```

### 5. Wire the e-paper HAT

The HAT plugs directly onto the Pi's 40-pin GPIO header --no extra wiring
needed. Just push it on, making sure pin 1 aligns.

Pin mapping (active pins used by this demo):

| Function | BCM GPIO | Board pin |
|----------|----------|-----------|
| SPI MOSI | 10       | 19        |
| SPI SCLK | 11       | 23        |
| SPI CE0  | 8        | 24        |
| DC       | 25       | 22        |
| RST      | 17       | 11        |
| BUSY     | 24       | 18        |

### 6. Connect the camera

Plug the CSI ribbon cable into the Pi Zero's camera connector (the small one
near the HDMI port, not the display connector). The contacts face the board.
Gently lift the plastic clip, insert the cable, and press the clip back down.

## Running

```sh
# On the Pi:
RUST_LOG=info ./pi-zero-demo
```

Output:

```
INFO selected video codec: h264-v4l2
publishing at pi-zero@abcdef1234...
INFO QR code displayed on e-paper
```

The ticket string (`pi-zero@...`) is always printed to the terminal regardless of whether the e-paper display works. If the HAT is not connected, you will see a warning, but the stream keeps publishing.

### Persistent secret key

On first run, iroh generates a new secret key and endpoint address. The ticket changes on every restart unless you pin the key:

```sh
# First run prints the generated key:
#   INFO Generated new secret key. Reuse with IROH_SECRET=abcdef...

# Re-use it on subsequent runs so the ticket stays the same:
export IROH_SECRET=abcdef...
./pi-zero-demo
```

## Watching the stream

On another machine (with a display), use the `watch` example from the workspace:

```sh
# From the iroh-live2 repo on your laptop/desktop:
cargo run --example watch -- <TICKET>
```

Replace `<TICKET>` with the ticket string printed by the Pi, or scan the QR code from the e-paper display.

## Troubleshooting

**"No video device found"** --Camera not detected. Check the ribbon cable
connection, run `v4l2-ctl --list-devices`, and make sure the camera is enabled
in `raspi-config`.

**"could not display QR on e-paper"** --SPI not enabled, HAT not connected, or
permission denied on `/dev/spidev0.0` or `/dev/gpiochip0`. Check
`raspi-config` SPI setting and file permissions. The stream is still publishing
normally.

**"no video codec compiled in"** --The binary was built without any video codec
feature. Rebuild with `h264` or `v4l2` features enabled.

**Software encoder is slow** --If the V4L2 hardware encoder is not available
(no `v4l2` feature, or `/dev/video11` missing), the fallback is openh264
software encoding, which may struggle at 720p on the Pi Zero 2's Cortex-A53.
Consider dropping to `VideoPreset::P360` in the source, or ensure the `v4l2`
feature is enabled and the hardware encoder device exists.

## E-paper precautions

The code respects all [Waveshare e-paper precautions](https://www.waveshare.com/wiki/2.13inch_Touch_e-Paper_HAT_Manual#Precautions):

| # | Precaution | Status |
|---|-----------|--------|
| 1 | No continuous partial refresh without full refresh | **Implemented** --only full refresh is used, never partial. |
| 2 | Do not leave powered on when not refreshing | **Implemented** --`epd.sleep()` is called immediately after every frame update. |
| 3 | Min 180 s between refreshes; refresh at least once per 24 h | **Implemented** --periodic refresh runs every 12 h (well above 180 s floor, well within 24 h ceiling). |
| 4 | After sleep, must re-initialise before sending data | **Implemented** --every operation calls `open_epd()` which creates a fresh `Epd2in13`, re-initialising from scratch. |
| 5 | Border waveform register (0x3C / 0x50) | **N/A** --default border settings from `epd-waveshare` are fine for QR display. |
| 6 | Image size must match display | **Implemented** --`Display2in13` buffer is exactly 122x250, matching the panel. |
| 7 | Working voltage 3.3V / level conversion | **N/A** --the HAT (V2.1+) has built-in level conversion; handled by hardware. |
| 8 | FPC cable is fragile --do not bend | **N/A** --physical handling, not software. Noted in README for awareness. |
| 9 | Screen is fragile --avoid drops/pressure | **N/A** --physical handling. |
| 10 | Clear screen before long-term storage | **Implemented** --on Ctrl-C shutdown, the display is cleared to white and put to sleep. |
