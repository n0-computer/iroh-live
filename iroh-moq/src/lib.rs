use std::{
    collections::{HashMap, hash_map},
    sync::Arc,
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, ConnectionError},
    protocol::ProtocolHandler,
};
use moq_lite::{BroadcastConsumer, BroadcastProducer, OriginConsumer, OriginProducer};
use n0_error::{AnyError, Result, StdResultExt, anyerr, e, stack_error};
use n0_future::{
    FuturesUnordered, StreamExt,
    boxed::BoxFuture,
    task::{AbortOnDropHandle, JoinSet},
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error_span, info, instrument};
use web_transport_iroh::SessionError;

pub const ALPN: &[u8] = moq_lite::lite::ALPN.as_bytes();

#[stack_error(derive, add_meta, from_sources)]
#[allow(private_interfaces)]
pub enum Error {
    #[error(transparent)]
    Connect(iroh::endpoint::ConnectError),
    #[error(transparent)]
    Moq(#[error(source, std_err)] moq_lite::Error),
    #[error(transparent)]
    Server(#[error(source, std_err)] web_transport_iroh::ServerError),
    #[error("internal consistency error")]
    InternalConsistencyError(#[error(source)] LiveActorDiedError),
    #[error("failed to perform request")]
    Request(#[error(source, std_err)] iroh::endpoint::WriteError),
}

#[stack_error(derive, add_meta, from_sources)]
#[allow(private_interfaces)]
pub enum SubscribeError {
    #[error("track was not announced")]
    NotAnnounced,
    #[error("track was closed")]
    Closed,
    #[error("session was closed")]
    SessionClosed(#[error(source, std_err)] SessionError),
}

#[stack_error(derive)]
#[error("live actor died")]
struct LiveActorDiedError;

impl From<mpsc::error::SendError<ActorMessage>> for LiveActorDiedError {
    fn from(_value: mpsc::error::SendError<ActorMessage>) -> Self {
        Self
    }
}

#[derive(Debug, Clone)]
pub struct Moq {
    tx: mpsc::Sender<ActorMessage>,
    shutdown_token: CancellationToken,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl Moq {
    pub fn new(endpoint: Endpoint) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let actor = Actor::new(endpoint);
        let shutdown_token = actor.shutdown_token.clone();
        let actor_task = n0_future::task::spawn(async move {
            actor.run(rx).instrument(error_span!("LiveActor")).await
        });
        Self {
            shutdown_token,
            tx,
            _actor_handle: Arc::new(AbortOnDropHandle::new(actor_task)),
        }
    }

    pub fn protocol_handler(&self) -> MoqProtocolHandler {
        MoqProtocolHandler {
            tx: self.tx.clone(),
        }
    }

    pub async fn publish(&self, name: impl ToString, producer: BroadcastProducer) -> Result<()> {
        self.tx
            .send(ActorMessage::PublishBroadcast {
                broadcast_name: name.to_string(),
                producer,
            })
            .await
            .std_context("live actor died")?;
        Ok(())
    }

    pub async fn published_broadcasts(&self) -> Vec<String> {
        let (reply, reply_rx) = oneshot::channel();
        if let Err(_) = self.tx.send(ActorMessage::GetPublished { reply }).await {
            return vec![];
        }
        reply_rx.await.unwrap_or_default()
    }

    pub async fn connect(&self, remote: impl Into<EndpointAddr>) -> Result<MoqSession, AnyError> {
        // MoqSession::connect(&self.endpoint, addr).await
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::Connect {
                remote: remote.into(),
                reply,
            })
            .await
            .map_err(|_| LiveActorDiedError)?;
        reply_rx
            .await
            .map_err(|_| LiveActorDiedError)?
            .map_err(|err| anyerr!(err))
    }

    pub fn shutdown(&self) {
        self.shutdown_token.cancel();
    }
}

#[derive(Debug, Clone)]
pub struct MoqProtocolHandler {
    tx: mpsc::Sender<ActorMessage>,
}

impl MoqProtocolHandler {
    async fn handle_connection(&self, connection: Connection) -> Result<(), Error> {
        info!(remote = %connection.remote_id().fmt_short(), "accepted");
        let session = web_transport_iroh::Session::raw(connection);
        let session = MoqSession::session_accept(session).await?;
        self.tx
            .send(ActorMessage::HandleSession { session })
            .await
            .map_err(LiveActorDiedError::from)?;
        Ok(())
    }
}

impl ProtocolHandler for MoqProtocolHandler {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        self.handle_connection(connection)
            .await
            .map_err(AnyError::from)?;
        Ok(())
    }
}

// TODO: resubscribing session?
// struct MoqSession2 {
//     session: MoqSession,
//     tx: mpsc::Sender<ActorMessage>,
//     remote: EndpointAddr,
// }

// impl MoqSession2 {
//     pub async fn subscribe(&mut self, name: &str) -> Result<BroadcastConsumer> {
//         match self.session.subscribe(name).await {
//             Ok(consumer) => return Ok(consumer),
//             Err(err) => {
//                 warn!("first attempt to subscribe failed, retrying. reason: {err:#}");
//                 let (reply, reply_rx) = oneshot::channel();
//                 self.tx
//                     .send(ActorMessage::Connect {
//                         remote: self.remote.clone(),
//                         reply,
//                     })
//                     .await
//                     .map_err(|_| LiveActorDiedError)?;
//                 self.session = reply_rx
//                     .await
//                     .map_err(|_| LiveActorDiedError)?
//                     .map_err(|err| anyerr!(err))?;
//                 self.session.subscribe(name).await.map_err(Into::into)
//             }
//         }
//     }
// }

#[derive(Clone)]
pub struct MoqSession {
    wt_session: web_transport_iroh::Session,
    publish: OriginProducer,
    subscribe: OriginConsumer,
}

impl MoqSession {
    #[instrument(skip_all, fields(remote=tracing::field::Empty))]
    pub async fn connect(
        endpoint: &Endpoint,
        remote_addr: impl Into<EndpointAddr>,
    ) -> Result<Self, Error> {
        let addr = remote_addr.into();
        tracing::Span::current().record("remote", tracing::field::display(addr.id.fmt_short()));
        let connection = endpoint.connect(addr, ALPN).await?;
        let wt_session = web_transport_iroh::Session::raw(connection);
        Self::session_connect(wt_session).await
    }

    pub async fn session_connect(wt_session: web_transport_iroh::Session) -> Result<Self, Error> {
        let publish = moq_lite::Origin::produce();
        let subscribe = moq_lite::Origin::produce();
        // We can drop the moq_lite::Session, it spawns it tasks in the background anyway.
        // If that changes and it becomes a guard, we should keep it around.
        let _moq_session =
            moq_lite::Session::connect(wt_session.clone(), publish.consumer, subscribe.producer)
                .await?;
        Ok(Self {
            publish: publish.producer,
            subscribe: subscribe.consumer,
            wt_session,
        })
    }

    pub async fn session_accept(wt_session: web_transport_iroh::Session) -> Result<Self, Error> {
        let publish = moq_lite::Origin::produce();
        let subscribe = moq_lite::Origin::produce();
        // We can drop the moq_lite::Session, it spawns it tasks in the background anyway.
        // If that changes and it becomes a guard, we should keep it around.
        let _moq_session =
            moq_lite::Session::accept(wt_session.clone(), publish.consumer, subscribe.producer)
                .await?;
        Ok(Self {
            publish: publish.producer,
            subscribe: subscribe.consumer,
            wt_session,
        })
    }

    pub fn remote_id(&self) -> EndpointId {
        self.wt_session.remote_id()
    }

    pub fn conn(&self) -> &iroh::endpoint::Connection {
        self.wt_session.conn()
    }

    pub async fn subscribe(&mut self, name: &str) -> Result<BroadcastConsumer, SubscribeError> {
        if let Some(reason) = self.conn().close_reason() {
            return Err(SessionError::from(reason).into());
        }
        if let Some(consumer) = self.subscribe.consume_broadcast(name) {
            return Ok(consumer);
        }
        loop {
            let res = tokio::select! {
                res = self.subscribe.announced() => res,
                reason = self.wt_session.closed() => {
                    return Err(reason.into())
                }
            };
            let (path, consumer) = res.ok_or_else(|| e!(SubscribeError::NotAnnounced))?;
            debug!("peer announced broadcast: {path}");
            if path.as_str() == name {
                return consumer.ok_or_else(|| e!(SubscribeError::Closed));
            }
        }
    }

    pub fn publish(&self, name: String, broadcast: BroadcastConsumer) {
        self.publish.publish_broadcast(name, broadcast);
    }

    pub fn close(&self, error_code: u32, reason: &[u8]) {
        self.wt_session.close(error_code, reason);
    }

    pub async fn closed(&self) -> web_transport_iroh::SessionError {
        self.wt_session.closed().await
    }
}

enum ActorMessage {
    HandleSession {
        session: MoqSession,
    },
    PublishBroadcast {
        broadcast_name: BroadcastName,
        producer: BroadcastProducer,
    },
    Connect {
        remote: EndpointAddr,
        reply: oneshot::Sender<Result<MoqSession, Arc<AnyError>>>,
    },
    GetPublished {
        reply: oneshot::Sender<Vec<BroadcastName>>,
    },
}

type BroadcastName = String;

#[derive()]
struct Actor {
    endpoint: Endpoint,
    shutdown_token: CancellationToken,
    publishing: HashMap<BroadcastName, BroadcastProducer>,
    publishing_closed_futs: FuturesUnordered<BoxFuture<BroadcastName>>,
    sessions: HashMap<EndpointId, MoqSession>,
    session_tasks: JoinSet<(EndpointId, Result<(), web_transport_iroh::SessionError>)>,
    pending_connects: HashMap<EndpointId, Vec<oneshot::Sender<Result<MoqSession, Arc<AnyError>>>>>,
    pending_connect_tasks: JoinSet<(EndpointId, Result<MoqSession, AnyError>)>,
}

impl Actor {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            shutdown_token: CancellationToken::new(),
            publishing: Default::default(),
            publishing_closed_futs: Default::default(),
            sessions: Default::default(),
            session_tasks: Default::default(),
            pending_connects: Default::default(),
            pending_connect_tasks: Default::default(),
        }
    }

    pub async fn run(mut self, mut inbox: mpsc::Receiver<ActorMessage>) {
        loop {
            tokio::select! {
                msg = inbox.recv() => {
                    match msg {
                        None => break,
                        Some(msg) => self.handle_message(msg)
                    }
                }
                Some(res) = self.session_tasks.join_next(), if !self.session_tasks.is_empty() => {
                    let (endpoint_id, res) = res.expect("session task panicked");
                    info!(remote=%endpoint_id.fmt_short(), "session closed: {res:?}");
                    self.sessions.remove(&endpoint_id);
                }
                Some(name) = self.publishing_closed_futs.next(), if !self.publishing_closed_futs.is_empty() => {
                    self.publishing.remove(&name);
                }
                Some(res) = self.pending_connect_tasks.join_next(), if !self.pending_connect_tasks.is_empty() => {
                    let (endpoint_id, res) = res.expect("connect task panicked");
                    match res {
                        Ok(session) => {
                            info!(remote=%endpoint_id.fmt_short(), "connected");
                            self.handle_incoming_session(session);
                        }
                        Err(err) => {
                            info!(remote=%endpoint_id.fmt_short(), "connect failed: {err:#}");
                            let replies = self.pending_connects.remove(&endpoint_id).into_iter().flatten();
                            let err = Arc::new(err);
                            for reply in replies {
                                reply.send(Err(err.clone())).ok();
                            }

                        }
                    }
                }
            }
        }
    }

    fn handle_message(&mut self, msg: ActorMessage) {
        match msg {
            ActorMessage::HandleSession { session: msg } => self.handle_incoming_session(msg),
            ActorMessage::PublishBroadcast {
                broadcast_name: name,
                producer,
            } => self.handle_publish_broadcast(name, producer),
            ActorMessage::Connect { remote, reply } => self.handle_connect(remote, reply),
            ActorMessage::GetPublished { reply } => {
                let names = self.publishing.keys().cloned().collect();
                reply.send(names).ok();
            }
        }
    }

    fn handle_incoming_session(&mut self, session: MoqSession) {
        tracing::info!("handle new incoming session");
        let remote = session.remote_id();
        for (name, producer) in self.publishing.iter() {
            session.publish(name.to_string(), producer.consume());
        }
        self.sessions.insert(remote, session.clone());
        for reply in self.pending_connects.remove(&remote).into_iter().flatten() {
            reply.send(Ok(session.clone())).ok();
        }

        let shutdown = self.shutdown_token.child_token();
        self.session_tasks.spawn(async move {
            let res = tokio::select! {
                _ = shutdown.cancelled() => {
                    session.close(0u32.into(), b"cancelled");
                    Ok(())
                }
                result = session.closed() => match result {
                    SessionError::ConnectionError(ConnectionError::LocallyClosed) => Ok(()),
                    err @ _ => Err(err)
                },
            };
            (remote, res)
        });
    }

    fn handle_publish_broadcast(&mut self, name: BroadcastName, producer: BroadcastProducer) {
        for session in self.sessions.values_mut() {
            session
                .publish
                .publish_broadcast(name.clone(), producer.consume());
        }
        let closed = producer.consume().closed();
        self.publishing.insert(name.clone(), producer);
        self.publishing_closed_futs.push(Box::pin(async move {
            closed.await;
            name
        }));
    }

    fn handle_connect(
        &mut self,
        remote: EndpointAddr,
        reply: oneshot::Sender<Result<MoqSession, Arc<AnyError>>>,
    ) {
        let remote_id = remote.id;
        if let Some(session) = self.sessions.get(&remote_id) {
            reply.send(Ok(session.clone())).ok();
            return;
        }
        match self.pending_connects.entry(remote_id) {
            hash_map::Entry::Occupied(mut entry) => {
                entry.get_mut().push(reply);
            }
            hash_map::Entry::Vacant(entry) => {
                let endpoint = self.endpoint.clone();
                self.pending_connect_tasks.spawn(async move {
                    let res = MoqSession::connect(&endpoint, remote)
                        .await
                        .map_err(Into::into);
                    (remote_id, res)
                });
                entry.insert(Default::default()).push(reply);
            }
        }
    }
}
