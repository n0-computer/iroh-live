//! A shared scene editor: everyone in an iroh-live room places and moves cubes.
//!
//! Each member publishes its own cubes as one latest-value JSON track, the
//! `state` track of its `world` broadcast, and reads every other member's.
//! Only the owner moves a cube; the others see it glide to where the owner
//! put it. A video cube publishes its own broadcast, `cube-<id>`, which the
//! others play onto their copy of the cube. The chat is the room chat that
//! `irl room` speaks too.
//!
//! ```sh
//! cargo run --manifest-path moq-ext-bevy/Cargo.toml --example editor
//! # in a second terminal, with the ticket the first one printed
//! cargo run --manifest-path moq-ext-bevy/Cargo.toml --example editor -- <TICKET>
//! ```

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    f32::consts::FRAC_PI_2,
    time::Duration,
};

use bevy::{
    camera_controller::free_camera::{FreeCamera, FreeCameraPlugin, FreeCameraState},
    gizmos::transform_gizmo::{
        TransformGizmoCamera, TransformGizmoFocus, TransformGizmoMode, TransformGizmoPlugin,
        TransformGizmoSettings, TransformGizmoState,
    },
    input::{ButtonState, keyboard::Key, keyboard::KeyboardInput},
    log::LogPlugin,
    picking::{events::PointerClick, mesh_picking::MeshPickingPlugin, pointer::PointerButton},
    prelude::*,
};
use clap::Parser;
use iroh::{EndpointId, protocol::Router};
use iroh_live::{
    EndpointConfig, Node, ValueWriter,
    layout::LiveLayout,
    media::{
        LocalBroadcast, PlayerConfig, SubscriptionExt, VideoEncoding, VideoRendition,
        spec::{VideoOpenOptions, VideoSourceSpec},
        video,
    },
    moq_net::{self, broadcast},
    rooms::{Room, RoomTicket, Rooms, RoomsLayout, chat::Chat},
};
use moq_ext_bevy::{VideoFrames, VideoTexture, VideoTexturePlugin};
use n0_error::{Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::mpsc};
use tracing::{debug, info, warn};

/// The broadcast each member publishes its cubes on.
const WORLD: &str = "world";
/// The latest-value track on [`WORLD`] that holds a [`MemberState`].
const STATE: &str = "state";
/// How often this member's cubes are published, at most.
const SYNC_INTERVAL: Duration = Duration::from_millis(50);
/// How many network events wait for the frame loop.
const EVENT_QUEUE: usize = 256;
/// How long a reader waits before reading a member again.
const RETRY: Duration = Duration::from_secs(2);
/// How many chat lines the panel shows.
const CHAT_LINES: usize = 10;

#[derive(Debug, Parser)]
#[command(about = "Edit a scene of cubes together, over an iroh-live room")]
struct Args {
    /// The room to join. Without one, a new room is opened.
    ticket: Option<RoomTicket>,
    /// The name the others see.
    #[arg(long)]
    name: Option<String>,
    /// The video a cube placed with `V` shows, such as `test:bounce`, `cam` or `file:clip.mp4`.
    #[arg(long, default_value = "test:bounce")]
    video: String,
    /// Places this many cubes at the start.
    #[arg(long, default_value_t = 0)]
    cubes: u32,
    /// Places a video cube at the start.
    #[arg(long)]
    video_cube: bool,
}

/// What one member publishes: its cubes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct MemberState {
    cubes: Vec<CubeState>,
}

/// One cube, as its owner placed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CubeState {
    id: u32,
    translation: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
    color: [f32; 3],
    /// The source spec of a video cube, published as `cube-<id>`.
    video: Option<String>,
}

impl CubeState {
    fn transform(&self) -> Transform {
        Transform {
            translation: Vec3::from_array(self.translation),
            rotation: Quat::from_array(self.rotation).normalize(),
            scale: Vec3::from_array(self.scale),
        }
    }
}

/// The name of the broadcast that carries cube `id`'s video.
fn video_broadcast(id: u32) -> String {
    format!("cube-{id}")
}

/// What the network tasks hand to the frame loop.
enum NetEvent {
    /// The other members, by id, with the names to show.
    Members(BTreeMap<EndpointId, String>),
    /// A member's newest cubes.
    State(EndpointId, MemberState),
    /// A member left the room, with all its cubes.
    Left(EndpointId),
    /// A video for `entity`, with what must live as long as it plays.
    Video {
        entity: Entity,
        frames: VideoFrames,
        publication: Option<VideoPublication>,
    },
}

/// The connection to the room, and the channels to the network tasks.
#[derive(Resource)]
struct Net {
    tokio: Handle,
    room: Room,
    chat: Chat,
    state: ValueWriter<MemberState>,
    events: mpsc::Receiver<NetEvent>,
    events_tx: mpsc::Sender<NetEvent>,
    ticket: String,
    /// The broadcasts this member publishes, held open while the app runs.
    _world: broadcast::Producer,
    _follow: AbortOnDropHandle<()>,
}

/// The editor's own state.
#[derive(Resource, Default)]
struct Editor {
    next_id: u32,
    members: BTreeMap<EndpointId, String>,
    remote: HashMap<(EndpointId, u32), Entity>,
    chat_log: VecDeque<String>,
    /// The chat line being typed, while the chat has the keyboard.
    typing: Option<String>,
    sync: Timer,
    video_spec: String,
    /// The cubes to place at the start: plain ones, and whether a video one.
    initial: (u32, bool),
}

/// A cube this member owns and can move.
#[derive(Component)]
struct OwnCube {
    id: u32,
    color: [f32; 3],
    video: Option<String>,
}

/// A cube another member owns, and where it is heading.
#[derive(Component)]
struct RemoteCube {
    owner: EndpointId,
    target: Transform,
}

/// A local video broadcast, which stops when the cube is gone.
struct VideoPublication {
    tokio: Handle,
    room: Room,
    name: String,
    _broadcast: LocalBroadcast,
}

impl Drop for VideoPublication {
    fn drop(&mut self) {
        // Dropped on a Bevy thread, outside the runtime.
        let _guard = self.tokio.enter();
        let _ = self.room.unpublish(&self.name);
    }
}

/// What keeps a cube's video playing or published: the task, the publication.
#[derive(Component)]
struct VideoHold {
    _task: Option<AbortOnDropHandle<()>>,
    _publication: Option<VideoPublication>,
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
        video_spec: args.video.clone(),
        initial: (args.cubes, args.video_cube),
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
        .add_plugins((
            FreeCameraPlugin,
            MeshPickingPlugin,
            TransformGizmoPlugin,
            VideoTexturePlugin,
        ))
        .insert_resource(net)
        .insert_resource(editor)
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (keyboard, network, glide, publish_state, draw_outlines, hud),
        )
        .add_observer(select_on_click)
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

/// Binds a node, joins the room and publishes this member's world and chat.
async fn connect(args: &Args, tokio: Handle) -> Result<(Net, Router)> {
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

    let chat = Chat::new(&room)?;
    let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
    let follow = tokio::spawn(follow(room.clone(), events_tx.clone()));
    info!(ticket = %room.ticket(), "joined the room");
    let net = Net {
        tokio,
        ticket: room.ticket().to_string(),
        room,
        chat,
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
        if tx.send(NetEvent::Members(names)).await.is_err() {
            return;
        }
        let gone: Vec<_> = readers
            .keys()
            .filter(|id| {
                !members
                    .get(*id)
                    .is_some_and(|m| m.broadcasts.contains(WORLD))
            })
            .copied()
            .collect();
        for id in gone {
            readers.remove(&id);
            if tx.send(NetEvent::Left(id)).await.is_err() {
                return;
            }
        }
        for (id, member) in &members {
            if member.broadcasts.contains(WORLD) && !readers.contains_key(id) {
                let reader = tokio::spawn(read_world(room.clone(), *id, tx.clone()));
                readers.insert(*id, AbortOnDropHandle::new(reader));
            }
        }
        if state.updated().await.is_err() {
            return;
        }
    }
}

/// Reads member `peer`'s cubes into `tx`, again after each failure.
async fn read_world(room: Room, peer: EndpointId, tx: mpsc::Sender<NetEvent>) {
    loop {
        if let Err(err) = read_world_once(&room, peer, &tx).await {
            debug!(peer = %peer.fmt_short(), error = %err, "world read ended");
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(RETRY).await;
    }
}

async fn read_world_once(room: &Room, peer: EndpointId, tx: &mpsc::Sender<NetEvent>) -> Result<()> {
    let subscription = room.subscribe(peer, WORLD).await?;
    let mut reader = subscription.value_track::<MemberState>(STATE)?;
    while let Some(state) = reader.latest().await.std_context("reading the cubes")? {
        if tx.send(NetEvent::State(peer, state.value)).await.is_err() {
            break;
        }
    }
    Ok(())
}

/// Plays member `peer`'s video broadcast `name` onto `entity`, again after each end.
async fn play_video(
    room: Room,
    peer: EndpointId,
    name: String,
    entity: Entity,
    tx: mpsc::Sender<NetEvent>,
) {
    loop {
        let played = async {
            let subscription = room.subscribe(peer, &name).await?;
            let broadcast = subscription.remote_broadcast();
            let player = broadcast.play(PlayerConfig::default())?;
            let video = NetEvent::Video {
                entity,
                frames: player.video_frames(),
                publication: None,
            };
            if tx.send(video).await.is_ok() {
                // The player decodes for as long as it lives.
                subscription.broadcast().closed().await;
            }
            n0_error::Ok(())
        };
        if let Err(err) = played.await {
            debug!(peer = %peer.fmt_short(), name, error = %err, "video ended");
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Opens video source `spec` and publishes it into the room as `name`.
async fn publish_video(
    room: Room,
    spec: String,
    name: String,
    entity: Entity,
    tx: mpsc::Sender<NetEvent>,
) {
    let published = async {
        let spec: VideoSourceSpec = spec.parse().std_context("parsing the video spec")?;
        let options = VideoOpenOptions::default().with_size(video::Size::new(640, 360));
        let source = spec
            .open(&options)
            .await?
            .std_context("the spec names no video")?;
        let frames = source.frames();
        let broadcast: LocalBroadcast = room.publish(&name)?;
        broadcast.set_video(source, VideoEncoding::single(VideoRendition::p360()))?;
        let publication = VideoPublication {
            tokio: Handle::current(),
            room: room.clone(),
            name: name.clone(),
            _broadcast: broadcast,
        };
        tx.send(NetEvent::Video {
            entity,
            frames,
            publication: Some(publication),
        })
        .await
        .ok();
        n0_error::Ok(())
    };
    if let Err(err) = published.await {
        warn!(spec, error = %err, "could not publish the video");
    }
}

fn setup(
    mut commands: Commands,
    mut editor: ResMut<Editor>,
    net: Res<Net>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (cubes, video_cube) = editor.initial;
    let row = (0..cubes)
        .map(|_| None)
        .chain(video_cube.then(|| Some(editor.video_spec.clone())));
    for (i, video) in row.enumerate() {
        let transform = Transform::from_xyz(i as f32 * 1.5 - 2.0, 0.5, 0.0);
        spawn_own(
            &mut commands,
            &mut editor,
            &net,
            transform,
            video,
            &mut meshes,
            &mut materials,
        );
    }
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(6.0, 5.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y),
        FreeCamera::default(),
        // The gizmo renders through a camera of its own, so it needs to be told.
        TransformGizmoCamera,
    ));
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
        Pickable::IGNORE,
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
    mut settings: ResMut<TransformGizmoSettings>,
    camera: Single<(&Transform, &mut FreeCameraState), With<Camera3d>>,
    selected: Query<Entity, With<TransformGizmoFocus>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (camera, mut camera_state) = camera.into_inner();
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
            KeyCode::KeyN | KeyCode::KeyV => {
                let video = (key.key_code == KeyCode::KeyV).then(|| editor.video_spec.clone());
                let at = camera.translation + camera.forward() * 5.0;
                let transform = Transform::from_translation(at.with_y(at.y.max(0.5)));
                let entity = spawn_own(
                    &mut commands,
                    &mut editor,
                    &net,
                    transform,
                    video,
                    &mut meshes,
                    &mut materials,
                );
                select(&mut commands, &selected, entity);
            }
            KeyCode::Delete | KeyCode::Backspace | KeyCode::KeyX => {
                for entity in &selected {
                    commands.entity(entity).despawn();
                }
            }
            KeyCode::Escape => {
                for entity in &selected {
                    commands.entity(entity).remove::<TransformGizmoFocus>();
                }
            }
            KeyCode::Digit1 => settings.mode = TransformGizmoMode::Translate,
            KeyCode::Digit2 => settings.mode = TransformGizmoMode::Rotate,
            KeyCode::Digit3 => settings.mode = TransformGizmoMode::Scale,
            _ => {}
        }
    }
    // The camera's WASD would steal the typed letters.
    camera_state.enabled = editor.typing.is_none();
}

/// Spawns a cube this member owns, and starts its video if it has one.
fn spawn_own(
    commands: &mut Commands,
    editor: &mut Editor,
    net: &Net,
    transform: Transform,
    video: Option<String>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> Entity {
    let id = editor.next_id;
    editor.next_id += 1;
    // Golden-angle hues keep neighbouring cubes apart.
    let color = Color::hsl((id as f32 * 137.5) % 360.0, 0.6, 0.55).to_srgba();
    let color = [color.red, color.green, color.blue];
    let material = cube_material(color, video.is_some());
    let entity = commands.spawn((
        Mesh3d(meshes.add(Cuboid::default())),
        MeshMaterial3d(materials.add(material)),
        transform,
        OwnCube {
            id,
            color,
            video: video.clone(),
        },
    ));
    if let Some(spec) = video {
        let task = publish_video(
            net.room.clone(),
            spec,
            video_broadcast(id),
            entity.id(),
            net.events_tx.clone(),
        );
        // The task ends once the video is published, and hands the
        // publication to the cube.
        net.tokio.spawn(task);
    }
    entity.id()
}

/// A cube's material: its color, or white and unlit for a video to show.
fn cube_material(color: [f32; 3], video: bool) -> StandardMaterial {
    if video {
        StandardMaterial {
            base_color: Color::WHITE,
            unlit: true,
            ..default()
        }
    } else {
        Color::srgb(color[0], color[1], color[2]).into()
    }
}

/// Moves the transform gizmo to `entity`.
fn select(
    commands: &mut Commands,
    selected: &Query<Entity, With<TransformGizmoFocus>>,
    entity: Entity,
) {
    for previous in selected {
        commands.entity(previous).remove::<TransformGizmoFocus>();
    }
    commands.entity(entity).insert(TransformGizmoFocus);
}

/// Selects an own cube when it is clicked, unless the click was on the gizmo.
fn select_on_click(
    click: On<PointerClick>,
    mut commands: Commands,
    own: Query<(), With<OwnCube>>,
    selected: Query<Entity, With<TransformGizmoFocus>>,
    gizmo: Res<TransformGizmoState>,
) {
    if click.button != PointerButton::Primary || gizmo.active || gizmo.hovered_axis.is_some() {
        return;
    }
    if own.contains(click.entity) {
        select(&mut commands, &selected, click.entity);
    }
}

fn push_chat(editor: &mut Editor, line: String) {
    editor.chat_log.push_back(line);
    while editor.chat_log.len() > CHAT_LINES {
        editor.chat_log.pop_front();
    }
}

/// Applies what the network tasks sent: members, remote cubes, videos, chat.
fn network(
    mut commands: Commands,
    mut net: ResMut<Net>,
    mut editor: ResMut<Editor>,
    mut remote: Query<&mut RemoteCube>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    while let Some(message) = net.chat.try_recv() {
        let name = editor
            .members
            .get(&message.from)
            .cloned()
            .unwrap_or_else(|| message.from.fmt_short().to_string());
        push_chat(&mut editor, format!("{name}: {}", message.text));
    }
    while let Ok(event) = net.events.try_recv() {
        match event {
            NetEvent::Members(members) => editor.members = members,
            NetEvent::Left(owner) => {
                editor.remote.retain(|(from, _), entity| {
                    if *from == owner {
                        commands.entity(*entity).despawn();
                    }
                    *from != owner
                });
            }
            NetEvent::State(owner, state) => {
                // Cubes the owner no longer lists are gone.
                editor.remote.retain(|(from, id), entity| {
                    let keep = *from != owner || state.cubes.iter().any(|cube| cube.id == *id);
                    if !keep {
                        commands.entity(*entity).despawn();
                    }
                    keep
                });
                for cube in &state.cubes {
                    let key = (owner, cube.id);
                    if let Some(&entity) = editor.remote.get(&key) {
                        match remote.get_mut(entity) {
                            Ok(mut existing) => existing.target = cube.transform(),
                            // Spawned this frame, so not in the query yet.
                            Err(_) => {
                                commands.entity(entity).insert(RemoteCube {
                                    owner,
                                    target: cube.transform(),
                                });
                            }
                        }
                        continue;
                    }
                    info!(owner = %owner.fmt_short(), id = cube.id, video = ?cube.video, "a remote cube appeared");
                    let material = cube_material(cube.color, cube.video.is_some());
                    let mut entity = commands.spawn((
                        Mesh3d(meshes.add(Cuboid::default())),
                        MeshMaterial3d(materials.add(material)),
                        cube.transform(),
                        RemoteCube {
                            owner,
                            target: cube.transform(),
                        },
                    ));
                    if cube.video.is_some() {
                        let task = play_video(
                            net.room.clone(),
                            owner,
                            video_broadcast(cube.id),
                            entity.id(),
                            net.events_tx.clone(),
                        );
                        entity.insert(VideoHold {
                            _task: Some(AbortOnDropHandle::new(net.tokio.spawn(task))),
                            _publication: None,
                        });
                    }
                    editor.remote.insert(key, entity.id());
                }
            }
            NetEvent::Video {
                entity,
                frames,
                publication,
            } => {
                // A cube deleted meanwhile drops the publication here.
                if let Ok(mut cube) = commands.get_entity(entity) {
                    info!(remote = publication.is_none(), "a video cube plays");
                    cube.insert(VideoTexture::new(frames));
                    if let Some(publication) = publication {
                        cube.insert(VideoHold {
                            _task: None,
                            _publication: Some(publication),
                        });
                    }
                }
            }
        }
    }
}

/// Moves remote cubes smoothly toward where their owners put them.
fn glide(time: Res<Time>, mut cubes: Query<(&mut Transform, &RemoteCube)>) {
    let t = 1.0 - (-12.0 * time.delta_secs()).exp();
    for (mut transform, cube) in &mut cubes {
        transform.translation = transform.translation.lerp(cube.target.translation, t);
        transform.rotation = transform.rotation.slerp(cube.target.rotation, t);
        transform.scale = transform.scale.lerp(cube.target.scale, t);
    }
}

/// Publishes this member's cubes, at most every [`SYNC_INTERVAL`].
fn publish_state(
    time: Res<Time>,
    mut editor: ResMut<Editor>,
    net: Res<Net>,
    cubes: Query<(&OwnCube, &Transform)>,
) {
    if !editor.sync.tick(time.delta()).just_finished() {
        return;
    }
    let mut cubes: Vec<_> = cubes
        .iter()
        .map(|(cube, transform)| CubeState {
            id: cube.id,
            translation: transform.translation.to_array(),
            rotation: transform.rotation.to_array(),
            scale: transform.scale.to_array(),
            color: cube.color,
            video: cube.video.clone(),
        })
        .collect();
    cubes.sort_by_key(|cube| cube.id);
    // Unchanged state writes nothing.
    if let Err(err) = net.state.update(&MemberState { cubes }) {
        warn!(error = %err, "could not publish the cubes");
    }
}

/// Draws the floor grid and an outline around every cube.
fn draw_outlines(
    mut gizmos: Gizmos,
    own: Query<(&Transform, Has<TransformGizmoFocus>), With<OwnCube>>,
    remote: Query<(&Transform, &RemoteCube)>,
) {
    gizmos.grid(
        Isometry3d::from_rotation(Quat::from_rotation_x(FRAC_PI_2)),
        UVec2::splat(40),
        Vec2::ONE,
        Color::srgba(1.0, 1.0, 1.0, 0.08),
    );
    for (transform, selected) in &own {
        let color = if selected {
            Color::srgb(1.0, 0.85, 0.2)
        } else {
            Color::WHITE
        };
        gizmos.cube(transform.with_scale(transform.scale * 1.02), color);
    }
    for (transform, cube) in &remote {
        gizmos.cube(
            transform.with_scale(transform.scale * 1.02),
            member_color(cube.owner),
        );
    }
}

/// A color per member, for the outlines of its cubes.
fn member_color(id: EndpointId) -> Color {
    let hue = f32::from(id.as_bytes()[0]) / 255.0 * 360.0;
    Color::hsl(hue, 0.8, 0.6)
}

/// Writes the panel: the controls, the members, the chat.
fn hud(
    editor: Res<Editor>,
    net: Res<Net>,
    settings: Res<TransformGizmoSettings>,
    mut text: Single<&mut Text, With<Hud>>,
) {
    let mut out = String::new();
    let ticket = &net.ticket;
    out.push_str(&format!(
        "room {}…  (full ticket on stdout)\n",
        &ticket[..ticket.len().min(24)]
    ));
    out.push_str("right mouse: look   WASD/QE: fly   click: select own cube\n");
    out.push_str(&format!(
        "N: cube   V: video cube ({})   X/Del: delete   Esc: deselect\n",
        editor.video_spec
    ));
    out.push_str(&format!(
        "1/2/3: move/rotate/scale ({:?})   Enter: chat\n\n",
        settings.mode
    ));
    out.push_str(&format!("{} others here\n", editor.members.len()));
    for name in editor.members.values() {
        out.push_str(&format!("  {name}\n"));
    }
    out.push('\n');
    for line in &editor.chat_log {
        out.push_str(line);
        out.push('\n');
    }
    if let Some(line) = &editor.typing {
        out.push_str(&format!("> {line}_\n"));
    }
    if text.0 != out {
        text.0 = out;
    }
}
