//! What the transport tells a subscriber about its downlink.
//!
//! [`NetworkSignals`] is the whole interface between whatever carries a
//! broadcast and [`crate::adaptive`]. Nothing here depends on iroh or on QUIC:
//! a caller polls its own connection and fills the struct in.

use std::time::Duration;

/// Transport-level network quality signals for adaptive rendition selection.
///
/// Produced by polling QUIC connection stats. Consumed by
/// [`VideoTrack::enable_adaptation`](crate::subscribe::VideoTrack::enable_adaptation)
/// to decide when to switch renditions.
///
/// Every figure here is measured by one endpoint about one path, which on a
/// subscriber means most of them describe the wrong direction: QUIC reports a
/// congestion window, a loss count and a congestion event count for what this
/// endpoint sends, and a subscriber sends little but acknowledgements. The one
/// figure that is genuinely about the downlink is
/// [`NetworkSignals::goodput_bps`], and it says what arrived rather than what
/// could have.
#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkSignals {
    /// Round-trip time to the remote peer.
    ///
    /// The only signal here that covers both directions, since the reply has to
    /// come back through whatever is delaying the downlink. That makes it the
    /// one way a receiver sees a bottleneck that is only saturated the other
    /// way: the queue in front of it holds up the acknowledgements too.
    ///
    /// Sampled sparsely on a subscriber, because QUIC takes a round-trip sample
    /// only from a packet that asks to be acknowledged and a subscriber mostly
    /// sends acknowledgements, which do not. Expect a reading that is minutes
    /// old to still be the current one, read it against
    /// [`NetworkSignals::min_rtt`] rather than in absolute terms, and use
    /// [`NetworkSignals::rtt_samples`] to tell a fresh reading from a repeat.
    pub rtt: Duration,
    /// The number of distinct [`NetworkSignals::rtt`] readings taken since the
    /// connection opened.
    ///
    /// A latched round trip and a freshly measured one are the same number read
    /// twice, and a consumer that cannot tell them apart counts elapsed time as
    /// though it were evidence: one bad sample then satisfies any hold shorter
    /// than the gap to the next reading, which on a subscriber is most of them.
    /// Comparing this counter across two readings says whether the round trip in
    /// the later one is new.
    pub rtt_samples: u64,
    /// The smallest [`NetworkSignals::rtt`] seen recently on the path now
    /// selected.
    ///
    /// The path's propagation delay with no queue in front of it, which is what
    /// makes the current round trip mean anything: 40ms is an idle
    /// intercontinental path or a badly congested local one, and only the
    /// difference between the two tells them apart.
    ///
    /// Recently, and on the path now selected, because both qualifications are
    /// what keep it honest. A minimum that remembers every path a connection
    /// ever took reads a fallback from a direct path to a relay as a queue that
    /// will never drain, and one that remembers every moment reads a link whose
    /// baseline moved for any other reason the same way.
    ///
    /// Zero means unmeasured. No real path reports one, and
    /// [`crate::adaptive`] reads it that way: without a baseline there is
    /// nothing to call a queue against, so it calls none.
    pub min_rtt: Duration,
    /// Recent packet loss rate in `0.0..=1.0`, computed over a 200ms delta
    /// window.
    ///
    /// Loss among the packets this endpoint sent, which on a subscriber are its
    /// acknowledgements. It stands in for loss on the downlink only as far as
    /// the two directions are impaired alike, which holds for a lossy radio or a
    /// saturated hop and not for much else. Read it as a symmetric-path proxy
    /// rather than as a count of what the subscriber failed to receive: QUIC
    /// offers a receiver no such count.
    pub loss_rate: f64,
    /// Recently observed downlink goodput in bits per second, or `None` while
    /// too little is arriving to measure one.
    ///
    /// Goodput, not capacity: it is the rate at which bytes turned up, so it is
    /// bounded by what the publisher chose to send and only becomes a reading of
    /// the link once the link is the thing holding it back. That makes it a
    /// lower bound on what the path can carry, which is enough to show a
    /// rendition failing to arrive in full but never enough to show room above
    /// the rate already flowing. Finding that room is what
    /// [`Decision::StartProbe`](crate::adaptive::Decision::StartProbe) is for.
    ///
    /// `None` while the arriving traffic is too thin to be media, so that a
    /// publisher going quiet reads as an absence of evidence rather than as a
    /// link that collapsed.
    pub goodput_bps: Option<u64>,
    /// The publisher's estimate of what the path to this subscriber can carry,
    /// in bits per second, or `None` while it has not sent one.
    ///
    /// The one figure here that describes capacity rather than what arrived:
    /// the sending side's congestion controller measures the rate the path
    /// delivers at, and moq-net carries that estimate here on the wire. Read
    /// it as an upper bound the way [`NetworkSignals::goodput_bps`] is a lower
    /// one, with two limits. It is only as good as the controller under it,
    /// and a loss-based one on a sender that has never filled its window
    /// reports the window, not the path. And an older publisher sends none, so
    /// a rule built on it needs another for when it is absent.
    pub delivery_bps: Option<u64>,
    /// Monotonically increasing congestion event counter.
    ///
    /// Congestion this endpoint's own sending ran into, so it carries the same
    /// caveat as [`NetworkSignals::loss_rate`].
    pub congestion_events: u64,
}
