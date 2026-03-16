//! JNI bridge for the iroh-live Android demo app.
//!
//! Exposes a minimal set of functions to Kotlin: connect to a broadcast,
//! poll decoded video frames, publish camera frames, and disconnect.
//! A global tokio runtime drives all async work.

use std::{
    mem::ManuallyDrop,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use anyhow::{Context, Result};
use jni::{
    JNIEnv, JavaVM,
    objects::{JByteArray, JClass, JString},
    sys::{jint, jlong},
};
use tokio::runtime::Runtime;
use tracing::info;

use n0_watcher::Watcher;
use iroh_live::{Call, CallTicket, Live};
use moq_media::{
    codec::{AudioCodec, VideoCodec},
    format::{AudioPreset, PixelFormat, PlaybackConfig, VideoFormat, VideoFrame, VideoPreset},
    publish::LocalBroadcast,
    subscribe::{AudioTrack, RemoteBroadcast, VideoTrack},
    traits::VideoSource,
    AudioBackend,
};
use rusty_codecs::format::Nv12Planes;
use rusty_codecs::codec::DefaultDecoders;

// ── JNI lifecycle ───────────────────────────────────────────────────

/// Initializes `ndk-context` so that cpal (Oboe backend) can access
/// the JVM and Activity pointers it needs for audio stream creation.
///
/// Called automatically by the JVM when `System.loadLibrary` loads this .so.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut std::ffi::c_void) -> jint {
    // SAFETY: The JVM guarantees `vm` is valid during JNI_OnLoad.
    // We pass a null activity context — cpal only needs the VM pointer
    // for Oboe initialization, not an Activity reference.
    unsafe {
        ndk_context::initialize_android_context(
            vm.get_java_vm_pointer().cast(),
            std::ptr::null_mut(),
        );
    }
    jni::sys::JNI_VERSION_1_6
}

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
///
/// Wrapped in `Arc<Mutex<>>` so that concurrent JNI calls from different
/// threads (render loop vs camera callback) are safe.
struct SessionHandle {
    live: Live,
    /// Subscribe-only session (from `connect`).
    _session: Option<iroh_live::moq::MoqSession>,
    /// Call handle (from `dial`). Keeps the session + local broadcast alive.
    _call: Option<Call>,
    /// Remote broadcast we're subscribed to.
    _broadcast: Option<RemoteBroadcast>,
    video: Option<VideoTrack>,
    /// Audio track — kept alive to sustain playback via the audio backend.
    _audio: Option<AudioTrack>,
    /// Audio backend (cpal/Oboe driver thread) — must outlive audio tracks.
    _audio_backend: Option<AudioBackend>,
    /// Actual decoded frame dimensions, updated each time `nextFrame`
    /// produces a frame. More reliable than catalog `coded_width`/`coded_height`
    /// which may include codec padding or differ from the scaler output.
    actual_frame_dims: Option<(u32, u32)>,
    /// Publish-side broadcast, created lazily on `startPublish`.
    _publish_broadcast: Option<LocalBroadcast>,
    /// Camera source for pushing frames into the publish pipeline.
    camera_source: Option<Arc<Mutex<CameraFrameSource>>>,
    /// Camera frames pushed from JNI.
    cam_frames_pushed: u64,
    /// Decoded video frames rendered.
    dec_frames_rendered: u64,
    /// Timestamp of session creation.
    created_at: std::time::Instant,
}

/// Thread-safe wrapper around `SessionHandle` stored as a raw pointer.
type SharedHandle = Arc<Mutex<SessionHandle>>;

fn handle_to_jlong(handle: SharedHandle) -> jlong {
    let raw = Arc::into_raw(handle);
    raw as jlong
}

/// Recovers a cloned `Arc` from a `jlong` without consuming the original.
///
/// # Safety
///
/// The pointer must have been created by [`handle_to_jlong`] and must not
/// have been freed yet.
unsafe fn handle_from_jlong(handle: jlong) -> SharedHandle {
    // Wrap in ManuallyDrop so the original Arc is never dropped.
    let arc = ManuallyDrop::new(unsafe { Arc::from_raw(handle as *const Mutex<SessionHandle>) });
    Arc::clone(&arc)
}

/// Takes ownership of the handle, dropping the `Arc` reference.
///
/// # Safety
///
/// The pointer must have been created by [`handle_to_jlong`] and must not
/// be used after this call.
unsafe fn handle_take(handle: jlong) -> SharedHandle {
    unsafe { Arc::from_raw(handle as *const Mutex<SessionHandle>) }
}

// ── Camera frame source ─────────────────────────────────────────────

/// A [`VideoSource`] that receives camera frames pushed from the JNI side.
struct CameraFrameSource {
    pending_frame: Option<VideoFrame>,
    format: VideoFormat,
    started: bool,
}

impl CameraFrameSource {
    /// Creates a camera source for the given dimensions.
    ///
    /// `pixel_format` only matters for `FrameData::Packed` frames — encoders
    /// use it to pick the right RGB→YUV conversion. Since this source pushes
    /// `FrameData::Nv12` frames, the encoder takes the NV12 fast path and
    /// never consults `pixel_format`.
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

/// [`VideoSource`] wrapper around `Arc<Mutex<CameraFrameSource>>`.
///
/// The publish pipeline takes ownership of the source, but we need a
/// shared handle so the JNI `pushCameraFrame` callback can push frames.
/// This wrapper bridges the two by delegating all trait methods through
/// the mutex.
struct SharedCameraSource {
    inner: Arc<Mutex<CameraFrameSource>>,
}

impl VideoSource for SharedCameraSource {
    fn name(&self) -> &str {
        "android-camera"
    }

    fn format(&self) -> VideoFormat {
        self.inner.lock().expect("poisoned").format.clone()
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        Ok(self.inner.lock().expect("poisoned").pending_frame.take())
    }

    fn start(&mut self) -> Result<()> {
        self.inner.lock().expect("poisoned").started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.inner.lock().expect("poisoned").started = false;
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
    let _ = logcat::init();

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

    // Start a video track with dynamic decoder dispatch (hw first, sw fallback).
    let video = broadcast
        .video()
        .inspect_err(|e| tracing::warn!("no video track: {e}"))
        .ok();

    // Start audio playback. AudioBackend uses cpal (Oboe on Android) which
    // spawns a driver thread. The AudioTrack must be kept alive for playback
    // to continue — dropping it stops the decoder pipeline.
    let audio_backend = AudioBackend::default();
    let audio = match broadcast.audio(&audio_backend).await {
        Ok(track) => {
            info!("audio track subscribed");
            Some(track)
        }
        Err(e) => {
            tracing::warn!("no audio track: {e:#}");
            None
        }
    };

    let handle = SessionHandle {
        live,
        _session: Some(session),
        _call: None,
        _broadcast: Some(broadcast),
        video,
        _audio: audio,
        _audio_backend: Some(audio_backend),
        actual_frame_dims: None,
        _publish_broadcast: None,
        camera_source: None,
        cam_frames_pushed: 0,
        dec_frames_rendered: 0,
        created_at: std::time::Instant::now(),
    };

    Ok(handle_to_jlong(Arc::new(Mutex::new(handle))))
}

/// Dials a remote peer using a call ticket string.
///
/// Sets up camera publishing (720p H.264 HW encoding) and microphone
/// audio (Opus), then subscribes to the remote peer's media. Returns
/// an opaque session handle as a `jlong`, or 0 on failure.
///
/// The caller must push camera frames via `pushCameraFrame` for the
/// video to encode. The camera dimensions (width, height) are passed
/// here so the encoder can be configured before frames arrive.
///
/// # Safety
///
/// Must be called from the JNI with valid arguments.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_dial(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    ticket: JString<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let _ = logcat::init();

    let ticket_str: String = match env.get_string(&ticket) {
        Ok(s) => s.into(),
        Err(e) => {
            tracing::error!("failed to read ticket string: {e}");
            return 0;
        }
    };

    match runtime().block_on(dial_impl(ticket_str, camera_width as u32, camera_height as u32)) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!("dial failed: {e:#}");
            0
        }
    }
}

async fn dial_impl(ticket_str: String, cam_w: u32, cam_h: u32) -> Result<jlong> {
    info!(ticket = %ticket_str, cam_w, cam_h, "parsing call ticket");
    let ticket: CallTicket = ticket_str
        .parse()
        .context("failed to parse call ticket")?;
    info!(?ticket, "ticket parsed");

    info!("creating Live endpoint");
    let live = Live::from_env().await?;
    info!(id = %live.endpoint().id().fmt_short(), "endpoint ready");

    // Set up local broadcast with camera video and microphone audio.
    let broadcast = LocalBroadcast::new();

    let camera_source = Arc::new(Mutex::new(CameraFrameSource::new(cam_w, cam_h)));
    let shared_source = SharedCameraSource {
        inner: Arc::clone(&camera_source),
    };
    let codec = VideoCodec::best_available();
    info!(?codec, "setting camera video source");
    broadcast
        .video()
        .set(shared_source, codec, [VideoPreset::P720])
        .context("failed to set video source")?;

    // Microphone audio via cpal/Oboe.
    info!("creating audio backend");
    let audio_backend = AudioBackend::default();
    info!("audio backend created, requesting microphone input");
    let mic = audio_backend
        .default_input()
        .await
        .context("failed to open microphone")?;
    info!("microphone opened, setting audio source");
    broadcast
        .audio()
        .set(mic, AudioCodec::Opus, [AudioPreset::Hq])
        .context("failed to set audio source")?;

    info!(?ticket, "dialing call");
    let call = Call::dial(&live, ticket, broadcast).await?;
    info!("call connected");

    // Subscribe to remote media (video + audio).
    info!("subscribing to remote media");
    let playback_config = PlaybackConfig::default();
    let tracks = call
        .remote()
        .media::<DefaultDecoders>(&audio_backend, playback_config)
        .await
        .context("failed to subscribe to remote media")?;

    info!(
        has_video = tracks.video.is_some(),
        has_audio = tracks.audio.is_some(),
        "remote media subscribed"
    );

    let handle = SessionHandle {
        live,
        _session: None,
        _call: Some(call),
        _broadcast: Some(tracks.broadcast),
        video: tracks.video,
        _audio: tracks.audio,
        _audio_backend: Some(audio_backend),
        actual_frame_dims: None,
        _publish_broadcast: None,
        camera_source: Some(camera_source),
        cam_frames_pushed: 0,
        dec_frames_rendered: 0,
        created_at: std::time::Instant::now(),
    };

    info!("call connected, publishing camera and subscribed to remote");

    Ok(handle_to_jlong(Arc::new(Mutex::new(handle))))
}

/// Returns the video dimensions as `(width << 32) | height`, or 0 if unknown.
///
/// Prefers actual decoded frame dimensions (set by `nextFrame`) over the
/// catalog's `coded_width`/`coded_height`, which may include codec padding
/// or differ from what the decoder actually produces.
///
/// # Safety
///
/// The `handle` must be a valid pointer returned by `connect`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getVideoDimensions(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jlong {
    if handle == 0 {
        return 0;
    }
    // SAFETY: handle was created by connect and has not been freed.
    let session = unsafe { handle_from_jlong(handle) };
    let Ok(guard) = session.lock() else {
        return 0;
    };

    // Use actual decoded frame dimensions when available.
    if let Some((w, h)) = guard.actual_frame_dims {
        return ((w as i64) << 32) | (h as i64);
    }

    // Fall back to catalog dimensions before the first frame arrives.
    let Some(video) = guard.video.as_ref() else {
        return 0;
    };
    let Some(ref broadcast) = guard._broadcast else {
        return 0;
    };
    let catalog = broadcast.catalog();
    let rendition = video.rendition();
    if let Some(config) = catalog.video.renditions.get(rendition) {
        let w = config.coded_width.unwrap_or(0) as i64;
        let h = config.coded_height.unwrap_or(0) as i64;
        (w << 32) | h
    } else {
        0
    }
}

/// Returns a raw `AHardwareBuffer*` pointer for the latest decoded frame.
///
/// The caller must call [`releaseHardwareBuffer`] when done with the
/// buffer. Returns 0 if no GPU frame is available (e.g. still using the
/// software decoder fallback, or no frame decoded yet).
///
/// # Safety
///
/// The `handle` must be a valid pointer returned by `connect`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_nextHardwareBuffer(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jlong {
    if handle == 0 {
        return 0;
    }

    // SAFETY: handle was created by connect and has not been freed.
    let session = unsafe { handle_from_jlong(handle) };
    let Ok(mut guard) = session.lock() else {
        return 0;
    };

    let Some(video) = guard.video.as_mut() else {
        return 0;
    };

    let Some(frame) = video.current_frame() else {
        return 0;
    };

    // Track actual decoded dimensions and frame count.
    guard.dec_frames_rendered += 1;
    let w = frame.width();
    let h = frame.height();
    if guard.actual_frame_dims != Some((w, h)) {
        info!(width = w, height = h, "decoded frame dimensions updated");
        guard.actual_frame_dims = Some((w, h));
    }

    // Extract HardwareBuffer from GPU frame's native handle.
    let native = match frame.native_handle() {
        Some(rusty_codecs::format::NativeFrameHandle::HardwareBuffer(info)) => info,
        _ => {
            tracing::trace!("frame has no HardwareBuffer native handle");
            return 0;
        }
    };

    // The HardwareBufferRef in `native.buffer` already holds an acquired
    // reference. We leak it into a raw pointer for the Kotlin side.
    // releaseHardwareBuffer will reconstruct and drop it.
    let raw_ptr = native.buffer.as_ptr();

    // Prevent the Rust drop from releasing the reference — Kotlin owns it now.
    std::mem::forget(native.buffer);

    raw_ptr as jlong
}

/// Releases a HardwareBuffer previously returned by `nextHardwareBuffer`.
///
/// # Safety
///
/// `buffer_ptr` must be a non-zero pointer previously returned by
/// `nextHardwareBuffer` and must not be used after this call.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_releaseHardwareBuffer(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    buffer_ptr: jlong,
) {
    if buffer_ptr == 0 {
        return;
    }

    // SAFETY: buffer_ptr was obtained from nextHardwareBuffer which
    // leaked a HardwareBufferRef (acquired reference). We call the NDK
    // release function directly to decrement the refcount.
    unsafe extern "C" {
        fn AHardwareBuffer_release(buffer: *mut std::ffi::c_void);
    }
    // SAFETY: buffer_ptr is a valid AHardwareBuffer* with an acquired
    // reference from nextHardwareBuffer.
    unsafe {
        AHardwareBuffer_release(buffer_ptr as *mut std::ffi::c_void);
    }
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
    let session = unsafe { handle_from_jlong(handle) };

    if let Err(e) = runtime().block_on(async {
        // Clone the Live handle while holding the lock briefly, then
        // release before the async publish call.
        let live = {
            let guard = session
                .lock()
                .map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
            guard.live.clone()
        };

        let broadcast = LocalBroadcast::new();
        live.publish(&broadcast_name, &broadcast)
            .await
            .context("failed to announce broadcast")?;

        info!(name = %broadcast_name, "publishing broadcast");

        // Re-acquire lock to store the broadcast. If disconnect() raced
        // between the two lock acquisitions, `handle_from_jlong` still
        // holds an Arc clone keeping the SessionHandle alive — the
        // broadcast is stored but dropped when this function returns.
        // This is benign: the user disconnected mid-publish.
        session
            .lock()
            .map_err(|_| anyhow::anyhow!("session lock poisoned"))?
            ._publish_broadcast = Some(broadcast);

        anyhow::Ok(())
    }) {
        tracing::error!("startPublish failed: {e:#}");
    }
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
    let session = unsafe { handle_from_jlong(handle) };
    let Ok(guard) = session.lock() else {
        return;
    };

    let Ok(bytes) = env.convert_byte_array(data) else {
        tracing::error!("failed to read camera frame byte array");
        return;
    };

    let expected_len = (width * height * 4) as usize;
    if bytes.len() != expected_len {
        tracing::warn!(
            actual = bytes.len(),
            expected = expected_len,
            "camera frame data length mismatch, skipping"
        );
        return;
    }

    let frame = VideoFrame::new_rgba(
        bytes::Bytes::from(bytes),
        width as u32,
        height as u32,
        Duration::ZERO,
    );

    if let Some(source) = guard.camera_source.as_ref()
        && let Ok(mut src) = source.lock()
    {
        src.push_frame(frame);
    }
}

/// Pushes a camera frame as NV12 planes into the publish pipeline.
///
/// The Y and UV planes may have strides larger than the image width due
/// to hardware padding. The encoder handles stride-aware copies.
///
/// # Safety
///
/// The `handle` must be a valid pointer returned by `dial`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_pushCameraNv12(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    y_data: JByteArray<'_>,
    uv_data: JByteArray<'_>,
    width: jint,
    height: jint,
    y_stride: jint,
    uv_stride: jint,
) {
    if handle == 0 {
        return;
    }

    // SAFETY: handle was created by dial and has not been freed.
    let session = unsafe { handle_from_jlong(handle) };
    let Ok(mut guard) = session.lock() else {
        return;
    };

    let Ok(y_bytes) = env.convert_byte_array(y_data) else {
        tracing::error!("failed to read Y plane byte array");
        return;
    };
    let Ok(uv_bytes) = env.convert_byte_array(uv_data) else {
        tracing::error!("failed to read UV plane byte array");
        return;
    };

    let y_len = y_bytes.len();
    let uv_len = uv_bytes.len();

    let planes = Nv12Planes {
        y_data: y_bytes,
        y_stride: y_stride as u32,
        uv_data: uv_bytes,
        uv_stride: uv_stride as u32,
        width: width as u32,
        height: height as u32,
    };

    let frame = VideoFrame::new_nv12(planes, Duration::ZERO);

    let source = guard.camera_source.clone();
    let frame_idx = guard.cam_frames_pushed;
    guard.cam_frames_pushed += 1;

    if let Some(source) = source
        && let Ok(mut src) = source.lock()
    {
        if frame_idx == 0 {
            info!(
                width, height, y_stride, uv_stride,
                y_len, uv_len, started = src.started,
                "first NV12 camera frame from JNI"
            );
        }
        src.push_frame(frame);
    }
}

/// Returns a human-readable status string with encode/decode/network stats.
///
/// # Safety
///
/// The `handle` must be a valid pointer returned by `connect` or `dial`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getStatusLine<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> jni::objects::JString<'a> {
    // Wrap in RefCell so the closure and final return can both call env.new_string().
    let env = std::cell::RefCell::new(env);
    let empty = || env.borrow_mut().new_string("").expect("new_string");

    if handle == 0 {
        return empty();
    }

    // SAFETY: handle was created by connect/dial and has not been freed.
    let session = unsafe { handle_from_jlong(handle) };
    let Ok(guard) = session.lock() else {
        return empty();
    };

    let elapsed = guard.created_at.elapsed().as_secs();
    let cam = guard.cam_frames_pushed;
    let dec = guard.dec_frames_rendered;

    // Video track info.
    let video_info = guard
        .video
        .as_ref()
        .map(|v| {
            let rendition = v.rendition();
            let decoder = v.decoder_name();
            format!("dec:{decoder} trk:{rendition}")
        })
        .unwrap_or_else(|| "no video".into());

    // Frame dimensions.
    let dims = guard
        .actual_frame_dims
        .map(|(w, h)| format!("{w}x{h}"))
        .unwrap_or_else(|| "?".into());

    // Network stats from the call's QUIC connection.
    let net_info = {
        let conn = guard
            ._call
            .as_ref()
            .map(|c| c.session().conn().clone())
            .or_else(|| guard._session.as_ref().map(|s| s.conn().clone()));
        if let Some(conn) = conn {
            let paths = conn.paths().get();
            let rtt = paths
                .iter()
                .find(|p| p.is_selected())
                .map(|p| p.rtt())
                .unwrap_or_default();
            format!("rtt:{}ms", rtt.as_millis())
        } else {
            String::new()
        }
    };

    // Playout buffer and jitter from the remote broadcast's clock.
    let playout_info = guard
        ._broadcast
        .as_ref()
        .map(|b| {
            let buf = b.clock().buffer();
            let jitter = b.clock().jitter();
            format!(
                "buf:{}ms jit:{:.0}ms",
                buf.as_millis(),
                jitter.as_secs_f64() * 1000.0,
            )
        })
        .unwrap_or_default();

    let line = format!(
        "{video_info} {dims} | cam:{cam} dec:{dec} | {net_info} {playout_info} | {elapsed}s",
    );

    env.borrow_mut().new_string(&line).unwrap_or_else(|_| empty())
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

    // SAFETY: handle was created by connect and is being consumed here.
    let session = unsafe { handle_take(handle) };

    // The Arc may still have clones if another JNI call is in progress.
    // We try to lock and shut down; if we can't, the Drop will clean up.
    if let Ok(guard) = session.lock() {
        runtime().block_on(async {
            guard.live.shutdown().await;
        });
    }

    info!("disconnected");
}

// ── EGL extension JNI wrappers ──────────────────────────────────────
//
// These functions wrap EGL/GLES extension calls that the Android Java
// SDK does not expose. Function pointers are resolved via eglGetProcAddress
// (the EGL-spec way to look up extension functions) and cached in OnceLock
// statics to avoid per-frame lookup overhead.
//
// We cannot declare eglGetProcAddress as an extern "C" symbol because
// libEGL.so is not linked to our .so — that causes an UnsatisfiedLinkError
// at dlopen time. Instead we dlopen("libEGL.so") and dlsym from it.

type EglGetNativeClientBufferFn =
    unsafe extern "C" fn(*const std::ffi::c_void) -> *mut std::ffi::c_void;
type EglCreateImageFn = unsafe extern "C" fn(
    *mut std::ffi::c_void,
    *mut std::ffi::c_void,
    u32,
    *mut std::ffi::c_void,
    *const i32,
) -> *mut std::ffi::c_void;
type EglDestroyImageFn = unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> i32;
type GlEglImageTargetFn = unsafe extern "C" fn(u32, *mut std::ffi::c_void);

type EglGetProcAddressFn = unsafe extern "C" fn(*const std::ffi::c_char) -> *mut std::ffi::c_void;

static FN_EGL_GET_PROC_ADDRESS: OnceLock<Option<EglGetProcAddressFn>> = OnceLock::new();
static FN_EGL_GET_NATIVE_CLIENT_BUFFER: OnceLock<Option<EglGetNativeClientBufferFn>> =
    OnceLock::new();
static FN_EGL_CREATE_IMAGE: OnceLock<Option<EglCreateImageFn>> = OnceLock::new();
static FN_EGL_DESTROY_IMAGE: OnceLock<Option<EglDestroyImageFn>> = OnceLock::new();
static FN_GL_EGL_IMAGE_TARGET_TEXTURE: OnceLock<Option<GlEglImageTargetFn>> = OnceLock::new();

/// Loads `eglGetProcAddress` from `libEGL.so` via dlopen + dlsym.
fn load_egl_get_proc_address() -> Option<EglGetProcAddressFn> {
    *FN_EGL_GET_PROC_ADDRESS.get_or_init(|| {
        // SAFETY: dlopen with RTLD_NOLOAD only returns a handle if
        // libEGL.so is already loaded (it always is on Android — the
        // Java side loads it). dlsym then resolves from that library.
        unsafe {
            let lib = libc::dlopen(
                b"libEGL.so\0".as_ptr().cast(),
                libc::RTLD_NOLOAD | libc::RTLD_LAZY,
            );
            if lib.is_null() {
                tracing::error!("dlopen(libEGL.so) failed");
                return None;
            }
            let sym = libc::dlsym(lib, b"eglGetProcAddress\0".as_ptr().cast());
            if sym.is_null() {
                tracing::error!("dlsym(eglGetProcAddress) failed");
                return None;
            }
            Some(std::mem::transmute_copy(&sym))
        }
    })
}

/// Resolves an EGL/GL extension function pointer via `eglGetProcAddress`.
///
/// # Safety
///
/// The caller must ensure `T` matches the actual signature of the
/// resolved symbol. The name must be a null-terminated byte string.
unsafe fn resolve_egl_proc<T: Copy>(name: &[u8]) -> Option<T> {
    let get_proc = load_egl_get_proc_address()?;
    // SAFETY: eglGetProcAddress is the EGL-spec way to resolve extension
    // functions. The caller guarantees name is null-terminated and T
    // matches the symbol's actual signature.
    unsafe {
        let sym = get_proc(name.as_ptr().cast());
        if sym.is_null() {
            None
        } else {
            Some(std::mem::transmute_copy(&sym))
        }
    }
}

fn get_egl_get_native_client_buffer() -> Option<EglGetNativeClientBufferFn> {
    *FN_EGL_GET_NATIVE_CLIENT_BUFFER.get_or_init(|| {
        // SAFETY: The function signature matches the EGL extension spec.
        unsafe { resolve_egl_proc(b"eglGetNativeClientBufferANDROID\0") }
    })
}

fn get_egl_create_image() -> Option<EglCreateImageFn> {
    *FN_EGL_CREATE_IMAGE.get_or_init(|| {
        // SAFETY: The function signature matches the EGL extension spec.
        unsafe { resolve_egl_proc(b"eglCreateImageKHR\0") }
    })
}

fn get_egl_destroy_image() -> Option<EglDestroyImageFn> {
    *FN_EGL_DESTROY_IMAGE.get_or_init(|| {
        // SAFETY: The function signature matches the EGL extension spec.
        unsafe { resolve_egl_proc(b"eglDestroyImageKHR\0") }
    })
}

fn get_gl_egl_image_target_texture() -> Option<GlEglImageTargetFn> {
    *FN_GL_EGL_IMAGE_TARGET_TEXTURE.get_or_init(|| {
        // SAFETY: The function signature matches the GLES extension spec.
        unsafe { resolve_egl_proc(b"glEGLImageTargetTexture2DOES\0") }
    })
}

/// EGL_ANDROID_get_native_client_buffer: converts an AHardwareBuffer
/// pointer into an EGLClientBuffer for use with eglCreateImageKHR.
///
/// # Safety
///
/// `hardware_buffer_ptr` must be a valid `AHardwareBuffer*` with an
/// acquired reference.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_MainActivity_eglGetNativeClientBufferANDROID(
    _env: JNIEnv<'_>,
    _this: jni::objects::JObject<'_>,
    hardware_buffer_ptr: jlong,
) -> jlong {
    let Some(func) = get_egl_get_native_client_buffer() else {
        tracing::error!("eglGetNativeClientBufferANDROID not available");
        return 0;
    };

    // SAFETY: The caller guarantees hardware_buffer_ptr is a valid
    // AHardwareBuffer* with an acquired reference.
    let result = unsafe { func(hardware_buffer_ptr as *const std::ffi::c_void) };
    result as jlong
}

/// EGL_KHR_image_base: creates an EGLImage from an EGLClientBuffer.
///
/// # Safety
///
/// `client_buffer` must be a valid EGLClientBuffer from
/// `eglGetNativeClientBufferANDROID`. `display` is the raw EGLDisplay
/// pointer from `EGLDisplay.getNativeHandle()`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_MainActivity_eglCreateImageKHR(
    mut env: JNIEnv<'_>,
    _this: jni::objects::JObject<'_>,
    display: jni::objects::JObject<'_>,
    _context: jni::objects::JObject<'_>,
    target: jint,
    client_buffer: jlong,
    attrs: jni::objects::JIntArray<'_>,
) -> jlong {
    let Some(func) = get_egl_create_image() else {
        tracing::error!("eglCreateImageKHR not available");
        return 0;
    };

    let egl_display = get_native_handle(&mut env, &display);

    // Convert Java int array to native array.
    // SAFETY: attrs is a valid JIntArray from the JVM, and NoCopyBack avoids
    // writing back changes we did not make.
    let attr_vec = unsafe {
        env.get_array_elements(&attrs, jni::objects::ReleaseMode::NoCopyBack)
            .ok()
    };
    let attr_ptr = attr_vec
        .as_ref()
        .map(|a: &jni::objects::AutoElements<'_, '_, '_, jni::sys::jint>| a.as_ptr())
        .unwrap_or(std::ptr::null_mut());

    // SAFETY: The caller guarantees client_buffer is a valid
    // EGLClientBuffer and display wraps a valid EGLDisplay. The
    // attrs array is null-terminated per the EGL spec.
    let result = unsafe {
        func(
            egl_display as *mut std::ffi::c_void,
            std::ptr::null_mut(), // EGL_NO_CONTEXT
            target as u32,
            client_buffer as *mut std::ffi::c_void,
            attr_ptr,
        )
    };
    result as jlong
}

/// EGL_KHR_image_base: destroys an EGLImage.
///
/// # Safety
///
/// `image` must be a valid EGLImage handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_MainActivity_eglDestroyImageKHR(
    mut env: JNIEnv<'_>,
    _this: jni::objects::JObject<'_>,
    display: jni::objects::JObject<'_>,
    image: jlong,
) {
    let Some(func) = get_egl_destroy_image() else {
        return;
    };

    let egl_display = get_native_handle(&mut env, &display);

    // SAFETY: The caller guarantees image is a valid EGLImage and
    // display wraps a valid EGLDisplay.
    unsafe {
        func(
            egl_display as *mut std::ffi::c_void,
            image as *mut std::ffi::c_void,
        );
    }
}

/// GL_OES_EGL_image_external: binds an EGLImage to a GL texture target.
///
/// # Safety
///
/// `image` must be a valid EGLImage handle. A GL context must be current.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_MainActivity_glEGLImageTargetTexture2DOES(
    _env: JNIEnv<'_>,
    _this: jni::objects::JObject<'_>,
    target: jint,
    image: jlong,
) {
    let Some(func) = get_gl_egl_image_target_texture() else {
        tracing::error!("glEGLImageTargetTexture2DOES not available");
        return;
    };

    // SAFETY: The caller guarantees image is a valid EGLImage and a
    // GL context is current on this thread.
    unsafe {
        func(target as u32, image as *mut std::ffi::c_void);
    }
}

/// Extracts the native EGL handle from a Java EGL14 wrapper object.
///
/// Uses reflection to call `getNativeHandle()` which returns the raw
/// native pointer as a long.
fn get_native_handle(env: &mut JNIEnv<'_>, obj: &jni::objects::JObject<'_>) -> jlong {
    // EGLDisplay wraps a long mEGLDisplay field on API 17+.
    // We access it via the public getNativeHandle() method.
    match env.call_method(obj, "getNativeHandle", "()J", &[]) {
        Ok(val) => val.j().unwrap_or(0),
        Err(_) => {
            tracing::warn!("getNativeHandle() not available, trying mEGLDisplay field");
            // Fallback: access the private field directly.
            match env.get_field(obj, "mEGLDisplay", "J") {
                Ok(val) => val.j().unwrap_or(0),
                Err(_) => 0,
            }
        }
    }
}

// ── Logcat tracing ──────────────────────────────────────────────────

/// Routes `tracing` output to Android logcat with standard `fmt::Layer` formatting.
mod logcat {
    use std::io::Write;

    use tracing::Level;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::prelude::*;

    unsafe extern "C" {
        fn __android_log_write(prio: i32, tag: *const i8, text: *const i8) -> i32;
    }

    const TAG: &str = "iroh_live\0";

    // Android log priorities.
    const VERBOSE: i32 = 2;
    const DEBUG: i32 = 3;
    const INFO: i32 = 4;
    const WARN: i32 = 5;
    const ERROR: i32 = 6;

    pub(crate) fn init() -> Result<(), tracing_subscriber::util::TryInitError> {
        let filter = EnvFilter::new(
            "warn,\
             iroh=debug,\
             iroh_live=debug,\
             iroh_moq=debug,\
             moq_media=debug,\
             hang=debug,\
             moq_lite=debug,\
             rusty_codecs=debug,\
             rusty_capture=debug,\
             cpal=debug,\
             firewheel=debug,\
             firewheel_cpal=debug,\
             oboe=debug",
        );

        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .without_time()
                    .with_writer(LogcatMakeWriter),
            )
            .try_init()
    }

    struct LogcatMakeWriter;

    impl<'a> MakeWriter<'a> for LogcatMakeWriter {
        type Writer = LogcatWriter;

        fn make_writer(&'a self) -> Self::Writer {
            LogcatWriter {
                buf: Vec::with_capacity(256),
                priority: INFO,
            }
        }

        fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
            let priority = match *meta.level() {
                Level::ERROR => ERROR,
                Level::WARN => WARN,
                Level::INFO => INFO,
                Level::DEBUG => DEBUG,
                Level::TRACE => VERBOSE,
            };
            LogcatWriter {
                buf: Vec::with_capacity(256),
                priority,
            }
        }
    }

    /// Buffers a single log line, writes to logcat on drop.
    struct LogcatWriter {
        buf: Vec<u8>,
        priority: i32,
    }

    impl Write for LogcatWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Drop for LogcatWriter {
        fn drop(&mut self) {
            if self.buf.is_empty() {
                return;
            }
            // Trim trailing newline that fmt layer adds.
            if self.buf.last() == Some(&b'\n') {
                self.buf.pop();
            }
            // __android_log_write needs a null-terminated string.
            // Replace any interior nulls to avoid truncation.
            for b in &mut self.buf {
                if *b == 0 {
                    *b = b'?';
                }
            }
            self.buf.push(0);
            unsafe {
                __android_log_write(self.priority, TAG.as_ptr().cast(), self.buf.as_ptr().cast());
            }
        }
    }
}
