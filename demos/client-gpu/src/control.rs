//! The control track: the client tells the gpu node which filter to apply.
//!
//! The client publishes a small broadcast at `live/<client id>/ctl` for the gpu
//! node only. It holds one track, `ctl`, and every frame is the filter name as
//! UTF-8. A change is sent at once, and the state is repeated every second, so
//! a reader that joins late, or misses a group, converges.

use std::time::Duration;

use anyhow::Result;
use iroh::EndpointId;
use iroh_live::{Audience, BroadcastTicket, Live, Publication, Reach};
use moq_net::{Timescale, Timestamp, broadcast, track};
use n0_future::task::{AbortOnDropHandle, spawn};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::filter::Filter;

/// The name of the control broadcast and of its one track.
const NAME: &str = "ctl";

/// How often the current state is repeated.
const REPEAT: Duration = Duration::from_secs(1);

/// Returns the path of `peer`'s control broadcast.
fn path(peer: EndpointId) -> String {
    BroadcastTicket::new(peer, NAME).path()
}

/// Publishes the filter in `filter` to `gpu` until dropped.
#[derive(Debug)]
pub struct Sender {
    _task: AbortOnDropHandle<()>,
}

impl Sender {
    /// Starts publishing the control broadcast to `gpu`.
    ///
    /// # Errors
    ///
    /// Fails if the broadcast cannot be published.
    pub fn spawn(
        live: &Live,
        gpu: EndpointId,
        mut filter: watch::Receiver<Filter>,
    ) -> Result<Self> {
        let producer = broadcast::Info::new().produce();
        let info = track::Info::default().with_timescale(Timescale::MILLI);
        let mut track = producer.create_track(NAME, info)?;
        let peers = std::collections::BTreeSet::from([gpu]);
        let publication = live.publish_at(
            &path(live.endpoint().id()),
            producer.consume(),
            Audience::Peers(n0_watcher::Watchable::new(peers)),
        )?;
        let task = spawn(async move {
            // The producer and the publication live as long as the task.
            let _keep: (broadcast::Producer, Publication) = (producer, publication);
            let mut tick = tokio::time::interval(REPEAT);
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    changed = filter.changed() => if changed.is_err() { break },
                }
                let name = filter.borrow_and_update().name();
                if let Err(err) = track.write_frame(Timestamp::now(), name) {
                    warn!(%err, "control write failed");
                    break;
                }
            }
        });
        Ok(Self {
            _task: AbortOnDropHandle::new(task),
        })
    }
}

/// Reads the client's control track and publishes the filter into `out`.
///
/// Returns when the track ends.
///
/// # Errors
///
/// Fails if the client's control broadcast cannot be reached or breaks.
pub async fn receive(live: &Live, client: EndpointId, out: watch::Sender<Filter>) -> Result<()> {
    let subscription = live
        .subscribe_path(&path(client), Reach::Direct(client))
        .await?;
    let mut track = subscription.as_moq().track(NAME)?.subscribe(None).await?;
    while let Some(mut group) = track.recv_group().await? {
        while let Some(frame) = group.read_frame().await? {
            match std::str::from_utf8(&frame.payload)
                .ok()
                .and_then(Filter::parse)
            {
                Some(filter) => {
                    let changed = out
                        .send_if_modified(|current| std::mem::replace(current, filter) != filter);
                    if changed {
                        info!(filter = filter.name(), "filter selected");
                    }
                }
                None => debug!("ignoring an unknown control message"),
            }
        }
    }
    Ok(())
}
