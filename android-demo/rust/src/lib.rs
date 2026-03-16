//! JNI bridge for the iroh-live Android demo app.
//!
//! Exposes a minimal set of functions to Kotlin: connect to a broadcast,
//! poll decoded video frames, publish camera frames, and disconnect.
//! A global tokio runtime drives all async work.

use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use anyhow::{Context, Result};
use jni::{
    JNIEnv,
    objects::{JByteArray, JByteBuffer, JClass, JString},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jlong},
};
use tokio::runtime::Runtime;
use tracing::info;

use iroh_live::Live;
use moq_media::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    publish::LocalBroadcast,
    subscribe::{RemoteBroadcast, VideoTrack},
    traits::VideoSource,
};

// ── Global runtime ──────────────────────────────────────────────────

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("iroh-live-android")
            .build()
            .expect("failed to create tokio runtime")
    })
}

// ── Session handle ──────────────────────────────────────────────────

/// Opaque handle stored as a `jlong` on the Kotlin side.
struct SessionHandle {
    live: Live,
    _session: iroh_live::moq::MoqSession,
    _broadcast: RemoteBroadcast,
    video: Option<VideoTrack>,
    /// Publish-side broadcast, created lazily on `startPublish`.
    _publish_broadcast: Option<LocalBroadcast>,
    /// Camera source for pushing frames into the publish pipeline.
    camera_source: Option<Arc<Mutex<CameraFrameSource>>>,
}

impl SessionHandle {
    fn into_jlong(self) -> jlong {
        let boxed = Box::new(self);
        Box::into_raw(boxed) as jlong
    }

    /// Recovers a mutable reference from a `jlong`.
    ///
    /// # Safety
    ///
    /// The pointer must have been created by [`into_jlong`](Self::into_jlong)
    /// and must not have been freed yet.
    unsafe fn from_jlong(handle: jlong) -> &'static mut Self {
        // SAFETY: the caller guarantees the pointer is valid.
        unsafe { &mut *(handle as *mut Self) }
    }
}

// ── Camera frame source ─────────────────────────────────────────────

/// A [`VideoSource`] that receives camera frames pushed from the JNI side.
struct CameraFrameSource {
    pending_frame: Option<VideoFrame>,
    format: VideoFormat,
    started: bool,
}

#[allow(dead_code, reason = "used when camera publish is wired up in Phase 2")]
impl CameraFrameSource {
    /// Creates a camera source that accepts RGBA frames of the given dimensions.
    fn new(width: u32, height: u32) -> Self {
        Self {
            pending_frame: None,
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [width, height],
            },
            started: false,
        }
    }

    fn push_frame(&mut self, frame: VideoFrame) {
        self.pending_frame = Some(frame);
    }
}

impl VideoSource for CameraFrameSource {
    fn name(&self) -> &str {
        "android-camera"
    }

    fn format(&self) -> VideoFormat {
        self.format.clone()
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        Ok(self.pending_frame.take())
    }

    fn start(&mut self) -> Result<()> {
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.started = false;
        Ok(())
    }
}

// ── JNI exports ─────────────────────────────────────────────────────

/// Connects to a remote broadcast using a ticket string.
///
/// Returns an opaque session handle as a `jlong`, or 0 on failure.
///
/// # Safety
///
/// Must be called from the JNI with valid arguments.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_connect(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    ticket: JString<'_>,
) -> jlong {
    let _ = tracing_subscriber::fmt::try_init();

    let ticket_str: String = match env.get_string(&ticket) {
        Ok(s) => s.into(),
        Err(e) => {
            tracing::error!("failed to read ticket string: {e}");
            return 0;
        }
    };

    match runtime().block_on(connect_impl(ticket_str)) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!("connect failed: {e:#}");
            0
        }
    }
}

async fn connect_impl(ticket_str: String) -> Result<jlong> {
    let ticket: iroh_live::ticket::LiveTicket = ticket_str
        .parse()
        .context("failed to parse ticket string")?;

    let live = Live::from_env().await?;

    info!(broadcast = %ticket.broadcast_name, "connecting to broadcast");

    let (session, broadcast) = live
        .subscribe(ticket.endpoint.clone(), &ticket.broadcast_name)
        .await?;

    // Start a video track with default software decoders.
    let video = broadcast
        .video()
        .inspect_err(|e| tracing::warn!("no video track: {e}"))
        .ok();

    let handle = SessionHandle {
        live,
        _session: session,
        _broadcast: broadcast,
        video,
        _publish_broadcast: None,
        camera_source: None,
    };

    Ok(handle.into_jlong())
}

/// Polls for the next decoded video frame.
///
/// If a new RGBA frame is available, copies it into the provided direct
/// `ByteBuffer` and returns `true`. Returns `false` when no frame is
/// ready. The buffer must be large enough for `width * height * 4` bytes.
///
/// # Safety
///
/// The `handle` must be a valid pointer returned by `connect`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_nextFrame(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    buffer: JByteBuffer<'_>,
) -> jboolean {
    if handle == 0 {
        return JNI_FALSE;
    }

    // SAFETY: handle was created by connect and has not been freed.
    let session = unsafe { SessionHandle::from_jlong(handle) };

    let Some(video) = session.video.as_mut() else {
        return JNI_FALSE;
    };

    let Some(frame) = video.current_frame() else {
        return JNI_FALSE;
    };

    let rgba = frame.rgba_image();
    let rgba_bytes = rgba.as_raw();

    let Ok(buf_ptr) = env.get_direct_buffer_address(&buffer) else {
        tracing::error!("failed to get direct ByteBuffer address");
        return JNI_FALSE;
    };

    let Ok(buf_len) = env.get_direct_buffer_capacity(&buffer) else {
        tracing::error!("failed to get direct ByteBuffer capacity");
        return JNI_FALSE;
    };

    let copy_len = rgba_bytes.len().min(buf_len);

    // SAFETY: buf_ptr points to a valid direct ByteBuffer of at least buf_len bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(rgba_bytes.as_ptr(), buf_ptr, copy_len);
    }

    JNI_TRUE
}

/// Starts publishing a broadcast with the given name.
///
/// Creates a [`LocalBroadcast`] and announces it. Camera frames can then
/// be pushed via `pushCameraFrame`.
///
/// # Safety
///
/// The `handle` must be a valid pointer returned by `connect`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_startPublish(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    name: JString<'_>,
) {
    if handle == 0 {
        return;
    }

    let broadcast_name: String = match env.get_string(&name) {
        Ok(s) => s.into(),
        Err(e) => {
            tracing::error!("failed to read broadcast name: {e}");
            return;
        }
    };

    // SAFETY: handle was created by connect and has not been freed.
    let session = unsafe { SessionHandle::from_jlong(handle) };

    if let Err(e) = runtime().block_on(start_publish_impl(session, broadcast_name)) {
        tracing::error!("startPublish failed: {e:#}");
    }
}

async fn start_publish_impl(session: &mut SessionHandle, name: String) -> Result<()> {
    let broadcast = LocalBroadcast::new();

    session
        .live
        .publish(&name, &broadcast)
        .await
        .context("failed to announce broadcast")?;

    info!(name = %name, "publishing broadcast");

    session._publish_broadcast = Some(broadcast);
    Ok(())
}

/// Pushes a camera frame (RGBA byte array) into the publish pipeline.
///
/// The `data` array must contain `width * height * 4` bytes of RGBA pixels.
///
/// # Safety
///
/// The `handle` must be a valid pointer returned by `connect`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_pushCameraFrame(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    data: JByteArray<'_>,
    width: jint,
    height: jint,
) {
    if handle == 0 {
        return;
    }

    // SAFETY: handle was created by connect and has not been freed.
    let session = unsafe { SessionHandle::from_jlong(handle) };

    let Ok(bytes) = env.convert_byte_array(data) else {
        tracing::error!("failed to read camera frame byte array");
        return;
    };

    let frame = VideoFrame::new_rgba(
        bytes::Bytes::from(bytes),
        width as u32,
        height as u32,
        Duration::ZERO,
    );

    if let Some(source) = session.camera_source.as_ref()
        && let Ok(mut src) = source.lock()
    {
        src.push_frame(frame);
    }
}

/// Disconnects and frees the session handle.
///
/// # Safety
///
/// The `handle` must be a valid pointer returned by `connect` and must
/// not be used after this call.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_disconnect(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }

    // SAFETY: handle was created by connect and is being freed here.
    let session = unsafe { Box::from_raw(handle as *mut SessionHandle) };

    runtime().block_on(async {
        session.live.shutdown().await;
    });

    info!("disconnected");
}
