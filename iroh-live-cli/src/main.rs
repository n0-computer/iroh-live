//! `irl`: publish and watch live media over iroh.

// A binary crate has no external API, so the visibility lints that keep a
// library's surface honest only produce noise here.
#![allow(
    unreachable_pub,
    unnameable_types,
    reason = "binary crate, internal modules use pub for convenience"
)]

use clap::{Parser, Subcommand};

mod args;
mod backend;
#[cfg(feature = "render")]
mod call;
mod devices;
mod import;
mod publish;
mod record;
#[cfg(feature = "render")]
mod room;
mod run;
#[cfg(feature = "render")]
mod scan;
mod source;
mod source_spec;
mod transport;
#[cfg(feature = "render")]
mod ui;
mod watch;

/// Publish and watch live audio and video over iroh.
#[derive(Parser, Debug)]
#[command(name = "irl", about, version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Place or answer a 1:1 video call.
    #[cfg(feature = "render")]
    Call(Box<args::CallArgs>),
    /// List the cameras, displays, and audio devices this machine offers.
    Devices,
    /// Publish a capture device or a media file.
    Publish(Box<args::PublishArgs>),
    /// Subscribe to a remote broadcast and write it to a file, headless.
    Record(Box<args::RecordArgs>),
    /// Join a multi-party room: publish, and watch everybody else.
    #[cfg(feature = "render")]
    Room(Box<args::RoomArgs>),
    /// Run a multi-stream session described by a TOML file.
    Run(args::RunArgs),
    /// Subscribe to a remote broadcast and play it.
    #[command(visible_alias = "play")]
    Watch(args::WatchArgs),
}

fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // One runtime for every command. The windowed commands do their async
    // setup inside `block_on` and then hand the main thread to eframe, keeping
    // the runtime alive through an enter guard, because `block_on` inside an
    // egui callback would panic.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match cli.command {
        #[cfg(feature = "render")]
        Command::Call(args) => call::run(*args, &rt),
        Command::Devices => devices::run(&rt),
        Command::Publish(args) => publish::run(*args, &rt),
        Command::Record(args) => record::run(*args, &rt),
        #[cfg(feature = "render")]
        Command::Room(args) => room::run(*args, &rt),
        Command::Run(args) => run::run(args, &rt),
        Command::Watch(args) => watch::run(args, &rt),
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Command};

    /// The endpoint id and broadcast path a subscriber can be pointed at
    /// instead of a ticket.
    fn endpoint_and_name() -> (String, String) {
        let id = iroh::SecretKey::generate().public();
        (id.to_string(), "hello".to_string())
    }

    #[test]
    fn the_command_tree_is_valid() {
        // clap checks ids, defaults, and the relationships between arguments
        // only when the command is built, so a mistake in an `#[arg]`
        // attribute would otherwise reach a user as a panic.
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
        assert_eq!(args.remote.ticket().expect("resolves").broadcast_name, name);

        let cli = Cli::try_parse_from(["irl", "record", "--endpoint-id", &id, "--name", &name])
            .expect("record accepts exactly what watch does");
        let Command::Record(args) = cli.command else {
            panic!("expected record");
        };
        assert_eq!(args.remote.ticket().expect("resolves").broadcast_name, name);
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

    /// `--scan` is what names the broadcast, so nothing else has to.
    #[test]
    #[cfg(feature = "render")]
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
    #[cfg(feature = "render")]
    fn scanning_and_no_video_are_rejected_together() {
        Cli::try_parse_from(["irl", "watch", "--scan", "--no-video"])
            .expect_err("a scan needs a window to draw the camera in");
    }
}
