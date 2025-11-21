// use anyhow::{Context, Result};
// use ffmpeg_next::{format::Pixel, frame::Video as FfmpegVideoFrame};
// use iroh_moq::av::{PixelFormat, VideoFormat, VideoFrame, VideoSource};
// use tracing::{debug, info};
// use xcap::{Monitor, VideoRecorder};

// pub struct ScreenCapturer {
//     pub(crate) _monitor: Monitor,
//     pub(crate) width: u32,
//     pub(crate) height: u32,
//     pub(crate) video_recorder: VideoRecorder,
//     pub(crate) rx: std::sync::mpsc::Receiver<xcap::Frame>,
// }

// impl Drop for ScreenCapturer {
//     fn drop(&mut self) {
//         self.video_recorder.stop().ok();
//     }
// }

// impl ScreenCapturer {
//     pub fn new() -> Result<Self> {
//         info!("Initializing screen capturer with xcap");

//         let monitors = Monitor::all().context("Failed to get monitors")?;
//         if monitors.is_empty() {
//             return Err(anyhow::anyhow!("No monitors available"));
//         }
//         debug!("Monitors: {monitors:?}");

//         let monitor = monitors.into_iter().next().unwrap();
//         let width = monitor.width()?;
//         let height = monitor.height()?;
//         let name = monitor
//             .name()
//             .unwrap_or_else(|_| "Unknown Monitor".to_string());

//         info!("Using primary monitor: {} ({}x{})", name, width, height);

//         let (video_recorder, rx) = monitor.video_recorder()?;
//         video_recorder.start()?;

//         Ok(Self {
//             _monitor: monitor,
//             video_recorder,
//             rx,
//             width,
//             height,
//         })
//     }

//     pub fn capture(&mut self) -> Result<VideoFrame> {
//         let mut raw_frame = None;
//         // We are only interested in the latest frame.
//         // Drain the channel to not build up memory.
//         while let Ok(next) = self.rx.try_recv() {
//             raw_frame = Some(next)
//         }
//         let raw_frame = match raw_frame {
//             Some(frame) => frame,
//             None => self
//                 .rx
//                 .recv()
//                 .context("video recorder did not produce new frame")?,
//         };
//         let mut frame = FfmpegVideoFrame::new(Pixel::RGBA, raw_frame.width, raw_frame.height);
//         frame.data_mut(0).copy_from_slice(&raw_frame.raw);
//         Ok(VideoFrame { raw: raw_frame.raw })
//     }
// }

// impl VideoSource for ScreenCapturer {
//     fn format(&self) -> VideoFormat {
//         VideoFormat {
//             pixel_format: PixelFormat::Rgba,
//             dimensions: [self.width, self.height],
//         }
//     }

//     fn current_frame(&mut self) -> Result<Option<VideoFrame>> {
//         let frame = self.capture()?;
//         Ok(Some(frame))
//     }
// }
