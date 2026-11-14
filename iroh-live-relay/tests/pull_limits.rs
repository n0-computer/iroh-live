//! Pull limits: the relay caps its pulls in total and per client, and rate-limits new dials.
//!
//! Every ticket names an endpoint nobody runs. The limits are checked before
//! the dial, and each test starts its pulls without yielding, so no dial
//! fails and retires a pull while a test counts them.

use std::time::Duration;

use iroh::SecretKey;
use iroh_live::{BroadcastTicket, layout::LiveLayout};
use iroh_live_node::test_util::endpoint;
use iroh_live_relay::{
    Error,
    pull::{Client, Limits, PullGuard, Pulls},
};
use moq_relay::cluster::Cluster;
use n0_future::task::AbortOnDropHandle;

/// Returns a cluster with a running driver, and nothing else.
fn cluster() -> (Cluster, AbortOnDropHandle<()>) {
    let (cluster, driver) =
        Cluster::new(moq_relay::cluster::Options::new(Default::default())).expect("init cluster");
    let task = tokio::spawn(async move {
        driver.run().await.ok();
    });
    (cluster, AbortOnDropHandle::new(task))
}

/// Returns a ticket to the `cam` broadcast of an endpoint nobody runs.
fn ticket() -> BroadcastTicket {
    LiveLayout::default()
        .ticket(SecretKey::generate().public(), "cam")
        .expect("a ticket")
}

/// Returns the browser client at `127.0.0.<last>`.
fn browser(last: u8) -> Client {
    Client::from_addr(Some(([127, 0, 0, last], 4443).into()))
}

/// Starts the pull of `ticket` for `client`.
fn start(pulls: &Pulls, client: Client, ticket: &BroadcastTicket) -> Result<PullGuard, Error> {
    pulls.start(client, &ticket.to_string(), ticket)
}

#[tokio::test]
async fn the_relay_holds_at_most_its_total_of_pulls() {
    let (cluster, _driver) = cluster();
    let mut limits = Limits::default();
    limits.total = 2;
    let pulls = Pulls::new(endpoint().await, cluster).with_limits(limits);
    let (first, second, third) = (ticket(), ticket(), ticket());

    let _first = start(&pulls, browser(1), &first).expect("room for the first");
    let _second = start(&pulls, browser(2), &second).expect("room for the second");
    let err = start(&pulls, browser(3), &third).expect_err("the relay is full");
    assert!(matches!(err, Error::PullLimit { limit: 2, .. }), "{err:#}");
    // Joining a pull dials nothing, so the cap does not apply.
    let _joined = start(&pulls, browser(3), &first).expect("joins the first pull");
}

#[tokio::test]
async fn a_client_holds_at_most_its_share_of_pulls() {
    let (cluster, _driver) = cluster();
    let mut limits = Limits::default();
    limits.per_client = 2;
    let pulls = Pulls::new(endpoint().await, cluster).with_limits(limits);
    let (first, second, third) = (ticket(), ticket(), ticket());
    let client = browser(1);

    let first_guard = start(&pulls, client, &first).expect("room for the first");
    let _second = start(&pulls, client, &second).expect("room for the second");
    let err = start(&pulls, client, &third).expect_err("the client holds its share");
    assert!(
        matches!(err, Error::ClientPullLimit { limit: 2, .. }),
        "{err:#}"
    );
    // A join holds a pull too.
    let err = start(&pulls, client, &first).expect_err("a join counts");
    assert!(matches!(err, Error::ClientPullLimit { .. }), "{err:#}");
    // Another client has a share of its own.
    let _other = start(&pulls, browser(2), &third).expect("another client");
    // Releasing a hold frees the client's share.
    drop(first_guard);
    let _third = start(&pulls, client, &third).expect("a share was released");
}

#[tokio::test]
async fn a_client_starts_new_dials_at_a_limited_rate() {
    let (cluster, _driver) = cluster();
    let mut limits = Limits::default();
    limits.dial_burst = 2;
    limits.dial_interval = Duration::from_secs(3600);
    let pulls = Pulls::new(endpoint().await, cluster).with_limits(limits);
    let (first, second, third) = (ticket(), ticket(), ticket());
    let client = Client::Peer(SecretKey::generate().public());

    let first_guard = start(&pulls, client, &first).expect("a dial left");
    let second_guard = start(&pulls, client, &second).expect("a dial left");
    let err = start(&pulls, client, &third).expect_err("the burst is spent");
    assert!(matches!(err, Error::DialRateLimit { .. }), "{err:#}");
    // Releasing a hold returns no dial: the rate counts dials, not holds.
    drop(second_guard);
    let err = start(&pulls, client, &third).expect_err("still no dial");
    assert!(matches!(err, Error::DialRateLimit { .. }), "{err:#}");
    // Joining a pull that is already dialing needs no dial of its own.
    let _joined = start(&pulls, client, &first).expect("joins without a dial");
    // Another client has its own burst.
    let _other = start(&pulls, browser(1), &third).expect("another client");
    drop(first_guard);
}
