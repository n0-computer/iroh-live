# Raspberry Pi

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | iroh-live, rusty-codecs (v4l2) |
| Platforms | Raspberry Pi Zero 2 W, Pi 4, Pi 5 |

## Hardware

The `demos/pi-zero` crate publishes a live camera stream from a Raspberry Pi and optionally displays the connection ticket as a QR code on an e-paper HAT.

- **Board**: Raspberry Pi Zero 2 W (tested), Pi 4 and Pi 5 should also work.
- **Camera**: Any Pi-compatible CSI camera module that exposes V4L2 devices.
- **Display** (optional): Waveshare 2.13" Touch e-Paper HAT for QR code display.

## Pi setup

These steps assume a fresh Raspberry Pi OS (Bookworm, 64-bit) with SSH enabled.

### Enable the camera

```sh
sudo raspi-config
# -> Interface Options -> Camera -> Enable
sudo reboot
```

After reboot, verify with `v4l2-ctl --list-devices`. You should see `/dev/video0` or similar.

### Set GPU memory

The bcm2835-codec hardware encoder shares VideoCore GPU memory. The default 64 MB supports resolutions up to 640x480. For 720p and above, set at least 128 MB:

```sh
echo 'gpu_mem=128' | sudo tee -a /boot/firmware/config.txt
sudo reboot
```

Verify with `vcgencmd get_mem gpu`.

### Enable SPI (for e-paper HAT)

```sh
sudo raspi-config
# -> Interface Options -> SPI -> Enable
sudo reboot
```

The HAT plugs directly onto the 40-pin GPIO header. If the HAT is not connected or SPI is not enabled, the binary still runs and prints the ticket to the terminal.

### Permissions

The binary needs access to `/dev/video*` (camera), `/dev/spidev*` (SPI), and `/dev/gpiochip0` (GPIO). Either run as root or add your user to the required groups:

```sh
sudo usermod -aG video,spi,gpio $USER
```

Log out and back in for group changes to take effect.

## Cross-compilation

The Pi Zero 2 W runs 64-bit ARM (aarch64) Linux. Cross-compiling on your host machine is much faster than building natively.

### Sysroot from your Pi

Install dev headers on the Pi:

```sh
sudo apt install libasound2-dev
```

Pull the sysroot to your host and install the cross-compiler:

```sh
mkdir -p ~/pi-sysroot
rsync -az pi@<PI_IP>:/usr/lib/aarch64-linux-gnu ~/pi-sysroot/usr/lib/
rsync -az pi@<PI_IP>:/usr/include ~/pi-sysroot/usr/
rsync -az pi@<PI_IP>:/lib/aarch64-linux-gnu ~/pi-sysroot/lib/

rustup target add aarch64-unknown-linux-gnu

# Arch Linux:
sudo pacman -S aarch64-linux-gnu-gcc
# Debian/Ubuntu:
# sudo apt install gcc-aarch64-linux-gnu
```

### Build

The included script sets up pkg-config, linker search paths, and validates the sysroot:

```sh
./demos/pi-zero/build.sh

# Or with a custom sysroot path:
PI_SYSROOT=/path/to/sysroot ./demos/pi-zero/build.sh
```

### Deploy

Copy the binary over SSH:

```sh
scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo pi@<PI_IP>:~/
```

## Running

```sh
RUST_LOG=info ./pi-zero-demo
```

The binary captures the camera at 720p via V4L2, encodes H.264 using the VideoCore hardware encoder (falling back to openh264 software encoding if the V4L2 M2M device is unavailable), publishes the stream, and prints the ticket. If the e-paper HAT is attached, it renders the ticket as a QR code and puts the display to sleep.

### Persistent secret key

On first run, iroh generates a new secret key and endpoint address. The ticket changes on every restart unless you pin the key:

```sh
# First run prints: INFO Generated new secret key. Reuse with IROH_SECRET=abcdef...
export IROH_SECRET=abcdef...
./pi-zero-demo
```

## Watching from desktop

On another machine with a display:

```sh
cargo run --example watch -- <TICKET>
```

Replace `<TICKET>` with the string printed by the Pi, or scan the QR code from the e-paper display.

## Performance

With 128 MB GPU memory on a 512 MB Pi Zero 2 W:

| Resolution | HW Encode | HW Decode | Roundtrip |
|-----------|-----------|-----------|-----------|
| 640x360 | 138 fps | 60 fps | 44 fps |
| 1280x720 | 53 fps | ~40 fps | 28 fps |
| 1920x1080 | 23 fps | 9 fps | 7 fps |

720p is comfortably real-time at 30 fps. 1080p encoding is near real-time but decode is slow because of CPU-side YUV-to-RGBA conversion; a future NV12 direct render path would improve this.

## Testing the V4L2 hardware codec

The `codec-test` subcommand runs on-device encoder, decoder, and round-trip tests using the SMPTE test pattern (no camera needed):

```sh
# Build and deploy
./demos/pi-zero/build.sh --deploy livepizero

# Run all tests at 640x360
ssh pi@livepizero "./pi-zero-demo codec-test all --frames 60"

# Test a specific resolution
ssh pi@livepizero "./pi-zero-demo codec-test roundtrip --frames 30 --width 1280 --height 720"

# Save decoded frames for visual inspection
ssh pi@livepizero "./pi-zero-demo codec-test roundtrip --frames 10 --save /tmp/frames"
```

## V4L2 portability

The V4L2 M2M encoder/decoder API is a Linux kernel standard, but SoC drivers differ in their defaults. The current implementation is tested on the Pi's bcm2835-codec. Key differences on other hardware:

- bcm2835 defaults to H.264 Level 1.0 (128x96 max). The encoder auto-selects the level based on resolution. Other SoCs may auto-negotiate.
- bcm2835 uses `repeat_sequence_header` instead of the standard `V4L2_CID_MPEG_VIDEO_PREPEND_SPSPPS_TO_IDR`. The encoder tries both.
- bcm2835 outputs YU12 (I420, planar). Other decoders may output NV12 (semi-planar). The decoder checks the negotiated format and handles both.
- Override device paths with `V4L2_ENC_DEVICE` and `V4L2_DEC_DEVICE` environment variables.

See the `rusty-codecs` V4L2 module documentation for full details on cross-SoC portability.
