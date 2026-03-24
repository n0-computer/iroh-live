use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, trace, warn};

use super::{AudioPosition, DecodeOpts, VIDEO_DECODE_AHEAD_PAUSE_THRESHOLD, forward_packets};
use crate::{
    format::{DecodeConfig, MediaPacket, VideoFrame},
    playout::{
        FrameDecision, PlayoutBuffer, RecvResult, SyncState, VideoSyncConfig, VideoSyncController,
    },
    traits::VideoDecoder,
    util::spawn_thread,
};

/// Standalone video decoder pipeline.
///
/// Reads encoded packets from any [`crate::transport::PacketSource`], decodes
/// on an OS thread, and outputs [`VideoFrame`]s via a single-slot channel.
#[derive(Debug)]
pub struct VideoDecoderPipeline {
    pub frames: VideoDecoderFrames,
    pub handle: VideoDecoderHandle,
}

/// Receiving end of decoded frames from a [`VideoDecoderPipeline`].
#[derive(Debug)]
pub struct VideoDecoderFrames {
    pub(crate) rx: crate::frame_channel::FrameReceiver<VideoFrame>,
}

/// Control handle for a [`VideoDecoderPipeline`].
#[derive(Debug)]
pub struct VideoDecoderHandle {
    rendition: String,
    decoder_name: String,
    pub(crate) viewport: n0_watcher::Watchable<(u32, u32)>,
    _guard: PipelineGuard,
}

/// Keeps decoder pipeline resources alive; dropping cancels everything.
#[derive(derive_more::Debug)]
struct PipelineGuard {
    #[debug(skip)]
    _shutdown_token_guard: tokio_util::sync::DropGuard,
    #[debug(skip)]
    _task_handle: AbortOnDropHandle<()>,
    #[debug(skip)]
    _thread_handle: std::thread::JoinHandle<()>,
}

impl VideoDecoderFrames {
    /// Returns the latest decoded frame, or `None` if no new frame
    /// has arrived since the last call.
    pub fn current_frame(&mut self) -> Option<VideoFrame> {
        self.rx.take()
    }

    /// Waits for the next frame. Returns `None` when the pipeline
    /// shuts down.
    pub async fn recv(&self) -> Option<VideoFrame> {
        self.rx.recv().await
    }

    /// Returns `true` if the decoder pipeline has shut down.
    pub fn is_closed(&self) -> bool {
        self.rx.is_closed()
    }

    /// Total frames decoded (including ones overwritten before the
    /// consumer could take them).
    pub fn produced(&self) -> u64 {
        self.rx.produced()
    }
}

impl VideoDecoderHandle {
    pub fn set_viewport(&self, w: u32, h: u32) {
        let _ = self.viewport.set((w, h));
    }

    pub fn rendition(&self) -> &str {
        &self.rendition
    }

    pub fn decoder_name(&self) -> &str {
        &self.decoder_name
    }
}

impl VideoDecoderPipeline {
    /// Creates a new decoder pipeline from any packet source.
    pub fn new<D: VideoDecoder>(
        name: String,
        source: impl crate::transport::PacketSource,
        config: &rusty_codecs::config::VideoConfig,
        decode_config: &DecodeConfig,
        opts: DecodeOpts,
    ) -> Result<Self> {
        let shutdown = CancellationToken::new();
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = crate::frame_channel::frame_channel();
        let viewport = n0_watcher::Watchable::new((1u32, 1u32));
        let viewport_watcher = viewport.watch();

        let decoder = D::new(config, decode_config)?;
        let decoder_name = decoder.name().to_string();
        let span = info_span!("videodec", %name, decoder = %decoder_name);

        let thread_name = format!("vdec-{name}");
        let decoder_name_for_handle = decoder_name;
        let framerate = config.framerate.unwrap_or_else(|| {
            warn!("catalog has no framerate, falling back to 30 fps");
            30.0
        });
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                info!("decode start");
                if let Err(err) = decode_loop(
                    &shutdown,
                    packet_rx,
                    frame_tx,
                    viewport_watcher,
                    decoder,
                    framerate,
                    opts,
                ) {
                    error!("decoder failed: {err:#}");
                }
                info!("decode stop");
                shutdown.cancel();
            }
        });

        let task = tokio::spawn(forward_packets(source, packet_tx));
        let guard = PipelineGuard {
            _shutdown_token_guard: shutdown.drop_guard(),
            _task_handle: AbortOnDropHandle::new(task),
            _thread_handle: thread,
        };

        Ok(Self {
            frames: VideoDecoderFrames { rx: frame_rx },
            handle: VideoDecoderHandle {
                rendition: name,
                decoder_name: decoder_name_for_handle,
                viewport,
                _guard: guard,
            },
        })
    }
}

/// The core video decode loop, running on an OS thread.
fn decode_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<MediaPacket>,
    output_tx: crate::frame_channel::FrameSender<VideoFrame>,
    mut viewport_watcher: n0_watcher::Direct<(u32, u32)>,
    mut decoder: impl VideoDecoder,
    framerate: f64,
    opts: DecodeOpts,
) -> Result<()> {
    let DecodeOpts {
        stats,
        audio_position,
        video_started,
        playback_policy,
        max_stale_duration_ms,
        skip_generation,
        ..
    } = opts;

    let mut sync =
        VideoSyncConfig::from_policy(&playback_policy, framerate).map(VideoSyncController::new);
    let playout_capacity = sync
        .as_ref()
        .map(|sync| sync.decode_pause_queue_len() + 2)
        .unwrap_or(8);
    let mut playout = PlayoutBuffer::new(playout_capacity);
    let mut state = VideoLoopState::new(Duration::from_secs_f64(1.0 / framerate.max(1.0)));

    const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        let packet = if let Some(wait) =
            maybe_pause_video_decode(&mut playout, &sync, &audio_position, &stats, &state)
        {
            if !wait.is_zero() {
                thread::sleep(wait);
            }
            None
        } else {
            let timeout =
                front_frame_wait(&playout, &sync, &audio_position, &state).unwrap_or(IDLE_TIMEOUT);
            match crate::playout::recv_timeout(&mut input_rx, timeout) {
                RecvResult::Value(pkt) => Some(pkt),
                RecvResult::Timeout => None,
                RecvResult::Disconnected => {
                    drain_remaining(
                        shutdown,
                        &mut playout,
                        &mut sync,
                        &audio_position,
                        &state,
                        &output_tx,
                    );
                    break;
                }
            }
        };

        let mut decode_ctx = VideoDecodeContext {
            sync: &mut sync,
            audio_position: &audio_position,
            playout: &mut playout,
            stats: &stats,
            skip_generation: &skip_generation,
            max_stale_duration_ms: &max_stale_duration_ms,
        };
        if let Some(pkt) = packet
            && decode_ctx.handle_packet(pkt, &mut state, &mut decoder, &mut viewport_watcher)
        {
            continue;
        }

        match take_ready_front_frame(&mut playout, &mut sync, &audio_position, &stats, &state) {
            FrontFrameAction::Idle | FrontFrameAction::Hold => {}
            FrontFrameAction::Render(frame) => {
                render_video_frame(frame, &mut state, &stats, &output_tx);
                video_started.store(true, Ordering::Release);
            }
        }

        record_video_status(&stats, &state, &playout, &sync, &audio_position);
        throttled_tracing::debug_every!(
            Duration::from_secs(5),
            buf_len = playout.len(),
            sync_state = sync
                .as_ref()
                .map(|sync| sync.state().as_str())
                .unwrap_or("unmanaged"),
            "decode_loop: status"
        );
    }
    Ok(())
}

/// Mutable state that belongs to one running video decode loop.
pub(super) struct VideoLoopState {
    waiting_for_keyframe: bool,
    skip_active: bool,
    last_send: Instant,
    last_video_pts: Option<Duration>,
    latest_received_video_pts: Option<Duration>,
    frame_interval: Duration,
}

impl VideoLoopState {
    pub(super) fn new(frame_interval: Duration) -> Self {
        Self {
            waiting_for_keyframe: false,
            skip_active: false,
            last_send: Instant::now(),
            last_video_pts: None,
            latest_received_video_pts: None,
            frame_interval,
        }
    }

    fn note_packet(&mut self, pts: Duration) {
        self.latest_received_video_pts = Some(
            self.latest_received_video_pts
                .map_or(pts, |prev| prev.max(pts)),
        );
    }

    fn note_render(&mut self, frame_pts: Duration, now: Instant) {
        self.last_video_pts = Some(frame_pts);
        self.last_send = now;
    }

    fn cadence_wait(&self, next_pts: Duration, sync_state: Option<SyncState>) -> Duration {
        let cadence_applies = sync_state.is_none_or(|state| state != SyncState::CatchUp);
        if !cadence_applies {
            return Duration::ZERO;
        }

        let Some(last_pts) = self.last_video_pts else {
            return Duration::ZERO;
        };
        let Some(delta_pts) = next_pts.checked_sub(last_pts) else {
            return Duration::ZERO;
        };
        let desired = self.last_send + delta_pts;
        desired.saturating_duration_since(Instant::now())
    }
}

/// Shared references used while decoding a single video packet.
struct VideoDecodeContext<'a> {
    sync: &'a mut Option<VideoSyncController>,
    audio_position: &'a AudioPosition,
    playout: &'a mut PlayoutBuffer,
    stats: &'a crate::stats::DecodeStats,
    skip_generation: &'a Arc<AtomicU64>,
    max_stale_duration_ms: &'a Arc<AtomicU64>,
}

impl VideoDecodeContext<'_> {
    /// Decodes one packet and updates skip / recovery state around it.
    fn handle_packet(
        &mut self,
        pkt: MediaPacket,
        state: &mut VideoLoopState,
        decoder: &mut impl VideoDecoder,
        viewport_watcher: &mut n0_watcher::Direct<(u32, u32)>,
    ) -> bool {
        let now = Instant::now();
        state.note_packet(pkt.timestamp);
        if let Some(sync) = self.sync.as_mut() {
            sync.note_arrival(now);
            sync.on_skip_generation(self.skip_generation.load(Ordering::Relaxed));
        }

        if self.should_skip_packet(&pkt, state) {
            drain_decoder(decoder, self.playout, self.stats);
            return true;
        }

        if state.waiting_for_keyframe && !self.accept_recovery_keyframe(&pkt, state) {
            return true;
        }

        if viewport_watcher.update() {
            let (w, h) = viewport_watcher.peek();
            decoder.set_viewport(*w, *h);
        }

        let started = Instant::now();
        if let Err(err) = decoder.push_packet(pkt) {
            warn!("push_packet failed, waiting for keyframe: {err:#}");
            if let Err(reset_err) = decoder.reset() {
                warn!("decoder reset after error failed: {reset_err:#}");
            }
            self.playout.clear();
            state.waiting_for_keyframe = true;
        } else {
            self.stats
                .render
                .decode_ms
                .record(started.elapsed().as_secs_f64() * 1000.0);
            drain_decoder(decoder, self.playout, self.stats);
        }

        false
    }

    /// Returns `true` when this packet is already too stale to help playout.
    fn should_skip_packet(&mut self, pkt: &MediaPacket, state: &mut VideoLoopState) -> bool {
        let threshold = Duration::from_millis(self.max_stale_duration_ms.load(Ordering::Relaxed));
        let decoder_late = self
            .audio_position
            .playback_pts()
            .map(|audio_pts| pkt.timestamp.saturating_add(threshold) < audio_pts)
            .unwrap_or(false);

        if state.waiting_for_keyframe || !decoder_late {
            state.skip_active = false;
            return false;
        }

        if pkt.is_keyframe {
            if state.skip_active {
                info!("decode_loop: skip complete, resuming at keyframe");
                self.playout.clear();
                self.skip_generation.fetch_add(1, Ordering::Relaxed);
                if let Some(sync) = self.sync.as_mut() {
                    sync.force_catchup();
                    sync.on_skip_generation(self.skip_generation.load(Ordering::Relaxed));
                }
                state.skip_active = false;
            }
            return false;
        }

        if !state.skip_active {
            info!("decode_loop: decoder behind, skipping to next keyframe");
            state.skip_active = true;
        }
        self.stats.timing.frames_skipped.record(1.0);
        true
    }

    /// Returns `true` once a fresh recovery keyframe has been accepted.
    fn accept_recovery_keyframe(&mut self, pkt: &MediaPacket, state: &mut VideoLoopState) -> bool {
        if !pkt.is_keyframe {
            trace!("skipping non-keyframe while waiting for recovery");
            return false;
        }

        info!("received keyframe, resuming decode");
        state.waiting_for_keyframe = false;
        self.playout.clear();
        if let Some(sync) = self.sync.as_mut() {
            sync.force_catchup();
        }
        true
    }
}

/// Timing facts about the current playout head.
struct FrontFrameStatus {
    wait: Duration,
    lead: Option<Duration>,
}

fn front_frame_status(
    playout: &PlayoutBuffer,
    sync: &Option<VideoSyncController>,
    audio_position: &AudioPosition,
    state: &VideoLoopState,
) -> Option<FrontFrameStatus> {
    let front = playout.front()?;
    let audio_pts = audio_position.playback_pts();
    let sync_wait = sync
        .as_ref()
        .map(|sync| sync.wait_hint(front.timestamp, audio_pts))
        .unwrap_or(Duration::ZERO);
    let cadence_wait = state.cadence_wait(front.timestamp, sync.as_ref().map(|sync| sync.state()));
    Some(FrontFrameStatus {
        wait: sync_wait.max(cadence_wait),
        lead: audio_pts.and_then(|audio_pts| front.timestamp.checked_sub(audio_pts)),
    })
}

fn front_frame_wait(
    playout: &PlayoutBuffer,
    sync: &Option<VideoSyncController>,
    audio_position: &AudioPosition,
    state: &VideoLoopState,
) -> Option<Duration> {
    front_frame_status(playout, sync, audio_position, state).map(|status| status.wait)
}

/// Decides whether decode should pause while playout is intentionally holding early video.
pub(super) fn maybe_pause_video_decode(
    playout: &mut PlayoutBuffer,
    sync: &Option<VideoSyncController>,
    audio_position: &AudioPosition,
    stats: &crate::stats::DecodeStats,
    state: &VideoLoopState,
) -> Option<Duration> {
    let status = front_frame_status(playout, sync, audio_position, state)?;
    let wait = status.wait;
    if wait.is_zero() {
        return None;
    }

    let sync = sync.as_ref()?;
    if playout.len() <= 1 {
        return None;
    }

    let lead = status.lead?;
    if lead <= VIDEO_DECODE_AHEAD_PAUSE_THRESHOLD {
        if playout.len() >= sync.decode_pause_queue_len() {
            return Some(wait);
        }
        return None;
    }

    let dropped = playout.trim_to_front();
    if dropped > 0 {
        stats.timing.frames_skipped.record(dropped as f64);
        debug!(
            lead_ms = lead.as_millis(),
            dropped, "video: trimmed future playout backlog while waiting for audio"
        );
    }

    Some(wait)
}

/// Outcome of inspecting the current front frame.
#[allow(
    clippy::large_enum_variant,
    reason = "avoid heap allocation in the render hot path"
)]
pub(super) enum FrontFrameAction {
    Idle,
    Hold,
    Render(VideoFrame),
}

/// Applies the sync controller decision to the current front frame.
pub(super) fn take_front_frame_action(
    playout: &mut PlayoutBuffer,
    sync: &mut Option<VideoSyncController>,
    audio_position: &AudioPosition,
    stats: &crate::stats::DecodeStats,
) -> FrontFrameAction {
    let Some(front) = playout.front() else {
        return FrontFrameAction::Idle;
    };
    let decision = if let Some(sync) = sync.as_mut() {
        sync.decide(
            front.timestamp,
            audio_position.playback_pts(),
            Instant::now(),
        )
    } else {
        FrameDecision::RenderNow
    };

    match decision {
        FrameDecision::Hold(wait) => {
            if let Some(sync) = sync.as_ref() {
                let audio_pts = audio_position.playback_pts();
                let delta_ms = audio_pts.map(|audio_pts| {
                    front.timestamp.as_micros() as f64 / 1000.0
                        - audio_pts.as_micros() as f64 / 1000.0
                });
                throttled_tracing::debug_every!(
                    Duration::from_secs(1),
                    sync_state = sync.state().as_str(),
                    front_pts_ms = front.timestamp.as_millis(),
                    audio_pts_ms = audio_pts.map(|pts| pts.as_millis()),
                    delta_ms = delta_ms.map(|delta| format!("{delta:.1}")),
                    wait_ms = wait.as_millis(),
                    "video playout holding front frame"
                );
            }
            FrontFrameAction::Hold
        }
        FrameDecision::RenderNow => match playout.pop_front() {
            Some(frame) => FrontFrameAction::Render(frame),
            None => FrontFrameAction::Idle,
        },
        FrameDecision::DropLate => {
            if let Some(sync) = sync.as_ref() {
                let audio_pts = audio_position.playback_pts();
                let delta_ms = audio_pts.map(|audio_pts| {
                    front.timestamp.as_micros() as f64 / 1000.0
                        - audio_pts.as_micros() as f64 / 1000.0
                });
                throttled_tracing::debug_every!(
                    Duration::from_secs(1),
                    sync_state = sync.state().as_str(),
                    front_pts_ms = front.timestamp.as_millis(),
                    audio_pts_ms = audio_pts.map(|pts| pts.as_millis()),
                    delta_ms = delta_ms.map(|delta| format!("{delta:.1}")),
                    "video playout dropping late front frame"
                );
            }
            let _ = playout.drop_front();
            stats.timing.late_frames_dropped.record(1.0);
            FrontFrameAction::Idle
        }
    }
}

/// Releases at most one renderable front frame, but may drop several stale frames first.
pub(super) fn take_ready_front_frame(
    playout: &mut PlayoutBuffer,
    sync: &mut Option<VideoSyncController>,
    audio_position: &AudioPosition,
    stats: &crate::stats::DecodeStats,
    state: &VideoLoopState,
) -> FrontFrameAction {
    loop {
        if front_frame_wait(playout, sync, audio_position, state)
            .is_some_and(|wait| !wait.is_zero())
        {
            return FrontFrameAction::Hold;
        }
        match take_front_frame_action(playout, sync, audio_position, stats) {
            FrontFrameAction::Idle if playout.front().is_some() => continue,
            action => return action,
        }
    }
}

fn render_video_frame(
    mut frame: VideoFrame,
    state: &mut VideoLoopState,
    stats: &crate::stats::DecodeStats,
    output_tx: &crate::frame_channel::FrameSender<VideoFrame>,
) {
    let now = Instant::now();
    let frame_pts = frame.timestamp;
    frame.timing.render_wall = Some(now);
    let gap = state.last_send.elapsed();
    if gap > state.frame_interval * 2 {
        stats.timing.freezes.record(1.0);
    }
    if gap >= Duration::from_millis(5) {
        stats.render.fps.record(1.0 / gap.as_secs_f64());
    }
    stats.timeline.push(crate::stats::FrameTimingEntry {
        kind: crate::stats::FrameKind::Video,
        pts: frame.timestamp,
        receive_wall: frame.timing.receive_wall.unwrap_or(now),
        decode_end: frame.timing.decode_end,
        render_wall: now,
        is_keyframe: false,
    });
    output_tx.send(frame);
    state.note_render(frame_pts, now);
}

fn record_video_status(
    stats: &crate::stats::DecodeStats,
    state: &VideoLoopState,
    playout: &PlayoutBuffer,
    sync: &Option<VideoSyncController>,
    audio_position: &AudioPosition,
) {
    let audio_pts = audio_position.playback_pts();
    let av_delta_ms = state
        .last_video_pts
        .zip(audio_pts)
        .map(|(video_pts, audio_pts)| {
            video_pts.as_micros() as f64 / 1000.0 - audio_pts.as_micros() as f64 / 1000.0
        });
    let video_live_lag = state
        .latest_received_video_pts
        .zip(state.last_video_pts)
        .map(|(latest_pts, rendered_pts)| latest_pts.saturating_sub(rendered_pts));
    let sync_state = sync
        .as_ref()
        .map(|sync| sync.state().as_str())
        .unwrap_or("unmanaged");
    stats.timing.record_video_status(
        playout.len(),
        front_frame_wait(playout, sync, audio_position, state),
        audio_position.buffered_duration(),
        av_delta_ms,
        video_live_lag,
        sync_state,
    );
}

fn drain_decoder(
    decoder: &mut impl VideoDecoder,
    buffer: &mut PlayoutBuffer,
    stats: &crate::stats::DecodeStats,
) {
    loop {
        match decoder.pop_frame() {
            Ok(Some(frame)) => {
                if buffer.push(frame) {
                    stats.timing.frames_skipped.record(1.0);
                }
            }
            Ok(None) => break,
            Err(err) => {
                warn!("pop_frame error during drain: {err:#}");
                break;
            }
        }
    }
}

fn drain_remaining(
    shutdown: &CancellationToken,
    playout: &mut PlayoutBuffer,
    sync: &mut Option<VideoSyncController>,
    audio_position: &AudioPosition,
    state: &VideoLoopState,
    output_tx: &crate::frame_channel::FrameSender<VideoFrame>,
) {
    let Some(wait) = front_frame_wait(playout, sync, audio_position, state) else {
        return;
    };
    if shutdown.is_cancelled() {
        return;
    }
    if !wait.is_zero() {
        playout.clear();
        return;
    }
    loop {
        match take_front_frame_action(
            playout,
            sync,
            audio_position,
            &crate::stats::DecodeStats::default(),
        ) {
            FrontFrameAction::Idle | FrontFrameAction::Hold => {
                playout.clear();
                return;
            }
            FrontFrameAction::Render(frame) => output_tx.send(frame),
        }
    }
}
