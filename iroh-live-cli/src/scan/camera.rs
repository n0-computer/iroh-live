//! The camera the scan screen reads, and how it is opened.

use std::time::Duration;

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::media::RpicamConfig;
use iroh_live::media::{
    VideoSource,
    video::{self, Size, capture},
};

use super::{FIRST_FRAME_GRACE, no_frames};
use crate::source_spec::VideoSourceSpec;

/// The capture size the scanner asks for.
///
/// 720p reads a Pi Zero's e-paper display at arm's length where 480p often
/// does not.
pub(super) const SCAN_SIZE: Size = Size {
    width: 1280,
    height: 720,
};

/// The capture frame rate the scanner asks for.
///
/// Enough for a live preview. Every frame costs a format conversion.
pub(super) const SCAN_FRAMERATE: u32 = 15;

/// How long the scanner waits before reopening a camera that failed.
pub(super) const REOPEN_DELAY: Duration = Duration::from_secs(2);

/// A camera the scanner opened.
pub(super) struct Camera {
    pub(super) source: VideoSource,
    /// The name used in logs and errors.
    pub(super) label: String,
    /// Whether this is the Raspberry Pi camera, which changes the no-frames hint.
    pub(super) rpicam: bool,
}

impl Camera {
    /// Opens the camera `choice` names, or the best guess if `None`.
    ///
    /// # Errors
    ///
    /// Returns a message for the screen if the camera does not open, or opens
    /// and delivers nothing within [`FIRST_FRAME_GRACE`].
    pub(super) async fn open(choice: Option<&VideoSourceSpec>) -> Result<Self, String> {
        match resolve(choice) {
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            VideoSourceSpec::Rpicam(_) => Self::open_rpicam().await,
            VideoSourceSpec::Camera(id) => Self::open_capture(id).await,
            // Unreachable: `camera_spec` rejects everything else.
            other => Err(format!("{other:?} is not a camera")),
        }
    }

    /// Opens a camera by id, or the default camera, and waits for a first frame.
    pub(super) async fn open_capture(id: Option<String>) -> Result<Self, String> {
        let label = match &id {
            Some(id) => format!("camera {id}"),
            None => "the default camera".to_string(),
        };
        let mut config = capture::Config::default();
        config.source = capture::Source::Camera(id);
        config.width = Some(SCAN_SIZE.width);
        config.height = Some(SCAN_SIZE.height);
        config.framerate = Some(
            video::FrameRate::new(SCAN_FRAMERATE, 1).expect("the scan rate is a valid frame rate"),
        );
        // `capture` returns after the first frame, so the timeout catches a
        // camera that opens and then delivers nothing.
        let source = tokio::time::timeout(FIRST_FRAME_GRACE, VideoSource::capture(config))
            .await
            .map_err(|_| no_frames(&label, false))?
            .map_err(|err| format!("the camera would not open: {err:#}"))?;
        Ok(Self {
            source,
            label,
            rpicam: false,
        })
    }

    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    /// Opens the Raspberry Pi camera and waits for a first frame.
    pub(super) async fn open_rpicam() -> Result<Self, String> {
        let config = RpicamConfig::new(SCAN_SIZE, SCAN_FRAMERATE);
        let source = VideoSource::rpicam(config)
            .await
            .map_err(|err| format!("the Raspberry Pi camera would not open: {err:#}"))?;
        Ok(Self {
            source,
            label: "rpicam-vid".to_string(),
            rpicam: true,
        })
    }
}

/// Parses `--scan-camera`, which uses the `--video` syntax.
///
/// Accepts only cameras and the Raspberry Pi camera. `rpicam` always means raw
/// frames, since the QR decoder cannot read H.264.
///
/// # Errors
///
/// Returns a message listing the accepted forms.
pub(crate) fn camera_spec(spec: &str) -> Result<VideoSourceSpec, String> {
    match spec.parse::<VideoSourceSpec>()? {
        VideoSourceSpec::Camera(id) => Ok(VideoSourceSpec::Camera(id)),
        #[cfg(all(target_os = "linux", feature = "rpicam"))]
        VideoSourceSpec::Rpicam(_) => {
            Ok(VideoSourceSpec::Rpicam(crate::source_spec::RpicamMode::Raw))
        }
        _ => Err(format!(
            "--scan-camera reads a camera: 'cam' for the default one, 'cam:<id>' for one \
             `irl devices` lists, or 'rpicam' for the Raspberry Pi camera; got '{spec}'"
        )),
    }
}

/// Returns the camera to open, preferring the Pi camera when none is named.
///
/// On a Raspberry Pi, `/dev/video0` is the Unicam node. It accepts the
/// requested format and then never delivers a frame, so `rpicam-vid` is the
/// better default where it is installed. A Pi with a USB webcam needs
/// `--scan-camera cam`.
pub(super) fn resolve(choice: Option<&VideoSourceSpec>) -> VideoSourceSpec {
    if let Some(spec) = choice {
        return spec.clone();
    }
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    if crate::devices::rpicam::installed() {
        return VideoSourceSpec::Rpicam(crate::source_spec::RpicamMode::Raw);
    }
    VideoSourceSpec::Camera(None)
}

#[cfg(test)]
mod tests {

    use super::*;

    /// `--scan-camera` accepts cameras in the `--video` syntax.
    #[test]
    fn the_scan_camera_takes_a_camera_by_id() {
        assert_eq!(
            camera_spec("cam").expect("a camera"),
            VideoSourceSpec::Camera(None)
        );
        assert_eq!(
            camera_spec("cam:/dev/video2").expect("a camera"),
            VideoSourceSpec::Camera(Some("/dev/video2".into()))
        );
    }

    /// `--scan-camera` rejects sources that are not cameras.
    #[test]
    fn the_scan_camera_refuses_what_is_not_a_camera() {
        for spec in ["screen", "test", "file:clip.mp4", "none"] {
            let err = camera_spec(spec).expect_err(spec);
            assert!(err.contains("--scan-camera"), "{spec}: {err}");
            assert!(err.contains(spec), "{spec}: {err}");
        }
    }

    /// Every Pi camera spec maps to raw frames.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    #[test]
    fn the_pi_camera_is_always_raw_for_scanning() {
        use crate::source_spec::RpicamMode;
        for spec in ["rpicam", "rpicam:raw", "picam"] {
            assert_eq!(
                camera_spec(spec).expect(spec),
                VideoSourceSpec::Rpicam(RpicamMode::Raw),
                "{spec}"
            );
        }
    }
}
