//! A shared scene editor: everyone in an iroh-live room places cubes and puts video on them.
//!
//! Each member publishes its own cubes as one latest-value JSON track, the
//! `state` track of its `world` broadcast, and reads every other member's.
//! Only the owner moves a cube or picks its video; the others see it glide to
//! where the owner put it. A video cube publishes a broadcast of its own,
//! which every member plays onto its copy of the cube, the owner included,
//! with the sound fading with the distance to the camera. The chat is the room
//! chat that `irl room` speaks too.
//!
//! Without a ticket or a name on the command line, a start screen asks for
//! them. C copies the room ticket, Ctrl+V pastes one.
//!
//! ```sh
//! cargo run --manifest-path moq-ext-bevy/Cargo.toml --example editor -- --name a
//! # in a second terminal, with the ticket the first one printed
//! cargo run --manifest-path moq-ext-bevy/Cargo.toml --example editor -- <TICKET>
//! ```

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    f32::consts::{FRAC_PI_2, PI},
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use bevy::{
    clipboard::Clipboard,
    input::{
        ButtonState,
        keyboard::{Key, KeyboardInput},
        mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit},
    },
    log::LogPlugin,
    math::Affine2,
    picking::events::PointerClick,
    picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings},
    prelude::*,
};
use clap::Parser;
use iroh::{EndpointId, protocol::Router};
use iroh_live::{
    EndpointConfig, Node, ValueWriter,
    layout::LiveLayout,
    media::{
        AudioMode, FileEnd, LocalBroadcast, MediaFile, Player, PlayerConfig, RemoteBroadcast,
        SubscriptionExt, VideoEncoding, VideoRendition,
        audio::playback::Engine,
        audio_encoding,
        spec::{AudioSourceSpec, TestTone, VideoOpenOptions, VideoSourceSpec},
        video,
    },
    moq_net::{self, broadcast},
    rooms::{Room, RoomTicket, Rooms, RoomsLayout, chat::Chat},
};
use moq_ext_bevy::{VideoTexture, VideoTexturePlugin};
use n0_error::{Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};
use tokio::{
    runtime,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tracing::{debug, error, info, warn};

/// The broadcast each member publishes its cubes on.
const WORLD: &str = "world";
/// The latest-value track on [`WORLD`] that holds a [`MemberState`].
const STATE: &str = "state";
/// How often this member's cubes are published, at most.
const SYNC_INTERVAL: Duration = Duration::from_millis(50);
/// How many network events wait for the frame loop.
const EVENT_QUEUE: usize = 256;
/// How long a reader waits before it reads a broadcast again.
const RETRY: Duration = Duration::from_secs(2);
/// How many chat lines the panel shows.
const CHAT_LINES: usize = 10;
/// How far the pointer travels, in pixels, before a press counts as a drag.
const DRAG_THRESHOLD: f32 = 5.0;
/// Up to this distance from the camera a cube plays at full volume.
const FULL_VOLUME_DISTANCE: f32 = 4.0;
/// How far the arrow keys turn a cube: 15 degrees.
const TURN_STEP: f32 = PI / 12.0;
/// The video sources an own cube switches to: the key, the label, the source spec.
const SOURCES: [(KeyCode, &str, Option<&str>); 4] = [
    (KeyCode::Digit1, "cam", Some("cam")),
    (KeyCode::Digit2, "screen", Some("screen")),
    (KeyCode::Digit3, "test pattern", Some("test:bounce")),
    (KeyCode::Digit4, "none", None),
];

#[derive(Debug, Parser)]
#[command(about = "Edit a scene of cubes together, over an iroh-live room")]
struct Args {
    /// The room to join. Without one, a new room is opened, and without a
    /// name either, a start screen asks.
    ticket: Option<RoomTicket>,
    /// The name the others see.
    #[arg(long)]
    name: Option<String>,
    /// Places a cube with the test pattern at the start.
    #[arg(long)]
    video_cube: bool,
    /// Converts video to RGBA on the CPU instead of drawing it on the GPU.
    #[arg(long)]
    cpu_video: bool,
}

/// What one member publishes: its cubes, and where it put them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct MemberState {
    cubes: Vec<(Cube, Transform)>,
}

/// A cube, and what its owner publishes about it besides the transform.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cube {
    /// The member who owns it, `None` for this member.
    ///
    /// A reader knows whose state it reads, so it is not sent.
    #[serde(skip)]
    owner: Option<EndpointId>,
    id: u32,
    color: [f32; 3],
    video: Option<CubeVideo>,
    /// Quarter turns of the picture on the faces.
    turns: u8,
}

/// The video on a cube: what it shows, and the broadcast that carries it.
///
/// Each switch of source publishes under a new name, so a reader never takes
/// the old video for the new one. The label is all the others learn of the
/// source: a file shows by its name, not its path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CubeVideo {
    label: String,
    broadcast: String,
}

/// Where the video of an own cube comes from.
#[derive(Debug, Clone)]
enum Source {
    /// A source spec, such as `cam`.
    Spec(&'static str),
    /// A video file, sent as it is stored.
    File(PathBuf),
}

/// What the network tasks hand to the frame loop.
enum NetEvent {
    /// The other members, by id, with the names to show.
    Members(BTreeMap<EndpointId, String>),
    /// A member's newest cubes, or none once it left.
    State(EndpointId, MemberState),
    /// A player of the named broadcast for `entity`.
    ///
    /// For an own cube it carries the published broadcast too.
    Video(Entity, String, Box<Player>, Option<LocalBroadcast>),
    /// The file picked for own cube `entity`.
    Picked(Entity, PathBuf),
}

/// The connection to the room, and the channels to the network tasks.
#[derive(Resource)]
struct Net {
    tokio: runtime::Handle,
    room: Room,
    chat: Chat,
    /// The speaker every cube plays its sound on, if one opened.
    speaker: Option<Engine>,
    state: ValueWriter<MemberState>,
    events: mpsc::Receiver<NetEvent>,
    events_tx: mpsc::Sender<NetEvent>,
    ticket: String,
    _world: broadcast::Producer,
    _follow: AbortOnDropHandle<()>,
}

/// The editor's own state.
#[derive(Resource, Default)]
struct Editor {
    next_id: u32,
    /// Numbers the video broadcasts, which get a new name with each switch.
    next_broadcast: u32,
    /// The mesh every cube shares.
    mesh: Handle<Mesh>,
    members: BTreeMap<EndpointId, String>,
    remote: HashMap<(EndpointId, u32), Entity>,
    /// The selected cube, any member's.
    selected: Option<Entity>,
    chat_log: VecDeque<String>,
    /// The chat line being typed, while the chat has the keyboard.
    typing: Option<String>,
    /// A short note for the panel, and until when it shows, in app seconds.
    note: Option<(String, f32)>,
    sync: Timer,
}

impl Editor {
    /// Returns the name member `id` goes by.
    fn name(&self, id: EndpointId) -> String {
        let name = self.members.get(&id).cloned();
        name.unwrap_or_else(|| id.fmt_short().to_string())
    }
}

/// What the left mouse button is doing.
#[derive(Resource, Default)]
struct Pointer {
    /// Where the button went down.
    pressed_at: Option<Vec2>,
    /// The cube the button went down on.
    hit: Option<Entity>,
    /// Whether the pointer moved past [`DRAG_THRESHOLD`] since.
    dragging: bool,
    /// The own cube being moved, and its offset from the point under the pointer.
    moving: Option<(Entity, Vec3)>,
}

/// Where a remote cube is heading.
#[derive(Component)]
struct Glide(Transform);

/// Marks a cube this member muted. Not synced.
#[derive(Component)]
struct Muted;

/// What keeps a cube's video running.
///
/// It holds the task that starts the video, the player, and for an own cube
/// the published broadcast, which ends when it drops.
#[derive(Component)]
struct VideoHold {
    /// The broadcast held, to tell a stale player from the current one.
    broadcast: String,
    player: Option<Player>,
    /// The volume last set on the player.
    volume: f32,
    _published: Option<LocalBroadcast>,
    _task: AbortOnDropHandle<()>,
}

impl VideoHold {
    /// Replaces what `entity` plays with `broadcast`, which `task` starts.
    fn start(commands: &mut Commands, entity: Entity, broadcast: String, task: JoinHandle<()>) {
        commands
            .entity(entity)
            .remove::<VideoTexture>()
            .insert(Self {
                broadcast,
                player: None,
                volume: -1.0,
                _published: None,
                _task: AbortOnDropHandle::new(task),
            });
    }
}

/// An orbit camera, which looks at `focus` from `distance` away.
#[derive(Component)]
struct Orbit {
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
}

impl Orbit {
    fn transform(&self) -> Transform {
        let rotation = Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0);
        Transform::from_translation(self.focus + rotation * Vec3::Z * self.distance)
            .with_rotation(rotation)
    }
}

/// A text panel: the room at the top left, the selected cube at the bottom left.
#[derive(Component, PartialEq)]
enum Hud {
    Room,
    Selection,
}

/// How the editor gets into a room: the start screen's form, then the connection.
#[derive(Resource)]
struct Launch {
    tokio: runtime::Handle,
    /// The room and the router once joined, to leave after the window closes.
    joined: Arc<OnceLock<(Room, Router)>>,
    connecting: Option<oneshot::Receiver<Result<Net>>>,
    name: String,
    ticket: String,
    /// Whether the ticket field has the keyboard, else the name field.
    on_ticket: bool,
    status: String,
    /// Whether to place a video cube once in the room.
    video_cube: bool,
}

impl Launch {
    /// Joins the room of `ticket`, or opens a new one.
    fn connect(&mut self, ticket: Option<RoomTicket>) {
        let (tx, rx) = oneshot::channel();
        let (tokio, joined) = (self.tokio.clone(), self.joined.clone());
        let name = Some(self.name.trim().to_string()).filter(|name| !name.is_empty());
        self.tokio.spawn(async move {
            tx.send(connect(name, ticket, tokio, joined).await).ok();
        });
        self.connecting = Some(rx);
        self.status = "connecting...".into();
    }
}

/// The start screen.
#[derive(Component)]
struct StartScreen;

/// A line of the start screen; all but the status take clicks.
#[derive(Component, Clone, Copy, PartialEq)]
enum FormLine {
    Name,
    Create,
    Ticket,
    Join,
    Status,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let rt = tokio::runtime::Runtime::new().std_context("starting tokio")?;
    let mut launch = Launch {
        tokio: rt.handle().clone(),
        joined: Default::default(),
        connecting: None,
        name: args.name.clone().unwrap_or_default(),
        ticket: String::new(),
        on_ticket: false,
        status: "Tab: next field   Enter: go   Ctrl+V: paste".into(),
        video_cube: args.video_cube,
    };
    if args.ticket.is_some() || args.name.is_some() {
        launch.connect(args.ticket.clone());
    }
    let joined = launch.joined.clone();
    let editor = Editor {
        next_id: 1,
        sync: Timer::new(SYNC_INTERVAL, TimerMode::Repeating),
        ..Default::default()
    };

    // Bevy runs its systems on its own threads, which use `Net::tokio`; this
    // guard covers the main thread.
    let _guard = rt.enter();
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "iroh-live editor".into(),
                        ..default()
                    }),
                    ..default()
                })
                .set(LogPlugin {
                    filter: "wgpu=error,naga=warn,iroh=warn,iroh_quinn=warn,iroh_relay=warn,\
                             iroh_gossip=warn,iroh_smol_kv=warn,arboard=error"
                        .into(),
                    ..default()
                }),
        )
        .add_plugins(VideoTexturePlugin {
            cpu_only: args.cpu_video,
        })
        .insert_resource(launch)
        .insert_resource(editor)
        .init_resource::<Pointer>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (start_form, poll_launch)
                .chain()
                .run_if(resource_exists::<Launch>),
        )
        .add_systems(
            Update,
            (
                (keyboard, mouse, network).chain(),
                (
                    glide,
                    publish_state,
                    apply_looks,
                    set_volumes,
                    draw_outlines,
                    hud,
                )
                    .after(network),
            )
                .run_if(resource_exists::<Net>),
        )
        .run();

    if let Some((room, router)) = joined.get() {
        let shutdown = async {
            room.leave().await;
            router.shutdown().await.ok();
        };
        rt.block_on(tokio::time::timeout(Duration::from_secs(2), shutdown))
            .ok();
    }
    Ok(())
}

/// Binds a node, joins the room, publishes this member's world and opens the speaker.
async fn connect(
    name: Option<String>,
    ticket: Option<RoomTicket>,
    tokio: runtime::Handle,
    joined: Arc<OnceLock<(Room, Router)>>,
) -> Result<Net> {
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(
        endpoint.clone(),
        iroh_live::layout::config((LiveLayout::default(), RoomsLayout)),
    );
    let rooms = Rooms::new(&node);
    let router = rooms.mount(node.mount(Router::builder(endpoint))).spawn();
    let ticket = ticket.unwrap_or_else(RoomTicket::generate);
    let room = rooms.join(&ticket, name).await?;

    let world: broadcast::Producer = room.publish(WORLD)?;
    let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
    let track = world
        .create_track(STATE, info)
        .std_context("creating the state track")?;
    let state = ValueWriter::new(track, Default::default());
    state
        .update(&MemberState::default())
        .std_context("publishing the cubes")?;

    let speaker = Engine::open(Default::default())
        .await
        .inspect_err(|err| warn!(error = %err, "no audio output, the cubes play silently"))
        .ok();
    let chat = Chat::new(&room)?;
    let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
    let follow = tokio::spawn(follow(room.clone(), events_tx.clone()));
    info!(ticket = %room.ticket(), "joined the room");
    joined.set((room.clone(), router)).ok();
    Ok(Net {
        tokio,
        ticket: room.ticket().to_string(),
        room,
        chat,
        speaker,
        state,
        events,
        events_tx,
        _world: world,
        _follow: AbortOnDropHandle::new(follow),
    })
}

/// Reports the membership, and keeps one world reader per member that has one.
async fn follow(room: Room, tx: mpsc::Sender<NetEvent>) {
    let mut state = room.state();
    let mut readers: HashMap<EndpointId, AbortOnDropHandle<()>> = HashMap::new();
    loop {
        let members = state.get().members;
        let names = members
            .iter()
            .map(|(id, member)| {
                let name = member.display_name.clone();
                (*id, name.unwrap_or_else(|| id.fmt_short().to_string()))
            })
            .collect();
        tx.send(NetEvent::Members(names)).await.ok();
        let has_world = |id: &EndpointId| {
            members
                .get(id)
                .is_some_and(|m| m.broadcasts.contains(WORLD))
        };
        let gone: Vec<_> = readers
            .keys()
            .filter(|id| !has_world(id))
            .copied()
            .collect();
        for id in gone {
            readers.remove(&id);
            tx.send(NetEvent::State(id, MemberState::default()))
                .await
                .ok();
        }
        for id in members.keys().filter(|id| has_world(id)) {
            readers.entry(*id).or_insert_with(|| {
                let (room, peer, tx) = (room.clone(), *id, tx.clone());
                let task = retry(tx, move |tx| read_world(room.clone(), peer, tx));
                AbortOnDropHandle::new(tokio::spawn(task))
            });
        }
        if tx.is_closed() || state.updated().await.is_err() {
            return;
        }
    }
}

/// Runs `attempt` again each time it ends, until the frame loop is gone.
async fn retry<F>(tx: mpsc::Sender<NetEvent>, mut attempt: impl FnMut(mpsc::Sender<NetEvent>) -> F)
where
    F: Future<Output = Result<()>>,
{
    while !tx.is_closed() {
        if let Err(err) = attempt(tx.clone()).await {
            debug!(error = %err, "a read ended");
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Reads member `peer`'s cubes into `tx`.
async fn read_world(room: Room, peer: EndpointId, tx: mpsc::Sender<NetEvent>) -> Result<()> {
    let subscription = room.subscribe(peer, WORLD).await?;
    let mut reader = subscription.value_track::<MemberState>(STATE)?;
    while let Some(state) = reader.latest().await.std_context("reading the cubes")? {
        if tx.send(NetEvent::State(peer, state.value)).await.is_err() {
            break;
        }
    }
    Ok(())
}

/// Returns how every cube plays: with its sound on the speaker, if there is one.
fn player_config(speaker: Option<Engine>) -> PlayerConfig {
    PlayerConfig::default().with_audio(speaker.map_or(AudioMode::Off, AudioMode::Play))
}

/// Plays member `peer`'s video broadcast `name` onto `entity`, until it closes.
async fn play_video(
    room: Room,
    speaker: Option<Engine>,
    peer: EndpointId,
    name: String,
    entity: Entity,
    tx: mpsc::Sender<NetEvent>,
) -> Result<()> {
    let subscription = room.subscribe(peer, &name).await?;
    let player = subscription
        .remote_broadcast()
        .play(player_config(speaker))?;
    let video = NetEvent::Video(entity, name, Box::new(player), None);
    if tx.send(video).await.is_ok() {
        subscription.broadcast().closed().await;
    }
    Ok(())
}

/// Opens `source`, publishes it into the room as `name`, and plays it back.
///
/// The owner plays its own broadcast, so it sees and hears what the others
/// do. A file goes out as it is stored, sound included, and starts over at its
/// end. The test pattern beeps once a second, which makes the volume easy to
/// follow.
async fn publish_video(
    room: Room,
    speaker: Option<Engine>,
    source: Source,
    name: String,
    entity: Entity,
    tx: mpsc::Sender<NetEvent>,
) -> Result<()> {
    let broadcast: LocalBroadcast = room.publish(&name)?;
    match source {
        Source::File(path) => {
            let file = MediaFile::open(&path).await?.with_end(FileEnd::Restart);
            broadcast.set_encoded_file(&file)?;
        }
        Source::Spec(spec) => {
            let spec: VideoSourceSpec = spec.parse().std_context("parsing the video spec")?;
            let options = VideoOpenOptions::default()
                .with_size(video::Size::new(640, 360))
                .with_portal_grants("iroh-live-editor");
            let video = spec.open(&options).await?.std_context("no video")?;
            broadcast.set_video(video, VideoEncoding::single(VideoRendition::p360()))?;
            if matches!(spec, VideoSourceSpec::Test(_)) {
                let beeps = AudioSourceSpec::Test(TestTone::Beeps).open().await?;
                let beeps = beeps.std_context("no test tone")?;
                broadcast.set_audio(beeps, audio_encoding::music())?;
            }
        }
    }
    let player =
        RemoteBroadcast::from_consumer(broadcast.consume()).play(player_config(speaker))?;
    let player = Box::new(player);
    tx.send(NetEvent::Video(entity, name, player, Some(broadcast)))
        .await
        .ok();
    Ok(())
}

/// Asks for an MP4 file in the desktop's file dialog, for own cube `entity`.
async fn pick_file(entity: Entity, tx: mpsc::Sender<NetEvent>) {
    let file = rfd::AsyncFileDialog::new()
        .set_title("A video for the cube")
        .add_filter("MP4 video", &["mp4", "m4v", "mov"])
        .pick_file()
        .await;
    if let Some(file) = file {
        let path = file.path().to_path_buf();
        tx.send(NetEvent::Picked(entity, path)).await.ok();
    }
}

fn setup(
    mut commands: Commands,
    mut editor: ResMut<Editor>,
    launch: Res<Launch>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    editor.mesh = meshes.add(Cuboid::default());
    let orbit = Orbit {
        focus: Vec3::ZERO,
        yaw: 0.5,
        pitch: -0.45,
        distance: 13.0,
    };
    commands.spawn((Camera3d::default(), orbit.transform(), orbit));
    commands.spawn((
        DirectionalLight {
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 10.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(40.0, 40.0))),
        MeshMaterial3d(materials.add(Color::srgb(0.18, 0.19, 0.21))),
        Transform::from_xyz(0.0, -0.01, 0.0),
    ));
    let panel = |hud, top, bottom| {
        let at = bevy::ui::Node {
            position_type: PositionType::Absolute,
            left: px(10),
            top,
            bottom,
            ..default()
        };
        (hud, Text::default(), TextFont::from_font_size(14.0), at)
    };
    commands.spawn(panel(Hud::Room, px(10), Val::Auto));
    commands.spawn(panel(Hud::Selection, Val::Auto, px(10)));
    if launch.connecting.is_some() {
        return;
    }
    let screen = bevy::ui::Node {
        width: percent(100),
        height: percent(100),
        flex_direction: FlexDirection::Column,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        row_gap: px(6),
        ..default()
    };
    let shade = BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.75));
    commands
        .spawn((StartScreen, screen, shade))
        .with_children(|ui| {
            let title = TextFont::from_font_size(22.0);
            ui.spawn((Text::new("iroh-live editor\n"), title));
            let (name, ticket, status) = (FormLine::Name, FormLine::Ticket, FormLine::Status);
            for line in [name, FormLine::Create, ticket, FormLine::Join, status] {
                let padding = UiRect::axes(px(8), px(3));
                let pad = bevy::ui::Node {
                    padding,
                    ..default()
                };
                ui.spawn((line, Text::default(), BackgroundColor::DEFAULT, pad));
            }
        });
}

/// Types `key` into `line`: text, Backspace, and Ctrl+V to paste.
fn type_into(
    line: &mut String,
    key: &KeyboardInput,
    held: &ButtonInput<KeyCode>,
    clip: &mut Clipboard,
) {
    let ctrl = held.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    let text = match (ctrl, key.key_code) {
        (true, KeyCode::KeyV) => clip.fetch_text().poll_result().and_then(|text| {
            text.inspect_err(|err| warn!(error = %err, "could not paste"))
                .ok()
        }),
        (false, KeyCode::Backspace) => {
            line.pop();
            None
        }
        (false, _) => key.text.as_ref().map(ToString::to_string),
        (true, _) => None,
    };
    line.extend(text.unwrap_or_default().chars().filter(|c| !c.is_control()));
}

/// The start screen: typing into its fields, Tab, Enter, and clicks.
fn start_form(
    mut launch: ResMut<Launch>,
    mut keys: MessageReader<KeyboardInput>,
    mut clicks: MessageReader<PointerClick>,
    held: Res<ButtonInput<KeyCode>>,
    mut clipboard: ResMut<Clipboard>,
    mut lines: Query<(&FormLine, &mut Text, &mut BackgroundColor)>,
) {
    if launch.connecting.is_some() {
        keys.clear();
        clicks.clear();
    }
    // Whether to join a room, or to open one.
    let mut go = None;
    for key in keys.read().filter(|key| key.state == ButtonState::Pressed) {
        let launch = &mut *launch;
        match key.logical_key {
            Key::Enter => go = Some(launch.on_ticket),
            Key::Tab => launch.on_ticket = !launch.on_ticket,
            _ if launch.on_ticket => type_into(&mut launch.ticket, key, &held, &mut clipboard),
            _ => type_into(&mut launch.name, key, &held, &mut clipboard),
        }
    }
    for click in clicks.read() {
        match lines.get(click.entity).map(|(line, ..)| *line) {
            Ok(FormLine::Name) => launch.on_ticket = false,
            Ok(FormLine::Ticket) => launch.on_ticket = true,
            Ok(FormLine::Create) => go = Some(false),
            Ok(FormLine::Join) => go = Some(true),
            _ => {}
        }
    }
    match go {
        Some(false) => launch.connect(None),
        Some(true) => match launch.ticket.trim().parse::<RoomTicket>() {
            Ok(ticket) => launch.connect(Some(ticket)),
            Err(err) => launch.status = format!("not a room ticket: {err}"),
        },
        None => {}
    }
    let (on_ticket, cursor) = (launch.on_ticket, |on: bool| if on { "_" } else { "" });
    // The end of a long ticket, which is where the typing happens.
    let (name, ticket) = (&launch.name, &launch.ticket);
    let ticket = ticket
        .get(ticket.len().saturating_sub(40)..)
        .unwrap_or(ticket);
    for (line, mut text, mut background) in &mut lines {
        let (out, lit) = match line {
            FormLine::Name => (format!("name    {name}{}", cursor(!on_ticket)), !on_ticket),
            FormLine::Create => ("[ create room ]".into(), false),
            FormLine::Ticket => (format!("ticket  {ticket}{}", cursor(on_ticket)), on_ticket),
            FormLine::Join => ("[ join room ]".into(), false),
            FormLine::Status => (format!("\n{}", launch.status), false),
        };
        if text.0 != out {
            text.0 = out;
        }
        let lit = Color::srgba(1.0, 1.0, 1.0, if lit { 0.15 } else { 0.0 });
        background.set_if_neq(BackgroundColor(lit));
    }
}

/// Takes the connection once it is up: into the editor, or back to the form if it failed.
fn poll_launch(
    mut commands: Commands,
    mut launch: ResMut<Launch>,
    mut editor: ResMut<Editor>,
    screen: Query<Entity, With<StartScreen>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(Ok(result)) = launch.connecting.as_mut().map(|rx| rx.try_recv()) else {
        return;
    };
    launch.connecting = None;
    let net = match result {
        Ok(net) => net,
        Err(err) if screen.is_empty() => {
            error!(error = %err, "could not join the room");
            exit.write(AppExit::error());
            return;
        }
        Err(err) => {
            launch.status = format!("could not connect: {err}");
            return;
        }
    };
    println!("room ticket: {}", net.ticket);
    for entity in &screen {
        commands.entity(entity).despawn();
    }
    if launch.video_cube {
        let at = Transform::from_xyz(0.0, 0.5, 0.0);
        let (_, label, spec) = SOURCES[2];
        let video = spec.map(|spec| (label.to_string(), Source::Spec(spec)));
        spawn_own(&mut commands, &mut editor, &net, at, video, &mut materials);
    }
    commands.insert_resource(net);
    commands.remove_resource::<Launch>();
}

/// The editor keys, or the chat line while it has the keyboard.
#[allow(clippy::too_many_arguments, reason = "a Bevy system")]
fn keyboard(
    mut commands: Commands,
    mut keys: MessageReader<KeyboardInput>,
    held: Res<ButtonInput<KeyCode>>,
    mut clipboard: ResMut<Clipboard>,
    time: Res<Time>,
    mut editor: ResMut<Editor>,
    net: Res<Net>,
    orbit: Single<&Orbit>,
    mut cubes: Query<(&mut Cube, &mut Transform, Has<Muted>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for key in keys.read() {
        if key.state != ButtonState::Pressed {
            continue;
        }
        if let Some(line) = &mut editor.typing {
            match &key.logical_key {
                Key::Enter => {
                    let line = editor.typing.take().unwrap_or_default();
                    if !line.trim().is_empty() {
                        net.chat.send(line.clone());
                        push_chat(&mut editor, format!("me: {line}"));
                    }
                }
                Key::Escape => editor.typing = None,
                _ => type_into(line, key, &held, &mut clipboard),
            }
            continue;
        }
        if key.repeat {
            continue;
        }
        match key.key_code {
            KeyCode::Enter => editor.typing = Some(String::new()),
            KeyCode::KeyC => {
                let note = match clipboard.set_text(net.ticket.as_str()) {
                    Ok(()) => "ticket copied".to_string(),
                    Err(err) => format!("could not copy: {err}"),
                };
                editor.note = Some((note, time.elapsed_secs() + 3.0));
            }
            KeyCode::KeyN => {
                let at = Transform::from_translation(orbit.focus.with_y(0.5));
                let entity = spawn_own(&mut commands, &mut editor, &net, at, None, &mut materials);
                editor.selected = Some(entity);
            }
            KeyCode::Escape => editor.selected = None,
            code => {
                let Some(entity) = editor.selected else {
                    continue;
                };
                let Ok((mut cube, mut transform, muted)) = cubes.get_mut(entity) else {
                    continue;
                };
                if code == KeyCode::KeyM {
                    if muted {
                        commands.entity(entity).remove::<Muted>();
                    } else {
                        commands.entity(entity).insert(Muted);
                    }
                }
                // The rest changes the cube, which only its owner may.
                if cube.owner.is_some() {
                    continue;
                }
                if let Some(&(_, label, spec)) = SOURCES.iter().find(|s| s.0 == code) {
                    let video = spec.map(|spec| (label.to_string(), Source::Spec(spec)));
                    switch_video(&mut commands, &mut editor, &net, entity, &mut cube, video);
                }
                match code {
                    KeyCode::KeyF => {
                        net.tokio.spawn(pick_file(entity, net.events_tx.clone()));
                    }
                    KeyCode::KeyX => cube.turns = (cube.turns + 1) % 4,
                    KeyCode::ArrowUp => transform.translation.y += 0.25,
                    KeyCode::ArrowDown => transform.translation.y -= 0.25,
                    KeyCode::ArrowLeft => transform.rotate_y(TURN_STEP),
                    KeyCode::ArrowRight => transform.rotate_y(-TURN_STEP),
                    KeyCode::Equal | KeyCode::NumpadAdd => transform.scale *= 1.25,
                    KeyCode::Minus | KeyCode::NumpadSubtract => transform.scale /= 1.25,
                    KeyCode::Delete | KeyCode::Backspace => commands.entity(entity).despawn(),
                    _ => {}
                }
            }
        }
    }
}

/// The mouse and WASD: moves the camera, picks a cube, moves an own cube.
///
/// A left press that stays within [`DRAG_THRESHOLD`] is a click, and selects
/// the cube under it or nothing. A left drag orbits, unless it started on an
/// own cube, which then follows the pointer over the ground. The right or
/// middle button pans, and the wheel zooms. WASD moves the camera along the
/// ground the way it faces, Q and E down and up, unless the chat is typed in.
#[allow(clippy::too_many_arguments, reason = "a Bevy system")]
fn mouse(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut pointer: ResMut<Pointer>,
    window: Single<&Window>,
    camera: Single<(&Camera, &GlobalTransform, &mut Transform, &mut Orbit)>,
    mut ray_cast: MeshRayCast,
    mut cubes: Query<(&Cube, &mut Transform), Without<Orbit>>,
    mut editor: ResMut<Editor>,
) {
    let (camera, view, mut camera_transform, mut orbit) = camera.into_inner();
    let cursor = window.cursor_position();
    let ray = cursor.and_then(|at| camera.viewport_to_world(view, at).ok());
    let delta = motion.delta;

    if buttons.just_pressed(MouseButton::Left) {
        let is_cube = |entity: Entity| cubes.contains(entity);
        let settings = MeshRayCastSettings::default().with_filter(&is_cube);
        let hit = ray.and_then(|ray| ray_cast.cast_ray(ray, &settings).first().map(|h| h.0));
        *pointer = Pointer {
            pressed_at: cursor,
            hit,
            ..default()
        };
        if let Some(entity) = hit
            && let Ok((cube, transform)) = cubes.get(entity)
            && cube.owner.is_none()
            && let Some(grab) = ray.and_then(|ray| ground_point(ray, transform.translation.y))
        {
            editor.selected = Some(entity);
            pointer.moving = Some((entity, (transform.translation - grab).with_y(0.0)));
        }
    }
    if buttons.pressed(MouseButton::Left) {
        if let (Some(from), Some(at)) = (pointer.pressed_at, cursor) {
            pointer.dragging |= from.distance(at) > DRAG_THRESHOLD;
        }
        match pointer.moving {
            _ if !pointer.dragging => {}
            Some((entity, offset)) => {
                if let Ok((_, mut transform)) = cubes.get_mut(entity)
                    && let Some(at) = ray.and_then(|ray| ground_point(ray, transform.translation.y))
                {
                    let y = transform.translation.y;
                    transform.translation = (at + offset).with_y(y);
                }
            }
            None => {
                orbit.yaw -= delta.x * 0.005;
                orbit.pitch = (orbit.pitch - delta.y * 0.005).clamp(-1.5, -0.02);
            }
        }
    }
    if buttons.just_released(MouseButton::Left) {
        if !pointer.dragging && pointer.moving.is_none() {
            editor.selected = pointer.hit;
        }
        *pointer = Pointer::default();
    }
    let (turn, distance) = (Quat::from_rotation_y(orbit.yaw), orbit.distance);
    if buttons.any_pressed([MouseButton::Right, MouseButton::Middle]) {
        // Pans over the ground, so the scene follows the pointer.
        let pan = turn * Vec3::new(-delta.x, 0.0, -delta.y);
        orbit.focus += pan * distance * 0.0015;
    }
    if editor.typing.is_none() {
        let axis =
            |plus, minus| f32::from(i8::from(keys.pressed(plus)) - i8::from(keys.pressed(minus)));
        let walk = Vec3::new(
            axis(KeyCode::KeyD, KeyCode::KeyA),
            axis(KeyCode::KeyE, KeyCode::KeyQ),
            axis(KeyCode::KeyS, KeyCode::KeyW),
        );
        orbit.focus += turn * walk * distance.max(4.0) * 0.6 * time.delta_secs();
    }
    let lines = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
    };
    orbit.distance = (orbit.distance * 0.9_f32.powf(lines)).clamp(2.0, 80.0);
    *camera_transform = orbit.transform();
}

/// Returns where `ray` meets the horizontal plane at height `y`.
fn ground_point(ray: Ray3d, y: f32) -> Option<Vec3> {
    let distance = ray.intersect_plane(Vec3::Y * y, InfinitePlane3d::new(Vec3::Y))?;
    Some(ray.get_point(distance))
}

/// Spawns a cube this member owns, showing `video` if given.
fn spawn_own(
    commands: &mut Commands,
    editor: &mut Editor,
    net: &Net,
    transform: Transform,
    video: Option<(String, Source)>,
    materials: &mut Assets<StandardMaterial>,
) -> Entity {
    let id = editor.next_id;
    editor.next_id += 1;
    // Golden-angle hues keep neighbouring cubes apart.
    let color = Color::hsl((id as f32 * 137.5) % 360.0, 0.6, 0.55).to_srgba();
    let color = [color.red, color.green, color.blue];
    let mut cube = Cube {
        owner: None,
        id,
        color,
        video: None,
        turns: 0,
    };
    let material = MeshMaterial3d(materials.add(StandardMaterial::default()));
    let entity = commands
        .spawn((Mesh3d(editor.mesh.clone()), material, transform))
        .id();
    switch_video(commands, editor, net, entity, &mut cube, video);
    commands.entity(entity).insert(cube);
    entity
}

/// Switches own cube `cube` to `video`, a label and a source, or to none.
///
/// The video is published under a fresh broadcast name, and the old one ends
/// when its [`VideoHold`] drops.
fn switch_video(
    commands: &mut Commands,
    editor: &mut Editor,
    net: &Net,
    entity: Entity,
    cube: &mut Cube,
    video: Option<(String, Source)>,
) {
    let Some((label, source)) = video else {
        cube.video = None;
        commands
            .entity(entity)
            .remove::<(VideoHold, VideoTexture)>();
        return;
    };
    editor.next_broadcast += 1;
    let broadcast = format!("cube-{}-{}", cube.id, editor.next_broadcast);
    cube.video = Some(CubeVideo {
        label: label.clone(),
        broadcast: broadcast.clone(),
    });
    let (room, speaker, tx) = (net.room.clone(), net.speaker.clone(), net.events_tx.clone());
    let name = broadcast.clone();
    let task = net.tokio.spawn(async move {
        if let Err(err) = publish_video(room, speaker, source, name, entity, tx).await {
            warn!(label, error = %err, "could not publish the video");
        }
    });
    VideoHold::start(commands, entity, broadcast, task);
}

/// Plays the video of remote cube `cube` on `entity`, or nothing if it has none.
fn play_remote(commands: &mut Commands, net: &Net, entity: Entity, cube: &Cube) {
    let (Some(peer), Some(video)) = (cube.owner, &cube.video) else {
        commands
            .entity(entity)
            .remove::<(VideoHold, VideoTexture)>();
        return;
    };
    let (room, speaker) = (net.room.clone(), net.speaker.clone());
    let name = video.broadcast.clone();
    let task = net.tokio.spawn(retry(net.events_tx.clone(), move |tx| {
        play_video(
            room.clone(),
            speaker.clone(),
            peer,
            name.clone(),
            entity,
            tx,
        )
    }));
    VideoHold::start(commands, entity, video.broadcast.clone(), task);
}

fn push_chat(editor: &mut Editor, line: String) {
    editor.chat_log.push_back(line);
    while editor.chat_log.len() > CHAT_LINES {
        editor.chat_log.pop_front();
    }
}

/// Applies what the network tasks sent: members, remote cubes, videos, files, chat.
fn network(
    mut commands: Commands,
    mut net: ResMut<Net>,
    mut editor: ResMut<Editor>,
    mut cubes: Query<&mut Cube>,
    mut holds: Query<&mut VideoHold>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = editor.mesh.clone();
    while let Some(message) = net.chat.try_recv() {
        let line = format!("{}: {}", editor.name(message.from), message.text);
        push_chat(&mut editor, line);
    }
    // Only a member's newest state counts. Applying two in one frame would
    // compare the second with components the first has not written yet.
    let mut states = BTreeMap::new();
    while let Ok(event) = net.events.try_recv() {
        match event {
            NetEvent::Members(members) => editor.members = members,
            NetEvent::State(owner, state) => {
                states.insert(owner, state);
            }
            NetEvent::Video(entity, broadcast, player, published) => {
                // A player for a cube deleted or switched meanwhile drops here.
                let Ok(mut hold) = holds.get_mut(entity) else {
                    continue;
                };
                if hold.broadcast != broadcast {
                    continue;
                }
                info!(broadcast, own = published.is_some(), "a video cube plays");
                let frames = VideoTexture::new(player.video_frames());
                commands.entity(entity).insert(frames);
                hold.player = Some(*player);
                hold.volume = -1.0;
                hold._published = published;
            }
            NetEvent::Picked(entity, path) => {
                if let Ok(mut cube) = cubes.get_mut(entity)
                    && cube.owner.is_none()
                {
                    let name = path.file_name().unwrap_or_default();
                    let video = (name.to_string_lossy().into_owned(), Source::File(path));
                    let (editor, net) = (&mut editor, &net);
                    switch_video(&mut commands, editor, net, entity, &mut cube, Some(video));
                }
            }
        }
    }
    for (owner, state) in states {
        // Cubes the owner no longer lists are gone.
        editor.remote.retain(|(from, id), entity| {
            let keep = *from != owner || state.cubes.iter().any(|(cube, _)| cube.id == *id);
            if !keep {
                commands.entity(*entity).despawn();
            }
            keep
        });
        for (cube, transform) in state.cubes {
            let cube = Cube {
                owner: Some(owner),
                ..cube
            };
            let key = (owner, cube.id);
            let previous = editor.remote.get(&key).and_then(|e| cubes.get(*e).ok());
            let previous = previous.map(|cube| (*cube).clone());
            let entity = *editor.remote.entry(key).or_insert_with(|| {
                info!(owner = %owner.fmt_short(), id = cube.id, "a remote cube appeared");
                let material = MeshMaterial3d(materials.add(StandardMaterial::default()));
                commands
                    .spawn((Mesh3d(mesh.clone()), material, transform))
                    .id()
            });
            if previous.as_ref().map(|p| &p.video) != Some(&cube.video) {
                play_remote(&mut commands, &net, entity, &cube);
            }
            if previous.as_ref() != Some(&cube) {
                commands.entity(entity).insert(cube);
            }
            commands.entity(entity).insert(Glide(transform));
        }
    }
}

/// Moves remote cubes smoothly toward where their owners put them.
fn glide(time: Res<Time>, mut cubes: Query<(&mut Transform, &Glide)>) {
    let t = 1.0 - (-12.0 * time.delta_secs()).exp();
    for (mut transform, Glide(target)) in &mut cubes {
        transform.translation = transform.translation.lerp(target.translation, t);
        transform.rotation = transform.rotation.slerp(target.rotation, t);
        transform.scale = transform.scale.lerp(target.scale, t);
    }
}

/// Publishes this member's cubes, at most every [`SYNC_INTERVAL`].
fn publish_state(
    time: Res<Time>,
    mut editor: ResMut<Editor>,
    net: Res<Net>,
    cubes: Query<(&Cube, &Transform)>,
) {
    if !editor.sync.tick(time.delta()).just_finished() {
        return;
    }
    let mut cubes: Vec<_> = cubes
        .iter()
        .filter(|(cube, _)| cube.owner.is_none())
        .map(|(cube, transform)| (cube.clone(), *transform))
        .collect();
    cubes.sort_by_key(|(cube, _)| cube.id);
    // Unchanged state writes nothing.
    if let Err(err) = net.state.update(&MemberState { cubes }) {
        warn!(error = %err, "could not publish the cubes");
    }
}

/// Sets the material of each changed cube.
///
/// A cube shows its color, or for a video white and unlit, with the picture
/// turned by the cube's quarter turns.
fn apply_looks(
    cubes: Query<(&Cube, &MeshMaterial3d<StandardMaterial>), Changed<Cube>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (cube, material) in &cubes {
        let Some(mut material) = materials.get_mut(&material.0) else {
            continue;
        };
        let [r, g, b] = cube.color;
        material.unlit = cube.video.is_some();
        material.base_color = if material.unlit {
            Color::WHITE
        } else {
            material.base_color_texture = None;
            Color::srgb(r, g, b)
        };
        let center = Affine2::from_translation(Vec2::splat(0.5));
        let turn = Affine2::from_angle(f32::from(cube.turns) * FRAC_PI_2);
        material.uv_transform = center * turn * center.inverse();
    }
}

/// Sets each video cube's volume by its distance to the camera, or to zero if muted.
fn set_volumes(
    camera: Single<&Transform, With<Orbit>>,
    mut cubes: Query<(&Transform, &mut VideoHold, Has<Muted>), Without<Orbit>>,
) {
    for (transform, mut hold, muted) in &mut cubes {
        let distance = transform.translation.distance(camera.translation);
        let volume = (FULL_VOLUME_DISTANCE / distance).min(1.0);
        let volume = if muted { 0.0 } else { volume };
        if (volume - hold.volume).abs() > 0.01
            && let Some(player) = &hold.player
        {
            player.set_volume(volume);
            hold.volume = volume;
        }
    }
}

/// Draws the floor grid and an outline around every cube.
fn draw_outlines(
    mut gizmos: Gizmos,
    editor: Res<Editor>,
    cubes: Query<(Entity, &Transform, &Cube)>,
) {
    gizmos.grid(
        Isometry3d::from_rotation(Quat::from_rotation_x(FRAC_PI_2)),
        UVec2::splat(40),
        Vec2::ONE,
        Color::srgba(1.0, 1.0, 1.0, 0.08),
    );
    for (entity, transform, cube) in &cubes {
        let color = match cube.owner {
            _ if editor.selected == Some(entity) => Color::srgb(1.0, 0.85, 0.2),
            None => Color::WHITE,
            // A color per member.
            Some(owner) => Color::hsl(f32::from(owner.as_bytes()[0]) / 255.0 * 360.0, 0.8, 0.6),
        };
        gizmos.cube(transform.with_scale(transform.scale * 1.02), color);
    }
}

/// Writes the panels: the room, the members and the chat at the top, the
/// selected cube and its keys at the bottom.
fn hud(
    time: Res<Time>,
    editor: Res<Editor>,
    net: Res<Net>,
    cubes: Query<(&Cube, Has<Muted>)>,
    mut texts: Query<(&Hud, &mut Text)>,
) {
    let note = editor
        .note
        .as_ref()
        .filter(|(_, until)| *until > time.elapsed_secs());
    let mut top = format!(
        "room {}...   C: copy ticket   {}\n\
         WASD: move   Q/E: down/up   drag: orbit   right drag: pan   wheel: zoom\n\
         click: select   N: new cube   Enter: chat\n",
        &net.ticket[..net.ticket.len().min(24)],
        note.map_or("", |(note, _)| note.as_str()),
    );
    top.push_str(&format!("\n{} others here\n", editor.members.len()));
    for name in editor.members.values() {
        top.push_str(&format!("  {name}\n"));
    }
    top.push('\n');
    for line in &editor.chat_log {
        top.push_str(&format!("{line}\n"));
    }
    if let Some(line) = &editor.typing {
        top.push_str(&format!("> {line}_\n"));
    }

    let mut bottom = String::new();
    if let Some((cube, muted)) = editor.selected.and_then(|entity| cubes.get(entity).ok()) {
        let label = cube.video.as_ref().map_or("none", |v| v.label.as_str());
        let mute = format!("M mute: {}\n", if muted { "on" } else { "off" });
        if let Some(owner) = cube.owner {
            let name = editor.name(owner);
            bottom = format!("cube {} of {name}\nvideo: {label}\n{mute}", cube.id);
        } else {
            bottom = format!("cube {}, yours\n", cube.id);
            let mut file = true;
            for (i, (_, name, _)) in SOURCES.iter().enumerate() {
                let mark = if *name == label { ">" } else { " " };
                file &= *name != label;
                bottom.push_str(&format!("{mark} {} {name}\n", i + 1));
            }
            if file {
                bottom.push_str(&format!("> F file: {label}\n"));
            } else {
                bottom.push_str("  F file...\n");
            }
            let turn = u32::from(cube.turns) * 90;
            bottom.push_str(&format!("X turn image: {turn} deg\n{mute}drag: move\n"));
            bottom.push_str("up/down: height\nleft/right: turn\n+/-: size\nDel: delete\n");
        }
        bottom.push_str("Esc: deselect");
    }
    for (hud, mut text) in &mut texts {
        let out = if *hud == Hud::Room { &top } else { &bottom };
        if text.0 != *out {
            text.0 = out.clone();
        }
    }
}
