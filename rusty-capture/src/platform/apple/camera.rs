//! AVFoundation camera capture for macOS and iOS.
//!
//! Uses `AVCaptureSession` + `AVCaptureVideoDataOutput` to capture frames from
//! the default camera device. Produces RGBA CPU frames.
//!
//! # Zero-Copy Plan
//!
//! AVFoundation delivers `CMSampleBuffer` containing IOSurface-backed
//! `CVPixelBuffer`. The same `AppleGpuFrame` / `NativeFrameHandle::IoSurface`
//! path described in `screen.rs` applies here — both screen and camera produce
//! the same `CVPixelBuffer` type, so 100% of the downstream zero-copy code
//! is shared.
//!
//! For VideoToolbox encoding:
//! ```ignore
//! // In vtb/encoder.rs push_frame():
//! match &frame.data {
//!     FrameData::Gpu(gpu) => {
//!         if let Some(NativeFrameHandle::IoSurface(surface)) = gpu.native_handle() {
//!             // Create CVPixelBuffer from IOSurface, pass to VTCompressionSession
//!             VTCompressionSessionEncodeFrame(session, pixel_buffer, pts, dur, ...);
//!             return Ok(());
//!         }
//!     }
//!     // ... existing fallback
//! }
//! ```
//!
//! # iOS Notes
//!
//! On iOS, AVFoundation works identically but with additional permission
//! requirements (`NSCameraUsageDescription` in Info.plist). The same code
//! compiles for both macOS and iOS.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};

use rusty_codecs::format::{PixelFormat, VideoFormat, VideoFrame};
use rusty_codecs::traits::VideoSource;

use crate::types::{CameraConfig, CameraInfo, CapturePixelFormat};

/// Lists available cameras via AVFoundation.
pub fn cameras() -> Result<Vec<CameraInfo>> {
    // TODO: Use objc2-av-foundation to enumerate AVCaptureDevice instances.
    //
    // Implementation:
    // ```
    // let device_types = NSArray::from_vec(vec![
    //     AVCaptureDeviceTypeBuiltInWideAngleCamera,
    //     AVCaptureDeviceTypeExternalUnknown, // macOS external cameras
    // ]);
    // let discovery = AVCaptureDeviceDiscoverySession::discovery_session_with_device_types(
    //     &device_types,
    //     AVMediaTypeVideo,
    //     AVCaptureDevicePositionUnspecified,
    // );
    // for device in discovery.devices().iter() {
    //     let name = device.localizedName().to_string();
    //     let id = device.uniqueID().to_string();
    //     // Enumerate formats via device.formats()
    //     // Each AVCaptureDeviceFormat has formatDescription (pixel format, dimensions)
    //     // and videoSupportedFrameRateRanges (min/max fps)
    // }
    // ```
    //
    // For now, return a placeholder for the default camera.
    Ok(vec![CameraInfo {
        id: "default".into(),
        name: "Default Camera".into(),
        supported_formats: vec![],
    }])
}

/// AVFoundation camera capturer for macOS and iOS.
///
/// Currently a stub that outlines the full implementation. The capture session
/// setup requires objc2 delegate patterns for `AVCaptureVideoDataOutputSampleBufferDelegate`.
#[derive(derive_more::Debug)]
pub struct AppleCameraCapturer {
    width: u32,
    height: u32,
    #[debug(skip)]
    rx: mpsc::Receiver<VideoFrame>,
    #[debug(skip)]
    stop_tx: Option<mpsc::Sender<()>>,
}

impl AppleCameraCapturer {
    /// Creates a camera capturer for the given device.
    pub fn new(info: &CameraInfo, config: &CameraConfig) -> Result<Self> {
        use crate::CameraSelector;

        let (frame_tx, frame_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();

        // Derive requested resolution from the selector strategy.
        let (width, height) = if let Some(fmt) = config.select_format(&info.supported_formats) {
            (fmt.dimensions[0], fmt.dimensions[1])
        } else {
            match &config.selector {
                CameraSelector::TargetResolution(w, h) => (*w, *h),
                _ => (1280, 720),
            }
        };
        let device_id = info.id.clone();

        // TODO: Implement AVFoundation capture session.
        //
        // Full implementation outline:
        //
        // 1. DEVICE SETUP:
        //    ```
        //    let device = if device_id == "default" {
        //        AVCaptureDevice::default_device_with_media_type(AVMediaTypeVideo)
        //    } else {
        //        AVCaptureDevice::device_with_unique_id(&device_id)
        //    };
        //    let input = AVCaptureDeviceInput::init_with_device(&device)?;
        //    ```
        //
        // 2. SESSION CONFIGURATION:
        //    ```
        //    let session = AVCaptureSession::new();
        //    session.beginConfiguration();
        //    session.addInput(&input);
        //
        //    let output = AVCaptureVideoDataOutput::new();
        //    output.setVideoSettings(&NSDictionary::from_keys_and_objects(
        //        &[kCVPixelBufferPixelFormatTypeKey],
        //        &[kCVPixelFormatType_32BGRA],
        //    ));
        //    output.setAlwaysDiscardsLateVideoFrames(true);
        //
        //    let queue = dispatch2::Queue::new("camera-capture", dispatch2::QueueAttribute::Serial);
        //    output.setSampleBufferDelegate(&delegate, &queue);
        //    session.addOutput(&output);
        //    session.commitConfiguration();
        //    ```
        //
        // 3. DELEGATE (frame callback):
        //    ```
        //    impl AVCaptureVideoDataOutputSampleBufferDelegate for CameraDelegate {
        //        fn captureOutput_didOutputSampleBuffer_fromConnection(
        //            &self, output, sample_buffer, connection
        //        ) {
        //            let pixel_buffer = CMSampleBufferGetImageBuffer(sample_buffer);
        //            // CPU path: lock base address, copy BGRA→RGBA
        //            // Zero-copy path: wrap CVPixelBuffer as AppleGpuFrame
        //            frame_tx.send(VideoFrame::new_rgba(rgba, w, h));
        //        }
        //    }
        //    ```
        //
        // 4. START:
        //    ```
        //    session.startRunning();
        //    ```
        //
        // The objc2 delegate pattern requires defining a class with
        // `declare_class!` macro. This is the most complex part.

        warn!("AVFoundation camera capture not yet fully implemented");

        // For now, produce no frames. The struct compiles and integrates
        // into the VideoSource trait correctly.
        Ok(Self {
            width,
            height,
            rx: frame_rx,
            stop_tx: Some(stop_tx),
        })
    }

    /// Opens the default camera.
    pub fn open_default() -> Result<Self> {
        Self::new(
            &CameraInfo {
                id: "default".into(),
                name: "Default Camera".into(),
                supported_formats: vec![],
            },
            &CameraConfig::default(),
        )
    }
}

impl VideoSource for AppleCameraCapturer {
    fn name(&self) -> &str {
        "apple-camera"
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let mut latest = None;
        while let Ok(frame) = self.rx.try_recv() {
            latest = Some(frame);
        }
        Ok(latest)
    }
}

impl Drop for AppleCameraCapturer {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}
