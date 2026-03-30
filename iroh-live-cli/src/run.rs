//! `irl run` — TOML-configured multi-stream publish/subscribe sessions.
//!
//! Reads a config file that declares multiple send and recv blocks, each
//! running as its own tokio task on a shared [`Live`] instance. This replaces
//! the need for multiple `irl publish` + `irl play` invocations when running
//! complex multi-stream scenarios.

use std::path::Path;

use iroh_live::{
    Live, Subscription,
    media::{
        AudioBackend,
        codec::VideoCodec,
        format::{AudioPreset, PlaybackConfig, VideoPreset},
        publish::LocalBroadcast,
        subscribe::MediaTracks,
    },
    ticket::LiveTicket,
};
use serde::Deserialize;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::{
    args::{AudioSourceSpec, RunArgs, VideoSourceSpec},
    transport::setup_live,
};

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Top-level configuration for `irl run`.
#[derive(Debug, Deserialize)]
pub struct RunConfig {
    /// Broadcasts to publish.
    #[serde(default)]
    pub send: Vec<SendConfig>,
    /// Remote broadcasts to subscribe to.
    #[serde(default)]
    pub recv: Vec<RecvConfig>,
}

/// Configuration for a single send (publish) stream.
#[derive(Debug, Deserialize)]
pub struct SendConfig {
    /// Human-readable name, also used as the broadcast name.
    pub name: String,
    /// Video source spec: `cam:pw:0`, `screen:pw:0`, `test`, `none`, etc.
    #[serde(default = "default_video_source")]
    pub video_source: String,
    /// Video codec: `h264`, `av1`, `h264-vaapi`, etc.
    #[serde(default = "default_video_codec")]
    pub video_codec: String,
    /// Video quality preset: `180p`, `360p`, `720p`, `1080p`.
    #[serde(default = "default_video_preset")]
    pub video_preset: String,
    /// Audio source: `default`, `none`, or a device spec.
    #[serde(default = "default_audio_source")]
    pub audio_source: String,
    /// Audio codec (currently only `opus` is supported).
    #[serde(default = "default_audio_codec")]
    #[allow(
        dead_code,
        reason = "config field reserved for multi-codec audio support"
    )]
    pub audio_codec: String,
    /// Audio bitrate in bits/sec (Opus only, ignored otherwise).
    #[serde(default)]
    #[allow(dead_code, reason = "config field reserved for future bitrate control")]
    pub audio_bitrate: Option<u32>,
}

/// Configuration for a single recv (subscribe) stream.
#[derive(Debug, Deserialize)]
pub struct RecvConfig {
    /// Human-readable label for this subscription.
    #[serde(default = "default_recv_name")]
    pub name: String,
    /// Connection ticket string (iroh-live ticket format).
    pub ticket: String,
    /// Audio output: `default`, `none`, or a device spec.
    #[serde(default = "default_audio_output")]
    #[allow(dead_code, reason = "config field, used during recv setup")]
    pub audio_output: String,
    /// Path to write raw encoded video to (optional).
    #[serde(default)]
    pub record_video: Option<String>,
    /// Path to write raw encoded audio to (optional).
    #[serde(default)]
    pub record_audio: Option<String>,
}

fn default_video_source() -> String {
    "cam:0".to_string()
}
fn default_video_codec() -> String {
    "h264".to_string()
}
fn default_video_preset() -> String {
    "360p".to_string()
}
fn default_audio_source() -> String {
    "default".to_string()
}
fn default_audio_codec() -> String {
    "opus".to_string()
}
fn default_recv_name() -> String {
    "recv".to_string()
}
fn default_audio_output() -> String {
    "default".to_string()
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn parse_config(path: &Path) -> anyhow::Result<RunConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read config {}: {e}", path.display()))?;
    let config: RunConfig = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse config {}: {e}", path.display()))?;

    if config.send.is_empty() && config.recv.is_empty() {
        anyhow::bail!(
            "config {} has no [[send]] or [[recv]] blocks",
            path.display()
        );
    }

    Ok(config)
}

fn parse_video_source(spec: &str) -> anyhow::Result<VideoSourceSpec> {
    if spec == "none" {
        return Ok(VideoSourceSpec::None);
    }
    VideoSourceSpec::parse(spec).map_err(|e| anyhow::anyhow!("{e}"))
}

fn parse_audio_source(spec: &str) -> anyhow::Result<AudioSourceSpec> {
    if spec == "none" {
        return Ok(AudioSourceSpec::None);
    }
    AudioSourceSpec::parse(spec).map_err(|e| anyhow::anyhow!("{e}"))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Runs the `irl run` command.
pub fn run(args: RunArgs, rt: &tokio::runtime::Runtime) -> n0_error::Result {
    let config = parse_config(&args.config)?;

    println!(
        "loaded config: {} send, {} recv",
        config.send.len(),
        config.recv.len()
    );

    rt.block_on(run_session(config))
}

async fn run_session(config: RunConfig) -> n0_error::Result {
    let needs_serve = !config.send.is_empty();
    let live = setup_live(needs_serve).await?;
    let audio_ctx = AudioBackend::default();

    let mut tasks = JoinSet::<anyhow::Result<()>>::new();

    // Holds send-side state so broadcasts stay alive until shutdown.
    let mut send_handles: Vec<(String, LocalBroadcast)> = Vec::new();
    let mut recv_handles: Vec<(String, Subscription, MediaTracks)> = Vec::new();

    // -- Set up send streams --
    for send in &config.send {
        match setup_send(&live, send, &audio_ctx).await {
            Ok(broadcast) => {
                let ticket = LiveTicket::new(live.endpoint().addr(), &send.name);
                println!("[send] {}: {ticket}", send.name);
                send_handles.push((send.name.clone(), broadcast));
            }
            Err(e) => {
                warn!(name = %send.name, err = %e, "send setup failed");
                eprintln!("[send] {}: ERROR - {e:#}", send.name);
            }
        }
    }

    // -- Set up recv streams --
    for recv in &config.recv {
        match setup_recv(&live, recv, &audio_ctx).await {
            Ok((sub, tracks)) => {
                println!("[recv] {}: connected to {}", recv.name, recv.ticket);

                // Spawn recording tasks if configured.
                if recv.record_video.is_some() || recv.record_audio.is_some() {
                    let name = recv.name.clone();
                    tasks.spawn(async move {
                        // TODO: implement raw recording (reuse record.rs logic)
                        info!(
                            name,
                            "recording requested but not yet implemented in run mode"
                        );
                        Ok(())
                    });
                }

                recv_handles.push((recv.name.clone(), sub, tracks));
            }
            Err(e) => {
                warn!(name = %recv.name, err = %e, "recv setup failed");
                eprintln!("[recv] {}: ERROR - {e:#}", recv.name);
            }
        }
    }

    if send_handles.is_empty() && recv_handles.is_empty() {
        n0_error::bail_any!("no streams could be set up, nothing to do");
    }

    println!(
        "\n{} send + {} recv active. press Ctrl+C to stop.",
        send_handles.len(),
        recv_handles.len()
    );

    // Wait for shutdown.
    tokio::signal::ctrl_c().await?;
    println!("\nshutting down...");

    // Close recv sessions so remote peers get a clean close.
    for (name, sub, _tracks) in &recv_handles {
        info!(name, "closing recv");
        sub.session().close(0, b"bye");
    }

    // Abort any background tasks.
    tasks.shutdown().await;

    // Drop state and shut down the endpoint.
    drop(send_handles);
    drop(recv_handles);
    live.shutdown().await;

    println!("done");
    n0_error::Ok(())
}

// ---------------------------------------------------------------------------
// Send setup
// ---------------------------------------------------------------------------

async fn setup_send(
    live: &Live,
    config: &SendConfig,
    audio_ctx: &AudioBackend,
) -> anyhow::Result<LocalBroadcast> {
    let broadcast = LocalBroadcast::new();

    // Video
    let video_source = parse_video_source(&config.video_source)?;
    if !matches!(video_source, VideoSourceSpec::None) {
        let codec = VideoCodec::parse_or_best(Some(&config.video_codec))?;
        let preset = VideoPreset::parse_or_list(&config.video_preset)?;
        crate::source::setup_video(&broadcast, &[video_source], codec, &[preset])?;
    }

    // Audio
    let audio_source = parse_audio_source(&config.audio_source)?;
    if !matches!(audio_source, AudioSourceSpec::None) {
        if config.audio_source.starts_with("file:") {
            // TODO: audio file import not yet supported in run mode
            warn!(
                name = %config.name,
                source = %config.audio_source,
                "audio file import not yet supported, skipping audio"
            );
        } else {
            let audio_preset = AudioPreset::parse_or_list("hq")?;
            crate::source::setup_audio(&broadcast, &[audio_source], audio_ctx, audio_preset)
                .await?;
        }
    }

    // Publish locally so subscribers can connect.
    live.publish(&config.name, &broadcast).await?;

    Ok(broadcast)
}

// ---------------------------------------------------------------------------
// Recv setup
// ---------------------------------------------------------------------------

async fn setup_recv(
    live: &Live,
    config: &RecvConfig,
    audio_ctx: &AudioBackend,
) -> anyhow::Result<(Subscription, MediaTracks)> {
    let ticket: LiveTicket = config
        .ticket
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid ticket for recv '{}': {e}", config.name))?;

    let sub = live
        .subscribe(ticket.endpoint, &ticket.broadcast_name)
        .await?;

    let playback = PlaybackConfig::default();
    let tracks = sub.broadcast().media(audio_ctx, playback).await?;

    Ok((sub, tracks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_send_only_config() {
        let toml = r#"
[[send]]
name = "my-camera"
video_source = "test"
video_preset = "360p"
audio_source = "none"
"#;
        let config: RunConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.send.len(), 1);
        assert_eq!(config.recv.len(), 0);
        assert_eq!(config.send[0].name, "my-camera");
        assert_eq!(config.send[0].video_source, "test");
        assert_eq!(config.send[0].audio_source, "none");
    }

    #[test]
    fn parse_recv_only_config() {
        let toml = r#"
[[recv]]
name = "friend"
ticket = "iroh-live:ABC123/camera"
audio_output = "none"
"#;
        let config: RunConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.send.len(), 0);
        assert_eq!(config.recv.len(), 1);
        assert_eq!(config.recv[0].name, "friend");
        assert_eq!(config.recv[0].audio_output, "none");
    }

    #[test]
    fn parse_mixed_config() {
        let toml = r#"
[[send]]
name = "cam"
video_source = "cam:pw:0"
audio_source = "default"

[[send]]
name = "screen"
video_source = "screen:pw:0"
video_preset = "1080p"
audio_source = "none"

[[recv]]
name = "remote"
ticket = "iroh-live:XYZ/stream"
"#;
        let config: RunConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.send.len(), 2);
        assert_eq!(config.recv.len(), 1);
        assert_eq!(config.send[0].video_codec, "h264"); // default
        assert_eq!(config.send[1].video_preset, "1080p");
    }

    #[test]
    fn defaults_applied_correctly() {
        let toml = r#"
[[send]]
name = "minimal"
"#;
        let config: RunConfig = toml::from_str(toml).unwrap();
        let s = &config.send[0];
        assert_eq!(s.video_source, "cam:0");
        assert_eq!(s.video_codec, "h264");
        assert_eq!(s.video_preset, "360p");
        assert_eq!(s.audio_source, "default");
        assert_eq!(s.audio_codec, "opus");
        assert!(s.audio_bitrate.is_none());
    }

    #[test]
    fn parse_config_rejects_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "").unwrap();
        let err = parse_config(&path).unwrap_err();
        assert!(
            err.to_string().contains("no [[send]] or [[recv]]"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_config_rejects_missing_file() {
        let err = parse_config(Path::new("/nonexistent/config.toml")).unwrap_err();
        assert!(
            err.to_string().contains("failed to read"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn video_source_none_parses() {
        assert!(matches!(
            parse_video_source("none").unwrap(),
            VideoSourceSpec::None
        ));
    }

    #[test]
    fn audio_source_none_parses() {
        assert!(matches!(
            parse_audio_source("none").unwrap(),
            AudioSourceSpec::None
        ));
    }
}
