#![allow(
    unreachable_pub,
    reason = "binary crate, internal modules use pub for convenience"
)]

use clap::{Parser, Subcommand};

mod args;
#[cfg(feature = "wgpu")]
mod call;
mod devices;
mod import;
#[cfg(feature = "wgpu")]
mod play;
mod publish;
#[cfg(feature = "wgpu")]
mod room;
mod source;
mod transport;
#[cfg(feature = "wgpu")]
mod ui;

#[derive(Parser)]
#[command(name = "irl", about = "iroh-live CLI — publish, play, call, and room")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List available cameras, screens, audio devices, and codecs.
    Devices,
    /// Publish capture or file over iroh.
    ///
    /// Subcommands: `capture` (default if omitted), `file`.
    /// Serves locally by default; use --relay/--room to push elsewhere.
    Publish(args::PublishArgs),
    /// Subscribe and play a remote broadcast.
    #[cfg(feature = "wgpu")]
    Play(args::PlayArgs),
    /// 1:1 bidirectional video call.
    #[cfg(feature = "wgpu")]
    Call(args::CallArgs),
    /// Multi-party room (publish + play grid).
    #[cfg(feature = "wgpu")]
    Room(args::RoomArgs),
}

fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // All commands share a single tokio runtime. GUI commands do async setup
    // inside block_on(), then run eframe *outside* block_on() so that
    // Handle::current().block_on() inside egui callbacks doesn't panic.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    match cli.command {
        Command::Devices => devices::run(),
        Command::Publish(args) => publish::run(args, &rt),
        #[cfg(feature = "wgpu")]
        Command::Play(args) => play::run(args, &rt),
        #[cfg(feature = "wgpu")]
        Command::Call(args) => call::run(args, &rt),
        #[cfg(feature = "wgpu")]
        Command::Room(args) => room::run(args, &rt),
    }
}
