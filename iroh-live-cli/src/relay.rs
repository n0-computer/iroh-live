//! `irl relay` — start a local relay server for browser WebTransport bridging.
//!
//! Delegates to the `iroh-live-relay` binary. The relay is a separate crate
//! with its own heavy dependencies (moq-relay, axum, TLS) that we do not
//! want to pull into the CLI.

use std::process::Command;

use clap::Args;

#[derive(Args, Debug)]
pub struct RelayArgs {
    /// Dev mode: self-signed TLS certificates, prints fingerprint URL.
    #[arg(long, default_value_t = true)]
    pub dev: bool,

    /// QUIC bind address (WebTransport and iroh).
    #[arg(long, default_value = "[::]:4443")]
    pub bind: String,

    /// HTTP bind address (static files and fingerprint endpoint).
    /// Defaults to the same as --bind.
    #[arg(long)]
    pub http_bind: Option<String>,
}

pub fn run(args: RelayArgs) -> n0_error::Result {
    let bin = find_relay_binary();

    let mut cmd = Command::new(&bin);
    if args.dev {
        cmd.arg("--dev");
    }
    cmd.arg("--bind").arg(&args.bind);
    if let Some(ref http_bind) = args.http_bind {
        cmd.arg("--http-bind").arg(http_bind);
    }

    tracing::info!(binary = %bin, "starting relay");
    let status = cmd
        .status()
        .map_err(|e| n0_error::anyerr!("failed to start iroh-live-relay: {e}. Is it installed?"))?;

    if !status.success() {
        n0_error::bail_any!("iroh-live-relay exited with {status}");
    }
    Ok(())
}

/// Finds the iroh-live-relay binary. Checks (in order):
/// 1. Next to the current executable (same directory as `irl`)
/// 2. On PATH
fn find_relay_binary() -> String {
    // Check next to current exe (common when both are cargo-installed or in target/debug/)
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("iroh-live-relay");
        if sibling.exists() {
            return sibling.to_string_lossy().into_owned();
        }
    }
    // Fall back to PATH lookup
    "iroh-live-relay".to_string()
}
