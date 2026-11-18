//! Groups: members that share a path root, and forward for each other per site.
//!
//! A [`Group`] names the peers allowed under a path root. Its members may
//! publish and subscribe anywhere under the root, and [`Group::audience`]
//! admits exactly them. With [`GroupOptions::forwarding`] on, members on one
//! site, behind one NAT or public address, also share the site's egress: for
//! each remote broadcast one member of the site pulls it across, and serves
//! the rest of the site over the LAN.
//!
//! Groups are experimental. The member list comes from the application, and
//! this node trusts it as given.
//!
//! How forwarding works:
//!
//! - Each member signs a hint with its site and publishes it at
//!   `<root>/.hint/<member id>`, for the other members only.
//! - Each member dials every remote member a second time, as a forwarder. On
//!   that session the remote member offers its group publications, and
//!   withholds them from the plain session. The routes land at a lower
//!   priority than a plain session's.
//! - For each path under the root, the members of a site agree on one
//!   ingress, by rendezvous hashing over the members whose hint names the
//!   site. The ingress offers the path on its plain sessions with the rest
//!   of the site. That route outranks each member's own forwarder route, so
//!   the site reads one copy, and falls back to its own route when the
//!   ingress goes away.

// TODO(groups): the `custom_group` example and `docs/concepts/groups.md`,
// once layouts and app hints settle the API.

mod driver;
mod hint;
mod plan;
mod site;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeSet,
    sync::{Arc, Weak},
};

use iroh::EndpointId;
pub use moq_ext_access::GroupId;
use moq_net::Path;
use n0_error::e;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watchable;
use tracing::{Instrument, info, info_span};

pub(crate) use self::{hint::Hints, site::SiteId};
use crate::{Audience, Error, Node, node::Shared, publish::check_path, state::GroupEntry};

/// The path segment under a group's root that holds the members' hints.
pub(crate) const HINT_SEGMENT: &str = ".hint";

/// How a group behaves on this node. The default is a named audience that never forwards.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GroupOptions {
    /// Whether members forward for each other on this node, and read through forwarders.
    pub forwarding: bool,
    // TODO(groups): `forward_class`, `layout` (owner checks at the publisher,
    // the forwarder and the viewer), `app_hints` (`with_app_hints`,
    // `Group::local_hint`, `Group::apply`, `Error::InvalidHint`), the
    // `GroupAuthority::Principal` authority, and one rendition per site, off
    // by default.
}

impl GroupOptions {
    /// Returns these options with forwarding on or off.
    #[must_use]
    pub fn with_forwarding(mut self, forwarding: bool) -> Self {
        self.forwarding = forwarding;
        self
    }
}

/// The plan of one group, as this node computes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroupPlan {
    /// The members this node shares a site with, by their hints.
    pub site: BTreeSet<EndpointId>,
    /// The paths this node is the ingress for in its site.
    pub ingress: BTreeSet<Path>,
    // TODO(groups): `via`, the forwarder each remote path is read through,
    // once `Node::subscribe` waits for the planned route.
}

/// A group on this node. Cheap to clone. The group ends when the last handle drops.
///
/// # Examples
///
/// ```no_run
/// # fn run(node: iroh_live_node::Node, members: Vec<iroh::EndpointId>)
/// # -> Result<(), iroh_live_node::Error> {
/// use iroh_live_node::GroupOptions;
///
/// let group = node.group("party", GroupOptions::default().with_forwarding(true))?;
/// group.set_members(members);
/// let _publication = node.publish(format!("party/{}/cam", node.id()), group.audience())?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Group {
    inner: Arc<GroupInner>,
}

/// What every clone of a [`Group`] shares.
#[derive(derive_more::Debug)]
pub(crate) struct GroupInner {
    id: GroupId,
    root: Path,
    /// Tells this group from a later one with the same root.
    handle: u64,
    #[debug(skip)]
    members: Watchable<BTreeSet<EndpointId>>,
    #[debug(skip)]
    plan: Watchable<GroupPlan>,
    #[debug(skip)]
    shared: Weak<Shared>,
    /// Runs the forwarding, while forwarding is on.
    #[debug(skip)]
    _driver: Option<AbortOnDropHandle<()>>,
}

impl Drop for GroupInner {
    fn drop(&mut self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let Ok(mut state) = shared.state.lock() else {
            return;
        };
        if state.remove_group(self.id, self.handle) {
            info!(root = %self.root, "group ended");
        }
    }
}

impl Group {
    /// Returns the group's id, the hash of its root.
    #[must_use]
    pub fn id(&self) -> GroupId {
        self.inner.id
    }

    /// Returns the path root the group's paths lie under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Replaces the member list. This node's own id may be left out.
    ///
    /// Takes effect on every live session at once, as an audience change does.
    pub fn set_members(&self, members: impl IntoIterator<Item = EndpointId>) {
        let members: BTreeSet<EndpointId> = members.into_iter().collect();
        if let Some(shared) = self.inner.shared.upgrade() {
            let mut state = shared.state.lock().expect("poisoned");
            state.set_group_members(self.inner.id, members.clone());
        }
        self.inner.members.set(members).ok();
    }

    /// Returns the audience of the group's members, for [`Node::publish`].
    ///
    /// It follows the member list.
    #[must_use]
    pub fn audience(&self) -> Audience {
        Audience::Group {
            id: self.inner.id,
            members: self.inner.members.clone(),
        }
    }

    /// Returns the plan as this node computes it, as it changes.
    ///
    /// It stays empty while forwarding is off.
    #[must_use]
    pub fn plan(&self) -> n0_watcher::Direct<GroupPlan> {
        self.inner.plan.watch()
    }
}

/// Returns the id of the group with root `root`.
pub(crate) fn group_id(root: &Path) -> GroupId {
    GroupId::from_bytes(blake3::derive_key(
        "iroh-live group v1",
        root.as_str().as_bytes(),
    ))
}

/// Creates the group at `root`, or returns the one that exists, for [`Node::group`].
pub(crate) fn group(node: &Node, root: Path, options: GroupOptions) -> Result<Group, Error> {
    check_path(&root)?;
    let shared = &node.shared;
    let id = group_id(&root);
    let mut state = shared.state.lock().expect("poisoned");
    if state.closed {
        return Err(e!(Error::ShutDown));
    }
    if let Some(existing) = state.groups.get(&id) {
        if let Some(inner) = existing.group.upgrade() {
            return Ok(Group { inner });
        }
        // The last handle is dropping and has not removed its entry yet. Its
        // removal checks the handle, so it leaves the new group alone.
        state.groups.remove(&id);
    }
    state.check_group_root(id, &root)?;
    let members = Watchable::new(BTreeSet::new());
    let plan = Watchable::new(GroupPlan::default());
    let handle = state.next_id();
    let driver = options.forwarding.then(|| {
        let driver = driver::Driver::new(shared, id, root.clone(), &members, plan.clone());
        let span = info_span!("group", root = %root);
        AbortOnDropHandle::new(tokio::spawn(driver.run().instrument(span)))
    });
    let inner = Arc::new(GroupInner {
        id,
        root: root.clone(),
        handle,
        members,
        plan,
        shared: Arc::downgrade(shared),
        _driver: driver,
    });
    state.add_group(
        id,
        GroupEntry::new(root.clone(), handle, options.forwarding, &inner),
    );
    info!(%root, forwarding = options.forwarding, "group created");
    Ok(Group { inner })
}
