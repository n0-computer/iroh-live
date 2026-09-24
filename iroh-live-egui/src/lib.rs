//! Egui integration for `moq-video` rendering.
//!
//! Provides `VideoView`, which draws a `VideoFrames` stream (a player's
//! pictures, a source's preview, anything else that hands one out) and wakes
//! the window when a picture arrives, and the lower-level `FrameView` it is
//! built on, for callers that hand it frames themselves. Both live behind the
//! `wgpu-render` feature (on by default) and need it to actually draw a
//! picture: upstream `moq_video` draws decoded frames through a `wgpu`
//! pipeline (`iroh_live_media::video::render::Renderer`) and offers no other
//! way to read a frame's pixels, so a view built without a real `wgpu` device
//! only ever shows a placeholder.
//!
//! Also provides `create_egui_wgpu_config` (also behind `wgpu-render`), which
//! builds an `egui_wgpu::WgpuConfiguration` that requests zero-copy DMA-BUF
//! import on Linux where the adapter supports it.
//!
//! These names are not doc-linked above: they only exist under
//! `wgpu-render`, and a plain `cargo doc --no-default-features` build fails
//! on a link to an item that feature compiles out.
//!
//! # Example
//!
//! ```no_run
//! use iroh_live_egui::VideoView;
//!
//! # fn draw(
//! #     ctx: &egui::Context,
//! #     ui: &mut egui::Ui,
//! #     player: &iroh_live_media::Player,
//! #     render_state: Option<&iroh_live_egui::egui_wgpu::RenderState>,
//! # ) {
//! let mut view = VideoView::new(ctx, "video", player.video(), render_state);
//! // in the update loop:
//! let (image, frame_ts) = view.render(ui.available_size());
//! ui.add(image);
//! # let _ = frame_ts;
//! # }
//! ```

pub mod overlay;

#[cfg(feature = "wgpu-render")]
use std::{fmt, time::Duration};

#[cfg(feature = "wgpu-render")]
pub use egui_wgpu;
#[cfg(feature = "wgpu-render")]
pub use epaint;
pub use iroh_live_media;
#[cfg(feature = "wgpu-render")]
use iroh_live_media::VideoFrames;

/// Formats a bitrate in bits per second as a human-readable string.
///
/// Returns values like "1.5 Mbps", "320 kbps", "64 bps".
pub fn format_bitrate(bits_per_second: f64) -> String {
    if bits_per_second >= 1_000_000.0 {
        format!("{:.1} Mbps", bits_per_second / 1_000_000.0)
    } else if bits_per_second >= 1_000.0 {
        format!("{:.0} kbps", bits_per_second / 1_000.0)
    } else {
        format!("{:.0} bps", bits_per_second)
    }
}

// ---------------------------------------------------------------------------
// EguiVideoRenderer: draws a moq_video::Frame into an egui-registered texture
// ---------------------------------------------------------------------------

/// The `wgpu` this crate draws through, re-exported from
/// [`iroh_live_media::video::render`] so a caller never has to name a `wgpu`
/// version independently and risk it drifting from the one the renderer
/// actually links.
#[cfg(feature = "wgpu-render")]
pub use iroh_live_media::video::render::wgpu;

/// Draws decoded [`iroh_live_media::video::Frame`]s into a texture registered with
/// egui, on top of [`iroh_live_media::video::render::Renderer`].
///
/// Bound to one `wgpu` device and queue for its lifetime; keep it alive
/// across frames rather than rebuilding it. [`FrameView`] and
/// [`VideoTrackView`] are the usual entry points: reach for this directly
/// only when neither fits (drawing into a texture id you manage yourself).
#[cfg(feature = "wgpu-render")]
pub struct EguiVideoRenderer {
    renderer: iroh_live_media::video::render::Renderer,
    render_state: egui_wgpu::RenderState,
    texture_id: Option<epaint::TextureId>,
    last_size: Option<(u32, u32)>,
}

#[cfg(feature = "wgpu-render")]
impl fmt::Debug for EguiVideoRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EguiVideoRenderer")
            .field("texture_id", &self.texture_id)
            .field("last_size", &self.last_size)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "wgpu-render")]
impl EguiVideoRenderer {
    /// Builds a renderer bound to `render_state`'s device and queue.
    ///
    /// # Errors
    ///
    /// Fails if the underlying `wgpu` pipeline cannot be built (a shader
    /// compile failure, or a resource creation error on the device).
    pub fn new(
        render_state: &egui_wgpu::RenderState,
    ) -> Result<Self, iroh_live_media::video::Error> {
        let renderer = iroh_live_media::video::render::Renderer::new(
            &render_state.device,
            &render_state.queue,
            iroh_live_media::video::render::Config::new(),
        )?;
        Ok(Self {
            renderer,
            render_state: render_state.clone(),
            texture_id: None,
            last_size: None,
        })
    }

    /// Draws `frame` and registers (or updates) its egui texture.
    ///
    /// Takes a plain reference so a caller holding an owned
    /// [`iroh_live_media::video::Frame`] or an `Arc<iroh_live_media::video::Frame>` (as
    /// the publisher's preview does) can pass either: `&Arc<Frame>` derefs to
    /// `&Frame` at the call site.
    ///
    /// # Errors
    ///
    /// Fails if the frame's surface cannot be drawn (an unsupported GPU
    /// format, or a device error).
    pub fn render(
        &mut self,
        frame: &iroh_live_media::video::Frame,
    ) -> Result<(epaint::TextureId, (u32, u32)), iroh_live_media::video::Error> {
        let texture = self.renderer.render(frame)?;
        let view = texture.create_view(&Default::default());
        let dims = (texture.width(), texture.height());

        let device = &self.render_state.device;
        let mut egui_renderer = self.render_state.renderer.write();
        let id = match self.texture_id {
            Some(id) => {
                egui_renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    &view,
                    wgpu::FilterMode::Linear,
                    id,
                );
                id
            }
            None => {
                let id =
                    egui_renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear);
                self.texture_id = Some(id);
                id
            }
        };

        self.last_size = Some(dims);
        Ok((id, dims))
    }

    /// Returns the last rendered texture id and its pixel size, if a frame
    /// has been drawn yet.
    pub fn last_texture(&self) -> Option<(epaint::TextureId, (u32, u32))> {
        self.texture_id.zip(self.last_size)
    }
}

#[cfg(feature = "wgpu-render")]
impl Drop for EguiVideoRenderer {
    /// Gives the registration back, which nothing else will.
    ///
    /// `register_native_texture` puts an entry in the egui renderer's own map,
    /// keyed by an id this owns and holding a bind group built from the
    /// texture view. Dropping this renderer drops the texture, so the entry
    /// then describes a resource that is gone, and it stays there for the life
    /// of the render state: a room whose tiles come and go accumulates one per
    /// tile it ever drew.
    fn drop(&mut self) {
        if let Some(id) = self.texture_id.take() {
            self.render_state.renderer.write().free_texture(&id);
        }
    }
}

// ---------------------------------------------------------------------------
// FrameView: a placeholder-or-drawn texture for one raw frame stream
// ---------------------------------------------------------------------------

/// Shows one video stream as an egui texture: either the most recently drawn
/// frame, or a black placeholder before the first one arrives.
///
/// The low-level building block: it knows nothing about where frames come
/// from, only how to draw the ones it is handed. [`VideoView`] adds the
/// waking and the reading over a [`VideoFrames`] stream.
#[cfg(feature = "wgpu-render")]
pub struct FrameView {
    renderer: Option<EguiVideoRenderer>,
    placeholder: egui::TextureHandle,
}

#[cfg(feature = "wgpu-render")]
impl fmt::Debug for FrameView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameView")
            .field("is_wgpu", &self.is_wgpu())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "wgpu-render")]
impl FrameView {
    /// Creates a view with no renderer. [`render_frame`](Self::render_frame)
    /// only ever shows the placeholder: without a `wgpu` device there is no
    /// way to read a frame's pixels, since
    /// [`Surface`](iroh_live_media::video::Surface)'s conversions consume it and
    /// [`Frame`](iroh_live_media::video::Frame) is not `Clone`.
    pub fn new(ctx: &egui::Context, name: &str) -> Self {
        Self::new_wgpu(ctx, name, None)
    }

    /// Creates a view that draws through `render_state`, if given.
    pub fn new_wgpu(
        ctx: &egui::Context,
        name: &str,
        render_state: Option<&egui_wgpu::RenderState>,
    ) -> Self {
        let renderer = render_state.and_then(|rs| {
            EguiVideoRenderer::new(rs)
                .inspect_err(|err| tracing::warn!(error = %err, "wgpu video renderer init failed"))
                .ok()
        });
        let placeholder = ctx.load_texture(
            name,
            egui::ColorImage::filled([1, 1], egui::Color32::BLACK),
            Default::default(),
        );
        Self {
            renderer,
            placeholder,
        }
    }

    /// Returns whether this view draws through `wgpu` (as opposed to only
    /// ever showing the placeholder).
    pub fn is_wgpu(&self) -> bool {
        self.renderer.is_some()
    }

    /// Draws `frame`, replacing whatever this view previously showed.
    ///
    /// A no-op (besides a warning) if this view has no renderer.
    pub fn render_frame(&mut self, frame: &iroh_live_media::video::Frame) {
        let Some(renderer) = &mut self.renderer else {
            tracing::warn!("frame dropped: view has no wgpu renderer to draw it with");
            return;
        };
        if let Err(err) = renderer.render(frame) {
            tracing::warn!(error = %err, "video render failed");
        }
    }

    /// Returns the current texture id and its pixel size.
    pub fn texture_info(&self) -> Option<(egui::TextureId, egui::Vec2)> {
        self.renderer
            .as_ref()
            .and_then(EguiVideoRenderer::last_texture)
            .map(|(id, (w, h))| (id, egui::vec2(w as f32, h as f32)))
    }

    /// Returns an [`egui::Image`] for the current texture, suitable for `ui.add()`.
    pub fn image(&self) -> egui::Image<'_> {
        match self.texture_info() {
            Some((id, size)) => {
                egui::Image::from_texture(egui::load::SizedTexture::new(id, size)).shrink_to_fit()
            }
            None => egui::Image::from_texture(&self.placeholder).shrink_to_fit(),
        }
    }
}

// ---------------------------------------------------------------------------
// VideoView: FrameView + a VideoFrames stream
// ---------------------------------------------------------------------------

/// Draws a [`VideoFrames`] stream into an egui UI.
///
/// Wakes the window when a picture arrives and draws the newest one on the
/// next pass, whether the frames come from a player, a local preview, or a
/// scanner.
#[cfg(feature = "wgpu-render")]
pub struct VideoView {
    frames: VideoFrames,
    frame_view: FrameView,
    /// The window to wake, kept so [`set_frames`](Self::set_frames) can build
    /// a waker for the replacement.
    ctx: egui::Context,
    /// Wakes the window when a picture lands. Dropping it stops the waking.
    _wake: n0_future::task::AbortOnDropHandle<()>,
}

#[cfg(feature = "wgpu-render")]
impl fmt::Debug for VideoView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoView")
            .field("frame_view", &self.frame_view)
            .finish_non_exhaustive()
    }
}

/// Asks the window to draw whenever a picture arrives.
///
/// The player does not hand a picture over until its playout clock says it is
/// due, so by the time one arrives it should be on screen. A window that
/// repaints on a timer presents each picture at the next tick of its own clock
/// rather than when it was due, which on a 30fps stream sampled every 16ms is
/// up to half a frame of added latency and a spacing that wobbles between one
/// tick and two: judder on an otherwise well paced stream.
///
/// The waker reads through a handle of its own, so it never takes a picture
/// from the drawing pass.
#[cfg(feature = "wgpu-render")]
fn wake_on_frame(
    ctx: &egui::Context,
    frames: &VideoFrames,
) -> n0_future::task::AbortOnDropHandle<()> {
    let ctx = ctx.clone();
    let mut frames = frames.clone();
    n0_future::task::AbortOnDropHandle::new(n0_future::task::spawn(async move {
        while frames.next().await.is_some() {
            ctx.request_repaint();
        }
    }))
}

#[cfg(feature = "wgpu-render")]
impl VideoView {
    /// Creates a view of `frames` that draws through `render_state`, if given.
    ///
    /// `name` names the placeholder texture. Must be called within a Tokio
    /// runtime, which the waker runs on.
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        frames: VideoFrames,
        render_state: Option<&egui_wgpu::RenderState>,
    ) -> Self {
        Self {
            _wake: wake_on_frame(ctx, &frames),
            frames,
            frame_view: FrameView::new_wgpu(ctx, name, render_state),
            ctx: ctx.clone(),
        }
    }

    /// Returns the frames this view draws.
    pub fn frames(&self) -> &VideoFrames {
        &self.frames
    }

    /// Replaces the frames this view draws, keeping the last picture up until
    /// the new stream has one.
    pub fn set_frames(&mut self, frames: VideoFrames) {
        self._wake = wake_on_frame(&self.ctx, &frames);
        self.frames = frames;
    }

    /// Returns whether this view draws through `wgpu`.
    pub fn is_wgpu(&self) -> bool {
        self.frame_view.is_wgpu()
    }

    /// Draws the newest frame if one arrived since the last call, and returns
    /// `(image, frame_timestamp)`.
    ///
    /// The timestamp is the new frame's presentation time, or `None` when the
    /// picture did not change.
    pub fn render(&mut self, _available_size: egui::Vec2) -> (egui::Image<'_>, Option<Duration>) {
        let frame_ts = self.frames.try_next().map(|frame| {
            self.frame_view.render_frame(&frame);
            Duration::from_micros(frame.timestamp.as_micros() as u64)
        });
        (self.frame_view.image(), frame_ts)
    }

    /// Returns the image for whatever frame was drawn last.
    pub fn image(&self) -> egui::Image<'_> {
        self.frame_view.image()
    }
}

// ---------------------------------------------------------------------------
// create_egui_wgpu_config: a wgpu device tuned for video rendering
// ---------------------------------------------------------------------------

/// Creates an [`egui_wgpu::WgpuConfiguration`] tuned for video rendering.
///
/// On Linux, builds a Vulkan device and requests
/// [`wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`] when the adapter
/// supports it, which lets [`iroh_live_media::video::render::Renderer`] import
/// PipeWire DMA-BUFs without a CPU round trip. Every other platform, and any
/// Linux adapter that lacks the feature, gets [`adapter_limits_config`]: the
/// renderer still draws every frame correctly through its CPU-upload
/// fallback, just without the zero-copy path.
#[cfg(feature = "wgpu-render")]
pub fn create_egui_wgpu_config() -> egui_wgpu::WgpuConfiguration {
    #[cfg(target_os = "linux")]
    {
        create_egui_wgpu_config_dmabuf()
    }
    #[cfg(not(target_os = "linux"))]
    {
        adapter_limits_config()
    }
}

/// Returns a device descriptor that asks for exactly what `adapter` reports.
///
/// egui asks for [`wgpu::Limits::default()`] on anything that is not the GL
/// backend, and those are desktop limits: eight colour attachments, for one.
/// A Raspberry Pi 4 has a conformant Vulkan driver that allows four, so the
/// device request fails and eframe exits before a window ever opens. An
/// adapter's own limits are the ones it is guaranteed to grant, and video
/// playback wants nothing beyond them: [`iroh_live_media::video::render::Renderer`]
/// draws one triangle with three sampled textures.
#[cfg(feature = "wgpu-render")]
fn device_descriptor(
    adapter: &wgpu::Adapter,
    required_features: wgpu::Features,
) -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor {
        label: Some("iroh-live-egui video device"),
        required_features,
        required_limits: adapter.limits(),
        ..Default::default()
    }
}

/// Returns egui's configuration with the device limits taken from the adapter.
///
/// This is the fallback for every path that does not build a device itself,
/// and it exists for the same reason [`device_descriptor`] does: egui's
/// default limits are not a subset of what every conformant driver grants.
#[cfg(feature = "wgpu-render")]
fn adapter_limits_config() -> egui_wgpu::WgpuConfiguration {
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(egui_wgpu::WgpuSetupCreateNew {
            device_descriptor: std::sync::Arc::new(|adapter| {
                device_descriptor(adapter, wgpu::Features::empty())
            }),
            ..egui_wgpu::WgpuSetupCreateNew::without_display_handle()
        }),
        ..Default::default()
    }
}

#[cfg(all(target_os = "linux", feature = "wgpu-render"))]
fn create_egui_wgpu_config_dmabuf() -> egui_wgpu::WgpuConfiguration {
    // `WGPU_BACKEND` is how one machine gets compared against itself, and a
    // board with both a Vulkan and a GL adapter is exactly where that
    // comparison is worth making. This path asks for Vulkan by name, because
    // DMA-BUF import is a Vulkan extension, so it has to stand aside when
    // somebody has asked for anything else: egui's own setup reads the same
    // variable, and `adapter_limits_config` keeps it.
    if let Some(requested) = wgpu::Backends::from_env()
        && !requested.contains(wgpu::Backends::VULKAN)
    {
        tracing::info!(
            ?requested,
            "WGPU_BACKEND excludes Vulkan, so the DMA-BUF import path is not used",
        );
        return adapter_limits_config();
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    })) {
        Ok(adapter) => adapter,
        Err(err) => {
            tracing::warn!(error = %err, "no Vulkan adapter available, letting egui pick one");
            return adapter_limits_config();
        }
    };

    let dma_buf = wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
    let required_features = if adapter.features().contains(dma_buf) {
        dma_buf
    } else {
        tracing::debug!(
            "adapter has no DMA-BUF external memory support; \
             video rendering will use the CPU-upload fallback"
        );
        wgpu::Features::empty()
    };

    let (device, queue) = match pollster::block_on(
        adapter.request_device(&device_descriptor(&adapter, required_features)),
    ) {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!(error = %err, "wgpu device request failed, letting egui build one");
            return adapter_limits_config();
        }
    };

    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
            instance,
            adapter,
            device,
            queue,
        }),
        ..Default::default()
    }
}
