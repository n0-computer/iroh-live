//! Integration test: PipeWire camera → VAAPI encode → VAAPI decode → wgpu DMA-BUF import.
//!
//! Confirms the full zero-copy pipeline works end-to-end on hardware with:
//! - PipeWire + camera (DMA-BUF capture)
//! - VAAPI (GPU encode + decode)
//! - Vulkan/wgpu (DMA-BUF import for rendering)
//!
//! Skips gracefully when hardware is unavailable.
//!
//! Run with:
//! ```sh
//! cargo test -p moq-media --features "capture-camera,vaapi,dmabuf-import" \
//!     --test zero_copy_pipeline -- --ignored --nocapture
//! ```
//!

#[cfg(all(
    target_os = "linux",
    feature = "capture-camera",
    feature = "vaapi",
    feature = "dmabuf-import",
))]
mod zero_copy {
    use std::time::{Duration, Instant};

    use moq_media::{
        capture::PipeWireCameraCapturer,
        codec::{VaapiDecoder, VaapiEncoder},
        format::{DecodeConfig, MediaPacket, NativeFrameHandle, VideoFrame, VideoPreset},
        render::{create_device_with_dmabuf_extensions, dmabuf_import},
        traits::{VideoDecoder, VideoEncoder, VideoEncoderFactory, VideoSource},
    };
    use rusty_capture::types::CameraConfig;

    /// Opens a PipeWire camera. Returns `None` if unavailable.
    fn open_camera() -> Option<PipeWireCameraCapturer> {
        match PipeWireCameraCapturer::new(&CameraConfig::default()) {
            Ok(cap) => Some(cap),
            Err(e) => {
                eprintln!("SKIPPED (camera unavailable): {e}");
                None
            }
        }
    }

    /// Captures up to `count` frames with a per-frame timeout.
    fn capture_frames(
        cap: &mut PipeWireCameraCapturer,
        count: usize,
        per_frame_timeout: Duration,
    ) -> Vec<VideoFrame> {
        let mut frames = Vec::new();
        for _ in 0..count {
            let start = Instant::now();
            loop {
                if start.elapsed() > per_frame_timeout {
                    break;
                }
                match cap.pop_frame() {
                    Ok(Some(f)) => {
                        frames.push(f);
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                    Err(_) => break,
                }
            }
        }
        frames
    }

    /// Creates a headless wgpu device with DMA-BUF extensions.
    fn create_wgpu_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        match create_device_with_dmabuf_extensions(&adapter) {
            Ok(pair) => Some(pair),
            Err(e) => {
                eprintln!("SKIPPED (wgpu DMA-BUF device creation failed): {e}");
                None
            }
        }
    }

    #[test]
    #[ignore = "requires PipeWire camera + VAAPI + Vulkan GPU hardware"]
    fn zero_copy_camera_vaapi_wgpu() {
        // Step 1: Open PipeWire camera and capture DMA-BUF frames.
        let Some(mut cam) = open_camera() else {
            return;
        };
        cam.start().ok();

        let frames = capture_frames(&mut cam, 30, Duration::from_secs(2));
        drop(cam);
        if frames.is_empty() {
            eprintln!("SKIPPED (no frames from camera)");
            return;
        }
        if !frames[0].is_gpu() {
            eprintln!("SKIPPED (camera delivers CPU frames, not DMA-BUF)");
            return;
        }
        assert!(
            frames[0].native_handle().is_some(),
            "GPU frame should have a DMA-BUF handle"
        );
        eprintln!(
            "step 1: captured {} DMA-BUF frames ({}x{})",
            frames.len(),
            frames[0].dimensions[0],
            frames[0].dimensions[1],
        );

        // Step 2: VAAPI encode all captured frames.
        let mut encoder =
            VaapiEncoder::with_preset(VideoPreset::P360).expect("VAAPI encoder should initialize");
        let enc_config = encoder.config();

        let mut encoded = Vec::new();
        for frame in frames {
            encoder
                .push_frame(frame)
                .expect("VAAPI encode should succeed");
            while let Some(pkt) = encoder.pop_packet().expect("pop_packet failed") {
                let mut payload = buf_list::BufList::new();
                payload.push_chunk(pkt.payload);
                encoded.push(MediaPacket {
                    timestamp: pkt.timestamp,
                    payload,
                    is_keyframe: pkt.is_keyframe,
                });
            }
        }
        assert!(
            !encoded.is_empty(),
            "encoder should produce at least one packet"
        );
        eprintln!("step 2: VAAPI encoded {} packets", encoded.len());

        // Step 3: VAAPI decode → GPU frames with DMA-BUF.
        //
        // VAAPI decoder uses a small frame pool (~4 buffers) with BlockingMode::Blocking.
        // To avoid blocking on pool exhaustion, pop each frame immediately after the
        // push_packet that produces it, but only keep the latest GPU frame alive.
        let decode_config = DecodeConfig::default();
        let mut decoder = VaapiDecoder::new(&enc_config, &decode_config)
            .expect("VAAPI decoder should initialize");

        let mut decoded_count = 0usize;
        let mut last_decoded: Option<VideoFrame> = None;
        for pkt in &encoded {
            decoder
                .push_packet(pkt.clone())
                .expect("decode push_packet failed");
            while let Some(frame) = decoder.pop_frame().expect("decode pop_frame failed") {
                decoded_count += 1;
                // Drop previous frame to free the VA surface pool slot.
                last_decoded = Some(frame);
            }
        }
        eprintln!(
            "step 3: VAAPI decoded {} frames from {} packets",
            decoded_count,
            encoded.len(),
        );
        assert!(
            decoded_count > 0,
            "decoder should produce at least one frame from {} packets",
            encoded.len(),
        );
        let decoded = last_decoded.unwrap();

        // Step 4: Verify decoded frame is GPU with DMA-BUF.
        assert!(decoded.is_gpu(), "decoded frame should be a GPU frame");
        let native = decoded.native_handle();
        assert!(
            native.is_some(),
            "decoded GPU frame should have a DMA-BUF handle"
        );
        let NativeFrameHandle::DmaBuf(ref dmabuf_info) = native.unwrap() else {
            panic!("expected DmaBuf native handle");
        };
        eprintln!(
            "step 4: decoded DMA-BUF {}x{}, modifier=0x{:x}, planes={}",
            dmabuf_info.coded_width,
            dmabuf_info.coded_height,
            dmabuf_info.modifier,
            dmabuf_info.planes.len(),
        );

        // Step 5: Import decoded DMA-BUF into wgpu.
        //
        // On Intel ANV, destroying Vulkan images that wrap imported DMA-BUFs
        // can SIGABRT when the source VA surface is still live. We work
        // around this by leaking the wgpu resources (device + imported
        // textures) — the OS reclaims them on process exit.
        let Some((device, _queue)) = create_wgpu_device() else {
            return;
        };
        let Some(mut importer) = dmabuf_import::DmaBufImporter::new(&device) else {
            eprintln!("SKIPPED (DmaBufImporter::new returned None)");
            return;
        };

        let imported = importer
            .import_nv12(&device, dmabuf_info)
            .expect("DMA-BUF import into wgpu should succeed");
        // coded dimensions may be macroblock-aligned (e.g. 368 vs display 360).
        assert!(imported.width > 0 && imported.height > 0);
        eprintln!(
            "step 5: wgpu DMA-BUF import OK ({}x{})",
            imported.width, imported.height
        );

        // Leak wgpu resources to avoid Intel ANV SIGABRT on teardown.
        std::mem::forget(imported);
        std::mem::forget(importer);
        std::mem::forget(device);

        eprintln!("PASS: full zero-copy pipeline PipeWire → VAAPI → wgpu confirmed");
    }
}
