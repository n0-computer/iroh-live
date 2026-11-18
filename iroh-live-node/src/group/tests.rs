//! Tests of groups on nodes in one process.
//!
//! Localhost has no public address, so each node gets one through the site
//! hook: sites L and M stand for two NATed networks.

use std::{collections::BTreeSet, net::Ipv4Addr, time::Duration};

use iroh::EndpointId;
use moq_net::Path;
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;

use super::{GroupOptions, group_id, site::test_hook};
use crate::{
    Audience, Error, Grant, Group, NodeConfig, Publication, Reach, Session, Subscription,
    policy::Open,
    state::SessionKind,
    test_util::{TestNode, eventually, step},
};

/// The root of the test group.
const ROOT: &str = "party";

/// How long a test waits to be sure something does not arrive.
const QUIET: Duration = Duration::from_secs(2);

/// One member: its node, its group handle and its publication.
struct Member {
    node: TestNode,
    group: Group,
    _publication: Publication,
}

impl Member {
    /// Returns the path the member publishes at.
    fn path(&self) -> Path {
        cam(self.node.id())
    }
}

/// Returns the path member `id` publishes at.
fn cam(id: EndpointId) -> Path {
    Path::new(&format!("{ROOT}/{id}/cam")).expect("a valid path")
}

/// Spawns a node that lets every peer do everything.
async fn open_node() -> TestNode {
    TestNode::spawn(NodeConfig::default().with_policy(Open(Grant::everything()))).await
}

/// Spawns members, one per entry of `sites`, each at that public address.
///
/// `None` is a member whose group does not forward, as an old node behaves:
/// it publishes no hint. Every member lists all of them.
async fn members(sites: &[Option<Ipv4Addr>], forwarding: bool) -> Vec<Member> {
    let mut nodes = Vec::new();
    for site in sites {
        let node = open_node().await;
        if let Some(addr) = site {
            test_hook::set_public_v4(node.id(), *addr);
        }
        nodes.push((node, site.is_some() && forwarding));
    }
    let ids: Vec<EndpointId> = nodes.iter().map(|(node, _)| node.id()).collect();
    let mut members = Vec::new();
    for (node, forwarding) in nodes {
        let options = GroupOptions::default().with_forwarding(forwarding);
        let group = node.node.group(ROOT, options).expect("a group");
        group.set_members(ids.iter().copied());
        let publication = node
            .node
            .publish(cam(node.id()), group.audience())
            .expect("publish");
        members.push(Member {
            node,
            group,
            _publication: publication,
        });
    }
    members
}

/// Waits until each member's plan lists `site` of the others in its site.
async fn settled(members: &[Member], sites: &[&[usize]]) {
    eventually("the plans list every site", || {
        sites.iter().all(|site| {
            site.iter().all(|&n| {
                let expected: BTreeSet<EndpointId> = site
                    .iter()
                    .filter(|&&other| other != n)
                    .map(|&other| members[other].node.id())
                    .collect();
                members[n].group.plan().get().site == expected
            })
        })
    })
    .await;
}

/// Returns the member of `site` whose plan names it the ingress for `path`.
async fn ingress_of(members: &[Member], site: &[usize], path: &Path) -> usize {
    let mut found = None;
    eventually(&format!("one ingress for {path}"), || {
        let ingress: Vec<usize> = site
            .iter()
            .copied()
            .filter(|&n| members[n].group.plan().get().ingress.contains(path))
            .collect();
        found = ingress.first().copied();
        ingress.len() == 1
    })
    .await;
    found.expect("an ingress")
}

/// Returns the kinds of `node`'s sessions with `peer`.
fn kinds(sessions: &[Session], peer: EndpointId) -> Vec<SessionKind> {
    sessions
        .iter()
        .filter(|session| session.peer() == peer)
        .map(|session| session.inner.kind)
        .collect()
}

/// Reports whether `subscription` reads through a route its node's own forwarder session landed.
fn reads_own_forwarder(subscription: &Subscription) -> bool {
    subscription.session().is_none() && subscription.serving_link().is_none()
}

/// Shuts every member down.
async fn shutdown(members: &[Member]) {
    for member in members {
        member.node.shutdown().await;
    }
}

#[test]
fn the_same_root_gives_the_same_group_id() {
    let root = Path::new("party").expect("a path");
    assert_eq!(group_id(&root), group_id(&root));
    assert_ne!(
        group_id(&root),
        group_id(&Path::new("party2").expect("a path"))
    );
}

#[tokio::test]
#[traced_test]
async fn overlapping_roots_and_foreign_audiences_are_refused() {
    let node = open_node().await;
    let group = node
        .node
        .group("party", GroupOptions::default())
        .expect("a group");
    let again = node
        .node
        .group("party", GroupOptions::default())
        .expect("the same group");
    assert_eq!(again.id(), group.id());
    for root in ["party/room", ""] {
        let err = node
            .node
            .group(root, GroupOptions::default())
            .expect_err("an overlapping root");
        assert!(
            matches!(err, Error::GroupOverlap { .. } | Error::InvalidPath { .. }),
            "{err:#}"
        );
    }
    let err = node
        .node
        .publish("party/me/cam", Audience::Granted)
        .expect_err("another audience under the root");
    assert!(matches!(err, Error::GroupAudience { .. }), "{err:#}");
    let err = node
        .node
        .publish("party/.hint/me", group.audience())
        .expect_err("the hint subtree");
    assert!(matches!(err, Error::GroupAudience { .. }), "{err:#}");
    node.node
        .publish("party/me/cam", group.audience())
        .expect("the group's audience");

    let _taken = node
        .node
        .publish("other/cam", Audience::Granted)
        .expect("publish");
    let err = node
        .node
        .group("other", GroupOptions::default())
        .expect_err("a publication with another audience under the root");
    assert!(matches!(err, Error::GroupAudience { .. }), "{err:#}");
    node.shutdown().await;
}

/// Two sites, L = {A, B, C} and M = {D, E}, plus O on L, which publishes no hint.
#[tokio::test]
#[traced_test]
async fn each_site_reads_a_remote_broadcast_through_its_ingress() {
    let l = Some(Ipv4Addr::new(198, 18, 1, 1));
    let m = Some(Ipv4Addr::new(198, 18, 2, 1));
    let members = members(&[l, l, l, m, m, None], true).await;
    let (site_l, site_m): (&[usize], &[usize]) = (&[0, 1, 2], &[3, 4]);
    let old = 5;
    step("plans settle", settled(&members, &[site_l, site_m])).await;

    for (viewers, publishers) in [(site_l, site_m), (site_m, site_l)] {
        for &publisher in publishers {
            let path = members[publisher].path();
            let ingress = ingress_of(&members, viewers, &path).await;
            for &viewer in viewers {
                let subscription = step(
                    "subscribe",
                    members[viewer]
                        .node
                        .node
                        .subscribe(path.clone(), Reach::Direct(members[publisher].node.id())),
                )
                .await
                .expect("subscribe");
                let want = members[ingress].node.id();
                let what = format!("viewer {viewer} reads {path} through ingress {ingress}");
                eventually(&what, || match viewer == ingress {
                    true => reads_own_forwarder(&subscription),
                    false => subscription.session().is_some_and(|s| s.peer() == want),
                })
                .await;
            }
        }
    }

    // The member without a hint reads and is read directly, without forwarder sessions.
    let o = members[old].node.id();
    for &other in site_l.iter().chain(site_m) {
        let subscription = step(
            "subscribe from the old node",
            members[old].node.node.subscribe(
                members[other].path(),
                Reach::Direct(members[other].node.id()),
            ),
        )
        .await
        .expect("subscribe");
        let peer = members[other].node.id();
        eventually("the old node reads directly", || {
            subscription.session().is_some_and(|s| s.peer() == peer)
        })
        .await;
        let subscription = step(
            "subscribe to the old node",
            members[other]
                .node
                .node
                .subscribe(members[old].path(), Reach::Direct(o)),
        )
        .await
        .expect("subscribe");
        eventually("the old node is read directly", || {
            subscription.session().is_some_and(|s| s.peer() == o)
        })
        .await;
        let sessions = members[other].node.node.sessions().get();
        assert!(
            kinds(&sessions, o)
                .iter()
                .all(|kind| *kind == SessionKind::Plain),
            "a forwarder session with the old node"
        );
    }
    shutdown(&members).await;
}

#[tokio::test]
#[traced_test]
async fn a_forwarder_serves_neither_outsiders_nor_peers_publications() {
    let l = Some(Ipv4Addr::new(198, 18, 3, 1));
    let m = Some(Ipv4Addr::new(198, 18, 4, 1));
    let members = members(&[l, l, m], true).await;
    step("plans settle", settled(&members, &[&[0, 1], &[2]])).await;
    let remote = &members[2];
    let path = remote.path();
    let ingress = ingress_of(&members, &[0, 1], &path).await;
    let viewer = 1 - ingress;

    // A `Peers` publication of the remote member, for the ingress alone.
    let secret = "private/secret";
    let _secret = remote
        .node
        .node
        .publish(secret, Audience::peers([members[ingress].node.id()]))
        .expect("publish");
    step(
        "the ingress reads the secret directly",
        members[ingress]
            .node
            .node
            .subscribe(secret, Reach::Direct(remote.node.id())),
    )
    .await
    .expect("subscribe");
    let reached = tokio::time::timeout(
        QUIET,
        members[viewer]
            .node
            .node
            .subscribe(secret, Reach::Direct(members[ingress].node.id())),
    )
    .await;
    assert!(
        !matches!(reached, Ok(Ok(_))),
        "the secret reached a third node"
    );

    // A node outside the group, on the ingress's network, gets nothing forwarded.
    let outsider = open_node().await;
    let session = step(
        "the outsider connects to the ingress",
        outsider.node.connect(members[ingress].node.id()),
    )
    .await
    .expect("connect");
    let reached = tokio::time::timeout(QUIET, session.subscribe(path.clone())).await;
    assert!(
        !matches!(reached, Ok(Ok(_))),
        "the outsider read a forwarded path"
    );
    outsider.shutdown().await;
    shutdown(&members).await;
}

#[tokio::test]
#[traced_test]
async fn without_forwarding_no_node_holds_a_forwarder_session() {
    let l = Some(Ipv4Addr::new(198, 18, 5, 1));
    let m = Some(Ipv4Addr::new(198, 18, 6, 1));
    let members = members(&[l, l, m], false).await;
    for viewer in 0..members.len() {
        for publisher in (0..members.len()).filter(|&n| n != viewer) {
            let peer = members[publisher].node.id();
            let subscription = step(
                "subscribe",
                members[viewer]
                    .node
                    .node
                    .subscribe(members[publisher].path(), Reach::Direct(peer)),
            )
            .await
            .expect("subscribe");
            assert_eq!(
                subscription.session().map(|session| session.peer()),
                Some(peer)
            );
        }
    }
    for member in &members {
        assert!(
            member
                .node
                .node
                .sessions()
                .get()
                .iter()
                .all(|session| session.inner.kind == SessionKind::Plain)
        );
        assert_eq!(
            member.group.plan().get(),
            super::GroupPlan::default(),
            "no plan without forwarding"
        );
    }
    shutdown(&members).await;
}
