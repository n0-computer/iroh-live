//! Link identity and measurement: the latest sample of each link, and its monitors.
//!
//! The sampler itself lives in `moq-transport-iroh`.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use moq_ext_access::LinkKind;
use moq_transport_iroh::{LinkSampler, NetworkSample, SAMPLE_INTERVAL};
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
    pub sample: NetworkSample,
}

/// The latest sample of a link, shared with every handle on it.
#[derive(Debug, Clone, Default)]
pub(crate) struct LatestSample(Arc<Mutex<NetworkSample>>);

impl LatestSample {
    /// Returns the latest reading.
    pub(crate) fn get(&self) -> NetworkSample {
        *self.0.lock().expect("poisoned")
    }

    /// Empties the reading, as between sessions, keeping `path_generation`.
    pub(crate) fn clear(&self, path_generation: u64) {
        self.set(NetworkSample {
            path_generation,
            ..NetworkSample::default()
        });
    }

    /// Replaces the reading with `sample`.
    pub(crate) fn set(&self, sample: NetworkSample) {
        trace!(
            path_generation = sample.path_generation,
            relayed = sample.relayed,
            rtt_ms = ?sample.rtt.map(|rtt| rtt.as_millis()),
            min_rtt_ms = ?sample.min_rtt.map(|rtt| rtt.as_millis()),
            loss = ?sample.loss,
            throughput_kbps = ?sample.throughput.map(|bps| bps / 1000),
            delivery_kbps = ?sample.delivery.map(|bps| bps / 1000),
            "link sample",
        );
        *self.0.lock().expect("poisoned") = sample;
    }
}

/// A sampler shared by the monitors of one link's successive sessions.
pub(crate) type SharedSampler = Arc<Mutex<LinkSampler>>;

/// Reads a direct session into `latest` until its connection closes.
pub(crate) async fn monitor_direct(
    session: moq_net::Session,
    connection: iroh::endpoint::Connection,
    latest: LatestSample,
    sampler: SharedSampler,
) {
    moq_transport_iroh::sample_link(session, connection, sampler, move |sample| {
        latest.set(sample);
    })
    .await;
}

/// Reads a relay link into `latest` across its reconnects.
pub(crate) async fn monitor_relay(monitor: moq_tokio::connection::Monitor, latest: LatestSample) {
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
            None => NetworkSample {
                path_generation: sampler.path_generation(),
                ..NetworkSample::default()
            },
        };
        latest.set(sample);
    }
}
