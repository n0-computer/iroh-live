//! The node's bookkeeping: publications, links, and what each link may see.
//!
//! Everything here changes under one lock and never awaits, so `publish` and
//! `unpublish` are synchronous.
//!
//! Whenever a publication, an audience or the set of links changes, the state
//! works the grants out again and re-authorizes each link's session. What
//! links announce goes straight into the table, so the state only maps a
//! route's source back to its link (see [`State::link_for`]). The lock order
//! is on `Shared::state`.

use std::{
    collections::{BTreeMap, HashMap},
    time::SystemTime,
};

use iroh::EndpointId;
use moq_ext_access::LinkRole;
use moq_net::{Path, Pattern, auth, origin, origin::Source};
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    Grant, LinkId, LinkKind, Session, handshake::Hop, link::LinkState, policy::Authorization,
    publish::AudienceKind,
};

/// The node's publications and links.
#[derive(Debug, Default)]
pub(crate) struct State {
    next_id: u64,
    pub(crate) publications: BTreeMap<u64, PubEntry>,
    pub(crate) links: BTreeMap<LinkId, LinkEntry>,
    /// Set once the node shuts down; nothing is added after that.
    pub(crate) closed: bool,
    /// Paths that tickets widened a peer's grant to, with how many claims hold each.
    tickets: HashMap<EndpointId, BTreeMap<Path, usize>>,
    /// The policy's authorization of each peer with a live or pending direct session.
    peers: HashMap<EndpointId, PeerEntry>,
    /// Orders the starts of decisions and the triggers that make them stale.
    ///
    /// Bumped by every decision that starts and every trigger, so comparing
    /// two epochs tells which came first.
    epoch: u64,
    /// The epoch of the last trigger that covered every peer.
    marked_all: u64,
}

/// A peer's authorization, as the node applies it to the peer's sessions.
///
/// The entry lives while the peer has a registered session or one whose
/// authorization is being decided or handshaken, so a trigger reaches a
/// peer whose first session has not registered yet.
#[derive(Debug, Default)]
struct PeerEntry {
    /// The authorization every session with the peer shares.
    ///
    /// `None` before the first session registers and after a rejection, so
    /// the peer's links may do nothing.
    authorization: Option<Authorization>,
    /// The authorization's expiry on moq's clock, worked out once when it is stored.
    ///
    /// moq skips a grant equal to the last one, so the expiry must not move
    /// on every re-authorization.
    expires: Option<moq_net::time::Instant>,
    /// Whether the authorization expired before a new decision replaced it.
    ///
    /// A lapsed authorization grants nothing, so an expired token loses its
    /// rights at expiry, not when a slow policy answers.
    lapsed: bool,
    /// The epoch at which the decision behind `authorization` started.
    decided: u64,
    /// The epoch of the last trigger or rejection for the peer.
    marked: u64,
    /// How many sessions are between their authorization and their registration.
    pending: usize,
    /// Whether the peer has a registered session.
    live: bool,
}

impl PeerEntry {
    /// Stores `authorization`, decided at epoch `decided`.
    fn set(&mut self, authorization: Authorization, decided: u64) {
        let left = authorization
            .expires
            .map(|expires| expires.duration_since(SystemTime::now()));
        self.lapsed = matches!(left, Some(Err(_)));
        self.expires = left
            .and_then(|left| moq_net::time::Instant::now().checked_add(left.unwrap_or_default()));
        self.authorization = Some(authorization);
        self.decided = decided;
    }

    /// Returns the grant the peer's links take now, if the peer is authorized.
    fn grant(&self) -> Option<Grant> {
        let authorization = self.authorization.as_ref()?;
        Some(match self.lapsed {
            true => Grant::nothing(),
            false => authorization.grant.clone(),
        })
    }
}

/// The authorization a session starts from, read before its decision or handshake.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    /// The peer's stored authorization, if it holds one.
    pub(crate) stored: Option<Authorization>,
    /// The epoch the stored authorization was decided at, or the epoch now.
    ///
    /// Without a stored authorization, a decision for the session starts at
    /// this epoch.
    pub(crate) epoch: u64,
}

/// How [`State::register`] reconciled a session's authorization with the peer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Registered {
    /// The session's authorization is the newest and now holds for the peer.
    Applied,
    /// The peer holds a newer authorization, which the session takes.
    Kept,
    /// A trigger or a rejection came after the session's authorization was
    /// decided. The peer keeps an older authorization, or holds nothing,
    /// until the node decides again.
    Stale,
}

/// How many paths a ticket may widen one peer's grant to.
const MAX_TICKET_PATHS: usize = 64;

/// How many bytes of paths a ticket may widen one peer's grant to.
const MAX_TICKET_BYTES: usize = 16 * 1024;

/// Why a ticket path could not be claimed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClaimRefused {
    /// The node has shut down.
    Closed,
    /// The peer holds too many ticket paths.
    TooMany,
}

/// One publication.
#[derive(derive_more::Debug)]
pub(crate) struct PubEntry {
    pub(crate) path: Path,
    /// The broadcast in the route table, and its announcement.
    #[debug(skip)]
    pub(crate) publication: origin::Route,
    pub(crate) audience: AudienceKind,
    /// Follows a `Peers` audience's set.
    pub(crate) peers_task: Option<AbortOnDropHandle<()>>,
    /// Counts the audiences set so far.
    ///
    /// A `peers_task` writes only while it matches the generation it was
    /// started for, since aborting a task does not stop one that already
    /// waits on the lock.
    pub(crate) audience_generation: u64,
    /// Withdraws the publication once its broadcast ends.
    pub(crate) _closed_task: AbortOnDropHandle<()>,
    /// Cancelled when the entry goes, for [`Publication::withdrawn`](crate::Publication::withdrawn).
    pub(crate) withdrawn: CancellationToken,
}

impl Drop for PubEntry {
    fn drop(&mut self) {
        // Out of the table, for local readers too. The broadcast lives on for
        // whoever holds it, unannounced.
        self.publication.withdraw();
        self.withdrawn.cancel();
    }
}

/// One link: a direct session or a relay.
#[derive(derive_more::Debug)]
pub(crate) struct LinkEntry {
    pub(crate) kind: LinkKind,
    pub(crate) remote: Option<EndpointId>,
    /// The identity the link's routes are attributed to in the route table.
    pub(crate) hop: Hop,
    /// What a relay may do at most, before audiences.
    ///
    /// `None` for a direct link, which takes its peer's base grant.
    pub(crate) grant: Option<Grant>,
    /// Whether the link feeds the route table.
    pub(crate) consume: bool,
    /// The link's sessions' authorization, which moq enforces on them.
    #[debug(skip)]
    pub(crate) auth: auth::Handle,
    /// The session, for a direct link.
    pub(crate) session: Option<Session>,
    /// The latest reading of the link's connection monitor.
    #[debug(skip)]
    pub(crate) link_state: LinkState,
}

impl State {
    /// Allocates an id for a publication or a link.
    pub(crate) fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Adds a link and authorizes it for what it may see now.
    pub(crate) fn add_link(&mut self, id: LinkId, link: LinkEntry) {
        self.links.insert(id, link);
        self.authorize(id);
    }

    /// Removes a link.
    pub(crate) fn remove_link(&mut self, id: LinkId) {
        self.links.remove(&id);
    }

    /// Adds a publication and re-authorizes every link for it.
    pub(crate) fn add_publication(&mut self, id: u64, publication: PubEntry) {
        self.publications.insert(id, publication);
        self.authorize_all();
    }

    /// Removes a publication, and re-authorizes every link without it.
    ///
    /// The path leaves the route table when the returned entry drops.
    pub(crate) fn remove_publication(&mut self, id: u64) -> Option<PubEntry> {
        let entry = self.publications.remove(&id)?;
        self.authorize_all();
        Some(entry)
    }

    /// Sets the audience of publication `id`, and re-authorizes every link.
    pub(crate) fn set_audience(&mut self, id: u64, audience: AudienceKind) {
        let Some(entry) = self.publications.get_mut(&id) else {
            return;
        };
        if entry.audience == audience {
            return;
        }
        debug!(path = %entry.path, ?audience, "audience changed");
        entry.audience = audience;
        self.authorize_all();
    }

    /// Returns the next epoch.
    pub(crate) fn next_epoch(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    /// Records that a session with `peer` starts its authorization, and returns what it starts from.
    ///
    /// The session counts as pending on the peer until [`register`] or
    /// [`abandon`].
    ///
    /// [`register`]: Self::register
    /// [`abandon`]: Self::abandon
    pub(crate) fn begin(&mut self, peer: EndpointId) -> Snapshot {
        let epoch = self.next_epoch();
        let entry = self.peers.entry(peer).or_default();
        entry.pending += 1;
        match &entry.authorization {
            Some(stored) => Snapshot {
                stored: Some(stored.clone()),
                epoch: entry.decided,
            },
            None => Snapshot {
                stored: None,
                epoch,
            },
        }
    }

    /// Records that a pending session with `peer` failed before it registered.
    pub(crate) fn abandon(&mut self, peer: &EndpointId) {
        if let Some(entry) = self.peers.get_mut(peer) {
            entry.pending = entry.pending.saturating_sub(1);
            self.prune(peer);
        }
    }

    /// Registers a pending session with `peer`, whose authorization was decided at `decided`.
    ///
    /// A pre-registration authorization never replaces a newer one, and
    /// never undoes a trigger or a rejection that came after its decision
    /// started: then the peer holds nothing until the node decides again.
    pub(crate) fn register(
        &mut self,
        peer: EndpointId,
        authorization: Authorization,
        decided: u64,
    ) -> Registered {
        let marked_all = self.marked_all;
        let entry = self.peers.entry(peer).or_default();
        entry.pending = entry.pending.saturating_sub(1);
        entry.live = true;
        let registered = if entry.authorization.is_some() && entry.decided >= decided {
            Registered::Kept
        } else if entry.marked.max(marked_all) > decided {
            // An older authorization stays until the next decision replaces it.
            Registered::Stale
        } else {
            entry.set(authorization, decided);
            Registered::Applied
        };
        self.authorize_peer(&peer);
        registered
    }

    /// Applies a decision about `peer` that started at `started`, unless a newer one applied.
    ///
    /// `None` is a rejection: the peer holds nothing from now on, and a
    /// pending session decided before it does not undo it. Returns whether
    /// the decision applied.
    pub(crate) fn decided(
        &mut self,
        peer: &EndpointId,
        started: u64,
        authorization: Option<Authorization>,
    ) -> bool {
        let Some(entry) = self.peers.get_mut(peer) else {
            return false;
        };
        if started <= entry.decided {
            return false;
        }
        match authorization {
            Some(authorization) => entry.set(authorization, started),
            None => {
                entry.authorization = None;
                entry.expires = None;
                entry.lapsed = false;
                entry.decided = started;
                entry.marked = entry.marked.max(started);
            }
        }
        self.authorize_peer(peer);
        true
    }

    /// Records a trigger for `peer`, which makes every earlier decision stale.
    ///
    /// Returns whether the peer has a live or pending session.
    pub(crate) fn mark(&mut self, peer: &EndpointId) -> bool {
        let epoch = self.next_epoch();
        match self.peers.get_mut(peer) {
            Some(entry) => {
                entry.marked = epoch;
                true
            }
            None => false,
        }
    }

    /// Records a trigger for every peer.
    pub(crate) fn mark_all(&mut self) {
        self.marked_all = self.next_epoch();
    }

    /// Takes the rights of `peer`, whose authorization expired, until a new decision applies.
    pub(crate) fn lapse(&mut self, peer: &EndpointId) {
        if let Some(entry) = self.peers.get_mut(peer)
            && entry.authorization.is_some()
            && !entry.lapsed
        {
            entry.lapsed = true;
            self.authorize_peer(peer);
        }
    }

    /// Records that the last registered session with `peer` ended.
    ///
    /// The peer's authorization goes with it, and its links narrow to nothing.
    pub(crate) fn peer_left(&mut self, peer: &EndpointId) {
        if let Some(entry) = self.peers.get_mut(peer) {
            entry.live = false;
            entry.authorization = None;
            entry.expires = None;
            entry.lapsed = false;
            self.prune(peer);
            self.authorize_peer(peer);
        }
    }

    /// Forgets `peer` once it has neither a registered nor a pending session.
    fn prune(&mut self, peer: &EndpointId) {
        if self
            .peers
            .get(peer)
            .is_some_and(|entry| !entry.live && entry.pending == 0)
        {
            self.peers.remove(peer);
        }
    }

    /// Sets the authorization of `peer` as if a session had registered with it.
    #[cfg(test)]
    pub(crate) fn admit_peer(&mut self, peer: EndpointId, authorization: Authorization) {
        let epoch = self.next_epoch();
        let entry = self.peers.entry(peer).or_default();
        entry.live = true;
        entry.set(authorization, epoch);
        self.authorize_peer(&peer);
    }

    /// Returns the authorization of `peer`, if it holds one.
    pub(crate) fn peer_authorization(&self, peer: &EndpointId) -> Option<&Authorization> {
        self.peers.get(peer)?.authorization.as_ref()
    }

    /// Returns the base grant of `peer`, if it holds an authorization.
    ///
    /// A lapsed authorization grants nothing.
    pub(crate) fn peer_grant(&self, peer: &EndpointId) -> Option<Grant> {
        self.peers.get(peer)?.grant()
    }

    /// Returns what `peer` may do on its direct sessions: its base grant and its ticket paths.
    pub(crate) fn peer_effective_grant(&self, peer: &EndpointId) -> Option<Grant> {
        let base = self.peer_grant(peer)?;
        Some(self.effective_grant(Some(peer), &base))
    }

    /// Forgets every peer, as the node shuts down.
    pub(crate) fn clear_peers(&mut self) {
        self.peers.clear();
    }

    /// Adds one claim on `path` for `peer`, and widens the peer's links to it.
    pub(crate) fn claim_path(&mut self, peer: EndpointId, path: &Path) -> Result<(), ClaimRefused> {
        if self.closed {
            return Err(ClaimRefused::Closed);
        }
        let paths = self.tickets.entry(peer).or_default();
        if let Some(count) = paths.get_mut(path) {
            *count += 1;
            return Ok(());
        }
        let bytes: usize = paths.keys().map(|path| path.as_str().len()).sum();
        if paths.len() >= MAX_TICKET_PATHS || bytes + path.as_str().len() > MAX_TICKET_BYTES {
            return Err(ClaimRefused::TooMany);
        }
        paths.insert(path.clone(), 1);
        self.authorize_peer(&peer);
        Ok(())
    }

    /// Drops one claim on `path` for `peer`, and narrows the peer's links if it was the last.
    pub(crate) fn release_path(&mut self, peer: &EndpointId, path: &Path) {
        let Some(paths) = self.tickets.get_mut(peer) else {
            return;
        };
        let Some(count) = paths.get_mut(path) else {
            return;
        };
        *count -= 1;
        if *count > 0 {
            return;
        }
        paths.remove(path);
        if paths.is_empty() {
            self.tickets.remove(peer);
        }
        self.authorize_peer(peer);
    }

    /// Returns `base` widened by the ticket paths of `remote` it does not cover yet.
    ///
    /// A claimed path that `base` already lets the peer publish adds nothing,
    /// but stays recorded, so it takes effect if the base narrows.
    pub(crate) fn effective_grant(&self, remote: Option<&EndpointId>, base: &Grant) -> Grant {
        let Some(paths) = remote.and_then(|remote| self.tickets.get(remote)) else {
            return base.clone();
        };
        paths
            .keys()
            .filter(|path| !base.allows_publish(path.as_str()))
            .fold(base.clone(), |grant, path| {
                grant
                    .with_publish_path(path.as_str())
                    .expect("a claimed path passed check_path, so a literal pattern names it")
            })
    }

    /// Re-authorizes every link with `peer`.
    fn authorize_peer(&self, peer: &EndpointId) {
        for (id, link) in &self.links {
            if link.remote.as_ref() == Some(peer) {
                self.authorize(*id);
            }
        }
    }

    /// Returns the publication at `path`, if any.
    pub(crate) fn publication_at(&self, path: &Path) -> Option<u64> {
        self.publications
            .iter()
            .find(|(_, publication)| publication.path == *path)
            .map(|(id, _)| *id)
    }

    /// Re-authorizes every link for what it may see now.
    ///
    /// Costs links times publications under the node lock, on every change of
    /// a publication or an audience. That is fine at today's sizes, tens of
    /// each. A node with thousands of either needs an index by path.
    fn authorize_all(&self) {
        for id in self.links.keys() {
            self.authorize(*id);
        }
    }

    /// Re-authorizes link `id` for what it may see now.
    ///
    /// moq skips a grant equal to the last one, so a change that leaves the
    /// link's view alone costs its session nothing.
    fn authorize(&self, id: LinkId) {
        let Some(link) = self.links.get(&id) else {
            return;
        };
        let grant = self.link_grant(link);
        link.auth.authorize(&grant);
    }

    /// Returns the moq grant of `link`, from its own grant or its peer's authorization.
    ///
    /// A direct link whose peer has no authorization may do nothing: after a
    /// rejection, while the peer's sessions close, and until a stale
    /// registration is decided again. So may one whose authorization lapsed.
    /// The grant carries the authorization's expiry.
    fn link_grant(&self, link: &LinkEntry) -> auth::Grant {
        let peer = match (&link.grant, &link.remote) {
            (None, Some(peer)) => self.peers.get(peer),
            _ => None,
        };
        let base = match (&link.grant, peer) {
            (Some(grant), _) => grant.clone(),
            (None, Some(entry)) => entry.grant().unwrap_or_else(Grant::nothing),
            (None, None) => Grant::nothing(),
        };
        let mut grant = self.grant_for(link.kind, link.remote.as_ref(), &base);
        grant.expires = peer.and_then(|entry| entry.expires);
        grant
    }

    /// Returns the moq grant of a link, from the publications it may see.
    ///
    /// Its subscribe half lists the exact paths of the publications whose
    /// audience admits the link, within `base`. Its publish half is `base`'s
    /// for a session, and anything for a relay, which is trusted with what it
    /// forwards.
    pub(crate) fn grant_for(
        &self,
        kind: LinkKind,
        remote: Option<&EndpointId>,
        base: &Grant,
    ) -> auth::Grant {
        let role = match kind {
            LinkKind::Direct => LinkRole::Direct,
            LinkKind::Relay => LinkRole::Relay,
        };
        let base = &self.effective_grant(remote, base);
        let subscribe = moq_ext_access::visible_paths(
            self.publications
                .values()
                .map(|publication| (publication.path.as_str(), &publication.audience)),
            base,
            role,
            remote,
        );
        let publish = match kind {
            LinkKind::Direct => base.publish.clone(),
            LinkKind::Relay => Pattern::all().into(),
        };
        auth::Grant {
            publish,
            subscribe,
            expires: None,
        }
    }

    /// Returns the link whose peer announced a route from `source`.
    ///
    /// A route is attributed to the hop its session's peer declares, or else
    /// to the hop this node assigned the session. `None` for a route
    /// published on this node, and for an unknown hop while no relay, or more
    /// than one, feeds the table.
    pub(crate) fn link_for(&self, source: Source) -> Option<LinkId> {
        let Source::Peer(_) = source else {
            return None;
        };
        if let Some(id) = Hop::of_route(source).and_then(|hop| self.link_with_hop(hop)) {
            return Some(id);
        }
        // moq-relay declares a hop of its own, not the one derived from its
        // endpoint id, and iroh-live-node peers always carry the derived one.
        // So an unknown hop is a relay's, but not one we can name if several
        // relays feed the table.
        let mut relays = self
            .links
            .iter()
            .filter(|(_, link)| link.kind == LinkKind::Relay && link.consume)
            .map(|(id, _)| *id);
        match (relays.next(), relays.next()) {
            (Some(id), None) => Some(id),
            _ => None,
        }
    }

    /// Returns the link whose peer carries `hop`, if any.
    pub(crate) fn link_with_hop(&self, hop: Hop) -> Option<LinkId> {
        self.links
            .iter()
            .find(|(_, link)| link.hop == hop)
            .map(|(id, _)| *id)
    }

    /// Reports whether a relay link feeds the route table.
    pub(crate) fn has_relays(&self) -> bool {
        self.links
            .values()
            .any(|link| link.kind == LinkKind::Relay && link.consume)
    }
}

/// Returns what every link of the node serves its peer: the table's local view.
///
/// It hides every route the node learned from a peer or a relay, so the node
/// never carries traffic between two of its links.
pub(crate) fn served(table: &origin::Producer) -> origin::Consumer {
    table.consume().local()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use iroh::SecretKey;
    use moq_net::origin::RouteOptions;

    use super::*;

    /// A route table, with the task that drives it.
    struct Table {
        producer: origin::Producer,
        _driver: AbortOnDropHandle<()>,
    }

    impl Table {
        /// Creates a route table and drives it on the current runtime.
        fn new() -> Self {
            let hop = Hop::of(&SecretKey::generate().public());
            let (producer, driver) = origin::Producer::new(origin::Config::new(hop.id()));
            let driver = tokio::spawn(async move {
                moq_net::time::run(driver).await;
            });
            Self {
                producer,
                _driver: AbortOnDropHandle::new(driver),
            }
        }

        /// Returns a publication at `path` for `audience`, as `publish` records it.
        fn publication(&self, path: &str, audience: AudienceKind) -> PubEntry {
            let path = Path::new(path).expect("a valid path");
            let publication = self
                .producer
                .route(path.clone(), RouteOptions::hidden())
                .expect("a route");
            PubEntry {
                path,
                publication,
                audience,
                peers_task: None,
                audience_generation: 0,
                _closed_task: AbortOnDropHandle::new(tokio::spawn(async {})),
                withdrawn: CancellationToken::new(),
            }
        }
    }

    /// Returns the moq grant `state` gives link `id` now.
    fn grant_of(state: &State, id: LinkId) -> auth::Grant {
        state.link_grant(&state.links[&id])
    }

    /// Adds `link` as link `id`, admitting a direct link's peer with every right first.
    fn add(state: &mut State, id: LinkId, link: LinkEntry) {
        if link.kind == LinkKind::Direct {
            let peer = link.remote.expect("a direct link has a peer");
            if state.peer_grant(&peer).is_none() {
                state.admit_peer(peer, Grant::everything().into());
            }
        }
        state.add_link(id, link);
    }

    /// Returns a grant that lets a peer subscribe anywhere and publish under `live/<peer>/`.
    fn own_paths(peer: EndpointId) -> Grant {
        let own: Pattern = format!("live/{peer}/**").parse().expect("a pattern");
        Grant::publish_under(own)
    }

    /// Returns a link entry of `kind` with a peer of its own, which feeds the table.
    fn link(kind: LinkKind) -> LinkEntry {
        let remote = SecretKey::generate().public();
        LinkEntry {
            kind,
            remote: Some(remote),
            hop: Hop::of(&remote),
            grant: (kind == LinkKind::Relay).then(Grant::everything),
            consume: true,
            auth: auth::Handle::new(),
            session: None,
            link_state: LinkState::default(),
        }
    }

    /// Returns a route source with a hop no link carries.
    fn unknown_hop() -> Source {
        Source::Peer(Hop::of(&SecretKey::generate().public()).id().get())
    }

    #[test]
    fn link_for_maps_an_unknown_hop_to_the_only_relay() {
        let mut state = State::default();
        assert_eq!(state.link_for(unknown_hop()), None);

        add(&mut state, LinkId(1), link(LinkKind::Direct));
        assert_eq!(state.link_for(unknown_hop()), None);

        add(&mut state, LinkId(2), link(LinkKind::Relay));
        assert_eq!(state.link_for(unknown_hop()), Some(LinkId(2)));

        add(&mut state, LinkId(3), link(LinkKind::Relay));
        assert_eq!(state.link_for(unknown_hop()), None);
    }

    #[test]
    fn link_for_ignores_a_relay_that_feeds_nothing() {
        let mut state = State::default();
        add(&mut state, LinkId(1), link(LinkKind::Relay));
        add(
            &mut state,
            LinkId(2),
            LinkEntry {
                consume: false,
                ..link(LinkKind::Relay)
            },
        );
        assert_eq!(state.link_for(unknown_hop()), Some(LinkId(1)));
    }

    #[test]
    fn link_for_finds_a_known_hop_and_skips_local_routes() {
        let mut state = State::default();
        let direct = link(LinkKind::Direct);
        let hop = direct.hop;
        add(&mut state, LinkId(1), direct);
        add(&mut state, LinkId(2), link(LinkKind::Relay));
        add(&mut state, LinkId(3), link(LinkKind::Relay));
        assert_eq!(
            state.link_for(Source::Peer(hop.id().get())),
            Some(LinkId(1))
        );
        assert_eq!(state.link_for(Source::Local), None);
    }

    #[tokio::test]
    async fn a_ticket_claim_widens_the_policy_grant_to_its_path() {
        let mut state = State::default();
        let direct = link(LinkKind::Direct);
        let peer = direct.remote.expect("a peer");
        state.admit_peer(peer, own_paths(peer).into());
        add(&mut state, LinkId(1), direct);
        let own = format!("live/{peer}/cam");
        let claimed = Path::new("shows/news").expect("a valid path");
        assert!(grant_of(&state, LinkId(1)).publish.matches(&own));
        assert!(
            !grant_of(&state, LinkId(1))
                .publish
                .matches(claimed.as_str())
        );

        state.claim_path(peer, &claimed).expect("claim");
        state.claim_path(peer, &claimed).expect("a second claim");
        let widened = grant_of(&state, LinkId(1));
        assert!(widened.publish.matches(claimed.as_str()));
        assert!(widened.publish.matches(&own), "the base grant stays");
        assert!(
            !widened.publish.matches("shows/news/extra"),
            "a claim covers exactly its path"
        );

        // The first release leaves the other claim in place.
        state.release_path(&peer, &claimed);
        assert!(
            grant_of(&state, LinkId(1))
                .publish
                .matches(claimed.as_str())
        );
        state.release_path(&peer, &claimed);
        assert!(
            !grant_of(&state, LinkId(1))
                .publish
                .matches(claimed.as_str())
        );

        // Another peer's claim does not reach this link.
        state
            .claim_path(SecretKey::generate().public(), &claimed)
            .expect("claim");
        assert!(
            !grant_of(&state, LinkId(1))
                .publish
                .matches(claimed.as_str())
        );
    }

    #[tokio::test]
    async fn an_audience_change_moves_a_publication_between_links() {
        let table = Table::new();
        let mut state = State::default();
        let alice = link(LinkKind::Direct);
        let bob = link(LinkKind::Direct);
        let alice_id = alice.remote.expect("a peer");
        add(&mut state, LinkId(1), alice);
        add(&mut state, LinkId(2), bob);

        let path = "live/me/cam";
        state.add_publication(
            10,
            table.publication(path, AudienceKind::Peers(BTreeSet::from([alice_id]))),
        );
        assert!(grant_of(&state, LinkId(1)).subscribe.matches(path));
        assert!(!grant_of(&state, LinkId(2)).subscribe.matches(path));

        state.set_audience(10, AudienceKind::Peers(BTreeSet::new()));
        assert!(!grant_of(&state, LinkId(1)).subscribe.matches(path));

        state.set_audience(10, AudienceKind::Granted);
        assert!(grant_of(&state, LinkId(1)).subscribe.matches(path));
        assert!(grant_of(&state, LinkId(2)).subscribe.matches(path));
    }

    #[tokio::test]
    async fn a_relay_link_sees_public_publications_only() {
        let table = Table::new();
        let mut state = State::default();
        let relay = link(LinkKind::Relay);
        let relay_id = relay.remote.expect("a peer");
        add(&mut state, LinkId(1), relay);

        // Even a `Peers` audience that names the relay stays off it.
        state.add_publication(
            10,
            table.publication(
                "live/me/private",
                AudienceKind::Peers(BTreeSet::from([relay_id])),
            ),
        );
        state.add_publication(
            11,
            table.publication("live/me/public", AudienceKind::Granted),
        );
        let grant = grant_of(&state, LinkId(1));
        assert!(!grant.subscribe.matches("live/me/private"));
        assert!(grant.subscribe.matches("live/me/public"));
        assert!(
            grant.publish.matches("anything/at/all"),
            "a relay is trusted with what it forwards"
        );
    }

    #[tokio::test]
    async fn a_removed_publication_leaves_every_grant() {
        let table = Table::new();
        let mut state = State::default();
        add(&mut state, LinkId(1), link(LinkKind::Direct));
        add(&mut state, LinkId(2), link(LinkKind::Relay));
        state.add_publication(10, table.publication("live/me/cam", AudienceKind::Granted));
        state.add_publication(11, table.publication("live/me/mic", AudienceKind::Granted));
        assert!(grant_of(&state, LinkId(1)).subscribe.matches("live/me/cam"));

        let removed = state.remove_publication(10).expect("the publication");
        assert_eq!(removed.path.as_str(), "live/me/cam");
        for id in [LinkId(1), LinkId(2)] {
            let grant = grant_of(&state, id);
            assert!(!grant.subscribe.matches("live/me/cam"), "{id:?}");
            assert!(grant.subscribe.matches("live/me/mic"), "{id:?}");
        }
        assert!(state.remove_publication(10).is_none());
    }

    #[tokio::test]
    async fn the_policy_grant_bounds_what_a_link_sees() {
        let table = Table::new();
        let mut state = State::default();
        let direct = link(LinkKind::Direct);
        let peer = direct.remote.expect("a peer");
        state.admit_peer(peer, Grant::nothing().into());
        add(&mut state, LinkId(1), direct);
        state.add_publication(10, table.publication("live/me/cam", AudienceKind::Granted));
        assert!(grant_of(&state, LinkId(1)).subscribe.is_empty());

        state.admit_peer(peer, Grant::subscribe_only().into());
        assert!(grant_of(&state, LinkId(1)).subscribe.matches("live/me/cam"));
    }

    #[tokio::test]
    async fn the_links_of_one_peer_share_its_policy_grant() {
        let table = Table::new();
        let mut state = State::default();
        let first = link(LinkKind::Direct);
        let peer = first.remote.expect("a peer");
        let second = LinkEntry {
            remote: Some(peer),
            hop: first.hop,
            ..link(LinkKind::Direct)
        };
        state.admit_peer(peer, Grant::nothing().into());
        add(&mut state, LinkId(1), first);
        add(&mut state, LinkId(2), second);
        state.add_publication(10, table.publication("live/me/cam", AudienceKind::Granted));
        for id in [LinkId(1), LinkId(2)] {
            assert!(grant_of(&state, id).subscribe.is_empty(), "{id:?}");
        }

        state.admit_peer(peer, Grant::subscribe_only().into());
        for id in [LinkId(1), LinkId(2)] {
            assert!(
                grant_of(&state, id).subscribe.matches("live/me/cam"),
                "{id:?}"
            );
        }

        // A later session's grant holds for the earlier one too.
        state.admit_peer(peer, Grant::nothing().into());
        assert!(grant_of(&state, LinkId(1)).subscribe.is_empty());

        // Once the peer is gone, its links see nothing.
        state.peer_left(&peer);
        assert_eq!(state.peer_grant(&peer), None);
        assert!(grant_of(&state, LinkId(1)).subscribe.is_empty());
    }

    /// Returns a peer and a state where its first session registered with `grant`.
    fn registered(grant: Grant) -> (State, EndpointId) {
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        let snapshot = state.begin(peer);
        assert!(snapshot.stored.is_none());
        assert_eq!(
            state.register(peer, grant.into(), snapshot.epoch),
            Registered::Applied
        );
        (state, peer)
    }

    #[test]
    fn a_session_snapshot_never_replaces_a_newer_decision() {
        let (mut state, peer) = registered(Grant::everything());
        // A second session starts from the stored authorization.
        let snapshot = state.begin(peer);
        assert_eq!(
            snapshot.stored.as_ref().map(|stored| &stored.grant),
            Some(&Grant::everything())
        );
        // A re-decide narrows the peer before that session registers.
        let started = state.next_epoch();
        assert!(state.decided(&peer, started, Some(Grant::subscribe_only().into())));
        let stored = snapshot.stored.expect("a stored authorization");
        assert_eq!(
            state.register(peer, stored, snapshot.epoch),
            Registered::Kept
        );
        assert_eq!(state.peer_grant(&peer), Some(Grant::subscribe_only()));
    }

    #[test]
    fn a_pending_session_does_not_undo_a_rejection() {
        let (mut state, peer) = registered(Grant::everything());
        let snapshot = state.begin(peer);
        let started = state.next_epoch();
        assert!(state.decided(&peer, started, None), "the rejection applies");
        assert_eq!(state.peer_grant(&peer), None);
        let stored = snapshot.stored.expect("a stored authorization");
        assert_eq!(
            state.register(peer, stored, snapshot.epoch),
            Registered::Stale
        );
        assert_eq!(state.peer_grant(&peer), None, "nothing until decided again");
    }

    #[test]
    fn a_trigger_during_a_first_decision_makes_it_stale() {
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        let snapshot = state.begin(peer);
        // The application reauthorizes the peer while its dial is decided.
        assert!(state.mark(&peer), "a pending peer is reachable");
        assert_eq!(
            state.register(peer, Grant::everything().into(), snapshot.epoch),
            Registered::Stale
        );
        assert_eq!(state.peer_grant(&peer), None);

        // So does a trigger for every peer.
        let other = SecretKey::generate().public();
        let snapshot = state.begin(other);
        state.mark_all();
        assert_eq!(
            state.register(other, Grant::everything().into(), snapshot.epoch),
            Registered::Stale
        );
    }

    #[test]
    fn a_decision_applies_however_many_triggers_came_after_it_started() {
        let (mut state, peer) = registered(Grant::everything());
        for round in 0..5 {
            let started = state.next_epoch();
            assert!(state.mark(&peer));
            state.mark_all();
            assert!(
                state.decided(&peer, started, Some(Grant::subscribe_only().into())),
                "round {round}"
            );
        }
        // An older decision that finishes late does not.
        let older = state.next_epoch();
        let newer = state.next_epoch();
        assert!(state.decided(&peer, newer, Some(Grant::nothing().into())));
        assert!(!state.decided(&peer, older, Some(Grant::everything().into())));
        assert_eq!(state.peer_grant(&peer), Some(Grant::nothing()));
    }

    #[test]
    fn a_lapsed_authorization_grants_nothing_until_a_decision() {
        let (mut state, peer) = registered(Grant::everything());
        state.lapse(&peer);
        assert_eq!(state.peer_grant(&peer), Some(Grant::nothing()));
        assert!(
            state.peer_authorization(&peer).is_some(),
            "the authorization stays for the policy to see"
        );
        let started = state.next_epoch();
        assert!(state.decided(&peer, started, Some(Grant::subscribe_only().into())));
        assert_eq!(state.peer_grant(&peer), Some(Grant::subscribe_only()));
    }

    #[test]
    fn a_peer_is_forgotten_once_no_session_is_live_or_pending() {
        let (mut state, peer) = registered(Grant::everything());
        state.begin(peer);
        state.peer_left(&peer);
        assert!(state.mark(&peer), "the pending session keeps the peer");
        state.abandon(&peer);
        assert!(!state.mark(&peer));
    }
}
