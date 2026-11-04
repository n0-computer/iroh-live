//! The node's bookkeeping: publications, links, and what each link may see.
//!
//! Everything here changes under one lock and never awaits, so `publish` and
//! `unpublish` are synchronous.
//!
//! Every publication is a broadcast created in the node's route table, and
//! every link serves the table's local view (see [`served`]). What a link's
//! peer may see is its grant, which moq enforces on the live session: the
//! exact paths of the publications whose audience admits the link. Whenever a
//! publication, an audience or the set of links changes, the state works the
//! grants out again and re-authorizes each link's session. What links announce
//! goes straight into the table, so the state only maps a route's source back
//! to its link (see [`State::link_for`]).

use std::collections::BTreeMap;

use iroh::EndpointId;
use moq_net::{Hop, Path, Pattern, auth, origin, origin::Source};
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use moq_ext_access::LinkRole;

use crate::{Grant, LinkKind, Session, link::LinkState, publish::AudienceKind};

/// The node's publications and links.
#[derive(Debug, Default)]
pub(crate) struct State {
    next_id: u64,
    pub(crate) publications: BTreeMap<u64, PubEntry>,
    pub(crate) links: BTreeMap<u64, LinkEntry>,
    /// Set once the node shuts down; nothing is added after that.
    pub(crate) closed: bool,
}

/// One publication.
#[derive(derive_more::Debug)]
pub(crate) struct PubEntry {
    pub(crate) path: Path,
    /// The broadcast in the route table, and its announcement.
    #[debug(skip)]
    pub(crate) publication: origin::Publication,
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
        self.publication.unannounce();
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
    /// What the link's peer may do at most, before audiences.
    pub(crate) grant: Grant,
    /// For a relay, whether `Everyone` publications go to it.
    pub(crate) public: bool,
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
    pub(crate) fn add_link(&mut self, id: u64, link: LinkEntry) {
        self.links.insert(id, link);
        self.authorize(id);
    }

    /// Removes a link.
    ///
    /// Its session takes its routes out of the route table as it ends.
    pub(crate) fn remove_link(&mut self, id: u64) {
        self.links.remove(&id);
    }

    /// Adds a publication and re-authorizes every link for it.
    pub(crate) fn add_publication(&mut self, id: u64, publication: PubEntry) {
        self.publications.insert(id, publication);
        self.authorize_all();
    }

    /// Removes a publication, and re-authorizes every link without it.
    ///
    /// Narrowing a link's grant retracts the path from its peer and resets
    /// what the peer reads there. The path leaves the route table when the
    /// returned entry drops.
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

    /// Sets the base grant of link `id`, and re-authorizes it.
    pub(crate) fn set_grant(&mut self, id: u64, grant: Grant) {
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        link.grant = grant;
        self.authorize(id);
    }

    /// Returns the publication at `path`, if any.
    pub(crate) fn publication_at(&self, path: &Path) -> Option<u64> {
        self.publications
            .iter()
            .find(|(_, publication)| publication.path == *path)
            .map(|(id, _)| *id)
    }

    /// Re-authorizes every link for what it may see now.
    fn authorize_all(&self) {
        for id in self.links.keys() {
            self.authorize(*id);
        }
    }

    /// Re-authorizes link `id` for what it may see now.
    ///
    /// moq skips a grant equal to the last one, so a change that leaves the
    /// link's view alone costs its session nothing.
    fn authorize(&self, id: u64) {
        let Some(link) = self.links.get(&id) else {
            return;
        };
        let grant = self.grant_for(link.kind, link.remote.as_ref(), &link.grant, link.public);
        link.auth.authorize(&grant);
    }

    /// Returns the moq grant of a link, from the publications it may see.
    ///
    /// Its subscribe half lists the exact paths of the publications whose
    /// audience admits the link, within `base`, so it grows with them. See
    /// [`moq_ext_access::visible_paths`] for the limits that puts on the
    /// number of publications. Its publish half is what the
    /// peer may announce into the route table: `base`'s for a session, and
    /// anything for a relay, which is trusted with what it forwards.
    pub(crate) fn grant_for(
        &self,
        kind: LinkKind,
        remote: Option<&EndpointId>,
        base: &Grant,
        public: bool,
    ) -> auth::Grant {
        let role = match kind {
            LinkKind::Direct => LinkRole::Direct,
            LinkKind::Relay => LinkRole::Relay { public },
        };
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
    /// to the hop this node assigned the session. The two agree for peers on
    /// this crate. A hop no link has is taken to be one a relay declares of
    /// its own, as moq-relay does, and maps to the oldest relay link. `None`
    /// for a route published on this node, and for an unknown hop while no
    /// relay is attached.
    pub(crate) fn link_for(&self, source: Source) -> Option<u64> {
        let Source::Peer(hop) = source else {
            return None;
        };
        if let Some(id) = self.link_with_hop(hop) {
            return Some(id);
        }
        // moq-relay declares a hop of its own, not the one derived from its
        // endpoint id, and iroh-live-node peers always carry the derived one. So an
        // unknown hop is a relay's: the oldest relay link, if there is one.
        self.links
            .iter()
            .filter(|(_, link)| link.kind == LinkKind::Relay)
            .map(|(id, _)| *id)
            .min()
    }

    /// Returns the link whose peer carries `hop`, if any.
    pub(crate) fn link_with_hop(&self, hop: moq_net::Hop) -> Option<u64> {
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
/// It holds this node's publications and hides every route the node learned
/// from a peer or a relay, so the node never carries traffic between two of
/// its links. A link's grant then narrows it to the publications whose
/// audience admits the link.
pub(crate) fn served(table: &origin::Producer) -> origin::Consumer {
    table.consume().local()
}
