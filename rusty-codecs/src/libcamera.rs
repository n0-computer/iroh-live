//! Raspberry Pi pre-encoded H.264 capture via `rpicam-vid` (libcamera subprocess).
//!
//! On Raspberry Pi OS Bookworm, the CSI camera is only accessible through
//! the libcamera stack. Direct V4L2 access to `/dev/video0` gives raw Bayer
//! data from the Unicam sensor, which is unusable without the ISP pipeline.
//!
//! [`LibcameraH264Source`] produces pre-encoded H.264 Annex-B packets
//! directly from rpicam-vid's internal hardware encoder. Preferred on Pi
//! Zero 2 because it avoids the ~10 MB/s raw-YUV pipe and redundant NV12
//! conversion, using rpicam-vid's DMABUF zero-copy ISP→encoder path.
//!
//! For raw YUV capture (to feed a separate encoder), use
//! [`rusty_capture::LibcameraCapturer`] with the `libcamera` feature flag.

use std::{
    io::Read,
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result};
use bytes::Bytes;

use crate::{
    codec::h264::annexb::{build_avcc, extract_sps_pps, parse_annex_b},
    config::{H264, VideoCodec, VideoConfig},
    format::EncodedFrame,
    traits::PreEncodedVideoSource,
};

// ---------------------------------------------------------------------------
// Pre-encoded H.264 source (preferred on Pi)
// ---------------------------------------------------------------------------

/// Configuration for [`LibcameraH264Source`].
///
/// Cloneable so the publish pipeline can create fresh instances per
/// subscriber. Each clone spawns its own `rpicam-vid` process.
#[derive(Debug, Clone)]
pub struct LibcameraH264Config {
    /// Capture width in pixels.
    pub width: u32,
    /// Capture height in pixels.
    pub height: u32,
    /// Capture framerate.
    pub framerate: u32,
    /// Target bitrate in bits per second.
    pub bitrate: u32,
    /// Keyframe interval in frames.
    pub keyframe_interval: u32,
}

impl LibcameraH264Config {
    /// Creates a configuration with sensible defaults for the given resolution.
    pub fn new(width: u32, height: u32, framerate: u32) -> Self {
        Self {
            width,
            height,
            framerate,
            bitrate: 500_000,
            // 1-second keyframe interval for fast channel join and
            // recovery. Shorter intervals increase bitrate slightly but
            // dramatically improve join latency (subscriber must wait for
            // the next keyframe before decoding can start).
            keyframe_interval: framerate,
        }
    }

    /// Sets the target bitrate.
    pub fn with_bitrate(mut self, bitrate: u32) -> Self {
        self.bitrate = bitrate;
        self
    }

    /// Sets the keyframe interval.
    pub fn with_keyframe_interval(mut self, frames: u32) -> Self {
        self.keyframe_interval = frames;
        self
    }

    /// Returns the [`VideoConfig`] for catalog/subscriber setup.
    pub fn video_config(&self) -> VideoConfig {
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                inline: true,
                profile: 0x42,
                constraints: 0xE0,
                // Not hardcoded — rpicam-vid auto-selects level from
                // resolution/bitrate/framerate. 3.0 is a safe floor that
                // covers 640×360@30 at 500 kbps; the actual SPS will carry
                // the real value once the first keyframe arrives.
                level: 0x1E, // 3.0
            }),
            description: None,
            coded_width: Some(self.width),
            coded_height: Some(self.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(self.bitrate as u64),
            framerate: Some(self.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }

    /// Returns a track name suitable for the MoQ catalog.
    pub fn track_name(&self) -> String {
        format!("video/h264-libcamera-{}p", self.height)
    }
}

/// Pre-encoded H.264 source via `rpicam-vid --codec h264`.
///
/// Each instance owns an `rpicam-vid` subprocess. The source reads the
/// Annex-B H.264 bytestream from stdout, splits it into access units,
/// and returns each as an [`EncodedFrame`].
pub struct LibcameraH264Source {
    config: LibcameraH264Config,
    child: Option<Child>,
    buf: Vec<u8>,
    remainder: Vec<u8>,
    frame_count: u64,
    avcc: Option<Bytes>,
}

impl std::fmt::Debug for LibcameraH264Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibcameraH264Source")
            .field("config", &self.config)
            .field("frame_count", &self.frame_count)
            .field("running", &self.child.is_some())
            .finish()
    }
}

impl LibcameraH264Source {
    /// Creates a new source from the given configuration.
    pub fn new(config: LibcameraH264Config) -> Self {
        Self {
            config,
            child: None,
            // Small read buffer to minimize latency. At 360p baseline H.264
            // ~500 kbps, a typical frame is 2-8 KB. A 32 KB buffer is large
            // enough for any single frame while ensuring read() returns
            // promptly (not waiting to fill a 256 KB buffer).
            buf: vec![0u8; 32 * 1024],
            remainder: Vec::new(),
            frame_count: 0,
            avcc: None,
        }
    }
}

impl PreEncodedVideoSource for LibcameraH264Source {
    fn name(&self) -> &str {
        "libcamera-h264"
    }

    fn config(&self) -> VideoConfig {
        let mut cfg = self.config.video_config();
        cfg.description = self.avcc.clone();
        cfg
    }

    fn start(&mut self) -> Result<()> {
        let c = &self.config;
        tracing::info!(
            width = c.width,
            height = c.height,
            fps = c.framerate,
            bitrate = c.bitrate,
            "starting rpicam-vid H.264 capture"
        );

        // Retry loop: the Pi has a single camera, and rpicam-vid holds an
        // exclusive lock on it. When a subscriber disconnects and immediately
        // reconnects, the new process can race with the dying one. Retry with
        // backoff so transient "pipeline handler in use" errors resolve once
        // the old process finishes releasing the camera.
        const MAX_RETRIES: u32 = 5;
        const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

        for attempt in 0..=MAX_RETRIES {
            let mut child = Command::new("rpicam-vid")
                .args([
                    "--codec",
                    "h264",
                    "--width",
                    &c.width.to_string(),
                    "--height",
                    &c.height.to_string(),
                    "--framerate",
                    &c.framerate.to_string(),
                    "--bitrate",
                    &c.bitrate.to_string(),
                    "--intra",
                    &c.keyframe_interval.to_string(),
                    "--profile",
                    "baseline",
                    // Note: do NOT pass --level — rpicam-vid (libcamera v0.3.0)
                    // rejects the "3.1" format with "no such level 3.1". Let it
                    // auto-select from resolution/bitrate/framerate instead.
                    "--inline", // prepend SPS+PPS before every IDR
                    "--flush",  // flush output after each frame (low latency)
                    "--timeout",
                    "0",
                    "--nopreview",
                    "-o",
                    "-",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("failed to spawn rpicam-vid — is libcamera installed?")?;

            let pid = child.id();
            tracing::info!(pid, "rpicam-vid spawned (H.264 mode)");

            // Give rpicam-vid a moment to initialize and potentially fail.
            // Camera acquisition errors surface within ~1 s on Pi Zero 2.
            std::thread::sleep(Duration::from_millis(1500));

            // Check if the process already exited (immediate failure).
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Process died immediately — read stderr for the reason.
                    let stderr_msg = child
                        .stderr
                        .as_mut()
                        .and_then(|se| {
                            let mut buf = String::new();
                            se.read_to_string(&mut buf).ok().map(|_| buf)
                        })
                        .unwrap_or_default();

                    if attempt < MAX_RETRIES
                        && (stderr_msg.contains("in use by another process")
                            || stderr_msg.contains("failed to acquire camera"))
                    {
                        let backoff = INITIAL_BACKOFF * 2u32.pow(attempt);
                        tracing::warn!(
                            attempt,
                            backoff_ms = backoff.as_millis() as u64,
                            "camera busy, retrying"
                        );
                        std::thread::sleep(backoff);
                        continue;
                    }

                    anyhow::bail!(
                        "rpicam-vid exited immediately (status: {status}, stderr: {:?})",
                        stderr_msg.trim(),
                    );
                }
                Ok(None) => {
                    // Still running — camera acquired successfully.
                    tracing::info!(pid, "rpicam-vid started (H.264 mode)");
                    self.child = Some(child);
                    self.frame_count = 0;
                    self.remainder.clear();
                    self.avcc = None;
                    return Ok(());
                }
                Err(e) => {
                    anyhow::bail!("failed to check rpicam-vid status: {e}");
                }
            }
        }

        anyhow::bail!("rpicam-vid failed after {MAX_RETRIES} retries — camera unavailable");
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        let child = self.child.as_mut().context("not started")?;
        let stdout = child
            .stdout
            .as_mut()
            .context("rpicam-vid stdout unavailable")?;

        // Read a chunk. Blocks until data is available.
        let n = stdout
            .read(&mut self.buf)
            .context("rpicam-vid read failed")?;
        if n == 0 {
            // Capture stderr and exit status for diagnostics.
            let mut child = self.child.take().unwrap();
            let status = child.wait().ok();
            let stderr_msg = child
                .stderr
                .as_mut()
                .and_then(|se| {
                    let mut buf = String::new();
                    se.read_to_string(&mut buf).ok().map(|_| buf)
                })
                .unwrap_or_default();
            let stderr_trimmed = stderr_msg.trim();
            if !stderr_trimmed.is_empty() {
                tracing::error!(stderr = stderr_trimmed, "rpicam-vid stderr");
            }
            anyhow::bail!(
                "rpicam-vid EOF — process exited (status: {}, stderr: {:?})",
                status.map_or_else(|| "unknown".into(), |s| format!("{s}")),
                stderr_trimmed,
            );
        }

        self.remainder.extend_from_slice(&self.buf[..n]);

        // Find the end of the first complete access unit.
        let split_pos = find_first_au_end(&self.remainder);

        let Some(pos) = split_pos else {
            return Ok(None);
        };

        // Split off the complete AU without an intermediate allocation:
        // split_off leaves [0..pos) in self.remainder and returns [pos..].
        let tail = self.remainder.split_off(pos);
        let frame_data = std::mem::replace(&mut self.remainder, tail);
        if frame_data.is_empty() {
            return Ok(None);
        }

        let is_keyframe = contains_idr_nal(&frame_data);

        // Extract avcC from first keyframe (only parse NALs when needed).
        if is_keyframe && self.avcc.is_none() {
            let nals = parse_annex_b(&frame_data);
            if let Some((sps, pps)) = extract_sps_pps(&nals) {
                self.avcc = Some(build_avcc(sps, pps).into());
                tracing::debug!(
                    sps_len = sps.len(),
                    pps_len = pps.len(),
                    "extracted SPS/PPS from first keyframe"
                );
            }
        }

        let fps = self.config.framerate as f64;
        let pts = Duration::from_secs_f64(self.frame_count as f64 / fps);
        self.frame_count += 1;

        if self.frame_count <= 3 || self.frame_count.is_multiple_of(150) {
            tracing::debug!(
                frame = self.frame_count,
                len = frame_data.len(),
                is_keyframe,
                "H.264 frame"
            );
        }

        Ok(Some(EncodedFrame {
            is_keyframe,
            timestamp: pts,
            payload: frame_data.into(),
        }))
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            tracing::info!("stopping rpicam-vid");
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

impl Drop for LibcameraH264Source {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Checks whether an Annex-B bytestream contains an IDR (type 5) NAL unit
/// without fully parsing all NAL boundaries.
fn contains_idr_nal(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < data.len() {
        let (sc_len, nal_start) = if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            (3, i + 3)
        } else if i + 4 <= data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            (4, i + 4)
        } else {
            i += 1;
            continue;
        };
        if nal_start < data.len() && (data[nal_start] & 0x1F) == 5 {
            return true;
        }
        i += sc_len;
    }
    false
}

/// Finds the end of the first complete access unit in an Annex-B bytestream.
///
/// Scans forward for AU-starting NAL types (SPS=7, IDR=5, non-IDR=1).
/// Returns the byte offset where the *second* such NAL starts — everything
/// before that offset is exactly one AU. Returns `None` if the buffer
/// contains fewer than two AU boundaries (i.e. no complete AU yet).
fn find_first_au_end(data: &[u8]) -> Option<usize> {
    let mut found_first = false;
    let mut i = 0;
    while i + 3 < data.len() {
        // Match 4-byte start code first (00 00 00 01), then 3-byte (00 00 01).
        let (sc_start, nal_start) = if i + 4 <= data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            (i, i + 4)
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            (i, i + 3)
        } else {
            i += 1;
            continue;
        };

        if nal_start < data.len() {
            let nal_type = data[nal_start] & 0x1F;
            // Only VCL NALs (slice types) start a new AU. SPS (7) and PPS (8)
            // are non-VCL and belong to the same AU as the following slice.
            // This ensures SPS+PPS+IDR stays together in one packet.
            if nal_type == 5 || nal_type == 1 {
                if found_first {
                    return Some(sc_start);
                }
                found_first = true;
            }
        }
        i = nal_start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn au_boundary_empty() {
        assert_eq!(find_first_au_end(&[]), None);
    }

    #[test]
    fn au_boundary_single_nal() {
        // Single IDR NAL — no second boundary, returns None.
        let data = [0, 0, 0, 1, 0x65, 0xAA, 0xBB];
        assert_eq!(find_first_au_end(&data), None);
    }

    #[test]
    fn au_boundary_two_slices() {
        // IDR (AU 1) + non-IDR (AU 2) → split at second slice start code.
        let mut data = vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB]; // IDR NAL (type 5)
        let second_start = data.len();
        data.extend_from_slice(&[0, 0, 0, 1, 0x41, 0xCC, 0xDD]); // non-IDR (type 1)
        assert_eq!(find_first_au_end(&data), Some(second_start));
    }

    #[test]
    fn au_boundary_sps_pps_idr_kept_together() {
        // SPS + PPS + IDR should be ONE AU (SPS/PPS are non-VCL, not AU boundaries).
        let mut data = vec![0, 0, 0, 1, 0x67, 0x42]; // SPS (type 7)
        data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]); // PPS (type 8)
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0xAA]); // IDR (type 5)
        // Only one VCL NAL (IDR) → no second AU boundary → None
        assert_eq!(find_first_au_end(&data), None);
    }

    #[test]
    fn au_boundary_sps_pps_idr_then_p_frame() {
        // SPS+PPS+IDR (AU 1) + P-frame (AU 2) → split at P-frame.
        let mut data = vec![0, 0, 0, 1, 0x67, 0x42]; // SPS
        data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]); // PPS
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0xAA]); // IDR
        let p_start = data.len();
        data.extend_from_slice(&[0, 0, 0, 1, 0x41, 0xBB]); // P-frame (type 1)
        assert_eq!(find_first_au_end(&data), Some(p_start));
    }

    #[test]
    fn au_boundary_sps_only() {
        // SPS alone — no VCL NAL, no AU boundary.
        let data = vec![0, 0, 0, 1, 0x67, 0x42];
        assert_eq!(find_first_au_end(&data), None);
    }

    #[test]
    fn h264_config_defaults() {
        let cfg = LibcameraH264Config::new(640, 360, 30);
        assert_eq!(cfg.bitrate, 500_000);
        assert_eq!(cfg.keyframe_interval, 30); // 1s at 30fps
        let vc = cfg.video_config();
        assert_eq!(vc.coded_width, Some(640));
        assert_eq!(vc.framerate, Some(30.0));
    }

    #[test]
    #[ignore = "requires Raspberry Pi with camera"]
    fn h264_source_produces_frames() {
        let config = LibcameraH264Config::new(640, 360, 30);
        let mut source = LibcameraH264Source::new(config);
        source.start().unwrap();
        let mut got_keyframe = false;
        for _ in 0..90 {
            if let Some(frame) = source.pop_packet().unwrap()
                && frame.is_keyframe
            {
                got_keyframe = true;
            }
        }
        source.stop().unwrap();
        assert!(got_keyframe, "should have received at least one keyframe");
    }
}
