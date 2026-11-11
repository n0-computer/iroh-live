//! The room handle, and the path layout of room broadcasts.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use iroh::EndpointId;
use iroh_gossip::TopicId;
use iroh_live_node::{Audience, Layout, Node, Publication, Subscription};
use moq_net::{Pattern, Patterns, broadcast};
use n0_error::e;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};

use crate::{rooms::Error, ticket::RoomTicket};

/// Who is in a room, and what each member publishes into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomState {
    /// Every other member, by endpoint id. This node is not among them.
    pub members: BTreeMap<EndpointId, RoomPeer>,
}

/// One member of a room.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPeer {
    /// The name the member announced, if any.
    pub display_name: Option<String>,
    /// The broadcasts the member publishes into the room, by name.
    pub broadcasts: BTreeSet<String>,
}

/// A joined room.
///
/// Cheap to clone. The actor behind it runs while any clone exists, and
/// [`leave`](Self::leave) ends it for all of them.
#[derive(Debug, Clone)]
pub struct Room {
    pub(crate) inner: Arc<Inner>,
    pub(crate) _actor: Arc<AbortOnDropHandle<()>>,
}

/// What every handle of a room shares with its actor.
#[derive(derive_more::Debug)]
pub(crate) struct Inner {
    /// This node's endpoint id.
    pub(crate) me: EndpointId,
    pub(crate) ticket: RoomTicket,
    #[debug(skip)]
    pub(crate) node: Node,
    pub(crate) state: Watchable<RoomState>,
    /// The members' ids, which every publication into the room is offered to.
    pub(crate) members: Watchable<BTreeSet<EndpointId>>,
    /// Asks the actor to leave, and hears back once it did.
    #[debug(skip)]
    pub(crate) leave: mpsc::Sender<oneshot::Sender<()>>,
    /// This member's publications, by name.
    #[debug(skip)]
    pub(crate) local: Mutex<BTreeMap<String, Local>>,
    /// Bumped whenever [`Inner::local`] changes, so the actor re-announces.
    pub(crate) local_changed: Watchable<u64>,
    /// The name this member announces.
    pub(crate) display_name: Watchable<Option<String>>,
    /// Set once the actor has stopped.
    pub(crate) done: Watchable<bool>,
    /// Asks the actor to resubscribe to its gossip map, and hears back once it did.
    ///
    /// Only the `test-util` feature's hook sends on it.
    #[cfg(feature = "test-util")]
    #[debug(skip)]
    pub(crate) restart_map: mpsc::Sender<oneshot::Sender<()>>,
}

/// One of this member's publications.
pub(crate) struct Local {
    pub(crate) publication: Publication,
    /// Forgets the entry once the publication is withdrawn, however it goes.
    _withdrawn: AbortOnDropHandle<()>,
}

impl Room {
    /// Ends the room's gossip map subscription, as smol-kv does with one that falls behind.
    ///
    /// Returns once the room has resubscribed. It then resyncs its members
    /// from the replay. Only for tests of that path, which no test can reach
    /// otherwise.
    #[cfg(feature = "test-util")]
    #[doc(hidden)]
    pub async fn restart_map_subscription(&self) {
        let (reply, resubscribed) = oneshot::channel();
        if self.inner.restart_map.send(reply).await.is_ok() {
            resubscribed.await.ok();
        }
    }

    /// Returns a ticket for this room with this member as its bootstrap peer.
    #[must_use]
    pub fn ticket(&self) -> RoomTicket {
        self.inner.ticket.with_bootstrap(self.inner.me)
    }

    /// Returns the members and their broadcasts, as they change.
    #[must_use]
    pub fn state(&self) -> n0_watcher::Direct<RoomState> {
        self.inner.state.watch()
    }

    /// Publishes a broadcast into the room as `name`, to its members only, and returns it.
    ///
    /// The path is `rooms/<topic>/<this member>/<name>`. The broadcast is
    /// created in the node's route table, and `B` fills it, built from its
    /// `broadcast::Producer`: a `LocalBroadcast`, or the producer itself.
    /// Members read it with nothing copied in between, and a member that
    /// leaves loses it at once. The broadcast stays in this member's
    /// announcement until it closes, [`unpublish`](Self::unpublish) takes it
    /// out, or the room is left.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidName`] for an empty name or one holding a
    /// slash, [`Error::Node`] if the name is already published or `B`
    /// cannot be built, and [`Error::Left`] once the room was left.
    pub fn publish<B>(&self, name: &str) -> Result<B, Error>
    where
        B: TryFrom<broadcast::Producer, Error: std::error::Error + Send + Sync + 'static>,
    {
        // Built before the broadcast is announced, so no member finds it
        // without the tracks `B` creates, such as a catalog.
        self.publish_with(name, |producer| B::try_from(producer.clone()))
    }

    /// Takes `name` out of the room, if this member publishes it.
    ///
    /// Members lose the path, and what they read there ends. The broadcast
    /// stays open for whoever holds it, unannounced. Returns whether `name`
    /// was published.
    pub fn unpublish(&self, name: &str) -> bool {
        let publication = self
            .inner
            .local
            .lock()
            .expect("poisoned")
            .get(name)
            .map(|local| local.publication.clone());
        match publication {
            Some(publication) => {
                // Its withdrawn task takes the name out of the announcement.
                publication.unpublish();
                true
            }
            None => false,
        }
    }

    /// Publishes `name` into the room, filled by `fill` before it is announced.
    fn publish_with<T, E>(
        &self,
        name: &str,
        fill: impl FnOnce(&broadcast::Producer) -> Result<T, E>,
    ) -> Result<T, Error>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        if name.is_empty() || name.contains('/') {
            return Err(e!(Error::InvalidName {
                name: name.to_owned()
            }));
        }
        if self.inner.done.get() {
            return Err(e!(Error::Left));
        }
        let topic = self.inner.ticket.topic_id();
        let (publication, filled) = self.inner.node.publish_with(
            room_path(topic, self.inner.me, name),
            Audience::Peers(self.inner.members.clone()),
            fill,
        )?;
        info!(%name, path = %publication.path(), "published into the room");
        {
            let mut local = self.inner.local.lock().expect("poisoned");
            // Spawned under the lock, so the task cannot run before the entry
            // exists.
            let withdrawn = {
                let inner = Arc::downgrade(&self.inner);
                let name = name.to_owned();
                let publication = publication.clone();
                AbortOnDropHandle::new(tokio::spawn(async move {
                    publication.withdrawn().await;
                    if let Some(inner) = inner.upgrade() {
                        debug!(%name, "room broadcast withdrawn");
                        inner.remove_local(&name, &publication);
                    }
                }))
            };
            local.insert(
                name.to_owned(),
                Local {
                    publication: publication.clone(),
                    _withdrawn: withdrawn,
                },
            );
        }
        self.inner.bump_local();
        Ok(filled)
    }

    /// Resolves `peer`'s broadcast `name` over the session with that member.
    ///
    /// Dials the member if needed, and reads what the member itself announces
    /// on that session, so no other peer can stand in for it. Waits until the
    /// member announces `name` to this node, so bound the wait if the member
    /// may never publish it.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Left`] once the room was left, and with
    /// [`Error::Node`] if the member cannot be dialed, if the node's
    /// policy does not let the member publish under its [`RoomsLayout`] scope, or if
    /// the session ends before the member announces `name`.
    pub async fn subscribe(&self, peer: EndpointId, name: &str) -> Result<Subscription, Error> {
        if self.inner.done.get() {
            return Err(e!(Error::Left));
        }
        let path = room_path(self.inner.ticket.topic_id(), peer, name);
        let session = self.inner.node.connect(peer).await?;
        Ok(session.subscribe(path).await?)
    }

    /// Sets the name other members see, or clears it.
    pub fn set_display_name(&self, name: Option<String>) {
        self.inner.display_name.set(name).ok();
    }

    /// Leaves the room for every clone of this handle.
    ///
    /// Tells the other members and withdraws this member's publications.
    /// Subscriptions made with [`subscribe`](Self::subscribe) stay open, so
    /// drop them too. Returns within seconds even if every other member is
    /// gone. Idempotent. Not cancellation safe: call it again to finish.
    pub async fn leave(&self) {
        let (reply, reply_rx) = oneshot::channel();
        if self.inner.leave.send(reply).await.is_ok() {
            reply_rx.await.ok();
        }
        let mut done = self.inner.done.watch();
        while !done.get() {
            if done.updated().await.is_err() {
                break;
            }
        }
    }
}

impl Drop for Inner {
    /// Unpublishes this member's broadcasts when the last handle drops.
    ///
    /// Applies when [`Room::leave`] was not called, so that rejoining can
    /// publish the same names.
    fn drop(&mut self) {
        for local in self.local.get_mut().expect("poisoned").values() {
            local.publication.unpublish();
        }
    }
}

impl Inner {
    /// Forgets the local publication `name`, if it is still `publication`.
    ///
    /// Compared by identity: `name` may have been published anew since, and
    /// the new publication must stay.
    pub(crate) fn remove_local(&self, name: &str, publication: &Publication) {
        let mut local = self.local.lock().expect("poisoned");
        if local
            .get(name)
            .is_some_and(|entry| entry.publication == *publication)
        {
            local.remove(name);
            drop(local);
            self.bump_local();
        }
    }

    /// Signals the actor that [`Inner::local`] changed.
    pub(crate) fn bump_local(&self) {
        self.local_changed.set(self.local_changed.get() + 1).ok();
    }

    /// Returns the names this member publishes into the room.
    pub(crate) fn local_names(&self) -> BTreeSet<String> {
        self.local
            .lock()
            .expect("poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

/// The path layout of room broadcasts: `rooms/<topic>/<member>/<name>`.
///
/// A member publishes under `rooms/*/<member>/**`, and owns the room paths
/// that name it in their canonical spelling. A node that runs rooms lets every
/// peer publish there, as `LiveLayout::policy(&[Arc::new(RoomsLayout)])` in
/// iroh-live does.
#[derive(Debug, Clone, Copy, Default)]
pub struct RoomsLayout;

impl Layout<EndpointId> for RoomsLayout {
    fn scope(&self, peer: &EndpointId) -> Patterns {
        Patterns::from(publish_scope(*peer))
    }

    fn owner(&self, path: &str) -> Option<EndpointId> {
        parse_room_path(path).map(|(_topic, member, _name)| member)
    }
}

/// Returns the paths `member` publishes room broadcasts under, `rooms/*/<member>/**`.
fn publish_scope(member: EndpointId) -> Pattern {
    format!("rooms/*/{member}/**")
        .parse()
        .expect("an endpoint id is a valid path segment")
}

/// Returns the path a room broadcast lives at: `rooms/<topic>/<publisher>/<name>`.
pub(crate) fn room_path(topic: TopicId, publisher: EndpointId, name: &str) -> String {
    format!("rooms/{topic}/{publisher}/{name}")
}

/// Returns the topic, publisher and name of a room broadcast's path.
///
/// The inverse of the room path layout, `rooms/<topic>/<publisher>/<name>`.
/// Returns `None` for any other path, and for a topic or publisher that is
/// not spelled the way the layout spells it.
fn parse_room_path(path: &str) -> Option<(TopicId, EndpointId, &str)> {
    let (topic_str, rest) = path.strip_prefix("rooms/")?.split_once('/')?;
    let (publisher_str, name) = rest.split_once('/')?;
    let topic: TopicId = topic_str.parse().ok()?;
    let publisher: EndpointId = publisher_str.parse().ok()?;
    let canonical = topic.to_string() == topic_str && publisher.to_string() == publisher_str;
    (canonical && !name.is_empty()).then_some((topic, publisher, name))
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn a_room_path_names_topic_and_publisher() {
        let topic = TopicId::from_bytes([7; 32]);
        let publisher = SecretKey::from_bytes(&[3; 32]).public();
        let path = room_path(topic, publisher, "cam");
        assert_eq!(path, format!("rooms/{topic}/{publisher}/cam"));
        assert!(publish_scope(publisher).matches(&path));
        assert!(!publish_scope(SecretKey::from_bytes(&[4; 32]).public()).matches(&path));
    }

    #[test]
    fn the_rooms_layout_owns_what_it_scopes() {
        let topic = TopicId::from_bytes([5; 32]);
        let (member, other) = (
            SecretKey::from_bytes(&[6; 32]).public(),
            SecretKey::from_bytes(&[7; 32]).public(),
        );
        let scope = RoomsLayout.scope(&member);
        for name in ["cam", "cam/hd"] {
            let path = room_path(topic, member, name);
            assert!(scope.matches(&path), "{path}");
            assert_eq!(RoomsLayout.owner(&path), Some(member), "{path}");
            assert!(!RoomsLayout.scope(&other).matches(&path), "{path}");
        }
        assert_eq!(RoomsLayout.owner(&format!("live/{member}/cam")), None);
    }

    #[test]
    fn a_room_path_parses_back() {
        let topic = TopicId::from_bytes([3; 32]);
        let publisher = SecretKey::from_bytes(&[4; 32]).public();
        let path = room_path(topic, publisher, "cam/hd");
        assert_eq!(parse_room_path(&path), Some((topic, publisher, "cam/hd")));
        assert_eq!(
            parse_room_path(&format!("rooms/{topic}/{publisher}/")),
            None
        );
        assert_eq!(
            parse_room_path(&format!("rooms/topic/{publisher}/cam")),
            None
        );
        assert_eq!(
            parse_room_path(&format!("live/{topic}/{publisher}/cam")),
            None
        );
        let upper = publisher.to_string().to_uppercase();
        assert_eq!(parse_room_path(&format!("rooms/{topic}/{upper}/cam")), None);
    }
}
