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
//! the FD is dup'd and wrapped in a [`GpuFrame`] with a
//! [`NativeFrameHandle::DmaBuf`]. This allows the VAAPI encoder to import
//! the buffer directly as a VA surface — no CPU round-trip.
//!
//! The dup'd FD is independent of PipeWire's buffer lifecycle, so the
//! producer can recycle its buffer immediately while the frame remains
//! valid until dropped. For rendering or software encode fallback, the
//! frame's `download_rgba()` mmaps the FD on demand.
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

use rusty_codecs::format::{
    DmaBufInfo, DmaBufPlaneInfo, GpuFrame, GpuFrameInner, GpuPixelFormat, NativeFrameHandle,
    Nv12Planes, PixelFormat, VideoFormat, VideoFrame,
};
use rusty_codecs::traits::VideoSource;

use crate::types::{CameraConfig, ScreenConfig};

// ── Shared PipeWire stream infrastructure ───────────────────────────

/// Initializes PipeWire exactly once per process.
///
/// `pw_init` is documented as idempotent, but calling it concurrently from
/// multiple threads can race on global plugin loading. A `Once` guard
/// removes that concern entirely.
fn ensure_pw_init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(pw::init);
}

/// Returns a shared tokio runtime for portal D-Bus interactions.
///
/// ashpd caches the D-Bus session connection in a process-global `OnceLock`.
/// That connection is bound to the tokio runtime that first created it. If
/// each portal call spins up its own short-lived runtime, the cached
/// connection dies when the first runtime shuts down, causing subsequent
/// `Camera::new()` / `Screencast::new()` calls to hang forever.
///
/// This function lazily creates a single-threaded tokio runtime (on a
/// dedicated OS thread) that lives for the entire process, keeping the
/// D-Bus connection alive across capture sessions.
fn portal_runtime() -> &'static tokio::runtime::Handle {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Handle> = OnceLock::new();
    RT.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("pw-dbus-rt".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create portal tokio runtime");
                tx.send(rt.handle().clone())
                    .expect("failed to send portal runtime handle");
                // Block forever — runtime must outlive the process.
                rt.block_on(std::future::pending::<()>());
            })
            .expect("failed to spawn portal runtime thread");
        rx.recv().expect("portal runtime thread died before sending handle")
    })
}

/// Maps a SPA video format to a DRM fourcc code, if known.
fn spa_format_to_drm_fourcc(spa_format: u32) -> Option<u32> {
    // DRM fourcc constants (from drm_fourcc.h)
    const DRM_FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
    const DRM_FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
    const DRM_FORMAT_ABGR8888: u32 = u32::from_le_bytes(*b"AB24");
    const DRM_FORMAT_XBGR8888: u32 = u32::from_le_bytes(*b"XB24");
    const DRM_FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");
    const DRM_FORMAT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");

    if spa_format == SpaVideoFormat::BGRA.as_raw() {
        Some(DRM_FORMAT_ARGB8888)
    } else if spa_format == SpaVideoFormat::BGRx.as_raw() {
        Some(DRM_FORMAT_XRGB8888)
    } else if spa_format == SpaVideoFormat::RGBA.as_raw() {
        Some(DRM_FORMAT_ABGR8888)
    } else if spa_format == SpaVideoFormat::RGBx.as_raw() {
        Some(DRM_FORMAT_XBGR8888)
    } else if spa_format == SpaVideoFormat::NV12.as_raw() {
        Some(DRM_FORMAT_NV12)
    } else if spa_format == SpaVideoFormat::YUY2.as_raw() {
        Some(DRM_FORMAT_YUYV)
    } else {
        None
    }
}

/// A DMA-BUF-backed GPU frame from PipeWire.
///
/// Holds a dup'd file descriptor so the PipeWire buffer can be recycled
/// independently. The FD is closed when the frame is dropped.
#[derive(Debug)]
struct PipeWireDmaBufFrame {
    fd: OwnedFd,
    drm_format: u32,
    width: u32,
    height: u32,
    stride: u32,
    offset: usize,
    /// Total mappable size (for mmap fallback).
    map_size: usize,
    /// Original SPA format for CPU fallback conversion.
    spa_format: u32,
}

impl GpuFrameInner for PipeWireDmaBufFrame {
    fn download_rgba(&self) -> anyhow::Result<image::RgbaImage> {
        use nix::sys::mman::{MapFlags, ProtFlags, mmap, munmap};
        use std::os::unix::io::AsFd;

        let ptr = unsafe {
            mmap(
                None,
                std::num::NonZeroUsize::new(self.map_size)
                    .ok_or_else(|| anyhow::anyhow!("zero-size DMA-BUF"))?,
                ProtFlags::PROT_READ,
                MapFlags::MAP_SHARED,
                self.fd.as_fd(),
                0,
            )
            .map_err(|e| anyhow::anyhow!("DMA-BUF mmap failed: {e}"))?
        };

        let slice = unsafe { std::slice::from_raw_parts(ptr.as_ptr() as *const u8, self.map_size) };
        anyhow::ensure!(
            self.offset <= self.map_size,
            "DMA-BUF offset ({}) exceeds map size ({})",
            self.offset,
            self.map_size
        );
        let data = &slice[self.offset..];
        let frame = buffer_to_frame(data, self.width, self.height, self.stride, self.spa_format);

        unsafe {
            let _ = munmap(ptr, self.map_size);
        }

        frame
            .ok_or_else(|| anyhow::anyhow!("DMA-BUF pixel conversion failed"))?
            .rgba_image()
            .clone()
            .pipe(Ok)
    }

    fn gpu_pixel_format(&self) -> GpuPixelFormat {
        // Only NV12 DMA-BUFs take this path (see `dmabuf_to_frame`).
        GpuPixelFormat::Nv12
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn native_handle(&self) -> Option<NativeFrameHandle> {
        use std::os::unix::io::AsFd;

        let dup_fd = self.fd.as_fd().try_clone_to_owned().ok()?;
        Some(NativeFrameHandle::DmaBuf(DmaBufInfo {
            fd: dup_fd,
            modifier: 0, // LINEAR assumed for PipeWire DMA-BUFs
            drm_format: self.drm_format,
            coded_width: self.width,
            coded_height: self.height,
            display_width: self.width,
            display_height: self.height,
            planes: vec![DmaBufPlaneInfo {
                offset: self.offset as u32,
                pitch: self.stride,
            }],
        }))
    }
}

/// Pipe adapter for method chaining in expressions.
trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T: Sized> Pipe for T {}

/// State shared between PipeWire callbacks.
struct CaptureState {
    frame_tx: mpsc::Sender<VideoFrame>,
    init_tx: Option<mpsc::Sender<Result<(u32, u32)>>>,
    width: u32,
    height: u32,
    spa_format: u32,
    /// Log buffer type once on first successful frame delivery.
    logged_buffer_type: bool,
}

/// Builds an `EnumFormat` pod requesting common video formats.
///
/// Preference order: BGRx (screen compositors), BGRA, RGBA, NV12.
/// The `preferred_size` default hint tells the PipeWire producer which
/// resolution to prefer within the supported range. The `preferred_fps`
/// default hint does the same for frame rate.
fn build_enum_format_pod(preferred_size: Rectangle, preferred_fps: Fraction) -> Result<Vec<u8>> {
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
                        default: preferred_size,
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
                        default: preferred_fps,
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

/// Extracts an [`Id`] from a [`Value`], handling both fixed values and
/// single-element choices (some PipeWire producers wrap negotiated formats
/// in a `Choice::None`).
fn extract_id(value: &Value) -> Option<Id> {
    match value {
        Value::Id(id) => Some(*id),
        Value::Choice(ChoiceValue::Id(Choice(_, enum_val))) => match enum_val {
            ChoiceEnum::None(default) => Some(*default),
            ChoiceEnum::Enum { default, .. } => Some(*default),
            _ => None,
        },
        _ => None,
    }
}

/// Extracts a [`Rectangle`] from a [`Value`], handling both fixed values
/// and single-element choices.
fn extract_rectangle(value: &Value) -> Option<Rectangle> {
    match value {
        Value::Rectangle(r) => Some(*r),
        Value::Choice(ChoiceValue::Rectangle(Choice(_, enum_val))) => match enum_val {
            ChoiceEnum::None(default) => Some(*default),
            ChoiceEnum::Range { default, .. } => Some(*default),
            _ => None,
        },
        _ => None,
    }
}

/// Parses a negotiated `Format` pod into `(width, height, spa_video_format)`.
fn parse_format_pod(pod: &Pod) -> Option<(u32, u32, u32)> {
    // Get pod bytes: header (8 bytes) + body.
    let raw = pod as *const Pod as *const libspa::sys::spa_pod;
    let total_size = unsafe { std::mem::size_of::<libspa::sys::spa_pod>() + (*raw).size as usize };
    let bytes = unsafe { std::slice::from_raw_parts(raw as *const u8, total_size) };

    let (_, value) =
        match libspa::pod::deserialize::PodDeserializer::deserialize_from::<Value>(bytes) {
            Ok(v) => v,
            Err(e) => {
                warn!("failed to deserialize PipeWire format pod: {e:?}");
                return None;
            }
        };

    if let Value::Object(obj) = value {
        let mut width = 0u32;
        let mut height = 0u32;
        let mut format = 0u32;

        for prop in &obj.properties {
            if prop.key == FormatProperties::VideoFormat.as_raw() {
                if let Some(id) = extract_id(&prop.value) {
                    format = id.0;
                } else {
                    debug!(
                        key = prop.key,
                        "VideoFormat property has unexpected Value type"
                    );
                }
            } else if prop.key == FormatProperties::VideoSize.as_raw() {
                if let Some(rect) = extract_rectangle(&prop.value) {
                    width = rect.width;
                    height = rect.height;
                } else {
                    debug!(
                        key = prop.key,
                        "VideoSize property has unexpected Value type"
                    );
                }
            }
        }

        if width > 0 && height > 0 {
            return Some((width, height, format));
        }

        warn!(width, height, format, "PipeWire format pod missing size");
    } else {
        warn!("PipeWire format pod is not an Object");
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
    // YUY2 requires even width (2-pixel pairs).
    let w = (width & !1) as usize;
    let h = height as usize;
    let s = stride as usize;
    let expected = h * s;
    if data.len() < expected || w == 0 {
        warn!(
            data_len = data.len(),
            expected, width, "YUY2 buffer too small or invalid width"
        );
        return None;
    }
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let row_start = y * s;
        let row = &data[row_start..row_start + s];
        for x in (0..w).step_by(2) {
            let base = x * 2;
            if base + 3 >= row.len() {
                break;
            }
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
    Some(VideoFrame::new_rgba(rgba.into(), w as u32, height))
}

/// Creates a zero-copy GPU frame from a DMA-BUF, or falls back to mmap+copy.
///
/// Only NV12 DMA-BUFs take the zero-copy GPU path (wrapped as `GpuFrame`).
/// Other formats fall back to mmap+copy because `GpuPixelFormat` currently
/// only supports NV12, and non-NV12 DMA-BUFs would be misinterpreted by
/// downstream consumers.
///
/// Falls back to mmap+copy for formats without a known DRM mapping.
fn dmabuf_to_frame(
    fd: std::os::unix::io::RawFd,
    size: usize,
    offset: usize,
    stride: u32,
    width: u32,
    height: u32,
    spa_format: u32,
) -> Option<VideoFrame> {
    if size == 0 || offset >= size {
        return None;
    }

    // Zero-copy path: only for NV12, which is the sole GpuPixelFormat variant.
    // Other formats fall back to mmap+copy to avoid misreporting the pixel format.
    if spa_format == SpaVideoFormat::NV12.as_raw()
        && let Some(drm_format) = spa_format_to_drm_fourcc(spa_format)
    {
        let dup_fd = unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .ok()?;
        let gpu_frame = PipeWireDmaBufFrame {
            fd: dup_fd,
            drm_format,
            width,
            height,
            stride,
            offset,
            map_size: size,
            spa_format,
        };
        return Some(VideoFrame::new_gpu(
            GpuFrame::new(Arc::new(gpu_frame)),
            std::time::Duration::ZERO,
        ));
    }

    // Fallback: mmap + copy for non-NV12 or unknown formats.
    dmabuf_to_frame_cpu(fd, size, offset, stride, width, height, spa_format)
}

/// Fallback: mmap a DMA-BUF, copy pixel data to CPU, and unmap.
fn dmabuf_to_frame_cpu(
    fd: std::os::unix::io::RawFd,
    size: usize,
    offset: usize,
    stride: u32,
    width: u32,
    height: u32,
    spa_format: u32,
) -> Option<VideoFrame> {
    use nix::sys::mman::{MapFlags, MmapAdvise, ProtFlags, madvise, mmap, munmap};

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
    preferred_size: Rectangle,
    preferred_fps: Fraction,
    frame_tx: mpsc::Sender<VideoFrame>,
    init_tx: mpsc::Sender<Result<(u32, u32)>>,
    should_stop: Arc<AtomicBool>,
) -> Result<()> {
    ensure_pw_init();

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
    let format_bytes = build_enum_format_pod(preferred_size, preferred_fps)?;
    let pod = Pod::from_bytes(&format_bytes).context("invalid format pod")?;

    let state = CaptureState {
        frame_tx,
        init_tx: Some(init_tx),
        width: 0,
        height: 0,
        spa_format: 0,
        logged_buffer_type: false,
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
            } else {
                warn!("PipeWire param_changed(Format) failed to parse pod");
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
                    if !state.logged_buffer_type {
                        let path = if frame.is_gpu() {
                            "DMA-BUF zero-copy (GPU frame)"
                        } else {
                            "DMA-BUF with CPU mmap+copy (non-NV12 format)"
                        };
                        info!(
                            width = state.width,
                            height = state.height,
                            "PipeWire capture: {path}"
                        );
                        state.logged_buffer_type = true;
                    }
                    let _ = state.frame_tx.send(frame);
                } else {
                    debug!("DMA-BUF frame extraction failed");
                }
            } else if let Some(slice) = data.data() {
                // CPU-mapped buffer path (MemPtr / MemFd with MAP_BUFFERS).
                if !state.logged_buffer_type {
                    info!(
                        width = state.width,
                        height = state.height,
                        "PipeWire capture: CPU-mapped buffer (shared memory)"
                    );
                    state.logged_buffer_type = true;
                }
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

/// Timeout for portal D-Bus operations (per step, not total).
///
/// Each portal step (create_session, select_sources, start, open_pipe_wire_remote)
/// gets its own timeout. The user-facing dialog (select_sources → start) may take
/// longer, so those get a generous 120s. Infrastructure calls get 10s.
const PORTAL_INFRA_TIMEOUT: Duration = Duration::from_secs(10);
const PORTAL_DIALOG_TIMEOUT: Duration = Duration::from_secs(120);

/// Handle that keeps a portal session alive in a background thread.
///
/// The ScreenCast portal session controls the compositor's screen sharing.
/// Dropping the session tells the compositor to stop, which destroys the
/// PipeWire source node. The session must therefore live as long as the
/// PipeWire stream that consumes it.
///
/// On drop, signals the background thread to close the session via D-Bus
/// and shut down its tokio runtime cleanly.
struct PortalSessionGuard {
    close_tx: Option<mpsc::Sender<()>>,
}

impl Drop for PortalSessionGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.close_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Runs the ashpd ScreenCast portal negotiation on a dedicated thread.
///
/// Portal D-Bus calls happen on a tokio runtime that lives in the background
/// thread, keeping the portal session alive until the returned
/// [`PortalSessionGuard`] is dropped.
fn portal_screen_capture(show_cursor: bool) -> Result<(OwnedFd, u32, PortalSessionGuard)> {
    let (result_tx, result_rx) = mpsc::channel();
    let (close_tx, close_rx) = mpsc::channel();

    std::thread::Builder::new()
        .name("pw-portal-screen".into())
        .spawn(move || {
            portal_screen_capture_thread(show_cursor, result_tx, close_rx);
        })
        .context("failed to spawn portal thread")?;

    let (fd, node_id) = result_rx
        .recv_timeout(PORTAL_DIALOG_TIMEOUT + PORTAL_INFRA_TIMEOUT * 3)
        .context("portal thread did not respond")??;

    let guard = PortalSessionGuard {
        close_tx: Some(close_tx),
    };
    Ok((fd, node_id, guard))
}

fn portal_screen_capture_thread(
    show_cursor: bool,
    result_tx: mpsc::Sender<Result<(OwnedFd, u32)>>,
    close_rx: mpsc::Receiver<()>,
) {
    let rt = portal_runtime();

    rt.block_on(async {
        use ashpd::desktop::PersistMode;
        use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
        use tokio::time::timeout;

        let result = async {
            debug!("creating ScreenCast proxy");
            let proxy = timeout(PORTAL_INFRA_TIMEOUT, Screencast::new())
                .await
                .context("timeout creating ScreenCast proxy")?
                .context("failed to create ScreenCast proxy")?;

            debug!("creating ScreenCast session");
            let session = timeout(PORTAL_INFRA_TIMEOUT, proxy.create_session())
                .await
                .context("timeout creating session")?
                .context("failed to create ScreenCast session")?;

            let cursor_mode = if show_cursor {
                CursorMode::Embedded
            } else {
                CursorMode::Hidden
            };

            debug!("selecting sources (waiting for user to pick a screen)");
            timeout(
                PORTAL_DIALOG_TIMEOUT,
                proxy.select_sources(
                    &session,
                    cursor_mode,
                    SourceType::Monitor.into(),
                    false,
                    None,
                    PersistMode::DoNot,
                ),
            )
            .await
            .context("timeout waiting for select_sources")?
            .context("select_sources failed")?
            .response()
            .context("select_sources response failed (permission denied?)")?;

            debug!("starting ScreenCast (waiting for user confirmation)");
            let streams = timeout(PORTAL_DIALOG_TIMEOUT, proxy.start(&session, None))
                .await
                .context("timeout waiting for start")?
                .context("start failed")?
                .response()
                .context("start response failed (user cancelled?)")?;

            let stream = streams
                .streams()
                .first()
                .context("no streams returned from ScreenCast portal")?;

            let node_id = stream.pipe_wire_node_id();

            debug!(node_id, "opening PipeWire remote");
            let fd = timeout(PORTAL_INFRA_TIMEOUT, proxy.open_pipe_wire_remote(&session))
                .await
                .context("timeout opening PipeWire remote")?
                .context("failed to open PipeWire remote")?;

            info!(node_id, "ScreenCast portal negotiated");
            Ok((fd, node_id, session))
        }
        .await;

        let session = match result {
            Ok((fd, node_id, session)) => {
                let _ = result_tx.send(Ok((fd, node_id)));
                session
            }
            Err(e) => {
                let _ = result_tx.send(Err(e));
                return;
            }
        };

        // Keep the session alive until the capturer signals us to close.
        // The portal session controls the compositor's screen sharing —
        // dropping it stops the PipeWire source node.
        // Use recv_timeout as a safety net in case the guard is leaked.
        let _ = tokio::task::spawn_blocking(move || {
            let _ = close_rx.recv_timeout(Duration::from_secs(600));
        })
        .await;

        debug!("closing ScreenCast portal session");
        match tokio::time::timeout(Duration::from_secs(2), session.close()).await {
            Ok(Ok(())) => debug!("ScreenCast portal session closed"),
            Ok(Err(e)) => debug!("failed to close ScreenCast session: {e}"),
            Err(_) => debug!("ScreenCast session close timed out, dropping"),
        }
    });
}

/// Runs the ashpd Camera portal negotiation on a dedicated thread.
///
/// Same timeout and thread-isolation strategy as [`portal_screen_capture`].
fn portal_camera_capture() -> Result<OwnedFd> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("pw-portal-camera".into())
        .spawn(move || {
            let result = portal_camera_capture_inner();
            let _ = tx.send(result);
        })
        .context("failed to spawn portal thread")?;

    rx.recv_timeout(PORTAL_DIALOG_TIMEOUT + PORTAL_INFRA_TIMEOUT * 3)
        .context("portal thread did not respond")?
}

fn portal_camera_capture_inner() -> Result<OwnedFd> {
    let rt = portal_runtime();

    rt.block_on(async {
        use ashpd::desktop::camera::Camera;
        use tokio::time::timeout;

        debug!("creating Camera proxy");
        let proxy = timeout(PORTAL_INFRA_TIMEOUT, Camera::new())
            .await
            .context("timeout creating Camera proxy")?
            .context("failed to create Camera proxy")?;

        debug!("checking camera presence");
        let present = timeout(PORTAL_INFRA_TIMEOUT, proxy.is_present())
            .await
            .context("timeout checking camera presence")?
            .context("camera presence check failed")?;
        if !present {
            anyhow::bail!("no camera available via PipeWire portal");
        }

        debug!("requesting camera access");
        timeout(PORTAL_DIALOG_TIMEOUT, proxy.request_access())
            .await
            .context("timeout requesting camera access")?
            .context("camera access request failed")?
            .response()
            .context("camera access denied")?;

        debug!("opening camera PipeWire remote");
        let fd = timeout(PORTAL_INFRA_TIMEOUT, proxy.open_pipe_wire_remote())
            .await
            .context("timeout opening camera PipeWire remote")?
            .context("failed to open camera PipeWire remote")?;

        info!("Camera portal negotiated");

        drop(proxy);

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
    #[debug(skip)]
    _portal_guard: PortalSessionGuard,
}

impl PipeWireScreenCapturer {
    /// Creates a new PipeWire screen capturer.
    ///
    /// Triggers the xdg-desktop-portal ScreenCast dialog where the user
    /// selects which monitor or window to share. The portal D-Bus negotiation
    /// runs on a dedicated thread so this is safe to call from async contexts
    /// (via `spawn_blocking`).
    pub fn new(config: &ScreenConfig) -> Result<Self> {
        let show_cursor = config.show_cursor;
        let preferred_fps = config.target_fps.unwrap_or(30.0);

        // Portal negotiation runs on its own thread (inside portal_screen_capture).
        let (fd, node_id, portal_guard) = portal_screen_capture(show_cursor)?;

        let (frame_tx, frame_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        let should_stop = Arc::new(AtomicBool::new(false));
        let stop_flag = should_stop.clone();

        std::thread::Builder::new()
            .name("pw-screen".into())
            .spawn(move || {
                let result = run_pipewire_stream(
                    fd,
                    Some(node_id),
                    Rectangle {
                        width: 8192,
                        height: 4320,
                    },
                    Fraction {
                        num: preferred_fps as u32,
                        denom: 1,
                    },
                    frame_tx,
                    init_tx.clone(),
                    stop_flag,
                );
                if let Err(e) = result {
                    let _ = init_tx.send(Err(e));
                }
            })
            .context("failed to spawn PipeWire screen capture thread")?;

        let (width, height) = init_rx
            .recv_timeout(Duration::from_secs(30))
            .context("PipeWire screen capture format negotiation timeout")?
            .context("PipeWire screen capture init failed")?;

        info!(width, height, "PipeWire screen capture started");

        Ok(Self {
            width,
            height,
            rx: frame_rx,
            should_stop,
            _portal_guard: portal_guard,
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
    /// The [`CameraSelector`](crate::types::CameraSelector) in `config`
    /// controls the resolution/framerate preference hint sent to PipeWire
    /// during format negotiation. `HighestResolution` (the default) requests
    /// the maximum size the camera supports.
    ///
    /// Triggers the Camera portal permission dialog if not already granted.
    /// The portal D-Bus negotiation runs on a dedicated thread so this is
    /// safe to call from async contexts (via `spawn_blocking`).
    pub fn new(config: &CameraConfig) -> Result<Self> {
        use crate::CameraSelector;

        // Translate CameraSelector into PipeWire format negotiation hints.
        let (preferred_size, preferred_fps) = match config.selector {
            CameraSelector::HighestResolution => (
                Rectangle {
                    width: 8192,
                    height: 4320,
                },
                Fraction { num: 30, denom: 1 },
            ),
            CameraSelector::HighestFramerate => (
                Rectangle {
                    width: 1920,
                    height: 1080,
                },
                Fraction { num: 144, denom: 1 },
            ),
            CameraSelector::TargetResolution(w, h) => (
                Rectangle {
                    width: w,
                    height: h,
                },
                Fraction { num: 30, denom: 1 },
            ),
        };

        // Portal negotiation runs on its own thread (inside portal_camera_capture).
        let fd = portal_camera_capture()?;

        let (frame_tx, frame_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        let should_stop = Arc::new(AtomicBool::new(false));
        let stop_flag = should_stop.clone();

        std::thread::Builder::new()
            .name("pw-camera".into())
            .spawn(move || {
                // node_id = None → PW_ID_ANY, AUTOCONNECT routes to camera.
                let result = run_pipewire_stream(
                    fd,
                    None,
                    preferred_size,
                    preferred_fps,
                    frame_tx,
                    init_tx.clone(),
                    stop_flag,
                );
                if let Err(e) = result {
                    let _ = init_tx.send(Err(e));
                }
            })
            .context("failed to spawn PipeWire camera capture thread")?;

        let (width, height) = init_rx
            .recv_timeout(Duration::from_secs(30))
            .context("PipeWire camera capture format negotiation timeout")?
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
