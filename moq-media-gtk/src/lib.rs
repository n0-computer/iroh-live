//! GTK4 integration for moq-media video rendering.
//!
//! Two widget types are provided:
//!
//! - [`VideoWidget`]: CPU path. Converts frames to RGBA and uploads via
//!   [`gdk4::MemoryTexture`]. Simple, works everywhere.
//!
//! - [`GlVideoWidget`] (requires `gl` feature): GPU path. Uses a
//!   [`gtk4::GLArea`] with the GLES2 NV12 shader from `rusty-codecs`.
//!   NV12 frames are uploaded as separate Y/UV textures and converted
//!   to RGBA by the GPU. DMA-BUF frames (e.g. from VAAPI) are imported
//!   zero-copy via EGL when the `gles-dmabuf` feature is enabled.

pub use moq_media;

use gdk4::MemoryFormat;
use glib::Bytes;
use moq_media::format::VideoFrame;

// ── CPU path (MemoryTexture) ────────────────────────────────────────

/// Renders [`VideoFrame`]s into a [`gtk4::Picture`] widget via CPU RGBA upload.
///
/// Converts every frame to RGBA on the CPU and creates a
/// [`gdk4::MemoryTexture`]. Simple fallback when GL is unavailable.
#[derive(Debug)]
pub struct VideoWidget {
    picture: gtk4::Picture,
}

impl Default for VideoWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoWidget {
    /// Creates a new video widget.
    pub fn new() -> Self {
        let picture = gtk4::Picture::new();
        picture.set_content_fit(gtk4::ContentFit::Contain);
        picture.set_can_shrink(true);
        Self { picture }
    }

    /// Returns the underlying [`gtk4::Picture`] for adding to a container.
    pub fn picture(&self) -> &gtk4::Picture {
        &self.picture
    }

    /// Uploads a decoded video frame (CPU RGBA conversion + MemoryTexture).
    ///
    /// Must be called from the GTK main thread.
    pub fn upload_frame(&self, frame: &VideoFrame) {
        let rgba = frame.rgba_image();
        let w = rgba.width() as i32;
        let h = rgba.height() as i32;
        let stride = w * 4;
        let bytes = Bytes::from(rgba.as_raw());
        let texture =
            gdk4::MemoryTexture::new(w, h, MemoryFormat::R8g8b8a8, &bytes, stride as usize);
        self.picture.set_paintable(Some(&texture));
    }
}

// ── GPU path (GLArea + GlesRenderer) ────────────────────────────────

#[cfg(feature = "gl")]
mod gl_widget;
#[cfg(feature = "gl")]
pub use gl_widget::GlVideoWidget;
