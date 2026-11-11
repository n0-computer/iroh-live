//! Link identity and measurement: the node's shared sample cell and the relay monitor.
//!
//! The sampler itself lives in `moq-transport-iroh`.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use moq_ext_access::LinkKind;
use moq_transport_iroh::{LinkSample, LinkSampler, SAMPLE_INTERVAL};
use tracing::trace;

/// Identifies one link of a node: a session or a relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Display)]
#[display("link-{_0}")]
pub struct LinkId(pub(crate) u64);

/// The link serving a subscription, and its latest reading.
#[derive(Debug, Clone, PartialEq)]
pub struct ServingLink {
    /// The session's or relay link's id.
    pub id: LinkId,
    /// What kind of link that is.
    pub kind: LinkKind,
    /// The link's latest reading.
    pub sample: LinkSample,
}

/// The latest sample of a link, shared with every handle on it.
#[derive(Debug, Clone, Default)]
pub(crate) struct LinkState(Arc<Mutex<LinkSample>>);

impl LinkState {
    /// Returns the latest reading.
    pub(crate) fn get(&self) -> LinkSample {
        *self.0.lock().expect("poisoned")
    }

    /// Empties the reading, as between sessions, keeping `path_generation`.
    pub(crate) fn clear(&self, path_generation: u64) {
        let mut sample = LinkSample::default();
        sample.network.path_generation = path_generation;
        self.set(sample);
    }

    pub(crate) fn set(&self, sample: LinkSample) {
        trace!(
            path_generation = sample.network.path_generation,
            relayed = sample.relayed,
            rtt_ms = ?sample.network.rtt.map(|rtt| rtt.as_millis()),
            min_rtt_ms = ?sample.network.min_rtt.map(|rtt| rtt.as_millis()),
            loss = ?sample.network.loss,
            throughput_kbps = ?sample.throughput_bps.map(|bps| bps / 1000),
            delivery_kbps = ?sample.network.delivery.map(|bps| bps / 1000),
            "link sample",
        );
        *self.0.lock().expect("poisoned") = sample;
    }
}

/// A sampler shared by the monitors of one link's successive sessions.
pub(crate) type SharedSampler = Arc<Mutex<LinkSampler>>;

/// Reads a direct session into `state` until its connection closes.
pub(crate) async fn monitor_direct(
    session: moq_net::Session,
    connection: iroh::endpoint::Connection,
    state: LinkState,
    sampler: SharedSampler,
) {
    moq_transport_iroh::sample_link(session, connection, sampler, move |sample| {
        state.set(sample);
    })
    .await;
}

/// Reads a relay link into `state` across its reconnects.
pub(crate) async fn monitor_relay(monitor: moq_tokio::connection::Monitor, state: LinkState) {
    let mut sampler = LinkSampler::default();
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let sample = match monitor.stats() {
            Some(stats) => {
                let session = monitor.presence().sessions_started;
                sampler.sample_session(session, &stats, Instant::now())
            }
            // Between sessions nothing is measured.
            None => {
                let mut sample = LinkSample::default();
                sample.network.path_generation = sampler.generation();
                sample
            }
        };
        state.set(sample);
    }
}
