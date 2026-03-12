//! Latency measurement for encode→decode pipelines.
//!
//! Measures pipeline depth (frames of delay) and per-frame wall-clock
//! timing for each encoder×decoder backend. Useful for comparing hardware
//! vs software paths and detecting regressions.

use std::time::Instant;

use super::test_util::{encoded_frames_to_media_packets, make_test_pattern};
use crate::{
    config::VideoConfig,
    format::{DecodeConfig, VideoPreset},
    traits::{VideoDecoder, VideoEncoder, VideoEncoderFactory},
};

/// Result of a pipeline latency measurement.
#[derive(Debug)]
struct LatencyReport {
    encoder_name: String,
    decoder_name: String,
    num_input_frames: usize,
    num_output_frames: usize,
    /// Frames pushed before the first decoded frame appears.
    pipeline_depth: usize,
    /// Wall-clock time per decoded frame, excluding pipeline fill.
    per_frame_us: Vec<u64>,
    /// Wall time for encode phase only.
    encode_wall_ms: u64,
    /// Total wall time for the full run.
    total_wall_ms: u64,
}

impl std::fmt::Display for LatencyReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let median = percentile(&self.per_frame_us, 50);
        let p95 = percentile(&self.per_frame_us, 95);
        let p99 = percentile(&self.per_frame_us, 99);
        let max = self.per_frame_us.iter().copied().max().unwrap_or(0);
        write!(
            f,
            "{} → {}: depth={} frames, decode median={}µs p95={}µs p99={}µs max={}µs, \
             in={} out={} enc={}ms total={}ms",
            self.encoder_name,
            self.decoder_name,
            self.pipeline_depth,
            median,
            p95,
            p99,
            max,
            self.num_input_frames,
            self.num_output_frames,
            self.encode_wall_ms,
            self.total_wall_ms,
        )
    }
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (pct * sorted.len() / 100).min(sorted.len() - 1);
    sorted[idx]
}

/// Measures encode→decode pipeline latency.
///
/// Feeds `num_frames` of test-pattern frames through the encoder and decoder,
/// measuring when each decoded frame first appears relative to the input frame
/// that produced it.
fn measure_pipeline_latency<E: VideoEncoder, D: VideoDecoder>(
    mut encoder: E,
    config: &VideoConfig,
    w: u32,
    h: u32,
    num_frames: usize,
) -> LatencyReport {
    let decode_config = DecodeConfig::default();
    let mut decoder = D::new(config, &decode_config).expect("decoder init failed");

    let encoder_name = encoder.name().to_string();
    let decoder_name = decoder.name().to_string();

    let mut pipeline_depth = None;
    let mut per_frame_us = Vec::new();
    let mut total_decoded = 0usize;

    let wall_start = Instant::now();

    // Pre-encode all frames to separate encode latency from decode latency.
    let mut all_packets = Vec::new();
    let encode_start = Instant::now();
    for i in 0..num_frames {
        let frame = make_test_pattern(w, h, i as u32);
        encoder.push_frame(frame).expect("push_frame failed");
        let mut frame_packets = Vec::new();
        while let Some(pkt) = encoder.pop_packet().expect("pop_packet failed") {
            frame_packets.push(pkt);
        }
        all_packets.push(encoded_frames_to_media_packets(frame_packets));
    }
    let encode_wall_ms = encode_start.elapsed().as_millis() as u64;

    // Now measure decode latency in isolation.
    for (i, packets) in all_packets.into_iter().enumerate() {
        let frame_start = Instant::now();

        // Decode all packets from this frame.
        // Drop decoded frames immediately to free VA surface pool slots.
        let mut decoded_this_frame = 0usize;
        for pkt in packets {
            decoder.push_packet(pkt).expect("push_packet failed");
            while let Some(frame) = decoder.pop_frame().expect("pop_frame failed") {
                drop(frame);
                total_decoded += 1;
                decoded_this_frame += 1;
            }
        }

        let elapsed_us = frame_start.elapsed().as_micros() as u64;

        if decoded_this_frame > 0 {
            if pipeline_depth.is_none() {
                pipeline_depth = Some(i);
            }
            per_frame_us.push(elapsed_us);
        }
    }

    per_frame_us.sort_unstable();

    LatencyReport {
        encoder_name,
        decoder_name,
        num_input_frames: num_frames,
        num_output_frames: total_decoded,
        pipeline_depth: pipeline_depth.unwrap_or(num_frames),
        per_frame_us,
        encode_wall_ms,
        total_wall_ms: wall_start.elapsed().as_millis() as u64,
    }
}

// ---------------------------------------------------------------------------
// Software H.264 (baseline)
// ---------------------------------------------------------------------------

use super::h264;

#[test]
fn pipeline_latency_h264_software() {
    let preset = VideoPreset::P360;
    let (w, h) = preset.dimensions();
    let encoder = h264::H264Encoder::with_preset(preset).expect("H264Encoder init");
    let config = encoder.config();

    let report =
        measure_pipeline_latency::<_, h264::decoder::H264VideoDecoder>(encoder, &config, w, h, 60);
    eprintln!("{report}");

    assert!(
        report.pipeline_depth <= 3,
        "software pipeline depth {} > 3 — unexpected buffering",
        report.pipeline_depth
    );
    assert!(
        report.num_output_frames >= 55,
        "software produced only {} frames from 60 input",
        report.num_output_frames
    );
}

// ---------------------------------------------------------------------------
// VAAPI H.264
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "linux", feature = "vaapi"))]
mod vaapi_latency {
    use super::*;
    use crate::codec::vaapi;

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn pipeline_latency_h264_vaapi_encode_sw_decode() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let encoder = vaapi::VaapiEncoder::with_preset(preset).expect("VAAPI encoder init");
        let config = encoder.config();

        let report = measure_pipeline_latency::<_, h264::decoder::H264VideoDecoder>(
            encoder, &config, w, h, 60,
        );
        eprintln!("{report}");

        assert!(
            report.pipeline_depth <= 5,
            "VAAPI encode + SW decode depth {} > 5",
            report.pipeline_depth
        );
        assert!(
            report.num_output_frames >= 50,
            "VAAPI→SW produced only {} frames from 60 input",
            report.num_output_frames
        );
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn pipeline_latency_h264_vaapi_full() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let encoder = vaapi::VaapiEncoder::with_preset(preset).expect("VAAPI encoder init");
        let config = encoder.config();

        let report = measure_pipeline_latency::<_, vaapi::VaapiDecoder>(encoder, &config, w, h, 60);
        eprintln!("{report}");

        assert!(
            report.pipeline_depth <= 5,
            "full VAAPI pipeline depth {} > 5",
            report.pipeline_depth
        );
        assert!(
            report.num_output_frames >= 50,
            "full VAAPI produced only {} frames from 60 input",
            report.num_output_frames
        );
    }

    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn pipeline_latency_sw_encode_vaapi_decode() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let encoder = h264::H264Encoder::with_preset(preset).expect("H264Encoder init");
        let config = encoder.config();

        let report = measure_pipeline_latency::<_, vaapi::VaapiDecoder>(encoder, &config, w, h, 60);
        eprintln!("{report}");

        assert!(
            report.pipeline_depth <= 5,
            "SW encode + VAAPI decode depth {} > 5",
            report.pipeline_depth
        );
        assert!(
            report.num_output_frames >= 50,
            "SW→VAAPI produced only {} frames from 60 input",
            report.num_output_frames
        );
    }

    /// Comparison test: prints all four encode×decode combinations side by side.
    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn pipeline_latency_comparison() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();

        eprintln!("\n=== Pipeline Latency Comparison (60 frames, {w}x{h}) ===\n");

        // SW → SW
        {
            let enc = h264::H264Encoder::with_preset(preset).unwrap();
            let config = enc.config();
            let r = measure_pipeline_latency::<_, h264::decoder::H264VideoDecoder>(
                enc, &config, w, h, 60,
            );
            eprintln!("  {r}");
        }

        // VAAPI → SW
        {
            let enc = vaapi::VaapiEncoder::with_preset(preset).unwrap();
            let config = enc.config();
            let r = measure_pipeline_latency::<_, h264::decoder::H264VideoDecoder>(
                enc, &config, w, h, 60,
            );
            eprintln!("  {r}");
        }

        // SW → VAAPI
        {
            let enc = h264::H264Encoder::with_preset(preset).unwrap();
            let config = enc.config();
            let r = measure_pipeline_latency::<_, vaapi::VaapiDecoder>(enc, &config, w, h, 60);
            eprintln!("  {r}");
        }

        // VAAPI → VAAPI
        {
            let enc = vaapi::VaapiEncoder::with_preset(preset).unwrap();
            let config = enc.config();
            let r = measure_pipeline_latency::<_, vaapi::VaapiDecoder>(enc, &config, w, h, 60);
            eprintln!("  {r}");
        }

        eprintln!();
    }
}
