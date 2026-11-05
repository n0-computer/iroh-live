//! [`MoqPreset`]: iroh's N0 preset with a QUIC transport tuned for MoQ, and
//! the [`Congestion`] controllers to choose from.

use std::{fmt, str::FromStr, sync::Arc};

use iroh::endpoint::{Builder, QuicTransportConfig, presets};
use noq_proto::congestion::{Bbr3Config, ControllerFactory, CubicConfig, NewRenoConfig};

/// iroh's [`N0`](presets::N0) preset with BBR3 congestion control.
///
/// Works anywhere iroh takes a preset:
///
/// ```no_run
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let endpoint = iroh::Endpoint::bind(moq_transport_iroh::MoqPreset).await?;
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
            .transport_config(Congestion::default().transport_config())
    }
}

/// A QUIC congestion controller.
///
/// [`MoqPreset`] binds an endpoint with [`Bbr3`](Self::Bbr3), which keeps
/// moq-net's send-rate estimate on the link. The others are there to compare
/// against. An endpoint uses one controller for every connection, and
/// [`dial_with`](crate::dial_with) can give one dial another through
/// [`transport_config`](Self::transport_config).
///
/// It parses from its name, ignoring case, and displays as it.
///
/// ```
/// use moq_transport_iroh::Congestion;
///
/// let congestion: Congestion = "cubic".parse().expect("a known controller");
/// assert_eq!(congestion, Congestion::Cubic);
/// assert_eq!(congestion.to_string(), "CUBIC");
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Congestion {
    /// BBR3, which paces to the delivery rate it measures.
    #[default]
    Bbr3,
    /// CUBIC, iroh's own default, which grows its window until a packet is lost.
    Cubic,
    /// NewReno, the classic loss-based controller.
    NewReno,
}

impl Congestion {
    /// Every controller, BBR3 first.
    pub const ALL: [Self; 3] = [Self::Bbr3, Self::Cubic, Self::NewReno];

    /// Returns the name it displays as and parses from.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Bbr3 => "BBR3",
            Self::Cubic => "CUBIC",
            Self::NewReno => "NewReno",
        }
    }

    /// Returns a QUIC transport config with this controller and iroh's other defaults.
    ///
    /// Pass it to an endpoint builder's `transport_config`, or to one dial in
    /// `iroh::endpoint::ConnectOptions`.
    #[must_use]
    pub fn transport_config(self) -> QuicTransportConfig {
        QuicTransportConfig::builder()
            .congestion_controller_factory(self.factory())
            .build()
    }

    /// Returns the factory that builds this controller for each path.
    fn factory(self) -> Arc<dyn ControllerFactory + Send + Sync + 'static> {
        match self {
            Self::Bbr3 => Arc::new(Bbr3Config::default()),
            Self::Cubic => Arc::new(CubicConfig::default()),
            Self::NewReno => Arc::new(NewRenoConfig::default()),
        }
    }
}

impl fmt::Display for Congestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Congestion {
    type Err = UnknownCongestion;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|congestion| congestion.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| UnknownCongestion(name.to_string()))
    }
}

/// An error returned when a name is not that of a [`Congestion`] controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCongestion(String);

impl fmt::Display for UnknownCongestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown congestion controller {:?}: expected bbr3, cubic or newreno",
            self.0
        )
    }
}

impl std::error::Error for UnknownCongestion {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_controller_parses_from_its_name() {
        for congestion in Congestion::ALL {
            assert_eq!(congestion.to_string().parse(), Ok(congestion));
            assert_eq!(congestion.name().to_lowercase().parse(), Ok(congestion));
        }
        assert!("vegas".parse::<Congestion>().is_err());
    }
}
