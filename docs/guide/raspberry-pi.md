# Raspberry Pi

Two demos target the Pi. `demos/pi-zero-minimal` is a 35-line publisher and
nothing else. `demos/pi-zero` adds an e-paper QR display and a watch mode that
renders with GLES2, either to a window or straight to HDMI. Both are tested on a
Pi Zero 2 W and should work on a Pi 4 or 5.

The `irl` CLI reaches the same camera, so publishing from a Pi no longer needs a
demo binary.

## Capture is rpicam-vid

On Raspberry Pi OS the CSI camera is only reachable through the libcamera stack.
`/dev/video0` hands back raw Bayer data from the Unicam sensor, which is unusable
without the ISP. `rpicam-vid` drives that pipeline and the Pi's hardware H.264
encoder, so the cheapest thing a Pi Zero can do is read the Annex-B bytes it
writes to stdout and publish them unchanged.

`moq_media::rpicam::open(config)` does exactly that. It spawns the process, reads
its stdout, and returns a `VideoSource::AnnexB` that the publisher hands to
`moq_mux::codec::h264`, which derives the catalog entry from the stream's own SPS.
The child is killed when the source drops, so the camera stops with the
broadcast. This avoids both the raw YUV pipe, about 10 MB/s at 640x360, and a
second encode. The Pi never software-encodes.

The feature is `rpicam`, Linux only, and `rpicam-vid` has to be on `PATH` at
runtime. It ships with Raspberry Pi OS.

From the CLI that is `--video rpicam`, which gets the ticket, the QR code, and
the relay flag every other source gets:

```sh
cargo build -p iroh-live-cli --features rpicam
irl publish --video rpicam --width 640 --height 360 --fps 30
```

`irl devices` lists the sensors `rpicam-vid` reports, and says so when the
binary is not installed.

Because the bytes arrive encoded, `--codec` and `--encoder` have nothing to act
on, and `irl publish` refuses them rather than ignoring them. `--renditions`
takes at most one rung, since a stream we did not encode cannot be produced
again at a second size. `--width`, `--height`, `--fps`, and `--bitrate` do reach
the camera: they become `rpicam-vid` arguments. `--preview` is refused for the
reason a file source is, that no raw picture ever reaches us.

Raw libcamera capture, which opened the camera as a frame source rather than a
subprocess, went with the in-house codec stack and has no upstream replacement.
Publishing does not need it, because `rpicam-vid` covers it.

## The V4L2 hardware codecs

The Pi's other codec path is the VideoCore stateful memory-to-memory block,
which the kernel exposes as a device node and which most ARM SoCs have an
equivalent of. Upstream carries an encoder and a decoder for it again, and the
`v4l2` feature reaches them:

```sh
cargo build -p iroh-live-cli --features v4l2
irl publish --video cam --encoder v4l2
```

The decoder needs no flag: it joins automatic backend selection, and a host with
no M2M node fails at open so selection falls through to openh264.

**Neither backend has been run on real hardware**, upstream or here. Making them
reachable is not a claim that they work, and a Pi that publishes through
`rpicam-vid` never opens either one. If probing picks the wrong node,
`MOQ_V4L2_ENCODER` and `MOQ_V4L2_DECODER` name one directly.

The `codec-test` subcommand that used to exercise this path on device is still
gone.

## Pi setup

Assume a fresh Raspberry Pi OS Bookworm, 64-bit, with SSH enabled.

Enable the camera through `sudo raspi-config`, under Interface Options, and
reboot. `rpicam-hello --timeout 2000` confirms it works.

For the e-paper HAT, enable SPI the same way. The HAT plugs onto the 40-pin
header with no extra wiring. If it is absent or SPI is off, the binary still runs
and prints the ticket to the terminal.

The binary needs `/dev/video*`, and with the HAT also `/dev/spidev*` and
`/dev/gpiochip0`:

```sh
sudo usermod -aG video,spi,gpio $USER
```

Log out and back in for that to take effect.

`gpu_mem` sized the VideoCore memory the M2M codec uses. It does not matter for
a publisher, which goes through `rpicam-vid`, and it is worth knowing about only
if you try the `v4l2` backends.

## Cross-compiling

Building on a Pi Zero 2 W works and is slow. Cross-compiling uses
`cargo-zigbuild` against a Debian Bookworm sysroot, assembled from `.deb` files
with `dpkg-deb` alone: no sudo, no chroot, one or two minutes.

```sh
cargo make cross-sysroot-aarch64                              # once
cargo make cross-build-aarch64 -- -p pi-zero-minimal --release
cargo make cross-build-aarch64 -- -p pi-zero-demo --release
```

Everything after `--` goes to `cargo zigbuild`. Binaries land in
`target/aarch64-unknown-linux-gnu/release/`. `cross/README.md` covers the
prerequisites and a Docker path for hosts that cannot install zig.

Deploy with `scp`, or `cargo make cross-deploy` from `demos/pi-zero`, which
builds, strips, and copies to `$PI_HOST` (default `livepizero`).

## pi-zero-minimal

```sh
./pi-zero-minimal
```

No flags. It publishes 640x360 at 30 fps under the path `pi-cam` and prints a
ticket. Pin `IROH_SECRET` if you want the ticket to survive a restart; the first
run logs the value to reuse.

## pi-zero-demo

Four subcommands.

`publish` streams the camera and optionally shows the ticket as a QR code on the
e-paper HAT. Flags: `--epaper`, `--relay <ENDPOINT_ID>`, `--name` (default
`pi-zero`), `--width` (640), `--height` (360), `--fps` (30), `--bitrate`
(500000). The ticket is printed to the terminal either way.

`watch <TICKET>` subscribes and renders. Without `--fb` it opens a window through
glutin and winit, which needs the `windowed` feature (on by default) and
`--fullscreen` if you want it borderless. With `--fb` it takes over the console
and renders through DRM/KMS, GBM, and EGL straight to HDMI, with no window system
at all. `--endpoint-id` plus `--name` works in place of a ticket.

`fb-demo` renders a generated pattern to HDMI with no network and no camera,
which isolates the display path when something is wrong.

`epaper-demo` walks the HAT through a checkerboard, a QR code, and a clear, one
Enter press at a time.

## Rendering on the Pi

The Pi Zero has no Vulkan and no wgpu, so `moq_video::render` cannot draw there.
`demos/pi-zero/src/gles.rs` is a GLES2 renderer over `glow` that can, with two
upload routes chosen from the decoder's surface: I420 goes up as three
`LUMINANCE` textures and is converted with a BT.601 limited-range fragment
shader, which is the openh264 output; packed RGBA goes up as a single texture.

It lives in the demo rather than a library crate because it had exactly one
caller. There is no zero-copy path: the DMA-BUF variant went with the crate the
renderer came from.

## E-paper

`epaper.rs` and `epd_v4.rs` are a hand-written driver for the Waveshare 2.13"
Touch e-Paper HAT, revision V4 (SSD1680), because `epd-waveshare` covers V2 and
V3 only and the V4 refresh command differs. The driver uses full refreshes only,
sleeps the panel after every update, re-displays every 12 hours to satisfy the
datasheet's 24-hour requirement, and clears to white on Ctrl-C. All of that
follows the manufacturer's precautions; `demos/pi-zero/README.md` lists them
individually.

## Watching from a desktop

```sh
irl watch <TICKET>
```

Or scan the QR code off the e-paper display.
