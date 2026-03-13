//! API sketch for the iroh-live ecosystem — wired to real types.
//!
//! Each example module uses real imports from the crates. Examples that require
//! unimplemented features (relay, room redesign) are gated behind `#[cfg(false)]`.
//!
//! Compile with:
//! ```sh
//! cargo check -p iroh-live --example api_sketch --all-features
//! ```

#![allow(
    unused,
    dead_code,
    unreachable_code,
    reason = "sketch example for API design"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

// ── Real imports from the iroh ecosystem ────────────────────────────────────

use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, protocol::Router};
use moq_lite::{BroadcastConsumer, BroadcastProducer};

// iroh-moq transport layer
use iroh_moq::{IncomingSession, IncomingSessionStream, Moq, MoqProtocolHandler, MoqSession};

// moq-media: publish + subscribe
use moq_media::{
    audio_backend::AudioBackend,
    publish::LocalBroadcast,
    subscribe::{AudioTrack, RemoteBroadcast, VideoOptions, VideoTarget, VideoTrack},
};

#[cfg(any_video_codec)]
use moq_media::{
    capture::{CameraCapturer, ScreenCapturer},
    codec::VideoCodec,
    format::VideoPreset,
};
#[cfg(any_audio_codec)]
use moq_media::{codec::AudioCodec, format::AudioPreset};

// iroh-live: high-level API
use iroh_live::{
    Call, CallError, DisconnectReason, Live, LiveBuilder, ParticipantId, TrackKind, TrackName,
};

type Result<T, E = anyhow::Error> = std::result::Result<T, E>;

// ═══════════════════════════════════════════════════════════════════════════════
// EXAMPLES — wired to real types
// ═══════════════════════════════════════════════════════════════════════════════

/// One-to-many streaming — publish from zero, subscribe from anywhere.
#[cfg(any_video_codec)]
mod ex_one_to_many {
    use super::*;

    async fn publish() -> Result<()> {
        let endpoint = Endpoint::builder()
            .secret_key(SecretKey::generate(&mut rand::rng()))
            .bind()
            .await?;
        let live = Live::builder(endpoint).spawn_with_router();

        let camera = CameraCapturer::new()?;
        let audio_ctx = AudioBackend::default();
        let mic = audio_ctx.default_input().await?;

        let broadcast = LocalBroadcast::new();
        broadcast.video().set(
            camera,
            VideoCodec::best_available(),
            [VideoPreset::P720, VideoPreset::P360],
        )?;
        #[cfg(any_audio_codec)]
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

        live.publish("my-stream", &broadcast).await?;
        println!("listening on {:?}", live.endpoint().id());

        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }

    #[cfg(any_audio_codec)]
    async fn subscribe(publisher_addr: EndpointAddr) -> Result<()> {
        let endpoint = Endpoint::bind().await?;
        let live = Live::builder(endpoint).spawn_with_router();
        let sub = live.subscribe(publisher_addr, "my-stream").await?;

        let mut video = sub.video()?;
        let audio_ctx = AudioBackend::default();
        let _audio = sub.audio(&audio_ctx).await?;

        loop {
            if let Some(_frame) = video.current_frame() {
                // render(frame)
            }
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    }
}

/// One-to-one call WITHOUT the Call helper — raw primitives.
#[cfg(any_video_codec)]
mod ex_call_raw {
    use super::*;

    #[cfg(any_audio_codec)]
    async fn caller(live: Live, remote: EndpointAddr) -> Result<()> {
        let mut session = live.transport().connect(remote).await?;

        let camera = CameraCapturer::new()?;
        let audio_ctx = AudioBackend::default();
        let mic = audio_ctx.default_input().await?;

        let broadcast = LocalBroadcast::new();
        broadcast
            .video()
            .set(camera, VideoCodec::best_available(), [VideoPreset::P720])?;
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;
        session.publish("call".to_string(), broadcast.consume());

        let consumer = session.subscribe("call").await?;
        let sub = RemoteBroadcast::new("call".to_string(), consumer).await?;
        let mut video = sub.video()?;
        let _audio = sub.audio(&audio_ctx).await?;

        loop {
            if let Some(_frame) = video.current_frame() { /* render */ }
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    }

    async fn receiver(live: Live) -> Result<()> {
        let mut incoming = live.transport().incoming_sessions();
        while let Some(incoming_session) = incoming.next().await {
            let mut session = incoming_session.accept();

            let camera = CameraCapturer::new()?;
            let broadcast = LocalBroadcast::new();
            broadcast
                .video()
                .set(camera, VideoCodec::best_available(), [VideoPreset::P720])?;
            session.publish("call".to_string(), broadcast.consume());

            let consumer = session.subscribe("call").await?;
            let sub = RemoteBroadcast::new("call".to_string(), consumer).await?;
            let mut _video = sub.video()?;
        }
        Ok(())
    }
}

/// Same call using the Call convenience helper.
#[cfg(any_video_codec)]
mod ex_call_with_helper {
    use super::*;

    #[cfg(any_audio_codec)]
    async fn caller(live: Live, remote: EndpointAddr) -> Result<()> {
        let call = Call::dial(&live, remote).await?;

        let camera = CameraCapturer::new()?;
        call.local()
            .video()
            .set(camera, VideoCodec::best_available(), [VideoPreset::P720])?;

        let audio_ctx = AudioBackend::default();
        let mic = audio_ctx.default_input().await?;
        call.local()
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

        let remote_sub = call.remote().expect("dial waits for remote");
        let mut video = remote_sub.video()?;
        let _audio = remote_sub.audio(&audio_ctx).await?;

        loop {
            if let Some(_frame) = video.current_frame() { /* render */ }
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    }

    async fn receiver(live: Live) -> Result<()> {
        let mut incoming = live.transport().incoming_sessions();
        while let Some(incoming_session) = incoming.next().await {
            let call = Call::accept(incoming_session.accept()).await?;

            let camera = CameraCapturer::new()?;
            call.local()
                .video()
                .set(camera, VideoCodec::best_available(), [VideoPreset::P720])?;

            let remote_sub = call.remote().expect("accept waits for remote");
            let mut _video = remote_sub.video()?;
        }
        Ok(())
    }
}

/// Call with screen share — caller shares screen as a second broadcast.
#[cfg(any_video_codec)]
mod ex_call_with_screenshare {
    use super::*;

    #[cfg(any_audio_codec)]
    async fn caller_with_screenshare(live: Live, remote: EndpointAddr) -> Result<()> {
        let mut session = live.transport().connect(remote).await?;
        let audio_ctx = AudioBackend::default();

        // Camera + mic broadcast
        let camera_broadcast = LocalBroadcast::new();
        let camera = CameraCapturer::new()?;
        let mic = audio_ctx.default_input().await?;
        camera_broadcast
            .video()
            .set(camera, VideoCodec::best_available(), [VideoPreset::P720])?;
        camera_broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;
        session.publish("camera".to_string(), camera_broadcast.consume());

        // Screen share as a separate broadcast — independent lifecycle
        let screen_broadcast = LocalBroadcast::new();
        let screen = ScreenCapturer::new()?;
        screen_broadcast
            .video()
            .set(screen, VideoCodec::best_available(), [VideoPreset::P1080])?;
        session.publish("screen".to_string(), screen_broadcast.consume());

        // Subscribe to remote's camera + optional screen
        let cam_consumer = session.subscribe("camera").await?;
        let cam_sub = RemoteBroadcast::new("camera".to_string(), cam_consumer).await?;
        let mut _cam_video = cam_sub.video()?;
        let _cam_audio = cam_sub.audio(&audio_ctx).await?;

        // Stop screen share later (drop stops encoding + un-announces)
        drop(screen_broadcast);

        session.closed().await;
        Ok(())
    }
}

/// Camera dashboard with quality constraints.
#[cfg(any_video_codec)]
mod ex_dashboard {
    use super::*;

    async fn dashboard(live: Live, cameras: Vec<(EndpointAddr, &str)>) -> Result<()> {
        let mut tracks: Vec<(String, VideoTrack)> = Vec::new();

        for (addr, name) in cameras {
            let sub = live.subscribe(addr, name).await?;
            let video = sub.video_with(
                VideoOptions::default().target(VideoTarget::default().max_pixels(320 * 180)),
            )?;
            tracks.push((name.to_string(), video));
        }

        loop {
            for (name, track) in &mut tracks {
                if let Some(_frame) = track.current_frame() {
                    let _ = name; // render_tile(name, frame)
                }
            }
            tokio::time::sleep(Duration::from_millis(33)).await;
        }
    }
}

/// Camera switch mid-stream — seamless source replacement.
#[cfg(any_video_codec)]
mod ex_camera_switch {
    use super::*;

    #[cfg(any_audio_codec)]
    async fn camera_switch() -> Result<()> {
        let broadcast = LocalBroadcast::new();

        let front = CameraCapturer::new()?;
        broadcast
            .video()
            .set(front, VideoCodec::best_available(), [VideoPreset::P720])?;

        let audio_ctx = AudioBackend::default();
        let mic = audio_ctx.default_input().await?;
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

        // Switch to a different camera
        let rear = CameraCapturer::with_index(1)?;
        broadcast
            .video()
            .replace(rear, VideoCodec::best_available(), [VideoPreset::P720])?;

        broadcast.video().set_enabled(false);
        broadcast.video().set_enabled(true);
        broadcast.audio().set_muted(true);
        broadcast.audio().set_muted(false);

        Ok(())
    }
}

// ── Examples requiring unimplemented features ──────────────────────────────
//
// The following examples require features not yet implemented:
// - LocalBroadcast::relay() — zero-transcode relay
// - VideoTrack as VideoSource — transcode relay
// - Room redesign with participants/events/TrackName
// - Relay URL connections
//
// They are kept as commented-out design references.

/*
/// Zero-transcode relay — forward packets without decoding.
mod ex_relay {
    use super::*;

    async fn relay(live: Live, source_addr: EndpointAddr) -> Result<()> {
        let sub = live.subscribe(source_addr, "concert-stream").await?;
        let relay = LocalBroadcast::relay(&sub)?;
        live.publish("concert-relay", &relay).await?;
        sub.closed().await;
        Ok(())
    }
}

/// Transcoding relay — decode then re-encode at different quality.
mod ex_transcode {
    use super::*;

    async fn transcode(live: Live, source_addr: EndpointAddr) -> Result<()> {
        let sub = live.subscribe(source_addr, "hd-stream").await?;
        let video = sub.video()?;
        let relay = LocalBroadcast::new();
        relay
            .video()
            .set(video, VideoCodec::best_available(), [VideoPreset::P360])?;
        live.publish("sd-stream", &relay).await?;
        sub.closed().await;
        Ok(())
    }
}

/// Multi-party room with event-driven participant handling.
mod ex_room {
    use super::*;

    async fn room_participant(live: Live, ticket: iroh_live::rooms::RoomTicket) -> Result<()> {
        // Room API redesign is pending — current Room type has different event model.
        // See plans/api/4-impl.md Phase 8 for the target API.
        todo!("Room participant model redesign")
    }
}
*/

fn main() {
    // This example is a compilation test — not meant to be run.
    // cargo check -p iroh-live --example api_sketch --all-features
    println!("api_sketch compiles successfully against real types!");
}
