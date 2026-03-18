//! Egui integration for moq-media video rendering.
//!
//! Provides [`FrameView`] for rendering raw [`VideoFrame`]s into egui, and
//! [`VideoTrackView`] which wraps a [`VideoTrack`] with viewport management.
//! Both support optional wgpu-accelerated rendering via [`EguiVideoRenderer`]
//! (requires `wgpu-render` feature).
//!
//! Also provides [`create_egui_wgpu_config`] which creates an optimized wgpu
//! device configuration for video rendering with DMA-BUF import support.
//!
//! # Example
//!
//! ```ignore
//! use moq_media_egui::VideoTrackView;
//!
//! let mut view = VideoTrackView::new(&ctx, "video", track);
//! // in update loop:
//! let (image, frame_ts) = view.render(&ctx, available_size);
//! ui.add(image);
//! ```

pub mod overlay;

use std::{fmt, time::Duration};

pub use moq_media;
use moq_media::format::VideoFrame;
use moq_media::subscribe::VideoTrack;

#[cfg(feature = "wgpu-render")]
pub use egui_wgpu;
#[cfg(feature = "wgpu-render")]
pub use epaint;

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
// FrameView — renders VideoFrames to an egui texture (CPU or wgpu)
// ---------------------------------------------------------------------------

/// Renders [`VideoFrame`]s into an egui texture.
///
/// Supports both CPU (egui texture upload) and wgpu-accelerated rendering.
/// The wgpu path requires the `wgpu-render` feature and is selected at
/// construction time via [`FrameView::new_wgpu`].
///
/// Low-level building block. For [`VideoTrack`]-based rendering with
/// automatic viewport management, use [`VideoTrackView`] instead.
pub struct FrameView {
    texture: egui::TextureHandle,
    #[cfg(feature = "wgpu-render")]
    egui_renderer: Option<EguiVideoRenderer>,
}

impl fmt::Debug for FrameView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameView").finish_non_exhaustive()
    }
}

impl FrameView {
    /// Creates a new CPU-only frame view.
    pub fn new(ctx: &egui::Context, name: &str) -> Self {
        let placeholder = egui::ColorImage::filled([1, 1], egui::Color32::BLACK);
        let texture = ctx.load_texture(name, placeholder, Default::default());
        Self {
            texture,
            #[cfg(feature = "wgpu-render")]
            egui_renderer: None,
        }
    }

    /// Creates a new frame view with wgpu-accelerated rendering.
    #[cfg(feature = "wgpu-render")]
    pub fn new_wgpu(
        ctx: &egui::Context,
        name: &str,
        render_state: Option<&egui_wgpu::RenderState>,
    ) -> Self {
        let placeholder = egui::ColorImage::filled([1, 1], egui::Color32::BLACK);
        let texture = ctx.load_texture(name, placeholder, Default::default());
        let egui_renderer = render_state.map(EguiVideoRenderer::new);
        Self {
            texture,
            egui_renderer,
        }
    }

    /// Returns whether this view uses wgpu rendering.
    pub fn is_wgpu(&self) -> bool {
        #[cfg(feature = "wgpu-render")]
        {
            self.egui_renderer.is_some()
        }
        #[cfg(not(feature = "wgpu-render"))]
        {
            false
        }
    }

    /// Returns which render path was used for the last frame.
    pub fn render_path_name(&self) -> &'static str {
        #[cfg(feature = "wgpu-render")]
        if let Some(ref r) = self.egui_renderer {
            return match r.last_render_path() {
                moq_media::render::RenderPath::None => "wgpu",
                moq_media::render::RenderPath::CpuRgba => "wgpu/cpu-rgba",
                moq_media::render::RenderPath::CpuNv12 => "wgpu/cpu-nv12",
                moq_media::render::RenderPath::DmaBuf => "wgpu/dmabuf",
                moq_media::render::RenderPath::MetalZeroCopy => "wgpu/metal",
                moq_media::render::RenderPath::GpuDownload => "wgpu/gpu-dl",
            };
        }
        "cpu"
    }

    /// Uploads a video frame to the texture (CPU or wgpu).
    pub fn render_frame(&mut self, frame: &VideoFrame) {
        #[cfg(feature = "wgpu-render")]
        if let Some(ref mut r) = self.egui_renderer {
            if let Err(e) = r.render(frame) {
                tracing::warn!("wgpu render failed: {e:#}");
            }
            return;
        }

        let (w, h) = (frame.width(), frame.height());
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [w as usize, h as usize],
            frame.rgba_image().as_raw(),
        );
        self.texture.set(image, Default::default());
    }

    /// Returns the texture ID and pixel dimensions, if any frame has been rendered.
    pub fn texture_info(&self) -> Option<(egui::TextureId, egui::Vec2)> {
        #[cfg(feature = "wgpu-render")]
        if let Some(ref r) = self.egui_renderer
            && let Some((id, (w, h))) = r.last_texture()
        {
            return Some((id, egui::vec2(w as f32, h as f32)));
        }

        let size = self.texture.size_vec2();
        if size.x > 0.0 && size.y > 0.0 {
            Some((self.texture.id(), size))
        } else {
            None
        }
    }

    /// Returns an [`egui::Image`] for the current texture, suitable for `ui.add()`.
    pub fn image(&self) -> egui::Image<'_> {
        #[cfg(feature = "wgpu-render")]
        if let Some(ref r) = self.egui_renderer
            && let Some((id, (w, h))) = r.last_texture()
        {
            return egui::Image::from_texture(egui::load::SizedTexture::new(
                id,
                [w as f32, h as f32],
            ))
            .shrink_to_fit();
        }

        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}

// ---------------------------------------------------------------------------
// VideoTrackView — FrameView + VideoTrack with viewport management
// ---------------------------------------------------------------------------

/// Renders a [`VideoTrack`] into an egui UI.
///
/// Wraps a [`FrameView`] with automatic viewport scaling and frame polling.
/// Supports both CPU and wgpu-accelerated rendering.
pub struct VideoTrackView {
    track: VideoTrack,
    frame_view: FrameView,
    size: egui::Vec2,
}

impl fmt::Debug for VideoTrackView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoTrackView")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl VideoTrackView {
    /// Creates a new CPU-only view.
    pub fn new(ctx: &egui::Context, name: &str, track: VideoTrack) -> Self {
        Self {
            track,
            frame_view: FrameView::new(ctx, name),
            size: egui::vec2(100.0, 100.0),
        }
    }

    /// Creates a new view with wgpu-accelerated rendering.
    #[cfg(feature = "wgpu-render")]
    pub fn new_wgpu(
        ctx: &egui::Context,
        name: &str,
        track: VideoTrack,
        render_state: Option<&egui_wgpu::RenderState>,
    ) -> Self {
        Self {
            track,
            frame_view: FrameView::new_wgpu(ctx, name, render_state),
            size: egui::vec2(100.0, 100.0),
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

    /// Returns whether this view uses wgpu rendering.
    pub fn is_wgpu(&self) -> bool {
        self.frame_view.is_wgpu()
    }

    /// Renders the current frame and returns `(image, frame_timestamp)`.
    ///
    /// Updates the viewport when `available_size` changes. The returned
    /// timestamp is from the most recent decoded frame, or `None` if no
    /// new frame arrived this call.
    pub fn render(
        &mut self,
        ctx: &egui::Context,
        available_size: egui::Vec2,
    ) -> (egui::Image<'_>, Option<Duration>) {
        if available_size != self.size {
            self.size = available_size;
            let ppp = ctx.pixels_per_point();
            let w = (available_size.x * ppp) as u32;
            let h = (available_size.y * ppp) as u32;
            self.track.set_viewport(w, h);
        }

        let mut frame_ts = None;
        if let Some(frame) = self.track.current_frame() {
            frame_ts = Some(frame.timestamp);
            self.frame_view.render_frame(&frame);
        }

        (self.frame_view.image(), frame_ts)
    }
}

// ---------------------------------------------------------------------------
// EguiVideoRenderer — wgpu rendering for raw VideoFrame
// ---------------------------------------------------------------------------

#[cfg(feature = "wgpu-render")]
use moq_media::render::WgpuVideoRenderer;

/// Convenience renderer that integrates [`WgpuVideoRenderer`] with egui.
///
/// Handles egui texture registration and updates automatically, eliminating
/// the boilerplate of managing render state, texture IDs, and frame size
/// tracking in application code.
#[cfg(feature = "wgpu-render")]
pub struct EguiVideoRenderer {
    renderer: WgpuVideoRenderer,
    render_state: egui_wgpu::RenderState,
    texture_id: Option<epaint::TextureId>,
    last_frame_size: Option<(u32, u32)>,
}

#[cfg(feature = "wgpu-render")]
impl fmt::Debug for EguiVideoRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EguiVideoRenderer")
            .field("renderer", &self.renderer)
            .field("texture_id", &self.texture_id)
            .field("last_frame_size", &self.last_frame_size)
            .finish()
    }
}

#[cfg(feature = "wgpu-render")]
impl EguiVideoRenderer {
    /// Creates a new renderer from an egui wgpu render state.
    pub fn new(render_state: &egui_wgpu::RenderState) -> Self {
        let renderer =
            WgpuVideoRenderer::new(render_state.device.clone(), render_state.queue.clone());
        Self {
            renderer,
            render_state: render_state.clone(),
            texture_id: None,
            last_frame_size: None,
        }
    }

    /// Renders a decoded video frame and returns the egui texture ID and dimensions.
    pub fn render(
        &mut self,
        frame: &VideoFrame,
    ) -> anyhow::Result<(epaint::TextureId, (u32, u32))> {
        let view = self.renderer.render(frame)?;
        let device = &self.render_state.device;
        let mut egui_renderer = self.render_state.renderer.write();

        let id = if let Some(id) = self.texture_id {
            egui_renderer.update_egui_texture_from_wgpu_texture(
                device,
                view,
                wgpu::FilterMode::Linear,
                id,
            );
            id
        } else {
            let id = egui_renderer.register_native_texture(device, view, wgpu::FilterMode::Linear);
            self.texture_id = Some(id);
            id
        };

        let dims = (frame.width(), frame.height());
        self.last_frame_size = Some(dims);
        Ok((id, dims))
    }

    /// Returns the last rendered texture ID and dimensions, if any frame has been rendered.
    pub fn last_texture(&self) -> Option<(epaint::TextureId, (u32, u32))> {
        self.texture_id.zip(self.last_frame_size)
    }

    /// Returns which render path was used for the last frame.
    pub fn last_render_path(&self) -> moq_media::render::RenderPath {
        self.renderer.last_render_path()
    }
}

/// Creates an [`egui_wgpu::WgpuConfiguration`] optimized for video rendering.
///
/// On Linux with the `dmabuf-import` feature, creates a Vulkan device with
/// DMA-BUF extensions for zero-copy hardware decoder import. On all other
/// platforms (including macOS with Metal), returns the default configuration.
#[cfg(feature = "wgpu-render")]
pub fn create_egui_wgpu_config() -> egui_wgpu::WgpuConfiguration {
    #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
    {
        create_egui_wgpu_config_dmabuf()
    }
    #[cfg(not(all(target_os = "linux", feature = "dmabuf-import")))]
    {
        egui_wgpu::WgpuConfiguration::default()
    }
}

#[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
fn create_egui_wgpu_config_dmabuf() -> egui_wgpu::WgpuConfiguration {
    use moq_media::render::create_device_with_dmabuf_extensions;

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("no suitable wgpu adapter");

    let (device, queue) = create_device_with_dmabuf_extensions(&adapter).unwrap_or_else(|e| {
        tracing::warn!("DMA-BUF device creation failed ({e}), using default");
        pollster::block_on(adapter.request_device(&Default::default()))
            .expect("wgpu device creation failed")
    });

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
