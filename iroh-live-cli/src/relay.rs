//! `irl relay` — start a local relay server for browser WebTransport bridging.

pub use iroh_live_relay::RelayConfig;

pub fn run(config: RelayConfig, rt: &tokio::runtime::Runtime) -> n0_error::Result {
    rt.block_on(async {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .ok(); // May already be installed by another command.
        iroh_live_relay::run(config).await?;
        n0_error::Ok(())
    })
}
