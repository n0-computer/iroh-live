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
//!
//! Of several sessions with one peer, only the oldest may announce into the
//! table (see [`State::is_standby`]).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Weak},
    time::SystemTime,
};

use iroh::EndpointId;
use moq_ext_access::{GroupId, Viewer};
use moq_net::{Path, Pattern, auth, origin, origin::Source};
use n0_error::e;
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    Error, Grant, LinkId, LinkKind,
    group::{GroupInner, Hints, SiteId},
    handshake::Hop,
    link::LatestSample,
    policy::Authorization,
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
    /// The node's groups, by id.
    pub(crate) groups: BTreeMap<GroupId, GroupEntry>,
}

/// One group, as the grants read it.
#[derive(derive_more::Debug)]
pub(crate) struct GroupEntry {
    pub(crate) root: Path,
    /// Tells the group's handle from a later one with the same root.
    handle: u64,
    /// The handle, so `Node::group` can hand out the group that exists.
    #[debug(skip)]
    pub(crate) group: Weak<GroupInner>,
    pub(crate) members: BTreeSet<EndpointId>,
    pub(crate) forwarding: bool,
    /// The members' valid hints, as the group's driver keeps them.
    pub(crate) hints: Hints,
    /// This node's site in the group.
    pub(crate) site: Option<SiteId>,
    /// The paths this node is the ingress for in its site.
    pub(crate) ingress: BTreeSet<Path>,
}

impl GroupEntry {
    /// Returns the entry of a new group at `root`, without members.
    pub(crate) fn new(root: Path, handle: u64, forwarding: bool, group: &Arc<GroupInner>) -> Self {
        Self {
            root,
            handle,
            group: Arc::downgrade(group),
            members: BTreeSet::new(),
            forwarding,
            hints: Hints::default(),
            site: None,
            ingress: BTreeSet::new(),
        }
    }
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

/// Which role a direct session plays for the node's groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SessionKind {
    /// A session as any peer opens it.
    Plain,
    /// A session this node dialed as a forwarder, whose routes it may serve to its site.
    Forwarding,
    /// A session the peer dialed as a forwarder, which gets the groups' publications.
    Forwarded,
}

impl SessionKind {
    /// Reports whether sessions of this kind and `other` compete for one announcing role.
    ///
    /// A forwarder session this node dialed lands its routes apart from the
    /// others, so it has a standby of its own.
    fn same_class(self, other: Self) -> bool {
        (self == Self::Forwarding) == (other == Self::Forwarding)
    }
}

/// What a link is: a session with a peer, or a relay.
#[derive(Debug)]
pub(crate) enum LinkRole {
    /// A direct session with `peer`, which takes its rights from the policy.
    Direct {
        peer: EndpointId,
        kind: SessionKind,
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

    /// Returns the peer and the session kind of a direct link, or `None` for a relay.
    fn session(&self) -> Option<(&EndpointId, SessionKind)> {
        match &self.role {
            LinkRole::Direct { peer, kind, .. } => Some((peer, *kind)),
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
    ///
    /// A forwarder session moves the group publications off the peer's other
    /// sessions, so those are authorized again too.
    pub(crate) fn add_link(&mut self, id: LinkId, link: LinkEntry) {
        let peer = link.peer().copied();
        self.links.insert(id, link);
        match peer {
            Some(peer) => self.authorize_peer(&peer),
            None => self.authorize(id),
        }
    }

    /// Removes a link.
    ///
    /// The next session with the same peer and of the same class, if any, may
    /// announce from now on.
    pub(crate) fn remove_link(&mut self, id: LinkId) {
        let Some(link) = self.links.remove(&id) else {
            return;
        };
        let Some((peer, kind)) = link.session() else {
            return;
        };
        // Removing a standby leaves the oldest session as it is.
        if let Some(next) = self.first_session_with(peer, kind)
            && next > id
        {
            debug!(
                peer = %peer.fmt_short(),
                removed = %id,
                %next,
                "the next session with the peer takes over its routes",
            );
        }
        self.authorize_peer(peer);
    }

    /// Returns the oldest session with `peer` in the class of `kind`.
    fn first_session_with(&self, peer: &EndpointId, kind: SessionKind) -> Option<LinkId> {
        self.links
            .iter()
            .find(|(_, link)| {
                link.session()
                    .is_some_and(|(other, other_kind)| other == peer && kind.same_class(other_kind))
            })
            .map(|(id, _)| *id)
    }

    /// Reports whether link `id` is a session with a peer that an older session of its class also serves.
    ///
    /// A simultaneous dial, or a dial racing an accept, leaves two sessions
    /// with one peer, and both would announce the peer's broadcasts.
    /// moq-lite-06 announcements name no instance, so each session mints its
    /// own, and when the second session's route ranks first moq takes it for
    /// the peer replacing the broadcast: track requests in flight on the first
    /// session fail as unroutable, and the broadcast its readers hold takes no
    /// new tracks. A standby therefore may announce nothing. Link ids grow, so
    /// the oldest session has the lowest id. It is the one `connect` hands
    /// out, and the next one takes over once it is removed.
    ///
    /// The forwarder sessions this node dialed form a class of their own, with
    /// their own announcing session, since their routes land apart.
    fn is_standby(&self, id: LinkId, peer: &EndpointId, kind: SessionKind) -> bool {
        self.first_session_with(peer, kind)
            .is_some_and(|first| first != id)
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

    /// Takes the rights of the session on link `id`, whose authorization expired.
    ///
    /// The session has no rights until a new decision applies.
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

    /// Returns what the session on link `id` may do.
    ///
    /// That is its authorization's grant and its peer's claimed paths.
    ///
    /// A lapsed or missing authorization grants nothing.
    pub(crate) fn session_grant(&self, id: LinkId) -> Grant {
        let Some(link) = self.links.get(&id) else {
            return Grant::nothing();
        };
        let Some(peer) = link.peer() else {
            return Grant::nothing();
        };
        match admitted(link.authorized()) {
            Some(base) => self.effective_grant(Some(peer), &self.member_grant(peer, &base)),
            None => self.effective_grant(Some(peer), &Grant::nothing()),
        }
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

    /// Returns `base` widened to `<root>/**` in both halves for each group that lists `peer`.
    ///
    /// Only for an admitted session: a group never widens a session that the
    /// policy rejected.
    // TODO(groups): layouts, which narrow the publish half to the member's own
    // scope under the root.
    fn member_grant(&self, peer: &EndpointId, base: &Grant) -> Grant {
        self.groups
            .values()
            .filter(|group| group.members.contains(peer))
            .filter_map(|group| subtree(&group.root))
            .fold(base.clone(), |mut grant, subtree| {
                grant.publish.insert(subtree.clone());
                grant.subscribe.insert(subtree);
                grant
            })
    }

    /// Returns the groups that `peer` may forward for: those with forwarding on that list it.
    fn forwarding_groups_of(&self, peer: &EndpointId) -> BTreeSet<GroupId> {
        self.groups
            .iter()
            .filter(|(_, group)| group.forwarding && group.members.contains(peer))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Reports whether `peer` holds a forwarder session it dialed to this node.
    fn forwards_from_here(&self, peer: &EndpointId) -> bool {
        self.links
            .values()
            .any(|link| link.session() == Some((peer, SessionKind::Forwarded)))
    }

    /// Returns the moq grant of a direct session of `kind` with `peer`, before standby and expiry.
    ///
    /// `base` is the session's admitted grant, or `None` for a session that
    /// may do nothing. Of the groups with forwarding on that list the peer:
    ///
    /// - A session the peer dialed as a forwarder sees their publications,
    ///   `Granted` ones, and no `Peers` one.
    /// - Once the peer holds such a session, its other sessions no longer
    ///   offer those groups' publications.
    /// - A session this node dialed as a forwarder lets the peer publish only
    ///   under the roots of the groups whose hint from the peer this node holds.
    /// - A session with a peer whose hint names this node's site offers the
    ///   paths this node is the ingress for.
    pub(crate) fn session_view(
        &self,
        peer: &EndpointId,
        kind: SessionKind,
        base: Option<&Grant>,
    ) -> auth::Grant {
        let Some(base) = base else {
            return self.view(Viewer::Direct(peer), &Grant::nothing(), &BTreeSet::new());
        };
        let base = self.member_grant(peer, base);
        let forwarding = self.forwarding_groups_of(peer);
        if kind == SessionKind::Forwarded {
            let viewer = Viewer::Forwarder {
                peer,
                groups: &forwarding,
            };
            return self.view(viewer, &base, &BTreeSet::new());
        }
        let hidden = match self.forwards_from_here(peer) {
            true => forwarding.clone(),
            false => BTreeSet::new(),
        };
        let mut grant = self.view(Viewer::Direct(peer), &base, &hidden);
        for id in &forwarding {
            let group = &self.groups[id];
            let same_site = group.site.is_some()
                && group
                    .hints
                    .get(peer)
                    .is_some_and(|hint| hint.site == group.site);
            if same_site {
                grant.subscribe.extend(
                    group
                        .ingress
                        .iter()
                        .filter_map(|path| Pattern::literal(path.as_str()).ok()),
                );
            }
        }
        if kind == SessionKind::Forwarding {
            grant.publish = forwarding
                .iter()
                .map(|id| &self.groups[id])
                .filter(|group| group.hints.get(peer).is_some())
                .filter_map(|group| subtree(&group.root))
                .collect();
        }
        grant
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
    /// a publication, an audience or a group. That is fine at today's sizes,
    /// tens of each. A node with thousands of either needs an index by path.
    // TODO(groups): an index by path, before groups grow beyond 16 members.
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
        let grant = self.link_grant(id, link);
        link.auth.authorize(&grant);
    }

    /// Returns the moq grant of link `id`, from a session's authorization.
    ///
    /// A relay may do everything before audiences. A session without an
    /// authorization, or with a lapsed one, may do nothing. A standby session
    /// may announce nothing (see [`Self::is_standby`]). The grant carries the
    /// authorization's expiry.
    fn link_grant(&self, id: LinkId, link: &LinkEntry) -> auth::Grant {
        match &link.role {
            LinkRole::Relay { .. } => self.link_view(Viewer::Relay, &Grant::everything()),
            LinkRole::Direct {
                peer,
                kind,
                authorized,
            } => {
                let base = admitted(authorized.as_ref());
                let mut grant = self.session_view(peer, *kind, base.as_ref());
                if self.is_standby(id, peer, *kind) {
                    grant.publish = Default::default();
                }
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
        self.view(viewer, base, &BTreeSet::new())
    }

    /// Returns the moq grant [`Self::link_view`] gives, without the publications of `hidden` groups.
    fn view(
        &self,
        viewer: Viewer<'_, EndpointId>,
        base: &Grant,
        hidden: &BTreeSet<GroupId>,
    ) -> auth::Grant {
        let peer = match viewer {
            Viewer::Direct(peer) | Viewer::Forwarder { peer, .. } => Some(peer),
            _ => None,
        };
        let base = &self.effective_grant(peer, base);
        let publish = match peer {
            Some(_) => base.publish.clone(),
            None => Pattern::all().into(),
        };
        let subscribe = moq_ext_access::visible_paths(
            self.publications
                .values()
                .filter(|publication| match &publication.audience {
                    AudienceKind::Group(id, _) => !hidden.contains(id),
                    _ => true,
                })
                .map(|publication| (publication.path.as_str(), &publication.audience)),
            base,
            viewer,
        );
        let mut grant = auth::Grant::default();
        grant.publish = publish;
        grant.subscribe = subscribe;
        grant
    }

    /// Fails if a group at `root` would overlap another group or a publication.
    ///
    /// A group's root may neither cover nor lie under another group's root,
    /// and every publication under it has to be for the group's audience.
    pub(crate) fn check_group_root(&self, id: GroupId, root: &Path) -> Result<(), Error> {
        if let Some(other) = self
            .groups
            .values()
            .find(|other| root.has_prefix(&other.root) || other.root.has_prefix(root))
        {
            return Err(e!(Error::GroupOverlap {
                root: root.clone(),
                other: other.root.clone()
            }));
        }
        if let Some(publication) = self.publications.values().find(|publication| {
            publication.path.has_prefix(root)
                && !matches!(publication.audience, AudienceKind::Group(group, _) if group == id)
        }) {
            return Err(e!(Error::GroupAudience {
                path: publication.path.clone()
            }));
        }
        Ok(())
    }

    /// Adds a group, and re-authorizes every link for it.
    pub(crate) fn add_group(&mut self, id: GroupId, group: GroupEntry) {
        self.groups.insert(id, group);
        self.authorize_all();
    }

    /// Removes group `id` if `handle` still names it, and re-authorizes every link.
    ///
    /// Returns whether the group was removed.
    pub(crate) fn remove_group(&mut self, id: GroupId, handle: u64) -> bool {
        if self
            .groups
            .get(&id)
            .is_none_or(|group| group.handle != handle)
        {
            return false;
        }
        self.groups.remove(&id);
        self.authorize_all();
        true
    }

    /// Replaces the member list of group `id`, and re-authorizes every link.
    pub(crate) fn set_group_members(&mut self, id: GroupId, members: BTreeSet<EndpointId>) {
        let Some(group) = self.groups.get_mut(&id) else {
            return;
        };
        if group.members == members {
            return;
        }
        group.members = members;
        self.authorize_all();
    }

    /// Replaces what group `id`'s driver learned, and re-authorizes every link if it changed.
    pub(crate) fn set_group_view(
        &mut self,
        id: GroupId,
        hints: Hints,
        site: Option<SiteId>,
        ingress: BTreeSet<Path>,
    ) {
        let Some(group) = self.groups.get_mut(&id) else {
            return;
        };
        if group.hints == hints && group.site == site && group.ingress == ingress {
            return;
        }
        group.hints = hints;
        group.site = site;
        group.ingress = ingress;
        self.authorize_all();
    }

    /// Returns the link whose peer announced a route from `source`.
    ///
    /// A route is attributed to the hop its session's peer declares, or else
    /// to the hop this node assigned the session. `None` for a route
    /// published on this node, and for an unknown hop while no relay, or more
    /// than one, feeds the table.
    // TODO(groups): a route that a forwarder session landed reports
    // `Source::Local`, so it finds no link here and `serving_link` returns
    // `None`. Attribute it through the hop chain.
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

/// Returns the pattern of `root` and every path below it.
fn subtree(root: &Path) -> Option<Pattern> {
    format!("{root}/**").parse().ok()
}

/// Returns the grant of an admitted session: `None` without an authorization or once it lapsed.
fn admitted(authorized: Option<&Authorized>) -> Option<Grant> {
    authorized
        .filter(|authorized| !authorized.lapsed)
        .map(Authorized::grant)
}

/// Returns what every link of the node serves its peer: the table's local view.
///
/// It hides every route the node learned from a peer or a relay, so the node
/// never carries traffic between two of its links. It includes paths with a
/// segment that starts with `.`, which moq leaves out of a listing by
/// default: a moq-lite-06 peer cannot ask for them, and a group's hints live
/// at `<root>/.hint/<member>`. The grants still decide what a peer sees.
pub(crate) fn served(table: &origin::Producer) -> origin::Consumer {
    table.consume().local().with_hidden(true)
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;
    use moq_net::origin::RouteOptions;

    use super::*;
    use crate::group::{Hints, SiteId};

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
        state.link_grant(id, &state.links[&id])
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
                kind: SessionKind::Plain,
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

    #[test]
    fn only_the_oldest_session_with_a_peer_may_announce() {
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        let own = format!("live/{peer}/cam");
        add(&mut state, LinkId(1), direct_with(peer, own_paths(peer)));
        add(&mut state, LinkId(2), direct_with(peer, own_paths(peer)));
        add(&mut state, LinkId(3), direct());
        assert!(grant_of(&state, LinkId(1)).publish.matches(&own));
        let standby = grant_of(&state, LinkId(2));
        assert!(!standby.publish.matches(&own), "the standby may announce");
        assert_eq!(
            standby.subscribe,
            grant_of(&state, LinkId(1)).subscribe,
            "the standby serves what the first serves"
        );
        assert!(
            grant_of(&state, LinkId(3)).publish.matches("anything"),
            "another peer's session is no standby"
        );
        assert_eq!(state.session_grant(LinkId(2)), own_paths(peer));

        state.remove_link(LinkId(1));
        assert!(
            grant_of(&state, LinkId(2)).publish.matches(&own),
            "the standby did not take over"
        );
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

    /// The id of the group the group tests use.
    const GROUP: GroupId = GroupId::from_bytes([9; 32]);

    /// Returns the site with public address 198.18.0.`n` in [`GROUP`].
    fn site(n: u8) -> SiteId {
        SiteId::new(GROUP, std::net::Ipv4Addr::new(198, 18, 0, n))
    }

    /// Adds group [`GROUP`] at `party`, with forwarding on, `members`, this node at `own`, and `hints`.
    fn add_group(
        state: &mut State,
        members: &[EndpointId],
        own: Option<SiteId>,
        hints: &[(EndpointId, Option<SiteId>)],
    ) {
        let mut kept = Hints::default();
        for (member, site) in hints {
            kept.offer(
                *member,
                crate::group::Hint::new(GROUP, *site, crate::group::now_ms()),
            );
        }
        state.add_group(
            GROUP,
            GroupEntry {
                root: Path::new("party").expect("a path"),
                handle: 1,
                group: Weak::new(),
                members: members.iter().copied().collect(),
                forwarding: true,
                hints: kept,
                site: own,
                ingress: BTreeSet::new(),
            },
        );
    }

    /// Returns a direct link of `kind` with `peer`, whose session holds every right.
    fn session(peer: EndpointId, kind: SessionKind) -> LinkEntry {
        let mut link = direct_with(peer, Grant::everything());
        if let LinkRole::Direct { kind: own, .. } = &mut link.role {
            *own = kind;
        }
        link
    }

    #[tokio::test]
    async fn a_forwarding_link_may_publish_only_under_shared_roots() {
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        add_group(&mut state, &[peer], Some(site(1)), &[(peer, Some(site(2)))]);
        add(
            &mut state,
            LinkId(1),
            session(peer, SessionKind::Forwarding),
        );
        let grant = grant_of(&state, LinkId(1));
        assert!(grant.publish.matches("party/x/cam"));
        assert!(!grant.publish.matches("live/x/cam"), "outside the root");

        // Without the peer's hint, the session may announce nothing.
        state.set_group_view(GROUP, Hints::default(), Some(site(1)), BTreeSet::new());
        assert!(!grant_of(&state, LinkId(1)).publish.matches("party/x/cam"));
    }

    #[tokio::test]
    async fn a_forwarded_link_never_lists_a_peers_publication() {
        let table = Table::new();
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        add_group(&mut state, &[peer], Some(site(1)), &[]);
        add(&mut state, LinkId(1), session(peer, SessionKind::Forwarded));
        let members = BTreeSet::from([peer]);
        state.add_publication(
            10,
            table.publication("party/me/cam", AudienceKind::Group(GROUP, members.clone())),
        );
        state.add_publication(
            11,
            table.publication("private", AudienceKind::Peers(members)),
        );
        state.add_publication(12, table.publication("public", AudienceKind::Granted));
        let grant = grant_of(&state, LinkId(1));
        assert!(grant.subscribe.matches("party/me/cam"));
        assert!(!grant.subscribe.matches("private"));
        assert!(grant.subscribe.matches("public"));
    }

    #[tokio::test]
    async fn group_publications_move_from_the_plain_link_to_the_forwarded_one() {
        let table = Table::new();
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        add_group(&mut state, &[peer], Some(site(1)), &[]);
        add(&mut state, LinkId(1), session(peer, SessionKind::Plain));
        let members = BTreeSet::from([peer]);
        state.add_publication(
            10,
            table.publication("party/me/cam", AudienceKind::Group(GROUP, members.clone())),
        );
        state.add_publication(
            11,
            table.publication("private", AudienceKind::Peers(members)),
        );
        assert!(
            grant_of(&state, LinkId(1))
                .subscribe
                .matches("party/me/cam")
        );

        add(&mut state, LinkId(2), session(peer, SessionKind::Forwarded));
        let plain = grant_of(&state, LinkId(1));
        assert!(
            !plain.subscribe.matches("party/me/cam"),
            "still on the plain link"
        );
        assert!(plain.subscribe.matches("private"), "a `Peers` one stays");
        assert!(
            grant_of(&state, LinkId(2))
                .subscribe
                .matches("party/me/cam")
        );

        state.remove_link(LinkId(2));
        assert!(
            grant_of(&state, LinkId(1))
                .subscribe
                .matches("party/me/cam")
        );
    }

    #[tokio::test]
    async fn ingress_paths_are_offered_to_same_site_members_only() {
        let mut state = State::default();
        let (near, far) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        add_group(
            &mut state,
            &[near, far],
            Some(site(1)),
            &[(near, Some(site(1))), (far, Some(site(2)))],
        );
        add(&mut state, LinkId(1), session(near, SessionKind::Plain));
        add(&mut state, LinkId(2), session(far, SessionKind::Plain));
        add(&mut state, LinkId(3), session(far, SessionKind::Forwarded));
        let forwarded = Path::new("party/remote/cam").expect("a path");
        let hints = state.groups[&GROUP].hints.clone();
        state.set_group_view(
            GROUP,
            hints,
            Some(site(1)),
            BTreeSet::from([forwarded.clone()]),
        );
        assert!(
            grant_of(&state, LinkId(1))
                .subscribe
                .matches(forwarded.as_str())
        );
        for link in [LinkId(2), LinkId(3)] {
            assert!(
                !grant_of(&state, link).subscribe.matches(forwarded.as_str()),
                "{link}"
            );
        }
    }

    #[test]
    fn each_session_class_has_one_announcing_session() {
        let mut state = State::default();
        let peer = SecretKey::generate().public();
        add_group(&mut state, &[peer], Some(site(1)), &[(peer, Some(site(2)))]);
        add(&mut state, LinkId(1), session(peer, SessionKind::Plain));
        add(
            &mut state,
            LinkId(2),
            session(peer, SessionKind::Forwarding),
        );
        add(&mut state, LinkId(3), session(peer, SessionKind::Forwarded));
        add(
            &mut state,
            LinkId(4),
            session(peer, SessionKind::Forwarding),
        );
        let announces = |state: &State, id| grant_of(state, id).publish.matches("party/x/cam");
        assert!(announces(&state, LinkId(1)));
        assert!(announces(&state, LinkId(2)), "the first of its own class");
        assert!(!announces(&state, LinkId(3)), "a standby of the plain one");
        assert!(!announces(&state, LinkId(4)));
        state.remove_link(LinkId(2));
        assert!(
            announces(&state, LinkId(4)),
            "the next forwarder session takes over"
        );
        assert!(!announces(&state, LinkId(3)));
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
