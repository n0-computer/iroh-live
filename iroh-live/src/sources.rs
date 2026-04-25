//! Transport sources for multi-origin publish and subscribe.
//!
//! A [`TransportSource`] names one place where a broadcast can be
//! reached or published: a direct iroh peer, a moq-relay via H3, or
//! a future peer that agreed to relay for another peer. A
//! [`SourceSet`] is an ordered list of candidates; index zero is the
//! preferred source. A [`SourceSetHandle`] wraps a set in a
//! [`Watchable`] so publishers and subscribers react to runtime
//! mutations.
//!
//! Subscribers route the active subscription through the source the
//! [`SelectionPolicy`] picks. The default [`PreferOrdered`] policy
//! picks the highest-priority candidate that has a live session and
//! a confirmed broadcast announcement.
//!
//! Publishers attach a single producer to all sessions named by the
//! set; a removed source ends the announce on that session only.

use iroh::{EndpointAddr, EndpointId};
use n0_watcher::Watchable;
use serde::{Deserialize, Serialize};

use crate::relay::RelayTarget;

/// One place where a broadcast can be reached or published.
///
/// Constructed by callers and grouped into a [`SourceSet`]. Carries
/// only identity and reachability information, no session state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportSource {
    /// A direct iroh peer. Subscribers dial the peer; the peer's
    /// router accepts the inbound session.
    Direct(DirectSource),
    /// A moq-relay reached via H3-over-iroh.
    Relay(RelayTarget),
}

impl TransportSource {
    /// Constructs a direct-peer source from an endpoint address.
    pub fn direct(peer: impl Into<EndpointAddr>) -> Self {
        Self::Direct(DirectSource { peer: peer.into() })
    }

    /// Constructs a relay source from a [`RelayTarget`].
    pub fn relay(target: RelayTarget) -> Self {
        Self::Relay(target)
    }

    /// Returns the stable identifier for this source.
    ///
    /// Two sources with the same identifier denote the same
    /// endpoint and, for relays, the same URL path. A [`SourceSet`]
    /// uses the identifier to dedupe entries and to track which
    /// source is currently active across mutations.
    pub fn id(&self) -> SourceId {
        match self {
            Self::Direct(d) => SourceId::direct(d.peer.id),
            Self::Relay(t) => SourceId::relay(t.endpoint(), t.path()),
        }
    }

    /// Returns `true` when this source points at a direct peer.
    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct(_))
    }

    /// Returns `true` when this source points at a relay.
    pub fn is_relay(&self) -> bool {
        matches!(self, Self::Relay(_))
    }
}

impl From<EndpointAddr> for TransportSource {
    fn from(addr: EndpointAddr) -> Self {
        Self::direct(addr)
    }
}

impl From<EndpointId> for TransportSource {
    fn from(id: EndpointId) -> Self {
        Self::direct(EndpointAddr::new(id))
    }
}

impl From<RelayTarget> for TransportSource {
    fn from(target: RelayTarget) -> Self {
        Self::relay(target)
    }
}

/// A direct iroh peer.
///
/// Holds the full [`EndpointAddr`] so callers can pass relay urls
/// and direct addresses alongside the endpoint id when useful for
/// connectivity hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectSource {
    /// The peer's endpoint address.
    pub peer: EndpointAddr,
}

/// Stable identifier for a [`TransportSource`].
///
/// Direct sources are identified by endpoint id; relay sources by
/// endpoint id combined with the URL path so two relay sources on
/// the same relay endpoint with different paths coexist in the same
/// set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(SourceIdInner);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SourceIdInner {
    Direct(EndpointId),
    Relay { endpoint: EndpointId, path: String },
}

impl SourceId {
    /// Identifier for a direct-peer source.
    pub fn direct(peer: EndpointId) -> Self {
        Self(SourceIdInner::Direct(peer))
    }

    /// Identifier for a relay source.
    pub fn relay(endpoint: EndpointId, path: impl Into<String>) -> Self {
        Self(SourceIdInner::Relay {
            endpoint,
            path: path.into(),
        })
    }

    /// Returns `true` when this identifier names a direct source.
    pub fn is_direct(&self) -> bool {
        matches!(self.0, SourceIdInner::Direct(_))
    }

    /// Returns `true` when this identifier names a relay source.
    pub fn is_relay(&self) -> bool {
        matches!(self.0, SourceIdInner::Relay { .. })
    }

    /// Returns the endpoint id of the target.
    pub fn endpoint(&self) -> EndpointId {
        match &self.0 {
            SourceIdInner::Direct(id) => *id,
            SourceIdInner::Relay { endpoint, .. } => *endpoint,
        }
    }

    /// Returns the relay path, or `None` for direct sources.
    pub fn relay_path(&self) -> Option<&str> {
        match &self.0 {
            SourceIdInner::Direct(_) => None,
            SourceIdInner::Relay { path, .. } => Some(path.as_str()),
        }
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            SourceIdInner::Direct(id) => write!(f, "direct:{}", id.fmt_short()),
            SourceIdInner::Relay { endpoint, path } => {
                write!(f, "relay:{}{}", endpoint.fmt_short(), path)
            }
        }
    }
}

/// Ordered list of candidate [`TransportSource`]s.
///
/// Index zero is the preferred source; subsequent entries are
/// fallbacks. Duplicates are dropped on [`SourceSet::push`] so two
/// calls to push with the same [`SourceId`] are a no-op after the
/// first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceSet {
    sources: Vec<TransportSource>,
}

impl SourceSet {
    /// Creates an empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a set with a single direct-peer source.
    pub fn direct(peer: impl Into<EndpointAddr>) -> Self {
        Self {
            sources: vec![TransportSource::direct(peer)],
        }
    }

    /// Creates a set with a single relay source.
    pub fn relay(target: RelayTarget) -> Self {
        Self {
            sources: vec![TransportSource::relay(target)],
        }
    }

    /// Appends a source. No-op when a source with the same
    /// [`SourceId`] is already in the set.
    pub fn push(&mut self, source: TransportSource) -> &mut Self {
        let id = source.id();
        if self.sources.iter().any(|s| s.id() == id) {
            return self;
        }
        self.sources.push(source);
        self
    }

    /// Removes the source identified by `id`. Returns `true` when a
    /// source was removed.
    pub fn remove(&mut self, id: &SourceId) -> bool {
        let before = self.sources.len();
        self.sources.retain(|s| s.id() != *id);
        self.sources.len() != before
    }

    /// Returns `true` when a source with the given identifier is in
    /// the set.
    pub fn contains(&self, id: &SourceId) -> bool {
        self.sources.iter().any(|s| s.id() == *id)
    }

    /// Returns the source at `idx`, or `None` if out of range.
    pub fn get(&self, idx: usize) -> Option<&TransportSource> {
        self.sources.get(idx)
    }

    /// Finds a source by its identifier.
    pub fn find(&self, id: &SourceId) -> Option<&TransportSource> {
        self.sources.iter().find(|s| s.id() == *id)
    }

    /// Returns an iterator over all sources in priority order.
    pub fn iter(&self) -> std::slice::Iter<'_, TransportSource> {
        self.sources.iter()
    }

    /// Returns the number of sources.
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Returns `true` when the set is empty.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Returns the preferred source, which is the first entry.
    pub fn preferred(&self) -> Option<&TransportSource> {
        self.sources.first()
    }

    /// Wraps this set in a [`SourceSetHandle`] for reactive mutation.
    pub fn into_handle(self) -> SourceSetHandle {
        SourceSetHandle::new(self)
    }
}

impl<I> FromIterator<I> for SourceSet
where
    I: Into<TransportSource>,
{
    fn from_iter<T: IntoIterator<Item = I>>(iter: T) -> Self {
        let mut set = Self::new();
        for item in iter {
            set.push(item.into());
        }
        set
    }
}

impl From<TransportSource> for SourceSet {
    fn from(source: TransportSource) -> Self {
        let mut set = Self::new();
        set.push(source);
        set
    }
}

impl From<DirectSource> for SourceSet {
    fn from(source: DirectSource) -> Self {
        Self::from(TransportSource::Direct(source))
    }
}

impl From<RelayTarget> for SourceSet {
    fn from(target: RelayTarget) -> Self {
        Self::from(TransportSource::Relay(target))
    }
}

impl From<EndpointAddr> for SourceSet {
    fn from(addr: EndpointAddr) -> Self {
        Self::direct(addr)
    }
}

impl From<EndpointId> for SourceSet {
    fn from(id: EndpointId) -> Self {
        Self::direct(EndpointAddr::new(id))
    }
}

/// Reactive handle around a [`SourceSet`].
///
/// Wraps a [`Watchable`] so callers observe mutations through a
/// [`Watcher`](n0_watcher::Watcher). Clones share the same
/// underlying set.
#[derive(Debug, Clone)]
pub struct SourceSetHandle {
    inner: Watchable<SourceSet>,
}

impl SourceSetHandle {
    /// Creates a handle from an existing set.
    pub fn new(set: SourceSet) -> Self {
        Self {
            inner: Watchable::new(set),
        }
    }

    /// Creates an empty handle.
    pub fn empty() -> Self {
        Self::new(SourceSet::new())
    }

    /// Returns a snapshot of the current set.
    pub fn get(&self) -> SourceSet {
        self.inner.get()
    }

    /// Returns a watcher that fires on each mutation.
    pub fn watch(&self) -> n0_watcher::Direct<SourceSet> {
        self.inner.watch()
    }

    /// Replaces the full set.
    pub fn set(&self, set: SourceSet) {
        self.inner.set(set).ok();
    }

    /// Applies `f` to a clone of the current set and publishes the
    /// result. The closure runs without holding a lock so it must
    /// not assume atomic read-modify-write semantics; the writer
    /// races against any other writer.
    pub fn update(&self, f: impl FnOnce(&mut SourceSet)) {
        let mut set = self.inner.get();
        f(&mut set);
        self.inner.set(set).ok();
    }

    /// Appends a source. No-op when an entry with the same
    /// [`SourceId`] is already present.
    pub fn push(&self, source: TransportSource) {
        self.update(|s| {
            s.push(source);
        });
    }

    /// Removes the source with the given id. Returns `true` when a
    /// source was removed.
    pub fn remove(&self, id: &SourceId) -> bool {
        let mut set = self.inner.get();
        let removed = set.remove(id);
        if removed {
            self.inner.set(set).ok();
        }
        removed
    }
}

impl<S> From<S> for SourceSetHandle
where
    S: Into<SourceSet>,
{
    fn from(set: S) -> Self {
        Self::new(set.into())
    }
}

/// Selection policy for the unified subscription.
///
/// Picks an active source from a ranked list of candidates given
/// observed liveness. Implementations may apply hysteresis through
/// the `current` argument.
pub trait SelectionPolicy: Send + Sync + 'static {
    /// Picks the active source from the candidates.
    ///
    /// `current` carries the previously active source, when any, so
    /// the policy can apply hysteresis or stickiness.
    fn pick(&self, candidates: &[Candidate<'_>], current: Option<&SourceId>) -> Option<SourceId>;
}

/// A [`TransportSource`] with its observed liveness.
#[derive(Debug)]
pub struct Candidate<'a> {
    /// The source itself.
    pub source: &'a TransportSource,
    /// `true` when the session to this source is currently alive
    /// and the broadcast has been confirmed announced.
    pub healthy: bool,
}

/// Picks the highest-priority healthy source.
///
/// Prefers sources by the order in which the [`SourceSet`] holds
/// them. When the active source becomes unhealthy and a lower-
/// priority source is healthy, switches to the lower-priority one.
/// Switches back as soon as a higher-priority source recovers; no
/// hysteresis is applied at this layer.
#[derive(Debug, Default, Clone, Copy)]
pub struct PreferOrdered;

impl SelectionPolicy for PreferOrdered {
    fn pick(&self, candidates: &[Candidate<'_>], _current: Option<&SourceId>) -> Option<SourceId> {
        candidates.iter().find(|c| c.healthy).map(|c| c.source.id())
    }
}

/// Pins selection to one specific source by id.
///
/// Useful for tests and for callers that want to disable dynamic
/// switching entirely. When the pinned source is unhealthy, no
/// alternative is picked; the consumer waits.
#[derive(Debug, Clone)]
pub struct Pinned(pub SourceId);

impl SelectionPolicy for Pinned {
    fn pick(&self, candidates: &[Candidate<'_>], _current: Option<&SourceId>) -> Option<SourceId> {
        candidates
            .iter()
            .find(|c| c.source.id() == self.0 && c.healthy)
            .map(|c| c.source.id())
    }
}

/// Offer to reach a broadcast through a relay.
///
/// Suitable for embedding in tickets or gossip announcements; the
/// receiver builds a [`RelayTarget`] from this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayOffer {
    /// Endpoint id of the relay.
    pub endpoint: EndpointId,
    /// URL path the client should observe when connecting.
    pub path: String,
    /// Optional JWT for the H3 handshake's `jwt` query parameter.
    pub jwt: Option<String>,
}

impl RelayOffer {
    /// Returns a [`RelayTarget`] reflecting this offer.
    pub fn to_target(&self) -> RelayTarget {
        RelayTarget::new(self.endpoint)
            .with_path(&self.path)
            .with_api_key(self.jwt.clone())
    }
}

impl From<&RelayOffer> for RelayTarget {
    fn from(offer: &RelayOffer) -> Self {
        offer.to_target()
    }
}

impl From<RelayOffer> for RelayTarget {
    fn from(offer: RelayOffer) -> Self {
        offer.to_target()
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;
    use n0_watcher::Watcher;

    use super::*;

    fn peer_addr() -> EndpointAddr {
        EndpointAddr::from(SecretKey::generate().public())
    }

    fn relay_target(path: &str) -> RelayTarget {
        RelayTarget::new(SecretKey::generate().public()).with_path(path)
    }

    #[test]
    fn source_id_distinguishes_direct_from_relay() {
        let p = peer_addr();
        let direct = TransportSource::direct(p.clone());
        let relay = TransportSource::relay(relay_target("/"));
        assert_ne!(direct.id(), relay.id());
        assert!(direct.id().is_direct());
        assert!(relay.id().is_relay());
        assert_eq!(direct.id().relay_path(), None);
        assert_eq!(relay.id().relay_path(), Some("/"));
    }

    #[test]
    fn relay_ids_differ_by_path() {
        let endpoint = SecretKey::generate().public();
        let a = TransportSource::relay(RelayTarget::new(endpoint).with_path("/a"));
        let b = TransportSource::relay(RelayTarget::new(endpoint).with_path("/b"));
        assert_ne!(a.id(), b.id());
        assert_eq!(a.id().endpoint(), b.id().endpoint());
    }

    #[test]
    fn source_set_dedupes_by_id() {
        let p = peer_addr();
        let mut set = SourceSet::new();
        set.push(TransportSource::direct(p.clone()));
        set.push(TransportSource::direct(p.clone()));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn source_set_remove_works() {
        let p = peer_addr();
        let d = TransportSource::direct(p);
        let id = d.id();
        let mut set = SourceSet::new();
        set.push(d);
        assert!(set.remove(&id));
        assert!(set.is_empty());
        assert!(!set.remove(&id));
    }

    #[test]
    fn from_iterator_dedupes() {
        let p = peer_addr();
        let d = TransportSource::direct(p);
        let r = TransportSource::relay(relay_target("/"));
        let set: SourceSet = [d.clone(), d, r].into_iter().collect();
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn into_source_set_for_endpoint_addr() {
        let addr = peer_addr();
        let set: SourceSet = addr.clone().into();
        assert_eq!(set.len(), 1);
        assert!(set.preferred().expect("preferred").is_direct());
    }

    #[test]
    fn into_source_set_for_relay_target() {
        let set: SourceSet = relay_target("/x").into();
        assert_eq!(set.len(), 1);
        assert!(set.preferred().expect("preferred").is_relay());
    }

    #[test]
    fn prefer_ordered_picks_first_healthy() {
        let p = peer_addr();
        let r = relay_target("/");
        let sources = [TransportSource::direct(p), TransportSource::relay(r)];
        let candidates = vec![
            Candidate {
                source: &sources[0],
                healthy: false,
            },
            Candidate {
                source: &sources[1],
                healthy: true,
            },
        ];
        let policy = PreferOrdered;
        let picked = policy.pick(&candidates, None).expect("some source");
        assert_eq!(picked, sources[1].id());
    }

    #[test]
    fn prefer_ordered_prefers_direct_when_healthy() {
        let p = peer_addr();
        let r = relay_target("/");
        let sources = [TransportSource::direct(p), TransportSource::relay(r)];
        let candidates = vec![
            Candidate {
                source: &sources[0],
                healthy: true,
            },
            Candidate {
                source: &sources[1],
                healthy: true,
            },
        ];
        let picked = PreferOrdered.pick(&candidates, None).expect("some source");
        assert_eq!(picked, sources[0].id());
    }

    #[test]
    fn prefer_ordered_picks_none_when_all_unhealthy() {
        let p = peer_addr();
        let r = relay_target("/");
        let sources = [TransportSource::direct(p), TransportSource::relay(r)];
        let candidates = vec![
            Candidate {
                source: &sources[0],
                healthy: false,
            },
            Candidate {
                source: &sources[1],
                healthy: false,
            },
        ];
        assert_eq!(PreferOrdered.pick(&candidates, None), None);
    }

    #[test]
    fn pinned_picks_only_its_target() {
        let p = peer_addr();
        let r = relay_target("/");
        let sources = [TransportSource::direct(p), TransportSource::relay(r)];
        let candidates = vec![
            Candidate {
                source: &sources[0],
                healthy: true,
            },
            Candidate {
                source: &sources[1],
                healthy: true,
            },
        ];
        let pinned = Pinned(sources[1].id());
        assert_eq!(
            pinned.pick(&candidates, None).expect("pinned"),
            sources[1].id()
        );
    }

    #[test]
    fn pinned_returns_none_when_unhealthy() {
        let p = peer_addr();
        let sources = [TransportSource::direct(p)];
        let candidates = vec![Candidate {
            source: &sources[0],
            healthy: false,
        }];
        let pinned = Pinned(sources[0].id());
        assert_eq!(pinned.pick(&candidates, None), None);
    }

    #[test]
    fn handle_mutations_propagate_to_watchers() {
        let handle = SourceSet::direct(peer_addr()).into_handle();
        let mut watcher = handle.watch();
        assert_eq!(handle.get().len(), 1);

        handle.push(TransportSource::relay(relay_target("/")));
        let _ = watcher.get();
        assert_eq!(handle.get().len(), 2);
    }

    #[test]
    fn relay_offer_round_trips_through_target() {
        let endpoint = SecretKey::generate().public();
        let offer = RelayOffer {
            endpoint,
            path: "/r".into(),
            jwt: Some("token".into()),
        };
        let target: RelayTarget = (&offer).into();
        assert_eq!(target.endpoint(), endpoint);
        assert_eq!(target.path(), "/r");
        assert_eq!(target.api_key(), Some("token"));
    }
}
