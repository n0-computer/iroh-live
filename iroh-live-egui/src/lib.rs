//! Video views for egui, on top of [`iroh_live_media`].
//!
//! The views draw decoded frames through the `wgpu` renderer in
//! `iroh_live_media`. They need the `wgpu-render` feature, which is on by
//! default, and an `egui_wgpu::RenderState`. A view without a render state
//! shows a black placeholder.
//!
//! This draws a player's video:
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
//! ui.add(view.render());
//! # }
//! ```
//!
//! The main items are:
//!
//! - `VideoView` draws a `VideoFrames` stream and wakes the window when a
//!   frame arrives.
//! - `FrameView` draws frames you hand it.
//! - `create_egui_wgpu_config` builds the `wgpu` setup for eframe, with
//!   DMA-BUF import on Linux.
//! - [`overlay::DebugOverlay`] paints playback or publish stats over a video.

// The `wgpu-render` items above are in plain backticks: an intra-doc link to
// them breaks `cargo doc --no-default-features`.

pub mod overlay;

#[cfg(feature = "wgpu-render")]
pub use egui_wgpu;
#[cfg(feature = "wgpu-render")]
pub use epaint;
pub use iroh_live_media;
#[cfg(feature = "wgpu-render")]
use iroh_live_media::VideoFrames;

/// Formats a bitrate as `1.5 Mbps`, `320 kbps` or `64 bps`.
pub fn format_bitrate(rate: iroh_live_media::Bitrate) -> String {
    let bps = rate.as_bps() as f64;
    if bps >= 1_000_000.0 {
        format!("{:.1} Mbps", bps / 1_000_000.0)
    } else if bps >= 1_000.0 {
        format!("{:.0} kbps", bps / 1_000.0)
    } else {
        format!("{bps:.0} bps")
    }
}

/// The `wgpu` crate the renderer links, re-exported from
/// [`iroh_live_media::video::render`].
///
/// Use it to name `wgpu` types, so they always match the renderer's version.
#[cfg(feature = "wgpu-render")]
pub use iroh_live_media::video::render::wgpu;

/// Renderer that draws decoded [`iroh_live_media::video::Frame`]s into an egui texture.
///
/// It is bound to one `wgpu` device and queue. Keep it alive across frames.
/// Most callers want [`FrameView`] or [`VideoView`] instead.
#[cfg(feature = "wgpu-render")]
#[derive(derive_more::Debug)]
pub struct EguiVideoRenderer {
    #[debug(skip)]
    renderer: iroh_live_media::video::render::Renderer,
    #[debug(skip)]
    render_state: egui_wgpu::RenderState,
    /// The registered texture and its size, once a frame is drawn.
    texture: Option<(epaint::TextureId, (u32, u32))>,
}

#[cfg(feature = "wgpu-render")]
impl EguiVideoRenderer {
    /// Creates a renderer bound to `render_state`'s device and queue.
    ///
    /// Fails if the `wgpu` pipeline cannot be built on the device.
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
            texture: None,
        })
    }

    /// Draws `frame` and registers or updates its egui texture.
    ///
    /// Returns the texture id and its size in pixels. Fails if the frame
    /// cannot be drawn, for example on an unsupported GPU format.
    pub fn render(
        &mut self,
        frame: &iroh_live_media::video::Frame,
    ) -> Result<(epaint::TextureId, (u32, u32)), iroh_live_media::video::Error> {
        let texture = self.renderer.render(frame)?;
        let view = texture.create_view(&Default::default());
        let dims = (texture.width(), texture.height());

        let device = &self.render_state.device;
        let mut egui_renderer = self.render_state.renderer.write();
        let id = match self.texture {
            Some((id, _)) => {
                egui_renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    &view,
                    wgpu::FilterMode::Linear,
                    id,
                );
                id
            }
            None => egui_renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear),
        };
        self.texture = Some((id, dims));
        Ok((id, dims))
    }

    /// Returns the last drawn texture id and its size in pixels.
    ///
    /// Returns `None` before the first frame.
    pub fn last_texture(&self) -> Option<(epaint::TextureId, (u32, u32))> {
        self.texture
    }
}

#[cfg(feature = "wgpu-render")]
impl Drop for EguiVideoRenderer {
    /// Frees the texture registered with egui.
    ///
    /// egui keeps a native texture registration until it is freed. Without
    /// this, every dropped view would leave a stale entry in the render state.
    fn drop(&mut self) {
        if let Some((id, _)) = self.texture.take() {
            self.render_state.renderer.write().free_texture(&id);
        }
    }
}

/// View that draws the frames you hand it.
///
/// It shows the last drawn frame, or a black placeholder before the first.
/// [`VideoView`] builds on it and reads frames from a [`VideoFrames`] stream.
#[cfg(feature = "wgpu-render")]
#[derive(derive_more::Debug)]
pub struct FrameView {
    renderer: Option<EguiVideoRenderer>,
    #[debug(skip)]
    placeholder: egui::TextureHandle,
}

#[cfg(feature = "wgpu-render")]
impl FrameView {
    /// Creates a view that draws through `render_state`.
    ///
    /// `name` names the placeholder texture. If `render_state` is `None` or the
    /// renderer fails to build, the view only shows the placeholder.
    pub fn new(
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

    /// Draws `frame`, replacing what the view showed before.
    ///
    /// Logs a warning and keeps the old picture if drawing fails or the view
    /// has no renderer.
    pub fn render_frame(&mut self, frame: &iroh_live_media::video::Frame) {
        let Some(renderer) = &mut self.renderer else {
            tracing::warn!("frame dropped: view has no wgpu renderer to draw it with");
            return;
        };
        if let Err(err) = renderer.render(frame) {
            tracing::warn!(error = %err, "video render failed");
        }
    }

    /// Returns the current texture id and its size in pixels.
    ///
    /// Returns `None` until a frame is drawn.
    pub fn texture_info(&self) -> Option<(egui::TextureId, egui::Vec2)> {
        self.renderer
            .as_ref()
            .and_then(EguiVideoRenderer::last_texture)
            .map(|(id, (w, h))| (id, egui::vec2(w as f32, h as f32)))
    }

    /// Returns an [`egui::Image`] of the current frame, or of the placeholder.
    pub fn image(&self) -> egui::Image<'_> {
        match self.texture_info() {
            Some((id, size)) => {
                egui::Image::from_texture(egui::load::SizedTexture::new(id, size)).shrink_to_fit()
            }
            None => egui::Image::from_texture(&self.placeholder).shrink_to_fit(),
        }
    }
}

/// View that draws a [`VideoFrames`] stream.
///
/// It wakes the window when a frame arrives and draws the newest frame on the
/// next pass.
#[cfg(feature = "wgpu-render")]
#[derive(derive_more::Debug)]
pub struct VideoView {
    frames: VideoFrames,
    frame_view: FrameView,
    /// Kept so [`set_frames`](Self::set_frames) can wake the window for the new
    /// stream.
    #[debug(skip)]
    ctx: egui::Context,
    /// Wakes the window on each frame. Dropping it stops the task.
    _wake: n0_future::task::AbortOnDropHandle<()>,
}

/// Spawns a task that requests a repaint whenever a frame arrives.
///
/// The player hands a frame over when it is due. Repainting on a timer would
/// show it up to one tick late, and the uneven delay makes a steady stream
/// judder. The task reads its own clone of `frames`, so it does not take
/// frames from the drawing pass.
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
    /// Creates a view of `frames` that draws through `render_state`.
    ///
    /// `name` names the placeholder texture. Must be called inside a Tokio
    /// runtime, which runs the task that wakes the window.
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        frames: VideoFrames,
        render_state: Option<&egui_wgpu::RenderState>,
    ) -> Self {
        Self {
            _wake: wake_on_frame(ctx, &frames),
            frames,
            frame_view: FrameView::new(ctx, name, render_state),
            ctx: ctx.clone(),
        }
    }

    /// Replaces the stream this view draws.
    ///
    /// The last frame stays on screen until the new stream delivers one. Like
    /// [`new`](Self::new), this must be called inside a Tokio runtime.
    pub fn set_frames(&mut self, frames: VideoFrames) {
        self._wake = wake_on_frame(&self.ctx, &frames);
        self.frames = frames;
    }

    /// Draws the newest frame, if a new one arrived, and returns the image.
    pub fn render(&mut self) -> egui::Image<'_> {
        if let Some(frame) = self.frames.try_next() {
            self.frame_view.render_frame(&frame);
        }
        self.frame_view.image()
    }

    /// Returns the image of the last drawn frame.
    pub fn image(&self) -> egui::Image<'_> {
        self.frame_view.image()
    }
}

/// Creates an [`egui_wgpu::WgpuConfiguration`] for video rendering.
///
/// On Linux, this builds a Vulkan device and enables
/// [`wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`] if the adapter supports
/// it. The renderer can then import DMA-BUF frames, such as PipeWire screen
/// capture, without a copy through the CPU. It blocks while it requests the
/// adapter and device.
///
/// Elsewhere, or if Vulkan is not available or excluded by `WGPU_BACKEND`,
/// egui builds the device and the renderer uploads frames from the CPU. The
/// device always requests the adapter's own limits.
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

/// Returns a device descriptor that requests the adapter's own limits.
///
/// egui requests [`wgpu::Limits::default()`] on non-GL backends. Those are
/// desktop limits, for example eight color attachments. The Raspberry Pi 4
/// Vulkan driver allows four, so the request fails and eframe exits before a
/// window opens. Video rendering needs nothing beyond the adapter's limits.
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
/// See [`device_descriptor`] for why.
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

/// Builds a Vulkan device, with DMA-BUF import if the adapter supports it.
#[cfg(all(target_os = "linux", feature = "wgpu-render"))]
fn create_egui_wgpu_config_dmabuf() -> egui_wgpu::WgpuConfiguration {
    // DMA-BUF import needs Vulkan. If `WGPU_BACKEND` asks for another
    // backend, fall back to egui's setup, which reads the same variable.
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
