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
//! ```sh
//! cargo run --manifest-path moq-ext-bevy/Cargo.toml --example editor
//! # in a second terminal, with the ticket the first one printed
//! cargo run --manifest-path moq-ext-bevy/Cargo.toml --example editor -- <TICKET>
//! ```

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    f32::consts::{FRAC_PI_2, PI},
    path::PathBuf,
    time::Duration,
};

use bevy::{
    input::{
        ButtonState,
        keyboard::{Key, KeyboardInput},
        mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit},
    },
    log::LogPlugin,
    math::Affine2,
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
use tokio::{runtime, sync::mpsc};
use tracing::{debug, info, warn};

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
    (KeyCode::Digit3, "test", Some("test:bounce")),
    (KeyCode::Digit4, "none", None),
];

#[derive(Debug, Parser)]
#[command(about = "Edit a scene of cubes together, over an iroh-live room")]
struct Args {
    /// The room to join. Without one, a new room is opened.
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
    /// The member who owns it, `None` for this member. A reader knows whose
    /// state it reads, so it is not sent.
    #[serde(skip)]
    owner: Option<EndpointId>,
    id: u32,
    color: [f32; 3],
    video: Option<CubeVideo>,
    /// Quarter turns of the picture on the faces.
    turns: u8,
}

/// The video on a cube: its source spec, and the broadcast that carries it.
///
/// Each switch of source publishes under a new name, so a reader never takes
/// the old video for the new one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CubeVideo {
    source: String,
    broadcast: String,
}

/// What the network tasks hand to the frame loop.
enum NetEvent {
    /// The other members, by id, with the names to show.
    Members(BTreeMap<EndpointId, String>),
    /// A member's newest cubes, or none once it left.
    State(EndpointId, MemberState),
    /// A player for `entity` of the named broadcast, and for an own cube the
    /// broadcast itself.
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
    sync: Timer,
    /// Whether to place a video cube at the start.
    video_cube: bool,
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
    /// Where the button went down, and the cube it went down on.
    pressed_at: Option<Vec2>,
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

/// What keeps a cube's video running: the task, the player, and for an own
/// cube the broadcast, which ends when it drops.
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

/// The text panel.
#[derive(Component)]
struct Hud;

fn main() -> Result<()> {
    let args = Args::parse();
    let rt = tokio::runtime::Runtime::new().std_context("starting tokio")?;
    let (net, router) = rt.block_on(connect(&args, rt.handle().clone()))?;
    println!("room ticket: {}", net.ticket);
    let room = net.room.clone();
    let editor = Editor {
        next_id: 1,
        sync: Timer::new(SYNC_INTERVAL, TimerMode::Repeating),
        video_cube: args.video_cube,
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
                             iroh_gossip=warn,iroh_smol_kv=warn"
                        .into(),
                    ..default()
                }),
        )
        .add_plugins(VideoTexturePlugin {
            cpu_only: args.cpu_video,
        })
        .insert_resource(net)
        .insert_resource(editor)
        .init_resource::<Pointer>()
        .add_systems(Startup, setup)
        .add_systems(Update, (keyboard, mouse, network).chain())
        .add_systems(
            Update,
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
        .run();

    rt.block_on(async {
        let shutdown = async {
            room.leave().await;
            router.shutdown().await.ok();
        };
        tokio::time::timeout(Duration::from_secs(2), shutdown)
            .await
            .ok();
    });
    Ok(())
}

/// Binds a node, joins the room, publishes this member's world and opens the speaker.
async fn connect(args: &Args, tokio: runtime::Handle) -> Result<(Net, Router)> {
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(
        endpoint.clone(),
        iroh_live::layout::config((LiveLayout::default(), RoomsLayout)),
    );
    let rooms = Rooms::new(&node);
    let router = rooms.mount(node.mount(Router::builder(endpoint))).spawn();
    let ticket = args.ticket.clone().unwrap_or_else(RoomTicket::generate);
    let room = rooms.join(&ticket, args.name.clone()).await?;

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
    let net = Net {
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
    };
    Ok((net, router))
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

/// Opens the source `video` names, publishes it into the room, and plays it back.
///
/// The owner plays its own broadcast, so it sees and hears what the others
/// do. A file goes out as it is stored, sound included. The test pattern beeps
/// once a second, which makes the volume easy to follow.
async fn publish_video(
    room: Room,
    speaker: Option<Engine>,
    video: CubeVideo,
    entity: Entity,
    tx: mpsc::Sender<NetEvent>,
) -> Result<()> {
    let spec: VideoSourceSpec = video.source.parse().std_context("parsing the video spec")?;
    let broadcast: LocalBroadcast = room.publish(&video.broadcast)?;
    if let VideoSourceSpec::File { path, .. } = &spec {
        let file = MediaFile::open(path).await?.with_end(FileEnd::Restart);
        broadcast.set_encoded_file(&file)?;
    } else {
        let options = VideoOpenOptions::default()
            .with_size(video::Size::new(640, 360))
            .with_portal_grants("iroh-live-editor");
        let source = spec.open(&options).await?.std_context("no video")?;
        broadcast.set_video(source, VideoEncoding::single(VideoRendition::p360()))?;
        if matches!(spec, VideoSourceSpec::Test(_)) {
            let beeps = AudioSourceSpec::Test(TestTone::Beeps).open().await?;
            let beeps = beeps.std_context("no test tone")?;
            broadcast.set_audio(beeps, audio_encoding::music())?;
        }
    }
    let player =
        RemoteBroadcast::from_consumer(broadcast.consume()).play(player_config(speaker))?;
    let player = Box::new(player);
    let video = NetEvent::Video(entity, video.broadcast, player, Some(broadcast));
    tx.send(video).await.ok();
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
    net: Res<Net>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    editor.mesh = meshes.add(Cuboid::default());
    if editor.video_cube {
        let (at, test) = (Transform::from_xyz(0.0, 0.5, 0.0), SOURCES[2].2);
        spawn_own(&mut commands, &mut editor, &net, at, test, &mut materials);
    }
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
    commands.spawn((
        Hud,
        Text::default(),
        TextFont::from_font_size(14.0),
        bevy::ui::Node {
            position_type: PositionType::Absolute,
            top: px(10),
            left: px(10),
            ..default()
        },
    ));
}

/// The editor keys, or the chat line while it has the keyboard.
#[allow(clippy::too_many_arguments, reason = "a Bevy system")]
fn keyboard(
    mut commands: Commands,
    mut keys: MessageReader<KeyboardInput>,
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
                Key::Backspace => {
                    line.pop();
                }
                _ => {
                    if let Some(text) = &key.text {
                        line.extend(text.chars().filter(|c| !c.is_control()));
                    }
                }
            }
            continue;
        }
        if key.repeat {
            continue;
        }
        match key.key_code {
            KeyCode::Enter => editor.typing = Some(String::new()),
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
                if let Some(&(_, _, source)) = SOURCES.iter().find(|s| s.0 == code) {
                    let source = source.map(str::to_string);
                    switch_video(&mut commands, &mut editor, &net, entity, &mut cube, source);
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

/// The mouse: orbits, pans and zooms the camera, picks a cube, moves an own cube.
///
/// A left press that stays within [`DRAG_THRESHOLD`] is a click, and selects
/// the cube under it or nothing. A left drag orbits, unless it started on an
/// own cube, which then follows the pointer over the ground. The right or
/// middle button pans, and the wheel zooms.
#[allow(clippy::too_many_arguments, reason = "a Bevy system")]
fn mouse(
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
    if buttons.any_pressed([MouseButton::Right, MouseButton::Middle]) {
        // Pans over the ground, so the scene follows the pointer.
        let turn = Quat::from_rotation_y(orbit.yaw);
        let pan = turn * Vec3::new(-delta.x, 0.0, -delta.y);
        let pan = pan * orbit.distance * 0.0015;
        orbit.focus += pan;
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

/// Spawns a cube this member owns, showing `source` if given.
fn spawn_own(
    commands: &mut Commands,
    editor: &mut Editor,
    net: &Net,
    transform: Transform,
    source: Option<&str>,
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
    let source = source.map(Into::into);
    switch_video(commands, editor, net, entity, &mut cube, source);
    commands.entity(entity).insert(cube);
    entity
}

/// Switches own cube `cube` to `source`, published under a fresh broadcast name.
fn switch_video(
    commands: &mut Commands,
    editor: &mut Editor,
    net: &Net,
    entity: Entity,
    cube: &mut Cube,
    source: Option<String>,
) {
    cube.video = source.map(|source| {
        editor.next_broadcast += 1;
        let broadcast = format!("cube-{}-{}", cube.id, editor.next_broadcast);
        CubeVideo { source, broadcast }
    });
    start_video(commands, net, entity, cube);
}

/// Replaces what `entity` plays with the video `cube` names.
///
/// An own cube publishes the video, a remote one subscribes to it.
fn start_video(commands: &mut Commands, net: &Net, entity: Entity, cube: &Cube) {
    let mut target = commands.entity(entity);
    target.remove::<(VideoHold, VideoTexture)>();
    let Some(video) = cube.video.clone() else {
        return;
    };
    let (room, speaker, tx) = (net.room.clone(), net.speaker.clone(), net.events_tx.clone());
    let broadcast = video.broadcast.clone();
    let task = match cube.owner {
        None => net.tokio.spawn(async move {
            let source = video.source.clone();
            if let Err(err) = publish_video(room, speaker, video, entity, tx).await {
                warn!(source, error = %err, "could not publish the video");
            }
        }),
        Some(peer) => net.tokio.spawn(retry(tx, move |tx| {
            let name = video.broadcast.clone();
            play_video(room.clone(), speaker.clone(), peer, name, entity, tx)
        })),
    };
    target.insert(VideoHold {
        broadcast,
        player: None,
        volume: -1.0,
        _published: None,
        _task: AbortOnDropHandle::new(task),
    });
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
    while let Ok(event) = net.events.try_recv() {
        match event {
            NetEvent::Members(members) => editor.members = members,
            NetEvent::State(owner, state) => {
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
                        start_video(&mut commands, &net, entity, &cube);
                    }
                    if previous.as_ref() != Some(&cube) {
                        commands.entity(entity).insert(cube);
                    }
                    commands.entity(entity).insert(Glide(transform));
                }
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
                if published.is_some() {
                    hold._published = published;
                }
            }
            NetEvent::Picked(entity, path) => {
                if let Ok(mut cube) = cubes.get_mut(entity) {
                    let source = format!("file:{}:loop", path.display());
                    let (editor, net) = (&mut editor, &net);
                    switch_video(&mut commands, editor, net, entity, &mut cube, Some(source));
                }
            }
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

/// Sets the material of each changed cube: its color, or white and unlit for a
/// video, with the picture turned by the cube's quarter turns.
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

/// Writes the panel: the room, the keys for what is selected, the members, the chat.
fn hud(
    editor: Res<Editor>,
    net: Res<Net>,
    cubes: Query<(&Cube, Has<Muted>)>,
    mut text: Single<&mut Text, With<Hud>>,
) {
    let ticket = &net.ticket;
    let mut out = format!(
        "room {}...  (full ticket on stdout)\n\
         left drag: orbit   right/middle drag: pan   wheel: zoom\n",
        &ticket[..ticket.len().min(24)]
    );
    match editor.selected.and_then(|entity| cubes.get(entity).ok()) {
        None => out.push_str("click: select a cube   N: new cube   Enter: chat\n"),
        Some((cube, muted)) => {
            let owner = cube
                .owner
                .map_or_else(|| "yours".into(), |id| editor.name(id));
            // A file shows by its name.
            let source = cube.video.as_ref().map(|v| v.source.as_str());
            let label = source.map_or("none", |s| s.rsplit(['/', '\\']).next().unwrap_or(s));
            out.push_str(&format!(
                "\n> cube {} ({owner})   video: {label}   image: {} deg   sound: {}\n",
                cube.id,
                u32::from(cube.turns) * 90,
                if muted { "muted" } else { "on" },
            ));
            if cube.owner.is_none() {
                for (i, (_, name, spec)) in SOURCES.iter().enumerate() {
                    let mark = if *spec == source { "*" } else { " " };
                    out.push_str(&format!(" {mark}{} {name}", i + 1));
                }
                out.push_str("   F file\n");
                out.push_str("  drag: move   up/down: height   left/right: turn   +/-: size\n");
                out.push_str("  X: turn image   M: mute   Del: delete   Esc: deselect\n");
            } else {
                out.push_str("  M: mute   Esc: deselect   (the owner picks the video)\n");
            }
        }
    }
    out.push_str(&format!("\n{} others here\n", editor.members.len()));
    for name in editor.members.values() {
        out.push_str(&format!("  {name}\n"));
    }
    out.push('\n');
    for line in &editor.chat_log {
        out.push_str(&format!("{line}\n"));
    }
    if let Some(line) = &editor.typing {
        out.push_str(&format!("> {line}_\n"));
    }
    if text.0 != out {
        text.0 = out;
    }
}
