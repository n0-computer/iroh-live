use crate::av::Decoders;

pub use self::{audio::*, video::*};

pub(crate) mod audio;
mod resample;
pub(crate) mod video;

#[derive(Debug, Clone, Copy)]
pub struct DefaultDecoders;

impl Decoders for DefaultDecoders {
    type Audio = OpusAudioDecoder;
    type Video = H264VideoDecoder;
}

/// No-op replacement for `ffmpeg_log_init`. Nothing to initialize.
pub fn codec_init() {}

#[cfg(test)]
mod tests {
    use crate::av::{
        AudioDecoder, AudioEncoder, AudioEncoderInner, AudioFormat, AudioPreset, DecodeConfig,
        Decoders, PixelFormat, VideoDecoder, VideoEncoder, VideoEncoderInner, VideoFormat,
        VideoFrame, VideoPreset,
    };

    use super::*;

    #[test]
    fn default_decoders_types() {
        // Verify DefaultDecoders associates the correct types
        fn assert_decoders<D: Decoders>() {}
        assert_decoders::<DefaultDecoders>();
    }

    #[test]
    fn video_pipeline_roundtrip() {
        let preset = VideoPreset::P180;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();

        // Encode 10 frames
        let mut packets = Vec::new();
        for i in 0..10u8 {
            let pixel = [i.wrapping_mul(25), 128, 64, 255];
            let raw: Vec<u8> = pixel.repeat((w * h) as usize);
            let frame = VideoFrame {
                format: VideoFormat {
                    pixel_format: PixelFormat::Rgba,
                    dimensions: [w, h],
                },
                raw: raw.into(),
            };
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        assert!(!packets.is_empty());

        let config = enc.config();
        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();

        let mut decoded = 0;
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                assert_eq!(frame.img().width(), w);
                assert_eq!(frame.img().height(), h);
                decoded += 1;
            }
        }
        assert!(decoded >= 5, "decoded {decoded} frames, expected >= 5");
    }

    #[test]
    fn audio_pipeline_roundtrip() {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        // Encode 1 second of 440Hz sine (50 frames * 20ms)
        let sine: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin())
            .collect();
        enc.push_samples(&sine).unwrap();

        let mut packets = Vec::new();
        while let Some(pkt) = enc.pop_packet().unwrap() {
            packets.push(pkt);
        }
        assert_eq!(packets.len(), 50, "1 second at 20ms/frame = 50 packets");

        let mut dec = OpusAudioDecoder::new(&config, format).unwrap();
        let mut total_samples = 0;
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(samples) = dec.pop_samples().unwrap() {
                total_samples += samples.len();
            }
        }
        assert_eq!(total_samples, 48000, "should decode 48000 mono samples");
    }
}
