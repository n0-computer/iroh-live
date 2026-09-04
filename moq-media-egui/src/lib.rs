//! Egui integration for `moq-video` rendering.
//!
//! Provides `VideoTrackView`, which wraps a `VideoTrack` with frame polling
//! and drawing, and the lower-level `FrameView` it is built on, for callers
//! that only have a raw frame stream (a publisher's local preview, for
//! instance) rather than a full `VideoTrack`. Both live behind the
//! `wgpu-render` feature (on by default) and need it to actually draw a
//! picture: upstream `moq_video` draws decoded frames through a `wgpu`
//! pipeline (`moq_media::video::render::Renderer`) and offers no other way to
//! read a frame's pixels, so a view built without a real `wgpu` device only
//! ever shows a placeholder.
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
//! use moq_media_egui::VideoTrackView;
//!
//! # fn draw(
//! #     ctx: &egui::Context,
//! #     ui: &mut egui::Ui,
//! #     track: moq_media::subscribe::VideoTrack,
//! #     render_state: Option<&moq_media_egui::egui_wgpu::RenderState>,
//! # ) {
//! let mut view = VideoTrackView::new_wgpu(ctx, "video", track, render_state);
//! // in the update loop:
//! let (image, frame_ts) = view.render(ctx, ui.available_size());
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
pub use moq_media;
#[cfg(feature = "wgpu-render")]
use moq_media::subscribe::VideoTrack;

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
/// [`moq_media::video::render`] so a caller never has to name a `wgpu`
/// version independently and risk it drifting from the one the renderer
/// actually links.
#[cfg(feature = "wgpu-render")]
pub use moq_media::video::render::wgpu;

/// Draws decoded [`moq_media::video::Frame`]s into a texture registered with
/// egui, on top of [`moq_media::video::render::Renderer`].
///
/// Bound to one `wgpu` device and queue for its lifetime; keep it alive
/// across frames rather than rebuilding it. [`FrameView`] and
/// [`VideoTrackView`] are the usual entry points: reach for this directly
/// only when neither fits (drawing into a texture id you manage yourself).
#[cfg(feature = "wgpu-render")]
pub struct EguiVideoRenderer {
    renderer: moq_media::video::render::Renderer,
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
    pub fn new(render_state: &egui_wgpu::RenderState) -> Result<Self, moq_media::video::Error> {
        let renderer = moq_media::video::render::Renderer::new(
            &render_state.device,
            &render_state.queue,
            moq_media::video::render::Config::new(),
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
    /// [`moq_media::video::Frame`] or an `Arc<moq_media::video::Frame>` (as
    /// the publisher's preview does) can pass either: `&Arc<Frame>` derefs to
    /// `&Frame` at the call site.
    ///
    /// # Errors
    ///
    /// Fails if the frame's surface cannot be drawn (an unsupported GPU
    /// format, or a device error).
    pub fn render(
        &mut self,
        frame: &moq_media::video::Frame,
    ) -> Result<(epaint::TextureId, (u32, u32)), moq_media::video::Error> {
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

// ---------------------------------------------------------------------------
// FrameView: a placeholder-or-drawn texture for one raw frame stream
// ---------------------------------------------------------------------------

/// Shows one video stream as an egui texture: either the most recently drawn
/// frame, or a black placeholder before the first one arrives.
///
/// The low-level building block: it knows nothing about where frames come
/// from, only how to draw the ones it is handed. [`VideoTrackView`] adds the
/// polling loop over a [`VideoTrack`]; a publisher's local preview (an
/// `Arc<FrameReceiver<Arc<Frame>>>`, not a [`VideoTrack`]) polls its own
/// receiver and feeds this directly.
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
    /// [`Surface`](moq_media::video::Surface)'s conversions consume it and
    /// [`Frame`](moq_media::video::Frame) is not `Clone`.
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
    pub fn render_frame(&mut self, frame: &moq_media::video::Frame) {
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
// VideoTrackView: FrameView + VideoTrack polling
// ---------------------------------------------------------------------------

/// Renders a [`VideoTrack`] into an egui UI.
///
/// Polls the track for the newest decoded frame on every
/// [`render`](Self::render) call and draws it through a [`FrameView`].
#[cfg(feature = "wgpu-render")]
pub struct VideoTrackView {
    track: VideoTrack,
    frame_view: FrameView,
    /// Wakes the window when a picture lands. Dropping it stops the waking.
    _wake: n0_future::task::AbortOnDropHandle<()>,
}

#[cfg(feature = "wgpu-render")]
impl fmt::Debug for VideoTrackView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoTrackView")
            .field("frame_view", &self.frame_view)
            .finish_non_exhaustive()
    }
}

/// Asks the window to draw whenever a picture arrives.
///
/// The decoder does not hand a picture over until the playout clock says it is
/// due, so by the time one reaches the slot it should be on screen. What put it
/// off was the drawing loop: a window that repaints on a timer presents each
/// picture at the next tick of its own clock rather than when the picture was
/// due, which on a 30fps stream sampled every 16ms is up to half a frame of
/// added latency and a spacing that wobbles between one tick and two. That is
/// what a viewer sees as judder on an otherwise well paced stream.
///
/// Waiting here rather than taking: the task only wakes the window, and the
/// drawing pass is what takes the picture.
#[cfg(feature = "wgpu-render")]
fn wake_on_frame(
    ctx: &egui::Context,
    track: &VideoTrack,
) -> n0_future::task::AbortOnDropHandle<()> {
    let ctx = ctx.clone();
    let slot = track.frame_slot();
    n0_future::task::AbortOnDropHandle::new(n0_future::task::spawn(async move {
        loop {
            slot.arrived().await;
            if slot.is_closed() && !slot.has_value() {
                return;
            }
            ctx.request_repaint();
        }
    }))
}

#[cfg(feature = "wgpu-render")]
impl VideoTrackView {
    /// Creates a view with no renderer; see [`FrameView::new`].
    pub fn new(ctx: &egui::Context, name: &str, track: VideoTrack) -> Self {
        Self {
            _wake: wake_on_frame(ctx, &track),
            track,
            frame_view: FrameView::new(ctx, name),
        }
    }

    /// Creates a view that draws through `render_state`, if given.
    pub fn new_wgpu(
        ctx: &egui::Context,
        name: &str,
        track: VideoTrack,
        render_state: Option<&egui_wgpu::RenderState>,
    ) -> Self {
        Self {
            _wake: wake_on_frame(ctx, &track),
            track,
            frame_view: FrameView::new_wgpu(ctx, name, render_state),
        }
    }

    /// Returns a reference to the underlying track.
    pub fn track(&self) -> &VideoTrack {
        &self.track
    }

    /// Returns a mutable reference to the underlying track.
    pub fn track_mut(&mut self) -> &mut VideoTrack {
        &mut self.track
    }

    /// Replaces the underlying track.
    pub fn set_track(&mut self, track: VideoTrack) {
        self.track = track;
    }

    /// Returns whether this view draws through `wgpu`.
    pub fn is_wgpu(&self) -> bool {
        self.frame_view.is_wgpu()
    }

    /// Draws the newest available frame and returns `(image, frame_timestamp)`.
    ///
    /// The returned timestamp is the decoded frame's presentation time, or
    /// `None` if no new frame arrived since the last call. Requests a
    /// repaint on `ctx` when a new frame lands, so the picture advances
    /// without waiting for the next input event.
    pub fn render(
        &mut self,
        ctx: &egui::Context,
        _available_size: egui::Vec2,
    ) -> (egui::Image<'_>, Option<Duration>) {
        let frame_ts = self.track.take().map(|frame| {
            let ts = Duration::from_micros(frame.timestamp.as_micros() as u64);
            self.frame_view.render_frame(&frame);
            ts
        });
        if frame_ts.is_some() {
            ctx.request_repaint();
        }
        (self.frame_view.image(), frame_ts)
    }
}

// ---------------------------------------------------------------------------
// create_egui_wgpu_config: a wgpu device tuned for video rendering
// ---------------------------------------------------------------------------

/// Creates an [`egui_wgpu::WgpuConfiguration`] tuned for video rendering.
///
/// On Linux, builds a Vulkan device and requests
/// [`wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`] when the adapter
/// supports it, which lets [`moq_media::video::render::Renderer`] import
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
/// playback wants nothing beyond them: [`moq_media::video::render::Renderer`]
/// draws one triangle with three sampled textures.
#[cfg(feature = "wgpu-render")]
fn device_descriptor(
    adapter: &wgpu::Adapter,
    required_features: wgpu::Features,
) -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor {
        label: Some("moq-media-egui video device"),
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
