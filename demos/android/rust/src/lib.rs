#![cfg(target_os = "android")]
//! JNI bridge for the iroh-live Android demo app.
//!
//! Kotlin can watch or publish a broadcast, make or answer a call, push camera
//! frames and draw the video. Two offline pipelines run the media stack with no
//! network, to smoke-test MediaCodec on a device.
//!
//! A global tokio runtime drives all async work. Each session lives behind a
//! `jlong` handle that Kotlin passes back in.

mod logcat;

use std::{
    collections::BTreeSet,
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

use iroh::EndpointId;
use iroh_live::{
    Audience, BroadcastTicket, CALL, EndpointOptions, Live, Publication, Reach,
    media::{
        AudioEncoding, AudioOutput, AudioSource, Catalog, FrameSender, LocalBroadcast,
        MicrophoneConfig, Player, PlayerConfig, RemoteBroadcast, RenditionMode, VideoEncoding,
        VideoFormat, VideoFrames, VideoRendition, VideoSource,
    },
};
use iroh_live_media_android::{handle, renderer::AndroidRenderer};
use jni::{
    JNIEnv, JavaVM,
    objects::{JByteArray, JClass, JObject, JString},
    sys::{jboolean, jint, jlong},
};
use moq_net::Timestamp;
use moq_video::{Frame, I420, Rate, Size, Surface};
use n0_error::{Result, StackResultExt, StdResultExt, anyerr};
use n0_future::task::AbortOnDropHandle;
use tokio::runtime::Runtime;
use tracing::{error, info, warn};

/// Log targets and levels the demo wants in logcat.
const LOGCAT_FILTER: &str = "\
    warn,\
    iroh=debug,\
    iroh_live=debug,\
    iroh_live_android=debug,\
    iroh_moq=debug,\
    iroh_live_media=debug,\
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
/// The demo stores no key, so without `IROH_SECRET` every screen gets a new
/// endpoint id.
async fn bind_live() -> Result<Live> {
    let options = EndpointOptions::from_env()?;
    Ok(Live::builder(options.bind().await?).with_router().spawn())
}

/// This node's side of a call, offered to one peer while held.
///
/// The peer sees the offer in its route table, which is how it learns it is
/// called, as `irl call` does.
struct Offer {
    _publication: Publication,
    /// The one peer the side is offered to. Dropping it offers to nobody.
    _audience: n0_watcher::Watchable<BTreeSet<EndpointId>>,
}

impl Offer {
    fn new(live: &Live, broadcast: &LocalBroadcast, peer: EndpointId) -> Result<Self> {
        let audience = n0_watcher::Watchable::new(BTreeSet::from([peer]));
        let publication = live.moq().publish(
            live.ticket(CALL).path(),
            broadcast,
            Audience::Peers(audience.watch()),
        )?;
        Ok(Self {
            _publication: publication,
            _audience: audience,
        })
    }
}

/// Subscribes to `peer`'s side of a call and plays it.
async fn play_call(live: &Live, peer: EndpointId, output: &AudioOutput) -> Result<Player> {
    let path = BroadcastTicket::new(peer, CALL).path();
    let subscription = live.moq().subscribe(path, Reach::Direct(peer)).await?;
    play(&live.remote_broadcast(&subscription), output)
}

/// The state behind the `jlong` handle Kotlin holds.
///
/// It is shared behind a mutex because the render, camera and UI threads call
/// in concurrently.
struct SessionHandle {
    /// The endpoint and transport. `None` for the offline pipelines.
    live: Option<Live>,
    /// This node's side of a call, once the call is up.
    offer: Option<Offer>,
    /// Playback of the watched broadcast. Dropping it stops decoding and audio.
    player: Option<Player>,
    /// The speaker, which is also the microphone's echo reference.
    ///
    /// `None` in the offline pipelines and in a publish-only session.
    #[allow(dead_code, reason = "dropping it closes the speaker")]
    output: Option<AudioOutput>,
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
            offer: None,
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

    /// Returns the round-trip time on the link the player reads, once measured.
    fn rtt(&self) -> Option<Duration> {
        self.player.as_ref()?.stats().network?.rtt
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
    fn catalog(&self) -> Option<Catalog> {
        self.player.as_ref()?.broadcast().catalog().borrow().clone()
    }
}

/// Returns a new reference to the handle's state.
///
/// # Safety
///
/// `h` must be a live handle from [`SessionHandle::into_shared`].
unsafe fn borrow_handle(h: jlong) -> SharedHandle {
    unsafe { handle::from_i64(h) }
}

/// Takes back ownership of the handle's state.
///
/// # Safety
///
/// `h` must be a live handle, and must not be used after this call.
unsafe fn take_handle(h: jlong) -> SharedHandle {
    unsafe { handle::take_i64(h) }
}

/// Reads a JNI string, returning `None` on failure.
fn read_jstring(env: &mut JNIEnv<'_>, s: &JString<'_>) -> Option<String> {
    match env.get_string(s) {
        Ok(s) => Some(s.into()),
        Err(err) => {
            error!("failed to read JNI string: {err}");
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
    let rate = Rate::new(CAMERA_FPS, 1).std_context("camera frame rate")?;
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
async fn set_microphone(broadcast: &LocalBroadcast, echo: Option<&AudioOutput>) {
    let config = MicrophoneConfig {
        echo_reference: echo.cloned(),
        ..MicrophoneConfig::default()
    };
    let published = AudioSource::microphone(config)
        .await
        .and_then(|source| broadcast.set_audio(source, AudioEncoding::voice()));
    if let Err(err) = published {
        warn!("publishing without audio: {err:#}");
    }
}

/// Opens the default speaker, or an output that discards audio if that fails.
async fn open_output() -> AudioOutput {
    match AudioOutput::open(None).await {
        Ok(output) => output,
        Err(err) => {
            warn!("playing audio nowhere, the speaker did not open: {err:#}");
            AudioOutput::null()
        }
    }
}

/// Starts playing `remote`, with its audio through `output`.
fn play(remote: &RemoteBroadcast, output: &AudioOutput) -> Result<Player> {
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
            error!("connect failed: {err:#}");
            0
        }
    }
}

async fn connect_impl(ticket: String) -> Result<jlong> {
    let ticket: BroadcastTicket = ticket.parse().context("failed to parse ticket")?;

    let live = bind_live().await?;
    info!(broadcast = %ticket.name(), "connecting to broadcast");

    let subscription = live.subscribe(&ticket).await?;
    info!("subscribed");

    // The player waits for the catalog on its own, so the handle is usable
    // right away.
    let output = open_output().await;
    let remote = live.remote_broadcast(&subscription);
    let player = play(&remote, &output)?;

    let mut session = SessionHandle::new();
    session.frames = Some(player.video());
    session.player = Some(player);
    session.output = Some(output);
    session.live = Some(live);
    Ok(handle::to_i64(session.into_shared()))
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
            error!("dial failed: {err:#}");
            0
        }
    }
}

async fn dial_impl(ticket: String, size: Size) -> Result<jlong> {
    info!(%ticket, %size, "parsing call ticket");
    let ticket: BroadcastTicket = ticket.parse().context("failed to parse call ticket")?;

    let live = bind_live().await?;
    info!(id = %live.endpoint().id().fmt_short(), "endpoint ready");

    let output = open_output().await;
    let broadcast = LocalBroadcast::new();
    let (camera, _preview) = set_camera(&broadcast, size)?;
    set_microphone(&broadcast, Some(&output)).await;

    let offer = Offer::new(&live, &broadcast, ticket.peer())?;
    // A busy callee answers only once its call ends, so give up as `irl call`
    // does.
    let player = tokio::time::timeout(PEER_TIMEOUT, play_call(&live, ticket.peer(), &output))
        .await
        .std_context("the peer did not answer")??;
    info!(remote = %ticket.peer().fmt_short(), "call connected");

    let mut session = SessionHandle::new();
    session.frames = Some(player.video());
    session.player = Some(player);
    session.output = Some(output);
    session.camera = Some(camera);
    session.broadcast = Some(broadcast);
    session.offer = Some(offer);
    session.live = Some(live);
    Ok(handle::to_i64(session.into_shared()))
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
            error!("answer failed: {err:#}");
            0
        }
    }
}

async fn answer_impl(size: Size) -> Result<jlong> {
    info!(%size, "waiting for a call");
    let live = bind_live().await?;
    let id = live.endpoint().id();
    info!(id = %id.fmt_short(), "endpoint ready");

    // Start the camera now, so the preview runs while the ticket is on screen
    // and the peer's first frame does not wait for the device to open.
    let output = open_output().await;
    let broadcast = LocalBroadcast::new();
    let (camera, preview) = set_camera(&broadcast, size)?;
    set_microphone(&broadcast, Some(&output)).await;

    let mut session = SessionHandle::new();
    session.frames = Some(preview);
    session.output = Some(output.clone());
    session.camera = Some(camera);
    session.ticket = Some(live.ticket(CALL).to_string());
    session.broadcast = Some(broadcast.clone());
    session.live = Some(live.clone());
    let shared = session.into_shared();

    // The task holds a `Weak`. A strong reference would keep the handle, and
    // with it the endpoint and camera, alive until somebody calls. The handle
    // owns the task through `waiting`, so dropping the last `Arc` aborts it.
    let waiting = Arc::downgrade(&shared);
    let task = runtime().spawn(async move {
        if let Err(err) = accept_one(live, broadcast, output, waiting).await {
            error!("answering failed: {err:#}");
        }
    });
    shared.lock().expect("poisoned").waiting = Some(AbortOnDropHandle::new(task));

    Ok(handle::to_i64(shared))
}

/// Answers the first peer that calls and installs a player for it on `session`.
///
/// A caller offers `live/<its id>/call` to this node only, so the first such
/// path in the route table is the call.
async fn accept_one(
    live: Live,
    broadcast: LocalBroadcast,
    output: AudioOutput,
    session: Weak<Mutex<SessionHandle>>,
) -> Result<()> {
    let me = live.endpoint().id();
    let mut updates = live.moq().origin().announced();
    let peer = loop {
        let Some(update) = updates.next().await else {
            return Ok(());
        };
        let caller = BroadcastTicket::from_path(update.prefix.as_str())
            .filter(|ticket| update.kind.is_active() && ticket.name() == CALL);
        if let Some(ticket) = caller.filter(|ticket| ticket.peer() != me) {
            break ticket.peer();
        }
    };
    info!(remote = %peer.fmt_short(), "answering");
    let offer = Offer::new(&live, &broadcast, peer)?;
    let player = play_call(&live, peer, &output).await?;
    info!(remote = %peer.fmt_short(), "call answered");

    let Some(session) = session.upgrade() else {
        return Ok(());
    };
    let mut held = session.lock().expect("poisoned");
    if held.closing {
        return Ok(());
    }
    // Replace the local preview with the peer's video.
    held.frames = Some(player.video());
    held.player = Some(player);
    held.offer = Some(offer);
    Ok(())
}

/// Returns whether an answered call has a peer yet.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_callConnected(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jboolean {
    if handle == 0 {
        return 0;
    }
    let session = unsafe { borrow_handle(handle) };
    let connected = session.lock().expect("poisoned").offer.is_some();
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
            error!("publish failed: {err:#}");
            0
        }
    }
}

async fn publish_impl(name: String, size: Size) -> Result<jlong> {
    info!(%name, %size, "publishing broadcast");
    let live = bind_live().await?;
    info!(id = %live.endpoint().id().fmt_short(), "endpoint ready");

    let broadcast = LocalBroadcast::new();
    let (camera, preview) = set_camera(&broadcast, size)?;
    // A publish-only session plays no sound, so there is no echo to cancel.
    set_microphone(&broadcast, None).await;
    live.publish(&name, &broadcast)?;

    let ticket = live.ticket(&name).to_string();
    info!(%ticket, "broadcast published");

    let mut session = SessionHandle::new();
    session.frames = Some(preview);
    session.camera = Some(camera);
    session.ticket = Some(ticket);
    session.broadcast = Some(broadcast);
    session.live = Some(live);
    Ok(handle::to_i64(session.into_shared()))
}

/// Returns the ticket a published broadcast is reachable by, or an empty string.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_getTicket<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> JString<'a> {
    if handle == 0 {
        return empty_string(&mut env);
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else {
        return empty_string(&mut env);
    };
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
            error!("startDirect failed: {err:#}");
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
    Ok(handle::to_i64(session.into_shared()))
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
            error!("startH264 failed: {err:#}");
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
    session.frames = Some(player.video());
    session.player = Some(player);
    session.camera = Some(camera);
    session.broadcast = Some(broadcast);
    Ok(handle::to_i64(session.into_shared()))
}

/// Plays a local broadcast in-process.
///
/// The catalog appears only after the first camera frame. The player waits for
/// it on its own.
fn open_loopback(broadcast: &LocalBroadcast) -> Result<Player> {
    let player = RemoteBroadcast::local(broadcast).play(PlayerConfig::default())?;
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
    let guard = session.lock().ok()?;
    Some(CameraTarget {
        sink: guard.camera.clone()?,
        timestamp: guard.timestamp(),
        pushed: guard.cam_frames_pushed,
    })
}

/// Counts a frame that reached the publish pipeline.
fn count_camera_frame(session: &SharedHandle) {
    if let Ok(mut guard) = session.lock() {
        guard.cam_frames_pushed += 1;
    }
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
    if handle == 0 {
        return;
    }
    let Ok(rgba) = env.convert_byte_array(data) else {
        error!("failed to read camera frame byte array");
        return;
    };

    let session = unsafe { borrow_handle(handle) };
    let Some(target) = camera_target(&session) else {
        return;
    };
    let size = Size::new(width as u32, height as u32);
    let surface = match Surface::rgba(&rgba, size) {
        Ok(surface) => surface,
        Err(err) => {
            warn!(%size, "rejected RGBA camera frame: {err}");
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
    if handle == 0 {
        return;
    }
    let (Ok(y), Ok(uv)) = (
        env.convert_byte_array(y_data),
        env.convert_byte_array(uv_data),
    ) else {
        error!("failed to read camera planes");
        return;
    };

    let session = unsafe { borrow_handle(handle) };
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
            warn!(%size, "rejected NV12 camera frame: {err:#}");
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
    if handle == 0 {
        return;
    }
    // SAFETY: JNI just passed a valid JNIEnv and a live `android.view.Surface`.
    let native_window = unsafe {
        moq_video::ndk::native_window::NativeWindow::from_surface(env.get_raw(), surface.as_raw())
    };
    let Some(native_window) = native_window else {
        error!("ANativeWindow_fromSurface returned null");
        return;
    };

    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else { return };
    // SAFETY: the window comes from a live surface and outlives this call. The
    // renderer takes its own reference.
    match unsafe { AndroidRenderer::new(native_window.ptr().as_ptr().cast()) } {
        Ok(renderer) => *guard.renderer.lock().expect("renderer lock") = Some(renderer),
        Err(err) => error!("initSurface failed: {err:#}"),
    }
}

/// Tears down the EGL surface and context when the render loop exits.
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
    let Ok(mut renderer) = guard.renderer.lock() else {
        return;
    };
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
    if handle == 0 {
        return false;
    }
    let session = unsafe { borrow_handle(handle) };

    // Hold the session lock only to take the frame. Drawing can be slow, and a
    // camera push that waits on the lock drops a frame.
    let (frame, renderer) = {
        let Ok(mut guard) = session.lock() else {
            return false;
        };
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

    let Ok(renderer) = renderer.lock() else {
        return false;
    };
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
                    warn!("decoded frame has no hardware buffer: {err}");
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
    if handle == 0 {
        return 0;
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else { return 0 };

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
    if handle == 0 {
        return empty_string(&mut env);
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else {
        return empty_string(&mut env);
    };
    let names = guard
        .catalog()
        .map(|catalog| {
            catalog
                .ranked_video()
                .into_iter()
                .map(|(name, _)| name)
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
    if handle == 0 {
        return;
    }
    let Some(name) = read_jstring(&mut env, &rendition_name) else {
        return;
    };
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else { return };
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
    if handle == 0 {
        return empty_string(&mut env);
    }
    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else {
        return empty_string(&mut env);
    };
    let line = status_line(&guard);
    new_string(&mut env, &line)
}

/// Formats the debug overlay line.
fn status_line(session: &SessionHandle) -> String {
    let video = session
        .rendition()
        .map(|name| format!("trk:{name}"))
        .unwrap_or_else(|| "no track".into());
    let dims = session
        .frame_dims
        .map(|size| size.to_string())
        .unwrap_or_else(|| "?".into());
    let cam = session.cam_frames_pushed;
    let dec = session.dec_frames_rendered;
    let net = session
        .rtt()
        .map(|rtt| format!("rtt:{}ms", rtt.as_millis()))
        .unwrap_or_default();
    let playout = session
        .player
        .as_ref()
        .map(|player| format!("lat:{}ms", player.stats().latency.as_millis()))
        .unwrap_or_default();
    let elapsed = session.created_at.elapsed().as_secs();

    format!("{video} {dims} | cam:{cam} dec:{dec} | {net} {playout} | {elapsed}s")
}

// JNI: teardown

/// Disconnects and frees the session handle, which must not be used after.
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
    // Release the lock before `block_on`. Holding it would stall every other
    // JNI call on this handle until the endpoint closes.
    let (waiting, player, offer, broadcast, live) = match session.lock() {
        Ok(mut guard) => (
            {
                guard.closing = true;
                guard.waiting.take()
            },
            guard.player.take(),
            guard.offer.take(),
            guard.broadcast.take(),
            guard.live.take(),
        ),
        Err(_) => {
            warn!("session handle was poisoned; skipping shutdown");
            return;
        }
    };
    runtime().block_on(async move {
        // Stop waiting for a caller first, so none is accepted during shutdown.
        drop(waiting);
        drop(player);
        drop(offer);
        if let Some(broadcast) = broadcast {
            // Let the encoders finish, so subscribers see the broadcast end.
            broadcast.close();
            broadcast.closed().await;
        }
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
            error!("failed to allocate a JNI string: {err}");
            empty_string(env)
        }
    }
}
