#![cfg(target_os = "android")]
//! JNI bridge for the iroh-live Android demo app.
//!
//! Exposes a minimal set of functions to Kotlin: connect to a broadcast,
//! poll decoded video frames, publish camera frames, and disconnect.
//! A global tokio runtime drives all async work.

mod logcat;

use std::{
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use iroh_live::{Call, Live, ticket::LiveTicket};
use jni::{
    JNIEnv, JavaVM,
    objects::{JByteArray, JClass, JString},
    sys::{jint, jlong},
};
use moq_media::{
    AudioBackend,
    codec::{AudioCodec, DynamicVideoDecoder, VideoCodec},
    format::{
        AudioPreset, DecodeConfig, PlaybackConfig, VideoEncoderConfig, VideoFrame, VideoPreset,
    },
    pipeline::{VideoDecoderPipeline, VideoEncoderPipeline},
    publish::{LocalBroadcast, VideoInput},
    subscribe::{AudioTrack, RemoteBroadcast, VideoTrack},
    transport::media_pipe,
};
use moq_media_android::{
    camera::{CameraFrameSource, SharedCameraSource},
    handle,
    renderer::AndroidRenderer,
};
use n0_watcher::Watcher;
use rusty_codecs::format::Nv12Planes;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;
use tracing::info;

// ── Constants ───────────────────────────────────────────────────────

const LOGCAT_FILTER: &str = "\
    warn,\
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
    oboe=debug";

// ── JNI lifecycle ───────────────────────────────────────────────────

/// Initializes ndk-context and tracing on library load.
///
/// Called automatically by the JVM when `System.loadLibrary` loads this .so.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut std::ffi::c_void) -> jint {
    // SAFETY: The JVM guarantees `vm` is valid during JNI_OnLoad.
    // Null activity — cpal only needs the VM pointer for Oboe init.
    unsafe {
        ndk_context::initialize_android_context(
            vm.get_java_vm_pointer().cast(),
            std::ptr::null_mut(),
        );
    }
    let _ = logcat::init(LOGCAT_FILTER);
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
/// `Arc<Mutex<..>>` allows concurrent JNI calls from different threads
/// (render loop vs camera callback).
struct SessionHandle {
    /// Network session — `None` for local debug pipelines.
    live: Option<Live>,
    /// Subscribe-only MoQ session (from `connect`).
    #[allow(dead_code, reason = "kept alive to sustain the transport session")]
    session: Option<iroh_live::moq::MoqSession>,
    /// Call handle (from `dial`). Owns the session + local broadcast.
    #[allow(dead_code, reason = "kept alive to sustain the call")]
    call: Option<Call>,
    /// Remote broadcast subscription.
    remote: Option<RemoteBroadcast>,
    /// Decoded video track.
    video: Option<VideoTrack>,
    /// Audio track — kept alive to sustain playback via cpal/Oboe.
    #[allow(dead_code, reason = "kept alive to sustain audio playout")]
    audio: Option<AudioTrack>,
    /// Audio backend — must outlive audio tracks.
    #[allow(dead_code, reason = "must outlive audio tracks")]
    audio_backend: Option<AudioBackend>,
    /// Local broadcast for publish mode.
    #[allow(dead_code, reason = "kept alive to sustain the broadcast")]
    broadcast: Option<LocalBroadcast>,
    /// Connection ticket string (set after publish).
    ticket: Option<String>,
    /// Encoder pipeline — kept alive to sustain the H264 debug pipeline.
    #[allow(dead_code, reason = "kept alive to sustain the encoder thread")]
    encoder_pipeline: Option<VideoEncoderPipeline>,
    /// GLES2 renderer — behind its own lock so rendering doesn't block
    /// camera frame pushes on the SessionHandle mutex.
    renderer: Arc<Mutex<Option<AndroidRenderer>>>,
    /// Actual decoded frame dimensions (more reliable than catalog).
    frame_dims: Option<(u32, u32)>,
    /// Camera source for pushing frames into the publish pipeline.
    camera_source: Option<Arc<Mutex<CameraFrameSource>>>,
    /// Counters for the status line.
    cam_frames_pushed: u64,
    dec_frames_rendered: u64,
    created_at: Instant,
}

type SharedHandle = Arc<Mutex<SessionHandle>>;

fn to_jlong(h: SharedHandle) -> jlong {
    handle::to_i64(h)
}

/// # Safety
/// `h` must be a live handle from [`to_jlong`].
unsafe fn borrow_handle(h: jlong) -> SharedHandle {
    unsafe { handle::from_i64(h) }
}

/// # Safety
/// `h` must be a live handle from [`to_jlong`]; must not be used after.
unsafe fn take_handle(h: jlong) -> SharedHandle {
    unsafe { handle::take_i64(h) }
}

/// Reads a JNI string, returning `None` on failure.
fn read_jstring(env: &mut JNIEnv<'_>, s: &JString<'_>) -> Option<String> {
    match env.get_string(s) {
        Ok(s) => Some(s.into()),
        Err(e) => {
            tracing::error!("failed to read JNI string: {e}");
            None
        }
    }
}

// ── JNI: connect (subscribe-only) ───────────────────────────────────

/// Connects to a remote broadcast. Returns a session handle or 0.
///
/// # Safety
/// Must be called from JNI with valid arguments.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_connect(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    ticket: JString<'_>,
) -> jlong {
    let Some(ticket_str) = read_jstring(&mut env, &ticket) else {
        return 0;
    };
    match runtime().block_on(connect_impl(ticket_str)) {
        Ok(h) => h,
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

    let live = Live::from_env().await?.with_router().with_gossip().spawn();
    info!(broadcast = %ticket.broadcast_name, "connecting to broadcast");

    let sub = live
        .subscribe(ticket.endpoint.clone(), &ticket.broadcast_name)
        .await?;
    let (session, remote, _signals) = sub.into_parts();

    let video = remote
        .video()
        .inspect_err(|e| tracing::warn!("no video track: {e}"))
        .ok();

    let audio_backend = AudioBackend::default();
    let audio = remote
        .audio(&audio_backend)
        .await
        .inspect_err(|e| tracing::warn!("no audio track: {e:#}"))
        .ok();

    let handle = SessionHandle {
        live: Some(live),
        session: Some(session),
        call: None,
        remote: Some(remote),
        video,
        audio,
        audio_backend: Some(audio_backend),
        broadcast: None,
        ticket: None,
        encoder_pipeline: None,
        renderer: Arc::new(Mutex::new(None)),
        frame_dims: None,
        camera_source: None,
        cam_frames_pushed: 0,
        dec_frames_rendered: 0,
        created_at: Instant::now(),
    };
    Ok(to_jlong(Arc::new(Mutex::new(handle))))
}

// ── JNI: dial (bidirectional call) ──────────────────────────────────

/// Dials a remote peer. Sets up camera+mic publishing, subscribes to
/// remote media. Returns a session handle or 0.
///
/// # Safety
/// Must be called from JNI with valid arguments.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_dial(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    ticket: JString<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let Some(ticket_str) = read_jstring(&mut env, &ticket) else {
        return 0;
    };
    match runtime().block_on(dial_impl(
        ticket_str,
        camera_width as u32,
        camera_height as u32,
    )) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("dial failed: {e:#}");
            0
        }
    }
}

async fn dial_impl(ticket_str: String, cam_w: u32, cam_h: u32) -> Result<jlong> {
    info!(ticket = %ticket_str, cam_w, cam_h, "parsing call ticket");
    let ticket: LiveTicket = ticket_str.parse().context("failed to parse call ticket")?;

    let live = Live::from_env().await?.with_router().with_gossip().spawn();
    info!(id = %live.endpoint().id().fmt_short(), "endpoint ready");

    // Camera video source.
    let broadcast = LocalBroadcast::new();
    let camera_source = Arc::new(Mutex::new(CameraFrameSource::new(cam_w, cam_h)));
    let shared_source = SharedCameraSource {
        inner: Arc::clone(&camera_source),
    };
    broadcast
        .video()
        .set(VideoInput::new(
            shared_source,
            VideoCodec::best_available().expect("no video codec available"),
            [VideoPreset::P720],
        ))
        .context("failed to set video source")?;

    // Microphone audio via cpal/Oboe.
    let audio_backend = AudioBackend::default();
    let mic = audio_backend
        .default_input()
        .await
        .context("failed to open microphone")?;
    broadcast
        .audio()
        .set(mic, AudioCodec::Opus, [AudioPreset::Hq])
        .context("failed to set audio source")?;

    // Dial and subscribe.
    let call = Call::dial(&live, ticket.endpoint, broadcast).await?;
    info!("call connected");

    let tracks = call
        .remote()
        .media(&audio_backend, PlaybackConfig::default())
        .await
        .context("failed to subscribe to remote media")?;

    info!(
        has_video = tracks.video.is_some(),
        has_audio = tracks.audio.is_some(),
        "remote media subscribed"
    );

    let handle = SessionHandle {
        live: Some(live),
        session: None,
        call: Some(call),
        remote: Some(tracks.broadcast),
        video: tracks.video,
        audio: tracks.audio,
        audio_backend: Some(audio_backend),
        broadcast: None,
        ticket: None,
        encoder_pipeline: None,
        renderer: Arc::new(Mutex::new(None)),
        frame_dims: None,
        camera_source: Some(camera_source),
        cam_frames_pushed: 0,
        dec_frames_rendered: 0,
        created_at: Instant::now(),
    };
    Ok(to_jlong(Arc::new(Mutex::new(handle))))
}

// ── JNI: debug pipelines (local, no network) ────────────────────────

/// Starts a direct camera passthrough pipeline (no encode/decode).
///
/// Camera frames pushed via `pushCameraNv12` are rendered directly.
///
/// # Safety
/// Must be called from JNI with valid arguments.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_startDirect(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    match start_direct_impl(camera_width as u32, camera_height as u32) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("startDirect failed: {e:#}");
            0
        }
    }
}

fn start_direct_impl(cam_w: u32, cam_h: u32) -> Result<jlong> {
    info!(cam_w, cam_h, "starting direct camera pipeline");
    let camera_source = Arc::new(Mutex::new(CameraFrameSource::new(cam_w, cam_h)));
    let shared_source = SharedCameraSource {
        inner: Arc::clone(&camera_source),
    };
    let shutdown = CancellationToken::new();
    let video = VideoTrack::from_video_source("direct".to_string(), shutdown, shared_source);
    let handle = SessionHandle {
        live: None,
        session: None,
        call: None,
        remote: None,
        video: Some(video),
        audio: None,
        audio_backend: None,
        broadcast: None,
        ticket: None,
        encoder_pipeline: None,
        renderer: Arc::new(Mutex::new(None)),
        frame_dims: None,
        camera_source: Some(camera_source),
        cam_frames_pushed: 0,
        dec_frames_rendered: 0,
        created_at: Instant::now(),
    };
    Ok(to_jlong(Arc::new(Mutex::new(handle))))
}

/// Starts a local H264 encode→decode pipeline (no network).
///
/// Camera → H264 HW encode → in-memory pipe → H264 HW decode → render.
///
/// # Safety
/// Must be called from JNI with valid arguments.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_startH264(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let _guard = runtime().enter();
    match start_h264_impl(camera_width as u32, camera_height as u32) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("startH264 failed: {e:#}");
            0
        }
    }
}

fn start_h264_impl(cam_w: u32, cam_h: u32) -> Result<jlong> {
    info!(cam_w, cam_h, "starting local H264 pipeline");
    let camera_source = Arc::new(Mutex::new(CameraFrameSource::new(cam_w, cam_h)));
    let shared_source = SharedCameraSource {
        inner: Arc::clone(&camera_source),
    };

    let enc_config = VideoEncoderConfig::from_preset(VideoPreset::P720);
    let encoder = VideoCodec::H264
        .create_encoder(enc_config)
        .context("failed to create H264 encoder")?;
    let video_config = encoder.config();

    let (sink, pipe_source) = media_pipe(32);
    let encoder_pipeline =
        VideoEncoderPipeline::new(shared_source, encoder, sink, Default::default());

    let decode_config = DecodeConfig::default();
    let decoder = VideoDecoderPipeline::new::<DynamicVideoDecoder>(
        "h264-debug".to_string(),
        pipe_source,
        &video_config,
        &decode_config,
        Default::default(),
    )
    .context("failed to create H264 decoder pipeline")?;
    let video = VideoTrack::from_pipeline(decoder);

    let handle = SessionHandle {
        live: None,
        session: None,
        call: None,
        remote: None,
        video: Some(video),
        audio: None,
        audio_backend: None,
        broadcast: None,
        ticket: None,
        encoder_pipeline: Some(encoder_pipeline),
        renderer: Arc::new(Mutex::new(None)),
        frame_dims: None,
        camera_source: Some(camera_source),
        cam_frames_pushed: 0,
        dec_frames_rendered: 0,
        created_at: Instant::now(),
    };
    Ok(to_jlong(Arc::new(Mutex::new(handle))))
}

// ── JNI: video frame access ─────────────────────────────────────────

/// Returns `(width << 32) | height`, or 0 if unknown.
///
/// # Safety
/// `handle` must be a live session handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getVideoDimensions(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jlong {
    if handle == 0 {
        return 0;
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else { return 0 };

    // Prefer actual decoded dims over catalog (avoids codec padding issues).
    if let Some((w, h)) = guard.frame_dims {
        return ((w as i64) << 32) | (h as i64);
    }
    let (Some(video), Some(remote)) = (guard.video.as_ref(), guard.remote.as_ref()) else {
        return 0;
    };
    let catalog = remote.catalog();
    catalog
        .video
        .renditions
        .get(video.rendition())
        .map(|c| {
            let w = c.coded_width.unwrap_or(0) as i64;
            let h = c.coded_height.unwrap_or(0) as i64;
            (w << 32) | h
        })
        .unwrap_or(0)
}

/// Returns a raw `AHardwareBuffer*` for the latest decoded frame, or 0.
///
/// Caller must call `releaseHardwareBuffer` when done.
///
/// # Safety
/// `handle` must be a live session handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_nextHardwareBuffer(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jlong {
    if handle == 0 {
        return 0;
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(mut guard) = session.lock() else {
        return 0;
    };

    let video = guard.video.as_mut();
    let frame = video.and_then(|v| v.try_recv());
    let Some(frame) = frame else { return 0 };

    guard.dec_frames_rendered += 1;
    let (w, h) = (frame.width(), frame.height());
    if guard.frame_dims != Some((w, h)) {
        info!(width = w, height = h, "decoded frame dimensions updated");
        guard.frame_dims = Some((w, h));
    }

    // Fast path: HW decoder produced a GPU-backed HardwareBuffer.
    if let Some(rusty_codecs::format::NativeFrameHandle::HardwareBuffer(info)) =
        frame.native_handle()
    {
        let raw_ptr = info.buffer.as_ptr();
        std::mem::forget(info.buffer); // Kotlin owns it now.
        return raw_ptr as jlong;
    }

    // Slow path: CPU frame (direct mode or SW decoder). Convert to RGBA and
    // wrap in an AHardwareBuffer so the existing EGL render path works.
    let img = frame.rgba_image();
    create_rgba_hardware_buffer(img.as_raw(), w, h)
        .map(|ptr| ptr as jlong)
        .unwrap_or(0)
}

/// Releases a HardwareBuffer returned by `nextHardwareBuffer`.
///
/// # Safety
/// `buffer_ptr` must be from `nextHardwareBuffer` and not yet released.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_releaseHardwareBuffer(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    buffer_ptr: jlong,
) {
    if buffer_ptr == 0 {
        return;
    }
    // SAFETY: buffer_ptr is a valid AHardwareBuffer* with an acquired reference.
    unsafe { AHardwareBuffer_release(buffer_ptr as *mut c_void) }
}

// ── JNI: Rust-side rendering ────────────────────────────────────────

/// Creates the EGL context + GL renderer for the given Android Surface.
///
/// Replaces the Kotlin-side EGL setup. The Surface is converted to an
/// `ANativeWindow` via NDK, then an EGL display/context/surface is
/// created in Rust. Must be called from the render thread.
///
/// # Safety
/// `handle` must be a live session handle. `surface` must be a valid
/// `android.view.Surface` Java object.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_initSurface(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    surface: jni::objects::JObject<'_>,
) {
    if handle == 0 {
        return;
    }
    // ANativeWindow_fromSurface requires the raw JNIEnv pointer.
    let raw_env = env.get_raw();
    let raw_surface = surface.as_raw();
    let native_window =
        unsafe { ndk::native_window::NativeWindow::from_surface(raw_env, raw_surface) };
    let Some(native_window) = native_window else {
        tracing::error!("ANativeWindow_fromSurface returned null");
        return;
    };

    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else { return };
    match unsafe { AndroidRenderer::new(native_window.ptr().as_ptr().cast()) } {
        Ok(r) => *guard.renderer.lock().expect("renderer lock") = Some(r),
        Err(e) => tracing::error!("initSurface failed: {e:#}"),
    }
}

/// Tears down the EGL surface and context. Called when the render loop exits.
///
/// # Safety
/// `handle` must be a live session handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_teardownSurface(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else { return };
    let Ok(mut renderer_guard) = guard.renderer.lock() else {
        return;
    };
    if let Some(ref renderer) = *renderer_guard {
        renderer.teardown();
    }
    *renderer_guard = None;
}

/// Polls for the next decoded frame, renders it, and swaps EGL buffers.
///
/// Returns `true` if a frame was rendered, `false` if no frame was available.
///
/// # Safety
/// `handle` must be a live session handle. Must be called from the render thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_renderNextFrame(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    surface_width: jint,
    surface_height: jint,
    rotation_degrees: jint,
) -> bool {
    if handle == 0 {
        return false;
    }
    let session = unsafe { borrow_handle(handle) };

    // Phase 1: briefly lock the session to extract the next frame.
    // This keeps the lock time short so camera pushes aren't blocked
    // during the (potentially slow) GL rendering in phase 2.
    let (frame, w, h, renderer_arc) = {
        let Ok(mut guard) = session.lock() else {
            return false;
        };
        let frame = guard.video.as_mut().and_then(|v| v.try_recv());
        let Some(frame) = frame else { return false };
        guard.dec_frames_rendered += 1;
        let (w, h) = (frame.width(), frame.height());
        if guard.frame_dims != Some((w, h)) {
            info!(width = w, height = h, "decoded frame dimensions updated");
            guard.frame_dims = Some((w, h));
        }
        let renderer_arc = Arc::clone(&guard.renderer);
        (frame, w, h, renderer_arc)
    }; // session mutex released here

    // Phase 2: lock only the renderer for GL rendering.
    let Ok(renderer_guard) = renderer_arc.lock() else {
        return false;
    };
    let Some(ref renderer) = *renderer_guard else {
        return false;
    };
    // Re-bind the EGL context in case the coroutine dispatcher switched threads.
    renderer.make_current();
    let rot = rotation_degrees as u32;

    // Try GPU HardwareBuffer first (zero-copy from HW decoder).
    if let Some(rusty_codecs::format::NativeFrameHandle::HardwareBuffer(info)) =
        frame.native_handle()
    {
        let raw_ptr = info.buffer.as_ptr();
        unsafe {
            renderer.render_hardware_buffer(
                raw_ptr as *mut c_void,
                surface_width,
                surface_height,
                w,
                h,
                rot,
            );
        }
        renderer.swap_buffers();
        return true;
    }

    // Fast CPU path: upload NV12 planes directly to GL textures.
    if let rusty_codecs::format::FrameData::Nv12(ref planes) = frame.data {
        unsafe {
            renderer.render_nv12(
                &planes.y_data,
                planes.y_stride,
                &planes.uv_data,
                planes.uv_stride,
                planes.width,
                planes.height,
                surface_width,
                surface_height,
                rot,
            );
        }
        renderer.swap_buffers();
        return true;
    }

    // Slowest fallback: RGBA conversion + AHardwareBuffer for non-NV12 CPU frames.
    let img = frame.rgba_image();
    if let Some(ahwb) = create_rgba_hardware_buffer(img.as_raw(), w, h) {
        unsafe {
            renderer.render_hardware_buffer(ahwb, surface_width, surface_height, w, h, rot);
            AHardwareBuffer_release(ahwb);
        }
        renderer.swap_buffers();
        return true;
    }

    false
}

// ── AHardwareBuffer helpers ─────────────────────────────────────────

unsafe extern "C" {
    fn AHardwareBuffer_allocate(desc: *const AHwbDesc, out: *mut *mut c_void) -> i32;
    fn AHardwareBuffer_describe(buffer: *const c_void, out_desc: *mut AHwbDesc);
    fn AHardwareBuffer_lock(
        buffer: *mut c_void,
        usage: u64,
        fence: i32,
        rect: *const c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn AHardwareBuffer_unlock(buffer: *mut c_void, fence: *mut i32) -> i32;
    fn AHardwareBuffer_release(buffer: *mut c_void);
}

/// NDK `AHardwareBuffer_Desc`.
#[repr(C)]
struct AHwbDesc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}

const AHWB_FORMAT_RGBA: u32 = 1; // AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM
const AHWB_USAGE: u64 = 0x30 | 0x100; // CPU_WRITE_OFTEN | GPU_SAMPLED_IMAGE

/// Allocates an RGBA `AHardwareBuffer`, copies pixel data in, returns the pointer.
///
/// Caller owns the returned buffer and must call `AHardwareBuffer_release`.
fn create_rgba_hardware_buffer(rgba: &[u8], width: u32, height: u32) -> Option<*mut c_void> {
    unsafe {
        let desc = AHwbDesc {
            width,
            height,
            layers: 1,
            format: AHWB_FORMAT_RGBA,
            usage: AHWB_USAGE,
            stride: 0,
            rfu0: 0,
            rfu1: 0,
        };
        let mut buffer = std::ptr::null_mut();
        if AHardwareBuffer_allocate(&desc, &mut buffer) != 0 {
            tracing::warn!("AHardwareBuffer_allocate failed");
            return None;
        }

        // Query actual stride (may differ from width due to GPU alignment).
        let mut actual = std::mem::zeroed::<AHwbDesc>();
        AHardwareBuffer_describe(buffer, &mut actual);
        let stride_bytes = actual.stride * 4; // RGBA = 4 bytes/pixel
        let row_bytes = width * 4;

        let mut ptr = std::ptr::null_mut();
        if AHardwareBuffer_lock(buffer, 0x30, -1, std::ptr::null(), &mut ptr) != 0 {
            tracing::warn!("AHardwareBuffer_lock failed");
            AHardwareBuffer_release(buffer);
            return None;
        }

        if stride_bytes == row_bytes {
            std::ptr::copy_nonoverlapping(rgba.as_ptr(), ptr as *mut u8, rgba.len());
        } else {
            for y in 0..height {
                let src = rgba.as_ptr().add((y * row_bytes) as usize);
                let dst = (ptr as *mut u8).add((y * stride_bytes) as usize);
                std::ptr::copy_nonoverlapping(src, dst, row_bytes as usize);
            }
        }

        AHardwareBuffer_unlock(buffer, std::ptr::null_mut());
        Some(buffer)
    }
}

// ── JNI: camera frame push ──────────────────────────────────────────

/// Pushes an RGBA camera frame into the publish pipeline.
///
/// # Safety
/// `handle` must be a live session handle.
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
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else { return };

    let Ok(bytes) = env.convert_byte_array(data) else {
        tracing::error!("failed to read camera frame byte array");
        return;
    };
    let expected = (width * height * 4) as usize;
    if bytes.len() != expected {
        tracing::warn!(actual = bytes.len(), expected, "RGBA frame size mismatch");
        return;
    }

    let frame = VideoFrame::new_rgba(
        bytes::Bytes::from(bytes),
        width as u32,
        height as u32,
        Duration::ZERO,
    );
    if let Some(src) = guard.camera_source.as_ref() {
        if let Ok(mut src) = src.lock() {
            src.push_frame(frame);
        }
    }
}

/// Pushes NV12 camera planes into the publish pipeline.
///
/// # Safety
/// `handle` must be a live session handle from `dial`.
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
    let session = unsafe { borrow_handle(handle) };
    let Ok(mut guard) = session.lock() else {
        return;
    };

    let Ok(y_bytes) = env.convert_byte_array(y_data) else {
        tracing::error!("failed to read Y plane");
        return;
    };
    let Ok(uv_bytes) = env.convert_byte_array(uv_data) else {
        tracing::error!("failed to read UV plane");
        return;
    };

    let frame_idx = guard.cam_frames_pushed;
    guard.cam_frames_pushed += 1;

    if frame_idx == 0 {
        info!(
            width,
            height,
            y_stride,
            uv_stride,
            y_len = y_bytes.len(),
            uv_len = uv_bytes.len(),
            "first NV12 camera frame from JNI"
        );
    }

    let frame = VideoFrame::new_nv12(
        Nv12Planes {
            y_data: y_bytes,
            y_stride: y_stride as u32,
            uv_data: uv_bytes,
            uv_stride: uv_stride as u32,
            width: width as u32,
            height: height as u32,
        },
        Duration::ZERO,
    );

    if let Some(src) = guard.camera_source.clone() {
        if let Ok(mut src) = src.lock() {
            src.push_frame(frame);
        }
    }
}

// ── JNI: status and lifecycle ───────────────────────────────────────

/// Returns a compact status string for the debug overlay.
///
/// # Safety
/// `handle` must be a live session handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getStatusLine<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> jni::objects::JString<'a> {
    let empty = |env: &mut JNIEnv<'a>| env.new_string("").expect("new_string");

    if handle == 0 {
        return empty(&mut env);
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else {
        return empty(&mut env);
    };

    let elapsed = guard.created_at.elapsed().as_secs();
    let cam = guard.cam_frames_pushed;
    let dec = guard.dec_frames_rendered;

    let video_info = guard
        .video
        .as_ref()
        .map(|v| format!("dec:{} trk:{}", v.decoder_name(), v.rendition()))
        .unwrap_or_else(|| "no video".into());

    let dims = guard
        .frame_dims
        .map(|(w, h)| format!("{w}x{h}"))
        .unwrap_or_else(|| "?".into());

    let net_info = guard
        .call
        .as_ref()
        .map(|c| c.session().conn().clone())
        .or_else(|| guard.session.as_ref().map(|s| s.conn().clone()))
        .map(|conn| {
            let rtt = conn
                .paths()
                .get()
                .iter()
                .find(|p| p.is_selected())
                .and_then(|p| p.rtt())
                .unwrap_or_default();
            format!("rtt:{}ms", rtt.as_millis())
        })
        .unwrap_or_default();

    let playout_info = guard
        .remote
        .as_ref()
        .map(|b| format!("lat:{}ms", b.clock().latency().as_millis(),))
        .unwrap_or_default();

    let line = format!(
        "{video_info} {dims} | cam:{cam} dec:{dec} | {net_info} {playout_info} | {elapsed}s",
    );
    env.new_string(&line).unwrap_or_else(|_| empty(&mut env))
}

/// Disconnects and frees the session handle.
///
/// # Safety
/// `handle` must be a live session handle; must not be used after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_disconnect(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    let session = unsafe { take_handle(handle) };
    if let Ok(guard) = session.lock() {
        if let Some(ref live) = guard.live {
            runtime().block_on(live.shutdown());
        }
    }
    info!("disconnected");
}

// ── JNI: publish ────────────────────────────────────────────────────

/// Publishes camera + mic as a broadcast. Returns a session handle or 0.
///
/// # Safety
/// Must be called from JNI with valid arguments.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_publish(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    name: JString<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let Some(name_str) = read_jstring(&mut env, &name) else {
        return 0;
    };
    match runtime().block_on(publish_impl(
        name_str,
        camera_width as u32,
        camera_height as u32,
    )) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("publish failed: {e:#}");
            0
        }
    }
}

async fn publish_impl(name: String, cam_w: u32, cam_h: u32) -> Result<jlong> {
    info!(name = %name, cam_w, cam_h, "publishing broadcast");
    let live = Live::from_env().await?.with_router().with_gossip().spawn();
    info!(id = %live.endpoint().id().fmt_short(), "endpoint ready");

    let broadcast = LocalBroadcast::new();
    let camera_source = Arc::new(Mutex::new(CameraFrameSource::new(cam_w, cam_h)));
    let shared_source = SharedCameraSource {
        inner: Arc::clone(&camera_source),
    };
    broadcast
        .video()
        .set(VideoInput::new(
            shared_source,
            VideoCodec::best_available().expect("no video codec available"),
            [VideoPreset::P720],
        ))
        .context("failed to set video source")?;

    let audio_backend = AudioBackend::default();
    let mic = audio_backend
        .default_input()
        .await
        .context("failed to open microphone")?;
    broadcast
        .audio()
        .set(mic, AudioCodec::Opus, [AudioPreset::Hq])
        .context("failed to set audio source")?;

    live.publish(&name, &broadcast).await?;

    let ticket = LiveTicket::new(live.endpoint().id(), &name);
    let ticket_str = ticket.to_string();
    info!(ticket = %ticket_str, "broadcast published");

    let handle = SessionHandle {
        live: Some(live),
        session: None,
        call: None,
        remote: None,
        video: None,
        audio: None,
        audio_backend: Some(audio_backend),
        broadcast: Some(broadcast),
        ticket: Some(ticket_str),
        encoder_pipeline: None,
        renderer: Arc::new(Mutex::new(None)),
        frame_dims: None,
        camera_source: Some(camera_source),
        cam_frames_pushed: 0,
        dec_frames_rendered: 0,
        created_at: Instant::now(),
    };
    Ok(to_jlong(Arc::new(Mutex::new(handle))))
}

/// Returns the connection ticket string, or empty if not available.
///
/// # Safety
/// `handle` must be a live session handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getTicket<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> jni::objects::JString<'a> {
    let empty = |env: &mut JNIEnv<'a>| env.new_string("").expect("new_string");
    if handle == 0 {
        return empty(&mut env);
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else {
        return empty(&mut env);
    };
    let ticket = guard.ticket.as_deref().unwrap_or("");
    env.new_string(ticket).unwrap_or_else(|_| empty(&mut env))
}

// ── JNI: rendition selection ────────────────────────────────────────

/// Returns available video rendition names as a `\n`-separated string.
///
/// # Safety
/// `handle` must be a live session handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getRenditions<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> jni::objects::JString<'a> {
    let empty = |env: &mut JNIEnv<'a>| env.new_string("").expect("new_string");
    if handle == 0 {
        return empty(&mut env);
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else {
        return empty(&mut env);
    };
    let names = guard
        .remote
        .as_ref()
        .map(|r| {
            r.catalog()
                .video_renditions()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    env.new_string(&names).unwrap_or_else(|_| empty(&mut env))
}

/// Switches to a different video rendition by name.
///
/// # Safety
/// `handle` must be a live session handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_switchRendition(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    rendition_name: JString<'_>,
) {
    if handle == 0 {
        return;
    }
    let Some(name) = read_jstring(&mut env, &rendition_name) else {
        return;
    };
    let session = unsafe { borrow_handle(handle) };
    let Ok(mut guard) = session.lock() else {
        return;
    };
    let Some(remote) = guard.remote.as_ref() else {
        return;
    };
    match remote.video_rendition::<DynamicVideoDecoder>(&DecodeConfig::default(), &name) {
        Ok(track) => {
            info!(rendition = %name, "switched video rendition");
            guard.video = Some(track);
            guard.frame_dims = None;
        }
        Err(e) => {
            tracing::error!(rendition = %name, "failed to switch rendition: {e:#}");
        }
    }
}

// (EGL extension JNI wrappers removed — Rust now owns the full EGL lifecycle.)
