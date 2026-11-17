//! Sessions from iroh clients, decided by a policy on their endpoint id.
//!
//! moq-tokio's server has no iroh backend, and an iroh session's auth request
//! would carry no peer identity anyway. So the relay accepts iroh sessions
//! itself with [`IrohSessions`], and asks a [`Policy`] about each. On an open
//! relay, [`relay_policy`] lets each client publish only at its own paths, the
//! paths that name it: `live/<its id>/...` and `rooms/<topic>/<its id>/...`.
//! Browsers come in through moq-relay's server, and [`browser_grant`] keeps
//! them to names of one segment below the root.

use std::{sync::Arc, time::Duration};

use iroh::{
    EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, RouterBuilder},
};
use iroh_live_node::{
    BroadcastTicket, DECIDE_TIMEOUT, Grant, SessionRequest,
    layout::{self, LiveLayout},
    policy::{self, Authorization, Closed, DynPolicy, Event, PeerRequest, Policy},
};
use iroh_live_rooms::RoomsLayout;
use moq_net::{Pattern, Patterns};
use moq_relay::cluster::Cluster;
use n0_error::e;
use n0_future::task::AbortOnDropHandle;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::pull::{Client, PullGuard, Pulls};

/// How long a closing session gets to tell its peer when the relay shuts down.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// How long a session the relay asked to reconnect keeps serving before the relay closes it.
///
/// The client dials its new session meanwhile, so its broadcasts move over
/// without a gap.
const DRAIN_DEADLINE: Duration = Duration::from_secs(5);

/// How often the relay checks its JWT key file for changes.
const KEY_CHECK: Duration = Duration::from_secs(1);

/// The protocol handler that accepts iroh clients' MoQ sessions into the cluster.
///
/// Mount it on a router with [`mount`](Self::mount).
#[derive(Clone, derive_more::Debug)]
pub struct IrohSessions {
    #[debug(skip)]
    cluster: Cluster,
    /// Pulls the broadcasts that sessions name by ticket, if enabled.
    pulls: Option<Pulls>,
    /// Decides who may hold a session, and what it may publish and read.
    policy: Arc<dyn DynPolicy<EndpointId>>,
    /// Decides who may pull without open pulls, asked at the ticket's path.
    pull_policy: Option<Arc<dyn DynPolicy<EndpointId>>>,
    /// The longest a session admitted with a token lasts.
    max_session: Option<Duration>,
    /// Changes when the JWT key file does, which decides the token sessions again.
    key_changes: Option<watch::Receiver<u64>>,
    /// Checks the key file for changes while any clone of the handler lives.
    #[debug(skip)]
    _key_watch: Option<Arc<AbortOnDropHandle<()>>>,
    /// Cancelled when the router shuts down, which closes every session.
    shutdown: CancellationToken,
}

impl IrohSessions {
    /// Creates the handler for `cluster`, which refuses every session until it gets a policy.
    ///
    /// Set the policies with [`with_policies`](Self::with_policies), as the
    /// relay does, or with [`with_policy`](Self::with_policy).
    ///
    /// With `pulls` set, a session that names a ticket in its path pulls the
    /// ticket's broadcast, as a browser's does, if the pulls let it: see
    /// [`with_pull_policy`](Self::with_pull_policy). A session that may not
    /// pull, or would exceed the pulls' [`Limits`](crate::pull::Limits), is
    /// refused.
    #[must_use]
    pub fn new(cluster: Cluster, pulls: Option<Pulls>) -> Self {
        Self {
            cluster,
            pulls,
            policy: Arc::new(Closed),
            pull_policy: None,
            max_session: None,
            key_changes: None,
            _key_watch: None,
            shutdown: CancellationToken::new(),
        }
    }

    /// Returns this handler with `policy` deciding sessions.
    ///
    /// The relay asks it after a client's MoQ SETUP. A rejection closes the
    /// session with the rejection's code. An authorization's grant bounds
    /// what the client publishes into the cluster and what it reads from it,
    /// and the session closes when the authorization expires. A client whose
    /// session closed reconnects.
    #[must_use]
    pub fn with_policy(mut self, policy: impl Policy<EndpointId>) -> Self {
        self.policy = Arc::new(policy);
        self
    }

    /// Returns this handler with `policy` deciding who may pull without open pulls.
    ///
    /// The relay asks it about the session's peer and token at the path of the
    /// ticket's broadcast, and the session may pull if the authorization lets
    /// it read that path. Without one, no session pulls unless the pulls are
    /// open. It should require a token, as a
    /// [`Verify`](moq_ext_access::policy::Verify) without a fallback does,
    /// since an open policy would let every session pull.
    #[must_use]
    pub fn with_pull_policy(mut self, policy: impl Policy<EndpointId>) -> Self {
        self.pull_policy = Some(Arc::new(policy));
        self
    }

    /// Returns this handler with the policies, the session bound and the key file of `policies`.
    ///
    /// A session that presented a token lasts at most
    /// [`max_session`](crate::Config::max_session). When the key file
    /// changes, which the handler checks once a second, every live session
    /// that presented a token is decided again, and closed if its grant
    /// changed or its token no longer verifies. Call it within a tokio
    /// runtime, which runs the check.
    #[doc(hidden)]
    #[must_use]
    pub fn with_policies(mut self, policies: &crate::Policies) -> Self {
        self.policy = policies.iroh.clone();
        self.pull_policy.clone_from(&policies.pull_tokens);
        self.max_session = Some(policies.max_session);
        if let Some(keys) = policies.key_file.clone() {
            self.key_changes = Some(keys.changes());
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => {
                    let watch = runtime.spawn(async move {
                        let mut interval = tokio::time::interval(KEY_CHECK);
                        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        loop {
                            interval.tick().await;
                            keys.refresh();
                        }
                    });
                    self._key_watch = Some(Arc::new(AbortOnDropHandle::new(watch)));
                }
                Err(_) => {
                    warn!("no tokio runtime: the key file is read again only when a token arrives")
                }
            }
        }
        self
    }

    /// Mounts this handler on `router` under every ALPN in [`moq_transport_iroh::alpns`].
    ///
    /// The caller may add its own protocols, and spawns the router.
    #[must_use]
    pub fn mount(self, mut router: RouterBuilder) -> RouterBuilder {
        for alpn in moq_transport_iroh::alpns() {
            router = router.accept(alpn, self.clone());
        }
        router
    }

    /// Runs one session until it closes or the relay shuts down.
    async fn serve(&self, connection: Connection) -> Result<(), AcceptError> {
        let Admitted {
            session,
            driver,
            authorization,
            bounded,
            recheck,
            pull,
        } = self.admit(connection).await?;
        // Held while the session runs.
        let _pull = pull.map(|(pull, name)| crate::hold_pull(pull, name));
        self.run(session, driver, &authorization, bounded, &recheck)
            .await;
        Ok(())
    }

    /// Decides an iroh client's session and completes its handshake.
    async fn admit(&self, connection: Connection) -> Result<Admitted, AcceptError> {
        let peer = connection.remote_id();
        let transport =
            moq_transport_iroh::Transport::new(connection).map_err(AcceptError::from_err)?;
        let handshake = tokio::time::timeout(
            moq_transport_iroh::HANDSHAKE_TIMEOUT,
            moq_net::Server::new().handshake(moq_net::time::Instant::now(), transport),
        )
        .await
        .map_err(|_| AcceptError::from_err(moq_net::Error::Timeout))?
        .map_err(AcceptError::from_err)?;
        let name = requested_name(handshake.path());
        // Logged without the query: a token rides it as `?jwt=`.
        let session = SessionRequest::from_handshake(&handshake);
        let path = session.path().to_owned();
        let token = session.token().map(str::to_owned);
        // A ticket the relay pulls for the session, read under `name`.
        let ticket = name
            .as_deref()
            .filter(|_| self.pulls.is_some())
            .and_then(|name| name.parse::<BroadcastTicket>().ok());
        let dialed = PeerRequest::new(peer, session.clone(), Event::Connect);
        let admitted = match &ticket {
            None => self.decide(&dialed).await.map(|authorization| {
                let recheck = (authorization.grant.clone(), dialed.clone());
                (authorization, false, vec![recheck])
            }),
            Some(ticket) => {
                self.decide_ticket(peer, &dialed, token.as_deref(), ticket)
                    .await
            }
        };
        let (mut authorization, reads_ticket, decided) = match admitted {
            Ok(admitted) => admitted,
            Err(rejection) => {
                info!(
                    reason = rejection.reason(),
                    code = %rejection.code(),
                    "the policy rejected the iroh client",
                );
                handshake.close(rejection.code().clone());
                return Err(AcceptError::from_err(rejection));
            }
        };
        // A session with a token is bounded, and decided again when the keys change.
        let mut recheck = Vec::new();
        let mut bounded = false;
        if token.is_some() {
            if let Some(bound) = self.max_session {
                let latest = std::time::SystemTime::now() + bound;
                bounded = authorization.expires.is_none_or(|expires| latest < expires);
                if bounded {
                    authorization.expires = Some(latest);
                }
            }
            recheck = decided
                .into_iter()
                .map(|(grant, request)| Decided {
                    request: PeerRequest::new(
                        *request.peer(),
                        request_of(&request),
                        Event::Reauthorize,
                    ),
                    grant,
                })
                .collect();
        }
        let pull = match &ticket {
            Some(ticket) => {
                self.start_pull(
                    peer,
                    token,
                    name.as_deref().unwrap_or_default(),
                    ticket,
                    reads_ticket,
                )
                .await
            }
            None => Ok(None),
        };
        let pull = match pull {
            Ok(pull) => pull,
            Err(err) => {
                info!(error = %err, "refusing the iroh client's pull");
                let code = match err {
                    crate::Error::PullDenied { .. } => moq_net::Error::Unauthorized,
                    _ => moq_net::Error::TooManyRequests,
                };
                handshake.close(code);
                return Err(AcceptError::from_err(err));
            }
        };
        let grant = &authorization.grant;
        // An empty half of the grant scopes to nothing, which moq refuses, so
        // the session then gets no origin for it.
        let publish = (!grant.publish.is_empty())
            .then(|| self.cluster.origin.scope("", &grant.publish))
            .transpose()
            .map_err(AcceptError::from_err)?;
        // A session that names a ticket reads the pulled broadcast at that
        // name, if the decision at the ticket's path lets it read there.
        let mut reads = grant.subscribe.clone();
        let source = match (&ticket, name.as_deref()) {
            (Some(_), Some(name)) => {
                if reads_ticket && let Ok(alias) = Pattern::literal(name) {
                    reads.extend([alias]);
                }
                crate::pull::mounted(&self.cluster, name)
            }
            _ => None,
        };
        let serving = (!reads.is_empty())
            .then(|| {
                source
                    .map_or_else(
                        || self.cluster.origin.consume(),
                        |cluster| cluster.origin.consume(),
                    )
                    .scope("", &reads)
            })
            .transpose()
            .map_err(AcceptError::from_err)?;
        let mut handshake = handshake;
        if let Some(serving) = serving {
            handshake = handshake.with_publisher(serving);
        }
        if let Some(publish) = publish {
            handshake = handshake.with_subscriber(publish);
        }
        let (session, driver) =
            tokio::time::timeout(moq_transport_iroh::HANDSHAKE_TIMEOUT, handshake.ok())
                .await
                .map_err(|_| AcceptError::from_err(moq_net::Error::Timeout))?
                .map_err(AcceptError::from_err)?;
        info!(%path, grant = ?authorization.grant, reads_ticket, expires = ?authorization.expires, "iroh client admitted");
        Ok(Admitted {
            session,
            driver,
            authorization,
            bounded,
            recheck,
            pull,
        })
    }

    /// Asks the session policy about `request` within the decide timeout.
    async fn decide(&self, request: &PeerRequest) -> policy::Decision {
        moq_ext_access::policy::decide_within(&*self.policy, request, DECIDE_TIMEOUT).await
    }

    /// Decides a session that names `ticket`, which the relay pulls for it.
    ///
    /// Two decisions. The session's own rights are decided at the path it
    /// dialed, as any session's are, and a rejection there only leaves it
    /// without them. Whether it reads the ticket's broadcast is decided at the
    /// ticket's path, which must admit the session, and only its subscribe
    /// half counts. Returns the session's authorization, whether it reads the
    /// ticket, and each decision that admitted it with its request.
    async fn decide_ticket(
        &self,
        peer: EndpointId,
        dialed: &PeerRequest,
        token: Option<&str>,
        ticket: &BroadcastTicket,
    ) -> Result<(Authorization, bool, Vec<(Grant, PeerRequest)>), policy::Rejection> {
        let at_ticket = PeerRequest::new(peer, ticket_request(ticket, token), Event::Connect);
        let mount = self.decide(&at_ticket).await?;
        let reads_ticket = mount.grant.allows_subscribe(ticket.path().as_str());
        let mut decided = vec![(mount.grant.clone(), at_ticket)];
        let own = match self.decide(dialed).await {
            Ok(own) => {
                decided.push((own.grant.clone(), dialed.clone()));
                own
            }
            Err(rejection) => {
                debug!(
                    reason = rejection.reason(),
                    "the ticket's session gets no rights at the dialed path"
                );
                Authorization::default()
            }
        };
        let expires = [mount.expires, own.expires].into_iter().flatten().min();
        Ok((
            Authorization {
                grant: own.grant,
                expires,
            },
            reads_ticket,
            decided,
        ))
    }

    /// Starts the pull of `ticket`, which the session named as `name`.
    ///
    /// Without [open pulls](Pulls::with_open_pulls) the pull policy decides,
    /// at the ticket's path with the session's `token`, and must grant
    /// reading that path. With them the session must read the ticket's path,
    /// which `reads_ticket` says.
    async fn start_pull(
        &self,
        peer: EndpointId,
        token: Option<String>,
        name: &str,
        ticket: &BroadcastTicket,
        reads_ticket: bool,
    ) -> Result<Option<(PullGuard, String)>, crate::Error> {
        let Some(pulls) = self.pulls.as_ref() else {
            return Ok(None);
        };
        let denied = || {
            e!(crate::Error::PullDenied {
                path: ticket.path().clone()
            })
        };
        if pulls.open_pulls() {
            if !reads_ticket {
                return Err(denied());
            }
        } else {
            let policy = self.pull_policy.as_deref().ok_or_else(denied)?;
            let request = PeerRequest::new(
                peer,
                ticket_request(ticket, token.as_deref()),
                Event::Connect,
            );
            let decision =
                moq_ext_access::policy::decide_within(policy, &request, DECIDE_TIMEOUT).await;
            if !decision.is_ok_and(|auth| auth.grant.allows_subscribe(ticket.path().as_str())) {
                return Err(denied());
            }
        }
        let pull = pulls.start(Client::Peer(peer), name, ticket)?;
        Ok(Some((pull, name.to_owned())))
    }

    /// Runs an admitted session until it ends, its authorization expires, or the relay shuts down.
    ///
    /// When the key file changes, the decisions in `recheck` are made again,
    /// and the session closes if one is rejected or its grant changed. A
    /// `bounded` session expires at the relay's `--max-session` bound before
    /// its token does, so the relay then asks the client to reconnect.
    async fn run(
        &self,
        session: moq_net::Session,
        driver: moq_net::Driver,
        authorization: &Authorization,
        bounded: bool,
        recheck: &[Decided],
    ) {
        let run = moq_net::time::run(driver);
        tokio::pin!(run);
        let expiry = expiry(authorization);
        tokio::pin!(expiry);
        let mut key_changes = self.key_changes.clone().filter(|_| !recheck.is_empty());
        let ending = loop {
            let changed = async {
                match key_changes.as_mut() {
                    Some(changes) => changes.changed().await.is_ok(),
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                err = &mut run => {
                    debug!(error = %err, "iroh session ended");
                    return;
                }
                () = self.shutdown.cancelled() => break Ending::Close(moq_net::Error::Cancel),
                () = &mut expiry => {
                    if bounded {
                        info!("the iroh client's session reached --max-session, asking it to reconnect");
                        break Ending::Reconnect;
                    }
                    info!("the iroh client's authorization expired, closing its session");
                    break Ending::Close(moq_net::Error::Expired);
                }
                live = changed => {
                    if !live {
                        key_changes = None;
                        continue;
                    }
                    if let Some(ending) = self.decide_again(recheck).await {
                        break ending;
                    }
                }
            }
        };
        if matches!(ending, Ending::Reconnect) {
            let goaway = moq_net::goaway::Goaway::new().with_timeout(Some(DRAIN_DEADLINE));
            match session.goaway(goaway) {
                // The driver closes the session at the deadline, if the client did not.
                Ok(()) => tokio::select! {
                    _ = tokio::time::timeout(DRAIN_DEADLINE + CLOSE_GRACE, &mut run) => {
                        debug!("the drained iroh session ended");
                        return;
                    }
                    () = self.shutdown.cancelled() => {}
                },
                Err(err) => debug!(error = %err, "could not send the GOAWAY"),
            }
        }
        let code = match ending {
            Ending::Close(code) => code,
            Ending::Reconnect => moq_net::Error::Cancel,
        };
        // The driver keeps running, so the session can tell its peer.
        session.abort(code);
        if tokio::time::timeout(CLOSE_GRACE, run).await.is_err() {
            warn!(grace = ?CLOSE_GRACE, "iroh session did not close in time");
        }
    }
}

impl IrohSessions {
    /// Makes the decisions in `recheck` again, and returns how to end the session if one changed.
    ///
    /// A rejection, or a grant that no longer covers the one the session got,
    /// closes it as unauthorized. A wider grant asks the client to reconnect,
    /// so its new session gets the new grant.
    async fn decide_again(&self, recheck: &[Decided]) -> Option<Ending> {
        for decided in recheck {
            let decision = moq_ext_access::policy::decide_within(
                &*self.policy,
                &decided.request,
                DECIDE_TIMEOUT,
            )
            .await;
            match decision {
                Err(rejection) => {
                    info!(
                        reason = rejection.reason(),
                        code = %rejection.code(),
                        "the key file changed and the token no longer verifies, closing the session",
                    );
                    return Some(Ending::Close(rejection.code().clone()));
                }
                Ok(authorization) if authorization.grant != decided.grant => {
                    let ending = ending_for_grant(&decided.grant, &authorization.grant);
                    info!(
                        grant = ?authorization.grant,
                        ?ending,
                        "the key file changed the token's grant, ending the session",
                    );
                    return Some(ending);
                }
                Ok(_) => debug!("the key file changed, and the token still verifies"),
            }
        }
        None
    }
}

/// Returns the request of a session that dialed `ticket`'s path, presenting `token`.
fn ticket_request(ticket: &BroadcastTicket, token: Option<&str>) -> SessionRequest {
    let request = SessionRequest::new(ticket.path().as_str());
    match token {
        Some(token) => request.with_token(token),
        None => request,
    }
}

/// Returns the session request a policy saw in `request`.
fn request_of(request: &PeerRequest) -> SessionRequest {
    let session = SessionRequest::new(request.path());
    match request.token() {
        Some(token) => session.with_token(token),
        None => session,
    }
}

/// How the relay ends a session that it no longer serves as it admitted it.
#[derive(Debug)]
enum Ending {
    /// Closes the session with this code.
    ///
    /// A client takes [`Expired`](moq_net::Error::Expired) and
    /// [`Unauthorized`](moq_net::Error::Unauthorized) as a refusal of its token.
    Close(moq_net::Error),
    /// Sends a GOAWAY, so the client dials a new session with the same token.
    ///
    /// The old session serves until the new one is up, or until
    /// [`DRAIN_DEADLINE`].
    Reconnect,
}

/// Returns how to end a session whose token's grant changed from `admitted` to `now`.
///
/// A grant that covers the admitted one asks the client to reconnect, any
/// other closes the session as unauthorized.
fn ending_for_grant(admitted: &Grant, now: &Grant) -> Ending {
    if now.publish.covers(&admitted.publish) && now.subscribe.covers(&admitted.subscribe) {
        Ending::Reconnect
    } else {
        Ending::Close(moq_net::Error::Unauthorized)
    }
}

/// A decision about a live session, as the relay makes it again when the key file changes.
struct Decided {
    request: PeerRequest,
    /// The grant the decision gave when the session was admitted.
    grant: Grant,
}

/// An iroh client's session after its handshake, before it runs.
struct Admitted {
    session: moq_net::Session,
    driver: moq_net::Driver,
    authorization: Authorization,
    /// Whether the session expires at the relay's max-session bound, before its token does.
    bounded: bool,
    /// The decisions to make again when the key file changes, empty without a token.
    recheck: Vec<Decided>,
    /// The pull of the ticket the session named, with that name.
    pull: Option<(PullGuard, String)>,
}

/// Waits until `authorization` expires, or forever without an expiry.
async fn expiry(authorization: &Authorization) {
    match authorization.expires {
        Some(expires) => {
            let left = expires
                .duration_since(std::time::SystemTime::now())
                .unwrap_or_default();
            tokio::time::sleep(left).await;
        }
        None => std::future::pending().await,
    }
}

impl ProtocolHandler for IrohSessions {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let span = info_span!("iroh_client", peer = %connection.remote_id().fmt_short());
        self.serve(connection).instrument(span).await
    }

    async fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

/// Returns the grant for a browser that dialed `path`: subscribe anywhere, publish one segment.
///
/// The rule is rooted at `/` and reduced to the dialed path, as moq-relay does
/// with its public rules. The grant itself is rooted at the dialed path, so
/// granting the unreduced rule would let a browser that dials `/live/<id>`
/// publish beneath it. One segment keeps a browser out of `live/` and
/// `rooms/`, whose paths name an iroh publisher.
///
/// # Errors
///
/// Returns an error if `path` is not a valid path.
pub fn browser_grant(path: &str) -> Result<moq_auth::Grant, moq_auth::Error> {
    let rules = browser_rules();
    let rule = moq_auth::Claims::default()
        .with_publish(rules.publish)
        .with_subscribe(rules.subscribe);
    let permissions = rule.authorize(path)?;
    Ok(moq_auth::Grant::new(
        permissions.publish,
        permissions.subscribe,
    ))
}

/// Returns what a browser may do without a token, rooted at `/`.
///
/// It may subscribe anywhere and publish at names of one segment.
pub(crate) fn browser_rules() -> moq_auth::Permissions {
    moq_auth::Permissions::new(
        "*".parse::<Pattern>().expect("a valid pattern"),
        Pattern::all(),
    )
}

/// Returns the broadcast name a session asked for in its path, if any.
///
/// A URL carries the name percent-encoded, so `/my%20cam` asks for `my cam`.
/// A name that does not decode to UTF-8 is none.
pub(crate) fn requested_name(path: &str) -> Option<String> {
    let path = path.split('?').next().unwrap_or_default();
    // A ticket is read from the raw path: its own escapes are part of it, and
    // decoding them first would change what it names.
    let raw = path.trim_start_matches('/');
    if raw.parse::<BroadcastTicket>().is_ok() {
        return Some(raw.to_owned());
    }
    let name = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()?;
    (!name.is_empty()).then(|| name.into_owned())
}

/// Returns the policy of an open relay for iroh clients.
///
/// It admits every client to read the whole cluster and to publish at its
/// own paths: its own `live/` broadcasts and its room broadcasts, at the
/// paths that name its endpoint id.
#[must_use]
pub fn relay_policy() -> impl Policy<EndpointId> + Clone {
    layout::policy((LiveLayout::default(), RoomsLayout))
}

/// Returns the own paths of `peer`: `live/<peer>/**` and `rooms/*/<peer>/**`.
pub(crate) fn own_paths(peer: EndpointId) -> Patterns {
    layout::grant(&(LiveLayout::default(), RoomsLayout), peer).publish
}

/// Returns the policy for iroh clients without a token on a private relay.
///
/// A client gets `anonymous`, the public patterns rooted at `/`, and with
/// `own_paths` also publishes at its own paths. With neither, every client
/// is refused.
pub(crate) fn anonymous_policy(
    anonymous: &moq_auth::Permissions,
    own_paths: bool,
) -> Arc<dyn DynPolicy<EndpointId>> {
    if anonymous.is_empty() && !own_paths {
        return Arc::new(policy::from_fn(|_: &PeerRequest| {
            Err(policy::Rejection::new(
                "this relay admits only token holders, and the session presented no token",
            ))
        }));
    }
    let grant = Grant {
        subscribe: anonymous.subscribe.clone(),
        publish: anonymous.publish.clone(),
    };
    Arc::new(policy::from_fn(move |request: &PeerRequest| {
        let mut grant = grant.clone();
        if own_paths {
            grant
                .publish
                .extend(self::own_paths(*request.peer()).iter().cloned());
        }
        Ok(grant.into())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(policy: &dyn DynPolicy<EndpointId>, peer: EndpointId) -> Option<Grant> {
        let request = PeerRequest::new(peer, SessionRequest::default(), Event::Connect);
        n0_future::future::block_on(DynPolicy::decide(policy, &request))
            .ok()
            .map(|authorization| authorization.grant)
    }

    #[test]
    fn the_anonymous_policy_follows_the_patterns_and_own_paths() {
        let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
        let own = format!("live/{id}/cam");
        let none = moq_auth::Permissions::default();
        assert!(decide(&*anonymous_policy(&none, false), id).is_none());
        let grant = decide(&*anonymous_policy(&none, true), id).expect("admitted");
        assert!(grant.allows_publish(&own));
        assert!(grant.allows_publish(&format!("rooms/topic/{id}/cam")));
        assert!(!grant.allows_subscribe(&own));
        let demo = moq_auth::Permissions::new(
            "demo/**".parse::<Pattern>().unwrap(),
            "demo/**".parse::<Pattern>().unwrap(),
        );
        let grant = decide(&*anonymous_policy(&demo, false), id).expect("admitted");
        assert!(grant.allows_publish("demo/cam") && grant.allows_subscribe("demo/cam"));
        assert!(!grant.allows_publish(&own) && !grant.allows_subscribe(&own));
        let grant = decide(&*anonymous_policy(&demo, true), id).expect("admitted");
        assert!(grant.allows_publish(&own) && grant.allows_publish("demo/cam"));
        let other = iroh::SecretKey::from_bytes(&[6; 32]).public();
        assert!(!grant.allows_publish(&format!("live/{other}/cam")));
    }

    /// A key change that widens a token's grant reconnects its session, one that narrows it closes it.
    #[test]
    fn a_changed_grant_reconnects_only_when_it_covers_the_admitted_one() {
        let admitted = Grant::publish_under(Pattern::subtree("tokens").unwrap());
        let wider = admitted.union(&Grant::publish_under(Pattern::subtree("other").unwrap()));
        assert!(matches!(
            ending_for_grant(&admitted, &wider),
            Ending::Reconnect
        ));
        assert!(matches!(
            ending_for_grant(&admitted, &Grant::everything()),
            Ending::Reconnect
        ));
        assert!(matches!(
            ending_for_grant(&admitted, &Grant::subscribe_only()),
            Ending::Close(moq_net::Error::Unauthorized)
        ));
        assert!(matches!(
            ending_for_grant(&wider, &admitted),
            Ending::Close(moq_net::Error::Unauthorized)
        ));
    }

    #[test]
    fn a_client_publishes_only_under_its_own_id() {
        let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[6; 32]).public();
        let request = PeerRequest::new(id, SessionRequest::default(), Event::Connect);
        let scope = n0_future::future::block_on(Policy::decide(&relay_policy(), &request))
            .expect("admitted")
            .grant
            .publish;
        assert!(scope.matches(&format!("live/{id}/cam")));
        assert!(scope.matches(&format!("rooms/topic/{id}/cam")));
        assert!(!scope.matches(&format!("live/{other}/cam")));
        assert!(!scope.matches(&format!("rooms/topic/{other}/cam")));
        assert!(!scope.matches("cam"));
    }

    /// Returns whether [`browser_grant`] lets a browser at the root publish at `path`.
    fn a_browser_may_publish(path: &str) -> bool {
        browser_grant("/")
            .expect("a grant")
            .publish
            .path
            .matches(path)
    }

    #[test]
    fn a_browser_publishes_one_segment_names() {
        assert!(a_browser_may_publish("hello"));
        assert!(!a_browser_may_publish("live/someone/cam"));
        assert!(!a_browser_may_publish("rooms/topic/someone/cam"));
    }

    /// The grant is rooted at the dialed path, so its publish half must not reach below it.
    #[test]
    fn a_browser_grant_is_reduced_to_the_dialed_path() {
        let peer = iroh::SecretKey::from_bytes(&[7; 32]).public();
        for dialed in [
            format!("/live/{peer}"),
            format!("/rooms/topic/{peer}"),
            "/live".to_owned(),
        ] {
            let grant = browser_grant(&dialed).expect("a grant");
            assert!(
                !grant.publish.path.matches("cam"),
                "a browser at {dialed} may publish cam beneath it"
            );
            assert!(grant.subscribe.path.matches("cam"), "{dialed}");
        }
        // A one-segment path is itself the one segment a browser may publish.
        let grant = browser_grant("/hello").expect("a grant");
        assert!(grant.publish.path.matches(""));
        assert!(!grant.publish.path.matches("cam"));
    }

    /// A browser that dials a ticket reads it, and publishes nothing beneath it.
    #[test]
    fn a_browser_at_a_ticket_path_publishes_nothing() {
        let peer = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let path = LiveLayout::default().path(peer, "cam").expect("a path");
        let ticket = BroadcastTicket::at(peer, path);
        let grant = browser_grant(&format!("/{ticket}")).expect("a grant");
        assert!(grant.publish.path.is_empty());
        assert!(grant.subscribe.path.matches(""));
    }

    /// A browser cannot publish at the name a pull is read under.
    ///
    /// Pulls are read at the ticket's spelling, which always has a `/` between
    /// the endpoint id and the name.
    #[test]
    fn a_browser_cannot_publish_at_a_pulled_name() {
        let peer = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let path = LiveLayout::default().path(peer, "cam").expect("a path");
        let ticket = BroadcastTicket::at(peer, path);
        let full = ticket.to_string();
        let bare = full.strip_prefix("ilv1:").expect("a ticket prefix");
        assert!(!a_browser_may_publish(&full));
        assert!(!a_browser_may_publish(bare));
    }

    #[test]
    fn a_requested_name_drops_the_slash_and_the_query() {
        assert_eq!(requested_name("/abc?jwt=x").as_deref(), Some("abc"));
        assert_eq!(requested_name(""), None);
        assert_eq!(requested_name("/"), None);
    }

    #[test]
    fn a_requested_name_is_percent_decoded() {
        assert_eq!(requested_name("/my%20cam").as_deref(), Some("my cam"));
        assert_eq!(
            requested_name("/caf%C3%A9?jwt=x").as_deref(),
            Some("caf\u{e9}")
        );
        assert_eq!(requested_name("/100%25").as_deref(), Some("100%"));
        assert_eq!(requested_name("/%FF"), None);
    }
}
