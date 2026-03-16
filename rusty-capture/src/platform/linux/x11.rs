//! X11 screen capture via MIT-SHM extension.
//!
//! CPU-only fallback for non-Wayland Linux systems. Uses shared memory to
//! avoid X protocol socket overhead, but the X server still copies the
//! framebuffer into the SHM region on each capture.
//!
//! No zero-copy path exists for X11 screen capture.
//!
//! # Threading Model
//!
//! No internal thread. The moq-media encode pipeline already runs capture on
//! its own thread via `spawn_thread`. `pop_frame()` calls `shm::get_image`
//! directly, which blocks until the X server completes the copy.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::shm;
use x11rb::rust_connection::RustConnection;

use rusty_codecs::format::{PixelFormat, VideoFormat, VideoFrame};
use rusty_codecs::traits::VideoSource;

use crate::types::{MonitorInfo, ScreenConfig};

/// Lists available X11 monitors via RANDR.
///
/// Uses the RANDR extension to enumerate outputs with their positions,
/// dimensions, refresh rates, and primary status. Falls back to simple
/// X11 screen enumeration if RANDR is not available.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    let (conn, screen_num) = x11rb::connect(None).context("failed to connect to X11")?;
    let setup = conn.setup();

    // Try RANDR first for accurate multihead info.
    if let Ok(monitors) = monitors_randr(&conn, setup, screen_num)
        && !monitors.is_empty()
    {
        return Ok(monitors);
    }

    // Fallback: enumerate X11 screens (no position info, no multihead).
    let mut monitors = Vec::new();
    for (i, screen) in setup.roots.iter().enumerate() {
        monitors.push(MonitorInfo {
            backend: crate::CaptureBackend::X11,
            id: format!("x11-screen-{i}"),
            name: format!("Screen {i}"),
            position: [0, 0],
            dimensions: [
                screen.width_in_pixels as u32,
                screen.height_in_pixels as u32,
            ],
            scale_factor: 1.0,
            refresh_rate_hz: None,
            is_primary: i == screen_num,
        });
    }
    Ok(monitors)
}

/// Enumerates monitors using RANDR extension for multihead support.
fn monitors_randr(
    conn: &impl Connection,
    setup: &x11rb::protocol::xproto::Setup,
    screen_num: usize,
) -> Result<Vec<MonitorInfo>> {
    use x11rb::protocol::randr;

    let root = setup.roots.get(screen_num).context("no root screen")?.root;

    let resources = randr::get_screen_resources(conn, root)?
        .reply()
        .context("RANDR get_screen_resources failed")?;

    let primary = randr::get_output_primary(conn, root)?
        .reply()
        .ok()
        .map(|r| r.output);

    let mut monitors = Vec::new();
    for output in &resources.outputs {
        let Ok(info) = randr::get_output_info(conn, *output, 0)?.reply() else {
            continue;
        };
        // Skip disconnected or off outputs.
        if info.crtc == 0 || info.connection != randr::Connection::CONNECTED {
            continue;
        }
        let Ok(crtc) = randr::get_crtc_info(conn, info.crtc, 0)?.reply() else {
            continue;
        };

        let name = String::from_utf8_lossy(&info.name).to_string();
        let is_primary = primary.is_some_and(|p| p == *output);

        // Compute refresh rate from the mode.
        let refresh_rate_hz = resources
            .modes
            .iter()
            .find(|m| m.id == crtc.mode)
            .map(|mode| {
                let interlaced =
                    mode.mode_flags & randr::ModeFlag::INTERLACE != randr::ModeFlag::from(0u8);
                let vtotal = mode.vtotal as f32 * if interlaced { 0.5 } else { 1.0 };
                if vtotal > 0.0 && mode.htotal > 0 {
                    mode.dot_clock as f32 / (vtotal * mode.htotal as f32)
                } else {
                    0.0
                }
            })
            .filter(|r| *r > 0.0);

        monitors.push(MonitorInfo {
            backend: crate::CaptureBackend::X11,
            id: format!("x11-screen-{screen_num}"),
            name,
            position: [crtc.x as i32, crtc.y as i32],
            dimensions: [crtc.width as u32, crtc.height as u32],
            scale_factor: 1.0,
            refresh_rate_hz,
            is_primary,
        });
    }
    Ok(monitors)
}

/// X11 screen capturer using MIT-SHM for efficient pixel transfer.
///
/// No internal thread — `pop_frame()` calls `shm::get_image` directly on the
/// caller's thread. The moq-media encode pipeline drives the capture loop.
#[derive(derive_more::Debug)]
pub struct X11ScreenCapturer {
    width: u32,
    height: u32,
    target_interval: Duration,
    /// SHM and X11 connection state. `None` before `start()` / after `stop()`.
    #[debug(skip)]
    state: Option<ShmState>,
    screen_idx: usize,
}

struct ShmState {
    conn: RustConnection,
    root: u32,
    seg: u32,
    shm_addr: *mut libc::c_void,
    buf_size: usize,
    capture_start: Instant,
    last_capture: Instant,
}

// SAFETY: shm_addr is a process-local mmap pointer, safe to send between
// threads. The X11 connection is not thread-safe but we only use it from one
// thread at a time (the caller's encode thread).
unsafe impl Send for ShmState {}

impl X11ScreenCapturer {
    /// Creates a new X11 screen capturer for the given monitor.
    pub fn new(monitor: &MonitorInfo, config: &ScreenConfig) -> Result<Self> {
        let screen_idx: usize = monitor
            .id
            .strip_prefix("x11-screen-")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let (conn, _) = x11rb::connect(None).context("failed to connect to X11")?;
        let setup = conn.setup();
        let screen = setup
            .roots
            .get(screen_idx)
            .context("X11 screen not found")?;
        let width = screen.width_in_pixels as u32;
        let height = screen.height_in_pixels as u32;

        // Check SHM extension.
        shm::query_version(&conn)
            .context("MIT-SHM not available")?
            .reply()
            .context("MIT-SHM query failed")?;

        let target_interval = config
            .target_fps
            .map(|fps| Duration::from_secs_f32(1.0 / fps))
            .unwrap_or(Duration::from_millis(33));

        info!(
            width,
            height,
            screen = screen_idx,
            "X11 screen capture ready"
        );

        Ok(Self {
            width,
            height,
            target_interval,
            state: None,
            screen_idx,
        })
    }

    /// Creates a screen capturer for the primary display.
    pub fn open_default(config: &ScreenConfig) -> Result<Self> {
        let monitors = monitors()?;
        let primary = monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
            .context("no X11 screens")?;
        Self::new(primary, config)
    }

    fn start_capture(&mut self) -> Result<()> {
        if self.state.is_some() {
            return Ok(());
        }

        let (conn, _) = x11rb::connect(None).context("failed to connect to X11")?;
        let setup = conn.setup();
        let screen = setup
            .roots
            .get(self.screen_idx)
            .context("X11 screen not found")?;
        let root = screen.root;

        let buf_size = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|n| n.checked_mul(4))
            .context("screen dimensions too large for SHM buffer")?;

        // Create SHM segment.
        let shm_id = unsafe { libc::shmget(libc::IPC_PRIVATE, buf_size, libc::IPC_CREAT | 0o600) };
        if shm_id < 0 {
            anyhow::bail!("shmget failed");
        }
        let shm_addr = unsafe { libc::shmat(shm_id, std::ptr::null(), 0) };
        if shm_addr == (-1_isize) as *mut libc::c_void {
            anyhow::bail!("shmat failed");
        }

        // Attach to X server.
        let seg = conn.generate_id()?;
        shm::attach(&conn, seg, shm_id as u32, false)?;
        conn.flush()?;

        // Mark for removal — segment will be freed when all processes detach.
        unsafe {
            libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut());
        }

        let now = Instant::now();
        self.state = Some(ShmState {
            conn,
            root,
            seg,
            shm_addr,
            buf_size,
            capture_start: now,
            last_capture: now - self.target_interval, // allow immediate first capture
        });

        debug!(screen = self.screen_idx, "X11 SHM capture started");
        Ok(())
    }

    fn stop_capture(&mut self) {
        if let Some(state) = self.state.take() {
            shm::detach(&state.conn, state.seg).ok();
            state.conn.flush().ok();
            unsafe {
                libc::shmdt(state.shm_addr);
            }
            debug!(screen = self.screen_idx, "X11 SHM capture stopped");
        }
    }
}

impl VideoSource for X11ScreenCapturer {
    fn name(&self) -> &str {
        "x11-screen"
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        self.start_capture()
    }

    fn stop(&mut self) -> Result<()> {
        self.stop_capture();
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let Some(state) = &mut self.state else {
            return Ok(None);
        };

        // Rate limit to target fps.
        let elapsed = state.last_capture.elapsed();
        if elapsed < self.target_interval {
            std::thread::sleep(self.target_interval - elapsed);
        }
        state.last_capture = Instant::now();

        // Capture via SHM — blocks until the X server completes the copy.
        let cookie = shm::get_image(
            &state.conn,
            state.root,
            0,
            0,
            self.width as u16,
            self.height as u16,
            !0, // all planes
            2,  // ZPixmap
            state.seg,
            0,
        )?;

        match cookie.reply() {
            Ok(_reply) => {
                // SHM buffer now contains BGRX pixel data.
                let shm_slice = unsafe {
                    std::slice::from_raw_parts(state.shm_addr as *const u8, state.buf_size)
                };

                // Convert BGRX → RGBA.
                let mut rgba = vec![0u8; state.buf_size];
                for (src, dst) in shm_slice.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
                    dst[0] = src[2]; // R ← B
                    dst[1] = src[1]; // G ← G
                    dst[2] = src[0]; // B ← R
                    dst[3] = 255; // A (X ignored)
                }

                let frame = VideoFrame::new_rgba(
                    rgba.into(),
                    self.width,
                    self.height,
                    state.capture_start.elapsed(),
                );
                Ok(Some(frame))
            }
            Err(e) => {
                warn!("X11 SHM get_image failed: {e}");
                Ok(None)
            }
        }
    }
}

impl Drop for X11ScreenCapturer {
    fn drop(&mut self) {
        self.stop_capture();
    }
}
