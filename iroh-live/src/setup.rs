use crate::{
    audio::AudioBackend,
    av::{AudioPreset, VideoPreset},
    capture::{CameraCapturer, ScreenCapturer},
    ffmpeg::{H264Encoder, OpusEncoder},
    live::Live,
    publish::{AudioRenditions, PublishBroadcast, VideoRenditions},
    rooms::{Room, RoomTicket},
};
use iroh::{Endpoint, protocol::Router};
use iroh_gossip::Gossip;
use n0_error::Result;
use tracing::info;

#[derive(Debug, Clone)]
pub struct Opts {
    pub audio: bool,
    pub video: Option<VideoSource>,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum VideoSource {
    #[default]
    Camera,
    Screen,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            audio: true,
            video: Some(VideoSource::Camera),
        }
    }
}

pub struct RoomState {
    pub router: Router,
    pub broadcast: PublishBroadcast,
    pub room: Room,
}

impl RoomState {
    pub async fn join(audio_ctx: AudioBackend, ticket: RoomTicket, opts: Opts) -> Result<Self> {
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

        // Publish ourselves.
        let broadcast = {
            let mut broadcast = PublishBroadcast::new();
            if opts.audio {
                let mic = audio_ctx.default_input().await?;
                let audio = AudioRenditions::new::<OpusEncoder>(mic, [AudioPreset::Hq]);
                broadcast.set_audio(Some(audio))?;
            }
            if let Some(video_source) = opts.video {
                let video = match video_source {
                    VideoSource::Camera => {
                        let camera = CameraCapturer::new()?;
                        VideoRenditions::new::<H264Encoder>(camera, VideoPreset::all())
                    }
                    VideoSource::Screen => {
                        let screen = ScreenCapturer::new()?;
                        VideoRenditions::new::<H264Encoder>(screen, VideoPreset::all())
                    }
                };
                broadcast.set_video(Some(video))?;
            }
            broadcast
        };

        live.publish(&ticket.broadcast_name, broadcast.producer())
            .await?;

        let room = Room::new(router.endpoint(), gossip, live, ticket).await?;

        println!("room ticket: {}", room.ticket());

        Ok(Self {
            router,
            broadcast,
            room,
        })
    }
}

fn secret_key_from_env() -> n0_error::Result<iroh::SecretKey> {
    Ok(match std::env::var("IROH_SECRET") {
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
