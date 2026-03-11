//! Egui integration for moq-media video rendering.
//!
//! Provides [`EguiVideoRenderer`] which wraps [`moq_media::render::WgpuVideoRenderer`]
//! and handles egui texture registration, and [`create_egui_wgpu_config`] which
//! creates an optimized wgpu device configuration for video rendering.
//!
//! # Example
//!
//! ```ignore
//! use moq_media_egui::{EguiVideoRenderer, create_egui_wgpu_config};
//!
//! // In eframe setup:
//! let native_options = eframe::NativeOptions {
//!     renderer: eframe::Renderer::Wgpu,
//!     wgpu_options: create_egui_wgpu_config(),
//!     ..Default::default()
//! };
//!
//! // In your app:
//! let mut renderer = EguiVideoRenderer::new(render_state);
//! if let Some(frame) = track.current_frame() {
//!     let (tex_id, (w, h)) = renderer.render(&frame);
//!     ui.image(egui::load::SizedTexture::new(tex_id, [w as f32, h as f32]));
//! }
//! ```

use std::fmt;

pub use egui_wgpu;
pub use epaint;
pub use moq_media;
#[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
use moq_media::render::create_device_with_dmabuf_extensions;
use moq_media::{format::VideoFrame, render::WgpuVideoRenderer};

/// Convenience renderer that integrates [`WgpuVideoRenderer`] with egui.
///
/// Handles egui texture registration and updates automatically, eliminating
/// the boilerplate of managing render state, texture IDs, and frame size
/// tracking in application code.
pub struct EguiVideoRenderer {
    renderer: WgpuVideoRenderer,
    render_state: egui_wgpu::RenderState,
    texture_id: Option<epaint::TextureId>,
    last_frame_size: Option<(u32, u32)>,
}

impl fmt::Debug for EguiVideoRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EguiVideoRenderer")
            .field("renderer", &self.renderer)
            .field("texture_id", &self.texture_id)
            .field("last_frame_size", &self.last_frame_size)
            .finish()
    }
}

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
    pub fn render(&mut self, frame: &VideoFrame) -> (epaint::TextureId, (u32, u32)) {
        let view = self.renderer.render(frame);
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
        (id, dims)
    }

    /// Returns the last rendered texture ID and dimensions, if any frame has been rendered.
    pub fn last_texture(&self) -> Option<(epaint::TextureId, (u32, u32))> {
        self.texture_id.zip(self.last_frame_size)
    }
}

/// Creates an [`egui_wgpu::WgpuConfiguration`] optimized for video rendering.
///
/// On Linux with the `dmabuf-import` feature, creates a Vulkan device with
/// DMA-BUF extensions for zero-copy hardware decoder import. On all other
/// platforms (including macOS with Metal), returns the default configuration.
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
