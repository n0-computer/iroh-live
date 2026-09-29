//! [`MoqPreset`]: iroh's N0 preset with a QUIC transport tuned for MoQ.

use std::sync::Arc;

use iroh::endpoint::{Builder, QuicTransportConfig, presets};
use noq_proto::congestion::Bbr3Config;

/// iroh's [`N0`](presets::N0) preset with BBR3 congestion control.
///
/// Works anywhere iroh takes a preset:
///
/// ```no_run
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let endpoint = iroh::Endpoint::bind(iroh_moq::MoqPreset).await?;
/// # Ok(())
/// # }
/// ```
///
/// moq-net sends every subscriber the publisher's send-rate estimate,
/// `cwnd / rtt`. A live publisher sends less than the link could take, and
/// under CUBIC, iroh's default, its window grows until something is lost, so
/// the estimate shows room to spare on a full link. BBR3 sizes the window from
/// the delivery rate it measures, so the estimate tracks the link.
#[derive(Debug, Clone, Copy, Default)]
pub struct MoqPreset;

impl presets::Preset for MoqPreset {
    fn apply(self, builder: Builder) -> Builder {
        presets::N0
            .apply(builder)
            .transport_config(transport_config())
    }
}

/// Returns the QUIC transport configuration [`MoqPreset`] installs.
fn transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .congestion_controller_factory(Arc::new(Bbr3Config::default()))
        .build()
}
