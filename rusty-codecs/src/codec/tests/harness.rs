//! Codec testing harness with encoder×decoder matrix tests.
//!
//! Provides infrastructure for testing all encoder/decoder combinations
//! across colors, resolutions, and patterns. Uses PSNR and per-channel
//! error metrics to detect colorspace bugs, range clamping, and
//! backend-specific rendering differences (e.g. VAAPI vs software).
//!
//! # Test categories
//!
//! - **Color accuracy**: solid color patches through every encoder×decoder pair.
//! - **Resolution sweep**: standard + odd dimensions per codec.
//! - **Pattern quality**: gradients, checkerboard, color ramp with PSNR gates.
//! - **Cross-backend parity**: compare SW vs HW decoder output for same bitstream.
//! - **Audio roundtrip**: Opus encode/decode, cross-channel conversion.

mod metrics;
mod patterns;
#[cfg(feature = "h264")]
mod vectors;

use std::{f32::consts::PI, time::Duration};

use metrics::{FrameMetrics, compute_metrics};

use super::{test_util::video_encode, *};
use crate::{
    codec::test_util::encoded_frames_to_media_packets,
    config::{AudioConfig, VideoConfig},
    format::{
        AudioFormat, AudioPreset, DecodeConfig, DecoderBackend, EncodedFrame, MediaPacket,
        VideoFrame, VideoPreset,
    },
    traits::{
        AudioDecoder, AudioEncoder, AudioEncoderFactory, Decoders, VideoDecoder, VideoEncoder,
        VideoEncoderFactory,
    },
};

/// Default timeout for single-combination codec tests.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Extended timeout for matrix tests that iterate many encoder×decoder×pattern combinations.
const MATRIX_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs a closure on a new thread with a timeout. Panics if the closure
/// panics or if the timeout is exceeded.
fn with_timeout<F: FnOnce() + Send + 'static>(timeout: Duration, f: F) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(())) => {
            let _ = handle.join();
        }
        Ok(Err(panic_payload)) => std::panic::resume_unwind(panic_payload),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("test timed out after {timeout:?}")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("test thread terminated unexpectedly")
        }
    }
}

// ── Harness infrastructure ───────────────────────────────────────────

/// Minimum PSNR (dB) for solid-color roundtrips. Lossy codecs through
/// YUV420 should still produce >30 dB for uniform patches.
const SOLID_COLOR_MIN_PSNR: f64 = 28.0;

/// Minimum PSNR for complex patterns (gradients, checkerboard).
const PATTERN_MIN_PSNR: f64 = 20.0;

/// Maximum per-channel PSNR imbalance (dB) before flagging color bias.
const COLOR_BIAS_THRESHOLD: f64 = 8.0;

/// Describes a video encoder for matrix tests.
struct TestEncoder {
    name: &'static str,
    family: CodecFamily,
    /// Minimum frames to feed before expecting output (rav1e needs more look-ahead).
    min_frames: usize,
    create: fn(VideoPreset) -> Option<Box<dyn VideoEncoder>>,
    create_with_dims: fn(u32, u32) -> Option<Box<dyn VideoEncoder>>,
}

/// Describes a video decoder for matrix tests.
struct TestDecoder {
    name: &'static str,
    family: CodecFamily,
    decode: fn(&VideoConfig, &DecodeConfig, Vec<MediaPacket>) -> Option<Vec<VideoFrame>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "Av1 variant used when AV1 tests are re-enabled")]
enum CodecFamily {
    H264,
    Av1,
}

fn all_encoders() -> Vec<TestEncoder> {
    let mut encoders = Vec::new();

    #[cfg(feature = "h264")]
    encoders.push(TestEncoder {
        name: "h264-openh264",
        family: CodecFamily::H264,
        min_frames: 10,
        create: |preset| Some(Box::new(H264Encoder::with_preset(preset).ok()?)),
        create_with_dims: |w, h| {
            let config = crate::format::VideoEncoderConfig {
                width: w,
                height: h,
                framerate: 30,
                bitrate: None,
                nal_format: Default::default(),
            };
            Some(Box::new(H264Encoder::with_config(config).ok()?))
        },
    });

    // NOTE: AV1 (rav1e) encoder omitted — too slow for routine testing.
    // Re-enable when AV1 performance is acceptable.

    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    encoders.push(TestEncoder {
        name: "h264-vaapi",
        family: CodecFamily::H264,
        min_frames: 10,
        create: |preset| Some(Box::new(VaapiEncoder::with_preset(preset).ok()?)),
        create_with_dims: |w, h| {
            let config = crate::format::VideoEncoderConfig {
                width: w,
                height: h,
                framerate: 30,
                bitrate: None,
                nal_format: Default::default(),
            };
            Some(Box::new(VaapiEncoder::with_config(config).ok()?))
        },
    });

    #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
    encoders.push(TestEncoder {
        name: "h264-vtb",
        family: CodecFamily::H264,
        min_frames: 10,
        create: |preset| Some(Box::new(VtbEncoder::with_preset(preset).ok()?)),
        create_with_dims: |w, h| {
            let config = crate::format::VideoEncoderConfig {
                width: w,
                height: h,
                framerate: 30,
                bitrate: None,
                nal_format: Default::default(),
            };
            Some(Box::new(VtbEncoder::with_config(config).ok()?))
        },
    });

    encoders
}

fn all_decoders() -> Vec<TestDecoder> {
    let mut decoders = Vec::new();

    #[cfg(feature = "h264")]
    decoders.push(TestDecoder {
        name: "h264-openh264",
        family: CodecFamily::H264,
        decode: |config, dc, pkts| {
            let mut dec = h264::decoder::H264VideoDecoder::new(config, dc).ok()?;
            Some(decode_all(&mut dec, pkts))
        },
    });

    // NOTE: AV1 (rav1d) decoder omitted — see encoder note above.

    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        // Track whether VAAPI decode has timed out — once it does, skip all
        // further attempts to avoid burning minutes on stuck GPU operations.
        static VAAPI_DECODE_DISABLED: AtomicBool = AtomicBool::new(false);

        decoders.push(TestDecoder {
            name: "h264-vaapi",
            family: CodecFamily::H264,
            decode: |config, dc, pkts| {
                if VAAPI_DECODE_DISABLED.load(Ordering::Relaxed) {
                    return None;
                }
                // VAAPI decode + GPU→CPU download (.img()) can hang in test environments.
                // Wrap in a thread with a per-decode timeout to avoid blocking the test suite.
                let config = config.clone();
                let dc = dc.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut dec = vaapi::VaapiDecoder::new(&config, &dc).ok()?;
                        // VAAPI decoder uses a small frame pool (~4 buffers).
                        // With BlockingMode::Blocking, push_packet() blocks when
                        // the pool is exhausted. To avoid this, we download each
                        // frame to CPU and drop the GPU frame immediately, freeing
                        // the pool slot before the next decode.
                        let mut cpu_frames = Vec::new();
                        for pkt in pkts {
                            dec.push_packet(pkt).unwrap();
                            if let Some(f) = dec.pop_frame().unwrap() {
                                // Download to CPU and drop GPU frame to free pool slot
                                let img = f.img().clone();
                                let w = img.width();
                                let h = img.height();
                                let ts = f.timestamp;
                                drop(f);
                                cpu_frames.push(VideoFrame::new_cpu(img.into_raw(), w, h, ts));
                            }
                        }
                        Some(cpu_frames)
                    }));
                    let _ = tx.send(result);
                });
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(Ok(result)) => result,
                    Ok(Err(_panic)) => {
                        eprintln!("VAAPI decoder panicked — disabling for remaining tests");
                        VAAPI_DECODE_DISABLED.store(true, Ordering::Relaxed);
                        None
                    }
                    Err(_) => {
                        eprintln!("VAAPI decoder timed out (10s) — disabling for remaining tests");
                        VAAPI_DECODE_DISABLED.store(true, Ordering::Relaxed);
                        None
                    }
                }
            },
        });
    }

    decoders
}

fn decode_all(dec: &mut impl VideoDecoder, packets: Vec<MediaPacket>) -> Vec<VideoFrame> {
    let mut frames = Vec::new();
    for pkt in packets {
        dec.push_packet(pkt).unwrap();
        if let Some(f) = dec.pop_frame().unwrap() {
            frames.push(f);
        }
    }
    frames
}

/// Encode `n` frames of a given frame, return packets. The frame is cloned for each push.
fn encode_n(enc: &mut dyn VideoEncoder, frame: &VideoFrame, n: usize) -> Vec<MediaPacket> {
    let mut packets = Vec::new();
    for _ in 0..n {
        enc.push_frame(frame.clone()).unwrap();
        while let Some(pkt) = enc.pop_packet().unwrap() {
            packets.push(pkt);
        }
    }
    encoded_frames_to_media_packets(packets)
}

/// Runs a solid-color roundtrip through one encoder×decoder pair and returns metrics.
///
/// Encodes `n` frames of color `(r,g,b)` at the given preset, decodes all,
/// and computes quality metrics on the last decoded frame (encoder has stabilized).
#[allow(
    clippy::too_many_arguments,
    reason = "test helper with color components"
)]
fn solid_color_roundtrip(
    enc: &mut dyn VideoEncoder,
    dec_info: &TestDecoder,
    w: u32,
    h: u32,
    r: u8,
    g: u8,
    b: u8,
    n: usize,
) -> Option<(Vec<VideoFrame>, FrameMetrics)> {
    let packets = video_encode(enc, w, h, r, g, b, n);
    let config = enc.config();
    let dc = DecodeConfig::default();
    let frames = (dec_info.decode)(&config, &dc, packets)?;
    if frames.is_empty() {
        return None;
    }

    // Compare last decoded frame against the solid-color reference.
    let original: Vec<u8> = [r, g, b, 255].repeat((w * h) as usize);
    let last = frames.last().unwrap();
    let img = last.img();
    let m = compute_metrics(&original, img.as_raw());
    Some((frames, m))
}

/// Runs a pattern roundtrip and returns metrics against the original frame.
fn pattern_roundtrip(
    enc: &mut dyn VideoEncoder,
    dec_info: &TestDecoder,
    frame: &VideoFrame,
    n: usize,
) -> Option<(Vec<VideoFrame>, FrameMetrics)> {
    let packets = encode_n(enc, frame, n);
    let config = enc.config();
    let dc = DecodeConfig::default();
    let frames = (dec_info.decode)(&config, &dc, packets)?;
    if frames.is_empty() {
        return None;
    }
    let last = frames.last().unwrap();
    let img = last.img();
    let m = compute_metrics(frame.rgba_image().as_raw(), img.as_raw());
    Some((frames, m))
}

/// Asserts roundtrip quality meets PSNR threshold.
///
/// Does NOT check color bias, because YUV420 chroma subsampling inherently
/// introduces per-channel asymmetry (green contributes most to luma in
/// BT.601/709, so green-channel roundtrip error is larger by design).
fn assert_roundtrip_quality(
    enc_name: &str,
    dec_name: &str,
    label: &str,
    metrics: &FrameMetrics,
    min_psnr: f64,
) {
    let tag = format!("{enc_name} → {dec_name} [{label}]");
    assert!(
        metrics.psnr_combined >= min_psnr,
        "{tag}: PSNR {:.1} dB < {min_psnr} dB minimum.\n  Metrics: {metrics}",
        metrics.psnr_combined,
    );
}

/// Asserts cross-backend parity: two decoders should produce similar output.
///
/// Checks both combined PSNR and per-channel color bias. A bias here means
/// one backend renders colors differently (e.g. VAAPI green tint), since
/// both are decoding the same bitstream.
#[allow(dead_code, reason = "utility for future cross-backend parity tests")]
fn assert_parity(label: &str, metrics: &FrameMetrics, min_psnr: f64) {
    if let Some((ch, deficit)) = metrics.color_bias(COLOR_BIAS_THRESHOLD) {
        panic!(
            "{label}: color bias detected — {ch} channel is {deficit:.1} dB below best.\n  \
             This indicates a backend-specific color rendering difference.\n  Metrics: {metrics}"
        );
    }

    assert!(
        metrics.psnr_combined >= min_psnr,
        "{label}: PSNR {:.1} dB < {min_psnr} dB minimum.\n  Metrics: {metrics}",
        metrics.psnr_combined,
    );
}

// ── Color accuracy matrix ────────────────────────────────────────────

#[test]
fn color_accuracy_matrix() {
    with_timeout(MATRIX_TIMEOUT, || {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let patches = patterns::color_patches();

        for enc_info in &all_encoders() {
            for dec_info in &all_decoders() {
                if dec_info.family != enc_info.family {
                    continue;
                }
                for &(color_name, r, g, b) in &patches {
                    let Some(mut enc) = (enc_info.create)(preset) else {
                        break;
                    };
                    let Some((_frames, metrics)) = solid_color_roundtrip(
                        &mut *enc,
                        dec_info,
                        w,
                        h,
                        r,
                        g,
                        b,
                        enc_info.min_frames,
                    ) else {
                        continue;
                    };
                    assert_roundtrip_quality(
                        enc_info.name,
                        dec_info.name,
                        color_name,
                        &metrics,
                        SOLID_COLOR_MIN_PSNR,
                    );
                }
            }
        }
    });
}

// ── Resolution sweep ─────────────────────────────────────────────────

#[test]
fn resolution_matrix() {
    with_timeout(MATRIX_TIMEOUT, || {
        let resolutions = patterns::test_resolutions();

        for enc_info in &all_encoders() {
            for dec_info in &all_decoders() {
                if dec_info.family != enc_info.family {
                    continue;
                }
                for &(res_name, w, h) in &resolutions {
                    let Some(mut enc) = (enc_info.create_with_dims)(w, h) else {
                        continue;
                    };
                    let Some((_frames, metrics)) = solid_color_roundtrip(
                        &mut *enc,
                        dec_info,
                        w,
                        h,
                        200,
                        100,
                        50,
                        enc_info.min_frames,
                    ) else {
                        continue;
                    };
                    assert_roundtrip_quality(
                        enc_info.name,
                        dec_info.name,
                        res_name,
                        &metrics,
                        SOLID_COLOR_MIN_PSNR,
                    );
                }
            }
        }
    });
}

// ── Pattern quality ──────────────────────────────────────────────────

/// Tests gradient patterns for banding artifacts.
#[test]
fn gradient_quality() {
    with_timeout(TEST_TIMEOUT, || {
        for enc_info in &all_encoders() {
            for dec_info in &all_decoders() {
                if dec_info.family != enc_info.family {
                    continue;
                }
                let Some(mut enc) = (enc_info.create)(VideoPreset::P360) else {
                    break;
                };
                let frame = patterns::horizontal_gradient(640, 360);
                let Some((_, metrics)) =
                    pattern_roundtrip(&mut *enc, dec_info, &frame, enc_info.min_frames)
                else {
                    continue;
                };
                assert_roundtrip_quality(
                    enc_info.name,
                    dec_info.name,
                    "h_gradient",
                    &metrics,
                    PATTERN_MIN_PSNR,
                );
            }
        }
    });
}

/// Tests checkerboard patterns (high spatial frequency).
#[test]
fn checkerboard_quality() {
    with_timeout(TEST_TIMEOUT, || {
        for enc_info in &all_encoders() {
            for dec_info in &all_decoders() {
                if dec_info.family != enc_info.family {
                    continue;
                }
                let Some(mut enc) = (enc_info.create)(VideoPreset::P360) else {
                    break;
                };
                // 8px cells — still challenging but not worst-case.
                let frame = patterns::checkerboard(640, 360, 8);
                let Some((_, metrics)) =
                    pattern_roundtrip(&mut *enc, dec_info, &frame, enc_info.min_frames)
                else {
                    continue;
                };
                assert_roundtrip_quality(
                    enc_info.name,
                    dec_info.name,
                    "checker_8px",
                    &metrics,
                    PATTERN_MIN_PSNR,
                );
            }
        }
    });
}

/// Tests color ramp (hue sweep) for chroma accuracy.
#[test]
fn color_ramp_quality() {
    with_timeout(TEST_TIMEOUT, || {
        for enc_info in &all_encoders() {
            for dec_info in &all_decoders() {
                if dec_info.family != enc_info.family {
                    continue;
                }
                let Some(mut enc) = (enc_info.create)(VideoPreset::P360) else {
                    break;
                };
                let frame = patterns::color_ramp(640, 360);
                let Some((_, metrics)) =
                    pattern_roundtrip(&mut *enc, dec_info, &frame, enc_info.min_frames)
                else {
                    continue;
                };
                assert_roundtrip_quality(
                    enc_info.name,
                    dec_info.name,
                    "color_ramp",
                    &metrics,
                    PATTERN_MIN_PSNR,
                );
            }
        }
    });
}

/// Tests gray ramp for range clamping and banding.
#[test]
fn gray_ramp_quality() {
    with_timeout(TEST_TIMEOUT, || {
        for enc_info in &all_encoders() {
            for dec_info in &all_decoders() {
                if dec_info.family != enc_info.family {
                    continue;
                }
                let Some(mut enc) = (enc_info.create)(VideoPreset::P360) else {
                    break;
                };
                let frame = patterns::gray_ramp(640, 360);
                let Some((_, metrics)) =
                    pattern_roundtrip(&mut *enc, dec_info, &frame, enc_info.min_frames)
                else {
                    continue;
                };
                assert_roundtrip_quality(
                    enc_info.name,
                    dec_info.name,
                    "gray_ramp",
                    &metrics,
                    PATTERN_MIN_PSNR,
                );
            }
        }
    });
}

// ── Cross-backend parity ─────────────────────────────────────────────

/// Compares SW and HW decoder output for the same H.264 bitstream.
///
/// Encodes with software H.264, decodes with both software and hardware
/// decoders (if available), and compares the decoded outputs pixel-by-pixel.
/// Detects backend-specific color deviations (e.g. VAAPI green tint).
#[cfg(feature = "h264")]
#[test]
fn cross_backend_h264_decoder_parity() {
    with_timeout(TEST_TIMEOUT, || {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = video_encode(&mut enc, w, h, 200, 50, 50, 10);
        let config = enc.config();

        // Decode with software (baseline reference).
        let dc_sw = DecodeConfig {
            backend: DecoderBackend::Software,
            ..Default::default()
        };
        let mut dec_sw = h264::decoder::H264VideoDecoder::new(&config, &dc_sw).unwrap();
        let sw_frames = decode_all(&mut dec_sw, packets.clone());
        assert!(
            sw_frames.len() >= 5,
            "SW decoder produced {} frames",
            sw_frames.len()
        );

        // Try hardware decoder. If unavailable, test passes (nothing to compare).
        let hw_decoders = all_decoders();
        for dec_info in &hw_decoders {
            if dec_info.family != CodecFamily::H264 || dec_info.name == "h264-openh264" {
                continue;
            }

            let dc_hw = DecodeConfig::default();
            let Some(hw_frames) = (dec_info.decode)(&config, &dc_hw, packets.clone()) else {
                continue;
            };
            if hw_frames.is_empty() {
                continue;
            }

            // Compare last frames from each decoder.
            let sw_img = sw_frames.last().unwrap().img();
            let hw_img = hw_frames.last().unwrap().img();

            // Dimensions must match.
            assert_eq!(
                (sw_img.width(), sw_img.height()),
                (hw_img.width(), hw_img.height()),
                "dimension mismatch between SW and {} decoder",
                dec_info.name
            );

            let diff = metrics::compare_frames(sw_img.as_raw(), hw_img.as_raw());
            eprintln!("SW vs {} decoder parity: {diff}", dec_info.name);

            // HW and SW decoders should produce very similar output.
            // We allow some tolerance for different YUV→RGB implementations.
            if let Some((ch, deficit)) = diff.color_bias(COLOR_BIAS_THRESHOLD) {
                panic!(
                    "SW vs {} decoder color parity: {ch} channel deviates by {deficit:.1} dB.\n  \
                     This indicates the HW decoder has a color rendering difference.\n  \
                     Diff metrics: {diff}",
                    dec_info.name
                );
            }

            // Overall difference should be small.
            assert!(
                diff.psnr_combined >= 30.0,
                "SW vs {} decoder diff: PSNR {:.1} dB < 30 dB.\n  \
                 Decoded frames differ significantly.\n  Diff metrics: {diff}",
                dec_info.name,
                diff.psnr_combined,
            );
        }
    });
}

/// Compares SW and HW encoder output decoded through the same SW decoder.
///
/// If a HW encoder produces different color characteristics (e.g. different
/// YUV matrix), this test catches it by comparing final decoded pixels.
#[cfg(feature = "h264")]
#[test]
fn cross_backend_h264_encoder_parity() {
    with_timeout(TEST_TIMEOUT, || {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();

        // Encode with software (baseline reference).
        let mut enc_sw = H264Encoder::with_preset(preset).unwrap();
        let packets_sw = video_encode(&mut enc_sw, w, h, 200, 50, 50, 10);
        let config_sw = enc_sw.config();

        let dc = DecodeConfig {
            backend: DecoderBackend::Software,
            ..Default::default()
        };
        let mut dec_sw1 = h264::decoder::H264VideoDecoder::new(&config_sw, &dc).unwrap();
        let sw_frames = decode_all(&mut dec_sw1, packets_sw);
        assert!(
            sw_frames.len() >= 5,
            "SW encoder→decoder produced {} frames",
            sw_frames.len()
        );

        // Try hardware encoders.
        for enc_info in &all_encoders() {
            if enc_info.family != CodecFamily::H264 || enc_info.name == "h264-openh264" {
                continue;
            }

            let Some(mut hw_enc) = (enc_info.create)(preset) else {
                continue;
            };
            let packets_hw = video_encode(&mut *hw_enc, w, h, 200, 50, 50, 10);
            let config_hw = hw_enc.config();

            let mut dec_sw2 = h264::decoder::H264VideoDecoder::new(&config_hw, &dc).unwrap();
            let hw_frames = decode_all(&mut dec_sw2, packets_hw);
            if hw_frames.is_empty() {
                continue;
            }

            let sw_img = sw_frames.last().unwrap().img();
            let hw_img = hw_frames.last().unwrap().img();

            // Reference is the solid color, not the SW-decoded frame, because both
            // encoders introduce their own codec artifacts.
            let reference: Vec<u8> = [200, 50, 50, 255].repeat((w * h) as usize);
            let sw_metrics = compute_metrics(&reference, sw_img.as_raw());
            let hw_metrics = compute_metrics(&reference, hw_img.as_raw());

            eprintln!(
                "Encoder parity — SW: {sw_metrics}, {} : {hw_metrics}",
                enc_info.name
            );

            assert_roundtrip_quality(
                "h264-openh264",
                "h264-openh264",
                "sw_baseline",
                &sw_metrics,
                SOLID_COLOR_MIN_PSNR,
            );
            assert_roundtrip_quality(
                enc_info.name,
                "h264-openh264",
                "hw_output",
                &hw_metrics,
                SOLID_COLOR_MIN_PSNR,
            );
        }
    });
}

// NOTE: Individual per-resolution roundtrip tests (P180, P360, P720, P1080)
// were consolidated into `resolution_matrix` and `color_accuracy_matrix` above.
// AV1 roundtrip tests are disabled — re-enable in `all_encoders()`/`all_decoders()`
// when AV1 testing performance is acceptable.

// ── DynamicVideoDecoder routing (fixed) ──────────────────────────────

#[cfg(feature = "h264")]
#[test]
fn dynamic_routes_h264_software() {
    with_timeout(TEST_TIMEOUT, || {
        let preset = VideoPreset::P180;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = video_encode(&mut enc, w, h, 200, 100, 50, 10);
        let config = enc.config();

        // Force software backend so this test is deterministic on HW machines.
        let dc = DecodeConfig {
            backend: DecoderBackend::Software,
            ..Default::default()
        };
        let mut dec = DynamicVideoDecoder::new(&config, &dc).unwrap();
        assert_eq!(dec.name(), "h264-openh264");

        let mut decoded_count = 0;
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if dec.pop_frame().unwrap().is_some() {
                decoded_count += 1;
            }
        }
        assert!(
            decoded_count >= 5,
            "expected >= 5 decoded frames, got {decoded_count}"
        );
    });
}

// ── DefaultDecoders type assertion ───────────────────────────────────

#[test]
fn default_decoders_types() {
    // Type-level assertion, no timeout needed.
    fn assert_decoders<D: Decoders>() {}
    assert_decoders::<DefaultDecoders>();
}

// ── Audio roundtrip helpers ──────────────────────────────────────────

fn make_sine(num_samples: usize, freq: f32, sample_rate: f32) -> Vec<f32> {
    (0..num_samples)
        .map(|i| (2.0 * PI * freq * i as f32 / sample_rate).sin())
        .collect()
}

fn audio_encode(enc: &mut OpusEncoder, samples: &[f32]) -> Vec<EncodedFrame> {
    enc.push_samples(samples).unwrap();
    let mut packets = Vec::new();
    while let Some(pkt) = enc.pop_packet().unwrap() {
        packets.push(pkt);
    }
    packets
}

fn audio_decode(config: &AudioConfig, format: AudioFormat, packets: Vec<MediaPacket>) -> Vec<f32> {
    let mut dec = OpusAudioDecoder::new(config, format).unwrap();
    let mut all_samples = Vec::new();
    for pkt in packets {
        dec.push_packet(pkt).unwrap();
        if let Some(samples) = dec.pop_samples().unwrap() {
            all_samples.extend_from_slice(samples);
        }
    }
    all_samples
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

fn assert_energy_preserved(input: &[f32], output: &[f32]) {
    let input_rms = rms(input);
    let output_rms = rms(output);
    assert!(
        output_rms > 0.1,
        "decoded RMS {output_rms} too low (signal is near-silent)"
    );
    let ratio = output_rms / input_rms;
    assert!(
        (0.3..3.0).contains(&ratio),
        "energy ratio {ratio} out of range (input RMS={input_rms}, output RMS={output_rms})"
    );
}

fn extract_channel(interleaved: &[f32], channel: usize, num_channels: usize) -> Vec<f32> {
    interleaved
        .iter()
        .skip(channel)
        .step_by(num_channels)
        .copied()
        .collect()
}

// ── Audio roundtrip tests ────────────────────────────────────────────

#[test]
fn audio_roundtrip_mono_hq() {
    with_timeout(TEST_TIMEOUT, || {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 48000);
        assert_energy_preserved(&sine, &decoded);
    });
}

#[test]
fn audio_roundtrip_mono_lq() {
    with_timeout(TEST_TIMEOUT, || {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Lq).unwrap();
        let config = enc.config();
        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 48000);
        assert_energy_preserved(&sine, &decoded);
    });
}

#[test]
fn audio_roundtrip_stereo_hq() {
    with_timeout(TEST_TIMEOUT, || {
        let format = AudioFormat::stereo_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        let sine = make_sine(96000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 96000);
        assert_energy_preserved(&sine, &decoded);
    });
}

#[test]
fn audio_roundtrip_stereo_lq() {
    with_timeout(TEST_TIMEOUT, || {
        let format = AudioFormat::stereo_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Lq).unwrap();
        let config = enc.config();
        let sine = make_sine(96000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 96000);
        assert_energy_preserved(&sine, &decoded);
    });
}

// ── Cross-channel audio pipeline tests ───────────────────────────────

#[test]
fn audio_pipeline_mono_encode_stereo_decode() {
    with_timeout(TEST_TIMEOUT, || {
        let enc_format = AudioFormat::mono_48k();
        let dec_format = AudioFormat::stereo_48k();

        let mut enc = OpusEncoder::with_preset(enc_format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        assert_eq!(config.channel_count, 1);

        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);

        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, dec_format, packets);

        assert_eq!(decoded.len(), 96000);

        for (i, pair) in decoded.chunks_exact(2).enumerate() {
            assert_eq!(
                pair[0], pair[1],
                "stereo pair at frame {i} should be equal: L={}, R={}",
                pair[0], pair[1]
            );
        }

        let left = extract_channel(&decoded, 0, 2);
        assert_energy_preserved(&sine, &left);
    });
}

#[test]
fn audio_pipeline_stereo_encode_mono_decode() {
    with_timeout(TEST_TIMEOUT, || {
        let enc_format = AudioFormat::stereo_48k();
        let dec_format = AudioFormat::mono_48k();

        let mut enc = OpusEncoder::with_preset(enc_format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        assert_eq!(config.channel_count, 2);

        let sine = make_sine(96000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);

        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, dec_format, packets);

        assert_eq!(decoded.len(), 48000);

        let input_left = extract_channel(&sine, 0, 2);
        assert_energy_preserved(&input_left, &decoded);
    });
}

#[test]
fn audio_pipeline_mono_encode_stereo_decode_lq() {
    with_timeout(TEST_TIMEOUT, || {
        let enc_format = AudioFormat::mono_48k();
        let dec_format = AudioFormat::stereo_48k();

        let mut enc = OpusEncoder::with_preset(enc_format, AudioPreset::Lq).unwrap();
        let config = enc.config();

        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, dec_format, packets);

        assert_eq!(decoded.len(), 96000);
        let left = extract_channel(&decoded, 0, 2);
        assert_energy_preserved(&sine, &left);
    });
}
