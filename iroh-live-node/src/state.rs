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
use moq_ext_access::Viewer;
use moq_net::{Path, Pattern, auth, origin, origin::Source};
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    Grant, LinkId, LinkKind, handshake::Hop, link::LatestSample, policy::Authorization,
    publish::AudienceKind,
};

/// The node's publications and links.
#[derive(Debug, Default)]
pub(crate) struct State {
    next_id: u64,
    pub(crate) publications: BTreeMap<u64, PublicationEntry>,
    pub(crate) links: BTreeMap<LinkId, LinkEntry>,
    /// Set once the node shuts down. Nothing is added after that.
    pub(crate) closed: bool,
    /// The paths claims widened each peer's grant to, with how many claims hold each.
    claims: HashMap<EndpointId, BTreeMap<Path, usize>>,
    /// Counts the calls to `Node::reauthorize` and `Node::reauthorize_all`.
    ///
    /// A session whose first decision started before the count moved may
    /// carry rights from before the call, so it registers with nothing and
    /// is decided again.
    pub(crate) reauthorized: u64,
}

/// A direct session's authorization, as the node applies it.
#[derive(Debug, Clone)]
pub(crate) struct Authorized {
    pub(crate) authorization: Authorization,
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
}

impl Authorized {
    /// Returns `authorization` as the node applies it from now on.
    pub(crate) fn new(authorization: Authorization) -> Self {
        let left = authorization
            .expires
            .map(|expires| expires.duration_since(SystemTime::now()));
        Self {
            lapsed: matches!(left, Some(Err(_))),
            expires: left.and_then(|left| {
                moq_net::time::Instant::now().checked_add(left.unwrap_or_default())
            }),
            authorization,
        }
    }

    /// Returns the grant the session takes now: nothing once the authorization lapsed.
    fn grant(&self) -> Grant {
        match self.lapsed {
            true => Grant::nothing(),
            false => self.authorization.grant.clone(),
        }
    }
}

/// How many claimed paths may widen one peer's grant.
const MAX_CLAIMED_PATHS: usize = 64;

/// How many bytes of claimed paths may widen one peer's grant.
const MAX_CLAIMED_BYTES: usize = 16 * 1024;

/// Why a ticket path could not be claimed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClaimRefused {
    /// The node has shut down.
    Closed,
    /// The peer holds too many claimed paths.
    TooMany,
}

/// One publication.
#[derive(derive_more::Debug)]
pub(crate) struct PublicationEntry {
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

impl Drop for PublicationEntry {
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
    /// What the link is, with what only that kind of link has.
    pub(crate) role: LinkRole,
    /// The identity the link's routes are attributed to in the route table.
    pub(crate) hop: Hop,
    /// The link's sessions' authorization, which moq enforces on them.
    #[debug(skip)]
    pub(crate) auth: auth::Handle,
    /// The latest reading of the link's connection monitor.
    #[debug(skip)]
    pub(crate) latest_sample: LatestSample,
}

/// What a link is: a session with a peer, or a relay.
#[derive(Debug)]
pub(crate) enum LinkRole {
    /// A direct session with `peer`, which takes its rights from the policy.
    Direct {
        peer: EndpointId,
        /// What the policy decided for the session.
        ///
        /// `None` for a session that may do nothing: after a rejection, while
        /// it closes, and until a stale first decision is decided again.
        authorized: Option<Authorized>,
    },
    /// A relay, trusted with every path it forwards.
    Relay {
        /// Whether the relay's routes enter the route table.
        consume: bool,
    },
}

impl LinkEntry {
    /// Returns the kind of link this is.
    pub(crate) fn kind(&self) -> LinkKind {
        match self.role {
            LinkRole::Direct { .. } => LinkKind::Direct,
            LinkRole::Relay { .. } => LinkKind::Relay,
        }
    }

    /// Returns the peer of a direct link, or `None` for a relay.
    fn peer(&self) -> Option<&EndpointId> {
        match &self.role {
            LinkRole::Direct { peer, .. } => Some(peer),
            LinkRole::Relay { .. } => None,
        }
    }

    /// Returns what the policy decided for a direct link's session.
    fn authorized(&self) -> Option<&Authorized> {
        match &self.role {
            LinkRole::Direct { authorized, .. } => authorized.as_ref(),
            LinkRole::Relay { .. } => None,
        }
    }

    /// Reports whether this is a relay whose routes enter the route table.
    fn is_consuming_relay(&self) -> bool {
        matches!(self.role, LinkRole::Relay { consume: true })
    }
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
    pub(crate) fn add_publication(&mut self, id: u64, publication: PublicationEntry) {
        self.publications.insert(id, publication);
        self.authorize_all();
    }

    /// Removes a publication, and re-authorizes every link without it.
    ///
    /// The path leaves the route table when the returned entry drops.
    pub(crate) fn remove_publication(&mut self, id: u64) -> Option<PublicationEntry> {
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

    /// Applies a new decision about the session on link `id`.
    ///
    /// `None` is a rejection: the session may do nothing from now on.
    pub(crate) fn decided(&mut self, id: LinkId, authorization: Option<Authorization>) {
        if let Some(LinkEntry {
            role: LinkRole::Direct { authorized, .. },
            ..
        }) = self.links.get_mut(&id)
        {
            *authorized = authorization.map(Authorized::new);
            self.authorize(id);
        }
    }

    /// Takes the rights of the session on link `id`, whose authorization expired, until a new decision applies.
    pub(crate) fn lapse(&mut self, id: LinkId) {
        if let Some(LinkEntry {
            role:
                LinkRole::Direct {
                    authorized: Some(authorized),
                    ..
                },
            ..
        }) = self.links.get_mut(&id)
            && !authorized.lapsed
        {
            authorized.lapsed = true;
            self.authorize(id);
        }
    }

    /// Returns the authorization of the newest session with `peer` that holds one.
    pub(crate) fn newest_authorization(&self, peer: &EndpointId) -> Option<&Authorization> {
        self.links
            .values()
            .rev()
            .filter(|link| link.peer() == Some(peer))
            .find_map(LinkEntry::authorized)
            .map(|authorized| &authorized.authorization)
    }

    /// Returns what the session on link `id` may do: its authorization's grant and its peer's claimed paths.
    ///
    /// A lapsed or missing authorization grants nothing.
    pub(crate) fn session_grant(&self, id: LinkId) -> Grant {
        let Some(link) = self.links.get(&id) else {
            return Grant::nothing();
        };
        let base = link
            .authorized()
            .map_or_else(Grant::nothing, Authorized::grant);
        self.effective_grant(link.peer(), &base)
    }

    /// Adds one claim on `path` for `peer`, and widens the peer's links to it.
    pub(crate) fn claim_path(&mut self, peer: EndpointId, path: &Path) -> Result<(), ClaimRefused> {
        if self.closed {
            return Err(ClaimRefused::Closed);
        }
        let paths = self.claims.entry(peer).or_default();
        if let Some(count) = paths.get_mut(path) {
            *count += 1;
            return Ok(());
        }
        let bytes: usize = paths.keys().map(|path| path.as_str().len()).sum();
        if paths.len() >= MAX_CLAIMED_PATHS || bytes + path.as_str().len() > MAX_CLAIMED_BYTES {
            return Err(ClaimRefused::TooMany);
        }
        paths.insert(path.clone(), 1);
        self.authorize_peer(&peer);
        Ok(())
    }

    /// Drops one claim on `path` for `peer`, and narrows the peer's links if it was the last.
    pub(crate) fn release_path(&mut self, peer: &EndpointId, path: &Path) {
        let Some(paths) = self.claims.get_mut(peer) else {
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
            self.claims.remove(peer);
        }
        self.authorize_peer(peer);
    }

    /// Returns `base` widened by the claimed paths of `peer` it does not cover yet.
    ///
    /// A claimed path that `base` already lets the peer publish adds nothing,
    /// but stays recorded, so it takes effect if the base narrows.
    pub(crate) fn effective_grant(&self, peer: Option<&EndpointId>, base: &Grant) -> Grant {
        let Some(paths) = peer.and_then(|peer| self.claims.get(peer)) else {
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
            if link.peer() == Some(peer) {
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

    /// Returns the moq grant of `link`, from a session's authorization.
    ///
    /// A relay may do everything before audiences. A session without an
    /// authorization, or with a lapsed one, may do nothing. The grant carries
    /// the authorization's expiry.
    fn link_grant(&self, link: &LinkEntry) -> auth::Grant {
        match &link.role {
            LinkRole::Relay { .. } => self.link_view(Viewer::Relay, &Grant::everything()),
            LinkRole::Direct { peer, authorized } => {
                let base = authorized
                    .as_ref()
                    .map_or_else(Grant::nothing, Authorized::grant);
                let mut grant = self.link_view(Viewer::Direct(peer), &base);
                grant.expires = authorized
                    .as_ref()
                    .and_then(|authorized| authorized.expires);
                grant
            }
        }
    }

    /// Returns the moq grant of a link, from the publications it may see.
    ///
    /// Its subscribe half lists the exact paths of the publications whose
    /// audience admits the link, within `base`. Its publish half is `base`'s
    /// for a session, and anything for a relay, which is trusted with what it
    /// forwards.
    pub(crate) fn link_view(&self, viewer: Viewer<'_, EndpointId>, base: &Grant) -> auth::Grant {
        let peer = match viewer {
            Viewer::Direct(peer) => Some(peer),
            Viewer::Relay => None,
        };
        let base = &self.effective_grant(peer, base);
        let publish = match viewer {
            Viewer::Direct(_) => base.publish.clone(),
            Viewer::Relay => Pattern::all().into(),
        };
        let subscribe = moq_ext_access::visible_paths(
            self.publications
                .values()
                .map(|publication| (publication.path.as_str(), &publication.audience)),
            base,
            viewer,
        );
        let mut grant = auth::Grant::default();
        grant.publish = publish;
        grant.subscribe = subscribe;
        grant
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
            .filter(|(_, link)| link.is_consuming_relay())
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
        self.links.values().any(LinkEntry::is_consuming_relay)
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
        fn publication(&self, path: &str, audience: AudienceKind) -> PublicationEntry {
            let path = Path::new(path).expect("a valid path");
            let publication = self
                .producer
                .route(path.clone(), RouteOptions::hidden())
                .expect("a route");
            PublicationEntry {
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

    /// Adds `link` as link `id`.
    fn add(state: &mut State, id: LinkId, link: LinkEntry) {
        state.add_link(id, link);
    }

    /// Returns a grant that lets a peer subscribe anywhere and publish under `live/<peer>/`.
    fn own_paths(peer: EndpointId) -> Grant {
        let own: Pattern = format!("live/{peer}/**").parse().expect("a pattern");
        Grant::publish_under(own)
    }

    /// Returns a link entry for `role`, with a hop of its own.
    fn link(role: LinkRole) -> LinkEntry {
        LinkEntry {
            role,
            hop: Hop::of(&SecretKey::generate().public()),
            auth: auth::Handle::new(),
            latest_sample: LatestSample::default(),
        }
    }

    /// Returns a direct link with a peer of its own, whose session holds every right.
    fn direct() -> LinkEntry {
        direct_with(SecretKey::generate().public(), Grant::everything())
    }

    /// Returns a direct link with `peer`, whose session holds `grant`.
    fn direct_with(peer: EndpointId, grant: Grant) -> LinkEntry {
        LinkEntry {
            hop: Hop::of(&peer),
            ..link(LinkRole::Direct {
                peer,
                authorized: Some(Authorized::new(grant.into())),
            })
        }
    }

    /// Returns a relay link whose routes enter the table.
    fn relay() -> LinkEntry {
        link(LinkRole::Relay { consume: true })
    }

    /// Returns a route source with a hop no link carries.
    fn unknown_hop() -> Source {
        Source::Peer(Hop::of(&SecretKey::generate().public()).id().get())
    }

    #[test]
    fn link_for_maps_an_unknown_hop_to_the_only_relay() {
        let mut state = State::default();
        assert_eq!(state.link_for(unknown_hop()), None);

        add(&mut state, LinkId(1), direct());
        assert_eq!(state.link_for(unknown_hop()), None);

        add(&mut state, LinkId(2), relay());
        assert_eq!(state.link_for(unknown_hop()), Some(LinkId(2)));

        add(&mut state, LinkId(3), relay());
        assert_eq!(state.link_for(unknown_hop()), None);
    }

    #[test]
    fn link_for_ignores_a_relay_that_feeds_nothing() {
        let mut state = State::default();
        add(&mut state, LinkId(1), relay());
        add(
            &mut state,
            LinkId(2),
            link(LinkRole::Relay { consume: false }),
        );
        assert_eq!(state.link_for(unknown_hop()), Some(LinkId(1)));
    }

    #[test]
    fn link_for_finds_a_known_hop_and_skips_local_routes() {
        let mut state = State::default();
        let direct = direct();
        let hop = direct.hop;
        add(&mut state, LinkId(1), direct);
        add(&mut state, LinkId(2), relay());
        add(&mut state, LinkId(3), relay());
        assert_eq!(
            state.link_for(Source::Peer(hop.id().get())),
            Some(LinkId(1))
        );
        assert_eq!(state.link_for(Source::Local), None);
    }

    #[tokio::test]
    async fn a_ticket_claim_widens_the_policy_grant_to_its_path() {
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        add(&mut state, LinkId(1), direct_with(peer, own_paths(peer)));
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
        let alice_id = SecretKey::generate().public();
        add(
            &mut state,
            LinkId(1),
            direct_with(alice_id, Grant::everything()),
        );
        add(&mut state, LinkId(2), direct());

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
        let relay_id = SecretKey::generate().public();
        add(
            &mut state,
            LinkId(1),
            LinkEntry {
                hop: Hop::of(&relay_id),
                ..relay()
            },
        );

        // Even a `Peers` audience that names the relay's endpoint id stays off it.
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
        add(&mut state, LinkId(1), direct());
        add(&mut state, LinkId(2), relay());
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
        add(&mut state, LinkId(1), direct());
        state.decided(LinkId(1), Some(Grant::nothing().into()));
        state.add_publication(10, table.publication("live/me/cam", AudienceKind::Granted));
        assert!(grant_of(&state, LinkId(1)).subscribe.is_empty());

        state.decided(LinkId(1), Some(Grant::subscribe_only().into()));
        assert!(grant_of(&state, LinkId(1)).subscribe.matches("live/me/cam"));
    }

    #[tokio::test]
    async fn each_session_of_a_peer_holds_its_own_authorization() {
        let table = Table::new();
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        add(
            &mut state,
            LinkId(1),
            direct_with(peer, Grant::everything()),
        );
        add(
            &mut state,
            LinkId(2),
            direct_with(peer, Grant::everything()),
        );
        state.add_publication(10, table.publication("live/me/cam", AudienceKind::Granted));

        state.decided(LinkId(1), Some(Grant::nothing().into()));
        assert!(grant_of(&state, LinkId(1)).subscribe.is_empty());
        assert!(grant_of(&state, LinkId(2)).subscribe.matches("live/me/cam"));
        assert_eq!(
            state.newest_authorization(&peer),
            Some(&Grant::everything().into()),
            "the newest session's"
        );

        // A rejected session holds nothing, and the older one answers for the peer.
        state.decided(LinkId(2), None);
        assert!(grant_of(&state, LinkId(2)).subscribe.is_empty());
        assert_eq!(state.session_grant(LinkId(2)), Grant::nothing());
        assert_eq!(
            state.newest_authorization(&peer),
            Some(&Grant::nothing().into())
        );
        state.remove_link(LinkId(1));
        state.remove_link(LinkId(2));
        assert_eq!(state.newest_authorization(&peer), None);
    }

    #[test]
    fn a_lapsed_authorization_grants_nothing_until_a_decision() {
        let mut state = State::default();
        add(&mut state, LinkId(1), direct());
        state.lapse(LinkId(1));
        assert_eq!(state.session_grant(LinkId(1)), Grant::nothing());
        assert!(grant_of(&state, LinkId(1)).publish.is_empty());
        assert!(
            state
                .links
                .values()
                .next()
                .is_some_and(|link| link.authorized().is_some()),
            "the authorization stays for the policy to see"
        );
        state.decided(LinkId(1), Some(Grant::subscribe_only().into()));
        assert_eq!(state.session_grant(LinkId(1)), Grant::subscribe_only());
    }

    #[test]
    fn an_expired_authorization_lapses_when_stored() {
        let expired = Authorization {
            grant: Grant::everything(),
            expires: Some(SystemTime::now() - std::time::Duration::from_secs(1)),
        };
        assert_eq!(Authorized::new(expired).grant(), Grant::nothing());
    }
}
