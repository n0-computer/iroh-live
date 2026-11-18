//! Measures what a group with forwarding sends across each site's egress.
//!
//! Two sites sit behind their own home NAT: L with members A, B and C, and M
//! with D and E. An iroh relay on the internet exchange connects the sites
//! and tells each endpoint its public address, which gives its site. Each
//! member publishes 500 kbit/s and reads every other member. With forwarding
//! on, each remote broadcast should cross a site's WAN link once, and each
//! local one once per remote site. A run with forwarding off is the control:
//! every reader pulls its own copy.
//!
//! Linux only, built on unprivileged user namespaces. Run with
//! `cargo make test-patchbay`, which includes the ignored tests.

#![cfg(target_os = "linux")]

use std::{
    collections::BTreeSet,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use iroh::{
    Endpoint, EndpointId, RelayMode, RelayUrl, address_lookup::MemoryLookup, endpoint::presets,
    protocol::Router,
};
use iroh_live::{
    Grant, Group, GroupOptions, Node, NodeConfig, Reach, Subscription,
    moq_net::{self, Path, Timescale, Timestamp, track},
    node::policy::Open,
};
use iroh_relay::{
    RelayConfig, RelayMap, RelayQuicConfig,
    server::{
        CertConfig, QuicConfig, RelayConfig as RelayServerConfig, Server, ServerConfig, TlsConfig,
        testing::self_signed_tls_certs_and_config,
    },
    tls::CaTlsConfig,
};
use n0_future::task::AbortOnDropHandle;
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;
use patchbay::{Lab, Nat, event::parse_proc_net_dev};
use tracing::info;

/// Sets up the user namespace the lab needs.
///
/// Unshare needs a single-threaded process. The test harness starts threads
/// before any test runs, so this runs from `.init_array`.
#[ctor::ctor(unsafe)]
fn patchbay_init() {
    // SAFETY: runs from `.init_array`, single-threaded, before `main`.
    unsafe { patchbay::init_userns_for_ctor() };
}

/// The group's root.
const ROOT: &str = "party";

/// The bytes per second each member publishes: 500 kbit/s.
const RATE: u64 = 500_000 / 8;

/// How many frames each member writes per second, in one group per second.
const FRAMES_PER_SECOND: u64 = 25;

/// How long any one wait may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How long the traffic runs before the measurement starts.
const WARMUP: Duration = Duration::from_secs(5);

/// How long the measurement runs.
const WINDOW: Duration = Duration::from_secs(10);

/// How much above the expected copies the forwarding run may send, for headers and control traffic.
const SLACK: f64 = 1.3;

/// How many members each site has: L first, then M.
const SITES: [usize; 2] = [3, 2];

/// The bytes a WAN interface moved during the window, in each direction.
#[derive(Debug, Clone, Copy)]
struct Wan {
    up: u64,
    down: u64,
}

impl Wan {
    /// Returns the copies of one broadcast the bytes amount to, up and down.
    fn copies(self) -> (f64, f64) {
        let copy = (RATE * WINDOW.as_secs()) as f64;
        (self.up as f64 / copy, self.down as f64 / copy)
    }
}

/// One member: its node, group, publication writer and readers.
struct Member {
    node: Node,
    group: Group,
    _router: Router,
    _writer: AbortOnDropHandle<()>,
    readers: Vec<(EndpointId, Arc<AtomicU64>, AbortOnDropHandle<()>)>,
}

/// Starts an iroh relay with QUIC address discovery, bound to every address of the device.
async fn relay_server(ip: Ipv4Addr) -> (Server, RelayMap) {
    let (_certs, server_config) = self_signed_tls_certs_and_config();
    let tls = TlsConfig::new(
        (Ipv4Addr::UNSPECIFIED, 0),
        CertConfig::Manual { server_config },
    );
    let mut relay = RelayServerConfig::new((Ipv4Addr::UNSPECIFIED, 0));
    relay.tls = Some(tls);
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.quic = Some(QuicConfig::new((Ipv4Addr::UNSPECIFIED, 0)));
    let server = Server::spawn(config).await.expect("relay server");
    let port = server.https_addr().expect("https").port();
    let url: RelayUrl = format!("https://{}", SocketAddr::from((ip, port)))
        .parse()
        .expect("a relay url");
    let quic = server
        .quic_addr()
        .map(|addr| RelayQuicConfig::new(addr.port()));
    (server, RelayConfig::new(url, quic).into())
}

/// Binds an endpoint on the relay `relays`, which it trusts without checking certificates.
async fn bind(relays: RelayMap, lookup: MemoryLookup) -> Endpoint {
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Custom(relays))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .address_lookup(lookup)
        .bind()
        .await
        .expect("bind");
    tokio::time::timeout(TIMEOUT, endpoint.online())
        .await
        .expect("the endpoint came online");
    endpoint
}

/// Returns the path member `id` publishes at.
fn cam(id: EndpointId) -> Path {
    Path::new(&format!("{ROOT}/{id}/cam")).expect("a path")
}

/// Writes [`RATE`] bytes per second into a track of `broadcast`, in one group per second.
fn write(broadcast: &moq_net::broadcast::Producer) -> AbortOnDropHandle<()> {
    let info = track::Info::default().with_timescale(Timescale::MILLI);
    let track = broadcast.create_track("data", info).expect("a track");
    let frame = vec![0u8; (RATE / FRAMES_PER_SECOND) as usize];
    AbortOnDropHandle::new(tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(1000 / FRAMES_PER_SECOND));
        for second in 0u64.. {
            let start = Timestamp::from_millis(second * 1000).expect("a time");
            let Ok(mut group) = track.append_group(start) else {
                return;
            };
            for n in 0..FRAMES_PER_SECOND {
                tick.tick().await;
                let at = Timestamp::from_millis(second * 1000 + n * 1000 / FRAMES_PER_SECOND)
                    .expect("a time");
                if group.write_frame(at, frame.clone()).is_err() {
                    return;
                }
            }
            group.finish().ok();
        }
    }))
}

/// Counts the bytes that arrive on `subscription`, following it to whatever serves it.
fn read(subscription: Subscription, bytes: Arc<AtomicU64>) -> AbortOnDropHandle<()> {
    AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            let broadcast = subscription.broadcast();
            let instance = broadcast.info().instance.clone();
            let Ok(track) = broadcast.track("data") else {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            let Ok(mut subscriber) = track.subscribe(track::Subscription::default()).await else {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            // Reads until the track ends or another instance takes the path over.
            let moved = async {
                while subscription.broadcast().info().instance == instance {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            };
            let reading = async {
                while let Ok(Some(mut group)) = subscriber.recv_group().await {
                    while let Ok(Some(frame)) = group.read_frame().await {
                        bytes.fetch_add(frame.payload.len() as u64, Ordering::Relaxed);
                    }
                }
            };
            tokio::select! {
                () = moved => {}
                () = reading => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }))
}

/// Subscribes `node` to `peer`'s broadcast, dialing again if a dial fails.
///
/// A dial across two NATs fails now and then in the lab. The group retries
/// its own dials, so this only steadies the control run, which dials once.
async fn subscribe(node: &Node, peer: EndpointId) -> Subscription {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let subscribed =
            tokio::time::timeout(TIMEOUT, node.subscribe(cam(peer), Reach::Direct(peer)))
                .await
                .expect("subscribed in time");
        match subscribed {
            Ok(subscription) => return subscription,
            Err(err) if attempts < 3 => info!(error = %err, attempts, "subscribe failed, retrying"),
            Err(err) => panic!("subscribe to {}: {err:#}", peer.fmt_short()),
        }
    }
}

/// Returns the bytes the router's WAN interface has received and sent so far.
fn wan_counters(router: &patchbay::Router) -> Wan {
    let wan = router.interfaces()[0].name().to_owned();
    let counters = router
        .run_sync(move || {
            let text = std::fs::read_to_string("/proc/net/dev")?;
            Ok(parse_proc_net_dev(&text))
        })
        .expect("read the router's counters");
    let wan = counters
        .into_iter()
        .find(|counters| counters.iface == wan)
        .expect("the WAN interface");
    Wan {
        up: wan.tx_bytes,
        down: wan.rx_bytes,
    }
}

/// Builds the two sites, runs the group, and returns what crossed the WAN links of L and M.
async fn measure(forwarding: bool) -> [Wan; 2] {
    let lab = Lab::new().await.expect("a lab");
    let ix = lab.add_router("ix").build().await.expect("the ix router");
    let relay_device = lab
        .add_device("relay")
        .uplink(ix.id())
        .build()
        .await
        .expect("the relay device");
    let relay_ip = relay_device.ip().expect("the relay's address");
    let (relay, relays) = relay_device
        .spawn(move |_| relay_server(relay_ip))
        .expect("spawn the relay")
        .await
        .expect("the relay task");

    let mut routers = Vec::new();
    let lookup = MemoryLookup::new();
    let mut endpoints = Vec::new();
    for (site, size) in SITES.into_iter().enumerate() {
        let router = lab
            .add_router(&format!("site{site}"))
            .nat(Nat::Moderate)
            .build()
            .await
            .expect("a site router");
        for n in 0..size {
            let device = lab
                .add_device(&format!("site{site}-{n}"))
                .uplink(router.id())
                .build()
                .await
                .expect("a device");
            let (relays, lookup) = (relays.clone(), lookup.clone());
            let endpoint = device
                .spawn(move |_| bind(relays, lookup))
                .expect("spawn the bind")
                .await
                .expect("the bind task");
            endpoints.push((site, endpoint));
        }
        routers.push(router);
    }
    for (_, endpoint) in &endpoints {
        lookup.add_endpoint_info(endpoint.addr());
    }
    let ids: Vec<EndpointId> = endpoints
        .iter()
        .map(|(_, endpoint)| endpoint.id())
        .collect();

    let mut members = Vec::new();
    for (_, endpoint) in &endpoints {
        let node = Node::new(
            endpoint.clone(),
            NodeConfig::default().with_policy(Open(Grant::everything())),
        );
        let router = node.mount(Router::builder(endpoint.clone())).spawn();
        let group = node
            .group(ROOT, GroupOptions::default().with_forwarding(forwarding))
            .expect("a group");
        group.set_members(ids.iter().copied());
        let publication = node
            .publish(cam(node.id()), group.audience())
            .expect("publish");
        let writer = write(publication.broadcast());
        members.push(Member {
            node,
            group,
            _router: router,
            _writer: writer,
            readers: Vec::new(),
        });
    }

    if forwarding {
        let sites: Vec<usize> = endpoints.iter().map(|(site, _)| *site).collect();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let settled = members.iter().enumerate().all(|(n, member)| {
                    let same_site = sites.iter().filter(|site| **site == sites[n]).count() - 1;
                    member.group.plan().get().site.len() == same_site
                });
                if settled {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("the plans settled");
        info!("plans settled");
    }

    for viewer in 0..members.len() {
        for publisher in (0..members.len()).filter(|&n| n != viewer) {
            let peer = ids[publisher];
            let subscription = subscribe(&members[viewer].node, peer).await;
            let bytes = Arc::new(AtomicU64::new(0));
            let reader = read(subscription, bytes.clone());
            members[viewer].readers.push((peer, bytes, reader));
        }
    }

    tokio::time::sleep(WARMUP).await;
    let before = [wan_counters(&routers[0]), wan_counters(&routers[1])];
    let received: Vec<Vec<u64>> = members
        .iter()
        .map(|member| {
            member
                .readers
                .iter()
                .map(|(_, bytes, _)| bytes.load(Ordering::Relaxed))
                .collect()
        })
        .collect();
    tokio::time::sleep(WINDOW).await;
    let after = [wan_counters(&routers[0]), wan_counters(&routers[1])];

    for (member, before) in members.iter().zip(received) {
        for ((peer, bytes, _), before) in member.readers.iter().zip(before) {
            let got = bytes.load(Ordering::Relaxed) - before;
            assert!(
                got > RATE * WINDOW.as_secs() / 2,
                "{} read {got} bytes from {} in the window",
                member.node.id().fmt_short(),
                peer.fmt_short()
            );
        }
    }
    let ingress: BTreeSet<Path> = members
        .iter()
        .flat_map(|member| member.group.plan().get().ingress)
        .collect();
    info!(forwarding, ingress = ingress.len(), "measured");

    for member in &members {
        member.node.shutdown().await;
    }
    drop(relay);
    [0, 1].map(|n| Wan {
        up: after[n].up - before[n].up,
        down: after[n].down - before[n].down,
    })
}

#[tokio::test]
#[traced_test]
#[ignore = "builds a network lab, run with cargo make test-patchbay"]
async fn forwarding_sends_each_broadcast_across_a_site_egress_once() {
    let [l, m] = measure(true).await;
    let [plain_l, plain_m] = measure(false).await;
    let (l_up, l_down) = l.copies();
    let (m_up, m_down) = m.copies();
    let (plain_l_up, plain_l_down) = plain_l.copies();
    let (plain_m_up, plain_m_down) = plain_m.copies();
    info!(
        l_up,
        l_down,
        m_up,
        m_down,
        plain_l_up,
        plain_l_down,
        plain_m_up,
        plain_m_down,
        "WAN copies of one broadcast, forwarding on and off"
    );
    println!(
        "copies on:  L up {l_up:.2} down {l_down:.2}, M up {m_up:.2} down {m_down:.2}\n\
         copies off: L up {plain_l_up:.2} down {plain_l_down:.2}, M up {plain_m_up:.2} down {plain_m_down:.2}"
    );

    // Without forwarding, each of the six cross-site pairs carries its own copy each way.
    for (what, copies) in [
        ("L up", plain_l_up),
        ("L down", plain_l_down),
        ("M up", plain_m_up),
        ("M down", plain_m_down),
    ] {
        assert!(
            copies > 6.0 * 0.9,
            "control {what}: {copies:.2} copies, expected 6"
        );
    }
    // With forwarding, L sends A, B and C once and receives D and E once.
    for (what, copies, expected) in [
        ("L up", l_up, 3.0),
        ("L down", l_down, 2.0),
        ("M up", m_up, 2.0),
        ("M down", m_down, 3.0),
    ] {
        assert!(
            copies < expected * SLACK,
            "{what}: {copies:.2} copies, expected about {expected}"
        );
        assert!(
            copies > expected * 0.9,
            "{what}: {copies:.2} copies, expected about {expected}"
        );
    }
}
