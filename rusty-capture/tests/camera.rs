//! Camera capture tests.
//!
//! Unit tests for types run without hardware. On-device V4L2 tests require a
//! real camera and must run serially (`--test-threads=1`) since only one
//! process can hold a V4L2 device at a time.
//!
//! PipeWire camera tests are skipped because the portal shows a consent popup.

use rusty_capture::types::*;

// ── Unit tests (no hardware needed) ─────────────────────────────────

#[test]
fn selector_highest_resolution() {
    let formats = vec![
        CameraFormat {
            dimensions: [640, 480],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1920, 1080],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1280, 720],
            fps: 60.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
    ];

    let sel = CameraSelector::HighestResolution;
    let best = sel.select(&formats).unwrap();
    assert_eq!(best.dimensions, [1920, 1080]);
}

#[test]
fn selector_highest_resolution_tiebreak_fps() {
    let formats = vec![
        CameraFormat {
            dimensions: [1920, 1080],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1920, 1080],
            fps: 60.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
    ];

    let sel = CameraSelector::HighestResolution;
    let best = sel.select(&formats).unwrap();
    assert_eq!(best.fps, 60.0);
}

#[test]
fn selector_highest_framerate() {
    let formats = vec![
        CameraFormat {
            dimensions: [1920, 1080],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [640, 480],
            fps: 120.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1280, 720],
            fps: 60.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
    ];

    let sel = CameraSelector::HighestFramerate;
    let best = sel.select(&formats).unwrap();
    assert_eq!(best.fps, 120.0);
    assert_eq!(best.dimensions, [640, 480]);
}

#[test]
fn selector_highest_framerate_tiebreak_resolution() {
    let formats = vec![
        CameraFormat {
            dimensions: [640, 480],
            fps: 60.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1280, 720],
            fps: 60.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
    ];

    let sel = CameraSelector::HighestFramerate;
    let best = sel.select(&formats).unwrap();
    assert_eq!(best.dimensions, [1280, 720]);
}

#[test]
fn selector_target_resolution_exact_match() {
    let formats = vec![
        CameraFormat {
            dimensions: [640, 480],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1280, 720],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1920, 1080],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
    ];

    let sel = CameraSelector::TargetResolution(1280, 720);
    let best = sel.select(&formats).unwrap();
    assert_eq!(best.dimensions, [1280, 720]);
}

#[test]
fn selector_target_resolution_picks_next_larger() {
    let formats = vec![
        CameraFormat {
            dimensions: [640, 480],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1920, 1080],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
    ];

    let sel = CameraSelector::TargetResolution(1280, 720);
    let best = sel.select(&formats).unwrap();
    assert_eq!(best.dimensions, [1920, 1080]);
}

#[test]
fn selector_target_resolution_fallback_largest_smaller() {
    let formats = vec![
        CameraFormat {
            dimensions: [320, 240],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [640, 480],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
    ];

    let sel = CameraSelector::TargetResolution(1920, 1080);
    let best = sel.select(&formats).unwrap();
    assert_eq!(best.dimensions, [640, 480]);
}

#[test]
fn selector_empty_returns_none() {
    assert!(CameraSelector::HighestResolution.select(&[]).is_none());
    assert!(CameraSelector::HighestFramerate.select(&[]).is_none());
    assert!(
        CameraSelector::TargetResolution(1280, 720)
            .select(&[])
            .is_none()
    );
}

#[test]
fn config_select_format_preferred() {
    let formats = vec![
        CameraFormat {
            dimensions: [1920, 1080],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Yuyv,
        },
        CameraFormat {
            dimensions: [1280, 720],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Mjpeg,
        },
        CameraFormat {
            dimensions: [1920, 1080],
            fps: 30.0,
            pixel_format: CapturePixelFormat::Mjpeg,
        },
    ];

    let config = CameraConfig {
        selector: CameraSelector::HighestResolution,
        preferred_format: Some(CapturePixelFormat::Mjpeg),
        zero_copy: false,
    };
    let best = config.select_format(&formats).unwrap();
    assert_eq!(best.pixel_format, CapturePixelFormat::Mjpeg);
    assert_eq!(best.dimensions, [1920, 1080]);
}

#[test]
fn config_select_format_preferred_fallback() {
    let formats = vec![CameraFormat {
        dimensions: [1920, 1080],
        fps: 30.0,
        pixel_format: CapturePixelFormat::Yuyv,
    }];

    let config = CameraConfig {
        selector: CameraSelector::HighestResolution,
        preferred_format: Some(CapturePixelFormat::Nv12),
        zero_copy: false,
    };
    let best = config.select_format(&formats).unwrap();
    assert_eq!(best.pixel_format, CapturePixelFormat::Yuyv);
}

#[test]
fn fourcc_roundtrip() {
    let formats = [
        CapturePixelFormat::Yuyv,
        CapturePixelFormat::Mjpeg,
        CapturePixelFormat::Nv12,
        CapturePixelFormat::I420,
        CapturePixelFormat::Rgb,
        CapturePixelFormat::Gray,
    ];
    for fmt in &formats {
        let fourcc = fmt.to_v4l2_fourcc();
        let back = CapturePixelFormat::from_v4l2_fourcc(&fourcc)
            .unwrap_or_else(|| panic!("roundtrip failed for {fmt:?}"));
        assert_eq!(*fmt, back, "roundtrip failed for {fmt:?}");
    }
}

#[test]
fn fourcc_rgba_bgra_fallback_to_rgb3() {
    assert_eq!(CapturePixelFormat::Rgba.to_v4l2_fourcc(), *b"RGB3");
    assert_eq!(CapturePixelFormat::Bgra.to_v4l2_fourcc(), *b"RGB3");
}

#[test]
fn fourcc_unknown_returns_none() {
    assert!(CapturePixelFormat::from_v4l2_fourcc(b"XXXX").is_none());
    assert!(CapturePixelFormat::from_v4l2_fourcc(b"BA10").is_none());
}

#[test]
fn camera_format_pixel_count() {
    let fmt = CameraFormat {
        dimensions: [1920, 1080],
        fps: 30.0,
        pixel_format: CapturePixelFormat::Yuyv,
    };
    assert_eq!(fmt.pixel_count(), 1920 * 1080);
}

#[test]
fn camera_config_default() {
    let config = CameraConfig::default();
    assert!(matches!(config.selector, CameraSelector::HighestResolution));
    assert!(config.preferred_format.is_none());
    assert!(config.zero_copy);
}

#[test]
fn screen_config_default() {
    let config = ScreenConfig::default();
    assert_eq!(config.target_fps, Some(30.0));
    assert!(config.show_cursor);
}

// ── On-device V4L2 tests ────────────────────────────────────────────
//
// These use V4l2CameraCapturer directly to bypass PipeWire (which needs
// a portal consent popup). Must run with --test-threads=1 since the
// camera device can only be opened by one capturer at a time.

#[cfg(all(target_os = "linux", feature = "v4l2"))]
mod v4l2_device {
    use std::time::{Duration, Instant};

    use rusty_capture::{V4l2CameraCapturer, VideoSource};

    fn wait_for_frame(
        cap: &mut V4l2CameraCapturer,
        timeout: Duration,
    ) -> Option<rusty_capture::VideoFrame> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            match cap.pop_frame() {
                Ok(Some(frame)) => return Some(frame),
                Ok(None) => continue,
                Err(e) => panic!("pop_frame error: {e}"),
            }
        }
        None
    }

    fn open_or_skip() -> Option<V4l2CameraCapturer> {
        match V4l2CameraCapturer::open_default() {
            Ok(cap) => Some(cap),
            Err(e) => {
                eprintln!("SKIPPED: {e}");
                None
            }
        }
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_open_default() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        assert!(!cap.name().is_empty());
        let fmt = cap.format();
        assert!(fmt.dimensions[0] > 0);
        assert!(fmt.dimensions[1] > 0);
        cap.stop().ok();
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_format_has_nonzero_dimensions() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        let fmt = cap.format();
        assert!(
            fmt.dimensions[0] > 0 && fmt.dimensions[1] > 0,
            "format dimensions should be nonzero: {:?}",
            fmt.dimensions
        );
        cap.stop().ok();
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_name_is_nonempty() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        let name = cap.name().to_string();
        assert!(!name.is_empty());
        eprintln!("camera name: {name}");
        cap.stop().ok();
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_capture_one_frame() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        cap.start().expect("start should succeed");

        let frame = wait_for_frame(&mut cap, Duration::from_secs(5))
            .expect("should receive a frame within 5s");
        assert!(frame.width() > 0);
        assert!(frame.height() > 0);

        cap.stop().ok();
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_capture_multiple_frames() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        cap.start().expect("start should succeed");

        let mut count = 0u32;
        let deadline = Instant::now() + Duration::from_secs(5);
        while count < 10 && Instant::now() < deadline {
            match cap.pop_frame() {
                Ok(Some(_)) => count += 1,
                Ok(None) => {}
                Err(e) => panic!("pop_frame error: {e}"),
            }
        }
        assert!(count >= 10, "expected >=10 frames in 5s, got {count}");

        cap.stop().ok();
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_frame_dimensions_match_format() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        let video_fmt = cap.format();
        cap.start().expect("start should succeed");

        let frame = wait_for_frame(&mut cap, Duration::from_secs(5))
            .expect("should receive a frame within 5s");

        assert_eq!(frame.width(), video_fmt.dimensions[0]);
        assert_eq!(frame.height(), video_fmt.dimensions[1]);

        cap.stop().ok();
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_stop_is_idempotent() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        cap.start().expect("start should succeed");
        cap.stop().expect("first stop should succeed");
        cap.stop().expect("second stop should succeed");
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_drop_stops_cleanly() {
        let Some(cap) = open_or_skip() else {
            return;
        };
        drop(cap);
        // If we get here without hanging, the test passes.
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_pop_frame_without_start() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        // V4L2 starts capturing from construction (start() is a no-op).
        // pop_frame should work immediately.
        let frame = wait_for_frame(&mut cap, Duration::from_secs(5));
        assert!(frame.is_some(), "should get frames even without start()");
        cap.stop().ok();
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_frames_are_continuous() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        cap.start().expect("start should succeed");

        // Capture frames for 2 seconds, verify we get a reasonable number.
        let mut count = 0u32;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match cap.pop_frame() {
                Ok(Some(_)) => count += 1,
                Ok(None) => {}
                Err(e) => panic!("pop_frame error: {e}"),
            }
        }
        // At even 5 FPS we'd expect >=10 frames in 2 seconds.
        assert!(
            count >= 5,
            "expected continuous frames (>=5 in 2s), got {count}"
        );
        eprintln!("captured {count} frames in 2s");

        cap.stop().ok();
    }

    #[test]
    #[ignore = "requires exclusive V4L2 camera access"]
    fn v4l2_all_frames_have_consistent_dimensions() {
        let Some(mut cap) = open_or_skip() else {
            return;
        };
        let expected_w = cap.format().dimensions[0];
        let expected_h = cap.format().dimensions[1];
        cap.start().expect("start should succeed");

        let mut count = 0u32;
        let deadline = Instant::now() + Duration::from_secs(5);
        while count < 3 && Instant::now() < deadline {
            match cap.pop_frame() {
                Ok(Some(frame)) => {
                    assert_eq!(frame.width(), expected_w, "frame {count} width mismatch");
                    assert_eq!(frame.height(), expected_h, "frame {count} height mismatch");
                    count += 1;
                }
                Ok(None) => {}
                Err(e) => panic!("pop_frame error at frame {count}: {e}"),
            }
        }
        assert!(count >= 3, "expected >=3 frames in 5s, got {count}");

        cap.stop().ok();
    }
}
