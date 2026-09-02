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
#[cfg(feature = "render")]
mod call;
mod devices;
mod import;
mod publish;
mod record;
mod run;
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
        Command::Run(args) => run::run(args, &rt),
        Command::Watch(args) => watch::run(args, &rt),
    }
}
