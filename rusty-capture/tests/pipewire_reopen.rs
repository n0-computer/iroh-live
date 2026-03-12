//! Regression test: PipeWire camera/screen capture must not hang when
//! reopened after closing.
//!
//! Root cause was ashpd caching the D-Bus connection in a process-global
//! `OnceLock<zbus::Connection>` tied to the first tokio runtime. When the
//! first portal thread's runtime shut down, subsequent `Camera::new()` /
//! `Screencast::new()` calls hung forever on the dead connection.
//!
//! These tests require a real PipeWire daemon and camera. They are skipped
//! gracefully when hardware is unavailable.

#[cfg(all(target_os = "linux", feature = "pipewire"))]
mod pipewire_reopen {
    use std::time::{Duration, Instant};

    use rusty_capture::types::CameraConfig;
    use rusty_capture::{PipeWireCameraCapturer, VideoSource};

    /// Tries to open a PipeWire camera. Returns `None` if no camera is
    /// available (skip the test gracefully).
    fn open_camera() -> Option<PipeWireCameraCapturer> {
        match PipeWireCameraCapturer::new(&CameraConfig::default()) {
            Ok(cap) => Some(cap),
            Err(e) => {
                eprintln!("SKIPPED (camera unavailable): {e}");
                None
            }
        }
    }

    fn wait_for_frame(cap: &mut PipeWireCameraCapturer, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            match cap.pop_frame() {
                Ok(Some(_)) => return true,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => return false,
            }
        }
        false
    }

    #[test]
    fn camera_reopen_does_not_hang() {
        // First open — establishes the D-Bus connection.
        let Some(mut cap1) = open_camera() else {
            return;
        };
        cap1.start().ok();
        assert!(
            wait_for_frame(&mut cap1, Duration::from_secs(5)),
            "first open: no frame within 5s"
        );
        eprintln!("first camera open OK, dropping...");
        drop(cap1);

        // Brief pause to let cleanup propagate.
        std::thread::sleep(Duration::from_millis(500));

        // Second open — this is where the hang used to occur.
        // Use a thread with a timeout so the test doesn't block forever.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("reopen-test".into())
            .spawn(move || {
                let result = PipeWireCameraCapturer::new(&CameraConfig::default());
                let _ = tx.send(result);
            })
            .expect("spawn reopen thread");

        let cap2_result = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("second camera open timed out (hang detected!)");

        let mut cap2 = cap2_result.expect("second camera open failed");
        cap2.start().ok();
        assert!(
            wait_for_frame(&mut cap2, Duration::from_secs(5)),
            "second open: no frame within 5s"
        );
        eprintln!("second camera open OK");
    }

    #[test]
    fn camera_delivers_dmabuf_frames() {
        let Some(mut cap) = open_camera() else {
            return;
        };
        cap.start().ok();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            match cap.pop_frame() {
                Ok(Some(frame)) => {
                    eprintln!(
                        "Frame: {}x{}, is_gpu={}",
                        frame.width(),
                        frame.height(),
                        frame.is_gpu()
                    );
                    // DMA-BUF should be negotiated on machines with
                    // PipeWire + V4L2 EXPBUF support.
                    assert!(
                        frame.is_gpu(),
                        "expected DMA-BUF GPU frame from PipeWire camera"
                    );
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => panic!("pop_frame error: {e}"),
            }
        }
        panic!("no frame within 5s");
    }
}
