//! `irl record` - subscribe to a remote broadcast and write encoded packets to file.
//!
//! Unlike `play`, this command runs headless (no GUI, no decoding). It captures
//! raw encoded packets from the transport layer and writes them directly to disk,
//! avoiding the decode-then-re-encode overhead.
//!
//! Current format: `raw` - writes separate files per track. Video gets an
//! extension matching the codec (.h264, .av1), audio gets .opus. The H.264
//! output uses Annex B framing (start codes), so it is directly playable with
//! ffplay/mpv. Audio packets are written with a 4-byte big-endian length
//! prefix so individual Opus frames can be recovered on playback.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Buf;
use hang::catalog::{AudioCodec, VideoCodec, VideoConfig};
use moq_media::{codec::h264::annexb, format::Quality, transport::PacketSource};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use crate::{args::RecordArgs, transport::setup_live};

/// Entry point for the `irl record` command.
pub fn run(args: RecordArgs, rt: &tokio::runtime::Runtime) -> n0_error::Result {
    let source = args.source()?;
    let output = args.output.clone();

    rt.block_on(async {
        let live = setup_live(false).await?;
        println!(
            "subscribing to {} via {} candidate source(s) ...",
            source.broadcast_name,
            source.sources.len()
        );
        let sub = live.subscribe(source.sources, source.broadcast_name);
        let active = sub.ready().await?;
        info!("session established");

        let broadcast = active.broadcast().clone();
        let catalog = broadcast.catalog();
        println!(
            "catalog: {} video renditions, {} audio renditions",
            catalog.video.renditions.len(),
            catalog.audio.renditions.len()
        );

        // Pick best-quality video and audio renditions.
        let video_name = catalog.select_video_rendition(Quality::Highest).ok();
        let audio_name = catalog.select_audio_rendition(Quality::Highest).ok();

        if video_name.is_none() && audio_name.is_none() {
            anyhow::bail!("broadcast has no video or audio renditions to record");
        }

        // Shared progress counters.
        let video_bytes = Arc::new(AtomicU64::new(0));
        let audio_bytes = Arc::new(AtomicU64::new(0));
        let video_frames = Arc::new(AtomicU64::new(0));
        let audio_frames = Arc::new(AtomicU64::new(0));

        // Track output paths for the post-recording hint.
        let mut video_path: Option<PathBuf> = None;
        let mut audio_path: Option<PathBuf> = None;

        // Spawn video recording task.
        let video_task = if let Some(ref name) = video_name {
            let (source, config) = broadcast.raw_video_track(name)?;
            let ext = video_codec_extension(&config.codec);
            let path = output.with_extension(ext);
            println!("video: {name} ({}) -> {}", config.codec, path.display());
            video_path = Some(path.clone());
            let vb = video_bytes.clone();
            let vf = video_frames.clone();
            let h264_state = H264AnnexBState::from_config(&config);
            Some(tokio::spawn(async move {
                record_video_track(source, &path, vb, vf, h264_state).await
            }))
        } else {
            println!("no video renditions, skipping video");
            None
        };

        // Spawn audio recording task.
        let audio_task = if let Some(ref name) = audio_name {
            let (source, config) = broadcast.raw_audio_track(name)?;
            let ext = audio_codec_extension(&config.codec);
            let path = output.with_extension(ext);
            println!("audio: {name} ({}) -> {}", config.codec, path.display());
            audio_path = Some(path.clone());
            let ab = audio_bytes.clone();
            let af = audio_frames.clone();
            Some(tokio::spawn(async move {
                record_raw_track(source, &path, ab, af).await
            }))
        } else {
            println!("no audio renditions, skipping audio");
            None
        };

        // Progress reporting + Ctrl-C handling.
        let shutdown = broadcast.shutdown_token();
        let progress_shutdown = shutdown.clone();
        let pvb = video_bytes.clone();
        let pab = audio_bytes.clone();
        let pvf = video_frames.clone();
        let paf = audio_frames.clone();

        let progress_task = tokio::spawn(async move {
            let start = Instant::now();
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.tick().await; // skip immediate tick
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let elapsed = start.elapsed();
                        let vb = pvb.load(Ordering::Relaxed);
                        let ab = pab.load(Ordering::Relaxed);
                        let vf = pvf.load(Ordering::Relaxed);
                        let af = paf.load(Ordering::Relaxed);
                        println!(
                            "[{:.1}s] video: {} frames ({}) | audio: {} frames ({})",
                            elapsed.as_secs_f64(),
                            vf,
                            format_bytes(vb),
                            af,
                            format_bytes(ab),
                        );
                    }
                    _ = progress_shutdown.cancelled() => break,
                }
            }
        });

        // Wait for Ctrl-C to stop recording.
        println!("recording... press Ctrl+C to stop");
        tokio::signal::ctrl_c().await?;
        println!("\nstopping...");

        // Shut down the broadcast (closes packet sources).
        broadcast.shutdown();

        // Wait for recording tasks to finish.
        if let Some(task) = video_task
            && let Err(e) = task.await?
        {
            warn!("video recording ended with error: {e:#}");
        }
        if let Some(task) = audio_task
            && let Err(e) = task.await?
        {
            warn!("audio recording ended with error: {e:#}");
        }

        progress_task.abort();

        // Final summary.
        let vb = video_bytes.load(Ordering::Relaxed);
        let ab = audio_bytes.load(Ordering::Relaxed);
        let vf = video_frames.load(Ordering::Relaxed);
        let af = audio_frames.load(Ordering::Relaxed);
        println!(
            "done: video {} frames ({}), audio {} frames ({})",
            vf,
            format_bytes(vb),
            af,
            format_bytes(ab),
        );

        print_remux_hint(&video_path, &audio_path, &output);

        if let Some(active) = sub.active().await {
            active.session().close(0, b"bye");
        }
        live.shutdown().await;
        anyhow::Ok(())
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// H.264 AVCC-to-Annex-B conversion
// ---------------------------------------------------------------------------

/// Holds Annex B SPS/PPS extracted from the avcC description, if the source
/// uses non-inline parameter sets (avc1). For inline sources (avc3) or
/// non-H.264 codecs this is `None` and packets pass through unmodified.
pub(crate) struct H264AnnexBState {
    /// Annex B SPS/PPS to prepend before each keyframe. `None` for inline
    /// (avc3) sources where SPS/PPS appear in the bitstream itself.
    sps_pps_annex_b: Option<Vec<u8>>,
}

impl H264AnnexBState {
    /// Builds the state from a catalog [`VideoConfig`]. Returns `None` when
    /// the codec is not H.264 (meaning no conversion is needed).
    pub(crate) fn from_config(config: &VideoConfig) -> Option<Self> {
        let VideoCodec::H264(ref h264) = config.codec else {
            return None;
        };

        // avc1: SPS/PPS live in the description (avcC record).
        // avc3: SPS/PPS are inline in keyframes; we still need to convert
        //       length-prefixed NALUs to Annex B start codes.
        let sps_pps_annex_b = if !h264.inline {
            config
                .description
                .as_ref()
                .and_then(|desc| annexb::avcc_to_annex_b(desc))
        } else {
            None
        };

        if !h264.inline && sps_pps_annex_b.is_none() {
            warn!(
                "H.264 avc1 source has no description - SPS/PPS will be missing from output; \
                 players may fail to decode"
            );
        }

        Some(Self { sps_pps_annex_b })
    }

    /// Converts a packet payload from length-prefixed NALUs to Annex B.
    /// For keyframes with avc1, prepends the SPS/PPS.
    fn convert(&self, payload: &[u8], is_keyframe: bool) -> Vec<u8> {
        let annex_b = annexb::length_prefixed_to_annex_b(payload);

        if is_keyframe && let Some(ref sps_pps) = self.sps_pps_annex_b {
            let mut out = Vec::with_capacity(sps_pps.len() + annex_b.len());
            out.extend_from_slice(sps_pps);
            out.extend_from_slice(&annex_b);
            return out;
        }

        annex_b
    }
}

// ---------------------------------------------------------------------------
// Track recording
// ---------------------------------------------------------------------------

/// Records a video track, converting H.264 AVCC payloads to Annex B when needed.
///
/// Non-H.264 codecs pass through without conversion.
pub(crate) async fn record_video_track(
    mut source: impl PacketSource,
    path: &PathBuf,
    bytes_written: Arc<AtomicU64>,
    frames_written: Arc<AtomicU64>,
    h264_state: Option<H264AnnexBState>,
) -> anyhow::Result<()> {
    let file = tokio::fs::File::create(path).await?;
    let mut writer = tokio::io::BufWriter::new(file);

    if h264_state.is_some() {
        debug!(path = %path.display(), "recording H.264 with AVCC-to-Annex-B conversion");
    }

    info!(path = %path.display(), "recording started");

    loop {
        match source.read().await {
            Ok(Some(packet)) => {
                let written = if let Some(ref state) = h264_state {
                    // Collect the scatter-gather payload into contiguous bytes
                    // so we can parse length-prefixed NALUs.
                    let mut payload = packet.payload;
                    let contiguous = payload.copy_to_bytes(payload.remaining());
                    let converted = state.convert(&contiguous, packet.is_keyframe);
                    let len = converted.len() as u64;
                    writer.write_all(&converted).await?;
                    len
                } else {
                    let mut written = 0u64;
                    for chunk in &packet.payload {
                        writer.write_all(chunk).await?;
                        written += chunk.len() as u64;
                    }
                    written
                };
                bytes_written.fetch_add(written, Ordering::Relaxed);
                frames_written.fetch_add(1, Ordering::Relaxed);
            }
            Ok(None) => {
                info!(path = %path.display(), "track ended");
                break;
            }
            Err(e) => {
                warn!(path = %path.display(), err = %e, "read error, stopping track");
                break;
            }
        }
    }

    writer.flush().await?;
    info!(
        path = %path.display(),
        frames = frames_written.load(Ordering::Relaxed),
        bytes = bytes_written.load(Ordering::Relaxed),
        "recording finished"
    );
    Ok(())
}

/// Records a non-video track by writing raw payloads directly.
pub(crate) async fn record_raw_track(
    mut source: impl PacketSource,
    path: &PathBuf,
    bytes_written: Arc<AtomicU64>,
    frames_written: Arc<AtomicU64>,
) -> anyhow::Result<()> {
    let file = tokio::fs::File::create(path).await?;
    let mut writer = tokio::io::BufWriter::new(file);
    info!(path = %path.display(), "recording started");

    loop {
        match source.read().await {
            Ok(Some(packet)) => {
                let mut written = 0u64;
                for chunk in &packet.payload {
                    writer.write_all(chunk).await?;
                    written += chunk.len() as u64;
                }
                bytes_written.fetch_add(written, Ordering::Relaxed);
                frames_written.fetch_add(1, Ordering::Relaxed);
            }
            Ok(None) => {
                info!(path = %path.display(), "track ended");
                break;
            }
            Err(e) => {
                warn!(path = %path.display(), err = %e, "read error, stopping track");
                break;
            }
        }
    }

    writer.flush().await?;
    info!(
        path = %path.display(),
        frames = frames_written.load(Ordering::Relaxed),
        bytes = bytes_written.load(Ordering::Relaxed),
        "recording finished"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Post-recording hints
// ---------------------------------------------------------------------------

/// Prints ffmpeg commands for remuxing raw recordings into a playable container.
fn print_remux_hint(
    video_path: &Option<PathBuf>,
    audio_path: &Option<PathBuf>,
    output_base: &std::path::Path,
) {
    let mp4_path = output_base.with_extension("mp4");
    println!();

    match (video_path, audio_path) {
        (Some(v), Some(a)) => {
            println!("to mux into a playable mp4:");
            println!(
                "  ffmpeg -i {} -i {} -c copy {}",
                v.display(),
                a.display(),
                mp4_path.display()
            );
            println!();
            println!(
                "the .h264 file is playable directly (ffplay {}).",
                v.display()
            );
            println!("the .opus file uses 4-byte big-endian length-delimited framing.");
            println!("to extract packets and wrap in ogg, use a script or custom demuxer.");
        }
        (Some(v), None) => {
            println!(
                "the .h264 file is playable directly (ffplay {}).",
                v.display()
            );
        }
        (None, Some(_a)) => {
            println!("the .opus file uses 4-byte big-endian length-delimited framing.");
            println!("to extract packets and wrap in ogg, use a script or custom demuxer.");
        }
        (None, None) => {}
    }
}

// ---------------------------------------------------------------------------
// Codec → extension mappings
// ---------------------------------------------------------------------------

/// Maps a video codec to a file extension for raw bitstream output.
pub(crate) fn video_codec_extension(codec: &VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264(_) => "h264",
        VideoCodec::H265(_) => "h265",
        VideoCodec::AV1(_) => "av1",
        VideoCodec::VP9(_) => "ivf",
        VideoCodec::VP8 => "ivf",
        VideoCodec::Unknown(_) | _ => "bin",
    }
}

/// Maps an audio codec to a file extension for raw output.
pub(crate) fn audio_codec_extension(codec: &AudioCodec) -> &'static str {
    match codec {
        AudioCodec::Opus => "opus",
        AudioCodec::AAC(_) => "aac",
        AudioCodec::Unknown(_) | _ => "bin",
    }
}

/// Formats a byte count as a human-readable string.
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal avcC record from SPS and PPS for testing.
    fn build_test_avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
        let mut avcc = vec![
            1,    // configurationVersion
            0x42, // profile
            0xC0, // compat
            0x1E, // level
            0xFF, // lengthSizeMinusOne=3 | reserved
            0xE1, // numSPS=1 | reserved
        ];
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(sps);
        avcc.push(1); // numPPS=1
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(pps);
        avcc
    }

    #[test]
    fn h264_state_prepends_sps_pps_on_keyframe() {
        let sps = [0x67, 0x42, 0xC0, 0x1E];
        let pps = [0x68, 0xCE, 0x38, 0x80];
        let avcc = build_test_avcc(&sps, &pps);

        let state = H264AnnexBState {
            sps_pps_annex_b: annexb::avcc_to_annex_b(&avcc),
        };

        // A keyframe payload with one IDR NAL (length-prefixed).
        let idr = [0x65, 0x88, 0x84];
        let mut payload = Vec::new();
        payload.extend_from_slice(&(idr.len() as u32).to_be_bytes());
        payload.extend_from_slice(&idr);

        let result = state.convert(&payload, true);
        // Expect: SPS + PPS + IDR, all with Annex B start codes.
        let expected: Vec<u8> = [
            &[0, 0, 0, 1][..],
            &sps,
            &[0, 0, 0, 1],
            &pps,
            &[0, 0, 0, 1],
            &idr,
        ]
        .concat();
        assert_eq!(result, expected);

        // Non-keyframe should not include SPS/PPS.
        let result_non_kf = state.convert(&payload, false);
        let expected_non_kf: Vec<u8> = [&[0, 0, 0, 1][..], &idr].concat();
        assert_eq!(result_non_kf, expected_non_kf);
    }
}
