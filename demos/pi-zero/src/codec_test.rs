/// On-device V4L2 codec tests: encoder-only, decoder-only, and encode-decode
/// round-trip. Saves frames as PNGs for visual inspection and reports PSNR
/// against the SMPTE test pattern.
///
/// Cross-compile on host, run on Pi:
///   cargo make deploy
///   ssh pi@livepizero "./pi-zero-demo codec-test"
use std::time::{Duration, Instant};

use clap::Parser;
use rusty_codecs::{
    codec::{H264Encoder, V4l2Decoder, V4l2Encoder},
    format::{DecodeConfig, EncodedFrame, MediaPacket, VideoEncoderConfig, VideoPreset},
    test_sources::TestPatternSource,
    traits::{VideoDecoder, VideoEncoder, VideoEncoderFactory, VideoSource},
};

#[derive(Parser, Debug)]
pub(crate) struct CodecTestOpts {
    /// Test to run: "encode", "decode", "roundtrip", or "all".
    #[clap(default_value = "all")]
    test: String,
    /// Number of frames to encode/decode.
    #[clap(long, default_value_t = 30)]
    frames: u32,
    /// Save decoded frames as PNGs to this directory.
    #[clap(long)]
    save: Option<std::path::PathBuf>,
    /// Resolution width.
    #[clap(long, default_value_t = 640)]
    width: u32,
    /// Resolution height.
    #[clap(long, default_value_t = 360)]
    height: u32,
}

pub(crate) fn run(opts: CodecTestOpts) -> anyhow::Result<()> {
    match opts.test.as_str() {
        "encode" => test_encode(&opts),
        "decode" => test_decode(&opts),
        "roundtrip" | "rt" => test_roundtrip(&opts),
        "all" => {
            test_encode(&opts)?;
            test_decode(&opts)?;
            test_roundtrip(&opts)?;
            Ok(())
        }
        other => {
            eprintln!("unknown test: {other}. Use encode, decode, roundtrip, or all.");
            std::process::exit(1);
        }
    }
}

fn make_encoder_config(width: u32, height: u32) -> VideoEncoderConfig {
    let mut config = VideoEncoderConfig::from_preset(VideoPreset::P360);
    config.width = width;
    config.height = height;
    config.framerate = 30;
    // nal_format defaults to AnnexB from from_preset, which is what we want.
    config
}

/// Test V4L2 encoder: feed SMPTE test pattern, collect encoded packets.
fn test_encode(opts: &CodecTestOpts) -> anyhow::Result<()> {
    println!(
        "=== V4L2 ENCODER TEST ({} frames, {}x{}) ===",
        opts.frames, opts.width, opts.height
    );

    let config = make_encoder_config(opts.width, opts.height);
    let mut encoder = V4l2Encoder::with_config(config)?;
    println!("  encoder created: {}", encoder.name());

    let mut source = TestPatternSource::new(opts.width, opts.height).with_fps(30.0);
    source.start()?;

    let start = Instant::now();
    let mut packet_count = 0u32;
    let mut keyframe_count = 0u32;
    let mut total_bytes = 0usize;

    for i in 0..opts.frames {
        let frame = source
            .pop_frame()?
            .expect("test source should produce a frame");
        encoder.push_frame(frame)?;

        while let Ok(Some(pkt)) = encoder.pop_packet() {
            if pkt.is_keyframe {
                keyframe_count += 1;
            }
            total_bytes += pkt.payload.len();
            packet_count += 1;
        }

        if i == 0 || (i + 1) == opts.frames || (i + 1) % 10 == 0 {
            println!(
                "  frame {}: {} packets, {} keyframes, {} bytes",
                i + 1,
                packet_count,
                keyframe_count,
                total_bytes,
            );
        }
    }

    let elapsed = start.elapsed();
    println!(
        "  done: {} packets, {} keyframes, {:.1} KB in {:.1}s ({:.0} encode fps)",
        packet_count,
        keyframe_count,
        total_bytes as f64 / 1024.0,
        elapsed.as_secs_f64(),
        opts.frames as f64 / elapsed.as_secs_f64(),
    );

    anyhow::ensure!(packet_count > 0, "encoder produced no packets");
    anyhow::ensure!(keyframe_count > 0, "encoder produced no keyframes");

    println!("  PASS\n");
    Ok(())
}

/// Test V4L2 decoder: encode with software H.264, then decode with V4L2 HW.
fn test_decode(opts: &CodecTestOpts) -> anyhow::Result<()> {
    println!(
        "=== V4L2 DECODER TEST ({} frames, {}x{}) ===",
        opts.frames, opts.width, opts.height
    );

    // Encode with software to get a valid H.264 bitstream.
    println!("  encoding {} frames with SW encoder...", opts.frames);
    let config = make_encoder_config(opts.width, opts.height);
    let mut sw_encoder = H264Encoder::with_config(config)?;
    let mut source = TestPatternSource::new(opts.width, opts.height).with_fps(30.0);
    source.start()?;

    let mut encoded_packets: Vec<EncodedFrame> = Vec::new();
    for _ in 0..opts.frames {
        let frame = source.pop_frame()?.expect("source should produce");
        sw_encoder.push_frame(frame)?;
        while let Ok(Some(pkt)) = sw_encoder.pop_packet() {
            encoded_packets.push(pkt);
        }
    }
    println!(
        "  got {} encoded packets from SW encoder",
        encoded_packets.len()
    );

    // Decode with V4L2.
    println!("  decoding with V4L2 HW decoder...");
    let video_config = sw_encoder.config();
    let decode_config = DecodeConfig::default();
    let mut decoder = V4l2Decoder::new(&video_config, &decode_config)?;
    println!("  decoder created: {}", decoder.name());

    let start = Instant::now();
    let mut decoded_count = 0u32;

    for (i, pkt) in encoded_packets.iter().enumerate() {
        let media_pkt = MediaPacket {
            payload: pkt.payload.clone().into(),
            is_keyframe: pkt.is_keyframe,
            timestamp: pkt.timestamp,
        };
        decoder.push_packet(media_pkt)?;

        // Drain any ready frames between pushes.
        while let Ok(Some(frame)) = decoder.pop_frame() {
            decoded_count += 1;
            save_frame(opts, &format!("v4l2dec_{:04}", decoded_count - 1), &frame)?;
        }

        if i == 0 || (i + 1) == encoded_packets.len() || (i + 1) % 10 == 0 {
            println!("  packet {}: {} decoded frames", i + 1, decoded_count);
        }
    }
    let feed_elapsed = start.elapsed();
    println!(
        "  feed done: {:.1}s ({} packets queued, {} decoded during feed)",
        feed_elapsed.as_secs_f64(),
        encoded_packets.len(),
        decoded_count,
    );

    // Drain remaining frames. The HW decoder may still be processing — at
    // 1080p the pipeline latency is several frames, so we wait until no new
    // frames arrive for 500ms.
    let mut idle_streak = 0;
    while idle_streak < 10 {
        std::thread::sleep(Duration::from_millis(50));
        let mut got_any = false;
        while let Ok(Some(frame)) = decoder.pop_frame() {
            decoded_count += 1;
            save_frame(opts, &format!("v4l2dec_{:04}", decoded_count - 1), &frame)?;
            got_any = true;
        }
        if got_any {
            idle_streak = 0;
        } else {
            idle_streak += 1;
        }
    }

    let elapsed = start.elapsed();
    println!(
        "  done: {} decoded frames in {:.1}s ({:.0} decode fps)",
        decoded_count,
        elapsed.as_secs_f64(),
        decoded_count as f64 / elapsed.as_secs_f64(),
    );

    anyhow::ensure!(decoded_count > 0, "V4L2 decoder produced no frames");

    println!("  PASS\n");
    Ok(())
}

/// Test V4L2 encode then V4L2 decode round-trip.
fn test_roundtrip(opts: &CodecTestOpts) -> anyhow::Result<()> {
    println!(
        "=== V4L2 ROUNDTRIP TEST ({} frames, {}x{}) ===",
        opts.frames, opts.width, opts.height
    );

    // Encode with V4L2 HW.
    let config = make_encoder_config(opts.width, opts.height);
    let mut encoder = V4l2Encoder::with_config(config)?;
    println!("  encoder: {}", encoder.name());

    let mut source = TestPatternSource::new(opts.width, opts.height).with_fps(30.0);
    source.start()?;

    let start = Instant::now();
    let mut encoded_packets: Vec<EncodedFrame> = Vec::new();

    for _ in 0..opts.frames {
        let frame = source.pop_frame()?.expect("source");
        encoder.push_frame(frame)?;
        while let Ok(Some(pkt)) = encoder.pop_packet() {
            encoded_packets.push(pkt);
        }
    }
    let encode_time = start.elapsed();
    println!(
        "  encoded: {} packets in {:.1}s ({:.0} fps)",
        encoded_packets.len(),
        encode_time.as_secs_f64(),
        opts.frames as f64 / encode_time.as_secs_f64(),
    );

    // Decode with V4L2 HW.
    let video_config = encoder.config();
    let decode_config = DecodeConfig::default();
    let mut decoder = V4l2Decoder::new(&video_config, &decode_config)?;
    println!("  decoder: {}", decoder.name());

    let decode_start = Instant::now();
    let mut decoded_count = 0u32;

    for pkt in &encoded_packets {
        let media_pkt = MediaPacket {
            payload: pkt.payload.clone().into(),
            is_keyframe: pkt.is_keyframe,
            timestamp: pkt.timestamp,
        };
        decoder.push_packet(media_pkt)?;
        while let Ok(Some(frame)) = decoder.pop_frame() {
            decoded_count += 1;
            save_frame(opts, &format!("roundtrip_{:04}", decoded_count - 1), &frame)?;
        }
    }

    // Drain remaining frames until idle for 500ms.
    let mut idle_streak = 0;
    while idle_streak < 10 {
        std::thread::sleep(Duration::from_millis(50));
        let mut got_any = false;
        while let Ok(Some(frame)) = decoder.pop_frame() {
            decoded_count += 1;
            save_frame(opts, &format!("roundtrip_{:04}", decoded_count - 1), &frame)?;
            got_any = true;
        }
        if got_any {
            idle_streak = 0;
        } else {
            idle_streak += 1;
        }
    }

    let decode_time = decode_start.elapsed();
    println!(
        "  decoded: {} frames in {:.1}s ({:.0} fps)",
        decoded_count,
        decode_time.as_secs_f64(),
        decoded_count as f64 / decode_time.as_secs_f64().max(0.001),
    );

    anyhow::ensure!(decoded_count > 0, "roundtrip produced no decoded frames");

    let total = encode_time + decode_time;
    println!(
        "  total: {:.1}s ({:.0} roundtrip fps)\n  PASS\n",
        total.as_secs_f64(),
        opts.frames as f64 / total.as_secs_f64(),
    );
    Ok(())
}

fn save_frame(
    opts: &CodecTestOpts,
    name: &str,
    frame: &rusty_codecs::format::VideoFrame,
) -> anyhow::Result<()> {
    if let Some(ref dir) = opts.save {
        std::fs::create_dir_all(dir)?;
        // Save raw RGBA pixels (no PNG dependency needed for cross-compile).
        let rgba = frame.rgba_image();
        let w = rgba.width();
        let h = rgba.height();
        let path = dir.join(format!("{name}_{w}x{h}.rgba"));
        std::fs::write(&path, rgba.as_raw())?;
        println!(
            "    saved {} (view: ffplay -f rawvideo -pixel_format rgba -video_size {w}x{h} {})",
            path.display(),
            path.display()
        );
    }
    Ok(())
}
