#![cfg(target_os = "android")]
//! JNI bridge for the iroh-live Android demo app.
//!
//! Kotlin can watch or publish a broadcast, make or answer a call, push camera
//! frames and draw the video. Two offline pipelines run the media stack with no
//! network, to smoke-test MediaCodec on a device.
//!
//! A global tokio runtime drives all async work. Each session lives in a
//! registry under a `jlong` id that Kotlin passes back in.

mod egl;
mod logcat;
mod renderer;

use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{
        Arc, LazyLock, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use iroh_live::{
    BoundNode, BroadcastTicket, EndpointConfig,
    calls::{CALL, Call, CallEvent, Calls},
    layout::LiveLayout,
    media::{
        AudioEncoding, AudioSource, AudioTarget, Catalog, FrameSender, LocalBroadcast,
        MicrophoneConfig, Player, PlayerConfig, RemoteBroadcast, RenditionMode, VideoEncoding,
        VideoFormat, VideoFrames, VideoRendition, VideoSource, remote_broadcast,
    },
    net::Timestamp,
};
use jni::{
    JNIEnv, JavaVM,
    objects::{JByteArray, JClass, JObject, JString},
    sys::{jboolean, jint, jlong},
};
use moq_video::{Frame, FrameRate, I420, Size, Surface};
use n0_error::{Result, StackResultExt, StdResultExt, anyerr};
use n0_future::{StreamExt as _, task::AbortOnDropHandle};
use tokio::runtime::Runtime;
use tracing::{error, info, warn};

use crate::renderer::AndroidRenderer;

/// Log targets and levels the demo wants in logcat.
const LOGCAT_FILTER: &str = "\
    warn,\
    iroh=debug,\
    iroh_live=debug,\
    iroh_live_android=debug,\
    iroh_live_node=debug,\
    moq_ext_player=debug,\
    moq_ext_publisher=debug,\
    moq_video=debug,\
    moq_audio=debug,\
    moq_net=debug,\
    hang=debug,\
    cpal=debug,\
    oboe=debug";

/// The broadcast name the local encode and decode loop reports in its logs.
const LOOPBACK_NAME: &str = "loopback";

/// The frame rate the camera source declares.
///
/// CameraX delivers at the rate the device picks. This is the most the encoder
/// plans for.
const CAMERA_FPS: u32 = 30;

/// The track name of the one video rendition the demo publishes.
const VIDEO_RENDITION: &str = "video";

/// Initializes ndk-context and tracing when `System.loadLibrary` loads this library.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut c_void) -> jint {
    // SAFETY: `vm` stays valid for the life of the process. The activity is
    // null because cpal's Oboe backend only needs the VM.
    unsafe {
        ndk_context::initialize_android_context(
            vm.get_java_vm_pointer().cast(),
            std::ptr::null_mut(),
        );
    }
    let _ = logcat::init(LOGCAT_FILTER);
    jni::sys::JNI_VERSION_1_6
}

// Global runtime

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

// Session handle

/// How long a dial waits for the peer to answer.
const PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// Binds the endpoint for a screen, with the key from `IROH_SECRET` if set.
///
/// Returns the node with the router that accepts its peers. The demo stores
/// no key, so without `IROH_SECRET` every screen gets a new endpoint id.
async fn bind_live() -> Result<BoundNode> {
    Ok(BoundNode::bind(EndpointConfig::from_env()?, LiveLayout::default().config()).await?)
}

/// Plays the side of the peer that `call` reached.
fn play_call(call: &Call, output: &AudioTarget) -> Result<Player> {
    play(&remote_broadcast(call.subscription()), output)
}

/// The state behind the `jlong` id Kotlin holds.
///
/// It is shared behind a mutex because the render, camera and UI threads call
/// in concurrently.
struct SessionHandle {
    /// The node, its endpoint and the router that accepts its peers.
    ///
    /// `None` for the offline pipelines. Shutting it down closes the endpoint.
    live: Option<BoundNode>,
    /// The call, once it is up. Dropping it hangs up.
    call: Option<Call>,
    /// Playback of the watched broadcast. Dropping it stops decoding and audio.
    player: Option<Player>,
    /// The speaker, which is also the microphone's echo reference.
    ///
    /// `None` in the offline pipelines and in a publish-only session.
    #[allow(dead_code, reason = "dropping it closes the speaker")]
    output: Option<AudioTarget>,
    /// The broadcast this node publishes, for a subscriber or a call.
    broadcast: Option<LocalBroadcast>,
    /// Where the camera frames Kotlin pushes go.
    camera: Option<FrameSender<Frame>>,
    /// What the render loop draws.
    ///
    /// The player's decoded frames, or the local camera frames on their way to
    /// the encoder.
    frames: Option<VideoFrames>,
    /// The ticket a published broadcast is reachable by.
    ticket: Option<String>,
    /// The GLES renderer.
    ///
    /// Behind its own lock, so drawing does not block camera pushes.
    renderer: Arc<Mutex<Option<AndroidRenderer>>>,
    /// The size of the last frame drawn.
    frame_dims: Option<Size>,
    /// The task waiting for a caller, for a handle from `answer`.
    ///
    /// Dropping the handle aborts it, so the wait ends with its screen.
    waiting: Option<AbortOnDropHandle<()>>,
    /// Set by `disconnect`, so a call answered during teardown is not installed.
    closing: bool,
    cam_frames_pushed: u64,
    dec_frames_rendered: u64,
    created_at: Instant,
}

type SharedHandle = Arc<Mutex<SessionHandle>>;

impl SessionHandle {
    /// Creates an empty handle.
    fn new() -> Self {
        Self {
            live: None,
            call: None,
            player: None,
            output: None,
            broadcast: None,
            camera: None,
            frames: None,
            ticket: None,
            renderer: Arc::new(Mutex::new(None)),
            frame_dims: None,
            waiting: None,
            closing: false,
            cam_frames_pushed: 0,
            dec_frames_rendered: 0,
            created_at: Instant::now(),
        }
    }

    fn into_shared(self) -> SharedHandle {
        Arc::new(Mutex::new(self))
    }

    /// Returns the timestamp for the next camera frame, counted from session start.
    ///
    /// The broadcast rebases each source onto its media clock at the first
    /// frame, so the camera does not have to share a clock with the microphone.
    fn timestamp(&self) -> Timestamp {
        let micros = u64::try_from(self.created_at.elapsed().as_micros()).unwrap_or(u64::MAX);
        Timestamp::from_micros(micros).unwrap_or(Timestamp::ZERO)
    }

    /// Returns the rendition the player is drawing.
    fn rendition(&self) -> Option<String> {
        self.player.as_ref()?.status().borrow().rendition.clone()
    }

    /// Returns the newest catalog of the watched broadcast, if one arrived.
    fn catalog(&self) -> Option<std::sync::Arc<Catalog>> {
        self.player.as_ref()?.broadcast().catalog().borrow().clone()
    }
}

/// Sessions by the id Kotlin holds as a `jlong`.
///
/// A raw pointer in the `jlong` would leave a call that races `disconnect`
/// with freed memory. An id is never reused, so a late call finds nothing.
static SESSIONS: LazyLock<Mutex<HashMap<u64, SharedHandle>>> = LazyLock::new(Default::default);

/// The id of the next session. Kotlin reads zero as no session.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// Registers `session`, and returns the id Kotlin holds for it.
fn register(session: SharedHandle) -> jlong {
    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    SESSIONS.lock().expect("poisoned").insert(id, session);
    jlong::try_from(id).expect("fewer than 2^63 sessions")
}

/// Returns the session with id `handle`, or `None` once it is disconnected.
fn lookup(handle: jlong) -> Option<SharedHandle> {
    let id = u64::try_from(handle).ok()?;
    SESSIONS.lock().expect("poisoned").get(&id).cloned()
}

/// Takes the session with id `handle` out of the registry.
///
/// Calls already running keep their reference until they return.
fn unregister(handle: jlong) -> Option<SharedHandle> {
    let id = u64::try_from(handle).ok()?;
    SESSIONS.lock().expect("poisoned").remove(&id)
}

/// Reads a JNI string, returning `None` on failure.
fn read_jstring(env: &mut JNIEnv<'_>, s: &JString<'_>) -> Option<String> {
    match env.get_string(s) {
        Ok(s) => Some(s.into()),
        Err(err) => {
            error!(error = %err, "failed to read JNI string");
            None
        }
    }
}

// Capture wiring

/// Sets the broadcast's video to a camera that Kotlin pushes frames into.
///
/// Returns the sink for those frames and a stream of the same frames on their
/// way to the encoder. A local preview draws that stream without any encode or
/// decode.
fn set_camera(broadcast: &LocalBroadcast, size: Size) -> Result<(FrameSender<Frame>, VideoFrames)> {
    let rate = FrameRate::new(CAMERA_FPS, 1).std_context("camera frame rate")?;
    let (sink, source) = VideoSource::push(VideoFormat { size, rate });
    let preview = source.frames();
    broadcast.set_video(
        source,
        VideoEncoding::single(VideoRendition::new(VIDEO_RENDITION)),
    )?;
    Ok((sink, preview))
}

/// Publishes the default microphone, cancelling the echo of `echo` if given.
///
/// A call passes its speaker here, or a phone on speaker sends the peer's
/// voice straight back. If the microphone does not open, the session goes on
/// with video only.
async fn set_microphone(broadcast: &LocalBroadcast, echo: Option<&AudioTarget>) {
    let config = MicrophoneConfig {
        echo_reference: echo.map(|output| std::sync::Arc::new(output.clone()) as _),
        ..MicrophoneConfig::default()
    };
    let published = match AudioSource::microphone(config).await {
        Ok(source) => broadcast.set_audio(source, AudioEncoding::voice()),
        Err(err) => Err(err.into()),
    };
    if let Err(err) = published {
        warn!(error = format_args!("{err:#}"), "publishing without audio");
    }
}

/// Starts playing `remote`, with its audio through `output`.
fn play(remote: &RemoteBroadcast, output: &AudioTarget) -> Result<Player> {
    Ok(remote.play(PlayerConfig {
        audio: Some(output.clone()),
        ..PlayerConfig::default()
    })?)
}

// JNI: connect (subscribe only)

/// Connects to a remote broadcast. Returns a session handle, or 0 on failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_connect(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    ticket: JString<'_>,
) -> jlong {
    let Some(ticket) = read_jstring(&mut env, &ticket) else {
        return 0;
    };
    match runtime().block_on(connect_impl(ticket)) {
        Ok(handle) => handle,
        Err(err) => {
            error!(error = format_args!("{err:#}"), "connect failed");
            0
        }
    }
}

async fn connect_impl(ticket: String) -> Result<jlong> {
    let ticket: BroadcastTicket = ticket.parse().context("failed to parse ticket")?;

    let live = bind_live().await?;
    info!(broadcast = %ticket.path(), "connecting to broadcast");

    let subscription = live.node().subscribe_ticket(&ticket).await?;
    info!("subscribed");

    // The player waits for the catalog on its own, so the handle is usable
    // right away.
    let output = AudioTarget::default_or_null().await;
    let remote = remote_broadcast(&subscription);
    let player = play(&remote, &output)?;

    let mut session = SessionHandle::new();
    session.frames = Some(player.video_frames());
    session.player = Some(player);
    session.output = Some(output);
    session.live = Some(live);
    Ok(register(session.into_shared()))
}

// JNI: dial (two-way call)

/// Dials a remote peer: publishes camera and microphone, subscribes to theirs.
///
/// Returns a session handle, or 0 on failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_dial(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    ticket: JString<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let Some(ticket) = read_jstring(&mut env, &ticket) else {
        return 0;
    };
    let size = Size::new(camera_width as u32, camera_height as u32);
    match runtime().block_on(dial_impl(ticket, size)) {
        Ok(handle) => handle,
        Err(err) => {
            error!(error = format_args!("{err:#}"), "dial failed");
            0
        }
    }
}

async fn dial_impl(ticket: String, size: Size) -> Result<jlong> {
    info!(%ticket, %size, "parsing call ticket");
    let ticket: BroadcastTicket = ticket.parse().context("failed to parse call ticket")?;

    let live = bind_live().await?;
    info!(id = %live.node().endpoint().id().fmt_short(), "endpoint ready");

    let output = AudioTarget::default_or_null().await;
    // Published to nobody, then offered to the peer.
    let (calls, broadcast): (Calls, LocalBroadcast) =
        Calls::new(live.node(), &LiveLayout::default())?;
    let (camera, _preview) = set_camera(&broadcast, size)?;
    set_microphone(&broadcast, Some(&output)).await;

    // A busy callee answers only once its call ends, so give up as `irl call`
    // does.
    let call = tokio::time::timeout(PEER_TIMEOUT, calls.call(ticket.peer()))
        .await
        .std_context("the peer did not answer")??;
    let player = play_call(&call, &output)?;
    info!(peer = %ticket.peer().fmt_short(), "call connected");

    let mut session = SessionHandle::new();
    session.frames = Some(player.video_frames());
    session.player = Some(player);
    session.output = Some(output);
    session.camera = Some(camera);
    session.broadcast = Some(broadcast);
    session.call = Some(call);
    session.live = Some(live);
    Ok(register(session.into_shared()))
}

/// Opens this node's side of a call and waits for a peer to call.
///
/// Returns a session handle at once, so the screen can show the ticket. A
/// task on the handle waits for the caller, and
/// [`Java_com_n0_irohlive_demo_IrohBridge_callConnected`] reports when one
/// arrived. Returns 0 on failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_answer(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let size = Size::new(camera_width as u32, camera_height as u32);
    match runtime().block_on(answer_impl(size)) {
        Ok(handle) => handle,
        Err(err) => {
            error!(error = format_args!("{err:#}"), "answer failed");
            0
        }
    }
}

async fn answer_impl(size: Size) -> Result<jlong> {
    info!(%size, "waiting for a call");
    let live = bind_live().await?;
    let id = live.node().endpoint().id();
    info!(id = %id.fmt_short(), "endpoint ready");

    // Start the camera now, so the preview runs while the ticket is on screen
    // and the peer's first frame does not wait for the device to open.
    let output = AudioTarget::default_or_null().await;
    let (calls, broadcast): (Calls, LocalBroadcast) =
        Calls::new(live.node(), &LiveLayout::default())?;
    let (camera, preview) = set_camera(&broadcast, size)?;
    set_microphone(&broadcast, Some(&output)).await;

    let mut session = SessionHandle::new();
    session.frames = Some(preview);
    session.output = Some(output.clone());
    session.camera = Some(camera);
    session.ticket = Some(
        LiveLayout::default()
            .ticket(live.node().id(), CALL)?
            .to_string(),
    );
    session.broadcast = Some(broadcast);
    session.live = Some(live);
    let shared = session.into_shared();

    // The task holds a `Weak`. A strong reference would keep the handle, and
    // with it the endpoint and camera, alive until somebody calls. The handle
    // owns the task through `waiting`, so dropping the last `Arc` aborts it.
    let waiting = Arc::downgrade(&shared);
    let task = runtime().spawn(async move {
        if let Err(err) = accept_one(calls, output, waiting).await {
            error!(error = format_args!("{err:#}"), "answering failed");
        }
    });
    shared.lock().expect("poisoned").waiting = Some(AbortOnDropHandle::new(task));

    Ok(register(shared))
}

/// Answers the first peer that calls and installs a player for it on `session`.
///
/// A caller offers its side to this node only, which `calls` reports as a
/// ring.
async fn accept_one(
    calls: Calls,
    output: AudioTarget,
    session: Weak<Mutex<SessionHandle>>,
) -> Result<()> {
    let mut events = calls.events();
    let peer = loop {
        match events.next().await {
            Some(CallEvent::Ringing(peer)) => break peer,
            Some(_) => {}
            None => return Ok(()),
        }
    };
    info!(peer = %peer.fmt_short(), "answering");
    let call = calls.answer(peer).await?;
    let player = play_call(&call, &output)?;
    info!(peer = %peer.fmt_short(), "call answered");

    let Some(session) = session.upgrade() else {
        return Ok(());
    };
    let mut held = session.lock().expect("poisoned");
    if held.closing {
        return Ok(());
    }
    // Replace the local preview with the peer's video.
    held.frames = Some(player.video_frames());
    held.player = Some(player);
    held.call = Some(call);
    Ok(())
}

/// Returns whether an answered call has a peer yet.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_callConnected(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jboolean {
    let Some(session) = lookup(handle) else {
        return 0;
    };
    let connected = session.lock().expect("poisoned").call.is_some();
    jboolean::from(connected)
}

// JNI: publish

/// Publishes camera and microphone under `name`.
///
/// Returns a session handle, or 0 on failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_publish(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    name: JString<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let Some(name) = read_jstring(&mut env, &name) else {
        return 0;
    };
    let size = Size::new(camera_width as u32, camera_height as u32);
    match runtime().block_on(publish_impl(name, size)) {
        Ok(handle) => handle,
        Err(err) => {
            error!(error = format_args!("{err:#}"), "publish failed");
            0
        }
    }
}

async fn publish_impl(name: String, size: Size) -> Result<jlong> {
    info!(%name, %size, "publishing broadcast");
    let live = bind_live().await?;
    info!(id = %live.node().endpoint().id().fmt_short(), "endpoint ready");

    let broadcast: LocalBroadcast = LiveLayout::default().publish(live.node(), &name)?;
    let (camera, preview) = set_camera(&broadcast, size)?;
    // A publish-only session plays no sound, so there is no echo to cancel.
    set_microphone(&broadcast, None).await;

    let ticket = LiveLayout::default()
        .ticket(live.node().id(), &name)?
        .to_string();
    info!(%ticket, "broadcast published");

    let mut session = SessionHandle::new();
    session.frames = Some(preview);
    session.camera = Some(camera);
    session.ticket = Some(ticket);
    session.broadcast = Some(broadcast);
    session.live = Some(live);
    Ok(register(session.into_shared()))
}

/// Returns the ticket a published broadcast is reachable by, or an empty string.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getTicket<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> JString<'a> {
    let Some(session) = lookup(handle) else {
        return empty_string(&mut env);
    };
    let guard = session.lock().expect("poisoned");
    let ticket = guard.ticket.clone().unwrap_or_default();
    new_string(&mut env, &ticket)
}

// JNI: offline pipelines

/// Starts a camera preview with no encode, decode or network.
///
/// Nobody subscribes to the broadcast, so its encoders stay idle. Returns a
/// session handle, or 0 on failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_startDirect(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let size = Size::new(camera_width as u32, camera_height as u32);
    match start_direct_impl(size) {
        Ok(handle) => handle,
        Err(err) => {
            error!(error = format_args!("{err:#}"), "startDirect failed");
            0
        }
    }
}

fn start_direct_impl(size: Size) -> Result<jlong> {
    info!(%size, "starting direct camera pipeline");
    // The broadcast spawns its encode task on tokio.
    let _guard = runtime().enter();

    let broadcast = LocalBroadcast::new();
    let (camera, preview) = set_camera(&broadcast, size)?;

    let mut session = SessionHandle::new();
    session.frames = Some(preview);
    session.camera = Some(camera);
    session.broadcast = Some(broadcast);
    Ok(register(session.into_shared()))
}

/// Starts a local loop from the camera through MediaCodec and back.
///
/// Uses no network. Returns a session handle, or 0 on failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_startH264(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    camera_width: jint,
    camera_height: jint,
) -> jlong {
    let size = Size::new(camera_width as u32, camera_height as u32);
    match start_h264_impl(size) {
        Ok(handle) => handle,
        Err(err) => {
            error!(error = format_args!("{err:#}"), "startH264 failed");
            0
        }
    }
}

fn start_h264_impl(size: Size) -> Result<jlong> {
    info!(%size, "starting local H264 pipeline");
    let _guard = runtime().enter();

    let broadcast = LocalBroadcast::new();
    let (camera, _preview) = set_camera(&broadcast, size)?;
    let player = open_loopback(&broadcast)?;

    let mut session = SessionHandle::new();
    session.frames = Some(player.video_frames());
    session.player = Some(player);
    session.camera = Some(camera);
    session.broadcast = Some(broadcast);
    Ok(register(session.into_shared()))
}

/// Plays a local broadcast in-process.
///
/// The catalog appears only after the first camera frame. The player waits for
/// it on its own.
fn open_loopback(broadcast: &LocalBroadcast) -> Result<Player> {
    let player =
        RemoteBroadcast::from_consumer(broadcast.consume()).play(PlayerConfig::default())?;
    info!(broadcast = LOOPBACK_NAME, "loopback playing");
    Ok(player)
}

// JNI: camera frame push

/// What a camera push needs from the handle, copied out of the lock.
///
/// Converting a camera buffer copies the whole picture, and the render loop
/// needs the lock back sooner.
struct CameraTarget {
    sink: FrameSender<Frame>,
    timestamp: Timestamp,
    /// How many frames were pushed before this one.
    pushed: u64,
}

/// Reads the camera target out of the handle.
fn camera_target(session: &SharedHandle) -> Option<CameraTarget> {
    let guard = session.lock().expect("poisoned");
    Some(CameraTarget {
        sink: guard.camera.clone()?,
        timestamp: guard.timestamp(),
        pushed: guard.cam_frames_pushed,
    })
}

/// Counts a frame that reached the publish pipeline.
fn count_camera_frame(session: &SharedHandle) {
    session.lock().expect("poisoned").cam_frames_pushed += 1;
}

/// Pushes one tightly packed RGBA camera frame.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_pushCameraFrame(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    data: JByteArray<'_>,
    width: jint,
    height: jint,
) {
    let Some(session) = lookup(handle) else {
        return;
    };
    let Ok(rgba) = env.convert_byte_array(data) else {
        error!("failed to read camera frame byte array");
        return;
    };

    let Some(target) = camera_target(&session) else {
        return;
    };
    let size = Size::new(width as u32, height as u32);
    let surface = match Surface::rgba(&rgba, size) {
        Ok(surface) => surface,
        Err(err) => {
            warn!(%size, error = %err, "rejected RGBA camera frame");
            return;
        }
    };
    if target
        .sink
        .push(Frame::new(surface, target.timestamp))
        .is_err()
    {
        return;
    }
    count_camera_frame(&session);
}

/// Pushes one camera frame as the NV12 planes CameraX hands out.
///
/// `y_stride` and `uv_stride` are the driver's row pitches, often wider than
/// the picture.
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
    let Some(session) = lookup(handle) else {
        return;
    };
    let (Ok(y), Ok(uv)) = (
        env.convert_byte_array(y_data),
        env.convert_byte_array(uv_data),
    ) else {
        error!("failed to read camera planes");
        return;
    };

    let Some(target) = camera_target(&session) else {
        return;
    };

    let size = Size::new(width as u32, height as u32);
    if target.pushed == 0 {
        info!(
            %size,
            y_stride,
            uv_stride,
            y_len = y.len(),
            uv_len = uv.len(),
            "first NV12 camera frame from JNI"
        );
    }

    let planes = match nv12_to_i420(&y, y_stride as usize, &uv, uv_stride as usize, size) {
        Ok(planes) => planes,
        Err(err) => {
            warn!(%size, error = format_args!("{err:#}"), "rejected NV12 camera frame");
            return;
        }
    };
    let frame = Frame::new(Surface::I420(planes), target.timestamp);
    if target.sink.push(frame).is_err() {
        return;
    }
    count_camera_frame(&session);
}

/// Converts CameraX's NV12 planes into packed I420.
///
/// Camera NV12 rows carry padding and interleaved chroma, and `I420` is packed
/// and planar, so a copy is needed.
fn nv12_to_i420(
    y: &[u8],
    y_stride: usize,
    uv: &[u8],
    uv_stride: usize,
    size: Size,
) -> Result<I420> {
    let (width, height) = (size.width as usize, size.height as usize);
    let (chroma_width, chroma_height) = (width / 2, height / 2);
    if y_stride < width || uv_stride < chroma_width * 2 {
        return Err(anyerr!(
            "strides {y_stride}/{uv_stride} are narrower than {size}"
        ));
    }
    if y.len() < y_stride * height || uv.len() < uv_stride * chroma_height {
        return Err(anyerr!(
            "planes are {}/{} bytes, too short for {size} at strides {y_stride}/{uv_stride}",
            y.len(),
            uv.len()
        ));
    }

    let mut data = vec![0u8; I420::len(size).std_context("sizing I420 planes")?];
    let (luma, chroma) = data.split_at_mut(width * height);
    let (u_plane, v_plane) = chroma.split_at_mut(chroma_width * chroma_height);

    for (row, dst) in luma.chunks_exact_mut(width).enumerate() {
        let src = row * y_stride;
        dst.copy_from_slice(&y[src..src + width]);
    }
    for row in 0..chroma_height {
        let src = &uv[row * uv_stride..row * uv_stride + chroma_width * 2];
        let u_row = &mut u_plane[row * chroma_width..(row + 1) * chroma_width];
        let v_row = &mut v_plane[row * chroma_width..(row + 1) * chroma_width];
        for (col, [u, v]) in src.as_chunks::<2>().0.iter().enumerate() {
            u_row[col] = *u;
            v_row[col] = *v;
        }
    }

    I420::new(size, data).std_context("packing I420 planes")
}

// JNI: rendering

/// Creates the EGL context and GL renderer for an Android surface.
///
/// Call it from the render thread. Kotlin only hands over the
/// `android.view.Surface`, and Rust manages EGL.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_initSurface(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    surface: JObject<'_>,
) {
    let Some(session) = lookup(handle) else {
        return;
    };
    // SAFETY: JNI just passed a valid JNIEnv and a live `android.view.Surface`.
    let native_window = unsafe {
        moq_video::ndk::native_window::NativeWindow::from_surface(env.get_raw(), surface.as_raw())
    };
    let Some(native_window) = native_window else {
        error!("ANativeWindow_fromSurface returned null");
        return;
    };

    let guard = session.lock().expect("poisoned");
    // SAFETY: the window comes from a live surface and outlives this call. The
    // renderer takes its own reference.
    match unsafe { AndroidRenderer::new(native_window.ptr().as_ptr().cast()) } {
        Ok(renderer) => *guard.renderer.lock().expect("poisoned") = Some(renderer),
        Err(err) => error!(error = format_args!("{err:#}"), "initSurface failed"),
    }
}

/// Tears down the EGL surface and context when the render loop exits.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_teardownSurface(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    let Some(session) = lookup(handle) else {
        return;
    };
    let guard = session.lock().expect("poisoned");
    let mut renderer = guard.renderer.lock().expect("poisoned");
    if let Some(renderer) = renderer.as_ref() {
        renderer.teardown();
    }
    *renderer = None;
}

/// Draws the newest frame and swaps the EGL buffers.
///
/// Returns whether it drew a frame. Call it from the render thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_renderNextFrame(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    surface_width: jint,
    surface_height: jint,
    rotation_degrees: jint,
) -> bool {
    let Some(session) = lookup(handle) else {
        return false;
    };

    // Hold the session lock only to take the frame. Drawing can be slow, and a
    // camera push that waits on the lock drops a frame.
    let (frame, renderer) = {
        let mut guard = session.lock().expect("poisoned");
        let Some(frame) = guard.frames.as_mut().and_then(VideoFrames::try_next) else {
            return false;
        };
        guard.dec_frames_rendered += 1;
        let size = frame.size();
        if guard.frame_dims != Some(size) {
            info!(%size, "frame dimensions updated");
            guard.frame_dims = Some(size);
        }
        (frame, Arc::clone(&guard.renderer))
    };

    let renderer = renderer.lock().expect("poisoned");
    let Some(renderer) = renderer.as_ref() else {
        return false;
    };
    // The coroutine dispatcher can move the render loop between threads, so
    // rebind the context before every draw.
    renderer.make_current();
    draw(
        renderer,
        &frame,
        surface_width,
        surface_height,
        rotation_degrees as u32,
    )
}

/// Draws one frame, taking the cheapest path its surface allows.
fn draw(
    renderer: &AndroidRenderer,
    frame: &Frame,
    surface_width: jint,
    surface_height: jint,
    rotation: u32,
) -> bool {
    let size = frame.size();
    match &frame.surface {
        // MediaCodec decodes into an ImageReader, so the picture reaches GL
        // without a CPU copy.
        Surface::HardwareBuffer(surface) => {
            let buffer = match surface.buffer() {
                Ok(buffer) => buffer,
                Err(err) => {
                    warn!(error = %err, "decoded frame has no hardware buffer");
                    return false;
                }
            };
            // SAFETY: the EGL context is current, and `buffer` holds its own
            // reference for the whole call.
            unsafe {
                renderer.render_hardware_buffer(
                    buffer.as_ptr().cast::<c_void>(),
                    surface_width,
                    surface_height,
                    size.width,
                    size.height,
                    rotation,
                );
            }
        }
        // Software decode and the camera preview land here. Interleaving the
        // chroma for the NV12 shader is much cheaper than an RGBA conversion on
        // the CPU.
        Surface::I420(planes) => {
            let chroma = interleave_chroma(planes);
            // SAFETY: the EGL context is current. Both planes are tightly
            // packed, so their strides equal the widths passed in.
            unsafe {
                renderer.render_nv12(
                    planes.y(),
                    size.width,
                    &chroma,
                    size.width,
                    size.width,
                    size.height,
                    surface_width,
                    surface_height,
                    rotation,
                );
            }
        }
        // On Android a surface is one of the two above. The enum is
        // non-exhaustive, so a new variant drops frames here.
        _ => {
            warn!("no render path for this surface");
            return false;
        }
    }
    renderer.swap_buffers();
    true
}

/// Interleaves I420's separate chroma planes into NV12's single one.
fn interleave_chroma(planes: &I420) -> Vec<u8> {
    let (u, v) = (planes.u(), planes.v());
    let mut chroma = Vec::with_capacity(u.len() * 2);
    for (u, v) in u.iter().zip(v) {
        chroma.push(*u);
        chroma.push(*v);
    }
    chroma
}

// JNI: status

/// Returns `(width << 32) | height` for the video being drawn, or 0.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getVideoDimensions(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jlong {
    let Some(session) = lookup(handle) else {
        return 0;
    };
    let guard = session.lock().expect("poisoned");

    // Prefer the decoded size. The catalog only has the encoded size.
    if let Some(size) = guard.frame_dims {
        return (i64::from(size.width) << 32) | i64::from(size.height);
    }
    let (Some(rendition), Some(catalog)) = (guard.rendition(), guard.catalog()) else {
        return 0;
    };
    catalog
        .video
        .renditions
        .get(&rendition)
        .and_then(|config| config.coded_width.zip(config.coded_height))
        .map_or(0, |(width, height)| {
            (i64::from(width) << 32) | i64::from(height)
        })
}

/// Returns the available video renditions, one per line.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getRenditions<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> JString<'a> {
    let Some(session) = lookup(handle) else {
        return empty_string(&mut env);
    };
    let guard = session.lock().expect("poisoned");
    let names = guard
        .catalog()
        .map(|catalog| {
            catalog
                .video
                .ranked()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    new_string(&mut env, &names)
}

/// Pins the player to a named rendition.
///
/// The new decoder runs next to the old one until it catches up, so the picture
/// does not go blank.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_switchRendition(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    rendition_name: JString<'_>,
) {
    let Some(session) = lookup(handle) else {
        return;
    };
    let Some(name) = read_jstring(&mut env, &rendition_name) else {
        return;
    };
    let guard = session.lock().expect("poisoned");
    let Some(player) = guard.player.as_ref() else {
        warn!(%name, "not playing a broadcast, nothing to switch");
        return;
    };
    info!(%name, "switching video rendition");
    player.set_rendition(RenditionMode::pinned(name));
}

/// Returns a compact status line for the debug overlay.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getStatusLine<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> JString<'a> {
    let Some(session) = lookup(handle) else {
        return empty_string(&mut env);
    };
    let guard = session.lock().expect("poisoned");
    let line = status_line(&guard);
    new_string(&mut env, &line)
}

/// Formats the debug overlay line.
fn status_line(session: &SessionHandle) -> String {
    let playing = session
        .player
        .as_ref()
        .map_or_else(|| "no track".to_string(), |player| player.stats().summary());
    let cam = session.cam_frames_pushed;
    let dec = session.dec_frames_rendered;
    let elapsed = session.created_at.elapsed().as_secs();

    format!("{playing} | cam:{cam} dec:{dec} | {elapsed}s")
}

// JNI: teardown

/// Disconnects and frees the session handle, which must not be used after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_disconnect(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    let Some(session) = unregister(handle) else {
        return;
    };
    // Release the lock before `block_on`. Holding it would stall every other
    // JNI call on this handle until the endpoint closes.
    let (waiting, player, call, broadcast, live) = {
        let mut guard = session.lock().expect("poisoned");
        guard.closing = true;
        (
            guard.waiting.take(),
            guard.player.take(),
            guard.call.take(),
            guard.broadcast.take(),
            guard.live.take(),
        )
    };
    runtime().block_on(async move {
        // Stop waiting for a caller first, so none is accepted during shutdown.
        drop(waiting);
        drop(player);
        drop(call);
        if let Some(broadcast) = broadcast {
            // Let the encoders finish, so subscribers see the broadcast end.
            broadcast.close();
            broadcast.closed().await;
        }
        // Closes the sessions, then the endpoint.
        if let Some(live) = live {
            live.shutdown().await;
        }
    });
    info!("disconnected");
}

// JNI string helpers

fn empty_string<'a>(env: &mut JNIEnv<'a>) -> JString<'a> {
    env.new_string("").expect("allocating an empty JNI string")
}

fn new_string<'a>(env: &mut JNIEnv<'a>, value: &str) -> JString<'a> {
    match env.new_string(value) {
        Ok(string) => string,
        Err(err) => {
            error!(error = %err, "failed to allocate a JNI string");
            empty_string(env)
        }
    }
}
