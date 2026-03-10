//! Dioxus-native integration for moq-media video rendering.
//!
//! Provides [`use_video_renderer`], a dioxus hook that creates a
//! [`DioxusVideoRenderer`] and returns a [`VideoTrackHandle`] for setting the
//! active video track.
//!
//! # Example
//!
//! ```ignore
//! use moq_media_dioxus::use_video_renderer;
//!
//! let (handle, paint_source_id) = use_video_renderer();
//!
//! // Set or swap the video track at any time:
//! handle.set(watch_track);
//!
//! // In RSX:
//! rsx!(canvas { "src": paint_source_id })
//! ```

pub use anyrender_vello;
pub use dioxus_native;
pub use moq_media;
pub use wgpu_context;

use std::sync::{Arc, Mutex};

use anyrender_vello::{CustomPaintCtx, CustomPaintSource, TextureHandle};
use dioxus_native::prelude::dioxus_core::use_hook;
use dioxus_native::use_wgpu;
use moq_media::render::WgpuVideoRenderer;
use moq_media::subscribe::WatchTrack;
use wgpu_context::DeviceHandle;

/// Dioxus hook that creates a video renderer and returns a handle for setting tracks.
///
/// Returns `(handle, paint_source_id)` where:
/// - `handle` is a [`VideoTrackHandle`] for setting/swapping the active video track
/// - `paint_source_id` is a `u64` to pass as `"src"` on a `<canvas>` element
///
/// This hook is safe to call on every re-render — the renderer and handle are
/// created once and persist across re-renders.
pub fn use_video_renderer() -> (VideoTrackHandle, u64) {
    let handle = use_hook(VideoTrackHandle::new);
    let paint_source_id = use_wgpu({
        let h = handle.clone();
        move || DioxusVideoRenderer::new(h)
    });
    tracing::debug!("use_video_renderer: paint_source_id={paint_source_id}");
    (handle, paint_source_id)
}

/// Shared handle for setting the active video track on a [`DioxusVideoRenderer`].
///
/// This is `Clone` and `Send`, so it can be stored in dioxus hooks and
/// cloned into `use_effect` closures. All clones share the same underlying state.
#[derive(Clone, Debug)]
pub struct VideoTrackHandle(Arc<Mutex<Option<WatchTrack>>>);

impl VideoTrackHandle {
    /// Creates a new empty handle.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    /// Sets or replaces the active video track.
    pub fn set(&self, track: WatchTrack) {
        *self.0.lock().expect("poisoned") = Some(track);
    }

    /// Clears the active video track.
    pub fn clear(&self) {
        *self.0.lock().expect("poisoned") = None;
    }
}

impl Default for VideoTrackHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience renderer that integrates [`WgpuVideoRenderer`] with dioxus-native.
///
/// Implements [`CustomPaintSource`] for use with dioxus-native's `<canvas>` element.
/// Prefer using [`use_video_renderer`] instead of constructing this directly.
#[allow(
    missing_debug_implementations,
    reason = "contains wgpu types that don't impl Debug"
)]
pub struct DioxusVideoRenderer {
    state: RendererState,
    track: VideoTrackHandle,
}

enum RendererState {
    Suspended,
    Active {
        renderer: Box<WgpuVideoRenderer>,
        /// Registered texture handle for vello, with cached dimensions.
        /// Re-registered when the output texture is recreated (resolution change).
        registered: Option<RegisteredTexture>,
    },
}

struct RegisteredTexture {
    width: u32,
    height: u32,
    handle: TextureHandle,
}

impl DioxusVideoRenderer {
    /// Creates a new renderer connected to the given track handle.
    pub fn new(track: VideoTrackHandle) -> Self {
        tracing::debug!("DioxusVideoRenderer created");
        Self {
            state: RendererState::Suspended,
            track,
        }
    }
}

impl CustomPaintSource for DioxusVideoRenderer {
    fn resume(&mut self, device_handle: &DeviceHandle) {
        let renderer = Box::new(WgpuVideoRenderer::new(
            device_handle.device.clone(),
            device_handle.queue.clone(),
        ));
        self.state = RendererState::Active {
            renderer,
            registered: None,
        };
        tracing::info!("DioxusVideoRenderer resumed");
    }

    fn suspend(&mut self) {
        self.state = RendererState::Suspended;
        tracing::info!("DioxusVideoRenderer suspended");
    }

    fn render(
        &mut self,
        mut ctx: CustomPaintCtx<'_>,
        width: u32,
        height: u32,
        _scale: f64,
    ) -> Option<TextureHandle> {
        let RendererState::Active {
            renderer,
            registered,
        } = &mut self.state
        else {
            tracing::trace!("render: suspended");
            return None;
        };

        let frame = self
            .track
            .0
            .lock()
            .expect("poisoned")
            .as_mut()
            .and_then(|t| t.current_frame());

        let Some(frame) = frame.as_ref() else {
            // No new frame — return the last registered texture if we have one.
            tracing::trace!("render: no frame, has_registered={}", registered.is_some());
            return registered.as_ref().map(|r| r.handle.clone());
        };

        tracing::trace!(
            "render: frame {:?}, canvas={}x{}",
            frame.dimensions(),
            width,
            height
        );

        // Render the frame to the WgpuVideoRenderer's internal texture.
        renderer.render(frame);

        let Some((out_w, out_h)) = renderer.output_dimensions() else {
            return registered.as_ref().map(|r| r.handle.clone());
        };

        // Re-register if the output texture was recreated (resolution change).
        let dims_match = registered
            .as_ref()
            .is_some_and(|r| r.width == out_w && r.height == out_h);

        if !dims_match {
            if let Some(old) = registered.take() {
                ctx.unregister_texture(old.handle);
            }
            let output = renderer
                .output_texture()
                .expect("has dimensions but no texture");
            let handle = ctx.register_texture(output.clone());
            tracing::debug!("render: registered texture {}x{}", out_w, out_h);
            *registered = Some(RegisteredTexture {
                width: out_w,
                height: out_h,
                handle,
            });
        }

        registered.as_ref().map(|r| r.handle.clone())
    }
}
