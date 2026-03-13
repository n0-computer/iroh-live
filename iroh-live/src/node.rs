use std::env;

use iroh::Endpoint;
use n0_error::Result;
use tracing::info;

use crate::{
    live::Live,
    rooms::{Room, RoomTicket},
};

/// Convenience type that creates an endpoint, gossip, MoQ transport, and router.
///
/// For more control, use [`Live::builder()`] directly.
#[derive(Debug, Clone)]
pub struct LiveNode {
    live: Live,
}

impl LiveNode {
    pub async fn spawn_from_env() -> Result<Self> {
        let endpoint = Endpoint::builder()
            .secret_key(secret_key_from_env()?)
            .bind()
            .await?;
        info!(endpoint_id=%endpoint.id(), "endpoint bound");

        let live = Live::builder(endpoint).enable_gossip().spawn_with_router();

        Ok(Self { live })
    }

    pub fn shutdown(&self) {
        self.live.shutdown();
    }

    pub fn endpoint(&self) -> &Endpoint {
        self.live.endpoint()
    }

    pub fn live(&self) -> &Live {
        &self.live
    }

    pub async fn join_room(&self, ticket: RoomTicket) -> Result<Room> {
        Room::new(
            self.endpoint(),
            self.live
                .gossip()
                .expect("LiveNode always has gossip enabled")
                .clone(),
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
