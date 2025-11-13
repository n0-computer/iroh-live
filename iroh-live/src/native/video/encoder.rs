//

use anyhow::Result;
use rav1e::prelude::*;
use crate::av::VideoPreset;

use crate::av as lav;
use yuv::{
    bgra_to_yuv420, rgba_to_yuv420, BufferStoreMut, YuvChromaSubsampling, YuvPlanarImageMut,
    YuvRange as YRange, YuvStandardMatrix as YMatrix,
};

pub struct Av1Encoder {
    ctx: Context<u8>,
    width: u32,
    height: u32,
    framerate: u32,
    frame_count: u64,
    // Reusable I420 buffers
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Av1Encoder {
    pub fn new(width: u32, height: u32, framerate: u32) -> Result<Self> {
        // Encoder config
        let mut enc = EncoderConfig::with_speed_preset(9);
        enc.width = width as usize;
        enc.height = height as usize;
        enc.time_base = Rational::new(1, framerate as u64);
        enc.bit_depth = 8;
        enc.chroma_sampling = ChromaSampling::Cs420;
        enc.low_latency = true;
        // realtime tuning: moderate quantizer, short GOP
        enc.quantizer = 160;
        enc.min_key_frame_interval = 1;
        enc.max_key_frame_interval = framerate as u64;

        let cfg = Config::new().with_encoder_config(enc);
        let ctx: Context<u8> = cfg.new_context()?;

        let y = vec![0u8; (width as usize) * (height as usize)];
        let u = vec![0u8; (width as usize / 2) * (height as usize / 2)];
        let v = vec![0u8; (width as usize / 2) * (height as usize / 2)];

        Ok(Self { ctx, width, height, framerate, frame_count: 0, y, u, v })
    }

    pub fn with_preset(width: u32, height: u32, framerate: u32, _preset: VideoPreset) -> Result<Self> {
        // For now presets map to the same encoder; future: adjust quantizer/bitrate per preset
        Self::new(width, height, framerate)
    }

    fn rgba_to_i420(&mut self, format: &lav::VideoFormat, frame: &lav::VideoFrame) -> anyhow::Result<()> {
        let [w, h] = format.dimensions;
        let range = YRange::Full;
        let matrix = YMatrix::Bt709;
        let mut planar = YuvPlanarImageMut::<u8> {
            y_plane: BufferStoreMut::Borrowed(&mut self.y),
            y_stride: w,
            u_plane: BufferStoreMut::Borrowed(&mut self.u),
            u_stride: w / 2,
            v_plane: BufferStoreMut::Borrowed(&mut self.v),
            v_stride: w / 2,
            width: w,
            height: h,
        };
        let stride = (w as u32) * 4;
        let mode = yuv::YuvConversionMode::Balanced;
        match format.pixel_format {
            lav::PixelFormat::Rgba => rgba_to_yuv420(&mut planar, &frame.raw, stride, range, matrix, mode)?,
            lav::PixelFormat::Bgra => bgra_to_yuv420(&mut planar, &frame.raw, stride, range, matrix, mode)?,
        }
        Ok(())
    }
}

impl lav::VideoEncoder for Av1Encoder {
    fn config(&self) -> hang::catalog::VideoConfig {
        hang::catalog::VideoConfig {
            codec: hang::catalog::VideoCodec::AV1(hang::catalog::AV1::default()),
            description: None,
            coded_width: Some(self.width),
            coded_height: Some(self.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: None,
            framerate: Some(self.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }

    fn push_frame(&mut self, format: &lav::VideoFormat, frame: lav::VideoFrame) -> anyhow::Result<()> {
        self.rgba_to_i420(format, &frame)?;
        let mut rav1e_frame = self.ctx.new_frame();
        // Copy into rav1e planes
        rav1e_frame.planes[0].copy_from_raw_u8(&self.y, self.width as usize, 1);
        rav1e_frame.planes[1].copy_from_raw_u8(&self.u, (self.width as usize) / 2, 1);
        rav1e_frame.planes[2].copy_from_raw_u8(&self.v, (self.width as usize) / 2, 1);

        self.ctx.send_frame(rav1e_frame)?;
        self.frame_count += 1;
        Ok(())
    }

    fn pop_packet(&mut self) -> anyhow::Result<Option<hang::Frame>> {
        match self.ctx.receive_packet() {
            Ok(pkt) => {
                let ts = std::time::Duration::from_nanos(
                    (pkt.input_frameno as u64) * 1_000_000_000u64 / (self.framerate as u64)
                );
                let key = matches!(pkt.frame_type, FrameType::KEY);
                Ok(Some(hang::Frame { payload: pkt.data.into(), timestamp: ts, keyframe: key }))
            }
            Err(EncoderStatus::Encoded) => Ok(None),
            Err(EncoderStatus::LimitReached) => Ok(None),
            Err(EncoderStatus::NeedMoreData) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("rav1e receive_packet error: {e:?}")),
        }
    }
}
