//! The relay binary: [`iroh_live_relay::run`] behind a command line.

use clap::Parser;

/// The relay's command line.
#[derive(Parser)]
#[command(about = "iroh-live relay: bridges iroh P2P and WebTransport/browser clients")]
struct Cli {
    #[command(flatten)]
    relay: iroh_live_relay::Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install crypto provider");

    // The relay's startup and session lines are at info, which `RUST_LOG`
    // left unset would hide.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    iroh_live_relay::run(cli.relay).await
}
