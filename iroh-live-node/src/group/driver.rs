//! The task that runs a group with forwarding on.
//!
//! It publishes this node's hint, reads the other members' hints, follows
//! the paths under the root and the node's site, and turns all of it into the
//! group's plan, which the node's grants read. It also holds the forwarder
//! sessions the plan asks for.

// TODO(groups): metrics: `ingress_roles`, `forwarded_readers` and
// `hints_rejected{reason}`.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
    sync::{Arc, Weak},
    time::Duration,
};

use data_encoding::BASE64URL_NOPAD;
use iroh::EndpointId;
use moq_ext_access::GroupId;
use moq_ext_json::ValueWriter;
use moq_net::{Path, Pattern, Patterns, announce, track};
use n0_error::{Result, StdResultExt, anyerr};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tokio::sync::mpsc;
use tracing::{Instrument, debug, debug_span, info, warn};

use super::{
    GroupPlan, HINT_SEGMENT,
    hint::{self, Hint, Hints},
    plan::PlanInput,
    site::{self, SiteId},
};
use crate::{Audience, ConnectOptions, UnpublishOnDrop, json, node::Shared, publish};

/// How often this node signs its hint anew.
///
/// Well inside [`hint::HINT_LIFETIME`], so a hint never expires while its
/// member lives.
const RENEW_INTERVAL: Duration = Duration::from_secs(30);

/// The name of the value track a member's hint travels in.
const HINT_TRACK: &str = "hint";

/// How many hints may wait for the driver.
///
/// A member sends one every [`RENEW_INTERVAL`], so this absorbs a burst of
/// members joining at once.
const HINT_QUEUE: usize = 64;

/// The first and the longest pause before a hint reader tries a member again.
const RETRY: (Duration, Duration) = (Duration::from_secs(1), Duration::from_secs(30));

/// A group's forwarding, before it runs.
pub(super) struct Driver {
    shared: Weak<Shared>,
    id: GroupId,
    root: Path,
    members: Watchable<BTreeSet<EndpointId>>,
    plan: Watchable<GroupPlan>,
}

/// What the driver knows while it runs.
struct Running {
    shared: Arc<Shared>,
    id: GroupId,
    root: Path,
    own: EndpointId,
    plan_out: Watchable<GroupPlan>,
    members: BTreeSet<EndpointId>,
    hints: Hints,
    site: Option<SiteId>,
    paths: BTreeSet<Path>,
    /// Reads each other member's hint, by member.
    readers: BTreeMap<EndpointId, AbortOnDropHandle<()>>,
    /// The hint readers' channel to the driver.
    hint_tx: mpsc::Sender<(EndpointId, String)>,
    /// This node's hint publication, unpublished when the driver ends.
    hint_writer: ValueWriter<String>,
    _hint_publication: UnpublishOnDrop,
}

impl Driver {
    /// Creates the driver of group `id` at `root`, which writes its plan to `plan`.
    pub(super) fn new(
        shared: &Arc<Shared>,
        id: GroupId,
        root: Path,
        members: &Watchable<BTreeSet<EndpointId>>,
        plan: Watchable<GroupPlan>,
    ) -> Self {
        Self {
            shared: Arc::downgrade(shared),
            id,
            root,
            members: members.clone(),
            plan,
        }
    }

    /// Runs the group's forwarding until the node shuts down or the group ends.
    pub(super) async fn run(self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let shutdown = shared.shutdown.clone();
        let mut members = self.members.watch();
        let mut public = site::watch_public_v4(&shared.endpoint);
        let mut paths = match cursor(&shared, &self.root) {
            Ok(paths) => paths,
            Err(err) => {
                warn!(error = %err, "cannot follow the group's paths, forwarding stays off");
                return;
            }
        };
        let (hint_tx, mut hint_rx) = mpsc::channel(HINT_QUEUE);
        let (hint_publication, hint_writer) = match publish_hint(&shared, &self.root, &self.members)
        {
            Ok(published) => published,
            Err(err) => {
                warn!(error = %err, "cannot publish this node's hint, forwarding stays off");
                return;
            }
        };
        let own = shared.id;
        let mut running = Running {
            shared,
            id: self.id,
            root: self.root,
            own,
            plan_out: self.plan,
            members: BTreeSet::new(),
            hints: Hints::default(),
            site: None,
            paths: BTreeSet::new(),
            readers: BTreeMap::new(),
            hint_tx,
            hint_writer,
            _hint_publication: hint_publication,
        };
        info!("group forwarding started");
        running.set_site(public.get());
        running.set_members(members.get());
        let mut renew = tokio::time::interval(RENEW_INTERVAL);
        renew.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let expiry = running.next_expiry();
            tokio::select! {
                () = shutdown.cancelled() => break,
                updated = members.updated() => match updated {
                    Ok(updated) => running.set_members(updated),
                    Err(_) => break,
                },
                updated = public.updated() => match updated {
                    Ok(updated) => running.set_site(updated),
                    Err(_) => break,
                },
                Some((member, encoded)) = hint_rx.recv() => running.received(member, &encoded),
                event = paths.next() => match event {
                    Some(event) => running.path_event(event),
                    None => break,
                },
                _ = renew.tick() => running.sign_hint(),
                () = tokio::time::sleep(expiry) => running.expire(),
            }
        }
        info!("group forwarding stopped");
    }
}

impl Running {
    /// Applies a new public address of this node.
    fn set_site(&mut self, public: Option<Ipv4Addr>) {
        let site = public.map(|addr| SiteId::new(self.id, addr));
        if site == self.site {
            return;
        }
        info!(?site, has_public_v4 = public.is_some(), "site changed");
        self.site = site;
        self.sign_hint();
        self.replan();
    }

    /// Applies a new member list, and reads the hint of every other member.
    fn set_members(&mut self, members: BTreeSet<EndpointId>) {
        self.readers.retain(|member, _| members.contains(member));
        for member in &members {
            if *member == self.own || self.readers.contains_key(member) {
                continue;
            }
            let reader = read_hints(
                Arc::downgrade(&self.shared),
                *member,
                hint_path(&self.root, member),
                self.hint_tx.clone(),
            )
            .instrument(debug_span!("hints", member = %member.fmt_short()));
            self.readers
                .insert(*member, AbortOnDropHandle::new(tokio::spawn(reader)));
        }
        debug!(members = members.len(), "member list changed");
        self.hints
            .retain(hint::now_ms(), |member| members.contains(member));
        self.members = members;
        self.replan();
    }

    /// Signs this node's hint anew and writes it to the hint track.
    // TODO(groups): the hint over the application's channel instead, and the
    // forwarder's `ingress` sample for hop-aware adaptation.
    fn sign_hint(&mut self) {
        let hint = Hint::new(self.id, self.site, hint::now_ms());
        let encoded = BASE64URL_NOPAD.encode(&hint.sign(self.shared.endpoint.secret_key()));
        if let Err(err) = self.hint_writer.update(&encoded) {
            warn!(error = %err, "failed to write this node's hint");
        }
    }

    /// Verifies a hint that a reader of `member` received, and keeps it.
    fn received(&mut self, member: EndpointId, encoded: &str) {
        if !self.members.contains(&member) {
            return;
        }
        let verified = BASE64URL_NOPAD
            .decode(encoded.as_bytes())
            .map_err(|_| hint::HintError::Malformed)
            .and_then(|bytes| hint::verify(&bytes, self.id, &member, hint::now_ms()));
        match verified {
            Ok(hint) => {
                if self.hints.offer(member, hint) {
                    debug!(member = %member.fmt_short(), "hint accepted");
                    self.replan();
                }
            }
            Err(reason) => {
                debug!(member = %member.fmt_short(), %reason, "hint rejected");
            }
        }
    }

    /// Applies a change of the paths under the root.
    fn path_event(&mut self, event: announce::Event) {
        let (path, present) = match event {
            announce::Event::Present(announce)
            | announce::Event::Available(announce)
            | announce::Event::Unavailable(announce) => (announce.prefix, true),
            announce::Event::Absent(announce) | announce::Event::Offline(announce) => {
                (announce.prefix, false)
            }
        };
        if is_hint_path(&self.root, &path) {
            return;
        }
        let changed = match present {
            true => self.paths.insert(path),
            false => self.paths.remove(&path),
        };
        if changed {
            self.replan();
        }
    }

    /// Drops the hints that expired.
    fn expire(&mut self) {
        let members = &self.members;
        if self
            .hints
            .retain(hint::now_ms(), |member| members.contains(member))
        {
            debug!("a member's hint expired");
            self.replan();
        }
    }

    /// Returns how long until the first kept hint expires.
    fn next_expiry(&self) -> Duration {
        match self.hints.next_expiry() {
            Some(expires) => Duration::from_millis(expires.saturating_sub(hint::now_ms())),
            None => RENEW_INTERVAL,
        }
    }

    /// Computes the plan, and hands it to the node's grants and the plan's watchers.
    fn replan(&mut self) {
        let input = PlanInput {
            own: self.own,
            site: self.site,
            members: &self.members,
            hints: &self.hints,
            paths: &self.paths,
        };
        let plan = input.plan();
        self.shared.state.lock().expect("poisoned").set_group_view(
            self.id,
            self.hints.clone(),
            self.site,
            plan.ingress.clone(),
        );
        if let Ok(previous) = self.plan_out.set(plan.clone()) {
            debug!(
                site = ?plan.site.iter().map(|id| id.fmt_short().to_string()).collect::<Vec<_>>(),
                ingress = ?plan.ingress.iter().map(Path::as_str).collect::<Vec<_>>(),
                previous_ingress = previous.ingress.len(),
                paths = self.paths.len(),
                hints = self.hints.signers().count(),
                "plan changed",
            );
        }
    }
}

/// Returns the path of `member`'s hint broadcast in the group at `root`.
fn hint_path(root: &Path, member: &EndpointId) -> Path {
    root.join(&Path::new(&format!("{HINT_SEGMENT}/{member}")).expect("a valid path"))
        .expect("a hint path is two segments below a valid root")
}

/// Reports whether `path` lies in the hint subtree of the group at `root`.
pub(crate) fn is_hint_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.parts().next() == Some(HINT_SEGMENT))
}

/// Returns a listing of the paths under `root` in the node's route table.
///
/// On a forwarder session, which moq treats as a client on a local link, the
/// listing is also what makes the session ask its peer about the root.
fn cursor(shared: &Shared, root: &Path) -> Result<announce::Consumer> {
    let pattern: Pattern = format!("{root}/**").parse().std_context("a root pattern")?;
    let scoped = shared
        .table
        .consume()
        .scope("", &Patterns::from(pattern))
        .std_context("scoping the route table to the root")?;
    Ok(scoped.announced())
}

/// Publishes this node's hint track, for the members of the group at `root`.
///
/// A `Peers` audience never reaches a forwarder, so a hint never transits.
fn publish_hint(
    shared: &Arc<Shared>,
    root: &Path,
    members: &Watchable<BTreeSet<EndpointId>>,
) -> Result<(UnpublishOnDrop, ValueWriter<String>)> {
    let path = hint_path(root, &shared.id);
    let (publication, writer) = publish::publish(
        shared,
        path,
        Audience::Peers(members.clone()),
        |broadcast| {
            let info = track::Info::default().with_timescale(moq_net::Timescale::MILLI);
            let track = broadcast.create_track(HINT_TRACK, info)?;
            Ok::<_, moq_net::Error>(ValueWriter::new(track, json::snapshot::Config::default()))
        },
    )?;
    Ok((publication.unpublish_on_drop(), writer))
}

/// Reads `member`'s hints on a plain session and hands them to the driver, retrying until aborted.
// TODO(groups): close the bootstrap plain session once both forwarder
// sessions exist and no `Peers` publication needs it.
async fn read_hints(
    shared: Weak<Shared>,
    member: EndpointId,
    path: Path,
    hints: mpsc::Sender<(EndpointId, String)>,
) {
    let mut pause = RETRY.0;
    loop {
        match read_hints_once(&shared, member, &path, &hints).await {
            Ok(()) => pause = RETRY.0,
            Err(err) => debug!(error = %err, ?pause, "cannot read the member's hint, retrying"),
        }
        if hints.is_closed() || shared.strong_count() == 0 {
            return;
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(RETRY.1);
    }
}

/// Reads `member`'s hints until its hint track or its session ends.
async fn read_hints_once(
    shared: &Weak<Shared>,
    member: EndpointId,
    path: &Path,
    hints: &mpsc::Sender<(EndpointId, String)>,
) -> Result<()> {
    let connect = {
        let shared = shared
            .upgrade()
            .ok_or_else(|| anyerr!("the node is gone"))?;
        shared.connect(member.into(), ConnectOptions::default())
    };
    let session = connect.await?;
    let subscription = session.subscribe(path.clone()).await?;
    let mut reader = subscription.value_track::<String>(HINT_TRACK)?;
    debug!(link = %session.inner.link, "reading the member's hint");
    while let Some(value) = reader
        .latest()
        .await
        .std_context("reading the hint track")?
    {
        if hints.send((member, value.value)).await.is_err() {
            return Ok(());
        }
    }
    Ok(())
}
