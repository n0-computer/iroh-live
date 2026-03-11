//! PipeWire screen and camera capture with DMA-BUF zero-copy support.
//!
//! Uses the xdg-desktop-portal for session negotiation:
//! - **ScreenCast portal** (`org.freedesktop.portal.ScreenCast`): user picks a
//!   monitor or window in the compositor dialog.
//! - **Camera portal** (`org.freedesktop.portal.Camera`): requests camera
//!   permission, then enumerates camera nodes in the PipeWire graph.
//!
//! Both portals return a PipeWire fd + node_id. The stream consumption code
//! is 100% shared — only the portal setup differs.
//!
//! # DMA-BUF Zero-Copy
//!
//! When PipeWire delivers DMA-BUF-backed buffers (`SPA_DATA_DmaBuf`),
//! `data.data()` returns `None` because the kernel does not automatically
//! map them. We mmap the DMA-BUF fd to extract pixel data within the
//! process callback, then unmap immediately. This avoids the
//! compositor→SHM copy overhead while keeping the buffer lifecycle simple.
//!
//! True zero-copy (holding the PipeWire buffer across frames) would require
//! more complex buffer management to prevent the producer from recycling
//! the buffer before the consumer finishes.
//!
//! # Raspberry Pi
//!
//! PipeWire's `spa-libcamera` plugin exposes Pi cameras (v2/v3 via libcamera)
//! as PipeWire nodes. The camera portal enumerates them transparently. DMA-BUF
//! output from libcamera is preserved through PipeWire when the spa plugin and
//! consumer both support it.

use std::io::Cursor;
use std::os::fd::OwnedFd;
use std::os::unix::io::BorrowedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use libspa::buffer::DataType;
use libspa::param::ParamType;
use libspa::param::format::{FormatProperties, MediaSubtype, MediaType};
use libspa::param::video::VideoFormat as SpaVideoFormat;
use libspa::pod::serialize::PodSerializer;
use libspa::pod::{ChoiceValue, Object, Pod, Property, Value};
use libspa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};
use pipewire as pw;
use pw::stream::StreamFlags;
use tracing::{debug, info, warn};

use rusty_codecs::format::{Nv12Planes, PixelFormat, VideoFormat, VideoFrame};
use rusty_codecs::traits::VideoSource;

use crate::types::ScreenConfig;

// ── Shared PipeWire stream infrastructure ───────────────────────────

/// State shared between PipeWire callbacks.
struct CaptureState {
    frame_tx: mpsc::Sender<VideoFrame>,
    init_tx: Option<mpsc::Sender<Result<(u32, u32)>>>,
    width: u32,
    height: u32,
    spa_format: u32,
}

/// Builds an `EnumFormat` pod requesting common video formats.
///
/// Preference order: BGRx (screen compositors), BGRA, RGBA, NV12.
/// Size and framerate are left as ranges so the producer picks optimal values.
fn build_enum_format_pod() -> Result<Vec<u8>> {
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw())),
            ),
            Property::new(
                FormatProperties::VideoFormat.as_raw(),
                Value::Choice(ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: Id(SpaVideoFormat::BGRx.as_raw()),
                        alternatives: vec![
                            Id(SpaVideoFormat::BGRx.as_raw()),
                            Id(SpaVideoFormat::BGRA.as_raw()),
                            Id(SpaVideoFormat::RGBA.as_raw()),
                            Id(SpaVideoFormat::RGBx.as_raw()),
                            Id(SpaVideoFormat::NV12.as_raw()),
                            Id(SpaVideoFormat::YUY2.as_raw()),
                        ],
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoSize.as_raw(),
                Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle {
                            width: 1920,
                            height: 1080,
                        },
                        min: Rectangle {
                            width: 1,
                            height: 1,
                        },
                        max: Rectangle {
                            width: 8192,
                            height: 4320,
                        },
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoFramerate.as_raw(),
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction { num: 30, denom: 1 },
                        min: Fraction { num: 0, denom: 1 },
                        max: Fraction { num: 144, denom: 1 },
                    },
                ))),
            ),
        ],
    };

    let (cursor, _) = PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .map_err(|e| anyhow::anyhow!("failed to serialize format pod: {e:?}"))?;

    Ok(cursor.into_inner())
}

/// Parses a negotiated `Format` pod into `(width, height, spa_video_format)`.
fn parse_format_pod(pod: &Pod) -> Option<(u32, u32, u32)> {
    // Get pod bytes: header (8 bytes) + body.
    let raw = pod as *const Pod as *const libspa::sys::spa_pod;
    let total_size = unsafe { std::mem::size_of::<libspa::sys::spa_pod>() + (*raw).size as usize };
    let bytes = unsafe { std::slice::from_raw_parts(raw as *const u8, total_size) };

    let (_, value) =
        libspa::pod::deserialize::PodDeserializer::deserialize_from::<Value>(bytes).ok()?;

    if let Value::Object(obj) = value {
        let mut width = 0u32;
        let mut height = 0u32;
        let mut format = 0u32;

        for prop in &obj.properties {
            if prop.key == FormatProperties::VideoFormat.as_raw()
                && let Value::Id(id) = &prop.value
            {
                format = id.0;
            } else if prop.key == FormatProperties::VideoSize.as_raw()
                && let Value::Rectangle(rect) = &prop.value
            {
                width = rect.width;
                height = rect.height;
            }
        }

        if width > 0 && height > 0 {
            return Some((width, height, format));
        }
    }

    None
}

/// Returns bytes-per-pixel for stride calculation fallback.
fn spa_format_bpp(format: u32) -> u32 {
    if format == SpaVideoFormat::BGRx.as_raw()
        || format == SpaVideoFormat::BGRA.as_raw()
        || format == SpaVideoFormat::RGBA.as_raw()
        || format == SpaVideoFormat::RGBx.as_raw()
    {
        4
    } else if format == SpaVideoFormat::NV12.as_raw() {
        1 // Y stride = width
    } else if format == SpaVideoFormat::YUY2.as_raw() {
        2
    } else {
        4
    }
}

/// Converts a CPU-mapped PipeWire buffer into a `VideoFrame`.
///
/// Avoids unnecessary color-space conversions: NV12 and BGRA are passed
/// through directly, only formats that have no native `VideoFrame` variant
/// are converted to RGBA.
fn buffer_to_frame(
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    spa_format: u32,
) -> Option<VideoFrame> {
    let w = width as usize;
    let h = height as usize;
    let s = stride as usize;

    if spa_format == SpaVideoFormat::NV12.as_raw() {
        return nv12_passthrough(data, width, height, stride);
    }

    if spa_format == SpaVideoFormat::BGRA.as_raw() {
        // Pass through as BGRA — downstream handles conversion if needed.
        let packed = copy_rows(data, w, h, s, 4);
        return Some(VideoFrame::new_packed(
            packed.into(),
            width,
            height,
            PixelFormat::Bgra,
            Duration::ZERO,
        ));
    }

    if spa_format == SpaVideoFormat::BGRx.as_raw() {
        // BGRx → BGRA with A=255.
        let mut buf = copy_rows(data, w, h, s, 4);
        for pixel in buf.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        return Some(VideoFrame::new_packed(
            buf.into(),
            width,
            height,
            PixelFormat::Bgra,
            Duration::ZERO,
        ));
    }

    if spa_format == SpaVideoFormat::RGBA.as_raw() {
        let packed = copy_rows(data, w, h, s, 4);
        return Some(VideoFrame::new_rgba(packed.into(), width, height));
    }

    if spa_format == SpaVideoFormat::RGBx.as_raw() {
        let mut packed = copy_rows(data, w, h, s, 4);
        for pixel in packed.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        return Some(VideoFrame::new_rgba(packed.into(), width, height));
    }

    if spa_format == SpaVideoFormat::YUY2.as_raw() {
        return yuy2_to_rgba(data, width, height, stride);
    }

    warn!(spa_format, "unsupported PipeWire video format");
    None
}

/// Copies pixel rows from a strided buffer into a tightly-packed buffer.
fn copy_rows(data: &[u8], w: usize, h: usize, stride: usize, bpp: usize) -> Vec<u8> {
    let row_bytes = w * bpp;
    if stride == row_bytes {
        // Fast path: no padding, single memcpy.
        let total = row_bytes * h;
        return data[..total.min(data.len())].to_vec();
    }
    let mut out = vec![0u8; row_bytes * h];
    for y in 0..h {
        let src_start = y * stride;
        let src_end = (src_start + row_bytes).min(data.len());
        let dst_start = y * row_bytes;
        let copy_len = src_end.saturating_sub(src_start).min(row_bytes);
        out[dst_start..dst_start + copy_len]
            .copy_from_slice(&data[src_start..src_start + copy_len]);
    }
    out
}

/// Passes NV12 data through as `VideoFrame::new_nv12` without color conversion.
fn nv12_passthrough(data: &[u8], width: u32, height: u32, stride: u32) -> Option<VideoFrame> {
    let h = height as usize;
    let s = stride as usize;
    let y_size = s * h;
    let uv_size = s * (h / 2);
    if data.len() < y_size + uv_size {
        warn!(
            data_len = data.len(),
            expected = y_size + uv_size,
            "NV12 buffer too small"
        );
        return None;
    }
    Some(VideoFrame::new_nv12(Nv12Planes {
        y_data: data[..y_size].to_vec(),
        y_stride: stride,
        uv_data: data[y_size..y_size + uv_size].to_vec(),
        uv_stride: stride,
        width,
        height,
    }))
}

/// Converts YUY2 (packed YUYV 4:2:2) to RGBA using integer BT.601 math.
fn yuy2_to_rgba(data: &[u8], width: u32, height: u32, stride: u32) -> Option<VideoFrame> {
    let w = width as usize;
    let h = height as usize;
    let s = stride as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = &data[y * s..];
        for x in (0..w).step_by(2) {
            let base = x * 2;
            let y0 = row[base] as i32;
            let cb = row[base + 1] as i32 - 128;
            let y1 = row[base + 2] as i32;
            let cr = row[base + 3] as i32 - 128;

            for (i, yv) in [(0usize, y0), (1, y1)] {
                let r = (yv + ((359 * cr + 128) >> 8)).clamp(0, 255) as u8;
                let g = (yv + ((-88 * cb - 183 * cr + 128) >> 8)).clamp(0, 255) as u8;
                let b = (yv + ((454 * cb + 128) >> 8)).clamp(0, 255) as u8;
                let di = (y * w + x + i) * 4;
                rgba[di] = r;
                rgba[di + 1] = g;
                rgba[di + 2] = b;
                rgba[di + 3] = 255;
            }
        }
    }
    Some(VideoFrame::new_rgba(rgba.into(), width, height))
}

/// Mmaps a DMA-BUF fd, copies the data, and unmaps immediately.
///
/// Returns `None` if the mmap or data extraction fails.
fn dmabuf_to_frame(
    fd: std::os::unix::io::RawFd,
    size: usize,
    offset: usize,
    stride: u32,
    width: u32,
    height: u32,
    spa_format: u32,
) -> Option<VideoFrame> {
    use nix::sys::mman::{MapFlags, MmapAdvise, ProtFlags, madvise, mmap, munmap};

    if size == 0 {
        return None;
    }

    // Safety: we mmap the DMA-BUF fd read-only and copy out within this scope.
    let ptr = unsafe {
        mmap(
            None,
            std::num::NonZeroUsize::new(size)?,
            ProtFlags::PROT_READ,
            MapFlags::MAP_SHARED,
            BorrowedFd::borrow_raw(fd),
            0,
        )
        .ok()?
    };

    // Hint to the kernel that we'll read sequentially.
    unsafe {
        let _ = madvise(ptr, size, MmapAdvise::MADV_SEQUENTIAL);
    }

    let slice = unsafe { std::slice::from_raw_parts(ptr.as_ptr() as *const u8, size) };
    let data = &slice[offset..];

    let frame = buffer_to_frame(data, width, height, stride, spa_format);

    unsafe {
        let _ = munmap(ptr, size);
    }

    frame
}

/// Runs a PipeWire capture stream, blocking until stopped.
///
/// The `init_tx` channel receives `Ok((width, height))` once the format is
/// negotiated, or `Err` if setup fails. Frames are sent to `frame_tx`.
/// Set `should_stop` to `true` to terminate the capture loop.
fn run_pipewire_stream(
    fd: OwnedFd,
    node_id: Option<u32>,
    frame_tx: mpsc::Sender<VideoFrame>,
    init_tx: mpsc::Sender<Result<(u32, u32)>>,
    should_stop: Arc<AtomicBool>,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow::anyhow!("failed to create PipeWire main loop: {e}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow::anyhow!("failed to create PipeWire context: {e}"))?;
    let core = context
        .connect_fd_rc(fd, None)
        .map_err(|e| anyhow::anyhow!("failed to connect PipeWire fd: {e}"))?;

    let stream = pw::stream::StreamRc::new(
        core,
        "rusty-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
        },
    )
    .map_err(|e| anyhow::anyhow!("failed to create PipeWire stream: {e}"))?;

    // Build format negotiation pod.
    let format_bytes = build_enum_format_pod()?;
    let pod = Pod::from_bytes(&format_bytes).context("invalid format pod")?;

    let state = CaptureState {
        frame_tx,
        init_tx: Some(init_tx),
        width: 0,
        height: 0,
        spa_format: 0,
    };

    // Spawn a stopper thread that quits the mainloop when stop is requested.
    // Safety: pw_main_loop_quit is documented as thread-safe in PipeWire.
    // Cast to usize to bypass Send constraints on raw pointers.
    let mainloop_addr = mainloop.as_raw_ptr() as usize;
    let stop_flag = should_stop.clone();
    std::thread::Builder::new()
        .name("pw-stopper".into())
        .spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            unsafe {
                pw::sys::pw_main_loop_quit(mainloop_addr as *mut pw::sys::pw_main_loop);
            }
        })
        .context("failed to spawn PipeWire stopper thread")?;

    let _listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed(|_stream, state, id, param| {
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else { return };

            if let Some((w, h, fmt)) = parse_format_pod(param) {
                state.width = w;
                state.height = h;
                state.spa_format = fmt;

                debug!(
                    width = w,
                    height = h,
                    format = fmt,
                    "PipeWire format negotiated"
                );

                if let Some(tx) = state.init_tx.take() {
                    let _ = tx.send(Ok((w, h)));
                }
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }

            let data = &mut datas[0];
            let chunk = data.chunk();
            let size = chunk.size() as usize;
            if size == 0 {
                return;
            }

            // Read stride from the chunk. PipeWire producers set this to the
            // actual row pitch, which may differ from width * bpp due to
            // alignment or tiling. Fall back to width * bpp when the
            // producer leaves stride at 0 (happens with some sources
            // during initial negotiation).
            let chunk_stride = chunk.stride();
            let stride = if chunk_stride > 0 {
                chunk_stride as u32
            } else {
                let bpp = spa_format_bpp(state.spa_format);
                state.width * bpp
            };
            let offset = chunk.offset() as usize;
            let data_type = data.type_();

            if data_type == DataType::DmaBuf {
                // DMA-BUF path: data.data() returns None — mmap the fd.
                let fd = data.fd();
                // The total mappable size includes offset + pixel data.
                let map_size = offset + size;
                if let Some(frame) = dmabuf_to_frame(
                    fd,
                    map_size,
                    offset,
                    stride,
                    state.width,
                    state.height,
                    state.spa_format,
                ) {
                    let _ = state.frame_tx.send(frame);
                } else {
                    debug!("DMA-BUF frame extraction failed");
                }
            } else if let Some(slice) = data.data() {
                // CPU-mapped buffer path (MemPtr / MemFd with MAP_BUFFERS).
                let start = offset.min(slice.len());
                let end = (offset + size).min(slice.len());
                let usable = &slice[start..end];
                if let Some(frame) =
                    buffer_to_frame(usable, state.width, state.height, stride, state.spa_format)
                {
                    let _ = state.frame_tx.send(frame);
                }
            } else {
                debug!(?data_type, "unsupported PipeWire buffer data type");
            }
        })
        .state_changed(|_stream, state, old, new| {
            debug!(?old, ?new, "PipeWire stream state changed");
            if let pw::stream::StreamState::Error(ref msg) = new {
                warn!("PipeWire stream error: {msg}");
                if let Some(tx) = state.init_tx.take() {
                    let _ = tx.send(Err(anyhow::anyhow!("PipeWire stream error: {msg}")));
                }
            }
        })
        .register()
        .map_err(|e| anyhow::anyhow!("failed to register PipeWire listener: {e}"))?;

    stream
        .connect(
            libspa::utils::Direction::Input,
            node_id,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut [pod],
        )
        .map_err(|e| anyhow::anyhow!("failed to connect PipeWire stream: {e}"))?;

    debug!("PipeWire main loop running");
    mainloop.run();
    debug!("PipeWire main loop exited");

    Ok(())
}

// ── Portal helpers ──────────────────────────────────────────────────

/// Runs the ashpd ScreenCast portal negotiation on a temporary tokio runtime.
fn portal_screen_capture(show_cursor: bool) -> Result<(OwnedFd, u32)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime for portal")?;

    rt.block_on(async {
        use ashpd::desktop::PersistMode;
        use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};

        let proxy = Screencast::new()
            .await
            .context("failed to create ScreenCast proxy")?;
        let session = proxy
            .create_session()
            .await
            .context("failed to create ScreenCast session")?;

        let cursor_mode = if show_cursor {
            CursorMode::Embedded
        } else {
            CursorMode::Hidden
        };

        proxy
            .select_sources(
                &session,
                cursor_mode,
                SourceType::Monitor.into(),
                false,
                None,
                PersistMode::DoNot,
            )
            .await
            .context("select_sources failed")?
            .response()
            .context("select_sources response failed")?;

        let streams = proxy
            .start(&session, None)
            .await
            .context("start failed")?
            .response()
            .context("start response failed (user cancelled?)")?;

        let stream = streams
            .streams()
            .first()
            .context("no streams returned from ScreenCast portal")?;

        let node_id = stream.pipe_wire_node_id();
        let fd = proxy
            .open_pipe_wire_remote(&session)
            .await
            .context("failed to open PipeWire remote")?;

        info!(node_id, "ScreenCast portal negotiated");
        Ok((fd, node_id))
    })
}

/// Runs the ashpd Camera portal negotiation on a temporary tokio runtime.
fn portal_camera_capture() -> Result<OwnedFd> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime for portal")?;

    rt.block_on(async {
        use ashpd::desktop::camera::Camera;

        let proxy = Camera::new()
            .await
            .context("failed to create Camera proxy")?;

        if !proxy
            .is_present()
            .await
            .context("camera presence check failed")?
        {
            anyhow::bail!("no camera available via PipeWire portal");
        }

        proxy
            .request_access()
            .await
            .context("camera access request failed")?
            .response()
            .context("camera access denied")?;

        let fd = proxy
            .open_pipe_wire_remote()
            .await
            .context("failed to open camera PipeWire remote")?;

        info!("Camera portal negotiated");
        Ok(fd)
    })
}

// ── PipeWire Screen Capturer ────────────────────────────────────────

/// Captures screen content via PipeWire ScreenCast portal with DMA-BUF
/// zero-copy when the compositor supports it.
#[derive(derive_more::Debug)]
pub struct PipeWireScreenCapturer {
    width: u32,
    height: u32,
    #[debug(skip)]
    rx: mpsc::Receiver<VideoFrame>,
    #[debug(skip)]
    should_stop: Arc<AtomicBool>,
}

impl PipeWireScreenCapturer {
    /// Creates a new PipeWire screen capturer.
    ///
    /// Triggers the xdg-desktop-portal ScreenCast dialog where the user
    /// selects which monitor or window to share. Blocks until the user makes
    /// a selection (or cancels).
    pub fn new(config: &ScreenConfig) -> Result<Self> {
        let show_cursor = config.show_cursor;
        let (frame_tx, frame_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        let should_stop = Arc::new(AtomicBool::new(false));
        let stop_flag = should_stop.clone();

        std::thread::Builder::new()
            .name("pw-screen".into())
            .spawn(move || {
                let result = (|| {
                    let (fd, node_id) = portal_screen_capture(show_cursor)?;
                    run_pipewire_stream(fd, Some(node_id), frame_tx, init_tx.clone(), stop_flag)
                })();

                if let Err(e) = result {
                    let _ = init_tx.send(Err(e));
                }
            })
            .context("failed to spawn PipeWire screen capture thread")?;

        let (width, height) = init_rx
            .recv_timeout(Duration::from_secs(60))
            .context("PipeWire screen capture init timeout")?
            .context("PipeWire screen capture init failed")?;

        info!(width, height, "PipeWire screen capture started");

        Ok(Self {
            width,
            height,
            rx: frame_rx,
            should_stop,
        })
    }
}

impl VideoSource for PipeWireScreenCapturer {
    fn name(&self) -> &str {
        "pipewire-screen"
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
        self.should_stop.store(true, Ordering::Relaxed);
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

impl Drop for PipeWireScreenCapturer {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Relaxed);
    }
}

// ── PipeWire Camera Capturer ────────────────────────────────────────

/// Captures camera frames via PipeWire Camera portal with DMA-BUF zero-copy
/// when the camera source (V4L2, libcamera) supports it.
#[derive(derive_more::Debug)]
pub struct PipeWireCameraCapturer {
    width: u32,
    height: u32,
    #[debug(skip)]
    rx: mpsc::Receiver<VideoFrame>,
    #[debug(skip)]
    should_stop: Arc<AtomicBool>,
}

impl PipeWireCameraCapturer {
    /// Creates a new PipeWire camera capturer.
    ///
    /// Triggers the Camera portal permission dialog if not already granted.
    /// Uses `PW_ID_ANY` with `AUTOCONNECT` — PipeWire routes to the first
    /// available camera node exposed by the portal.
    pub fn new() -> Result<Self> {
        let (frame_tx, frame_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        let should_stop = Arc::new(AtomicBool::new(false));
        let stop_flag = should_stop.clone();

        std::thread::Builder::new()
            .name("pw-camera".into())
            .spawn(move || {
                let result = (|| {
                    let fd = portal_camera_capture()?;
                    // node_id = None → PW_ID_ANY, AUTOCONNECT routes to camera.
                    run_pipewire_stream(fd, None, frame_tx, init_tx.clone(), stop_flag)
                })();

                if let Err(e) = result {
                    let _ = init_tx.send(Err(e));
                }
            })
            .context("failed to spawn PipeWire camera capture thread")?;

        let (width, height) = init_rx
            .recv_timeout(Duration::from_secs(30))
            .context("PipeWire camera capture init timeout")?
            .context("PipeWire camera capture init failed")?;

        info!(width, height, "PipeWire camera capture started");

        Ok(Self {
            width,
            height,
            rx: frame_rx,
            should_stop,
        })
    }
}

impl VideoSource for PipeWireCameraCapturer {
    fn name(&self) -> &str {
        "pipewire-camera"
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
        self.should_stop.store(true, Ordering::Relaxed);
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

impl Drop for PipeWireCameraCapturer {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Relaxed);
    }
}
