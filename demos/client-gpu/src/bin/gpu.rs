//! The headless gpu node: filters the client's video and sends it back.
//!
//! It accepts a client by watching for a `live/<client id>/call` offer. It
//! plays the client's video with the lowest latency the player offers, applies
//! the filter the client selected, draws a wall-clock watermark, and publishes
//! the result as its own side of the call. The filters are on the CPU. The
//! name stands for the node that would run them on a GPU in a real deployment.

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use clap::Parser;
use client_gpu_demo::{Offer, control, filter::Filter, i420, stamp, watermark};
use iroh::EndpointId;
use iroh_live::{
    EndpointOptions, Live, Mdns,
    media::{
        Latency, LocalBroadcast, PlayerConfig, VideoEncoding, VideoFormat, VideoRendition,
        VideoSource,
        video::{self, FrameRate, Size},
    },
};
use moq_net::bandwidth::Rate;
use n0_future::task::{AbortOnDropHandle, spawn};
use tokio::sync::watch;
use tracing::{info, warn};

/// Command line of the gpu node.
#[derive(Debug, Parser)]
struct Args {
    /// Target bitrate of the returned video, in kbit/s.
    #[arg(long, default_value_t = 1500)]
    kbps: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let options = EndpointOptions {
        mdns: Mdns::Announce,
        ..EndpointOptions::from_env()?
    };
    let live = Live::builder(options.bind().await?).with_router().spawn();
    let id = live.endpoint().id();
    println!("gpu node {id} is up");
    println!("run: cargo run -p client-gpu-demo --bin client {id}");

    // The client's call offer appears in the route table once it has a session.
    let mut serving: HashMap<EndpointId, AbortOnDropHandle<()>> = HashMap::new();
    let mut updates = live.moq().origin().announced();
    loop {
        let update = tokio::select! {
            update = updates.next() => update,
            _ = tokio::signal::ctrl_c() => break,
        };
        let Some(update) = update else { break };
        use moq_net::announce::Event;
        let (announce, active) = match &update {
            Event::End(announce) => (announce, false),
            Event::Start(announce)
            | Event::Update(announce)
            | Event::Restart(announce)
            | Event::Takeover { announce, .. } => (announce, true),
        };
        let Some(client) = iroh_live::BroadcastTicket::from_path(announce.prefix.as_str())
            .filter(|ticket| ticket.name() == iroh_live::CALL)
            .map(|ticket| ticket.peer())
            .filter(|peer| *peer != id)
        else {
            continue;
        };
        if !active {
            info!(client = %client.fmt_short(), "client hung up");
            serving.remove(&client);
        } else if let std::collections::hash_map::Entry::Vacant(slot) = serving.entry(client) {
            info!(client = %client.fmt_short(), "client called");
            let live = live.clone();
            let task = spawn(async move {
                match serve(live, client, args.kbps).await {
                    Ok(()) => info!("the client's video ended"),
                    Err(err) => warn!("serving the client failed: {err:#}"),
                }
            });
            slot.insert(AbortOnDropHandle::new(task));
        }
    }
    serving.clear();
    live.shutdown().await;
    Ok(())
}

/// Serves one client until its video ends.
async fn serve(live: Live, client: EndpointId, kbps: u64) -> Result<()> {
    // The returned video goes out first, so the client finds it when it answers.
    let rendition = VideoRendition {
        bitrate: Some(Rate::from_kbps(kbps)),
        keyframe_interval: Duration::from_secs(1),
        ..VideoRendition::new("returned")
    };
    let format = VideoFormat {
        size: Size::new(640, 360),
        rate: FrameRate::new(30, 1).expect("a valid rate"),
    };
    let (sender, source) = VideoSource::push(format);
    let returned = LocalBroadcast::new();
    returned.set_video(source, VideoEncoding::single(rendition))?;
    let _offer = Offer::new(&live, client, &returned)?;

    let (filter_tx, filter) = watch::channel(Filter::None);
    let control = {
        let live = live.clone();
        spawn(async move {
            if let Err(err) = control::receive(&live, client, filter_tx).await {
                warn!("the control track ended: {err:#}");
            }
        })
    };
    let _control = AbortOnDropHandle::new(control);

    let subscription = live.answer(client).await?;
    let config = PlayerConfig {
        latency: Latency::IMMEDIATE,
        // Software decoders hand back CPU pictures, which the filters need.
        decoder: video::decode::Kind::Software,
        ..PlayerConfig::default()
    };
    let player = live.remote_broadcast(&subscription).play(config)?;
    let mut frames = player.video();
    info!("playing the client's video");

    while let Some(frame) = frames.next().await {
        let filter = *filter.borrow();
        // Read before filtering: a blur would smear the id.
        let id = stamp::read(&frame);
        let text = format!(
            "GPU {} {}",
            watermark::wall_clock(),
            filter.name().to_uppercase()
        );
        let processed = tokio::task::block_in_place(|| {
            i420::edit(&frame, |data, width, height| {
                filter.apply(data, width, height);
                watermark::draw(data, width, height, &text);
                if let Some(id) = id {
                    stamp::write(data, width, height, id);
                }
            })
        });
        match processed {
            Ok(frame) => {
                if sender.push(frame).is_err() {
                    break;
                }
            }
            Err(err) => warn!("cannot process a frame: {err:#}"),
        }
    }
    Ok(())
}
