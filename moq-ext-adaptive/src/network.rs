//! What a transport tells a player about the link it plays over.

use std::time::Duration;

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
    /// The sender's estimate of what the path delivers, in bits per second.
    ///
    /// The estimate is for the path to this endpoint. It is the only field
    /// that measures capacity. Adaptation caps the rendition's bitrate with
    /// it.
    pub delivery: Option<u64>,
    /// A counter that changes whenever the path changes.
    ///
    /// Adaptation drops its history when it changes.
    pub path_generation: u64,
    /// The rate bytes arrived at, in bits per second.
    ///
    /// The count includes packet overhead and duplicates, so it is no
    /// capacity. Adaptation does not read it.
    pub throughput: Option<u64>,
    /// Whether the path goes through a relay server rather than straight to the peer.
    pub relayed: bool,
}
