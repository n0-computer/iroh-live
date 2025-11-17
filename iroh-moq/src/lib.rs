use std::{collections::HashMap, sync::Arc};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, ConnectionStats},
    protocol::ProtocolHandler,
};
use moq_lite::{BroadcastConsumer, BroadcastProducer, OriginConsumer, OriginProducer};
use n0_error::{AnyError, Result, StdResultExt, stack_error};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error_span, info, instrument};
use web_transport_iroh::Request;

use self::subscribe::SubscribeBroadcast;
pub use self::ticket::LiveTicket;

pub mod av;
pub mod publish;
pub mod subscribe;
mod ticket;

pub const ALPN: &[u8] = b"iroh-live/1";

#[stack_error(derive, add_meta, from_sources)]
#[allow(private_interfaces)]
pub enum Error {
    #[error(transparent)]
    Connect(iroh::endpoint::ConnectError),
    #[error(transparent)]
    Moq(#[error(std_err)] moq_lite::Error),
    #[error(transparent)]
    Server(#[error(std_err)] web_transport_iroh::ServerError),
    #[error("internal consistency error")]
    InternalConsistencyError(#[error(source)] LiveActorDiedError),
    // TODO: Remove
    #[error(transparent)]
    Other(n0_error::AnyError),
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
pub struct Live {
    endpoint: Endpoint,
    tx: mpsc::Sender<ActorMessage>,
    shutdown_token: CancellationToken,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl Live {
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

    pub fn protocol_handler(&self) -> LiveProtocolHandler {
        LiveProtocolHandler {
            tx: self.tx.clone(),
        }
    }

    pub async fn publish(&self, broadcast: &publish::PublishBroadcast) -> Result<LiveTicket> {
        let ticket = LiveTicket {
            endpoint_id: self.endpoint.id(),
            broadcast_name: broadcast.name.clone(),
        };
        self.tx
            .send(ActorMessage::PublishBroadcast(
                broadcast.name.clone(),
                broadcast.producer.clone(),
            ))
            .await
            .std_context("live actor died")?;
        Ok(ticket)
    }

    #[instrument(skip_all, fields(remote=tracing::field::Empty))]
    pub async fn connect(&self, addr: impl Into<EndpointAddr>) -> Result<LiveSession, Error> {
        let addr = addr.into();
        tracing::Span::current().record("remote", tracing::field::display(addr.id.fmt_short()));
        let connection = self.endpoint.connect(addr, ALPN).await?;
        let url: url::Url = format!("iroh://{}", connection.remote_id())
            .parse()
            .expect("valid url");
        let session = web_transport_iroh::Session::raw(connection, url);
        let session = LiveSession::connect(session).await?;
        Ok(session)
    }

    pub fn shutdown(&self) {
        self.shutdown_token.cancel();
    }
}

#[derive(Debug, Clone)]
pub struct LiveProtocolHandler {
    tx: mpsc::Sender<ActorMessage>,
}

impl LiveProtocolHandler {
    async fn handle_connection(&self, connection: Connection) -> Result<(), Error> {
        let request = Request::accept(connection).await?;
        info!(url=%request.url(), "accepted");
        let session = request.ok().await.std_context("Failed to accept session")?;
        let session = LiveSession::accept(session).await?;
        self.tx
            .send(ActorMessage::HandleSession(session))
            .await
            .map_err(LiveActorDiedError::from)?;
        Ok(())
    }
}

impl ProtocolHandler for LiveProtocolHandler {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        self.handle_connection(connection)
            .await
            .map_err(AnyError::from)?;
        Ok(())
    }
}

pub struct LiveSession {
    remote: EndpointId,
    wt_session: web_transport_iroh::Session,
    moq_session: moq_lite::Session<web_transport_iroh::Session>,
    publish: OriginProducer,
    subscribe: OriginConsumer,
}

impl LiveSession {
    pub async fn connect(wt_session: web_transport_iroh::Session) -> Result<Self, Error> {
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
    pub async fn accept(wt_session: web_transport_iroh::Session) -> Result<Self, Error> {
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

    pub fn stats(&self) -> ConnectionStats {
        self.wt_session.stats()
    }

    pub async fn subscribe(&mut self, name: &str) -> Result<SubscribeBroadcast, Error> {
        let consumer = self.wait_for_broadcast(name).await?;
        let broadcast = SubscribeBroadcast::new(consumer).await?;
        Ok(broadcast)
    }

    pub fn publish(&self, broadcast: &publish::PublishBroadcast) {
        let consumer = broadcast.producer.consume();
        self.publish
            .publish_broadcast(broadcast.name.clone(), consumer);
    }

    async fn wait_for_broadcast(&mut self, name: &str) -> Result<BroadcastConsumer, Error> {
        loop {
            let (path, consumer) = self
                .subscribe
                .announced()
                .await
                .std_context("session closed before broadcast was announced")?;
            debug!("peer announced broadcast: {path}");
            if path.as_str() == name {
                return consumer
                    .std_context("peer closed the broadcast")
                    .map_err(Into::into);
            }
        }
    }
}

enum ActorMessage {
    HandleSession(LiveSession),
    PublishBroadcast(BroadcastName, BroadcastProducer),
}

struct SessionState {
    publish: OriginProducer,
}

type BroadcastName = String;

pub type PacketSender = mpsc::Sender<hang::Frame>;

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

    fn handle_incoming_session(&mut self, session: LiveSession) {
        tracing::info!("handle new incoming session");
        let LiveSession {
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
