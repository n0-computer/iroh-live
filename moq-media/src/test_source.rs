//! Generated media sources, for tests and demos that need no hardware.
//!
//! `moq-video` and `moq-audio` ship no test sources: a caller that wants
//! pixels without a camera builds a [`Surface`] itself.
//! These do that, so a test can publish something real over a real transport
//! without a device, a driver, or a file.

use std::time::Duration;

use moq_video::{Frame, Size, Surface};
use n0_future::boxed::BoxStream;

use crate::publish::{AudioSource, VideoSource};

/// A moving colour pattern at a fixed size and frame rate.
///
/// The picture changes every frame, which matters: a static image compresses
/// to almost nothing after the first keyframe, so a test watching for bytes on
/// the wire would pass on a pipeline that had stalled.
pub fn video(size: Size, framerate: u32) -> VideoSource {
    let interval = Duration::from_secs_f64(1.0 / framerate.max(1) as f64);
    let clock = moq_mux::Clock::new();
    let mut rgba = vec![0u8; (size.width * size.height * 4) as usize];

    let frames: BoxStream<Frame> = Box::pin(n0_future::stream::unfold(0u32, move |tick| {
        // Paint into the same buffer every frame; `Surface::rgba` copies.
        paint(&mut rgba, size, tick);
        let surface = Surface::rgba(&rgba, size).expect("generated pattern is well formed");
        async move {
            if tick > 0 {
                tokio::time::sleep(interval).await;
            }
            // Read the clock after the wait, not before it, or every frame
            // carries the timestamp of the one before.
            let timestamp = moq_net::Timestamp::from_micros(clock.micros())
                .expect("clock micros out of Timestamp range");
            Some((Frame::new(surface, timestamp), tick.wrapping_add(1)))
        }
    }));
    VideoSource::Frames(frames)
}

/// Fills `rgba` with a diagonal gradient that shifts with `tick`.
fn paint(rgba: &mut [u8], size: Size, tick: u32) {
    let phase = tick.wrapping_mul(3) as u8;
    for y in 0..size.height {
        for x in 0..size.width {
            let offset = ((y * size.width + x) * 4) as usize;
            rgba[offset] = (x as u8).wrapping_add(phase);
            rgba[offset + 1] = (y as u8).wrapping_add(phase);
            rgba[offset + 2] = phase;
            rgba[offset + 3] = 0xff;
        }
    }
}

/// A sine tone at `hz`, in the layout the encoder is told to expect.
pub fn audio(hz: f64, sample_rate: u32, channels: u32) -> AudioSource {
    /// One buffer per 20 ms, matching the Opus frame duration so the encoder
    /// consumes each buffer whole.
    const FRAME: Duration = Duration::from_millis(20);

    /// Peak amplitude. Not full scale on purpose: Opus overshoots a little on
    /// decode, and a tone at 1.0 clips against the mixer's clamp, which is
    /// audible as distortion on a signal chosen for being unmistakable.
    const AMPLITUDE: f32 = 0.5;

    let per_frame = (sample_rate as f64 * FRAME.as_secs_f64()) as usize;
    let step = hz * std::f64::consts::TAU / sample_rate as f64;
    let input = moq_audio::encode::Input {
        format: moq_audio::Format::F32,
        sample_rate,
        channels,
    };

    // Pace against an absolute schedule, not `sleep(FRAME)` per iteration.
    // A sleep always overshoots, and these timestamps advance by exactly one
    // frame whatever the clock did, so sleeping relatively makes the media
    // timeline run slower than real time: a live subscriber consuming at the
    // rate the timestamps promise starves, which is heard as glitching rather
    // than as the stream being late.
    let started = tokio::time::Instant::now();

    let frames: BoxStream<moq_audio::Frame> = Box::pin(n0_future::stream::unfold(
        (0usize, 0u64),
        move |(sample, elapsed_us)| async move {
            let next = started + FRAME * (elapsed_us / FRAME.as_micros() as u64 + 1) as u32;
            tokio::time::sleep_until(next).await;
            let mut data = Vec::with_capacity(per_frame * channels as usize * 4);
            for index in 0..per_frame {
                let value = ((sample + index) as f64 * step).sin() as f32 * AMPLITUDE;
                for _ in 0..channels {
                    data.extend_from_slice(&value.to_le_bytes());
                }
            }
            let timestamp = moq_net::Timestamp::from_micros(elapsed_us)
                .expect("elapsed micros out of Timestamp range");
            let frame = moq_audio::Frame::new(data.into(), timestamp);
            let next_us = elapsed_us + FRAME.as_micros() as u64;
            Some((frame, (sample + per_frame, next_us)))
        },
    ));

    AudioSource::Frames { input, frames }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use n0_future::StreamExt;

    use super::*;
    use crate::publish::AudioSource;

    /// The tone has to arrive as fast as its own timestamps claim.
    ///
    /// A generator that sleeps for a frame's length each iteration overshoots
    /// every time, while its timestamps advance by exactly one frame, so the
    /// media timeline falls behind the clock. A subscriber consuming at the
    /// promised rate then starves, which is heard as glitching rather than as
    /// the stream being late.
    #[tokio::test(flavor = "current_thread")]
    async fn the_tone_keeps_up_with_the_clock() {
        const RATE: u32 = 48_000;
        const RUN: Duration = Duration::from_millis(600);

        let AudioSource::Frames { mut frames, .. } = audio(440.0, RATE, 2) else {
            panic!("the generated tone is a frame source");
        };

        let started = Instant::now();
        let mut media = Duration::ZERO;
        while started.elapsed() < RUN {
            let Some(frame) = frames.next().await else {
                break;
            };
            // Every frame is one buffer's worth, so the media timeline is the
            // last timestamp plus the frame it stands for.
            media = Duration::from(frame.timestamp) + Duration::from_millis(20);
        }
        let real = started.elapsed();

        // Generous, because a loaded machine can lose a scheduling slice: this
        // catches a timeline running slow by design, not by a few milliseconds.
        let ratio = media.as_secs_f64() / real.as_secs_f64();
        assert!(
            ratio > 0.95,
            "the tone produced {media:?} of audio in {real:?} ({:.1}% of real time); \
             a live subscriber starves at that rate",
            ratio * 100.0,
        );
    }

    /// Keeps the tone below full scale, so Opus overshoot does not clip.
    #[tokio::test(flavor = "current_thread")]
    async fn the_tone_leaves_headroom() {
        let AudioSource::Frames { mut frames, .. } = audio(440.0, 48_000, 1) else {
            panic!("the generated tone is a frame source");
        };

        let frame = frames.next().await.expect("the tone yields a first frame");
        let peak = frame
            .data
            .chunks_exact(4)
            .map(|s| f32::from_le_bytes(s.try_into().expect("four bytes")).abs())
            .fold(0.0f32, f32::max);

        assert!(peak > 0.1, "the tone is silent: peak {peak}");
        assert!(
            peak < 0.95,
            "the tone reaches {peak}, close enough to full scale to clip"
        );
    }
}
