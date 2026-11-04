//! Editing the pixels of a frame on the CPU.

use anyhow::Result;
use iroh_live::media::video::{Frame, I420, Size, Surface};

/// Returns a copy of `frame` whose I420 planes `edit` changed.
///
/// `edit` gets the packed planes (Y, then U, then V, no padding), the width and
/// the height. A frame on the GPU is downloaded first, which fails for a
/// surface with no CPU mapping. The timestamp and color space carry over.
///
/// # Errors
///
/// Fails if the frame has no CPU representation.
pub fn edit(frame: &Frame, edit: impl FnOnce(&mut [u8], usize, usize)) -> Result<Frame> {
    let source = frame.surface.to_i420()?;
    let (width, height) = (source.width() as usize, source.height() as usize);
    let color = source.color();
    let mut data = source.into_owned().into_data();
    edit(&mut data, width, height);
    let mut image = I420::new(Size::new(width as u32, height as u32), data)?;
    if let Some(color) = color {
        image = image.with_color(color);
    }
    Ok(Frame::new(Surface::I420(image), frame.timestamp))
}
