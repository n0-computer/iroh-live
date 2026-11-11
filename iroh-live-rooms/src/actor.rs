//! The actor behind a room: it announces this member and follows the others.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use iroh::EndpointId;
use iroh_smol_kv::{Filter, SignedValue, Subscribe, SubscribeItem, SubscribeMode, WriteScope};
use n0_future::{StreamExt, task::AbortOnDropHandle};
use n0_watcher::{Watchable, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{Instrument, debug, info, trace, warn};

use crate::room::{Inner, Local, RoomPeer, RoomState};

/// The key a member's announcement is stored under in the room's gossip map.
pub(crate) const PEER_STATE_KEY: &[u8] = b"s";

/// How long a member's announcement lives in the gossip map without renewal.
///
/// smol-kv 0.4 sweeps every 30 seconds, so a vanished member drops off two to
/// two and a half minutes after its last renewal. The age is measured by the
/// member's own clock: a member whose clock runs more than this far behind is
/// never seen.
pub(crate) const STATE_HORIZON: Duration = Duration::from_secs(2 * 60);

/// How often a member rewrites its announcement to renew its lease.
pub(crate) const STATE_REFRESH: Duration = Duration::from_secs(30);

/// How often the gossip map should look for announcements past the horizon.
///
/// smol-kv 0.4 ignores it and sweeps every 30 seconds.
pub(crate) const EXPIRY_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// How long leaving waits for each step that needs the network.
///
/// Telling the others and stopping the gossip map wait on peers, who may be
/// gone.
pub(crate) const LEAVE_STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times in a row the room resubscribes to its gossip map.
///
/// smol-kv ends a subscription that falls behind, and a fresh one replays the
/// map. One that keeps ending means the map is gone.
pub(crate) const MAP_RESUBSCRIBES: u32 = 3;

/// A member's announcement in the room's gossip map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Announcement {
    /// The member's name and broadcasts.
    peer: RoomPeer,
    /// Whether the member left, so the others drop it before its lease runs out.
    left: bool,
}

/// The task behind a room: it announces this member and follows the others.
pub(crate) struct Actor {
    pub(crate) inner: Arc<Inner>,
    pub(crate) kv: iroh_smol_kv::Client,
    /// Writes this member's announcement into the gossip map.
    pub(crate) writer: WriteScope,
    /// The other members, as their announcements last said.
    pub(crate) peers: BTreeMap<EndpointId, RoomPeer>,
    /// The members the replay after a resubscription has shown so far.
    pub(crate) resync: Option<BTreeSet<EndpointId>>,
    /// Set once the [`Rooms`](crate::Rooms) this room belongs to shut down.
    pub(crate) rooms_closing: Arc<AtomicBool>,
}

impl Drop for Actor {
    /// Lets [`Room::leave`](crate::Room::leave) return, even if the actor panicked.
    fn drop(&mut self) {
        self.inner.done.set(true).ok();
    }
}

impl Actor {
    /// Runs until the room is left, the rooms shut down, or the gossip map ends.
    pub(crate) async fn run(mut self, mut inbox: mpsc::Receiver<oneshot::Sender<()>>) {
        let mut updates = Self::subscribe_map(self.kv.clone());

        // One writer, so announcements go out in the order they were decided.
        let desired = Watchable::new(self.announcement(false));
        let announcer = AbortOnDropHandle::new(tokio::spawn(
            announce(
                self.writer.clone(),
                desired.watch(),
                self.rooms_closing.clone(),
            )
            .in_current_span(),
        ));
        let mut local_changed = self.inner.local_changed.watch();
        let mut display_name = self.inner.display_name.watch();
        let mut resubscribes = 0;

        let leave = loop {
            let mut ended = false;
            tokio::select! {
                update = updates.next() => match update {
                    None => ended = true,
                    Some(Err(err)) if self.closing() => {
                        debug!(%err, "gossip map update failed while the rooms shut down");
                    }
                    Some(Err(err)) => warn!(%err, "gossip map update failed"),
                    Some(Ok(item)) => {
                        resubscribes = 0;
                        self.handle_item(item);
                    }
                },
                changed = local_changed.updated() => {
                    if changed.is_err() {
                        break None;
                    }
                    desired.set(self.announcement(false)).ok();
                }
                changed = display_name.updated() => {
                    if changed.is_err() {
                        break None;
                    }
                    desired.set(self.announcement(false)).ok();
                }
                reply = inbox.recv() => break reply,
            }
            if ended {
                // The gossip stopped under the map, which is how the rooms
                // shut down a room nobody left.
                if self.closing() {
                    debug!("the rooms shut down, stopping the room");
                    break None;
                }
                if resubscribes == MAP_RESUBSCRIBES {
                    warn!("the room's gossip map ended");
                    break None;
                }
                resubscribes += 1;
                warn!(
                    attempt = resubscribes,
                    "gossip map subscription ended, resubscribing"
                );
                updates = Self::subscribe_map(self.kv.clone());
                self.resync = Some(BTreeSet::new());
            }
        };
        // Stopped first, so a refresh cannot write over the announcement that
        // says this member left.
        drop(announcer);
        self.shut_down(leave.is_some()).await;
        if let Some(reply) = leave {
            reply.send(()).ok();
        }
    }

    /// Returns whether the [`Rooms`](crate::Rooms) this room belongs to are shutting down.
    fn closing(&self) -> bool {
        self.rooms_closing.load(Ordering::Relaxed)
    }

    /// Subscribes to the gossip map: every entry there is now, then changes.
    fn subscribe_map(
        kv: iroh_smol_kv::Client,
    ) -> std::pin::Pin<
        Box<impl n0_future::Stream<Item = Result<SubscribeItem, impl std::fmt::Display>> + Send>,
    > {
        // The raw stream rather than `stream()`, which drops the expiry items
        // membership is read out of.
        Box::pin(
            kv.subscribe_with_opts(Subscribe {
                mode: SubscribeMode::Both,
                filter: Filter::ALL,
            })
            .stream_raw(),
        )
    }

    /// Withdraws this member's publications, and tells the others if `leaving`.
    async fn shut_down(&mut self, leaving: bool) {
        if leaving {
            info!("leaving the room");
            let told = tokio::time::timeout(
                LEAVE_STEP_TIMEOUT,
                put(&self.writer, &self.announcement(true), &self.rooms_closing),
            )
            .await;
            if told.is_err() {
                warn!(
                    "could not tell the others in time; they drop this member when its lease ends"
                );
            }
        }
        let local: Vec<Local> = std::mem::take(&mut *self.inner.local.lock().expect("poisoned"))
            .into_values()
            .collect();
        for entry in local {
            entry.publication.unpublish();
        }
        self.peers.clear();
        self.publish_state();
        match tokio::time::timeout(LEAVE_STEP_TIMEOUT, self.kv.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => debug!(%err, "gossip map already stopped"),
            Err(_) => warn!("the gossip map did not stop in time"),
        }
        self.inner.done.set(true).ok();
    }

    /// Returns this member's announcement as it stands.
    fn announcement(&self, left: bool) -> Announcement {
        let broadcasts = if left {
            BTreeSet::new()
        } else {
            self.inner.local_names()
        };
        Announcement {
            peer: RoomPeer {
                display_name: self.inner.display_name.get(),
                broadcasts,
            },
            left,
        }
    }

    /// Applies one change of the gossip map to the membership.
    fn handle_item(&mut self, item: SubscribeItem) {
        match item {
            SubscribeItem::Entry(entry) => self.handle_entry(entry),
            SubscribeItem::Expired((remote, key, _timestamp)) => {
                if remote != self.inner.me
                    && key == PEER_STATE_KEY
                    && self.peers.remove(&remote).is_some()
                {
                    info!(
                        remote = %remote.fmt_short(),
                        horizon = ?STATE_HORIZON,
                        "member stopped announcing, treating it as gone",
                    );
                    self.publish_state();
                }
            }
            // The replay is complete. After a resubscription, a member it did
            // not show expired in the meantime.
            SubscribeItem::CurrentDone => {
                if let Some(seen) = self.resync.take() {
                    let before = self.peers.len();
                    self.peers.retain(|remote, _| seen.contains(remote));
                    if self.peers.len() != before {
                        info!(
                            gone = before - self.peers.len(),
                            "members expired while resubscribing"
                        );
                        self.publish_state();
                    }
                }
            }
        }
    }

    /// Applies a member's announcement to the membership.
    fn handle_entry(&mut self, (remote, key, value): (EndpointId, Bytes, SignedValue)) {
        if remote == self.inner.me || key != PEER_STATE_KEY {
            return;
        }
        let Ok(announcement) = postcard::from_bytes::<Announcement>(&value.value) else {
            warn!(
                remote = %remote.fmt_short(),
                len = value.value.len(),
                "member announcement does not decode",
            );
            return;
        };
        if announcement.left {
            if self.peers.remove(&remote).is_some() {
                info!(remote = %remote.fmt_short(), "member left the room");
                self.publish_state();
            }
            return;
        }
        if let Some(seen) = self.resync.as_mut() {
            seen.insert(remote);
        }
        // Every member rewrites its announcement to renew its lease, so most of
        // these say nothing new.
        let peer = announcement.peer;
        match self.peers.get(&remote) {
            Some(known) if *known == peer => {
                trace!(remote = %remote.fmt_short(), "announcement renewed");
                return;
            }
            Some(_) => debug!(
                remote = %remote.fmt_short(),
                broadcasts = ?peer.broadcasts,
                "member announcement changed",
            ),
            None => info!(
                remote = %remote.fmt_short(),
                display_name = ?peer.display_name,
                "member joined the room",
            ),
        }
        self.peers.insert(remote, peer);
        self.publish_state();
    }

    /// Publishes the membership to the state watcher and the audience set.
    fn publish_state(&self) {
        let members: BTreeSet<EndpointId> = self.peers.keys().copied().collect();
        self.inner.members.set(members).ok();
        self.inner
            .state
            .set(RoomState {
                members: self.peers.clone(),
            })
            .ok();
    }
}

/// Writes `desired` whenever it changes, and every [`STATE_REFRESH`].
async fn announce(
    writer: WriteScope,
    mut desired: n0_watcher::Direct<Announcement>,
    rooms_closing: Arc<AtomicBool>,
) {
    let mut refresh = tokio::time::interval(STATE_REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        put(&writer, &desired.get(), &rooms_closing).await;
        tokio::select! {
            changed = desired.updated() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = refresh.tick() => trace!("renewing the announcement"),
        }
    }
}

/// Writes `announcement` under [`PEER_STATE_KEY`].
///
/// A failure while the rooms shut down is expected, and logs at DEBUG only.
async fn put(writer: &WriteScope, announcement: &Announcement, rooms_closing: &AtomicBool) {
    let bytes = postcard::to_stdvec(announcement).expect("an announcement serializes");
    if let Err(err) = writer.put(PEER_STATE_KEY, bytes).await {
        if rooms_closing.load(Ordering::Relaxed) {
            debug!(%err, "could not write the announcement while the rooms shut down");
        } else {
            warn!(%err, "failed to write the announcement");
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    /// A member survives two missed refreshes.
    #[test]
    fn several_refreshes_fit_inside_the_expiry_horizon() {
        assert!(STATE_REFRESH * 3 <= STATE_HORIZON);
        assert!(EXPIRY_CHECK_INTERVAL < STATE_REFRESH);
    }

    /// A member leaving says so, and the flag survives the wire.
    #[test]
    fn a_leaving_announcement_decodes_as_left() {
        let announcement = Announcement {
            left: true,
            ..Announcement::default()
        };
        let bytes = postcard::to_stdvec(&announcement).expect("encode");
        let decoded: Announcement = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, announcement);
    }
}
