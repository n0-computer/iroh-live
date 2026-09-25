# Raspberry Pi

`irl publish --video rpicam` publishes a Pi camera. Two programs target the Pi
as well. `iroh-live/examples/publish-pi.rs` is the shortest program that
publishes a Pi camera. `demos/pi-zero` adds a QR code on an e-paper display and
a player that draws with GLES2, in a window or straight to HDMI. All of them
are tested on a Pi Zero 2 W and a Pi 4.

## Capture is rpicam-vid

On Raspberry Pi OS, the CSI camera is only reachable through libcamera.
`/dev/video0` returns raw Bayer data from the sensor, which is unusable without
the ISP. `rpicam-vid` drives the ISP and the Pi's hardware H.264 encoder and
writes Annex-B H.264 to stdout. A Pi Zero can publish those bytes as they are.

`EncodedVideoSource::rpicam(config)` starts `rpicam-vid` and reads its output.
`LocalBroadcast::set_encoded_video` publishes it as the one rendition `video`,
with the catalog entry taken from the stream's SPS. The process is killed when
the source is dropped. The Pi does no software encoding, and no raw pictures
cross the pipe.

`VideoSource::rpicam(config)` reads raw pictures from `rpicam-vid` instead, for
our own encoders.

Both need the `rpicam` feature, which is Linux only, and `rpicam-vid` on
`PATH`. Raspberry Pi OS ships it.

From the CLI:

```sh
cargo build -p iroh-live-cli --features rpicam
irl publish --video rpicam --width 640 --height 360 --fps 30
```

`irl devices` lists the sensors `rpicam-vid` reports.

`--video rpicam` refuses `--codec h265`, any `--encoder` other than `auto`, and
more than one rung in `--renditions`, since the stream arrives encoded.
`--width`, `--height`, `--fps`, and `--bitrate` become `rpicam-vid` arguments.
`--preview` needs `--video rpicam:raw`.

## The V4L2 hardware codecs

The Pi also has a VideoCore memory-to-memory codec, which the kernel exposes as
a device node. The `v4l2` feature adds an encoder and a decoder for it:

```sh
cargo build -p iroh-live-cli --features v4l2
irl publish --video rpicam:raw --encoder v4l2
```

The decoder joins automatic backend selection. On a host without a
memory-to-memory node it fails to open, and selection moves on to openh264.

Both run on a Pi 4 (bcm2835-codec, Raspberry Pi OS Bookworm). The encoder opens
`/dev/video11`, takes NV12, and produces Constrained Baseline H.264. The decoder
is what `--decoder auto` picks on that board.

The example uses `rpicam:raw` because `--video cam` cannot feed the encoder on
a Pi: `/dev/video0` serves raw Bayer, so it opens and never delivers a frame.

If probing picks the wrong node, `MOQ_V4L2_ENCODER` and `MOQ_V4L2_DECODER`
name one.

## Pi setup

These steps assume Raspberry Pi OS Bookworm, 64-bit, with SSH enabled.

Enable the camera with `sudo raspi-config`, under Interface Options, and
reboot. `rpicam-hello --timeout 2000` checks that it works.

For the e-paper HAT, enable SPI the same way. The HAT plugs onto the 40-pin
header. If it is missing or SPI is off, the demo still runs and prints the
ticket to the terminal.

The user needs access to `/dev/video*`, and with the HAT also `/dev/spidev*`
and `/dev/gpiochip0`:

```sh
sudo usermod -aG video,spi,gpio $USER
```

Log out and back in for this to take effect.

## Cross-compiling

Building on a Pi Zero 2 W works, but slowly. Cross-compiling uses
`cargo-zigbuild` against a Debian Bookworm sysroot. The sysroot is built from
`.deb` files with `dpkg-deb`, without sudo or a chroot.

```sh
cargo make cross-sysroot-aarch64                              # once
cargo make cross-build-aarch64 -- -p iroh-live --example publish-pi --features rpicam --release
cargo make cross-build-aarch64 -- -p pi-zero-demo --release
```

Everything after `--` goes to `cargo zigbuild`. Binaries land in
`target/aarch64-unknown-linux-gnu/release/`, and examples in its `examples/`
directory. `cross/README.md` covers the prerequisites and a Docker build for
hosts without zig.

Copy the binaries with `scp`, or run `cargo make cross-deploy` in
`demos/pi-zero`. It builds, strips, and copies the demo to `$PI_HOST` (default
`livepizero`).

## Watching on a Pi 4: name the decoder

On a Pi 4, `irl watch` picks the V4L2 hardware decoder, which adds about 700 ms
of latency. Measured against a clock drawn into the picture, the hardware
decoder showed a frame about a second after it was drawn, and openh264 about
360 ms after. Both ran at 30 fps. A Pi 4 has enough CPU for openh264 at these
sizes, so name it when latency matters:

```sh
irl watch <TICKET> --fullscreen --decoder openh264
```

A Pi Zero 2 W cannot decode 720p at 30 fps in software, so it needs the
hardware decoder.

## publish-pi

```sh
./publish-pi
```

It takes no flags. It publishes 640x360 at 30 fps under the name `pi-cam` and
prints a ticket. Set `IROH_SECRET` to keep the same endpoint id, and so the
same ticket, across restarts.

## pi-zero-demo

The demo has four subcommands.

`publish` streams the camera and prints the ticket. With `--epaper` it also
shows the ticket as a QR code on the HAT. Flags: `--epaper`,
`--relay <ENDPOINT_ID>`, `--name` (default `pi-zero`), `--width` (640),
`--height` (360), `--fps` (30), `--bitrate` (500000).

`watch <TICKET>` subscribes and draws. Without `--fb` it opens a window through
glutin and winit, which needs the `windowed` feature (on by default).
`--fullscreen` makes the window borderless. With `--fb` it draws straight to
HDMI through DRM/KMS, GBM, and EGL, without a window system. `--endpoint-id`
with `--name` works instead of a ticket.

`fb-demo` draws a test pattern to HDMI without network or camera, to check the
display path on its own.

`epaper-demo` shows a checkerboard, then a QR code, then clears the HAT. Each
step waits for Enter.

## Rendering on the Pi

The Pi Zero has no Vulkan, so `moq_video::render` cannot draw there.
`demos/pi-zero/src/gles.rs` is a GLES2 renderer built on `glow`. I420 frames
from openh264 go up as three `LUMINANCE` textures and are converted by a
BT.601 limited-range shader. Other frames go up as one RGBA texture. Every
frame is copied to the GPU.

## E-paper

`epaper.rs` and `epd_v4.rs` drive the Waveshare 2.13" Touch e-Paper HAT,
revision V4 (SSD1680). `epd-waveshare` covers only V2 and V3, and V4 uses a
different refresh command. The driver does full refreshes only, puts the panel
to sleep after every update, redraws every 12 hours, and clears the panel to
white on Ctrl-C. These follow the manufacturer's precautions, which
`demos/pi-zero/README.md` lists.

## Watching from a desktop

```sh
irl watch <TICKET>
```

Or scan the QR code on the e-paper display with `irl watch --scan`.
