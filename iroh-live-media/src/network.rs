//! What a transport tells a player about the link it plays over.
//!
//! A transport attaches [`NetworkSignals`] to a
//! [`RemoteBroadcast`](crate::RemoteBroadcast) with
//! [`with_network`](crate::RemoteBroadcast::with_network). Every player of that
//! broadcast reads a [`NetworkSample`] from it a few times a second to choose a
//! rendition.

use std::time::Duration;

use crate::Bitrate;

/// A source of [`NetworkSample`]s for automatic rendition selection.
///
/// The player asks for a sample when it needs one. A closure returning a
/// [`NetworkSample`] implements this trait.
pub trait NetworkSignals: Send + Sync + 'static {
    /// Returns the link as it is now.
    ///
    /// Called a few times per second, and on every
    /// [`Player::stats`](crate::Player::stats). Must not block.
    fn sample(&self) -> NetworkSample;
}

impl<F> NetworkSignals for F
where
    F: Fn() -> NetworkSample + Send + Sync + 'static,
{
    fn sample(&self) -> NetworkSample {
        self()
    }
}

/// A shared [`NetworkSignals`].
#[derive(derive_more::Debug, Clone)]
#[debug("SharedSignals")]
pub(crate) struct SharedSignals(pub(crate) std::sync::Arc<dyn NetworkSignals>);

/// One reading of the link a broadcast arrives over.
///
/// Transports measure different things, so most fields are optional. A field
/// left `None` means unmeasured, not zero.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetworkSample {
    /// The round trip to the peer.
    pub rtt: Option<Duration>,
    /// The smallest recent round trip on the current path.
    ///
    /// This is the path's delay with nothing queued.
    pub min_rtt: Option<Duration>,
    /// The fraction of this endpoint's packets lost, in `0.0..=1.0`.
    ///
    /// On a subscriber these packets are mostly acknowledgements. The figure
    /// matches loss on the media's direction only when both directions are
    /// impaired alike.
    pub loss: Option<f32>,
    /// The sender's estimate of what the path to this endpoint delivers.
    ///
    /// This is the only field that measures capacity. Adaptation caps the
    /// rendition's bitrate with it.
    pub delivery: Option<Bitrate>,
    /// A counter that changes whenever the path changes.
    ///
    /// Adaptation drops its history when it changes.
    pub path_generation: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closure_is_a_signal_source() {
        let signals = || NetworkSample {
            loss: Some(0.5),
            ..Default::default()
        };
        fn read(signals: &impl NetworkSignals) -> NetworkSample {
            signals.sample()
        }
        assert_eq!(read(&signals).loss, Some(0.5));
    }
}
