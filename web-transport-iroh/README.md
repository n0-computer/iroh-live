[![crates.io](https://img.shields.io/crates/v/web-transport-quinn)](https://crates.io/crates/web-transport-quinn)
[![docs.rs](https://img.shields.io/docsrs/web-transport-quinn)](https://docs.rs/web-transport-quinn)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-iiroh

A wrapper around the Iroh API, implementing the [`web-transport-trait`](https://github.com/kixelated/web-transport/tree/main/web-transport-trait) traits.

Note that this does *not* actually implement WebTransport for iroh. Instead, it implements the WebTransport traits on raw iroh QUIC connection. Thus, you can use an iroh connection wherever the `web-transport-trait` traits are expected (i.e. in hang). But there's no actual HTTP/3 WebTransport session being established, we just use the raw iroh QUIC connection directly.
