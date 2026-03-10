//! Dioxus-native integration for moq-media video rendering.
//!
//! Provides [`DioxusVideoRenderer`] which wraps [`moq_media::render::WgpuVideoRenderer`]
//! and implements [`CustomPaintSource`] for use with dioxus-native's `<canvas>` element.
//!
//! # Example
//!
//! ```ignore
//! use moq_media_dioxus::DioxusVideoRenderer;
//! use dioxus_native::use_wgpu;
//!
//! let renderer = DioxusVideoRenderer::new();
//! let frame_tx = renderer.frame_sender();
//! let paint_source_id = use_wgpu(move || renderer);
//!
//! // Send frames from a decoder pipeline:
//! frame_tx.send(decoded_frame).ok();
//!
//! // In RSX:
//! rsx!(canvas { src: paint_source_id })
//! ```

pub use anyrender_vello;
pub use moq_media;
pub use wgpu_context;

use std::sync::mpsc;

use anyrender_vello::{CustomPaintCtx, CustomPaintSource, TextureHandle};
use moq_media::format::DecodedVideoFrame;
use moq_media::render::WgpuVideoRenderer;
use wgpu_context::DeviceHandle;

/// Convenience renderer that integrates [`WgpuVideoRenderer`] with dioxus-native.
///
/// Implements [`CustomPaintSource`] for use with dioxus-native's `<canvas>` element.
/// Frames are fed via a channel obtained from [`frame_sender`](Self::frame_sender).
#[allow(
    missing_debug_implementations,
    reason = "contains mpsc channel and wgpu types"
)]
pub struct DioxusVideoRenderer {
    state: RendererState,
    frame_tx: mpsc::Sender<DecodedVideoFrame>,
    frame_rx: mpsc::Receiver<DecodedVideoFrame>,
    latest_frame: Option<DecodedVideoFrame>,
}

enum RendererState {
    Suspended,
    Active {
        renderer: WgpuVideoRenderer,
        displayed: Option<TextureAndHandle>,
        next: Option<TextureAndHandle>,
    },
}

#[derive(Clone)]
struct TextureAndHandle {
    texture: wgpu::Texture,
    handle: TextureHandle,
}

impl DioxusVideoRenderer {
    /// Creates a new renderer.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            state: RendererState::Suspended,
            frame_tx: tx,
            frame_rx: rx,
            latest_frame: None,
        }
    }

    /// Returns a sender for feeding decoded video frames to this renderer.
    ///
    /// The renderer will always display the most recent frame received.
    pub fn frame_sender(&self) -> mpsc::Sender<DecodedVideoFrame> {
        self.frame_tx.clone()
    }

    /// Drains the channel, keeping only the latest frame.
    fn drain_frames(&mut self) {
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.latest_frame = Some(frame);
        }
    }
}

impl Default for DioxusVideoRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomPaintSource for DioxusVideoRenderer {
    fn resume(&mut self, device_handle: &DeviceHandle) {
        let renderer =
            WgpuVideoRenderer::new(device_handle.device.clone(), device_handle.queue.clone());
        self.state = RendererState::Active {
            renderer,
            displayed: None,
            next: None,
        };
        tracing::debug!("DioxusVideoRenderer resumed");
    }

    fn suspend(&mut self) {
        self.state = RendererState::Suspended;
        tracing::debug!("DioxusVideoRenderer suspended");
    }

    fn render(
        &mut self,
        mut ctx: CustomPaintCtx<'_>,
        width: u32,
        height: u32,
        _scale: f64,
    ) -> Option<TextureHandle> {
        if width == 0 || height == 0 {
            return None;
        }

        // Drain channel before borrowing state.
        self.drain_frames();

        let RendererState::Active {
            renderer,
            displayed,
            next,
        } = &mut self.state
        else {
            return None;
        };

        let Some(frame) = self.latest_frame.as_ref() else {
            // No frame yet — return the last displayed texture if we have one.
            return displayed.as_ref().map(|t| t.handle.clone());
        };

        // Render the frame to the WgpuVideoRenderer's internal texture.
        renderer.render(frame);

        // Get the output texture to register with vello.
        let Some(output) = renderer.output_texture() else {
            return displayed.as_ref().map(|t| t.handle.clone());
        };

        // If "next texture" dimensions don't match, unregister and drop it.
        if next
            .as_ref()
            .is_some_and(|t| t.texture.width() != width || t.texture.height() != height)
        {
            let handle = next.take().unwrap().handle;
            ctx.unregister_texture(handle);
        }

        // Register or reuse the texture handle.
        let texture_and_handle = match next {
            Some(existing) => existing,
            None => {
                let handle = ctx.register_texture(output.clone());
                *next = Some(TextureAndHandle {
                    texture: output.clone(),
                    handle,
                });
                next.as_ref().unwrap()
            }
        };

        let result = texture_and_handle.handle.clone();

        // Double-buffer: swap next → displayed.
        std::mem::swap(next, displayed);

        Some(result)
    }
}
