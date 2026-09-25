#![cfg(target_os = "android")]
//! JNI bridge for the iroh-live Android demo app.
//!
//! Exposes a small set of functions to Kotlin: connect to a broadcast, publish
//! one, dial a peer for a two-way call, push camera frames, and draw whatever is
//! being decoded. Two offline pipelines exercise the media stack with no network
//! at all, which is how the MediaCodec encoder and decoder get smoke-tested on a
//! device.
//!
//! A global tokio runtime drives all async work. Every session lives behind one
//! `jlong` handle that Kotlin holds and passes back in.

mod logcat;

use std::{
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

use iroh::EndpointId;
use iroh_live::{BroadcastTicket, EndpointOptions, Live, Session, Subscription};
use iroh_live_media::{
    AudioEncoding, AudioOutput, AudioSource, Catalog, LocalBroadcast, MicrophoneConfig, Player,
    PlayerConfig, RemoteBroadcast, RenditionMode, VideoEncoding, VideoFrames, VideoRendition,
};
use iroh_live_media_android::{
    camera::{CameraSink, camera},
    handle,
    renderer::AndroidRenderer,
};
use jni::{
    JNIEnv, JavaVM,
    objects::{JByteArray, JClass, JObject, JString},
    sys::{jboolean, jint, jlong},
};
use moq_net::Timestamp;
use moq_video::{Frame, I420, Rate, Size, Surface};
use n0_error::{Result, StackResultExt, StdResultExt, anyerr};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;
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

/// The frame rate the camera source is declared at.
///
/// Camera2 delivers at whatever rate the device settles on, and the broadcast
/// encodes the frames that actually arrive; this is the ceiling the encoder
/// plans for.
const CAMERA_FPS: u32 = 30;

/// The track name of the one video rendition the demo publishes.
const VIDEO_RENDITION: &str = "video";

/// Initializes ndk-context and tracing on library load.
///
/// Called by the JVM when `System.loadLibrary` loads this `.so`.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut c_void) -> jint {
    // SAFETY: the JVM guarantees `vm` is valid for the duration of JNI_OnLoad,
    // and the pointer it hands out stays valid for the process. The activity is
    // null because cpal's Oboe backend only needs the VM pointer.
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

/// Binds the endpoint every screen runs on, under `IROH_SECRET` when it is
/// set.
///
/// An app keeps its identity in its own storage; the demo has none, so it
/// takes a fresh one unless the environment says otherwise.
async fn bind_live() -> Result<Live> {
    let mut options = EndpointOptions::default();
    if let Ok(key) = std::env::var("IROH_SECRET") {
        options.secret_key = Some(key.parse().context("IROH_SECRET is not a key")?);
    }
    Ok(Live::builder(options.bind().await?).with_router().spawn())
}

/// How long an incoming session has to show its side of a call before it
/// counts as not a caller.
///
/// A caller publishes its side before it dials, so it announces it right after
/// the session opens. A plain subscriber never does, and without a bound it
/// would hold up answering until its session closed.
const CALLER_TIMEOUT: Duration = Duration::from_secs(10);

/// The broadcast a peer publishes its side of a call as, the convention
/// `irl call` shares.
const CALL: &str = "call";

/// Publishes a fresh broadcast as `name` to everyone.
fn publish(live: &Live, name: &str) -> Result<LocalBroadcast> {
    let local = LocalBroadcast::new();
    live.publish(name, &local)?;
    Ok(local)
}

/// A call: the session with the peer, and the peer's side read over it.
#[derive(Debug)]
struct Call {
    session: Session,
    remote: RemoteBroadcast,
}

impl Call {
    /// Dials `peer` and subscribes to its side of the call.
    async fn dial(live: &Live, peer: EndpointId) -> Result<Self> {
        let session = live.moq().connect(peer).await?;
        Self::accept(live, session).await
    }

    /// Subscribes to the side of the call the peer of `session` publishes.
    async fn accept(live: &Live, session: Session) -> Result<Self> {
        let path = BroadcastTicket::new(session.remote_id(), CALL).path();
        let subscription = session.subscribe(path).await?;
        let remote = live.remote_broadcast(&subscription);
        Ok(Self { session, remote })
    }

    fn remote(&self) -> &RemoteBroadcast {
        &self.remote
    }

    fn remote_id(&self) -> EndpointId {
        self.session.remote_id()
    }

    fn session(&self) -> &Session {
        &self.session
    }

    /// Hangs up: closes the session, which the peer sees as the call ending.
    fn close(&self) {
        self.session.close("hung up");
    }
}

/// Opaque handle stored as a `jlong` on the Kotlin side.
///
/// `Arc<Mutex<..>>` because JNI calls arrive concurrently from the render
/// thread, the camera analyzer thread, and the UI thread.
struct SessionHandle {
    /// The endpoint and transport. `None` for the offline pipelines.
    live: Option<Live>,
    /// A subscribe-only session, from `connect`. Held so the route it
    /// resolved stays readable for the overlay's round trip.
    subscription: Option<Subscription>,
    /// A two-way call, from `dial`. Owns its session and the peer's broadcast.
    call: Option<Call>,
    /// The playback of the broadcast being watched, from whichever path opened
    /// it. Dropping it stops its decoders and its audio.
    player: Option<Player>,
    /// The speaker the player's audio plays through, which is also the signal
    /// the microphone's echo canceller subtracts. `None` where nothing plays
    /// sound: the offline pipelines and a publish-only session.
    #[allow(dead_code, reason = "dropping it closes the speaker")]
    output: Option<AudioOutput>,
    /// The broadcast this node publishes, whether it is being watched by a
    /// subscriber or carried by a call.
    broadcast: Option<LocalBroadcast>,
    /// Where the camera frames Kotlin pushes go.
    camera: Option<CameraSink>,
    /// What the render loop draws: the player's decoded frames, or this node's
    /// own camera frames as they reach the encoder. `None` while there is
    /// nothing to draw, as in a publish-only session with no preview.
    frames: Option<VideoFrames>,
    /// The ticket a published broadcast is reachable by.
    ticket: Option<String>,
    /// The GLES renderer, behind its own lock so drawing does not block camera
    /// pushes on the session mutex.
    renderer: Arc<Mutex<Option<AndroidRenderer>>>,
    /// The dimensions of the last frame drawn, which the decoder knows more
    /// precisely than the catalog does.
    frame_dims: Option<Size>,
    /// The task waiting for somebody to call, for a handle from `answer`.
    /// Aborted when this handle drops, which is what stops an unanswered wait
    /// outliving the screen that started it.
    waiting: Option<AbortOnDropHandle<()>>,
    /// Set by `disconnect` under the lock, so an answer that lands while the
    /// handle is being torn down closes its call rather than installing it on
    /// a handle nobody will close again.
    closing: bool,
    cam_frames_pushed: u64,
    dec_frames_rendered: u64,
    created_at: Instant,
}

type SharedHandle = Arc<Mutex<SessionHandle>>;

impl SessionHandle {
    /// An empty handle: no session, nothing to draw, counters at zero.
    fn new() -> Self {
        Self {
            live: None,
            subscription: None,
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

    /// The round-trip time on the serving link, once it measured one.
    fn rtt(&self) -> Option<Duration> {
        if let Some(serving) = self.subscription.as_ref().and_then(Subscription::link) {
            return serving.sample.rtt;
        }
        self.call.as_ref()?.session().link().rtt
    }

    /// The timestamp to stamp the next camera frame with.
    ///
    /// Taken from this session's own start. The broadcast rebases every source
    /// onto its media clock at the source's first frame, so a camera only has
    /// to hand it a steady timeline, not one shared with the microphone.
    fn timestamp(&self) -> Timestamp {
        let micros = u64::try_from(self.created_at.elapsed().as_micros()).unwrap_or(u64::MAX);
        // Overflows only after some 146,000 years of uptime.
        Timestamp::from_micros(micros).unwrap_or(Timestamp::ZERO)
    }

    /// The rendition the player is drawing, for the status line.
    fn rendition(&self) -> Option<String> {
        self.player.as_ref()?.status().get().rendition
    }

    /// The newest catalog of the broadcast being watched, if it arrived yet.
    fn catalog(&self) -> Option<Catalog> {
        self.player.as_ref()?.broadcast().catalog().get()
    }
}

/// # Safety
/// `h` must be a live handle from [`SessionHandle::into_shared`].
unsafe fn borrow_handle(h: jlong) -> SharedHandle {
    unsafe { handle::from_i64(h) }
}

/// # Safety
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

// ── Capture wiring ──────────────────────────────────────────────────

/// Points a broadcast's video at the camera Kotlin pushes into.
///
/// Video is a push source: Camera2 delivers frames on whichever thread it likes
/// and the encoder reads the newest one. Returns the sink for Kotlin's frames,
/// and the same frames as they reach the encoder, which a local preview draws
/// at no encode or decode cost.
fn set_camera(broadcast: &LocalBroadcast, size: Size) -> Result<(CameraSink, VideoFrames)> {
    let rate = Rate::new(CAMERA_FPS, 1).std_context("camera frame rate")?;
    let (sink, source) = camera(size, rate);
    let preview = source.frames();
    broadcast.set_video(
        source,
        VideoEncoding::single(VideoRendition::new(VIDEO_RENDITION)),
    )?;
    Ok((sink, preview))
}

/// Publishes the default microphone, with `echo`'s signal cancelled from it
/// when one is given.
///
/// Unlike the camera, this is a device the broadcast opens itself: nothing in
/// Kotlin touches the microphone. A call passes the speaker its player plays
/// through, since a handset on speakerphone would otherwise send the peer's
/// voice straight back. A microphone that will not open is logged and left
/// out, so the session still carries video.
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

/// Opens the default speaker, or an output that discards everything if there
/// is none.
///
/// A session without sound is still worth having, so a speaker that will not
/// open is logged rather than failing the session.
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

// ── JNI: connect (subscribe only) ───────────────────────────────────

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

    let subscription = live
        .moq()
        .subscribe(ticket.path(), iroh_live::Reach::Both(ticket.peer()))
        .await?;
    info!("subscribed");

    // The player picks up the catalog and the first rendition on its own, so
    // the handle is usable before either has arrived.
    let output = open_output().await;
    let remote = live.remote_broadcast(&subscription);
    let player = play(&remote, &output)?;

    let mut session = SessionHandle::new();
    session.frames = Some(player.video());
    session.player = Some(player);
    session.output = Some(output);
    session.subscription = Some(subscription);
    session.live = Some(live);
    Ok(handle::to_i64(session.into_shared()))
}

// ── JNI: dial (two-way call) ────────────────────────────────────────

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

    // Each peer publishes its own side of the call under its own endpoint id,
    // and subscribes to the other's.
    let output = open_output().await;
    let broadcast = publish(&live, CALL)?;
    let (camera, _preview) = set_camera(&broadcast, size)?;
    set_microphone(&broadcast, Some(&output)).await;

    let call = Call::dial(&live, ticket.peer()).await?;
    info!(remote = %call.remote_id().fmt_short(), "call connected");

    let player = play(call.remote(), &output)?;

    let mut session = SessionHandle::new();
    session.frames = Some(player.video());
    session.player = Some(player);
    session.output = Some(output);
    session.camera = Some(camera);
    session.broadcast = Some(broadcast);
    session.call = Some(call);
    session.live = Some(live);
    Ok(handle::to_i64(session.into_shared()))
}

/// Publishes this node's side of a call and waits for a peer to dial it.
///
/// Returns a session handle at once, with the ticket already readable, so the
/// screen can show the code before any peer exists. The peer's tracks arrive
/// later, on a task this handle owns;
/// [`Java_com_n0_irohlive_demo_IrohBridge_callConnected`] says when.
///
/// Returns 0 on failure.
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

    // The same shape the dialing side uses: each peer publishes its own half of
    // the call under its own endpoint id, and subscribes to the other's. The
    // camera starts here rather than when a peer arrives, so the preview is
    // live while the code is on screen and the first frame the peer sees does
    // not wait for a device to open.
    let output = open_output().await;
    let broadcast = publish(&live, CALL)?;
    let (camera, preview) = set_camera(&broadcast, size)?;
    set_microphone(&broadcast, Some(&output)).await;

    let mut session = SessionHandle::new();
    session.frames = Some(preview);
    session.output = Some(output.clone());
    session.camera = Some(camera);
    session.ticket = Some(live.ticket(CALL).to_string());
    session.broadcast = Some(broadcast);
    session.live = Some(live.clone());
    let shared = session.into_shared();

    // The task gets a `Weak`, not an `Arc`. Holding a strong reference to the
    // handle it is going to write into would keep that handle alive for as long
    // as the task runs, and the task runs until somebody calls, so a screen
    // backed out of before any peer arrived would hold its endpoint, its camera
    // and its broadcast open for the rest of the process. Instead the task is
    // owned by the handle through `waiting`, so `disconnect` dropping the last
    // `Arc` aborts it.
    let waiting = Arc::downgrade(&shared);
    let task = runtime().spawn(async move {
        if let Err(err) = accept_one(live, output, waiting).await {
            error!("answering failed: {err:#}");
        }
    });
    shared.lock().expect("poisoned").waiting = Some(AbortOnDropHandle::new(task));

    Ok(handle::to_i64(shared))
}

/// Accepts the first peer that calls, and installs a player for its side on
/// `session`, with the audio through `output`.
///
/// Sessions this node dialed are skipped: everything that speaks MoQ arrives
/// the same way, and only an inbound one can be a caller.
async fn accept_one(
    live: Live,
    output: AudioOutput,
    session: Weak<Mutex<SessionHandle>>,
) -> Result<()> {
    let mut sessions = live.moq().sessions();
    let mut seen: Vec<Session> = Vec::new();
    loop {
        let current = sessions.get();
        let arrived: Vec<Session> = current
            .iter()
            .filter(|moq| !moq.dialed() && !seen.contains(moq))
            .cloned()
            .collect();
        seen = current;
        if arrived.is_empty() {
            if sessions.updated().await.is_err() {
                return Ok(());
            }
            continue;
        }
        let Some(moq) = arrived.into_iter().next() else {
            continue;
        };
        let remote_id = moq.remote_id();
        info!(remote = %remote_id.fmt_short(), "incoming session");
        // A plain subscriber arrives here too and never publishes the call path
        // this waits for, so a failure is an ordinary outcome rather than an
        // error: keep listening for somebody who does.
        let call = match tokio::time::timeout(CALLER_TIMEOUT, Call::accept(&live, moq)).await {
            Ok(Ok(call)) => call,
            Ok(Err(err)) => {
                info!(remote = %remote_id.fmt_short(), error = %err, "not a caller");
                continue;
            }
            Err(_) => {
                info!(remote = %remote_id.fmt_short(), "not a caller: announced no call in time");
                continue;
            }
        };
        info!(remote = %call.remote_id().fmt_short(), "call answered");
        let player = play(call.remote(), &output)?;

        let Some(session) = session.upgrade() else {
            // The screen was left while this was settling, so there is nothing
            // to install it on. Closing the call tells the peer, where dropping
            // it would leave them waiting out a timeout.
            call.close();
            return Ok(());
        };
        let mut held = session.lock().expect("poisoned");
        if held.closing {
            // Disconnected while this was settling: the fields were already
            // taken for shutdown, so nothing would close this call later.
            drop(held);
            drop(player);
            call.close();
            return Ok(());
        }
        // Replaces the local preview, so the screen switches from this node's
        // own camera to the peer's picture the moment there is one.
        held.frames = Some(player.video());
        held.player = Some(player);
        held.call = Some(call);
        return Ok(());
    }
}

/// Whether an answered call has a peer on it yet.
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
    let connected = session.lock().expect("poisoned").call.is_some();
    jboolean::from(connected)
}

// ── JNI: publish ────────────────────────────────────────────────────

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
    // Nothing plays sound in a publish-only session, so there is no echo to
    // cancel and no speaker to open.
    set_microphone(&broadcast, None).await;
    live.publish(&name, &broadcast)?;

    let ticket = BroadcastTicket::new(live.endpoint().id(), name.as_str()).to_string();
    info!(%ticket, "broadcast published");

    let mut session = SessionHandle::new();
    // The publisher watches itself: the preview is the frames on their way to
    // the encoder, so it costs no decode.
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

// ── JNI: offline pipelines ──────────────────────────────────────────

/// Starts a camera passthrough pipeline: no encode, no decode, no network.
///
/// The camera feeds a broadcast nobody subscribes to, so its encoders stay
/// idle and the preview draws the frames exactly as they arrive.
///
/// Returns a session handle, or 0 on failure.
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
    // The encode task is a tokio task even with no transport under it.
    let _guard = runtime().enter();

    let broadcast = LocalBroadcast::new();
    let (camera, preview) = set_camera(&broadcast, size)?;

    let mut session = SessionHandle::new();
    session.frames = Some(preview);
    session.camera = Some(camera);
    session.broadcast = Some(broadcast);
    Ok(handle::to_i64(session.into_shared()))
}

/// Starts a local encode and decode loop: camera to MediaCodec and back, with
/// no network in between.
///
/// Returns a session handle, or 0 on failure.
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

/// Plays a broadcast this node publishes, in-process, for the render loop to
/// draw what comes back.
///
/// The catalog only appears once the camera has pushed a first frame, which
/// cannot happen before Kotlin holds the handle. The player waits for it on its
/// own, so nothing here has to.
fn open_loopback(broadcast: &LocalBroadcast) -> Result<Player> {
    let player = RemoteBroadcast::local(broadcast).play(PlayerConfig::default())?;
    info!(broadcast = LOOPBACK_NAME, "loopback playing");
    Ok(player)
}

// ── JNI: camera frame push ──────────────────────────────────────────

/// What a camera push needs from the handle.
///
/// Read under the lock and returned by value, because turning a camera buffer
/// into a frame costs a full-picture copy and the render loop wants this mutex
/// back long before that finishes.
struct CameraTarget {
    sink: CameraSink,
    timestamp: Timestamp,
    /// How many frames were pushed before this one.
    pushed: u64,
}

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
    if let Err(err) = target.sink.push_rgba(&rgba, target.timestamp) {
        warn!(width, height, "rejected RGBA camera frame: {err}");
        return;
    }
    count_camera_frame(&session);
}

/// Pushes one camera frame as the NV12 planes Camera2 hands out.
///
/// `y_stride` and `uv_stride` are the driver's row pitches, which are often
/// wider than the picture.
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
    if let Err(err) = target.sink.push(frame) {
        warn!(%size, "rejected NV12 camera frame: {err}");
        return;
    }
    count_camera_frame(&session);
}

/// Deinterleaves Camera2's NV12 planes into the packed I420 an encoder wants.
///
/// The copy is unavoidable: NV12 rows carry driver padding and its chroma is
/// interleaved, while `moq_video::I420` is tightly packed and planar.
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

// ── JNI: rendering ──────────────────────────────────────────────────

/// Creates the EGL context and GL renderer for an Android surface.
///
/// Must be called from the render thread. Rust owns the whole EGL lifecycle;
/// Kotlin only hands over the `android.view.Surface`.
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
    // SAFETY: ANativeWindow_fromSurface needs the raw JNIEnv and a live
    // `android.view.Surface`, both of which JNI just handed us.
    let native_window = unsafe {
        moq_video::ndk::native_window::NativeWindow::from_surface(env.get_raw(), surface.as_raw())
    };
    let Some(native_window) = native_window else {
        error!("ANativeWindow_fromSurface returned null");
        return;
    };

    let session = unsafe { borrow_handle(handle) };
    let Ok(guard) = session.lock() else { return };
    // SAFETY: the window was just created from a live surface and outlives this
    // call; the renderer acquires its own reference.
    match unsafe { AndroidRenderer::new(native_window.ptr().as_ptr().cast()) } {
        Ok(renderer) => *guard.renderer.lock().expect("renderer lock") = Some(renderer),
        Err(err) => error!("initSurface failed: {err:#}"),
    }
}

/// Tears down the EGL surface and context, when the render loop exits.
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
/// Returns whether anything was drawn. Must be called from the render thread.
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

    // Hold the session lock only long enough to take the frame. Drawing can be
    // slow, and a camera push waiting on this mutex is a dropped frame.
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
    // The coroutine dispatcher can move the render loop between threads, so the
    // context has to be rebound before every draw.
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
        // The MediaCodec decoder renders into an ImageReader, so a decoded
        // picture reaches GL without a CPU round trip.
        Surface::HardwareBuffer(surface) => {
            let buffer = match surface.buffer() {
                Ok(buffer) => buffer,
                Err(err) => {
                    warn!("decoded frame has no hardware buffer: {err}");
                    return false;
                }
            };
            // SAFETY: the EGL context is current, and `buffer` holds a reference
            // of its own for the whole call.
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
        // Software decode and the camera preview both land here. The shader
        // converts NV12 on the GPU, so interleaving the two chroma planes is
        // much cheaper than converting to RGBA on the CPU.
        Surface::I420(planes) => {
            let chroma = interleave_chroma(planes);
            // SAFETY: the EGL context is current. Both planes are tightly
            // packed, so their strides are the row lengths passed alongside.
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
        // Unreachable today: on Android a surface is one of the two above. The
        // enum is non-exhaustive so that a new variant upstream is a dropped
        // frame here rather than a build failure.
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

// ── JNI: status ─────────────────────────────────────────────────────

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

    // The decoder's own answer beats the catalog's, which describes the encoded
    // resolution rather than what came out.
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
/// The replacement decoder opens alongside the incumbent and takes over once it
/// has caught up, so the picture does not go blank across the switch.
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

// ── JNI: teardown ───────────────────────────────────────────────────

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
    // Take what shutdown needs and release the lock before blocking on it.
    // Holding it across `block_on` would stall every other JNI entry point
    // that touches this handle, including the status line the UI thread reads,
    // for as long as the router and endpoint take to close.
    let (waiting, player, call, subscription, broadcast, live) = match session.lock() {
        Ok(mut guard) => (
            {
                guard.closing = true;
                guard.waiting.take()
            },
            guard.player.take(),
            guard.call.take(),
            guard.subscription.take(),
            guard.broadcast.take(),
            guard.live.take(),
        ),
        Err(_) => {
            warn!("session handle was poisoned; skipping shutdown");
            return;
        }
    };
    runtime().block_on(async move {
        // Stop waiting for a caller first, so none is accepted while the rest
        // closes.
        drop(waiting);
        // Dropping the player stops its decoders and takes its audio off the
        // speaker.
        drop(player);
        if let Some(call) = call {
            // Tells the peer, where only dropping the call would leave them to
            // wait out a timeout.
            call.close();
        }
        drop(subscription);
        if let Some(broadcast) = broadcast {
            // Lets the encoders finish their tracks, so subscribers see the
            // broadcast end rather than stall.
            broadcast.close();
            broadcast.closed().await;
        }
        if let Some(live) = live {
            live.shutdown().await;
        }
    });
    info!("disconnected");
}

// ── JNI string helpers ──────────────────────────────────────────────

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
