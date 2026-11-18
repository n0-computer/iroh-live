# Groups and per-site forwarding

Groups are experimental. The API and the wire format of the hints may change
without notice.

A group is a set of peers that share a path root. `Node::group(root,
options)` creates it on one node, and the application hands it the member
list with `Group::set_members`. Members may publish and subscribe anywhere
under the root, and a publication there uses `Group::audience`, which admits
exactly the members. The node refuses a root that overlaps another group's,
and a publication under the root with any other audience.

## Forwarding

With `GroupOptions::with_forwarding(true)`, members that share a network
egress also share its traffic. Without it, a site of three members that reads
a remote broadcast pulls three copies across its uplink. With it, the site
pulls one, and the member that pulled it serves the others over the LAN.

The group module in `iroh-live-node/src/group/` does this in four parts:

- **Hints** (`hint.rs`). Each member signs a small hint with its endpoint
  key and publishes it at `<root>/.hint/<member id>`, with a `Peers` audience
  over the member list. The hint names the member's site and expires after
  two minutes. The member renews it every 30 seconds.
- **Sites** (`site.rs`). A member's site is a hash of the smallest public
  IPv4 address its endpoint knows, salted with the group id. QUIC address
  discovery reports the NAT's public address, so every member behind one NAT
  lands on one site. A node without a public IPv4 address has no site and
  forwards nothing.
- **The plan** (`plan.rs`). For each path under the root, the members of a
  site pick one ingress by rendezvous hashing over the members whose hint
  names the site. Every member computes the same answer, so they agree
  without talking. `Group::plan` shows this node's part of it.
- **The driver** (`driver.rs`). One task per group publishes the hint, reads
  the other members' hints on plain sessions, follows the paths under the
  root, and holds a forwarder session with each remote member.

## Sessions and routes

A member dials each remote member a second time, with `role=forwarder` in
the SETUP path. The remote member offers its group publications on that
session only, and withholds them from its plain sessions with the dialer. A
`Peers` publication never goes on a forwarder session. The dialer lands the
session's routes tagged `Link::Local` at priority -10, so it may serve them
to its own site.

The ingress for a path also offers it on its plain sessions with members
whose hint names its site. A plain session's routes rank at priority 0, above
the -10 of each member's own forwarder route, so the site reads through the
ingress. When the ingress goes away, each member's own route takes over. A
same-site publisher's broadcast arrives on a plain LAN session, and its lower
cost beats any forwarded copy.

An old node never sends the role and publishes no hint. Other members treat
it as remote without a forwarder session, and read it directly.

## Gaps

The first version leaves out layouts, forward classes, application-carried
hints, a planned `via` with a wait in `Node::subscribe`, short idle timeouts
on forwarder sessions, and closing sessions the plan no longer needs. Each is
a `TODO(groups)` comment at the place it belongs. `Subscription::serving_link`
returns `None` for a route a forwarder session landed, since moq reports it
as local.
