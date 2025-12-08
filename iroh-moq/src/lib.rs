use std::{collections::HashMap, sync::Arc};

use iroh::{Endpoint, EndpointAddr, EndpointId, endpoint::Connection, protocol::ProtocolHandler};
use moq_lite::{BroadcastConsumer, BroadcastProducer, OriginConsumer, OriginProducer};
use n0_error::{AnyError, Result, StdResultExt, e, stack_error};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error_span, info, instrument};
use web_transport_iroh::Request;

pub const ALPN: &[u8] = b"iroh-live/1";

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
    endpoint: Endpoint,
    tx: mpsc::Sender<ActorMessage>,
    shutdown_token: CancellationToken,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl Moq {
    pub fn new(endpoint: Endpoint) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let actor = Actor::default();
        let shutdown_token = actor.shutdown_token.clone();
        let actor_task = n0_future::task::spawn(async move {
            actor.run(rx).instrument(error_span!("LiveActor")).await
        });
        Self {
            shutdown_token,
            endpoint,
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
            .send(ActorMessage::PublishBroadcast(name.to_string(), producer))
            .await
            .std_context("live actor died")?;
        Ok(())
    }

    pub async fn connect(&self, addr: impl Into<EndpointAddr>) -> Result<MoqSession, Error> {
        MoqSession::connect(&self.endpoint, addr).await
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
        let request = Request::accept(connection).await?;
        info!(url=%request.url(), "accepted");
        let session = request.ok().await?;
        let session = MoqSession::session_accept(session).await?;
        self.tx
            .send(ActorMessage::HandleSession(session))
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

pub struct MoqSession {
    remote: EndpointId,
    wt_session: web_transport_iroh::Session,
    moq_session: moq_lite::Session<web_transport_iroh::Session>,
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
        let url: url::Url = format!("iroh://{}", connection.remote_id())
            .parse()
            .expect("valid url");
        let wt_session = web_transport_iroh::Session::raw(connection, url);
        Self::session_connect(wt_session).await
    }

    pub async fn session_connect(wt_session: web_transport_iroh::Session) -> Result<Self, Error> {
        let remote = wt_session.remote_id();
        let publish = moq_lite::Origin::produce();
        let subscribe = moq_lite::Origin::produce();
        let moq_session =
            moq_lite::Session::connect(wt_session.clone(), publish.consumer, subscribe.producer)
                .await?;
        Ok(Self {
            publish: publish.producer,
            subscribe: subscribe.consumer,
            remote,
            moq_session,
            wt_session,
        })
    }

    pub async fn session_accept(wt_session: web_transport_iroh::Session) -> Result<Self, Error> {
        let remote = wt_session.remote_id();
        let publish = moq_lite::Origin::produce();
        let subscribe = moq_lite::Origin::produce();
        let moq_session =
            moq_lite::Session::accept(wt_session.clone(), publish.consumer, subscribe.producer)
                .await?;
        Ok(Self {
            remote,
            publish: publish.producer,
            subscribe: subscribe.consumer,
            moq_session,
            wt_session,
        })
    }

    pub fn conn(&self) -> &iroh::endpoint::Connection {
        self.wt_session.conn()
    }

    pub async fn subscribe(&mut self, name: &str) -> Result<BroadcastConsumer, SubscribeError> {
        let consumer = self.wait_for_broadcast(name).await?;
        Ok(consumer)
    }

    pub fn publish(&self, name: String, broadcast: BroadcastConsumer) {
        self.publish.publish_broadcast(name, broadcast);
    }

    async fn wait_for_broadcast(
        &mut self,
        name: &str,
    ) -> Result<BroadcastConsumer, SubscribeError> {
        if let Some(consumer) = self.subscribe.consume_broadcast(name) {
            return Ok(consumer);
        }
        loop {
            let (path, consumer) = self
                .subscribe
                .announced()
                .await
                .ok_or_else(|| e!(SubscribeError::NotAnnounced))?;
            debug!("peer announced broadcast: {path}");
            if path.as_str() == name {
                return consumer.ok_or_else(|| e!(SubscribeError::Closed));
            }
        }
    }
}

enum ActorMessage {
    HandleSession(MoqSession),
    PublishBroadcast(BroadcastName, BroadcastProducer),
}

struct SessionState {
    publish: OriginProducer,
}

type BroadcastName = String;

#[derive(Default)]
struct Actor {
    shutdown_token: CancellationToken,
    broadcasts: HashMap<BroadcastName, BroadcastProducer>,
    sessions: HashMap<EndpointId, SessionState>,
    session_tasks: JoinSet<(EndpointId, Result<(), moq_lite::Error>)>,
}

impl Actor {
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
            }
        }
    }

    fn handle_message(&mut self, msg: ActorMessage) {
        match msg {
            ActorMessage::HandleSession(msg) => self.handle_incoming_session(msg),
            ActorMessage::PublishBroadcast(name, producer) => {
                self.handle_publish_broadcast(name, producer)
            }
        }
    }

    fn handle_incoming_session(&mut self, session: MoqSession) {
        tracing::info!("handle new incoming session");
        let MoqSession {
            remote,
            moq_session,
            publish,
            subscribe: _,
            ..
        } = session;
        for (name, producer) in self.broadcasts.iter() {
            publish.publish_broadcast(name.to_string(), producer.consume());
        }
        self.sessions.insert(remote, SessionState { publish });

        let shutdown = self.shutdown_token.child_token();
        self.session_tasks.spawn(async move {
            let res = tokio::select! {
                _ = shutdown.cancelled() => {
                    moq_session.close(moq_lite::Error::Cancel);
                    Ok(())
                }
                result = moq_session.closed() => result,
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
        self.broadcasts.insert(name, producer);
    }
}
