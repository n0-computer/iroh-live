//! `irl`: publish and watch live media over iroh.

use clap::{Parser, Subcommand};

mod args;
mod backend;
mod call;
mod devices;
mod playback;
mod publish;
mod record;
mod rendition;
mod room;
mod run;
mod scan;
mod shutdown;
mod source;
mod source_spec;
mod transport;
mod ui;
mod watch;

/// Publish and watch live audio and video over iroh.
#[derive(Parser, Debug)]
#[command(name = "irl", about, version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The `irl` subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Place or answer a 1:1 video call.
    Call(Box<args::CallArgs>),
    /// List cameras, displays, and audio devices.
    Devices,
    /// Publish a capture device or a media file.
    Publish(Box<args::PublishArgs>),
    /// Record a remote broadcast to a file, without a window.
    Record(Box<args::RecordArgs>),
    /// Join a room: publish, and watch everyone else.
    Room(Box<args::RoomArgs>),
    /// Run a multi-stream session described by a TOML file.
    Run(args::RunArgs),
    /// Subscribe to a remote broadcast and play it.
    #[command(visible_alias = "play")]
    Watch(Box<args::WatchArgs>),
}

/// Parses the command line and runs the chosen command.
fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // Windowed commands set up inside `block_on`, then hand the main thread to
    // eframe with the runtime entered. `block_on` in an egui callback panics.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let result = match cli.command {
        Command::Call(args) => call::run(*args, &rt),
        Command::Devices => devices::run(&rt),
        Command::Publish(args) => publish::run(*args, &rt),
        Command::Record(args) => record::run(*args, &rt),
        Command::Room(args) => room::run(*args, &rt),
        Command::Run(args) => run::run(args, &rt),
        Command::Watch(args) => watch::run(*args, &rt),
    };
    // Waits for the runtime's blocking tasks. After a Ctrl+C, the shutdown
    // watchdog bounds the wait.
    shutdown::step("stopping the runtime");
    drop(rt);
    result
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Command};

    /// Returns an endpoint id and broadcast name to use instead of a ticket.
    fn endpoint_and_name() -> (String, String) {
        let id = iroh::SecretKey::generate().public();
        (id.to_string(), "hello".to_string())
    }

    #[test]
    fn the_command_tree_is_valid() {
        // clap validates `#[arg]` attributes only when it builds the command.
        // Without this test a mistake panics at the user.
        Cli::command().debug_assert();
    }

    #[test]
    fn watch_and_record_name_a_broadcast_the_same_way() {
        let (id, name) = endpoint_and_name();

        let cli = Cli::try_parse_from(["irl", "watch", "--endpoint-id", &id, "--name", &name])
            .expect("the pair is one of the two accepted forms");
        let Command::Watch(args) = cli.command else {
            panic!("expected watch");
        };
        assert_eq!(
            args.remote.ticket().expect("resolves").path().as_str(),
            format!("live/{id}/{name}")
        );

        let cli = Cli::try_parse_from(["irl", "record", "--endpoint-id", &id, "--name", &name])
            .expect("record accepts exactly what watch does");
        let Command::Record(args) = cli.command else {
            panic!("expected record");
        };
        assert_eq!(
            args.remote.ticket().expect("resolves").path().as_str(),
            format!("live/{id}/{name}")
        );
    }

    #[test]
    fn a_broadcast_named_no_way_at_all_is_rejected() {
        let cli = Cli::try_parse_from(["irl", "watch"]).expect("clap accepts no arguments");
        let Command::Watch(args) = cli.command else {
            panic!("expected watch");
        };
        let err = args.remote.ticket().expect_err("nothing names a broadcast");
        assert!(
            err.to_string().contains("--endpoint-id"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn the_endpoint_id_needs_a_name() {
        let (id, _) = endpoint_and_name();
        Cli::try_parse_from(["irl", "watch", "--endpoint-id", &id])
            .expect_err("--endpoint-id alone does not name a broadcast");
    }

    /// `--scan` names the broadcast, so no ticket is needed.
    #[test]
    fn scanning_stands_in_for_a_ticket() {
        let cli = Cli::try_parse_from(["irl", "watch", "--scan"])
            .expect("--scan supplies the ticket the camera is about to read");
        let Command::Watch(args) = cli.command else {
            panic!("expected watch");
        };
        assert!(args.scan);
        assert!(args.remote.ticket().is_err(), "nothing named a broadcast");
    }

    /// The scan screen is a window, and `--no-video` opens none.
    #[test]
    fn scanning_and_no_video_are_rejected_together() {
        Cli::try_parse_from(["irl", "watch", "--scan", "--no-video"])
            .expect_err("a scan needs a window to draw the camera in");
    }

    /// A `file:` video brings its own audio unless `--audio` names another.
    #[test]
    fn a_file_video_brings_its_audio() {
        let cli = Cli::try_parse_from(["irl", "publish", "--video", "file:clip.mp4:loop"])
            .expect("a file video parses");
        let Command::Publish(args) = cli.command else {
            panic!("expected publish");
        };
        assert!(!args.capture.reencode);
        assert_eq!(
            args.capture.audio_source().to_string(),
            "file:clip.mp4:loop"
        );
    }

    /// Parses `irl publish` with `flags` after a `file:` video.
    fn publish_file(flags: &[&str]) -> Result<crate::args::PublishArgs, clap::Error> {
        let base = ["irl", "publish", "--video", "file:clip.mp4"];
        let cli = Cli::try_parse_from(base.iter().chain(flags))?;
        let Command::Publish(args) = cli.command else {
            panic!("expected publish");
        };
        Ok(*args)
    }

    /// A flag only an encoder acts on decodes the file, as `--reencode` does.
    #[test]
    fn encoding_flags_imply_reencode() {
        for flags in [
            &["--codec", "h265"][..],
            &["--encoder", "hw"],
            &["--renditions", "360p"],
            &["--bitrate", "1000000"],
            &["--fps", "25"],
            &["--audio-codec", "pcm"],
        ] {
            let args = publish_file(flags).expect("the flags parse");
            assert!(
                args.capture.reencodes().expect("no --no-reencode"),
                "{flags:?}"
            );
        }
        let args = publish_file(&[]).expect("a file video parses");
        assert!(!args.capture.reencodes().expect("no flags"));
    }

    /// `--no-reencode` refuses an encoding flag and contradicts `--reencode`.
    #[test]
    fn no_reencode_refuses_encoding_flags() {
        let args = publish_file(&["--no-reencode", "--width", "640", "--codec", "h265"])
            .expect("clap leaves the check to the source");
        let err = args
            .capture
            .reencodes()
            .expect_err("the flags need an encoder");
        assert!(err.to_string().contains("--codec, --width"), "{err}");
        publish_file(&["--no-reencode", "--reencode"]).expect_err("the two conflict");
    }
}
