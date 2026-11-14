# Watch a broadcast and render it

This page plays a broadcast with sound and draws its frames. The
[library quickstart](../getting-started/library-quickstart.md) has the
complete viewer program that this page builds on.

## With `irl`

```sh
irl watch <TICKET>
irl watch --scan                      # read the ticket from a QR code
irl watch <TICKET> --latency smooth   # more buffer for Wi-Fi or mobile links
```

The window shows the stats overlay along its bottom edge, and has controls to
pin a rendition or change the decoder. The [CLI reference](../reference/cli.md#irl-watch)
lists every flag.

## Play the audio

A player subscribes to audio only when `PlayerConfig::audio` asks for it.
`AudioMode::Play` mixes it into a `playback::Engine`, which needs the
`playback` feature:

```rust,no_run
use iroh_live::{
    Subscription,
    media::{AudioMode, Player, PlayerConfig, audio::playback::Engine, remote_broadcast},
};

async fn play(subscription: &Subscription) -> Result<Player, Box<dyn std::error::Error>> {
    // One engine per output device. Every player that uses it is mixed.
    let engine = Engine::open(Default::default()).await?;
    let config = PlayerConfig::default().with_audio(AudioMode::Play(engine));
    let player = remote_broadcast(subscription).play(config)?;
    Ok(player)
}
```

`AudioMode::Decode` decodes the audio without a speaker, for a headless
consumer. `AudioMode::Off`, the default, does not subscribe to audio at all.

## Draw the frames

`Player::video_frames()` returns a `VideoFrames` stream of decoded
`moq_video::Frame` values. It holds only the newest frame, so a renderer that
falls behind skips to the newest picture.

On the desktop, frames are drawn through wgpu, which needs the `render`
feature. `moq_video::render::Renderer::new(device, queue, config)` binds a
renderer to a wgpu device and queue, and `render(&frame)` returns a
`wgpu::Texture` to present, hand to a UI toolkit, or copy back. Backend
selection, colour handling and zero-copy import are documented in
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video).

Two points matter here:

- The renderer fixes the wgpu version. `iroh_live::media::video::render::wgpu`
  re-exports the wgpu it links. A texture from another wgpu major version is a
  different type.
- On Linux, request `wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF` when you
  create the device. Without it, frames that arrive as DMA-BUFs, such as
  hardware-decoded video and PipeWire screen capture, are copied through the
  CPU.

### egui

The `moq-ext-egui` crate draws frames in egui. `VideoView` draws a
`VideoFrames` stream:

```rust,ignore
use moq_ext_egui::VideoView;

let mut view = VideoView::new(ctx, "video", player.video_frames(), render_state);
// In the update loop:
ui.add(view.render());
```

`render_state` is an `Option<&egui_wgpu::RenderState>`, and without one the
view shows a black placeholder. The view wakes the window when a frame
arrives. `set_frames` switches to another stream and keeps the last picture
until the new stream delivers one. `VideoView::new` must run inside a Tokio
runtime.

Pass `moq_ext_egui::egui_wgpu_config()` to eframe. On Linux it selects Vulkan
and enables `VULKAN_EXTERNAL_MEMORY_DMA_BUF` when the adapter supports it,
which eframe does not do on its own.

`FrameView` draws frames that you hand it with `render_frame`, and
`overlay::DebugOverlay` draws the stats bar that `irl watch` shows.

### Other toolkits

A toolkit that shares a wgpu device can draw the renderer's texture directly.
Otherwise, `moq_video::Surface::to_rgba` converts a frame to RGBA8 pixels on
the CPU. No library crate has a GLES renderer. The Pi demo has one in
`demos/pi-zero/src/gles.rs`, see [Raspberry Pi](raspberry-pi.md).

## Follow the player

- `Player::status()` watches the rendition on screen, the decoder, and why
  the last switch failed.
- `Player::stats()` returns frame rate, decode time, latency and the link's
  round trip and loss, cheap enough for every redraw.
- `RemoteBroadcast::closed()` resolves when the broadcast ends.

## Where to go next

- [Tune quality and latency](tune.md) pins a rendition and sets the latency.
- [Subscribing](../architecture/subscribe.md) explains decoding and
  rendition switches.
