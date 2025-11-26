pub mod audio;
pub mod av;
pub mod capture;
pub mod ffmpeg;
pub mod publish;
pub mod subscribe;
pub mod ticket;

pub type PacketSender = tokio::sync::mpsc::Sender<hang::Frame>;
pub type BroadcastName = String;

pub use iroh_moq::{ALPN, Error, Live, LiveProtocolHandler, LiveSession};

// pub async fn publish(&self, broadcast: &publish::PublishBroadcast) -> Result<LiveTicket> {
//     let ticket = LiveTicket {
//         endpoint_id: self.endpoint.id(),
//         broadcast_name: broadcast.name.clone(),
//     };
//     self.tx
//         .send(ActorMessage::PublishBroadcast(
//             broadcast.name.clone(),
//             broadcast.producer.clone(),
//         ))
//         .await
//         .std_context("live actor died")?;
//     Ok(ticket)
// }
