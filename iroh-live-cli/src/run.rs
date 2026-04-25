//! `irl run` - TOML-configured multi-stream publish/subscribe sessions.
//!
//! Reads a config file that declares multiple send and recv blocks, each
//! running as its own tokio task on a shared [`Live`] instance. This replaces
//! the need for multiple `irl publish` + `irl play` invocations when running
//! complex multi-stream scenarios.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicU64},
};

use iroh::SecretKey;
use iroh_live::{
    Live, Subscription,
    media::{
        AudioBackend,
        codec::VideoCodec,
        format::{AudioPreset, PlaybackConfig, Quality, VideoPreset},
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
    record::{
        H264AnnexBState, audio_codec_extension, record_raw_track, record_video_track,
        video_codec_extension,
    },
    transport::setup_live,
};

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Top-level configuration for `irl run`.
#[derive(Debug, Deserialize)]
pub struct RunConfig {
    /// Named secret key for persistent endpoint identity. Stored in
    /// `~/.config/iroh-live/secret_keys/<name>.key` (hex-encoded).
    /// Generated on first use so that tickets remain stable across runs.
    pub secret_key_name: Option<String>,
    /// Broadcasts to publish.
    #[serde(default)]
    pub send: Vec<SendConfig>,
    /// Remote broadcasts to subscribe to.
    #[serde(default)]
    pub recv: Vec<RecvConfig>,
}

/// Configuration for a single send (publish) stream. All fields are
/// required - no implicit defaults.
#[derive(Debug, Deserialize)]
pub struct SendConfig {
    /// Human-readable name, also used as the broadcast name.
    pub name: String,
    /// Video source spec: `cam:pw:0`, `screen:pw:0`, `test`, `none`.
    pub video_source: String,
    /// Video codec: `h264`, `av1`, `h264-vaapi`.
    pub video_codec: String,
    /// Video quality preset: `180p`, `360p`, `720p`, `1080p`.
    pub video_preset: String,
    /// Audio source: `default`, `none`, `file:path.mp3`, or a device spec.
    pub audio_source: String,
    /// Audio codec: `opus`, `pcm`.
    pub audio_codec: String,
    /// Audio bitrate in bits/sec (Opus only, ignored for PCM).
    #[expect(
        dead_code,
        reason = "parsed from config, wired when bitrate control lands"
    )]
    pub audio_bitrate: Option<u32>,
}

/// Configuration for a single recv (subscribe) stream. All fields
/// except `record` are required.
#[derive(Debug, Deserialize)]
pub struct RecvConfig {
    /// Human-readable label for this subscription.
    pub name: String,
    /// Connection ticket string (iroh-live ticket format).
    pub ticket: String,
    /// Audio output: `default` or `none`.
    #[allow(
        dead_code,
        reason = "parsed from config, wired when recv audio output is implemented"
    )]
    pub audio_output: String,
    /// Base path for recording. When set, raw encoded video and audio
    /// are written to `<record>.h264` / `<record>.opus` (or appropriate
    /// extension based on codec). Omit to skip recording.
    pub record: Option<String>,
}

// ---------------------------------------------------------------------------
// Secret key management
// ---------------------------------------------------------------------------

/// Loads or creates a named secret key from the config directory.
fn load_or_create_secret_key(name: &str) -> anyhow::Result<SecretKey> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine config directory"))?
        .join("iroh-live")
        .join("secret_keys");
    std::fs::create_dir_all(&config_dir)?;
    let key_path = config_dir.join(format!("{name}.key"));

    if key_path.exists() {
        let hex_str = std::fs::read_to_string(&key_path)?.trim().to_string();
        let key: SecretKey = hex_str
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid key in {}: {e}", key_path.display()))?;
        info!(name, path = %key_path.display(), "loaded secret key");
        Ok(key)
    } else {
        let key = SecretKey::generate();
        let hex_str = data_encoding::HEXLOWER.encode(&key.to_bytes());
        std::fs::write(&key_path, &hex_str)?;
        info!(name, path = %key_path.display(), "generated and saved new secret key");
        Ok(key)
    }
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

    // Load or create persistent secret key if configured.
    let secret_key = if let Some(ref name) = config.secret_key_name {
        Some(load_or_create_secret_key(name)?)
    } else {
        None
    };

    // Workaround: Live::from_env() reads IROH_SECRET from env, so we set
    // it here before calling setup_live. A proper API would accept a
    // SecretKey directly on the LiveBuilder, but from_env covers this.
    if let Some(ref key) = secret_key {
        let hex = data_encoding::HEXLOWER.encode(&key.to_bytes());
        // SAFETY: set_var is unsafe in Rust 2024 edition due to potential
        // data races, but we call it before any threads are spawned.
        unsafe { std::env::set_var("IROH_SECRET", hex) };
    }

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

                if recv.record.is_none() {
                    // No recording configured. MediaTracks already handles
                    // decoding and audio playback via the AudioBackend.
                    // Video frames are decoded but not displayed since wiring
                    // an egui window from `irl run` requires owning the main
                    // thread or spawning a dedicated render thread.
                    // TODO: spawn egui viewer window when moq-media-egui is available
                    info!(name = %recv.name, "receiving (audio playback active, video decode only)");
                }

                // Spawn recording if configured.
                if let Some(ref base_path) = recv.record {
                    let base = PathBuf::from(base_path);
                    let name = recv.name.clone();
                    let Some(active) = sub.active().await else {
                        warn!("recv stream has no active source; skipping recording setup");
                        continue;
                    };
                    let broadcast = active.broadcast().clone();
                    let catalog = broadcast.catalog();
                    info!(name, path = %base.display(), "recording enabled");

                    // Record video if available.
                    if let Ok(video_name) = catalog.select_video_rendition(Quality::Highest) {
                        match broadcast.raw_video_track(&video_name) {
                            Ok((source, config)) => {
                                let ext = video_codec_extension(&config.codec);
                                let path = base.with_extension(ext);
                                let h264_state = H264AnnexBState::from_config(&config);
                                let vb = Arc::new(AtomicU64::new(0));
                                let vf = Arc::new(AtomicU64::new(0));
                                info!(name, track = %video_name, path = %path.display(), "recording video");
                                tasks.spawn(async move {
                                    record_video_track(source, &path, vb, vf, h264_state).await
                                });
                            }
                            Err(e) => {
                                warn!(name, err = %e, "failed to get raw video track for recording")
                            }
                        }
                    }

                    // Record audio if available.
                    if let Ok(audio_name) = catalog.select_audio_rendition(Quality::Highest) {
                        match broadcast.raw_audio_track(&audio_name) {
                            Ok((source, config)) => {
                                let ext = audio_codec_extension(&config.codec);
                                let path = base.with_extension(ext);
                                let ab = Arc::new(AtomicU64::new(0));
                                let af = Arc::new(AtomicU64::new(0));
                                info!(name, track = %audio_name, path = %path.display(), "recording audio");
                                tasks.spawn(async move {
                                    record_raw_track(source, &path, ab, af).await
                                });
                            }
                            Err(e) => {
                                warn!(name, err = %e, "failed to get raw audio track for recording")
                            }
                        }
                    }
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
        if let Some(active) = sub.active().await {
            active.session().close(0, b"bye");
        }
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
        let audio_preset = AudioPreset::parse_or_list("hq")?;
        let audio_codec = moq_media::codec::AudioCodec::parse_or_list(&config.audio_codec)?;
        crate::source::setup_audio(
            &broadcast,
            &[audio_source],
            audio_ctx,
            audio_preset,
            audio_codec,
        )
        .await?;
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

    let sub = live.subscribe_ticket(&ticket);
    let active = sub.ready().await?;

    let playback = PlaybackConfig::default();
    let tracks = active.broadcast().media(audio_ctx, playback).await?;

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
video_codec = "h264"
video_preset = "360p"
audio_source = "none"
audio_codec = "opus"
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
video_codec = "h264"
video_preset = "360p"
audio_source = "default"
audio_codec = "opus"

[[send]]
name = "screen"
video_source = "screen:pw:0"
video_codec = "h264"
video_preset = "1080p"
audio_source = "none"
audio_codec = "opus"

[[recv]]
name = "remote"
ticket = "iroh-live:XYZ/stream"
audio_output = "default"
"#;
        let config: RunConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.send.len(), 2);
        assert_eq!(config.recv.len(), 1);
        assert_eq!(config.send[0].video_codec, "h264");
        assert_eq!(config.send[1].video_preset, "1080p");
    }

    #[test]
    fn missing_required_fields_rejected() {
        // Send without video_codec should fail.
        let toml = r#"
[[send]]
name = "incomplete"
video_source = "test"
"#;
        assert!(toml::from_str::<RunConfig>(toml).is_err());
    }

    #[test]
    fn secret_key_name_parsed() {
        let toml = r#"
secret_key_name = "my-studio"

[[send]]
name = "cam"
video_source = "test"
video_codec = "h264"
video_preset = "360p"
audio_source = "none"
audio_codec = "opus"
"#;
        let config: RunConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.secret_key_name.as_deref(), Some("my-studio"));
    }

    #[test]
    fn record_field_parsed() {
        let toml = r#"
[[recv]]
name = "rec"
ticket = "iroh-live:ABC/stream"
audio_output = "none"
record = "output"
"#;
        let config: RunConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.recv[0].record.as_deref(), Some("output"));
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
