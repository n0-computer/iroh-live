//! What a transport tells a player about the link it plays over.
//!
//! A transport attaches a function returning a [`NetworkSample`] to a
//! [`RemoteBroadcast`](crate::RemoteBroadcast) with
//! [`with_network`](crate::RemoteBroadcast::with_network). Every player of that
//! broadcast calls it a few times a second to choose a rendition.

use std::{sync::Arc, time::Duration};

use crate::Bitrate;

/// A transport's view of the link, attached with [`crate::RemoteBroadcast::with_network`].
pub(crate) type NetworkSignals = Arc<dyn Fn() -> NetworkSample + Send + Sync>;

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
    pub loss: Option<f64>,
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
