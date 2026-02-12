use std::env;

use crate::{
    live::Live,
    rooms::{Room, RoomTicket},
};
use iroh::{Endpoint, protocol::Router};
use iroh_gossip::Gossip;
use n0_error::{Result, StdResultExt};
use tracing::info;

#[derive(Debug, Clone)]
pub struct LiveNode {
    router: Router,
    pub live: Live,
    pub gossip: Gossip,
}

impl LiveNode {
    pub async fn spawn_from_env() -> Result<Self> {
        let endpoint = Endpoint::builder()
            .secret_key(secret_key_from_env()?)
            .bind()
            .await?;
        info!(endpoint_id=%endpoint.id(), "endpoint bound");

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let live = Live::new(endpoint.clone());

        let router = Router::builder(endpoint)
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(iroh_moq::ALPN, live.protocol_handler())
            .spawn();

        Ok(Self {
            router,
            gossip,
            live,
        })
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.live.shutdown();
        self.router.shutdown().await.anyerr()
    }

    pub fn endpoint(&self) -> &Endpoint {
        self.router.endpoint()
    }

    pub async fn join_room(&self, ticket: RoomTicket) -> Result<Room> {
        Room::new(
            self.endpoint(),
            self.gossip.clone(),
            self.live.clone(),
            ticket,
        )
        .await
    }
}

fn secret_key_from_env() -> n0_error::Result<iroh::SecretKey> {
    Ok(match env::var("IROH_SECRET") {
        Ok(key) => key.parse()?,
        Err(_) => {
            let key = iroh::SecretKey::generate(&mut rand::rng());
            println!(
                "Created new secret. Reuse with IROH_SECRET={}",
                data_encoding::HEXLOWER.encode(&key.to_bytes())
            );
            key
        }
    })
}
