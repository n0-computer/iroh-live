//! X11 screen capture via MIT-SHM extension.
//!
//! CPU-only fallback for non-Wayland Linux systems. Uses shared memory to
//! avoid X protocol socket overhead, but the X server still copies the
//! framebuffer into the SHM region on each capture.
//!
//! No zero-copy path exists for X11 screen capture.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::shm;

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
#[derive(derive_more::Debug)]
pub struct X11ScreenCapturer {
    width: u32,
    height: u32,
    #[debug(skip)]
    rx: mpsc::Receiver<VideoFrame>,
    #[debug(skip)]
    stop_tx: Option<mpsc::Sender<()>>,
}

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
        let root = screen.root;
        let depth = screen.root_depth;

        // Check SHM extension.
        shm::query_version(&conn)
            .context("MIT-SHM not available")?
            .reply()
            .context("MIT-SHM query failed")?;

        info!(
            width,
            height,
            screen = screen_idx,
            "X11 screen capture started"
        );

        let (frame_tx, frame_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();

        let target_interval = config
            .target_fps
            .map(|fps| Duration::from_secs_f32(1.0 / fps))
            .unwrap_or(Duration::from_millis(33));

        std::thread::Builder::new()
            .name("x11-capture".into())
            .spawn(move || {
                if let Err(e) = capture_loop_shm(
                    root,
                    width,
                    height,
                    depth,
                    target_interval,
                    frame_tx,
                    stop_rx,
                ) {
                    warn!("X11 capture loop exited: {e}");
                }
            })
            .context("failed to spawn X11 capture thread")?;

        Ok(Self {
            width,
            height,
            rx: frame_rx,
            stop_tx: Some(stop_tx),
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
}

fn capture_loop_shm(
    root: u32,
    width: u32,
    height: u32,
    _depth: u8,
    target_interval: Duration,
    tx: mpsc::Sender<VideoFrame>,
    stop_rx: mpsc::Receiver<()>,
) -> Result<()> {
    let (conn, _) = x11rb::connect(None)?;
    let buf_size = (width as usize)
        .checked_mul(height as usize)
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

    let capture_start = Instant::now();
    let mut last_capture = Instant::now();

    loop {
        if stop_rx.try_recv().is_ok() {
            debug!("X11 capture stopping");
            break;
        }

        // Rate limit.
        let elapsed = last_capture.elapsed();
        if elapsed < target_interval {
            std::thread::sleep(target_interval - elapsed);
        }
        last_capture = Instant::now();

        // Capture via SHM.
        let cookie = shm::get_image(
            &conn,
            root,
            0,
            0,
            width as u16,
            height as u16,
            !0, // all planes
            2,  // ZPixmap
            seg,
            0,
        )?;

        match cookie.reply() {
            Ok(_reply) => {
                // SHM buffer now contains BGRX pixel data.
                let shm_slice =
                    unsafe { std::slice::from_raw_parts(shm_addr as *const u8, buf_size) };

                // Convert BGRX → RGBA.
                let mut rgba = vec![0u8; buf_size];
                for (src, dst) in shm_slice.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
                    dst[0] = src[2]; // R ← B
                    dst[1] = src[1]; // G ← G
                    dst[2] = src[0]; // B ← R
                    dst[3] = 255; // A (X ignored)
                }

                let frame =
                    VideoFrame::new_rgba(rgba.into(), width, height, capture_start.elapsed());
                if tx.send(frame).is_err() {
                    debug!("X11 frame receiver dropped");
                    break;
                }
            }
            Err(e) => {
                warn!("X11 SHM get_image failed: {e}");
            }
        }
    }

    // Cleanup.
    shm::detach(&conn, seg).ok();
    conn.flush().ok();
    unsafe {
        libc::shmdt(shm_addr);
    }

    Ok(())
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

impl Drop for X11ScreenCapturer {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}
