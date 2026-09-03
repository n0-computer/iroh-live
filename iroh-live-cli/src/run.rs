//! `irl run`: a multi-stream session described by a TOML file.
//!
//! One endpoint publishes every `[[send]]` block and subscribes to every
//! `[[recv]]` block, which is what `irl publish` and `irl watch` cannot do
//! between them: they own a process each, and each binds its own endpoint.
//!
//! The session is headless. A `[[recv]]` block plays audio and can record to a
//! file, but it never opens a window, because a window owns the main thread and
//! there is only one of those to go around.

use std::path::Path;

use iroh::SecretKey;
use iroh_live::{
    Live, Subscription,
    media::{publish::LocalBroadcast, subscribe::AudioTrack},
    ticket::LiveTicket,
};
use n0_error::{Result, anyerr};
use serde::Deserialize;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::{
    args::{AudioCodecArg, CaptureArgs, DEFAULT_AUDIO, DEFAULT_VIDEO, RunArgs, VideoCodecArg},
    backend::EncoderArg,
    record::{RecordOptions, Recorder},
    source, transport,
};

/// Runs the `run` command.
pub fn run(args: RunArgs, rt: &tokio::runtime::Runtime) -> Result {
    let config = parse_config(&args.config)?;
    println!(
        "loaded {}: {} send, {} recv",
        args.config.display(),
        config.send.len(),
        config.recv.len()
    );
    rt.block_on(run_session(config))
}

/// A session: an optional persistent identity, the broadcasts to publish, and
/// the broadcasts to subscribe to.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    /// A name for this session's endpoint identity, stored under
    /// `<config dir>/iroh-live/secret_keys/<name>.key` and generated on first
    /// use, so the tickets a session prints survive a restart.
    pub secret_key_name: Option<String>,

    /// The broadcasts this session publishes.
    #[serde(default)]
    pub send: Vec<SendConfig>,

    /// The broadcasts this session subscribes to.
    #[serde(default)]
    pub recv: Vec<RecvConfig>,
}

/// One broadcast to publish.
///
/// Every field but `name` is an `irl publish` capture flag under the flag's own
/// name, and takes the flag's own default when it is left out.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendConfig {
    /// Broadcast path, which is also the label the session reports under.
    pub name: String,

    /// Video source: `cam`, `screen`, `test`, `none`, and the rest of
    /// `--video`'s grammar. A `file:` source is not accepted here; publish it
    /// with `irl publish`, which owns the import path.
    #[serde(default = "default_video")]
    pub video: String,

    /// Audio source, in `--audio`'s grammar.
    #[serde(default = "default_audio")]
    pub audio: String,

    /// Video codec: `h264` or `h265`.
    #[serde(default)]
    pub codec: VideoCodecArg,

    /// Encoder backend: `auto`, `hardware`, `software`, or the name of one
    /// backend, such as `vaapi`.
    #[serde(default)]
    pub encoder: EncoderArg,

    /// The simulcast ladder, one entry per rung, in `--renditions`' grammar.
    /// Empty publishes a single unscaled rendition named `video`.
    #[serde(default)]
    pub renditions: Vec<String>,

    /// Target video bitrate for the largest rung, in bits per second.
    pub bitrate: Option<u64>,

    /// Requested capture width.
    pub width: Option<u32>,

    /// Requested capture height.
    pub height: Option<u32>,

    /// Requested capture framerate.
    pub fps: Option<u32>,

    /// Hide the mouse cursor in screen, window, and application capture.
    #[serde(default)]
    pub no_cursor: bool,

    /// Audio codec: `opus` or `pcm`.
    #[serde(default)]
    pub audio_codec: AudioCodecArg,

    /// Target audio bitrate in bits per second. Opus only.
    pub audio_bitrate: Option<u32>,
}

impl SendConfig {
    /// The capture flags this block stands for.
    fn capture(&self) -> CaptureArgs {
        CaptureArgs {
            video: self.video.clone(),
            audio: self.audio.clone(),
            codec: self.codec,
            encoder: self.encoder,
            renditions: self.renditions.clone(),
            bitrate: self.bitrate,
            width: self.width,
            height: self.height,
            fps: self.fps,
            no_cursor: self.no_cursor,
            audio_codec: self.audio_codec,
            audio_bitrate: self.audio_bitrate,
            ..CaptureArgs::default()
        }
    }
}

/// What a `[[recv]]` block does with the audio it receives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioOutput {
    /// Play it through whatever the system calls its default output.
    #[default]
    Default,
    /// Leave the speakers alone.
    None,
}

/// One broadcast to subscribe to.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecvConfig {
    /// The label the session reports this subscription under.
    pub name: String,

    /// The ticket to subscribe to, as `irl publish` printed it.
    pub ticket: String,

    /// Whether this subscription's audio is played.
    ///
    /// Unlike `irl watch --audio-output`, this does not name a device: the
    /// playback engine is process-wide and a session has many subscriptions,
    /// so there is no per-block device to choose.
    #[serde(default)]
    pub audio_output: AudioOutput,

    /// A file to record the broadcast to, in the container its extension
    /// names. Omit to only receive.
    pub record: Option<String>,

    /// The one video rendition to record, instead of every rung the catalog
    /// offers. Only meaningful alongside `record`.
    pub rendition: Option<String>,
}

/// The `video` default, which is `--video`'s.
fn default_video() -> String {
    DEFAULT_VIDEO.to_string()
}

/// The `audio` default, which is `--audio`'s.
fn default_audio() -> String {
    DEFAULT_AUDIO.to_string()
}

/// Reads and validates the session file at `path`.
///
/// # Errors
///
/// Fails if the file cannot be read, is not the schema above, or describes no
/// streams at all.
fn parse_config(path: &Path) -> Result<RunConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| anyerr!("failed to read {}: {err}", path.display()))?;
    let config: RunConfig = toml::from_str(&text)
        .map_err(|err| anyerr!("failed to parse {}: {err}", path.display()))?;

    if config.send.is_empty() && config.recv.is_empty() {
        return Err(anyerr!(
            "{} has no [[send]] and no [[recv]] blocks, so there is nothing to run",
            path.display()
        ));
    }
    Ok(config)
}

/// Publishes and subscribes everything the session describes, then holds it
/// open until the user interrupts.
async fn run_session(config: RunConfig) -> Result {
    // Only a session that publishes needs to accept incoming connections; one
    // that only subscribes dials out and never has to be reachable.
    let serve = !config.send.is_empty();
    let secret_key = match &config.secret_key_name {
        Some(name) => load_or_create_secret_key(name)?,
        None => iroh_live::util::secret_key_from_env()?,
    };
    let live = transport::setup_live_with_key(secret_key, serve).await?;
    let result = run_streams(&live, &config).await;
    live.shutdown().await;
    println!("done");
    result
}

/// Sets up every block over `live` and holds the session open until the user
/// interrupts. The caller closes the endpoint either way.
///
/// # Errors
///
/// Fails if no block could be set up at all. A block that fails on its own is
/// reported and the rest of the session runs without it.
async fn run_streams(live: &Live, config: &RunConfig) -> Result {
    // Every handle is kept until shutdown, because dropping one is what stops
    // it: a broadcast stops publishing when its handle goes, and a
    // subscription cancels every task it started.
    let mut broadcasts: Vec<LocalBroadcast> = Vec::new();
    let mut receivers: Vec<Receiver> = Vec::new();
    let mut recordings: JoinSet<Result<()>> = JoinSet::new();

    for send in &config.send {
        match setup_send(live, send) {
            Ok(broadcast) => {
                let ticket = LiveTicket::new(live.endpoint().id(), &send.name);
                println!("[send] {}: {ticket}", send.name);
                broadcasts.push(broadcast);
            }
            Err(err) => {
                warn!(name = %send.name, error = %err, "publish failed");
                eprintln!("[send] {}: {err:#}", send.name);
            }
        }
    }

    // Concurrently, because a subscription waits for the peer's first catalog
    // and a peer that has not started publishing yet would otherwise hold up
    // every block behind it.
    let setups = config
        .recv
        .iter()
        .map(|recv| async move { (recv, setup_recv(live, recv).await) });
    for (recv, result) in n0_future::join_all(setups).await {
        match result {
            Ok((receiver, recorder)) => {
                println!("[recv] {}: subscribed to {}", recv.name, recv.ticket);
                if let Some(recorder) = recorder {
                    let path = recorder.path().display().to_string();
                    println!("[recv] {}: recording to {path}", recv.name);
                    let stop = receiver.sub.broadcast().shutdown_token().cancelled_owned();
                    let name = recv.name.clone();
                    recordings.spawn(async move {
                        let written = recorder.run(stop).await?;
                        info!(name, bytes = written, path, "recording finished");
                        Ok(())
                    });
                }
                receivers.push(receiver);
            }
            Err(err) => {
                warn!(name = %recv.name, error = %err, "subscribe failed");
                eprintln!("[recv] {}: {err:#}", recv.name);
            }
        }
    }

    if broadcasts.is_empty() && receivers.is_empty() {
        return Err(anyerr!(
            "no stream could be set up, so there is nothing to do"
        ));
    }

    println!(
        "{} send, {} recv. press Ctrl+C to stop",
        broadcasts.len(),
        receivers.len()
    );
    tokio::signal::ctrl_c().await?;
    println!("stopping ...");

    // Cancelling each subscription is what ends its recording: the recorder
    // stops on the broadcast's shutdown token, and only then is the file
    // flushed, so the recordings are awaited before anything else closes.
    for receiver in &receivers {
        receiver.sub.broadcast().shutdown();
    }
    while let Some(finished) = recordings.join_next().await {
        match finished {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(error = %err, "recording ended with an error"),
            Err(err) => warn!(error = %err, "a recording task panicked"),
        }
    }

    for broadcast in broadcasts {
        broadcast.finish();
    }
    for receiver in &receivers {
        receiver.sub.session().close(moq_net::Error::Cancel);
    }
    Ok(())
}

/// One live `[[recv]]` block.
///
/// The audio track is held rather than used: opening it starts playback, and
/// dropping it stops it.
struct Receiver {
    sub: Subscription,
    _audio: Option<AudioTrack>,
}

/// Publishes one `[[send]]` block.
///
/// # Errors
///
/// Fails if the broadcast path is taken, or the block's sources or ladder do
/// not parse. A device that will not open surfaces in the log and ends its
/// track, not here.
fn setup_send(live: &Live, config: &SendConfig) -> Result<LocalBroadcast> {
    let broadcast = live.publish(&config.name)?;
    source::configure(&broadcast, &config.capture())?;
    Ok(broadcast)
}

/// Subscribes to one `[[recv]]` block, opening its audio and its recording if
/// it asked for either.
///
/// The recorder comes back rather than running here: the caller owns the tasks,
/// and starting one before every block has been set up would record a stretch
/// of nothing while the rest are still connecting.
///
/// # Errors
///
/// Fails if the ticket does not parse, the peer cannot be reached, or the
/// recording file cannot be created. Audio that will not open is reported and
/// the subscription continues without it.
async fn setup_recv(live: &Live, config: &RecvConfig) -> Result<(Receiver, Option<Recorder>)> {
    let ticket: LiveTicket = config.ticket.parse().map_err(|err| {
        anyerr!(
            "invalid ticket: {err}; it should be the string `irl publish` \
             printed, starting with `iroh-live:`"
        )
    })?;
    let sub = transport::subscribe(live, &ticket).await?;

    let recorder = match &config.record {
        None => None,
        Some(path) => {
            let mut options = RecordOptions::new(path.clone(), None)?;
            options.rendition = config.rendition.clone();
            Some(Recorder::open(sub.session(), sub.broadcast(), &options).await?)
        }
    };

    let audio = match config.audio_output {
        AudioOutput::None => None,
        AudioOutput::Default => play_audio(&sub, &config.name).await,
    };
    Ok((Receiver { sub, _audio: audio }, recorder))
}

/// Opens the broadcast's audio track, which starts playing it.
// A build without `playback` has no sink to open, so nothing in that arm awaits.
#[allow(
    clippy::unused_async,
    reason = "one arm of a feature-gated body awaits"
)]
async fn play_audio(sub: &Subscription, name: &str) -> Option<AudioTrack> {
    #[cfg(feature = "playback")]
    {
        if !sub.broadcast().has_audio() {
            info!(name, "the broadcast carries no audio");
            return None;
        }
        sub.broadcast()
            .audio()
            .await
            .inspect_err(|err| warn!(name, error = %err, "audio track failed to open"))
            .ok()
    }
    #[cfg(not(feature = "playback"))]
    {
        let _ = sub;
        warn!(
            name,
            "audio_output asks for playback, which this build was compiled without"
        );
        None
    }
}

/// Loads the named secret key, generating and storing one on first use.
///
/// # Errors
///
/// Fails if the config directory cannot be found or written, or if the stored
/// key is not a key.
fn load_or_create_secret_key(name: &str) -> Result<SecretKey> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyerr!("cannot find this platform's config directory"))?
        .join("iroh-live")
        .join("secret_keys");
    std::fs::create_dir_all(&dir)
        .map_err(|err| anyerr!("failed to create {}: {err}", dir.display()))?;
    let path = dir.join(format!("{name}.key"));

    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|err| anyerr!("failed to read {}: {err}", path.display()))?;
        let key: SecretKey = text
            .trim()
            .parse()
            .map_err(|err| anyerr!("{} does not hold a secret key: {err}", path.display()))?;
        info!(name, path = %path.display(), "loaded the session secret key");
        return Ok(key);
    }

    let key = SecretKey::generate();
    std::fs::write(&path, data_encoding::HEXLOWER.encode(&key.to_bytes()))
        .map_err(|err| anyerr!("failed to write {}: {err}", path.display()))?;
    info!(name, path = %path.display(), "generated a session secret key");
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_block_takes_the_flag_defaults() {
        let config: RunConfig = toml::from_str(
            r#"
            [[send]]
            name = "cam"
            "#,
        )
        .expect("only `name` is required");
        let send = &config.send[0];
        assert_eq!(send.video, DEFAULT_VIDEO);
        assert_eq!(send.audio, DEFAULT_AUDIO);
        assert_eq!(send.encoder, EncoderArg::Auto);
        assert_eq!(send.codec, VideoCodecArg::H264);
        assert_eq!(send.audio_codec, AudioCodecArg::Opus);
        assert!(send.renditions.is_empty());
    }

    #[test]
    fn a_send_block_becomes_capture_flags() {
        let config: RunConfig = toml::from_str(
            r#"
            [[send]]
            name = "screen"
            video = "screen"
            audio = "none"
            codec = "h265"
            encoder = "vaapi"
            renditions = ["low:320x180", "720p"]
            bitrate = 3000000
            fps = 30
            no_cursor = true
            audio_codec = "pcm"
            "#,
        )
        .expect("a full send block");
        let capture = config.send[0].capture();
        assert_eq!(capture.video, "screen");
        assert_eq!(capture.codec, VideoCodecArg::H265);
        assert_eq!(capture.encoder, EncoderArg::Vaapi);
        assert_eq!(capture.renditions, ["low:320x180", "720p"]);
        assert_eq!(capture.bitrate, Some(3_000_000));
        assert!(capture.no_cursor);
        assert_eq!(capture.audio_codec, AudioCodecArg::Pcm);
        // `--test-source` is a flag with no config spelling: a session file
        // says `video = "test"` instead.
        assert!(!capture.test_source);
    }

    #[test]
    fn a_recv_block_defaults_to_playing_audio_and_not_recording() {
        let config: RunConfig = toml::from_str(
            r#"
            [[recv]]
            name = "friend"
            ticket = "iroh-live:abc/hello"
            "#,
        )
        .expect("only `name` and `ticket` are required");
        assert_eq!(config.recv[0].audio_output, AudioOutput::Default);
        assert!(config.recv[0].record.is_none());
    }

    #[test]
    fn a_misspelled_key_is_rejected() {
        let err = toml::from_str::<RunConfig>(
            r#"
            [[send]]
            name = "cam"
            bitrat = 3000000
            "#,
        )
        .expect_err("a typo must not be silently ignored");
        assert!(err.to_string().contains("bitrat"), "unexpected: {err}");
    }

    #[test]
    fn a_config_with_no_blocks_is_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "").expect("write");
        let err = parse_config(&path).expect_err("an empty session runs nothing");
        assert!(
            err.to_string().contains("nothing to run"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn a_missing_config_is_rejected() {
        let err = parse_config(Path::new("/nonexistent/session.toml"))
            .expect_err("the file does not exist");
        assert!(
            err.to_string().contains("failed to read"),
            "unexpected: {err}"
        );
    }
}
