use std::time::Duration;

use anyhow::Result;
use image::{imageops, Delay, RgbaImage};
use yuv::{
    yuv400_to_bgra, yuv400_to_rgba, yuv420_to_bgra, yuv420_to_rgba, yuv422_to_bgra,
    yuv422_to_rgba, yuv444_to_bgra, yuv444_to_rgba, YuvChromaSubsampling, YuvGrayImage,
    YuvPlanarImage, YuvRange as YRange, YuvStandardMatrix as YMatrix,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::av::PixelFormat;

pub type FrameReceiver = crate::av::FrameReceiver;
pub type ResizeSender = crate::av::ResizeSender;

// Use the shared DecodedFrame from the ffmpeg decoder module to avoid adapters
pub use crate::video::decoder::DecodedFrame;

pub type DecoderContext = crate::av::DecoderContext;

pub struct Decoder {
    finished: oneshot::Receiver<(DecoderContext, anyhow::Result<()>)>,
}

impl Decoder {
    pub fn new(_config: &hang::catalog::VideoConfig, mut ctx: DecoderContext) -> Result<Self> {
        // We assume AV1 bitstreams (rav1d). No extradata handling required here.
        let (finished_tx, finished) = oneshot::channel();
        std::thread::spawn(move || {
            let res = decode_loop(&mut ctx);
            let _ = finished_tx.send((ctx, res));
        });
        Ok(Decoder { finished })
    }

    pub async fn closed(self) -> Result<DecoderContext> {
        let (ctx, _res) = self.finished.await?;
        Ok(ctx)
    }
}

fn decode_loop(ctx: &mut DecoderContext) -> Result<()> {
    use rav1d as dav1d;
    let mut settings = dav1d::Settings::new();
    // Let dav1d decide number of threads
    settings.set_n_threads(0);
    let mut dec = dav1d::Decoder::with_settings(&settings)?;

    let DecoderContext { packet_rx, frame_tx, resize_rx, shutdown, target_pixel_format } = ctx;

    // Simple stream clock (unused but kept for future sync)
    let mut requested_target: Option<(u32, u32)> = None;

    while let Some(pkt) = packet_rx.blocking_recv() {
        if shutdown.is_cancelled() { break; }

        // Feed data
        // Offsets/timestamps are optional, we pass timestamps in nanoseconds
        let ts = pkt.timestamp.as_nanos() as i64;
        let buf = pkt.payload.to_vec();
        // Send once; if buffer remains, push pending data until accepted
        if let Err(e) = dec.send_data(buf, None, Some(ts), None) {
            if !matches!(e, rav1d::Rav1dError::TryAgain) {
                tracing::warn!("rav1d send_data error: {e:?}");
            }
        }
        // Drain any pending slice; avoid tight loops by yielding when needed
        loop {
            match dec.send_pending_data() {
                Ok(()) => break,
                Err(rav1d::Rav1dError::TryAgain) => std::thread::yield_now(),
                Err(e) => { tracing::warn!("rav1d pending error: {e:?}"); break; }
            }
        }

        // Drain all available pictures
        loop {
            match dec.get_picture() {
                Ok(pic) => {
                    // Observe any pending resize requests
                    while let Ok((w, h)) = resize_rx.try_recv() {
                        if w > 0 && h > 0 { requested_target = Some((w, h)); }
                    }

                    // Convert YUV → RGB(A) using yuv crate (matrix + range aware)
                    let full_range = matches!(pic.color_range(), rav1d::pixel::YUVRange::Full);
                    let m = match pic.matrix_coefficients() {
                        rav1d::pixel::MatrixCoefficients::BT709 => YMatrix::Bt709,
                        _ => YMatrix::Bt601,
                    };
                    let range = if full_range { YRange::Full } else { YRange::Limited };
                    let rgba = yuv_picture_to_rgba(&pic, *target_pixel_format, range, m);

                    // Apply resize if requested (fit within bounds, preserve aspect)
                    let final_img = if let Some((max_w, max_h)) = requested_target {
                        let (w, h) = calculate_resized_size_rgba(&rgba, max_w, max_h);
                        if w != rgba.width() || h != rgba.height() {
                            imageops::resize(&rgba, w, h, imageops::FilterType::Triangle)
                        } else { rgba }
                    } else { rgba };

                    // Pixels already in requested order
                    let final_img = final_img;
                    // Use per-picture timestamps if available
                    let pic_ts = pic.timestamp().unwrap_or(ts);
                    let ts_dur = Duration::from_nanos(pic_ts as u64);

                    let frame = image::Frame::from_parts(final_img, 0, 0, Delay::from_numer_denom_ms(0,1));
                    let out = DecodedFrame { frame, timestamp: ts_dur };
                    match frame_tx.try_send(out) {
                        Ok(()) => {},
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            // Drop frame if UI is not keeping up to avoid blocking/hangs
                        }
                    }
                }
                Err(rav1d::Rav1dError::TryAgain) => break,
                Err(e) => {
                    // On decode error continue with next packets
                    tracing::warn!("rav1d decode error: {e:?}");
                    break;
                }
            }
        }
    }

    Ok(())
}

fn yuv_picture_to_rgba(
    pic: &rav1d::Picture,
    pixfmt: PixelFormat,
    range: YRange,
    matrix: YMatrix,
) -> RgbaImage {
    use rav1d::PixelLayout;
    let w = pic.width();
    let h = pic.height();
    let layout = pic.pixel_layout();
    let (stride_y, _) = pic.plane_data_geometry(rav1d::PlanarImageComponent::Y);
    let (stride_u, _) = pic.plane_data_geometry(rav1d::PlanarImageComponent::U);
    let (stride_v, _) = pic.plane_data_geometry(rav1d::PlanarImageComponent::V);
    let y_plane = pic.plane(rav1d::PlanarImageComponent::Y);
    let u_plane = pic.plane(rav1d::PlanarImageComponent::U);
    let v_plane = pic.plane(rav1d::PlanarImageComponent::V);

    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let out_stride = (w as u32) * 4;

    match layout {
        PixelLayout::I400 => {
            match pixfmt {
                PixelFormat::Rgba => yuv400_to_rgba(
                    &YuvGrayImage { y_plane: &y_plane, y_stride: stride_y as u32, width: w, height: h },
                    &mut out,
                    out_stride,
                    range,
                    matrix,
                )
                .unwrap(),
                PixelFormat::Bgra => yuv400_to_bgra(
                    &YuvGrayImage { y_plane: &y_plane, y_stride: stride_y as u32, width: w, height: h },
                    &mut out,
                    out_stride,
                    range,
                    matrix,
                )
                .unwrap(),
            }
        }
        PixelLayout::I420 | PixelLayout::I444 | PixelLayout::I422 => {
            let subsampling = match layout {
                PixelLayout::I420 => YuvChromaSubsampling::Yuv420,
                PixelLayout::I422 => YuvChromaSubsampling::Yuv422,
                _ => YuvChromaSubsampling::Yuv444,
            };
            let yuv = YuvPlanarImage {
                y_plane: &y_plane,
                y_stride: stride_y as u32,
                u_plane: &u_plane,
                u_stride: stride_u as u32,
                v_plane: &v_plane,
                v_stride: stride_v as u32,
                width: w,
                height: h,
            };
            match (pixfmt, subsampling) {
                (PixelFormat::Rgba, YuvChromaSubsampling::Yuv420) =>
                    yuv420_to_rgba(&yuv, &mut out, out_stride, range, matrix).unwrap(),
                (PixelFormat::Bgra, YuvChromaSubsampling::Yuv420) =>
                    yuv420_to_bgra(&yuv, &mut out, out_stride, range, matrix).unwrap(),
                (PixelFormat::Rgba, YuvChromaSubsampling::Yuv422) =>
                    yuv422_to_rgba(&yuv, &mut out, out_stride, range, matrix).unwrap(),
                (PixelFormat::Bgra, YuvChromaSubsampling::Yuv422) =>
                    yuv422_to_bgra(&yuv, &mut out, out_stride, range, matrix).unwrap(),
                (PixelFormat::Rgba, YuvChromaSubsampling::Yuv444) =>
                    yuv444_to_rgba(&yuv, &mut out, out_stride, range, matrix).unwrap(),
                (PixelFormat::Bgra, YuvChromaSubsampling::Yuv444) =>
                    yuv444_to_bgra(&yuv, &mut out, out_stride, range, matrix).unwrap(),
            }
        }
    }

    RgbaImage::from_raw(w, h, out).expect("valid image buffer")
}

fn calculate_resized_size_rgba(img: &RgbaImage, max_width: u32, max_height: u32) -> (u32, u32) {
    let src_w = img.width().max(1);
    let src_h = img.height().max(1);
    let max_w = max_width.max(1);
    let max_h = max_height.max(1);
    let scale_w = (max_w as f32) / (src_w as f32);
    let scale_h = (max_h as f32) / (src_h as f32);
    let scale = scale_w.min(scale_h).min(1.0).max(0.0);
    let target_width = ((src_w as f32) * scale).floor().max(1.0) as u32;
    let target_height = ((src_h as f32) * scale).floor().max(1.0) as u32;
    (target_width, target_height)
}
