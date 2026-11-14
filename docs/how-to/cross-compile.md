# Cross-compile for aarch64

This page builds `irl` or an example for 64-bit ARM Linux, such as a
Raspberry Pi, on an x86-64 Linux host. The release workflow builds
`irl-linux-aarch64` the same way, see [Install](../getting-started/install.md).

```sh
cargo make cross-sysroot-aarch64                              # once
cargo make cross-build-aarch64 -- -p iroh-live-cli --release
```

The build uses `cargo-zigbuild` against a Debian Bookworm sysroot, so the
binary runs on Raspberry Pi OS Bookworm (glibc 2.36). The sysroot is built
from `.deb` files with `dpkg-deb`, without sudo or a chroot. Everything after
`--` goes to `cargo zigbuild`, and the binaries land in
`target/aarch64-unknown-linux-gnu/release/`.

[cross/README.md](../../cross/README.md) covers the prerequisites, and a
Docker build for hosts without zig. [Raspberry Pi](raspberry-pi.md#cross-compiling)
builds the Pi examples and demos this way.
