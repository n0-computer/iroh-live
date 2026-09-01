//! Generated media sources, for tests and demos that need no hardware.
//!
//! `moq-video` and `moq-audio` ship no test sources: a caller that wants
//! pixels without a camera builds a [`Surface`](moq_video::Surface) itself.
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
        let clock = clock;
        let size = size;
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

    let per_frame = (sample_rate as f64 * FRAME.as_secs_f64()) as usize;
    let step = hz * std::f64::consts::TAU / sample_rate as f64;
    let input = moq_audio::encode::Input {
        format: moq_audio::Format::F32,
        sample_rate,
        channels,
    };

    let frames: BoxStream<moq_audio::Frame> = Box::pin(n0_future::stream::unfold(
        (0usize, 0u64),
        move |(sample, elapsed_us)| async move {
            tokio::time::sleep(FRAME).await;
            let mut data = Vec::with_capacity(per_frame * channels as usize * 4);
            for index in 0..per_frame {
                let value = ((sample + index) as f64 * step).sin() as f32;
                for _ in 0..channels {
                    data.extend_from_slice(&value.to_le_bytes());
                }
            }
            let timestamp = moq_net::Timestamp::from_micros(elapsed_us)
                .expect("elapsed micros out of Timestamp range");
            let frame = moq_audio::Frame {
                timestamp,
                data: data.into(),
            };
            let next_us = elapsed_us + FRAME.as_micros() as u64;
            Some((frame, (sample + per_frame, next_us)))
        },
    ));

    AudioSource::Frames { input, frames }
}
