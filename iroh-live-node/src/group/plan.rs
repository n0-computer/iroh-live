//! The plan of a group: which paths this node pulls across its site's egress.
//!
//! The plan is a pure function of what every member of a site knows alike:
//! the member list, the members' hints, and the paths under the root. So the
//! members of one site agree on one ingress per path without talking.

// TODO(groups): weighted rendezvous hashing over `ForwardClass`, so a phone
// that never forwards is never picked.

use std::collections::BTreeSet;

use iroh::EndpointId;
use moq_net::Path;

use super::{GroupPlan, hint::Hints, site::SiteId};

/// What a member of a group knows when it plans.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PlanInput<'a> {
    /// This node's id.
    pub(crate) own: EndpointId,
    /// This node's site, or `None` without a public address.
    pub(crate) site: Option<SiteId>,
    /// The member list, which may or may not hold this node.
    pub(crate) members: &'a BTreeSet<EndpointId>,
    /// The members' valid hints.
    pub(crate) hints: &'a Hints,
    /// The paths under the root that some route serves.
    pub(crate) paths: &'a BTreeSet<Path>,
}

impl PlanInput<'_> {
    /// Returns the plan of this node.
    ///
    /// A node without a site forwards nothing. The willing members of the
    /// site are this node and the members whose hint names its site and does
    /// not refuse to forward. Of them, the one with the highest rendezvous
    /// score for a path is its ingress.
    pub(crate) fn plan(&self) -> GroupPlan {
        let Some(site) = self.site else {
            return GroupPlan::default();
        };
        let same_site: BTreeSet<EndpointId> = self
            .members
            .iter()
            .filter(|member| **member != self.own)
            .filter(|member| {
                self.hints
                    .get(member)
                    .is_some_and(|hint| hint.site == Some(site) && hint.weight > 0)
            })
            .copied()
            .collect();
        let ingress = match same_site.is_empty() {
            // A lone member reads every path directly.
            true => BTreeSet::new(),
            false => self
                .paths
                .iter()
                .filter(|path| {
                    let own = score(path, &self.own);
                    same_site.iter().all(|member| score(path, member) < own)
                })
                .cloned()
                .collect(),
        };
        GroupPlan {
            site: same_site,
            ingress,
        }
    }

    /// Reports whether this node holds a forwarder session with `member`, given its `plan`.
    ///
    /// It does for a remote member whose hint it holds, while another member
    /// shares its site. A member is remote when its hint names another site,
    /// or none, and every member is remote to a node without a site.
    pub(crate) fn dials_forwarder(&self, plan: &GroupPlan, member: &EndpointId) -> bool {
        if *member == self.own || !self.members.contains(member) {
            return false;
        }
        let Some(site) = self.site else {
            return false;
        };
        !plan.site.is_empty()
            && self
                .hints
                .get(member)
                .is_some_and(|hint| hint.site != Some(site))
    }
}

/// Returns the rendezvous score of `member` for `path`: the highest score wins.
fn score(path: &Path, member: &EndpointId) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"iroh-live ingress v1");
    hasher.update(path.as_str().as_bytes());
    hasher.update(member.as_bytes());
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;
    use moq_ext_access::GroupId;

    use super::*;
    use crate::group::hint::Hint;

    const NOW: u64 = 1_800_000_000_000;

    fn site(n: u8) -> SiteId {
        SiteId::new(
            GroupId::from_bytes([1; 32]),
            std::net::Ipv4Addr::new(198, 18, 0, n),
        )
    }

    /// A group of members, each with a site or none.
    struct Fixture {
        members: BTreeSet<EndpointId>,
        hints: Hints,
        sites: Vec<(EndpointId, Option<SiteId>)>,
        paths: BTreeSet<Path>,
    }

    impl Fixture {
        /// Creates members at `sites`, with hints, and `paths` paths.
        fn new(sites: &[Option<u8>], paths: usize) -> Self {
            let mut fixture = Self {
                members: BTreeSet::new(),
                hints: Hints::default(),
                sites: Vec::new(),
                paths: (0..paths)
                    .map(|n| Path::new(&format!("room/p{n}/cam")).expect("a path"))
                    .collect(),
            };
            for site in sites {
                let id = SecretKey::generate().public();
                let site = site.map(self::site);
                fixture.members.insert(id);
                fixture
                    .hints
                    .offer(id, Hint::new(GroupId::from_bytes([1; 32]), site, NOW));
                fixture.sites.push((id, site));
            }
            fixture
        }

        /// Returns the input member `n` plans with.
        fn input(&self, n: usize) -> PlanInput<'_> {
            let (own, site) = self.sites[n];
            PlanInput {
                own,
                site,
                members: &self.members,
                hints: &self.hints,
                paths: &self.paths,
            }
        }

        /// Returns the ingress of each path in the site of member `n`, by every member there.
        fn ingress_of_site(&self, n: usize) -> Vec<(Path, EndpointId)> {
            let site = self.sites[n].1;
            let mut ingress = Vec::new();
            for (index, (id, _)) in self.sites.iter().enumerate() {
                if self.sites[index].1 != site {
                    continue;
                }
                for path in self.input(index).plan().ingress {
                    ingress.push((path, *id));
                }
            }
            ingress.sort();
            ingress
        }
    }

    #[test]
    fn every_path_has_one_ingress_per_site() {
        let fixture = Fixture::new(&[Some(1), Some(1), Some(1), Some(2), Some(2)], 20);
        for member in [0, 3] {
            let ingress = fixture.ingress_of_site(member);
            let paths: Vec<&Path> = ingress.iter().map(|(path, _)| path).collect();
            assert_eq!(paths, fixture.paths.iter().collect::<Vec<_>>());
        }
        let site_one: BTreeSet<EndpointId> = fixture
            .ingress_of_site(0)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert!(site_one.len() > 1, "20 paths spread over three members");
    }

    #[test]
    fn the_same_inputs_give_the_same_plan() {
        let fixture = Fixture::new(&[Some(1), Some(1), Some(2)], 8);
        assert_eq!(fixture.input(0).plan(), fixture.input(0).plan());
        assert_eq!(
            fixture.input(0).plan().site,
            BTreeSet::from([fixture.sites[1].0])
        );
        assert_eq!(
            fixture.input(2).plan(),
            GroupPlan::default(),
            "a lone member of its site plans nothing"
        );
    }

    #[test]
    fn removing_a_member_moves_only_its_paths() {
        let mut fixture = Fixture::new(&[Some(1), Some(1), Some(1)], 30);
        let before = fixture.ingress_of_site(0);
        let removed = fixture.sites[2].0;
        fixture.members.remove(&removed);
        fixture.sites.pop();
        let after = fixture.ingress_of_site(0);
        for (path, ingress) in &before {
            if *ingress != removed {
                assert!(after.contains(&(path.clone(), *ingress)), "{path} moved");
            }
        }
        assert_eq!(after.len(), before.len());
    }

    #[test]
    fn members_without_hints_are_remote() {
        let mut fixture = Fixture::new(&[Some(1), Some(1)], 4);
        let other = fixture.sites[1].0;
        fixture.hints.retain(NOW, |id| *id != other);
        let input = fixture.input(0);
        let plan = input.plan();
        assert!(plan.site.is_empty());
        assert!(plan.ingress.is_empty());
        assert!(
            !input.dials_forwarder(&plan, &other),
            "no hint, no forwarder session"
        );
    }

    #[test]
    fn forwarder_sessions_go_to_remote_members_only() {
        let fixture = Fixture::new(&[Some(1), Some(1), Some(2), None], 4);
        let input = fixture.input(0);
        let plan = input.plan();
        let [a, b, c, d] = [0, 1, 2, 3].map(|n| fixture.sites[n].0);
        assert!(!input.dials_forwarder(&plan, &a), "itself");
        assert!(!input.dials_forwarder(&plan, &b), "its own site");
        assert!(input.dials_forwarder(&plan, &c), "another site");
        assert!(input.dials_forwarder(&plan, &d), "a member without a site");
        let lone = fixture.input(2);
        assert!(
            !lone.dials_forwarder(&lone.plan(), &a),
            "a lone member dials no forwarder session"
        );
        let siteless = fixture.input(3);
        assert!(!siteless.dials_forwarder(&siteless.plan(), &a));
    }
}
