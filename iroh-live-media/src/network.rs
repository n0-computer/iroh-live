//! What a transport tells a player about the link it plays over.
//!
//! [`NetworkSignals`] is the one coupling between media and transport. A
//! transport attaches it to a [`RemoteBroadcast`](crate::RemoteBroadcast) with
//! [`with_network`](crate::RemoteBroadcast::with_network), and every player of
//! that broadcast reads a [`NetworkSample`] from it a few times a second to
//! choose a rendition. Nothing here names iroh or QUIC.

use std::{fmt, time::Duration};

use crate::Bitrate;

/// A source of [`NetworkSample`]s for automatic rendition selection.
///
/// Pull-style rather than a channel: adaptation samples on its own schedule,
/// and a transport computes a sample on demand. A closure returning a
/// [`NetworkSample`] implements it.
pub trait NetworkSignals: Send + Sync + 'static {
    /// Returns the link as it is now.
    ///
    /// Called a few times per second from the adaptation loop. Must not block.
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

/// A shared [`NetworkSignals`], as a broadcast holds it.
#[derive(Clone)]
pub(crate) struct SharedSignals(pub(crate) std::sync::Arc<dyn NetworkSignals>);

impl fmt::Debug for SharedSignals {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SharedSignals")
    }
}

/// One reading of the link a broadcast arrives over.
///
/// Every field is optional because every transport measures a different
/// subset, and a field left `None` is read as unmeasured rather than as zero.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetworkSample {
    /// The round trip to the peer.
    pub rtt: Option<Duration>,
    /// The smallest recent round trip on the current path: its delay with
    /// nothing queued.
    pub min_rtt: Option<Duration>,
    /// The fraction of this endpoint's packets lost, in `0.0..=1.0`.
    ///
    /// On a subscriber these are mostly acknowledgements, so this stands in for
    /// loss on the media's direction only as far as the two directions are
    /// impaired alike.
    pub loss: Option<f32>,
    /// The sender's estimate of what the path to this endpoint delivers.
    ///
    /// The one figure here that describes capacity rather than what arrived,
    /// and the one adaptation bounds the rendition's bitrate with.
    pub delivery: Option<Bitrate>,
    /// Bumped whenever the path changes, so history does not straddle two
    /// paths.
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
