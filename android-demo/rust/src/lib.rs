#![cfg(target_os = "android")]
//! JNI bridge for the iroh-live Android demo app.
//!
//! Exposes a minimal set of functions to Kotlin: connect to a broadcast,
//! poll decoded video frames, publish camera frames, and disconnect.
//! A global tokio runtime drives all async work.

mod logcat;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use jni::{
    JNIEnv, JavaVM,
    objects::{JByteArray, JClass, JString},
    sys::{jint, jlong},
};
use tokio::runtime::Runtime;
use tracing::info;

use iroh_live::{Call, CallTicket, Live};
use moq_media::{
    AudioBackend,
    codec::{AudioCodec, VideoCodec},
    format::{AudioPreset, PlaybackConfig, VideoFrame, VideoPreset},
    publish::LocalBroadcast,
    subscribe::{AudioTrack, RemoteBroadcast, VideoTrack},
};
use moq_media_android::{
    camera::{CameraFrameSource, SharedCameraSource},
    handle,
};
use n0_watcher::Watcher;
use rusty_codecs::codec::DefaultDecoders;
use rusty_codecs::format::Nv12Planes;

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
    live: Live,
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

    let live = Live::from_env().await?;
    info!(broadcast = %ticket.broadcast_name, "connecting to broadcast");

    let (session, remote) = live
        .subscribe(ticket.endpoint.clone(), &ticket.broadcast_name)
        .await?;

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
        live,
        session: Some(session),
        call: None,
        remote: Some(remote),
        video,
        audio,
        audio_backend: Some(audio_backend),
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
    let ticket: CallTicket = ticket_str.parse().context("failed to parse call ticket")?;

    let live = Live::from_env().await?;
    info!(id = %live.endpoint().id().fmt_short(), "endpoint ready");

    // Camera video source.
    let broadcast = LocalBroadcast::new();
    let camera_source = Arc::new(Mutex::new(CameraFrameSource::new(cam_w, cam_h)));
    let shared_source = SharedCameraSource {
        inner: Arc::clone(&camera_source),
    };
    broadcast
        .video()
        .set(
            shared_source,
            VideoCodec::best_available(),
            [VideoPreset::P720],
        )
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
    let call = Call::dial(&live, ticket, broadcast).await?;
    info!("call connected");

    let tracks = call
        .remote()
        .media::<DefaultDecoders>(&audio_backend, PlaybackConfig::default())
        .await
        .context("failed to subscribe to remote media")?;

    info!(
        has_video = tracks.video.is_some(),
        has_audio = tracks.audio.is_some(),
        "remote media subscribed"
    );

    let handle = SessionHandle {
        live,
        session: None,
        call: Some(call),
        remote: Some(tracks.broadcast),
        video: tracks.video,
        audio: tracks.audio,
        audio_backend: Some(audio_backend),
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
    let frame = video.and_then(|v| v.current_frame());
    let Some(frame) = frame else { return 0 };

    guard.dec_frames_rendered += 1;
    let (w, h) = (frame.width(), frame.height());
    if guard.frame_dims != Some((w, h)) {
        info!(width = w, height = h, "decoded frame dimensions updated");
        guard.frame_dims = Some((w, h));
    }

    let native = match frame.native_handle() {
        Some(rusty_codecs::format::NativeFrameHandle::HardwareBuffer(info)) => info,
        _ => return 0,
    };

    let raw_ptr = native.buffer.as_ptr();
    std::mem::forget(native.buffer); // Kotlin owns it now.
    raw_ptr as jlong
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
    unsafe extern "C" {
        fn AHardwareBuffer_release(buffer: *mut std::ffi::c_void);
    }
    // SAFETY: buffer_ptr is a valid AHardwareBuffer* with an acquired reference.
    unsafe { AHardwareBuffer_release(buffer_ptr as *mut std::ffi::c_void) }
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
        .map(|b| {
            format!(
                "buf:{}ms jit:{:.0}ms",
                b.clock().buffer().as_millis(),
                b.clock().jitter().as_secs_f64() * 1000.0,
            )
        })
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
        runtime().block_on(guard.live.shutdown());
    }
    info!("disconnected");
}

// ── EGL extension JNI wrappers ──────────────────────────────────────
//
// Thin wrappers around moq_media_android::egl. The JNI export names are
// tied to this demo's class names; the EGL logic is in the library crate.

/// `eglGetNativeClientBufferANDROID`: AHardwareBuffer → EGLClientBuffer.
///
/// # Safety
/// `hardware_buffer_ptr` must be a valid `AHardwareBuffer*`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_MainActivity_eglGetNativeClientBufferANDROID(
    _env: JNIEnv<'_>,
    _this: jni::objects::JObject<'_>,
    hardware_buffer_ptr: jlong,
) -> jlong {
    unsafe {
        moq_media_android::egl::get_native_client_buffer(
            hardware_buffer_ptr as *const std::ffi::c_void,
        )
    }
    .map_or(0, |p| p as jlong)
}

/// `eglCreateImageKHR`: EGLClientBuffer → EGLImage.
///
/// # Safety
/// `client_buffer` and `display` must be valid EGL handles.
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
    let egl_display = get_native_handle(&mut env, &display);
    // SAFETY: attrs is a valid JIntArray from the JVM.
    let attr_vec = unsafe {
        env.get_array_elements(&attrs, jni::objects::ReleaseMode::NoCopyBack)
            .ok()
    };
    let attr_ptr = attr_vec
        .as_ref()
        .map(|a: &jni::objects::AutoElements<'_, '_, '_, jni::sys::jint>| a.as_ptr())
        .unwrap_or(std::ptr::null_mut());

    unsafe {
        moq_media_android::egl::create_image(
            egl_display as *mut std::ffi::c_void,
            target as u32,
            client_buffer as *mut std::ffi::c_void,
            attr_ptr,
        )
    }
    .map_or(0, |p| p as jlong)
}

/// `eglDestroyImageKHR`: releases an EGLImage.
///
/// # Safety
/// `image` and `display` must be valid EGL handles.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_MainActivity_eglDestroyImageKHR(
    mut env: JNIEnv<'_>,
    _this: jni::objects::JObject<'_>,
    display: jni::objects::JObject<'_>,
    image: jlong,
) {
    let egl_display = get_native_handle(&mut env, &display);
    unsafe {
        moq_media_android::egl::destroy_image(
            egl_display as *mut std::ffi::c_void,
            image as *mut std::ffi::c_void,
        );
    }
}

/// `glEGLImageTargetTexture2DOES`: binds EGLImage to GL texture.
///
/// # Safety
/// `image` must be valid; a GL context must be current.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_MainActivity_glEGLImageTargetTexture2DOES(
    _env: JNIEnv<'_>,
    _this: jni::objects::JObject<'_>,
    target: jint,
    image: jlong,
) {
    unsafe {
        moq_media_android::egl::image_target_texture_2d(
            target as u32,
            image as *mut std::ffi::c_void,
        );
    }
}

/// Extracts the native EGL handle from a Java EGL14 wrapper object.
fn get_native_handle(env: &mut JNIEnv<'_>, obj: &jni::objects::JObject<'_>) -> jlong {
    env.call_method(obj, "getNativeHandle", "()J", &[])
        .and_then(|v| v.j())
        .or_else(|_| {
            tracing::warn!("getNativeHandle() unavailable, trying mEGLDisplay field");
            env.get_field(obj, "mEGLDisplay", "J").and_then(|v| v.j())
        })
        .unwrap_or(0)
}
