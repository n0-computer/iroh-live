//! `irl run`: a multi-stream session described by a TOML file.
//!
//! One endpoint publishes every `[[send]]` block and subscribes to every
//! `[[recv]]` block. The session is headless: a `[[recv]]` block plays audio
//! and can record, but opens no window.

use std::path::Path;

use iroh::SecretKey;
use iroh_live::{
    BroadcastTicket, EndpointOptions, Live,
    media::{self, LocalBroadcast, Player, PlayerConfig, Recording, RenditionMode},
    secret_key_file,
};
use n0_error::{Result, anyerr};
use serde::{Deserialize, Deserializer, de};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    args::{CaptureArgs, RunArgs},
    record::RecordOptions,
    source,
    transport::{self, Subscribed},
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

/// A session file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    /// Name of a stored secret key, so the session's tickets survive a restart.
    ///
    /// The key lives in `<config dir>/iroh-live/secret_keys/<name>.key` and is
    /// generated on first use.
    pub secret_key_name: Option<String>,

    /// The broadcasts this session publishes.
    #[serde(default)]
    pub send: Vec<SendConfig>,

    /// The broadcasts this session subscribes to.
    #[serde(default)]
    pub recv: Vec<RecvConfig>,
}

/// One broadcast to publish: `name` plus the `irl publish` capture flags.
///
/// Hand-deserialized, since `#[serde(flatten)]` cannot reject unknown keys.
#[derive(Debug)]
pub struct SendConfig {
    /// The broadcast name, also the label in output.
    pub name: String,
    /// The capture and encoding flags. `file:` sources need `irl publish`.
    pub capture: CaptureArgs,
}

impl<'de> Deserialize<'de> for SendConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut table = toml::Table::deserialize(deserializer)?;
        let name = table
            .remove("name")
            .ok_or_else(|| de::Error::missing_field("name"))?
            .try_into()
            .map_err(de::Error::custom)?;
        let capture = table.try_into().map_err(de::Error::custom)?;
        Ok(Self { name, capture })
    }
}

/// What a `[[recv]]` block does with the audio it receives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioOutput {
    /// Play it through the system default output.
    #[default]
    Default,
    /// Do not play it.
    None,
}

/// One broadcast to subscribe to.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecvConfig {
    /// Label for this subscription in output.
    pub name: String,

    /// Ticket that `irl publish` printed.
    pub ticket: String,

    /// Whether to play the audio: `default` or `none`.
    ///
    /// All blocks share one output, so this cannot name a device.
    #[serde(default)]
    pub audio_output: AudioOutput,

    /// File to record to. Its extension picks the container.
    pub record: Option<String>,

    /// The only video rendition to record. Needs `record`.
    pub rendition: Option<String>,
}

/// Reads and validates the session file at `path`.
///
/// Fails if it has no `[[send]]` and no `[[recv]]` blocks.
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

/// Runs the session until Ctrl+C, then closes the endpoint.
async fn run_session(config: RunConfig) -> Result {
    let serve = !config.send.is_empty();
    let options = match &config.secret_key_name {
        Some(name) => EndpointOptions {
            secret_key: Some(stored_secret_key(name)?),
            ..EndpointOptions::default()
        },
        None => EndpointOptions::from_env()?,
    };
    let live = transport::setup_live_with(options, serve).await?;
    let result = run_streams(&live, &config).await;
    live.shutdown().await;
    println!("done");
    result
}

/// Sets up every block and runs until Ctrl+C.
///
/// A block that fails is reported and skipped. Fails only if no block could be
/// set up.
async fn run_streams(live: &Live, config: &RunConfig) -> Result {
    // Dropping a handle stops it, so all are kept until shutdown.
    let mut broadcasts: Vec<(LocalBroadcast, source::Opened)> = Vec::new();
    let mut receivers: Vec<Receiver> = Vec::new();
    let mut recordings: JoinSet<Result<()>> = JoinSet::new();
    let stop_recording = CancellationToken::new();
    let output = match config
        .recv
        .iter()
        .any(|recv| recv.audio_output == AudioOutput::Default)
    {
        true => Some(crate::playback::output(None).await?),
        false => None,
    };

    for send in &config.send {
        match setup_send(live, send).await {
            Ok(published) => {
                let ticket = live.ticket(&send.name);
                println!("[send] {}: {ticket}", send.name);
                broadcasts.push(published);
            }
            Err(err) => {
                warn!(name = %send.name, error = %err, "publish failed");
                eprintln!("[send] {}: {err:#}", send.name);
            }
        }
    }

    // Concurrent, because each waits for its peer's catalog and a peer that
    // has not started would hold up the rest.
    let setups = config.recv.iter().map(|recv| {
        let output = output.as_ref();
        async move { (recv, setup_recv(live, recv, output).await) }
    });
    for (recv, result) in n0_future::join_all(setups).await {
        match result {
            Ok((receiver, recording)) => {
                println!("[recv] {}: subscribed to {}", recv.name, recv.ticket);
                if let Some((recording, path)) = recording {
                    let path = path.display().to_string();
                    println!("[recv] {}: recording to {path}", recv.name);
                    let stop = stop_recording.clone().cancelled_owned();
                    let name = recv.name.clone();
                    recordings.spawn(async move {
                        let written = crate::record::finish(recording, stop).await?;
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

    // Finish recordings first, while their broadcasts are still open.
    stop_recording.cancel();
    while let Some(finished) = recordings.join_next().await {
        match finished {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(error = %err, "recording ended with an error"),
            Err(err) => warn!(error = %err, "a recording task panicked"),
        }
    }

    for (broadcast, _sources) in broadcasts {
        broadcast.close();
        broadcast.closed().await;
    }
    for receiver in &receivers {
        receiver.sub.close();
    }
    Ok(())
}

/// One live `[[recv]]` block.
struct Receiver {
    sub: Subscribed,
    _player: Option<Player>,
}

/// Publishes one `[[send]]` block.
async fn setup_send(live: &Live, config: &SendConfig) -> Result<(LocalBroadcast, source::Opened)> {
    let broadcast = LocalBroadcast::new();
    let sources = source::configure(&broadcast, &config.capture, None).await?;
    live.publish(&config.name, &broadcast)?;
    Ok((broadcast, sources))
}

/// Subscribes to one `[[recv]]` block, playing and recording as it asks.
///
/// Returns the recording and its path for the caller to finish. Audio that
/// fails to play is logged and skipped.
async fn setup_recv(
    live: &Live,
    config: &RecvConfig,
    output: Option<&media::AudioOutput>,
) -> Result<(Receiver, Option<(Recording, std::path::PathBuf)>)> {
    let ticket: BroadcastTicket = config.ticket.parse().map_err(|err| {
        anyerr!(
            "invalid ticket: {err}; it should be the string `irl publish` \
             printed, starting with `iroh-live:`"
        )
    })?;
    let sub = transport::subscribe(live, &ticket).await?;
    let catalog = crate::playback::catalog(sub.broadcast()).await?;

    let recording = match &config.record {
        None => None,
        Some(path) => {
            let mut options = RecordOptions::new(path.clone(), None)?;
            options.rendition = config.rendition.clone();
            let recording = crate::record::start(sub.broadcast(), &catalog, &options).await?;
            Some((recording, std::path::PathBuf::from(path)))
        }
    };

    let player = match (config.audio_output, output) {
        (AudioOutput::Default, Some(output)) => play_audio(&sub, &catalog, &config.name, output),
        _ => None,
    };
    Ok((
        Receiver {
            sub,
            _player: player,
        },
        recording,
    ))
}

/// Plays the broadcast's audio through `output`, with no video.
fn play_audio(
    sub: &Subscribed,
    catalog: &media::Catalog,
    name: &str,
    output: &media::AudioOutput,
) -> Option<Player> {
    if catalog.audio.renditions.is_empty() {
        info!(name, "the broadcast carries no audio");
        return None;
    }
    let config = PlayerConfig {
        rendition: RenditionMode::Off,
        audio: Some(output.clone()),
        ..PlayerConfig::default()
    };
    sub.broadcast()
        .play(config)
        .inspect_err(|err| warn!(name, error = %err, "audio failed to play"))
        .ok()
}

/// Checks that `name` is a single, non-empty file name.
///
/// `dir.join(name)` escapes `dir` for an absolute name or `..`. This catches
/// config typos. It is not a security check.
fn check_key_name(name: &str) -> Result<()> {
    if !name.is_empty() && Path::new(name).file_name() == Some(name.as_ref()) {
        return Ok(());
    }
    Err(anyerr!(
        "secret_key_name = {name:?} is not a file name; it names one file in \
         this platform's config directory, so it cannot be a path or empty",
    ))
}

/// Loads the named secret key, generating and storing one on first use.
fn stored_secret_key(name: &str) -> Result<SecretKey> {
    check_key_name(name)?;
    let dir = dirs::config_dir()
        .ok_or_else(|| anyerr!("cannot find this platform's config directory"))?
        .join("iroh-live")
        .join("secret_keys");
    std::fs::create_dir_all(&dir)
        .map_err(|err| anyerr!("failed to create {}: {err}", dir.display()))?;
    let path = dir.join(format!("{name}.key"));
    let key = secret_key_file(&path)
        .map_err(|err| anyerr!("failed to load {}: {err}", path.display()))?;
    info!(name, path = %path.display(), "session secret key ready");
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        args::{AudioCodecArg, DEFAULT_AUDIO, DEFAULT_VIDEO, VideoCodecArg},
        backend::Backend,
    };

    #[test]
    fn a_send_block_takes_the_flag_defaults() {
        let config: RunConfig = toml::from_str(
            r#"
            [[send]]
            name = "cam"
            "#,
        )
        .expect("only `name` is required");
        let send = &config.send[0].capture;
        assert_eq!(send.video, DEFAULT_VIDEO);
        assert_eq!(send.audio, DEFAULT_AUDIO);
        assert_eq!(send.encoder, Backend::Auto);
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
        let capture = &config.send[0].capture;
        assert_eq!(capture.video, "screen");
        assert_eq!(capture.codec, VideoCodecArg::H265);
        assert_eq!(capture.encoder, Backend::Named("vaapi"));
        assert_eq!(capture.renditions, ["low:320x180", "720p"]);
        assert_eq!(capture.bitrate, Some(3_000_000));
        assert!(capture.no_cursor);
        assert_eq!(capture.audio_codec, AudioCodecArg::Pcm);
        // A session file has no `--test-source`. It uses `video = "test"`.
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

    /// A key name that would escape the key directory is refused.
    #[test]
    fn a_key_name_is_one_file_name() {
        check_key_name("laptop").expect("an ordinary name");
        check_key_name("laptop.2").expect("a dot inside a name is still a name");

        for name in ["", "/tmp/evil", "../../evil", "a/b", ".", ".."] {
            assert!(
                check_key_name(name).is_err(),
                "{name:?} is not a file name in the key directory",
            );
        }
    }
}
