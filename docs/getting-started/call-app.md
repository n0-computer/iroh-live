# Tutorial: a video call app

This tutorial builds a desktop video call in one program: a window with the
peer's video and a preview of your camera, the microphone with echo
cancellation, the speaker, and buttons to answer and hang up. It builds on
the [library quickstart](library-quickstart.md), which explains the node and
the router.

## Add the dependencies

The window is [eframe](https://github.com/emilk/egui/tree/main/crates/eframe),
and `moq-ext-egui` draws the video in it. Echo cancellation needs the `aec`
feature:

```toml
[dependencies]
eframe = { version = "0.37", default-features = false, features = ["default_fonts", "wgpu", "wayland", "x11"] }
iroh = "1.4"
iroh-live = { git = "https://github.com/n0-computer/iroh-live", features = ["full", "aec"] }
moq-ext-egui = { git = "https://github.com/n0-computer/iroh-live" }
tokio = { version = "1", features = ["full"] }
```

eframe has to be the version whose `egui-wgpu` links the same wgpu as
iroh-live's renderer, 0.37 for this release. The `wayland` and `x11`
features matter on Linux only.

## The program

Run it on one machine with no argument. It prints its ticket, and the window
shows it too. Run it on a second machine with that ticket, and it calls the
first, which shows an answer button.

```rust,no_run
use std::task::Poll;

use eframe::egui;
use iroh::{EndpointId, Watcher as _, protocol::Router};
use iroh_live::{
    BroadcastTicket, EndpointConfig, Node,
    calls::{self, Call, Calls, HangUpReason},
    layout::LiveLayout,
    media::{
        AudioMode, AudioSource, LocalBroadcast, Player, PlayerConfig, SubscriptionExt,
        VideoEncoding, VideoRendition, VideoSource,
        audio::{self, playback::Engine},
        audio_encoding,
    },
};
use moq_ext_egui::{Pending, VideoView, egui_wgpu::RenderState};

type Error = Box<dyn std::error::Error + Send + Sync>;

fn main() -> Result<(), Error> {
    // Views and tasks spawn on the runtime, so the window runs inside it.
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let dial = std::env::args()
        .nth(1)
        .map(|t| t.parse::<BroadcastTicket>())
        .transpose()?;
    let (router, side) = runtime.block_on(start())?;
    println!("call me: {}", side.calls.ticket());

    let options = eframe::NativeOptions {
        // Vulkan with DMA-BUF import on Linux, and the adapter's own limits.
        wgpu_options: moq_ext_egui::egui_wgpu_config(),
        ..Default::default()
    };
    eframe::run_native(
        "call",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, side, dial)))),
    )
    .map_err(|err| err.to_string())?;
    runtime.block_on(router.shutdown())?;
    Ok(())
}

/// This node's side of every call.
struct Side {
    calls: Calls,
    broadcast: LocalBroadcast,
    speaker: Engine,
}

async fn start() -> Result<(Router, Side), Error> {
    let layout = LiveLayout::default();
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(endpoint.clone(), layout.config());
    // The peer dials in to read this side.
    let router = node.mount(Router::builder(endpoint)).spawn();
    let (calls, broadcast): (Calls, LocalBroadcast) = Calls::new(&node, &layout)?;

    let speaker = Engine::open(Default::default()).await?;
    let mut microphone = audio::capture::Config::default();
    // Removes what the speaker plays from the microphone, so the peer does
    // not hear itself.
    microphone.aec = Some(speaker.canceller(audio::aec::Config::default())?);
    broadcast.set_audio(
        AudioSource::microphone(microphone).await?,
        audio_encoding::voice(),
    )?;
    broadcast.set_video(
        VideoSource::camera(None).await?,
        VideoEncoding::single(VideoRendition::p720()),
    )?;
    Ok((
        router,
        Side {
            calls,
            broadcast,
            speaker,
        },
    ))
}

enum State {
    Waiting(Pending<Option<EndpointId>>),
    Ringing(EndpointId),
    Connecting(Pending<Result<Call, calls::Error>>),
    // The player plays while this state lives. The `ended` task holds the
    // call, so dropping this state hangs up.
    InCall {
        _player: Player,
        view: VideoView,
        ended: Pending<HangUpReason>,
    },
}

fn wait(ctx: &egui::Context, calls: &Calls) -> State {
    let calls = calls.clone();
    State::Waiting(Pending::spawn(ctx, async move { calls.next_ring().await }))
}

struct App {
    side: Side,
    ticket: String,
    preview: VideoView,
    render_state: Option<RenderState>,
    state: State,
    note: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, side: Side, dial: Option<BroadcastTicket>) -> Self {
        let ctx = &cc.egui_ctx;
        // The broadcast hands back the camera it encodes.
        let camera = side.broadcast.video_source().map(|source| source.frames());
        let preview = VideoView::new(ctx, "preview", camera, cc.wgpu_render_state.as_ref());
        let state = match dial {
            Some(ticket) => {
                let calls = side.calls.clone();
                State::Connecting(Pending::spawn(ctx, async move {
                    calls.call(ticket.peer()).await
                }))
            }
            None => wait(ctx, &side.calls),
        };
        let ticket = side.calls.ticket().to_string();
        let render_state = cc.wgpu_render_state.clone();
        Self {
            side,
            ticket,
            preview,
            render_state,
            state,
            note: String::new(),
        }
    }

    /// Returns the next state once a task has finished.
    fn next(&mut self, ctx: &egui::Context) -> Option<State> {
        match &mut self.state {
            State::Waiting(ring) => match ring.poll() {
                Poll::Ready(peer) => peer.flatten().map(State::Ringing),
                Poll::Pending => None,
            },
            // The caller gave up before the user answered.
            State::Ringing(peer) => (!self.side.calls.ringing().get().contains(peer))
                .then(|| wait(ctx, &self.side.calls)),
            State::Connecting(attempt) => match attempt.poll() {
                Poll::Ready(Some(Ok(call))) => Some(self.connected(ctx, call)),
                Poll::Ready(Some(Err(err))) => {
                    self.note = format!("the call failed: {err}");
                    Some(wait(ctx, &self.side.calls))
                }
                _ => None,
            },
            State::InCall { ended, .. } => match ended.poll() {
                Poll::Ready(reason) => {
                    self.note = match reason {
                        Some(HangUpReason::Withdrawn) => "the peer hung up".into(),
                        _ => "the connection was lost".into(),
                    };
                    Some(wait(ctx, &self.side.calls))
                }
                Poll::Pending => None,
            },
        }
    }

    fn connected(&mut self, ctx: &egui::Context, call: Call) -> State {
        let config = PlayerConfig::default().with_audio(AudioMode::Play(self.side.speaker.clone()));
        match call.subscription().remote_broadcast().play(config) {
            Ok(player) => {
                self.note = format!("in a call with {}", call.peer().fmt_short());
                let view = VideoView::new(
                    ctx,
                    "peer",
                    player.video_frames(),
                    self.render_state.as_ref(),
                );
                let ended = Pending::spawn(ctx, async move { call.ended().await });
                State::InCall {
                    _player: player,
                    view,
                    ended,
                }
            }
            Err(err) => {
                self.note = format!("cannot play the peer: {err}");
                wait(ctx, &self.side.calls)
            }
        }
    }
}

impl eframe::App for App {
    // Runs on every frame, also while the window is hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(next) = self.next(ctx) {
            self.state = next;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let calls = &self.side.calls;
        ui.label(format!("call me: {}", self.ticket));
        let mut next = None;
        ui.horizontal(|ui| {
            match &self.state {
                State::Waiting(_) => {
                    ui.label("waiting for a call");
                }
                State::Ringing(peer) => {
                    if ui.button(format!("answer {}", peer.fmt_short())).clicked() {
                        let (calls, peer) = (calls.clone(), *peer);
                        let answer = Pending::spawn(&ctx, async move { calls.answer(peer).await });
                        next = Some(State::Connecting(answer));
                    }
                }
                State::Connecting(_) | State::InCall { .. } => {
                    if ui.button("hang up").clicked() {
                        next = Some(wait(&ctx, calls));
                    }
                }
            }
            ui.label(&self.note);
        });
        if let Some(next) = next {
            self.state = next;
        }
        ui.horizontal(|ui| {
            if let State::InCall { view, .. } = &mut self.state {
                ui.add(view.render().max_width(640.0));
            }
            ui.add(self.preview.render().max_width(240.0));
        });
    }
}
```

## How it works

**The side.** `Calls::new` publishes this node's side of a call at
`live/<endpoint id>/call`, visible to nobody, and returns it as a
`LocalBroadcast`. One side serves every call in turn, so the camera and the
preview keep running between calls. `VideoSource::camera(None)` opens the
default camera, and `LocalBroadcast::video_source()` hands it back for the
preview.

**Echo cancellation.** The speaker is a `playback::Engine`, and every player
that plays into it is mixed. `Engine::canceller` returns a canceller that
knows what the speaker plays, and the microphone's capture config takes it.
Without it, the peer hears its own voice come back from your speaker.

**Ringing and answering.** `Calls::call(peer)` rings the peer and resolves
once it answers. On the other side, `Calls::next_ring()` resolves when a peer
rings, and `Calls::answer(peer)` takes the call. `Calls::ringing()` lists
every peer that rings now, which the program uses to notice a caller who
gave up. `Call::ended()` resolves when either side hangs up or the
connection is lost, and dropping the `Call` hangs up.

**The window.** An egui update cannot await, so each dial, answer and wait
runs as a `Pending` task that repaints the window when it finishes, and
`logic` collects the result on the next frame. `VideoView` draws a stream of
decoded frames and wakes the window for each new one.
`moq_ext_egui::egui_wgpu_config()` sets up wgpu so hardware-decoded frames
reach the GPU without a copy on Linux.

## Where to go next

- [Make a one-to-one call](../how-to/call.md) covers the call convention and
  access control for calls.
- For more than two people, a room replaces `Calls`: each member publishes
  one broadcast to the room and plays the others'. See
  [Run a room](../how-to/rooms.md).
- [Publish a camera, screen or file](../how-to/publish.md) picks other
  cameras, the screen, or a source by name.
- [Watch a broadcast and render it](../how-to/watch.md) adds the stats
  overlay and other toolkits.
